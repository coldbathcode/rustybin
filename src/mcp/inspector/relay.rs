//! The relay: the console's MCP client sends its JSON-RPC messages through
//! the instance when the browser cannot reach a server (no CORS, a private
//! network the instance sees). `POST /_rustybin/mcp-relay` forwards one
//! message and streams the answer back as SSE (`relay` with the upstream
//! status and headers, `message` per JSON-RPC message, `body` for a
//! non-JSON-RPC answer, `done`). Targets on this instance are served in
//! process; anything else goes through the egress policy under
//! `Purpose::McpRelay` (`RUSTYBIN_EGRESS_ALLOW` names the hosts, loopback
//! only with the switch, never this instance's control plane).
//!
//! A relay session (at most 64, idle 10 minutes) keeps the upstream session
//! id of a Streamable HTTP server and the open stream of a legacy one,
//! keyed by an id the browser holds. Credentials travel in the request (or
//! come from a saved connection) and live only as long as the session; they
//! are never logged and the control plane is never captured.

use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use futures_util::StreamExt;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::client::{self, Legacy, Target};
use super::Inspector;
use crate::state::AppState;

pub const MAX_SESSIONS: usize = 64;
pub const IDLE: Duration = Duration::from_secs(600);
/// Largest body the fetch helper returns.
pub const MAX_FETCH_BYTES: usize = 256 * 1024;
/// Upstream headers the browser sees.
const SHOWN_HEADERS: &[&str] = &[
    "content-type",
    "mcp-session-id",
    "mcp-protocol-version",
    "www-authenticate",
    "retry-after",
    "ratelimit",
    "ratelimit-policy",
    "x-ratelimit-limit",
    "x-ratelimit-remaining",
    "location",
];

/// What a request needs of its relay session: the target, the upstream
/// session id, the legacy stream, the saved credentials.
type SessionFacts = (
    Target,
    Option<String>,
    Option<Arc<Mutex<Legacy>>>,
    Vec<(String, String)>,
);

pub struct RelaySession {
    target: Target,
    legacy: Option<Arc<Mutex<Legacy>>>,
    upstream_session: Option<String>,
    /// Credentials from a saved connection, for the session's life.
    extra: Vec<(String, String)>,
    last_used: Instant,
}

#[derive(Default)]
pub struct Relay {
    sessions: StdMutex<HashMap<String, RelaySession>>,
}

