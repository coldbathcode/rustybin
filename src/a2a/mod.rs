//! Mock A2A (Agent2Agent protocol) agents for gateway routing demos.
//!
//! Several named agents (`echo`, `weather`, `travel-planner`, `approval`,
//! `flaky`, `secure`, `reject`), each with its own Agent Card and endpoint
//! under `/a2a/{agent}`, speaking both protocol generations:
//!
//! - v1.0 (`A2A-Version: 1.0`): JSON-RPC methods `SendMessage`, ... and the
//!   HTTP+JSON binding under `/a2a/{agent}/v1/...` (`/message:send`, ...).
//! - v0.3 (no header, or `A2A-Version: 0.3`): JSON-RPC methods
//!   `message/send`, `tasks/get`, ... and the v0.3 HTTP+JSON binding under
//!   the same `/a2a/{agent}/v1/...` paths.
//!
//! Discovery: `/.well-known/agent-card.json` (default agent, lists all
//! agents), `/.well-known/agent.json` (legacy v0.3 card) and
//! `/a2a/{agent}/.well-known/agent-card.json`. Push notifications go to the
//! built-in sink `/a2a/webhook-sink/{id}` or allowlisted hosts only.
//!
//! Files: `model` (canonical types), `wire` (JSON shapes per version),
//! `agents` (behaviour), `tasks` (bounded store + operations), `card`,
//! `jsonrpc`, `rest`, `push` (SSRF policy, delivery, sink), `errors`.

pub mod agents;
pub mod card;
pub mod errors;
pub mod jsonrpc;
pub mod model;
pub mod push;
pub mod rest;
pub mod tasks;
pub mod wire;

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Extension, Path, RawQuery};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use serde_json::{json, Value};
use sha2::Digest;
use tokio::sync::broadcast;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::jwt_state::JwtState;
use crate::session::Session;
use crate::state::AppState;

use agents::{AgentDef, AuthOutcome};
use model::{StreamEvent, Version};
use push::{PushPolicy, PushService, WebhookSink};
use tasks::{Limits, Service};

/// Longest a single stream (SSE) stays open.
pub const MAX_STREAM: Duration = Duration::from_secs(10 * 60);
/// Longest a blocking SendMessage waits for the task to settle.
pub const MAX_BLOCKING_WAIT: Duration = Duration::from_secs(100);

/// Module state (attached with `Extension`).
pub struct A2a {
    pub config: Arc<Config>,
    pub jwt: Arc<JwtState>,
    pub svc: Arc<Service>,
}

impl A2a {
    pub fn new(config: Arc<Config>, jwt: Arc<JwtState>, policy: PushPolicy) -> Self {
        let public = config.public_mode;
        let sink = Arc::new(WebhookSink::new(
            if public { 100 } else { 500 },
            50,
            Duration::from_secs(60 * 60),
        ));
        let svc = Arc::new(Service::new(
            Limits::for_mode(public),
            PushService::new(policy, sink),
        ));
        Self { config, jwt, svc }
    }
}

// ── Request context ─────────────────────────────────────────────────

/// Externally visible origin of this server, as seen by the client.
#[derive(Clone, Debug)]
pub struct Origin {
    /// `scheme://host[:port]`, no trailing slash.
    pub base_url: String,
    /// `host[:port]`, lowercase.
    pub authority: String,
}

/// Derive the origin with [`crate::session::request_origin`]: the listener
/// scheme and port, the `Host` header (or HTTP/2 authority), and the
/// `Forwarded` / `X-Forwarded-*` headers only when `RUSTYBIN_TRUST_FORWARD`
/// is on.
pub fn origin(headers: &HeaderMap, extensions: &Extensions, uri: &Uri, config: &Config) -> Origin {
    let o = crate::session::request_origin(headers, extensions, uri, config);
    Origin {
        base_url: o.base_url(),
        authority: o.authority(),
    }
}

