use axum::{
    extract::Query,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

// ── Request types ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct ChatRequest {
    #[serde(default = "default_model")]
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(default)]
    stream: bool,
    #[allow(dead_code)]
    #[serde(default)]
    max_tokens: Option<u32>,
    #[allow(dead_code)]
    #[serde(default)]
    temperature: Option<f64>,
}

#[derive(Deserialize, Serialize, Clone)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct CompletionRequest {
    #[serde(default = "default_model")]
    model: String,
    prompt: String,
    #[allow(dead_code)]
    #[serde(default)]
    max_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct EmbeddingRequest {
    #[serde(default = "default_embed_model")]
    model: String,
    input: EmbeddingInput,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EmbeddingInput {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Deserialize)]
struct DelayQuery {
    #[serde(default)]
    delay: Option<u64>,
}

fn default_model() -> String {
    "rustybin-gpt".to_string()
}

fn default_embed_model() -> String {
    "rustybin-embed".to_string()
}

// ── Response types ──────────────────────────────────────────────────

#[derive(Serialize)]
struct ChatCompletionResponse {
    id: String,
    object: String,
    created: i64,
    model: String,
    choices: Vec<ChatChoice>,
    usage: Usage,
}

#[derive(Serialize)]
struct ChatChoice {
    index: u32,
    message: ChatMessage,
    finish_reason: String,
}

#[derive(Serialize)]
struct CompletionResponse {
    id: String,
    object: String,
    created: i64,
    model: String,
    choices: Vec<CompletionChoice>,
    usage: Usage,
}

#[derive(Serialize)]
struct CompletionChoice {
    text: String,
    index: u32,
    finish_reason: String,
}

#[derive(Serialize)]
struct EmbeddingResponse {
    object: String,
    data: Vec<EmbeddingData>,
    model: String,
    usage: EmbeddingUsage,
}

#[derive(Serialize)]
struct EmbeddingData {
    object: String,
    index: usize,
    embedding: Vec<f64>,
}

#[derive(Serialize)]
struct EmbeddingUsage {
    prompt_tokens: usize,
    total_tokens: usize,
}

#[derive(Serialize)]
struct Usage {
    prompt_tokens: usize,
    completion_tokens: usize,
    total_tokens: usize,
}

#[derive(Serialize)]
struct ModelsResponse {
    object: String,
    data: Vec<ModelEntry>,
}

#[derive(Serialize)]
struct ModelEntry {
    id: String,
    object: String,
    created: i64,
    owned_by: String,
}

#[derive(Serialize)]
struct OpenAIError {
    error: OpenAIErrorDetail,
}

#[derive(Serialize)]
struct OpenAIErrorDetail {
    message: String,
    r#type: String,
    param: Option<String>,
    code: Option<String>,
}

// ── Token counting ──────────────────────────────────────────────────

pub(crate) fn count_tokens(text: &str) -> usize {
    // Approximate tokenization: split on whitespace and punctuation
    text.split(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
        .filter(|s| !s.is_empty())
        .count()
}

// ── Canned responses ────────────────────────────────────────────────

pub(crate) fn canned_response(last_message: &str) -> &'static str {
    let lower = last_message.to_lowercase();

    if lower.contains("hello") || lower.contains("hi") || lower.contains("hey") {
        "Hello! I'm Rustybin, a mock AI endpoint for API gateway testing. \
         I'm here to help you test your AI proxy configuration, rate limiting, \
         prompt guardrails, and other gateway plugins. How can I help you today?"
    } else if lower.contains("code") || lower.contains("python") || lower.contains("function") {
        "Here's a simple example function:\n\n```python\ndef greet(name: str) -> str:\n    \
         \"\"\"Return a greeting message.\"\"\"\n    return f\"Hello, {name}! \
         Welcome to Rustybin.\"\n\nprint(greet(\"World\"))\n```\n\n\
         This function takes a name parameter and returns a formatted greeting string."
    } else if lower.contains("json") || lower.contains("data") {
        "Here's a sample JSON data structure:\n\n```json\n{\n  \"users\": [\n    \
         {\"id\": 1, \"name\": \"Alice\", \"role\": \"admin\"},\n    \
         {\"id\": 2, \"name\": \"Bob\", \"role\": \"user\"}\n  ],\n  \
         \"total\": 2,\n  \"page\": 1\n}\n```\n\n\
         This represents a paginated list of users with their roles."
    } else if lower.contains("long") || lower.contains("essay") {
        "API gateways serve as the critical entry point for all API traffic in modern \
         microservices architectures. They provide a centralized layer for cross-cutting \
         concerns such as authentication, rate limiting, request transformation, and \
         observability.\n\n\
         When deploying AI-powered services, the gateway takes on additional \
         responsibilities. It must handle streaming responses via Server-Sent Events, \
         count tokens for usage-based rate limiting, cache semantically similar requests, \
         and enforce prompt safety guardrails.\n\n\
         AI gateway plugins demonstrate these capabilities effectively. An \
         AI proxy routes requests to multiple LLM providers with a unified API. \
         AI rate limiting tracks token consumption across time windows. Prompt \
         guarding inspects prompts for policy violations before they reach \
         the model. Together, these form a comprehensive AI governance layer."
    } else {
        "Hello! I'm Rustybin, a mock AI endpoint for API gateway testing. I received \
         your message and I'm responding with a canned response. This is useful for \
         testing AI proxy plugins, rate limiting by token count, prompt guardrails, \
         and semantic caching."
    }
}

