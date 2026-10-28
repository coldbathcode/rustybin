//! The harness control API under `/_rustybin/harness*` (plan section 9):
//! single-node runs, the clusters this node leads, the node routes members
//! call (join and heartbeat) and the verification token path.
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

use super::cluster::{run_links, Cluster, CreateCluster, NodeError};
use super::correlate::now_ns;
use super::member::{self, JoinTarget};
use super::scenario::{Caps, Scenario};
use super::wire::{ElectedRequest, HeartbeatRequest, JoinRequest, VoteRequest};
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

/// `GET /_rustybin/harness/scenarios`: the built-in scenarios and the ones
/// saved on this node.
pub async fn list_scenarios(State(state): State<AppState>) -> Response {
    let saved: Vec<Value> = state
        .harness
        .as_ref()
        .map(|h| {
            h.saved
                .list()
                .into_iter()
                .map(|s| {
                    let mut sc = s.scenario.clone();
                    sc.normalise();
                    json!({
                        "name": s.name,
                        "description": sc.description,
                        "builtin": false,
                        "saved_at": s.saved_at,
                        "summary": sc.summary(),
                        "links": { "self": format!("{PREFIX}/scenarios/{}", s.name) },
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Json(json!({
        "scenarios": builtin_scenarios()
            .iter()
            .map(|(name, text)| scenario_listing(name, text))
            .collect::<Vec<_>>(),
        "saved": saved,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct FormatQuery {
    #[serde(default)]
    pub format: Option<String>,
}

/// `GET /_rustybin/harness/scenarios/{name}`: a built-in or saved scenario.
pub async fn get_scenario(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<FormatQuery>,
) -> Response {
    let Some((_, text)) = builtin_scenarios().iter().find(|(n, _)| *n == name) else {
        let saved = state.harness.as_ref().and_then(|h| h.saved.get(&name));
        return match saved {
            Some(s) => match q.format.as_deref() {
                Some("yaml") | Some("yml") => (
                    [(header::CONTENT_TYPE, "application/yaml; charset=utf-8")],
                    serde_yaml::to_string(&s.scenario).unwrap_or_default(),
                )
                    .into_response(),
                _ => Json(serde_json::to_value(&s.scenario).unwrap_or(Value::Null)).into_response(),
            },
            None => error(
                StatusCode::NOT_FOUND,
                format!("no built-in or saved scenario {name:?}"),
            ),
        };
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
    let mut problems = scenario.validate(&caps);
    if problems.is_empty() {
        problems = super::check_sources(&scenario, &state.mocks);
    }
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
    report_response(report, q.format.as_deref())
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
            h.served_verification_token(),
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

// ── Clusters (plan section 9, phase 3) ─────────────────────────────

/// The bearer token of a node route (the join token or the node credential).
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            let (scheme, token) = v.trim().split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
        })
        .filter(|t| !t.is_empty())
}

fn node_error(e: NodeError) -> Response {
    let status = StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_REQUEST);
    let mut resp = (status, Json(json!({ "error": e.error }))).into_response();
    if status == StatusCode::UNAUTHORIZED {
        resp.headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    resp
}

/// The cluster `id` this node leads. A member of a cluster another node
/// leads answers `307` to the same path on the leader (with
/// `X-Rustybin-Harness-Leader`), so a controller can ask any node.
#[allow(clippy::result_large_err)]
fn led_cluster(h: &Harness, id: &str, path: &str) -> Result<Arc<Cluster>, Response> {
    if let Some(c) = h.coordinator.get(id) {
        return Ok(c);
    }
    if let Some(m) = h.membership() {
        if m.cluster_id == id {
            if let Some(url) = m.url() {
                let rest = path
                    .strip_prefix(&format!("{PREFIX}/clusters/{id}"))
                    .unwrap_or("");
                let leader = m.leader();
                let location = format!("{url}{rest}");
                let mut resp = (
                    StatusCode::TEMPORARY_REDIRECT,
                    Json(json!({
                        "error": "this node is a member, not the leader, of the cluster",
                        "leader": leader,
                        "location": location,
                    })),
                )
                    .into_response();
                if let Ok(v) = HeaderValue::from_str(&location) {
                    resp.headers_mut().insert(header::LOCATION, v);
                }
                let tag = format!("{}; term={}", leader.node_id, leader.term);
                if let Ok(v) = HeaderValue::from_str(&tag) {
                    resp.headers_mut().insert(super::LEADER_HEADER, v);
                }
                return Err(resp);
            }
        }
    }
    Err(error(
        StatusCode::NOT_FOUND,
        format!("no cluster {id:?} on this node"),
    ))
}

/// `GET /_rustybin/harness/clusters`.
pub async fn list_clusters(State(state): State<AppState>) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let clusters: Vec<Value> = h.coordinator.list().iter().map(|c| c.summary()).collect();
    Json(json!({
        "clusters": clusters,
        "member_of": h.membership().map(|m| m.cluster_id.clone()),
    }))
    .into_response()
}

/// `POST /_rustybin/harness/clusters`: create a cluster led by this node.
/// The join token is in the answer once. A node that also has the client
/// or server role joins its own cluster.
pub async fn create_cluster(
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
    let req: CreateCluster = if body.iter().all(u8::is_ascii_whitespace) {
        CreateCluster::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid body: {e}")),
        }
    };
    match super::create_cluster(&h, req).await {
        Ok((cluster, token)) => {
            let mut v = cluster.describe();
            v["join_token"] = json!(token);
            let mut resp = (StatusCode::CREATED, Json(v)).into_response();
            if let Ok(loc) = HeaderValue::from_str(&format!("{PREFIX}/clusters/{}", cluster.id)) {
                resp.headers_mut().insert(header::LOCATION, loc);
            }
            resp
        }
        Err(e) => run_error(e),
    }
}

/// `GET /_rustybin/harness/clusters/{id}`.
pub async fn get_cluster(
    State(state): State<AppState>,
    Path(id): Path<String>,
    req_uri: axum::http::Uri,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    match led_cluster(&h, &id, req_uri.path()) {
        Ok(c) => Json(c.describe()).into_response(),
        Err(resp) => resp,
    }
}

/// `DELETE /_rustybin/harness/clusters/{id}` (no active run).
pub async fn delete_cluster(
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
    if let Err(e) = h.coordinator.remove(&id) {
        return run_error(e);
    }
    // The leader's own membership of the cluster ends with it.
    if h.membership()
        .is_some_and(|m| m.cluster_id == id && m.url().is_none())
    {
        h.leave().await;
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `GET /_rustybin/harness/clusters/{id}/upstreams`: the server nodes'
/// advertised URLs, for the gateway's upstream pool.
pub async fn upstreams(
    State(state): State<AppState>,
    Path(id): Path<String>,
    req_uri: axum::http::Uri,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    match led_cluster(&h, &id, req_uri.path()) {
        Ok(c) => Json(json!(c.upstreams())).into_response(),
        Err(resp) => resp,
    }
}

/// `POST /_rustybin/harness/clusters/{id}/nodes`: a node joins with the
/// join token (bearer). Not behind the control-plane auth: the token is
/// the credential.
pub async fn node_join(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let t2 = now_ns();
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let Some(token) = bearer(&headers) else {
        return node_error(NodeError {
            status: 401,
            error: "the join token is required (Authorization: Bearer)".into(),
        });
    };
    let Some(cluster) = h.coordinator.get(&id) else {
        // Not the leader: an eligible member names it (409), so a node can
        // join through any eligible member.
        if let Some(m) = h.membership().filter(|m| m.cluster_id == id) {
            return match m.join_redirect(token) {
                Ok(answer) => (StatusCode::CONFLICT, Json(answer)).into_response(),
                Err(e) => node_error(e),
            };
        }
        return error(
            StatusCode::NOT_FOUND,
            format!("no cluster {id:?} on this node"),
        );
    };
    let req: JoinRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!("invalid join request: {e}"),
            )
        }
    };
    match cluster.join(token, req, t2) {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => node_error(e),
    }
}

/// `POST /_rustybin/harness/clusters/{id}/nodes/{node}/heartbeat`: a long
/// poll with the node credential (bearer).
pub async fn node_heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, node)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let t2 = now_ns();
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let Some(credential) = bearer(&headers).map(str::to_string) else {
        return node_error(NodeError {
            status: 401,
            error: "the node credential is required (Authorization: Bearer)".into(),
        });
    };
    let Some(cluster) = h.coordinator.get(&id) else {
        // Not the leader: a member names the leader it follows (409), so a
        // node that lost its leader finds the new one.
        if let Some(m) = h.membership().filter(|m| m.cluster_id == id) {
            return match m.not_leader(&node, &credential) {
                Ok(answer) => (StatusCode::CONFLICT, Json(answer)).into_response(),
                Err(e) => node_error(e),
            };
        }
        return error(
            StatusCode::NOT_FOUND,
            format!("no cluster {id:?} on this node"),
        );
    };
    let mut req: HeartbeatRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid heartbeat: {e}")),
    };
    // The long poll ends well inside the time-to-headers timeout.
    if state.config.request_timeout_secs > 0 {
        let room = state.config.request_timeout_secs.saturating_sub(1) * 1000;
        req.wait_ms = req.wait_ms.min(room);
    }
    match cluster.heartbeat(&node, &credential, req, t2).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => node_error(e),
    }
}

/// `GET /_rustybin/harness/clusters/{id}/runs`.
pub async fn list_cluster_runs(
    State(state): State<AppState>,
    Path(id): Path<String>,
    req_uri: axum::http::Uri,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let c = match led_cluster(&h, &id, req_uri.path()) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let runs: Vec<Value> = c
        .runs()
        .iter()
        .rev()
        .map(|r| {
            let mut v = r.summary();
            v["links"] = run_links(&c.id, &r.id);
            v
        })
        .collect();
    Json(json!({ "runs": runs, "active": c.active_run().map(|r| r.id.clone()) })).into_response()
}

/// `POST /_rustybin/harness/clusters/{id}/runs`: submit a scenario
/// (validated against the cluster, not started).
pub async fn create_cluster_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    req_uri: axum::http::Uri,
    body: Bytes,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    if let Err(resp) = guard_mutation(&state, &headers) {
        return resp;
    }
    let c = match led_cluster(&h, &id, req_uri.path()) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let scenario = match parse_body(&body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    match c.create_run(scenario, &h.caps()) {
        Ok(run) => {
            let mut v = c.run_snapshot(&run);
            v["quorum"] = json!(c.quorum(&run.scenario));
            let mut resp = (StatusCode::CREATED, Json(v)).into_response();
            if let Ok(loc) =
                HeaderValue::from_str(&format!("{PREFIX}/clusters/{}/runs/{}", c.id, run.id))
            {
                resp.headers_mut().insert(header::LOCATION, loc);
            }
            resp
        }
        Err(e) => run_error(e),
    }
}

/// `GET /_rustybin/harness/clusters/{id}/runs/{run}`: live merged stats.
pub async fn get_cluster_run(
    State(state): State<AppState>,
    Path((id, run_id)): Path<(String, String)>,
    req_uri: axum::http::Uri,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let c = match led_cluster(&h, &id, req_uri.path()) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match c.run(&run_id) {
        Some(run) => {
            let mut v = c.run_snapshot(&run);
            v["quorum"] = json!(c.quorum(&run.scenario));
            Json(v).into_response()
        }
        None => error(
            StatusCode::NOT_FOUND,
            format!("no run {run_id:?} in cluster {id}"),
        ),
    }
}

/// `POST /_rustybin/harness/clusters/{id}/runs/{run}/start`: the shares
/// and the synchronised start, once the quorum is there (409 before).
pub async fn start_cluster_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, run_id)): Path<(String, String)>,
    req_uri: axum::http::Uri,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    if let Err(resp) = guard_mutation(&state, &headers) {
        return resp;
    }
    let c = match led_cluster(&h, &id, req_uri.path()) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match c.start_run(&run_id) {
        Ok(run) => (StatusCode::ACCEPTED, Json(c.run_snapshot(&run))).into_response(),
        Err(e) => run_error(e),
    }
}

