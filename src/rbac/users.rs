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
            "source": self.source.as_str(),
            "created_at": self.created_at,
            "last_sign_in": self.last_sign_in,
        })
    }
}

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
}

impl std::fmt::Display for UsersError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsersError::Invalid(p) => write!(f, "invalid: {}", p.join("; ")),
            UsersError::NotFound => f.write_str("not found"),
            UsersError::Conflict(m) | UsersError::Full(m) | UsersError::Io(m) => f.write_str(m),
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
        let out = change(&mut next)?;
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

    /// Add a user (step 09's invitations and documents build on this).
    pub fn add(&self, new: NewUser, password_hash: Option<String>) -> Result<User, UsersError> {
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
        if password_hash
            .as_deref()
            .is_some_and(|h| !crate::login::valid_hash(h))
        {
            problems.push("password: not an argon2 hash".into());
        }
        if !problems.is_empty() {
            return Err(UsersError::Invalid(problems));
        }
        let created_at = now_rfc3339(self.clock.as_ref());
        self.commit(|doc| {
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
                disabled: new.disabled,
                source: new.source,
                created_at,
                last_sign_in: None,
                generation: FIRST_GENERATION,
            };
            if let Some(h) = password_hash {
                doc.password_hashes.insert(user.id.clone(), h);
            }
            doc.users.push(user.clone());
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
        self.commit(|doc| {
            let user = doc.user_mut(id)?;
            if disabled && !user.disabled {
                user.generation = user.generation.saturating_add(1);
            }
            user.disabled = disabled;
            Ok(user.clone())
        })
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
            if let Some(u) = doc.users.iter().find(|u| u.roles.iter().any(|r| r == name)) {
                return Err(UsersError::Conflict(format!(
                    "the role is held by {}",
                    u.name
                )));
            }
            doc.roles.retain(|r| r.name != name);
            Ok(())
        })
    }
}
