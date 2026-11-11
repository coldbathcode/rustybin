//! A mock recorded from a real upstream: Rustybin sits in front of a real API
//! as a recording proxy (`/record/{id}/...`), the presenter or a test suite
//! sends traffic through it, and the recording becomes a mock whose spec is
//! inferred by code from the traffic ([`infer`]) with stored examples taken
//! from the real answers (`source: recorded`).
//!
//! The instance reaches out on a caller's word, so the rules of a spec
//! fetched from a URL hold: starting a recording needs `mocks.write` (never a
//! visitor of a public instance); the proxy goes through the outbound policy
//! under `Purpose::Record`, which has no default (a host is reachable only
//! when the operator named it), with pinned addresses and no redirects
//! followed (a redirect is recorded as an answer). The proxy's address
//! carries a random id of the recording, never its name.
//!
//! What is kept never holds a credential: request headers are not kept
//! (only the content type), query values and JSON fields with
//! secret-looking names become `{{NAME}}`, cookies are dropped, and nothing
//! recorded reaches an audit entry. Recordings are bounded ([`MAX_RECORDINGS`],
//! [`MAX_EXCHANGES`], [`MAX_KEPT_BODY`], [`MAX_BYTES`], [`TTL`]) and belong
//! to the session that started them.

pub mod infer;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{Extensions, HeaderMap, HeaderName, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::egress::Purpose;
use crate::mocks::model::{ExampleSource, OperationSettings, StoredExample};
use crate::mocks::service::{CreateBody, Svc};
use crate::rbac::Permission;
use crate::session::Session;
use crate::state::AppState;

/// Most recordings at once, every caller together.
pub const MAX_RECORDINGS: usize = 8;
/// Most exchanges a recording keeps.
pub const MAX_EXCHANGES: usize = 500;
/// Most of each body kept (a longer one is cut and marked).
pub const MAX_KEPT_BODY: usize = 64 * 1024;
/// Most bytes of bodies a recording keeps in all.
pub const MAX_BYTES: usize = 4 << 20;
/// Most of an upstream answer the proxy passes on.
pub const MAX_FORWARD_BODY: usize = 8 << 20;
/// A recording ends after this.
pub const TTL: Duration = Duration::from_secs(3600);
/// How long the proxy waits for the upstream.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);
/// The proxy's path.
pub const PROXY_PREFIX: &str = "/record/";

pub const LIST_PATH: &str = "/_rustybin/recordings";
const ONE_PATH: &str = "/_rustybin/recordings/{name}";
const STOP_PATH: &str = "/_rustybin/recordings/{name}/stop";
const BUILD_PATH: &str = "/_rustybin/recordings/{name}/build";

/// One request and its answer, as kept (redacted, bodies cut).
#[derive(Clone, Debug, Default, Serialize)]
pub struct Exchange {
    pub at: String,
    pub method: String,
    pub path: String,
    /// Query parameters; secret-looking values as `{{NAME}}`.
    pub query: Vec<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
    /// A body was longer than [`MAX_KEPT_BODY`] and is cut.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    pub ms: u64,
}

impl Exchange {
    fn bytes(&self) -> usize {
        self.request_body.as_ref().map_or(0, String::len)
            + self.response_body.as_ref().map_or(0, String::len)
    }
}

struct Recording {
    name: String,
    /// In the proxy's address (96 random bits), never the name.
    id: String,
    upstream: String,
    mock: String,
    owner: String,
    created_at: String,
    expires: Instant,
    expires_at: String,
    recording: bool,
    exchanges: Vec<Exchange>,
    bytes: usize,
    /// Requests forwarded but not kept (over a cap).
    not_kept: u32,
    /// Requests refused (stopped, or the egress policy).
    refused: u32,
}

impl Recording {
    fn view(&self, base: &str, with_exchanges: bool) -> Value {
        let mut v = json!({
            "name": self.name,
            "upstream": self.upstream,
            "mock": self.mock,
            "proxy_url": format!("{base}{PROXY_PREFIX}{}", self.id),
            "state": if self.recording { "recording" } else { "stopped" },
            "created_at": self.created_at,
            "expires_at": self.expires_at,
            "exchanges": self.exchanges.len(),
            "bytes": self.bytes,
            "not_kept": self.not_kept,
            "refused": self.refused,
            "limits": { "exchanges": MAX_EXCHANGES, "bytes": MAX_BYTES, "body": MAX_KEPT_BODY },
        });
        if with_exchanges {
            v["recorded"] = json!(self.exchanges);
        }
        v
    }
}

/// A refusal: status and message.
#[derive(Debug)]
pub struct Refused(pub StatusCode, pub String);

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn refused(status: StatusCode, m: impl Into<String>) -> Refused {
    Refused(status, m.into())
}

/// The recordings of an instance (`AppState.recordings`).
#[derive(Default)]
pub struct Recordings {
    list: Mutex<Vec<Recording>>,
}

impl Recordings {
    fn lock(&self) -> MutexGuard<'_, Vec<Recording>> {
        let mut l = self.list.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        l.retain(|r| r.expires > now);
        l
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn now_text() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A name: 1 to 40 of `a-z`, `0-9`, `-`, starting with a letter.
fn check_name(n: &str) -> Result<(), Refused> {
    let ok = !n.is_empty()
        && n.len() <= 40
        && n.starts_with(|c: char| c.is_ascii_lowercase())
        && n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(refused(
            StatusCode::BAD_REQUEST,
            format!("name {n:?}: 1 to 40 of a-z, 0-9 and -, starting with a letter"),
        ))
    }
}

/// What starting takes.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartBody {
    pub name: String,
    /// The real API's base URL (https or http, no query).
    pub upstream: String,
    /// The mock the recording becomes (default: the recording's name).
    #[serde(default)]
    pub mock: Option<String>,
}

