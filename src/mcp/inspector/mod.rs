//! The MCP inspector's server side (`docs/plans/next/step-05-mcp-inspector.md`):
//!
//! | Route | | |
//! |---|---|---|
//! | `GET /_rustybin/mcp-servers` | read | Every MCP server this instance serves (the `/mcp` variants, the management server, each mock's server) and the relay's policy |
//! | `POST /_rustybin/mcp-relay` | read | Forward one JSON-RPC message to a server, the answer as SSE ([`relay`]) |
//! | `DELETE /_rustybin/mcp-relay/{session}` | read | End a relay session (and its upstream session) |
//! | `POST /_rustybin/mcp-relay/fetch` | read | GET or POST one document for the OAuth discovery |
//! | `POST /_rustybin/mcp-checks` | read | Run the check suite ([`checks`]); JSON or JUnit; saved (at most 50) |
//! | `GET /_rustybin/mcp-checks`, `GET`, `DELETE /_rustybin/mcp-checks/{id}` | read / admin | The saved runs |
//! | `GET /_rustybin/mcp-connections`, `PUT`, `DELETE /_rustybin/mcp-connections/{id}` | read / admin | Saved connections, credentials sealed ([`store`]) |
//!
//! "read" is the console scope (control auth lets it through as a
//! read-only POST: these routes change nothing on the instance); outbound
//! traffic is bounded by the egress policy (`Purpose::McpRelay`).

pub mod checks;
pub mod client;
pub mod relay;
pub mod store;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::control_auth::ControlAuth;
use crate::state::AppState;
use client::{Target, Transport};

pub const SERVERS_PATH: &str = "/_rustybin/mcp-servers";
pub const RELAY_PATH: &str = "/_rustybin/mcp-relay";
pub const RELAY_FETCH_PATH: &str = "/_rustybin/mcp-relay/fetch";
pub const CHECKS_PATH: &str = "/_rustybin/mcp-checks";
pub const CONNECTIONS_PATH: &str = "/_rustybin/mcp-connections";

/// The inspector's state (module-local, attached with `Extension`).
pub struct Inspector {
    pub relay: relay::Relay,
    pub store: store::Store,
}

/// POST routes the console scope may call (they change nothing on the
/// instance; outbound calls follow the egress policy).
pub fn is_console_post(path: &str) -> bool {
    matches!(path, RELAY_PATH | RELAY_FETCH_PATH | CHECKS_PATH)
}

/// DELETE of a relay session: the console scope may end its own sessions.
pub fn is_relay_session_path(path: &str) -> bool {
    path.strip_prefix("/_rustybin/mcp-relay/")
        .is_some_and(|id| !id.is_empty() && id != "fetch" && !id.contains('/'))
}

fn error(status: StatusCode, message: impl Into<String>, reason: &str) -> Response {
    (
        status,
        Json(json!({ "error": message.into(), "reason": reason })),
    )
        .into_response()
}

// ── GET /_rustybin/mcp-servers ──────────────────────────────────────

fn versions() -> Value {
    let mut v = vec![json!(client::MODERN)];
    v.extend(client::SESSION_VERSIONS.iter().map(|s| json!(s)));
    v.push(json!(client::LEGACY_VERSION));
    Value::Array(v)
}

fn mock_auth(settings: &crate::mocks::model::SecuritySettings, declares: bool) -> Value {
    use crate::mocks::model::SecurityMode;
    if !settings.enabled {
        return json!({ "kind": "none" });
    }
    match settings.mode {
        SecurityMode::ApiKey => json!({ "kind": "api_key", "name": settings.api_key.header }),
        SecurityMode::Basic => json!({ "kind": "basic" }),
        SecurityMode::Bearer | SecurityMode::Jwt => {
            json!({ "kind": "bearer", "hint": "a token from the Security tab of the mock, or the built-in identity provider" })
        }
        SecurityMode::Spec if declares => {
            json!({ "kind": "spec", "hint": "the spec's security schemes" })
        }
        SecurityMode::Spec => json!({ "kind": "none" }),
    }
}

