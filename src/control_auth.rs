//! Control-plane authentication (`RUSTYBIN_CONTROL_AUTH`) and the one
//! permission check.
//!
//! Modes:
//! - `password` (default): the console signs in with a user name and
//!   password (`crate::login`, `rustybin` / `rustybin` out of the box) for a
//!   control token; the admin token and Ed25519 control JWTs are accepted
//!   as in `jwt` mode. `POST /_rustybin/login` is the one open route.
//! - `open`: the control plane is reachable without credentials; a caller
//!   without the admin token holds every permission when there is no admin
//!   token outside public mode, else the anonymous reads
//!   (`crate::rbac::need::ANONYMOUS`).
//! - `token`: every `/_rustybin/*` route except the open ones needs the
//!   admin token (`Authorization: Bearer` or `X-Rustybin-Admin-Token`),
//!   which is every permission.
//! - `jwt`: like `token`, and an Ed25519 (EdDSA) JWT signed by the key in
//!   `RUSTYBIN_CONTROL_JWT_PUBLIC_KEY` is accepted too. It must carry
//!   `aud` == `RUSTYBIN_CONTROL_JWT_AUDIENCE` (default: the instance id) and
//!   `exp`. Its permissions come from the `scope` claim (the 0.4 scopes as
//!   roles, see `crate::rbac::scope_role`), `rb_roles` and `rb_perms`.
//!
//! Two steps: [`enforce`] (a layer outside routing) authenticates, `401`
//! without a credential the instance accepts; [`check_route`] (a route
//! layer, so it sees the matched route template) checks the permission the
//! catalogue declares for the route and method before the handler runs,
//! `403 {"reason": "permission_required", "permission": ...}` naming what is
//! missing. Data-plane routes are never affected.
//!
//! The web console (`/ui/*`) is a set of static files with no instance data:
//! it is served without credentials so a browser can load it and pick up a
//! token from the URL fragment (fragments never reach the server). Every
//! request it makes to `/_rustybin/*` carries that token.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::{MatchedPath, Request, State};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde_json::{json, Value};

use crate::config::Config;
use crate::control::CONTROL_PREFIX;
use crate::rbac::{Need, Resolved};
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

/// The 0.4 scopes a caller's permissions amount to (answers, logs, the
/// OAuth consent cap). Never a decision: routes and tools check
/// permissions.
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
    /// names grant nothing. Derived from the permissions the scopes map to
    /// (`crate::rbac::from_scope_claim`), which give every 0.3 scope the
    /// grant it had.
    pub fn from_scope_claim(claim: Option<&Value>) -> Self {
        crate::rbac::grant_of(crate::rbac::from_scope_claim(claim).1)
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
}

/// Request extension set by [`enforce`] (and, in shared mode, by the key
/// layer for the admin) on authenticated control-plane requests: the
/// caller's roles and permissions. The plan limiter never rejects them.
#[derive(Clone, Debug)]
pub struct ControlAuthorized(pub Arc<Resolved>);

impl ControlAuthorized {
    /// The 0.4 scopes the permissions amount to (answers and logs).
    pub fn grant(&self) -> Grant {
        self.0.grant()
    }
}

/// Request extension set by [`check_route`] on every control request it
/// lets through: the caller it checked (also on an open control plane and
/// for API keys in shared mode), for the handlers' body checks.
#[derive(Clone, Debug)]
pub struct ControlCaller(pub Arc<Resolved>);

/// The permission a project-scoped route declares for the request's method
/// (step 11): the service checks it again on each resource, in the
/// resource's project.
#[derive(Clone, Copy, Debug)]
pub struct RouteNeed(pub crate::rbac::Permission);

/// The caller of a request: what [`check_route`] or [`enforce`] resolved,
/// else (an open control plane, a data-plane route) the presented admin
/// token, an API key in shared mode, or nobody ([`Resolved::anonymous`]).
pub fn caller_of(state: &AppState, headers: &HeaderMap, extensions: &Extensions) -> Arc<Resolved> {
    if let Some(c) = extensions.get::<ControlCaller>() {
        return c.0.clone();
    }
    if let Some(a) = extensions.get::<ControlAuthorized>() {
        return a.0.clone();
    }
    if let Some(k) = extensions
        .get::<crate::keys::KeyContext>()
        .filter(|k| k.shared && k.control)
    {
        return Arc::new(Resolved::key(&k.key.id));
    }
    Arc::new(open_caller(headers, &state.config))
}

