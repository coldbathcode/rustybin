use axum::{
    extract::{Extension, Form, Query},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use base64::Engine;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::config::Config;
use crate::content_negotiation::negotiate;
use crate::jwt_state::JwtState;

// ── Types ───────────────────────────────────────────────────────────

#[derive(Clone)]
#[allow(dead_code)]
struct AuthCodeEntry {
    sub: String,
    name: String,
    email: String,
    scope: String,
    redirect_uri: String,
    client_id: String,
    created_at: i64,
}

struct OidcState {
    jwt: Arc<JwtState>,
    auth_codes: Mutex<HashMap<String, AuthCodeEntry>>,
}

#[derive(Serialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: i64,
    id_token: String,
    scope: String,
}

#[derive(Serialize)]
struct UserinfoResponse {
    sub: String,
    name: String,
    email: String,
    email_verified: bool,
}

#[derive(Serialize)]
struct IntrospectActive {
    active: bool,
    sub: String,
    scope: String,
    exp: i64,
    client_id: String,
}

#[derive(Serialize)]
struct IntrospectInactive {
    active: bool,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct TokenRequest {
    grant_type: String,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    // RFC 8693 Token Exchange fields
    #[serde(default)]
    subject_token: Option<String>,
    #[serde(default)]
    subject_token_type: Option<String>,
    #[serde(default)]
    actor_token: Option<String>,
    #[serde(default)]
    actor_token_type: Option<String>,
    #[serde(default)]
    audience: Option<String>,
    #[serde(default)]
    resource: Option<String>,
    #[serde(default)]
    requested_token_type: Option<String>,
}

#[derive(Deserialize)]
struct AuthorizeQuery {
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    response_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Deserialize)]
struct AuthorizeForm {
    username: String,
    #[allow(dead_code)]
    password: String,
    client_id: String,
    redirect_uri: String,
    #[allow(dead_code)]
    response_type: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct IntrospectRequest {
    token: String,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
}

// ── RFC 8693 Token Exchange types ────────────────────────────────────

const TOKEN_TYPE_ACCESS_TOKEN: &str = "urn:ietf:params:oauth:token-type:access_token";
const TOKEN_TYPE_ID_TOKEN: &str = "urn:ietf:params:oauth:token-type:id_token";
const TOKEN_TYPE_JWT: &str = "urn:ietf:params:oauth:token-type:jwt";
const TOKEN_TYPE_REFRESH_TOKEN: &str = "urn:ietf:params:oauth:token-type:refresh_token";

const VALID_TOKEN_TYPES: &[&str] = &[
    TOKEN_TYPE_ACCESS_TOKEN,
    TOKEN_TYPE_ID_TOKEN,
    TOKEN_TYPE_JWT,
    TOKEN_TYPE_REFRESH_TOKEN,
];

#[derive(Serialize)]
struct TokenExchangeResponse {
    access_token: String,
    issued_token_type: String,
    token_type: String,
    expires_in: i64,
    scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id_token: Option<String>,
}

// ── Helpers ─────────────────────────────────────────────────────────

fn derive_issuer(headers: &HeaderMap) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    format!("{scheme}://{host}")
}

fn extract_client_credentials(headers: &HeaderMap, form: &TokenRequest) -> (String, String) {
    // Try Basic Auth first
    if let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
    {
        if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(auth.trim()) {
            if let Ok(cred_str) = String::from_utf8(decoded) {
                if let Some((id, secret)) = cred_str.split_once(':') {
                    return (id.to_string(), secret.to_string());
                }
            }
        }
    }
    // Fall back to form body
    (
        form.client_id.clone().unwrap_or_default(),
        form.client_secret.clone().unwrap_or_default(),
    )
}

fn sign_rs256_token(
    jwt_state: &JwtState,
    claims: &serde_json::Map<String, serde_json::Value>,
) -> Result<String, jsonwebtoken::errors::Error> {
    let mut hdr = jsonwebtoken::Header::new(Algorithm::RS256);
    hdr.kid = Some("rustybin-rs256-key".to_string());
    jsonwebtoken::encode(&hdr, claims, &jwt_state.rs256_encoding_key)
}

