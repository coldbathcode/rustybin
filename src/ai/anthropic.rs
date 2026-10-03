//! Anthropic Messages API: `/ai/anthropic/v1/messages`,
//! `/ai/anthropic/v1/messages/count_tokens`, `/ai/anthropic/v1/models`.
//!
//! Supports `system` as a string or an array of text blocks (with
//! `cache_control`), content blocks (text, image, document, tool_use,
//! tool_result), tools with `tool_choice` (auto, any, tool, none),
//! `output_format` json_schema, `stop_sequences`, and the native SSE event
//! sequence (message_start, content_block_start, ping, content_block_delta
//! with text_delta / input_json_delta, content_block_stop, message_delta,
//! message_stop). `max_tokens` is required, like the real API.
//!
//! Prompt caching is simulated: the prefix up to the last `cache_control`
//! block is hashed; the first request reports it as
//! `cache_creation_input_tokens`, repeats within 5 minutes (same session and
//! model) as `cache_read_input_tokens`.

use axum::body::Bytes;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::{json, Value};
use std::convert::Infallible;

use super::engine::{
    self, ChatInput, Finish, Format, Msg, Part, Reply, Role, Tool, ToolCall, ToolChoice,
};
use super::faults::ErrorKind;
use super::{json_response, rand_id, sse_event, stream_response, AiCtx, Served};
use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

pub const DEFAULT_MODEL: &str = "rustybin-claude";

/// A parsed Messages request.
pub struct MessagesReq {
    pub input: ChatInput,
    pub stream: bool,
    /// Text of the cacheable prefix (up to the last cache_control), if any.
    pub cache_prefix: Option<String>,
}

fn block_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn parse_block(b: &Value) -> Option<Part> {
    let s = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match b.get("type").and_then(Value::as_str).unwrap_or("text") {
        "text" => Some(Part::Text(s("text"))),
        "image" => {
            let src = b.get("source");
            let d = src
                .and_then(|x| x.get("url").and_then(Value::as_str))
                .map(String::from)
                .or_else(|| {
                    src.and_then(|x| x.get("media_type"))
                        .and_then(Value::as_str)
                        .map(|m| format!("{m} (base64)"))
                })
                .unwrap_or_else(|| "image".into());
            Some(Part::Image(d))
        }
        "document" => Some(Part::File(
            b.get("title")
                .and_then(Value::as_str)
                .or_else(|| {
                    b.get("source")
                        .and_then(|x| x.get("media_type"))
                        .and_then(Value::as_str)
                })
                .unwrap_or("document")
                .to_string(),
        )),
        "tool_use" | "server_tool_use" => Some(Part::ToolCall(ToolCall {
            id: s("id"),
            name: s("name"),
            arguments: b.get("input").cloned().unwrap_or(json!({})),
        })),
        "tool_result" => Some(Part::ToolResult {
            id: s("tool_use_id"),
            name: None,
            content: block_text(b.get("content").unwrap_or(&Value::Null)),
            is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
        }),
        _ => None,
    }
}

