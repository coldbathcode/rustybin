//! Anthropic Messages API-compatible mock endpoint.
//!
//! Mirrors the request/response shape of Anthropic's `POST /v1/messages`,
//! including the native server-sent-event stream format (`message_start`,
//! `content_block_delta`, `message_stop`, …). This lets an AI gateway's
//! provider-routing be exercised against the Anthropic provider format —
//! not just the OpenAI format served by `ai_gateway`.

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::ai_gateway::{canned_response, count_tokens};
use crate::config::Config;

// ── Request types ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct MessagesRequest {
    #[serde(default = "default_model")]
    model: String,
    messages: Vec<InMessage>,
    #[allow(dead_code)]
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    stream: bool,
    #[allow(dead_code)]
    #[serde(default)]
    temperature: Option<f64>,
}

#[derive(Deserialize)]
struct InMessage {
    #[allow(dead_code)]
    role: String,
    content: MessageContent,
}

/// Anthropic content is either a bare string or an array of typed blocks.
#[derive(Deserialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(default)]
    text: Option<String>,
}

impl MessageContent {
    fn as_text(&self) -> String {
        match self {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| b.text.clone())
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

fn default_model() -> String {
    "rustybin-claude".to_string()
}

// ── Response types ──────────────────────────────────────────────────

#[derive(Serialize)]
struct MessagesResponse {
    id: String,
    r#type: String,
    role: String,
    model: String,
    content: Vec<OutBlock>,
    stop_reason: String,
    stop_sequence: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Serialize)]
struct OutBlock {
    r#type: String,
    text: String,
}

#[derive(Serialize)]
struct AnthropicUsage {
    input_tokens: usize,
    output_tokens: usize,
}

#[derive(Serialize)]
struct AnthropicError {
    r#type: String,
    error: AnthropicErrorDetail,
}

#[derive(Serialize)]
struct AnthropicErrorDetail {
    r#type: String,
    message: String,
}

fn anthropic_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        serde_json::to_string(&AnthropicError {
            r#type: "error".to_string(),
            error: AnthropicErrorDetail {
                r#type: "invalid_request_error".to_string(),
                message: message.to_string(),
            },
        })
        .unwrap_or_default(),
    )
        .into_response()
}

// ── Handler ─────────────────────────────────────────────────────────

async fn messages(body: axum::body::Bytes) -> Response {
    let req: MessagesRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return anthropic_error(StatusCode::BAD_REQUEST, &format!("invalid request body: {e}"))
        }
    };

    if req.messages.is_empty() {
        return anthropic_error(StatusCode::BAD_REQUEST, "messages array must not be empty");
    }

    // Use the last user-turn message to pick a canned reply.
    let last = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.as_text())
        .unwrap_or_default();

    let response_text = canned_response(&last);

    let mut input_tokens: usize = req.messages.iter().map(|m| count_tokens(&m.content.as_text())).sum();
    if let Some(system) = &req.system {
        input_tokens += count_tokens(system);
    }
    let output_tokens = count_tokens(response_text);

    let id = format!("msg_{}", uuid::Uuid::new_v4().simple());

    if req.stream {
        return stream_messages(id, &req.model, response_text, input_tokens, output_tokens);
    }

    let resp = MessagesResponse {
        id,
        r#type: "message".to_string(),
        role: "assistant".to_string(),
        model: req.model,
        content: vec![OutBlock {
            r#type: "text".to_string(),
            text: response_text.to_string(),
        }],
        stop_reason: "end_turn".to_string(),
        stop_sequence: None,
        usage: AnthropicUsage {
            input_tokens,
            output_tokens,
        },
    };

    Json(resp).into_response()
}

/// Emit the Anthropic SSE event sequence: message_start → content_block_start →
/// content_block_delta* → content_block_stop → message_delta → message_stop.
fn stream_messages(
    id: String,
    model: &str,
    text: &str,
    input_tokens: usize,
    output_tokens: usize,
) -> Response {
    let model = model.to_string();
    let words: Vec<String> = text.split_whitespace().map(|w| w.to_string()).collect();

    let stream = async_stream::stream! {
        let message_start = serde_json::json!({
            "type": "message_start",
            "message": {
                "id": &id,
                "type": "message",
                "role": "assistant",
                "model": &model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": input_tokens, "output_tokens": 0}
            }
        });
        yield Ok::<_, std::convert::Infallible>(sse("message_start", &message_start));

        let block_start = serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        });
        yield Ok(sse("content_block_start", &block_start));

        yield Ok(sse("ping", &serde_json::json!({"type": "ping"})));

        for (i, word) in words.iter().enumerate() {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let chunk = if i == 0 { word.clone() } else { format!(" {word}") };
            let delta = serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": chunk}
            });
            yield Ok(sse("content_block_delta", &delta));
        }

        yield Ok(sse(
            "content_block_stop",
            &serde_json::json!({"type": "content_block_stop", "index": 0}),
        ));

        let message_delta = serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": output_tokens}
        });
        yield Ok(sse("message_delta", &message_delta));

        yield Ok(sse("message_stop", &serde_json::json!({"type": "message_stop"})));
    };

    let body = axum::body::Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn sse(event: &str, data: &serde_json::Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router() -> Router<Arc<Config>> {
    Router::new().route("/ai/anthropic/v1/messages", post(messages))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
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

    async fn json_body(resp: axum::http::Response<Body>) -> serde_json::Value {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&body).expect("json")
    }

    #[tokio::test]
    async fn messages_non_streaming() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/anthropic/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"rustybin-claude","max_tokens":256,"messages":[{"role":"user","content":"hello"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["type"], "message");
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["model"], "rustybin-claude");
        assert!(json["id"].as_str().unwrap().starts_with("msg_"));
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["stop_reason"], "end_turn");
        assert!(json["usage"]["input_tokens"].as_u64().unwrap() > 0);
        assert!(json["usage"]["output_tokens"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn messages_block_content() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/anthropic/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"rustybin-claude","max_tokens":256,"system":"be terse","messages":[{"role":"user","content":[{"type":"text","text":"write python code"}]}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert!(json["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("python"));
    }

    #[tokio::test]
    async fn messages_streaming_event_sequence() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/anthropic/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"rustybin-claude","max_tokens":256,"stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap().to_str().unwrap(),
            "text/event-stream"
        );
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("event: message_start"));
        assert!(text.contains("event: content_block_delta"));
        assert!(text.contains("\"type\":\"text_delta\""));
        assert!(text.contains("event: message_stop"));
    }

    #[tokio::test]
    async fn messages_empty_returns_error() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/anthropic/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"rustybin-claude","messages":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = json_body(resp).await;
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "invalid_request_error");
    }
}
