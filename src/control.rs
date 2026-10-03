//! Control-plane endpoints under `/_rustybin/*` (config and version).
//! The inspector endpoints live in `crate::inspector`.

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::state::AppState;

/// Prefix of every control-plane route.
pub const CONTROL_PREFIX: &str = "/_rustybin";
/// Prefix of the web console.
pub const UI_PREFIX: &str = "/ui";

/// True for `/_rustybin`, `/_rustybin/*`, `/ui` and `/ui/*`. These paths are
/// never captured by the inspector and ignore fault-injection headers.
pub fn is_control_path(path: &str) -> bool {
    fn under(path: &str, prefix: &str) -> bool {
        path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    }
    under(path, CONTROL_PREFIX) || under(path, UI_PREFIX)
}

async fn config_handler(State(config): State<Arc<Config>>) -> impl IntoResponse {
    Json(config.public_view())
}

async fn version_handler() -> impl IntoResponse {
    Json(version_info())
}

/// Build/version information.
pub fn version_info() -> Value {
    json!({
        "name": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "rust_version": env!("CARGO_PKG_RUST_VERSION"),
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
    })
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/_rustybin/config", get(config_handler))
        .route("/_rustybin/version", get(version_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/_rustybin/config",
            &["GET"],
            category::CONTROL,
            "Effective configuration (no secrets)",
        )
        .example(Example::get("Effective config", "/_rustybin/config")),
        Endpoint::new(
            "/_rustybin/version",
            &["GET"],
            category::CONTROL,
            "Service name and version",
        )
        .example(Example::get("Version", "/_rustybin/version")),
    ]
}

pub fn openapi_paths() -> Value {
    json!({
        "/_rustybin/config": {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Effective configuration",
                "description": "The running configuration without secrets (the admin token is reported only as `admin_token_configured`).",
                "operationId": "getRustybinConfig",
                "responses": { "200": { "description": "Configuration", "content": { "application/json": { "schema": { "type": "object" } } } } }
            }
        },
        "/_rustybin/version": {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Version information",
                "operationId": "getRustybinVersion",
                "responses": { "200": { "description": "Version", "content": { "application/json": { "schema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "version": { "type": "string" },
                        "rust_version": { "type": "string" },
                        "profile": { "type": "string" }
                    }
                } } } } }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_json, get_request, test_app};
    use axum::http::StatusCode;
    use tower::ServiceExt;

    #[test]
    fn control_paths() {
        assert!(is_control_path("/_rustybin"));
        assert!(is_control_path("/_rustybin/requests"));
        assert!(is_control_path("/ui"));
        assert!(is_control_path("/ui/index.html"));
        assert!(!is_control_path("/uix"));
        assert!(!is_control_path("/echo"));
    }

    #[tokio::test]
    async fn config_and_version() {
        let resp = test_app()
            .oneshot(get_request("/_rustybin/config"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["instance_id"], "test-instance");
        assert!(v.get("admin_token").is_none());

        let resp = test_app()
            .oneshot(get_request("/_rustybin/version"))
            .await
            .expect("response");
        assert_eq!(body_json(resp).await["version"], env!("CARGO_PKG_VERSION"));
    }
}
