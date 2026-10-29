//! `examples_for_operations`: realistic example payloads for the success
//! responses of operations that have none. Every example is validated
//! against its response schema; the failures go back to the model per
//! operation. The mock provider answers with the deterministic generator.

use serde_json::{json, Map, Value};

use super::{compile_doc, ABOUT};
use crate::assist::models::Tier;
use crate::assist::task::{text_field, Task};
use crate::mocks::schema::{self, Direction, GenOptions};

pub const TASK: Task = Task {
    name: "examples_for_operations",
    title: "Generate examples",
    description: "Example payloads for the success responses of operations that lack them, validated against their schemas",
    writes: false,
    tier: Tier::Capable,
    system: SYSTEM,
    schema: Some(schema),
    prompt,
    mock,
    cache_key: None,
    post: Some(post),
};

const SYSTEM: &str = "You write example response bodies for an OpenAPI mock. For every operation \
and status listed, write the number of examples asked for: realistic, varied, consistent with \
each other (the same ids and names across operations where it makes sense), and valid against \
the response schema given, including required properties, enums, formats and bounds. Answer \
with one JSON object: {\"examples\": {\"<operationId>\": {\"<status>\": [example, ...]}}}, nothing \
else.";

/// Most operations one call covers (the route batches).
pub const BATCH: usize = 10;
pub const MAX_PER_RESPONSE: usize = 3;

pub fn schema() -> Value {
    json!({
        "type": "object",
        "required": ["examples"],
        "properties": {
            "examples": {
                "type": "object",
                "additionalProperties": {
                    "type": "object",
                    "additionalProperties": { "type": "array", "minItems": 1, "maxItems": MAX_PER_RESPONSE }
                }
            }
        }
    })
}

/// The operations to cover: `(id, [(status, schema)])`, from the inputs
/// (`operations`: ids, or every operation lacking examples) and the spec.
/// An operation and the `(status, schema)` pairs that need examples.
type Target = (String, Vec<(u16, Value)>);

fn targets(inputs: &Value) -> Result<(crate::mocks::compile::Compiled, Vec<Target>), String> {
    let doc = inputs
        .get("spec")
        .ok_or("inputs.spec (the OpenAPI document) is required")?;
    let compiled = compile_doc(doc)?;
    let wanted: Option<Vec<String>> = inputs.get("operations").and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    });
    let mut out = Vec::new();
    for op in &compiled.spec.operations {
        let listed = wanted.as_ref().is_none_or(|w| w.contains(&op.id));
        if !listed {
            continue;
        }
        let statuses: Vec<u16> = match &wanted {
            // Named operations: every success response with a schema.
            Some(_) => op
                .responses
                .iter()
                .filter_map(|r| r.code())
                .filter(|c| (200..300).contains(c))
                .collect(),
            None => crate::mocks::checks::missing_examples(&compiled, op),
        };
        let mut pairs = Vec::new();
        for status in statuses {
            let Some(def) = op.response_for(status) else {
                continue;
            };
            let Some(media) = def.content.iter().find(|m| m.is_json()) else {
                continue;
            };
            let Some(s) = &media.schema else { continue };
            pairs.push((status, s.clone()));
        }
        if !pairs.is_empty() {
            out.push((op.id.clone(), pairs));
        }
        if out.len() >= BATCH {
            break;
        }
    }
    if out.is_empty() {
        return Err("no operation needs examples (every success response has one, or none has a JSON schema)".to_string());
    }
    Ok((compiled, out))
}

fn per_response(inputs: &Value) -> usize {
    inputs
        .get("per_response")
        .and_then(Value::as_u64)
        .map(|n| n.clamp(1, MAX_PER_RESPONSE as u64) as usize)
        .unwrap_or(1)
}

