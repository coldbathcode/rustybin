//! Mock MCP (Model Context Protocol) server for gateway demos.
//!
//! - Streamable HTTP at `/mcp` (POST, GET, DELETE): protocol 2026-07-28
//!   (stateless, `server/discover`, per-request `_meta` envelope, `Mcp-Method`
//!   / `Mcp-Name` header validation, multi round-trip requests,
//!   `subscriptions/listen`) and the handshake era 2025-11-25, 2025-06-18,
//!   2025-03-26 (`initialize`, `Mcp-Session-Id`, GET stream, DELETE).
//! - Legacy HTTP+SSE (2024-11-05) at `/mcp/sse` + `/mcp/messages`.
//! - Variants: `/mcp/protected` (OAuth 2.1 bearer, RFC 9728 metadata),
//!   `/mcp/apikey` (`X-API-Key`), `/mcp/servers/{name}` (tool subsets).
//!
//! All variants share one implementation and differ by [`Profile`].
//! Environment (read when the router is built):
//! `RUSTYBIN_MCP_API_KEY`, `RUSTYBIN_MCP_ALLOWED_ORIGINS` (comma list, default `*`),
//! `RUSTYBIN_MCP_ACCEPTED_AUDIENCES` (extra token audiences for `/mcp/protected`),
//! `RUSTYBIN_MCP_RESOURCE_URL` (override the protected resource identifier),
//! `RUSTYBIN_MCP_CLOCK_TICK_SECS` (default 5).

pub mod auth;
pub mod core;
pub mod data;
pub mod prompts;
pub mod protocol;
pub mod resources;
pub mod sessions;
pub mod tools;
pub mod transport_sse_legacy;
pub mod transport_streamable;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use rand::RngCore;
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::state::AppState;

pub const OPEN_PATH: &str = "/mcp";
pub const PROTECTED_PATH: &str = "/mcp/protected";
pub const APIKEY_PATH: &str = "/mcp/apikey";
/// Names accepted by `/mcp/servers/{name}`.
pub const NAMED_SERVERS: &[&str] = &["weather", "crm", "devtools"];

/// MCP-specific limits and settings.
#[derive(Clone, Debug)]
pub struct McpConfig {
    pub public_mode: bool,
    pub trust_forward: bool,
    pub max_sessions: usize,
    pub session_ttl: Duration,
    /// Lifetime cap of long-lived streams (GET stream, HTTP+SSE, subscriptions/listen).
    pub max_stream: Duration,
    pub max_task_ms: u64,
    pub max_large_output_kb: usize,
    pub max_body: usize,
    pub default_page_size: usize,
    pub tick: Duration,
    /// How long a POST waits for a result before committing to SSE.
    pub sse_deferral: Duration,
    pub keepalive: Duration,
    /// Timeout for legacy server-to-client requests (elicitation, sampling).
    pub client_request_timeout: Duration,
    pub allowed_origins: Vec<String>,
    pub api_key: Option<String>,
    pub extra_audiences: Vec<String>,
    pub resource_url: Option<String>,
}

