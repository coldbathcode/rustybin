//! Console sign-in with a user name and password
//! (`RUSTYBIN_CONTROL_AUTH=password`, the default).
//!
//! `POST /_rustybin/login` checks the user name and password and answers a
//! control token: an HS256 JWT signed with the instance key the OAuth
//! sign-in uses ([`crate::control_oauth::OAuthKey`]), audience
//! `RUSTYBIN_CONTROL_JWT_AUDIENCE`, scope `admin`, `rb_use: "console"`, 12
//! hours, a `jti` and the token generation. The whole control plane, the
//! management MCP server and the data plane's admin guard accept it like any
//! control JWT ([`crate::control_auth::verify_jwt`] calls [`verify`] for
//! HS256 tokens in password mode), so nothing else in the credential model
//! changes. `RUSTYBIN_ADMIN_TOKEN` and Ed25519 control JWTs keep working.
//!
//! Credentials: `RUSTYBIN_CONSOLE_USER` (default `rustybin`) and
//! `RUSTYBIN_CONSOLE_PASSWORD` (default `rustybin`) or
//! `RUSTYBIN_CONSOLE_PASSWORD_HASH` (an argon2id PHC string). A password
//! changed in the console is stored as a hash in the settings store
//! (`console.password_hash`, a secret) and wins over the environment unless
//! `RUSTYBIN_SETTINGS_LOCK` is on; the user name is `console.user`.
//!
//! State ([`Tokens`], in `Config.console_tokens` because every credential
//! check takes `&Config`): the generation counter ("sign out everywhere"
//! rotates it), the revoked `jti`s (a bounded denylist that expires with the
//! tokens), the issued sessions (bounded, for Management, Sign-in) and the
//! failed attempts per client address (5 per minute, then 429 with
//! `Retry-After`, exponential up to 10 minutes). Every attempt is audited on
//! the `rustybin::auth` target with the user name, the address and the
//! outcome, never the password.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Path as RoutePath, State};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::control_auth::{constant_time_eq, presented_token, ControlAuth, Grant, JwtError};
use crate::limits::{Clock, SystemClock};
use crate::settings::{Kind, Spec};
use crate::state::AppState;

pub const LOGIN_PATH: &str = "/_rustybin/login";
pub const LOGOUT_PATH: &str = "/_rustybin/logout";
pub const SESSION_PATH: &str = "/_rustybin/session";
pub const SESSIONS_PATH: &str = "/_rustybin/sessions";
pub const PASSWORD_PATH: &str = "/_rustybin/password";

/// The user name and password out of the box.
pub const DEFAULT_USER: &str = "rustybin";
pub const DEFAULT_PASSWORD: &str = "rustybin";
/// Settings keys.
pub const USER_KEY: &str = "console.user";
pub const HASH_KEY: &str = "console.password_hash";
/// Life of a token.
pub const TOKEN_TTL_SECS: i64 = 12 * 3600;
/// Bounds.
pub const MAX_REVOKED: usize = 4096;
pub const MAX_SESSIONS: usize = 256;
pub const MAX_ATTEMPT_ADDRESSES: usize = 1024;
/// Failed attempts per address and window before the address waits.
pub const FAILURES_PER_WINDOW: u32 = 5;
pub const WINDOW_SECS: u64 = 60;
pub const FIRST_BLOCK_SECS: u64 = 60;
pub const MAX_BLOCK_SECS: u64 = 600;
pub const MIN_PASSWORD_CHARS: usize = 8;
pub const MAX_PASSWORD_CHARS: usize = 256;
pub const MAX_USER_CHARS: usize = 64;
const MAX_HASH_CHARS: usize = 512;
const LEEWAY_SECS: u64 = 30;
const TOKEN_USE: &str = "console";

// ── Modes ───────────────────────────────────────────────────────────

/// Is the password sign-in on? Password mode outside shared mode (API keys
/// authenticate the control plane there).
pub fn enabled(config: &Config) -> bool {
    config.control_auth == ControlAuth::Password && !config.shared_mode
}

/// Control-plane paths that need no credentials (the sign-in itself).
pub fn is_public_path(path: &str) -> bool {
    path == LOGIN_PATH
}

/// The user name that signs in: the `console.user` setting (stored, else
/// `RUSTYBIN_CONSOLE_USER`, else `rustybin`).
pub fn effective_user(settings: &crate::settings::Settings) -> String {
    settings
        .string(USER_KEY)
        .unwrap_or_else(|| DEFAULT_USER.to_string())
}

/// Does the instance still sign in with the default password? True in
/// password mode when no hash is stored or seeded and the plain password
/// from the environment is absent or `rustybin` itself.
pub fn default_password(state: &AppState) -> bool {
    enabled(&state.config)
        && state.settings.secret(HASH_KEY).is_none()
        && state
            .config
            .console_password
            .as_deref()
            .unwrap_or(DEFAULT_PASSWORD)
            == DEFAULT_PASSWORD
}

