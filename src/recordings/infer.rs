//! A spec inferred by code from recorded exchanges: deterministic, no key
//! needed. Exchanges are grouped into operations (method plus a path
//! template whose varying segments are parameters), and the schemas of
//! parameters, request bodies and responses come from the JSON samples.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::Exchange;

/// Most samples a schema is inferred from per operation and status.
const MAX_SAMPLES: usize = 50;
/// Most stored examples kept per operation and status.
pub const EXAMPLES_PER_STATUS: usize = 2;
/// Most distinct values of a string field listed as an `enum`.
const MAX_ENUM: usize = 5;

/// Is a path segment a value rather than a name (a number, a UUID, a long
/// hex or a token with digits)?
pub fn is_value_segment(seg: &str) -> bool {
    if seg.is_empty() {
        return false;
    }
    let digits = seg.chars().filter(char::is_ascii_digit).count();
    let uuid = seg.len() == 36 && seg.chars().filter(|c| *c == '-').count() == 4;
    let hex = seg.len() >= 8 && seg.chars().all(|c| c.is_ascii_hexdigit());
    seg.chars().all(|c| c.is_ascii_digit())
        || uuid
        || hex
        || (seg.len() >= 6
            && digits >= 2
            && seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

/// The name of the parameter after `prev` (`pets` gives `petId`).
fn param_name(prev: Option<&str>, taken: &[String]) -> String {
    let base = match prev {
        Some(p) if !p.is_empty() && !p.starts_with('{') => {
            let singular = p
                .strip_suffix("ies")
                .map(|s| format!("{s}y"))
                .unwrap_or_else(|| p.strip_suffix('s').unwrap_or(p).to_string());
            let clean: String = singular
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect();
            if clean.is_empty() {
                "id".to_string()
            } else {
                format!("{clean}Id")
            }
        }
        _ => "id".to_string(),
    };
    let mut name = base.clone();
    let mut n = 2;
    while taken.contains(&name) {
        name = format!("{base}{n}");
        n += 1;
    }
    name
}

/// A path's template and its parameter names.
pub fn template(path: &str) -> (String, Vec<String>) {
    let mut out = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    for (i, seg) in segs.iter().enumerate() {
        if is_value_segment(seg) {
            let name = param_name(i.checked_sub(1).and_then(|j| segs.get(j)).copied(), &names);
            out.push(format!("{{{name}}}"));
            names.push(name);
        } else {
            out.push((*seg).to_string());
        }
    }
    (format!("/{}", out.join("/")), names)
}

/// A string's format, when it has a common one.
fn string_format(s: &str) -> Option<&'static str> {
    let b = s.as_bytes();
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let date = s.len() >= 10
        && (0..4).all(digit)
        && b[4] == b'-'
        && digit(5)
        && digit(6)
        && b[7] == b'-'
        && digit(8)
        && digit(9);
    if date && s.len() == 10 {
        return Some("date");
    }
    if date && s.len() > 10 && (b[10] == b'T' || b[10] == b' ') {
        return Some("date-time");
    }
    if s.len() == 36
        && s.chars().filter(|c| *c == '-').count() == 4
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
    {
        return Some("uuid");
    }
    if let Some((user, host)) = s.split_once('@') {
        if !user.is_empty() && host.contains('.') && !s.contains(' ') {
            return Some("email");
        }
    }
    if s.starts_with("https://") || s.starts_with("http://") {
        return Some("uri");
    }
    None
}

/// The schema of one value.
pub fn schema_of(v: &Value) -> Value {
    match v {
        Value::Null => json!({ "nullable": true }),
        Value::Bool(_) => json!({ "type": "boolean" }),
        Value::Number(n) if n.is_i64() || n.is_u64() => json!({ "type": "integer" }),
        Value::Number(_) => json!({ "type": "number" }),
        Value::String(s) => match string_format(s) {
            Some(f) => json!({ "type": "string", "format": f }),
            None => json!({ "type": "string" }),
        },
        Value::Array(items) => {
            let item = merge_all(items.iter().take(MAX_SAMPLES).map(schema_of));
            json!({ "type": "array", "items": item.unwrap_or_else(|| json!({})) })
        }
        Value::Object(m) => {
            let mut props = Map::new();
            for (k, v) in m {
                props.insert(k.clone(), schema_of(v));
            }
            let required: Vec<&String> = m.keys().collect();
            json!({ "type": "object", "properties": props, "required": required })
        }
    }
}

fn merge_all(mut schemas: impl Iterator<Item = Value>) -> Option<Value> {
    let first = schemas.next()?;
    Some(schemas.fold(first, |a, b| merge(&a, &b)))
}

/// Two schemas as one: properties joined, required only when in both,
/// integer and number as number, null as nullable, other conflicts as the
/// first with a note.
pub fn merge(a: &Value, b: &Value) -> Value {
    let ty = |s: &Value| s.get("type").and_then(Value::as_str).map(str::to_string);
    let nullable = |s: &Value| s.get("nullable") == Some(&json!(true));
    // A null on either side.
    match (ty(a), ty(b)) {
        (None, Some(_)) if nullable(a) => {
            let mut m = b.clone();
            m["nullable"] = json!(true);
            return m;
        }
        (Some(_), None) if nullable(b) => {
            let mut m = a.clone();
            m["nullable"] = json!(true);
            return m;
        }
        _ => {}
    }
    let mut out = match (ty(a).as_deref(), ty(b).as_deref()) {
        (Some("object"), Some("object")) => {
            let pa = a["properties"].as_object().cloned().unwrap_or_default();
            let pb = b["properties"].as_object().cloned().unwrap_or_default();
            let mut props = Map::new();
            for (k, v) in &pa {
                props.insert(
                    k.clone(),
                    match pb.get(k) {
                        Some(w) => merge(v, w),
                        None => v.clone(),
                    },
                );
            }
            for (k, v) in &pb {
                props.entry(k.clone()).or_insert_with(|| v.clone());
            }
            let req = |s: &Value| -> Vec<String> {
                s["required"]
                    .as_array()
                    .map(|r| {
                        r.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let rb = req(b);
            let required: Vec<String> = req(a).into_iter().filter(|k| rb.contains(k)).collect();
            json!({ "type": "object", "properties": props, "required": required })
        }
        (Some("array"), Some("array")) => {
            json!({ "type": "array", "items": merge(&a["items"], &b["items"]) })
        }
        (Some("integer"), Some("number")) | (Some("number"), Some("integer")) => {
            json!({ "type": "number" })
        }
        (Some("string"), Some("string")) => match (a.get("format"), b.get("format")) {
            (Some(x), Some(y)) if x == y => a.clone(),
            _ => {
                let mut s = json!({ "type": "string" });
                if let Some(e) = a.get("enum") {
                    s["enum"] = e.clone();
                }
                s
            }
        },
        (Some(x), Some(y)) if x == y => a.clone(),
        (None, None) => a.clone(),
        _ => {
            let mut m = a.clone();
            m["description"] = json!("Recorded with more than one type; the first one seen");
            m
        }
    };
    if nullable(a) || nullable(b) {
        out["nullable"] = json!(true);
    }
    out
}

/// A query or path value's schema.
fn scalar_schema(values: &[&str]) -> Value {
    if !values.is_empty() && values.iter().all(|v| v.parse::<i64>().is_ok()) {
        json!({ "type": "integer" })
    } else if !values.is_empty() && values.iter().all(|v| v.parse::<f64>().is_ok()) {
        json!({ "type": "number" })
    } else if !values.is_empty() && values.iter().all(|v| *v == "true" || *v == "false") {
        json!({ "type": "boolean" })
    } else {
        let mut distinct: Vec<&str> = values.to_vec();
        distinct.sort();
        distinct.dedup();
        if distinct.len() > 1 && distinct.len() <= MAX_ENUM && values.len() >= 2 * distinct.len() {
            json!({ "type": "string", "enum": distinct })
        } else {
            json!({ "type": "string" })
        }
    }
}

/// The JSON body of an exchange side, when it is JSON.
fn json_body(body: &Option<String>, content_type: &Option<String>) -> Option<Value> {
    let ct = content_type.as_deref().unwrap_or("");
    if !ct.contains("json") && !ct.is_empty() {
        return None;
    }
    serde_json::from_str(body.as_deref()?).ok()
}

fn operation_id(method: &str, template: &str, taken: &mut Vec<String>) -> String {
    let mut id = method.to_ascii_lowercase();
    for seg in template.split('/').filter(|s| !s.is_empty()) {
        let (by, word) = match seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            Some(p) => ("By", p),
            None => ("", seg),
        };
        id.push_str(by);
        let mut up = true;
        for c in word.chars() {
            if c.is_ascii_alphanumeric() {
                if up {
                    id.extend(c.to_uppercase());
                } else {
                    id.push(c);
                }
                up = false;
            } else {
                up = true;
            }
        }
    }
    let mut unique = id.clone();
    let mut n = 2;
    while taken.contains(&unique) {
        unique = format!("{id}{n}");
        n += 1;
    }
    taken.push(unique.clone());
    unique
}

/// What the inference made.
pub struct Inferred {
    /// The OpenAPI 3.0 document.
    pub spec: Value,
    /// Stored examples per operationId and status.
    pub examples: BTreeMap<String, BTreeMap<String, Vec<Value>>>,
    /// One line per operation: `GET /pets/{petId}` and its statuses.
    pub operations: Vec<Value>,
}

/// An operation's key: its path template and its method.
type OpKey = (String, String);

/// Infer a spec from `exchanges`, titled after `upstream`.
pub fn infer(exchanges: &[Exchange], upstream: &str) -> Inferred {
    // (template, method) -> the exchanges, in order of first appearance.
    let mut order: Vec<(String, String)> = Vec::new();
    let mut groups: BTreeMap<OpKey, (Vec<String>, Vec<&Exchange>)> = BTreeMap::new();
    for e in exchanges {
        let (t, names) = template(&e.path);
        let key = (t, e.method.to_ascii_lowercase());
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups
            .entry(key)
            .or_insert_with(|| (names, Vec::new()))
            .1
            .push(e);
    }
    let mut paths = Map::new();
    let mut examples = BTreeMap::new();
    let mut operations = Vec::new();
    let mut taken = Vec::new();
    for key in &order {
        let (names, list) = &groups[key];
        let (template, method) = key;
        let id = operation_id(method, template, &mut taken);
        let mut parameters = Vec::new();
        // Path parameters.
        for (i, name) in names.iter().enumerate() {
            let values: Vec<&str> = list
                .iter()
                .filter_map(|e| {
                    e.path
                        .split('/')
                        .filter(|s| !s.is_empty())
                        .filter(|s| is_value_segment(s))
                        .nth(i)
                })
                .collect();
            parameters.push(json!({ "name": name, "in": "path", "required": true, "schema": scalar_schema(&values) }));
        }
        // Query parameters: required when every exchange has them.
        let mut query: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for e in list {
            for (k, v) in &e.query {
                query.entry(k.as_str()).or_default().push(v.as_str());
            }
        }
        for (k, values) in &query {
            let required = list.iter().all(|e| e.query.iter().any(|(q, _)| q == k));
            let plain: Vec<&str> = values
                .iter()
                .copied()
                .filter(|v| !v.starts_with("{{"))
                .collect();
            parameters.push(json!({ "name": k, "in": "query", "required": required, "schema": scalar_schema(&plain) }));
        }
        let mut op = json!({ "operationId": id, "summary": format!("{} {template} (recorded)", method.to_ascii_uppercase()) });
        if !parameters.is_empty() {
            op["parameters"] = json!(parameters);
        }
        // The request body.
        let bodies: Vec<Value> = list
            .iter()
            .filter_map(|e| json_body(&e.request_body, &e.request_type))
            .take(MAX_SAMPLES)
            .collect();
        if let Some(schema) = merge_all(bodies.iter().map(schema_of)) {
            op["requestBody"] = json!({ "required": true, "content": { "application/json": { "schema": schema } } });
        }
        // The responses, by status.
        let mut by_status: BTreeMap<u16, Vec<&Exchange>> = BTreeMap::new();
        for e in list {
            by_status.entry(e.status).or_default().push(e);
        }
        let mut responses = Map::new();
        let mut op_examples = BTreeMap::new();
        for (status, es) in &by_status {
            let samples: Vec<Value> = es
                .iter()
                .filter_map(|e| json_body(&e.response_body, &e.response_type))
                .take(MAX_SAMPLES)
                .collect();
            let mut resp = json!({ "description": format!("Recorded {status}") });
            if let Some(schema) = merge_all(samples.iter().map(schema_of)) {
                resp["content"] = json!({ "application/json": { "schema": schema } });
                op_examples.insert(
                    status.to_string(),
                    samples
                        .iter()
                        .take(EXAMPLES_PER_STATUS)
                        .cloned()
                        .collect::<Vec<_>>(),
                );
            } else if es
                .iter()
                .any(|e| e.response_body.as_deref().is_some_and(|b| !b.is_empty()))
            {
                let ct = es
                    .iter()
                    .find_map(|e| e.response_type.clone())
                    .unwrap_or_else(|| "text/plain".into());
                let ct = ct
                    .split(';')
                    .next()
                    .unwrap_or("text/plain")
                    .trim()
                    .to_string();
                resp["content"] = json!({ ct: { "schema": { "type": "string" } } });
            }
            responses.insert(status.to_string(), resp);
        }
        op["responses"] = Value::Object(responses);
        operations.push(json!({
            "operation": id,
            "method": method.to_ascii_uppercase(),
            "path": template,
            "exchanges": list.len(),
            "statuses": by_status.keys().collect::<Vec<_>>(),
        }));
        if !op_examples.is_empty() {
            examples.insert(id.clone(), op_examples);
        }
        let entry = paths.entry(template.clone()).or_insert_with(|| json!({}));
        entry[method.as_str()] = op;
    }
    let spec = json!({
        "openapi": "3.0.3",
        "info": {
            "title": format!("Recorded from {}", host_of(upstream)),
            "version": "1.0.0",
            "description": format!("Inferred by Rustybin from traffic recorded in front of {upstream}. Review the examples before sharing: they are what the real API answered."),
        },
        "paths": paths,
    });
    Inferred {
        spec,
        examples,
        operations,
    }
}

fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| url.to_string())
}

/// The kinds of personal data a text seems to hold (emails, phone numbers,
/// card numbers), each once, never the values.
pub fn personal_data(text: &str) -> Vec<&'static str> {
    static EMAIL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static PHONE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static CARD: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let email = EMAIL.get_or_init(|| {
        regex::Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").expect("email")
    });
    let phone =
        PHONE.get_or_init(|| regex::Regex::new(r"\+[0-9][0-9 ().-]{6,}[0-9]").expect("phone"));
    let card = CARD.get_or_init(|| regex::Regex::new(r"\b(?:[0-9][ -]?){13,19}\b").expect("card"));
    let mut kinds = Vec::new();
    if email.is_match(text) {
        kinds.push("email");
    }
    if phone.is_match(text) {
        kinds.push("phone number");
    }
    if card.find_iter(text).any(|m| luhn(m.as_str())) {
        kinds.push("card number");
    }
    kinds
}

