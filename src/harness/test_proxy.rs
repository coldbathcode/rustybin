//! A small stand-in for "the gateway" (plan section 16, tests only): an
//! axum server that forwards to an upstream and misbehaves on command. It
//! adds and removes request headers, strips response headers (the served-by
//! tag), delays, answers its own status without forwarding every n-th
//! request, and drops (never answers) every n-th request. It forwards to one
//! upstream or round robin over a pool (a cluster's server nodes). Several
//! instances stand in for a gateway fleet. The reference rate limiters come with
//! phase 5.

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
}

#[derive(Default)]
pub struct Counters {
    pub requests: AtomicU64,
    pub forwarded: AtomicU64,
    pub own_answers: AtomicU64,
    pub dropped: AtomicU64,
}

struct Inner {
    /// The upstream pool, round robin by request count.
    upstreams: Vec<SocketAddr>,
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
            let (mut parts, body) = resp.into_parts();
            for name in &inner.config.strip_response_headers {
                parts.headers.remove(name.as_str());
            }
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
