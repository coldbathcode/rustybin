//! Users, roles and your own account: the logic the REST handlers
//! (`crate::rbac::api`), the management tools (`crate::mcp::manage::users`)
//! and documents (`crate::configdoc`) share. Every function takes the
//! caller's [`Resolved`] and answers JSON or a [`ServiceError`] with the
//! status and body the API sends.
//!
//! The rules on top of the store's guard rails (`crate::rbac::users`):
//!
//! - **Nobody changes their own roles** (`own_roles`).
//! - **Nobody grants more than they hold**: a caller gives a user only
//!   roles whose permissions they hold, manages (changes, disables,
//!   deletes, resets) only users whose permissions they hold, and saves or
//!   deletes only custom roles within their own permissions. An admin
//!   holds every permission, so this binds only custom roles with
//!   `users.manage`. The refusal names the first permission missing.
//! - **Ending a user** (signed out everywhere) ends their listed sessions
//!   and their agents' MCP sessions ([`ended_user`]); **removing one**
//!   (disabled, deleted) also revokes every AI agent acting for them
//!   ([`removed_user`]).
//!
//! Invitation links are answered once, by the call that made them, and
//! never logged.

use axum::http::StatusCode;
use serde_json::{json, Value};

use super::users::{self, NewInvitation, NewUser, Source, User, UserPatch, UsersError};
use super::{BuiltIn, CustomRole, PermSet, Permission, Principal, Resolved};
use crate::mocks::service::{ServiceError, SvcResult};
use crate::state::AppState;

/// The console page an invitation link opens.
pub const JOIN_PAGE: &str = "/ui/#/join/";
/// The page an invitation of an account that opens only the API portal
/// opens.
pub const PORTAL_JOIN_PAGE: &str = "/portal/#/join/";

/// The answer of a store refusal.
pub fn store_error(e: UsersError) -> ServiceError {
    match e {
        UsersError::Invalid(problems) => ServiceError::with_body(
            StatusCode::BAD_REQUEST,
            json!({ "error": format!("invalid: {}", problems.join("; ")), "reason": "invalid", "problems": problems }),
        ),
        UsersError::NotFound => ServiceError::with_body(
            StatusCode::NOT_FOUND,
            json!({ "error": "no such user", "reason": "not_found" }),
        ),
        UsersError::Conflict(m) => ServiceError::with_body(
            StatusCode::CONFLICT,
            json!({ "error": m, "reason": "conflict" }),
        ),
        UsersError::Full(m) => ServiceError::with_body(
            StatusCode::CONFLICT,
            json!({ "error": m, "reason": "full" }),
        ),
        UsersError::Refused(reason, m) => ServiceError::with_body(
            StatusCode::CONFLICT,
            json!({ "error": m, "reason": reason }),
        ),
        UsersError::Io(m) => ServiceError::with_body(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": m, "reason": "io" }),
        ),
    }
}

fn refused(status: StatusCode, reason: &str, message: impl Into<String>) -> ServiceError {
    ServiceError::with_body(status, json!({ "error": message.into(), "reason": reason }))
}

/// `404` outside the password sign-in: users sign in only there.
pub fn require_sign_in(state: &AppState) -> SvcResult<()> {
    if crate::login::enabled(&state.config) {
        return Ok(());
    }
    let hint = if state.config.shared_mode {
        "shared mode authenticates the control plane with API keys"
    } else {
        "users sign in only with RUSTYBIN_CONTROL_AUTH=password (the default); roles work in every mode"
    };
    Err(ServiceError::with_body(
        StatusCode::NOT_FOUND,
        json!({ "error": "not found", "control_auth": state.config.control_auth.as_str(), "hint": hint }),
    ))
}

