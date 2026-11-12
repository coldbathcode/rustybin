//! Saved tests: a captured request (or one written by hand) with assertions
//! (status, a header, the body, a JSON value, the time), replayed against
//! this instance in process or through a gateway, and run in CI (JUnit, or
//! exported as Hurl). Kept in `<state>/tests.json` and in `rustybin.yaml`
//! (`tests:`). Credentials are never saved: a captured secret-looking header
//! or query value becomes a `{{name}}` variable the run fills (from the run's
//! `variables`, else a `RUSTYBIN_TEST_*` environment variable). Every run
//! is kept in the history ([`history`], `/_rustybin/tests/runs*`), and
//! tests run on a schedule ([`schedule`], `/_rustybin/tests/schedules*`).

pub mod history;
pub mod hurl;
pub mod run;
pub mod schedule;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::session::Session;
use crate::state::AppState;

/// Most saved tests.
pub const MAX_TESTS: usize = 200;
/// The largest saved test (as JSON).
pub const MAX_TEST_BYTES: usize = 64 * 1024;
/// Most headers, assertions.
const MAX_HEADERS: usize = 50;
const MAX_ASSERTIONS: usize = 32;
const MAX_NAME: usize = 64;
const FILE: &str = "tests.json";
pub const METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// One saved test, as in `rustybin.yaml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedTest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub request: TestRequest,
    /// The gateway base URL the request goes through (the request's path is
    /// appended); absent: this instance, in process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assert: Vec<Assertion>,
    /// The project the test belongs to (step 11); absent: the instance's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestRequest {
    #[serde(default = "default_method")]
    pub method: String,
    /// Path and query on the target (`/mock/petstore/pets/1?x=1`).
    pub path: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

fn default_method() -> String {
    "GET".into()
}

/// One assertion: exactly one of `status`, `header` (with `equals`, else it
/// must exist), `body_contains`, `json` (a path such as `$.items[0].id`, with
/// `equals`, else it must exist) and `max_ms`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assertion {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_contains: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ms: Option<u64>,
}

impl Assertion {
    pub fn status(code: u16) -> Self {
        Self {
            status: Some(code),
            ..Self::default()
        }
    }

    /// What it checks, in a few words (`status 200`, `header content-type`).
    pub fn label(&self) -> String {
        if let Some(s) = self.status {
            format!("status {s}")
        } else if let Some(h) = &self.header {
            format!("header {h}")
        } else if let Some(b) = &self.body_contains {
            format!("body contains {b:?}")
        } else if let Some(p) = &self.json {
            format!("json {p}")
        } else if let Some(ms) = self.max_ms {
            format!("max_ms {ms}")
        } else {
            "nothing".into()
        }
    }

    fn validate(&self) -> Result<(), String> {
        let kinds = [
            self.status.is_some(),
            self.header.is_some(),
            self.body_contains.is_some(),
            self.json.is_some(),
            self.max_ms.is_some(),
        ]
        .iter()
        .filter(|k| **k)
        .count();
        if kinds != 1 {
            return Err("one of status, header, body_contains, json, max_ms".into());
        }
        if self.equals.is_some() && self.header.is_none() && self.json.is_none() {
            return Err("equals goes with header or json".into());
        }
        if let Some(s) = self.status {
            if !(100..=599).contains(&s) {
                return Err(format!("status {s}: 100 to 599"));
            }
        }
        if let Some(h) = &self.header {
            HeaderName::from_bytes(h.as_bytes())
                .map_err(|_| format!("header {h:?}: not a header name"))?;
            if matches!(&self.equals, Some(v) if !v.is_string()) {
                return Err(format!("header {h}: equals takes a string"));
            }
        }
        if let Some(p) = &self.json {
            run::parse_path(p)?;
        }
        Ok(())
    }
}

/// A name a test may have (`run`, `runs`, `schedules` and `export` are
/// routes).
pub fn valid_name(name: &str) -> bool {
    !matches!(name, "run" | "runs" | "schedules" | "export")
        && !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl SavedTest {
    /// Every rule a saved test follows (a document's tests too).
    pub fn validate(&self) -> Result<(), String> {
        if !valid_name(&self.name) {
            return Err(format!(
                "name {:?}: 1 to {MAX_NAME} letters, digits, . _ - (not run, runs, schedules or export)",
                self.name
            ));
        }
        if let Some(p) = &self.project {
            crate::rbac::projects::check_name(p)?;
        }
        let r = &self.request;
        if !METHODS.contains(&r.method.as_str()) {
            return Err(format!(
                "request.method {:?}: one of {}",
                r.method,
                METHODS.join(", ")
            ));
        }
        if !r.path.starts_with('/') || r.path.starts_with("//") {
            return Err("request.path: a path starting with / (never a URL)".into());
        }
        // The path and gateway go into request lines and the Hurl export:
        // no whitespace or control characters.
        let unsafe_char = |c: char| c.is_whitespace() || c.is_control();
        if r.path.chars().any(unsafe_char) {
            return Err(
                "request.path: no spaces or control characters (percent-encode them)".into(),
            );
        }
        if self
            .gateway
            .as_deref()
            .is_some_and(|g| g.chars().any(unsafe_char))
        {
            return Err("gateway: no spaces or control characters".into());
        }
        // Credentials are never saved: a secret-looking header or query
        // value must reference a variable (`Bearer {{TOKEN}}`), never be one.
        for (k, v) in &r.headers {
            if crate::keys::looks_secret(k) && run::variables_in(v).is_empty() {
                return Err(format!(
                    "request.headers.{k}: a credential is never saved: use a variable, such as {{{{{}}}}}",
                    variable_name(k)
                ));
            }
        }
        if let Some((_, query)) = r.path.split_once('?') {
            for (k, v) in form_urlencoded::parse(query.as_bytes()) {
                if crate::keys::looks_secret(&k) && run::variables_in(&v).is_empty() {
                    return Err(format!(
                        "request.path: the query value of {k} is a credential, never saved: use a variable, such as {{{{{}}}}}",
                        variable_name(&k)
                    ));
                }
            }
        }
        let path_only = r.path.split('?').next().unwrap_or("");
        if crate::control::is_control_path(path_only) {
            return Err(
                "request.path: control-plane paths (/_rustybin/*, /ui/*) are not tested".into(),
            );
        }
        if r.headers.len() > MAX_HEADERS {
            return Err(format!("request.headers: at most {MAX_HEADERS}"));
        }
        for (k, v) in &r.headers {
            HeaderName::from_bytes(k.as_bytes())
                .map_err(|_| format!("request.headers: {k:?} is not a header name"))?;
            HeaderValue::from_str(v)
                .map_err(|_| format!("request.headers.{k}: not a header value"))?;
        }
        if let Some(g) = &self.gateway {
            crate::egress::parse_url(g, false).map_err(|e| format!("gateway: {e}"))?;
        }
        if self.assert.len() > MAX_ASSERTIONS {
            return Err(format!("assert: at most {MAX_ASSERTIONS}"));
        }
        for (i, a) in self.assert.iter().enumerate() {
            a.validate().map_err(|e| format!("assert[{i}]: {e}"))?;
        }
        let size = serde_json::to_vec(self)
            .map(|v| v.len())
            .unwrap_or(usize::MAX);
        if size > MAX_TEST_BYTES {
            return Err(format!(
                "the test is {size} bytes: at most {MAX_TEST_BYTES}"
            ));
        }
        Ok(())
    }

    /// The variables it uses (`{{name}}`), in order of appearance.
    pub fn variables(&self) -> Vec<String> {
        let mut out = Vec::new();
        let r = &self.request;
        let texts = std::iter::once(r.path.as_str())
            .chain(r.headers.values().map(String::as_str))
            .chain(r.body.as_deref());
        for t in texts {
            for v in run::variables_in(t) {
                if !out.contains(&v) {
                    out.push(v);
                }
            }
        }
        out
    }
}

// ── The store ───────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    saved_at: String,
    test: SavedTest,
}

