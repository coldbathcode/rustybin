//! Deploy checks: for every operation, generate a valid request and the
//! responses each configured source would return, and validate both against
//! the spec. User-provided bodies that do not validate (and references to
//! missing examples) are errors and block a deploy; generator limits and
//! odd specs are warnings.

use serde::Serialize;
use serde_json::Value;

use super::compile::Compiled;
use super::model::{ResponseMode, ResponseOverride, SecurityMode};
use super::request;
use super::respond::{self, Body, BodyChoice};
use super::schema::{self, Direction, GenOptions, Issue};
use super::spec::{Location, Operation};

/// At most this many documented statuses are checked per operation.
const MAX_STATUSES: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warning,
}

#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub level: Level,
    pub message: String,
}

impl Finding {
    fn error(message: impl Into<String>) -> Self {
        Self {
            level: Level::Error,
            message: message.into(),
        }
    }

    fn warning(message: impl Into<String>) -> Self {
        Self {
            level: Level::Warning,
            message: message.into(),
        }
    }
}

/// A generated request that the operation accepts.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SampleRequest {
    pub method: String,
    /// Path below the mock root, with the server prefix (`/v1/pets/1`).
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

/// The check of one response source.
#[derive(Clone, Debug, Serialize)]
pub struct ResponseCheck {
    /// `default`, `rule 2` (or `rule "name"`), `status:404`, ...
    pub source: String,
    pub status: u16,
    /// Where the body came from (`example:rex`, `generated`, `static`, ...).
    pub body_source: String,
    pub valid: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<Issue>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OperationReport {
    pub id: String,
    pub method: String,
    pub path: String,
    /// `ok`, `warning` or `error`.
    pub status: &'static str,
    pub findings: Vec<Finding>,
    pub request: SampleRequest,
    pub responses: Vec<ResponseCheck>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    /// No errors: the revision can be deployed.
    pub ok: bool,
    pub errors: usize,
    pub warnings: usize,
    pub spec_warnings: Vec<String>,
    /// Findings not tied to one operation.
    pub findings: Vec<Finding>,
    pub operations: Vec<OperationReport>,
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Characters kept as is in a generated path segment.
fn encode_segment(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn param_value(
    c: &Compiled,
    name: &str,
    schema_v: &Value,
    example: Option<&Value>,
    seed_offset: u64,
) -> String {
    if let Some(e) = example {
        return text_of(e);
    }
    let v = schema::generate_named(
        schema_v,
        name,
        &GenOptions {
            root: &c.spec.root,
            direction: Direction::Request,
            seed: c.settings.responses.seed.wrapping_add(seed_offset),
            use_examples: true,
            echo: &[],
        },
    );
    match v {
        Value::Array(items) => items.iter().map(text_of).collect::<Vec<_>>().join(","),
        other => text_of(&other),
    }
}

/// A request the operation accepts (required parameters, a generated body,
/// demo credentials when security is on).
pub fn sample_request(c: &Compiled, op: &Operation) -> SampleRequest {
    let mut path = op.path.clone();
    for p in op.params_in(Location::Path) {
        let v = param_value(c, &p.name, &p.schema, p.example.as_ref(), 0);
        path = path.replace(&format!("{{{}}}", p.name), &encode_segment(&v));
    }
    let mut query = Vec::new();
    for p in op.params_in(Location::Query).filter(|p| p.required) {
        query.push((
            p.name.clone(),
            param_value(c, &p.name, &p.schema, p.example.as_ref(), 1),
        ));
    }
    let mut headers = Vec::new();
    for p in op.params_in(Location::Header).filter(|p| p.required) {
        headers.push((
            p.name.clone(),
            param_value(c, &p.name, &p.schema, p.example.as_ref(), 2),
        ));
    }
    let cookies: Vec<String> = op
        .params_in(Location::Cookie)
        .filter(|p| p.required)
        .map(|p| {
            format!(
                "{}={}",
                p.name,
                param_value(c, &p.name, &p.schema, p.example.as_ref(), 3)
            )
        })
        .collect();
    if !cookies.is_empty() {
        headers.push(("Cookie".into(), cookies.join("; ")));
    }
    let (auth_headers, auth_query) = c.sample_credentials(op);
    headers.extend(auth_headers);
    query.extend(auth_query);
    let mut content_type = None;
    let mut body = None;
    if let Some(media) = op.request_body.as_ref().and_then(|rb| rb.json_media()) {
        let value = media
            .example
            .clone()
            .or_else(|| media.examples.first().map(|e| e.value.clone()))
            .or_else(|| {
                media.schema.as_ref().map(|s| {
                    schema::generate(
                        s,
                        &GenOptions {
                            root: &c.spec.root,
                            direction: Direction::Request,
                            seed: c.settings.responses.seed,
                            use_examples: true,
                            echo: &[],
                        },
                    )
                })
            });
        if let Some(v) = value {
            let ct = if media.media_type == "*/*" {
                "application/json".to_string()
            } else {
                media.media_type.clone()
            };
            content_type = Some(ct);
            body = Some(v);
        }
    }
    let prefix = c.path_prefix();
    SampleRequest {
        method: op.method.clone(),
        path: format!(
            "{prefix}{}",
            if path == "/" && !prefix.is_empty() {
                ""
            } else {
                &path
            }
        ),
        query,
        headers,
        content_type,
        body,
    }
}

/// Validate a rendered body against the schema of the media it was taken from.
pub fn validate_body(
    c: &Compiled,
    op: &Operation,
    status: u16,
    content_type: Option<&str>,
    body: Option<&Body>,
) -> Vec<Issue> {
    let Some(def) = op.response_for(status) else {
        return Vec::new();
    };
    let Some(body) = body else {
        return Vec::new();
    };
    let ct = content_type.map(request::base_media).unwrap_or_default();
    let media = def
        .content
        .iter()
        .find(|m| request::base_media(&m.media_type) == ct)
        .or_else(|| def.content.iter().find(|m| m.is_json()));
    let Some(schema_v) = media.and_then(|m| m.schema.as_ref()) else {
        return Vec::new();
    };
    let value = match body {
        Body::Json(v) => v.clone(),
        Body::Text(s) => match serde_json::from_str::<Value>(s) {
            Ok(v) => v,
            // A text body for a JSON schema of type string.
            Err(_) => Value::String(s.clone()),
        },
    };
    schema::validate(&value, schema_v, &c.spec.root, Direction::Response)
}

fn issues_text(issues: &[Issue]) -> String {
    issues
        .iter()
        .take(3)
        .map(|i| format!("{} {}", i.pointer, i.message))
        .collect::<Vec<_>>()
        .join("; ")
}

fn check_override(
    c: &Compiled,
    op: &Operation,
    o: &ResponseOverride,
    source: String,
    mode: ResponseMode,
    findings: &mut Vec<Finding>,
) -> ResponseCheck {
    let status = o.status.unwrap_or_else(|| op.default_status());
    if op.response_for(status).is_none() && !op.responses.is_empty() {
        findings.push(Finding::warning(format!(
            "{source}: status {status} is not documented in the spec"
        )));
    }
    if let Some(name) = &o.example {
        let exists = op
            .response_for(status)
            .is_some_and(|d| d.content.iter().any(|m| m.named_example(name).is_some()));
        if !exists {
            findings.push(Finding::error(format!(
                "{source}: example {name:?} does not exist for status {status}"
            )));
        }
    }
    let (ct, body, body_source) = respond::render_body(
        c,
        op,
        status,
        BodyChoice {
            body: o.body.as_ref(),
            example: o.example.as_deref(),
            mode,
        },
        None,
        &[],
    );
    let issues = validate_body(c, op, status, ct.as_deref(), body.as_ref());
    if !issues.is_empty() {
        let message = format!(
            "{source}: response does not match the schema ({})",
            issues_text(&issues)
        );
        findings.push(if o.body.is_some() {
            Finding::error(message)
        } else {
            Finding::warning(message)
        });
    }
    ResponseCheck {
        source,
        status,
        body_source,
        valid: issues.is_empty(),
        issues,
    }
}

fn check_operation(c: &Compiled, op: &Operation) -> OperationReport {
    let mut findings = Vec::new();
    let settings = c.settings.operations.get(&op.id);
    let mode = settings
        .and_then(|o| o.mode)
        .unwrap_or(c.settings.responses.mode);

    let request = sample_request(c, op);
    let path_params = extract_params(op, &request.path, c.path_prefix());
    let mut headers = axum::http::HeaderMap::new();
    for (k, v) in &request.headers {
        if let (Ok(name), Ok(value)) = (
            axum::http::HeaderName::from_bytes(k.as_bytes()),
            axum::http::HeaderValue::from_str(v),
        ) {
            headers.append(name, value);
        }
    }
    let body_bytes = request
        .body
        .as_ref()
        .map(|b| serde_json::to_vec(b).unwrap_or_default())
        .unwrap_or_default();
    let req_issues = request::validate(
        &c.spec.root,
        op,
        &path_params,
        &request.query,
        &headers,
        request.content_type.as_deref(),
        &body_bytes,
    );
    if !req_issues.is_empty() {
        findings.push(Finding::warning(format!(
            "the generated request does not validate ({}); check the request schema",
            issues_text(&req_issues)
        )));
    }

    let mut responses = Vec::new();
    // What a plain request gets (rules aside).
    match settings.and_then(|o| o.respond.as_ref()) {
        Some(o) => responses.push(check_override(
            c,
            op,
            o,
            "default".into(),
            mode,
            &mut findings,
        )),
        None => {
            let status = op.default_status();
            let (ct, body, body_source) = respond::render_body(
                c,
                op,
                status,
                BodyChoice {
                    body: None,
                    example: None,
                    mode,
                },
                None,
                &path_params,
            );
            let issues = validate_body(c, op, status, ct.as_deref(), body.as_ref());
            if !issues.is_empty() {
                let what = if body_source.starts_with("example") {
                    "the spec example"
                } else {
                    "generated data"
                };
                findings.push(Finding::warning(format!(
                    "{what} for {status} does not match the schema ({})",
                    issues_text(&issues)
                )));
            }
            responses.push(ResponseCheck {
                source: "default".into(),
                status,
                body_source,
                valid: issues.is_empty(),
                issues,
            });
        }
    }
    if let Some(o) = settings {
        let path_names: Vec<&str> = op
            .params_in(Location::Path)
            .map(|p| p.name.as_str())
            .collect();
        let query_names: Vec<&str> = op
            .params_in(Location::Query)
            .map(|p| p.name.as_str())
            .collect();
        for (i, rule) in o.rules.iter().enumerate() {
            let label = match &rule.name {
                Some(n) => format!("rule {n:?}"),
                None => format!("rule {}", i + 1),
            };
            for key in rule.when.keys() {
                if let Some(name) = key.strip_prefix("path.") {
                    if !path_names.contains(&name) {
                        findings.push(Finding::warning(format!(
                            "{label}: {key} is not a path parameter of {}, the rule never matches",
                            op.path
                        )));
                    }
                } else if let Some(name) = key.strip_prefix("query.") {
                    if !query_names.contains(&name) {
                        findings.push(Finding::warning(format!(
                            "{label}: {key} is not a documented query parameter"
                        )));
                    }
                } else if key.starts_with("body") && op.request_body.is_none() {
                    findings.push(Finding::warning(format!(
                        "{label}: the operation has no request body"
                    )));
                }
            }
            let check = check_override(c, op, &rule.respond, label, mode, &mut findings);
            responses.push(check);
        }
    }
    // Every other documented status (what `Prefer: code=` returns).
    let checked: Vec<u16> = responses.iter().map(|r| r.status).collect();
    for status in op
        .documented_statuses()
        .into_iter()
        .filter(|s| !checked.contains(s))
        .take(MAX_STATUSES)
    {
        let (ct, body, body_source) = respond::render_body(
            c,
            op,
            status,
            BodyChoice {
                body: None,
                example: None,
                mode,
            },
            None,
            &path_params,
        );
        let issues = validate_body(c, op, status, ct.as_deref(), body.as_ref());
        if !issues.is_empty() {
            findings.push(Finding::warning(format!(
                "the {status} response ({body_source}) does not match the schema ({})",
                issues_text(&issues)
            )));
        }
        responses.push(ResponseCheck {
            source: format!("status:{status}"),
            status,
            body_source,
            valid: issues.is_empty(),
            issues,
        });
    }
    let status = if findings.iter().any(|f| f.level == Level::Error) {
        "error"
    } else if findings.is_empty() {
        "ok"
    } else {
        "warning"
    };
    OperationReport {
        id: op.id.clone(),
        method: op.method.clone(),
        path: op.path.clone(),
        status,
        findings,
        request,
        responses,
    }
}

/// Path parameters of a concrete sample path.
fn extract_params(op: &Operation, path: &str, prefix: &str) -> Vec<(String, String)> {
    let rest = path.strip_prefix(prefix).unwrap_or(path);
    let template: Vec<&str> = op.path.trim_matches('/').split('/').collect();
    let actual: Vec<&str> = rest.trim_matches('/').split('/').collect();
    let mut out = Vec::new();
    for (t, a) in template.iter().zip(actual.iter()) {
        if let (Some(open), Some(close)) = (t.find('{'), t.find('}')) {
            let prefix_lit = &t[..open];
            let suffix_lit = &t[close + 1..];
            if let Some(inner) = a
                .strip_prefix(prefix_lit)
                .and_then(|x| x.strip_suffix(suffix_lit))
            {
                out.push((
                    t[open + 1..close].to_string(),
                    super::compile::percent_decode(inner),
                ));
            }
        }
    }
    out
}

/// Run every check.
pub fn run(c: &Compiled) -> Report {
    let mut findings = Vec::new();
    for key in c.settings.operations.keys() {
        if c.spec.operation(key).is_none() {
            findings.push(Finding::warning(format!(
                "settings for operation {key:?} are ignored: the spec has no such operationId"
            )));
        }
    }
    let sec = &c.settings.security;
    if sec.enabled && sec.mode == SecurityMode::Spec && !c.spec.declares_security() {
        findings.push(Finding::warning(
            "security is on but the spec declares no security requirements: pick a fixed mode \
             (api_key, basic, bearer or jwt) to protect the operations",
        ));
    }
    let operations: Vec<OperationReport> = c
        .spec
        .operations
        .iter()
        .map(|op| check_operation(c, op))
        .collect();
    let all_findings = findings
        .iter()
        .chain(operations.iter().flat_map(|o| o.findings.iter()));
    let (mut errors, mut warnings) = (0, 0);
    for f in all_findings {
        match f.level {
            Level::Error => errors += 1,
            Level::Warning => warnings += 1,
        }
    }
    warnings += c.spec.warnings.len();
    Report {
        ok: errors == 0,
        errors,
        warnings,
        spec_warnings: c.spec.warnings.clone(),
        findings,
        operations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::compile::tests::compile;
    use crate::mocks::model::MockSettings;
    use serde_json::json;

    const SPEC: &str = r##"{
      "openapi": "3.0.3", "info": {"title": "t", "version": "1"},
      "servers": [{"url": "/api"}],
      "paths": {
        "/pets/{petId}": {"get": {"operationId": "getPet",
          "parameters": [{"name": "petId", "in": "path", "required": true, "schema": {"type": "integer"}},
                         {"name": "fields", "in": "query", "required": true, "schema": {"type": "string", "enum": ["all", "min"]}}],
          "responses": {
            "200": {"description": "ok", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Pet"},
              "examples": {"bad": {"value": {"id": "nope"}}}}}},
            "404": {"description": "missing", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}}
          }}},
        "/pets": {"post": {"operationId": "createPet",
          "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Pet"}}}},
          "responses": {"201": {"description": "created", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Pet"}}}}}}}
      },
      "components": {"schemas": {
        "Pet": {"type": "object", "required": ["id", "name"], "properties": {"id": {"type": "integer", "readOnly": true}, "name": {"type": "string"}}},
        "Error": {"type": "object", "required": ["code"], "properties": {"code": {"type": "integer"}}}
      }}
    }"##;

