//! Shared helpers for unit tests (`#[cfg(test)]` only).
//!
//! ```ignore
//! use crate::test_support::{body_json, get_request, module_app};
//! let app = module_app(super::router);           // one module, no middleware
//! let app = crate::test_support::test_app();     // full app with middleware
//! let resp = app.oneshot(get_request("/echo")).await.unwrap();
//! let json = body_json(resp).await;
//! ```

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, Response};
use axum::Router;
use std::net::SocketAddr;

use crate::config::Config;
use crate::state::AppState;

/// Peer address injected as `ConnectInfo` in tests.
pub const TEST_PEER: &str = "127.0.0.1:40000";

fn peer() -> SocketAddr {
    TEST_PEER
        .parse()
        .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], 40000)))
}

/// `Config::for_tests()`.
pub fn test_config() -> Config {
    Config::for_tests()
}

/// State with test config and shared (once per process) keys.
pub fn test_state() -> AppState {
    AppState::for_tests(Config::for_tests())
}

/// State with a custom config and shared keys.
pub fn test_state_with(config: Config) -> AppState {
    AppState::for_tests(config)
}

/// The full application (all routes + middleware) with a mock peer address.
pub fn test_app() -> Router {
    test_app_with(test_state())
}

/// The full application for a given state.
pub fn test_app_with(state: AppState) -> Router {
    crate::build_app(state).layer(MockConnectInfo(peer()))
}

/// A single module's router (no middleware) with test state.
pub fn module_app(router: fn(&AppState) -> Router<AppState>) -> Router {
    module_app_with(test_state(), router)
}

/// A single module's router for a given state.
pub fn module_app_with(state: AppState, router: fn(&AppState) -> Router<AppState>) -> Router {
    router(&state)
        .with_state(state)
        .layer(MockConnectInfo(peer()))
}

/// A single module's router with a custom config.
pub fn module_app_with_config(config: Config, router: fn(&AppState) -> Router<AppState>) -> Router {
    module_app_with(test_state_with(config), router)
}

/// `GET uri` with an empty body.
pub fn get_request(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("valid request")
}

/// `method uri` with a JSON body.
pub fn json_request(method: &str, uri: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("valid request")
}

pub async fn body_bytes(resp: Response<Body>) -> bytes::Bytes {
    axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body")
}

pub async fn body_string(resp: Response<Body>) -> String {
    String::from_utf8(body_bytes(resp).await.to_vec()).expect("utf8 body")
}

pub async fn body_json(resp: Response<Body>) -> serde_json::Value {
    serde_json::from_slice(&body_bytes(resp).await).expect("json body")
}
