//! Saved tests: a captured request (or one written by hand) with assertions
//! (status, a header, the body, a JSON value, the time), replayed against
//! this instance in process or through a gateway, and run in CI (JUnit, or
//! exported as Hurl). Kept in `<state>/tests.json` and in `rustybin.yaml`
//! (`tests:`). Credentials are never saved: a captured secret-looking header
//! or query value becomes a `{{name}}` variable the run fills (from the run's
//! `variables`, else a `RUSTYBIN_TEST_*` environment variable).

pub mod hurl;
pub mod run;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

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

/// A name a test may have (`run` and `export` are routes).
pub fn valid_name(name: &str) -> bool {
    !matches!(name, "run" | "export")
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
                "name {:?}: 1 to {MAX_NAME} letters, digits, . _ - (not run or export)",
                self.name
            ));
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
}

/// Why a store change was refused.
#[derive(Debug)]
pub enum StoreError {
    Invalid(String),
    Exists(String),
    Full,
    NotFound(String),
}

impl StoreError {
    pub fn status(&self) -> StatusCode {
        match self {
            StoreError::Invalid(_) => StatusCode::BAD_REQUEST,
            StoreError::Exists(_) => StatusCode::CONFLICT,
            StoreError::Full => StatusCode::INSUFFICIENT_STORAGE,
            StoreError::NotFound(_) => StatusCode::NOT_FOUND,
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
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Entry>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Write the file (serialised under the lock, written after it).
    fn save_unlock(&self, items: MutexGuard<'_, BTreeMap<String, Entry>>) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let doc = Doc {
            version: 1,
            tests: items.values().cloned().collect(),
        };
        let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
        let order = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        drop(items);
        let r = crate::settings::write_private(path, &bytes);
        drop(order);
        r
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
    pub fn put(&self, test: SavedTest, replace: bool) -> Result<Value, StoreError> {
        test.validate().map_err(StoreError::Invalid)?;
        let mut items = self.lock();
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
        if let Err(e) = self.save_unlock(items) {
            tracing::warn!("tests: {e}");
        }
        Ok(view)
    }

    pub fn delete(&self, name: &str) -> bool {
        let mut items = self.lock();
        let removed = items.remove(name).is_some();
        if removed {
            if let Err(e) = self.save_unlock(items) {
                tracing::warn!("tests: {e}");
            }
        }
        removed
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
}

/// Save a new test (admin, checked by the caller of this function).
pub fn save(
    state: &AppState,
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
    };
    let mut v = state
        .saved_tests
        .put(test, false)
        .map_err(|e| (e.status(), e.message()))?;
    if !introduced.is_empty() {
        v["introduced_variables"] = json!(introduced);
    }
    Ok(v)
}

// ── Routes ──────────────────────────────────────────────────────────

pub const LIST_PATH: &str = "/_rustybin/tests";
const ONE_PATH: &str = "/_rustybin/tests/{name}";
pub const RUN_PATH: &str = "/_rustybin/tests/run";
pub const EXPORT_PATH: &str = "/_rustybin/tests/export";

fn refuse(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

/// Every read is admin: a test may hold a captured request's body.
async fn list_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = crate::admin::require_admin(&headers, &state.config) {
        return r;
    }
    Json(json!({
        "tests": state.saved_tests.views(),
        "max_tests": MAX_TESTS,
    }))
    .into_response()
}

async fn save_handler(
    State(state): State<AppState>,
    Session(session): Session,
    key: Option<axum::Extension<crate::keys::KeyContext>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(r) = crate::admin::require_admin(&headers, &state.config) {
        return r;
    }
    let body: SaveBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, format!("invalid body: {e}")),
    };
    let key_id = key
        .filter(|k| k.shared && k.control)
        .map(|axum::Extension(k)| k.key.id.to_string());
    match save(&state, body, &session, key_id.as_deref()) {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err((s, m)) => refuse(s, m),
    }
}