/// `POST /_rustybin/harness/clusters/{id}/runs/{run}/stop`.
pub async fn stop_cluster_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, run_id)): Path<(String, String)>,
    req_uri: axum::http::Uri,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    if let Err(resp) = guard_mutation(&state, &headers) {
        return resp;
    }
    let c = match led_cluster(&h, &id, req_uri.path()) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match c.stop_run(&run_id, "user") {
        Ok((run, changed)) => (
            if changed {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            },
            Json(json!({ "run_id": run.id, "state": run.state().as_str(), "stopped": changed })),
        )
            .into_response(),
        Err(e) => run_error(e),
    }
}

/// `GET /_rustybin/harness/clusters/{id}/runs/{run}/report`: the merged report.
pub async fn cluster_run_report(
    State(state): State<AppState>,
    Path((id, run_id)): Path<(String, String)>,
    Query(q): Query<FormatQuery>,
    req_uri: axum::http::Uri,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let c = match led_cluster(&h, &id, req_uri.path()) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let Some(run) = c.run(&run_id) else {
        return error(
            StatusCode::NOT_FOUND,
            format!("no run {run_id:?} in cluster {id}"),
        );
    };
    c.tick();
    let Some(report) = run.report() else {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "the run has not finished",
                "state": run.state().as_str(),
                "hint": format!("poll GET {PREFIX}/clusters/{id}/runs/{run_id} until the state is finished"),
            })),
        )
            .into_response();
    };
    report_response(report, q.format.as_deref())
}

