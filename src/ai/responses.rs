//! OpenAI Responses API: `POST /ai/openai/v1/responses` (and `/ai/v1/responses`).
//!
//! Non-streaming returns a `response` object whose `output` holds a
//! `message` item (with `output_text` content) or `function_call` items.
//! Streaming emits the native event sequence: `response.created`,
//! `response.in_progress`, `response.output_item.added`,
//! `response.content_part.added`, `response.output_text.delta`...,
//! `response.output_text.done`, `response.content_part.done`,
//! `response.output_item.done` (function calls use
//! `response.function_call_arguments.delta/done`), `response.completed`
//! (or `response.incomplete` when `max_output_tokens` cut the text).

use axum::body::Bytes;
use axum::response::Response;
use axum::routing::post;
use axum::{Extension, Router};
use serde_json::{json, Value};
use std::convert::Infallible;

use super::engine::{ChatInput, Finish, Msg, Part, Reply, Role, ToolCall};
use super::faults::ErrorKind;
use super::openai::{
    parse_arguments, parse_content, parse_format, parse_tool_choice, parse_tools, DEFAULT_MODEL,
};
use super::{json_response, now_secs, rand_id, sse_event, stream_response, AiCtx, Served};
use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

/// Parse a Responses API body into a chat input.
pub fn parse(body: &Value) -> Result<ChatInput, String> {
    if !body.is_object() {
        return Err("The request body must be a JSON object.".into());
    }
    let mut input = ChatInput {
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_MODEL)
            .to_string(),
        tools: parse_tools(body),
        tool_choice: parse_tool_choice(body),
        format: parse_format(body.get("text").and_then(|t| t.get("format"))),
        max_tokens: body
            .get("max_output_tokens")
            .and_then(Value::as_u64)
            .map(|n| n.min(u64::from(u32::MAX)) as u32),
        ..Default::default()
    };
    if let Some(instr) = body.get("instructions").and_then(Value::as_str) {
        input.system.push(instr.to_string());
    }
    match body.get("input") {
        Some(Value::String(s)) => input.messages.push(Msg::text(Role::User, s.clone())),
        Some(Value::Array(items)) => {
            for item in items {
                let ty = item
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("message");
                match ty {
                    "message" => {
                        let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                        let parts = parse_content(item.get("content").unwrap_or(&Value::Null));
                        match role {
                            "system" | "developer" => {
                                let t: Vec<String> = parts
                                    .into_iter()
                                    .filter_map(|p| match p {
                                        Part::Text(t) => Some(t),
                                        _ => None,
                                    })
                                    .collect();
                                input.system.push(t.join("\n"));
                            }
                            "assistant" => input.messages.push(Msg {
                                role: Role::Assistant,
                                parts,
                            }),
                            _ => input.messages.push(Msg {
                                role: Role::User,
                                parts,
                            }),
                        }
                    }
                    "function_call" => input.messages.push(Msg {
                        role: Role::Assistant,
                        parts: vec![Part::ToolCall(ToolCall {
                            id: item
                                .get("call_id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            name: item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            arguments: parse_arguments(item.get("arguments")),
                        })],
                    }),
                    "function_call_output" => {
                        let out = match item.get("output") {
                            Some(Value::String(s)) => s.clone(),
                            Some(other) => other.to_string(),
                            None => String::new(),
                        };
                        input.messages.push(Msg {
                            role: Role::Tool,
                            parts: vec![Part::ToolResult {
                                id: item
                                    .get("call_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                                name: None,
                                content: out,
                                is_error: false,
                            }],
                        });
                    }
                    _ => {}
                }
            }
        }
        _ => return Err("Missing required parameter: 'input'.".into()),
    }
    Ok(input)
}

fn message_item(id: &str, text: &str, status: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "status": status,
        "role": "assistant",
        "content": [{"type": "output_text", "text": text, "annotations": [], "logprobs": []}],
    })
}

fn call_item(id: &str, c: &ToolCall, status: &str, args: &str) -> Value {
    json!({
        "type": "function_call",
        "id": id,
        "call_id": format!("call_{}", c.id),
        "name": c.name,
        "arguments": args,
        "status": status,
    })
}

struct Ids {
    resp: String,
    msg: String,
    calls: Vec<String>,
}

