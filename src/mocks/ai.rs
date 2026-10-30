//! AI on mocks (`docs/plans/next/step-04-ai-mocks.md`): a spec from a
//! description, examples for the operations that lack them, an edit of the
//! spec through an instruction, and the example coverage. Every model call
//! is an assist task (`crate::assist::tasks`); these routes add the mock
//! context (the draft's spec, the manage guard, the store update) and the
//! streamed progress the console shows.
//!
//! | Route | | |
//! |---|---|---|
//! | `POST /_rustybin/mocks/ai/spec` | manage | `{description, openapi_version, style}`: SSE progress, then the document and its preview; nothing is created |
//! | `POST /_rustybin/mocks/{name}/ai/examples` | manage | `{operations?, per_response, persona, source, apply}`: preview, or store the examples in the draft |
//! | `POST /_rustybin/mocks/{name}/ai/edit` | manage | `{instruction, apply}`: the new document, the diff summary and its checks; `apply` saves it as the draft |
//! | `GET /_rustybin/mocks/{name}/coverage` | read | Which operations have examples |
//!
//! The instance's key belongs to the operator: self-service visitors get
//! `403` unless `RUSTYBIN_ASSIST_VISITORS=true`.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::model::{ExampleSource, OperationSettings, StoredExample, MAX_STORED_EXAMPLES};
use super::service::{PatchBody, ServiceError, Svc, SvcResult};
use crate::assist::tasks::{edit_spec, examples_for_operations};
use crate::assist::{task, Out, Refusal, RunParams};
use crate::state::AppState;

pub const SPEC_PATH: &str = "/_rustybin/mocks/ai/spec";
pub const EXAMPLES_PATH: &str = "/_rustybin/mocks/{name}/ai/examples";
pub const EDIT_PATH: &str = "/_rustybin/mocks/{name}/ai/edit";
pub const COVERAGE_PATH: &str = "/_rustybin/mocks/{name}/coverage";

const MAX_BODY: usize = 256 * 1024;

#[derive(Deserialize)]
pub struct MockPath {
    pub name: String,
}

fn svc(state: &AppState, headers: &HeaderMap, extensions: &Extensions, uri: &Uri) -> Svc {
    Svc::from_request(state, headers, extensions, uri)
}

fn parse_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, ServiceError> {
    if body.len() > MAX_BODY {
        return Err(ServiceError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the body is larger than 256 KiB",
        ));
    }
    serde_json::from_slice(body)
        .map_err(|e| ServiceError::bad_request(format!("invalid body: {e}")))
}

/// May the caller use the instance's AI on mocks? The operator always; a
/// self-service visitor with `RUSTYBIN_ASSIST_VISITORS`.
pub fn ai_allowed(s: &Svc) -> SvcResult<()> {
    if s.caller.admin {
        return Ok(());
    }
    if s.state.mocks.visits().is_some() {
        if s.state.config.assist_visitors {
            return Ok(());
        }
        return Err(ServiceError::forbidden(
            "AI assist on this instance is for the operator: visitors may use it only with RUSTYBIN_ASSIST_VISITORS=true",
        ));
    }
    s.require_admin()
}

fn wants_stream(headers: &HeaderMap, body_flag: Option<bool>) -> bool {
    body_flag.unwrap_or_else(|| {
        headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|a| a.contains("text/event-stream"))
    })
}

