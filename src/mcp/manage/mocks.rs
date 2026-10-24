//! OpenAPI mock tools, over `crate::mocks::service::Svc` (the same logic as
//! the REST handlers).

use serde_json::{json, Map, Value};

use super::access::Access;
use super::tools::{
    arg_bool, arg_str, arg_u64, arg_usize, confirm, err, need_str, ok, service_error,
    untrusted_headers, Budget, Confirm,
};
use crate::mcp::core::CallCtx;
use crate::mocks::capture::MockCapture;
use crate::mocks::model::OperationSettings;
use crate::mocks::service::{CreateBody, PatchBody, RuleTestBody, SampleQuery, Svc, ValidateBody};
use crate::state::AppState;

fn svc(ctx: &CallCtx, access: &Access) -> Svc {
    access.svc(&ctx.app)
}

/// `structuredContent` for a mock detail plus a summary line.
fn mock_summary(detail: &Value, verb: &str) -> String {
    let ops = detail["operations"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    let report = &detail["report"];
    format!(
        "{verb} mock {:?} ({}, {} operation(s), state {}, checks: {} error(s), {} warning(s)): {}{}",
        detail["name"].as_str().unwrap_or("?"),
        detail["title"].as_str().unwrap_or(""),
        ops,
        detail["state"].as_str().unwrap_or("?"),
        report["errors"],
        report["warnings"],
        detail["url"].as_str().unwrap_or(""),
        if detail.get("manage_token").is_some() {
            "; a visit started: keep manage_token, it is shown once"
        } else {
            ""
        }
    )
}

/// `list_mocks`.
pub fn list_mocks(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let s = svc(ctx, access);
    let all = arg_bool(args, "all").unwrap_or(false);
    let query = arg_str(args, "query").map(str::to_lowercase);
    let state_filter = arg_str(args, "state");
    let mut listing = s.list(all);
    let mocks: Vec<Value> = listing["mocks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|m| {
            state_filter.is_none_or(|st| m["state"].as_str() == Some(st))
                && query.as_deref().is_none_or(|q| {
                    format!(
                        "{} {} {}",
                        m["name"].as_str().unwrap_or(""),
                        m["title"].as_str().unwrap_or(""),
                        m["url"].as_str().unwrap_or("")
                    )
                    .to_lowercase()
                    .contains(q)
                })
        })
        .map(|m| {
            json!({
                "name": m["name"], "title": m["title"], "version": m["version"],
                "url": m["url"], "host_url": m.get("host_url"), "state": m["state"],
                "enabled": m["enabled"], "has_draft_changes": m["has_draft_changes"],
                "origin": m["origin"], "owner": m["owner"], "yours": m["yours"],
                "operations": m["operations"], "security": m["security"], "mode": m["mode"],
                "requests_total": m["requests_total"], "requests_last_hour": m["requests_last_hour"],
                "last_hit": m["last_hit"], "live_revision": m["live_revision"],
                "draft_revision": m["draft_revision"], "check": m["check"],
                "expires_at": m["expires_at"], "load_error": m["load_error"],
            })
        })
        .collect();
    let names: Vec<&str> = mocks.iter().filter_map(|m| m["name"].as_str()).collect();
    let summary = if mocks.is_empty() {
        "No mocks match (create one with create_mock, or preview_spec first).".to_string()
    } else {
        format!(
            "{} mock(s): {}",
            mocks.len(),
            names
                .iter()
                .take(20)
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    if let Value::Object(m) = &mut listing {
        m.insert("count".into(), json!(mocks.len()));
        m.insert("mocks".into(), Value::Array(mocks));
        m.remove("load_errors");
    }
    ok(ctx, summary, listing)
}

/// `get_mock`.
pub fn get_mock(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    match svc(ctx, access).get(name) {
        Ok(d) => {
            let changes = d["draft_changes"].as_array().map(|a| a.len()).unwrap_or(0);
            let mut summary = mock_summary(&d, "Mock");
            if changes > 0 {
                summary.push_str(&format!(
                    "; the draft has {changes} undeployed change(s) (deploy_mock)"
                ));
            }
            ok(ctx, summary, d)
        }
        Err(e) => service_error(&e),
    }
}

fn spec_arg(args: &Value) -> Result<Value, Value> {
    match args.get("spec") {
        Some(v @ Value::String(_)) | Some(v @ Value::Object(_)) => Ok(v.clone()),
        _ => Err(err(
            "spec must be the document text (JSON or YAML) or a JSON object",
        )),
    }
}

/// `preview_spec`.
pub fn preview_spec(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let spec = match spec_arg(args) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let text = match crate::mocks::service::spec_text(&spec) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    match svc(ctx, access).preview(&text) {
        Ok(p) => {
            let ops = p["operations"].as_array().map(|a| a.len()).unwrap_or(0);
            let summary = format!(
                "{} {} (OpenAPI {}): {ops} operation(s), suggested name {:?}, security schemes {}, \
                 checks {} error(s) and {} warning(s), {} spec warning(s)",
                p["title"].as_str().unwrap_or("Untitled"),
                p["version"].as_str().unwrap_or(""),
                p["openapi"].as_str().unwrap_or("?"),
                p["suggested_name"].as_str().unwrap_or(""),
                p["security_schemes"]
                    .as_object()
                    .map(|m| m.keys().cloned().collect::<Vec<_>>().join(", "))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "none".into()),
                p["report"]["errors"],
                p["report"]["warnings"],
                p["warnings"].as_array().map(|a| a.len()).unwrap_or(0)
            );
            ok(ctx, summary, p)
        }
        Err(e) => service_error(&e),
    }
}

/// `get_mock_spec`.
pub fn get_mock_spec(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    let s = svc(ctx, access);
    if arg_bool(args, "served").unwrap_or(false) {
        return match s.readable(name) {
            Ok(snap) => {
                let c = snap.live.as_ref().unwrap_or(&snap.draft).clone();
                let root = s.links.mock_base(&snap.owner, name);
                let server = format!("{root}{}", c.path_prefix());
                let doc =
                    crate::mocks::handlers::served_spec(&c, &server, s.links.base_url(), name);
                ok(
                    ctx,
                    format!("The OpenAPI document mock {name:?} serves (server URL {server})"),
                    json!({ "name": name, "served": true, "server": server, "spec": doc }),
                )
            }
            Err(e) => service_error(&e),
        };
    }
    match s.spec_source(name) {
        Ok((text, json_format)) => ok(
            ctx,
            format!(
                "The spec of mock {name:?} as uploaded ({} bytes, {}); edit it and send it back with update_mock",
                text.len(),
                if json_format { "JSON" } else { "YAML" }
            ),
            json!({ "name": name, "format": if json_format { "json" } else { "yaml" }, "spec": text }),
        ),
        Err(e) => service_error(&e),
    }
}

fn settings_from<T: serde::de::DeserializeOwned>(
    args: &Value,
    key: &str,
) -> Result<Option<T>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|e| err(format!("invalid {key}: {e}"))),
    }
}

