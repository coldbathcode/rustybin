//! `edit_spec`: the whole document after an instruction. The answer is
//! parsed and checked, must differ from the original, and may not remove
//! more than 40 percent of the operations unless the instruction says so.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::{check_spec, compile_doc, ABOUT};
use crate::assist::models::Tier;
use crate::assist::task::{text_field, Task, MAX_TEXT_CHARS};

pub const TASK: Task = Task {
    name: "edit_spec",
    title: "Edit with AI",
    description: "The whole OpenAPI document after an instruction, parsed and checked, with a bounded structural diff",
    writes: false,
    tier: Tier::Capable,
    system: SYSTEM,
    schema: Some(super::spec_from_description::schema),
    prompt,
    mock,
    cache_key: None,
    post: Some(post),
};

const SYSTEM: &str = "You edit OpenAPI documents for mock APIs. Apply the instruction and answer \
with the complete, updated document as one JSON object, nothing else. Keep the OpenAPI version, \
every operationId and everything the instruction does not touch exactly as it is; add examples \
to new success responses; never remove operations unless the instruction asks for it.";

/// The share of operations an edit may remove without an explicit ask.
pub const MAX_REMOVED_SHARE: f64 = 0.4;

/// What changed between two documents, by operation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffSummary {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
    pub info_changed: bool,
    pub components_changed: bool,
}

impl DiffSummary {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
            && !self.info_changed
            && !self.components_changed
    }

    pub fn describe(&self) -> Value {
        json!({
            "added": self.added,
            "removed": self.removed,
            "changed": self.changed,
            "info_changed": self.info_changed,
            "components_changed": self.components_changed,
        })
    }
}