fn env_list(key: &str) -> Option<Vec<String>> {
    std::env::var(key).ok().map(|v| {
        v.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl McpConfig {
    pub fn from_config(config: &Config) -> Self {
        let public = config.public_mode;
        let mut cfg = Self {
            public_mode: public,
            trust_forward: config.trust_forward,
            max_sessions: if public { 200 } else { 1000 },
            session_ttl: Duration::from_secs(if public { 600 } else { 1800 }),
            max_stream: Duration::from_secs(if public { 300 } else { 3600 }),
            max_task_ms: if public { 15_000 } else { 60_000 },
            max_large_output_kb: if public { 256 } else { 1024 },
            max_body: config
                .body_limit
                .min(if public { 256 * 1024 } else { 4 << 20 }),
            default_page_size: 100,
            tick: Duration::from_secs(5),
            sse_deferral: Duration::from_secs(10),
            keepalive: Duration::from_secs(15),
            client_request_timeout: Duration::from_secs(if public { 30 } else { 120 }),
            allowed_origins: vec!["*".to_string()],
            api_key: None,
            extra_audiences: Vec::new(),
            resource_url: None,
        };
        if let Some(origins) = env_list("RUSTYBIN_MCP_ALLOWED_ORIGINS").filter(|o| !o.is_empty()) {
            cfg.allowed_origins = origins;
        }
        cfg.api_key = env_nonempty("RUSTYBIN_MCP_API_KEY");
        cfg.extra_audiences = env_list("RUSTYBIN_MCP_ACCEPTED_AUDIENCES").unwrap_or_default();
        cfg.resource_url = env_nonempty("RUSTYBIN_MCP_RESOURCE_URL");
        if let Some(secs) =
            env_nonempty("RUSTYBIN_MCP_CLOCK_TICK_SECS").and_then(|s| s.parse::<u64>().ok())
        {
            cfg.tick = Duration::from_secs(secs.clamp(1, 3600));
        }
        cfg
    }

    /// Deterministic settings for tests (no environment).
    pub fn for_tests() -> Self {
        let mut cfg = Self::from_config(&Config::for_tests());
        cfg.allowed_origins = vec!["*".to_string()];
        cfg.api_key = None;
        cfg.extra_audiences = Vec::new();
        cfg.resource_url = None;
        cfg.tick = Duration::from_millis(50);
        cfg.sse_deferral = Duration::from_secs(10);
        cfg
    }
}

/// How a variant authenticates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Open,
    OAuth,
    ApiKey,
}

/// One served MCP server variant.
#[derive(Clone, Debug)]
pub struct Profile {
    /// Stable key, also the session namespace (`default`, `protected`, `apikey`, `weather`, ...).
    pub key: String,
    pub path: String,
    pub access: Access,
    /// Tool/resource/prompt subset (`None` = everything).
    pub server: Option<&'static str>,
}

impl Profile {
    pub fn open() -> Self {
        Self {
            key: "default".into(),
            path: OPEN_PATH.into(),
            access: Access::Open,
            server: None,
        }
    }

    pub fn protected() -> Self {
        Self {
            key: "protected".into(),
            path: PROTECTED_PATH.into(),
            access: Access::OAuth,
            server: None,
        }
    }

    pub fn apikey() -> Self {
        Self {
            key: "apikey".into(),
            path: APIKEY_PATH.into(),
            access: Access::ApiKey,
            server: None,
        }
    }

    pub fn named(name: &str) -> Option<Self> {
        let server = NAMED_SERVERS.iter().find(|s| **s == name)?;
        Some(Self {
            key: (*server).to_string(),
            path: format!("/mcp/servers/{server}"),
            access: Access::Open,
            server: Some(server),
        })
    }

    pub fn server_name(&self) -> String {
        match self.server {
            Some(s) => format!("rustybin-mcp-{s}"),
            None => "rustybin-mcp".to_string(),
        }
    }

    pub fn title(&self) -> String {
        match (self.server, self.access) {
            (Some(s), _) => format!("Rustybin MCP: {s}"),
            (None, Access::OAuth) => "Rustybin MCP (OAuth protected)".into(),
            (None, Access::ApiKey) => "Rustybin MCP (API key)".into(),
            (None, Access::Open) => "Rustybin MCP".into(),
        }
    }
}

/// Module state shared by all MCP handlers.
pub struct Shared {
    pub cfg: McpConfig,
    pub sessions: sessions::SessionStore,
    /// HMAC key for multi round-trip `requestState` (per process).
    pub state_key: [u8; 32],
    pub profiles: HashMap<String, Arc<Profile>>,
}

impl Shared {
    pub fn new(cfg: McpConfig) -> Self {
        let mut state_key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut state_key);
        let mut profiles = HashMap::new();
        let mut all = vec![Profile::open(), Profile::protected(), Profile::apikey()];
        all.extend(NAMED_SERVERS.iter().filter_map(|n| Profile::named(n)));
        for p in all {
            profiles.insert(p.key.clone(), Arc::new(p));
        }
        Self {
            sessions: sessions::SessionStore::new(cfg.max_sessions, cfg.session_ttl),
            cfg,
            state_key,
            profiles,
        }
    }

    pub fn profile(&self, key: &str) -> Option<Arc<Profile>> {
        self.profiles.get(key).cloned()
    }
}

type Sh = Extension<Arc<Shared>>;

async fn open_handler(State(app): State<AppState>, Extension(sh): Sh, req: Request) -> Response {
    let profile = sh.profile("default");
    transport_streamable::handle(app, sh, profile, req).await
}

async fn protected_handler(
    State(app): State<AppState>,
    Extension(sh): Sh,
    req: Request,
) -> Response {
    let profile = sh.profile("protected");
    transport_streamable::handle(app, sh, profile, req).await
}

async fn apikey_handler(State(app): State<AppState>, Extension(sh): Sh, req: Request) -> Response {
    let profile = sh.profile("apikey");
    transport_streamable::handle(app, sh, profile, req).await
}

async fn named_handler(
    State(app): State<AppState>,
    Extension(sh): Sh,
    Path(name): Path<String>,
    req: Request,
) -> Response {
    if !NAMED_SERVERS.contains(&name.as_str()) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "unknown MCP server",
                "name": name,
                "available": NAMED_SERVERS,
            })),
        )
            .into_response();
    }
    let profile = sh.profile(&name);
    transport_streamable::handle(app, sh, profile, req).await
}