/// A public instance must not run with the default password: fatal at
/// startup (`server::start_with_state`).
pub fn check_startup(state: &AppState) -> Result<(), String> {
    if state.config.public_mode && default_password(state) {
        return Err(
            "RUSTYBIN_PUBLIC_MODE with the default console password: set RUSTYBIN_CONSOLE_PASSWORD \
             or RUSTYBIN_CONSOLE_PASSWORD_HASH, or choose RUSTYBIN_CONTROL_AUTH=open on purpose"
                .to_string(),
        );
    }
    Ok(())
}

/// The startup warning when the default password is in use.
pub fn warn_default_password(state: &AppState) {
    if default_password(state) {
        tracing::warn!(
            target: "rustybin::auth",
            "the console uses the default password ({DEFAULT_USER} / {DEFAULT_PASSWORD}): set \
             RUSTYBIN_CONSOLE_PASSWORD or change it in Management, Sign-in"
        );
    }
}

// ── Settings ────────────────────────────────────────────────────────

fn valid_user_value(v: &Value) -> Result<(), String> {
    match v.as_str() {
        Some(s) => valid_user(s),
        None => Ok(()),
    }
}

/// A user name: 1 to 64 characters, letters, digits and `. _ @ -`.
pub fn valid_user(s: &str) -> Result<(), String> {
    if s.is_empty() || s.chars().count() > MAX_USER_CHARS {
        return Err(format!("between 1 and {MAX_USER_CHARS} characters"));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-'))
    {
        return Err("letters, digits and . _ @ - only".to_string());
    }
    Ok(())
}

fn valid_hash_value(v: &Value) -> Result<(), String> {
    match v.as_str() {
        Some(s) if valid_hash(s) => Ok(()),
        Some(_) => Err("expected an argon2 PHC string ($argon2id$v=19$...)".to_string()),
        None => Ok(()),
    }
}

/// An argon2 PHC string this instance can verify.
pub fn valid_hash(s: &str) -> bool {
    s.len() <= MAX_HASH_CHARS
        && PasswordHash::new(s).is_ok_and(|h| h.algorithm.as_str().starts_with("argon2"))
}

/// The settings of the sign-in: the user name and the password hash.
pub fn specs() -> Vec<Spec> {
    vec![
        Spec::new(
            USER_KEY,
            "signin",
            "User name",
            "The user name of the console sign-in (RUSTYBIN_CONTROL_AUTH=password). Letters, digits and . _ @ - only, at most 64 characters.",
            Kind::Text {
                max_len: MAX_USER_CHARS,
            },
            json!(DEFAULT_USER),
        )
        .env("RUSTYBIN_CONSOLE_USER", |c| {
            c.console_user.clone().map(Value::String)
        })
        .check(valid_user_value),
        Spec::new(
            HASH_KEY,
            "signin",
            "Password",
            "The password of the console sign-in, kept as an argon2id hash (a PHC string): change it under Management, Sign-in with the current password. RUSTYBIN_CONSOLE_PASSWORD_HASH seeds it, RUSTYBIN_CONSOLE_PASSWORD is the plain alternative, and rustybin is the default until one is set.",
            Kind::Text {
                max_len: MAX_HASH_CHARS,
            },
            Value::Null,
        )
        .env("RUSTYBIN_CONSOLE_PASSWORD_HASH", |c| {
            c.console_password_hash.clone().map(Value::String)
        })
        .secret()
        .check(valid_hash_value),
    ]
}

// ── Passwords ───────────────────────────────────────────────────────

/// Hash a password (argon2id, the crate's defaults, a random salt).
pub fn hash_password(plain: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    let salt = SaltString::encode_b64(&salt).map_err(|e| e.to_string())?;
    Argon2::default()
        .hash_password(plain.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

/// Does `plain` match the PHC `hash`? (Slow by design: call off the runtime.)
pub fn verify_hash(hash: &str, plain: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(plain.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

/// Rules for a new password: 8 to 256 characters, no control characters,
/// not the default.
pub fn valid_new_password(plain: &str) -> Result<(), String> {
    let n = plain.chars().count();
    if n < MIN_PASSWORD_CHARS {
        return Err(format!("at least {MIN_PASSWORD_CHARS} characters"));
    }
    if n > MAX_PASSWORD_CHARS {
        return Err(format!("at most {MAX_PASSWORD_CHARS} characters"));
    }
    if plain.chars().any(char::is_control) {
        return Err("no control characters".to_string());
    }
    if plain == DEFAULT_PASSWORD {
        return Err("choose something other than the default".to_string());
    }
    Ok(())
}

/// Do `user` and `password` sign in? The stored or seeded hash wins, else the
/// plain password from the environment, else the default. Both the user name
/// and the password are always checked (constant-time comparison, the hash
/// off the async runtime), whatever the outcome.
pub async fn check_credentials(state: &AppState, user: &str, password: &str) -> bool {
    let user_ok = constant_time_eq(user, &effective_user(&state.settings));
    let password_ok = match state.settings.secret(HASH_KEY) {
        Some(hash) => {
            let hash = hash.clone();
            let plain = password.to_string();
            tokio::task::spawn_blocking(move || verify_hash(&hash, &plain))
                .await
                .unwrap_or(false)
        }
        None => constant_time_eq(
            password,
            state
                .config
                .console_password
                .as_deref()
                .unwrap_or(DEFAULT_PASSWORD),
        ),
    };
    user_ok & password_ok
}

// ── Tokens: generation, revocation, sessions, attempts ───────────────

/// One issued sign-in token (for Management, Sign-in).
#[derive(Clone, Debug)]
struct Issued {
    jti: String,
    user: String,
    issued_at: i64,
    expires_at: i64,
    ip: Option<IpAddr>,
    last_used: i64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Attempt {
    failures: u32,
    window_start: Duration,
    blocked_until: Option<Duration>,
    strikes: u32,
    last: Duration,
}

/// The runtime state of the console sign-in. Every structure is bounded.
pub struct Tokens {
    clock: Arc<dyn Clock>,
    generation: AtomicU64,
    revoked: Mutex<BTreeMap<String, i64>>,
    issued: Mutex<VecDeque<Issued>>,
    attempts: Mutex<HashMap<IpAddr, Attempt>>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Tokens(generation {}, {} session(s), {} revoked)",
            self.generation(),
            lock(&self.issued).len(),
            lock(&self.revoked).len()
        )
    }
}

impl Default for Tokens {
    fn default() -> Self {
        Self::new()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Tokens {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock::new()))
    }

    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            generation: AtomicU64::new(1),
            revoked: Mutex::new(BTreeMap::new()),
            issued: Mutex::new(VecDeque::new()),
            attempts: Mutex::new(HashMap::new()),
        }
    }

    fn now(&self) -> i64 {
        self.clock.wall().timestamp()
    }

    /// The current token generation (every token carries the one it was
    /// issued under).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Sign out everywhere: tokens of earlier generations fail from now on.
    /// Returns the number of sessions that were listed.
    pub fn rotate(&self) -> usize {
        self.generation.fetch_add(1, Ordering::Relaxed);
        lock(&self.revoked).clear();
        let mut issued = lock(&self.issued);
        let n = issued.len();
        issued.clear();
        n
    }

    /// Forget sessions whose tokens expired.
    fn prune(&self) {
        let now = self.now();
        lock(&self.revoked).retain(|_, exp| *exp > now);
        lock(&self.issued).retain(|s| s.expires_at > now);
    }

    /// Record a freshly issued token.
    fn record(&self, issued: Issued) {
        self.prune();
        let mut list = lock(&self.issued);
        list.push_back(issued);
        while list.len() > MAX_SESSIONS {
            list.pop_front();
        }
    }

    /// Revoke a token id until `expires_at`.
    pub fn revoke(&self, jti: &str, expires_at: i64) {
        self.prune();
        lock(&self.issued).retain(|s| s.jti != jti);
        let mut revoked = lock(&self.revoked);
        revoked.insert(jti.to_string(), expires_at);
        while revoked.len() > MAX_REVOKED {
            // Drop the entry that expires first.
            let Some(first) = revoked
                .iter()
                .min_by_key(|(_, exp)| **exp)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            revoked.remove(&first);
        }
    }

    pub fn is_revoked(&self, jti: &str) -> bool {
        let now = self.now();
        lock(&self.revoked).get(jti).is_some_and(|exp| *exp > now)
    }

    /// Note a use of a listed session.
    fn touch(&self, jti: &str) {
        let now = self.now();
        if let Some(s) = lock(&self.issued).iter_mut().find(|s| s.jti == jti) {
            s.last_used = now;
        }
    }

    /// End one listed session. `false` when it is not listed (expired,
    /// already ended, or evicted from the bounded list).
    pub fn end(&self, jti: &str) -> bool {
        let exp = lock(&self.issued)
            .iter()
            .find(|s| s.jti == jti)
            .map(|s| s.expires_at);
        match exp {
            Some(exp) => {
                self.revoke(jti, exp);
                true
            }
            None => false,
        }
    }

    /// The listed sessions, newest first.
    pub fn sessions(&self) -> Vec<Value> {
        self.prune();
        lock(&self.issued)
            .iter()
            .rev()
            .map(|s| {
                json!({
                    "id": s.jti,
                    "user": s.user,
                    "issued_at": rfc3339(s.issued_at),
                    "expires_at": rfc3339(s.expires_at),
                    "last_used": rfc3339(s.last_used),
                    "ip": s.ip.map(|ip| ip.to_string()),
                })
            })
            .collect()
    }

    /// May `ip` try to sign in? `Err(seconds)` while it waits.
    pub fn check(&self, ip: Option<IpAddr>) -> Result<(), u64> {
        let Some(ip) = ip else { return Ok(()) };
        let now = self.clock.mono();
        let attempts = lock(&self.attempts);
        match attempts.get(&ip).and_then(|a| a.blocked_until) {
            Some(until) if until > now => Err((until - now).as_secs().max(1)),
            _ => Ok(()),
        }
    }

    /// A failed attempt from `ip`.
    pub fn failed(&self, ip: Option<IpAddr>) {
        let Some(ip) = ip else { return };
        let now = self.clock.mono();
        let mut attempts = lock(&self.attempts);
        if !attempts.contains_key(&ip) && attempts.len() >= MAX_ATTEMPT_ADDRESSES {
            let stale = Duration::from_secs(MAX_BLOCK_SECS);
            attempts.retain(|_, a| {
                a.blocked_until.is_some_and(|u| u > now) || now.saturating_sub(a.last) < stale
            });
            if attempts.len() >= MAX_ATTEMPT_ADDRESSES {
                if let Some(oldest) = attempts.iter().min_by_key(|(_, a)| a.last).map(|(k, _)| *k) {
                    attempts.remove(&oldest);
                }
            }
        }
        let a = attempts.entry(ip).or_default();
        if now.saturating_sub(a.window_start) > Duration::from_secs(WINDOW_SECS) {
            a.failures = 0;
            a.window_start = now;
        }
        a.failures += 1;
        a.last = now;
        if a.failures >= FAILURES_PER_WINDOW {
            let secs = (FIRST_BLOCK_SECS << a.strikes.min(10)).min(MAX_BLOCK_SECS);
            a.blocked_until = Some(now + Duration::from_secs(secs));
            a.strikes = a.strikes.saturating_add(1);
            a.failures = 0;
            a.window_start = now;
        }
    }

    /// A successful sign-in from `ip` clears its record.
    pub fn succeeded(&self, ip: Option<IpAddr>) {
        if let Some(ip) = ip {
            lock(&self.attempts).remove(&ip);
        }
    }
}

fn rfc3339(ts: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

// ── Tokens: mint and verify ─────────────────────────────────────────

fn random_id() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Mint a token for `user`: the claims and the signed token.
fn mint(config: &Config, user: &str) -> Option<(String, Value)> {
    let tokens = &config.console_tokens;
    let iat = tokens.now();
    let claims = json!({
        "iss": "rustybin",
        "aud": config.control_jwt_audience,
        "sub": format!("user:{user}"),
        "scope": "admin",
        "iat": iat,
        "exp": iat + TOKEN_TTL_SECS,
        "jti": random_id(),
        "rb_use": TOKEN_USE,
        "rb_gen": tokens.generation(),
    });
    let mut header = Header::new(Algorithm::HS256);
    header.typ = Some("JWT".into());
    let token = jsonwebtoken::encode(
        &header,
        &claims,
        &EncodingKey::from_secret(&config.control_oauth_key.console_secret()),
    )
    .ok()?;
    Some((token, claims))
}

/// Shaped like one of ours: three parts, an HS256 header and a payload that
/// says `rb_use: "console"` (read without verifying, which [`verify`] then
/// does). The OAuth server's tokens share the key and the algorithm but say
/// `access` or `refresh`, so they are never mistaken for a sign-in.
pub fn is_console_token(token: &str) -> bool {
    if token.len() > crate::control_auth::MAX_TOKEN_LEN {
        return false;
    }
    let mut parts = token.split('.');
    let (Some(_), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if !jsonwebtoken::decode_header(token).is_ok_and(|h| h.alg == Algorithm::HS256) {
        return false;
    }
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|claims| claims.get("rb_use").and_then(Value::as_str) == Some(TOKEN_USE))
}

/// Verify a sign-in token: signature, audience, expiry, use, generation and
/// the denylist. Returns its claims.
pub fn verify(token: &str, config: &Config) -> Result<Value, JwtError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = LEEWAY_SECS;
    validation.set_audience(&[config.control_jwt_audience.as_str()]);
    validation.required_spec_claims =
        std::collections::HashSet::from(["exp".to_string(), "aud".to_string()]);
    let data = jsonwebtoken::decode::<Value>(
        token,
        &DecodingKey::from_secret(&config.control_oauth_key.console_secret()),
        &validation,
    )
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
    let claims = data.claims;
    if claims.get("rb_use").and_then(Value::as_str) != Some(TOKEN_USE) {
        return Err(JwtError::Malformed);
    }
    let tokens = &config.console_tokens;
    let generation = claims.get("rb_gen").and_then(Value::as_u64);
    if generation != Some(tokens.generation()) {
        return Err(JwtError::Revoked);
    }
    let jti = claims.get("jti").and_then(Value::as_str).unwrap_or("");
    if jti.is_empty() || tokens.is_revoked(jti) {
        return Err(JwtError::Revoked);
    }
    tokens.touch(jti);
    Ok(claims)
}

/// The verified claims of the sign-in token a request presents, if it does.
fn presented_claims(headers: &HeaderMap, config: &Config) -> Option<Value> {
    let token = presented_token(headers)?;
    if !is_console_token(token) {
        return None;
    }
    verify(token, config).ok()
}

fn user_of(claims: &Value) -> Option<&str> {
    claims
        .get("sub")
        .and_then(Value::as_str)
        .and_then(|s| s.strip_prefix("user:"))
}

// ── Responses ───────────────────────────────────────────────────────

fn error(status: StatusCode, message: &str, reason: &str) -> Response {
    let mut resp = (status, Json(json!({ "error": message, "reason": reason }))).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn not_found(config: &Config) -> Response {
    let hint = if config.shared_mode {
        "shared mode authenticates the control plane with API keys"
    } else {
        "the password sign-in is on only with RUSTYBIN_CONTROL_AUTH=password (the default)"
    };
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": "not found",
            "control_auth": config.control_auth.as_str(),
            "hint": hint,
        })),
    )
        .into_response()
}

