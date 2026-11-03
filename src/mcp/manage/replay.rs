//! Saved test tools (`crate::replay`): the same functions as the
//! `/_rustybin/tests*` handlers. Tests come from captured traffic, so their
//! requests and the answers a run saw go under `untrusted_*`, never in the
//! text summary. Variables are never tool inputs: a run fills them from the
//! instance's `RUSTYBIN_TEST_*` environment variables.

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_bool, arg_str, confirm, err, need_str, ok, Confirm};
use crate::mcp::core::CallCtx;
use crate::replay::{self, history, run, SaveBody};

/// `list_tests`: every saved test, one by name, or the Hurl export.
pub fn list_tests(ctx: &CallCtx, args: &Value) -> Value {
    let store = &ctx.app.saved_tests;
    if arg_str(args, "format") == Some("hurl") {
        let text = replay::hurl::export(&store.list());
        return ok(
            ctx,
            format!("{} saved test(s) as Hurl (in untrusted_hurl)", store.len()),
            json!({ "untrusted_hurl": text }),
        );
    }
    if let Some(name) = arg_str(args, "name") {
        return match store.view_of(name) {
            Some(v) => ok(
                ctx,
                format!("Saved test {name}"),
                json!({ "untrusted_test": v }),
            ),
            None => err(format!("no test named {name:?}")),
        };
    }
    let tests = store.views();
    let names: Vec<&str> = tests.iter().filter_map(|t| t["name"].as_str()).collect();
    ok(
        ctx,
        format!("{} saved test(s): {}", tests.len(), names.join(", ")),
        json!({ "untrusted_tests": tests, "max_tests": replay::MAX_TESTS }),
    )
}

