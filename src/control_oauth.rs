//! OAuth sign-in to the management MCP server (`docs/plans/control-oauth.md`).
//!
//! Where the control plane asks for credentials (`RUSTYBIN_CONTROL_AUTH`
//! `token` or `jwt`, not shared mode), `/_rustybin/mcp` answers `401` with an
//! RFC 9728 `resource_metadata` pointer, so an MCP client can sign in through
//! the browser instead of the user pasting a token:
//!
//! - `GET /.well-known/oauth-protected-resource/_rustybin/mcp`: the protected
//!   resource metadata, naming the built-in authorization server
//!   (`<base>/_rustybin/oauth`) or `RUSTYBIN_CONTROL_OAUTH_ISSUER`;
//! - the built-in server: RFC 8414 metadata, RFC 7591 registration (public
//!   clients, stateless client ids signed with the instance key), the
//!   authorization endpoint (redirects to the console's consent page), the
//!   consent API the console calls with its own credential (the grant is
//!   capped by the approver's), and the token endpoint (PKCE S256, refresh
//!   tokens).
//!
//! Tokens are HS256 JWTs signed with [`OAuthKey`], audience-bound to this
//! instance's `/_rustybin/mcp` URL, and accepted there only: every other
//! control-plane route and the data plane's admin guard refuse them.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use hmac::{Hmac, Mac};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::control_auth::{ControlAuth, ControlAuthorized, Grant};
use crate::mcp::manage::MANAGE_PATH;
use crate::oidc::store::BoundedStore;
use crate::state::AppState;

/// Issuer path of the built-in authorization server.
pub const ISSUER_PATH: &str = "/_rustybin/oauth";
pub const PRM_PATH: &str = "/.well-known/oauth-protected-resource/_rustybin/mcp";
pub const AS_METADATA_PATH: &str = "/.well-known/oauth-authorization-server/_rustybin/oauth";
pub const OIDC_METADATA_PATH: &str = "/.well-known/openid-configuration/_rustybin/oauth";
pub const REGISTER_PATH: &str = "/_rustybin/oauth/register";
pub const AUTHORIZE_PATH: &str = "/_rustybin/oauth/authorize";
pub const TOKEN_PATH: &str = "/_rustybin/oauth/token";
pub const REQUESTS_PATH: &str = "/_rustybin/oauth/requests/{id}";
const REQUESTS_PREFIX: &str = "/_rustybin/oauth/requests/";
/// The console's consent page (the request id follows).
pub const CONSENT_PAGE: &str = "/ui/#/authorize/";

/// Scopes, most powerful first (the control-plane scopes).
pub const SCOPES: &[&str] = &["admin", "console", "inspector"];
pub const ACCESS_TTL_SECS: i64 = 3600;
pub const REFRESH_TTL_SECS: i64 = 30 * 24 * 3600;
pub const PENDING_TTL_SECS: i64 = 600;
pub const CODE_TTL_SECS: i64 = 120;
pub const MAX_PENDING: usize = 1000;
pub const MAX_PENDING_PUBLIC: usize = 200;
const MAX_REDIRECT_URIS: usize = 3;
const MAX_REDIRECT_URI_LEN: usize = 512;
const MAX_CLIENT_NAME: usize = 80;
const MAX_CLIENT_ID_LEN: usize = 4096;
const MAX_STATE_LEN: usize = 1024;
const MAX_REGISTRATION_BYTES: usize = 16 * 1024;
/// Clock skew tolerated for `exp`, in seconds.
const LEEWAY_SECS: u64 = 30;

type HmacSha256 = Hmac<Sha256>;

// ── Key ─────────────────────────────────────────────────────────────

/// Where the key came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    /// `RUSTYBIN_CONTROL_OAUTH_KEY`.
    Setting,
    /// Derived from `RUSTYBIN_ADMIN_TOKEN` (rotating it signs everyone out).
    AdminToken,
    /// Random per process (tokens end with a restart).
    Random,
}

impl KeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Setting => "setting",
            Self::AdminToken => "admin_token",
            Self::Random => "random",
        }
    }
}

/// The key signing client ids and tokens of the built-in server. Its
/// `Debug` never shows the bytes.
#[derive(Clone)]
pub struct OAuthKey {
    bytes: [u8; 32],
    source: KeySource,
}

impl std::fmt::Debug for OAuthKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OAuthKey([redacted], {})", self.source.as_str())
    }
}

impl OAuthKey {
    /// From `RUSTYBIN_CONTROL_OAUTH_KEY` (already checked to be long
    /// enough), else the admin token, else random.
    pub fn from_settings(explicit: Option<&str>, admin_token: Option<&str>) -> Self {
        let (seed, source) = match (explicit, admin_token) {
            (Some(k), _) => (
                format!("rustybin-control-oauth\nsetting\n{}", k.trim()),
                KeySource::Setting,
            ),
            (None, Some(t)) => (
                format!("rustybin-control-oauth\nadmin-token\n{t}"),
                KeySource::AdminToken,
            ),
            (None, None) => {
                let mut bytes = [0u8; 32];
                rand::thread_rng().fill_bytes(&mut bytes);
                return Self {
                    bytes,
                    source: KeySource::Random,
                };
            }
        };
        Self {
            bytes: Sha256::digest(seed.as_bytes()).into(),
            source,
        }
    }

    pub fn source(&self) -> KeySource {
        self.source
    }

    /// HMAC-SHA256 of `data` under a purpose label (client ids and token
    /// signing use separate subkeys).
    fn mac(&self, label: &str, data: &[u8]) -> [u8; 32] {
        let Ok(mut m) = HmacSha256::new_from_slice(&self.bytes) else {
            return [0u8; 32];
        };
        m.update(label.as_bytes());
        m.update(&[0]);
        m.update(data);
        m.finalize().into_bytes().into()
    }