/// The sign-in management routes need the admin token or a token with the
/// admin scope (a sign-in token has it), never the open-mode leniency of
/// `admin::require_admin`.
#[allow(clippy::result_large_err)]
fn admin_only(headers: &HeaderMap, config: &Config) -> Result<(), Response> {
    match crate::control_auth::credentials_grant(headers, config) {
        Ok(grant) if grant.admin => Ok(()),
        _ => Err(error(
            StatusCode::FORBIDDEN,
            "this needs the admin token or a token with the admin scope",
            "insufficient_scope",
        )),
    }
}

fn audit(event: &str, user: &str, ip: Option<IpAddr>, outcome: &str) {
    tracing::info!(
        target: "rustybin::auth",
        event,
        user = %user.chars().take(MAX_USER_CHARS).collect::<String>(),
        ip = %ip.map(|i| i.to_string()).unwrap_or_default(),
        outcome,
        "console sign-in"
    );
}

fn client_ip(headers: &HeaderMap, extensions: &Extensions, config: &Config) -> Option<IpAddr> {
    crate::session::client_ip(headers, extensions, config)
}

/// A token response body.
fn token_body(token: &str, claims: &Value, state: &AppState) -> Value {
    let exp = claims.get("exp").and_then(Value::as_i64).unwrap_or(0);
    json!({
        "token": token,
        "token_type": "Bearer",
        "expires_in": TOKEN_TTL_SECS,
        "expires_at": rfc3339(exp),
        "scopes": Grant::from_scope_claim(claims.get("scope")).scopes(),
        "user": user_of(claims),
        "default_password": default_password(state),
    })
}

