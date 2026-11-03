//! Console sign-in with a user name and password
//! (`RUSTYBIN_CONTROL_AUTH=password`, the default).
//!
//! `POST /_rustybin/login` looks the user up in the user store
//! ([`crate::rbac::users`]), checks the password and answers a control
//! token: an HS256 JWT signed with the instance key the OAuth sign-in uses
//! ([`crate::control_oauth::OAuthKey`]), audience
//! `RUSTYBIN_CONTROL_JWT_AUDIENCE`, `sub` the user id, `rb_user` the sign-in
//! name, `rb_use: "console"`, 12 hours, a `jti`, the instance's token
//! generation (`rb_gen`) and the user's (`rb_ugen`). Its permissions are
//! the user's roles, resolved at every request ([`crate::rbac`]); a 0.3
//! token (`sub: "user:<name>"`) is user one's. The whole control plane, the
//! management MCP server and the data plane's admin guard accept it like any
//! control JWT ([`crate::control_auth::verify_jwt`] calls [`verify`] for
//! HS256 tokens in password mode), so nothing else in the credential model
//! changes. `RUSTYBIN_ADMIN_TOKEN` and Ed25519 control JWTs keep working.
//!
//! Credentials of user one (the first admin, the 0.3 console user; other
//! users keep their own hash in the user store): `RUSTYBIN_CONSOLE_USER`
//! (default `rustybin`) and
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
//! failed attempts per client address (an IPv6 /64) and user name (5 per
//! minute, then 429 with `Retry-After`, exponential up to 10 minutes), plus
//! a budget of failures at each user name from every address together
//! (invitations and single sign-on are lanes of their own). Every attempt is
//! audited on the `rustybin::auth` target with the user name (control
//! characters escaped), the address and the outcome, never the password.

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
use crate::control_auth::{constant_time_eq, presented_token, ControlAuth, JwtError};
use crate::limits::{Clock, SystemClock};
use crate::settings::{Kind, Spec};
use crate::state::AppState;

pub const LOGIN_PATH: &str = "/_rustybin/login";
pub const LOGOUT_PATH: &str = "/_rustybin/logout";
pub const SESSION_PATH: &str = "/_rustybin/session";
pub const SESSIONS_PATH: &str = "/_rustybin/sessions";
pub const PASSWORD_PATH: &str = "/_rustybin/password";
/// Your own password (the 0.3 `PASSWORD_PATH` is its alias).
pub const ME_PASSWORD_PATH: &str = "/_rustybin/me/password";
/// Accept an invitation (open, rate limited).
pub const JOIN_PATH: &str = "/_rustybin/join";
/// What the sign-in throttle counts invitation attempts under (never a
/// user name: the parentheses are not allowed in one).
const JOIN_KEY: &str = "(invitation)";

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
/// Revoked token subjects (an AI agent's OAuth client, see
/// `crate::mcp::manage::agents`).
pub const MAX_REVOKED_SUBJECTS: usize = 1024;
/// Listed sessions of every user together (a backstop: the per-user cap
/// binds first).
pub const MAX_SESSIONS: usize = 4096;
/// Listed sessions of one user: a sign-in past it ends the user's oldest
/// session (its token is refused from then on), so a session is never live
/// without being listed, and nobody's sign-ins push another user's out.
pub const MAX_SESSIONS_PER_USER: usize = 16;
pub const MAX_ATTEMPT_ADDRESSES: usize = 1024;
/// Failed attempts per address and user name and window before they wait.
pub const FAILURES_PER_WINDOW: u32 = 5;
/// Failed attempts at one user name from every address together per
/// window before sign-ins as that name wait for the window's end: rotating
/// addresses buys no more guesses at a name than this, and failures at
/// other names (or junk invitation tokens and single sign-on callbacks,
/// each a lane of their own) never hold anyone else back.
pub const NAME_FAILURES_PER_WINDOW: u32 = 20;
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

/// Control-plane paths that need no credentials (the sign-in itself and
/// accepting an invitation).
pub fn is_public_path(path: &str) -> bool {
    path == LOGIN_PATH || path == JOIN_PATH
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

/// The startup warning of a public instance whose proxy is not trusted:
/// every client then shares the proxy's address for the sign-in throttle
/// and the limits.
pub fn warn_untrusted_forwarding(state: &AppState) {
    if enabled(&state.config) && state.config.public_mode && !state.config.trust_forward {
        tracing::warn!(
            target: "rustybin::auth",
            "RUSTYBIN_PUBLIC_MODE without RUSTYBIN_TRUST_FORWARD: behind a proxy every client \
             shares the proxy's address, for the sign-in throttle and every limit. Set \
             RUSTYBIN_TRUST_FORWARD=true (and RUSTYBIN_TRUST_FORWARD_HEADER) when a proxy you \
             run sets the client address"
        );
    }
}

/// Once per process: a sign-in arrived with a forwarding header the
/// instance does not trust, so the throttle counts the proxy's address.
fn warn_untrusted_proxy(headers: &HeaderMap, config: &Config) {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if config.trust_forward || WARNED.load(Ordering::Relaxed) {
        return;
    }
    let forwarded = ["x-forwarded-for", "forwarded", "x-real-ip", "fly-client-ip"]
        .iter()
        .any(|h| headers.contains_key(*h));
    if forwarded && !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            target: "rustybin::auth",
            "a sign-in came through a proxy (a forwarding header is present) but \
             RUSTYBIN_TRUST_FORWARD is off: the sign-in throttle and the limits count the \
             proxy's address for every client behind it"
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

/// A hash nobody's password matches, verified where there is no hash to
/// check (an unknown name, an invited user, user one's plain password from
/// the environment or the default): every sign-in runs exactly one argon2id
/// verify under a hash permit, so neither the time nor a `busy` answer
/// tells a real user from a name nobody has. Made once per process, with
/// the parameters every stored hash uses.
fn dummy_hash() -> &'static str {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DUMMY.get_or_init(|| {
        hash_password("rustybin-no-such-user").unwrap_or_else(|_| {
            "$argon2id$v=19$m=19456,t=2,p=1$cnVzdHliaW5kdW1teQ$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string()
        })
    })
}

/// Verify `plain` against `hash` (the dummy one when `None`) off the async
/// runtime, under a hash permit. `None`: every permit is taken.
async fn verify_with_permit(state: &AppState, hash: Option<String>, plain: &str) -> Option<bool> {
    let permit = state.config.console_tokens.hash_permit()?;
    let plain = plain.to_string();
    Some(
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            match hash {
                Some(h) => verify_hash(&h, &plain),
                None => {
                    let _ = verify_hash(dummy_hash(), &plain);
                    false
                }
            }
        })
        .await
        .unwrap_or(false),
    )
}

/// Do `user` and `password` sign in? The stored or seeded hash wins, else the
/// plain password from the environment, else the default. Both the user name
/// and the password are always checked (constant-time comparison, one argon2id
/// verify off the async runtime: the dummy one without a hash), whatever the
/// outcome. `None`: every hash permit is taken (`HASH_PERMITS`), try again in
/// a moment.
pub async fn check_credentials(state: &AppState, user: &str, password: &str) -> Option<bool> {
    let user_ok = constant_time_eq(user, &effective_user(&state.settings));
    let password_ok = match state.settings.secret(HASH_KEY) {
        Some(hash) => verify_with_permit(state, Some(hash.to_string()), password).await?,
        None => {
            verify_with_permit(state, None, password).await?;
            constant_time_eq(
                password,
                state
                    .config
                    .console_password
                    .as_deref()
                    .unwrap_or(DEFAULT_PASSWORD),
            )
        }
    };
    Some(user_ok & password_ok)
}

fn busy() -> Response {
    let mut resp = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "error": "too many sign-ins at once: try again in a moment",
            "reason": "busy",
            "retry_after": 1,
        })),
    )
        .into_response();
    resp.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    resp
}

// ── Tokens: generation, revocation, sessions, attempts ───────────────

/// One issued sign-in token (for Management, Sign-in).
#[derive(Clone, Debug)]
struct Issued {
    jti: String,
    user: String,
    user_id: String,
    issued_at: i64,
    /// When it was issued, in milliseconds (the `rb_ms` claim): unique
    /// and increasing, the order the per-user floor follows.
    issued_ms: i64,
    expires_at: i64,
    ip: Option<IpAddr>,
    last_used: i64,
}