/// Parse a Messages body (also the Bedrock InvokeModel Anthropic body).
pub fn parse(body: &Value, require_max_tokens: bool) -> Result<MessagesReq, String> {
    if !body.is_object() {
        return Err("The request body must be a JSON object.".into());
    }
    let max_tokens = body.get("max_tokens").and_then(Value::as_u64);
    if require_max_tokens && max_tokens.is_none() {
        return Err("max_tokens: Field required".into());
    }
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or("messages: Field required")?;
    if messages.is_empty() {
        return Err("messages: at least one message is required".into());
    }
    let mut input = ChatInput {
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_MODEL)
            .to_string(),
        max_tokens: max_tokens.map(|n| n.min(u64::from(u32::MAX)) as u32),
        stop: body
            .get("stop_sequences")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .take(8)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        ..Default::default()
    };
    // Track the cacheable prefix in request order: tools, system, messages.
    let mut prefix = String::new();
    let mut cache_prefix = None;
    let mut mark = |prefix: &str, b: &Value| {
        if b.get("cache_control").is_some() {
            cache_prefix = Some(prefix.to_string());
        }
    };
    for t in body
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(name), Some(schema)) =
            (t.get("name").and_then(Value::as_str), t.get("input_schema"))
        {
            let tool = Tool {
                name: name.to_string(),
                description: t
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                parameters: schema.clone(),
            };
            prefix.push_str(&format!(
                "{} {} {}\n",
                tool.name, tool.description, tool.parameters
            ));
            input.tools.push(tool);
        }
        mark(&prefix, t);
    }
    match body.get("system") {
        Some(Value::String(s)) => {
            prefix.push_str(s);
            input.system.push(s.clone());
        }
        Some(Value::Array(blocks)) => {
            for b in blocks {
                let t = b.get("text").and_then(Value::as_str).unwrap_or("");
                prefix.push_str(t);
                prefix.push('\n');
                input.system.push(t.to_string());
                mark(&prefix, b);
            }
        }
        _ => {}
    }
    for (i, m) in messages.iter().enumerate() {
        let role = match m.get("role").and_then(Value::as_str) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            other => {
                return Err(format!(
                    "messages.{i}.role: Input should be 'user' or 'assistant' (got {})",
                    other.unwrap_or("nothing")
                ))
            }
        };
        let mut parts = Vec::new();
        match m.get("content") {
            Some(Value::String(s)) => {
                prefix.push_str(s);
                parts.push(Part::Text(s.clone()));
            }
            Some(Value::Array(blocks)) => {
                for b in blocks {
                    if let Some(p) = parse_block(b) {
                        prefix.push_str(&engine::render(&ChatInput {
                            messages: vec![Msg {
                                role,
                                parts: vec![p.clone()],
                            }],
                            ..Default::default()
                        }));
                        prefix.push('\n');
                        parts.push(p);
                    }
                    mark(&prefix, b);
                }
            }
            _ => return Err(format!("messages.{i}.content: Field required")),
        }
        input.messages.push(Msg { role, parts });
    }
    input.tool_choice = match body.get("tool_choice") {
        Some(tc) => match tc.get("type").and_then(Value::as_str) {
            Some("any") => ToolChoice::Required,
            Some("none") => ToolChoice::None,
            Some("tool") => ToolChoice::Named(
                tc.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
            _ => ToolChoice::Auto,
        },
        None => ToolChoice::Auto,
    };
    if let Some(of) = body.get("output_format") {
        if of.get("type").and_then(Value::as_str) == Some("json_schema") {
            input.format = Format::JsonSchema(
                of.get("schema")
                    .cloned()
                    .unwrap_or(json!({"type": "object"})),
            );
        }
    }
    Ok(MessagesReq {
        stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
        cache_prefix,
        input,
    })
}

/// Usage split into uncached input, cache creation and cache reads.
#[derive(Clone, Copy, Debug, Default)]
pub struct CacheUsage {
    pub input: u32,
    pub creation: u32,
    pub read: u32,
}

pub fn cache_usage(ctx: &AiCtx, req: &MessagesReq, prompt_tokens: u32) -> CacheUsage {
    let Some(prefix) = &req.cache_prefix else {
        return CacheUsage {
            input: prompt_tokens,
            ..Default::default()
        };
    };
    let cached = super::tokens::count(prefix).min(prompt_tokens.saturating_sub(1));
    let key = engine::stable_hash(&format!(
        "{}\u{0}{}\u{0}{prefix}",
        ctx.session, req.input.model
    ));
    if ctx.shared.prompt_cache.hit(key) {
        CacheUsage {
            input: prompt_tokens - cached,
            creation: 0,
            read: cached,
        }
    } else {
        CacheUsage {
            input: prompt_tokens - cached,
            creation: cached,
            read: 0,
        }
    }
}

pub fn stop_reason(f: &Finish) -> (&'static str, Value) {
    match f {
        Finish::Stop => ("end_turn", Value::Null),
        Finish::StopSequence(s) => ("stop_sequence", json!(s)),
        Finish::Length => ("max_tokens", Value::Null),
        Finish::ToolCalls => ("tool_use", Value::Null),
        Finish::ContentFilter => ("refusal", Value::Null),
    }
}

fn tool_use_json(c: &ToolCall) -> Value {
    json!({"type": "tool_use", "id": format!("toolu_{}", c.id), "name": c.name, "input": c.arguments})
}