// ── Handlers ────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct LoginBody {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

async fn login_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    body: Result<Json<LoginBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let config = &state.config;
    if !enabled(config) {
        return not_found(config);
    }
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                &format!(
                    "expected {{ \"username\": \"...\", \"password\": \"...\" }}: {}",
                    e.body_text()
                ),
                "invalid_body",
            )
        }
    };
    let ip = client_ip(&headers, &extensions, config);
    let tokens = &config.console_tokens;
    if let Err(wait) = tokens.check(ip) {
        audit("login", &body.username, ip, "rate_limited");
        return rate_limited(wait);
    }
    if body.username.chars().count() > MAX_USER_CHARS
        || body.password.chars().count() > MAX_PASSWORD_CHARS * 4
    {
        tokens.failed(ip);
        audit("login", &body.username, ip, "invalid_credentials");
        return error(
            StatusCode::UNAUTHORIZED,
            "invalid user name or password",
            "invalid_credentials",
        );
    }
    if !check_credentials(&state, &body.username, &body.password).await {
        tokens.failed(ip);
        audit("login", &body.username, ip, "invalid_credentials");
        return error(
            StatusCode::UNAUTHORIZED,
            "invalid user name or password",
            "invalid_credentials",
        );
    }
    tokens.succeeded(ip);
    let user = effective_user(&state.settings);
    let Some((token, claims)) = mint(config, &user) else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not sign the token",
            "server_error",
        );
    };
    tokens.record(Issued {
        jti: claims["jti"].as_str().unwrap_or("").to_string(),
        user: user.clone(),
        issued_at: claims["iat"].as_i64().unwrap_or(0),
        expires_at: claims["exp"].as_i64().unwrap_or(0),
        ip,
        last_used: claims["iat"].as_i64().unwrap_or(0),
    });
    audit("login", &user, ip, "ok");
    let mut resp = Json(token_body(&token, &claims, &state)).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn rate_limited(wait: u64) -> Response {
    let mut resp = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "error": format!("too many failed sign-in attempts: try again in {wait} s"),
            "reason": "rate_limited",
            "retry_after": wait,
        })),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
        resp.headers_mut().insert(header::RETRY_AFTER, v);
    }
    resp
}