/// Who the caller is, for an invitation's `by` and audit lines (never a
/// token).
pub fn principal_name(caller: &Resolved) -> String {
    match &caller.principal {
        Principal::User { name, .. } => name.clone(),
        Principal::AdminToken => "admin token".into(),
        Principal::Token { sub } => format!("jwt:{}", sub.as_deref().unwrap_or("")),
        Principal::Open => "open control plane".into(),
        Principal::Anonymous { .. } => "anonymous".into(),
        Principal::Key { id } => format!("key:{id}"),
    }
}

fn audit(event: &str, caller: &Resolved, target: &str, outcome: &str) {
    tracing::info!(
        target: "rustybin::auth",
        event,
        by = %crate::logging::printable(&principal_name(caller), 80),
        user = %crate::logging::printable(target, 64),
        outcome,
        "users and roles"
    );
}

/// The permissions of a list of role names (unknown ones count for
/// nothing; the store refuses them).
fn roles_permissions(state: &AppState, roles: &[String]) -> PermSet {
    roles
        .iter()
        .filter_map(|r| state.users.role_permissions(r))
        .fold(PermSet::EMPTY, PermSet::union)
}

/// The caller holds every permission of `perms`, else the refusal naming
/// the first one missing.
fn within(caller: &Resolved, perms: PermSet) -> SvcResult<()> {
    match perms.iter().find(|p| !caller.has(*p)) {
        None => Ok(()),
        Some(p) => Err(ServiceError::permission(caller, p)),
    }
}

/// The caller holds, in each project, what the user's roles there grant
/// (managing a user means managing their project roles too).
fn within_bindings(state: &AppState, caller: &Resolved, user_id: &str) -> SvcResult<()> {
    for b in state.users.bindings(user_id) {
        let perms = roles_permissions(state, &b.roles);
        let missing = perms.iter().find(|p| !caller.has_in(*p, Some(&b.project)));
        if let Some(p) = missing {
            return Err(ServiceError::permission(caller, p));
        }
    }
    Ok(())
}

fn find(state: &AppState, id_or_name: &str) -> SvcResult<User> {
    state.users.find(id_or_name).ok_or_else(|| {
        ServiceError::with_body(
            StatusCode::NOT_FOUND,
            json!({
                "error": format!("no user {}", crate::logging::printable(id_or_name, 64)),
                "reason": "not_found",
            }),
        )
    })
}

/// A user as the users routes answer it: the view plus the open
/// invitation (never its token), the sessions listed and whether it is
/// user one.
pub fn user_view(state: &AppState, u: &User) -> Value {
    let mut v = u.view();
    if let Value::Object(m) = &mut v {
        m.insert(
            "invitation".into(),
            state
                .users
                .invitation(&u.id)
                .map(|i| i.view())
                .unwrap_or(Value::Null),
        );
        m.insert(
            "sessions".into(),
            json!(state.config.console_tokens.sessions_of(&u.id).len()),
        );
        m.insert(
            "first_admin".into(),
            json!(state.users.is_first_admin(&u.id)),
        );
        m.insert(
            "projects".into(),
            json!(state
                .users
                .bindings(&u.id)
                .iter()
                .map(|b| b.view())
                .collect::<Vec<_>>()),
        );
    }
    v
}

/// The link an invitation opens: `<base>/ui/#/join/<token>`.
pub fn invitation_link(base_url: &str, token: &str) -> String {
    format!("{}{JOIN_PAGE}{token}", base_url.trim_end_matches('/'))
}

/// The link an invitation of `user` opens: the API portal's join page
/// (`<base>/portal/#/join/<token>`) for an account whose roles hold nothing
/// but `portal.read` (a tester never sees the console), else the console's.
pub fn invitation_link_for(state: &AppState, user: &User, base_url: &str, token: &str) -> String {
    let portal_only = state
        .users
        .resolve(&user.id)
        .is_some_and(|r| r.permissions.has(Permission::PortalRead) && r.permissions.len() == 1);
    if portal_only {
        format!(
            "{}{PORTAL_JOIN_PAGE}{token}",
            base_url.trim_end_matches('/')
        )
    } else {
        invitation_link(base_url, token)
    }
}

