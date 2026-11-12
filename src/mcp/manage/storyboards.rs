//! Storyboard tools (`crate::storyboards`): the same functions as the
//! `/_rustybin/storyboards*` handlers. A storyboard is written by people
//! (its talk tracks, its requests), so it goes under `untrusted_*`, never
//! in the text summary. A step runs with the caller's own permissions, as
//! on the REST route; one whose action destroys something, sends traffic
//! out or replays a request that changes state is confirmed. Variables are
//! never tool inputs: a step fills them from the instance's
//! `RUSTYBIN_TEST_*` environment variables.

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_bool, arg_str, confirm, err, need_str, ok, Confirm};
use crate::mcp::core::CallCtx;
use crate::storyboards::{self, run};

fn runner(ctx: &CallCtx, access: &Access) -> run::Runner {
    run::Runner {
        svc: access.svc(&ctx.app),
        session: access.test_session(),
        principal: access.principal(),
        variables: Default::default(),
    }
}

/// `list_storyboards`: the storyboards the caller may see (of one project
/// with `project`), or one by name with the caller's progress.
pub fn list_storyboards(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let caller = &access.resolved;
    if let Some(name) = arg_str(args, "name") {
        return match storyboards::view_for(&ctx.app, caller, &access.test_session(), name) {
            Ok(v) => {
                let steps = v["steps"].as_array().map(Vec::len).unwrap_or(0);
                let at = v["progress"]["current"].as_u64().unwrap_or(1);
                ok(
                    ctx,
                    format!("Storyboard {name}: {steps} step(s), you are at step {at}"),
                    json!({ "untrusted_storyboard": v }),
                )
            }
            Err(e) => err(e.message()),
        };
    }
    let filter = match crate::rbac::projects::Filter::parse(arg_str(args, "project")) {
        Ok(f) => f,
        Err(e) => return err(e),
    };
    let list = storyboards::list_for(&ctx.app, caller, &filter);
    let names: Vec<&str> = list.iter().filter_map(|s| s["name"].as_str()).collect();
    ok(
        ctx,
        format!("{} storyboard(s): {}", list.len(), names.join(", ")),
        json!({
            "untrusted_storyboards": list,
            "max_storyboards": storyboards::MAX_STORYBOARDS,
            "max_steps": storyboards::MAX_STEPS,
        }),
    )
}

/// `save_storyboard`: `replace` overwrites one of the same name.
pub fn save_storyboard(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n.to_string(),
        Err(e) => return e,
    };
    let replace = arg_bool(args, "replace").unwrap_or(false);
    let mut body = args.clone();
    if let Value::Object(m) = &mut body {
        m.remove("replace");
    }
    let s = match storyboards::from_value(&name, body) {
        Ok(s) => s,
        Err(e) => return err(format!("invalid arguments: {e}")),
    };
    let steps = s.steps.len();
    match storyboards::save_for(&ctx.app, &access.resolved, s, replace) {
        Ok(v) => ok(
            ctx,
            format!("Saved storyboard {name} ({steps} step(s)). Rehearse it with run_storyboard_step, from step 1."),
            json!({ "untrusted_storyboard": v }),
        ),
        Err(e) => err(e.message()),
    }
}

/// `delete_storyboard` (confirmed).
pub async fn delete_storyboard(
    ctx: &CallCtx,
    access: &Access,
    args: &Value,
    params: &Value,
) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    let action = format!("delete the storyboard {name}");
    if let Confirm::Answer(v) = confirm(ctx, "delete_storyboard", args, params, &action).await {
        return v;
    }
    match storyboards::delete_for(&ctx.app, &access.resolved, name) {
        Ok(v) => ok(ctx, format!("Deleted storyboard {name}"), v),
        Err(e) => err(e.message()),
    }
}

