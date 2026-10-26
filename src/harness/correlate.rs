//! Correlation (`docs/plans/harness.md`, section 7): the client marks every
//! request, the server side records and tags every answer, and the client
//! then knows what the gateway did and not only what it saw.
//!
//! Every harness request carries:
//!
//! - `X-Request-Id: <run>-<node>-<seq>` and a W3C `traceparent`, so gateway
//!   access logs join with the report;
//! - `X-Rustybin-Harness: v1; run=..; node=..; seq=..; vu=..; sent=<unix ns>;
//!   h=<header names>; hd=<headers digest>; b=<body digest>; p=<method and
//!   path digest>; sig=<hmac>`, the marker. The signature is an HMAC-SHA256
//!   with the run key, which only the run's nodes hold, so stray or spoofed
//!   markers on a shared instance never pollute a run.
//!
//! The server layer ([`layer`], installed by `build_app` next to inspector
//! capture) verifies the marker, compares the digests with what arrived
//! (header adds, removes and rewrites, body integrity, path rewrites), records
//! per-run arrivals by node and route and the processing time, samples
//! captures into the inspector (1 in N plus every anomaly), and answers with
//! `Server-Timing: rb;dur=<ms>` and `X-Rustybin-Served-By: <node>; run=..;
//! seq=..; t=<arrived ns>,<done ns>; sig=<hmac>`, the tag.
//!
//! The client classifies every answer: `upstream` (a valid tag for this
//! run), `gateway` (no valid tag: a rate limit, an auth failure, a gateway
//! error, a cached answer), `transport` (connect, TLS, timeout, reset,
//! protocol). After the run, a transport failure the server never saw is
//! `lost`: the gateway swallowed it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{MatchedPath, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method};
use axum::middleware::Next;
use axum::response::Response;
use hdrhistogram::Histogram;
use hmac::{Hmac, Mac};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::report::Percentiles;
use super::scenario::ServerProfile;
use crate::state::AppState;

type HmacSha256 = Hmac<Sha256>;

/// The marker header (request).
pub const HEADER: &str = "x-rustybin-harness";
/// The tag header (response).
pub const SERVED_BY: &str = "x-rustybin-served-by";
pub const VERSION: &str = "v1";
/// Longest marker or tag accepted.
pub const MAX_HEADER_LEN: usize = 4096;
/// Most header names a marker may list.
pub const MAX_NAMES: usize = 48;
/// Runs the server side keeps statistics for (the active run plus the last ones).
pub const KEPT_RUNS: usize = 8;
/// Seen-sequence bitmap cap per run (bits); sequences above it are counted,
/// not tracked, and `lost` becomes a lower bound.
pub const SEEN_CAP_BITS: u64 = 1 << 26;
/// Most distinct nodes and header names tracked per run.
pub const MAX_LABELS: usize = 64;
/// Most distinct routes tracked per run.
pub const MAX_ROUTES: usize = 256;
/// Headers the client never lists and the server never reports as added.
pub const UNLISTED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "x-rustybin-harness",
    "x-request-id",
    "traceparent",
    "te",
    "upgrade",
];

fn unix_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Now as nanoseconds since the Unix epoch.
pub fn now_ns() -> u64 {
    unix_ns()
}

/// The key of one run: signs markers and tags. Only the run's nodes hold it.
#[derive(Clone)]
pub struct RunKey([u8; 32]);

impl RunKey {
    pub fn random() -> Self {
        let mut bytes = [0u8; 32];
        for b in &mut bytes {
            *b = rand::random();
        }
        Self(bytes)
    }

    /// 128-bit HMAC-SHA256 tag of `message`, as hex.
    pub fn sign(&self, message: &str) -> String {
        let Ok(mut mac) = HmacSha256::new_from_slice(&self.0) else {
            return String::new();
        };
        mac.update(message.as_bytes());
        let out = mac.finalize().into_bytes();
        out.iter().take(16).map(|b| format!("{b:02x}")).collect()
    }

    pub fn verify(&self, message: &str, sig: &str) -> bool {
        crate::control_auth::constant_time_eq(&self.sign(message), sig)
    }
}

