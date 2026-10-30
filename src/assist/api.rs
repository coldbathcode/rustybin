//! The routes of AI assist.

use std::convert::Infallible;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

use super::provider::Kind;
use super::{models, task, tasks, Out, Refusal, RunParams};
use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

pub const STATUS_PATH: &str = "/_rustybin/assist";
pub const TEST_PATH: &str = "/_rustybin/assist/test";
pub const RUN_PATH: &str = "/_rustybin/assist/run";
pub const USAGE_PATH: &str = "/_rustybin/assist/usage";

/// Largest `run` body.
const MAX_BODY: usize = 256 * 1024;

pub const PRIVACY: &str = "Prompts carry only what the task names (the question, the view's \
context document, the selected request or spec) and go to the configured provider over TLS \
through the outbound policy. Keys never leave the instance. Inputs that contain a stored secret \
or the admin token are refused before any call. Every answer shows exactly what was sent.";

fn error(status: StatusCode, message: &str, reason: &str) -> Response {
    (status, Json(json!({ "error": message, "reason": reason }))).into_response()
}

/// Who calls, for the caps: the control principal, plus the client
/// address when the control plane is open.
fn principal(headers: &HeaderMap, extensions: &Extensions, state: &AppState) -> String {
    let who = crate::settings::principal(headers, &state.config);
    if who == "open" {
        let ip = crate::session::client_ip(headers, extensions, &state.config)
            .map(|ip| ip.to_string())
            .unwrap_or_default();
        format!("open@{ip}")
    } else {
        who
    }
}

async fn status_handler(State(state): State<AppState>) -> Response {
    Json(status_json(&state)).into_response()
}

async fn test_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    match state.assist.test().await {
        Ok(v) => Json(v).into_response(),
        Err(e) => {
            let status = state.assist.status();
            let (code, reason) = if status.enabled {
                (StatusCode::BAD_GATEWAY, "provider_error")
            } else {
                (StatusCode::SERVICE_UNAVAILABLE, "assist_disabled")
            };
            (
                code,
                Json(json!({
                    "ok": false,
                    "error": e,
                    "reason": reason,
                    "provider": status.kind.map(|k| k.as_str()),
                    "model": status.model,
                })),
            )
                .into_response()
        }
    }
}

async fn usage_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    Json(state.assist.usage()).into_response()
}

#[derive(Deserialize, Default)]
struct RunBody {
    #[serde(default)]
    task: String,
    #[serde(default)]
    inputs: Value,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    stream: Option<bool>,
}

/// A refusal as `(status, message, reason)`; the tools reuse it.
pub fn refusal_parts(r: &Refusal) -> (StatusCode, String, &'static str) {
    match r {
        Refusal::Disabled(m) => (
            StatusCode::SERVICE_UNAVAILABLE,
            m.clone(),
            "assist_disabled",
        ),
        Refusal::RateLimited(wait) => (
            StatusCode::TOO_MANY_REQUESTS,
            format!("too many AI calls: try again in {wait} s"),
            "rate_limited",
        ),
        Refusal::Invalid(m) => (StatusCode::BAD_REQUEST, m.clone(), "invalid_inputs"),
        Refusal::Secret(name) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("the inputs contain the value of {name}: nothing was sent"),
            "secret_in_inputs",
        ),
    }
}

/// The principal of an assist call for the caps (also the tools').
pub fn principal_of(headers: &HeaderMap, extensions: &Extensions, state: &AppState) -> String {
    principal(headers, extensions, state)
}

