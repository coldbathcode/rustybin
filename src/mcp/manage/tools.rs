//! The management tool registry: definitions (rights, annotations, input
//! schemas), the role-filtered `tools/list`, `tools/call` with audit lines
//! and metrics, confirmation of destructive tools, and the helpers the tool
//! modules share (results, arguments, untrusted content).

use serde_json::{json, Map, Value};

use super::access::{Access, Right};
use super::{
    agents, assist, config, harness, inspect, instance, mocks, portal, registry, settings, testing,
    traffic, visits,
};
use crate::mcp::core::{self, CallCtx};
use crate::mcp::protocol::{RpcError, Version};
use crate::mcp::tools as mock_tools;
use crate::mocks::service::ServiceError;

/// Longest captured body returned per entry.
pub const MAX_UNTRUSTED_BODY: usize = 4 * 1024;
/// Longest total of untrusted bodies per result.
pub const MAX_UNTRUSTED_TOTAL: usize = 64 * 1024;
/// Longest `try_request` response body.
pub const MAX_TRY_BODY: usize = 64 * 1024;

pub struct ToolDef {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub right: Right,
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    /// Needs confirmation (elicitation or `confirm: true`).
    pub confirm: bool,
    /// Reaches outside the instance (`openWorldHint`): harness runs send
    /// load to the targets a scenario names.
    pub open_world: bool,
    pub input_schema: fn() -> Value,
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false })
}

fn no_args() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn confirm_prop() -> Value {
    json!({ "type": "boolean", "description": "Set to true only after the user agreed to this destructive action (used when the client cannot ask through elicitation)." })
}

fn name_prop() -> Value {
    json!({ "type": "string", "description": "The mock's name (as in /mock/{name})" })
}

fn spec_prop() -> Value {
    json!({ "description": "The OpenAPI 3.0 / 3.1 or Swagger 2.0 document: its text (JSON or YAML) or the JSON object. Read the user's file and pass its content; the server never fetches URLs.", "oneOf": [{ "type": "string" }, { "type": "object" }] })
}

fn settings_props() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("responses".into(), json!({ "type": "object", "description": "Response settings: mode (examples, generated, static, stateful), seed, validate_requests (off, warn, enforce), delay_ms, headers. Same shape as rustybin.yaml (resource rustybin://config.schema.json)." }));
    m.insert("security".into(), json!({ "type": "object", "description": "Mocked security: enabled, mode (spec, api_key, basic, bearer, jwt, none) and the scheme's settings (keys, users, tokens, issuer, audience, scopes). Same shape as rustybin.yaml." }));
    m.insert("operations".into(), json!({ "type": "object", "description": "Per-operation settings keyed by operationId: respond (status, body, headers, delay_ms), rules, mode, public. Same shape as rustybin.yaml.", "additionalProperties": { "type": "object" } }));
    m.insert("strip_server_prefix".into(), json!({ "type": "boolean", "description": "Serve paths without the servers[0] path prefix (e.g. /v1)" }));
    m.insert("mcp".into(), json!({ "type": "object", "description": "The mock's own MCP server at /mock/{name}/_rustybin/mcp (one tool per operation): enabled (default true) and exclude (operationIds that are not tools).", "properties": { "enabled": { "type": "boolean" }, "exclude": { "type": "array", "items": { "type": "string" } } }, "additionalProperties": false }));
    m
}

macro_rules! tool {
    ($name:literal, $title:literal, $desc:literal, $right:expr, ro=$ro:literal, destructive=$d:literal, idem=$i:literal, confirm=$c:literal, $schema:expr) => {
        ToolDef {
            name: $name,
            title: $title,
            description: $desc,
            right: $right,
            read_only: $ro,
            destructive: $d,
            idempotent: $i,
            confirm: $c,
            open_world: false,
            input_schema: $schema,
        }
    };
}

fn scenario_prop() -> Value {
    json!({
        "description": "The scenario: an object in the harness scenario schema (GET /_rustybin/harness/schema, resource rustybin://harness.schema.json), or the same as JSON or YAML text. Shorthand: name, target, source.request.path, pattern {type: constant, rate, for}.",
        "oneOf": [
            { "type": "object" },
            { "type": "string" }
        ]
    })
}

fn run_id_prop() -> Value {
    json!({ "type": "string", "description": "The run id (from create_run or list_runs)" })
}

fn cluster_id_prop() -> Value {
    json!({ "type": "string", "description": "A cluster this node leads (create_cluster): the run is the cluster's, split over its client nodes. Omit for a run on this node alone." })
}

