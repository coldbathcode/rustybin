//! Checkpoint tools (`crate::checkpoints`): the same functions as the
//! `/_rustybin/checkpoints*` handlers. A restore and a delete are confirmed.

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_bool, arg_str, confirm, err, need_str, ok, Confirm};
use crate::checkpoints;
use crate::mcp::core::CallCtx;

/// `list_checkpoints`: every checkpoint, or one by name.
pub fn list_checkpoints(ctx: &CallCtx, args: &Value) -> Value {
    let store = &ctx.app.checkpoints;
    if let Some(name) = arg_str(args, "name") {
        return match store.summary(name) {
            Some(v) => ok(ctx, format!("Checkpoint {name}"), v),
            None => err(format!("no checkpoint named {name:?}")),
        };
    }
    let list = store.list();
    let names: Vec<&str> = list.iter().filter_map(|c| c["name"].as_str()).collect();
    ok(
        ctx,
        format!("{} checkpoint(s): {}", list.len(), names.join(", ")),
        json!({ "checkpoints": list, "max_checkpoints": checkpoints::MAX_CHECKPOINTS }),
    )
}

/// `save_checkpoint`.
pub async fn save_checkpoint(ctx: &CallCtx, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n.to_string(),
        Err(e) => return e,
    };
    let replace = arg_bool(args, "replace").unwrap_or(false);
    let state = ctx.app.clone();
    let n = name.clone();
    match tokio::task::spawn_blocking(move || checkpoints::save(&state, &n, replace)).await {
        Ok(Ok(v)) => {
            let count = v["mocks"].as_array().map(Vec::len).unwrap_or(0);
            ok(
                ctx,
                format!("Saved checkpoint {name} ({count} mock(s) and the health toggle)"),
                v,
            )
        }
        Ok(Err(e)) => err(e.1),
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
    if !dry_run {
        let action = format!(
            "restore checkpoint {name}: the instance's API mocks become what it saved (others are deleted), the health toggle is set, the flaky counters reset"
        );
        if let Confirm::Answer(v) = confirm(ctx, "restore_checkpoint", args, params, &action).await
        {
            return v;
        }
    }
    match checkpoints::restore_with(access.svc(&ctx.app), name.clone(), dry_run).await {
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
        Err(e) => err(e.1),
    }
}

/// `delete_checkpoint` (confirmed).
pub async fn delete_checkpoint(ctx: &CallCtx, args: &Value, params: &Value) -> Value {
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
    if ctx.app.checkpoints.delete(name) {
        ok(
            ctx,
            format!("Deleted checkpoint {name}"),
            json!({ "deleted": name }),
        )
    } else {
        err(format!("no checkpoint named {name:?}"))
    }
}
