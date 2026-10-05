//! Prometheus metrics: `GET /_rustybin/metrics` (text exposition format 0.0.4).
//!
//! Series (every label set is bounded):
//! - `rustybin_requests_total{route,method,status_class}`: `route` is the
//!   matched axum route template (`/status/{code}`), never the raw path;
//!   unmatched requests use `unmatched`.
//! - `rustybin_request_duration_seconds{route}`: time until response headers
//!   (histogram, buckets from 1 ms to 60 s, see [`BUCKETS`], so
//!   `histogram_quantile` gives usable p50 and p99 values).
//! - `rustybin_streams_open`: SSE responses and WebSocket connections open now.
//! - `rustybin_egress_bytes_total`: response body bytes sent.
//! - `rustybin_protocol_requests_total{protocol}`: `http`, `graphql`, `grpc`,
//!   `websocket`, `sse`, `mcp`, `a2a`, `llm`.
//! - `rustybin_llm_tokens_total{provider,model_family,direction}`: mock LLM
//!   tokens; models map to a fixed list of families (unknown: `other`).
//! - `rustybin_llm_requests_total{provider,model_family,streaming}`: mock LLM
//!   requests served (`streaming` is `true` or `false`), same families.
//! - `rustybin_llm_faults_total{provider,kind}`: native provider errors (and
//!   content filter results) injected via `X-Rustybin-Fail` / `?fail=` on the
//!   mock LLM; `kind` comes from a fixed list ([`LLM_FAULT_KINDS`]).
//! - `rustybin_limit_rejections_total{dimension}`: requests the plan limiter
//!   rejected (HTTP 429 or gRPC `RESOURCE_EXHAUSTED`); `dimension` is `rps`,
//!   `concurrency`, `streams`, `requests` or `egress`.
//! - `rustybin_faults_injected_total{kind}`: `fail`, `delay`, `ai`.
//! - `rustybin_build_info{version,git_sha}`: always 1.
//!
//! The counters live in [`Metrics`] (shared through `AppState`); the
//! [`track`] middleware feeds the HTTP series, the gRPC services count
//! themselves ([`Metrics::count_protocol`]), the mock LLM reports tokens,
//! requests and faults ([`Metrics::record_llm_tokens`],
//! [`Metrics::count_llm_request`], [`Metrics::count_llm_fault`]) and the plan
//! limiter its rejections ([`Metrics::count_limit_rejection`]). Protected like
//! every control route.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{MatchedPath, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::limits::Dimension;
use crate::state::AppState;

/// Path of the metrics endpoint.
pub const METRICS_PATH: &str = "/_rustybin/metrics";
/// Most distinct `route` label values (beyond: `other`). The catalogue has
/// far fewer routes; this is a safety net.
pub const MAX_ROUTES: usize = 1024;
/// Most distinct series per LLM counter (beyond: dropped into `other`).
pub const MAX_LLM_SERIES: usize = 256;
/// Histogram bucket upper bounds, in seconds: 1 ms to 60 s in 1, 2.5, 5
/// steps, so `histogram_quantile(0.5, ...)` and `histogram_quantile(0.99,
/// ...)` stay meaningful from fast stubs up to slow mock LLM streams and
/// long injected delays.
pub const BUCKETS: [f64; 15] = [
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
];
/// `kind` label values of `rustybin_llm_faults_total` (anything else: `other`).
pub const LLM_FAULT_KINDS: [&str; 16] = [
    "bad_request",
    "missing_credential",
    "invalid_credential",
    "forbidden",
    "not_found",
    "too_large",
    "rate_limit",
    "server_error",
    "unavailable",
    "overloaded",
    "timeout",
    "context_length",
    "prompt_filter",
    "content_filter",
    "status_4xx",
    "status_5xx",
];
/// `provider` label values of the LLM counters (anything else: `other`).
pub const LLM_PROVIDERS: [&str; 7] = [
    "openai",
    "azure",
    "anthropic",
    "gemini",
    "bedrock",
    "ollama",
    "cohere",
];
/// Route label of requests no route matched (JSON 404 fallback).
pub const UNMATCHED: &str = "unmatched";