/// Start a recording: answers the proxy URL to send traffic to, and what
/// the outbound policy says of the upstream (nothing is sent yet).
pub async fn start(
    state: &AppState,
    owner: &str,
    base: &str,
    body: StartBody,
) -> Result<Value, Refused> {
    check_name(&body.name)?;
    let mock = body.mock.clone().unwrap_or_else(|| body.name.clone());
    crate::mocks::service::check_name(&mock)
        .map_err(|e| refused(StatusCode::BAD_REQUEST, format!("mock: {}", e.message())))?;
    let upstream = body.upstream.trim().trim_end_matches('/').to_string();
    crate::egress::parse_url(&upstream, false)
        .map_err(|e| refused(StatusCode::BAD_REQUEST, format!("upstream: {e}")))?;
    if upstream.contains('?') || upstream.contains('#') {
        return Err(refused(
            StatusCode::BAD_REQUEST,
            "upstream: a base URL, without a query or a fragment",
        ));
    }
    let egress = state.egress.dry_check(&upstream, Purpose::Record);
    let now = chrono::Utc::now();
    let rec = Recording {
        name: body.name.clone(),
        id: random_hex(12),
        upstream,
        mock,
        owner: owner.to_string(),
        created_at: now_text(),
        expires: Instant::now() + TTL,
        expires_at: (now + chrono::TimeDelta::from_std(TTL).unwrap_or_default())
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        recording: true,
        exchanges: Vec::new(),
        bytes: 0,
        not_kept: 0,
        refused: 0,
    };
    let mut list = state.recordings.lock();
    if list.iter().any(|r| r.name == body.name) {
        return Err(refused(
            StatusCode::CONFLICT,
            format!(
                "a recording {:?} exists: end it or choose another name",
                body.name
            ),
        ));
    }
    if list.len() >= MAX_RECORDINGS {
        return Err(refused(
            StatusCode::TOO_MANY_REQUESTS,
            format!("at most {MAX_RECORDINGS} recordings at once: delete one first"),
        ));
    }
    let mut v = rec.view(base, false);
    v["egress"] = egress;
    list.push(rec);
    tracing::info!(recording = %body.name, "recording started");
    Ok(v)
}

