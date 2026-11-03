//! The user store (`AppState.users`, also `Config.users` because every
//! credential check takes `&Config`): `<state>/users.json` (0600), at most
//! [`MAX_USERS`] users and [`MAX_CUSTOM_ROLES`] custom roles.
//!
//! A user has an id (`usr_` plus 16 random hex), a name (the sign-in name,
//! [`crate::login::valid_user`]), a display name, an optional email (never
//! logged), roles, `disabled`, a `source` (`password`, `sso`, `document`),
//! `created_at`, `last_sign_in` and a generation (signing one user out
//! everywhere rotates theirs only). Password hashes are kept apart from the
//! users in the file and never leave the store: [`User`] has no hash field,
//! so serialising one (an answer, an export) cannot carry it.
//!
//! The first admin ("user one") is the 0.3 console user: its name and
//! password stay those of the sign-in settings (`console.user`,
//! `console.password_hash`) and the environment
//! (`RUSTYBIN_CONSOLE_USER`, `RUSTYBIN_CONSOLE_PASSWORD*`), so a downgrade
//! reads them and the environment keeps seeding it on every start.
//!
//! Writes: the change is made on a copy, the copy is written (off the async
//! workers, one writer at a time, so the latest change wins), and only a
//! write that succeeded replaces what readers see: a failed write changes
//! nothing. Without a state directory (or with a file that could not be
//! read, which is never written over) the users live in memory; the
//! diagnostics say so.
//!
//! Resolution: [`Users::resolve`] answers a user's roles and permissions,
//! cached for at most [`CACHE_TTL`] per user and dropped at every change,
//! so a role change or a disable applies to the next request.
//!
//! Invitations (step 09): inviting a user creates them `pending` (and
//! disabled) with a one-time token (32 random bytes, `rbi_` plus base64url)
//! that is answered once and kept only as its SHA-256, for
//! [`INVITATION_TTL_SECS`], at most [`MAX_OPEN_INVITATIONS`] open. A reset
//! is a new invitation for a user who already joined; their old password
//! works until it is accepted. Accepting sets the password, enables a
//! pending user and moves their generation on (every older session ends).
//!
//! Guard rails, checked inside every change (`Users::commit`): the last
//! enabled user with the `admin` role is never disabled, demoted or
//! deleted; user one is never deleted (its sign-in is the instance's
//! sign-in settings: disable it instead); a custom role somebody holds is
//! never deleted.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::permission::PermSet;
use super::role::{self, BuiltIn, CustomRole, MAX_CUSTOM_ROLES};
use crate::limits::{Clock, SystemClock};

/// The file under the state directory.
pub const USERS_FILE: &str = "users.json";
/// Users an instance keeps.
pub const MAX_USERS: usize = 500;
pub const MAX_DISPLAY_NAME_CHARS: usize = 100;
pub const MAX_EMAIL_CHARS: usize = 254;
/// Roles one user holds.
pub const MAX_ROLES_PER_USER: usize = 16;
/// How long a resolution is reused.
pub const CACHE_TTL: Duration = Duration::from_secs(1);
/// The generation of a new user (and of a 0.3 token, which carries none).
pub const FIRST_GENERATION: u64 = 1;
/// Invitations open at once.
pub const MAX_OPEN_INVITATIONS: usize = 100;
/// How long an invitation link works.
pub const INVITATION_TTL_SECS: i64 = 72 * 3600;
/// What an invitation token starts with.
pub const INVITATION_PREFIX: &str = "rbi_";
/// The length of an invitation token (`rbi_` plus 43 base64url characters).
pub const INVITATION_TOKEN_CHARS: usize = 4 + 43;

/// Where a user comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Signs in with a password.
    Password,
    /// Signs in with single sign-on (step 10).
    Sso,
    /// Defined by a `rustybin.yaml` document (step 09).
    Document,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Password => "password",
            Source::Sso => "sso",
            Source::Document => "document",
        }
    }
}

fn first_generation() -> u64 {
    FIRST_GENERATION
}

/// A user. Never holds the password hash (see the module docs).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub name: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub roles: Vec<String>,
    #[serde(default)]
    pub disabled: bool,
    pub source: Source,
    pub created_at: String,
    #[serde(default)]
    pub last_sign_in: Option<String>,
    #[serde(default = "first_generation")]
    pub generation: u64,
    /// Invited and not joined yet (also disabled): the invitation sets the
    /// password and enables them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
    /// A single sign-on user's identity at the provider (step 10): the
    /// issuer, the subject and the roles its groups gave at the last
    /// sign-in. `None` until the first sign-in links it (an SSO user an
    /// admin created ahead).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sso: Option<SsoLink>,
    /// Roles an admin pinned on a single sign-on user, kept on top of the
    /// ones the provider's groups give (`roles` is their union).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned_roles: Vec<String>,
}

/// Who a single sign-on user is at the provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SsoLink {
    pub issuer: String,
    pub subject: String,
    /// The roles the provider's groups gave at the last sign-in (the role
    /// map, else the default roles).
    #[serde(default)]
    pub mapped_roles: Vec<String>,
}

/// What a verified sign-in at the provider says about the person
/// (`crate::sso`): the store links, creates or updates the user from it.
#[derive(Clone, Debug)]
pub struct SsoIdentity {
    pub issuer: String,
    pub subject: String,
    /// The sign-in name the instance gives a new user.
    pub name: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    /// The roles the groups map to (known roles only), else the default
    /// roles; may be empty.
    pub mapped_roles: Vec<String>,
}

/// The roles of `a` then those of `b` not in `a`.
pub fn union_roles(a: &[String], b: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in a.iter().chain(b) {
        if !out.contains(r) {
            out.push(r.clone());
        }
    }
    out
}

fn sso_refusal(name: &str) -> UsersError {
    UsersError::Refused(
        SSO_USER,
        format!(
            "{} signs in with single sign-on: no password and no invitation on this instance (the identity provider decides who they are)",
            crate::logging::printable(name, 64)
        ),
    )
}

