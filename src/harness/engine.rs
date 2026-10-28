//! The load engine (`docs/plans/harness.md`, sections 4, 6.1 and 10, built the
//! way phase 0 found it must work):
//!
//! - its own multi-threaded tokio runtime, so load generation never starves
//!   the node's data plane in loop-through;
//! - one **scheduler thread** per run, outside every runtime: it keeps the
//!   intended send times with `std::thread::sleep` (finer than tokio's 1 ms
//!   timer, never queued behind request tasks) and spawns each request onto
//!   the engine runtime;
//! - an **in-flight cap** from the descriptor limit: at the cap a request is
//!   counted as `not_sent` and the run is flagged as saturated, instead of
//!   slowing down silently;
//! - **per-worker histograms**: every worker thread records into its own
//!   shard, and the scheduler merges the shards into the totals and one
//!   timeline bucket per second (10 s buckets after the first hour);
//! - `TCP_NODELAY` on every connection, connection pools that live as long
//!   as the run's targets, connections pinned to the resolved addresses;
//! - saturation judged per one-second bucket, two buckets in a row;
//!   the achieved rate counted by completion time.
//!
//! Open layers send at their intended times (latency is measured from the
//! intended send, the coordinated omission correction, and from the actual
//! send). Closed layers run the number of users the pattern gives, each
//! waiting for its answer plus a think time.
//!
//! While a `limit_test` runs, a request carries an identity of the test's
//! pool (`{{id}}`, `{{ip}}`, a rotating forwarded address with `many_ips`),
//! and a refused request is retried with `retry_wave`. A run that analyses
//! rate limits counts every answer in its layer's [`LimitAcc`] (per worker,
//! merged with the shards).

use std::cell::Cell;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use hdrhistogram::Histogram;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Uri};
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::runtime::{Handle, Runtime};
use tokio::sync::watch;

use super::correlate::{self, Correlator, Marker, RunEntry, RunKey, Tag};
use super::limit_test::{identity_ip, MAX_RETRY_WAIT};
use super::live::{Change, Controls, Level, LiveView, StageLog};
use super::ratelimit::{self, Grid, LimitAcc, Obs};
use super::report::{
    self, histogram, Breakdown, Bucket, Counts, Percentiles, Report, Sample, Saturation,
    TargetReport, ThresholdResult, WireHist, LAG_SATURATION_MS, MAX_SAMPLES,
};
use super::safety::{KillSwitch, Lease, ParsedTarget};
use super::scenario::{
    Caps, HttpVersion, Layer, Model, RequestTemplate, Scenario, Target, MAX_LAYERS,
};
use super::wire::{HistSet, NodeStats, WirePart};
use crate::config::HarnessConfig;
use crate::metrics::{HarnessRoll, Metrics};

/// Descriptors kept free for the node itself (listeners, logs, the control plane).
pub const DESCRIPTOR_RESERVE: u64 = 256;
/// Connect timeout of one connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the scheduler waits for in-flight requests after the last send,
/// on top of the request timeout.
pub const DRAIN_GRACE: Duration = Duration::from_secs(1);
/// How often a closed layer adjusts its number of users.
pub const USERS_TICK: Duration = Duration::from_millis(100);
/// Longest single sleep of the scheduler (so a stop is noticed quickly).
pub const MAX_SLEEP: Duration = Duration::from_millis(50);
/// One-second buckets up to here, then ten-second buckets.
pub const FINE_TIMELINE_SECS: u64 = 3_600;
/// Longest response body read per request (the rest is dropped, not counted).
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Shard-local samples kept until the next roll.
const SHARD_SAMPLES: usize = 32;
/// The open scheduler's longest step: the rate is re-read at least this
/// often (and at every edge of the pattern).
const RATE_STEP: f64 = 0.010;

/// How often the scheduler notes where each layer's pattern is.
const NOTE_EVERY: Duration = Duration::from_millis(100);
/// Seconds the roller may fall behind before the scheduler waits for it.
const ROLL_QUEUE: usize = 64;
/// Rate-limit observations a worker buffers between two rolls (more are
/// counted as dropped).
const MAX_SHARD_OBS: usize = 65_536;
/// Most transport-failed sequence numbers remembered for the `lost` count.
pub const MAX_FAILED_SEQS: usize = 1 << 20;
/// Most failed sequence numbers a node ships to its leader at the end of a
/// cluster run (the rest are counted as untracked).
pub const MAX_WIRE_FAILED: usize = 65_536;
/// Most samples a node ships to its leader with its final stats.
pub const MAX_WIRE_SAMPLES: usize = 100;
/// Most timeline buckets in one heartbeat (a node that follows a new leader
/// sends its whole timeline again, in parts this size).
pub const MAX_WIRE_BUCKETS: usize = 600;

/// The part of a cluster run's load one client node sends: the interval
/// `[from, to)` of the cumulative capacity shares (0 to 1). A single node
/// sends all of it.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Share {
    pub from: f64,
    pub to: f64,
}

impl Default for Share {
    fn default() -> Self {
        Self { from: 0.0, to: 1.0 }
    }
}

impl Share {
    /// The fraction of the rate this node sends.
    pub fn fraction(&self) -> f64 {
        (self.to - self.from).clamp(0.0, 1.0)
    }

    pub fn is_whole(&self) -> bool {
        self.from <= 0.0 && self.to >= 1.0
    }

    /// Valid: `0 <= from <= to <= 1`.
    pub fn valid(&self) -> bool {
        self.from.is_finite()
            && self.to.is_finite()
            && 0.0 <= self.from
            && self.from <= self.to
            && self.to <= 1.0
    }

    /// This node's users out of `total`, by cumulative rounding: the shares
    /// of every node add up to `total` exactly.
    pub fn users(&self, total: f64) -> usize {
        let t = total.round().max(0.0);
        let hi = (t * self.to + 1e-9).floor();
        let lo = (t * self.from + 1e-9).floor();
        (hi - lo).max(0.0) as usize
    }

    /// This node's virtual clients `(base, count)` of `total`: disjoint
    /// ranges over the nodes (at least one each).
    pub fn virtual_clients(&self, total: u32) -> (u32, u32) {
        let total = total.max(1);
        let t = f64::from(total);
        let lo = ((t * self.from + 1e-9).floor() as u32).min(total - 1);
        let hi = ((t * self.to + 1e-9).floor() as u32).min(total);
        (lo, hi.saturating_sub(lo).max(1))
    }
}

/// How the client classifies an answer (plan section 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// A valid served-by tag of this run: an upstream answered.
    Upstream,
    /// An answer without a valid tag: the gateway made it.
    Gateway,
    /// No answer: connect, TLS, timeout, reset, protocol.
    Transport,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Upstream => "upstream",
            Class::Gateway => "gateway",
            Class::Transport => "transport",
        }
    }
}

/// Histogram values as (seconds, count) pairs, for the metrics.
fn pairs(h: &Histogram<u64>) -> Vec<(f64, u64)> {
    h.iter_recorded()
        .map(|v| (v.value_iterated_to() as f64 / 1e6, v.count_at_value()))
        .collect()
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX).max(1)
}

/// Process CPU time (user plus system) in seconds, Linux only.
pub fn cpu_seconds() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let after = stat.rsplit_once(") ")?.1;
    let fields: Vec<&str> = after.split_whitespace().collect();
    let utime: f64 = fields.get(11)?.parse().ok()?;
    let stime: f64 = fields.get(12)?.parse().ok()?;
    Some((utime + stime) / 100.0)
}

/// Raise the soft descriptor limit to the hard limit and return the limit in
/// force (Unix). `None` when it cannot be read.
#[allow(clippy::unnecessary_cast)] // rlim_t is not u64 on every platform
pub fn raise_descriptor_limit() -> Option<u64> {
    #[cfg(unix)]
    {
        // SAFETY: getrlimit and setrlimit only read and write the rlimit
        // struct passed by pointer; it is fully initialised below.
        unsafe {
            let mut lim = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
                return None;
            }
            if lim.rlim_cur < lim.rlim_max {
                let raised = libc::rlimit {
                    rlim_cur: lim.rlim_max,
                    rlim_max: lim.rlim_max,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &raised) == 0 {
                    lim.rlim_cur = lim.rlim_max;
                }
            }
            Some(lim.rlim_cur as u64)
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The in-flight cap for a node: the configured cap, and at most what the
/// descriptor limit allows (plan section 10).
pub fn inflight_cap(configured: usize, descriptor_limit: Option<u64>, both: bool) -> usize {
    let from_descriptors = descriptor_limit.map(|limit| {
        let free = limit.saturating_sub(DESCRIPTOR_RESERVE);
        let per_request = if both { 2 } else { 1 };
        usize::try_from(free / per_request).unwrap_or(usize::MAX)
    });
    match from_descriptors {
        Some(d) => configured.min(d).max(1),
        None => configured.max(1),
    }
}

/// The engine runtime of a client node.
pub struct Engine {
    /// Taken on drop: the runtime shuts down in the background, so a
    /// harness dropped inside an async context (the CLI, tests) never blocks.
    runtime: Option<Runtime>,
    pub threads: usize,
    pub inflight_cap: usize,
    pub descriptor_limit: Option<u64>,
    /// `both` mode: every request costs two descriptors on this node.
    pub both: bool,
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_background();
        }
    }
}

impl Engine {
    pub fn new(config: &HarnessConfig) -> Result<Self, String> {
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        let both = config.is_server();
        let threads = config.threads.unwrap_or(if both {
            cores / 2
        } else {
            cores.saturating_sub(1)
        });
        let threads = threads.max(1);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(threads)
            .thread_name("harness-engine")
            .enable_all()
            .build()
            .map_err(|e| format!("cannot start the harness engine runtime: {e}"))?;
        let descriptor_limit = raise_descriptor_limit();
        Ok(Self {
            runtime: Some(runtime),
            threads,
            inflight_cap: inflight_cap(config.max_inflight, descriptor_limit, both),
            descriptor_limit,
            both,
        })
    }

    pub fn handle(&self) -> Handle {
        match &self.runtime {
            Some(rt) => rt.handle().clone(),
            // Only after drop started; never reached by callers.
            None => Handle::current(),
        }
    }

    /// The caps a scenario is validated against on this node.
    pub fn caps(&self, config: &HarnessConfig) -> Caps {
        let mut caps = Caps::from_config(config);
        caps.max_inflight = caps.max_inflight.min(self.inflight_cap);
        caps
    }

    pub fn describe(&self) -> Value {
        json!({
            "threads": self.threads,
            "inflight_cap": self.inflight_cap,
            "descriptor_limit": self.descriptor_limit,
            "descriptors_per_request": if self.both { 2 } else { 1 },
        })
    }
}

// ── Connections ─────────────────────────────────────────────────────

/// A plain or TLS connection to a pinned address.
pub enum HarnessStream {
    Plain(TcpStream),
    Tls {
        stream: Box<tokio_rustls::client::TlsStream<TcpStream>>,
        h2: bool,
    },
}

impl AsyncRead for HarnessStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            HarnessStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            HarnessStream::Tls { stream, .. } => Pin::new(stream.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for HarnessStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            HarnessStream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            HarnessStream::Tls { stream, .. } => Pin::new(stream.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            HarnessStream::Plain(s) => Pin::new(s).poll_flush(cx),
            HarnessStream::Tls { stream, .. } => Pin::new(stream.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            HarnessStream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            HarnessStream::Tls { stream, .. } => Pin::new(stream.as_mut()).poll_shutdown(cx),
        }
    }
}

impl Connection for HarnessStream {
    fn connected(&self) -> Connected {
        let c = Connected::new();
        match self {
            HarnessStream::Tls { h2: true, .. } => c.negotiated_h2(),
            _ => c,
        }
    }
}

