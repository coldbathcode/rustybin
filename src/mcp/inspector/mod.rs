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
//! | `GET /.well-known/rustybin-inspector-client.json` | open | The inspector's client ID metadata document ([`clientdoc`]), on the data plane |
//! | `GET /_rustybin/oauth/inspector-client.json` | open | The same document at its 0.7.0 address |
//!
//! "read" is the console scope (control auth lets it through as a
//! read-only POST: these routes change nothing on the instance); outbound
//! traffic is bounded by the egress policy (`Purpose::McpRelay`).

pub mod checks;
pub mod client;
pub mod clientdoc;
pub mod relay;
pub mod store;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode, Uri};
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
    /// Check runs at once (`mcp-checks`, `run_mcp_checks`).
    pub check_runs: Arc<tokio::sync::Semaphore>,
    /// Relay exchanges and `mcp_call`s at once.
    pub calls: Arc<tokio::sync::Semaphore>,
}

/// Check suites that may run at once, every caller together.
pub const MAX_CHECK_RUNS: usize = 2;
/// Relay exchanges and management `mcp_call`s that may go at once.
pub const MAX_CALLS: usize = 16;

/// The answer when every check run is taken.
pub const CHECKS_BUSY: &str =
    "the MCP checks are already running twice on this instance: try again when one ends";
/// The answer when every relay call is taken.
pub const CALLS_BUSY: &str =
    "too many MCP inspector calls at once on this instance: try again in a moment";

impl Inspector {
    pub fn new(settings: std::sync::Arc<crate::settings::Settings>) -> Self {
        Self {
            relay: relay::Relay::default(),
            store: store::Store::new(settings),
            check_runs: Arc::new(tokio::sync::Semaphore::new(MAX_CHECK_RUNS)),
            calls: Arc::new(tokio::sync::Semaphore::new(MAX_CALLS)),
        }
    }

    /// A relay or `mcp_call` slot, held for the exchange (`None`: all taken).
    pub fn call_slot(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.calls.clone().try_acquire_owned().ok()
    }
}

/// JSON-RPC methods that only read what a server offers: all a caller
/// without admin rights may send to a server on another host (the console
/// scope's relay and checks, a read-only agent's `mcp_call`). `tools/call`
/// and the rest act on that server, so they need admin.
pub const READ_ONLY_METHODS: &[&str] = &[
    "initialize",
    "server/discover",
    "ping",
    "tools/list",
    "resources/list",
    "resources/read",
    "resources/templates/list",
    "prompts/list",
    "prompts/get",
];

