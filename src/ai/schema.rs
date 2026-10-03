//! Generate a JSON value that is valid against a JSON Schema (best effort).
//!
//! Used for tool-call arguments and structured output (OpenAI
//! `response_format`, Responses `text.format`, Gemini `responseSchema`,
//! Anthropic `output_format` and tool-forced output, Ollama `format`).
//!
//! Handles `type` (also arrays of types and Gemini's upper-case names),
//! `object`/`properties`/`required`, `array`/`items`/`prefixItems`/`minItems`/
//! `maxItems`, `string` formats and length bounds, `number`/`integer` bounds,
//! `boolean`, `null`, `enum`, `const`, `default`, `anyOf`/`oneOf`/`allOf` and
//! local `$ref`s (`#/$defs/...`, `#/definitions/...`). Strings are chosen
//! from the property name ("email", "city", "date", ...) so tool arguments
//! look plausible.

use serde_json::{json, Map, Value};

/// Context used to pick plausible values.
#[derive(Clone, Debug, Default)]
pub struct Hints {
    /// Last user message (used for query-like string properties).
    pub user_text: Option<String>,
    /// A capitalised entity found in the user text (e.g. a city after "in").
    pub entity: Option<String>,
}

impl Hints {
    pub fn from_user_text(text: &str) -> Self {
        Self {
            user_text: Some(text.chars().take(200).collect()),
            entity: extract_entity(text),
        }
    }
}

/// A capitalised word following "in", "for", "at", "to", "from" or "about"
/// (e.g. "What is the weather in Paris?" -> "Paris").
pub fn extract_entity(text: &str) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    for w in words.windows(2) {
        let prep = w[0].to_lowercase();
        if !matches!(
            prep.as_str(),
            "in" | "for" | "at" | "to" | "from" | "about" | "near"
        ) {
            continue;
        }
        let cand: String = w[1]
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-')
            .collect();
        if cand.chars().next().is_some_and(|c| c.is_uppercase()) {
            return Some(cand);
        }
    }
    None
}

const MAX_DEPTH: usize = 12;

/// Generate a value for `schema`. `root` resolves `$ref`s; `name` is the
/// property name (or "" at the top level).
pub fn generate(schema: &Value, hints: &Hints) -> Value {
    let mut g = Gen {
        root: schema,
        hints,
        budget: MAX_NODES,
    };
    g.gen(schema, "", 0)
}

/// Upper bound on generated nodes (recursive schemas cannot blow up).
const MAX_NODES: usize = 2_000;

struct Gen<'a> {
    root: &'a Value,
    hints: &'a Hints,
    budget: usize,
}

fn resolve<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let pointer = reference.strip_prefix('#')?;
    root.pointer(pointer)
}

fn type_names(schema: &Value) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.to_ascii_lowercase()],
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_ascii_lowercase)
            .collect(),
        _ => Vec::new(),
    }
}

