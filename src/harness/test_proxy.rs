//! A small stand-in for "the gateway" (plan section 16, tests only): an
//! axum server that forwards to an upstream and misbehaves on command. It
//! adds and removes request headers, strips response headers (the served-by
//! tag), delays, answers its own status without forwarding every n-th
//! request, and drops (never answers) every n-th request. It forwards to one
//! upstream or round robin over a pool (a cluster's server nodes). Several
//! instances stand in for a gateway fleet. Its reference rate limiters
//! ([`Limiter`]: fixed window, sliding window, token bucket; counters shared
//! by every instance or local to each) have known flaws to switch on, so
//! every rate-limit test has something to find.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

/// What the proxy does to traffic.
#[derive(Clone, Debug, Default)]
pub struct ProxyConfig {
    /// Headers added to every forwarded request.
    pub add_request_headers: Vec<(String, String)>,
    /// Request headers removed before forwarding.
    pub remove_request_headers: Vec<String>,
    /// Request headers rewritten before forwarding.
    pub rewrite_request_headers: Vec<(String, String)>,
    /// Response headers removed on the way back (the served-by tag, say).
    pub strip_response_headers: Vec<String>,
    /// Added to every response.
    pub add_response_headers: Vec<(String, String)>,
    /// Delay before forwarding (the gateway's own overhead).
    pub delay: Duration,
    /// Every n-th request (1-based count) is answered by the proxy itself
    /// with this status, without forwarding.
    pub own_answer_every: Option<(u64, u16)>,
    /// Every n-th request is never answered (held until the client gives up).
    pub drop_every: Option<u64>,
    /// Prepend this to the forwarded path.
    pub path_prefix: String,
    /// A gateway at capacity: requests over this many in a wall-clock
    /// second are answered 503 by the proxy.
    pub capacity_per_sec: Option<u64>,
    /// A rate limiter. Give several instances the same one for counters
    /// shared across the fleet, each its own for counters local to a node.
    pub limiter: Option<Arc<Limiter>>,
}

/// How a reference limiter counts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Algorithm {
    /// Windows at multiples of the window on the wall clock (Unix time):
    /// up to twice the limit gets through around a boundary.
    FixedWindow,
    /// A log of the last window's admissions per key.
    SlidingWindow,
    /// `burst` tokens (default the limit), refilled at limit per window.
    TokenBucket,
}

/// What a reference limiter counts by.
#[derive(Clone, Debug, PartialEq)]
pub enum LimitKey {
    /// A request header (an API key).
    Header(String),
    /// `X-Forwarded-For`, trusted (the flaw `many_ips` finds).
    Forwarded,
    /// The whole fleet (or instance) at once.
    Global,
}

/// A reference rate limiter for the test proxy.
pub struct Limiter {
    pub algorithm: Algorithm,
    pub limit: u64,
    pub window: Duration,
    pub burst: Option<u64>,
    pub key: LimitKey,
    /// Add `X-RateLimit-*`, `RateLimit-Policy` and (on refusals)
    /// `Retry-After`.
    pub headers: bool,
    /// The flaw of a limiter that does not shield the upstream: refused
    /// requests are forwarded anyway, and the answer replaced with a 429.
    pub forward_refused: bool,
    /// Requests admitted and refused.
    pub admitted: AtomicU64,
    pub refused: AtomicU64,
    keys: std::sync::Mutex<HashMap<String, KeyState>>,
}

impl std::fmt::Debug for Limiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Limiter")
            .field("algorithm", &self.algorithm)
            .field("limit", &self.limit)
            .field("window", &self.window)
            .field("key", &self.key)
            .finish()
    }
}

#[derive(Default)]
struct KeyState {
    window: u64,
    count: u64,
    log: VecDeque<std::time::Instant>,
    tokens: f64,
    refilled: Option<std::time::Instant>,
}

/// What a limiter decided: admitted, the requests left and the seconds
/// until more are.
struct Decision {
    allowed: bool,
    remaining: u64,
    reset_secs: f64,
}

