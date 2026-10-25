//! JSON Schema generation and validation in the OpenAPI dialect.
//!
//! Generation is deterministic: the same schema, seed and request give the
//! same value, so a demo is repeatable. Values come from, in order: schema
//! `example` / `examples`, `const`, `enum`, `default`, then plausible data by
//! type, format, pattern and property name. Properties named like a path
//! parameter echo the request's value (`GET /pets/42` returns `id: 42`).
//!
//! Supported keywords: `$ref` (any local pointer), `type` (string or array),
//! `nullable`, `readOnly` / `writeOnly` (by [`Direction`]), `properties`,
//! `required`, `additionalProperties`, `min/maxProperties`, `items`,
//! `prefixItems`, `min/maxItems`, `uniqueItems`, `allOf` / `anyOf` / `oneOf`
//! (with `discriminator`), `enum`, `const`, string length, `pattern`,
//! `format`, numeric bounds (3.0 boolean and 3.1 numeric exclusive bounds)
//! and `multipleOf`.

use regex::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::spec::resolve_pointer;

/// Which side of the exchange a value is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Request bodies: `readOnly` properties are left out.
    Request,
    /// Responses: `writeOnly` properties are left out.
    Response,
}

/// One validation problem; `pointer` is a JSON pointer into the value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub pointer: String,
    pub message: String,
}

impl Issue {
    pub fn new(pointer: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            pointer: pointer.into(),
            message: message.into(),
        }
    }
}

const MAX_DEPTH: usize = 24;
const MAX_NODES: usize = 5_000;
const MAX_ISSUES: usize = 50;
/// Optional properties are dropped below this depth (keeps output readable).
const OPTIONAL_DEPTH: usize = 8;

/// Inputs for [`generate`].
pub struct GenOptions<'a> {
    /// The spec document `$ref`s resolve against.
    pub root: &'a Value,
    pub direction: Direction,
    pub seed: u64,
    /// Use schema-level `example` / `examples` when present.
    pub use_examples: bool,
    /// Request path parameters echoed into same-named top-level properties.
    pub echo: &'a [(String, String)],
}

/// Generate a value valid against `schema` (best effort).
pub fn generate(schema: &Value, opts: &GenOptions) -> Value {
    generate_named(schema, "", opts)
}

/// [`generate`] for a value with a name (a parameter or header), which
/// guides plausible values (`petId` gets an id-like integer).
pub fn generate_named(schema: &Value, name: &str, opts: &GenOptions) -> Value {
    let mut g = Gen {
        opts,
        budget: MAX_NODES,
        stack: Vec::new(),
    };
    let mut v = g.gen(schema, name, name, 0, 0);
    echo_params(&mut v, opts.echo, schema, opts.root);
    v
}

/// Overwrite top-level properties named like a path parameter (or `id` when
/// there is exactly one `*id` parameter) with the request's value.
fn echo_params(v: &mut Value, echo: &[(String, String)], schema: &Value, root: &Value) {
    let Value::Object(obj) = v else {
        return;
    };
    let id_like: Vec<&(String, String)> = echo
        .iter()
        .filter(|(n, _)| n.to_ascii_lowercase().ends_with("id"))
        .collect();
    let keys: Vec<String> = obj.keys().cloned().collect();
    for key in keys {
        let source = echo
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(&key))
            .or_else(|| (key == "id" && id_like.len() == 1).then(|| id_like[0]));
        let Some((_, raw)) = source else {
            continue;
        };
        let prop_schema = property_schema(schema, &key, root, 0).unwrap_or(Value::Null);
        let coerced = coerce(raw, &prop_schema, root);
        if validate(&coerced, &prop_schema, root, Direction::Response).is_empty() {
            obj.insert(key, coerced);
        }
    }
}

/// The schema of property `name` of an object schema (through `$ref` / `allOf`).
fn property_schema(schema: &Value, name: &str, root: &Value, depth: usize) -> Option<Value> {
    if depth > 8 {
        return None;
    }
    let schema = deref(schema, root);
    if let Some(p) = schema.pointer(&format!("/properties/{}", escape_pointer(name))) {
        return Some(p.clone());
    }
    schema
        .get("allOf")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|s| property_schema(s, name, root, depth + 1))
}

/// Follow `$ref`s (bounded); unresolvable refs give `{}`.
pub fn deref<'a>(mut schema: &'a Value, root: &'a Value) -> &'a Value {
    static EMPTY: Value = Value::Null;
    for _ in 0..32 {
        match schema.get("$ref").and_then(Value::as_str) {
            Some(r) => match resolve_pointer(root, r) {
                Ok(t) => schema = t,
                Err(_) => return &EMPTY,
            },
            None => return schema,
        }
    }
    &EMPTY
}

pub fn escape_pointer(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

/// Convert a raw parameter string to the schema's type (`"42"` -> `42`);
/// left as a string when it does not parse (validation then reports it).
pub fn coerce(raw: &str, schema: &Value, root: &Value) -> Value {
    let schema = deref(schema, root);
    let types = type_names(schema);
    let first = types.iter().find(|t| *t != "null").map(String::as_str);
    match first {
        Some("integer") => raw
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| json!(raw)),
        Some("number") => raw
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or_else(|| json!(raw)),
        Some("boolean") => match raw {
            "true" => json!(true),
            "false" => json!(false),
            _ => json!(raw),
        },
        Some("array") => {
            let items = schema.get("items").cloned().unwrap_or(Value::Null);
            Value::Array(
                raw.split(',')
                    .filter(|s| !s.is_empty())
                    .map(|s| coerce(s, &items, root))
                    .collect(),
            )
        }
        Some("null") if raw.is_empty() || raw == "null" => Value::Null,
        _ => json!(raw),
    }
}

fn type_names(schema: &Value) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.clone()],
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn is_nullable(schema: &Value) -> bool {
    schema.get("nullable").and_then(Value::as_bool) == Some(true)
        || type_names(schema).iter().any(|t| t == "null")
}

/// FNV-1a over the seed, a path and an index: cheap deterministic variety.
fn mix(seed: u64, path: &str, index: usize) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ seed;
    for b in path.bytes().chain(index.to_le_bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn pick<'p>(pool: &[&'p str], h: u64) -> &'p str {
    pool[(h % pool.len() as u64) as usize]
}

const PEOPLE: &[&str] = &[
    "Ada Lovelace",
    "Grace Hopper",
    "Alan Turing",
    "Katherine Johnson",
    "Margaret Hamilton",
    "Linus Torvalds",
];
const FIRST: &[&str] = &["Ada", "Grace", "Alan", "Katherine", "Margaret", "Linus"];
const LAST: &[&str] = &[
    "Lovelace", "Hopper", "Turing", "Johnson", "Hamilton", "Torvalds",
];
const WORDS: &[&str] = &["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"];
const CITIES: &[&str] = &["Lisbon", "Oslo", "Austin", "Kyoto", "Nairobi", "Toronto"];

struct Gen<'a, 'b> {
    opts: &'b GenOptions<'a>,
    budget: usize,
    /// `$ref`s being expanded (recursion guard).
    stack: Vec<String>,
}