fn report_response(report: &super::report::Report, format: Option<&str>) -> Response {
    match format {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinBody {
    /// The cluster URL (`<leader>/_rustybin/harness/clusters/<id>`).
    pub url: String,
    pub join_token: String,
    /// A server node's data-plane URL as the gateway reaches it.
    #[serde(default)]
    pub advertise: Option<String>,
}

/// `POST /_rustybin/harness/join`: a controller tells this node to join a
/// cluster (orchestrated). The URL is checked like every outbound address:
/// never link-local, metadata or other forbidden addresses.
pub async fn join(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    if let Err(resp) = guard_mutation(&state, &headers) {
        return resp;
    }
    let req: JoinBody = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid body: {e}")),
    };
    if let Some(a) = &req.advertise {
        if !(a.starts_with("http://") || a.starts_with("https://")) || a.len() > 2048 {
            return error(StatusCode::BAD_REQUEST, "advertise: an http or https URL");
        }
    }
    let (url, _) = match member::cluster_url(&req.url, None) {
        Ok(u) => u,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    if let Err(e) = super::safety::check_join_url(&url).await {
        return error(StatusCode::FORBIDDEN, e);
    }
    let advertise = req.advertise.or_else(|| h.config.advertise.clone());
    match member::join(&h, JoinTarget::Url(url), req.join_token, advertise).await {
        Ok(m) => Json(json!({ "joined": true, "cluster": m.describe() })).into_response(),
        Err(e) => error(StatusCode::CONFLICT, e),
    }
}

/// `POST /_rustybin/harness/leave`: leave the cluster (its runs stop on
/// this node).
pub async fn leave(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    if let Err(resp) = guard_mutation(&state, &headers) {
        return resp;
    }
    match h.leave().await {
        Some(id) => Json(json!({ "left": id })).into_response(),
        None => error(
            StatusCode::NOT_FOUND,
            "this node is not a member of a cluster",
        ),
    }
}

/// `GET /_rustybin/harness/clusters/{id}/leader`: the leader, the term, the
/// succession with each node's work score and the history of leader
/// changes. Answered by every member (never redirected), so a controller
/// can always find the leader.
pub async fn get_leader(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    if let Some(c) = h.coordinator.get(&id) {
        c.tick();
        return Json(c.leader_view()).into_response();
    }
    match h.membership().filter(|m| m.cluster_id == id) {
        Some(m) => Json(m.leader_view()).into_response(),
        None => error(
            StatusCode::NOT_FOUND,
            format!("this node is not a member of cluster {id:?}"),
        ),
    }
}

/// `POST /_rustybin/harness/clusters/{id}/election/vote`: a candidate asks
/// for this node's vote (its node credential as bearer).
pub async fn node_vote(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let Some(credential) = bearer(&headers) else {
        return node_error(NodeError::new(
            401,
            "the node credential is required (Authorization: Bearer)",
        ));
    };
    let req: VoteRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!("invalid vote request: {e}"),
            )
        }
    };
    let answer = if let Some(c) = h.coordinator.get(&id) {
        c.answer_vote(credential, &req)
    } else if let Some(m) = h.membership().filter(|m| m.cluster_id == id) {
        m.vote(credential, &req)
    } else {
        Err(NodeError::new(
            404,
            format!("no cluster {id:?} on this node"),
        ))
    };
    match answer {
        Ok(v) => {
            tracing::info!(
                target: "rustybin::harness",
                cluster = %id,
                candidate = %req.candidate,
                term = req.term,
                granted = v.granted,
                reason = %v.reason,
                "harness: vote requested"
            );
            Json(v).into_response()
        }
        Err(e) => node_error(e),
    }
}