fn invitation_answer(state: &AppState, inv: &NewInvitation, base_url: &str) -> Value {
    json!({
        "user": user_view(state, &inv.user),
        "invitation": {
            "url": invitation_link_for(state, &inv.user, base_url, &inv.token),
            "token": inv.token,
            "expires_at": users::rfc3339(inv.expires_at),
            "reset": !inv.user.pending,
        },
        "note": "the link is shown once: send it to the person, who opens it and sets their own password",
    })
}

/// A user's sessions end (disabled, deleted, signed out everywhere, a
/// password change): their listed sessions are forgotten and the MCP
/// sessions of the agents acting for them end (their tokens, sign-in and
/// OAuth alike, are refused by the moved generation or the missing user).
/// Answers how many listed sessions there were.
pub fn ended_user(state: &AppState, user_id: &str) -> usize {
    state.agents.end_owned_sessions(user_id);
    state.config.console_tokens.forget_user(user_id)
}

/// A user is disabled or deleted: [`ended_user`], and every AI agent
/// acting for them is revoked (an operator restores one, after enabling
/// the user again). Answers the sessions and the agents.
pub fn removed_user(state: &AppState, caller: &Resolved, user_id: &str) -> (usize, usize) {
    let ended = ended_user(state, user_id);
    let agents = state.agents.revoke_owned(
        user_id,
        &format!("{} (user removed)", principal_name(caller)),
        &state.config.console_tokens,
    );
    (ended, agents)
}

// ── Users ───────────────────────────────────────────────────────────

/// `GET /_rustybin/users`.
pub fn list_users(state: &AppState) -> SvcResult<Value> {
    require_sign_in(state)?;
    let list: Vec<Value> = state
        .users
        .list()
        .iter()
        .map(|u| user_view(state, u))
        .collect();
    Ok(json!({
        "count": list.len(),
        "capacity": users::MAX_USERS,
        "open_invitations": state.users.open_invitations(),
        "max_open_invitations": users::MAX_OPEN_INVITATIONS,
        "users": list,
    }))
}

/// `GET /_rustybin/users/{id}` (an id or a sign-in name).
pub fn get_user(state: &AppState, id: &str) -> SvcResult<Value> {
    require_sign_in(state)?;
    let u = find(state, id)?;
    let mut v = user_view(state, &u);
    v["permissions"] = json!(roles_permissions(state, &u.roles).names());
    Ok(v)
}

/// What an invitation of a new user carries.
#[derive(Clone, Debug, Default)]
pub struct Invite {
    pub name: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub roles: Vec<String>,
    /// A single sign-on user, created ahead of their first sign-in (no
    /// invitation, no password; `roles` are pinned on them).
    pub sso: bool,
    /// Roles in projects (step 11): `(project, roles)`. With some, the
    /// instance roles may be empty (the user sees those projects only).
    pub projects: Vec<(String, Vec<String>)>,
}

