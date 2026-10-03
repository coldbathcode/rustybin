//! Mock LLM: a deterministic multi-provider model server for AI gateway demos.
//!
//! Provider front ends (each normalises its native request into
//! [`engine::ChatInput`] and renders the [`engine::Reply`] natively):
//! - [`openai`] serves `/ai/openai/v1/*` (and the `/ai/v1/*` aliases): chat
//!   completions, completions, embeddings, models, moderations, images,
//!   audio transcriptions; [`responses`]: the Responses API.
//! - [`azure`] serves `/ai/azure/openai/deployments/{deployment}/*`.
//! - [`anthropic`] serves `/ai/anthropic/v1/messages`, `count_tokens`, models.
//! - [`gemini`] serves `/ai/gemini/v1beta/models/{model}:{action}`.
//! - [`bedrock`] serves Converse, ConverseStream (AWS event stream), InvokeModel.
//! - [`ollama`] serves `/ai/ollama/api/*` (NDJSON streaming).
//! - [`cohere`] serves rerank and embed.
//!
//! Cross-cutting behaviour lives in the [`ai_layer`] middleware: native
//! credential checks ([`auth`]), latency and fault simulation ([`faults`]),
//! and the common response headers:
//! `X-Rustybin-Request-Id`, `X-Rustybin-Credential`, `X-Rustybin-Provider`,
//! `X-Rustybin-Instance`, `X-Rustybin-Model`, `X-Rustybin-Mode` and success
//! rate-limit headers. Every exchange is recorded ([`store`]) and served by
//! `GET /ai/requests/{id}`.

pub mod anthropic;
pub mod auth;
pub mod azure;
pub mod bedrock;
pub mod cohere;
pub mod embed;
pub mod engine;
pub mod eventstream;
pub mod faults;
pub mod gemini;
pub mod ollama;
pub mod openai;
pub mod responses;
pub mod schema;
pub mod store;
pub mod tokens;

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::catalog::Endpoint;
use crate::config::Config;
use crate::inspector::Inspector;
use crate::state::AppState;

use auth::AuthSettings;
use engine::{ChatInput, Mode, Reply};
use faults::{Fault, Pace, Provider};
use store::{AiRecord, PromptCache, RequestStore};

pub const MODE_HEADER: &str = "x-rustybin-mode";
pub const LATENCY_HEADER: &str = "x-rustybin-latency-ms";
pub const TTFT_HEADER: &str = "x-rustybin-ttft-ms";
pub const TPS_HEADER: &str = "x-rustybin-tokens-per-second";
pub const FAIL_HEADER: &str = "x-rustybin-fail";
pub const REQUIRE_AUTH_HEADER: &str = "x-rustybin-require-auth";
pub const REQUEST_ID_HEADER: &str = "x-rustybin-request-id";
pub const CREDENTIAL_HEADER: &str = "x-rustybin-credential";

/// Shared state of the AI module (created in [`router`]).
pub struct AiShared {
    pub config: Arc<Config>,
    pub inspector: Inspector,
    pub auth: AuthSettings,
    pub store: RequestStore,
    pub prompt_cache: PromptCache,
}

impl AiShared {
    pub fn new(state: &AppState, auth: AuthSettings) -> Self {
        let public = state.config.public_mode;
        Self {
            config: state.config.clone(),
            inspector: state.inspector.clone(),
            auth,
            store: RequestStore::new(if public { 200 } else { 1000 }, Duration::from_secs(3600)),
            prompt_cache: PromptCache::new(
                if public { 512 } else { 4096 },
                Duration::from_secs(300),
            ),
        }
    }

    pub fn public(&self) -> bool {
        self.config.public_mode
    }

    /// Cap of `n` (choices) per request.
    pub fn max_choices(&self) -> u32 {
        if self.public() {
            2
        } else {
            8
        }
    }

    /// Cap on the number of inputs of one embeddings / rerank request.
    pub fn max_inputs(&self) -> usize {
        if self.public() {
            256
        } else {
            2048
        }
    }

    /// Cap on embedding dimensions.
    pub fn max_dims(&self) -> usize {
        if self.public() {
            1536
        } else {
            4096
        }
    }
}