async fn servers(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let mut list = vec![
        json!({ "id": "mcp", "path": "/mcp", "kind": "mock-mcp", "title": "Mock MCP server (open)", "transports": ["modern", "session"], "versions": versions(), "auth": { "kind": "none" }, "state": "serving" }),
        json!({ "id": "mcp-protected", "path": "/mcp/protected", "kind": "mock-mcp", "title": "Mock MCP server behind OAuth 2.1", "transports": ["modern", "session"], "versions": versions(), "auth": { "kind": "oauth", "hint": "discovery from the 401 (RFC 9728), the built-in identity provider" }, "state": "serving" }),
        json!({ "id": "mcp-apikey", "path": "/mcp/apikey", "kind": "mock-mcp", "title": "Mock MCP server behind an API key", "transports": ["modern", "session"], "versions": versions(), "auth": { "kind": "api_key", "name": "X-API-Key", "hint": "any non-empty key unless RUSTYBIN_MCP_API_KEY is set" }, "state": "serving" }),
    ];
    for name in crate::mcp::NAMED_SERVERS {
        list.push(json!({ "id": format!("mcp-{name}"), "path": format!("/mcp/servers/{name}"), "kind": "mock-mcp", "title": format!("Named server {name} (a tool subset)"), "transports": ["modern", "session"], "versions": versions(), "auth": { "kind": "none" }, "state": "serving" }));
    }
    list.push(json!({ "id": "mcp-sse", "path": "/mcp/sse", "kind": "mock-mcp", "title": "Mock MCP server, legacy HTTP+SSE (2024-11-05)", "transports": ["legacy"], "versions": [client::LEGACY_VERSION], "auth": { "kind": "none" }, "state": "serving" }));
    let manage_auth = match state.config.control_auth {
        ControlAuth::Open => json!({ "kind": "none" }),
        _ => {
            json!({ "kind": "bearer", "hint": "the console's control token (sign-in), the admin token, or OAuth sign-in for agents" })
        }
    };
    list.push(json!({ "id": "manage", "path": crate::mcp::manage::MANAGE_PATH, "kind": "manage", "title": "Management MCP server (configures this instance)", "transports": ["modern", "session"], "versions": versions(), "auth": manage_auth, "state": "serving" }));
    let caller = crate::mocks::selfservice::caller(&state, &headers, &extensions);
    for s in state.mocks.list() {
        if !caller.can_read(&s) {
            continue;
        }
        let c = s.live.as_ref().unwrap_or(&s.draft);
        let serving = s.live.is_some() && s.enabled && c.settings.mcp.enabled;
        list.push(json!({
            "id": format!("mock-{}", s.name),
            "path": format!("/mock/{}/_rustybin/mcp", s.name),
            "kind": "mock-api",
            "mock": s.name,
            "title": format!("Mock API {} ({})", s.name, c.spec.title),
            "transports": ["modern", "session"],
            "versions": versions(),
            "auth": mock_auth(&c.settings.security, c.spec.declares_security()),
            "state": if serving { "serving" } else { s.state() },
            "tools": c.mcp_tools().tools.len(),
        }));
    }
    let (allow, _) = state.egress.allowlist();
    Json(json!({
        "servers": list,
        "relay": {
            "allow": allow.describe(),
            "allow_loopback": state.egress.loopback_allowed(),
            "public_mode": state.config.public_mode,
            "hint": "Servers on this instance are relayed in process. Other hosts need RUSTYBIN_EGRESS_ALLOW (Management, Network); loopback needs the loopback switch.",
        },
        "checks": checks::CHECKS.iter().map(|(id, title)| json!({ "id": id, "title": title })).collect::<Vec<_>>(),
    }))
    .into_response()
}

// ── POST /_rustybin/mcp-checks and the saved runs ───────────────────

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ChecksBody {
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub headers: Value,
    #[serde(default)]
    pub connection: Option<String>,
    #[serde(default)]
    pub checks: Value,
    #[serde(default)]
    pub include_destructive: Option<bool>,
    #[serde(default)]
    pub max_tools: Option<usize>,
    #[serde(default)]
    pub ai_arguments: Option<bool>,
    #[serde(default)]
    pub save: Option<bool>,
    #[serde(default)]
    pub format: Option<String>,
}

fn wants_junit(headers: &HeaderMap, format: Option<&str>) -> bool {
    format == Some("junit")
        || (format.is_none()
            && headers
                .get(header::ACCEPT)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|a| a.contains("xml") && !a.contains("json")))
}

fn junit_response(report: &Value) -> Response {
    let mut resp = checks::junit(report).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml; charset=utf-8"),
    );
    resp
}