pub static TOOLS: &[ToolDef] = &[
    // ── Instance ──
    tool!("get_instance", "Instance", "What this Rustybin is: version, modes (public, shared, hosted, self-service), control auth, limits, the public configuration view, the caller's rights and the console and docs URLs. Start here.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("find_endpoints", "Find endpoints", "Search the route catalogue of this instance (echo, status codes, mock LLM providers, MCP, A2A, OAuth, bins, ...) by text and/or category. Returns path, methods, summary and an example request per route.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "query": { "type": "string", "description": "Words to look for in paths, summaries and descriptions (e.g. \"anthropic streaming\")" },
        "category": { "type": "string", "description": "Exact category name (get_instance lists them)" },
        "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 25 }
    }), &[])),
    tool!("get_health", "Health toggle", "The state of the /health demo toggle (healthy or unhealthy).", Right::Read, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("set_health", "Set health", "Set the /health demo toggle: healthy true or false (503). Admin.", Right::Admin, ro=false, destructive=false, idem=true, confirm=false, || obj(json!({ "healthy": { "type": "boolean" } }), &["healthy"])),
    tool!("get_usage", "Plan usage", "The plan, its limits and what is left (requests, egress, in flight, streams) for the caller (their key in shared mode), so a 429 can be explained.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("get_key_usage", "Key usage", "Per-key daily totals in shared mode (admin), paginated.", Right::Admin, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "day": { "type": "string", "description": "YYYY-MM-DD (default: today, UTC)" },
        "after": { "type": "string", "description": "Cursor from a previous page" },
        "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 100 }
    }), &[])),
    tool!("reset_flaky", "Reset flaky counters", "Reset the /flaky counters between test runs: the caller's session (default) or, with scope \"all\" (admin), everyone's.", Right::Read, ro=false, destructive=false, idem=true, confirm=false, || obj(json!({
        "scope": { "type": "string", "enum": ["session", "all"], "default": "session" },
        "session": { "type": "string", "description": "The X-Rustybin-Session whose counters to reset (default: the session try_request uses)" }
    }), &[])),
    // ── Mocks ──
    tool!("list_mocks", "List mocks", "The OpenAPI mocks the caller may see: name, URL, state (live, off, draft), security, calls, owner. Filter by text or state.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "query": { "type": "string", "description": "Text to look for in names, titles and URLs" },
        "state": { "type": "string", "enum": ["live", "off", "draft"] },
        "all": { "type": "boolean", "description": "Admin: include every visitor's mocks" }
    }), &[])),
    tool!("get_mock", "Mock detail", "One mock in full: operations with hit counts, settings (secrets redacted unless the caller may change it), deploy report, revisions, undeployed draft changes, URLs.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "name": name_prop() }), &["name"])),
    tool!("preview_spec", "Preview a spec", "Parse an OpenAPI spec without creating anything: title, operations, security schemes, warnings and the deploy checks. Use it before create_mock to show the user what they would get.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "spec": spec_prop() }), &["spec"])),
    tool!("get_mock_spec", "Mock spec", "The spec as uploaded (the draft's), to edit and send back with update_mock; or, with served true, the OpenAPI document the mock serves (with its server URL and mocked security).", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "name": name_prop(),
        "served": { "type": "boolean", "description": "Return the served openapi.json instead of the uploaded text" }
    }), &["name"])),
    tool!("create_mock", "Create a mock", "Create an OpenAPI mock from a spec (JSON or YAML text, or the object; Swagger 2.0 is converted). Deployed at once unless deploy is false; deploy fails on check errors unless force is true (ask the user first). Optional name, responses, security, operations. On a self-service demo the name gets a suffix and the mock belongs to the caller's visit.", Right::Manage, ro=false, destructive=false, idem=false, confirm=false, || {
        let mut props = settings_props();
        props.insert("spec".into(), spec_prop());
        props.insert("name".into(), json!({ "type": "string", "description": "Mock name (a-z, 0-9, -; default: from the spec title)" }));
        props.insert("deploy".into(), json!({ "type": "boolean", "default": true }));
        props.insert("force".into(), json!({ "type": "boolean", "description": "Deploy despite check errors (only after the user accepted them)" }));
        props.insert("enabled".into(), json!({ "type": "boolean", "default": true }));
        obj(Value::Object(props), &["spec"])
    }),
    tool!("update_mock", "Update a mock", "Replace the draft's spec and/or settings (responses, security, operations, mcp, strip_server_prefix) or rename it; deploy true publishes the draft (force to ignore check errors). Settings given replace the whole section; use set_operation for one operation.", Right::Manage, ro=false, destructive=false, idem=false, confirm=false, || {
        let mut props = settings_props();
        props.insert("name".into(), name_prop());
        props.insert("spec".into(), spec_prop());
        props.insert("rename".into(), json!({ "type": "string", "description": "A new name (instance mocks only)" }));
        props.insert("deploy".into(), json!({ "type": "boolean" }));
        props.insert("force".into(), json!({ "type": "boolean" }));
        obj(Value::Object(props), &["name"])
    }),
    tool!("set_operation", "Set an operation", "Replace one operation's settings in the draft: respond (status, body, headers, delay_ms), rules (conditions on path, query, headers, body with their own respond), mode, public (skip mocked security). clear true removes them. Deploy afterwards with deploy_mock.", Right::Manage, ro=false, destructive=false, idem=true, confirm=false, || obj(json!({
        "name": name_prop(),
        "operation": { "type": "string", "description": "operationId" },
        "settings": { "type": "object", "description": "The operation settings (same shape as rustybin.yaml operations.<id>)" },
        "clear": { "type": "boolean", "description": "Remove the operation's settings" }
    }), &["name", "operation"])),
    tool!("test_rules", "Test rules", "Which rule would answer a captured call of an operation (the latest, or request seq): every rule's conditions explained. Pass rules to try new ones without saving.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "name": name_prop(),
        "operation": { "type": "string" },
        "rules": { "type": "array", "items": { "type": "object" } },
        "request": { "type": "integer", "description": "Captured request number (default: the latest call of the operation)" }
    }), &["name", "operation"])),
    tool!("check_mock", "Check a mock", "The draft's deploy report: a valid request and response per operation, errors and warnings.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "name": name_prop() }), &["name"])),
    tool!("deploy_mock", "Deploy a mock", "Publish the draft as the live revision. Fails on check errors unless force is true (only after the user accepted them).", Right::Manage, ro=false, destructive=false, idem=true, confirm=false, || obj(json!({ "name": name_prop(), "force": { "type": "boolean" } }), &["name"])),
    tool!("rollback_mock", "Roll back a mock", "Make an earlier revision live again (the previous one, or revision N). Destructive: confirmed.", Right::Manage, ro=false, destructive=true, idem=false, confirm=true, || obj(json!({ "name": name_prop(), "revision": { "type": "integer" }, "confirm": confirm_prop() }), &["name"])),
    tool!("set_mock_enabled", "Switch a mock on or off", "Switch a mock on or off (off answers 503).", Right::Manage, ro=false, destructive=false, idem=true, confirm=false, || obj(json!({ "name": name_prop(), "enabled": { "type": "boolean" } }), &["name", "enabled"])),
    tool!("duplicate_mock", "Duplicate a mock", "Copy a mock under a new name (deployed when the source is).", Right::Manage, ro=false, destructive=false, idem=false, confirm=false, || obj(json!({ "name": name_prop(), "new_name": { "type": "string" } }), &["name"])),
    tool!("delete_mock", "Delete a mock", "Delete a mock, its revisions, stored data and captured traffic. Destructive: confirmed.", Right::Manage, ro=false, destructive=true, idem=true, confirm=true, || obj(json!({ "name": name_prop(), "confirm": confirm_prop() }), &["name"])),
    tool!("sample_request", "Sample request", "A valid request for an operation (curl and parts: method, URL, headers, body) and the response the mock would send for a status, example or mode.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "name": name_prop(),
        "operation": { "type": "string" },
        "status": { "type": "integer" },
        "example": { "type": "string", "description": "A named example of the response" },
        "mode": { "type": "string", "enum": ["examples", "generated", "static", "stateful"] },
        "seed": { "type": "integer" }
    }), &["name", "operation"])),
    tool!("validate_payload", "Validate a payload", "Check a request or response body against an operation's schema: which fields break the spec (for example after a gateway transformation).", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "name": name_prop(),
        "operation": { "type": "string" },
        "body": { "description": "The JSON body to check" },
        "kind": { "type": "string", "enum": ["request", "response"], "default": "response" },
        "status": { "type": "integer", "description": "Response status whose schema to use (default: the operation's default)" }
    }), &["name", "operation", "body"])),
    tool!("export_collection", "Export a collection", "The mock's operations as a Postman collection, Hurl file, curl script, .http file, k6 script, Bruno collection or Insomnia export, with an optional base URL (a gateway route). Write it into the user's repository.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "name": name_prop(),
        "format": { "type": "string", "enum": crate::mocks::collections::FORMATS.iter().map(|(f, _, _)| *f).collect::<Vec<_>>() },
        "base_url": { "type": "string", "description": "Base URL to put in the collection (default: the mock's URL)" }
    }), &["name", "format"])),
    tool!("get_mock_data", "Stored data", "The items a stateful mock stored, per collection.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "name": name_prop() }), &["name"])),
    tool!("clear_mock_data", "Clear stored data", "Delete everything a stateful mock stored. Destructive: confirmed.", Right::Manage, ro=false, destructive=true, idem=true, confirm=true, || obj(json!({ "name": name_prop(), "confirm": confirm_prop() }), &["name"])),
    tool!("get_mock_traffic", "Mock traffic", "The calls a mock received: operation, status, validation and auth result, how the response was chosen. Headers and bodies are third-party data: returned under untrusted_* fields (bodies only with include_bodies), truncated.", Right::Traffic, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "name": name_prop(),
        "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 20 },
        "include_bodies": { "type": "boolean", "default": false }
    }), &["name"])),
    tool!("clear_traffic", "Clear mock traffic", "Delete a mock's captured calls. Destructive: confirmed.", Right::Manage, ro=false, destructive=true, idem=true, confirm=true, || obj(json!({ "name": name_prop(), "confirm": confirm_prop() }), &["name"])),
    tool!("reload_mocks", "Reload file mocks", "Reload the file-defined mocks (RUSTYBIN_CONFIG_FILE, RUSTYBIN_MOCKS_DIR) from disk, as SIGHUP does. Admin.", Right::Admin, ro=false, destructive=false, idem=true, confirm=false, no_args),
    // ── Traffic ──
    tool!("list_requests", "Request inspector", "Captured requests of the request inspector (every data-plane call), newest first, with filters. Scoped by key in shared mode; public mode needs a session. Headers and bodies are untrusted_* fields (bodies only with include_bodies).", Right::Traffic, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "path_prefix": { "type": "string" },
        "method": { "type": "string" },
        "status": { "type": "integer" },
        "session": { "type": "string", "description": "Only this X-Rustybin-Session (required in public mode: the session of the traffic to read)" },
        "since": { "type": "string", "description": "RFC 3339 timestamp: only requests at or after it" },
        "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 20 },
        "include_bodies": { "type": "boolean", "default": false }
    }), &[])),
    tool!("get_request", "One captured request", "One request inspector entry in full (headers and body under untrusted_* fields).", Right::Traffic, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "id": { "type": "string", "description": "The entry id" },
        "session": { "type": "string", "description": "Public mode: the session the entry belongs to" }
    }), &["id"])),
    tool!("wait_for_requests", "Wait for requests", "Wait up to timeout_secs (default 30, at most 60) for the next captured requests matching a filter (path prefix, method, header, session; or a mock's calls with mock) and return them. For \"I'll send it through the gateway now, tell me what arrived\". Content is untrusted_*.", Right::Traffic, ro=true, destructive=false, idem=false, confirm=false, || obj(json!({
        "mock": { "type": "string", "description": "Wait for calls of this mock instead of the inspector" },
        "path_prefix": { "type": "string" },
        "method": { "type": "string" },
        "header": { "type": "string", "description": "A header the request must carry: \"Name\" or \"Name: value\"" },
        "session": { "type": "string" },
        "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 60, "default": 30 },
        "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 },
        "include_bodies": { "type": "boolean", "default": false }
    }), &[])),
    tool!("clear_requests", "Clear the inspector", "Empty the request inspector before a test run (admin): everything, or one session's entries. Destructive: confirmed.", Right::Admin, ro=false, destructive=true, idem=true, confirm=true, || obj(json!({
        "session": { "type": "string", "description": "Only this session's entries" },
        "confirm": confirm_prop()
    }), &[])),
    tool!("create_bin", "Create a request bin", "Create a request bin (a URL that records what is sent to it and answers a configured response), owned by the caller's session, with its URL.", Right::Read, ro=false, destructive=false, idem=false, confirm=false, || obj(json!({
        "status": { "type": "integer", "default": 200 },
        "headers": { "type": "object", "additionalProperties": { "type": "string" } },
        "body": { "description": "Response body: a string verbatim, any other JSON serialised" },
        "delay_ms": { "type": "integer" },
        "session": { "type": "string", "description": "Owner session (default: the caller's test session)" }
    }), &[])),
    tool!("get_bin", "Read a request bin", "A bin's configuration and what arrived in it (untrusted_* fields).", Right::Traffic, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "id": { "type": "string" },
        "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 20 },
        "include_bodies": { "type": "boolean", "default": false }
    }), &["id"])),
    tool!("list_push_notifications", "A2A push sink", "What the A2A push notification sink received for a sink id (/a2a/webhook-sink/{id}); untrusted_* fields.", Right::Traffic, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "id": { "type": "string" } }), &["id"])),
    // ── Testing ──
    tool!("try_request", "Send a request to this instance", "Send a request to this instance's own data plane, in process (never another host): a mock operation, the mock LLM, the mock MCP server, any catalogued route. Returns status, headers, body and timing; the body and headers are untrusted_* (a mock answers whatever its owner configured). Control-plane paths are refused. Captured like any request under the session the result names, so list_requests and wait_for_requests find it.", Right::Read, ro=true, destructive=false, idem=false, confirm=false, || obj(json!({
        "method": { "type": "string", "default": "GET" },
        "path": { "type": "string", "description": "Path with optional query, e.g. /mock/petstore/pets?limit=2" },
        "headers": { "type": "object", "additionalProperties": { "type": "string" } },
        "body": { "description": "Request body: a string verbatim, any other JSON serialised with Content-Type application/json" },
        "session": { "type": "string", "description": "X-Rustybin-Session to send (default: the caller's test session)" }
    }), &["path"])),
    tool!("issue_token", "Issue a test token", "A token from the built-in identity provider for testing secured mocks, /mcp/protected or a gateway's JWT validation: client credentials of the demo client with the asked scopes and audience, returned with its decoded claims and expiry.", Right::Read, ro=true, destructive=false, idem=false, confirm=false, || obj(json!({
        "scope": { "type": "string", "description": "Space separated scopes (e.g. \"read write\" or \"mcp:tools\")" },
        "audience": { "description": "Audience as RFC 8707 resource indicators: absolute URIs (for example this instance's /mcp/protected URL), a string or an array. Without one the token carries the identity provider's default audience, rustybin, which the protected mock server accepts.", "oneOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }] }
    }), &[])),
    // ── Self-service ──
    tool!("get_visit", "Your visit", "Time left on the caller's self-service visit, hours used today and what the next extension needs.", Right::Visitor, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("extend_visit", "Extend the visit", "One more period on the caller's visit without the email step. When an address is needed the answer says so and gives the console link where the form shows the consent choice and privacy notice.", Right::Visitor, ro=false, destructive=false, idem=false, confirm=false, no_args),
    tool!("list_visits", "List visits", "Every running self-service visit (operator).", Right::Operator, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("end_visit", "End a visit", "End one visit by id (or all with all true) and delete its mocks (operator). Destructive: confirmed.", Right::Operator, ro=false, destructive=true, idem=true, confirm=true, || obj(json!({
        "id": { "type": "string" },
        "all": { "type": "boolean" },
        "confirm": confirm_prop()
    }), &[])),
    tool!("pause_self_service", "Pause self-service", "No new visits or visitor mocks until resumed (operator). Confirmed.", Right::Operator, ro=false, destructive=true, idem=true, confirm=true, || obj(json!({ "confirm": confirm_prop() }), &[])),
    tool!("resume_self_service", "Resume self-service", "Resume visits after a pause (operator).", Right::Operator, ro=false, destructive=false, idem=true, confirm=false, no_args),
    // ── Harness ──
    tool!("harness_status", "Harness status", "Harness mode (this Rustybin as a load client): whether it is on, the node's roles, caps (rate, in flight, duration), engine, allowed targets, the active run and the console URL. Start here before any load test.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("validate_scenario", "Validate a scenario", "Check a harness scenario against this node's caps without sending anything: every problem, the normalised scenario, its hash and a summary (duration, peak rate, expected requests). Always call it before create_run.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "scenario": scenario_prop() }), &["scenario"])),
    tool!("list_scenarios", "List scenarios", "The built-in harness scenarios (smoke, ramp-to-target, soak, closed-users) with their summaries, a starting point to copy and tune.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("get_scenario", "Get a scenario", "One built-in harness scenario as an object and as YAML.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "name": { "type": "string", "description": "A built-in scenario name (list_scenarios)" } }), &["name"])),
    ToolDef {
        name: "create_run",
        title: "Start a harness run",
        description: "Validate a scenario, pin and (when configured) verify its targets and START SENDING LOAD to them from this node. Targets must be in RUSTYBIN_HARNESS_TARGETS; one run at a time per node. Asks the user to confirm (elicitation) or needs \"confirm\": true, which you set only after the user agreed to this scenario against these targets. Then poll get_run until report_ready and call get_run_report. With cluster_id the scenario is only submitted to that cluster (validated, nothing sent, no confirmation): start it with start_run.",
        right: Right::Harness,
        read_only: false,
        destructive: false,
        idempotent: false,
        confirm: true,
        open_world: true,
        input_schema: || obj(json!({ "scenario": scenario_prop(), "cluster_id": cluster_id_prop(), "confirm": confirm_prop() }), &["scenario"]),
    },
    tool!("create_cluster", "Create a cluster", "Create a harness cluster led by this node (coordinator role): client nodes send the load, server nodes are the gateway's upstreams, the rate is split over the clients by capacity and one report is merged. Returns the join URL and the join token (once) that nodes join with (RUSTYBIN_HARNESS_JOIN and RUSTYBIN_HARNESS_JOIN_TOKEN, or POST /_rustybin/harness/join).", Right::Harness, ro=false, destructive=false, idem=false, confirm=false, || obj(json!({
        "cluster_id": { "type": "string", "description": "Letters, digits, . _ - (default: random)" },
        "min_nodes": { "type": "object", "description": "Nodes a run waits for, per role", "properties": { "client": { "type": "integer", "minimum": 0 }, "server": { "type": "integer", "minimum": 0 } }, "additionalProperties": false },
        "lease": { "type": "string", "description": "How long a client keeps sending without an answer from the leader (500ms to 60s, default 5s)" },
        "election_timeout": { "type": "string", "description": "How long without the leader before the eligible nodes elect a new one (200ms, below the lease; default 3s)" },
        "join_token": { "type": "string", "description": "A join secret to use (16 to 256 characters); random when absent" }
    }), &[])),
    tool!("list_cluster_nodes", "Cluster nodes", "A cluster's members: roles, health, declared capacity, clock offset and error bound, in-flight requests; the leader, the current run and recent membership events.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "cluster_id": cluster_id_prop() }), &["cluster_id"])),
    tool!("get_leader", "Cluster leader", "Who leads a cluster as this node sees it: the leader, its term and control URL, whether failover is available (three or more nodes with RUSTYBIN_HARNESS_CONTROL_URL), the succession with each node's work score and the history of leader changes. Every member answers.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "cluster_id": cluster_id_prop() }), &["cluster_id"])),
    tool!("export_upstreams", "Export upstreams", "The server nodes' advertised URLs of a cluster, to configure the gateway's upstream pool.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "cluster_id": cluster_id_prop() }), &["cluster_id"])),
    ToolDef {
        name: "start_run",
        title: "Start a cluster run",
        description: "START SENDING LOAD from every client node of a cluster at once: the run submitted with create_run (cluster_id), once the nodes it waits for have joined. Asks the user to confirm (elicitation) or needs \"confirm\": true, which you set only after the user agreed. Then poll get_run (with cluster_id) until the state is finished and call get_run_report.",
        right: Right::Harness,
        read_only: false,
        destructive: false,
        idempotent: false,
        confirm: true,
        open_world: true,
        input_schema: || obj(json!({ "cluster_id": cluster_id_prop(), "run_id": run_id_prop(), "confirm": confirm_prop() }), &["cluster_id", "run_id"]),
    },
    tool!("list_patterns", "List patterns", "The pattern types (constant, ramp, step, target, breakpoint, spike, pulse, burst, wave) with their parameters, from the scenario schema. Chain them with then:; every one takes jitter and sync.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("preview_scenario", "Preview a scenario", "The expected requests per second of every layer and in total (users for closed layers), from the patterns' exact math, plus the validation's problems. Sends nothing: call it before create_run.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "scenario": scenario_prop() }), &["scenario"])),
    tool!("save_scenario", "Save a scenario", "Keep a scenario by name on this node (validated against its caps; at most 100), next to the built-in ones. list_scenarios shows it, get_scenario returns it.", Right::Harness, ro=false, destructive=false, idem=true, confirm=false, || obj(json!({
        "name": { "type": "string", "description": "Letters, digits, . _ - (not a built-in name)" },
        "scenario": scenario_prop()
    }), &["name", "scenario"])),
    ToolDef {
        name: "update_run",
        title: "Change a running run",
        description: "Tune a running harness run without stopping it: rate_multiplier (0 to 10, the run or one layer), pause / resume, hold (freeze the pattern; false releases), skip_stage, set ({\"<layer>.pattern.<field>\": value}), add_layer, remove_layer; layer aims a change at one layer. Checked like a new scenario, caps included; with cluster_id every client node follows on its next heartbeat. A change that may send more load asks the user to confirm (elicitation) or needs \"confirm\": true.",
        right: Right::Harness,
        read_only: false,
        destructive: false,
        idempotent: false,
        confirm: true,
        open_world: true,
        input_schema: || obj(json!({
            "run_id": run_id_prop(),
            "cluster_id": cluster_id_prop(),
            "change": { "type": "object", "description": "The change (the schema is $defs.change of GET /_rustybin/harness/schema): layer, rate_multiplier, pause, resume, hold, skip_stage, set, add_layer, remove_layer." },
            "confirm": confirm_prop()
        }), &["run_id", "change"]),
    },
    tool!("list_runs", "List runs", "The harness runs this node keeps (newest first) and the active one.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("get_run", "Run status", "Live view of a harness run: progress, counts (sent, completed, ok, errors, not sent, in flight), latency so far, the last second, saturation, report_ready.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "run_id": run_id_prop(), "cluster_id": cluster_id_prop() }), &["run_id"])),
    tool!("stop_run", "Stop a run", "Stop a harness run early: it drains its requests and reports. With cluster_id, every client node of the cluster run stops.", Right::Harness, ro=false, destructive=false, idem=true, confirm=false, || obj(json!({ "run_id": run_id_prop(), "cluster_id": cluster_id_prop() }), &["run_id"])),
    tool!("get_run_report", "Run report", "The final report of a finished harness run: totals and classes (upstream, gateway by status, transport, lost), latency percentiles (from the intended and the actual send time, upstream processing, gateway overhead), per layer and per target, the server side (arrivals, what the gateway changed), the timeline, saturation, thresholds with pass or fail, samples with their X-Request-Id. format json (default), text or junit.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "run_id": run_id_prop(),
        "cluster_id": cluster_id_prop(),
        "format": { "type": "string", "enum": ["json", "text", "junit"], "default": "json" }
    }), &["run_id"])),
    tool!("harness_stop", "Kill switch", "Stop every harness run on this node now (no confirmation: it only stops load).", Right::Harness, ro=false, destructive=false, idem=true, confirm=false, no_args),
    // ── Settings and egress ──
    tool!("get_settings", "Instance settings", "Every instance setting (console, network, AI, portal, registry, git, agents, ...) with its value, source (environment, stored, default) and lock state; secrets show only {set, hint}. Filter by group or one key. Admin.", Right::Admin, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "group": { "type": "string", "description": "Only this group (console, egress, assist, portal, registry, git, agents, ...)" },
        "key": { "type": "string", "description": "Only this dotted key (console.title)" }
    }), &[])),
    tool!("update_settings", "Change settings", "Set instance settings (values: key to value; null forgets a stored value) and/or unset stored values (back to the environment or the default). Never secrets: provider keys, tokens and passwords are set by the operator in the console, the CLI or the environment. Admin.", Right::Admin, ro=false, destructive=false, idem=true, confirm=false, || obj(json!({
        "values": { "type": "object", "description": "Setting key to value, in the key's kind (get_settings shows kinds and defaults)", "additionalProperties": true },
        "unset": { "type": "array", "items": { "type": "string" }, "description": "Keys whose stored value to forget" }
    }), &[])),
    tool!("check_egress", "Outbound policy", "The outbound connection policy (the allowlist, the loopback switch, each feature's rule, the last decisions) and, with url, whether that URL would be allowed for a purpose before DNS, with the reasons when not. Admin.", Right::Admin, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "url": { "type": "string", "description": "An http(s) URL to check" },
        "purpose": { "type": "string", "enum": ["assist", "mcp_relay", "registry", "git", "a2a_push", "leads", "key_registry"], "default": "mcp_relay" }
    }), &[])),
    // ── AI assist ──
    tool!("assist_status", "AI assist status", "Whether AI assist is on, the provider and models, the caps, each provider's key state ({set, hint}: never a value), the tasks and the usage total. Keys are set by the operator in the console or the CLI, never here.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("assist_usage", "AI usage", "AI calls, tokens and the estimated cost since start, per model and per task. Admin.", Right::Admin, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("test_assist", "Test the AI provider", "One small call to the configured provider with the instance's key (it costs a few tokens): the model answers or the error says why not. Admin.", Right::Admin, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("run_assist_task", "Run an AI task", "Run a read task of AI assist with its inputs: free_answer (question, context), explain (what, context), suggest, request_examples, tool_arguments, server_json, summarise_for_portal (assist_status lists them with their inputs). The writing tasks are generate_spec, generate_examples and edit_spec_with_ai. The answer costs the user's tokens.", Right::Read, ro=true, destructive=false, idem=false, confirm=false, || obj(json!({
        "task": { "type": "string", "description": "The task name" },
        "inputs": { "type": "object", "description": "The task's inputs", "additionalProperties": true },
        "model": { "type": "string", "description": "A model id instead of the task's default" }
    }), &["task"])),
    tool!("generate_spec", "Describe an API (AI)", "An OpenAPI document written by the AI provider from a description (resources, operations, fields, style), checked like an upload: the document, its preview and the deploy checks. Nothing is created unless create is true, which asks the user to confirm (elicitation, else \"confirm\": true) and then creates the mock as create_mock would (name, deploy, force).", Right::Manage, ro=false, destructive=false, idem=false, confirm=true, || obj(json!({
        "description": { "type": "string", "description": "What the API does: resources, operations, fields, conventions" },
        "openapi_version": { "type": "string", "description": "3.1.0 (default) or 3.0.3" },
        "style": { "type": "object", "description": "Conventions to follow (naming, pagination, errors), free form", "additionalProperties": true },
        "model": { "type": "string" },
        "create": { "type": "boolean", "default": false, "description": "Create the mock from the document (confirmed)" },
        "name": { "type": "string", "description": "The mock's name when creating (default: from the title)" },
        "deploy": { "type": "boolean", "default": true },
        "force": { "type": "boolean", "description": "Create despite check errors (only after the user accepted them)" },
        "confirm": confirm_prop()
    }), &["description"])),
    tool!("generate_examples", "Examples for operations (AI)", "Example payloads for the operations of a mock that lack one (or the ones named), written by the AI provider (source ai) or the deterministic generator (source generated, no key). Previewed unless apply is true, which asks the user to confirm and stores them in the draft (deploy_mock publishes them).", Right::Manage, ro=false, destructive=false, idem=false, confirm=true, || obj(json!({
        "name": name_prop(),
        "operations": { "type": "array", "items": { "type": "string" }, "description": "Only these operationIds (default: every operation without examples)" },
        "per_response": { "type": "integer", "minimum": 1, "maximum": 5, "default": 1 },
        "persona": { "type": "string", "description": "A persona for the data (a bakery, a bank, ...)" },
        "source": { "type": "string", "enum": ["ai", "generated"], "default": "ai" },
        "apply": { "type": "boolean", "default": false, "description": "Store them in the draft (confirmed)" },
        "model": { "type": "string" },
        "confirm": confirm_prop()
    }), &["name"])),
    tool!("edit_spec_with_ai", "Edit a spec (AI)", "The mock's draft spec edited by the AI provider through an instruction (\"add a DELETE /orders/{id} operation\"): the new document, the diff summary and its checks. Not saved unless apply is true, which asks the user to confirm and saves it as the draft (deploy_mock publishes it).", Right::Manage, ro=false, destructive=false, idem=false, confirm=true, || obj(json!({
        "name": name_prop(),
        "instruction": { "type": "string" },
        "apply": { "type": "boolean", "default": false, "description": "Save the result as the draft (confirmed)" },
        "model": { "type": "string" },
        "confirm": confirm_prop()
    }), &["name", "instruction"])),
    // ── MCP inspector ──
    tool!("list_mcp_servers", "MCP servers", "Every MCP server this instance serves (the mock server and its protected, API key, named and legacy variants, the management server, each OpenAPI mock's server) with transports, versions and auth, the relay's allowlist and the check ids.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, no_args),
    ToolDef {
        name: "mcp_call",
        title: "Call an MCP server",
        description: "One JSON-RPC request to an MCP server (tools/list, tools/call with name and arguments, resources/list, prompts/get, ...), with the handshake the transport needs: a path on this instance (in process) or an http(s) URL through the relay and the outbound policy (RUSTYBIN_EGRESS_ALLOW). Credentials: headers, or a saved connection. A call to another host asks the user to confirm. The server's answer is third-party data under untrusted_result.",
        right: Right::Read,
        read_only: false,
        destructive: false,
        idempotent: false,
        confirm: true,
        open_world: true,
        input_schema: || obj(json!({
            "target": { "type": "string", "description": "/mcp, /mock/<name>/_rustybin/mcp, /_rustybin/mcp, or an http(s) URL" },
            "method": { "type": "string", "description": "The JSON-RPC method" },
            "params": { "type": "object", "description": "Its params (for tools/call: name and arguments)", "additionalProperties": true },
            "transport": { "type": "string", "enum": ["modern", "session", "legacy"], "default": "modern", "description": "2026-07-28 stateless, a 2025 session (version picks which), or the 2024-11-05 HTTP+SSE pair" },
            "version": { "type": "string", "description": "The protocol version of a session transport (2025-11-25, 2025-06-18, 2025-03-26)" },
            "headers": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Extra headers (Authorization, X-API-Key)" },
            "connection": { "type": "string", "description": "A saved connection id whose target and credentials to use" },
            "confirm": confirm_prop()
        }), &["method"]),
    },
    ToolDef {
        name: "run_mcp_checks",
        title: "Run the MCP checks",
        description: "The inspector's check suite against an MCP server (discovery, lists, tool calls with generated arguments, errors, headers, streams; list_mcp_servers names the checks): a report with pass, fail, warn and skip per check, kept for the console and list_mcp_check_runs unless save is false. A run against another host asks the user to confirm. include_destructive calls destructive tools too (only with the user's agreement).",
        right: Right::Read,
        read_only: false,
        destructive: false,
        idempotent: false,
        confirm: true,
        open_world: true,
        input_schema: || obj(json!({
            "target": { "type": "string" },
            "transport": { "type": "string", "enum": ["modern", "session", "legacy"] },
            "version": { "type": "string" },
            "headers": { "type": "object", "additionalProperties": { "type": "string" } },
            "connection": { "type": "string" },
            "checks": { "type": "array", "items": { "type": "string" }, "description": "Only these check ids (default: all)" },
            "include_destructive": { "type": "boolean", "default": false },
            "max_tools": { "type": "integer", "minimum": 1, "description": "At most this many tools called" },
            "ai_arguments": { "type": "boolean", "default": false, "description": "Let the AI provider write the tool arguments (the generator by default)" },
            "save": { "type": "boolean", "default": true },
            "confirm": confirm_prop()
        }), &[]),
    },
    tool!("list_mcp_check_runs", "Saved check runs", "The saved check runs of the MCP inspector (newest first), or one report in full with id.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "id": { "type": "string" } }), &[])),
    tool!("list_mcp_connections", "Saved connections", "The MCP inspector's saved connections (target, transport, auth kind), or one with id. Their credentials are sealed on the instance and never returned; saving one is done in the console or with rustybin mcp save-connection.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "id": { "type": "string" } }), &[])),
    // ── Mock MCP registry ──
    tool!("list_registry_servers", "Mock registry", "The mock MCP registry at /mcp-registry: every entry under the current scenario (generated from this instance's servers, the file's, the published ones) with its validation, the counts, the scenarios and a gateway's sync configuration.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("set_registry_scenario", "Registry scenario", "How the mock registry misbehaves for gateway tests: normal, or a scenario that breaks it on purpose (list_registry_servers lists them with their effect). Admin.", Right::Admin, ro=false, destructive=false, idem=true, confirm=false, || obj(json!({ "scenario": { "type": "string", "enum": crate::mcp::registry::SCENARIOS.iter().map(|(id, _)| *id).collect::<Vec<_>>() } }), &["scenario"])),
    ToolDef {
        name: "browse_registry",
        title: "Browse a registry",
        description: "A page of a real MCP registry (the official registry.modelcontextprotocol.io by default, or url) through the outbound policy, with a search and a cursor. Browsing another host asks the user to confirm. The entries are third-party data under untrusted_servers.",
        right: Right::Read,
        read_only: true,
        destructive: false,
        idempotent: true,
        confirm: true,
        open_world: true,
        input_schema: || obj(json!({
            "url": { "type": "string", "description": "The registry's base URL (default: the official registry; /mcp-registry is this instance's mock)" },
            "search": { "type": "string" },
            "cursor": { "type": "string", "description": "next_cursor of the previous page" },
            "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 30 },
            "version": { "type": "string", "description": "latest, or a version" },
            "confirm": confirm_prop()
        }), &[]),
    },
    tool!("validate_server_json", "Validate a server.json", "Check a registry server.json (object or JSON text) against the registry schema and the semantic checks (names, versions, remotes, packages): valid or the issues.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "server": { "description": "The server.json document", "oneOf": [{ "type": "object" }, { "type": "string" }] } }), &["server"])),
    tool!("unpublish_registry_server", "Remove a published entry", "Remove an entry published through the mock registry's publish API (every version, or one). Destructive: confirmed. Admin.", Right::Admin, ro=false, destructive=true, idem=true, confirm=true, || obj(json!({
        "name": { "type": "string", "description": "The server name (io.example/orders)" },
        "version": { "type": "string" },
        "confirm": confirm_prop()
    }), &["name"])),
    // ── Dev portal ──
    tool!("list_portal_items", "Portal items", "The dev portal: on or off, who may read it (auth), the branding, every item that can be published (mocks, MCP servers, the LLM, agents, the tools, the registry) with its published metadata, whether it is hidden and why, and its insights.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("publish_portal_item", "Publish to the portal", "Publish an item to the dev portal with its metadata (or update the metadata of a published one): mock:<name>, mcp:<server>, llm, a2a:<agent>, tools or registry. Mocks need manage rights, the rest admin. When the portal is on and open to anyone the item becomes public at once, so the user confirms.", Right::Manage, ro=false, destructive=false, idem=true, confirm=true, || obj(json!({
        "id": { "type": "string", "description": "mock:<name>, mcp:<server>, llm, a2a:<agent>, tools or registry" },
        "title": { "type": "string" },
        "summary": { "type": "string", "description": "A paragraph in Markdown" },
        "tags": { "type": "array", "items": { "type": "string" } },
        "contact": { "type": "string" },
        "order": { "type": "integer", "description": "Lower comes first" },
        "credentials": { "type": "boolean", "description": "Show a mock's demo credentials (default true)" },
        "confirm": confirm_prop()
    }), &["id"])),
    tool!("unpublish_portal_item", "Unpublish from the portal", "Remove an item from the dev portal (its metadata goes with it).", Right::Manage, ro=false, destructive=false, idem=true, confirm=false, || obj(json!({ "id": { "type": "string" } }), &["id"])),
    tool!("set_portal_branding", "Portal settings", "Change the portal.* settings: enabled, title, intro, contact, auth (open, password, key), base_url (a gateway URL in the snippets), visitors. Keys with or without the portal. prefix. Admin.", Right::Admin, ro=false, destructive=false, idem=true, confirm=false, || obj(json!({ "settings": { "type": "object", "description": "The portal settings to change (get_settings with group portal lists them)", "additionalProperties": true } }), &["settings"])),
    tool!("get_portal_insights", "Portal insights", "Views, downloads and Try it calls per published item over the last days (counts only).", Right::Read, ro=true, destructive=false, idem=true, confirm=false, no_args),
    // ── Configuration as code and git ──
    tool!("export_config", "Export rustybin.yaml", "The instance as rustybin.yaml: version 2 (the whole instance: instance, settings, secrets as references, mocks, registry, portal, harness, agents, health) for an admin, else the mocks the caller may see (version 1); or one mock with name. Secrets are references (${VAR} or secret:stored), never values. To commit as configuration as code.", Right::Read, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "name": { "type": "string", "description": "One mock (default: the whole instance)" },
        "secrets": { "type": "string", "enum": ["refs", "omit"], "default": "refs" },
        "format": { "type": "string", "enum": ["yaml", "json"], "default": "yaml" }
    }), &[])),
    tool!("diff_config", "Diff rustybin.yaml", "What applying a rustybin.yaml document (version 1 or 2) would change on this instance, section by section: a dry run that changes nothing. Admin.", Right::Admin, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({
        "document": { "description": "The document: YAML or JSON text, or the object", "oneOf": [{ "type": "string" }, { "type": "object" }] },
        "prune": { "type": "boolean", "default": false, "description": "Count what pruning would remove" }
    }), &["document"])),
    tool!("apply_config", "Apply rustybin.yaml", "Apply a rustybin.yaml document (version 1 or 2) through every section's service: settings, secret references, mocks, registry, portal, harness, agents, health. dry_run true only reports the changes. prune true makes a present section the whole truth (what it does not name is removed; destructive: confirmed). Admin.", Right::Admin, ro=false, destructive=true, idem=true, confirm=true, || obj(json!({
        "document": { "description": "The document: YAML or JSON text, or the object", "oneOf": [{ "type": "string" }, { "type": "object" }] },
        "dry_run": { "type": "boolean", "default": false },
        "prune": { "type": "boolean", "default": false },
        "confirm": confirm_prop()
    }), &["document"])),
    tool!("git_status", "Git status", "Source control: the repository (remote, branch, auth, deploy key), the working copy, the drift between it and the instance, ahead and behind (with fetch), the timeline of operations and the last commits. Admin.", Right::Admin, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "fetch": { "type": "boolean", "default": false, "description": "Ask the remote for ahead and behind" } }), &[])),
    tool!("git_init", "Connect the repository", "Clone the configured remote (git.remote, git.branch settings) into the working copy, or start an empty one for a new repository. Admin.", Right::Admin, ro=false, destructive=false, idem=true, confirm=false, no_args),
    tool!("git_commit", "Commit the configuration", "Export the instance into the working copy and commit with a message (nothing to commit is a fine answer). Admin.", Right::Admin, ro=false, destructive=false, idem=false, confirm=false, || obj(json!({ "message": { "type": "string" } }), &["message"])),
    ToolDef {
        name: "git_push",
        title: "Push to the remote",
        description: "Push the branch to the remote: the instance's configuration leaves this host (secrets as references, never values), so the user confirms (elicitation, else \"confirm\": true). A diverged branch is reported, never merged: git_resolve. Admin.",
        right: Right::Admin,
        read_only: false,
        destructive: false,
        idempotent: true,
        confirm: true,
        open_world: true,
        input_schema: || obj(json!({ "confirm": confirm_prop() }), &[]),
    },
    ToolDef {
        name: "git_pull",
        title: "Pull from the remote",
        description: "Fetch and fast-forward the working copy; with apply true the repository's document is applied to this instance (the user confirms, unless dry_run shows the diff only); prune makes the document the whole truth. Admin.",
        right: Right::Admin,
        read_only: false,
        destructive: false,
        idempotent: true,
        confirm: true,
        open_world: true,
        input_schema: || obj(json!({
            "apply": { "type": "boolean", "default": false },
            "dry_run": { "type": "boolean", "default": false },
            "prune": { "type": "boolean", "default": false },
            "confirm": confirm_prop()
        }), &[]),
    },
    ToolDef {
        name: "git_resolve",
        title: "Resolve a diverged branch",
        description: "After a diverged branch: with instance, this instance's configuration replaces the remote branch (a force push); with repository, the repository's document is applied here and the local commits are dropped. Destructive: confirmed. Admin.",
        right: Right::Admin,
        read_only: false,
        destructive: true,
        idempotent: false,
        confirm: true,
        open_world: true,
        input_schema: || obj(json!({
            "with": { "type": "string", "enum": ["instance", "repository"] },
            "message": { "type": "string", "description": "The commit message when the instance wins" },
            "confirm": confirm_prop()
        }), &["with"]),
    },
    tool!("git_deploy_key", "Deploy key", "The deploy key's public half for ssh remotes (register it on the repository with write access); generate true makes one, replace true (confirmed) replaces an existing key. Admin.", Right::Admin, ro=false, destructive=false, idem=true, confirm=true, || obj(json!({
        "generate": { "type": "boolean", "default": false },
        "replace": { "type": "boolean", "default": false },
        "confirm": confirm_prop()
    }), &[])),
    // ── Agents ──
    tool!("list_agents", "AI agents", "Every agent that called this management server (its credential and client name, first and last seen, calls, sessions, status, rights) and the policy (default ceiling, the rules of rustybin.yaml). Admin.", Right::Admin, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("get_agent", "One agent", "One agent in full: calls per tool with their outcomes, last errors, sessions, the tools its rights allow and where the rights come from, recent activity. Admin.", Right::Admin, ro=true, destructive=false, idem=true, confirm=false, || obj(json!({ "id": { "type": "string", "description": "The agent id (list_agents)" } }), &["id"])),
    tool!("set_agent_rights", "Set an agent's rights", "Limit an agent: a ceiling (read-only, traffic, manage, full), tools allowed, tools denied, a note; reset true clears its own rights (back to the rules and the default). An agent never gets more than its credential allows. Admin.", Right::Admin, ro=false, destructive=false, idem=true, confirm=false, || obj(json!({
        "id": { "type": "string" },
        "ceiling": { "type": "string", "enum": ["read-only", "traffic", "manage", "full"] },
        "allow": { "type": "array", "items": { "type": "string" }, "description": "Only these tools" },
        "deny": { "type": "array", "items": { "type": "string" }, "description": "Never these tools" },
        "note": { "type": "string", "description": "A note for the operator (null clears it)" },
        "reset": { "type": "boolean", "default": false }
    }), &["id"])),
    tool!("revoke_agent", "Revoke an agent", "Refuse the agent's calls, end its sessions and revoke its tokens (console sign-in tokens, OAuth tokens and refresh tokens it used). An agent cannot revoke itself. Destructive: confirmed. Admin.", Right::Admin, ro=false, destructive=true, idem=true, confirm=true, || obj(json!({ "id": { "type": "string" }, "confirm": confirm_prop() }), &["id"])),
    tool!("restore_agent", "Restore an agent", "Undo a revocation. Admin.", Right::Admin, ro=false, destructive=false, idem=true, confirm=false, || obj(json!({ "id": { "type": "string" } }), &["id"])),
    tool!("end_agent_sessions", "End an agent's sessions", "Close the agent's handshake-era MCP sessions without revoking it. Admin.", Right::Admin, ro=false, destructive=false, idem=true, confirm=false, || obj(json!({ "id": { "type": "string" } }), &["id"])),
    // ── Sign-in sessions ──
    tool!("list_sessions", "Sign-in sessions", "The console's sign-in sessions since the last restart (user, issued, expiry, last seen), when the password sign-in is on. The password itself is changed only in the console, with rustybin password change, or through the environment. Admin.", Right::Admin, ro=true, destructive=false, idem=true, confirm=false, no_args),
    tool!("end_session", "End a sign-in session", "End one console sign-in session by id, or with all true sign out everywhere (every token issued so far is refused, the user's own console included; destructive: confirmed). Admin.", Right::Admin, ro=false, destructive=true, idem=true, confirm=true, || obj(json!({
        "id": { "type": "string" },
        "all": { "type": "boolean", "default": false },
        "confirm": confirm_prop()
    }), &[])),
];

