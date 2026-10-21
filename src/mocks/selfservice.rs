//! Self-service access (`docs/plans/hosted-mocks.md` sections 3 and 4): who
//! may read and change which mock, and the visit routes
//! (`/_rustybin/mocks/visit*` for visitors, `/_rustybin/mocks/visits*` for
//! operators).
//!
//! Instance mocks (admin, file, sample) keep today's rules: everyone allowed
//! on the control plane reads them, admin rights change them. A visitor
//! mock (a visit or an API key) is read and changed by its owner and by
//! admin; to anyone else it does not exist (`404`).

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{Extensions, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::service::ServiceError;
use super::store::{Snapshot, StoreError};
use super::visits::{EmailInput, Owner, VisitError, Visits, SELF_HOST_URL, TOKEN_HEADER};
use crate::config::Config;
use crate::control_auth::ControlAuthorized;
use crate::keys::KeyContext;
use crate::state::AppState;

/// Who is calling.
#[derive(Clone)]
pub struct Caller {
    /// Admin rights (the admin guard passes and the control-plane grant, if
    /// any, has the admin scope).
    pub admin: bool,
    /// Owners whose mocks the caller holds: running visits of the presented
    /// manage tokens, and the API key in shared mode.
    pub owners: Vec<Owner>,
    /// The session key (`X-Rustybin-Session`, else the client IP).
    pub session: String,
    /// The admin guard's own answer for this request
    /// ([`crate::admin::admin_check`]): what a refused instance-global
    /// change says.
    pub admin_guard: Result<(), (StatusCode, &'static str)>,
}

impl Caller {
    pub fn owns(&self, owner: &Owner) -> bool {
        self.owners.contains(owner)
    }

    /// The caller's visit (the first running one presented).
    pub fn visit(&self) -> Option<&str> {
        self.owners.iter().find_map(|o| match o {
            Owner::Visit(h) => Some(h.as_str()),
            _ => None,
        })
    }

    pub fn key(&self) -> Option<&str> {
        self.owners.iter().find_map(|o| match o {
            Owner::Key(k) => Some(k.as_str()),
            _ => None,
        })
    }

    pub fn can_read(&self, s: &Snapshot) -> bool {
        !s.owner.is_visitor() || self.admin || self.owns(&s.owner)
    }

    /// May the caller see the mock's secrets? Admin, or its owner.
    pub fn reveals(&self, s: &Snapshot) -> bool {
        self.admin || (s.owner.is_visitor() && self.owns(&s.owner))
    }
}

pub fn caller(state: &AppState, headers: &HeaderMap, extensions: &Extensions) -> Caller {
    let config = &state.config;
    let grant = extensions.get::<ControlAuthorized>().map(|g| g.0);
    // A token from the control plane's OAuth server (accepted on the
    // management MCP server only) with the admin scope is admin there.
    let via_oauth = extensions.get::<crate::control_oauth::ViaOAuth>().is_some();
    let admin_guard = if via_oauth && grant.is_some_and(|g| g.admin) {
        Ok(())
    } else {
        crate::admin::admin_check(headers, config)
    };
    let admin = grant.is_none_or(|g| g.admin) && admin_guard.is_ok();
    let mut owners = Vec::new();
    if let Some(visits) = state.mocks.visits() {
        let tokens: Vec<&str> = headers
            .get_all(TOKEN_HEADER)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();
        owners.extend(visits.resolve(tokens).into_iter().map(Owner::Visit));
        if let Some(k) = extensions.get::<KeyContext>().filter(|k| k.shared) {
            owners.push(Owner::Key(k.key.id.to_string()));
        }
    }
    let ip = crate::session::client_ip(headers, extensions, config);
    Caller {
        admin,
        owners,
        session: crate::session::session_key(headers, ip),
        admin_guard,
    }
}

/// The JSON error of a visit decision.
pub fn visit_error(e: VisitError, config: &Config) -> ServiceError {
    let cfg = &config.self_service;
    let (status, code, message, extra, retry) = match e {
        VisitError::NotFound => (
            StatusCode::NOT_FOUND,
            "rustybin_visit_not_found",
            "this visit has ended or the manage token is unknown".to_string(),
            json!({}),
            None,
        ),
        VisitError::EmailRequired => (
            StatusCode::FORBIDDEN,
            "rustybin_email_required",
            format!(
                "the free {} hours are used: give an email address to keep going (up to {} \
                 hours a day)",
                cfg.anon_hours, cfg.daily_hours
            ),
            json!({
                "privacy_url": cfg.privacy_url,
                "consent": cfg.leads_consent.as_str(),
            }),
            None,
        ),
        VisitError::TimeUsed { resets_at } => (
            StatusCode::TOO_MANY_REQUESTS,
            "rustybin_visit_limit",
            "the free time of the demo is used up: come back tomorrow, or run your own Rustybin"
                .to_string(),
            json!({ "resets_at": resets_at, "self_host_url": SELF_HOST_URL }),
            Some((resets_at - chrono::Utc::now()).num_seconds().max(1) as u64),
        ),
        VisitError::DailyLimit { resets_at } => (
            StatusCode::TOO_MANY_REQUESTS,
            "rustybin_daily_limit",
            format!(
                "today's {} hours are used: come back after midnight UTC, or run your own \
                 Rustybin",
                cfg.daily_hours
            ),
            json!({ "resets_at": resets_at, "self_host_url": SELF_HOST_URL }),
            Some((resets_at - chrono::Utc::now()).num_seconds().max(1) as u64),
        ),
        VisitError::TooEarly { at } => (
            StatusCode::CONFLICT,
            "rustybin_extend_too_early",
            format!(
                "Extend opens {} minutes before the visit ends",
                super::visits::EXTEND_WINDOW_MINUTES
            ),
            json!({ "extendable_at": at }),
            None,
        ),
        VisitError::Busy { retry_after } => (
            StatusCode::SERVICE_UNAVAILABLE,
            "rustybin_mocks_busy",
            "the demo is busy: try again in a few minutes, or run your own Rustybin".to_string(),
            json!({ "self_host_url": SELF_HOST_URL }),
            Some(retry_after),
        ),
        VisitError::RateLimited { retry_after } => (
            StatusCode::TOO_MANY_REQUESTS,
            "rustybin_mocks_rate_limited",
            "mocks are being created too fast: wait a little".to_string(),
            json!({}),
            Some(retry_after),
        ),
        VisitError::InvalidEmail(m) => (
            StatusCode::BAD_REQUEST,
            "rustybin_invalid_email",
            m,
            json!({}),
            None,
        ),
        VisitError::Paused => (
            StatusCode::SERVICE_UNAVAILABLE,
            "rustybin_self_service_paused",
            "creating mocks is paused on this demo: try again later, or run your own Rustybin"
                .to_string(),
            json!({ "self_host_url": SELF_HOST_URL }),
            Some(300),
        ),
    };
    let mut body = json!({ "error": code, "message": message });
    if let (Value::Object(b), Value::Object(more)) = (&mut body, extra) {
        b.extend(more);
    }
    ServiceError {
        status,
        body,
        retry_after: retry,
    }
}

/// Metric label of a visit decision.
pub fn event_of(e: &VisitError) -> &'static str {
    match e {
        VisitError::NotFound => "not_found",
        VisitError::EmailRequired => "email_required",
        VisitError::TimeUsed { .. } => "time_used",
        VisitError::DailyLimit { .. } => "daily_limit",
        VisitError::TooEarly { .. } => "too_early",
        VisitError::Busy { .. } => "busy",
        VisitError::RateLimited { .. } => "rate_limited",
        VisitError::InvalidEmail(_) => "invalid_email",
        VisitError::Paused => "paused",
    }
}

/// A store refusal for a visitor (caps) as the matching visit error.
pub fn store_refusal(e: &StoreError) -> Option<ServiceError> {
    match e {
        StoreError::OwnerCap(m) => Some(ServiceError::with_body(
            StatusCode::CONFLICT,
            json!({ "error": "rustybin_mock_cap", "message": m }),
        )),
        _ => None,
    }
}

/// What the console needs to know about self-service (`GET /_rustybin/mocks`).
pub fn info(config: &Config) -> Value {
    let cfg = &config.self_service;
    json!({
        "enabled": cfg.enabled,
        "host": cfg.host,
        "per_owner": cfg.per_owner,
        "visit_minutes": cfg.visit_minutes,
        "anon_hours": cfg.anon_hours,
        "daily_hours": cfg.daily_hours,
        "leads": cfg.enabled && cfg.leads_enabled(),
        "privacy_url": cfg.privacy_url,
        "consent": cfg.leads_consent.as_str(),
        "self_host_url": SELF_HOST_URL,
    })
}

/// The visit store, or the `404` of an instance without self-service.
pub fn visits_of(state: &AppState) -> Result<&std::sync::Arc<Visits>, ServiceError> {
    state.mocks.visits().ok_or_else(|| {
        ServiceError::with_body(
            StatusCode::NOT_FOUND,
            json!({
                "error": "rustybin_self_service_off",
                "message": "self-service mocks are off on this instance",
            }),
        )
    })
}

fn email_input(body: &Bytes) -> Result<EmailInput, ServiceError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(EmailInput::default());
    }
    serde_json::from_slice(body)
        .map_err(|e| ServiceError::bad_request(format!("invalid request: {e}")))
}

