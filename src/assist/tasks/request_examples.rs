//! `request_examples`: request bodies and parameter sets an operation
//! accepts (for "Generate arguments" on an MCP tool, and for sample
//! requests). Validated against the operation's parameter and body schemas.

use serde_json::{json, Map, Value};

use super::{compile_doc, ABOUT};
use crate::assist::models::Tier;
use crate::assist::task::{text_field, Task};
use crate::mocks::schema::{self, Direction, GenOptions};
use crate::mocks::spec::Location;

pub const TASK: Task = Task {
    name: "request_examples",
    title: "Generate requests",
    description:
        "Request bodies and parameter sets an operation accepts, validated against its schemas",
    writes: false,
    tier: Tier::Fast,
    system: SYSTEM,
    schema: Some(schema),
    prompt,
    mock,
    cache_key: None,
    post: Some(post),
};

const SYSTEM: &str = "You write example requests for one operation of an OpenAPI mock: realistic \
values for its path, query and header parameters and, where it takes one, a request body valid \
against its schema. Answer with one JSON object: {\"requests\": [{\"path\": {...}, \"query\": \
{...}, \"headers\": {...}, \"body\": ...}, ...]}, nothing else.";

pub const MAX_REQUESTS: usize = 5;

pub fn schema() -> Value {
    json!({
        "type": "object",
        "required": ["requests"],
        "properties": {
            "requests": {
                "type": "array", "minItems": 1, "maxItems": MAX_REQUESTS,
                "items": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "object" },
                        "query": { "type": "object" },
                        "headers": { "type": "object" },
                        "body": {}
                    }
                }
            }
        }
    })
}

fn count(inputs: &Value) -> usize {
    inputs
        .get("count")
        .and_then(Value::as_u64)
        .map(|n| n.clamp(1, MAX_REQUESTS as u64) as usize)
        .unwrap_or(2)
}

/// The compiled spec and the operation named by `inputs.operation`.
fn target(inputs: &Value) -> Result<(crate::mocks::compile::Compiled, String), String> {
    let doc = inputs
        .get("spec")
        .ok_or("inputs.spec (the OpenAPI document) is required")?;
    let op = text_field(inputs, "operation", 256, true)?.unwrap_or_default();
    let compiled = compile_doc(doc)?;
    if compiled.spec.operation(&op).is_none() {
        return Err(format!("inputs.operation: no operation {op:?} in the spec"));
    }
    Ok((compiled, op))
}

fn prompt(inputs: &Value) -> Result<String, String> {
    let (compiled, id) = target(inputs)?;
    let op = compiled.spec.operation(&id).ok_or("no operation")?;
    let n = count(inputs);
    let op_json = serde_json::to_value(op).unwrap_or(Value::Null);
    let inlined =
        crate::mocks::service::inline_refs(&op_json, &compiled.spec.root, &mut Vec::new(), 0);
    Ok(format!(
        "{ABOUT}\n\nThe API: {} {}.\nWrite {n} request(s) for this operation:\n{}\n",
        compiled.spec.title,
        compiled.spec.version,
        crate::assist::task::truncate_bytes(&inlined.to_string(), 24 * 1024)
    ))
}

fn post(inputs: &Value, answer: Value) -> Result<Value, String> {
    let (compiled, id) = target(inputs)?;
    let op = compiled.spec.operation(&id).ok_or("no operation")?;
    let requests = answer
        .get("requests")
        .and_then(Value::as_array)
        .ok_or("requests must be an array")?;
    let root = &compiled.spec.root;
    let mut problems = Vec::new();
    for (i, r) in requests.iter().enumerate() {
        for p in &op.parameters {
            let group = match p.location {
                Location::Path => "path",
                Location::Query => "query",
                Location::Header => "headers",
                Location::Cookie => "cookies",
            };
            let given = r.get(group).and_then(|g| g.get(&p.name));
            match given {
                None if p.required => {
                    problems.push(format!("request {i}: {group}.{} is required", p.name))
                }
                None => {}
                Some(v) => {
                    // Parameter values arrive as strings in HTTP: coerce like a request would.
                    let coerced = match v {
                        Value::String(s) => schema::coerce(s, &p.schema, root),
                        other => other.clone(),
                    };
                    let issues = schema::validate(&coerced, &p.schema, root, Direction::Request);
                    if !issues.is_empty() {
                        problems.push(format!(
                            "request {i}: {group}.{}: {}",
                            p.name, issues[0].message
                        ));
                    }
                }
            }
        }
        if let Some(body_def) = &op.request_body {
            match (
                r.get("body"),
                body_def.json_media().and_then(|m| m.schema.as_ref()),
            ) {
                (None | Some(Value::Null), _) if body_def.required => {
                    problems.push(format!("request {i}: a body is required"))
                }
                (Some(body), Some(s)) => {
                    let issues = schema::validate(body, s, root, Direction::Request);
                    if !issues.is_empty() {
                        problems.push(format!(
                            "request {i}: body: {}",
                            issues
                                .iter()
                                .take(3)
                                .map(|x| format!("{} {}", x.pointer, x.message))
                                .collect::<Vec<_>>()
                                .join("; ")
                        ));
                    }
                }
                _ => {}
            }
        }
    }
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    Ok(answer)
}

/// Deterministic requests from the schemas.
pub fn generate(inputs: &Value) -> Result<Value, String> {
    let (compiled, id) = target(inputs)?;
    let op = compiled.spec.operation(&id).ok_or("no operation")?;
    let root = &compiled.spec.root;
    let n = count(inputs);
    let requests: Vec<Value> = (0..n)
        .map(|i| {
            let opts = |seed: u64| GenOptions {
                root,
                direction: Direction::Request,
                seed,
                use_examples: true,
                echo: &[],
            };
            let mut groups: Map<String, Value> = Map::new();
            for p in &op.parameters {
                let group = match p.location {
                    Location::Path => "path",
                    Location::Query => "query",
                    Location::Header => "headers",
                    Location::Cookie => "cookies",
                };
                if !p.required && i % 2 == 1 {
                    continue;
                }
                let v = p
                    .example
                    .clone()
                    .unwrap_or_else(|| schema::generate(&p.schema, &opts(500 + i as u64)));
                groups
                    .entry(group)
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .map(|g| g.insert(p.name.clone(), v));
            }
            let mut r = Value::Object(groups);
            if let Some(body) = &op.request_body {
                if let Some(media) = body.json_media() {
                    let v = media
                        .example
                        .clone()
                        .or_else(|| media.examples.first().map(|e| e.value.clone()));
                    let v = v.or_else(|| {
                        media
                            .schema
                            .as_ref()
                            .map(|s| schema::generate(s, &opts(700 + i as u64)))
                    });
                    if let Some(v) = v {
                        r["body"] = v;
                    }
                }
            }
            r
        })
        .collect();
    Ok(json!({ "requests": requests }))
}

fn mock(inputs: &Value) -> Value {
    generate(inputs).unwrap_or_else(|_| json!({ "requests": [] }))
}