impl Gen<'_> {
    fn gen(&mut self, schema: &Value, name: &str, depth: usize) -> Value {
        if depth > MAX_DEPTH || self.budget == 0 {
            return Value::Null;
        }
        self.budget -= 1;
        let root = self.root;
        let hints = self.hints;
        let Some(obj) = schema.as_object() else {
            // `true` / `{}` / anything else: any value is fine.
            return json!(placeholder_string(name, hints));
        };
        if let Some(Value::String(r)) = obj.get("$ref") {
            return match resolve(root, r) {
                Some(target) => self.gen(target, name, depth + 1),
                None => Value::Null,
            };
        }
        if let Some(c) = obj.get("const") {
            return c.clone();
        }
        if let Some(Value::Array(values)) = obj.get("enum") {
            if let Some(first) = values.iter().find(|v| !v.is_null()).or(values.first()) {
                return first.clone();
            }
        }
        if let Some(d) = obj.get("default") {
            return d.clone();
        }
        for key in ["anyOf", "oneOf"] {
            if let Some(Value::Array(options)) = obj.get(key) {
                let pick = options
                    .iter()
                    .find(|o| {
                        let t = type_names(o);
                        t.is_empty() || !t.iter().all(|t| t == "null")
                    })
                    .or(options.first());
                if let Some(o) = pick {
                    return self.gen(o, name, depth + 1);
                }
            }
        }
        if let Some(Value::Array(parts)) = obj.get("allOf") {
            let mut merged = Map::new();
            let mut other = None;
            for p in parts {
                match self.gen(p, name, depth + 1) {
                    Value::Object(m) => merged.extend(m),
                    v => other = Some(v),
                }
            }
            // Sibling properties next to allOf.
            if obj.contains_key("properties") {
                let mut rest = obj.clone();
                rest.remove("allOf");
                if let Value::Object(m) = self.gen(&Value::Object(rest), name, depth + 1) {
                    merged.extend(m);
                }
            }
            return match other {
                Some(v) if merged.is_empty() => v,
                _ => Value::Object(merged),
            };
        }

        let types = type_names(schema);
        let nullable = obj.get("nullable").and_then(Value::as_bool) == Some(true);
        let ty = types
            .iter()
            .find(|t| *t != "null")
            .cloned()
            .or_else(|| {
                if obj.contains_key("properties") {
                    Some("object".into())
                } else if obj.contains_key("items") || obj.contains_key("prefixItems") {
                    Some("array".into())
                } else if types.iter().any(|t| t == "null") {
                    Some("null".into())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                if nullable {
                    "null".into()
                } else {
                    "string".into()
                }
            });

        match ty.as_str() {
            "object" => self.gen_object(obj, depth),
            "array" => self.gen_array(obj, name, depth),
            "integer" => json!(gen_integer(obj, name)),
            "number" => gen_number(obj, name),
            "boolean" => json!(true),
            "null" => Value::Null,
            _ => json!(gen_string(obj, name, hints)),
        }
    }

    fn gen_object(&mut self, obj: &Map<String, Value>, depth: usize) -> Value {
        let hints = self.hints;
        let mut out = Map::new();
        if let Some(Value::Object(props)) = obj.get("properties") {
            // Gemini's propertyOrdering, else declaration order.
            let mut names: Vec<&String> = props.keys().collect();
            if let Some(Value::Array(order)) = obj.get("propertyOrdering") {
                let ordered: Vec<&String> = order
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(|n| props.keys().find(|k| k.as_str() == n))
                    .collect();
                if !ordered.is_empty() {
                    let rest: Vec<&String> = names
                        .iter()
                        .copied()
                        .filter(|k| !ordered.contains(k))
                        .collect();
                    names = ordered.into_iter().chain(rest).collect();
                }
            }
            for k in names {
                if let Some(p) = props.get(k) {
                    out.insert(k.clone(), self.gen(p, k, depth + 1));
                }
            }
        }
        // Required names without a property schema still get a value.
        if let Some(Value::Array(req)) = obj.get("required") {
            for r in req.iter().filter_map(Value::as_str) {
                if !out.contains_key(r) {
                    out.insert(r.to_string(), json!(placeholder_string(r, hints)));
                }
            }
        }
        Value::Object(out)
    }

    fn gen_array(&mut self, obj: &Map<String, Value>, name: &str, depth: usize) -> Value {
        let min = obj.get("minItems").and_then(Value::as_u64).unwrap_or(1) as usize;
        let max = obj.get("maxItems").and_then(Value::as_u64).unwrap_or(10) as usize;
        let mut n = min.max(1).min(max).min(20);
        let mut out = Vec::new();
        if let Some(Value::Array(prefix)) = obj.get("prefixItems") {
            for p in prefix {
                out.push(self.gen(p, name, depth + 1));
            }
            n = n.max(out.len());
        }
        let item_schema = obj.get("items").cloned().unwrap_or(json!({}));
        let singular = name.strip_suffix('s').unwrap_or(name);
        let unique = obj.get("uniqueItems").and_then(Value::as_bool) == Some(true);
        while out.len() < n {
            let mut v = self.gen(&item_schema, singular, depth + 1);
            if unique {
                // Make repeated scalars distinct.
                let i = out.len();
                v = match v {
                    Value::String(s) if i > 0 => json!(format!("{s} {}", i + 1)),
                    Value::Number(num) if i > 0 => num
                        .as_i64()
                        .map(|x| json!(x + i as i64))
                        .unwrap_or(Value::Number(num)),
                    other => other,
                };
            }
            out.push(v);
        }
        Value::Array(out)
    }
}

fn bound(obj: &Map<String, Value>, key: &str) -> Option<f64> {
    obj.get(key).and_then(Value::as_f64)
}

