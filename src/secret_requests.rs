//! A secret set at an agent's request, confirmed out of band.
//!
//! A secret is never a tool input. An agent on the management MCP server
//! asks for a secret setting to be set (`request_secret`: the setting's key
//! and a reason), a person who may set that secret types the value in the
//! console or gives it to the CLI on stdin, and the agent reads the
//! request's state (`get_secret_request`). The value passes through the
//! person's own call only, into the settings store, as a `PATCH` of
//! `/_rustybin/settings` would put it there.
//!
//! - `GET /_rustybin/secret-requests[?state=pending]`: every request
//!   (`secrets.write`).
//! - `GET /_rustybin/secret-requests/{id}`: one.
//! - `POST /_rustybin/secret-requests/{id}/fulfil`: `{"value": "..."}`; what
//!   setting that secret needs (`settings.write`, `secrets.write`, the
//!   group's permission), the settings lock and the spec's checks included.
//! - `POST /_rustybin/secret-requests/{id}/decline`.
//!
//! Requests live in memory, bounded ([`MAX_PENDING`], [`MAX_PENDING_PER_AGENT`],
//! [`MAX_PER_AGENT_PER_HOUR`], [`MAX_KEPT`]) and expire ([`TTL_SECS`]); a
//! resolved one stays readable for [`KEEP_RESOLVED_SECS`]. Off until the
//! `agents.secret_requests` setting is on. The reason is an agent's claim:
//! cleaned, capped, answered as `untrusted_reason`, never in the audit log.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{Extensions, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::limits::Clock;
use crate::mcp::manage::agents::{Credential, Policy};
use crate::rbac::Permission;
use crate::settings::{Settings, Spec};
use crate::state::AppState;

/// The setting that turns the feature on.
pub const SWITCH_KEY: &str = "agents.secret_requests";
/// Pending requests in all.
pub const MAX_PENDING: usize = 32;
/// Pending requests of one agent.
pub const MAX_PENDING_PER_AGENT: usize = 3;
/// Requests one agent may make in an hour.
pub const MAX_PER_AGENT_PER_HOUR: usize = 10;
/// Requests kept in all (pending and resolved).
pub const MAX_KEPT: usize = 128;
/// How long a request waits for a person.
pub const TTL_SECS: i64 = 15 * 60;
/// How long a resolved request stays readable.
pub const KEEP_RESOLVED_SECS: i64 = 60 * 60;
/// Longest reason, in characters.
pub const MAX_REASON: usize = 300;

pub const LIST_PATH: &str = "/_rustybin/secret-requests";
pub const ONE_PATH: &str = "/_rustybin/secret-requests/{id}";
pub const FULFIL_PATH: &str = "/_rustybin/secret-requests/{id}/fulfil";
pub const DECLINE_PATH: &str = "/_rustybin/secret-requests/{id}/decline";

/// The settings this feature registers.
pub fn specs() -> Vec<Spec> {
    vec![Spec::new(
        SWITCH_KEY,
        "agents",
        "Secret requests from AI agents",
        "Lets an agent on the management MCP server ask for a secret setting to be set (request_secret: the setting's key and a reason). A person with the permission to set that secret types the value in the console (Management) or gives it to `rustybin settings fulfil` on stdin, or declines; the value never passes through the agent. Requests expire after 15 minutes. Off: an agent is told to ask the operator.",
        crate::settings::Kind::Bool,
        json!(false),
    )]
}

/// Is the feature on?
pub fn enabled(settings: &Settings) -> bool {
    settings.bool(SWITCH_KEY)
}

/// Where a request stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Fulfilled,
    Declined,
    Expired,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Fulfilled => "fulfilled",
            Self::Declined => "declined",
            Self::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Pending,
            Self::Fulfilled,
            Self::Declined,
            Self::Expired,
        ]
        .into_iter()
        .find(|x| x.as_str() == s)
    }
}

/// Who asks: the agent as the registry knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asker {
    /// The agent's id (`agents::agent_id`).
    pub agent: String,
    /// The client's name (`clientInfo`): a claim of the client.
    pub name: String,
    /// What the agent authenticates with (never the secret itself).
    pub credential: Credential,
}

/// One request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: String,
    /// The secret setting's key.
    pub key: String,
    pub asker: Asker,
    /// The agent's reason: cleaned and capped, third-party text.
    pub reason: String,
    pub created: DateTime<Utc>,
    pub expires: DateTime<Utc>,
    pub status: Status,
    pub resolved_at: Option<DateTime<Utc>>,
    /// Who fulfilled or declined it (a name, never a credential).
    pub resolved_by: Option<String>,
}

