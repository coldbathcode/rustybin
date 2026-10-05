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

/// Test-only Ed25519 keys for control-plane JWTs (shared with the
/// integration tests).
pub mod control_keys {
    include!("../tests/common/control_jwt_keys.rs");
}

/// A config with `RUSTYBIN_CONTROL_AUTH=jwt`, the test public key, audience
/// `test-instance` and admin token `admin-secret`.
pub fn jwt_config() -> Config {
    let mut c = Config::for_tests();
    c.control_auth = crate::control_auth::ControlAuth::Jwt;
    c.control_jwt_key = crate::control_auth::parse_public_key(control_keys::CONTROL_JWT_PUBLIC_PEM);
    c.control_jwt_audience = "test-instance".to_string();
    c.admin_token = Some("admin-secret".to_string());
    c
}

/// Sign `claims` as an EdDSA JWT with the test key (or the other key).
pub fn sign_control_jwt(claims: &serde_json::Value, other_key: bool) -> String {
    let pem = if other_key {
        control_keys::CONTROL_JWT_OTHER_PRIVATE_PEM
    } else {
        control_keys::CONTROL_JWT_PRIVATE_PEM
    };
    let key = jsonwebtoken::EncodingKey::from_ed_pem(pem.as_bytes()).expect("test key");
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA),
        claims,
        &key,
    )
    .expect("sign")
}

/// A valid control JWT for `test-instance` with `scope` (None: no claim),
/// expiring in 10 minutes.
pub fn control_jwt(scope: Option<&str>) -> String {
    let mut claims = serde_json::json!({
        "aud": "test-instance",
        "sub": "user-1",
        "exp": chrono::Utc::now().timestamp() + 600,
    });
    if let Some(s) = scope {
        claims["scope"] = serde_json::json!(s);
    }
    sign_control_jwt(&claims, false)
}

/// `method uri` with `Authorization: Bearer token` (when given).
pub fn bearer_request(method: &str, uri: &str, token: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).expect("valid request")
}