/// Raw `A2A-Version` from the header or the `A2A-Version` query parameter.
pub fn requested_version(headers: &HeaderMap, query: Option<&str>) -> String {
    if let Some(v) = headers.get("a2a-version").and_then(|v| v.to_str().ok()) {
        return v.trim().to_string();
    }
    if let Some(q) = query {
        for (k, v) in form_urlencoded::parse(q.as_bytes()) {
            if k.eq_ignore_ascii_case("a2a-version") {
                return v.trim().to_string();
            }
        }
    }
    String::new()
}

/// Verify the bearer token (built-in IdP, RS256) for secured agents.
pub fn authenticate(jwt: &JwtState, headers: &HeaderMap, agent: &AgentDef) -> AuthOutcome {
    if !agent.secured {
        return AuthOutcome::Missing;
    }
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return AuthOutcome::Missing;
    };
    let mut parts = value.trim().splitn(2, ' ');
    let scheme = parts.next().unwrap_or("");
    let token = parts.next().unwrap_or("").trim();
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
        return AuthOutcome::Invalid("expected an Authorization: Bearer token".into());
    }
    match jwt.verify_rs256(token) {
        Ok(claims) => AuthOutcome::Valid(claims),
        Err(e) => AuthOutcome::Invalid(e),
    }
}

/// Everything a binding needs about the request.
pub struct Req {
    pub agent: &'static AgentDef,
    pub owner: String,
    pub origin: Origin,
    pub version_raw: String,
    pub auth: AuthOutcome,
}

impl Req {
    pub fn caller(&self, version: Version) -> tasks::Caller<'_> {
        tasks::Caller {
            owner: &self.owner,
            agent: self.agent,
            base_url: &self.origin.base_url,
            authority: &self.origin.authority,
            auth: &self.auth,
            version,
        }
    }
}

fn build_req(
    a2a: &A2a,
    agent: &'static AgentDef,
    owner: String,
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
    query: Option<&str>,
) -> Req {
    Req {
        agent,
        owner,
        origin: origin(headers, extensions, uri, &a2a.config),
        version_raw: requested_version(headers, query),
        auth: authenticate(&a2a.jwt, headers, agent),
    }
}

fn unknown_agent(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": "unknown A2A agent",
            "agent": name,
            "agents": agents::AGENTS.iter().map(|a| a.id).collect::<Vec<_>>(),
            "hint": "GET /a2a lists the demo agents",
        })),
    )
        .into_response()
}

// ── Shared response helpers ─────────────────────────────────────────

/// JSON response with a content type and the negotiated `A2A-Version`.
pub fn json_response(
    status: StatusCode,
    body: &Value,
    content_type: &'static str,
    version: Option<Version>,
) -> Response {
    let mut resp = (status, body.to_string()).into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Some(v) = version {
        h.insert("a2a-version", HeaderValue::from_static(v.as_str()));
    }
    resp
}

/// Server-Sent Events: `first`, then events from `rx` until one ends the
/// stream (terminal or interrupted state), the channel closes, or
/// [`MAX_STREAM`] elapses. `render` produces each `data:` payload.
pub fn sse_response<F>(
    first: StreamEvent,
    rx: Option<broadcast::Receiver<StreamEvent>>,
    render: F,
    version: Version,
) -> Response
where
    F: Fn(&StreamEvent) -> String + Send + Sync + 'static,
{
    let stream = async_stream::stream! {
        let first_ends = matches!(first, StreamEvent::Message(_));
        yield Ok::<Event, Infallible>(Event::default().data(render(&first)));
        if first_ends {
            return;
        }
        if let Some(mut rx) = rx {
            let deadline = tokio::time::Instant::now() + MAX_STREAM;
            loop {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(Ok(ev)) => {
                        let ends = ev.ends_stream();
                        yield Ok(Event::default().data(render(&ev)));
                        if ends {
                            break;
                        }
                    }
                    Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                    Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => break,
                }
            }
        }
    };
    let mut resp = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response();
    let h = resp.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    h.insert("a2a-version", HeaderValue::from_static(version.as_str()));
    resp
}