/// The tools the caller may use.
pub fn list(access: &Access) -> Vec<&'static ToolDef> {
    TOOLS.iter().filter(|t| access.allows(t.right)).collect()
}

pub fn find(name: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.name == name)
}

pub fn tool_json(t: &ToolDef, version: Version) -> Value {
    let mut v = json!({
        "name": t.name,
        "description": t.description,
        "inputSchema": (t.input_schema)(),
    });
    if version.has_structured_output() {
        v["title"] = json!(t.title);
    }
    if version.has_annotations() {
        v["annotations"] = json!({
            "title": t.title,
            "readOnlyHint": t.read_only,
            "destructiveHint": t.destructive,
            "idempotentHint": t.idempotent,
            "openWorldHint": t.open_world,
        });
    }
    v
}

fn access_of(ctx: &CallCtx) -> Result<&Access, RpcError> {
    ctx.access
        .as_deref()
        .ok_or_else(|| RpcError::internal("management server without a caller"))
}

/// `tools/list`, filtered by the caller's rights.
pub fn list_result(ctx: &CallCtx, params: &Value) -> Result<Value, RpcError> {
    let access = access_of(ctx)?;
    let agent = agents::observe_call(ctx, access, params);
    ctx.app.agents.record_list(&agent.id);
    // A revoked agent sees nothing; a limited one only what its rights allow.
    let all: Vec<&ToolDef> = list(access)
        .into_iter()
        .filter(|t| !agent.revoked && agent.rights.allows(t))
        .collect();
    let (page, next) = core::paginate(&all, params, ctx.page_size, "tools")?;
    let mut r = Map::new();
    r.insert(
        "tools".into(),
        Value::Array(page.iter().map(|t| tool_json(t, ctx.version)).collect()),
    );
    if let Some(next) = next {
        r.insert("nextCursor".into(), json!(next));
    }
    Ok(core::cacheable(ctx, Value::Object(r), 60_000, "private"))
}