#[derive(Serialize, Deserialize)]
struct Doc {
    version: u32,
    tests: Vec<Entry>,
}

/// The saved tests of an instance (`AppState.saved_tests`).
pub struct Tests {
    path: Option<PathBuf>,
    items: Mutex<BTreeMap<String, Entry>>,
    save_lock: Mutex<()>,
    /// A run is going: one at a time ([`Tests::begin_run`]).
    running: Arc<AtomicBool>,
    /// Every run, kept ([`history::History`]).
    pub history: history::History,
    /// The tests on a schedule ([`schedule::Schedules`]).
    pub schedules: schedule::Schedules,
}

/// Held while a run goes; the next one may start once it is dropped.
pub struct RunGuard(Arc<AtomicBool>);

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The refusal of a run while another goes.
pub const RUN_BUSY: &str = "a run of the saved tests is going: try again when it ends";

/// Why a store change was refused.
#[derive(Debug)]
pub enum StoreError {
    Invalid(String),
    Exists(String),
    Full,
    NotFound(String),
    /// The tests file could not be written: nothing changed.
    Io(String),
}

impl StoreError {
    pub fn status(&self) -> StatusCode {
        match self {
            StoreError::Invalid(_) => StatusCode::BAD_REQUEST,
            StoreError::Exists(_) => StatusCode::CONFLICT,
            StoreError::Full => StatusCode::INSUFFICIENT_STORAGE,
            StoreError::NotFound(_) => StatusCode::NOT_FOUND,
            StoreError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn message(&self) -> String {
        match self {
            StoreError::Invalid(m) => m.clone(),
            StoreError::Exists(n) => {
                format!("a test named {n:?} exists: PUT /_rustybin/tests/{n} replaces it")
            }
            StoreError::Full => format!("at most {MAX_TESTS} saved tests: delete one first"),
            StoreError::NotFound(n) => format!("no test named {n:?}"),
            StoreError::Io(e) => {
                format!("the tests file could not be written ({e}): nothing changed")
            }
        }
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Tests {
    /// The store, loaded from `<state>/tests.json` when a state directory is set.
    pub fn new(state_dir: Option<&std::path::Path>) -> Self {
        let path = state_dir.map(|d| d.join(FILE));
        let mut items = BTreeMap::new();
        if let Some(p) = &path {
            match std::fs::read(p) {
                Ok(bytes) => match serde_json::from_slice::<Doc>(&bytes) {
                    Ok(doc) => {
                        for e in doc.tests.into_iter().take(MAX_TESTS) {
                            match e.test.validate() {
                                Ok(()) => {
                                    items.insert(e.test.name.clone(), e);
                                }
                                Err(err) => tracing::warn!("{}: test skipped: {err}", p.display()),
                            }
                        }
                    }
                    Err(e) => tracing::warn!(
                        "{}: not a tests document ({e}): starting empty",
                        p.display()
                    ),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!("cannot read {}: {e}", p.display()),
            }
        }
        Self {
            path,
            items: Mutex::new(items),
            save_lock: Mutex::new(()),
            running: Arc::new(AtomicBool::new(false)),
            history: history::History::new(state_dir),
            schedules: schedule::Schedules::new(state_dir),
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Entry>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Make one change: under the save lock (one change at a time), `change`
    /// builds the next set of tests from the current one, the file is
    /// written (off the items lock: reads go on meanwhile), and only then
    /// does the store hold it. A failed write changes nothing.
    fn change<T>(
        &self,
        change: impl FnOnce(&mut BTreeMap<String, Entry>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let _order = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = self.lock().clone();
        let out = change(&mut next)?;
        if let Some(path) = &self.path {
            let doc = Doc {
                version: 1,
                tests: next.values().cloned().collect(),
            };
            let bytes =
                serde_json::to_vec_pretty(&doc).map_err(|e| StoreError::Io(e.to_string()))?;
            crate::mocks::store::off_runtime(|| crate::settings::write_private(path, &bytes))
                .map_err(StoreError::Io)?;
        }
        *self.lock() = next;
        Ok(out)
    }

    /// Start a run: `None` while another one goes (runs are single-flight,
    /// whichever surface starts them).
    pub fn begin_run(&self) -> Option<RunGuard> {
        self.running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| RunGuard(self.running.clone()))
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn list(&self) -> Vec<SavedTest> {
        self.lock().values().map(|e| e.test.clone()).collect()
    }

    pub fn get(&self, name: &str) -> Option<SavedTest> {
        self.lock().get(name).map(|e| e.test.clone())
    }

    fn view(e: &Entry) -> Value {
        let mut v = serde_json::to_value(&e.test).unwrap_or(Value::Null);
        if let Value::Object(m) = &mut v {
            m.insert("saved_at".into(), json!(e.saved_at));
            m.insert("variables".into(), json!(e.test.variables()));
        }
        v
    }

    /// The tests as the API lists them.
    pub fn views(&self) -> Vec<Value> {
        self.lock().values().map(Self::view).collect()
    }

    pub fn view_of(&self, name: &str) -> Option<Value> {
        self.lock().get(name).map(Self::view)
    }

    /// Save a test: `replace` lets it overwrite one of the same name.
    /// A file that cannot be written refuses the change
    /// ([`StoreError::Io`]): the store and the file never disagree.
    pub fn put(&self, test: SavedTest, replace: bool) -> Result<Value, StoreError> {
        test.validate().map_err(StoreError::Invalid)?;
        self.change(|items| {
            let exists = items.contains_key(&test.name);
            if exists && !replace {
                return Err(StoreError::Exists(test.name));
            }
            if !exists && items.len() >= MAX_TESTS {
                return Err(StoreError::Full);
            }
            let entry = Entry {
                saved_at: now(),
                test,
            };
            let view = Self::view(&entry);
            items.insert(entry.test.name.clone(), entry);
            Ok(view)
        })
    }

    /// Delete a test: `Ok(false)` when there is none of that name, an
    /// error (nothing deleted) when the file cannot be written.
    pub fn delete(&self, name: &str) -> Result<bool, StoreError> {
        if !self.lock().contains_key(name) {
            return Ok(false);
        }
        self.change(|items| Ok(items.remove(name).is_some()))
    }
}

// ── A test from a captured request ──────────────────────────────────

/// Request headers a saved test never keeps (the transport's, the
/// instance's own tags).
const DROPPED: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "transfer-encoding",
    "upgrade",
    "keep-alive",
    "te",
    "trailer",
    "proxy-connection",
    "x-rustybin-session",
    "x-rustybin-client",
    "x-request-id",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-forwarded-port",
    "forwarded",
    "via",
    "x-real-ip",
];

/// `X-Api-Key` -> `X_API_KEY`.
fn variable_name(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// The request of a capture, with every secret-looking header and query
/// value replaced by a variable (never filled back from the capture).
pub fn request_from_capture(
    c: &crate::inspector::CapturedRequest,
) -> Result<(TestRequest, Vec<String>), String> {
    if c.body_truncated {
        return Err(format!(
            "the captured body was cut at {} bytes: save a test with the request written out instead",
            c.body.as_deref().map(str::len).unwrap_or(0)
        ));
    }
    if c.body_base64.is_some() {
        return Err(
            "the captured body is binary: save a test with the request written out instead".into(),
        );
    }
    let mut vars = Vec::new();
    let mut path = c.path.clone();
    if let Some(q) = c.query.as_deref().filter(|q| !q.is_empty()) {
        // Names are decoded before the check (`to%6Ben` is `token`).
        let decoded = |k: &str| {
            form_urlencoded::parse(format!("{k}=").as_bytes())
                .next()
                .map(|(k, _)| k.into_owned())
                .unwrap_or_else(|| k.to_string())
        };
        let parts: Vec<String> = q
            .split('&')
            .map(|pair| match pair.split_once('=') {
                Some((k, _)) if crate::keys::looks_secret(&decoded(k)) => {
                    let v = variable_name(k);
                    if !vars.contains(&v) {
                        vars.push(v.clone());
                    }
                    format!("{k}={{{{{v}}}}}")
                }
                _ => pair.to_string(),
            })
            .collect();
        path = format!("{path}?{}", parts.join("&"));
    }
    let mut headers = BTreeMap::new();
    for h in &c.headers {
        let name = h.name.to_ascii_lowercase();
        if DROPPED.contains(&name.as_str()) {
            continue;
        }
        let value = if crate::keys::looks_secret(&name) {
            let v = variable_name(&name);
            if !vars.contains(&v) {
                vars.push(v.clone());
            }
            format!("{{{{{v}}}}}")
        } else {
            h.value.clone()
        };
        headers.insert(name, value);
    }
    Ok((
        TestRequest {
            method: c.method.clone(),
            path,
            headers,
            body: c
                .body
                .clone()
                .filter(|b| !b.is_empty())
                .map(|b| mask_body_secrets(&b, &mut vars)),
        },
        vars,
    ))
}

/// A captured body with the values of secret-looking JSON fields (at any
/// depth) and form fields replaced by variables.
fn mask_body_secrets(body: &str, vars: &mut Vec<String>) -> String {
    let mut note = |k: &str| {
        let v = variable_name(k);
        if !vars.contains(&v) {
            vars.push(v.clone());
        }
        format!("{{{{{v}}}}}")
    };
    if let Ok(mut doc) = serde_json::from_str::<Value>(body) {
        fn walk(v: &mut Value, note: &mut dyn FnMut(&str) -> String) {
            match v {
                Value::Object(m) => {
                    for (k, val) in m.iter_mut() {
                        if crate::keys::looks_secret(k) && (val.is_string() || val.is_number()) {
                            *val = Value::String(note(k));
                        } else {
                            walk(val, note);
                        }
                    }
                }
                Value::Array(a) => a.iter_mut().for_each(|x| walk(x, note)),
                _ => {}
            }
        }
        walk(&mut doc, &mut note);
        return serde_json::to_string(&doc).unwrap_or_else(|_| body.to_string());
    }
    // A form body: `a=1&password=x` keeps `a`, the password becomes a
    // variable (written unencoded, so it stays a `{{NAME}}` reference).
    if body.contains('=') && !body.contains(['\n', ' ']) {
        let pairs: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        if pairs.iter().any(|(k, _)| crate::keys::looks_secret(k)) {
            let parts: Vec<String> = pairs
                .iter()
                .map(|(k, v)| {
                    let key: String = form_urlencoded::byte_serialize(k.as_bytes()).collect();
                    if crate::keys::looks_secret(k) {
                        format!("{key}={}", note(k))
                    } else {
                        let val: String = form_urlencoded::byte_serialize(v.as_bytes()).collect();
                        format!("{key}={val}")
                    }
                })
                .collect();
            return parts.join("&");
        }
    }
    body.to_string()
}

/// May this caller turn the capture into a test? The inspector's own rule:
/// in shared mode a key's captures, in public mode the session's.
fn capture_visible(
    state: &AppState,
    c: &crate::inspector::CapturedRequest,
    session: &str,
    key_id: Option<&str>,
) -> bool {
    let inspector = &state.inspector;
    match key_id {
        Some(k) => c.key_id.as_deref() == Some(k),
        None if inspector.keyed() => true,
        None => !inspector.public_mode() || c.session.as_deref() == Some(session),
    }
}

/// `POST /_rustybin/tests`: a capture or a request, the assertions.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveBody {
    pub name: String,
    /// A captured request's inspector id.
    #[serde(default)]
    pub capture: Option<String>,
    #[serde(default)]
    pub request: Option<TestRequest>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub gateway: Option<String>,
    /// Default for a capture: its status.
    #[serde(default)]
    pub assert: Option<Vec<Assertion>>,
    /// The project the test belongs to (step 11).
    #[serde(default)]
    pub project: Option<String>,
}

/// Save a new test: `tests.write` in its project (the one named, else the
/// instance's, else the caller's one project).
pub fn save(
    state: &AppState,
    caller: &Resolved,
    body: SaveBody,
    session: &str,
    key_id: Option<&str>,
) -> Result<Value, (StatusCode, String)> {
    let (request, introduced, captured_status) = match (body.capture, body.request) {
        (Some(id), None) => {
            let c = state
                .inspector
                .get(&id)
                .filter(|c| capture_visible(state, c, session, key_id))
                .ok_or((
                    StatusCode::NOT_FOUND,
                    format!("no captured request {id:?} you may read"),
                ))?;
            let (r, vars) = request_from_capture(&c).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
            (r, vars, Some(c.status))
        }
        (None, Some(r)) => (r, Vec::new(), None),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "give either capture (an inspector id) or request".into(),
            ))
        }
    };
    let assert = body
        .assert
        .unwrap_or_else(|| captured_status.map(Assertion::status).into_iter().collect());
    let test = SavedTest {
        name: body.name,
        description: body.description,
        request,
        gateway: body.gateway,
        assert,
        project: body.project,
    };
    if state.saved_tests.get(&test.name).is_some() {
        return Err((
            StatusCode::CONFLICT,
            StoreError::Exists(test.name).message(),
        ));
    }
    let test = admit_for(state, caller, test).map_err(|e| (e.status, e.message()))?;
    let mut v = state
        .saved_tests
        .put(test, false)
        .map_err(|e| (e.status(), e.message()))?;
    if !introduced.is_empty() {
        v["introduced_variables"] = json!(introduced);
    }
    Ok(v)
}

// ── Projects (step 11) ──────────────────────────────────────────────

use crate::mocks::service::ServiceError;
use crate::rbac::{Permission as Perm, Resolved};

/// May the caller see a test (`tests.read` in its project, or
/// instance-wide)?
pub fn visible(caller: &Resolved, t: &SavedTest) -> bool {
    caller.has_in(Perm::TestsRead, t.project.as_deref())
}

fn no_test(name: &str) -> ServiceError {
    ServiceError::not_found(format!("no test named {name:?}"))
}

/// The tests the caller may see, optionally of one project.
pub fn tests_for(
    state: &AppState,
    caller: &Resolved,
    filter: &Option<crate::rbac::projects::Filter>,
) -> Vec<SavedTest> {
    state
        .saved_tests
        .list()
        .into_iter()
        .filter(|t| {
            visible(caller, t) && crate::rbac::projects::passes(filter, t.project.as_deref())
        })
        .collect()
}

/// The views of [`tests_for`].
pub fn views_for(
    state: &AppState,
    caller: &Resolved,
    filter: &Option<crate::rbac::projects::Filter>,
) -> Vec<Value> {
    tests_for(state, caller, filter)
        .iter()
        .filter_map(|t| state.saved_tests.view_of(&t.name))
        .collect()
}

/// One test's view, `404` when the caller may not see it.
pub fn view_for(state: &AppState, caller: &Resolved, name: &str) -> Result<Value, ServiceError> {
    match state.saved_tests.get(name) {
        Some(t) if visible(caller, &t) => {
            state.saved_tests.view_of(name).ok_or_else(|| no_test(name))
        }
        _ => Err(no_test(name)),
    }
}

/// Where a test the caller saves goes, and may it replace the one of that
/// name: `tests.write` in the test's project (the one named, else the
/// caller's one project), and on the one it replaces (a move needs both).
/// Answers the test with its project set.
pub fn admit_for(
    state: &AppState,
    caller: &Resolved,
    mut test: SavedTest,
) -> Result<SavedTest, ServiceError> {
    crate::audit::target("test", &test.name);
    let write = Perm::TestsWrite;
    let old = state.saved_tests.get(&test.name);
    test.project = match (&old, test.project.as_deref()) {
        // A replace that names no project keeps the old one's.
        (Some(o), None) => o.project.clone(),
        _ => crate::rbac::projects::target(&state.users, caller, write, test.project.as_deref())?,
    };
    if let Some(o) = &old {
        if !visible(caller, o) {
            return Err(ServiceError::new(
                StatusCode::CONFLICT,
                format!("the name {:?} is taken: pick another", test.name),
            ));
        }
        crate::rbac::projects::check_move(
            &state.users,
            caller,
            write,
            o.project.as_deref(),
            test.project.as_deref(),
        )?;
    }
    Ok(test)
}

/// Delete a test (`tests.write` in its project).
pub fn delete_for(state: &AppState, caller: &Resolved, name: &str) -> Result<Value, ServiceError> {
    let Some(t) = state.saved_tests.get(name).filter(|t| visible(caller, t)) else {
        return Err(no_test(name));
    };
    if !caller.has_in(Perm::TestsWrite, t.project.as_deref()) {
        return Err(ServiceError::permission(caller, Perm::TestsWrite));
    }
    match state.saved_tests.delete(name) {
        Ok(true) => Ok(json!({ "deleted": name })),
        Ok(false) => Err(no_test(name)),
        Err(e) => Err(ServiceError::new(e.status(), e.message())),
    }
}

/// Narrow a run to what the caller may run: the names asked for must be
/// tests it sees (`404`) and runs (`tests.run` in their project, `403`);
/// without names, every test it may run (of `project` when given). A caller
/// with the instance-wide permission and no `project` runs as before.
pub fn select_for(
    state: &AppState,
    caller: &Resolved,
    body: &mut run::RunBody,
) -> Result<(), ServiceError> {
    let filter = crate::rbac::projects::Filter::parse(body.project.as_deref())
        .map_err(ServiceError::bad_request)?;
    if filter.is_none() && caller.has(Perm::TestsRun) {
        return Ok(());
    }
    let all = state.saved_tests.list();
    if body.names.is_empty() {
        let names: Vec<String> = all
            .iter()
            .filter(|t| {
                caller.has_in(Perm::TestsRun, t.project.as_deref())
                    && crate::rbac::projects::passes(&filter, t.project.as_deref())
            })
            .map(|t| t.name.clone())
            .collect();
        if names.is_empty() {
            return Err(ServiceError::not_found(
                "no saved test you may run here (in this project)",
            ));
        }
        body.names = names;
        return Ok(());
    }
    for n in &body.names {
        let Some(t) = all
            .iter()
            .find(|t| &t.name == n)
            .filter(|t| visible(caller, t))
        else {
            return Err(no_test(n));
        };
        if !crate::rbac::projects::passes(&filter, t.project.as_deref()) {
            return Err(ServiceError::bad_request(format!(
                "the test {n:?} is not in that project"
            )));
        }
        if !caller.has_in(Perm::TestsRun, t.project.as_deref()) {
            return Err(ServiceError::permission(caller, Perm::TestsRun));
        }
    }
    Ok(())
}

/// May the caller see a kept run? Instance-wide `tests.read`, or
/// `tests.read` in every project of its tests (and the run ran none of the
/// instance's).
pub fn run_visible(caller: &Resolved, scope: &history::Scope) -> bool {
    caller.has(Perm::TestsRead)
        || (!scope.instance
            && scope
                .projects
                .iter()
                .all(|p| caller.has_in(Perm::TestsRead, Some(p))))
}

// ── A run, kept ─────────────────────────────────────────────────────

/// Run the tests and keep the run in the history (every surface runs
/// through here). The values of the run's variables are replaced by their
/// `{{NAME}}` wherever the report would repeat them, so neither the answer
/// nor the kept run holds one. The caller holds the run guard.
pub async fn run_and_keep(
    state: &AppState,
    session: &str,
    body: run::RunBody,
    trigger: &str,
    principal: &str,
) -> Result<history::TestRun, (StatusCode, String)> {
    let selection = body.names.clone();
    let given: Vec<(String, String)> = body
        .variables
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let ran: Vec<SavedTest> = state
        .saved_tests
        .list()
        .into_iter()
        .filter(|t| body.names.is_empty() || body.names.contains(&t.name))
        .collect();
    let scope = history::Scope::of(&ran);
    let mut report = run::run(state, session, body).await?;
    for r in &mut report.results {
        run::scrub(r, &given);
    }
    Ok(state.saved_tests.history.record_in(
        trigger,
        principal,
        selection,
        report,
        scope,
        chrono::Utc::now(),
    ))
}

/// The answer of a run: the report, its id in the history and the change
/// since the previous run.
pub fn run_view(run: &history::TestRun) -> Value {
    let mut v = serde_json::to_value(&run.report).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut v {
        m.insert("run_id".into(), json!(run.id));
        m.insert("trigger".into(), json!(run.trigger));
        m.insert("principal".into(), json!(run.principal));
        m.insert("selection".into(), json!(run.selection));
        m.insert("change".into(), json!(run.change));
    }
    v
}

/// The caller of a REST run, for the history: never a credential.
pub(crate) fn rest_principal(
    state: &AppState,
    headers: &HeaderMap,
    key: Option<&crate::keys::KeyContext>,
) -> String {
    match key.filter(|k| k.shared && k.control) {
        Some(k) => format!("key:{}", k.key.id),
        None => crate::settings::principal(headers, &state.config),
    }
}

// ── Routes ──────────────────────────────────────────────────────────

pub const LIST_PATH: &str = "/_rustybin/tests";
pub const RUNS_PATH: &str = "/_rustybin/tests/runs";
const RUN_ONE_PATH: &str = "/_rustybin/tests/runs/{id}";
/// The header a JUnit answer of a run names the kept run in.
pub const RUN_ID_HEADER: &str = "x-rustybin-test-run";
const ONE_PATH: &str = "/_rustybin/tests/{name}";
pub const RUN_PATH: &str = "/_rustybin/tests/run";
pub const EXPORT_PATH: &str = "/_rustybin/tests/export";

fn refuse(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

#[derive(Deserialize)]
struct ProjectQuery {
    #[serde(default)]
    project: Option<String>,
}

#[allow(clippy::result_large_err)]
fn filter_of(q: &ProjectQuery) -> Result<Option<crate::rbac::projects::Filter>, Response> {
    crate::rbac::projects::Filter::parse(q.project.as_deref())
        .map_err(|e| refuse(StatusCode::BAD_REQUEST, e))
}

fn caller(
    state: &AppState,
    headers: &HeaderMap,
    extensions: &axum::http::Extensions,
) -> Arc<Resolved> {
    crate::control_auth::caller_of(state, headers, extensions)
}

/// Every read needs `tests.read` (a test may hold a captured request's
/// body), in the test's project.
async fn list_handler(
    State(state): State<AppState>,
    Query(q): Query<ProjectQuery>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
) -> Response {
    let filter = match filter_of(&q) {
        Ok(f) => f,
        Err(r) => return r,
    };
    let c = caller(&state, &headers, &extensions);
    Json(json!({
        "tests": views_for(&state, &c, &filter),
        "max_tests": MAX_TESTS,
    }))
    .into_response()
}

async fn save_handler(
    State(state): State<AppState>,
    Session(session): Session,
    key: Option<axum::Extension<crate::keys::KeyContext>>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
    body: Bytes,
) -> Response {
    let body: SaveBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, format!("invalid body: {e}")),
    };
    let key_id = key
        .filter(|k| k.shared && k.control)
        .map(|axum::Extension(k)| k.key.id.to_string());
    let c = caller(&state, &headers, &extensions);
    // The permission refusal with its body (the save answers a message).
    if let Err(e) =
        crate::rbac::projects::target(&state.users, &c, Perm::TestsWrite, body.project.as_deref())
    {
        return e.into_response();
    }
    match save(&state, &c, body, &session, key_id.as_deref()) {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err((s, m)) => refuse(s, m),
    }
}

async fn get_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
) -> Response {
    let c = caller(&state, &headers, &extensions);
    match view_for(&state, &c, &name) {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

/// A full test for `PUT` (the name comes from the path).
pub fn test_from_value(name: &str, mut v: Value) -> Result<SavedTest, String> {
    let Value::Object(m) = &mut v else {
        return Err("a test object".into());
    };
    match m.get("name") {
        Some(Value::String(n)) if n != name => {
            return Err(format!("the body names {n:?}, the path {name:?}"))
        }
        _ => {}
    }
    m.insert("name".into(), json!(name));
    for k in ["saved_at", "variables"] {
        m.remove(k);
    }
    serde_json::from_value(v).map_err(|e| e.to_string())
}

async fn put_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
    body: Bytes,
) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, format!("invalid body: {e}")),
    };
    let test = match test_from_value(&name, v) {
        Ok(t) => t,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, e),
    };
    let c = caller(&state, &headers, &extensions);
    let test = match admit_for(&state, &c, test) {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    match state.saved_tests.put(test, true) {
        Ok(v) => Json(v).into_response(),
        Err(e) => refuse(e.status(), e.message()),
    }
}