    fn jwt_secret(&self) -> [u8; 32] {
        self.mac("jwt", b"")
    }
}

// ── Modes ───────────────────────────────────────────────────────────

/// Is OAuth sign-in offered for `/_rustybin/mcp`? Only where the control
/// plane asks for credentials, outside shared mode (keys authenticate
/// there), unless `RUSTYBIN_CONTROL_OAUTH=off`.
pub fn enabled(config: &Config) -> bool {
    config.control_oauth
        && matches!(config.control_auth, ControlAuth::Token | ControlAuth::Jwt)
        && !config.shared_mode
}

/// Does this instance run the built-in authorization server?
pub fn builtin(config: &Config) -> bool {
    enabled(config) && config.control_oauth_issuer.is_none()
}

/// `off`, `built-in` or `external` (for the public config view).
pub fn mode(config: &Config) -> &'static str {
    if !enabled(config) {
        "off"
    } else if builtin(config) {
        "built-in"
    } else {
        "external"
    }
}

/// Control-plane paths the OAuth flow needs without credentials.
pub fn is_public_path(path: &str) -> bool {
    matches!(path, REGISTER_PATH | AUTHORIZE_PATH | TOKEN_PATH)
}

/// The discovery documents (public metadata, no instance data).
pub fn is_discovery_path(path: &str) -> bool {
    matches!(path, PRM_PATH | AS_METADATA_PATH | OIDC_METADATA_PATH)
}

/// The consent API, which checks the approver's grant itself (any scope
/// may approve what it holds).
pub fn is_consent_path(path: &str) -> bool {
    path.strip_prefix(REQUESTS_PREFIX)
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

/// Request extension: the caller authenticated with a token from the
/// built-in server (an `admin` scope is admin on the management server).
#[derive(Clone, Copy, Debug)]
pub struct ViaOAuth;

// ── URLs ────────────────────────────────────────────────────────────

fn base_url(headers: &HeaderMap, extensions: &Extensions, uri: &Uri, config: &Config) -> String {
    crate::session::request_origin(headers, extensions, uri, config).base_url()
}

fn issuer_url(base: &str, config: &Config) -> String {
    config
        .control_oauth_issuer
        .clone()
        .unwrap_or_else(|| format!("{base}{ISSUER_PATH}"))
}

fn resource_url(base: &str) -> String {
    format!("{base}{MANAGE_PATH}")
}

fn metadata_url(base: &str) -> String {
    format!("{base}{PRM_PATH}")
}

// ── Checks on /_rustybin/mcp (called by `control_auth::enforce`) ─────

/// A token from the built-in server presented to `/_rustybin/mcp`:
/// `Some(Ok(grant))` when valid, `Some(Err(401))` when it is one of ours but
/// invalid, `None` when the request carries something else (the admin token,
/// a control JWT, nothing), which control auth then checks as usual.
#[allow(clippy::result_large_err)]
pub fn authorize_manage(req: &Request, config: &Config) -> Option<Result<Grant, Response>> {
    if !builtin(config) || req.uri().path() != MANAGE_PATH {
        return None;
    }
    let token = crate::control_auth::presented_token(req.headers())?;
    if token.len() > crate::control_auth::MAX_TOKEN_LEN || token.matches('.').count() != 2 {
        return None;
    }
    if config
        .admin_token
        .as_deref()
        .is_some_and(|t| crate::control_auth::constant_time_eq(t, token))
    {
        return None;
    }
    let ours = jsonwebtoken::decode_header(token).is_ok_and(|h| h.alg == Algorithm::HS256);
    if !ours {
        return None;
    }
    let base = base_url(req.headers(), req.extensions(), req.uri(), config);
    Some(
        verify(token, &resource_url(&base), "access", config)
            .map(|claims| Grant::from_scope_claim(claims.get("scope")))
            .map_err(|reason| {
                let mut resp = (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({
                        "error": "invalid control-plane token",
                        "reason": reason,
                        "control_auth": config.control_auth.as_str(),
                    })),
                )
                    .into_response();
                let challenge = format!(
                    "Bearer realm=\"rustybin-control\", error=\"invalid_token\", \
                     error_description=\"{reason}\", resource_metadata=\"{}\"",
                    metadata_url(&base)
                );
                if let Ok(v) = HeaderValue::from_str(&challenge) {
                    resp.headers_mut().insert(header::WWW_AUTHENTICATE, v);
                }
                resp
            }),
    )
}

/// Add `resource_metadata` to a `401` challenge from `/_rustybin/mcp`.
pub fn add_challenge(
    resp: &mut Response,
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
    config: &Config,
) {
    if !enabled(config) || uri.path() != MANAGE_PATH || resp.status() != StatusCode::UNAUTHORIZED {
        return;
    }
    let base = base_url(headers, extensions, uri, config);
    let current = resp
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("Bearer realm=\"rustybin-control\"")
        .to_string();
    let value = format!("{current}, resource_metadata=\"{}\"", metadata_url(&base));
    if let Ok(v) = HeaderValue::from_str(&value) {
        resp.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
}

// ── Tokens ──────────────────────────────────────────────────────────

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn random_id() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    B64.encode(b)
}

