use axum::{
    extract::{Extension, Path, State},
    http::{header::HeaderValue, HeaderMap, StatusCode},
    response::Response,
    routing::{any, get, post},
    Router,
};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::state::AppState;
use crate::types::ErrorResponse;

// ── State ───────────────────────────────────────────────────────────

pub struct FlakyState {
    pub pattern_counter: AtomicU64,
    pub after_counter: AtomicU64,
    pub recover_counter: AtomicU64,
    pub random_counter: AtomicU64,
}

impl FlakyState {
    pub fn new() -> Self {
        Self {
            pattern_counter: AtomicU64::new(0),
            after_counter: AtomicU64::new(0),
            recover_counter: AtomicU64::new(0),
            random_counter: AtomicU64::new(0),
        }
    }
}

impl Default for FlakyState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Response types ──────────────────────────────────────────────────

#[derive(Serialize)]
struct FlakySuccess {
    status: &'static str,
    fail_rate: u8,
    message: String,
    request_number: u64,
}

#[derive(Serialize)]
struct FlakyFailure {
    error: &'static str,
    fail_rate: u8,
    message: String,
}

#[derive(Serialize)]
struct PatternSuccess {
    status: &'static str,
    pattern: String,
    position: u64,
    current: &'static str,
    request_number: u64,
}

#[derive(Serialize)]
struct PatternFailure {
    error: &'static str,
    pattern: String,
    position: u64,
    current: &'static str,
    request_number: u64,
}

#[derive(Serialize)]
struct AfterSuccess {
    status: &'static str,
    request_number: u64,
    threshold: u64,
    will_fail_after: u64,
}

#[derive(Serialize)]
struct AfterFailure {
    error: &'static str,
    request_number: u64,
    threshold: u64,
    will_fail_after: u64,
}

#[derive(Serialize)]
struct RecoverSuccess {
    status: &'static str,
    request_number: u64,
    threshold: u64,
    will_recover_after: u64,
}

#[derive(Serialize)]
struct RecoverFailure {
    error: &'static str,
    request_number: u64,
    threshold: u64,
    will_recover_after: u64,
}

#[derive(Serialize)]
struct ResetResponse {
    status: &'static str,
    message: &'static str,
}

#[derive(Serialize)]
struct StatusResponse {
    pattern_counter: u64,
    after_counter: u64,
    recover_counter: u64,
    random_counter: u64,
}

// ── Helpers ─────────────────────────────────────────────────────────

fn flaky_headers(fail_rate: u8) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-rustybin-flaky", HeaderValue::from_static("true"));
    if let Ok(v) = HeaderValue::from_str(&fail_rate.to_string()) {
        headers.insert("x-rustybin-fail-rate", v);
    }
    headers
}

fn add_headers(mut resp: Response, extra: HeaderMap) -> Response {
    let headers = resp.headers_mut();
    for (k, v) in extra.iter() {
        headers.insert(k.clone(), v.clone());
    }
    resp
}

// ── Handlers ────────────────────────────────────────────────────────

async fn flaky_rate_handler(
    Path(fail_rate): Path<u8>,
    Extension(state): Extension<Arc<FlakyState>>,
    headers: HeaderMap,
) -> Response {
    if fail_rate > 100 {
        return negotiate_with_status(
            &headers,
            &ErrorResponse {
                error: "bad_request".to_string(),
                details: Some("fail_rate must be 0-100".to_string()),
            },
            StatusCode::BAD_REQUEST,
        );
    }

    let count = state.random_counter.fetch_add(1, Ordering::Relaxed) + 1;
    let roll = rand::random::<u8>() % 100;
    let extra = flaky_headers(fail_rate);

    if roll < fail_rate {
        let mut resp = negotiate_with_status(
            &headers,
            &FlakyFailure {
                error: "service_unavailable",
                fail_rate,
                message: format!("Simulated failure ({}% fail rate)", fail_rate),
            },
            StatusCode::SERVICE_UNAVAILABLE,
        );
        resp.headers_mut()
            .insert("retry-after", HeaderValue::from_static("1"));
        add_headers(resp, extra)
    } else {
        let resp = negotiate(
            &headers,
            &FlakySuccess {
                status: "ok",
                fail_rate,
                message: "Request succeeded".to_string(),
                request_number: count,
            },
        );
        add_headers(resp, extra)
    }
}