/// `POST /_rustybin/harness/clusters/{id}/election/elected`: a new leader
/// announces its term (its node credential as bearer). A node that led an
/// older term steps down; a member follows.
pub async fn node_elected(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let Some(credential) = bearer(&headers) else {
        return node_error(NodeError::new(
            401,
            "the node credential is required (Authorization: Bearer)",
        ));
    };
    let req: ElectedRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!("invalid announcement: {e}"),
            )
        }
    };
    if let Some(c) = h.coordinator.get(&id) {
        if let Err(e) = c.authenticate(&req.leader.node_id, credential) {
            return node_error(e);
        }
        if req.leader.term > c.term {
            super::election::step_down(&h, &c, Some(req.leader.clone()));
            return Json(json!({ "accepted": true, "stepped_down": true })).into_response();
        }
        return Json(json!({ "accepted": false, "term": c.term })).into_response();
    }
    match h.membership().filter(|m| m.cluster_id == id) {
        Some(m) => match m.elected(credential, &req) {
            Ok(followed) => Json(json!({ "accepted": followed })).into_response(),
            Err(e) => node_error(e),
        },
        None => error(
            StatusCode::NOT_FOUND,
            format!("no cluster {id:?} on this node"),
        ),
    }
}

// ── Phase 4: live changes, preview, saved scenarios, the run feed ──

