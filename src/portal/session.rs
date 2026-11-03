//! The API portal's sign-in: a user name and password for a session cookie.
//!
//! `POST /portal/api/login` checks the user exactly as the console sign-in
//! does (`crate::login::check_sign_in`: the same throttle per address bucket
//! and user name and for every address together, the same answers, a
//! disabled user answered as a wrong password) and sets `rustybin_portal`
//! (`HttpOnly`, `SameSite=Strict`, `Secure` behind HTTPS, `Path=/portal`, 12
//! hours): an HS256 token signed with its own subkey of the OAuth key
//! (`portal`), `rb_use: "portal"`, the user id and the generations of the
//! user store and of the sign-in (so "sign out everywhere", a disabled user
//! or a moved user generation end it). Nothing outside `/portal` accepts
//! it: the browser sends it under `/portal` only, and the control plane, the
//! data plane and `/_rustybin/mcp` read bearer credentials that it never
//! verifies as (its subkey and its `rb_use` differ from every other token).
//! Permissions are resolved at every request, so a role removed or a user
//! disabled loses the closed items on the next one.
//!
//! A reader of closed items ([`can_read_closed`]) is a portal session whose
//! user holds `portal.read`, else a bearer control credential (the admin
//! token, a control JWT, a console sign-in token) that resolves it, for
//! scripts that download collections.

use axum::extract::State;
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::Config;
use crate::control_auth::JwtError;
use crate::rbac::{Permission, Principal, Resolved};
use crate::state::AppState;

/// The cookie's name.
pub const COOKIE: &str = "rustybin_portal";
/// Sign in (`POST`).
pub const LOGIN_PATH: &str = "/portal/api/login";
/// Sign out (`POST`).
pub const LOGOUT_PATH: &str = "/portal/api/logout";
/// Who is signed in (`GET`).
pub const SESSION_PATH: &str = "/portal/api/session";
/// The cookie's lifetime, the console sign-in's.
pub const TTL_SECS: i64 = crate::login::TOKEN_TTL_SECS;
/// The `rb_use` claim of a portal session.
const TOKEN_USE: &str = "portal";
const LEEWAY_SECS: u64 = 30;

/// Is the portal sign-in on? With the password sign-in (users exist only
/// there).
pub fn enabled(config: &Config) -> bool {
    crate::login::enabled(config)
}

fn random_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A portal session token for `user`: the token and its claims.
fn mint(config: &Config, user: &crate::rbac::User) -> Option<(String, Value)> {
    let tokens = &config.console_tokens;
    let iat = tokens.now();
    let claims = json!({
        "iss": "rustybin",
        "aud": config.control_jwt_audience,
        "sub": user.id,
        "rb_user": user.name,
        "iat": iat,
        "exp": iat + TTL_SECS,
        "jti": random_id(),
        "rb_use": TOKEN_USE,
        "rb_gen": tokens.generation(),
        "rb_ugen": user.generation,
    });
    let mut h = Header::new(Algorithm::HS256);
    h.typ = Some("JWT".into());
    let token = jsonwebtoken::encode(
        &h,
        &claims,
        &EncodingKey::from_secret(&config.control_oauth_key.portal_secret()),
    )
    .ok()?;
    Some((token, claims))
}

/// Verify a portal session token: signature (the `portal` subkey),
/// audience, expiry, `rb_use`, the sign-in generation, the denylist and its
/// user (there, enabled, of the user's generation). The claims and the
/// user's roles and permissions as they are now.
pub fn verify(token: &str, config: &Config) -> Result<(Value, Resolved), JwtError> {
    if token.len() > crate::control_auth::MAX_TOKEN_LEN {
        return Err(JwtError::Malformed);
    }
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = LEEWAY_SECS;
    validation.set_audience(&[config.control_jwt_audience.as_str()]);
    validation.required_spec_claims =
        std::collections::HashSet::from(["exp".to_string(), "aud".to_string()]);
    let data = jsonwebtoken::decode::<Value>(
        token,
        &DecodingKey::from_secret(&config.control_oauth_key.portal_secret()),
        &validation,
    )
    .map_err(|e| {
        use jsonwebtoken::errors::ErrorKind as K;
        match e.kind() {
            K::ExpiredSignature => JwtError::Expired,
            K::InvalidSignature => JwtError::Signature,
            K::InvalidAudience => JwtError::Audience,
            _ => JwtError::Malformed,
        }
    })?;
    let claims = data.claims;
    if claims.get("rb_use").and_then(Value::as_str) != Some(TOKEN_USE) {
        return Err(JwtError::Malformed);
    }
    let tokens = &config.console_tokens;
    if claims.get("rb_gen").and_then(Value::as_u64) != Some(tokens.generation()) {
        return Err(JwtError::Revoked);
    }
    let jti = claims.get("jti").and_then(Value::as_str).unwrap_or("");
    if jti.is_empty() || tokens.is_revoked(jti) {
        return Err(JwtError::Revoked);
    }
    let resolved = crate::rbac::from_sign_in(&claims, &config.users)?;
    Ok((claims, resolved))
}

/// The value of the `rustybin_portal` cookie a request carries.
pub fn cookie_value(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty())
}

/// The verified portal session of a request (only with the sign-in on).
pub fn session(state: &AppState, headers: &HeaderMap) -> Option<(Value, Resolved)> {
    if !enabled(&state.config) {
        return None;
    }
    verify(cookie_value(headers)?, &state.config).ok()
}