impl Issued {
    fn of(claims: &Value, user: &str, user_id: &str, ip: Option<IpAddr>) -> Self {
        let iat = claims["iat"].as_i64().unwrap_or(0);
        Self {
            jti: claims["jti"].as_str().unwrap_or("").to_string(),
            user: user.to_string(),
            user_id: user_id.to_string(),
            issued_at: iat,
            issued_ms: token_ms(claims),
            expires_at: claims["exp"].as_i64().unwrap_or(0),
            ip,
            last_used: iat,
        }
    }
}

/// When a sign-in token was issued, in milliseconds: `rb_ms`, else `iat`
/// (a token from before 0.5).
fn token_ms(claims: &Value) -> i64 {
    claims
        .get("rb_ms")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| claims["iat"].as_i64().unwrap_or(0).saturating_mul(1000))
}

#[derive(Clone, Copy, Debug, Default)]
struct Attempt {
    failures: u32,
    /// Attempts under way (counted in `check`, settled by `failed` or
    /// `succeeded`): parallel guesses count before their answers.
    pending: u32,
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
    /// Subjects whose tokens are refused (until the time given): an OAuth
    /// client revoked from Management, AI agents. Kept across "sign out
    /// everywhere".
    revoked_subjects: Mutex<BTreeMap<String, i64>>,
    /// Token ids revoked with an AI agent (Management, AI agents): a sign
    /// out everywhere never clears them; rebuilt from the agents' records at
    /// load.
    agent_revoked: Mutex<BTreeMap<String, i64>>,
    issued: Mutex<VecDeque<Issued>>,
    /// Per user: sign-in tokens issued before this time (milliseconds) are
    /// refused, their sessions ended when the user went past
    /// [`MAX_SESSIONS_PER_USER`]. A floor, not a list: however many times
    /// a user signs in, it takes one entry and never pushes out anyone
    /// else's revocation. Dropped once every token under it expired.
    floors: Mutex<HashMap<String, i64>>,
    /// The last `rb_ms` handed out (unique and increasing).
    last_ms: std::sync::atomic::AtomicI64,
    /// Failed attempts per client address (an IPv6 /64) and user name: one
    /// client behind a shared address (a proxy without
    /// `RUSTYBIN_TRUST_FORWARD`) typing another name does not lock the
    /// real user out.
    attempts: Mutex<HashMap<AttemptKey, Attempt>>,
    /// Failures from every address in the current window, per user name
    /// tried (lower case; the invitation and single sign-on lanes are names
    /// no user can have). At most [`MAX_ATTEMPT_ADDRESSES`] names.
    global: Mutex<HashMap<String, GlobalWindow>>,
    /// One writer of `sessions.json` at a time, the snapshot taken under it:
    /// a slower save never overwrites a newer one (a revocation is never lost).
    saving: Mutex<()>,
    /// Password hashes computed at once (argon2id takes 19 MiB each).
    hashing: Arc<tokio::sync::Semaphore>,
    /// `<state>/sessions.json`: the generation and the revocations survive a
    /// restart there.
    file: Option<std::path::PathBuf>,
}

/// What the sign-in throttle counts: the client address (an IPv6 /64) and
/// the user name tried (lower case, at most 64 characters).
type AttemptKey = (IpAddr, String);

/// Failures at one name from every address in one window.
#[derive(Clone, Copy, Debug, Default)]
struct GlobalWindow {
    start: Duration,
    failures: u32,
}

fn name_lane(user: &str) -> String {
    user.chars()
        .take(MAX_USER_CHARS)
        .collect::<String>()
        .to_lowercase()
}

fn attempt_key(ip: Option<IpAddr>, user: &str) -> Option<AttemptKey> {
    ip.map(|ip| (crate::session::address_bucket(ip), name_lane(user)))
}

/// Password hashes (sign-in, password change) computed at once.
pub const HASH_PERMITS: usize = 4;
/// The file of the sign-in state under the state directory.
pub const SESSIONS_FILE: &str = "sessions.json";

/// What `<state>/sessions.json` holds.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct SessionsDoc {
    version: u32,
    generation: u64,
    revoked: BTreeMap<String, i64>,
    revoked_subjects: BTreeMap<String, i64>,
    agent_revoked: BTreeMap<String, i64>,
    floors: BTreeMap<String, i64>,
}