/// A result for a tool: the verdict in the summary's fields, the action's
/// own answer (which may repeat what an upstream or a gateway said) under
/// `untrusted_detail`.
fn result_value(mut v: Value) -> Value {
    if let Value::Object(m) = &mut v {
        if let Some(d) = m.remove("detail") {
            m.insert("untrusted_detail".into(), d);
        }
        // The title is the storyboard author's text.
        if let Some(t) = m.remove("title") {
            m.insert("untrusted_title".into(), t);
        }
        // So is a failure's wording when it quotes what was seen.
        if m.get("ok") != Some(&json!(true)) {
            if let Some(s) = m.remove("summary") {
                m.insert("untrusted_summary".into(), s);
            }
        }
    }
    v
}

/// `run_storyboard_step`: one step (counted from 1), as the caller.
pub async fn run_storyboard_step(
    ctx: &CallCtx,
    access: &Access,
    args: &Value,
    params: &Value,
) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n.to_string(),
        Err(e) => return e,
    };
    let Some(index) = args.get("step").and_then(Value::as_u64).map(|n| n as usize) else {
        return err("missing required argument: step (a number, counted from 1)");
    };
    // What the step would do, to ask first when it is of that kind. One
    // the caller may not see is not described: the run answers the 404.
    let step = ctx
        .app
        .storyboards
        .get(&name)
        .filter(|s| storyboards::visible(&access.resolved, s))
        .and_then(|s| index.checked_sub(1).and_then(|i| s.steps.get(i).cloned()));
    if let Some(reason) = step
        .as_ref()
        .and_then(|s| run::confirm_reason(&ctx.app, &s.action))
    {
        if let Confirm::Answer(v) = confirm(ctx, "run_storyboard_step", args, params, &reason).await
        {
            return v;
        }
    }
    match run::run_step(runner(ctx, access), &name, index).await {
        Ok(v) => {
            let verdict = if v["ok"] == json!(true) {
                "ok"
            } else {
                "FAILED (see untrusted_summary)"
            };
            let next = match v["next"].as_u64() {
                Some(n) => format!("Next: step {n}."),
                None => "That was the last step.".to_string(),
            };
            ok(
                ctx,
                format!(
                    "Step {index} of {name} ({}): {verdict}. {next}",
                    v["action"].as_str().unwrap_or("")
                ),
                result_value(v),
            )
        }
        Err(e) => err(e.message()),
    }
}

/// `reset_storyboard`: every step's reset, as the caller (confirmed when
/// one of them is of the kind a step is confirmed for).
pub async fn reset_storyboard(
    ctx: &CallCtx,
    access: &Access,
    args: &Value,
    params: &Value,
) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n.to_string(),
        Err(e) => return e,
    };
    let reasons: Vec<String> = ctx
        .app
        .storyboards
        .get(&name)
        .filter(|s| storyboards::visible(&access.resolved, s))
        .map(|s| {
            s.steps
                .iter()
                .filter_map(|st| st.reset.as_ref())
                .filter_map(|a| run::confirm_reason(&ctx.app, a))
                .collect()
        })
        .unwrap_or_default();
    if !reasons.is_empty() {
        let action = format!("reset the storyboard {name}: {}", reasons.join("; "));
        if let Confirm::Answer(v) = confirm(ctx, "reset_storyboard", args, params, &action).await {
            return v;
        }
    }
    match run::reset(runner(ctx, access), &name).await {
        Ok(mut v) => {
            let n = v["reset"].as_array().map(Vec::len).unwrap_or(0);
            let all_ok = v["ok"] == json!(true);
            if let Some(list) = v["reset"].as_array_mut() {
                for r in list.iter_mut() {
                    *r = result_value(r.take());
                }
            }
            ok(
                ctx,
                format!(
                    "Reset {name}: {n} reset action(s) ran{}, your progress is cleared.",
                    if all_ok { "" } else { ", at least one FAILED" }
                ),
                v,
            )
        }
        Err(e) => err(e.message()),
    }
}