/// Per-request context inserted by [`ai_layer`].
#[derive(Clone)]
pub struct AiCtx {
    pub shared: Arc<AiShared>,
    pub provider: Provider,
    pub request_id: String,
    pub session: String,
    pub method: String,
    pub path: String,
    pub query_string: Option<String>,
    pub query: HashMap<String, String>,
    pub mode_header: Option<Mode>,
    pub content_filter: bool,
    pub pace: Pace,
    pub credential: Option<String>,
    pub headers: Vec<(String, String)>,
}

impl AiCtx {
    pub fn mode(&self, model: &str) -> Mode {
        Mode::resolve(self.mode_header, model)
    }

    pub fn gen_opts(&self, model: &str, choice: u32) -> engine::GenOpts {
        engine::GenOpts {
            mode: self.mode(model),
            choice,
            content_filter: self.content_filter,
            public: self.shared.public(),
        }
    }

    /// Generate a reply for a normalised input.
    pub fn generate(&self, input: &ChatInput, choice: u32) -> Reply {
        engine::generate(input, self.gen_opts(&input.model, choice))
    }

    /// Wait for the time to first token (non-streaming responses).
    pub async fn wait_ttft(&self) {
        if !self.pace.ttft.is_zero() {
            tokio::time::sleep(self.pace.ttft).await;
        }
    }

    /// Record the exchange for `GET /ai/requests/{id}`.
    pub fn record(&self, r: RecordArgs<'_>) {
        let body = match serde_json::from_slice::<Value>(r.body) {
            Ok(v) if r.body.len() <= 64 * 1024 => v,
            _ => {
                let text = String::from_utf8_lossy(&r.body[..r.body.len().min(16 * 1024)]);
                Value::String(text.into_owned())
            }
        };
        let preview: String = r.reply.chars().take(500).collect();
        let prompt: String = r.prompt.chars().take(32 * 1024).collect();
        self.shared.store.insert(AiRecord {
            request_id: self.request_id.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            session: self.session.clone(),
            provider: self.provider.as_str(),
            method: self.method.clone(),
            endpoint: self.path.clone(),
            query: self.query_string.clone(),
            model: r.model.to_string(),
            mode: r.mode.to_string(),
            stream: r.stream,
            credential: self.credential.clone(),
            headers: self.headers.clone(),
            body,
            normalized_prompt: prompt,
            prompt_tokens: r.prompt_tokens,
            completion_tokens: r.completion_tokens,
            finish_reason: r.finish.to_string(),
            reply_preview: preview,
        });
    }

    /// Record a chat exchange from its input and reply.
    pub fn record_chat(
        &self,
        body: &[u8],
        input: &ChatInput,
        reply: &Reply,
        stream: bool,
        finish: &str,
    ) {
        let preview = if reply.tool_calls.is_empty() {
            reply.text.clone()
        } else {
            reply
                .tool_calls
                .iter()
                .map(|c| format!("[tool_call {}] {}", c.name, c.arguments))
                .collect::<Vec<_>>()
                .join("\n")
        };
        self.record(RecordArgs {
            body,
            model: &input.model,
            mode: reply.mode.as_str(),
            stream,
            prompt: &engine::render(input),
            prompt_tokens: reply.prompt_tokens,
            completion_tokens: reply.completion_tokens,
            finish,
            reply: &preview,
        });
    }

    /// The provider's native error.
    pub fn error(&self, kind: faults::ErrorKind, message: impl Into<String>) -> Response {
        let m: String = message.into();
        faults::error_response(self.provider, kind, Some(&m))
    }
}

/// Arguments of [`AiCtx::record`].
pub struct RecordArgs<'a> {
    pub body: &'a [u8],
    pub model: &'a str,
    pub mode: &'a str,
    pub stream: bool,
    pub prompt: &'a str,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub finish: &'a str,
    pub reply: &'a str,
}

/// Response extension: lets [`ai_layer`] add model and rate-limit headers.
#[derive(Clone, Debug)]
pub struct Served {
    pub model: String,
    pub mode: Option<Mode>,
    pub tokens: u32,
}

/// JSON response carrying [`Served`] info.
pub fn json_response(value: Value, served: Served) -> Response {
    let mut resp = Json(value).into_response();
    resp.extensions_mut().insert(served);
    resp
}