impl std::fmt::Debug for User {
    // No email (never logged).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("roles", &self.roles)
            .field("disabled", &self.disabled)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl User {
    /// The user as answers show it (never a password or a hash).
    pub fn view(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "display_name": self.display_name,
            "email": self.email,
            "roles": self.roles,
            "disabled": self.disabled,
            "status": self.status(),
            "source": self.source.as_str(),
            "created_at": self.created_at,
            "last_sign_in": self.last_sign_in,
            "sso": self.sso.as_ref().map(|l| json!({
                "issuer": l.issuer,
                "subject": l.subject,
                "mapped_roles": l.mapped_roles,
            })),
            "pinned_roles": self.pinned_roles,
        })
    }

    /// Signs in with single sign-on (never a password or an invitation).
    pub fn is_sso(&self) -> bool {
        self.source == Source::Sso
    }

    /// `invited` (not joined yet), `disabled` or `active`.
    pub fn status(&self) -> &'static str {
        if self.pending {
            "invited"
        } else if self.disabled {
            "disabled"
        } else {
            "active"
        }
    }

    /// An enabled user with the built-in `admin` role (the guard rails
    /// keep at least one).
    pub fn is_enabled_admin(&self) -> bool {
        !self.disabled && !self.pending && self.roles.iter().any(|r| r == BuiltIn::Admin.name())
    }
}

/// An open invitation, kept as the SHA-256 of its token (never the token).
#[derive(Clone, Serialize, Deserialize)]
struct Invitation {
    /// Hex SHA-256 of the token.
    hash: String,
    user_id: String,
    created_at: i64,
    expires_at: i64,
    /// Who invited (a user name, or how the caller authenticated).
    #[serde(default)]
    by: String,
    /// A new password for a user who joined already.
    #[serde(default)]
    reset: bool,
}

/// An invitation as answers show it (never the token, which only the
/// answer that created it carries).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvitationInfo {
    pub created_at: i64,
    pub expires_at: i64,
    pub by: String,
    pub reset: bool,
}

impl InvitationInfo {
    pub fn view(&self) -> Value {
        json!({
            "created_at": rfc3339(self.created_at),
            "expires_at": rfc3339(self.expires_at),
            "by": self.by,
            "reset": self.reset,
        })
    }
}

/// A freshly created invitation: the token is in this value only.
#[derive(Clone)]
pub struct NewInvitation {
    pub token: String,
    pub expires_at: i64,
    pub user: User,
}

impl std::fmt::Debug for NewInvitation {
    // Never the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewInvitation")
            .field("user", &self.user)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// An accepted invitation: who joined, whether their password lives in
/// the sign-in settings (user one) rather than the store, and whether it
/// was a reset.
#[derive(Clone, Debug)]
pub struct Accepted {
    pub user: User,
    pub first_admin: bool,
    pub reset: bool,
}

pub(crate) fn rfc3339(ts: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// A fresh invitation token: `rbi_` plus 32 random bytes in base64url.
pub fn new_invitation_token() -> String {
    use base64::Engine;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    format!(
        "{INVITATION_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
    )
}

/// Is this shaped like an invitation token? (Checked before hashing.)
pub fn is_invitation_token(s: &str) -> bool {
    s.len() == INVITATION_TOKEN_CHARS
        && s.strip_prefix(INVITATION_PREFIX).is_some_and(|rest| {
            rest.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

/// The hex SHA-256 an invitation is kept as.
pub fn invitation_hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A change of a user (`PATCH /_rustybin/users/{id}`, a document entry).
#[derive(Clone, Debug, Default)]
pub struct UserPatch {
    pub display_name: Option<String>,
    /// `Some(None)` clears it.
    pub email: Option<Option<String>>,
    pub roles: Option<Vec<String>>,
    pub disabled: Option<bool>,
    /// The roles pinned on a single sign-on user (on top of its groups').
    pub pinned_roles: Option<Vec<String>>,
}

impl UserPatch {
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.email.is_none()
            && self.roles.is_none()
            && self.disabled.is_none()
            && self.pinned_roles.is_none()
    }
}

/// The reasons the guard rails give.
pub const LAST_ADMIN: &str = "last_admin";
pub const FIRST_ADMIN: &str = "first_admin";
pub const PENDING: &str = "invited";
pub const ROLE_IN_USE: &str = "role_in_use";
/// A single sign-on user: no password, no invitation, roles from the
/// provider's groups (plus pinned ones).
pub const SSO_USER: &str = "sso_user";
/// A password user has no pinned roles (their roles are set directly).
pub const NOT_SSO: &str = "not_sso";
/// Why a single sign-on was refused ([`Users::sso_sign_in`]).
pub const SSO_NAME_TAKEN: &str = "name_taken";
pub const SSO_OTHER_IDENTITY: &str = "other_identity";
pub const SSO_NOT_PROVISIONED: &str = "not_provisioned";
pub const SSO_DISABLED: &str = "disabled";
pub const SSO_NO_ROLE: &str = "no_role";

/// What `<state>/users.json` holds.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Doc {
    version: u32,
    /// The id of user one, the 0.3 console user.
    first_admin: Option<String>,
    users: Vec<User>,
    roles: Vec<CustomRole>,
    /// argon2id PHC strings by user id (users who sign in with a password
    /// of their own; user one's stays in the sign-in settings).
    password_hashes: BTreeMap<String, String>,
    /// Open invitations (the SHA-256 of their tokens).
    invitations: Vec<Invitation>,
}

/// A new user.
#[derive(Clone, Debug)]
pub struct NewUser {
    pub name: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub roles: Vec<String>,
    pub disabled: bool,
    pub source: Source,
}

impl NewUser {
    pub fn new(name: &str, roles: &[&str]) -> Self {
        Self {
            name: name.to_string(),
            display_name: None,
            email: None,
            roles: roles.iter().map(|r| r.to_string()).collect(),
            disabled: false,
            source: Source::Password,
        }
    }
}

/// Why a change was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsersError {
    Invalid(Vec<String>),
    NotFound,
    /// A name taken, a role in use.
    Conflict(String),
    /// A cap reached.
    Full(String),
    /// The file could not be written: nothing changed.
    Io(String),
    /// A guard rail ([`LAST_ADMIN`], [`FIRST_ADMIN`], [`PENDING`],
    /// [`ROLE_IN_USE`]): the reason and the message.
    Refused(&'static str, String),
}

impl std::fmt::Display for UsersError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsersError::Invalid(p) => write!(f, "invalid: {}", p.join("; ")),
            UsersError::NotFound => f.write_str("not found"),
            UsersError::Conflict(m)
            | UsersError::Full(m)
            | UsersError::Io(m)
            | UsersError::Refused(_, m) => f.write_str(m),
        }
    }
}

