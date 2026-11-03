//! The routes over the agents registry. Everyone's agents need
//! `agents.read` to see and `agents.manage` to change; a user without them
//! sees, limits, revokes and ends the sessions of their own agents (and
//! restores one they revoked themselves): `super::visible_to`,
//! `super::may_manage`. Any signed-in caller reaches the routes; the
//! handlers check the rest.
//!
//! - `GET /_rustybin/agents[?owner=<user>]`: the agents (each with the
//!   user it acts for), the policy (default ceiling, the defaults per role,
//!   the rules of `rustybin.yaml`).
//! - `GET /_rustybin/agents/events`: the live feed of calls (SSE).
//! - `GET /_rustybin/agents/{id}`: one agent with its tools, last errors and
//!   sessions.
//! - `PATCH /_rustybin/agents/{id}`: `{rights: {ceiling, allow, deny} | null,
//!   note}`; a ceiling is a 0.3 ceiling or a role name.
//! - `POST /_rustybin/agents/{id}/revoke`, `/restore`.
//! - `DELETE /_rustybin/agents/{id}/sessions`: end its sessions.

use std::convert::Infallible;

use axum::extract::{Path, Query, State};
use axum::http::{Extensions, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use super::{Action, AgentError, Policy, Rights};
use crate::catalog::{category, Endpoint, Example};
use crate::rbac::Resolved;
use crate::state::AppState;

pub const AGENTS_PATH: &str = "/_rustybin/agents";
pub const EVENTS_PATH: &str = "/_rustybin/agents/events";

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

fn refused(caller: &Resolved, e: AgentError) -> Response {
    match e {
        AgentError::NotFound => error(StatusCode::NOT_FOUND, "no such agent"),
        AgentError::Invalid(m) => error(StatusCode::BAD_REQUEST, m),
        AgentError::Io(m) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the agents state could not be saved: {m}"),
        ),
        AgentError::Needs(p, m) => {
            let (status, _) = crate::rbac::need::refusal_of(caller, p);
            (status, Json(crate::rbac::need::refusal_body(p, Some(&m)))).into_response()
        }
    }
}

/// Who acts, for the revocation record and the audit line (never a secret).
fn operator(caller: &Resolved) -> String {
    match &caller.principal {
        crate::rbac::Principal::User { name, .. } => format!("console:{name}"),
        crate::rbac::Principal::AdminToken | crate::rbac::Principal::Open => "admin".into(),
        _ => crate::rbac::service::principal_name(caller),
    }
}

fn caller(
    state: &AppState,
    headers: &HeaderMap,
    extensions: &Extensions,
) -> std::sync::Arc<Resolved> {
    crate::control_auth::caller_of(state, headers, extensions)
}

#[derive(Deserialize, Default)]
struct ListQuery {
    #[serde(default)]
    owner: Option<String>,
}

/// The owner a list is for: the one asked (`agents.read` for anyone but
/// yourself), else the caller's own without `agents.read`.
fn list_owner(
    state: &AppState,
    caller: &Resolved,
    asked: Option<&str>,
) -> Result<Option<String>, AgentError> {
    let own = super::visible_to(caller)?;
    let asked = asked
        .map(str::trim)
        .filter(|o| !o.is_empty())
        .map(|o| super::owner_filter(&state.users, o));
    match (own, asked) {
        (Some(me), Some(o)) if o != me => Err(AgentError::Needs(
            crate::rbac::Permission::AgentsRead,
            "other people's agents need the agents.read permission".into(),
        )),
        (Some(me), _) => Ok(Some(me)),
        (None, asked) => Ok(asked),
    }
}

async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    Query(q): Query<ListQuery>,
) -> Response {
    let caller = caller(&state, &headers, &extensions);
    let owner = match list_owner(&state, &caller, q.owner.as_deref()) {
        Ok(o) => o,
        Err(e) => return refused(&caller, e),
    };
    let policy = Policy::of(&state);
    let agents = state.agents.list(&policy, owner.as_deref());
    Json(json!({
        "agents": agents,
        "owner": owner,
        "you": caller.user_id(),
        "policy": policy.view(),
        "capacity": super::MAX_AGENTS,
        "persisted": state.agents.persisted(),
        "control_auth": state.config.control_auth.as_str(),
        "ceilings": super::CEILINGS,
        "roles": super::ceiling_roles(&state, &caller),
    }))
    .into_response()
}

