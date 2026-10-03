//! OpenAI-compatible API under `/ai/openai/v1` (and the legacy `/ai/v1`
//! aliases): chat completions, completions, embeddings, models,
//! moderations, image generation and audio transcription.

use axum::body::Bytes;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::{json, Value};
use std::convert::Infallible;

use super::engine::{
    self, ChatInput, Finish, Format, Msg, Part, Reply, Role, Tool, ToolCall, ToolChoice,
};
use super::faults::ErrorKind;
use super::{embed, json_response, now_secs, rand_id, stream_response, AiCtx, Served};
use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

pub const DEFAULT_MODEL: &str = "rustybin-gpt";
pub const DEFAULT_EMBED_MODEL: &str = "rustybin-embed";

// ── Request parsing (shared with Azure and the Responses API) ───────

fn describe_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("data:") {
        let media = rest.split([';', ',']).next().unwrap_or("");
        format!("data:{media} ({} bytes)", url.len())
    } else {
        url.chars().take(200).collect()
    }
}

/// Content: a string, null, one part object, or an array of parts
/// (`text`, `image_url`, `input_audio`, `file`, `refusal`, and the
/// Responses API `input_text` / `input_image` / `input_file` / `output_text`).
pub fn parse_content(v: &Value) -> Vec<Part> {
    match v {
        Value::String(s) => vec![Part::Text(s.clone())],
        Value::Array(parts) => parts.iter().filter_map(parse_part).collect(),
        Value::Object(_) => parse_part(v).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn parse_part(p: &Value) -> Option<Part> {
    if let Value::String(s) = p {
        return Some(Part::Text(s.clone()));
    }
    let ty = p.get("type").and_then(Value::as_str).unwrap_or("text");
    let s = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or("").to_string();
    match ty {
        "text" | "input_text" | "output_text" => Some(Part::Text(s(p.get("text")))),
        "refusal" => Some(Part::Text(s(p.get("refusal")))),
        "image_url" => {
            let url = match p.get("image_url") {
                Some(Value::String(u)) => u.clone(),
                Some(o) => s(o.get("url")),
                None => String::new(),
            };
            Some(Part::Image(describe_url(&url)))
        }
        "input_image" => {
            let url = p
                .get("image_url")
                .and_then(Value::as_str)
                .or_else(|| p.get("file_id").and_then(Value::as_str))
                .unwrap_or("");
            Some(Part::Image(describe_url(url)))
        }
        "input_audio" => Some(Part::Audio(s(p
            .get("input_audio")
            .and_then(|a| a.get("format"))))),
        "file" | "input_file" => {
            let f = p.get("file").unwrap_or(p);
            let name = f
                .get("filename")
                .and_then(Value::as_str)
                .or_else(|| f.get("file_id").and_then(Value::as_str))
                .unwrap_or("file");
            Some(Part::File(name.to_string()))
        }
        _ => p
            .get("text")
            .and_then(Value::as_str)
            .map(|t| Part::Text(t.into())),
    }
}

/// Arguments arrive as a JSON string; keep invalid JSON as a string.
pub fn parse_arguments(v: Option<&Value>) -> Value {
    match v {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::String(s.clone())),
        Some(other) => other.clone(),
        None => json!({}),
    }
}

/// `tools` (`{"type":"function","function":{...}}` or the flat Responses
/// shape `{"type":"function","name":...}`) plus legacy `functions`.
pub fn parse_tools(body: &Value) -> Vec<Tool> {
    let mut out = Vec::new();
    let defs = body
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|t| t.get("type").and_then(Value::as_str).unwrap_or("function") == "function")
        .map(|t| t.get("function").unwrap_or(t))
        .chain(
            body.get("functions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        );
    for f in defs {
        let Some(name) = f.get("name").and_then(Value::as_str) else {
            continue;
        };
        out.push(Tool {
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
    out
}

/// `tool_choice` (or legacy `function_call`).
pub fn parse_tool_choice(body: &Value) -> ToolChoice {
    let v = body
        .get("tool_choice")
        .or_else(|| body.get("function_call"));
    match v {
        Some(Value::String(s)) => match s.as_str() {
            "none" => ToolChoice::None,
            "required" | "any" => ToolChoice::Required,
            _ => ToolChoice::Auto,
        },
        Some(Value::Object(o)) => {
            let name = o
                .get("function")
                .and_then(|f| f.get("name"))
                .or_else(|| o.get("name"))
                .and_then(Value::as_str);
            match name {
                Some(n) => ToolChoice::Named(n.to_string()),
                None => match o.get("type").and_then(Value::as_str) {
                    Some("none") => ToolChoice::None,
                    Some("required") | Some("any") => ToolChoice::Required,
                    _ => ToolChoice::Auto,
                },
            }
        }
        _ => ToolChoice::Auto,
    }
}

/// `response_format` (chat) or `text.format` (Responses API).
pub fn parse_format(f: Option<&Value>) -> Format {
    let Some(f) = f else {
        return Format::Text;
    };
    match f.get("type").and_then(Value::as_str) {
        Some("json_object") => Format::JsonObject,
        Some("json_schema") => {
            let schema = f
                .get("json_schema")
                .and_then(|j| j.get("schema"))
                .or_else(|| f.get("schema"))
                .cloned()
                .unwrap_or(json!({"type": "object"}));
            Format::JsonSchema(schema)
        }
        _ => Format::Text,
    }
}

/// `stop`: a string or an array of strings.
pub fn parse_stop(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .take(4)
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    }
}

fn as_u32(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_u64)
        .map(|n| n.min(u64::from(u32::MAX)) as u32)
}

/// A parsed chat completions request.
pub struct ChatReq {
    pub input: ChatInput,
    pub stream: bool,
    pub include_usage: bool,
    pub n: u32,
}

/// Parse a chat completions body.
pub fn parse_chat(body: &Value, max_n: u32) -> Result<ChatReq, String> {
    if !body.is_object() {
        return Err("The request body must be a JSON object.".into());
    }
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or("Missing required parameter: 'messages'.")?;
    if messages.is_empty() {
        return Err("Invalid 'messages': empty array. Expected an array with minimum length 1, but got an empty array instead.".into());
    }
    let mut input = ChatInput {
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_MODEL)
            .to_string(),
        tools: parse_tools(body),
        tool_choice: parse_tool_choice(body),
        format: parse_format(body.get("response_format")),
        max_tokens: as_u32(body.get("max_completion_tokens")).or(as_u32(body.get("max_tokens"))),
        stop: parse_stop(body.get("stop")),
        ..Default::default()
    };
    for (i, m) in messages.iter().enumerate() {
        let role = m
            .get("role")
            .and_then(Value::as_str)
            .ok_or(format!("Missing required parameter: 'messages[{i}].role'."))?;
        let content = m.get("content").unwrap_or(&Value::Null);
        match role {
            "system" | "developer" => {
                let text: Vec<String> = parse_content(content)
                    .into_iter()
                    .filter_map(|p| match p {
                        Part::Text(t) => Some(t),
                        _ => None,
                    })
                    .collect();
                input.system.push(text.join("\n"));
            }
            "user" => input.messages.push(Msg { role: Role::User, parts: parse_content(content) }),
            "assistant" => {
                let mut parts = parse_content(content);
                for tc in m.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
                    let f = tc.get("function").unwrap_or(tc);
                    parts.push(Part::ToolCall(ToolCall {
                        id: tc.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                        name: f.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                        arguments: parse_arguments(f.get("arguments")),
                    }));
                }
                if let Some(fc) = m.get("function_call") {
                    parts.push(Part::ToolCall(ToolCall {
                        id: String::new(),
                        name: fc.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                        arguments: parse_arguments(fc.get("arguments")),
                    }));
                }
                input.messages.push(Msg { role: Role::Assistant, parts });
            }
            "tool" | "function" => {
                let text: Vec<String> = parse_content(content)
                    .into_iter()
                    .filter_map(|p| match p {
                        Part::Text(t) => Some(t),
                        _ => None,
                    })
                    .collect();
                input.messages.push(Msg {
                    role: Role::Tool,
                    parts: vec![Part::ToolResult {
                        id: m.get("tool_call_id").and_then(Value::as_str).unwrap_or("").to_string(),
                        name: m.get("name").and_then(Value::as_str).map(String::from),
                        content: text.join("\n"),
                        is_error: false,
                    }],
                });
            }
            other => {
                return Err(format!(
                    "Invalid value: '{other}'. Supported values are: 'system', 'assistant', 'user', 'function', 'tool', and 'developer'."
                ))
            }
        }
    }
    Ok(ChatReq {
        stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
        include_usage: body
            .get("stream_options")
            .and_then(|o| o.get("include_usage"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        n: as_u32(body.get("n")).unwrap_or(1).clamp(1, max_n),
        input,
    })
}

// ── Response rendering ──────────────────────────────────────────────

pub fn finish_reason(f: &Finish) -> &'static str {
    match f {
        Finish::Stop | Finish::StopSequence(_) => "stop",
        Finish::Length => "length",
        Finish::ToolCalls => "tool_calls",
        Finish::ContentFilter => "content_filter",
    }
}

pub fn tool_call_json(c: &ToolCall) -> Value {
    json!({
        "id": format!("call_{}", c.id),
        "type": "function",
        "function": {"name": c.name, "arguments": c.arguments.to_string()},
    })
}

fn message_json(r: &Reply) -> Value {
    let mut m = json!({
        "role": "assistant",
        "content": if r.text.is_empty() && (!r.tool_calls.is_empty() || r.finish == Finish::ContentFilter) { Value::Null } else { json!(r.text) },
        "refusal": null,
        "annotations": [],
    });
    if !r.tool_calls.is_empty() {
        m["tool_calls"] = r.tool_calls.iter().map(tool_call_json).collect();
    }
    m
}

/// Azure content filter annotations.
fn azure_filter(filtered: bool) -> Value {
    let cat = |f: bool| json!({"filtered": f, "severity": if f { "high" } else { "safe" }});
    json!({
        "hate": cat(filtered),
        "self_harm": cat(false),
        "sexual": cat(false),
        "violence": cat(false),
    })
}

pub fn usage_json(prompt: u32, completion: u32) -> Value {
    json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
        "prompt_tokens_details": {"cached_tokens": 0, "audio_tokens": 0},
        "completion_tokens_details": {
            "reasoning_tokens": 0,
            "audio_tokens": 0,
            "accepted_prediction_tokens": 0,
            "rejected_prediction_tokens": 0
        }
    })
}