fn build_token_claims(
    issuer: &str,
    sub: &str,
    aud: &str,
    scope: &str,
    extra: Option<(&str, &str)>,
) -> serde_json::Map<String, serde_json::Value> {
    let now = chrono::Utc::now().timestamp();
    let mut claims = serde_json::Map::new();
    claims.insert("iss".into(), serde_json::json!(issuer));
    claims.insert("sub".into(), serde_json::json!(sub));
    claims.insert("aud".into(), serde_json::json!(aud));
    claims.insert("exp".into(), serde_json::json!(now + 3600));
    claims.insert("iat".into(), serde_json::json!(now));
    claims.insert(
        "jti".into(),
        serde_json::json!(uuid::Uuid::new_v4().to_string()),
    );
    claims.insert("scope".into(), serde_json::json!(scope));
    claims.insert("token_type".into(), serde_json::json!("bearer"));
    if let Some((name_key, email_key)) = extra {
        claims.insert("name".into(), serde_json::json!(name_key));
        claims.insert("email".into(), serde_json::json!(email_key));
    }
    claims
}

fn verify_token(jwt_state: &JwtState, token: &str) -> Option<serde_json::Value> {
    // Try RS256
    let mut val = Validation::new(Algorithm::RS256);
    val.validate_aud = false;
    if let Ok(data) =
        jsonwebtoken::decode::<serde_json::Value>(token, &jwt_state.rs256_decoding_key, &val)
    {
        return Some(data.claims);
    }

    // Try HS256
    let mut val = Validation::new(Algorithm::HS256);
    val.validate_aud = false;
    let hs_key = DecodingKey::from_secret(jwt_state.hs256_secret.as_bytes());
    if let Ok(data) = jsonwebtoken::decode::<serde_json::Value>(token, &hs_key, &val) {
        return Some(data.claims);
    }

    None
}

fn cleanup_expired_codes(codes: &mut HashMap<String, AuthCodeEntry>) {
    let now = chrono::Utc::now().timestamp();
    codes.retain(|_, entry| now - entry.created_at < 60);
}

/// Decode a JWT by verifying signature first (RS256/HS256), falling back to
/// structural-only decode for third-party tokens.
fn decode_token_best_effort(
    jwt_state: &JwtState,
    token: &str,
) -> Option<serde_json::Value> {
    // Try verified decode first
    if let Some(claims) = verify_token(jwt_state, token) {
        return Some(claims);
    }
    // Fall back to structural decode (split on '.', base64url-decode)
    structural_decode_jwt(token)
}

fn structural_decode_jwt(token: &str) -> Option<serde_json::Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = parts[1].trim_end_matches('=');
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn is_valid_token_type(t: &str) -> bool {
    VALID_TOKEN_TYPES.contains(&t)
}

// ── Handlers ────────────────────────────────────────────────────────

async fn discovery(headers: HeaderMap) -> Response {
    let base = derive_issuer(&headers);
    let doc = serde_json::json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "userinfo_endpoint": format!("{base}/oauth/userinfo"),
        "jwks_uri": format!("{base}/oauth/jwks"),
        "introspection_endpoint": format!("{base}/oauth/introspect"),
        "response_types_supported": ["code", "token", "id_token", "code id_token"],
        "grant_types_supported": ["authorization_code", "client_credentials", "password", "urn:ietf:params:oauth:grant-type:token-exchange"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256", "HS256"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
        "scopes_supported": ["openid", "profile", "email"],
        "claims_supported": ["sub", "iss", "aud", "exp", "iat", "name", "email"],
    });
    negotiate(&headers, &doc)
}

async fn jwks(Extension(oidc): Extension<Arc<OidcState>>, headers: HeaderMap) -> Response {
    let keys = serde_json::json!({
        "keys": [oidc.jwt.rs256_jwk.clone()]
    });
    negotiate(&headers, &keys)
}