fn new_id() -> String {
    let mut b = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Relay {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, RelaySession>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn prune(map: &mut HashMap<String, RelaySession>) {
        map.retain(|_, s| s.last_used.elapsed() < IDLE);
    }

    /// Store a session; the least recently used one goes when full.
    fn insert(&self, id: String, s: RelaySession) {
        let mut map = self.lock();
        Self::prune(&mut map);
        while map.len() >= MAX_SESSIONS {
            let Some(oldest) = map
                .iter()
                .min_by_key(|(_, s)| s.last_used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            map.remove(&oldest);
        }
        map.insert(id, s);
    }

    pub fn len(&self) -> usize {
        let mut map = self.lock();
        Self::prune(&mut map);
        map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Insert an empty session (tests of the bounds).
    #[cfg(test)]
    pub fn insert_for_tests(&self, id: String) {
        self.insert(
            id,
            RelaySession {
                target: Target::Local("/mcp".into()),
                legacy: None,
                upstream_session: None,
                extra: Vec::new(),
                last_used: Instant::now(),
            },
        );
    }

    fn take(&self, id: &str) -> Option<RelaySession> {
        self.lock().remove(id)
    }

    /// Facts about a session (and mark it used).
    fn touch(&self, id: &str) -> Option<SessionFacts> {
        let mut map = self.lock();
        Self::prune(&mut map);
        let s = map.get_mut(id)?;
        s.last_used = Instant::now();
        Some((
            s.target.clone(),
            s.upstream_session.clone(),
            s.legacy.clone(),
            s.extra.clone(),
        ))
    }

    fn set_upstream(&self, id: &str, sid: &str) {
        if let Some(s) = self.lock().get_mut(id) {
            s.upstream_session = Some(sid.to_string());
        }
    }
}

fn error(status: StatusCode, message: impl Into<String>, reason: &str) -> Response {
    (
        status,
        Json(json!({ "error": message.into(), "reason": reason })),
    )
        .into_response()
}

fn frame(event: &str, data: &Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

fn sse(stream: impl futures_util::Stream<Item = Bytes> + Send + 'static) -> Response {
    let mut resp = Response::new(Body::from_stream(stream.map(Ok::<_, Infallible>)));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    resp
}

fn shown(headers: &[(String, String)]) -> Value {
    let mut m = serde_json::Map::new();
    for (k, v) in headers {
        let lk = k.to_ascii_lowercase();
        if SHOWN_HEADERS.contains(&lk.as_str()) || lk.starts_with("x-rustybin-mcp-") {
            m.insert(lk, json!(v));
        }
    }
    Value::Object(m)
}

fn audit(event: &str, target: &Target, outcome: &str) {
    let shown = match target {
        Target::Local(p) => p.split('?').next().unwrap_or("").to_string(),
        Target::Remote(u) => reqwest::Url::parse(u)
            .ok()
            .map(|u| format!("{}://{}", u.scheme(), u.host_str().unwrap_or("")))
            .unwrap_or_default(),
    };
    tracing::info!(target: "rustybin::mcp_inspector", event, target = %shown, outcome, "mcp relay");
}

/// Request headers from the body: a map or a list of pairs, validated.
pub fn header_list(v: &Value) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    match v {
        Value::Null => {}
        Value::Object(m) => {
            for (k, v) in m {
                let Some(s) = v.as_str() else {
                    return Err(format!("header {k:?}: the value must be a string"));
                };
                out.push((k.clone(), s.to_string()));
            }
        }
        Value::Array(items) => {
            for item in items {
                let (Some(k), Some(v)) = (
                    item.get(0).and_then(Value::as_str),
                    item.get(1).and_then(Value::as_str),
                ) else {
                    return Err("headers: a list of [name, value] pairs".into());
                };
                out.push((k.to_string(), v.to_string()));
            }
        }
        _ => return Err("headers: an object or a list of [name, value] pairs".into()),
    }
    if out.len() > 64 {
        return Err("at most 64 headers".into());
    }
    for (k, v) in &out {
        if axum::http::HeaderName::from_bytes(k.as_bytes()).is_err()
            || HeaderValue::from_str(v).is_err()
        {
            return Err(format!("invalid header {k:?}"));
        }
    }
    Ok(out)
}

/// A saved connection's auth (headers and query) when the request names one.
pub fn connection_auth(
    inspector: &Inspector,
    id: Option<&str>,
) -> Result<(super::store::Pairs, super::store::Pairs), Box<Response>> {
    let Some(id) = id else {
        return Ok((Vec::new(), Vec::new()));
    };
    let Some((conn, secrets)) = inspector.store.resolve(id) else {
        return Err(Box::new(error(
            StatusCode::NOT_FOUND,
            format!("no saved connection {id:?}"),
            "unknown_connection",
        )));
    };
    Ok(super::store::build_auth(&conn.auth, &secrets))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RelayBody {
    #[serde(default)]
    pub target: String,
    /// `streamable` (default) or `legacy`.
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub headers: Value,
    #[serde(default)]
    pub message: Option<Value>,
    /// `delete`: end the upstream session.
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub connection: Option<String>,
}

pub async fn relay(
    State(state): State<AppState>,
    Extension(inspector): Extension<Arc<Inspector>>,
    body: Bytes,
) -> Response {
    if body.len() > 1024 * 1024 {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the relay body is larger than 1 MiB",
            "too_large",
        );
    }
    let req: RelayBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!(
                "expected {{target, message, headers?, session?, transport?, connection?}}: {e}"
            ),
                "invalid_body",
            )
        }
    };
    let mut headers = match header_list(&req.headers) {
        Ok(h) => h,
        Err(e) => return error(StatusCode::BAD_REQUEST, e, "invalid_headers"),
    };
    let legacy = matches!(req.transport.as_deref(), Some("legacy") | Some("sse"));
    // An existing session decides the target; otherwise the body does.
    let existing = match &req.session {
        Some(id) => match inspector.relay.touch(id) {
            Some(s) => Some((id.clone(), s)),
            None => {
                return error(
                    StatusCode::NOT_FOUND,
                    "this relay session ended (idle for 10 minutes, or deleted): connect again",
                    "unknown_session",
                )
            }
        },
        None => None,
    };
    let (target, extra) = match &existing {
        Some((_, (t, _, _, extra))) => (t.clone(), extra.clone()),
        None => {
            let base = match Target::parse(&req.target, false) {
                Ok(t) => t,
                Err(e) => return error(StatusCode::BAD_REQUEST, e, "invalid_target"),
            };
            let (h, q) = match connection_auth(&inspector, req.connection.as_deref()) {
                Ok(a) => a,
                Err(resp) => return *resp,
            };
            (base.with_query(&q), h)
        }
    };
    // Saved credentials fill in what the browser did not send.
    for (k, v) in &extra {
        if !headers.iter().any(|(hk, _)| hk.eq_ignore_ascii_case(k)) {
            headers.push((k.clone(), v.clone()));
        }
    }
    if req.op.as_deref() == Some("delete") {
        let id = existing.as_ref().map(|(id, _)| id.clone());
        return match id {
            Some(id) => end_session(&state, &inspector, &id).await,
            None => error(
                StatusCode::BAD_REQUEST,
                "op delete needs a session",
                "invalid_body",
            ),
        };
    }
    let Some(message) = req.message.clone() else {
        return error(
            StatusCode::BAD_REQUEST,
            "message (one JSON-RPC message) is required",
            "invalid_body",
        );
    };
    if !message.is_object() {
        return error(
            StatusCode::BAD_REQUEST,
            "message must be one JSON-RPC object",
            "invalid_body",
        );
    }
    if legacy {
        return relay_legacy(state, inspector, target, headers, message, existing, extra).await;
    }
    // Streamable HTTP: the stored upstream session unless the browser sends one.
    if let Some((_, (_, Some(sid), _, _))) = &existing {
        if !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("mcp-session-id"))
        {
            headers.push(("mcp-session-id".into(), sid.clone()));
        }
    }
    if !headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        headers.push(("content-type".into(), "application/json".into()));
    }
    if !headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("accept"))
    {
        headers.push((
            "accept".into(),
            "application/json, text/event-stream".into(),
        ));
    }
    let resp = match client::send(
        &state,
        &target,
        "POST",
        &headers,
        Some(message.to_string().into_bytes()),
        client::TIMEOUT,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            audit("post", &target, "refused");
            let refused = e.starts_with("outbound call refused");
            return error(
                if refused {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::BAD_GATEWAY
                },
                e,
                if refused {
                    "egress_refused"
                } else {
                    "upstream_error"
                },
            );
        }
    };
    audit("post", &target, "ok");
    // A session for the browser once the server hands out one.
    let session_id = match (&existing, resp.header("mcp-session-id")) {
        (Some((id, _)), Some(sid)) => {
            inspector.relay.set_upstream(id, sid);
            Some(id.clone())
        }
        (Some((id, _)), None) => Some(id.clone()),
        (None, Some(sid)) => {
            let id = new_id();
            inspector.relay.insert(
                id.clone(),
                RelaySession {
                    target: target.clone(),
                    legacy: None,
                    upstream_session: Some(sid.to_string()),
                    extra,
                    last_used: Instant::now(),
                },
            );
            Some(id)
        }
        (None, None) => None,
    };
    let meta =
        json!({ "session": session_id, "status": resp.status, "headers": shown(&resp.headers) });
    let is_sse = resp.is_sse();
    let id = message.get("id").cloned();
    let stream = async_stream::stream! {
        yield frame("relay", &meta);
        if is_sse {
            let mut events = Box::pin(client::sse_events(resp.body));
            let mut n = 0;
            loop {
                let next = tokio::time::timeout(client::TIMEOUT, events.next()).await;
                let Ok(Some(ev)) = next else { break };
                let Ok(ev) = ev else { break };
                let Ok(v) = serde_json::from_str::<Value>(&ev.data) else { continue };
                let done = id.is_some() && v.get("id") == id.as_ref() && (v.get("result").is_some() || v.get("error").is_some());
                yield frame("message", &v);
                n += 1;
                if done || n >= client::MAX_MESSAGES {
                    break;
                }
            }
        } else {
            match client::read_all(resp.body, client::MAX_ANSWER_BYTES).await {
                Ok(bytes) => match client::messages_of_json(&bytes) {
                    Some(msgs) if msgs.iter().any(client::is_jsonrpc) => {
                        for m in msgs {
                            yield frame("message", &m);
                        }
                    }
                    _ => {
                        if !bytes.is_empty() {
                            let text: String = String::from_utf8_lossy(&bytes).chars().take(16 * 1024).collect();
                            yield frame("body", &json!({ "text": text }));
                        }
                    }
                },
                Err(e) => yield frame("error", &json!({ "error": e })),
            }
        }
        yield frame("done", &json!({}));
    };
    sse(stream)
}