/// `create_mock`.
pub fn create_mock(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let spec = match spec_arg(args) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let body = CreateBody {
        name: arg_str(args, "name").map(str::to_string),
        spec,
        enabled: arg_bool(args, "enabled"),
        deploy: arg_bool(args, "deploy"),
        force: arg_bool(args, "force"),
        strip_server_prefix: arg_bool(args, "strip_server_prefix"),
        responses: match settings_from(args, "responses") {
            Ok(v) => v,
            Err(e) => return e,
        },
        security: match settings_from(args, "security") {
            Ok(v) => v,
            Err(e) => return e,
        },
        operations: match settings_from(args, "operations") {
            Ok(v) => v,
            Err(e) => return e,
        },
        mcp: match settings_from(args, "mcp") {
            Ok(v) => v,
            Err(e) => return e,
        },
    };
    match svc(ctx, access).create(body) {
        Ok(d) => {
            let summary = mock_summary(&d, "Created");
            ok(ctx, summary, d)
        }
        Err(e) => service_error(&e),
    }
}

/// `update_mock`.
pub fn update_mock(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    let spec = match args.get("spec") {
        None | Some(Value::Null) => None,
        Some(_) => match spec_arg(args) {
            Ok(s) => Some(s),
            Err(e) => return e,
        },
    };
    let body = PatchBody {
        name: arg_str(args, "rename").map(str::to_string),
        spec,
        strip_server_prefix: arg_bool(args, "strip_server_prefix"),
        responses: match settings_from(args, "responses") {
            Ok(v) => v,
            Err(e) => return e,
        },
        security: match settings_from(args, "security") {
            Ok(v) => v,
            Err(e) => return e,
        },
        operations: match settings_from(args, "operations") {
            Ok(v) => v,
            Err(e) => return e,
        },
        mcp: match settings_from(args, "mcp") {
            Ok(v) => v,
            Err(e) => return e,
        },
        deploy: arg_bool(args, "deploy"),
        force: arg_bool(args, "force"),
    };
    let nothing = body.name.is_none()
        && body.spec.is_none()
        && body.strip_server_prefix.is_none()
        && body.responses.is_none()
        && body.security.is_none()
        && body.operations.is_none()
        && body.mcp.is_none()
        && body.deploy.is_none();
    if nothing {
        return err("nothing to change: give spec, responses, security, operations, mcp, strip_server_prefix, rename or deploy");
    }
    let deployed = body.deploy == Some(true);
    match svc(ctx, access).update(name, body) {
        Ok(d) => {
            let mut summary = mock_summary(&d, "Updated");
            if deployed {
                summary.push_str("; deployed");
            } else if d["has_draft_changes"].as_bool() == Some(true) {
                summary.push_str("; the draft is not deployed yet (deploy_mock)");
            }
            ok(ctx, summary, d)
        }
        Err(e) => service_error(&e),
    }
}

