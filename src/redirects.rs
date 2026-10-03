use axum::{
    extract::Path,
    http::{header, HeaderMap, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use crate::catalog::{category, Endpoint, Example};
use crate::content_negotiation::negotiate_with_status;
use crate::state::AppState;
use crate::types::ErrorResponse;

// ── /redirect/:n ─────────────────────────────────────────────────────
//
// Only relative, same-origin redirect chains are supported. The open
// `/redirect-to?url=` and `/absolute-redirect` variants were removed because
// they are open-redirect / SSRF vectors when the service is publicly hosted.

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

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new().route("/redirect/{n}", get(redirect_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![Endpoint::new(
        "/redirect/{n}",
        &["GET"],
        category::REDIRECTS,
        "Chain of n relative 302 redirects",
    )
    .example(Example::get("Redirect chain (3)", "/redirect/3"))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app() -> Router {
        crate::test_support::module_app(router)
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
            resp.headers()
                .get("location")
                .expect("location")
                .to_str()
                .expect("str"),
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
            resp.headers()
                .get("location")
                .expect("location")
                .to_str()
                .expect("str"),
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
            resp.headers()
                .get("location")
                .expect("location")
                .to_str()
                .expect("str"),
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
}
