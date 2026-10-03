//! AWS Bedrock Runtime under `/ai/bedrock`:
//! - `POST /model/{modelId}/converse`: Converse API;
//! - `POST /model/{modelId}/converse-stream`: ConverseStream as an AWS event
//!   stream (`application/vnd.amazon.eventstream`: messageStart,
//!   contentBlockStart, contentBlockDelta, contentBlockStop, messageStop,
//!   metadata);
//! - `POST /model/{modelId}/invoke`: InvokeModel with the model family's
//!   native body (Anthropic `anthropic_version` Messages body, Titan
//!   `inputText` text or embeddings, Llama-style `prompt`);
//! - `POST /model/{modelId}/invoke-with-response-stream`: event stream of
//!   `chunk` events whose `bytes` are base64 JSON (Anthropic stream events).
//!
//! Credentials: SigV4 `Authorization` header (structural check only) or a
//! Bedrock API key as `Authorization: Bearer`, enforced only when required.

use axum::body::Bytes;
use axum::extract::Path;
use axum::http::HeaderValue;
use axum::response::Response;
use axum::routing::post;
use axum::{Extension, Router};
use base64::Engine;
use serde_json::{json, Value};
use std::convert::Infallible;

use super::anthropic;
use super::engine::{ChatInput, Finish, Msg, Part, Reply, Role, Tool, ToolCall, ToolChoice};
use super::eventstream;
use super::faults::ErrorKind;
use super::{embed, json_response, rand_id, stream_response, AiCtx, Served};
use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

pub const EVENTSTREAM: &str = "application/vnd.amazon.eventstream";

fn parse_blocks(content: Option<&Value>) -> Vec<Part> {
    let mut out = Vec::new();
    for b in content.and_then(Value::as_array).into_iter().flatten() {
        if let Some(t) = b.get("text").and_then(Value::as_str) {
            out.push(Part::Text(t.to_string()));
        } else if let Some(i) = b.get("image") {
            out.push(Part::Image(format!(
                "image/{} (bytes)",
                i.get("format").and_then(Value::as_str).unwrap_or("png")
            )));
        } else if let Some(d) = b.get("document") {
            out.push(Part::File(
                d.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("document")
                    .to_string(),
            ));
        } else if let Some(tu) = b.get("toolUse") {
            out.push(Part::ToolCall(ToolCall {
                id: tu
                    .get("toolUseId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                name: tu
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                arguments: tu.get("input").cloned().unwrap_or(json!({})),
            }));
        } else if let Some(tr) = b.get("toolResult") {
            let content: Vec<String> = tr
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(
                    |c| match (c.get("text").and_then(Value::as_str), c.get("json")) {
                        (Some(t), _) => t.to_string(),
                        (None, Some(j)) => j.to_string(),
                        _ => String::new(),
                    },
                )
                .collect();
            out.push(Part::ToolResult {
                id: tr
                    .get("toolUseId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                name: None,
                content: content.join("\n"),
                is_error: tr.get("status").and_then(Value::as_str) == Some("error"),
            });
        }
    }
    out
}

/// Parse a Converse request.
pub fn parse_converse(body: &Value, model: &str) -> Result<ChatInput, String> {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or("1 validation error detected: Value null at 'messages' failed to satisfy constraint: Member must not be null")?;
    let ic = body.get("inferenceConfig").cloned().unwrap_or(json!({}));
    let mut input = ChatInput {
        model: model.to_string(),
        max_tokens: ic
            .get("maxTokens")
            .and_then(Value::as_u64)
            .map(|n| n.min(u64::from(u32::MAX)) as u32),
        stop: ic
            .get("stopSequences")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .take(4)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        ..Default::default()
    };
    for s in body
        .get("system")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(t) = s.get("text").and_then(Value::as_str) {
            input.system.push(t.to_string());
        }
    }
    for (i, m) in messages.iter().enumerate() {
        let role = match m.get("role").and_then(Value::as_str) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => return Err(format!("1 validation error detected: Value at 'messages.{}.member.role' failed to satisfy constraint: Member must satisfy enum value set: [user, assistant]", i + 1)),
        };
        input.messages.push(Msg {
            role,
            parts: parse_blocks(m.get("content")),
        });
    }
    if let Some(tc) = body.get("toolConfig") {
        for t in tc
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(spec) = t.get("toolSpec") else {
                continue;
            };
            let Some(name) = spec.get("name").and_then(Value::as_str) else {
                continue;
            };
            input.tools.push(Tool {
                name: name.to_string(),
                description: spec
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                parameters: spec
                    .get("inputSchema")
                    .and_then(|s| s.get("json"))
                    .cloned()
                    .unwrap_or(json!({"type": "object"})),
            });
        }
        input.tool_choice = match tc.get("toolChoice") {
            Some(c) if c.get("any").is_some() => ToolChoice::Required,
            Some(c) if c.get("tool").is_some() => ToolChoice::Named(
                c["tool"]
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
            _ => ToolChoice::Auto,
        };
    }
    Ok(input)
}

