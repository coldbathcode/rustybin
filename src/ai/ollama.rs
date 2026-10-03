//! Ollama API under `/ai/ollama/api`: `chat`, `generate` (NDJSON streaming,
//! on by default like Ollama), `tags`, `embed` and legacy `embeddings`.

use axum::body::Bytes;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::{json, Value};
use std::convert::Infallible;

use super::engine::{ChatInput, Finish, Format, Msg, Part, Reply, Role, Tool, ToolCall};
use super::faults::ErrorKind;
use super::{embed, json_response, stream_response, AiCtx, Served};
use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

pub const DEFAULT_MODEL: &str = "llama3.2";

fn format_of(v: Option<&Value>) -> Format {
    match v {
        Some(Value::String(s)) if s == "json" => Format::JsonObject,
        Some(o @ Value::Object(_)) => Format::JsonSchema(o.clone()),
        _ => Format::Text,
    }
}

fn options(body: &Value) -> (Option<u32>, Vec<String>) {
    let o = body.get("options").cloned().unwrap_or(json!({}));
    let max = o
        .get("num_predict")
        .and_then(Value::as_i64)
        .filter(|n| *n >= 0)
        .map(|n| n.min(i64::from(u32::MAX)) as u32);
    let stop = match o.get("stop") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .take(8)
            .map(String::from)
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    };
    (max, stop)
}

fn parse_chat(body: &Value) -> Result<ChatInput, String> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or("model is required")?;
    let (max_tokens, stop) = options(body);
    let mut input = ChatInput {
        model: model.to_string(),
        format: format_of(body.get("format")),
        max_tokens,
        stop,
        ..Default::default()
    };
    for t in body
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let f = t.get("function").unwrap_or(t);
        if let Some(name) = f.get("name").and_then(Value::as_str) {
            input.tools.push(Tool {
                name: name.to_string(),
                description: f
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                parameters: f
                    .get("parameters")
                    .cloned()
                    .unwrap_or(json!({"type": "object"})),
            });
        }
    }
    for m in body
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let content = m
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        match m.get("role").and_then(Value::as_str).unwrap_or("user") {
            "system" => input.system.push(content),
            "assistant" => {
                let mut parts = Vec::new();
                if !content.is_empty() {
                    parts.push(Part::Text(content));
                }
                for tc in m
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let f = tc.get("function").unwrap_or(tc);
                    let name = f
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    parts.push(Part::ToolCall(ToolCall {
                        id: name.clone(),
                        name,
                        arguments: f.get("arguments").cloned().unwrap_or(json!({})),
                    }));
                }
                input.messages.push(Msg {
                    role: Role::Assistant,
                    parts,
                });
            }
            "tool" => input.messages.push(Msg {
                role: Role::Tool,
                parts: vec![Part::ToolResult {
                    id: String::new(),
                    name: m
                        .get("tool_name")
                        .or_else(|| m.get("name"))
                        .and_then(Value::as_str)
                        .map(String::from),
                    content,
                    is_error: false,
                }],
            }),
            _ => {
                let mut parts = vec![Part::Text(content)];
                for _ in m
                    .get("images")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    parts.push(Part::Image("image (base64)".into()));
                }
                input.messages.push(Msg {
                    role: Role::User,
                    parts,
                });
            }
        }
    }
    Ok(input)
}

