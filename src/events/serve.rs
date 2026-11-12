//! The handlers of the event mocks: a channel's data plane (WebSocket,
//! server-sent events, a client's `POST`) and the thin control routes over
//! the service functions of [`super`].

use std::convert::Infallible;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

use super::{channel_of, settings_of, Frame, Kind, Listener, MAX_MESSAGE_BYTES};
use crate::mocks::asyncapi::{self, Channel, Verdict};
use crate::mocks::capture::{CaptureLog, Sender};
use crate::mocks::compile::Compiled;
use crate::mocks::restrict;
use crate::mocks::service::{ServiceError, Svc};
use crate::mocks::store::Lookup;
use crate::session::ClientIp;
use crate::state::AppState;

/// Messages one WebSocket connection may publish a second.
const MAX_PER_SECOND: u32 = 50;
const SEND_LIMIT: Duration = Duration::from_secs(10);

fn answer(status: StatusCode, body: Value, visitor: bool) -> Response {
    let mut resp = (status, Json(body)).into_response();
    if visitor {
        restrict::harden(&mut resp);
    }
    resp
}

/// The error a client gets for a message that is not valid.
fn invalid(v: &Verdict) -> Value {
    json!({
        "error": {
            "type": "invalid_message",
            "message": "the message does not match what this channel receives",
            "issues": v.issues,
        }
    })
}

/// A mock's channel, ready to serve.
struct Target {
    mock: String,
    compiled: Arc<Compiled>,
    log: CaptureLog,
    visitor: bool,
}

impl Target {
    fn channel(&self, id: &str) -> Option<&Channel> {
        channel_of(&self.compiled, id)
    }
}

/// Find the mock and the channel, as the mock's own data plane would:
/// a missing, undeployed or disabled mock and a visitor's mock off its
/// domain answer what `/mock/{name}` answers.
#[allow(clippy::result_large_err)]
fn target(
    state: &AppState,
    name: &str,
    channel: &str,
    via_mocks_domain: bool,
) -> Result<Target, Response> {
    let live = match state.mocks.lookup(name) {
        Lookup::Missing => {
            return Err(answer(
                StatusCode::NOT_FOUND,
                json!({ "error": "mock not found", "mock": name }),
                false,
            ))
        }
        Lookup::NotDeployed(..) => {
            return Err(answer(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({ "error": "this mock has not been deployed yet", "mock": name }),
                false,
            ))
        }
        Lookup::Ready(live) => live,
    };
    let visitor = live.visitor;
    if visitor && !via_mocks_domain {
        if let Some(domain) = state.config.self_service.host.as_deref() {
            return Err(answer(
                StatusCode::NOT_FOUND,
                json!({
                    "error": "this self-service mock answers on its own domain",
                    "mock": name,
                    "url": format!("https://{name}.{domain}/_rustybin/events/{channel}"),
                }),
                true,
            ));
        }
    }
    let Some(events) = live.compiled.spec.events.as_deref() else {
        return Err(answer(
            StatusCode::NOT_FOUND,
            json!({
                "error": "this mock is not an AsyncAPI mock: it has operations, not channels",
                "mock": name,
            }),
            visitor,
        ));
    };
    if !live.enabled {
        let status = StatusCode::from_u16(live.compiled.settings.responses.disabled_status)
            .unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
        return Err(answer(
            status,
            json!({ "error": "this mock is turned off", "mock": name }),
            visitor,
        ));
    }
    if events.channel(channel).is_none() {
        let known: Vec<&str> = events.channels.iter().map(|c| c.id.as_str()).collect();
        return Err(answer(
            StatusCode::NOT_FOUND,
            json!({ "error": "the mock has no such channel", "mock": name, "channel": channel, "channels": known }),
            visitor,
        ));
    }
    Ok(Target {
        mock: name.to_string(),
        compiled: live.compiled,
        log: live.capture,
        visitor,
    })
}

/// Who publishes: the API key in shared mode, else the session.
fn sender_of(
    headers: &HeaderMap,
    ip: Option<std::net::IpAddr>,
    key: Option<&Extension<crate::keys::KeyContext>>,
) -> Option<Sender> {
    if let Some(k) = key.filter(|k| k.0.shared) {
        return Some(Sender::Key(k.0.key.id.to_string()));
    }
    crate::session::session_header(headers)
        .or_else(|| ip.map(|ip| format!("ip:{ip}")))
        .map(Sender::Session)
}

