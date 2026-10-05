//! Control-plane endpoints under `/_rustybin/*` (config, version, ready).
//! The inspector endpoints live in `crate::inspector`, metrics in
//! `crate::metrics`, authentication in `crate::control_auth`.

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

async fn version_handler(State(config): State<Arc<Config>>) -> impl IntoResponse {
    let mut v = version_info();
    v["control_auth"] = json!(config.control_auth.as_str());
    v["hosted_mode"] = json!(config.hosted_mode);
    v["grpc_on_http"] = json!(config.grpc_on_http);
    Json(v)
}

/// Readiness: 200 while the process serves HTTP, whatever the `/health`
/// demo toggle says. Never authenticated, limited or captured.
async fn ready_handler() -> impl IntoResponse {
    Json(json!({ "status": "ready" }))
}

/// Commit the binary was built from (`RUSTYBIN_GIT_SHA` at build time, else
/// the checkout's HEAD, see build.rs), when known.
pub fn git_sha() -> Option<&'static str> {
    option_env!("RUSTYBIN_GIT_SHA").filter(|s| !s.is_empty())
}

/// Build information (also embedded in `/_rustybin/usage`).
pub fn build_info() -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": git_sha(),
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "rustc": env!("RUSTYBIN_RUSTC_VERSION"),
    })
}

/// Build/version information.
pub fn version_info() -> Value {
    json!({
        "name": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": git_sha(),
        "rust_version": env!("CARGO_PKG_RUST_VERSION"),
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
    })
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/_rustybin/config", get(config_handler))
        .route("/_rustybin/version", get(version_handler))
        .route("/_rustybin/ready", get(ready_handler))
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
            "Service name, version, commit and control-plane auth mode",
        )
        .example(Example::get("Version", "/_rustybin/version")),
        Endpoint::new(
            "/_rustybin/ready",
            &["GET"],
            category::CONTROL,
            "Readiness probe: 200 while serving, independent of the /health toggle",
        )
        .description(
            "Never authenticated (RUSTYBIN_CONTROL_AUTH), never limited by the plan, never \
             captured by the inspector. Use it for platform readiness checks; /health is a demo \
             toggle that may return 503.",
        )
        .example(Example::get("Readiness", "/_rustybin/ready")),
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
                        "git_sha": { "type": ["string", "null"], "description": "Commit the binary was built from, when known" },
                        "rust_version": { "type": "string" },
                        "profile": { "type": "string" },
                        "control_auth": { "type": "string", "enum": ["open", "token", "jwt"] },
                        "hosted_mode": { "type": "boolean" },
                        "grpc_on_http": { "type": "boolean" }
                    }
                } } } } }
            }
        },
        "/_rustybin/ready": {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Readiness probe",
                "description": "Always 200 while the process serves HTTP, independent of the `/health` demo toggle. Exempt from control-plane auth, plan limits and inspector capture.",
                "operationId": "getRustybinReady",
                "security": [],
                "responses": { "200": { "description": "Ready", "content": { "application/json": { "schema": {
                    "type": "object",
                    "properties": { "status": { "type": "string", "enum": ["ready"] } }
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
