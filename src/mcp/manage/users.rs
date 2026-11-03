//! Users and roles tools (access control step 09): the same functions as
//! the `/_rustybin/users*` and `/_rustybin/roles*` handlers
//! (`crate::rbac::service`). No tool takes or returns a password or a hash:
//! people join through the invitation link `invite_user` answers once and
//! set their own password in the browser.
//!
//! Confirmations: `invite_user` always (it answers a link that signs a
//! person in), `update_user` when it grants a privileged role (one with
//! `users.manage` or `settings.security`), `delete_user` always, `save_role`
//! when the role is privileged.

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_bool, arg_str, confirm, err, need_str, ok, Confirm};
use crate::mcp::core::CallCtx;
use crate::mocks::service::ServiceError;
use crate::rbac::{role, service, users::Source};

fn failed(e: ServiceError) -> Value {
    super::tools::service_error(&e)
}

/// `list_users`: every user, or one (`user`: an id or a sign-in name).
pub fn list_users(ctx: &CallCtx, args: &Value) -> Value {
    if let Some(u) = arg_str(args, "user").filter(|u| !u.is_empty()) {
        return match service::get_user(&ctx.app, u) {
            Ok(v) => ok(
                ctx,
                format!(
                    "{} ({}): {}, roles {}",
                    v["name"].as_str().unwrap_or(""),
                    v["id"].as_str().unwrap_or(""),
                    v["status"].as_str().unwrap_or(""),
                    roles_text(&v["roles"])
                ),
                v,
            ),
            Err(e) => failed(e),
        };
    }
    match service::list_users(&ctx.app) {
        Ok(v) => {
            let lines: Vec<String> = v["users"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .map(|u| {
                            format!(
                                "{} ({}, {})",
                                u["name"].as_str().unwrap_or(""),
                                roles_text(&u["roles"]),
                                u["status"].as_str().unwrap_or("")
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            ok(
                ctx,
                format!("{} user(s): {}", lines.len(), lines.join(", ")),
                v,
            )
        }
        Err(e) => failed(e),
    }
}

fn roles_text(v: &Value) -> String {
    v.as_array()
        .map(|r| {
            r.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

fn privileged(ctx: &CallCtx, roles: &[String]) -> Option<String> {
    roles
        .iter()
        .find(|r| {
            ctx.app
                .users
                .role_permissions(r)
                .is_some_and(role::is_privileged)
        })
        .cloned()
}

/// `invite_user` (confirmed): a new user, invited, and the one-time link;
/// with `reset: true` for a user who exists, a new link (a password reset).
pub async fn invite_user(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n.to_string(),
        Err(e) => return e,
    };
    let reset = arg_bool(args, "reset").unwrap_or(false);
    let base = access.links.base_url().to_string();
    if reset {
        let action = format!(
            "send {name} a new invitation link: whoever opens it sets the user's password and signs in as them"
        );
        if let Confirm::Answer(v) = confirm(ctx, "invite_user", args, params, &action).await {
            return v;
        }
        return match service::reinvite_user(&ctx.app, &access.resolved, &name, &base) {
            Ok(v) => link_answer(ctx, &name, v),
            Err(e) => failed(e),
        };
    }
    let invite = match crate::rbac::api::invite_of(args) {
        Ok(i) => i,
        Err(e) => return err(e),
    };
    let sso = invite.sso;
    let action = if sso {
        format!(
            "add {name} as a single sign-on user with the pinned role(s) {}: whoever the identity provider signs in as {name} gets them",
            invite.roles.join(", ")
        )
    } else {
        format!(
            "invite {name} with the role(s) {}: the answer is a link that lets whoever opens it set the password and sign in as {name}",
            invite.roles.join(", ")
        )
    };
    if let Confirm::Answer(v) = confirm(ctx, "invite_user", args, params, &action).await {
        return v;
    }
    match service::invite_user(&ctx.app, &access.resolved, invite, &base, Source::Password) {
        Ok(v) if sso => ok(
            ctx,
            format!("{name} signs in with single sign-on (no link, no password): their groups add roles at every sign-in."),
            v,
        ),
        Ok(v) => link_answer(ctx, &name, v),
        Err(e) => failed(e),
    }
}

fn link_answer(ctx: &CallCtx, name: &str, v: Value) -> Value {
    let url = v["invitation"]["url"].as_str().unwrap_or("").to_string();
    let expires = v["invitation"]["expires_at"].as_str().unwrap_or("");
    ok(
        ctx,
        format!(
            "Invitation for {name} (shown once, valid until {expires}): {url} Send it to them; they set their own password."
        ),
        v,
    )
}

/// `update_user`: roles, display name, email, disabled (confirmed when it
/// grants a privileged role).
pub async fn update_user(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let user = match need_str(args, "user") {
        Ok(u) => u.to_string(),
        Err(e) => return e,
    };
    let mut fields = args.clone();
    if let Some(m) = fields.as_object_mut() {
        m.remove("user");
        m.remove("confirm");
    }
    let patch = match crate::rbac::api::patch_of(&fields) {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let granted = patch
        .roles
        .as_deref()
        .and_then(|r| privileged(ctx, r))
        .or_else(|| {
            patch
                .pinned_roles
                .as_deref()
                .and_then(|r| privileged(ctx, r))
        });
    if let Some(role) = granted {
        let action = format!(
            "give {user} the privileged role {role}: it may change who may do what on this instance"
        );
        if let Confirm::Answer(v) = confirm(ctx, "update_user", args, params, &action).await {
            return v;
        }
    }
    match service::update_user(&ctx.app, &access.resolved, &user, patch) {
        Ok(v) => ok(
            ctx,
            format!(
                "{}: {}, roles {}",
                v["name"].as_str().unwrap_or(&user),
                v["status"].as_str().unwrap_or(""),
                roles_text(&v["roles"])
            ),
            v,
        ),
        Err(e) => failed(e),
    }
}

/// `delete_user` (confirmed).
pub async fn delete_user(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let user = match need_str(args, "user") {
        Ok(u) => u.to_string(),
        Err(e) => return e,
    };
    let action = format!("delete the user {user}: they can no longer sign in, their sessions end");
    if let Confirm::Answer(v) = confirm(ctx, "delete_user", args, params, &action).await {
        return v;
    }
    match service::delete_user(&ctx.app, &access.resolved, &user) {
        Ok(v) => ok(ctx, format!("Deleted {user}"), v),
        Err(e) => failed(e),
    }
}

/// `list_roles`: every role with its permissions, or one (`name`) with who
/// holds it.
pub fn list_roles(ctx: &CallCtx, args: &Value) -> Value {
    if let Some(n) = arg_str(args, "name").filter(|n| !n.is_empty()) {
        return match service::get_role(&ctx.app, n) {
            Ok(v) => ok(
                ctx,
                format!(
                    "Role {n}{}: {} permission(s), {} user(s)",
                    if v["privileged"] == json!(true) {
                        " (privileged)"
                    } else {
                        ""
                    },
                    v["permissions"].as_array().map(Vec::len).unwrap_or(0),
                    v["users"]
                ),
                v,
            ),
            Err(e) => failed(e),
        };
    }
    let v = service::list_roles(&ctx.app);
    let names: Vec<String> = v["roles"]
        .as_array()
        .map(|l| {
            l.iter()
                .map(|r| {
                    let n = r["name"].as_str().unwrap_or("");
                    if r["privileged"] == json!(true) {
                        format!("{n} (privileged)")
                    } else {
                        n.to_string()
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    ok(
        ctx,
        format!("{} role(s): {}", names.len(), names.join(", ")),
        v,
    )
}

/// `save_role`: create or change a custom role (confirmed when it is
/// privileged).
pub async fn save_role(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let role = match crate::rbac::api::role_of(args, None) {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    if role::is_privileged(role.permission_set()) {
        let action = format!(
            "save the privileged role {}: whoever holds it may change who may do what on this instance",
            role.name
        );
        if let Confirm::Answer(v) = confirm(ctx, "save_role", args, params, &action).await {
            return v;
        }
    }
    let name = role.name.clone();
    match service::save_role(&ctx.app, &access.resolved, role, false) {
        Ok(v) => ok(
            ctx,
            format!(
                "{} role {name} ({} permission(s))",
                if v["created"] == json!(true) {
                    "Created"
                } else {
                    "Changed"
                },
                v["permissions"].as_array().map(Vec::len).unwrap_or(0)
            ),
            v,
        ),
        Err(e) => failed(e),
    }
}

/// `delete_role`: a custom role nobody holds.
pub fn delete_role(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n.to_string(),
        Err(e) => return e,
    };
    match service::delete_role(&ctx.app, &access.resolved, &name) {
        Ok(v) => ok(ctx, format!("Deleted role {name}"), v),
        Err(e) => failed(e),
    }
}
