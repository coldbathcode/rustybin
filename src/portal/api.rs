//! The portal's routes.
//!
//! Public (`/portal*`, outside the control plane, exempt from the key layer
//! like `/ui`; `portal.auth` decides who reads the data):
//! - `GET /portal` (redirect), `GET /portal/` and `GET /portal/{*path}`: the
//!   embedded SPA (`ui-portal/`), `index.html` for extension-less paths.
//! - `GET /portal/api/catalog`: the published items ([`super::items`]).
//! - `POST /portal/api/events`: a view or a Try it call, counted.
//! - `GET /portal/api/items/{id}/collections/{format}`: a published mock as a
//!   collection, counted.
//! - `GET /portal/reference/{id}`: a published mock's spec in the full API
//!   reference viewer (the `/docs` viewer).
//!
//! Control (`/_rustybin/portal*`): the console's view, publish (`PUT`) and
//! unpublish (`DELETE`) per item, insights.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use super::insights::Event;
use super::items::{self, Ctx, Entry};
use super::{AuthMode, Cached, ItemId, Meta, PublishError};
use crate::catalog::{category, Endpoint, Example};
use crate::landing::html_escape;
use crate::mocks::service::{Links, ServiceError, Svc};
use crate::mocks::visits::Owner;
use crate::state::AppState;

include!(concat!(env!("OUT_DIR"), "/portal_assets.rs"));

fn off() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": "the portal is off",
            "hint": "publish an item (Management, Portal in the console) or set portal.enabled (RUSTYBIN_PORTAL=on)",
        })),
    )
        .into_response()
}

fn unauthorized(state: &AppState, mode: AuthMode) -> Response {
    let (message, extra) = match mode {
        AuthMode::Key => (
            "this portal needs an API key",
            json!({ "header": crate::keys::KEY_HEADER }),
        ),
        _ => (
            "this portal needs the console sign-in",
            json!({ "signin": crate::login::enabled(&state.config) }),
        ),
    };
    let mut body = json!({ "error": message, "auth": mode.as_str() });
    if let (Value::Object(b), Value::Object(e)) = (&mut body, extra) {
        b.extend(e);
    }
    let mut resp = (StatusCode::UNAUTHORIZED, Json(body)).into_response();
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"rustybin-portal\""),
    );
    resp
}

/// The portal is on and the caller holds what `portal.auth` asks for.
#[allow(clippy::result_large_err)]
fn gate(
    state: &AppState,
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
) -> Result<(), Response> {
    if !super::active(state) {
        return Err(off());
    }
    match super::auth_mode(&state.settings) {
        AuthMode::Open => Ok(()),
        AuthMode::Password => crate::control_auth::credentials_grant(headers, &state.config)
            .map(|_| ())
            .map_err(|_| unauthorized(state, AuthMode::Password)),
        AuthMode::Key => {
            if extensions.get::<crate::keys::KeyContext>().is_some() {
                return Ok(());
            }
            match state
                .keys
                .lookup(crate::keys::presented_key(headers, uri).as_deref())
            {
                crate::keys::Lookup::Found(_) => Ok(()),
                _ => Err(unauthorized(state, AuthMode::Key)),
            }
        }
    }
}

fn ctx(state: &AppState, headers: &HeaderMap, extensions: &Extensions, uri: &Uri) -> Ctx {
    let links = Links::from_request(headers, extensions, uri, &state.config);
    match super::base_url_setting(&state.settings) {
        Some(base) => Ctx {
            base,
            gateway: true,
            links,
        },
        None => Ctx {
            base: links.base_url().to_string(),
            gateway: false,
            links,
        },
    }
}

/// The published, shown entry for `key`.
fn public_entry(state: &AppState, key: &str) -> Option<Entry> {
    items::public_entries(state)
        .into_iter()
        .find(|e| e.key == key)
}

// ── The SPA ─────────────────────────────────────────────────────────

fn portal_assets() -> &'static std::collections::HashMap<&'static str, (&'static [u8], String)> {
    static ASSETS: std::sync::OnceLock<
        std::collections::HashMap<&'static str, (&'static [u8], String)>,
    > = std::sync::OnceLock::new();
    ASSETS.get_or_init(|| {
        PORTAL_ASSETS
            .iter()
            .map(|(p, b)| (*p, (*b, crate::ui::etag_of(b))))
            .collect()
    })
}