fn stop_reason(f: &Finish) -> &'static str {
    match f {
        Finish::Stop => "end_turn",
        Finish::StopSequence(_) => "stop_sequence",
        Finish::Length => "max_tokens",
        Finish::ToolCalls => "tool_use",
        Finish::ContentFilter => "content_filtered",
    }
}

fn tool_use_id(c: &ToolCall) -> String {
    format!("tooluse_{}", c.id)
}

fn converse_json(r: &Reply, latency_ms: u64) -> Value {
    let mut content = Vec::new();
    if !r.text.is_empty() || (r.tool_calls.is_empty() && r.finish != Finish::ContentFilter) {
        content.push(json!({"text": r.text}));
    }
    for c in &r.tool_calls {
        content.push(
            json!({"toolUse": {"toolUseId": tool_use_id(c), "name": c.name, "input": c.arguments}}),
        );
    }
    json!({
        "output": {"message": {"role": "assistant", "content": content}},
        "stopReason": stop_reason(&r.finish),
        "usage": {"inputTokens": r.prompt_tokens, "outputTokens": r.completion_tokens, "totalTokens": r.prompt_tokens + r.completion_tokens},
        "metrics": {"latencyMs": latency_ms},
    })
}

/// The ConverseStream events of a reply.
pub fn converse_events(r: &Reply) -> Vec<(&'static str, Value)> {
    let mut ev = vec![("messageStart", json!({"role": "assistant"}))];
    let mut index = 0;
    if !r.text.is_empty() || (r.tool_calls.is_empty() && r.finish != Finish::ContentFilter) {
        for p in r.pieces() {
            ev.push((
                "contentBlockDelta",
                json!({"contentBlockIndex": index, "delta": {"text": p}}),
            ));
        }
        ev.push(("contentBlockStop", json!({"contentBlockIndex": index})));
        index += 1;
    }
    for c in &r.tool_calls {
        ev.push(("contentBlockStart", json!({"contentBlockIndex": index, "start": {"toolUse": {"toolUseId": tool_use_id(c), "name": c.name}}})));
        for p in super::tokens::pieces(&c.arguments.to_string()) {
            ev.push((
                "contentBlockDelta",
                json!({"contentBlockIndex": index, "delta": {"toolUse": {"input": p}}}),
            ));
        }
        ev.push(("contentBlockStop", json!({"contentBlockIndex": index})));
        index += 1;
    }
    ev.push(("messageStop", json!({"stopReason": stop_reason(&r.finish)})));
    ev.push(("metadata", json!({
        "usage": {"inputTokens": r.prompt_tokens, "outputTokens": r.completion_tokens, "totalTokens": r.prompt_tokens + r.completion_tokens},
        "metrics": {"latencyMs": 42}
    })));
    ev
}

fn token_headers(resp: &mut Response, input: u32, output: u32) {
    let h = resp.headers_mut();
    h.insert("x-amzn-bedrock-input-token-count", HeaderValue::from(input));
    h.insert(
        "x-amzn-bedrock-output-token-count",
        HeaderValue::from(output),
    );
    h.insert(
        "x-amzn-bedrock-invocation-latency",
        HeaderValue::from(42u32),
    );
}

#[allow(clippy::result_large_err)]
fn parse_body(ctx: &AiCtx, raw: &[u8]) -> Result<Value, Response> {
    serde_json::from_slice(raw).map_err(|_| {
        ctx.error(
            ErrorKind::BadRequest,
            "Malformed input request, please reformat your input and try again.",
        )
    })
}