fn output_items(reply: &Reply, ids: &Ids) -> Vec<Value> {
    if reply.tool_calls.is_empty() {
        let st = if reply.finish == Finish::Length {
            "incomplete"
        } else {
            "completed"
        };
        vec![message_item(&ids.msg, &reply.text, st)]
    } else {
        reply
            .tool_calls
            .iter()
            .zip(&ids.calls)
            .map(|(c, id)| call_item(id, c, "completed", &c.arguments.to_string()))
            .collect()
    }
}

fn response_obj(
    body: &Value,
    input: &ChatInput,
    reply: &Reply,
    ids: &Ids,
    created: i64,
    status: &str,
    output: Vec<Value>,
) -> Value {
    let done = status != "in_progress";
    let incomplete = reply.finish == Finish::Length;
    let status = if done && incomplete {
        "incomplete"
    } else {
        status
    };
    json!({
        "id": ids.resp,
        "object": "response",
        "created_at": created,
        "status": status,
        "background": false,
        "error": null,
        "incomplete_details": if done && incomplete { json!({"reason": "max_output_tokens"}) } else if done && reply.finish == Finish::ContentFilter { json!({"reason": "content_filter"}) } else { Value::Null },
        "instructions": body.get("instructions").cloned().unwrap_or(Value::Null),
        "max_output_tokens": body.get("max_output_tokens").cloned().unwrap_or(Value::Null),
        "model": input.model,
        "output": output,
        "parallel_tool_calls": body.get("parallel_tool_calls").cloned().unwrap_or(json!(true)),
        "previous_response_id": body.get("previous_response_id").cloned().unwrap_or(Value::Null),
        "reasoning": {"effort": null, "summary": null},
        "store": body.get("store").cloned().unwrap_or(json!(true)),
        "temperature": body.get("temperature").cloned().unwrap_or(json!(1.0)),
        "text": body.get("text").cloned().unwrap_or(json!({"format": {"type": "text"}})),
        "tool_choice": body.get("tool_choice").cloned().unwrap_or(json!("auto")),
        "tools": body.get("tools").cloned().unwrap_or(json!([])),
        "top_p": body.get("top_p").cloned().unwrap_or(json!(1.0)),
        "truncation": "disabled",
        "usage": if done { json!({
            "input_tokens": reply.prompt_tokens,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens": reply.completion_tokens,
            "output_tokens_details": {"reasoning_tokens": 0},
            "total_tokens": reply.prompt_tokens + reply.completion_tokens
        }) } else { Value::Null },
        "user": null,
        "metadata": body.get("metadata").cloned().unwrap_or(json!({})),
    })
}