impl Limiter {
    pub fn new(algorithm: Algorithm, limit: u64, window: Duration) -> Self {
        Self {
            algorithm,
            limit,
            window,
            burst: None,
            key: LimitKey::Global,
            headers: false,
            forward_refused: false,
            admitted: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            keys: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn keyed(mut self, key: LimitKey) -> Self {
        self.key = key;
        self
    }

    pub fn with_headers(mut self) -> Self {
        self.headers = true;
        self
    }

    pub fn forwarding_refused(mut self) -> Self {
        self.forward_refused = true;
        self
    }

    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }

    fn key_of(&self, headers: &axum::http::HeaderMap) -> String {
        let get = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.split(',').next().unwrap_or("").trim().to_string())
        };
        match &self.key {
            LimitKey::Header(name) => get(name).unwrap_or_else(|| "-".into()),
            LimitKey::Forwarded => get("x-forwarded-for").unwrap_or_else(|| "peer".into()),
            LimitKey::Global => "*".into(),
        }
    }

    fn check(&self, key: &str) -> Decision {
        let now = std::time::Instant::now();
        let w = self.window.as_secs_f64();
        let mut keys = self.keys.lock().unwrap_or_else(|p| p.into_inner());
        let st = keys.entry(key.to_string()).or_default();
        let d = match self.algorithm {
            Algorithm::FixedWindow => {
                let unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                let index = (unix / w).floor() as u64;
                if st.window != index {
                    st.window = index;
                    st.count = 0;
                }
                let allowed = st.count < self.limit;
                if allowed {
                    st.count += 1;
                }
                Decision {
                    allowed,
                    remaining: self.limit - st.count,
                    reset_secs: (index + 1) as f64 * w - unix,
                }
            }
            Algorithm::SlidingWindow => {
                while st
                    .log
                    .front()
                    .is_some_and(|t| now.duration_since(*t) >= self.window)
                {
                    st.log.pop_front();
                }
                let allowed = (st.log.len() as u64) < self.limit;
                if allowed {
                    st.log.push_back(now);
                }
                Decision {
                    allowed,
                    remaining: self.limit.saturating_sub(st.log.len() as u64),
                    reset_secs: st
                        .log
                        .front()
                        .map_or(0.0, |t| w - now.duration_since(*t).as_secs_f64()),
                }
            }
            Algorithm::TokenBucket => {
                let capacity = self.burst.unwrap_or(self.limit) as f64;
                let rate = self.limit as f64 / w;
                let last = st.refilled.get_or_insert_with(|| {
                    st.tokens = capacity;
                    now
                });
                st.tokens =
                    (st.tokens + now.duration_since(*last).as_secs_f64() * rate).min(capacity);
                st.refilled = Some(now);
                let allowed = st.tokens >= 1.0;
                if allowed {
                    st.tokens -= 1.0;
                }
                Decision {
                    allowed,
                    remaining: st.tokens.floor() as u64,
                    reset_secs: ((1.0 - st.tokens).max(0.0)) / rate,
                }
            }
        };
        if d.allowed {
            self.admitted.fetch_add(1, Ordering::Relaxed);
        } else {
            self.refused.fetch_add(1, Ordering::Relaxed);
        }
        d
    }

    fn headers_for(&self, d: &Decision) -> Vec<(String, String)> {
        if !self.headers {
            return Vec::new();
        }
        let mut out = vec![
            ("x-ratelimit-limit".to_string(), self.limit.to_string()),
            ("x-ratelimit-remaining".to_string(), d.remaining.to_string()),
            (
                "ratelimit-policy".to_string(),
                format!("{};w={}", self.limit, self.window.as_secs_f64()),
            ),
        ];
        if !d.allowed {
            out.push((
                "retry-after".to_string(),
                (d.reset_secs.ceil().max(1.0) as u64).to_string(),
            ));
        }
        out
    }
}

#[derive(Default)]
pub struct Counters {
    pub requests: AtomicU64,
    pub forwarded: AtomicU64,
    pub own_answers: AtomicU64,
    pub dropped: AtomicU64,
    /// Answered 503 over `capacity_per_sec`.
    pub over_capacity: AtomicU64,
    /// When each request arrived (the first 100 000).
    pub arrivals: std::sync::Mutex<Vec<std::time::Instant>>,
}

struct Inner {
    /// The upstream pool, round robin by request count.
    upstreams: Vec<SocketAddr>,
    started: std::time::Instant,
    /// The current wall-clock second and the requests in it (capacity).
    second: std::sync::Mutex<(u64, u64)>,
    config: ProxyConfig,
    counters: Arc<Counters>,
    client: Client<HttpConnector, Full<Bytes>>,
}

