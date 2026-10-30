//! Mock MCP registry tools: the entries and the scenario, browsing a real
//! registry through the outbound policy, validating a `server.json`,
//! removing a published entry.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_str, arg_u64, confirm, err, need_str, ok, truncate, Confirm};
use crate::mcp::core::CallCtx;
use crate::mcp::registry::{self, browse, validate, Scenario, SCENARIOS};

/// `list_registry_servers`: every entry of the mock registry under the
/// current scenario (generated from this instance's servers, the file's,
/// the published ones), each with its validation, plus the scenarios and a
/// gateway's sync configuration.
pub fn list_registry_servers(ctx: &CallCtx, access: &Access, _args: &Value) -> Value {
    let v = registry::status_json(&ctx.app, &ctx.app.registry, access.links.base_url());
    let summary = format!(
        "Mock registry at {}: {} entr{} (generated {}, file {}, published {}), scenario {}",
        v["base_url"].as_str().unwrap_or("/mcp-registry"),
        v["entries"].as_array().map(Vec::len).unwrap_or(0),
        if v["entries"].as_array().map(Vec::len) == Some(1) {
            "y"
        } else {
            "ies"
        },
        v["counts"]["generated"],
        v["counts"]["file"],
        v["counts"]["published"],
        v["scenario"].as_str().unwrap_or("normal")
    );
    ok(ctx, summary, v)
}

/// `set_registry_scenario` (admin): how the mock registry misbehaves.
pub fn set_registry_scenario(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let raw = match need_str(args, "scenario") {
        Ok(s) => s,
        Err(e) => return e,
    };
    let Some(scenario) = Scenario::parse(raw) else {
        return err(format!(
            "unknown scenario {raw:?}: one of {}",
            SCENARIOS
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    let mut values = BTreeMap::new();
    values.insert("registry.scenario".to_string(), json!(scenario.id()));
    match ctx
        .app
        .settings
        .update(values, BTreeMap::new(), &access.principal())
    {
        Ok(_) => {
            let title = SCENARIOS
                .iter()
                .find(|(id, _)| *id == scenario.id())
                .map(|(_, t)| *t)
                .unwrap_or("");
            ok(
                ctx,
                format!("Registry scenario set to {} ({title})", scenario.id()),
                json!({ "scenario": scenario.id(), "title": title, "scenarios": SCENARIOS.iter().map(|(id, t)| json!({ "id": id, "title": t })).collect::<Vec<_>>() }),
            )
        }
        Err(e) => super::settings::update_error(e),
    }
}

/// `browse_registry`: a page of a real registry (the official one by
/// default) through the outbound policy. Confirmed when the registry is
/// outside this instance. Entries are third-party data (`untrusted_servers`).
pub async fn browse_registry(
    ctx: &CallCtx,
    _access: &Access,
    args: &Value,
    params: &Value,
) -> Value {
    let url = arg_str(args, "url")
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string);
    let remote = !url.as_deref().is_some_and(|u| u.starts_with('/'));
    if remote {
        let action = format!(
            "browse the MCP registry at {} through this instance's outbound policy",
            url.as_deref().unwrap_or(browse::DEFAULT_REGISTRY)
        );
        if let Confirm::Answer(v) = confirm(ctx, "browse_registry", args, params, &action).await {
            return v;
        }
    }
    let q = browse::BrowseQuery {
        url,
        search: arg_str(args, "search").map(str::to_string),
        cursor: arg_str(args, "cursor").map(str::to_string),
        limit: arg_u64(args, "limit").map(|n| n.min(100) as u32),
        version: arg_str(args, "version").map(str::to_string),
    };
    match browse::browse_json(&ctx.app, &ctx.app.registry, &q).await {
        Ok(mut page) => {
            let servers = page
                .get_mut("servers")
                .map(Value::take)
                .unwrap_or(Value::Array(Vec::new()));
            let count = servers.as_array().map(Vec::len).unwrap_or(0);
            if let Value::Object(m) = &mut page {
                m.remove("servers");
                m.insert("untrusted_servers".into(), servers);
            }
            ok(
                ctx,
                format!(
                    "{count} server(s) from {}{}{} (the entries are third-party data, under untrusted_servers)",
                    page["registry"].as_str().unwrap_or("the registry"),
                    if page["cached"].as_bool() == Some(true) {
                        " (cached)"
                    } else {
                        ""
                    },
                    if page["next_cursor"].is_string() {
                        "; more pages (cursor)"
                    } else {
                        ""
                    }
                ),
                page,
            )
        }
        Err((status, body)) => {
            let mut v = err(format!(
                "{} ({})",
                body["error"]
                    .as_str()
                    .unwrap_or("the registry could not be browsed"),
                status.as_u16()
            ));
            v["structuredContent"] = body;
            v
        }
    }
}

/// `validate_server_json`: a `server.json` against the registry schema and
/// the semantic checks.
pub fn validate_server_json(ctx: &CallCtx, _access: &Access, args: &Value) -> Value {
    let doc = match args.get("server") {
        Some(v @ Value::Object(_)) => v.clone(),
        Some(Value::String(text)) => {
            if text.len() > validate::MAX_DOC_BYTES {
                return err("the document is larger than 64 KiB");
            }
            match serde_json::from_str::<Value>(text) {
                Ok(v) => v,
                Err(e) => return err(format!("the document is not JSON: {e}")),
            }
        }
        _ => return err("server is required: the server.json document (object or JSON text)"),
    };
    let result = validate::validate(&doc);
    let issues = result["issues"].as_array().map(Vec::len).unwrap_or(0);
    let valid = result["valid"].as_bool() == Some(true);
    let summary = if valid && issues == 0 {
        format!(
            "{} is a valid server.json",
            doc["name"].as_str().unwrap_or("the document")
        )
    } else {
        let first: Vec<String> = result["issues"]
            .as_array()
            .map(|a| {
                a.iter()
                    .take(5)
                    .map(|i| {
                        truncate(i["message"].as_str().or(i.as_str()).unwrap_or("issue"), 160).0
                    })
                    .collect()
            })
            .unwrap_or_default();
        format!(
            "{}: {issues} issue(s){}",
            if valid {
                "Valid with warnings"
            } else {
                "Invalid"
            },
            if first.is_empty() {
                String::new()
            } else {
                format!(": {}", first.join("; "))
            }
        )
    };
    ok(ctx, summary, result)
}

/// `unpublish_registry_server` (admin, confirmed): drop an entry published
/// through the registry's publish API (every version, or one).
pub async fn unpublish_registry_server(
    ctx: &CallCtx,
    _access: &Access,
    args: &Value,
    params: &Value,
) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    let version = arg_str(args, "version").filter(|v| !v.is_empty());
    let action = format!(
        "remove {name}{} from the mock registry (every client of the registry stops seeing it)",
        version.map(|v| format!(" version {v}")).unwrap_or_default()
    );
    if let Confirm::Answer(v) =
        confirm(ctx, "unpublish_registry_server", args, params, &action).await
    {
        return v;
    }
    let removed = ctx.app.registry.delete_published(name, version);
    if removed == 0 {
        return err(format!(
            "no published entry {name:?}{} (file entries are removed from rustybin.yaml, generated ones follow the mocks)",
            version.map(|v| format!(" version {v:?}")).unwrap_or_default()
        ));
    }
    ok(
        ctx,
        format!("Removed {removed} version(s) of {name} from the mock registry"),
        json!({ "deleted": name, "versions": removed }),
    )
}
