use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::{routing::get, routing::post, Router};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::config::Config;
use crate::content_negotiation::negotiate_with_status;

// ── State ───────────────────────────────────────────────────────────

/// Runtime-toggleable liveness state. Lets a running instance be flipped
/// to "unhealthy" (returning 503 from `/health`) so gateway upstream active
/// health checks and load-balancer failover can be demonstrated live.
pub struct HealthState {
    healthy: AtomicBool,
}

impl HealthState {
    pub fn new() -> Self {
        HealthState {
            healthy: AtomicBool::new(true),
        }
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }

    fn set(&self, healthy: bool) {
        self.healthy.store(healthy, Ordering::Relaxed);
    }
}

impl Default for HealthState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Response type ───────────────────────────────────────────────────

#[derive(Serialize, Clone, Debug)]
struct HealthResponse {
    status: String,
    service: String,
    version: String,
    instance_id: String,
}

// ── Handlers ────────────────────────────────────────────────────────

fn health_body(config: &Config, healthy: bool, headers: &HeaderMap) -> Response {
    let (status_text, code) = if healthy {
        ("healthy", StatusCode::OK)
    } else {
        ("unhealthy", StatusCode::SERVICE_UNAVAILABLE)
    };
    let resp = HealthResponse {
        status: status_text.to_string(),
        service: "rustybin".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        instance_id: config.instance_id.clone(),
    };
    negotiate_with_status(headers, &resp, code)
}

async fn health(
    State(config): State<Arc<Config>>,
    Extension(state): Extension<Arc<HealthState>>,
    headers: HeaderMap,
) -> Response {
    health_body(&config, state.is_healthy(), &headers)
}

async fn set_unhealthy(
    State(config): State<Arc<Config>>,
    Extension(state): Extension<Arc<HealthState>>,
    headers: HeaderMap,
) -> Response {
    state.set(false);
    health_body(&config, false, &headers)
}

async fn set_healthy(
    State(config): State<Arc<Config>>,
    Extension(state): Extension<Arc<HealthState>>,
    headers: HeaderMap,
) -> Response {
    state.set(true);
    health_body(&config, true, &headers)
}

async fn toggle(
    State(config): State<Arc<Config>>,
    Extension(state): Extension<Arc<HealthState>>,
    headers: HeaderMap,
) -> Response {
    let now_healthy = !state.is_healthy();
    state.set(now_healthy);
    health_body(&config, now_healthy, &headers)
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(state: Arc<HealthState>) -> Router<Arc<Config>> {
    Router::new()
        .route("/health", get(health))
        .route("/health/healthy", post(set_healthy))
        .route("/health/unhealthy", post(set_unhealthy))
        .route("/health/toggle", post(toggle))
        .layer(Extension(state))
}

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
            instance_id: "test-instance".to_string(),
            tls_cert: "certs/server.crt".to_string(),
            tls_key: "certs/server.key".to_string(),
            mtls_in_header: None,
        })
    }

    fn test_app() -> Router {
        router(Arc::new(HealthState::new())).with_state(test_config())
    }

    #[tokio::test]
    async fn health_returns_json() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["status"], "healthy");
        assert_eq!(json["service"], "rustybin");
        assert_eq!(json["instance_id"], "test-instance");
        assert!(json["version"].is_string());
    }

    #[tokio::test]
    async fn health_returns_xml_when_requested() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header("Accept", "application/xml")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        let content_type = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(content_type, "application/xml");
    }

    #[tokio::test]
    async fn unhealthy_then_healthy_toggle() {
        // Shared state across requests, so build the app once.
        let state = Arc::new(HealthState::new());
        let make = || router(state.clone()).with_state(test_config());

        // Flip to unhealthy → 503
        let resp = make()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/health/unhealthy")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        // GET now reflects unhealthy
        let resp = make()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "unhealthy");

        // Recover → 200
        let resp = make()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/health/healthy")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn toggle_flips_state() {
        let state = Arc::new(HealthState::new());
        let resp = router(state.clone())
            .with_state(test_config())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/health/toggle")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Started healthy, toggled → unhealthy
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
