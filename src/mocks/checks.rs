//! Deploy checks: for every operation, generate a valid request and the
//! responses each configured source would return, and validate both against
//! the spec. User-provided bodies that do not validate (and references to
//! missing examples) are errors and block a deploy; generator limits and
//! odd specs are warnings.

use serde::Serialize;
use serde_json::Value;

use super::compile::Compiled;
use super::model::{Condition, ResponseMode, ResponseOverride, Rule, SecurityMode};
use super::request;
use super::respond::{self, Body, BodyChoice};
use super::schema::{self, Direction, GenOptions, Issue};
use super::spec::{Location, Operation, SchemeKind};

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

/// Replace the demo credentials [`sample_request`] added with placeholders,
/// for callers who may not see the mock's secrets.
pub fn hide_credentials(c: &Compiled, op: &Operation, req: &mut SampleRequest) {
    let (real_h, real_q) = c.sample_credentials(op);
    let (shown_h, shown_q) = c.placeholder_credentials(op);
    for (pairs, real, shown) in [
        (&mut req.headers, real_h, shown_h),
        (&mut req.query, real_q, shown_q),
    ] {
        for (r, s) in real.into_iter().zip(shown) {
            if let Some(p) = pairs.iter_mut().rev().find(|p| **p == r) {
                *p = s;
            }
        }
    }
}