async fn converse_impl(ctx: AiCtx, model: String, raw: Bytes, stream: bool) -> Response {
    let body = match parse_body(&ctx, &raw) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let input = match parse_converse(&body, &model) {
        Ok(i) => i,
        Err(m) => return ctx.error(ErrorKind::BadRequest, m),
    };
    let reply = ctx.generate(&input, 0);
    ctx.record_chat(&raw, &input, &reply, stream, stop_reason(&reply.finish));
    let served = Served {
        model,
        mode: Some(reply.mode),
        tokens: reply.prompt_tokens + reply.completion_tokens,
    };
    if !stream {
        ctx.wait_ttft().await;
        return json_response(converse_json(&reply, 42), served);
    }
    let events = converse_events(&reply);
    let deltas = events
        .iter()
        .filter(|(n, _)| *n == "contentBlockDelta")
        .count();
    let pace = ctx.pace;
    let s = async_stream::stream! {
        let delay = pace.per_piece(deltas);
        let mut first = true;
        for (name, data) in events {
            if name == "contentBlockDelta" {
                if first {
                    first = false;
                    if !pace.ttft.is_zero() {
                        tokio::time::sleep(pace.ttft).await;
                    }
                } else if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
            }
            yield Ok::<_, Infallible>(Bytes::from(eventstream::event(name, &data)));
        }
    };
    stream_response(EVENTSTREAM, s, served)
}

async fn converse(
    Extension(ctx): Extension<AiCtx>,
    Path(model): Path<String>,
    raw: Bytes,
) -> Response {
    converse_impl(ctx, model, raw, false).await
}

async fn converse_stream(
    Extension(ctx): Extension<AiCtx>,
    Path(model): Path<String>,
    raw: Bytes,
) -> Response {
    converse_impl(ctx, model, raw, true).await
}

/// InvokeModel body families.
enum Invoke {
    Anthropic(anthropic::MessagesReq),
    TitanText { input: ChatInput },
    TitanEmbed { text: String, dims: usize },
    Prompt { input: ChatInput },
}

fn parse_invoke(ctx: &AiCtx, body: &Value, model: &str) -> Result<Invoke, String> {
    if body.get("anthropic_version").is_some() || body.get("messages").is_some() {
        let mut req = anthropic::parse(body, true)?;
        req.input.model = model.to_string();
        return Ok(Invoke::Anthropic(req));
    }
    if let Some(text) = body.get("inputText").and_then(Value::as_str) {
        if model.contains("embed") {
            let dims = body
                .get("dimensions")
                .and_then(Value::as_u64)
                .unwrap_or(1024) as usize;
            if dims == 0 || dims > ctx.shared.max_dims() {
                return Err("dimensions out of range".into());
            }
            return Ok(Invoke::TitanEmbed {
                text: text.to_string(),
                dims,
            });
        }
        let cfg = body
            .get("textGenerationConfig")
            .cloned()
            .unwrap_or(json!({}));
        return Ok(Invoke::TitanText {
            input: ChatInput {
                model: model.to_string(),
                messages: vec![Msg::text(Role::User, text)],
                max_tokens: cfg
                    .get("maxTokenCount")
                    .and_then(Value::as_u64)
                    .map(|n| n.min(100_000) as u32),
                ..Default::default()
            },
        });
    }
    if let Some(prompt) = body.get("prompt").and_then(Value::as_str) {
        return Ok(Invoke::Prompt {
            input: ChatInput {
                model: model.to_string(),
                messages: vec![Msg::text(Role::User, prompt)],
                max_tokens: body
                    .get("max_gen_len")
                    .or_else(|| body.get("max_tokens"))
                    .and_then(Value::as_u64)
                    .map(|n| n.min(100_000) as u32),
                ..Default::default()
            },
        });
    }
    Err("Malformed input request: expected an Anthropic Messages body (anthropic_version), Titan inputText or a prompt.".into())
}

