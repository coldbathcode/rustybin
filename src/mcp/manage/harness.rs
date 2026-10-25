//! Harness tools (`docs/plans/harness.md`, section 13, the phase 1 subset):
//! the node status, scenario validation, the built-in scenarios, runs and
//! reports, the kill switch. They call the same [`crate::harness::Harness`]
//! as the REST handlers, so caps, allowlist and verification are identical.
//! `create_run` sends real traffic to the outside world: it is confirmed
//! (elicitation, else `confirm: true`) and carries `openWorldHint`.

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_str, confirm, err, ok, Confirm};
use crate::harness::scenario::Scenario;
use crate::harness::{self, Harness, PREFIX};
use crate::mcp::core::CallCtx;

fn harness_of(ctx: &CallCtx) -> Result<std::sync::Arc<Harness>, Value> {
    ctx.app.harness.clone().ok_or_else(|| {
        err("harness mode is off on this instance: start it with RUSTYBIN_HARNESS=on, RUSTYBIN_HARNESS_ROLES=client (or both) and RUSTYBIN_HARNESS_TARGETS=<allowed hosts>")
    })
}

/// The scenario argument: an object, or JSON / YAML text.
fn scenario_arg(args: &Value) -> Result<Scenario, Value> {
    match args.get("scenario") {
        Some(v @ Value::Object(_)) => Scenario::from_value(v).map_err(err),
        Some(Value::String(text)) => Scenario::parse(text).map_err(err),
        _ => Err(err("scenario is required: an object, or JSON / YAML text")),
    }
}

/// `harness_status`.
pub fn harness_status(ctx: &CallCtx, access: &Access) -> Value {
    let base = access.links.base_url();
    match &ctx.app.harness {
        None => ok(
            ctx,
            "Harness mode is off on this instance (RUSTYBIN_HARNESS=on turns it on; docs/plans/harness.md).",
            json!({
                "enabled": false,
                "caps": crate::harness::scenario::Caps::from_config(&ctx.app.config.harness),
                "urls": { "status": format!("{base}{PREFIX}"), "schema": format!("{base}{PREFIX}/schema") },
            }),
        ),
        Some(h) => {
            let mut status = h.status();
            status["urls"] = json!({
                "status": format!("{base}{PREFIX}"),
                "schema": format!("{base}{PREFIX}/schema"),
                "runs": format!("{base}{PREFIX}/runs"),
                "console": format!("{base}/ui/#/harness"),
            });
            let active = h.active_run();
            let summary = format!(
                "Harness node {} with roles {}: caps {} requests/s, {} in flight, {} s; allowed targets: {}; {}.",
                h.node_id,
                h.config.roles.iter().map(|r| r.as_str()).collect::<Vec<_>>().join(", "),
                h.caps().max_rate,
                h.caps().max_inflight,
                h.caps().max_duration_secs,
                if h.allowlist.is_empty() { "none".to_string() } else { h.allowlist.describe().join(", ") },
                match active {
                    Some(r) => format!("run {} is {}", r.id, r.state().as_str()),
                    None => format!("no active run, {} kept", h.runs().len()),
                }
            );
            ok(ctx, summary, status)
        }
    }
}

/// `validate_scenario`.
pub fn validate_scenario(ctx: &CallCtx, args: &Value) -> Value {
    let mut scenario = match scenario_arg(args) {
        Ok(s) => s,
        Err(e) => return e,
    };
    scenario.normalise();
    let caps = match &ctx.app.harness {
        Some(h) => h.caps(),
        None => crate::harness::scenario::Caps::from_config(&ctx.app.config.harness),
    };
    let problems = scenario.validate(&caps);
    let summary = scenario.summary();
    let text = if problems.is_empty() {
        format!(
            "Scenario {} is valid: {} layer(s), {} target(s), {:.0} s, peak {:.0} requests/s, about {:.0} requests.",
            scenario.name, summary.layers.len(), summary.targets, summary.duration_secs, summary.peak_rate, summary.expected_requests
        )
    } else {
        format!(
            "Scenario {} has {} problem(s): {}",
            scenario.name,
            problems.len(),
            problems.join("; ")
        )
    };
    ok(
        ctx,
        text,
        json!({
            "valid": problems.is_empty(),
            "problems": problems,
            "scenario": scenario,
            "hash": scenario.hash(),
            "summary": summary,
            "caps": caps,
        }),
    )
}