async fn prm_handler(Extension(sh): Sh, headers: HeaderMap) -> Response {
    Json(auth::protected_resource_metadata(&headers, &sh.cfg)).into_response()
}

pub fn router(state: &AppState) -> Router<AppState> {
    let shared = Arc::new(Shared::new(McpConfig::from_config(&state.config)));
    router_with(shared)
}

/// Router over explicit shared state (tests use custom limits).
pub fn router_with(shared: Arc<Shared>) -> Router<AppState> {
    Router::new()
        .route(
            "/mcp",
            post(open_handler).get(open_handler).delete(open_handler),
        )
        .route(
            "/mcp/protected",
            post(protected_handler)
                .get(protected_handler)
                .delete(protected_handler),
        )
        .route(
            "/mcp/apikey",
            post(apikey_handler)
                .get(apikey_handler)
                .delete(apikey_handler),
        )
        .route(
            "/mcp/servers/{name}",
            post(named_handler).get(named_handler).delete(named_handler),
        )
        .route("/mcp/sse", get(transport_sse_legacy::sse_handler))
        .route(
            "/mcp/messages",
            post(transport_sse_legacy::messages_handler),
        )
        .route("/.well-known/oauth-protected-resource", get(prm_handler))
        .route(
            "/.well-known/oauth-protected-resource/mcp/protected",
            get(prm_handler),
        )
        .layer(Extension(shared))
}

const INIT_BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"demo","version":"1.0.0"}}}"#;
const DISCOVER_BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
const TOOLS_LIST_BODY: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
const WEATHER_BODY: &str = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_weather","arguments":{"city":"Paris"},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
const MODERN_ACCEPT: &str = "application/json, text/event-stream";

