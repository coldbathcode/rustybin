//! `/health` plus the admin-guarded toggles.
//!
//! The state is instance-global ([`crate::state::AppState::health`]) and is
//! shared with the gRPC `grpc.health.v1.Health` service, which follows it
//! through [`HealthState::subscribe`]. Mutations answer 200 with the new state
//! (they succeeded); `GET /health` then returns 503 while unhealthy.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::{routing::get, routing::post, Router};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::watch;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::content_negotiation::negotiate_with_status;
use crate::state::AppState;

// ── State ───────────────────────────────────────────────────────────

/// Runtime-toggleable liveness state. Lets a running instance be flipped
/// to "unhealthy" (503 from `/health`, NOT_SERVING from gRPC health) so
/// gateway active health checks and load-balancer failover can be shown live.
pub struct HealthState {
    healthy: AtomicBool,
    tx: watch::Sender<bool>,
}

impl HealthState {
    pub fn new() -> Self {
        let (tx, _) = watch::channel(true);
        HealthState {
            healthy: AtomicBool::new(true),
            tx,
        }
    }

    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::SeqCst)
    }

    /// Set the state; returns the new value.
    pub fn set(&self, healthy: bool) -> bool {
        // send_modify runs under the channel lock, so the atomic update and
        // the notification are serialised across concurrent callers.
        self.tx.send_modify(|v| {
            self.healthy.store(healthy, Ordering::SeqCst);
            *v = healthy;
        });
        healthy
    }

    /// Flip the state atomically; returns the new value.
    pub fn toggle(&self) -> bool {
        let mut now = true;
        self.tx.send_modify(|v| {
            now = !self.healthy.fetch_xor(true, Ordering::SeqCst);
            *v = now;
        });
        now
    }

    /// Follow state changes (used by the gRPC health service).
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.tx.subscribe()
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
    status: &'static str,
    healthy: bool,
    service: &'static str,
    version: &'static str,
    instance_id: String,
}

fn health_body(config: &Config, healthy: bool, headers: &HeaderMap, code: StatusCode) -> Response {
    let resp = HealthResponse {
        status: if healthy { "healthy" } else { "unhealthy" },
        healthy,
        service: "rustybin",
        version: env!("CARGO_PKG_VERSION"),
        instance_id: config.instance_id.clone(),
    };
    negotiate_with_status(headers, &resp, code)
}

// ── Handlers ────────────────────────────────────────────────────────

async fn health(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let healthy = state.health.is_healthy();
    let code = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    health_body(&state.config, healthy, &headers, code)
}

async fn set_unhealthy(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let now = state.health.set(false);
    health_body(&state.config, now, &headers, StatusCode::OK)
}

async fn set_healthy(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let now = state.health.set(true);
    health_body(&state.config, now, &headers, StatusCode::OK)
}

async fn toggle(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let now = state.health.toggle();
    health_body(&state.config, now, &headers, StatusCode::OK)
}

// ── Router ──────────────────────────────────────────────────────────

/// Health toggles are instance-global (they drive gateway health checks), so
/// the mutations are admin-guarded.
pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/health/healthy", post(set_healthy))
        .route("/health/unhealthy", post(set_unhealthy))
        .route("/health/toggle", post(toggle))
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
            "Mark the instance unhealthy: 200 here, then /health returns 503 (admin-guarded)",
        )
        .description(
            "For gateway active health-check and failover demos. Global to the instance; \
             the gRPC grpc.health.v1 service reports NOT_SERVING too.",
        ),
        Endpoint::new(
            "/health/toggle",
            &["POST"],
            category::HEALTH,
            "Flip the health state, 200 with the new state (admin-guarded)",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_json, get_request};
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn post(uri: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .body(Body::empty())
            .expect("request")
    }

    #[tokio::test]
    async fn health_returns_json() {
        let resp = crate::test_support::module_app(router)
            .oneshot(get_request("/health"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["status"], "healthy");
        assert_eq!(json["service"], "rustybin");
        assert_eq!(json["instance_id"], "test-instance");
        assert!(json["version"].is_string());
    }

    #[tokio::test]
    async fn health_returns_xml_when_requested() {
        let resp = crate::test_support::module_app(router)
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header("Accept", "application/xml")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "application/xml");
    }

    #[tokio::test]
    async fn unhealthy_then_healthy_toggle() {
        let state = crate::test_support::test_state();
        let mut rx = state.health.subscribe();
        let app = crate::test_support::module_app_with(state, router);

        // The mutation succeeds (200) and reports the new state.
        let resp = app
            .clone()
            .oneshot(post("/health/unhealthy"))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["status"], "unhealthy");
        assert!(!*rx.borrow_and_update());

        let resp = app
            .clone()
            .oneshot(get_request("/health"))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(resp).await["status"], "unhealthy");

        let resp = app
            .clone()
            .oneshot(post("/health/healthy"))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(*rx.borrow_and_update());
        let resp = app.oneshot(get_request("/health")).await.expect("r");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn toggle_flips_state() {
        let app = crate::test_support::module_app(router);
        let resp = app
            .clone()
            .oneshot(post("/health/toggle"))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["healthy"], false);
        let resp = app
            .clone()
            .oneshot(post("/health/toggle"))
            .await
            .expect("r");
        assert_eq!(body_json(resp).await["healthy"], true);
    }

    #[test]
    fn concurrent_toggles_stay_consistent() {
        let state = Arc::new(HealthState::new());
        let rx = state.subscribe();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let s = state.clone();
                std::thread::spawn(move || {
                    for _ in 0..1000 {
                        s.toggle();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread");
        }
        // 8000 flips: back to healthy, and the channel agrees with the atomic.
        assert!(state.is_healthy());
        assert_eq!(*rx.borrow(), state.is_healthy());
    }

    #[tokio::test]
    async fn mutations_require_admin_token_when_configured() {
        let mut config = crate::test_support::test_config();
        config.admin_token = Some("tok".to_string());
        let app = crate::test_support::module_app_with_config(config, router);
        let resp = app
            .clone()
            .oneshot(post("/health/unhealthy"))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let req = Request::builder()
            .method("POST")
            .uri("/health/unhealthy")
            .header("Authorization", "Bearer tok")
            .body(Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("r");
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
