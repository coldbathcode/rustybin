//! Contract drift: a mock says what an API does, the real API may have moved
//! on. A drift check sends one valid request per operation (the mock's own
//! sample, as the deploy checks make it) to the real API the mock imitates
//! and reports where the answers disagree with the spec: a status the spec
//! does not declare, a body that breaks its schema, a field it does not name,
//! an operation the API no longer answers.
//!
//! It reaches out on a caller's word, under the rules of recordings: it needs
//! `mocks.write`, goes through the outbound policy under `Purpose::Record`
//! (no default), never follows a redirect, and is bounded (at most
//! [`MAX_OPERATIONS`] requests, [`DEADLINE`] in all). Credentials for the real
//! API come from the request's `headers`, whose values may name `{{VAR}}`
//! variables given with the run or as `RUSTYBIN_TEST_<VAR>` in the
//! environment: never kept, never in a run. The mock's own fake credentials
//! are never sent. A check never changes the mock.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};

use super::checks;
use super::compile::Compiled;
use super::respond::Body;
use super::service::{ServiceError, Svc, SvcResult};
use super::spec::Operation;
use crate::egress::Purpose;

/// Most operations a check sends a request for.
pub const MAX_OPERATIONS: usize = 200;
/// The longest a whole check takes, and one request.
pub const DEADLINE: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Most of an answer read.
const MAX_ANSWER: usize = 1 << 20;
/// Most findings of one kind kept per operation.
const MAX_ISSUES: usize = 5;
/// Runs kept per mock, and mocks with runs.
pub const RUNS_PER_MOCK: usize = 10;
pub const MAX_MOCKS: usize = 50;

/// What a drift check takes.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriftBody {
    /// The real API's base URL (default: the mock's `drift.upstream`).
    #[serde(default)]
    pub upstream: Option<String>,
    /// Headers for the real API (a credential); values may hold `{{VAR}}`.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Values of the `{{VAR}}` variables (never kept).
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    /// Only these operationIds (default: every one).
    #[serde(default)]
    pub operations: Option<Vec<String>>,
}

/// The runs kept per mock (`AppState.drift`).
#[derive(Default)]
pub struct DriftRuns {
    runs: Mutex<BTreeMap<String, VecDeque<Value>>>,
}

impl DriftRuns {
    fn keep(&self, mock: &str, run: Value) {
        let mut m = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        if !m.contains_key(mock) && m.len() >= MAX_MOCKS {
            // The mock whose newest run is the oldest goes.
            let oldest = m
                .iter()
                .min_by(|a, b| {
                    let at = |q: &VecDeque<Value>| {
                        q.back()
                            .and_then(|r| r["checked_at"].as_str().map(str::to_string))
                            .unwrap_or_default()
                    };
                    at(a.1).cmp(&at(b.1))
                })
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                m.remove(&k);
            }
        }
        let q = m.entry(mock.to_string()).or_default();
        q.push_back(run);
        while q.len() > RUNS_PER_MOCK {
            q.pop_front();
        }
    }

    /// A mock's kept runs, newest first.
    pub fn list(&self, mock: &str) -> Vec<Value> {
        let m = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        m.get(mock)
            .map(|q| q.iter().rev().cloned().collect())
            .unwrap_or_default()
    }

    /// Forget a mock's runs (it was deleted).
    pub fn forget(&self, mock: &str) {
        self.runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(mock);
    }
}

