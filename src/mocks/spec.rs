//! OpenAPI 3.0 / 3.1 parsing into a small, normalised model.
//!
//! The document is kept as a `serde_json::Value` (YAML is converted, map keys
//! are stringified) and walked leniently: anything odd becomes a warning, not
//! a failure. Only structural problems (not OpenAPI 3, external `$ref`s, too
//! many operations) are errors.
//!
//! Schemas keep their `$ref`s; [`super::schema`] resolves them against
//! [`ParsedSpec::root`] on demand. Parameters, request bodies, responses,
//! headers and examples are resolved here.

use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashSet};

/// HTTP methods an OpenAPI path item can declare, in display order.
pub const METHODS: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// Maximum `$ref` chain length (and YAML nesting depth).
const MAX_REF_DEPTH: usize = 32;
const MAX_YAML_DEPTH: usize = 128;
/// Unresolvable local refs listed in warnings.
const MAX_LISTED_REFS: usize = 10;

/// Where a parameter or API key lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Location {
    Path,
    Query,
    Header,
    Cookie,
}

impl Location {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "path" => Some(Self::Path),
            "query" => Some(Self::Query),
            "header" => Some(Self::Header),
            "cookie" => Some(Self::Cookie),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Query => "query",
            Self::Header => "header",
            Self::Cookie => "cookie",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Parameter {
    pub name: String,
    #[serde(rename = "in")]
    pub location: Location,
    pub required: bool,
    pub schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<Value>,
}

/// One media type of a request body or response.
#[derive(Clone, Debug, Serialize)]
pub struct Media {
    pub media_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<Value>,
    /// Named examples (resolved `value`s), in declaration order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<NamedExample>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NamedExample {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub value: Value,
}

impl Media {
    pub fn is_json(&self) -> bool {
        is_json_media(&self.media_type)
    }

