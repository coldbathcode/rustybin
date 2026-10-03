use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use serde::Serialize;
use std::sync::Arc;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::content_negotiation::negotiate_with_status;
use crate::state::AppState;
use crate::types::ErrorResponse;

#[derive(Serialize)]
struct StatusResponse {
    status: u16,
}

async fn status_handler(
    State(_config): State<Arc<Config>>,
    Path(code): Path<String>,
    headers: HeaderMap,
) -> Response {
    let code_num: u16 = match code.parse() {
        Ok(c) if (100..=599).contains(&c) => c,
        _ => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "invalid_status_code".to_string(),
                    details: Some("Status code must be between 100 and 599".to_string()),
                },
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let status = match StatusCode::from_u16(code_num) {
        Ok(s) => s,
        Err(_) => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "invalid_status_code".to_string(),
                    details: Some("Status code must be between 100 and 599".to_string()),
                },
                StatusCode::BAD_REQUEST,
            );
        }
    };

    // 1xx informational or 204/304: empty body
    if status.is_informational()
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED
    {
        return (status, ()).into_response();
    }

    // Redirect codes: include Location header
    let is_redirect = matches!(code_num, 301 | 302 | 307 | 308);

    let mut resp = negotiate_with_status(&headers, &StatusResponse { status: code_num }, status);

    if is_redirect {
        resp.headers_mut()
            .insert(header::LOCATION, HeaderValue::from_static("/echo"));
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
        "Respond with any HTTP status code (200-599)",
    )
    .example(Example::get("200 OK", "/status/200"))
    .example(Example::get("418 I'm a Teapot", "/status/418"))
    .example(Example::get("503 Service Unavailable", "/status/503"))]
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
    async fn status_200() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/status/200")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["status"], 200);
    }

    #[tokio::test]
    async fn status_418() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/status/418")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status().as_u16(), 418);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["status"], 418);
    }

    #[tokio::test]
    async fn status_204_empty_body() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/status/204")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn status_304_empty_body() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/status/304")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn status_302_redirect() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/status/302")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::FOUND);
        assert_eq!(
            resp.headers()
                .get("location")
                .expect("location header")
                .to_str()
                .expect("str"),
            "/echo"
        );
    }

    #[tokio::test]
    async fn status_invalid_returns_400() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/status/999")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["error"], "invalid_status_code");
    }

    #[tokio::test]
    async fn status_not_a_number_returns_400() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/status/abc")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn status_xml_negotiation() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/status/200")
                    .header("accept", "application/xml")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
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