/// The caller's recordings.
pub fn list(state: &AppState, owner: &str, base: &str) -> Value {
    let list = state.recordings.lock();
    json!({
        "recordings": list.iter().filter(|r| r.owner == owner).map(|r| r.view(base, false)).collect::<Vec<_>>(),
        "max_recordings": MAX_RECORDINGS,
    })
}

fn not_found(name: &str) -> Refused {
    refused(
        StatusCode::NOT_FOUND,
        format!("no recording {name:?} in this session"),
    )
}

/// One of the caller's recordings with what it kept.
pub fn get_one(state: &AppState, owner: &str, base: &str, name: &str) -> Result<Value, Refused> {
    let list = state.recordings.lock();
    list.iter()
        .find(|r| r.name == name && r.owner == owner)
        .map(|r| r.view(base, true))
        .ok_or_else(|| not_found(name))
}

/// Stop recording: the proxy refuses from now on; what was kept stays.
pub fn stop(state: &AppState, owner: &str, base: &str, name: &str) -> Result<Value, Refused> {
    let mut list = state.recordings.lock();
    let r = list
        .iter_mut()
        .find(|r| r.name == name && r.owner == owner)
        .ok_or_else(|| not_found(name))?;
    r.recording = false;
    Ok(r.view(base, false))
}

/// Delete a recording and what it kept (never the mock it built).
pub fn delete(state: &AppState, owner: &str, name: &str) -> Result<Value, Refused> {
    let mut list = state.recordings.lock();
    let i = list
        .iter()
        .position(|r| r.name == name && r.owner == owner)
        .ok_or_else(|| not_found(name))?;
    let r = list.remove(i);
    Ok(json!({ "deleted": r.name, "exchanges": r.exchanges.len() }))
}

/// What building takes.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildBody {
    /// Another mock name than the recording's.
    #[serde(default)]
    pub mock: Option<String>,
    /// Answer the inferred spec and what would be made, change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// Build the mock: the inferred spec, compiled and checked like any other,
/// with the recorded answers as stored examples. Answers the operations and
/// what looked like personal data, by kind and operation, never the value.
pub async fn build(
    state: &AppState,
    svc: Svc,
    owner: &str,
    name: &str,
    body: BuildBody,
) -> Result<Value, Refused> {
    let (exchanges, upstream, mock) = {
        let list = state.recordings.lock();
        let r = list
            .iter()
            .find(|r| r.name == name && r.owner == owner)
            .ok_or_else(|| not_found(name))?;
        (
            r.exchanges.clone(),
            r.upstream.clone(),
            body.mock.clone().unwrap_or_else(|| r.mock.clone()),
        )
    };
    if exchanges.is_empty() {
        return Err(refused(
            StatusCode::CONFLICT,
            "nothing recorded yet: send requests through the proxy URL first",
        ));
    }
    crate::mocks::service::check_name(&mock)
        .map_err(|e| refused(StatusCode::BAD_REQUEST, format!("mock: {}", e.message())))?;
    let inferred = infer::infer(&exchanges, &upstream);
    // Personal data seen in what the real API answered: kinds per operation.
    let mut findings = Vec::new();
    for op in &inferred.operations {
        let id = op["operation"].as_str().unwrap_or("");
        let mut kinds: Vec<&str> = Vec::new();
        if let Some(by_status) = inferred.examples.get(id) {
            for examples in by_status.values() {
                for e in examples {
                    for k in infer::personal_data(&e.to_string()) {
                        if !kinds.contains(&k) {
                            kinds.push(k);
                        }
                    }
                }
            }
        }
        if !kinds.is_empty() {
            findings.push(json!({ "operation": id, "kinds": kinds }));
        }
    }
    let mut operations = BTreeMap::new();
    let mut stored = 0usize;
    for (id, by_status) in &inferred.examples {
        let mut settings = OperationSettings::default();
        for (status, values) in by_status {
            let list: Vec<StoredExample> = values
                .iter()
                .enumerate()
                .map(|(i, v)| StoredExample {
                    name: format!("recorded-{}", i + 1),
                    value: v.clone(),
                    source: ExampleSource::Recorded,
                    created_at: Some(now_text()),
                })
                .collect();
            stored += list.len();
            settings.examples.insert(status.clone(), list);
        }
        operations.insert(id.clone(), settings);
    }
    let mut answer = json!({
        "recording": name,
        "mock": mock,
        "dry_run": body.dry_run,
        "operations": inferred.operations,
        "stored_examples": stored,
        "personal_data": findings,
        "review": "The examples are what the real API answered: review them before publishing the mock to the portal.",
    });
    if body.dry_run {
        answer["spec"] = inferred.spec;
        return Ok(answer);
    }
    let create = CreateBody {
        name: Some(mock.clone()),
        spec: inferred.spec,
        operations: Some(operations),
        deploy: Some(true),
        ..CreateBody::default()
    };
    let created = svc
        .compiling(move |s| s.create(create))
        .await
        .map_err(|e| Refused(e.status, e.message()))?;
    answer["created"] = created;
    tracing::info!(recording = name, mock = %mock, "mock built from a recording");
    Ok(answer)
}

