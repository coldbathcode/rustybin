//! Arazzo 1.0 workflows as multi-step saved tests, and back.
//!
//! An Arazzo document describes journeys over the operations of OpenAPI
//! descriptions. Here its source descriptions are this instance's mocks
//! (by name, else the one mock that has the operations the document
//! uses): nothing is fetched. Each workflow becomes one multi-step
//! [`SavedTest`]: `operationId` and `operationPath` become concrete
//! requests from the mock's spec, `parameters` and `requestBody` map
//! directly, `successCriteria` of the simple and JSONPath kinds become
//! assertions, `outputs` stay outputs, workflow `inputs` become
//! `{{NAME}}` variables.
//!
//! As the harness imports: nothing runs, nothing is guessed. What has no
//! counterpart (an `onFailure` goto, a retry, a regex or XPath criterion, a
//! nested workflow, `replacements`) is named in `left_out` with its place
//! in the document; the step it belongs to is still imported when it can
//! run without it, and a workflow that cannot is left out whole, with why.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Map, Value};

use super::run::parse_path;
use super::{Assertion, SavedTest, TestRequest, TestStep, MAX_STEPS};
use crate::mocks::compile::{Compiled, RouteMatch};
use crate::mocks::spec::{Location, Operation};

/// Most workflows read from one document.
pub const MAX_WORKFLOWS: usize = 50;
/// Most names in `left_out`.
const MAX_LEFT_OUT: usize = 200;

/// A mock a source description may be: its name and the revision that
/// answers (the deployed one, else the draft).
pub type Mock = (String, Arc<Compiled>);

/// What an import made, and what it left out.
#[derive(Debug, Default, Serialize)]
pub struct Imported {
    /// One multi-step test a workflow that could be imported.
    pub tests: Vec<SavedTest>,
    /// What the document says that has no counterpart here, by its place
    /// in the document. Never run, never guessed.
    pub left_out: Vec<String>,
    /// Each source description and the mock it is on this instance.
    pub sources: BTreeMap<String, String>,
    /// Workflows in the document.
    pub workflows: usize,
}

fn note(list: &mut Vec<String>, what: impl Into<String>) {
    if list.len() < MAX_LEFT_OUT {
        list.push(what.into());
    }
}

/// Is this an Arazzo document (`arazzo: 1.0.x`)?
pub fn is_arazzo(doc: &Value) -> bool {
    doc.get("arazzo").is_some() && doc.get("workflows").is_some()
}

/// A `{{NAME}}` variable's name for a workflow input.
pub fn input_variable(name: &str) -> String {
    super::variable_name(name)
}

/// A JSON pointer (`/items/0/id`) as a path an assertion takes
/// (`$.items[0].id`).
pub fn pointer_path(pointer: &str) -> String {
    let mut out = String::from("$");
    for seg in pointer.split('/').skip(1) {
        let seg = seg.replace("~1", "/").replace("~0", "~");
        if !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()) {
            out.push_str(&format!("[{seg}]"));
        } else if !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            out.push_str(&format!(".{seg}"));
        } else {
            out.push_str(&format!("['{seg}']"));
        }
    }
    out
}

// ── Expressions ─────────────────────────────────────────────────────

/// One runtime expression as the text a test's request takes: an input is
/// a variable, an earlier step's output a step reference. `Err` names an
/// expression with no counterpart.
fn expression(expr: &str) -> Result<String, String> {
    let expr = expr.trim();
    if let Some(name) = expr.strip_prefix("$inputs.") {
        if !name.is_empty() && !name.contains(['.', '#', ' ']) {
            return Ok(format!("{{{{{}}}}}", input_variable(name)));
        }
    }
    if let Some(rest) = expr.strip_prefix("$steps.") {
        if let Some((id, name)) = rest.split_once(".outputs.") {
            let word = |w: &str| {
                !w.is_empty()
                    && w.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            };
            if word(id) && word(name) {
                return Ok(format!("{{{{steps.{id}.outputs.{name}}}}}"));
            }
        }
    }
    Err(expr.to_string())
}