/// Protocol label values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Http,
    Graphql,
    Grpc,
    Websocket,
    Sse,
    Mcp,
    A2a,
    Llm,
}

impl Protocol {
    pub const ALL: [Protocol; 8] = [
        Protocol::Http,
        Protocol::Graphql,
        Protocol::Grpc,
        Protocol::Websocket,
        Protocol::Sse,
        Protocol::Mcp,
        Protocol::A2a,
        Protocol::Llm,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Http => "http",
            Protocol::Graphql => "graphql",
            Protocol::Grpc => "grpc",
            Protocol::Websocket => "websocket",
            Protocol::Sse => "sse",
            Protocol::Mcp => "mcp",
            Protocol::A2a => "a2a",
            Protocol::Llm => "llm",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Default)]
struct Histogram {
    buckets: [u64; BUCKETS.len()],
    count: u64,
    sum: f64,
}

impl Histogram {
    fn observe(&mut self, secs: f64) {
        for (i, bound) in BUCKETS.iter().enumerate() {
            if secs <= *bound {
                self.buckets[i] += 1;
            }
        }
        self.count += 1;
        self.sum += secs;
    }
}

#[derive(Default)]
struct RouteStats {
    /// (method, status class) -> count.
    requests: HashMap<(&'static str, &'static str), u64>,
    duration: Histogram,
}

#[derive(Default)]
struct Inner {
    routes: HashMap<String, RouteStats>,
    llm_tokens: HashMap<(&'static str, &'static str, &'static str), u64>,
    llm_requests: HashMap<(&'static str, &'static str, bool), u64>,
    llm_faults: HashMap<(&'static str, &'static str), u64>,
    faults: HashMap<&'static str, u64>,
}

/// Process-wide metric counters.
pub struct Metrics {
    inner: Mutex<Inner>,
    streams_open: AtomicI64,
    egress_bytes: AtomicU64,
    protocols: [AtomicU64; 8],
    limit_rejections: [AtomicU64; Dimension::ALL.len()],
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Metrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Metrics").finish_non_exhaustive()
    }
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            streams_open: AtomicI64::new(0),
            egress_bytes: AtomicU64::new(0),
            protocols: Default::default(),
            limit_rejections: Default::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Count one finished HTTP exchange (headers sent).
    pub fn observe_request(&self, route: &str, method: &Method, status: u16, elapsed: Duration) {
        let method = method_label(method);
        let class = status_class(status);
        let mut inner = self.lock();
        let at_cap = inner.routes.len() >= MAX_ROUTES;
        let key = if !inner.routes.contains_key(route) && at_cap {
            "other"
        } else {
            route
        };
        if !inner.routes.contains_key(key) {
            inner.routes.insert(key.to_string(), RouteStats::default());
        }
        if let Some(stats) = inner.routes.get_mut(key) {
            *stats.requests.entry((method, class)).or_insert(0) += 1;
            stats.duration.observe(elapsed.as_secs_f64());
        }
    }