impl std::fmt::Debug for RunKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RunKey([redacted])")
    }
}

/// 32-bit digest (8 hex) of the given parts.
pub fn digest<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
        h.update([0u8]);
    }
    let out = h.finalize();
    out.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// Digest of the listed headers as they are now: `name:value` pairs in the
/// listed order, every value of a repeated header. Returns the digest and the
/// listed names that are absent.
pub fn digest_headers(names: &[String], headers: &HeaderMap) -> (String, Vec<String>) {
    let mut parts: Vec<Vec<u8>> = Vec::with_capacity(names.len());
    let mut missing = Vec::new();
    for name in names {
        let values: Vec<&HeaderValue> = headers.get_all(name.as_str()).iter().collect();
        if values.is_empty() {
            missing.push(name.clone());
            continue;
        }
        for v in values {
            let mut p = Vec::with_capacity(name.len() + 1 + v.len());
            p.extend_from_slice(name.as_bytes());
            p.push(b':');
            p.extend_from_slice(v.as_bytes());
            parts.push(p);
        }
    }
    (digest(parts.iter().map(Vec::as_slice)), missing)
}

/// Digest of the method and the path with its query.
pub fn digest_path(method: &Method, path_and_query: &str) -> String {
    digest([method.as_str().as_bytes(), path_and_query.as_bytes()])
}

/// The marker a client puts on every request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    pub run: String,
    pub node: String,
    pub seq: u64,
    pub vu: u32,
    /// Unix nanoseconds at the client when the request was built.
    pub sent_ns: u64,
    /// The header names the client set (lower case), covered by `hd`.
    pub names: Vec<String>,
    pub headers_digest: String,
    /// Empty when the request has no body.
    pub body_digest: String,
    pub path_digest: String,
    pub sig: String,
}

fn safe_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl Marker {
    fn message(&self) -> String {
        format!(
            "{VERSION}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.run,
            self.node,
            self.seq,
            self.vu,
            self.sent_ns,
            self.names.join(","),
            self.headers_digest,
            self.body_digest,
            self.path_digest
        )
    }

    /// Sign with the run key (sets `sig`).
    pub fn sign(&mut self, key: &RunKey) {
        self.sig = key.sign(&self.message());
    }

    pub fn verify(&self, key: &RunKey) -> bool {
        !self.sig.is_empty() && key.verify(&self.message(), &self.sig)
    }

    /// The header value.
    pub fn to_header(&self) -> String {
        format!(
            "{VERSION}; run={}; node={}; seq={}; vu={}; sent={}; h={}; hd={}; b={}; p={}; sig={}",
            self.run,
            self.node,
            self.seq,
            self.vu,
            self.sent_ns,
            self.names.join(","),
            self.headers_digest,
            self.body_digest,
            self.path_digest,
            self.sig
        )
    }

    /// Parse a header value (shape only; [`Marker::verify`] checks the signature).
    pub fn parse(raw: &str) -> Option<Self> {
        if raw.len() > MAX_HEADER_LEN {
            return None;
        }
        let mut fields = raw.split(';').map(str::trim);
        if fields.next()? != VERSION {
            return None;
        }
        let mut m = Marker {
            run: String::new(),
            node: String::new(),
            seq: 0,
            vu: 0,
            sent_ns: 0,
            names: Vec::new(),
            headers_digest: String::new(),
            body_digest: String::new(),
            path_digest: String::new(),
            sig: String::new(),
        };
        for f in fields {
            let (k, v) = f.split_once('=')?;
            match k {
                "run" => m.run = v.to_string(),
                "node" => m.node = v.to_string(),
                "seq" => m.seq = v.parse().ok()?,
                "vu" => m.vu = v.parse().ok()?,
                "sent" => m.sent_ns = v.parse().ok()?,
                "h" => {
                    m.names = v
                        .split(',')
                        .filter(|n| !n.is_empty())
                        .map(|n| n.to_ascii_lowercase())
                        .collect();
                    if m.names.len() > MAX_NAMES
                        || m.names.iter().any(|n| !safe_token(n) || n.len() > 64)
                    {
                        return None;
                    }
                }
                "hd" => m.headers_digest = v.to_string(),
                "b" => m.body_digest = v.to_string(),
                "p" => m.path_digest = v.to_string(),
                "sig" => m.sig = v.to_string(),
                _ => return None,
            }
        }
        let hex = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit());
        if !safe_token(&m.run)
            || !safe_token(&m.node)
            || !hex(&m.headers_digest, 8)
            || !(m.body_digest.is_empty() || hex(&m.body_digest, 8))
            || !hex(&m.path_digest, 8)
            || !hex(&m.sig, 32)
        {
            return None;
        }
        Some(m)
    }
}