/// Short, non-secret label of a client id (logs, `sub`).
fn fingerprint(client_id: &str) -> String {
    let digest = Sha256::digest(client_id.as_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

fn sign(claims: &Value, config: &Config) -> Option<String> {
    let mut header = Header::new(Algorithm::HS256);
    header.typ = Some("JWT".into());
    jsonwebtoken::encode(
        &header,
        claims,
        &EncodingKey::from_secret(&config.control_oauth_key.jwt_secret()),
    )
    .ok()
}

/// Verify one of our tokens for `audience` and `use_` (`access` or
/// `refresh`); returns its claims or the reason it was refused.
fn verify(token: &str, audience: &str, use_: &str, config: &Config) -> Result<Value, &'static str> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = LEEWAY_SECS;
    validation.set_audience(&[audience]);
    validation.required_spec_claims =
        std::collections::HashSet::from(["exp".to_string(), "aud".to_string()]);
    let data = jsonwebtoken::decode::<Value>(
        token,
        &DecodingKey::from_secret(&config.control_oauth_key.jwt_secret()),
        &validation,
    )
    .map_err(|e| {
        use jsonwebtoken::errors::ErrorKind as K;
        match e.kind() {
            K::ExpiredSignature => "expired",
            K::InvalidAudience => "invalid_audience",
            K::InvalidSignature => "invalid_signature",
            _ => "malformed",
        }
    })?;
    if data.claims.get("rb_use").and_then(Value::as_str) != Some(use_) {
        return Err("wrong_token_type");
    }
    Ok(data.claims)
}

/// A token response for `scopes`: an access token and a refresh token.
#[allow(clippy::result_large_err)]
fn issue(
    client_id: &str,
    scopes: &[&'static str],
    resource: &str,
    issuer: &str,
    config: &Config,
) -> Result<Value, Response> {
    let scope = scopes.join(" ");
    let iat = now();
    let claims = |use_: &str, ttl: i64| {
        json!({
            "iss": issuer,
            "aud": resource,
            "sub": format!("client:{}", fingerprint(client_id)),
            "client_id": client_id,
            "scope": scope,
            "iat": iat,
            "exp": iat + ttl,
            "jti": uuid::Uuid::new_v4().to_string(),
            "rb_use": use_,
        })
    };
    let (Some(access), Some(refresh)) = (
        sign(&claims("access", ACCESS_TTL_SECS), config),
        sign(&claims("refresh", REFRESH_TTL_SECS), config),
    ) else {
        return Err(oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "could not sign the token",
        ));
    };
    Ok(json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": ACCESS_TTL_SECS,
        "refresh_token": refresh,
        "scope": scope,
    }))
}

// ── Scopes ──────────────────────────────────────────────────────────

/// Parse a space separated scope list; `None` when a name is unknown.
/// Absent or empty asks for everything (the approver's grant caps it).
fn parse_scopes(raw: Option<&str>) -> Option<Vec<&'static str>> {
    let raw = raw.map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return Some(SCOPES.to_vec());
    }
    let mut out = Vec::new();
    for name in raw.split_whitespace() {
        let s = SCOPES.iter().find(|s| **s == name)?;
        if !out.contains(s) {
            out.push(*s);
        }
    }
    // Keep the canonical order (most powerful first).
    out.sort_by_key(|s| SCOPES.iter().position(|x| x == s));
    Some(out)
}

fn grant_allows(grant: &Grant, scope: &str) -> bool {
    match scope {
        "admin" => grant.admin,
        "console" => grant.console,
        "inspector" => grant.inspector,
        _ => false,
    }
}

// ── Clients ─────────────────────────────────────────────────────────

/// A registered client, carried inside its signed client id.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Client {
    redirect_uris: Vec<String>,
    name: String,
}

fn client_id(client: &Client, config: &Config) -> String {
    let payload = B64.encode(json!({ "r": client.redirect_uris, "n": client.name }).to_string());
    let sig = B64.encode(config.control_oauth_key.mac("client", payload.as_bytes()));
    format!("rbc.{payload}.{sig}")
}

fn parse_client(id: &str, config: &Config) -> Option<Client> {
    if id.len() > MAX_CLIENT_ID_LEN {
        return None;
    }
    let (payload, sig) = id.strip_prefix("rbc.")?.split_once('.')?;
    let expected = B64.encode(config.control_oauth_key.mac("client", payload.as_bytes()));
    if !crate::types::constant_time_eq(expected.as_bytes(), sig.as_bytes()) {
        return None;
    }
    let v: Value = serde_json::from_slice(&B64.decode(payload).ok()?).ok()?;
    let redirect_uris = v["r"]
        .as_array()?
        .iter()
        .map(|u| u.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    Some(Client {
        redirect_uris,
        name: v["n"].as_str().unwrap_or("").to_string(),
    })
}

/// URI scheme (lower-cased) if it is syntactically one.
fn scheme_of(uri: &str) -> Option<String> {
    let (scheme, _) = uri.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then(|| scheme.to_ascii_lowercase())
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Redirect URIs a client may register: `https`, `http` on a loopback
/// host, or a private-use scheme of a native app; never a scheme that runs
/// or reads something in the browser.
fn validate_redirect_uri(uri: &str) -> Result<(), &'static str> {
    if uri.is_empty() || uri.len() > MAX_REDIRECT_URI_LEN {
        return Err("redirect URIs must be 1 to 512 characters");
    }
    if uri.contains('#') {
        return Err("redirect URIs must not contain a fragment");
    }
    if uri.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("redirect URIs must not contain whitespace or control characters");
    }
    let scheme = scheme_of(uri).ok_or("redirect URIs must be absolute")?;
    match scheme.as_str() {
        "https" | "http" => {
            let parsed: Uri = uri.parse().map_err(|_| "invalid redirect URI")?;
            let host = parsed.host().ok_or("redirect URIs must have a host")?;
            if scheme == "http" && !is_loopback_host(host) {
                return Err("http redirect URIs must point at a loopback address (localhost, 127.0.0.1, [::1]); use https otherwise");
            }
            Ok(())
        }
        "javascript" | "data" | "file" | "vbscript" | "about" | "blob" | "filesystem" | "ws"
        | "wss" | "ftp" | "mailto" | "tel" => Err("this redirect URI scheme is not allowed"),
        _ => Ok(()),
    }
}

