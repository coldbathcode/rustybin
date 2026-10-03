//! Built-in OAuth 2.0 / OpenID Connect provider for gateway demos.
//!
//! - Discovery: OIDC (`/.well-known/openid-configuration`) and RFC 8414
//!   (`/.well-known/oauth-authorization-server`), JWKS.
//! - Authorization code flow with a demo login page, PKCE (S256, plain),
//!   nonce, single-use codes; `resource` (RFC 8707) sets the access token
//!   audience.
//! - Token endpoint: authorization_code, refresh_token (rotating),
//!   client_credentials, password and token exchange (RFC 8693, subject
//!   tokens must verify with this server's keys).
//! - Userinfo, introspection (RFC 7662), revocation (RFC 7009) and dynamic
//!   client registration (RFC 7591).
//!
//! Demo clients: `rustybin` / `secret` (confidential, any http(s)
//! redirect URI) and `rustybin-public` (public, PKCE required). Demo users:
//! `demo`/`demo`, `alice`/`alice` (groups users, admins), `bob`/`bob`.
//! All state is bounded (capacity + TTL).

mod clients;
mod store;

use axum::{
    body::Bytes,
    extract::{Extension, RawQuery},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

use crate::catalog::{category, Endpoint, Example};
use crate::jwt_state::{bearer_token, JwtState, VerifyOptions};
use crate::landing::html_escape;
use crate::server::TlsConnectionInfo;
use crate::state::AppState;
use clients::{
    Client, ClientRegistry, DemoUser, GRANT_AUTHORIZATION_CODE, GRANT_CLIENT_CREDENTIALS,
    GRANT_PASSWORD, GRANT_REFRESH_TOKEN, GRANT_TOKEN_EXCHANGE,
};
use store::BoundedStore;

pub use clients::{DEMO_CLIENT_ID, DEMO_CLIENT_SECRET, PUBLIC_CLIENT_ID};

/// Audience of access tokens requested without `resource` / `audience`.
pub const DEFAULT_AUDIENCE: &str = "rustybin";

const ACCESS_TOKEN_TTL: i64 = 3600;
const AUTH_CODE_TTL: i64 = 300;
const REFRESH_TOKEN_TTL: i64 = 24 * 3600;
const CLIENT_TTL: i64 = 24 * 3600;
const MAX_RESOURCES: usize = 5;

const TOKEN_TYPE_ACCESS_TOKEN: &str = "urn:ietf:params:oauth:token-type:access_token";
const TOKEN_TYPE_ID_TOKEN: &str = "urn:ietf:params:oauth:token-type:id_token";
const TOKEN_TYPE_JWT: &str = "urn:ietf:params:oauth:token-type:jwt";

// ── State ───────────────────────────────────────────────────────────

#[derive(Clone)]
struct AuthCode {
    client_id: String,
    redirect_uri: String,
    /// The authorization request carried `redirect_uri`, so the token
    /// request must send the identical value (RFC 6749 4.1.3).
    redirect_uri_given: bool,
    username: String,
    scope: String,
    nonce: Option<String>,
    code_challenge: Option<(String, String)>,
    resources: Vec<String>,
    auth_time: i64,
}

#[derive(Clone)]
struct RefreshGrant {
    client_id: String,
    sub: String,
    username: Option<String>,
    scope: String,
    audience: Vec<String>,
    auth_time: Option<i64>,
}

struct OidcState {
    jwt: Arc<JwtState>,
    trust_forward: bool,
    clients: ClientRegistry,
    codes: BoundedStore<AuthCode>,
    refresh_tokens: BoundedStore<RefreshGrant>,
    /// Revoked access token `jti`s, kept until the token would expire.
    revoked: BoundedStore<()>,
}

impl OidcState {
    fn new(jwt: Arc<JwtState>, trust_forward: bool, public_mode: bool) -> Self {
        let (cap, clients_cap) = if public_mode {
            (2_000, 200)
        } else {
            (10_000, 1_000)
        };
        Self {
            jwt,
            trust_forward,
            clients: ClientRegistry::new(clients_cap, CLIENT_TTL),
            codes: BoundedStore::new(cap, AUTH_CODE_TTL),
            refresh_tokens: BoundedStore::new(cap, REFRESH_TOKEN_TTL),
            revoked: BoundedStore::new(cap, ACCESS_TOKEN_TTL + 120),
        }
    }

    fn is_revoked(&self, claims: &Value) -> bool {
        claims
            .get("jti")
            .and_then(|j| j.as_str())
            .is_some_and(|jti| self.revoked.contains(jti))
    }

    /// Verify a token issued by this server (RS256 IdP key or HS256 demo
    /// secret) that has not been revoked.
    fn verify(&self, token: &str) -> Result<crate::jwt_state::VerifiedJwt, String> {
        let verified = self.jwt.verify(token, &VerifyOptions::default())?;
        if self.is_revoked(&verified.claims) {
            return Err("token revoked".into());
        }
        Ok(verified)
    }
}

// ── Issuer ──────────────────────────────────────────────────────────

fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 255
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-:[]_".contains(&b))
}

fn valid_prefix(prefix: &str) -> bool {
    prefix.starts_with('/')
        && prefix.len() <= 200
        && !prefix.contains("//")
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-._~".contains(&b))
}

fn first_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// The issuer URL for a request: `https` on the TLS listener, the `Host`
/// header (validated, else `localhost`). `X-Forwarded-Proto`,
/// `X-Forwarded-Host` and `X-Forwarded-Prefix` are honoured only when
/// `trust_forward` (`RUSTYBIN_TRUST_FORWARD`) is set.
pub fn issuer_for(headers: &HeaderMap, is_tls: bool, trust_forward: bool) -> String {
    let mut scheme = if is_tls { "https" } else { "http" };
    let mut host = first_header(headers, header::HOST.as_str())
        .filter(|h| valid_host(h))
        .unwrap_or("localhost");
    let mut prefix = "";
    if trust_forward {
        match first_header(headers, "x-forwarded-proto") {
            Some(p) if p.eq_ignore_ascii_case("https") => scheme = "https",
            Some(p) if p.eq_ignore_ascii_case("http") => scheme = "http",
            _ => {}
        }
        if let Some(h) = first_header(headers, "x-forwarded-host").filter(|h| valid_host(h)) {
            host = h;
        }
        if let Some(p) = first_header(headers, "x-forwarded-prefix").filter(|p| valid_prefix(p)) {
            prefix = p.trim_end_matches('/');
        }
    }
    format!("{scheme}://{host}{prefix}")
}

fn request_issuer(
    oidc: &OidcState,
    headers: &HeaderMap,
    tls: &Option<Extension<TlsConnectionInfo>>,
) -> String {
    issuer_for(headers, tls.is_some(), oidc.trust_forward)
}

// ── OAuth errors and responses ──────────────────────────────────────

/// An RFC 6749 error response (`{"error", "error_description"}`).
struct OAuthError {
    status: StatusCode,
    error: &'static str,
    description: String,
}

impl OAuthError {
    fn new(status: StatusCode, error: &'static str, description: impl Into<String>) -> Self {
        Self {
            status,
            error,
            description: description.into(),
        }
    }
    fn bad(error: &'static str, description: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, error, description)
    }
    fn invalid_request(description: impl Into<String>) -> Self {
        Self::bad("invalid_request", description)
    }
    fn invalid_grant(description: impl Into<String>) -> Self {
        Self::bad("invalid_grant", description)
    }
    fn invalid_client(description: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "invalid_client", description)
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        let mut resp = no_store(
            self.status,
            json!({"error": self.error, "error_description": self.description}),
        );
        if self.status == StatusCode::UNAUTHORIZED {
            resp.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"rustybin\""),
            );
        }
        resp
    }
}

/// JSON with `Cache-Control: no-store` and `Pragma: no-cache` (RFC 6749 5.1).
fn no_store(status: StatusCode, body: Value) -> Response {
    let mut resp = (status, Json(body)).into_response();
    let h = resp.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    resp
}

// ── Request parameters ──────────────────────────────────────────────