    pub fn count_protocol(&self, protocol: Protocol) {
        self.protocols[protocol.index()].fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_egress(&self, bytes: usize) {
        self.egress_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Count an injected fault (the `X-Rustybin-Fault` response header value).
    pub fn count_fault(&self, kind: &str) {
        let kind = match kind {
            "fail" => "fail",
            "delay" => "delay",
            "ai" => "ai",
            _ => "other",
        };
        *self.lock().faults.entry(kind).or_insert(0) += 1;
    }

    /// Count mock LLM tokens (input = prompt, output = completion).
    pub fn record_llm_tokens(&self, provider: &'static str, model: &str, input: u32, output: u32) {
        let family = model_family(model);
        let mut inner = self.lock();
        for (direction, n) in [("input", input), ("output", output)] {
            if n == 0 {
                continue;
            }
            let mut key = (provider, family, direction);
            if !inner.llm_tokens.contains_key(&key) && inner.llm_tokens.len() >= MAX_LLM_SERIES {
                key = ("other", "other", direction);
            }
            *inner.llm_tokens.entry(key).or_insert(0) += u64::from(n);
        }
    }

    /// Count one mock LLM request served (once per recorded exchange).
    pub fn count_llm_request(&self, provider: &str, model: &str, streaming: bool) {
        let provider = llm_provider(provider);
        let family = model_family(model);
        let mut inner = self.lock();
        let mut key = (provider, family, streaming);
        if !inner.llm_requests.contains_key(&key) && inner.llm_requests.len() >= MAX_LLM_SERIES {
            key = ("other", "other", streaming);
        }
        *inner.llm_requests.entry(key).or_insert(0) += 1;
    }

    /// Count one fault injected on the mock LLM (`kind`: one of
    /// [`LLM_FAULT_KINDS`], anything else is `other`).
    pub fn count_llm_fault(&self, provider: &str, kind: &str) {
        let provider = llm_provider(provider);
        let kind = LLM_FAULT_KINDS
            .iter()
            .copied()
            .find(|k| *k == kind)
            .unwrap_or("other");
        *self.lock().llm_faults.entry((provider, kind)).or_insert(0) += 1;
    }

    /// Count one plan limiter rejection (HTTP 429 or gRPC RESOURCE_EXHAUSTED).
    pub fn count_limit_rejection(&self, dimension: Dimension) {
        self.limit_rejections[dimension.index()].fetch_add(1, Ordering::Relaxed);
    }

    /// A guard counting one open stream until dropped.
    pub fn open_stream(self: &Arc<Self>) -> StreamGuard {
        self.streams_open.fetch_add(1, Ordering::Relaxed);
        StreamGuard(self.clone())
    }

    pub fn streams_open(&self) -> i64 {
        self.streams_open.load(Ordering::Relaxed)
    }

    /// Distinct `route` label values recorded so far (tests, diagnostics).
    pub fn route_labels(&self) -> Vec<String> {
        let mut v: Vec<String> = self.lock().routes.keys().cloned().collect();
        v.sort();
        v
    }

    /// The text exposition.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(16 * 1024);
        let inner = self.lock();
        let mut routes: Vec<(&String, &RouteStats)> = inner.routes.iter().collect();
        routes.sort_by(|a, b| a.0.cmp(b.0));

        out.push_str("# HELP rustybin_requests_total HTTP requests by matched route template, method and status class.\n");
        out.push_str("# TYPE rustybin_requests_total counter\n");
        for (route, stats) in &routes {
            let mut series: Vec<_> = stats.requests.iter().collect();
            series.sort();
            for ((method, class), n) in series {
                let _ = writeln!(
                    out,
                    "rustybin_requests_total{{route=\"{}\",method=\"{method}\",status_class=\"{class}\"}} {n}",
                    escape(route)
                );
            }
        }

        out.push_str("# HELP rustybin_request_duration_seconds Time until response headers, by matched route template.\n");
        out.push_str("# TYPE rustybin_request_duration_seconds histogram\n");
        for (route, stats) in &routes {
            let route = escape(route);
            let h = &stats.duration;
            for (bound, n) in BUCKETS.iter().zip(h.buckets.iter()) {
                let _ = writeln!(
                    out,
                    "rustybin_request_duration_seconds_bucket{{route=\"{route}\",le=\"{bound}\"}} {n}"
                );
            }
            let _ = writeln!(
                out,
                "rustybin_request_duration_seconds_bucket{{route=\"{route}\",le=\"+Inf\"}} {}",
                h.count
            );
            let _ = writeln!(
                out,
                "rustybin_request_duration_seconds_sum{{route=\"{route}\"}} {}",
                h.sum
            );
            let _ = writeln!(
                out,
                "rustybin_request_duration_seconds_count{{route=\"{route}\"}} {}",
                h.count
            );
        }

        out.push_str(
            "# HELP rustybin_streams_open Open SSE responses and WebSocket connections.\n",
        );
        out.push_str("# TYPE rustybin_streams_open gauge\n");
        let _ = writeln!(out, "rustybin_streams_open {}", self.streams_open().max(0));

        out.push_str("# HELP rustybin_egress_bytes_total Response body bytes sent.\n");
        out.push_str("# TYPE rustybin_egress_bytes_total counter\n");
        let _ = writeln!(
            out,
            "rustybin_egress_bytes_total {}",
            self.egress_bytes.load(Ordering::Relaxed)
        );

        out.push_str("# HELP rustybin_protocol_requests_total Requests by protocol.\n");
        out.push_str("# TYPE rustybin_protocol_requests_total counter\n");
        for p in Protocol::ALL {
            let _ = writeln!(
                out,
                "rustybin_protocol_requests_total{{protocol=\"{}\"}} {}",
                p.as_str(),
                self.protocols[p.index()].load(Ordering::Relaxed)
            );
        }

        out.push_str("# HELP rustybin_llm_tokens_total Mock LLM tokens by provider, model family and direction.\n");
        out.push_str("# TYPE rustybin_llm_tokens_total counter\n");
        let mut tokens: Vec<_> = inner.llm_tokens.iter().collect();
        tokens.sort();
        for ((provider, family, direction), n) in tokens {
            let _ = writeln!(
                out,
                "rustybin_llm_tokens_total{{provider=\"{provider}\",model_family=\"{family}\",direction=\"{direction}\"}} {n}"
            );
        }

        out.push_str("# HELP rustybin_llm_requests_total Mock LLM requests by provider, model family and streaming.\n");
        out.push_str("# TYPE rustybin_llm_requests_total counter\n");
        let mut requests: Vec<_> = inner.llm_requests.iter().collect();
        requests.sort();
        for ((provider, family, streaming), n) in requests {
            let _ = writeln!(
                out,
                "rustybin_llm_requests_total{{provider=\"{provider}\",model_family=\"{family}\",streaming=\"{streaming}\"}} {n}"
            );
        }

        out.push_str("# HELP rustybin_llm_faults_total Faults injected on the mock LLM (native provider errors, content filter) by provider and kind.\n");
        out.push_str("# TYPE rustybin_llm_faults_total counter\n");
        let mut llm_faults: Vec<_> = inner.llm_faults.iter().collect();
        llm_faults.sort();
        for ((provider, kind), n) in llm_faults {
            let _ = writeln!(
                out,
                "rustybin_llm_faults_total{{provider=\"{provider}\",kind=\"{kind}\"}} {n}"
            );
        }

        out.push_str("# HELP rustybin_limit_rejections_total Requests rejected by the plan limiter (HTTP 429, gRPC RESOURCE_EXHAUSTED) by dimension.\n");
        out.push_str("# TYPE rustybin_limit_rejections_total counter\n");
        for d in Dimension::ALL {
            let _ = writeln!(
                out,
                "rustybin_limit_rejections_total{{dimension=\"{}\"}} {}",
                d.as_str(),
                self.limit_rejections[d.index()].load(Ordering::Relaxed)
            );
        }

        out.push_str("# HELP rustybin_faults_injected_total Faults injected (X-Rustybin-Fail, X-Rustybin-Delay, mock LLM faults).\n");
        out.push_str("# TYPE rustybin_faults_injected_total counter\n");
        let mut faults: Vec<_> = inner.faults.iter().collect();
        faults.sort();
        for (kind, n) in faults {
            let _ = writeln!(out, "rustybin_faults_injected_total{{kind=\"{kind}\"}} {n}");
        }

        out.push_str("# HELP rustybin_build_info Build information (always 1).\n");
        out.push_str("# TYPE rustybin_build_info gauge\n");
        let _ = writeln!(
            out,
            "rustybin_build_info{{version=\"{}\",git_sha=\"{}\"}} 1",
            escape(env!("CARGO_PKG_VERSION")),
            escape(crate::control::git_sha().unwrap_or("unknown"))
        );
        out
    }
}

/// Counts one open stream (SSE response or WebSocket connection) while alive.
#[derive(Debug)]
pub struct StreamGuard(Arc<Metrics>);

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.0.streams_open.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Request extension on WebSocket upgrade requests: the handler calls
/// [`StreamTracker::open`] inside its upgrade callback and keeps the guard
/// for the life of the socket.
#[derive(Clone, Debug)]
pub struct StreamTracker(pub Arc<Metrics>);

impl StreamTracker {
    pub fn open(&self) -> StreamGuard {
        self.0.open_stream()
    }
}

/// `StreamTracker::open` for an optional extension.
pub fn open_ws(tracker: &Option<axum::Extension<StreamTracker>>) -> Option<StreamGuard> {
    tracker.as_ref().map(|t| t.0.open())
}

/// Prometheus label value escaping.
fn escape(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::HEAD => "HEAD",
        Method::OPTIONS => "OPTIONS",
        Method::CONNECT => "CONNECT",
        Method::TRACE => "TRACE",
        _ => "OTHER",
    }
}