fn modern(ex: Example, method: &'static str) -> Example {
    ex.header("Accept", MODERN_ACCEPT)
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", method)
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/mcp",
            &["POST", "GET", "DELETE"],
            category::MCP,
            "MCP Streamable HTTP server (2026-07-28 stateless + 2025-xx sessions)",
        )
        .description(
            "POST one JSON-RPC message. 2026-07-28: no initialize; every request carries \
             `_meta` io.modelcontextprotocol/protocolVersion + clientCapabilities and the \
             MCP-Protocol-Version, Mcp-Method and Mcp-Name headers (validated against the body, \
             -32020 on mismatch); `server/discover`, `subscriptions/listen`, multi round-trip \
             elicitation/sampling, ttlMs/cacheScope on list results. 2025-11-25/2025-06-18/2025-03-26: \
             `initialize` returns Mcp-Session-Id (required afterwards), GET opens the server stream, \
             DELETE ends the session. Responses are JSON or SSE (progress, logs). \
             `?page_size=N` paginates list results.",
        )
        .example(
            modern(Example::post("server/discover (2026-07-28)", "/mcp"), "server/discover")
                .json(DISCOVER_BODY),
        )
        .example(
            modern(Example::post("tools/list (2026-07-28)", "/mcp"), "tools/list")
                .json(TOOLS_LIST_BODY),
        )
        .example(
            modern(Example::post("Call get_weather (2026-07-28)", "/mcp"), "tools/call")
                .header("Mcp-Name", "get_weather")
                .header("Mcp-Param-City", "Paris")
                .json(WEATHER_BODY),
        )
        .example(
            Example::post("initialize (2025-11-25 session)", "/mcp")
                .header("Accept", MODERN_ACCEPT)
                .json(INIT_BODY),
        )
        .example(
            Example::get("GET stream without a session", "/mcp")
                .header("Accept", "text/event-stream")
                .expect_status(400),
        )
        .example(Example::delete("DELETE without a session", "/mcp").expect_status(400)),
        Endpoint::new(
            "/mcp/protected",
            &["POST", "GET", "DELETE"],
            category::MCP,
            "MCP server behind OAuth 2.1 bearer auth (MCP authorization spec)",
        )
        .description(
            "Same server as /mcp. Without a valid RS256 token from the built-in IdP: 401 with \
             `WWW-Authenticate: Bearer resource_metadata=...`. The token audience must name this \
             resource (RFC 8707 `resource`). Destructive tools (cancel_order) need the \
             mcp:tools:write scope, else 403 insufficient_scope.",
        )
        .example(
            modern(Example::post("Unauthenticated discover (401)", "/mcp/protected"), "server/discover")
                .json(DISCOVER_BODY)
                .expect_status(401),
        ),
        Endpoint::new(
            "/mcp/apikey",
            &["POST", "GET", "DELETE"],
            category::MCP,
            "MCP server requiring an X-API-Key header",
        )
        .description(
            "Any non-empty X-API-Key is accepted unless RUSTYBIN_MCP_API_KEY is set. Useful to \
             demo gateway credential injection.",
        )
        .example(
            modern(Example::post("Discover with API key", "/mcp/apikey"), "server/discover")
                .header("X-API-Key", "demo-key")
                .json(DISCOVER_BODY),
        )
        .example(
            modern(Example::post("Missing API key (401)", "/mcp/apikey"), "server/discover")
                .json(DISCOVER_BODY)
                .expect_status(401),
        ),
        Endpoint::new(
            "/mcp/servers/{name}",
            &["POST", "GET", "DELETE"],
            category::MCP,
            "Named MCP servers with tool subsets: weather, crm, devtools",
        )
        .description(
            "Separate MCP servers for aggregation and routing demos: weather (get_weather, \
             get_time), crm (lookup_customer, search_orders, cancel_order), devtools (echo, \
             calculate, slow_task, failures, images, elicitation, sampling, ...). Every server \
             also has inspect_request.",
        )
        .example(
            modern(Example::post("Weather server tools/list", "/mcp/servers/weather"), "tools/list")
                .json(TOOLS_LIST_BODY),
        )
        .example(
            modern(Example::post("CRM server tools/list", "/mcp/servers/crm"), "tools/list")
                .json(TOOLS_LIST_BODY),
        )
        .example(
            modern(Example::post("Devtools server tools/list", "/mcp/servers/devtools"), "tools/list")
                .json(TOOLS_LIST_BODY),
        ),
        Endpoint::new(
            "/mcp/sse",
            &["GET"],
            category::MCP,
            "Legacy MCP HTTP+SSE transport (2024-11-05): event stream",
        )
        .description(
            "The first event is `endpoint` with the POST URL (/mcp/messages?sessionId=...); \
             responses and notifications arrive as `message` events.",
        )
        .sse()
        .example(Example::get("Open legacy SSE stream", "/mcp/sse").skip_check("long-lived SSE stream")),
        Endpoint::new(
            "/mcp/messages",
            &["POST"],
            category::MCP,
            "Legacy MCP HTTP+SSE transport (2024-11-05): message endpoint",
        )
        .description("POST JSON-RPC with ?sessionId= from the endpoint event; answers 202 and replies on the stream.")
        .example(
            Example::post("Unknown session (404)", "/mcp/messages?sessionId=unknown")
                .json(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
                .expect_status(404),
        ),
        Endpoint::new(
            "/.well-known/oauth-protected-resource",
            &["GET"],
            category::MCP,
            "OAuth Protected Resource Metadata (RFC 9728) for /mcp/protected",
        )
        .example(Example::get("Protected resource metadata", "/.well-known/oauth-protected-resource")),
        Endpoint::new(
            "/.well-known/oauth-protected-resource/mcp/protected",
            &["GET"],
            category::MCP,
            "OAuth Protected Resource Metadata (RFC 9728), path-suffixed form",
        )
        .example(Example::get(
            "Protected resource metadata (path form)",
            "/.well-known/oauth-protected-resource/mcp/protected",
        )),
    ]
}

