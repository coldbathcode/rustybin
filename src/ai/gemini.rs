//! Gemini API (Generative Language, v1beta) under `/ai/gemini/v1beta`.
//!
//! The model and the action share one path segment
//! (`models/gemini-2.0-flash:generateContent`), which axum cannot match
//! with a parameter plus a literal, so `/ai/gemini/v1beta/models/{*rest}`
//! is parsed here: `generateContent`, `streamGenerateContent` (SSE with
//! `?alt=sse`, else a streamed JSON array), `countTokens`, `embedContent`,
//! `batchEmbedContents`; a GET without an action returns the model.
//!
//! Supports `systemInstruction`, `tools[].functionDeclarations` ->
//! `functionCall` parts, `functionResponse` parts, `toolConfig`
//! (AUTO / ANY / NONE, allowedFunctionNames), `generationConfig`
//! (`maxOutputTokens`, `stopSequences`, `candidateCount`,
//! `responseMimeType` + `responseSchema` / `responseJsonSchema`) and
//! `usageMetadata`.

use axum::body::Bytes;
use axum::extract::Path;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde_json::{json, Value};
use std::convert::Infallible;

use super::engine::{
    ChatInput, Finish, Format, Msg, Part, Reply, Role, Tool, ToolCall, ToolChoice,
};
use super::faults::ErrorKind;
use super::{embed, json_response, rand_id, stream_response, AiCtx, Served};
use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

fn field<'a>(v: &'a Value, camel: &str, snake: &str) -> Option<&'a Value> {
    v.get(camel).or_else(|| v.get(snake))
}

fn parse_parts(parts: Option<&Value>) -> Vec<Part> {
    let mut out = Vec::new();
    for p in parts.and_then(Value::as_array).into_iter().flatten() {
        if let Some(t) = p.get("text").and_then(Value::as_str) {
            out.push(Part::Text(t.to_string()));
        } else if let Some(d) = field(p, "inlineData", "inline_data") {
            let mime = field(d, "mimeType", "mime_type")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream");
            if mime.starts_with("audio/") {
                out.push(Part::Audio(mime.to_string()));
            } else if mime.starts_with("image/") {
                out.push(Part::Image(format!("{mime} (inline)")));
            } else {
                out.push(Part::File(mime.to_string()));
            }
        } else if let Some(d) = field(p, "fileData", "file_data") {
            let uri = field(d, "fileUri", "file_uri")
                .and_then(Value::as_str)
                .unwrap_or("file");
            out.push(Part::File(uri.to_string()));
        } else if let Some(fc) = field(p, "functionCall", "function_call") {
            let name = fc
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            out.push(Part::ToolCall(ToolCall {
                id: fc
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(&name)
                    .to_string(),
                name,
                arguments: fc.get("args").cloned().unwrap_or(json!({})),
            }));
        } else if let Some(fr) = field(p, "functionResponse", "function_response") {
            let name = fr
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            out.push(Part::ToolResult {
                id: fr
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(&name)
                    .to_string(),
                name: Some(name),
                content: fr.get("response").map(Value::to_string).unwrap_or_default(),
                is_error: false,
            });
        }
    }
    out
}