async fn invoke_impl(ctx: AiCtx, model: String, raw: Bytes, stream: bool) -> Response {
    let body = match parse_body(&ctx, &raw) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let inv = match parse_invoke(&ctx, &body, &model) {
        Ok(i) => i,
        Err(m) => return ctx.error(ErrorKind::BadRequest, m),
    };
    let pace = ctx.pace;
    let (value, events, input_t, output_t, mode) = match inv {
        Invoke::Anthropic(req) => {
            let reply = ctx.generate(&req.input, 0);
            ctx.record_chat(
                &raw,
                &req.input,
                &reply,
                stream,
                anthropic::stop_reason(&reply.finish).0,
            );
            let cu = anthropic::cache_usage(&ctx, &req, reply.prompt_tokens);
            let id = format!("msg_bdrk_{}", rand_id(24));
            let v = anthropic::message_json(&id, &model, &reply, cu);
            let ev = anthropic::stream_events(&id, &model, &reply, cu);
            (
                v,
                Some(ev),
                reply.prompt_tokens,
                reply.completion_tokens,
                Some(reply.mode),
            )
        }
        Invoke::TitanText { input } => {
            let r = ctx.generate(&input, 0);
            ctx.record_chat(&raw, &input, &r, stream, "FINISH");
            let reason = if r.finish == Finish::Length {
                "LENGTH"
            } else {
                "FINISH"
            };
            let v = json!({"inputTextTokenCount": r.prompt_tokens, "results": [{"tokenCount": r.completion_tokens, "outputText": r.text, "completionReason": reason}]});
            (v, None, r.prompt_tokens, r.completion_tokens, Some(r.mode))
        }
        Invoke::Prompt { input } => {
            let r = ctx.generate(&input, 0);
            ctx.record_chat(&raw, &input, &r, stream, "stop");
            let reason = if r.finish == Finish::Length {
                "length"
            } else {
                "stop"
            };
            let v = json!({"generation": r.text, "prompt_token_count": r.prompt_tokens, "generation_token_count": r.completion_tokens, "stop_reason": reason});
            (v, None, r.prompt_tokens, r.completion_tokens, Some(r.mode))
        }
        Invoke::TitanEmbed { text, dims } => {
            let t = super::tokens::count(&text);
            ctx.record(super::RecordArgs {
                body: &raw,
                model: &model,
                mode: "embedding",
                stream: false,
                prompt: &text,
                prompt_tokens: t,
                completion_tokens: 0,
                finish: "stop",
                reply: "",
            });
            (
                json!({"embedding": embed::embed(&text, dims), "inputTextTokenCount": t}),
                None,
                t,
                0,
                None,
            )
        }
    };
    let served = Served {
        model,
        mode,
        tokens: input_t + output_t,
    };
    if !stream {
        ctx.wait_ttft().await;
        let mut resp = json_response(value, served);
        token_headers(&mut resp, input_t, output_t);
        return resp;
    }
    let mut events = events.unwrap_or_else(|| vec![("chunk", value)]);
    if let Some(last) = events.last_mut() {
        last.1["amazon-bedrock-invocationMetrics"] = json!({"inputTokenCount": input_t, "outputTokenCount": output_t, "invocationLatency": 42, "firstByteLatency": 10});
    }
    let deltas = events.len();
    let s = async_stream::stream! {
        let delay = pace.per_piece(deltas);
        if !pace.ttft.is_zero() {
            tokio::time::sleep(pace.ttft).await;
        }
        for (_, data) in events {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let b64 = base64::engine::general_purpose::STANDARD.encode(data.to_string());
            yield Ok::<_, Infallible>(Bytes::from(eventstream::event("chunk", &json!({"bytes": b64}))));
        }
    };
    let mut resp = stream_response(EVENTSTREAM, s, served);
    resp.headers_mut().insert(
        "x-amzn-bedrock-content-type",
        HeaderValue::from_static("application/json"),
    );
    resp
}

async fn invoke(
    Extension(ctx): Extension<AiCtx>,
    Path(model): Path<String>,
    raw: Bytes,
) -> Response {
    invoke_impl(ctx, model, raw, false).await
}

