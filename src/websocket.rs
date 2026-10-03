//! WebSocket endpoints for gateway WebSocket proxying tests.
//!
//! - `/ws`: echo of every text and binary frame.
//! - `/ws/time`: server-push ticker; the socket is read concurrently, so a
//!   client close or ping is honoured mid-stream.
//!
//! Common behaviour:
//! - Subprotocols: the first protocol offered in `Sec-WebSocket-Protocol` is
//!   echoed back (gateways often test subprotocol pass-through).
//! - Pings are answered once, by the WebSocket stack (no manual pong).
//! - Close handshake: a client close is answered with a close frame and the
//!   connection is flushed before it is dropped; server-initiated closes
//!   (idle timeout, max lifetime, end of ticker) wait briefly for the reply.
//! - Limits ([`limits`]): max message size, idle timeout and max lifetime,
//!   lower in public mode.

use axum::{
    extract::ws::{close_code, CloseFrame, Message, WebSocket, WebSocketUpgrade},
    extract::{Query, State},
    http::HeaderMap,
    response::Response,
    routing::get,
    Extension, Router,
};
use futures_util::SinkExt;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::limits::{lease_expired, StreamLease, STREAM_END_REASON};
use crate::state::AppState;

/// Connection limits for WebSocket endpoints (also used by `/graphql/ws`).
#[derive(Clone, Copy, Debug)]
pub struct WsLimits {
    /// Maximum message (and frame) size in bytes; larger ones close the
    /// connection with 1009.
    pub max_message_size: usize,
    /// Close (1001) after this long without a message from the client.
    pub idle_timeout: Duration,
    /// Close (1001) after this long regardless of activity.
    pub max_lifetime: Duration,
}

/// Normal mode: 1 MiB messages, 5 min idle, 1 h lifetime.
/// Public mode: 256 KiB messages, 1 min idle, 10 min lifetime.
pub fn limits(config: &Config) -> WsLimits {
    if config.public_mode {
        WsLimits {
            max_message_size: 256 * 1024,
            idle_timeout: Duration::from_secs(60),
            max_lifetime: Duration::from_secs(600),
        }
    } else {
        WsLimits {
            max_message_size: 1024 * 1024,
            idle_timeout: Duration::from_secs(300),
            max_lifetime: Duration::from_secs(3600),
        }
    }
}

/// How long to wait for the peer's close reply (or our flush) on close.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// The first valid subprotocol token the client offered.
fn first_protocol(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all("sec-websocket-protocol")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .find(|p| {
            !p.is_empty()
                && p.len() <= 64
                && p.bytes().all(|b| {
                    b.is_ascii_alphanumeric()
                        || matches!(
                            b,
                            b'!' | b'#'
                                | b'$'
                                | b'%'
                                | b'&'
                                | b'\''
                                | b'*'
                                | b'+'
                                | b'-'
                                | b'.'
                                | b'^'
                                | b'_'
                                | b'`'
                                | b'|'
                                | b'~'
                        )
                })
        })
        .map(str::to_string)
}

fn configure(ws: WebSocketUpgrade, headers: &HeaderMap, limits: WsLimits) -> WebSocketUpgrade {
    let ws = ws
        .max_message_size(limits.max_message_size)
        .max_frame_size(limits.max_message_size);
    match first_protocol(headers) {
        Some(p) => ws.protocols([p]),
        None => ws,
    }
}