fn jsonrpc_body() -> Value {
    json!({
        "required": true,
        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/McpJsonRpcMessage" } } }
    })
}

fn mcp_operation(summary: &str, op_id: &str, secured: Option<&str>) -> Value {
    let mut op = json!({
        "tags": ["MCP"],
        "summary": summary,
        "operationId": op_id,
        "parameters": [
            { "name": "MCP-Protocol-Version", "in": "header", "required": false, "schema": { "type": "string", "example": "2026-07-28" } },
            { "name": "Mcp-Method", "in": "header", "required": false, "schema": { "type": "string" }, "description": "Required on 2026-07-28 requests; must equal the body method" },
            { "name": "Mcp-Name", "in": "header", "required": false, "schema": { "type": "string" }, "description": "tools/call, prompts/get, resources/read (2026-07-28)" },
            { "name": "Mcp-Session-Id", "in": "header", "required": false, "schema": { "type": "string" }, "description": "Session from initialize (2025-xx)" },
            { "name": "page_size", "in": "query", "required": false, "schema": { "type": "integer" } }
        ],
        "requestBody": jsonrpc_body(),
        "responses": {
            "200": { "description": "JSON-RPC response (application/json) or SSE stream (text/event-stream)",
                     "content": { "application/json": {}, "text/event-stream": {} } },
            "202": { "description": "Notification or response accepted" },
            "400": { "description": "Invalid message, header mismatch (-32020), unsupported version (-32022), missing session" },
            "401": { "description": "Authentication required" },
            "403": { "description": "Origin not allowed or insufficient scope" },
            "404": { "description": "Unknown method (-32601, 2026-07-28) or unknown session" },
            "406": { "description": "Accept does not allow JSON or SSE" }
        }
    });
    if let Some(scheme) = secured {
        op["security"] = json!([{ scheme: [] }]);
    }
    op
}

