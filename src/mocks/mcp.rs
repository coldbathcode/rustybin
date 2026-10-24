//! An OpenAPI mock as MCP tools (`docs/plans/mock-mcp.md`): one tool per
//! operation, built from a compiled revision and cached on it
//! ([`Compiled::mcp_tools`]). A revision is immutable, so its tools are too.
//!
//! The MCP endpoint (`/mock/{name}/_rustybin/mcp`, `crate::mcp::openapi`)
//! answers every call through the mock engine, so REST and MCP answers
//! never differ. This module only decides what the tools look like and how
//! their arguments map back to a request.

use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

use super::compile::Compiled;
use super::schema::{self, Direction};
use super::spec::{Location, Operation, SchemeKind};

/// Longest tool name (the common client limit, inside the 128 that MCP
/// 2025-11-25 allows).
pub const MAX_NAME: usize = 64;
/// Longest tool description.
pub const MAX_DESCRIPTION: usize = 1000;
/// Longest tool title.
pub const MAX_TITLE: usize = 120;
/// The argument that carries the request body.
pub const BODY_ARG: &str = "body";

/// Request headers that are never tool arguments: the transport sets them,
/// or they carry credentials, which come from the MCP request instead.
const RESERVED_HEADERS: &[&str] = &[
    "authorization",
    "content-type",
    "content-length",
    "accept",
    "host",
    "cookie",
    "connection",
    "transfer-encoding",
];

/// The tools of one revision.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ToolSet {
    pub tools: Vec<Tool>,
    /// Operations that are not tools, with the reason.
    pub skipped: Vec<Skipped>,
}

