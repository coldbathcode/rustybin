use axum::{
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::any,
    Router,
};

use crate::catalog::{category, Endpoint, Example};
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::state::AppState;
use crate::types::{constant_time_eq, AuthFailure, AuthResponse};

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

    let valid = provided_key
        .as_deref()
        .is_some_and(|key| constant_time_eq(key.as_bytes(), expected_key.as_bytes()));
    if !valid {
        return negotiate_with_status(
            headers,
            &AuthFailure::new("unauthorized"),
            StatusCode::UNAUTHORIZED,
        );
    }

    negotiate(
        headers,
        &AuthResponse {
            header: Some(expected_header.to_string()),
            ..AuthResponse::ok("api-key")
        },
    )
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/auth/api-key", any(apikey_default))
        .route(
            "/auth/api-key/{header_name}/{key_value}",
            any(apikey_custom),
        )
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/auth/api-key",
            &["ANY"],
            category::AUTH_BASIC,
            "API key in a header (default apikey: my-key)",
        )
        .description(
            "Default: header `apikey` with value `my-key` (header name matched \
             case-insensitively, value compared in constant time).",
        )
        .example(Example::get("API key (default)", "/auth/api-key").header("apikey", "my-key")),
        Endpoint::new(
            "/auth/api-key/{header_name}/{key_value}",
            &["ANY"],
            category::AUTH_BASIC,
            "API key with header name and value from the path",
        )
        .example(
            Example::get("API key (custom)", "/auth/api-key/x-token/s3cret")
                .header("x-token", "s3cret"),
        ),
    ]
}

pub fn openapi_paths() -> serde_json::Value {
    use serde_json::json;
    let ok =
        crate::openapi::json_xml_content(json!({ "$ref": "#/components/schemas/AuthResponse" }));
    let fail =
        crate::openapi::json_xml_content(json!({ "$ref": "#/components/schemas/AuthFailure" }));
    json!({
        "/auth/api-key": { "get": {
            "tags": ["Auth"],
            "summary": "API key authentication (default header)",
            "description": "Validates the API key in the `apikey` header against the default value `my-key`.",
            "operationId": "getApiKey",
            "security": [{ "apiKeyAuth": [] }],
            "responses": {
                "200": { "description": "Authenticated", "content": ok },
                "401": { "description": "Unauthorized", "content": fail }
            }
        }},
        "/auth/api-key/{header_name}/{key_value}": { "get": {
            "tags": ["Auth"],
            "summary": "API key authentication (custom header and value)",
            "description": "Validates the API key in a custom header against a custom value, both from the path.",
            "operationId": "getApiKeyCustom",
            "parameters": [
                { "name": "header_name", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Header name to check" },
                { "name": "key_value", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Expected key value" }
            ],
            "responses": {
                "200": { "description": "Authenticated", "content": ok },
                "401": { "description": "Unauthorized", "content": fail }
            }
        }}
    })
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