/// Users with a session floor at most (more than the user store holds).
const MAX_FLOORS: usize = 1024;

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
        // Without a state file the generation starts at the start time, so it
        // only grows across restarts: a restart signs every token out rather
        // than letting a revoked one work again.
        let start = u64::try_from(clock.wall().timestamp()).unwrap_or(1).max(1);
        Self {
            clock,
            generation: AtomicU64::new(start),
            revoked: Mutex::new(BTreeMap::new()),
            revoked_subjects: Mutex::new(BTreeMap::new()),
            agent_revoked: Mutex::new(BTreeMap::new()),
            issued: Mutex::new(VecDeque::new()),
            floors: Mutex::new(HashMap::new()),
            last_ms: std::sync::atomic::AtomicI64::new(0),
            attempts: Mutex::new(HashMap::new()),
            global: Mutex::new(HashMap::new()),
            saving: Mutex::new(()),
            hashing: Arc::new(tokio::sync::Semaphore::new(HASH_PERMITS)),
            file: None,
        }
    }

    /// The sign-in state of an instance: kept in `<state>/sessions.json`
    /// when there is a state directory (the generation, revoked token ids
    /// and subjects survive a restart), else in memory.
    pub fn persistent(state_dir: Option<&std::path::Path>) -> Self {
        Self::persistent_with_clock(state_dir, Arc::new(SystemClock::new()))
    }

    pub fn persistent_with_clock(
        state_dir: Option<&std::path::Path>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let mut t = Self::with_clock(clock);
        let Some(dir) = state_dir else { return t };
        if let Err(e) = crate::settings::create_private_dir(dir) {
            tracing::warn!(target: "rustybin::auth", dir = %dir.display(), error = %e, "the state directory could not be created: the sign-in state stays in memory");
            return t;
        }
        let path = dir.join(SESSIONS_FILE);
        if let Ok(text) = std::fs::read_to_string(&path) {
            match serde_json::from_str::<SessionsDoc>(&text) {
                Ok(doc) => {
                    let now = t.now();
                    let live = |m: BTreeMap<String, i64>, cap: usize| -> BTreeMap<String, i64> {
                        m.into_iter().filter(|(_, e)| *e > now).take(cap).collect()
                    };
                    t.generation = AtomicU64::new(doc.generation.max(1));
                    *lock(&t.revoked) = live(doc.revoked, MAX_REVOKED);
                    *lock(&t.revoked_subjects) = live(doc.revoked_subjects, MAX_REVOKED_SUBJECTS);
                    *lock(&t.agent_revoked) = live(doc.agent_revoked, MAX_REVOKED);
                    *lock(&t.floors) = doc
                        .floors
                        .into_iter()
                        .filter(|(_, f)| floor_live(*f, now))
                        .take(MAX_FLOORS)
                        .collect();
                }
                Err(e) => {
                    tracing::warn!(target: "rustybin::auth", file = %path.display(), error = %e, "the sign-in state could not be read: every earlier token is signed out");
                }
            }
        }
        t.file = Some(path);
        t.save();
        t
    }

    /// Write the generation and the revocations (0600, replaced atomically).
    fn save(&self) {
        let Some(path) = &self.file else { return };
        let _writer = lock(&self.saving);
        let doc = SessionsDoc {
            version: 1,
            generation: self.generation(),
            revoked: lock(&self.revoked).clone(),
            revoked_subjects: lock(&self.revoked_subjects).clone(),
            agent_revoked: lock(&self.agent_revoked).clone(),
            floors: lock(&self.floors)
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
        };
        let Ok(text) = serde_json::to_vec_pretty(&doc) else {
            return;
        };
        if let Err(e) = crate::settings::write_private(path, &text) {
            tracing::warn!(target: "rustybin::auth", file = %path.display(), error = %e, "the sign-in state could not be saved");
        }
    }

    /// A permit to compute one password hash, if one is free now.
    pub fn hash_permit(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.hashing.clone().try_acquire_owned().ok()
    }

    pub(crate) fn now(&self) -> i64 {
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
        let n = {
            let mut issued = lock(&self.issued);
            let n = issued.len();
            issued.clear();
            n
        };
        self.save();
        n
    }

    /// The number of listed sessions.
    pub fn listed(&self) -> usize {
        lock(&self.issued).len()
    }

    /// Forget sessions whose tokens expired.
    fn prune(&self) {
        let now = self.now();
        lock(&self.revoked).retain(|_, exp| *exp > now);
        lock(&self.agent_revoked).retain(|_, exp| *exp > now);
        lock(&self.issued).retain(|s| s.expires_at > now);
        lock(&self.floors).retain(|_, f| floor_live(*f, now));
    }

    /// A unique, increasing issue time in milliseconds for a new token.
    fn next_ms(&self) -> i64 {
        let now = self.clock.wall().timestamp_millis();
        let mut last = self.last_ms.load(Ordering::Relaxed);
        loop {
            let next = now.max(last.saturating_add(1));
            match self.last_ms.compare_exchange_weak(
                last,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return next,
                Err(seen) => last = seen,
            }
        }
    }

    /// Record a freshly issued token. Past [`MAX_SESSIONS_PER_USER`] the
    /// user's oldest session ends; past [`MAX_SESSIONS`] the oldest of the
    /// user with the most. An ended session's token is refused through the
    /// user's floor, so nothing stays live unlisted.
    fn record(&self, issued: Issued) {
        self.prune();
        let mut ended: Vec<Issued> = Vec::new();
        {
            let mut list = lock(&self.issued);
            let uid = issued.user_id.clone();
            list.push_back(issued);
            if list.iter().filter(|s| s.user_id == uid).count() > MAX_SESSIONS_PER_USER {
                if let Some(e) = list
                    .iter()
                    .position(|s| s.user_id == uid)
                    .and_then(|p| list.remove(p))
                {
                    ended.push(e);
                }
            }
            while list.len() > MAX_SESSIONS {
                let mut counts: HashMap<&str, usize> = HashMap::new();
                for s in list.iter() {
                    *counts.entry(s.user_id.as_str()).or_default() += 1;
                }
                let busiest = counts
                    .into_iter()
                    .max_by_key(|(_, n)| *n)
                    .map(|(u, _)| u.to_string());
                match busiest
                    .and_then(|u| list.iter().position(|s| s.user_id == u))
                    .and_then(|p| list.remove(p))
                {
                    Some(e) => ended.push(e),
                    None => break,
                }
            }
        }
        if ended.is_empty() {
            return;
        }
        {
            let mut floors = lock(&self.floors);
            for e in &ended {
                let f = floors.entry(e.user_id.clone()).or_insert(0);
                *f = (*f).max(e.issued_ms.saturating_add(1));
            }
            while floors.len() > MAX_FLOORS {
                let Some(k) = floors
                    .iter()
                    .min_by_key(|(_, f)| **f)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                floors.remove(&k);
            }
        }
        self.save();
    }

    /// Was this sign-in token of `user_id` ended by the per-user cap (issued
    /// before the user's floor)?
    fn below_floor(&self, user_id: &str, claims: &Value) -> bool {
        lock(&self.floors)
            .get(user_id)
            .is_some_and(|f| token_ms(claims) < *f)
    }

    /// Revoke a token id until `expires_at`.
    pub fn revoke(&self, jti: &str, expires_at: i64) {
        self.prune();
        lock(&self.issued).retain(|s| s.jti != jti);
        insert_bounded(&mut lock(&self.revoked), jti, expires_at, MAX_REVOKED);
        self.save();
    }

    /// Revoke a token id with an AI agent: kept apart from sign-outs, so a
    /// sign out everywhere (which clears those) keeps it.
    pub fn revoke_for_agent(&self, jti: &str, expires_at: i64) {
        self.prune();
        lock(&self.issued).retain(|s| s.jti != jti);
        insert_bounded(&mut lock(&self.agent_revoked), jti, expires_at, MAX_REVOKED);
        self.save();
    }

    /// Re-add an agent's revocation at load, without writing the file.
    pub fn remember_agent_revocation(&self, jti: &str, expires_at: i64) {
        if expires_at > self.now() {
            insert_bounded(&mut lock(&self.agent_revoked), jti, expires_at, MAX_REVOKED);
        }
    }

    pub fn is_revoked(&self, jti: &str) -> bool {
        let now = self.now();
        lock(&self.revoked).get(jti).is_some_and(|exp| *exp > now)
            || lock(&self.agent_revoked)
                .get(jti)
                .is_some_and(|exp| *exp > now)
    }

    /// Take back a revocation (an agent restored by the operator).
    pub fn unrevoke(&self, jti: &str) -> bool {
        let was = lock(&self.agent_revoked).remove(jti).is_some()
            | lock(&self.revoked).remove(jti).is_some();
        if was {
            self.save();
        }
        was
    }

    /// Refuse every token of `sub` until `until`.
    pub fn revoke_subject(&self, sub: &str, until: i64) {
        let now = self.now();
        {
            let mut subjects = lock(&self.revoked_subjects);
            subjects.retain(|_, exp| *exp > now);
            insert_bounded(&mut subjects, sub, until, MAX_REVOKED_SUBJECTS);
        }
        self.save();
    }

    pub fn restore_subject(&self, sub: &str) -> bool {
        let was = lock(&self.revoked_subjects).remove(sub).is_some();
        if was {
            self.save();
        }
        was
    }

    pub fn is_subject_revoked(&self, sub: &str) -> bool {
        let now = self.now();
        lock(&self.revoked_subjects)
            .get(sub)
            .is_some_and(|exp| *exp > now)
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

    /// The listed sessions of one user, newest first.
    pub fn sessions_of(&self, user_id: &str) -> Vec<Value> {
        self.sessions()
            .into_iter()
            .filter(|s| s["user_id"] == user_id)
            .collect()
    }

    /// Forget one user's listed sessions (their tokens are refused by their
    /// generation, which the caller moved on). Answers how many there were.
    pub fn forget_user(&self, user_id: &str) -> usize {
        let mut issued = lock(&self.issued);
        let before = issued.len();
        issued.retain(|s| s.user_id != user_id);
        before - issued.len()
    }

    /// The user id a listed session belongs to.
    pub fn session_user(&self, jti: &str) -> Option<String> {
        lock(&self.issued)
            .iter()
            .find(|s| s.jti == jti)
            .map(|s| s.user_id.clone())
    }

    /// End one listed session when it is `user_id`'s. `false` otherwise.
    pub fn end_of(&self, user_id: &str, jti: &str) -> bool {
        let theirs = lock(&self.issued)
            .iter()
            .any(|s| s.jti == jti && s.user_id == user_id);
        theirs && self.end(jti)
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
                    "user_id": s.user_id,
                    "issued_at": rfc3339(s.issued_at),
                    "expires_at": rfc3339(s.expires_at),
                    "last_used": rfc3339(s.last_used),
                    "ip": s.ip.map(|ip| ip.to_string()),
                })
            })
            .collect()
    }

    /// May `ip` try to sign in as `user`? `Err(seconds)` while it waits
    /// (the address and name, or the name from every address once the
    /// window's failures at it reach [`NAME_FAILURES_PER_WINDOW`]). An allowed
    /// attempt counts as pending until `failed` or `succeeded` settles it,
    /// so parallel guesses are refused once the window's allowance is taken.
    pub fn check(&self, ip: Option<IpAddr>, user: &str) -> Result<(), u64> {
        let now = self.clock.mono();
        if let Some(g) = lock(&self.global).get(&name_lane(user)) {
            let elapsed = now.saturating_sub(g.start);
            if elapsed <= Duration::from_secs(WINDOW_SECS) && g.failures >= NAME_FAILURES_PER_WINDOW
            {
                return Err(Duration::from_secs(WINDOW_SECS)
                    .saturating_sub(elapsed)
                    .as_secs()
                    .max(1));
            }
        }
        let Some(key) = attempt_key(ip, user) else {
            return Ok(());
        };
        let mut attempts = lock(&self.attempts);
        if !attempts.contains_key(&key) && attempts.len() >= MAX_ATTEMPT_ADDRESSES {
            Self::evict_attempts(&mut attempts, now);
        }
        let a = attempts.entry(key).or_default();
        if let Some(until) = a.blocked_until.filter(|u| *u > now) {
            return Err((until - now).as_secs().max(1));
        }
        if now.saturating_sub(a.window_start) > Duration::from_secs(WINDOW_SECS) {
            a.failures = 0;
            a.pending = 0;
            a.window_start = now;
        }
        if a.failures + a.pending >= FAILURES_PER_WINDOW {
            let left =
                Duration::from_secs(WINDOW_SECS).saturating_sub(now.saturating_sub(a.window_start));
            return Err(left.as_secs().max(1));
        }
        a.pending += 1;
        a.last = now;
        Ok(())
    }

    fn evict_attempts(attempts: &mut HashMap<AttemptKey, Attempt>, now: Duration) {
        let stale = Duration::from_secs(MAX_BLOCK_SECS);
        attempts.retain(|_, a| {
            a.blocked_until.is_some_and(|u| u > now) || now.saturating_sub(a.last) < stale
        });
        if attempts.len() >= MAX_ATTEMPT_ADDRESSES {
            if let Some(oldest) = attempts
                .iter()
                .min_by_key(|(_, a)| a.last)
                .map(|(k, _)| k.clone())
            {
                attempts.remove(&oldest);
            }
        }
    }

    /// Make room for a name: windows that ended go, else the name with the
    /// fewest failures (junk names never push out the count of a name
    /// under attack).
    fn evict_names(global: &mut HashMap<String, GlobalWindow>, now: Duration) {
        let window = Duration::from_secs(WINDOW_SECS);
        global.retain(|_, g| now.saturating_sub(g.start) <= window);
        if global.len() >= MAX_ATTEMPT_ADDRESSES {
            if let Some(k) = global
                .iter()
                .min_by_key(|(_, g)| (g.failures, g.start))
                .map(|(k, _)| k.clone())
            {
                global.remove(&k);
            }
        }
    }

    /// Settle a pending attempt that did not run (the hash pool was full).
    pub fn abandoned(&self, ip: Option<IpAddr>, user: &str) {
        let Some(key) = attempt_key(ip, user) else {
            return;
        };
        if let Some(a) = lock(&self.attempts).get_mut(&key) {
            a.pending = a.pending.saturating_sub(1);
        }
    }

    /// A failed attempt from `ip` as `user`.
    pub fn failed(&self, ip: Option<IpAddr>, user: &str) {
        let now = self.clock.mono();
        {
            let mut global = lock(&self.global);
            let lane = name_lane(user);
            if !global.contains_key(&lane) && global.len() >= MAX_ATTEMPT_ADDRESSES {
                Self::evict_names(&mut global, now);
            }
            let g = global.entry(lane).or_default();
            if now.saturating_sub(g.start) > Duration::from_secs(WINDOW_SECS) {
                g.start = now;
                g.failures = 0;
            }
            g.failures = g.failures.saturating_add(1);
        }
        let Some(key) = attempt_key(ip, user) else {
            return;
        };
        let mut attempts = lock(&self.attempts);
        if !attempts.contains_key(&key) && attempts.len() >= MAX_ATTEMPT_ADDRESSES {
            Self::evict_attempts(&mut attempts, now);
        }
        let a = attempts.entry(key).or_default();
        a.pending = a.pending.saturating_sub(1);
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

    /// A successful sign-in from `ip` as `user` clears its record.
    pub fn succeeded(&self, ip: Option<IpAddr>, user: &str) {
        if let Some(key) = attempt_key(ip, user) {
            lock(&self.attempts).remove(&key);
        }
    }
}