/// Paths of every embedded portal file (tests).
pub fn asset_paths() -> Vec<&'static str> {
    let mut v: Vec<_> = PORTAL_ASSETS.iter().map(|(p, _)| *p).collect();
    v.sort_unstable();
    v
}

/// `<meta name="rustybin-portal-*">` elements: what the SPA needs before it
/// may read the catalogue (escaped).
pub fn portal_meta(state: &AppState) -> String {
    let brand = super::branding(&state.settings);
    let mut out = String::new();
    let mut meta = |name: &str, value: &str| {
        out.push_str(&format!(
            "<meta name=\"rustybin-portal-{name}\" content=\"{}\">",
            html_escape(value)
        ));
    };
    meta("auth", super::auth_mode(&state.settings).as_str());
    meta(
        "signin",
        if crate::login::enabled(&state.config) {
            "true"
        } else {
            "false"
        },
    );
    meta("title", brand["title"].as_str().unwrap_or("API portal"));
    meta("accent", brand["accent"].as_str().unwrap_or("rust"));
    out
}

fn serve_index(headers: &HeaderMap, depth: usize, state: &AppState) -> Response {
    let Some((bytes, _)) = portal_assets().get("index.html") else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "portal assets are not embedded in this build" })),
        )
            .into_response();
    };
    let base = if depth <= 1 {
        String::new()
    } else {
        format!("<base href=\"{}\">", "../".repeat(depth - 1))
    };
    let title = super::branding(&state.settings)["title"]
        .as_str()
        .unwrap_or("API portal")
        .to_string();
    let html = String::from_utf8_lossy(bytes)
        .replacen("<head>", &format!("<head>{base}{}", portal_meta(state)), 1)
        .replacen(
            "<title>API portal</title>",
            &format!("<title>{}</title>", html_escape(&title)),
            1,
        );
    let etag = crate::ui::etag_of(html.as_bytes());
    crate::ui::serve_bytes(
        headers,
        html.into_bytes(),
        "text/html; charset=utf-8",
        &etag,
    )
}

async fn root(State(state): State<AppState>, RawQuery(query): RawQuery) -> Response {
    if !super::active(&state) {
        return off();
    }
    let location = match query {
        Some(q) if !q.is_empty() => format!("portal/?{q}"),
        _ => "portal/".to_string(),
    };
    let mut resp = StatusCode::TEMPORARY_REDIRECT.into_response();
    if let Ok(v) = HeaderValue::from_str(&location) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    resp
}

async fn index(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !super::active(&state) {
        return off();
    }
    serve_index(&headers, 0, &state)
}

async fn asset(
    State(state): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !super::active(&state) {
        return off();
    }
    if let Some((bytes, etag)) = portal_assets().get(path.as_str()) {
        return crate::ui::serve_bytes(
            &headers,
            bytes.to_vec(),
            crate::ui::content_type_for(&path),
            etag,
        );
    }
    let last = path.rsplit('/').next().unwrap_or("");
    if path.starts_with("api/")
        || path.starts_with("reference/")
        || last.contains('.')
        || path.split('/').any(|s| s == "..")
    {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no such portal page or file", "path": format!("/portal/{path}") })),
        )
            .into_response();
    }
    let depth = path.trim_end_matches('/').split('/').count();
    serve_index(&headers, depth, &state)
}

// ── The catalogue ───────────────────────────────────────────────────

async fn catalog_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    if let Err(resp) = gate(&state, &headers, &extensions, &uri) {
        return resp;
    }
    state.portal.sweep(&state);
    let ctx = ctx(&state, &headers, &extensions, &uri);
    let fingerprint = format!(
        "{}|{}",
        items::fingerprint(&state, &ctx),
        Utc::now().date_naive()
    );
    let entry = match state.portal.cached(&fingerprint) {
        Some(c) => c,
        None => {
            let body = serde_json::to_vec(&items::catalog_json(&state, &ctx)).unwrap_or_default();
            let etag = crate::ui::etag_of(&body);
            let c = Arc::new(Cached {
                fingerprint,
                body: Arc::new(body),
                etag,
            });
            state.portal.store_cached(c.clone());
            c
        }
    };
    let mut resp = crate::ui::serve_bytes(
        &headers,
        entry.body.to_vec(),
        "application/json",
        &entry.etag,
    );
    if super::auth_mode(&state.settings) != AuthMode::Open {
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-cache"),
        );
    }
    resp
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventBody {
    item: String,
    event: String,
}