/// `list_scenarios`.
pub fn list_scenarios(ctx: &CallCtx) -> Value {
    let scenarios: Vec<Value> = harness::builtin_scenarios()
        .iter()
        .filter_map(|(name, text)| {
            let mut s = Scenario::parse(text).ok()?;
            s.normalise();
            Some(json!({
                "name": name,
                "description": s.description,
                "builtin": true,
                "summary": s.summary(),
            }))
        })
        .collect();
    let names: Vec<&str> = harness::builtin_scenarios()
        .iter()
        .map(|(n, _)| *n)
        .collect();
    ok(
        ctx,
        format!(
            "{} built-in scenario(s): {}. get_scenario returns one to copy and tune; saved scenarios come in a later phase.",
            names.len(),
            names.join(", ")
        ),
        json!({ "scenarios": scenarios, "saved": [] }),
    )
}

/// `get_scenario`.
pub fn get_scenario(ctx: &CallCtx, args: &Value) -> Value {
    let Some(name) = arg_str(args, "name") else {
        return err("name is required (list_scenarios names them)");
    };
    let Some((_, text)) = harness::builtin_scenarios()
        .iter()
        .find(|(n, _)| *n == name)
    else {
        return err(format!("no built-in scenario {name:?}"));
    };
    let Some(scenario) = harness::builtin_scenario(name) else {
        return err("the built-in scenario does not parse");
    };
    ok(
        ctx,
        format!(
            "Built-in scenario {name}: {}",
            scenario.description.clone().unwrap_or_default()
        ),
        json!({ "name": name, "scenario": scenario, "yaml": text }),
    )
}

/// `create_run`: validate, pin, verify and start. Confirmed: it sends load.
pub async fn create_run(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let h = match harness_of(ctx) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let mut scenario = match scenario_arg(args) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let problems = h.validate(&mut scenario);
    if !problems.is_empty() {
        return err(format!(
            "the scenario has {} problem(s), nothing was started: {}",
            problems.len(),
            problems.join("; ")
        ));
    }
    let summary = scenario.summary();
    let targets: Vec<&str> = scenario.targets.iter().map(|t| t.url.as_str()).collect();
    let action = format!(
        "start harness run {} against {} ({:.0} s, peak {:.0} requests/s, about {:.0} requests)",
        scenario.name,
        targets.join(", "),
        summary.duration_secs,
        summary.peak_rate,
        summary.expected_requests
    );
    match confirm(ctx, "create_run", args, params, &action).await {
        Confirm::Yes => {}
        Confirm::Answer(v) => return v,
    }
    let base = access.links.base_url();
    match h.create_run(scenario).await {
        Ok(run) => ok(
            ctx,
            format!(
                "Run {} started: {} for {:.0} s, peak {:.0} requests/s. Poll get_run until report_ready, then get_run_report. stop_run stops it early.",
                run.id, run.scenario.name, summary.duration_secs, summary.peak_rate
            ),
            harness::engine::with_links(run.snapshot(), &format!("{base}{PREFIX}"), &run.id),
        ),
        Err(e) => {
            let mut v = err(format!(
                "{} ({}){}",
                e.error,
                e.status,
                if e.problems.is_empty() { String::new() } else { format!(": {}", e.problems.join("; ")) }
            ));
            v["structuredContent"] = e.to_json();
            v
        }
    }
}

/// `list_runs`.
pub fn list_runs(ctx: &CallCtx) -> Value {
    let h = match harness_of(ctx) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let runs: Vec<Value> = h.runs().iter().rev().map(|r| r.summary()).collect();
    ok(
        ctx,
        format!(
            "{} run(s) on node {}{}.",
            runs.len(),
            h.node_id,
            h.active_run()
                .map(|r| format!(", {} is {}", r.id, r.state().as_str()))
                .unwrap_or_default()
        ),
        json!({ "runs": runs, "active": h.active_run().map(|r| r.id.clone()) }),
    )
}