/// Parameters that may legitimately repeat (RFC 8707, RFC 8693).
const MULTI_PARAMS: &[&str] = &["resource", "audience"];

#[derive(Default)]
struct Params {
    single: HashMap<String, String>,
    multi: HashMap<String, Vec<String>>,
}

impl Params {
    /// Parse urlencoded parameters. Empty values count as omitted; other
    /// parameters must not repeat (RFC 6749 3.1).
    fn parse(raw: &[u8]) -> Result<Self, OAuthError> {
        let mut params = Params::default();
        for (k, v) in form_urlencoded::parse(raw) {
            if v.is_empty() {
                continue;
            }
            if MULTI_PARAMS.contains(&k.as_ref()) {
                params
                    .multi
                    .entry(k.into_owned())
                    .or_default()
                    .push(v.into_owned());
            } else if params
                .single
                .insert(k.to_string(), v.into_owned())
                .is_some()
            {
                return Err(OAuthError::invalid_request(format!(
                    "parameter {k} must not be repeated"
                )));
            }
        }
        Ok(params)
    }

    /// Parse a form body, requiring `application/x-www-form-urlencoded`.
    fn from_form(headers: &HeaderMap, body: &[u8]) -> Result<Self, OAuthError> {
        let is_form = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .is_some_and(|v| {
                v.trim()
                    .eq_ignore_ascii_case("application/x-www-form-urlencoded")
            });
        if !is_form {
            return Err(OAuthError::invalid_request(
                "Content-Type must be application/x-www-form-urlencoded",
            ));
        }
        Self::parse(body)
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.single.get(key).map(String::as_str)
    }

    fn all(&self, key: &str) -> &[String] {
        self.multi.get(key).map(Vec::as_slice).unwrap_or(&[])
    }
}

// ── Validation helpers ──────────────────────────────────────────────

/// Normalise a scope string (RFC 6749 3.3 characters, deduplicated).
fn normalize_scope(scope: &str) -> Result<String, OAuthError> {
    if scope.len() > 1000 {
        return Err(OAuthError::bad("invalid_scope", "scope is too long"));
    }
    let mut out: Vec<&str> = Vec::new();
    for s in scope.split(' ').filter(|s| !s.is_empty()) {
        if !s
            .bytes()
            .all(|b| b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b))
        {
            return Err(OAuthError::bad(
                "invalid_scope",
                "scope contains invalid characters",
            ));
        }
        if !out.contains(&s) {
            out.push(s);
        }
    }
    Ok(out.join(" "))
}

fn scope_has(scope: &str, item: &str) -> bool {
    scope.split(' ').any(|s| s == item)
}

/// Requested scope must be a subset of the granted one.
fn narrow_scope(requested: Option<&str>, granted: &str) -> Result<String, OAuthError> {
    let Some(requested) = requested else {
        return Ok(granted.to_string());
    };
    let requested = normalize_scope(requested)?;
    if requested.split(' ').all(|s| scope_has(granted, s)) {
        Ok(requested)
    } else {
        Err(OAuthError::bad(
            "invalid_scope",
            "requested scope exceeds the granted scope",
        ))
    }
}

/// RFC 8707 resource indicators: absolute URIs without fragment.
fn validate_resources(resources: &[String]) -> Result<(), OAuthError> {
    if resources.len() > MAX_RESOURCES {
        return Err(OAuthError::bad(
            "invalid_target",
            format!("at most {MAX_RESOURCES} resource parameters"),
        ));
    }
    for r in resources {
        let ok = r.len() <= 2048
            && !r.contains('#')
            && r.parse::<axum::http::Uri>()
                .ok()
                .is_some_and(|u| u.scheme().is_some());
        if !ok {
            return Err(OAuthError::bad(
                "invalid_target",
                "resource must be an absolute URI without a fragment",
            ));
        }
    }
    Ok(())
}

/// PKCE code_challenge / code_verifier syntax (RFC 7636 4.1).
fn valid_pkce_value(v: &str) -> bool {
    (43..=128).contains(&v.len())
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
}

fn pkce_matches(verifier: &str, challenge: &str, method: &str) -> bool {
    let computed = match method {
        "S256" => base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes())),
        _ => verifier.to_string(),
    };
    crate::types::constant_time_eq(computed.as_bytes(), challenge.as_bytes())
}

fn new_secret_token(prefix: &str) -> String {
    format!(
        "{prefix}{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

// ── Client authentication ───────────────────────────────────────────

/// Authenticate the client: `client_secret_basic`, `client_secret_post`, or
/// `none` (public clients send only `client_id`).
fn authenticate_client(
    oidc: &OidcState,
    headers: &HeaderMap,
    params: &Params,
) -> Result<Client, OAuthError> {
    let basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("basic"))
        .map(|(_, creds)| creds.trim());
    let (client_id, secret) = if let Some(encoded) = basic {
        if params.get("client_secret").is_some() {
            return Err(OAuthError::invalid_request(
                "use only one client authentication method",
            ));
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .ok_or_else(|| OAuthError::invalid_client("malformed Basic credentials"))?;
        let (id, secret) = decoded
            .split_once(':')
            .ok_or_else(|| OAuthError::invalid_client("malformed Basic credentials"))?;
        // RFC 6749 2.3.1: both parts are form-urlencoded.
        let decode =
            |s: &str| String::from_utf8(crate::types::percent_decode(s, true)).unwrap_or_default();
        let (id, secret) = (decode(id), decode(secret));
        if params.get("client_id").is_some_and(|c| c != id) {
            return Err(OAuthError::invalid_request(
                "client_id does not match the Basic credentials",
            ));
        }
        (id, Some(secret))
    } else {
        let Some(id) = params.get("client_id") else {
            return Err(OAuthError::invalid_client(
                "client authentication required (client_secret_basic, client_secret_post, or client_id for public clients)",
            ));
        };
        (
            id.to_string(),
            params.get("client_secret").map(String::from),
        )
    };
    let client = oidc
        .clients
        .get(&client_id)
        .ok_or_else(|| OAuthError::invalid_client("unknown client"))?;
    match (&secret, client.is_public()) {
        (Some(s), false) if client.secret_matches(s) => Ok(client),
        (None, true) => Ok(client),
        (Some(_), true) => Err(OAuthError::invalid_client(
            "public client must not send a client_secret",
        )),
        _ => Err(OAuthError::invalid_client("invalid client credentials")),
    }
}

// ── Token issuance ──────────────────────────────────────────────────

struct Grant<'a> {
    client: &'a Client,
    sub: String,
    user: Option<&'static DemoUser>,
    scope: String,
    audience: Vec<String>,
    auth_time: Option<i64>,
    nonce: Option<String>,
    refresh: bool,
    extra: Map<String, Value>,
}

fn audience_value(audience: &[String]) -> Value {
    match audience {
        [] => json!(DEFAULT_AUDIENCE),
        [one] => json!(one),
        many => json!(many),
    }
}

fn user_claims(claims: &mut Map<String, Value>, user: &DemoUser, scope: &str) {
    claims.insert("preferred_username".into(), json!(user.username));
    if scope_has(scope, "profile") {
        claims.insert("name".into(), json!(user.name));
    }
    if scope_has(scope, "email") {
        claims.insert("email".into(), json!(user.email));
        claims.insert("email_verified".into(), json!(true));
    }
}

fn sign(oidc: &OidcState, claims: &Map<String, Value>, typ: &str) -> Result<String, OAuthError> {
    oidc.jwt.sign_rs256(claims, typ).map_err(|e| {
        tracing::error!("failed to sign token: {e}");
        OAuthError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "token signing failed",
        )
    })
}