    #[test]
    fn report_flags_bad_examples_and_overrides() {
        let settings: MockSettings = serde_json::from_value(json!({
            "operations": {
                "createPet": {"rules": [
                    {"when": {"path.nope": {"exists": true}}, "respond": {"status": 201, "body": {"name": 1}}},
                    {"when": {"body.name": {"equals": "x"}}, "respond": {"example": "missing"}}
                ]},
                "ghost": {"public": true}
            }
        }))
        .expect("settings");
        let c = compile(SPEC, settings);
        let r = &c.report;
        assert!(!r.ok);
        assert_eq!(r.errors, 2, "{r:#?}");
        assert!(r.findings[0].message.contains("ghost"));

        let get = r
            .operations
            .iter()
            .find(|o| o.id == "getPet")
            .expect("getPet");
        assert_eq!(get.status, "warning");
        assert_eq!(get.request.path, "/api/pets/2");
        assert_eq!(get.request.query[0].0, "fields");
        assert!(
            get.findings[0].message.contains("spec example"),
            "{:?}",
            get.findings
        );
        assert_eq!(get.responses.len(), 2);
        assert!(get.responses[1].valid);

        let create = r
            .operations
            .iter()
            .find(|o| o.id == "createPet")
            .expect("createPet");
        assert_eq!(create.status, "error");
        assert_eq!(
            create.request.content_type.as_deref(),
            Some("application/json")
        );
        assert!(create
            .request
            .body
            .as_ref()
            .expect("body")
            .get("id")
            .is_none());
        let messages: Vec<&str> = create.findings.iter().map(|f| f.message.as_str()).collect();
        assert!(
            messages.iter().any(|m| m.contains("never matches")),
            "{messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("does not exist")),
            "{messages:?}"
        );
    }

    #[test]
    fn clean_spec_is_ok_and_credentials_are_added() {
        let mut settings = MockSettings::default();
        settings.responses.mode = ResponseMode::Generated;
        settings.security.enabled = true;
        settings.security.mode = SecurityMode::ApiKey;
        let c = compile(SPEC, settings);
        assert!(c.report.ok, "{:#?}", c.report);
        let get = &c.report.operations[0];
        assert!(get
            .request
            .headers
            .contains(&("x-api-key".to_string(), "demo-key".to_string())));
        assert_eq!(get.status, "ok", "{:?}", get.findings);
    }
}