/// Accepts any server certificate (`insecure: true`), with the provider's
/// signature checks kept so the handshake itself is still well formed.
#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// rustls client configuration for a target.
pub fn tls_config(target: &Target) -> Result<rustls::ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS configuration: {e}"))?;
    let mut config = if target.insecure {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        builder.with_root_certificates(roots).with_no_client_auth()
    };
    config.alpn_protocols = match target.http {
        HttpVersion::Auto => vec![b"h2".to_vec(), b"http/1.1".to_vec()],
        HttpVersion::H1 | HttpVersion::H2c => vec![b"http/1.1".to_vec()],
        HttpVersion::H2 => vec![b"h2".to_vec()],
    };
    Ok(config)
}

/// Connects to the run's pinned addresses only, round robin, with
/// `TCP_NODELAY`, and TLS with the target's server name.
#[derive(Clone)]
pub struct PinnedConnector {
    addrs: Arc<Vec<SocketAddr>>,
    next: Arc<AtomicUsize>,
    tls: Option<Arc<rustls::ClientConfig>>,
    server_name: Option<rustls::pki_types::ServerName<'static>>,
}

impl PinnedConnector {
    pub fn new(
        addrs: Vec<SocketAddr>,
        tls: Option<rustls::ClientConfig>,
        server_name: &str,
    ) -> Result<Self, String> {
        let server_name = if tls.is_some() {
            Some(
                rustls::pki_types::ServerName::try_from(server_name.to_string())
                    .map_err(|_| format!("{server_name:?} is not a valid TLS server name"))?,
            )
        } else {
            None
        };
        Ok(Self {
            addrs: Arc::new(addrs),
            next: Arc::new(AtomicUsize::new(0)),
            tls: tls.map(Arc::new),
            server_name,
        })
    }
}

type ConnectError = Box<dyn std::error::Error + Send + Sync>;

impl tower::Service<Uri> for PinnedConnector {
    type Response = TokioIo<HarnessStream>;
    type Error = ConnectError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _uri: Uri) -> Self::Future {
        let addrs = self.addrs.clone();
        let i = self.next.fetch_add(1, Ordering::Relaxed);
        let tls = self.tls.clone();
        let server_name = self.server_name.clone();
        Box::pin(async move {
            let addr = addrs
                .get(i % addrs.len().max(1))
                .copied()
                .ok_or_else(|| ConnectError::from("no pinned address"))?;
            let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
                .await
                .map_err(|_| ConnectError::from(format!("connect to {addr} timed out")))?
                .map_err(|e| ConnectError::from(format!("connect to {addr}: {e}")))?;
            let _ = stream.set_nodelay(true);
            match (tls, server_name) {
                (Some(config), Some(name)) => {
                    let connector = tokio_rustls::TlsConnector::from(config);
                    let tls_stream = connector.connect(name, stream).await.map_err(|e| {
                        ConnectError::from(format!("tls handshake with {addr}: {e}"))
                    })?;
                    let h2 = tls_stream
                        .get_ref()
                        .1
                        .alpn_protocol()
                        .is_some_and(|p| p == b"h2");
                    Ok(TokioIo::new(HarnessStream::Tls {
                        stream: Box::new(tls_stream),
                        h2,
                    }))
                }
                _ => Ok(TokioIo::new(HarnessStream::Plain(stream))),
            }
        })
    }
}

pub type HttpClient = Client<PinnedConnector, Full<Bytes>>;

/// A target with its pinned addresses and its client.
pub struct PinnedTarget {
    pub target: Target,
    pub parsed: ParsedTarget,
    pub addrs: Vec<SocketAddr>,
    pub verified: bool,
    pub client: HttpClient,
    /// `scheme://authority` the requests name (the Host override when set).
    pub authority: String,
}

impl PinnedTarget {
    /// Build the client (inside the engine runtime: the pool needs it).
    pub fn new(
        handle: &Handle,
        target: Target,
        parsed: ParsedTarget,
        addrs: Vec<SocketAddr>,
        pool_size: usize,
    ) -> Result<Self, String> {
        let server_name = target
            .host
            .as_deref()
            .map(|h| h.rsplit_once(':').map_or(h, |(n, _)| n).to_string())
            .unwrap_or_else(|| parsed.host.clone());
        let tls = if parsed.tls() {
            Some(tls_config(&target)?)
        } else {
            None
        };
        let connector = PinnedConnector::new(addrs.clone(), tls, &server_name)?;
        let _enter = handle.enter();
        let mut builder = Client::builder(TokioExecutor::new());
        builder
            .pool_max_idle_per_host(pool_size.max(1))
            .pool_idle_timeout(Duration::from_secs(60))
            .pool_timer(TokioTimer::new())
            .timer(TokioTimer::new());
        if target.http == HttpVersion::H2c {
            builder.http2_only(true);
        }
        let client = builder.build(connector);
        let authority = match &target.host {
            Some(h) => {
                let h = h.trim_end_matches('.');
                let host = if h.contains(':') && !h.starts_with('[') && h.matches(':').count() > 1 {
                    format!("[{h}]")
                } else {
                    h.to_string()
                };
                format!("{}://{host}", parsed.scheme)
            }
            None => parsed.origin(),
        };
        Ok(Self {
            target,
            parsed,
            addrs,
            verified: false,
            client,
            authority,
        })
    }

    pub fn report(&self) -> TargetReport {
        TargetReport {
            url: self.target.url.clone(),
            weight: self.target.weight,
            http: self.target.http.as_str().into(),
            pinned: self.addrs.iter().map(|a| a.to_string()).collect(),
            verified: self.verified,
        }
    }
}

// ── Recording ───────────────────────────────────────────────────────

/// What one request came to.
pub struct Outcome {
    pub layer: usize,
    pub target: usize,
    pub seq: u64,
    pub request_id: String,
    pub method: String,
    pub path: String,
    pub status: Option<u16>,
    /// Transport failure kind (`connect`, `tls`, `timeout`, `reset`, `protocol`).
    pub transport: Option<&'static str>,
    pub expected: bool,
    pub bytes: u64,
    pub corrected: Duration,
    pub service: Duration,
    pub at: Duration,
    pub class: Class,
    /// The server's processing time (`Server-Timing: rb`), tagged answers only.
    pub upstream: Option<Duration>,
    /// `service` minus `upstream`: gateway plus network.
    pub overhead: Option<Duration>,
    /// One-way legs when client and server share a clock (loop-through).
    pub request_leg: Option<Duration>,
    pub response_leg: Option<Duration>,
    /// Answered with one of the layer's rate-limit refusal statuses.
    pub limited: bool,
    /// What the rate-limit analysis counts (runs that analyse rate limits).
    pub limit: Option<(Grid, Obs)>,
}

/// Per-layer or per-target accumulator in a shard.
struct PartShard {
    counts: Counts,
    corrected: Histogram<u64>,
    overhead: Histogram<u64>,
}

impl PartShard {
    fn new() -> Self {
        Self {
            counts: Counts::default(),
            corrected: histogram(2),
            overhead: histogram(2),
        }
    }

    fn merge_from(&mut self, src: &mut PartShard) {
        // Nothing recorded since the last roll: most layer slots stay empty
        // (they are room for layers added while the run runs), and merging
        // a histogram walks all its buckets.
        if src.counts.completed == 0 {
            return;
        }
        let _ = self.corrected.add(&src.corrected);
        let _ = self.overhead.add(&src.overhead);
        src.corrected.reset();
        src.overhead.reset();
        self.counts.add(&std::mem::take(&mut src.counts));
    }

    fn record(&mut self, o: &Outcome, corrected: u64) {
        self.counts.completed += 1;
        self.counts.bytes_received += o.bytes;
        self.corrected.saturating_record(corrected);
        if let Some(over) = o.overhead {
            self.overhead.saturating_record(micros(over));
        }
        count_class(&mut self.counts, o);
    }
}

/// Count one answer's status, class and expectation.
fn count_class(counts: &mut Counts, o: &Outcome) {
    match (o.status, o.transport) {
        (Some(status), _) => {
            *counts.statuses.entry(status.to_string()).or_default() += 1;
            if o.limited {
                counts.limited += 1;
            }
            if o.expected {
                counts.ok += 1;
            } else {
                counts.unexpected_status += 1;
            }
            match o.class {
                Class::Upstream => counts.upstream += 1,
                Class::Gateway => {
                    counts.gateway += 1;
                    *counts
                        .gateway_statuses
                        .entry(status.to_string())
                        .or_default() += 1;
                }
                Class::Transport => {}
            }
        }
        (None, kind) => {
            counts.transport += 1;
            *counts
                .transport_kinds
                .entry(kind.unwrap_or("other").to_string())
                .or_default() += 1;
        }
    }
}

/// One worker thread's accumulator since the last roll.
struct Shard {
    counts: Counts,
    corrected: Histogram<u64>,
    service: Histogram<u64>,
    upstream: Histogram<u64>,
    overhead: Histogram<u64>,
    request_leg: Histogram<u64>,
    response_leg: Histogram<u64>,
    layers: Vec<PartShard>,
    targets: Vec<PartShard>,
    samples: Vec<Sample>,
    /// Sequence numbers of transport failures (for the `lost` count).
    failed_seqs: Vec<u64>,
    /// The rate-limit observations since the last roll (layer, grid,
    /// observation): the roll counts them into the run's accumulators, in
    /// this worker's completion order.
    limits: Vec<(usize, Grid, Obs)>,
    /// Per layer: observations over [`MAX_SHARD_OBS`].
    limits_dropped: Vec<u64>,
}

impl Shard {
    fn new(layers: usize, targets: usize) -> Self {
        Self {
            limits: Vec::new(),
            limits_dropped: vec![0; layers],
            counts: Counts::default(),
            corrected: histogram(3),
            service: histogram(3),
            upstream: histogram(3),
            overhead: histogram(3),
            request_leg: histogram(3),
            response_leg: histogram(3),
            layers: (0..layers).map(|_| PartShard::new()).collect(),
            targets: (0..targets).map(|_| PartShard::new()).collect(),
            samples: Vec::new(),
            failed_seqs: Vec::new(),
        }
    }

    fn record(&mut self, o: &Outcome, layer_name: &str, target_url: &str) {
        let corrected = micros(o.corrected);
        let service = micros(o.service);
        self.counts.completed += 1;
        self.counts.bytes_received += o.bytes;
        count_class(&mut self.counts, o);
        self.corrected.saturating_record(corrected);
        self.service.saturating_record(service);
        if let Some(up) = o.upstream {
            self.upstream.saturating_record(micros(up));
        }
        if let Some(over) = o.overhead {
            self.overhead.saturating_record(micros(over));
        }
        if let Some(leg) = o.request_leg {
            self.request_leg.saturating_record(micros(leg));
        }
        if let Some(leg) = o.response_leg {
            self.response_leg.saturating_record(micros(leg));
        }
        if o.class == Class::Transport && self.failed_seqs.len() < MAX_FAILED_SEQS {
            self.failed_seqs.push(o.seq);
        }
        if let Some(p) = self.layers.get_mut(o.layer) {
            p.record(o, corrected);
        }
        if let Some(p) = self.targets.get_mut(o.target) {
            p.record(o, corrected);
        }
        if let Some((grid, obs)) = &o.limit {
            if self.limits.len() < MAX_SHARD_OBS {
                self.limits.push((o.layer, *grid, obs.clone()));
            } else if let Some(n) = self.limits_dropped.get_mut(o.layer) {
                *n += 1;
            }
        }
        let is_error = o.transport.is_some() || !o.expected;
        if is_error || self.samples.len() < SHARD_SAMPLES {
            if self.samples.len() >= SHARD_SAMPLES {
                // Keep the first errors; replace a slow sample.
                if let Some(pos) = self.samples.iter().position(|s| s.kind == "slowest") {
                    self.samples.remove(pos);
                } else {
                    return;
                }
            }
            self.samples.push(Sample {
                kind: if is_error { "error" } else { "slowest" }.into(),
                class: o.class.as_str().into(),
                seq: o.seq,
                request_id: o.request_id.clone(),
                layer: layer_name.into(),
                target: target_url.into(),
                method: o.method.clone(),
                path: o.path.clone(),
                status: o.status,
                error: o.transport.map(str::to_string),
                latency_ms: o.corrected.as_secs_f64() * 1000.0,
                service_ms: o.service.as_secs_f64() * 1000.0,
                upstream_ms: o.upstream.map(|d| d.as_secs_f64() * 1000.0),
                at_secs: o.at.as_secs_f64(),
            });
        }
    }
}