/// A user's resolved roles and permissions.
#[derive(Clone, Debug)]
pub struct Resolution {
    pub user: User,
    /// The user's roles that exist (an unknown one counts for nothing).
    pub roles: Vec<String>,
    pub permissions: PermSet,
}

struct Cached {
    at: Duration,
    version: u64,
    resolution: Arc<Resolution>,
}

/// The user store.
pub struct Users {
    clock: Arc<dyn Clock>,
    doc: RwLock<Doc>,
    /// One writer at a time; held across the file write.
    writing: Mutex<()>,
    /// `<state>/users.json`, when it is kept there.
    path: Option<PathBuf>,
    /// The state directory is set but its file could not be read: kept in
    /// memory so a good file is never written over.
    load_error: Option<String>,
    last_write_error: Mutex<Option<String>>,
    /// Bumped at every change: a cached resolution of an older version is
    /// never used.
    version: AtomicU64,
    cache: Mutex<HashMap<String, Cached>>,
    /// Resolutions computed (not served from the cache), for tests.
    misses: AtomicU64,
}

impl std::fmt::Debug for Users {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Users({} user(s), {})",
            self.read().users.len(),
            if self.path.is_some() {
                "persisted"
            } else {
                "in memory"
            }
        )
    }
}

impl Default for Users {
    fn default() -> Self {
        Self::in_memory()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_rfc3339(clock: &dyn Clock) -> String {
    clock
        .wall()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A fresh user id: `usr_` plus 16 random hex.
pub fn new_id() -> String {
    let mut b = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut b);
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("usr_{hex}")
}

/// Is this shaped like a user id?
pub fn is_user_id(s: &str) -> bool {
    s.len() == 20
        && s.strip_prefix("usr_").is_some_and(|h| {
            h.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        })
}

fn valid_display_name(s: &str) -> Result<(), String> {
    if s.trim().is_empty() || s.chars().count() > MAX_DISPLAY_NAME_CHARS {
        return Err(format!(
            "display name: 1 to {MAX_DISPLAY_NAME_CHARS} characters"
        ));
    }
    if s.chars().any(char::is_control) {
        return Err("display name: no control characters".into());
    }
    Ok(())
}

fn valid_email(s: &str) -> Result<(), String> {
    let ok = s.chars().count() <= MAX_EMAIL_CHARS
        && !s.chars().any(|c| c.is_control() || c.is_whitespace())
        && s.split_once('@')
            .is_some_and(|(l, d)| !l.is_empty() && d.contains('.') && !d.contains('@'));
    if ok {
        Ok(())
    } else {
        Err("email: not an email address".into())
    }
}

impl Doc {
    fn user(&self, id: &str) -> Option<&User> {
        self.users.iter().find(|u| u.id == id)
    }

    fn user_mut(&mut self, id: &str) -> Result<&mut User, UsersError> {
        self.users
            .iter_mut()
            .find(|u| u.id == id)
            .ok_or(UsersError::NotFound)
    }

    fn role_permissions(&self, name: &str) -> Option<PermSet> {
        BuiltIn::parse(name).map(BuiltIn::permissions).or_else(|| {
            self.roles
                .iter()
                .find(|r| r.name == name)
                .map(CustomRole::permission_set)
        })
    }

    fn check_roles(&self, roles: &[String]) -> Result<(), Vec<String>> {
        let mut problems = Vec::new();
        if roles.len() > MAX_ROLES_PER_USER {
            problems.push(format!("roles: at most {MAX_ROLES_PER_USER}"));
        }
        for r in roles {
            if self.role_permissions(r).is_none() {
                problems.push(format!(
                    "roles: {} is not a role",
                    crate::logging::printable(r, 64)
                ));
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }

    fn enabled_admins(&self) -> usize {
        self.users.iter().filter(|u| u.is_enabled_admin()).count()
    }

    /// Forget invitations past their expiry.
    fn prune_invitations(&mut self, now: i64) {
        self.invitations.retain(|i| i.expires_at > now);
    }

    fn name_taken(&self, name: &str, except: Option<&str>) -> bool {
        self.users
            .iter()
            .any(|u| Some(u.id.as_str()) != except && u.name.eq_ignore_ascii_case(name))
    }
}

impl Users {
    /// A store in memory (tests, no state directory).
    pub fn in_memory() -> Self {
        Self::with_clock(Arc::new(SystemClock::new()))
    }

    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            doc: RwLock::new(Doc {
                version: 1,
                ..Doc::default()
            }),
            writing: Mutex::new(()),
            path: None,
            load_error: None,
            last_write_error: Mutex::new(None),
            version: AtomicU64::new(0),
            cache: Mutex::new(HashMap::new()),
            misses: AtomicU64::new(0),
        }
    }

    /// The store of an instance: `<state>/users.json` when there is a state
    /// directory, else in memory.
    pub fn persistent(state_dir: Option<&Path>) -> Self {
        Self::persistent_with_clock(state_dir, Arc::new(SystemClock::new()))
    }

    pub fn persistent_with_clock(state_dir: Option<&Path>, clock: Arc<dyn Clock>) -> Self {
        let mut store = Self::with_clock(clock);
        let Some(dir) = state_dir else { return store };
        if let Err(e) = crate::settings::create_private_dir(dir) {
            tracing::warn!(target: "rustybin::auth", error = %e, "the state directory could not be created: users are kept in memory");
            store.load_error = Some(e);
            return store;
        }
        let path = dir.join(USERS_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Doc>(&text) {
                Ok(mut doc) => {
                    doc.users.truncate(MAX_USERS);
                    doc.roles.truncate(MAX_CUSTOM_ROLES);
                    let ids: std::collections::BTreeSet<String> =
                        doc.users.iter().map(|u| u.id.clone()).collect();
                    doc.password_hashes.retain(|id, _| ids.contains(id));
                    doc.invitations.retain(|i| ids.contains(&i.user_id));
                    doc.invitations.truncate(MAX_OPEN_INVITATIONS);
                    *store.doc.get_mut().unwrap_or_else(|e| e.into_inner()) = doc;
                    store.path = Some(path);
                }
                Err(e) => {
                    let msg = format!("{} could not be read: {e}", path.display());
                    tracing::warn!(target: "rustybin::auth", "{msg}: users are kept in memory and the file is left as it is");
                    store.load_error = Some(msg);
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => store.path = Some(path),
            Err(e) => {
                let msg = format!("{} could not be read: {e}", path.display());
                tracing::warn!(target: "rustybin::auth", "{msg}: users are kept in memory");
                store.load_error = Some(msg);
            }
        }
        store
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Doc> {
        self.doc.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Change a copy, write it, then publish it. A refused change or a
    /// failed write leaves the store as it was.
    fn commit<T>(
        &self,
        change: impl FnOnce(&mut Doc) -> Result<T, UsersError>,
    ) -> Result<T, UsersError> {
        let _writer = lock(&self.writing);
        let mut next = self.read().clone();
        let admins_before = next.enabled_admins();
        let out = change(&mut next)?;
        // The last enabled admin stays, whatever the change.
        if admins_before > 0 && next.enabled_admins() == 0 {
            return Err(UsersError::Refused(
                LAST_ADMIN,
                "the last enabled user with the admin role cannot be disabled, demoted or deleted: give another user the admin role first".into(),
            ));
        }
        if let Some(path) = &self.path {
            let text = serde_json::to_vec_pretty(&next)
                .map_err(|e| UsersError::Io(format!("users not saved: {e}")))?;
            if let Err(e) =
                crate::mocks::store::off_runtime(|| crate::settings::write_private(path, &text))
            {
                tracing::warn!(target: "rustybin::auth", error = %e, "users not saved");
                *lock(&self.last_write_error) = Some(e.clone());
                return Err(UsersError::Io(format!("users not saved: {e}")));
            }
            *lock(&self.last_write_error) = None;
        }
        *self.doc.write().unwrap_or_else(|e| e.into_inner()) = next;
        self.version.fetch_add(1, Ordering::Relaxed);
        lock(&self.cache).clear();
        Ok(out)
    }

    // ── Reads ───────────────────────────────────────────────────────

    pub fn len(&self) -> usize {
        self.read().users.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn list(&self) -> Vec<User> {
        self.read().users.clone()
    }

    pub fn get(&self, id: &str) -> Option<User> {
        self.read().user(id).cloned()
    }

    /// The user of a sign-in name (names are unique ignoring case; the
    /// sign-in compares exactly).
    pub fn find_by_name(&self, name: &str) -> Option<User> {
        self.read().users.iter().find(|u| u.name == name).cloned()
    }

    /// User one: the 0.3 console user, the first admin.
    pub fn first_admin(&self) -> Option<User> {
        let doc = self.read();
        doc.first_admin
            .as_deref()
            .and_then(|id| doc.user(id))
            .cloned()
    }

    pub fn is_first_admin(&self, id: &str) -> bool {
        self.read().first_admin.as_deref() == Some(id)
    }

    /// The password hash of a user who signs in with their own (never
    /// user one's, which the sign-in settings keep).
    pub(crate) fn password_hash(&self, id: &str) -> Option<String> {
        self.read().password_hashes.get(id).cloned()
    }

    pub fn custom_roles(&self) -> Vec<CustomRole> {
        self.read().roles.clone()
    }

    /// The permissions of a role, built-in or custom; `None` when there is
    /// no such role.
    pub fn role_permissions(&self, name: &str) -> Option<PermSet> {
        self.read().role_permissions(name)
    }

    /// Every role as answers show it: the built-in ones, then the custom.
    pub fn roles_view(&self) -> Vec<Value> {
        let mut out: Vec<Value> = BuiltIn::ALL
            .iter()
            .map(|r| role::view(r.name(), r.description(), r.permissions(), true))
            .collect();
        for r in self.custom_roles() {
            out.push(role::view(
                &r.name,
                &r.description,
                r.permission_set(),
                false,
            ));
        }
        out
    }

    pub fn persisted(&self) -> bool {
        self.path.is_some()
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn write_error(&self) -> Option<String> {
        lock(&self.last_write_error).clone()
    }

    // ── Resolution ──────────────────────────────────────────────────

    /// A user's roles and permissions, from the cache when it was resolved
    /// less than [`CACHE_TTL`] ago and nothing changed since. `None` when
    /// there is no such user.
    pub fn resolve(&self, id: &str) -> Option<Arc<Resolution>> {
        let now = self.clock.mono();
        let version = self.version.load(Ordering::Relaxed);
        if let Some(c) = lock(&self.cache).get(id) {
            if c.version == version && now.saturating_sub(c.at) < CACHE_TTL {
                return Some(c.resolution.clone());
            }
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let resolution = {
            let doc = self.read();
            let user = doc.user(id)?.clone();
            let mut roles = Vec::new();
            let mut permissions = PermSet::EMPTY;
            for r in &user.roles {
                match doc.role_permissions(r) {
                    Some(p) => {
                        roles.push(r.clone());
                        permissions = permissions.union(p);
                    }
                    None => super::note_unknown("role", r),
                }
            }
            Arc::new(Resolution {
                user,
                roles,
                permissions,
            })
        };
        let mut cache = lock(&self.cache);
        if cache.len() >= MAX_USERS {
            cache.clear();
        }
        cache.insert(
            id.to_string(),
            Cached {
                at: now,
                version,
                resolution: resolution.clone(),
            },
        );
        Some(resolution)
    }

    /// Resolutions computed so far (the rest came from the cache).
    pub fn cache_misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    // ── Changes ─────────────────────────────────────────────────────

    /// Make sure user one exists with `name` (startup, every start: the
    /// 0.3 console user becomes the first admin, and its name follows the
    /// sign-in settings). Answers whether it was created. When the file
    /// cannot be written the user is kept in memory (the instance still
    /// signs in), with a warning and a diagnostics finding.
    pub fn ensure_first_admin(&self, name: &str) -> bool {
        if let Some(first) = self.first_admin() {
            if first.name != name {
                if let Err(e) = self.rename_first(name) {
                    tracing::warn!(target: "rustybin::auth", error = %e, "the first admin's name could not follow the sign-in settings");
                }
            }
            return false;
        }
        let clock = self.clock.clone();
        let create = |doc: &mut Doc| -> Result<String, UsersError> {
            let user = User {
                id: new_id(),
                name: name.to_string(),
                display_name: name.to_string(),
                email: None,
                roles: vec![BuiltIn::Admin.name().to_string()],
                disabled: false,
                source: Source::Password,
                created_at: now_rfc3339(clock.as_ref()),
                last_sign_in: None,
                generation: FIRST_GENERATION,
                pending: false,
                sso: None,
                pinned_roles: Vec::new(),
            };
            // A user of that name from a newer version: it becomes user one.
            if let Some(existing) = doc.users.iter().find(|u| u.name == name) {
                let id = existing.id.clone();
                doc.first_admin = Some(id.clone());
                return Ok(id);
            }
            let id = user.id.clone();
            doc.users.insert(0, user);
            doc.first_admin = Some(id.clone());
            Ok(id)
        };
        match self.commit(create) {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(target: "rustybin::auth", error = %e, "the first admin is kept in memory only");
                let mut doc = self.doc.write().unwrap_or_else(|e| e.into_inner());
                let _ = create(&mut doc);
                drop(doc);
                self.version.fetch_add(1, Ordering::Relaxed);
                lock(&self.cache).clear();
                true
            }
        }
    }

    /// User one's name follows the sign-in settings' `console.user`.
    pub fn rename_first(&self, name: &str) -> Result<(), UsersError> {
        self.commit(|doc| {
            let Some(id) = doc.first_admin.clone() else {
                return Ok(());
            };
            if doc.name_taken(name, Some(&id)) {
                return Err(UsersError::Conflict(format!(
                    "another user signs in as {}",
                    crate::logging::printable(name, 64)
                )));
            }
            let user = doc.user_mut(&id)?;
            if user.display_name == user.name {
                user.display_name = name.to_string();
            }
            user.name = name.to_string();
            Ok(())
        })
    }

    /// Add a user with a password hash of their own (or none: they cannot
    /// sign in until an invitation sets one).
    pub fn add(&self, new: NewUser, password_hash: Option<String>) -> Result<User, UsersError> {
        Self::check_new(&new, password_hash.as_deref())?;
        if new.source == Source::Sso && password_hash.is_some() {
            return Err(sso_refusal(&new.name));
        }
        let created_at = now_rfc3339(self.clock.as_ref());
        self.commit(|doc| Self::insert(doc, &new, password_hash, created_at, false))
    }

    fn check_new(new: &NewUser, password_hash: Option<&str>) -> Result<(), UsersError> {
        let mut problems = Vec::new();
        if let Err(e) = crate::login::valid_user(&new.name) {
            problems.push(format!("name: {e}"));
        }
        if let Some(d) = &new.display_name {
            if let Err(e) = valid_display_name(d) {
                problems.push(e);
            }
        }
        if let Some(e) = &new.email {
            if let Err(e) = valid_email(e) {
                problems.push(e);
            }
        }
        if password_hash.is_some_and(|h| !crate::login::valid_hash(h)) {
            problems.push("password: not an argon2 hash".into());
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(UsersError::Invalid(problems))
        }
    }

    fn insert(
        doc: &mut Doc,
        new: &NewUser,
        password_hash: Option<String>,
        created_at: String,
        pending: bool,
    ) -> Result<User, UsersError> {
        doc.check_roles(&new.roles).map_err(UsersError::Invalid)?;
        if doc.name_taken(&new.name, None) {
            return Err(UsersError::Conflict(format!(
                "a user named {} exists",
                new.name
            )));
        }
        if doc.users.len() >= MAX_USERS {
            return Err(UsersError::Full(format!(
                "an instance keeps at most {MAX_USERS} users"
            )));
        }
        let user = User {
            id: new_id(),
            display_name: new.display_name.clone().unwrap_or_else(|| new.name.clone()),
            name: new.name.clone(),
            email: new.email.clone(),
            roles: new.roles.clone(),
            disabled: new.disabled || pending,
            source: new.source,
            created_at,
            last_sign_in: None,
            generation: FIRST_GENERATION,
            pending,
            sso: None,
            pinned_roles: if new.source == Source::Sso {
                new.roles.clone()
            } else {
                Vec::new()
            },
        };
        if let Some(h) = password_hash {
            doc.password_hashes.insert(user.id.clone(), h);
        }
        doc.users.push(user.clone());
        Ok(user)
    }

    /// Add a user invited, without a link (a configuration file at startup
    /// has nobody to answer one to): `invite` sends one later.
    pub fn add_invited(&self, new: NewUser) -> Result<User, UsersError> {
        Self::check_new(&new, None)?;
        let created_at = now_rfc3339(self.clock.as_ref());
        self.commit(|doc| Self::insert(doc, &new, None, created_at, true))
    }

    /// Add a single sign-on user ahead of their first sign-in (an admin
    /// with `sso.provision` off, a document): no password, no invitation;
    /// `new.roles` are pinned (the provider's groups add theirs at every
    /// sign-in). The first sign-in links them by name.
    pub fn add_sso(&self, new: NewUser) -> Result<User, UsersError> {
        let new = NewUser {
            source: Source::Sso,
            ..new
        };
        Self::check_new(&new, None)?;
        if new.roles.is_empty() {
            return Err(UsersError::Invalid(vec![
                "roles: a single sign-on user created ahead needs at least one role until the provider gives one".into(),
            ]));
        }
        let created_at = now_rfc3339(self.clock.as_ref());
        self.commit(|doc| Self::insert(doc, &new, None, created_at, false))
    }

    /// A verified single sign-on (`crate::sso`): the user linked to the
    /// identity (issuer and subject), else a single sign-on user of that
    /// name not linked to this issuer yet (created ahead, or linked to an
    /// issuer the instance no longer uses), else a new user when
    /// `provision` is on. Their roles become the mapped roles plus the
    /// pinned ones, their display name and email follow the provider, and
    /// the sign-in time is noted. Refused ([`UsersError::Refused`]): a
    /// password user has the name ([`SSO_NAME_TAKEN`]: an identity never
    /// takes over a password account), another identity of the same issuer
    /// is linked to it ([`SSO_OTHER_IDENTITY`]), nobody to link and
    /// provisioning off ([`SSO_NOT_PROVISIONED`]), the user is disabled
    /// ([`SSO_DISABLED`]), no role at all ([`SSO_NO_ROLE`]), and the guard
    /// rail ([`LAST_ADMIN`]: the provider dropped the last admin's group).
    /// A refusal changes nothing.
    pub fn sso_sign_in(&self, ident: &SsoIdentity, provision: bool) -> Result<User, UsersError> {
        let now = now_rfc3339(self.clock.as_ref());
        let display_name = ident
            .display_name
            .clone()
            .filter(|d| valid_display_name(d).is_ok());
        let email = ident.email.clone().filter(|e| valid_email(e).is_ok());
        let printable = crate::logging::printable(&ident.name, 64).to_string();
        self.commit(|doc| {
            let linked = doc.users.iter().position(|u| {
                u.sso
                    .as_ref()
                    .is_some_and(|l| l.issuer == ident.issuer && l.subject == ident.subject)
            });
            let pos = match linked {
                Some(p) => p,
                None => match doc
                    .users
                    .iter()
                    .position(|u| u.name.eq_ignore_ascii_case(&ident.name))
                {
                    Some(p) => {
                        let u = &doc.users[p];
                        if !u.is_sso() {
                            return Err(UsersError::Refused(
                                SSO_NAME_TAKEN,
                                format!("{printable} is a user who signs in with a password here: a single sign-on never takes over that account (an admin deletes it first)"),
                            ));
                        }
                        if u.sso.as_ref().is_some_and(|l| l.issuer == ident.issuer) {
                            return Err(UsersError::Refused(
                                SSO_OTHER_IDENTITY,
                                format!("another identity of the provider signs in as {printable}"),
                            ));
                        }
                        p
                    }
                    None => {
                        if !provision {
                            return Err(UsersError::Refused(
                                SSO_NOT_PROVISIONED,
                                format!("{printable} has no user on this instance and sso.provision is off: an admin adds them first"),
                            ));
                        }
                        let new = NewUser {
                            name: ident.name.clone(),
                            display_name: display_name.clone(),
                            email: email.clone(),
                            roles: Vec::new(),
                            disabled: false,
                            source: Source::Sso,
                        };
                        Self::check_new(&new, None)?;
                        Self::insert(doc, &new, None, now.clone(), false)?;
                        doc.users.len() - 1
                    }
                },
            };
            let user = &mut doc.users[pos];
            if user.disabled {
                return Err(UsersError::Refused(
                    SSO_DISABLED,
                    format!("{} is disabled on this instance", user.name),
                ));
            }
            let roles = union_roles(&ident.mapped_roles, &user.pinned_roles);
            if roles.is_empty() {
                return Err(UsersError::Refused(
                    SSO_NO_ROLE,
                    format!("none of {}'s groups maps to a role (sso.role_map) and sso.default_roles is empty", user.name),
                ));
            }
            user.roles = roles;
            user.sso = Some(SsoLink {
                issuer: ident.issuer.clone(),
                subject: ident.subject.clone(),
                mapped_roles: ident.mapped_roles.clone(),
            });
            if let Some(d) = &display_name {
                user.display_name = d.clone();
            }
            if email.is_some() {
                user.email = email.clone();
            }
            user.last_sign_in = Some(now.clone());
            let out = user.clone();
            doc.check_roles(&out.roles).map_err(UsersError::Invalid)?;
            Ok(out)
        })
    }

    /// A user by id, else by sign-in name (the API, the CLI and the tools
    /// take either).
    pub fn find(&self, id_or_name: &str) -> Option<User> {
        let doc = self.read();
        doc.user(id_or_name)
            .or_else(|| doc.users.iter().find(|u| u.name == id_or_name))
            .cloned()
    }

    /// How many users hold `role`.
    pub fn holders(&self, role: &str) -> usize {
        self.read()
            .users
            .iter()
            .filter(|u| u.roles.iter().any(|r| r == role))
            .count()
    }

    /// Enabled users with the `admin` role.
    pub fn enabled_admins(&self) -> usize {
        self.read().enabled_admins()
    }

    // ── Invitations ─────────────────────────────────────────────────

    fn now_ts(&self) -> i64 {
        self.clock.wall().timestamp()
    }

    /// Open invitations (expired ones do not count).
    pub fn open_invitations(&self) -> usize {
        let now = self.now_ts();
        self.read()
            .invitations
            .iter()
            .filter(|i| i.expires_at > now)
            .count()
    }

    /// A user's open invitation, if they have one.
    pub fn invitation(&self, user_id: &str) -> Option<InvitationInfo> {
        let now = self.now_ts();
        self.read()
            .invitations
            .iter()
            .find(|i| i.user_id == user_id && i.expires_at > now)
            .map(|i| InvitationInfo {
                created_at: i.created_at,
                expires_at: i.expires_at,
                by: i.by.clone(),
                reset: i.reset,
            })
    }

    fn open_invitation(
        doc: &mut Doc,
        user_id: &str,
        by: &str,
        reset: bool,
        now: i64,
    ) -> Result<(String, i64), UsersError> {
        doc.prune_invitations(now);
        doc.invitations.retain(|i| i.user_id != user_id);
        if doc.invitations.len() >= MAX_OPEN_INVITATIONS {
            return Err(UsersError::Full(format!(
                "at most {MAX_OPEN_INVITATIONS} invitations are open at once: wait for some to be accepted or to expire"
            )));
        }
        let token = new_invitation_token();
        let expires_at = now + INVITATION_TTL_SECS;
        doc.invitations.push(Invitation {
            hash: invitation_hash(&token),
            user_id: user_id.to_string(),
            created_at: now,
            expires_at,
            by: crate::logging::printable(by, 64).to_string(),
            reset,
        });
        Ok((token, expires_at))
    }

    /// Invite a new user: created invited (and disabled) with a one-time
    /// invitation. The token is in the answer only.
    pub fn invite_new(&self, new: NewUser, by: &str) -> Result<NewInvitation, UsersError> {
        Self::check_new(&new, None)?;
        if new.source == Source::Sso {
            return Err(sso_refusal(&new.name));
        }
        let created_at = now_rfc3339(self.clock.as_ref());
        let now = self.now_ts();
        self.commit(|doc| {
            // The cap first, so a refused invitation leaves no user behind.
            doc.prune_invitations(now);
            if doc.invitations.len() >= MAX_OPEN_INVITATIONS {
                return Err(UsersError::Full(format!(
                    "at most {MAX_OPEN_INVITATIONS} invitations are open at once: wait for some to be accepted or to expire"
                )));
            }
            let user = Self::insert(doc, &new, None, created_at, true)?;
            let (token, expires_at) = Self::open_invitation(doc, &user.id, by, false, now)?;
            Ok(NewInvitation {
                token,
                expires_at,
                user,
            })
        })
    }

    /// A new invitation for an existing user: a reset for a user who joined
    /// (their old password works until it is accepted), a fresh link for
    /// one still invited. Replaces their open invitation. Refused for a
    /// disabled user who joined (enable them first).
    pub fn invite(&self, id: &str, by: &str) -> Result<NewInvitation, UsersError> {
        let now = self.now_ts();
        self.commit(|doc| {
            let user = doc.user(id).cloned().ok_or(UsersError::NotFound)?;
            if user.is_sso() {
                return Err(sso_refusal(&user.name));
            }
            if user.disabled && !user.pending {
                return Err(UsersError::Refused(
                    "disabled",
                    format!(
                        "{} is disabled: enable the user before sending a new invitation",
                        user.name
                    ),
                ));
            }
            let (token, expires_at) = Self::open_invitation(doc, id, by, !user.pending, now)?;
            Ok(NewInvitation {
                token,
                expires_at,
                user,
            })
        })
    }

    /// The user an invitation token would let join, if it is open (the
    /// join route checks before it hashes a password).
    pub fn invitation_user(&self, token: &str) -> Option<User> {
        if !is_invitation_token(token) {
            return None;
        }
        let hash = invitation_hash(token);
        let now = self.now_ts();
        let doc = self.read();
        let inv = doc
            .invitations
            .iter()
            .find(|i| i.hash == hash && i.expires_at > now)?;
        let user = doc.user(&inv.user_id)?;
        ((!user.disabled || user.pending) && !user.is_sso()).then(|| user.clone())
    }

    /// Accept an invitation: one use. The password hash goes to the store
    /// (user one's goes to the sign-in settings, which the caller writes:
    /// `Accepted::first_admin`); a pending user is enabled; the user's
    /// generation moves on (every older session ends). `NotFound` for a
    /// token that is not open (unknown, used, replaced, expired, or a user
    /// disabled since).
    pub fn accept_invitation(
        &self,
        token: &str,
        password_hash: String,
    ) -> Result<Accepted, UsersError> {
        if !is_invitation_token(token) || !crate::login::valid_hash(&password_hash) {
            return Err(UsersError::NotFound);
        }
        let hash = invitation_hash(token);
        let now = self.now_ts();
        self.commit(|doc| {
            doc.prune_invitations(now);
            let pos = doc
                .invitations
                .iter()
                .position(|i| i.hash == hash)
                .ok_or(UsersError::NotFound)?;
            let inv = doc.invitations.remove(pos);
            let first_admin = doc.first_admin.as_deref() == Some(inv.user_id.as_str());
            let user = doc.user_mut(&inv.user_id)?;
            if (user.disabled && !user.pending) || user.is_sso() {
                return Err(UsersError::NotFound);
            }
            if user.pending {
                user.pending = false;
                user.disabled = false;
            }
            user.generation = user.generation.saturating_add(1);
            let user = user.clone();
            if !first_admin {
                doc.password_hashes.insert(user.id.clone(), password_hash);
            }
            Ok(Accepted {
                user,
                first_admin,
                reset: inv.reset,
            })
        })
    }

    /// A user's own new password (`/me/password`; never user one's, which
    /// the sign-in settings keep): stored, and every session of theirs
    /// ends (their generation moves on). Answers the user.
    pub fn set_password(&self, id: &str, password_hash: String) -> Result<User, UsersError> {
        if !crate::login::valid_hash(&password_hash) {
            return Err(UsersError::Invalid(vec![
                "password: not an argon2 hash".into()
            ]));
        }
        self.commit(|doc| {
            if doc.first_admin.as_deref() == Some(id) {
                return Err(UsersError::Refused(
                    FIRST_ADMIN,
                    "the first admin's password is the instance's sign-in setting".into(),
                ));
            }
            let user = doc.user_mut(id)?;
            if user.is_sso() {
                return Err(sso_refusal(&user.name));
            }
            user.generation = user.generation.saturating_add(1);
            let user = user.clone();
            doc.password_hashes.insert(id.to_string(), password_hash);
            Ok(user)
        })
    }

    /// Change a user: display name, email, roles, disabled. The guard rails
    /// apply (the last enabled admin stays; an invited user joins through
    /// the invitation, never by being enabled). Disabling ends the user's
    /// sessions (their generation moves on) and drops their invitation.
    pub fn update(&self, id: &str, patch: UserPatch) -> Result<User, UsersError> {
        let mut problems = Vec::new();
        if let Some(d) = &patch.display_name {
            if let Err(e) = valid_display_name(d) {
                problems.push(e);
            }
        }
        if let Some(Some(e)) = &patch.email {
            if let Err(e) = valid_email(e) {
                problems.push(e);
            }
        }
        if !problems.is_empty() {
            return Err(UsersError::Invalid(problems));
        }
        self.commit(|doc| {
            if let Some(roles) = &patch.roles {
                doc.check_roles(roles).map_err(UsersError::Invalid)?;
            }
            if let Some(pinned) = &patch.pinned_roles {
                doc.check_roles(pinned).map_err(UsersError::Invalid)?;
            }
            let user = doc.user_mut(id)?;
            if user.is_sso() && patch.roles.as_ref().is_some_and(|r| *r != user.roles) {
                return Err(UsersError::Refused(
                    SSO_USER,
                    format!(
                        "{} signs in with single sign-on: the provider's groups give their roles at every sign-in (the sso.role_map setting); pin extra roles with pinned_roles",
                        user.name
                    ),
                ));
            }
            if !user.is_sso() && patch.pinned_roles.is_some() {
                return Err(UsersError::Refused(
                    NOT_SSO,
                    format!(
                        "{} signs in with a password: change their roles instead (pinned roles are for single sign-on users)",
                        user.name
                    ),
                ));
            }
            if let Some(pinned) = &patch.pinned_roles {
                let mapped = user
                    .sso
                    .as_ref()
                    .map(|l| l.mapped_roles.clone())
                    .unwrap_or_default();
                let roles = union_roles(&mapped, pinned);
                if roles.is_empty() {
                    return Err(UsersError::Invalid(vec![
                        "pinned_roles: the provider gave this user no role yet: pin at least one".into(),
                    ]));
                }
                if roles.len() > MAX_ROLES_PER_USER {
                    return Err(UsersError::Invalid(vec![format!(
                        "roles: at most {MAX_ROLES_PER_USER}"
                    )]));
                }
                user.pinned_roles = pinned.clone();
                user.roles = roles;
            }
            if patch.disabled == Some(false) && user.pending {
                return Err(UsersError::Refused(
                    PENDING,
                    format!(
                        "{} has not accepted the invitation yet: they join through it (a new link: POST /_rustybin/users/{}/invite)",
                        user.name, user.id
                    ),
                ));
            }
            if let Some(d) = &patch.display_name {
                user.display_name = d.clone();
            }
            if let Some(e) = &patch.email {
                user.email = e.clone();
            }
            if let Some(r) = &patch.roles {
                user.roles = r.clone();
            }
            let mut dropped = false;
            if let Some(disabled) = patch.disabled {
                if disabled && !user.disabled {
                    user.generation = user.generation.saturating_add(1);
                }
                if disabled {
                    // An invited user disabled is a disabled user: the
                    // invitation goes, and a new link (once enabled) is a
                    // reset.
                    dropped = true;
                    user.pending = false;
                }
                user.disabled = disabled;
            }
            let user = user.clone();
            if dropped {
                doc.invitations.retain(|i| i.user_id != id);
            }
            Ok(user)
        })
    }

    /// Delete a user (never user one: disable it instead), with their
    /// password and invitation. The last enabled admin stays.
    pub fn delete(&self, id: &str) -> Result<User, UsersError> {
        self.commit(|doc| {
            if doc.first_admin.as_deref() == Some(id) {
                return Err(UsersError::Refused(
                    FIRST_ADMIN,
                    "the first admin signs in with the instance's sign-in settings and cannot be deleted: disable it instead".into(),
                ));
            }
            let pos = doc
                .users
                .iter()
                .position(|u| u.id == id)
                .ok_or(UsersError::NotFound)?;
            let user = doc.users.remove(pos);
            doc.password_hashes.remove(id);
            doc.invitations.retain(|i| i.user_id != id);
            Ok(user)
        })
    }

    /// Replace a user's roles.
    pub fn set_roles(&self, id: &str, roles: Vec<String>) -> Result<User, UsersError> {
        self.commit(|doc| {
            doc.check_roles(&roles).map_err(UsersError::Invalid)?;
            let user = doc.user_mut(id)?;
            user.roles = roles;
            Ok(user.clone())
        })
    }

    /// Disable or enable a user. A disabled user's tokens are refused at
    /// the next request and their sign-in answers as a wrong password;
    /// disabling also moves their generation on, so enabling them again
    /// does not bring an old token back.
    pub fn set_disabled(&self, id: &str, disabled: bool) -> Result<User, UsersError> {
        self.update(
            id,
            UserPatch {
                disabled: Some(disabled),
                ..UserPatch::default()
            },
        )
    }

    pub fn set_display_name(&self, id: &str, display_name: &str) -> Result<User, UsersError> {
        valid_display_name(display_name).map_err(|e| UsersError::Invalid(vec![e]))?;
        self.commit(|doc| {
            let user = doc.user_mut(id)?;
            user.display_name = display_name.to_string();
            Ok(user.clone())
        })
    }

    /// Sign one user out everywhere: their generation moves on, every
    /// token of theirs carries the old one. Answers the new generation.
    pub fn rotate_generation(&self, id: &str) -> Result<u64, UsersError> {
        self.commit(|doc| {
            let user = doc.user_mut(id)?;
            user.generation = user.generation.saturating_add(1);
            Ok(user.generation)
        })
    }

    /// Note a sign-in.
    pub fn note_sign_in(&self, id: &str) -> Result<(), UsersError> {
        let at = now_rfc3339(self.clock.as_ref());
        self.commit(|doc| {
            doc.user_mut(id)?.last_sign_in = Some(at);
            Ok(())
        })
    }

    /// Save a custom role (new, or a change of one).
    pub fn save_role(&self, role: CustomRole) -> Result<(), UsersError> {
        role::validate_custom(&role).map_err(UsersError::Invalid)?;
        self.commit(|doc| {
            let full = doc.roles.len() >= MAX_CUSTOM_ROLES;
            match doc.roles.iter_mut().find(|r| r.name == role.name) {
                Some(existing) => *existing = role,
                None if full => {
                    return Err(UsersError::Full(format!(
                        "an instance keeps at most {MAX_CUSTOM_ROLES} custom roles"
                    )))
                }
                None => doc.roles.push(role),
            }
            Ok(())
        })
    }

    /// Delete a custom role nobody holds.
    pub fn delete_role(&self, name: &str) -> Result<(), UsersError> {
        if BuiltIn::parse(name).is_some() {
            return Err(UsersError::Invalid(vec![format!(
                "{name} is a built-in role, which cannot be deleted"
            )]));
        }
        self.commit(|doc| {
            if !doc.roles.iter().any(|r| r.name == name) {
                return Err(UsersError::NotFound);
            }
            let holders = doc
                .users
                .iter()
                .filter(|u| u.roles.iter().any(|r| r == name))
                .count();
            if holders > 0 {
                return Err(UsersError::Refused(
                    ROLE_IN_USE,
                    format!("the role is held by {holders} user(s): give them another role first"),
                ));
            }
            doc.roles.retain(|r| r.name != name);
            Ok(())
        })
    }
}
