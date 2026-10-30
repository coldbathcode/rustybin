//! A task is the only way to call a model: a name, a system prompt, a user
//! prompt rendered from validated inputs, an optional output JSON schema
//! (validated, repaired on failure), a deterministic mock answer, and for
//! some tasks a cache key. The API, the console, the CLI and the MCP tools
//! call tasks; nothing builds prompts elsewhere.

use serde_json::{json, Map, Value};

use super::models::Tier;

/// Largest free-text input (characters).
pub const MAX_TEXT_CHARS: usize = 8_000;
/// Largest context document (bytes of its JSON).
pub const MAX_CONTEXT_BYTES: usize = 32 * 1024;
/// The views the console has (for `open_view` and the suggestions).
pub const VIEWS: &[&str] = &[
    "overview", "traffic", "mocks", "bins", "explorer", "manage", "ai", "mcp", "registry", "a2a",
    "chaos", "harness", "tokens",
];

/// A task's post-check: `(inputs, answer) -> transformed answer | repair message`.
pub type PostCheck = fn(&Value, Value) -> Result<Value, String>;

#[derive(Clone, Copy)]
pub struct Task {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// The task changes the instance (needs admin). None in this step.
    pub writes: bool,
    /// Which configured model answers it by default.
    pub tier: Tier,
    pub system: &'static str,
    /// The output schema of a structured task (`None`: free text, which may
    /// carry action blocks).
    pub schema: Option<fn() -> Value>,
    /// Validates the inputs and renders the user prompt.
    pub prompt: fn(&Value) -> Result<String, String>,
    /// The deterministic answer of the mock provider (a string for text
    /// tasks, the structured value otherwise).
    pub mock: fn(&Value) -> Value,
    /// Answers of a task with a cache key are kept for an hour per key and model.
    pub cache_key: Option<fn(&Value) -> Option<String>>,
    /// A check beyond the schema, with the inputs at hand (parse a spec, run
    /// the deploy checks, validate examples against their schemas). It may
    /// return a transformed answer; `Err` goes back to the model as a repair.
    pub post: Option<PostCheck>,
}

impl Task {
    pub fn describe(&self) -> Value {
        json!({
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "writes": self.writes,
            "tier": self.tier.as_str(),
            "structured": self.schema.is_some(),
            "cached": self.cache_key.is_some(),
        })
    }
}

/// Every task, in display order.
pub fn all() -> &'static [Task] {
    super::tasks::ALL
}

pub fn find(name: &str) -> Option<&'static Task> {
    all().iter().find(|t| t.name == name)
}

// ── Input helpers ───────────────────────────────────────────────────

/// A string input, trimmed, at most `max_chars` characters.
pub fn text_field(
    inputs: &Value,
    name: &str,
    max_chars: usize,
    required: bool,
) -> Result<Option<String>, String> {
    match inputs.get(name) {
        None | Some(Value::Null) => {
            if required {
                Err(format!("inputs.{name} is required"))
            } else {
                Ok(None)
            }
        }
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() && required {
                return Err(format!("inputs.{name} is empty"));
            }
            if s.chars().count() > max_chars {
                return Err(format!(
                    "inputs.{name} is longer than {max_chars} characters"
                ));
            }
            Ok(Some(s.to_string()))
        }
        Some(_) => Err(format!("inputs.{name} must be a string")),
    }
}

/// A context document: an object or array rendered as pretty JSON, or a
/// string; truncated to [`MAX_CONTEXT_BYTES`].
pub fn context_field(inputs: &Value, name: &str) -> Result<Option<String>, String> {
    let text = match inputs.get(name) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(s)) => s.clone(),
        Some(other) => serde_json::to_string_pretty(other).map_err(|e| e.to_string())?,
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(truncate_bytes(&text, MAX_CONTEXT_BYTES)))
}

/// Cut a string at a character boundary, marking the cut.
pub fn truncate_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated]", &text[..end])
}

/// A view id (one of [`VIEWS`], else `None`).
pub fn view_field(inputs: &Value) -> Option<&'static str> {
    let raw = inputs.get("view").and_then(Value::as_str)?.trim();
    VIEWS.iter().copied().find(|v| *v == raw)
}

// ── Actions in text answers ─────────────────────────────────────────

/// The actions a text answer may propose, each as a fenced block:
///
/// ````text
/// ```action
/// {"action": "open_view", "label": "Open Live traffic", "payload": {"view": "traffic"}}
/// ```
/// ````
pub const ACTIONS: &[&str] = &[
    "open_view",
    "copy",
    "run_request",
    "create_mock",
    "edit_spec",
    "generate_examples",
];