/// `set_operation`.
pub fn set_operation(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let (name, op) = match (need_str(args, "name"), need_str(args, "operation")) {
        (Ok(n), Ok(o)) => (n, o),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let settings: Option<OperationSettings> = if arg_bool(args, "clear").unwrap_or(false) {
        None
    } else {
        match args.get("settings") {
            Some(v @ Value::Object(_)) => match serde_json::from_value(v.clone()) {
                Ok(s) => Some(s),
                Err(e) => return err(format!("invalid operation settings: {e}")),
            },
            _ => return err("give settings (an object) or clear: true"),
        }
    };
    match svc(ctx, access).set_operation(name, op, settings) {
        Ok(d) => ok(
            ctx,
            format!(
                "Operation {op} of mock {name:?} updated in the draft (mode {}, {} rule(s)); deploy_mock publishes it. Sample URL: {}",
                d["mode"].as_str().unwrap_or("?"),
                d["settings"]["rules"].as_array().map(|a| a.len()).unwrap_or(0),
                d["url"].as_str().unwrap_or("")
            ),
            d,
        ),
        Err(e) => service_error(&e),
    }
}

/// `test_rules`.
pub fn test_rules(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let (name, op) = match (need_str(args, "name"), need_str(args, "operation")) {
        (Ok(n), Ok(o)) => (n, o),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let rules = match settings_from(args, "rules") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let body = RuleTestBody {
        rules,
        request: arg_u64(args, "request"),
    };
    match svc(ctx, access).test_rules(name, op, body) {
        Ok(r) => {
            let summary = match (r["request"].is_null(), r["winner"].as_u64()) {
                (true, _) => {
                    format!("No captured call of {op} yet: send one (try_request) and test again")
                }
                (false, Some(i)) => format!(
                    "Rule {i} ({}) would answer request #{} of {op}",
                    r["rules"][i as usize]["name"].as_str().unwrap_or("unnamed"),
                    r["request"]["seq"]
                ),
                (false, None) => format!(
                    "No rule matches request #{} of {op}: the default response would answer",
                    r["request"]["seq"]
                ),
            };
            ok(ctx, summary, r)
        }
        Err(e) => service_error(&e),
    }
}

/// `check_mock`.
pub fn check_mock(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    match svc(ctx, access).report(name) {
        Ok(r) => {
            let summary = format!(
                "Deploy checks of mock {name:?}: {} ({} error(s), {} warning(s), {} spec warning(s))",
                if r["ok"].as_bool() == Some(true) { "ok" } else { "errors" },
                r["errors"],
                r["warnings"],
                r["spec_warnings"].as_array().map(|a| a.len()).unwrap_or(0)
            );
            ok(ctx, summary, r)
        }
        Err(e) => service_error(&e),
    }
}

/// `deploy_mock`.
pub fn deploy_mock(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    match svc(ctx, access).deploy(name, arg_bool(args, "force").unwrap_or(false)) {
        Ok(r) => ok(
            ctx,
            format!(
                "Deployed mock {name:?} as revision {} ({} error(s), {} warning(s))",
                r["revision"], r["report"]["errors"], r["report"]["warnings"]
            ),
            r,
        ),
        Err(e) => service_error(&e),
    }
}

/// `rollback_mock` (confirmed).
pub async fn rollback_mock(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    let revision = arg_u64(args, "revision");
    let action = match revision {
        Some(r) => format!("roll mock {name} back to revision {r}"),
        None => format!("roll mock {name} back to its previous revision"),
    };
    if let Confirm::Answer(v) = confirm(ctx, "rollback_mock", args, params, &action).await {
        return v;
    }
    match svc(ctx, access).rollback(name, revision) {
        Ok(r) => ok(
            ctx,
            format!(
                "Mock {name:?} rolled back: revision {} is live",
                r["revision"]
            ),
            r,
        ),
        Err(e) => service_error(&e),
    }
}

/// `set_mock_enabled`.
pub fn set_mock_enabled(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    let Some(on) = arg_bool(args, "enabled") else {
        return err("missing required argument: enabled (true or false)");
    };
    match svc(ctx, access).set_enabled(name, on) {
        Ok(r) => ok(
            ctx,
            format!(
                "Mock {name:?} is now {}",
                if on { "on" } else { "off (answers 503)" }
            ),
            r,
        ),
        Err(e) => service_error(&e),
    }
}

/// `duplicate_mock`.
pub fn duplicate_mock(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    match svc(ctx, access).duplicate(name, arg_str(args, "new_name").map(str::to_string)) {
        Ok(d) => {
            let summary = mock_summary(&d, "Duplicated into");
            ok(ctx, summary, d)
        }
        Err(e) => service_error(&e),
    }
}

/// `delete_mock` (confirmed).
pub async fn delete_mock(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    let action = format!("delete mock {name} with its revisions, stored data and traffic");
    if let Confirm::Answer(v) = confirm(ctx, "delete_mock", args, params, &action).await {
        return v;
    }
    match svc(ctx, access).delete(name) {
        Ok(r) => ok(ctx, format!("Deleted mock {name:?}"), r),
        Err(e) => service_error(&e),
    }
}

/// `sample_request`.
pub fn sample_request(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let (name, op) = match (need_str(args, "name"), need_str(args, "operation")) {
        (Ok(n), Ok(o)) => (n, o),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let mode = match settings_from(args, "mode") {
        Ok(m) => m,
        Err(e) => return e,
    };
    let q = SampleQuery {
        status: arg_u64(args, "status").map(|s| s as u16),
        seed: arg_u64(args, "seed"),
        mode,
        example: arg_str(args, "example").map(str::to_string),
    };
    match svc(ctx, access).sample(name, op, &q) {
        Ok(mut r) => {
            let request = r["request"].clone();
            let url = r["url"].as_str().unwrap_or("").to_string();
            let method = request["method"].as_str().unwrap_or("GET").to_string();
            let mut curl = format!("curl -s -X {method} '{url}'");
            if let Some(hs) = request["headers"].as_array() {
                for h in hs {
                    if let (Some(n), Some(v)) = (h["name"].as_str(), h["value"].as_str()) {
                        curl.push_str(&format!(" -H '{n}: {v}'"));
                    }
                }
            }
            if let Some(b) = request.get("body").filter(|b| !b.is_null()) {
                let text = match b {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                curl.push_str(&format!(" --data '{}'", text.replace('\'', "'\\''")));
            }
            r["curl"] = json!(curl);
            ok(
                ctx,
                format!(
                    "{method} {url} would answer {} ({}, source {}, {})",
                    r["response"]["status"],
                    r["response"]["content_type"].as_str().unwrap_or("no body"),
                    r["response"]["source"].as_str().unwrap_or("?"),
                    if r["response"]["valid"].as_bool() == Some(true) {
                        "valid against the schema"
                    } else {
                        "with schema issues"
                    }
                ),
                r,
            )
        }
        Err(e) => service_error(&e),
    }
}

/// `validate_payload`.
pub fn validate_payload(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let (name, op) = match (need_str(args, "name"), need_str(args, "operation")) {
        (Ok(n), Ok(o)) => (n, o),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let Some(body) = args.get("body").cloned() else {
        return err("missing required argument: body");
    };
    let req = ValidateBody {
        kind: arg_str(args, "kind").map(str::to_string),
        status: arg_u64(args, "status").map(|s| s as u16),
        body,
    };
    match svc(ctx, access).validate(name, op, &req) {
        Ok(r) => {
            let issues = r["issues"].as_array().map(|a| a.len()).unwrap_or(0);
            let summary = if r["has_schema"].as_bool() != Some(true) {
                format!(
                    "Operation {op} declares no schema for this {}: nothing to check",
                    req.kind.as_deref().unwrap_or("response")
                )
            } else if issues == 0 {
                format!(
                    "The {} body is valid against {op}'s schema",
                    req.kind.as_deref().unwrap_or("response")
                )
            } else {
                format!(
                    "The {} body breaks {op}'s schema in {issues} place(s): {}",
                    req.kind.as_deref().unwrap_or("response"),
                    r["issues"]
                        .as_array()
                        .map(|a| a
                            .iter()
                            .take(5)
                            .map(|i| format!(
                                "{} {}",
                                i["pointer"].as_str().unwrap_or(""),
                                i["message"].as_str().unwrap_or("")
                            ))
                            .collect::<Vec<_>>()
                            .join("; "))
                        .unwrap_or_default()
                )
            };
            ok(ctx, summary, r)
        }
        Err(e) => service_error(&e),
    }
}

/// `export_collection`.
pub fn export_collection(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let (name, format) = match (need_str(args, "name"), need_str(args, "format")) {
        (Ok(n), Ok(f)) => (n, f),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    match svc(ctx, access).collection(name, format, arg_str(args, "base_url")) {
        Ok((ct, file, text)) => ok(
            ctx,
            format!(
                "{format} collection of mock {name:?}: {} bytes, suggested file name {file}",
                text.len()
            ),
            json!({ "name": name, "format": format, "content_type": ct, "file_name": file, "content": text }),
        ),
        Err(e) => service_error(&e),
    }
}

/// `get_mock_data`.
pub fn get_mock_data(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    match svc(ctx, access).state_dump(name) {
        Ok(d) => {
            let collections = d.as_object().map(|m| m.len()).unwrap_or(0);
            ok(
                ctx,
                format!(
                    "Mock {name:?} stores {collections} collection(s) of items (stateful mode)"
                ),
                json!({ "name": name, "untrusted_data": d }),
            )
        }
        Err(e) => service_error(&e),
    }
}

/// `clear_mock_data` (confirmed).
pub async fn clear_mock_data(
    ctx: &CallCtx,
    access: &Access,
    args: &Value,
    params: &Value,
) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    let action = format!("delete everything mock {name} stored");
    if let Confirm::Answer(v) = confirm(ctx, "clear_mock_data", args, params, &action).await {
        return v;
    }
    match svc(ctx, access).clear_state(name) {
        Ok(r) => ok(ctx, format!("Cleared the stored data of mock {name:?}"), r),
        Err(e) => service_error(&e),
    }
}

/// A captured mock call as a result row: metadata in the clear, headers
/// and bodies under `untrusted_*`.
pub fn capture_row(c: &MockCapture, include_bodies: bool, budget: &mut Budget) -> Value {
    let mut row = json!({
        "seq": c.seq,
        "timestamp": c.timestamp,
        "method": c.method,
        "path": c.path,
        "query": c.query,
        "status": c.status,
        "outcome": c.outcome,
        "operation": c.operation,
        "source": c.source,
        "auth": c.auth,
        "validation_issues": c.validation.len(),
        "validation": c.validation,
        "duration_ms": c.duration_ms,
        "body_bytes": c.body_size,
        "client_ip": c.client_ip,
        "untrusted_headers": untrusted_headers(&c.headers.iter().map(|h| (h.name.clone(), h.value.clone())).collect::<Vec<_>>()),
        "untrusted_query": c.query,
    });
    if include_bodies {
        if let Some(body) = c.body.as_deref().or(c.body_base64.as_deref()) {
            if let Some((cut, truncated)) = budget.body(body) {
                row["untrusted_body"] = json!(cut);
                row["body_truncated"] = json!(truncated || c.body_truncated);
                row["body_base64"] = json!(c.body.is_none());
            }
        }
        if let Some(resp) = c.response_body.as_deref() {
            if let Some((cut, truncated)) = budget.body(resp) {
                row["untrusted_response_body"] = json!(cut);
                row["response_truncated"] = json!(truncated || c.response_truncated);
            }
        }
    }
    row
}

/// `get_mock_traffic`.
pub fn get_mock_traffic(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    let limit = arg_usize(args, "limit").unwrap_or(20).clamp(1, 500);
    let include_bodies = arg_bool(args, "include_bodies").unwrap_or(false);
    let s = svc(ctx, access);
    let snap = match s.readable(name) {
        Ok(s) => s,
        Err(e) => return service_error(&e),
    };
    let limit = limit.min(ctx.app.mocks.limits().captures);
    let entries = snap.capture.entries(limit);
    let mut budget = Budget::new();
    let rows: Vec<Value> = entries
        .iter()
        .map(|c| capture_row(c, include_bodies, &mut budget))
        .collect();
    let mut by_outcome: Map<String, Value> = Map::new();
    for c in &entries {
        let n = by_outcome.entry(c.outcome.to_string()).or_insert(json!(0));
        *n = json!(n.as_u64().unwrap_or(0) + 1);
    }
    let summary = format!(
        "Mock {name:?} received {} call(s) in total; showing the latest {}{}. Outcomes: {}. Request content is under untrusted_* fields.",
        snap.capture.total(),
        rows.len(),
        if include_bodies { " with bodies" } else { " (metadata only; include_bodies for bodies)" },
        by_outcome
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    ok(
        ctx,
        summary,
        json!({
            "mock": name,
            "total": snap.capture.total(),
            "count": rows.len(),
            "by_outcome": by_outcome,
            "result_truncated": budget.truncated,
            "requests": rows,
        }),
    )
}

/// `clear_traffic` (confirmed).
pub async fn clear_traffic(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    let action = format!("delete the captured traffic of mock {name}");
    if let Confirm::Answer(v) = confirm(ctx, "clear_traffic", args, params, &action).await {
        return v;
    }
    match svc(ctx, access).clear_requests(name) {
        Ok(r) => ok(
            ctx,
            format!("Cleared the captured traffic of mock {name:?}"),
            r,
        ),
        Err(e) => service_error(&e),
    }
}

/// `export_config`.
pub fn export_config(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let s = svc(ctx, access);
    match arg_str(args, "name").filter(|n| !n.is_empty()) {
        Some(name) => match s.export_one(name) {
            Ok(text) => ok(
                ctx,
                format!(
                    "rustybin.yaml for mock {name:?} ({} bytes){}",
                    text.len(),
                    secrets_note(&s, &ctx.app, Some(name))
                ),
                json!({ "file_name": format!("{name}.rustybin.yaml"), "yaml": text }),
            ),
            Err(e) => service_error(&e),
        },
        None => {
            let text = s.export_all();
            ok(
                ctx,
                format!(
                    "rustybin.yaml for every mock the caller may see ({} bytes){}",
                    text.len(),
                    secrets_note(&s, &ctx.app, None)
                ),
                json!({ "file_name": "rustybin.yaml", "yaml": text }),
            )
        }
    }
}

fn secrets_note(s: &Svc, _state: &AppState, name: Option<&str>) -> &'static str {
    let reveal = match name {
        Some(n) => s
            .readable(n)
            .map(|snap| s.caller.reveals(&snap))
            .unwrap_or(false),
        None => s.caller.admin,
    };
    if reveal {
        ""
    } else {
        "; security secrets are redacted (placeholders) because the caller may not change these mocks"
    }
}

/// `apply_config` (admin; `prune` confirmed).
pub async fn apply_config(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let yaml = match need_str(args, "yaml") {
        Ok(y) => y,
        Err(e) => return e,
    };
    let dry_run = arg_bool(args, "dry_run").unwrap_or(false);
    let prune = arg_bool(args, "prune").unwrap_or(false);
    let s = svc(ctx, access);
    if dry_run {
        return match s.apply_dry_run(yaml, prune) {
            Ok(r) => ok(
                ctx,
                format!(
                    "Dry run: would create {}, update {} and delete {} mock(s); {} error(s) in the document",
                    r["would_create"].as_array().map(|a| a.len()).unwrap_or(0),
                    r["would_update"].as_array().map(|a| a.len()).unwrap_or(0),
                    r["would_delete"].as_array().map(|a| a.len()).unwrap_or(0),
                    r["errors"].as_array().map(|a| a.len()).unwrap_or(0)
                ),
                r,
            ),
            Err(e) => service_error(&e),
        };
    }
    if prune {
        let action =
            "apply the document and delete every API-created mock it does not name (prune)";
        if let Confirm::Answer(v) = confirm(ctx, "apply_config", args, params, action).await {
            return v;
        }
    }
    match s.apply(yaml, prune) {
        Ok(r) => ok(
            ctx,
            format!(
                "Applied: created {}, updated {}, deleted {} mock(s); {} error(s)",
                r["created"].as_array().map(|a| a.len()).unwrap_or(0),
                r["updated"].as_array().map(|a| a.len()).unwrap_or(0),
                r["deleted"].as_array().map(|a| a.len()).unwrap_or(0),
                r["errors"].as_array().map(|a| a.len()).unwrap_or(0)
            ),
            r,
        ),
        Err(e) => service_error(&e),
    }
}