/// `save_test`: from a capture or a request; `replace` overwrites one of
/// the same name.
pub fn save_test(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let mut body = args.clone();
    let replace = arg_bool(args, "replace").unwrap_or(false);
    if let Value::Object(m) = &mut body {
        m.remove("replace");
    }
    if replace {
        let name = match need_str(args, "name") {
            Ok(n) => n.to_string(),
            Err(e) => return e,
        };
        if args.get("capture").is_some() {
            return err(
                "replace takes a request, not a capture: save the capture under a new name",
            );
        }
        let test = match replay::test_from_value(&name, body) {
            Ok(t) => t,
            Err(e) => return err(e),
        };
        return match ctx.app.saved_tests.put(test, true) {
            Ok(v) => ok(
                ctx,
                format!("Saved test {name}"),
                json!({ "untrusted_test": v }),
            ),
            Err(e) => err(e.message()),
        };
    }
    let body: SaveBody = match serde_json::from_value(body) {
        Ok(b) => b,
        Err(e) => return err(format!("invalid arguments: {e}")),
    };
    let name = body.name.clone();
    match replay::save(
        &ctx.app,
        body,
        &access.test_session(),
        access.key_id.as_deref(),
    ) {
        Ok(v) => {
            let vars = v["introduced_variables"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let note = if vars.is_empty() {
                String::new()
            } else {
                format!(" Its credentials became the variables {vars}: set RUSTYBIN_TEST_<NAME> on the instance (or pass them on the CLI) to run it.")
            };
            ok(
                ctx,
                format!("Saved test {name}.{note}"),
                json!({ "untrusted_test": v }),
            )
        }
        Err((_, m)) => err(m),
    }
}

/// `delete_test` (confirmed).
pub async fn delete_test(ctx: &CallCtx, args: &Value, params: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    if let Confirm::Answer(v) = confirm(
        ctx,
        "delete_test",
        args,
        params,
        &format!("delete the saved test {name}"),
    )
    .await
    {
        return v;
    }
    match ctx.app.saved_tests.delete(name) {
        Ok(true) => ok(
            ctx,
            format!("Deleted test {name}"),
            json!({ "deleted": name }),
        ),
        Ok(false) => err(format!("no test named {name:?}")),
        Err(e) => err(e.message()),
    }
}

/// Methods a run sends without asking: they read, never change.
const SAFE_METHODS: &[&str] = &["GET", "HEAD", "OPTIONS"];

/// `run_tests`: confirmed when a test goes through a gateway or sends a
/// method that changes something (`POST`, `PUT`, `PATCH`, `DELETE`: the
/// mocks' stateful data, a flaky counter, a gateway's upstream); one run at
/// a time.
pub async fn run_tests(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let names: Vec<String> = args
        .get("names")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let selected: Vec<replay::SavedTest> = ctx
        .app
        .saved_tests
        .list()
        .into_iter()
        .filter(|t| names.is_empty() || names.contains(&t.name))
        .collect();
    let gateways: Vec<String> = selected
        .iter()
        .filter_map(|t| t.gateway.as_deref().map(crate::egress::url_origin))
        .collect();
    let mut writes: Vec<&str> = selected
        .iter()
        .filter(|t| {
            !SAFE_METHODS
                .iter()
                .any(|m| m.eq_ignore_ascii_case(t.request.method.trim()))
        })
        .map(|t| t.name.as_str())
        .collect();
    writes.truncate(10);
    let mut what = Vec::new();
    if !gateways.is_empty() {
        what.push(format!(
            "send saved test requests through {}",
            gateways.join(", ")
        ));
    }
    if !writes.is_empty() {
        what.push(format!(
            "replay requests that change state (tests {})",
            writes.join(", ")
        ));
    }
    if !what.is_empty() {
        let action = what.join(" and ");
        if let Confirm::Answer(v) = confirm(ctx, "run_tests", args, params, &action).await {
            return v;
        }
    }
    let Some(_running) = ctx.app.saved_tests.begin_run() else {
        return err(replay::RUN_BUSY);
    };
    let body = run::RunBody {
        names,
        variables: Default::default(),
    };
    match replay::run_and_keep(
        &ctx.app,
        &access.test_session(),
        body,
        "mcp",
        &access.principal(),
    )
    .await
    {
        Ok(kept) => {
            let summary = format!("{} Kept as run {}.", run_summary(&kept), kept.id);
            ok(ctx, summary, run_value(&kept, arg_str(args, "format")))
        }
        Err((_, m)) => err(m),
    }
}

/// A run in a line: the counts, the failing tests and the change.
fn run_summary(kept: &history::TestRun) -> String {
    let r = &kept.report;
    let failing: Vec<&str> = r
        .results
        .iter()
        .filter(|t| !t.passed)
        .map(|t| t.name.as_str())
        .collect();
    let mut s = if r.passed {
        format!("All {} test(s) passed.", r.total)
    } else {
        format!(
            "{} of {} test(s) failed and {} could not run: {}. The details are in untrusted_results.",
            r.failed,
            r.total,
            r.errors,
            failing.join(", ")
        )
    };
    let c = &kept.change;
    if !c.newly_failing.is_empty() {
        s.push_str(&format!(
            " Newly failing since the previous run: {}.",
            c.newly_failing.join(", ")
        ));
    }
    if !c.newly_passing.is_empty() {
        s.push_str(&format!(" Newly passing: {}.", c.newly_passing.join(", ")));
    }
    s
}

/// A run as the tools answer it: what the tests saw under `untrusted_*`.
fn run_value(kept: &history::TestRun, format: Option<&str>) -> Value {
    let r = &kept.report;
    let mut v = json!({
        "run_id": kept.id, "trigger": kept.trigger, "principal": kept.principal,
        "selection": kept.selection,
        "passed": r.passed, "total": r.total, "failed": r.failed, "errors": r.errors,
        "duration_ms": r.duration_ms, "ran_at": r.ran_at,
        "change": kept.change,
        "untrusted_results": r.results,
    });
    if format == Some("junit") {
        v["untrusted_junit"] = json!(run::junit(r));
    }
    v
}

/// `list_test_runs`: the history, newest first.
pub fn list_test_runs(ctx: &CallCtx, args: &Value) -> Value {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|n| n as usize);
    let v = replay::runs_page(&ctx.app, limit, arg_str(args, "cursor"));
    let n = v["runs"].as_array().map(Vec::len).unwrap_or(0);
    let failed = v["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["passed"] != json!(true))
        .count();
    let more = if v["next_cursor"].is_string() {
        " More with cursor next_cursor."
    } else {
        ""
    };
    ok(
        ctx,
        format!(
            "{n} of {} kept test run(s), newest first; {failed} of them did not pass.{more}",
            v["total"]
        ),
        v,
    )
}

/// `get_test_run`: one kept run, whole (JUnit with `format: junit`).
pub fn get_test_run(ctx: &CallCtx, args: &Value) -> Value {
    let id = match need_str(args, "id") {
        Ok(n) => n,
        Err(e) => return e,
    };
    match ctx.app.saved_tests.history.get(id) {
        Some(kept) => ok(
            ctx,
            format!("Test run {}: {}", kept.id, run_summary(&kept)),
            run_value(&kept, arg_str(args, "format")),
        ),
        None => err(format!("no test run {id:?}: list_test_runs names them")),
    }
}

/// `clear_test_runs` (confirmed): forget every kept run.
pub async fn clear_test_runs(ctx: &CallCtx, args: &Value, params: &Value) -> Value {
    let n = ctx.app.saved_tests.history.len();
    if let Confirm::Answer(v) = confirm(
        ctx,
        "clear_test_runs",
        args,
        params,
        &format!("forget the {n} kept test run(s)"),
    )
    .await
    {
        return v;
    }
    let cleared = ctx.app.saved_tests.history.clear();
    ok(
        ctx,
        format!("Forgot {cleared} test run(s)"),
        json!({ "cleared": cleared }),
    )
}