fn gen_integer(obj: &Map<String, Value>, name: &str) -> i64 {
    let lname = name.to_ascii_lowercase();
    let mut v: i64 = if lname.contains("age") {
        30
    } else if lname.contains("year") {
        2026
    } else if lname.contains("count")
        || lname.contains("quantity")
        || lname.contains("num")
        || lname.contains("limit")
    {
        3
    } else if lname.contains("id") {
        12345
    } else {
        42
    };
    if let Some(min) = bound(obj, "minimum") {
        v = v.max(min.ceil() as i64);
    }
    if let Some(min) = bound(obj, "exclusiveMinimum") {
        v = v.max(min.floor() as i64 + 1);
    }
    if let Some(max) = bound(obj, "maximum") {
        v = v.min(max.floor() as i64);
    }
    if let Some(max) = bound(obj, "exclusiveMaximum") {
        v = v.min(max.ceil() as i64 - 1);
    }
    if let Some(m) = obj
        .get("multipleOf")
        .and_then(Value::as_i64)
        .filter(|m| *m > 0)
    {
        v = (v / m) * m;
    }
    v
}

fn gen_number(obj: &Map<String, Value>, name: &str) -> Value {
    let lname = name.to_ascii_lowercase();
    let mut v: f64 = if lname.contains("lat") {
        48.8566
    } else if lname.contains("lon") || lname.contains("lng") {
        2.3522
    } else if lname.contains("temp") {
        21.5
    } else if lname.contains("price") || lname.contains("amount") || lname.contains("cost") {
        19.99
    } else if lname.contains("score") || lname.contains("confidence") || lname.contains("prob") {
        0.87
    } else {
        2.5
    };
    if let Some(min) = bound(obj, "minimum") {
        v = v.max(min);
    }
    if let Some(min) = bound(obj, "exclusiveMinimum") {
        if v <= min {
            v = min + 1.0;
        }
    }
    if let Some(max) = bound(obj, "maximum") {
        v = v.min(max);
    }
    if let Some(max) = bound(obj, "exclusiveMaximum") {
        if v >= max {
            v = max - (max - bound(obj, "minimum").unwrap_or(max - 1.0)).abs() / 2.0;
        }
    }
    serde_json::Number::from_f64(v)
        .map(Value::Number)
        .unwrap_or(json!(0))
}

fn gen_string(obj: &Map<String, Value>, name: &str, hints: &Hints) -> String {
    let format = obj
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut s = match format.as_str() {
        "date-time" => "2026-01-15T09:30:00Z".to_string(),
        "date" => "2026-01-15".to_string(),
        "time" => "09:30:00".to_string(),
        "email" => "jane.doe@example.com".to_string(),
        "uri" | "url" | "iri" => "https://example.com/resource".to_string(),
        "uuid" => "123e4567-e89b-12d3-a456-426614174000".to_string(),
        "ipv4" => "192.0.2.10".to_string(),
        "ipv6" => "2001:db8::10".to_string(),
        "hostname" => "api.example.com".to_string(),
        "duration" => "PT1H".to_string(),
        _ => placeholder_string(name, hints),
    };
    let min = obj.get("minLength").and_then(Value::as_u64).unwrap_or(0) as usize;
    let max = obj
        .get("maxLength")
        .and_then(Value::as_u64)
        .map(|m| m as usize);
    while s.chars().count() < min {
        s.push('x');
    }
    if let Some(max) = max {
        if s.chars().count() > max {
            s = s.chars().take(max).collect();
        }
    }
    s
}

