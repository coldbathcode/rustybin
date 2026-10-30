//! The MCP client the relay and the checks share: where a target is (a path
//! on this instance, served in process, or a URL reached through the egress
//! policy under `Purpose::McpRelay`), one HTTP exchange with a streamed body,
//! the JSON-RPC messages of an answer (JSON or SSE), the legacy HTTP+SSE
//! transport, and a small session-aware client for the checks.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderName, HeaderValue, Request};
use base64::Engine;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use tower::ServiceExt;

use crate::assist::provider::{SseEvent, SseParser};
use crate::egress::Purpose;
use crate::state::AppState;

/// Time allowed for the response headers of one exchange.
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// Life of a legacy SSE stream the relay or a check holds open.
pub const STREAM_TIMEOUT: Duration = Duration::from_secs(600);
/// Largest answer read (JSON body, or the whole SSE stream of one request).
pub const MAX_ANSWER_BYTES: usize = 4 * 1024 * 1024;
/// Most messages one answer may carry.
pub const MAX_MESSAGES: usize = 500;
/// Messages buffered from a legacy stream.
const LEGACY_BUFFER: usize = 256;

/// The protocol versions the client speaks.
pub const MODERN: &str = "2026-07-28";
pub const SESSION_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];
pub const LEGACY_VERSION: &str = "2024-11-05";

pub type BodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, String>> + Send>>;

/// Where an MCP server is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A path on this instance (with its query), served in process.
    Local(String),
    /// An absolute `http(s)` URL, reached through the egress policy.
    Remote(String),
}

/// Paths on this instance the inspector may reach: the MCP servers, the
/// legacy SSE pair, and (for OAuth discovery) the metadata documents and
/// the identity provider.
pub fn local_allowed(path: &str, for_fetch: bool) -> bool {
    let p = path.split('?').next().unwrap_or("");
    if p == "/mcp" || p.starts_with("/mcp/") || p == crate::mcp::manage::MANAGE_PATH {
        return true;
    }
    if let Some(rest) = p.strip_prefix("/mock/") {
        if let Some((name, tail)) = rest.split_once('/') {
            if crate::mocks::model::valid_name(name) && tail == "_rustybin/mcp" {
                return true;
            }
        }
    }
    for_fetch
        && (p.starts_with("/.well-known/")
            || p.starts_with("/oauth/")
            || p.starts_with("/_rustybin/oauth/"))
}

impl Target {
    /// `/path` (this instance) or an `http(s)://` URL.
    pub fn parse(raw: &str, for_fetch: bool) -> Result<Target, String> {
        let raw = raw.trim();
        if raw.starts_with('/') && !raw.starts_with("//") {
            if raw.len() > 2048 {
                return Err("the path is longer than 2048 characters".into());
            }
            if !local_allowed(raw, for_fetch) {
                return Err(format!(
                    "{raw:?} is not an MCP server of this instance (the inspector reaches /mcp*, /_rustybin/mcp and /mock/<name>/_rustybin/mcp{})",
                    if for_fetch { ", and the OAuth documents" } else { "" }
                ));
            }
            return Ok(Target::Local(raw.to_string()));
        }
        let url = reqwest::Url::parse(raw).map_err(|e| format!("invalid URL {raw:?}: {e}"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(
                "the target must be an http:// or https:// URL, or a path on this instance".into(),
            );
        }
        Ok(Target::Remote(url.to_string()))
    }

    /// The target as text (paths stay paths).
    pub fn as_str(&self) -> &str {
        match self {
            Target::Local(p) | Target::Remote(p) => p,
        }
    }

    /// Resolve a URL the server handed back (the legacy `endpoint` event,
    /// a metadata document's link) against this target.
    pub fn join(&self, reference: &str) -> Result<Target, String> {
        match self {
            Target::Local(_) => {
                if reference.starts_with('/') {
                    Target::parse(reference, true)
                } else {
                    Target::parse(reference, true).map_err(|_| {
                        format!(
                            "{reference:?} leaves this instance; use the full URL as the target"
                        )
                    })
                }
            }
            Target::Remote(base) => {
                let base = reqwest::Url::parse(base).map_err(|e| e.to_string())?;
                let joined = base.join(reference).map_err(|e| e.to_string())?;
                Target::parse(joined.as_str(), true)
            }
        }
    }

    /// Append query parameters (API keys sent as a query).
    pub fn with_query(&self, query: &[(String, String)]) -> Target {
        if query.is_empty() {
            return self.clone();
        }
        let encoded: String = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(query.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .finish();
        let raw = self.as_str();
        let sep = if raw.contains('?') { '&' } else { '?' };
        let next = format!("{raw}{sep}{encoded}");
        match self {
            Target::Local(_) => Target::Local(next),
            Target::Remote(_) => Target::Remote(next),
        }
    }
}

/// One HTTP answer with its body still streaming.
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: BodyStream,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn is_sse(&self) -> bool {
        self.header("content-type")
            .is_some_and(|c| c.contains("text/event-stream"))
    }
}

/// Headers never forwarded (hop-by-hop, framing, the host).
fn skip_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "upgrade"
            | "keep-alive"
            | "te"
            | "trailer"
            | "proxy-authorization"
            | "proxy-connection"
    )
}

