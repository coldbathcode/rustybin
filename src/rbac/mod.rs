//! Users, roles and permissions (access control, the model).
//!
//! - [`permission`]: the named permissions ([`Permission`], [`PermSet`]).
//! - [`role`]: the five built-in roles and custom roles.
//! - [`users`]: the user store (`AppState.users`, `<state>/users.json`).
//!
//! Resolution: a control-plane credential resolves to a [`Resolved`] (who,
//! the roles, the permissions) at every request. The admin token is every
//! permission (break glass, never a user); an open control plane is every
//! permission; a sign-in token resolves from its user (`sub`, the user id,
//! with the instance's `rb_gen` and the user's `rb_ugen`; a 0.3 token
//! without a user id is user one); a control JWT resolves from its `scope`
//! (the old scopes as roles: `admin` the admin role, `console` the viewer,
//! `inspector` `traffic.read` alone, `harness` `harness.read` and
//! `harness.run`) plus the optional `rb_roles` and `rb_perms` claims.
//! Unknown role and permission names are ignored and logged once, never an
//! error.
//!
//! Enforcement ([`need`]): every control route and management tool
//! declares a [`Need`]; `control_auth::check_route` checks a route's need
//! against the caller's permissions before the handler runs, and a
//! handler whose need depends on the body adds [`require`]. [`grant_of`]
//! still names the 0.3 scopes a set amounts to (answers and logs).

pub mod api;
pub mod need;
pub mod permission;
pub mod role;
pub mod service;
pub mod users;

use std::collections::BTreeSet;
use std::sync::Mutex;

use serde_json::{json, Value};

pub use need::{forbidden, refuse, require, Need};
pub use permission::{Group, PermSet, Permission};
pub use role::{BuiltIn, CustomRole};
pub use users::{NewUser, Source, SsoIdentity, SsoLink, User, Users, UsersError};

use crate::control_auth::Grant;

/// Who a control-plane caller is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    /// The control plane is open (`RUSTYBIN_CONTROL_AUTH=open`) and has no
    /// admin token, outside public mode: every permission, as before.
    Open,
    /// No credential on an open control plane that has an admin token
    /// (`admin_token: true`) or runs in public mode: the reads of
    /// [`need::ANONYMOUS`].
    Anonymous { admin_token: bool },
    /// An API key in shared mode (or a control JWT bound to one): the reads
    /// of [`need::ANONYMOUS`], inside the key layer's fence.
    Key { id: String },
    /// `RUSTYBIN_ADMIN_TOKEN` itself.
    AdminToken,
    /// A user of the store (a sign-in token).
    User {
        id: String,
        name: String,
        display_name: String,
    },
    /// A control JWT a platform issued, by its `sub`.
    Token { sub: Option<String> },
}

/// A caller's roles and permissions, resolved for one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub principal: Principal,
    /// The roles the permissions come from (known ones only).
    pub roles: Vec<String>,
    pub permissions: PermSet,
}

impl Resolved {
    /// The admin token: every permission.
    pub fn admin_token() -> Self {
        Self {
            principal: Principal::AdminToken,
            roles: vec![BuiltIn::Admin.name().to_string()],
            permissions: PermSet::all(),
        }
    }

    /// An open control plane: every permission, as before.
    pub fn open() -> Self {
        Self {
            principal: Principal::Open,
            roles: vec![BuiltIn::Admin.name().to_string()],
            permissions: PermSet::all(),
        }
    }

    /// A caller without a credential on an open control plane: every
    /// permission without an admin token outside public mode, else the
    /// anonymous reads.
    pub fn anonymous(config: &crate::config::Config) -> Self {
        if config.admin_token.is_none() && !config.public_mode {
            return Self::open();
        }
        Self {
            principal: Principal::Anonymous {
                admin_token: config.admin_token.is_some(),
            },
            roles: Vec::new(),
            permissions: need::anonymous(),
        }
    }

    /// An API key in shared mode.
    pub fn key(id: &str) -> Self {
        Self {
            principal: Principal::Key { id: id.to_string() },
            roles: Vec::new(),
            permissions: need::anonymous(),
        }
    }

    pub fn has(&self, p: Permission) -> bool {
        self.permissions.has(p)
    }

    /// The 0.3 grant these permissions amount to.
    pub fn grant(&self) -> Grant {
        grant_of(self.permissions)
    }

    /// The user of a sign-in token, if this is one.
    pub fn user_id(&self) -> Option<&str> {
        match &self.principal {
            Principal::User { id, .. } => Some(id),
            _ => None,
        }
    }

    /// What `GET /_rustybin/session` adds beside the user name: the user id
    /// and display name, the roles and the permissions.
    pub fn session_fields(&self) -> Value {
        let (id, display) = match &self.principal {
            Principal::User {
                id, display_name, ..
            } => (Some(id.as_str()), Some(display_name.as_str())),
            _ => (None, None),
        };
        json!({
            "user_id": id,
            "display_name": display,
            "roles": self.roles,
            "permissions": self.permissions.names(),
        })
    }
}