async fn events(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Response {
    if let Err(resp) = gate(&state, &headers, &extensions, &uri) {
        return resp;
    }
    if body.len() > 1024 {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "error": "an event is at most 1 KiB" })),
        )
            .into_response();
    }
    let ev: EventBody = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("invalid event: {e}"), "expected": { "item": "mock:petstore", "event": "view | try" } })),
            )
                .into_response()
        }
    };
    let event = match ev.event.as_str() {
        "view" => Event::View,
        "try" => Event::Try,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "event is view or try" })),
            )
                .into_response()
        }
    };
    if public_entry(&state, &ev.item).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no such published item" })),
        )
            .into_response();
    }
    state.portal.record(&ev.item, event, Utc::now());
    StatusCode::NO_CONTENT.into_response()
}

async fn download(
    State(state): State<AppState>,
    Path((id, format)): Path<(String, String)>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    if let Err(resp) = gate(&state, &headers, &extensions, &uri) {
        return resp;
    }
    let not_found = || {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no such published mock" })),
        )
            .into_response()
    };
    let Some(entry) = public_entry(&state, &id) else {
        return not_found();
    };
    let ItemId::Mock(name) = &entry.id else {
        return not_found();
    };
    let Some(s) = state.mocks.get(name) else {
        return not_found();
    };
    let Some(c) = s.live.as_ref() else {
        return not_found();
    };
    let ctx = ctx(&state, &headers, &extensions, &uri);
    let base = ctx.mock_base(&s.owner, name);
    let Some((ct, file, text)) =
        crate::mocks::collections::render(c, name, &format, &base, entry.published.credentials)
    else {
        let known: Vec<&str> = crate::mocks::collections::FORMATS
            .iter()
            .map(|(f, _, _)| *f)
            .collect();
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("unknown format {format:?}: use one of {}", known.join(", ")) })),
        )
            .into_response();
    };
    state
        .portal
        .record(&entry.key, Event::Download(&format), Utc::now());
    let mut resp = ([(header::CONTENT_TYPE, ct)], text).into_response();
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{file}\"")) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}

/// The full API reference of a published mock (the `/docs` viewer). Not
/// behind `portal.auth`: a page load carries no header, and the spec it
/// shows is the mock's public one.
async fn reference(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    if !super::active(&state) {
        return off();
    }
    let entry = public_entry(&state, &id);
    let target = entry.as_ref().and_then(|e| match &e.id {
        ItemId::Mock(name) => state.mocks.get(name).map(|s| (e, s)),
        _ => None,
    });
    let Some((entry, s)) = target else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no such published mock" })),
        )
            .into_response();
    };
    let spec_url = if s.owner.is_visitor() {
        let ctx = ctx(&state, &headers, &extensions, &uri);
        format!(
            "{}/_rustybin/openapi.json",
            ctx.mock_base(&s.owner, &s.name)
        )
    } else {
        format!("../../mock/{}/_rustybin/openapi.json", s.name)
    };
    let title = entry
        .published
        .title
        .clone()
        .or_else(|| s.live.as_ref().map(|c| c.spec.title.clone()))
        .unwrap_or_else(|| s.name.clone());
    let html = format!(
        "<!DOCTYPE html>\n<html>\n<head>\n    <title>{}</title>\n    <meta charset=\"utf-8\" />\n    <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n</head>\n<body>\n    <script id=\"api-reference\" data-url=\"{}\"></script>\n    <script src=\"https://cdn.jsdelivr.net/npm/@scalar/api-reference\"></script>\n</body>\n</html>",
        html_escape(&title),
        html_escape(&spec_url)
    );
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::REFERRER_POLICY, "same-origin"),
        ],
        html,
    )
        .into_response()
}

// ── Control plane ───────────────────────────────────────────────────

fn published_json(e: &Entry) -> Value {
    let p = &e.published;
    json!({
        "title": p.title,
        "summary": p.summary,
        "tags": p.tags,
        "contact": p.contact,
        "order": p.order,
        "credentials": p.credentials,
        "published_at": p.published_at.to_rfc3339(),
        "updated_at": p.updated_at.to_rfc3339(),
        "by": p.by,
        "visitor": p.visitor(),
        "source": e.source,
    })
}