/// [`hide_credentials`] for every operation of a report.
pub fn hide_report_credentials(c: &Compiled, report: &mut Report) {
    for r in &mut report.operations {
        if let Some(op) = c.spec.operation(&r.id) {
            hide_credentials(c, op, &mut r.request);
        }
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
        let mut shadowed_by: Option<String> = None;
        for (i, rule) in o.rules.iter().enumerate() {
            let label = match &rule.name {
                Some(n) => format!("rule {n:?}"),
                None => format!("rule {}", i + 1),
            };
            if let Some(first) = &shadowed_by {
                findings.push(Finding::warning(format!(
                    "{label} is never reached: {first} matches every request"
                )));
            }
            for (key, cond) in &rule.when {
                if let Some(why) = contradiction(key, cond, &path_names) {
                    findings.push(Finding::warning(format!(
                        "{label}: {key} {why}, the rule never matches"
                    )));
                }
            }
            if shadowed_by.is_none() && always_matches(rule, &path_names) {
                shadowed_by = Some(label.clone());
            }
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
        if let (Some(first), Some(_)) = (&shadowed_by, &o.respond) {
            findings.push(Finding::warning(format!(
                "the operation's respond override is never used: {first} matches every request"
            )));
        }
    }
    findings.extend(header_findings(c, op));
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

/// Why a condition can never hold, if it cannot.
fn contradiction(key: &str, cond: &Condition, path_names: &[&str]) -> Option<String> {
    let wants_value = cond.equals.is_some() || cond.matches.is_some() || cond.exists == Some(true);
    if cond.exists == Some(false) {
        if wants_value {
            return Some("must both be absent and have a value".into());
        }
        if key
            .strip_prefix("path.")
            .is_some_and(|n| path_names.contains(&n))
        {
            return Some("is a path parameter and always present".into());
        }
    }
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if let (Some(a), Some(b)) = (&cond.equals, &cond.not_equals) {
        if text(a) == text(b) {
            return Some(format!("must both equal and not equal {:?}", text(a)));
        }
    }
    if let (Some(v), Some(p)) = (&cond.equals, &cond.matches) {
        if schema::cached_regex(p).is_some_and(|re| !re.is_match(&text(v))) {
            return Some(format!(
                "must equal {:?}, which does not match {p:?}",
                text(v)
            ));
        }
    }
    None
}

/// A rule without conditions, or one that only asks for path parameters to
/// exist, matches every request of its operation.
fn always_matches(rule: &Rule, path_names: &[&str]) -> bool {
    rule.when.iter().all(|(key, cond)| {
        let is_path = key
            .strip_prefix("path.")
            .is_some_and(|n| path_names.contains(&n));
        is_path
            && cond.equals.is_none()
            && cond.not_equals.is_none()
            && cond.matches.is_none()
            && cond.exists != Some(false)
    })
}

/// Documented response headers whose example or generated value cannot be
/// sent as a header value (they are left out of the response).
fn header_findings(c: &Compiled, op: &Operation) -> Vec<Finding> {
    let mut out = Vec::new();
    for status in op.documented_statuses().into_iter().take(MAX_STATUSES) {
        let Some(def) = op.response_for(status) else {
            continue;
        };
        if def.headers.is_empty() {
            continue;
        }
        let sent: Vec<String> = respond::spec_headers(c, op, status)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        for h in &def.headers {
            if !sent.contains(&h.name) {
                out.push(Finding::warning(format!(
                    "response {status}: the value of header {:?} cannot be sent as a header value (control characters or line breaks), the header is left out",
                    h.name
                )));
            }
        }
    }
    out
}

/// Most `pattern` findings in one report.
const MAX_PATTERN_FINDINGS: usize = 20;

/// `pattern`s the generator cannot produce values for (the spec's examples
/// and defaults are data, not schemas, and are skipped).
fn unsupported_patterns(root: &Value) -> Vec<(String, String)> {
    fn walk(v: &Value, pointer: &mut String, out: &mut Vec<(String, String)>, depth: usize) {
        if depth > 64 || out.len() >= MAX_PATTERN_FINDINGS {
            return;
        }
        match v {
            Value::Object(m) => {
                if let Some(Value::String(p)) = m.get("pattern") {
                    if schema::pattern_string(p).is_none() && !out.iter().any(|(q, _)| q == p) {
                        out.push((p.clone(), format!("{pointer}/pattern")));
                    }
                }
                for (k, child) in m {
                    if matches!(
                        k.as_str(),
                        "example" | "examples" | "default" | "enum" | "const" | "x-examples"
                    ) {
                        continue;
                    }
                    let len = pointer.len();
                    pointer.push('/');
                    pointer.push_str(&k.replace('~', "~0").replace('/', "~1"));
                    walk(child, pointer, out, depth + 1);
                    pointer.truncate(len);
                }
            }
            Value::Array(a) => {
                for (i, child) in a.iter().enumerate() {
                    let len = pointer.len();
                    pointer.push_str(&format!("/{i}"));
                    walk(child, pointer, out, depth + 1);
                    pointer.truncate(len);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(root, &mut String::from("#"), &mut out, 0);
    out
}

/// Security settings that no request can satisfy, and schemes that are not
/// mocked (requests pass them unchecked).
fn security_findings(c: &Compiled) -> Vec<Finding> {
    let sec = &c.settings.security;
    let mut out = Vec::new();
    if !sec.enabled {
        return out;
    }
    let no_keys =
        "no API key is configured (security.api_key.keys): API key checks reject every call";
    let no_users =
        "no basic user is configured (security.basic.users): basic auth checks reject every call";
    match sec.mode {
        SecurityMode::ApiKey if sec.api_key.keys.is_empty() => {
            out.push(Finding::warning(no_keys));
        }
        SecurityMode::Basic if sec.basic.users.is_empty() => {
            out.push(Finding::warning(no_users));
        }
        SecurityMode::Spec => {
            let mut used: Vec<&str> = Vec::new();
            for op in &c.spec.operations {
                if c.settings.operations.get(&op.id).is_some_and(|o| o.public) {
                    continue;
                }
                for alt in &op.security {
                    for (name, _) in alt {
                        if !used.contains(&name.as_str()) {
                            used.push(name);
                        }
                    }
                }
            }
            let (mut keys, mut users) = (false, false);
            for name in used {
                match c.spec.scheme(name).map(|s| &s.kind) {
                    Some(SchemeKind::ApiKey { .. }) => keys = true,
                    Some(SchemeKind::Basic) => users = true,
                    Some(SchemeKind::Unsupported { detail }) => out.push(Finding::warning(format!(
                        "security scheme {name:?} is not mocked ({detail}): requests pass it unchecked"
                    ))),
                    _ => {}
                }
            }
            if keys && sec.api_key.keys.is_empty() {
                out.push(Finding::warning(no_keys));
            }
            if users && sec.basic.users.is_empty() {
                out.push(Finding::warning(no_users));
            }
        }
        _ => {}
    }
    out
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
    findings.extend(security_findings(c));
    for (pattern, pointer) in unsupported_patterns(&c.spec.root) {
        findings.push(Finding::warning(format!(
            "pattern {pattern:?} at {pointer} is too complex for the generator: generated values \
             use a placeholder that may not match it (add an example to the schema)"
        )));
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
    fn unreachable_rules_headers_patterns_and_security() {
        const SPEC2: &str = r##"{
          "openapi": "3.0.3", "info": {"title": "t", "version": "1"},
          "paths": {"/items/{id}": {"get": {"operationId": "item",
            "security": [{"key": []}, {"weird": []}],
            "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "string", "pattern": "^(?=x)a$"}}],
            "responses": {"200": {"description": "ok",
              "headers": {
                "Content-Length": {"schema": {"type": "integer"}, "example": 3},
                "X-Bad": {"schema": {"type": "string"}, "example": "a\nb"},
                "X-Good": {"schema": {"type": "string"}, "example": "fine"}
              },
              "content": {"application/json": {"schema": {"type": "object"}, "example": {"pattern": "(?=ignored)"}}}}}}}},
          "components": {"securitySchemes": {
            "key": {"type": "apiKey", "name": "X-Key", "in": "header"},
            "weird": {"type": "http", "scheme": "digest"}
          }}
        }"##;
        let mut settings: MockSettings = serde_json::from_value(json!({
            "security": {"enabled": true, "api_key": {"keys": []}},
            "operations": {"item": {
                "rules": [
                    {"when": {"path.id": {"exists": false}}, "respond": {"status": 404}},
                    {"when": {"query.q": {"equals": "a", "not_equals": "a"}}, "respond": {"status": 400}},
                    {"name": "catch-all", "when": {"path.id": {"exists": true}}, "respond": {"status": 200}},
                    {"when": {"header.x": {"equals": "abc", "matches": "^z"}}, "respond": {"status": 418}}
                ],
                "respond": {"status": 200}
            }}
        }))
        .expect("settings");
        settings.responses.mode = ResponseMode::Examples;
        let c = compile(SPEC2, settings);
        let r = &c.report;
        assert!(
            r.spec_warnings
                .iter()
                .any(|w| w.contains("\"Content-Length\" is set by the server")),
            "{:?}",
            r.spec_warnings
        );
        let op = &r.operations[0];
        let messages: Vec<&str> = op.findings.iter().map(|f| f.message.as_str()).collect();
        let has = |needle: &str| messages.iter().any(|m| m.contains(needle));
        assert!(
            has("rule 1: path.id is a path parameter and always present"),
            "{messages:?}"
        );
        assert!(
            has("rule 2: query.q must both equal and not equal"),
            "{messages:?}"
        );
        assert!(
            has("rule 4 is never reached: rule \"catch-all\" matches every request"),
            "{messages:?}"
        );
        assert!(
            has("rule 4: header.x must equal \"abc\", which does not match"),
            "{messages:?}"
        );
        assert!(has("respond override is never used"), "{messages:?}");
        assert!(has("header \"X-Bad\" cannot be sent"), "{messages:?}");
        assert!(!has("X-Good"), "{messages:?}");

        let global: Vec<&str> = r.findings.iter().map(|f| f.message.as_str()).collect();
        assert!(
            global.iter().any(|m| m.contains("\"weird\" is not mocked")),
            "{global:?}"
        );
        assert!(
            global
                .iter()
                .any(|m| m.contains("no API key is configured")),
            "{global:?}"
        );
        let patterns: Vec<&&str> = global.iter().filter(|m| m.contains("pattern")).collect();
        assert_eq!(patterns.len(), 1, "{global:?}");
        assert!(patterns[0].contains("#/paths/~1items~1{id}/get/parameters/0/schema/pattern"));
        assert!(r.ok, "warnings only: {r:#?}");
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

    #[test]
    fn hidden_credentials_become_placeholders() {
        for (mode, header, real, shown) in [
            (SecurityMode::ApiKey, "x-api-key", "demo-key", "<api-key>"),
            (
                SecurityMode::Bearer,
                "Authorization",
                "Bearer demo-token",
                "Bearer <token>",
            ),
            (
                SecurityMode::Basic,
                "Authorization",
                "Basic ZGVtbzpkZW1v",
                "Basic <base64 user:password>",
            ),
        ] {
            let mut settings = MockSettings::default();
            settings.security.enabled = true;
            settings.security.mode = mode;
            let c = compile(SPEC, settings);
            let mut report = c.report.clone();
            let pair = |v: &str| (header.to_string(), v.to_string());
            assert!(report.operations[0].request.headers.contains(&pair(real)));
            hide_report_credentials(&c, &mut report);
            let headers = &report.operations[0].request.headers;
            assert!(headers.contains(&pair(shown)), "{mode:?}: {headers:?}");
            assert!(!headers.contains(&pair(real)), "{mode:?}: {headers:?}");
        }
    }
}
