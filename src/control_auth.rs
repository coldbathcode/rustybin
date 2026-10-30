//! Control-plane authentication (`RUSTYBIN_CONTROL_AUTH`).
//!
//! Modes:
//! - `password` (default): the console signs in with a user name and
//!   password (`crate::login`, `rustybin` / `rustybin` out of the box) for a
//!   control token; the admin token and Ed25519 control JWTs are accepted
//!   as in `jwt` mode. `POST /_rustybin/login` is the one open route.
//! - `open`: the control plane is reachable without credentials
//!   (individual mutations still follow [`crate::admin::require_admin`]).
//! - `token`: every `/_rustybin/*` route except `/_rustybin/ready` needs the
//!   admin token (`Authorization: Bearer` or `X-Rustybin-Admin-Token`).
//! - `jwt`: like `token`, and an Ed25519 (EdDSA) JWT signed by the key in
//!   `RUSTYBIN_CONTROL_JWT_PUBLIC_KEY` is accepted too. It must carry
//!   `aud` == `RUSTYBIN_CONTROL_JWT_AUDIENCE` (default: the instance id) and
//!   `exp`. The optional `scope` claim (space separated string or array)
//!   grants:
//!   - `inspector`: read-only `/_rustybin/requests*`;
//!   - `console` (the default when `scope` is absent): read-only
//!     `/_rustybin/*`;
//!   - `harness`: the harness control routes (`/_rustybin/harness*`),
//!     reads and mutations (runs send load to allowlisted targets);
//!   - `admin`: everything, including mutations.
//!
//! Mutations (any method other than GET, HEAD, OPTIONS) need the admin token
//! or the `admin` scope. The exceptions are POST routes that only compute an
//! answer (previewing a mock spec, validating a body against a mock
//! operation, testing a mock's rules): the `console` scope may call them. Data-plane routes are never
//! affected.
//!
//! The web console (`/ui/*`) is a set of static files with no instance data:
//! it is served without credentials so a browser can load it and pick up a
//! token from the URL fragment (fragments never reach the server). Every
//! request it makes to `/_rustybin/*` carries that token.

use std::collections::HashSet;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde_json::{json, Value};

use crate::config::Config;
use crate::control::CONTROL_PREFIX;
use crate::state::AppState;

/// Readiness probe path, never authenticated.
pub const READY_PATH: &str = "/_rustybin/ready";
/// Longest token accepted (anything longer is rejected before parsing).
pub const MAX_TOKEN_LEN: usize = 8 * 1024;
/// Clock skew tolerated for `exp` and `nbf`, in seconds.
pub const LEEWAY_SECS: u64 = 30;

/// DER prefix of an Ed25519 `SubjectPublicKeyInfo` (followed by the 32 key bytes).
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// `RUSTYBIN_CONTROL_AUTH`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ControlAuth {
    Open,
    Token,
    Jwt,
    /// The console's user name and password (`crate::login`), plus the
    /// admin token and control JWTs. The default since 0.2.
    #[default]
    Password,
}

impl ControlAuth {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "open" | "off" | "none" => Some(Self::Open),
            "token" | "admin" => Some(Self::Token),
            "jwt" => Some(Self::Jwt),
            "password" => Some(Self::Password),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Token => "token",
            Self::Jwt => "jwt",
            Self::Password => "password",
        }
    }

    /// Every mode's name, for messages.
    pub const NAMES: &'static str = "password, open, token or jwt";
}

/// What an authenticated caller may do on the control plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    /// Admin token or `admin` scope: everything.
    pub admin: bool,
    /// `console` scope: read-only control plane.
    pub console: bool,
    /// `inspector` scope: read-only `/_rustybin/requests*`.
    pub inspector: bool,
    /// `harness` scope: `/_rustybin/harness*`, including starting and
    /// stopping runs.
    pub harness: bool,
}

impl Grant {
    pub const ADMIN: Grant = Grant {
        admin: true,
        console: true,
        inspector: true,
        harness: true,
    };

