//! `/auth/jwt*`: real JWT validation (HS256 demo secret or RS256 IdP key),
//! a decode-only endpoint for "what did the gateway forward" demos, and an
//! exchange endpoint that re-signs a verified token.

use axum::{
    extract::{Extension, RawQuery},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::Response,
    routing::any,
    Router,
};
use serde::Serialize;
use std::sync::Arc;

use crate::catalog::{category, Endpoint, Example};
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::jwt_state::{bearer_token, decode_unverified, JwtState, VerifyOptions};
use crate::state::AppState;
use crate::types::{AuthFailure, AuthResponse};

/// Demo JWT: HS256 signed with the demo secret
/// ([`crate::jwt_state::HS256_DEMO_SECRET`]), `exp` in the year 2100, so it
/// validates on every instance. Claims: sub 1234567890, name John Doe,
/// iss rustybin, aud rustybin, scope "openid profile".
pub const SAMPLE_JWT: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaXNzIjoicnVzdHliaW4iLCJhdWQiOiJydXN0eWJpbiIsInNjb3BlIjoib3BlbmlkIHByb2ZpbGUiLCJpYXQiOjE1MTYyMzkwMjIsImV4cCI6NDEwMjQ0NDgwMH0.o8obKOeREkQXJtz3ZyqxTs5bvJBn9lxiYnL2NC9Npbw";

// ── Response types ───────────────────────────────────────────────────

#[derive(Serialize)]
struct JwtExchangeResponse {
    authenticated: bool,
    auth_type: String,
    claims: serde_json::Value,
    exchanged_token: String,
}

#[derive(Serialize)]
struct JwtDecodeResponse {
    /// Always false: this endpoint never validates.
    verified: bool,
    jwt_header: serde_json::Value,
    claims: serde_json::Value,
    /// `exp` is in the past (informational only).
    #[serde(skip_serializing_if = "Option::is_none")]
    expired: Option<bool>,
    note: &'static str,
}

// ── Helpers ─────────────────────────────────────────────────────────

/// `?iss=` / `?aud=` constraints.
#[derive(Default)]
struct Constraints {
    iss: Option<String>,
    aud: Option<String>,
}

fn constraints(query: Option<&str>) -> Constraints {
    let mut c = Constraints::default();
    for (k, v) in form_urlencoded::parse(query.unwrap_or("").as_bytes()) {
        match k.as_ref() {
            "iss" if !v.is_empty() => c.iss = Some(v.into_owned()),
            "aud" if !v.is_empty() => c.aud = Some(v.into_owned()),
            _ => {}
        }
    }
    c
}

/// 401 with an RFC 6750 `WWW-Authenticate` challenge. `reason` is always one
/// of our own fixed messages, never request input.
fn unauthorized(headers: &HeaderMap, reason: Option<&str>) -> Response {
    let (error, challenge) = match reason {
        Some(r) => (
            format!("invalid_token: {r}"),
            format!(
                "Bearer realm=\"rustybin\", error=\"invalid_token\", error_description=\"{}\"",
                r.replace(['"', '\\'], "'")
            ),
        ),
        None => (
            "unauthorized: missing Bearer token".to_string(),
            "Bearer realm=\"rustybin\"".to_string(),
        ),
    };
    let mut resp =
        negotiate_with_status(headers, &AuthFailure::new(error), StatusCode::UNAUTHORIZED);
    let value = HeaderValue::from_str(&challenge)
        .unwrap_or_else(|_| HeaderValue::from_static("Bearer realm=\"rustybin\""));
    resp.headers_mut().insert(header::WWW_AUTHENTICATE, value);
    resp
}

fn verify_request(
    jwt: &JwtState,
    headers: &HeaderMap,
    query: Option<&str>,
) -> Result<crate::jwt_state::VerifiedJwt, Box<Response>> {
    let Some(token) = bearer_token(headers) else {
        return Err(Box::new(unauthorized(headers, None)));
    };
    let c = constraints(query);
    let opts = VerifyOptions {
        issuer: c.iss.as_deref(),
        audience: c.aud.as_deref(),
    };
    jwt.verify(token, &opts)
        .map_err(|reason| Box::new(unauthorized(headers, Some(&reason))))
}

