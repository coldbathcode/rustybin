use axum::{
    extract::Extension,
    http::{header, HeaderMap, StatusCode},
    response::Response,
    routing::any,
    Router,
};
use base64::Engine;
use serde::Serialize;
use std::sync::Arc;

use crate::config::Config;
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::jwt_state::JwtState;
use crate::types::{AuthFailure, AuthResponse};

// ── Response types ───────────────────────────────────────────────────

#[derive(Serialize)]
struct JwtExchangeResponse {
    authenticated: bool,
    auth_type: String,
    claims: serde_json::Value,
    exchanged_token: String,
}

// ── Helpers ─────────────────────────────────────────────────────────

fn extract_bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

fn base64url_decode(input: &str) -> Result<Vec<u8>, base64::DecodeError> {
    let input = input.trim_end_matches('=');
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(input)
}

fn decode_jwt_structure(
    token: &str,
) -> Result<(serde_json::Value, serde_json::Value), &'static str> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err("token must have exactly 3 parts");
    }

    let header_bytes = base64url_decode(parts[0]).map_err(|_| "invalid base64 in header")?;
    let payload_bytes = base64url_decode(parts[1]).map_err(|_| "invalid base64 in payload")?;

    let jwt_header: serde_json::Value =
        serde_json::from_slice(&header_bytes).map_err(|_| "invalid JSON in header")?;
    let claims: serde_json::Value =
        serde_json::from_slice(&payload_bytes).map_err(|_| "invalid JSON in payload")?;

    Ok((jwt_header, claims))
}

fn unauthorized(headers: &HeaderMap) -> Response {
    negotiate_with_status(
        headers,
        &AuthFailure {
            authenticated: false,
            error: "unauthorized".to_string(),
        },
        StatusCode::UNAUTHORIZED,
    )
}

// ── Handlers ────────────────────────────────────────────────────────

async fn jwt_validate(headers: HeaderMap) -> Response {
    let Some(token) = extract_bearer(&headers) else {
        return unauthorized(&headers);
    };

    let (jwt_header, claims) = match decode_jwt_structure(token) {
        Ok(parts) => parts,
        Err(_) => return unauthorized(&headers),
    };

    negotiate(
        &headers,
        &AuthResponse {
            authenticated: true,
            auth_type: "jwt".to_string(),
            username: None,
            header: None,
            claims: Some(claims),
            jwt_header: Some(jwt_header),
            client_dn: None,
            client_ca: None,
        },
    )
}