    pub fn named_example(&self, name: &str) -> Option<&Value> {
        self.examples
            .iter()
            .find(|e| e.name == name)
            .map(|e| &e.value)
    }
}

/// `application/json`, `application/problem+json`, `application/vnd.x+json`, ...
pub fn is_json_media(media_type: &str) -> bool {
    let base = media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    base == "application/json" || base.ends_with("+json") || base == "*/*"
}

#[derive(Clone, Debug, Serialize)]
pub struct RequestBody {
    pub required: bool,
    pub content: Vec<Media>,
}

impl RequestBody {
    pub fn json_media(&self) -> Option<&Media> {
        self.content
            .iter()
            .find(|m| m.is_json())
            .or(self.content.first())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct HeaderDef {
    pub name: String,
    pub required: bool,
    pub schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResponseDef {
    /// `"200"`, `"4XX"` or `"default"`.
    pub status: String,
    pub description: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<HeaderDef>,
    pub content: Vec<Media>,
}

impl ResponseDef {
    /// The numeric status for exact codes.
    pub fn code(&self) -> Option<u16> {
        self.status.parse().ok()
    }

    /// Does this definition cover `status` (exact, range such as `2XX`, or default)?
    pub fn rank_for(&self, status: u16) -> Option<u8> {
        if self.code() == Some(status) {
            return Some(0);
        }
        let s = self.status.to_ascii_uppercase();
        if s.len() == 3 && s.ends_with("XX") && s.starts_with(&(status / 100).to_string()) {
            return Some(1);
        }
        (s == "DEFAULT").then_some(2)
    }
}

/// `[(scheme name, required scopes)]`: every scheme must pass (AND).
pub type Requirement = Vec<(String, Vec<String>)>;

#[derive(Clone, Debug, Serialize)]
pub struct Operation {
    /// `operationId`, or a synthesised one (`get_pets_petId`).
    pub id: String,
    /// True when `id` was synthesised.
    pub synthetic_id: bool,
    /// Upper case.
    pub method: String,
    /// Path template as written in the spec (`/pets/{petId}`).
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub deprecated: bool,
    pub parameters: Vec<Parameter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<RequestBody>,
    pub responses: Vec<ResponseDef>,
    /// Effective security alternatives (OR); empty means none required.
    pub security: Vec<Requirement>,
}

impl Operation {
    pub fn params_in(&self, location: Location) -> impl Iterator<Item = &Parameter> {
        self.parameters
            .iter()
            .filter(move |p| p.location == location)
    }

    /// The best response definition for `status`.
    pub fn response_for(&self, status: u16) -> Option<&ResponseDef> {
        self.responses
            .iter()
            .filter_map(|r| r.rank_for(status).map(|rank| (rank, r)))
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, r)| r)
    }

    /// The status a request answers with by default: the lowest documented
    /// 2xx, else 200 (covered by a `2XX` range or `default`, or undocumented).
    pub fn default_status(&self) -> u16 {
        self.responses
            .iter()
            .filter_map(ResponseDef::code)
            .filter(|c| (200..300).contains(c))
            .min()
            .unwrap_or(200)
    }

    /// Documented exact status codes, ascending.
    pub fn documented_statuses(&self) -> Vec<u16> {
        let mut codes: Vec<u16> = self
            .responses
            .iter()
            .filter_map(ResponseDef::code)
            .collect();
        codes.sort_unstable();
        codes.dedup();
        codes
    }
}

/// A security scheme from `components.securitySchemes`.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SchemeKind {
    ApiKey {
        name: String,
        #[serde(rename = "in")]
        location: Location,
    },
    Basic,
    Bearer {
        #[serde(skip_serializing_if = "Option::is_none")]
        bearer_format: Option<String>,
    },
    #[serde(rename = "oauth2")]
    OAuth2 {
        scopes: Vec<String>,
    },
    OpenIdConnect,
    MutualTls,
    Unsupported {
        detail: String,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct SecurityScheme {
    pub name: String,
    #[serde(flatten)]
    pub kind: SchemeKind,
}

#[derive(Clone, Debug, Serialize)]
pub struct ParsedSpec {
    /// The whole document as JSON (schemas are resolved against it).
    #[serde(skip)]
    pub root: Value,
    pub openapi: String,
    pub title: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path of `servers[0].url` (`/v1`), empty when none.
    pub server_prefix: String,
    pub tags: Vec<String>,
    pub operations: Vec<Operation>,
    pub security_schemes: Vec<SecurityScheme>,
    pub warnings: Vec<String>,
}

impl ParsedSpec {
    pub fn operation(&self, id: &str) -> Option<&Operation> {
        self.operations.iter().find(|o| o.id == id)
    }

    pub fn scheme(&self, name: &str) -> Option<&SecurityScheme> {
        self.security_schemes.iter().find(|s| s.name == name)
    }

    /// True when any operation requires security.
    pub fn declares_security(&self) -> bool {
        self.operations.iter().any(|o| !o.security.is_empty())
    }

    /// A URL-safe name derived from the title (`Swagger Petstore` -> `swagger-petstore`).
    pub fn suggested_name(&self) -> String {
        slugify(&self.title)
    }
}

/// Lower-case, `[a-z0-9-]`, at most 40 characters, never empty.
pub fn slugify(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 40 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "mock".to_string()
    } else {
        out
    }
}

/// Parsing limits.
#[derive(Clone, Copy, Debug)]
pub struct ParseLimits {
    pub max_bytes: usize,
    pub max_operations: usize,
}

/// Parse JSON or YAML text into JSON (YAML map keys become strings).
/// Decode a JSON or YAML document; Swagger 2.0 comes back as OpenAPI 3.
pub fn parse_document(text: &str) -> Result<Value, String> {
    let root = decode(text)?;
    if root.get("swagger").is_some() {
        return super::swagger2::convert(&root).map(|(doc, _)| doc);
    }
    Ok(root)
}

fn decode(text: &str) -> Result<Value, String> {
    let trimmed = text.trim_start_matches('\u{feff}').trim();
    if trimmed.is_empty() {
        return Err("the document is empty".into());
    }
    if trimmed.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            return Ok(v);
        }
    }
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(trimmed).map_err(|e| format!("not valid JSON or YAML: {e}"))?;
    yaml_to_json(yaml, 0)
}

