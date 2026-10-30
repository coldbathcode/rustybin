//! `tool_arguments`: arguments for an MCP tool, valid against its
//! inputSchema ("Generate arguments" in the MCP inspector, and the checks'
//! `ai_arguments`). The mock provider answers with the schema generator.

use serde_json::{json, Value};

use super::ABOUT;
use crate::assist::models::Tier;
use crate::assist::task::{truncate_bytes, Task};
use crate::mocks::schema::{self, Direction, GenOptions};

pub const TASK: Task = Task {
    name: "tool_arguments",
    title: "Generate arguments",
    description: "Arguments for an MCP tool, valid against its inputSchema",
    writes: false,
    tier: Tier::Fast,
    system: SYSTEM,
    schema: Some(schema),
    prompt,
    mock,
    cache_key: None,
    post: Some(post),
};

const SYSTEM: &str = "You write the arguments of one call to an MCP tool: realistic values a \
tester would use, valid against the tool's inputSchema (required properties, types, enums, \
formats, bounds). Answer with one JSON object: {\"arguments\": {...}}, nothing else.";

pub fn schema() -> Value {
    json!({
        "type": "object",
        "required": ["arguments"],
        "properties": { "arguments": { "type": "object" } }
    })
}

fn tool(inputs: &Value) -> Result<(&Value, &Value), String> {
    let tool = inputs
        .get("tool")
        .filter(|t| t.is_object())
        .ok_or("inputs.tool (name, description, inputSchema) is required")?;
    let input = tool
        .get("inputSchema")
        .filter(|s| s.is_object())
        .ok_or("inputs.tool.inputSchema must be an object")?;
    if tool.to_string().len() > 64 * 1024 {
        return Err("the tool is larger than 64 KiB".into());
    }
    Ok((tool, input))
}

fn prompt(inputs: &Value) -> Result<String, String> {
    let (tool, input) = tool(inputs)?;
    let hint = inputs
        .get("hint")
        .and_then(Value::as_str)
        .map(|h| h.chars().take(500).collect::<String>());
    Ok(format!(
        "{ABOUT}\n\nThe tool: {} ({}).\nIts inputSchema:\n{}\n{}",
        tool.get("name").and_then(Value::as_str).unwrap_or("?"),
        tool.get("description")
            .and_then(Value::as_str)
            .unwrap_or("no description"),
        truncate_bytes(&input.to_string(), 24 * 1024),
        hint.map(|h| format!("\nWhat the user wants to try: {h}\n"))
            .unwrap_or_default(),
    ))
}

fn post(inputs: &Value, answer: Value) -> Result<Value, String> {
    let (_, input) = tool(inputs)?;
    let args = answer.get("arguments").ok_or("arguments is missing")?;
    let issues = schema::validate(args, input, input, Direction::Request);
    if issues.is_empty() {
        Ok(answer)
    } else {
        Err(issues
            .iter()
            .take(5)
            .map(|i| format!("{} {}", i.pointer, i.message))
            .collect::<Vec<_>>()
            .join("; "))
    }
}

/// Arguments from the schema generator.
pub fn generate(input: &Value, seed: u64) -> Value {
    let v = schema::generate(
        input,
        &GenOptions {
            root: input,
            direction: Direction::Request,
            seed,
            use_examples: true,
            echo: &[],
        },
    );
    if v.is_object() {
        v
    } else {
        json!({})
    }
}

fn mock(inputs: &Value) -> Value {
    match tool(inputs) {
        Ok((_, input)) => json!({ "arguments": generate(input, 7) }),
        Err(_) => json!({ "arguments": {} }),
    }
}
