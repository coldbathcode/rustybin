use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::Query,
    response::Response,
    routing::get,
    Router,
};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;

// ── /ws — echo ──────────────────────────────────────────────────────

/// Upgrade to a WebSocket that echoes back every text/binary frame it
/// receives. Useful for testing API gateway WebSocket proxying and
/// frame size/validation policies. Responds to ping/close per the protocol.
async fn ws_echo(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(handle_echo)
}

async fn handle_echo(mut socket: WebSocket) {
    while let Some(Ok(msg)) = socket.recv().await {
        match msg {
            Message::Text(text) => {
                if socket.send(Message::Text(text)).await.is_err() {
                    break;
                }
            }
            Message::Binary(bin) => {
                if socket.send(Message::Binary(bin)).await.is_err() {
                    break;
                }
            }
            Message::Ping(payload) => {
                // axum auto-responds to pings, but echo explicitly for clarity.
                let _ = socket.send(Message::Pong(payload)).await;
            }
            Message::Close(_) => break,
            Message::Pong(_) => {}
        }
    }
}

// ── /ws/time — server-push ticker ───────────────────────────────────

#[derive(Deserialize)]
struct TimeParams {
    /// Tick interval in milliseconds (default 1000, clamped to 100..=60000).
    #[serde(default)]
    interval_ms: Option<u64>,
    /// Number of ticks before the server closes (default 10, clamped to 1..=1000).
    #[serde(default)]
    count: Option<u64>,
}

/// Upgrade to a WebSocket that pushes the current RFC 3339 timestamp on a
/// fixed interval, then closes. Exercises server-initiated frames through the gateway.
async fn ws_time(ws: WebSocketUpgrade, Query(params): Query<TimeParams>) -> Response {
    let interval = params.interval_ms.unwrap_or(1000).clamp(100, 60_000);
    let count = params.count.unwrap_or(10).clamp(1, 1000);
    ws.on_upgrade(move |socket| handle_time(socket, interval, count))
}

async fn handle_time(mut socket: WebSocket, interval_ms: u64, count: u64) {
    let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
    for n in 0..count {
        ticker.tick().await;
        let now = chrono::Utc::now().to_rfc3339();
        let payload = format!("{{\"tick\":{n},\"timestamp\":\"{now}\"}}");
        if socket.send(Message::Text(payload)).await.is_err() {
            return;
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router() -> Router<Arc<Config>> {
    Router::new()
        .route("/ws", get(ws_echo))
        .route("/ws/time", get(ws_time))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as TMessage;
    use tower::ServiceExt;

    fn test_config() -> Arc<Config> {
        Arc::new(Config {
            http_port: 80,
            https_port: 443,
            host: "0.0.0.0".to_string(),
            log_level: "info".to_string(),
            trust_forward: false,
            body_limit: 1_048_576,
            instance_id: "test-instance".to_string(),
            tls_cert: "certs/server.crt".to_string(),
            tls_key: "certs/server.key".to_string(),
            mtls_in_header: None,
        })
    }

    fn test_app() -> Router {
        router().with_state(test_config())
    }

    /// Spawn the router on an ephemeral port and return its base ws:// URL.
    async fn spawn_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = test_app();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("ws://{addr}")
    }

    /// A plain GET without the WebSocket upgrade headers must be rejected
    /// (axum returns 426 Upgrade Required), not served as a normal 200.
    #[tokio::test]
    async fn non_upgrade_request_is_rejected() {
        let resp = test_app()
            .oneshot(Request::builder().uri("/ws").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::OK);
        assert!(resp.status().is_client_error());
    }

    #[tokio::test]
    async fn echo_round_trips_text_and_binary() {
        let base = spawn_server().await;
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("{base}/ws"))
            .await
            .unwrap();

        socket.send(TMessage::Text("hello".into())).await.unwrap();
        let reply = socket.next().await.unwrap().unwrap();
        assert_eq!(reply, TMessage::Text("hello".into()));

        socket
            .send(TMessage::Binary(vec![1, 2, 3, 4]))
            .await
            .unwrap();
        let reply = socket.next().await.unwrap().unwrap();
        assert_eq!(reply, TMessage::Binary(vec![1, 2, 3, 4]));
    }

    #[tokio::test]
    async fn time_endpoint_pushes_ticks_then_closes() {
        let base = spawn_server().await;
        let (mut socket, _) =
            tokio_tungstenite::connect_async(format!("{base}/ws/time?interval_ms=100&count=2"))
                .await
                .unwrap();

        let mut ticks = 0;
        while let Some(Ok(msg)) = socket.next().await {
            match msg {
                TMessage::Text(t) => {
                    assert!(t.contains("\"tick\""));
                    assert!(t.contains("\"timestamp\""));
                    ticks += 1;
                }
                TMessage::Close(_) => break,
                _ => {}
            }
        }
        assert_eq!(ticks, 2);
    }
}