pub fn usage_json(cu: CacheUsage, output: u32) -> Value {
    json!({
        "input_tokens": cu.input,
        "cache_creation_input_tokens": cu.creation,
        "cache_read_input_tokens": cu.read,
        "cache_creation": {"ephemeral_5m_input_tokens": cu.creation, "ephemeral_1h_input_tokens": 0},
        "output_tokens": output,
        "service_tier": "standard",
    })
}

/// Non-streaming message.
pub fn message_json(id: &str, model: &str, reply: &Reply, cu: CacheUsage) -> Value {
    let mut content = Vec::new();
    if (!reply.text.is_empty() || reply.tool_calls.is_empty())
        && !(reply.text.is_empty() && reply.finish == Finish::ContentFilter)
    {
        content.push(json!({"type": "text", "text": reply.text}));
    }
    content.extend(reply.tool_calls.iter().map(tool_use_json));
    let (reason, seq) = stop_reason(&reply.finish);
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": reason,
        "stop_sequence": seq,
        "usage": usage_json(cu, reply.completion_tokens),
    })
}

/// The streaming event sequence as (event name, data) pairs.
pub fn stream_events(
    id: &str,
    model: &str,
    reply: &Reply,
    cu: CacheUsage,
) -> Vec<(&'static str, Value)> {
    let mut ev: Vec<(&'static str, Value)> = Vec::new();
    let mut start_usage = usage_json(cu, 1);
    start_usage["output_tokens"] = json!(1);
    ev.push((
        "message_start",
        json!({"type": "message_start", "message": {
            "id": id, "type": "message", "role": "assistant", "model": model, "content": [],
            "stop_reason": null, "stop_sequence": null, "usage": start_usage
        }}),
    ));
    let mut index = 0;
    let text_block = !reply.text.is_empty()
        || (reply.tool_calls.is_empty() && reply.finish != Finish::ContentFilter);
    if text_block {
        ev.push(("content_block_start", json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}})));
        ev.push(("ping", json!({"type": "ping"})));
        for p in reply.pieces() {
            ev.push(("content_block_delta", json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": p}})));
        }
        ev.push((
            "content_block_stop",
            json!({"type": "content_block_stop", "index": index}),
        ));
        index += 1;
    }
    for c in &reply.tool_calls {
        ev.push(("content_block_start", json!({"type": "content_block_start", "index": index, "content_block": {"type": "tool_use", "id": format!("toolu_{}", c.id), "name": c.name, "input": {}}})));
        if index == 0 {
            ev.push(("ping", json!({"type": "ping"})));
        }
        ev.push(("content_block_delta", json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": ""}})));
        let args = c.arguments.to_string();
        for p in super::tokens::pieces(&args) {
            ev.push(("content_block_delta", json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": p}})));
        }
        ev.push((
            "content_block_stop",
            json!({"type": "content_block_stop", "index": index}),
        ));
        index += 1;
    }
    let (reason, seq) = stop_reason(&reply.finish);
    ev.push(("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": reason, "stop_sequence": seq}, "usage": {
        "input_tokens": cu.input,
        "cache_creation_input_tokens": cu.creation,
        "cache_read_input_tokens": cu.read,
        "output_tokens": reply.completion_tokens
    }})));
    ev.push(("message_stop", json!({"type": "message_stop"})));
    ev
}

async fn messages(Extension(ctx): Extension<AiCtx>, raw: Bytes) -> Response {
    let body: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => {
            return ctx.error(
                ErrorKind::BadRequest,
                format!("There was an issue with your request body: {e}"),
            )
        }
    };
    let req = match parse(&body, true) {
        Ok(r) => r,
        Err(m) => return ctx.error(ErrorKind::BadRequest, m),
    };
    let reply = ctx.generate(&req.input, 0);
    let cu = cache_usage(&ctx, &req, reply.prompt_tokens);
    let id = format!("msg_{}", rand_id(24));
    let model = req.input.model.clone();
    ctx.record_chat(
        &raw,
        &req.input,
        &reply,
        req.stream,
        stop_reason(&reply.finish).0,
    );
    let served = Served {
        model: model.clone(),
        mode: Some(reply.mode),
        tokens: reply.prompt_tokens + reply.completion_tokens,
    };
    if !req.stream {
        ctx.wait_ttft().await;
        return json_response(message_json(&id, &model, &reply, cu), served);
    }
    let events = stream_events(&id, &model, &reply, cu);
    let pace = ctx.pace;
    let deltas = events
        .iter()
        .filter(|(n, _)| *n == "content_block_delta")
        .count();
    let s = async_stream::stream! {
        let delay = pace.per_piece(deltas);
        let mut first = true;
        for (name, data) in events {
            if name == "content_block_delta" {
                if first {
                    first = false;
                    if !pace.ttft.is_zero() {
                        tokio::time::sleep(pace.ttft).await;
                    }
                } else if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
            }
            yield Ok::<_, Infallible>(sse_event(name, &data));
        }
    };
    stream_response("text/event-stream; charset=utf-8", s, served)
}

