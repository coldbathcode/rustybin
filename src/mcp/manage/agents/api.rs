//! The operator's routes over the agents registry (admin):
//!
//! - `GET /_rustybin/agents`: every agent, the policy (default ceiling and
//!   the rules of `rustybin.yaml`).
//! - `GET /_rustybin/agents/events`: the live feed of calls (SSE).
//! - `GET /_rustybin/agents/{id}`: one agent with its tools, last errors and
//!   sessions.
//! - `PATCH /_rustybin/agents/{id}`: `{rights: {ceiling, allow, deny} | null,
//!   note}`.
//! - `POST /_rustybin/agents/{id}/revoke`, `/restore`.
//! - `DELETE /_rustybin/agents/{id}/sessions`: end its sessions.

use std::convert::Infallible;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use super::{AgentError, Policy, Rights};
use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

pub const AGENTS_PATH: &str = "/_rustybin/agents";
pub const EVENTS_PATH: &str = "/_rustybin/agents/events";

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

fn refused(e: AgentError) -> Response {
    match e {
        AgentError::NotFound => error(StatusCode::NOT_FOUND, "no such agent"),
        AgentError::Invalid(m) => error(StatusCode::BAD_REQUEST, m),
        AgentError::Io(m) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the agents state could not be saved: {m}"),
        ),
    }
}

/// Who acts, for the revocation record and the audit line (never a secret).
fn operator(headers: &HeaderMap) -> String {
    let Some(token) = crate::control_auth::presented_token(headers) else {
        return "operator".into();
    };
    if crate::login::is_console_token(token) {
        if let Some(user) = crate::control_auth::unverified_claims(token)
            .and_then(|c| c.get("sub").and_then(Value::as_str).map(str::to_string))
        {
            return format!("console:{}", user.strip_prefix("user:").unwrap_or(&user));
        }
    }
    "admin".into()
}

async fn list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let policy = Policy::of(&state);
    let agents = state.agents.list(&policy);
    Json(json!({
        "agents": agents,
        "policy": policy.view(),
        "capacity": super::MAX_AGENTS,
        "persisted": state.agents.persisted(),
        "control_auth": state.config.control_auth.as_str(),
        "ceilings": super::CEILINGS,
    }))
    .into_response()
}

async fn get_one(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    match state.agents.get(&id, &Policy::of(&state)) {
        Some(v) => Json(v).into_response(),
        None => refused(AgentError::NotFound),
    }
}

/// `PATCH` body: a field left out is unchanged, `null` clears it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    #[serde(default, deserialize_with = "present")]
    rights: Option<Option<Rights>>,
    #[serde(default, deserialize_with = "present")]
    note: Option<Option<String>>,
}

/// Tell a field set to `null` (`Some(None)`) from one left out (`None`).
fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

async fn patch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let p: Patch = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!("expected {{rights: {{ceiling, allow, deny}}, note}}: {e}"),
            )
        }
    };
    if let Err(e) = state.agents.set_rights(&id, p.rights, p.note) {
        return refused(e);
    }
    tracing::info!(target: "rustybin::agents", agent = %id, by = %operator(&headers), "rights saved");
    match state.agents.get(&id, &Policy::of(&state)) {
        Some(v) => Json(v).into_response(),
        None => refused(AgentError::NotFound),
    }
}

async fn revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    match state
        .agents
        .revoke(&id, &operator(&headers), &state.config.console_tokens)
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => refused(e),
    }
}

async fn restore(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    match state.agents.restore(&id, &state.config.console_tokens) {
        Ok(v) => Json(v).into_response(),
        Err(e) => refused(e),
    }
}

async fn end_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    match state.agents.end_sessions(&id) {
        Ok(n) => Json(json!({ "id": id, "sessions_ended": n })).into_response(),
        Err(e) => refused(e),
    }
}