/// `POST /_rustybin/users`: create the user, invited, and answer the
/// one-time link.
pub fn invite_user(
    state: &AppState,
    caller: &Resolved,
    invite: Invite,
    base_url: &str,
    source: Source,
) -> SvcResult<Value> {
    require_sign_in(state)?;
    if invite.roles.is_empty() && invite.projects.is_empty() {
        return Err(refused(
            StatusCode::BAD_REQUEST,
            "invalid",
            "roles: give the user at least one role, or roles in a project (GET /_rustybin/roles lists them)",
        ));
    }
    for (project, roles) in &invite.projects {
        if !state.users.project_exists(project) {
            return Err(refused(
                StatusCode::BAD_REQUEST,
                "invalid",
                format!(
                    "projects: {} is not a project",
                    crate::logging::printable(project, 64)
                ),
            ));
        }
        if roles.is_empty() {
            return Err(refused(
                StatusCode::BAD_REQUEST,
                "invalid",
                "projects: each project needs at least one role",
            ));
        }
        if let Some(unknown) = roles
            .iter()
            .find(|r| state.users.role_permissions(r).is_none())
        {
            return Err(refused(
                StatusCode::BAD_REQUEST,
                "invalid",
                format!(
                    "projects: {} is not a role",
                    crate::logging::printable(unknown, 64)
                ),
            ));
        }
        let perms = roles_permissions(state, roles);
        let missing = perms.iter().find(|p| !caller.has_in(*p, Some(project)));
        if let Some(p) = missing {
            return Err(ServiceError::permission(caller, p));
        }
    }
    if let Some(unknown) = invite
        .roles
        .iter()
        .find(|r| state.users.role_permissions(r).is_none())
    {
        return Err(refused(
            StatusCode::BAD_REQUEST,
            "invalid",
            format!(
                "roles: {} is not a role",
                crate::logging::printable(unknown, 64)
            ),
        ));
    }
    within(caller, roles_permissions(state, &invite.roles))?;
    let sso = invite.sso;
    let bindings = invite.projects.clone();
    if sso && invite.roles.is_empty() {
        return Err(refused(
            StatusCode::BAD_REQUEST,
            "invalid",
            "roles: a single sign-on user created ahead needs at least one pinned role (their groups give project roles at sign-in)",
        ));
    }
    let new = NewUser {
        name: invite.name.trim().to_string(),
        display_name: invite.display_name.filter(|d| !d.trim().is_empty()),
        email: invite.email.filter(|e| !e.trim().is_empty()),
        roles: invite.roles,
        disabled: false,
        source: if sso { Source::Sso } else { source },
    };
    let name = new.name.clone();
    if sso {
        let user = state.users.add_sso(new).map_err(store_error)?;
        bind_new(state, &user.id, &bindings)?;
        audit("add_sso_user", caller, &name, "ok");
        return Ok(json!({
            "user": user_view(state, &user),
            "invitation": Value::Null,
            "note": "the user signs in with single sign-on: no link, no password; the provider's groups add their roles at every sign-in, on top of the pinned ones",
        }));
    }
    let inv = state
        .users
        .invite_new(new, &principal_name(caller))
        .map_err(store_error)?;
    bind_new(state, &inv.user.id, &bindings)?;
    audit("invite", caller, &name, "ok");
    Ok(invitation_answer(state, &inv, base_url))
}

/// A new user's project roles; on a failure the user goes again (nothing
/// half made).
fn bind_new(state: &AppState, user_id: &str, bindings: &[(String, Vec<String>)]) -> SvcResult<()> {
    for (project, roles) in bindings {
        if let Err(e) = state.users.set_binding(project, user_id, roles.clone()) {
            let _ = state.users.delete(user_id);
            return Err(store_error(e));
        }
    }
    Ok(())
}

/// `POST /_rustybin/users/{id}/invite`: a new link (a reset for a user who
/// joined: their password works until it is accepted).
pub fn reinvite_user(
    state: &AppState,
    caller: &Resolved,
    id: &str,
    base_url: &str,
) -> SvcResult<Value> {
    require_sign_in(state)?;
    let u = find(state, id)?;
    within(caller, roles_permissions(state, &u.roles))?;
    within_bindings(state, caller, &u.id)?;
    if state.users.is_first_admin(&u.id) && state.settings.locked() {
        return Err(refused(
            StatusCode::CONFLICT,
            "locked",
            "the first admin's password is held by the environment (RUSTYBIN_SETTINGS_LOCK): set RUSTYBIN_CONSOLE_PASSWORD_HASH instead",
        ));
    }
    let inv = state
        .users
        .invite(&u.id, &principal_name(caller))
        .map_err(store_error)?;
    audit(
        if u.pending { "invite" } else { "reset" },
        caller,
        &u.name,
        "ok",
    );
    Ok(invitation_answer(state, &inv, base_url))
}