/// A value of a parameter or a payload as text: a literal, an expression
/// (`$inputs.x`), or a string with embedded ones (`Bearer {$inputs.x}`).
fn text_of(v: &Value) -> Result<String, String> {
    let s = match v {
        Value::String(s) => s,
        Value::Null => return Ok(String::new()),
        other => return Ok(other.to_string()),
    };
    if s.starts_with('$') && !s.contains(' ') {
        return expression(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(start) = rest.find("{$") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        out.push_str(&expression(&after[..end])?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A payload with its expressions translated, in place.
fn translate(v: &mut Value) -> Result<(), String> {
    match v {
        Value::String(_) => {
            *v = Value::String(text_of(v)?);
            Ok(())
        }
        Value::Array(a) => a.iter_mut().try_for_each(translate),
        Value::Object(o) => o.values_mut().try_for_each(translate),
        _ => Ok(()),
    }
}

/// A literal of a criterion: a number, `true`, `false`, `null`, or a
/// quoted string.
fn literal(raw: &str) -> Option<Value> {
    let raw = raw.trim();
    for q in ['\'', '"'] {
        if raw.len() >= 2 && raw.starts_with(q) && raw.ends_with(q) {
            return Some(Value::String(raw[1..raw.len() - 1].to_string()));
        }
    }
    match raw {
        "true" => Some(json!(true)),
        "false" => Some(json!(false)),
        "null" => Some(Value::Null),
        _ => raw
            .parse::<i64>()
            .map(|n| json!(n))
            .ok()
            .or_else(|| raw.parse::<f64>().ok().map(|n| json!(n))),
    }
}

/// One success criterion as an assertion, when it is of the subset: a
/// simple `$statusCode == 200`, `$response.header.X == 'v'` or
/// `$response.body#/pointer == value`, or a JSONPath criterion on the
/// body that is a path (`$.a.b`) or a path and a value (`$.a.b == 1`).
fn criterion(c: &Value) -> Option<Assertion> {
    let condition = c["condition"].as_str()?.trim();
    let kind = match &c["type"] {
        Value::Null => "simple",
        Value::String(s) => s.as_str(),
        // A criterion expression type object (a versioned JSONPath or XPath).
        other => other["type"].as_str()?,
    };
    let (lhs, rhs) = match condition.split_once("==") {
        Some((l, r)) if !r.starts_with('=') => (l.trim(), Some(literal(r)?)),
        _ => (condition, None),
    };
    if lhs.contains(['&', '|', '<', '>', '!']) || lhs.contains(' ') {
        return None;
    }
    match kind {
        "simple" => {
            let rhs = rhs?;
            if lhs == "$statusCode" {
                let code = u16::try_from(rhs.as_u64()?).ok()?;
                return (100..=599).contains(&code).then(|| Assertion::status(code));
            }
            if let Some(h) = lhs.strip_prefix("$response.header.") {
                return rhs.is_string().then(|| Assertion {
                    header: Some(h.to_string()),
                    equals: Some(rhs),
                    ..Assertion::default()
                });
            }
            let pointer = lhs.strip_prefix("$response.body#")?;
            let path = pointer_path(pointer);
            parse_path(&path).ok()?;
            Some(Assertion {
                json: Some(path),
                equals: Some(rhs),
                ..Assertion::default()
            })
        }
        "jsonpath" => {
            if c["context"].as_str().map(str::trim) != Some("$response.body") {
                return None;
            }
            parse_path(lhs).ok()?;
            Some(Assertion {
                json: Some(lhs.to_string()),
                equals: rhs,
                ..Assertion::default()
            })
        }
        _ => None,
    }
}

// ── Sources ─────────────────────────────────────────────────────────

/// The operation a step names, as (source description if named, id).
fn operation_ref(step: &Value) -> Option<(Option<&str>, &str)> {
    let id = step["operationId"].as_str()?.trim();
    match id.strip_prefix("$sourceDescriptions.") {
        Some(rest) => rest.split_once('.').map(|(s, i)| (Some(s), i)),
        None => Some((None, id)),
    }
}

/// `{$sourceDescriptions.<name>.url}#/paths/~1pets~1{id}/get` as the
/// source, the path template and the method.
fn operation_path(raw: &str) -> Option<(&str, String, String)> {
    let rest = raw.trim().strip_prefix("{$sourceDescriptions.")?;
    let (source, rest) = rest.split_once(".url}")?;
    let pointer = rest.strip_prefix("#/paths/")?;
    let (path, method) = pointer.rsplit_once('/')?;
    Some((
        source,
        path.replace("~1", "/").replace("~0", "~"),
        method.to_ascii_uppercase(),
    ))
}

fn find_op<'a>(mock: &'a Compiled, id: &str) -> Option<&'a Operation> {
    mock.spec
        .operations
        .iter()
        .find(|o| o.id == id && !o.synthetic_id)
}

/// The mock each OpenAPI source description is: the mock of its name,
/// else the one mock that has every operation the document uses from it.
fn resolve_sources<'a>(
    doc: &Value,
    mocks: &'a [Mock],
    left_out: &mut Vec<String>,
) -> Result<BTreeMap<String, &'a Mock>, String> {
    let sources = doc["sourceDescriptions"]
        .as_array()
        .filter(|s| !s.is_empty())
        .ok_or("sourceDescriptions: an Arazzo document names at least one")?;
    let openapi: Vec<&str> = sources
        .iter()
        .filter(|s| s["type"].as_str().unwrap_or("openapi") == "openapi")
        .filter_map(|s| s["name"].as_str())
        .collect();
    let mut out = BTreeMap::new();
    for (i, s) in sources.iter().enumerate() {
        let name = s["name"]
            .as_str()
            .ok_or_else(|| format!("sourceDescriptions[{i}]: no name"))?;
        if s["type"].as_str().unwrap_or("openapi") != "openapi" {
            note(
                left_out,
                format!(
                    "sourceDescriptions.{name} (type {}: another Arazzo document is not read)",
                    s["type"]
                ),
            );
            continue;
        }
        if let Some(m) = mocks.iter().find(|(n, _)| n == name) {
            out.insert(name.to_string(), m);
            continue;
        }
        // The operations the document uses from this source: the ones
        // named with it, and the plain ones when it is the only source.
        let mut ids = BTreeSet::new();
        for step in doc["workflows"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|w| w["steps"].as_array().into_iter().flatten())
        {
            match operation_ref(step) {
                Some((Some(src), id)) if src == name => {
                    ids.insert(id);
                }
                Some((None, id)) if openapi.len() == 1 => {
                    ids.insert(id);
                }
                _ => {}
            }
        }
        let fits: Vec<&Mock> = mocks
            .iter()
            .filter(|(_, c)| !ids.is_empty() && ids.iter().all(|id| find_op(c, id).is_some()))
            .collect();
        match fits.as_slice() {
            [one] => {
                out.insert(name.to_string(), *one);
            }
            [] => {
                return Err(format!(
                    "sourceDescriptions.{name}: no mock on this instance is named {name} or has the operations the workflows use from it ({}): create a mock named {name} from its OpenAPI description first (the description's url is never fetched)",
                    if ids.is_empty() { "none by operationId".to_string() } else { ids.into_iter().collect::<Vec<_>>().join(", ") }
                ))
            }
            many => {
                return Err(format!(
                    "sourceDescriptions.{name}: {} mocks have the operations the workflows use from it ({}): name the source description after the one meant",
                    many.len(),
                    many.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
                ))
            }
        }
    }
    if out.is_empty() {
        return Err("sourceDescriptions: none is an OpenAPI description".into());
    }
    Ok(out)
}

