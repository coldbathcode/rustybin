//! Gateway scenario tools (`crate::scenarios`): the same functions as the
//! `/_rustybin/scenarios*` handlers. A run belongs to the caller's session
//! (`X-Rustybin-Session`, else the client address), as on the REST routes.

use serde_json::{json, Value};

use super::access::{Access, Right};
use super::tools::{arg_u64, confirm, err, need_str, ok, Confirm};
use crate::mcp::core::CallCtx;
use crate::scenarios::{self, CheckBody};

fn owner(ctx: &CallCtx) -> String {
    let ip = ctx.http.client_ip.as_deref().and_then(|s| s.parse().ok());
    crate::session::session_key(&ctx.http.headers, ip)
}

fn ids(args: &Value) -> Result<(&str, &str), Value> {
    Ok((need_str(args, "scenario")?, need_str(args, "run")?))
}

/// Does the caller see a run's credential-injection key (admin rights)?
fn reveals(access: &Access) -> bool {
    access.allows(Right::Admin)
}

/// `list_gateway_scenarios`.
pub fn list_gateway_scenarios(ctx: &CallCtx, access: &Access) -> Value {
    let v = scenarios::list(&ctx.app, &owner(ctx), reveals(access));
    let runs = v["runs"].as_array().map(Vec::len).unwrap_or(0);
    ok(
        ctx,
        format!(
            "{} gateway scenarios; {runs} run(s) of yours. Start one with start_gateway_scenario.",
            scenarios::SCENARIOS.len()
        ),
        v,
    )
}

/// `start_gateway_scenario`.
pub async fn start_gateway_scenario(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let id = match need_str(args, "scenario") {
        Ok(i) => i,
        Err(e) => return e,
    };
    match scenarios::start(&ctx.app, access.svc(&ctx.app), &owner(ctx), id).await {
        Ok(v) => {
            let steps = v["setup"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .enumerate()
                .map(|(i, s)| format!("{}. {s}", i + 1))
                .collect::<Vec<_>>()
                .join("\n");
            ok(
                ctx,
                format!(
                    "Run {} of {} started. On the gateway:\n{steps}\nThen call check_gateway_scenario with the gateway URL.",
                    v["id"].as_str().unwrap_or("?"),
                    id
                ),
                v,
            )
        }
        Err(e) => err(e.1),
    }
}

/// `get_gateway_scenario_run`.
pub fn get_gateway_scenario_run(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let (id, run) = match ids(args) {
        Ok(x) => x,
        Err(e) => return e,
    };
    match scenarios::get_run(&ctx.app, &owner(ctx), id, run, reveals(access)) {
        Ok(v) => ok(ctx, format!("Run {run} of {id}"), v),
        Err(e) => err(e.1),
    }
}

/// `check_gateway_scenario`: sends test traffic to the gateway (confirmed).
pub async fn check_gateway_scenario(ctx: &CallCtx, args: &Value, params: &Value) -> Value {
    let (id, run) = match ids(args) {
        Ok(x) => x,
        Err(e) => return e,
    };
    let gateway = match need_str(args, "gateway_url") {
        Ok(g) => g,
        Err(e) => return e,
    };
    let action = format!("send the {id} scenario's test requests to {gateway}");
    if let Confirm::Answer(v) = confirm(ctx, "check_gateway_scenario", args, params, &action).await
    {
        return v;
    }
    let body = CheckBody {
        gateway_url: gateway.to_string(),
        limit: arg_u64(args, "limit").map(|l| l.min(u32::MAX as u64) as u32),
    };
    match scenarios::check(&ctx.app, &owner(ctx), id, run, body).await {
        Ok(v) => {
            let lines = v["expectations"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| {
                    format!(
                        "{} {}: expected {}, saw {}",
                        if e["ok"] == true { "ok  " } else { "FAIL" },
                        e["name"].as_str().unwrap_or(""),
                        e["expected"].as_str().unwrap_or(""),
                        e["seen"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let verdict = if v["passed"] == true {
                "passed"
            } else {
                "failed"
            };
            ok(ctx, format!("The {id} scenario {verdict}.\n{lines}"), v)
        }
        Err(e) => err(e.1),
    }
}

/// `end_gateway_scenario`: removes what the run created (confirmed).
pub async fn end_gateway_scenario(ctx: &CallCtx, args: &Value, params: &Value) -> Value {
    let (id, run) = match ids(args) {
        Ok(x) => x,
        Err(e) => return e,
    };
    let action = format!("end run {run} of {id} and remove the mock and counters it created");
    if let Confirm::Answer(v) = confirm(ctx, "end_gateway_scenario", args, params, &action).await {
        return v;
    }
    match scenarios::end(&ctx.app, &owner(ctx), id, run) {
        Ok(v) => ok(ctx, format!("Ended run {run} of {id}"), v),
        Err(e) => err(e.1),
    }
}

/// The input schema of the run tools.
pub fn run_props() -> Value {
    json!({
        "scenario": { "type": "string", "description": "The scenario id (list_gateway_scenarios)" },
        "run": { "type": "string", "description": "The run id (start_gateway_scenario)" }
    })
}
