//! The harness control API under `/_rustybin/harness*` (plan section 9, the
//! single-node subset) and the verification token path.
//!
//! The status, the schema, the built-in scenarios and validation work on
//! every node (they send nothing). Everything else needs harness mode (404
//! otherwise, with a hint). Mutations follow the admin guard on an open
//! control plane; with `RUSTYBIN_CONTROL_AUTH` on, `control_auth::enforce`
//! already checked the `harness` (or admin) scope.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::scenario::{Caps, Scenario};
use super::{builtin_scenario, builtin_scenarios, Harness, RunError, PREFIX};
use crate::state::AppState;

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

fn off() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": "harness mode is off on this instance",
            "hint": "start with RUSTYBIN_HARNESS=on, RUSTYBIN_HARNESS_ROLES=client (or both) and RUSTYBIN_HARNESS_TARGETS=<hosts the harness may send load to>; see docs/plans/harness.md",
        })),
    )
        .into_response()
}

/// The node's harness, or the 404 answer.
#[allow(clippy::result_large_err)]
fn harness(state: &AppState) -> Result<Arc<Harness>, Response> {
    state.harness.clone().ok_or_else(off)
}

/// Mutations on an open control plane follow the admin guard (token set:
/// required; public mode is refused at startup anyway). With control auth
/// on, the enforce layer already checked the scope.
#[allow(clippy::result_large_err)]
fn guard_mutation(state: &AppState, headers: &HeaderMap) -> Result<(), Response> {
    if state.config.control_auth == crate::control_auth::ControlAuth::Open && !state.keys.shared() {
        crate::admin::require_admin(headers, &state.config)?;
    }
    Ok(())
}

fn run_error(e: RunError) -> Response {
    let status = StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_REQUEST);
    (status, Json(e.to_json())).into_response()
}

/// Parse a scenario body (JSON or YAML, any content type).
#[allow(clippy::result_large_err)]
fn parse_body(body: &Bytes) -> Result<Scenario, Response> {
    let text = std::str::from_utf8(body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "the scenario must be UTF-8 text"))?;
    Scenario::parse(text).map_err(|e| error(StatusCode::BAD_REQUEST, e))
}

fn caps_of(state: &AppState) -> Caps {
    match &state.harness {
        Some(h) => h.caps(),
        None => Caps::from_config(&state.config.harness),
    }
}

/// `GET /_rustybin/harness`.
pub async fn node_status(State(state): State<AppState>) -> Response {
    match &state.harness {
        Some(h) => Json(h.status()).into_response(),
        None => Json(json!({
            "enabled": false,
            "harness_api": super::HARNESS_API,
            "hint": "RUSTYBIN_HARNESS=on turns this instance into a harness node (docs/plans/harness.md)",
            "caps": Caps::from_config(&state.config.harness),
        }))
        .into_response(),
    }
}

/// `GET /_rustybin/harness/schema`.
pub async fn schema(State(state): State<AppState>) -> Response {
    let mut resp = Json(super::scenario::schema(&caps_of(&state))).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/schema+json"),
    );
    resp
}

fn scenario_listing(name: &str, text: &str) -> Value {
    match Scenario::parse(text) {
        Ok(mut s) => {
            s.normalise();
            json!({
                "name": name,
                "description": s.description,
                "builtin": true,
                "summary": s.summary(),
                "links": { "self": format!("{PREFIX}/scenarios/{name}") },
            })
        }
        Err(e) => json!({ "name": name, "builtin": true, "error": e }),
    }
}

