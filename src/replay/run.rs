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

use super::{Assertion, SavedTest};
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
}

#[derive(Clone, Debug, Serialize)]
pub struct Failure {
    pub assertion: String,
    pub expected: String,
    pub seen: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct TestResult {
    pub name: String,
    /// `direct` or the gateway's origin.
    pub target: String,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<Failure>,
    /// The request could not be made (a missing variable, a refusal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
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

fn prepare(t: &SavedTest, lookup: &dyn Fn(&str) -> Option<String>) -> Result<Prepared, String> {
    let missing = |n: String| {
        format!("needs the variable {n} (give it in the run's variables, or set {ENV_PREFIX}{n} on the instance)")
    };
    let r = &t.request;
    let path = fill(&r.path, lookup).map_err(missing)?;
    let mut headers = Vec::new();
    for (k, v) in &r.headers {
        let v = fill(v, lookup).map_err(missing)?;
        let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(&v),
        ) else {
            return Err(format!("header {k}: not a valid header once filled"));
        };
        headers.push((n, v));
    }
    let body = r
        .body
        .as_deref()
        .map(|b| fill(b, lookup))
        .transpose()
        .map_err(missing)?;
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
    req.extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 0))));
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
fn scrub(result: &mut TestResult, filled: &[(String, String)]) {
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
    let vars = body.variables;
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
    let mut clients: HashMap<String, Result<reqwest::Client, String>> = HashMap::new();
    let mut results = Vec::new();
    for (i, t) in tests.iter().enumerate() {
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
        };
        if i >= MAX_RUN_REQUESTS || started.elapsed() > RUN_DEADLINE {
            result.error = Some(format!(
                "not run: a run sends at most {MAX_RUN_REQUESTS} requests in {} s",
                RUN_DEADLINE.as_secs()
            ));
            results.push(result);
            continue;
        }
        let answer = match prepare(t, &lookup) {
            Err(e) => Err(e),
            Ok(p) => match &t.gateway {
                None => direct(state, p, session).await,
                Some(g) => {
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
            },
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
        // What the run saw may echo a filled variable (a request to /echo):
        // a report never carries a variable's value, only its name.
        let filled: Vec<(String, String)> = t
            .variables()
            .into_iter()
            .filter_map(|n| lookup(&n).map(|v| (n, v)))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        scrub(&mut result, &filled);
        results.push(result);
    }
    let failed = results
        .iter()
        .filter(|r| !r.passed && r.error.is_none())
        .count();
    let errors = results.iter().filter(|r| r.error.is_some()).count();
    Ok(Report {
        passed: failed == 0 && errors == 0,
        total: results.len(),
        failed,
        errors,
        duration_ms: started.elapsed().as_millis() as u64,
        ran_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        results,
    })
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