// ── Deterministic embedding ─────────────────────────────────────────

fn generate_embedding(text: &str) -> Vec<f64> {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let seed = hasher.finish();

    // Generate 1536 deterministic floats from the seed
    let mut rng_state = seed;
    (0..1536)
        .map(|_| {
            // Simple LCG PRNG for deterministic values
            rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
            // Normalize to [-1, 1] range
            ((rng_state >> 32) as f64 / u32::MAX as f64) * 2.0 - 1.0
        })
        .collect()
}

// ── Error helper ────────────────────────────────────────────────────

fn openai_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        serde_json::to_string(&OpenAIError {
            error: OpenAIErrorDetail {
                message: message.to_string(),
                r#type: "invalid_request_error".to_string(),
                param: None,
                code: None,
            },
        })
        .unwrap_or_default(),
    )
        .into_response()
}

// ── Handlers ────────────────────────────────────────────────────────

async fn chat_completions(Query(q): Query<DelayQuery>, body: axum::body::Bytes) -> Response {
    if let Some(delay_ms) = q.delay {
        let delay = delay_ms.min(60000);
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }

    let req: ChatRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return openai_error(
                StatusCode::BAD_REQUEST,
                &format!("Invalid request body: {e}"),
            )
        }
    };

    if req.messages.is_empty() {
        return openai_error(StatusCode::BAD_REQUEST, "messages array must not be empty");
    }

    let last_user_msg = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.as_str())
        .unwrap_or("");

    let response_text = canned_response(last_user_msg);
    let created = chrono::Utc::now().timestamp();
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());

    let prompt_tokens: usize = req.messages.iter().map(|m| count_tokens(&m.content)).sum();

    if req.stream {
        return stream_chat_response(id, created, &req.model, response_text, prompt_tokens);
    }

    let completion_tokens = count_tokens(response_text);

    let resp = ChatCompletionResponse {
        id,
        object: "chat.completion".to_string(),
        created,
        model: req.model,
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content: response_text.to_string(),
            },
            finish_reason: "stop".to_string(),
        }],
        usage: Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        },
    };

    Json(resp).into_response()
}

fn stream_chat_response(
    id: String,
    created: i64,
    model: &str,
    text: &str,
    _prompt_tokens: usize,
) -> Response {
    let model = model.to_string();
    let words: Vec<String> = text.split_whitespace().map(|w| w.to_string()).collect();

    let stream = async_stream::stream! {
        // First chunk: role
        let chunk = serde_json::json!({
            "id": &id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": &model,
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant"},
                "finish_reason": null
            }]
        });
        yield Ok::<_, std::convert::Infallible>(format!("data: {}\n\n", chunk));

        // Content chunks: one word at a time
        for (i, word) in words.iter().enumerate() {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let content = if i == 0 {
                word.clone()
            } else {
                format!(" {word}")
            };
            let chunk = serde_json::json!({
                "id": &id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": &model,
                "choices": [{
                    "index": 0,
                    "delta": {"content": content},
                    "finish_reason": null
                }]
            });
            yield Ok(format!("data: {}\n\n", chunk));
        }

        // Final chunk: finish_reason
        let chunk = serde_json::json!({
            "id": &id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": &model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        });
        yield Ok(format!("data: {}\n\n", chunk));

        // DONE marker
        yield Ok("data: [DONE]\n\n".to_string());
    };

    let body = axum::body::Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn completions(Query(q): Query<DelayQuery>, body: axum::body::Bytes) -> Response {
    if let Some(delay_ms) = q.delay {
        let delay = delay_ms.min(60000);
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }

    let req: CompletionRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return openai_error(
                StatusCode::BAD_REQUEST,
                &format!("Invalid request body: {e}"),
            )
        }
    };

    let response_text = canned_response(&req.prompt);
    let prompt_tokens = count_tokens(&req.prompt);
    let completion_tokens = count_tokens(response_text);

    let resp = CompletionResponse {
        id: format!("cmpl-{}", uuid::Uuid::new_v4()),
        object: "text_completion".to_string(),
        created: chrono::Utc::now().timestamp(),
        model: req.model,
        choices: vec![CompletionChoice {
            text: response_text.to_string(),
            index: 0,
            finish_reason: "stop".to_string(),
        }],
        usage: Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        },
    };

    Json(resp).into_response()
}