async fn delete_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
) -> Response {
    let c = caller(&state, &headers, &extensions);
    match delete_for(&state, &c, &name) {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
struct FormatQuery {
    #[serde(default)]
    format: Option<String>,
}

fn junit_wanted(q: &FormatQuery) -> Result<bool, String> {
    match q.format.as_deref() {
        None | Some("json") => Ok(false),
        Some("junit") => Ok(true),
        Some(f) => Err(format!("format {f:?}: json or junit")),
    }
}

fn junit_response(run: &history::TestRun) -> Response {
    let mut resp = (
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        run::junit(&run.report),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&run.id) {
        resp.headers_mut().insert(RUN_ID_HEADER, v);
    }
    resp
}

async fn run_handler(
    State(state): State<AppState>,
    Session(session): Session,
    Query(q): Query<FormatQuery>,
    key: Option<axum::Extension<crate::keys::KeyContext>>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
    body: Bytes,
) -> Response {
    let mut body: run::RunBody = if body.is_empty() {
        run::RunBody::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(e) => return refuse(StatusCode::BAD_REQUEST, format!("invalid body: {e}")),
        }
    };
    let junit = match junit_wanted(&q) {
        Ok(j) => j,
        Err(m) => return refuse(StatusCode::BAD_REQUEST, m),
    };
    if let Err(e) = select_for(&state, &caller(&state, &headers, &extensions), &mut body) {
        return e.into_response();
    }
    let Some(_running) = state.saved_tests.begin_run() else {
        return refuse(StatusCode::CONFLICT, RUN_BUSY);
    };
    let trigger = history::trigger_from_headers(&headers);
    let principal = rest_principal(&state, &headers, key.as_ref().map(|k| &k.0));
    match run_and_keep(&state, &session, body, trigger, &principal).await {
        Ok(run) if junit => junit_response(&run),
        Ok(run) => Json(run_view(&run)).into_response(),
        Err((s, m)) => refuse(s, m),
    }
}

#[derive(Deserialize)]
struct PageQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

/// `GET /_rustybin/tests/runs`: newest first, `limit` (20, at most 100)
/// and `cursor` (the `next_cursor` of the page before).
async fn runs_handler(
    State(state): State<AppState>,
    Query(q): Query<PageQuery>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
) -> Response {
    let c = caller(&state, &headers, &extensions);
    Json(runs_page(&state, &c, q.limit, q.cursor.as_deref())).into_response()
}

/// A page of the history as the API and the tools answer it: the runs the
/// caller may see.
pub fn runs_page(
    state: &AppState,
    caller: &Resolved,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Value {
    let h = &state.saved_tests.history;
    let p = h.page_where(limit.unwrap_or(history::DEFAULT_PAGE), cursor, |s| {
        run_visible(caller, s)
    });
    json!({
        "runs": p.runs,
        "next_cursor": p.next_cursor,
        "total": p.total,
        "max_runs": h.cap(),
        "max_age_days": history::MAX_AGE_DAYS,
    })
}

async fn clear_runs_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
) -> Response {
    // Every run, everyone's: the instance-wide permission (the route is
    // project-scoped for its GET).
    if let Err(r) = crate::rbac::require(&caller(&state, &headers, &extensions), Perm::TestsWrite) {
        return r;
    }
    let n = state.saved_tests.history.clear();
    tracing::info!(
        principal = %crate::settings::principal(&headers, &state.config),
        "test run history cleared"
    );
    Json(json!({ "cleared": n })).into_response()
}

/// One kept run, if the caller may see it.
pub fn run_for(state: &AppState, caller: &Resolved, id: &str) -> Option<history::TestRun> {
    state.saved_tests.history.get(id).filter(|r| {
        run_visible(
            caller,
            &history::Scope {
                projects: r.projects.clone(),
                instance: r.instance,
            },
        )
    })
}

async fn run_one_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<FormatQuery>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
) -> Response {
    let junit = match junit_wanted(&q) {
        Ok(j) => j,
        Err(m) => return refuse(StatusCode::BAD_REQUEST, m),
    };
    match run_for(&state, &caller(&state, &headers, &extensions), &id) {
        Some(run) if junit => junit_response(&run),
        Some(run) => Json(run_view(&run)).into_response(),
        None if history::valid_id(&id) => {
            refuse(StatusCode::NOT_FOUND, format!("no test run {id:?}"))
        }
        None => refuse(StatusCode::NOT_FOUND, "no such test run"),
    }
}