/// `/events/{name}/{channel}` and `/mock/{name}/_rustybin/events/{channel}`.
#[allow(clippy::too_many_arguments)]
pub async fn channel(
    State(state): State<AppState>,
    Path((name, channel)): Path<(String, String)>,
    ClientIp(ip): ClientIp,
    via: Option<Extension<crate::mocks::host::ViaMocksDomain>>,
    key: Option<Extension<crate::keys::KeyContext>>,
    tracker: Option<Extension<crate::metrics::StreamTracker>>,
    method: Method,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
    body: Bytes,
) -> Response {
    let t = match target(&state, &name, &channel, via.is_some()) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    let sender = sender_of(&headers, ip, key.as_ref());
    if method == Method::POST {
        return published(&state, &t, &channel, &body, sender).await;
    }
    if method != Method::GET {
        let mut resp = answer(
            StatusCode::METHOD_NOT_ALLOWED,
            json!({ "error": "a channel answers GET (WebSocket, server-sent events) and POST" }),
            t.visitor,
        );
        resp.headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET, POST"));
        return resp;
    }
    let wants_sse = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/event-stream"));
    if let Ok(ws) = ws {
        let listener = match state
            .events
            .listen(&state, &name, &channel, &t.log, Kind::Ws)
        {
            Ok(l) => l,
            Err((status, message)) => {
                return answer(status, json!({ "error": message }), t.visitor)
            }
        };
        let lifetime = state.events.session();
        return ws
            .max_message_size(MAX_MESSAGE_BYTES)
            .max_frame_size(MAX_MESSAGE_BYTES)
            .on_upgrade(move |socket| async move {
                let _open = crate::metrics::open_ws(&tracker);
                run_ws(state, t, channel, listener, socket, sender, lifetime).await
            });
    }
    if wants_sse {
        let listener = match state
            .events
            .listen(&state, &name, &channel, &t.log, Kind::Sse)
        {
            Ok(l) => l,
            Err((status, message)) => {
                return answer(status, json!({ "error": message }), t.visitor)
            }
        };
        return sse(listener, state.events.session(), t.visitor);
    }
    described(&t, &channel)
}

/// A plain `GET`: what the channel is and how to use it.
fn described(t: &Target, channel: &str) -> Response {
    let Some(ch) = t.channel(channel) else {
        return answer(
            StatusCode::NOT_FOUND,
            json!({ "error": "no such channel" }),
            t.visitor,
        );
    };
    let (pattern, cfg) = settings_of(&t.compiled, ch);
    let mut v = asyncapi::channel_json(ch);
    if let Value::Object(m) = &mut v {
        m.insert("mock".into(), json!(t.mock));
        m.insert("pattern".into(), json!(pattern.as_str()));
        m.insert("every_ms".into(), json!(cfg.every_ms));
        m.insert(
            "how".into(),
            json!("connect a WebSocket, GET with Accept: text/event-stream, or POST a message"),
        );
    }
    answer(StatusCode::OK, v, t.visitor)
}

/// A client's `POST`: one message, `202` when valid, `422` with the issues.
async fn published(
    state: &AppState,
    t: &Target,
    channel: &str,
    body: &Bytes,
    sender: Option<Sender>,
) -> Response {
    let Some(ch) = t.channel(channel) else {
        return answer(
            StatusCode::NOT_FOUND,
            json!({ "error": "no such channel" }),
            t.visitor,
        );
    };
    let text = String::from_utf8_lossy(body);
    let (verdict, reply) = state
        .events
        .receive(
            state,
            &t.mock,
            &t.compiled,
            &t.log,
            ch,
            &text,
            "http",
            sender,
        )
        .await;
    if !verdict.valid() {
        return answer(
            StatusCode::UNPROCESSABLE_ENTITY,
            invalid(&verdict),
            t.visitor,
        );
    }
    let mut out = json!({ "accepted": true, "message": verdict.message });
    if let Some(r) = reply {
        out["reply"] = json!({ "message": r.message, "payload": r.payload });
    }
    answer(StatusCode::ACCEPTED, out, t.visitor)
}

async fn send_text(socket: &mut WebSocket, text: String) -> bool {
    matches!(
        tokio::time::timeout(SEND_LIMIT, socket.send(Message::Text(text.into()))).await,
        Ok(Ok(()))
    )
}

async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        socket.send(Message::Close(Some(frame))),
    )
    .await;
}