/// Streaming response (`text/event-stream`, NDJSON, event stream, ...).
pub fn stream_response<S>(content_type: &'static str, stream: S, served: Served) -> Response
where
    S: Stream<Item = Result<Bytes, std::convert::Infallible>> + Send + 'static,
{
    let mut resp = Response::new(Body::from_stream(stream));
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    resp.extensions_mut().insert(served);
    resp
}

/// `event: name\ndata: json\n\n` (or only `data:` when `event` is empty).
pub fn sse_event(event: &str, data: &Value) -> Bytes {
    if event.is_empty() {
        Bytes::from(format!("data: {data}\n\n"))
    } else {
        Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
    }
}

/// Unix seconds.
pub fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Random id suffix (hex, `len` chars).
pub fn rand_id(len: usize) -> String {
    let mut s = uuid::Uuid::new_v4().simple().to_string();
    s.push_str(&uuid::Uuid::new_v4().simple().to_string());
    s.truncate(len);
    s
}

fn parse_query(q: Option<&str>) -> HashMap<String, String> {
    q.map(|q| {
        form_urlencoded::parse(q.as_bytes())
            .take(64)
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    })
    .unwrap_or_default()
}

fn header_str<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok()).map(str::trim)
}

const SECRET_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
    "proxy-authorization",
    "cookie",
    "x-amz-security-token",
];

fn redacted_headers(h: &HeaderMap) -> Vec<(String, String)> {
    h.iter()
        .take(100)
        .map(|(k, v)| {
            let name = k.as_str().to_string();
            let raw = String::from_utf8_lossy(v.as_bytes()).into_owned();
            let value = if SECRET_HEADERS.contains(&name.as_str()) {
                match raw.split_once(' ') {
                    Some((scheme, rest)) if name.ends_with("authorization") => {
                        if scheme.starts_with("AWS4") {
                            format!("{scheme} {}", rest.chars().take(60).collect::<String>())
                        } else {
                            format!("{scheme} {}", auth::redact(rest))
                        }
                    }
                    _ => auth::redact(&raw),
                }
            } else {
                raw.chars().take(2048).collect()
            };
            (name, value)
        })
        .collect()
}

fn insert_header(h: &mut HeaderMap, name: &'static str, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        h.insert(name, v);
    }
}