fn luhn(s: &str) -> bool {
    let digits: Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| {
            if i % 2 == 1 {
                let x = d * 2;
                if x > 9 {
                    x - 9
                } else {
                    x
                }
            } else {
                *d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(method: &str, path: &str, status: u16, body: &str) -> Exchange {
        Exchange {
            method: method.into(),
            path: path.into(),
            status,
            response_type: Some("application/json".into()),
            response_body: Some(body.into()),
            ..Exchange::default()
        }
    }

    #[test]
    fn varying_segments_become_parameters_named_after_the_collection() {
        assert_eq!(template("/pets/1").0, "/pets/{petId}");
        assert_eq!(template("/pets/abc").0, "/pets/abc");
        assert_eq!(
            template("/categories/4f3c2a1b/items/42").0,
            "/categories/{categoryId}/items/{itemId}"
        );
        assert_eq!(template("/v1/orders/ord_12ab34").0, "/v1/orders/{orderId}");
        assert_eq!(template("/").0, "/");
    }

    #[test]
    fn schemas_merge_and_name_what_is_always_there() {
        let a = schema_of(&json!({ "id": 1, "name": "Rex", "born": "2020-01-02", "tag": null }));
        let b = schema_of(&json!({ "id": 2.5, "name": "Tom", "tag": "x" }));
        let m = merge(&a, &b);
        assert_eq!(m["properties"]["id"]["type"], "number");
        assert_eq!(m["properties"]["born"]["format"], "date");
        assert_eq!(m["properties"]["tag"]["type"], "string");
        assert_eq!(m["properties"]["tag"]["nullable"], true);
        let req: Vec<&str> = m["required"]
            .as_array()
            .expect("req")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(req, ["id", "name", "tag"]);
    }

    #[test]
    fn exchanges_become_operations_with_examples() {
        let es = vec![
            ex("GET", "/pets/1", 200, r#"{"id":1,"name":"Rex"}"#),
            ex("GET", "/pets/2", 200, r#"{"id":2,"name":"Tom"}"#),
            ex("GET", "/pets/9", 404, r#"{"error":"not found"}"#),
            ex("GET", "/pets", 200, r#"[{"id":1,"name":"Rex"}]"#),
        ];
        let i = infer(&es, "https://api.example.com");
        let op = &i.spec["paths"]["/pets/{petId}"]["get"];
        assert_eq!(op["operationId"], "getPetsByPetId");
        assert_eq!(op["parameters"][0]["schema"]["type"], "integer");
        assert!(op["responses"]["404"].is_object());
        assert_eq!(
            i.examples["getPetsByPetId"]["200"].len(),
            EXAMPLES_PER_STATUS
        );
        assert_eq!(
            i.spec["paths"]["/pets"]["get"]["responses"]["200"]["content"]["application/json"]
                ["schema"]["type"],
            "array"
        );
        assert_eq!(i.operations.len(), 2);
        assert_eq!(i.spec["info"]["title"], "Recorded from api.example.com");
    }

    #[test]
    fn personal_data_is_named_never_shown() {
        assert_eq!(personal_data("write to jane.doe@example.com"), ["email"]);
        assert_eq!(personal_data("call +1 555 0100 123"), ["phone number"]);
        assert_eq!(personal_data("card 4111 1111 1111 1111"), ["card number"]);
        assert!(personal_data("order 1234567890123 shipped").is_empty());
    }
}