/// Agent Card response with caching headers (`Cache-Control`, `ETag`,
/// `If-None-Match` -> 304).
fn card_response(headers: &HeaderMap, card: &Value) -> Response {
    let body = card.to_string();
    let digest = sha2::Sha256::digest(body.as_bytes());
    let etag: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    let etag = format!("\"{etag}\"");
    let matches = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"));
    let mut resp = if matches {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let mut r = body.into_response();
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        r
    };
    let h = resp.headers_mut();
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    h.insert(header::VARY, HeaderValue::from_static("Host"));
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    resp
}

// ── Handlers ────────────────────────────────────────────────────────

fn default_agent() -> &'static AgentDef {
    agents::find(agents::DEFAULT_AGENT).unwrap_or(&agents::AGENTS[0])
}

async fn well_known_card(
    Extension(a2a): Extension<Arc<A2a>>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let o = origin(&headers, &extensions, &uri, &a2a.config);
    card_response(&headers, &card::hybrid(&o.base_url, default_agent(), true))
}

async fn well_known_legacy(
    Extension(a2a): Extension<Arc<A2a>>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let o = origin(&headers, &extensions, &uri, &a2a.config);
    card_response(
        &headers,
        &card::v03(&o.base_url, default_agent(), card::CardKind::Public),
    )
}

async fn agent_card(
    Extension(a2a): Extension<Arc<A2a>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let Some(agent) = agents::find(&name) else {
        return unknown_agent(&name);
    };
    let o = origin(&headers, &extensions, &uri, &a2a.config);
    card_response(
        &headers,
        &card::hybrid(&o.base_url, agent, agent.id == agents::DEFAULT_AGENT),
    )
}

async fn agent_legacy_card(
    Extension(a2a): Extension<Arc<A2a>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let Some(agent) = agents::find(&name) else {
        return unknown_agent(&name);
    };
    let o = origin(&headers, &extensions, &uri, &a2a.config);
    card_response(
        &headers,
        &card::v03(&o.base_url, agent, card::CardKind::Public),
    )
}

async fn directory(
    Extension(a2a): Extension<Arc<A2a>>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let o = origin(&headers, &extensions, &uri, &a2a.config);
    Json(card::directory_index(&o.base_url)).into_response()
}

async fn rpc_default(
    Extension(a2a): Extension<Arc<A2a>>,
    Session(owner): Session,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    let req = build_req(
        &a2a,
        default_agent(),
        owner,
        &headers,
        &extensions,
        &uri,
        query.as_deref(),
    );
    jsonrpc::handle(&a2a, req, &body).await
}

#[allow(clippy::too_many_arguments)]
async fn rpc_agent(
    Extension(a2a): Extension<Arc<A2a>>,
    Path(name): Path<String>,
    Session(owner): Session,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    let Some(agent) = agents::find(&name) else {
        return unknown_agent(&name);
    };
    let req = build_req(
        &a2a,
        agent,
        owner,
        &headers,
        &extensions,
        &uri,
        query.as_deref(),
    );
    jsonrpc::handle(&a2a, req, &body).await
}

#[allow(clippy::too_many_arguments)]
async fn rest_agent(
    Extension(a2a): Extension<Arc<A2a>>,
    Path((name, rest)): Path<(String, String)>,
    Session(owner): Session,
    method: Method,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    let Some(agent) = agents::find(&name) else {
        return unknown_agent(&name);
    };
    let req = build_req(
        &a2a,
        agent,
        owner,
        &headers,
        &extensions,
        &uri,
        query.as_deref(),
    );
    rest::handle(&a2a, req, &method, &rest, query.as_deref(), &body).await
}

// ── Webhook sink ────────────────────────────────────────────────────

fn sink_bad_id() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": "sink ids are 1-64 characters of A-Z a-z 0-9 . _ -" })),
    )
        .into_response()
}