/// Middleware for every AI route: auth, latency, faults, common headers.
pub async fn ai_layer(
    State(shared): State<Arc<AiShared>>,
    mut req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    let provider = Provider::from_path(&path);
    let headers = req.headers().clone();
    let query_string = req.uri().query().map(str::to_string);
    let query = parse_query(query_string.as_deref());
    let request_id = header_str(&headers, "x-request-id")
        .filter(|s| !s.is_empty() && s.len() <= 200)
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let ip = crate::session::client_ip(&headers, req.extensions(), &shared.config);
    let session = crate::session::session_key(&headers, ip);
    let max_delay = shared.config.max_delay_ms();
    let ms = |name: &str| {
        header_str(&headers, name)
            .and_then(|v| v.parse::<u64>().ok())
            .map(|v| v.min(max_delay))
            .unwrap_or(0)
    };
    let latency = ms(LATENCY_HEADER);
    let ttft = ms(TTFT_HEADER);
    let tps = header_str(&headers, TPS_HEADER)
        .and_then(|v| v.parse::<u32>().ok())
        .map(|v| v.min(100_000))
        .unwrap_or(Pace::DEFAULT_TPS);
    let pace = Pace {
        ttft: Duration::from_millis(ttft),
        tps,
        max_total: Duration::from_secs(if shared.public() { 20 } else { 60 }),
    };

    let common = |resp: &mut Response, cred: &Option<String>| {
        let h = resp.headers_mut();
        insert_header(h, REQUEST_ID_HEADER, &request_id);
        insert_header(h, CREDENTIAL_HEADER, cred.as_deref().unwrap_or("none"));
        insert_header(h, "x-rustybin-provider", provider.as_str());
        insert_header(h, "x-rustybin-instance", &shared.config.instance_id);
        match provider {
            Provider::Anthropic => insert_header(
                h,
                "request-id",
                &format!("req_{}", request_id.replace('-', "")),
            ),
            Provider::Bedrock => insert_header(h, "x-amzn-requestid", &request_id),
            _ => {}
        }
    };

    // Credentials.
    let required =
        shared.auth.require || header_str(&headers, REQUIRE_AUTH_HEADER).is_some_and(auth::is_true);
    let seen = auth::find(provider, &headers, &query).map(|c| c.redacted());
    if let Err(mut resp) = auth::check(
        provider,
        &headers,
        &query,
        required,
        shared.auth.api_key.as_deref(),
    ) {
        common(&mut resp, &seen);
        return resp;
    }

    // Latency before headers.
    if latency > 0 {
        tokio::time::sleep(Duration::from_millis(latency)).await;
    }

    // Faults: header first, then ?fail= / ?fail_rate=.
    let fault = header_str(&headers, FAIL_HEADER)
        .and_then(faults::parse_fail)
        .or_else(|| {
            let rate = query.get("fail_rate").and_then(|r| faults::parse_rate(r));
            let kind = query.get("fail").and_then(|k| faults::parse_fail(k));
            match (kind, rate) {
                (Some((k, p)), None) => Some((k, p)),
                (Some((k, _)), Some(r)) => Some((k, r)),
                (None, Some(r)) => Some((Fault::Error(faults::ErrorKind::Unavailable), r)),
                (None, None) => None,
            }
        });
    let mut content_filter = false;
    if let Some((f, pct)) = fault {
        if faults::roll(pct) {
            match f {
                Fault::Error(kind) => {
                    let mut resp = faults::error_response(provider, kind, None);
                    resp.headers_mut()
                        .insert("x-rustybin-fault", HeaderValue::from_static("ai"));
                    common(&mut resp, &seen);
                    return resp;
                }
                Fault::ContentFilter => content_filter = true,
            }
        }
    }

    let mode_header = header_str(&headers, MODE_HEADER).and_then(Mode::parse);
    let ctx = AiCtx {
        shared: shared.clone(),
        provider,
        request_id: request_id.clone(),
        session,
        method: req.method().to_string(),
        path: path.clone(),
        query_string,
        query,
        mode_header,
        content_filter,
        pace,
        credential: seen.clone(),
        headers: redacted_headers(&headers),
    };
    req.extensions_mut().insert(ctx);

    let mut resp = next.run(req).await;
    common(&mut resp, &seen);
    if let Some(served) = resp.extensions().get::<Served>().cloned() {
        let h = resp.headers_mut();
        insert_header(h, "x-rustybin-model", &served.model);
        if let Some(m) = served.mode {
            insert_header(h, "x-rustybin-mode", m.as_str());
        }
        if resp.status().is_success() {
            faults::success_rate_limit_headers(provider, resp.headers_mut(), served.tokens);
        }
    }
    resp
}

// ── Request inspection ──────────────────────────────────────────────

#[derive(Deserialize)]
struct InspectQuery {
    session: Option<String>,
    limit: Option<usize>,
}

/// Session the caller may read: in public mode the caller's own session
/// key (or the `?session=` value), otherwise anything.
fn allowed_session(
    shared: &AiShared,
    headers: &HeaderMap,
    ip: Option<std::net::IpAddr>,
    q: &InspectQuery,
) -> Option<String> {
    // An IP-derived key ("ip:...") cannot be claimed through ?session= in
    // public mode (it would be guessable).
    if let Some(s) = q
        .session
        .as_ref()
        .filter(|s| crate::session::is_valid_session(s))
        .filter(|s| !(shared.public() && s.starts_with("ip:")))
    {
        return Some(s.clone());
    }
    if shared.public() {
        return Some(crate::session::session_key(headers, ip));
    }
    None
}

fn record_json(shared: &AiShared, r: &AiRecord) -> Value {
    let captured = shared.inspector.find_by_request_id(&r.request_id).map(|e| {
        json!({
            "inspector_id": e.id,
            "status": e.status,
            "latency_ms": e.latency_ms,
            "client_ip": e.client_ip,
        })
    });
    let mut v = serde_json::to_value(r).unwrap_or(Value::Null);
    if let Some(obj) = v.as_object_mut() {
        let headers: serde_json::Map<String, Value> = r
            .headers
            .iter()
            .map(|(k, val)| (k.clone(), Value::String(val.clone())))
            .collect();
        obj.insert("headers".into(), Value::Object(headers));
        obj.insert(
            "usage".into(),
            json!({
                "prompt_tokens": r.prompt_tokens,
                "completion_tokens": r.completion_tokens,
                "total_tokens": r.prompt_tokens + r.completion_tokens,
            }),
        );
        obj.insert("captured".into(), captured.unwrap_or(Value::Null));
    }
    v
}

