use axum::{extract::Extension, http::HeaderMap, response::Response, routing::any, Router};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::content_negotiation::negotiate;
use crate::state::AppState;

// ── State ───────────────────────────────────────────────────────────

pub struct IdentityState {
    pub start_time: Instant,
    pub request_count: AtomicU64,
}

impl IdentityState {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            request_count: AtomicU64::new(0),
        }
    }
}

impl Default for IdentityState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Response types ──────────────────────────────────────────────────

#[derive(Serialize)]
struct IdentityResponse {
    instance_id: String,
    hostname: String,
    version: &'static str,
    uptime_seconds: u64,
    request_count: u64,
    port: PortInfo,
    environment: EnvInfo,
    request: RequestInfo,
    timestamp: String,
}

#[derive(Serialize)]
struct PortInfo {
    http: u16,
    https: u16,
}

#[derive(Serialize)]
struct EnvInfo {
    rust_version: &'static str,
    profile: &'static str,
}

#[derive(Serialize)]
struct RequestInfo {
    remote_ip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    forwarded_for: Option<String>,
    host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    via: Option<String>,
}

// ── Handler ─────────────────────────────────────────────────────────

async fn identity_handler(
    axum::extract::State(config): axum::extract::State<Arc<Config>>,
    Extension(state): Extension<Arc<IdentityState>>,
    headers: HeaderMap,
    crate::session::PeerAddr(peer): crate::session::PeerAddr,
) -> Response {
    let count = state.request_count.fetch_add(1, Ordering::Relaxed) + 1;
    let uptime = state.start_time.elapsed().as_secs();

    let hostname = gethostname::gethostname().to_string_lossy().to_string();

    let remote_ip = peer
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let forwarded_for = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let via = headers
        .get("via")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    negotiate(
        &headers,
        &IdentityResponse {
            instance_id: config.instance_id.clone(),
            hostname,
            version: env!("CARGO_PKG_VERSION"),
            uptime_seconds: uptime,
            request_count: count,
            port: PortInfo {
                http: config.http_port,
                https: config.https_port,
            },
            environment: EnvInfo {
                rust_version: env!("CARGO_PKG_RUST_VERSION", "unknown"),
                profile,
            },
            request: RequestInfo {
                remote_ip,
                forwarded_for,
                host,
                via,
            },
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
    )
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/identity", any(identity_handler))
        .layer(Extension(state.identity.clone()))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![Endpoint::new(
        "/identity",
        &["ANY"],
        category::HEALTH,
        "Instance identity: id, hostname, uptime, request count (load-balancing demos)",
    )
    .example(Example::get("Identity", "/identity"))]
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app() -> (Router, Arc<IdentityState>) {
        let state = crate::test_support::test_state();
        let identity = state.identity.clone();
        (
            crate::test_support::module_app_with(state, router),
            identity,
        )
    }

    async fn json_body(resp: axum::http::Response<Body>) -> serde_json::Value {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&body).expect("json")
    }

    #[tokio::test]
    async fn identity_returns_instance_info() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/identity")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["instance_id"], "test-instance");
        assert!(json["hostname"].is_string());
        assert!(json["version"].is_string());
        assert!(json["uptime_seconds"].is_number());
        assert_eq!(json["port"]["http"], 0);
        assert_eq!(json["port"]["https"], 0);
        assert!(json["environment"]["profile"].is_string());
        assert!(json["timestamp"].is_string());
    }

    #[tokio::test]
    async fn request_count_increments() {
        let state = crate::test_support::test_state();

        // First request
        let app1 = crate::test_support::module_app_with(state.clone(), router);
        let resp1 = app1
            .oneshot(
                Request::builder()
                    .uri("/identity")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let json1 = json_body(resp1).await;
        assert_eq!(json1["request_count"], 1);

        // Second request
        let app2 = crate::test_support::module_app_with(state, router);
        let resp2 = app2
            .oneshot(
                Request::builder()
                    .uri("/identity")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let json2 = json_body(resp2).await;
        assert_eq!(json2["request_count"], 2);
    }

    #[tokio::test]
    async fn forwards_headers_included() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/identity")
                    .header("x-forwarded-for", "10.0.0.1")
                    .header("host", "api.example.com")
                    .header("via", "1.1 gateway")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        let json = json_body(resp).await;
        assert_eq!(json["request"]["forwarded_for"], "10.0.0.1");
        assert_eq!(json["request"]["host"], "api.example.com");
        assert_eq!(json["request"]["via"], "1.1 gateway");
    }

    #[tokio::test]
    async fn post_method_works() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/identity")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn xml_content_negotiation() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/identity")
                    .header("accept", "application/xml")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(
            resp.headers()
                .get("content-type")
                .expect("ct")
                .to_str()
                .expect("str"),
            "application/xml"
        );
    }
}