fn item_view(
    state: &AppState,
    c: &items::Candidate,
    entry: Option<&Entry>,
    insights: &super::insights::Insights,
) -> Value {
    let today = Utc::now().date_naive();
    json!({
        "id": c.id,
        "kind": c.kind,
        "title": c.title,
        "summary": c.summary,
        "state": c.state,
        "visitor": c.owner.as_ref().is_some_and(Owner::is_visitor),
        "published": entry.map(published_json),
        "hidden": entry.and_then(|e| items::hidden_reason(state, e)),
        "insights": entry.map(|_| insights.item_json(&c.id, today)),
    })
}

fn settings_view(state: &AppState) -> Value {
    json!({
        "enabled": super::active(state),
        "mode": super::mode(&state.settings),
        "auth": super::auth_mode(&state.settings).as_str(),
        "base_url": super::base_url_setting(&state.settings),
        "visitors": state.settings.bool("portal.visitors"),
        "url": "/portal/",
        "persisted": state.portal.persisted(),
        "branding": super::branding(&state.settings),
    })
}

async fn control_view(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    state.portal.sweep(&state);
    let caller = crate::mocks::selfservice::caller(&state, &headers, &extensions);
    let entries = items::entries(&state);
    let insights = state.portal.insights();
    let candidates = items::candidates(&state, |s| caller.can_read(s));
    let mut list: Vec<Value> = candidates
        .iter()
        .map(|c| item_view(&state, c, entries.iter().find(|e| e.key == c.id), &insights))
        .collect();
    // Published items whose mock is gone (shown so they can be removed).
    for e in &entries {
        if candidates.iter().any(|c| c.id == e.key) {
            continue;
        }
        let mine = e
            .published
            .owner
            .as_ref()
            .is_none_or(|o| caller.admin || caller.owns(o));
        if !mine {
            continue;
        }
        let c = items::Candidate {
            id: e.key.clone(),
            kind: e.id.kind(),
            title: e.published.title.clone().unwrap_or_else(|| e.key.clone()),
            summary: String::new(),
            state: "missing".into(),
            owner: e.published.owner.clone(),
        };
        list.push(item_view(&state, &c, Some(e), &insights));
    }
    let published = list.iter().filter(|i| !i["published"].is_null()).count();
    let shown = list
        .iter()
        .filter(|i| !i["published"].is_null() && i["hidden"].is_null())
        .count();
    let mut body = settings_view(&state);
    body["items"] = json!(list);
    body["counts"] = json!({ "published": published, "shown": shown, "max": super::MAX_ITEMS });
    Json(body).into_response()
}

/// May the caller publish `id`? Manage rights on a mock (a visitor's own
/// mock only with `portal.visitors`), admin for everything else and for a
/// mock that no longer exists. Returns the mock's owner and the audit name.
fn authorize_item(
    state: &AppState,
    id: &ItemId,
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
) -> Result<(Option<Owner>, String), ServiceError> {
    let svc = Svc::from_request(state, headers, extensions, uri);
    let admin = |svc: &Svc| -> Result<(), ServiceError> {
        svc.require_admin()?;
        if !svc.caller.admin {
            return Err(ServiceError::forbidden(
                "publishing this item needs the admin token or a token with the admin scope",
            ));
        }
        Ok(())
    };
    let principal = crate::settings::principal(headers, &state.config);
    match id {
        ItemId::Mock(name) if state.mocks.get(name).is_some() => {
            let snap = svc.manageable(name)?;
            if snap.owner.is_visitor() && !svc.caller.admin {
                if !state.settings.bool("portal.visitors") {
                    return Err(ServiceError::forbidden(
                        "visitors may not publish to the portal on this instance (portal.visitors is off)",
                    ));
                }
                let by = snap.owner.fingerprint();
                return Ok((Some(snap.owner), by));
            }
            Ok((Some(snap.owner), principal))
        }
        _ => {
            admin(&svc)?;
            Ok((None, principal))
        }
    }
}

fn file_defined(state: &AppState, key: &str) -> bool {
    state.mocks.file_portal().0.iter().any(|i| i.id == key)
}

fn conflict_file(key: &str) -> ServiceError {
    ServiceError::new(
        StatusCode::CONFLICT,
        format!("{key} is published by the config file (portal.items of rustybin.yaml): change it there"),
    )
}

