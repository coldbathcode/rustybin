//! `/status/{codes}`: respond with a chosen HTTP status.
//!
//! - Single code: `/status/418`.
//! - httpbin-style random choice: `/status/200,500` (uniform) or weighted
//!   `/status/200:0.9,500:0.1` (weights are relative, need not sum to 1).
//! - 1xx codes are rejected with a JSON 400: an informational status cannot
//!   be a final response and the HTTP stack (hyper) cannot send one on demand.
//! - 204, 205 and 304 have no body; 3xx redirects (301, 302, 303, 307, 308)
//!   carry `Location: /echo`; 401 adds `WWW-Authenticate`, 407
//!   `Proxy-Authenticate`, 429 and 503 `Retry-After`.

use axum::{
    extract::Path,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use rand::distributions::{Distribution, WeightedIndex};
use serde::Serialize;

use crate::catalog::{category, Endpoint, Example};
use crate::content_negotiation::negotiate_with_status;
use crate::state::AppState;
use crate::types::ErrorResponse;

/// Maximum number of choices in a weighted list.
const MAX_CHOICES: usize = 20;

#[derive(Serialize)]
struct StatusResponse {
    status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    chosen_from: Option<String>,
}

fn bad_request(headers: &HeaderMap, error: &str, details: String) -> Response {
    negotiate_with_status(
        headers,
        &ErrorResponse {
            error: error.to_string(),
            details: Some(details),
        },
        StatusCode::BAD_REQUEST,
    )
}

/// Parse `200`, `200,500` or `200:0.9,500:0.1` into (code, weight) pairs.
fn parse_choices(spec: &str) -> Result<Vec<(u16, f64)>, String> {
    let parts: Vec<&str> = spec.split(',').map(str::trim).collect();
    if parts.is_empty() || parts.len() > MAX_CHOICES {
        return Err(format!("between 1 and {MAX_CHOICES} codes are allowed"));
    }
    parts
        .iter()
        .map(|part| {
            let (code, weight) = match part.split_once(':') {
                Some((c, w)) => {
                    let w: f64 = w
                        .trim()
                        .parse()
                        .map_err(|_| format!("invalid weight in {part:?}"))?;
                    (c.trim(), w)
                }
                None => (*part, 1.0),
            };
            if !weight.is_finite() || weight < 0.0 {
                return Err(format!(
                    "weight in {part:?} must be a finite non-negative number"
                ));
            }
            let code: u16 = code
                .parse()
                .ok()
                .filter(|c| (100..=599).contains(c))
                .ok_or_else(|| format!("{code:?} is not a status code between 100 and 599"))?;
            Ok((code, weight))
        })
        .collect()
}

fn choose(choices: &[(u16, f64)]) -> Result<u16, String> {
    if choices.len() == 1 {
        return Ok(choices[0].0);
    }
    let dist = WeightedIndex::new(choices.iter().map(|(_, w)| *w))
        .map_err(|_| "at least one weight must be greater than zero".to_string())?;
    Ok(choices[dist.sample(&mut rand::thread_rng())].0)
}

async fn status_handler(Path(spec): Path<String>, headers: HeaderMap) -> Response {
    let choices = match parse_choices(&spec) {
        Ok(c) => c,
        Err(e) => return bad_request(&headers, "invalid_status_code", e),
    };
    if let Some((code, _)) = choices.iter().find(|(c, _)| *c < 200) {
        return bad_request(
            &headers,
            "unsupported_status_code",
            format!(
                "{code} is an informational (1xx) status: it cannot be sent as a final \
                 response. Use 200-599."
            ),
        );
    }
    let code = match choose(&choices) {
        Ok(c) => c,
        Err(e) => return bad_request(&headers, "invalid_status_code", e),
    };
    let Ok(status) = StatusCode::from_u16(code) else {
        return bad_request(
            &headers,
            "invalid_status_code",
            "Status code must be between 200 and 599".to_string(),
        );
    };

    let mut resp = if matches!(code, 204 | 205 | 304) {
        status.into_response()
    } else {
        negotiate_with_status(
            &headers,
            &StatusResponse {
                status: code,
                chosen_from: (choices.len() > 1).then(|| spec.clone()),
            },
            status,
        )
    };

    let extra: &[(header::HeaderName, &'static str)] = match code {
        301 | 302 | 303 | 307 | 308 => &[(header::LOCATION, "/echo")],
        401 => &[(header::WWW_AUTHENTICATE, "Basic realm=\"rustybin\"")],
        407 => &[(header::PROXY_AUTHENTICATE, "Basic realm=\"rustybin\"")],
        429 | 503 => &[(header::RETRY_AFTER, "1")],
        _ => &[],
    };
    for (name, value) in extra {
        resp.headers_mut()
            .insert(name.clone(), HeaderValue::from_static(value));
    }
    resp
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new().route("/status/{code}", any(status_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![Endpoint::new(
        "/status/{code}",
        &["ANY"],
        category::STATUS,
        "Respond with any HTTP status code (200-599), or a weighted random choice",
    )
    .description(
        "`/status/200,500` picks uniformly, `/status/200:0.9,500:0.1` by weight (up to 20 \
         codes). 1xx codes return 400 (they cannot be final responses). Redirects carry \
         `Location: /echo`, 401 `WWW-Authenticate`, 407 `Proxy-Authenticate`, 429/503 \
         `Retry-After`.",
    )
    .example(Example::get("200 OK", "/status/200"))
    .example(Example::get("418 I'm a Teapot", "/status/418"))
    .example(Example::get("503 Service Unavailable", "/status/503"))
    .example(Example::get(
        "Weighted 90% 200, 10% 500",
        "/status/200:0.9,500:0.1",
    ))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_bytes, body_json, get_request};
    use tower::ServiceExt;

    fn test_app() -> Router {
        crate::test_support::module_app(router)
    }

    async fn get(uri: &str) -> Response {
        test_app()
            .oneshot(get_request(uri))
            .await
            .expect("response")
    }

    #[tokio::test]
    async fn status_200_and_418() {
        let resp = get("/status/200").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["status"], 200);
        let resp = get("/status/418").await;
        assert_eq!(resp.status().as_u16(), 418);
        assert_eq!(body_json(resp).await["status"], 418);
    }

    #[tokio::test]
    async fn empty_body_codes() {
        for code in [204u16, 205, 304] {
            let resp = get(&format!("/status/{code}")).await;
            assert_eq!(resp.status().as_u16(), code);
            assert!(body_bytes(resp).await.is_empty());
        }
    }

    #[tokio::test]
    async fn redirects_carry_location_including_303() {
        for code in [301u16, 302, 303, 307, 308] {
            let resp = get(&format!("/status/{code}")).await;
            assert_eq!(resp.status().as_u16(), code);
            assert_eq!(resp.headers()["location"], "/echo", "code {code}");
        }
    }

    #[tokio::test]
    async fn auth_and_retry_headers() {
        assert!(get("/status/401")
            .await
            .headers()
            .contains_key("www-authenticate"));
        assert!(get("/status/407")
            .await
            .headers()
            .contains_key("proxy-authenticate"));
        assert_eq!(get("/status/429").await.headers()["retry-after"], "1");
    }

    #[tokio::test]
    async fn informational_codes_are_rejected_with_json() {
        let resp = get("/status/101").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert_eq!(json["error"], "unsupported_status_code");
        assert!(json["details"]
            .as_str()
            .unwrap_or("")
            .contains("informational"));
    }

    #[tokio::test]
    async fn invalid_codes_return_400() {
        for uri in [
            "/status/999",
            "/status/abc",
            "/status/200:-1",
            "/status/200:0,500:0",
            "/status/200:nan",
            "/status/200,",
        ] {
            let resp = get(uri).await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
        }
        let many = vec!["200"; 21].join(",");
        assert_eq!(
            get(&format!("/status/{many}")).await.status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn weighted_choice() {
        // Zero weight is never chosen.
        for _ in 0..20 {
            let resp = get("/status/200:0,503:1").await;
            assert_eq!(resp.status().as_u16(), 503);
        }
        let mut seen = std::collections::HashSet::new();
        for _ in 0..60 {
            seen.insert(get("/status/200,500").await.status().as_u16());
        }
        assert_eq!(seen.len(), 2, "both codes should appear: {seen:?}");
        let json = body_json(get("/status/201,202").await).await;
        assert_eq!(json["chosen_from"], "201,202");
    }

    #[tokio::test]
    async fn xml_negotiation() {
        let resp = test_app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/status/200")
                    .header("accept", "application/xml")
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.headers()["content-type"], "application/xml");
        assert_eq!(resp.headers()["vary"], "Accept");
    }
}