/// `{{VAR}}` replaced from `vars`, else `RUSTYBIN_TEST_<VAR>`; a variable
/// with neither is an error naming it.
fn expand(text: &str, vars: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(j) = after.find("}}") else {
            out.push_str(&rest[i..]);
            return Ok(out);
        };
        let name = after[..j].trim();
        let value = vars
            .get(name)
            .cloned()
            .or_else(|| std::env::var(format!("RUSTYBIN_TEST_{name}")).ok())
            .ok_or_else(|| {
                format!("variable {name} is not given (variables, or RUSTYBIN_TEST_{name})")
            })?;
        out.push_str(&value);
        rest = &after[j + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A schema with `$ref`s followed (a few hops).
fn deref<'a>(mut s: &'a Value, root: &'a Value) -> &'a Value {
    for _ in 0..8 {
        match s.get("$ref").and_then(Value::as_str) {
            Some(r) => match r.strip_prefix('#').and_then(|p| root.pointer(p)) {
                Some(t) => s = t,
                None => return s,
            },
            None => return s,
        }
    }
    s
}

/// Fields of `value` its schema does not name (where extra properties are
/// not forbidden: those are schema violations), as JSON pointers.
fn undocumented(value: &Value, schema: &Value, root: &Value, at: &str, out: &mut Vec<String>) {
    let schema = deref(schema, root);
    match value {
        Value::Object(m) => {
            let props = schema.get("properties").and_then(Value::as_object);
            let open = schema.get("additionalProperties").is_none();
            for (k, v) in m {
                match props.and_then(|p| p.get(k)) {
                    Some(ps) => undocumented(v, ps, root, &format!("{at}/{k}"), out),
                    None if props.is_some() && open => out.push(format!("{at}/{k}")),
                    None => {}
                }
            }
        }
        Value::Array(items) => {
            if let (Some(first), Some(is)) = (items.first(), schema.get("items")) {
                undocumented(first, is, root, &format!("{at}/0"), out);
            }
        }
        _ => {}
    }
}

/// The findings of one operation's answer.
fn judge(
    c: &Compiled,
    op: &Operation,
    status: u16,
    content_type: Option<&str>,
    body: &[u8],
) -> Vec<Value> {
    let mut findings = Vec::new();
    if status == 404 || status == 405 {
        findings.push(json!({ "kind": "unreachable", "detail": format!("the API answered {status}: the operation was removed or renamed") }));
        return findings;
    }
    if op.response_for(status).is_none() && !op.responses.is_empty() {
        let declared: Vec<&str> = op.responses.iter().map(|r| r.status.as_str()).collect();
        findings.push(json!({ "kind": "status_differs", "detail": format!("{status}, the spec declares {}", declared.join(", ")) }));
        return findings;
    }
    let json = content_type.is_none_or(|c| c.contains("json"));
    let text = String::from_utf8_lossy(body);
    let parsed: Option<Value> = if json {
        serde_json::from_str(&text).ok()
    } else {
        None
    };
    let as_body = match &parsed {
        Some(v) => Body::Json(v.clone()),
        None => Body::Text(text.into_owned()),
    };
    if body.is_empty() {
        return findings;
    }
    for i in checks::validate_body(c, op, status, content_type, Some(&as_body))
        .iter()
        .take(MAX_ISSUES)
    {
        let kind = if i.message.contains("required") {
            "missing_field"
        } else {
            "schema_violation"
        };
        findings.push(json!({ "kind": kind, "detail": format!("{} {}", if i.pointer.is_empty() { "/" } else { &i.pointer }, i.message) }));
    }
    if let (Some(v), Some(def)) = (&parsed, op.response_for(status)) {
        let schema = def
            .content
            .iter()
            .find(|m| m.is_json())
            .and_then(|m| m.schema.as_ref());
        if let Some(schema) = schema {
            let mut extra = Vec::new();
            undocumented(v, schema, &c.spec.root, "", &mut extra);
            for p in extra.iter().take(MAX_ISSUES) {
                findings.push(json!({ "kind": "undocumented_field", "detail": format!("{p} is not in the schema"), "warning": true }));
            }
        }
    }
    findings
}

/// Run a drift check of mock `name` (`mocks.write`): answers the run, kept
/// with the mock's last [`RUNS_PER_MOCK`].
pub async fn run(svc: Svc, name: &str, body: DriftBody) -> SvcResult<Value> {
    let snap = svc.manageable(name)?;
    let c = snap.live.clone().unwrap_or_else(|| snap.draft.clone());
    let upstream = body
        .upstream
        .clone()
        // The setting is the draft's (saving it deploys nothing), the
        // definition checked the deployed one.
        .or_else(|| {
            snap.draft
                .settings
                .drift
                .as_ref()
                .map(|d| d.upstream.clone())
        })
        .filter(|u| !u.trim().is_empty())
        .ok_or_else(|| {
            ServiceError::bad_request(
                "upstream: the real API's base URL (or set the mock's drift.upstream)",
            )
        })?;
    let upstream = upstream.trim().trim_end_matches('/').to_string();
    crate::egress::parse_url(&upstream, false)
        .map_err(|e| ServiceError::bad_request(format!("upstream: {e}")))?;
    // Headers, variables expanded first: a missing variable sends nothing.
    let mut headers = Vec::new();
    for (k, v) in &body.headers {
        let name = axum::http::HeaderName::from_bytes(k.as_bytes()).map_err(|_| {
            ServiceError::bad_request(format!("headers: {k:?} is not a header name"))
        })?;
        let value = expand(v, &body.variables).map_err(ServiceError::bad_request)?;
        let value = axum::http::HeaderValue::from_str(&value).map_err(|_| {
            ServiceError::bad_request(format!("headers: the value of {k} is not a header value"))
        })?;
        headers.push((name, value));
    }
    let state = svc.state.clone();
    let pinned = state
        .egress
        .resolve(&upstream, Purpose::Record)
        .await
        .map_err(|e| {
            ServiceError::with_body(
                StatusCode::FORBIDDEN,
                json!({ "error": e.0, "reason": "egress_refused", "purpose": "record" }),
            )
        })?;
    let client = state
        .egress
        .client(&pinned, REQUEST_TIMEOUT)
        .map_err(|e| ServiceError::new(StatusCode::BAD_GATEWAY, e))?;
    let started = Instant::now();
    let ops: Vec<&Operation> = c
        .spec
        .operations
        .iter()
        .filter(|o| body.operations.as_ref().is_none_or(|l| l.contains(&o.id)))
        .take(MAX_OPERATIONS)
        .collect();
    let mut results = Vec::new();
    let mut skipped = 0usize;
    for op in &ops {
        if started.elapsed() > DEADLINE {
            skipped += 1;
            continue;
        }
        let sample = checks::sample_request(&c, op);
        let (_, mock_query) = c.sample_credentials(op);
        let mock_query: Vec<String> = mock_query.into_iter().map(|(k, _)| k).collect();
        let query: Vec<(String, String)> = sample
            .query
            .iter()
            .filter(|(k, _)| !mock_query.contains(k))
            .cloned()
            .collect();
        // The sample's path is below the mock root, with the server prefix:
        // the real API's base URL already has it.
        let path = sample
            .path
            .strip_prefix(c.path_prefix())
            .unwrap_or(&sample.path)
            .to_string();
        let mut url = format!("{}{path}", pinned.url.trim_end_matches('/'));
        if !query.is_empty() {
            let q: String = form_urlencoded::Serializer::new(String::new())
                .extend_pairs(query.iter())
                .finish();
            url = format!("{url}?{q}");
        }
        let method =
            reqwest::Method::from_bytes(op.method.as_bytes()).unwrap_or(reqwest::Method::GET);
        let mut req = client.request(method, &url);
        for (k, v) in &headers {
            req = req.header(k, v);
        }
        if let Some(b) = &sample.body {
            req = req
                .header(
                    "content-type",
                    sample.content_type.as_deref().unwrap_or("application/json"),
                )
                .body(
                    if b.is_string()
                        && !sample
                            .content_type
                            .as_deref()
                            .is_some_and(|c| c.contains("json"))
                    {
                        b.as_str().unwrap_or("").to_string()
                    } else {
                        b.to_string()
                    },
                );
        }
        let t = Instant::now();
        let mut row = json!({ "operation": op.id, "method": op.method, "path": op.path });
        match req.send().await {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let ct = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let mut got = Vec::new();
                while let Ok(Some(chunk)) = resp.chunk().await {
                    if got.len() + chunk.len() > MAX_ANSWER {
                        break;
                    }
                    got.extend_from_slice(&chunk);
                }
                let findings = judge(&c, op, status, ct.as_deref(), &got);
                let failed = findings.iter().any(|f| f["warning"] != true);
                row["status"] = json!(status);
                row["ms"] = json!(t.elapsed().as_millis() as u64);
                row["ok"] = json!(!failed);
                row["findings"] = json!(findings);
            }
            Err(e) => {
                let what = if e.is_timeout() {
                    "timed out"
                } else {
                    "could not reach the API"
                };
                row["ok"] = json!(false);
                row["findings"] = json!([{ "kind": "unreachable", "detail": what }]);
            }
        }
        results.push(row);
    }
    let failed = results.iter().filter(|r| r["ok"] == false).count();
    let warnings: usize = results
        .iter()
        .map(|r| {
            r["findings"]
                .as_array()
                .map_or(0, |f| f.iter().filter(|x| x["warning"] == true).count())
        })
        .sum();
    let run = json!({
        "mock": name,
        "upstream": upstream,
        "checked_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "passed": failed == 0 && skipped == 0,
        "operations": results.len(),
        "drifted": failed,
        "warnings": warnings,
        "skipped": skipped,
        "ms": started.elapsed().as_millis() as u64,
        "results": results,
    });
    state.drift.keep(name, run.clone());
    tracing::info!(mock = name, drifted = failed, "drift check");
    Ok(run)
}

/// A mock's kept runs (`mocks.read`).
pub fn runs(svc: &Svc, name: &str) -> SvcResult<Value> {
    svc.readable(name)?;
    Ok(json!({ "mock": name, "runs": svc.state.drift.list(name), "max_runs": RUNS_PER_MOCK }))
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// A run as JUnit: a test case per operation, a failure per finding that
/// is not a warning.
pub fn junit(run: &Value) -> String {
    let results = run["results"].as_array().cloned().unwrap_or_default();
    let mut cases = String::new();
    let mut failures = 0;
    for r in &results {
        let name = format!(
            "{} {} ({})",
            r["method"].as_str().unwrap_or(""),
            r["path"].as_str().unwrap_or(""),
            r["operation"].as_str().unwrap_or("")
        );
        let bad: Vec<String> = r["findings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|f| f["warning"] != true)
            .map(|f| {
                format!(
                    "{}: {}",
                    f["kind"].as_str().unwrap_or(""),
                    f["detail"].as_str().unwrap_or("")
                )
            })
            .collect();
        if bad.is_empty() {
            cases.push_str(&format!(
                "    <testcase classname=\"drift\" name=\"{}\"/>\n",
                xml(&name)
            ));
        } else {
            failures += 1;
            cases.push_str(&format!(
                "    <testcase classname=\"drift\" name=\"{}\">\n      <failure message=\"{}\">{}</failure>\n    </testcase>\n",
                xml(&name),
                xml(bad.first().map(String::as_str).unwrap_or("")),
                xml(&bad.join("\n"))
            ));
        }
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n  <testsuite name=\"drift {}\" tests=\"{}\" failures=\"{failures}\" errors=\"0\" timestamp=\"{}\">\n{cases}  </testsuite>\n</testsuites>\n",
        xml(run["mock"].as_str().unwrap_or("")),
        results.len(),
        xml(run["checked_at"].as_str().unwrap_or(""))
    )
}

// ── Routes ──────────────────────────────────────────────────────────

pub const PATH: &str = "/_rustybin/mocks/{name}/drift";

#[derive(Deserialize)]
pub struct NamePath {
    name: String,
}

#[derive(Deserialize, Default)]
pub struct FormatQuery {
    format: Option<String>,
}

fn wants_junit(q: &FormatQuery, headers: &axum::http::HeaderMap) -> bool {
    q.format.as_deref() == Some("junit")
        || headers
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|a| a.contains("xml"))
}

/// `POST /_rustybin/mocks/{name}/drift`: run a check (JSON, or JUnit).
pub async fn run_handler(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    axum::extract::Path(NamePath { name }): axum::extract::Path<NamePath>,
    axum::extract::Query(q): axum::extract::Query<FormatQuery>,
    headers: axum::http::HeaderMap,
    extensions: axum::http::Extensions,
    uri: axum::http::Uri,
    body: bytes::Bytes,
) -> Result<axum::response::Response, ServiceError> {
    use axum::response::IntoResponse;
    let req: DriftBody = if body.is_empty() {
        DriftBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| ServiceError::bad_request(format!("invalid body: {e}")))?
    };
    let svc = Svc::from_request(&state, &headers, &extensions, &uri);
    let run = run(svc, &name, req).await?;
    if wants_junit(&q, &headers) {
        return Ok((
            [("content-type", "application/xml; charset=utf-8")],
            junit(&run),
        )
            .into_response());
    }
    Ok(axum::Json(run).into_response())
}

/// `GET /_rustybin/mocks/{name}/drift`: the kept runs.
pub async fn runs_handler(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    axum::extract::Path(NamePath { name }): axum::extract::Path<NamePath>,
    headers: axum::http::HeaderMap,
    extensions: axum::http::Extensions,
    uri: axum::http::Uri,
) -> Result<axum::response::Response, ServiceError> {
    use axum::response::IntoResponse;
    let svc = Svc::from_request(&state, &headers, &extensions, &uri);
    Ok(axum::Json(runs(&svc, &name)?).into_response())
}

#[cfg(test)]
mod tests;