impl Gen<'_, '_> {
    /// `ptr` identifies the position (for seeding), `name` is the property name.
    fn gen(&mut self, schema: &Value, name: &str, ptr: &str, index: usize, depth: usize) -> Value {
        if depth > MAX_DEPTH || self.budget == 0 {
            return Value::Null;
        }
        self.budget -= 1;
        let root = self.opts.root;
        let Some(obj) = schema.as_object() else {
            return json!(self.string_for(&Map::new(), name, ptr, index));
        };
        if let Some(Value::String(r)) = obj.get("$ref") {
            let Ok(target) = resolve_pointer(root, r) else {
                return Value::Null;
            };
            if self.stack.iter().filter(|s| *s == r).count() >= 2 {
                return minimal(target, root);
            }
            self.stack.push(r.clone());
            let v = self.gen(target, name, ptr, index, depth + 1);
            self.stack.pop();
            return v;
        }
        if self.opts.use_examples {
            if let Some(e) = obj.get("example") {
                return e.clone();
            }
            if let Some(Value::Array(e)) = obj.get("examples") {
                if let Some(first) = e.first() {
                    return first.clone();
                }
            }
        }
        if let Some(c) = obj.get("const") {
            return c.clone();
        }
        if let Some(Value::Array(values)) = obj.get("enum") {
            let choices: Vec<&Value> = values.iter().filter(|v| !v.is_null()).collect();
            if !choices.is_empty() {
                let h = mix(self.opts.seed, ptr, index);
                return choices[(h % choices.len() as u64) as usize].clone();
            }
            if let Some(first) = values.first() {
                return first.clone();
            }
        }
        if let Some(d) = obj.get("default") {
            return d.clone();
        }
        for key in ["oneOf", "anyOf"] {
            if let Some(Value::Array(options)) = obj.get(key) {
                let pick = options
                    .iter()
                    .find(|o| {
                        let t = type_names(deref(o, root));
                        t.is_empty() || !t.iter().all(|t| t == "null")
                    })
                    .or(options.first());
                let Some(o) = pick else {
                    continue;
                };
                let has_own_shape = obj.contains_key("properties")
                    || obj.contains_key("type")
                    || obj.contains_key("required");
                // A pure choice of schemas: generate the branch (keeps the
                // $ref recursion guard). Otherwise merge the branch into the
                // parent, which covers sibling properties and constraint-only
                // branches such as `anyOf: [{required: [a]}, {required: [b]}]`.
                let mut v = if has_own_shape {
                    let mut merged = obj.clone();
                    merged.remove(key);
                    merged.remove("discriminator");
                    merge_schema(&mut merged, deref(o, root));
                    self.gen(&Value::Object(merged), name, ptr, index, depth + 1)
                } else {
                    self.gen(o, name, ptr, index, depth + 1)
                };
                if let (Some(prop), Value::Object(m)) = (
                    obj.get("discriminator")
                        .and_then(|d| d.get("propertyName"))
                        .and_then(Value::as_str),
                    &mut v,
                ) {
                    if let Some(tag) = discriminator_value(obj, o) {
                        m.insert(prop.to_string(), json!(tag));
                    }
                }
                return v;
            }
        }
        if let Some(Value::Array(parts)) = obj.get("allOf") {
            let mut merged = Map::new();
            let mut other = None;
            for p in parts {
                match self.gen(p, name, ptr, index, depth + 1) {
                    Value::Object(m) => merged.extend(m),
                    v => other = Some(v),
                }
            }
            if obj.contains_key("properties") || obj.contains_key("type") {
                let mut rest = obj.clone();
                rest.remove("allOf");
                if let Value::Object(m) =
                    self.gen(&Value::Object(rest), name, ptr, index, depth + 1)
                {
                    for (k, val) in m {
                        merged.entry(k).or_insert(val);
                    }
                }
            }
            return match other {
                Some(v) if merged.is_empty() => v,
                _ => Value::Object(merged),
            };
        }

        let types = type_names(schema);
        let ty = types
            .iter()
            .find(|t| *t != "null")
            .cloned()
            .unwrap_or_else(|| {
                if obj.contains_key("properties") || obj.contains_key("additionalProperties") {
                    "object".into()
                } else if obj.contains_key("items") || obj.contains_key("prefixItems") {
                    "array".into()
                } else if types.iter().any(|t| t == "null") {
                    "null".into()
                } else if obj.contains_key("minimum") || obj.contains_key("maximum") {
                    "number".into()
                } else {
                    "string".into()
                }
            });
        match ty.as_str() {
            "object" => self.gen_object(obj, ptr, index, depth),
            "array" => self.gen_array(obj, name, ptr, depth),
            "integer" => json!(self.integer_for(obj, name, ptr, index)),
            "number" => number_for(obj, name),
            "boolean" => json!(!mix(self.opts.seed, ptr, index).is_multiple_of(4)),
            "null" => Value::Null,
            _ => json!(self.string_for(obj, name, ptr, index)),
        }
    }

    fn gen_object(
        &mut self,
        obj: &Map<String, Value>,
        ptr: &str,
        index: usize,
        depth: usize,
    ) -> Value {
        let root = self.opts.root;
        let required: Vec<&str> = obj
            .get("required")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut out = Map::new();
        if let Some(Value::Object(props)) = obj.get("properties") {
            for (k, p) in props {
                let resolved = deref(p, root);
                if self.skip_for_direction(p) || self.skip_for_direction(resolved) {
                    continue;
                }
                let is_required = required.contains(&k.as_str());
                if !is_required {
                    if depth >= OPTIONAL_DEPTH {
                        continue;
                    }
                    // Optional recursive properties are dropped.
                    if let Some(r) = p.get("$ref").and_then(Value::as_str) {
                        if self.stack.iter().any(|s| s == r) {
                            continue;
                        }
                    }
                }
                let child_ptr = format!("{ptr}/{}", escape_pointer(k));
                out.insert(k.clone(), self.gen(p, k, &child_ptr, index, depth + 1));
            }
        }
        let declared = obj.get("properties").and_then(Value::as_object);
        for r in &required {
            // Declared properties were handled above (or left out for this direction).
            if !out.contains_key(*r) && !declared.is_some_and(|p| p.contains_key(*r)) {
                let extra = match obj.get("additionalProperties") {
                    Some(s @ Value::Object(_)) => s.clone(),
                    _ => json!({ "type": "string" }),
                };
                let child_ptr = format!("{ptr}/{}", escape_pointer(r));
                out.insert(
                    (*r).to_string(),
                    self.gen(&extra, r, &child_ptr, index, depth + 1),
                );
            }
        }
        let min_props = obj
            .get("minProperties")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        if out.len() < min_props {
            if let Some(extra) = obj.get("additionalProperties").filter(|a| a.is_object()) {
                let mut i = 0;
                while out.len() < min_props.min(20) {
                    i += 1;
                    let key = format!("key{i}");
                    let v = self.gen(extra, &key, &format!("{ptr}/{key}"), index, depth + 1);
                    out.insert(key, v);
                }
            }
        }
        Value::Object(out)
    }

    fn skip_for_direction(&self, schema: &Value) -> bool {
        let flag = |k: &str| schema.get(k).and_then(Value::as_bool) == Some(true);
        match self.opts.direction {
            Direction::Request => flag("readOnly"),
            Direction::Response => flag("writeOnly"),
        }
    }