fn access_token_claims(issuer: &str, grant: &Grant, now: i64) -> Map<String, Value> {
    let mut claims = Map::new();
    claims.insert("iss".into(), json!(issuer));
    claims.insert("sub".into(), json!(grant.sub));
    claims.insert("aud".into(), audience_value(&grant.audience));
    claims.insert("exp".into(), json!(now + ACCESS_TOKEN_TTL));
    claims.insert("iat".into(), json!(now));
    claims.insert("jti".into(), json!(uuid::Uuid::new_v4().to_string()));
    claims.insert("client_id".into(), json!(grant.client.client_id));
    if !grant.scope.is_empty() {
        claims.insert("scope".into(), json!(grant.scope));
    }
    if let Some(user) = grant.user {
        claims.insert("name".into(), json!(user.name));
        claims.insert("email".into(), json!(user.email));
        claims.insert("preferred_username".into(), json!(user.username));
        claims.insert("groups".into(), json!(user.groups));
    }
    if let Some(t) = grant.auth_time {
        claims.insert("auth_time".into(), json!(t));
    }
    for (k, v) in &grant.extra {
        claims.insert(k.clone(), v.clone());
    }
    claims
}

fn id_token_claims(issuer: &str, grant: &Grant, user: &DemoUser, now: i64) -> Map<String, Value> {
    let mut claims = Map::new();
    claims.insert("iss".into(), json!(issuer));
    claims.insert("sub".into(), json!(grant.sub));
    claims.insert("aud".into(), json!(grant.client.client_id));
    claims.insert("azp".into(), json!(grant.client.client_id));
    claims.insert("exp".into(), json!(now + ACCESS_TOKEN_TTL));
    claims.insert("iat".into(), json!(now));
    claims.insert("jti".into(), json!(uuid::Uuid::new_v4().to_string()));
    claims.insert("auth_time".into(), json!(grant.auth_time.unwrap_or(now)));
    if let Some(nonce) = &grant.nonce {
        claims.insert("nonce".into(), json!(nonce));
    }
    user_claims(&mut claims, user, &grant.scope);
    claims
}

/// Sign the access token (and ID token / refresh token when applicable)
/// and build the token response.
fn issue_tokens(oidc: &OidcState, issuer: &str, grant: Grant) -> Result<Response, OAuthError> {
    let now = chrono::Utc::now().timestamp();
    let access_token = sign(oidc, &access_token_claims(issuer, &grant, now), "at+jwt")?;
    let mut body = json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": ACCESS_TOKEN_TTL,
    });
    let Some(obj) = body.as_object_mut() else {
        return Err(OAuthError::invalid_request("internal error"));
    };
    if !grant.scope.is_empty() {
        obj.insert("scope".into(), json!(grant.scope));
    }
    if let Some(user) = grant.user.filter(|_| scope_has(&grant.scope, "openid")) {
        let id_token = sign(oidc, &id_token_claims(issuer, &grant, user, now), "JWT")?;
        obj.insert("id_token".into(), json!(id_token));
    }
    if grant.refresh && grant.client.allows_grant(GRANT_REFRESH_TOKEN) {
        let token = new_secret_token("rt_");
        oidc.refresh_tokens.insert(
            token.clone(),
            RefreshGrant {
                client_id: grant.client.client_id.clone(),
                sub: grant.sub.clone(),
                username: grant.user.map(|u| u.username.to_string()),
                scope: grant.scope.clone(),
                audience: grant.audience.clone(),
                auth_time: grant.auth_time,
            },
        );
        obj.insert("refresh_token".into(), json!(token));
    }
    Ok(no_store(StatusCode::OK, body))
}

// ── Token endpoint ──────────────────────────────────────────────────

async fn token(
    Extension(oidc): Extension<Arc<OidcState>>,
    tls: Option<Extension<TlsConnectionInfo>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let issuer = request_issuer(&oidc, &headers, &tls);
    match token_inner(&oidc, &issuer, &headers, &body) {
        Ok(resp) => resp,
        Err(e) => e.into_response(),
    }
}

