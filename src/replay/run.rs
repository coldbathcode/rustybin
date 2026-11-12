//! Running saved tests: variables filled, the request sent in process (this
//! instance, the caller's session) or through the test's gateway (egress
//! purpose `replay`), every assertion judged. Bounded: at most
//! [`MAX_RUN_REQUESTS`] requests and [`RUN_DEADLINE`] a run.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderName, HeaderValue, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower::ServiceExt;

use super::{Assertion, SavedTest, TestRequest};
use crate::egress::Purpose;
use crate::state::AppState;

/// Most requests of one run.
pub const MAX_RUN_REQUESTS: usize = 200;
/// The longest a run takes.
pub const RUN_DEADLINE: Duration = Duration::from_secs(60);
/// The longest one request waits.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// The most of a body read for the assertions.
const MAX_BODY: usize = 1024 * 1024;
/// The prefix of the environment variables a run may read.
pub const ENV_PREFIX: &str = "RUSTYBIN_TEST_";
/// The most of an observed value a result repeats.
const MAX_SEEN: usize = 200;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunBody {
    /// The tests to run (default: every one).
    #[serde(default)]
    pub names: Vec<String>,
    /// `{{name}}` values (else `RUSTYBIN_TEST_*` environment variables).
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    /// Only this project's tests (`instance`: the instance's); a member of
    /// projects runs the tests it may run.
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Failure {
    pub assertion: String,
    pub expected: String,
    pub seen: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestResult {
    pub name: String,
    /// `direct` or the gateway's origin.
    pub target: String,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<Failure>,
    /// The request could not be made (a missing variable, a refusal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A multi-step test: each step's result, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepResult>,
}

/// One step of a multi-step test's result.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StepResult {
    pub id: String,
    pub passed: bool,
    /// Not run: an earlier step failed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<Failure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What the step took from its answer, cut short; one whose name looks
    /// like a credential is shown as `(hidden)`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub passed: bool,
    pub total: usize,
    pub failed: usize,
    pub errors: usize,
    pub duration_ms: u64,
    pub ran_at: String,
    pub results: Vec<TestResult>,
}

// ── Variables ───────────────────────────────────────────────────────

fn valid_var(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 64
}

/// The `{{name}}` references in a text.
pub fn variables_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let name = after[..end].trim();
        if valid_var(name) && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
        rest = &after[end + 2..];
    }
    out
}

/// Fill `{{name}}` references; the first missing name is the error.
pub fn fill(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
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
        let name = after[..end].trim();
        if valid_var(name) {
            out.push_str(&lookup(name).ok_or_else(|| name.to_string())?);
        } else {
            out.push_str(&rest[start..start + 2 + end + 2]);
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// One `{{steps.<id>.outputs.<name>}}` reference: the step and the output.
fn step_ref(inner: &str) -> Option<(&str, &str)> {
    let rest = inner.trim().strip_prefix("steps.")?;
    let (id, name) = rest.split_once(".outputs.")?;
    let word = |w: &str| {
        !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    };
    (word(id) && word(name)).then_some((id, name))
}

/// The `{{steps.<id>.outputs.<name>}}` references in a text.
pub fn step_refs(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        if let Some((id, name)) = step_ref(&after[..end]) {
            out.push((id.to_string(), name.to_string()));
        }
        rest = &after[end + 2..];
    }
    out
}

/// The outputs of the steps run so far, by step then name.
type Outputs = BTreeMap<String, BTreeMap<String, String>>;

/// Fill `{{steps.<id>.outputs.<name>}}` references from the steps run so
/// far; the first one that has no value is the error. Done after the
/// variables, so a value an answer gave is never read as a variable.
fn fill_outputs(text: &str, outputs: &Outputs) -> Result<String, String> {
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
        match step_ref(&after[..end]) {
            Some((id, name)) => {
                let v = outputs.get(id).and_then(|o| o.get(name)).ok_or_else(|| {
                    format!("the step {id} gave no output {name} (its answer did not have it)")
                })?;
                out.push_str(v);
            }
            None => out.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The value of an output expression in an answer, as text.
fn output_of(expr: &str, ans: &Answer) -> Option<String> {
    let expr = expr.trim();
    if expr == "$statusCode" {
        return Some(ans.status.to_string());
    }
    if let Some(h) = expr.strip_prefix("$response.header.") {
        return ans
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(h))
            .map(|(_, v)| v.clone());
    }
    let pointer = expr.strip_prefix("$response.body#")?;
    let doc: Value = serde_json::from_slice(&ans.body).ok()?;
    match doc.pointer(pointer)? {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

// ── JSON paths ──────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
pub enum Step {
    Key(String),
    Index(usize),
}

/// `$`, `$.a.b`, `$.items[0].id`, `$['a b']`.
pub fn parse_path(path: &str) -> Result<Vec<Step>, String> {
    let bad = || format!("json {path:?}: a path such as $.items[0].id");
    let mut rest = path.strip_prefix('$').ok_or_else(bad)?;
    let mut steps = Vec::new();
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('.') {
            let end = r.find(['.', '[']).unwrap_or(r.len());
            if end == 0 {
                return Err(bad());
            }
            steps.push(Step::Key(r[..end].to_string()));
            rest = &r[end..];
        } else if let Some(r) = rest.strip_prefix("['") {
            let end = r.find("']").ok_or_else(bad)?;
            steps.push(Step::Key(r[..end].to_string()));
            rest = &r[end + 2..];
        } else if let Some(r) = rest.strip_prefix('[') {
            let end = r.find(']').ok_or_else(bad)?;
            steps.push(Step::Index(r[..end].parse().map_err(|_| bad())?));
            rest = &r[end + 1..];
        } else {
            return Err(bad());
        }
        if steps.len() > 32 {
            return Err(bad());
        }
    }
    Ok(steps)
}

pub fn lookup<'a>(v: &'a Value, steps: &[Step]) -> Option<&'a Value> {
    steps.iter().try_fold(v, |v, s| match s {
        Step::Key(k) => v.get(k),
        Step::Index(i) => v.get(*i),
    })
}

// ── Sending ─────────────────────────────────────────────────────────

struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    ms: u64,
}