// ── The proxy ───────────────────────────────────────────────────────

/// Headers never forwarded as they are, or Rustybin's own.
const NOT_FORWARDED: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "te",
    "trailer",
    "upgrade",
    "proxy-connection",
    "proxy-authorization",
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-real-ip",
];

/// Does a key name a secret (a query parameter or a JSON field)?
pub fn secret_name(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    [
        "key",
        "token",
        "secret",
        "password",
        "passwd",
        "signature",
        "auth",
        "session",
        "cookie",
        "credential",
        "sig",
    ]
    .iter()
    .any(|s| k.contains(s))
}

/// `{{NAME}}` for a key: upper case, `_` for anything else.
fn variable(k: &str) -> String {
    let n: String = k
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{{{{{n}}}}}")
}

/// A JSON value with secret-looking fields replaced.
pub fn redact_json(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (k, val) in m.iter_mut() {
                if secret_name(k) && !val.is_object() && !val.is_array() {
                    *val = Value::String(variable(k));
                } else {
                    redact_json(val);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(redact_json),
        _ => {}
    }
}

/// A body as kept: JSON redacted, text cut at [`MAX_KEPT_BODY`], binary
/// left out.
fn kept_body(bytes: &[u8], content_type: Option<&str>) -> (Option<String>, bool) {
    if bytes.is_empty() {
        return (None, false);
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return (Some(format!("({} bytes, not text)", bytes.len())), false);
    };
    let json_type = content_type.is_none_or(|c| c.contains("json"));
    if json_type {
        if let Ok(mut v) = serde_json::from_str::<Value>(text) {
            redact_json(&mut v);
            let s = v.to_string();
            if s.len() <= MAX_KEPT_BODY {
                return (Some(s), false);
            }
        }
    }
    let cut = text.len() > MAX_KEPT_BODY;
    let mut end = text.len().min(MAX_KEPT_BODY);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (Some(text[..end].to_string()), cut)
}

fn header_text(h: &HeaderMap, name: &str) -> Option<String> {
    h.get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

async fn proxy_root(
    State(state): State<AppState>,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    proxy(state, id, String::new(), req).await
}

async fn proxy_path(
    State(state): State<AppState>,
    Path((id, path)): Path<(String, String)>,
    req: Request,
) -> Response {
    proxy(state, id, path, req).await
}

/// Forward one request to the recording's upstream and keep the exchange.
async fn proxy(state: AppState, id: String, path: String, req: Request) -> Response {
    let (upstream, name) = {
        let mut list = state.recordings.lock();
        let Some(r) = list.iter_mut().find(|r| r.id == id) else {
            return refused(StatusCode::NOT_FOUND, "no such recording").into_response();
        };
        if !r.recording {
            r.refused += 1;
            return refused(
                StatusCode::GONE,
                format!("recording {:?} is stopped", r.name),
            )
            .into_response();
        }
        (r.upstream.clone(), r.name.clone())
    };
    let method = req.method().clone();
    let query = req.uri().query().map(str::to_string);
    let headers = req.headers().clone();
    let body = match axum::body::to_bytes(req.into_body(), state.config.body_limit).await {
        Ok(b) => b,
        Err(_) => {
            return refused(
                StatusCode::PAYLOAD_TOO_LARGE,
                "the request body is over this instance's limit",
            )
            .into_response()
        }
    };
    let rel = if path.is_empty() {
        String::new()
    } else {
        format!("/{path}")
    };
    let target = match &query {
        Some(q) => format!("{upstream}{rel}?{q}"),
        None => format!("{upstream}{rel}"),
    };
    let pinned = match state.egress.resolve(&target, Purpose::Record).await {
        Ok(p) => p,
        Err(e) => {
            if let Some(r) = state.recordings.lock().iter_mut().find(|r| r.id == id) {
                r.refused += 1;
            }
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": e.0, "reason": "egress_refused", "purpose": "record" })),
            )
                .into_response();
        }
    };
    let client = match state.egress.client(&pinned, UPSTREAM_TIMEOUT) {
        Ok(c) => c,
        Err(e) => return refused(StatusCode::BAD_GATEWAY, e).into_response(),
    };
    let mut out = client
        .request(method.clone(), &pinned.url)
        .body(body.clone());
    for (k, v) in headers.iter() {
        let n = k.as_str();
        if NOT_FORWARDED.contains(&n) || n.starts_with("x-rustybin-") {
            continue;
        }
        out = out.header(k, v);
    }
    let t = Instant::now();
    let resp = match out.send().await {
        Ok(r) => r,
        Err(e) => {
            let kind = if e.is_timeout() {
                "timed out"
            } else {
                "could not reach the upstream"
            };
            return refused(StatusCode::BAD_GATEWAY, kind).into_response();
        }
    };
    let status = resp.status();
    let resp_headers = resp.headers().clone();
    let mut got = Vec::new();
    let mut resp = resp;
    loop {
        match resp.chunk().await {
            Ok(Some(c)) => {
                if got.len() + c.len() > MAX_FORWARD_BODY {
                    return refused(
                        StatusCode::BAD_GATEWAY,
                        format!("the upstream answered more than {MAX_FORWARD_BODY} bytes"),
                    )
                    .into_response();
                }
                got.extend_from_slice(&c);
            }
            Ok(None) => break,
            Err(_) => {
                return refused(StatusCode::BAD_GATEWAY, "the upstream's answer broke off")
                    .into_response()
            }
        }
    }
    let ms = t.elapsed().as_millis() as u64;
    // Keep the exchange, redacted.
    let request_type = header_text(&headers, "content-type");
    let response_type = header_text(&resp_headers, "content-type");
    let (request_body, cut_req) = kept_body(&body, request_type.as_deref());
    let (response_body, cut_resp) = kept_body(&got, response_type.as_deref());
    let query_kept: Vec<(String, String)> = query
        .as_deref()
        .map(|q| {
            form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| {
                    let v = if secret_name(&k) {
                        variable(&k)
                    } else {
                        v.into_owned()
                    };
                    (k.into_owned(), v)
                })
                .collect()
        })
        .unwrap_or_default();
    let exchange = Exchange {
        at: now_text(),
        method: method.to_string(),
        path: if rel.is_empty() { "/".to_string() } else { rel },
        query: query_kept,
        request_type,
        request_body,
        status: status.as_u16(),
        response_type,
        response_body,
        truncated: cut_req || cut_resp,
        ms,
    };
    if let Some(r) = state.recordings.lock().iter_mut().find(|r| r.id == id) {
        let size = exchange.bytes();
        if r.exchanges.len() >= MAX_EXCHANGES || r.bytes + size > MAX_BYTES {
            r.not_kept += 1;
        } else {
            r.bytes += size;
            r.exchanges.push(exchange);
        }
    }
    tracing::debug!(recording = %name, status = status.as_u16(), "exchange recorded");
    // The answer as the upstream gave it (hop-by-hop headers aside).
    let mut answer = Response::new(Body::from(got));
    *answer.status_mut() = status;
    for (k, v) in resp_headers.iter() {
        let n = k.as_str();
        if NOT_FORWARDED.contains(&n) {
            continue;
        }
        if let (Ok(k), Ok(v)) = (
            HeaderName::from_bytes(k.as_str().as_bytes()),
            HeaderValue::from_bytes(v.as_bytes()),
        ) {
            answer.headers_mut().append(k, v);
        }
    }
    answer
}