/// A plausible string for a property name.
pub fn placeholder_string(name: &str, hints: &Hints) -> String {
    let lname = name.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lname.contains(n));
    if has(&["location", "city", "place", "destination", "region"]) {
        return hints
            .entity
            .clone()
            .unwrap_or_else(|| "San Francisco".to_string());
    }
    if has(&["country"]) {
        return "France".into();
    }
    if has(&["email"]) {
        return "jane.doe@example.com".into();
    }
    if has(&["phone"]) {
        return "+1-555-0100".into();
    }
    if has(&["url", "link", "website", "uri"]) {
        return "https://example.com".into();
    }
    if has(&["date", "day"]) {
        return "2026-01-15".into();
    }
    if has(&["time"]) {
        return "09:30".into();
    }
    if has(&["unit"]) {
        return "celsius".into();
    }
    if has(&["currency"]) {
        return "USD".into();
    }
    if has(&["lang"]) {
        return "en".into();
    }
    if has(&[
        "query", "question", "prompt", "search", "keyword", "text", "input", "message",
    ]) || lname == "q"
    {
        if let Some(t) = &hints.user_text {
            return t.clone();
        }
        return "example query".into();
    }
    if has(&["first"]) && has(&["name"]) {
        return "Jane".into();
    }
    if has(&["last"]) && has(&["name"]) {
        return "Doe".into();
    }
    if has(&["user", "author", "owner", "customer"]) || lname == "name" {
        return "Jane Doe".into();
    }
    if lname.ends_with("id") || lname.ends_with("_id") {
        return format!(
            "{}_12345",
            lname.trim_end_matches("_id").trim_end_matches("id")
        )
        .trim_start_matches('_')
        .to_string();
    }
    if has(&["title"]) {
        return "Example title".into();
    }
    if has(&[
        "description",
        "summary",
        "content",
        "body",
        "answer",
        "reason",
    ]) {
        return "This is a deterministic sample value generated by Rustybin.".into();
    }
    if has(&["status", "state"]) {
        return "active".into();
    }
    if has(&["color", "colour"]) {
        return "blue".into();
    }
    if lname.is_empty() {
        return "example".into();
    }
    format!("example {}", name.replace('_', " "))
}

/// Minimal validator for the same subset (tests only).
#[cfg(test)]
pub fn validate(value: &Value, schema: &Value) -> Result<(), String> {
    validate_at(value, schema, schema, "$", 0)
}