    /// Grants named by a `scope` claim (absent: `console`). Unknown scope
    /// names grant nothing.
    pub fn from_scope_claim(claim: Option<&Value>) -> Self {
        let names: Vec<String> = match claim {
            None | Some(Value::Null) => vec!["console".to_string()],
            Some(Value::String(s)) => s.split_whitespace().map(str::to_string).collect(),
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            Some(_) => Vec::new(),
        };
        let has = |n: &str| names.iter().any(|s| s == n);
        let admin = has("admin");
        Grant {
            admin,
            console: admin || has("console"),
            inspector: admin || has("console") || has("inspector"),
            harness: admin || has("harness"),
        }
    }

    /// Names of the granted scopes (for responses and logs).
    pub fn scopes(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.admin {
            v.push("admin");
        }
        if self.console {
            v.push("console");
        }
        if self.inspector {
            v.push("inspector");
        }
        if self.harness {
            v.push("harness");
        }
        v
    }

    /// May this grant call `method path`?
    pub fn allows(&self, method: &Method, path: &str) -> bool {
        if self.admin {
            return true;
        }
        // The management MCP server checks every tool against the grant
        // itself (read-only tools for the console and inspector scopes).
        if path == crate::mcp::manage::MANAGE_PATH {
            return true;
        }
        // The OAuth consent API caps what it grants at the approver's own
        // grant (any scope may approve what it holds).
        if crate::control_oauth::is_consent_path(path) {
            return true;
        }
        // The harness: reads for the console and harness scopes, runs and
        // stops only for the harness scope (and admin).
        if crate::harness::is_harness_path(path) {
            return self.harness || (is_safe(method) && self.console);
        }
        if !is_safe(method) {
            // The MCP inspector's relay sessions: the console ends its own.
            if method == Method::DELETE && crate::mcp::inspector::is_relay_session_path(path) {
                return self.console;
            }
            return self.console && method == Method::POST && is_read_only_post(path);
        }
        self.console || (self.inspector && is_inspector_path(path))
    }
}

/// Request extension set by [`enforce`] on authenticated control-plane
/// requests (the plan limiter never rejects them).
#[derive(Clone, Copy, Debug)]
pub struct ControlAuthorized(pub Grant);

fn is_safe(method: &Method) -> bool {
    method == Method::GET || method == Method::HEAD || method == Method::OPTIONS
}

fn under(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// POST routes that change nothing: `/_rustybin/mocks/import`,
/// `/_rustybin/mocks/{name}/operations/{op}/validate`,
/// `/_rustybin/mocks/{name}/operations/{op}/rules/test` and
/// `/_rustybin/assist/run` (read tasks; the handler checks the task).
fn is_read_only_post(path: &str) -> bool {
    if path == "/_rustybin/mocks/import"
        || path == crate::assist::RUN_PATH
        || crate::mcp::inspector::is_console_post(path)
        || crate::mcp::registry::is_console_post(path)
    {
        return true;
    }
    let Some(rest) = path.strip_prefix("/_rustybin/mocks/") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        [name, "operations", op, "validate"] | [name, "operations", op, "rules", "test"] => {
            !name.is_empty() && !op.is_empty()
        }
        _ => false,
    }
}

fn is_inspector_path(path: &str) -> bool {
    under(path, "/_rustybin/requests")
}

/// True for the paths [`enforce`] protects when control auth is on.
pub fn is_protected_path(path: &str) -> bool {
    under(path, CONTROL_PREFIX)
        && path != READY_PATH
        && !crate::control_oauth::is_public_path(path)
        && !crate::login::is_public_path(path)
}

/// The token a request presents: `X-Rustybin-Admin-Token`, else
/// `Authorization: Bearer`.
pub fn presented_token(headers: &HeaderMap) -> Option<&str> {
    if let Some(v) = headers
        .get(crate::admin::ADMIN_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        return Some(v.trim()).filter(|t| !t.is_empty());
    }
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            let (scheme, token) = v.trim().split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
        })
        .filter(|t| !t.is_empty())
}