async fn list_requests(
    Extension(shared): Extension<Arc<AiShared>>,
    crate::session::ClientIp(ip): crate::session::ClientIp,
    headers: HeaderMap,
    Query(q): Query<InspectQuery>,
) -> Response {
    let session = allowed_session(&shared, &headers, ip, &q);
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let items: Vec<Value> = shared
        .store
        .list(session.as_deref(), limit)
        .iter()
        .map(|r| {
            json!({
                "request_id": r.request_id,
                "timestamp": r.timestamp,
                "provider": r.provider,
                "endpoint": r.endpoint,
                "model": r.model,
                "mode": r.mode,
                "stream": r.stream,
                "prompt_tokens": r.prompt_tokens,
                "completion_tokens": r.completion_tokens,
                "finish_reason": r.finish_reason,
            })
        })
        .collect();
    Json(json!({ "count": items.len(), "requests": items })).into_response()
}

async fn get_request(
    Extension(shared): Extension<Arc<AiShared>>,
    crate::session::ClientIp(ip): crate::session::ClientIp,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<InspectQuery>,
) -> Response {
    let session = allowed_session(&shared, &headers, ip, &q);
    match shared.store.get(&id) {
        Some(r) if session.as_deref().is_none_or(|s| r.session == s) => {
            Json(record_json(&shared, &r)).into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "no such AI request",
                "request_id": id,
                "hint": "use the X-Rustybin-Request-Id response header of an /ai/* call; records expire after an hour",
            })),
        )
            .into_response(),
    }
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

pub fn router(state: &AppState) -> Router<AppState> {
    router_with(state, AuthSettings::from_env())
}

/// Router with explicit auth settings (tests).
pub fn router_with(state: &AppState, auth: AuthSettings) -> Router<AppState> {
    let shared = Arc::new(AiShared::new(state, auth));
    let api = Router::new()
        .merge(openai::routes())
        .merge(responses::routes())
        .merge(azure::routes())
        .merge(anthropic::routes())
        .merge(gemini::routes())
        .merge(bedrock::routes())
        .merge(ollama::routes())
        .merge(cohere::routes())
        .layer(middleware::from_fn_with_state(shared.clone(), ai_layer))
        .layer(Extension(shared.clone()));
    let inspect = Router::new()
        .route("/ai/requests", get(list_requests))
        .route("/ai/requests/{id}", get(get_request))
        .layer(Extension(shared));
    api.merge(inspect)
}

pub fn catalog() -> Vec<Endpoint> {
    use crate::catalog::{category, Example};
    let mut v = Vec::new();
    v.extend(openai::catalog());
    v.extend(responses::catalog());
    v.extend(azure::catalog());
    v.extend(anthropic::catalog());
    v.extend(gemini::catalog());
    v.extend(bedrock::catalog());
    v.extend(ollama::catalog());
    v.extend(cohere::catalog());
    v.push(
        Endpoint::new(
            "/ai/requests",
            &["GET"],
            category::AI_MOCK,
            "Recent mock LLM exchanges (newest first)",
        )
        .description("Public mode only lists the caller's own session (X-Rustybin-Session or client IP). ?limit=, ?session=.")
        .example(Example::get("Recent AI requests", "/ai/requests?limit=10")),
    );
    v.push(
        Endpoint::new(
            "/ai/requests/{id}",
            &["GET"],
            category::AI_MOCK,
            "What the upstream received for one AI request",
        )
        .description("Look up by the X-Rustybin-Request-Id response header: headers (credentials redacted), body, normalised prompt, provider, model, mode and computed tokens.")
        .example(Example::get("Inspect an AI request", "/ai/requests/unknown-id").expect_status(404)),
    );
    v
}