/// Can a token under a floor of `floor_ms` still be live at `now`?
fn floor_live(floor_ms: i64, now: i64) -> bool {
    floor_ms / 1000 + TOKEN_TTL_SECS + LEEWAY_SECS as i64 > now
}

/// Insert into a bounded map of expiries, dropping the entry that expires
/// first past `cap`.
fn insert_bounded(map: &mut BTreeMap<String, i64>, key: &str, until: i64, cap: usize) {
    map.insert(key.to_string(), until);
    while map.len() > cap {
        let Some(first) = map
            .iter()
            .min_by_key(|(_, exp)| **exp)
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        map.remove(&first);
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

/// The `scope` claim a sign-in token carries for a user's permissions:
/// informational (every request resolves the user again), and what a 0.3
/// binary would honour after a downgrade.
fn scope_claim(perms: crate::rbac::PermSet) -> String {
    let grant = crate::rbac::grant_of(perms);
    if grant.admin {
        "admin".to_string()
    } else {
        grant.scopes().join(" ")
    }
}

/// Mint a token for `user`: the claims and the signed token. `sub` is the
/// user id, `rb_user` the sign-in name, `rb_gen` the instance's generation
/// and `rb_ugen` the user's.
fn mint(config: &Config, user: &crate::rbac::User) -> Option<(String, Value)> {
    let tokens = &config.console_tokens;
    let perms = config
        .users
        .resolve(&user.id)
        .map(|r| r.permissions)
        .unwrap_or_default();
    let iat = tokens.now();
    let claims = json!({
        "iss": "rustybin",
        "aud": config.control_jwt_audience,
        "sub": user.id,
        "rb_user": user.name,
        "scope": scope_claim(perms),
        "iat": iat,
        "exp": iat + TOKEN_TTL_SECS,
        "jti": random_id(),
        "rb_use": TOKEN_USE,
        "rb_gen": tokens.generation(),
        "rb_ugen": user.generation,
        "rb_ms": tokens.next_ms(),
    });
    let mut claims = claims;
    if user.is_sso() {
        // Informational: how the person signed in (the session answer and
        // the console say so).
        claims["rb_via"] = json!("sso");
    }
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

/// Verify a sign-in token: signature, audience, expiry, use, generation,
/// the denylist and its user (there, enabled, of the user's generation).
/// Returns its claims.
pub fn verify(token: &str, config: &Config) -> Result<Value, JwtError> {
    verify_resolved(token, config).map(|(claims, _)| claims)
}

/// [`verify`], with the user's roles and permissions resolved
/// (`crate::rbac::from_sign_in`).
pub fn verify_resolved(
    token: &str,
    config: &Config,
) -> Result<(Value, crate::rbac::Resolved), JwtError> {
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
    let resolved = crate::rbac::from_sign_in(&claims, &config.users)?;
    if resolved
        .user_id()
        .is_some_and(|id| tokens.below_floor(id, &claims))
    {
        return Err(JwtError::Revoked);
    }
    tokens.touch(jti);
    Ok((claims, resolved))
}

/// The verified claims of the sign-in token a request presents, if it does.
fn presented_claims(headers: &HeaderMap, config: &Config) -> Option<Value> {
    presented_resolved(headers, config).map(|(c, _)| c)
}

fn presented_resolved(
    headers: &HeaderMap,
    config: &Config,
) -> Option<(Value, crate::rbac::Resolved)> {
    let token = presented_token(headers)?;
    if !is_console_token(token) {
        return None;
    }
    verify_resolved(token, config).ok()
}

/// The session id (`jti`) of the sign-in token a request presents.
pub fn current_session(headers: &HeaderMap, config: &Config) -> Option<String> {
    presented_claims(headers, config)
        .and_then(|c| c.get("jti").and_then(Value::as_str).map(str::to_string))
}

/// Mint a token for `account` and list its session. `None` when it could
/// not be signed.
fn issue(
    config: &Config,
    account: &crate::rbac::User,
    ip: Option<IpAddr>,
) -> Option<(String, Value)> {
    let (token, claims) = mint(config, account)?;
    config
        .console_tokens
        .record(Issued::of(&claims, &account.name, &account.id, ip));
    Some((token, claims))
}

/// A sign-in token for a user single sign-on verified
/// (`crate::sso::api`): listed as a session, the sign-in noted, answered as
/// `POST /_rustybin/login` answers (plus `obtained: "sso"`). `None` when it
/// could not be signed.
pub(crate) fn sso_session(
    state: &AppState,
    account: &crate::rbac::User,
    ip: Option<IpAddr>,
) -> Option<Value> {
    let (token, claims) = issue(&state.config, account, ip)?;
    if let Err(e) = state.users.note_sign_in(&account.id) {
        tracing::warn!(target: "rustybin::auth", error = %e, "the sign-in time could not be saved");
    }
    let mut body = token_body(&token, &claims, account, state);
    body["obtained"] = json!("sso");
    body["display_name"] = json!(account.display_name);
    body["roles"] = json!(account.roles);
    Some(body)
}

/// The refusal of a password sign-in while single sign-on is the only way
/// in (`sso.password_sign_in` off and single sign-on set up).
fn password_sign_in_off() -> Response {
    error(
        StatusCode::FORBIDDEN,
        "the password sign-in is off on this instance: sign in with single sign-on (or the admin token)",
        "password_sign_in_off",
    )
}

/// The sign-in name a sign-in token carries: `rb_user`, or the `sub` of a
/// 0.3 token (`user:<name>`).
pub fn user_of(claims: &Value) -> Option<&str> {
    claims.get("rb_user").and_then(Value::as_str).or_else(|| {
        claims
            .get("sub")
            .and_then(Value::as_str)
            .and_then(|s| s.strip_prefix("user:"))
    })
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

fn audit(event: &str, user: &str, ip: Option<IpAddr>, outcome: &str) {
    crate::audit::event(event);
    tracing::info!(
        target: "rustybin::auth",
        event,
        user = %crate::logging::printable(user, MAX_USER_CHARS),
        ip = %ip.map(|i| i.to_string()).unwrap_or_default(),
        outcome,
        "console sign-in"
    );
}

fn client_ip(headers: &HeaderMap, extensions: &Extensions, config: &Config) -> Option<IpAddr> {
    crate::session::client_ip(headers, extensions, config)
}

/// A token response body.
fn token_body(token: &str, claims: &Value, user: &crate::rbac::User, state: &AppState) -> Value {
    let exp = claims.get("exp").and_then(Value::as_i64).unwrap_or(0);
    let perms = state
        .users
        .resolve(&user.id)
        .map(|r| r.permissions)
        .unwrap_or_default();
    json!({
        "token": token,
        "token_type": "Bearer",
        "expires_in": TOKEN_TTL_SECS,
        "expires_at": rfc3339(exp),
        "scopes": crate::rbac::grant_of(perms).scopes(),
        "user": user.name,
        "user_id": user.id,
        "default_password": default_password(state),
    })
}

/// Check the password of a user who is not user one: their own hash in the
/// user store. Without one (an invitation not accepted yet) it is refused
/// after the same work as a real check (the dummy verify). `None`: every
/// hash permit is taken.
async fn check_user_password(
    state: &AppState,
    user: &crate::rbac::User,
    password: &str,
) -> Option<bool> {
    let hash = state.users.password_hash(&user.id);
    verify_with_permit(state, hash, password).await
}

/// User one (the first admin), created now if startup could not.
fn first_admin(state: &AppState) -> Option<crate::rbac::User> {
    if let Some(u) = state.users.first_admin() {
        return Some(u);
    }
    crate::rbac::migrate(&state.config, &state.settings);
    state.users.first_admin()
}

/// Check a sign-in: the throttle (per address bucket and user name, and
/// per user name from every address together), the user store (user one through the sign-in
/// settings), the password, a disabled user answered as a wrong password.
/// The answers are the console sign-in's, whichever route asks: the
/// console's (`POST /_rustybin/login`) and the portal's (`POST
/// /portal/api/login`) share the throttle and the audit target (`event`
/// names the route). `Ok` is the user and the client's address.
#[allow(clippy::result_large_err)]
pub(crate) async fn check_sign_in(
    state: &AppState,
    headers: &HeaderMap,
    extensions: &Extensions,
    username: &str,
    password: &str,
    event: &str,
) -> Result<(crate::rbac::User, Option<IpAddr>), Response> {
    let config = &state.config;
    // Single sign-on as the only way in (step 10) closes every password
    // sign-in: the console's and the portal's.
    if !crate::sso::password_sign_in(&state.settings) {
        return Err(password_sign_in_off());
    }
    let ip = client_ip(headers, extensions, config);
    warn_untrusted_proxy(headers, config);
    let tokens = &config.console_tokens;
    let tried = username;
    // The audit log names the user the sign-in is for only when it exists
    // (a name nobody has may be a password typed in the wrong field).
    if let Some(u) = state.users.find_by_name(tried) {
        crate::audit::actor_user(&u.id, &u.name);
    }
    if let Err(wait) = tokens.check(ip, tried) {
        audit(event, username, ip, "rate_limited");
        return Err(rate_limited(wait));
    }
    if username.chars().count() > MAX_USER_CHARS
        || password.chars().count() > MAX_PASSWORD_CHARS * 4
    {
        tokens.failed(ip, tried);
        audit(event, username, ip, "invalid_credentials");
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "invalid user name or password",
            "invalid_credentials",
        ));
    }
    // User one signs in with the sign-in settings (the 0.3 console user);
    // anyone else with their own password in the user store. A name nobody
    // has still checks the settings' password, so the answer takes as long.
    let first = constant_time_eq(tried, &effective_user(&state.settings));
    let other = if first {
        None
    } else {
        state
            .users
            .find_by_name(tried)
            .filter(|u| !state.users.is_first_admin(&u.id))
    };
    let checked = match &other {
        Some(u) => check_user_password(state, u, password).await,
        None => check_credentials(state, username, password).await,
    };
    let Some(ok) = checked else {
        tokens.abandoned(ip, tried);
        return Err(busy());
    };
    let account = if !ok {
        None
    } else if first {
        first_admin(state)
    } else {
        other
    };
    // A disabled user gets the answer of a wrong password.
    let Some(account) = account.filter(|u| !u.disabled) else {
        tokens.failed(ip, tried);
        audit(event, username, ip, "invalid_credentials");
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "invalid user name or password",
            "invalid_credentials",
        ));
    };
    tokens.succeeded(ip, tried);
    Ok((account, ip))
}