/// Why an ask was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AskError {
    /// The agent has [`MAX_PENDING_PER_AGENT`] waiting.
    AgentPending,
    /// The agent asked [`MAX_PER_AGENT_PER_HOUR`] times this hour.
    AgentHour,
    /// [`MAX_PENDING`] are waiting, or [`MAX_KEPT`] are kept.
    Full,
}

impl AskError {
    pub fn message(&self) -> String {
        match self {
            Self::AgentPending => format!(
                "this agent already has {MAX_PENDING_PER_AGENT} secret requests waiting for a person: wait for one to be set, declined or expired (get_secret_request lists them)"
            ),
            Self::AgentHour => format!(
                "this agent asked for secrets {MAX_PER_AGENT_PER_HOUR} times in the last hour: try again later"
            ),
            Self::Full => format!(
                "too many secret requests are waiting on this instance (at most {MAX_PENDING}): try again later"
            ),
        }
    }
}

/// Why a request could not be fulfilled or declined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveError {
    NotFound,
    /// It is no longer pending.
    Gone(Status),
}

/// Free text from an agent: control characters dropped, whitespace runs
/// folded, trimmed, capped.
pub fn clean_reason(raw: &str) -> String {
    let mut out = String::new();
    let mut len = 0;
    let mut space = false;
    for c in raw.chars() {
        if c.is_control() || c.is_whitespace() {
            space = len > 0;
            continue;
        }
        let needed = if space { 2 } else { 1 };
        if len + needed > MAX_REASON {
            break;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
        len += needed;
    }
    out
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn new_id() -> String {
    let mut b = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut b);
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("sr_{hex}")
}

/// The requests (`AppState.secret_requests`).
pub struct SecretRequests {
    clock: Arc<dyn Clock>,
    inner: Mutex<Vec<Request>>,
}

impl SecretRequests {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            inner: Mutex::new(Vec::new()),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.wall()
    }

    /// Expire what waited too long, drop what was resolved long ago.
    fn sweep(list: &mut Vec<Request>, now: DateTime<Utc>) {
        for r in list.iter_mut() {
            if r.status == Status::Pending && now >= r.expires {
                r.status = Status::Expired;
                r.resolved_at = Some(r.expires);
            }
        }
        let keep = Duration::seconds(KEEP_RESOLVED_SECS);
        list.retain(|r| r.resolved_at.is_none_or(|at| now - at < keep));
    }

    /// Ask for `key` to be set. The same agent asking for the same key
    /// while a request waits gets that request (`false`: nothing new).
    pub fn ask(&self, asker: Asker, key: &str, reason: &str) -> Result<(Request, bool), AskError> {
        let now = self.now();
        let mut list = lock(&self.inner);
        Self::sweep(&mut list, now);
        if let Some(r) = list
            .iter()
            .find(|r| r.status == Status::Pending && r.asker.agent == asker.agent && r.key == key)
        {
            return Ok((r.clone(), false));
        }
        let mine = |r: &&Request| r.asker.agent == asker.agent;
        if list
            .iter()
            .filter(mine)
            .filter(|r| r.status == Status::Pending)
            .count()
            >= MAX_PENDING_PER_AGENT
        {
            return Err(AskError::AgentPending);
        }
        let hour = Duration::seconds(3600);
        if list
            .iter()
            .filter(mine)
            .filter(|r| now - r.created < hour)
            .count()
            >= MAX_PER_AGENT_PER_HOUR
        {
            return Err(AskError::AgentHour);
        }
        if list.iter().filter(|r| r.status == Status::Pending).count() >= MAX_PENDING
            || list.len() >= MAX_KEPT
        {
            return Err(AskError::Full);
        }
        let r = Request {
            id: new_id(),
            key: key.to_string(),
            asker,
            reason: clean_reason(reason),
            created: now,
            expires: now + Duration::seconds(TTL_SECS),
            status: Status::Pending,
            resolved_at: None,
            resolved_by: None,
        };
        list.push(r.clone());
        Ok((r, true))
    }

    /// One request, as it stands now.
    pub fn get(&self, id: &str) -> Option<Request> {
        let now = self.now();
        let mut list = lock(&self.inner);
        Self::sweep(&mut list, now);
        list.iter().find(|r| r.id == id).cloned()
    }

    /// Every request, newest first.
    pub fn list(&self) -> Vec<Request> {
        let now = self.now();
        let mut list = lock(&self.inner);
        Self::sweep(&mut list, now);
        let mut out = list.clone();
        out.sort_by_key(|r| std::cmp::Reverse(r.created));
        out
    }

    /// The requests of one agent, newest first.
    pub fn of_agent(&self, agent: &str) -> Vec<Request> {
        self.list()
            .into_iter()
            .filter(|r| r.asker.agent == agent)
            .collect()
    }

    /// A request that still waits.
    pub fn pending(&self, id: &str) -> Result<Request, ResolveError> {
        match self.get(id) {
            None => Err(ResolveError::NotFound),
            Some(r) if r.status == Status::Pending => Ok(r),
            Some(r) => Err(ResolveError::Gone(r.status)),
        }
    }

    /// Fulfil or decline a request that waits.
    pub fn resolve(&self, id: &str, status: Status, by: &str) -> Result<Request, ResolveError> {
        let now = self.now();
        let mut list = lock(&self.inner);
        Self::sweep(&mut list, now);
        let Some(r) = list.iter_mut().find(|r| r.id == id) else {
            return Err(ResolveError::NotFound);
        };
        if r.status != Status::Pending {
            return Err(ResolveError::Gone(r.status));
        }
        r.status = status;
        r.resolved_at = Some(now);
        r.resolved_by = Some(crate::logging::printable(by, 128));
        Ok(r.clone())
    }

    /// How many wait.
    pub fn pending_count(&self) -> usize {
        self.list()
            .iter()
            .filter(|r| r.status == Status::Pending)
            .count()
    }

    fn times(&self, r: &Request) -> Value {
        let left = (r.expires - self.now()).num_seconds().max(0);
        json!({
            "created_at": r.created.to_rfc3339(),
            "expires_at": r.expires.to_rfc3339(),
            "expires_in": if r.status == Status::Pending { left } else { 0 },
            "resolved_at": r.resolved_at.map(|t| t.to_rfc3339()),
        })
    }

    /// A request as its agent reads it: the state, never the value, a hint
    /// or who resolved it.
    pub fn agent_view(&self, r: &Request, settings: &Settings) -> Value {
        let mut v = json!({
            "id": r.id,
            "state": r.status.as_str(),
            "setting": r.key,
            "set": settings.secret_state(&r.key)["set"],
        });
        merge(&mut v, self.times(r));
        v
    }

    /// A request as a person who may fulfil it reads it: what the instance
    /// knows (the setting, the agent, its owner) apart from what the agent
    /// claims (`untrusted_reason`, `untrusted_agent_name`).
    pub fn view(&self, r: &Request, state: &AppState) -> Value {
        let policy = Policy::of(state);
        let spec = state.settings.spec(&r.key);
        let known = state.agents.get(&r.asker.agent, &policy);
        let mut v = json!({
            "id": r.id,
            "state": r.status.as_str(),
            "setting": {
                "key": r.key,
                "title": spec.map(|s| s.title),
                "group": spec.map(|s| s.group),
                "set": state.settings.secret_state(&r.key)["set"],
                "locked": !state.settings.writable(&r.key),
                "needs": needs(&r.key),
            },
            "agent": {
                "id": r.asker.agent,
                "untrusted_name": r.asker.name,
                "credential": policy.credential_label(&r.asker.credential),
                "owner": policy.owner_view(&r.asker.credential),
                "owner_label": policy.owner_label(&r.asker.credential),
                "status": known.as_ref().map(|a| a["status"].clone()),
            },
            "untrusted_reason": r.reason,
            "resolved_by": r.resolved_by,
        });
        merge(&mut v, self.times(r));
        v
    }
}

