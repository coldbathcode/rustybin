//! Reliability testing: random, patterned and threshold-based failures.
//!
//! Counters are scoped per client: the key is (session, route), where the
//! session is [`crate::session::session_key`] (`X-Rustybin-Session`, else the
//! client IP) and the route is the endpoint plus its parameter (`after:3`,
//! `pattern:SSF`, ...). Two clients (or two different `n`) never share a
//! counter. The map is bounded (capacity cap, idle TTL, oldest evicted).
//!
//! `POST /flaky/reset` resets the caller's own counters (open);
//! `POST /flaky/reset?scope=all` resets everyone's and is admin-guarded.

use axum::{
    extract::{Extension, Path, Query, State},
    http::{header::HeaderValue, HeaderMap, StatusCode},
    response::Response,
    routing::{any, get, post},
    Router,
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::session::Session;
use crate::state::AppState;
use crate::types::ErrorResponse;

/// Maximum (session, route) counters kept (normal mode).
pub const MAX_COUNTERS: usize = 10_000;
/// Maximum (session, route) counters kept (public mode).
pub const MAX_COUNTERS_PUBLIC: usize = 2_000;
/// Counters idle for longer than this are dropped.
pub const COUNTER_TTL: Duration = Duration::from_secs(3600);
/// Maximum pattern length.
pub const MAX_PATTERN_LEN: usize = 64;
/// Maximum `n` for `/flaky/after/{n}` and `/flaky/recover/{n}`.
pub const MAX_THRESHOLD: u64 = 1_000_000;

// ── State ───────────────────────────────────────────────────────────

struct Counter {
    count: u64,
    last_used: Instant,
}

/// Bounded per-(session, route) request counters.
pub struct FlakyState {
    counters: Mutex<HashMap<(String, String), Counter>>,
    capacity: usize,
    ttl: Duration,
}

impl FlakyState {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            counters: Mutex::new(HashMap::new()),
            capacity: capacity.max(1),
            ttl,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), Counter>> {
        // A poisoned lock only means another request panicked mid-update;
        // the counters are still usable.
        self.counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Increment and return the 1-based request number for (session, route).
    pub fn next(&self, session: &str, route: &str) -> u64 {
        let now = Instant::now();
        let mut map = self.lock();
        let key = (session.to_string(), route.to_string());
        if let Some(c) = map.get_mut(&key) {
            if now.duration_since(c.last_used) <= self.ttl {
                c.count += 1;
                c.last_used = now;
                return c.count;
            }
            map.remove(&key);
        }
        if map.len() >= self.capacity {
            let ttl = self.ttl;
            map.retain(|_, c| now.duration_since(c.last_used) <= ttl);
            if map.len() >= self.capacity {
                if let Some(oldest) = map
                    .iter()
                    .min_by_key(|(_, c)| c.last_used)
                    .map(|(k, _)| k.clone())
                {
                    map.remove(&oldest);
                }
            }
        }
        map.insert(
            key,
            Counter {
                count: 1,
                last_used: now,
            },
        );
        1
    }

    /// Counters of one session: (route, count), sorted by route.
    pub fn session_counters(&self, session: &str) -> Vec<(String, u64)> {
        let now = Instant::now();
        let map = self.lock();
        let mut out: Vec<(String, u64)> = map
            .iter()
            .filter(|((s, _), c)| s == session && now.duration_since(c.last_used) <= self.ttl)
            .map(|((_, r), c)| (r.clone(), c.count))
            .collect();
        out.sort();
        out
    }

    /// Remove one session's counters; returns how many were removed.
    pub fn reset_session(&self, session: &str) -> usize {
        let mut map = self.lock();
        let before = map.len();
        map.retain(|(s, _), _| s != session);
        before - map.len()
    }

    /// Remove every counter; returns how many were removed.
    pub fn reset_all(&self) -> usize {
        let mut map = self.lock();
        let n = map.len();
        map.clear();
        n
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ── Response types ──────────────────────────────────────────────────

#[derive(Serialize)]
struct FlakyOutcome {
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'static str>,
    message: String,
    route: String,
    request_number: u64,
    session: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fail_rate: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    threshold: Option<u64>,
}

impl FlakyOutcome {
    fn new(route: String, session: String, request_number: u64, ok: bool, message: String) -> Self {
        Self {
            status: ok.then_some("ok"),
            error: (!ok).then_some("service_unavailable"),
            message,
            route,
            request_number,
            session,
            fail_rate: None,
            pattern: None,
            position: None,
            current: None,
            threshold: None,
        }
    }
}

#[derive(Serialize)]
struct ResetResponse {
    status: &'static str,
    scope: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<String>,
    counters_removed: usize,
}

#[derive(Serialize)]
struct CounterView {
    route: String,
    count: u64,
}

#[derive(Serialize)]
struct StatusResponse {
    session: String,
    counters: Vec<CounterView>,
}

// ── Helpers ─────────────────────────────────────────────────────────

fn bad_request(headers: &HeaderMap, details: String) -> Response {
    negotiate_with_status(
        headers,
        &ErrorResponse {
            error: "bad_request".to_string(),
            details: Some(details),
        },
        StatusCode::BAD_REQUEST,
    )
}

fn respond(headers: &HeaderMap, outcome: &FlakyOutcome, ok: bool) -> Response {
    let mut resp = if ok {
        negotiate(headers, outcome)
    } else {
        negotiate_with_status(headers, outcome, StatusCode::SERVICE_UNAVAILABLE)
    };
    let h = resp.headers_mut();
    h.insert("x-rustybin-flaky", HeaderValue::from_static("true"));
    if !ok {
        h.insert("retry-after", HeaderValue::from_static("1"));
    }
    if let Ok(v) = HeaderValue::from_str(&outcome.request_number.to_string()) {
        h.insert("x-rustybin-request-number", v);
    }
    if let Some(rate) = outcome.fail_rate {
        if let Ok(v) = HeaderValue::from_str(&rate.to_string()) {
            h.insert("x-rustybin-fail-rate", v);
        }
    }
    resp
}

fn parse_threshold(raw: &str) -> Result<u64, String> {
    raw.trim()
        .parse::<u64>()
        .ok()
        .filter(|n| *n <= MAX_THRESHOLD)
        .ok_or_else(|| format!("n must be an integer between 0 and {MAX_THRESHOLD}"))
}

// ── Handlers ────────────────────────────────────────────────────────

async fn flaky_rate_handler(
    Path(raw): Path<String>,
    Session(session): Session,
    Extension(state): Extension<Arc<FlakyState>>,
    headers: HeaderMap,
) -> Response {
    let Some(fail_rate) = raw.trim().parse::<u8>().ok().filter(|r| *r <= 100) else {
        return bad_request(&headers, "fail_rate must be an integer 0-100".to_string());
    };
    let route = format!("rate:{fail_rate}");
    let count = state.next(&session, &route);
    // Unbiased roll in 0..100.
    let ok = rand::thread_rng().gen_range(0..100u8) >= fail_rate;
    let message = if ok {
        "Request succeeded".to_string()
    } else {
        format!("Simulated failure ({fail_rate}% fail rate)")
    };
    let mut outcome = FlakyOutcome::new(route, session, count, ok, message);
    outcome.fail_rate = Some(fail_rate);
    respond(&headers, &outcome, ok)
}

async fn flaky_pattern_handler(
    Path(pattern): Path<String>,
    Session(session): Session,
    Extension(state): Extension<Arc<FlakyState>>,
    headers: HeaderMap,
) -> Response {
    let upper = pattern.to_ascii_uppercase();
    if upper.is_empty()
        || upper.len() > MAX_PATTERN_LEN
        || !upper.bytes().all(|c| c == b'S' || c == b'F')
    {
        return bad_request(
            &headers,
            format!("Pattern must be 1-{MAX_PATTERN_LEN} characters of S (success) and F (fail)"),
        );
    }
    let route = format!("pattern:{upper}");
    let count = state.next(&session, &route);
    let pos = (count - 1) % upper.len() as u64;
    let ok = upper.as_bytes().get(pos as usize) != Some(&b'F');
    let mut outcome =
        FlakyOutcome::new(route, session, count, ok, format!("pattern position {pos}"));
    outcome.pattern = Some(upper);
    outcome.position = Some(pos);
    outcome.current = Some(if ok { "S" } else { "F" });
    respond(&headers, &outcome, ok)
}

async fn flaky_after_handler(
    Path(raw): Path<String>,
    Session(session): Session,
    Extension(state): Extension<Arc<FlakyState>>,
    headers: HeaderMap,
) -> Response {
    let n = match parse_threshold(&raw) {
        Ok(n) => n,
        Err(e) => return bad_request(&headers, e),
    };
    let route = format!("after:{n}");
    let count = state.next(&session, &route);
    let ok = count <= n;
    let message = if ok {
        format!("success {count} of {n}, failing afterwards")
    } else {
        format!("failing after {n} successful requests")
    };
    let mut outcome = FlakyOutcome::new(route, session, count, ok, message);
    outcome.threshold = Some(n);
    respond(&headers, &outcome, ok)
}

async fn flaky_recover_handler(
    Path(raw): Path<String>,
    Session(session): Session,
    Extension(state): Extension<Arc<FlakyState>>,
    headers: HeaderMap,
) -> Response {
    let n = match parse_threshold(&raw) {
        Ok(n) => n,
        Err(e) => return bad_request(&headers, e),
    };
    let route = format!("recover:{n}");
    let count = state.next(&session, &route);
    let ok = count > n;
    let message = if ok {
        format!("recovered after {n} failures")
    } else {
        format!("failure {count} of {n} before recovering")
    };
    let mut outcome = FlakyOutcome::new(route, session, count, ok, message);
    outcome.threshold = Some(n);
    respond(&headers, &outcome, ok)
}

#[derive(Deserialize)]
struct ResetQuery {
    #[serde(default)]
    scope: Option<String>,
}

async fn flaky_reset_handler(
    State(config): State<Arc<Config>>,
    Session(session): Session,
    Extension(state): Extension<Arc<FlakyState>>,
    Query(query): Query<ResetQuery>,
    headers: HeaderMap,
) -> Response {
    match query.scope.as_deref() {
        Some("all") => {
            // Resetting every client's counters is a global mutation.
            if let Err(resp) = crate::admin::require_admin(&headers, &config) {
                return resp;
            }
            let removed = state.reset_all();
            negotiate(
                &headers,
                &ResetResponse {
                    status: "reset",
                    scope: "all",
                    session: None,
                    counters_removed: removed,
                },
            )
        }
        None | Some("session") => {
            let removed = state.reset_session(&session);
            negotiate(
                &headers,
                &ResetResponse {
                    status: "reset",
                    scope: "session",
                    session: Some(session),
                    counters_removed: removed,
                },
            )
        }
        Some(_) => bad_request(&headers, "scope must be session or all".to_string()),
    }
}

async fn flaky_status_handler(
    Session(session): Session,
    Extension(state): Extension<Arc<FlakyState>>,
    headers: HeaderMap,
) -> Response {
    let counters = state
        .session_counters(&session)
        .into_iter()
        .map(|(route, count)| CounterView { route, count })
        .collect();
    negotiate(&headers, &StatusResponse { session, counters })
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(state: &AppState) -> Router<AppState> {
    let capacity = if state.config.public_mode {
        MAX_COUNTERS_PUBLIC
    } else {
        MAX_COUNTERS
    };
    routes(Arc::new(FlakyState::new(capacity, COUNTER_TTL)))
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
            "Deterministic success/failure pattern (S = success, F = failure), per session",
        )
        .example(Example::get("Flaky pattern SSFSS", "/flaky/pattern/SSFSS")),
        Endpoint::new(
            "/flaky/after/{n}",
            &["ANY"],
            category::RELIABILITY,
            "Succeed n times, then fail (circuit breaker trip), per session and n",
        )
        .example(Example::get("Fail after 3", "/flaky/after/3")),
        Endpoint::new(
            "/flaky/recover/{n}",
            &["ANY"],
            category::RELIABILITY,
            "Fail n times, then recover (circuit breaker half-open), per session and n",
        )
        .example(Example::get("Recover after 3", "/flaky/recover/3")),
        Endpoint::new(
            "/flaky/reset",
            &["POST"],
            category::RELIABILITY,
            "Reset the caller's counters (?scope=all resets everyone's, admin-guarded)",
        )
        .description(
            "Counters are keyed by session (X-Rustybin-Session header, else client IP) and \
             route. Without scope only the caller's session is reset.",
        )
        .example(Example::post("Reset my counters", "/flaky/reset")),
        Endpoint::new(
            "/flaky/status",
            &["GET"],
            category::RELIABILITY,
            "The caller's flaky counters",
        )
        .example(Example::get("Counter status", "/flaky/status")),
    ]
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::body_json;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn app() -> Router {
        crate::test_support::module_app(router)
    }

    fn req(method: &str, uri: &str, session: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().method(method).uri(uri);
        if let Some(s) = session {
            b = b.header("x-rustybin-session", s);
        }
        b.body(Body::empty()).expect("request")
    }

    async fn status_of(app: &Router, uri: &str, session: &str) -> u16 {
        app.clone()
            .oneshot(req("GET", uri, Some(session)))
            .await
            .expect("response")
            .status()
            .as_u16()
    }

    #[tokio::test]
    async fn rate_extremes() {
        let app = app();
        for _ in 0..20 {
            assert_eq!(status_of(&app, "/flaky/0", "s").await, 200);
            assert_eq!(status_of(&app, "/flaky/100", "s").await, 503);
        }
        let resp = app
            .clone()
            .oneshot(req("GET", "/flaky/101", None))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(resp).await["error"], "bad_request");
        let resp = app
            .oneshot(req("GET", "/flaky/abc", None))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn failure_carries_retry_after_and_headers() {
        let resp = app()
            .oneshot(req("GET", "/flaky/100", None))
            .await
            .expect("r");
        assert_eq!(resp.headers()["retry-after"], "1");
        assert_eq!(resp.headers()["x-rustybin-fail-rate"], "100");
        assert_eq!(resp.headers()["x-rustybin-request-number"], "1");
    }

    #[tokio::test]
    async fn pattern_is_per_session() {
        let app = app();
        let seq_a: Vec<u16> = {
            let mut v = Vec::new();
            for _ in 0..4 {
                v.push(status_of(&app, "/flaky/pattern/SFS", "a").await);
            }
            v
        };
        assert_eq!(seq_a, vec![200, 503, 200, 200]);
        // Session b starts from the beginning of the pattern.
        assert_eq!(status_of(&app, "/flaky/pattern/sfs", "b").await, 200);
        assert_eq!(status_of(&app, "/flaky/pattern/SFS", "b").await, 503);
        let resp = app
            .oneshot(req(
                "GET",
                &format!("/flaky/pattern/{}", "S".repeat(65)),
                None,
            ))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn after_and_recover_are_per_session_and_n() {
        let app = app();
        assert_eq!(status_of(&app, "/flaky/after/2", "a").await, 200);
        assert_eq!(status_of(&app, "/flaky/after/2", "a").await, 200);
        assert_eq!(status_of(&app, "/flaky/after/2", "a").await, 503);
        // Different n: separate counter.
        assert_eq!(status_of(&app, "/flaky/after/3", "a").await, 200);
        // Different session: separate counter.
        assert_eq!(status_of(&app, "/flaky/after/2", "b").await, 200);

        assert_eq!(status_of(&app, "/flaky/recover/1", "a").await, 503);
        assert_eq!(status_of(&app, "/flaky/recover/1", "a").await, 200);
        assert_eq!(status_of(&app, "/flaky/recover/1", "b").await, 503);
    }

    #[tokio::test]
    async fn session_reset_is_open_global_reset_is_guarded() {
        let mut config = crate::test_support::test_config();
        config.admin_token = Some("tok".to_string());
        let app = crate::test_support::module_app_with_config(config, router);
        status_of(&app, "/flaky/after/1", "a").await;
        assert_eq!(status_of(&app, "/flaky/after/1", "a").await, 503);
        status_of(&app, "/flaky/after/1", "b").await;

        let resp = app
            .clone()
            .oneshot(req("POST", "/flaky/reset", Some("a")))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["scope"], "session");
        assert_eq!(json["counters_removed"], 1);
        assert_eq!(status_of(&app, "/flaky/after/1", "a").await, 200);
        // b untouched.
        assert_eq!(status_of(&app, "/flaky/after/1", "b").await, 503);

        let resp = app
            .clone()
            .oneshot(req("POST", "/flaky/reset?scope=all", Some("a")))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/flaky/reset?scope=all")
                    .header("authorization", "Bearer tok")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(status_of(&app, "/flaky/after/1", "b").await, 200);
    }

    #[tokio::test]
    async fn status_lists_only_the_callers_counters() {
        let app = app();
        status_of(&app, "/flaky/after/5", "a").await;
        status_of(&app, "/flaky/after/5", "a").await;
        status_of(&app, "/flaky/pattern/SF", "b").await;
        let resp = app
            .oneshot(req("GET", "/flaky/status", Some("a")))
            .await
            .expect("r");
        let json = body_json(resp).await;
        assert_eq!(json["session"], "a");
        assert_eq!(
            json["counters"],
            serde_json::json!([{ "route": "after:5", "count": 2 }])
        );
    }

    #[test]
    fn state_is_bounded_and_expires() {
        let state = FlakyState::new(3, Duration::from_secs(3600));
        for i in 0..10 {
            state.next(&format!("s{i}"), "r");
        }
        assert_eq!(state.len(), 3);
        // The most recent survive.
        assert_eq!(state.session_counters("s9"), vec![("r".to_string(), 1)]);
        assert!(state.session_counters("s0").is_empty());

        let state = FlakyState::new(10, Duration::from_millis(0));
        state.next("a", "r");
        std::thread::sleep(Duration::from_millis(5));
        // Expired: starts again from 1.
        assert_eq!(state.next("a", "r"), 1);
        assert_eq!(state.reset_all(), 1);
        assert!(state.is_empty());
    }
}
