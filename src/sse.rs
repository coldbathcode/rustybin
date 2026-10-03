//! Generic Server-Sent Events endpoints.
//!
//! - `GET /sse`: a numbered event stream (`count`, `interval_ms`, `event`,
//!   `retry`, `mode=json|text`, `heartbeat_ms`). Event ids are the sequence
//!   numbers, so a reconnecting client sending `Last-Event-ID: n` resumes at
//!   `n + 1`. Comment heartbeats (`: heartbeat`) keep idle connections alive.
//! - `GET|POST /sse/chat`: a provider-neutral token stream that looks like a
//!   language model streaming text, ending with `data: [DONE]`.
//!
//! Every stream is capped in event count and lifetime (lower in public mode).

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::state::AppState;

/// Stream limits for one configuration.
#[derive(Clone, Copy, Debug)]
pub struct SseLimits {
    pub max_count: u64,
    pub max_interval_ms: u64,
    pub max_lifetime: Duration,
    pub max_chat_tokens: usize,
    pub max_chat_delay_ms: u64,
}

impl SseLimits {
    pub fn for_config(config: &Config) -> Self {
        if config.public_mode {
            Self {
                max_count: 100,
                max_interval_ms: 10_000,
                max_lifetime: Duration::from_secs(120),
                max_chat_tokens: 200,
                max_chat_delay_ms: 1_000,
            }
        } else {
            Self {
                max_count: 1_000,
                max_interval_ms: 60_000,
                max_lifetime: Duration::from_secs(600),
                max_chat_tokens: 500,
                max_chat_delay_ms: 2_000,
            }
        }
    }
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// Event names: 1..=64 chars of `[A-Za-z0-9_.:-]` (never a newline).
fn valid_event_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

fn sse_response<S>(stream: S) -> Response
where
    S: futures_util::Stream<Item = Result<Event, Infallible>> + Send + 'static,
{
    let mut resp = Sse::new(stream).into_response();
    // Ask intermediaries not to buffer the stream.
    resp.headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    resp
}

// ── /sse ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
struct SseQuery {
    count: Option<u64>,
    interval_ms: Option<u64>,
    event: Option<String>,
    retry: Option<u64>,
    mode: Option<String>,
    heartbeat_ms: Option<u64>,
    last_event_id: Option<String>,
}

/// Validated `/sse` parameters.
#[derive(Debug, PartialEq, Eq)]
struct SsePlan {
    first: u64,
    count: u64,
    interval: Duration,
    event: Option<String>,
    retry: Option<Duration>,
    text: bool,
    heartbeat: Option<Duration>,
    lifetime: Duration,
}

fn plan(q: SseQuery, last_event_id: Option<&str>, limits: &SseLimits) -> Result<SsePlan, String> {
    let count = q.count.unwrap_or(10);
    if count > limits.max_count {
        return Err(format!("count must be at most {}", limits.max_count));
    }
    let interval_ms = q.interval_ms.unwrap_or(1000);
    if interval_ms > limits.max_interval_ms {
        return Err(format!(
            "interval_ms must be at most {}",
            limits.max_interval_ms
        ));
    }
    if let Some(e) = &q.event {
        if !valid_event_name(e) {
            return Err("event must be 1-64 characters of [A-Za-z0-9_.:-]".into());
        }
    }
    let text = match q.mode.as_deref() {
        None | Some("json") => false,
        Some("text") => true,
        Some(_) => return Err("mode must be json or text".into()),
    };
    let heartbeat = match q.heartbeat_ms.unwrap_or(15_000) {
        0 => None,
        ms if ms < 100 => return Err("heartbeat_ms must be 0 (off) or at least 100".into()),
        ms => Some(Duration::from_millis(ms.min(60_000))),
    };
    // Resume: the header wins over the query parameter; unparsable ids restart.
    let resume = last_event_id
        .or(q.last_event_id.as_deref())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    Ok(SsePlan {
        first: resume.saturating_add(1),
        count,
        interval: Duration::from_millis(interval_ms),
        event: q.event,
        retry: q.retry.map(|ms| Duration::from_millis(ms.min(3_600_000))),
        text,
        heartbeat,
        lifetime: limits.max_lifetime,
    })
}

fn numbered_event(plan: &SsePlan, n: u64) -> Event {
    let mut ev = Event::default().id(n.to_string());
    if let Some(name) = &plan.event {
        ev = ev.event(name);
    }
    let message = format!("event {n} of {}", plan.count);
    ev = if plan.text {
        ev.data(message)
    } else {
        ev.data(
            json!({
                "id": n,
                "count": plan.count,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "message": message,
            })
            .to_string(),
        )
    };
    if n == plan.first {
        if let Some(retry) = plan.retry {
            ev = ev.retry(retry);
        }
    }
    ev
}

async fn sse_handler(
    State(config): State<Arc<Config>>,
    Query(q): Query<SseQuery>,
    headers: HeaderMap,
) -> Response {
    let limits = SseLimits::for_config(&config);
    let last = headers.get("last-event-id").and_then(|v| v.to_str().ok());
    let plan = match plan(q, last, &limits) {
        Ok(p) => p,
        Err(msg) => return json_error(StatusCode::BAD_REQUEST, &msg),
    };
    let stream = async_stream::stream! {
        let start = Instant::now();
        let deadline = start + plan.lifetime;
        let mut n = plan.first;
        if n > plan.count {
            yield Ok::<Event, Infallible>(Event::default().comment("nothing to resume: all events were delivered"));
        }
        let mut next_event = start;
        let mut next_heartbeat = plan.heartbeat.map(|h| start + h);
        while n <= plan.count {
            if let Some(hb) = next_heartbeat.filter(|hb| *hb < next_event) {
                if hb >= deadline {
                    break;
                }
                tokio::time::sleep_until(hb).await;
                yield Ok(Event::default().comment("heartbeat"));
                next_heartbeat = plan.heartbeat.map(|h| hb + h);
                continue;
            }
            if next_event >= deadline && next_event > start {
                break;
            }
            tokio::time::sleep_until(next_event).await;
            yield Ok(numbered_event(&plan, n));
            n += 1;
            let now = Instant::now();
            next_event = now + plan.interval;
            next_heartbeat = plan.heartbeat.map(|h| now + h);
        }
    };
    sse_response(stream)
}

// ── /sse/chat ───────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
struct ChatParams {
    prompt: Option<String>,
    max_tokens: Option<usize>,
    delay_ms: Option<u64>,
}

const CHAT_FILLER: &str = "This is a simulated streaming response. Each word arrives as its own \
server-sent event, the way language model APIs stream tokens, so you can test buffering, \
time-to-first-token, timeouts and token counting in front of a streaming backend. The stream \
ends with a final chunk carrying the finish reason and usage, followed by a [DONE] marker.";

/// Split text into word tokens that keep their trailing whitespace.
fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            in_space = true;
            current.push(c);
        } else {
            if in_space && !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            in_space = false;
            current.push(c);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn chat_text(prompt: &str) -> String {
    let prompt: String = prompt
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    let prompt = prompt.trim();
    if prompt.is_empty() {
        CHAT_FILLER.to_string()
    } else {
        format!("You said: \"{prompt}\". {CHAT_FILLER}")
    }
}

async fn chat_get(State(config): State<Arc<Config>>, Query(p): Query<ChatParams>) -> Response {
    chat_stream(&config, p)
}

async fn chat_post(
    State(config): State<Arc<Config>>,
    Query(q): Query<ChatParams>,
    body: Bytes,
) -> Response {
    let mut p: ChatParams = if body.iter().all(u8::is_ascii_whitespace) {
        ChatParams::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(p) => p,
            Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("invalid JSON: {e}")),
        }
    };
    p.prompt = p.prompt.or(q.prompt);
    p.max_tokens = p.max_tokens.or(q.max_tokens);
    p.delay_ms = p.delay_ms.or(q.delay_ms);
    chat_stream(&config, p)
}

