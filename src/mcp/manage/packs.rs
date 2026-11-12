//! Demo pack tools (`crate::packs`): the same functions as the
//! `/_rustybin/packs*` handlers. A pack is a document the user gives; what
//! an exported pack holds was written by people (talk tracks, captured
//! requests), so it goes under `untrusted_*`. A teardown is confirmed.

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_bool, arg_str, confirm, err, need_str, ok, Confirm};
use crate::mcp::core::CallCtx;
use crate::packs;

/// `list_packs`.
pub fn list_packs(ctx: &CallCtx) -> Value {
    let v = packs::list(&ctx.app);
    let names: Vec<&str> = v["packs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["name"].as_str())
        .collect();
    let summary = format!("{} pack(s) applied: {}", names.len(), names.join(", "));
    // Titles, customers and notes are what the pack's author wrote.
    ok(
        ctx,
        summary,
        json!({ "untrusted_packs": v["packs"], "max_packs": v["max_packs"], "sections": v["sections"] }),
    )
}

/// `apply_pack` (`dry_run`: the preview).
pub async fn apply_pack(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let Some(document) = args.get("document").filter(|d| !d.is_null()) else {
        return err(
            "missing required argument: document (the pack: YAML or JSON text, or an object)",
        );
    };
    let dry_run = arg_bool(args, "dry_run").unwrap_or(false);
    let untrusted = arg_bool(args, "untrusted").unwrap_or(false);
    let by = packs::Applier {
        caller: access.resolved.clone(),
        principal: access.principal(),
        base_url: Some(access.svc(&ctx.app).links.base_url().to_string()),
    };
    match packs::apply(&ctx.app, by, document, dry_run, untrusted).await {
        Ok(v) => {
            let name = v["project"].as_str().unwrap_or("?").to_string();
            let key = if dry_run { "would_create" } else { "created" };
            let count = |k: &str| v[key][k].as_array().map(Vec::len).unwrap_or(0);
            let summary = format!(
                "{} {name}: {} mock(s), {} portal item(s), {} test(s), {} schedule(s), {} storyboard(s) in the project {name}.{}",
                if dry_run { "The pack would apply as" } else { "Applied pack" },
                count("mocks"),
                count("portal_items"),
                count("tests"),
                count("test_schedules"),
                count("storyboards"),
                if dry_run { " Nothing was changed." } else { " Remove it with teardown_pack." }
            );
            ok(ctx, summary, v)
        }
        Err(e) => {
            // The refusal with its names (collisions, problems), as data.
            let mut message = e.message();
            if let Some(p) = e.body.get("permission").and_then(Value::as_str) {
                message = format!("{message} (permission {p})");
            }
            err(message)
        }
    }
}

/// `teardown_pack` (confirmed unless `dry_run`).
pub async fn teardown_pack(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n.to_string(),
        Err(e) => return e,
    };
    let dry_run = arg_bool(args, "dry_run").unwrap_or(false);
    if !dry_run {
        let action = format!(
            "tear down the pack {name}: DELETE the mocks, portal items, saved tests, storyboards and checkpoints its project holds, its test schedules and the project"
        );
        if let Confirm::Answer(v) = confirm(ctx, "teardown_pack", args, params, &action).await {
            return v;
        }
    }
    match packs::teardown(
        &ctx.app,
        &access.resolved,
        &access.principal(),
        &name,
        dry_run,
    )
    .await
    {
        Ok(v) => {
            let key = if dry_run { "would_remove" } else { "removed" };
            let count: usize = v[key]
                .as_object()
                .map(|m| m.values().filter_map(Value::as_array).map(Vec::len).sum())
                .unwrap_or(0);
            let summary = if dry_run {
                format!("A teardown of {name} would remove {count} resource(s) and the project. Nothing was changed.")
            } else if v["ok"] == json!(true) {
                format!("Tore down {name}: {count} resource(s) and the project removed.")
            } else {
                format!("Tore down {name} with problems (see problems).")
            };
            ok(ctx, summary, v)
        }
        Err(e) => err(e.message()),
    }
}

/// `export_pack`: a project as a pack document.
pub fn export_pack(ctx: &CallCtx, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    match packs::export(&ctx.app, name) {
        Ok(doc) => {
            let count = |k: &str| doc[k].as_array().map(Vec::len).unwrap_or(0);
            let summary = format!(
                "The project {name} as a pack: {} mock(s), {} test(s), {} storyboard(s). The document is in untrusted_pack.",
                count("mocks"),
                count("tests"),
                count("storyboards")
            );
            let body = if arg_str(args, "format") == Some("json") {
                json!({ "untrusted_pack": doc })
            } else {
                json!({ "untrusted_pack": packs::to_yaml(&doc) })
            };
            ok(ctx, summary, body)
        }
        Err(e) => err(e.message()),
    }
}
