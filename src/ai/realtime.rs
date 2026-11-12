//! The OpenAI-shaped realtime API: `GET /ai/openai/v1/realtime`, a
//! WebSocket with JSON events both ways.
//!
//! Client events: `session.update`, `input_audio_buffer.append`, `commit`
//! and `clear`, `conversation.item.create`, `response.create`,
//! `response.cancel`. Server events: `session.created` and `updated`,
//! `input_audio_buffer.committed` and `cleared`,
//! `conversation.item.created`, the `response.*` family (text, audio,
//! transcript and function call argument deltas), `response.done` with
//! usage, `rate_limits.updated`, `error`.
//!
//! Answers come from the engine every other AI path uses
//! ([`super::engine`]), so the modes, the scripted rules and the tool
//! calls are the same. Audio is never decoded or kept: input audio is
//! counted (bytes to duration to tokens), output audio is silence or a
//! tone in the session's format, and the transcript carries the text. A
//! session's record ([`AiCtx::record_detail`]) holds event types, sizes
//! and text only.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use axum::{Extension, Router};
use base64::Engine as _;
use serde_json::{json, Value};

use super::engine::{self, ChatInput, Finish, Msg, Part, Role, ToolCall};
use super::faults::{ErrorKind, Pace};
use super::{openai, rand_id, AiCtx, DetailArgs};
use crate::catalog::{category, Endpoint};
use crate::state::AppState;

pub const PATH: &str = "/ai/openai/v1/realtime";
pub const DEFAULT_MODEL: &str = "rustybin-realtime";
/// The largest client event (an audio append is base64 in one event).
pub const MAX_EVENT_BYTES: usize = 512 * 1024;
/// Most items a session's conversation keeps (the oldest go first).
const MAX_ITEMS: usize = 200;
/// Most events a session's record keeps.
const MAX_LOGGED: usize = 200;
/// A session without a client event this long is closed.
const IDLE: Duration = Duration::from_secs(300);
const SEND_LIMIT: Duration = Duration::from_secs(10);

/// The realtime sessions open on the instance: at most
/// `ai.realtime.max_sessions`.
#[derive(Default)]
pub struct Sessions {
    open: AtomicUsize,
}

/// Held while a session is open.
pub struct Slot(Arc<Sessions>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Sessions {
    pub fn open(&self) -> usize {
        self.open.load(Ordering::Acquire)
    }

    /// A slot, unless `max` sessions are open.
    pub fn take(self: &Arc<Self>, max: usize) -> Option<Slot> {
        let mut now = self.open.load(Ordering::Acquire);
        loop {
            if now >= max {
                return None;
            }
            match self
                .open
                .compare_exchange(now, now + 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return Some(Slot(self.clone())),
                Err(seen) => now = seen,
            }
        }
    }
}

// ── Audio ───────────────────────────────────────────────────────────

/// The audio formats of a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AudioFormat {
    Pcm16,
    Ulaw,
    Alaw,
}

impl AudioFormat {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "pcm16" => Some(Self::Pcm16),
            "g711_ulaw" => Some(Self::Ulaw),
            "g711_alaw" => Some(Self::Alaw),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Pcm16 => "pcm16",
            Self::Ulaw => "g711_ulaw",
            Self::Alaw => "g711_alaw",
        }
    }

    /// Bytes of one second: PCM16 at 24 kHz mono, G.711 at 8 kHz.
    fn bytes_per_second(self) -> usize {
        match self {
            Self::Pcm16 => 48_000,
            Self::Ulaw | Self::Alaw => 8_000,
        }
    }

    fn sample_rate(self) -> f32 {
        match self {
            Self::Pcm16 => 24_000.0,
            Self::Ulaw | Self::Alaw => 8_000.0,
        }
    }
}

/// G.711 mu-law of one 16-bit sample.
fn ulaw(sample: i16) -> u8 {
    const BIAS: i32 = 0x84;
    let mut s = i32::from(sample);
    let sign = if s < 0 {
        s = -s;
        0x80
    } else {
        0
    };
    s = (s + BIAS).min(32_635 + BIAS);
    let mut exponent = 7u8;
    let mut mask = 0x4000;
    while exponent > 0 && s & mask == 0 {
        exponent -= 1;
        mask >>= 1;
    }
    let mantissa = ((s >> (exponent + 3)) & 0x0f) as u8;
    !(sign | (exponent << 4) | mantissa)
}

/// G.711 A-law of one 16-bit sample.
fn alaw(sample: i16) -> u8 {
    let mut s = i32::from(sample);
    let sign = if s >= 0 { 0x80 } else { 0 };
    if s < 0 {
        s = -s - 1;
    }
    s = s.min(32_635);
    let (exponent, mantissa) = if s >= 256 {
        let mut e = 7u8;
        let mut mask = 0x4000;
        while e > 1 && s & mask == 0 {
            e -= 1;
            mask >>= 1;
        }
        (e, ((s >> (e + 3)) & 0x0f) as u8)
    } else {
        (0, (s >> 4) as u8)
    };
    (sign | (exponent << 4) | mantissa) ^ 0x55
}

/// 100 ms of output audio: silence, or a 440 Hz tone at a low level.
/// `chunk` keeps the tone's phase across deltas.
fn audio_chunk(format: AudioFormat, tone: bool, chunk: usize) -> Vec<u8> {
    let samples = (format.sample_rate() / 10.0) as usize;
    let mut out = Vec::with_capacity(samples * 2);
    for i in 0..samples {
        let sample: i16 = if tone {
            let t = (chunk * samples + i) as f32 / format.sample_rate();
            ((t * 440.0 * std::f32::consts::TAU).sin() * 3000.0) as i16
        } else {
            0
        };
        match format {
            AudioFormat::Pcm16 => out.extend_from_slice(&sample.to_le_bytes()),
            AudioFormat::Ulaw => out.push(ulaw(sample)),
            AudioFormat::Alaw => out.push(alaw(sample)),
        }
    }
    out
}

/// Input audio tokens for a duration: ten a second.
fn audio_tokens(ms: u64) -> u32 {
    (ms / 100).min(u64::from(u32::MAX)) as u32
}

// ── A session ───────────────────────────────────────────────────────

/// What a fault query asks of a session (`?fail=drop_after:N`,
/// `?fail=error_after:N`): after N server events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    DropAfter(u64),
    ErrorAfter(u64),
}

fn parse_fault(raw: Option<&String>) -> Option<Fault> {
    let (kind, n) = raw?.split_once(':')?;
    let n = n.trim().parse::<u64>().ok()?;
    match kind.trim() {
        "drop_after" => Some(Fault::DropAfter(n)),
        "error_after" => Some(Fault::ErrorAfter(n)),
        _ => None,
    }
}

