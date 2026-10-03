//! Cohere-style rerank and embed: `/ai/cohere/v2/rerank`, `/ai/cohere/v2/embed`,
//! plus the generic `/ai/v1/rerank` alias (also used by Jina / Voyage style clients).
//!
//! Relevance scores are deterministic: query-word overlap blended with the
//! cosine similarity of the mock embeddings (see [`crate::ai::embed`]).

use axum::body::Bytes;
use axum::response::Response;
use axum::routing::post;
use axum::{Extension, Router};
use serde_json::{json, Value};

use super::faults::ErrorKind;
use super::{embed, json_response, AiCtx, Served};
use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

fn doc_text(d: &Value) -> String {
    match d {
        Value::String(s) => s.clone(),
        Value::Object(o) => o
            .get("text")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| d.to_string()),
        other => other.to_string(),
    }
}

async fn rerank(Extension(ctx): Extension<AiCtx>, raw: Bytes) -> Response {
    let body: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return ctx.error(ErrorKind::BadRequest, format!("invalid request: {e}")),
    };
    let Some(query) = body.get("query").and_then(Value::as_str) else {
        return ctx.error(ErrorKind::BadRequest, "invalid request: query is required");
    };
    let Some(docs) = body.get("documents").and_then(Value::as_array) else {
        return ctx.error(
            ErrorKind::BadRequest,
            "invalid request: documents is required",
        );
    };
    if docs.len() > ctx.shared.max_inputs() {
        return ctx.error(
            ErrorKind::BadRequest,
            format!(
                "invalid request: at most {} documents",
                ctx.shared.max_inputs()
            ),
        );
    }
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("rerank-v3.5")
        .to_string();
    let top_n = body
        .get("top_n")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(docs.len())
        .min(docs.len());
    let return_docs = body
        .get("return_documents")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let texts: Vec<String> = docs.iter().map(doc_text).collect();
    let mut scored: Vec<(usize, f64)> = texts
        .iter()
        .enumerate()
        .map(|(i, t)| (i, embed::relevance(query, t)))
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    let results: Vec<Value> = scored
        .iter()
        .take(top_n)
        .map(|(i, s)| {
            let mut r = json!({"index": i, "relevance_score": s});
            if return_docs {
                r["document"] = json!({"text": texts[*i]});
            }
            r
        })
        .collect();
    let tokens: u32 =
        super::tokens::count(query) + texts.iter().map(|t| super::tokens::count(t)).sum::<u32>();
    ctx.record(super::RecordArgs {
        body: &raw,
        model: &model,
        mode: "rerank",
        stream: false,
        prompt: query,
        prompt_tokens: tokens,
        completion_tokens: 0,
        finish: "stop",
        reply: &format!("{} results", results.len()),
    });
    json_response(
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "results": results,
            "model": model,
            "usage": {"total_tokens": tokens},
            "meta": {"api_version": {"version": "2"}, "billed_units": {"search_units": 1}}
        }),
        Served {
            model,
            mode: None,
            tokens,
        },
    )
}

