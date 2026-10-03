use axum::{
    extract::Path,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::Response,
    routing::any,
    Router,
};
use base64::Engine;

use crate::catalog::{category, Endpoint, Example};
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::state::AppState;
use crate::types::{constant_time_eq, AuthFailure, AuthResponse};

const DEFAULT_USERNAME: &str = "basic";
const DEFAULT_PASSWORD: &str = "password";

// ── Handlers ────────────────────────────────────────────────────────

async fn basic_auth_default(headers: HeaderMap) -> Response {
    check_basic_auth(&headers, DEFAULT_USERNAME, DEFAULT_PASSWORD)
}

async fn basic_auth_custom(
    Path((username, password)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    check_basic_auth(&headers, &username, &password)
}

/// `(user, password)` from an `Authorization: Basic ...` header (scheme
/// matched case-insensitively).
pub fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, encoded) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let decoded = String::from_utf8(bytes).ok()?;
    let (user, pass) = decoded.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

fn check_basic_auth(headers: &HeaderMap, expected_user: &str, expected_pass: &str) -> Response {
    let Some((username, password)) = basic_credentials(headers) else {
        return unauthorized(headers);
    };

    // Evaluate both comparisons so timing does not reveal which one failed.
    let user_ok = constant_time_eq(username.as_bytes(), expected_user.as_bytes());
    let pass_ok = constant_time_eq(password.as_bytes(), expected_pass.as_bytes());
    if !(user_ok & pass_ok) {
        return unauthorized(headers);
    }

    negotiate(
        headers,
        &AuthResponse {
            username: Some(username),
            ..AuthResponse::ok("basic-auth")
        },
    )
}

fn unauthorized(headers: &HeaderMap) -> Response {
    let mut resp = negotiate_with_status(
        headers,
        &AuthFailure::new("unauthorized"),
        StatusCode::UNAUTHORIZED,
    );
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"rustybin\", charset=\"UTF-8\""),
    );
    resp
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/auth/basic-auth", any(basic_auth_default))
        .route(
            "/auth/basic-auth/{username}/{password}",
            any(basic_auth_custom),
        )
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/auth/basic-auth",
            &["ANY"],
            category::AUTH_BASIC,
            "HTTP Basic auth (default user basic, password password)",
        )
        .description(
            "Default credentials: user `basic`, password `password`. 401 responses carry \
             `WWW-Authenticate: Basic realm=\"rustybin\"`. Comparisons are constant time.",
        )
        .example(
            Example::get("Basic auth (default)", "/auth/basic-auth").basic("basic", "password"),
        ),
        Endpoint::new(
            "/auth/basic-auth/{username}/{password}",
            &["ANY"],
            category::AUTH_BASIC,
            "HTTP Basic auth with credentials from the path",
        )
        .example(
            Example::get("Basic auth (custom)", "/auth/basic-auth/alice/secret")
                .basic("alice", "secret"),
        ),
    ]
}

pub fn openapi_paths() -> serde_json::Value {
    use serde_json::json;
    let ok =
        crate::openapi::json_xml_content(json!({ "$ref": "#/components/schemas/AuthResponse" }));
    let unauthorized = json!({
        "description": "Unauthorized",
        "headers": { "WWW-Authenticate": { "schema": { "type": "string", "example": "Basic realm=\"rustybin\", charset=\"UTF-8\"" } } },
        "content": crate::openapi::json_xml_content(json!({ "$ref": "#/components/schemas/AuthFailure" }))
    });
    json!({
        "/auth/basic-auth": { "get": {
            "tags": ["Auth"],
            "summary": "HTTP Basic authentication (default credentials)",
            "description": "Validates HTTP Basic auth with the default credentials basic:password.",
            "operationId": "getBasicAuth",
            "security": [{ "basicAuth": [] }],
            "responses": { "200": { "description": "Authenticated", "content": ok }, "401": unauthorized }
        }},
        "/auth/basic-auth/{username}/{password}": { "get": {
            "tags": ["Auth"],
            "summary": "HTTP Basic authentication (custom credentials)",
            "description": "Validates HTTP Basic auth against the username and password given in the path.",
            "operationId": "getBasicAuthCustom",
            "security": [{ "basicAuth": [] }],
            "parameters": [
                { "name": "username", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Expected username" },
                { "name": "password", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Expected password" }
            ],
            "responses": { "200": { "description": "Authenticated", "content": ok }, "401": unauthorized }
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

    fn basic_auth_header(user: &str, pass: &str) -> String {
        let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        format!("Basic {encoded}")
    }

    async fn json_body(resp: axum::http::Response<Body>) -> serde_json::Value {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&body).expect("json")
    }

    #[tokio::test]
    async fn default_creds_success() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/basic-auth")
                    .header("authorization", basic_auth_header("basic", "password"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["authenticated"], true);
        assert_eq!(json["auth_type"], "basic-auth");
        assert_eq!(json["username"], "basic");
    }

    #[tokio::test]
    async fn no_auth_header_returns_401() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/basic-auth")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(resp.headers().get("www-authenticate").is_some());
        let json = json_body(resp).await;
        assert_eq!(json["authenticated"], false);
        assert_eq!(json["error"], "unauthorized");
    }

    #[tokio::test]
    async fn wrong_password_returns_401() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/basic-auth")
                    .header("authorization", basic_auth_header("basic", "wrong"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn custom_creds_success() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/basic-auth/alice/secret")
                    .header("authorization", basic_auth_header("alice", "secret"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["authenticated"], true);
        assert_eq!(json["username"], "alice");
    }

    #[tokio::test]
    async fn lowercase_scheme_accepted() {
        let encoded = base64::engine::general_purpose::STANDARD.encode("basic:password");
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/auth/basic-auth")
                    .header("authorization", format!("basic {encoded}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn custom_creds_wrong_returns_401() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/basic-auth/alice/secret")
                    .header("authorization", basic_auth_header("alice", "wrong"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn post_method_works() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/basic-auth")
                    .header("authorization", basic_auth_header("basic", "password"))
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
                    .uri("/auth/basic-auth")
                    .header("authorization", basic_auth_header("basic", "password"))
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