/// W3C trace context for a request: the trace id from the run and the
/// sequence number (deterministic, so a gateway's traces join with the
/// report), the span id from the sequence number.
pub fn traceparent(run: &str, node: &str, seq: u64) -> String {
    let mut h = Sha256::new();
    h.update(run.as_bytes());
    h.update(b"|");
    h.update(node.as_bytes());
    let out = h.finalize();
    let prefix: String = out.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("00-{prefix}{seq:016x}-{seq:016x}-01")
}

/// The tag a server puts on every answer to a marked request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    pub node: String,
    pub run: String,
    pub seq: u64,
    /// Unix nanoseconds at the server: arrival and completion.
    pub arrived_ns: u64,
    pub done_ns: u64,
    pub sig: String,
}

impl Tag {
    fn message(&self) -> String {
        format!(
            "tag|{}|{}|{}|{}|{}",
            self.node, self.run, self.seq, self.arrived_ns, self.done_ns
        )
    }

    pub fn sign(&mut self, key: &RunKey) {
        self.sig = key.sign(&self.message());
    }

    pub fn verify(&self, key: &RunKey) -> bool {
        !self.sig.is_empty() && key.verify(&self.message(), &self.sig)
    }

    pub fn to_header(&self) -> String {
        format!(
            "{}; run={}; seq={}; t={},{}; sig={}",
            self.node, self.run, self.seq, self.arrived_ns, self.done_ns, self.sig
        )
    }

    pub fn parse(raw: &str) -> Option<Self> {
        if raw.len() > MAX_HEADER_LEN {
            return None;
        }
        let mut fields = raw.split(';').map(str::trim);
        let node = fields.next()?.to_string();
        let mut t = Tag {
            node,
            run: String::new(),
            seq: 0,
            arrived_ns: 0,
            done_ns: 0,
            sig: String::new(),
        };
        for f in fields {
            let (k, v) = f.split_once('=')?;
            match k {
                "run" => t.run = v.to_string(),
                "seq" => t.seq = v.parse().ok()?,
                "t" => {
                    let (a, d) = v.split_once(',')?;
                    t.arrived_ns = a.parse().ok()?;
                    t.done_ns = d.parse().ok()?;
                }
                "sig" => t.sig = v.to_string(),
                _ => return None,
            }
        }
        if !safe_token(&t.node) || !safe_token(&t.run) || t.sig.len() != 32 {
            return None;
        }
        Some(t)
    }
}

/// The `rb` entry of a `Server-Timing` header, in milliseconds.
pub fn parse_server_timing(raw: &str) -> Option<f64> {
    raw.split(',').map(str::trim).find_map(|entry| {
        let mut parts = entry.split(';').map(str::trim);
        if parts.next()? != "rb" {
            return None;
        }
        parts
            .find_map(|p| p.strip_prefix("dur="))
            .and_then(|d| d.parse().ok())
    })
}

/// Which digests did not match what arrived.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub headers: bool,
    pub body: bool,
    pub path: bool,
    pub missing: Vec<String>,
    pub added: Vec<String>,
}

impl Changes {
    pub fn any(&self) -> bool {
        self.headers || self.body || self.path || !self.missing.is_empty()
    }
}