/// One WebSocket connection: the channel's messages out, the client's in.
async fn run_ws(
    state: AppState,
    t: Target,
    channel: String,
    mut listener: Listener,
    mut socket: WebSocket,
    sender: Option<Sender>,
    lifetime: Duration,
) {
    use axum::extract::ws::close_code;
    let deadline = tokio::time::Instant::now() + lifetime;
    let mut window = tokio::time::Instant::now();
    let mut in_window = 0u32;
    loop {
        tokio::select! {
            frame = listener.rx.recv() => match frame {
                Ok(f) => match &*f {
                    Frame::Message { text, .. } => {
                        if !send_text(&mut socket, text.clone()).await {
                            return;
                        }
                    }
                    Frame::Closed => {
                        return close(&mut socket, close_code::AWAY, "the mock is gone or turned off").await;
                    }
                },
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return close(&mut socket, close_code::AWAY, "").await,
            },
            got = socket.recv() => {
                let text = match got {
                    Some(Ok(Message::Text(text))) => text.to_string(),
                    Some(Ok(Message::Binary(b))) => String::from_utf8_lossy(&b).into_owned(),
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                    Some(Ok(_)) => continue,
                };
                if window.elapsed() >= Duration::from_secs(1) {
                    window = tokio::time::Instant::now();
                    in_window = 0;
                }
                in_window += 1;
                if in_window > MAX_PER_SECOND {
                    let e = json!({ "error": { "type": "rate_limited", "message": format!("at most {MAX_PER_SECOND} messages a second on one connection") } });
                    if !send_text(&mut socket, e.to_string()).await {
                        return;
                    }
                    continue;
                }
                // The revision live now: a deploy changes what is valid.
                let (compiled, log) = match state.mocks.lookup(&t.mock) {
                    Lookup::Ready(live) if live.enabled => (live.compiled, live.capture),
                    _ => return close(&mut socket, close_code::AWAY, "the mock is gone or turned off").await,
                };
                let Some(ch) = channel_of(&compiled, &channel) else {
                    return close(&mut socket, close_code::AWAY, "the channel is gone").await;
                };
                let (verdict, _) = state
                    .events
                    .receive(&state, &t.mock, &compiled, &log, ch, &text, "ws", sender.clone())
                    .await;
                if !verdict.valid() && !send_text(&mut socket, invalid(&verdict).to_string()).await {
                    return;
                }
            },
            _ = tokio::time::sleep_until(deadline) => {
                return close(&mut socket, close_code::NORMAL, "the connection reached its longest duration").await;
            }
        }
    }
}

/// Server-sent events: each message is an event named after it, its number
/// on the channel as the id, the payload as the data.
fn sse(mut listener: Listener, lifetime: Duration, visitor: bool) -> Response {
    let deadline = tokio::time::Instant::now() + lifetime;
    let stream = async_stream::stream! {
        loop {
            tokio::select! {
                frame = listener.rx.recv() => match frame {
                    Ok(f) => match &*f {
                        Frame::Message { id, name, text, .. } => {
                            // A name with a line break would end the field.
                            let name: String = name.chars().filter(|c| !c.is_control()).collect();
                            yield Ok::<Event, Infallible>(
                                Event::default().event(name).id(id.to_string()).data(text.clone()),
                            );
                        }
                        Frame::Closed => break,
                    },
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                },
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
    };
    let mut resp = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    if visitor {
        // The safety headers of a visitor's mock; the stream keeps its type
        // (a browser does not run it).
        let kept = resp.headers().get(header::CONTENT_TYPE).cloned();
        restrict::harden(&mut resp);
        if let Some(ct) = kept {
            resp.headers_mut().insert(header::CONTENT_TYPE, ct);
        }
    }
    resp
}

// ── Control plane ───────────────────────────────────────────────────

fn svc(state: &AppState, headers: &HeaderMap, extensions: &Extensions, uri: &Uri) -> Svc {
    Svc::from_request(state, headers, extensions, uri)
}

fn done(r: Result<Value, ServiceError>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

pub async fn channels(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    done(super::channels(
        &svc(&state, &headers, &extensions, &uri),
        &name,
    ))
}

#[derive(Deserialize)]
pub struct MessagesQuery {
    limit: Option<usize>,
    after: Option<u64>,
}

pub async fn messages(
    State(state): State<AppState>,
    Path((name, channel)): Path<(String, String)>,
    Query(q): Query<MessagesQuery>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let s = svc(&state, &headers, &extensions, &uri);
    done(super::messages(&s, &name, &channel, q.limit, q.after))
}

pub async fn publish(
    State(state): State<AppState>,
    Path((name, channel)): Path<(String, String)>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Response {
    let s = svc(&state, &headers, &extensions, &uri);
    let body: super::PublishBody = if body.iter().all(u8::is_ascii_whitespace) {
        super::PublishBody::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(e) => {
                return ServiceError::bad_request(format!("invalid body: {e}")).into_response()
            }
        }
    };
    done(super::publish(&s, &name, &channel, body))
}

pub async fn subscribers(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    done(super::subscribers(
        &svc(&state, &headers, &extensions, &uri),
        &name,
    ))
}

pub async fn subscribe(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Response {
    let s = svc(&state, &headers, &extensions, &uri);
    let body: super::SubscribeBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return ServiceError::bad_request(format!("invalid body: {e}")).into_response(),
    };
    match super::subscribe(&s, &name, body) {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => e.into_response(),
    }
}

pub async fn unsubscribe(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, String)>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    done(super::unsubscribe(
        &svc(&state, &headers, &extensions, &uri),
        &name,
        &id,
    ))
}
