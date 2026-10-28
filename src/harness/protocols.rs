//! Protocol sources (`docs/plans/harness.md`, phase 7): a layer whose
//! source is an LLM call (`llm`), a server-sent event stream (`sse`), a
//! WebSocket session (`websocket`), a gRPC call (`grpc`) or an MCP tool call
//! (`mcp`). Each resolves into one request template the engine sends like
//! any other, plus a [`Protocol`] that says how the answer is read: an SSE
//! stream event by event (time to the first event or token, the gaps
//! between them, items per second, stalls), a WebSocket upgrade followed
//! by masked text frames and their echoes, gRPC's length-prefixed message
//! and `grpc-status` trailer, or a JSON-RPC result with its `isError`.
//!
//! WebSocket frames and gRPC framing are written here (a few dozen lines
//! each) rather than pulled from a client library: the engine's pinned
//! connections, TLS and pools stay the same for every protocol.

use std::collections::BTreeMap;

use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::report::{histogram, Percentiles, WireHist};
use super::scenario::{Dur, RequestTemplate, Source};

/// Most tokens an LLM call asks for.
pub const MAX_TOKENS: u32 = 4096;
/// Most messages of a WebSocket session.
pub const MAX_WS_MESSAGES: u32 = 10_000;
/// Largest WebSocket message.
pub const MAX_WS_SIZE: u32 = 64 * 1024;
/// Longest a WebSocket or SSE session is held open on purpose.
pub const MAX_HOLD: std::time::Duration = std::time::Duration::from_secs(3600);
/// Most events an SSE layer reads per stream.
pub const MAX_EVENTS: u32 = 100_000;
/// Largest SSE event kept (the rest of a longer one is dropped).
pub const MAX_EVENT_BYTES: usize = 64 * 1024;

/// How an answer is read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    Http,
    Llm,
    Sse,
    Websocket,
    Grpc,
    Mcp,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Http => "http",
            Protocol::Llm => "llm",
            Protocol::Sse => "sse",
            Protocol::Websocket => "websocket",
            Protocol::Grpc => "grpc",
            Protocol::Mcp => "mcp",
        }
    }

    /// The answer is a stream of items timed one by one.
    pub fn streams(self) -> bool {
        matches!(self, Protocol::Llm | Protocol::Sse)
    }

    /// The answer may carry no served-by tag: gRPC never does (its service
    /// is outside the server layer), a WebSocket upgrade only when it went
    /// through a Rustybin server's layer. Without one the class is
    /// `unattributed`, not `gateway`.
    pub fn unattributed(self) -> bool {
        matches!(self, Protocol::Websocket | Protocol::Grpc)
    }
}

/// An LLM chat completion (the OpenAI-style route of the mock LLM or a
/// gateway in front of one).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmSource {
    /// Default `/ai/v1/chat/completions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// `max_tokens` (default 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u32>,
    /// Stream the answer (default true: tokens are timed one by one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Extra headers (an API key, the mock LLM's pacing headers).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// A server-sent event stream.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SseSource {
    /// Default `/sse`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Stop reading after this many events (default: the stream's end).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<u32>,
    /// A reader that never reads: hold the stream open without reading it
    /// for this long, then close (what a stalled client does to the
    /// server's buffers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_unread: Option<Dur>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// A WebSocket session: connect, send text messages, read the echoes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WsSource {
    /// Default `/ws`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Messages per session (default 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<u32>,
    /// Bytes per message (default 64).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    /// Keep the session open this long after the messages (connection
    /// bursts that stay).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<Dur>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// A unary gRPC call of Rustybin's echo service (or a gateway in front).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrpcSource {
    /// Default `/rustybin.echo.v1.EchoService/Echo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The echoed message (default `ping`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// An MCP tool call (stateless, protocol 2026-07-28).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpSource {
    /// Default `/mcp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// The protocol of a source, and whether it has more than one kind.