/// What the server side saw of one run.
pub struct ServerStats {
    pub arrivals: u64,
    pub by_node: HashMap<String, u64>,
    pub by_route: HashMap<String, u64>,
    pub statuses: HashMap<u16, u64>,
    processing: Histogram<u64>,
    pub headers_changed: u64,
    pub body_changed: u64,
    pub path_changed: u64,
    pub headers_missing: HashMap<String, u64>,
    pub headers_added: HashMap<String, u64>,
    pub captured: u64,
    seen: Vec<u64>,
    pub seen_overflow: u64,
}

impl ServerStats {
    fn new() -> Self {
        Self {
            arrivals: 0,
            by_node: HashMap::new(),
            by_route: HashMap::new(),
            statuses: HashMap::new(),
            processing: Histogram::new_with_bounds(1, 60_000_000, 2)
                .unwrap_or_else(|_| Histogram::new(2).unwrap_or_else(|_| unreachable!())),
            headers_changed: 0,
            body_changed: 0,
            path_changed: 0,
            headers_missing: HashMap::new(),
            headers_added: HashMap::new(),
            captured: 0,
            seen: Vec::new(),
            seen_overflow: 0,
        }
    }

    fn mark_seen(&mut self, seq: u64) {
        if seq >= SEEN_CAP_BITS {
            self.seen_overflow += 1;
            return;
        }
        let word = (seq / 64) as usize;
        if self.seen.len() <= word {
            self.seen.resize(word + 1, 0);
        }
        self.seen[word] |= 1 << (seq % 64);
    }

    /// Did a request with this sequence number arrive? `None` above the cap.
    pub fn was_seen(&self, seq: u64) -> Option<bool> {
        if seq >= SEEN_CAP_BITS {
            return None;
        }
        Some(
            self.seen
                .get((seq / 64) as usize)
                .is_some_and(|w| w & (1 << (seq % 64)) != 0),
        )
    }

    fn record(
        &mut self,
        marker: &Marker,
        route: &str,
        status: u16,
        processing: Duration,
        changes: &Changes,
        captured: bool,
    ) {
        self.arrivals += 1;
        bump(&mut self.by_node, &marker.node, MAX_LABELS);
        bump(&mut self.by_route, route, MAX_ROUTES);
        *self.statuses.entry(status).or_default() += 1;
        self.processing.saturating_record(
            u64::try_from(processing.as_micros())
                .unwrap_or(u64::MAX)
                .max(1),
        );
        if changes.headers {
            self.headers_changed += 1;
        }
        if changes.body {
            self.body_changed += 1;
        }
        if changes.path {
            self.path_changed += 1;
        }
        for name in &changes.missing {
            bump(&mut self.headers_missing, name, MAX_LABELS);
        }
        for name in &changes.added {
            bump(&mut self.headers_added, name, MAX_LABELS);
        }
        if captured {
            self.captured += 1;
        }
        self.mark_seen(marker.seq);
    }

    pub fn report(&self) -> ServerReport {
        let sorted = |m: &HashMap<String, u64>| {
            let mut v: Vec<(String, u64)> = m.iter().map(|(k, v)| (k.clone(), *v)).collect();
            v.sort();
            v.into_iter().collect()
        };
        ServerReport {
            arrivals: self.arrivals,
            by_node: sorted(&self.by_node),
            by_route: sorted(&self.by_route),
            statuses: {
                let mut v: Vec<(String, u64)> = self
                    .statuses
                    .iter()
                    .map(|(k, v)| (k.to_string(), *v))
                    .collect();
                v.sort();
                v.into_iter().collect()
            },
            processing: Percentiles::from_histogram(&self.processing),
            changes: ChangesReport {
                headers_changed: self.headers_changed,
                body_changed: self.body_changed,
                path_changed: self.path_changed,
                headers_missing: sorted(&self.headers_missing),
                headers_added: sorted(&self.headers_added),
            },
            captured: self.captured,
            seen_tracked_up_to: SEEN_CAP_BITS,
            seen_overflow: self.seen_overflow,
        }
    }
}

fn bump(map: &mut HashMap<String, u64>, key: &str, cap: usize) {
    if let Some(n) = map.get_mut(key) {
        *n += 1;
    } else if map.len() < cap {
        map.insert(key.to_string(), 1);
    } else {
        *map.entry("other".into()).or_default() += 1;
    }
}