fn total_usage(replies: &[Reply]) -> (u32, u32) {
    let prompt = replies.first().map(|r| r.prompt_tokens).unwrap_or(0);
    let completion = replies.iter().map(|r| r.completion_tokens).sum();
    (prompt, completion)
}

/// Non-streaming chat completion.
pub fn chat_json(id: &str, created: i64, model: &str, replies: &[Reply], azure: bool) -> Value {
    let choices: Vec<Value> = replies
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut c = json!({
                "index": i,
                "message": message_json(r),
                "logprobs": null,
                "finish_reason": finish_reason(&r.finish),
            });
            if azure {
                c["content_filter_results"] = azure_filter(r.finish == Finish::ContentFilter);
            }
            c
        })
        .collect();
    let (p, c) = total_usage(replies);
    let mut v = json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "system_fingerprint": "fp_rustybin",
        "service_tier": "default",
        "choices": choices,
        "usage": usage_json(p, c),
    });
    if azure {
        v["prompt_filter_results"] =
            json!([{"prompt_index": 0, "content_filter_results": azure_filter(false)}]);
    }
    v
}

/// SSE chunks of a streamed chat completion.
pub fn chat_stream(
    ctx: AiCtx,
    id: String,
    created: i64,
    model: String,
    replies: Vec<Reply>,
    include_usage: bool,
    azure: bool,
) -> impl futures_util::Stream<Item = Result<Bytes, Infallible>> {
    let pace = ctx.pace;
    async_stream::stream! {
        let chunk = |choices: Value, usage: Option<Value>| {
            let mut c = json!({
                "id": &id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": &model,
                "system_fingerprint": "fp_rustybin",
                "service_tier": "default",
                "choices": choices,
            });
            if include_usage {
                c["usage"] = usage.unwrap_or(Value::Null);
            }
            if azure {
                c["obfuscation"] = json!("");
            }
            Bytes::from(format!("data: {c}\n\n"))
        };
        if azure {
            let pf = json!({
                "id": "", "object": "", "created": 0, "model": "",
                "choices": [],
                "prompt_filter_results": [{"prompt_index": 0, "content_filter_results": azure_filter(false)}]
            });
            yield Ok(Bytes::from(format!("data: {pf}\n\n")));
        }
        if !pace.ttft.is_zero() {
            tokio::time::sleep(pace.ttft).await;
        }
        for (i, r) in replies.iter().enumerate() {
            yield Ok(chunk(json!([{"index": i, "delta": {"role": "assistant", "content": "", "refusal": null}, "logprobs": null, "finish_reason": null}]), None));
            let pieces = r.pieces();
            let arg_strings: Vec<String> = r.tool_calls.iter().map(|c| c.arguments.to_string()).collect();
            let arg_pieces: usize = arg_strings.iter().map(|a| super::tokens::pieces(a).len()).sum();
            let delay = pace.per_piece(pieces.len() + arg_pieces);
            for p in pieces {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                yield Ok(chunk(json!([{"index": i, "delta": {"content": p}, "logprobs": null, "finish_reason": null}]), None));
            }
            for (ti, (c, args)) in r.tool_calls.iter().zip(&arg_strings).enumerate() {
                yield Ok(chunk(json!([{"index": i, "delta": {"tool_calls": [{
                    "index": ti, "id": format!("call_{}", c.id), "type": "function",
                    "function": {"name": c.name, "arguments": ""}
                }]}, "logprobs": null, "finish_reason": null}]), None));
                for p in super::tokens::pieces(args) {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    yield Ok(chunk(json!([{"index": i, "delta": {"tool_calls": [{
                        "index": ti, "function": {"arguments": p}
                    }]}, "logprobs": null, "finish_reason": null}]), None));
                }
            }
            let mut last = json!({"index": i, "delta": {}, "logprobs": null, "finish_reason": finish_reason(&r.finish)});
            if azure {
                last["content_filter_results"] = azure_filter(r.finish == Finish::ContentFilter);
            }
            yield Ok(chunk(json!([last]), None));
        }
        if include_usage {
            let (p, c) = total_usage(&replies);
            yield Ok(chunk(json!([]), Some(usage_json(p, c))));
        }
        yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
    }
}

/// Shared chat handler body (OpenAI and Azure).
pub async fn chat_impl(ctx: AiCtx, body: Bytes, model_override: Option<String>) -> Response {
    let azure = ctx.provider == super::faults::Provider::Azure;
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return ctx.error(
                ErrorKind::BadRequest,
                format!("We could not parse the JSON body of your request. ({e})"),
            )
        }
    };
    let mut req = match parse_chat(&v, ctx.shared.max_choices()) {
        Ok(r) => r,
        Err(m) => return ctx.error(ErrorKind::BadRequest, m),
    };
    if let Some(m) = model_override {
        req.input.model = m;
    }
    let replies: Vec<Reply> = (0..req.n).map(|i| ctx.generate(&req.input, i)).collect();
    let id = format!("chatcmpl-{}", rand_id(29));
    let created = now_secs();
    let model = req.input.model.clone();
    let first = &replies[0];
    ctx.record_chat(
        &body,
        &req.input,
        first,
        req.stream,
        finish_reason(&first.finish),
    );
    let (p, c) = total_usage(&replies);
    let served = Served {
        model: model.clone(),
        mode: Some(first.mode),
        tokens: p + c,
    };
    if req.stream {
        let s = chat_stream(ctx, id, created, model, replies, req.include_usage, azure);
        return stream_response("text/event-stream; charset=utf-8", s, served);
    }
    ctx.wait_ttft().await;
    json_response(chat_json(&id, created, &model, &replies, azure), served)
}

async fn chat(Extension(ctx): Extension<AiCtx>, body: Bytes) -> Response {
    chat_impl(ctx, body, None).await
}

// ── Legacy completions ──────────────────────────────────────────────

fn prompt_texts(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) if a.iter().all(Value::is_string) => a
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect(),
        Some(Value::Array(a)) if a.iter().all(Value::is_number) => {
            vec![a
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(" ")]
        }
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_array)
            .map(|t| {
                t.iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect(),
        _ => vec!["<|endoftext|>".to_string()],
    }
}