fn yaml_to_json(v: serde_yaml::Value, depth: usize) -> Result<Value, String> {
    use serde_yaml::Value as Y;
    if depth > MAX_YAML_DEPTH {
        return Err("the document is nested too deeply".into());
    }
    Ok(match v {
        Y::Null => Value::Null,
        Y::Bool(b) => Value::Bool(b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else if let Some(u) = n.as_u64() {
                Value::from(u)
            } else {
                n.as_f64()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            }
        }
        Y::String(s) => Value::String(s),
        Y::Sequence(items) => Value::Array(
            items
                .into_iter()
                .map(|i| yaml_to_json(i, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        Y::Mapping(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                let key = match k {
                    Y::String(s) => s,
                    Y::Number(n) => n.to_string(),
                    Y::Bool(b) => b.to_string(),
                    Y::Null => "null".to_string(),
                    _ => return Err("unsupported YAML map key (use strings)".into()),
                };
                out.insert(key, yaml_to_json(v, depth + 1)?);
            }
            Value::Object(out)
        }
        Y::Tagged(t) => yaml_to_json(t.value, depth + 1)?,
    })
}

/// Follow `$ref` chains (local JSON pointers only).
pub fn resolve<'a>(root: &'a Value, mut v: &'a Value) -> Result<&'a Value, String> {
    for _ in 0..MAX_REF_DEPTH {
        let Some(r) = v.get("$ref").and_then(Value::as_str) else {
            return Ok(v);
        };
        v = resolve_pointer(root, r)?;
    }
    Err("$ref chain too long (cycle?)".into())
}

/// Resolve one local reference (`#/components/schemas/Pet`).
pub fn resolve_pointer<'a>(root: &'a Value, reference: &str) -> Result<&'a Value, String> {
    let pointer = reference
        .strip_prefix('#')
        .ok_or_else(|| format!("external $ref {reference:?} is not supported (bundle the spec)"))?;
    root.pointer(pointer)
        .ok_or_else(|| format!("unresolved $ref {reference:?}"))
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Every `$ref` string in the document (bounded walk).
fn collect_refs<'a>(v: &'a Value, out: &mut Vec<&'a str>, depth: usize) {
    if depth > MAX_YAML_DEPTH || out.len() > 100_000 {
        return;
    }
    match v {
        Value::Object(m) => {
            for (k, val) in m {
                if k == "$ref" {
                    if let Value::String(s) = val {
                        out.push(s);
                        continue;
                    }
                }
                collect_refs(val, out, depth + 1);
            }
        }
        Value::Array(items) => {
            for i in items {
                collect_refs(i, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// Parse an OpenAPI document (JSON or YAML text).
pub fn parse(text: &str, limits: ParseLimits) -> Result<ParsedSpec, String> {
    if text.len() > limits.max_bytes {
        return Err(format!(
            "the spec is {} bytes, the limit is {} bytes",
            text.len(),
            limits.max_bytes
        ));
    }
    let root = decode(text)?;
    parse_value(root, limits)
}

/// Parse an already decoded OpenAPI document.
pub fn parse_value(root: Value, limits: ParseLimits) -> Result<ParsedSpec, String> {
    if !root.is_object() {
        return Err("the spec must be a JSON or YAML object".into());
    }
    let mut warnings = Vec::new();
    let root = if root.get("swagger").is_some() {
        let (doc, notes) = super::swagger2::convert(&root)?;
        warnings.extend(notes);
        doc
    } else {
        root
    };
    let openapi = match root.get("openapi") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Err("missing \"openapi\" version field (expected 3.0.x or 3.1.x)".into()),
    };
    if !openapi.starts_with("3.") {
        return Err(format!(
            "unsupported OpenAPI version {openapi:?} (expected 3.0.x or 3.1.x)"
        ));
    }
    if !openapi.starts_with("3.0") && !openapi.starts_with("3.1") {
        warnings.push(format!(
            "OpenAPI {openapi} is newer than 3.1; parsed as 3.1"
        ));
    }

    let mut refs = Vec::new();
    collect_refs(&root, &mut refs, 0);
    if let Some(ext) = refs.iter().find(|r| !r.starts_with('#')) {
        return Err(format!(
            "external $ref {ext:?} is not supported: bundle the spec into one file first"
        ));
    }
    let unresolved: Vec<&str> = refs
        .iter()
        .copied()
        .filter(|r| resolve_pointer(&root, r).is_err())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if !unresolved.is_empty() {
        let mut listed: Vec<&str> = unresolved.iter().copied().take(MAX_LISTED_REFS).collect();
        listed.sort_unstable();
        warnings.push(format!(
            "{} unresolved $ref(s): {}",
            unresolved.len(),
            listed.join(", ")
        ));
    }

    let info = root.get("info").cloned().unwrap_or(Value::Null);
    let title = str_of(&info, "title").unwrap_or_else(|| "Untitled API".into());
    let version = str_of(&info, "version").unwrap_or_default();
    let description = str_of(&info, "description");
    let server_prefix = server_prefix(&root, &mut warnings);

    let security_schemes = parse_security_schemes(&root, &mut warnings);
    let global_security = root
        .get("security")
        .map(parse_requirements)
        .unwrap_or_default();

    let mut operations = Vec::new();
    let mut ids = HashSet::new();
    let mut tags: Vec<String> = root
        .get("tags")
        .and_then(Value::as_array)
        .map(|t| t.iter().filter_map(|t| str_of(t, "name")).collect())
        .unwrap_or_default();

    let paths = match root.get("paths") {
        Some(Value::Object(p)) => p.clone(),
        Some(_) => return Err("\"paths\" must be an object".into()),
        None => Map::new(),
    };
    for (path, item) in &paths {
        if !path.starts_with('/') {
            warnings.push(format!("path {path:?} does not start with '/', skipped"));
            continue;
        }
        let item = match resolve(&root, item) {
            Ok(i) => i,
            Err(e) => {
                warnings.push(format!("{path}: {e}"));
                continue;
            }
        };
        let path_params = parse_parameters(&root, item.get("parameters"), path, &mut warnings);
        for method in METHODS {
            let Some(op) = item.get(*method) else {
                continue;
            };
            if operations.len() >= limits.max_operations {
                return Err(format!(
                    "the spec has more than {} operations",
                    limits.max_operations
                ));
            }
            let label = format!("{} {path}", method.to_ascii_uppercase());
            let mut params = path_params.clone();
            for p in parse_parameters(&root, op.get("parameters"), &label, &mut warnings) {
                params.retain(|q| !(q.name == p.name && q.location == p.location));
                params.push(p);
            }
            // Path template parameters that are not declared still match.
            for name in template_params(path) {
                if !params
                    .iter()
                    .any(|p| p.location == Location::Path && p.name == name)
                {
                    warnings.push(format!(
                        "{label}: path parameter {{{name}}} is not declared (treated as a string)"
                    ));
                    params.push(Parameter {
                        name,
                        location: Location::Path,
                        required: true,
                        schema: serde_json::json!({ "type": "string" }),
                        description: None,
                        example: None,
                    });
                }
            }

            let declared_id = str_of(op, "operationId").filter(|s| valid_operation_id(s));
            if let Some(raw) = str_of(op, "operationId") {
                if declared_id.is_none() {
                    warnings.push(format!(
                        "{label}: operationId {raw:?} has unsupported characters, a generated id is used"
                    ));
                }
            }
            let synthetic_id = declared_id.is_none();
            let mut id = declared_id.unwrap_or_else(|| synth_id(method, path));
            if !ids.insert(id.clone()) {
                let mut n = 2;
                while ids.contains(&format!("{id}_{n}")) {
                    n += 1;
                }
                warnings.push(format!(
                    "{label}: duplicate operationId {id:?}, renamed to {id}_{n}"
                ));
                id = format!("{id}_{n}");
                ids.insert(id.clone());
            }

            let request_body = match op.get("requestBody") {
                None => None,
                Some(rb) => match resolve(&root, rb) {
                    Ok(rb) => Some(RequestBody {
                        required: rb.get("required").and_then(Value::as_bool) == Some(true),
                        content: parse_content(&root, rb.get("content"), &label, &mut warnings),
                    }),
                    Err(e) => {
                        warnings.push(format!("{label}: requestBody {e}"));
                        None
                    }
                },
            };
            let responses = parse_responses(&root, op.get("responses"), &label, &mut warnings);
            if responses.is_empty() {
                warnings.push(format!(
                    "{label}: no responses documented, answers 200 with an empty body"
                ));
            }
            let security = match op.get("security") {
                Some(s) => parse_requirements(s),
                None => global_security.clone(),
            };
            for req in &security {
                for (scheme, _) in req {
                    if !security_schemes.iter().any(|s| &s.name == scheme) {
                        warnings.push(format!(
                            "{label}: security scheme {scheme:?} is not defined in components.securitySchemes"
                        ));
                    }
                }
            }
            let op_tags: Vec<String> = op
                .get("tags")
                .and_then(Value::as_array)
                .map(|t| {
                    t.iter()
                        .filter_map(|t| t.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            for t in &op_tags {
                if !tags.contains(t) {
                    tags.push(t.clone());
                }
            }
            operations.push(Operation {
                id,
                synthetic_id,
                method: method.to_ascii_uppercase(),
                path: path.clone(),
                summary: str_of(op, "summary"),
                description: str_of(op, "description"),
                tags: op_tags,
                deprecated: op.get("deprecated").and_then(Value::as_bool) == Some(true),
                parameters: params,
                request_body,
                responses,
                security,
            });
        }
    }
    if operations.is_empty() {
        warnings.push("the spec declares no operations".into());
    }
    conflict_warnings(&operations, &mut warnings);

    Ok(ParsedSpec {
        root,
        openapi,
        title,
        version,
        description,
        server_prefix,
        tags,
        operations,
        security_schemes,
        warnings,
    })
}

/// operationIds are used in URLs: keep them to unreserved characters.
fn valid_operation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '~'))
}

/// `get /pets/{petId}` -> `get_pets_petId`.
fn synth_id(method: &str, path: &str) -> String {
    let mut out = method.to_ascii_lowercase();
    let mut segment_started = false;
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            if !segment_started {
                out.push('_');
                segment_started = true;
            }
            out.push(c);
        } else {
            segment_started = false;
        }
    }
    if out == method.to_ascii_lowercase() {
        out.push_str("_root");
    }
    out.chars().take(128).collect()
}

/// Names of the `{param}` placeholders in a path template.
pub fn template_params(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        let name = &rest[start + 1..start + end];
        if !name.is_empty() {
            out.push(name.to_string());
        }
        rest = &rest[start + end + 1..];
    }
    out
}

fn conflict_warnings(ops: &[Operation], warnings: &mut Vec<String>) {
    let mut seen: BTreeMap<(String, String), &str> = BTreeMap::new();
    for op in ops {
        // Normalise parameter names: /a/{x} and /a/{y} are the same route.
        let shape: String = op
            .path
            .split('/')
            .map(|s| if s.contains('{') { "{}" } else { s })
            .collect::<Vec<_>>()
            .join("/");
        if let Some(first) = seen.insert((op.method.clone(), shape), &op.path) {
            warnings.push(format!(
                "{} {} conflicts with {} {first}: the first one declared wins",
                op.method, op.path, op.method
            ));
        }
    }
}

fn server_prefix(root: &Value, warnings: &mut Vec<String>) -> String {
    let Some(server) = root
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|s| s.first())
    else {
        return String::new();
    };
    let Some(mut url) = str_of(server, "url") else {
        return String::new();
    };
    if url.contains('{') {
        let vars = server.get("variables").cloned().unwrap_or(Value::Null);
        for name in template_params(&url) {
            let default = vars
                .get(&name)
                .and_then(|v| v.get("default"))
                .and_then(Value::as_str)
                .map(str::to_string);
            match default {
                Some(d) => url = url.replace(&format!("{{{name}}}"), &d),
                None => {
                    warnings.push(format!(
                        "server variable {{{name}}} has no default, servers[0] is ignored"
                    ));
                    return String::new();
                }
            }
        }
    }
    let path = match url.find("://") {
        Some(i) => match url[i + 3..].find('/') {
            Some(j) => url[i + 3 + j..].to_string(),
            None => String::new(),
        },
        None => url,
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() || !trimmed.starts_with('/') {
        String::new()
    } else {
        trimmed.to_string()
    }
}

fn parse_requirements(v: &Value) -> Vec<Requirement> {
    let Some(list) = v.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for req in list {
        let Some(map) = req.as_object() else {
            continue;
        };
        if map.is_empty() {
            // `{}` makes security optional: one alternative needs nothing.
            return Vec::new();
        }
        out.push(
            map.iter()
                .map(|(k, scopes)| {
                    let scopes = scopes
                        .as_array()
                        .map(|s| {
                            s.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    (k.clone(), scopes)
                })
                .collect(),
        );
    }
    out
}

fn parse_security_schemes(root: &Value, warnings: &mut Vec<String>) -> Vec<SecurityScheme> {
    let Some(map) = root
        .pointer("/components/securitySchemes")
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, raw) in map {
        let s = match resolve(root, raw) {
            Ok(s) => s,
            Err(e) => {
                warnings.push(format!("security scheme {name:?}: {e}"));
                continue;
            }
        };
        let ty = str_of(s, "type").unwrap_or_default();
        let kind = match ty.as_str() {
            "apiKey" => {
                let key_name = str_of(s, "name").unwrap_or_else(|| "x-api-key".into());
                match str_of(s, "in").as_deref().and_then(Location::parse) {
                    Some(loc) if loc != Location::Path => SchemeKind::ApiKey {
                        name: key_name,
                        location: loc,
                    },
                    _ => SchemeKind::Unsupported {
                        detail: "apiKey must be in header, query or cookie".into(),
                    },
                }
            }
            "http" => match str_of(s, "scheme")
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str()
            {
                "basic" => SchemeKind::Basic,
                "bearer" => SchemeKind::Bearer {
                    bearer_format: str_of(s, "bearerFormat"),
                },
                other => SchemeKind::Unsupported {
                    detail: format!("http scheme {other:?} is not mocked"),
                },
            },
            "oauth2" => {
                let mut scopes = Vec::new();
                if let Some(flows) = s.get("flows").and_then(Value::as_object) {
                    for flow in flows.values() {
                        if let Some(sc) = flow.get("scopes").and_then(Value::as_object) {
                            for k in sc.keys() {
                                if !scopes.contains(k) {
                                    scopes.push(k.clone());
                                }
                            }
                        }
                    }
                }
                SchemeKind::OAuth2 { scopes }
            }
            "openIdConnect" => SchemeKind::OpenIdConnect,
            "mutualTLS" => SchemeKind::MutualTls,
            other => SchemeKind::Unsupported {
                detail: format!("type {other:?} is not mocked"),
            },
        };
        if let SchemeKind::Unsupported { detail } = &kind {
            warnings.push(format!(
                "security scheme {name:?}: {detail}, requests are let through"
            ));
        }
        out.push(SecurityScheme {
            name: name.clone(),
            kind,
        });
    }
    out
}

fn parse_parameters(
    root: &Value,
    v: Option<&Value>,
    label: &str,
    warnings: &mut Vec<String>,
) -> Vec<Parameter> {
    let Some(list) = v.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for raw in list {
        let p = match resolve(root, raw) {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!("{label}: parameter {e}"));
                continue;
            }
        };
        let Some(name) = str_of(p, "name") else {
            warnings.push(format!("{label}: a parameter has no name, skipped"));
            continue;
        };
        let Some(location) = str_of(p, "in").as_deref().and_then(Location::parse) else {
            warnings.push(format!(
                "{label}: parameter {name:?} has an invalid \"in\", skipped"
            ));
            continue;
        };
        let schema = match p.get("schema") {
            Some(s) => s.clone(),
            None => p
                .get("content")
                .and_then(Value::as_object)
                .and_then(|c| c.values().next())
                .and_then(|m| m.get("schema"))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({ "type": "string" })),
        };
        let example = p.get("example").cloned().or_else(|| {
            p.get("examples")
                .and_then(Value::as_object)
                .and_then(|e| e.values().next())
                .and_then(|e| resolve(root, e).ok())
                .and_then(|e| e.get("value"))
                .cloned()
        });
        out.push(Parameter {
            name,
            location,
            required: location == Location::Path
                || p.get("required").and_then(Value::as_bool) == Some(true),
            schema,
            description: str_of(p, "description"),
            example,
        });
    }
    out
}