/// `get_run`.
pub fn get_run(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let h = match harness_of(ctx) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let Some(id) = arg_str(args, "run_id") else {
        return err("run_id is required");
    };
    let Some(run) = h.run(id) else {
        return err(format!("no run {id:?} on this node"));
    };
    let s = run.snapshot();
    let text = format!(
        "Run {} ({}) is {}: {:.1} of {:.0} s, {} sent, {} completed, {} ok, {} errors, {} not sent, p99 {:.2} ms{}{}.",
        run.id,
        run.scenario.name,
        run.state().as_str(),
        s["elapsed_secs"].as_f64().unwrap_or(0.0),
        s["planned_secs"].as_f64().unwrap_or(0.0),
        s["sent"],
        s["completed"],
        s["ok"],
        s["errors"],
        s["not_sent"],
        s["latency"]["p99_ms"].as_f64().unwrap_or(0.0),
        if s["saturated"] == true { ", the client is saturated" } else { "" },
        if s["report_ready"] == true { ", report ready" } else { "" }
    );
    let base = access.links.base_url();
    ok(
        ctx,
        text,
        harness::engine::with_links(s, &format!("{base}{PREFIX}"), &run.id),
    )
}

/// `stop_run`.
pub fn stop_run(ctx: &CallCtx, args: &Value) -> Value {
    let h = match harness_of(ctx) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let Some(id) = arg_str(args, "run_id") else {
        return err("run_id is required");
    };
    let Some(run) = h.run(id) else {
        return err(format!("no run {id:?} on this node"));
    };
    if !run.is_active() {
        return ok(
            ctx,
            format!("Run {id} had already finished."),
            json!({ "run_id": id, "state": "finished", "stopped": false }),
        );
    }
    run.stop("user");
    ok(
        ctx,
        format!("Run {id} is stopping: it drains its requests and then reports (get_run_report)."),
        json!({ "run_id": id, "state": run.state().as_str(), "stopped": true }),
    )
}

/// `get_run_report`.
pub fn get_run_report(ctx: &CallCtx, args: &Value) -> Value {
    let h = match harness_of(ctx) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let Some(id) = arg_str(args, "run_id") else {
        return err("run_id is required");
    };
    let Some(run) = h.run(id) else {
        return err(format!("no run {id:?} on this node"));
    };
    let Some(report) = run.report() else {
        return err(format!(
            "run {id} is {}: the report comes when it finishes (poll get_run)",
            run.state().as_str()
        ));
    };
    let format = arg_str(args, "format").unwrap_or("json");
    let structured = match format {
        "text" => json!({ "run_id": id, "text": report.to_text() }),
        "junit" => json!({ "run_id": id, "junit": report.to_junit() }),
        _ => serde_json::to_value(report).unwrap_or(Value::Null),
    };
    let text = format!(
        "Run {} {}: {} sent, {} completed, {} ok, {} errors ({:.2}%), {} not sent; latency p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms; {:.1} answers/s of {:.1} requested; {}; thresholds {} (exit code {}).",
        report.run_id,
        report.state,
        report.totals.sent,
        report.totals.completed,
        report.totals.ok,
        report.totals.errors(),
        report.totals.error_rate() * 100.0,
        report.totals.not_sent,
        report.latency.p50_ms,
        report.latency.p99_ms,
        report.latency.max_ms,
        report.achieved_rate,
        report.requested_rate,
        if report.saturation.saturated {
            "the CLIENT SATURATED (the numbers describe the client, not the gateway)"
        } else {
            "the client was not saturated"
        },
        if report.passed { "pass" } else { "FAIL" },
        report.exit_code
    );
    ok(ctx, text, structured)
}

/// `harness_stop`: the kill switch. No confirmation: it stops load.
pub fn harness_stop(ctx: &CallCtx) -> Value {
    let h = match harness_of(ctx) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let stopped = h.stop_all();
    ok(
        ctx,
        if stopped.is_empty() {
            "No run was active; nothing to stop.".to_string()
        } else {
            format!("Stopped {} run(s): {}.", stopped.len(), stopped.join(", "))
        },
        json!({ "stopped": stopped, "kill_switch": true }),
    )
}