struct Session {
    ctx: AiCtx,
    id: String,
    model: String,
    instructions: String,
    tools: Vec<engine::Tool>,
    tool_choice: engine::ToolChoice,
    tools_json: Value,
    tool_choice_json: Value,
    /// `text`, or `audio` and `text`.
    audio_out: bool,
    voice: String,
    input_format: AudioFormat,
    output_format: AudioFormat,
    max_output_tokens: Option<u32>,
    tone: bool,
    pace: Pace,
    items: VecDeque<(String, Msg)>,
    /// Input audio waiting for a commit: bytes only, never the audio.
    buffered: usize,
    /// Input audio tokens of the conversation.
    audio_in_tokens: u32,
    sent: u64,
    fault: Option<Fault>,
    log: Vec<Value>,
    logged_out: usize,
    prompt_tokens: u32,
    completion_tokens: u32,
    responses: u32,
    last_reply: String,
    last_mode: String,
    last_finish: String,
    recorded: bool,
}

/// What ends a session's loop.
enum Flow {
    Go,
    /// The client is gone, or a send failed.
    Gone,
    /// A fault: drop the connection without a close frame.
    Drop,
}

impl Session {
    fn log(&mut self, dir: &str, kind: &str, bytes: usize, text: Option<&str>) {
        if self.log.len() >= MAX_LOGGED {
            self.logged_out += 1;
            return;
        }
        let mut e = json!({ "dir": dir, "type": kind, "bytes": bytes });
        if let Some(t) = text {
            e["text"] = json!(t.chars().take(300).collect::<String>());
        }
        self.log.push(e);
    }

    fn session_json(&self) -> Value {
        json!({
            "id": self.id,
            "object": "realtime.session",
            "model": self.model,
            "modalities": if self.audio_out { json!(["audio", "text"]) } else { json!(["text"]) },
            "instructions": self.instructions,
            "voice": self.voice,
            "input_audio_format": self.input_format.as_str(),
            "output_audio_format": self.output_format.as_str(),
            "input_audio_transcription": null,
            "turn_detection": null,
            "tools": self.tools_json,
            "tool_choice": self.tool_choice_json,
            "temperature": 0.8,
            "max_response_output_tokens": match self.max_output_tokens {
                Some(n) => json!(n),
                None => json!("inf"),
            },
        })
    }

    /// Send one server event. A fault counts them: the Nth is the last
    /// before a drop, or is followed by an `error` event.
    async fn emit(&mut self, socket: &mut WebSocket, mut event: Value) -> Flow {
        self.sent += 1;
        event["event_id"] = json!(format!("event_{}", rand_id(21)));
        let kind = event["type"].as_str().unwrap_or("").to_string();
        let text = event["delta"]
            .as_str()
            .filter(|_| kind == "response.text.delta" || kind == "response.audio_transcript.delta")
            .or(event["error"]["message"].as_str())
            .map(str::to_string);
        let wire = event.to_string();
        self.log("out", &kind, wire.len(), text.as_deref());
        let sent = tokio::time::timeout(SEND_LIMIT, socket.send(Message::Text(wire.into()))).await;
        if !matches!(sent, Ok(Ok(()))) {
            return Flow::Gone;
        }
        match self.fault {
            Some(Fault::DropAfter(n)) if self.sent >= n => Flow::Drop,
            Some(Fault::ErrorAfter(n)) if self.sent == n => {
                self.fault = None;
                self.ctx
                    .shared
                    .metrics
                    .count_llm_fault(self.ctx.provider.as_str(), "server_error");
                let e = error_event(
                    "server_error",
                    None,
                    "The server had an error while processing your request. Sorry about that!",
                    None,
                );
                Box::pin(self.emit(socket, e)).await
            }
            _ => Flow::Go,
        }
    }

    fn add_item(&mut self, id: String, msg: Msg) {
        if self.items.len() >= MAX_ITEMS {
            self.items.pop_front();
        }
        self.items.push_back((id, msg));
    }

    fn input(&self, extra_instructions: Option<&str>) -> ChatInput {
        let mut system = Vec::new();
        if !self.instructions.is_empty() {
            system.push(self.instructions.clone());
        }
        if let Some(i) = extra_instructions.filter(|i| !i.is_empty()) {
            system.push(i.to_string());
        }
        ChatInput {
            model: self.model.clone(),
            system,
            messages: self.items.iter().map(|(_, m)| m.clone()).collect(),
            tools: self.tools.clone(),
            tool_choice: self.tool_choice.clone(),
            max_tokens: self.max_output_tokens,
            ..ChatInput::default()
        }
    }

    /// Record the session as it stands (in place of its earlier record).
    fn record(&mut self, new_prompt: u32, new_completion: u32) {
        let detail = json!({
            "session_id": self.id,
            "responses": self.responses,
            "events": self.log,
            "events_not_kept": self.logged_out,
            "audio": {
                "input_tokens": self.audio_in_tokens,
                "kept": false,
            },
        });
        let prompt = engine::render(&self.input(None));
        self.ctx.record_detail(DetailArgs {
            kind: "realtime",
            model: &self.model,
            mode: &self.last_mode,
            prompt: &prompt,
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            new_prompt_tokens: new_prompt,
            new_completion_tokens: new_completion,
            first: !self.recorded,
            finish: &self.last_finish,
            reply: &self.last_reply,
            detail,
        });
        self.recorded = true;
    }
}

fn error_event(
    kind: &str,
    code: Option<&str>,
    message: &str,
    client_event: Option<&Value>,
) -> Value {
    json!({
        "type": "error",
        "error": {
            "type": kind,
            "code": code,
            "message": message,
            "param": null,
            "event_id": client_event.and_then(|e| e["event_id"].as_str()),
        }
    })
}