async fn relay_legacy(
    state: AppState,
    inspector: Arc<Inspector>,
    target: Target,
    headers: Vec<(String, String)>,
    message: Value,
    existing: Option<(String, SessionFacts)>,
    extra: Vec<(String, String)>,
) -> Response {
    let (session_id, legacy) = match existing.and_then(|(id, (_, _, l, _))| l.map(|l| (id, l))) {
        Some(found) => found,
        None => {
            let opened = match Legacy::open(&state, &target, &headers).await {
                Ok(l) => Arc::new(Mutex::new(l)),
                Err(e) => {
                    audit("open", &target, "refused");
                    let refused = e.starts_with("outbound call refused");
                    return error(
                        if refused {
                            StatusCode::FORBIDDEN
                        } else {
                            StatusCode::BAD_GATEWAY
                        },
                        e,
                        if refused {
                            "egress_refused"
                        } else {
                            "upstream_error"
                        },
                    );
                }
            };
            let id = new_id();
            inspector.relay.insert(
                id.clone(),
                RelaySession {
                    target: target.clone(),
                    legacy: Some(opened.clone()),
                    upstream_session: None,
                    extra,
                    last_used: Instant::now(),
                },
            );
            audit("open", &target, "ok");
            (id, opened)
        }
    };
    let id = message.get("id").cloned();
    let expects_answer = message.get("method").is_some() && id.is_some();
    let status = {
        let l = legacy.lock().await;
        match l.post(&state, &headers, &message).await {
            Ok(s) => s,
            Err(e) => return error(StatusCode::BAD_GATEWAY, e, "upstream_error"),
        }
    };
    let meta =
        json!({ "session": session_id, "status": status, "headers": {}, "transport": "legacy" });
    let stream = async_stream::stream! {
        yield frame("relay", &meta);
        if expects_answer && status < 400 {
            let deadline = Instant::now() + client::TIMEOUT;
            let mut l = legacy.lock().await;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                let Some(m) = l.next(left).await else {
                    yield frame("error", &json!({ "error": "no answer on the stream within 30 s" }));
                    break;
                };
                let done = m.get("id") == id.as_ref() && (m.get("result").is_some() || m.get("error").is_some());
                yield frame("message", &m);
                if done {
                    break;
                }
            }
        }
        yield frame("done", &json!({}));
    };
    sse(stream)
}