pub fn openapi_paths() -> Value {
    let mut out = serde_json::Map::new();
    for frag in [
        openai::openapi_paths(),
        responses::openapi_paths(),
        azure::openapi_paths(),
        anthropic::openapi_paths(),
        gemini::openapi_paths(),
        bedrock::openapi_paths(),
        ollama::openapi_paths(),
        cohere::openapi_paths(),
    ] {
        if let Value::Object(m) = frag {
            out.extend(m);
        }
    }
    out.insert(
        "/ai/requests".into(),
        json!({"get": {
            "tags": ["AI Gateway"],
            "summary": "Recent mock LLM exchanges",
            "operationId": "aiListRequests",
            "parameters": [
                {"name": "limit", "in": "query", "schema": {"type": "integer", "maximum": 200}},
                {"name": "session", "in": "query", "schema": {"type": "string"}}
            ],
            "responses": {"200": {"description": "Summaries, newest first"}}
        }}),
    );
    out.insert(
        "/ai/requests/{id}".into(),
        json!({"get": {
            "tags": ["AI Gateway"],
            "summary": "What the upstream received for one AI request",
            "operationId": "aiGetRequest",
            "parameters": [
                {"name": "id", "in": "path", "required": true, "schema": {"type": "string"}, "description": "X-Rustybin-Request-Id of the AI response"},
                {"name": "session", "in": "query", "schema": {"type": "string"}}
            ],
            "responses": {
                "200": {"description": "Recorded exchange", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/AiRequestRecord"}}}},
                "404": {"description": "Unknown or expired id (or another session's request in public mode)"}
            }
        }}),
    );
    Value::Object(out)
}

/// Shared header parameters of the mock LLM operations.
pub fn common_parameters() -> Value {
    json!([
        {"name": "X-Rustybin-Mode", "in": "header", "schema": {"type": "string", "enum": ["canned", "echo", "scripted", "random"]}, "description": "Reply mode (also selectable by a model name segment, e.g. rustybin-echo)"},
        {"name": "X-Rustybin-Latency-Ms", "in": "header", "schema": {"type": "integer"}, "description": "Delay before response headers"},
        {"name": "X-Rustybin-TTFT-Ms", "in": "header", "schema": {"type": "integer"}, "description": "Time to first token"},
        {"name": "X-Rustybin-Tokens-Per-Second", "in": "header", "schema": {"type": "integer"}, "description": "Streaming pace (default 100, 0 = unpaced)"},
        {"name": "X-Rustybin-Fail", "in": "header", "schema": {"type": "string"}, "description": "kind[:percent]: 429, 500, 503, 529, 504, context_length, content_filter, prompt_filter, 401, ... in the provider's native error shape"},
        {"name": "X-Rustybin-Require-Auth", "in": "header", "schema": {"type": "boolean"}, "description": "Enforce the provider's native credential"}
    ])
}

pub fn openapi_components() -> Value {
    json!({"schemas": {
        "ChatCompletionRequest": openai::chat_request_schema(),
        "ChatCompletionResponse": openai::chat_response_schema(),
        "AnthropicMessagesRequest": anthropic::request_schema(),
        "AnthropicMessage": anthropic::response_schema(),
        "AiRequestRecord": {
            "type": "object",
            "properties": {
                "request_id": {"type": "string"},
                "provider": {"type": "string"},
                "endpoint": {"type": "string"},
                "model": {"type": "string"},
                "mode": {"type": "string"},
                "stream": {"type": "boolean"},
                "credential": {"type": "string", "nullable": true},
                "headers": {"type": "object", "additionalProperties": {"type": "string"}},
                "body": {},
                "normalized_prompt": {"type": "string"},
                "usage": {"type": "object", "properties": {
                    "prompt_tokens": {"type": "integer"},
                    "completion_tokens": {"type": "integer"},
                    "total_tokens": {"type": "integer"}
                }},
                "finish_reason": {"type": "string"},
                "reply_preview": {"type": "string"},
                "captured": {"type": "object", "nullable": true}
            }
        }
    }})
}

#[cfg(test)]
pub(crate) mod test_util {
    use axum::body::Body;
    use axum::http::Request;
    use axum::Router;
    use tower::ServiceExt;

    use super::auth::AuthSettings;

    pub fn app() -> Router {
        app_with(AuthSettings::default())
    }

    pub fn app_with(auth: AuthSettings) -> Router {
        app_with_state(crate::test_support::test_state(), auth)
    }

    /// Public-mode instance.
    pub fn app_public() -> Router {
        let mut config = crate::config::Config::for_tests();
        config.public_mode = true;
        app_with_state(
            crate::test_support::test_state_with(config),
            AuthSettings::default(),
        )
    }

    fn app_with_state(state: crate::state::AppState, auth: AuthSettings) -> Router {
        let r = super::router_with(&state, auth);
        r.with_state(state)
            .layer(axum::extract::connect_info::MockConnectInfo(
                std::net::SocketAddr::from(([127, 0, 0, 1], 40000)),
            ))
    }

    /// POST JSON with extra headers; returns (status, headers, body bytes).
    pub async fn send(
        app: &Router,
        uri: &str,
        body: &serde_json::Value,
        headers: &[(&str, &str)],
    ) -> (u16, axum::http::HeaderMap, bytes::Bytes) {
        let mut b = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("x-rustybin-tokens-per-second", "0");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let resp = app
            .clone()
            .oneshot(b.body(Body::from(body.to_string())).expect("request"))
            .await
            .expect("response");
        let status = resp.status().as_u16();
        let h = resp.headers().clone();
        (status, h, crate::test_support::body_bytes(resp).await)
    }

    pub async fn fetch(app: &Router, uri: &str) -> (u16, axum::http::HeaderMap, bytes::Bytes) {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = resp.status().as_u16();
        let h = resp.headers().clone();
        (status, h, crate::test_support::body_bytes(resp).await)
    }

    pub fn json(b: &bytes::Bytes) -> serde_json::Value {
        serde_json::from_slice(b).expect("json body")
    }

    /// `data:` payloads of an SSE body (excluding `[DONE]`).
    pub fn sse_data(b: &bytes::Bytes) -> Vec<serde_json::Value> {
        String::from_utf8_lossy(b)
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter(|d| *d != "[DONE]")
            .map(|d| serde_json::from_str(d).expect("sse json"))
            .collect()
    }

    /// `event:` names of an SSE body.
    pub fn sse_events(b: &bytes::Bytes) -> Vec<String> {
        String::from_utf8_lossy(b)
            .lines()
            .filter_map(|l| l.strip_prefix("event: "))
            .map(str::to_string)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::*;
    use serde_json::json;

    #[tokio::test]
    async fn common_headers_and_inspection() {
        let app = app();
        let (s, h, _) = send(
            &app,
            "/ai/openai/v1/chat/completions",
            &json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hello"}]}),
            &[
                ("authorization", "Bearer sk-test-wxyz"),
                ("x-request-id", "req-abc"),
            ],
        )
        .await;
        assert_eq!(s, 200);
        assert_eq!(h["x-rustybin-request-id"], "req-abc");
        assert_eq!(h["x-rustybin-credential"], "bearer ****wxyz");
        assert_eq!(h["x-rustybin-model"], "gpt-4o");
        assert_eq!(h["x-rustybin-provider"], "openai");
        assert!(h.contains_key("x-ratelimit-remaining-tokens"));

        let (s, _, b) = fetch(&app, "/ai/requests/req-abc").await;
        assert_eq!(s, 200);
        let v = json(&b);
        assert_eq!(v["provider"], "openai");
        assert_eq!(v["model"], "gpt-4o");
        assert_eq!(v["normalized_prompt"], "user: hello");
        assert_eq!(v["headers"]["authorization"], "Bearer ****wxyz");
        assert!(v["usage"]["prompt_tokens"].as_u64().unwrap_or(0) > 0);

        let (s, _, b) = fetch(&app, "/ai/requests").await;
        assert_eq!(s, 200);
        assert_eq!(json(&b)["count"], 1);
        let (s, _, _) = fetch(&app, "/ai/requests/nope").await;
        assert_eq!(s, 404);
    }

    #[tokio::test]
    async fn faults_per_provider() {
        let app = app();
        let chat = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        let (s, h, b) = send(
            &app,
            "/ai/openai/v1/chat/completions",
            &chat,
            &[("x-rustybin-fail", "429")],
        )
        .await;
        assert_eq!(s, 429);
        assert_eq!(h["retry-after"], "2");
        assert_eq!(json(&b)["error"]["code"], "rate_limit_exceeded");
        let msg = json!({"model": "m", "max_tokens": 10, "messages": [{"role": "user", "content": "hi"}]});
        let (s, h, b) = send(
            &app,
            "/ai/anthropic/v1/messages",
            &msg,
            &[("x-rustybin-fail", "overloaded")],
        )
        .await;
        assert_eq!(s, 529);
        assert!(h.contains_key("x-rustybin-request-id"));
        assert_eq!(json(&b)["error"]["type"], "overloaded_error");
        let (s, _, b) = send(
            &app,
            "/ai/openai/v1/chat/completions?fail=context_length&fail_rate=1",
            &chat,
            &[],
        )
        .await;
        assert_eq!(s, 400);
        assert_eq!(json(&b)["error"]["code"], "context_length_exceeded");
        let (s, _, _) = send(
            &app,
            "/ai/openai/v1/chat/completions?fail_rate=0",
            &chat,
            &[],
        )
        .await;
        assert_eq!(s, 200);
        let (s, _, b) = send(
            &app,
            "/ai/openai/v1/chat/completions",
            &chat,
            &[("x-rustybin-fail", "content_filter")],
        )
        .await;
        assert_eq!(s, 200);
        assert_eq!(json(&b)["choices"][0]["finish_reason"], "content_filter");
    }

    #[tokio::test]
    async fn auth_required_by_header_and_settings() {
        let app = app();
        let chat = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        let (s, h, b) = send(
            &app,
            "/ai/openai/v1/chat/completions",
            &chat,
            &[("x-rustybin-require-auth", "true")],
        )
        .await;
        assert_eq!(s, 401);
        assert_eq!(h["x-rustybin-credential"], "none");
        assert!(json(&b)["error"]["message"].as_str().is_some());

        let app = app_with(super::AuthSettings {
            require: true,
            api_key: Some("secret-key-1".into()),
        });
        let (s, _, b) = send(
            &app,
            "/ai/openai/v1/chat/completions",
            &chat,
            &[("authorization", "Bearer wrong-key")],
        )
        .await;
        assert_eq!(s, 401);
        assert_eq!(json(&b)["error"]["code"], "invalid_api_key");
        let (s, _, _) = send(
            &app,
            "/ai/openai/v1/chat/completions",
            &chat,
            &[("authorization", "Bearer secret-key-1")],
        )
        .await;
        assert_eq!(s, 200);
    }

    #[tokio::test]
    async fn public_mode_scopes_inspection_and_caps() {
        let app = app_public();
        let chat = json!({"model": "m", "n": 8, "messages": [{"role": "user", "content": "hi"}]});
        let (s, h, b) = send(
            &app,
            "/ai/openai/v1/chat/completions",
            &chat,
            &[("x-rustybin-session", "alice"), ("x-request-id", "pub-1")],
        )
        .await;
        assert_eq!(s, 200);
        assert_eq!(h["x-rustybin-request-id"], "pub-1");
        // n is capped at 2 in public mode.
        assert_eq!(json(&b)["choices"].as_array().map(Vec::len), Some(2));
        // Another caller (IP-derived session) cannot read it.
        let (s, _, _) = fetch(&app, "/ai/requests/pub-1").await;
        assert_eq!(s, 404);
        let (s, _, _) = fetch(&app, "/ai/requests/pub-1?session=alice").await;
        assert_eq!(s, 200);
        let (s, _, b) = fetch(&app, "/ai/requests?session=ip:127.0.0.1").await;
        assert_eq!(s, 200);
        assert_eq!(json(&b)["count"], 0);
        // Lorem is capped at 500 words.
        let (_, _, b) = send(
            &app,
            "/ai/openai/v1/chat/completions",
            &json!({"model": "rustybin-scripted", "messages": [{"role": "user", "content": "lorem 100000"}]}),
            &[],
        )
        .await;
        let text = json(&b)["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();
        assert_eq!(text.split_whitespace().count(), 500);
    }

    #[tokio::test]
    async fn latency_header_delays() {
        let app = app();
        let chat = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        let start = std::time::Instant::now();
        let (s, _, _) = send(
            &app,
            "/ai/openai/v1/chat/completions",
            &chat,
            &[("x-rustybin-latency-ms", "120")],
        )
        .await;
        assert_eq!(s, 200);
        assert!(start.elapsed() >= std::time::Duration::from_millis(120));
    }
}