/// The status document of `GET /_rustybin/assist`.
pub fn status_json(state: &AppState) -> Value {
    let assist = &state.assist;
    assist.sync_egress();
    let status = assist.status();
    let providers: Vec<Value> = Kind::ALL
        .iter()
        .map(|k| {
            json!({
                "id": k.as_str(),
                "title": k.title(),
                "key_setting": k.key_setting(),
                "key": k.key_setting().map(|s| state.settings.secret_state(s)),
                "base_url": if *k == Kind::Compatible { status.base_url.clone() } else { k.default_base_url().map(str::to_string) },
                "default_model": models::default_model(*k),
                "default_fast_model": models::default_fast_model(*k),
                "models": models::for_provider(*k).iter().map(|m| m.describe()).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "enabled": status.enabled,
        "reason": status.reason,
        "provider": status.kind.map(|k| k.as_str()),
        "model": (!status.model.is_empty()).then_some(&status.model),
        "fast_model": (!status.fast_model.is_empty()).then_some(&status.fast_model),
        "base_url": status.base_url,
        "key_set": status.key_set,
        "public_mode": assist.public_mode(),
        "caps": {
            "calls_per_minute": assist.calls_per_minute(),
            "max_output_tokens": assist.max_output_tokens(),
        },
        "providers": providers,
        "tasks": task::all().iter().map(task::Task::describe).collect::<Vec<_>>(),
        "suggestions": tasks::suggest::static_table(),
        "usage": assist.usage()["total"],
        "privacy": PRIVACY,
    })
}

fn refusal(r: Refusal) -> Response {
    match r {
        Refusal::Disabled(m) => error(StatusCode::SERVICE_UNAVAILABLE, &m, "assist_disabled"),
        Refusal::RateLimited(wait) => {
            let mut resp = (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({
                    "error": format!("too many AI calls: try again in {wait} s"),
                    "reason": "rate_limited",
                    "retry_after": wait,
                })),
            )
                .into_response();
            if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
                resp.headers_mut().insert(header::RETRY_AFTER, v);
            }
            resp
        }
        Refusal::Invalid(m) => error(StatusCode::BAD_REQUEST, &m, "invalid_inputs"),
        Refusal::Secret(name) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("the inputs contain the value of {name}: nothing was sent"),
            "secret_in_inputs",
        ),
    }
}

fn sse_frame(out: &Out) -> Bytes {
    let (event, data) = out.sse();
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

async fn run_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    body: Bytes,
) -> Response {
    if body.len() > MAX_BODY {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the run body is larger than 256 KiB",
            "too_large",
        );
    }
    let body: RunBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                &format!("expected {{ \"task\": \"...\", \"inputs\": {{...}}, \"model\"?: \"...\", \"stream\"?: true }}: {e}"),
                "invalid_body",
            )
        }
    };
    let Some(task) = task::find(&body.task) else {
        return error(
            StatusCode::NOT_FOUND,
            &format!(
                "no task named {:?}; the tasks are {}",
                body.task,
                task::all()
                    .iter()
                    .map(|t| t.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "unknown_task",
        );
    };
    if task.writes {
        if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
            return resp;
        }
    }
    let wants_stream = body.stream.unwrap_or_else(|| {
        headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|a| a.contains("text/event-stream"))
    });
    let params = RunParams {
        task,
        inputs: body.inputs,
        model: body.model,
        principal: principal(&headers, &extensions, &state),
    };
    let stream = match Arc::clone(&state.assist).run(params) {
        Ok(s) => s,
        Err(r) => return refusal(r),
    };
    if wants_stream {
        let body = Body::from_stream(stream.map(|out| Ok::<_, Infallible>(sse_frame(&out))));
        let mut resp = Response::new(body);
        let h = resp.headers_mut();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        h.insert("x-accel-buffering", HeaderValue::from_static("no"));
        return resp;
    }
    match collect(task, stream).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(e)).into_response(),
    }
}