async fn create(Extension(ctx): Extension<AiCtx>, raw: Bytes) -> Response {
    let body: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => {
            return ctx.error(
                ErrorKind::BadRequest,
                format!("We could not parse the JSON body of your request. ({e})"),
            )
        }
    };
    let input = match parse(&body) {
        Ok(i) => i,
        Err(m) => return ctx.error(ErrorKind::BadRequest, m),
    };
    let reply = ctx.generate(&input, 0);
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let finish = match reply.finish {
        Finish::Length => "incomplete",
        Finish::ToolCalls => "function_call",
        Finish::ContentFilter => "content_filter",
        _ => "completed",
    };
    ctx.record_chat(&raw, &input, &reply, stream, finish);
    let ids = Ids {
        resp: format!("resp_{}", rand_id(48)),
        msg: format!("msg_{}", rand_id(48)),
        calls: reply
            .tool_calls
            .iter()
            .map(|_| format!("fc_{}", rand_id(48)))
            .collect(),
    };
    let created = now_secs();
    let served = Served {
        model: input.model.clone(),
        mode: Some(reply.mode),
        tokens: reply.prompt_tokens + reply.completion_tokens,
    };
    if !stream {
        ctx.wait_ttft().await;
        let output = output_items(&reply, &ids);
        return json_response(
            response_obj(&body, &input, &reply, &ids, created, "completed", output),
            served,
        );
    }
    let pace = ctx.pace;
    let s = async_stream::stream! {
        let mut seq: u64 = 0;
        let mut ev = |name: &str, mut data: Value| {
            data["type"] = json!(name);
            data["sequence_number"] = json!(seq);
            seq += 1;
            sse_event(name, &data)
        };
        let shell = response_obj(&body, &input, &reply, &ids, created, "in_progress", vec![]);
        yield Ok::<_, Infallible>(ev("response.created", json!({"response": shell.clone()})));
        yield Ok(ev("response.in_progress", json!({"response": shell})));
        if !pace.ttft.is_zero() {
            tokio::time::sleep(pace.ttft).await;
        }
        if reply.tool_calls.is_empty() {
            let pieces = reply.pieces();
            let delay = pace.per_piece(pieces.len());
            yield Ok(ev("response.output_item.added", json!({"output_index": 0, "item": {"type": "message", "id": ids.msg, "status": "in_progress", "role": "assistant", "content": []}})));
            yield Ok(ev("response.content_part.added", json!({"item_id": ids.msg, "output_index": 0, "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": [], "logprobs": []}})));
            for p in pieces {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                yield Ok(ev("response.output_text.delta", json!({"item_id": ids.msg, "output_index": 0, "content_index": 0, "delta": p, "logprobs": []})));
            }
            yield Ok(ev("response.output_text.done", json!({"item_id": ids.msg, "output_index": 0, "content_index": 0, "text": reply.text, "logprobs": []})));
            yield Ok(ev("response.content_part.done", json!({"item_id": ids.msg, "output_index": 0, "content_index": 0, "part": {"type": "output_text", "text": reply.text, "annotations": [], "logprobs": []}})));
            let st = if reply.finish == Finish::Length { "incomplete" } else { "completed" };
            yield Ok(ev("response.output_item.done", json!({"output_index": 0, "item": message_item(&ids.msg, &reply.text, st)})));
        } else {
            for (i, (c, id)) in reply.tool_calls.iter().zip(&ids.calls).enumerate() {
                let args = c.arguments.to_string();
                yield Ok(ev("response.output_item.added", json!({"output_index": i, "item": call_item(id, c, "in_progress", "")})));
                let pieces = super::tokens::pieces(&args);
                let delay = pace.per_piece(pieces.len());
                for p in pieces {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    yield Ok(ev("response.function_call_arguments.delta", json!({"item_id": id, "output_index": i, "delta": p})));
                }
                yield Ok(ev("response.function_call_arguments.done", json!({"item_id": id, "output_index": i, "name": c.name, "arguments": args})));
                yield Ok(ev("response.output_item.done", json!({"output_index": i, "item": call_item(id, c, "completed", &args)})));
            }
        }
        let output = output_items(&reply, &ids);
        let fin = response_obj(&body, &input, &reply, &ids, created, "completed", output);
        let name = if reply.finish == Finish::Length { "response.incomplete" } else { "response.completed" };
        yield Ok(ev(name, json!({"response": fin})));
    };
    stream_response("text/event-stream; charset=utf-8", s, served)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ai/openai/v1/responses", post(create))
        .route("/ai/v1/responses", post(create))
}

const EX: &str = r#"{"model":"gpt-4o","input":"hello"}"#;
const EX_STREAM: &str = r#"{"model":"gpt-4o","input":[{"role":"user","content":[{"type":"input_text","text":"hello"}]}],"stream":true}"#;
const EX_TOOL: &str = r#"{"model":"gpt-4o","input":"What is the weather in Paris?","tools":[{"type":"function","name":"get_weather","description":"Get the current weather for a location","parameters":{"type":"object","properties":{"location":{"type":"string"}},"required":["location"]}}]}"#;

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new("/ai/openai/v1/responses", &["POST"], category::AI_OPENAI, "Responses API (output items, function calls, native SSE events with stream=true)")
            .description("input may be a string or a list of message / function_call / function_call_output items; instructions is the system prompt; text.format json_schema gives schema-valid JSON; max_output_tokens truncates (status incomplete).")
            .example(Example::post("Create a response", "/ai/openai/v1/responses").json(EX))
            .example(Example::post("Create a response (streaming)", "/ai/openai/v1/responses").json(EX_STREAM))
            .example(Example::post("Function call", "/ai/openai/v1/responses").json(EX_TOOL)),
        Endpoint::new("/ai/v1/responses", &["POST"], category::AI_OPENAI, "Responses API")
            .description("Alias of /ai/openai/v1/responses.")
            .example(Example::post("Create a response", "/ai/v1/responses").json(EX)),
    ]
}