/// Send one HTTP request to a target.
pub async fn send(
    state: &AppState,
    target: &Target,
    method: &str,
    headers: &[(String, String)],
    body: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<HttpResponse, String> {
    match target {
        Target::Local(path) => send_local(state, path, method, headers, body, timeout).await,
        Target::Remote(url) => send_remote(state, url, method, headers, body, timeout).await,
    }
}

pub(crate) async fn send_local(
    state: &AppState,
    path: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<HttpResponse, String> {
    let Some(router) = state.router.get().cloned() else {
        return Err("in-process requests are not available (no application router)".into());
    };
    let mut b = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        if skip_header(name) {
            continue;
        }
        let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) else {
            return Err(format!("invalid header {name:?}"));
        };
        b = b.header(n, v);
    }
    let mut req = b
        .body(body.map(Body::from).unwrap_or_else(Body::empty))
        .map_err(|e| format!("invalid request: {e}"))?;
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))));
    let resp = match tokio::time::timeout(timeout, router.oneshot(req)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return Err(format!("the request failed: {e}")),
        Err(_) => return Err(format!("no answer within {} s", timeout.as_secs())),
    };
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let stream = resp
        .into_body()
        .into_data_stream()
        .map(|r| r.map_err(|e| e.to_string()));
    Ok(HttpResponse {
        status,
        headers,
        body: Box::pin(stream),
    })
}

async fn send_remote(
    state: &AppState,
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<HttpResponse, String> {
    let pinned = state
        .egress
        .resolve(url, Purpose::McpRelay)
        .await
        .map_err(|e| format!("outbound call refused: {e}"))?;
    let client = state.egress.client(&pinned, timeout)?;
    let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let mut req = client.request(m, &pinned.url);
    for (name, value) in headers {
        if skip_header(name) {
            continue;
        }
        req = req.header(name.as_str(), value.as_str());
    }
    if let Some(b) = body {
        req = req.body(b);
    }
    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            "the server did not answer in time".to_string()
        } else if e.is_connect() {
            "could not connect to the server".to_string()
        } else {
            let mut t = e.to_string();
            if let Some(i) = t.find(" for url") {
                t.truncate(i);
            }
            t
        }
    })?;
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let stream = resp.bytes_stream().map(|r| r.map_err(|e| e.to_string()));
    Ok(HttpResponse {
        status,
        headers,
        body: Box::pin(stream),
    })
}