// ── Handlers ────────────────────────────────────────────────────────

async fn jwt_validate(
    Extension(jwt): Extension<Arc<JwtState>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let verified = match verify_request(&jwt, &headers, query.as_deref()) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let username = verified
        .claims
        .get("sub")
        .and_then(|s| s.as_str())
        .map(String::from);
    negotiate(
        &headers,
        &AuthResponse {
            username,
            claims: Some(verified.claims),
            jwt_header: Some(verified.header),
            ..AuthResponse::ok("jwt")
        },
    )
}

async fn jwt_decode(headers: HeaderMap) -> Response {
    let Some(token) = bearer_token(&headers) else {
        return unauthorized(&headers, None);
    };
    let (jwt_header, claims) = match decode_unverified(token) {
        Ok(parts) => parts,
        Err(reason) => {
            return negotiate_with_status(
                &headers,
                &AuthFailure::new(format!("malformed_token: {reason}")),
                StatusCode::BAD_REQUEST,
            )
        }
    };
    let now = chrono::Utc::now().timestamp();
    let expired = claims
        .get("exp")
        .and_then(|e| e.as_i64())
        .map(|exp| exp < now);
    negotiate(
        &headers,
        &JwtDecodeResponse {
            verified: false,
            jwt_header,
            claims,
            expired,
            note: "decoded WITHOUT signature or claim validation; use /auth/jwt to validate",
        },
    )
}

async fn jwt_exchange(
    Extension(jwt): Extension<Arc<JwtState>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let verified = match verify_request(&jwt, &headers, query.as_deref()) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };

    // New claims inherit the verified token's claims.
    let mut new_claims = verified.claims.as_object().cloned().unwrap_or_default();
    let now = chrono::Utc::now().timestamp();
    new_claims.remove("nbf");
    new_claims.insert("iss".to_string(), serde_json::json!("rustybin"));
    new_claims.insert("iat".to_string(), serde_json::json!(now));
    new_claims.insert(
        "jti".to_string(),
        serde_json::json!(uuid::Uuid::new_v4().to_string()),
    );
    new_claims.insert("exp".to_string(), serde_json::json!(now + 3600));

    let exchanged_token = match jwt.sign_hs256(&new_claims) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("failed to sign exchanged JWT: {e}");
            return negotiate_with_status(
                &headers,
                &crate::types::ErrorResponse {
                    error: "token_signing_failed".to_string(),
                    details: None,
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    negotiate(
        &headers,
        &JwtExchangeResponse {
            authenticated: true,
            auth_type: "jwt".to_string(),
            claims: serde_json::Value::Object(new_claims),
            exchanged_token,
        },
    )
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/auth/jwt", any(jwt_validate))
        .route("/auth/jwt/decode", any(jwt_decode))
        .route("/auth/jwt/exchange", any(jwt_exchange))
        .layer(Extension(state.jwt.clone()))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/auth/jwt",
            &["ANY"],
            category::AUTH_JWT,
            "Validate a Bearer JWT (HS256 demo secret or RS256 IdP key, exp/nbf, optional ?iss= ?aud=)",
        )
        .description(
            "Verifies the signature (alg from the header: HS256 with the demo secret \
             `rustybin-demo-secret-do-not-use-in-production`, RS256 with the key at /oauth/jwks; \
             alg none and other algorithms are rejected), requires exp, checks nbf, and \
             optionally ?iss= and ?aud=. 401 with WWW-Authenticate on failure.",
        )
        .example(Example::get("Validate JWT", "/auth/jwt").bearer(SAMPLE_JWT))
        .example(
            Example::get("Validate JWT with iss and aud", "/auth/jwt?iss=rustybin&aud=rustybin")
                .bearer(SAMPLE_JWT),
        ),
        Endpoint::new(
            "/auth/jwt/decode",
            &["ANY"],
            category::AUTH_JWT,
            "Decode a Bearer JWT WITHOUT validation (shows what the gateway forwarded)",
        )
        .description(
            "Returns the header and claims of any structurally valid JWT. Nothing is verified: \
             use it to inspect tokens a gateway injected or forwarded.",
        )
        .example(Example::get("Decode JWT (no validation)", "/auth/jwt/decode").bearer(SAMPLE_JWT)),
        Endpoint::new(
            "/auth/jwt/exchange",
            &["ANY"],
            category::AUTH_JWT,
            "Exchange a valid JWT for a new HS256-signed token (same checks as /auth/jwt)",
        )
        .example(Example::post("Exchange JWT", "/auth/jwt/exchange").bearer(SAMPLE_JWT)),
    ]
}