/// A conversation item of a client event as a message, and its JSON back.
fn parse_item(item: &Value) -> Result<(Msg, Value), String> {
    let kind = item["type"].as_str().unwrap_or("message");
    match kind {
        "message" => {
            let role = match item["role"].as_str().unwrap_or("user") {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                "system" => Role::System,
                other => return Err(format!("Invalid value: '{other}'. Supported values are: 'user', 'assistant' and 'system'.")),
            };
            let mut parts = Vec::new();
            let mut content = Vec::new();
            for c in item["content"].as_array().into_iter().flatten() {
                match c["type"].as_str().unwrap_or("") {
                    t @ ("input_text" | "text") => {
                        let text = c["text"].as_str().unwrap_or_default();
                        parts.push(Part::Text(text.to_string()));
                        content.push(json!({ "type": t, "text": text }));
                    }
                    "input_audio" | "audio" => {
                        // Counted by its size, never decoded or kept.
                        let bytes = c["audio"].as_str().map_or(0, |a| a.len() * 3 / 4);
                        parts.push(Part::Audio(format!("{bytes} bytes")));
                        if let Some(t) = c["transcript"].as_str() {
                            parts.push(Part::Text(t.to_string()));
                        }
                        content.push(json!({ "type": "input_audio", "transcript": c["transcript"] }));
                    }
                    other => return Err(format!("Invalid value: '{other}'. Supported values are: 'input_text', 'input_audio' and 'text'.")),
                }
            }
            let json = json!({ "object": "realtime.item", "type": "message", "status": "completed", "role": role.as_str(), "content": content });
            Ok((Msg { role, parts }, json))
        }
        "function_call_output" => {
            let call_id = item["call_id"].as_str().unwrap_or_default().to_string();
            let output = match &item["output"] {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let json = json!({ "object": "realtime.item", "type": "function_call_output", "call_id": call_id, "output": output });
            let part = Part::ToolResult {
                id: call_id,
                name: None,
                content: output,
                is_error: false,
            };
            Ok((
                Msg {
                    role: Role::Tool,
                    parts: vec![part],
                },
                json,
            ))
        }
        other => Err(format!(
            "Invalid value: '{other}'. Supported values are: 'message' and 'function_call_output'."
        )),
    }
}

/// Send the events of a response, paced; a `response.cancel` that arrives
/// meanwhile ends it. Other client events wait in `pending`.
async fn respond(
    s: &mut Session,
    socket: &mut WebSocket,
    options: &Value,
    pending: &mut VecDeque<String>,
) -> Flow {
    let audio = match options["modalities"].as_array() {
        Some(m) => m.iter().any(|x| x == "audio"),
        None => s.audio_out,
    };
    let input = s.input(options["instructions"].as_str());
    let reply = s.ctx.generate(&input, 0);
    let response_id = format!("resp_{}", rand_id(21));
    let item_id = format!("item_{}", rand_id(21));
    let pieces: Vec<String> = reply.pieces().into_iter().map(str::to_string).collect();
    let call: Option<&ToolCall> = reply.tool_calls.first();
    let modalities = if audio {
        json!(["audio", "text"])
    } else {
        json!(["text"])
    };
    let response = |status: &str, output: Value, usage: Value| {
        json!({
            "id": response_id, "object": "realtime.response", "status": status,
            "status_details": if status == "cancelled" { json!({ "type": "cancelled", "reason": "client_cancelled" }) } else { Value::Null },
            "output": output, "modalities": modalities, "usage": usage,
        })
    };
    macro_rules! emit {
        ($event:expr) => {
            match s.emit(socket, $event).await {
                Flow::Go => {}
                other => return other,
            }
        };
    }
    emit!(
        json!({ "type": "response.created", "response": response("in_progress", json!([]), Value::Null) })
    );

    // The output item: a function call, or a message.
    let mut output_tokens_audio = 0u32;
    let mut cancelled = false;
    let mut said = String::new();
    let item = if let Some(c) = call {
        let call_id = format!("call_{}", c.id);
        let arguments = c.arguments.to_string();
        let item = json!({ "id": item_id, "object": "realtime.item", "type": "function_call", "status": "in_progress", "name": c.name, "call_id": call_id, "arguments": "" });
        emit!(
            json!({ "type": "response.output_item.added", "response_id": response_id, "output_index": 0, "item": item })
        );
        emit!(
            json!({ "type": "conversation.item.created", "previous_item_id": s.items.back().map(|(i, _)| i.clone()), "item": item })
        );
        if !s.pace.ttft.is_zero() {
            tokio::time::sleep(s.pace.ttft).await;
        }
        emit!(
            json!({ "type": "response.function_call_arguments.delta", "response_id": response_id, "item_id": item_id, "output_index": 0, "call_id": call_id, "delta": arguments })
        );
        emit!(
            json!({ "type": "response.function_call_arguments.done", "response_id": response_id, "item_id": item_id, "output_index": 0, "call_id": call_id, "arguments": arguments })
        );
        s.add_item(
            item_id.clone(),
            Msg {
                role: Role::Assistant,
                parts: vec![Part::ToolCall(c.clone())],
            },
        );
        s.last_reply = format!("[tool_call {}] {arguments}", c.name);
        json!({ "id": item_id, "object": "realtime.item", "type": "function_call", "status": "completed", "name": c.name, "call_id": call_id, "arguments": arguments })
    } else {
        let part_type = if audio { "audio" } else { "text" };
        let item = json!({ "id": item_id, "object": "realtime.item", "type": "message", "status": "in_progress", "role": "assistant", "content": [] });
        emit!(
            json!({ "type": "response.output_item.added", "response_id": response_id, "output_index": 0, "item": item })
        );
        emit!(
            json!({ "type": "conversation.item.created", "previous_item_id": s.items.back().map(|(i, _)| i.clone()), "item": item })
        );
        let empty_part = if audio {
            json!({ "type": "audio", "transcript": "" })
        } else {
            json!({ "type": "text", "text": "" })
        };
        emit!(
            json!({ "type": "response.content_part.added", "response_id": response_id, "item_id": item_id, "output_index": 0, "content_index": 0, "part": empty_part })
        );
        let delay = s.pace.per_piece(pieces.len());
        for (i, piece) in pieces.iter().enumerate() {
            let wait = if i == 0 { s.pace.ttft } else { delay };
            if !wait.is_zero() {
                // Paced: a cancel that arrives meanwhile stops the response.
                let sleep = tokio::time::sleep(wait);
                tokio::pin!(sleep);
                loop {
                    tokio::select! {
                        _ = &mut sleep => break,
                        msg = socket.recv() => match msg {
                            Some(Ok(Message::Text(t))) => {
                                let is_cancel = serde_json::from_str::<Value>(&t)
                                    .is_ok_and(|v| v["type"] == "response.cancel");
                                if is_cancel {
                                    s.log("in", "response.cancel", t.len(), None);
                                    cancelled = true;
                                    break;
                                }
                                if pending.len() < 32 {
                                    pending.push_back(t.to_string());
                                }
                            }
                            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return Flow::Gone,
                            Some(Ok(_)) => {}
                        }
                    }
                }
                if cancelled {
                    break;
                }
            }
            said.push_str(piece);
            if audio {
                emit!(
                    json!({ "type": "response.audio_transcript.delta", "response_id": response_id, "item_id": item_id, "output_index": 0, "content_index": 0, "delta": piece })
                );
                let chunk = audio_chunk(s.output_format, s.tone, i);
                output_tokens_audio += 2;
                emit!(
                    json!({ "type": "response.audio.delta", "response_id": response_id, "item_id": item_id, "output_index": 0, "content_index": 0, "delta": base64::engine::general_purpose::STANDARD.encode(chunk) })
                );
            } else {
                emit!(
                    json!({ "type": "response.text.delta", "response_id": response_id, "item_id": item_id, "output_index": 0, "content_index": 0, "delta": piece })
                );
            }
        }
        let part = if audio {
            json!({ "type": "audio", "transcript": said })
        } else {
            json!({ "type": "text", "text": said })
        };
        if !cancelled {
            if audio {
                emit!(
                    json!({ "type": "response.audio.done", "response_id": response_id, "item_id": item_id, "output_index": 0, "content_index": 0 })
                );
                emit!(
                    json!({ "type": "response.audio_transcript.done", "response_id": response_id, "item_id": item_id, "output_index": 0, "content_index": 0, "transcript": said })
                );
            } else {
                emit!(
                    json!({ "type": "response.text.done", "response_id": response_id, "item_id": item_id, "output_index": 0, "content_index": 0, "text": said })
                );
            }
            emit!(
                json!({ "type": "response.content_part.done", "response_id": response_id, "item_id": item_id, "output_index": 0, "content_index": 0, "part": part })
            );
        }
        s.add_item(item_id.clone(), Msg::text(Role::Assistant, said.clone()));
        s.last_reply = said.clone();
        let _ = part_type;
        json!({ "id": item_id, "object": "realtime.item", "type": "message", "status": if cancelled { "incomplete" } else { "completed" }, "role": "assistant", "content": [part] })
    };
    if !cancelled {
        emit!(
            json!({ "type": "response.output_item.done", "response_id": response_id, "output_index": 0, "item": item })
        );
    }

    // Usage: the engine's counts, plus the audio both ways.
    let text_out = if cancelled {
        super::tokens::count(&said)
    } else {
        reply.completion_tokens
    };
    let input_tokens = reply.prompt_tokens + s.audio_in_tokens;
    let output_tokens = text_out + output_tokens_audio;
    let usage = json!({
        "total_tokens": input_tokens + output_tokens,
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "input_token_details": { "cached_tokens": 0, "text_tokens": reply.prompt_tokens, "audio_tokens": s.audio_in_tokens },
        "output_token_details": { "text_tokens": text_out, "audio_tokens": output_tokens_audio },
    });
    s.responses += 1;
    s.prompt_tokens += input_tokens;
    s.completion_tokens += output_tokens;
    s.last_mode = reply.mode.as_str().to_string();
    s.last_finish = if cancelled {
        "cancelled".into()
    } else {
        match reply.finish {
            Finish::ToolCalls => "tool_calls".into(),
            Finish::Length => "length".into(),
            Finish::ContentFilter => "content_filter".into(),
            _ => "stop".into(),
        }
    };
    let status = if cancelled { "cancelled" } else { "completed" };
    let done = s
        .emit(
            socket,
            json!({ "type": "response.done", "response": response(status, json!([item]), usage) }),
        )
        .await;
    // Recorded whatever the send did: the usage the client saw (or would
    // have) is the record's.
    s.record(input_tokens, output_tokens);
    if !matches!(done, Flow::Go) {
        return done;
    }
    emit!(json!({ "type": "rate_limits.updated", "rate_limits": [
        { "name": "requests", "limit": 1000, "remaining": 999, "reset_seconds": 60.0 },
        { "name": "tokens", "limit": 100_000, "remaining": 100_000u32.saturating_sub(input_tokens + output_tokens), "reset_seconds": 60.0 },
    ] }));
    Flow::Go
}

/// Handle one client event.
async fn handle(
    s: &mut Session,
    socket: &mut WebSocket,
    text: &str,
    pending: &mut VecDeque<String>,
) -> Flow {
    let event: Value = match serde_json::from_str(text) {
        Ok(v @ Value::Object(_)) => v,
        _ => {
            s.log("in", "(not JSON)", text.len(), None);
            return s
                .emit(
                    socket,
                    error_event(
                        "invalid_request_error",
                        Some("invalid_json"),
                        "Could not parse the event as a JSON object.",
                        None,
                    ),
                )
                .await;
        }
    };
    let kind = event["type"].as_str().unwrap_or("").to_string();
    let logged = event["item"]["content"][0]["text"]
        .as_str()
        .map(str::to_string);
    s.log("in", &kind, text.len(), logged.as_deref());
    let bad = |m: String| {
        error_event(
            "invalid_request_error",
            Some("invalid_value"),
            &m,
            Some(&event),
        )
    };
    match kind.as_str() {
        "session.update" => {
            let u = &event["session"];
            if let Some(i) = u["instructions"].as_str() {
                s.instructions = i.chars().take(32 * 1024).collect();
            }
            if u.get("tools").is_some() {
                s.tools = openai::parse_tools(u);
                s.tools.truncate(64);
                s.tools_json = u["tools"].clone();
            }
            if u.get("tool_choice").is_some() {
                s.tool_choice = openai::parse_tool_choice(u);
                s.tool_choice_json = u["tool_choice"].clone();
            }
            if let Some(m) = u["modalities"].as_array() {
                s.audio_out = m.iter().any(|x| x == "audio");
            }
            if let Some(v) = u["voice"].as_str() {
                s.voice = v.chars().take(64).collect();
            }
            for (key, slot) in [("input_audio_format", 0), ("output_audio_format", 1)] {
                if let Some(raw) = u[key].as_str() {
                    match AudioFormat::parse(raw) {
                        Some(f) if slot == 0 => s.input_format = f,
                        Some(f) => s.output_format = f,
                        None => return s.emit(socket, bad(format!("Invalid value: '{raw}'. Supported values are: 'pcm16', 'g711_ulaw' and 'g711_alaw'."))).await,
                    }
                }
            }
            match &u["max_response_output_tokens"] {
                Value::Number(n) => s.max_output_tokens = n.as_u64().map(|n| n.min(4096) as u32),
                Value::String(i) if i == "inf" => s.max_output_tokens = None,
                _ => {}
            }
            // The mock's own pacing, as the headers of the other AI paths.
            let max_delay = s.ctx.shared.config.max_delay_ms();
            if let Some(ms) = u["rustybin"]["ttft_ms"].as_u64() {
                s.pace.ttft = Duration::from_millis(ms.min(max_delay));
            }
            if let Some(tps) = u["rustybin"]["tokens_per_second"].as_u64() {
                s.pace.tps = tps.min(100_000) as u32;
            }
            let session = s.session_json();
            s.emit(
                socket,
                json!({ "type": "session.updated", "session": session }),
            )
            .await
        }
        "input_audio_buffer.append" => {
            // Counted, never decoded or kept.
            let bytes = event["audio"].as_str().map_or(0, |a| a.len() * 3 / 4);
            s.buffered = s.buffered.saturating_add(bytes).min(64 * 1024 * 1024);
            Flow::Go
        }
        "input_audio_buffer.clear" => {
            s.buffered = 0;
            s.emit(socket, json!({ "type": "input_audio_buffer.cleared" }))
                .await
        }
        "input_audio_buffer.commit" => {
            if s.buffered == 0 {
                return s
                    .emit(
                        socket,
                        error_event(
                            "invalid_request_error",
                            Some("input_audio_buffer_commit_empty"),
                            "Error committing input audio buffer: the buffer is empty.",
                            Some(&event),
                        ),
                    )
                    .await;
            }
            let ms =
                (s.buffered as u64).saturating_mul(1000) / s.input_format.bytes_per_second() as u64;
            s.buffered = 0;
            s.audio_in_tokens = s.audio_in_tokens.saturating_add(audio_tokens(ms));
            let item_id = format!("item_{}", rand_id(21));
            let previous = s.items.back().map(|(i, _)| i.clone());
            s.add_item(
                item_id.clone(),
                Msg {
                    role: Role::User,
                    parts: vec![Part::Audio(format!(
                        "{} of {ms} ms",
                        s.input_format.as_str()
                    ))],
                },
            );
            match s.emit(socket, json!({ "type": "input_audio_buffer.committed", "previous_item_id": previous, "item_id": item_id })).await {
                Flow::Go => {}
                other => return other,
            }
            let item = json!({ "id": item_id, "object": "realtime.item", "type": "message", "status": "completed", "role": "user", "content": [{ "type": "input_audio", "transcript": null }] });
            s.emit(socket, json!({ "type": "conversation.item.created", "previous_item_id": previous, "item": item })).await
        }
        "conversation.item.create" => match parse_item(&event["item"]) {
            Ok((msg, mut item)) => {
                let id = event["item"]["id"]
                    .as_str()
                    .filter(|i| !i.is_empty() && i.len() <= 64)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("item_{}", rand_id(21)));
                item["id"] = json!(id);
                let previous = s.items.back().map(|(i, _)| i.clone());
                s.add_item(id, msg);
                s.emit(socket, json!({ "type": "conversation.item.created", "previous_item_id": previous, "item": item })).await
            }
            Err(m) => s.emit(socket, bad(m)).await,
        },
        "response.create" => respond(s, socket, &event["response"], pending).await,
        "response.cancel" => {
            s.emit(
                socket,
                error_event(
                    "invalid_request_error",
                    Some("response_cancel_not_active"),
                    "Cancellation failed: no active response found.",
                    Some(&event),
                ),
            )
            .await
        }
        other => {
            let m = format!("Invalid value: '{other}'. Supported values are: 'session.update', 'input_audio_buffer.append', 'input_audio_buffer.commit', 'input_audio_buffer.clear', 'conversation.item.create', 'response.create' and 'response.cancel'.");
            s.emit(socket, bad(m)).await
        }
    }
}