/// `PATCH /_rustybin/users/{id}`.
pub fn update_user(
    state: &AppState,
    caller: &Resolved,
    id: &str,
    patch: UserPatch,
) -> SvcResult<Value> {
    require_sign_in(state)?;
    let u = find(state, id)?;
    if patch.is_empty() {
        return Err(refused(
            StatusCode::BAD_REQUEST,
            "invalid",
            "nothing to change: roles, pinned_roles, display_name, email or disabled",
        ));
    }
    if let Some(pinned) = &patch.pinned_roles {
        if caller.user_id() == Some(u.id.as_str()) && *pinned != u.pinned_roles {
            return Err(refused(
                StatusCode::FORBIDDEN,
                "own_roles",
                "nobody changes their own roles: ask another user who manages users",
            ));
        }
        within(caller, roles_permissions(state, pinned))?;
    }
    if let Some(roles) = &patch.roles {
        if caller.user_id() == Some(u.id.as_str()) && *roles != u.roles {
            return Err(refused(
                StatusCode::FORBIDDEN,
                "own_roles",
                "nobody changes their own roles: ask another user who manages users",
            ));
        }
        if roles.is_empty() && state.users.bindings(&u.id).is_empty() {
            return Err(refused(
                StatusCode::BAD_REQUEST,
                "invalid",
                "roles: a user holds at least one role, or roles in a project (disable the user instead)",
            ));
        }
        within(caller, roles_permissions(state, roles))?;
    }
    within(caller, roles_permissions(state, &u.roles))?;
    within_bindings(state, caller, &u.id)?;
    let disabling = patch.disabled == Some(true) && !u.disabled;
    let updated = state.users.update(&u.id, patch).map_err(store_error)?;
    let mut revoked = 0;
    if disabling {
        revoked = removed_user(state, caller, &u.id).1;
        audit("disable", caller, &u.name, "ok");
    } else {
        audit("update", caller, &u.name, "ok");
    }
    let mut v = user_view(state, &updated);
    if disabling {
        v["agents_revoked"] = json!(revoked);
    }
    Ok(v)
}

/// `DELETE /_rustybin/users/{id}`.
pub fn delete_user(state: &AppState, caller: &Resolved, id: &str) -> SvcResult<Value> {
    require_sign_in(state)?;
    let u = find(state, id)?;
    within(caller, roles_permissions(state, &u.roles))?;
    within_bindings(state, caller, &u.id)?;
    let gone = state.users.delete(&u.id).map_err(store_error)?;
    let (ended, agents) = removed_user(state, caller, &gone.id);
    audit("delete", caller, &gone.name, "ok");
    Ok(
        json!({ "deleted": gone.name, "id": gone.id, "sessions_ended": ended, "agents_revoked": agents }),
    )
}

/// `GET /_rustybin/users/{id}/sessions`.
pub fn user_sessions(state: &AppState, id: &str) -> SvcResult<Value> {
    require_sign_in(state)?;
    let u = find(state, id)?;
    let sessions = state.config.console_tokens.sessions_of(&u.id);
    Ok(json!({ "user": u.name, "user_id": u.id, "count": sessions.len(), "sessions": sessions }))
}

/// `DELETE /_rustybin/users/{id}/sessions`: sign the user out everywhere.
pub fn end_user_sessions(state: &AppState, caller: &Resolved, id: &str) -> SvcResult<Value> {
    require_sign_in(state)?;
    let u = find(state, id)?;
    within(caller, roles_permissions(state, &u.roles))?;
    within_bindings(state, caller, &u.id)?;
    state.users.rotate_generation(&u.id).map_err(store_error)?;

    let ended = ended_user(state, &u.id);
    audit("sign_out_user", caller, &u.name, "ok");
    Ok(
        json!({ "user": u.name, "user_id": u.id, "ended": ended, "note": "every token the user holds is refused from now on" }),
    )
}

