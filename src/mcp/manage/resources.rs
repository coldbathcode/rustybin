//! Resources of the management server: the `rustybin.yaml` JSON Schema
//! (version 2), the route catalogue, the harness scenario schema, the dev
//! portal's public catalogue and the command line reference.

use serde_json::{json, Value};

use crate::mcp::core::{self, CallCtx};
use crate::mcp::protocol::RpcError;

pub const SCHEMA_URI: &str = "rustybin://config.schema.json";
pub const CATALOGUE_URI: &str = "rustybin://catalogue";
pub const HARNESS_SCHEMA_URI: &str = "rustybin://harness.schema.json";
pub const PORTAL_CATALOG_URI: &str = "rustybin://portal/catalog";
pub const CLI_URI: &str = "rustybin://cli.md";

/// Every resource: `(uri, name, title, description, mime type)`.
pub const RESOURCES: &[(&str, &str, &str, &str, &str)] = &[
    (
        SCHEMA_URI,
        "config.schema.json",
        "rustybin.yaml JSON Schema",
        "The schema of rustybin.yaml version 2: the whole instance (instance, settings, secrets as references, mocks, registry, portal, harness, agents, health). The mocks sections are the settings arguments of create_mock, update_mock and set_operation. Also GET /_rustybin/config/schema.",
        "application/schema+json",
    ),
    (
        CATALOGUE_URI,
        "catalogue",
        "Route catalogue",
        "Every route of this instance with methods, category, summary, description and runnable examples (the same data as GET /_rustybin/catalog).",
        "application/json",
    ),
    (
        HARNESS_SCHEMA_URI,
        "harness.schema.json",
        "Harness scenario JSON Schema",
        "The schema of a load test scenario (targets, layers with sources and patterns, scale, thresholds, live changes under $defs.change) with this node's caps as bounds. Also GET /_rustybin/harness/schema.",
        "application/schema+json",
    ),
    (
        PORTAL_CATALOG_URI,
        "portal/catalog",
        "Dev portal catalogue",
        "What visitors of the dev portal see: the published items with their metadata, snippets and links (the same data as GET /portal/api/catalog), as the caller's request reaches the instance.",
        "application/json",
    ),
    (
        CLI_URI,
        "cli.md",
        "Command line reference",
        "The reference of the rustybin command line (every noun and verb, flags, the routes they call), for the commands to suggest when something is done from a shell or CI.",
        "text/markdown",
    ),
];

fn resources() -> Vec<Value> {
    RESOURCES
        .iter()
        .map(|(uri, name, title, description, mime)| {
            json!({
                "uri": uri,
                "name": name,
                "title": title,
                "description": description,
                "mimeType": mime,
            })
        })
        .collect()
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
    let state = &ctx.app;
    let (mime, text) = match uri {
        SCHEMA_URI => (
            "application/schema+json",
            pretty(&crate::mocks::config_file::json_schema()),
        ),
        CATALOGUE_URI => ("application/json", pretty(&crate::ui::catalog_json())),
        HARNESS_SCHEMA_URI => {
            let caps = match &state.harness {
                Some(h) => h.caps(),
                None => crate::harness::scenario::Caps::from_config(&state.config.harness),
            };
            (
                "application/schema+json",
                pretty(&crate::harness::scenario::schema(&caps)),
            )
        }
        PORTAL_CATALOG_URI => {
            let links = ctx
                .access
                .as_ref()
                .map(|a| a.links.clone())
                .ok_or_else(|| RpcError::internal("management server without a caller"))?;
            let portal_ctx = crate::portal::api::ctx_for(state, links);
            (
                "application/json",
                pretty(&crate::portal::items::catalog_json(state, &portal_ctx)),
            )
        }
        CLI_URI => ("text/markdown", crate::cli::reference_markdown()),
        _ => return Err(RpcError::resource_not_found(uri, ctx.version)),
    };
    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": mime,
            "text": text,
        }]
    }))
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}
