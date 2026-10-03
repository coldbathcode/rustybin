//! Azure OpenAI: `/ai/azure/openai/deployments/{deployment}/...?api-version=...`.
//!
//! Same engine and payloads as OpenAI, plus Azure specifics: the
//! `api-version` query parameter is required, the deployment name is the
//! model, credentials come from `api-key`, and responses carry
//! `prompt_filter_results` / `content_filter_results`.

use axum::body::Bytes;
use axum::extract::Path;
use axum::response::Response;
use axum::routing::post;
use axum::{Extension, Router};
use serde_json::{json, Value};

use super::faults::ErrorKind;
use super::AiCtx;
use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

#[allow(clippy::result_large_err)]
fn require_api_version(ctx: &AiCtx) -> Result<(), Response> {
    match ctx.query.get("api-version").filter(|v| !v.is_empty()) {
        Some(_) => Ok(()),
        None => {
            let mut r = ctx.error(
                ErrorKind::BadRequest,
                "The api-version query parameter (?api-version=) is required for all requests.",
            );
            r.headers_mut().insert(
                "x-ms-error-code",
                axum::http::HeaderValue::from_static("MissingApiVersionParameter"),
            );
            Err(r)
        }
    }
}

async fn chat(
    Extension(ctx): Extension<AiCtx>,
    Path(deployment): Path<String>,
    body: Bytes,
) -> Response {
    if let Err(r) = require_api_version(&ctx) {
        return r;
    }
    super::openai::chat_impl(ctx, body, Some(deployment)).await
}

async fn completions(
    Extension(ctx): Extension<AiCtx>,
    Path(deployment): Path<String>,
    body: Bytes,
) -> Response {
    if let Err(r) = require_api_version(&ctx) {
        return r;
    }
    super::openai::completions_impl(ctx, body, Some(deployment)).await
}

async fn embeddings(
    Extension(ctx): Extension<AiCtx>,
    Path(deployment): Path<String>,
    body: Bytes,
) -> Response {
    if let Err(r) = require_api_version(&ctx) {
        return r;
    }
    super::openai::embeddings_impl(ctx, body, Some(deployment)).await
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/ai/azure/openai/deployments/{deployment}/chat/completions",
            post(chat),
        )
        .route(
            "/ai/azure/openai/deployments/{deployment}/completions",
            post(completions),
        )
        .route(
            "/ai/azure/openai/deployments/{deployment}/embeddings",
            post(embeddings),
        )
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new("/ai/azure/openai/deployments/{deployment}/chat/completions", &["POST"], category::AI_OPENAI, "Azure OpenAI chat completions (api-version required, model = deployment)")
            .description("Credential header: api-key. Responses include prompt_filter_results and content_filter_results; X-Rustybin-Fail: prompt_filter returns Azure's content_filter error.")
            .example(Example::post("Azure chat completion", "/ai/azure/openai/deployments/gpt-4o/chat/completions?api-version=2024-10-21")
                .header("api-key", "azure-demo-key")
                .json(r#"{"messages":[{"role":"user","content":"hello"}]}"#))
            .example(Example::post("Missing api-version", "/ai/azure/openai/deployments/gpt-4o/chat/completions")
                .json(r#"{"messages":[{"role":"user","content":"hello"}]}"#)
                .expect_status(400)),
        Endpoint::new("/ai/azure/openai/deployments/{deployment}/completions", &["POST"], category::AI_OPENAI, "Azure OpenAI legacy completions")
            .example(Example::post("Azure completion", "/ai/azure/openai/deployments/gpt-35-instruct/completions?api-version=2024-10-21")
                .json(r#"{"prompt":"Say hello"}"#)),
        Endpoint::new("/ai/azure/openai/deployments/{deployment}/embeddings", &["POST"], category::AI_OPENAI, "Azure OpenAI embeddings")
            .example(Example::post("Azure embeddings", "/ai/azure/openai/deployments/text-embedding-3-small/embeddings?api-version=2024-10-21")
                .json(r#"{"input":"Hello world"}"#)),
    ]
}

pub fn openapi_paths() -> Value {
    let op = |id: &str, summary: &str, schema: Value| {
        let mut params = vec![
            json!({"name": "deployment", "in": "path", "required": true, "schema": {"type": "string"}}),
            json!({"name": "api-version", "in": "query", "required": true, "schema": {"type": "string", "example": "2024-10-21"}}),
            json!({"name": "api-key", "in": "header", "schema": {"type": "string"}}),
        ];
        if let Value::Array(c) = super::common_parameters() {
            params.extend(c);
        }
        json!({"post": {
            "tags": ["AI Gateway"],
            "summary": summary,
            "operationId": id,
            "parameters": params,
            "requestBody": {"required": true, "content": {"application/json": {"schema": schema}}},
            "responses": {
                "200": {"description": "OK (OpenAI shape with Azure content filter annotations)"},
                "400": {"description": "Missing api-version or invalid request"},
                "401": {"description": "Missing or invalid api-key (when required)"}
            }
        }})
    };
    json!({
        "/ai/azure/openai/deployments/{deployment}/chat/completions": op("azureChatCompletions", "Azure OpenAI chat completions", json!({"$ref": "#/components/schemas/ChatCompletionRequest"})),
        "/ai/azure/openai/deployments/{deployment}/completions": op("azureCompletions", "Azure OpenAI completions", json!({"type": "object", "properties": {"prompt": {}}})),
        "/ai/azure/openai/deployments/{deployment}/embeddings": op("azureEmbeddings", "Azure OpenAI embeddings", json!({"type": "object", "properties": {"input": {}}})),
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use serde_json::json;

    #[tokio::test]
    async fn azure_chat_and_api_version() {
        let app = app();
        let body = json!({"messages": [{"role": "user", "content": "hello"}]});
        let url = "/ai/azure/openai/deployments/my-gpt/chat/completions?api-version=2024-10-21";
        let (s, h, b) = send(&app, url, &body, &[("api-key", "az-key-9876")]).await;
        assert_eq!(s, 200);
        assert_eq!(h["x-rustybin-credential"], "api-key ****9876");
        let v = json(&b);
        assert_eq!(v["model"], "my-gpt");
        assert!(v["prompt_filter_results"].is_array());
        assert_eq!(
            v["choices"][0]["content_filter_results"]["hate"]["filtered"],
            false
        );

        let (s, _, b) = send(
            &app,
            "/ai/azure/openai/deployments/my-gpt/chat/completions",
            &body,
            &[],
        )
        .await;
        assert_eq!(s, 400);
        assert!(json(&b)["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("api-version"));

        let (s, _, b) = send(&app, url, &body, &[("x-rustybin-fail", "prompt_filter")]).await;
        assert_eq!(s, 400);
        assert_eq!(json(&b)["error"]["code"], "content_filter");

        let (s, _, b) = send(
            &app,
            url,
            &json!({"messages": [{"role": "user", "content": "hello"}], "stream": true}),
            &[],
        )
        .await;
        assert_eq!(s, 200);
        let chunks = sse_data(&b);
        assert!(chunks[0]["prompt_filter_results"].is_array());

        let (s, _, b) = send(
            &app,
            "/ai/azure/openai/deployments/emb/embeddings?api-version=1",
            &json!({"input": "x"}),
            &[],
        )
        .await;
        assert_eq!(s, 200);
        assert_eq!(json(&b)["model"], "emb");
        let (s, _, _) = send(
            &app,
            "/ai/azure/openai/deployments/inst/completions?api-version=1",
            &json!({"prompt": "x"}),
            &[],
        )
        .await;
        assert_eq!(s, 200);
    }
}