fn prompt(inputs: &Value) -> Result<String, String> {
    let (compiled, targets) = targets(inputs)?;
    let n = per_response(inputs);
    let persona = text_field(inputs, "persona", 200, false)?
        .unwrap_or_else(|| "realistic data for a demo".to_string());
    let mut out = format!(
        "{ABOUT}\n\nThe API: {} {} ({}).\nStyle of the data: {persona}.\nWrite {n} example(s) per response.\n\nOperations and the response schemas (resolved):\n",
        compiled.spec.title, compiled.spec.version,
        compiled.spec.description.as_deref().unwrap_or("no description")
    );
    for (id, pairs) in &targets {
        let op = compiled.spec.operation(id);
        out.push_str(&format!(
            "\n- {id}: {} {}{}\n",
            op.map(|o| o.method.as_str()).unwrap_or("?"),
            op.map(|o| o.path.as_str()).unwrap_or("?"),
            op.and_then(|o| o.summary.as_deref())
                .map(|s| format!(" ({s})"))
                .unwrap_or_default()
        ));
        for (status, s) in pairs {
            let inlined =
                crate::mocks::service::inline_refs(s, &compiled.spec.root, &mut Vec::new(), 0);
            out.push_str(&format!(
                "  {status}: {}\n",
                crate::assist::task::truncate_bytes(&inlined.to_string(), 6 * 1024)
            ));
        }
    }
    Ok(out)
}

/// Validate every example against its schema; drop nothing, report all.
fn post(inputs: &Value, answer: Value) -> Result<Value, String> {
    let (compiled, targets) = targets(inputs)?;
    let examples = answer
        .get("examples")
        .and_then(Value::as_object)
        .ok_or("examples must be an object keyed by operationId")?;
    let mut problems = Vec::new();
    let mut out = Map::new();
    for (id, pairs) in &targets {
        let Some(by_status) = examples.get(id).and_then(Value::as_object) else {
            problems.push(format!("{id}: no examples"));
            continue;
        };
        let mut statuses = Map::new();
        for (status, schema_v) in pairs {
            let Some(list) = by_status.get(&status.to_string()).and_then(Value::as_array) else {
                problems.push(format!("{id}/{status}: no examples"));
                continue;
            };
            let mut good = Vec::new();
            for (i, ex) in list.iter().enumerate() {
                let issues =
                    schema::validate(ex, schema_v, &compiled.spec.root, Direction::Response);
                if issues.is_empty() {
                    good.push(ex.clone());
                } else {
                    problems.push(format!(
                        "{id}/{status} example {i}: {}",
                        issues
                            .iter()
                            .take(3)
                            .map(|x| format!("{} {}", x.pointer, x.message))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ));
                }
            }
            if !good.is_empty() {
                statuses.insert(status.to_string(), Value::Array(good));
            }
        }
        if !statuses.is_empty() {
            out.insert(id.clone(), Value::Object(statuses));
        }
    }
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    Ok(json!({ "examples": out }))
}

/// Deterministic examples from the schemas (the mock provider, and "Use
/// generated values" without AI).
pub fn generate(inputs: &Value) -> Result<Value, String> {
    let (compiled, targets) = targets(inputs)?;
    let n = per_response(inputs);
    let mut out = Map::new();
    for (id, pairs) in &targets {
        let mut statuses = Map::new();
        for (status, schema_v) in pairs {
            let list: Vec<Value> = (0..n)
                .map(|i| {
                    schema::generate(
                        schema_v,
                        &GenOptions {
                            root: &compiled.spec.root,
                            direction: Direction::Response,
                            seed: 1000 + i as u64,
                            use_examples: true,
                            echo: &[],
                        },
                    )
                })
                .collect();
            statuses.insert(status.to_string(), Value::Array(list));
        }
        out.insert(id.clone(), Value::Object(statuses));
    }
    Ok(json!({ "examples": out }))
}

fn mock(inputs: &Value) -> Value {
    generate(inputs).unwrap_or_else(|_| json!({ "examples": {} }))
}