async fn count_tokens(Extension(ctx): Extension<AiCtx>, raw: Bytes) -> Response {
    let body: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => {
            return ctx.error(
                ErrorKind::BadRequest,
                format!("There was an issue with your request body: {e}"),
            )
        }
    };
    let req = match parse(&body, false) {
        Ok(r) => r,
        Err(m) => return ctx.error(ErrorKind::BadRequest, m),
    };
    let n = engine::prompt_tokens(&req.input);
    ctx.record(super::RecordArgs {
        body: &raw,
        model: &req.input.model,
        mode: "count_tokens",
        stream: false,
        prompt: &engine::render(&req.input),
        prompt_tokens: n,
        completion_tokens: 0,
        finish: "count",
        reply: "",
    });
    Json(json!({"input_tokens": n})).into_response()
}

pub const MODELS: &[(&str, &str)] = &[
    ("rustybin-claude", "Rustybin Claude (mock)"),
    ("claude-sonnet-4-5", "Claude Sonnet 4.5 (mock)"),
    ("claude-opus-4-1", "Claude Opus 4.1 (mock)"),
    ("claude-haiku-4-5", "Claude Haiku 4.5 (mock)"),
    ("rustybin-echo", "Rustybin echo mode"),
    ("rustybin-scripted", "Rustybin scripted mode"),
];

fn model_json(id: &str, name: &str) -> Value {
    json!({"type": "model", "id": id, "display_name": name, "created_at": "2025-01-01T00:00:00Z"})
}

async fn models() -> Response {
    let data: Vec<Value> = MODELS.iter().map(|(id, n)| model_json(id, n)).collect();
    Json(json!({
        "data": data,
        "has_more": false,
        "first_id": MODELS.first().map(|m| m.0),
        "last_id": MODELS.last().map(|m| m.0),
    }))
    .into_response()
}