/// Read a whole body, at most `cap` bytes.
pub async fn read_all(mut body: BodyStream, cap: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        out.extend_from_slice(&chunk);
        if out.len() > cap {
            return Err(format!("the answer exceeded {cap} bytes"));
        }
    }
    Ok(out)
}

/// The JSON-RPC messages of a JSON body (one object or a batch).
pub fn messages_of_json(bytes: &[u8]) -> Option<Vec<Value>> {
    match serde_json::from_slice::<Value>(bytes).ok()? {
        Value::Array(items) => Some(items.into_iter().filter(Value::is_object).collect()),
        v @ Value::Object(_) => Some(vec![v]),
        _ => None,
    }
}

/// Is this a JSON-RPC message (request, notification or response)?
pub fn is_jsonrpc(v: &Value) -> bool {
    v.get("jsonrpc").is_some()
        || v.get("method").is_some()
        || v.get("result").is_some()
        || v.get("error").is_some()
}

/// A stream of SSE events from a body.
pub fn sse_events(body: BodyStream) -> impl Stream<Item = Result<SseEvent, String>> {
    async_stream::stream! {
        let mut parser = SseParser::default();
        let mut body = body;
        let mut total = 0usize;
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(bytes) => {
                    total += bytes.len();
                    if total > MAX_ANSWER_BYTES * 4 {
                        yield Err("the stream exceeded its size cap".to_string());
                        return;
                    }
                    for ev in parser.feed(&bytes) {
                        yield Ok(ev);
                    }
                }
                Err(e) => {
                    yield Err(e);
                    return;
                }
            }
        }
        if let Some(ev) = parser.finish() {
            yield Ok(ev);
        }
    }
}

/// `Mcp-Name` and friends accept ASCII only: other values are sent base64
/// encoded, as the console does.
pub fn header_safe(v: &str) -> String {
    let printable = !v.is_empty()
        && v.bytes().all(|b| (0x20..=0x7e).contains(&b))
        && !v.starts_with(' ')
        && !v.ends_with(' ');
    if printable {
        v.to_string()
    } else {
        format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(v.as_bytes())
        )
    }
}

// ── The legacy HTTP+SSE transport ───────────────────────────────────

/// An open legacy stream: where to POST, and the messages it delivered.
pub struct Legacy {
    pub endpoint: Target,
    rx: mpsc::Receiver<Value>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Legacy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Legacy {
    /// Open the stream (`GET` the SSE URL) and wait for its `endpoint` event.
    pub async fn open(
        state: &AppState,
        target: &Target,
        headers: &[(String, String)],
    ) -> Result<Legacy, String> {
        let mut hs: Vec<(String, String)> = headers.to_vec();
        hs.push(("accept".into(), "text/event-stream".into()));
        let resp = send(state, target, "GET", &hs, None, STREAM_TIMEOUT).await?;
        if resp.status >= 400 {
            let status = resp.status;
            let body = read_all(resp.body, 16 * 1024).await.unwrap_or_default();
            return Err(format!(
                "the SSE stream answered {status}: {}",
                String::from_utf8_lossy(&body)
                    .chars()
                    .take(300)
                    .collect::<String>()
            ));
        }
        if !resp.is_sse() {
            return Err(format!(
                "the SSE URL answered {} with {} instead of text/event-stream: is this a legacy (2024-11-05) server?",
                resp.status,
                resp.header("content-type").unwrap_or("no content type")
            ));
        }
        let mut events = Box::pin(sse_events(resp.body));
        let endpoint = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(ev) = events.next().await {
                let ev = ev?;
                if ev.event == "endpoint" {
                    return Ok(ev.data.trim().to_string());
                }
            }
            Err("the stream ended before its endpoint event".to_string())
        })
        .await
        .map_err(|_| "no endpoint event within 10 s".to_string())??;
        let endpoint = target.join(&endpoint)?;
        let (tx, rx) = mpsc::channel(LEGACY_BUFFER);
        let task = tokio::spawn(async move {
            while let Some(ev) = events.next().await {
                let Ok(ev) = ev else { break };
                if ev.event != "message" && !ev.event.is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
                    continue;
                };
                if tx.send(v).await.is_err() {
                    break;
                }
            }
        });
        Ok(Legacy { endpoint, rx, task })
    }

    /// POST a message to the endpoint; returns the HTTP status.
    pub async fn post(
        &self,
        state: &AppState,
        headers: &[(String, String)],
        message: &Value,
    ) -> Result<u16, String> {
        let mut hs: Vec<(String, String)> = headers.to_vec();
        hs.push(("content-type".into(), "application/json".into()));
        let resp = send(
            state,
            &self.endpoint,
            "POST",
            &hs,
            Some(message.to_string().into_bytes()),
            TIMEOUT,
        )
        .await?;
        let status = resp.status;
        let _ = read_all(resp.body, 64 * 1024).await;
        Ok(status)
    }

    /// The next message from the stream (`None` after `wait` or when the
    /// stream ended).
    pub async fn next(&mut self, wait: Duration) -> Option<Value> {
        tokio::time::timeout(wait, self.rx.recv())
            .await
            .ok()
            .flatten()
    }
}

