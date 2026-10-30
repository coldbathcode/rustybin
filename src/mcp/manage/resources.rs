//! Resources of the management server: the `rustybin.yaml` JSON Schema and
//! the route catalogue.

use serde_json::{json, Value};

use crate::mcp::core::{self, CallCtx};
use crate::mcp::protocol::RpcError;

pub const SCHEMA_URI: &str = "rustybin://config.schema.json";
pub const CATALOGUE_URI: &str = "rustybin://catalogue";

fn resources() -> Vec<Value> {
    vec![
        json!({
            "uri": SCHEMA_URI,
            "name": "config.schema.json",
            "title": "rustybin.yaml JSON Schema",
            "description": "The schema of rustybin.yaml (version 2: the whole instance; the mocks sections are the settings arguments of create_mock, update_mock and set_operation: responses, security, operations). Also GET /_rustybin/config/schema.",
            "mimeType": "application/schema+json",
        }),
        json!({
            "uri": CATALOGUE_URI,
            "name": "catalogue",
            "title": "Route catalogue",
            "description": "Every route of this instance with methods, category, summary, description and runnable examples (the same data as GET /_rustybin/catalog).",
            "mimeType": "application/json",
        }),
    ]
}

/// `resources/list`.
pub fn list_result(ctx: &CallCtx, params: &Value) -> Result<Value, RpcError> {
    let all = resources();
    let (page, next) = core::paginate(&all, params, ctx.page_size, "resources")?;
    let mut r = json!({ "resources": page });
    if let Some(next) = next {
        r["nextCursor"] = json!(next);
    }
    Ok(core::cacheable(ctx, r, 300_000, "public"))
}

/// `resources/read`.
pub fn read(ctx: &CallCtx, params: &Value) -> Result<Value, RpcError> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("Missing required parameter: uri"))?;
    let (mime, body) = match uri {
        SCHEMA_URI => (
            "application/schema+json",
            crate::mocks::config_file::json_schema(),
        ),
        CATALOGUE_URI => ("application/json", crate::ui::catalog_json()),
        _ => return Err(RpcError::resource_not_found(uri, ctx.version)),
    };
    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": mime,
            "text": serde_json::to_string_pretty(&body).unwrap_or_default(),
        }]
    }))
}