// ── Results ─────────────────────────────────────────────────────────

/// A successful result: a one-line summary plus structured content.
pub fn ok(ctx: &CallCtx, summary: impl Into<String>, structured: Value) -> Value {
    mock_tools::ok_structured(ctx, summary.into(), structured)
}

/// A tool error (never a protocol error).
pub fn err(msg: impl Into<String>) -> Value {
    mock_tools::tool_error(msg)
}

/// A service refusal as a tool error with its code and status.
pub fn service_error(e: &ServiceError) -> Value {
    let mut v = err(format!("{} ({})", e.message(), e.status.as_u16()));
    let mut body = e.body.clone();
    if let Value::Object(m) = &mut body {
        m.insert("status".into(), json!(e.status.as_u16()));
    }
    v["structuredContent"] = body;
    v
}

/// Either the tool's structured result or an error result.
pub type ToolOutcome = Result<(String, Value), Value>;

pub fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

pub fn arg_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

pub fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| {
        v.as_u64().or_else(|| {
            v.as_f64()
                .filter(|f| f.fract() == 0.0 && *f >= 0.0)
                .map(|f| f as u64)
        })
    })
}

pub fn arg_usize(args: &Value, key: &str) -> Option<usize> {
    arg_u64(args, key).map(|n| n as usize)
}