/// Run-wide accumulators (the scheduler thread merges shards into them).
struct Totals {
    counts: Counts,
    corrected: Histogram<u64>,
    service: Histogram<u64>,
    upstream: Histogram<u64>,
    overhead: Histogram<u64>,
    request_leg: Histogram<u64>,
    response_leg: Histogram<u64>,
    layers: Vec<PartShard>,
    targets: Vec<PartShard>,
    lag: Histogram<u64>,
    samples: Vec<Sample>,
    failed_seqs: Vec<u64>,
    failed_overflow: u64,
    timeline: Vec<Bucket>,
    saturated_buckets: Vec<f64>,
    saturation_reasons: Vec<String>,
    saturated: bool,
    consecutive_saturated: u32,
    /// Per layer: the rate-limit counters of the run.
    limits: Vec<Option<LimitAcc>>,
}

impl Totals {
    fn new(layers: usize, targets: usize) -> Self {
        Self {
            limits: vec![None; layers],
            counts: Counts::default(),
            corrected: histogram(3),
            service: histogram(3),
            upstream: histogram(3),
            overhead: histogram(3),
            request_leg: histogram(3),
            response_leg: histogram(3),
            layers: (0..layers).map(|_| PartShard::new()).collect(),
            targets: (0..targets).map(|_| PartShard::new()).collect(),
            lag: histogram(3),
            samples: Vec::new(),
            failed_seqs: Vec::new(),
            failed_overflow: 0,
            timeline: Vec::new(),
            saturated_buckets: Vec::new(),
            saturation_reasons: Vec::new(),
            saturated: false,
            consecutive_saturated: 0,
        }
    }

    fn keep_samples(&mut self, mut incoming: Vec<Sample>) {
        self.samples.append(&mut incoming);
        if self.samples.len() > MAX_SAMPLES {
            // Errors first (oldest kept), then the slowest.
            self.samples.sort_by(|a, b| {
                (b.kind == "error")
                    .cmp(&(a.kind == "error"))
                    .then_with(|| b.latency_ms.total_cmp(&a.latency_ms))
            });
            self.samples.truncate(MAX_SAMPLES);
        }
    }
}

thread_local! {
    static SHARD_INDEX: Cell<usize> = const { Cell::new(usize::MAX) };
}

static NEXT_SHARD: AtomicUsize = AtomicUsize::new(0);

/// The shard of the current thread (workers get their own; anything else
/// shares the last one).
fn shard_index(count: usize) -> usize {
    SHARD_INDEX.with(|cell| {
        let mut i = cell.get();
        if i == usize::MAX {
            i = NEXT_SHARD.fetch_add(1, Ordering::Relaxed);
            cell.set(i);
        }
        i % count
    })
}

// ── Runs ────────────────────────────────────────────────────────────

/// Where a run is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Starting,
    Running,
    Draining,
    Finished,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::Running => "running",
            State::Draining => "draining",
            State::Finished => "finished",
        }
    }
}

/// Everything a run needs before it starts.
pub struct RunParams {
    pub id: String,
    pub node_id: String,
    pub scenario: Scenario,
    pub targets: Vec<PinnedTarget>,
    pub caps: Caps,
    pub threads: usize,
    pub inflight_cap: usize,
    pub descriptor_limit: Option<u64>,
    pub report_dir: Option<PathBuf>,
    pub kill: Arc<KillSwitch>,
    /// Signs the markers and verifies the tags.
    pub key: RunKey,
    /// This node's server side of the run (`None` on a client-only node).
    pub entry: Option<Arc<RunEntry>>,
    pub correlator: Arc<Correlator>,
    pub metrics: Option<Arc<Metrics>>,
    /// A cluster run: this node's share of the load (default: all of it).
    pub share: Share,
    /// The synchronised start (cluster runs): the scheduler waits for it.
    pub start_at: Option<Instant>,
    /// The dead man's switch (cluster runs): no leader answer for the lease
    /// period stops the run.
    pub lease: Option<Arc<Lease>>,
    /// The cluster the run belongs to.
    pub cluster: Option<String>,
    /// Live changes already made (a cluster member joining a run late);
    /// default: none.
    pub controls: Option<Controls>,
}

/// One execution of a scenario on this node.
pub struct Run {
    pub id: String,
    pub node_id: String,
    pub scenario: Scenario,
    pub hash: String,
    pub targets: Vec<PinnedTarget>,
    pub caps: Caps,
    pub threads: usize,
    pub inflight_cap: usize,
    pub descriptor_limit: Option<u64>,
    pub report_dir: Option<PathBuf>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub created: Instant,
    started: OnceLock<Instant>,
    kill: Arc<KillSwitch>,
    state: Mutex<State>,
    stop: AtomicBool,
    stop_reason: Mutex<Option<String>>,
    seq: AtomicU64,
    inflight: AtomicUsize,
    sent: AtomicU64,
    not_sent: AtomicU64,
    requested: AtomicU64,
    shards: Vec<Mutex<Shard>>,
    totals: Mutex<Totals>,
    report: OnceLock<Report>,
    done: watch::Sender<bool>,
    target_weights: Vec<u32>,
    /// Per layer: requested, sent and not sent (scheduler and user side).
    layer_sched: Vec<[AtomicU64; 3]>,
    key: RunKey,
    entry: Option<Arc<RunEntry>>,
    correlator: Arc<Correlator>,
    metrics: Option<Arc<Metrics>>,
    rejected_before: u64,
    /// This node's part of a cluster run; the leader changes it when it
    /// re-splits the rate (read by the scheduler on every pass).
    share: Mutex<Share>,
    /// Live changes and the closed loops' state (section 6.5).
    controls: Mutex<Controls>,
    /// The layers as the scheduler evaluates them (rebuilt on a change).
    view: Mutex<Arc<LiveView>>,
    /// Where each layer's pattern was as the run went (the closed loops
    /// and the report's stages read it).
    stages: Mutex<StageLog>,
    pub start_at: Option<Instant>,
    start_lag: OnceLock<Duration>,
    lease: Option<Arc<Lease>>,
    pub cluster: Option<String>,
    /// Unix time (seconds) of the run's start: stages aligned to the wall
    /// clock and the rate-limit grids follow it.
    wall_start: OnceLock<f64>,
}