/// Parse a GenerateContentRequest.
pub fn parse(body: &Value, model: &str) -> Result<(ChatInput, u32), String> {
    if !body.is_object() {
        return Err("Invalid JSON payload received.".into());
    }
    let contents = body
        .get("contents")
        .and_then(Value::as_array)
        .ok_or("* GenerateContentRequest.contents: contents is not specified")?;
    let gc = field(body, "generationConfig", "generation_config")
        .cloned()
        .unwrap_or(json!({}));
    let mut input = ChatInput {
        model: model.to_string(),
        max_tokens: field(&gc, "maxOutputTokens", "max_output_tokens")
            .and_then(Value::as_u64)
            .map(|n| n.min(u64::from(u32::MAX)) as u32),
        stop: field(&gc, "stopSequences", "stop_sequences")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .take(5)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        ..Default::default()
    };
    if let Some(si) = field(body, "systemInstruction", "system_instruction") {
        let text = match si {
            Value::String(s) => s.clone(),
            other => parse_parts(other.get("parts"))
                .into_iter()
                .filter_map(|p| match p {
                    Part::Text(t) => Some(t),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };
        input.system.push(text);
    }
    for c in contents {
        let role = match c.get("role").and_then(Value::as_str) {
            Some("model") => Role::Assistant,
            Some("function") | Some("tool") => Role::Tool,
            _ => Role::User,
        };
        input.messages.push(Msg {
            role,
            parts: parse_parts(c.get("parts")),
        });
    }
    for t in body
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for f in field(t, "functionDeclarations", "function_declarations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(name) = f.get("name").and_then(Value::as_str) else {
                continue;
            };
            let params = f
                .get("parameters")
                .or_else(|| field(f, "parametersJsonSchema", "parameters_json_schema"))
                .cloned()
                .unwrap_or(json!({"type": "object"}));
            input.tools.push(Tool {
                name: name.to_string(),
                description: f
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                parameters: params,
            });
        }
    }
    if let Some(fcc) = field(body, "toolConfig", "tool_config")
        .and_then(|t| field(t, "functionCallingConfig", "function_calling_config"))
    {
        let allowed: Vec<&str> = field(fcc, "allowedFunctionNames", "allowed_function_names")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        input.tool_choice = match fcc
            .get("mode")
            .and_then(Value::as_str)
            .map(str::to_ascii_uppercase)
            .as_deref()
        {
            Some("NONE") => ToolChoice::None,
            Some("ANY") | Some("VALIDATED") if allowed.len() == 1 => {
                ToolChoice::Named(allowed[0].to_string())
            }
            Some("ANY") => ToolChoice::Required,
            _ => ToolChoice::Auto,
        };
    }
    let mime = field(&gc, "responseMimeType", "response_mime_type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let schema = field(&gc, "responseSchema", "response_schema")
        .or_else(|| field(&gc, "responseJsonSchema", "response_json_schema"))
        .or_else(|| gc.get("_responseJsonSchema"));
    if let Some(s) = schema {
        input.format = Format::JsonSchema(s.clone());
    } else if mime == "application/json" {
        input.format = Format::JsonObject;
    }
    let n = field(&gc, "candidateCount", "candidate_count")
        .and_then(Value::as_u64)
        .unwrap_or(1) as u32;
    Ok((input, n))
}

fn finish_reason(f: &Finish) -> &'static str {
    match f {
        Finish::Length => "MAX_TOKENS",
        Finish::ContentFilter => "SAFETY",
        _ => "STOP",
    }
}

fn parts_json(r: &Reply) -> Vec<Value> {
    let mut parts = Vec::new();
    if !r.text.is_empty() || (r.tool_calls.is_empty() && r.finish != Finish::ContentFilter) {
        parts.push(json!({"text": r.text}));
    }
    for c in &r.tool_calls {
        parts.push(json!({"functionCall": {"name": c.name, "args": c.arguments}}));
    }
    parts
}

fn safety(blocked: bool) -> Value {
    let p = |cat: &str, b: bool| json!({"category": cat, "probability": if b { "HIGH" } else { "NEGLIGIBLE" }, "blocked": b});
    json!([
        p("HARM_CATEGORY_HATE_SPEECH", blocked),
        p("HARM_CATEGORY_DANGEROUS_CONTENT", false),
        p("HARM_CATEGORY_HARASSMENT", false),
        p("HARM_CATEGORY_SEXUALLY_EXPLICIT", false)
    ])
}

fn usage(prompt: u32, completion: u32) -> Value {
    json!({
        "promptTokenCount": prompt,
        "candidatesTokenCount": completion,
        "totalTokenCount": prompt + completion,
        "promptTokensDetails": [{"modality": "TEXT", "tokenCount": prompt}],
    })
}

fn response_json(model: &str, id: &str, replies: &[Reply]) -> Value {
    let candidates: Vec<Value> = replies
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut c = json!({
                "content": {"parts": parts_json(r), "role": "model"},
                "finishReason": finish_reason(&r.finish),
                "index": i,
            });
            if r.finish == Finish::ContentFilter {
                c["safetyRatings"] = safety(true);
            }
            c
        })
        .collect();
    let prompt = replies.first().map(|r| r.prompt_tokens).unwrap_or(0);
    let completion: u32 = replies.iter().map(|r| r.completion_tokens).sum();
    json!({
        "candidates": candidates,
        "usageMetadata": usage(prompt, completion),
        "modelVersion": model,
        "responseId": id,
    })
}