/// The visit state with its mocks.
pub fn visit_json(state: &AppState, hash: &str) -> Option<Value> {
    let visits = state.mocks.visits()?;
    let view = visits.view(hash)?;
    let mocks = state.mocks.owned_names(&Owner::Visit(hash.to_string()));
    let mut v = serde_json::to_value(view).ok()?;
    if let Value::Object(m) = &mut v {
        m.insert("mocks".into(), json!(mocks));
        m.insert("mock_cap".into(), json!(visits.config().per_owner));
    }
    Some(v)
}

// ── Visits: the logic (shared with the management MCP) ──────────────

/// The caller's visit (`GET /_rustybin/mocks/visit`).
pub fn visit_view(state: &AppState, c: &Caller) -> Result<Value, ServiceError> {
    visits_of(state)?;
    c.visit()
        .and_then(|h| visit_json(state, h))
        .ok_or_else(|| visit_error(VisitError::NotFound, &state.config))
}

/// Start a visit (`POST /_rustybin/mocks/visit`): the manage token (shown
/// once) and the visit.
pub fn start(state: &AppState, c: &Caller, input: &EmailInput) -> Result<Value, ServiceError> {
    let visits = visits_of(state)?.clone();
    match visits.start(&c.session, input) {
        Ok((token, hash)) => {
            state.metrics.count_visit_event("started");
            tracing::info!(owner = %Owner::Visit(hash.clone()).fingerprint(), "visit started");
            Ok(json!({ "manage_token": token, "visit": visit_json(state, &hash) }))
        }
        Err(e) => {
            state.metrics.count_visit_event(event_of(&e));
            Err(visit_error(e, &state.config))
        }
    }
}