/// A loopback `http` redirect URI without its port (RFC 8252 7.3: native
/// apps pick a free port at authorization time).
fn loopback_without_port(uri: &str) -> Option<String> {
    let parsed: Uri = uri.parse().ok()?;
    if parsed.scheme_str() != Some("http") || !parsed.host().is_some_and(is_loopback_host) {
        return None;
    }
    Some(format!(
        "{}{}",
        parsed.host().unwrap_or_default(),
        parsed.path_and_query().map(|p| p.as_str()).unwrap_or("/")
    ))
}

fn redirect_matches(registered: &str, given: &str) -> bool {
    registered == given
        || matches!(
            (loopback_without_port(registered), loopback_without_port(given)),
            (Some(a), Some(b)) if a == b
        )
}

/// `uri` with `params` appended to its query.
fn with_params(uri: &str, params: &[(&str, &str)]) -> String {
    let mut q = form_urlencoded::Serializer::new(String::new());
    for (k, v) in params {
        q.append_pair(k, v);
    }
    let sep = if !uri.contains('?') {
        "?"
    } else if uri.ends_with('?') || uri.ends_with('&') {
        ""
    } else {
        "&"
    };
    format!("{uri}{sep}{}", q.finish())
}

// ── State ───────────────────────────────────────────────────────────

/// An authorization request waiting for the consent page.
#[derive(Clone)]
struct Pending {
    client_id: String,
    client_name: String,
    redirect_uri: String,
    state: Option<String>,
    challenge: String,
    scopes: Vec<&'static str>,
    resource: String,
    issuer: String,
}

/// An authorization code (single use).
#[derive(Clone)]
struct Code {
    client_id: String,
    redirect_uri: String,
    challenge: String,
    scopes: Vec<&'static str>,
    resource: String,
    issuer: String,
}

pub struct OAuthState {
    pending: BoundedStore<Pending>,
    codes: BoundedStore<Code>,
}

impl OAuthState {
    pub fn new(public_mode: bool) -> Self {
        let cap = if public_mode {
            MAX_PENDING_PUBLIC
        } else {
            MAX_PENDING
        };
        Self {
            pending: BoundedStore::new(cap, PENDING_TTL_SECS),
            codes: BoundedStore::new(cap, CODE_TTL_SECS),
        }
    }
}

type St = Extension<Arc<OAuthState>>;

pub fn router(state: &AppState) -> Router<AppState> {
    let st = Arc::new(OAuthState::new(state.config.public_mode));
    Router::new()
        .route(PRM_PATH, get(prm_handler))
        .route(AS_METADATA_PATH, get(as_metadata_handler))
        .route(OIDC_METADATA_PATH, get(as_metadata_handler))
        .route(REGISTER_PATH, post(register_handler))
        .route(AUTHORIZE_PATH, get(authorize_handler))
        .route(
            REQUESTS_PATH,
            get(request_get_handler).post(request_post_handler),
        )
        .route(TOKEN_PATH, post(token_handler))
        .layer(Extension(st))
}

// ── Responses ───────────────────────────────────────────────────────

fn not_found(uri: &Uri) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": "not found",
            "path": uri.path(),
            "hint": "OAuth sign-in to /_rustybin/mcp is offered only when RUSTYBIN_CONTROL_AUTH is token or jwt (not in shared mode, not with RUSTYBIN_CONTROL_OAUTH=off); the built-in authorization server only without RUSTYBIN_CONTROL_OAUTH_ISSUER",
        })),
    )
        .into_response()
}

fn no_store(mut resp: Response) -> Response {
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp.headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    resp
}

fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    no_store(
        (
            status,
            Json(json!({ "error": error, "error_description": description })),
        )
            .into_response(),
    )
}

fn error_page(status: StatusCode, message: &str) -> Response {
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" \
         content=\"width=device-width, initial-scale=1\"><title>Sign-in failed</title></head>\
         <body style=\"font-family: system-ui, sans-serif; max-width: 40rem; margin: 3rem auto; \
         padding: 0 1rem\"><h1>Sign-in failed</h1><p>{}</p><p>Start the sign-in again from your \
         agent.</p></body></html>",
        crate::landing::html_escape(message)
    );
    let mut resp = (status, Html(body)).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn found(location: &str) -> Response {
    match HeaderValue::from_str(location) {
        Ok(v) => {
            let mut resp = StatusCode::FOUND.into_response();
            resp.headers_mut().insert(header::LOCATION, v);
            no_store(resp)
        }
        Err(_) => error_page(StatusCode::BAD_REQUEST, "the redirect URI cannot be used"),
    }
}

fn audit(event: &str, client_id: &str, scopes: &[&str], approver: Option<&Grant>) {
    tracing::info!(
        target: "rustybin::control_oauth",
        event,
        client = %fingerprint(client_id),
        scopes = %scopes.join(" "),
        approver = %approver.map(|g| g.scopes().join(" ")).unwrap_or_default(),
        "management MCP sign-in"
    );
}

// ── Metadata ────────────────────────────────────────────────────────

async fn prm_handler(State(app): State<AppState>, req: Request) -> Response {
    let config = &app.config;
    if !enabled(config) {
        return not_found(req.uri());
    }
    let base = base_url(req.headers(), req.extensions(), req.uri(), config);
    Json(json!({
        "resource": resource_url(&base),
        "authorization_servers": [issuer_url(&base, config)],
        "scopes_supported": SCOPES,
        "bearer_methods_supported": ["header"],
        "resource_name": "Rustybin management MCP",
        "resource_documentation": format!("{base}/ui/#/mocks"),
    }))
    .into_response()
}