/// The 0.3 [`Grant`] of a permission set: `admin` with every permission,
/// `console` with every permission of the viewer role, `inspector` with
/// `console` or `traffic.read`, `harness` with `admin` or `harness.read`
/// and `harness.run`. Every 0.3 scope keeps its grant
/// (`the_old_scopes_keep_their_grants`).
pub fn grant_of(perms: PermSet) -> Grant {
    let admin = perms.is_all();
    let console = admin || perms.contains(BuiltIn::Viewer.permissions());
    Grant {
        admin,
        console,
        inspector: console || perms.has(Permission::TrafficRead),
        harness: admin || (perms.has(Permission::HarnessRead) && perms.has(Permission::HarnessRun)),
    }
}

/// The names in a claim: a space separated string or an array of strings.
/// `None` when absent (or null).
fn claim_names(claim: Option<&Value>) -> Option<Vec<String>> {
    match claim {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.split_whitespace().map(str::to_string).collect()),
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        ),
        Some(_) => Some(Vec::new()),
    }
}

/// What the `console` scope adds to the viewer role: the reads it had in
/// 0.4 (captured traffic, the AI assist's read tasks and the MCP
/// inspector), so a token a platform issued keeps its reach.
pub const CONSOLE_SCOPE_EXTRA: &[Permission] = &[
    Permission::TrafficRead,
    Permission::AssistUse,
    Permission::McpInspect,
];

/// The role and permissions of one 0.3 scope name (`None`: not a scope).
/// Each scope keeps the reach it had in 0.4: `admin` is the admin role,
/// `console` the viewer role plus [`CONSOLE_SCOPE_EXTRA`], `inspector`
/// `traffic.read` alone, `harness` every `harness.*` permission.
pub fn scope_role(scope: &str) -> Option<(Option<BuiltIn>, PermSet)> {
    match scope {
        "admin" => Some((Some(BuiltIn::Admin), BuiltIn::Admin.permissions())),
        "console" => Some((
            Some(BuiltIn::Viewer),
            BuiltIn::Viewer
                .permissions()
                .union(PermSet::of(CONSOLE_SCOPE_EXTRA)),
        )),
        "inspector" => Some((None, PermSet::of(&[Permission::TrafficRead]))),
        "harness" => Some((
            None,
            PermSet::of(&[
                Permission::HarnessRead,
                Permission::HarnessRun,
                Permission::HarnessCluster,
            ]),
        )),
        _ => None,
    }
}

/// The roles and permissions of a `scope` claim (absent: `console`).
/// Unknown scope names grant nothing, as in 0.3.
pub fn from_scope_claim(claim: Option<&Value>) -> (Vec<String>, PermSet) {
    let names = claim_names(claim).unwrap_or_else(|| vec!["console".to_string()]);
    let mut roles = Vec::new();
    let mut perms = PermSet::EMPTY;
    for n in &names {
        if let Some((role, p)) = scope_role(n) {
            if let Some(r) = role {
                if !roles.iter().any(|x| x == r.name()) {
                    roles.push(r.name().to_string());
                }
            }
            perms = perms.union(p);
        }
    }
    (roles, perms)
}

/// The roles and permissions of a control JWT's claims: its `scope`, plus
/// `rb_roles` (role names, built-in or custom) and `rb_perms` (permission
/// names). Without a `scope` but with either of those, the token is what
/// they say (no `console` default). Unknown names are ignored and logged
/// once.
pub fn from_jwt_claims(claims: &Value, users: &Users) -> Resolved {
    let rb_roles = claim_names(claims.get("rb_roles"));
    let rb_perms = claim_names(claims.get("rb_perms"));
    let scope = claims.get("scope").filter(|v| !v.is_null());
    let (mut roles, mut perms) = if scope.is_none() && (rb_roles.is_some() || rb_perms.is_some()) {
        (Vec::new(), PermSet::EMPTY)
    } else {
        from_scope_claim(scope)
    };
    for name in rb_roles.unwrap_or_default() {
        match users.role_permissions(&name) {
            Some(p) => {
                perms = perms.union(p);
                if !roles.contains(&name) {
                    roles.push(name);
                }
            }
            None => note_unknown("role", &name),
        }
    }
    for name in rb_perms.unwrap_or_default() {
        match Permission::parse(&name) {
            Some(p) => perms.insert(p),
            None => note_unknown("permission", &name),
        }
    }
    Resolved {
        principal: Principal::Token {
            sub: claims
                .get("sub")
                .and_then(Value::as_str)
                .map(|s| s.chars().take(128).collect()),
        },
        roles,
        permissions: perms,
    }
}

/// The user a sign-in token names: `sub` is the user id; a 0.3 token
/// (`sub: "user:<name>"`, no user id) is user one. `None` when the token
/// names nobody the store has.
fn sign_in_user_id(claims: &Value, users: &Users) -> Option<String> {
    let sub = claims.get("sub").and_then(Value::as_str)?;
    if users::is_user_id(sub) {
        return Some(sub.to_string());
    }
    if sub.starts_with("user:") {
        return users.first_admin().map(|u| u.id);
    }
    None
}