pub fn protocol_of(src: &Source) -> Result<Protocol, usize> {
    let kinds = [
        (src.llm.is_some(), Protocol::Llm),
        (src.sse.is_some(), Protocol::Sse),
        (src.websocket.is_some(), Protocol::Websocket),
        (src.grpc.is_some(), Protocol::Grpc),
        (src.mcp.is_some(), Protocol::Mcp),
        (
            src.mock.is_some() || src.request.is_some() || !src.requests.is_empty(),
            Protocol::Http,
        ),
    ];
    let set: Vec<Protocol> = kinds
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, p)| *p)
        .collect();
    match set.as_slice() {
        [one] => Ok(*one),
        [] => Ok(Protocol::Http),
        many => Err(many.len()),
    }
}

fn path_ok(p: &str) -> bool {
    p.starts_with('/') && p.len() <= 2048 && !p.contains(['\r', '\n', ' '])
}

/// Every problem with a protocol source, prefixed with `here`
/// (`layers[0].source`).
pub fn validate(src: &Source, here: &str, p: &mut Vec<String>) {
    if let Err(n) = protocol_of(src) {
        p.push(format!(
            "{here}: one source kind only (request, requests, mock, llm, sse, websocket, grpc or mcp), not {n}"
        ));
    }
    let headers = |name: &str, h: &BTreeMap<String, String>, p: &mut Vec<String>| {
        if h.len() > super::scenario::MAX_HEADERS {
            p.push(format!(
                "{here}.{name}.headers: at most {}",
                super::scenario::MAX_HEADERS
            ));
        }
        for (k, v) in h {
            if axum::http::HeaderName::from_bytes(k.as_bytes()).is_err()
                || axum::http::HeaderValue::from_str(v).is_err()
                || v.len() > 8192
            {
                p.push(format!("{here}.{name}.headers.{k}: not a header"));
            }
        }
    };
    let path = |name: &str, v: &Option<String>, p: &mut Vec<String>| {
        if let Some(v) = v {
            if !path_ok(v) {
                p.push(format!(
                    "{here}.{name}.path: a path starting with /, at most 2048 characters"
                ));
            } else if super::safety::is_control_prefix(v) {
                p.push(format!(
                    "{here}.{name}.path: a run never targets a Rustybin control plane"
                ));
            }
        }
    };
    if let Some(l) = &src.llm {
        path("llm", &l.path, p);
        headers("llm", &l.headers, p);
        if l.tokens.is_some_and(|t| t == 0 || t > MAX_TOKENS) {
            p.push(format!("{here}.llm.tokens: 1 to {MAX_TOKENS}"));
        }
        if l.prompt.as_ref().is_some_and(|x| x.len() > 16 * 1024) {
            p.push(format!("{here}.llm.prompt: at most 16 KB"));
        }
        if l.model
            .as_ref()
            .is_some_and(|m| m.is_empty() || m.len() > 128)
        {
            p.push(format!("{here}.llm.model: 1 to 128 characters"));
        }
    }
    if let Some(s) = &src.sse {
        path("sse", &s.path, p);
        headers("sse", &s.headers, p);
        if s.events.is_some_and(|e| e == 0 || e > MAX_EVENTS) {
            p.push(format!("{here}.sse.events: 1 to {MAX_EVENTS}"));
        }
        if s.hold_unread
            .is_some_and(|d| d.0.is_zero() || d.0 > MAX_HOLD)
        {
            p.push(format!("{here}.sse.hold_unread: longer than 0, at most 1h"));
        }
    }
    if let Some(w) = &src.websocket {
        path("websocket", &w.path, p);
        headers("websocket", &w.headers, p);
        if w.messages.is_some_and(|m| m > MAX_WS_MESSAGES) {
            p.push(format!("{here}.websocket.messages: 0 to {MAX_WS_MESSAGES}"));
        }
        if w.size.is_some_and(|z| z == 0 || z > MAX_WS_SIZE) {
            p.push(format!("{here}.websocket.size: 1 to {MAX_WS_SIZE} bytes"));
        }
        if w.hold.is_some_and(|d| d.0 > MAX_HOLD) {
            p.push(format!("{here}.websocket.hold: at most 1h"));
        }
    }
    if let Some(g) = &src.grpc {
        path("grpc", &g.path, p);
        headers("grpc", &g.headers, p);
        if g.message.as_ref().is_some_and(|m| m.len() > 64 * 1024) {
            p.push(format!("{here}.grpc.message: at most 64 KB"));
        }
    }
    if let Some(m) = &src.mcp {
        path("mcp", &m.path, p);
        headers("mcp", &m.headers, p);
        if m.tool.is_empty() || m.tool.len() > 128 {
            p.push(format!("{here}.mcp.tool: 1 to 128 characters"));
        }
        if m.arguments
            .as_ref()
            .is_some_and(|a| !a.is_object() || a.to_string().len() > 64 * 1024)
        {
            p.push(format!("{here}.mcp.arguments: an object of at most 64 KB"));
        }
    }
}