/// Constant-time string comparison (length is not secret).
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Parse an Ed25519 public key: PEM (`-----BEGIN PUBLIC KEY-----`, newlines
/// may be written as `\n`), base64 of the DER `SubjectPublicKeyInfo`, or
/// base64 / base64url of the raw 32 bytes.
pub fn parse_public_key(raw: &str) -> Option<[u8; 32]> {
    let raw = raw.trim();
    let b64 = if raw.contains("-----BEGIN") {
        let parts: Vec<&str> = raw.split("-----").collect();
        let begin = parts.iter().position(|p| p.trim().starts_with("BEGIN"))?;
        parts.get(begin + 1)?.replace("\\n", "\n")
    } else {
        raw.to_string()
    };
    let compact: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    let der = [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD]
        .iter()
        .find_map(|engine| engine.decode(&compact).ok())?;
    let key: &[u8] = match der.len() {
        32 => &der,
        44 if der[..12] == ED25519_SPKI_PREFIX => &der[12..],
        _ => return None,
    };
    key.try_into().ok()
}

/// Why a JWT was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JwtError {
    /// No public key is configured (or the mode is not `jwt`).
    NotAccepted,
    Malformed,
    Expired,
    NotYetValid,
    Audience,
    Signature,
    Algorithm,
    MissingClaim,
    /// A sign-in token that was signed out (its id revoked, or issued
    /// before "sign out everywhere").
    Revoked,
}

impl JwtError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotAccepted => "jwt_not_accepted",
            Self::Malformed => "malformed",
            Self::Expired => "expired",
            Self::NotYetValid => "not_yet_valid",
            Self::Audience => "invalid_audience",
            Self::Signature => "invalid_signature",
            Self::Algorithm => "invalid_algorithm",
            Self::MissingClaim => "missing_claim",
            Self::Revoked => "revoked",
        }
    }
}

/// The payload of a JWT, read without verifying it: only for a token a
/// verification already accepted (naming the caller, never deciding).
pub fn unverified_claims(token: &str) -> Option<Value> {
    use base64::Engine;
    if token.len() > MAX_TOKEN_LEN {
        return None;
    }
    let mut parts = token.split('.');
    let (Some(_), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(Value::is_object)
}

/// Verify a control-plane JWT and return its grant.
pub fn verify_jwt(token: &str, config: &Config) -> Result<Grant, JwtError> {
    verify_jwt_claims(token, config).map(|(grant, _)| grant)
}

/// Verify a control-plane JWT and return its grant and claims (shared mode
/// reads the `rb_key` claim, see `crate::keys`). In `password` mode a
/// sign-in token (HS256, `crate::login`) is verified here too, so every
/// caller of this function accepts it.
pub fn verify_jwt_claims(token: &str, config: &Config) -> Result<(Grant, Value), JwtError> {
    match config.control_auth {
        ControlAuth::Password if crate::login::is_console_token(token) => {
            let claims = crate::login::verify(token, config)?;
            return Ok((Grant::from_scope_claim(claims.get("scope")), claims));
        }
        ControlAuth::Jwt | ControlAuth::Password => {}
        ControlAuth::Open | ControlAuth::Token => return Err(JwtError::NotAccepted),
    }
    let Some(key) = config.control_jwt_key.as_ref() else {
        return Err(JwtError::NotAccepted);
    };
    if token.len() > MAX_TOKEN_LEN || token.split('.').count() != 3 {
        return Err(JwtError::Malformed);
    }
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.leeway = LEEWAY_SECS;
    validation.validate_nbf = true;
    validation.set_audience(&[config.control_jwt_audience.as_str()]);
    validation.required_spec_claims = HashSet::from(["exp".to_string(), "aud".to_string()]);
    let data = jsonwebtoken::decode::<Value>(token, &DecodingKey::from_ed_der(key), &validation)
        .map_err(|e| {
            use jsonwebtoken::errors::ErrorKind as K;
            match e.kind() {
                K::ExpiredSignature => JwtError::Expired,
                K::ImmatureSignature => JwtError::NotYetValid,
                K::InvalidAudience => JwtError::Audience,
                K::InvalidSignature => JwtError::Signature,
                K::InvalidAlgorithm | K::InvalidAlgorithmName => JwtError::Algorithm,
                K::MissingRequiredClaim(_) => JwtError::MissingClaim,
                _ => JwtError::Malformed,
            }
        })?;
    // A token id revoked with its agent (Management, AI agents).
    if data
        .claims
        .get("jti")
        .and_then(Value::as_str)
        .is_some_and(|j| config.console_tokens.is_revoked(j))
    {
        return Err(JwtError::Revoked);
    }
    Ok((
        Grant::from_scope_claim(data.claims.get("scope")),
        data.claims,
    ))
}

/// The grant of a request's credentials, ignoring the mode's path rules:
/// the admin token (when configured) grants everything, a valid JWT (in
/// `jwt` mode) grants its scopes.
pub fn credentials_grant(headers: &HeaderMap, config: &Config) -> Result<Grant, Option<JwtError>> {
    let Some(token) = presented_token(headers) else {
        return Err(None);
    };
    if let Some(expected) = config.admin_token.as_deref() {
        if constant_time_eq(token, expected) {
            return Ok(Grant::ADMIN);
        }
    }
    if token.matches('.').count() != 2 {
        // Not shaped like a JWT: a wrong admin token, most likely.
        return Err(None);
    }
    match verify_jwt(token, config) {
        Ok(grant) => Ok(grant),
        Err(JwtError::NotAccepted) => Err(None),
        Err(e) => Err(Some(e)),
    }
}

fn unauthorized(config: &Config, message: &str, reason: &str) -> Response {
    let mut resp = (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": message,
            "reason": reason,
            "control_auth": config.control_auth.as_str(),
        })),
    )
        .into_response();
    let challenge = if reason == "missing_token" {
        "Bearer realm=\"rustybin-control\"".to_string()
    } else {
        format!("Bearer realm=\"rustybin-control\", error=\"invalid_token\", error_description=\"{reason}\"")
    };
    if let Ok(v) = HeaderValue::from_str(&challenge) {
        resp.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    resp
}