struct Prepared {
    method: Method,
    path: String,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Option<String>,
}

fn prepare(
    r: &TestRequest,
    lookup: &dyn Fn(&str) -> Option<String>,
    outputs: &Outputs,
) -> Result<Prepared, String> {
    let missing = |n: String| {
        format!("needs the variable {n} (give it in the run's variables, or set {ENV_PREFIX}{n} on the instance)")
    };
    // The variables, then the earlier steps' outputs.
    let fill = |text: &str| -> Result<String, String> {
        fill_outputs(&fill(text, lookup).map_err(missing)?, outputs)
    };
    let path = fill(&r.path)?;
    // A value from an answer never adds a line to the request.
    if path.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the path has a space or a control character once filled".into());
    }
    let mut headers = Vec::new();
    for (k, v) in &r.headers {
        let v = fill(v)?;
        let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(&v),
        ) else {
            return Err(format!("header {k}: not a valid header once filled"));
        };
        headers.push((n, v));
    }
    let body = r.body.as_deref().map(fill).transpose()?;
    let method =
        Method::from_bytes(r.method.as_bytes()).map_err(|_| "invalid method".to_string())?;
    Ok(Prepared {
        method,
        path,
        headers,
        body,
    })
}

/// Through this instance's application, in process, with the caller's session.
async fn direct(state: &AppState, p: Prepared, session: &str) -> Result<Answer, String> {
    let Some(router) = state.router.get().cloned() else {
        return Err("in-process requests are not available on this instance".into());
    };
    let path_only = p.path.split('?').next().unwrap_or("");
    if crate::control::is_control_path(path_only) {
        return Err("control-plane paths are not tested".into());
    }
    let mut b = Request::builder().method(p.method).uri(&p.path);
    let mut has_session = false;
    for (n, v) in p.headers {
        if matches!(
            n.as_str(),
            "content-length" | "transfer-encoding" | "connection" | "host" | "upgrade"
        ) {
            continue;
        }
        has_session |= n.as_str() == "x-rustybin-session";
        b = b.header(n, v);
    }
    if !has_session && crate::session::is_valid_session(session) {
        b = b.header("x-rustybin-session", session);
    }
    let mut req = b
        .body(p.body.map(Body::from).unwrap_or_else(Body::empty))
        .map_err(|e| format!("invalid request: {e}"))?;
    // A peer of the caller's own (its session), not one every run shares.
    req.extensions_mut()
        .insert(ConnectInfo(crate::session::in_process_peer(None, session)));
    // Its credentials were filled from the run's variables: the capture
    // redacts every secret-looking header and query value, as it does for
    // the MCP inspector's own requests.
    req.extensions_mut()
        .insert(crate::mcp::inspector::client::InspectorOrigin);
    let started = Instant::now();
    let resp = match tokio::time::timeout(REQUEST_TIMEOUT, router.oneshot(req)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return Err(format!("the request failed: {e}")),
        Err(_) => return Err(format!("no answer within {} s", REQUEST_TIMEOUT.as_secs())),
    };
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    let mut body = resp.into_body();
    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
    loop {
        let next = tokio::select! {
            f = body.frame() => f,
            _ = tokio::time::sleep_until(deadline) => break,
        };
        match next {
            Some(Ok(frame)) => {
                if let Some(d) = frame.data_ref() {
                    let room = MAX_BODY.saturating_sub(collected.len());
                    collected.extend_from_slice(&d[..d.len().min(room)]);
                    if collected.len() >= MAX_BODY {
                        break;
                    }
                }
            }
            _ => break,
        }
    }
    Ok(Answer {
        status,
        headers,
        body: collected,
        ms: started.elapsed().as_millis() as u64,
    })
}