fn done_reason(f: &Finish) -> &'static str {
    match f {
        Finish::Length => "length",
        _ => "stop",
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

fn final_stats(r: &Reply) -> Value {
    json!({
        "total_duration": 120_000_000u64,
        "load_duration": 1_000_000u64,
        "prompt_eval_count": r.prompt_tokens,
        "prompt_eval_duration": 10_000_000u64,
        "eval_count": r.completion_tokens,
        "eval_duration": 100_000_000u64,
    })
}

fn merge(mut a: Value, b: Value) -> Value {
    if let (Some(a), Value::Object(b)) = (a.as_object_mut(), b) {
        a.extend(b);
    }
    a
}

/// NDJSON stream (or one JSON object) for chat or generate.
fn respond(ctx: AiCtx, model: String, reply: Reply, chat: bool, stream: bool) -> Response {
    let served = Served {
        model: model.clone(),
        mode: Some(reply.mode),
        tokens: reply.prompt_tokens + reply.completion_tokens,
    };
    let tool_calls: Vec<Value> = reply
        .tool_calls
        .iter()
        .map(|c| json!({"function": {"name": c.name, "arguments": c.arguments}}))
        .collect();
    let piece = |text: &str, done: bool| {
        let base = if chat {
            let mut msg = json!({"role": "assistant", "content": text});
            if done && !tool_calls.is_empty() {
                msg["tool_calls"] = json!(tool_calls);
            }
            json!({"model": model, "created_at": now(), "message": msg, "done": done})
        } else {
            json!({"model": model, "created_at": now(), "response": text, "done": done})
        };
        if done {
            merge(
                merge(base, json!({"done_reason": done_reason(&reply.finish)})),
                final_stats(&reply),
            )
        } else {
            base
        }
    };
    if !stream {
        let v = piece(&reply.text, true);
        return json_response(v, served);
    }
    let lines: Vec<Value> = reply
        .pieces()
        .iter()
        .map(|p| piece(p, false))
        .chain(std::iter::once(piece("", true)))
        .collect();
    let pace = ctx.pace;
    let s = async_stream::stream! {
        let delay = pace.per_piece(lines.len());
        if !pace.ttft.is_zero() {
            tokio::time::sleep(pace.ttft).await;
        }
        for (i, l) in lines.into_iter().enumerate() {
            if i > 0 && !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            yield Ok::<_, Infallible>(Bytes::from(format!("{l}\n")));
        }
    };
    stream_response("application/x-ndjson", s, served)
}

async fn chat(Extension(ctx): Extension<AiCtx>, raw: Bytes) -> Response {
    let body: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return ctx.error(ErrorKind::BadRequest, format!("invalid JSON: {e}")),
    };
    let input = match parse_chat(&body) {
        Ok(i) => i,
        Err(m) => return ctx.error(ErrorKind::BadRequest, m),
    };
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(true);
    let reply = ctx.generate(&input, 0);
    ctx.record_chat(&raw, &input, &reply, stream, done_reason(&reply.finish));
    if !stream {
        ctx.wait_ttft().await;
    }
    respond(ctx, input.model, reply, true, stream)
}

async fn generate(Extension(ctx): Extension<AiCtx>, raw: Bytes) -> Response {
    let body: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return ctx.error(ErrorKind::BadRequest, format!("invalid JSON: {e}")),
    };
    let Some(model) = body.get("model").and_then(Value::as_str) else {
        return ctx.error(ErrorKind::BadRequest, "model is required");
    };
    let (max_tokens, stop) = options(&body);
    let mut input = ChatInput {
        model: model.to_string(),
        messages: vec![Msg::text(
            Role::User,
            body.get("prompt").and_then(Value::as_str).unwrap_or(""),
        )],
        format: format_of(body.get("format")),
        max_tokens,
        stop,
        ..Default::default()
    };
    if let Some(s) = body.get("system").and_then(Value::as_str) {
        input.system.push(s.to_string());
    }
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(true);
    let reply = ctx.generate(&input, 0);
    ctx.record_chat(&raw, &input, &reply, stream, done_reason(&reply.finish));
    if !stream {
        ctx.wait_ttft().await;
    }
    respond(ctx, input.model, reply, false, stream)
}

pub const MODELS: &[&str] = &[
    "llama3.2:latest",
    "qwen2.5:7b",
    "mistral:latest",
    "nomic-embed-text:latest",
    "rustybin-echo:latest",
];

async fn tags() -> Response {
    let models: Vec<Value> = MODELS
        .iter()
        .map(|m| {
            let digest: String = super::engine::stable_hash(m).iter().map(|b| format!("{b:02x}")).collect();
            json!({
                "name": m, "model": m, "modified_at": "2025-01-01T00:00:00Z", "size": 2_019_393_189u64,
                "digest": digest,
                "details": {"parent_model": "", "format": "gguf", "family": "llama", "families": ["llama"], "parameter_size": "3.2B", "quantization_level": "Q4_K_M"}
            })
        })
        .collect();
    Json(json!({"models": models})).into_response()
}