async fn invoke_stream(
    Extension(ctx): Extension<AiCtx>,
    Path(model): Path<String>,
    raw: Bytes,
) -> Response {
    invoke_impl(ctx, model, raw, true).await
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ai/bedrock/model/{model_id}/converse", post(converse))
        .route(
            "/ai/bedrock/model/{model_id}/converse-stream",
            post(converse_stream),
        )
        .route("/ai/bedrock/model/{model_id}/invoke", post(invoke))
        .route(
            "/ai/bedrock/model/{model_id}/invoke-with-response-stream",
            post(invoke_stream),
        )
}

const EX: &str = r#"{"messages":[{"role":"user","content":[{"text":"hello"}]}],"inferenceConfig":{"maxTokens":256}}"#;
const EX_TOOL: &str = r#"{"messages":[{"role":"user","content":[{"text":"What is the weather in Paris?"}]}],"toolConfig":{"tools":[{"toolSpec":{"name":"get_weather","description":"Get the current weather for a location","inputSchema":{"json":{"type":"object","properties":{"location":{"type":"string"}},"required":["location"]}}}}]}}"#;
const EX_INVOKE: &str = r#"{"anthropic_version":"bedrock-2023-05-31","max_tokens":256,"messages":[{"role":"user","content":"hello"}]}"#;

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new("/ai/bedrock/model/{model_id}/converse", &["POST"], category::AI_MOCK, "Bedrock Converse")
            .description("Credential: SigV4 Authorization (structural check) or Bedrock API key bearer, enforced with X-Rustybin-Require-Auth. toolConfig produces toolUse blocks.")
            .example(Example::post("Converse", "/ai/bedrock/model/anthropic.claude-3-5-sonnet-20240620-v1:0/converse").json(EX))
            .example(Example::post("Converse with tools", "/ai/bedrock/model/anthropic.claude-3-5-sonnet-20240620-v1:0/converse").json(EX_TOOL)),
        Endpoint::new("/ai/bedrock/model/{model_id}/converse-stream", &["POST"], category::AI_MOCK, "Bedrock ConverseStream (binary AWS event stream with CRC32 framing)")
            .example(Example::post("ConverseStream", "/ai/bedrock/model/amazon.nova-pro-v1:0/converse-stream").json(EX)),
        Endpoint::new("/ai/bedrock/model/{model_id}/invoke", &["POST"], category::AI_MOCK, "Bedrock InvokeModel (Anthropic body, Titan text/embeddings, Llama prompt)")
            .example(Example::post("InvokeModel (Anthropic)", "/ai/bedrock/model/anthropic.claude-3-haiku-20240307-v1:0/invoke").json(EX_INVOKE))
            .example(Example::post("InvokeModel (Titan embeddings)", "/ai/bedrock/model/amazon.titan-embed-text-v2:0/invoke").json(r#"{"inputText":"Hello world","dimensions":256}"#)),
        Endpoint::new("/ai/bedrock/model/{model_id}/invoke-with-response-stream", &["POST"], category::AI_MOCK, "Bedrock InvokeModelWithResponseStream (event stream of base64 chunks)")
            .example(Example::post("InvokeModelWithResponseStream", "/ai/bedrock/model/anthropic.claude-3-haiku-20240307-v1:0/invoke-with-response-stream").json(EX_INVOKE)),
    ]
}