pub async fn completions_impl(ctx: AiCtx, body: Bytes, model_override: Option<String>) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return ctx.error(
                ErrorKind::BadRequest,
                format!("We could not parse the JSON body of your request. ({e})"),
            )
        }
    };
    let model = model_override
        .or_else(|| v.get("model").and_then(Value::as_str).map(String::from))
        .unwrap_or_else(|| DEFAULT_MODEL.into());
    let prompts: Vec<String> = prompt_texts(v.get("prompt"))
        .into_iter()
        .take(ctx.shared.max_inputs().min(64))
        .collect();
    let n = as_u32(v.get("n"))
        .unwrap_or(1)
        .clamp(1, ctx.shared.max_choices());
    let echo = v.get("echo").and_then(Value::as_bool).unwrap_or(false);
    let suffix = v.get("suffix").and_then(Value::as_str).unwrap_or("");
    let stream = v.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let mut choices = Vec::new();
    let mut prompt_tokens = 0;
    let mut completion_tokens = 0;
    let mut first_input = None;
    for p in &prompts {
        let input = ChatInput {
            model: model.clone(),
            messages: vec![Msg::text(Role::User, p.clone())],
            max_tokens: as_u32(v.get("max_tokens")),
            stop: parse_stop(v.get("stop")),
            ..Default::default()
        };
        prompt_tokens += super::tokens::count(p).max(1);
        for i in 0..n {
            let r = ctx.generate(&input, i);
            completion_tokens += r.completion_tokens;
            let text = if echo {
                format!("{p}{}{suffix}", r.text)
            } else {
                format!("{}{suffix}", r.text)
            };
            choices.push((text, r.finish.clone(), r.mode));
        }
        first_input.get_or_insert(input);
    }
    let id = format!("cmpl-{}", rand_id(29));
    let created = now_secs();
    let mode = choices.first().map(|c| c.2).unwrap_or(engine::Mode::Canned);
    let finish = choices
        .first()
        .map(|c| finish_reason(&c.1))
        .unwrap_or("stop");
    ctx.record(super::RecordArgs {
        body: &body,
        model: &model,
        mode: mode.as_str(),
        stream,
        prompt: &prompts.join("\n"),
        prompt_tokens,
        completion_tokens,
        finish,
        reply: choices.first().map(|c| c.0.as_str()).unwrap_or(""),
    });
    let served = Served {
        model: model.clone(),
        mode: Some(mode),
        tokens: prompt_tokens + completion_tokens,
    };
    if stream {
        let include_usage = v
            .get("stream_options")
            .and_then(|o| o.get("include_usage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let pace = ctx.pace;
        let s = async_stream::stream! {
            let chunk = |choice: Value, usage: Option<Value>| {
                let mut c = json!({"id": &id, "object": "text_completion", "created": created, "model": &model, "system_fingerprint": "fp_rustybin", "choices": choice});
                if include_usage { c["usage"] = usage.unwrap_or(Value::Null); }
                Bytes::from(format!("data: {c}\n\n"))
            };
            if !pace.ttft.is_zero() { tokio::time::sleep(pace.ttft).await; }
            for (i, (text, fin, _)) in choices.iter().enumerate() {
                let pieces = super::tokens::pieces(text);
                let delay = pace.per_piece(pieces.len());
                for p in pieces {
                    if !delay.is_zero() { tokio::time::sleep(delay).await; }
                    yield Ok::<_, Infallible>(chunk(json!([{"text": p, "index": i, "logprobs": null, "finish_reason": null}]), None));
                }
                yield Ok(chunk(json!([{"text": "", "index": i, "logprobs": null, "finish_reason": finish_reason(fin)}]), None));
            }
            if include_usage {
                yield Ok(chunk(json!([]), Some(json!({"prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens, "total_tokens": prompt_tokens + completion_tokens}))));
            }
            yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
        };
        return stream_response("text/event-stream; charset=utf-8", s, served);
    }
    ctx.wait_ttft().await;
    let choices: Vec<Value> = choices
        .iter()
        .enumerate()
        .map(|(i, (t, f, _))| json!({"text": t, "index": i, "logprobs": null, "finish_reason": finish_reason(f)}))
        .collect();
    json_response(
        json!({
            "id": id,
            "object": "text_completion",
            "created": created,
            "model": model,
            "system_fingerprint": "fp_rustybin",
            "choices": choices,
            "usage": {"prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens, "total_tokens": prompt_tokens + completion_tokens}
        }),
        served,
    )
}

async fn completions(Extension(ctx): Extension<AiCtx>, body: Bytes) -> Response {
    completions_impl(ctx, body, None).await
}

// ── Embeddings ──────────────────────────────────────────────────────

/// Embedding inputs: string, array of strings, token array, array of token arrays.
pub fn embedding_inputs(v: Option<&Value>) -> Result<Vec<(String, u32)>, String> {
    let tok_text = |a: &Vec<Value>| -> (String, u32) {
        (
            a.iter()
                .map(|n| format!("t{n}"))
                .collect::<Vec<_>>()
                .join(" "),
            a.len() as u32,
        )
    };
    let out = match v {
        Some(Value::String(s)) => vec![(s.clone(), super::tokens::count(s))],
        Some(Value::Array(a)) if a.is_empty() => {
            return Err("'$.input' is invalid: an empty array is not allowed.".into())
        }
        Some(Value::Array(a)) if a.iter().all(Value::is_number) => vec![tok_text(a)],
        Some(Value::Array(a)) => {
            let mut out = Vec::new();
            for item in a {
                match item {
                    Value::String(s) => out.push((s.clone(), super::tokens::count(s))),
                    Value::Array(t) => out.push(tok_text(t)),
                    _ => {
                        return Err("'$.input' is invalid: expected strings or token arrays.".into())
                    }
                }
            }
            out
        }
        _ => return Err("'$.input' is invalid. Please check the API reference.".into()),
    };
    if out.iter().any(|(s, _)| s.is_empty()) {
        return Err("'$.input' is invalid: empty strings are not allowed.".into());
    }
    Ok(out)
}

/// Default dimensions of a model name.
pub fn default_dims(model: &str) -> usize {
    if model.contains("large") {
        3072
    } else if model.contains("ollama") || model.contains("nomic") {
        768
    } else {
        1536
    }
}

pub async fn embeddings_impl(ctx: AiCtx, body: Bytes, model_override: Option<String>) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return ctx.error(
                ErrorKind::BadRequest,
                format!("We could not parse the JSON body of your request. ({e})"),
            )
        }
    };
    let model = model_override
        .or_else(|| v.get("model").and_then(Value::as_str).map(String::from))
        .unwrap_or_else(|| DEFAULT_EMBED_MODEL.into());
    let inputs = match embedding_inputs(v.get("input")) {
        Ok(i) => i,
        Err(m) => return ctx.error(ErrorKind::BadRequest, m),
    };
    if inputs.len() > ctx.shared.max_inputs() {
        return ctx.error(
            ErrorKind::BadRequest,
            format!(
                "'$.input' is invalid: at most {} inputs per request.",
                ctx.shared.max_inputs()
            ),
        );
    }
    let dims = v
        .get("dimensions")
        .and_then(Value::as_u64)
        .map(|d| d as usize)
        .unwrap_or_else(|| default_dims(&model));
    if dims == 0 || dims > ctx.shared.max_dims() {
        return ctx.error(
            ErrorKind::BadRequest,
            format!(
                "'dimensions' must be between 1 and {}.",
                ctx.shared.max_dims()
            ),
        );
    }
    let base64 = v.get("encoding_format").and_then(Value::as_str) == Some("base64");
    let mut tokens = 0;
    let data: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(i, (text, t))| {
            tokens += *t;
            let e = embed::embed(text, dims);
            let embedding = if base64 {
                json!(embed::to_base64(&e))
            } else {
                json!(e)
            };
            json!({"object": "embedding", "index": i, "embedding": embedding})
        })
        .collect();
    let joined: Vec<&str> = inputs.iter().map(|(s, _)| s.as_str()).collect();
    ctx.record(super::RecordArgs {
        body: &body,
        model: &model,
        mode: "embedding",
        stream: false,
        prompt: &joined.join("\n"),
        prompt_tokens: tokens,
        completion_tokens: 0,
        finish: "stop",
        reply: &format!("{} embeddings of {dims} dimensions", inputs.len()),
    });
    json_response(
        json!({
            "object": "list",
            "data": data,
            "model": model,
            "usage": {"prompt_tokens": tokens, "total_tokens": tokens}
        }),
        Served {
            model,
            mode: None,
            tokens,
        },
    )
}

async fn embeddings(Extension(ctx): Extension<AiCtx>, body: Bytes) -> Response {
    embeddings_impl(ctx, body, None).await
}

// ── Models ──────────────────────────────────────────────────────────

pub const MODELS: &[(&str, &str)] = &[
    ("rustybin-gpt", "rustybin"),
    ("rustybin-gpt-fast", "rustybin"),
    ("rustybin-embed", "rustybin"),
    ("rustybin-echo", "rustybin"),
    ("rustybin-scripted", "rustybin"),
    ("rustybin-random", "rustybin"),
    ("gpt-4o", "rustybin"),
    ("gpt-4o-mini", "rustybin"),
    ("gpt-4.1", "rustybin"),
    ("text-embedding-3-small", "rustybin"),
    ("text-embedding-3-large", "rustybin"),
    ("omni-moderation-latest", "rustybin"),
    ("gpt-image-1", "rustybin"),
    ("whisper-1", "rustybin"),
];

fn model_json(id: &str, owner: &str) -> Value {
    json!({"id": id, "object": "model", "created": 1_700_000_000, "owned_by": owner})
}

async fn models() -> Response {
    let data: Vec<Value> = MODELS.iter().map(|(id, o)| model_json(id, o)).collect();
    axum::Json(json!({"object": "list", "data": data})).into_response()
}

async fn model(
    Extension(ctx): Extension<AiCtx>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let known = MODELS.iter().any(|(m, _)| *m == id)
        || id.starts_with("rustybin")
        || engine::Mode::from_model(&id).is_some();
    if !known {
        return ctx.error(
            ErrorKind::NotFound,
            format!(
                "The model '{}' does not exist",
                id.chars().take(100).collect::<String>()
            ),
        );
    }
    axum::Json(model_json(&id, "rustybin")).into_response()
}

// ── Moderations ─────────────────────────────────────────────────────

/// Moderation categories and their trigger words (whole words).
pub const MODERATION_RULES: &[(&str, &[&str])] = &[
    (
        "harassment",
        &["idiot", "stupid", "moron", "loser", "toxic"],
    ),
    ("harassment/threatening", &["threaten", "threat"]),
    ("hate", &["hate", "racist", "bigot"]),
    ("hate/threatening", &["exterminate"]),
    (
        "illicit",
        &["jailbreak", "hack", "hacking", "steal", "drugs"],
    ),
    ("illicit/violent", &["bomb", "explosive"]),
    ("self-harm", &["suicide"]),
    ("self-harm/intent", &["suicidal"]),
    ("self-harm/instructions", &["overdose"]),
    ("sexual", &["nsfw", "explicit"]),
    ("sexual/minors", &[]),
    ("violence", &["kill", "murder", "attack", "weapon"]),
    ("violence/graphic", &["gore", "blood"]),
];

/// Moderation result for one text.
pub fn moderate(text: &str) -> Value {
    let words: std::collections::HashSet<String> = engine::words(text).into_iter().collect();
    let mut categories = serde_json::Map::new();
    let mut scores = serde_json::Map::new();
    let mut applied = serde_json::Map::new();
    let mut flagged = false;
    let h = engine::stable_hash(text);
    for (i, (cat, triggers)) in MODERATION_RULES.iter().enumerate() {
        let hit = triggers.iter().any(|t| words.contains(*t));
        flagged |= hit;
        categories.insert((*cat).into(), json!(hit));
        let noise = f64::from(h[i % 32]) / 255.0 / 1000.0;
        let score = if hit {
            0.9 + noise * 50.0
        } else {
            0.0001 + noise
        };
        scores.insert((*cat).into(), json!((score * 1e6).round() / 1e6));
        applied.insert((*cat).into(), json!(["text"]));
    }
    json!({
        "flagged": flagged,
        "categories": categories,
        "category_scores": scores,
        "category_applied_input_types": applied,
    })
}

async fn moderations(Extension(ctx): Extension<AiCtx>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return ctx.error(
                ErrorKind::BadRequest,
                format!("We could not parse the JSON body of your request. ({e})"),
            )
        }
    };
    let texts: Vec<String> = match v.get("input") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) if a.iter().all(Value::is_string) => a
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect(),
        Some(Value::Array(a)) => {
            // Multi-modal parts: one result for all text parts.
            let t: Vec<&str> = a
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect();
            vec![t.join("\n")]
        }
        _ => {
            return ctx.error(
                ErrorKind::BadRequest,
                "Missing required parameter: 'input'.",
            )
        }
    };
    let texts: Vec<String> = texts.into_iter().take(ctx.shared.max_inputs()).collect();
    let model = v
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("omni-moderation-latest")
        .to_string();
    let results: Vec<Value> = texts.iter().map(|t| moderate(t)).collect();
    let any = results.iter().any(|r| r["flagged"] == json!(true));
    ctx.record(super::RecordArgs {
        body: &body,
        model: &model,
        mode: "moderation",
        stream: false,
        prompt: &texts.join("\n"),
        prompt_tokens: texts.iter().map(|t| super::tokens::count(t)).sum(),
        completion_tokens: 0,
        finish: if any { "flagged" } else { "clean" },
        reply: "",
    });
    json_response(
        json!({"id": format!("modr-{}", rand_id(24)), "model": model, "results": results}),
        Served {
            model,
            mode: None,
            tokens: 0,
        },
    )
}