async fn embed_handler(Extension(ctx): Extension<AiCtx>, raw: Bytes) -> Response {
    let body: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return ctx.error(ErrorKind::BadRequest, format!("invalid JSON: {e}")),
    };
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("nomic-embed-text")
        .to_string();
    let inputs: Vec<String> = match body.get("input") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect(),
        _ => return ctx.error(ErrorKind::BadRequest, "input is required"),
    };
    if inputs.len() > ctx.shared.max_inputs() {
        return ctx.error(ErrorKind::BadRequest, "too many inputs");
    }
    let dims = body
        .get("dimensions")
        .and_then(Value::as_u64)
        .map(|d| d as usize)
        .unwrap_or(768);
    if dims == 0 || dims > ctx.shared.max_dims() {
        return ctx.error(ErrorKind::BadRequest, "dimensions out of range");
    }
    let tokens: u32 = inputs.iter().map(|s| super::tokens::count(s)).sum();
    let embeddings: Vec<Vec<f32>> = inputs.iter().map(|s| embed::embed(s, dims)).collect();
    ctx.record(super::RecordArgs {
        body: &raw,
        model: &model,
        mode: "embedding",
        stream: false,
        prompt: &inputs.join("\n"),
        prompt_tokens: tokens,
        completion_tokens: 0,
        finish: "stop",
        reply: "",
    });
    json_response(
        json!({"model": model, "embeddings": embeddings, "total_duration": 14_143_917u64, "load_duration": 1_019_500u64, "prompt_eval_count": tokens}),
        Served {
            model,
            mode: None,
            tokens,
        },
    )
}