#[cfg(test)]
fn validate_at(
    value: &Value,
    schema: &Value,
    root: &Value,
    path: &str,
    depth: usize,
) -> Result<(), String> {
    if depth > 30 {
        return Ok(());
    }
    let Some(obj) = schema.as_object() else {
        return Ok(());
    };
    if let Some(Value::String(r)) = obj.get("$ref") {
        let target = resolve(root, r).ok_or(format!("{path}: unresolved {r}"))?;
        return validate_at(value, target, root, path, depth + 1);
    }
    if let Some(c) = obj.get("const") {
        if c != value {
            return Err(format!("{path}: const mismatch"));
        }
    }
    if let Some(Value::Array(e)) = obj.get("enum") {
        if !e.contains(value) {
            return Err(format!("{path}: not in enum"));
        }
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(Value::Array(opts)) = obj.get(key) {
            if !opts
                .iter()
                .any(|o| validate_at(value, o, root, path, depth + 1).is_ok())
            {
                return Err(format!("{path}: no {key} branch matches"));
            }
        }
    }
    if let Some(Value::Array(parts)) = obj.get("allOf") {
        for p in parts {
            validate_at(value, p, root, path, depth + 1)?;
        }
    }
    let types = type_names(schema);
    if !types.is_empty() {
        let ok = types.iter().any(|t| match t.as_str() {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => true,
        });
        let nullable_ok =
            value.is_null() && obj.get("nullable").and_then(Value::as_bool) == Some(true);
        if !ok && !nullable_ok {
            return Err(format!("{path}: expected {types:?}, got {value}"));
        }
    }
    if let Value::Object(m) = value {
        if let Some(Value::Array(req)) = obj.get("required") {
            for r in req.iter().filter_map(Value::as_str) {
                if !m.contains_key(r) {
                    return Err(format!("{path}: missing required {r}"));
                }
            }
        }
        let props = obj.get("properties").and_then(Value::as_object);
        for (k, v) in m {
            match props.and_then(|p| p.get(k)) {
                Some(ps) => validate_at(v, ps, root, &format!("{path}.{k}"), depth + 1)?,
                None => {
                    if obj.get("additionalProperties") == Some(&Value::Bool(false)) {
                        return Err(format!("{path}: unexpected property {k}"));
                    }
                }
            }
        }
    }
    if let Value::Array(items) = value {
        if let Some(min) = obj.get("minItems").and_then(Value::as_u64) {
            if (items.len() as u64) < min {
                return Err(format!("{path}: too few items"));
            }
        }
        if let Some(max) = obj.get("maxItems").and_then(Value::as_u64) {
            if (items.len() as u64) > max {
                return Err(format!("{path}: too many items"));
            }
        }
        if let Some(is) = obj.get("items") {
            for (i, it) in items.iter().enumerate() {
                validate_at(it, is, root, &format!("{path}[{i}]"), depth + 1)?;
            }
        }
    }
    if let Some(n) = value.as_f64() {
        if let Some(min) = bound(obj, "minimum") {
            if n < min {
                return Err(format!("{path}: below minimum"));
            }
        }
        if let Some(max) = bound(obj, "maximum") {
            if n > max {
                return Err(format!("{path}: above maximum"));
            }
        }
        if let Some(min) = bound(obj, "exclusiveMinimum") {
            if n <= min {
                return Err(format!("{path}: not above exclusiveMinimum"));
            }
        }
        if let Some(max) = bound(obj, "exclusiveMaximum") {
            if n >= max {
                return Err(format!("{path}: not below exclusiveMaximum"));
            }
        }
    }
    if let Value::String(s) = value {
        let len = s.chars().count() as u64;
        if obj
            .get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|m| len < m)
        {
            return Err(format!("{path}: too short"));
        }
        if obj
            .get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|m| len > m)
        {
            return Err(format!("{path}: too long"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(schema: Value) -> Value {
        let v = generate(&schema, &Hints::default());
        validate(&v, &schema).unwrap_or_else(|e| panic!("{e}: {v} against {schema}"));
        v
    }

    #[test]
    fn scalars_and_enums() {
        assert!(check(json!({"type": "string"})).is_string());
        assert!(check(json!({"type": "integer", "minimum": 100, "maximum": 200})).is_i64());
        assert!(check(json!({"type": "number", "exclusiveMaximum": 1, "minimum": 0})).is_f64());
        assert_eq!(check(json!({"type": "boolean"})), json!(true));
        assert_eq!(check(json!({"enum": ["c", "f"]})), json!("c"));
        assert_eq!(check(json!({"type": ["null", "string"]})), json!("example"));
        assert_eq!(
            check(json!({"type": "STRING", "format": "date"})),
            json!("2026-01-15")
        );
        assert_eq!(
            check(json!({"type": "string", "minLength": 30, "maxLength": 40}))
                .as_str()
                .map(|s| s.len() >= 30),
            Some(true)
        );
    }

    #[test]
    fn nested_objects_arrays_and_refs() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "integer", "minimum": 0},
                "email": {"type": "string", "format": "email"},
                "tags": {"type": "array", "items": {"type": "string"}, "minItems": 2, "uniqueItems": true},
                "address": {"$ref": "#/$defs/address"},
                "kind": {"anyOf": [{"type": "null"}, {"enum": ["a", "b"]}]},
                "both": {"allOf": [{"type": "object", "properties": {"x": {"type": "integer"}}}, {"type": "object", "properties": {"y": {"type": "boolean"}}, "required": ["y"]}]}
            },
            "required": ["name", "age", "email", "tags", "address", "kind", "both"],
            "additionalProperties": false,
            "$defs": {
                "address": {
                    "type": "object",
                    "properties": {"city": {"type": "string"}, "zip": {"type": "string", "maxLength": 5}},
                    "required": ["city", "zip"],
                    "additionalProperties": false
                }
            }
        });
        let v = check(schema);
        assert_eq!(v["age"], json!(30));
        assert_eq!(v["kind"], json!("a"));
        assert_eq!(v["tags"].as_array().map(Vec::len), Some(2));
        assert_ne!(v["tags"][0], v["tags"][1]);
    }

    #[test]
    fn gemini_style_schema() {
        let schema = json!({
            "type": "OBJECT",
            "properties": {
                "recipe_name": {"type": "STRING"},
                "ingredients": {"type": "ARRAY", "items": {"type": "STRING"}},
                "rating": {"type": "NUMBER", "nullable": true}
            },
            "propertyOrdering": ["rating", "recipe_name"],
            "required": ["recipe_name"]
        });
        let v = check(schema);
        let keys: Vec<&String> = v
            .as_object()
            .map(|m| m.keys().collect())
            .unwrap_or_default();
        assert_eq!(keys.len(), 3);
    }

    #[test]
    fn hints_fill_location_and_query() {
        let hints = Hints::from_user_text("What is the weather in Paris today?");
        assert_eq!(hints.entity.as_deref(), Some("Paris"));
        let v = generate(
            &json!({"type": "object", "properties": {"location": {"type": "string"}, "query": {"type": "string"}}}),
            &hints,
        );
        assert_eq!(v["location"], json!("Paris"));
        assert_eq!(v["query"], json!("What is the weather in Paris today?"));
    }

    #[test]
    fn recursive_ref_terminates() {
        let schema = json!({
            "type": "object",
            "properties": {"child": {"$ref": "#"}},
        });
        let _ = generate(&schema, &Hints::default());
    }
}