async fn logout_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let config = &state.config;
    if !enabled(config) {
        return not_found(config);
    }
    match presented_claims(&headers, config) {
        Some(claims) => {
            let jti = claims["jti"].as_str().unwrap_or("");
            let exp = claims["exp"].as_i64().unwrap_or(0);
            config.console_tokens.revoke(jti, exp);
            audit("logout", user_of(&claims).unwrap_or(""), None, "ok");
            Json(json!({ "revoked": true })).into_response()
        }
        None => Json(json!({
            "revoked": false,
            "reason": "not a sign-in token (the admin token and platform-issued control JWTs cannot be revoked here)",
        }))
        .into_response(),
    }
}

async fn session_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let config = &state.config;
    if config.control_auth == ControlAuth::Open {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "not found",
                "control_auth": "open",
                "hint": "the control plane is open: there is nothing to sign in to",
            })),
        )
            .into_response();
    }
    let token = presented_token(&headers);
    let is_admin_token = token.is_some_and(|t| {
        config
            .admin_token
            .as_deref()
            .is_some_and(|expected| constant_time_eq(t, expected))
    });
    let mut body = json!({
        "control_auth": config.control_auth.as_str(),
        "default_password": default_password(&state),
        "sign_in_user": enabled(config).then(|| effective_user(&state.settings)),
    });
    let fill = |body: &mut Value, pairs: Value| {
        if let (Value::Object(b), Value::Object(p)) = (body, pairs) {
            b.extend(p);
        }
    };
    if is_admin_token {
        fill(
            &mut body,
            json!({ "obtained": "admin_token", "scopes": Grant::ADMIN.scopes() }),
        );
        return Json(body).into_response();
    }
    if let Some(claims) = presented_claims(&headers, config) {
        fill(
            &mut body,
            json!({
                "obtained": "password",
                "user": user_of(&claims),
                "scopes": Grant::from_scope_claim(claims.get("scope")).scopes(),
                "issued_at": rfc3339(claims["iat"].as_i64().unwrap_or(0)),
                "expires_at": rfc3339(claims["exp"].as_i64().unwrap_or(0)),
                "id": claims["jti"],
            }),
        );
        return Json(body).into_response();
    }
    if let Some(t) = token {
        if let Ok((grant, claims)) = crate::control_auth::verify_jwt_claims(t, config) {
            fill(
                &mut body,
                json!({
                    "obtained": "control_jwt",
                    "sub": claims.get("sub"),
                    "scopes": grant.scopes(),
                    "expires_at": claims.get("exp").and_then(Value::as_i64).map(rfc3339),
                }),
            );
            return Json(body).into_response();
        }
    }
    if let Some(key) = headers
        .get(crate::keys::KEY_HEADER)
        .filter(|_| config.shared_mode)
    {
        let _ = key;
        fill(&mut body, json!({ "obtained": "api_key" }));
        return Json(body).into_response();
    }
    error(
        StatusCode::UNAUTHORIZED,
        "no credential recognised on this request",
        "invalid_token",
    )
}