async fn embeddings_legacy(Extension(ctx): Extension<AiCtx>, raw: Bytes) -> Response {
    let body: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return ctx.error(ErrorKind::BadRequest, format!("invalid JSON: {e}")),
    };
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("nomic-embed-text")
        .to_string();
    let prompt = body.get("prompt").and_then(Value::as_str).unwrap_or("");
    let tokens = super::tokens::count(prompt);
    json_response(
        json!({"embedding": embed::embed(prompt, 768)}),
        Served {
            model,
            mode: None,
            tokens,
        },
    )
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ai/ollama/api/chat", post(chat))
        .route("/ai/ollama/api/generate", post(generate))
        .route("/ai/ollama/api/tags", get(tags))
        .route("/ai/ollama/api/embed", post(embed_handler))
        .route("/ai/ollama/api/embeddings", post(embeddings_legacy))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new("/ai/ollama/api/chat", &["POST"], category::AI_MOCK, "Ollama chat (NDJSON stream by default; tools, format)")
            .example(Example::post("Ollama chat (non-streaming)", "/ai/ollama/api/chat").json(r#"{"model":"llama3.2","messages":[{"role":"user","content":"hello"}],"stream":false}"#))
            .example(Example::post("Ollama chat (NDJSON stream)", "/ai/ollama/api/chat").json(r#"{"model":"llama3.2","messages":[{"role":"user","content":"hello"}]}"#)),
        Endpoint::new("/ai/ollama/api/generate", &["POST"], category::AI_MOCK, "Ollama generate (NDJSON stream by default)")
            .example(Example::post("Ollama generate", "/ai/ollama/api/generate").json(r#"{"model":"llama3.2","prompt":"Why is the sky blue?","stream":false}"#)),
        Endpoint::new("/ai/ollama/api/tags", &["GET"], category::AI_MOCK, "Ollama local models")
            .example(Example::get("Ollama tags", "/ai/ollama/api/tags")),
        Endpoint::new("/ai/ollama/api/embed", &["POST"], category::AI_MOCK, "Ollama embeddings (768 dimensions by default)")
            .example(Example::post("Ollama embed", "/ai/ollama/api/embed").json(r#"{"model":"nomic-embed-text","input":["Hello world"]}"#)),
        Endpoint::new("/ai/ollama/api/embeddings", &["POST"], category::AI_MOCK, "Ollama legacy embeddings")
            .example(Example::post("Ollama legacy embeddings", "/ai/ollama/api/embeddings").json(r#"{"model":"nomic-embed-text","prompt":"Hello world"}"#)),
    ]
}

pub fn openapi_paths() -> Value {
    let op = |id: &str, summary: &str, ct: &str| {
        json!({"post": {
            "tags": ["AI Gateway"], "summary": summary, "operationId": id,
            "parameters": super::common_parameters(),
            "requestBody": {"required": true, "content": {"application/json": {"schema": {"type": "object", "properties": {
                "model": {"type": "string"}, "messages": {"type": "array", "items": {"type": "object"}},
                "prompt": {"type": "string"}, "input": {}, "stream": {"type": "boolean", "default": true},
                "format": {}, "options": {"type": "object"}, "tools": {"type": "array", "items": {"type": "object"}}
            }}}}},
            "responses": {"200": {"description": "OK", "content": {ct: {"schema": {"type": "object"}}}}, "400": {"description": "{\"error\": ...}"}}
        }})
    };
    json!({
        "/ai/ollama/api/chat": op("ollamaChat", "Ollama chat", "application/x-ndjson"),
        "/ai/ollama/api/generate": op("ollamaGenerate", "Ollama generate", "application/x-ndjson"),
        "/ai/ollama/api/embed": op("ollamaEmbed", "Ollama embed", "application/json"),
        "/ai/ollama/api/embeddings": op("ollamaEmbeddings", "Ollama legacy embeddings", "application/json"),
        "/ai/ollama/api/tags": {"get": {"tags": ["AI Gateway"], "summary": "Ollama tags", "operationId": "ollamaTags", "responses": {"200": {"description": "Models"}}}},
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    #[tokio::test]
    async fn chat_ndjson_and_non_stream() {
        let app = app();
        let (s, h, b) = send(
            &app,
            "/ai/ollama/api/chat",
            &json!({"model": "llama3.2", "messages": [{"role": "user", "content": "hello"}]}),
            &[],
        )
        .await;
        assert_eq!(s, 200);
        assert_eq!(h["content-type"], "application/x-ndjson");
        let lines: Vec<Value> = String::from_utf8_lossy(&b)
            .lines()
            .map(|l| serde_json::from_str(l).expect("ndjson"))
            .collect();
        let text: String = lines
            .iter()
            .filter_map(|l| l["message"]["content"].as_str())
            .collect();
        assert_eq!(text, super::super::engine::GREETING);
        let last = lines.last().expect("last");
        assert_eq!(last["done"], true);
        assert_eq!(last["done_reason"], "stop");
        assert!(last["eval_count"].as_u64().unwrap_or(0) > 0);

        let (_, _, b) = send(&app, "/ai/ollama/api/chat", &json!({"model": "llama3.2", "stream": false, "messages": [{"role": "user", "content": "weather in Paris?"}], "tools": [{"type": "function", "function": {"name": "get_weather", "description": "weather", "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}}]}), &[]).await;
        let v = json(&b);
        assert_eq!(
            v["message"]["tool_calls"][0]["function"]["arguments"]["city"],
            "Paris"
        );
        let (_, _, b) = send(
            &app,
            "/ai/ollama/api/generate",
            &json!({"model": "llama3.2", "prompt": "hello", "stream": false, "format": "json"}),
            &[],
        )
        .await;
        let v = json(&b);
        assert!(serde_json::from_str::<Value>(v["response"].as_str().unwrap_or("")).is_ok());
        let (s, _, _) = send(&app, "/ai/ollama/api/chat", &json!({"messages": []}), &[]).await;
        assert_eq!(s, 400);
    }

    #[tokio::test]
    async fn tags_and_embed() {
        let app = app();
        let (_, _, b) = fetch(&app, "/ai/ollama/api/tags").await;
        assert!(json(&b)["models"][0]["name"].is_string());
        let (_, _, b) = send(
            &app,
            "/ai/ollama/api/embed",
            &json!({"model": "nomic-embed-text", "input": ["a", "b"]}),
            &[],
        )
        .await;
        let v = json(&b);
        assert_eq!(v["embeddings"].as_array().map(Vec::len), Some(2));
        assert_eq!(v["embeddings"][0].as_array().map(Vec::len), Some(768));
    }
}