fn merge(into: &mut Value, from: Value) {
    if let (Value::Object(a), Value::Object(b)) = (into, from) {
        a.extend(b);
    }
}

/// What fulfilling a request for `key` needs: what setting that secret
/// needs.
pub fn needs(key: &str) -> Vec<&'static str> {
    let mut out = vec![Permission::SettingsWrite.name()];
    out.extend(
        crate::settings::extra_permissions(key, true)
            .iter()
            .map(|p| p.name()),
    );
    out
}

/// Why a secret may not be asked for at all (`Ok`: its spec).
pub fn askable<'a>(settings: &'a Settings, key: &str) -> Result<&'a Spec, String> {
    let Some(spec) = settings.spec(key) else {
        return Err(format!(
            "no setting named {key:?} (get_settings lists them)"
        ));
    };
    if !spec.secret {
        return Err(format!("{key} is not a secret: update_settings changes it"));
    }
    if key == crate::login::HASH_KEY {
        return Err(format!(
            "{key} is a password: it passes through nobody but its owner (the console's Sign-in tab, `rustybin passwd`, or the environment)"
        ));
    }
    if !settings.writable(key) {
        return Err(format!(
            "{key} is locked by RUSTYBIN_SETTINGS_LOCK (the environment is authoritative): the operator sets {}",
            spec.env.unwrap_or("its environment variable")
        ));
    }
    Ok(spec)
}