// ── Images ──────────────────────────────────────────────────────────

/// A valid 1x1 RGBA PNG (built once, with correct CRCs and Adler-32).
pub fn tiny_png() -> &'static [u8] {
    static PNG: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    PNG.get_or_init(|| {
        fn chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut c = ty.to_vec();
            c.extend_from_slice(data);
            out.extend_from_slice(&c);
            out.extend_from_slice(&super::eventstream::crc32(&c).to_be_bytes());
        }
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        // One scanline: filter byte 0 + RGBA (Rustybin orange).
        let raw = [0u8, 0xE0, 0x6C, 0x2B, 0xFF];
        let mut z = vec![0x78, 0x01, 0x01];
        z.extend_from_slice(&(raw.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
        z.extend_from_slice(&raw);
        let (mut a, mut b) = (1u32, 0u32);
        for x in raw {
            a = (a + u32::from(x)) % 65521;
            b = (b + a) % 65521;
        }
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    })
}

/// `scheme://host` of the request (forwarded headers honoured, host validated).
fn base_url(headers: &HeaderMap) -> String {
    let get = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).map(str::trim);
    let valid = |h: &&str| {
        !h.is_empty()
            && h.len() <= 255
            && h.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-.:[]_".contains(&b))
    };
    let host = get("x-forwarded-host")
        .and_then(|h| h.split(',').next())
        .map(str::trim)
        .filter(valid)
        .or_else(|| get("host").filter(valid))
        .unwrap_or("localhost");
    let scheme = match get("x-forwarded-proto")
        .and_then(|p| p.split(',').next())
        .map(str::trim)
    {
        Some("https") => "https",
        _ => "http",
    };
    format!("{scheme}://{host}")
}

async fn images(Extension(ctx): Extension<AiCtx>, headers: HeaderMap, body: Bytes) -> Response {
    use base64::Engine;
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return ctx.error(
                ErrorKind::BadRequest,
                format!("We could not parse the JSON body of your request. ({e})"),
            )
        }
    };
    let Some(prompt) = v.get("prompt").and_then(Value::as_str) else {
        return ctx.error(
            ErrorKind::BadRequest,
            "Missing required parameter: 'prompt'.",
        );
    };
    let model = v
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("dall-e-2")
        .to_string();
    let n = as_u32(v.get("n"))
        .unwrap_or(1)
        .clamp(1, if ctx.shared.public() { 1 } else { 4 });
    let b64 = match v.get("response_format").and_then(Value::as_str) {
        Some("b64_json") => true,
        Some(_) => false,
        None => model.starts_with("gpt-image"),
    };
    let item = if b64 {
        json!({"b64_json": base64::engine::general_purpose::STANDARD.encode(tiny_png()), "revised_prompt": prompt})
    } else {
        json!({"url": format!("{}/image/png", base_url(&headers)), "revised_prompt": prompt})
    };
    let data: Vec<Value> = (0..n).map(|_| item.clone()).collect();
    let pt = super::tokens::count(prompt);
    ctx.record(super::RecordArgs {
        body: &body,
        model: &model,
        mode: "image",
        stream: false,
        prompt,
        prompt_tokens: pt,
        completion_tokens: 0,
        finish: "stop",
        reply: &format!("{n} image(s)"),
    });
    json_response(
        json!({
            "created": now_secs(),
            "data": data,
            "usage": {"input_tokens": pt, "output_tokens": 0, "total_tokens": pt, "input_tokens_details": {"text_tokens": pt, "image_tokens": 0}}
        }),
        Served {
            model,
            mode: None,
            tokens: pt,
        },
    )
}

