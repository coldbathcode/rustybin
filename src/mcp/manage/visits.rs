//! Self-service visit tools: the caller's own visit, and the operator
//! routes (admin).

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_bool, arg_str, confirm, err, ok, service_error, Confirm};
use crate::mcp::core::CallCtx;
use crate::mocks::selfservice as ss;
use crate::mocks::visits::EmailInput;

fn visit_summary(v: &Value) -> String {
    format!(
        "Visit: {} min left (ends {}), {} of {} min used today, {} mock(s); next extension: {}{}",
        v["expires_in_secs"].as_i64().unwrap_or(0).max(0) / 60,
        v["expires_at"].as_str().unwrap_or("?"),
        v["minutes_used"],
        v["minutes_allowed"],
        v["mocks"].as_array().map(|a| a.len()).unwrap_or(0),
        v["next_extension"].as_str().unwrap_or("?"),
        if v["extendable"].as_bool() == Some(true) {
            " (Extend is open now)"
        } else {
            ""
        }
    )
}

/// `get_visit`.
pub fn get_visit(ctx: &CallCtx, access: &Access) -> Value {
    match ss::visit_view(&ctx.app, &access.caller) {
        Ok(v) => {
            let summary = visit_summary(&v);
            ok(ctx, summary, v)
        }
        Err(e) => service_error(&e),
    }
}

/// `extend_visit` (never with the email step: that stays in the console).
pub fn extend_visit(ctx: &CallCtx, access: &Access) -> Value {
    match ss::extend(&ctx.app, &access.caller, &EmailInput::default()) {
        Ok(v) => {
            let summary = format!("Extended. {}", visit_summary(&v));
            ok(ctx, summary, v)
        }
        Err(e) if e.code() == Some("rustybin_email_required") => {
            let console = format!("{}/ui/#/mocks", access.links.base_url());
            let mut structured = e.body.clone();
            structured["console_url"] = json!(console);
            let mut v = err(format!(
                "{} The email step (with its consent choice and privacy notice) is done in the console: {console}",
                e.message()
            ));
            v["structuredContent"] = structured;
            v
        }
        Err(e) => service_error(&e),
    }
}

/// `list_visits` (operator).
pub fn list_visits(ctx: &CallCtx, access: &Access) -> Value {
    match ss::list_all(&ctx.app, &access.caller) {
        Ok(v) => ok(
            ctx,
            format!(
                "{} running visit(s), {} visitor mock(s); self-service {}",
                v["count"],
                v["visitor_mocks"],
                if v["paused"].as_bool() == Some(true) {
                    "paused"
                } else {
                    "running"
                }
            ),
            v,
        ),
        Err(e) => service_error(&e),
    }
}

/// `end_visit` (operator, confirmed): one visit by id, or all.
pub async fn end_visit(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let all = arg_bool(args, "all").unwrap_or(false);
    let id = arg_str(args, "id").filter(|i| !i.is_empty());
    if !all && id.is_none() {
        return err("give id (one visit) or all: true");
    }
    let action = match id {
        Some(i) if !all => format!("end visit {i} and delete its mocks"),
        _ => "end every visit and delete all visitor mocks".to_string(),
    };
    if let Confirm::Answer(v) = confirm(ctx, "end_visit", args, params, &action).await {
        return v;
    }
    let result = match id {
        Some(i) if !all => ss::end_one(&ctx.app, &access.caller, i),
        _ => ss::end_all(&ctx.app, &access.caller),
    };
    match result {
        Ok(v) => ok(
            ctx,
            format!(
                "Ended; {} mock(s) deleted",
                v["deleted"].as_array().map(|a| a.len()).unwrap_or(0)
            ),
            v,
        ),
        Err(e) => service_error(&e),
    }
}

/// `pause_self_service` / `resume_self_service` (operator).
pub async fn set_paused(
    ctx: &CallCtx,
    access: &Access,
    args: &Value,
    params: &Value,
    paused: bool,
) -> Value {
    if paused {
        let action = "pause self-service: no new visits or visitor mocks until resumed";
        if let Confirm::Answer(v) = confirm(ctx, "pause_self_service", args, params, action).await {
            return v;
        }
    }
    match ss::set_paused(&ctx.app, &access.caller, paused) {
        Ok(v) => ok(
            ctx,
            if paused {
                "Self-service paused: no new visits or visitor mocks until resume_self_service"
            } else {
                "Self-service resumed"
            },
            v,
        ),
        Err(e) => service_error(&e),
    }
}