async fn cohere_embed(Extension(ctx): Extension<AiCtx>, raw: Bytes) -> Response {
    let body: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return ctx.error(ErrorKind::BadRequest, format!("invalid request: {e}")),
    };
    let texts: Vec<String> = body
        .get("texts")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .or_else(|| {
            body.get("inputs").and_then(Value::as_array).map(|a| {
                a.iter()
                    .map(|i| {
                        i.get("content")
                            .and_then(Value::as_array)
                            .map(|c| {
                                c.iter()
                                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            })
        })
        .unwrap_or_default();
    if texts.is_empty() {
        return ctx.error(
            ErrorKind::BadRequest,
            "invalid request: texts must not be empty",
        );
    }
    if texts.len() > ctx.shared.max_inputs() {
        return ctx.error(ErrorKind::BadRequest, "invalid request: too many texts");
    }
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("embed-v4.0")
        .to_string();
    let dims = body
        .get("output_dimension")
        .and_then(Value::as_u64)
        .map(|d| d as usize)
        .unwrap_or(1024);
    if dims == 0 || dims > ctx.shared.max_dims() {
        return ctx.error(
            ErrorKind::BadRequest,
            "invalid request: output_dimension out of range",
        );
    }
    let types: Vec<String> = body
        .get("embedding_types")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_else(|| vec!["float".into()]);
    let vectors: Vec<Vec<f32>> = texts.iter().map(|t| embed::embed(t, dims)).collect();
    let mut embeddings = serde_json::Map::new();
    for t in &types {
        let v: Value = match t.as_str() {
            "int8" => json!(vectors
                .iter()
                .map(|v| v
                    .iter()
                    .map(|x| (x * 127.0).round() as i8)
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>()),
            "uint8" => json!(vectors
                .iter()
                .map(|v| v
                    .iter()
                    .map(|x| ((x + 1.0) * 127.5).round() as u8)
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>()),
            "base64" => json!(vectors
                .iter()
                .map(|v| embed::to_base64(v))
                .collect::<Vec<_>>()),
            _ => json!(vectors),
        };
        embeddings.insert(t.clone(), v);
    }
    let tokens: u32 = texts.iter().map(|t| super::tokens::count(t)).sum();
    ctx.record(super::RecordArgs {
        body: &raw,
        model: &model,
        mode: "embedding",
        stream: false,
        prompt: &texts.join("\n"),
        prompt_tokens: tokens,
        completion_tokens: 0,
        finish: "stop",
        reply: "",
    });
    json_response(
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "embeddings": embeddings,
            "texts": texts,
            "response_type": "embeddings_by_type",
            "meta": {"api_version": {"version": "2"}, "billed_units": {"input_tokens": tokens}}
        }),
        Served {
            model,
            mode: None,
            tokens,
        },
    )
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ai/cohere/v2/rerank", post(rerank))
        .route("/ai/cohere/v2/embed", post(cohere_embed))
        .route("/ai/v1/rerank", post(rerank))
}

const EX: &str = r#"{"model":"rerank-v3.5","query":"What is the capital of France?","documents":["Paris is the capital of France.","Bananas are yellow.","France is in Europe."],"top_n":2}"#;

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new("/ai/cohere/v2/rerank", &["POST"], category::AI_MOCK, "Rerank (deterministic relevance scores, top_n)")
            .example(Example::post("Rerank", "/ai/cohere/v2/rerank").bearer("cohere-demo-key").json(EX)),
        Endpoint::new("/ai/cohere/v2/embed", &["POST"], category::AI_MOCK, "Cohere embed (float, int8, uint8, base64)")
            .example(Example::post("Cohere embed", "/ai/cohere/v2/embed").json(r#"{"model":"embed-v4.0","texts":["Hello world"],"input_type":"search_document","embedding_types":["float"]}"#)),
        Endpoint::new("/ai/v1/rerank", &["POST"], category::AI_MOCK, "Generic rerank alias (same as /ai/cohere/v2/rerank)")
            .example(Example::post("Rerank (generic)", "/ai/v1/rerank").json(r#"{"query":"capital of France","documents":["Paris is the capital of France.","Bananas are yellow."],"return_documents":true}"#)),
    ]
}

pub fn openapi_paths() -> Value {
    let rerank = |id: &str| {
        json!({"post": {
            "tags": ["AI Gateway"], "summary": "Rerank documents", "operationId": id,
            "requestBody": {"required": true, "content": {"application/json": {"schema": {"type": "object", "required": ["query", "documents"], "properties": {
                "model": {"type": "string"}, "query": {"type": "string"},
                "documents": {"type": "array", "items": {}}, "top_n": {"type": "integer"}, "return_documents": {"type": "boolean"}
            }}}}},
            "responses": {"200": {"description": "Results sorted by relevance", "content": {"application/json": {"schema": {"type": "object", "properties": {
                "results": {"type": "array", "items": {"type": "object", "properties": {"index": {"type": "integer"}, "relevance_score": {"type": "number"}}}}
            }}}}}, "400": {"description": "Invalid request"}}
        }})
    };
    json!({
        "/ai/cohere/v2/rerank": rerank("cohereRerank"),
        "/ai/v1/rerank": rerank("rerank"),
        "/ai/cohere/v2/embed": {"post": {
            "tags": ["AI Gateway"], "summary": "Cohere embed", "operationId": "cohereEmbed",
            "requestBody": {"required": true, "content": {"application/json": {"schema": {"type": "object", "properties": {
                "model": {"type": "string"}, "texts": {"type": "array", "items": {"type": "string"}},
                "input_type": {"type": "string"}, "embedding_types": {"type": "array", "items": {"type": "string"}},
                "output_dimension": {"type": "integer"}
            }}}}},
            "responses": {"200": {"description": "Embeddings by type"}, "400": {"description": "Invalid request"}}
        }},
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    #[tokio::test]
    async fn rerank_orders_and_limits() {
        let app = app();
        let (s, _, b) = send(
            &app,
            "/ai/cohere/v2/rerank",
            &serde_json::from_str(EX).expect("json"),
            &[],
        )
        .await;
        assert_eq!(s, 200);
        let v = json(&b);
        let r = v["results"].as_array().expect("results");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0]["index"], 0);
        assert!(r[0]["relevance_score"].as_f64() >= r[1]["relevance_score"].as_f64());
        let (_, _, b2) = send(
            &app,
            "/ai/v1/rerank",
            &serde_json::from_str(EX).expect("json"),
            &[],
        )
        .await;
        assert_eq!(json(&b2)["results"], v["results"]);
        let (s, _, _) = send(&app, "/ai/v1/rerank", &json!({"documents": []}), &[]).await;
        assert_eq!(s, 400);
    }

    #[tokio::test]
    async fn embed_types() {
        let app = app();
        let (_, _, b) = send(&app, "/ai/cohere/v2/embed", &json!({"texts": ["a b"], "embedding_types": ["float", "int8"], "output_dimension": 256}), &[]).await;
        let v = json(&b);
        assert_eq!(
            v["embeddings"]["float"][0].as_array().map(Vec::len),
            Some(256)
        );
        assert!(v["embeddings"]["int8"][0].is_array());
    }
}
