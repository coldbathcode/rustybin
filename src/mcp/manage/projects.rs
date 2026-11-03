//! Project tools (access control step 11): the same functions as the
//! `/_rustybin/projects*` handlers (`crate::rbac::project_api`).
//! `save_project` is confirmed when it gives a privileged role,
//! `delete_project` always (it takes the members' roles away, and with
//! `move_to` moves what the project holds).

use serde_json::Value;

use super::access::Access;
use super::tools::{arg_str, confirm, err, need_str, ok, Confirm};
use crate::mcp::core::CallCtx;
use crate::rbac::project_api::{self, MemberInput, SaveInput};

/// `list_projects`: the projects the caller may see, or one.
pub fn list_projects(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    if let Some(name) = arg_str(args, "name").filter(|n| !n.is_empty()) {
        return match project_api::get(&ctx.app, &access.resolved, name) {
            Ok(v) => ok(
                ctx,
                format!(
                    "Project {name}: {} member(s), your roles {}",
                    v["member_count"],
                    list_text(&v["your_roles"])
                ),
                v,
            ),
            Err(e) => super::tools::service_error(&e),
        };
    }
    let v = project_api::list(&ctx.app, &access.resolved);
    let lines: Vec<String> = v["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            format!(
                "{} (your roles: {})",
                p["name"].as_str().unwrap_or(""),
                list_text(&p["your_roles"])
            )
        })
        .collect();
    ok(
        ctx,
        format!("{} project(s): {}", lines.len(), lines.join(", ")),
        v,
    )
}

fn list_text(v: &Value) -> String {
    let items: Vec<&str> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if items.is_empty() {
        "none".into()
    } else {
        items.join(", ")
    }
}

/// `save_project` (confirmed when it gives a privileged role).
pub async fn save_project(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n.to_string(),
        Err(e) => return e,
    };
    let members: Option<Vec<MemberInput>> = match args.get("members") {
        None | Some(Value::Null) => None,
        Some(v) => match serde_json::from_value(v.clone()) {
            Ok(m) => Some(m),
            Err(e) => return err(format!("invalid members: {e}")),
        },
    };
    let privileged = members.iter().flatten().find_map(|m| {
        m.roles
            .iter()
            .find(|r| {
                ctx.app
                    .users
                    .role_permissions(r)
                    .is_some_and(crate::rbac::role::is_privileged)
            })
            .map(|r| (m.user.clone(), r.clone()))
    });
    if let Some((user, role)) = privileged {
        let action = format!(
            "give {user} the privileged role {role} in the project {name}: it may change who may do what there"
        );
        if let Confirm::Answer(v) = confirm(ctx, "save_project", args, params, &action).await {
            return v;
        }
    }
    let input = SaveInput {
        description: arg_str(args, "description").map(str::to_string),
        members,
    };
    match project_api::save(&ctx.app, &access.resolved, &name, input) {
        Ok(v) => {
            let verb = if v["created"] == Value::Bool(true) {
                "Created"
            } else {
                "Saved"
            };
            ok(
                ctx,
                format!("{verb} project {name} ({} member(s))", v["member_count"]),
                v,
            )
        }
        Err(e) => super::tools::service_error(&e),
    }
}

/// `delete_project` (confirmed).
pub async fn delete_project(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n.to_string(),
        Err(e) => return e,
    };
    let move_to = match project_api::parse_move_to(arg_str(args, "move_to")) {
        Ok(m) => m,
        Err(e) => return super::tools::service_error(&e),
    };
    let action = match &move_to {
        Some(to) => format!(
            "delete the project {name}: its members lose their roles there and its mocks, saved tests and checkpoints move to {}",
            to.as_deref().unwrap_or("the instance")
        ),
        None => format!("delete the project {name}: its members lose their roles there"),
    };
    if let Confirm::Answer(v) = confirm(ctx, "delete_project", args, params, &action).await {
        return v;
    }
    match project_api::delete(&ctx.app, &access.resolved, &name, move_to) {
        Ok(v) => ok(ctx, format!("Deleted project {name}"), v),
        Err(e) => super::tools::service_error(&e),
    }
}