fn status_class(status: u16) -> &'static str {
    match status {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    }
}

/// Provider label: a fixed list, everything else is `other`.
fn llm_provider(provider: &str) -> &'static str {
    LLM_PROVIDERS
        .iter()
        .copied()
        .find(|p| *p == provider)
        .unwrap_or("other")
}

/// Model family label: a fixed list, everything else is `other`.
pub fn model_family(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    // Provider-qualified ids: `openai/gpt-4o`, `us.anthropic.claude-...`.
    let m = m.rsplit('/').next().unwrap_or(&m);
    const FAMILIES: &[(&str, &str)] = &[
        ("gpt-4o", "gpt-4o"),
        ("gpt-4.1", "gpt-4.1"),
        ("gpt-4", "gpt-4"),
        ("gpt-5", "gpt-5"),
        ("gpt-3.5", "gpt-3.5"),
        ("gpt-oss", "gpt-oss"),
        ("o1", "o-series"),
        ("o3", "o-series"),
        ("o4", "o-series"),
        ("claude", "claude"),
        ("gemini", "gemini"),
        ("gemma", "gemma"),
        ("llama", "llama"),
        ("mixtral", "mistral"),
        ("mistral", "mistral"),
        ("command", "command"),
        ("titan", "titan"),
        ("nova", "nova"),
        ("qwen", "qwen"),
        ("deepseek", "deepseek"),
        ("phi", "phi"),
        ("rerank", "rerank"),
        ("embed", "embedding"),
    ];
    // `o1`, `o3`, `o4` only as a prefix; the others anywhere in the id.
    for (needle, family) in FAMILIES {
        let hit = if needle.len() == 2 && needle.starts_with('o') {
            m.starts_with(needle)
        } else {
            m.contains(needle)
        };
        if hit {
            return family;
        }
    }
    "other"
}

