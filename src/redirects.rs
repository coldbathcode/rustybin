use axum::{
    extract::Path,
    http::{header, HeaderMap, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::config::Config;
use crate::content_negotiation::negotiate_with_status;
use crate::types::ErrorResponse;

// ── /redirect/:n ─────────────────────────────────────────────────────

async fn redirect_handler(Path(n): Path<String>, uri: Uri, headers: HeaderMap) -> Response {
    let n_val: u32 = match n.parse() {
        Ok(0) => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "invalid_redirect_count".to_string(),
                    details: Some("Use /echo directly instead of /redirect/0".to_string()),
                },
                StatusCode::BAD_REQUEST,
            );
        }
        Ok(v) if v <= 20 => v,
        _ => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "invalid_redirect_count".to_string(),
                    details: Some("Redirect count must be between 1 and 20".to_string()),
                },
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();

    let location = if n_val == 1 {
        format!("/echo{query}")
    } else {
        format!("/redirect/{}{query}", n_val - 1)
    };

    build_redirect(&location, StatusCode::FOUND, n_val)
}

// ── /absolute-redirect/:n ────────────────────────────────────────────

async fn absolute_redirect_handler(
    Path(n): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let n_val: u32 = match n.parse() {
        Ok(0) => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "invalid_redirect_count".to_string(),
                    details: Some("Use /echo directly instead of /absolute-redirect/0".to_string()),
                },
                StatusCode::BAD_REQUEST,
            );
        }
        Ok(v) if v <= 20 => v,
        _ => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "invalid_redirect_count".to_string(),
                    details: Some("Redirect count must be between 1 and 20".to_string()),
                },
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");

    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");

    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();

    let location = if n_val == 1 {
        format!("{scheme}://{host}/echo{query}")
    } else {
        format!("{scheme}://{host}/absolute-redirect/{}{query}", n_val - 1)
    };

    build_redirect(&location, StatusCode::FOUND, n_val)
}

// ── /redirect-to ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RedirectToQuery {
    url: Option<String>,
    status: Option<u16>,
}

async fn redirect_to_handler(
    axum::extract::Query(query): axum::extract::Query<RedirectToQuery>,
    headers: HeaderMap,
) -> Response {
    let url = match &query.url {
        Some(u) if !u.is_empty() => u.clone(),
        _ => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "missing_url".to_string(),
                    details: Some("Query parameter 'url' is required".to_string()),
                },
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let status_code = query.status.unwrap_or(302);
    let valid_redirects = [301, 302, 303, 307, 308];
    if !valid_redirects.contains(&status_code) {
        return negotiate_with_status(
            &headers,
            &ErrorResponse {
                error: "invalid_redirect_status".to_string(),
                details: Some(
                    "Status must be a redirect code: 301, 302, 303, 307, 308".to_string(),
                ),
            },
            StatusCode::BAD_REQUEST,
        );
    }

    let status = match StatusCode::from_u16(status_code) {
        Ok(s) => s,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    build_redirect(&url, status, 1)
}

// ── Helpers ──────────────────────────────────────────────────────────

fn build_redirect(location: &str, status: StatusCode, remaining: u32) -> Response {
    let mut resp = status.into_response();
    if let Ok(loc) = HeaderValue::from_str(location) {
        resp.headers_mut().insert(header::LOCATION, loc);
    }
    if let (Ok(name), Ok(val)) = (
        "x-redirect-count".parse::<axum::http::HeaderName>(),
        HeaderValue::from_str(&remaining.to_string()),
    ) {
        resp.headers_mut().insert(name, val);
    }
    resp
}

// ── Router ───────────────────────────────────────────────────────────

pub fn router() -> Router<Arc<Config>> {
    Router::new()
        .route("/redirect/:n", get(redirect_handler))
        .route("/redirect-to", get(redirect_to_handler))
        .route("/absolute-redirect/:n", get(absolute_redirect_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_config() -> Arc<Config> {
        Arc::new(Config {
            http_port: 80,
            https_port: 443,
            host: "0.0.0.0".to_string(),
            log_level: "info".to_string(),
            trust_forward: false,
            body_limit: 1_048_576,
            instance_id: "test-instance".to_string(),
            tls_cert: "certs/server.crt".to_string(),
            tls_key: "certs/server.key".to_string(),
            mtls_in_header: None,
        })
    }

    fn test_app() -> Router {
        router().with_state(test_config())
    }

    #[tokio::test]
    async fn redirect_3_goes_to_redirect_2() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/redirect/3")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::FOUND);
        assert_eq!(
            resp.headers().get("location").expect("location").to_str().expect("str"),
            "/redirect/2"
        );
    }

    #[tokio::test]
    async fn redirect_1_goes_to_echo() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/redirect/1")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::FOUND);
        assert_eq!(
            resp.headers().get("location").expect("location").to_str().expect("str"),
            "/echo"
        );
    }

    #[tokio::test]
    async fn redirect_preserves_query() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/redirect/1?foo=bar")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(
            resp.headers().get("location").expect("location").to_str().expect("str"),
            "/echo?foo=bar"
        );
    }

    #[tokio::test]
    async fn redirect_0_returns_400() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/redirect/0")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn redirect_over_20_returns_400() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/redirect/21")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn redirect_to_with_url() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/redirect-to?url=http://example.com&status=307")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            resp.headers().get("location").expect("location").to_str().expect("str"),
            "http://example.com"
        );
    }

    #[tokio::test]
    async fn redirect_to_missing_url_returns_400() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/redirect-to")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn redirect_to_invalid_status_returns_400() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/redirect-to?url=http://example.com&status=200")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn absolute_redirect_uses_host() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/absolute-redirect/2")
                    .header("host", "example.com")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::FOUND);
        assert_eq!(
            resp.headers().get("location").expect("location").to_str().expect("str"),
            "http://example.com/absolute-redirect/1"
        );
    }
}