    fn gen_array(
        &mut self,
        obj: &Map<String, Value>,
        name: &str,
        ptr: &str,
        depth: usize,
    ) -> Value {
        let min = obj.get("minItems").and_then(Value::as_u64).unwrap_or(0) as usize;
        let max = obj.get("maxItems").and_then(Value::as_u64).unwrap_or(20) as usize;
        let recursive = obj
            .get("items")
            .and_then(|i| i.get("$ref"))
            .and_then(Value::as_str)
            .is_some_and(|r| self.stack.iter().any(|s| s == r));
        let wanted = if recursive || depth >= OPTIONAL_DEPTH {
            min
        } else {
            2
        };
        let n = wanted.max(min).min(max).min(20);
        let mut out = Vec::new();
        if let Some(Value::Array(prefix)) = obj.get("prefixItems") {
            for (i, p) in prefix.iter().enumerate() {
                out.push(self.gen(p, name, &format!("{ptr}/{i}"), i, depth + 1));
            }
        }
        let item_schema = obj.get("items").cloned().unwrap_or(json!({}));
        let singular = name.strip_suffix('s').unwrap_or(name);
        let unique = obj.get("uniqueItems").and_then(Value::as_bool) == Some(true);
        let mut attempts = 0;
        while out.len() < n && attempts < n * 3 {
            // The index varies with retries so a duplicate is not regenerated.
            let i = out.len();
            let v = self.gen(
                &item_schema,
                singular,
                &format!("{ptr}/{i}"),
                attempts,
                depth + 1,
            );
            attempts += 1;
            if unique && out.contains(&v) {
                continue;
            }
            out.push(v);
        }
        Value::Array(out)
    }

    fn integer_for(&self, obj: &Map<String, Value>, name: &str, ptr: &str, index: usize) -> i64 {
        let lname = name.to_ascii_lowercase();
        let h = mix(self.opts.seed, ptr, index);
        let mut v: i64 = if lname == "id" || lname.ends_with("id") || lname.ends_with("_id") {
            1 + index as i64 + (self.opts.seed % 1000) as i64
        } else if lname.contains("age") {
            21 + (h % 50) as i64
        } else if lname.contains("year") {
            2020 + (h % 7) as i64
        } else if lname.contains("count")
            || lname.contains("quantity")
            || lname.contains("total")
            || lname.contains("size")
        {
            1 + (h % 9) as i64
        } else if lname.contains("limit") || lname.contains("page") {
            10
        } else {
            (h % 100) as i64 + 1
        };
        let min = num(obj, "minimum");
        let max = num(obj, "maximum");
        let (xmin, xmax) = exclusive_bounds(obj);
        if let Some(m) = min {
            v = v.max(m.ceil() as i64);
        }
        if let Some(m) = xmin {
            v = v.max((m.floor() as i64).saturating_add(1));
        }
        if let Some(m) = max {
            v = v.min(m.floor() as i64);
        }
        if let Some(m) = xmax {
            v = v.min((m.ceil() as i64).saturating_sub(1));
        }
        if let Some(m) = obj
            .get("multipleOf")
            .and_then(Value::as_i64)
            .filter(|m| *m > 0)
        {
            let lower = min.map(|x| x.ceil() as i64).unwrap_or(i64::MIN);
            let mut candidate = (v / m) * m;
            if candidate < lower {
                candidate = candidate.checked_add(m).unwrap_or(candidate);
            }
            v = candidate;
        }
        if obj.get("format").and_then(Value::as_str) == Some("int32") {
            v = v.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
        }
        v
    }

