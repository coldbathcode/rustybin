//! `explain`: a captured request, an MCP tool, a spec operation or any
//! text, explained as a summary, details, gateway notes and next steps.

use serde_json::{json, Value};

use super::ABOUT;
use crate::assist::models::Tier;
use crate::assist::task::{context_field, text_field, Task};

pub const TASK: Task = Task {
    name: "explain",
    title: "Explain",
    description: "Explain a captured request, an MCP tool, a spec operation or a piece of text",
    writes: false,
    tier: Tier::Fast,
    system: SYSTEM,
    schema: Some(schema),
    prompt,
    mock,
    cache_key: None,
};

const SYSTEM: &str = "You explain one thing the user selected in the Rustybin console: a request \
a gateway forwarded (its method, path, headers and body), an MCP tool, an operation of an \
OpenAPI spec, or a piece of text. Say what it is and what it does, point out anything a gateway \
added or changed (authentication, correlation and rate-limit headers, rewritten paths), and \
suggest the next steps a tester would take. Be concrete and short.";

pub const KINDS: &[&str] = &["request", "tool", "operation", "text"];

pub fn schema() -> Value {
    json!({
        "type": "object",
        "required": ["summary", "details"],
        "additionalProperties": false,
        "properties": {
            "summary": { "type": "string", "description": "One or two sentences" },
            "details": { "type": "array", "items": { "type": "string" }, "maxItems": 12 },
            "gateway_notes": { "type": "array", "items": { "type": "string" }, "maxItems": 8,
                "description": "What a gateway added or changed, when visible" },
            "next_steps": { "type": "array", "items": { "type": "string" }, "maxItems": 6 }
        }
    })
}

fn kind(inputs: &Value) -> Result<&'static str, String> {
    let raw = text_field(inputs, "kind", 20, false)?.unwrap_or_else(|| "text".to_string());
    KINDS
        .iter()
        .copied()
        .find(|k| *k == raw)
        .ok_or_else(|| format!("inputs.kind must be one of {}", KINDS.join(", ")))
}

fn prompt(inputs: &Value) -> Result<String, String> {
    let kind = kind(inputs)?;
    let subject = context_field(inputs, "subject")?.ok_or("inputs.subject is required")?;
    Ok(format!(
        "{ABOUT}\n\nExplain this {kind}:\n\n{subject}\n\nAnswer with the summary, the details, \
         the gateway notes (if any) and the next steps."
    ))
}

fn mock(inputs: &Value) -> Value {
    let kind = kind(inputs).unwrap_or("text");
    let subject = inputs.get("subject").cloned().unwrap_or(Value::Null);
    let method = subject.get("method").and_then(Value::as_str);
    let path = subject.get("path").and_then(Value::as_str);
    let summary = match (kind, method, path) {
        ("request", Some(m), Some(p)) => {
            format!("A {m} request to {p}, as this instance received it (mock explanation).")
        }
        ("tool", _, _) => format!(
            "The MCP tool {} (mock explanation).",
            subject
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("selected")
        ),
        ("operation", _, _) => "An operation of the mock's OpenAPI spec (mock explanation).".into(),
        _ => "The selected text, explained by the built-in provider (mock explanation).".into(),
    };
    let mut gateway_notes = Vec::new();
    if let Some(headers) = subject.get("headers") {
        let names: Vec<String> = match headers {
            Value::Object(m) => m.keys().cloned().collect(),
            Value::Array(items) => items
                .iter()
                .filter_map(|h| h.get(0).or_else(|| h.get("name")).and_then(Value::as_str))
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        for name in names {
            let lower = name.to_ascii_lowercase();
            if lower.starts_with("x-forwarded")
                || lower.starts_with("x-real-ip")
                || lower.starts_with("x-kong")
                || lower.starts_with("x-request-id")
                || lower.starts_with("x-correlation")
                || lower.starts_with("ratelimit")
                || lower.starts_with("x-ratelimit")
                || lower == "via"
            {
                gateway_notes.push(format!("{name} looks like a header a gateway added."));
            }
        }
    }
    json!({
        "summary": summary,
        "details": [
            "No model was called: the built-in provider answers deterministically.",
            format!("Kind: {kind}."),
            "Add a key under Management, AI for a real explanation.",
        ],
        "gateway_notes": gateway_notes,
        "next_steps": [
            "Send the same request through the gateway and compare the headers in Live traffic.",
            "Inject a fault with X-Rustybin-Fail: 503 to see how the gateway handles it.",
        ],
    })
}