fn parse_content(
    root: &Value,
    v: Option<&Value>,
    label: &str,
    warnings: &mut Vec<String>,
) -> Vec<Media> {
    let Some(map) = v.and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (media_type, m) in map {
        let mut examples = Vec::new();
        if let Some(ex) = m.get("examples").and_then(Value::as_object) {
            for (name, e) in ex {
                match resolve(root, e) {
                    Ok(e) => match e.get("value") {
                        Some(value) => examples.push(NamedExample {
                            name: name.clone(),
                            summary: str_of(e, "summary"),
                            value: value.clone(),
                        }),
                        None => warnings.push(format!(
                            "{label}: example {name:?} has no inline value (externalValue is not fetched)"
                        )),
                    },
                    Err(err) => warnings.push(format!("{label}: example {name:?}: {err}")),
                }
            }
        }
        out.push(Media {
            media_type: media_type.clone(),
            schema: m.get("schema").cloned(),
            example: m.get("example").cloned(),
            examples,
        });
    }
    out
}

fn parse_responses(
    root: &Value,
    v: Option<&Value>,
    label: &str,
    warnings: &mut Vec<String>,
) -> Vec<ResponseDef> {
    let Some(map) = v.and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (status, raw) in map {
        let valid = status.eq_ignore_ascii_case("default")
            || status
                .parse::<u16>()
                .is_ok_and(|c| (100..=599).contains(&c))
            || (status.len() == 3
                && status[1..].eq_ignore_ascii_case("XX")
                && matches!(&status[..1], "1" | "2" | "3" | "4" | "5"));
        if !valid {
            warnings.push(format!(
                "{label}: response key {status:?} is not a status code, skipped"
            ));
            continue;
        }
        let r = match resolve(root, raw) {
            Ok(r) => r,
            Err(e) => {
                warnings.push(format!("{label}: response {status}: {e}"));
                continue;
            }
        };
        let mut headers = Vec::new();
        if let Some(hmap) = r.get("headers").and_then(Value::as_object) {
            for (name, h) in hmap {
                if name.eq_ignore_ascii_case("content-type") {
                    continue;
                }
                let Ok(h) = resolve(root, h) else {
                    continue;
                };
                headers.push(HeaderDef {
                    name: name.clone(),
                    required: h.get("required").and_then(Value::as_bool) == Some(true),
                    schema: h
                        .get("schema")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({ "type": "string" })),
                    example: h.get("example").cloned(),
                });
            }
        }
        out.push(ResponseDef {
            status: status.to_ascii_uppercase().replace("DEFAULT", "default"),
            description: str_of(r, "description").unwrap_or_default(),
            headers,
            content: parse_content(root, r.get("content"), label, warnings),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: ParseLimits = ParseLimits {
        max_bytes: 1024 * 1024,
        max_operations: 500,
    };

    #[test]
    fn parses_yaml_with_numeric_status_keys_and_refs() {
        let text = r##"
openapi: 3.0.3
info: { title: Pet Store, version: "1.0" }
servers: [{ url: "https://api.example.com/v1/" }]
paths:
  /pets/{petId}:
    parameters:
      - $ref: "#/components/parameters/PetId"
    get:
      operationId: getPet
      responses:
        200:
          $ref: "#/components/responses/Pet"
        default:
          description: error
components:
  parameters:
    PetId: { name: petId, in: path, required: true, schema: { type: integer } }
  responses:
    Pet:
      description: a pet
      content:
        application/json:
          schema: { $ref: "#/components/schemas/Pet" }
          examples:
            rex: { value: { id: 1, name: Rex } }
  schemas:
    Pet: { type: object, properties: { id: { type: integer }, name: { type: string } } }
"##;
        let spec = parse(text, LIMITS).expect("parsed");
        assert_eq!(spec.title, "Pet Store");
        assert_eq!(spec.server_prefix, "/v1");
        assert_eq!(spec.suggested_name(), "pet-store");
        let op = spec.operation("getPet").expect("op");
        assert_eq!(op.method, "GET");
        assert_eq!(op.parameters[0].name, "petId");
        assert_eq!(op.responses.len(), 2);
        let ok = op.response_for(200).expect("200");
        assert_eq!(
            ok.content[0].named_example("rex").expect("rex")["name"],
            "Rex"
        );
        assert_eq!(op.response_for(500).expect("default").status, "default");
        assert_eq!(op.default_status(), 200);
        assert!(spec.warnings.is_empty(), "{:?}", spec.warnings);
    }

    #[test]
    fn converts_swagger_and_rejects_external_refs() {
        let err = parse(r#"{"swagger":"1.2"}"#, LIMITS).unwrap_err();
        assert!(err.contains("Swagger version"), "{err}");
        let v2 = "swagger: '2.0'\ninfo: {title: Old, version: '1'}\nbasePath: /api\npaths:\n  /things/{id}:\n    get:\n      parameters: [{name: id, in: path, required: true, type: integer}]\n      responses: {'200': {description: ok, schema: {type: object, properties: {id: {type: integer}}}}}\n";
        let spec = parse(v2, LIMITS).expect("swagger 2");
        assert_eq!(spec.openapi, "3.0.3");
        assert_eq!(spec.server_prefix, "/api");
        assert_eq!(spec.operations.len(), 1);
        assert!(
            spec.warnings[0].contains("Swagger 2.0"),
            "{:?}",
            spec.warnings
        );
        let err = parse(
            r#"{"openapi":"3.0.0","info":{},"paths":{"/a":{"get":{"responses":{"200":{"$ref":"other.yaml#/x"}}}}}}"#,
            LIMITS,
        )
        .unwrap_err();
        assert!(err.contains("external $ref"), "{err}");
        assert!(parse("", LIMITS).is_err());
        assert!(parse("[1,2]", LIMITS).is_err());
        let big = ParseLimits {
            max_bytes: 10,
            max_operations: 1,
        };
        assert!(parse(r#"{"openapi":"3.0.0","info":{}}"#, big).is_err());
    }

    #[test]
    fn synthesises_ids_and_warns_about_problems() {
        let text = r##"{
          "openapi": "3.1.0", "info": {"title": "  "},
          "security": [{"key": []}],
          "paths": {
            "/a/{x}": {"get": {"responses": {"200": {"description": "ok"}}}},
            "/a/{y}": {"get": {"operationId": "has space", "security": [], "responses": {}}},
            "/": {"post": {"responses": {"2XX": {"description": "ok"}, "teapot": {}}}}
          }
        }"##;
        let spec = parse(text, LIMITS).expect("parsed");
        let ids: Vec<&str> = spec.operations.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, vec!["get_a_x", "get_a_y", "post_root"]);
        assert_eq!(spec.suggested_name(), "mock");
        let w = spec.warnings.join("\n");
        assert!(w.contains("not declared"), "{w}");
        assert!(w.contains("conflicts with"), "{w}");
        assert!(w.contains("unsupported characters"), "{w}");
        assert!(w.contains("no responses"), "{w}");
        assert!(w.contains("\"teapot\""), "{w}");
        assert!(w.contains("\"key\" is not defined"), "{w}");
        assert_eq!(spec.operations[0].security.len(), 1);
        assert!(spec.operations[1].security.is_empty());
        assert_eq!(spec.operations[2].default_status(), 200);
    }

    #[test]
    fn security_schemes_are_classified() {
        let text = r##"{
          "openapi": "3.0.0", "info": {"title": "x"}, "paths": {},
          "components": {"securitySchemes": {
            "key": {"type": "apiKey", "name": "X-Key", "in": "header"},
            "basic": {"type": "http", "scheme": "Basic"},
            "jwt": {"type": "http", "scheme": "bearer", "bearerFormat": "JWT"},
            "oauth": {"type": "oauth2", "flows": {"clientCredentials": {"tokenUrl": "/t", "scopes": {"read": "", "write": ""}}}},
            "oidc": {"type": "openIdConnect", "openIdConnectUrl": "/x"},
            "digest": {"type": "http", "scheme": "digest"}
          }}
        }"##;
        let spec = parse(text, LIMITS).expect("parsed");
        assert!(matches!(
            spec.scheme("key").expect("key").kind,
            SchemeKind::ApiKey { ref name, location: Location::Header } if name == "X-Key"
        ));
        assert!(matches!(
            spec.scheme("basic").expect("b").kind,
            SchemeKind::Basic
        ));
        assert!(matches!(
            spec.scheme("oauth").expect("o").kind,
            SchemeKind::OAuth2 { ref scopes } if scopes.len() == 2
        ));
        assert!(matches!(
            spec.scheme("digest").expect("d").kind,
            SchemeKind::Unsupported { .. }
        ));
    }

    #[test]
    fn slugify_names() {
        assert_eq!(
            slugify("Swagger Petstore - OpenAPI 3.0"),
            "swagger-petstore-openapi-3-0"
        );
        assert_eq!(slugify("***"), "mock");
        assert!(slugify(&"a".repeat(100)).len() <= 40);
    }

    #[test]
    fn yaml_bombs_are_rejected() {
        let bomb = "a: &a [x, x, x, x, x, x, x, x, x]\nb: &b [*a, *a, *a, *a, *a, *a, *a, *a, *a]\nc: &c [*b, *b, *b, *b, *b, *b, *b, *b, *b]\nd: &d [*c, *c, *c, *c, *c, *c, *c, *c, *c]\ne: &e [*d, *d, *d, *d, *d, *d, *d, *d, *d]\nf: &f [*e, *e, *e, *e, *e, *e, *e, *e, *e]\ng: &g [*f, *f, *f, *f, *f, *f, *f, *f, *f]\nh: &h [*g, *g, *g, *g, *g, *g, *g, *g, *g]\n";
        // serde_yaml caps alias expansion; either way this must not hang.
        let _ = parse_document(bomb);
    }
}