// ── A client for the checks ─────────────────────────────────────────

/// Which transport a connection speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Streamable HTTP, 2026-07-28 stateless.
    Modern,
    /// Streamable HTTP with sessions (the version).
    Session(&'static str),
    /// The legacy HTTP+SSE pair (2024-11-05).
    Legacy,
}

impl Transport {
    /// `modern` (default), `session` with a version, `legacy`.
    pub fn parse(kind: Option<&str>, version: Option<&str>) -> Result<Transport, String> {
        match kind.unwrap_or("modern") {
            "modern" | "stateless" | "streamable" if version.is_none_or(|v| v == MODERN) => {
                Ok(Transport::Modern)
            }
            "modern" | "stateless" | "streamable" | "session" => {
                let v = version.unwrap_or(SESSION_VERSIONS[0]);
                SESSION_VERSIONS
                    .iter()
                    .find(|x| **x == v)
                    .map(|x| Transport::Session(x))
                    .ok_or_else(|| {
                        format!(
                            "version {v:?}: the session transport speaks {}",
                            SESSION_VERSIONS.join(", ")
                        )
                    })
            }
            "legacy" | "sse" => Ok(Transport::Legacy),
            other => Err(format!(
                "transport {other:?}: expected modern, session or legacy"
            )),
        }
    }

    pub fn version(&self) -> &'static str {
        match self {
            Transport::Modern => MODERN,
            Transport::Session(v) => v,
            Transport::Legacy => LEGACY_VERSION,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Transport::Modern => "modern",
            Transport::Session(_) => "session",
            Transport::Legacy => "legacy",
        }
    }
}

/// The outcome of one request.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub status: u16,
    pub result: Option<Value>,
    pub error: Option<Value>,
    /// The exchange failed below JSON-RPC (network, refusal, no answer).
    pub transport_error: Option<String>,
    /// A non-JSON-RPC body (an HTTP error page), truncated.
    pub body: Option<String>,
    pub headers: Vec<(String, String)>,
    pub ms: u64,
    /// Notifications that arrived with the answer.
    pub notifications: Vec<Value>,
}

impl Outcome {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A connection to one MCP server.
pub struct Conn {
    state: AppState,
    pub target: Target,
    pub transport: Transport,
    /// Credentials (and any custom headers) sent on every request.
    pub auth: Vec<(String, String)>,
    pub session_id: Option<String>,
    legacy: Option<Arc<Mutex<Legacy>>>,
    seq: u64,
    initialized: bool,
    /// Input schemas by tool name (for the `Mcp-Param-*` headers).
    pub tool_schemas: HashMap<String, Value>,
}

impl Conn {
    pub fn new(
        state: AppState,
        target: Target,
        transport: Transport,
        auth: Vec<(String, String)>,
    ) -> Self {
        Self {
            state,
            target,
            transport,
            auth,
            session_id: None,
            legacy: None,
            seq: 0,
            initialized: false,
            tool_schemas: HashMap::new(),
        }
    }