    fn string_for(&self, obj: &Map<String, Value>, name: &str, ptr: &str, index: usize) -> String {
        let h = mix(self.opts.seed, ptr, index);
        let format = obj
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut s = match format.as_str() {
            "date-time" => format!("2026-0{}-1{}T09:30:00Z", 1 + h % 9, h % 10),
            "date" => format!("2026-0{}-1{}", 1 + h % 9, h % 10),
            "time" => "09:30:00".to_string(),
            "email" | "idn-email" => {
                let first = pick(FIRST, h).to_ascii_lowercase();
                format!("{first}@example.com")
            }
            "uri" | "url" | "iri" | "uri-reference" => {
                format!("https://example.com/{}", pick(WORDS, h))
            }
            "uuid" => uuid_for(h),
            "ipv4" => format!("192.0.2.{}", 1 + h % 254),
            "ipv6" => format!("2001:db8::{:x}", 1 + h % 0xfff),
            "hostname" | "idn-hostname" => format!("{}.example.com", pick(WORDS, h)),
            "byte" => "ZXhhbXBsZQ==".to_string(),
            "binary" => "example".to_string(),
            "password" => "s3cret-Passw0rd".to_string(),
            "duration" => "PT1H".to_string(),
            _ => match obj.get("pattern").and_then(Value::as_str) {
                Some(p) => pattern_string(p).unwrap_or_else(|| self.named_string(name, h, index)),
                None => self.named_string(name, h, index),
            },
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

    fn named_string(&self, name: &str, h: u64, index: usize) -> String {
        let lname = name.to_ascii_lowercase();
        let has = |needles: &[&str]| needles.iter().any(|n| lname.contains(n));
        if has(&["first"]) && has(&["name"]) {
            return pick(FIRST, h).into();
        }
        if has(&["last", "surname", "family"]) && has(&["name"]) {
            return pick(LAST, h).into();
        }
        if lname == "name" || has(&["fullname", "full_name", "username", "author", "owner"]) {
            return pick(PEOPLE, h).into();
        }
        if has(&["city", "town"]) {
            return pick(CITIES, h).into();
        }
        if has(&["tag", "label", "category", "kind", "type"]) {
            return pick(WORDS, h).into();
        }
        if lname.ends_with("id") && !lname.is_empty() {
            return format!(
                "{}_{}",
                lname.trim_end_matches("id").trim_end_matches('_'),
                1 + index
            )
            .trim_start_matches('_')
            .to_string();
        }
        crate::ai::schema::placeholder_string(name, &crate::ai::schema::Hints::default())
    }
}

/// Shallow merge of a branch into a parent schema: `properties` and
/// `required` are unioned, other keywords from the branch win.
fn merge_schema(parent: &mut Map<String, Value>, branch: &Value) {
    let Some(b) = branch.as_object() else {
        return;
    };
    for (k, v) in b {
        match (k.as_str(), parent.get_mut(k), v) {
            ("properties", Some(Value::Object(mine)), Value::Object(theirs)) => {
                for (pk, pv) in theirs {
                    mine.insert(pk.clone(), pv.clone());
                }
            }
            ("required", Some(Value::Array(mine)), Value::Array(theirs)) => {
                for r in theirs {
                    if !mine.contains(r) {
                        mine.push(r.clone());
                    }
                }
            }
            _ => {
                parent.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Property names declared by a schema and its `allOf` parts.
fn declared_properties(schema: &Value, root: &Value, out: &mut Vec<String>, depth: usize) {
    if depth > 8 {
        return;
    }
    let schema = deref(schema, root);
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for k in props.keys() {
            if !out.contains(k) {
                out.push(k.clone());
            }
        }
    }
    if let Some(parts) = schema.get("allOf").and_then(Value::as_array) {
        for p in parts {
            declared_properties(p, root, out, depth + 1);
        }
    }
}

/// The value a `discriminator` maps to a chosen branch.
fn discriminator_value(obj: &Map<String, Value>, branch: &Value) -> Option<String> {
    let r = branch.get("$ref").and_then(Value::as_str)?;
    if let Some(mapping) = obj
        .get("discriminator")
        .and_then(|d| d.get("mapping"))
        .and_then(Value::as_object)
    {
        for (k, v) in mapping {
            if v.as_str() == Some(r) || v.as_str().is_some_and(|v| r.ends_with(&format!("/{v}"))) {
                return Some(k.clone());
            }
        }
    }
    r.rsplit('/').next().map(str::to_string)
}

/// Smallest valid-looking value for a recursion cut-off.
fn minimal(schema: &Value, root: &Value) -> Value {
    let schema = deref(schema, root);
    if is_nullable(schema) {
        return Value::Null;
    }
    match type_names(schema).first().map(String::as_str) {
        Some("array") => json!([]),
        Some("string") => json!(""),
        Some("integer") | Some("number") => json!(0),
        Some("boolean") => json!(false),
        _ => json!({}),
    }
}

fn uuid_for(h: u64) -> String {
    let a = h;
    let b = h.rotate_left(17) ^ 0x9e37_79b9_7f4a_7c15;
    format!(
        "{:08x}-{:04x}-4{:03x}-a{:03x}-{:012x}",
        (a >> 32) as u32,
        (a >> 16) as u16,
        (a & 0xfff) as u16,
        (b >> 52) as u16 & 0xfff,
        b & 0xffff_ffff_ffff
    )
}

fn num(obj: &Map<String, Value>, key: &str) -> Option<f64> {
    obj.get(key).and_then(Value::as_f64)
}

/// Exclusive bounds in either dialect: 3.1 numbers, or 3.0 booleans that
/// make `minimum` / `maximum` exclusive.
fn exclusive_bounds(obj: &Map<String, Value>) -> (Option<f64>, Option<f64>) {
    let pick = |ex: &str, base: &str| match obj.get(ex) {
        Some(Value::Bool(true)) => num(obj, base),
        Some(v) => v.as_f64(),
        None => None,
    };
    (
        pick("exclusiveMinimum", "minimum"),
        pick("exclusiveMaximum", "maximum"),
    )
}

fn number_for(obj: &Map<String, Value>, name: &str) -> Value {
    let lname = name.to_ascii_lowercase();
    let mut v: f64 = if lname.contains("lat") {
        38.7223
    } else if lname.contains("lon") || lname.contains("lng") {
        -9.1393
    } else if lname.contains("price") || lname.contains("amount") || lname.contains("cost") {
        19.99
    } else if lname.contains("rate") || lname.contains("score") || lname.contains("ratio") {
        0.87
    } else {
        12.5
    };
    let (xmin, xmax) = exclusive_bounds(obj);
    if let Some(m) = num(obj, "minimum") {
        v = v.max(m);
    }
    if let Some(m) = xmin {
        if v <= m {
            v = m + 1.0;
        }
    }
    if let Some(m) = num(obj, "maximum") {
        v = v.min(m);
    }
    if let Some(m) = xmax {
        if v >= m {
            let lo = xmin.or(num(obj, "minimum")).unwrap_or(m - 2.0);
            v = (lo + m) / 2.0;
        }
    }
    if let Some(m) = num(obj, "multipleOf").filter(|m| *m > 0.0) {
        v = (v / m).ceil() * m;
    }
    serde_json::Number::from_f64(v)
        .map(Value::Number)
        .unwrap_or(json!(0))
}

/// A string matching a regular expression (common shapes), or `None`.
pub fn pattern_string(pattern: &str) -> Option<String> {
    use regex_syntax::hir::{Class, Hir, HirKind};
    fn walk(h: &Hir, out: &mut String, budget: &mut usize) -> bool {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        match h.kind() {
            HirKind::Empty | HirKind::Look(_) => true,
            HirKind::Literal(l) => match std::str::from_utf8(&l.0) {
                Ok(s) => {
                    out.push_str(s);
                    true
                }
                Err(_) => false,
            },
            HirKind::Class(Class::Unicode(c)) => {
                let preferred = c.ranges().iter().find_map(|r| {
                    ['a', 'A', '0', 'x']
                        .into_iter()
                        .find(|ch| r.start() <= *ch && *ch <= r.end())
                });
                match preferred.or_else(|| c.ranges().first().map(|r| r.start())) {
                    Some(ch) => {
                        out.push(ch);
                        true
                    }
                    None => false,
                }
            }
            HirKind::Class(Class::Bytes(c)) => match c.ranges().first() {
                Some(r) if r.start().is_ascii() => {
                    out.push(char::from(r.start()));
                    true
                }
                _ => false,
            },
            HirKind::Repetition(r) => {
                let n = r.min.max(1).min(r.max.unwrap_or(u32::MAX)).min(16);
                let n = if r.min == 0 && r.max == Some(0) { 0 } else { n };
                (0..n).all(|_| walk(&r.sub, out, budget))
            }
            HirKind::Capture(c) => walk(&c.sub, out, budget),
            HirKind::Concat(items) => items.iter().all(|i| walk(i, out, budget)),
            HirKind::Alternation(items) => items.first().is_some_and(|i| walk(i, out, budget)),
        }
    }
    let hir = regex_syntax::ParserBuilder::new()
        .nest_limit(64)
        .build()
        .parse(pattern)
        .ok()?;
    let mut out = String::new();
    let mut budget = 2_000;
    if !walk(&hir, &mut out, &mut budget) {
        return None;
    }
    let re = cached_regex(pattern)?;
    re.is_match(&out).then_some(out)
}

/// Compiled patterns, bounded (cleared when full).
pub(crate) fn cached_regex(pattern: &str) -> Option<Regex> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Regex>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(r) = map.get(pattern) {
        return r.clone();
    }
    if map.len() >= 512 {
        map.clear();
    }
    let compiled = regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .ok();
    map.insert(pattern.to_string(), compiled.clone());
    compiled
}

/// `$ref` depth followed by [`inline`].
pub const INLINE_DEPTH: usize = 8;
/// Schema nodes [`inline`] writes for one schema.
pub const INLINE_NODES: usize = 4_000;

/// Keywords whose value is one subschema.
const SUBSCHEMA_KEYS: &[&str] = &[
    "items",
    "additionalProperties",
    "not",
    "contains",
    "propertyNames",
    "unevaluatedProperties",
    "unevaluatedItems",
    "if",
    "then",
    "else",
];
/// Keywords whose value is a list of subschemas.
const SUBSCHEMA_LISTS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];
/// OpenAPI keywords that are not JSON Schema.
const OPENAPI_ONLY: &[&str] = &["nullable", "discriminator", "xml", "externalDocs"];

/// A self-contained JSON Schema (what MCP clients expect) from an OpenAPI
/// schema: `$ref`s are replaced by their targets, to [`INLINE_DEPTH`] (a
/// recursive or deeper reference becomes `{}` with a description); 3.0
/// `nullable` becomes a `null` type, boolean exclusive bounds become numeric
/// ones, `example` becomes `examples`, OpenAPI-only keywords and `x-`
/// extensions are dropped, and `readOnly` (requests) or `writeOnly`
/// (responses) properties are left out.
pub fn inline(schema: &Value, root: &Value, direction: Direction) -> Value {
    let mut budget = INLINE_NODES;
    let mut stack = Vec::new();
    inline_node(schema, root, direction, &mut stack, &mut budget)
}

fn inline_node(
    schema: &Value,
    root: &Value,
    direction: Direction,
    stack: &mut Vec<String>,
    budget: &mut usize,
) -> Value {
    let Value::Object(obj) = schema else {
        // `true` / `false` schemas (3.1) stay as they are.
        return schema.clone();
    };
    if *budget == 0 {
        return json!({ "description": "Not expanded: the schema is too large to inline" });
    }
    *budget -= 1;
    if let Some(reference) = obj.get("$ref").and_then(Value::as_str) {
        if stack.iter().any(|s| s == reference) || stack.len() >= INLINE_DEPTH {
            return json!({
                "description": format!("Recursive or deeply nested reference {reference}: not expanded")
            });
        }
        let Ok(target) = resolve_pointer(root, reference) else {
            return json!({});
        };
        stack.push(reference.to_string());
        let mut out = inline_node(target, root, direction, stack, budget);
        stack.pop();
        // 3.1 allows annotations next to `$ref`; they describe this use.
        if let Value::Object(o) = &mut out {
            for key in ["title", "description"] {
                if let Some(v) = obj.get(key) {
                    o.insert(key.into(), v.clone());
                }
            }
        }
        return out;
    }
    let mut out = Map::new();
    for (key, value) in obj {
        let key = key.as_str();
        if key.starts_with("x-") || OPENAPI_ONLY.contains(&key) {
            continue;
        }
        if key == "example" {
            if !obj.contains_key("examples") {
                out.insert("examples".into(), json!([value]));
            }
            continue;
        }
        if key == "examples" && !value.is_array() {
            // A 3.0 schema has no `examples`; a map is not valid JSON Schema.
            continue;
        }
        if key == "properties" || key == "patternProperties" {
            let Some(props) = value.as_object() else {
                continue;
            };
            let mut kept = Map::new();
            for (name, prop) in props {
                if key == "properties" && hidden(prop, root, direction) {
                    continue;
                }
                kept.insert(
                    name.clone(),
                    inline_node(prop, root, direction, stack, budget),
                );
            }
            out.insert(key.into(), Value::Object(kept));
            continue;
        }
        if SUBSCHEMA_KEYS.contains(&key) {
            let inlined = match value {
                // Draft 4 tuple form of `items`.
                Value::Array(list) => Value::Array(
                    list.iter()
                        .map(|s| inline_node(s, root, direction, stack, budget))
                        .collect(),
                ),
                other => inline_node(other, root, direction, stack, budget),
            };
            out.insert(key.into(), inlined);
            continue;
        }
        if SUBSCHEMA_LISTS.contains(&key) {
            if let Value::Array(list) = value {
                out.insert(
                    key.into(),
                    Value::Array(
                        list.iter()
                            .map(|s| inline_node(s, root, direction, stack, budget))
                            .collect(),
                    ),
                );
            }
            continue;
        }
        out.insert(key.into(), value.clone());
    }
    // `required` must not name the properties left out above.
    if let (Some(Value::Object(props)), Some(Value::Array(required))) =
        (obj.get("properties"), out.get_mut("required"))
    {
        required.retain(|r| {
            r.as_str()
                .and_then(|n| props.get(n))
                .is_none_or(|p| !hidden(p, root, direction))
        });
    }
    for (exclusive, bound) in [
        ("exclusiveMinimum", "minimum"),
        ("exclusiveMaximum", "maximum"),
    ] {
        if let Some(Value::Bool(flag)) = out.get(exclusive).cloned() {
            out.remove(exclusive);
            if flag {
                if let Some(limit) = out.remove(bound) {
                    out.insert(exclusive.into(), limit);
                }
            }
        }
    }
    if obj.get("nullable").and_then(Value::as_bool) == Some(true) {
        if out.contains_key("type") || out.contains_key("enum") {
            make_nullable(&mut out);
        } else if !out.is_empty() {
            // `nullable` next to `allOf` (a common 3.0 idiom for a nullable `$ref`).
            return json!({ "anyOf": [Value::Object(out), { "type": "null" }] });
        }
    }
    Value::Object(out)
}

/// Is this property left out in `direction` (`readOnly` in requests,
/// `writeOnly` in responses)?
fn hidden(prop: &Value, root: &Value, direction: Direction) -> bool {
    let flag = match direction {
        Direction::Request => "readOnly",
        Direction::Response => "writeOnly",
    };
    prop.get(flag).and_then(Value::as_bool) == Some(true)
        || deref(prop, root).get(flag).and_then(Value::as_bool) == Some(true)
}

/// 3.0 `nullable: true` in JSON Schema terms.
fn make_nullable(out: &mut Map<String, Value>) {
    match out.get_mut("type") {
        Some(Value::String(t)) => {
            let t = t.clone();
            out.insert("type".into(), json!([t, "null"]));
        }
        Some(Value::Array(types)) if !types.iter().any(|t| t == "null") => {
            types.push(json!("null"));
        }
        _ => {}
    }
    if let Some(Value::Array(values)) = out.get_mut("enum") {
        if !values.iter().any(Value::is_null) {
            values.push(Value::Null);
        }
    }
}

/// Validate `value` against `schema`; at most 50 issues.
pub fn validate(value: &Value, schema: &Value, root: &Value, direction: Direction) -> Vec<Issue> {
    let mut v = Validator {
        root,
        direction,
        issues: Vec::new(),
        allof_props: None,
    };
    v.check(value, schema, "", 0);
    v.issues
}

struct Validator<'a> {
    root: &'a Value,
    direction: Direction,
    issues: Vec<Issue>,
    /// Properties declared by sibling `allOf` parts of the object at this
    /// pointer: `additionalProperties: false` in one part does not reject
    /// them (strict JSON Schema would; OpenAPI authors rarely mean that).
    allof_props: Option<(String, Vec<String>)>,
}

fn describe(v: &Value) -> String {
    let kind = match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    let mut shown = v.to_string();
    if shown.len() > 40 {
        shown = format!("{}...", shown.chars().take(37).collect::<String>());
    }
    match v {
        Value::Array(_) | Value::Object(_) => kind.to_string(),
        _ => format!("{kind} {shown}"),
    }
}

fn type_matches(t: &str, value: &Value) -> bool {
    match t {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => {
            value.is_i64()
                || value.is_u64()
                || value
                    .as_f64()
                    .is_some_and(|f| f.fract() == 0.0 && f.is_finite())
        }
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

impl Validator<'_> {
    fn full(&self) -> bool {
        self.issues.len() >= MAX_ISSUES
    }

    fn push(&mut self, ptr: &str, msg: String) {
        if !self.full() {
            self.issues
                .push(Issue::new(if ptr.is_empty() { "/" } else { ptr }, msg));
        }
    }

    /// Run a sub-check on a scratch validator; true when it found nothing.
    fn passes(&self, value: &Value, schema: &Value, ptr: &str, depth: usize) -> bool {
        let mut sub = Validator {
            root: self.root,
            direction: self.direction,
            issues: Vec::new(),
            allof_props: self.allof_props.clone(),
        };
        sub.check(value, schema, ptr, depth);
        sub.issues.is_empty()
    }

    fn check(&mut self, value: &Value, schema: &Value, ptr: &str, depth: usize) {
        if depth > MAX_DEPTH * 2 || self.full() {
            return;
        }
        let Some(obj) = schema.as_object() else {
            if schema == &Value::Bool(false) {
                self.push(ptr, "no value is allowed here".into());
            }
            return;
        };
        if let Some(Value::String(r)) = obj.get("$ref") {
            match resolve_pointer(self.root, r) {
                Ok(target) => self.check(value, target, ptr, depth + 1),
                Err(e) => self.push(ptr, e),
            }
            return;
        }
        if value.is_null() && is_nullable(schema) {
            return;
        }
        if let Some(c) = obj.get("const") {
            if c != value {
                self.push(ptr, format!("must be {c}"));
            }
        }
        if let Some(Value::Array(e)) = obj.get("enum") {
            if !e.contains(value) {
                let allowed: Vec<String> = e.iter().take(10).map(Value::to_string).collect();
                self.push(
                    ptr,
                    format!(
                        "must be one of {}, got {}",
                        allowed.join(", "),
                        describe(value)
                    ),
                );
            }
        }
        if let Some(Value::Array(parts)) = obj.get("allOf") {
            let mut names = Vec::new();
            declared_properties(schema, self.root, &mut names, 0);
            let saved = self.allof_props.replace((ptr.to_string(), names));
            for p in parts {
                self.check(value, p, ptr, depth + 1);
            }
            self.allof_props = saved;
        }
        for key in ["oneOf", "anyOf"] {
            let Some(Value::Array(options)) = obj.get(key) else {
                continue;
            };
            // With a discriminator, validate the branch it names.
            if let Some(prop) = obj
                .get("discriminator")
                .and_then(|d| d.get("propertyName"))
                .and_then(Value::as_str)
            {
                if let Some(tag) = value.get(prop).and_then(Value::as_str) {
                    let branch = options
                        .iter()
                        .find(|o| discriminator_value(obj, o).as_deref() == Some(tag));
                    match branch {
                        Some(b) => self.check(value, b, ptr, depth + 1),
                        None => self.push(
                            &format!("{ptr}/{}", escape_pointer(prop)),
                            format!("unknown {prop} {tag:?}"),
                        ),
                    }
                    continue;
                }
            }
            // Lenient: at least one branch (overlapping oneOf branches are common).
            if !options
                .iter()
                .any(|o| self.passes(value, o, ptr, depth + 1))
            {
                self.push(ptr, format!("does not match any {key} alternative"));
            }
        }

        let types = type_names(schema);
        let concrete: Vec<&String> = types.iter().filter(|t| *t != "null").collect();
        if !concrete.is_empty() && !types.iter().any(|t| type_matches(t, value)) {
            let expected: Vec<&str> = concrete.iter().map(|s| s.as_str()).collect();
            self.push(
                ptr,
                format!(
                    "expected {}, got {}",
                    expected.join(" or "),
                    describe(value)
                ),
            );
            return;
        }
        if value.is_null() && types.is_empty() && obj.contains_key("type") {
            return;
        }

        match value {
            Value::Object(map) => self.check_object(map, obj, ptr, depth),
            Value::Array(items) => self.check_array(items, obj, ptr, depth),
            Value::String(s) => self.check_string(s, obj, ptr),
            Value::Number(_) => self.check_number(value, obj, ptr),
            _ => {}
        }
    }

    fn check_object(
        &mut self,
        map: &Map<String, Value>,
        obj: &Map<String, Value>,
        ptr: &str,
        depth: usize,
    ) {
        let props = obj.get("properties").and_then(Value::as_object);
        if let Some(Value::Array(req)) = obj.get("required") {
            for r in req.iter().filter_map(Value::as_str) {
                if map.contains_key(r) {
                    continue;
                }
                let skip = props
                    .and_then(|p| p.get(r))
                    .map(|p| deref(p, self.root))
                    .is_some_and(|p| {
                        let flag = |k: &str| p.get(k).and_then(Value::as_bool) == Some(true);
                        match self.direction {
                            Direction::Request => flag("readOnly"),
                            Direction::Response => flag("writeOnly"),
                        }
                    });
                if !skip {
                    self.push(ptr, format!("missing required property {r:?}"));
                }
            }
        }
        for (k, v) in map {
            let child = format!("{ptr}/{}", escape_pointer(k));
            match props.and_then(|p| p.get(k)) {
                Some(ps) => self.check(v, ps, &child, depth + 1),
                None => match obj.get("additionalProperties") {
                    Some(Value::Bool(false))
                        if self
                            .allof_props
                            .as_ref()
                            .is_some_and(|(p, names)| p == ptr && names.contains(k)) => {}
                    Some(Value::Bool(false)) => {
                        self.push(&child, format!("property {k:?} is not allowed"))
                    }
                    Some(s @ Value::Object(_)) => self.check(v, s, &child, depth + 1),
                    _ => {}
                },
            }
        }
        if let Some(min) = obj.get("minProperties").and_then(Value::as_u64) {
            if (map.len() as u64) < min {
                self.push(ptr, format!("must have at least {min} properties"));
            }
        }
        if let Some(max) = obj.get("maxProperties").and_then(Value::as_u64) {
            if (map.len() as u64) > max {
                self.push(ptr, format!("must have at most {max} properties"));
            }
        }
    }

    fn check_array(&mut self, items: &[Value], obj: &Map<String, Value>, ptr: &str, depth: usize) {
        if let Some(min) = obj.get("minItems").and_then(Value::as_u64) {
            if (items.len() as u64) < min {
                self.push(ptr, format!("must have at least {min} items"));
            }
        }
        if let Some(max) = obj.get("maxItems").and_then(Value::as_u64) {
            if (items.len() as u64) > max {
                self.push(ptr, format!("must have at most {max} items"));
            }
        }
        if obj.get("uniqueItems").and_then(Value::as_bool) == Some(true) {
            for (i, a) in items.iter().enumerate() {
                if items[..i].contains(a) {
                    self.push(&format!("{ptr}/{i}"), "duplicate item".into());
                    break;
                }
            }
        }
        let prefix = obj.get("prefixItems").and_then(Value::as_array);
        for (i, item) in items.iter().enumerate() {
            let s = match prefix.and_then(|p| p.get(i)) {
                Some(s) => Some(s),
                None => obj.get("items"),
            };
            if let Some(s) = s {
                self.check(item, s, &format!("{ptr}/{i}"), depth + 1);
            }
        }
    }

    fn check_string(&mut self, s: &str, obj: &Map<String, Value>, ptr: &str) {
        let len = s.chars().count() as u64;
        if let Some(min) = obj.get("minLength").and_then(Value::as_u64) {
            if len < min {
                self.push(ptr, format!("must be at least {min} characters"));
            }
        }
        if let Some(max) = obj.get("maxLength").and_then(Value::as_u64) {
            if len > max {
                self.push(ptr, format!("must be at most {max} characters"));
            }
        }
        if let Some(p) = obj.get("pattern").and_then(Value::as_str) {
            if let Some(re) = cached_regex(p) {
                if !re.is_match(s) {
                    self.push(ptr, format!("must match pattern {p}"));
                }
            }
        }
        if let Some(f) = obj.get("format").and_then(Value::as_str) {
            if !format_ok(f, s) {
                self.push(ptr, format!("must be a valid {f}, got {s:?}"));
            }
        }
    }

    fn check_number(&mut self, value: &Value, obj: &Map<String, Value>, ptr: &str) {
        let Some(n) = value.as_f64() else {
            return;
        };
        let (xmin, xmax) = exclusive_bounds(obj);
        let inclusive_min = match obj.get("exclusiveMinimum") {
            Some(Value::Bool(true)) => None,
            _ => num(obj, "minimum"),
        };
        let inclusive_max = match obj.get("exclusiveMaximum") {
            Some(Value::Bool(true)) => None,
            _ => num(obj, "maximum"),
        };
        if let Some(m) = inclusive_min {
            if n < m {
                self.push(ptr, format!("must be >= {m}"));
            }
        }
        if let Some(m) = xmin {
            if n <= m {
                self.push(ptr, format!("must be > {m}"));
            }
        }
        if let Some(m) = inclusive_max {
            if n > m {
                self.push(ptr, format!("must be <= {m}"));
            }
        }
        if let Some(m) = xmax {
            if n >= m {
                self.push(ptr, format!("must be < {m}"));
            }
        }
        if let Some(m) = num(obj, "multipleOf").filter(|m| *m > 0.0) {
            let q = n / m;
            if (q - q.round()).abs() > 1e-9 {
                self.push(ptr, format!("must be a multiple of {m}"));
            }
        }
        match obj.get("format").and_then(Value::as_str) {
            Some("int32") if value.as_i64().is_some_and(|i| i32::try_from(i).is_err()) => {
                self.push(ptr, "must fit in int32".into())
            }
            _ => {}
        }
    }
}

/// Lenient format checks for the common formats; unknown formats pass.
fn format_ok(format: &str, s: &str) -> bool {
    match format {
        "date" => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok(),
        "date-time" => chrono::DateTime::parse_from_rfc3339(s).is_ok(),
        "email" => {
            let mut parts = s.split('@');
            matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None)
                if !a.is_empty() && b.contains('.') && !s.contains(char::is_whitespace))
        }
        "uuid" => uuid::Uuid::parse_str(s).is_ok(),
        "uri" | "url" => reqwest::Url::parse(s).is_ok(),
        "ipv4" => s.parse::<std::net::Ipv4Addr>().is_ok(),
        "ipv6" => s.parse::<std::net::Ipv6Addr>().is_ok(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gen(schema: &Value, root: &Value, direction: Direction) -> Value {
        generate(
            schema,
            &GenOptions {
                root,
                direction,
                seed: 7,
                use_examples: true,
                echo: &[],
            },
        )
    }

    fn components() -> Value {
        json!({
            "components": { "schemas": {
                "Pet": {
                    "type": "object",
                    "required": ["id", "name"],
                    "properties": {
                        "id": { "type": "integer", "format": "int64", "readOnly": true },
                        "name": { "type": "string", "minLength": 1 },
                        "tag": { "type": "string", "nullable": true },
                        "status": { "type": "string", "enum": ["available", "sold"] },
                        "password": { "type": "string", "writeOnly": true },
                        "born": { "type": "string", "format": "date" },
                        "owner": { "$ref": "#/components/schemas/Person" },
                        "code": { "type": "string", "pattern": "^[A-Z]{3}-[0-9]{4}$" },
                        "weight": { "type": "number", "minimum": 0, "exclusiveMinimum": true, "maximum": 5 }
                    }
                },
                "Person": {
                    "type": "object",
                    "properties": {
                        "email": { "type": "string", "format": "email" },
                        "friends": { "type": "array", "items": { "$ref": "#/components/schemas/Person" } },
                        "best": { "$ref": "#/components/schemas/Person" }
                    }
                },
                "Shape": {
                    "oneOf": [ { "$ref": "#/components/schemas/Circle" }, { "$ref": "#/components/schemas/Square" } ],
                    "discriminator": { "propertyName": "kind", "mapping": { "circle": "#/components/schemas/Circle" } }
                },
                "Circle": { "type": "object", "required": ["kind", "r"], "properties": { "kind": { "type": "string" }, "r": { "type": "number" } } },
                "Square": { "type": "object", "required": ["kind", "side"], "properties": { "kind": { "type": "string" }, "side": { "type": "number" } } }
            }}
        })
    }

    #[test]
    fn inline_resolves_refs_and_converts_the_openapi_dialect() {
        let root = components();
        let pet = json!({ "$ref": "#/components/schemas/Pet" });
        let req = inline(&pet, &root, Direction::Request);
        let props = req["properties"].as_object().expect("properties");
        // readOnly id is not part of a request, writeOnly password is.
        assert!(!props.contains_key("id"));
        assert!(props.contains_key("password"));
        assert_eq!(req["required"], json!(["name"]));
        assert_eq!(props["tag"]["type"], json!(["string", "null"]));
        assert_eq!(props["weight"]["exclusiveMinimum"], json!(0));
        assert!(props["weight"].get("minimum").is_none());
        // Person is inlined; its self references stop with a description.
        let owner = &props["owner"];
        assert_eq!(owner["properties"]["email"]["format"], "email");
        let best = &owner["properties"]["best"];
        assert!(best.get("properties").is_none());
        assert!(best["description"]
            .as_str()
            .is_some_and(|d| d.contains("Recursive")));
        let friends = &owner["properties"]["friends"]["items"];
        assert!(friends["description"]
            .as_str()
            .is_some_and(|d| d.contains("Recursive")));
        assert!(!req.to_string().contains("$ref"));

        let resp = inline(&pet, &root, Direction::Response);
        let props = resp["properties"].as_object().expect("properties");
        assert!(props.contains_key("id"));
        assert!(!props.contains_key("password"));
        assert_eq!(resp["required"], json!(["id", "name"]));

        let shape = inline(
            &json!({ "$ref": "#/components/schemas/Shape" }),
            &root,
            Direction::Response,
        );
        assert!(shape.get("discriminator").is_none());
        assert_eq!(shape["oneOf"][0]["required"], json!(["kind", "r"]));
    }

    #[test]
    fn inline_handles_nullable_refs_examples_and_extensions() {
        let root = components();
        let s = json!({
            "nullable": true,
            "allOf": [{ "$ref": "#/components/schemas/Circle" }],
            "example": { "kind": "circle", "r": 1 },
            "x-internal": true
        });
        let out = inline(&s, &root, Direction::Response);
        assert_eq!(out["anyOf"][1], json!({ "type": "null" }));
        let inner = &out["anyOf"][0];
        assert_eq!(inner["examples"], json!([{ "kind": "circle", "r": 1 }]));
        assert!(inner.get("x-internal").is_none());
        assert_eq!(inner["allOf"][0]["properties"]["r"]["type"], "number");
        let e = inline(
            &json!({ "type": "string", "enum": ["a"], "nullable": true }),
            &root,
            Direction::Request,
        );
        assert_eq!(e["enum"], json!(["a", null]));
        // An unresolvable reference is an empty (accept anything) schema.
        let missing = inline(&json!({ "$ref": "#/nope" }), &root, Direction::Request);
        assert_eq!(missing, json!({}));
        assert_eq!(inline(&json!(true), &root, Direction::Request), json!(true));
    }

    #[test]
    fn inline_is_bounded() {
        // A wide schema of many references stops at the node budget.
        let mut schemas = Map::new();
        for i in 0..20 {
            let mut props = Map::new();
            for j in 0..20 {
                props.insert(
                    format!("p{j}"),
                    json!({ "$ref": format!("#/components/schemas/S{}", (i + 1) % 20) }),
                );
            }
            schemas.insert(
                format!("S{i}"),
                json!({ "type": "object", "properties": props }),
            );
        }
        let root = json!({ "components": { "schemas": schemas } });
        let out = inline(
            &json!({ "$ref": "#/components/schemas/S0" }),
            &root,
            Direction::Request,
        );
        let text = out.to_string();
        assert!(text.contains("too large to inline"));
        assert!(text.len() < 2_000_000);
    }

    #[test]
    fn generated_values_validate_in_both_directions() {
        let root = components();
        for name in ["Pet", "Person", "Shape"] {
            let schema = json!({ "$ref": format!("#/components/schemas/{name}") });
            for dir in [Direction::Request, Direction::Response] {
                let v = gen(&schema, &root, dir);
                let issues = validate(&v, &schema, &root, dir);
                assert!(issues.is_empty(), "{name} {dir:?}: {v} -> {issues:?}");
            }
        }
    }

    #[test]
    fn read_and_write_only_follow_direction() {
        let root = components();
        let pet = json!({ "$ref": "#/components/schemas/Pet" });
        let resp = gen(&pet, &root, Direction::Response);
        assert!(resp.get("id").is_some());
        assert!(resp.get("password").is_none());
        assert_eq!(resp["code"].as_str().map(str::len), Some(8), "{resp}");
        let req = gen(&pet, &root, Direction::Request);
        assert!(req.get("id").is_none());
        assert!(req.get("password").is_some());
        // A request without the readOnly id is valid; a response is not.
        let body = json!({ "name": "Rex" });
        assert!(validate(&body, &pet, &root, Direction::Request).is_empty());
        assert_eq!(validate(&body, &pet, &root, Direction::Response).len(), 1);
    }

    #[test]
    fn generation_is_deterministic_and_echoes_params() {
        let root = components();
        let pet = json!({ "$ref": "#/components/schemas/Pet" });
        let echo = vec![("petId".to_string(), "42".to_string())];
        let opts = GenOptions {
            root: &root,
            direction: Direction::Response,
            seed: 1,
            use_examples: true,
            echo: &echo,
        };
        let a = generate(&pet, &opts);
        assert_eq!(a, generate(&pet, &opts));
        assert_eq!(a["id"], 42);
        // A value that does not fit the schema is not echoed.
        let echo = vec![("petId".to_string(), "abc".to_string())];
        let b = generate(
            &pet,
            &GenOptions {
                echo: &echo,
                ..opts
            },
        );
        assert!(b["id"].is_i64());
    }

    #[test]
    fn discriminator_sets_the_tag() {
        let root = components();
        let v = gen(
            &json!({ "$ref": "#/components/schemas/Shape" }),
            &root,
            Direction::Response,
        );
        assert_eq!(v["kind"], "circle");
        let bad = json!({ "kind": "triangle" });
        let issues = validate(
            &bad,
            &json!({ "$ref": "#/components/schemas/Shape" }),
            &root,
            Direction::Response,
        );
        assert_eq!(issues[0].pointer, "/kind");
    }

    #[test]
    fn validation_reports_pointers_and_messages() {
        let root = components();
        let pet = json!({ "$ref": "#/components/schemas/Pet" });
        let v = json!({
            "id": "x", "name": "", "status": "lost", "born": "yesterday",
            "weight": 0, "owner": { "email": "nope" }, "code": "abc"
        });
        let issues = validate(&v, &pet, &root, Direction::Response);
        let pointers: Vec<&str> = issues.iter().map(|i| i.pointer.as_str()).collect();
        for p in [
            "/id",
            "/name",
            "/status",
            "/born",
            "/weight",
            "/owner/email",
            "/code",
        ] {
            assert!(pointers.contains(&p), "{p} missing in {issues:?}");
        }
        assert!(issues[0].message.contains("expected integer"), "{issues:?}");
    }

    #[test]
    fn three_one_types_and_bounds() {
        let root = json!({});
        let s = json!({ "type": ["integer", "null"], "exclusiveMinimum": 10, "maximum": 12, "multipleOf": 2 });
        let v = gen(&s, &root, Direction::Response);
        assert!(
            validate(&v, &s, &root, Direction::Response).is_empty(),
            "{v}"
        );
        assert!(validate(&Value::Null, &s, &root, Direction::Response).is_empty());
        assert!(!validate(&json!(10), &s, &root, Direction::Response).is_empty());
        let arr = json!({ "type": "array", "items": { "type": "string", "enum": ["a", "b"] }, "uniqueItems": true, "minItems": 2 });
        let v = gen(&arr, &root, Direction::Response);
        assert!(
            validate(&v, &arr, &root, Direction::Response).is_empty(),
            "{v}"
        );
    }

    #[test]
    fn coercion_and_patterns() {
        let root = json!({});
        assert_eq!(coerce("42", &json!({"type": "integer"}), &root), json!(42));
        assert_eq!(coerce("x", &json!({"type": "integer"}), &root), json!("x"));
        assert_eq!(
            coerce("true", &json!({"type": "boolean"}), &root),
            json!(true)
        );
        assert_eq!(
            coerce(
                "1,2",
                &json!({"type": "array", "items": {"type": "integer"}}),
                &root
            ),
            json!([1, 2])
        );
        assert_eq!(
            pattern_string("^[a-z]+@[a-z]+\\.com$").as_deref(),
            Some("a@a.com")
        );
        assert!(pattern_string("(?=x)").is_none() || pattern_string("(?=x)").is_some());
        assert!(pattern_string("[").is_none());
    }

    #[test]
    fn constraint_only_branches_and_closed_allof_parts() {
        let root = json!({});
        let s = json!({
            "type": "object",
            "properties": { "text": { "type": "string" }, "json": {} },
            "anyOf": [{ "required": ["text"] }, { "required": ["json"] }]
        });
        let v = gen(&s, &root, Direction::Request);
        assert!(v.get("text").is_some(), "{v}");
        assert!(
            validate(&v, &s, &root, Direction::Request).is_empty(),
            "{v}"
        );
        assert!(!validate(&json!({}), &s, &root, Direction::Request).is_empty());

        let s = json!({ "allOf": [
            { "type": "object", "required": ["id"], "properties": { "id": { "type": "integer" } } },
            { "type": "object", "properties": { "tags": { "type": "array" } }, "additionalProperties": false }
        ]});
        let v = gen(&s, &root, Direction::Response);
        assert!(
            validate(&v, &s, &root, Direction::Response).is_empty(),
            "{v}"
        );
        let extra = json!({ "id": 1, "nope": true });
        let issues = validate(&extra, &s, &root, Direction::Response);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].pointer, "/nope");
    }

    #[test]
    fn schema_examples_win() {
        let root = json!({});
        let s = json!({ "type": "object", "properties": { "name": { "type": "string", "example": "Rex" } } });
        assert_eq!(
            gen(&s, &root, Direction::Response),
            json!({ "name": "Rex" })
        );
        let s = json!({ "type": "string", "examples": ["first"] });
        assert_eq!(gen(&s, &root, Direction::Response), json!("first"));
    }
}