pub fn openapi_paths() -> Value {
    let op = |id: &str| {
        json!({"post": {
            "tags": ["AI Gateway"],
            "summary": "Responses API (OpenAI-compatible)",
            "operationId": id,
            "parameters": super::common_parameters(),
            "requestBody": {"required": true, "content": {"application/json": {"schema": {"type": "object", "required": ["input"], "properties": {
                "model": {"type": "string"},
                "input": {"oneOf": [{"type": "string"}, {"type": "array", "items": {"type": "object"}}]},
                "instructions": {"type": "string"},
                "tools": {"type": "array", "items": {"type": "object"}},
                "tool_choice": {},
                "text": {"type": "object", "properties": {"format": {"type": "object"}}},
                "max_output_tokens": {"type": "integer"},
                "stream": {"type": "boolean"}
            }}}}},
            "responses": {
                "200": {"description": "Response object, or text/event-stream with response.* events", "content": {
                    "application/json": {"schema": {"type": "object", "properties": {
                        "id": {"type": "string"}, "object": {"type": "string"}, "status": {"type": "string"},
                        "output": {"type": "array", "items": {"type": "object"}}, "usage": {"type": "object"}
                    }}},
                    "text/event-stream": {"schema": {"type": "string"}}
                }},
                "400": {"description": "Invalid request"}
            }
        }})
    };
    json!({
        "/ai/openai/v1/responses": op("openaiResponses"),
        "/ai/v1/responses": op("postResponses"),
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    const URL: &str = "/ai/openai/v1/responses";

    #[tokio::test]
    async fn non_stream_message_and_function_call() {
        let app = app();
        let (s, _, b) = send(&app, URL, &serde_json::from_str(EX).expect("json"), &[]).await;
        assert_eq!(s, 200);
        let v = json(&b);
        assert_eq!(v["object"], "response");
        assert_eq!(v["status"], "completed");
        assert_eq!(v["output"][0]["type"], "message");
        assert_eq!(v["output"][0]["content"][0]["type"], "output_text");
        assert!(v["usage"]["total_tokens"].as_u64().unwrap_or(0) > 0);

        let (_, _, b) = send(
            &app,
            URL,
            &serde_json::from_str(EX_TOOL).expect("json"),
            &[],
        )
        .await;
        let v = json(&b);
        assert_eq!(v["output"][0]["type"], "function_call");
        assert_eq!(v["output"][0]["name"], "get_weather");
        let call_id = v["output"][0]["call_id"].clone();
        let follow = json!({"model": "gpt-4o", "input": [
            {"role": "user", "content": "What is the weather in Paris?"},
            v["output"][0].clone(),
            {"type": "function_call_output", "call_id": call_id, "output": "sunny, 21C"}
        ]});
        let (_, _, b) = send(&app, URL, &follow, &[]).await;
        let v = json(&b);
        assert!(v["output"][0]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("sunny, 21C"));

        let (_, _, b) = send(
            &app,
            URL,
            &json!({"input": "hello", "max_output_tokens": 2}),
            &[],
        )
        .await;
        let v = json(&b);
        assert_eq!(v["status"], "incomplete");
        assert_eq!(v["incomplete_details"]["reason"], "max_output_tokens");
    }

    #[tokio::test]
    async fn stream_event_sequence() {
        let app = app();
        let (_, _, b) = send(
            &app,
            URL,
            &serde_json::from_str(EX_STREAM).expect("json"),
            &[],
        )
        .await;
        let events = sse_events(&b);
        let mut dedup: Vec<&str> = Vec::new();
        for e in &events {
            if dedup.last() != Some(&e.as_str()) {
                dedup.push(e);
            }
        }
        assert_eq!(
            dedup,
            vec![
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        let data = sse_data(&b);
        let text: String = data
            .iter()
            .filter(|d| d["type"] == "response.output_text.delta")
            .filter_map(|d| d["delta"].as_str())
            .collect();
        assert_eq!(text, super::super::engine::GREETING);
        for (i, d) in data.iter().enumerate() {
            assert_eq!(d["sequence_number"], i);
        }

        let mut tool: Value = serde_json::from_str(EX_TOOL).expect("json");
        tool["stream"] = json!(true);
        let (_, _, b) = send(&app, URL, &tool, &[]).await;
        let events = sse_events(&b);
        assert!(events.contains(&"response.function_call_arguments.done".to_string()));
        let data = sse_data(&b);
        let args: String = data
            .iter()
            .filter(|d| d["type"] == "response.function_call_arguments.delta")
            .filter_map(|d| d["delta"].as_str())
            .collect();
        assert_eq!(
            serde_json::from_str::<Value>(&args).expect("args")["location"],
            "Paris"
        );
    }
}