/// Protocol of a catalogued route (by category); `None` for plain HTTP.
fn route_protocol(route: &str) -> Option<Protocol> {
    static MAP: OnceLock<HashMap<&'static str, Protocol>> = OnceLock::new();
    MAP.get_or_init(|| {
        crate::catalog::all()
            .iter()
            .filter_map(|ep| {
                let p = match ep.category {
                    category::AI_OPENAI | category::AI_ANTHROPIC | category::AI_MOCK => {
                        Protocol::Llm
                    }
                    category::MCP => Protocol::Mcp,
                    category::A2A => Protocol::A2a,
                    category::GRAPHQL => Protocol::Graphql,
                    _ => return None,
                };
                Some((ep.path, p))
            })
            .collect()
    })
    .get(route)
    .copied()
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.trim_start()
                .get(..17)
                .is_some_and(|p| p.eq_ignore_ascii_case("text/event-stream"))
        })
}

/// Middleware feeding the HTTP series (outermost, after routing so the
/// matched route template is known).
pub async fn track(State(metrics): State<Arc<Metrics>>, mut req: Request, next: Next) -> Response {
    let start = Instant::now();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string());
    let method = req.method().clone();
    let websocket = is_websocket_upgrade(req.headers());
    if websocket {
        req.extensions_mut().insert(StreamTracker(metrics.clone()));
    }
    let resp = next.run(req).await;
    let elapsed = start.elapsed();

    let sse = is_event_stream(resp.headers());
    let protocol = route
        .as_deref()
        .and_then(route_protocol)
        .unwrap_or(if websocket {
            Protocol::Websocket
        } else if sse {
            Protocol::Sse
        } else {
            Protocol::Http
        });
    metrics.count_protocol(protocol);
    if let Some(kind) = resp
        .headers()
        .get(crate::fault::INJECTED_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        metrics.count_fault(kind);
    }
    metrics.observe_request(
        route.as_deref().unwrap_or(UNMATCHED),
        &method,
        resp.status().as_u16(),
        elapsed,
    );

    let (parts, body) = resp.into_parts();
    let guard = sse.then(|| metrics.open_stream());
    let counted = CountedBody {
        inner: body,
        metrics,
        _stream: guard,
    };
    Response::from_parts(parts, Body::new(counted))
}