/// The request template a protocol source sends (`None` for an HTTP
/// source: its own templates).
pub fn template_of(src: &Source) -> Option<RequestTemplate> {
    let mut t = RequestTemplate::default();
    let put = |t: &mut RequestTemplate, h: &BTreeMap<String, String>| {
        for (k, v) in h {
            t.headers.insert(k.to_ascii_lowercase(), v.clone());
        }
    };
    if let Some(l) = &src.llm {
        t.method = "POST".into();
        t.path = l
            .path
            .clone()
            .unwrap_or_else(|| "/ai/v1/chat/completions".into());
        t.body = Some(json!({
            "model": l.model.clone().unwrap_or_else(|| "gpt-4o-mini".into()),
            "messages": [{ "role": "user", "content": l.prompt.clone().unwrap_or_else(|| "Write a short paragraph about load testing an API gateway.".into()) }],
            "max_tokens": l.tokens.unwrap_or(100),
            "stream": l.stream.unwrap_or(true),
        }));
        t.headers.insert(
            "accept".into(),
            "text/event-stream, application/json".into(),
        );
        put(&mut t, &l.headers);
        return Some(t);
    }
    if let Some(s) = &src.sse {
        t.path = s.path.clone().unwrap_or_else(|| "/sse".into());
        t.headers
            .insert("accept".into(), "text/event-stream".into());
        put(&mut t, &s.headers);
        return Some(t);
    }
    if let Some(w) = &src.websocket {
        t.path = w.path.clone().unwrap_or_else(|| "/ws".into());
        t.expect_status = vec![101];
        put(&mut t, &w.headers);
        return Some(t);
    }
    if let Some(g) = &src.grpc {
        t.method = "POST".into();
        t.path = g
            .path
            .clone()
            .unwrap_or_else(|| "/rustybin.echo.v1.EchoService/Echo".into());
        t.headers
            .insert("content-type".into(), "application/grpc".into());
        t.headers.insert("te".into(), "trailers".into());
        put(&mut t, &g.headers);
        // The body is bytes ([`grpc_body`]), not a template value.
        return Some(t);
    }
    if let Some(m) = &src.mcp {
        t.method = "POST".into();
        t.path = m.path.clone().unwrap_or_else(|| "/mcp".into());
        t.headers.insert(
            "accept".into(),
            "application/json, text/event-stream".into(),
        );
        t.headers
            .insert("mcp-protocol-version".into(), "2026-07-28".into());
        // The modern era derives these from the body and checks they match.
        t.headers.insert("mcp-method".into(), "tools/call".into());
        t.headers.insert("mcp-name".into(), m.tool.clone());
        t.body = Some(json!({
            "jsonrpc": "2.0",
            "id": "{{seq}}",
            "method": "tools/call",
            "params": {
                "name": m.tool,
                "arguments": m.arguments.clone().unwrap_or_else(|| json!({})),
                "_meta": {
                    crate::mcp::protocol::META_PROTOCOL_VERSION: "2026-07-28",
                    crate::mcp::protocol::META_CLIENT_CAPABILITIES: {},
                    crate::mcp::protocol::META_CLIENT_INFO: { "name": "rustybin-harness", "version": env!("CARGO_PKG_VERSION") }
                }
            }
        }));
        put(&mut t, &m.headers);
        return Some(t);
    }
    None
}