/// Does `user` hold `portal.read` and nothing else? Such an account opens
/// the API portal, not the console.
pub fn portal_only(state: &AppState, user: &crate::rbac::User) -> bool {
    state.users.resolve(&user.id).is_some_and(|r| {
        r.permissions.has(crate::rbac::Permission::PortalRead) && r.permissions.len() == 1
    })
}

/// The answer to a console sign-in (or invitation) of a portal-only
/// account, or an invitation accepted on the portal's page: `body` plus
/// `portal_only`, where to go, and the portal session cookie when the
/// portal is on.
fn portal_only_answer(
    state: &AppState,
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &axum::http::Uri,
    account: &crate::rbac::User,
    status: StatusCode,
    mut body: Value,
) -> Response {
    if body.get("portal_only").is_none() {
        body["portal_only"] = json!(true);
    }
    body["portal_url"] = json!("/portal/");
    body["user"] = json!(account.name);
    body["display_name"] = json!(account.display_name);
    let cookie = if crate::portal::active(state) {
        crate::portal::session::issue_cookie(state, headers, extensions, uri, account)
    } else {
        body["hint"] =
            json!("the API portal is off on this instance: ask its operator to publish something");
        None
    };
    body["portal_signed_in"] = json!(cookie.is_some());
    let mut resp = (status, Json(body)).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(c) = cookie {
        resp.headers_mut().insert(header::SET_COOKIE, c);
    }
    resp
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
    uri: axum::http::Uri,
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
    let (account, ip) = match check_sign_in(
        &state,
        &headers,
        &extensions,
        &body.username,
        &body.password,
        "login",
    )
    .await
    {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let tokens = &config.console_tokens;
    let user = account.name.clone();
    let Some((token, claims)) = mint(config, &account) else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not sign the token",
            "server_error",
        );
    };
    tokens.record(Issued::of(&claims, &user, &account.id, ip));
    if let Err(e) = state.users.note_sign_in(&account.id) {
        tracing::warn!(target: "rustybin::auth", error = %e, "the sign-in time could not be saved");
    }
    audit("login", &user, ip, "ok");
    let body = token_body(&token, &claims, &account, &state);
    // An account that opens only the API portal: the console sends it
    // there, signed in to the portal too (the token still serves its own
    // account, /_rustybin/me, and nothing else).
    if portal_only(&state, &account) {
        return portal_only_answer(
            &state,
            &headers,
            &extensions,
            &uri,
            &account,
            StatusCode::OK,
            body,
        );
    }
    let mut resp = Json(body).into_response();
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
        let resolved = crate::rbac::Resolved::admin_token();
        fill(
            &mut body,
            json!({ "obtained": "admin_token", "scopes": resolved.grant().scopes() }),
        );
        fill(&mut body, resolved.session_fields());
        return Json(body).into_response();
    }
    if let Some((claims, resolved)) = presented_resolved(&headers, config) {
        let name = match &resolved.principal {
            crate::rbac::Principal::User { name, .. } => Some(name.as_str()),
            _ => user_of(&claims),
        };
        let obtained = if claims.get("rb_via").and_then(Value::as_str) == Some("sso") {
            "sso"
        } else {
            "password"
        };
        fill(
            &mut body,
            json!({
                "obtained": obtained,
                "user": name,
                "scopes": resolved.grant().scopes(),
                "issued_at": rfc3339(claims["iat"].as_i64().unwrap_or(0)),
                "expires_at": rfc3339(claims["exp"].as_i64().unwrap_or(0)),
                "id": claims["jti"],
            }),
        );
        fill(&mut body, resolved.session_fields());
        return Json(body).into_response();
    }
    if let Some(t) = token {
        if let Ok((resolved, claims)) = crate::control_auth::verify_jwt_resolved(t, config) {
            fill(
                &mut body,
                json!({
                    "obtained": "control_jwt",
                    "sub": claims.get("sub"),
                    "scopes": resolved.grant().scopes(),
                    "expires_at": claims.get("exp").and_then(Value::as_i64).map(rfc3339),
                }),
            );
            fill(&mut body, resolved.session_fields());
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

/// `DELETE /_rustybin/sessions/{id}`: the caller holds what the session's
/// user holds (`crate::rbac::service::end_session`).
async fn end_session_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    RoutePath(id): RoutePath<String>,
) -> Response {
    let config = &state.config;
    if !enabled(config) {
        return not_found(config);
    }
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    match crate::rbac::service::end_session(&state, &caller, &id) {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `DELETE /_rustybin/sessions`: sign out everywhere, for a caller holding
/// every permission (`crate::rbac::service::end_all_sessions`).
async fn end_all_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let config = &state.config;
    if !enabled(config) {
        return not_found(config);
    }
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    match crate::rbac::service::end_all_sessions(&state, &caller) {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
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

/// `PUT /_rustybin/password` (0.3): the caller's own password, as `PUT
/// /_rustybin/me/password`; a caller who is not a user (the admin token, a
/// control JWT) changes the first admin's, which needs `users.manage`.
async fn password_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    body: Result<Json<PasswordBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    change_password(state, headers, extensions, body, false).await
}

/// `PUT /_rustybin/me/password`: the caller's own password, with the
/// current one.
async fn me_password_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    body: Result<Json<PasswordBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    change_password(state, headers, extensions, body, true).await
}

async fn change_password(
    state: AppState,
    headers: HeaderMap,
    extensions: Extensions,
    body: Result<Json<PasswordBody>, axum::extract::rejection::JsonRejection>,
    me: bool,
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
                &format!("expected {{ \"current_password\": \"...\", \"new_password\": \"...\", \"username\"?: \"...\" }}: {}", e.body_text()),
                "invalid_body",
            )
        }
    };
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    match caller.user_id() {
        Some(id) if !state.users.is_first_admin(id) => {
            let id = id.to_string();
            own_password(&state, &id, &headers, &extensions, body).await
        }
        Some(_) => first_admin_password(&state, &headers, &extensions, body).await,
        None if me => error(
            StatusCode::BAD_REQUEST,
            "this credential is not a user's (the admin token or a control JWT): it has no password of its own",
            "not_a_user",
        ),
        None => match crate::rbac::require(&caller, crate::rbac::Permission::UsersManage) {
            Ok(()) => first_admin_password(&state, &headers, &extensions, body).await,
            Err(resp) => resp,
        },
    }
}