    fn headers(&self, method: Option<&str>, params: &Value) -> Vec<(String, String)> {
        let mut hs = vec![
            ("content-type".to_string(), "application/json".to_string()),
            (
                "accept".to_string(),
                "application/json, text/event-stream".to_string(),
            ),
        ];
        hs.extend(self.auth.iter().cloned());
        match self.transport {
            Transport::Modern => {
                hs.push(("mcp-protocol-version".into(), MODERN.into()));
                if let Some(m) = method {
                    hs.push(("mcp-method".into(), m.into()));
                    let name = match m {
                        "tools/call" | "prompts/get" => params.get("name").and_then(Value::as_str),
                        "resources/read" => params.get("uri").and_then(Value::as_str),
                        _ => None,
                    };
                    if let Some(n) = name {
                        hs.push(("mcp-name".into(), header_safe(n)));
                    }
                    if m == "tools/call" {
                        if let Some(props) = params
                            .get("name")
                            .and_then(Value::as_str)
                            .and_then(|n| self.tool_schemas.get(n))
                            .and_then(|s| s.get("properties"))
                            .and_then(Value::as_object)
                        {
                            for (prop, spec) in props {
                                let Some(token) = spec.get("x-mcp-header").and_then(Value::as_str)
                                else {
                                    continue;
                                };
                                let v = params.get("arguments").and_then(|a| a.get(prop));
                                let text = match v {
                                    Some(Value::String(s)) => Some(s.clone()),
                                    Some(Value::Number(n)) => Some(n.to_string()),
                                    Some(Value::Bool(b)) => Some(b.to_string()),
                                    _ => None,
                                };
                                if let Some(t) = text {
                                    hs.push((format!("mcp-param-{token}"), header_safe(&t)));
                                }
                            }
                        }
                    }
                }
            }
            Transport::Session(v) => {
                if let Some(sid) = &self.session_id {
                    hs.push(("mcp-session-id".into(), sid.clone()));
                }
                if self.initialized && v != "2025-03-26" {
                    hs.push(("mcp-protocol-version".into(), v.into()));
                }
            }
            Transport::Legacy => {}
        }
        hs
    }

    fn with_meta(&self, params: &Value) -> Value {
        let mut p = match params {
            Value::Object(m) => Value::Object(m.clone()),
            _ => json!({}),
        };
        if self.transport == Transport::Modern {
            p["_meta"] = json!({
                "io.modelcontextprotocol/protocolVersion": MODERN,
                "io.modelcontextprotocol/clientCapabilities": {},
                "io.modelcontextprotocol/clientInfo": { "name": "rustybin-mcp-checks", "version": env!("CARGO_PKG_VERSION") },
            });
        }
        p
    }

    /// Send a request and wait for its answer.
    pub async fn request(&mut self, method: &str, params: Value) -> Outcome {
        self.seq += 1;
        let id = self.seq;
        let params = self.with_meta(&params);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let started = Instant::now();
        let mut out = if self.transport == Transport::Legacy {
            self.legacy_request(id, &msg).await
        } else {
            let headers = self.headers(Some(method), &params);
            self.http_request(id, &msg, &headers).await
        };
        out.ms = started.elapsed().as_millis() as u64;
        out
    }

    /// Send a raw message with explicit headers (the bad-header checks).
    pub async fn raw(&mut self, msg: &Value, headers: Vec<(String, String)>) -> Outcome {
        let started = Instant::now();
        let id = msg.get("id").and_then(Value::as_u64).unwrap_or(0);
        let mut out = self.http_request(id, msg, &headers).await;
        out.ms = started.elapsed().as_millis() as u64;
        out
    }