/// The schema properties of the protocol sources.
pub fn schema_properties(duration: impl Fn(&str, u64) -> Value) -> Value {
    let string =
        |d: &str, max: usize| json!({ "type": "string", "description": d, "maxLength": max });
    let integer = |d: &str, min: u64, max: u64| json!({ "type": "integer", "description": d, "minimum": min, "maximum": max });
    let headers = || json!({ "type": "object", "description": "Extra request headers (an API key; placeholders allowed).", "additionalProperties": { "type": "string", "description": "A header value.", "maxLength": 8192 }, "maxProperties": super::scenario::MAX_HEADERS });
    json!({
        "llm": {
            "type": "object",
            "description": "An LLM chat completion in the OpenAI shape (the mock LLM at /ai, or a gateway in front of one). Streamed answers are timed token by token: time to the first token, the gaps between tokens, tokens per second, stalls. The mock LLM's X-Rustybin-TTFT-Ms and X-Rustybin-Tokens-Per-Second headers give it known timings.",
            "properties": {
                "path": string("The route (default /ai/v1/chat/completions).", 2048),
                "model": string("The model name sent (default gpt-4o-mini).", 128),
                "prompt": string("The user message.", 16 * 1024),
                "tokens": integer("max_tokens (default 100).", 1, MAX_TOKENS as u64),
                "stream": { "type": "boolean", "description": "Stream the answer (default true)." },
                "headers": headers()
            },
            "additionalProperties": false
        },
        "sse": {
            "type": "object",
            "description": "A server-sent event stream, read event by event: time to the first event, the gaps, events per second.",
            "properties": {
                "path": string("The route (default /sse).", 2048),
                "events": integer("Stop reading after this many events (default: the stream's end).", 1, MAX_EVENTS as u64),
                "hold_unread": duration("A reader that never reads: hold the stream open without reading for this long, then close.", MAX_HOLD.as_secs()),
                "headers": headers()
            },
            "additionalProperties": false
        },
        "websocket": {
            "type": "object",
            "description": "A WebSocket session (HTTP/1.1 upgrade on the target): connect, send text messages, read the echoes, close. Timed per session; the messages' round trips are the stream items.",
            "properties": {
                "path": string("The route (default /ws).", 2048),
                "messages": integer("Messages per session (default 10; 0 connects and closes).", 0, MAX_WS_MESSAGES as u64),
                "size": integer("Bytes per message (default 64).", 1, MAX_WS_SIZE as u64),
                "hold": duration("Keep the session open this long after the messages.", MAX_HOLD.as_secs()),
                "headers": headers()
            },
            "additionalProperties": false
        },
        "grpc": {
            "type": "object",
            "description": "A unary gRPC call of Rustybin's echo service (or a gateway in front of it) over HTTP/2: the target needs http: h2c (plain) or h2 (TLS). The answer's grpc-status decides; it carries no served-by tag (class unattributed).",
            "properties": {
                "path": string("The method path (default /rustybin.echo.v1.EchoService/Echo).", 2048),
                "message": string("The echoed message (default ping).", 64 * 1024),
                "headers": headers()
            },
            "additionalProperties": false
        },
        "mcp": {
            "type": "object",
            "description": "An MCP tool call (tools/call, stateless protocol 2026-07-28) on the mock MCP server at /mcp, an OpenAPI mock's MCP server, or a gateway in front. A JSON-RPC error or a result with isError is an unexpected answer.",
            "properties": {
                "path": string("The MCP endpoint (default /mcp).", 2048),
                "tool": string("The tool name.", 128),
                "arguments": { "type": "object", "description": "The tool's arguments.", "additionalProperties": { "description": "An argument." } },
                "headers": headers()
            },
            "required": ["tool"],
            "additionalProperties": false
        }
    })
}

// ── SSE ─────────────────────────────────────────────────────────────

/// Splits a byte stream into SSE events (the `data` lines joined).
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    /// Feed bytes; the complete events in them.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        if self.buf.len() < MAX_EVENT_BYTES {
            let room = MAX_EVENT_BYTES - self.buf.len();
            self.buf.extend_from_slice(&bytes[..bytes.len().min(room)]);
        }
        let mut out = Vec::new();
        while let Some((len, skip)) = find_event_end(&self.buf) {
            let raw = self.buf.drain(..len + skip).collect::<Vec<u8>>();
            let text = String::from_utf8_lossy(&raw[..len]);
            let data: Vec<&str> = text
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(|d| d.strip_prefix(' ').unwrap_or(d))
                .collect();
            if !data.is_empty() {
                out.push(data.join("\n"));
            }
        }
        if self.buf.len() >= MAX_EVENT_BYTES {
            // An event longer than the cap: drop it.
            self.buf.clear();
        }
        out
    }
}