/// A required string argument.
pub fn need_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, Value> {
    arg_str(args, key)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| err(format!("missing required argument: {key}")))
}

// ── Untrusted content ───────────────────────────────────────────────

/// Keeps the total of untrusted bodies in a result under the cap.
pub struct Budget {
    left: usize,
    pub truncated: bool,
}

impl Budget {
    pub fn new() -> Self {
        Self {
            left: MAX_UNTRUSTED_TOTAL,
            truncated: false,
        }
    }

    /// A body for the result: cut at the per-body cap and at what is left
    /// of the per-result budget; `None` when nothing is left.
    pub fn body(&mut self, text: &str) -> Option<(String, bool)> {
        if self.left == 0 {
            self.truncated = true;
            return None;
        }
        let cap = MAX_UNTRUSTED_BODY.min(self.left);
        let (cut, truncated) = truncate(text, cap);
        self.left = self.left.saturating_sub(cut.len());
        if truncated {
            self.truncated = true;
        }
        Some((cut, truncated))
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::new()
    }
}

/// Cut `text` at `cap` bytes on a character boundary.
pub fn truncate(text: &str, cap: usize) -> (String, bool) {
    if text.len() <= cap {
        return (text.to_string(), false);
    }
    let mut end = cap;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

/// Header list as `[{name, value}]` under an untrusted key.
pub fn untrusted_headers(headers: &[(String, String)]) -> Value {
    Value::Array(
        headers
            .iter()
            .map(|(n, v)| json!({ "name": n, "value": truncate(v, 1024).0 }))
            .collect(),
    )
}

// ── Confirmation ────────────────────────────────────────────────────

/// Outcome of asking for confirmation.
pub enum Confirm {
    /// Go ahead.
    Yes,
    /// Answer this result instead (a question to the client, a refusal, or
    /// the user said no).
    Answer(Value),
}

/// Confirm a destructive action: `confirm: true` in the arguments, an
/// elicitation answer, or ask the client (multi round-trip on 2026-07-28,
/// `elicitation/create` on a legacy session stream).
pub async fn confirm(
    ctx: &CallCtx,
    tool: &str,
    args: &Value,
    params: &Value,
    action: &str,
) -> Confirm {
    if arg_bool(args, "confirm") == Some(true) {
        return Confirm::Yes;
    }
    let decline = |result: &Value| {
        let decision = result
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("cancel");
        let yes = decision == "accept"
            && result
                .pointer("/content/confirm")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        if yes {
            Confirm::Yes
        } else {
            Confirm::Answer(err(format!(
                "the user did not confirm: {action} (nothing was changed)"
            )))
        }
    };
    if ctx.version.is_modern() {
        match mock_tools::input_response(ctx, tool, params, "confirm") {
            Ok(Some(resp)) => return decline(resp),
            Ok(None) => {}
            Err(e) => return Confirm::Answer(err(e.message)),
        }
        if ctx.client_has("elicitation") {
            let req = json!({
                "method": "elicitation/create",
                "params": mock_tools::elicit_request_params(ctx, action),
            });
            return Confirm::Answer(mock_tools::input_required(ctx, tool, "confirm", req));
        }
    } else if ctx.version.has_structured_output()
        && ctx.client_has("elicitation")
        && ctx.can_request_client()
    {
        return match ctx
            .request_client(
                "elicitation/create",
                mock_tools::elicit_request_params(ctx, action),
            )
            .await
        {
            Ok(result) => decline(&result),
            Err(e) => Confirm::Answer(err(format!("confirmation failed: {e}"))),
        };
    }
    Confirm::Answer(err(format!(
        "{action}: this is destructive and the client cannot ask the user to confirm. Ask them \
         yourself, and call the tool again with \"confirm\": true only when they agreed."
    )))
}

// ── Dispatch ────────────────────────────────────────────────────────

/// `tools/call`.
pub async fn call(ctx: &CallCtx, params: &Value) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("Missing required parameter: name"))?;
    let Some(def) = find(name) else {
        return Err(RpcError::invalid_params(format!("Unknown tool: {name}")));
    };
    let access = access_of(ctx)?;
    let args = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => return Err(RpcError::invalid_params("arguments must be an object")),
    };
    let metrics = ctx.app.metrics.clone();
    let agent = agents::observe_call(ctx, access, params);
    let refusal = if agent.revoked {
        Some(format!(
            "{}: this agent was revoked by the operator of this instance",
            def.name
        ))
    } else if !access.allows(def.right) {
        Some(format!(
            "{}: not allowed. {}",
            def.name,
            access.denial(def.right)
        ))
    } else if !agent.rights.allows(def) {
        Some(format!(
            "{}: not allowed. {}",
            def.name,
            agent.rights.denial(def)
        ))
    } else {
        None
    };
    if let Some(message) = refusal {
        metrics.count_manage_call(def.name, "denied");
        audit(def, access, &agent.id, &args, "denied");
        ctx.app
            .agents
            .record_call(&agent.id, def.name, "denied", None);
        return Ok(err(message));
    }
    ctx.log("debug", json!({ "event": "tools/call", "tool": name }))
        .await;
    let result = match def.name {
        // Instance
        "get_instance" => instance::get_instance(ctx, access),
        "find_endpoints" => instance::find_endpoints(ctx, &args),
        "get_health" => instance::get_health(ctx),
        "set_health" => instance::set_health(ctx, &args),
        "get_usage" => instance::get_usage(ctx, access),
        "get_key_usage" => instance::get_key_usage(ctx, access, &args).await,
        "reset_flaky" => instance::reset_flaky(ctx, access, &args).await,
        // Mocks
        "list_mocks" => mocks::list_mocks(ctx, access, &args),
        "get_mock" => mocks::get_mock(ctx, access, &args),
        "preview_spec" => mocks::preview_spec(ctx, access, &args),
        "get_mock_spec" => mocks::get_mock_spec(ctx, access, &args),
        "create_mock" => mocks::create_mock(ctx, access, &args),
        "update_mock" => mocks::update_mock(ctx, access, &args),
        "set_operation" => mocks::set_operation(ctx, access, &args),
        "test_rules" => mocks::test_rules(ctx, access, &args),
        "check_mock" => mocks::check_mock(ctx, access, &args),
        "deploy_mock" => mocks::deploy_mock(ctx, access, &args),
        "rollback_mock" => mocks::rollback_mock(ctx, access, &args, params).await,
        "set_mock_enabled" => mocks::set_mock_enabled(ctx, access, &args),
        "duplicate_mock" => mocks::duplicate_mock(ctx, access, &args),
        "delete_mock" => mocks::delete_mock(ctx, access, &args, params).await,
        "sample_request" => mocks::sample_request(ctx, access, &args),
        "validate_payload" => mocks::validate_payload(ctx, access, &args),
        "export_collection" => mocks::export_collection(ctx, access, &args),
        "get_mock_data" => mocks::get_mock_data(ctx, access, &args),
        "clear_mock_data" => mocks::clear_mock_data(ctx, access, &args, params).await,
        "get_mock_traffic" => mocks::get_mock_traffic(ctx, access, &args),
        "clear_traffic" => mocks::clear_traffic(ctx, access, &args, params).await,
        "reload_mocks" => instance::reload_mocks(ctx, access),
        // Traffic
        "list_requests" => traffic::list_requests(ctx, access, &args),
        "get_request" => traffic::get_request(ctx, access, &args),
        "wait_for_requests" => traffic::wait_for_requests(ctx, access, &args).await,
        "clear_requests" => traffic::clear_requests(ctx, access, &args, params).await,
        "create_bin" => traffic::create_bin(ctx, access, &args).await,
        "get_bin" => traffic::get_bin(ctx, access, &args).await,
        "list_push_notifications" => traffic::list_push_notifications(ctx, access, &args).await,
        // Testing
        "try_request" => testing::try_request(ctx, access, &args).await,
        "issue_token" => testing::issue_token(ctx, access, &args).await,
        // Self-service
        "get_visit" => visits::get_visit(ctx, access),
        "extend_visit" => visits::extend_visit(ctx, access),
        "list_visits" => visits::list_visits(ctx, access),
        "end_visit" => visits::end_visit(ctx, access, &args, params).await,
        "pause_self_service" => visits::set_paused(ctx, access, &args, params, true).await,
        "resume_self_service" => visits::set_paused(ctx, access, &args, params, false).await,
        // Harness
        "harness_status" => harness::harness_status(ctx, access),
        "validate_scenario" => harness::validate_scenario(ctx, &args),
        "list_scenarios" => harness::list_scenarios(ctx),
        "get_scenario" => harness::get_scenario(ctx, &args),
        "create_run" => harness::create_run(ctx, access, &args, params).await,
        "list_runs" => harness::list_runs(ctx),
        "get_run" => harness::get_run(ctx, access, &args),
        "stop_run" => harness::stop_run(ctx, &args),
        "get_run_report" => harness::get_run_report(ctx, &args),
        "harness_stop" => harness::harness_stop(ctx),
        "create_cluster" => harness::create_cluster(ctx, access, &args).await,
        "list_cluster_nodes" => harness::list_cluster_nodes(ctx, &args),
        "export_upstreams" => harness::export_upstreams(ctx, &args),
        "get_leader" => harness::get_leader(ctx, &args),
        "list_patterns" => harness::list_patterns(ctx),
        "preview_scenario" => harness::preview_scenario(ctx, &args),
        "save_scenario" => harness::save_scenario(ctx, &args),
        "update_run" => harness::update_run(ctx, &args, params).await,
        "start_run" => harness::start_run(ctx, &args, params).await,
        // Settings and egress
        "get_settings" => settings::get_settings(ctx, access, &args),
        "update_settings" => settings::update_settings(ctx, access, &args),
        "check_egress" => settings::check_egress(ctx, access, &args),
        // AI assist
        "assist_status" => assist::assist_status(ctx, access, &args),
        "assist_usage" => assist::assist_usage(ctx, access, &args),
        "test_assist" => assist::test_assist(ctx, access, &args).await,
        "run_assist_task" => assist::run_assist_task(ctx, access, &args).await,
        "generate_spec" => assist::generate_spec(ctx, access, &args, params).await,
        "generate_examples" => assist::generate_examples(ctx, access, &args, params).await,
        "edit_spec_with_ai" => assist::edit_spec_with_ai(ctx, access, &args, params).await,
        // MCP inspector
        "list_mcp_servers" => inspect::list_mcp_servers(ctx, access, &args),
        "mcp_call" => inspect::mcp_call(ctx, access, &args, params).await,
        "run_mcp_checks" => inspect::run_mcp_checks(ctx, access, &args, params).await,
        "list_mcp_check_runs" => inspect::list_mcp_check_runs(ctx, access, &args),
        "list_mcp_connections" => inspect::list_mcp_connections(ctx, access, &args),
        // Mock MCP registry
        "list_registry_servers" => registry::list_registry_servers(ctx, access, &args),
        "set_registry_scenario" => registry::set_registry_scenario(ctx, access, &args),
        "browse_registry" => registry::browse_registry(ctx, access, &args, params).await,
        "validate_server_json" => registry::validate_server_json(ctx, access, &args),
        "unpublish_registry_server" => {
            registry::unpublish_registry_server(ctx, access, &args, params).await
        }
        // Dev portal
        "list_portal_items" => portal::list_portal_items(ctx, access, &args),
        "publish_portal_item" => portal::publish_portal_item(ctx, access, &args, params).await,
        "unpublish_portal_item" => portal::unpublish_portal_item(ctx, access, &args),
        "set_portal_branding" => portal::set_portal_branding(ctx, access, &args),
        "get_portal_insights" => portal::get_portal_insights(ctx, access, &args),
        // Configuration as code and git
        "export_config" => config::export_config(ctx, access, &args),
        "diff_config" => config::diff_config(ctx, access, &args),
        "apply_config" => config::apply_config(ctx, access, &args, params).await,
        "git_status" => config::git_status(ctx, access, &args).await,
        "git_init" => config::git_init(ctx, access, &args).await,
        "git_commit" => config::git_commit(ctx, access, &args).await,
        "git_push" => config::git_push(ctx, access, &args, params).await,
        "git_pull" => config::git_pull(ctx, access, &args, params).await,
        "git_resolve" => config::git_resolve(ctx, access, &args, params).await,
        "git_deploy_key" => config::git_deploy_key(ctx, access, &args, params).await,
        // Agents
        "list_agents" => agents::tools::list_agents(ctx, access, &args),
        "get_agent" => agents::tools::get_agent(ctx, access, &args),
        "set_agent_rights" => agents::tools::set_agent_rights(ctx, access, &args),
        "revoke_agent" => agents::tools::revoke_agent(ctx, access, &agent.id, &args, params).await,
        "restore_agent" => agents::tools::restore_agent(ctx, access, &args),
        "end_agent_sessions" => agents::tools::end_agent_sessions(ctx, access, &args),
        // Sign-in sessions
        "list_sessions" => instance::list_sessions(ctx, access),
        "end_session" => instance::end_session(ctx, access, &args, params).await,
        _ => return Err(RpcError::invalid_params(format!("Unknown tool: {name}"))),
    };
    let outcome = if result.get("isError").and_then(Value::as_bool) == Some(true) {
        "error"
    } else if result.get("resultType").and_then(Value::as_str) == Some("input_required") {
        "asked"
    } else {
        "ok"
    };
    metrics.count_manage_call(def.name, if outcome == "asked" { "ok" } else { outcome });
    if !def.read_only {
        audit(def, access, &agent.id, &args, outcome);
    }
    let message = (outcome == "error").then(|| {
        result["content"]
            .as_array()
            .and_then(|c| c.first())
            .and_then(|c| c["text"].as_str())
            .unwrap_or("error")
            .to_string()
    });
    ctx.app
        .agents
        .record_call(&agent.id, def.name, outcome, message.as_deref());
    Ok(result)
}

/// One JSON log line per change: tool, target, caller kind and fingerprint,
/// outcome; never arguments that may hold secrets or spec bodies.
fn audit(def: &ToolDef, access: &Access, agent: &str, args: &Value, outcome: &str) {
    let target = arg_str(args, "name")
        .or_else(|| arg_str(args, "id"))
        .or_else(|| arg_str(args, "mock"))
        .or_else(|| arg_str(args, "run_id"))
        .unwrap_or("-");
    tracing::info!(
        target: "rustybin::manage",
        tool = def.name,
        target = target,
        caller = %access.principal(),
        agent,
        outcome,
        "management MCP call"
    );
}