async fn run_checks(
    State(state): State<AppState>,
    Extension(inspector): Extension<Arc<Inspector>>,
    headers: HeaderMap,
    extensions: Extensions,
    body: Bytes,
) -> Response {
    if body.len() > 256 * 1024 {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the body is larger than 256 KiB",
            "too_large",
        );
    }
    let req: ChecksBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("expected {{target, transport?, version?, headers?, connection?, checks?, include_destructive?, max_tools?, ai_arguments?, save?, format?}}: {e}"), "invalid_body"),
    };
    let (conn_target, conn_transport, conn_version, mut auth, query) = match &req.connection {
        Some(id) => match inspector.store.resolve(id) {
            Some((c, secrets)) => {
                let (h, q) = store::build_auth(&c.auth, &secrets);
                (Some(c.target), Some(c.transport), c.version, h, q)
            }
            None => {
                return error(
                    StatusCode::NOT_FOUND,
                    format!("no saved connection {id:?}"),
                    "unknown_connection",
                )
            }
        },
        None => (None, None, None, Vec::new(), Vec::new()),
    };
    let raw_target = if req.target.is_empty() {
        conn_target.unwrap_or_default()
    } else {
        req.target.clone()
    };
    let target = match Target::parse(&raw_target, false) {
        Ok(t) => t.with_query(&query),
        Err(e) => return error(StatusCode::BAD_REQUEST, e, "invalid_target"),
    };
    let transport = match Transport::parse(
        req.transport.as_deref().or(conn_transport.as_deref()),
        req.version.as_deref().or(conn_version.as_deref()),
    ) {
        Ok(t) => t,
        Err(e) => return error(StatusCode::BAD_REQUEST, e, "invalid_transport"),
    };
    match relay::header_list(&req.headers) {
        Ok(h) => {
            for (k, v) in h {
                auth.retain(|(ak, _)| !ak.eq_ignore_ascii_case(&k));
                auth.push((k, v));
            }
        }
        Err(e) => return error(StatusCode::BAD_REQUEST, e, "invalid_headers"),
    }
    let only = match checks::parse_only(&req.checks) {
        Ok(o) => o,
        Err(e) => return error(StatusCode::BAD_REQUEST, e, "invalid_checks"),
    };
    let principal = {
        let who = crate::settings::principal(&headers, &state.config);
        if who == "open" {
            format!(
                "open@{}",
                crate::session::client_ip(&headers, &extensions, &state.config)
                    .map(|i| i.to_string())
                    .unwrap_or_default()
            )
        } else {
            who
        }
    };
    let spec = checks::RunSpec {
        target,
        transport,
        auth,
        only,
        include_destructive: req.include_destructive.unwrap_or(false),
        max_tools: req
            .max_tools
            .unwrap_or(checks::DEFAULT_MAX_TOOLS)
            .clamp(1, checks::MAX_TOOLS),
        ai_arguments: req.ai_arguments.unwrap_or(false),
        principal,
    };
    let report = checks::run(&state, spec).await;
    tracing::info!(target: "rustybin::mcp_inspector", target = %raw_target.split('?').next().unwrap_or(""), ok = %report["ok"], "mcp checks run");
    if req.save.unwrap_or(true) {
        if let Err(e) = inspector.store.save_run(&report) {
            tracing::warn!(target: "rustybin::mcp_inspector", "check run not saved: {e}");
        }
    }
    if wants_junit(&headers, req.format.as_deref()) {
        return junit_response(&report);
    }
    Json(report).into_response()
}

async fn list_runs(Extension(inspector): Extension<Arc<Inspector>>) -> Response {
    Json(json!({ "runs": inspector.store.runs(), "capacity": store::MAX_RUNS })).into_response()
}

#[derive(Deserialize, Default)]
struct FormatQuery {
    format: Option<String>,
}

async fn get_run(
    Extension(inspector): Extension<Arc<Inspector>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<FormatQuery>,
) -> Response {
    match inspector.store.run(&id) {
        Some(r) if wants_junit(&headers, q.format.as_deref()) => junit_response(&r),
        Some(r) => Json(r).into_response(),
        None => error(
            StatusCode::NOT_FOUND,
            format!("no saved run {id:?}"),
            "unknown_run",
        ),
    }
}