async fn through(client: &reqwest::Client, url: &str, p: Prepared) -> Result<Answer, String> {
    let method =
        reqwest::Method::from_bytes(p.method.as_str().as_bytes()).map_err(|e| e.to_string())?;
    let mut rb = client.request(method, url);
    for (n, v) in p.headers {
        rb = rb.header(n.as_str(), v.as_bytes());
    }
    if let Some(b) = p.body {
        rb = rb.body(b);
    }
    let started = Instant::now();
    let mut resp = rb.send().await.map_err(|e| {
        if e.is_timeout() {
            "timed out".to_string()
        } else if e.is_connect() {
            "could not connect to the gateway".to_string()
        } else {
            "the request failed".to_string()
        }
    })?;
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    let mut collected = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        let room = MAX_BODY.saturating_sub(collected.len());
        collected.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if collected.len() >= MAX_BODY {
            break;
        }
    }
    Ok(Answer {
        status,
        headers,
        body: collected,
        ms: started.elapsed().as_millis() as u64,
    })
}

/// The gateway clients of a run, by origin (resolved once each).
type Clients = HashMap<String, Result<reqwest::Client, String>>;

/// Send a prepared request: in process, or through the test's gateway
/// under the egress policy (purpose replay).
async fn send(
    state: &AppState,
    session: &str,
    gateway: Option<&str>,
    p: Prepared,
    clients: &mut Clients,
) -> Result<Answer, String> {
    let Some(g) = gateway else {
        return direct(state, p, session).await;
    };
    let url = format!("{}{}", g.trim_end_matches('/'), p.path);
    let origin = crate::egress::url_origin(&url);
    if !clients.contains_key(&origin) {
        let c = match state.egress.resolve(&url, Purpose::Replay).await {
            Ok(pinned) => state.egress.client(&pinned, REQUEST_TIMEOUT),
            Err(e) => Err(e.0),
        };
        clients.insert(origin.clone(), c);
    }
    match clients.get(&origin) {
        Some(Ok(c)) => through(c, &url, p).await,
        Some(Err(e)) => Err(format!("refused by the egress policy: {e}")),
        None => Err("no client".into()),
    }
}

// ── Judging ─────────────────────────────────────────────────────────

fn cut(s: &str) -> String {
    if s.len() <= MAX_SEEN {
        return s.to_string();
    }
    let mut end = MAX_SEEN;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

fn judge(a: &Assertion, ans: &Answer) -> Option<Failure> {
    let fail = |expected: String, seen: String| {
        Some(Failure {
            assertion: a.label(),
            expected,
            seen: cut(&seen),
        })
    };
    if let Some(s) = a.status {
        return (ans.status != s)
            .then(|| fail(s.to_string(), ans.status.to_string()))
            .flatten();
    }
    if let Some(h) = &a.header {
        let got = ans
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(h))
            .map(|(_, v)| v.as_str());
        return match (&a.equals, got) {
            (None, Some(_)) => None,
            (None, None) => fail("present".into(), "absent".into()),
            (Some(want), got) => {
                let want = want.as_str().unwrap_or_default();
                if got == Some(want) {
                    None
                } else {
                    fail(want.to_string(), got.unwrap_or("absent").to_string())
                }
            }
        };
    }
    if let Some(b) = &a.body_contains {
        let text = String::from_utf8_lossy(&ans.body);
        return (!text.contains(b.as_str()))
            .then(|| fail(format!("a body containing {b:?}"), text.into_owned()))
            .flatten();
    }
    if let Some(p) = &a.json {
        let Ok(steps) = parse_path(p) else {
            return fail("a valid path".into(), p.clone());
        };
        let Ok(doc) = serde_json::from_slice::<Value>(&ans.body) else {
            return fail(
                "a JSON body".into(),
                String::from_utf8_lossy(&ans.body).into_owned(),
            );
        };
        let got = lookup(&doc, &steps);
        return match (&a.equals, got) {
            (None, Some(_)) => None,
            (None, None) => fail("present".into(), "absent".into()),
            (Some(want), got) => {
                if got == Some(want) {
                    None
                } else {
                    fail(
                        want.to_string(),
                        got.map(Value::to_string).unwrap_or_else(|| "absent".into()),
                    )
                }
            }
        };
    }
    if let Some(max) = a.max_ms {
        return (ans.ms > max)
            .then(|| fail(format!("at most {max} ms"), format!("{} ms", ans.ms)))
            .flatten();
    }
    None
}