// ── Routes ──────────────────────────────────────────────────────────

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

fn gone(e: ResolveError) -> Response {
    match e {
        ResolveError::NotFound => error(
            StatusCode::NOT_FOUND,
            "no such secret request (it may have expired more than an hour ago)",
        ),
        ResolveError::Gone(status) => (
            StatusCode::GONE,
            Json(json!({
                "error": format!("this secret request is {}: the agent asks again if it still needs the secret", status.as_str()),
                "state": status.as_str(),
            })),
        )
            .into_response(),
    }
}

#[derive(Deserialize, Default)]
struct ListQuery {
    #[serde(default)]
    state: Option<String>,
}

async fn list(State(state): State<AppState>, Query(q): Query<ListQuery>) -> Response {
    let wanted = match q.state.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => match Status::parse(s) {
            Some(s) => Some(s),
            None => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "state is pending, fulfilled, declined or expired",
                )
            }
        },
    };
    let store = &state.secret_requests;
    let all = store.list();
    let pending = all.iter().filter(|r| r.status == Status::Pending).count();
    let requests: Vec<Value> = all
        .iter()
        .filter(|r| wanted.is_none_or(|w| r.status == w))
        .map(|r| store.view(r, &state))
        .collect();
    Json(json!({
        "enabled": enabled(&state.settings),
        "requests": requests,
        "pending": pending,
        "limits": {
            "pending": MAX_PENDING,
            "pending_per_agent": MAX_PENDING_PER_AGENT,
            "per_agent_per_hour": MAX_PER_AGENT_PER_HOUR,
            "expires_after_secs": TTL_SECS,
            "kept_secs": KEEP_RESOLVED_SECS,
        },
    }))
    .into_response()
}

async fn get_one(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.secret_requests.get(&id) {
        Some(r) => Json(state.secret_requests.view(&r, &state)).into_response(),
        None => gone(ResolveError::NotFound),
    }
}

/// What only the handler knows, for the audit entry (the route layer
/// names the request, `secret-request:<id>`, from the path).
fn note(r: &Request, event: &str) {
    crate::audit::event(event);
    crate::audit::target("setting", &r.key);
    crate::audit::target("agent", &r.asker.agent);
}

#[derive(Deserialize)]
struct Fulfil {
    /// The secret. Never logged, never answered.
    value: String,
}

/// Is the agent that asked revoked now?
fn asker_revoked(state: &AppState, r: &Request) -> bool {
    state
        .agents
        .get(&r.asker.agent, &Policy::of(state))
        .is_some_and(|a| a["revoked"].is_object())
}

async fn fulfil(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    Path(id): Path<String>,
    body: Result<Json<Fulfil>, JsonRejection>,
) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    let store = &state.secret_requests;
    let r = match store.pending(&id) {
        Ok(r) => r,
        Err(e) => return gone(e),
    };
    note(&r, "secret_fulfilled");
    // What a person setting this secret needs, checked as the settings
    // route checks it: nobody fulfils more than they could set themselves.
    if let Err(resp) = crate::rbac::require(&caller, Permission::SettingsWrite) {
        return resp;
    }
    if let Err(resp) =
        crate::settings::check_keys(&caller, &state.settings, [(r.key.as_str(), true)])
    {
        return resp;
    }
    let who = crate::rbac::service::principal_name(&caller);
    if asker_revoked(&state, &r) {
        let _ = store.resolve(&id, Status::Declined, "the agent was revoked");
        return (
            StatusCode::GONE,
            Json(json!({
                "error": "the agent that asked was revoked since: the request is declined and nothing was set",
                "state": Status::Declined.as_str(),
                "reason": "agent_revoked",
            })),
        )
            .into_response();
    }
    let value = match body {
        Ok(Json(b)) => zeroize::Zeroizing::new(b.value),
        Err(_) => {
            // Never the rejection's text: it may quote the body.
            return error(
                StatusCode::BAD_REQUEST,
                "expected { \"value\": \"<the secret>\" }",
            );
        }
    };
    if value.trim().is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "a value is required: decline the request instead of setting nothing",
        );
    }
    let secrets = BTreeMap::from([(r.key.clone(), Some(value.to_string()))]);
    let principal = crate::settings::principal(&headers, &state.config);
    if let Err(e) = state.settings.update(BTreeMap::new(), secrets, &principal) {
        return crate::settings::refusal(e);
    }
    // Fulfilled even when someone else resolved it meanwhile: the secret
    // is set either way, and the answer says how the request stands.
    let r = store
        .resolve(&id, Status::Fulfilled, &who)
        .ok()
        .or_else(|| store.get(&id))
        .unwrap_or(r);
    tracing::info!(target: "rustybin::settings", request = %r.id, key = %r.key, agent = %r.asker.agent, by = %principal, "secret request fulfilled");
    Json(store.view(&r, &state)).into_response()
}