/// Extend the caller's visit (`POST /_rustybin/mocks/visit/extend`).
pub fn extend(state: &AppState, c: &Caller, input: &EmailInput) -> Result<Value, ServiceError> {
    let visits = visits_of(state)?.clone();
    let Some(hash) = c.visit().map(str::to_string) else {
        return Err(visit_error(VisitError::NotFound, &state.config));
    };
    let mocks = state.mocks.owned_names(&Owner::Visit(hash.clone()));
    match visits.extend(&hash, input, &mocks) {
        Ok(_) => {
            state.metrics.count_visit_event("extended");
            tracing::info!(owner = %Owner::Visit(hash.clone()).fingerprint(), "visit extended");
            Ok(visit_json(state, &hash).unwrap_or(Value::Null))
        }
        Err(e) => {
            state.metrics.count_visit_event(event_of(&e));
            Err(visit_error(e, &state.config))
        }
    }
}

/// A new manage token for the caller's visit (`POST /_rustybin/mocks/visit/token`).
pub fn rotate(state: &AppState, c: &Caller) -> Result<Value, ServiceError> {
    let visits = visits_of(state)?.clone();
    let rotated = c
        .visit()
        .and_then(|h| visits.rotate(h).map(|r| (h.to_string(), r)));
    let Some((old, (token, hash))) = rotated else {
        return Err(visit_error(VisitError::NotFound, &state.config));
    };
    state
        .mocks
        .reassign(&Owner::Visit(old), Owner::Visit(hash.clone()));
    Ok(json!({ "manage_token": token, "visit": visit_json(state, &hash) }))
}

/// End the caller's visit and delete its mocks (`DELETE /_rustybin/mocks/visit`).
pub fn end_own(state: &AppState, c: &Caller) -> Result<Value, ServiceError> {
    let visits = visits_of(state)?.clone();
    let Some(hash) = c.visit().map(str::to_string) else {
        return Err(visit_error(VisitError::NotFound, &state.config));
    };
    let deleted = state.mocks.delete_owned(&Owner::Visit(hash.clone()));
    visits.end(&hash);
    state.metrics.count_visit_event("ended");
    Ok(json!({ "ended": true, "deleted": deleted }))
}

/// The visit store for an operator (admin), else the refusal.
fn operator(state: &AppState, c: &Caller) -> Result<std::sync::Arc<Visits>, ServiceError> {
    let visits = visits_of(state)?.clone();
    if !c.admin {
        c.admin_guard.map_err(ServiceError::admin)?;
        return Err(ServiceError::forbidden("this route needs admin rights"));
    }
    Ok(visits)
}

/// Every running visit (`GET /_rustybin/mocks/visits`, admin).
pub fn list_all(state: &AppState, c: &Caller) -> Result<Value, ServiceError> {
    let visits = operator(state, c)?;
    let rows: Vec<Value> = visits
        .list()
        .into_iter()
        .map(|mut row| {
            let full = row["id"].as_str().and_then(|id| visits.hash_for_id(id));
            if let (Some(h), Value::Object(m)) = (full, &mut row) {
                m.insert(
                    "mocks".into(),
                    json!(state.mocks.owned_names(&Owner::Visit(h))),
                );
            }
            row
        })
        .collect();
    Ok(json!({
        "paused": visits.paused(),
        "count": rows.len(),
        "visitor_mocks": state.mocks.visitor_count(),
        "visits": rows,
    }))
}