async fn get_one(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    Path(id): Path<String>,
) -> Response {
    let caller = caller(&state, &headers, &extensions);
    if let Err(e) = super::may_read(&state.agents, &caller, &id) {
        return refused(&caller, e);
    }
    match state.agents.get(&id, &Policy::of(&state)) {
        Some(v) => Json(v).into_response(),
        None => refused(&caller, AgentError::NotFound),
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
    extensions: Extensions,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let caller = caller(&state, &headers, &extensions);
    let p: Patch = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!("expected {{rights: {{ceiling, allow, deny}}, note}}: {e}"),
            )
        }
    };
    if let Err(e) = super::check_rights(&p.rights, &state.users) {
        return refused(&caller, e);
    }
    let policy = Policy::of(&state);
    if let Err(e) = super::may_manage(
        &state.agents,
        &policy,
        &caller,
        &id,
        &Action::Rights(p.rights.clone()),
    ) {
        return refused(&caller, e);
    }
    if let Err(e) = state.agents.set_rights(&id, p.rights, p.note) {
        return refused(&caller, e);
    }
    tracing::info!(target: "rustybin::agents", agent = %id, by = %operator(&caller), "rights saved");
    match state.agents.get(&id, &policy) {
        Some(v) => Json(v).into_response(),
        None => refused(&caller, AgentError::NotFound),
    }
}

async fn revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    Path(id): Path<String>,
) -> Response {
    let caller = caller(&state, &headers, &extensions);
    if let Err(e) = super::may_manage(
        &state.agents,
        &Policy::of(&state),
        &caller,
        &id,
        &Action::Revoke,
    ) {
        return refused(&caller, e);
    }
    match state.agents.revoke_as(
        &id,
        &operator(&caller),
        caller.user_id(),
        &state.config.console_tokens,
    ) {
        Ok(v) => Json(v).into_response(),
        Err(e) => refused(&caller, e),
    }
}

async fn restore(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    Path(id): Path<String>,
) -> Response {
    let caller = caller(&state, &headers, &extensions);
    if let Err(e) = super::may_manage(
        &state.agents,
        &Policy::of(&state),
        &caller,
        &id,
        &Action::Restore,
    ) {
        return refused(&caller, e);
    }
    match state.agents.restore(&id, &state.config.console_tokens) {
        Ok(v) => Json(v).into_response(),
        Err(e) => refused(&caller, e),
    }
}

async fn end_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    Path(id): Path<String>,
) -> Response {
    let caller = caller(&state, &headers, &extensions);
    if let Err(e) = super::may_manage(
        &state.agents,
        &Policy::of(&state),
        &caller,
        &id,
        &Action::EndSessions,
    ) {
        return refused(&caller, e);
    }
    match state.agents.end_sessions(&id) {
        Ok(n) => Json(json!({ "id": id, "sessions_ended": n })).into_response(),
        Err(e) => refused(&caller, e),
    }
}

async fn events(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let caller = caller(&state, &headers, &extensions);
    // Without agents.read, the events of the caller's own agents only.
    let own = match super::visible_to(&caller) {
        Ok(o) => o,
        Err(e) => return refused(&caller, e),
    };
    let Some((rx, guard)) = state.agents.subscribe() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            format!("at most {} live agent feeds at once", super::MAX_LISTENERS),
        );
    };
    let stream = BroadcastStream::new(rx).filter_map(move |item| {
        let _hold = &guard;
        let v = item.ok()?;
        if let Some(me) = &own {
            if v.get("owner").and_then(Value::as_str) != Some(me.as_str()) {
                return None;
            }
        }
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
            "AI agents on the management MCP server, each with the user it acts for (agents.read; a user's own without it)",
        )
        .authenticated(&[])
        .description(
            "Every client that called /_rustybin/mcp: its name and version (clientInfo), the \
             credential it presents and the user it acts for (owner; the admin token and \
             control JWTs act for nobody), first and last seen, calls today and in total, open \
             sessions, status (active, idle, revoked) and the rights that apply to it (ceiling, \
             tool allow and deny lists, from its own override, a rule of rustybin.yaml, the \
             default for a role of its user or the default). Plus the policy. ?owner=<user id \
             or sign-in name> keeps one user's agents. Everyone's need agents.read; a user \
             without it sees their own.",
        )
        .example(Example::get("List the agents", AGENTS_PATH)),
        Endpoint::new(
            EVENTS_PATH,
            &["GET"],
            category::CONTROL,
            "Live feed of agent calls (agents.read; a user's own without it)",
        )
        .authenticated(&[])
        .description(
            "Server-Sent Events: a `call` event per management tool call (agent, owner, tool, \
             outcome), an `agent` event when one is revoked or restored. At most 16 listeners.",
        )
        .sse()
        .example(
            Example::get("Agent call feed", EVENTS_PATH).skip_check("never-ending SSE stream"),
        ),
        Endpoint::new(
            "/_rustybin/agents/{id}",
            &["GET", "PATCH"],
            category::CONTROL,
            "One AI agent: its user, calls per tool, last errors, sessions and rights (agents.read / agents.manage; your own without them)",
        )
        .authenticated(&[])
        .description(
            "PATCH {\"rights\": {\"ceiling\": \"read-only\" | \"traffic\" | \"manage\" | \"full\" | \
             a role name, \"allow\": [tools], \"deny\": [tools]}, \"note\": \"...\"}: the agent's \
             own rights, field by field over the matching rule; \"rights\": null goes back to \
             the rules. Another user's agent needs agents.manage; your own you may limit \
             (never widen) without it.",
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
            "Revoke an AI agent: refuse its calls, end its sessions, revoke its tokens (agents.manage; your own without it)",
        )
        .authenticated(&[])
        .example(Example::post("Revoke an unknown agent", "/_rustybin/agents/0000000000000000/revoke").expect_status(404)),
        Endpoint::new(
            "/_rustybin/agents/{id}/restore",
            &["POST"],
            category::CONTROL,
            "Restore a revoked AI agent (agents.manage; one of yours you revoked without it)",
        )
        .authenticated(&[])
        .example(Example::post("Restore an unknown agent", "/_rustybin/agents/0000000000000000/restore").expect_status(404)),
        Endpoint::new(
            "/_rustybin/agents/{id}/sessions",
            &["DELETE"],
            category::CONTROL,
            "End an AI agent's MCP sessions (agents.manage; your own without it)",
        )
        .authenticated(&[])
        .example(Example::delete("End the sessions of an unknown agent", "/_rustybin/agents/0000000000000000/sessions").expect_status(404)),
    ]
}