/// Resolve a verified sign-in token's user: refused (as signed out) when
/// the user is gone, disabled, or signed out everywhere since (their
/// generation moved past the token's `rb_ugen`).
pub fn from_sign_in(
    claims: &Value,
    users: &Users,
) -> Result<Resolved, crate::control_auth::JwtError> {
    use crate::control_auth::JwtError;
    let id = sign_in_user_id(claims, users).ok_or(JwtError::Revoked)?;
    let r = users.resolve(&id).ok_or(JwtError::Revoked)?;
    if r.user.disabled {
        return Err(JwtError::Revoked);
    }
    let ugen = claims
        .get("rb_ugen")
        .and_then(Value::as_u64)
        .unwrap_or(users::FIRST_GENERATION);
    if ugen != r.user.generation {
        return Err(JwtError::Revoked);
    }
    Ok(Resolved {
        principal: Principal::User {
            id: r.user.id.clone(),
            name: r.user.name.clone(),
            display_name: r.user.display_name.clone(),
        },
        roles: r.roles.clone(),
        permissions: r.permissions,
    })
}

/// Unknown names logged so far (bounded: past the cap nothing more is
/// logged).
const MAX_NOTED: usize = 256;
static NOTED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Log an unknown role or permission name once (a platform may roll out a
/// newer token format first): never an error.
pub fn note_unknown(kind: &str, name: &str) {
    let key = format!("{kind}:{}", crate::logging::printable(name, 64));
    let mut noted = NOTED.lock().unwrap_or_else(|e| e.into_inner());
    if noted.len() >= MAX_NOTED || noted.contains(&key) {
        return;
    }
    tracing::warn!(
        target: "rustybin::auth",
        kind,
        name = %crate::logging::printable(name, 64),
        "an unknown {kind} name was ignored"
    );
    noted.insert(key);
}

/// Has this unknown name been logged? (tests)
#[cfg(test)]
pub(crate) fn was_noted(kind: &str, name: &str) -> bool {
    NOTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&format!("{kind}:{name}"))
}

/// At startup: the 0.3 console user becomes user one (the first admin)
/// when the sign-in is on; its name follows the sign-in settings.
pub fn migrate(config: &crate::config::Config, settings: &crate::settings::Settings) {
    if !crate::login::enabled(config) {
        return;
    }
    let name = crate::login::effective_user(settings);
    if config.users.ensure_first_admin(&name) {
        tracing::info!(
            target: "rustybin::auth",
            user = %crate::logging::printable(&name, 64),
            "the console user is user one, with the admin role"
        );
    }
}

/// The startup warning when users live in memory.
pub fn warn_in_memory(state: &crate::state::AppState) {
    if !crate::login::enabled(&state.config) {
        return;
    }
    if let Some(e) = state.users.load_error() {
        tracing::warn!(target: "rustybin::auth", "{e}: users are kept in memory until it is fixed");
    } else if !state.users.persisted() {
        tracing::warn!(
            target: "rustybin::auth",
            "no RUSTYBIN_STATE_DIR: users and roles live in memory and are gone at the next restart"
        );
    }
}

/// The diagnostics check of the user store.
pub fn diagnostics(
    state: &crate::state::AppState,
) -> Option<(&'static str, String, Option<&'static str>)> {
    if !crate::login::enabled(&state.config) {
        return None;
    }
    let users = &state.users;
    Some(if let Some(e) = users.load_error() {
        (
            "fail",
            format!("{e}: users are kept in memory and the file is left as it is"),
            Some("fix or restore users.json in the state directory, then restart"),
        )
    } else if let Some(e) = users.write_error() {
        (
            "fail",
            format!("the last change of the users could not be saved: {e}"),
            Some("give the process write access to the state directory"),
        )
    } else if !users.persisted() {
        (
            "warn",
            format!(
                "{} user(s) in memory only: users and roles are gone at the next restart",
                users.len()
            ),
            Some("set RUSTYBIN_STATE_DIR to a writable directory (users are kept in <state>/users.json)"),
        )
    } else {
        (
            "ok",
            format!("{} user(s) kept in users.json", users.len()),
            None,
        )
    })
}

/// May a token with `scope` call `method path` (by the catalogue's
/// declarations)? `admin` is every permission. Tests.
#[cfg(test)]
pub(crate) fn scope_may(scope: &str, method: &str, path: &str) -> bool {
    let perms = if scope == "admin" {
        PermSet::all()
    } else {
        from_scope_claim(Some(&Value::String(scope.into()))).1
    };
    crate::catalog::control_route_for_path(path)
        .filter(|e| e.expanded_methods().contains(&method))
        .and_then(|e| e.need(method))
        .is_some_and(|n| n.met_by(perms))
}

#[cfg(test)]
mod access_table;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod users_tests;