async fn events(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let Some((rx, guard)) = state.agents.subscribe() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            format!("at most {} live agent feeds at once", super::MAX_LISTENERS),
        );
    };
    let stream = BroadcastStream::new(rx).filter_map(move |item| {
        let _hold = &guard;
        let v = item.ok()?;
        let kind = if v.get("event").is_some() {
            "agent"
        } else {
            "call"
        };
        Some(Ok::<Event, Infallible>(
            Event::default().event(kind).data(v.to_string()),
        ))
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(AGENTS_PATH, get(list))
        .route(EVENTS_PATH, get(events))
        .route("/_rustybin/agents/{id}", get(get_one).patch(patch))
        .route("/_rustybin/agents/{id}/revoke", post(revoke))
        .route("/_rustybin/agents/{id}/restore", post(restore))
        .route("/_rustybin/agents/{id}/sessions", delete(end_sessions))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            AGENTS_PATH,
            &["GET"],
            category::CONTROL,
            "AI agents on the management MCP server (admin)",
        )
        .description(
            "Every client that called /_rustybin/mcp: its name and version (clientInfo), the \
             credential it presents, first and last seen, calls today and in total, open \
             sessions, status (active, idle, revoked) and the rights that apply to it (ceiling, \
             tool allow and deny lists, from its own override, a rule of rustybin.yaml or the \
             default). Plus the policy: the default ceiling and the rules.",
        )
        .example(Example::get("List the agents", AGENTS_PATH)),
        Endpoint::new(
            EVENTS_PATH,
            &["GET"],
            category::CONTROL,
            "Live feed of agent calls (admin)",
        )
        .description(
            "Server-Sent Events: a `call` event per management tool call (agent, tool, outcome), \
             an `agent` event when one is revoked or restored. At most 16 listeners.",
        )
        .sse()
        .example(
            Example::get("Agent call feed", EVENTS_PATH).skip_check("never-ending SSE stream"),
        ),
        Endpoint::new(
            "/_rustybin/agents/{id}",
            &["GET", "PATCH"],
            category::CONTROL,
            "One AI agent: its calls per tool, last errors, sessions and rights (admin)",
        )
        .description(
            "PATCH {\"rights\": {\"ceiling\": \"read-only\" | \"traffic\" | \"manage\" | \"full\", \
             \"allow\": [tools], \"deny\": [tools]}, \"note\": \"...\"}: the agent's own rights, \
             field by field over the matching rule; \"rights\": null goes back to the rules.",
        )
        .example(Example::get("An unknown agent", "/_rustybin/agents/0000000000000000").expect_status(404))
        .example(
            Example::new("Limit an unknown agent", "PATCH", "/_rustybin/agents/0000000000000000")
                .json(r#"{"rights":{"ceiling":"read-only"}}"#)
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/agents/{id}/revoke",
            &["POST"],
            category::CONTROL,
            "Revoke an AI agent: refuse its calls, end its sessions, revoke its tokens (admin)",
        )
        .example(Example::post("Revoke an unknown agent", "/_rustybin/agents/0000000000000000/revoke").expect_status(404)),
        Endpoint::new(
            "/_rustybin/agents/{id}/restore",
            &["POST"],
            category::CONTROL,
            "Restore a revoked AI agent (admin)",
        )
        .example(Example::post("Restore an unknown agent", "/_rustybin/agents/0000000000000000/restore").expect_status(404)),
        Endpoint::new(
            "/_rustybin/agents/{id}/sessions",
            &["DELETE"],
            category::CONTROL,
            "End an AI agent's MCP sessions (admin)",
        )
        .example(Example::delete("End the sessions of an unknown agent", "/_rustybin/agents/0000000000000000/sessions").expect_status(404)),
    ]
}

pub fn openapi_paths() -> Value {
    let tags = json!(["Control Plane"]);
    let id =
        json!([{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }]);
    let not_found = json!({ "description": "No such agent" });
    let tool_list = json!({ "type": "array", "items": { "type": "string" }, "description": "Management tool names" });
    json!({
        AGENTS_PATH: { "get": {
            "tags": tags, "summary": "AI agents on the management MCP server", "operationId": "listAgents",
            "responses": { "200": { "description": "{agents, policy, capacity, persisted}" }, "401": { "description": "Admin rights required" } }
        } },
        EVENTS_PATH: { "get": {
            "tags": tags, "summary": "Live feed of agent calls (SSE)", "operationId": "streamAgentEvents",
            "responses": { "200": { "description": "text/event-stream: call and agent events" }, "429": { "description": "Too many listeners" } }
        } },
        "/_rustybin/agents/{id}": {
            "get": { "tags": tags, "summary": "One AI agent", "operationId": "getAgent", "parameters": id,
                "responses": { "200": { "description": "The agent with tools, last_errors, sessions, override and available_tools" }, "404": not_found } },
            "patch": { "tags": tags, "summary": "Set an AI agent's rights and note", "operationId": "setAgentRights", "parameters": id,
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "additionalProperties": false, "properties": {
                    "rights": { "type": ["object", "null"], "additionalProperties": false, "properties": {
                        "ceiling": { "type": "string", "enum": super::CEILINGS },
                        "allow": tool_list, "deny": tool_list } },
                    "note": { "type": ["string", "null"], "maxLength": super::MAX_NOTE } } } } } },
                "responses": { "200": { "description": "The agent" }, "400": { "description": "Invalid rights" }, "404": not_found } }
        },
        "/_rustybin/agents/{id}/revoke": { "post": { "tags": tags, "summary": "Revoke an AI agent", "operationId": "revokeAgent", "parameters": id,
            "responses": { "200": { "description": "{revoked, sessions_ended, tokens_revoked, note}" }, "404": not_found } } },
        "/_rustybin/agents/{id}/restore": { "post": { "tags": tags, "summary": "Restore a revoked AI agent", "operationId": "restoreAgent", "parameters": id,
            "responses": { "200": { "description": "{restored}" }, "404": not_found } } },
        "/_rustybin/agents/{id}/sessions": { "delete": { "tags": tags, "summary": "End an AI agent's MCP sessions", "operationId": "endAgentSessions", "parameters": id,
            "responses": { "200": { "description": "{sessions_ended}" }, "404": not_found } } },
    })
}