/// The caller of a request that no control-plane layer authenticated (an
/// open control plane, a data-plane route): the admin token (or, outside
/// `open` mode, a control JWT) as it resolves, else nobody, which is every
/// permission without an admin token outside public mode and the
/// anonymous reads otherwise (the 0.4 admin guard, as permissions).
pub fn open_caller(headers: &HeaderMap, config: &Config) -> Resolved {
    credentials_resolved(headers, config).unwrap_or_else(|_| Resolved::anonymous(config))
}

fn under(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// True for the paths [`enforce`] protects when control auth is on.
pub fn is_protected_path(path: &str) -> bool {
    under(path, CONTROL_PREFIX)
        && path != READY_PATH
        && !crate::control_oauth::is_public_path(path)
        && !crate::login::is_public_path(path)
        && !crate::sso::is_public_path(path)
        && !crate::sourcecontrol::is_public_path(path)
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
    verify_jwt_resolved(token, config).map(|(r, claims)| (r.grant(), claims))
}

/// Verify a control-plane JWT and resolve its roles and permissions
/// (`crate::rbac`): a sign-in token from its user, at every call (a role
/// change or a disable applies to the next request); a control JWT from
/// its `scope`, `rb_roles` and `rb_perms`.
pub fn verify_jwt_resolved(
    token: &str,
    config: &Config,
) -> Result<(crate::rbac::Resolved, Value), JwtError> {
    match config.control_auth {
        ControlAuth::Password if crate::login::is_console_token(token) => {
            return crate::login::verify_resolved(token, config).map(|(c, r)| (r, c));
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
    let resolved = crate::rbac::from_jwt_claims(&data.claims, &config.users);
    Ok((resolved, data.claims))
}

/// The roles and permissions of a request's credentials: the admin token
/// (when configured) is every permission, a valid JWT resolves as
/// [`verify_jwt_resolved`] says. `Err(None)`: no credential, or one that is
/// not a token this instance accepts.
pub fn credentials_resolved(
    headers: &HeaderMap,
    config: &Config,
) -> Result<crate::rbac::Resolved, Option<JwtError>> {
    let Some(token) = presented_token(headers) else {
        return Err(None);
    };
    if let Some(expected) = config.admin_token.as_deref() {
        if constant_time_eq(token, expected) {
            return Ok(crate::rbac::Resolved::admin_token());
        }
    }
    if token.matches('.').count() != 2 {
        // Not shaped like a JWT: a wrong admin token, most likely.
        return Err(None);
    }
    match verify_jwt_resolved(token, config) {
        Ok((r, _)) => Ok(r),
        Err(JwtError::NotAccepted) => Err(None),
        Err(e) => Err(Some(e)),
    }
}

/// The grant of a request's credentials, ignoring the mode's path rules:
/// the admin token (when configured) grants everything, a valid JWT (in
/// `jwt` mode) grants its scopes.
pub fn credentials_grant(headers: &HeaderMap, config: &Config) -> Result<Grant, Option<JwtError>> {
    credentials_resolved(headers, config).map(|r| r.grant())
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

/// Authenticate a control-plane request: the caller's roles and
/// permissions, or the `401`.
#[allow(clippy::result_large_err)]
pub fn authenticate(headers: &HeaderMap, config: &Config) -> Result<Resolved, Response> {
    if presented_token(headers).is_none() {
        let hint = match config.control_auth {
            ControlAuth::Password => "control plane authentication required: sign in with POST /_rustybin/login (user name and password) and send the token as Authorization: Bearer, or send the admin token or a control JWT",
            ControlAuth::Jwt => "control plane authentication required: send Authorization: Bearer <admin token or control JWT>",
            _ => "control plane authentication required: send Authorization: Bearer <admin token> or X-Rustybin-Admin-Token",
        };
        return Err(unauthorized(config, hint, "missing_token"));
    }
    match credentials_resolved(headers, config) {
        Ok(r) => Ok(r),
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
    // A visitor of a self-service instance with a sign-in: `check_route`
    // holds them to what a caller without a credential may do.
    if crate::login::optional(config) && presented_token(req.headers()).is_none() {
        return next.run(req).await;
    }
    // A token from the built-in OAuth server, on /_rustybin/mcp only.
    match crate::control_oauth::authorize_manage(&req, config) {
        Some(Ok(resolved)) => {
            req.extensions_mut()
                .insert(ControlAuthorized(Arc::new(resolved)));
            req.extensions_mut().insert(crate::control_oauth::ViaOAuth);
            return next.run(req).await;
        }
        Some(Err(resp)) => return resp,
        None => {}
    }
    // CORS preflights carry no credentials; the CORS layer answers them.
    // The permission is checked once the route is known (`check_route`).
    match authenticate(req.headers(), config) {
        Ok(resolved) => {
            req.extensions_mut()
                .insert(ControlAuthorized(Arc::new(resolved)));
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

/// Route layer (`build_app`): the one permission check. For a control
/// route it looks up what the catalogue declares for the matched template
/// and the method, resolves the caller ([`caller_of`]) and answers the
/// refusal when the caller lacks the permission; the handler runs only
/// when it holds it. A route whose need is undeclared is refused (the
/// parity test `every_control_route_declares_a_permission` keeps that from
/// happening), and so is a control route the catalogue does not know
/// (fail closed: a route registered outside the catalogue never runs for
/// any credential). On a self-service instance the routes that say so let
/// a visitor without the permission through to the per-mock rules (a
/// visitor or an API key manages its own mocks only): a caller acting as a
/// visitor (no credential, an API key, or a visit's manage token). Anyone
/// else (a user, a control JWT) is held to the permission, and the route's
/// permission goes on to the per-mock rules, which hold it on the
/// instance's mocks.
pub async fn check_route(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let Some(template) = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str())
        .filter(|t| under(t, CONTROL_PREFIX))
    else {
        return next.run(req).await;
    };
    let Some(endpoint) = crate::catalog::control_route(template) else {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "this route is not in the catalogue, so it declares no permission",
                "reason": "permission_required",
                "permission": null,
            })),
        )
            .into_response();
    };
    // A method the route does not have: the router answers 405.
    let method = match req.method().as_str() {
        "HEAD" => "GET",
        m => m,
    };
    if !endpoint.expanded_methods().contains(&method) {
        return next.run(req).await;
    }
    let need = endpoint.need(method);
    if need == Some(Need::Open) {
        return next.run(req).await;
    }
    let caller = caller_of(&state, req.headers(), req.extensions());
    match need {
        None => {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "this route declares no permission",
                    "reason": "permission_required",
                    "permission": null,
                })),
            )
                .into_response()
        }
        Some(Need::Perm(p))
            if !caller.has(p)
                && !(endpoint.project && caller.has_somewhere(p))
                && !(endpoint.self_service
                    && state.mocks.visits().is_some()
                    && acts_as_visitor(&caller, req.headers())) =>
        {
            return crate::rbac::refuse(&caller, p);
        }
        // Past `enforce` without a credential only on a self-service instance
        // with a sign-in: "signed in" then means it, but for a visit's own
        // routes (the manage token) and the management MCP server (each tool
        // checks its own need, and a visitor has tools).
        Some(Need::Authenticated)
            if crate::login::optional(&state.config)
                && matches!(caller.principal, crate::rbac::Principal::Anonymous { .. })
                && !endpoint.self_service
                && endpoint.path != crate::mcp::manage::MANAGE_PATH =>
        {
            if let Err(refused) = authenticate(req.headers(), &state.config) {
                return refused;
            }
        }
        _ => {}
    }
    // The per-mock rules hold the route's permission again on the mock: its
    // project's roles for a project route, the instance's for a visitor let
    // through above.
    if let Some(Need::Perm(p)) = need {
        if endpoint.project || (endpoint.self_service && !caller.has(p)) {
            req.extensions_mut().insert(RouteNeed(p));
        }
    }
    req.extensions_mut().insert(ControlCaller(caller.clone()));
    let mut resp = next.run(req).await;
    // Who called, for the audit log (`crate::audit::layer`, outside).
    if resp.extensions().get::<ControlCaller>().is_none() {
        resp.extensions_mut().insert(ControlCaller(caller));
    }
    resp
}

/// Does the caller act as a self-service visitor: no credential, an API
/// key, or a visit's manage token (`X-Rustybin-Mock-Token`)?
fn acts_as_visitor(caller: &Resolved, headers: &HeaderMap) -> bool {
    matches!(
        caller.principal,
        crate::rbac::Principal::Anonymous { .. } | crate::rbac::Principal::Key { .. }
    ) || headers.contains_key(crate::mocks::visits::TOKEN_HEADER)
}

#[cfg(test)]
mod tests;
