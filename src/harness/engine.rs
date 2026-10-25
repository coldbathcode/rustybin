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

use std::cell::Cell;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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

use super::report::{
    self, Breakdown, Bucket, Counts, Percentiles, Report, Sample, Saturation, TargetReport,
    ThresholdResult, LAG_SATURATION_MS, MAX_SAMPLES,
};
use super::safety::{KillSwitch, ParsedTarget};
use super::scenario::{Caps, HttpVersion, Layer, Model, RequestTemplate, Scenario, Target};
use crate::config::HarnessConfig;

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

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX).max(1)
}

/// Microsecond histogram, 1 us to 60 s.
fn histogram(sigfig: u8) -> Histogram<u64> {
    Histogram::new_with_bounds(1, 60_000_000, sigfig).unwrap_or_else(|_| {
        // The bounds above are valid; this is never reached.
        Histogram::new(2).unwrap_or_else(|_| unreachable!("histogram with 2 significant digits"))
    })
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
}

/// Per-layer or per-target accumulator in a shard.
struct PartShard {
    counts: Counts,
    corrected: Histogram<u64>,
}

impl PartShard {
    fn new() -> Self {
        Self {
            counts: Counts::default(),
            corrected: histogram(2),
        }
    }
}

/// One worker thread's accumulator since the last roll.
struct Shard {
    counts: Counts,
    corrected: Histogram<u64>,
    service: Histogram<u64>,
    layers: Vec<PartShard>,
    targets: Vec<PartShard>,
    samples: Vec<Sample>,
}

impl Shard {
    fn new(layers: usize, targets: usize) -> Self {
        Self {
            counts: Counts::default(),
            corrected: histogram(3),
            service: histogram(3),
            layers: (0..layers).map(|_| PartShard::new()).collect(),
            targets: (0..targets).map(|_| PartShard::new()).collect(),
            samples: Vec::new(),
        }
    }