async fn model(
    Extension(ctx): Extension<AiCtx>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match MODELS.iter().find(|(m, _)| *m == id) {
        Some((m, n)) => Json(model_json(m, n)).into_response(),
        None if id.starts_with("claude") || id.starts_with("rustybin") => {
            Json(model_json(&id, &id)).into_response()
        }
        None => ctx.error(
            ErrorKind::NotFound,
            format!("model: {}", id.chars().take(100).collect::<String>()),
        ),
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ai/anthropic/v1/messages", post(messages))
        .route("/ai/anthropic/v1/messages/count_tokens", post(count_tokens))
        .route("/ai/anthropic/v1/models", get(models))
        .route("/ai/anthropic/v1/models/{model}", get(model))
}

const EX: &str = r#"{"model":"rustybin-claude","max_tokens":256,"messages":[{"role":"user","content":"hello"}]}"#;
const EX_STREAM: &str = r#"{"model":"claude-sonnet-4-5","max_tokens":256,"stream":true,"messages":[{"role":"user","content":"hello"}]}"#;
const EX_TOOL: &str = r#"{"model":"claude-sonnet-4-5","max_tokens":512,"tools":[{"name":"get_weather","description":"Get the current weather for a location","input_schema":{"type":"object","properties":{"location":{"type":"string"}},"required":["location"]}}],"messages":[{"role":"user","content":"What is the weather in Paris?"}]}"#;
const EX_CACHE: &str = r#"{"model":"claude-sonnet-4-5","max_tokens":256,"system":[{"type":"text","text":"You are a helpful assistant with a very long cached system prompt.","cache_control":{"type":"ephemeral"}}],"messages":[{"role":"user","content":"hello"}]}"#;

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new("/ai/anthropic/v1/messages", &["POST"], category::AI_ANTHROPIC, "Messages API (tools, prompt caching usage, native SSE event stream with stream=true)")
            .description("Credential: x-api-key + anthropic-version (enforced with X-Rustybin-Require-Auth). max_tokens is required (400 otherwise). X-Rustybin-Fail: 529 returns overloaded_error.")
            .example(Example::post("Messages", "/ai/anthropic/v1/messages").json(EX))
            .example(Example::post("Messages (streaming)", "/ai/anthropic/v1/messages").json(EX_STREAM))
            .example(Example::post("Tool use", "/ai/anthropic/v1/messages").json(EX_TOOL))
            .example(Example::post("Prompt caching (cache_control)", "/ai/anthropic/v1/messages").json(EX_CACHE))
            .example(Example::post("Missing max_tokens", "/ai/anthropic/v1/messages").json(r#"{"model":"rustybin-claude","messages":[{"role":"user","content":"hello"}]}"#).expect_status(400))
            .example(Example::post("Simulated overload (529)", "/ai/anthropic/v1/messages").header("X-Rustybin-Fail", "529").json(EX).expect_status(529)),
        Endpoint::new("/ai/anthropic/v1/messages/count_tokens", &["POST"], category::AI_ANTHROPIC, "Count input tokens")
            .example(Example::post("Count tokens", "/ai/anthropic/v1/messages/count_tokens").json(r#"{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hello"}]}"#)),
        Endpoint::new("/ai/anthropic/v1/models", &["GET"], category::AI_ANTHROPIC, "List models")
            .example(Example::get("List models", "/ai/anthropic/v1/models")),
        Endpoint::new("/ai/anthropic/v1/models/{model}", &["GET"], category::AI_ANTHROPIC, "Retrieve a model")
            .example(Example::get("Retrieve a model", "/ai/anthropic/v1/models/claude-sonnet-4-5")),
    ]
}

pub fn request_schema() -> Value {
    json!({
        "type": "object",
        "required": ["model", "max_tokens", "messages"],
        "properties": {
            "model": {"type": "string", "example": "claude-sonnet-4-5"},
            "max_tokens": {"type": "integer", "example": 256},
            "system": {"oneOf": [{"type": "string"}, {"type": "array", "items": {"type": "object", "properties": {
                "type": {"type": "string"}, "text": {"type": "string"}, "cache_control": {"type": "object"}
            }}}]},
            "messages": {"type": "array", "items": {"type": "object", "required": ["role", "content"], "properties": {
                "role": {"type": "string", "enum": ["user", "assistant"]},
                "content": {"oneOf": [{"type": "string"}, {"type": "array", "items": {"type": "object", "properties": {
                    "type": {"type": "string", "enum": ["text", "image", "document", "tool_use", "tool_result"]}
                }}}]}
            }}},
            "tools": {"type": "array", "items": {"type": "object", "properties": {
                "name": {"type": "string"}, "description": {"type": "string"}, "input_schema": {"type": "object"}
            }}},
            "tool_choice": {"type": "object", "properties": {"type": {"type": "string", "enum": ["auto", "any", "tool", "none"]}, "name": {"type": "string"}}},
            "stop_sequences": {"type": "array", "items": {"type": "string"}},
            "stream": {"type": "boolean", "default": false}
        }
    })
}

pub fn response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "string"},
            "type": {"type": "string", "example": "message"},
            "role": {"type": "string"},
            "model": {"type": "string"},
            "content": {"type": "array", "items": {"type": "object"}},
            "stop_reason": {"type": "string", "enum": ["end_turn", "max_tokens", "stop_sequence", "tool_use", "refusal"]},
            "stop_sequence": {"type": "string", "nullable": true},
            "usage": {"type": "object", "properties": {
                "input_tokens": {"type": "integer"},
                "output_tokens": {"type": "integer"},
                "cache_creation_input_tokens": {"type": "integer"},
                "cache_read_input_tokens": {"type": "integer"}
            }}
        }
    })
}