/// Send a close frame and wait (bounded) for the peer's reply.
async fn close_with(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };
    if socket.send(Message::Close(Some(frame))).await.is_err() {
        return;
    }
    let _ = tokio::time::timeout(CLOSE_GRACE, async {
        while let Some(Ok(msg)) = socket.recv().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
}

/// The client sent a close frame: the stack has queued the reply; flush it
/// and drain until the stream ends (bounded).
async fn finish_close(socket: &mut WebSocket) {
    let _ = socket.flush().await;
    let _ = tokio::time::timeout(CLOSE_GRACE, async {
        while let Some(Ok(_)) = socket.recv().await {}
    })
    .await;
}

// ── /ws - echo ──────────────────────────────────────────────────────

async fn ws_echo(
    ws: WebSocketUpgrade,
    State(config): State<Arc<Config>>,
    headers: HeaderMap,
    lease: Option<Extension<StreamLease>>,
) -> Response {
    let limits = limits(&config);
    let lease = lease.map(|Extension(l)| l);
    configure(ws, &headers, limits).on_upgrade(move |socket| handle_echo(socket, limits, lease))
}

/// `lease`: the plan's stream slot (held while the socket is open) and lifetime.
async fn handle_echo(mut socket: WebSocket, limits: WsLimits, lease: Option<StreamLease>) {
    let deadline = Instant::now() + limits.max_lifetime;
    loop {
        let idle = tokio::time::sleep(limits.idle_timeout);
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(text))) => {
                    if socket.send(Message::Text(text)).await.is_err() {
                        return;
                    }
                }
                Some(Ok(Message::Binary(bin))) => {
                    if socket.send(Message::Binary(bin)).await.is_err() {
                        return;
                    }
                }
                // Pings are answered by the WebSocket stack itself.
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) => {
                    finish_close(&mut socket).await;
                    return;
                }
                Some(Err(_)) | None => return,
            },
            _ = idle => {
                close_with(&mut socket, close_code::AWAY, "idle timeout").await;
                return;
            }
            _ = tokio::time::sleep_until(deadline) => {
                close_with(&mut socket, close_code::AWAY, "maximum connection lifetime reached").await;
                return;
            }
            _ = lease_expired(&lease) => {
                close_with(&mut socket, close_code::POLICY, STREAM_END_REASON).await;
                return;
            }
        }
    }
}

// ── /ws/time - server-push ticker ───────────────────────────────────

#[derive(Deserialize)]
struct TimeParams {
    /// Tick interval in milliseconds (default 1000, clamped to 100..=60000).
    #[serde(default)]
    interval_ms: Option<u64>,
    /// Number of ticks before the server closes (default 10, clamped to 1..=1000).
    #[serde(default)]
    count: Option<u64>,
}

async fn ws_time(
    ws: WebSocketUpgrade,
    State(config): State<Arc<Config>>,
    headers: HeaderMap,
    Query(params): Query<TimeParams>,
    lease: Option<Extension<StreamLease>>,
) -> Response {
    let interval = params.interval_ms.unwrap_or(1000).clamp(100, 60_000);
    let count = params.count.unwrap_or(10).clamp(1, 1000);
    let limits = limits(&config);
    let lease = lease.map(|Extension(l)| l);
    configure(ws, &headers, limits)
        .on_upgrade(move |socket| handle_time(socket, interval, count, limits, lease))
}