/// A user's own password (not user one's): the current one checked, the
/// new one stored in the user store, every other session of theirs ended
/// and a fresh token answered.
async fn own_password(
    state: &AppState,
    id: &str,
    headers: &HeaderMap,
    extensions: &Extensions,
    body: PasswordBody,
) -> Response {
    let config = &state.config;
    let Some(user) = state.users.get(id) else {
        return error(StatusCode::UNAUTHORIZED, "signed out", "revoked");
    };
    if user.is_sso() {
        return error(
            StatusCode::CONFLICT,
            "you sign in with single sign-on: your password is the identity provider's, never this instance's",
            crate::rbac::users::SSO_USER,
        );
    }
    let ip = client_ip(headers, extensions, config);
    let tokens = &config.console_tokens;
    if let Err(wait) = tokens.check(ip, &user.name) {
        return rate_limited(wait);
    }
    let current_ok = if body.current_password.chars().count() > MAX_PASSWORD_CHARS * 4 {
        false
    } else {
        match check_user_password(state, &user, &body.current_password).await {
            Some(ok) => ok,
            None => {
                tokens.abandoned(ip, &user.name);
                return busy();
            }
        }
    };
    if !current_ok {
        tokens.failed(ip, &user.name);
        audit("change_password", &user.name, ip, "wrong_current_password");
        return error(
            StatusCode::FORBIDDEN,
            "the current password is wrong",
            "invalid_credentials",
        );
    }
    tokens.succeeded(ip, &user.name);
    if body
        .username
        .as_deref()
        .map(str::trim)
        .is_some_and(|u| !u.is_empty() && u != user.name)
    {
        return error(
            StatusCode::BAD_REQUEST,
            "user name: only the first admin's sign-in name changes here (console.user); other users keep the name they were invited with",
            "invalid_user",
        );
    }
    if let Err(e) = valid_new_password(&body.new_password) {
        return error(
            StatusCode::BAD_REQUEST,
            &format!("new password: {e}"),
            "invalid_password",
        );
    }
    let Some(hash) = hash_off_runtime(tokens, &body.new_password).await else {
        return busy();
    };
    let listed = tokens.sessions_of(id).len();
    let user = match state.users.set_password(id, hash) {
        Ok(u) => u,
        Err(e) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("the password was not saved: {e}"),
                "io",
            )
        }
    };
    crate::rbac::service::ended_user(state, id);
    audit("change_password", &user.name, ip, "ok");
    let mut out = json!({
        "changed": true,
        "user": user.name,
        "signed_out": listed,
        "default_password": default_password(state),
    });
    if let Some((token, claims)) = issue(config, &user, ip) {
        out["token"] = json!(token);
        out["expires_at"] = json!(rfc3339(claims["exp"].as_i64().unwrap_or(0)));
    }
    let mut resp = Json(out).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// Hash a new password on the blocking pool behind a hash permit. `None`
/// when every permit is taken or the hash failed.
async fn hash_off_runtime(tokens: &Tokens, plain: &str) -> Option<String> {
    let permit = tokens.hash_permit()?;
    let plain = plain.to_string();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        hash_password(&plain)
    })
    .await
    .ok()
    .and_then(Result::ok)
}

