//! Fault injection middleware, applied to every route except the control
//! plane (`/_rustybin/*`) and the UI (`/ui/*`).
//!
//! - `X-Rustybin-Delay: <ms>`: wait before handling (capped by
//!   `Config::max_delay_ms`, 30 s normally, 10 s in public mode).
//! - `X-Rustybin-Fail: <status>` or `<status>:<percent>` (e.g. `503:50`):
//!   respond with that status (400..=599) and a JSON error body, always or
//!   with the given probability.
//!
//! Invalid header values are ignored (the request is handled normally).

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rand::Rng;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;

pub const DELAY_HEADER: &str = "x-rustybin-delay";
pub const FAIL_HEADER: &str = "x-rustybin-fail";
/// Response header set when a fault was injected.
pub const INJECTED_HEADER: &str = "x-rustybin-fault";

/// Parsed `X-Rustybin-Fail` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailSpec {
    pub status: StatusCode,
    pub percent: u8,
}

/// Parse `<status>` or `<status>:<percent>`; status must be 400..=599 and
/// percent 0..=100.
pub fn parse_fail(value: &str) -> Option<FailSpec> {
    let (status, percent) = match value.trim().split_once(':') {
        Some((s, p)) => (s.trim(), p.trim().trim_end_matches('%').parse::<u8>().ok()?),
        None => (value.trim(), 100),
    };
    let code: u16 = status.parse().ok()?;
    if !(400..=599).contains(&code) || percent > 100 {
        return None;
    }
    Some(FailSpec {
        status: StatusCode::from_u16(code).ok()?,
        percent,
    })
}

/// Parse a delay in milliseconds and clamp it to `max_ms`.
pub fn parse_delay(value: &str, max_ms: u64) -> Option<u64> {
    value.trim().parse::<u64>().ok().map(|ms| ms.min(max_ms))
}

pub async fn inject(State(config): State<Arc<Config>>, req: Request, next: Next) -> Response {
    if crate::control::is_control_path(req.uri().path()) {
        return next.run(req).await;
    }

    let (delay, fail) = {
        let headers = req.headers();
        let value = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        (
            value(DELAY_HEADER).and_then(|v| parse_delay(v, config.max_delay_ms())),
            value(FAIL_HEADER).and_then(parse_fail),
        )
    };

    if let Some(ms) = delay.filter(|ms| *ms > 0) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    if let Some(spec) = fail {
        let hit = spec.percent >= 100
            || (spec.percent > 0 && rand::thread_rng().gen_range(0..100u8) < spec.percent);
        if hit {
            let mut resp = (
                spec.status,
                Json(json!({
                    "error": "injected fault",
                    "status": spec.status.as_u16(),
                    "reason": spec.status.canonical_reason().unwrap_or("Error"),
                    "percent": spec.percent,
                    "injected_by": "X-Rustybin-Fail",
                })),
            )
                .into_response();
            resp.headers_mut()
                .insert(INJECTED_HEADER, HeaderValue::from_static("fail"));
            return resp;
        }
    }

    let mut resp = next.run(req).await;
    if delay.is_some_and(|ms| ms > 0) {
        resp.headers_mut()
            .insert(INJECTED_HEADER, HeaderValue::from_static("delay"));
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_json, test_app};
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    #[test]
    fn parse_fail_values() {
        assert_eq!(
            parse_fail("503"),
            Some(FailSpec {
                status: StatusCode::SERVICE_UNAVAILABLE,
                percent: 100
            })
        );
        assert_eq!(parse_fail("500:25").map(|s| s.percent), Some(25));
        assert_eq!(parse_fail("500:25%").map(|s| s.percent), Some(25));
        assert!(parse_fail("200").is_none());
        assert!(parse_fail("503:101").is_none());
        assert!(parse_fail("abc").is_none());
    }

    #[test]
    fn delay_is_capped() {
        assert_eq!(parse_delay("999999", 30_000), Some(30_000));
        assert_eq!(parse_delay("15", 30_000), Some(15));
        assert_eq!(parse_delay("-1", 30_000), None);
    }

    #[tokio::test]
    async fn fail_header_injects_status() {
        let req = HttpRequest::builder()
            .uri("/echo")
            .header(FAIL_HEADER, "503")
            .body(Body::empty())
            .expect("request");
        let resp = test_app().oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(resp.headers()[INJECTED_HEADER], "fail");
        assert_eq!(body_json(resp).await["error"], "injected fault");
    }

    #[tokio::test]
    async fn zero_percent_never_fails_and_control_plane_is_exempt() {
        let req = HttpRequest::builder()
            .uri("/echo")
            .header(FAIL_HEADER, "500:0")
            .body(Body::empty())
            .expect("request");
        let resp = test_app().oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let req = HttpRequest::builder()
            .uri("/_rustybin/version")
            .header(FAIL_HEADER, "500")
            .body(Body::empty())
            .expect("request");
        let resp = test_app().oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn delay_header_delays() {
        let req = HttpRequest::builder()
            .uri("/echo")
            .header(DELAY_HEADER, "50")
            .body(Body::empty())
            .expect("request");
        let start = std::time::Instant::now();
        let resp = test_app().oneshot(req).await.expect("response");
        assert!(start.elapsed() >= Duration::from_millis(50));
        assert_eq!(resp.headers()[INJECTED_HEADER], "delay");
    }
}