pub fn openapi_paths() -> serde_json::Value {
    use serde_json::json;
    let auth_ok =
        crate::openapi::json_xml_content(json!({ "$ref": "#/components/schemas/AuthResponse" }));
    let auth_fail =
        crate::openapi::json_xml_content(json!({ "$ref": "#/components/schemas/AuthFailure" }));
    let constraints = json!([
        { "name": "iss", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Required issuer" },
        { "name": "aud", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Required audience" }
    ]);
    json!({
        "/auth/jwt": { "get": {
            "tags": ["Auth"],
            "summary": "JWT validation",
            "description": "Verifies the Bearer JWT: HS256 with the demo secret or RS256 with the IdP key (selected by the header alg; none and other algorithms are rejected), exp (required), nbf, and the optional iss / aud query constraints.",
            "operationId": "getJwt",
            "security": [{ "bearerAuth": [] }],
            "parameters": constraints,
            "responses": {
                "200": { "description": "Token valid", "content": auth_ok },
                "401": { "description": "Missing or invalid token (WWW-Authenticate: Bearer error=\"invalid_token\")", "content": auth_fail }
            }
        }},
        "/auth/jwt/decode": { "get": {
            "tags": ["Auth"],
            "summary": "JWT decode (no validation)",
            "description": "Returns the header and claims of the Bearer JWT WITHOUT verifying signature or claims.",
            "operationId": "getJwtDecode",
            "security": [{ "bearerAuth": [] }],
            "responses": {
                "200": { "description": "Decoded token", "content": crate::openapi::json_xml_content(json!({
                    "type": "object",
                    "properties": {
                        "verified": { "type": "boolean", "example": false },
                        "jwt_header": { "type": "object" },
                        "claims": { "type": "object" },
                        "expired": { "type": "boolean" },
                        "note": { "type": "string" }
                    }
                })) },
                "400": { "description": "Malformed token", "content": auth_fail },
                "401": { "description": "Missing Bearer token", "content": auth_fail }
            }
        }},
        "/auth/jwt/exchange": { "post": {
            "tags": ["Auth"],
            "summary": "JWT token exchange",
            "description": "Validates the Bearer JWT like /auth/jwt, then returns a new HS256 token inheriting its claims (new iss, iat, jti, exp).",
            "operationId": "postJwtExchange",
            "security": [{ "bearerAuth": [] }],
            "parameters": constraints,
            "responses": {
                "200": { "description": "Exchanged token", "content": crate::openapi::json_xml_content(json!({
                    "type": "object",
                    "properties": {
                        "authenticated": { "type": "boolean" },
                        "auth_type": { "type": "string" },
                        "claims": { "type": "object" },
                        "exchanged_token": { "type": "string" }
                    }
                })) },
                "401": { "description": "Missing or invalid token", "content": auth_fail }
            }
        }}
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_json, module_app};
    use axum::body::Body;
    use axum::http::Request;
    use base64::Engine;
    use tower::ServiceExt;

    fn app() -> Router {
        module_app(router)
    }

    fn get(uri: &str, auth: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().uri(uri);
        if let Some(a) = auth {
            b = b.header("authorization", a);
        }
        b.body(Body::empty()).expect("request")
    }

    fn signed(claims: serde_json::Value) -> String {
        let jwt = JwtState::shared_for_tests();
        jwt.sign_hs256(claims.as_object().expect("object"))
            .expect("sign")
    }

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    #[tokio::test]
    async fn sample_jwt_validates() {
        let resp = app()
            .oneshot(get("/auth/jwt", Some(&format!("Bearer {SAMPLE_JWT}"))))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["authenticated"], true);
        assert_eq!(json["claims"]["name"], "John Doe");
        assert_eq!(json["jwt_header"]["alg"], "HS256");
        assert_eq!(json["username"], "1234567890");
    }

    #[tokio::test]
    async fn rs256_token_validates_with_lowercase_bearer() {
        let jwt = JwtState::shared_for_tests();
        let claims = serde_json::json!({"sub": "rs", "exp": now() + 60});
        let token = jwt
            .sign_rs256(claims.as_object().expect("object"), "JWT")
            .expect("sign");
        let resp = app()
            .oneshot(get("/auth/jwt", Some(&format!("bearer {token}"))))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["jwt_header"]["alg"], "RS256");
    }

    #[tokio::test]
    async fn forged_expired_and_none_are_rejected() {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let payload = b64.encode(format!(r#"{{"sub":"x","exp":{}}}"#, now() + 600));
        let forged = format!(
            "{}.{payload}.{}",
            b64.encode(r#"{"alg":"HS256","typ":"JWT"}"#),
            b64.encode("not-the-signature")
        );
        let none = format!("{}.{payload}.", b64.encode(r#"{"alg":"none"}"#));
        let expired = signed(serde_json::json!({"sub": "x", "exp": now() - 3600}));
        for (token, reason) in [
            (forged, "invalid signature"),
            (none, "alg none"),
            (expired, "token expired"),
        ] {
            let resp = app()
                .oneshot(get("/auth/jwt", Some(&format!("Bearer {token}"))))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{reason}");
            let challenge = resp
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            assert!(challenge.contains("invalid_token"), "{challenge}");
            let json = body_json(resp).await;
            assert!(
                json["error"].as_str().unwrap_or("").contains(reason),
                "{json}"
            );
        }
    }

    #[tokio::test]
    async fn iss_and_aud_constraints() {
        let auth = format!("Bearer {SAMPLE_JWT}");
        let ok = app()
            .oneshot(get("/auth/jwt?iss=rustybin&aud=rustybin", Some(&auth)))
            .await
            .expect("response");
        assert_eq!(ok.status(), StatusCode::OK);
        let bad = app()
            .oneshot(get("/auth/jwt?aud=other", Some(&auth)))
            .await
            .expect("response");
        assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
        let bad = app()
            .oneshot(get("/auth/jwt?iss=other", Some(&auth)))
            .await
            .expect("response");
        assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn missing_or_non_bearer_is_401() {
        for auth in [
            None,
            Some("Basic dGVzdDp0ZXN0"),
            Some("Bearer header.payload"),
        ] {
            let resp = app()
                .oneshot(get("/auth/jwt", auth))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
            assert!(resp.headers().contains_key("www-authenticate"));
        }
    }

    #[tokio::test]
    async fn decode_shows_unverified_token() {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let token = format!(
            "{}.{}.sig",
            b64.encode(r#"{"alg":"RS256","kid":"other"}"#),
            b64.encode(r#"{"sub":"someone","exp":1}"#)
        );
        let resp = app()
            .oneshot(get("/auth/jwt/decode", Some(&format!("Bearer {token}"))))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["verified"], false);
        assert_eq!(json["expired"], true);
        assert_eq!(json["claims"]["sub"], "someone");
        assert_eq!(json["jwt_header"]["kid"], "other");

        let resp = app()
            .oneshot(get("/auth/jwt/decode", Some("Bearer nope")))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn exchange_requires_valid_token() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/jwt/exchange")
                    .header("authorization", format!("Bearer {SAMPLE_JWT}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["claims"]["iss"], "rustybin");
        assert_eq!(json["claims"]["sub"], "1234567890");
        let exchanged = json["exchanged_token"].as_str().expect("token");
        let jwt = JwtState::shared_for_tests();
        assert!(jwt.verify(exchanged, &VerifyOptions::default()).is_ok());

        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let forged = format!(
            "{}.{}.sig",
            b64.encode(r#"{"alg":"HS256"}"#),
            b64.encode(format!(r#"{{"sub":"evil","exp":{}}}"#, now() + 60))
        );
        let resp = app()
            .oneshot(get("/auth/jwt/exchange", Some(&format!("Bearer {forged}"))))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn xml_negotiation() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/auth/jwt")
                    .header("authorization", format!("Bearer {SAMPLE_JWT}"))
                    .header("accept", "application/xml")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/xml")
        );
    }
}