/// Who reads the portal: the portal session, else a bearer control
/// credential; `via` says which (`cookie`, `bearer`).
pub fn reader(state: &AppState, headers: &HeaderMap) -> Option<(Resolved, &'static str)> {
    if let Some((_, r)) = session(state, headers) {
        return Some((r, "cookie"));
    }
    crate::control_auth::credentials_resolved(headers, &state.config)
        .ok()
        .map(|r| (r, "bearer"))
}

/// May this request use closed items (Try it, credentials, collections,
/// connection snippets)? A reader holding `portal.read`.
pub fn can_read_closed(state: &AppState, headers: &HeaderMap) -> bool {
    reader(state, headers).is_some_and(|(r, _)| r.has(Permission::PortalRead))
}

fn secure(headers: &HeaderMap, extensions: &Extensions, uri: &Uri, config: &Config) -> bool {
    crate::session::request_origin(headers, extensions, uri, config).scheme == "https"
}

fn cookie_header(value: &str, max_age: i64, secure: bool) -> Option<HeaderValue> {
    let mut c = format!(
        "{COOKIE}={value}; Path={}; Max-Age={max_age}; HttpOnly; SameSite=Strict",
        super::PREFIX
    );
    if secure {
        c.push_str("; Secure");
    }
    HeaderValue::from_str(&c).ok()
}

/// Mint a portal session for `user` and the `Set-Cookie` that carries it
/// (`None` when it could not be signed).
pub fn issue_cookie(
    state: &AppState,
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
    user: &crate::rbac::User,
) -> Option<HeaderValue> {
    let (token, _) = mint(&state.config, user)?;
    cookie_header(
        &token,
        TTL_SECS,
        secure(headers, extensions, uri, &state.config),
    )
}

fn audit(event: &str, user: &str, outcome: &str) {
    tracing::info!(
        target: "rustybin::auth",
        event,
        user = %crate::logging::printable(user, crate::login::MAX_USER_CHARS),
        outcome,
        "portal sign-in"
    );
}

fn no_store(mut resp: Response) -> Response {
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn sign_in_off() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": "the portal sign-in is off",
            "reason": "signin_off",
            "hint": "users sign in only with the password sign-in (RUSTYBIN_CONTROL_AUTH=password, the default, outside shared mode); send a control credential as Authorization: Bearer instead",
        })),
    )
        .into_response()
}

/// The display name of a resolved user (the name for anyone else).
fn names(r: &Resolved) -> (Option<String>, Option<String>) {
    match &r.principal {
        Principal::User {
            name, display_name, ..
        } => (Some(name.clone()), Some(display_name.clone())),
        _ => (None, None),
    }
}

#[derive(Deserialize, Default)]
pub(super) struct LoginBody {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

/// `POST /portal/api/login`.
pub(super) async fn login_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Result<Json<LoginBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if !super::active(&state) {
        return super::api::off();
    }
    if !enabled(&state.config) {
        return sign_in_off();
    }
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => {
            return no_store(
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": format!("expected {{ \"username\": \"...\", \"password\": \"...\" }}: {}", e.body_text()),
                        "reason": "invalid_body",
                    })),
                )
                    .into_response(),
            )
        }
    };
    let account = match crate::login::check_sign_in(
        &state,
        &headers,
        &extensions,
        &body.username,
        &body.password,
        "portal_login",
    )
    .await
    {
        Ok((a, _)) => a,
        Err(resp) => return resp,
    };
    let Some(cookie) = issue_cookie(&state, &headers, &extensions, &uri, &account) else {
        return no_store(
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "could not sign the session", "reason": "server_error" })),
            )
                .into_response(),
        );
    };
    if let Err(e) = state.users.note_sign_in(&account.id) {
        tracing::warn!(target: "rustybin::auth", error = %e, "the sign-in time could not be saved");
    }
    audit("portal_login", &account.name, "ok");
    let reader = state
        .users
        .resolve(&account.id)
        .is_some_and(|r| r.permissions.has(Permission::PortalRead));
    let mut resp = Json(json!({
        "signed_in": true,
        "user": account.name,
        "display_name": account.display_name,
        "reader": reader,
        "expires_in": TTL_SECS,
    }))
    .into_response();
    resp.headers_mut().insert(header::SET_COOKIE, cookie);
    no_store(resp)
}

/// `POST /portal/api/logout`: the session's id joins the denylist and the
/// cookie is cleared (answered whether there was one or not).
pub(super) async fn logout_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    if !super::active(&state) {
        return super::api::off();
    }
    let ended = match session(&state, &headers) {
        Some((claims, r)) => {
            let jti = claims["jti"].as_str().unwrap_or("");
            let exp = claims["exp"].as_i64().unwrap_or(0);
            state.config.console_tokens.revoke(jti, exp);
            audit("portal_logout", &names(&r).0.unwrap_or_default(), "ok");
            true
        }
        None => false,
    };
    let mut resp = Json(json!({ "signed_out": true, "ended": ended })).into_response();
    if let Some(c) = cookie_header("", 0, secure(&headers, &extensions, &uri, &state.config)) {
        resp.headers_mut().insert(header::SET_COOKIE, c);
    }
    no_store(resp)
}

/// `GET /portal/api/session`: who is signed in to the portal (or reads it
/// with a bearer credential), and whether they read closed items.
pub(super) async fn session_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !super::active(&state) {
        return super::api::off();
    }
    let signin = enabled(&state.config);
    let body = match reader(&state, &headers) {
        Some((r, via)) => {
            let (user, display_name) = names(&r);
            json!({
                "signed_in": via == "cookie",
                "via": via,
                "user": user,
                "display_name": display_name,
                "reader": r.has(Permission::PortalRead),
                "signin": signin,
            })
        }
        None => json!({ "signed_in": false, "reader": false, "signin": signin }),
    };
    no_store(Json(body).into_response())
}