async fn sink_post(
    Extension(a2a): Extension<Arc<A2a>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !push::valid_sink_id(&id) {
        return sink_bad_id();
    }
    let keep = [
        "content-type",
        "authorization",
        "x-a2a-notification-token",
        "a2a-version",
        "user-agent",
    ];
    let mut h = serde_json::Map::new();
    for k in keep {
        if let Some(v) = headers.get(k).and_then(|v| v.to_str().ok()) {
            h.insert(k.to_string(), Value::String(v.to_string()));
        }
    }
    let parsed = serde_json::from_slice::<Value>(&body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into_owned()));
    let count = a2a
        .svc
        .push
        .sink
        .record(&id, Value::Object(h), parsed, "http");
    Json(json!({ "received": true, "id": id, "count": count })).into_response()
}

async fn sink_get(Extension(a2a): Extension<Arc<A2a>>, Path(id): Path<String>) -> Response {
    if !push::valid_sink_id(&id) {
        return sink_bad_id();
    }
    let items = a2a.svc.push.sink.list(&id);
    Json(json!({ "id": id, "count": items.len(), "notifications": items })).into_response()
}

async fn sink_delete(Extension(a2a): Extension<Arc<A2a>>, Path(id): Path<String>) -> Response {
    if !push::valid_sink_id(&id) {
        return sink_bad_id();
    }
    let existed = a2a.svc.push.sink.clear(&id);
    Json(json!({ "id": id, "deleted": existed })).into_response()
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

pub fn router(state: &AppState) -> Router<AppState> {
    let a2a = Arc::new(A2a::new(
        state.config.clone(),
        state.jwt.clone(),
        PushPolicy::from_env(state.config.public_mode),
    ));
    router_with(a2a)
}

/// Router for a prepared module state (tests use a custom push policy).
pub fn router_with(a2a: Arc<A2a>) -> Router<AppState> {
    Router::new()
        .route("/.well-known/agent-card.json", get(well_known_card))
        .route("/.well-known/agent.json", get(well_known_legacy))
        .route("/a2a", get(directory).post(rpc_default))
        .route(
            "/a2a/webhook-sink/{id}",
            get(sink_get).post(sink_post).delete(sink_delete),
        )
        .route("/a2a/{agent}", get(agent_card).post(rpc_agent))
        .route("/a2a/{agent}/.well-known/agent-card.json", get(agent_card))
        .route(
            "/a2a/{agent}/.well-known/agent.json",
            get(agent_legacy_card),
        )
        .route("/a2a/{agent}/v1/{*rest}", any(rest_agent))
        .layer(Extension(a2a))
}

const SEND_V1: &str = r#"{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"message":{"messageId":"m-1","role":"ROLE_USER","parts":[{"text":"hello agent"}]}}}"#;
const SEND_V03: &str = r#"{"jsonrpc":"2.0","id":1,"method":"message/send","params":{"message":{"kind":"message","messageId":"m-1","role":"user","parts":[{"kind":"text","text":"hello agent"}]}}}"#;
const STREAM_V1: &str = r#"{"jsonrpc":"2.0","id":1,"method":"SendStreamingMessage","params":{"message":{"messageId":"m-2","role":"ROLE_USER","parts":[{"text":"Plan a 2 day trip to Kyoto"}],"metadata":{"stepDelayMs":200}}}}"#;
const WEATHER_V1: &str = r#"{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"message":{"messageId":"m-3","role":"ROLE_USER","parts":[{"text":"What is the weather in Paris?"}]}}}"#;
const APPROVAL_V1: &str = r#"{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"message":{"messageId":"m-4","role":"ROLE_USER","parts":[{"text":"Approve my $120 taxi expense"}]}}}"#;
const LIST_V1: &str = r#"{"jsonrpc":"2.0","id":1,"method":"ListTasks","params":{"pageSize":10}}"#;
const GET_V1: &str = r#"{"jsonrpc":"2.0","id":1,"method":"GetTask","params":{"id":"00000000-0000-0000-0000-000000000000","historyLength":5}}"#;
const EXT_CARD_V1: &str = r#"{"jsonrpc":"2.0","id":1,"method":"GetExtendedAgentCard"}"#;
const REST_SEND: &str =
    r#"{"message":{"messageId":"m-5","role":"ROLE_USER","parts":[{"text":"hello over REST"}]}}"#;
const SINK_BODY: &str = r#"{"statusUpdate":{"taskId":"t-1","contextId":"c-1","status":{"state":"TASK_STATE_COMPLETED"}}}"#;

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new("/.well-known/agent-card.json", &["GET"], category::A2A, "A2A Agent Card (v1.0, readable by v0.3 clients) of the default echo agent, listing every demo agent")
            .description("Card URLs are derived from the listener (http or https) and the Host header (Forwarded / X-Forwarded-* only with RUSTYBIN_TRUST_FORWARD). Cache-Control and ETag are set.")
            .example(Example::get("Default agent card", "/.well-known/agent-card.json")),
        Endpoint::new("/.well-known/agent.json", &["GET"], category::A2A, "Legacy A2A v0.3 Agent Card (url + preferredTransport) of the default agent")
            .example(Example::get("Legacy v0.3 card", "/.well-known/agent.json")),
        Endpoint::new("/a2a", &["GET", "POST"], category::A2A, "GET: directory of demo agents; POST: JSON-RPC endpoint of the default echo agent")
            .example(Example::get("List A2A agents", "/a2a"))
            .example(Example::post("SendMessage to the default agent (v1.0)", "/a2a").header("A2A-Version", "1.0").json(SEND_V1)),
        Endpoint::new("/a2a/{agent}", &["GET", "POST"], category::A2A, "A2A JSON-RPC endpoint per agent (v1.0 methods with A2A-Version: 1.0, v0.3 methods without); GET returns the agent card")
            .description("Agents: echo, weather, travel-planner, approval, flaky, secure, reject. v1.0: SendMessage, SendStreamingMessage, GetTask, ListTasks, CancelTask, SubscribeToTask, Create/Get/List/DeleteTaskPushNotificationConfig, GetExtendedAgentCard. v0.3: message/send, message/stream, tasks/get, tasks/cancel, tasks/resubscribe, tasks/pushNotificationConfig/set|get|list|delete, agent/getAuthenticatedExtendedCard.")
            .example(Example::get("Echo agent card", "/a2a/echo"))
            .example(Example::post("SendMessage (v1.0)", "/a2a/echo").header("A2A-Version", "1.0").json(SEND_V1))
            .example(Example::post("message/send (v0.3)", "/a2a/echo").json(SEND_V03))
            .example(Example::post("Weather forecast (data artifact)", "/a2a/weather").header("A2A-Version", "1.0").json(WEATHER_V1))
            .example(Example::post("Approval (input-required)", "/a2a/approval").header("A2A-Version", "1.0").json(APPROVAL_V1))
            .example(Example::post("Travel planner stream (SSE)", "/a2a/travel-planner").header("A2A-Version", "1.0").header("Accept", "text/event-stream").json(STREAM_V1))
            .example(Example::post("ListTasks", "/a2a/echo").header("A2A-Version", "1.0").json(LIST_V1))
            .example(Example::post("GetTask (unknown id: TaskNotFoundError)", "/a2a/echo").header("A2A-Version", "1.0").json(GET_V1))
            .example(Example::post("GetExtendedAgentCard (needs a token)", "/a2a/secure").header("A2A-Version", "1.0").json(EXT_CARD_V1).expect_status(401)),
        Endpoint::new("/a2a/{agent}/.well-known/agent-card.json", &["GET"], category::A2A, "Agent Card of one agent (v1.0 + v0.3 fields)")
            .example(Example::get("Travel planner card", "/a2a/travel-planner/.well-known/agent-card.json"))
            .example(Example::get("Secure agent card (OAuth2 / bearer)", "/a2a/secure/.well-known/agent-card.json")),
        Endpoint::new("/a2a/{agent}/.well-known/agent.json", &["GET"], category::A2A, "Legacy v0.3 Agent Card of one agent")
            .example(Example::get("Weather legacy card", "/a2a/weather/.well-known/agent.json")),
        Endpoint::new("/a2a/{agent}/v1/{*rest}", &["ANY"], category::A2A, "A2A HTTP+JSON (REST) binding: message:send, message:stream, tasks, tasks/{id}, tasks/{id}:cancel, tasks/{id}:subscribe, push configs, extendedAgentCard")
            .description("v1.0 with A2A-Version: 1.0 (ProtoJSON, google.rpc.Status errors), v0.3 otherwise. Paths mirror the proto: POST /message:send, POST /message:stream, GET /tasks, GET /tasks/{id}, POST /tasks/{id}:cancel, GET|POST /tasks/{id}:subscribe, POST|GET /tasks/{id}/pushNotificationConfigs, GET|DELETE /tasks/{id}/pushNotificationConfigs/{configId}, GET /extendedAgentCard, GET /card (v0.3).")
            .example(Example::post("REST message:send (v1.0)", "/a2a/echo/v1/message:send").header("A2A-Version", "1.0").json(REST_SEND))
            .example(Example::get("REST list tasks (v1.0)", "/a2a/echo/v1/tasks?pageSize=10").header("A2A-Version", "1.0"))
            .example(Example::get("REST get unknown task (404)", "/a2a/echo/v1/tasks/00000000-0000-0000-0000-000000000000").header("A2A-Version", "1.0").expect_status(404)),
        Endpoint::new("/a2a/webhook-sink/{id}", &["GET", "POST", "DELETE"], category::A2A, "Built-in push notification sink: POST records a notification, GET lists them, DELETE clears")
            .description("Use http(s)://<this host>/a2a/webhook-sink/{id} (or just /a2a/webhook-sink/{id}) as the push URL: delivered in-process, no SSRF risk. Other targets need RUSTYBIN_A2A_PUSH_ALLOWLIST.")
            .example(Example::post("Record a notification", "/a2a/webhook-sink/demo").json(SINK_BODY))
            .example(Example::get("List notifications", "/a2a/webhook-sink/demo"))
            .example(Example::delete("Clear the sink", "/a2a/webhook-sink/demo")),
    ]
}