async fn run(mut s: Session, mut socket: WebSocket, lifetime: Duration) {
    let deadline = tokio::time::Instant::now() + lifetime;
    let created = json!({ "type": "session.created", "session": s.session_json() });
    let mut flow = s.emit(&mut socket, created).await;
    let mut pending: VecDeque<String> = VecDeque::new();
    while matches!(flow, Flow::Go) {
        let text = match pending.pop_front() {
            Some(t) => t,
            None => {
                let next = tokio::select! {
                    m = tokio::time::timeout(IDLE, socket.recv()) => m,
                    _ = tokio::time::sleep_until(deadline) => {
                        let e = error_event("invalid_request_error", Some("session_expired"), "Your session hit the maximum duration.", None);
                        let _ = s.emit(&mut socket, e).await;
                        break;
                    }
                };
                match next {
                    Ok(Some(Ok(Message::Text(t)))) => t.to_string(),
                    Ok(Some(Ok(Message::Binary(_)))) => {
                        flow = s
                            .emit(
                                &mut socket,
                                error_event(
                                    "invalid_request_error",
                                    Some("invalid_event"),
                                    "Events are JSON text frames.",
                                    None,
                                ),
                            )
                            .await;
                        continue;
                    }
                    Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
                    Ok(Some(Ok(_))) => continue,
                }
            }
        };
        flow = handle(&mut s, &mut socket, &text, &mut pending).await;
    }
    // A session nobody asked a response of is recorded too.
    s.record(0, 0);
    if matches!(flow, Flow::Drop) {
        // A fault: the connection ends without a close handshake.
        drop(socket);
        return;
    }
    let frame = CloseFrame {
        code: axum::extract::ws::close_code::NORMAL,
        reason: "".into(),
    };
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        socket.send(Message::Close(Some(frame))),
    )
    .await;
}

