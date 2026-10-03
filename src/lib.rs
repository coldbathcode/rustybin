//! Rustybin: an HTTP stub service for API and AI gateway demos.
//!
//! Architecture:
//! - [`build_app`] merges every module's `router(&AppState)` (see [`ROUTERS`])
//!   and wraps them in the shared middleware stack.
//! - [`run`] / [`start`] bind the HTTP, HTTPS and gRPC listeners.
//! - [`catalog`] is the single source of truth for routes (landing page,
//!   exports, README); [`openapi`] merges per-module OpenAPI fragments.

pub mod admin;
pub mod ai_anthropic;
pub mod ai_gateway;
pub mod auth_apikey;
pub mod auth_basic;
pub mod auth_hmac;
pub mod auth_jwt;
pub mod auth_mtls;
pub mod catalog;
pub mod cert_state;
pub mod collections;
pub mod config;
pub mod content_negotiation;
pub mod control;
pub mod cookies;
pub mod echo;
pub mod fault;
pub mod flaky;
pub mod graphql;
pub mod grpc;
pub mod health;
pub mod identity;
pub mod image;
pub mod info;
pub mod inspector;
pub mod jwt_state;
pub mod landing;
pub mod logging;
pub mod oidc;
pub mod openapi;
pub mod orchestration;
pub mod random;
pub mod redirects;
pub mod response_shaping;
pub mod server;
pub mod session;
pub mod soap;
pub mod state;
pub mod status;
pub mod types;
pub mod websocket;

#[cfg(test)]
pub mod test_support;

use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::http::{header, HeaderName, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{middleware, Json, Router};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer, ExposeHeaders};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultOnFailure, DefaultOnResponse, TraceLayer};
use tower_http::LatencyUnit;

pub use config::Config;
pub use server::{run, start, start_with_state, Error, RunningServer};
pub use state::AppState;

/// Every module's router. Register new modules here (once), plus in
/// `catalog::all()` and (for OpenAPI) in `openapi.rs`.
pub const ROUTERS: &[fn(&AppState) -> Router<AppState>] = &[
    landing::router,
    health::router,
    echo::router,
    status::router,
    response_shaping::router,
    redirects::router,
    cookies::router,
    info::router,
    random::router,
    image::router,
    auth_basic::router,
    auth_apikey::router,
    auth_hmac::router,
    auth_jwt::router,
    oidc::router,
    auth_mtls::router,
    ai_gateway::router,
    ai_anthropic::router,
    graphql::router,
    orchestration::router,
    soap::router,
    websocket::router,
    identity::router,
    flaky::router,
    collections::router,
    openapi::router,
    inspector::router,
    control::router,
];

/// Build the complete application: all module routers plus the middleware
/// stack (outermost first): request id, tracing, request id propagation,
/// CORS, time-to-headers timeout, inspector capture, body limit, fault injection.
pub fn build_app(state: AppState) -> Router {
    let config = state.config.clone();

    let mut router = Router::new();
    for make in ROUTERS {
        router = router.merge(make(&state));
    }

    let mut router = router
        .fallback(not_found)
        // innermost: per-route fault injection
        .layer(middleware::from_fn_with_state(
            config.clone(),
            fault::inject,
        ))
        .layer(DefaultBodyLimit::max(config.body_limit))
        .layer(RequestBodyLimitLayer::new(config.body_limit))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            inspector::capture,
        ));

    // Time-to-headers timeout: tower-http's TimeoutLayer bounds the time until
    // the response future resolves, never the streaming body, so SSE and
    // WebSocket connections are not cut off.
    if config.request_timeout_secs > 0 {
        router = router.layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            Duration::from_secs(config.request_timeout_secs),
        ));
    }

    if let Some(cors) = cors_layer(&config.cors_allow_origins) {
        router = router.layer(cors);
    }

    router
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|req: &axum::http::Request<axum::body::Body>| {
                    let request_id = req
                        .headers()
                        .get("x-request-id")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("-");
                    tracing::info_span!(
                        "request",
                        method = %req.method(),
                        path = %req.uri().path(),
                        request_id = %request_id,
                    )
                })
                .on_request(())
                .on_response(
                    DefaultOnResponse::new()
                        .level(tracing::Level::INFO)
                        .latency_unit(LatencyUnit::Millis),
                )
                // 5xx responses are normal for a stub service (/status/503,
                // /flaky, fault injection): do not log them as errors.
                .on_failure(
                    DefaultOnFailure::new()
                        .level(tracing::Level::DEBUG)
                        .latency_unit(LatencyUnit::Millis),
                ),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state)
}

/// CORS for browser clients (the UI, docs, demos). `None` disables the layer
/// (`RUSTYBIN_CORS_ORIGINS=off`) so a gateway's own CORS handling can be shown.
/// Preflights are answered here only when they carry
/// `Access-Control-Request-Method`; a plain `OPTIONS` still reaches the route.
fn cors_layer(origins: &[String]) -> Option<CorsLayer> {
    if origins.is_empty() {
        return None;
    }
    let allow_origin = if origins.iter().any(|o| o == "*") {
        AllowOrigin::any()
    } else {
        let list: Vec<_> = origins.iter().filter_map(|o| o.parse().ok()).collect();
        AllowOrigin::list(list)
    };
    Some(
        CorsLayer::new()
            .allow_origin(allow_origin)
            .allow_methods(AllowMethods::mirror_request())
            .allow_headers(AllowHeaders::mirror_request())
            .expose_headers(ExposeHeaders::list([
                HeaderName::from_static("x-request-id"),
                HeaderName::from_static("x-rustybin-fault"),
                header::CONTENT_LENGTH,
            ]))
            .max_age(Duration::from_secs(600)),
    )
}

async fn not_found(method: Method, uri: Uri) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": "not found",
            "method": method.as_str(),
            "path": uri.path(),
            "hint": "GET / lists every endpoint; /openapi.json has the full spec",
        })),
    )
        .into_response()
}