pub fn openapi_paths() -> Value {
    let tags = json!(["Control Plane"]);
    let id =
        json!([{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }]);
    let not_found = json!({ "description": "No such agent" });
    let forbidden = json!({ "description": "permission_required: agents.read (another user's agent) or agents.manage (changing another user's agent, or widening your own)" });
    let tool_list = json!({ "type": "array", "items": { "type": "string" }, "description": "Management tool names" });
    json!({
        AGENTS_PATH: { "get": {
            "tags": tags, "summary": "AI agents on the management MCP server", "operationId": "listAgents",
            "parameters": [{ "name": "owner", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Only the agents acting for this user (an id or a sign-in name)" }],
            "responses": { "200": { "description": "{agents (each with owner and owner_label), owner, policy, capacity, persisted}" }, "403": forbidden }
        } },
        EVENTS_PATH: { "get": {
            "tags": tags, "summary": "Live feed of agent calls (SSE)", "operationId": "streamAgentEvents",
            "responses": { "200": { "description": "text/event-stream: call and agent events" }, "403": forbidden, "429": { "description": "Too many listeners" } }
        } },
        "/_rustybin/agents/{id}": {
            "get": { "tags": tags, "summary": "One AI agent", "operationId": "getAgent", "parameters": id,
                "responses": { "200": { "description": "The agent with owner, tools, last_errors, sessions, override and available_tools" }, "403": forbidden, "404": not_found } },
            "patch": { "tags": tags, "summary": "Set an AI agent's rights and note", "operationId": "setAgentRights", "parameters": id,
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "additionalProperties": false, "properties": {
                    "rights": { "type": ["object", "null"], "additionalProperties": false, "properties": {
                        "ceiling": { "type": "string", "description": "read-only, traffic, manage, full, or a role name" },
                        "allow": tool_list, "deny": tool_list } },
                    "note": { "type": ["string", "null"], "maxLength": super::MAX_NOTE } } } } } },
                "responses": { "200": { "description": "The agent" }, "400": { "description": "Invalid rights (an unknown tool or role)" }, "403": forbidden, "404": not_found } }
        },
        "/_rustybin/agents/{id}/revoke": { "post": { "tags": tags, "summary": "Revoke an AI agent", "operationId": "revokeAgent", "parameters": id,
            "responses": { "200": { "description": "{revoked, sessions_ended, tokens_revoked, note}" }, "403": forbidden, "404": not_found } } },
        "/_rustybin/agents/{id}/restore": { "post": { "tags": tags, "summary": "Restore a revoked AI agent", "operationId": "restoreAgent", "parameters": id,
            "responses": { "200": { "description": "{restored}" }, "403": forbidden, "404": not_found } } },
        "/_rustybin/agents/{id}/sessions": { "delete": { "tags": tags, "summary": "End an AI agent's MCP sessions", "operationId": "endAgentSessions", "parameters": id,
            "responses": { "200": { "description": "{sessions_ended}" }, "403": forbidden, "404": not_found } } },
    })
}