async fn get_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = crate::admin::require_admin(&headers, &state.config) {
        return r;
    }
    match state.saved_tests.view_of(&name) {
        Some(v) => Json(v).into_response(),
        None => refuse(StatusCode::NOT_FOUND, format!("no test named {name:?}")),
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
    body: Bytes,
) -> Response {
    if let Err(r) = crate::admin::require_admin(&headers, &state.config) {
        return r;
    }
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, format!("invalid body: {e}")),
    };
    let test = match test_from_value(&name, v) {
        Ok(t) => t,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, e),
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
) -> Response {
    if let Err(r) = crate::admin::require_admin(&headers, &state.config) {
        return r;
    }
    if state.saved_tests.delete(&name) {
        Json(json!({ "deleted": name })).into_response()
    } else {
        refuse(StatusCode::NOT_FOUND, format!("no test named {name:?}"))
    }
}

#[derive(Deserialize)]
struct FormatQuery {
    #[serde(default)]
    format: Option<String>,
}

async fn run_handler(
    State(state): State<AppState>,
    Session(session): Session,
    Query(q): Query<FormatQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(r) = crate::admin::require_admin(&headers, &state.config) {
        return r;
    }
    let body: run::RunBody = if body.is_empty() {
        run::RunBody::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(e) => return refuse(StatusCode::BAD_REQUEST, format!("invalid body: {e}")),
        }
    };
    let junit = match q.format.as_deref() {
        None | Some("json") => false,
        Some("junit") => true,
        Some(f) => {
            return refuse(
                StatusCode::BAD_REQUEST,
                format!("format {f:?}: json or junit"),
            )
        }
    };
    match run::run(&state, &session, body).await {
        Ok(report) if junit => (
            [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
            run::junit(&report),
        )
            .into_response(),
        Ok(report) => Json(report).into_response(),
        Err((s, m)) => refuse(s, m),
    }
}

async fn export_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = crate::admin::require_admin(&headers, &state.config) {
        return r;
    }
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        hurl::export(&state.saved_tests.list()),
    )
        .into_response()
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(LIST_PATH, get(list_handler).post(save_handler))
        .route(RUN_PATH, post(run_handler))
        .route(EXPORT_PATH, get(export_handler))
        .route(
            ONE_PATH,
            get(get_handler).put(put_handler).delete(delete_handler),
        )
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            LIST_PATH,
            &["GET", "POST"],
            category::CONTROL,
            "Saved tests: list them, or save one from a captured request or a written one (admin)",
        )
        .example(Example::get("Saved tests", LIST_PATH)),
        Endpoint::new(
            ONE_PATH,
            &["GET", "PUT", "DELETE"],
            category::CONTROL,
            "A saved test: read, replace or delete it (admin)",
        )
        .example(Example::get("A saved test", "/_rustybin/tests/pet-1").expect_status(404)),
        Endpoint::new(
            RUN_PATH,
            &["POST"],
            category::CONTROL,
            "Run saved tests in process or through their gateway; JSON or JUnit (?format=junit) (admin)",
        )
        .example(Example::post("Run every test", RUN_PATH).json("{}")),
        Endpoint::new(
            EXPORT_PATH,
            &["GET"],
            category::CONTROL,
            "The saved tests as a Hurl file, assertions included, for CI",
        )
        .example(Example::get("Export as Hurl", EXPORT_PATH)),
    ]
}

pub fn openapi_paths() -> Value {
    let obj = json!({ "application/json": { "schema": { "type": "object" } } });
    let name =
        json!({ "name": "name", "in": "path", "required": true, "schema": { "type": "string" } });
    json!({
        LIST_PATH: {
            "get": {
                "tags": ["Control Plane"], "summary": "Saved tests",
                "operationId": "listSavedTests",
                "responses": { "200": { "description": "The tests with the variables each uses", "content": obj } }
            },
            "post": {
                "tags": ["Control Plane"], "summary": "Save a test (admin)",
                "description": "From a captured request (capture: its inspector id; secret-looking headers and query values become {{variables}}) or a written request, with assertions (default for a capture: its status). At most 200 tests, 64 KiB each.",
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
            "description": "Every test, or names; variables fill {{name}} references (else RUSTYBIN_TEST_* environment variables). Tests without a gateway run in process with the caller's session; gateway tests go through the egress policy (purpose replay). At most 200 requests and 60 seconds a run.",
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
                "404": { "description": "No such test" }
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