/// Split `model:action` (the model may itself be `models/...` or `tunedModels/...`).
fn split_rest(rest: &str) -> (String, Option<String>) {
    let rest = rest.trim_start_matches('/');
    match rest.rsplit_once(':') {
        Some((m, a)) if !a.contains('/') && !a.is_empty() => (m.to_string(), Some(a.to_string())),
        _ => (rest.to_string(), None),
    }
}

fn model_json(id: &str) -> Value {
    let embed = id.contains("embed");
    json!({
        "name": format!("models/{id}"),
        "version": "001",
        "displayName": format!("{id} (Rustybin mock)"),
        "description": "Deterministic mock model served by Rustybin.",
        "inputTokenLimit": if embed { 2048 } else { 1_048_576 },
        "outputTokenLimit": if embed { 1 } else { 8192 },
        "supportedGenerationMethods": if embed { json!(["embedContent", "batchEmbedContents"]) } else { json!(["generateContent", "streamGenerateContent", "countTokens"]) },
    })
}

pub const MODELS: &[&str] = &[
    "rustybin-gemini",
    "gemini-2.5-flash",
    "gemini-2.5-pro",
    "gemini-2.0-flash",
    "text-embedding-004",
    "gemini-embedding-001",
];

async fn list_models() -> Response {
    let models: Vec<Value> = MODELS.iter().map(|m| model_json(m)).collect();
    Json(json!({"models": models})).into_response()
}

async fn get_model(Path(rest): Path<String>) -> Response {
    let (model, _) = split_rest(&rest);
    Json(model_json(&model)).into_response()
}

#[allow(clippy::result_large_err)]
fn embed_dims(ctx: &AiCtx, model: &str, req: &Value) -> Result<usize, Response> {
    let d = field(req, "outputDimensionality", "output_dimensionality")
        .and_then(Value::as_u64)
        .map(|d| d as usize)
        .unwrap_or(if model.contains("gemini-embedding") {
            3072
        } else {
            768
        });
    if d == 0 || d > ctx.shared.max_dims() {
        return Err(ctx.error(
            ErrorKind::BadRequest,
            format!(
                "outputDimensionality must be between 1 and {}",
                ctx.shared.max_dims()
            ),
        ));
    }
    Ok(d)
}

