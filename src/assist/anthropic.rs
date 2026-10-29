//! The Anthropic Messages API: `POST {base}/v1/messages` with `stream: true`.
//! A structured answer uses tool use: one tool named `answer` whose input
//! schema is the task's, forced with `tool_choice`, so the answer arrives as
//! `input_json_delta` pieces.

use serde_json::{json, Value};
use zeroize::Zeroizing;

use super::provider::{data_json, post_sse, Event, EventStream, Problem, Request};
use crate::egress::Egress;

pub const VERSION: &str = "2023-06-01";
pub const TOOL_NAME: &str = "answer";

/// The request body for `req`.
pub fn body(req: &Request) -> Value {
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_output_tokens,
        "system": req.system,
        "messages": [{ "role": "user", "content": req.user }],
        "stream": true,
    });
    if let Some(schema) = &req.schema {
        body["tools"] = json!([{
            "name": TOOL_NAME,
            "description": "The structured answer. Call it exactly once.",
            "input_schema": schema,
        }]);
        body["tool_choice"] = json!({ "type": "tool", "name": TOOL_NAME });
    }
    body
}

pub async fn complete(
    egress: &Egress,
    base: &str,
    key: &Zeroizing<String>,
    req: &Request,
) -> Result<EventStream, Problem> {
    let url = format!("{}/v1/messages", base.trim_end_matches('/'));
    let headers = vec![
        ("x-api-key", key.to_string()),
        ("anthropic-version", VERSION.to_string()),
    ];
    let mut sse = post_sse(egress, &url, headers, &body(req)).await?;
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

/// The events of one SSE payload.
pub fn events(v: &Value) -> Vec<Event> {
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "message_start" => {
            let input = v
                .pointer("/message/usage/input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            vec![Event::Usage { input, output: 0 }]
        }
        "content_block_delta" => {
            let delta = &v["delta"];
            let text = match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => delta.get("text").and_then(Value::as_str),
                Some("input_json_delta") => delta.get("partial_json").and_then(Value::as_str),
                _ => None,
            };
            text.map(|t| vec![Event::Delta(t.to_string())])
                .unwrap_or_default()
        }
        "message_delta" => {
            let output = v
                .pointer("/usage/output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let input = v
                .pointer("/usage/input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            vec![Event::Usage { input, output }]
        }
        "error" => {
            let message = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("the provider reported an error");
            vec![Event::Error(message.to_string())]
        }
        _ => Vec::new(),
    }
}