fn op(tag_summary: &str, op_id: &str, description: &str) -> Value {
    json!({
        "tags": ["A2A Agents"],
        "summary": tag_summary,
        "operationId": op_id,
        "description": description,
        "parameters": [
            { "name": "A2A-Version", "in": "header", "required": false, "schema": { "type": "string", "enum": ["1.0", "0.3"] },
              "description": "Protocol version; empty means 0.3" }
        ],
        "responses": { "200": { "description": "A2A response", "content": { "application/json": { "schema": { "type": "object" } } } } }
    })
}

fn with_agent_param(mut v: Value) -> Value {
    if let Some(params) = v.get_mut("parameters").and_then(|p| p.as_array_mut()) {
        params.push(json!({ "name": "agent", "in": "path", "required": true,
            "schema": { "type": "string", "enum": agents::AGENTS.iter().map(|a| a.id).collect::<Vec<_>>() } }));
    }
    v
}

fn with_body(mut v: Value) -> Value {
    v["requestBody"] = json!({ "required": true, "content": { "application/json": { "schema": { "type": "object" } } } });
    v
}

pub fn openapi_paths() -> Value {
    let rpc_desc = "JSON-RPC 2.0. v1.0 methods (A2A-Version: 1.0): SendMessage, SendStreamingMessage (SSE), GetTask, ListTasks, CancelTask, SubscribeToTask (SSE), Create/Get/List/DeleteTaskPushNotificationConfig, GetExtendedAgentCard. v0.3 methods (no header): message/send, message/stream, tasks/get, tasks/cancel, tasks/resubscribe, tasks/pushNotificationConfig/set|get|list|delete, agent/getAuthenticatedExtendedCard.";
    let rest_desc = "HTTP+JSON binding. v1.0: POST message:send, POST message:stream (SSE), GET tasks, GET tasks/{id}, POST tasks/{id}:cancel, GET|POST tasks/{id}:subscribe (SSE), POST|GET tasks/{id}/pushNotificationConfigs, GET|DELETE tasks/{id}/pushNotificationConfigs/{configId}, GET extendedAgentCard. v0.3 (no header): same paths with v0.3 ProtoJSON plus GET card.";
    let rest_op = |m: &str| {
        with_agent_param(op(
            &format!("A2A REST binding ({m})"),
            &format!("a2aRest{m}"),
            rest_desc,
        ))
    };
    let mut rest_item = serde_json::Map::new();
    for m in ["get", "post", "put", "patch", "delete"] {
        let mut o = rest_op(m);
        if let Some(params) = o.get_mut("parameters").and_then(|p| p.as_array_mut()) {
            params.push(json!({ "name": "rest", "in": "path", "required": true, "schema": { "type": "string" },
                "description": "Binding path, e.g. message:send or tasks/{id}:cancel" }));
        }
        if m == "post" {
            o = with_body(o);
        }
        rest_item.insert(m.to_string(), o);
    }
    let rest_named = |summary: &str, id: &str, body: bool| {
        let mut o = with_agent_param(op(summary, id, rest_desc));
        if body {
            o = with_body(o);
        }
        o
    };
    json!({
        "/.well-known/agent-card.json": { "get": op("A2A Agent Card (default agent)", "a2aAgentCard", "v1.0 card with supportedInterfaces plus the v0.3 url/preferredTransport fields; lists all demo agents in capabilities.extensions") },
        "/.well-known/agent.json": { "get": op("Legacy A2A v0.3 Agent Card", "a2aLegacyAgentCard", "v0.3 card of the default agent") },
        "/a2a": {
            "get": op("A2A agent directory", "a2aDirectory", "Lists the demo agents and their endpoints"),
            "post": with_body(op("A2A JSON-RPC (default echo agent)", "a2aJsonRpcDefault", rpc_desc)),
        },
        "/a2a/{agent}": {
            "get": with_agent_param(op("A2A Agent Card of an agent", "a2aAgentCardByAgent", "Same as /a2a/{agent}/.well-known/agent-card.json")),
            "post": with_body(with_agent_param(op("A2A JSON-RPC endpoint", "a2aJsonRpc", rpc_desc))),
        },
        "/a2a/{agent}/.well-known/agent-card.json": { "get": with_agent_param(op("A2A Agent Card of an agent", "a2aAgentCardWellKnown", "v1.0 card plus v0.3 fields")) },
        "/a2a/{agent}/.well-known/agent.json": { "get": with_agent_param(op("Legacy v0.3 Agent Card of an agent", "a2aLegacyAgentCardByAgent", "v0.3 card")) },
        "/a2a/{agent}/v1/{rest}": Value::Object(rest_item),
        "/a2a/{agent}/v1/message:send": { "post": rest_named("REST SendMessage", "a2aRestSendMessage", true) },
        "/a2a/{agent}/v1/message:stream": { "post": rest_named("REST SendStreamingMessage (SSE)", "a2aRestSendStreamingMessage", true) },
        "/a2a/{agent}/v1/tasks": { "get": rest_named("REST ListTasks (contextId, status, pageSize, pageToken, historyLength, statusTimestampAfter, includeArtifacts)", "a2aRestListTasks", false) },
        "/a2a/{agent}/v1/extendedAgentCard": { "get": rest_named("REST GetExtendedAgentCard (bearer token)", "a2aRestExtendedAgentCard", false) },
        "/a2a/webhook-sink/{id}": {
            "get": op("List push notifications received by a sink", "a2aSinkList", "Bounded: 50 notifications per sink, idle sinks expire after an hour"),
            "post": with_body(op("Record a push notification", "a2aSinkRecord", "Any JSON body; selected headers are stored")),
            "delete": op("Clear a sink", "a2aSinkClear", "Removes all stored notifications"),
        },
    })
}

#[cfg(test)]
mod tests;