// ── Routes ──────────────────────────────────────────────────────────

fn base_of(state: &AppState, headers: &HeaderMap, extensions: &Extensions, uri: &Uri) -> String {
    Svc::from_request(state, headers, extensions, uri)
        .links
        .base_url()
        .to_string()
}

fn answer(r: Result<Value, Refused>, created: bool) -> Response {
    match r {
        Ok(v) if created => (StatusCode::CREATED, Json(v)).into_response(),
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn list_handler(
    State(state): State<AppState>,
    Session(owner): Session,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let base = base_of(&state, &headers, &extensions, &uri);
    Json(list(&state, &owner, &base)).into_response()
}

async fn start_handler(
    State(state): State<AppState>,
    Session(owner): Session,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: axum::body::Bytes,
) -> Response {
    let body: StartBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            return refused(StatusCode::BAD_REQUEST, format!("invalid body: {e}")).into_response()
        }
    };
    let base = base_of(&state, &headers, &extensions, &uri);
    answer(start(&state, &owner, &base, body).await, true)
}

async fn get_handler(
    State(state): State<AppState>,
    Session(owner): Session,
    Path(name): Path<String>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let base = base_of(&state, &headers, &extensions, &uri);
    answer(get_one(&state, &owner, &base, &name), false)
}

async fn stop_handler(
    State(state): State<AppState>,
    Session(owner): Session,
    Path(name): Path<String>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let base = base_of(&state, &headers, &extensions, &uri);
    answer(stop(&state, &owner, &base, &name), false)
}

