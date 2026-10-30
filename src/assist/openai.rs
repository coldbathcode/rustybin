//! OpenAI Chat Completions, `POST {base}/chat/completions` with `stream:
//! true` and `stream_options.include_usage`, and any OpenAI-compatible
//! endpoint through the same code. A structured answer uses
//! `response_format: json_schema` on OpenAI; a compatible endpoint gets
//! `json_object` plus the schema in the prompt, and `max_tokens` instead of
//! `max_completion_tokens`.

use serde_json::{json, Value};
use zeroize::Zeroizing;

use super::provider::{data_json, post_sse, Event, EventStream, Problem, Request};
use crate::egress::Egress;

pub const SCHEMA_NAME: &str = "answer";

/// The request body for `req`.
pub fn body(req: &Request, compatible: bool) -> Value {
    let mut user = req.user.clone();
    let mut body = json!({
        "model": req.model,
        "messages": [
            { "role": "system", "content": req.system },
        ],
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if compatible {
        body["max_tokens"] = json!(req.max_output_tokens);
    } else {
        body["max_completion_tokens"] = json!(req.max_output_tokens);
        if is_reasoning_model(&req.model) {
            // Reasoning models spend output tokens thinking before they write;
            // the tasks here are structured jobs, so keep the thinking short or
            // a small budget (the key test's) comes back empty.
            body["reasoning_effort"] = json!("low");
        }
    }
    if let Some(schema) = &req.schema {
        if compatible {
            body["response_format"] = json!({ "type": "json_object" });
            user.push_str(
                "\n\nAnswer with one JSON object and nothing else, valid against this JSON Schema:\n",
            );
            user.push_str(&schema.to_string());
        } else {
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": { "name": SCHEMA_NAME, "schema": schema, "strict": false },
            });
        }
    }
    if let Some(m) = body["messages"].as_array_mut() {
        m.push(json!({ "role": "user", "content": user }));
    }
    body
}

pub async fn complete(
    egress: &Egress,
    base: &str,
    key: Option<&Zeroizing<String>>,
    compatible: bool,
    req: &Request,
) -> Result<EventStream, Problem> {
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let mut headers = Vec::new();
    if let Some(key) = key {
        headers.push(("authorization", format!("Bearer {}", key.as_str())));
    }
    let mut sse = post_sse(egress, &url, headers, &body(req, compatible)).await?;
    let stream = async_stream::stream! {
        use futures_util::StreamExt;
        while let Some(item) = sse.next().await {
            let ev = match item {
                Ok(ev) => ev,
                Err(e) => {
                    yield Event::Error(e);
                    return;
                }
            };
            let Some(v) = data_json(&ev) else { continue };
            for out in events(&v) {
                yield out;
            }
        }
    };
    Ok(Box::pin(stream))
}

/// OpenAI's reasoning models (`gpt-5*`, `o1*`, `o3*`, `o4*`): they take
/// `reasoning_effort` and count their thinking against the output budget.
pub fn is_reasoning_model(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    m.starts_with("gpt-5") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4")
}

/// The message when a stream ends with `finish_reason: length`: the answer is
/// cut (or, with a reasoning model, never started), so it is an error, not a
/// shorter answer.
pub const RAN_OUT: &str = "the model hit its output token limit before finishing the answer \
(a reasoning model spends part of the budget thinking): raise RUSTYBIN_ASSIST_MAX_OUTPUT_TOKENS, \
pick a smaller job, or a model that does not reason";

/// The events of one chunk.
pub fn events(v: &Value) -> Vec<Event> {
    let mut out = Vec::new();
    if let Some(message) = v.pointer("/error/message").and_then(Value::as_str) {
        out.push(Event::Error(message.to_string()));
        return out;
    }
    if let Some(text) = v
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
    {
        if !text.is_empty() {
            out.push(Event::Delta(text.to_string()));
        }
    }
    if v.pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        == Some("length")
    {
        out.push(Event::Error(RAN_OUT.to_string()));
    }
    if let Some(usage) = v.get("usage").filter(|u| u.is_object()) {
        out.push(Event::Usage {
            input: usage
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output: usage
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        });
    }
    out
}