/// End every visit (`DELETE /_rustybin/mocks/visits`, admin).
pub fn end_all(state: &AppState, c: &Caller) -> Result<Value, ServiceError> {
    let visits = operator(state, c)?;
    let mut deleted = Vec::new();
    for hash in visits.end_all() {
        deleted.extend(state.mocks.delete_owned(&Owner::Visit(hash)));
    }
    tracing::info!(
        "all visits ended by an operator ({} mocks deleted)",
        deleted.len()
    );
    Ok(json!({ "ended": true, "deleted": deleted }))
}

/// End one visit by its id (`DELETE /_rustybin/mocks/visits/{id}`, admin).
pub fn end_one(state: &AppState, c: &Caller, id: &str) -> Result<Value, ServiceError> {
    let visits = operator(state, c)?;
    let Some(hash) = visits.hash_for_id(id) else {
        return Err(visit_error(VisitError::NotFound, &state.config));
    };
    let deleted = state.mocks.delete_owned(&Owner::Visit(hash.clone()));
    visits.end(&hash);
    Ok(json!({ "ended": true, "deleted": deleted }))
}

/// Pause or resume self-service (`POST /_rustybin/mocks/visits/pause`,
/// `/resume`, admin).
pub fn set_paused(state: &AppState, c: &Caller, paused: bool) -> Result<Value, ServiceError> {
    let visits = operator(state, c)?;
    visits.pause(paused);
    tracing::info!(
        "self-service {} by an operator",
        if paused { "paused" } else { "resumed" }
    );
    Ok(json!({ "paused": paused }))
}

// ── Visits: the routes ──────────────────────────────────────────────

fn answer(status: StatusCode, r: Result<Value, ServiceError>) -> Response {
    match r {
        Ok(v) => (status, Json(v)).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /_rustybin/mocks/visit`: the caller's visit.
pub async fn get_visit(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let c = caller(&state, &headers, &extensions);
    answer(StatusCode::OK, visit_view(&state, &c))
}

/// `POST /_rustybin/mocks/visit`: start a visit (optionally with the email
/// step); answers the manage token once.
pub async fn start_visit(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    body: Bytes,
) -> Response {
    if let Err(e) = visits_of(&state) {
        return e.into_response();
    }
    let input = match email_input(&body) {
        Ok(i) => i,
        Err(e) => return e.into_response(),
    };
    let c = caller(&state, &headers, &extensions);
    answer(StatusCode::CREATED, start(&state, &c, &input))
}

/// `POST /_rustybin/mocks/visit/extend`.
pub async fn extend_visit(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    body: Bytes,
) -> Response {
    if let Err(e) = visits_of(&state) {
        return e.into_response();
    }
    let input = match email_input(&body) {
        Ok(i) => i,
        Err(e) => return e.into_response(),
    };
    let c = caller(&state, &headers, &extensions);
    answer(StatusCode::OK, extend(&state, &c, &input))
}

/// `POST /_rustybin/mocks/visit/token`: a new manage token for the visit.
pub async fn rotate_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let c = caller(&state, &headers, &extensions);
    answer(StatusCode::OK, rotate(&state, &c))
}

/// `DELETE /_rustybin/mocks/visit`: end the caller's visit and delete its
/// mocks.
pub async fn end_visit(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let c = caller(&state, &headers, &extensions);
    answer(StatusCode::OK, end_own(&state, &c))
}

/// `GET /_rustybin/mocks/visits` (admin): every running visit.
pub async fn list_visits(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let c = caller(&state, &headers, &extensions);
    answer(StatusCode::OK, list_all(&state, &c))
}

/// `DELETE /_rustybin/mocks/visits` (admin): end every visit.
pub async fn end_all_visits(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let c = caller(&state, &headers, &extensions);
    answer(StatusCode::OK, end_all(&state, &c))
}

/// `DELETE /_rustybin/mocks/visits/{id}` (admin): end one visit.
pub async fn end_one_visit(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let c = caller(&state, &headers, &extensions);
    answer(StatusCode::OK, end_one(&state, &c, &id))
}

/// `POST /_rustybin/mocks/visits/pause` (admin): no new visits or mocks.
pub async fn pause(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let c = caller(&state, &headers, &extensions);
    answer(StatusCode::OK, set_paused(&state, &c, true))
}

/// `POST /_rustybin/mocks/visits/resume` (admin).
pub async fn resume(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let c = caller(&state, &headers, &extensions);
    answer(StatusCode::OK, set_paused(&state, &c, false))
}

#[cfg(test)]
#[path = "selfservice_tests.rs"]
mod tests;
