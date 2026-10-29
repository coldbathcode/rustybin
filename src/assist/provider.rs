//! The model providers behind AI assist: the kinds, the one request shape
//! every task builds, the events a completion streams, and the HTTP and SSE
//! plumbing the real providers share (through the egress policy: pinned
//! addresses, no redirects, a 60 s timeout, a 2 MiB cap).

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::egress::{Egress, Purpose};

/// Time allowed for one completion.
pub const TIMEOUT: Duration = Duration::from_secs(60);
/// Largest response body read from a provider.
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// Largest error body kept for the message.
const MAX_ERROR_BYTES: usize = 16 * 1024;

/// A provider, by its API shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Anthropic,
    OpenAi,
    Gemini,
    /// Any OpenAI-compatible Chat Completions endpoint (`assist.base_url`).
    Compatible,
    /// The built-in deterministic answers: no key, no network.
    Mock,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Anthropic,
        Kind::OpenAi,
        Kind::Gemini,
        Kind::Compatible,
        Kind::Mock,
    ];

    pub fn parse(raw: &str) -> Option<Kind> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "anthropic" | "claude" => Some(Kind::Anthropic),
            "openai" => Some(Kind::OpenAi),
            "gemini" | "google" => Some(Kind::Gemini),
            "compatible" | "openai-compatible" | "openai_compatible" => Some(Kind::Compatible),
            "mock" | "builtin" | "built-in" => Some(Kind::Mock),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Anthropic => "anthropic",
            Kind::OpenAi => "openai",
            Kind::Gemini => "gemini",
            Kind::Compatible => "compatible",
            Kind::Mock => "mock",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Kind::Anthropic => "Anthropic (Messages API)",
            Kind::OpenAi => "OpenAI (Chat Completions)",
            Kind::Gemini => "Gemini (generateContent)",
            Kind::Compatible => "OpenAI-compatible endpoint",
            Kind::Mock => "Built-in mock LLM (no key, no network)",
        }
    }

    /// The settings key of the provider's API key (`None` for the mock).
    pub fn key_setting(self) -> Option<&'static str> {
        match self {
            Kind::Anthropic => Some("assist.anthropic_key"),
            Kind::OpenAi => Some("assist.openai_key"),
            Kind::Gemini => Some("assist.gemini_key"),
            Kind::Compatible => Some("assist.compatible_key"),
            Kind::Mock => None,
        }
    }

    /// The provider's API origin (the compatible one comes from `assist.base_url`).
    pub fn default_base_url(self) -> Option<&'static str> {
        match self {
            Kind::Anthropic => Some("https://api.anthropic.com"),
            Kind::OpenAi => Some("https://api.openai.com/v1"),
            Kind::Gemini => Some("https://generativelanguage.googleapis.com"),
            Kind::Compatible | Kind::Mock => None,
        }
    }
}

/// What a task asks a model: one system prompt, one user prompt, an
/// optional JSON schema for a structured answer, and the output cap. For the
/// mock provider, the deterministic answer the task prepared.
#[derive(Clone, Debug)]
pub struct Request {
    pub model: String,
    pub system: String,
    pub user: String,
    pub schema: Option<Value>,
    pub max_output_tokens: u32,
    pub mock_answer: Option<Value>,
}

/// What a completion streams back.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A piece of the answer (text, or the JSON of a structured answer).
    Delta(String),
    /// Token counts so far (cumulative where the provider reports them so;
    /// the runner keeps the largest value seen).
    Usage { input: u64, output: u64 },
    /// The provider reported an error mid-stream.
    Error(String),
}

/// A failure before any event: the provider refused the call, the egress
/// policy refused the host, the network failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// The provider's HTTP status when there was one, else 0.
    pub status: u16,
    pub message: String,
}

impl Problem {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

pub type EventStream = Pin<Box<dyn Stream<Item = Event> + Send>>;

/// A scripted answer sequence (tests): each call pops one entry.
pub type Script = Arc<Mutex<VecDeque<Result<Vec<Event>, Problem>>>>;

/// The configured provider with its credential.
#[derive(Clone)]
pub enum Client {
    Anthropic {
        key: Zeroizing<String>,
        base: String,
    },
    OpenAi {
        key: Option<Zeroizing<String>>,
        base: String,
        /// An OpenAI-compatible endpoint: `max_tokens` and a JSON object
        /// response format instead of OpenAI's newer parameters.
        compatible: bool,
    },
    Gemini {
        key: Zeroizing<String>,
        base: String,
    },
    Mock,
    /// Answers from a script (tests).
    #[doc(hidden)]
    Scripted(Script),
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Client({})", self.kind().as_str())
    }
}

impl Client {
    pub fn kind(&self) -> Kind {
        match self {
            Client::Anthropic { .. } => Kind::Anthropic,
            Client::OpenAi {
                compatible: false, ..
            } => Kind::OpenAi,
            Client::OpenAi {
                compatible: true, ..
            } => Kind::Compatible,
            Client::Gemini { .. } => Kind::Gemini,
            Client::Mock | Client::Scripted(_) => Kind::Mock,
        }
    }

    /// Start a completion: the events, or why it could not start.
    pub async fn complete(&self, egress: &Egress, req: &Request) -> Result<EventStream, Problem> {
        match self {
            Client::Anthropic { key, base } => {
                super::anthropic::complete(egress, base, key, req).await
            }
            Client::OpenAi {
                key,
                base,
                compatible,
            } => super::openai::complete(egress, base, key.as_ref(), *compatible, req).await,
            Client::Gemini { key, base } => super::gemini::complete(egress, base, key, req).await,
            Client::Mock => Ok(super::mock::complete(req)),
            Client::Scripted(script) => {
                let next = script
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pop_front()
                    .unwrap_or_else(|| Err(Problem::new(0, "the script ran out")));
                next.map(|events| Box::pin(futures_util::stream::iter(events)) as EventStream)
            }
        }
    }
}