/// The server side of a run in the report.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerReport {
    /// Marked requests that reached the server side.
    pub arrivals: u64,
    pub by_node: std::collections::BTreeMap<String, u64>,
    pub by_route: std::collections::BTreeMap<String, u64>,
    pub statuses: std::collections::BTreeMap<String, u64>,
    /// Upstream processing time (`Server-Timing: rb`).
    pub processing: Percentiles,
    pub changes: ChangesReport,
    /// Requests sampled into the inspector.
    pub captured: u64,
    pub seen_tracked_up_to: u64,
    pub seen_overflow: u64,
}

/// What the gateway changed on the way in.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChangesReport {
    /// Requests whose listed headers arrived with other values.
    pub headers_changed: u64,
    pub body_changed: u64,
    pub path_changed: u64,
    /// Listed headers that did not arrive, by name.
    pub headers_missing: std::collections::BTreeMap<String, u64>,
    /// Headers that arrived without being sent, by name (`via`, `x-forwarded-for`, ...).
    pub headers_added: std::collections::BTreeMap<String, u64>,
}

/// One run the server side knows: its key, its server profile and stats.
pub struct RunEntry {
    pub id: String,
    pub key: RunKey,
    pub profile: ServerProfile,
    pub stats: Mutex<ServerStats>,
    pub created: Instant,
}

/// The server side's registry of runs (bounded to [`KEPT_RUNS`]).
#[derive(Default)]
pub struct Correlator {
    runs: Mutex<Vec<Arc<RunEntry>>>,
    rejected: std::sync::atomic::AtomicU64,
}

