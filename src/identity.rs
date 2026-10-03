//! `/identity`: which instance answered, with its configuration and what it
//! saw of the request (load-balancing and proxy demos).

use axum::{
    extract::{Extension, State},
    http::request::Parts,
    response::Response,
    routing::any,
    Router,
};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::content_negotiation::negotiate;
use crate::session::{client_ip, request_origin, ListenerInfo};
use crate::state::AppState;

/// `rustc --version` of the compiler that built this binary (from build.rs).
pub const RUSTC_VERSION: &str = env!("RUSTYBIN_RUSTC_VERSION");

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
    config: ConfigInfo,
    environment: EnvInfo,
    request: RequestInfo,
    timestamp: String,
}

#[derive(Serialize)]
struct PortInfo {
    http: u16,
    https: u16,
    grpc: u16,
    /// The listener that accepted this request (`http` or `https`) and its
    /// bound port, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    listener: Option<ListenerView>,
}

#[derive(Serialize)]
struct ListenerView {
    scheme: &'static str,
    port: u16,
}

#[derive(Serialize)]
struct ConfigInfo {
    host: String,
    public_mode: bool,
    trust_forward: bool,
    body_limit: usize,
    request_timeout_secs: u64,
    max_delay_ms: u64,
    inspector_capacity: usize,
    cors_allow_origins: Vec<String>,
    admin_token_configured: bool,
}

#[derive(Serialize)]
struct EnvInfo {
    /// `rustc --version` used to build the binary.
    rust_version: &'static str,
    /// Minimum supported Rust version declared in Cargo.toml.
    msrv: &'static str,
    profile: &'static str,
    os: &'static str,
    arch: &'static str,
}

#[derive(Serialize)]
struct RequestInfo {
    method: String,
    /// TCP peer address.
    peer_ip: String,
    /// Client IP after applying trusted proxy headers.
    remote_ip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    forwarded_for: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    forwarded: Option<String>,
    host: String,
    scheme: String,
    url_base: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    via: Option<String>,
}

// ── Handler ─────────────────────────────────────────────────────────

fn header(parts: &Parts, name: &str) -> Option<String> {
    let values: Vec<&str> = parts
        .headers
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

async fn identity_handler(
    State(config): State<Arc<Config>>,
    Extension(state): Extension<Arc<IdentityState>>,
    crate::session::PeerAddr(peer): crate::session::PeerAddr,
    parts: Parts,
) -> Response {
    let count = state.request_count.fetch_add(1, Ordering::Relaxed) + 1;
    let uptime = state.start_time.elapsed().as_secs();
    let hostname = gethostname::gethostname().to_string_lossy().to_string();
    let origin = request_origin(&parts.headers, &parts.extensions, &parts.uri, &config);
    let listener = parts
        .extensions
        .get::<ListenerInfo>()
        .map(|l| ListenerView {
            scheme: l.scheme,
            port: l.port,
        });

    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    negotiate(
        &parts.headers,
        &IdentityResponse {
            instance_id: config.instance_id.clone(),
            hostname,
            version: env!("CARGO_PKG_VERSION"),
            uptime_seconds: uptime,
            request_count: count,
            port: PortInfo {
                http: config.http_port,
                https: config.https_port,
                grpc: config.grpc_port,
                listener,
            },
            config: ConfigInfo {
                host: config.host.to_string(),
                public_mode: config.public_mode,
                trust_forward: config.trust_forward,
                body_limit: config.body_limit,
                request_timeout_secs: config.request_timeout_secs,
                max_delay_ms: config.max_delay_ms(),
                inspector_capacity: config.inspector_capacity,
                cors_allow_origins: config.cors_allow_origins.clone(),
                admin_token_configured: config.admin_token.is_some(),
            },
            environment: EnvInfo {
                rust_version: RUSTC_VERSION,
                msrv: env!("CARGO_PKG_RUST_VERSION"),
                profile,
                os: std::env::consts::OS,
                arch: std::env::consts::ARCH,
            },
            request: RequestInfo {
                method: parts.method.to_string(),
                peer_ip: peer
                    .map(|addr| addr.ip().to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                remote_ip: client_ip(&parts.headers, &parts.extensions, &config)
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                forwarded_for: header(&parts, "x-forwarded-for"),
                forwarded: header(&parts, "forwarded"),
                host: header(&parts, "host").unwrap_or_else(|| origin.authority()),
                scheme: origin.scheme.clone(),
                url_base: origin.base_url(),
                via: header(&parts, "via"),
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
        "Instance identity: id, hostname, uptime, /identity request count, ports and config (load-balancing demos)",
    )
    .example(Example::get("Identity", "/identity"))]
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_json, get_request};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app() -> Router {
        crate::test_support::module_app(router)
    }

    #[tokio::test]
    async fn identity_returns_instance_info() {
        let resp = test_app()
            .oneshot(get_request("/identity"))
            .await
            .expect("response");
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["instance_id"], "test-instance");
        assert!(json["hostname"].is_string());
        assert!(json["version"].is_string());
        assert!(json["uptime_seconds"].is_number());
        assert_eq!(json["port"]["http"], 0);
        assert_eq!(json["port"]["https"], 0);
        assert_eq!(json["port"]["grpc"], 0);
        assert_eq!(json["config"]["public_mode"], false);
        assert!(json["config"].get("admin_token").is_none());
        assert!(json["environment"]["profile"].is_string());
        assert!(json["timestamp"].is_string());
    }

    #[tokio::test]
    async fn rust_version_is_the_compiler_version() {
        let json = body_json(
            test_app()
                .oneshot(get_request("/identity"))
                .await
                .expect("response"),
        )
        .await;
        let v = json["environment"]["rust_version"].as_str().unwrap_or("");
        assert!(v.starts_with("rustc "), "{v}");
        assert_eq!(json["environment"]["msrv"], env!("CARGO_PKG_RUST_VERSION"));
    }

    #[tokio::test]
    async fn request_count_increments() {
        let state = crate::test_support::test_state();
        let app = crate::test_support::module_app_with(state, router);
        let json1 = body_json(
            app.clone()
                .oneshot(get_request("/identity"))
                .await
                .expect("r"),
        )
        .await;
        assert_eq!(json1["request_count"], 1);
        let json2 = body_json(app.oneshot(get_request("/identity")).await.expect("r")).await;
        assert_eq!(json2["request_count"], 2);
    }

    #[tokio::test]
    async fn forwards_headers_included() {
        let resp = test_app()
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
        let json = body_json(resp).await;
        assert_eq!(json["request"]["forwarded_for"], "10.0.0.1");
        assert_eq!(json["request"]["host"], "api.example.com");
        assert_eq!(json["request"]["via"], "1.1 gateway");
        // Not trusted by default: remote_ip is the peer.
        assert_eq!(json["request"]["remote_ip"], "127.0.0.1");
    }

    #[tokio::test]
    async fn post_method_works() {
        let resp = test_app()
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
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/identity")
                    .header("accept", "application/xml")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.headers()["content-type"], "application/xml");
    }
}