// ── Roles ───────────────────────────────────────────────────────────

fn role_view(state: &AppState, name: &str) -> Option<Value> {
    let mut v = state
        .users
        .roles_view()
        .into_iter()
        .find(|r| r["name"] == name)?;
    v["users"] = json!(state.users.holders(name));
    Some(v)
}

/// `GET /_rustybin/roles`: every role (built-in, then custom) with how
/// many users hold it, and the permission table the matrix shows.
pub fn list_roles(state: &AppState) -> Value {
    let roles: Vec<Value> = state
        .users
        .roles_view()
        .into_iter()
        .map(|mut r| {
            let n = state.users.holders(r["name"].as_str().unwrap_or(""));
            r["users"] = json!(n);
            r
        })
        .collect();
    json!({
        "count": roles.len(),
        "custom": state.users.custom_roles().len(),
        "max_custom_roles": super::role::MAX_CUSTOM_ROLES,
        "roles": roles,
        "permissions": Permission::table(),
    })
}

/// `GET /_rustybin/roles/{name}`: the role and who holds it.
pub fn get_role(state: &AppState, name: &str) -> SvcResult<Value> {
    let mut v = role_view(state, name).ok_or_else(|| {
        ServiceError::with_body(
            StatusCode::NOT_FOUND,
            json!({ "error": format!("no role {}", crate::logging::printable(name, 64)), "reason": "not_found" }),
        )
    })?;
    let holders: Vec<String> = state
        .users
        .list()
        .into_iter()
        .filter(|u| u.roles.iter().any(|r| r == name))
        .map(|u| u.name)
        .collect();
    v["holders"] = json!(holders);
    Ok(v)
}

/// `POST /_rustybin/roles` (`create`: refused when the name is taken) and
/// `PUT /_rustybin/roles/{name}` (create or replace).
pub fn save_role(
    state: &AppState,
    caller: &Resolved,
    role: CustomRole,
    create: bool,
) -> SvcResult<Value> {
    if BuiltIn::parse(&role.name).is_some() {
        return Err(refused(
            StatusCode::BAD_REQUEST,
            "built_in",
            format!("{} is a built-in role, which cannot be changed", role.name),
        ));
    }
    let existing = state
        .users
        .custom_roles()
        .into_iter()
        .find(|r| r.name == role.name);
    if create && existing.is_some() {
        return Err(refused(
            StatusCode::CONFLICT,
            "conflict",
            format!(
                "a role named {} exists (PUT /_rustybin/roles/{} changes it)",
                role.name, role.name
            ),
        ));
    }
    within(caller, role.permission_set())?;
    if let Some(old) = &existing {
        within(caller, old.permission_set())?;
    }
    let name = role.name.clone();
    state.users.save_role(role).map_err(store_error)?;
    audit(
        if existing.is_some() {
            "change_role"
        } else {
            "create_role"
        },
        caller,
        &name,
        "ok",
    );
    let mut v = role_view(state, &name).unwrap_or(Value::Null);
    v["created"] = json!(existing.is_none());
    Ok(v)
}

/// `DELETE /_rustybin/roles/{name}`: a custom role nobody holds.
pub fn delete_role(state: &AppState, caller: &Resolved, name: &str) -> SvcResult<Value> {
    if BuiltIn::parse(name).is_some() {
        return Err(refused(
            StatusCode::BAD_REQUEST,
            "built_in",
            format!("{name} is a built-in role, which cannot be deleted"),
        ));
    }
    let Some(old) = state
        .users
        .custom_roles()
        .into_iter()
        .find(|r| r.name == name)
    else {
        return Err(ServiceError::with_body(
            StatusCode::NOT_FOUND,
            json!({ "error": format!("no custom role {}", crate::logging::printable(name, 64)), "reason": "not_found" }),
        ));
    };
    within(caller, old.permission_set())?;
    state.users.delete_role(name).map_err(store_error)?;
    audit("delete_role", caller, name, "ok");
    Ok(json!({ "deleted": name }))
}