async fn delete_run(
    State(state): State<AppState>,
    Extension(inspector): Extension<Arc<Inspector>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    match inspector.store.delete_run(&id) {
        Ok(true) => Json(json!({ "deleted": id })).into_response(),
        Ok(false) => error(
            StatusCode::NOT_FOUND,
            format!("no saved run {id:?}"),
            "unknown_run",
        ),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e, "io"),
    }
}

// ── Saved connections ───────────────────────────────────────────────

async fn list_connections(Extension(inspector): Extension<Arc<Inspector>>) -> Response {
    Json(json!({
        "connections": inspector.store.list(),
        "capacity": store::MAX_CONNECTIONS,
        "persisted": inspector.store.persisted(),
    }))
    .into_response()
}

async fn get_connection(
    Extension(inspector): Extension<Arc<Inspector>>,
    Path(id): Path<String>,
) -> Response {
    match inspector.store.get(&id) {
        Some(c) => Json(c).into_response(),
        None => error(
            StatusCode::NOT_FOUND,
            format!("no saved connection {id:?}"),
            "unknown_connection",
        ),
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ConnectionBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    transport: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    auth: store::AuthSpec,
    #[serde(default)]
    secrets: BTreeMap<String, Option<String>>,
}

async fn put_connection(
    State(state): State<AppState>,
    Extension(inspector): Extension<Arc<Inspector>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let req: ConnectionBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!(
                    "expected {{name, target, transport?, version?, mode?, auth?, secrets?}}: {e}"
                ),
                "invalid_body",
            )
        }
    };
    let conn = store::Connection {
        id: id.clone(),
        name: req.name,
        target: req.target,
        transport: req.transport,
        version: req.version,
        mode: req.mode,
        auth: req.auth,
        created_at: String::new(),
        updated_at: String::new(),
    };
    match inspector.store.put(&id, conn, req.secrets) {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, e, "invalid_connection"),
    }
}

async fn delete_connection(
    State(state): State<AppState>,
    Extension(inspector): Extension<Arc<Inspector>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    match inspector.store.delete(&id) {
        Ok(true) => Json(json!({ "deleted": id })).into_response(),
        Ok(false) => error(
            StatusCode::NOT_FOUND,
            format!("no saved connection {id:?}"),
            "unknown_connection",
        ),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e, "io"),
    }
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

