//! AI assist tools: the provider's status and usage, a test call, the read
//! tasks, and the AI-built mocks (a spec from a description, examples for
//! the operations that lack one, an edit through an instruction). Every
//! model call is an assist task with the user's own key; keys are never
//! settable here.

use std::sync::Arc;

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_bool, arg_str, arg_u64, confirm, err, need_str, ok, truncate, Confirm};
use crate::assist::{api as assist_api, task, RunParams};
use crate::mcp::core::CallCtx;
use crate::mocks::ai;

/// `assist_status`: whether AI assist is on, the provider and models, the
/// caps, the providers' key state (`{set, hint}`, never a value), the tasks.
pub fn assist_status(ctx: &CallCtx, _access: &Access, _args: &Value) -> Value {
    let v = assist_api::status_json(&ctx.app);
    let summary = if v["enabled"].as_bool() == Some(true) {
        format!(
            "AI assist is on: provider {}, model {} (fast {}), {} calls per minute per caller; {} task(s)",
            v["provider"].as_str().unwrap_or("?"),
            v["model"].as_str().unwrap_or("?"),
            v["fast_model"].as_str().unwrap_or("?"),
            v["caps"]["calls_per_minute"],
            v["tasks"].as_array().map(Vec::len).unwrap_or(0)
        )
    } else {
        format!(
            "AI assist is off: {}. The operator picks a provider and sets its key in the console (Management, AI) or with `rustybin settings set-secret`; an agent never sets keys.",
            v["reason"].as_str().unwrap_or("no provider")
        )
    };
    ok(ctx, summary, v)
}

/// `assist_usage` (admin): calls, tokens and the estimated cost since start.
pub fn assist_usage(ctx: &CallCtx, _access: &Access, _args: &Value) -> Value {
    let v = ctx.app.assist.usage();
    let t = &v["total"];
    ok(
        ctx,
        format!(
            "{} AI call(s) since start ({} error(s)), {} input and {} output tokens{}",
            t["calls"],
            t["errors"],
            t["input_tokens"],
            t["output_tokens"],
            t["estimated_cost_usd"]
                .as_f64()
                .map(|c| format!(", about {c} USD at list prices"))
                .unwrap_or_default()
        ),
        v,
    )
}

/// `test_assist` (admin): one small call to the configured provider.
pub async fn test_assist(ctx: &CallCtx, _access: &Access, _args: &Value) -> Value {
    match ctx.app.assist.test().await {
        Ok(v) => ok(
            ctx,
            format!(
                "The provider answered: {} {} in {} ms",
                v["provider"].as_str().unwrap_or("?"),
                v["model"].as_str().unwrap_or("?"),
                v["ms"].as_u64().unwrap_or(0)
            ),
            v,
        ),
        Err(e) => {
            let status = ctx.app.assist.status();
            let mut v = err(format!("the provider test failed: {e}"));
            v["structuredContent"] = json!({
                "ok": false,
                "error": e,
                "reason": if status.enabled { "provider_error" } else { "assist_disabled" },
                "provider": status.kind.map(|k| k.as_str()),
                "model": status.model,
            });
            v
        }
    }
}

fn refusal(r: &crate::assist::Refusal) -> Value {
    let (status, message, reason) = assist_api::refusal_parts(r);
    let mut v = err(format!("{message} ({})", status.as_u16()));
    v["structuredContent"] =
        json!({ "error": message, "reason": reason, "status": status.as_u16() });
    if let crate::assist::Refusal::RateLimited(wait) = r {
        v["structuredContent"]["retry_after"] = json!(wait);
    }
    v
}