/// A live change body: JSON (or YAML).
#[allow(clippy::result_large_err)]
fn parse_change(body: &Bytes) -> Result<super::live::Change, Response> {
    let text = std::str::from_utf8(body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "the change must be UTF-8 text"))?;
    serde_json::from_str(text)
        .or_else(|_| serde_yaml::from_str(text))
        .map_err(|e| error(StatusCode::BAD_REQUEST, format!("invalid change: {e}")))
}

fn problems(status: StatusCode, error: &str, problems: Vec<String>) -> Response {
    (
        status,
        Json(json!({ "error": error, "problems": problems })),
    )
        .into_response()
}

/// `PATCH /_rustybin/harness/runs/{run}`: a live change of a single-node run.
pub async fn patch_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
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
    if run.cluster.is_some() {
        return error(
            StatusCode::CONFLICT,
            "this run is part of a cluster run: change it on the leader (PATCH .../clusters/{id}/runs/{run})",
        );
    }
    let change = match parse_change(&body) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match run.apply_change(&change) {
        Ok(controls) => Json(json!({
            "run_id": id,
            "applied": change.describe(),
            "controls": controls,
        }))
        .into_response(),
        Err(p) if !run.is_active() => problems(StatusCode::CONFLICT, "the run has finished", p),
        Err(p) => problems(StatusCode::BAD_REQUEST, "the change was refused", p),
    }
}