async fn realtime(
    ws: WebSocketUpgrade,
    Extension(ctx): Extension<AiCtx>,
    tracker: Option<Extension<crate::metrics::StreamTracker>>,
) -> Response {
    let max = super::setting_usize(&ctx.shared, MAX_SESSIONS_KEY, DEFAULT_MAX_SESSIONS);
    let Some(slot) = ctx.shared.realtime.take(max) else {
        return ctx.error(
            ErrorKind::RateLimit,
            format!("Too many realtime sessions are open on this instance (at most {max}): close one and try again."),
        );
    };
    let minutes = super::setting_usize(&ctx.shared, SESSION_MINUTES_KEY, DEFAULT_SESSION_MINUTES);
    let q = &ctx.query;
    let max_delay = ctx.shared.config.max_delay_ms();
    let mut pace = ctx.pace;
    if let Some(ms) = q.get("ttft_ms").and_then(|v| v.parse::<u64>().ok()) {
        pace.ttft = Duration::from_millis(ms.min(max_delay));
    }
    if let Some(tps) = q
        .get("tokens_per_second")
        .and_then(|v| v.parse::<u32>().ok())
    {
        pace.tps = tps.min(100_000);
    }
    let session = Session {
        id: format!("sess_{}", rand_id(21)),
        model: q
            .get("model")
            .filter(|m| !m.is_empty() && m.len() <= 128)
            .cloned()
            .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        instructions: String::new(),
        tools: Vec::new(),
        tool_choice: engine::ToolChoice::Auto,
        tools_json: json!([]),
        tool_choice_json: json!("auto"),
        audio_out: false,
        voice: "alloy".into(),
        input_format: AudioFormat::Pcm16,
        output_format: AudioFormat::Pcm16,
        max_output_tokens: None,
        tone: q.get("audio").is_some_and(|a| a == "tone"),
        pace,
        items: VecDeque::new(),
        buffered: 0,
        audio_in_tokens: 0,
        sent: 0,
        fault: parse_fault(q.get("fail")),
        log: Vec::new(),
        logged_out: 0,
        prompt_tokens: 0,
        completion_tokens: 0,
        responses: 0,
        last_reply: String::new(),
        last_mode: String::new(),
        last_finish: String::new(),
        recorded: false,
        ctx,
    };
    ws.max_message_size(MAX_EVENT_BYTES)
        .max_frame_size(MAX_EVENT_BYTES)
        .on_upgrade(move |socket| async move {
            let _open = crate::metrics::open_ws(&tracker);
            let _slot = slot;
            run(session, socket, Duration::from_secs(minutes as u64 * 60)).await
        })
}