/// A running proxy instance.
pub struct Proxy {
    pub addr: SocketAddr,
    pub counters: Arc<Counters>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Proxy {
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

async fn forward(State(inner): State<Arc<Inner>>, req: Request) -> Response {
    let n = inner.counters.requests.fetch_add(1, Ordering::Relaxed) + 1;
    {
        let mut arrivals = inner
            .counters
            .arrivals
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if arrivals.len() < 100_000 {
            arrivals.push(std::time::Instant::now());
        }
    }
    if let Some(cap) = inner.config.capacity_per_sec {
        let sec = inner.started.elapsed().as_secs();
        let over = {
            let mut cur = inner.second.lock().unwrap_or_else(|p| p.into_inner());
            if cur.0 != sec {
                *cur = (sec, 0);
            }
            cur.1 += 1;
            cur.1 > cap
        };
        if over {
            inner.counters.over_capacity.fetch_add(1, Ordering::Relaxed);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "over the test proxy's capacity",
            )
                .into_response();
        }
    }
    let mut limit_headers = Vec::new();
    let mut refuse_after_forwarding = false;
    if let Some(limiter) = &inner.config.limiter {
        let d = limiter.check(&limiter.key_of(req.headers()));
        limit_headers = limiter.headers_for(&d);
        if !d.allowed {
            if limiter.forward_refused {
                refuse_after_forwarding = true;
            } else {
                let mut resp = (
                    StatusCode::TOO_MANY_REQUESTS,
                    "rate limited by the test proxy",
                )
                    .into_response();
                apply_headers(resp.headers_mut(), &limit_headers);
                apply_headers(resp.headers_mut(), &inner.config.add_response_headers);
                return resp;
            }
        }
    }
    if let Some(every) = inner.config.drop_every {
        if every > 0 && n % every == 0 {
            inner.counters.dropped.fetch_add(1, Ordering::Relaxed);
            // Never answers: the client times out.
            std::future::pending::<()>().await;
        }
    }
    if let Some((every, status)) = inner.config.own_answer_every {
        if every > 0 && n % every == 0 {
            inner.counters.own_answers.fetch_add(1, Ordering::Relaxed);
            let mut resp = (
                StatusCode::from_u16(status).unwrap_or(StatusCode::TOO_MANY_REQUESTS),
                "answered by the test proxy",
            )
                .into_response();
            apply_headers(resp.headers_mut(), &inner.config.add_response_headers);
            return resp;
        }
    }
    if !inner.config.delay.is_zero() {
        tokio::time::sleep(inner.config.delay).await;
    }
    let (mut parts, body) = req.into_parts();
    for name in &inner.config.remove_request_headers {
        parts.headers.remove(name.as_str());
    }
    apply_headers(&mut parts.headers, &inner.config.rewrite_request_headers);
    apply_headers(&mut parts.headers, &inner.config.add_request_headers);
    let path = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".into());
    let upstream = inner.upstreams[(n as usize) % inner.upstreams.len()];
    let uri: hyper::Uri =
        match format!("http://{}{}{}", upstream, inner.config.path_prefix, path).parse() {
            Ok(u) => u,
            Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
        };
    parts.uri = uri;
    parts.headers.remove("host");
    let bytes = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let upstream_req = Request::from_parts(parts, Full::new(bytes));
    inner.counters.forwarded.fetch_add(1, Ordering::Relaxed);
    match inner.client.request(upstream_req).await {
        Ok(resp) => {
            if refuse_after_forwarding {
                // The upstream did the work; the client is told it was refused.
                let _ = resp.into_body().collect().await;
                let mut out = (
                    StatusCode::TOO_MANY_REQUESTS,
                    "rate limited by the test proxy",
                )
                    .into_response();
                apply_headers(out.headers_mut(), &limit_headers);
                apply_headers(out.headers_mut(), &inner.config.add_response_headers);
                return out;
            }
            let (mut parts, body) = resp.into_parts();
            for name in &inner.config.strip_response_headers {
                parts.headers.remove(name.as_str());
            }
            apply_headers(&mut parts.headers, &limit_headers);
            apply_headers(&mut parts.headers, &inner.config.add_response_headers);
            Response::from_parts(parts, Body::new(body))
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("upstream error: {e}")).into_response(),
    }
}

fn apply_headers(headers: &mut axum::http::HeaderMap, list: &[(String, String)]) {
    for (name, value) in list {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(n, v);
        }
    }
}

/// Start a proxy in front of `upstream` on an ephemeral loopback port.
pub async fn start(upstream: SocketAddr, config: ProxyConfig) -> Proxy {
    start_pool(vec![upstream], config).await
}

/// Start a proxy in front of an upstream pool (round robin), as a gateway
/// in front of a cluster's server nodes.
pub async fn start_pool(upstreams: Vec<SocketAddr>, config: ProxyConfig) -> Proxy {
    assert!(!upstreams.is_empty(), "an upstream pool needs a member");
    let counters = Arc::new(Counters::default());
    let mut connector = HttpConnector::new();
    connector.set_nodelay(true);
    let client = Client::builder(TokioExecutor::new())
        .pool_max_idle_per_host(1024)
        .build(connector);
    let inner = Arc::new(Inner {
        upstreams,
        started: std::time::Instant::now(),
        second: std::sync::Mutex::new((0, 0)),
        config,
        counters: counters.clone(),
        client,
    });
    let app = Router::new().fallback(forward).with_state(inner);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test proxy");
    let addr = listener.local_addr().expect("proxy addr");
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Proxy {
        addr,
        counters,
        task,
    }
}