/// `run_assist_task`: a read task (`free_answer`, `explain`, `suggest`,
/// ...) with its inputs; the writing tasks are their own tools.
pub async fn run_assist_task(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "task") {
        Ok(t) => t,
        Err(e) => return e,
    };
    let Some(task) = task::find(name) else {
        return err(format!(
            "no task named {name:?}; the read tasks are {}",
            task::all()
                .iter()
                .filter(|t| !t.writes)
                .map(|t| t.name)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    let own_tool = match name {
        "spec_from_description" => Some("generate_spec"),
        "examples_for_operations" => Some("generate_examples"),
        "edit_spec" => Some("edit_spec_with_ai"),
        _ => None,
    };
    if let Some(tool) = own_tool.or(task.writes.then_some("the mock tools")) {
        return err(format!(
            "{name} builds a mock: use {tool} instead (it checks the answer against the mock and asks the user before writing)"
        ));
    }
    let inputs = args.get("inputs").cloned().unwrap_or_else(|| json!({}));
    let stream = match Arc::clone(&ctx.app.assist).run(RunParams {
        task,
        inputs,
        model: arg_str(args, "model").map(str::to_string),
        principal: access.principal(),
    }) {
        Ok(s) => s,
        Err(r) => return refusal(&r),
    };
    match assist_api::collect(task, stream).await {
        Ok(v) => {
            let summary = match v["text"].as_str() {
                Some(text) if !text.trim().is_empty() => {
                    let (cut, more) = truncate(text.trim(), 400);
                    format!(
                        "{name} answered ({} {}): {cut}{}",
                        v["usage"]["provider"].as_str().unwrap_or("?"),
                        v["usage"]["model"].as_str().unwrap_or("?"),
                        if more { " ..." } else { "" }
                    )
                }
                _ => format!(
                    "{name} answered with a structured result ({} {}){}",
                    v["usage"]["provider"].as_str().unwrap_or("?"),
                    v["usage"]["model"].as_str().unwrap_or("?"),
                    if v["actions"].as_array().is_some_and(|a| !a.is_empty()) {
                        "; it proposes actions (apply them with the mock tools after the user agrees)"
                    } else {
                        ""
                    }
                ),
            };
            ok(ctx, summary, v)
        }
        Err(e) => {
            let mut v = err(format!(
                "{name} failed: {}",
                e["error"].as_str().unwrap_or("provider error")
            ));
            v["structuredContent"] = e;
            v
        }
    }
}

/// A failed work as a tool error.
fn work_error(what: &str, e: Value) -> Value {
    let mut v = err(format!(
        "{what}: {}{}",
        e["error"].as_str().unwrap_or("failed"),
        e["reason"]
            .as_str()
            .map(|r| format!(" ({r})"))
            .unwrap_or_default()
    ));
    v["structuredContent"] = e;
    v
}

fn progress() -> ai::Progress {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    tx
}

fn preview_line(preview: &Value) -> String {
    format!(
        "{} {} (OpenAPI {}): {} operation(s), checks {} error(s) and {} warning(s)",
        preview["title"].as_str().unwrap_or("Untitled"),
        preview["version"].as_str().unwrap_or(""),
        preview["openapi"].as_str().unwrap_or("?"),
        preview["operations"].as_array().map(Vec::len).unwrap_or(0),
        preview["report"]["errors"],
        preview["report"]["warnings"]
    )
}

/// `generate_spec` (manage): an OpenAPI document from a description,
/// checked like an upload; with `create` (confirmed) the mock is created
/// from it at once.
pub async fn generate_spec(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let description = match need_str(args, "description") {
        Ok(d) => d,
        Err(e) => return e,
    };
    let create = arg_bool(args, "create").unwrap_or(false);
    let name = arg_str(args, "name").filter(|n| !n.is_empty());
    if create {
        let action = format!(
            "generate an OpenAPI spec with the AI provider for {:?} and create {} from it",
            truncate(description, 120).0,
            name.map(|n| format!("the mock {n}"))
                .unwrap_or_else(|| "a mock".to_string())
        );
        if let Confirm::Answer(v) = confirm(ctx, "generate_spec", args, params, &action).await {
            return v;
        }
    }
    let req = ai::SpecBody {
        description: description.to_string(),
        openapi_version: arg_str(args, "openapi_version").map(str::to_string),
        style: args.get("style").cloned().filter(|s| !s.is_null()),
        model: arg_str(args, "model").map(str::to_string),
        stream: None,
    };
    let svc = access.svc(&ctx.app);
    let mut result =
        match ai::spec_work(ctx.app.clone(), svc, req, access.principal(), progress()).await {
            Ok(v) => v,
            Err(e) => return work_error("the spec could not be generated", e),
        };
    let line = preview_line(&result["preview"]);
    if !create {
        return ok(
            ctx,
            format!(
                "Generated {line}; suggested name {:?}. Nothing was created: show it to the user, then create_mock with the text (or generate_spec with create true).",
                result["preview"]["suggested_name"].as_str().unwrap_or("")
            ),
            result,
        );
    }
    let mut create_args = json!({ "spec": result["text"].clone() });
    if let Some(n) = name {
        create_args["name"] = json!(n);
    }
    for key in ["deploy", "enabled", "force"] {
        if let Some(v) = args.get(key) {
            create_args[key] = v.clone();
        }
    }
    let created = super::mocks::create_mock(ctx, access, &create_args);
    if created["isError"].as_bool() == Some(true) {
        let mut v = created;
        result["created"] = Value::Null;
        v["structuredContent"] =
            json!({ "generated": result, "create_error": v["structuredContent"].take() });
        return v;
    }
    let detail = created["structuredContent"].clone();
    let summary = format!(
        "Generated {line}, and created mock {:?} at {}{}",
        detail["name"].as_str().unwrap_or("?"),
        detail["url"].as_str().unwrap_or("?"),
        if detail["live"].is_null() {
            " (a draft: deploy_mock publishes it)"
        } else {
            ""
        }
    );
    result["mock"] = detail;
    ok(ctx, summary, result)
}

/// `generate_examples` (manage): example payloads for the operations that
/// lack one (or the ones named), previewed, or stored in the draft with
/// `apply` (confirmed).
pub async fn generate_examples(
    ctx: &CallCtx,
    access: &Access,
    args: &Value,
    params: &Value,
) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    let apply = arg_bool(args, "apply").unwrap_or(false);
    let source = arg_str(args, "source").unwrap_or("ai");
    if !matches!(source, "ai" | "generated") {
        return err(
            "source must be ai (the provider) or generated (the deterministic generator, no key)",
        );
    }
    if apply {
        let action = format!(
            "store {} examples in the draft of mock {name} (deploy_mock publishes them)",
            if source == "ai" {
                "AI-written"
            } else {
                "generated"
            }
        );
        if let Confirm::Answer(v) = confirm(ctx, "generate_examples", args, params, &action).await {
            return v;
        }
    }
    let operations = match args.get("operations") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>(),
        ),
        Some(Value::String(s)) => Some(
            s.split(',')
                .map(str::trim)
                .filter(|o| !o.is_empty())
                .map(str::to_string)
                .collect(),
        ),
        Some(_) => return err("operations must be a list of operationIds"),
    };
    let req = ai::ExamplesBody {
        operations,
        per_response: arg_u64(args, "per_response"),
        persona: arg_str(args, "persona").map(str::to_string),
        source: Some(source.to_string()),
        apply: Some(apply),
        model: arg_str(args, "model").map(str::to_string),
        stream: None,
    };
    let svc = access.svc(&ctx.app);
    match ai::examples_work(
        ctx.app.clone(),
        svc,
        name.to_string(),
        req,
        access.principal(),
        progress(),
    )
    .await
    {
        Ok(v) => {
            let ops = v["examples"].as_object().map(|m| m.len()).unwrap_or(0);
            let summary = if v["applied"].as_bool() == Some(true) {
                format!(
                    "Stored {} example(s) for {ops} operation(s) in the draft of mock {name} (revision {}); {} operation(s) still without examples. deploy_mock publishes them.",
                    v["stored"],
                    v["draft_revision"],
                    v["coverage"]["without"].as_array().map(Vec::len).unwrap_or(0)
                )
            } else {
                format!(
                    "Examples for {ops} operation(s) of mock {name} (source {}), not stored: show them to the user, then call again with apply true.",
                    v["source"].as_str().unwrap_or(source)
                )
            };
            ok(ctx, summary, v)
        }
        Err(e) => work_error("examples could not be generated", e),
    }
}