pub const MAX_SESSIONS_KEY: &str = "ai.realtime.max_sessions";
pub const SESSION_MINUTES_KEY: &str = "ai.realtime.session_minutes";
pub const DEFAULT_MAX_SESSIONS: usize = 64;
pub const DEFAULT_SESSION_MINUTES: usize = 30;

pub fn routes() -> Router<AppState> {
    Router::new().route(PATH, get(realtime))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![Endpoint::new(
        PATH,
        &["GET"],
        category::AI_OPENAI,
        "Realtime API over WebSocket: session, conversation and response events both ways, text and audio",
    )
    .description(
        "Upgrade with ?model=. Client events: session.update, input_audio_buffer.append | commit | clear, \
         conversation.item.create, response.create, response.cancel. Answers come from the engine of the \
         other AI paths (modes, scripted rules, tool calls from the session's tools). Input audio is counted \
         and never kept; output audio is silence (?audio=tone for a tone) in pcm16 or G.711 with the text as \
         its transcript. Pace with ?ttft_ms= and ?tokens_per_second= (or X-Rustybin-TTFT-Ms and \
         X-Rustybin-Tokens-Per-Second); ?fail=429 refuses the upgrade, ?fail=drop_after:N drops the \
         connection after N server events, ?fail=error_after:N sends an error event then.",
    )
    .websocket()]
}