// ── Your own account ────────────────────────────────────────────────

/// The caller's own user, or `400 not_a_user` (the admin token, a
/// platform's control JWT, an open control plane: no account of theirs).
pub fn me_user(state: &AppState, caller: &Resolved) -> SvcResult<User> {
    require_sign_in(state)?;
    caller
        .user_id()
        .and_then(|id| state.users.get(id))
        .ok_or_else(|| {
            refused(
                StatusCode::BAD_REQUEST,
                "not_a_user",
                "this credential is not a user's (the admin token or a control JWT): GET /_rustybin/session says what it is",
            )
        })
}

/// `GET /_rustybin/me`.
pub fn me(state: &AppState, caller: &Resolved) -> SvcResult<Value> {
    let u = me_user(state, caller)?;
    let mut v = user_view(state, &u);
    if let Value::Object(m) = &mut v {
        m.insert("permissions".into(), json!(caller.permissions.names()));
        m.insert(
            "password".into(),
            json!({
                "change": "PUT /_rustybin/me/password with current_password and new_password",
                "kept_in": if u.is_sso() { "the identity provider (single sign-on: no password on this instance)" } else if state.users.is_first_admin(&u.id) { "the sign-in settings (console.password_hash)" } else { "the user store" },
                "locked": state.users.is_first_admin(&u.id) && state.settings.locked(),
            }),
        );
    }
    Ok(v)
}

/// `PATCH /_rustybin/me`: the caller's display name and email (`null`
/// clears the email). Roles and the rest are for whoever manages users.
pub fn update_me(
    state: &AppState,
    caller: &Resolved,
    display_name: Option<String>,
    email: Option<Option<String>>,
) -> SvcResult<Value> {
    let u = me_user(state, caller)?;
    if display_name.is_none() && email.is_none() {
        return Err(refused(
            StatusCode::BAD_REQUEST,
            "invalid",
            "nothing to change: display_name or email",
        ));
    }
    let patch = UserPatch {
        display_name,
        email: email.map(|e| e.filter(|s| !s.trim().is_empty())),
        ..UserPatch::default()
    };
    let updated = state.users.update(&u.id, patch).map_err(store_error)?;
    audit("update_me", caller, &u.name, "ok");
    me(state, caller).map(|mut v| {
        v["display_name"] = json!(updated.display_name);
        v
    })
}

/// `GET /_rustybin/me/sessions`.
pub fn my_sessions(
    state: &AppState,
    caller: &Resolved,
    current: Option<String>,
) -> SvcResult<Value> {
    let u = me_user(state, caller)?;
    let sessions = state.config.console_tokens.sessions_of(&u.id);
    Ok(json!({ "count": sessions.len(), "current": current, "sessions": sessions }))
}

/// `DELETE /_rustybin/me/sessions`: sign yourself out everywhere (this
/// session too).
pub fn end_my_sessions(state: &AppState, caller: &Resolved) -> SvcResult<Value> {
    let u = me_user(state, caller)?;
    state.users.rotate_generation(&u.id).map_err(store_error)?;
    let ended = ended_user(state, &u.id);
    audit("sign_out_me", caller, &u.name, "ok");
    Ok(
        json!({ "ended": ended, "note": "every token of yours is refused from now on, this one included" }),
    )
}

/// `DELETE /_rustybin/me/sessions/{id}`: end one of your sessions.
pub fn end_my_session(state: &AppState, caller: &Resolved, id: &str) -> SvcResult<Value> {
    let u = me_user(state, caller)?;
    let ended = state.config.console_tokens.end_of(&u.id, id);
    Ok(json!({ "ended": ended, "id": id }))
}