impl Run {
    pub fn new(params: RunParams) -> Arc<Self> {
        let shard_count = params.threads + 2;
        // Room for layers added while running.
        let layers = MAX_LAYERS.max(params.scenario.layers.len());
        let targets = params.targets.len();
        let (done, _) = watch::channel(false);
        let target_weights = cumulative(params.targets.iter().map(|t| t.target.weight));
        let controls = params
            .controls
            .unwrap_or_else(|| Controls::for_scenario(&params.scenario));
        let view = Arc::new(LiveView::build(
            &params.scenario,
            &controls,
            &params.id,
            &params.node_id,
            params.share,
        ));
        Arc::new(Self {
            hash: params.scenario.hash(),
            id: params.id,
            node_id: params.node_id,
            scenario: params.scenario,
            targets: params.targets,
            caps: params.caps,
            threads: params.threads,
            inflight_cap: params.inflight_cap,
            descriptor_limit: params.descriptor_limit,
            report_dir: params.report_dir,
            created_at: chrono::Utc::now(),
            created: Instant::now(),
            started: OnceLock::new(),
            kill: params.kill,
            state: Mutex::new(State::Starting),
            stop: AtomicBool::new(false),
            stop_reason: Mutex::new(None),
            seq: AtomicU64::new(0),
            inflight: AtomicUsize::new(0),
            sent: AtomicU64::new(0),
            not_sent: AtomicU64::new(0),
            requested: AtomicU64::new(0),
            shards: (0..shard_count)
                .map(|_| Mutex::new(Shard::new(layers, targets)))
                .collect(),
            totals: Mutex::new(Totals::new(layers, targets)),
            report: OnceLock::new(),
            done,
            target_weights,
            layer_sched: (0..layers)
                .map(|_| [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)])
                .collect(),
            key: params.key,
            entry: params.entry,
            rejected_before: params.correlator.rejected(),
            correlator: params.correlator,
            metrics: params.metrics,
            share: Mutex::new(params.share),
            controls: Mutex::new(controls),
            view: Mutex::new(view),
            stages: Mutex::new(StageLog::default()),
            start_at: params.start_at,
            start_lag: OnceLock::new(),
            lease: params.lease,
            cluster: params.cluster,
            wall_start: OnceLock::new(),
        })
    }

    /// The run key (tests spoof markers with another one).
    pub fn key(&self) -> &RunKey {
        &self.key
    }

    /// Count a request the scheduler (or a user) asked for in `layer`:
    /// `sent` true when it went out, false when the in-flight cap held it.
    fn count_layer(&self, layer: usize, sent: bool) {
        if let Some(c) = self.layer_sched.get(layer) {
            c[0].fetch_add(1, Ordering::Relaxed);
            c[usize::from(!sent) + 1].fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn state(&self) -> State {
        *self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn set_state(&self, s: State) {
        *self.state.lock().unwrap_or_else(|p| p.into_inner()) = s;
    }

    pub fn is_active(&self) -> bool {
        self.state() != State::Finished
    }

    /// Ask the run to stop (`reason`: `user`, `kill_switch`, ...). The
    /// scheduler notices within [`MAX_SLEEP`] and drains.
    pub fn stop(&self, reason: &str) {
        let mut r = self.stop_reason.lock().unwrap_or_else(|p| p.into_inner());
        if r.is_none() {
            *r = Some(reason.to_string());
        }
        drop(r);
        self.stop.store(true, Ordering::SeqCst);
    }

    fn stop_requested(&self) -> bool {
        if self.stop.load(Ordering::Relaxed) {
            return true;
        }
        if self.kill.is_set() {
            self.stop("kill_switch");
            return true;
        }
        if self.lease.as_ref().is_some_and(|l| l.expired()) {
            tracing::warn!(
                target: "rustybin::harness",
                run = %self.id,
                cluster = ?self.cluster,
                "harness lease expired (no answer from the leader): the run stops"
            );
            self.stop("lease_expired");
            return true;
        }
        false
    }

    /// Requests sent so far.
    pub fn sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }

    /// This node's share of a cluster run (all of it on a single node).
    pub fn share(&self) -> Share {
        *self.share.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A new share from the leader (a re-split). Returns whether it changed.
    pub fn set_share(&self, share: Share) -> bool {
        {
            let mut cur = self.share.lock().unwrap_or_else(|p| p.into_inner());
            if *cur == share {
                return false;
            }
            *cur = share;
        }
        // The phase of `sync: node` patterns follows the share.
        self.rebuild_view();
        true
    }

    /// The run as the scheduler evaluates it now: the layers (added ones
    /// too) with their patterns and live changes.
    pub fn view(&self) -> Arc<LiveView> {
        self.view.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn controls(&self) -> Controls {
        self.controls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn rebuild_view(&self) {
        let controls = self.controls();
        let view = LiveView::build(
            &self.scenario,
            &controls,
            &self.id,
            &self.node_id,
            self.share(),
        )
        .with_wall(self.wall_start.get().copied());
        *self.view.lock().unwrap_or_else(|p| p.into_inner()) = Arc::new(view);
        // A change shows in the stages from the moment it is made.
        self.note_stages();
    }

    /// Note where each layer is now (every 100 ms from the scheduler, and
    /// on every change).
    fn note_stages(&self) {
        if self.started.get().is_none() {
            return;
        }
        let mut log = self.stages.lock().unwrap_or_else(|p| p.into_inner());
        log.note(&self.view(), self.run_secs());
    }

    /// The layers stop sending: the seconds after (the drain) are in no
    /// stage.
    fn end_stages(&self) {
        let layers = self.view().layers.len();
        let t = self.run_secs();
        self.stages
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .end(layers, t);
    }

    /// Install controls (a change, the closed loops, or the leader's): the
    /// scheduler applies them on its next pass.
    pub fn set_controls(&self, controls: Controls) {
        *self.controls.lock().unwrap_or_else(|p| p.into_inner()) = controls;
        self.rebuild_view();
    }

    /// Seconds on the run's clock: since the scheduler started (0 before).
    pub fn run_secs(&self) -> f64 {
        self.started
            .get()
            .map_or(0.0, |s| s.elapsed().as_secs_f64())
    }

    /// A live change on a single-node run: checked like a new scenario
    /// (this node's caps included), then applied on the next pass.
    pub fn apply_change(&self, change: &Change) -> Result<Controls, Vec<String>> {
        if !self.is_active() {
            return Err(vec!["the run has finished".into()]);
        }
        let current = self.controls();
        let next = current.apply(&self.scenario, change, self.run_secs())?;
        let problems = next.check(&self.scenario, &self.caps, self.share().fraction());
        if !problems.is_empty() {
            return Err(problems);
        }
        self.set_controls(next.clone());
        tracing::info!(
            target: "rustybin::harness",
            run = %self.id,
            version = next.version,
            change = %change.describe(),
            "harness run changed"
        );
        Ok(next)
    }

    /// Controls from the leader (a cluster member): checked against this
    /// node's caps for its share. Returns the problems when refused.
    pub fn follow_controls(&self, controls: Controls) -> Result<(), Vec<String>> {
        if controls.version <= self.controls().version {
            return Ok(());
        }
        let problems = controls.check(&self.scenario, &self.caps, self.share().fraction());
        if !problems.is_empty() {
            return Err(problems);
        }
        self.set_controls(controls);
        Ok(())
    }

    /// A single-node run's closed loops over the seconds rolled so far
    /// (`last`: the run ended, judge what is open). A cluster's leader runs
    /// them on the merged results instead.
    fn run_loops(&self, last: bool) {
        if self.cluster.is_some() {
            return;
        }
        let view = self.view();
        let looping = view.layers.iter().any(|l| {
            l.compiled.stages.iter().any(|s| {
                matches!(
                    s.shape,
                    super::pattern::Shape::Closed { .. } | super::pattern::Shape::Growth { .. }
                )
            })
        });
        if !looping {
            return;
        }
        let mut controls = self.controls();
        let changed = {
            let totals = self.totals.lock().unwrap_or_else(|p| p.into_inner());
            let stages = self.stages.lock().unwrap_or_else(|p| p.into_inner());
            super::live::run_loops(&mut controls, &view, &stages, &totals.timeline, last)
        };
        if changed {
            controls.version += 1;
            self.set_controls(controls);
        }
    }

    /// This node's virtual clients `(base, count)`: `{{vu}}` runs from
    /// `base` over `count` values.
    pub fn virtual_clients(&self) -> (u32, u32) {
        self.share()
            .virtual_clients(self.scenario.scale.virtual_clients)
    }

    /// The scheduler lag p99 of the last full second, in milliseconds.
    pub fn last_lag_p99_ms(&self) -> f64 {
        self.totals
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .timeline
            .last()
            .map_or(0.0, |b| b.lag_p99_ms)
    }

    /// Why the run was asked to stop, if it was.
    pub fn stop_reason(&self) -> Option<String> {
        self.stop_reason
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// How late the scheduler started after the synchronised start.
    pub fn start_lag(&self) -> Option<Duration> {
        self.start_lag.get().copied()
    }

    pub fn report(&self) -> Option<&Report> {
        self.report.get()
    }

    /// Resolves when the run finished (at once if it already did).
    pub async fn wait(&self) {
        let mut rx = self.done.subscribe();
        if self.report.get().is_some() {
            return;
        }
        let _ = rx.wait_for(|v| *v).await;
    }

    fn elapsed(&self) -> Duration {
        self.started.get().map_or(Duration::ZERO, |s| s.elapsed())
    }

    /// Live view for `GET .../runs/{run}`.
    pub fn snapshot(&self) -> Value {
        let planned = self.scenario.duration().as_secs_f64();
        let elapsed = self.elapsed().as_secs_f64();
        let totals = self.totals.lock().unwrap_or_else(|p| p.into_inner());
        let last = totals.timeline.last().cloned();
        let latency = Percentiles::from_histogram(&totals.corrected);
        let view = self.view();
        let share = self.share().fraction();
        let run_secs = self.run_secs();
        let layers: Vec<Value> = view
            .layers
            .iter()
            .zip(&totals.layers)
            .enumerate()
            .map(|(i, (l, p))| {
                let target_now = match view.level(i, run_secs) {
                    Level::Run(v) if self.is_active() => Some(v * share),
                    _ => None,
                };
                json!({
                    "name": l.def.name,
                    "model": l.def.model.as_str(),
                    "completed": p.counts.completed,
                    "ok": p.counts.ok,
                    "errors": p.counts.errors(),
                    "p99_ms": Percentiles::from_histogram(&p.corrected).p99_ms,
                    "target_now": target_now,
                    "multiplier": l.control.multiplier,
                    "paused": l.control.paused || view.paused,
                    "held": l.control.held_at.is_some(),
                    "removed": l.control.removed,
                    "stage": view.position(i, run_secs).map(|(s, part)| json!({ "index": s, "part": part, "type": l.compiled.stages.get(s).map(|st| st.kind) })),
                })
            })
            .collect();
        json!({
            "run_id": self.id,
            "scenario": self.scenario.name,
            "hash": self.hash,
            "node_id": self.node_id,
            "cluster": self.cluster,
            "share": self.share().fraction(),
            "state": self.state().as_str(),
            "created_at": self.created_at.to_rfc3339(),
            "elapsed_secs": elapsed,
            "multiplier": view.multiplier,
            "paused": view.paused,
            "controls_version": view.version,
            "planned_secs": planned,
            "progress": if planned > 0.0 { (elapsed / planned).min(1.0) } else { 1.0 },
            "requested": self.requested.load(Ordering::Relaxed),
            "sent": self.sent.load(Ordering::Relaxed),
            "not_sent": self.not_sent.load(Ordering::Relaxed),
            "in_flight": self.inflight.load(Ordering::Relaxed),
            "completed": totals.counts.completed,
            "ok": totals.counts.ok,
            "errors": totals.counts.errors(),
            "upstream": totals.counts.upstream,
            "gateway": totals.counts.gateway,
            "transport": totals.counts.transport,
            "statuses": totals.counts.statuses,
            "gateway_statuses": totals.counts.gateway_statuses,
            "transport_kinds": totals.counts.transport_kinds,
            "latency": latency,
            "upstream_latency": Percentiles::from_histogram(&totals.upstream),
            "overhead": Percentiles::from_histogram(&totals.overhead),
            "server_arrivals": self.entry.as_ref().map(|e| e.stats.lock().unwrap_or_else(|p| p.into_inner()).arrivals),
            "last_second": last,
            "saturated": totals.saturated,
            "layers": layers,
            "report_ready": self.report.get().is_some(),
            "exit_code": self.report.get().map(|r| r.exit_code),
            "passed": self.report.get().map(|r| r.passed),
        })
    }

    /// One-line summary for listings.
    pub fn summary(&self) -> Value {
        json!({
            "run_id": self.id,
            "scenario": self.scenario.name,
            "cluster": self.cluster,
            "state": self.state().as_str(),
            "created_at": self.created_at.to_rfc3339(),
            "elapsed_secs": self.elapsed().as_secs_f64(),
            "planned_secs": self.scenario.duration().as_secs_f64(),
            "sent": self.sent.load(Ordering::Relaxed),
            "passed": self.report.get().map(|r| r.passed),
            "exit_code": self.report.get().map(|r| r.exit_code),
        })
    }

    /// The cumulative stats a cluster member sends its leader: counters and
    /// exact histograms since the run started, the timeline from bucket
    /// `timeline_from` on, and (once the run ended) samples and the failed
    /// sequence numbers for the `lost` check.
    pub fn export(&self, timeline_from: usize) -> NodeStats {
        let report = self.report.get();
        let nlayers = self.view().layers.len();
        let controls_version = self.controls().version;
        let totals = self.totals.lock().unwrap_or_else(|p| p.into_inner());
        let part = |p: &PartShard| WirePart {
            counts: p.counts.clone(),
            corrected: WireHist::encode(&p.corrected),
            overhead: WireHist::encode(&p.overhead),
        };
        let mut layers: Vec<WirePart> = totals.layers.iter().take(nlayers).map(part).collect();
        for (w, c) in layers.iter_mut().zip(&self.layer_sched) {
            w.counts.requested = c[0].load(Ordering::Relaxed);
            w.counts.sent = c[1].load(Ordering::Relaxed);
            w.counts.not_sent = c[2].load(Ordering::Relaxed);
        }
        let mut targets: Vec<WirePart> = totals.targets.iter().map(part).collect();
        for w in &mut targets {
            w.counts.sent = w.counts.completed;
            w.counts.requested = w.counts.completed;
        }
        let from = timeline_from.min(totals.timeline.len());
        let to = (from + MAX_WIRE_BUCKETS).min(totals.timeline.len());
        let (failed_seqs, failed_untracked) = if report.is_some() {
            let take = totals.failed_seqs.len().min(MAX_WIRE_FAILED);
            (
                report::encode_numbers(&totals.failed_seqs[..take]),
                (totals.failed_seqs.len() - take) as u64 + totals.failed_overflow,
            )
        } else {
            (String::new(), 0)
        };
        let saturation = match report {
            Some(r) => r.saturation.clone(),
            None => Saturation {
                saturated: totals.saturated,
                reasons: totals.saturation_reasons.clone(),
                buckets_secs: totals.saturated_buckets.clone(),
                inflight_cap: self.inflight_cap,
                descriptor_limit: self.descriptor_limit,
                threads: self.threads,
                scheduler_lag: Percentiles::from_histogram(&totals.lag),
                cpu_seconds: None,
            },
        };
        NodeStats {
            run_id: self.id.clone(),
            state: match report {
                Some(r) => r.state.clone(),
                None => self.state().as_str().into(),
            },
            done: report.is_some(),
            error: report.and_then(|r| r.error.clone()),
            stopped_by: self.stop_reason(),
            elapsed_secs: report.map_or(self.elapsed().as_secs_f64(), |r| r.duration_secs),
            start_lag_ms: self.start_lag().map(|d| d.as_secs_f64() * 1000.0),
            in_flight: self.inflight.load(Ordering::Relaxed) as u64,
            counts: totals.counts.clone(),
            hist: HistSet {
                corrected: WireHist::encode(&totals.corrected),
                service: WireHist::encode(&totals.service),
                upstream: WireHist::encode(&totals.upstream),
                overhead: WireHist::encode(&totals.overhead),
                request_leg: WireHist::encode(&totals.request_leg),
                response_leg: WireHist::encode(&totals.response_leg),
                lag: WireHist::encode(&totals.lag),
            },
            layers,
            targets,
            target_reports: self.targets.iter().map(PinnedTarget::report).collect(),
            timeline_from: from,
            timeline: totals.timeline[from..to].to_vec(),
            timeline_total: totals.timeline.len(),
            saturation,
            samples: report
                .map(|r| r.samples.iter().take(MAX_WIRE_SAMPLES).cloned().collect())
                .unwrap_or_default(),
            failed_seqs,
            failed_untracked,
            controls_version,
            limits: if report.is_some() {
                totals.limits.iter().take(nlayers).cloned().collect()
            } else {
                Vec::new()
            },
        }
    }

    fn record(&self, outcome: Outcome) {
        let i = shard_index(self.shards.len());
        let layer_name = self
            .scenario
            .layers
            .get(outcome.layer)
            .map_or("", |l| l.name.as_str());
        let target_url = self
            .targets
            .get(outcome.target)
            .map_or("", |t| t.target.url.as_str());
        let mut shard = self.shards[i].lock().unwrap_or_else(|p| p.into_inner());
        shard.record(&outcome, layer_name, target_url);
    }

    /// Weighted pick of a target by sequence number.
    fn pick_target(&self, seq: u64) -> usize {
        pick(&self.target_weights, seq)
    }

    fn pick_template(&self, layer: usize, seq: u64) -> usize {
        self.view()
            .layers
            .get(layer)
            .map_or(0, |l| pick(&l.weights, seq))
    }

    fn expand(&self, text: &str, layer: &str, seq: u64, vu: u32, id: u32) -> String {
        if !text.contains("{{") {
            return text.to_string();
        }
        let mut out = text.to_string();
        let replacements: [(&str, String); 8] = [
            ("seq", seq.to_string()),
            ("vu", vu.to_string()),
            ("run", self.id.clone()),
            ("node", self.node_id.clone()),
            ("layer", layer.to_string()),
            ("rand", rand::random::<u32>().to_string()),
            ("id", id.to_string()),
            ("ip", identity_ip(id)),
        ];
        for (name, value) in &replacements {
            for form in [format!("{{{{{name}}}}}"), format!("{{{{ {name} }}}}")] {
                if out.contains(&form) {
                    out = out.replace(&form, value);
                }
            }
        }
        out
    }

    fn build_request(
        &self,
        layer_idx: usize,
        tpl: &RequestTemplate,
        target: &PinnedTarget,
        seq: u64,
        vu: u32,
        test: Option<TestReq>,
    ) -> Result<Built, String> {
        use axum::http::{HeaderMap, HeaderName, HeaderValue};
        let view = self.view();
        let live = view.layers.get(layer_idx).ok_or("the layer is gone")?;
        let layer = &live.def;
        let id = test.map_or(0, |t| t.id);
        let path = self.expand(&tpl.path, &layer.name, seq, vu, id);
        let uri: Uri = format!("{}{}{}", target.authority, target.parsed.path, path)
            .parse()
            .map_err(|e| format!("request URI: {e}"))?;
        let method: axum::http::Method = tpl
            .method
            .to_ascii_uppercase()
            .parse()
            .map_err(|_| "request method".to_string())?;
        let request_id = format!("{}-{}-{seq}", self.id, self.node_id);
        let mut headers = HeaderMap::new();
        fn set(headers: &mut HeaderMap, name: &str, value: String) -> Result<(), String> {
            let n = HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| format!("header {name}: {e}"))?;
            let v = HeaderValue::from_str(&value).map_err(|e| format!("header {name}: {e}"))?;
            headers.append(n, v);
            Ok(())
        }
        set(
            &mut headers,
            "user-agent",
            concat!("rustybin-harness/", env!("CARGO_PKG_VERSION")).to_string(),
        )?;
        let mut has_content_type = false;
        for (name, value) in &tpl.headers {
            if name.eq_ignore_ascii_case("content-type") {
                has_content_type = true;
            }
            set(
                &mut headers,
                name,
                self.expand(value, &layer.name, seq, vu, id),
            )?;
        }
        // `many_ips`: the identity's address in the forwarded header,
        // unless the template sets that header itself.
        let forwarded = test
            .and_then(|t| live.behaviours.get(t.chain))
            .and_then(|b| b.as_ref())
            .and_then(|b| b.forwarded_header.as_deref());
        if let Some(name) = forwarded {
            if !headers.contains_key(name) {
                set(&mut headers, name, identity_ip(id))?;
            }
        }
        let body = match &tpl.body {
            None => Bytes::new(),
            Some(Value::String(s)) => Bytes::from(self.expand(s, &layer.name, seq, vu, id)),
            Some(other) => {
                if !has_content_type {
                    set(&mut headers, "content-type", "application/json".to_string())?;
                }
                Bytes::from(self.expand(&other.to_string(), &layer.name, seq, vu, id))
            }
        };
        // The marker: the names of the headers set above (sorted, unique),
        // their digest, the body digest and the path digest, signed.
        let mut names: Vec<String> = headers.keys().map(|k| k.as_str().to_string()).collect();
        names.sort();
        names.dedup();
        let (headers_digest, _) = correlate::digest_headers(&names, &headers);
        let path_and_query = uri
            .path_and_query()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "/".into());
        let sent_ns = correlate::now_ns();
        let mut marker = Marker {
            run: self.id.clone(),
            node: self.node_id.clone(),
            seq,
            vu,
            layer: layer_idx as u32,
            sent_ns,
            names,
            headers_digest,
            body_digest: if body.is_empty() {
                String::new()
            } else {
                correlate::digest([body.as_ref()])
            },
            path_digest: correlate::digest_path(&method, &path_and_query),
            sig: String::new(),
        };
        marker.sign(&self.key);
        set(&mut headers, "x-request-id", request_id.clone())?;
        set(
            &mut headers,
            "traceparent",
            correlate::traceparent(&self.id, &self.node_id, seq),
        )?;
        set(&mut headers, correlate::HEADER, marker.to_header())?;
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .body(Full::new(body))
            .map_err(|e| format!("request: {e}"))?;
        *req.headers_mut() = headers;
        Ok(Built {
            req,
            request_id,
            sent_ns,
        })
    }

    fn mark_started(&self) {
        let now = Instant::now();
        let _ = self.started.set(now);
        // The wall time of the start: a cluster's synchronised start, so
        // every node aligns to the same windows.
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        let late = self
            .start_at
            .map_or(0.0, |at| now.saturating_duration_since(at).as_secs_f64());
        let _ = self.wall_start.set(unix - late);
        self.rebuild_view();
        self.set_state(State::Running);
    }

    /// Roll every shard into the totals and one timeline bucket: the
    /// second `bucket`, with the scheduler lag of its sends.
    fn roll(&self, bucket: &SchedBucket, lag: &Histogram<u64>) {
        let mut merged = histogram(3);
        let mut merged_service = histogram(3);
        let mut merged_upstream = histogram(3);
        let mut merged_overhead = histogram(3);
        let mut counts = Counts::default();
        let mut samples = Vec::new();
        let mut totals = self.totals.lock().unwrap_or_else(|p| p.into_inner());
        for shard in &self.shards {
            let mut s = shard.lock().unwrap_or_else(|p| p.into_inner());
            let _ = merged.add(&s.corrected);
            let _ = merged_service.add(&s.service);
            let _ = merged_upstream.add(&s.upstream);
            let _ = merged_overhead.add(&s.overhead);
            let _ = totals.corrected.add(&s.corrected);
            let _ = totals.service.add(&s.service);
            let _ = totals.upstream.add(&s.upstream);
            let _ = totals.overhead.add(&s.overhead);
            let _ = totals.request_leg.add(&s.request_leg);
            let _ = totals.response_leg.add(&s.response_leg);
            s.corrected.reset();
            s.service.reset();
            s.upstream.reset();
            s.overhead.reset();
            s.request_leg.reset();
            s.response_leg.reset();
            let c = std::mem::take(&mut s.counts);
            counts.add(&c);
            totals.counts.add(&c);
            for (dst, src) in totals.layers.iter_mut().zip(s.layers.iter_mut()) {
                dst.merge_from(src);
            }
            for (dst, src) in totals.targets.iter_mut().zip(s.targets.iter_mut()) {
                dst.merge_from(src);
            }
            for (layer, grid, obs) in s.limits.drain(..) {
                if let Some(slot) = totals.limits.get_mut(layer) {
                    slot.get_or_insert_with(|| LimitAcc::new(grid)).record(&obs);
                }
            }
            for (layer, n) in s.limits_dropped.iter_mut().enumerate() {
                if *n > 0 {
                    if let Some(Some(acc)) = totals.limits.get_mut(layer) {
                        acc.cells_dropped += *n;
                    }
                    *n = 0;
                }
            }
            samples.append(&mut s.samples);
            let room = MAX_FAILED_SEQS.saturating_sub(totals.failed_seqs.len());
            let take = s.failed_seqs.len().min(room);
            totals.failed_overflow += (s.failed_seqs.len() - take) as u64;
            totals.failed_seqs.extend(s.failed_seqs.drain(..take));
            s.failed_seqs.clear();
        }
        totals.keep_samples(samples);
        if let Some(metrics) = &self.metrics {
            let mut requests: Vec<(&'static str, String, u64)> = Vec::new();
            for (status, n) in &counts.statuses {
                let gateway = counts.gateway_statuses.get(status).copied().unwrap_or(0);
                if *n > gateway {
                    requests.push(("upstream", status.clone(), n - gateway));
                }
                if gateway > 0 {
                    requests.push(("gateway", status.clone(), gateway));
                }
            }
            for (kind, n) in &counts.transport_kinds {
                requests.push(("transport", kind.clone(), *n));
            }
            metrics.harness_roll(HarnessRoll {
                run: &self.id,
                requests,
                latency: vec![
                    ("total", pairs(&merged)),
                    ("service", pairs(&merged_service)),
                    ("upstream", pairs(&merged_upstream)),
                    ("overhead", pairs(&merged_overhead)),
                ],
                lag: pairs(lag),
                inflight: self.inflight.load(Ordering::Relaxed) as i64,
            });
        }
        let _ = totals.lag.add(lag);
        let lag_p = Percentiles::from_histogram(lag);
        let p = Percentiles::from_histogram(&merged);
        let width = bucket
            .end
            .saturating_sub(bucket.start)
            .as_secs_f64()
            .max(1e-9);
        let (mut s2, mut s3, mut s4, mut s5) = (0, 0, 0, 0);
        for (code, n) in &counts.statuses {
            match code.chars().next() {
                Some('2') => s2 += n,
                Some('3') => s3 += n,
                Some('4') => s4 += n,
                Some('5') => s5 += n,
                _ => {}
            }
        }
        // A bucket counts as saturated when the cap held requests back or the
        // scheduler could not keep its send times; the run (and the timeline)
        // is flagged only on two such buckets in a row, so one noisy second
        // in a VM is never read as client saturation.
        let suspicious = bucket.not_sent > 0 || lag_p.p99_ms > LAG_SATURATION_MS;
        if suspicious {
            totals.consecutive_saturated += 1;
        } else {
            totals.consecutive_saturated = 0;
        }
        let saturated = suspicious && totals.consecutive_saturated >= 2;
        let mut entry = Bucket {
            t_secs: bucket.start.as_secs_f64(),
            width_secs: width,
            requested: bucket.requested,
            sent: bucket.sent,
            not_sent: bucket.not_sent,
            completed: counts.completed,
            ok: counts.ok,
            errors: counts.errors(),
            transport: counts.transport,
            status_2xx: s2,
            status_3xx: s3,
            status_4xx: s4,
            status_5xx: s5,
            upstream: counts.upstream,
            gateway: counts.gateway,
            limited: counts.limited,
            rate: counts.completed as f64 / width,
            p50_ms: p.p50_ms,
            p99_ms: p.p99_ms,
            max_ms: p.max_ms,
            service_p99_ms: Percentiles::from_histogram(&merged_service).p99_ms,
            upstream_p99_ms: Percentiles::from_histogram(&merged_upstream).p99_ms,
            overhead_p99_ms: Percentiles::from_histogram(&merged_overhead).p99_ms,
            lag_p99_ms: lag_p.p99_ms,
            lag_max_ms: lag_p.max_ms,
            in_flight: self.inflight.load(Ordering::Relaxed) as u64,
            saturated,
        };
        totals.counts.requested += bucket.requested;
        totals.counts.sent += bucket.sent;
        totals.counts.not_sent += bucket.not_sent;
        if saturated {
            entry.saturated = true;
            if !totals.saturated {
                totals.saturated = true;
                let why = if bucket.not_sent > 0 {
                    "the in-flight cap was reached (requests counted as not_sent)"
                } else {
                    "scheduler lag p99 over 5 ms (the client could not keep the send times)"
                };
                totals.saturation_reasons.push(format!(
                    "saturated from {:.0} s: {why}",
                    bucket.start.as_secs_f64()
                ));
            }
            // The bucket before this one was the first of the pair.
            let t = &mut *totals;
            if let Some(prev) = t.timeline.last_mut() {
                if !prev.saturated && t.saturated_buckets.len() < 100 {
                    prev.saturated = true;
                    t.saturated_buckets.push(prev.t_secs);
                }
            }
            if totals.saturated_buckets.len() < 100 {
                totals.saturated_buckets.push(entry.t_secs);
            }
        }
        // Fine buckets for the first hour, then fold into 10 s buckets.
        if bucket.start.as_secs() >= FINE_TIMELINE_SECS {
            let fold = totals
                .timeline
                .last_mut()
                .filter(|last| last.t_secs >= FINE_TIMELINE_SECS as f64 && last.width_secs < 10.0);
            if let Some(last) = fold {
                last.absorb(&entry);
            } else {
                totals.timeline.push(entry);
            }
        } else {
            totals.timeline.push(entry);
        }
    }

    /// Build the final report, store it and finish the run.
    fn finish(&self, start: Instant, cpu_before: Option<f64>, error: Option<String>) {
        self.set_state(State::Finished);
        let elapsed = start.elapsed();
        let mut totals = self.totals.lock().unwrap_or_else(|p| p.into_inner());
        // Lost: a transport failure the server side never saw. With no
        // server side on this node, nothing can be told apart.
        let server = self.entry.as_ref().map(|e| {
            let stats = e.stats.lock().unwrap_or_else(|p| p.into_inner());
            let mut lost = 0;
            let mut after = 0;
            let mut untracked = 0;
            for seq in &totals.failed_seqs {
                match stats.was_seen(&self.node_id, *seq) {
                    Some(true) => after += 1,
                    Some(false) => lost += 1,
                    None => untracked += 1,
                }
            }
            (stats.report(), lost, after, untracked)
        });
        if let Some((_, lost, after, untracked)) = &server {
            totals.counts.lost = *lost;
            totals.counts.transport_after_upstream = *after;
            totals.counts.transport_untracked = *untracked + totals.failed_overflow;
        }
        let server = server.map(|(r, _, _, _)| r);
        let totals = totals;
        let stopped_by = self
            .stop_reason
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let state = if error.is_some() {
            "failed"
        } else if stopped_by.is_some() {
            "stopped"
        } else {
            "finished"
        };
        let latency = Percentiles::from_histogram(&totals.corrected);
        let service_latency = Percentiles::from_histogram(&totals.service);
        let upstream_latency = Percentiles::from_histogram(&totals.upstream);
        let overhead = Percentiles::from_histogram(&totals.overhead);
        // Answers per second over the send window: the last answers arrive
        // one latency after the window ends, so the median latency is taken
        // off the elapsed time (plan section 10).
        let planned = self.scenario.duration().as_secs_f64();
        let tail = Duration::from_secs_f64(latency.p50_ms / 1000.0);
        let window = elapsed.saturating_sub(tail).as_secs_f64().max(1e-9);
        let achieved_rate = totals.counts.completed as f64 / window;
        let requested_rate = if planned > 0.0 {
            totals.counts.requested as f64 / planned.min(elapsed.as_secs_f64().max(1e-9))
        } else {
            0.0
        };
        let saturation = Saturation {
            saturated: totals.saturated,
            reasons: totals.saturation_reasons.clone(),
            buckets_secs: totals.saturated_buckets.clone(),
            inflight_cap: self.inflight_cap,
            descriptor_limit: self.descriptor_limit,
            threads: self.threads,
            scheduler_lag: Percentiles::from_histogram(&totals.lag),
            cpu_seconds: cpu_before.zip(cpu_seconds()).map(|(a, b)| b - a),
        };
        let mut thresholds: Vec<ThresholdResult> = report::evaluate(
            &self.scenario.thresholds,
            &totals.counts,
            &latency,
            &overhead,
            server.as_ref().map(|s| s.arrivals),
            &saturation,
        );
        // The rate-limit analysis and the layers' own thresholds.
        let controls = self.controls();
        let (rate_limits, layer_thresholds) = ratelimit::analyse(
            &self.scenario,
            &controls,
            &totals.limits,
            server.as_ref().map(|s| &s.by_layer),
        );
        thresholds.extend(layer_thresholds);
        let exit_code = Report::exit_code_for(state, &thresholds);
        let breakdown = |names: Vec<String>, parts: &[PartShard]| -> Vec<Breakdown> {
            names
                .into_iter()
                .zip(parts)
                .map(|(name, p)| Breakdown {
                    name,
                    counts: p.counts.clone(),
                    latency: Percentiles::from_histogram(&p.corrected),
                })
                .collect()
        };
        // The layers as they ran (added ones too), and their stages.
        let view = self.view();
        let names: Vec<String> = view.layers.iter().map(|l| l.def.name.clone()).collect();
        let stages = report::stage_reports(
            &totals.timeline,
            &names,
            &self.stages.lock().unwrap_or_else(|p| p.into_inner()),
        );
        let mut layers = breakdown(names, &totals.layers);
        for (b, c) in layers.iter_mut().zip(&self.layer_sched) {
            b.counts.requested = c[0].load(Ordering::Relaxed);
            b.counts.sent = c[1].load(Ordering::Relaxed);
            b.counts.not_sent = c[2].load(Ordering::Relaxed);
        }
        let mut targets_breakdown = breakdown(
            self.targets.iter().map(|t| t.target.url.clone()).collect(),
            &totals.targets,
        );
        for b in &mut targets_breakdown {
            // Targets see what reached them; a skipped request has no target.
            b.counts.sent = b.counts.completed;
            b.counts.requested = b.counts.completed;
        }
        let mut samples = totals.samples.clone();
        samples.sort_by(|a, b| {
            (b.kind == "error")
                .cmp(&(a.kind == "error"))
                .then_with(|| b.latency_ms.total_cmp(&a.latency_ms))
        });
        samples.truncate(MAX_SAMPLES);
        let report = Report {
            run_id: self.id.clone(),
            scenario_name: self.scenario.name.clone(),
            scenario_hash: self.hash.clone(),
            node_id: self.node_id.clone(),
            rustybin_version: env!("CARGO_PKG_VERSION").into(),
            started_at: self.created_at.to_rfc3339(),
            finished_at: chrono::Utc::now().to_rfc3339(),
            duration_secs: elapsed.as_secs_f64(),
            planned_secs: planned,
            state: state.into(),
            stopped_by,
            error,
            caps: self.caps,
            targets: self.targets.iter().map(PinnedTarget::report).collect(),
            totals: totals.counts.clone(),
            latency,
            service_latency,
            upstream_latency,
            overhead,
            request_leg: Percentiles::from_histogram(&totals.request_leg),
            response_leg: Percentiles::from_histogram(&totals.response_leg),
            server,
            rejected_markers: self
                .correlator
                .rejected()
                .saturating_sub(self.rejected_before),
            achieved_rate,
            requested_rate,
            layers,
            targets_breakdown,
            timeline: totals.timeline.clone(),
            saturation,
            passed: exit_code == 0,
            exit_code,
            thresholds,
            samples,
            scenario: report::redact_scenario(
                &serde_json::to_value(&self.scenario).unwrap_or(Value::Null),
            ),
            cluster: None,
            findings: controls.findings.clone(),
            changes: controls.log.clone(),
            stages,
            rate_limits,
        };
        drop(totals);
        if let Some(dir) = &self.report_dir {
            let path = dir.join(format!("{}.json", self.id));
            match serde_json::to_vec_pretty(&report) {
                Ok(bytes) => {
                    if let Err(e) =
                        std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&path, bytes))
                    {
                        tracing::warn!("harness report {} not written: {e}", path.display());
                    }
                }
                Err(e) => tracing::warn!("harness report not serialised: {e}"),
            }
        }
        tracing::info!(
            target: "rustybin::harness",
            run = %self.id,
            scenario = %self.scenario.name,
            state,
            sent = report.totals.sent,
            completed = report.totals.completed,
            errors = report.totals.errors(),
            not_sent = report.totals.not_sent,
            upstream = report.totals.upstream,
            gateway = report.totals.gateway,
            lost = report.totals.lost,
            p99_ms = report.latency.p99_ms,
            overhead_p99_ms = report.overhead.p99_ms,
            saturated = report.saturation.saturated,
            exit_code,
            "harness run ended"
        );
        let _ = self.report.set(report);
        let _ = self.done.send(true);
    }
}

fn cumulative(weights: impl Iterator<Item = u32>) -> Vec<u32> {
    let mut total = 0u32;
    weights
        .map(|w| {
            total = total.saturating_add(w.max(1));
            total
        })
        .collect()
}

fn pick(cumulative: &[u32], seq: u64) -> usize {
    let Some(&total) = cumulative.last() else {
        return 0;
    };
    let point = (seq % u64::from(total.max(1))) as u32;
    cumulative.iter().position(|&c| point < c).unwrap_or(0)
}

/// A built request with its marker facts.
struct Built {
    req: Request<Full<Bytes>>,
    request_id: String,
    sent_ns: u64,
}

/// Scheduler-side counters of the current bucket.
#[derive(Default)]
struct SchedBucket {
    start: Duration,
    end: Duration,
    requested: u64,
    sent: u64,
    not_sent: u64,
}

/// A second that ended, for the roller.
struct RollJob {
    bucket: SchedBucket,
    lag: Histogram<u64>,
    /// Run the closed loops after it (not while draining).
    loops: bool,
}

/// Rolls each second into the totals and the timeline off the scheduler
/// thread: merging histograms walks all their buckets (a few milliseconds
/// a second in a release build, tens in a debug one), and the send times
/// must not wait for it. Rolls inline when the thread cannot start.
struct Roller {
    tx: Option<SyncSender<RollJob>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Roller {
    fn start(run: &Arc<Run>) -> Self {
        let (tx, rx) = sync_channel::<RollJob>(ROLL_QUEUE);
        let worker = run.clone();
        let spawned = std::thread::Builder::new()
            .name("rustybin-harness-roll".into())
            .spawn(move || {
                for job in rx {
                    worker.roll(&job.bucket, &job.lag);
                    if job.loops {
                        worker.run_loops(false);
                    }
                }
            });
        match spawned {
            Ok(handle) => Self {
                tx: Some(tx),
                handle: Some(handle),
            },
            Err(e) => {
                tracing::warn!(
                    target: "rustybin::harness",
                    run = %run.id,
                    "harness: no roller thread ({e}); the scheduler rolls each second itself"
                );
                Self {
                    tx: None,
                    handle: None,
                }
            }
        }
    }

    /// Hand the second that ended to the roller and start the next one.
    fn roll(&self, run: &Run, bucket: &mut SchedBucket, lag: &mut Histogram<u64>, loops: bool) {
        let next = SchedBucket {
            start: bucket.end,
            end: bucket.end + Duration::from_secs(1),
            ..SchedBucket::default()
        };
        let job = RollJob {
            bucket: std::mem::replace(bucket, next),
            lag: std::mem::replace(lag, histogram(3)),
            loops,
        };
        let job = match &self.tx {
            Some(tx) => match tx.send(job) {
                Ok(()) => return,
                Err(e) => e.0,
            },
            None => job,
        };
        run.roll(&job.bucket, &job.lag);
        if job.loops {
            run.run_loops(false);
        }
    }

    /// Wait until every second handed over is rolled.
    fn finish(mut self) {
        drop(self.tx.take());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Why a request failed, as a bounded set of kinds.
fn classify_error(e: &hyper_util::client::legacy::Error) -> &'static str {
    if e.is_connect() {
        let text = e.to_string().to_ascii_lowercase();
        let source = std::error::Error::source(e)
            .map(|s| s.to_string().to_ascii_lowercase())
            .unwrap_or_default();
        if text.contains("tls") || source.contains("tls") || source.contains("certificate") {
            return "tls";
        }
        return "connect";
    }
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = cur {
        if let Some(h) = err.downcast_ref::<hyper::Error>() {
            if h.is_timeout() {
                return "timeout";
            }
            if h.is_parse() || h.is_parse_status() || h.is_parse_too_large() || h.is_user() {
                return "protocol";
            }
            if h.is_closed() || h.is_incomplete_message() || h.is_canceled() {
                return "reset";
            }
        }
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            return match io.kind() {
                std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::UnexpectedEof => "reset",
                std::io::ErrorKind::TimedOut => "timeout",
                _ => "reset",
            };
        }
        cur = err.source();
    }
    let text = e.to_string().to_ascii_lowercase();
    if text.contains("timed out") {
        "timeout"
    } else if text.contains("reset") || text.contains("closed") || text.contains("broken pipe") {
        "reset"
    } else {
        "protocol"
    }
}

/// One request to send: which layer, target and template, its sequence
/// number, virtual client and intended send time.
#[derive(Clone, Copy)]
struct Job {
    layer: usize,
    target: usize,
    template: usize,
    seq: u64,
    vu: u32,
    intended: Instant,
    start: Instant,
    /// Sent while a rate-limit test runs.
    test: Option<TestReq>,
}

/// A request of a rate-limit test: its identity, the test's pattern in the
/// chain, the retries it has left (`retry_wave`), and whether it is one.
#[derive(Clone, Copy, Debug)]
struct TestReq {
    id: u32,
    chain: usize,
    retries_left: u32,
    retry: bool,
}

/// Send one request and record its outcome. The caller counted it in flight.
async fn send_one(run: Arc<Run>, job: Job) {
    let Job {
        layer: layer_idx,
        target: target_idx,
        template: tpl_idx,
        seq,
        vu,
        intended,
        start,
        test,
    } = job;
    let view = run.view();
    let Some(live) = view.layers.get(layer_idx) else {
        run.inflight.fetch_sub(1, Ordering::Relaxed);
        return;
    };
    let templates = live.def.source.templates();
    let Some(tpl) = templates.get(tpl_idx).copied() else {
        run.inflight.fetch_sub(1, Ordering::Relaxed);
        return;
    };
    let target = &run.targets[target_idx];
    let Built {
        req,
        request_id,
        sent_ns,
    } = match run.build_request(layer_idx, tpl, target, seq, vu, test) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("harness request {seq} not built: {e}");
            run.inflight.fetch_sub(1, Ordering::Relaxed);
            return;
        }
    };
    let method = req.method().to_string();
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.to_string())
        .unwrap_or_default();
    let timeout = run.scenario.timeout();
    // The rate-limit headers are read on runs that analyse rate limits, and
    // for `Retry-After` on retries.
    let grid = live.grid;
    let read_headers = grid.is_some() || test.is_some_and(|t| t.retries_left > 0);
    let node_header = live.node_header.clone();
    let sent = Instant::now();
    let result = tokio::time::timeout(timeout, async {
        let resp = target
            .client
            .request(req)
            .await
            .map_err(|e| classify_error(&e))?;
        let status = resp.status().as_u16();
        // The tag and the upstream timing, verified with the run key.
        let tag = resp
            .headers()
            .get(correlate::SERVED_BY)
            .and_then(|v| v.to_str().ok())
            .and_then(Tag::parse)
            .filter(|t| t.run == run.id && t.seq == seq && t.verify(&run.key));
        let upstream_ms = resp
            .headers()
            .get("server-timing")
            .and_then(|v| v.to_str().ok())
            .and_then(correlate::parse_server_timing);
        let observed = if read_headers {
            ratelimit::observe(resp.headers(), node_header.as_deref())
        } else {
            ratelimit::Observed::default()
        };
        let bytes = read_body(resp.into_body()).await?;
        Ok::<_, &'static str>((status, bytes, tag, upstream_ms, observed))
    })
    .await;
    let done = Instant::now();
    let done_ns = correlate::now_ns();
    let (status, transport, bytes, expected, tag, upstream_ms, observed) = match result {
        Ok(Ok((status, bytes, tag, up, observed))) => (
            Some(status),
            None,
            bytes,
            tpl.expects(status),
            tag,
            up,
            observed,
        ),
        Ok(Err(kind)) => (None, Some(kind), 0, false, None, None, Default::default()),
        Err(_) => (
            None,
            Some("timeout"),
            0,
            false,
            None,
            None,
            Default::default(),
        ),
    };
    let limited = status.is_some_and(|s| live.limited.contains(&s));
    let service = done.saturating_duration_since(sent);
    let class = match (status, &tag) {
        (None, _) => Class::Transport,
        (Some(_), Some(_)) => Class::Upstream,
        (Some(_), None) => Class::Gateway,
    };
    let upstream = match (&tag, upstream_ms) {
        (Some(_), Some(ms)) if ms >= 0.0 => Some(Duration::from_secs_f64(ms / 1000.0)),
        _ => None,
    };
    let overhead = upstream.map(|up| service.saturating_sub(up));
    // One-way legs when the answering node shares this clock (loop-through).
    let (request_leg, response_leg) = match &tag {
        Some(t) if t.node == run.node_id => (
            Some(Duration::from_nanos(t.arrived_ns.saturating_sub(sent_ns))),
            Some(Duration::from_nanos(done_ns.saturating_sub(t.done_ns))),
        ),
        _ => (None, None),
    };
    // `retry_wave`: a refused request goes again, at once or after its
    // `Retry-After`.
    if let Some(t) = test.filter(|t| limited && t.retries_left > 0) {
        if let Some(b) = live.behaviours.get(t.chain).and_then(|b| b.as_ref()) {
            let wait = if b.honour_retry_after {
                observed
                    .retry_after_secs
                    .map_or(b.retry_delay, Duration::from_secs_f64)
            } else {
                b.retry_delay
            };
            retry(
                run.clone(),
                Job {
                    test: Some(TestReq {
                        retries_left: t.retries_left - 1,
                        retry: true,
                        ..t
                    }),
                    ..job
                },
                wait.min(MAX_RETRY_WAIT),
            );
        }
    }
    let at = sent.saturating_duration_since(start);
    let limit = grid.map(|g| {
        (
            g,
            Obs {
                at: at.as_secs_f64(),
                // Identities are followed one by one when the test has a pool.
                id: test.and_then(|t| {
                    live.behaviours
                        .get(t.chain)?
                        .as_ref()
                        .filter(|b| b.identities > 1)
                        .map(|_| t.id)
                }),
                target: target_idx,
                limited,
                error: status.is_none(),
                retry: test.is_some_and(|t| t.retry),
                tagged: class == Class::Upstream,
                latency_us: micros(done.saturating_duration_since(intended)),
                headers: observed,
            },
        )
    });
    run.inflight.fetch_sub(1, Ordering::Relaxed);
    run.record(Outcome {
        layer: layer_idx,
        target: target_idx,
        seq,
        request_id,
        method,
        path,
        status,
        transport,
        expected,
        bytes,
        corrected: done.saturating_duration_since(intended),
        service,
        at,
        class,
        upstream,
        overhead,
        request_leg,
        response_leg,
        limited,
        limit,
    });
}