/// Response body wrapper: counts egress bytes and holds the open stream
/// guard of an SSE response until the body ends or is dropped.
struct CountedBody {
    inner: Body,
    metrics: Arc<Metrics>,
    _stream: Option<StreamGuard>,
}

impl HttpBody for CountedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.metrics.add_egress(data.len());
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(None) => {
                this._stream = None;
                Poll::Ready(None)
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

// ── Endpoint ────────────────────────────────────────────────────────

async fn metrics_handler(State(metrics): State<Arc<Metrics>>) -> Response {
    let mut resp = metrics.render().into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new().route(METRICS_PATH, get(metrics_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![Endpoint::new(
        METRICS_PATH,
        &["GET"],
        category::CONTROL,
        "Prometheus metrics (requests by route template, latency, streams, egress, protocols, LLM tokens, requests and faults, limit rejections)",
    )
    .description(
        "Text exposition format 0.0.4. Labels are bounded: the route label is the matched route \
         template, never the raw path. Protected like the rest of the control plane \
         (RUSTYBIN_CONTROL_AUTH).",
    )
    .example(Example::get("Scrape the metrics", METRICS_PATH))]
}

pub fn openapi_paths() -> Value {
    json!({
        METRICS_PATH: {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Prometheus metrics",
                "description": "Prometheus text exposition (version 0.0.4): rustybin_requests_total{route,method,status_class}, rustybin_request_duration_seconds{route} (histogram, buckets 1 ms to 60 s), rustybin_streams_open, rustybin_egress_bytes_total, rustybin_protocol_requests_total{protocol}, rustybin_llm_tokens_total{provider,model_family,direction}, rustybin_llm_requests_total{provider,model_family,streaming}, rustybin_llm_faults_total{provider,kind}, rustybin_limit_rejections_total{dimension}, rustybin_faults_injected_total{kind}, rustybin_build_info{version,git_sha}. The route label is the matched route template.",
                "operationId": "getRustybinMetrics",
                "responses": {
                    "200": { "description": "Metrics", "content": { "text/plain": { "schema": { "type": "string" } } } },
                    "401": { "description": "Control-plane authentication required (RUSTYBIN_CONTROL_AUTH token or jwt)" }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests;