/// Replace every filled variable value in a result with `{{NAME}}`.
pub(crate) fn scrub(result: &mut TestResult, filled: &[(String, String)]) {
    if filled.is_empty() {
        return;
    }
    let clean = |text: &mut String| {
        for (name, value) in filled {
            if text.contains(value.as_str()) {
                *text = text.replace(value.as_str(), &format!("{{{{{name}}}}}"));
            }
        }
    };
    for f in &mut result.failures {
        clean(&mut f.seen);
        clean(&mut f.expected);
    }
    if let Some(e) = &mut result.error {
        clean(e);
    }
    for step in &mut result.steps {
        for f in &mut step.failures {
            clean(&mut f.seen);
            clean(&mut f.expected);
        }
        if let Some(e) = &mut step.error {
            clean(e);
        }
        for v in step.outputs.values_mut() {
            clean(v);
        }
    }
}

// ── A run ───────────────────────────────────────────────────────────

/// Run the tests `body` names (every one by default).
pub async fn run(
    state: &AppState,
    session: &str,
    body: RunBody,
) -> Result<Report, (StatusCode, String)> {
    let all = state.saved_tests.list();
    let tests: Vec<SavedTest> = if body.names.is_empty() {
        all
    } else {
        let mut out = Vec::new();
        for n in &body.names {
            match all.iter().find(|t| &t.name == n) {
                Some(t) => out.push(t.clone()),
                None => return Err((StatusCode::NOT_FOUND, format!("no test named {n:?}"))),
            }
        }
        out
    };
    Ok(run_these(state, session, &tests, body.variables).await)
}