/// The operations of a document: `operationId` (or `METHOD path`) to the
/// operation object.
fn operations(doc: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let Some(paths) = doc.get("paths").and_then(Value::as_object) else {
        return out;
    };
    for (path, item) in paths {
        let Some(item) = item.as_object() else {
            continue;
        };
        for (method, op) in item {
            if !matches!(
                method.as_str(),
                "get" | "put" | "post" | "delete" | "options" | "head" | "patch" | "trace"
            ) {
                continue;
            }
            let id = op
                .get("operationId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("{} {path}", method.to_uppercase()));
            out.insert(id, op.clone());
        }
    }
    out
}

/// Compare two documents.
pub fn diff(before: &Value, after: &Value) -> DiffSummary {
    let (a, b) = (operations(before), operations(after));
    let mut d = DiffSummary::default();
    for (id, op) in &b {
        match a.get(id) {
            None => d.added.push(id.clone()),
            Some(prev) if prev != op => d.changed.push(id.clone()),
            Some(_) => {}
        }
    }
    for id in a.keys() {
        if !b.contains_key(id) {
            d.removed.push(id.clone());
        }
    }
    d.info_changed = before.get("info") != after.get("info");
    d.components_changed = before.get("components") != after.get("components")
        || before.get("definitions") != after.get("definitions");
    d
}

fn asks_for_removal(instruction: &str) -> bool {
    let l = instruction.to_ascii_lowercase();
    ["remove", "delete", "drop", "take out", "get rid"]
        .iter()
        .any(|w| l.contains(w))
}

fn prompt(inputs: &Value) -> Result<String, String> {
    let doc = inputs
        .get("spec")
        .ok_or("inputs.spec (the OpenAPI document) is required")?;
    let instruction = text_field(inputs, "instruction", MAX_TEXT_CHARS, true)?.unwrap_or_default();
    let compiled = compile_doc(doc)?;
    let version = doc
        .get("openapi")
        .or_else(|| doc.get("swagger"))
        .and_then(Value::as_str)
        .unwrap_or("3.0.3");
    Ok(format!(
        "{ABOUT}\n\nThe document (version {version}, {} operations):\n{}\n\nInstruction: {instruction}\n\nAnswer with the whole updated document.",
        compiled.spec.operations.len(),
        crate::assist::task::truncate_bytes(&doc.to_string(), 48 * 1024)
    ))
}

fn post(inputs: &Value, answer: Value) -> Result<Value, String> {
    let before = inputs.get("spec").ok_or("inputs.spec is required")?;
    let instruction = inputs
        .get("instruction")
        .and_then(Value::as_str)
        .unwrap_or("");
    let before_version = before
        .get("openapi")
        .or_else(|| before.get("swagger"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let after_version = answer
        .get("openapi")
        .or_else(|| answer.get("swagger"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if before_version.split('.').take(2).collect::<Vec<_>>()
        != after_version.split('.').take(2).collect::<Vec<_>>()
    {
        return Err(format!(
            "the version changed from {before_version} to {after_version}: keep it"
        ));
    }
    check_spec(&answer)?;
    let d = diff(before, &answer);
    if d.is_empty() {
        return Err("the document is unchanged: apply the instruction".to_string());
    }
    let total = operations(before).len().max(1);
    if !d.removed.is_empty()
        && !asks_for_removal(instruction)
        && d.removed.len() as f64 > total as f64 * MAX_REMOVED_SHARE
    {
        return Err(format!(
            "the edit removes {} of {total} operations ({}) but the instruction does not ask to remove anything: keep them",
            d.removed.len(),
            d.removed.join(", ")
        ));
    }
    Ok(answer)
}

/// The deterministic edit: the instruction lands in the description; a
/// `404` is added where asked; a `DELETE` is added for a named resource.
fn mock(inputs: &Value) -> Value {
    let mut doc = inputs.get("spec").cloned().unwrap_or(json!({}));
    let instruction = inputs
        .get("instruction")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let lower = instruction.to_ascii_lowercase();
    let swagger = doc.get("swagger").is_some();
    let note = format!("AI edit (mock): {instruction}");
    match doc
        .get("info")
        .and_then(Value::as_object)
        .and_then(|i| i.get("description"))
    {
        Some(Value::String(d)) => doc["info"]["description"] = json!(format!("{d}\n\n{note}")),
        _ => doc["info"]["description"] = json!(note),
    }
    let not_found = if swagger {
        json!({ "description": "Not found", "schema": { "type": "object", "properties": { "title": { "type": "string" }, "status": { "type": "integer" } } }, "examples": { "application/json": { "title": "Not Found", "status": 404 } } })
    } else {
        json!({ "description": "Not found", "content": { "application/json": { "schema": { "type": "object", "properties": { "title": { "type": "string" }, "status": { "type": "integer" } } }, "example": { "title": "Not Found", "status": 404 } } } })
    };
    if let Some(paths) = doc.get_mut("paths").and_then(Value::as_object_mut) {
        let path_keys: Vec<String> = paths.keys().cloned().collect();
        if lower.contains("404") {
            for item in paths.values_mut() {
                let Some(item) = item.as_object_mut() else {
                    continue;
                };
                for (method, op) in item.iter_mut() {
                    if !matches!(method.as_str(), "get" | "put" | "post" | "delete" | "patch") {
                        continue;
                    }
                    if let Some(responses) = op.get_mut("responses").and_then(Value::as_object_mut)
                    {
                        responses.entry("404").or_insert_with(|| not_found.clone());
                    }
                }
            }
        }
        if lower.contains("delete") {
            for key in &path_keys {
                let is_item = key.ends_with('}');
                let mentioned = key
                    .trim_matches('/')
                    .split('/')
                    .any(|seg| seg.len() > 2 && !seg.starts_with('{') && lower.contains(seg));
                if is_item && mentioned {
                    if let Some(item) = paths.get_mut(key).and_then(Value::as_object_mut) {
                        if !item.contains_key("delete") {
                            let params = item
                                .get("get")
                                .and_then(|g| g.get("parameters"))
                                .cloned()
                                .unwrap_or(json!([]));
                            let id = format!(
                                "delete{}",
                                key.trim_matches('/')
                                    .split('/')
                                    .filter(|s| !s.starts_with('{'))
                                    .map(|s| {
                                        let mut c = s.chars();
                                        c.next()
                                            .map(|f| {
                                                f.to_uppercase().collect::<String>() + c.as_str()
                                            })
                                            .unwrap_or_default()
                                    })
                                    .collect::<String>()
                            );
                            item.insert("delete".into(), json!({
                                "operationId": id,
                                "summary": "Delete (added by the mock assistant)",
                                "parameters": params,
                                "responses": { "204": { "description": "Deleted" }, "404": not_found.clone() }
                            }));
                        }
                    }
                }
            }
        }
    }
    doc
}