pub fn router(state: &AppState) -> Router<AppState> {
    let inspector = Arc::new(Inspector {
        relay: relay::Relay::default(),
        store: store::Store::new(state.settings.clone()),
    });
    Router::new()
        .route(SERVERS_PATH, get(servers))
        .route(RELAY_PATH, post(relay::relay))
        .route(RELAY_FETCH_PATH, post(relay::fetch))
        .route(
            "/_rustybin/mcp-relay/{session}",
            delete(relay::delete_session),
        )
        .route(CHECKS_PATH, get(list_runs).post(run_checks))
        .route(
            "/_rustybin/mcp-checks/{id}",
            get(get_run).delete(delete_run),
        )
        .route(CONNECTIONS_PATH, get(list_connections))
        .route(
            "/_rustybin/mcp-connections/{id}",
            get(get_connection)
                .put(put_connection)
                .delete(delete_connection),
        )
        .layer(Extension(inspector))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(SERVERS_PATH, &["GET"], category::CONTROL, "MCP inspector: every MCP server this instance serves, and the relay's policy")
            .example(Example::get("MCP servers of this instance", SERVERS_PATH)),
        Endpoint::new(RELAY_PATH, &["POST"], category::CONTROL, "MCP inspector: forward one JSON-RPC message to an MCP server, the answer as SSE")
            .description("Targets on this instance (/mcp*, /_rustybin/mcp, /mock/<name>/_rustybin/mcp) are served in process; other hosts go through the egress policy (RUSTYBIN_EGRESS_ALLOW). Events: relay (upstream status and headers, the relay session), message (one per JSON-RPC message), body (a non-JSON-RPC answer), error, done.")
            .example(
                Example::post("Relay server/discover to /mcp", RELAY_PATH)
                    .json(r#"{"target":"/mcp","headers":{"MCP-Protocol-Version":"2026-07-28","Mcp-Method":"server/discover"},"message":{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}}"#)
                    .skip_check("an SSE answer"),
            ),
        Endpoint::new(RELAY_FETCH_PATH, &["POST"], category::CONTROL, "MCP inspector: fetch one OAuth discovery or token document through the instance")
            .example(
                Example::post("Fetch protected resource metadata", RELAY_FETCH_PATH)
                    .json(r#"{"url":"/.well-known/oauth-protected-resource/mcp/protected"}"#),
            ),
        Endpoint::new("/_rustybin/mcp-relay/{session}", &["DELETE"], category::CONTROL, "MCP inspector: end a relay session")
            .example(Example::delete("End a relay session", "/_rustybin/mcp-relay/0123456789abcdef01234567").expect_status(404)),
        Endpoint::new(CHECKS_PATH, &["GET", "POST"], category::CONTROL, "MCP inspector: run the check suite against an MCP server (JSON or JUnit), or list saved runs")
            .example(Example::get("Saved check runs", CHECKS_PATH))
            .example(Example::post("Check /mcp", CHECKS_PATH).json(r#"{"target":"/mcp","checks":["connect","version","ping"],"save":false}"#)),
        Endpoint::new("/_rustybin/mcp-checks/{id}", &["GET", "DELETE"], category::CONTROL, "MCP inspector: one saved check run (format=junit for JUnit)")
            .example(Example::get("A saved run", "/_rustybin/mcp-checks/run-unknown").expect_status(404)),
        Endpoint::new(CONNECTIONS_PATH, &["GET"], category::CONTROL, "MCP inspector: saved connections (credentials as {set, hint})")
            .example(Example::get("Saved connections", CONNECTIONS_PATH)),
        Endpoint::new("/_rustybin/mcp-connections/{id}", &["GET", "PUT", "DELETE"], category::CONTROL, "MCP inspector: one saved connection; PUT and DELETE need admin")
            .example(Example::get("A saved connection", "/_rustybin/mcp-connections/unknown").expect_status(404)),
    ]
}

pub fn openapi_paths() -> Value {
    let tags = json!(["Control Plane"]);
    let id_param = |name: &str| json!([{ "name": name, "in": "path", "required": true, "schema": { "type": "string" } }]);
    let ok = |d: &str| json!({ "200": { "description": d } });
    let target_props = json!({
        "target": { "type": "string", "description": "A path on this instance (/mcp, /mcp/protected, /_rustybin/mcp, /mock/<name>/_rustybin/mcp) or an http(s) URL" },
        "headers": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Headers sent upstream (MCP routing headers, credentials)" },
        "connection": { "type": "string", "description": "A saved connection whose credentials fill in" }
    });
    let mut relay_props = target_props.clone();
    relay_props["transport"] = json!({ "type": "string", "enum": ["streamable", "legacy"] });
    relay_props["message"] = json!({ "type": "object", "description": "One JSON-RPC message" });
    relay_props["session"] =
        json!({ "type": "string", "description": "The relay session of an earlier answer" });
    relay_props["op"] = json!({ "type": "string", "enum": ["delete"] });
    let mut checks_props = target_props;
    checks_props["transport"] =
        json!({ "type": "string", "enum": ["modern", "session", "legacy"] });
    checks_props["version"] = json!({ "type": "string", "enum": [client::MODERN, "2025-11-25", "2025-06-18", "2025-03-26"] });
    checks_props["checks"] = json!({ "type": "array", "items": { "type": "string", "enum": checks::CHECKS.iter().map(|(c, _)| *c).collect::<Vec<_>>() } });
    checks_props["include_destructive"] = json!({ "type": "boolean" });
    checks_props["max_tools"] =
        json!({ "type": "integer", "minimum": 1, "maximum": checks::MAX_TOOLS });
    checks_props["ai_arguments"] = json!({ "type": "boolean", "description": "Tool arguments from the AI assist provider (else the schema generator)" });
    checks_props["save"] = json!({ "type": "boolean" });
    checks_props["format"] = json!({ "type": "string", "enum": ["json", "junit"] });
    json!({
        SERVERS_PATH: { "get": { "tags": tags, "summary": "MCP servers of this instance", "operationId": "listMcpServers", "responses": ok("{servers, relay, checks}") } },
        RELAY_PATH: { "post": {
            "tags": tags, "summary": "Relay one JSON-RPC message to an MCP server", "operationId": "mcpRelay",
            "description": "The answer streams as SSE: relay ({session, status, headers}), message per JSON-RPC message, body for a non-JSON-RPC answer, error, done. Relay sessions keep the upstream session id or the open legacy stream (at most 64, idle 10 minutes).",
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "properties": relay_props } } } },
            "responses": { "200": { "description": "text/event-stream" }, "400": { "description": "Invalid target or message" }, "403": { "description": "The egress policy refused the host" }, "404": { "description": "Unknown relay session" }, "502": { "description": "The server could not be reached" } }
        } },
        RELAY_FETCH_PATH: { "post": {
            "tags": tags, "summary": "Fetch an OAuth discovery or token document", "operationId": "mcpRelayFetch",
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["url"], "properties": {
                "url": { "type": "string" }, "method": { "type": "string", "enum": ["GET", "POST"] }, "headers": { "type": "object" }, "body": { "type": "string" } } } } } },
            "responses": { "200": { "description": "{status, headers, body, json}" }, "403": { "description": "The egress policy refused the host" } }
        } },
        "/_rustybin/mcp-relay/{session}": { "delete": { "tags": tags, "summary": "End a relay session", "operationId": "endMcpRelaySession", "parameters": id_param("session"), "responses": { "200": { "description": "Ended" }, "404": { "description": "Unknown session" } } } },
        CHECKS_PATH: {
            "get": { "tags": tags, "summary": "Saved MCP check runs", "operationId": "listMcpCheckRuns", "responses": ok("{runs, capacity}") },
            "post": {
                "tags": tags, "summary": "Run the MCP check suite", "operationId": "runMcpChecks",
                "description": "Connect, version, auth refusal, tools/list schemas, every tool called with generated arguments, unknown tool and method, bad arguments, resources, prompts, ping, routing headers (2026-07-28), the session stream and lifecycle (session transports). A report like a harness report; format junit (or Accept: application/xml) for JUnit. Saved unless save is false.",
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "properties": checks_props } } } },
                "responses": { "200": { "description": "The report (JSON or JUnit XML)" }, "400": { "description": "Invalid target, transport or check ids" } }
            }
        },
        "/_rustybin/mcp-checks/{id}": {
            "get": { "tags": tags, "summary": "A saved MCP check run", "operationId": "getMcpCheckRun", "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }, { "name": "format", "in": "query", "schema": { "type": "string", "enum": ["json", "junit"] } }], "responses": { "200": { "description": "The report" }, "404": { "description": "Unknown run" } } },
            "delete": { "tags": tags, "summary": "Delete a saved run (admin)", "operationId": "deleteMcpCheckRun", "parameters": id_param("id"), "responses": { "200": { "description": "Deleted" }, "404": { "description": "Unknown run" } } }
        },
        CONNECTIONS_PATH: { "get": { "tags": tags, "summary": "Saved MCP connections", "operationId": "listMcpConnections", "responses": ok("{connections, capacity, persisted}") } },
        "/_rustybin/mcp-connections/{id}": {
            "get": { "tags": tags, "summary": "A saved MCP connection", "operationId": "getMcpConnection", "parameters": id_param("id"), "responses": { "200": { "description": "The connection; secrets as {set, hint}" }, "404": { "description": "Unknown connection" } } },
            "put": {
                "tags": tags, "summary": "Save an MCP connection (admin)", "operationId": "putMcpConnection", "parameters": id_param("id"),
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["name", "target"], "properties": {
                    "name": { "type": "string" }, "target": { "type": "string" }, "transport": { "type": "string", "enum": ["modern", "session", "legacy"] },
                    "version": { "type": "string" }, "mode": { "type": "string", "enum": ["browser", "relay"] },
                    "auth": { "type": "object", "properties": { "kind": { "type": "string", "enum": store::AUTH_KINDS }, "name": { "type": "string" }, "location": { "type": "string", "enum": ["header", "query"] }, "username": { "type": "string" }, "headers": { "type": "array", "items": { "type": "string" } }, "oauth": { "type": "object" } } },
                    "secrets": { "type": "object", "description": "token, api_key, password, refresh_token, client_secret or header:<name>; null clears, absent keeps" } } } } } },
                "responses": { "200": { "description": "The saved connection" }, "400": { "description": "Invalid connection" } }
            },
            "delete": { "tags": tags, "summary": "Delete a saved MCP connection (admin)", "operationId": "deleteMcpConnection", "parameters": id_param("id"), "responses": { "200": { "description": "Deleted" }, "404": { "description": "Unknown connection" } } }
        }
    })
}

#[cfg(test)]
mod tests;