async fn export_handler(
    State(state): State<AppState>,
    Query(q): Query<ProjectQuery>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
) -> Response {
    let filter = match filter_of(&q) {
        Ok(f) => f,
        Err(r) => return r,
    };
    let c = caller(&state, &headers, &extensions);
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        hurl::export(&tests_for(&state, &c, &filter)),
    )
        .into_response()
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(LIST_PATH, get(list_handler).post(save_handler))
        .route(RUN_PATH, post(run_handler))
        .route(RUNS_PATH, get(runs_handler).delete(clear_runs_handler))
        .route(RUN_ONE_PATH, get(run_one_handler))
        .route(EXPORT_PATH, get(export_handler))
        .route(
            schedule::LIST_PATH,
            get(schedule::list_handler).post(schedule::create_handler),
        )
        .route(
            schedule::ONE_PATH,
            get(schedule::get_handler)
                .put(schedule::put_handler)
                .delete(schedule::delete_handler),
        )
        .route(schedule::RUN_PATH, post(schedule::run_handler))
        .route(
            ONE_PATH,
            get(get_handler).put(put_handler).delete(delete_handler),
        )
}

pub fn catalog() -> Vec<Endpoint> {
    let mut out = vec![
        Endpoint::new(
            LIST_PATH,
            &["GET", "POST"],
            category::CONTROL,
            "Saved tests: list them, or save one from a captured request or a written one (admin)",
        )
.permission_for(&["GET"], crate::rbac::Permission::TestsRead).permission_for(&["POST"], crate::rbac::Permission::TestsWrite).project_scoped()
        .example(Example::get("Saved tests", LIST_PATH)),
        Endpoint::new(
            ONE_PATH,
            &["GET", "PUT", "DELETE"],
            category::CONTROL,
            "A saved test: read, replace or delete it (admin)",
        )
.permission_for(&["GET"], crate::rbac::Permission::TestsRead).permission_for(&["PUT", "DELETE"], crate::rbac::Permission::TestsWrite).project_scoped()
        .example(Example::get("A saved test", "/_rustybin/tests/pet-1").expect_status(404)),
        Endpoint::new(
            RUN_PATH,
            &["POST"],
            category::CONTROL,
            "Run saved tests in process or through their gateway; JSON or JUnit (?format=junit) (admin)",
        )
.permission(crate::rbac::Permission::TestsRun).project_scoped()
        .example(Example::post("Run every test", RUN_PATH).json("{}")),
        Endpoint::new(
            RUNS_PATH,
            &["GET", "DELETE"],
            category::CONTROL,
            "The history of saved-test runs, newest first (limit, cursor); DELETE forgets every run (admin)",
        )
.permission_for(&["GET"], crate::rbac::Permission::TestsRead).permission_for(&["DELETE"], crate::rbac::Permission::TestsWrite).project_scoped()
        .example(Example::get("Test run history", RUNS_PATH)),
        Endpoint::new(
            RUN_ONE_PATH,
            &["GET"],
            category::CONTROL,
            "One kept test run: each test's result, the change since the previous run; JSON or JUnit (?format=junit) (admin)",
        )
.permission(crate::rbac::Permission::TestsRead).project_scoped()
        .example(
            Example::get("A test run", "/_rustybin/tests/runs/20261103T101500000Z-abc123")
                .expect_status(404),
        ),
        Endpoint::new(
            EXPORT_PATH,
            &["GET"],
            category::CONTROL,
            "The saved tests as a Hurl file, assertions included, for CI",
        )
.permission(crate::rbac::Permission::TestsRead).project_scoped()
        .example(Example::get("Export as Hurl", EXPORT_PATH)),
    ];
    out.extend(schedule::catalog());
    out
}