pub fn openapi_paths() -> Value {
    json!({
        PATH: { "get": {
            "tags": ["AI: OpenAI-compatible"],
            "summary": "Realtime API (WebSocket)",
            "description": "The OpenAI-shaped realtime protocol over a WebSocket: JSON events both ways. Client events session.update, input_audio_buffer.append, commit and clear, conversation.item.create, response.create, response.cancel; server events session.created and updated, input_audio_buffer.committed and cleared, conversation.item.created, response.created, response.output_item.added and done, response.content_part.added and done, response.text.delta and done, response.audio.delta and done, response.audio_transcript.delta and done, response.function_call_arguments.delta and done, response.done (with usage), rate_limits.updated, error. At most ai.realtime.max_sessions sessions (64) of ai.realtime.session_minutes (30) each; a client event is at most 512 KiB. Audio is never decoded or kept.",
            "operationId": "aiOpenAiRealtime",
            "parameters": [
                { "name": "model", "in": "query", "required": false, "schema": { "type": "string" } },
                { "name": "ttft_ms", "in": "query", "required": false, "schema": { "type": "integer" } },
                { "name": "tokens_per_second", "in": "query", "required": false, "schema": { "type": "integer" } },
                { "name": "audio", "in": "query", "required": false, "schema": { "type": "string", "enum": ["silence", "tone"] } },
                { "name": "fail", "in": "query", "required": false, "description": "A fail kind refuses the upgrade (429, 503, ...); drop_after:N and error_after:N act after N server events", "schema": { "type": "string" } }
            ],
            "responses": { "101": { "description": "Switching protocols" }, "429": { "description": "Too many sessions, or a simulated rate limit" } }
        } }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as TMessage;

    type Client = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    /// The whole application on a real port (a WebSocket needs one).
    async fn serve(state: crate::state::AppState) -> String {
        let app: Router = crate::build_app(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await;
        });
        format!("{addr}")
    }

    async fn connect(addr: &str, query: &str) -> Client {
        tokio_tungstenite::connect_async(format!("ws://{addr}{PATH}{query}"))
            .await
            .expect("upgrade")
            .0
    }

    /// The next server event (`None`: the connection ended).
    async fn next(socket: &mut Client) -> Option<Value> {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .expect("an event in time")?;
            match msg {
                Ok(TMessage::Text(t)) => return serde_json::from_str(&t).ok(),
                Ok(TMessage::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    async fn say(socket: &mut Client, event: Value) {
        socket
            .send(TMessage::Text(event.to_string()))
            .await
            .expect("send");
    }

    /// Server events up to and with the first of `last`.
    async fn until(socket: &mut Client, last: &str) -> Vec<Value> {
        let mut out = Vec::new();
        while let Some(e) = next(socket).await {
            let done = e["type"] == last;
            out.push(e);
            if done {
                return out;
            }
        }
        panic!("the connection ended before {last}: {out:?}");
    }

    fn types(events: &[Value]) -> Vec<&str> {
        events.iter().filter_map(|e| e["type"].as_str()).collect()
    }

    async fn record(addr: &str, id: &str) -> Value {
        reqwest::get(format!("http://{addr}/ai/requests/{id}"))
            .await
            .expect("get")
            .json()
            .await
            .expect("json")
    }

    #[tokio::test]
    async fn a_session_answers_in_text_deltas_with_the_usage_the_log_has() {
        let addr = serve(crate::test_support::test_state()).await;
        let mut req =
            tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
                format!("ws://{addr}{PATH}?model=rustybin-echo&tokens_per_second=0"),
            )
            .expect("request");
        req.headers_mut()
            .insert("x-request-id", "rt-one".parse().expect("header"));
        let (mut socket, resp) = tokio_tungstenite::connect_async(req)
            .await
            .expect("upgrade");
        assert_eq!(resp.headers()["x-rustybin-provider"], "openai");
        let created = next(&mut socket).await.expect("session.created");
        assert_eq!(created["type"], "session.created");
        assert_eq!(created["session"]["model"], "rustybin-echo");
        assert_eq!(created["session"]["modalities"], json!(["text"]));
        assert!(created["event_id"]
            .as_str()
            .expect("id")
            .starts_with("event_"));

        say(
            &mut socket,
            json!({ "type": "session.update", "session": { "instructions": "Be terse." } }),
        )
        .await;
        let updated = next(&mut socket).await.expect("session.updated");
        assert_eq!(updated["session"]["instructions"], "Be terse.");
        say(&mut socket, json!({ "type": "conversation.item.create", "item": { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "What did you receive?" }] } })).await;
        let item = next(&mut socket).await.expect("item");
        assert_eq!(item["type"], "conversation.item.created");
        assert_eq!(item["item"]["content"][0]["text"], "What did you receive?");

        say(&mut socket, json!({ "type": "response.create" })).await;
        let events = until(&mut socket, "rate_limits.updated").await;
        let seen = types(&events);
        assert_eq!(seen[0], "response.created");
        assert_eq!(
            &seen[1..4],
            [
                "response.output_item.added",
                "conversation.item.created",
                "response.content_part.added"
            ]
        );
        let deltas: Vec<&str> = events
            .iter()
            .filter(|e| e["type"] == "response.text.delta")
            .filter_map(|e| e["delta"].as_str())
            .collect();
        assert!(deltas.len() > 3, "{seen:?}");
        let done = events
            .iter()
            .find(|e| e["type"] == "response.text.done")
            .expect("text.done");
        // The deltas, in order, are the text; echo mode shows what arrived.
        assert_eq!(deltas.concat(), done["text"].as_str().expect("text"));
        assert!(
            done["text"].as_str().expect("t").contains("Be terse."),
            "{done}"
        );
        let finished = events
            .iter()
            .find(|e| e["type"] == "response.done")
            .expect("response.done");
        assert_eq!(finished["response"]["status"], "completed");
        let usage = &finished["response"]["usage"];
        assert_eq!(
            usage["total_tokens"],
            usage["input_tokens"].as_u64().expect("in")
                + usage["output_tokens"].as_u64().expect("out")
        );
        assert!(usage["output_tokens"].as_u64().expect("out") > 0);

        // The request log has the session: the same usage, the events, no audio.
        let r = record(&addr, "rt-one").await;
        assert_eq!(r["kind"], "realtime", "{r}");
        assert_eq!(r["usage"]["prompt_tokens"], usage["input_tokens"]);
        assert_eq!(r["usage"]["completion_tokens"], usage["output_tokens"]);
        assert_eq!(r["model"], "rustybin-echo");
        let logged: Vec<&str> = r["detail"]["events"]
            .as_array()
            .expect("events")
            .iter()
            .filter_map(|e| e["type"].as_str())
            .collect();
        assert!(
            logged.contains(&"session.update") && logged.contains(&"response.done"),
            "{logged:?}"
        );
        let list: Value = reqwest::get(format!("http://{addr}/ai/requests?limit=5"))
            .await
            .expect("get")
            .json()
            .await
            .expect("json");
        assert_eq!(list["requests"][0]["kind"], "realtime");

        // What is not an event is answered with an error event, and the session goes on.
        say(&mut socket, json!({ "type": "response.fly" })).await;
        let e = next(&mut socket).await.expect("error");
        assert_eq!(e["type"], "error");
        assert_eq!(e["error"]["type"], "invalid_request_error");
        say(&mut socket, json!({ "type": "response.cancel" })).await;
        assert_eq!(
            next(&mut socket).await.expect("error")["error"]["code"],
            "response_cancel_not_active"
        );
        socket.close(None).await.expect("close");
    }

    #[tokio::test]
    async fn a_tool_of_the_session_is_called_with_valid_arguments() {
        let addr = serve(crate::test_support::test_state()).await;
        let mut socket = connect(&addr, "?tokens_per_second=0").await;
        next(&mut socket).await.expect("session.created");
        say(&mut socket, json!({ "type": "session.update", "session": { "tools": [{ "type": "function", "name": "get_weather", "description": "Get the current weather for a location", "parameters": { "type": "object", "properties": { "location": { "type": "string" }, "unit": { "type": "string", "enum": ["celsius", "fahrenheit"] } }, "required": ["location"] } }] } })).await;
        let updated = next(&mut socket).await.expect("session.updated");
        assert_eq!(updated["session"]["tools"][0]["name"], "get_weather");
        say(&mut socket, json!({ "type": "conversation.item.create", "item": { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "What is the weather in Paris?" }] } })).await;
        next(&mut socket).await.expect("item");
        say(&mut socket, json!({ "type": "response.create" })).await;
        let events = until(&mut socket, "response.done").await;
        let done = events
            .iter()
            .find(|e| e["type"] == "response.function_call_arguments.done")
            .expect("a function call");
        let args: Value = serde_json::from_str(done["arguments"].as_str().expect("arguments"))
            .expect("JSON arguments");
        assert!(args["location"].is_string(), "{args}");
        if let Some(u) = args.get("unit") {
            assert!(u == "celsius" || u == "fahrenheit", "{args}");
        }
        let out = &events.last().expect("done")["response"]["output"][0];
        assert_eq!(out["type"], "function_call");
        assert_eq!(out["name"], "get_weather");
        let call_id = out["call_id"].as_str().expect("call id").to_string();
        // The tool's result goes back, and the next response is text.
        until(&mut socket, "rate_limits.updated").await;
        say(&mut socket, json!({ "type": "conversation.item.create", "item": { "type": "function_call_output", "call_id": call_id, "output": "{\"temperature\": 18}" } })).await;
        assert_eq!(
            next(&mut socket).await.expect("item")["item"]["type"],
            "function_call_output"
        );
        say(&mut socket, json!({ "type": "response.create" })).await;
        let events = until(&mut socket, "response.done").await;
        assert_eq!(
            events.last().expect("done")["response"]["output"][0]["type"],
            "message"
        );
    }

    #[tokio::test]
    async fn audio_is_counted_never_kept_and_answered_in_the_sessions_format() {
        let addr = serve(crate::test_support::test_state()).await;
        let mut req =
            tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
                format!("ws://{addr}{PATH}?tokens_per_second=0&audio=tone"),
            )
            .expect("request");
        req.headers_mut()
            .insert("x-request-id", "rt-audio".parse().expect("header"));
        let (mut socket, _) = tokio_tungstenite::connect_async(req)
            .await
            .expect("upgrade");
        next(&mut socket).await.expect("session.created");
        say(&mut socket, json!({ "type": "session.update", "session": { "modalities": ["audio", "text"], "output_audio_format": "g711_ulaw" } })).await;
        assert_eq!(
            next(&mut socket).await.expect("updated")["session"]["output_audio_format"],
            "g711_ulaw"
        );
        // An empty buffer cannot be committed.
        say(&mut socket, json!({ "type": "input_audio_buffer.commit" })).await;
        assert_eq!(
            next(&mut socket).await.expect("error")["error"]["code"],
            "input_audio_buffer_commit_empty"
        );
        // Two seconds of PCM16 at 24 kHz: 96,000 bytes, twenty tokens.
        let secret_audio = base64::engine::general_purpose::STANDARD.encode(vec![7u8; 48_000]);
        for _ in 0..2 {
            say(
                &mut socket,
                json!({ "type": "input_audio_buffer.append", "audio": secret_audio }),
            )
            .await;
        }
        say(&mut socket, json!({ "type": "input_audio_buffer.commit" })).await;
        assert_eq!(
            next(&mut socket).await.expect("committed")["type"],
            "input_audio_buffer.committed"
        );
        assert_eq!(
            next(&mut socket).await.expect("item")["item"]["content"][0]["type"],
            "input_audio"
        );
        say(&mut socket, json!({ "type": "response.create" })).await;
        let events = until(&mut socket, "response.done").await;
        let audio: Vec<&Value> = events
            .iter()
            .filter(|e| e["type"] == "response.audio.delta")
            .collect();
        let transcript: String = events
            .iter()
            .filter(|e| e["type"] == "response.audio_transcript.delta")
            .filter_map(|e| e["delta"].as_str())
            .collect();
        assert!(!audio.is_empty() && !transcript.is_empty());
        // 100 ms of G.711 a delta: 800 bytes, and a tone is not all one byte.
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(audio[0]["delta"].as_str().expect("b64"))
            .expect("base64");
        assert_eq!(bytes.len(), 800);
        assert!(bytes.iter().any(|b| *b != bytes[0]));
        let usage = &events.last().expect("done")["response"]["usage"];
        assert_eq!(usage["input_token_details"]["audio_tokens"], 20);
        assert_eq!(
            usage["output_token_details"]["audio_tokens"],
            2 * audio.len()
        );
        let r = record(&addr, "rt-audio").await;
        assert_eq!(r["detail"]["audio"]["kept"], false);
        assert!(
            !r.to_string().contains(&secret_audio[..64]),
            "no audio in the record"
        );
        assert!(
            !r.to_string()
                .contains(audio[0]["delta"].as_str().expect("b64")),
            "no audio in the record"
        );
    }

    #[test]
    fn g711_encodes_silence_and_full_scale() {
        assert_eq!(ulaw(0), 0xff);
        assert_eq!(alaw(0), 0xd5);
        assert_eq!(ulaw(i16::MAX), 0x80);
        assert_eq!(ulaw(i16::MIN), 0x00);
        assert_eq!(alaw(i16::MAX), 0xaa);
        assert_eq!(audio_chunk(AudioFormat::Pcm16, false, 0), vec![0u8; 4800]);
        assert_eq!(audio_chunk(AudioFormat::Alaw, false, 3).len(), 800);
    }

    #[tokio::test]
    async fn faults_refuse_the_upgrade_drop_the_connection_or_send_an_error() {
        let addr = serve(crate::test_support::test_state()).await;
        // A fail kind on the upgrade: the provider's refusal, no session.
        let refused = tokio_tungstenite::connect_async(format!("ws://{addr}{PATH}?fail=429")).await;
        match refused {
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
                assert_eq!(resp.status(), 429);
                let body: Value =
                    serde_json::from_slice(resp.body().as_deref().unwrap_or_default())
                        .expect("json");
                assert_eq!(body["error"]["code"], "rate_limit_exceeded");
            }
            other => panic!("expected a 429, got {other:?}"),
        }
        // drop_after:3 closes after the third server event, without a close frame.
        let mut socket = connect(&addr, "?fail=drop_after:3&tokens_per_second=0").await;
        assert_eq!(
            next(&mut socket).await.expect("1")["type"],
            "session.created"
        );
        say(&mut socket, json!({ "type": "conversation.item.create", "item": { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hello" }] } })).await;
        assert_eq!(
            next(&mut socket).await.expect("2")["type"],
            "conversation.item.created"
        );
        say(&mut socket, json!({ "type": "response.create" })).await;
        assert_eq!(
            next(&mut socket).await.expect("3")["type"],
            "response.created"
        );
        assert!(
            next(&mut socket).await.is_none(),
            "the connection is gone after the third event"
        );
        // error_after:2 sends an error event then, and the session goes on.
        let mut socket = connect(&addr, "?fail=error_after:2&tokens_per_second=0").await;
        next(&mut socket).await.expect("session.created");
        say(&mut socket, json!({ "type": "input_audio_buffer.clear" })).await;
        assert_eq!(
            next(&mut socket).await.expect("2")["type"],
            "input_audio_buffer.cleared"
        );
        let e = next(&mut socket).await.expect("error");
        assert_eq!(e["error"]["type"], "server_error");
        say(&mut socket, json!({ "type": "input_audio_buffer.clear" })).await;
        assert_eq!(
            next(&mut socket).await.expect("on")["type"],
            "input_audio_buffer.cleared"
        );
    }

    #[tokio::test]
    async fn a_response_is_cancelled_while_it_is_paced() {
        let addr = serve(crate::test_support::test_state()).await;
        // Slow enough to cancel: a token every 200 ms.
        let mut socket = connect(&addr, "?tokens_per_second=5").await;
        next(&mut socket).await.expect("session.created");
        say(&mut socket, json!({ "type": "conversation.item.create", "item": { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hello" }] } })).await;
        next(&mut socket).await.expect("item");
        say(&mut socket, json!({ "type": "response.create" })).await;
        loop {
            let e = next(&mut socket).await.expect("an event");
            if e["type"] == "response.text.delta" {
                break;
            }
        }
        say(&mut socket, json!({ "type": "response.cancel" })).await;
        let events = until(&mut socket, "response.done").await;
        let done = events.last().expect("done");
        assert_eq!(done["response"]["status"], "cancelled");
        assert_eq!(done["response"]["output"][0]["status"], "incomplete");
        assert!(events.len() < 6, "it stopped: {:?}", types(&events));
    }

    #[tokio::test]
    async fn the_sessions_at_once_are_capped_by_the_setting() {
        let state = crate::test_support::test_state();
        state
            .settings
            .update(
                std::collections::BTreeMap::from([(MAX_SESSIONS_KEY.to_string(), json!(2))]),
                Default::default(),
                "test",
            )
            .expect("setting");
        let addr = serve(state).await;
        let mut first = connect(&addr, "").await;
        let _second = connect(&addr, "").await;
        next(&mut first).await.expect("session.created");
        // One more is refused with the provider's error.
        match tokio_tungstenite::connect_async(format!("ws://{addr}{PATH}")).await {
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
                assert_eq!(resp.status(), 429);
                let body: Value =
                    serde_json::from_slice(resp.body().as_deref().unwrap_or_default())
                        .expect("json");
                assert!(
                    body["error"]["message"]
                        .as_str()
                        .expect("m")
                        .contains("at most 2"),
                    "{body}"
                );
            }
            other => panic!("expected a 429, got {other:?}"),
        }
        // A session that ends frees its slot.
        first.close(None).await.expect("close");
        for _ in 0..50 {
            if tokio_tungstenite::connect_async(format!("ws://{addr}{PATH}"))
                .await
                .is_ok()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the slot was not freed");
    }

    #[test]
    fn the_default_cap_is_64_sessions() {
        let sessions = Arc::new(Sessions::default());
        let held: Vec<Slot> = (0..DEFAULT_MAX_SESSIONS)
            .map(|_| sessions.take(DEFAULT_MAX_SESSIONS).expect("a slot"))
            .collect();
        assert!(
            sessions.take(DEFAULT_MAX_SESSIONS).is_none(),
            "the 65th is refused"
        );
        drop(held);
        assert_eq!(sessions.open(), 0);
    }
}