pub fn openapi_paths() -> Value {
    let mut params = vec![
        json!({"name": "x-api-key", "in": "header", "schema": {"type": "string"}}),
        json!({"name": "anthropic-version", "in": "header", "schema": {"type": "string", "example": "2023-06-01"}}),
    ];
    if let Value::Array(c) = super::common_parameters() {
        params.extend(c);
    }
    json!({
        "/ai/anthropic/v1/messages": {"post": {
            "tags": ["AI Gateway"],
            "summary": "Anthropic Messages API (mock)",
            "operationId": "anthropicMessages",
            "parameters": params,
            "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/AnthropicMessagesRequest"}}}},
            "responses": {
                "200": {"description": "Message, or text/event-stream when stream=true", "content": {
                    "application/json": {"schema": {"$ref": "#/components/schemas/AnthropicMessage"}},
                    "text/event-stream": {"schema": {"type": "string"}}
                }},
                "400": {"description": "invalid_request_error (e.g. missing max_tokens)"},
                "401": {"description": "authentication_error (when required)"},
                "529": {"description": "overloaded_error (simulated)"}
            }
        }},
        "/ai/anthropic/v1/messages/count_tokens": {"post": {
            "tags": ["AI Gateway"],
            "summary": "Count input tokens",
            "operationId": "anthropicCountTokens",
            "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/AnthropicMessagesRequest"}}}},
            "responses": {"200": {"description": "Token count", "content": {"application/json": {"schema": {"type": "object", "properties": {"input_tokens": {"type": "integer"}}}}}}}
        }},
        "/ai/anthropic/v1/models": {"get": {
            "tags": ["AI Gateway"], "summary": "List models", "operationId": "anthropicListModels",
            "responses": {"200": {"description": "Models"}}
        }},
        "/ai/anthropic/v1/models/{model}": {"get": {
            "tags": ["AI Gateway"], "summary": "Retrieve a model", "operationId": "anthropicGetModel",
            "parameters": [{"name": "model", "in": "path", "required": true, "schema": {"type": "string"}}],
            "responses": {"200": {"description": "Model"}, "404": {"description": "not_found_error"}}
        }},
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    const URL: &str = "/ai/anthropic/v1/messages";

    #[tokio::test]
    async fn non_stream_and_validation() {
        let app = app();
        let (s, h, b) = send(
            &app,
            URL,
            &serde_json::from_str(EX).expect("json"),
            &[("x-api-key", "sk-ant-abcd")],
        )
        .await;
        assert_eq!(s, 200);
        assert_eq!(h["x-rustybin-credential"], "x-api-key ****abcd");
        assert!(h.contains_key("anthropic-ratelimit-tokens-remaining"));
        assert!(h.contains_key("request-id"));
        let v = json(&b);
        assert_eq!(v["type"], "message");
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["stop_reason"], "end_turn");
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["usage"]["cache_read_input_tokens"], 0);
        let (s, _, b) = send(
            &app,
            URL,
            &json!({"messages": [{"role": "user", "content": "hi"}]}),
            &[],
        )
        .await;
        assert_eq!(s, 400);
        let v = json(&b);
        assert_eq!(v["type"], "error");
        assert_eq!(v["error"]["type"], "invalid_request_error");
        assert!(v["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("max_tokens"));
        let (_, _, b) = send(
            &app,
            URL,
            &json!({"max_tokens": 2, "messages": [{"role": "user", "content": "hello"}]}),
            &[],
        )
        .await;
        assert_eq!(json(&b)["stop_reason"], "max_tokens");
        let (_, _, b) = send(&app, URL, &json!({"max_tokens": 100, "stop_sequences": ["mock"], "messages": [{"role": "user", "content": "hello"}]}), &[]).await;
        let v = json(&b);
        assert_eq!(v["stop_reason"], "stop_sequence");
        assert_eq!(v["stop_sequence"], "mock");
    }

    #[tokio::test]
    async fn system_blocks_and_cache_usage() {
        let app = app();
        let body: Value = serde_json::from_str(EX_CACHE).expect("json");
        let (_, _, b) = send(&app, URL, &body, &[]).await;
        let first = json(&b);
        let created = first["usage"]["cache_creation_input_tokens"]
            .as_u64()
            .unwrap_or(0);
        assert!(created > 0);
        assert_eq!(first["usage"]["cache_read_input_tokens"], 0);
        let (_, _, b) = send(&app, URL, &body, &[]).await;
        let second = json(&b);
        assert_eq!(
            second["usage"]["cache_read_input_tokens"].as_u64(),
            Some(created)
        );
        assert_eq!(second["usage"]["cache_creation_input_tokens"], 0);
        assert_eq!(
            second["usage"]["input_tokens"],
            first["usage"]["input_tokens"]
        );
        // Echo shows the system blocks.
        let (_, _, b) = send(&app, URL, &body, &[("x-rustybin-mode", "echo")]).await;
        assert!(json(&b)["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .starts_with("system: You are a helpful"));
    }

    #[tokio::test]
    async fn tool_use_round_trip_and_stream() {
        let app = app();
        let body: Value = serde_json::from_str(EX_TOOL).expect("json");
        let (_, _, b) = send(&app, URL, &body, &[]).await;
        let v = json(&b);
        assert_eq!(v["stop_reason"], "tool_use");
        let tu = v["content"][0].clone();
        assert_eq!(tu["type"], "tool_use");
        assert!(tu["id"].as_str().unwrap_or("").starts_with("toolu_"));
        assert_eq!(tu["input"]["location"], "Paris");

        let follow = json!({"max_tokens": 200, "tools": body["tools"], "messages": [
            {"role": "user", "content": "What is the weather in Paris?"},
            {"role": "assistant", "content": [tu]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tu["id"], "content": [{"type": "text", "text": "18C and cloudy"}]}]}
        ]});
        let (_, _, b) = send(&app, URL, &follow, &[]).await;
        let v = json(&b);
        assert_eq!(v["stop_reason"], "end_turn");
        assert!(v["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("18C and cloudy"));

        let mut sbody = body.clone();
        sbody["stream"] = json!(true);
        let (_, h, b) = send(&app, URL, &sbody, &[]).await;
        assert!(h["content-type"]
            .to_str()
            .unwrap_or("")
            .starts_with("text/event-stream"));
        let events = sse_events(&b);
        assert_eq!(events.first().map(String::as_str), Some("message_start"));
        assert_eq!(events.last().map(String::as_str), Some("message_stop"));
        let data = sse_data(&b);
        let partial: String = data
            .iter()
            .filter_map(|d| d["delta"]["partial_json"].as_str())
            .collect();
        assert_eq!(
            serde_json::from_str::<Value>(&partial).expect("json")["location"],
            "Paris"
        );
        let md = data
            .iter()
            .find(|d| d["type"] == "message_delta")
            .expect("message_delta");
        assert_eq!(md["delta"]["stop_reason"], "tool_use");
    }

    #[tokio::test]
    async fn text_stream_and_count_tokens_and_models() {
        let app = app();
        let (_, _, b) = send(
            &app,
            URL,
            &serde_json::from_str(EX_STREAM).expect("json"),
            &[],
        )
        .await;
        let data = sse_data(&b);
        let text: String = data
            .iter()
            .filter_map(|d| d["delta"]["text"].as_str())
            .collect();
        assert_eq!(text, engine::GREETING);
        let md = data
            .iter()
            .find(|d| d["type"] == "message_delta")
            .expect("md");
        assert_eq!(
            md["usage"]["output_tokens"].as_u64(),
            Some(u64::from(super::super::tokens::count(engine::GREETING)))
        );

        let (s, _, b) = send(
            &app,
            "/ai/anthropic/v1/messages/count_tokens",
            &json!({"model": "x", "messages": [{"role": "user", "content": "hello"}]}),
            &[],
        )
        .await;
        assert_eq!(s, 200);
        assert!(json(&b)["input_tokens"].as_u64().unwrap_or(0) > 0);
        let (s, _, b) = fetch(&app, "/ai/anthropic/v1/models").await;
        assert_eq!(s, 200);
        assert_eq!(json(&b)["data"][0]["type"], "model");
        let (s, _, _) = fetch(&app, "/ai/anthropic/v1/models/gpt-nope").await;
        assert_eq!(s, 404);
    }

    #[tokio::test]
    async fn native_auth_errors() {
        let app = app();
        let body: Value = serde_json::from_str(EX).expect("json");
        let (s, _, b) = send(&app, URL, &body, &[("x-rustybin-require-auth", "true")]).await;
        assert_eq!(s, 401);
        assert_eq!(json(&b)["error"]["type"], "authentication_error");
        let (s, _, _) = send(
            &app,
            URL,
            &body,
            &[
                ("x-rustybin-require-auth", "true"),
                ("x-api-key", "k"),
                ("anthropic-version", "2023-06-01"),
            ],
        )
        .await;
        assert_eq!(s, 200);
    }
}