async fn token(
    Extension(oidc): Extension<Arc<OidcState>>,
    headers: HeaderMap,
    Form(form): Form<TokenRequest>,
) -> Response {
    let issuer = derive_issuer(&headers);
    let (client_id, _client_secret) = extract_client_credentials(&headers, &form);

    match form.grant_type.as_str() {
        "client_credentials" => {
            if client_id != "rustybin" {
                tracing::warn!(client_id = %client_id, "non-default client_id used in token request");
            }
            let scope = form.scope.as_deref().unwrap_or("openid");
            let claims = build_token_claims(&issuer, &client_id, &client_id, scope, None);
            issue_token_response(&oidc.jwt, &claims, scope, &headers)
        }
        "password" => {
            let username = form.username.as_deref().unwrap_or("demo");
            let name = format!("{} User", capitalize(username));
            let email = format!("{username}@rustybin.local");
            let scope = form.scope.as_deref().unwrap_or("openid profile email");
            let aud = if client_id.is_empty() {
                "rustybin"
            } else {
                &client_id
            };
            let claims =
                build_token_claims(&issuer, username, aud, scope, Some((&name, &email)));
            issue_token_response(&oidc.jwt, &claims, scope, &headers)
        }
        "authorization_code" => {
            let Some(code) = form.code.as_deref() else {
                return error_response(&headers, "invalid_request", "missing code parameter");
            };

            let entry = {
                let mut codes = oidc.auth_codes.lock().unwrap_or_else(|e| e.into_inner());
                cleanup_expired_codes(&mut codes);
                codes.remove(code)
            };

            let Some(entry) = entry else {
                return error_response(&headers, "invalid_grant", "invalid or expired authorization code");
            };

            let claims = build_token_claims(
                &issuer,
                &entry.sub,
                &entry.client_id,
                &entry.scope,
                Some((&entry.name, &entry.email)),
            );
            issue_token_response(&oidc.jwt, &claims, &entry.scope, &headers)
        }
        // RFC 8693 Token Exchange
        "urn:ietf:params:oauth:grant-type:token-exchange" => {
            handle_token_exchange(&oidc, &headers, &form, &issuer, &client_id)
        }
        _ => error_response(&headers, "unsupported_grant_type", "supported: client_credentials, password, authorization_code, urn:ietf:params:oauth:grant-type:token-exchange"),
    }
}

fn issue_token_response(
    jwt_state: &JwtState,
    claims: &serde_json::Map<String, serde_json::Value>,
    scope: &str,
    headers: &HeaderMap,
) -> Response {
    let access_token = match sign_rs256_token(jwt_state, claims) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("failed to sign token: {e}");
            return error_response(headers, "server_error", &e.to_string());
        }
    };
    // id_token is the same signed JWT for this demo
    let id_token = access_token.clone();

    negotiate(
        headers,
        &TokenResponse {
            access_token,
            token_type: "Bearer".to_string(),
            expires_in: 3600,
            id_token,
            scope: scope.to_string(),
        },
    )
}