async fn as_metadata_handler(State(app): State<AppState>, req: Request) -> Response {
    let config = &app.config;
    if !builtin(config) {
        return not_found(req.uri());
    }
    let base = base_url(req.headers(), req.extensions(), req.uri(), config);
    Json(json!({
        "issuer": issuer_url(&base, config),
        "authorization_endpoint": format!("{base}{AUTHORIZE_PATH}"),
        "token_endpoint": format!("{base}{TOKEN_PATH}"),
        "registration_endpoint": format!("{base}{REGISTER_PATH}"),
        "scopes_supported": SCOPES,
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["none"],
        "code_challenge_methods_supported": ["S256"],
        "authorization_response_iss_parameter_supported": true,
        "client_id_metadata_document_supported": false,
    }))
    .into_response()
}

// ── Registration (RFC 7591) ─────────────────────────────────────────

async fn register_handler(State(app): State<AppState>, uri: Uri, body: Bytes) -> Response {
    let config = &app.config;
    if !builtin(config) {
        return not_found(&uri);
    }
    let bad =
        |error: &str, description: &str| oauth_error(StatusCode::BAD_REQUEST, error, description);
    if body.len() > MAX_REGISTRATION_BYTES {
        return bad("invalid_client_metadata", "registration body too large");
    }
    let Ok(meta) = serde_json::from_slice::<Value>(&body) else {
        return bad("invalid_client_metadata", "the body must be a JSON object");
    };
    let Some(uris) = meta.get("redirect_uris").and_then(Value::as_array) else {
        return bad("invalid_redirect_uri", "redirect_uris is required");
    };
    if uris.is_empty() || uris.len() > MAX_REDIRECT_URIS {
        return bad("invalid_redirect_uri", "register 1 to 3 redirect URIs");
    }
    let mut redirect_uris = Vec::new();
    for u in uris {
        let Some(u) = u.as_str() else {
            return bad("invalid_redirect_uri", "redirect URIs must be strings");
        };
        if let Err(e) = validate_redirect_uri(u) {
            return bad("invalid_redirect_uri", e);
        }
        redirect_uris.push(u.to_string());
    }
    if let Some(m) = meta
        .get("token_endpoint_auth_method")
        .and_then(Value::as_str)
    {
        if m != "none" {
            return bad(
                "invalid_client_metadata",
                "only public clients are supported (token_endpoint_auth_method none, with PKCE)",
            );
        }
    }
    if let Some(gt) = meta.get("grant_types").and_then(Value::as_array) {
        if gt
            .iter()
            .any(|g| !matches!(g.as_str(), Some("authorization_code" | "refresh_token")))
        {
            return bad(
                "invalid_client_metadata",
                "grant_types may only be authorization_code and refresh_token",
            );
        }
    }
    if let Some(rt) = meta.get("response_types").and_then(Value::as_array) {
        if rt.iter().any(|r| r.as_str() != Some("code")) {
            return bad("invalid_client_metadata", "response_types may only be code");
        }
    }
    let name: String = meta
        .get("client_name")
        .and_then(Value::as_str)
        .unwrap_or("MCP client")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_CLIENT_NAME)
        .collect();
    let client = Client {
        redirect_uris,
        name,
    };
    let id = client_id(&client, config);
    audit("register", &id, &[], None);
    no_store(
        (
            StatusCode::CREATED,
            Json(json!({
                "client_id": id,
                "client_id_issued_at": now(),
                "client_name": client.name,
                "redirect_uris": client.redirect_uris,
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "none",
            })),
        )
            .into_response(),
    )
}

// ── Authorization endpoint ──────────────────────────────────────────

fn query_map(uri: &Uri) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (k, v) in form_urlencoded::parse(uri.query().unwrap_or("").as_bytes()) {
        map.entry(k.into_owned()).or_insert_with(|| v.into_owned());
    }
    map
}