fn find_event_end(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2));
    let crlf = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

/// Whether an LLM stream event carries a token (OpenAI chat chunks), and
/// whether it ends the stream.
pub fn llm_event(data: &str) -> (bool, bool) {
    if data.trim() == "[DONE]" {
        return (false, true);
    }
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return (false, false);
    };
    let token = v["choices"]
        .as_array()
        .and_then(|c| c.first())
        .map(|c| {
            c["delta"]["content"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
                || c["delta"]["tool_calls"].is_array()
                || c["text"].as_str().is_some_and(|s| !s.is_empty())
        })
        .unwrap_or(false);
    let done = v["choices"]
        .as_array()
        .and_then(|c| c.first())
        .and_then(|c| c["finish_reason"].as_str())
        .is_some();
    (token, done)
}

// ── WebSocket ───────────────────────────────────────────────────────

/// A random `Sec-WebSocket-Key`.
pub fn ws_key() -> String {
    let bytes: [u8; 16] = rand::random();
    base64_encode(&bytes)
}

/// The `Sec-WebSocket-Accept` a server must answer `key` with.
pub fn ws_accept(key: &str) -> String {
    use sha1::Digest;
    let mut h = sha1::Sha1::new();
    h.update(key.as_bytes());
    h.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64_encode(&h.finalize())
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A masked client frame (`opcode` 1 text, 8 close, 9 ping).
pub fn ws_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mask: [u8; 4] = rand::random();
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | (opcode & 0x0F));
    let n = payload.len();
    if n < 126 {
        out.push(0x80 | n as u8);
    } else if n < 65_536 {
        out.push(0x80 | 126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

/// A server frame parsed from the front of `buf`: (opcode, payload,
/// bytes consumed), `None` when more bytes are needed.
pub fn ws_parse(buf: &[u8]) -> Option<(u8, Vec<u8>, usize)> {
    if buf.len() < 2 {
        return None;
    }
    let opcode = buf[0] & 0x0F;
    let masked = buf[1] & 0x80 != 0;
    let mut len = usize::from(buf[1] & 0x7F);
    let mut at = 2;
    if len == 126 {
        if buf.len() < 4 {
            return None;
        }
        len = usize::from(u16::from_be_bytes([buf[2], buf[3]]));
        at = 4;
    } else if len == 127 {
        if buf.len() < 10 {
            return None;
        }
        len = u64::from_be_bytes(buf[2..10].try_into().ok()?) as usize;
        at = 10;
    }
    let mask = if masked {
        if buf.len() < at + 4 {
            return None;
        }
        let m = [buf[at], buf[at + 1], buf[at + 2], buf[at + 3]];
        at += 4;
        Some(m)
    } else {
        None
    };
    if buf.len() < at + len {
        return None;
    }
    let payload: Vec<u8> = match mask {
        Some(m) => buf[at..at + len]
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ m[i % 4])
            .collect(),
        None => buf[at..at + len].to_vec(),
    };
    Some((opcode, payload, at + len))
}

// ── gRPC ────────────────────────────────────────────────────────────

/// A length-prefixed gRPC message (uncompressed).
pub fn grpc_frame(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 5);
    out.push(0);
    out.extend_from_slice(&(message.len() as u32).to_be_bytes());
    out.extend_from_slice(message);
    out
}

/// The framed request body of a gRPC source.
pub fn grpc_body(src: &Source) -> Option<Vec<u8>> {
    src.grpc
        .as_ref()
        .map(|g| grpc_frame(&echo_request(g.message.as_deref().unwrap_or("ping"))))
}