async fn sessions_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let config = &state.config;
    if !enabled(config) {
        return not_found(config);
    }
    if let Err(resp) = admin_only(&headers, config) {
        return resp;
    }
    let sessions = config.console_tokens.sessions();
    Json(json!({
        "generation": config.console_tokens.generation(),
        "count": sessions.len(),
        "capacity": MAX_SESSIONS,
        "current": presented_claims(&headers, config).and_then(|c| c.get("jti").cloned()),
        "sessions": sessions,
    }))
    .into_response()
}

async fn end_session_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    RoutePath(id): RoutePath<String>,
) -> Response {
    let config = &state.config;
    if !enabled(config) {
        return not_found(config);
    }
    if let Err(resp) = admin_only(&headers, config) {
        return resp;
    }
    let ended = config.console_tokens.end(&id);
    if ended {
        audit("end_session", "", None, "ok");
    }
    Json(json!({ "ended": ended, "id": id })).into_response()
}

async fn end_all_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let config = &state.config;
    if !enabled(config) {
        return not_found(config);
    }
    if let Err(resp) = admin_only(&headers, config) {
        return resp;
    }
    let n = config.console_tokens.rotate();
    audit("sign_out_everywhere", "", None, "ok");
    Json(json!({
        "ended": n,
        "generation": config.console_tokens.generation(),
        "note": "every sign-in token issued before now is refused, this one included",
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
struct PasswordBody {
    #[serde(default)]
    current_password: String,
    #[serde(default)]
    new_password: String,
    #[serde(default)]
    username: Option<String>,
}

async fn password_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    body: Result<Json<PasswordBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let config = &state.config;
    if !enabled(config) {
        return not_found(config);
    }
    if let Err(resp) = admin_only(&headers, config) {
        return resp;
    }
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                &format!("expected {{ \"current_password\": \"...\", \"new_password\": \"...\", \"username\"?: \"...\" }}: {}", e.body_text()),
                "invalid_body",
            )
        }
    };
    let ip = client_ip(&headers, &extensions, config);
    let tokens = &config.console_tokens;
    if let Err(wait) = tokens.check(ip) {
        return rate_limited(wait);
    }
    let current_user = effective_user(&state.settings);
    if body.current_password.chars().count() > MAX_PASSWORD_CHARS * 4
        || !check_credentials(&state, &current_user, &body.current_password).await
    {
        tokens.failed(ip);
        audit(
            "change_password",
            &current_user,
            ip,
            "wrong_current_password",
        );
        return error(
            StatusCode::FORBIDDEN,
            "the current password is wrong",
            "invalid_credentials",
        );
    }
    tokens.succeeded(ip);
    if let Err(e) = valid_new_password(&body.new_password) {
        return error(
            StatusCode::BAD_REQUEST,
            &format!("new password: {e}"),
            "invalid_password",
        );
    }
    let new_user = body
        .username
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string);
    if let Some(u) = &new_user {
        if let Err(e) = valid_user(u) {
            return error(
                StatusCode::BAD_REQUEST,
                &format!("user name: {e}"),
                "invalid_user",
            );
        }
    }
    let plain = body.new_password.clone();
    let hash = match tokio::task::spawn_blocking(move || hash_password(&plain)).await {
        Ok(Ok(h)) => h,
        _ => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not hash the password",
                "server_error",
            )
        }
    };
    let mut values = BTreeMap::new();
    if let Some(u) = &new_user {
        if *u != current_user {
            values.insert(USER_KEY.to_string(), Value::String(u.clone()));
        }
    }
    let mut secrets = BTreeMap::new();
    secrets.insert(HASH_KEY.to_string(), Some(hash));
    let who = crate::settings::principal(&headers, config);
    if let Err(e) = state.settings.update(values, secrets, &who) {
        return match e {
            crate::settings::UpdateError::Locked(keys) => (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "the sign-in settings are locked by RUSTYBIN_SETTINGS_LOCK: the environment is authoritative (RUSTYBIN_CONSOLE_USER, RUSTYBIN_CONSOLE_PASSWORD_HASH)",
                    "reason": "locked",
                    "locked": keys,
                })),
            )
                .into_response(),
            crate::settings::UpdateError::Invalid(problems) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid sign-in settings", "reason": "invalid", "problems": problems })),
            )
                .into_response(),
            crate::settings::UpdateError::Io(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("sign-in settings not saved: {e}"), "reason": "io" })),
            )
                .into_response(),
        };
    }
    let user = new_user.unwrap_or(current_user);
    // Every other session ends; the caller gets a fresh token when it signed
    // in with one.
    let had_session = presented_claims(&headers, config).is_some();
    let ended = tokens.rotate();
    audit("change_password", &user, ip, "ok");
    let mut out = json!({
        "changed": true,
        "user": user,
        "signed_out": ended,
        "default_password": default_password(&state),
    });
    if had_session {
        if let Some((token, claims)) = mint(config, &user) {
            tokens.record(Issued {
                jti: claims["jti"].as_str().unwrap_or("").to_string(),
                user: user.clone(),
                issued_at: claims["iat"].as_i64().unwrap_or(0),
                expires_at: claims["exp"].as_i64().unwrap_or(0),
                ip,
                last_used: claims["iat"].as_i64().unwrap_or(0),
            });
            if let Value::Object(m) = &mut out {
                m.insert("token".into(), json!(token));
                m.insert(
                    "expires_at".into(),
                    json!(rfc3339(claims["exp"].as_i64().unwrap_or(0))),
                );
            }
        }
    }
    let mut resp = Json(out).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(LOGIN_PATH, post(login_handler))
        .route(LOGOUT_PATH, post(logout_handler))
        .route(SESSION_PATH, get(session_handler))
        .route(SESSIONS_PATH, get(sessions_handler).delete(end_all_handler))
        .route("/_rustybin/sessions/{id}", delete(end_session_handler))
        .route(PASSWORD_PATH, put(password_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            LOGIN_PATH,
            &["POST"],
            category::CONTROL,
            "Console sign-in: a user name and password for a control token (password mode)",
        )
        .description(
            "Never behind control-plane auth. Answers a 12 hour control token (scope admin) \
             that every control-plane route, the management MCP server and the admin guard \
             accept. 5 failed attempts per client address and minute, then 429 with Retry-After. \
             404 unless RUSTYBIN_CONTROL_AUTH=password (the default).",
        )
        .example(
            Example::post("Sign in with the defaults", LOGIN_PATH)
                .json(r#"{"username":"rustybin","password":"rustybin"}"#)
                .expect_status(404),
        ),
        Endpoint::new(
            LOGOUT_PATH,
            &["POST"],
            category::CONTROL,
            "Console sign-out: revoke the presented sign-in token",
        )
        .example(Example::post("Sign out", LOGOUT_PATH).expect_status(404)),
        Endpoint::new(
            SESSION_PATH,
            &["GET"],
            category::CONTROL,
            "Who the presented credential is: user, scopes, expiry, how it was obtained",
        )
        .example(Example::get("This session", SESSION_PATH).expect_status(404)),
        Endpoint::new(
            SESSIONS_PATH,
            &["GET", "DELETE"],
            category::CONTROL,
            "Sign-in sessions (admin): list them, or DELETE to sign out everywhere",
        )
        .example(Example::get("Sessions", SESSIONS_PATH).expect_status(404)),
        Endpoint::new(
            "/_rustybin/sessions/{id}",
            &["DELETE"],
            category::CONTROL,
            "End one sign-in session (admin)",
        )
        .example(
            Example::delete(
                "End a session",
                "/_rustybin/sessions/0123456789abcdef0123456789abcdef",
            )
            .expect_status(404),
        ),
        Endpoint::new(
            PASSWORD_PATH,
            &["PUT"],
            category::CONTROL,
            "Change the console user name and password (admin, current password required)",
        )
        .description(
            "Stores an argon2id hash as the secret console.password_hash (and the user name as \
             console.user), ends every other session and answers a fresh token for the caller. \
             409 when RUSTYBIN_SETTINGS_LOCK holds the sign-in settings.",
        )
        .example(
            Example::new("Change the password", "PUT", PASSWORD_PATH)
                .json(r#"{"current_password":"rustybin","new_password":"a-longer-secret"}"#)
                .expect_status(404),
        ),
    ]
}

pub fn openapi_paths() -> Value {
    let token_schema = json!({
        "type": "object",
        "properties": {
            "token": { "type": "string", "description": "A control token (HS256 JWT): send it as Authorization: Bearer" },
            "token_type": { "type": "string", "enum": ["Bearer"] },
            "expires_in": { "type": "integer" },
            "expires_at": { "type": "string", "format": "date-time" },
            "scopes": { "type": "array", "items": { "type": "string" } },
            "user": { "type": "string" },
            "default_password": { "type": "boolean", "description": "The instance still signs in with the default password" }
        }
    });
    let not_found = json!({ "description": "Not in password mode (or shared mode)" });
    json!({
        LOGIN_PATH: {
            "post": {
                "tags": ["Control Plane"],
                "summary": "Console sign-in",
                "description": "Checks the user name and password (RUSTYBIN_CONSOLE_USER, RUSTYBIN_CONSOLE_PASSWORD or _HASH, the stored console.password_hash; rustybin / rustybin by default) and answers a 12 hour control token with the admin scope. Never behind control-plane auth. After 5 failed attempts from one client address within a minute the address waits (429, Retry-After, doubling up to 10 minutes).",
                "operationId": "consoleLogin",
                "security": [],
                "requestBody": { "required": true, "content": { "application/json": { "schema": {
                    "type": "object",
                    "required": ["username", "password"],
                    "properties": { "username": { "type": "string" }, "password": { "type": "string" } }
                } } } },
                "responses": {
                    "200": { "description": "Signed in", "content": { "application/json": { "schema": token_schema } } },
                    "401": { "description": "Wrong user name or password (reason invalid_credentials)" },
                    "404": not_found,
                    "429": { "description": "Too many failed attempts from this address (Retry-After)" }
                }
            }
        },
        LOGOUT_PATH: {
            "post": {
                "tags": ["Control Plane"],
                "summary": "Console sign-out",
                "description": "Revokes the presented sign-in token for the rest of its life (a bounded denylist of token ids). Other credentials answer revoked: false.",
                "operationId": "consoleLogout",
                "responses": {
                    "200": { "description": "Done", "content": { "application/json": { "schema": { "type": "object", "properties": { "revoked": { "type": "boolean" } } } } } },
                    "404": not_found
                }
            }
        },
        SESSION_PATH: {
            "get": {
                "tags": ["Control Plane"],
                "summary": "This session",
                "description": "Describes the credential the request presents: obtained (password, admin_token, control_jwt, api_key), user, scopes, issue and expiry times, plus whether the instance still uses the default password.",
                "operationId": "getConsoleSession",
                "responses": {
                    "200": { "description": "The session", "content": { "application/json": { "schema": { "type": "object" } } } },
                    "404": { "description": "The control plane is open" }
                }
            }
        },
        SESSIONS_PATH: {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Sign-in sessions",
                "description": "The sign-in tokens issued since the last restart or sign-out-everywhere (at most 256, newest first): id, user, issue, expiry and last use times, client address. Admin.",
                "operationId": "listConsoleSessions",
                "responses": {
                    "200": { "description": "Sessions", "content": { "application/json": { "schema": { "type": "object", "properties": {
                        "generation": { "type": "integer" },
                        "count": { "type": "integer" },
                        "current": { "type": ["string", "null"], "description": "The caller's own session id" },
                        "sessions": { "type": "array", "items": { "type": "object" } }
                    } } } } },
                    "404": not_found
                }
            },
            "delete": {
                "tags": ["Control Plane"],
                "summary": "Sign out everywhere",
                "description": "Rotates the token generation: every sign-in token issued before now is refused, the caller's included. Admin.",
                "operationId": "endAllConsoleSessions",
                "responses": {
                    "200": { "description": "Done", "content": { "application/json": { "schema": { "type": "object", "properties": { "ended": { "type": "integer" }, "generation": { "type": "integer" } } } } } },
                    "404": not_found
                }
            }
        },
        "/_rustybin/sessions/{id}": {
            "delete": {
                "tags": ["Control Plane"],
                "summary": "End a sign-in session",
                "operationId": "endConsoleSession",
                "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }],
                "responses": {
                    "200": { "description": "Done (ended: false when the session is not listed)", "content": { "application/json": { "schema": { "type": "object", "properties": { "ended": { "type": "boolean" } } } } } },
                    "404": not_found
                }
            }
        },
        PASSWORD_PATH: {
            "put": {
                "tags": ["Control Plane"],
                "summary": "Change the console user name and password",
                "description": "Needs admin and the current password. The new password (8 to 256 characters, not the default) is stored as an argon2id hash in the secret console.password_hash; a new username goes to console.user. Every other sign-in session ends and the caller gets a fresh token when it signed in with one.",
                "operationId": "changeConsolePassword",
                "requestBody": { "required": true, "content": { "application/json": { "schema": {
                    "type": "object",
                    "required": ["current_password", "new_password"],
                    "properties": {
                        "current_password": { "type": "string" },
                        "new_password": { "type": "string", "minLength": 8, "maxLength": 256 },
                        "username": { "type": "string", "description": "A new user name (letters, digits and . _ @ -)" }
                    }
                } } } },
                "responses": {
                    "200": { "description": "Changed", "content": { "application/json": { "schema": { "type": "object", "properties": {
                        "changed": { "type": "boolean" },
                        "user": { "type": "string" },
                        "signed_out": { "type": "integer" },
                        "token": { "type": "string" },
                        "expires_at": { "type": "string", "format": "date-time" }
                    } } } } },
                    "400": { "description": "The new password or user name breaks a rule" },
                    "403": { "description": "The current password is wrong" },
                    "404": not_found,
                    "409": { "description": "RUSTYBIN_SETTINGS_LOCK holds the sign-in settings" }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests;