/// The first admin's password (and user name): the sign-in settings, as in
/// 0.4. The first admin's sessions end (the settings move their
/// generation, which their sign-in, portal and agents' OAuth tokens carry);
/// other users stay signed in; the caller gets a fresh token when it signed
/// in with one.
async fn first_admin_password(
    state: &AppState,
    headers: &HeaderMap,
    extensions: &Extensions,
    body: PasswordBody,
) -> Response {
    let config = &state.config;
    let ip = client_ip(headers, extensions, config);
    let tokens = &config.console_tokens;
    let current_user = effective_user(&state.settings);
    if let Err(wait) = tokens.check(ip, &current_user) {
        return rate_limited(wait);
    }
    let current_ok = if body.current_password.chars().count() > MAX_PASSWORD_CHARS * 4 {
        false
    } else {
        match check_credentials(state, &current_user, &body.current_password).await {
            Some(ok) => ok,
            None => {
                tokens.abandoned(ip, &current_user);
                return busy();
            }
        }
    };
    if !current_ok {
        tokens.failed(ip, &current_user);
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
    tokens.succeeded(ip, &current_user);
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
    let Some(permit) = tokens.hash_permit() else {
        return busy();
    };
    let hash = match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        hash_password(&plain)
    })
    .await
    {
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
    let who = crate::settings::principal(headers, config);
    // The store signs the first admin's sessions out with the change (their
    // generation moves on: sign-in, portal and their agents' OAuth tokens):
    // count them, and see whether the caller signed in with one, first.
    let first_id = first_admin(state).map(|u| u.id);
    let listed = match &first_id {
        Some(id) => tokens.sessions_of(id).len(),
        None => tokens.listed(),
    };
    let had_session = presented_claims(headers, config).is_some();
    if let Err(e) = state.settings.update_sign_in(values, secrets, &who) {
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
    // The first admin's other sessions end, and their agents' MCP sessions;
    // the caller gets a fresh token when it signed in with one.
    if let Some(id) = &first_id {
        state.agents.end_owned_sessions(id);
    }
    let ended = listed;
    audit("change_password", &user, ip, "ok");
    let mut out = json!({
        "changed": true,
        "user": user,
        "signed_out": ended,
        "default_password": default_password(state),
    });
    let account = first_admin(state);
    if let Some(account) = account.filter(|_| had_session) {
        if let Some((token, claims)) = mint(config, &account) {
            tokens.record(Issued::of(&claims, &user, &account.id, ip));
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

// ── Invitations ─────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct JoinBody {
    #[serde(default)]
    token: String,
    #[serde(default)]
    password: String,
    /// Joined from the API portal's page (`/portal/#/join/<token>`): the
    /// answer signs in to the portal (its cookie) instead of the console.
    #[serde(default)]
    portal: bool,
}

fn locked_first_admin() -> Response {
    error(
        StatusCode::CONFLICT,
        "the first admin's password is held by RUSTYBIN_SETTINGS_LOCK (the environment is authoritative): the link stays valid for when the sign-in settings can change",
        "locked",
    )
}

fn invalid_invitation() -> Response {
    error(
        StatusCode::UNAUTHORIZED,
        "this invitation is not valid: it was used, it expired, or a newer one replaced it. Ask for a new link",
        "invalid_invitation",
    )
}

/// `POST /_rustybin/join`: accept an invitation with the new password (the
/// 0.3 rules). Open and rate limited like the sign-in (per address, and
/// every address together in the invitations' own lane); the token is checked before the password is
/// hashed, used once, and never logged. Answers a sign-in token: the person
/// is signed in. A join from the portal's page (`portal: true`) signs in to
/// the portal instead (its cookie, no console token); an account that opens
/// only the API portal gets the portal's cookie too, and `portal_only`.
async fn join_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: axum::http::Uri,
    body: Result<Json<JoinBody>, axum::extract::rejection::JsonRejection>,
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
                    "expected {{ \"token\": \"rbi_...\", \"password\": \"...\" }}: {}",
                    e.body_text()
                ),
                "invalid_body",
            )
        }
    };
    if !crate::sso::password_sign_in(&state.settings) {
        return password_sign_in_off();
    }
    let ip = client_ip(&headers, &extensions, config);
    warn_untrusted_proxy(&headers, config);
    let tokens = &config.console_tokens;
    if let Err(wait) = tokens.check(ip, JOIN_KEY) {
        audit("join", "", ip, "rate_limited");
        return rate_limited(wait);
    }
    if body.password.chars().count() > MAX_PASSWORD_CHARS * 4 {
        tokens.abandoned(ip, JOIN_KEY);
        return error(
            StatusCode::BAD_REQUEST,
            &format!("password: at most {MAX_PASSWORD_CHARS} characters"),
            "invalid_password",
        );
    }
    if let Err(e) = valid_new_password(&body.password) {
        tokens.abandoned(ip, JOIN_KEY);
        return error(
            StatusCode::BAD_REQUEST,
            &format!("password: {e}"),
            "invalid_password",
        );
    }
    let Some(invited) = state.users.invitation_user(&body.token) else {
        tokens.failed(ip, JOIN_KEY);
        audit("join", "", ip, "invalid_invitation");
        return invalid_invitation();
    };
    // User one's password is a sign-in setting: when the settings lock
    // holds it, the link is refused before it is used (it stays valid, and
    // nobody is signed out for nothing).
    if state.users.is_first_admin(&invited.id) && !state.settings.writable(HASH_KEY) {
        tokens.abandoned(ip, JOIN_KEY);
        return locked_first_admin();
    }
    crate::audit::actor_user(&invited.id, &invited.name);
    let Some(hash) = hash_off_runtime(tokens, &body.password).await else {
        tokens.abandoned(ip, JOIN_KEY);
        return busy();
    };
    let accepted = match state.users.accept_invitation(&body.token, hash.clone()) {
        Ok(a) => a,
        Err(crate::rbac::UsersError::NotFound) => {
            tokens.failed(ip, JOIN_KEY);
            audit("join", &invited.name, ip, "invalid_invitation");
            return invalid_invitation();
        }
        Err(e) => {
            tokens.abandoned(ip, JOIN_KEY);
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("the invitation could not be accepted: {e}"),
                "io",
            );
        }
    };
    // User one's password is the instance's sign-in setting.
    if accepted.first_admin {
        let mut secrets = BTreeMap::new();
        secrets.insert(HASH_KEY.to_string(), Some(hash));
        if let Err(e) = state
            .settings
            .update_sign_in(BTreeMap::new(), secrets, "invitation")
        {
            tokens.abandoned(ip, JOIN_KEY);
            return error(
                StatusCode::CONFLICT,
                &format!("the first admin's password could not be saved ({e:?}): ask for a new link once the sign-in settings can change"),
                "locked",
            );
        }
    }
    tokens.succeeded(ip, JOIN_KEY);
    tokens.forget_user(&accepted.user.id);
    let account = state
        .users
        .get(&accepted.user.id)
        .unwrap_or(accepted.user.clone());
    audit(
        if accepted.reset { "reset" } else { "join" },
        &account.name,
        ip,
        "ok",
    );
    if body.portal {
        if let Err(e) = state.users.note_sign_in(&account.id) {
            tracing::warn!(target: "rustybin::auth", error = %e, "the sign-in time could not be saved");
        }
        audit("join", &account.name, ip, "portal");
        return portal_only_answer(
            &state,
            &headers,
            &extensions,
            &uri,
            &account,
            StatusCode::OK,
            json!({
                "joined": true,
                "reset": accepted.reset,
                "portal_only": portal_only(&state, &account),
            }),
        );
    }
    let Some((token, claims)) = issue(config, &account, ip) else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not sign the token: sign in with the new password",
            "server_error",
        );
    };
    if let Err(e) = state.users.note_sign_in(&account.id) {
        tracing::warn!(target: "rustybin::auth", error = %e, "the sign-in time could not be saved");
    }
    let mut out = token_body(&token, &claims, &account, &state);
    out["joined"] = json!(true);
    out["reset"] = json!(accepted.reset);
    out["display_name"] = json!(account.display_name);
    if portal_only(&state, &account) {
        return portal_only_answer(
            &state,
            &headers,
            &extensions,
            &uri,
            &account,
            StatusCode::OK,
            out,
        );
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
        .route(ME_PASSWORD_PATH, put(me_password_handler))
        .route(JOIN_PATH, post(join_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            LOGIN_PATH,
            &["POST"],
            category::CONTROL,
            "Console sign-in: a user name and password for a control token (password mode)",
        )
.open()
        .description(
            "Never behind control-plane auth. Answers a 12 hour control token for the user \
             (the first admin is the console user of the sign-in settings) that every \
             control-plane route, the management MCP server and the admin guard accept, with \
             the user's roles resolved at every request. 5 failed attempts per client address and user name and minute (20 \
             at one user name from every address together), then 429 with Retry-After. \
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
.authenticated(&[])
        .example(Example::post("Sign out", LOGOUT_PATH).expect_status(404)),
        Endpoint::new(
            SESSION_PATH,
            &["GET"],
            category::CONTROL,
            "Who the presented credential is: user, roles, permissions, scopes, expiry, how it was obtained",
        )
.authenticated(&[])
        .description(
            "A sign-in token answers its user (user, user_id, display_name), resolved from the \
             user store at every request; every credential answers its roles and permissions \
             (the admin token: every permission).",
        )
        .example(Example::get("This session", SESSION_PATH).expect_status(404)),
        Endpoint::new(
            SESSIONS_PATH,
            &["GET", "DELETE"],
            category::CONTROL,
            "Sign-in sessions (admin): list them, or DELETE to sign out everywhere",
        )
.permission_for(&["GET"], crate::rbac::Permission::UsersRead).permission_for(&["DELETE"], crate::rbac::Permission::UsersManage)
        .example(Example::get("Sessions", SESSIONS_PATH).expect_status(404)),
        Endpoint::new(
            "/_rustybin/sessions/{id}",
            &["DELETE"],
            category::CONTROL,
            "End one sign-in session (admin)",
        )
.permission(crate::rbac::Permission::UsersManage)
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
            "Change your own password (0.3's route, an alias of PUT /_rustybin/me/password); the first admin's also takes a new user name",
        )
        .authenticated(&[])
        .description(
            "For the caller, with the current password. The first admin's password is the \
             secret console.password_hash (and its user name console.user, 409 when \
             RUSTYBIN_SETTINGS_LOCK holds them); other users' is in the user store. A caller \
             who is not a user (the admin token, a control JWT) changes the first admin's, \
             which needs users.manage. Ends the user's other sessions and answers a fresh \
             token.",
        )
        .example(
            Example::new("Change the password", "PUT", PASSWORD_PATH)
                .json(r#"{"current_password":"rustybin","new_password":"a-longer-secret"}"#)
                .expect_status(404),
        ),
        Endpoint::new(
            ME_PASSWORD_PATH,
            &["PUT"],
            category::CONTROL,
            "Change your own password (the current one required)",
        )
        .authenticated(&[])
        .description(
            "Any signed-in user: 8 to 256 characters, not the default. Ends your other \
             sessions and answers a fresh token. 400 not_a_user for the admin token or a \
             control JWT.",
        )
        .example(
            Example::new("Change my password", "PUT", ME_PASSWORD_PATH)
                .json(r#"{"current_password":"rustybin","new_password":"a-longer-secret"}"#)
                .expect_status(404),
        ),
        Endpoint::new(
            JOIN_PATH,
            &["POST"],
            category::CONTROL,
            "Accept an invitation: the link's token and your new password, for a sign-in token",
        )
        .open()
        .description(
            "Never behind control-plane auth; rate limited like the sign-in (5 failures per \
             address and minute, 20 from every address together in a lane of their own). The token works once, for \
             72 hours; a reset's ends the user's other sessions.",
        )
        .example(
            Example::post("Join", JOIN_PATH)
                .json(r#"{"token":"rbi_0000000000000000000000000000000000000000000","password":"a-longer-secret"}"#)
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
            "user_id": { "type": "string", "description": "The user's id in the user store (usr_ plus 16 hex)" },
            "default_password": { "type": "boolean", "description": "The instance still signs in with the default password" }
        }
    });
    let session_schema = json!({
        "type": "object",
        "properties": {
            "control_auth": { "type": "string" },
            "obtained": { "type": "string", "enum": ["password", "sso", "admin_token", "control_jwt", "api_key"], "description": "How the credential was obtained: password and sso are sign-in tokens (the user's own password, or single sign-on)" },
            "user": { "type": ["string", "null"], "description": "The sign-in name (sign-in tokens)" },
            "user_id": { "type": ["string", "null"], "description": "The user's id (sign-in tokens): usr_ plus 16 hex" },
            "display_name": { "type": ["string", "null"] },
            "roles": { "type": "array", "items": { "type": "string" }, "description": "The roles the permissions come from (admin for the admin token; a control JWT's scopes as roles plus rb_roles)" },
            "permissions": { "type": "array", "items": { "type": "string" }, "description": "The caller's permissions, resolved at this request (dotted names, mocks.write)" },
            "scopes": { "type": "array", "items": { "type": "string" }, "description": "The 0.3 scopes these permissions amount to" },
            "sub": { "type": "string", "description": "The sub claim of a control JWT" },
            "id": { "type": "string", "description": "The session id of a sign-in token" },
            "issued_at": { "type": "string", "format": "date-time" },
            "expires_at": { "type": "string", "format": "date-time" },
            "default_password": { "type": "boolean" },
            "sign_in_user": { "type": ["string", "null"], "description": "The first admin's sign-in name (password mode)" }
        }
    });
    let not_found = json!({ "description": "Not in password mode (or shared mode)" });
    json!({
        LOGIN_PATH: {
            "post": {
                "tags": ["Control Plane"],
                "summary": "Console sign-in",
                "description": "Looks the user up in the user store and checks the password: the first admin's is the sign-in settings' (RUSTYBIN_CONSOLE_USER, RUSTYBIN_CONSOLE_PASSWORD or _HASH, the stored console.password_hash; rustybin / rustybin by default). Answers a 12 hour control token whose permissions are the user's roles, resolved at every request (a disabled user is answered as a wrong password). Never behind control-plane auth. After 5 failed attempts from one client address within a minute the address waits (429, Retry-After, doubling up to 10 minutes).",
                "operationId": "consoleLogin",
                "security": [],
                "requestBody": { "required": true, "content": { "application/json": { "schema": {
                    "type": "object",
                    "required": ["username", "password"],
                    "properties": { "username": { "type": "string" }, "password": { "type": "string" } }
                } } } },
                "responses": {
                    "200": { "description": "Signed in", "content": { "application/json": { "schema": token_schema.clone() } } },
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
                "description": "Describes the credential the request presents: obtained (password, admin_token, control_jwt, api_key), the user (user, user_id, display_name) of a sign-in token, the roles and permissions resolved at this request, scopes, issue and expiry times, plus whether the instance still uses the default password.",
                "operationId": "getConsoleSession",
                "responses": {
                    "200": { "description": "The session", "content": { "application/json": { "schema": session_schema } } },
                    "404": { "description": "The control plane is open" }
                }
            }
        },
        SESSIONS_PATH: {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Sign-in sessions",
                "description": "The sign-in tokens issued since the last restart or sign-out-everywhere (at most 16 per user and 4096 in all, newest first; a sign-in past a user's 16 ends their oldest session): id, user, user_id, issue, expiry and last use times, client address. Admin.",
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
                "description": "Rotates the token generation: every sign-in token issued before now is refused, the caller's included, and every OAuth and agent token. Needs every permission (the admin role or the admin token).",
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
                "description": "Needs users.manage plus every permission the session's user holds (in the instance and in their projects), as for any change to that user.",
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
        },
        ME_PASSWORD_PATH: {
            "put": {
                "tags": ["Control Plane"],
                "summary": "Change my password",
                "description": "The caller's own password, with the current one: 8 to 256 characters, not the default. The first admin's is stored in the sign-in settings (console.password_hash), anyone else's in the user store. The caller's other sessions end and a fresh token is answered. 400 not_a_user for a credential that is not a user's.",
                "operationId": "changeMyPassword",
                "requestBody": { "required": true, "content": { "application/json": { "schema": {
                    "type": "object",
                    "required": ["current_password", "new_password"],
                    "properties": {
                        "current_password": { "type": "string" },
                        "new_password": { "type": "string", "minLength": 8, "maxLength": 256 }
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
                    "400": { "description": "The new password breaks a rule, or not a user" },
                    "403": { "description": "The current password is wrong" },
                    "404": not_found,
                    "429": { "description": "Too many wrong current passwords" }
                }
            }
        },
        JOIN_PATH: {
            "post": {
                "tags": ["Control Plane"],
                "summary": "Accept an invitation",
                "description": "The token of an invitation link (/ui/#/join/<token>) and the new password (8 to 256 characters, not the default). One use, 72 hours. Answers a sign-in token for the user, so the person is signed in. Never behind control-plane auth; rate limited per client address (5 failures a minute) and for every address together (20 a minute, the invitations' own lane: failures here never hold back a sign-in).",
                "operationId": "acceptInvitation",
                "security": [],
                "requestBody": { "required": true, "content": { "application/json": { "schema": {
                    "type": "object",
                    "required": ["token", "password"],
                    "properties": {
                        "token": { "type": "string", "description": "rbi_ plus 43 base64url characters" },
                        "password": { "type": "string", "minLength": 8, "maxLength": 256 }
                    }
                } } } },
                "responses": {
                    "200": { "description": "Joined and signed in", "content": { "application/json": { "schema": token_schema } } },
                    "400": { "description": "The password breaks a rule (the invitation is not used)" },
                    "401": { "description": "Not a valid invitation (reason invalid_invitation)" },
                    "404": not_found,
                    "429": { "description": "Too many attempts from this address (Retry-After)" }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests;