impl ToolSet {
    pub fn find(&self, name: &str) -> Option<&Tool> {
        self.tools.iter().find(|t| t.name == name)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Skipped {
    pub operation: String,
    pub reason: String,
}

/// One tool: an operation and how its arguments become a request.
#[derive(Clone, Debug, Serialize)]
pub struct Tool {
    pub name: String,
    /// The operationId.
    pub operation: String,
    pub method: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub description: String,
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
    #[serde(skip)]
    pub input_schema: Value,
    /// `{status, body}` with the default 2xx JSON response schema.
    #[serde(skip)]
    pub output_schema: Option<Value>,
    /// The body schema of `output_schema` as written in the spec (results
    /// are checked against it before they are returned as structured).
    #[serde(skip)]
    pub output_body: Option<Value>,
    #[serde(skip)]
    pub args: Vec<Arg>,
    #[serde(skip)]
    pub body: Option<BodyArg>,
}

/// A tool argument that is a request parameter.
#[derive(Clone, Debug)]
pub struct Arg {
    /// The argument name (the parameter name, or `<location>_<name>` when two
    /// locations use the same name).
    pub name: String,
    pub param: String,
    pub location: Location,
    pub required: bool,
}

/// The request body argument.
#[derive(Clone, Debug)]
pub struct BodyArg {
    pub required: bool,
    pub media_type: String,
}

/// `[A-Za-z0-9_.-]`, at most [`MAX_NAME`] characters, never empty.
pub fn sanitize_name(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    out.truncate(MAX_NAME);
    if out.is_empty() {
        "operation".to_string()
    } else {
        out
    }
}

/// `name`, else `name_2`, `name_3`, ... (still at most [`MAX_NAME`]).
fn unique(name: String, taken: &mut HashSet<String>) -> String {
    if taken.insert(name.clone()) {
        return name;
    }
    let mut n = 2usize;
    loop {
        let suffix = format!("_{n}");
        let mut base = name.clone();
        base.truncate(MAX_NAME.saturating_sub(suffix.len()));
        let candidate = format!("{base}{suffix}");
        if taken.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// Text for a client: control characters (except new lines) removed, at
/// most `max` characters.
fn clean(text: &str, max: usize) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

/// Credentials of the mock's security schemes and settings: never arguments.
fn credential_params(c: &Compiled) -> Vec<(Location, String)> {
    let mut out = vec![(
        Location::Header,
        c.settings.security.api_key.header.to_ascii_lowercase(),
    )];
    for s in &c.spec.security_schemes {
        if let SchemeKind::ApiKey { name, location } = &s.kind {
            out.push((*location, name.to_ascii_lowercase()));
        }
    }
    out
}

fn description_of(op: &Operation) -> String {
    let mut parts = Vec::new();
    let mut head = String::new();
    if op.deprecated {
        head.push_str("Deprecated. ");
    }
    if let Some(s) = op.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        head.push_str(s.trim());
    }
    if !head.trim().is_empty() {
        parts.push(head.trim().to_string());
    }
    if let Some(d) = op.description.as_deref().filter(|d| !d.trim().is_empty()) {
        parts.push(d.trim().to_string());
    }
    parts.push(format!("{} {}", op.method, op.path));
    clean(&parts.join("\n\n"), MAX_DESCRIPTION)
}

/// The description of a parameter argument, saying where it goes.
fn arg_description(p: &super::spec::Parameter, renamed: bool) -> String {
    let place = match p.location {
        Location::Path => "Path parameter",
        Location::Query => "Query parameter",
        Location::Header => "Header",
        Location::Cookie => "Cookie",
    };
    let mut text = if renamed {
        format!("{place} {:?}.", p.name)
    } else {
        format!("{place}.")
    };
    if let Some(d) = p.description.as_deref().filter(|d| !d.trim().is_empty()) {
        text = format!("{} {text}", d.trim());
    }
    clean(&text, MAX_DESCRIPTION)
}

/// `schema` with `description` in front of the description it has.
fn with_description(mut schema: Value, description: String) -> Value {
    match &mut schema {
        Value::Object(o) => {
            let text = match o.get("description").and_then(Value::as_str) {
                Some(own) if !own.trim().is_empty() => {
                    clean(&format!("{description} {}", own.trim()), MAX_DESCRIPTION)
                }
                _ => description,
            };
            o.insert("description".into(), json!(text));
            schema
        }
        // `true` (anything): keep it as an object schema with a description.
        _ => json!({ "description": description }),
    }
}

/// The tool for one operation, or why it is not a tool.
fn tool_for(
    c: &Compiled,
    op: &Operation,
    credentials: &[(Location, String)],
    taken: &mut HashSet<String>,
) -> Result<Tool, String> {
    if c.settings.mcp.exclude.iter().any(|e| e == &op.id) {
        return Err("excluded by the mock's settings (mcp.exclude)".into());
    }
    let root = &c.spec.root;
    let body = match &op.request_body {
        None => None,
        Some(rb) => {
            let media = if rb.content.is_empty() {
                None
            } else {
                match rb.content.iter().find(|m| m.is_json()) {
                    Some(m) => Some(m),
                    None => {
                        let types: Vec<&str> =
                            rb.content.iter().map(|m| m.media_type.as_str()).collect();
                        return Err(format!(
                            "the request body is {}: only JSON bodies are tool arguments",
                            types.join(", ")
                        ));
                    }
                }
            };
            Some((rb.required, media))
        }
    };

    let mut properties = Map::new();
    let mut required = Vec::new();
    let mut args = Vec::new();
    let mut used: HashSet<String> = HashSet::new();
    if body.is_some() {
        used.insert(BODY_ARG.to_string());
    }
    for location in [
        Location::Path,
        Location::Query,
        Location::Header,
        Location::Cookie,
    ] {
        for p in op.params_in(location) {
            let lower = p.name.to_ascii_lowercase();
            if location == Location::Header && RESERVED_HEADERS.contains(&lower.as_str()) {
                continue;
            }
            if credentials
                .iter()
                .any(|(l, n)| *l == location && *n == lower)
            {
                continue;
            }
            let mut name = p.name.clone();
            let mut renamed = false;
            if used.contains(&name) {
                name = format!("{}_{}", location.as_str(), p.name);
                renamed = true;
                let mut n = 2;
                while used.contains(&name) {
                    name = format!("{}_{}_{n}", location.as_str(), p.name);
                    n += 1;
                }
            }
            used.insert(name.clone());
            let inlined = schema::inline(&p.schema, root, Direction::Request);
            let mut prop = with_description(inlined, arg_description(p, renamed));
            if let (Some(ex), Value::Object(o)) = (&p.example, &mut prop) {
                o.entry("examples").or_insert_with(|| json!([ex]));
            }
            let is_required = p.required || location == Location::Path;
            if is_required {
                required.push(json!(name));
            }
            properties.insert(name.clone(), prop);
            args.push(Arg {
                name,
                param: p.name.clone(),
                location,
                required: is_required,
            });
        }
    }
    let body_arg = body.map(|(body_required, media)| {
        let media_type = media
            .map(|m| m.media_type.clone())
            .unwrap_or_else(|| "application/json".to_string());
        let schema = media
            .and_then(|m| m.schema.as_ref())
            .map(|s| schema::inline(s, root, Direction::Request))
            .unwrap_or_else(|| json!({}));
        properties.insert(
            BODY_ARG.into(),
            with_description(schema, format!("Request body ({media_type}).")),
        );
        if body_required {
            required.push(json!(BODY_ARG));
        }
        BodyArg {
            required: body_required,
            media_type,
        }
    });
    let input_schema = json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    });

    let (output_schema, output_body) = output_of(op, root);
    let method = op.method.as_str();
    Ok(Tool {
        name: unique(sanitize_name(&op.id), taken),
        operation: op.id.clone(),
        method: op.method.clone(),
        path: op.path.clone(),
        title: op
            .summary
            .as_deref()
            .map(|s| clean(s, MAX_TITLE))
            .filter(|s| !s.is_empty()),
        description: description_of(op),
        read_only: matches!(method, "GET" | "HEAD" | "OPTIONS"),
        destructive: method == "DELETE",
        idempotent: matches!(method, "GET" | "HEAD" | "OPTIONS" | "PUT" | "DELETE"),
        deprecated: op.deprecated,
        input_schema,
        output_schema,
        output_body,
        args,
        body: body_arg,
    })
}

/// The output schema from the default 2xx response, when it is JSON with a
/// schema.
fn output_of(op: &Operation, root: &Value) -> (Option<Value>, Option<Value>) {
    let status = op.default_status();
    let Some(def) = op.response_for(status) else {
        return (None, None);
    };
    let Some(schema) = def
        .content
        .iter()
        .find(|m| m.is_json())
        .and_then(|m| m.schema.as_ref())
    else {
        return (None, None);
    };
    let body = schema::inline(schema, root, Direction::Response);
    let output = json!({
        "type": "object",
        "properties": {
            "status": { "type": "integer", "description": "HTTP status of the mocked response" },
            "body": with_description(body, format!("Response body (as documented for {status})")),
        },
        "required": ["status"],
    });
    (Some(output), Some(schema.clone()))
}

/// Build the tools of a revision (spec order).
pub fn build(c: &Compiled) -> ToolSet {
    let credentials = credential_params(c);
    let mut taken = HashSet::new();
    let mut set = ToolSet::default();
    for op in &c.spec.operations {
        match tool_for(c, op, &credentials, &mut taken) {
            Ok(tool) => set.tools.push(tool),
            Err(reason) => set.skipped.push(Skipped {
                operation: op.id.clone(),
                reason,
            }),
        }
    }
    set
}

#[cfg(test)]
mod tests;