pub fn openapi_paths() -> Value {
    let mut paths = tests_openapi_paths();
    if let (Value::Object(m), Value::Object(more)) = (&mut paths, schedule::openapi_paths()) {
        m.extend(more);
    }
    paths
}

fn tests_openapi_paths() -> Value {
    let obj = json!({ "application/json": { "schema": { "type": "object" } } });
    let name =
        json!({ "name": "name", "in": "path", "required": true, "schema": { "type": "string" } });
    json!({
        LIST_PATH: {
            "get": {
                "tags": ["Control Plane"], "summary": "Saved tests",
                "operationId": "listSavedTests",
                "parameters": [{ "name": "project", "in": "query", "required": false, "description": "One project's tests (instance: the instance's)", "schema": { "type": "string" } }],
                "responses": { "200": { "description": "The tests with the variables each uses", "content": obj } }
            },
            "post": {
                "tags": ["Control Plane"], "summary": "Save a test (admin)",
                "description": "From a captured request (capture: its inspector id; secret-looking headers and query values become {{variables}}) or a written request, with assertions (default for a capture: its status). project: the project it belongs to (tests.write there). At most 200 tests, 64 KiB each.",
                "operationId": "saveTest",
                "requestBody": { "required": true, "content": obj },
                "responses": { "201": { "description": "The saved test", "content": obj }, "400": { "description": "Invalid test" }, "404": { "description": "No such capture you may read" }, "409": { "description": "A test of that name exists" } }
            }
        },
        ONE_PATH: {
            "get": {
                "tags": ["Control Plane"], "summary": "A saved test", "operationId": "getSavedTest",
                "parameters": [name],
                "responses": { "200": { "description": "The test", "content": obj }, "404": { "description": "No such test" } }
            },
            "put": {
                "tags": ["Control Plane"], "summary": "Save or replace a test (admin)", "operationId": "putSavedTest",
                "parameters": [name],
                "requestBody": { "required": true, "content": obj },
                "responses": { "200": { "description": "The saved test", "content": obj }, "400": { "description": "Invalid test" } }
            },
            "delete": {
                "tags": ["Control Plane"], "summary": "Delete a saved test (admin)", "operationId": "deleteSavedTest",
                "parameters": [name],
                "responses": { "200": { "description": "Deleted", "content": obj }, "404": { "description": "No such test" } }
            }
        },
        RUN_PATH: { "post": {
            "tags": ["Control Plane"], "summary": "Run saved tests (admin)",
            "description": "Every test, or names; variables fill {{name}} references (else RUSTYBIN_TEST_* environment variables). Tests without a gateway run in process with the caller's session; gateway tests go through the egress policy (purpose replay). At most 200 requests and 60 seconds a run; one run at a time (409 while another goes). Every run is kept in the history: the answer has its run_id (the X-Rustybin-Test-Run header with JUnit) and the change since the previous run. X-Rustybin-Client (console or cli) labels the run's trigger, else api.",
            "operationId": "runSavedTests",
            "parameters": [{ "name": "format", "in": "query", "required": false, "schema": { "type": "string", "enum": ["json", "junit"] } }],
            "requestBody": { "required": false, "content": { "application/json": { "schema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "names": { "type": "array", "items": { "type": "string" } },
                    "variables": { "type": "object", "additionalProperties": { "type": "string" } }
                }
            } } } },
            "responses": {
                "200": { "description": "The report (JSON, or JUnit XML)", "content": { "application/json": { "schema": { "type": "object" } }, "application/xml": { "schema": { "type": "string" } } } },
                "403": { "description": "The egress policy refused a gateway" },
                "404": { "description": "No such test" },
                "409": { "description": "Another run is going" }
            }
        } },
        RUNS_PATH: {
            "get": {
                "tags": ["Control Plane"], "summary": "The history of test runs (admin)",
                "description": "Every run of the saved tests (console, CLI, API, MCP), newest first: id, ran_at, trigger, principal (never a credential), the counts, the failing tests and the change since the previous run. At most 200 runs and 30 days (50 in memory without a state directory). Variables given to a run are never kept.",
                "operationId": "listTestRuns",
                "parameters": [
                    { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer", "minimum": 1, "maximum": history::MAX_PAGE, "default": history::DEFAULT_PAGE } },
                    { "name": "cursor", "in": "query", "required": false, "schema": { "type": "string" }, "description": "The next_cursor of the page before" }
                ],
                "responses": { "200": { "description": "A page: runs, next_cursor, total", "content": obj } }
            },
            "delete": {
                "tags": ["Control Plane"], "summary": "Forget every test run (admin)", "operationId": "clearTestRuns",
                "responses": { "200": { "description": "cleared: the number of runs forgotten", "content": obj } }
            }
        },
        RUN_ONE_PATH: { "get": {
            "tags": ["Control Plane"], "summary": "A kept test run (admin)", "operationId": "getTestRun",
            "parameters": [
                { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } },
                { "name": "format", "in": "query", "required": false, "schema": { "type": "string", "enum": ["json", "junit"] } }
            ],
            "responses": {
                "200": { "description": "The run: the report, run_id, trigger, principal, selection, change (JSON, or JUnit XML)", "content": { "application/json": { "schema": { "type": "object" } }, "application/xml": { "schema": { "type": "string" } } } },
                "404": { "description": "No such run" }
            }
        } },
        EXPORT_PATH: { "get": {
            "tags": ["Control Plane"], "summary": "Saved tests as Hurl", "operationId": "exportSavedTests",
            "responses": { "200": { "description": "A Hurl file ({{base_url}} for in-process tests)", "content": { "text/plain": { "schema": { "type": "string" } } } } }
        } }
    })
}