    /// The headers a request would carry (the raw checks start from them).
    pub fn request_headers(&self, method: &str, params: &Value) -> Vec<(String, String)> {
        self.headers(Some(method), params)
    }

    async fn http_request(
        &mut self,
        id: u64,
        msg: &Value,
        headers: &[(String, String)],
    ) -> Outcome {
        let mut out = Outcome::default();
        let resp = match send(
            &self.state,
            &self.target,
            "POST",
            headers,
            Some(msg.to_string().into_bytes()),
            TIMEOUT,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                out.transport_error = Some(e);
                return out;
            }
        };
        out.status = resp.status;
        out.headers = resp.headers.clone();
        if let Some(sid) = resp.header("mcp-session-id") {
            self.session_id = Some(sid.to_string());
        }
        let messages = if resp.is_sse() {
            let mut events = Box::pin(sse_events(resp.body));
            let mut msgs = Vec::new();
            loop {
                let next = tokio::time::timeout(TIMEOUT, events.next()).await;
                let Ok(Some(ev)) = next else { break };
                let Ok(ev) = ev else { break };
                let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
                    continue;
                };
                let done = v.get("id").and_then(Value::as_u64) == Some(id)
                    && (v.get("result").is_some() || v.get("error").is_some());
                // A request from the server (elicitation, sampling): decline it.
                if v.get("method").is_some() && v.get("id").is_some() {
                    let reply = json!({ "jsonrpc": "2.0", "id": v["id"], "error": { "code": -32601, "message": "the checks do not answer server requests" } });
                    let mut hs = self.headers(None, &json!({}));
                    hs.retain(|(k, _)| k != "mcp-method" && k != "mcp-name");
                    let _ = send(
                        &self.state,
                        &self.target,
                        "POST",
                        &hs,
                        Some(reply.to_string().into_bytes()),
                        TIMEOUT,
                    )
                    .await;
                }
                msgs.push(v);
                if done || msgs.len() >= MAX_MESSAGES {
                    break;
                }
            }
            msgs
        } else {
            match read_all(resp.body, MAX_ANSWER_BYTES).await {
                Ok(bytes) => match messages_of_json(&bytes) {
                    Some(m) if m.iter().any(is_jsonrpc) => m,
                    _ => {
                        if !bytes.is_empty() {
                            out.body =
                                Some(String::from_utf8_lossy(&bytes).chars().take(2000).collect());
                        }
                        Vec::new()
                    }
                },
                Err(e) => {
                    out.transport_error = Some(e);
                    Vec::new()
                }
            }
        };
        for m in messages {
            let is_answer = (m.get("id").and_then(Value::as_u64) == Some(id)
                || m.get("id").is_some_and(Value::is_null))
                && (m.get("result").is_some() || m.get("error").is_some());
            if is_answer {
                out.result = m.get("result").cloned();
                out.error = m.get("error").cloned();
            } else if m.get("method").is_some() && m.get("id").is_none() {
                out.notifications.push(m);
            }
        }
        if out.result.is_none()
            && out.error.is_none()
            && out.transport_error.is_none()
            && out.body.is_none()
            && out.status >= 400
        {
            out.transport_error = Some(format!("HTTP {}", out.status));
        }
        out
    }

    async fn legacy_request(&mut self, id: u64, msg: &Value) -> Outcome {
        let mut out = Outcome::default();
        if self.legacy.is_none() {
            match Legacy::open(&self.state, &self.target, &self.auth).await {
                Ok(l) => self.legacy = Some(Arc::new(Mutex::new(l))),
                Err(e) => {
                    out.transport_error = Some(e);
                    return out;
                }
            }
        }
        let Some(legacy) = self.legacy.clone() else {
            out.transport_error = Some("no stream".into());
            return out;
        };
        let mut l = legacy.lock().await;
        match l.post(&self.state, &self.auth, msg).await {
            Ok(status) => {
                out.status = status;
                if status >= 400 {
                    out.transport_error = Some(format!("the message endpoint answered {status}"));
                    return out;
                }
            }
            Err(e) => {
                out.transport_error = Some(e);
                return out;
            }
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(m) = l.next(left).await else {
                out.transport_error = Some("no answer on the stream within 30 s".into());
                return out;
            };
            if m.get("id").and_then(Value::as_u64) == Some(id)
                && (m.get("result").is_some() || m.get("error").is_some())
            {
                out.result = m.get("result").cloned();
                out.error = m.get("error").cloned();
                return out;
            }
            if m.get("method").is_some() && m.get("id").is_none() {
                out.notifications.push(m);
            }
        }
    }

    /// Send a notification.
    pub async fn notify(&mut self, method: &str, params: Option<Value>) -> u16 {
        let mut msg = json!({ "jsonrpc": "2.0", "method": method });
        if let Some(p) = params {
            msg["params"] = p;
        }
        if self.transport == Transport::Legacy {
            if let Some(l) = self.legacy.clone() {
                return l
                    .lock()
                    .await
                    .post(&self.state, &self.auth, &msg)
                    .await
                    .unwrap_or(0);
            }
            return 0;
        }
        let headers = self.headers(Some(method), &json!({}));
        match send(
            &self.state,
            &self.target,
            "POST",
            &headers,
            Some(msg.to_string().into_bytes()),
            TIMEOUT,
        )
        .await
        {
            Ok(r) => {
                let s = r.status;
                let _ = read_all(r.body, 64 * 1024).await;
                s
            }
            Err(_) => 0,
        }
    }

    /// Connect: `server/discover` (modern) or the `initialize` handshake.
    pub async fn connect(&mut self) -> Outcome {
        match self.transport {
            Transport::Modern => self.request("server/discover", json!({})).await,
            Transport::Session(_) | Transport::Legacy if !self.initialized => {
                let version = self.transport.version();
                let out = self
                    .request(
                        "initialize",
                        json!({
                            "protocolVersion": version,
                            "capabilities": {},
                            "clientInfo": { "name": "rustybin-mcp-checks", "version": env!("CARGO_PKG_VERSION") },
                        }),
                    )
                    .await;
                if out.result.is_some() {
                    self.initialized = true;
                    self.notify("notifications/initialized", None).await;
                }
                out
            }
            _ => Outcome::default(),
        }
    }

    /// End the session (`DELETE` with the session id); the status, if any.
    pub async fn close(&mut self) -> Option<u16> {
        if self.transport == Transport::Legacy {
            self.legacy = None;
            return None;
        }
        let sid = self.session_id.clone()?;
        let mut hs = self.auth.clone();
        hs.push(("mcp-session-id".into(), sid));
        let status = match send(&self.state, &self.target, "DELETE", &hs, None, TIMEOUT).await {
            Ok(r) => {
                let s = r.status;
                let _ = read_all(r.body, 64 * 1024).await;
                Some(s)
            }
            Err(_) => None,
        };
        status
    }

    /// Open the server-to-client stream of a session (`GET`); the status
    /// and content type, without reading it.
    pub async fn open_stream(&self) -> Result<(u16, String), String> {
        let mut hs = self.auth.clone();
        hs.push(("accept".into(), "text/event-stream".into()));
        if let Some(sid) = &self.session_id {
            hs.push(("mcp-session-id".into(), sid.clone()));
        }
        if let Transport::Session(v) = self.transport {
            hs.push(("mcp-protocol-version".into(), v.into()));
        }
        let r = send(
            &self.state,
            &self.target,
            "GET",
            &hs,
            None,
            Duration::from_secs(5),
        )
        .await?;
        let ct = r.header("content-type").unwrap_or("").to_string();
        Ok((r.status, ct))
    }
}