/// May a caller without admin rights send `method` to `target`? Anything
/// to this instance's own servers (in process); read-only methods and
/// notifications elsewhere.
pub fn allowed_without_admin(target: &client::Target, method: Option<&str>) -> bool {
    matches!(target, client::Target::Local(_))
        || method.is_some_and(|m| READ_ONLY_METHODS.contains(&m) || m.starts_with("notifications/"))
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

/// Every MCP server this instance serves, the mocks' servers filtered by
/// `can_read` (the inspector: the caller's; the registry: the instance's).
pub fn server_list(
    state: &AppState,
    can_read: impl Fn(&crate::mocks::store::Snapshot) -> bool,
) -> Vec<Value> {
    let mut list = vec![
        json!({ "id": "mcp", "path": "/mcp", "kind": "mock-mcp", "title": "Mock MCP server (open)", "transports": ["modern", "session"], "versions": versions(), "auth": { "kind": "none" }, "state": "serving" }),
        json!({ "id": "mcp-protected", "path": "/mcp/protected", "kind": "mock-mcp", "title": "Mock MCP server behind OAuth 2.1", "transports": ["modern", "session"], "versions": versions(), "auth": { "kind": "oauth", "hint": "discovery from the 401 (RFC 9728), the built-in identity provider" }, "state": "serving" }),
        json!({ "id": "mcp-apikey", "path": "/mcp/apikey", "kind": "mock-mcp", "title": "Mock MCP server behind an API key", "transports": ["modern", "session"], "versions": versions(), "auth": { "kind": "api_key", "name": "X-API-Key", "hint": "any non-empty key unless RUSTYBIN_MCP_API_KEY is set" }, "state": "serving" }),
    ];
    for name in crate::mcp::NAMED_SERVERS {
        list.push(json!({ "id": format!("mcp-{name}"), "path": format!("/mcp/servers/{name}"), "kind": "mock-mcp", "title": format!("Named server {name} (a tool subset)"), "transports": ["modern", "session"], "versions": versions(), "auth": { "kind": "none" }, "state": "serving" }));
    }
    if crate::mcp::attacks::enabled(&state.settings, state.config.public_mode) {
        for (kind, title, attack, _) in crate::mcp::attacks::KINDS {
            let auth = if *kind == "scope_overreach" {
                json!({ "kind": "oauth", "hint": "any bearer passes: the point is the scope it asks for" })
            } else {
                json!({ "kind": "none" })
            };
            list.push(json!({ "id": format!("attack-{kind}"), "path": format!("{}{kind}", crate::mcp::attacks::PATH_PREFIX), "kind": "attack-mcp", "title": format!("Attack server: {title} (misbehaves on purpose)"), "attack": attack, "transports": ["modern", "session"], "versions": versions(), "auth": auth, "state": "serving" }));
        }
    }
    list.push(json!({ "id": "mcp-sse", "path": "/mcp/sse", "kind": "mock-mcp", "title": "Mock MCP server, legacy HTTP+SSE (2024-11-05)", "transports": ["legacy"], "versions": [client::LEGACY_VERSION], "auth": { "kind": "none" }, "state": "serving" }));
    let manage_auth = match state.config.control_auth {
        ControlAuth::Open => json!({ "kind": "none" }),
        _ => {
            json!({ "kind": "bearer", "hint": "the console's control token (sign-in), the admin token, or OAuth sign-in for agents" })
        }
    };
    list.push(json!({ "id": "manage", "path": crate::mcp::manage::MANAGE_PATH, "kind": "manage", "title": "Management MCP server (configures this instance)", "transports": ["modern", "session"], "versions": versions(), "auth": manage_auth, "state": "serving" }));
    for s in state.mocks.list() {
        if !can_read(&s) {
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
    list
}

/// The answer of `GET /_rustybin/mcp-servers` for a caller (also the
/// management tool `list_mcp_servers`). `base` is the URL the caller reached
/// this instance at: the inspector's client document depends on it.
pub fn servers_json(
    state: &AppState,
    caller: &crate::mocks::selfservice::Caller,
    base: &str,
) -> Value {
    let list = server_list(state, |s| caller.can_read(s));
    let (allow, _) = state.egress.allowlist();
    json!({
        "servers": list,
        "relay": {
            "allow": allow.describe(),
            "allow_loopback": state.egress.loopback_allowed(),
            "public_mode": state.config.public_mode,
            "hint": "Servers on this instance are relayed in process. Other hosts need RUSTYBIN_EGRESS_ALLOW (Management, Network); loopback needs the loopback switch.",
        },
        "checks": checks::CHECKS.iter().map(|(id, title)| json!({ "id": id, "title": title })).collect::<Vec<_>>(),
        "client_document": clientdoc::status(base),
    })
}

async fn servers(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let caller = crate::mocks::selfservice::caller(&state, &headers, &extensions);
    let base = clientdoc::base_url(&headers, &extensions, &uri, &state);
    Json(servers_json(&state, &caller, &base)).into_response()
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

/// A check run's specification from a request body: the saved connection's
/// target and credentials, the body's headers on top, the transport and the
/// checks to run. Shared by `POST /_rustybin/mcp-checks` and the management
/// tool `run_mcp_checks`. `base_url` is the URL the caller reached this
/// instance at (the `client_document` check offers the document served there).
pub fn build_run_spec(
    inspector: &Inspector,
    req: ChecksBody,
    principal: String,
    admin: bool,
    base_url: &str,
) -> Result<checks::RunSpec, (StatusCode, String, &'static str)> {
    let (conn_target, conn_transport, conn_version, mut auth, query) = match &req.connection {
        Some(id) => match inspector.store.use_connection(id, &req.target, admin) {
            Ok(r) => (
                Some(r.conn.target),
                Some(r.conn.transport),
                r.conn.version,
                r.headers,
                r.query,
            ),
            Err(e) => {
                return Err((
                    StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_REQUEST),
                    e.message,
                    e.reason,
                ))
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
        Err(e) => return Err((StatusCode::BAD_REQUEST, e, "invalid_target")),
    };
    let transport = match Transport::parse(
        req.transport.as_deref().or(conn_transport.as_deref()),
        req.version.as_deref().or(conn_version.as_deref()),
    ) {
        Ok(t) => t,
        Err(e) => return Err((StatusCode::BAD_REQUEST, e, "invalid_transport")),
    };
    match relay::header_list(&req.headers) {
        Ok(h) => {
            for (k, v) in h {
                auth.retain(|(ak, _)| !ak.eq_ignore_ascii_case(&k));
                auth.push((k, v));
            }
        }
        Err(e) => return Err((StatusCode::BAD_REQUEST, e, "invalid_headers")),
    }
    let only = match checks::parse_only(&req.checks) {
        Ok(o) => o,
        Err(e) => return Err((StatusCode::BAD_REQUEST, e, "invalid_checks")),
    };
    Ok(checks::RunSpec {
        // Tool calls go to another host's server only for admin callers.
        call_tools: admin || matches!(target, Target::Local(_)),
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
        base_url: base_url.to_string(),
    })
}

async fn run_checks(
    State(state): State<AppState>,
    Extension(inspector): Extension<Arc<Inspector>>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
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
    let format = req.format.clone();
    // Saved credentials and calls that act on a server elsewhere: `mcp.connections`.
    let caller = crate::control_auth::open_caller(&headers, &state.config);
    let admin = caller.has(crate::rbac::Permission::McpConnections);
    let save = req.save.unwrap_or(true) && keeps_runs(&state.config, &caller);
    let base = clientdoc::base_url(&headers, &extensions, &uri, &state);
    let spec = match build_run_spec(&inspector, req, principal, admin, &base) {
        Ok(s) => s,
        Err((status, message, reason)) => return error(status, message, reason),
    };
    let Some(report) = run_and_save(&state, &inspector, spec, save).await else {
        return error(StatusCode::SERVICE_UNAVAILABLE, CHECKS_BUSY, "busy");
    };
    if wants_junit(&headers, format.as_deref()) {
        return junit_response(&report);
    }
    Json(report).into_response()
}

/// Does this caller read and keep saved check runs? The 50 saved runs are one
/// list, so a visitor of a public instance runs checks and keeps the answer
/// only: keeping theirs would show every visitor the others' targets, and let
/// one push the others' runs out.
pub fn keeps_runs(config: &crate::config::Config, caller: &crate::rbac::Resolved) -> bool {
    !config.public_mode || caller.has(crate::rbac::Permission::McpConnections)
}

/// Run the checks, log the run and keep its report (unless `save` is
/// false). Shared with the management tool `run_mcp_checks`. `None` when
/// [`MAX_CHECK_RUNS`] runs are going already.
pub async fn run_and_save(
    state: &AppState,
    inspector: &Inspector,
    spec: checks::RunSpec,
    save: bool,
) -> Option<Value> {
    let _slot = inspector.check_runs.clone().try_acquire_owned().ok()?;
    let target = spec
        .target
        .as_str()
        .split('?')
        .next()
        .unwrap_or("")
        .to_string();
    let report = checks::run(state, spec).await;
    tracing::info!(target: "rustybin::mcp_inspector", target = %target, ok = %report["ok"], "mcp checks run");
    if save {
        if let Err(e) = inspector.store.save_run(&report) {
            tracing::warn!(target: "rustybin::mcp_inspector", "check run not saved: {e}");
        }
    }
    Some(report)
}

async fn list_runs(
    State(state): State<AppState>,
    Extension(inspector): Extension<Arc<Inspector>>,
    headers: HeaderMap,
) -> Response {
    let caller = crate::control_auth::open_caller(&headers, &state.config);
    let runs = if keeps_runs(&state.config, &caller) {
        inspector.store.runs()
    } else {
        Vec::new()
    };
    Json(json!({ "runs": runs, "capacity": store::MAX_RUNS })).into_response()
}

#[derive(Deserialize, Default)]
struct FormatQuery {
    format: Option<String>,
}

async fn get_run(
    State(state): State<AppState>,
    Extension(inspector): Extension<Arc<Inspector>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<FormatQuery>,
) -> Response {
    let caller = crate::control_auth::open_caller(&headers, &state.config);
    let run = inspector
        .store
        .run(&id)
        .filter(|_| keeps_runs(&state.config, &caller));
    match run {
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
    Extension(inspector): Extension<Arc<Inspector>>,
    Path(id): Path<String>,
) -> Response {
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
    Extension(inspector): Extension<Arc<Inspector>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
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
    Extension(inspector): Extension<Arc<Inspector>>,
    Path(id): Path<String>,
) -> Response {
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
    let inspector = state.mcp_inspector.clone();
    Router::new()
        .route(SERVERS_PATH, get(servers))
        .route(clientdoc::DOCUMENT_PATH, get(clientdoc::handler))
        .route(
            clientdoc::LEGACY_DOCUMENT_PATH,
            get(clientdoc::legacy_handler),
        )
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
.permission(crate::rbac::Permission::McpInspect)
            .example(Example::get("MCP servers of this instance", SERVERS_PATH)),
        Endpoint::new(RELAY_PATH, &["POST"], category::CONTROL, "MCP inspector: forward one JSON-RPC message to an MCP server, the answer as SSE")
.permission(crate::rbac::Permission::McpInspect)
            .description("Targets on this instance (/mcp*, /_rustybin/mcp, /mock/<name>/_rustybin/mcp) are served in process; other hosts go through the egress policy (RUSTYBIN_EGRESS_ALLOW). Events: relay (upstream status and headers, the relay session), message (one per JSON-RPC message), body (a non-JSON-RPC answer), error, done.")
            .example(
                Example::post("Relay server/discover to /mcp", RELAY_PATH)
                    .json(r#"{"target":"/mcp","headers":{"MCP-Protocol-Version":"2026-07-28","Mcp-Method":"server/discover"},"message":{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}}"#)
                    .skip_check("an SSE answer"),
            ),
        Endpoint::new(RELAY_FETCH_PATH, &["POST"], category::CONTROL, "MCP inspector: fetch one OAuth discovery or token document through the instance")
.permission(crate::rbac::Permission::McpInspect)
            .example(
                Example::post("Fetch protected resource metadata", RELAY_FETCH_PATH)
                    .json(r#"{"url":"/.well-known/oauth-protected-resource/mcp/protected"}"#),
            ),
        Endpoint::new("/_rustybin/mcp-relay/{session}", &["DELETE"], category::CONTROL, "MCP inspector: end a relay session")
.permission(crate::rbac::Permission::McpInspect)
            .example(Example::delete("End a relay session", "/_rustybin/mcp-relay/0123456789abcdef01234567").expect_status(404)),
        Endpoint::new(CHECKS_PATH, &["GET", "POST"], category::CONTROL, "MCP inspector: run the check suite against an MCP server (JSON or JUnit), or list saved runs")
.permission(crate::rbac::Permission::McpInspect)
            .example(Example::get("Saved check runs", CHECKS_PATH))
            .example(Example::post("Check /mcp", CHECKS_PATH).json(r#"{"target":"/mcp","checks":["connect","version","ping"],"save":false}"#)),
        Endpoint::new("/_rustybin/mcp-checks/{id}", &["GET", "DELETE"], category::CONTROL, "MCP inspector: one saved check run (format=junit for JUnit)")
.permission_for(&["GET"], crate::rbac::Permission::McpInspect).permission_for(&["DELETE"], crate::rbac::Permission::McpConnections)
            .example(Example::get("A saved run", "/_rustybin/mcp-checks/run-unknown").expect_status(404)),
        Endpoint::new(CONNECTIONS_PATH, &["GET"], category::CONTROL, "MCP inspector: saved connections (credentials as {set, hint})")
.permission(crate::rbac::Permission::McpInspect)
            .example(Example::get("Saved connections", CONNECTIONS_PATH)),
        Endpoint::new("/_rustybin/mcp-connections/{id}", &["GET", "PUT", "DELETE"], category::CONTROL, "MCP inspector: one saved connection; PUT and DELETE need admin")
.permission_for(&["GET"], crate::rbac::Permission::McpInspect).permission_for(&["PUT", "DELETE"], crate::rbac::Permission::McpConnections)
            .example(Example::get("A saved connection", "/_rustybin/mcp-connections/unknown").expect_status(404)),
        Endpoint::new(clientdoc::DOCUMENT_PATH, &["GET"], category::CONTROL, "MCP inspector: its client ID metadata document (the client id the OAuth walk sends where a server supports one)")
            .description("Public, like the other OAuth metadata, and outside the control plane so that any authorization server may fetch it (another Rustybin's outbound policy never calls a /_rustybin path): the inspector's name and the console's callback as its only redirect URI. No credential, API key or session. An authorization server that says client_id_metadata_document_supported fetches it, so it is usable only when this instance is reached over https on a host that is not loopback (GET /_rustybin/mcp-servers says which under client_document).")
            .example(Example::get("The inspector's client document", clientdoc::DOCUMENT_PATH)),
        Endpoint::new(clientdoc::LEGACY_DOCUMENT_PATH, &["GET"], category::CONTROL, "MCP inspector: its client document at the address 0.7.0 used (kept for saved connections; the walk sends /.well-known/rustybin-inspector-client.json)")
.open()
            .description("The same document but for its client_id, which is this address. Kept so a server that knows the inspector by it still can read it; the OAuth walk and the client_document check send the address outside the control plane.")
            .example(Example::get("The inspector's client document, 0.7.0 address", clientdoc::LEGACY_DOCUMENT_PATH)),
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
        SERVERS_PATH: { "get": { "tags": tags, "summary": "MCP servers of this instance", "operationId": "listMcpServers", "responses": ok("{servers, relay, checks, client_document: {url, legacy_url, redirect_uri, usable, reason, document, ways}}") } },
        clientdoc::LEGACY_DOCUMENT_PATH: { "get": {
            "tags": tags, "summary": "The MCP inspector's client document at its 0.7.0 address", "operationId": "getInspectorClientDocumentLegacy", "security": [],
            "description": "The document of /.well-known/rustybin-inspector-client.json with this address as its client_id. Kept for saved connections and servers that know the inspector by it; the OAuth walk sends the other address. No credentials; nothing secret.",
            "responses": ok("The document"),
        } },
        clientdoc::DOCUMENT_PATH: { "get": {
            "tags": tags, "summary": "The MCP inspector's client ID metadata document", "operationId": "getInspectorClientDocument", "security": [],
            "description": "The document whose URL the inspector's OAuth walk sends as its client id to an authorization server that supports client ID metadata documents. Outside the control plane, so any authorization server may fetch it. No credentials, API key or session; nothing secret.",
            "responses": { "200": { "description": "The document", "content": { "application/json": { "schema": { "type": "object", "required": ["client_id", "redirect_uris"], "properties": {
                "client_id": { "type": "string", "description": "This document's own URL" },
                "client_name": { "type": "string" },
                "client_uri": { "type": "string" },
                "redirect_uris": { "type": "array", "items": { "type": "string" }, "description": "The console's callback" },
                "grant_types": { "type": "array", "items": { "type": "string" } },
                "response_types": { "type": "array", "items": { "type": "string" } },
                "token_endpoint_auth_method": { "type": "string", "enum": ["none"] } } } } } } }
        } },
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
                    "auth": { "type": "object", "properties": { "kind": { "type": "string", "enum": store::AUTH_KINDS }, "name": { "type": "string" }, "location": { "type": "string", "enum": ["header", "query"] }, "username": { "type": "string" }, "headers": { "type": "array", "items": { "type": "string" } }, "oauth": { "type": "object", "description": "The OAuth facts worth keeping, never a secret: issuer, token_endpoint, client_id, scope, and client (how the walk names its client)", "properties": { "client": { "type": "string", "enum": clientdoc::CLIENT_WAYS } } } } },
                    "secrets": { "type": "object", "description": "token, api_key, password, refresh_token, client_secret or header:<name>; null clears, absent keeps" } } } } } },
                "responses": { "200": { "description": "The saved connection" }, "400": { "description": "Invalid connection" } }
            },
            "delete": { "tags": tags, "summary": "Delete a saved MCP connection (admin)", "operationId": "deleteMcpConnection", "parameters": id_param("id"), "responses": { "200": { "description": "Deleted" }, "404": { "description": "Unknown connection" } } }
        }
    })
}

#[cfg(test)]
mod tests;