/// `edit_spec_with_ai` (manage): a mock's draft spec edited through an
/// instruction: the new document, the diff and its checks; `apply`
/// (confirmed) saves it as the draft.
pub async fn edit_spec_with_ai(
    ctx: &CallCtx,
    access: &Access,
    args: &Value,
    params: &Value,
) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    let instruction = match need_str(args, "instruction") {
        Ok(i) => i,
        Err(e) => return e,
    };
    let apply = arg_bool(args, "apply").unwrap_or(false);
    if apply {
        let action = format!(
            "edit the spec of mock {name} with the AI provider ({:?}) and save the result as its draft",
            truncate(instruction, 120).0
        );
        if let Confirm::Answer(v) = confirm(ctx, "edit_spec_with_ai", args, params, &action).await {
            return v;
        }
    }
    let req = ai::EditBody {
        instruction: instruction.to_string(),
        apply: Some(apply),
        model: arg_str(args, "model").map(str::to_string),
        stream: None,
    };
    let svc = access.svc(&ctx.app);
    match ai::edit_work(
        ctx.app.clone(),
        svc,
        name.to_string(),
        req,
        access.principal(),
        progress(),
    )
    .await
    {
        Ok(v) => {
            let line = preview_line(&v["preview"]);
            let diff = &v["diff"];
            let diff_line = diff
                .as_str()
                .map(str::to_string)
                .or_else(|| {
                    diff.as_object().map(|m| {
                        m.iter()
                            .filter(|(_, v)| {
                                v.as_array().is_some_and(|a| !a.is_empty())
                                    || v.as_u64().is_some_and(|n| n > 0)
                            })
                            .map(|(k, v)| match v.as_array() {
                                Some(a) => format!("{k} {}", a.len()),
                                None => format!("{k} {v}"),
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                })
                .unwrap_or_default();
            let summary = format!(
                "{}: {line}{}{}",
                if v["applied"].as_bool() == Some(true) {
                    format!(
                        "Edited and saved as the draft of mock {name} (deploy_mock publishes it)"
                    )
                } else {
                    format!("Edit of mock {name} prepared, not saved: show the diff to the user, then call again with apply true")
                },
                if diff_line.is_empty() {
                    String::new()
                } else {
                    format!("; diff: {diff_line}")
                },
                if v["preview"]["report"]["errors"].as_u64().unwrap_or(0) > 0 {
                    "; the new document has check errors"
                } else {
                    ""
                }
            );
            ok(ctx, summary, v)
        }
        Err(e) => work_error("the spec could not be edited", e),
    }
}