async fn authorize_handler(State(app): State<AppState>, st: St, req: Request) -> Response {
    let config = &app.config;
    if !builtin(config) {
        return not_found(req.uri());
    }
    let q = query_map(req.uri());
    let base = base_url(req.headers(), req.extensions(), req.uri(), config);
    let issuer = issuer_url(&base, config);

    // Errors before the redirect URI is known to be the client's: a page,
    // never a redirect (RFC 6749 4.1.2.1).
    let Some(id) = q.get("client_id") else {
        return error_page(StatusCode::BAD_REQUEST, "client_id is missing.");
    };
    let Some(client) = parse_client(id, config) else {
        return error_page(
            StatusCode::BAD_REQUEST,
            "Unknown client: register again (client ids from another instance, or from before a key change, are not accepted).",
        );
    };
    let redirect_uri = match q.get("redirect_uri") {
        Some(given) => {
            if !client
                .redirect_uris
                .iter()
                .any(|r| redirect_matches(r, given))
            {
                return error_page(
                    StatusCode::BAD_REQUEST,
                    "redirect_uri is not one the client registered.",
                );
            }
            given.clone()
        }
        None if client.redirect_uris.len() == 1 => client.redirect_uris[0].clone(),
        None => {
            return error_page(
                StatusCode::BAD_REQUEST,
                "redirect_uri is required when the client registered several.",
            )
        }
    };
    let state = q.get("state").cloned();
    if state.as_ref().is_some_and(|s| s.len() > MAX_STATE_LEN) {
        return error_page(StatusCode::BAD_REQUEST, "state is too long.");
    }
    // Registration is open, so a redirect to a registered https or app URI
    // proves nothing about the client: errors go back automatically only to
    // a loopback listener (an agent waiting for its callback), and show a
    // page otherwise (RFC 9700 4.11.2). Approvals and denials redirect after
    // the user acted on the consent page.
    let loopback = loopback_without_port(&redirect_uri).is_some();
    let fail = |error: &str, description: &str| {
        if !loopback {
            return error_page(StatusCode::BAD_REQUEST, &format!("{error}: {description}"));
        }
        let mut params = vec![("error", error), ("error_description", description)];
        if let Some(s) = &state {
            params.push(("state", s.as_str()));
        }
        params.push(("iss", issuer.as_str()));
        found(&with_params(&redirect_uri, &params))
    };
    if q.get("response_type").map(String::as_str) != Some("code") {
        return fail("unsupported_response_type", "response_type must be code");
    }
    let Some(challenge) = q.get("code_challenge") else {
        return fail(
            "invalid_request",
            "PKCE is required: send code_challenge (S256)",
        );
    };
    if q.get("code_challenge_method").map(String::as_str) != Some("S256") {
        return fail("invalid_request", "code_challenge_method must be S256");
    }
    if !crate::oidc::valid_pkce_value(challenge) {
        return fail("invalid_request", "invalid code_challenge");
    }
    let Some(scopes) = parse_scopes(q.get("scope").map(String::as_str)) else {
        return fail("invalid_scope", "scopes are admin, console and inspector");
    };
    let resource = resource_url(&base);
    if let Some(r) = q.get("resource") {
        if r.trim_end_matches('/') != resource {
            return fail(
                "invalid_target",
                "resource must be this instance's /_rustybin/mcp URL",
            );
        }
    }
    let request_id = random_id();
    st.pending.insert(
        request_id.clone(),
        Pending {
            client_id: id.clone(),
            client_name: client.name,
            redirect_uri,
            state,
            challenge: challenge.clone(),
            scopes,
            resource,
            issuer,
        },
    );
    found(&format!("{CONSENT_PAGE}{request_id}"))
}

// ── Consent API (the console) ───────────────────────────────────────

fn redirect_kind(uri: &str) -> &'static str {
    if loopback_without_port(uri).is_some() {
        "loopback"
    } else if scheme_of(uri).as_deref() == Some("https") {
        "web"
    } else {
        "app"
    }
}

fn approver(extensions: &Extensions) -> Option<Grant> {
    extensions.get::<ControlAuthorized>().map(|g| g.0)
}

fn consent_error(status: StatusCode, message: &str) -> Response {
    no_store((status, Json(json!({ "error": message }))).into_response())
}

async fn request_get_handler(
    State(app): State<AppState>,
    st: St,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    if !builtin(&app.config) {
        return not_found(req.uri());
    }
    let Some(grant) = approver(req.extensions()) else {
        return consent_error(
            StatusCode::UNAUTHORIZED,
            "control-plane credentials required",
        );
    };
    let Some((p, expires_at)) = st.pending.get_with_expiry(&id) else {
        return consent_error(
            StatusCode::NOT_FOUND,
            "this sign-in request is unknown or expired: start it again from the agent",
        );
    };
    let grantable: Vec<&str> = p
        .scopes
        .iter()
        .copied()
        .filter(|s| grant_allows(&grant, s))
        .collect();
    no_store(
        Json(json!({
            "id": id,
            "client_name": p.client_name,
            "client": fingerprint(&p.client_id),
            "redirect_uri": p.redirect_uri,
            "redirect_kind": redirect_kind(&p.redirect_uri),
            "resource": p.resource,
            "scopes_requested": p.scopes,
            "scopes_grantable": grantable,
            "expires_in": (expires_at - now()).max(0),
        }))
        .into_response(),
    )
}

async fn request_post_handler(
    State(app): State<AppState>,
    st: St,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    if !builtin(&app.config) {
        return not_found(req.uri());
    }
    let Some(grant) = approver(req.extensions()) else {
        return consent_error(
            StatusCode::UNAUTHORIZED,
            "control-plane credentials required",
        );
    };
    let limit = MAX_REGISTRATION_BYTES;
    let Ok(body) = axum::body::to_bytes(req.into_body(), limit).await else {
        return consent_error(StatusCode::BAD_REQUEST, "body too large");
    };
    let Ok(input) = serde_json::from_slice::<Value>(&body) else {
        return consent_error(StatusCode::BAD_REQUEST, "the body must be JSON");
    };
    let decision = input.get("decision").and_then(Value::as_str);
    if !matches!(decision, Some("approve" | "deny")) {
        return consent_error(StatusCode::BAD_REQUEST, "decision must be approve or deny");
    }
    let Some(p) = st.pending.get(&id) else {
        return consent_error(
            StatusCode::NOT_FOUND,
            "this sign-in request is unknown or expired: start it again from the agent",
        );
    };
    let mut params: Vec<(&str, String)> = Vec::new();
    if decision == Some("deny") {
        st.pending.remove(&id);
        audit("deny", &p.client_id, &p.scopes, Some(&grant));
        params.push(("error", "access_denied".into()));
        params.push(("error_description", "the user denied the request".into()));
    } else {
        let chosen = match input.get("scope") {
            None | Some(Value::Null) => Some(p.scopes.clone()),
            Some(Value::String(s)) => parse_scopes(Some(s)),
            Some(Value::Array(a)) => parse_scopes(Some(
                &a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
            )),
            Some(_) => None,
        };
        let Some(chosen) = chosen else {
            return consent_error(
                StatusCode::BAD_REQUEST,
                "scope must name admin, console or inspector",
            );
        };
        if chosen.iter().any(|s| !p.scopes.contains(s)) {
            return consent_error(
                StatusCode::BAD_REQUEST,
                "the scope chosen must be among the scopes the agent asked for",
            );
        }
        let scopes: Vec<&'static str> = chosen
            .into_iter()
            .filter(|s| grant_allows(&grant, s))
            .collect();
        if scopes.is_empty() {
            return consent_error(
                StatusCode::FORBIDDEN,
                "your credential cannot grant any of the scopes asked for",
            );
        }
        // Single use: a second approval of the same request finds nothing.
        if st.pending.take(&id).is_none() {
            return consent_error(
                StatusCode::NOT_FOUND,
                "this sign-in request was already answered",
            );
        }
        let code = random_id();
        st.codes.insert(
            code.clone(),
            Code {
                client_id: p.client_id.clone(),
                redirect_uri: p.redirect_uri.clone(),
                challenge: p.challenge.clone(),
                scopes: scopes.clone(),
                resource: p.resource.clone(),
                issuer: p.issuer.clone(),
            },
        );
        audit("approve", &p.client_id, &scopes, Some(&grant));
        params.push(("code", code));
    }
    if let Some(s) = &p.state {
        params.push(("state", s.clone()));
    }
    params.push(("iss", p.issuer.clone()));
    let pairs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    no_store(
        Json(json!({
            "redirect_to": with_params(&p.redirect_uri, &pairs),
            "redirect_kind": redirect_kind(&p.redirect_uri),
        }))
        .into_response(),
    )
}