// ── Audio transcription ─────────────────────────────────────────────

/// One multipart/form-data field.
pub struct FormField {
    pub name: String,
    pub filename: Option<String>,
    pub data: Vec<u8>,
}

/// Minimal multipart/form-data parser (bounded by the body limit).
pub fn parse_multipart(content_type: &str, body: &[u8]) -> Option<Vec<FormField>> {
    let boundary = content_type
        .split(';')
        .map(str::trim)
        .find_map(|p| p.strip_prefix("boundary="))?
        .trim_matches('"');
    if boundary.is_empty() {
        return None;
    }
    let delim = format!("--{boundary}");
    let d = delim.as_bytes();
    let find = |hay: &[u8], needle: &[u8], from: usize| -> Option<usize> {
        hay.get(from..)?
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| p + from)
    };
    let mut fields = Vec::new();
    let mut pos = find(body, d, 0)? + d.len();
    for _ in 0..64 {
        if body.get(pos..pos + 2) == Some(b"--") {
            break;
        }
        let start = pos + 2; // skip CRLF
        let header_end = find(body, b"\r\n\r\n", start)?;
        let head = String::from_utf8_lossy(body.get(start..header_end)?).into_owned();
        let data_start = header_end + 4;
        let next = find(body, d, data_start)?;
        let data_end = next.saturating_sub(2).max(data_start);
        let mut name = String::new();
        let mut filename = None;
        for line in head.lines() {
            if line.to_ascii_lowercase().starts_with("content-disposition") {
                for attr in line.split(';').map(str::trim) {
                    if let Some(v) = attr.strip_prefix("name=") {
                        name = v.trim_matches('"').to_string();
                    } else if let Some(v) = attr.strip_prefix("filename=") {
                        filename = Some(v.trim_matches('"').to_string());
                    }
                }
            }
        }
        fields.push(FormField {
            name,
            filename,
            data: body.get(data_start..data_end)?.to_vec(),
        });
        pos = next + d.len();
    }
    Some(fields)
}

pub const TRANSCRIPT: &str =
    "Hello from Rustybin. This is a deterministic mock transcription for AI gateway testing.";

async fn transcriptions(
    Extension(ctx): Extension<AiCtx>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(fields) = parse_multipart(ct, &body) else {
        return ctx.error(
            ErrorKind::BadRequest,
            "Expected multipart/form-data with a 'file' field.",
        );
    };
    let field = |n: &str| fields.iter().find(|f| f.name == n);
    let text_of = |n: &str| field(n).map(|f| String::from_utf8_lossy(&f.data).into_owned());
    let Some(file) = field("file") else {
        return ctx.error(ErrorKind::BadRequest, "Missing required parameter: 'file'.");
    };
    let model = text_of("model").unwrap_or_else(|| "whisper-1".into());
    let format = text_of("response_format").unwrap_or_else(|| "json".into());
    let language = text_of("language").unwrap_or_else(|| "en".into());
    let duration = (file.data.len() as f64 / 16_000.0).max(1.0);
    let tokens = super::tokens::count(TRANSCRIPT);
    ctx.record(super::RecordArgs {
        body: format!(
            "multipart: file={} ({} bytes), model={model}, response_format={format}",
            file.filename.as_deref().unwrap_or("?"),
            file.data.len()
        )
        .as_bytes(),
        model: &model,
        mode: "transcription",
        stream: false,
        prompt: "",
        prompt_tokens: 0,
        completion_tokens: tokens,
        finish: "stop",
        reply: TRANSCRIPT,
    });
    let served = Served {
        model: model.clone(),
        mode: None,
        tokens,
    };
    let text_resp = |ct: &'static str, body: String| {
        let mut r = (StatusCode::OK, body).into_response();
        r.headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
        r.extensions_mut().insert(served.clone());
        r
    };
    match format.as_str() {
        "text" => text_resp("text/plain; charset=utf-8", format!("{TRANSCRIPT}\n")),
        "srt" => text_resp(
            "text/plain; charset=utf-8",
            format!(
                "1\n00:00:00,000 --> 00:00:0{},000\n{TRANSCRIPT}\n",
                (duration as u64).min(9)
            ),
        ),
        "vtt" => text_resp(
            "text/vtt; charset=utf-8",
            format!(
                "WEBVTT\n\n00:00:00.000 --> 00:00:0{}.000\n{TRANSCRIPT}\n",
                (duration as u64).min(9)
            ),
        ),
        "verbose_json" => json_response(
            json!({
                "task": "transcribe",
                "language": language,
                "duration": duration,
                "text": TRANSCRIPT,
                "segments": [{
                    "id": 0, "seek": 0, "start": 0.0, "end": duration, "text": TRANSCRIPT,
                    "tokens": [], "temperature": 0.0, "avg_logprob": -0.2,
                    "compression_ratio": 1.2, "no_speech_prob": 0.01
                }]
            }),
            served,
        ),
        _ => json_response(
            json!({"text": TRANSCRIPT, "usage": {"type": "duration", "seconds": duration.ceil() as u64}}),
            served,
        ),
    }
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ai/openai/v1/chat/completions", post(chat))
        .route("/ai/openai/v1/completions", post(completions))
        .route("/ai/openai/v1/embeddings", post(embeddings))
        .route("/ai/openai/v1/models", get(models))
        .route("/ai/openai/v1/models/{model}", get(model))
        .route("/ai/openai/v1/moderations", post(moderations))
        .route("/ai/openai/v1/images/generations", post(images))
        .route("/ai/openai/v1/audio/transcriptions", post(transcriptions))
        // Legacy aliases (backwards compatible).
        .route("/ai/v1/chat/completions", post(chat))
        .route("/ai/v1/completions", post(completions))
        .route("/ai/v1/embeddings", post(embeddings))
        .route("/ai/v1/models", get(models))
        .route("/ai/v1/models/{model}", get(model))
        .route("/ai/v1/moderations", post(moderations))
        .route("/ai/v1/images/generations", post(images))
        .route("/ai/v1/audio/transcriptions", post(transcriptions))
}

const CHAT_EX: &str = r#"{"model":"rustybin-gpt","messages":[{"role":"user","content":"hello"}]}"#;
const CHAT_STREAM_EX: &str = r#"{"model":"rustybin-gpt","messages":[{"role":"user","content":"hello"}],"stream":true,"stream_options":{"include_usage":true}}"#;
const CHAT_TOOLS_EX: &str = r#"{"model":"gpt-4o","messages":[{"role":"user","content":"What is the weather in Paris?"}],"tools":[{"type":"function","function":{"name":"get_weather","description":"Get the current weather for a location","parameters":{"type":"object","properties":{"location":{"type":"string"},"unit":{"type":"string","enum":["celsius","fahrenheit"]}},"required":["location"]}}}]}"#;
const CHAT_JSON_EX: &str = r#"{"model":"gpt-4o","messages":[{"role":"user","content":"Extract the person"}],"response_format":{"type":"json_schema","json_schema":{"name":"person","strict":true,"schema":{"type":"object","properties":{"name":{"type":"string"},"age":{"type":"integer"}},"required":["name","age"],"additionalProperties":false}}}}"#;
const CHAT_ECHO_EX: &str = r#"{"model":"rustybin-echo","messages":[{"role":"system","content":"You are terse."},{"role":"user","content":"What did you receive?"}]}"#;
const CHAT_PII_EX: &str = r#"{"model":"rustybin-scripted","messages":[{"role":"user","content":"Show me the customer SSN and credit card"}]}"#;

