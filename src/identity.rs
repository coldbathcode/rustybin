use axum::{
    extract::Extension,
    http::HeaderMap,
    response::Response,
    routing::any,
    Router,
};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::config::Config;
use crate::content_negotiation::negotiate;

// ── State ───────────────────────────────────────────────────────────

pub struct IdentityState {
    pub start_time: Instant,
    pub request_count: AtomicU64,
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
    connect_info: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
) -> Response {
    let count = state.request_count.fetch_add(1, Ordering::Relaxed) + 1;
    let uptime = state.start_time.elapsed().as_secs();

    let hostname = gethostname::gethostname()
        .to_string_lossy()
        .to_string();

    let remote_ip = connect_info
        .map(|ci| ci.0.ip().to_string())
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

pub fn router(identity_state: Arc<IdentityState>) -> Router<Arc<Config>> {
    Router::new()
        .route("/identity", any(identity_handler))
        .layer(Extension(identity_state))
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_config() -> Arc<Config> {
        Arc::new(Config {
            http_port: 80,
            https_port: 443,
            host: "0.0.0.0".to_string(),
            log_level: "info".to_string(),
            trust_forward: false,
            body_limit: 1_048_576,
            instance_id: "test-instance-01".to_string(),
            tls_cert: "certs/server.crt".to_string(),
            tls_key: "certs/server.key".to_string(),
            mtls_in_header: None,
        })
    }

    fn test_state() -> Arc<IdentityState> {
        Arc::new(IdentityState {
            start_time: Instant::now(),
            request_count: AtomicU64::new(0),
        })
    }

    fn test_app() -> (Router, Arc<IdentityState>) {
        let state = test_state();
        let app = router(state.clone()).with_state(test_config());
        (app, state)
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
        assert_eq!(json["instance_id"], "test-instance-01");
        assert!(json["hostname"].is_string());
        assert!(json["version"].is_string());
        assert!(json["uptime_seconds"].is_number());
        assert_eq!(json["port"]["http"], 80);
        assert_eq!(json["port"]["https"], 443);
        assert!(json["environment"]["profile"].is_string());
        assert!(json["timestamp"].is_string());
    }

    #[tokio::test]
    async fn request_count_increments() {
        let state = test_state();
        let config = test_config();

        // First request
        let app1 = router(state.clone()).with_state(config.clone());
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
        let app2 = router(state.clone()).with_state(config);
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