async fn handle_time(
    mut socket: WebSocket,
    interval_ms: u64,
    count: u64,
    limits: WsLimits,
    lease: Option<StreamLease>,
) {
    let deadline = Instant::now() + limits.max_lifetime;
    let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
    let mut sent = 0u64;
    while sent < count {
        tokio::select! {
            _ = ticker.tick() => {
                let payload = serde_json::json!({
                    "tick": sent,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                })
                .to_string();
                if socket.send(Message::Text(payload.into())).await.is_err() {
                    return;
                }
                sent += 1;
            }
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) => {
                    finish_close(&mut socket).await;
                    return;
                }
                Some(Ok(_)) => {} // pings answered by the stack; data ignored
                Some(Err(_)) | None => return,
            },
            _ = tokio::time::sleep_until(deadline) => {
                close_with(&mut socket, close_code::AWAY, "maximum connection lifetime reached").await;
                return;
            }
            _ = lease_expired(&lease) => {
                close_with(&mut socket, close_code::POLICY, STREAM_END_REASON).await;
                return;
            }
        }
    }
    close_with(&mut socket, close_code::NORMAL, "done").await;
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/ws", get(ws_echo))
        .route("/ws/time", get(ws_time))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/ws",
            &["GET"],
            category::WEBSOCKET,
            "WebSocket echo of every text and binary frame (subprotocol echoed)",
        )
        .description(
            "Echoes the first offered Sec-WebSocket-Protocol. Max message 1 MiB (256 KiB public), \
             idle timeout 5 min (1 min), max lifetime 1 h (10 min); closes with 1009 / 1001.",
        )
        .websocket()
        .example(Example::get("WebSocket echo", "/ws")),
        Endpoint::new(
            "/ws/time",
            &["GET"],
            category::WEBSOCKET,
            "WebSocket timestamp ticker (?interval_ms=&count=), then a normal close",
        )
        .websocket()
        .example(Example::get(
            "WebSocket ticker",
            "/ws/time?interval_ms=500&count=5",
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use tokio_tungstenite::tungstenite::Message as TMessage;
    use tower::ServiceExt;

    type Client = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    fn test_app() -> Router {
        crate::test_support::module_app(router)
    }

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("ws://{addr}")
    }

    async fn spawn_server() -> String {
        serve(test_app()).await
    }

    async fn next(socket: &mut Client) -> Option<TMessage> {
        tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("message in time")
            .and_then(Result::ok)
    }

    #[tokio::test]
    async fn non_upgrade_request_is_rejected() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/ws")
                    .body(Body::empty())
                    .expect("req"),
            )
            .await
            .expect("resp");
        assert_ne!(resp.status(), StatusCode::OK);
        assert!(resp.status().is_client_error());
    }

    #[tokio::test]
    async fn echo_round_trips_text_and_binary() {
        let base = spawn_server().await;
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("{base}/ws"))
            .await
            .expect("connect");
        socket
            .send(TMessage::Text("hello".into()))
            .await
            .expect("send");
        assert_eq!(
            next(&mut socket).await,
            Some(TMessage::Text("hello".into()))
        );
        socket
            .send(TMessage::Binary(vec![1, 2, 3, 4]))
            .await
            .expect("send");
        assert_eq!(
            next(&mut socket).await,
            Some(TMessage::Binary(vec![1, 2, 3, 4]))
        );
    }

    #[tokio::test]
    async fn subprotocol_is_echoed() {
        let base = spawn_server().await;
        let mut req = format!("{base}/ws").into_client_request().expect("req");
        req.headers_mut().insert(
            "sec-websocket-protocol",
            "chat.v2, superchat".parse().expect("hdr"),
        );
        let (_socket, resp) = tokio_tungstenite::connect_async(req)
            .await
            .expect("connect");
        assert_eq!(
            resp.headers()
                .get("sec-websocket-protocol")
                .and_then(|v| v.to_str().ok()),
            Some("chat.v2")
        );
        // No protocol offered: none selected.
        let (_socket, resp) = tokio_tungstenite::connect_async(format!("{base}/ws"))
            .await
            .expect("connect");
        assert!(resp.headers().get("sec-websocket-protocol").is_none());
    }

    #[tokio::test]
    async fn single_pong_per_ping() {
        let base = spawn_server().await;
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("{base}/ws"))
            .await
            .expect("connect");
        socket.send(TMessage::Ping(vec![7])).await.expect("ping");
        socket
            .send(TMessage::Text("after".into()))
            .await
            .expect("send");
        assert_eq!(next(&mut socket).await, Some(TMessage::Pong(vec![7])));
        // The next frame is the echo, not a second pong.
        assert_eq!(
            next(&mut socket).await,
            Some(TMessage::Text("after".into()))
        );
    }

    #[tokio::test]
    async fn client_close_gets_a_close_reply() {
        let base = spawn_server().await;
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("{base}/ws"))
            .await
            .expect("connect");
        socket.close(None).await.expect("close");
        let mut got_close = false;
        while let Some(msg) = next(&mut socket).await {
            if let TMessage::Close(_) = msg {
                got_close = true;
            }
        }
        assert!(got_close, "server must answer the close handshake");
    }

    #[tokio::test]
    async fn oversized_messages_close_the_connection() {
        let base = spawn_server().await;
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("{base}/ws"))
            .await
            .expect("connect");
        let big = vec![0u8; 1024 * 1024 + 1];
        let _ = socket.send(TMessage::Binary(big)).await;
        // Either a close frame (1009) or the connection drops; never an echo.
        match next(&mut socket).await {
            None => {}
            Some(TMessage::Close(Some(frame))) => assert_eq!(frame.code, CloseCode::Size),
            Some(TMessage::Close(None)) => {}
            Some(other) => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn idle_and_lifetime_limits_close_with_1001() {
        let small = WsLimits {
            max_message_size: 1024,
            idle_timeout: Duration::from_millis(150),
            max_lifetime: Duration::from_secs(60),
        };
        let app = Router::new().route(
            "/ws",
            get(move |ws: WebSocketUpgrade| async move {
                ws.on_upgrade(move |s| handle_echo(s, small, None))
            }),
        );
        let base = serve(app).await;
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("{base}/ws"))
            .await
            .expect("connect");
        match next(&mut socket).await {
            Some(TMessage::Close(Some(frame))) => {
                assert_eq!(frame.code, CloseCode::Away);
                assert_eq!(frame.reason, "idle timeout");
            }
            other => panic!("expected close, got {other:?}"),
        }

        let short_life = WsLimits {
            max_lifetime: Duration::from_millis(150),
            idle_timeout: Duration::from_secs(60),
            ..small
        };
        let app = Router::new().route(
            "/ws/time",
            get(move |ws: WebSocketUpgrade| async move {
                ws.on_upgrade(move |s| handle_time(s, 100, 1000, short_life, None))
            }),
        );
        let base = serve(app).await;
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("{base}/ws/time"))
            .await
            .expect("connect");
        let mut closed = None;
        while let Some(msg) = next(&mut socket).await {
            if let TMessage::Close(frame) = msg {
                closed = frame;
                break;
            }
        }
        assert_eq!(closed.map(|f| f.code), Some(CloseCode::Away));
    }

    #[tokio::test]
    async fn time_endpoint_pushes_ticks_then_closes() {
        let base = spawn_server().await;
        let (mut socket, _) =
            tokio_tungstenite::connect_async(format!("{base}/ws/time?interval_ms=100&count=2"))
                .await
                .expect("connect");
        let mut ticks = 0;
        let mut close_code = None;
        while let Some(msg) = next(&mut socket).await {
            match msg {
                TMessage::Text(t) => {
                    assert!(t.contains("\"tick\""));
                    assert!(t.contains("\"timestamp\""));
                    ticks += 1;
                }
                TMessage::Close(frame) => {
                    close_code = frame.map(|f| f.code);
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(ticks, 2);
        assert_eq!(close_code, Some(CloseCode::Normal));
    }

    #[tokio::test]
    async fn time_endpoint_honours_client_close_and_ping() {
        let base = spawn_server().await;
        let (mut socket, _) =
            tokio_tungstenite::connect_async(format!("{base}/ws/time?interval_ms=60000&count=5"))
                .await
                .expect("connect");
        // First tick is immediate.
        assert!(matches!(next(&mut socket).await, Some(TMessage::Text(_))));
        // A ping is answered while the ticker waits a minute.
        socket.send(TMessage::Ping(vec![1])).await.expect("ping");
        assert_eq!(next(&mut socket).await, Some(TMessage::Pong(vec![1])));
        // A close is answered right away, not after the next tick.
        let started = std::time::Instant::now();
        socket.close(None).await.expect("close");
        let mut got_close = false;
        while let Some(msg) = next(&mut socket).await {
            if matches!(msg, TMessage::Close(_)) {
                got_close = true;
            }
        }
        assert!(got_close);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn protocol_tokens_are_validated() {
        let mut h = HeaderMap::new();
        h.insert(
            "sec-websocket-protocol",
            "bad proto, ok-1".parse().expect("hdr"),
        );
        assert_eq!(first_protocol(&h).as_deref(), Some("ok-1"));
        let mut h = HeaderMap::new();
        h.insert("sec-websocket-protocol", "\"quoted\"".parse().expect("hdr"));
        assert_eq!(first_protocol(&h), None);
    }
}