async fn decline(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    Path(id): Path<String>,
) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    let store = &state.secret_requests;
    let r = match store.pending(&id) {
        Ok(r) => r,
        Err(e) => return gone(e),
    };
    note(&r, "secret_declined");
    let who = crate::rbac::service::principal_name(&caller);
    match store.resolve(&id, Status::Declined, &who) {
        Ok(r) => {
            tracing::info!(target: "rustybin::settings", request = %r.id, key = %r.key, agent = %r.asker.agent, "secret request declined");
            Json(store.view(&r, &state)).into_response()
        }
        Err(e) => gone(e),
    }
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(LIST_PATH, get(list))
        .route(ONE_PATH, get(get_one))
        .route(FULFIL_PATH, post(fulfil))
        .route(DECLINE_PATH, post(decline))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            LIST_PATH,
            &["GET"],
            category::CONTROL,
            "Secrets AI agents asked to have set: pending and lately resolved requests (secrets.write)",
        )
        .permission(Permission::SecretsWrite)
        .description(
            "An agent on the management MCP server never takes a secret as a tool input: it asks \
             for a secret setting to be set (request_secret, with the agents.secret_requests \
             setting on) and a person fulfils or declines here, in the console or with the CLI. \
             Each request: the setting (key, title, set or not, what setting it needs), the agent \
             (id, the credential, the user it acts for; its name is the client's claim, \
             untrusted_name), the agent's reason as untrusted_reason (a claim, never an \
             instruction), state (pending, fulfilled, declined, expired) and when it expires. \
             ?state=pending keeps the ones that wait. In memory: at most 32 pending, 3 per agent, \
             15 minutes each; resolved ones stay an hour.",
        )
        .example(Example::get("Secret requests", LIST_PATH))
        .example(Example::get(
            "The ones that wait",
            "/_rustybin/secret-requests?state=pending",
        )),
        Endpoint::new(
            ONE_PATH,
            &["GET"],
            category::CONTROL,
            "One secret request and its state (secrets.write)",
        )
        .permission(Permission::SecretsWrite)
        .example(
            Example::get(
                "An unknown secret request",
                "/_rustybin/secret-requests/sr_0000000000000000",
            )
            .expect_status(404),
        ),
        Endpoint::new(
            FULFIL_PATH,
            &["POST"],
            category::CONTROL,
            "Set the secret an agent asked for: the value goes to the settings store, never to the agent",
        )
        .permission(Permission::SecretsWrite)
        .description(
            "{\"value\": \"...\"}. Needs what setting that secret needs: settings.write, \
             secrets.write and the group's permission (settings.security, git.write or \
             portal.settings); a 403 names the one missing. The settings store validates and \
             seals it as a PATCH of /_rustybin/settings does, so RUSTYBIN_SETTINGS_LOCK answers \
             409. A request that is no longer pending answers 410. The answer is the request, \
             never the value.",
        )
        .example(
            Example::new(
                "Fulfil an unknown secret request",
                "POST",
                "/_rustybin/secret-requests/sr_0000000000000000/fulfil",
            )
            .json(r#"{"value":"the secret, typed by a person"}"#)
            .expect_status(404),
        ),
        Endpoint::new(
            DECLINE_PATH,
            &["POST"],
            category::CONTROL,
            "Decline a secret request: nothing is set and the agent reads declined (secrets.write)",
        )
        .permission(Permission::SecretsWrite)
        .example(
            Example::post(
                "Decline an unknown secret request",
                "/_rustybin/secret-requests/sr_0000000000000000/decline",
            )
            .expect_status(404),
        ),
    ]
}