/// `PATCH /_rustybin/harness/clusters/{id}/runs/{run}`: a live change of a
/// cluster run; every client node follows it on its next heartbeat.
pub async fn patch_cluster_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, run_id)): Path<(String, String)>,
    req_uri: axum::http::Uri,
    body: Bytes,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    if let Err(resp) = guard_mutation(&state, &headers) {
        return resp;
    }
    let c = match led_cluster(&h, &id, req_uri.path()) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let change = match parse_change(&body) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match c.change_run(&run_id, &change) {
        Ok((run, controls)) => Json(json!({
            "run_id": run.id,
            "cluster_id": c.id,
            "applied": change.describe(),
            "controls": controls,
        }))
        .into_response(),
        Err(e) => run_error(e),
    }
}

/// `POST /_rustybin/harness/preview`: the expected rate per second per layer
/// and in total, without sending anything (works with harness mode off).
pub async fn preview(State(state): State<AppState>, body: Bytes) -> Response {
    let mut scenario = match parse_body(&body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    scenario.normalise();
    let caps = caps_of(&state);
    let problems = scenario.validate(&caps);
    let mut v = scenario.preview();
    v["valid"] = json!(problems.is_empty());
    v["problems"] = json!(problems);
    v["caps"] = json!(caps);
    Json(v).into_response()
}

/// `PUT /_rustybin/harness/scenarios/{name}`: save a scenario (validated
/// against this node's caps) under a name.
pub async fn put_scenario(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    body: Bytes,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    if let Err(resp) = guard_mutation(&state, &headers) {
        return resp;
    }
    if builtin_scenarios().iter().any(|(n, _)| *n == name) {
        return error(
            StatusCode::CONFLICT,
            format!("{name:?} is a built-in scenario: save the copy under another name"),
        );
    }
    let scenario = match parse_body(&body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let mut check = scenario.clone();
    check.normalise();
    let p = check.validate(&h.caps());
    if !p.is_empty() {
        return problems(StatusCode::BAD_REQUEST, "the scenario has problems", p);
    }
    match h.saved.put(&name, scenario) {
        Ok(created) => (
            if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            },
            Json(json!({
                "name": name,
                "saved": true,
                "summary": check.summary(),
                "links": { "self": format!("{PREFIX}/scenarios/{name}") },
            })),
        )
            .into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

/// `DELETE /_rustybin/harness/scenarios/{name}`.
pub async fn delete_scenario(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    if let Err(resp) = guard_mutation(&state, &headers) {
        return resp;
    }
    if builtin_scenarios().iter().any(|(n, _)| *n == name) {
        return error(StatusCode::CONFLICT, "built-in scenarios cannot be deleted");
    }
    if h.saved.delete(&name) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        error(StatusCode::NOT_FOUND, format!("no saved scenario {name:?}"))
    }
}

/// Events for a run feed: the new log entries and findings since `seen`.
fn control_events(
    controls: &super::live::Controls,
    seen_log: &mut usize,
    seen_findings: &mut usize,
) -> Vec<axum::response::sse::Event> {
    use axum::response::sse::Event;
    let mut out = Vec::new();
    // The log is bounded (oldest dropped): look for entries by version.
    let new: Vec<&super::live::LogEntry> = controls
        .log
        .iter()
        .filter(|e| e.version as usize > *seen_log)
        .collect();
    for e in new {
        *seen_log = e.version as usize;
        if let Ok(ev) = Event::default().event("change").json_data(e) {
            out.push(ev);
        }
    }
    for f in controls.findings.iter().skip(*seen_findings) {
        if let Ok(ev) = Event::default().event("finding").json_data(f) {
            out.push(ev);
        }
    }
    *seen_findings = controls.findings.len();
    out
}

/// `GET /_rustybin/harness/runs/{run}/events`: the run's feed: a `snapshot`
/// every second, `change` and `finding` events, and `finished` with the
/// report's summary, after which the stream ends. The console reads it with
/// `send({stream: true})` (control-plane feeds need their credential).
pub async fn run_events(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let Some(run) = h.run(&id) else {
        return error(StatusCode::NOT_FOUND, format!("no run {id:?} on this node"));
    };
    let until = std::time::Instant::now()
        + run.scenario.duration()
        + h.caps().max_duration()
        + std::time::Duration::from_secs(60);
    let stream = async_stream::stream! {
        let mut seen_log = 0usize;
        let mut seen_findings = 0usize;
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tick.tick().await;
            for ev in control_events(&run.controls(), &mut seen_log, &mut seen_findings) {
                yield Ok::<Event, std::convert::Infallible>(ev);
            }
            if let Ok(ev) = Event::default()
                .event("snapshot")
                .json_data(super::engine::with_links(run.snapshot(), PREFIX, &run.id))
            {
                yield Ok(ev);
            }
            if let Some(report) = run.report() {
                if let Ok(ev) = Event::default().event("finished").json_data(json!({
                    "run_id": run.id,
                    "state": report.state,
                    "passed": report.passed,
                    "exit_code": report.exit_code,
                    "report": format!("{PREFIX}/runs/{}/report", run.id),
                })) {
                    yield Ok(ev);
                }
                break;
            }
            if std::time::Instant::now() > until {
                break;
            }
        }
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// `GET /_rustybin/harness/clusters/{id}/runs/{run}/events`: the cluster
/// run's feed, like a single node's, plus `leader_changed` (this node no
/// longer leads: the event names the new leader, then the stream ends; a
/// controller reconnects there) and `scale_request`.
pub async fn cluster_run_events(
    State(state): State<AppState>,
    Path((id, run_id)): Path<(String, String)>,
    req_uri: axum::http::Uri,
) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    let h = match harness(&state) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let c = match led_cluster(&h, &id, req_uri.path()) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let Some(run) = c.run(&run_id) else {
        return error(
            StatusCode::NOT_FOUND,
            format!("no run {run_id:?} in cluster {id}"),
        );
    };
    let until = std::time::Instant::now()
        + run.scenario.duration()
        + h.caps().max_duration()
        + std::time::Duration::from_secs(60);
    let h2 = h.clone();
    let stream = async_stream::stream! {
        let mut seen_log = 0usize;
        let mut seen_findings = 0usize;
        let mut seen_scale = 0usize;
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tick.tick().await;
            if c.is_stale() || h2.coordinator.get(&c.id).is_none_or(|led| !Arc::ptr_eq(&led, &c)) {
                let leader = h2.membership().map(|m| m.leader());
                let control_url = leader
                    .as_ref()
                    .and_then(|l| l.control_url.clone())
                    .map(|u| format!("{}{PREFIX}/clusters/{}/runs/{}/events", u.trim_end_matches('/'), c.id, run.id));
                if let Ok(ev) = Event::default().event("leader_changed").json_data(json!({
                    "leader": leader,
                    "events": control_url,
                })) {
                    yield Ok::<Event, std::convert::Infallible>(ev);
                }
                break;
            }
            for ev in control_events(&c.run_controls(&run), &mut seen_log, &mut seen_findings) {
                yield Ok(ev);
            }
            for req in c.scale_requests(&run).into_iter().skip(seen_scale) {
                seen_scale += 1;
                if let Ok(ev) = Event::default().event("scale_request").json_data(req) {
                    yield Ok(ev);
                }
            }
            if let Ok(ev) = Event::default().event("snapshot").json_data(c.run_snapshot(&run)) {
                yield Ok(ev);
            }
            if let Some(report) = run.report() {
                if let Ok(ev) = Event::default().event("finished").json_data(json!({
                    "run_id": run.id,
                    "state": report.state,
                    "passed": report.passed,
                    "exit_code": report.exit_code,
                    "report": format!("{PREFIX}/clusters/{}/runs/{}/report", c.id, run.id),
                })) {
                    yield Ok(ev);
                }
                break;
            }
            if std::time::Instant::now() > until {
                break;
            }
        }
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