async fn publish(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ServiceError> {
    let id = ItemId::parse(&id).map_err(ServiceError::bad_request)?;
    let key = id.as_string();
    if let ItemId::Mock(name) = &id {
        if state.mocks.get(name).is_none() {
            // Admin first (an unknown name says nothing to others), then 404.
            let svc = Svc::from_request(&state, &headers, &extensions, &uri);
            if !svc.caller.admin {
                svc.manageable(name)?;
            }
            return Err(ServiceError::not_found(format!("mock {name:?} not found")));
        }
    }
    let (owner, by) = authorize_item(&state, &id, &headers, &extensions, &uri)?;
    if file_defined(&state, &key) {
        return Err(conflict_file(&key));
    }
    let meta: Meta = if body.iter().all(u8::is_ascii_whitespace) {
        Meta::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| ServiceError::bad_request(format!("invalid request: {e}")))?
    };
    let meta = super::clean_meta(&meta).map_err(ServiceError::bad_request)?;
    let (_, created) = state
        .portal
        .publish(&key, meta, by.clone(), owner, Utc::now())
        .map_err(|e| match e {
            PublishError::Full => ServiceError::new(
                StatusCode::CONFLICT,
                format!("the portal holds at most {} items", super::MAX_ITEMS),
            ),
            PublishError::Io(e) => ServiceError::new(StatusCode::INTERNAL_SERVER_ERROR, e),
        })?;
    tracing::info!(target: "rustybin::portal", item = %key, by = %by, created, "item published");
    let entries = items::entries(&state);
    let entry = entries.iter().find(|e| e.key == key);
    let cand = items::candidate(&state, &id).ok_or_else(|| ServiceError::not_found("gone"))?;
    let view = item_view(&state, &cand, entry, &state.portal.insights());
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(view)).into_response())
}