/// Decide whether a control-plane request may proceed.
#[allow(clippy::result_large_err)]
pub fn authorize(
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    config: &Config,
) -> Result<Grant, Response> {
    if presented_token(headers).is_none() {
        let hint = match config.control_auth {
            ControlAuth::Password => "control plane authentication required: sign in with POST /_rustybin/login (user name and password) and send the token as Authorization: Bearer, or send the admin token or a control JWT",
            ControlAuth::Jwt => "control plane authentication required: send Authorization: Bearer <admin token or control JWT>",
            _ => "control plane authentication required: send Authorization: Bearer <admin token> or X-Rustybin-Admin-Token",
        };
        return Err(unauthorized(config, hint, "missing_token"));
    }
    match credentials_grant(headers, config) {
        Ok(grant) if grant.allows(method, path) => Ok(grant),
        Ok(grant) => {
            let message = if is_safe(method) {
                "the token's scope does not include this control-plane route"
            } else {
                "control-plane mutations need the admin token or a token with the admin scope"
            };
            Err((
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": message,
                    "reason": "insufficient_scope",
                    "scopes": grant.scopes(),
                    "control_auth": config.control_auth.as_str(),
                })),
            )
                .into_response())
        }
        Err(Some(e)) => Err(unauthorized(
            config,
            "invalid control-plane token",
            e.as_str(),
        )),
        Err(None) => Err(unauthorized(
            config,
            "invalid control-plane token",
            "invalid_token",
        )),
    }
}

/// Middleware: protects `/_rustybin/*` (except `/_rustybin/ready` and the
/// sign-in routes) when the mode is `password`, `token` or `jwt`. A no-op
/// in `open` mode.
pub async fn enforce(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let config = &state.config;
    if config.control_auth == ControlAuth::Open || !is_protected_path(req.uri().path()) {
        return next.run(req).await;
    }
    // Harness cluster members calling their leader present the join token
    // or their node credential, which the handlers check.
    if crate::harness::is_node_path(req.method(), req.uri().path()) {
        return next.run(req).await;
    }
    // A token from the built-in OAuth server, on /_rustybin/mcp only.
    match crate::control_oauth::authorize_manage(&req, config) {
        Some(Ok(grant)) => {
            req.extensions_mut().insert(ControlAuthorized(grant));
            req.extensions_mut().insert(crate::control_oauth::ViaOAuth);
            return next.run(req).await;
        }
        Some(Err(resp)) => return resp,
        None => {}
    }
    // CORS preflights carry no credentials; the CORS layer answers them.
    match authorize(req.method(), req.uri().path(), req.headers(), config) {
        Ok(grant) => {
            req.extensions_mut().insert(ControlAuthorized(grant));
            next.run(req).await
        }
        Err(mut resp) => {
            crate::control_oauth::add_challenge(
                &mut resp,
                req.headers(),
                req.extensions(),
                req.uri(),
                config,
            );
            resp
        }
    }
}

#[cfg(test)]
mod tests;