/// `tests:` of `rustybin.yaml`.
pub fn schema() -> Value {
    let s = json!({ "type": "string" });
    json!({
        "type": "array", "maxItems": MAX_TESTS,
        "description": "Saved tests: a request (with {{variables}} for credentials, never values) and assertions",
        "items": {
            "type": "object", "additionalProperties": false, "required": ["name", "request"],
            "properties": {
                "name": { "type": "string", "maxLength": MAX_NAME, "pattern": "^[A-Za-z0-9._-]+$" },
                "description": s,
                "gateway": { "type": "string", "description": "A gateway base URL; absent: this instance" },
                "request": {
                    "type": "object", "additionalProperties": false, "required": ["path"],
                    "properties": {
                        "method": { "type": "string", "enum": METHODS },
                        "path": { "type": "string" },
                        "headers": { "type": "object", "additionalProperties": { "type": "string" } },
                        "body": s
                    }
                },
                "assert": { "type": "array", "maxItems": MAX_ASSERTIONS, "items": {
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "status": { "type": "integer", "minimum": 100, "maximum": 599 },
                        "header": s,
                        "body_contains": s,
                        "json": { "type": "string", "description": "A path: $.a.b[0]" },
                        "equals": {},
                        "max_ms": { "type": "integer", "minimum": 0 }
                    }
                } }
            }
        }
    })
}