// ── HTTP and SSE shared by the real providers ───────────────────────

/// One Server-Sent Event from a provider.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

/// An incremental SSE parser over byte chunks.
#[derive(Default)]
pub struct SseParser {
    buf: String,
    pending: Vec<u8>,
}

impl SseParser {
    /// Feed a chunk; returns the events completed by it.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.pending.extend_from_slice(chunk);
        // Decode as far as the bytes form valid UTF-8; keep a split character.
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(s) => s.len(),
            Err(e) => e.valid_up_to(),
        };
        let text = String::from_utf8_lossy(&self.pending[..valid]).into_owned();
        self.pending.drain(..valid);
        if self.pending.len() > 4 {
            // Not a split character: invalid bytes, dropped.
            self.pending.clear();
        }
        self.buf
            .push_str(&text.replace("\r\n", "\n").replace('\r', "\n"));
        let mut out = Vec::new();
        while let Some(idx) = self.buf.find("\n\n") {
            let block = self.buf[..idx].to_string();
            self.buf.drain(..idx + 2);
            if let Some(ev) = parse_block(&block) {
                out.push(ev);
            }
        }
        out
    }

    /// The last block when the stream ends without a blank line.
    pub fn finish(&mut self) -> Option<SseEvent> {
        let block = std::mem::take(&mut self.buf);
        parse_block(&block)
    }
}

fn parse_block(block: &str) -> Option<SseEvent> {
    let mut ev = SseEvent::default();
    let mut data = Vec::new();
    for line in block.lines() {
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => ev.event = value.to_string(),
            "data" => data.push(value.to_string()),
            _ => {}
        }
    }
    if data.is_empty() && ev.event.is_empty() {
        return None;
    }
    ev.data = data.join("\n");
    Some(ev)
}

/// POST a JSON body and stream the SSE events of the response. A non-2xx
/// answer is a [`Problem`] carrying the provider's message.
pub async fn post_sse(
    egress: &Egress,
    url: &str,
    headers: Vec<(&'static str, String)>,
    body: &Value,
) -> Result<Pin<Box<dyn Stream<Item = Result<SseEvent, String>> + Send>>, Problem> {
    let pinned = egress
        .resolve(url, Purpose::Assist)
        .await
        .map_err(|e| Problem::new(0, format!("outbound call refused: {e}")))?;
    let client = egress
        .client(&pinned, TIMEOUT)
        .map_err(|e| Problem::new(0, format!("client: {e}")))?;
    let mut request = client
        .post(&pinned.url)
        .header("accept", "text/event-stream")
        .json(body);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let resp = request
        .send()
        .await
        .map_err(|e| Problem::new(0, describe_reqwest(&e)))?;
    let status = resp.status();
    if !status.is_success() {
        let bytes = read_capped(resp, MAX_ERROR_BYTES).await;
        let text = String::from_utf8_lossy(&bytes);
        return Err(Problem::new(
            status.as_u16(),
            provider_error(&text, status.as_u16()),
        ));
    }
    let mut parser = SseParser::default();
    let mut total = 0usize;
    let mut body_stream = resp.bytes_stream();
    let stream = async_stream::stream! {
        while let Some(chunk) = body_stream.next().await {
            match chunk {
                Ok(bytes) => {
                    total += bytes.len();
                    if total > MAX_RESPONSE_BYTES {
                        yield Err(format!("the response exceeded {MAX_RESPONSE_BYTES} bytes"));
                        return;
                    }
                    for ev in parser.feed(&bytes) {
                        yield Ok(ev);
                    }
                }
                Err(e) => {
                    yield Err(describe_reqwest(&e));
                    return;
                }
            }
        }
        if let Some(ev) = parser.finish() {
            yield Ok(ev);
        }
    };
    Ok(Box::pin(stream))
}

async fn read_capped(resp: reqwest::Response, cap: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(Ok(chunk)) = stream.next().await {
        out.extend_from_slice(&chunk);
        if out.len() >= cap {
            out.truncate(cap);
            break;
        }
    }
    out
}

fn describe_reqwest(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "the provider did not answer in time".to_string()
    } else if e.is_connect() {
        "could not connect to the provider".to_string()
    } else {
        let mut text = e.to_string();
        // reqwest's messages include the URL (which may carry a key in a
        // query, as Gemini's once did): keep the kind only.
        if let Some(idx) = text.find(" for url") {
            text.truncate(idx);
        }
        text
    }
}

/// The message of a provider's error body (`{"error": {"message": ...}}`,
/// Gemini's `{"error": {"message"}}`, Anthropic's `{"error": {"message"}}`),
/// else the status.
pub fn provider_error(body: &str, status: u16) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let message = parsed
        .as_ref()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.pointer("/error"))
                .or_else(|| v.pointer("/message"))
        })
        .map(|m| match m {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let t = body.trim();
            if t.is_empty() {
                format!("HTTP {status}")
            } else {
                t.chars().take(300).collect()
            }
        });
    format!("the provider answered {status}: {message}")
}

/// `data:` payload of an SSE event parsed as JSON (`None` for `[DONE]` and
/// non-JSON keep-alives).
pub fn data_json(ev: &SseEvent) -> Option<Value> {
    let d = ev.data.trim();
    if d.is_empty() || d == "[DONE]" {
        return None;
    }
    serde_json::from_str(d).ok()
}

/// A rough token count for providers that report none (and the mock): about
/// four characters per token.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}