/// The encoded `EchoRequest`.
pub fn echo_request(message: &str) -> Vec<u8> {
    use prost::Message;
    crate::grpc::pb::EchoRequest {
        message: message.to_string(),
        count: 0,
    }
    .encode_to_vec()
}

// ── Stream statistics ───────────────────────────────────────────────

/// What one stream (an LLM answer, an SSE stream, a WebSocket session)
/// came to.
#[derive(Clone, Debug, Default)]
pub struct StreamOutcome {
    /// Items read (tokens, events, round trips).
    pub items: u64,
    /// Send to the first item, microseconds.
    pub first_us: Option<u64>,
    /// Gaps between items, microseconds (the first 1024).
    pub gaps_us: Vec<u64>,
    /// Send to the end of the stream, microseconds.
    pub total_us: u64,
    /// No item came for the timeout: the stream was abandoned.
    pub stalled: bool,
    /// The stream ended with an error before its end.
    pub incomplete: bool,
}

/// Most gaps one stream reports.
pub const MAX_GAPS: usize = 1024;

/// A layer's streams added up.
pub struct StreamAcc {
    pub streams: u64,
    pub items: u64,
    pub stalled: u64,
    pub incomplete: u64,
    pub first: Histogram<u64>,
    pub gap: Histogram<u64>,
    pub total: Histogram<u64>,
}

impl Default for StreamAcc {
    fn default() -> Self {
        Self {
            streams: 0,
            items: 0,
            stalled: 0,
            incomplete: 0,
            first: histogram(2),
            gap: histogram(2),
            total: histogram(2),
        }
    }
}

impl StreamAcc {
    pub fn record(&mut self, o: &StreamOutcome) {
        self.streams += 1;
        self.items += o.items;
        self.stalled += u64::from(o.stalled);
        self.incomplete += u64::from(o.incomplete);
        if let Some(f) = o.first_us {
            self.first.saturating_record(f.max(1));
        }
        for g in &o.gaps_us {
            self.gap.saturating_record((*g).max(1));
        }
        self.total.saturating_record(o.total_us.max(1));
    }

    pub fn merge(&mut self, o: &StreamAcc) {
        self.streams += o.streams;
        self.items += o.items;
        self.stalled += o.stalled;
        self.incomplete += o.incomplete;
        let _ = self.first.add(&o.first);
        let _ = self.gap.add(&o.gap);
        let _ = self.total.add(&o.total);
    }

    pub fn is_empty(&self) -> bool {
        self.streams == 0
    }

    pub fn to_wire(&self) -> StreamWire {
        StreamWire {
            streams: self.streams,
            items: self.items,
            stalled: self.stalled,
            incomplete: self.incomplete,
            first: WireHist::encode(&self.first),
            gap: WireHist::encode(&self.gap),
            total: WireHist::encode(&self.total),
        }
    }

    pub fn merge_wire(&mut self, w: &StreamWire) {
        self.streams += w.streams;
        self.items += w.items;
        self.stalled += w.stalled;
        self.incomplete += w.incomplete;
        let _ = w.first.add_to(&mut self.first);
        let _ = w.gap.add_to(&mut self.gap);
        let _ = w.total.add_to(&mut self.total);
    }

    /// The report of layer `layer`.
    pub fn report(&self, layer: &str, protocol: Protocol) -> StreamReport {
        let total = Percentiles::from_histogram(&self.total);
        let secs = self.total.mean() / 1e6;
        StreamReport {
            layer: layer.to_string(),
            protocol: protocol.as_str().to_string(),
            streams: self.streams,
            items: self.items,
            items_per_stream: if self.streams == 0 {
                0.0
            } else {
                (self.items as f64 / self.streams as f64 * 100.0).round() / 100.0
            },
            items_per_second: if secs <= 0.0 || self.streams == 0 {
                0.0
            } else {
                (self.items as f64 / self.streams as f64 / secs * 100.0).round() / 100.0
            },
            first_item: Percentiles::from_histogram(&self.first),
            gap: Percentiles::from_histogram(&self.gap),
            total,
            stalled: self.stalled,
            incomplete: self.incomplete,
        }
    }
}