fn error_response(headers: &HeaderMap, error: &str, description: &str) -> Response {
    let body = serde_json::json!({
        "error": error,
        "error_description": description,
    });
    crate::content_negotiation::negotiate_with_status(headers, &body, StatusCode::BAD_REQUEST)
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

async fn authorize_get(Query(q): Query<AuthorizeQuery>) -> Html<String> {
    let client_id = q.client_id.as_deref().unwrap_or("rustybin");
    let redirect_uri = q.redirect_uri.as_deref().unwrap_or("");
    let response_type = q.response_type.as_deref().unwrap_or("code");
    let scope = q.scope.as_deref().unwrap_or("openid profile email");
    let state = q.state.as_deref().unwrap_or("");

    Html(format!(
        r#"<!DOCTYPE html>
<html>
<head><title>Rustybin OIDC Login</title>
<style>
body {{ font-family: sans-serif; max-width: 400px; margin: 80px auto; }}
input {{ display: block; width: 100%; margin: 8px 0; padding: 8px; box-sizing: border-box; }}
button {{ padding: 10px 24px; margin-top: 12px; cursor: pointer; }}
h2 {{ text-align: center; }}
</style>
</head>
<body>
<h2>Rustybin OIDC Login</h2>
<form method="POST" action="/oauth/authorize">
  <label>Username</label>
  <input name="username" value="demo" />
  <label>Password</label>
  <input name="password" type="password" value="demo" />
  <input type="hidden" name="client_id" value="{client_id}" />
  <input type="hidden" name="redirect_uri" value="{redirect_uri}" />
  <input type="hidden" name="response_type" value="{response_type}" />
  <input type="hidden" name="scope" value="{scope}" />
  <input type="hidden" name="state" value="{state}" />
  <button type="submit">Authorize</button>
</form>
</body>
</html>"#
    ))
}

async fn authorize_post(
    Extension(oidc): Extension<Arc<OidcState>>,
    Form(form): Form<AuthorizeForm>,
) -> Response {
    let code = uuid::Uuid::new_v4().to_string();
    let name = format!("{} User", capitalize(&form.username));
    let email = format!("{}@rustybin.local", form.username);
    let scope = form.scope.unwrap_or_else(|| "openid profile email".to_string());

    let entry = AuthCodeEntry {
        sub: form.username,
        name,
        email,
        scope,
        redirect_uri: form.redirect_uri.clone(),
        client_id: form.client_id,
        created_at: chrono::Utc::now().timestamp(),
    };

    {
        let mut codes = oidc.auth_codes.lock().unwrap_or_else(|e| e.into_inner());
        cleanup_expired_codes(&mut codes);
        codes.insert(code.clone(), entry);
    }

    // Build redirect URL
    let separator = if form.redirect_uri.contains('?') {
        '&'
    } else {
        '?'
    };
    let mut redirect_url = format!("{}{}code={}", form.redirect_uri, separator, code);
    if let Some(state) = form.state {
        if !state.is_empty() {
            redirect_url.push_str(&format!("&state={state}"));
        }
    }

    Redirect::to(&redirect_url).into_response()
}

async fn userinfo(Extension(oidc): Extension<Arc<OidcState>>, headers: HeaderMap) -> Response {
    let token = match headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        Some(t) => t,
        None => {
            return crate::content_negotiation::negotiate_with_status(
                &headers,
                &serde_json::json!({"error": "unauthorized"}),
                StatusCode::UNAUTHORIZED,
            );
        }
    };

    let Some(claims) = verify_token(&oidc.jwt, token) else {
        return crate::content_negotiation::negotiate_with_status(
            &headers,
            &serde_json::json!({"error": "invalid_token"}),
            StatusCode::UNAUTHORIZED,
        );
    };

    let sub = claims["sub"].as_str().unwrap_or("unknown").to_string();
    let name = claims["name"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{} User", capitalize(&sub)));
    let email = claims["email"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{sub}@rustybin.local"));

    negotiate(
        &headers,
        &UserinfoResponse {
            sub,
            name,
            email,
            email_verified: true,
        },
    )
}

async fn introspect(
    Extension(oidc): Extension<Arc<OidcState>>,
    headers: HeaderMap,
    Form(form): Form<IntrospectRequest>,
) -> Response {
    match verify_token(&oidc.jwt, &form.token) {
        Some(claims) => {
            let sub = claims["sub"].as_str().unwrap_or("unknown").to_string();
            let scope = claims["scope"].as_str().unwrap_or("openid").to_string();
            let exp = claims["exp"].as_i64().unwrap_or(0);
            let client_id = form
                .client_id
                .unwrap_or_else(|| claims["aud"].as_str().unwrap_or("unknown").to_string());

            negotiate(
                &headers,
                &IntrospectActive {
                    active: true,
                    sub,
                    scope,
                    exp,
                    client_id,
                },
            )
        }
        None => negotiate(&headers, &IntrospectInactive { active: false }),
    }
}

// ── RFC 8693 Token Exchange ──────────────────────────────────────────

fn handle_token_exchange(
    oidc: &OidcState,
    headers: &HeaderMap,
    form: &TokenRequest,
    issuer: &str,
    client_id: &str,
) -> Response {
    // Validate required fields
    let Some(subject_token) = form.subject_token.as_deref() else {
        return error_response(headers, "invalid_request", "missing required parameter: subject_token");
    };
    let Some(subject_token_type) = form.subject_token_type.as_deref() else {
        return error_response(headers, "invalid_request", "missing required parameter: subject_token_type");
    };

    // Validate subject_token_type
    if !is_valid_token_type(subject_token_type) {
        return error_response(
            headers,
            "invalid_request",
            &format!("unsupported subject_token_type: {subject_token_type}"),
        );
    }

    // Validate actor_token_type if actor_token is present
    if let Some(_actor_token) = form.actor_token.as_deref() {
        match form.actor_token_type.as_deref() {
            None => {
                return error_response(
                    headers,
                    "invalid_request",
                    "actor_token_type is required when actor_token is present",
                );
            }
            Some(att) if !is_valid_token_type(att) => {
                return error_response(
                    headers,
                    "invalid_request",
                    &format!("unsupported actor_token_type: {att}"),
                );
            }
            _ => {}
        }
    }

    // Validate resource URI if provided
    if let Some(resource) = form.resource.as_deref() {
        if !resource.starts_with("http://") && !resource.starts_with("https://") {
            return error_response(headers, "invalid_target", "resource must be an absolute URI");
        }
    }

    // Decode subject token (verified or structural fallback)
    let subject_claims = decode_token_best_effort(&oidc.jwt, subject_token)
        .unwrap_or_else(|| serde_json::json!({}));

    // Decode actor token if present
    let actor_claims = form
        .actor_token
        .as_deref()
        .and_then(|at| decode_token_best_effort(&oidc.jwt, at));

    // Build new token claims
    let now = chrono::Utc::now().timestamp();
    let sub = subject_claims["sub"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();

    let default_client = if client_id.is_empty() {
        "rustybin"
    } else {
        client_id
    };
    let aud = form
        .audience
        .clone()
        .or_else(|| subject_claims["aud"].as_str().map(String::from))
        .unwrap_or_else(|| default_client.to_string());

    let scope = form
        .scope
        .clone()
        .or_else(|| subject_claims["scope"].as_str().map(String::from))
        .unwrap_or_else(|| "openid".to_string());

    let mut claims = serde_json::Map::new();
    claims.insert("iss".into(), serde_json::json!(issuer));
    claims.insert("sub".into(), serde_json::json!(sub));
    claims.insert("aud".into(), serde_json::json!(aud));
    claims.insert("exp".into(), serde_json::json!(now + 3600));
    claims.insert("iat".into(), serde_json::json!(now));
    claims.insert(
        "jti".into(),
        serde_json::json!(uuid::Uuid::new_v4().to_string()),
    );
    claims.insert("scope".into(), serde_json::json!(scope));
    claims.insert("token_type".into(), serde_json::json!("bearer"));

    // RFC 8693 §4.1 — delegation: include act claim with actor's sub
    if let Some(ref actor) = actor_claims {
        let actor_sub = actor["sub"].as_str().unwrap_or("unknown");
        claims.insert("act".into(), serde_json::json!({"sub": actor_sub}));
    }

    // Rustybin-specific: include original claims for debugging
    claims.insert("original_claims".into(), subject_claims);

    // Sign the new token
    let token = match sign_rs256_token(&oidc.jwt, &claims) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("failed to sign token-exchange JWT: {e}");
            return error_response(headers, "server_error", &e.to_string());
        }
    };

    // Determine issued_token_type
    let requested = form
        .requested_token_type
        .as_deref()
        .unwrap_or(TOKEN_TYPE_ACCESS_TOKEN);

    let issued_token_type = if requested == TOKEN_TYPE_ID_TOKEN {
        TOKEN_TYPE_ID_TOKEN
    } else {
        TOKEN_TYPE_ACCESS_TOKEN
    };

    let id_token = if issued_token_type == TOKEN_TYPE_ID_TOKEN {
        Some(token.clone())
    } else {
        None
    };

    negotiate(
        headers,
        &TokenExchangeResponse {
            access_token: token,
            issued_token_type: issued_token_type.to_string(),
            token_type: "Bearer".to_string(),
            expires_in: 3600,
            scope,
            id_token,
        },
    )
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(jwt_state: Arc<JwtState>) -> Router<Arc<Config>> {
    let oidc_state = Arc::new(OidcState {
        jwt: jwt_state,
        auth_codes: Mutex::new(HashMap::new()),
    });

    Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/oauth/token", post(token))
        .route("/oauth/jwks", get(jwks))
        .route(
            "/oauth/authorize",
            get(authorize_get).post(authorize_post),
        )
        .route("/oauth/userinfo", get(userinfo))
        .route("/oauth/introspect", post(introspect))
        .layer(Extension(oidc_state))
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
        Arc::new(JwtState::generate())
    }

    fn test_app() -> (Router, Arc<JwtState>) {
        let jwt = test_jwt_state();
        let app = router(jwt.clone()).with_state(test_config());
        (app, jwt)
    }

    async fn json_body(resp: axum::http::Response<Body>) -> serde_json::Value {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&body).expect("json")
    }

    #[tokio::test]
    async fn discovery_returns_valid_doc() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/.well-known/openid-configuration")
                    .header("host", "localhost")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["issuer"], "http://localhost");
        assert_eq!(
            json["token_endpoint"],
            "http://localhost/oauth/token"
        );
        assert!(json["scopes_supported"].is_array());
    }

    #[tokio::test]
    async fn jwks_returns_keys() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/oauth/jwks")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        let keys = json["keys"].as_array().expect("keys array");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0]["kty"], "RSA");
        assert_eq!(keys[0]["alg"], "RS256");
        assert!(keys[0]["n"].is_string());
        assert!(keys[0]["e"].is_string());
    }

    #[tokio::test]
    async fn token_client_credentials() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("host", "localhost")
                    .body(Body::from(
                        "grant_type=client_credentials&client_id=rustybin&client_secret=secret",
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["token_type"], "Bearer");
        assert_eq!(json["expires_in"], 3600);
        assert!(json["access_token"].is_string());
        assert!(json["id_token"].is_string());
        // access_token should be a valid 3-part JWT
        let token = json["access_token"].as_str().expect("token");
        assert_eq!(token.split('.').count(), 3);
    }

    #[tokio::test]
    async fn token_password_grant() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("host", "localhost")
                    .body(Body::from(
                        "grant_type=password&username=alice&password=pass&client_id=rustybin&client_secret=secret",
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert!(json["access_token"].is_string());
        assert_eq!(json["scope"], "openid profile email");
    }

    #[tokio::test]
    async fn token_unsupported_grant() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("grant_type=magic&client_id=x&client_secret=y"))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = json_body(resp).await;
        assert_eq!(json["error"], "unsupported_grant_type");
    }

    #[tokio::test]
    async fn token_basic_auth_credentials() {
        let (app, _) = test_app();
        let creds = base64::engine::general_purpose::STANDARD.encode("rustybin:secret");
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("authorization", format!("Basic {creds}"))
                    .header("host", "localhost")
                    .body(Body::from("grant_type=client_credentials"))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert!(json["access_token"].is_string());
    }

    #[tokio::test]
    async fn userinfo_with_valid_token() {
        let (app, jwt) = test_app();

        // First get a token
        let claims = build_token_claims(
            "http://localhost",
            "testuser",
            "rustybin",
            "openid profile email",
            Some(("Test User", "testuser@rustybin.local")),
        );
        let token = sign_rs256_token(&jwt, &claims).expect("sign");

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/oauth/userinfo")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["sub"], "testuser");
        assert_eq!(json["name"], "Test User");
        assert_eq!(json["email"], "testuser@rustybin.local");
        assert_eq!(json["email_verified"], true);
    }

    #[tokio::test]
    async fn userinfo_no_token_returns_401() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/oauth/userinfo")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn introspect_active_token() {
        let (app, jwt) = test_app();

        let claims = build_token_claims(
            "http://localhost",
            "testuser",
            "rustybin",
            "openid",
            None,
        );
        let token = sign_rs256_token(&jwt, &claims).expect("sign");

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/introspect")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "token={token}&client_id=rustybin&client_secret=secret"
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["active"], true);
        assert_eq!(json["sub"], "testuser");
    }

    #[tokio::test]
    async fn introspect_invalid_token() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/introspect")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(
                        "token=invalid.token.here&client_id=rustybin&client_secret=secret",
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["active"], false);
    }

    #[tokio::test]
    async fn authorize_get_returns_html() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/oauth/authorize?client_id=rustybin&redirect_uri=http://example.com/cb&response_type=code&scope=openid&state=xyz")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let html = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(html.contains("Rustybin OIDC Login"));
        assert!(html.contains("rustybin"));
    }

    #[tokio::test]
    async fn authorize_post_redirects_with_code() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/authorize")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(
                        "username=demo&password=demo&client_id=rustybin&redirect_uri=http://example.com/cb&response_type=code&scope=openid&state=xyz",
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let location = resp
            .headers()
            .get("location")
            .expect("location")
            .to_str()
            .expect("str");
        assert!(location.starts_with("http://example.com/cb?code="));
        assert!(location.contains("state=xyz"));
    }

    #[tokio::test]
    async fn full_auth_code_flow() {
        let jwt = test_jwt_state();
        let oidc_state = Arc::new(OidcState {
            jwt: jwt.clone(),
            auth_codes: Mutex::new(HashMap::new()),
        });
        let config = test_config();

        // Step 1: POST authorize to get code
        let app = Router::new()
            .route(
                "/oauth/authorize",
                get(authorize_get).post(authorize_post),
            )
            .route("/oauth/token", post(token))
            .layer(Extension(oidc_state))
            .with_state(config);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/authorize")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(
                        "username=demo&password=demo&client_id=rustybin&redirect_uri=http://example.com/cb&response_type=code&scope=openid+profile+email",
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        let location = resp
            .headers()
            .get("location")
            .expect("location")
            .to_str()
            .expect("str")
            .to_string();

        // Extract code from redirect URL
        let code = location
            .split("code=")
            .nth(1)
            .expect("code in url")
            .split('&')
            .next()
            .expect("code value");

        // Step 2: Exchange code for token
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("host", "localhost")
                    .body(Body::from(format!(
                        "grant_type=authorization_code&code={code}&redirect_uri=http://example.com/cb&client_id=rustybin&client_secret=secret"
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert!(json["access_token"].is_string());
        assert_eq!(json["token_type"], "Bearer");
    }

    // ── RFC 8693 Token Exchange tests ─────────────────────────────

    fn get_test_token(jwt: &JwtState) -> String {
        let claims = build_token_claims(
            "http://localhost",
            "testuser",
            "rustybin",
            "openid profile",
            Some(("Test User", "testuser@rustybin.local")),
        );
        sign_rs256_token(jwt, &claims).expect("sign")
    }

    #[tokio::test]
    async fn token_exchange_success() {
        let (app, jwt) = test_app();
        let subject = get_test_token(&jwt);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("host", "localhost")
                    .body(Body::from(format!(
                        "grant_type=urn:ietf:params:oauth:grant-type:token-exchange&subject_token={subject}&subject_token_type=urn:ietf:params:oauth:token-type:access_token&audience=my-service"
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert!(json["access_token"].is_string());
        assert_eq!(
            json["issued_token_type"],
            "urn:ietf:params:oauth:token-type:access_token"
        );
        assert_eq!(json["token_type"], "Bearer");
        assert_eq!(json["expires_in"], 3600);
        assert_eq!(json["scope"], "openid profile");
    }

    #[tokio::test]
    async fn token_exchange_missing_subject_token() {
        let (app, _) = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(
                        "grant_type=urn:ietf:params:oauth:grant-type:token-exchange&subject_token_type=urn:ietf:params:oauth:token-type:access_token",
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = json_body(resp).await;
        assert_eq!(json["error"], "invalid_request");
    }

    #[tokio::test]
    async fn token_exchange_missing_subject_token_type() {
        let (app, jwt) = test_app();
        let subject = get_test_token(&jwt);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "grant_type=urn:ietf:params:oauth:grant-type:token-exchange&subject_token={subject}"
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = json_body(resp).await;
        assert_eq!(json["error"], "invalid_request");
    }

    #[tokio::test]
    async fn token_exchange_invalid_subject_token_type() {
        let (app, jwt) = test_app();
        let subject = get_test_token(&jwt);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "grant_type=urn:ietf:params:oauth:grant-type:token-exchange&subject_token={subject}&subject_token_type=invalid"
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = json_body(resp).await;
        assert_eq!(json["error"], "invalid_request");
    }

    #[tokio::test]
    async fn token_exchange_actor_missing_type() {
        let (app, jwt) = test_app();
        let subject = get_test_token(&jwt);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "grant_type=urn:ietf:params:oauth:grant-type:token-exchange&subject_token={subject}&subject_token_type=urn:ietf:params:oauth:token-type:access_token&actor_token=sometoken"
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = json_body(resp).await;
        assert_eq!(json["error"], "invalid_request");
        assert!(json["error_description"]
            .as_str()
            .unwrap_or("")
            .contains("actor_token_type"));
    }

    #[tokio::test]
    async fn token_exchange_with_actor_delegation() {
        let jwt = test_jwt_state();
        let oidc_state = Arc::new(OidcState {
            jwt: jwt.clone(),
            auth_codes: Mutex::new(HashMap::new()),
        });
        let config = test_config();
        let app = Router::new()
            .route("/oauth/token", post(token))
            .layer(Extension(oidc_state))
            .with_state(config);

        let subject = get_test_token(&jwt);

        // Create actor token with different sub
        let actor_claims = build_token_claims(
            "http://localhost",
            "admin",
            "rustybin",
            "openid",
            Some(("Admin User", "admin@rustybin.local")),
        );
        let actor = sign_rs256_token(&jwt, &actor_claims).expect("sign actor");

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("host", "localhost")
                    .body(Body::from(format!(
                        "grant_type=urn:ietf:params:oauth:grant-type:token-exchange&subject_token={subject}&subject_token_type=urn:ietf:params:oauth:token-type:access_token&actor_token={actor}&actor_token_type=urn:ietf:params:oauth:token-type:access_token"
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert!(json["access_token"].is_string());

        // Decode the exchanged token and check for act claim
        let exchanged = json["access_token"].as_str().expect("token");
        let claims = verify_token(&jwt, exchanged).expect("verify exchanged token");
        assert_eq!(claims["act"]["sub"], "admin");
        assert_eq!(claims["sub"], "testuser");
    }

    #[tokio::test]
    async fn token_exchange_requested_id_token() {
        let (app, jwt) = test_app();
        let subject = get_test_token(&jwt);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("host", "localhost")
                    .body(Body::from(format!(
                        "grant_type=urn:ietf:params:oauth:grant-type:token-exchange&subject_token={subject}&subject_token_type=urn:ietf:params:oauth:token-type:access_token&requested_token_type=urn:ietf:params:oauth:token-type:id_token"
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(
            json["issued_token_type"],
            "urn:ietf:params:oauth:token-type:id_token"
        );
        // id_token field should be present
        assert!(json["id_token"].is_string());
    }

    #[tokio::test]
    async fn token_exchange_third_party_jwt() {
        let (app, _) = test_app();

        // Create a structurally valid JWT that can't be signature-verified
        let b64url = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header = b64url.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = b64url.encode(br#"{"sub":"external-user","iss":"external-idp","scope":"read write"}"#);
        let third_party_jwt = format!("{header}.{payload}.fakesignature");

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("host", "localhost")
                    .body(Body::from(format!(
                        "grant_type=urn:ietf:params:oauth:grant-type:token-exchange&subject_token={third_party_jwt}&subject_token_type=urn:ietf:params:oauth:token-type:jwt"
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert!(json["access_token"].is_string());
        assert_eq!(json["scope"], "read write");
    }

    #[tokio::test]
    async fn discovery_includes_token_exchange_grant() {
        let (app, _) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/.well-known/openid-configuration")
                    .header("host", "localhost")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        let json = json_body(resp).await;
        let grants = json["grant_types_supported"]
            .as_array()
            .expect("grants array");
        assert!(
            grants
                .iter()
                .any(|g| g == "urn:ietf:params:oauth:grant-type:token-exchange"),
            "discovery should include token-exchange grant"
        );
    }
}
