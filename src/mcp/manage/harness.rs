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
    let saved: Vec<Value> = ctx
        .app
        .harness
        .as_ref()
        .map(|h| {
            h.saved
                .list()
                .into_iter()
                .map(|s| {
                    let mut sc = s.scenario.clone();
                    sc.normalise();
                    json!({
                        "name": s.name,
                        "description": sc.description,
                        "builtin": false,
                        "saved_at": s.saved_at,
                        "summary": sc.summary(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    ok(
        ctx,
        format!(
            "{} built-in scenario(s): {}; {} saved on this node. get_scenario returns one to copy and tune; save_scenario keeps one by name.",
            names.len(),
            names.join(", "),
            saved.len()
        ),
        json!({ "scenarios": scenarios, "saved": saved }),
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
        return match ctx.app.harness.as_ref().and_then(|h| h.saved.get(name)) {
            Some(s) => ok(
                ctx,
                format!(
                    "Saved scenario {name}: {}",
                    s.scenario.description.clone().unwrap_or_default()
                ),
                json!({
                    "name": name,
                    "saved_at": s.saved_at,
                    "scenario": s.scenario,
                    "yaml": serde_yaml::to_string(&s.scenario).unwrap_or_default(),
                }),
            ),
            None => err(format!("no built-in or saved scenario {name:?}")),
        };
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
    if arg_str(args, "cluster_id").is_some() {
        return create_cluster_run(ctx, access, &h, args);
    }
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
    if arg_str(args, "cluster_id").is_some() {
        return get_cluster_run(ctx, &h, args);
    }
    let Some(id) = arg_str(args, "run_id") else {
        return err("run_id is required");
    };
    let Some(run) = h.run(id) else {
        return err(format!("no run {id:?} on this node"));
    };
    let s = run.snapshot();
    let text = format!(
        "Run {} ({}) is {}: {:.1} of {:.0} s, {} sent, {} completed, {} ok, {} errors, {} not sent ({} upstream, {} gateway), p99 {:.2} ms{}{}.",
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
        s["upstream"],
        s["gateway"],
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
    if arg_str(args, "cluster_id").is_some() {
        return stop_cluster_run(ctx, &h, args);
    }
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
    let mut cluster_run = None;
    let local;
    let report = if arg_str(args, "cluster_id").is_some() {
        match cluster_report(&h, args, &mut cluster_run) {
            Ok(r) => r,
            Err(e) => return e,
        }
    } else {
        let Some(run) = h.run(id) else {
            return err(format!("no run {id:?} on this node"));
        };
        local = run;
        let Some(report) = local.report() else {
            return err(format!(
                "run {id} is {}: the report comes when it finishes (poll get_run)",
                local.state().as_str()
            ));
        };
        report
    };
    let format = arg_str(args, "format").unwrap_or("json");
    let structured = match format {
        "text" => json!({ "run_id": id, "text": report.to_text() }),
        "junit" => json!({ "run_id": id, "junit": report.to_junit() }),
        _ => serde_json::to_value(report).unwrap_or(Value::Null),
    };
    let text = format!(
        "Run {} {}: {} sent, {} completed, {} ok, {} errors ({:.2}%), {} not sent; classes: {} upstream, {} gateway{}, {} transport, {} lost; latency p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms{}; {:.1} answers/s of {:.1} requested; {}{}; thresholds {} (exit code {}).",
        report.run_id,
        report.state,
        report.totals.sent,
        report.totals.completed,
        report.totals.ok,
        report.totals.errors(),
        report.totals.error_rate() * 100.0,
        report.totals.not_sent,
        report.totals.upstream,
        report.totals.gateway,
        if report.totals.gateway_statuses.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                report
                    .totals
                    .gateway_statuses
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
        report.totals.transport,
        report.totals.lost,
        report.latency.p50_ms,
        report.latency.p99_ms,
        report.latency.max_ms,
        if report.overhead.count > 0 {
            format!(
                "; gateway overhead p50 {:.2} ms, p99 {:.2} ms (upstream processing p50 {:.2} ms)",
                report.overhead.p50_ms, report.overhead.p99_ms, report.upstream_latency.p50_ms
            )
        } else {
            String::new()
        },
        report.achieved_rate,
        report.requested_rate,
        if report.saturation.saturated {
            "the CLIENT SATURATED (the numbers describe the client, not the gateway)"
        } else {
            "the client was not saturated"
        },
        match &report.server {
            Some(srv) => format!(
                "; server side saw {} arrivals, gateway added {} header name(s), changed {} values, {} bodies, {} paths",
                srv.arrivals,
                srv.changes.headers_added.len(),
                srv.changes.headers_changed,
                srv.changes.body_changed,
                srv.changes.path_changed
            ),
            None => String::new(),
        },
        if report.passed { "pass" } else { "FAIL" },
        report.exit_code
    );
    // The rate-limit analysis: counts and findings the harness computed
    // (no gateway-provided text: node header values stay in the structure).
    let mut text = text;
    for r in &report.rate_limits {
        text.push_str(&format!(
            "\nRate limits of {}: {} admitted, {} refused ({:.1}%), worst window-long span {} admitted{}{}",
            r.layer,
            r.admitted,
            r.limited,
            r.limited_fraction * 100.0,
            r.windows.sliding_max,
            r.over_admission
                .map(|v| format!(", over-admission {:.1}%", v * 100.0))
                .unwrap_or_default(),
            if r.findings.is_empty() {
                String::new()
            } else {
                format!(". {}", r.findings.join(". "))
            }
        ));
    }
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

// ── Clusters (phase 3) ──────────────────────────────────────────────

/// The cluster `cluster_id` names, led by this node. On a member of a
/// cluster another node leads, the error names the leader to call instead.
fn cluster_of(
    h: &Harness,
    args: &Value,
) -> Result<std::sync::Arc<harness::cluster::Cluster>, Value> {
    let Some(id) = arg_str(args, "cluster_id") else {
        return Err(err("cluster_id is required"));
    };
    if let Some(c) = h.coordinator.get(id) {
        return Ok(c);
    }
    if let Some(m) = h.membership().filter(|m| m.cluster_id == id) {
        if let Some(url) = m.url() {
            return Err(err(format!(
                "this node is a member of cluster {id}, not its leader: call the leader's management MCP server (its cluster URL is {url})"
            )));
        }
    }
    Err(err(format!("no cluster {id:?} on this node")))
}

fn run_error_value(e: harness::RunError) -> Value {
    let mut v = err(format!(
        "{} ({}){}",
        e.error,
        e.status,
        if e.problems.is_empty() {
            String::new()
        } else {
            format!(": {}", e.problems.join("; "))
        }
    ));
    v["structuredContent"] = e.to_json();
    v
}

/// `create_cluster`: a cluster led by this node. The join token is in the
/// structured result only, never in the text.
pub async fn create_cluster(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let h = match harness_of(ctx) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let mut body = args.clone();
    if let Value::Object(m) = &mut body {
        m.remove("confirm");
    }
    let req: harness::cluster::CreateCluster = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(e) => return err(format!("invalid arguments: {e}")),
    };
    match harness::create_cluster(&h, req).await {
        Ok((cluster, token)) => {
            let base = access.links.base_url();
            let mut v = cluster.describe();
            v["join_token"] = json!(token);
            v["join_url"] = json!(format!("{base}{PREFIX}/clusters/{}", cluster.id));
            ok(
                ctx,
                format!(
                    "Cluster {} created, led by node {} (lease {} ms). Nodes join with RUSTYBIN_HARNESS_JOIN={base}{PREFIX}/clusters/{} and the join token in structuredContent.join_token (shown once), or POST {PREFIX}/join on each node. Then create_run with cluster_id, and start_run.",
                    cluster.id,
                    cluster.leader,
                    cluster.lease.as_millis(),
                    cluster.id
                ),
                v,
            )
        }
        Err(e) => run_error_value(e),
    }
}

/// `list_cluster_nodes`.
pub fn list_cluster_nodes(ctx: &CallCtx, args: &Value) -> Value {
    let h = match harness_of(ctx) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let c = match cluster_of(&h, args) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let v = c.describe();
    let members = v["members"].as_array().cloned().unwrap_or_default();
    let text = format!(
        "Cluster {} (leader {}, term 1): {} member(s), {} client(s) and {} server(s) alive{}.",
        c.id,
        c.leader,
        members.len(),
        v["alive"]["clients"],
        v["alive"]["servers"],
        members
            .iter()
            .map(|m| format!(
                "; {} [{}] {}",
                m["node_id"].as_str().unwrap_or(""),
                m["roles"]
                    .as_array()
                    .map(|r| r
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", "))
                    .unwrap_or_default(),
                m["health"].as_str().unwrap_or("")
            ))
            .collect::<String>()
    );
    ok(ctx, text, v)
}

/// `export_upstreams`: the server nodes' URLs for the gateway's pool.
pub fn export_upstreams(ctx: &CallCtx, args: &Value) -> Value {
    let h = match harness_of(ctx) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let c = match cluster_of(&h, args) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let ups = c.upstreams();
    ok(
        ctx,
        if ups.is_empty() {
            format!(
                "Cluster {} has no server node with an advertised URL yet.",
                c.id
            )
        } else {
            format!(
                "Point the gateway's upstream pool at the {} server node(s) of cluster {}: {}.",
                ups.len(),
                c.id,
                ups.join(", ")
            )
        },
        json!({ "cluster_id": c.id, "upstreams": ups }),
    )
}

/// `create_run` with `cluster_id`: submit the scenario to the cluster
/// (validated, not started; nothing is sent until `start_run`).
pub fn create_cluster_run(ctx: &CallCtx, access: &Access, h: &Harness, args: &Value) -> Value {
    let c = match cluster_of(h, args) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let scenario = match scenario_arg(args) {
        Ok(s) => s,
        Err(e) => return e,
    };
    match c.create_run(scenario, &h.caps()) {
        Ok(run) => {
            let q = c.quorum(&run.scenario);
            let base = access.links.base_url();
            let mut v = c.run_snapshot(&run);
            v["quorum"] = json!(q);
            v["urls"] =
                json!({ "self": format!("{base}{PREFIX}/clusters/{}/runs/{}", c.id, run.id) });
            ok(
                ctx,
                format!(
                    "Run {} submitted to cluster {} (not started). Quorum: {} of {} client node(s), {} of {} server node(s){}. start_run starts it; it asks the user to confirm.",
                    run.id,
                    c.id,
                    q.clients,
                    q.need_clients,
                    q.servers,
                    q.need_servers,
                    if q.met { ", met" } else { ", waiting" }
                ),
                v,
            )
        }
        Err(e) => run_error_value(e),
    }
}

/// `start_run`: start a cluster run. Sends real traffic: confirmed.
pub async fn start_run(ctx: &CallCtx, args: &Value, params: &Value) -> Value {
    let h = match harness_of(ctx) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let c = match cluster_of(&h, args) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let Some(id) = arg_str(args, "run_id") else {
        return err("run_id is required");
    };
    let Some(run) = c.run(id) else {
        return err(format!("no run {id:?} in cluster {}", c.id));
    };
    let summary = run.scenario.summary();
    let targets: Vec<&str> = run
        .scenario
        .targets
        .iter()
        .map(|t| t.url.as_str())
        .collect();
    let action = format!(
        "start cluster run {} ({}) of cluster {} against {} ({:.0} s, peak {:.0} requests/s over the client nodes)",
        run.id,
        run.scenario.name,
        c.id,
        targets.join(", "),
        summary.duration_secs,
        summary.peak_rate
    );
    match confirm(ctx, "start_run", args, params, &action).await {
        Confirm::Yes => {}
        Confirm::Answer(v) => return v,
    }
    match c.start_run(id) {
        Ok(run) => ok(
            ctx,
            format!(
                "Cluster run {} is starting on every client node at once. Poll get_run (with cluster_id) until the state is finished, then get_run_report.",
                run.id
            ),
            c.run_snapshot(&run),
        ),
        Err(e) => run_error_value(e),
    }
}

/// `get_run` with `cluster_id`.
pub fn get_cluster_run(ctx: &CallCtx, h: &Harness, args: &Value) -> Value {
    let c = match cluster_of(h, args) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let Some(id) = arg_str(args, "run_id") else {
        return err("run_id is required");
    };
    let Some(run) = c.run(id) else {
        return err(format!("no run {id:?} in cluster {}", c.id));
    };
    let mut v = c.run_snapshot(&run);
    v["quorum"] = json!(c.quorum(&run.scenario));
    let text = format!(
        "Cluster run {} ({}) of cluster {} is {}: {} sent, {} completed, {} errors so far over {} client node(s){}.",
        run.id,
        run.scenario.name,
        c.id,
        run.state().as_str(),
        v["sent"],
        v["completed"],
        v["errors"],
        v["nodes"]
            .as_array()
            .map_or(0, |n| n.iter().filter(|x| x["role"] == "client").count()),
        if run.report().is_some() { ", report ready" } else { "" }
    );
    ok(ctx, text, v)
}

/// `stop_run` with `cluster_id`.
pub fn stop_cluster_run(ctx: &CallCtx, h: &Harness, args: &Value) -> Value {
    let c = match cluster_of(h, args) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let Some(id) = arg_str(args, "run_id") else {
        return err("run_id is required");
    };
    match c.stop_run(id, "user") {
        Ok((run, changed)) => {
            ok(
                ctx,
                if changed {
                    format!("Cluster run {id} is stopping on every client node; the merged report follows.")
                } else {
                    format!(
                        "Cluster run {id} is {}: nothing to stop.",
                        run.state().as_str()
                    )
                },
                json!({ "run_id": id, "state": run.state().as_str(), "stopped": changed }),
            )
        }
        Err(e) => run_error_value(e),
    }
}

/// The finished report of a cluster run, if `cluster_id` is given.
pub fn cluster_report<'a>(
    h: &Harness,
    args: &Value,
    keep: &'a mut Option<std::sync::Arc<harness::cluster::ClusterRun>>,
) -> Result<&'a harness::report::Report, Value> {
    let c = cluster_of(h, args)?;
    let Some(id) = arg_str(args, "run_id") else {
        return Err(err("run_id is required"));
    };
    let Some(run) = c.run(id) else {
        return Err(err(format!("no run {id:?} in cluster {}", c.id)));
    };
    c.tick();
    let run = keep.insert(run);
    run.report().ok_or_else(|| {
        err(format!(
            "cluster run {id} is {}: the report comes when it finishes (poll get_run)",
            run.state().as_str()
        ))
    })
}

/// `get_leader`: the cluster's leader as this node sees it. Every member
/// answers (the leader too), so an agent can always find the leader.
pub fn get_leader(ctx: &CallCtx, args: &Value) -> Value {
    let h = match harness_of(ctx) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let Some(id) = arg_str(args, "cluster_id") else {
        return err("cluster_id is required");
    };
    let view = if let Some(c) = h.coordinator.get(id) {
        c.tick();
        c.leader_view()
    } else if let Some(m) = h.membership().filter(|m| m.cluster_id == id) {
        m.leader_view()
    } else {
        return err(format!("this node is not a member of cluster {id:?}"));
    };
    let succession: Vec<String> = view["succession"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| {
                    format!(
                        "{}{} (work {})",
                        s["node_id"].as_str().unwrap_or(""),
                        if s["prefer"] == true { ", standby" } else { "" },
                        s["work_score"]
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let text = format!(
        "Cluster {id}: leader {} (term {}{}), failover {}; next in line: {}.",
        view["leader"]["node_id"].as_str().unwrap_or("unknown"),
        view["term"],
        view["leader"]["control_url"]
            .as_str()
            .map(|u| format!(", at {u}"))
            .unwrap_or_default(),
        view["failover"].as_str().unwrap_or(""),
        if succession.is_empty() {
            "nobody".to_string()
        } else {
            succession.join(", ")
        }
    );
    ok(ctx, text, view)
}

// ── Phase 4: patterns, preview, saved scenarios, live changes ──────

/// `list_patterns`: the pattern types with their parameters, from the
/// scenario schema (so the tool never drifts from the model).
pub fn list_patterns(ctx: &CallCtx) -> Value {
    let caps = match &ctx.app.harness {
        Some(h) => h.caps(),
        None => crate::harness::scenario::Caps::from_config(&ctx.app.config.harness),
    };
    let schema = crate::harness::scenario::schema(&caps);
    let patterns: Vec<Value> = schema["$defs"]["pattern"]["oneOf"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|b| {
            let params: serde_json::Map<String, Value> = b["properties"]
                .as_object()
                .map(|p| {
                    p.iter()
                        .filter(|(k, _)| *k != "type" && *k != "then")
                        .map(|(k, v)| (k.clone(), v["description"].clone()))
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "type": b["title"],
                "description": b["description"],
                "required": b["required"],
                "parameters": params,
            })
        })
        .collect();
    let text = patterns
        .iter()
        .map(|p| {
            format!(
                "{}: {}",
                p["type"].as_str().unwrap_or(""),
                p["description"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    ok(
        ctx,
        format!(
            "{} pattern types (chain them with then:; every one takes jitter and sync):\n{text}",
            patterns.len()
        ),
        json!({ "patterns": patterns }),
    )
}

/// `preview_scenario`: the expected rate per second, nothing sent.
pub fn preview_scenario(ctx: &CallCtx, args: &Value) -> Value {
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
    let mut v = scenario.preview();
    let peak_total = v["total"]
        .as_array()
        .map(|t| t.iter().filter_map(Value::as_f64).fold(0.0, f64::max))
        .unwrap_or(0.0);
    let text = format!(
        "{}: {:.0} s, {} layer(s), the busiest second {:.0} requests, peak rate {:.0} per second{}.",
        scenario.name,
        v["duration_secs"].as_f64().unwrap_or(0.0),
        scenario.layers.len(),
        peak_total,
        v["peak_rate"].as_f64().unwrap_or(0.0),
        if problems.is_empty() {
            String::new()
        } else {
            format!("; {} problem(s): {}", problems.len(), problems.join("; "))
        }
    );
    v["valid"] = json!(problems.is_empty());
    v["problems"] = json!(problems);
    ok(ctx, text, v)
}

/// `save_scenario`: keep a scenario by name on this node.
pub fn save_scenario(ctx: &CallCtx, args: &Value) -> Value {
    let h = match harness_of(ctx) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let Some(name) = arg_str(args, "name") else {
        return err("name is required (letters, digits, . _ -)");
    };
    if harness::builtin_scenarios().iter().any(|(n, _)| *n == name) {
        return err(format!(
            "{name:?} is a built-in scenario: save the copy under another name"
        ));
    }
    let scenario = match scenario_arg(args) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let mut check = scenario.clone();
    check.normalise();
    let problems = check.validate(&h.caps());
    if !problems.is_empty() {
        return err(format!(
            "the scenario has {} problem(s), nothing was saved: {}",
            problems.len(),
            problems.join("; ")
        ));
    }
    match h.saved.put(name, scenario) {
        Ok(created) => ok(
            ctx,
            format!(
                "Scenario {name} {}; list_scenarios shows it, create_run runs it.",
                if created { "saved" } else { "replaced" }
            ),
            json!({ "name": name, "saved": true, "summary": check.summary() }),
        ),
        Err(e) => err(e),
    }
}

/// `update_run`: a live change of a running run (this node's, or a
/// cluster run with `cluster_id`). A change that may send more load asks
/// the user to confirm.
pub async fn update_run(ctx: &CallCtx, args: &Value, params: &Value) -> Value {
    let h = match harness_of(ctx) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let Some(run_id) = arg_str(args, "run_id") else {
        return err("run_id is required");
    };
    let change: crate::harness::live::Change = match args.get("change") {
        Some(v) => match serde_json::from_value(v.clone()) {
            Ok(c) => c,
            Err(e) => return err(format!("change: {e}")),
        },
        None => return err("change is required: rate_multiplier, pause, resume, hold, skip_stage, set, add_layer or remove_layer"),
    };
    if change.may_raise_load() {
        let action = format!("change harness run {run_id}: {}", change.describe());
        match confirm(ctx, "update_run", args, params, &action).await {
            Confirm::Yes => {}
            Confirm::Answer(v) => return v,
        }
    }
    let result = if arg_str(args, "cluster_id").is_some() {
        let c = match cluster_of(&h, args) {
            Ok(c) => c,
            Err(e) => return e,
        };
        c.change_run(run_id, &change).map(|(_, controls)| controls)
    } else {
        match h.run(run_id) {
            Some(run) if run.cluster.is_none() => run
                .apply_change(&change)
                .map_err(crate::harness::RunError::invalid),
            Some(_) => Err(crate::harness::RunError::new(
                409,
                "this run is part of a cluster run: pass its cluster_id on the leader",
            )),
            None => Err(crate::harness::RunError::new(
                404,
                format!("no run {run_id:?} on this node"),
            )),
        }
    };
    match result {
        Ok(controls) => ok(
            ctx,
            format!(
                "Run {run_id} changed ({}); the nodes apply it on their next pass. The run's log has {} change(s).",
                change.describe(),
                controls.log.len()
            ),
            json!({ "run_id": run_id, "controls": controls }),
        ),
        Err(e) => run_error_value(e),
    }
}