/// `GET /_rustybin/harness/scenarios`.
pub async fn list_scenarios() -> Response {
    Json(json!({
        "scenarios": builtin_scenarios()
            .iter()
            .map(|(name, text)| scenario_listing(name, text))
            .collect::<Vec<_>>(),
        "saved": [],
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct FormatQuery {
    #[serde(default)]
    pub format: Option<String>,
}

/// `GET /_rustybin/harness/scenarios/{name}`.
pub async fn get_scenario(Path(name): Path<String>, Query(q): Query<FormatQuery>) -> Response {
    let Some((_, text)) = builtin_scenarios().iter().find(|(n, _)| *n == name) else {
        return error(
            StatusCode::NOT_FOUND,
            format!("no built-in scenario {name:?}"),
        );
    };
    match q.format.as_deref() {
        Some("yaml") | Some("yml") => (
            [(header::CONTENT_TYPE, "application/yaml; charset=utf-8")],
            text.to_string(),
        )
            .into_response(),
        _ => match builtin_scenario(&name) {
            Some(s) => Json(serde_json::to_value(s).unwrap_or(Value::Null)).into_response(),
            None => error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "built-in scenario does not parse",
            ),
        },
    }
}

/// `POST /_rustybin/harness/validate`.
pub async fn validate(State(state): State<AppState>, body: Bytes) -> Response {
    let mut scenario = match parse_body(&body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    scenario.normalise();
    let caps = caps_of(&state);
    let problems = scenario.validate(&caps);
    Json(json!({
        "valid": problems.is_empty(),
        "problems": problems,
        "scenario": scenario,
        "hash": scenario.hash(),
        "summary": scenario.summary(),
        "caps": caps,
    }))
    .into_response()
}

/// `GET /_rustybin/harness/runs`.
pub async fn list_runs(State(state): State<AppState>) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let runs: Vec<Value> = h.runs().iter().rev().map(|r| r.summary()).collect();
    Json(json!({ "runs": runs, "active": h.active_run().map(|r| r.id.clone()) })).into_response()
}

/// `POST /_rustybin/harness/runs`.
pub async fn create_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    if let Err(resp) = guard_mutation(&state, &headers) {
        return resp;
    }
    let scenario = match parse_body(&body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    match h.create_run(scenario).await {
        Ok(run) => {
            let mut resp = (
                StatusCode::ACCEPTED,
                Json(super::engine::with_links(run.snapshot(), PREFIX, &run.id)),
            )
                .into_response();
            if let Ok(v) = HeaderValue::from_str(&format!("{PREFIX}/runs/{}", run.id)) {
                resp.headers_mut().insert(header::LOCATION, v);
            }
            resp
        }
        Err(e) => run_error(e),
    }
}

/// `GET /_rustybin/harness/runs/{run}`.
pub async fn get_run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    match h.run(&id) {
        Some(run) => {
            Json(super::engine::with_links(run.snapshot(), PREFIX, &run.id)).into_response()
        }
        None => error(StatusCode::NOT_FOUND, format!("no run {id:?} on this node")),
    }
}

/// `POST /_rustybin/harness/runs/{run}/stop`.
pub async fn stop_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    if let Err(resp) = guard_mutation(&state, &headers) {
        return resp;
    }
    let Some(run) = h.run(&id) else {
        return error(StatusCode::NOT_FOUND, format!("no run {id:?} on this node"));
    };
    if !run.is_active() {
        return Json(json!({ "run_id": id, "state": "finished", "stopped": false }))
            .into_response();
    }
    run.stop("user");
    tracing::info!(target: "rustybin::harness", run = %id, "harness run stop requested");
    (
        StatusCode::ACCEPTED,
        Json(json!({ "run_id": id, "state": run.state().as_str(), "stopped": true })),
    )
        .into_response()
}

/// `GET /_rustybin/harness/runs/{run}/report`.
pub async fn run_report(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<FormatQuery>,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let Some(run) = h.run(&id) else {
        return error(StatusCode::NOT_FOUND, format!("no run {id:?} on this node"));
    };
    let Some(report) = run.report() else {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "the run has not finished",
                "state": run.state().as_str(),
                "hint": format!("poll GET {PREFIX}/runs/{id} until report_ready is true"),
            })),
        )
            .into_response();
    };
    match q.format.as_deref() {
        Some("text") => (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            report.to_text(),
        )
            .into_response(),
        Some("junit") | Some("xml") => (
            [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
            report.to_junit(),
        )
            .into_response(),
        _ => Json(serde_json::to_value(report).unwrap_or(Value::Null)).into_response(),
    }
}

/// `POST /_rustybin/harness/stop`: the kill switch.
pub async fn stop_all(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    if let Err(resp) = guard_mutation(&state, &headers) {
        return resp;
    }
    let stopped = h.stop_all();
    Json(json!({ "stopped": stopped, "kill_switch": true })).into_response()
}

/// `GET /.well-known/rustybin-harness`: the verification token.
pub async fn well_known(State(state): State<AppState>) -> Response {
    match &state.harness {
        Some(h) => (
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            h.verification_token.clone(),
        )
            .into_response(),
        None => error(
            StatusCode::NOT_FOUND,
            "harness mode is off on this instance",
        ),
    }
}

/// `RUSTYBIN_HARNESS_DATA_PLANE=off` on a client-only node: only `/`, the
/// control plane, the console and the verification token are served.
pub async fn data_plane_off(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if path == "/" || crate::control::is_control_path(path) || path == super::WELL_KNOWN_PATH {
        return next.run(req).await;
    }
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": "not found",
            "hint": "this is a harness client node with RUSTYBIN_HARNESS_DATA_PLANE=off: only /, /_rustybin/* and /ui are served",
        })),
    )
        .into_response()
}