fn token_inner(
    oidc: &OidcState,
    issuer: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Response, OAuthError> {
    let params = Params::from_form(headers, body)?;
    let grant_type = params
        .get("grant_type")
        .ok_or_else(|| OAuthError::invalid_request("missing grant_type"))?;
    if !clients::ALL_GRANTS.contains(&grant_type) {
        return Err(OAuthError::bad(
            "unsupported_grant_type",
            "supported: authorization_code, refresh_token, client_credentials, password, urn:ietf:params:oauth:grant-type:token-exchange",
        ));
    }
    let client = authenticate_client(oidc, headers, &params)?;
    if !client.allows_grant(grant_type) {
        return Err(OAuthError::bad(
            "unauthorized_client",
            format!(
                "client {} may not use the {grant_type} grant",
                client.client_id
            ),
        ));
    }
    let resources = params.all("resource").to_vec();
    validate_resources(&resources)?;
    match grant_type {
        GRANT_AUTHORIZATION_CODE => grant_authorization_code(oidc, issuer, &client, &params),
        GRANT_REFRESH_TOKEN => grant_refresh_token(oidc, issuer, &client, &params),
        GRANT_CLIENT_CREDENTIALS => {
            let scope = normalize_scope(
                params
                    .get("scope")
                    .or(client.scope.as_deref())
                    .unwrap_or("openid"),
            )?;
            issue_tokens(
                oidc,
                issuer,
                Grant {
                    client: &client,
                    sub: client.client_id.clone(),
                    user: None,
                    scope,
                    audience: resources,
                    auth_time: None,
                    nonce: None,
                    refresh: false,
                    extra: Map::new(),
                },
            )
        }
        GRANT_PASSWORD => {
            let (Some(username), Some(password)) = (params.get("username"), params.get("password"))
            else {
                return Err(OAuthError::invalid_request(
                    "username and password are required",
                ));
            };
            let user = clients::authenticate_user(username, password)
                .ok_or_else(|| OAuthError::invalid_grant("invalid username or password"))?;
            let scope = normalize_scope(params.get("scope").unwrap_or("openid profile email"))?;
            issue_tokens(
                oidc,
                issuer,
                Grant {
                    client: &client,
                    sub: user.username.to_string(),
                    user: Some(user),
                    scope,
                    audience: resources,
                    auth_time: Some(chrono::Utc::now().timestamp()),
                    nonce: None,
                    refresh: true,
                    extra: Map::new(),
                },
            )
        }
        GRANT_TOKEN_EXCHANGE => grant_token_exchange(oidc, issuer, &client, &params),
        _ => Err(OAuthError::bad(
            "unsupported_grant_type",
            "unsupported grant_type",
        )),
    }
}

fn grant_authorization_code(
    oidc: &OidcState,
    issuer: &str,
    client: &Client,
    params: &Params,
) -> Result<Response, OAuthError> {
    let code = params
        .get("code")
        .ok_or_else(|| OAuthError::invalid_request("missing code"))?;
    // Single use: the code is removed whatever happens next.
    let entry = oidc
        .codes
        .take(code)
        .ok_or_else(|| OAuthError::invalid_grant("invalid, expired or already used code"))?;
    if entry.client_id != client.client_id {
        return Err(OAuthError::invalid_grant(
            "code was issued to another client",
        ));
    }
    let redirect_ok = match params.get("redirect_uri") {
        Some(uri) => uri == entry.redirect_uri,
        None => !entry.redirect_uri_given,
    };
    if !redirect_ok {
        return Err(OAuthError::invalid_grant(
            "redirect_uri does not match the authorization request",
        ));
    }
    let verifier = params.get("code_verifier");
    match (&entry.code_challenge, verifier) {
        (Some((challenge, method)), Some(v)) => {
            if !valid_pkce_value(v) || !pkce_matches(v, challenge, method) {
                return Err(OAuthError::invalid_grant("PKCE verification failed"));
            }
        }
        (Some(_), None) => {
            return Err(OAuthError::invalid_grant("code_verifier is required"));
        }
        (None, Some(_)) => {
            return Err(OAuthError::invalid_grant(
                "code_verifier sent but no code_challenge was used",
            ));
        }
        (None, None) => {}
    }
    let user = clients::user_by_name(&entry.username)
        .ok_or_else(|| OAuthError::invalid_grant("unknown user"))?;
    let requested = params.all("resource");
    let audience = if requested.is_empty() {
        entry.resources.clone()
    } else {
        requested.to_vec()
    };
    issue_tokens(
        oidc,
        issuer,
        Grant {
            client,
            sub: user.username.to_string(),
            user: Some(user),
            scope: entry.scope,
            audience,
            auth_time: Some(entry.auth_time),
            nonce: entry.nonce,
            refresh: true,
            extra: Map::new(),
        },
    )
}

fn grant_refresh_token(
    oidc: &OidcState,
    issuer: &str,
    client: &Client,
    params: &Params,
) -> Result<Response, OAuthError> {
    let token = params
        .get("refresh_token")
        .ok_or_else(|| OAuthError::invalid_request("missing refresh_token"))?;
    // Rotation: the presented refresh token is consumed.
    let grant = oidc.refresh_tokens.take(token).ok_or_else(|| {
        OAuthError::invalid_grant("invalid, expired or already used refresh token")
    })?;
    if grant.client_id != client.client_id {
        return Err(OAuthError::invalid_grant(
            "refresh token was issued to another client",
        ));
    }
    let scope = narrow_scope(params.get("scope"), &grant.scope)?;
    let requested = params.all("resource");
    let audience = if requested.is_empty() {
        grant.audience.clone()
    } else {
        requested.to_vec()
    };
    issue_tokens(
        oidc,
        issuer,
        Grant {
            client,
            sub: grant.sub.clone(),
            user: grant.username.as_deref().and_then(clients::user_by_name),
            scope,
            audience,
            auth_time: grant.auth_time,
            nonce: None,
            refresh: true,
            extra: Map::new(),
        },
    )
}

/// RFC 8693 token exchange. Subject (and actor) tokens must verify with
/// this server's keys; otherwise `invalid_request` (RFC 8693 2.2.2).
fn grant_token_exchange(
    oidc: &OidcState,
    issuer: &str,
    client: &Client,
    params: &Params,
) -> Result<Response, OAuthError> {
    const SUBJECT_TYPES: &[&str] = &[TOKEN_TYPE_ACCESS_TOKEN, TOKEN_TYPE_ID_TOKEN, TOKEN_TYPE_JWT];
    let subject_token = params
        .get("subject_token")
        .ok_or_else(|| OAuthError::invalid_request("missing required parameter: subject_token"))?;
    let subject_type = params.get("subject_token_type").ok_or_else(|| {
        OAuthError::invalid_request("missing required parameter: subject_token_type")
    })?;
    if !SUBJECT_TYPES.contains(&subject_type) {
        return Err(OAuthError::invalid_request(
            "unsupported subject_token_type (access_token, id_token or jwt)",
        ));
    }
    let subject = oidc
        .verify(subject_token)
        .map_err(|e| OAuthError::invalid_request(format!("subject_token is invalid: {e}")))?
        .claims;
    let sub = subject
        .get("sub")
        .and_then(|s| s.as_str())
        .ok_or_else(|| OAuthError::invalid_request("subject_token has no sub claim"))?
        .to_string();

    let actor = match params.get("actor_token") {
        None => None,
        Some(actor_token) => {
            match params.get("actor_token_type") {
                Some(t) if SUBJECT_TYPES.contains(&t) => {}
                Some(_) => {
                    return Err(OAuthError::invalid_request(
                        "unsupported actor_token_type (access_token, id_token or jwt)",
                    ))
                }
                None => {
                    return Err(OAuthError::invalid_request(
                        "actor_token_type is required when actor_token is present",
                    ))
                }
            }
            let claims = oidc
                .verify(actor_token)
                .map_err(|e| OAuthError::invalid_request(format!("actor_token is invalid: {e}")))?
                .claims;
            Some(claims)
        }
    };

    let requested_type = params
        .get("requested_token_type")
        .unwrap_or(TOKEN_TYPE_ACCESS_TOKEN);
    if ![TOKEN_TYPE_ACCESS_TOKEN, TOKEN_TYPE_JWT, TOKEN_TYPE_ID_TOKEN].contains(&requested_type) {
        return Err(OAuthError::invalid_request(
            "unsupported requested_token_type (access_token, jwt or id_token)",
        ));
    }

    let granted = subject
        .get("scope")
        .and_then(|s| s.as_str())
        .map(normalize_scope)
        .transpose()?;
    let scope = match granted {
        Some(g) => narrow_scope(params.get("scope"), &g)?,
        None => normalize_scope(params.get("scope").unwrap_or("openid"))?,
    };

    let mut audience: Vec<String> = params.all("audience").to_vec();
    audience.extend(params.all("resource").iter().cloned());
    if audience.len() > MAX_RESOURCES {
        return Err(OAuthError::bad("invalid_target", "too many audiences"));
    }

    let mut extra = Map::new();
    for key in ["name", "email", "preferred_username", "groups"] {
        if let Some(v) = subject.get(key) {
            extra.insert(key.into(), v.clone());
        }
    }
    if let Some(actor) = &actor {
        let mut act = Map::new();
        act.insert(
            "sub".into(),
            actor.get("sub").cloned().unwrap_or(json!("unknown")),
        );
        // Delegation chain: an earlier actor nests inside the new one.
        if let Some(prior) = subject.get("act") {
            act.insert("act".into(), prior.clone());
        }
        extra.insert("act".into(), Value::Object(act));
    }

    let grant = Grant {
        client,
        sub,
        user: None,
        scope,
        audience,
        auth_time: None,
        nonce: None,
        refresh: false,
        extra,
    };
    let now = chrono::Utc::now().timestamp();
    let (token, issued_type, token_type) = if requested_type == TOKEN_TYPE_ID_TOKEN {
        let mut claims = Map::new();
        claims.insert("iss".into(), json!(issuer));
        claims.insert("sub".into(), json!(grant.sub));
        claims.insert("aud".into(), json!(client.client_id));
        claims.insert("azp".into(), json!(client.client_id));
        claims.insert("exp".into(), json!(now + ACCESS_TOKEN_TTL));
        claims.insert("iat".into(), json!(now));
        for (k, v) in &grant.extra {
            claims.insert(k.clone(), v.clone());
        }
        (sign(oidc, &claims, "JWT")?, TOKEN_TYPE_ID_TOKEN, "N_A")
    } else {
        let claims = access_token_claims(issuer, &grant, now);
        (sign(oidc, &claims, "at+jwt")?, requested_type, "Bearer")
    };
    let mut body = json!({
        "access_token": token,
        "issued_token_type": issued_type,
        "token_type": token_type,
        "expires_in": ACCESS_TOKEN_TTL,
    });
    if let Some(obj) = body.as_object_mut() {
        if !grant.scope.is_empty() {
            obj.insert("scope".into(), json!(grant.scope));
        }
    }
    Ok(no_store(StatusCode::OK, body))
}

// ── Authorization endpoint ──────────────────────────────────────────

struct AuthzRequest {
    client: Client,
    redirect_uri: String,
    redirect_uri_given: bool,
    scope: String,
    state: Option<String>,
    nonce: Option<String>,
    code_challenge: Option<(String, String)>,
    resources: Vec<String>,
    login_hint: Option<String>,
}

enum AuthzError {
    /// Shown to the user; never redirected (unknown client or bad redirect_uri).
    Page(StatusCode, String),
    /// Sent back to the (validated) redirect URI.
    Redirect {
        redirect_uri: String,
        error: &'static str,
        description: String,
        state: Option<String>,
    },
}

fn validate_authorize(oidc: &OidcState, params: &Params) -> Result<AuthzRequest, AuthzError> {
    let page = |msg: &str| AuthzError::Page(StatusCode::BAD_REQUEST, msg.to_string());
    let client_id = params
        .get("client_id")
        .ok_or_else(|| page("missing client_id"))?;
    let client = oidc
        .clients
        .get(client_id)
        .ok_or_else(|| page("unknown client_id"))?;
    let redirect_uri = match params.get("redirect_uri") {
        Some(uri) => {
            if !client.redirect_allowed(uri) {
                return Err(page(
                    "redirect_uri is invalid or not registered for this client",
                ));
            }
            uri.to_string()
        }
        None => match client.redirect_uris.as_slice() {
            [only] => only.clone(),
            _ => return Err(page("missing redirect_uri")),
        },
    };
    let redirect_uri_given = params.get("redirect_uri").is_some();
    let state = params.get("state").map(String::from);
    let redirect_err = |error: &'static str, description: &str| AuthzError::Redirect {
        redirect_uri: redirect_uri.clone(),
        error,
        description: description.to_string(),
        state: state.clone(),
    };
    if state.as_ref().is_some_and(|s| s.len() > 2048) {
        return Err(redirect_err("invalid_request", "state is too long"));
    }
    if params.get("response_type") != Some("code") {
        return Err(redirect_err(
            "unsupported_response_type",
            "only response_type=code is supported",
        ));
    }
    if !client.allows_grant(GRANT_AUTHORIZATION_CODE) {
        return Err(redirect_err(
            "unauthorized_client",
            "client may not use the authorization code flow",
        ));
    }
    let scope = normalize_scope(params.get("scope").unwrap_or("openid profile email"))
        .map_err(|e| redirect_err("invalid_scope", &e.description))?;
    let nonce = params.get("nonce").map(String::from);
    if nonce.as_ref().is_some_and(|n| n.len() > 512) {
        return Err(redirect_err("invalid_request", "nonce is too long"));
    }
    let code_challenge = match params.get("code_challenge") {
        Some(challenge) => {
            let method = params.get("code_challenge_method").unwrap_or("plain");
            if method != "S256" && method != "plain" {
                return Err(redirect_err(
                    "invalid_request",
                    "code_challenge_method must be S256 or plain",
                ));
            }
            if !valid_pkce_value(challenge) {
                return Err(redirect_err(
                    "invalid_request",
                    "code_challenge must be 43 to 128 unreserved characters",
                ));
            }
            Some((challenge.to_string(), method.to_string()))
        }
        None if client.is_public() => {
            return Err(redirect_err(
                "invalid_request",
                "public clients must use PKCE (code_challenge)",
            ))
        }
        None => None,
    };
    let resources = params.all("resource").to_vec();
    validate_resources(&resources).map_err(|e| redirect_err("invalid_target", &e.description))?;
    Ok(AuthzRequest {
        client,
        redirect_uri,
        redirect_uri_given,
        scope,
        state,
        nonce,
        code_challenge,
        resources,
        login_hint: params.get("login_hint").map(String::from),
    })
}

