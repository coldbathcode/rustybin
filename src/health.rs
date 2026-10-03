use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::{routing::get, routing::post, Router};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::content_negotiation::negotiate_with_status;
use crate::state::AppState;

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
    if let Err(resp) = crate::admin::require_admin(&headers, &config) {
        return resp;
    }
    state.set(false);
    health_body(&config, false, &headers)
}

async fn set_healthy(
    State(config): State<Arc<Config>>,
    Extension(state): Extension<Arc<HealthState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &config) {
        return resp;
    }
    state.set(true);
    health_body(&config, true, &headers)
}

async fn toggle(
    State(config): State<Arc<Config>>,
    Extension(state): Extension<Arc<HealthState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &config) {
        return resp;
    }
    let now_healthy = !state.is_healthy();
    state.set(now_healthy);
    health_body(&config, now_healthy, &headers)
}

// ── Router ──────────────────────────────────────────────────────────

/// Health toggles are instance-global (they drive gateway health checks), so
/// the mutations are admin-guarded.
pub fn router(_state: &AppState) -> Router<AppState> {
    routes(Arc::new(HealthState::new()))
}

fn routes(state: Arc<HealthState>) -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/health/healthy", post(set_healthy))
        .route("/health/unhealthy", post(set_unhealthy))
        .route("/health/toggle", post(toggle))
        .layer(Extension(state))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/health",
            &["GET"],
            category::HEALTH,
            "Health check (200, or 503 when toggled unhealthy)",
        )
        .example(Example::get("Health check", "/health")),
        Endpoint::new(
            "/health/healthy",
            &["POST"],
            category::HEALTH,
            "Mark the instance healthy (admin-guarded)",
        )
        .example(Example::post("Mark healthy", "/health/healthy")),
        Endpoint::new(
            "/health/unhealthy",
            &["POST"],
            category::HEALTH,
            "Mark the instance unhealthy, /health returns 503 (admin-guarded)",
        )
        .description("For gateway active health-check and failover demos. Global to the instance."),
        Endpoint::new(
            "/health/toggle",
            &["POST"],
            category::HEALTH,
            "Flip the health state (admin-guarded)",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app() -> Router {
        crate::test_support::module_app(router)
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
        let app = test_app();
        let make = || app.clone();

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
        let resp = test_app()
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

    #[tokio::test]
    async fn mutations_require_admin_token_when_configured() {
        let mut config = crate::test_support::test_config();
        config.admin_token = Some("tok".to_string());
        let app = crate::test_support::module_app_with_config(config, router);
        let post = |auth: Option<&str>| {
            let mut b = Request::builder().method("POST").uri("/health/unhealthy");
            if let Some(a) = auth {
                b = b.header("Authorization", a);
            }
            b.body(Body::empty()).unwrap()
        };
        let resp = app.clone().oneshot(post(None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = app.clone().oneshot(post(Some("Bearer tok"))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