// ── Token endpoint ──────────────────────────────────────────────────

async fn token_handler(State(app): State<AppState>, st: St, req: Request) -> Response {
    let config = app.config.clone();
    if !builtin(&config) {
        return not_found(req.uri());
    }
    let base = base_url(req.headers(), req.extensions(), req.uri(), &config);
    let Ok(body) = axum::body::to_bytes(req.into_body(), MAX_REGISTRATION_BYTES).await else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "body too large");
    };
    let mut f: HashMap<String, String> = HashMap::new();
    for (k, v) in form_urlencoded::parse(&body) {
        f.entry(k.into_owned()).or_insert_with(|| v.into_owned());
    }
    let bad =
        |error: &str, description: &str| oauth_error(StatusCode::BAD_REQUEST, error, description);
    let Some(client_id) = f.get("client_id") else {
        return oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "client_id is required (public clients only)",
        );
    };
    if parse_client(client_id, &config).is_none() {
        return oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "unknown client: register again",
        );
    }
    let result = match f.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            let (Some(code), Some(verifier)) = (f.get("code"), f.get("code_verifier")) else {
                return bad("invalid_request", "code and code_verifier are required");
            };
            let Some(entry) = st.codes.take(code) else {
                return bad("invalid_grant", "invalid, expired or already used code");
            };
            if &entry.client_id != client_id {
                return bad("invalid_grant", "the code was issued to another client");
            }
            if f.get("redirect_uri")
                .is_some_and(|r| r != &entry.redirect_uri)
            {
                return bad(
                    "invalid_grant",
                    "redirect_uri does not match the authorization request",
                );
            }
            if !crate::oidc::valid_pkce_value(verifier)
                || !crate::oidc::pkce_matches(verifier, &entry.challenge, "S256")
            {
                return bad(
                    "invalid_grant",
                    "code_verifier does not match the code_challenge",
                );
            }
            if f.get("resource")
                .is_some_and(|r| r.trim_end_matches('/') != entry.resource)
            {
                return bad(
                    "invalid_target",
                    "resource does not match the authorization request",
                );
            }
            audit("token", client_id, &entry.scopes, None);
            issue(
                client_id,
                &entry.scopes,
                &entry.resource,
                &entry.issuer,
                &config,
            )
        }
        Some("refresh_token") => {
            let Some(rt) = f.get("refresh_token") else {
                return bad("invalid_request", "refresh_token is required");
            };
            let resource = resource_url(&base);
            let claims = match verify(rt, &resource, "refresh", &config) {
                Ok(c) => c,
                Err(reason) => {
                    return bad("invalid_grant", &format!("refresh token refused: {reason}"))
                }
            };
            if claims.get("client_id").and_then(Value::as_str) != Some(client_id.as_str()) {
                return bad(
                    "invalid_grant",
                    "the refresh token was issued to another client",
                );
            }
            let Some(held) = parse_scopes(claims.get("scope").and_then(Value::as_str)) else {
                return bad("invalid_grant", "malformed refresh token");
            };
            let scopes = match f.get("scope") {
                Some(s) => match parse_scopes(Some(s)) {
                    Some(asked) if asked.iter().all(|a| held.contains(a)) => asked,
                    _ => return bad("invalid_scope", "a refresh may only narrow the scopes"),
                },
                None => held,
            };
            let issuer = claims
                .get("iss")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| issuer_url(&base, &config));
            audit("refresh", client_id, &scopes, None);
            issue(client_id, &scopes, &resource, &issuer, &config)
        }
        _ => {
            return bad(
                "unsupported_grant_type",
                "grant_type must be authorization_code or refresh_token",
            )
        }
    };
    match result {
        Ok(v) => no_store(Json(v).into_response()),
        Err(resp) => resp,
    }
}

// ── Catalogue and OpenAPI ───────────────────────────────────────────