fn chat_stream(config: &Config, p: ChatParams) -> Response {
    let limits = SseLimits::for_config(config);
    let delay_ms = p.delay_ms.unwrap_or(50);
    if delay_ms > limits.max_chat_delay_ms {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("delay_ms must be at most {}", limits.max_chat_delay_ms),
        );
    }
    let max_tokens = p
        .max_tokens
        .unwrap_or(limits.max_chat_tokens)
        .clamp(1, limits.max_chat_tokens);
    let prompt = p.prompt.unwrap_or_default();
    let mut tokens = tokenize(&chat_text(&prompt));
    let finish_reason = if tokens.len() > max_tokens {
        tokens.truncate(max_tokens);
        "length"
    } else {
        "stop"
    };
    let prompt_tokens = tokenize(&prompt).len();
    let id = format!("chat-{}", uuid::Uuid::new_v4().simple());
    let lifetime = limits.max_lifetime;
    let stream = async_stream::stream! {
        let deadline = Instant::now() + lifetime;
        let total = tokens.len();
        let mut sent = 0usize;
        for (index, token) in tokens.into_iter().enumerate() {
            if index > 0 {
                let next = Instant::now() + Duration::from_millis(delay_ms);
                if next >= deadline {
                    break;
                }
                tokio::time::sleep_until(next).await;
            }
            let chunk = json!({
                "id": id,
                "object": "chat.chunk",
                "index": index,
                "delta": token,
                "finish_reason": Value::Null,
            });
            sent += 1;
            yield Ok::<Event, Infallible>(Event::default().data(chunk.to_string()));
        }
        let reason = if sent < total { "length" } else { finish_reason };
        let last = json!({
            "id": id,
            "object": "chat.chunk",
            "index": sent,
            "delta": "",
            "finish_reason": reason,
            "usage": {
                "prompt_tokens": prompt_tokens,
                "completion_tokens": sent,
                "total_tokens": prompt_tokens + sent,
            },
        });
        yield Ok(Event::default().data(last.to_string()));
        yield Ok(Event::default().data("[DONE]"));
    };
    sse_response(stream)
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/sse", get(sse_handler))
        .route("/sse/chat", get(chat_get).post(chat_post))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/sse",
            &["GET"],
            category::STREAMING,
            "Numbered event stream with resume (Last-Event-ID) and heartbeats",
        )
        .description(
            "Query: count (default 10, max 1000; public 100), interval_ms (default 1000, max 60000; \
             public 10000), event (event name), retry (reconnection delay sent with the first event), \
             mode=json|text, heartbeat_ms (comment heartbeat when idle, default 15000, 0 disables), \
             last_event_id. Event ids are sequence numbers; Last-Event-ID: n resumes at n+1. \
             Streams end after 10 minutes (public 2 minutes).",
        )
        .sse()
        .example(Example::get(
            "Five events, 200 ms apart",
            "/sse?count=5&interval_ms=200",
        ))
        .example(
            Example::get("Resume after event 3", "/sse?count=5&interval_ms=100")
                .header("Last-Event-ID", "3"),
        )
        .example(Example::get(
            "Named text events with retry",
            "/sse?count=3&interval_ms=100&event=tick&mode=text&retry=5000",
        )),
        Endpoint::new(
            "/sse/chat",
            &["GET", "POST"],
            category::STREAMING,
            "Provider-neutral streamed chat text, one word per event, ending with [DONE]",
        )
        .description(
            "GET ?prompt=&max_tokens=&delay_ms= or POST {\"prompt\", \"max_tokens\", \"delay_ms\"}. \
             Each event is `data: {\"id\",\"object\":\"chat.chunk\",\"index\",\"delta\",\"finish_reason\"}`; \
             the last chunk carries finish_reason (stop or length) and usage, then `data: [DONE]`. \
             delay_ms defaults to 50 (max 2000; public 1000), max_tokens to 500 (public 200).",
        )
        .sse()
        .example(Example::get(
            "Stream a reply",
            "/sse/chat?prompt=hello&delay_ms=20",
        ))
        .example(
            Example::post("Stream a reply (POST)", "/sse/chat")
                .json(r#"{"prompt":"Tell me a story","max_tokens":20,"delay_ms":20}"#),
        ),
    ]
}