fn mcp_path_item(name: &str, op_prefix: &str, secured: Option<&str>) -> Value {
    let mut get = mcp_operation(
        &format!("{name}: open the server-to-client SSE stream (2025-xx sessions)"),
        &format!("{op_prefix}Stream"),
        secured,
    );
    if let Some(o) = get.as_object_mut() {
        o.remove("requestBody");
    }
    let mut delete = mcp_operation(
        &format!("{name}: terminate the session (2025-xx)"),
        &format!("{op_prefix}Delete"),
        secured,
    );
    if let Some(o) = delete.as_object_mut() {
        o.remove("requestBody");
    }
    json!({
        "post": mcp_operation(&format!("{name}: send a JSON-RPC message"), &format!("{op_prefix}Post"), secured),
        "get": get,
        "delete": delete,
    })
}

pub fn openapi_paths() -> Value {
    let mut named = mcp_path_item("Named MCP server", "mcpNamedServer", None);
    for op in ["post", "get", "delete"] {
        if let Some(params) = named[op]["parameters"].as_array_mut() {
            params.insert(
                0,
                json!({ "name": "name", "in": "path", "required": true,
                        "schema": { "type": "string", "enum": NAMED_SERVERS } }),
            );
        }
    }
    let prm = json!({
        "tags": ["MCP"],
        "summary": "OAuth 2.0 Protected Resource Metadata (RFC 9728) for /mcp/protected",
        "responses": { "200": { "description": "Metadata document", "content": { "application/json": { "schema": {
            "type": "object",
            "properties": {
                "resource": { "type": "string" },
                "authorization_servers": { "type": "array", "items": { "type": "string" } },
                "scopes_supported": { "type": "array", "items": { "type": "string" } },
                "bearer_methods_supported": { "type": "array", "items": { "type": "string" } }
            }
        } } } } }
    });
    let mut prm_root = prm.clone();
    prm_root["operationId"] = json!("mcpProtectedResourceMetadata");
    let mut prm_path = prm;
    prm_path["operationId"] = json!("mcpProtectedResourceMetadataPath");
    json!({
        "/mcp": mcp_path_item("MCP", "mcp", None),
        "/mcp/protected": mcp_path_item("Protected MCP", "mcpProtected", Some("bearerAuth")),
        "/mcp/apikey": mcp_path_item("API key MCP", "mcpApiKey", Some("mcpApiKey")),
        "/mcp/servers/{name}": named,
        "/mcp/sse": { "get": {
            "tags": ["MCP"],
            "summary": "Legacy HTTP+SSE transport (2024-11-05) event stream",
            "operationId": "mcpLegacySse",
            "responses": { "200": { "description": "SSE stream; first event `endpoint`", "content": { "text/event-stream": {} } } }
        } },
        "/mcp/messages": { "post": {
            "tags": ["MCP"],
            "summary": "Legacy HTTP+SSE transport (2024-11-05) message endpoint",
            "operationId": "mcpLegacyMessages",
            "parameters": [{ "name": "sessionId", "in": "query", "required": true, "schema": { "type": "string" } }],
            "requestBody": jsonrpc_body(),
            "responses": {
                "202": { "description": "Accepted; the reply arrives on the SSE stream" },
                "400": { "description": "Invalid JSON-RPC" },
                "404": { "description": "Unknown session" }
            }
        } },
        "/.well-known/oauth-protected-resource": { "get": prm_root },
        "/.well-known/oauth-protected-resource/mcp/protected": { "get": prm_path },
    })
}

pub fn openapi_components() -> Value {
    json!({
        "schemas": {
            "McpJsonRpcMessage": {
                "type": "object",
                "description": "A JSON-RPC 2.0 request, notification or response",
                "required": ["jsonrpc"],
                "properties": {
                    "jsonrpc": { "type": "string", "enum": ["2.0"] },
                    "id": { "oneOf": [{ "type": "string" }, { "type": "integer" }] },
                    "method": { "type": "string" },
                    "params": { "type": "object" },
                    "result": { "type": "object" },
                    "error": { "type": "object" }
                }
            }
        },
        "securitySchemes": {
            "mcpApiKey": { "type": "apiKey", "in": "header", "name": "X-API-Key" }
        }
    })
}

#[cfg(test)]
mod tests;