fn valid_action(v: &Value) -> Option<Value> {
    let action = v.get("action")?.as_str()?;
    if !ACTIONS.contains(&action) {
        return None;
    }
    let payload = v.get("payload")?.as_object()?;
    let label = v
        .get("label")
        .and_then(Value::as_str)
        .map(|l| l.chars().take(80).collect::<String>());
    let clean: Map<String, Value> = match action {
        "open_view" => {
            let view = payload.get("view")?.as_str()?;
            if !VIEWS.contains(&view) {
                return None;
            }
            let mut m = Map::new();
            m.insert("view".into(), json!(view));
            m
        }
        "copy" => {
            let text = payload.get("text")?.as_str()?;
            if text.is_empty() || text.chars().count() > MAX_TEXT_CHARS {
                return None;
            }
            let mut m = Map::new();
            m.insert("text".into(), json!(text));
            m
        }
        "create_mock" => {
            // Opens the wizard's Describe tab with the description and runs it.
            let description = payload.get("description")?.as_str()?;
            if description.is_empty() || description.chars().count() > MAX_TEXT_CHARS {
                return None;
            }
            let mut m = Map::new();
            m.insert("description".into(), json!(description));
            if let Some(v) = payload.get("openapi_version").and_then(Value::as_str) {
                if matches!(v, "3.1.0" | "3.0.3" | "2.0") {
                    m.insert("openapi_version".into(), json!(v));
                }
            }
            m
        }
        "edit_spec" => {
            let mock = payload.get("mock")?.as_str()?;
            let instruction = payload.get("instruction")?.as_str()?;
            if !crate::mocks::model::valid_name(mock)
                || instruction.is_empty()
                || instruction.chars().count() > MAX_TEXT_CHARS
            {
                return None;
            }
            let mut m = Map::new();
            m.insert("mock".into(), json!(mock));
            m.insert("instruction".into(), json!(instruction));
            m
        }
        "generate_examples" => {
            let mock = payload.get("mock")?.as_str()?;
            if !crate::mocks::model::valid_name(mock) {
                return None;
            }
            let mut m = Map::new();
            m.insert("mock".into(), json!(mock));
            m
        }
        "run_request" => {
            let method = payload
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("GET")
                .to_ascii_uppercase();
            if !matches!(
                method.as_str(),
                "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
            ) {
                return None;
            }
            let path = payload.get("path")?.as_str()?;
            if !path.starts_with('/') || path.starts_with("//") || path.len() > 2048 {
                return None;
            }
            let mut m = Map::new();
            m.insert("method".into(), json!(method));
            m.insert("path".into(), json!(path));
            if let Some(Value::Object(h)) = payload.get("headers") {
                let headers: Map<String, Value> = h
                    .iter()
                    .filter(|(k, v)| v.is_string() && k.len() <= 128)
                    .take(16)
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                m.insert("headers".into(), Value::Object(headers));
            }
            match payload.get("body") {
                None | Some(Value::Null) => {}
                Some(Value::String(s)) if s.len() <= MAX_CONTEXT_BYTES => {
                    m.insert("body".into(), json!(s));
                }
                Some(other) if other.to_string().len() <= MAX_CONTEXT_BYTES => {
                    m.insert("body".into(), other.clone());
                }
                Some(_) => return None,
            }
            m
        }
        _ => return None,
    };
    Some(json!({ "action": action, "label": label, "payload": clean }))
}

/// Split a text answer into its prose and its valid action blocks.
pub fn extract_actions(text: &str) -> (String, Vec<Value>) {
    let mut prose = String::new();
    let mut actions = Vec::new();
    let mut rest = text;
    loop {
        let Some(start) = rest.find("```action") else {
            prose.push_str(rest);
            break;
        };
        prose.push_str(&rest[..start]);
        let after = &rest[start + "```action".len()..];
        let Some(end) = after.find("```") else {
            // An unterminated block: keep it as prose.
            prose.push_str(&rest[start..]);
            break;
        };
        let block = after[..end].trim();
        if let Some(a) = serde_json::from_str::<Value>(block)
            .ok()
            .and_then(|v| valid_action(&v))
        {
            actions.push(a);
        }
        rest = &after[end + 3..];
    }
    (prose.trim().to_string(), actions)
}

/// Parse a structured answer: JSON as is, or the first JSON object inside
/// prose or a code fence.
pub fn parse_json_answer(text: &str) -> Result<Value, String> {
    let trimmed = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Ok(v);
    }
    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(str::trim);
    if let Some(inner) = unfenced {
        if let Ok(v) = serde_json::from_str::<Value>(inner) {
            return Ok(v);
        }
    }
    let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) else {
        return Err("the answer holds no JSON object".to_string());
    };
    if end <= start {
        return Err("the answer holds no JSON object".to_string());
    }
    serde_json::from_str::<Value>(&trimmed[start..=end]).map_err(|e| format!("invalid JSON: {e}"))
}

/// Validate an answer against the task's schema; the issues as one line.
pub fn validate(value: &Value, schema: &Value) -> Result<(), String> {
    let issues = crate::mocks::schema::validate(
        value,
        schema,
        schema,
        crate::mocks::schema::Direction::Response,
    );
    if issues.is_empty() {
        return Ok(());
    }
    Err(issues
        .iter()
        .take(8)
        .map(|i| {
            if i.pointer.is_empty() {
                i.message.clone()
            } else {
                format!("{}: {}", i.pointer, i.message)
            }
        })
        .collect::<Vec<_>>()
        .join("; "))
}