const OFF_IN_OPEN_MODE: &str = "only with RUSTYBIN_CONTROL_AUTH token or jwt";

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(PRM_PATH, &["GET"], category::CONTROL, "Management MCP: protected resource metadata (RFC 9728)")
            .description("Names the authorization server agents sign in with to use /_rustybin/mcp: the built-in one at /_rustybin/oauth, or RUSTYBIN_CONTROL_OAUTH_ISSUER. Only where the control plane asks for credentials (token or jwt control auth, not shared mode).")
            .example(Example::get("Protected resource metadata", PRM_PATH).skip_check(OFF_IN_OPEN_MODE)),
        Endpoint::new(AS_METADATA_PATH, &["GET"], category::CONTROL, "Management MCP sign-in: authorization server metadata (RFC 8414)")
            .example(Example::get("Authorization server metadata", AS_METADATA_PATH).skip_check(OFF_IN_OPEN_MODE)),
        Endpoint::new(OIDC_METADATA_PATH, &["GET"], category::CONTROL, "Management MCP sign-in: the same metadata at the OpenID Connect discovery path"),
        Endpoint::new(REGISTER_PATH, &["POST"], category::CONTROL, "Management MCP sign-in: dynamic client registration (RFC 7591, public clients)"),
        Endpoint::new(AUTHORIZE_PATH, &["GET"], category::CONTROL, "Management MCP sign-in: authorization endpoint (redirects to the console's consent page)"),
        Endpoint::new(REQUESTS_PATH, &["GET", "POST"], category::CONTROL, "Management MCP sign-in: a pending request, approved or denied by the console"),
        Endpoint::new(TOKEN_PATH, &["POST"], category::CONTROL, "Management MCP sign-in: token endpoint (authorization code with PKCE, refresh token)"),
    ]
}

pub fn openapi_paths() -> Value {
    let tag = json!(["Control Plane"]);
    let authorize_params: Vec<Value> = [
        "response_type",
        "client_id",
        "redirect_uri",
        "code_challenge",
        "code_challenge_method",
        "scope",
        "state",
        "resource",
    ]
    .iter()
    .map(|n| {
        let required = matches!(
            *n,
            "response_type" | "client_id" | "code_challenge" | "code_challenge_method"
        );
        json!({ "name": n, "in": "query", "required": required, "schema": { "type": "string" } })
    })
    .collect();
    let ok = |d: &str| json!({ "200": { "description": d }, "404": { "description": "OAuth sign-in is off on this instance" } });
    json!({
        PRM_PATH: { "get": { "tags": tag, "summary": "Protected resource metadata of /_rustybin/mcp (RFC 9728)", "operationId": "getManageMcpResourceMetadata", "responses": ok("Metadata") } },
        AS_METADATA_PATH: { "get": { "tags": tag, "summary": "Authorization server metadata (RFC 8414)", "operationId": "getControlOAuthMetadata", "responses": ok("Metadata") } },
        OIDC_METADATA_PATH: { "get": { "tags": tag, "summary": "Authorization server metadata (OpenID Connect discovery path)", "operationId": "getControlOAuthOidcMetadata", "responses": ok("Metadata") } },
        REGISTER_PATH: { "post": {
            "tags": tag, "summary": "Register a public client (RFC 7591)", "operationId": "registerControlOAuthClient",
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["redirect_uris"], "properties": {
                "redirect_uris": { "type": "array", "items": { "type": "string" }, "maxItems": 3 },
                "client_name": { "type": "string" },
                "grant_types": { "type": "array", "items": { "type": "string" } },
                "token_endpoint_auth_method": { "type": "string", "enum": ["none"] }
            } } } } },
            "responses": { "201": { "description": "Registered" }, "400": { "description": "Invalid metadata" }, "404": { "description": "OAuth sign-in is off" } }
        } },
        AUTHORIZE_PATH: { "get": {
            "tags": tag, "summary": "Authorization endpoint: redirects to the console's consent page", "operationId": "authorizeControlOAuth",
            "parameters": authorize_params,
            "responses": { "302": { "description": "To the consent page, or back to the client with an error" }, "400": { "description": "Unknown client or redirect URI (HTML page)" }, "404": { "description": "OAuth sign-in is off" } }
        } },
        REQUESTS_PATH: {
            "get": { "tags": tag, "summary": "A pending sign-in request (for the consent page)", "operationId": "getControlOAuthRequest",
                     "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }],
                     "responses": { "200": { "description": "The request" }, "401": { "description": "Control-plane credentials required" }, "404": { "description": "Unknown or expired" } } },
            "post": { "tags": tag, "summary": "Approve or deny a pending sign-in request", "operationId": "answerControlOAuthRequest",
                      "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }],
                      "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["decision"], "properties": {
                          "decision": { "type": "string", "enum": ["approve", "deny"] },
                          "scope": { "type": "string", "description": "Space separated: admin, console, inspector (default: what the agent asked for, capped by the approver's grant)" }
                      } } } } },
                      "responses": { "200": { "description": "Where to send the browser (redirect_to)" }, "403": { "description": "The approver's credential cannot grant the scopes" }, "404": { "description": "Unknown or expired" } } }
        },
        TOKEN_PATH: { "post": {
            "tags": tag, "summary": "Token endpoint (authorization_code with PKCE S256, refresh_token)", "operationId": "controlOAuthToken",
            "requestBody": { "required": true, "content": { "application/x-www-form-urlencoded": { "schema": { "type": "object", "required": ["grant_type", "client_id"], "properties": {
                "grant_type": { "type": "string", "enum": ["authorization_code", "refresh_token"] },
                "client_id": { "type": "string" }, "code": { "type": "string" }, "code_verifier": { "type": "string" },
                "redirect_uri": { "type": "string" }, "refresh_token": { "type": "string" }, "scope": { "type": "string" }, "resource": { "type": "string" }
            } } } } },
            "responses": { "200": { "description": "Tokens" }, "400": { "description": "OAuth error" }, "401": { "description": "Unknown client" }, "404": { "description": "OAuth sign-in is off" } }
        } },
    })
}

#[cfg(test)]
mod tests;