/// A layer's streams on the wire.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamWire {
    pub streams: u64,
    pub items: u64,
    pub stalled: u64,
    pub incomplete: u64,
    pub first: WireHist,
    pub gap: WireHist,
    pub total: WireHist,
}

/// A layer's streams in the report.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamReport {
    pub layer: String,
    pub protocol: String,
    pub streams: u64,
    /// Tokens (llm), events (sse) or message round trips (websocket).
    pub items: u64,
    pub items_per_stream: f64,
    /// Items per second of stream time, averaged over the streams.
    pub items_per_second: f64,
    /// Send to the first item (time to first token).
    pub first_item: Percentiles,
    /// Between two items (inter-token latency).
    pub gap: Percentiles,
    /// Send to the end of the stream.
    pub total: Percentiles,
    /// Streams abandoned after no item came for the timeout.
    pub stalled: u64,
    /// Streams that ended with an error before their end.
    pub incomplete: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_events_are_split_and_llm_tokens_counted() {
        let mut p = SseParser::default();
        let mut events = p.feed(b"event: x\ndata: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: a\ndata: b\r\n\r\ndata: [DO");
        events.extend(p.feed(b"NE]\n\n"));
        assert_eq!(events.len(), 3);
        assert_eq!(events[1], "a\nb");
        assert_eq!(llm_event(&events[0]), (true, false));
        assert_eq!(llm_event(&events[2]), (false, true));
        assert_eq!(
            llm_event("{\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}"),
            (false, true)
        );
    }

    #[test]
    fn websocket_frames_round_trip_and_the_accept_matches() {
        let frame = ws_frame(1, b"hello");
        assert_eq!(frame.len(), 2 + 4 + 5);
        assert!(frame[1] & 0x80 != 0);
        let (op, payload, used) = ws_parse(&frame).unwrap();
        assert_eq!(
            (op, payload.as_slice(), used),
            (1, &b"hello"[..], frame.len())
        );
        let long = ws_frame(2, &vec![7u8; 300]);
        let (_, payload, _) = ws_parse(&long).unwrap();
        assert_eq!(payload.len(), 300);
        assert!(ws_parse(&frame[..3]).is_none());
        // RFC 6455's example.
        assert_eq!(
            ws_accept("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        assert_eq!(ws_key().len(), 24);
    }

    #[test]
    fn grpc_frames_and_templates() {
        let f = grpc_frame(b"abc");
        assert_eq!(&f[..5], &[0, 0, 0, 0, 3]);
        let src: Source = serde_yaml::from_str("grpc: { message: hi }").unwrap();
        assert_eq!(protocol_of(&src), Ok(Protocol::Grpc));
        let t = template_of(&src).unwrap();
        assert_eq!(t.headers["content-type"], "application/grpc");
        assert_eq!(&grpc_body(&src).unwrap()[..5], &[0, 0, 0, 0, 4]);
        let src: Source =
            serde_yaml::from_str("mcp: { tool: echo, arguments: { message: x } }").unwrap();
        let t = template_of(&src).unwrap();
        assert_eq!(t.body.as_ref().unwrap()["method"], "tools/call");
        let src: Source = serde_yaml::from_str("llm: { tokens: 5 }\nrequest: { path: / }").unwrap();
        let mut p = Vec::new();
        validate(&src, "s", &mut p);
        assert!(p[0].contains("one source kind"), "{p:?}");
    }

    #[test]
    fn stream_stats_add_up() {
        let mut a = StreamAcc::default();
        a.record(&StreamOutcome {
            items: 10,
            first_us: Some(50_000),
            gaps_us: vec![10_000; 9],
            total_us: 140_000,
            ..StreamOutcome::default()
        });
        let mut b = StreamAcc::default();
        b.merge_wire(&a.to_wire());
        let r = b.report("l", Protocol::Llm);
        assert_eq!((r.streams, r.items, r.items_per_stream), (1, 10, 10.0));
        assert!((r.first_item.p50_ms - 50.0).abs() < 1.0, "{r:?}");
        assert!((r.gap.p50_ms - 10.0).abs() < 0.5);
        assert!(
            (r.items_per_second - 71.4).abs() < 2.0,
            "{}",
            r.items_per_second
        );
    }
}
