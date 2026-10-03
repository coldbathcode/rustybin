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
use crate::types::{AuthFailure, AuthResponse};

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

fn check_basic_auth(headers: &HeaderMap, expected_user: &str, expected_pass: &str) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let Some(auth_value) = auth_header else {
        return unauthorized(headers);
    };

    let Some(encoded) = auth_value.strip_prefix("Basic ") else {
        return unauthorized(headers);
    };

    let decoded = match base64::engine::general_purpose::STANDARD.decode(encoded.trim()) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => return unauthorized(headers),
        },
        Err(_) => return unauthorized(headers),
    };

    let Some((username, password)) = decoded.split_once(':') else {
        return unauthorized(headers);
    };

    if username != expected_user || password != expected_pass {
        return unauthorized(headers);
    }

    negotiate(
        headers,
        &AuthResponse {
            authenticated: true,
            auth_type: "basic-auth".to_string(),
            username: Some(username.to_string()),
            header: None,
            claims: None,
            jwt_header: None,
            client_dn: None,
            client_ca: None,
        },
    )
}

fn unauthorized(headers: &HeaderMap) -> Response {
    let body = negotiate_with_status(
        headers,
        &AuthFailure {
            authenticated: false,
            error: "unauthorized".to_string(),
        },
        StatusCode::UNAUTHORIZED,
    );

    // Add WWW-Authenticate header
    let (parts, body_inner) = body.into_parts();
    let mut resp = Response::from_parts(parts, body_inner);
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"rustybin\""),
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