fn pair(
    primary: &'static str,
    alias: &'static str,
    methods: &'static [&'static str],
    summary: &'static str,
    description: &'static str,
    examples: impl Fn(&'static str, bool) -> Vec<Example>,
) -> [Endpoint; 2] {
    let mut a =
        Endpoint::new(primary, methods, category::AI_OPENAI, summary).description(description);
    for e in examples(primary, true) {
        a = a.example(e);
    }
    let mut b = Endpoint::new(alias, methods, category::AI_OPENAI, summary)
        .description("Alias of the /ai/openai/v1 route (kept for backwards compatibility).");
    for e in examples(alias, false) {
        b = b.example(e);
    }
    [a, b]
}

pub fn catalog() -> Vec<Endpoint> {
    let mut v = Vec::new();
    v.extend(pair(
        "/ai/openai/v1/chat/completions",
        "/ai/v1/chat/completions",
        &["POST"],
        "Chat completions (SSE streaming with stream=true, tools, structured output, n)",
        "Content may be a string, null or an array of parts (text, image_url, input_audio, file). Tools produce tool_calls when the user text mentions the tool; response_format json_schema returns schema-valid JSON. Mode: X-Rustybin-Mode or model name (rustybin-echo, rustybin-scripted, rustybin-random).",
        |p, full| {
            let mut ex = vec![
                Example::post("Chat completion", p).json(CHAT_EX),
                Example::post("Chat completion (streaming)", p).json(CHAT_STREAM_EX),
            ];
            if full {
                ex.push(Example::post("Tool call", p).json(CHAT_TOOLS_EX));
                ex.push(Example::post("Structured output (json_schema)", p).json(CHAT_JSON_EX));
                ex.push(Example::post("Echo mode (shows decorated prompt)", p).json(CHAT_ECHO_EX));
                ex.push(Example::post("Scripted PII reply", p).json(CHAT_PII_EX));
                ex.push(Example::post("Simulated 429", p).header("X-Rustybin-Fail", "429").json(CHAT_EX).expect_status(429));
                ex.push(Example::post("Require a credential", p).header("X-Rustybin-Require-Auth", "true").bearer("sk-demo-1234").json(CHAT_EX));
            }
            ex
        },
    ));
    v.extend(pair(
        "/ai/openai/v1/completions",
        "/ai/v1/completions",
        &["POST"],
        "Legacy text completions (stream, echo, suffix, n)",
        "prompt may be a string, an array of strings or token arrays.",
        |p, _| {
            vec![Example::post("Text completion", p)
                .json(r#"{"model":"rustybin-gpt","prompt":"Say hello"}"#)]
        },
    ));
    v.extend(pair(
        "/ai/openai/v1/embeddings",
        "/ai/v1/embeddings",
        &["POST"],
        "Deterministic bag-of-words embeddings (dimensions, base64)",
        "Same input gives the same vector; case-insensitive; reordered words stay close (cosine about 0.9). Default 1536 dimensions (3072 for *large* models), `dimensions` overrides, `encoding_format: base64` returns little-endian f32.",
        |p, _| vec![Example::post("Embeddings", p).json(r#"{"model":"text-embedding-3-small","input":"Hello world"}"#)],
    ));
    v.extend(pair(
        "/ai/openai/v1/models",
        "/ai/v1/models",
        &["GET"],
        "List available models",
        "",
        |p, _| vec![Example::get("List models", p)],
    ));
    v.extend(pair(
        "/ai/openai/v1/models/{model}",
        "/ai/v1/models/{model}",
        &["GET"],
        "Retrieve a model (404 model_not_found for unknown ids)",
        "",
        |_, primary| {
            if primary {
                vec![Example::get(
                    "Retrieve a model",
                    "/ai/openai/v1/models/gpt-4o",
                )]
            } else {
                vec![Example::get(
                    "Retrieve a model",
                    "/ai/v1/models/rustybin-gpt",
                )]
            }
        },
    ));
    v.extend(pair(
        "/ai/openai/v1/moderations",
        "/ai/v1/moderations",
        &["POST"],
        "Moderation (keyword-based categories, deterministic scores)",
        "Flags whole words such as hate, idiot, kill, jailbreak, suicide (see the README table).",
        |p, _| vec![Example::post("Moderation", p).json(r#"{"input":"I hate you, you idiot"}"#)],
    ));
    v.extend(pair(
        "/ai/openai/v1/images/generations",
        "/ai/v1/images/generations",
        &["POST"],
        "Image generation (tiny valid PNG as b64_json or a URL to /image/png)",
        "",
        |p, _| {
            vec![Example::post("Generate an image", p).json(
                r#"{"model":"gpt-image-1","prompt":"a rusty bin","response_format":"b64_json"}"#,
            )]
        },
    ));
    v.extend(pair(
        "/ai/openai/v1/audio/transcriptions",
        "/ai/v1/audio/transcriptions",
        &["POST"],
        "Audio transcription (multipart; json, text, srt, vtt, verbose_json)",
        "Returns a fixed transcript for any uploaded file.",
        |p, _| vec![Example::post("Transcribe (missing file)", p).expect_status(400)],
    ));
    v
}

pub fn chat_request_schema() -> Value {
    json!({
        "type": "object",
        "required": ["messages"],
        "properties": {
            "model": {"type": "string", "example": "rustybin-gpt"},
            "messages": {"type": "array", "items": {"type": "object", "required": ["role"], "properties": {
                "role": {"type": "string", "enum": ["system", "developer", "user", "assistant", "tool", "function"]},
                "content": {"nullable": true, "oneOf": [
                    {"type": "string"},
                    {"type": "array", "items": {"type": "object", "properties": {
                        "type": {"type": "string", "enum": ["text", "image_url", "input_audio", "file", "refusal"]},
                        "text": {"type": "string"},
                        "image_url": {"type": "object", "properties": {"url": {"type": "string"}}}
                    }}}
                ]},
                "tool_calls": {"type": "array", "items": {"type": "object"}},
                "tool_call_id": {"type": "string"}
            }}},
            "stream": {"type": "boolean", "default": false},
            "stream_options": {"type": "object", "properties": {"include_usage": {"type": "boolean"}}},
            "n": {"type": "integer", "minimum": 1, "maximum": 8},
            "max_tokens": {"type": "integer"},
            "max_completion_tokens": {"type": "integer"},
            "stop": {"oneOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}]},
            "temperature": {"type": "number", "minimum": 0, "maximum": 2},
            "tools": {"type": "array", "items": {"type": "object", "properties": {
                "type": {"type": "string", "enum": ["function"]},
                "function": {"type": "object", "properties": {
                    "name": {"type": "string"}, "description": {"type": "string"}, "parameters": {"type": "object"}
                }}
            }}},
            "tool_choice": {"oneOf": [{"type": "string", "enum": ["none", "auto", "required"]}, {"type": "object"}]},
            "response_format": {"type": "object", "properties": {
                "type": {"type": "string", "enum": ["text", "json_object", "json_schema"]},
                "json_schema": {"type": "object"}
            }}
        }
    })
}

pub fn chat_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "string", "example": "chatcmpl-abc123"},
            "object": {"type": "string", "example": "chat.completion"},
            "created": {"type": "integer"},
            "model": {"type": "string"},
            "choices": {"type": "array", "items": {"type": "object", "properties": {
                "index": {"type": "integer"},
                "message": {"type": "object", "properties": {
                    "role": {"type": "string"},
                    "content": {"type": "string", "nullable": true},
                    "tool_calls": {"type": "array", "items": {"type": "object"}}
                }},
                "logprobs": {"nullable": true},
                "finish_reason": {"type": "string", "enum": ["stop", "length", "tool_calls", "content_filter"]}
            }}},
            "usage": {"type": "object", "properties": {
                "prompt_tokens": {"type": "integer"},
                "completion_tokens": {"type": "integer"},
                "total_tokens": {"type": "integer"}
            }}
        }
    })
}

fn op(id: &str, summary: &str, req: Option<Value>, resp: Value) -> Value {
    let mut o = json!({
        "tags": ["AI Gateway"],
        "summary": summary,
        "operationId": id,
        "parameters": super::common_parameters(),
        "responses": {
            "200": {"description": "OK", "content": {"application/json": {"schema": resp}}},
            "400": {"description": "Invalid request (OpenAI error shape)"},
            "401": {"description": "Missing or invalid credential (when required)"},
            "429": {"description": "Simulated rate limit"}
        }
    });
    if let Some(r) = req {
        o["requestBody"] =
            json!({"required": true, "content": {"application/json": {"schema": r}}});
    }
    o
}

pub fn openapi_paths() -> Value {
    let chat = |id: &str| {
        let mut o = op(
            id,
            "Chat completions (OpenAI-compatible)",
            Some(json!({"$ref": "#/components/schemas/ChatCompletionRequest"})),
            json!({"$ref": "#/components/schemas/ChatCompletionResponse"}),
        );
        o["responses"]["200"]["content"]["text/event-stream"] = json!({"schema": {"type": "string", "description": "chat.completion.chunk events, then data: [DONE]"}});
        json!({"post": o})
    };
    let completions = |id: &str| json!({"post": op(id, "Legacy text completions", Some(json!({"type": "object", "properties": {"model": {"type": "string"}, "prompt": {"oneOf": [{"type": "string"}, {"type": "array", "items": {}}]}, "max_tokens": {"type": "integer"}, "stream": {"type": "boolean"}, "echo": {"type": "boolean"}, "suffix": {"type": "string"}, "n": {"type": "integer"}}})), json!({"type": "object"}))});
    let embeddings = |id: &str| {
        json!({"post": op(id, "Embeddings", Some(json!({"type": "object", "required": ["input"], "properties": {
        "model": {"type": "string", "example": "text-embedding-3-small"},
        "input": {"oneOf": [{"type": "string"}, {"type": "array", "items": {}}]},
        "dimensions": {"type": "integer"},
        "encoding_format": {"type": "string", "enum": ["float", "base64"]}
    }})), json!({"type": "object", "properties": {"object": {"type": "string"}, "data": {"type": "array", "items": {"type": "object", "properties": {"index": {"type": "integer"}, "embedding": {"oneOf": [{"type": "array", "items": {"type": "number"}}, {"type": "string"}]}}}}, "usage": {"type": "object"}}}))})
    };
    let models = |id: &str| json!({"get": {"tags": ["AI Gateway"], "summary": "List models", "operationId": id, "responses": {"200": {"description": "Model list"}}}});
    let model = |id: &str| json!({"get": {"tags": ["AI Gateway"], "summary": "Retrieve a model", "operationId": id, "parameters": [{"name": "model", "in": "path", "required": true, "schema": {"type": "string"}}], "responses": {"200": {"description": "Model"}, "404": {"description": "Unknown model"}}}});
    let moderations = |id: &str| json!({"post": op(id, "Moderations", Some(json!({"type": "object", "required": ["input"], "properties": {"input": {"oneOf": [{"type": "string"}, {"type": "array", "items": {}}]}, "model": {"type": "string"}}})), json!({"type": "object", "properties": {"id": {"type": "string"}, "model": {"type": "string"}, "results": {"type": "array", "items": {"type": "object"}}}}))});
    let images = |id: &str| json!({"post": op(id, "Image generation", Some(json!({"type": "object", "required": ["prompt"], "properties": {"prompt": {"type": "string"}, "model": {"type": "string"}, "n": {"type": "integer"}, "size": {"type": "string"}, "response_format": {"type": "string", "enum": ["url", "b64_json"]}}})), json!({"type": "object", "properties": {"created": {"type": "integer"}, "data": {"type": "array", "items": {"type": "object", "properties": {"url": {"type": "string"}, "b64_json": {"type": "string"}}}}}}))});
    let audio = |id: &str| {
        let mut o = op(
            id,
            "Audio transcription",
            None,
            json!({"type": "object", "properties": {"text": {"type": "string"}}}),
        );
        o["requestBody"] = json!({"required": true, "content": {"multipart/form-data": {"schema": {"type": "object", "required": ["file", "model"], "properties": {"file": {"type": "string", "format": "binary"}, "model": {"type": "string"}, "response_format": {"type": "string", "enum": ["json", "text", "srt", "verbose_json", "vtt"]}, "language": {"type": "string"}}}}}});
        json!({"post": o})
    };
    json!({
        "/ai/openai/v1/chat/completions": chat("openaiChatCompletions"),
        "/ai/v1/chat/completions": chat("postChatCompletions"),
        "/ai/openai/v1/completions": completions("openaiCompletions"),
        "/ai/v1/completions": completions("postCompletions"),
        "/ai/openai/v1/embeddings": embeddings("openaiEmbeddings"),
        "/ai/v1/embeddings": embeddings("postEmbeddings"),
        "/ai/openai/v1/models": models("openaiListModels"),
        "/ai/v1/models": models("getModels"),
        "/ai/openai/v1/models/{model}": model("openaiGetModel"),
        "/ai/v1/models/{model}": model("getModel"),
        "/ai/openai/v1/moderations": moderations("openaiModerations"),
        "/ai/v1/moderations": moderations("postModerations"),
        "/ai/openai/v1/images/generations": images("openaiImages"),
        "/ai/v1/images/generations": images("postImages"),
        "/ai/openai/v1/audio/transcriptions": audio("openaiTranscriptions"),
        "/ai/v1/audio/transcriptions": audio("postTranscriptions"),
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    const URL: &str = "/ai/openai/v1/chat/completions";

    #[tokio::test]
    async fn chat_non_stream_and_legacy_alias() {
        let app = app();
        for url in [URL, "/ai/v1/chat/completions"] {
            let (s, _, b) = send(
                &app,
                url,
                &serde_json::from_str(CHAT_EX).expect("json"),
                &[],
            )
            .await;
            assert_eq!(s, 200);
            let v = json(&b);
            assert_eq!(v["object"], "chat.completion");
            assert_eq!(v["model"], "rustybin-gpt");
            assert!(v["id"].as_str().unwrap_or("").starts_with("chatcmpl-"));
            assert_eq!(v["choices"][0]["finish_reason"], "stop");
            assert!(v["choices"][0]["logprobs"].is_null());
            assert!(v["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or("")
                .contains("Rustybin"));
            let u = &v["usage"];
            assert_eq!(
                u["total_tokens"].as_u64(),
                Some(
                    u["prompt_tokens"].as_u64().unwrap_or(0)
                        + u["completion_tokens"].as_u64().unwrap_or(0)
                )
            );
        }
    }

    #[tokio::test]
    async fn content_shapes_are_accepted() {
        let app = app();
        let body = json!({"messages": [
            {"role": "system", "content": [{"type": "text", "text": "sys"}]},
            {"role": "user", "content": [
                {"type": "text", "text": "describe"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
                {"type": "input_audio", "input_audio": {"data": "AAAA", "format": "wav"}}
            ]},
            {"role": "assistant", "content": null},
            {"role": "user", "content": "hello"}
        ]});
        let (s, _, b) = send(&app, URL, &body, &[]).await;
        assert_eq!(s, 200, "{}", String::from_utf8_lossy(&b));
        let v = json(&b);
        // 85 tokens for the image.
        assert!(v["usage"]["prompt_tokens"].as_u64().unwrap_or(0) > 85);
        let (s, _, b) = send(&app, URL, &json!({"messages": []}), &[]).await;
        assert_eq!(s, 400);
        assert_eq!(json(&b)["error"]["type"], "invalid_request_error");
        let (s, _, _) = send(
            &app,
            URL,
            &json!({"messages": [{"role": "wizard", "content": "x"}]}),
            &[],
        )
        .await;
        assert_eq!(s, 400);
    }

    #[tokio::test]
    async fn stream_with_usage_and_n() {
        let app = app();
        let body = json!({"messages": [{"role": "user", "content": "hello"}], "stream": true, "n": 2, "stream_options": {"include_usage": true}});
        let (s, h, b) = send(&app, URL, &body, &[]).await;
        assert_eq!(s, 200);
        assert!(h["content-type"]
            .to_str()
            .unwrap_or("")
            .starts_with("text/event-stream"));
        let text = String::from_utf8_lossy(&b);
        assert!(text.trim_end().ends_with("data: [DONE]"));
        let chunks = sse_data(&b);
        let last = chunks.last().expect("chunks");
        assert_eq!(last["choices"], json!([]));
        assert!(last["usage"]["total_tokens"].as_u64().unwrap_or(0) > 0);
        assert!(chunks[0]["usage"].is_null());
        // Rebuild choice 1 content and compare token count.
        let content: String = chunks
            .iter()
            .filter(|c| c["choices"][0]["index"] == 1)
            .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
            .collect();
        assert_eq!(content, engine::GREETING);
        assert_eq!(
            chunks
                .iter()
                .filter(|c| c["choices"][0]["finish_reason"] == "stop")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn tool_calls_non_stream_and_stream() {
        let app = app();
        let body: Value = serde_json::from_str(CHAT_TOOLS_EX).expect("json");
        let (_, _, b) = send(&app, URL, &body, &[]).await;
        let v = json(&b);
        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
        assert!(v["choices"][0]["message"]["content"].is_null());
        let tc = &v["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tc["function"]["name"], "get_weather");
        let args: Value =
            serde_json::from_str(tc["function"]["arguments"].as_str().unwrap_or("")).expect("args");
        assert_eq!(args["location"], "Paris");

        let mut sbody = body.clone();
        sbody["stream"] = json!(true);
        let (_, _, b) = send(&app, URL, &sbody, &[]).await;
        let chunks = sse_data(&b);
        let mut name = String::new();
        let mut arguments = String::new();
        for c in &chunks {
            if let Some(tcs) = c["choices"][0]["delta"]["tool_calls"].as_array() {
                for t in tcs {
                    if let Some(n) = t["function"]["name"].as_str() {
                        name.push_str(n);
                    }
                    if let Some(a) = t["function"]["arguments"].as_str() {
                        arguments.push_str(a);
                    }
                }
            }
        }
        assert_eq!(name, "get_weather");
        assert_eq!(
            serde_json::from_str::<Value>(&arguments).expect("args"),
            args
        );
        assert!(chunks
            .iter()
            .any(|c| c["choices"][0]["finish_reason"] == "tool_calls"));

        // Follow-up with the tool result.
        let follow = json!({"model": "gpt-4o", "messages": [
            {"role": "user", "content": "What is the weather in Paris?"},
            {"role": "assistant", "content": null, "tool_calls": [tc]},
            {"role": "tool", "tool_call_id": tc["id"], "content": "{\"temperature\": 21}"}
        ], "tools": body["tools"]});
        let (_, _, b) = send(&app, URL, &follow, &[]).await;
        let v = json(&b);
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        assert!(v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .contains("get_weather returned {\"temperature\": 21}"));
    }

    #[tokio::test]
    async fn structured_output_and_max_tokens() {
        let app = app();
        let body: Value = serde_json::from_str(CHAT_JSON_EX).expect("json");
        let (_, _, b) = send(&app, URL, &body, &[]).await;
        let v = json(&b);
        let out: Value =
            serde_json::from_str(v["choices"][0]["message"]["content"].as_str().unwrap_or(""))
                .expect("json content");
        let schema = &body["response_format"]["json_schema"]["schema"];
        assert!(super::super::schema::validate(&out, schema).is_ok());

        let (_, _, b) = send(
            &app,
            URL,
            &json!({"messages": [{"role": "user", "content": "hello"}], "max_tokens": 3}),
            &[],
        )
        .await;
        let v = json(&b);
        assert_eq!(v["choices"][0]["finish_reason"], "length");
        assert_eq!(v["usage"]["completion_tokens"], 3);
    }

    #[tokio::test]
    async fn modes_by_header_and_model() {
        let app = app();
        let body: Value = serde_json::from_str(CHAT_ECHO_EX).expect("json");
        let (_, h, b) = send(&app, URL, &body, &[]).await;
        assert_eq!(h["x-rustybin-mode"], "echo");
        assert_eq!(
            json(&b)["choices"][0]["message"]["content"],
            "system: You are terse.\nuser: What did you receive?"
        );
        let (_, _, b) = send(
            &app,
            URL,
            &json!({"messages": [{"role": "user", "content": "my ssn please"}]}),
            &[("x-rustybin-mode", "scripted")],
        )
        .await;
        assert!(json(&b)["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .contains("123-45-6789"));
    }

    #[tokio::test]
    async fn completions_embeddings_models() {
        let app = app();
        let (s, _, b) = send(
            &app,
            "/ai/openai/v1/completions",
            &json!({"prompt": ["a", "b"], "n": 2}),
            &[],
        )
        .await;
        assert_eq!(s, 200);
        assert_eq!(json(&b)["choices"].as_array().map(Vec::len), Some(4));
        let (_, _, b) = send(
            &app,
            "/ai/openai/v1/completions",
            &json!({"prompt": "hello", "stream": true}),
            &[],
        )
        .await;
        assert!(String::from_utf8_lossy(&b).contains("text_completion"));

        let (s, _, b) = send(
            &app,
            "/ai/openai/v1/embeddings",
            &json!({"input": ["Hello World", "world hello"], "dimensions": 64}),
            &[],
        )
        .await;
        assert_eq!(s, 200);
        let v = json(&b);
        assert_eq!(v["data"][0]["embedding"].as_array().map(Vec::len), Some(64));
        let (_, _, b2) = send(
            &app,
            "/ai/v1/embeddings",
            &json!({"input": "Hello World", "dimensions": 64}),
            &[],
        )
        .await;
        assert_eq!(json(&b2)["data"][0]["embedding"], v["data"][0]["embedding"]);
        let (_, _, b) = send(
            &app,
            "/ai/openai/v1/embeddings",
            &json!({"input": "x", "encoding_format": "base64"}),
            &[],
        )
        .await;
        assert!(json(&b)["data"][0]["embedding"].is_string());
        let (s, _, _) = send(&app, "/ai/openai/v1/embeddings", &json!({"input": []}), &[]).await;
        assert_eq!(s, 400);
        let (s, _, b) = fetch(&app, "/ai/v1/models").await;
        assert_eq!(s, 200);
        assert_eq!(json(&b)["data"][0]["id"], "rustybin-gpt");
        let (s, _, _) = fetch(&app, "/ai/openai/v1/models/gpt-4o").await;
        assert_eq!(s, 200);
        let (s, _, b) = fetch(&app, "/ai/openai/v1/models/nope").await;
        assert_eq!(s, 404);
        assert_eq!(json(&b)["error"]["code"], "model_not_found");
    }

    #[tokio::test]
    async fn moderation_images_audio() {
        let app = app();
        let (_, _, b) = send(
            &app,
            "/ai/openai/v1/moderations",
            &json!({"input": ["I hate you", "nice day"]}),
            &[],
        )
        .await;
        let v = json(&b);
        assert_eq!(v["results"][0]["flagged"], true);
        assert_eq!(v["results"][0]["categories"]["hate"], true);
        assert_eq!(v["results"][1]["flagged"], false);
        assert_eq!(
            v["results"][0]["category_scores"]
                .as_object()
                .map(|m| m.len()),
            Some(13)
        );

        let (_, _, b) = send(
            &app,
            "/ai/openai/v1/images/generations",
            &json!({"prompt": "x", "response_format": "b64_json"}),
            &[],
        )
        .await;
        use base64::Engine;
        let png = base64::engine::general_purpose::STANDARD
            .decode(json(&b)["data"][0]["b64_json"].as_str().unwrap_or(""))
            .expect("b64");
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        // IHDR CRC is valid.
        let crc = u32::from_be_bytes([png[29], png[30], png[31], png[32]]);
        assert_eq!(crc, super::super::eventstream::crc32(&png[12..29]));
        let (_, _, b) = send(
            &app,
            "/ai/openai/v1/images/generations",
            &json!({"prompt": "x"}),
            &[("host", "demo.example:8080")],
        )
        .await;
        assert_eq!(
            json(&b)["data"][0]["url"],
            "http://demo.example:8080/image/png"
        );

        let boundary = "XyZ";
        let mp = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nwhisper-1\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF0000\r\n--{boundary}--\r\n");
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/ai/openai/v1/audio/transcriptions")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(axum::body::Body::from(mp))
            .expect("req");
        use tower::ServiceExt;
        let resp = app.clone().oneshot(req).await.expect("resp");
        assert_eq!(resp.status(), 200);
        let v = crate::test_support::body_json(resp).await;
        assert_eq!(v["text"], TRANSCRIPT);
        let fields = parse_multipart(&format!("multipart/form-data; boundary=\"{boundary}\""), format!("--{boundary}\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nvalue\r\n--{boundary}--").as_bytes()).expect("parse");
        assert_eq!(fields[0].data, b"value");
    }
}