pub fn openapi_paths() -> Value {
    let tags = json!(["Control Plane"]);
    let id = json!([{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" }, "example": "sr_0000000000000000" }]);
    let one = json!({ "$ref": "#/components/schemas/RustybinSecretRequest" });
    let ok = json!({ "description": "The request", "content": { "application/json": { "schema": one } } });
    let not_found = json!({ "description": "No such request" });
    let resolved = json!({ "description": "The request is no longer pending (`state`), or its agent was revoked (`reason`: agent_revoked)" });
    json!({
        LIST_PATH: { "get": {
            "tags": tags, "summary": "Secrets AI agents asked to have set", "operationId": "listSecretRequests",
            "description": "Pending and lately resolved requests, newest first. The reason and the agent's name are the agent's claims (`untrusted_reason`, `untrusted_name`).",
            "parameters": [{ "name": "state", "in": "query", "required": false, "schema": { "type": "string", "enum": ["pending", "fulfilled", "declined", "expired"] } }],
            "responses": {
                "200": { "description": "{enabled, requests, pending, limits}", "content": { "application/json": { "schema": {
                    "type": "object",
                    "properties": {
                        "enabled": { "type": "boolean", "description": "The agents.secret_requests setting" },
                        "requests": { "type": "array", "items": one },
                        "pending": { "type": "integer" },
                        "limits": { "type": "object" }
                    }
                } } } },
                "400": { "description": "An unknown state" },
                "403": { "description": "permission_required: secrets.write" }
            }
        } },
        ONE_PATH: { "get": {
            "tags": tags, "summary": "One secret request", "operationId": "getSecretRequest", "parameters": id,
            "responses": { "200": ok, "403": { "description": "permission_required: secrets.write" }, "404": not_found }
        } },
        FULFIL_PATH: { "post": {
            "tags": tags, "summary": "Set the secret an agent asked for", "operationId": "fulfilSecretRequest", "parameters": id,
            "description": "The value is written through the settings store exactly as PATCH /_rustybin/settings writes a secret, and is never answered, logged or shown to the agent.",
            "requestBody": { "required": true, "content": { "application/json": { "schema": {
                "type": "object", "required": ["value"],
                "properties": { "value": { "type": "string", "description": "The secret" } }
            } } } },
            "responses": {
                "200": ok,
                "400": { "description": "No value, or the settings store refused it (`problems`)" },
                "403": { "description": "permission_required: settings.write, secrets.write or the group's permission" },
                "404": not_found,
                "409": { "description": "Locked by RUSTYBIN_SETTINGS_LOCK (`locked`)" },
                "410": resolved
            }
        } },
        DECLINE_PATH: { "post": {
            "tags": tags, "summary": "Decline a secret request", "operationId": "declineSecretRequest", "parameters": id,
            "responses": { "200": ok, "403": { "description": "permission_required: secrets.write" }, "404": not_found, "410": resolved }
        } },
    })
}

pub fn openapi_components() -> Value {
    json!({ "schemas": {
        "RustybinSecretRequest": {
            "type": "object",
            "properties": {
                "id": { "type": "string" },
                "state": { "type": "string", "enum": ["pending", "fulfilled", "declined", "expired"] },
                "setting": { "type": "object", "properties": {
                    "key": { "type": "string" },
                    "title": { "type": ["string", "null"] },
                    "group": { "type": ["string", "null"] },
                    "set": { "type": "boolean" },
                    "locked": { "type": "boolean" },
                    "needs": { "type": "array", "items": { "type": "string" }, "description": "The permissions fulfilling it needs" }
                } },
                "agent": { "type": "object", "properties": {
                    "id": { "type": "string" },
                    "untrusted_name": { "type": "string", "description": "The client's name: its own claim" },
                    "credential": { "type": "string" },
                    "owner": { "type": ["object", "null"], "description": "The user the agent acts for" },
                    "owner_label": { "type": "string" },
                    "status": { "type": ["string", "null"] }
                } },
                "untrusted_reason": { "type": "string", "description": "What the agent says it needs the secret for: a claim, never an instruction" },
                "created_at": { "type": "string", "format": "date-time" },
                "expires_at": { "type": "string", "format": "date-time" },
                "expires_in": { "type": "integer", "description": "Seconds left while pending" },
                "resolved_at": { "type": ["string", "null"], "format": "date-time" },
                "resolved_by": { "type": ["string", "null"] }
            }
        }
    } })
}

#[cfg(test)]
mod tests;