pub fn openapi_paths() -> Value {
    let q = |name: &str, ty: &str, desc: &str| json!({ "name": name, "in": "query", "required": false, "schema": { "type": ty }, "description": desc });
    let stream =
        json!({ "description": "text/event-stream", "content": { "text/event-stream": {} } });
    json!({
        "/sse": {
            "get": {
                "tags": ["Streaming"],
                "summary": "Numbered Server-Sent Events stream",
                "operationId": "sseStream",
                "parameters": [
                    q("count", "integer", "Number of events (default 10)"),
                    q("interval_ms", "integer", "Delay between events (default 1000)"),
                    q("event", "string", "Event name"),
                    q("retry", "integer", "Reconnection delay sent with the first event"),
                    q("mode", "string", "json (default) or text"),
                    q("heartbeat_ms", "integer", "Comment heartbeat interval (default 15000, 0 disables)"),
                    q("last_event_id", "string", "Resume after this id (Last-Event-ID header wins)"),
                    { "name": "Last-Event-ID", "in": "header", "required": false, "schema": { "type": "string" } }
                ],
                "responses": { "200": stream, "400": { "description": "Invalid parameters" } }
            }
        },
        "/sse/chat": {
            "get": {
                "tags": ["Streaming"],
                "summary": "Streamed chat-style text",
                "operationId": "sseChat",
                "parameters": [
                    q("prompt", "string", "Prompt echoed in the reply"),
                    q("max_tokens", "integer", "Maximum number of word tokens"),
                    q("delay_ms", "integer", "Delay between tokens (default 50)")
                ],
                "responses": { "200": stream, "400": { "description": "Invalid parameters" } }
            },
            "post": {
                "tags": ["Streaming"],
                "summary": "Streamed chat-style text",
                "operationId": "sseChatPost",
                "requestBody": { "required": false, "content": { "application/json": { "schema": {
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string" },
                        "max_tokens": { "type": "integer" },
                        "delay_ms": { "type": "integer" }
                    }
                } } } },
                "responses": { "200": stream, "400": { "description": "Invalid parameters" } }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_string, get_request, json_request, module_app};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn ids(text: &str) -> Vec<u64> {
        text.lines()
            .filter_map(|l| l.strip_prefix("id: "))
            .filter_map(|v| v.parse().ok())
            .collect()
    }

    #[tokio::test]
    async fn numbered_stream() {
        let app = module_app(router);
        let resp = app
            .oneshot(get_request("/sse?count=3&interval_ms=0&retry=1500"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "text/event-stream");
        let text = body_string(resp).await;
        assert_eq!(ids(&text), vec![1, 2, 3]);
        assert_eq!(text.matches("retry: 1500").count(), 1);
        assert!(text.contains("\"message\":\"event 2 of 3\""), "{text}");
    }

    #[tokio::test]
    async fn resume_with_last_event_id() {
        let app = module_app(router);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sse?count=5&interval_ms=0&mode=text&event=tick")
                    .header("last-event-id", "3")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let text = body_string(resp).await;
        assert_eq!(ids(&text), vec![4, 5]);
        assert!(text.contains("event: tick\n"));
        assert!(text.contains("data: event 5 of 5\n"));
        // Query parameter works too; an id past the end yields no events.
        let text = body_string(
            app.oneshot(get_request("/sse?count=2&interval_ms=0&last_event_id=7"))
                .await
                .expect("response"),
        )
        .await;
        assert!(ids(&text).is_empty());
    }

    #[tokio::test]
    async fn heartbeats_are_comments() {
        let app = module_app(router);
        let resp = app
            .oneshot(get_request("/sse?count=2&interval_ms=350&heartbeat_ms=100"))
            .await
            .expect("response");
        let text = body_string(resp).await;
        assert!(text.matches(": heartbeat").count() >= 2, "{text}");
        assert_eq!(ids(&text), vec![1, 2]);
    }

    #[tokio::test]
    async fn caps_and_validation() {
        let app = module_app(router);
        for uri in [
            "/sse?count=1001",
            "/sse?interval_ms=60001",
            "/sse?event=bad%0Aname",
            "/sse?mode=xml",
            "/sse?heartbeat_ms=5",
            "/sse/chat?delay_ms=5000",
        ] {
            let resp = app
                .clone()
                .oneshot(get_request(uri))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
        }
        let mut config = Config::for_tests();
        config.public_mode = true;
        let app = crate::test_support::module_app_with_config(config, router);
        let resp = app
            .oneshot(get_request("/sse?count=101"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn chat_stream_ends_with_done() {
        let app = module_app(router);
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/sse/chat",
                &json!({"prompt": "hi there", "max_tokens": 4, "delay_ms": 0}),
            ))
            .await
            .expect("response");
        let text = body_string(resp).await;
        let data: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .collect();
        assert_eq!(data.last(), Some(&"[DONE]"));
        assert_eq!(data.len(), 6);
        let first: Value = serde_json::from_str(data[0]).expect("json");
        assert_eq!(first["delta"], "You ");
        let last: Value = serde_json::from_str(data[4]).expect("json");
        assert_eq!(last["finish_reason"], "length");
        assert_eq!(last["usage"]["completion_tokens"], 4);
        assert_eq!(last["usage"]["prompt_tokens"], 2);

        let text = body_string(
            app.oneshot(get_request("/sse/chat?delay_ms=0"))
                .await
                .expect("response"),
        )
        .await;
        assert!(text.contains("\"finish_reason\":\"stop\""));
    }

    #[test]
    fn tokenizer_keeps_whitespace() {
        assert_eq!(tokenize("a  b c"), vec!["a  ", "b ", "c"]);
        assert_eq!(tokenize(" x"), vec![" ", "x"]);
        assert!(tokenize("").is_empty());
    }
}
