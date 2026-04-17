use axum::{
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::any,
    Router,
};
use std::sync::Arc;

use crate::config::Config;
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::types::{AuthFailure, AuthResponse};

const DEFAULT_HEADER: &str = "apikey";
const DEFAULT_KEY: &str = "my-key";

// ── Handlers ────────────────────────────────────────────────────────

async fn apikey_default(headers: HeaderMap) -> Response {
    check_apikey(&headers, DEFAULT_HEADER, DEFAULT_KEY)
}

async fn apikey_custom(
    Path((header_name, key_value)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    check_apikey(&headers, &header_name, &key_value)
}

fn check_apikey(headers: &HeaderMap, expected_header: &str, expected_key: &str) -> Response {
    // Case-insensitive header lookup
    let provided_key = headers.iter().find_map(|(name, value)| {
        if name.as_str().eq_ignore_ascii_case(expected_header) {
            value.to_str().ok().map(|v| v.to_string())
        } else {
            None
        }
    });

    let Some(key) = provided_key else {
        return negotiate_with_status(
            headers,
            &AuthFailure {
                authenticated: false,
                error: "unauthorized".to_string(),
            },
            StatusCode::UNAUTHORIZED,
        );
    };

    if key != expected_key {
        return negotiate_with_status(
            headers,
            &AuthFailure {
                authenticated: false,
                error: "unauthorized".to_string(),
            },
            StatusCode::UNAUTHORIZED,
        );
    }

    negotiate(
        headers,
        &AuthResponse {
            authenticated: true,
            auth_type: "api-key".to_string(),
            username: None,
            header: Some(expected_header.to_string()),
            claims: None,
            jwt_header: None,
            client_dn: None,
            client_ca: None,
        },
    )
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router() -> Router<Arc<Config>> {
    Router::new()
        .route("/auth/api-key", any(apikey_default))
        .route("/auth/api-key/:header_name/:key_value", any(apikey_custom))
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

    async fn json_body(resp: axum::http::Response<Body>) -> serde_json::Value {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&body).expect("json")
    }

    #[tokio::test]
    async fn default_key_success() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/api-key")
                    .header("apikey", "my-key")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["authenticated"], true);
        assert_eq!(json["auth_type"], "api-key");
        assert_eq!(json["header"], "apikey");
    }

    #[tokio::test]
    async fn missing_key_returns_401() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/api-key")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let json = json_body(resp).await;
        assert_eq!(json["authenticated"], false);
        assert_eq!(json["error"], "unauthorized");
    }

    #[tokio::test]
    async fn wrong_key_returns_401() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/api-key")
                    .header("apikey", "wrong-key")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn custom_header_and_key_success() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/api-key/x-api-key/supersecret")
                    .header("x-api-key", "supersecret")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["authenticated"], true);
        assert_eq!(json["header"], "x-api-key");
    }

    #[tokio::test]
    async fn case_insensitive_header_match() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/api-key/X-Api-Key/secret123")
                    .header("x-api-key", "secret123")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn post_method_works() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/api-key")
                    .header("apikey", "my-key")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn xml_content_negotiation() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/api-key")
                    .header("apikey", "my-key")
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