async fn delete_handler(
    State(state): State<AppState>,
    Session(owner): Session,
    Path(name): Path<String>,
) -> Response {
    answer(delete(&state, &owner, &name), false)
}

async fn build_handler(
    State(state): State<AppState>,
    Session(owner): Session,
    Path(name): Path<String>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: axum::body::Bytes,
) -> Response {
    let body: BuildBody = if body.is_empty() {
        BuildBody::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(e) => {
                return refused(StatusCode::BAD_REQUEST, format!("invalid body: {e}"))
                    .into_response()
            }
        }
    };
    let svc = Svc::from_request(&state, &headers, &extensions, &uri);
    let dry = body.dry_run;
    answer(build(&state, svc, &owner, &name, body).await, !dry)
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(LIST_PATH, get(list_handler).post(start_handler))
        .route(ONE_PATH, get(get_handler).delete(delete_handler))
        .route(STOP_PATH, post(stop_handler))
        .route(BUILD_PATH, post(build_handler))
        .route("/record/{id}", any(proxy_root))
        .route("/record/{id}/{*path}", any(proxy_path))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(LIST_PATH, &["GET", "POST"], category::MOCKS, "Recordings of a real API (GET, the caller's) and starting one (POST: a name, the upstream's base URL)")
            .permission_for(&["GET"], Permission::MocksRead)
            .permission_for(&["POST"], Permission::MocksWrite)
            .example(Example::get("Recordings", LIST_PATH)),
        Endpoint::new(ONE_PATH, &["GET", "DELETE"], category::MOCKS, "A recording with what it kept (redacted); DELETE removes it (never the mock it built)")
            .permission_for(&["GET"], Permission::TrafficRead)
            .permission_for(&["DELETE"], Permission::MocksWrite)
            .example(Example::get("A recording", "/_rustybin/recordings/orders").expect_status(404)),
        Endpoint::new(STOP_PATH, &["POST"], category::MOCKS, "Stop recording: the proxy refuses from now on")
            .permission(Permission::MocksWrite)
            .example(Example::post("Stop", "/_rustybin/recordings/orders/stop").expect_status(404)),
        Endpoint::new(BUILD_PATH, &["POST"], category::MOCKS, "Build a mock from a recording: an inferred spec, the recorded answers as examples (dry_run answers the spec)")
            .permission(Permission::MocksWrite)
            .example(Example::post("Build", "/_rustybin/recordings/orders/build").json(r#"{"dry_run": true}"#).expect_status(404)),
        Endpoint::new("/record/{id}", &["ANY"], category::MOCKS, "The recording proxy: forwards to the recording's upstream (egress purpose record) and keeps the exchange")
            .example(Example::get("The proxy of no recording", "/record/0000").expect_status(404)),
        Endpoint::new("/record/{id}/{*path}", &["ANY"], category::MOCKS, "The recording proxy with a path")
            .example(Example::get("The proxy of no recording", "/record/0000/pets/1").expect_status(404)),
    ]
}