async fn flaky_pattern_handler(
    Path(pattern): Path<String>,
    Extension(state): Extension<Arc<FlakyState>>,
    headers: HeaderMap,
) -> Response {
    let upper = pattern.to_uppercase();
    if upper.is_empty() || !upper.chars().all(|c| c == 'S' || c == 'F') {
        return negotiate_with_status(
            &headers,
            &ErrorResponse {
                error: "bad_request".to_string(),
                details: Some(
                    "Pattern must contain only S (success) and F (fail) characters".to_string(),
                ),
            },
            StatusCode::BAD_REQUEST,
        );
    }

    let count = state.pattern_counter.fetch_add(1, Ordering::Relaxed);
    let pos = count % upper.len() as u64;
    let ch = upper.as_bytes()[pos as usize];

    if ch == b'F' {
        let mut resp = negotiate_with_status(
            &headers,
            &PatternFailure {
                error: "service_unavailable",
                pattern: upper,
                position: pos,
                current: "F",
                request_number: count + 1,
            },
            StatusCode::SERVICE_UNAVAILABLE,
        );
        resp.headers_mut()
            .insert("retry-after", HeaderValue::from_static("1"));
        resp
    } else {
        negotiate(
            &headers,
            &PatternSuccess {
                status: "ok",
                pattern: upper,
                position: pos,
                current: "S",
                request_number: count + 1,
            },
        )
    }
}

async fn flaky_after_handler(
    Path(n): Path<u64>,
    Extension(state): Extension<Arc<FlakyState>>,
    headers: HeaderMap,
) -> Response {
    let count = state.after_counter.fetch_add(1, Ordering::Relaxed) + 1;

    if count <= n {
        negotiate(
            &headers,
            &AfterSuccess {
                status: "ok",
                request_number: count,
                threshold: n,
                will_fail_after: n,
            },
        )
    } else {
        let mut resp = negotiate_with_status(
            &headers,
            &AfterFailure {
                error: "service_unavailable",
                request_number: count,
                threshold: n,
                will_fail_after: n,
            },
            StatusCode::SERVICE_UNAVAILABLE,
        );
        resp.headers_mut()
            .insert("retry-after", HeaderValue::from_static("1"));
        resp
    }
}

async fn flaky_recover_handler(
    Path(n): Path<u64>,
    Extension(state): Extension<Arc<FlakyState>>,
    headers: HeaderMap,
) -> Response {
    let count = state.recover_counter.fetch_add(1, Ordering::Relaxed) + 1;

    if count <= n {
        let mut resp = negotiate_with_status(
            &headers,
            &RecoverFailure {
                error: "service_unavailable",
                request_number: count,
                threshold: n,
                will_recover_after: n,
            },
            StatusCode::SERVICE_UNAVAILABLE,
        );
        resp.headers_mut()
            .insert("retry-after", HeaderValue::from_static("1"));
        resp
    } else {
        negotiate(
            &headers,
            &RecoverSuccess {
                status: "ok",
                request_number: count,
                threshold: n,
                will_recover_after: n,
            },
        )
    }
}

async fn flaky_reset_handler(
    State(config): State<Arc<Config>>,
    Extension(state): Extension<Arc<FlakyState>>,
    headers: HeaderMap,
) -> Response {
    // Resetting the global counters affects every client: admin-guarded.
    if let Err(resp) = crate::admin::require_admin(&headers, &config) {
        return resp;
    }
    state.pattern_counter.store(0, Ordering::Relaxed);
    state.after_counter.store(0, Ordering::Relaxed);
    state.recover_counter.store(0, Ordering::Relaxed);
    state.random_counter.store(0, Ordering::Relaxed);

    negotiate(
        &headers,
        &ResetResponse {
            status: "reset",
            message: "All flaky counters reset",
        },
    )
}

async fn flaky_status_handler(
    Extension(state): Extension<Arc<FlakyState>>,
    headers: HeaderMap,
) -> Response {
    negotiate(
        &headers,
        &StatusResponse {
            pattern_counter: state.pattern_counter.load(Ordering::Relaxed),
            after_counter: state.after_counter.load(Ordering::Relaxed),
            recover_counter: state.recover_counter.load(Ordering::Relaxed),
            random_counter: state.random_counter.load(Ordering::Relaxed),
        },
    )
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    routes(Arc::new(FlakyState::new()))
}