/// Collect a run into one answer (`{task, text, json, actions, usage,
/// sent}`), or the provider's failure. Shared by the non-streaming route
/// and the management tool `run_assist_task`.
pub async fn collect(task: &task::Task, stream: super::OutStream) -> Result<Value, Value> {
    let mut text = String::new();
    let mut json = None;
    let mut actions = Vec::new();
    let mut usage = Value::Null;
    let mut sent = Value::Null;
    let mut failure: Option<(String, u16)> = None;
    let mut stream = stream;
    while let Some(out) = stream.next().await {
        match out {
            Out::Sent(v) => sent = v,
            Out::Progress(_) => {}
            Out::Delta(d) => text.push_str(&d),
            Out::Json(v) => json = Some(v),
            Out::Action(a) => actions.push(a),
            Out::Usage { .. } => usage = out.sse().1,
            Out::Error { message, status } => failure = Some((message, status)),
            Out::Done => {}
        }
    }
    let (clean_text, _) = task::extract_actions(&text);
    if let Some((message, status)) = failure {
        return Err(json!({
            "error": message,
            "reason": "provider_error",
            "provider_status": status,
            "task": task.name,
            "sent": sent,
            "usage": usage,
        }));
    }
    Ok(json!({
        "task": task.name,
        "text": if task.schema.is_some() { Value::Null } else { json!(clean_text) },
        "json": json,
        "actions": actions,
        "usage": usage,
        "sent": sent,
    }))
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(STATUS_PATH, get(status_handler))
        .route(TEST_PATH, post(test_handler))
        .route(RUN_PATH, post(run_handler))
        .route(USAGE_PATH, get(usage_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            STATUS_PATH,
            &["GET"],
            category::CONTROL,
            "AI assist: the provider, models, whether a key is set, the tasks and usage",
        )
        .example(Example::get("AI assist status", STATUS_PATH)),
        Endpoint::new(
            TEST_PATH,
            &["POST"],
            category::CONTROL,
            "AI assist: one short call to the configured provider (admin)",
        )
        .example(Example::post("Test the AI provider", TEST_PATH).expect_status(503)),
        Endpoint::new(
            RUN_PATH,
            &["POST"],
            category::CONTROL,
            "AI assist: run a task (free_answer, explain, suggest) as SSE or one JSON answer",
        )
        .description(
            "{task, inputs, model?, stream?}. Streams events sent, delta, json, action, usage, \
             error, done; with stream false answers one JSON document. 503 until a provider and \
             key are set, 429 over the caller's calls per minute, 422 when the inputs hold a \
             stored secret.",
        )
        .example(
            Example::post("Ask a question", RUN_PATH)
                .json(r#"{"task":"free_answer","inputs":{"question":"What does /anything show?","view":"overview"},"stream":false}"#)
                .expect_status(503),
        ),
        Endpoint::new(
            USAGE_PATH,
            &["GET"],
            category::CONTROL,
            "AI assist: calls, tokens, estimated cost per provider, model and task (admin)",
        )
        .example(Example::get("AI assist usage", USAGE_PATH)),
    ]
}

pub fn openapi_paths() -> Value {
    json!({
        STATUS_PATH: {
            "get": {
                "tags": ["Control Plane"],
                "summary": "AI assist status",
                "description": "Whether AI assist is on (a provider and, except for the mock, a key), the effective models, the providers with their curated models and key state ({set, hint}), the tasks, the static suggestions per console view, the usage totals and the privacy note.",
                "operationId": "getAssist",
                "responses": { "200": { "description": "Status", "content": { "application/json": { "schema": { "type": "object" } } } } }
            }
        },
        TEST_PATH: {
            "post": {
                "tags": ["Control Plane"],
                "summary": "Test the AI provider",
                "description": "One short call (\"Reply with the single word OK\") to the configured provider: latency, model and the answer, or the provider's error. Admin.",
                "operationId": "testAssist",
                "responses": {
                    "200": { "description": "The provider answered", "content": { "application/json": { "schema": { "type": "object", "properties": {
                        "ok": { "type": "boolean" }, "provider": { "type": "string" }, "model": { "type": "string" },
                        "latency_ms": { "type": "integer" }, "answer": { "type": "string" }
                    } } } } },
                    "502": { "description": "The provider refused or failed" },
                    "503": { "description": "No provider or key is set" }
                }
            }
        },
        RUN_PATH: {
            "post": {
                "tags": ["Control Plane"],
                "summary": "Run an AI assist task",
                "description": "Runs a task with validated inputs. Streamed (stream true, or Accept: text/event-stream): events `sent` (what went to the provider, never the key), `delta` ({text}), `json` (the validated structured answer), `action` (an action a text answer proposed: open_view, copy, run_request), `usage` (tokens and estimated cost), `error`, `done`. Otherwise one JSON document {task, text, json, actions, usage, sent}. Tasks that change the instance need admin; the others the console scope.",
                "operationId": "runAssist",
                "requestBody": { "required": true, "content": { "application/json": { "schema": {
                    "type": "object",
                    "required": ["task", "inputs"],
                    "properties": {
                        "task": { "type": "string", "enum": ["free_answer", "explain", "suggest"] },
                        "inputs": { "type": "object" },
                        "model": { "type": "string", "description": "A model id instead of the task's default" },
                        "stream": { "type": "boolean" }
                    }
                } } } },
                "responses": {
                    "200": { "description": "The answer (SSE or JSON)" },
                    "400": { "description": "Invalid inputs" },
                    "404": { "description": "Unknown task" },
                    "422": { "description": "The inputs contain a stored secret or the admin token" },
                    "429": { "description": "The caller's calls per minute (Retry-After)" },
                    "502": { "description": "The provider failed (JSON mode)" },
                    "503": { "description": "No provider or key is set" }
                }
            }
        },
        USAGE_PATH: {
            "get": {
                "tags": ["Control Plane"],
                "summary": "AI assist usage",
                "description": "Calls, errors, tokens and the estimated cost since start, in total, per provider and model, and per task. Admin.",
                "operationId": "getAssistUsage",
                "responses": { "200": { "description": "Usage", "content": { "application/json": { "schema": { "type": "object" } } } } }
            }
        }
    })
}
