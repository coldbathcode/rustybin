//! Control-plane authentication (`RUSTYBIN_CONTROL_AUTH`).
//!
//! Modes:
//! - `open` (default): the control plane is reachable without credentials
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
//!   - `admin`: everything, including mutations.
//!
//! Mutations (any method other than GET, HEAD, OPTIONS) need the admin token
//! or the `admin` scope. Data-plane routes are never affected.
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
    #[default]
    Open,
    Token,
    Jwt,
}

impl ControlAuth {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "open" | "off" | "none" => Some(Self::Open),
            "token" | "admin" => Some(Self::Token),
            "jwt" => Some(Self::Jwt),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Token => "token",
            Self::Jwt => "jwt",
        }
    }
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
}

impl Grant {
    pub const ADMIN: Grant = Grant {
        admin: true,
        console: true,
        inspector: true,
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
        v
    }

    /// May this grant call `method path`?
    pub fn allows(&self, method: &Method, path: &str) -> bool {
        if self.admin {
            return true;
        }
        if !is_safe(method) {
            return false;
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

fn is_inspector_path(path: &str) -> bool {
    under(path, "/_rustybin/requests")
}

/// True for the paths [`enforce`] protects when control auth is on.
pub fn is_protected_path(path: &str) -> bool {
    under(path, CONTROL_PREFIX) && path != READY_PATH
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
        }
    }
}

/// Verify a control-plane JWT and return its grant.
pub fn verify_jwt(token: &str, config: &Config) -> Result<Grant, JwtError> {
    if config.control_auth != ControlAuth::Jwt {
        return Err(JwtError::NotAccepted);
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
    Ok(Grant::from_scope_claim(data.claims.get("scope")))
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

/// Middleware: protects `/_rustybin/*` (except `/_rustybin/ready`) when the
/// mode is `token` or `jwt`. A no-op in `open` mode.
pub async fn enforce(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let config = &state.config;
    if config.control_auth == ControlAuth::Open || !is_protected_path(req.uri().path()) {
        return next.run(req).await;
    }
    // CORS preflights carry no credentials; the CORS layer answers them.
    match authorize(req.method(), req.uri().path(), req.headers(), config) {
        Ok(grant) => {
            req.extensions_mut().insert(ControlAuthorized(grant));
            next.run(req).await
        }
        Err(resp) => resp,
    }
}

#[cfg(test)]
mod tests;