fn content_text(c: Option<&Value>) -> String {
    parse_parts(c.and_then(|c| c.get("parts")))
        .into_iter()
        .filter_map(|p| match p {
            Part::Text(t) => Some(t),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn action(
    Extension(ctx): Extension<AiCtx>,
    Path(rest): Path<String>,
    raw: Bytes,
) -> Response {
    let (model, action) = split_rest(&rest);
    let model = model.trim_start_matches("models/").to_string();
    let Some(action) = action else {
        return ctx.error(
            ErrorKind::NotFound,
            format!("POST requires an action: models/{model}:generateContent"),
        );
    };
    let body: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => {
            return ctx.error(
                ErrorKind::BadRequest,
                format!("Invalid JSON payload received. {e}"),
            )
        }
    };
    match action.as_str() {
        "generateContent" | "streamGenerateContent" => {
            generate(ctx, &raw, &body, model, action == "streamGenerateContent").await
        }
        "countTokens" => {
            let req = body.get("generateContentRequest").unwrap_or(&body);
            let req = if req.get("contents").is_none() {
                json!({"contents": []})
            } else {
                req.clone()
            };
            match parse(&req, &model) {
                Ok((input, _)) => {
                    let n = super::engine::prompt_tokens(&input);
                    let n = if input.messages.is_empty() && input.system.is_empty() {
                        0
                    } else {
                        n
                    };
                    Json(json!({"totalTokens": n, "promptTokensDetails": [{"modality": "TEXT", "tokenCount": n}]})).into_response()
                }
                Err(m) => ctx.error(ErrorKind::BadRequest, m),
            }
        }
        "embedContent" => {
            let dims = match embed_dims(&ctx, &model, &body) {
                Ok(d) => d,
                Err(r) => return r,
            };
            let text = content_text(body.get("content"));
            let tokens = super::tokens::count(&text);
            ctx.record(super::RecordArgs {
                body: &raw,
                model: &model,
                mode: "embedding",
                stream: false,
                prompt: &text,
                prompt_tokens: tokens,
                completion_tokens: 0,
                finish: "stop",
                reply: "",
            });
            json_response(
                json!({"embedding": {"values": embed::embed(&text, dims)}}),
                Served {
                    model,
                    mode: None,
                    tokens,
                },
            )
        }
        "batchEmbedContents" => {
            let reqs: Vec<Value> = body
                .get("requests")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if reqs.len() > ctx.shared.max_inputs() {
                return ctx.error(
                    ErrorKind::BadRequest,
                    format!("at most {} requests per batch", ctx.shared.max_inputs()),
                );
            }
            let mut out = Vec::new();
            let mut tokens = 0;
            for r in &reqs {
                let dims = match embed_dims(&ctx, &model, r) {
                    Ok(d) => d,
                    Err(resp) => return resp,
                };
                let text = content_text(r.get("content"));
                tokens += super::tokens::count(&text);
                out.push(json!({"values": embed::embed(&text, dims)}));
            }
            ctx.record(super::RecordArgs {
                body: &raw,
                model: &model,
                mode: "embedding",
                stream: false,
                prompt: "",
                prompt_tokens: tokens,
                completion_tokens: 0,
                finish: "stop",
                reply: "",
            });
            json_response(
                json!({"embeddings": out}),
                Served {
                    model,
                    mode: None,
                    tokens,
                },
            )
        }
        other => ctx.error(
            ErrorKind::NotFound,
            format!(
                "Unknown action: {}",
                other.chars().take(64).collect::<String>()
            ),
        ),
    }
}

async fn generate(ctx: AiCtx, raw: &[u8], body: &Value, model: String, stream: bool) -> Response {
    let (input, n) = match parse(body, &model) {
        Ok(x) => x,
        Err(m) => return ctx.error(ErrorKind::BadRequest, m),
    };
    let n = n.clamp(1, ctx.shared.max_choices());
    let replies: Vec<Reply> = (0..n).map(|i| ctx.generate(&input, i)).collect();
    let first = &replies[0];
    ctx.record_chat(raw, &input, first, stream, finish_reason(&first.finish));
    let id = rand_id(22);
    let served = Served {
        model: model.clone(),
        mode: Some(first.mode),
        tokens: first.prompt_tokens + replies.iter().map(|r| r.completion_tokens).sum::<u32>(),
    };
    if !stream {
        ctx.wait_ttft().await;
        return json_response(response_json(&model, &id, &replies), served);
    }
    let sse = ctx.query.get("alt").map(String::as_str) == Some("sse");
    let pace = ctx.pace;
    let s = async_stream::stream! {
        let mut chunks: Vec<Value> = Vec::new();
        let r = replies[0].clone();
        let pieces: Vec<String> = r.pieces().iter().map(|p| p.to_string()).collect();
        let groups: Vec<String> = pieces.chunks(4).map(|c| c.concat()).collect();
        let total = groups.len();
        for (i, g) in groups.into_iter().enumerate() {
            let mut c = json!({"candidates": [{"content": {"parts": [{"text": g}], "role": "model"}, "index": 0}], "modelVersion": &model, "responseId": &id});
            if i + 1 == total && r.tool_calls.is_empty() {
                c["candidates"][0]["finishReason"] = json!(finish_reason(&r.finish));
                c["usageMetadata"] = usage(r.prompt_tokens, r.completion_tokens);
            } else {
                c["usageMetadata"] = json!({"promptTokenCount": r.prompt_tokens, "totalTokenCount": r.prompt_tokens});
            }
            chunks.push(c);
        }
        if !r.tool_calls.is_empty() || chunks.is_empty() {
            let parts: Vec<Value> = r.tool_calls.iter().map(|c| json!({"functionCall": {"name": c.name, "args": c.arguments}})).collect();
            let mut c = json!({"candidates": [{"content": {"parts": parts, "role": "model"}, "finishReason": finish_reason(&r.finish), "index": 0}], "usageMetadata": usage(r.prompt_tokens, r.completion_tokens), "modelVersion": &model, "responseId": &id});
            if r.finish == Finish::ContentFilter {
                c["candidates"][0]["safetyRatings"] = safety(true);
            }
            chunks.push(c);
        }
        let delay = pace.per_piece(pieces.len()) * 4;
        if !sse {
            yield Ok::<_, Infallible>(Bytes::from_static(b"["));
        }
        for (i, c) in chunks.iter().enumerate() {
            if i == 0 {
                if !pace.ttft.is_zero() {
                    tokio::time::sleep(pace.ttft).await;
                }
            } else if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if sse {
                yield Ok(Bytes::from(format!("data: {c}\r\n\r\n")));
            } else {
                let sep = if i == 0 { "" } else { ",\r\n" };
                yield Ok(Bytes::from(format!("{sep}{c}")));
            }
        }
        if !sse {
            yield Ok(Bytes::from_static(b"]"));
        }
    };
    let ct = if sse {
        "text/event-stream"
    } else {
        "application/json; charset=UTF-8"
    };
    stream_response(ct, s, served)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ai/gemini/v1beta/models", get(list_models))
        .route(
            "/ai/gemini/v1beta/models/{*rest}",
            get(get_model).post(action),
        )
}

const EX: &str = r#"{"contents":[{"role":"user","parts":[{"text":"hello"}]}]}"#;
const EX_TOOL: &str = r#"{"contents":[{"role":"user","parts":[{"text":"What is the weather in Paris?"}]}],"tools":[{"functionDeclarations":[{"name":"get_weather","description":"Get the current weather for a location","parameters":{"type":"OBJECT","properties":{"location":{"type":"STRING"}},"required":["location"]}}]}]}"#;
const EX_JSON: &str = r#"{"contents":[{"role":"user","parts":[{"text":"List a recipe"}]}],"generationConfig":{"responseMimeType":"application/json","responseSchema":{"type":"OBJECT","properties":{"recipe_name":{"type":"STRING"},"ingredients":{"type":"ARRAY","items":{"type":"STRING"}}},"required":["recipe_name"]}}}"#;

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new("/ai/gemini/v1beta/models", &["GET"], category::AI_MOCK, "Gemini: list models")
            .example(Example::get("List Gemini models", "/ai/gemini/v1beta/models")),
        Endpoint::new("/ai/gemini/v1beta/models/{*rest}", &["GET", "POST"], category::AI_MOCK, "Gemini: {model}:generateContent, :streamGenerateContent (?alt=sse), :countTokens, :embedContent, :batchEmbedContents")
            .description("Credential: x-goog-api-key or ?key=. functionDeclarations produce functionCall parts; generationConfig.responseSchema gives schema-valid JSON; usageMetadata on every response. GET models/{model} describes the model.")
            .example(Example::post("generateContent", "/ai/gemini/v1beta/models/gemini-2.5-flash:generateContent").header("x-goog-api-key", "gemini-demo-key").json(EX))
            .example(Example::post("streamGenerateContent (SSE)", "/ai/gemini/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse").json(EX))
            .example(Example::post("Function calling", "/ai/gemini/v1beta/models/gemini-2.5-flash:generateContent").json(EX_TOOL))
            .example(Example::post("Structured output", "/ai/gemini/v1beta/models/gemini-2.5-flash:generateContent").json(EX_JSON))
            .example(Example::post("countTokens", "/ai/gemini/v1beta/models/gemini-2.5-flash:countTokens").json(EX))
            .example(Example::post("embedContent", "/ai/gemini/v1beta/models/text-embedding-004:embedContent").json(r#"{"content":{"parts":[{"text":"Hello world"}]}}"#))
            .example(Example::get("Get a model", "/ai/gemini/v1beta/models/gemini-2.5-flash")),
    ]
}

pub fn openapi_paths() -> Value {
    let mut params = vec![
        json!({"name": "rest", "in": "path", "required": true, "schema": {"type": "string"}, "description": "{model}:{action}, e.g. gemini-2.5-flash:generateContent"}),
        json!({"name": "alt", "in": "query", "schema": {"type": "string", "enum": ["sse"]}}),
        json!({"name": "key", "in": "query", "schema": {"type": "string"}}),
        json!({"name": "x-goog-api-key", "in": "header", "schema": {"type": "string"}}),
    ];
    if let Value::Array(c) = super::common_parameters() {
        params.extend(c);
    }
    json!({
        "/ai/gemini/v1beta/models": {"get": {
            "tags": ["AI Gateway"], "summary": "Gemini: list models", "operationId": "geminiListModels",
            "responses": {"200": {"description": "Models"}}
        }},
        "/ai/gemini/v1beta/models/{rest}": {
            "get": {
                "tags": ["AI Gateway"], "summary": "Gemini: get a model", "operationId": "geminiGetModel",
                "parameters": [{"name": "rest", "in": "path", "required": true, "schema": {"type": "string"}}],
                "responses": {"200": {"description": "Model"}}
            },
            "post": {
                "tags": ["AI Gateway"],
                "summary": "Gemini: generateContent / streamGenerateContent / countTokens / embedContent / batchEmbedContents",
                "operationId": "geminiAction",
                "parameters": params,
                "requestBody": {"required": true, "content": {"application/json": {"schema": {"type": "object", "properties": {
                    "contents": {"type": "array", "items": {"type": "object", "properties": {
                        "role": {"type": "string", "enum": ["user", "model"]},
                        "parts": {"type": "array", "items": {"type": "object"}}
                    }}},
                    "systemInstruction": {"type": "object"},
                    "tools": {"type": "array", "items": {"type": "object"}},
                    "toolConfig": {"type": "object"},
                    "generationConfig": {"type": "object", "properties": {
                        "maxOutputTokens": {"type": "integer"},
                        "responseMimeType": {"type": "string"},
                        "responseSchema": {"type": "object"},
                        "candidateCount": {"type": "integer"},
                        "stopSequences": {"type": "array", "items": {"type": "string"}}
                    }},
                    "content": {"type": "object", "description": "embedContent"},
                    "requests": {"type": "array", "items": {"type": "object"}, "description": "batchEmbedContents"}
                }}}}},
                "responses": {
                    "200": {"description": "GenerateContentResponse (candidates, usageMetadata), SSE chunks, token count or embedding"},
                    "400": {"description": "INVALID_ARGUMENT"},
                    "403": {"description": "PERMISSION_DENIED (missing key, when required)"},
                    "429": {"description": "RESOURCE_EXHAUSTED (simulated)"}
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    const BASE: &str = "/ai/gemini/v1beta/models/gemini-2.5-flash";

    #[tokio::test]
    async fn generate_and_stream() {
        let app = app();
        let (s, _, b) = send(
            &app,
            &format!("{BASE}:generateContent?key=abcd1234"),
            &serde_json::from_str(EX).expect("json"),
            &[],
        )
        .await;
        assert_eq!(s, 200);
        let v = json(&b);
        assert_eq!(v["candidates"][0]["content"]["role"], "model");
        assert_eq!(v["candidates"][0]["finishReason"], "STOP");
        assert_eq!(
            v["candidates"][0]["content"]["parts"][0]["text"],
            super::super::engine::GREETING
        );
        assert!(v["usageMetadata"]["totalTokenCount"].as_u64().unwrap_or(0) > 0);

        let (s, h, b) = send(
            &app,
            &format!("{BASE}:streamGenerateContent?alt=sse"),
            &serde_json::from_str(EX).expect("json"),
            &[],
        )
        .await;
        assert_eq!(s, 200);
        assert!(h["content-type"]
            .to_str()
            .unwrap_or("")
            .starts_with("text/event-stream"));
        let chunks = sse_data(&b);
        let text: String = chunks
            .iter()
            .filter_map(|c| c["candidates"][0]["content"]["parts"][0]["text"].as_str())
            .collect();
        assert_eq!(text, super::super::engine::GREETING);
        assert_eq!(
            chunks
                .last()
                .map(|c| c["candidates"][0]["finishReason"].clone()),
            Some(json!("STOP"))
        );

        let (_, _, b) = send(
            &app,
            &format!("{BASE}:streamGenerateContent"),
            &serde_json::from_str(EX).expect("json"),
            &[],
        )
        .await;
        let arr: Value = serde_json::from_slice(&b).expect("json array");
        assert!(arr.as_array().map(|a| a.len() > 1).unwrap_or(false));
    }

    #[tokio::test]
    async fn functions_schema_tokens_embeddings() {
        let app = app();
        let (_, _, b) = send(
            &app,
            &format!("{BASE}:generateContent"),
            &serde_json::from_str(EX_TOOL).expect("json"),
            &[],
        )
        .await;
        let v = json(&b);
        let fc = &v["candidates"][0]["content"]["parts"][0]["functionCall"];
        assert_eq!(fc["name"], "get_weather");
        assert_eq!(fc["args"]["location"], "Paris");
        let follow = json!({"contents": [
            {"role": "user", "parts": [{"text": "What is the weather in Paris?"}]},
            {"role": "model", "parts": [{"functionCall": fc}]},
            {"role": "user", "parts": [{"functionResponse": {"name": "get_weather", "response": {"temp": 19}}}]}
        ]});
        let (_, _, b) = send(&app, &format!("{BASE}:generateContent"), &follow, &[]).await;
        assert!(json(&b)["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("get_weather returned"));

        let body: Value = serde_json::from_str(EX_JSON).expect("json");
        let (_, _, b) = send(&app, &format!("{BASE}:generateContent"), &body, &[]).await;
        let text = json(&b)["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let out: Value = serde_json::from_str(&text).expect("json output");
        assert!(
            super::super::schema::validate(&out, &body["generationConfig"]["responseSchema"])
                .is_ok()
        );

        let (_, _, b) = send(
            &app,
            &format!("{BASE}:countTokens"),
            &serde_json::from_str(EX).expect("json"),
            &[],
        )
        .await;
        assert!(json(&b)["totalTokens"].as_u64().unwrap_or(0) > 0);
        let (_, _, b) = send(
            &app,
            "/ai/gemini/v1beta/models/text-embedding-004:embedContent",
            &json!({"content": {"parts": [{"text": "hi"}]}, "outputDimensionality": 32}),
            &[],
        )
        .await;
        assert_eq!(
            json(&b)["embedding"]["values"].as_array().map(Vec::len),
            Some(32)
        );
        let (_, _, b) = send(&app, "/ai/gemini/v1beta/models/text-embedding-004:batchEmbedContents", &json!({"requests": [{"content": {"parts": [{"text": "a"}]}}, {"content": {"parts": [{"text": "b"}]}}]}), &[]).await;
        assert_eq!(json(&b)["embeddings"].as_array().map(Vec::len), Some(2));
        let (s, _, b) = fetch(&app, "/ai/gemini/v1beta/models").await;
        assert_eq!(s, 200);
        assert!(json(&b)["models"].is_array());
        let (_, _, b) = fetch(&app, BASE).await;
        assert_eq!(json(&b)["name"], "models/gemini-2.5-flash");
    }

    #[tokio::test]
    async fn native_errors() {
        let app = app();
        let (s, _, b) = send(
            &app,
            &format!("{BASE}:generateContent"),
            &serde_json::from_str(EX).expect("json"),
            &[("x-rustybin-require-auth", "1")],
        )
        .await;
        assert_eq!(s, 403);
        assert_eq!(json(&b)["error"]["status"], "PERMISSION_DENIED");
        let (s, _, b) = send(
            &app,
            &format!("{BASE}:generateContent"),
            &json!({"nope": 1}),
            &[],
        )
        .await;
        assert_eq!(s, 400);
        assert_eq!(json(&b)["error"]["status"], "INVALID_ARGUMENT");
        let (_, _, b) = send(
            &app,
            &format!("{BASE}:generateContent"),
            &serde_json::from_str(EX).expect("json"),
            &[("x-rustybin-fail", "content_filter")],
        )
        .await;
        assert_eq!(json(&b)["candidates"][0]["finishReason"], "SAFETY");
    }

    #[test]
    fn split_model_action() {
        assert_eq!(
            split_rest("gemini-2.0-flash:generateContent"),
            ("gemini-2.0-flash".into(), Some("generateContent".into()))
        );
        assert_eq!(
            split_rest("gemini-2.0-flash"),
            ("gemini-2.0-flash".into(), None)
        );
    }
}
