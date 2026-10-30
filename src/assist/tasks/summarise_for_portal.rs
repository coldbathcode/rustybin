//! `summarise_for_portal`: the dev portal metadata of a mock ("Write with
//! AI" in the console's Publish to portal form): a title, a short Markdown
//! summary for testers and a few tags, checked like a publish
//! (`portal::clean_meta`). The console sends the spec's title, description
//! and operations; the mock provider answers from them.

use serde_json::{json, Value};

use super::ABOUT;
use crate::assist::models::Tier;
use crate::assist::task::{context_field, text_field, truncate_bytes, Task};

pub const TASK: Task = Task {
    name: "summarise_for_portal",
    title: "Write the portal summary",
    description: "A dev portal title, summary and tags for a mock API, written for the testers who will call it",
    writes: false,
    tier: Tier::Fast,
    system: SYSTEM,
    schema: Some(schema),
    prompt,
    mock,
    cache_key: None,
    post: Some(post),
};

/// Longest summary the task writes (the portal accepts more).
const MAX_SUMMARY: usize = 600;

const SYSTEM: &str = "You write the dev portal entry of a mock API for the testers who will call \
it: a title (at most 80 characters, the API's name, no version), a summary in Markdown of two or \
three short sentences (what the API is for, what a tester can do with the mock, nothing about \
how it is built), and up to five lower-case tags (the domain and the main resources). Answer with \
one JSON object: {\"title\": \"...\", \"summary\": \"...\", \"tags\": [\"...\"]}, nothing else.";

pub fn schema() -> Value {
    json!({
        "type": "object",
        "required": ["title", "summary"],
        "properties": {
            "title": { "type": "string", "maxLength": 80 },
            "summary": { "type": "string", "maxLength": MAX_SUMMARY },
            "tags": { "type": "array", "items": { "type": "string", "maxLength": 30 }, "maxItems": 5 }
        }
    })
}

fn prompt(inputs: &Value) -> Result<String, String> {
    let title = text_field(inputs, "title", 200, true)?.unwrap_or_default();
    let description = text_field(inputs, "description", 4000, false)?;
    let operations = context_field(inputs, "operations")?;
    let hint = text_field(inputs, "hint", 500, false)?;
    Ok(format!(
        "{ABOUT}\n\nThe mock API: {title}\n{}{}{}",
        description
            .map(|d| format!("\nIts description:\n{d}\n"))
            .unwrap_or_default(),
        operations
            .map(|o| format!("\nIts operations:\n{}\n", truncate_bytes(&o, 12 * 1024)))
            .unwrap_or_default(),
        hint.map(|h| format!("\nWhat the user wants: {h}\n"))
            .unwrap_or_default(),
    ))
}

fn post(_inputs: &Value, answer: Value) -> Result<Value, String> {
    let meta: crate::portal::Meta = serde_json::from_value(json!({
        "title": answer.get("title"),
        "summary": answer.get("summary"),
        "tags": answer.get("tags").filter(|t| !t.is_null()),
    }))
    .map_err(|e| format!("not portal metadata: {e}"))?;
    let clean = crate::portal::clean_meta(&meta)?;
    if clean.title.is_none() || clean.summary.is_none() {
        return Err("the title and the summary must not be empty".into());
    }
    Ok(json!({
        "title": clean.title,
        "summary": clean.summary,
        "tags": clean.tags.unwrap_or_default(),
    }))
}

fn mock(inputs: &Value) -> Value {
    let title = inputs
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("Mock API");
    let ops: Vec<&Value> = inputs
        .get("operations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let first = inputs
        .get("description")
        .and_then(Value::as_str)
        .and_then(|d| d.split("\n\n").next())
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(|d| d.chars().take(300).collect::<String>());
    let names: Vec<String> = ops
        .iter()
        .filter_map(|o| {
            o.get("summary")
                .and_then(Value::as_str)
                .or_else(|| o.get("path").and_then(Value::as_str))
                .map(|s| s.trim().trim_end_matches('.').to_lowercase())
        })
        .filter(|s| !s.is_empty())
        .take(3)
        .collect();
    let mut summary = first.unwrap_or_else(|| format!("A mock of the {title}."));
    if !names.is_empty() {
        summary.push_str(&format!(
            "\n\n{} operations to try, for example: {}.",
            ops.len(),
            names.join(", ")
        ));
    }
    let mut tags: Vec<String> = Vec::new();
    for o in &ops {
        for t in o
            .get("tags")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(t) = t.as_str().map(|t| t.trim().to_lowercase()) {
                if !t.is_empty() && t.len() <= 30 && !tags.contains(&t) && tags.len() < 5 {
                    tags.push(t);
                }
            }
        }
    }
    json!({
        "title": title.chars().take(80).collect::<String>(),
        "summary": summary.chars().take(MAX_SUMMARY).collect::<String>(),
        "tags": tags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_answer_passes_its_own_check() {
        let inputs = json!({
            "title": "Petstore",
            "description": "A sample API.\n\nMore.",
            "operations": [
                { "method": "GET", "path": "/pets", "summary": "List pets", "tags": ["Pets"] },
                { "method": "POST", "path": "/pets", "summary": "Add a pet", "tags": ["pets", "store"] }
            ]
        });
        let answer = mock(&inputs);
        assert_eq!(answer["title"], "Petstore");
        assert!(answer["summary"]
            .as_str()
            .is_some_and(|s| s.starts_with("A sample API.") && s.contains("list pets")));
        assert_eq!(answer["tags"], json!(["pets", "store"]));
        let checked = post(&inputs, answer).expect("valid");
        assert_eq!(checked["tags"], json!(["pets", "store"]));
        assert!(post(&inputs, json!({ "title": "", "summary": "x" })).is_err());
        assert!(post(
            &inputs,
            json!({ "title": "t", "summary": "x".repeat(5000) })
        )
        .is_err());
        assert!(prompt(&inputs).expect("prompt").contains("Petstore"));
        assert!(prompt(&json!({})).is_err(), "the title is required");
    }
}