pub fn openapi_paths() -> Value {
    let name =
        json!({ "name": "name", "in": "path", "required": true, "schema": { "type": "string" } });
    let id =
        json!({ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } });
    let path =
        json!({ "name": "path", "in": "path", "required": true, "schema": { "type": "string" } });
    let obj = json!({ "application/json": { "schema": { "type": "object" } } });
    let proxy = |op: &str, params: Value| {
        let one = |m: &str| json!({ "tags": ["Mocks"], "summary": "The recording proxy", "operationId": format!("{op}{m}"), "parameters": params.clone(), "responses": { "200": { "description": "What the upstream answered" }, "403": { "description": "The egress policy refused the upstream" }, "404": { "description": "No such recording" }, "410": { "description": "The recording is stopped" } } });
        json!({ "get": one("Get"), "post": one("Post"), "put": one("Put"), "patch": one("Patch"), "delete": one("Delete") })
    };
    json!({
        LIST_PATH: {
            "get": { "tags": ["Mocks"], "summary": "The caller's recordings", "operationId": "listRecordings", "responses": { "200": { "description": "The recordings", "content": obj } } },
            "post": { "tags": ["Mocks"], "summary": "Start a recording", "operationId": "startRecording",
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["name", "upstream"], "additionalProperties": false, "properties": {
                    "name": { "type": "string" }, "upstream": { "type": "string", "description": "The real API's base URL" }, "mock": { "type": "string" } } } } } },
                "responses": { "201": { "description": "The recording, its proxy URL and the egress check", "content": obj }, "400": { "description": "Invalid" }, "409": { "description": "The name is taken" }, "429": { "description": "Too many recordings" } } }
        },
        ONE_PATH: {
            "get": { "tags": ["Mocks"], "summary": "A recording with what it kept", "operationId": "getRecording", "parameters": [name], "responses": { "200": { "description": "The recording", "content": obj }, "404": { "description": "No such recording in this session" } } },
            "delete": { "tags": ["Mocks"], "summary": "Delete a recording", "operationId": "deleteRecording", "parameters": [name], "responses": { "200": { "description": "Deleted", "content": obj }, "404": { "description": "No such recording in this session" } } }
        },
        STOP_PATH: { "post": { "tags": ["Mocks"], "summary": "Stop a recording", "operationId": "stopRecording", "parameters": [name], "responses": { "200": { "description": "Stopped", "content": obj }, "404": { "description": "No such recording in this session" } } } },
        BUILD_PATH: { "post": { "tags": ["Mocks"], "summary": "Build a mock from a recording", "operationId": "buildMockFromRecording", "parameters": [name],
            "requestBody": { "required": false, "content": { "application/json": { "schema": { "type": "object", "additionalProperties": false, "properties": { "mock": { "type": "string" }, "dry_run": { "type": "boolean" } } } } } },
            "responses": { "200": { "description": "The dry run: the spec and what would be made", "content": obj }, "201": { "description": "The mock, the operations and the personal data seen", "content": obj }, "404": { "description": "No such recording in this session" }, "409": { "description": "Nothing recorded, or the mock exists" } } } },
        "/record/{id}": proxy("recordProxy", json!([id])),
        "/record/{id}/{path}": proxy("recordProxyPath", json!([id.clone(), path])),
    })
}