impl Correlator {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Make a run known to the server side (single node: at run creation;
    /// cluster nodes get it from the leader in phase 3).
    pub fn register(&self, id: &str, key: RunKey, profile: ServerProfile) -> Arc<RunEntry> {
        let entry = Arc::new(RunEntry {
            id: id.to_string(),
            key,
            profile,
            stats: Mutex::new(ServerStats::new()),
            created: Instant::now(),
        });
        let mut runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
        runs.retain(|r| r.id != id);
        while runs.len() >= KEPT_RUNS {
            runs.remove(0);
        }
        runs.push(entry.clone());
        entry
    }

    pub fn entry(&self, id: &str) -> Option<Arc<RunEntry>> {
        self.runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find(|r| r.id == id)
            .cloned()
    }

    /// Markers refused (unknown run, bad shape or signature).
    pub fn rejected(&self) -> u64 {
        self.rejected.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn reject(&self) {
        self.rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Request extension: whether the inspector should capture this request
/// (sampled harness traffic; absent on ordinary requests).
#[derive(Clone, Copy, Debug)]
pub struct CaptureDecision(pub bool);

/// Request extension: the verified marker of a harness request, for the
/// shaped endpoint and anything else that wants the run.
#[derive(Clone)]
pub struct HarnessRequest {
    pub run: String,
    pub seq: u64,
    pub entry: Arc<RunEntry>,
}

impl std::fmt::Debug for HarnessRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessRequest")
            .field("run", &self.run)
            .field("seq", &self.seq)
            .finish_non_exhaustive()
    }
}

/// The server correlation layer. A no-op (one header lookup) without a
/// marker; with an unverifiable marker the request is served as an ordinary
/// request and counted as rejected.
pub async fn layer(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(harness) = state.harness.as_ref() else {
        return next.run(req).await;
    };
    let Some(raw) = req.headers().get(HEADER).and_then(|v| v.to_str().ok()) else {
        return next.run(req).await;
    };
    if crate::control::is_control_path(req.uri().path()) {
        return next.run(req).await;
    }
    let correlator = &harness.correlator;
    let Some(marker) = Marker::parse(raw) else {
        correlator.reject();
        return next.run(req).await;
    };
    let Some(entry) = correlator.entry(&marker.run) else {
        correlator.reject();
        return next.run(req).await;
    };
    if !marker.verify(&entry.key) {
        correlator.reject();
        return next.run(req).await;
    }
    let arrived_ns = unix_ns();
    let started = Instant::now();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| crate::metrics::UNMATCHED.to_string());
    let (parts, body) = req.into_parts();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| parts.uri.path().to_string());
    let path_digest = digest_path(&parts.method, &path_and_query);
    let (headers_digest, missing) = digest_headers(&marker.names, &parts.headers);
    let mut added: Vec<String> = parts
        .headers
        .keys()
        .map(|k| k.as_str().to_string())
        .filter(|k| !marker.names.contains(k) && !UNLISTED_HEADERS.contains(&k.as_str()))
        .collect();
    added.sort();
    added.dedup();
    // The body is read only when the client sent one (its digest is set);
    // a body over the limit is left to the body-limit layer, as changed.
    let (body, body_changed) = if marker.body_digest.is_empty() {
        (body, false)
    } else {
        let limit = state.config.body_limit;
        match http_body_util::Limited::new(body, limit).collect().await {
            Ok(collected) => {
                let bytes = collected.to_bytes();
                let changed = digest([bytes.as_ref()]) != marker.body_digest;
                (axum::body::Body::from(bytes), changed)
            }
            Err(_) => (axum::body::Body::empty(), true),
        }
    };
    let changes = Changes {
        headers: headers_digest != marker.headers_digest,
        body: body_changed,
        path: path_digest != marker.path_digest,
        missing,
        added,
    };
    let sample = entry.profile.capture_sample;
    let captured = changes.any() || (sample > 0 && marker.seq % u64::from(sample) == 0);
    let mut req = Request::from_parts(parts, body);
    req.extensions_mut().insert(CaptureDecision(captured));
    req.extensions_mut().insert(HarnessRequest {
        run: marker.run.clone(),
        seq: marker.seq,
        entry: entry.clone(),
    });
    let mut resp = next.run(req).await;
    let processing = started.elapsed();
    let done_ns = unix_ns();
    let status = resp.status().as_u16();
    let anomaly = changes.any() || status >= 500;
    {
        let mut stats = entry.stats.lock().unwrap_or_else(|p| p.into_inner());
        stats.record(
            &marker,
            &route,
            status,
            processing,
            &changes,
            captured || anomaly,
        );
    }
    state.metrics.harness_arrival(&marker.run);
    let timing = format!("rb;dur={:.3}", processing.as_secs_f64() * 1000.0);
    let headers = resp.headers_mut();
    let timing_value = match headers.get(header::HeaderName::from_static("server-timing")) {
        Some(existing) => format!("{}, {timing}", existing.to_str().unwrap_or("")),
        None => timing,
    };
    if let Ok(v) = HeaderValue::from_str(&timing_value) {
        headers.insert(header::HeaderName::from_static("server-timing"), v);
    }
    let mut tag = Tag {
        node: harness.node_id.clone(),
        run: marker.run.clone(),
        seq: marker.seq,
        arrived_ns,
        done_ns,
        sig: String::new(),
    };
    tag.sign(&entry.key);
    if let Ok(v) = HeaderValue::from_str(&tag.to_header()) {
        headers.insert(header::HeaderName::from_static(SERVED_BY), v);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker() -> Marker {
        Marker {
            run: "abc123".into(),
            node: "node-1".into(),
            seq: 42,
            vu: 3,
            sent_ns: 1_700_000_000_000_000_000,
            names: vec!["accept".into(), "x-demo".into()],
            headers_digest: "0123abcd".into(),
            body_digest: String::new(),
            path_digest: "89abcdef".into(),
            sig: String::new(),
        }
    }

    #[test]
    fn marker_round_trip_and_signature() {
        let key = RunKey::random();
        let mut m = marker();
        m.sign(&key);
        let header = m.to_header();
        assert!(header.starts_with("v1; run=abc123; node=node-1; seq=42; vu=3;"));
        let parsed = Marker::parse(&header).expect("parses");
        assert_eq!(parsed, m);
        assert!(parsed.verify(&key));
        assert!(!parsed.verify(&RunKey::random()));
        let mut tampered = parsed.clone();
        tampered.seq = 43;
        assert!(!tampered.verify(&key));
        for bad in [
            "v2; run=a",
            "v1; run=a; nope=1",
            "v1; run=a; node=b; seq=x",
            "v1; run=a; node=b; hd=zz",
            &"v1; run=a; node=b; sig=".to_string().repeat(300),
        ] {
            assert!(Marker::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn tag_round_trip_and_timing() {
        let key = RunKey::random();
        let mut t = Tag {
            node: "n".into(),
            run: "r".into(),
            seq: 7,
            arrived_ns: 10,
            done_ns: 20,
            sig: String::new(),
        };
        t.sign(&key);
        let parsed = Tag::parse(&t.to_header()).expect("parses");
        assert_eq!(parsed, t);
        assert!(parsed.verify(&key));
        assert!(Tag::parse("n; run=r; seq=1; t=1,2; sig=short").is_none());
        assert_eq!(parse_server_timing("rb;dur=1.25"), Some(1.25));
        assert_eq!(
            parse_server_timing("cache;desc=\"hit\", rb;dur=3, db;dur=1"),
            Some(3.0)
        );
        assert_eq!(parse_server_timing("db;dur=1"), None);
    }

    #[test]
    fn digests_and_traceparent() {
        let mut h = HeaderMap::new();
        h.insert("accept", HeaderValue::from_static("*/*"));
        h.insert("x-demo", HeaderValue::from_static("1"));
        let names = vec!["accept".to_string(), "x-demo".to_string()];
        let (d1, missing) = digest_headers(&names, &h);
        assert!(missing.is_empty());
        assert_eq!(d1.len(), 8);
        h.insert("x-demo", HeaderValue::from_static("2"));
        let (d2, _) = digest_headers(&names, &h);
        assert_ne!(d1, d2);
        h.remove("x-demo");
        let (_, missing) = digest_headers(&names, &h);
        assert_eq!(missing, vec!["x-demo"]);
        let tp = traceparent("run", "node", 5);
        assert_eq!(tp.len(), 55);
        assert!(tp.ends_with("-0000000000000005-01"));
        assert_ne!(traceparent("run", "node", 6), tp);
        assert_eq!(
            digest_path(&Method::GET, "/a?b=1"),
            digest_path(&Method::GET, "/a?b=1")
        );
        assert_ne!(
            digest_path(&Method::GET, "/a"),
            digest_path(&Method::POST, "/a")
        );
    }

    #[test]
    fn server_stats_are_bounded_and_track_seen() {
        let mut s = ServerStats::new();
        let m = marker();
        for i in 0..(MAX_LABELS + 5) {
            let mut mm = m.clone();
            mm.node = format!("node-{i}");
            mm.seq = i as u64 * 3;
            s.record(
                &mm,
                "/status/{code}",
                200,
                Duration::from_millis(1),
                &Changes::default(),
                false,
            );
        }
        assert_eq!(s.by_node.len(), MAX_LABELS + 1);
        assert_eq!(s.by_node["other"], 5);
        assert_eq!(s.was_seen(3), Some(true));
        assert_eq!(s.was_seen(4), Some(false));
        assert_eq!(s.was_seen(SEEN_CAP_BITS + 1), None);
        let mut big = m.clone();
        big.seq = SEEN_CAP_BITS + 1;
        s.record(
            &big,
            "r",
            200,
            Duration::from_millis(1),
            &Changes::default(),
            false,
        );
        assert_eq!(s.seen_overflow, 1);
        let r = s.report();
        assert_eq!(r.arrivals, MAX_LABELS as u64 + 6);
        assert_eq!(r.by_route["/status/{code}"], MAX_LABELS as u64 + 5);
    }

    #[test]
    fn correlator_keeps_a_bounded_number_of_runs() {
        let c = Correlator::new();
        for i in 0..(KEPT_RUNS + 3) {
            c.register(
                &format!("run{i}"),
                RunKey::random(),
                ServerProfile::default(),
            );
        }
        assert!(c.entry("run0").is_none());
        assert!(c.entry(&format!("run{}", KEPT_RUNS + 2)).is_some());
        assert_eq!(c.rejected(), 0);
    }
}