async fn unpublish(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let id = ItemId::parse(&id).map_err(ServiceError::bad_request)?;
    let key = id.as_string();
    let (_, by) = authorize_item(&state, &id, &headers, &extensions, &uri)?;
    if file_defined(&state, &key) {
        return Err(conflict_file(&key));
    }
    let removed = state
        .portal
        .unpublish(&key)
        .map_err(|e| ServiceError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if !removed {
        return Err(ServiceError::not_found(format!("{key} is not published")));
    }
    tracing::info!(target: "rustybin::portal", item = %key, by = %by, "item unpublished");
    Ok(Json(json!({ "id": key, "published": false })).into_response())
}

async fn insights_view(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let caller = crate::mocks::selfservice::caller(&state, &headers, &extensions);
    let today = Utc::now().date_naive();
    let insights = state.portal.insights();
    let list: Vec<Value> = items::entries(&state)
        .iter()
        .filter(|e| {
            e.published
                .owner
                .as_ref()
                .is_none_or(|o| !o.is_visitor() || caller.admin || caller.owns(o))
        })
        .map(|e| {
            let title = items::candidate(&state, &e.id)
                .map(|c| e.published.title.clone().unwrap_or(c.title))
                .unwrap_or_else(|| e.key.clone());
            let mut v = insights.item_json(&e.key, today);
            v["id"] = json!(e.key);
            v["title"] = json!(title);
            v
        })
        .collect();
    Json(json!({
        "items": list,
        "days": super::insights::DAYS,
        "persisted": state.portal.persisted(),
        "note": "Counts only: no visitor identifiers are stored.",
    }))
    .into_response()
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/portal", get(root))
        .route("/portal/", get(index))
        .route(super::CATALOG_PATH, get(catalog_handler))
        .route(super::EVENTS_PATH, post(events))
        .route(super::DOWNLOAD_PATH, get(download))
        .route(super::REFERENCE_PATH, get(reference))
        .route("/portal/{*path}", get(asset))
        .route(super::CONTROL_PATH, get(control_view))
        .route(super::CONTROL_ITEM_PATH, put(publish).delete(unpublish))
        .route(super::INSIGHTS_PATH, get(insights_view))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/_rustybin/portal",
            &["GET"],
            category::CONTROL,
            "Dev portal: settings, every publishable item and its state",
        )
        .description(
            "Every item that can be published (mock APIs, MCP servers, the mock LLM, A2A agents, \
             the HTTP tools, the MCP registry), with its portal metadata, why a published item is \
             hidden (a mock not deployed) and its counters.",
        )
        .example(Example::get("Portal items", "/_rustybin/portal")),
        Endpoint::new(
            "/_rustybin/portal/items/{id}",
            &["PUT", "DELETE"],
            category::CONTROL,
            "Publish an item to the dev portal (with its metadata), or unpublish it",
        )
        .description(
            "Ids: mock:<name>, mcp:<server>, llm, a2a:<agent>, tools, registry. A mock needs \
             manage rights (visitors only with portal.visitors), everything else admin. Body: \
             title, summary (Markdown), tags, contact, order, credentials (show a mock's demo \
             credentials).",
        )
        .example(
            Example::new("Publish the mock LLM", "PUT", "/_rustybin/portal/items/llm")
                .json(r#"{"title": "Mock LLM", "tags": ["ai"]}"#),
        )
        .example(
            Example::new(
                "Publish the sample mock",
                "PUT",
                "/_rustybin/portal/items/mock:petstore",
            )
            .json(r#"{"summary": "The pet store for gateway demos."}"#),
        )
        .example(Example::delete(
            "Unpublish the mock LLM",
            "/_rustybin/portal/items/llm",
        )),
        Endpoint::new(
            "/_rustybin/portal/insights",
            &["GET"],
            category::CONTROL,
            "Dev portal insights: views per day, downloads by format, Try it calls",
        )
        .example(Example::get(
            "Portal insights",
            "/_rustybin/portal/insights",
        )),
        Endpoint::new(
            "/portal",
            &["GET"],
            category::DOCS,
            "Dev portal (redirects to /portal/)",
        )
        .description(
            "The read-only portal testers use: published mock APIs with Try it and downloads, MCP \
             servers, the mock LLM, agents and tools. On once an item is published \
             (RUSTYBIN_PORTAL), behind RUSTYBIN_PORTAL_AUTH.",
        )
        .example(Example::get("Open the portal", "/portal").expect_status(307)),
        Endpoint::new(
            "/portal/",
            &["GET"],
            category::DOCS,
            "Dev portal entry page",
        )
        .example(Example::get("Portal index", "/portal/")),
        Endpoint::new(
            "/portal/{*path}",
            &["GET"],
            category::DOCS,
            "Dev portal assets; extension-less paths fall back to index.html",
        )
        .example(Example::get("Portal script", "/portal/portal.js")),
        Endpoint::new(
            "/portal/api/catalog",
            &["GET"],
            category::DOCS,
            "Dev portal catalogue: the published items with what their pages show",
        )
        .description("Only published items; ETag and If-None-Match.")
        .example(Example::get("Portal catalogue", "/portal/api/catalog")),
        Endpoint::new(
            "/portal/api/events",
            &["POST"],
            category::DOCS,
            "Count a portal view or Try it call (no identifiers stored)",
        )
        .example(
            Example::post("Count a view", "/portal/api/events")
                .json(r#"{"item": "mock:petstore", "event": "view"}"#)
                .expect_status(204),
        ),
        Endpoint::new(
            "/portal/api/items/{id}/collections/{format}",
            &["GET"],
            category::DOCS,
            "A published mock as a collection (Postman, Hurl, curl, .http, k6, Bruno, Insomnia)",
        )
        .example(Example::get(
            "Sample mock as Hurl",
            "/portal/api/items/mock:petstore/collections/hurl",
        )),
        Endpoint::new(
            "/portal/reference/{id}",
            &["GET"],
            category::DOCS,
            "A published mock's spec in the API reference viewer",
        )
        .example(Example::get(
            "Sample mock reference",
            "/portal/reference/mock:petstore",
        )),
    ]
}

pub fn openapi_paths() -> Value {
    let json_ok = |d: &str| json!({ "200": { "description": d, "content": { "application/json": { "schema": { "type": "object" } } } } });
    let id_param = json!({ "name": "id", "in": "path", "required": true, "schema": { "type": "string" }, "description": "mock:<name>, mcp:<server>, llm, a2a:<agent>, tools or registry" });
    let meta = json!({ "type": "object", "properties": {
        "title": { "type": "string", "maxLength": super::MAX_TITLE },
        "summary": { "type": "string", "maxLength": super::MAX_SUMMARY, "description": "Markdown" },
        "tags": { "type": "array", "items": { "type": "string", "maxLength": super::MAX_TAG }, "maxItems": super::MAX_TAGS },
        "contact": { "type": "string", "maxLength": super::MAX_CONTACT },
        "order": { "type": "integer", "minimum": -1000, "maximum": 1000 },
        "credentials": { "type": "boolean", "description": "Show a mock's demo credentials (default true)" }
    } });
    json!({
        "/_rustybin/portal": { "get": {
            "tags": ["Control Plane"], "summary": "Dev portal items and settings", "operationId": "getPortal",
            "responses": json_ok("Settings, items, counts")
        } },
        "/_rustybin/portal/items/{id}": {
            "put": {
                "tags": ["Control Plane"], "summary": "Publish an item to the dev portal", "operationId": "publishPortalItem",
                "parameters": [id_param],
                "requestBody": { "required": false, "content": { "application/json": { "schema": meta } } },
                "responses": {
                    "200": { "description": "Updated" }, "201": { "description": "Published" },
                    "400": { "description": "Invalid id or metadata" }, "403": { "description": "Not allowed" },
                    "404": { "description": "Unknown mock" }, "409": { "description": "Defined in the config file, or the portal is full" }
                }
            },
            "delete": {
                "tags": ["Control Plane"], "summary": "Unpublish an item", "operationId": "unpublishPortalItem",
                "parameters": [id_param],
                "responses": { "200": { "description": "Unpublished" }, "404": { "description": "Not published" }, "409": { "description": "Defined in the config file" } }
            }
        },
        "/_rustybin/portal/insights": { "get": {
            "tags": ["Control Plane"], "summary": "Dev portal insights", "operationId": "getPortalInsights",
            "responses": json_ok("Counters per item")
        } },
        "/portal": { "get": {
            "tags": ["Docs"], "summary": "Dev portal", "operationId": "getPortalRedirect",
            "responses": { "307": { "description": "Redirect to /portal/" }, "404": { "description": "The portal is off" } }
        } },
        "/portal/": { "get": {
            "tags": ["Docs"], "summary": "Dev portal entry page", "operationId": "getPortalIndex",
            "responses": { "200": { "description": "HTML" }, "404": { "description": "The portal is off" } }
        } },
        "/portal/{path}": { "get": {
            "tags": ["Docs"], "summary": "Dev portal asset (SPA fallback to index.html)", "operationId": "getPortalAsset",
            "parameters": [{ "name": "path", "in": "path", "required": true, "schema": { "type": "string" } }],
            "responses": { "200": { "description": "Asset" }, "304": { "description": "Not modified" }, "404": { "description": "Unknown asset, or the portal is off" } }
        } },
        "/portal/api/catalog": { "get": {
            "tags": ["Docs"], "summary": "Dev portal catalogue", "operationId": "getPortalCatalog",
            "responses": {
                "200": { "description": "Published items", "content": { "application/json": { "schema": { "type": "object", "properties": {
                    "portal": { "type": "object" }, "kinds": { "type": "array", "items": { "type": "object" } },
                    "items": { "type": "array", "items": { "type": "object" } }, "whats_new": { "type": "array", "items": { "type": "string" } }
                } } } } },
                "304": { "description": "Not modified" }, "401": { "description": "portal.auth asks for a credential" }, "404": { "description": "The portal is off" }
            }
        } },
        "/portal/api/events": { "post": {
            "tags": ["Docs"], "summary": "Count a portal view or Try it call", "operationId": "postPortalEvent",
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["item", "event"], "properties": {
                "item": { "type": "string" }, "event": { "type": "string", "enum": ["view", "try"] }
            } } } } },
            "responses": { "204": { "description": "Counted" }, "400": { "description": "Invalid event" }, "404": { "description": "No such published item" } }
        } },
        "/portal/api/items/{id}/collections/{format}": { "get": {
            "tags": ["Docs"], "summary": "A published mock as a collection", "operationId": "getPortalCollection",
            "parameters": [id_param, { "name": "format", "in": "path", "required": true, "schema": { "type": "string", "enum": crate::mocks::collections::FORMATS.iter().map(|(f, _, _)| *f).collect::<Vec<_>>() } }],
            "responses": { "200": { "description": "The collection (attachment)" }, "404": { "description": "Not published, or an unknown format" } }
        } },
        "/portal/reference/{id}": { "get": {
            "tags": ["Docs"], "summary": "A published mock's API reference", "operationId": "getPortalReference",
            "parameters": [id_param],
            "responses": { "200": { "description": "HTML" }, "404": { "description": "Not published" } }
        } }
    })
}