    fn record(&mut self, o: &Outcome, layer_name: &str, target_url: &str) {
        let corrected = micros(o.corrected);
        let service = micros(o.service);
        self.counts.completed += 1;
        self.counts.bytes_received += o.bytes;
        match (o.status, o.transport) {
            (Some(status), _) => {
                *self.counts.statuses.entry(status.to_string()).or_default() += 1;
                if o.expected {
                    self.counts.ok += 1;
                } else {
                    self.counts.unexpected_status += 1;
                }
            }
            (None, kind) => {
                self.counts.transport += 1;
                *self
                    .counts
                    .transport_kinds
                    .entry(kind.unwrap_or("other").to_string())
                    .or_default() += 1;
            }
        }
        self.corrected.saturating_record(corrected);
        self.service.saturating_record(service);
        for (part, idx) in [(&mut self.layers, o.layer), (&mut self.targets, o.target)] {
            if let Some(p) = part.get_mut(idx) {
                p.counts.completed += 1;
                p.counts.bytes_received += o.bytes;
                p.corrected.saturating_record(corrected);
                match (o.status, o.transport) {
                    (Some(status), _) => {
                        *p.counts.statuses.entry(status.to_string()).or_default() += 1;
                        if o.expected {
                            p.counts.ok += 1;
                        } else {
                            p.counts.unexpected_status += 1;
                        }
                    }
                    (None, kind) => {
                        p.counts.transport += 1;
                        *p.counts
                            .transport_kinds
                            .entry(kind.unwrap_or("other").to_string())
                            .or_default() += 1;
                    }
                }
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
    layers: Vec<PartShard>,
    targets: Vec<PartShard>,
    lag: Histogram<u64>,
    samples: Vec<Sample>,
    timeline: Vec<Bucket>,
    saturated_buckets: Vec<f64>,
    saturation_reasons: Vec<String>,
    saturated: bool,
    consecutive_saturated: u32,
}

impl Totals {
    fn new(layers: usize, targets: usize) -> Self {
        Self {
            counts: Counts::default(),
            corrected: histogram(3),
            service: histogram(3),
            layers: (0..layers).map(|_| PartShard::new()).collect(),
            targets: (0..targets).map(|_| PartShard::new()).collect(),
            lag: histogram(3),
            samples: Vec::new(),
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
    layer_weights: Vec<Vec<u32>>,
    /// Per layer: requested, sent and not sent (scheduler and user side).
    layer_sched: Vec<[AtomicU64; 3]>,
}

impl Run {
    pub fn new(params: RunParams) -> Arc<Self> {
        let shard_count = params.threads + 2;
        let layers = params.scenario.layers.len();
        let targets = params.targets.len();
        let (done, _) = watch::channel(false);
        let target_weights = cumulative(params.targets.iter().map(|t| t.target.weight));
        let layer_weights = params
            .scenario
            .layers
            .iter()
            .map(|l| cumulative(l.source.templates().iter().map(|t| t.weight)))
            .collect();
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
            layer_weights,
            layer_sched: (0..layers)
                .map(|_| [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)])
                .collect(),
        })
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
        false
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
        let layers: Vec<Value> = self
            .scenario
            .layers
            .iter()
            .zip(&totals.layers)
            .map(|(l, p)| {
                json!({
                    "name": l.name,
                    "model": l.model.as_str(),
                    "completed": p.counts.completed,
                    "ok": p.counts.ok,
                    "errors": p.counts.errors(),
                    "p99_ms": Percentiles::from_histogram(&p.corrected).p99_ms,
                    "target_now": l.pattern.value_at(
                        self.elapsed().saturating_sub(l.starts_after.map_or(Duration::ZERO, |d| d.0))
                    ),
                })
            })
            .collect();
        json!({
            "run_id": self.id,
            "scenario": self.scenario.name,
            "hash": self.hash,
            "node_id": self.node_id,
            "state": self.state().as_str(),
            "created_at": self.created_at.to_rfc3339(),
            "elapsed_secs": elapsed,
            "planned_secs": planned,
            "progress": if planned > 0.0 { (elapsed / planned).min(1.0) } else { 1.0 },
            "requested": self.requested.load(Ordering::Relaxed),
            "sent": self.sent.load(Ordering::Relaxed),
            "not_sent": self.not_sent.load(Ordering::Relaxed),
            "in_flight": self.inflight.load(Ordering::Relaxed),
            "completed": totals.counts.completed,
            "ok": totals.counts.ok,
            "errors": totals.counts.errors(),
            "statuses": totals.counts.statuses,
            "transport_kinds": totals.counts.transport_kinds,
            "latency": latency,
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
            "state": self.state().as_str(),
            "created_at": self.created_at.to_rfc3339(),
            "elapsed_secs": self.elapsed().as_secs_f64(),
            "planned_secs": self.scenario.duration().as_secs_f64(),
            "sent": self.sent.load(Ordering::Relaxed),
            "passed": self.report.get().map(|r| r.passed),
            "exit_code": self.report.get().map(|r| r.exit_code),
        })
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
        self.layer_weights.get(layer).map_or(0, |w| pick(w, seq))
    }

    fn expand(&self, text: &str, layer: &str, seq: u64, vu: u32) -> String {
        if !text.contains("{{") {
            return text.to_string();
        }
        let mut out = text.to_string();
        let replacements: [(&str, String); 6] = [
            ("seq", seq.to_string()),
            ("vu", vu.to_string()),
            ("run", self.id.clone()),
            ("node", self.node_id.clone()),
            ("layer", layer.to_string()),
            ("rand", rand::random::<u32>().to_string()),
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
    ) -> Result<(Request<Full<Bytes>>, String), String> {
        let layer = &self.scenario.layers[layer_idx];
        let path = self.expand(&tpl.path, &layer.name, seq, vu);
        let uri: Uri = format!("{}{}{}", target.authority, target.parsed.path, path)
            .parse()
            .map_err(|e| format!("request URI: {e}"))?;
        let request_id = format!("{}-{}-{seq}", self.id, self.node_id);
        let mut builder = Request::builder()
            .method(tpl.method.to_ascii_uppercase().as_str())
            .uri(uri)
            .header("x-request-id", &request_id)
            .header(
                "user-agent",
                concat!("rustybin-harness/", env!("CARGO_PKG_VERSION")),
            );
        let mut has_content_type = false;
        for (name, value) in &tpl.headers {
            if name.eq_ignore_ascii_case("content-type") {
                has_content_type = true;
            }
            builder = builder.header(name.as_str(), self.expand(value, &layer.name, seq, vu));
        }
        let body = match &tpl.body {
            None => Bytes::new(),
            Some(Value::String(s)) => Bytes::from(self.expand(s, &layer.name, seq, vu)),
            Some(other) => {
                if !has_content_type {
                    builder = builder.header("content-type", "application/json");
                }
                Bytes::from(self.expand(&other.to_string(), &layer.name, seq, vu))
            }
        };
        let req = builder
            .body(Full::new(body))
            .map_err(|e| format!("request: {e}"))?;
        Ok((req, request_id))
    }

    fn mark_started(&self) {
        let _ = self.started.set(Instant::now());
        self.set_state(State::Running);
    }

    /// Roll every shard into the totals and one timeline bucket.
    fn roll(&self, bucket: &mut SchedBucket, start: Instant, lag: &mut Histogram<u64>) {
        let mut merged = histogram(3);
        let mut merged_service = histogram(3);
        let mut counts = Counts::default();
        let mut samples = Vec::new();
        let mut totals = self.totals.lock().unwrap_or_else(|p| p.into_inner());
        for shard in &self.shards {
            let mut s = shard.lock().unwrap_or_else(|p| p.into_inner());
            let _ = merged.add(&s.corrected);
            let _ = merged_service.add(&s.service);
            let _ = totals.corrected.add(&s.corrected);
            let _ = totals.service.add(&s.service);
            s.corrected.reset();
            s.service.reset();
            let c = std::mem::take(&mut s.counts);
            counts.add(&c);
            totals.counts.add(&c);
            for (dst, src) in totals.layers.iter_mut().zip(s.layers.iter_mut()) {
                let _ = dst.corrected.add(&src.corrected);
                src.corrected.reset();
                dst.counts.add(&std::mem::take(&mut src.counts));
            }
            for (dst, src) in totals.targets.iter_mut().zip(s.targets.iter_mut()) {
                let _ = dst.corrected.add(&src.corrected);
                src.corrected.reset();
                dst.counts.add(&std::mem::take(&mut src.counts));
            }
            samples.append(&mut s.samples);
        }
        totals.keep_samples(samples);
        let _ = totals.lag.add(&*lag);
        let lag_p = Percentiles::from_histogram(lag);
        lag.reset();
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
            rate: counts.completed as f64 / width,
            p50_ms: p.p50_ms,
            p99_ms: p.p99_ms,
            max_ms: p.max_ms,
            service_p99_ms: Percentiles::from_histogram(&merged_service).p99_ms,
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
        drop(totals);
        let elapsed = start.elapsed();
        *bucket = SchedBucket {
            start: bucket.end,
            end: bucket.end + Duration::from_secs(1),
            ..SchedBucket::default()
        };
        let _ = elapsed;
    }

    /// Build the final report, store it and finish the run.
    fn finish(&self, start: Instant, cpu_before: Option<f64>, error: Option<String>) {
        self.set_state(State::Finished);
        let elapsed = start.elapsed();
        let totals = self.totals.lock().unwrap_or_else(|p| p.into_inner());
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
        let thresholds: Vec<ThresholdResult> = report::evaluate(
            &self.scenario.thresholds,
            &totals.counts,
            &latency,
            &saturation,
        );
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
        let mut layers = breakdown(
            self.scenario
                .layers
                .iter()
                .map(|l| l.name.clone())
                .collect(),
            &totals.layers,
        );
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
            achieved_rate,
            requested_rate,
            layers: breakdown(
                self.scenario
                    .layers
                    .iter()
                    .map(|l| l.name.clone())
                    .collect(),
                &totals.layers,
            ),
            targets_breakdown: breakdown(
                self.targets.iter().map(|t| t.target.url.clone()).collect(),
                &totals.targets,
            ),
            timeline: totals.timeline.clone(),
            saturation,
            passed: exit_code == 0,
            exit_code,
            thresholds,
            samples,
            scenario: report::redact_scenario(
                &serde_json::to_value(&self.scenario).unwrap_or(Value::Null),
            ),
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
            p99_ms = report.latency.p99_ms,
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

/// Scheduler-side counters of the current bucket.
#[derive(Default)]
struct SchedBucket {
    start: Duration,
    end: Duration,
    requested: u64,
    sent: u64,
    not_sent: u64,
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
    } = job;
    let layer = &run.scenario.layers[layer_idx];
    let templates = layer.source.templates();
    let Some(tpl) = templates.get(tpl_idx).copied() else {
        run.inflight.fetch_sub(1, Ordering::Relaxed);
        return;
    };
    let target = &run.targets[target_idx];
    let (req, request_id) = match run.build_request(layer_idx, tpl, target, seq, vu) {
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
    let sent = Instant::now();
    let result = tokio::time::timeout(timeout, async {
        let resp = target
            .client
            .request(req)
            .await
            .map_err(|e| classify_error(&e))?;
        let status = resp.status().as_u16();
        let bytes = read_body(resp.into_body()).await?;
        Ok::<(u16, u64), &'static str>((status, bytes))
    })
    .await;
    let done = Instant::now();
    let (status, transport, bytes, expected) = match result {
        Ok(Ok((status, bytes))) => (Some(status), None, bytes, tpl.expects(status)),
        Ok(Err(kind)) => (None, Some(kind), 0, false),
        Err(_) => (None, Some("timeout"), 0, false),
    };
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
        service: done.saturating_duration_since(sent),
        at: sent.saturating_duration_since(start),
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
    starts_after: Duration,
    finished: bool,
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
    fn new(idx: usize, layer: &Layer) -> Self {
        let starts_after = layer.starts_after.map_or(Duration::ZERO, |d| d.0);
        Self {
            idx,
            starts_after,
            finished: false,
            seq: 0,
            model: layer.model,
            next_at: starts_after,
            users: Vec::new(),
            next_adjust: starts_after,
            vu_cursor: 0,
        }
    }
}

/// The scheduler loop of one run: runs on its own OS thread until the
/// patterns end or the run is stopped, then drains and finishes.
pub fn drive(run: Arc<Run>, handle: Handle) {
    let _enter = handle.enter();
    let cpu_before = cpu_seconds();
    run.mark_started();
    let start = Instant::now();
    let virtual_clients = run.scenario.scale.virtual_clients.max(1);
    let mut layers: Vec<LayerSched> = run
        .scenario
        .layers
        .iter()
        .enumerate()
        .map(|(i, l)| LayerSched::new(i, l))
        .collect();
    let mut bucket = SchedBucket {
        start: Duration::ZERO,
        end: Duration::from_secs(1),
        ..SchedBucket::default()
    };
    let mut lag = histogram(3);
    let mut vu_cursor: u32 = 0;
    loop {
        if run.stop_requested() {
            break;
        }
        let now = start.elapsed();
        if now >= bucket.end {
            run.roll(&mut bucket, start, &mut lag);
            continue;
        }
        let mut earliest = bucket.end;
        let mut all_finished = true;
        for layer in layers.iter_mut().filter(|l| !l.finished) {
            all_finished = false;
            let def = &run.scenario.layers[layer.idx];
            match layer.model {
                Model::Open => {
                    while layer.next_at <= now && !layer.finished {
                        let intended = start + layer.next_at;
                        let Some(rate) = def
                            .pattern
                            .value_at(layer.next_at.saturating_sub(layer.starts_after))
                        else {
                            layer.finished = true;
                            break;
                        };
                        if rate <= 0.0 {
                            // Skip ahead to where the pattern sends again.
                            layer.next_at += Duration::from_millis(10);
                            continue;
                        }
                        lag.saturating_record(micros(now.saturating_sub(layer.next_at)));
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
                            let vu = vu_cursor % virtual_clients;
                            vu_cursor = vu_cursor.wrapping_add(1);
                            let tpl = run.pick_template(layer.idx, layer.seq);
                            let target = run.pick_target(seq);
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
                                },
                            ));
                        }
                        layer.seq += 1;
                        layer.next_at += Duration::from_secs_f64(1.0 / rate);
                    }
                    if !layer.finished {
                        earliest = earliest.min(layer.next_at);
                    }
                }
                Model::Closed => {
                    if layer.next_adjust <= now {
                        let t = now.saturating_sub(layer.starts_after);
                        match def.pattern.value_at(t) {
                            None => {
                                for u in layer.users.drain(..) {
                                    u.stop.store(true, Ordering::Relaxed);
                                }
                                layer.finished = true;
                                continue;
                            }
                            Some(users) => {
                                let desired = (users.round().max(0.0) as usize)
                                    .min(virtual_clients as usize)
                                    .min(run.inflight_cap);
                                while layer.users.len() > desired {
                                    if let Some(u) = layer.users.pop() {
                                        u.stop.store(true, Ordering::Relaxed);
                                    }
                                }
                                let end = start + layer.starts_after + def.pattern.duration();
                                let think = def.think_time.map_or(Duration::ZERO, |d| d.0);
                                while layer.users.len() < desired {
                                    let stop = Arc::new(AtomicBool::new(false));
                                    let vu = layer.vu_cursor % virtual_clients;
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
        if all_finished {
            break;
        }
        let now = start.elapsed();
        if earliest > now {
            std::thread::sleep((earliest - now).min(MAX_SLEEP));
        }
    }
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
            run.roll(&mut bucket, start, &mut lag);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // The last, partial bucket.
    bucket.end = start.elapsed().max(bucket.start + Duration::from_millis(1));
    run.roll(&mut bucket, start, &mut lag);
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
    fn descriptor_limit_is_readable() {
        if cfg!(unix) {
            assert!(raise_descriptor_limit().is_some_and(|n| n > 0));
        }
    }
}