// ── A step ──────────────────────────────────────────────────────────

/// A path segment or a query value as it goes into a path: a template
/// stays, anything else is percent-encoded.
fn encoded(value: &str) -> String {
    if value.contains("{{") {
        return value.to_string();
    }
    form_urlencoded::byte_serialize(value.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

/// One step as a test step. `Err` is why the step, and so its workflow,
/// cannot be imported; what it can run without goes to `left_out`.
fn read_step(
    at: &str,
    step: &Value,
    workflow_params: &[Value],
    sources: &BTreeMap<String, &Mock>,
    left_out: &mut Vec<String>,
) -> Result<TestStep, String> {
    let id = step["stepId"].as_str().unwrap_or_default().to_string();
    if step.get("workflowId").is_some() {
        return Err("a nested workflow (workflowId) is not run".into());
    }
    // The operation, and the mock that serves it.
    let (mock, op): (&Mock, &Operation) = if let Some((source, op_id)) = operation_ref(step) {
        let candidates: Vec<&Mock> = match source {
            Some(s) => sources.get(s).copied().into_iter().collect(),
            None => sources.values().copied().collect(),
        };
        let mut found: Vec<(&Mock, &Operation)> = candidates
            .into_iter()
            .filter_map(|m| find_op(&m.1, op_id).map(|o| (m, o)))
            .collect();
        found.dedup_by(|a, b| a.0 .0 == b.0 .0);
        match found.as_slice() {
            [one] => *one,
            [] => return Err(format!("operationId {op_id}: no source description has it")),
            _ => {
                return Err(format!(
                    "operationId {op_id}: more than one source description has it: name the source ($sourceDescriptions.<name>.{op_id})"
                ))
            }
        }
    } else if let Some(raw) = step["operationPath"].as_str() {
        let (source, path, method) = operation_path(raw).ok_or_else(|| {
            format!("operationPath {raw}: expected {{$sourceDescriptions.<name>.url}}#/paths/<path>/<method>")
        })?;
        let mock = sources
            .get(source)
            .copied()
            .ok_or_else(|| format!("operationPath: no source description {source}"))?;
        let op = mock
            .1
            .spec
            .operations
            .iter()
            .find(|o| o.path == path && o.method == method)
            .ok_or_else(|| {
                format!("operationPath: {method} {path} is not an operation of {source}")
            })?;
        (mock, op)
    } else {
        return Err("no operationId or operationPath".into());
    };

    // Parameters: the workflow's, then the step's (which win by name and place).
    let mut params: Vec<&Value> = Vec::new();
    for p in workflow_params
        .iter()
        .chain(step["parameters"].as_array().into_iter().flatten())
    {
        if p.get("reference").is_some() || p.get("$ref").is_some() {
            note(
                left_out,
                format!("{at}.parameters (a reusable reference is not followed)"),
            );
            continue;
        }
        params.retain(|q| !(q["name"] == p["name"] && q["in"] == p["in"]));
        params.push(p);
    }
    let mut path = op.path.clone();
    let mut query: Vec<String> = Vec::new();
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    for p in params {
        let name = p["name"].as_str().unwrap_or_default();
        let place = match p["in"].as_str() {
            Some(l) => l.to_string(),
            // Arazzo lets `in` go when the operation has one parameter of that name.
            None => match op.parameters.iter().find(|q| q.name == name) {
                Some(q) => match q.location {
                    Location::Path => "path",
                    Location::Query => "query",
                    Location::Header => "header",
                    Location::Cookie => "cookie",
                }
                .to_string(),
                None => String::new(),
            },
        };
        let value = match text_of(&p["value"]) {
            Ok(v) => v,
            Err(expr) => {
                note(
                    left_out,
                    format!("{at}.parameters.{name} (the expression {expr} has no counterpart)"),
                );
                continue;
            }
        };
        match place.as_str() {
            "path" => path = path.replace(&format!("{{{name}}}"), &encoded(&value)),
            "query" => query.push(format!("{}={}", encoded(name), encoded(&value))),
            "header" => {
                headers.insert(name.to_ascii_lowercase(), value);
            }
            other => note(left_out, format!("{at}.parameters.{name} (in: {other})")),
        }
    }
    // What is left of the template: a path parameter without a value.
    if path.replace("{{", "").contains('{') {
        return Err(format!(
            "the path {path} still has a parameter without a value"
        ));
    }
    let mut full = format!("/mock/{}{}{}", mock.0, mock.1.path_prefix(), path);
    if !query.is_empty() {
        full.push('?');
        full.push_str(&query.join("&"));
    }

    let mut body = None;
    if let Some(rb) = step.get("requestBody").filter(|b| !b.is_null()) {
        if rb.get("replacements").is_some() {
            note(left_out, format!("{at}.requestBody.replacements"));
        }
        if let Some(payload) = rb.get("payload").filter(|p| !p.is_null()) {
            let mut payload = payload.clone();
            match translate(&mut payload) {
                Ok(()) => {
                    body = Some(match &payload {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    });
                    let content_type = rb["contentType"].as_str().unwrap_or("application/json");
                    headers
                        .entry("content-type".into())
                        .or_insert_with(|| content_type.to_string());
                }
                Err(expr) => {
                    return Err(format!(
                        "requestBody.payload: the expression {expr} has no counterpart"
                    ))
                }
            }
        }
    }

    let mut assert = Vec::new();
    for (i, c) in step["successCriteria"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        match criterion(c) {
            Some(a) => assert.push(a),
            None => note(
                left_out,
                format!(
                    "{at}.successCriteria[{i}] ({}: not a criterion of the subset)",
                    c["condition"].as_str().unwrap_or("?")
                ),
            ),
        }
    }
    let mut outputs = BTreeMap::new();
    for (name, expr) in step["outputs"].as_object().into_iter().flatten() {
        let ok = expr
            .as_str()
            .filter(|e| super::valid_output(e).is_ok())
            .filter(|_| super::valid_step_id(name));
        match ok {
            Some(e) => {
                outputs.insert(name.clone(), e.trim().to_string());
            }
            None => note(
                left_out,
                format!("{at}.outputs.{name} ({expr}: not an output of the subset)"),
            ),
        }
    }
    for key in ["onSuccess", "onFailure"] {
        for (i, action) in step[key].as_array().into_iter().flatten().enumerate() {
            note(
                left_out,
                format!(
                    "{at}.{key}[{i}] ({})",
                    action["type"].as_str().unwrap_or("an action")
                ),
            );
        }
    }
    Ok(TestStep {
        id,
        description: step["description"].as_str().map(str::to_string),
        request: TestRequest {
            method: op.method.clone(),
            path: full,
            headers,
            body,
        },
        assert,
        outputs,
    })
}

/// Read an Arazzo document against the mocks the caller may read.
pub fn import(doc: &Value, mocks: &[Mock]) -> Result<Imported, String> {
    let version = doc["arazzo"].as_str().unwrap_or_default();
    if !version.starts_with("1.0") {
        return Err(format!(
            "not an Arazzo 1.0 document (arazzo: {})",
            if version.is_empty() {
                "missing"
            } else {
                version
            }
        ));
    }
    let workflows = doc["workflows"]
        .as_array()
        .filter(|w| !w.is_empty())
        .ok_or("workflows: an Arazzo document has at least one")?;
    let mut out = Imported {
        workflows: workflows.len(),
        ..Imported::default()
    };
    let sources = resolve_sources(doc, mocks, &mut out.left_out)?;
    out.sources = sources
        .iter()
        .map(|(k, m)| (k.clone(), m.0.clone()))
        .collect();
    if doc.get("components").is_some() {
        note(
            &mut out.left_out,
            "components (reusable inputs, parameters and actions are not followed)",
        );
    }
    for (n, w) in workflows.iter().enumerate() {
        let id = w["workflowId"].as_str().unwrap_or_default();
        let at = if id.is_empty() {
            format!("workflows[{n}]")
        } else {
            format!("workflows.{id}")
        };
        if n >= MAX_WORKFLOWS {
            note(
                &mut out.left_out,
                format!("{at} (more than {MAX_WORKFLOWS} workflows)"),
            );
            continue;
        }
        if w.get("dependsOn").is_some() {
            note(
                &mut out.left_out,
                format!("{at}.dependsOn (run the workflows it names first)"),
            );
        }
        if w.get("outputs").is_some() {
            note(
                &mut out.left_out,
                format!("{at}.outputs (a test keeps each step's outputs)"),
            );
        }
        for key in ["successActions", "failureActions"] {
            if w.get(key).is_some() {
                note(&mut out.left_out, format!("{at}.{key}"));
            }
        }
        let steps = w["steps"].as_array().cloned().unwrap_or_default();
        let params: Vec<Value> = w["parameters"].as_array().cloned().unwrap_or_default();
        let mut pending = Vec::new();
        let mut test_steps = Vec::new();
        let mut failed = None;
        if steps.is_empty() {
            failed = Some("no steps".to_string());
        } else if steps.len() > MAX_STEPS {
            failed = Some(format!(
                "{} steps: a test has at most {MAX_STEPS}",
                steps.len()
            ));
        }
        for (i, step) in steps.iter().enumerate() {
            if failed.is_some() {
                break;
            }
            let sid = step["stepId"].as_str().unwrap_or_default();
            let step_at = if sid.is_empty() {
                format!("{at}.steps[{i}]")
            } else {
                format!("{at}.steps.{sid}")
            };
            match read_step(&step_at, step, &params, &sources, &mut pending) {
                Ok(s) => test_steps.push(s),
                Err(why) => failed = Some(format!("{step_at}: {why}")),
            }
        }
        let test = SavedTest {
            name: id.to_string(),
            description: w["summary"]
                .as_str()
                .or(w["description"].as_str())
                .map(|d| d.chars().take(500).collect()),
            request: TestRequest::default(),
            steps: test_steps,
            gateway: None,
            assert: Vec::new(),
            project: None,
        };
        // Validated as a saved test is: an import never makes one a save
        // would refuse.
        let failed = failed.or_else(|| test.validate().err());
        match failed {
            None => {
                out.left_out.append(&mut pending);
                out.tests.push(test);
            }
            Some(why) => note(&mut out.left_out, format!("{at} (not imported: {why})")),
        }
    }
    out.left_out.truncate(MAX_LEFT_OUT);
    Ok(out)
}

// ── Export ──────────────────────────────────────────────────────────

/// A test's text back as an Arazzo value: a variable is an input, a step
/// reference a step's output.
fn untranslate(text: &str) -> Value {
    let whole = text.trim();
    let one = |inner: &str| -> Option<String> {
        let inner = inner.trim();
        if let Some(rest) = inner.strip_prefix("steps.") {
            return Some(format!("$steps.{rest}"));
        }
        (!inner.is_empty() && inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            .then(|| format!("$inputs.{inner}"))
    };
    if let Some(inner) = whole.strip_prefix("{{").and_then(|r| r.strip_suffix("}}")) {
        if !inner.contains("{{") {
            if let Some(e) = one(inner) {
                return Value::String(e);
            }
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        match one(&after[..end]) {
            Some(e) => out.push_str(&format!("{{{e}}}")),
            None => out.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Value::String(out)
}

fn untranslate_in(v: &mut Value) {
    match v {
        Value::String(s) => *v = untranslate(s),
        Value::Array(a) => a.iter_mut().for_each(untranslate_in),
        Value::Object(o) => o.values_mut().for_each(untranslate_in),
        _ => {}
    }
}

/// A saved test as an Arazzo document: one workflow, a step a request.
/// Every request must be an operation of a mock (Arazzo describes
/// operations of OpenAPI descriptions): `Err` names the one that is not.
/// Answers the document and the assertions Arazzo's subset cannot say.
pub fn export(
    test: &SavedTest,
    mocks: &[Mock],
    base_url: &str,
) -> Result<(Value, Vec<String>), String> {
    let single;
    let steps: &[TestStep] = if test.steps.is_empty() {
        single = [TestStep {
            id: "request".into(),
            description: None,
            request: test.request.clone(),
            assert: test.assert.clone(),
            outputs: BTreeMap::new(),
        }];
        &single
    } else {
        &test.steps
    };
    let mut left_out = Vec::new();
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut found = Vec::new();
    for step in steps {
        let r = &step.request;
        let (path, query) = match r.path.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (r.path.as_str(), None),
        };
        let not_a_mock = || {
            format!(
                "step {}: {} {} is not an operation of a mock on this instance (Arazzo describes operations of OpenAPI descriptions)",
                step.id, r.method, path
            )
        };
        let rest = path.strip_prefix("/mock/").ok_or_else(not_a_mock)?;
        let (name, below) = match rest.split_once('/') {
            Some((n, b)) => (n, format!("/{b}")),
            None => (rest, "/".to_string()),
        };
        let mock = mocks
            .iter()
            .find(|(n, _)| n == name)
            .ok_or_else(not_a_mock)?;
        let RouteMatch::Found { op, params } = mock.1.match_route(&r.method, &below) else {
            return Err(not_a_mock());
        };
        used.insert(name.to_string());
        found.push((step, name.to_string(), op, params, query));
    }
    let many = used.len() > 1;
    let mut out_steps = Vec::new();
    for (step, name, op, params, query) in found {
        let mut s = Map::new();
        s.insert("stepId".into(), json!(step.id));
        if let Some(d) = &step.description {
            s.insert("description".into(), json!(d));
        }
        if op.synthetic_id {
            let pointer = op.path.replace('~', "~0").replace('/', "~1");
            s.insert(
                "operationPath".into(),
                json!(format!(
                    "{{$sourceDescriptions.{name}.url}}#/paths/{pointer}/{}",
                    op.method.to_ascii_lowercase()
                )),
            );
        } else if many {
            s.insert(
                "operationId".into(),
                json!(format!("$sourceDescriptions.{name}.{}", op.id)),
            );
        } else {
            s.insert("operationId".into(), json!(op.id));
        }
        let mut parameters = Vec::new();
        for (k, v) in params {
            let v = percent_decode(&v);
            parameters.push(json!({ "name": k, "in": "path", "value": untranslate(&v) }));
        }
        for (k, v) in query
            .into_iter()
            .flat_map(|q| form_urlencoded::parse(q.as_bytes()))
        {
            parameters.push(json!({ "name": k, "in": "query", "value": untranslate(&v) }));
        }
        let r = &step.request;
        for (k, v) in &r.headers {
            if k.eq_ignore_ascii_case("content-type") && r.body.is_some() {
                continue;
            }
            parameters.push(json!({ "name": k, "in": "header", "value": untranslate(v) }));
        }
        if !parameters.is_empty() {
            s.insert("parameters".into(), json!(parameters));
        }
        if let Some(b) = &r.body {
            let content_type = r
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                .map(|(_, v)| v.as_str())
                .unwrap_or("application/json");
            let mut payload = serde_json::from_str::<Value>(b)
                .ok()
                .filter(|v| v.is_object() || v.is_array())
                .unwrap_or_else(|| json!(b));
            untranslate_in(&mut payload);
            s.insert(
                "requestBody".into(),
                json!({ "contentType": content_type, "payload": payload }),
            );
        }
        let mut criteria = Vec::new();
        for a in &step.assert {
            let lit = |v: &Value| match v {
                Value::String(s) => format!("'{s}'"),
                other => other.to_string(),
            };
            if let Some(code) = a.status {
                criteria.push(json!({ "condition": format!("$statusCode == {code}") }));
            } else if let (Some(h), Some(v)) = (&a.header, &a.equals) {
                criteria
                    .push(json!({ "condition": format!("$response.header.{h} == {}", lit(v)) }));
            } else if let Some(p) = &a.json {
                let condition = match &a.equals {
                    Some(v) if !v.is_object() && !v.is_array() => format!("{p} == {}", lit(v)),
                    Some(_) => {
                        left_out.push(format!(
                            "step {}: {} equals an object or a list",
                            step.id,
                            a.label()
                        ));
                        continue;
                    }
                    None => p.clone(),
                };
                criteria.push(json!({ "context": "$response.body", "condition": condition, "type": "jsonpath" }));
            } else {
                left_out.push(format!("step {}: {}", step.id, a.label()));
            }
        }
        if !criteria.is_empty() {
            s.insert("successCriteria".into(), json!(criteria));
        }
        if !step.outputs.is_empty() {
            s.insert("outputs".into(), json!(step.outputs));
        }
        out_steps.push(Value::Object(s));
    }
    let mut workflow = Map::new();
    workflow.insert("workflowId".into(), json!(test.name));
    if let Some(d) = &test.description {
        workflow.insert("summary".into(), json!(d));
    }
    let variables = test.variables();
    if !variables.is_empty() {
        let props: Map<String, Value> = variables
            .iter()
            .map(|v| (v.clone(), json!({ "type": "string" })))
            .collect();
        workflow.insert(
            "inputs".into(),
            json!({ "type": "object", "properties": props }),
        );
    }
    workflow.insert("steps".into(), json!(out_steps));
    if test.gateway.is_some() {
        left_out.push(
            "gateway (an Arazzo document names descriptions, not where they are served)".into(),
        );
    }
    let base = base_url.trim_end_matches('/');
    let doc = json!({
        "arazzo": "1.0.1",
        "info": { "title": test.name, "version": "1.0.0" },
        "sourceDescriptions": used.iter().map(|n| json!({
            "name": n,
            "url": format!("{base}/mock/{n}/_rustybin/openapi.json"),
            "type": "openapi"
        })).collect::<Vec<_>>(),
        "workflows": [workflow]
    });
    Ok((doc, left_out))
}

fn percent_decode(s: &str) -> String {
    if s.contains("{{") {
        return s.to_string();
    }
    form_urlencoded::parse(format!("x={}", s.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| s.to_string())
}