/// HTML response with headers suited to a login page.
fn html(status: StatusCode, body: String) -> Response {
    let mut resp = (status, axum::response::Html(body)).into_response();
    let h = resp.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'",
        ),
    );
    resp
}

const PAGE_STYLE: &str = "body{font-family:system-ui,sans-serif;max-width:420px;margin:60px auto;padding:0 16px;color:#1d1d1f;background:#fff}\
input{display:block;width:100%;margin:6px 0 12px;padding:8px;box-sizing:border-box}\
button{padding:10px 20px;margin:8px 8px 0 0;cursor:pointer}\
.err{color:#b00020}.meta{color:#555;font-size:14px}code{background:#f2f2f2;padding:1px 4px}\
@media (prefers-color-scheme:dark){body{background:#151515;color:#eee}.meta{color:#aaa}code{background:#2a2a2a}}";

fn error_page(status: StatusCode, message: &str) -> Response {
    html(
        status,
        format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Authorization error</title>\
             <style>{PAGE_STYLE}</style></head><body><h2>Authorization error</h2>\
             <p class=\"err\">{}</p></body></html>",
            html_escape(message)
        ),
    )
}

fn login_page(req: &AuthzRequest, error: Option<&str>, username: &str) -> Response {
    let e = html_escape;
    let mut hidden = String::new();
    let mut field = |name: &str, value: &str| {
        hidden.push_str(&format!(
            "<input type=\"hidden\" name=\"{}\" value=\"{}\">",
            e(name),
            e(value)
        ));
    };
    field("client_id", &req.client.client_id);
    if req.redirect_uri_given {
        field("redirect_uri", &req.redirect_uri);
    }
    field("response_type", "code");
    field("scope", &req.scope);
    if let Some(s) = &req.state {
        field("state", s);
    }
    if let Some(n) = &req.nonce {
        field("nonce", n);
    }
    if let Some((c, m)) = &req.code_challenge {
        field("code_challenge", c);
        field("code_challenge_method", m);
    }
    for r in &req.resources {
        field("resource", r);
    }
    let error_html = error
        .map(|m| format!("<p class=\"err\">{}</p>", e(m)))
        .unwrap_or_default();
    let status = if error.is_some() {
        StatusCode::UNAUTHORIZED
    } else {
        StatusCode::OK
    };
    html(
        status,
        format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
             <title>Rustybin login</title><style>{PAGE_STYLE}</style></head><body>\
             <h2>Rustybin OIDC Login</h2>\
             <p class=\"meta\">Client <code>{client}</code> ({name}) asks for <code>{scope}</code>.</p>\
             {error_html}\
             <form method=\"POST\" action=\"authorize\">\
             <label>Username</label><input name=\"username\" value=\"{username}\" autocomplete=\"username\">\
             <label>Password</label><input name=\"password\" type=\"password\" autocomplete=\"current-password\">\
             {hidden}\
             <button type=\"submit\" name=\"action\" value=\"approve\">Authorize</button>\
             <button type=\"submit\" name=\"action\" value=\"deny\">Deny</button>\
             </form>\
             <p class=\"meta\">Demo users: demo / demo, alice / alice, bob / bob.</p>\
             </body></html>",
            client = e(&req.client.client_id),
            name = e(&req.client.name),
            scope = e(&req.scope),
            username = e(username),
        ),
    )
}

fn redirect_response(redirect_uri: &str, params: &[(&str, &str)]) -> Response {
    match clients::redirect_location(redirect_uri, params) {
        Some(location) => {
            let mut resp = StatusCode::SEE_OTHER.into_response();
            resp.headers_mut().insert(header::LOCATION, location);
            resp.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            resp
        }
        None => error_page(StatusCode::BAD_REQUEST, "redirect_uri could not be used"),
    }
}

fn authz_error_response(err: AuthzError) -> Response {
    match err {
        AuthzError::Page(status, msg) => error_page(status, &msg),
        AuthzError::Redirect {
            redirect_uri,
            error,
            description,
            state,
        } => redirect_response(
            &redirect_uri,
            &[
                ("error", error),
                ("error_description", &description),
                ("state", state.as_deref().unwrap_or("")),
            ],
        ),
    }
}

async fn authorize_get(
    Extension(oidc): Extension<Arc<OidcState>>,
    RawQuery(query): RawQuery,
) -> Response {
    let params = match Params::parse(query.unwrap_or_default().as_bytes()) {
        Ok(p) => p,
        Err(e) => return error_page(StatusCode::BAD_REQUEST, &e.description),
    };
    match validate_authorize(&oidc, &params) {
        Ok(req) => {
            let hint = req.login_hint.clone().unwrap_or_else(|| "demo".into());
            login_page(&req, None, &hint)
        }
        Err(e) => authz_error_response(e),
    }
}

async fn authorize_post(
    Extension(oidc): Extension<Arc<OidcState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let params = match Params::from_form(&headers, &body) {
        Ok(p) => p,
        Err(e) => return error_page(StatusCode::BAD_REQUEST, &e.description),
    };
    let req = match validate_authorize(&oidc, &params) {
        Ok(r) => r,
        Err(e) => return authz_error_response(e),
    };
    let state = req.state.as_deref().unwrap_or("");
    if params.get("action") == Some("deny") {
        return redirect_response(
            &req.redirect_uri,
            &[
                ("error", "access_denied"),
                ("error_description", "the user denied the request"),
                ("state", state),
            ],
        );
    }
    let username = params.get("username").unwrap_or("");
    let password = params.get("password").unwrap_or("");
    let Some(user) = clients::authenticate_user(username, password) else {
        return login_page(&req, Some("Invalid username or password."), username);
    };
    let code = new_secret_token("");
    oidc.codes.insert(
        code.clone(),
        AuthCode {
            client_id: req.client.client_id.clone(),
            redirect_uri: req.redirect_uri.clone(),
            redirect_uri_given: req.redirect_uri_given,
            username: user.username.to_string(),
            scope: req.scope.clone(),
            nonce: req.nonce.clone(),
            code_challenge: req.code_challenge.clone(),
            resources: req.resources.clone(),
            auth_time: chrono::Utc::now().timestamp(),
        },
    );
    redirect_response(&req.redirect_uri, &[("code", &code), ("state", state)])
}