async fn end_session(state: &AppState, inspector: &Inspector, id: &str) -> Response {
    let Some(s) = inspector.relay.take(id) else {
        return error(
            StatusCode::NOT_FOUND,
            "no such relay session",
            "unknown_session",
        );
    };
    let mut upstream = Value::Null;
    if let Some(sid) = &s.upstream_session {
        let mut hs = s.extra.clone();
        hs.push(("mcp-session-id".into(), sid.clone()));
        if let Ok(r) = client::send(state, &s.target, "DELETE", &hs, None, client::TIMEOUT).await {
            upstream = json!(r.status);
        }
    }
    audit("delete", &s.target, "ok");
    Json(json!({ "ended": true, "session": id, "upstream_status": upstream })).into_response()
}

pub async fn delete_session(
    State(state): State<AppState>,
    Extension(inspector): Extension<Arc<Inspector>>,
    Path(id): Path<String>,
) -> Response {
    end_session(&state, &inspector, &id).await
}

// ── The fetch helper (OAuth discovery for servers the browser cannot reach) ──

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FetchBody {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub headers: Value,
    #[serde(default)]
    pub body: Option<String>,
}

pub async fn fetch(State(state): State<AppState>, headers_in: HeaderMap, body: Bytes) -> Response {
    let _ = headers_in;
    let req: FetchBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!("expected {{url, method?, headers?, body?}}: {e}"),
                "invalid_body",
            )
        }
    };
    let method = req.method.as_deref().unwrap_or("GET").to_ascii_uppercase();
    if method != "GET" && method != "POST" {
        return error(
            StatusCode::BAD_REQUEST,
            "method must be GET or POST",
            "invalid_body",
        );
    }
    let target = match Target::parse(&req.url, true) {
        Ok(t) => t,
        Err(e) => return error(StatusCode::BAD_REQUEST, e, "invalid_target"),
    };
    let headers = match header_list(&req.headers) {
        Ok(h) => h,
        Err(e) => return error(StatusCode::BAD_REQUEST, e, "invalid_headers"),
    };
    if req.body.as_ref().is_some_and(|b| b.len() > 64 * 1024) {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the body is larger than 64 KiB",
            "too_large",
        );
    }
    let resp = match client::send(
        &state,
        &target,
        &method,
        &headers,
        req.body.map(String::into_bytes),
        client::TIMEOUT,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            audit("fetch", &target, "refused");
            let refused = e.starts_with("outbound call refused");
            return error(
                if refused {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::BAD_GATEWAY
                },
                e,
                if refused {
                    "egress_refused"
                } else {
                    "upstream_error"
                },
            );
        }
    };
    audit("fetch", &target, "ok");
    let status = resp.status;
    let hs: BTreeMap<String, String> = resp
        .headers
        .iter()
        .filter(|(k, _)| {
            matches!(
                k.to_ascii_lowercase().as_str(),
                "content-type" | "www-authenticate" | "location" | "cache-control"
            )
        })
        .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
        .collect();
    let bytes = match client::read_all(resp.body, MAX_FETCH_BYTES).await {
        Ok(b) => b,
        Err(e) => return error(StatusCode::BAD_GATEWAY, e, "upstream_error"),
    };
    let text = String::from_utf8_lossy(&bytes).to_string();
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    Json(json!({ "status": status, "headers": hs, "body": text, "json": parsed })).into_response()
}
