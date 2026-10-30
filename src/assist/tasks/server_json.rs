//! `server_json`: a `server.json` registry entry for one of the instance's
//! MCP servers ("Write a server.json for my mock" on the MCP registry
//! view), valid against the vendored schema. The console sends the entry
//! the mock registry generates as the draft, with the server's tools; the
//! mock provider answers with the draft, tidied.

use serde_json::{json, Value};

use super::ABOUT;
use crate::assist::models::Tier;
use crate::assist::task::{context_field, text_field, truncate_bytes, Task};
use crate::mcp::registry::{entries, validate};

pub const TASK: Task = Task {
    name: "server_json",
    title: "Write a server.json",
    description: "A server.json registry entry for an MCP server of this instance, valid against the registry schema",
    writes: false,
    tier: Tier::Fast,
    system: SYSTEM,
    schema: Some(schema),
    prompt,
    mock,
    cache_key: None,
    post: Some(post),
};

const SYSTEM: &str = "You write the server.json entry an MCP registry lists for one MCP server: \
a reverse-DNS name (namespace/server), a title, a description of at most 100 characters that \
says what the tools do, an exact version, the remotes a client connects to (keep the URLs and \
headers of the draft) and, when it helps, a websiteUrl. Keep the draft's $schema. Answer with \
one JSON object: {\"server\": {...}, \"notes\": [\"...\"]}, nothing else.";

pub fn schema() -> Value {
    json!({
        "type": "object",
        "required": ["server"],
        "properties": {
            "server": { "type": "object" },
            "notes": { "type": "array", "items": { "type": "string" }, "maxItems": 6 }
        }
    })
}

fn draft(inputs: &Value) -> Result<&Value, String> {
    let d = inputs
        .get("draft")
        .filter(|d| d.is_object())
        .ok_or("inputs.draft (the server.json the mock registry generates) is required")?;
    if d.to_string().len() > validate::MAX_DOC_BYTES {
        return Err("the draft is larger than 64 KiB".into());
    }
    Ok(d)
}

fn prompt(inputs: &Value) -> Result<String, String> {
    let d = draft(inputs)?;
    let tools = context_field(inputs, "tools")?;
    let hint = text_field(inputs, "hint", 500, false)?;
    Ok(format!(
        "{ABOUT}\n\nThe draft entry:\n{}\n{}{}\nThe document must validate against {}.",
        truncate_bytes(
            &serde_json::to_string_pretty(d).unwrap_or_default(),
            16 * 1024
        ),
        tools
            .map(|t| format!("\nThe server's tools:\n{}\n", truncate_bytes(&t, 16 * 1024)))
            .unwrap_or_default(),
        hint.map(|h| format!("\nWhat the user wants: {h}\n"))
            .unwrap_or_default(),
        validate::SCHEMA_URL,
    ))
}

fn post(_inputs: &Value, answer: Value) -> Result<Value, String> {
    let server = answer.get("server").ok_or("server is missing")?;
    let errors = validate::errors(&validate::validate(server));
    if errors.is_empty() {
        Ok(answer)
    } else {
        Err(errors.into_iter().take(6).collect::<Vec<_>>().join("; "))
    }
}

fn mock(inputs: &Value) -> Value {
    let Ok(d) = draft(inputs) else {
        return json!({ "server": {}, "notes": ["No draft was sent."] });
    };
    let mut server = d.clone();
    if let Some(m) = server.as_object_mut() {
        m.entry("$schema").or_insert(json!(validate::SCHEMA_URL));
        // The registry's own metadata is not the publisher's to write.
        m.remove("_meta");
        let tools: Vec<&str> = inputs
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .take(4)
            .collect();
        if !tools.is_empty() {
            let title = m
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("MCP server")
                .to_string();
            m.insert(
                "description".into(),
                json!(entries::clip(
                    &format!("{title}: {}", tools.join(", ")),
                    100
                )),
            );
        }
    }
    json!({
        "server": server,
        "notes": [
            "No model was called: the built-in provider tidies the generated draft.",
            "Publishing under io.rustybin is refused: pick your own namespace before you publish.",
        ],
    })
}