// ── Userinfo, introspection, revocation ─────────────────────────────

fn bearer_error(status: StatusCode, error: &'static str, description: &str) -> Response {
    let mut resp = no_store(
        status,
        json!({"error": error, "error_description": description}),
    );
    let challenge = format!("Bearer realm=\"rustybin\", error=\"{error}\"");
    if let Ok(v) = HeaderValue::from_str(&challenge) {
        resp.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    resp
}

/// UserInfo (GET or POST; Bearer header, or `access_token` form field on POST).
async fn userinfo(
    Extension(oidc): Extension<Arc<OidcState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let form_token = Params::from_form(&headers, &body)
        .ok()
        .and_then(|p| p.get("access_token").map(String::from));
    let Some(token) = bearer_token(&headers).map(String::from).or(form_token) else {
        return bearer_error(
            StatusCode::UNAUTHORIZED,
            "invalid_request",
            "missing Bearer access token",
        );
    };
    let verified = match oidc.verify(&token) {
        Ok(v) if v.alg == "RS256" && v.header.get("typ") == Some(&json!("at+jwt")) => v,
        Ok(_) => {
            return bearer_error(
                StatusCode::UNAUTHORIZED,
                "invalid_token",
                "not an access token issued by this provider",
            )
        }
        Err(e) => return bearer_error(StatusCode::UNAUTHORIZED, "invalid_token", &e),
    };
    let claims = verified.claims;
    let scope = claims.get("scope").and_then(|s| s.as_str()).unwrap_or("");
    if !scope_has(scope, "openid") {
        return bearer_error(
            StatusCode::FORBIDDEN,
            "insufficient_scope",
            "the access token lacks the openid scope",
        );
    }
    let sub = claims.get("sub").cloned().unwrap_or(json!(""));
    let mut info = Map::new();
    info.insert("sub".into(), sub.clone());
    if let Some(user) = sub.as_str().and_then(clients::user_by_name) {
        user_claims(&mut info, user, scope);
        info.insert("groups".into(), json!(user.groups));
    }
    no_store(StatusCode::OK, Value::Object(info))
}

/// RFC 7662 token introspection (client authentication required).
async fn introspect(
    Extension(oidc): Extension<Arc<OidcState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let result: Result<_, OAuthError> = (|| {
        let params = Params::from_form(&headers, &body)?;
        let client = authenticate_client(&oidc, &headers, &params)?;
        if client.is_public() {
            return Err(OAuthError::invalid_client(
                "introspection requires a confidential client",
            ));
        }
        let token = params
            .get("token")
            .ok_or_else(|| OAuthError::invalid_request("missing token"))?;
        Ok(introspect_token(&oidc, token))
    })();
    match result {
        Ok(body) => no_store(StatusCode::OK, body),
        Err(e) => e.into_response(),
    }
}

fn introspect_token(oidc: &OidcState, token: &str) -> Value {
    let inactive = json!({"active": false});
    if let Some((grant, exp)) = oidc.refresh_tokens.get_with_expiry(token) {
        let mut out = json!({
            "active": true,
            "token_type": "refresh_token",
            "client_id": grant.client_id,
            "sub": grant.sub,
            "exp": exp,
        });
        if let Some(obj) = out.as_object_mut() {
            if !grant.scope.is_empty() {
                obj.insert("scope".into(), json!(grant.scope));
            }
            if let Some(u) = grant.username {
                obj.insert("username".into(), json!(u));
            }
        }
        return out;
    }
    let Ok(verified) = oidc.verify(token) else {
        return inactive;
    };
    let claims = verified.claims;
    let mut out = Map::new();
    out.insert("active".into(), json!(true));
    let token_type =
        if verified.header.get("typ") == Some(&json!("at+jwt")) || verified.alg == "HS256" {
            "Bearer"
        } else {
            "id_token"
        };
    out.insert("token_type".into(), json!(token_type));
    for key in [
        "scope",
        "client_id",
        "sub",
        "aud",
        "iss",
        "exp",
        "iat",
        "nbf",
        "jti",
        "act",
    ] {
        if let Some(v) = claims.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    if !out.contains_key("client_id") {
        if let Some(azp) = claims.get("azp") {
            out.insert("client_id".into(), azp.clone());
        }
    }
    if let Some(u) = claims.get("preferred_username") {
        out.insert("username".into(), u.clone());
    }
    Value::Object(out)
}

/// RFC 7009 token revocation. Always 200 for well-formed requests.
async fn revoke(
    Extension(oidc): Extension<Arc<OidcState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let result: Result<_, OAuthError> = (|| {
        let params = Params::from_form(&headers, &body)?;
        let client = authenticate_client(&oidc, &headers, &params)?;
        let token = params
            .get("token")
            .ok_or_else(|| OAuthError::invalid_request("missing token"))?;
        if let Some(grant) = oidc.refresh_tokens.get(token) {
            if grant.client_id == client.client_id {
                oidc.refresh_tokens.remove(token);
            }
            return Ok(());
        }
        if let Ok(verified) = oidc.verify(token) {
            let claims = verified.claims;
            let owner = claims
                .get("client_id")
                .or_else(|| claims.get("azp"))
                .and_then(|c| c.as_str());
            let jti = claims.get("jti").and_then(|j| j.as_str());
            let exp = claims.get("exp").and_then(|e| e.as_i64()).unwrap_or(0);
            if let (Some(jti), true) = (jti, owner == Some(client.client_id.as_str())) {
                oidc.revoked.insert_until(jti.to_string(), (), exp + 60);
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => no_store(StatusCode::OK, json!({})),
        Err(e) => e.into_response(),
    }
}

/// RFC 7591 dynamic client registration.
async fn register(
    Extension(oidc): Extension<Arc<OidcState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
    if !is_json {
        return OAuthError::bad(
            "invalid_client_metadata",
            "Content-Type must be application/json",
        )
        .into_response();
    }
    let Ok(metadata) = serde_json::from_slice::<Value>(&body) else {
        return OAuthError::bad("invalid_client_metadata", "body is not valid JSON")
            .into_response();
    };
    match oidc.clients.register(&metadata) {
        Ok(registered) => no_store(StatusCode::CREATED, registered),
        Err((error, description)) => OAuthError::bad(error, description).into_response(),
    }
}

// ── Discovery ───────────────────────────────────────────────────────

fn metadata(issuer: &str, oidc: bool) -> Value {
    let mut doc = json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/oauth/authorize"),
        "token_endpoint": format!("{issuer}/oauth/token"),
        "jwks_uri": format!("{issuer}/oauth/jwks"),
        "registration_endpoint": format!("{issuer}/oauth/register"),
        "introspection_endpoint": format!("{issuer}/oauth/introspect"),
        "revocation_endpoint": format!("{issuer}/oauth/revoke"),
        "userinfo_endpoint": format!("{issuer}/oauth/userinfo"),
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": clients::ALL_GRANTS,
        "code_challenge_methods_supported": ["S256", "plain"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
        "introspection_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
        "revocation_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
        "scopes_supported": ["openid", "profile", "email"],
    });
    if oidc {
        if let Some(obj) = doc.as_object_mut() {
            obj.insert("subject_types_supported".into(), json!(["public"]));
            obj.insert(
                "id_token_signing_alg_values_supported".into(),
                json!(["RS256"]),
            );
            obj.insert(
                "claims_supported".into(),
                json!([
                    "sub",
                    "iss",
                    "aud",
                    "exp",
                    "iat",
                    "auth_time",
                    "nonce",
                    "azp",
                    "name",
                    "preferred_username",
                    "email",
                    "email_verified",
                    "groups"
                ]),
            );
            obj.insert("claims_parameter_supported".into(), json!(false));
            obj.insert("request_parameter_supported".into(), json!(false));
            obj.insert("request_uri_parameter_supported".into(), json!(false));
        }
    }
    doc
}

async fn openid_configuration(
    Extension(oidc): Extension<Arc<OidcState>>,
    tls: Option<Extension<TlsConnectionInfo>>,
    headers: HeaderMap,
) -> Response {
    Json(metadata(&request_issuer(&oidc, &headers, &tls), true)).into_response()
}

async fn oauth_authorization_server(
    Extension(oidc): Extension<Arc<OidcState>>,
    tls: Option<Extension<TlsConnectionInfo>>,
    headers: HeaderMap,
) -> Response {
    Json(metadata(&request_issuer(&oidc, &headers, &tls), false)).into_response()
}

async fn jwks(Extension(oidc): Extension<Arc<OidcState>>) -> Response {
    Json(json!({ "keys": [oidc.jwt.rs256_jwk.clone()] })).into_response()
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(state: &AppState) -> Router<AppState> {
    let oidc = Arc::new(OidcState::new(
        state.jwt.clone(),
        state.config.trust_forward,
        state.config.public_mode,
    ));
    Router::new()
        .route(
            "/.well-known/openid-configuration",
            get(openid_configuration),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth_authorization_server),
        )
        .route("/oauth/token", post(token))
        .route("/oauth/jwks", get(jwks))
        .route("/oauth/authorize", get(authorize_get).post(authorize_post))
        .route("/oauth/userinfo", get(userinfo).post(userinfo))
        .route("/oauth/introspect", post(introspect))
        .route("/oauth/revoke", post(revoke))
        .route("/oauth/register", post(register))
        .layer(Extension(oidc))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new("/.well-known/openid-configuration", &["GET"], category::AUTH_OIDC, "OIDC discovery document")
            .description("Issuer is derived from Host (and X-Forwarded-Proto / X-Forwarded-Host / X-Forwarded-Prefix only when RUSTYBIN_TRUST_FORWARD=true).")
            .example(Example::get("OIDC Discovery", "/.well-known/openid-configuration")),
        Endpoint::new("/.well-known/oauth-authorization-server", &["GET"], category::AUTH_OIDC, "OAuth 2.0 authorization server metadata (RFC 8414)")
            .example(Example::get("OAuth AS metadata", "/.well-known/oauth-authorization-server")),
        Endpoint::new("/oauth/token", &["POST"], category::AUTH_OIDC, "Token endpoint (authorization_code + PKCE, refresh_token, client_credentials, password, token exchange)")
            .description("Clients: rustybin / secret (confidential; client_secret_basic or client_secret_post) and rustybin-public (public, PKCE). \
                Users for the password grant and login form: demo/demo, alice/alice, bob/bob. \
                `resource` (RFC 8707) sets the access token audience (default rustybin). Refresh tokens rotate. \
                Token exchange (RFC 8693) only accepts subject tokens signed by this server (RS256 IdP key or HS256 demo secret).")
            .example(Example::post("Token (client_credentials)", "/oauth/token").form(&[
                ("grant_type", "client_credentials"),
                ("client_id", "rustybin"),
                ("client_secret", "secret"),
            ]))
            .example(Example::post("Token (client_credentials for a resource)", "/oauth/token").form(&[
                ("grant_type", "client_credentials"),
                ("client_id", "rustybin"),
                ("client_secret", "secret"),
                ("resource", "https://api.example.com"),
            ]))
            .example(Example::post("Token (password grant)", "/oauth/token").form(&[
                ("grant_type", "password"),
                ("client_id", "rustybin"),
                ("client_secret", "secret"),
                ("username", "demo"),
                ("password", "demo"),
            ]))
            .example(Example::post("Token Exchange (RFC 8693)", "/oauth/token").form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:token-exchange"),
                ("client_id", "rustybin"),
                ("client_secret", "secret"),
                ("subject_token", crate::auth_jwt::SAMPLE_JWT),
                ("subject_token_type", "urn:ietf:params:oauth:token-type:access_token"),
                ("audience", "https://api.example.com"),
            ]))
            .example(Example::post("Refresh token", "/oauth/token").form(&[
                ("grant_type", "refresh_token"),
                ("client_id", "rustybin"),
                ("client_secret", "secret"),
                ("refresh_token", "<refresh_token>"),
            ])),
        Endpoint::new("/oauth/jwks", &["GET"], category::AUTH_OIDC, "RS256 public key in JWK Set format")
            .example(Example::get("JWKS", "/oauth/jwks")),
        Endpoint::new("/oauth/authorize", &["GET", "POST"], category::AUTH_OIDC, "Authorization code flow with a demo login form (PKCE, nonce, resource)")
            .description("redirect_uri must be an absolute http(s) URL: any for the demo clients, an exact registered one for dynamically registered clients. \
                The public client rustybin-public must send code_challenge (S256 or plain). Codes are single use and expire after 5 minutes.")
            .example(Example::get(
                "Authorize (login form)",
                "/oauth/authorize?response_type=code&client_id=rustybin&redirect_uri=http://localhost/callback&scope=openid%20profile%20email&state=xyz&nonce=n-0S6",
            )),
        Endpoint::new("/oauth/userinfo", &["GET", "POST"], category::AUTH_OIDC, "User claims for a Bearer access token (scope openid)")
            .example(Example::get("UserInfo", "/oauth/userinfo").header("Authorization", "Bearer <access_token>")),
        Endpoint::new("/oauth/introspect", &["POST"], category::AUTH_OIDC, "Token introspection (RFC 7662, client authentication required)")
            .example(Example::post("Introspect token", "/oauth/introspect").form(&[
                ("token", "<access_token>"),
                ("client_id", "rustybin"),
                ("client_secret", "secret"),
            ])),
        Endpoint::new("/oauth/revoke", &["POST"], category::AUTH_OIDC, "Token revocation (RFC 7009) for refresh and access tokens")
            .example(Example::post("Revoke token", "/oauth/revoke").form(&[
                ("token", "<refresh_token>"),
                ("client_id", "rustybin"),
                ("client_secret", "secret"),
            ])),
        Endpoint::new("/oauth/register", &["POST"], category::AUTH_OIDC, "Dynamic client registration (RFC 7591, used by MCP clients)")
            .description("Registered clients are kept for 24 hours in a bounded registry (1000 clients, 200 in public mode). \
                token_endpoint_auth_method none registers a public client (PKCE required).")
            .example(Example::post("Register client", "/oauth/register").json(
                r#"{"client_name":"demo app","redirect_uris":["http://localhost:3000/callback"],"grant_types":["authorization_code","refresh_token"],"token_endpoint_auth_method":"client_secret_basic"}"#,
            )),
    ]
}

pub fn openapi_paths() -> Value {
    let error = json!({
        "type": "object",
        "properties": {
            "error": { "type": "string", "example": "invalid_request" },
            "error_description": { "type": "string" }
        }
    });
    let error_content = json!({ "application/json": { "schema": error } });
    let form = |required: Value, properties: Value| {
        json!({
            "required": true,
            "content": { "application/x-www-form-urlencoded": { "schema": {
                "type": "object", "required": required, "properties": properties
            }}}
        })
    };
    let client_auth = json!({
        "client_id": { "type": "string", "example": "rustybin" },
        "client_secret": { "type": "string", "example": "secret" }
    });
    let mut token_props = json!({
        "grant_type": { "type": "string", "enum": clients::ALL_GRANTS },
        "scope": { "type": "string" },
        "resource": { "type": "string", "description": "RFC 8707 resource indicator (repeatable); becomes the access token aud" },
        "username": { "type": "string", "description": "password grant (demo/demo, alice/alice, bob/bob)" },
        "password": { "type": "string", "description": "password grant" },
        "code": { "type": "string", "description": "authorization_code grant" },
        "redirect_uri": { "type": "string", "description": "authorization_code grant (must match the authorization request)" },
        "code_verifier": { "type": "string", "description": "PKCE verifier" },
        "refresh_token": { "type": "string", "description": "refresh_token grant (rotated on use)" },
        "subject_token": { "type": "string", "description": "RFC 8693: token signed by this server" },
        "subject_token_type": { "type": "string" },
        "actor_token": { "type": "string" },
        "actor_token_type": { "type": "string" },
        "audience": { "type": "string", "description": "RFC 8693 target audience (repeatable)" },
        "requested_token_type": { "type": "string" }
    });
    if let (Some(t), Some(c)) = (token_props.as_object_mut(), client_auth.as_object()) {
        for (k, v) in c {
            t.insert(k.clone(), v.clone());
        }
    }
    let token_with_client = |extra: Value| {
        let mut props = client_auth.clone();
        if let (Some(p), Some(e)) = (props.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                p.insert(k.clone(), v.clone());
            }
        }
        props
    };
    let discovery = |op: &str, summary: &str| {
        json!({ "get": {
            "tags": ["Auth"], "summary": summary, "operationId": op,
            "responses": { "200": { "description": "Metadata", "content": { "application/json": { "schema": { "type": "object" } } } } }
        }})
    };
    json!({
        "/.well-known/openid-configuration": discovery("getOidcDiscovery", "OIDC discovery document"),
        "/.well-known/oauth-authorization-server": discovery("getOAuthServerMetadata", "OAuth 2.0 authorization server metadata (RFC 8414)"),
        "/oauth/jwks": { "get": {
            "tags": ["Auth"], "summary": "JWKS endpoint", "operationId": "getJwks",
            "description": "JSON Web Key Set with the RS256 public key.",
            "responses": { "200": { "description": "JWKS" } }
        }},
        "/oauth/token": { "post": {
            "tags": ["Auth"],
            "summary": "OAuth 2.0 token endpoint",
            "description": "Grants: authorization_code (+PKCE), refresh_token (rotating), client_credentials, password, RFC 8693 token exchange. Client auth: client_secret_basic, client_secret_post, or client_id only for public clients. Responses carry Cache-Control: no-store.",
            "operationId": "postOAuthToken",
            "requestBody": form(json!(["grant_type"]), token_props),
            "responses": {
                "200": { "description": "Token response", "content": { "application/json": { "schema": {
                    "type": "object",
                    "properties": {
                        "access_token": { "type": "string" },
                        "token_type": { "type": "string", "example": "Bearer" },
                        "expires_in": { "type": "integer", "example": 3600 },
                        "scope": { "type": "string" },
                        "id_token": { "type": "string" },
                        "refresh_token": { "type": "string" },
                        "issued_token_type": { "type": "string" }
                    }
                }}}},
                "400": { "description": "OAuth error (invalid_request, invalid_grant, unsupported_grant_type, invalid_scope, invalid_target, ...)", "content": error_content },
                "401": { "description": "invalid_client", "content": error_content }
            }
        }},
        "/oauth/authorize": {
            "get": {
                "tags": ["Auth"],
                "summary": "Authorization endpoint (login form)",
                "operationId": "getOAuthAuthorize",
                "parameters": [
                    { "name": "response_type", "in": "query", "required": true, "schema": { "type": "string", "enum": ["code"] } },
                    { "name": "client_id", "in": "query", "required": true, "schema": { "type": "string" } },
                    { "name": "redirect_uri", "in": "query", "schema": { "type": "string" } },
                    { "name": "scope", "in": "query", "schema": { "type": "string" } },
                    { "name": "state", "in": "query", "schema": { "type": "string" } },
                    { "name": "nonce", "in": "query", "schema": { "type": "string" } },
                    { "name": "code_challenge", "in": "query", "schema": { "type": "string" } },
                    { "name": "code_challenge_method", "in": "query", "schema": { "type": "string", "enum": ["S256", "plain"] } },
                    { "name": "resource", "in": "query", "schema": { "type": "string" } },
                    { "name": "login_hint", "in": "query", "schema": { "type": "string" } }
                ],
                "responses": {
                    "200": { "description": "HTML login form" },
                    "303": { "description": "Error redirect to the client" },
                    "400": { "description": "Unknown client or invalid redirect_uri (HTML)" }
                }
            },
            "post": {
                "tags": ["Auth"],
                "summary": "Authorization endpoint (submit login)",
                "operationId": "postOAuthAuthorize",
                "requestBody": form(json!(["client_id", "response_type", "username", "password"]), json!({
                    "username": { "type": "string" }, "password": { "type": "string" },
                    "client_id": { "type": "string" }, "redirect_uri": { "type": "string" },
                    "response_type": { "type": "string" }, "scope": { "type": "string" },
                    "state": { "type": "string" }, "nonce": { "type": "string" },
                    "code_challenge": { "type": "string" }, "code_challenge_method": { "type": "string" },
                    "resource": { "type": "string" }, "action": { "type": "string", "enum": ["approve", "deny"] }
                })),
                "responses": {
                    "303": { "description": "Redirect with code and state (or an error)" },
                    "401": { "description": "Invalid credentials (login form shown again)" }
                }
            }
        },
        "/oauth/userinfo": {
            "get": {
                "tags": ["Auth"], "summary": "OIDC UserInfo", "operationId": "getOAuthUserinfo",
                "security": [{ "bearerAuth": [] }],
                "responses": {
                    "200": { "description": "User claims", "content": { "application/json": { "schema": {
                        "type": "object",
                        "properties": {
                            "sub": { "type": "string" }, "name": { "type": "string" },
                            "preferred_username": { "type": "string" }, "email": { "type": "string" },
                            "email_verified": { "type": "boolean" }, "groups": { "type": "array", "items": { "type": "string" } }
                        }
                    }}}},
                    "401": { "description": "Missing or invalid token", "content": error_content },
                    "403": { "description": "insufficient_scope", "content": error_content }
                }
            },
            "post": {
                "tags": ["Auth"], "summary": "OIDC UserInfo (POST)", "operationId": "postOAuthUserinfo",
                "security": [{ "bearerAuth": [] }],
                "responses": {
                    "200": { "description": "User claims" },
                    "401": { "description": "Missing or invalid token", "content": error_content }
                }
            }
        },
        "/oauth/introspect": { "post": {
            "tags": ["Auth"], "summary": "Token introspection (RFC 7662)", "operationId": "postOAuthIntrospect",
            "requestBody": form(json!(["token"]), token_with_client(json!({ "token": { "type": "string" }, "token_type_hint": { "type": "string" } }))),
            "responses": {
                "200": { "description": "Introspection result (active=false for anything invalid)", "content": { "application/json": { "schema": {
                    "type": "object",
                    "properties": {
                        "active": { "type": "boolean" }, "scope": { "type": "string" }, "client_id": { "type": "string" },
                        "username": { "type": "string" }, "token_type": { "type": "string" }, "exp": { "type": "integer" },
                        "iat": { "type": "integer" }, "sub": { "type": "string" }, "aud": {}, "iss": { "type": "string" }, "jti": { "type": "string" }
                    }
                }}}},
                "401": { "description": "invalid_client", "content": error_content }
            }
        }},
        "/oauth/revoke": { "post": {
            "tags": ["Auth"], "summary": "Token revocation (RFC 7009)", "operationId": "postOAuthRevoke",
            "requestBody": form(json!(["token"]), token_with_client(json!({ "token": { "type": "string" }, "token_type_hint": { "type": "string" } }))),
            "responses": {
                "200": { "description": "Revoked (or the token was already invalid)" },
                "401": { "description": "invalid_client", "content": error_content }
            }
        }},
        "/oauth/register": { "post": {
            "tags": ["Auth"], "summary": "Dynamic client registration (RFC 7591)", "operationId": "postOAuthRegister",
            "requestBody": { "required": true, "content": { "application/json": { "schema": {
                "type": "object",
                "properties": {
                    "redirect_uris": { "type": "array", "items": { "type": "string" } },
                    "client_name": { "type": "string" },
                    "grant_types": { "type": "array", "items": { "type": "string" } },
                    "response_types": { "type": "array", "items": { "type": "string" } },
                    "token_endpoint_auth_method": { "type": "string", "enum": ["client_secret_basic", "client_secret_post", "none"] },
                    "scope": { "type": "string" }
                }
            }}}},
            "responses": {
                "201": { "description": "Registered client (client_id, client_secret, client_secret_expires_at, metadata)" },
                "400": { "description": "invalid_redirect_uri or invalid_client_metadata", "content": error_content }
            }
        }}
    })
}

#[cfg(test)]
mod tests;