pub fn openapi_paths() -> Value {
    let op = |id: &str, summary: &str, stream: bool| {
        let mut params = vec![
            json!({"name": "model_id", "in": "path", "required": true, "schema": {"type": "string"}, "example": "anthropic.claude-3-5-sonnet-20240620-v1:0"}),
            json!({"name": "Authorization", "in": "header", "schema": {"type": "string"}, "description": "AWS4-HMAC-SHA256 Credential=AKID/yyyymmdd/region/bedrock/aws4_request, SignedHeaders=..., Signature=..."}),
            json!({"name": "X-Amz-Date", "in": "header", "schema": {"type": "string"}}),
        ];
        if let Value::Array(c) = super::common_parameters() {
            params.extend(c);
        }
        let content = if stream {
            json!({"application/vnd.amazon.eventstream": {"schema": {"type": "string", "format": "binary"}}})
        } else {
            json!({"application/json": {"schema": {"type": "object"}}})
        };
        json!({"post": {
            "tags": ["AI Gateway"],
            "summary": summary,
            "operationId": id,
            "parameters": params,
            "requestBody": {"required": true, "content": {"application/json": {"schema": {"type": "object", "properties": {
                "messages": {"type": "array", "items": {"type": "object"}},
                "system": {"type": "array", "items": {"type": "object"}},
                "inferenceConfig": {"type": "object"},
                "toolConfig": {"type": "object"}
            }}}}},
            "responses": {
                "200": {"description": "OK", "content": content},
                "400": {"description": "ValidationException"},
                "403": {"description": "MissingAuthenticationTokenException / IncompleteSignatureException (when required)"},
                "429": {"description": "ThrottlingException (simulated)"}
            }
        }})
    };
    json!({
        "/ai/bedrock/model/{model_id}/converse": op("bedrockConverse", "Bedrock Converse", false),
        "/ai/bedrock/model/{model_id}/converse-stream": op("bedrockConverseStream", "Bedrock ConverseStream", true),
        "/ai/bedrock/model/{model_id}/invoke": op("bedrockInvoke", "Bedrock InvokeModel", false),
        "/ai/bedrock/model/{model_id}/invoke-with-response-stream": op("bedrockInvokeStream", "Bedrock InvokeModelWithResponseStream", true),
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    const BASE: &str = "/ai/bedrock/model/anthropic.claude-3-5-sonnet-20240620-v1:0";

    #[tokio::test]
    async fn converse_and_tools() {
        let app = app();
        let (s, h, b) = send(
            &app,
            &format!("{BASE}/converse"),
            &serde_json::from_str(EX).expect("json"),
            &[],
        )
        .await;
        assert_eq!(s, 200);
        assert!(h.contains_key("x-amzn-requestid"));
        let v = json(&b);
        assert_eq!(v["output"]["message"]["role"], "assistant");
        assert_eq!(v["stopReason"], "end_turn");
        assert!(v["usage"]["totalTokens"].as_u64().unwrap_or(0) > 0);
        let (_, _, b) = send(
            &app,
            &format!("{BASE}/converse"),
            &serde_json::from_str(EX_TOOL).expect("json"),
            &[],
        )
        .await;
        let v = json(&b);
        assert_eq!(v["stopReason"], "tool_use");
        let tu = v["output"]["message"]["content"][0]["toolUse"].clone();
        assert_eq!(tu["input"]["location"], "Paris");
        let follow = json!({"messages": [
            {"role": "user", "content": [{"text": "What is the weather in Paris?"}]},
            {"role": "assistant", "content": [{"toolUse": tu}]},
            {"role": "user", "content": [{"toolResult": {"toolUseId": tu["toolUseId"], "content": [{"json": {"temp": 20}}]}}]}
        ]});
        let (_, _, b) = send(&app, &format!("{BASE}/converse"), &follow, &[]).await;
        assert!(json(&b)["output"]["message"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("get_weather returned"));
    }

    #[tokio::test]
    async fn converse_stream_decodes() {
        let app = app();
        let (s, h, b) = send(
            &app,
            &format!("{BASE}/converse-stream"),
            &serde_json::from_str(EX_TOOL).expect("json"),
            &[],
        )
        .await;
        assert_eq!(s, 200);
        assert_eq!(h["content-type"], EVENTSTREAM);
        let msgs = eventstream::decode_all(&b).expect("valid event stream");
        let types: Vec<&str> = msgs
            .iter()
            .filter_map(|m| m.header(":event-type"))
            .collect();
        assert_eq!(types.first(), Some(&"messageStart"));
        assert!(types.contains(&"contentBlockStart"));
        assert_eq!(&types[types.len() - 2..], &["messageStop", "metadata"]);
        let input: String = msgs
            .iter()
            .filter(|m| m.header(":event-type") == Some("contentBlockDelta"))
            .filter_map(|m| serde_json::from_slice::<Value>(&m.payload).ok())
            .filter_map(|v| v["delta"]["toolUse"]["input"].as_str().map(String::from))
            .collect();
        assert_eq!(
            serde_json::from_str::<Value>(&input).expect("json")["location"],
            "Paris"
        );

        let (_, _, b) = send(
            &app,
            &format!("{BASE}/converse-stream"),
            &serde_json::from_str(EX).expect("json"),
            &[],
        )
        .await;
        let msgs = eventstream::decode_all(&b).expect("valid event stream");
        let text: String = msgs
            .iter()
            .filter_map(|m| serde_json::from_slice::<Value>(&m.payload).ok())
            .filter_map(|v| v["delta"]["text"].as_str().map(String::from))
            .collect();
        assert_eq!(text, super::super::engine::GREETING);
    }

    #[tokio::test]
    async fn invoke_families_and_stream() {
        let app = app();
        let (s, h, b) = send(
            &app,
            &format!("{BASE}/invoke"),
            &serde_json::from_str(EX_INVOKE).expect("json"),
            &[],
        )
        .await;
        assert_eq!(s, 200);
        assert!(h.contains_key("x-amzn-bedrock-input-token-count"));
        let v = json(&b);
        assert_eq!(v["type"], "message");
        assert_eq!(v["model"], "anthropic.claude-3-5-sonnet-20240620-v1:0");
        let (_, _, b) = send(
            &app,
            "/ai/bedrock/model/amazon.titan-embed-text-v2:0/invoke",
            &json!({"inputText": "hi", "dimensions": 256}),
            &[],
        )
        .await;
        assert_eq!(json(&b)["embedding"].as_array().map(Vec::len), Some(256));
        let (_, _, b) = send(
            &app,
            "/ai/bedrock/model/amazon.titan-text-express-v1/invoke",
            &json!({"inputText": "hello"}),
            &[],
        )
        .await;
        assert_eq!(json(&b)["results"][0]["completionReason"], "FINISH");
        let (_, _, b) = send(
            &app,
            "/ai/bedrock/model/meta.llama3-8b-instruct-v1:0/invoke",
            &json!({"prompt": "hello", "max_gen_len": 3}),
            &[],
        )
        .await;
        assert_eq!(json(&b)["stop_reason"], "length");
        let (s, h, _) = send(&app, &format!("{BASE}/invoke"), &json!({"foo": 1}), &[]).await;
        assert_eq!(s, 400);
        assert_eq!(h["x-amzn-errortype"], "ValidationException");

        let (_, _, b) = send(
            &app,
            &format!("{BASE}/invoke-with-response-stream"),
            &serde_json::from_str(EX_INVOKE).expect("json"),
            &[],
        )
        .await;
        let msgs = eventstream::decode_all(&b).expect("valid");
        let events: Vec<Value> = msgs
            .iter()
            .filter_map(|m| serde_json::from_slice::<Value>(&m.payload).ok())
            .filter_map(|v| {
                v["bytes"]
                    .as_str()
                    .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
            })
            .filter_map(|b| serde_json::from_slice::<Value>(&b).ok())
            .collect();
        assert_eq!(
            events.first().map(|e| e["type"].clone()),
            Some(json!("message_start"))
        );
        assert!(events
            .last()
            .map(|e| e["amazon-bedrock-invocationMetrics"].is_object())
            .unwrap_or(false));
    }

    #[tokio::test]
    async fn sigv4_auth() {
        let app = app();
        let body: Value = serde_json::from_str(EX).expect("json");
        let (s, h, _) = send(
            &app,
            &format!("{BASE}/converse"),
            &body,
            &[("x-rustybin-require-auth", "true")],
        )
        .await;
        assert_eq!(s, 403);
        assert_eq!(h["x-amzn-errortype"], "MissingAuthenticationTokenException");
        let sig = "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/us-east-1/bedrock/aws4_request, SignedHeaders=host;x-amz-date, Signature=fe5f80f77d5fa3beca038a248ff027d0445342fe2855ddc963176630326f1024";
        let (s, h, _) = send(
            &app,
            &format!("{BASE}/converse"),
            &body,
            &[
                ("x-rustybin-require-auth", "true"),
                ("authorization", sig),
                ("x-amz-date", "20260101T000000Z"),
            ],
        )
        .await;
        assert_eq!(s, 200);
        assert_eq!(h["x-rustybin-credential"], "sigv4 ****MPLE");
        let (s, h, _) = send(
            &app,
            &format!("{BASE}/converse"),
            &body,
            &[("x-rustybin-fail", "429")],
        )
        .await;
        assert_eq!(s, 429);
        assert_eq!(h["x-amzn-errortype"], "ThrottlingException");
    }
}
