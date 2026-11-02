//! Saved test tools (`crate::replay`): the same functions as the
//! `/_rustybin/tests*` handlers. Tests come from captured traffic, so their
//! requests and the answers a run saw go under `untrusted_*`, never in the
//! text summary. Variables are never tool inputs: a run fills them from the
//! instance's `RUSTYBIN_TEST_*` environment variables.

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_bool, arg_str, confirm, err, need_str, ok, Confirm};
use crate::mcp::core::CallCtx;
use crate::replay::{self, run, SaveBody};

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
    match run::run(&ctx.app, &access.test_session(), body).await {
        Ok(r) => {
            let failing: Vec<&str> = r
                .results
                .iter()
                .filter(|t| !t.passed)
                .map(|t| t.name.as_str())
                .collect();
            let summary = if r.passed {
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
            let mut v = json!({
                "passed": r.passed, "total": r.total, "failed": r.failed, "errors": r.errors,
                "duration_ms": r.duration_ms, "ran_at": r.ran_at,
                "untrusted_results": r.results,
            });
            if arg_str(args, "format") == Some("junit") {
                v["untrusted_junit"] = json!(run::junit(&r));
            }
            ok(ctx, summary, v)
        }
        Err((_, m)) => err(m),
    }
}
