//! Checkpoint tools (`crate::checkpoints`): the same functions as the
//! `/_rustybin/checkpoints*` handlers. A restore and a delete are confirmed.

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_bool, arg_str, confirm, err, need_str, ok, Confirm};
use crate::checkpoints;
use crate::mcp::core::CallCtx;

/// `list_checkpoints`: the checkpoints the caller may see (of one project
/// with `project`), or one by name.
pub fn list_checkpoints(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let caller = &access.resolved;
    if let Some(name) = arg_str(args, "name") {
        return match checkpoints::summary_for(&ctx.app, caller, name) {
            Ok(v) => ok(ctx, format!("Checkpoint {name}"), v),
            Err(e) => err(e.message()),
        };
    }
    let filter = match crate::rbac::projects::Filter::parse(arg_str(args, "project")) {
        Ok(f) => f,
        Err(e) => return err(e),
    };
    let v = checkpoints::list_for(&ctx.app, caller, &filter);
    let list = v["checkpoints"].as_array().cloned().unwrap_or_default();
    let names: Vec<&str> = list.iter().filter_map(|c| c["name"].as_str()).collect();
    ok(
        ctx,
        format!("{} checkpoint(s): {}", list.len(), names.join(", ")),
        json!({ "checkpoints": list, "max_checkpoints": checkpoints::MAX_CHECKPOINTS }),
    )
}

/// `save_checkpoint` (`project`: that project's mocks only).
pub async fn save_checkpoint(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n.to_string(),
        Err(e) => return e,
    };
    let replace = arg_bool(args, "replace").unwrap_or(false);
    let project = arg_str(args, "project").map(str::to_string);
    let state = ctx.app.clone();
    let caller = access.resolved.clone();
    let n = name.clone();
    let saved = tokio::task::spawn_blocking(move || {
        checkpoints::save_for(&state, &caller, &n, replace, project.as_deref())
    })
    .await;
    match saved {
        Ok(Ok(v)) => {
            let count = v["mocks"].as_array().map(Vec::len).unwrap_or(0);
            let what = match v["project"].as_str() {
                Some(p) => format!("{count} mock(s) of the project {p}"),
                None => format!("{count} mock(s) and the health toggle"),
            };
            ok(ctx, format!("Saved checkpoint {name} ({what})"), v)
        }
        Ok(Err(e)) => err(e.message()),
        Err(_) => err("the save did not finish"),
    }
}

/// `restore_checkpoint` (confirmed unless `dry_run`).
pub async fn restore_checkpoint(
    ctx: &CallCtx,
    access: &Access,
    args: &Value,
    params: &Value,
) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n.to_string(),
        Err(e) => return e,
    };
    let dry_run = arg_bool(args, "dry_run").unwrap_or(false);
    let source = match checkpoints::Source::parse(arg_str(args, "from")) {
        Ok(s) => s,
        Err(e) => return err(e.1),
    };
    let git = source == checkpoints::Source::Git;
    if !dry_run {
        let action = format!(
            "restore checkpoint {name}{}: the instance's API mocks become what it saved (others are deleted), the health toggle is set, the flaky counters reset",
            if git { " from the configuration repository" } else { "" }
        );
        if let Confirm::Answer(v) = confirm(ctx, "restore_checkpoint", args, params, &action).await
        {
            return v;
        }
    }
    let svc = access.svc(&ctx.app);
    let result = if git {
        checkpoints::restore_from_git(svc, name.clone(), dry_run).await
    } else {
        checkpoints::restore_with(svc, name.clone(), dry_run).await
    };
    match result {
        Ok(v) => {
            let m = &v["mocks"];
            let n = |k: &str| m[k].as_array().map(Vec::len).unwrap_or(0);
            ok(
                ctx,
                format!(
                    "{} checkpoint {name}: {} created, {} replaced, {} with data restored, {} deleted, {} unchanged.",
                    if dry_run { "A restore would apply" } else { "Restored" },
                    n("created"),
                    n("replaced"),
                    n("data_restored"),
                    n("deleted"),
                    n("unchanged")
                ),
                v,
            )
        }
        Err(e) => refused(&e),
    }
}

/// A refusal as a tool error: the message, and the fields of the answer
/// when it has some (a repository's unverified commit).
fn refused(e: &checkpoints::Refused) -> Value {
    let mut v = err(format!("{} ({})", e.1, e.0.as_u16()));
    if e.2.is_object() {
        let mut body = e.body();
        body["status"] = json!(e.0.as_u16());
        v["structuredContent"] = body;
    }
    v
}

/// `list_repository_checkpoints` (admin): the configuration repository's
/// checkpoints at `HEAD`. Written by whoever can push to the repository, so
/// the list goes under `untrusted_checkpoints`; the summary names only the
/// files (checked names).
pub async fn list_repository_checkpoints(ctx: &CallCtx) -> Value {
    match checkpoints::list_from_git(&ctx.app).await {
        Ok(mut v) => {
            let list = v["checkpoints"].take();
            let names: Vec<String> = list
                .as_array()
                .map(|l| {
                    l.iter()
                        .map(|c| {
                            let n = c["name"].as_str().unwrap_or("");
                            if c["ok"] == json!(true) {
                                n.to_string()
                            } else {
                                format!("{n} (cannot be restored)")
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            if let Value::Object(m) = &mut v {
                m.remove("checkpoints");
                m.insert("untrusted_checkpoints".into(), list);
            }
            ok(
                ctx,
                format!(
                    "{} checkpoint(s) in the repository at {}: {}",
                    names.len(),
                    v["path"].as_str().unwrap_or("checkpoints"),
                    names.join(", ")
                ),
                v,
            )
        }
        Err(e) => refused(&e),
    }
}

/// `delete_checkpoint` (confirmed).
pub async fn delete_checkpoint(
    ctx: &CallCtx,
    access: &Access,
    args: &Value,
    params: &Value,
) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    if let Confirm::Answer(v) = confirm(
        ctx,
        "delete_checkpoint",
        args,
        params,
        &format!("delete checkpoint {name}"),
    )
    .await
    {
        return v;
    }
    match checkpoints::delete_for(&ctx.app, &access.resolved, name) {
        Ok(v) => ok(ctx, format!("Deleted checkpoint {name}"), v),
        Err(e) => err(e.message()),
    }
}