/// Run these tests in order (a saved test, or one a storyboard's `send`
/// step wrote out): the report, with no variable's value in it.
pub async fn run_these(
    state: &AppState,
    session: &str,
    tests: &[SavedTest],
    vars: BTreeMap<String, String>,
) -> Report {
    let lookup = move |n: &str| -> Option<String> {
        vars.get(n).cloned().or_else(|| {
            let env = if n.starts_with(ENV_PREFIX) {
                n.to_string()
            } else {
                format!("{ENV_PREFIX}{n}")
            };
            std::env::var(env).ok()
        })
    };
    let started = Instant::now();
    let mut clients: Clients = HashMap::new();
    let mut results = Vec::new();
    // Requests sent in this run: a multi-step test sends one a step.
    let mut sent = 0usize;
    for t in tests {
        let target = t
            .gateway
            .as_deref()
            .map(crate::egress::url_origin)
            .unwrap_or_else(|| "direct".into());
        let mut result = TestResult {
            name: t.name.clone(),
            target,
            passed: false,
            status: None,
            ms: None,
            failures: Vec::new(),
            error: None,
            steps: Vec::new(),
        };
        let over = |sent: usize| sent >= MAX_RUN_REQUESTS || started.elapsed() > RUN_DEADLINE;
        let not_run = || {
            format!(
                "not run: a run sends at most {MAX_RUN_REQUESTS} requests in {} s",
                RUN_DEADLINE.as_secs()
            )
        };
        // What the run saw may echo a filled variable (a request to /echo):
        // a report never carries a variable's value, only its name.
        let filled: Vec<(String, String)> = t
            .variables()
            .into_iter()
            .filter_map(|n| lookup(&n).map(|v| (n, v)))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        if t.steps.is_empty() {
            if over(sent) {
                result.error = Some(not_run());
                results.push(result);
                continue;
            }
            sent += 1;
            let answer = match prepare(&t.request, &lookup, &Outputs::new()) {
                Err(e) => Err(e),
                Ok(p) => send(state, session, t.gateway.as_deref(), p, &mut clients).await,
            };
            match answer {
                Err(e) => result.error = Some(e),
                Ok(ans) => {
                    result.status = Some(ans.status);
                    result.ms = Some(ans.ms);
                    result.failures = t.assert.iter().filter_map(|a| judge(a, &ans)).collect();
                    result.passed = result.failures.is_empty();
                }
            }
        } else {
            // In order; a step that fails stops the test, and the steps
            // after it are reported as skipped.
            let mut outputs = Outputs::new();
            let mut stopped = false;
            let mut total_ms = 0u64;
            for step in &t.steps {
                let mut sr = StepResult {
                    id: step.id.clone(),
                    ..StepResult::default()
                };
                if stopped {
                    sr.skipped = true;
                    result.steps.push(sr);
                    continue;
                }
                if over(sent) {
                    sr.error = Some(not_run());
                } else {
                    sent += 1;
                    let answer = match prepare(&step.request, &lookup, &outputs) {
                        Err(e) => Err(e),
                        Ok(p) => send(state, session, t.gateway.as_deref(), p, &mut clients).await,
                    };
                    match answer {
                        Err(e) => sr.error = Some(e),
                        Ok(ans) => {
                            sr.status = Some(ans.status);
                            sr.ms = Some(ans.ms);
                            total_ms += ans.ms;
                            sr.failures =
                                step.assert.iter().filter_map(|a| judge(a, &ans)).collect();
                            sr.passed = sr.failures.is_empty();
                            let mut got = BTreeMap::new();
                            for (name, expr) in &step.outputs {
                                if let Some(v) = output_of(expr, &ans) {
                                    // What is kept never holds a credential,
                                    // nor a variable's value an answer echoed.
                                    let mut shown = if crate::keys::looks_secret(name) {
                                        "(hidden)".to_string()
                                    } else {
                                        cut(&v)
                                    };
                                    for (n, value) in &filled {
                                        if shown.contains(value.as_str()) {
                                            shown = shown
                                                .replace(value.as_str(), &format!("{{{{{n}}}}}"));
                                        }
                                    }
                                    sr.outputs.insert(name.clone(), shown);
                                    got.insert(name.clone(), v);
                                }
                            }
                            outputs.insert(step.id.clone(), got);
                            result.status = Some(ans.status);
                        }
                    }
                }
                if !sr.passed {
                    stopped = true;
                    // The test's own lines name the step, so every reader
                    // of a result (the CLI, JUnit, the history) does too.
                    match &sr.error {
                        Some(e) => result.error = Some(format!("step {}: {e}", step.id)),
                        None => result.failures.extend(sr.failures.iter().map(|f| Failure {
                            assertion: format!("step {}: {}", step.id, f.assertion),
                            expected: f.expected.clone(),
                            seen: f.seen.clone(),
                        })),
                    }
                }
                result.steps.push(sr);
            }
            result.ms = Some(total_ms);
            result.passed = !stopped;
        }
        scrub(&mut result, &filled);
        results.push(result);
    }
    let failed = results
        .iter()
        .filter(|r| !r.passed && r.error.is_none())
        .count();
    let errors = results.iter().filter(|r| r.error.is_some()).count();
    Report {
        passed: failed == 0 && errors == 0,
        total: results.len(),
        failed,
        errors,
        duration_ms: started.elapsed().as_millis() as u64,
        ran_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        results,
    }
}

/// The report as JUnit XML (one suite, a case per test).
pub fn junit(r: &Report) -> String {
    use crate::content_negotiation::xml_escape as x;
    let mut s = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuite name=\"rustybin\" tests=\"{}\" failures=\"{}\" errors=\"{}\" time=\"{:.3}\" timestamp=\"{}\">\n",
        r.total,
        r.failed,
        r.errors,
        r.duration_ms as f64 / 1000.0,
        x(&r.ran_at)
    );
    for t in &r.results {
        s.push_str(&format!(
            "  <testcase classname=\"rustybin.tests\" name=\"{}\" time=\"{:.3}\">",
            x(&t.name),
            t.ms.unwrap_or(0) as f64 / 1000.0
        ));
        if let Some(e) = &t.error {
            s.push_str(&format!("<error message=\"{}\"/>", x(e)));
        } else if !t.failures.is_empty() {
            let detail: Vec<String> = t
                .failures
                .iter()
                .map(|f| format!("{}: expected {}, saw {}", f.assertion, f.expected, f.seen))
                .collect();
            s.push_str(&format!(
                "<failure message=\"{}\">{}</failure>",
                x(&detail[0]),
                x(&detail.join("\n"))
            ));
        }
        s.push_str("</testcase>\n");
    }
    s.push_str("</testsuite>\n");
    s
}