async fn embeddings(body: axum::body::Bytes) -> Response {
    let req: EmbeddingRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return openai_error(
                StatusCode::BAD_REQUEST,
                &format!("Invalid request body: {e}"),
            )
        }
    };

    let inputs = match req.input {
        EmbeddingInput::Single(s) => vec![s],
        EmbeddingInput::Multiple(v) => v,
    };

    let mut total_tokens = 0;
    let data: Vec<EmbeddingData> = inputs
        .iter()
        .enumerate()
        .map(|(i, text)| {
            total_tokens += count_tokens(text);
            EmbeddingData {
                object: "embedding".to_string(),
                index: i,
                embedding: generate_embedding(text),
            }
        })
        .collect();

    let resp = EmbeddingResponse {
        object: "list".to_string(),
        data,
        model: req.model,
        usage: EmbeddingUsage {
            prompt_tokens: total_tokens,
            total_tokens,
        },
    };

    Json(resp).into_response()
}

async fn models(headers: HeaderMap) -> Response {
    let resp = ModelsResponse {
        object: "list".to_string(),
        data: vec![
            ModelEntry {
                id: "rustybin-gpt".to_string(),
                object: "model".to_string(),
                created: 1700000000,
                owned_by: "rustybin".to_string(),
            },
            ModelEntry {
                id: "rustybin-gpt-fast".to_string(),
                object: "model".to_string(),
                created: 1700000000,
                owned_by: "rustybin".to_string(),
            },
            ModelEntry {
                id: "rustybin-embed".to_string(),
                object: "model".to_string(),
                created: 1700000000,
                owned_by: "rustybin".to_string(),
            },
        ],
    };

    crate::content_negotiation::negotiate(&headers, &resp)
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/ai/v1/chat/completions", post(chat_completions))
        .route("/ai/v1/completions", post(completions))
        .route("/ai/v1/embeddings", post(embeddings))
        .route("/ai/v1/models", get(models))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new("/ai/v1/chat/completions", &["POST"], category::AI_OPENAI, "Chat completions (SSE streaming with stream=true)")
            .example(Example::post("Chat completions", "/ai/v1/chat/completions")
                .json(r#"{"model":"rustybin-gpt","messages":[{"role":"user","content":"hello"}]}"#))
            .example(Example::post("Chat completions (streaming)", "/ai/v1/chat/completions")
                .json(r#"{"model":"rustybin-gpt","messages":[{"role":"user","content":"hello"}],"stream":true}"#)),
        Endpoint::new("/ai/v1/completions", &["POST"], category::AI_OPENAI, "Legacy text completions")
            .example(Example::post("Text completions", "/ai/v1/completions")
                .json(r#"{"model":"rustybin-gpt","prompt":"Say hello"}"#)),
        Endpoint::new("/ai/v1/embeddings", &["POST"], category::AI_OPENAI, "Deterministic 1536-dimension embeddings")
            .example(Example::post("Embeddings", "/ai/v1/embeddings")
                .json(r#"{"model":"rustybin-embed","input":"Hello world"}"#)),
        Endpoint::new("/ai/v1/models", &["GET"], category::AI_OPENAI, "List available models")
            .example(Example::get("List models", "/ai/v1/models")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app() -> Router {
        crate::test_support::module_app(router)
    }

    async fn json_body(resp: axum::http::Response<Body>) -> serde_json::Value {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&body).expect("json")
    }

    #[tokio::test]
    async fn chat_completion_non_streaming() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"rustybin-gpt","messages":[{"role":"user","content":"hello"}]}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["object"], "chat.completion");
        assert_eq!(json["model"], "rustybin-gpt");
        assert!(json["id"].as_str().expect("id").starts_with("chatcmpl-"));
        assert_eq!(json["choices"][0]["finish_reason"], "stop");
        assert!(json["choices"][0]["message"]["content"]
            .as_str()
            .expect("content")
            .contains("Rustybin"));
        // Token usage
        assert!(json["usage"]["prompt_tokens"].as_u64().expect("pt") > 0);
        assert!(json["usage"]["completion_tokens"].as_u64().expect("ct") > 0);
        assert!(json["usage"]["total_tokens"].as_u64().expect("tt") > 0);
    }

    #[tokio::test]
    async fn chat_completion_streaming() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"rustybin-gpt","messages":[{"role":"user","content":"hello"}],"stream":true}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .expect("ct")
                .to_str()
                .expect("str"),
            "text/event-stream"
        );

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let text = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(text.contains("data: "));
        assert!(text.contains("[DONE]"));
        assert!(text.contains("\"role\":\"assistant\""));
        assert!(text.contains("\"finish_reason\":\"stop\""));
    }

    #[tokio::test]
    async fn chat_completion_canned_code_response() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"rustybin-gpt","messages":[{"role":"user","content":"write me some python code"}]}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        let json = json_body(resp).await;
        assert!(json["choices"][0]["message"]["content"]
            .as_str()
            .expect("content")
            .contains("python"));
    }

    #[tokio::test]
    async fn chat_completion_empty_messages_returns_error() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"rustybin-gpt","messages":[]}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = json_body(resp).await;
        assert_eq!(json["error"]["type"], "invalid_request_error");
    }

    #[tokio::test]
    async fn chat_completion_invalid_body() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from("not json"))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn legacy_completions() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/v1/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"rustybin-gpt","prompt":"Once upon a time"}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["object"], "text_completion");
        assert!(json["id"].as_str().expect("id").starts_with("cmpl-"));
        assert!(json["choices"][0]["text"].is_string());
        assert!(json["usage"]["prompt_tokens"].as_u64().expect("pt") > 0);
    }

    #[tokio::test]
    async fn embeddings_single_input() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/v1/embeddings")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"rustybin-embed","input":"The quick brown fox"}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["object"], "list");
        assert_eq!(json["data"][0]["object"], "embedding");
        let embedding = json["data"][0]["embedding"].as_array().expect("array");
        assert_eq!(embedding.len(), 1536);
        assert!(json["usage"]["prompt_tokens"].as_u64().expect("pt") > 0);
    }

    #[tokio::test]
    async fn embeddings_deterministic() {
        let app = test_app();

        // First request
        let resp1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/v1/embeddings")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"rustybin-embed","input":"test input"}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");
        let json1 = json_body(resp1).await;

        // Second request with same input
        let resp2 = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/v1/embeddings")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"rustybin-embed","input":"test input"}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");
        let json2 = json_body(resp2).await;

        // Must be identical
        assert_eq!(json1["data"][0]["embedding"], json2["data"][0]["embedding"]);
    }

    #[tokio::test]
    async fn embeddings_multiple_inputs() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/v1/embeddings")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"rustybin-embed","input":["hello","world"]}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        let data = json["data"].as_array().expect("array");
        assert_eq!(data.len(), 2);
        assert_eq!(data[0]["index"], 0);
        assert_eq!(data[1]["index"], 1);
    }

    #[tokio::test]
    async fn models_list() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/ai/v1/models")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["object"], "list");
        let data = json["data"].as_array().expect("array");
        assert_eq!(data.len(), 3);
        assert_eq!(data[0]["id"], "rustybin-gpt");
        assert_eq!(data[2]["id"], "rustybin-embed");
    }

    #[test]
    fn token_counting_works() {
        assert_eq!(count_tokens("hello world"), 2);
        assert_eq!(count_tokens("Hello, how are you?"), 4);
        assert_eq!(count_tokens(""), 0);
        assert_eq!(count_tokens("one"), 1);
    }

    #[test]
    fn embedding_is_deterministic() {
        let e1 = generate_embedding("test");
        let e2 = generate_embedding("test");
        assert_eq!(e1, e2);

        let e3 = generate_embedding("different");
        assert_ne!(e1, e3);
    }
}