/// Send `job` again after `wait` (a `retry_wave` retry): a new request of
/// the same layer, identity and target. It counts in flight while it waits,
/// so the drain waits for it, and is dropped when the run stops sending.
fn retry(run: Arc<Run>, job: Job, wait: Duration) {
    if run.inflight.load(Ordering::Relaxed) >= run.inflight_cap {
        run.not_sent.fetch_add(1, Ordering::Relaxed);
        run.requested.fetch_add(1, Ordering::Relaxed);
        run.count_layer(job.layer, false);
        let mut totals = run.totals.lock().unwrap_or_else(|p| p.into_inner());
        totals.counts.requested += 1;
        totals.counts.not_sent += 1;
        return;
    }
    run.inflight.fetch_add(1, Ordering::Relaxed);
    tokio::spawn(async move {
        let until = Instant::now() + wait;
        loop {
            if run.stop_requested() || run.state() != State::Running {
                run.inflight.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            let now = Instant::now();
            if now >= until {
                break;
            }
            tokio::time::sleep((until - now).min(Duration::from_millis(100))).await;
        }
        let seq = run.seq.fetch_add(1, Ordering::Relaxed);
        run.requested.fetch_add(1, Ordering::Relaxed);
        run.sent.fetch_add(1, Ordering::Relaxed);
        run.count_layer(job.layer, true);
        {
            let mut totals = run.totals.lock().unwrap_or_else(|p| p.into_inner());
            totals.counts.requested += 1;
            totals.counts.sent += 1;
        }
        send_one(
            run.clone(),
            Job {
                seq,
                intended: Instant::now(),
                ..job
            },
        )
        .await;
    });
}

/// Read and count a response body (bounded).
async fn read_body(mut body: Incoming) -> Result<u64, &'static str> {
    let mut total = 0u64;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| "reset")?;
        if let Some(data) = frame.data_ref() {
            total += data.len() as u64;
            if total > MAX_RESPONSE_BYTES as u64 {
                return Err("protocol");
            }
        }
    }
    Ok(total)
}

