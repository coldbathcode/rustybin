//! Gemini `generateContent`, streamed:
//! `POST {base}/v1beta/models/{model}:streamGenerateContent?alt=sse` with the
//! key in the `x-goog-api-key` header (never in the URL). A structured answer
//! uses `generationConfig.responseSchema`, a subset of JSON Schema: the
//! keywords it does not accept are stripped first.

use serde_json::{json, Map, Value};
use zeroize::Zeroizing;

use super::provider::{data_json, post_sse, Event, EventStream, Problem, Request};
use crate::egress::Egress;

/// Keywords Gemini's `responseSchema` rejects.
const UNSUPPORTED: &[&str] = &[
    "$schema",
    "$id",
    "$ref",
    "$defs",
    "definitions",
    "additionalProperties",
    "examples",
    "example",
    "default",
    "title",
    "pattern",
    "patternProperties",
    "const",
    "minLength",
    "maxLength",
    "format",
    "if",
    "then",
    "else",
    "not",
    "oneOf",
    "allOf",
];

/// A JSON Schema reduced to what `responseSchema` accepts.
pub fn response_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                if UNSUPPORTED.contains(&k.as_str()) {
                    continue;
                }
                let v = match k.as_str() {
                    "properties" => match v {
                        Value::Object(props) => Value::Object(
                            props
                                .iter()
                                .map(|(name, s)| (name.clone(), response_schema(s)))
                                .collect(),
                        ),
                        other => other.clone(),
                    },
                    "items" => response_schema(v),
                    "anyOf" => match v {
                        Value::Array(items) => {
                            Value::Array(items.iter().map(response_schema).collect())
                        }
                        other => other.clone(),
                    },
                    "type" => match v {
                        // ["string", "null"] -> string plus nullable.
                        Value::Array(types) => {
                            let non_null: Vec<&Value> = types
                                .iter()
                                .filter(|t| t.as_str() != Some("null"))
                                .collect();
                            if non_null.len() < types.len() {
                                out.insert("nullable".into(), json!(true));
                            }
                            non_null
                                .first()
                                .map(|t| (*t).clone())
                                .unwrap_or(json!("string"))
                        }
                        other => other.clone(),
                    },
                    _ => v.clone(),
                };
                out.insert(k.clone(), v);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// The request body for `req`.
pub fn body(req: &Request) -> Value {
    let mut generation = json!({ "maxOutputTokens": req.max_output_tokens });
    if let Some(schema) = &req.schema {
        generation["responseMimeType"] = json!("application/json");
        generation["responseSchema"] = response_schema(schema);
    }
    json!({
        "systemInstruction": { "parts": [{ "text": req.system }] },
        "contents": [{ "role": "user", "parts": [{ "text": req.user }] }],
        "generationConfig": generation,
    })
}

pub async fn complete(
    egress: &Egress,
    base: &str,
    key: &Zeroizing<String>,
    req: &Request,
) -> Result<EventStream, Problem> {
    let model: String = req
        .model
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
        .collect();
    let url = format!(
        "{}/v1beta/models/{model}:streamGenerateContent?alt=sse",
        base.trim_end_matches('/')
    );
    let headers = vec![("x-goog-api-key", key.to_string())];
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

/// The events of one chunk.
pub fn events(v: &Value) -> Vec<Event> {
    let mut out = Vec::new();
    if let Some(message) = v.pointer("/error/message").and_then(Value::as_str) {
        out.push(Event::Error(message.to_string()));
        return out;
    }
    if let Some(parts) = v
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
    {
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    out.push(Event::Delta(text.to_string()));
                }
            }
        }
    }
    if let Some(reason) = v
        .pointer("/promptFeedback/blockReason")
        .and_then(Value::as_str)
    {
        out.push(Event::Error(format!("the prompt was blocked ({reason})")));
    }
    if let Some(usage) = v.get("usageMetadata").filter(|u| u.is_object()) {
        out.push(Event::Usage {
            input: usage
                .get("promptTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output: usage
                .get("candidatesTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        });
    }
    out
}