async fn jwt_exchange(Extension(jwt_state): Extension<Arc<JwtState>>, headers: HeaderMap) -> Response {
    let Some(token) = extract_bearer(&headers) else {
        return unauthorized(&headers);
    };

    let (_, claims) = match decode_jwt_structure(token) {
        Ok(parts) => parts,
        Err(_) => return unauthorized(&headers),
    };

    // Build new claims inheriting from incoming token
    let mut new_claims = match claims.as_object() {
        Some(obj) => obj.clone(),
        None => serde_json::Map::new(),
    };

    let now = chrono::Utc::now().timestamp();
    new_claims.insert("iss".to_string(), serde_json::json!("rustybin"));
    new_claims.insert("iat".to_string(), serde_json::json!(now));
    new_claims.insert(
        "jti".to_string(),
        serde_json::json!(uuid::Uuid::new_v4().to_string()),
    );
    new_claims.insert("exp".to_string(), serde_json::json!(now + 3600));

    // Sign with HS256
    let exchanged_token = match jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &new_claims,
        &jsonwebtoken::EncodingKey::from_secret(jwt_state.hs256_secret.as_bytes()),
    ) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("failed to sign exchanged JWT: {e}");
            return negotiate_with_status(
                &headers,
                &crate::types::ErrorResponse {
                    error: "token_signing_failed".to_string(),
                    details: Some(e.to_string()),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let claims_value = serde_json::Value::Object(new_claims);

    negotiate(
        &headers,
        &JwtExchangeResponse {
            authenticated: true,
            auth_type: "jwt".to_string(),
            claims: claims_value,
            exchanged_token,
        },
    )
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(jwt_state: Arc<JwtState>) -> Router<Arc<Config>> {
    Router::new()
        .route("/auth/jwt", any(jwt_validate))
        .route("/auth/jwt/exchange", any(jwt_exchange))
        .layer(Extension(jwt_state))
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

    fn test_jwt_state() -> Arc<JwtState> {
        Arc::new(JwtState {
            hs256_secret: "test-secret".to_string(),
            rs256_encoding_key: jsonwebtoken::EncodingKey::from_secret(b"unused"),
            rs256_decoding_key: jsonwebtoken::DecodingKey::from_secret(b"unused"),
            rs256_jwk: serde_json::json!({}),
        })
    }

    fn test_app() -> Router {
        router(test_jwt_state()).with_state(test_config())
    }

    /// Build a minimal structurally-valid JWT (no real signature).
    fn make_test_jwt(header: &serde_json::Value, payload: &serde_json::Value) -> String {
        let b64url = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let h = b64url.encode(serde_json::to_vec(header).expect("header json"));
        let p = b64url.encode(serde_json::to_vec(payload).expect("payload json"));
        format!("{h}.{p}.fakesig")
    }

    fn default_test_jwt() -> String {
        make_test_jwt(
            &serde_json::json!({"alg": "HS256", "typ": "JWT"}),
            &serde_json::json!({"sub": "1234", "name": "Test User"}),
        )
    }

    async fn json_body(resp: axum::http::Response<Body>) -> serde_json::Value {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&body).expect("json")
    }

    #[tokio::test]
    async fn jwt_validate_success() {
        let app = test_app();
        let token = default_test_jwt();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/jwt")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["authenticated"], true);
        assert_eq!(json["auth_type"], "jwt");
        assert_eq!(json["claims"]["sub"], "1234");
        assert_eq!(json["claims"]["name"], "Test User");
        assert_eq!(json["jwt_header"]["alg"], "HS256");
    }

    #[tokio::test]
    async fn jwt_validate_no_auth_returns_401() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/jwt")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let json = json_body(resp).await;
        assert_eq!(json["authenticated"], false);
    }

    #[tokio::test]
    async fn jwt_validate_bad_token_returns_401() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/jwt")
                    .header("authorization", "Bearer not.a.valid-jwt!!!")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn jwt_validate_two_parts_returns_401() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/jwt")
                    .header("authorization", "Bearer header.payload")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn jwt_validate_not_bearer_returns_401() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/jwt")
                    .header("authorization", "Basic dGVzdDp0ZXN0")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn jwt_exchange_success() {
        let app = test_app();
        let token = default_test_jwt();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/jwt/exchange")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["authenticated"], true);
        assert_eq!(json["auth_type"], "jwt");
        assert_eq!(json["claims"]["iss"], "rustybin");
        assert!(json["claims"]["iat"].is_i64());
        assert!(json["claims"]["jti"].is_string());
        assert!(json["claims"]["exp"].is_i64());
        // Inherited claims
        assert_eq!(json["claims"]["sub"], "1234");
        assert_eq!(json["claims"]["name"], "Test User");
        // Exchanged token is present
        let exchanged = json["exchanged_token"].as_str().expect("token string");
        assert_eq!(exchanged.split('.').count(), 3);
    }

    #[tokio::test]
    async fn jwt_exchange_no_auth_returns_401() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/jwt/exchange")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn jwt_validate_post_method() {
        let app = test_app();
        let token = default_test_jwt();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/jwt")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn jwt_validate_xml_negotiation() {
        let app = test_app();
        let token = default_test_jwt();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/jwt")
                    .header("authorization", format!("Bearer {token}"))
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