/// A closed layer's running user.
struct User {
    stop: Arc<AtomicBool>,
}

struct LayerSched {
    idx: usize,
    finished: bool,
    /// Open: the fraction of the next request the rate has paid so far.
    credit: f64,
    seq: u64,
    model: Model,
    /// Open: the next intended send (offset from the run start).
    next_at: Duration,
    /// Closed: the running users and when they were last adjusted.
    users: Vec<User>,
    next_adjust: Duration,
    vu_cursor: u32,
}

impl LayerSched {
    /// A layer whose pattern starts at run time `starts_at` (seconds).
    fn new(idx: usize, layer: &Layer, starts_at: f64) -> Self {
        let starts_at = Duration::from_secs_f64(starts_at.max(0.0));
        Self {
            idx,
            finished: false,
            credit: 0.0,
            seq: 0,
            model: layer.model,
            next_at: starts_at,
            users: Vec::new(),
            next_adjust: starts_at,
            vu_cursor: 0,
        }
    }
}

/// The scheduler loop of one run: runs on its own OS thread until the
/// patterns end or the run is stopped, then drains and finishes.
pub fn drive(run: Arc<Run>, handle: Handle) {
    let _enter = handle.enter();
    let cpu_before = cpu_seconds();
    // A cluster run starts at the synchronised start, on every node at once.
    if let Some(at) = run.start_at {
        loop {
            let now = Instant::now();
            if now >= at {
                let _ = run.start_lag.set(now - at);
                break;
            }
            if run.stop_requested() {
                run.finish(Instant::now(), cpu_before, None);
                return;
            }
            std::thread::sleep((at - now).min(MAX_SLEEP));
        }
    }
    run.mark_started();
    let start = Instant::now();
    let total_vus = run.scenario.scale.virtual_clients;
    // One per layer of the view; layers added while running join here.
    let mut layers: Vec<LayerSched> = Vec::new();
    let mut bucket = SchedBucket {
        start: Duration::ZERO,
        end: Duration::from_secs(1),
        ..SchedBucket::default()
    };
    let mut lag = histogram(3);
    let roller = Roller::start(&run);
    let mut vu_cursor: u32 = 0;
    let max_duration = run.caps.max_duration();
    let mut next_note = Duration::ZERO;
    loop {
        if run.stop_requested() {
            break;
        }
        // The layers, their patterns and the live changes, as of now.
        let view = run.view();
        if let Some(reason) = &view.stop {
            run.stop(reason);
            break;
        }
        // A run never outlasts the node's duration cap (a held pattern
        // would).
        if start.elapsed() >= max_duration {
            run.stop("max_duration");
            break;
        }
        while layers.len() < view.layers.len() {
            let l = &view.layers[layers.len()];
            layers.push(LayerSched::new(layers.len(), &l.def, l.starts_at));
        }
        // The share can change mid-run (the leader re-splits the rate).
        let part = run.share();
        let share = part.fraction();
        let whole = part.is_whole();
        let (vu_base, vu_count) = part.virtual_clients(total_vus);
        let vu_count = vu_count.max(1);
        let now = start.elapsed();
        if now >= next_note {
            run.note_stages();
            next_note = now + NOTE_EVERY;
        }
        if now >= bucket.end {
            roller.roll(&run, &mut bucket, &mut lag, true);
            continue;
        }
        let mut earliest = bucket.end.min(next_note);
        let mut all_finished = true;
        for layer in layers.iter_mut().filter(|l| !l.finished) {
            all_finished = false;
            let Some(live) = view.layers.get(layer.idx) else {
                layer.finished = true;
                continue;
            };
            match layer.model {
                Model::Open => {
                    // `next_at` is a cursor: the rate's integral up to it is
                    // in `credit`. A request is due when the credit reaches
                    // one, so a curve that climbs from zero is followed
                    // second by second. Steps are at most 10 ms and never
                    // cross a pattern edge (a burst starts on time).
                    let mut wake = None;
                    while layer.next_at <= now && !layer.finished {
                        let t = layer.next_at.as_secs_f64();
                        let rate = match view.level(layer.idx, t) {
                            Level::Ended => {
                                layer.finished = true;
                                break;
                            }
                            Level::Waiting => 0.0,
                            Level::Run(r) => r,
                        };
                        let rate = if whole { rate } else { rate * share };
                        let edge = view.next_boundary(layer.idx, t);
                        let step = (edge - t).clamp(1e-6, RATE_STEP);
                        if rate <= 0.0 {
                            // Nothing to send: on to the next edge or step.
                            layer.next_at += Duration::from_secs_f64(step);
                            continue;
                        }
                        let to_next = (1.0 - layer.credit).max(0.0) / rate;
                        if to_next > step {
                            // The rate may change before then: bank the step.
                            layer.credit += rate * step;
                            layer.next_at += Duration::from_secs_f64(step);
                            continue;
                        }
                        let due = layer.next_at + Duration::from_secs_f64(to_next);
                        if due > now {
                            // Sleep until it is due (the cursor stays).
                            wake = Some(due);
                            break;
                        }
                        layer.next_at = due;
                        layer.credit = 0.0;
                        let intended = start + due;
                        lag.saturating_record(micros(now.saturating_sub(due)));
                        bucket.requested += 1;
                        run.requested.fetch_add(1, Ordering::Relaxed);
                        let seq = run.seq.fetch_add(1, Ordering::Relaxed);
                        if run.inflight.load(Ordering::Relaxed) >= run.inflight_cap {
                            bucket.not_sent += 1;
                            run.not_sent.fetch_add(1, Ordering::Relaxed);
                            run.count_layer(layer.idx, false);
                        } else {
                            run.inflight.fetch_add(1, Ordering::Relaxed);
                            bucket.sent += 1;
                            run.sent.fetch_add(1, Ordering::Relaxed);
                            run.count_layer(layer.idx, true);
                            let vu = vu_base + vu_cursor % vu_count;
                            vu_cursor = vu_cursor.wrapping_add(1);
                            let tpl = pick(&live.weights, layer.seq);
                            let target = run.pick_target(seq);
                            // A rate-limit test: the next identity of its pool.
                            let test = view.behaviour(layer.idx, t).map(|(chain, b)| TestReq {
                                id: (layer.seq % u64::from(b.identities.max(1))) as u32,
                                chain,
                                retries_left: b.retries,
                                retry: false,
                            });
                            handle.spawn(send_one(
                                run.clone(),
                                Job {
                                    layer: layer.idx,
                                    target,
                                    template: tpl,
                                    seq,
                                    vu,
                                    intended,
                                    start,
                                    test,
                                },
                            ));
                        }
                        layer.seq += 1;
                    }
                    if !layer.finished {
                        earliest = earliest.min(wake.unwrap_or(layer.next_at));
                    }
                }
                Model::Closed => {
                    if layer.next_adjust <= now {
                        match view.level(layer.idx, now.as_secs_f64()) {
                            Level::Ended => {
                                for u in layer.users.drain(..) {
                                    u.stop.store(true, Ordering::Relaxed);
                                }
                                layer.finished = true;
                                continue;
                            }
                            Level::Waiting => {}
                            Level::Run(users) => {
                                let desired = part
                                    .users(users)
                                    .min(vu_count as usize)
                                    .min(run.inflight_cap);
                                while layer.users.len() > desired {
                                    if let Some(u) = layer.users.pop() {
                                        u.stop.store(true, Ordering::Relaxed);
                                    }
                                }
                                // Users end when the scheduler stops them (the
                                // pattern's end moves with live changes).
                                let end = start + max_duration;
                                let think = live.def.think_time.map_or(Duration::ZERO, |d| d.0);
                                while layer.users.len() < desired {
                                    let stop = Arc::new(AtomicBool::new(false));
                                    let vu = vu_base + layer.vu_cursor % vu_count;
                                    layer.vu_cursor = layer.vu_cursor.wrapping_add(1);
                                    handle.spawn(user_loop(
                                        run.clone(),
                                        layer.idx,
                                        vu,
                                        stop.clone(),
                                        think,
                                        end,
                                        start,
                                    ));
                                    layer.users.push(User { stop });
                                }
                            }
                        }
                        layer.next_adjust = now + USERS_TICK;
                    }
                    earliest = earliest.min(layer.next_adjust);
                }
            }
        }
        if all_finished && layers.len() >= view.layers.len() {
            break;
        }
        let now = start.elapsed();
        if earliest > now {
            std::thread::sleep((earliest - now).min(MAX_SLEEP));
        }
    }
    run.end_stages();
    // Stop the users of closed layers and drain what is in flight.
    for layer in &mut layers {
        for u in layer.users.drain(..) {
            u.stop.store(true, Ordering::Relaxed);
        }
    }
    run.set_state(State::Draining);
    let drain_until = Instant::now() + run.scenario.timeout() + DRAIN_GRACE;
    while run.inflight.load(Ordering::Relaxed) > 0 && Instant::now() < drain_until {
        if start.elapsed() >= bucket.end {
            roller.roll(&run, &mut bucket, &mut lag, false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // The last, partial bucket.
    bucket.end = start.elapsed().max(bucket.start + Duration::from_millis(1));
    roller.roll(&run, &mut bucket, &mut lag, false);
    roller.finish();
    run.run_loops(true);
    run.finish(start, cpu_before, None);
}

/// One user of a closed layer: send, wait for the answer, think, repeat.
async fn user_loop(
    run: Arc<Run>,
    layer_idx: usize,
    vu: u32,
    stop: Arc<AtomicBool>,
    think: Duration,
    end: Instant,
    start: Instant,
) {
    let mut local_seq = 0u64;
    while !stop.load(Ordering::Relaxed) && !run.stop_requested() && Instant::now() < end {
        if run.inflight.load(Ordering::Relaxed) >= run.inflight_cap {
            run.not_sent.fetch_add(1, Ordering::Relaxed);
            run.requested.fetch_add(1, Ordering::Relaxed);
            run.count_layer(layer_idx, false);
            {
                let mut totals = run.totals.lock().unwrap_or_else(|p| p.into_inner());
                totals.counts.requested += 1;
                totals.counts.not_sent += 1;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        }
        let seq = run.seq.fetch_add(1, Ordering::Relaxed);
        run.requested.fetch_add(1, Ordering::Relaxed);
        run.sent.fetch_add(1, Ordering::Relaxed);
        run.inflight.fetch_add(1, Ordering::Relaxed);
        run.count_layer(layer_idx, true);
        {
            // Closed-model sends count in the current bucket through the
            // totals (the scheduler owns the bucket counters for open layers).
            let mut totals = run.totals.lock().unwrap_or_else(|p| p.into_inner());
            totals.counts.requested += 1;
            totals.counts.sent += 1;
        }
        let tpl = run.pick_template(layer_idx, local_seq);
        let target = run.pick_target(seq);
        let now = Instant::now();
        send_one(
            run.clone(),
            Job {
                layer: layer_idx,
                target,
                template: tpl,
                seq,
                vu,
                intended: now,
                start,
                test: None,
            },
        )
        .await;
        local_seq += 1;
        if !think.is_zero() {
            tokio::time::sleep(think).await;
        }
    }
}

/// Start a run on its own scheduler thread.
pub fn start(run: Arc<Run>, handle: Handle) -> Result<(), String> {
    let name = format!("harness-{}", run.id);
    std::thread::Builder::new()
        .name(name)
        .spawn(move || drive(run, handle))
        .map(|_| ())
        .map_err(|e| format!("cannot start the scheduler thread: {e}"))
}

/// Extra fields of a live snapshot the API adds (links), kept here so the
/// JSON shape has one owner.
pub fn with_links(mut snapshot: Value, prefix: &str, id: &str) -> Value {
    if let Value::Object(m) = &mut snapshot {
        m.insert(
            "links".into(),
            json!({
                "self": format!("{prefix}/runs/{id}"),
                "report": format!("{prefix}/runs/{id}/report"),
                "stop": format!("{prefix}/runs/{id}/stop"),
            }),
        );
    }
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflight_cap_follows_the_descriptor_limit() {
        assert_eq!(inflight_cap(10_000, None, false), 10_000);
        assert_eq!(inflight_cap(10_000, Some(1024), false), 768);
        assert_eq!(inflight_cap(10_000, Some(1024), true), 384);
        assert_eq!(inflight_cap(10_000, Some(65_536), true), 10_000);
        assert_eq!(inflight_cap(10_000, Some(100), true), 1);
    }

    #[test]
    fn weighted_picks() {
        let w = cumulative([3u32, 1].into_iter());
        assert_eq!(w, vec![3, 4]);
        let picks: Vec<usize> = (0..8).map(|s| pick(&w, s)).collect();
        assert_eq!(picks, vec![0, 0, 0, 1, 0, 0, 0, 1]);
        assert_eq!(pick(&[], 5), 0);
    }

    #[test]
    fn shares_split_users_and_virtual_clients_exactly() {
        // Three nodes with capacities 1, 1, 1 and 1, 2, 5.
        for cuts in [
            vec![0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0],
            vec![0.0, 0.125, 0.375, 1.0],
        ] {
            let shares: Vec<Share> = cuts
                .windows(2)
                .map(|w| Share {
                    from: w[0],
                    to: w[1],
                })
                .collect();
            for total in [0.0, 1.0, 2.0, 7.0, 10.0, 99.6, 1000.0] {
                let sum: usize = shares.iter().map(|s| s.users(total)).sum();
                assert_eq!(sum, total.round() as usize, "{cuts:?} {total}");
            }
            let mut next = 0;
            for s in &shares {
                let (base, count) = s.virtual_clients(100);
                assert_eq!(base, next, "disjoint, in order");
                next = base + count;
            }
            assert_eq!(next, 100);
            let fractions: f64 = shares.iter().map(Share::fraction).sum();
            assert!((fractions - 1.0).abs() < 1e-12);
        }
        // Fewer virtual clients than nodes: everyone still gets one.
        let s = Share {
            from: 0.5,
            to: 0.75,
        };
        assert_eq!(s.virtual_clients(1), (0, 1));
        assert!(Share::default().is_whole() && Share::default().valid());
        assert!(!Share { from: 0.6, to: 0.5 }.valid());
        assert_eq!(Share::default().users(4.4), 4);
    }

    #[test]
    fn descriptor_limit_is_readable() {
        if cfg!(unix) {
            assert!(raise_descriptor_limit().is_some_and(|n| n > 0));
        }
    }
}