pub fn principal(headers: &HeaderMap, extensions: &Extensions, state: &AppState) -> String {
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

/// A refusal as the JSON of a failed batch (`reason` drives the status).
fn refusal_json(r: &Refusal) -> Value {
    match r {
        Refusal::Disabled(m) => json!({ "error": m, "reason": "assist_disabled" }),
        Refusal::RateLimited(wait) => {
            json!({ "error": format!("too many AI calls: try again in {wait} s"), "reason": "rate_limited", "retry_after": wait })
        }
        Refusal::Invalid(m) => json!({ "error": m, "reason": "invalid_inputs" }),
        Refusal::Secret(name) => {
            json!({ "error": format!("the inputs contain the value of {name}: nothing was sent"), "reason": "secret_in_inputs" })
        }
    }
}

/// A run's outcome once collected: the structured answer, or the failure.
struct Collected {
    json: Option<Value>,
    error: Option<(String, u16)>,
    usage: Value,
    sent: Value,
}

/// An event of the mock AI streams.
fn frame(event: &str, data: &Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

fn sse(body: Body) -> Response {
    let mut resp = Response::new(body);
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    resp
}

/// Run a task, forwarding its events to `emit` (SSE) or dropping them, and
/// collect the result.
async fn run_collect(stream: crate::assist::OutStream, mut emit: impl FnMut(Bytes)) -> Collected {
    let mut out = Collected {
        json: None,
        error: None,
        usage: Value::Null,
        sent: Value::Null,
    };
    let mut stream = stream;
    while let Some(ev) = stream.next().await {
        match &ev {
            Out::Sent(v) => out.sent = v.clone(),
            Out::Json(v) => out.json = Some(v.clone()),
            Out::Usage { .. } => out.usage = ev.sse().1,
            Out::Error { message, status } => out.error = Some((message.clone(), *status)),
            _ => {}
        }
        match &ev {
            Out::Progress(_) | Out::Usage { .. } | Out::Error { .. } | Out::Sent(_) => {
                let (name, data) = ev.sse();
                emit(frame(name, &data));
            }
            _ => {}
        }
    }
    out
}

fn failure(c: &Collected, task: &str) -> Value {
    let (message, status) = c
        .error
        .clone()
        .unwrap_or_else(|| ("the task produced no answer".into(), 0));
    json!({
        "error": message,
        "reason": "provider_error",
        "provider_status": status,
        "task": task,
        "usage": c.usage,
        "sent": c.sent,
    })
}

/// Progress frames of a run, for the SSE answer (the tools drop them).
pub type Progress = tokio::sync::mpsc::UnboundedSender<Bytes>;

/// The status a failed work's `reason` maps to.
fn status_of(reason: Option<&str>) -> StatusCode {
    match reason {
        Some("assist_disabled") => StatusCode::SERVICE_UNAVAILABLE,
        Some("rate_limited") => StatusCode::TOO_MANY_REQUESTS,
        Some("secret_in_inputs") => StatusCode::UNPROCESSABLE_ENTITY,
        Some("generator")
        | Some("invalid_settings")
        | Some("invalid_inputs")
        | Some("invalid_spec") => StatusCode::BAD_REQUEST,
        Some("not_found") => StatusCode::NOT_FOUND,
        Some("forbidden") => StatusCode::FORBIDDEN,
        Some("nothing_to_do") => StatusCode::CONFLICT,
        _ => StatusCode::BAD_GATEWAY,
    }
}

/// Answer a work: the SSE stream (progress, then `result` or `error`, then
/// `done`) or one JSON document.
async fn finish(
    streaming: bool,
    rx: tokio::sync::mpsc::UnboundedReceiver<Bytes>,
    work: impl std::future::Future<Output = Result<Value, Value>> + Send + 'static,
) -> Result<Response, ServiceError> {
    if streaming {
        let out = async_stream::stream! {
            let mut rx = rx;
            let result = work.await;
            while let Ok(b) = rx.try_recv() {
                yield Ok::<_, Infallible>(b);
            }
            match result {
                Ok(v) => yield Ok(frame("result", &v)),
                Err(e) => yield Ok(frame("error", &e)),
            }
            yield Ok(frame("done", &json!({})));
        };
        return Ok(sse(Body::from_stream(out)));
    }
    drop(rx);
    match work.await {
        Ok(v) => Ok(Json(v).into_response()),
        Err(e) => Ok((status_of(e["reason"].as_str()), Json(e)).into_response()),
    }
}

/// A `ServiceError` as a failed work's document.
fn service_json(e: &ServiceError) -> Value {
    let mut body = e.body.clone();
    if !body.is_object() {
        body = json!({ "error": e.message() });
    }
    if body.get("reason").is_none() {
        let reason = match e.status {
            StatusCode::FORBIDDEN => "forbidden",
            StatusCode::NOT_FOUND => "not_found",
            StatusCode::SERVICE_UNAVAILABLE => "assist_disabled",
            StatusCode::TOO_MANY_REQUESTS => "rate_limited",
            StatusCode::UNPROCESSABLE_ENTITY => "secret_in_inputs",
            _ => "invalid_inputs",
        };
        body["reason"] = json!(reason);
    }
    body
}

// ── POST /_rustybin/mocks/ai/spec ───────────────────────────────────

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SpecBody {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub openapi_version: Option<String>,
    #[serde(default)]
    pub style: Option<Value>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub stream: Option<bool>,
}

/// Generate a spec from a description, then preview it: `{spec, text,
/// preview, usage, sent}`, or the failure document (`reason`). Shared by the
/// route and the management tool `generate_spec`.
pub async fn spec_work(
    state: AppState,
    s: Svc,
    req: SpecBody,
    principal: String,
    tx: Progress,
) -> Result<Value, Value> {
    ai_allowed(&s).map_err(|e| service_json(&e))?;
    let taken: Vec<String> = state.mocks.list().iter().map(|m| m.name.clone()).collect();
    let mut inputs = json!({
        "description": req.description,
        "openapi_version": req.openapi_version.unwrap_or_else(|| "3.1.0".to_string()),
        "avoid_names": taken,
    });
    if let Some(style) = req.style {
        inputs["style"] = style;
    }
    let task = task::find("spec_from_description")
        .ok_or_else(|| json!({ "error": "no task", "reason": "internal" }))?;
    let stream = Arc::clone(&state.assist)
        .run(RunParams {
            task,
            inputs,
            model: req.model,
            principal,
        })
        .map_err(|r| refusal_json(&r))?;
    let collected = run_collect(stream, |b| {
        let _ = tx.send(b);
    })
    .await;
    let Some(doc) = &collected.json else {
        return Err(failure(&collected, "spec_from_description"));
    };
    let _ = tx.send(frame(
        "progress",
        &json!({ "message": "checking the document" }),
    ));
    let text = serde_json::to_string_pretty(doc).unwrap_or_default();
    match s.preview(&text) {
        Ok(preview) => Ok(
            json!({ "spec": doc, "text": text, "preview": preview, "usage": collected.usage, "sent": collected.sent }),
        ),
        Err(e) => Err(
            json!({ "error": e.message(), "reason": "invalid_spec", "usage": collected.usage, "sent": collected.sent }),
        ),
    }
}

pub async fn spec(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    ai_allowed(&s)?;
    let req: SpecBody = parse_body(&body)?;
    let streaming = wants_stream(&headers, req.stream);
    let who = principal(&headers, &extensions, &state);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    finish(streaming, rx, spec_work(state.clone(), s, req, who, tx)).await
}

// ── POST /_rustybin/mocks/{name}/ai/examples ────────────────────────

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ExamplesBody {
    #[serde(default)]
    pub operations: Option<Vec<String>>,
    #[serde(default)]
    pub per_response: Option<u64>,
    #[serde(default)]
    pub persona: Option<String>,
    /// `ai` (default) or `generated` (the deterministic generator, no key).
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub apply: Option<bool>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub stream: Option<bool>,
}

/// Merge generated examples into the draft's operation settings: the AI's
/// and the generator's replace earlier ones of their source for the same
/// status; a person's stay.
fn merge_examples(
    operations: &mut BTreeMap<String, OperationSettings>,
    examples: &Map<String, Value>,
    source: ExampleSource,
) -> usize {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut stored = 0;
    for (op, by_status) in examples {
        let Some(by_status) = by_status.as_object() else {
            continue;
        };
        let settings = operations.entry(op.clone()).or_default();
        for (status, list) in by_status {
            let Some(list) = list.as_array() else {
                continue;
            };
            let slot = settings.examples.entry(status.clone()).or_default();
            slot.retain(|e| e.source != source);
            let prefix = match source {
                ExampleSource::Ai => "ai",
                ExampleSource::Generated => "generated",
                ExampleSource::User => "user",
            };
            for (i, value) in list.iter().enumerate() {
                if slot.len() >= MAX_STORED_EXAMPLES {
                    break;
                }
                slot.push(StoredExample {
                    name: format!("{prefix}-{}", i + 1),
                    value: value.clone(),
                    source,
                    created_at: Some(now.clone()),
                });
                stored += 1;
            }
        }
    }
    stored
}

/// Examples for the operations of a mock that lack one (or the ones
/// named), from the AI or the generator, previewed or stored in the draft
/// (`apply`). `s` passed the manage guard for `name`. Shared by the route
/// and the management tool `generate_examples`.
pub async fn examples_work(
    state: AppState,
    s: Svc,
    name: String,
    req: ExamplesBody,
    principal: String,
    tx: Progress,
) -> Result<Value, Value> {
    let snap = s.manageable(&name).map_err(|e| service_json(&e))?;
    let generated = req.source.as_deref() == Some("generated");
    if !generated {
        ai_allowed(&s).map_err(|e| service_json(&e))?;
        // No provider: say so before any work.
        state
            .assist
            .client()
            .map_err(|m| refusal_json(&Refusal::Disabled(m)))?;
    }
    let doc: Value = serde_json::from_str(&snap.draft.source)
        .or_else(|_| serde_yaml::from_str(&snap.draft.source))
        .map_err(|e| json!({ "error": format!("the draft's spec cannot be read: {e}"), "reason": "invalid_spec" }))?;
    let per_response = req.per_response.unwrap_or(1);
    let persona = req.persona.clone();
    let apply = req.apply.unwrap_or(false);
    let source = if generated {
        ExampleSource::Generated
    } else {
        ExampleSource::Ai
    };
    // The operations to cover, in batches.
    let compiled = &snap.draft;
    let wanted: Vec<String> = match &req.operations {
        Some(ids) => ids.clone(),
        None => compiled
            .report
            .coverage
            .without
            .iter()
            .map(|o| o.id.clone())
            .collect(),
    };
    if wanted.is_empty() {
        return Err(
            json!({ "error": "every operation has examples for its success responses", "reason": "nothing_to_do", "coverage": super::service::coverage_summary(compiled) }),
        );
    }
    let batches: Vec<Vec<String>> = wanted
        .chunks(examples_for_operations::BATCH)
        .map(|c| c.to_vec())
        .collect();
    let total = batches.len();
    let task = task::find("examples_for_operations")
        .ok_or_else(|| json!({ "error": "no task", "reason": "internal" }))?;
    let assist = Arc::clone(&state.assist);
    let mut all: Map<String, Value> = Map::new();
    let mut usage = Value::Null;
    for (i, batch) in batches.iter().enumerate() {
        let _ = tx.send(frame("progress", &json!({ "message": format!("operations {} to {} of {} (batch {} of {total})", i * examples_for_operations::BATCH + 1, i * examples_for_operations::BATCH + batch.len(), wanted.len(), i + 1), "batch": i + 1, "batches": total })));
        let inputs = json!({ "spec": doc, "operations": batch, "per_response": per_response, "persona": persona });
        let answer = if generated {
            examples_for_operations::generate(&inputs)
                .map_err(|e| json!({ "error": e, "reason": "generator" }))?
        } else {
            let stream = assist
                .run(RunParams {
                    task,
                    inputs,
                    model: req.model.clone(),
                    principal: principal.clone(),
                })
                .map_err(|r| refusal_json(&r))?;
            let c = run_collect(stream, |b| {
                let _ = tx.send(b);
            })
            .await;
            usage = c.usage.clone();
            match &c.json {
                Some(v) => v.clone(),
                None => return Err(failure(&c, "examples_for_operations")),
            }
        };
        if let Some(m) = answer.get("examples").and_then(Value::as_object) {
            for (k, val) in m {
                all.insert(k.clone(), val.clone());
            }
        }
    }
    let mut result =
        json!({ "examples": all, "source": source.as_str(), "applied": false, "usage": usage });
    if apply {
        let _ = tx.send(frame(
            "progress",
            &json!({ "message": "saving the examples in the draft" }),
        ));
        // The caller passed the manage guard above; the store update
        // reuses the draft's settings with the examples merged in.
        let draft = match state.mocks.get(&name) {
            Some(snap) => snap.draft,
            None => return Err(json!({ "error": "the mock is gone", "reason": "not_found" })),
        };
        let mut operations = draft.settings.operations.clone();
        let stored = merge_examples(&mut operations, &all, source);
        let compiled = state
            .mocks
            .recompile(&draft, None, {
                let mut settings = draft.settings.clone();
                settings.operations = operations;
                settings
            })
            .map_err(|e| json!({ "error": e, "reason": "invalid_settings" }))?;
        let snap = state
            .mocks
            .set_draft(&name, Some(compiled), None)
            .map_err(|e| json!({ "error": ServiceError::from(e).message(), "reason": "store" }))?;
        result["applied"] = json!(true);
        result["stored"] = json!(stored);
        result["coverage"] = super::service::coverage_summary(&snap.draft);
        result["draft_revision"] = json!(snap.draft.revision);
    }
    Ok(result)
}

pub async fn examples(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    s.manageable(&name)?;
    let req: ExamplesBody = parse_body(&body)?;
    if req.source.as_deref() != Some("generated") {
        ai_allowed(&s)?;
    }
    let streaming = wants_stream(&headers, req.stream);
    let who = principal(&headers, &extensions, &state);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    finish(
        streaming,
        rx,
        examples_work(state.clone(), s, name, req, who, tx),
    )
    .await
}

// ── POST /_rustybin/mocks/{name}/ai/edit ────────────────────────────

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct EditBody {
    #[serde(default)]
    pub instruction: String,
    #[serde(default)]
    pub apply: Option<bool>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub stream: Option<bool>,
}

/// Edit a mock's draft spec through an instruction: the new document, the
/// diff summary and its preview; `apply` saves it as the draft. Shared by
/// the route and the management tool `edit_spec_with_ai`.
pub async fn edit_work(
    state: AppState,
    s: Svc,
    name: String,
    req: EditBody,
    principal: String,
    tx: Progress,
) -> Result<Value, Value> {
    let snap = s.manageable(&name).map_err(|e| service_json(&e))?;
    ai_allowed(&s).map_err(|e| service_json(&e))?;
    let before: Value = serde_json::from_str(&snap.draft.source)
        .or_else(|_| serde_yaml::from_str(&snap.draft.source))
        .map_err(|e| json!({ "error": format!("the draft's spec cannot be read: {e}"), "reason": "invalid_spec" }))?;
    let inputs = json!({ "spec": before, "instruction": req.instruction });
    let task = task::find("edit_spec")
        .ok_or_else(|| json!({ "error": "no task", "reason": "internal" }))?;
    let stream = Arc::clone(&state.assist)
        .run(RunParams {
            task,
            inputs,
            model: req.model,
            principal,
        })
        .map_err(|r| refusal_json(&r))?;
    let apply = req.apply.unwrap_or(false);
    let instruction = req.instruction.clone();
    let before_text = snap.draft.source.to_string();
    let collected = run_collect(stream, |b| {
        let _ = tx.send(b);
    })
    .await;
    let Some(after) = collected.json.clone() else {
        return Err(failure(&collected, "edit_spec"));
    };
    let _ = tx.send(frame(
        "progress",
        &json!({ "message": "checking the new document" }),
    ));
    let text = serde_json::to_string_pretty(&after).unwrap_or_default();
    let preview = s.preview(&text).map_err(|e| {
        json!({ "error": e.message(), "reason": "invalid_spec", "usage": collected.usage, "sent": collected.sent })
    })?;
    let summary = edit_spec::diff(&before, &after).describe();
    let mut result = json!({
        "spec": after,
        "text": text,
        "before_text": before_text,
        "diff": summary,
        "preview": preview,
        "instruction": instruction,
        "applied": false,
        "usage": collected.usage,
        "sent": collected.sent,
    });
    if apply {
        let _ = tx.send(frame("progress", &json!({ "message": "saving the draft" })));
        let detail = s
            .update(
                &name,
                PatchBody {
                    name: None,
                    spec: Some(json!(text)),
                    strip_server_prefix: None,
                    responses: None,
                    security: None,
                    operations: None,
                    mcp: None,
                    deploy: None,
                    force: None,
                },
            )
            .map_err(|e| json!({ "error": e.message(), "reason": "store" }))?;
        result["applied"] = json!(true);
        result["detail"] = detail;
    }
    Ok(result)
}

pub async fn edit(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    s.manageable(&name)?;
    ai_allowed(&s)?;
    let req: EditBody = parse_body(&body)?;
    let streaming = wants_stream(&headers, req.stream);
    let who = principal(&headers, &extensions, &state);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    finish(
        streaming,
        rx,
        edit_work(state.clone(), s, name, req, who, tx),
    )
    .await
}

// ── GET /_rustybin/mocks/{name}/coverage ────────────────────────────

pub async fn coverage(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    let snap = s.readable(&name)?;
    let mut out = super::service::coverage_summary(&snap.draft);
    if let Value::Object(m) = &mut out {
        m.insert("name".into(), json!(snap.name));
        m.insert("revision".into(), json!(snap.draft.revision));
        m.insert(
            "live".into(),
            snap.live
                .as_ref()
                .map(|l| super::service::coverage_summary(l))
                .unwrap_or(Value::Null),
        );
    }
    Ok(Json(out).into_response())
}

#[cfg(test)]
mod tests;
