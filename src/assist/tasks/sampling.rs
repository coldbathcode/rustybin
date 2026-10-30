//! `sampling`: answer an MCP server's `sampling/createMessage` request with
//! the user's provider (the MCP inspector asks before it sends anything).

use serde_json::Value;

use crate::assist::models::Tier;
use crate::assist::task::{truncate_bytes, Task};

pub const TASK: Task = Task {
    name: "sampling",
    title: "Answer a sampling request",
    description: "The reply to an MCP server's sampling/createMessage request, from its messages and system prompt",
    writes: false,
    tier: Tier::Fast,
    system: SYSTEM,
    schema: None,
    prompt,
    mock,
    cache_key: None,
    post: None,
};

const SYSTEM: &str = "An MCP server asked its client to sample a language model. You are that \
model. Answer the conversation below as the assistant, following the server's system prompt \
when there is one, briefly.";

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Object(_) => content
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "[{} content]",
                    content
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("non-text")
                )
            }),
        Value::Array(items) => items.iter().map(text_of).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn prompt(inputs: &Value) -> Result<String, String> {
    let messages = inputs
        .get("messages")
        .and_then(Value::as_array)
        .filter(|m| !m.is_empty())
        .ok_or("inputs.messages (the sampling request's messages) is required")?;
    if messages.len() > 50 {
        return Err("at most 50 messages".into());
    }
    let mut out = String::new();
    if let Some(sp) = inputs.get("system_prompt").and_then(Value::as_str) {
        out.push_str(&format!(
            "The server's system prompt: {}\n\n",
            truncate_bytes(sp, 4 * 1024)
        ));
    }
    for m in messages {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
        out.push_str(&format!(
            "{role}: {}\n",
            truncate_bytes(&text_of(m.get("content").unwrap_or(&Value::Null)), 8 * 1024)
        ));
    }
    Ok(truncate_bytes(&out, 32 * 1024))
}

fn mock(inputs: &Value) -> Value {
    let last = inputs
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|m| m.last())
        .map(|m| text_of(m.get("content").unwrap_or(&Value::Null)))
        .unwrap_or_default();
    Value::String(format!(
        "Mock model reply (the built-in provider, nothing left the instance) to: {}",
        last.chars().take(200).collect::<String>()
    ))
}