fn routes(flaky_state: Arc<FlakyState>) -> Router<AppState> {
    Router::new()
        .route("/flaky/pattern/{pattern}", any(flaky_pattern_handler))
        .route("/flaky/after/{n}", any(flaky_after_handler))
        .route("/flaky/recover/{n}", any(flaky_recover_handler))
        .route("/flaky/reset", post(flaky_reset_handler))
        .route("/flaky/status", get(flaky_status_handler))
        .route("/flaky/{fail_rate}", any(flaky_rate_handler))
        .layer(Extension(flaky_state))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/flaky/{fail_rate}",
            &["ANY"],
            category::RELIABILITY,
            "Fail with 503 for fail_rate percent of requests",
        )
        .example(Example::get("Flaky 50%", "/flaky/50")),
        Endpoint::new(
            "/flaky/pattern/{pattern}",
            &["ANY"],
            category::RELIABILITY,
            "Deterministic success/failure pattern (S = success, F = failure)",
        )
        .example(Example::get("Flaky pattern SSFSS", "/flaky/pattern/SSFSS")),
        Endpoint::new(
            "/flaky/after/{n}",
            &["ANY"],
            category::RELIABILITY,
            "Succeed n times, then fail (circuit breaker trip)",
        )
        .example(Example::get("Fail after 3", "/flaky/after/3")),
        Endpoint::new(
            "/flaky/recover/{n}",
            &["ANY"],
            category::RELIABILITY,
            "Fail n times, then recover (circuit breaker half-open)",
        )
        .example(Example::get("Recover after 3", "/flaky/recover/3")),
        Endpoint::new(
            "/flaky/reset",
            &["POST"],
            category::RELIABILITY,
            "Reset all flaky counters (admin-guarded)",
        )
        .example(Example::post("Reset counters", "/flaky/reset")),
        Endpoint::new(
            "/flaky/status",
            &["GET"],
            category::RELIABILITY,
            "Current flaky counters",
        )
        .example(Example::get("Counter status", "/flaky/status")),
    ]
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_state() -> Arc<FlakyState> {
        Arc::new(FlakyState::new())
    }

    fn test_app() -> (Router, Arc<FlakyState>) {
        let state = test_state();
        let app = routes(state.clone()).with_state(crate::test_support::test_state());
        (app, state)
    }

    async fn json_body(resp: axum::http::Response<Body>) -> serde_json::Value {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&body).expect("json")
    }

    #[tokio::test]
    async fn rate_0_always_succeeds() {
        let state = test_state();
        let config = crate::test_support::test_state();
        for _ in 0..10 {
            let app = routes(state.clone()).with_state(config.clone());
            let resp = app
                .oneshot(
                    Request::builder()
                        .uri("/flaky/0")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(
                resp.headers()
                    .get("x-rustybin-flaky")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "true"
            );
        }
    }

    #[tokio::test]
    async fn rate_100_always_fails() {
        let state = test_state();
        let config = crate::test_support::test_state();
        for _ in 0..10 {
            let app = routes(state.clone()).with_state(config.clone());
            let resp = app
                .oneshot(
                    Request::builder()
                        .uri("/flaky/100")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert!(resp.headers().get("retry-after").is_some());
        }
    }

    #[tokio::test]
    async fn pattern_sfs() {
        let state = test_state();
        let config = crate::test_support::test_state();
        let expected = [
            StatusCode::OK,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::OK,
            StatusCode::OK,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::OK,
        ];

        for expected_status in &expected {
            let app = routes(state.clone()).with_state(config.clone());
            let resp = app
                .oneshot(
                    Request::builder()
                        .uri("/flaky/pattern/SFS")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(resp.status(), *expected_status);
        }
    }

    #[tokio::test]
    async fn pattern_invalid_returns_400() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/flaky/pattern/ABCD")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn pattern_case_insensitive() {
        let state = test_state();
        let config = crate::test_support::test_state();
        let app = routes(state.clone()).with_state(config.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/flaky/pattern/sf")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK); // first char is S
    }

    #[tokio::test]
    async fn after_3_succeeds_then_fails() {
        let state = test_state();
        let config = crate::test_support::test_state();

        for i in 1..=5 {
            let app = routes(state.clone()).with_state(config.clone());
            let resp = app
                .oneshot(
                    Request::builder()
                        .uri("/flaky/after/3")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            if i <= 3 {
                assert_eq!(
                    resp.status(),
                    StatusCode::OK,
                    "request {} should succeed",
                    i
                );
            } else {
                assert_eq!(
                    resp.status(),
                    StatusCode::SERVICE_UNAVAILABLE,
                    "request {} should fail",
                    i
                );
            }
        }
    }

    #[tokio::test]
    async fn recover_3_fails_then_succeeds() {
        let state = test_state();
        let config = crate::test_support::test_state();

        for i in 1..=5 {
            let app = routes(state.clone()).with_state(config.clone());
            let resp = app
                .oneshot(
                    Request::builder()
                        .uri("/flaky/recover/3")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            if i <= 3 {
                assert_eq!(
                    resp.status(),
                    StatusCode::SERVICE_UNAVAILABLE,
                    "request {} should fail",
                    i
                );
            } else {
                assert_eq!(
                    resp.status(),
                    StatusCode::OK,
                    "request {} should succeed",
                    i
                );
            }
        }
    }

    #[tokio::test]
    async fn reset_clears_counters() {
        let state = test_state();
        let config = crate::test_support::test_state();

        // Bump the after counter
        state.after_counter.store(10, Ordering::Relaxed);

        let app = routes(state.clone()).with_state(config.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/flaky/reset")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["status"], "reset");
        assert_eq!(state.after_counter.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn status_shows_counters() {
        let state = test_state();
        state.after_counter.store(5, Ordering::Relaxed);
        state.recover_counter.store(3, Ordering::Relaxed);

        let config = crate::test_support::test_state();
        let app = routes(state.clone()).with_state(config);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/flaky/status")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["after_counter"], 5);
        assert_eq!(json["recover_counter"], 3);
    }

    #[tokio::test]
    async fn flaky_xml_negotiation() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/flaky/status")
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
