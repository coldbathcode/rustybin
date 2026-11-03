//! The portal's routes.
//!
//! Public (`/portal*`, outside the control plane, exempt from the key layer
//! like `/ui`; `portal.auth` decides who reads the data, each item's access
//! who uses it):
//! - `GET /portal` (redirect), `GET /portal/` and `GET /portal/{*path}`: the
//!   embedded SPA (`ui-portal/`), `index.html` for extension-less paths.
//! - `GET /portal/api/catalog`: the published items ([`super::items`]).
//! - `POST /portal/api/events`: a view or a Try it call, counted.
//! - `GET /portal/api/items/{id}/collections/{format}`: a published mock as a
//!   collection, counted.
//! - `GET /portal/reference/{id}`: a published mock's spec in the full API
//!   reference viewer (the `/docs` viewer).
//! - `POST /portal/api/login`, `POST /portal/api/logout`, `GET
//!   /portal/api/session`: the portal sign-in ([`super::session`]).
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
use super::{Access, AuthMode, Cached, ItemId, Meta, PublishError};
use crate::catalog::{category, Endpoint, Example};
use crate::landing::html_escape;
use crate::mocks::service::{Links, ServiceError, Svc};
use crate::mocks::visits::Owner;
use crate::state::AppState;

include!(concat!(env!("OUT_DIR"), "/portal_assets.rs"));

pub(super) fn off() -> Response {
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
            "this portal needs an API key (or the portal sign-in)",
            json!({ "header": crate::keys::KEY_HEADER, "signin": super::session::enabled(&state.config) }),
        ),
        _ => (
            "this portal needs the portal sign-in",
            json!({ "signin": super::session::enabled(&state.config) }),
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

/// A signed-in caller without `portal.read` asks for what only readers get.
fn not_a_reader() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "this account does not read the portal's closed items",
            "reason": "permission_required",
            "permission": crate::rbac::Permission::PortalRead.name(),
        })),
    )
        .into_response()
}

/// A closed item's use (a collection, a Try it) by a caller who is not a
/// reader.
fn locked(state: &AppState, headers: &HeaderMap) -> Response {
    if super::session::reader(state, headers).is_some() {
        return not_a_reader();
    }
    let mut resp = (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": "this API is closed: sign in to the portal to use it",
            "reason": "signin_required",
            "locked": true,
            "signin": super::session::enabled(&state.config),
        })),
    )
        .into_response();
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"rustybin-portal\""),
    );
    resp
}

/// The portal is on and the caller holds what `portal.auth` asks for:
/// whether the caller reads closed items. `password` lets readers only;
/// `key` an API key or a reader; `open` everyone.
#[allow(clippy::result_large_err)]
fn gate(
    state: &AppState,
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
) -> Result<bool, Response> {
    if !super::active(state) {
        return Err(off());
    }
    let reader = super::session::can_read_closed(state, headers);
    match super::auth_mode(&state.settings) {
        AuthMode::Open => Ok(reader),
        AuthMode::Password if reader => Ok(true),
        AuthMode::Password => {
            if super::session::reader(state, headers).is_some() {
                Err(not_a_reader())
            } else {
                Err(unauthorized(state, AuthMode::Password))
            }
        }
        AuthMode::Key => {
            if reader || extensions.get::<crate::keys::KeyContext>().is_some() {
                return Ok(reader);
            }
            match state
                .keys
                .lookup(crate::keys::presented_key(headers, uri).as_deref())
            {
                crate::keys::Lookup::Found(_) => Ok(false),
                _ => Err(unauthorized(state, AuthMode::Key)),
            }
        }
    }
}

/// Is a published entry closed (its own access, or `portal.auth=password`)?
fn entry_closed(state: &AppState, e: &Entry) -> bool {
    super::closed(&state.settings, e.published.access)
}

fn ctx(
    state: &AppState,
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
    reader: bool,
) -> Ctx {
    let mut c = ctx_for(
        state,
        Links::from_request(headers, extensions, uri, &state.config),
    );
    c.reader = reader;
    c
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
        if super::session::enabled(&state.config) {
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
    let reader = match gate(&state, &headers, &extensions, &uri) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    state.portal.sweep(&state);
    let ctx = ctx(&state, &headers, &extensions, &uri, reader);
    let fingerprint = format!(
        "{}|{}",
        items::fingerprint(&state, &ctx),
        Utc::now().date_naive()
    );
    let entry = match state.portal.cached(&fingerprint) {
        Some(c) => c,
        None => {
            if !state.portal.allow_render() {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(axum::http::header::RETRY_AFTER, "10")],
                    Json(json!({ "error": "the catalogue is being rendered too often: try again shortly" })),
                )
                    .into_response();
            }
            let render_state = state.clone();
            let body = tokio::task::spawn_blocking(move || {
                serde_json::to_vec(&items::catalog_json(&render_state, &ctx)).unwrap_or_default()
            })
            .await
            .unwrap_or_default();
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
    // A reader's catalogue holds what others do not: never in a shared
    // cache, and caches keep the answers apart by credential.
    if reader || super::auth_mode(&state.settings) != AuthMode::Open {
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-cache"),
        );
    }
    resp.headers_mut().insert(
        header::VARY,
        HeaderValue::from_static("Cookie, Authorization, X-Rustybin-Admin-Token, X-Rustybin-Key"),
    );
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
    let reader = match gate(&state, &headers, &extensions, &uri) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
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
    let Some(entry) = public_entry(&state, &ev.item) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no such published item" })),
        )
            .into_response();
    };
    // Views are counted for everyone; a Try it of a closed item only for
    // readers (nobody else can make one).
    if matches!(event, Event::Try) && !reader && entry_closed(&state, &entry) {
        return locked(&state, &headers);
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
    let reader = match gate(&state, &headers, &extensions, &uri) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
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
    // Collections carry the credentials.
    if !reader && entry_closed(&state, &entry) {
        return locked(&state, &headers);
    }
    let ctx = ctx(&state, &headers, &extensions, &uri, reader);
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

/// The full API reference of a published mock (the `/docs` viewer). Read
/// by everyone in `open` and `key` mode, closed item or not (the spec it
/// shows is the mock's public one and holds no credential); with
/// `portal.auth=password` it needs a reader (the page load carries the
/// portal's cookie).
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
    if super::auth_mode(&state.settings) == AuthMode::Password
        && !super::session::can_read_closed(&state, &headers)
    {
        return unauthorized(&state, AuthMode::Password);
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
    let Some(c) = s.live.clone() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "the published mock is not deployed" })),
        )
            .into_response();
    };
    let (mut server, idp) = crate::mocks::handlers::served_bases(
        &c,
        &s.name,
        &headers,
        &extensions,
        &uri,
        &state.config,
    );
    if s.owner.is_visitor() {
        let ctx = ctx(&state, &headers, &extensions, &uri, false);
        server = format!("{}{}", ctx.mock_base(&s.owner, &s.name), c.path_prefix());
    }
    let title = entry
        .published
        .title
        .clone()
        .unwrap_or_else(|| c.spec.title.clone());
    // The spec inlined into a sandboxed page (pinned script, opaque origin).
    crate::openapi::reference_page(
        &title,
        &crate::mocks::handlers::served_spec(&c, &server, &idp, &s.name),
    )
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
        "access": p.access.as_str(),
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
    let mut v = json!({
        "id": c.id,
        "kind": c.kind,
        "title": c.title,
        "summary": c.summary,
        "state": c.state,
        "visitor": c.owner.as_ref().is_some_and(Owner::is_visitor),
        "published": entry.map(published_json),
        "hidden": entry.and_then(|e| items::hidden_reason(state, e)),
        "insights": entry.map(|_| insights.item_json(&c.id, today)),
    });
    // A mock: does a call without credentials get refused (what closing
    // needs), and is a closed one still so?
    if let Ok(ItemId::Mock(name)) = ItemId::parse(&c.id) {
        if let Some(s) = state.mocks.get(&name) {
            let secured = s
                .live
                .as_ref()
                .is_some_and(|l| super::mock_secured(&state.config, l));
            v["secured"] = json!(secured);
            if !secured && entry.is_some_and(|e| e.published.access == Access::Closed) {
                v["warning"] = json!("closed, but the mock's mocked security is off: its calls answer without credentials (turn it on, or open the item)");
            }
        }
    }
    v
}

/// Who reads closed items: enabled users holding `portal.read`, and those
/// of them whose account opens nothing but the portal (`null` without the
/// password sign-in).
fn readers_view(state: &AppState) -> Value {
    if !super::session::enabled(&state.config) {
        return Value::Null;
    }
    let mut readers = 0;
    let mut portal_only = 0;
    for u in state.users.list().iter().filter(|u| !u.disabled) {
        let Some(r) = state.users.resolve(&u.id) else {
            continue;
        };
        if r.permissions.has(crate::rbac::Permission::PortalRead) {
            readers += 1;
            if r.permissions.len() == 1 {
                portal_only += 1;
            }
        }
    }
    json!({ "users": readers, "portal_only": portal_only })
}

fn settings_view(state: &AppState) -> Value {
    json!({
        "enabled": super::active(state),
        "mode": super::mode(&state.settings),
        "auth": super::auth_mode(&state.settings).as_str(),
        "signin": super::session::enabled(&state.config),
        "readers": readers_view(state),
        "base_url": super::base_url_setting(&state.settings),
        "visitors": state.settings.bool("portal.visitors"),
        "url": "/portal/",
        "persisted": state.portal.persisted(),
        "branding": super::branding(&state.settings),
    })
}

/// The console's view of the portal for a caller: settings, every item the
/// caller may publish with its published state and insights, and the
/// published items whose mock is gone (so they can be removed). Also the
/// management tool `list_portal_items`.
pub fn control_json(state: &AppState, caller: &crate::mocks::selfservice::Caller) -> Value {
    state.portal.sweep(state);
    let entries = items::entries(state);
    let insights = state.portal.insights();
    let candidates = items::candidates(state, |s| caller.can_read(s));
    let mut list: Vec<Value> = candidates
        .iter()
        .map(|c| item_view(state, c, entries.iter().find(|e| e.key == c.id), &insights))
        .collect();
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
        list.push(item_view(state, &c, Some(e), &insights));
    }
    let published = list.iter().filter(|i| !i["published"].is_null()).count();
    let closed = list
        .iter()
        .filter(|i| i["published"]["access"] == "closed")
        .count();
    let shown = list
        .iter()
        .filter(|i| !i["published"].is_null() && i["hidden"].is_null())
        .count();
    let mut body = settings_view(state);
    body["items"] = json!(list);
    body["counts"] = json!({ "published": published, "shown": shown, "closed": closed, "max": super::MAX_ITEMS });
    body
}

async fn control_view(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let caller = crate::mocks::selfservice::caller(&state, &headers, &extensions);
    Json(control_json(&state, &caller)).into_response()
}

/// May the caller publish `id`? Manage rights on a mock (a visitor's own
/// mock only with `portal.visitors`), admin for everything else and for a
/// mock that no longer exists. Returns the mock's owner and the audit name.
fn authorize_item(
    state: &AppState,
    id: &ItemId,
    svc: &Svc,
    principal: &str,
) -> Result<(Option<Owner>, String), ServiceError> {
    let admin = |svc: &Svc| -> Result<(), ServiceError> {
        svc.need(crate::rbac::Permission::PortalPublish)
    };
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
            Ok((Some(snap.owner), principal.to_string()))
        }
        _ => {
            admin(svc)?;
            Ok((None, principal.to_string()))
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

/// Publish `id` with its metadata as `svc`'s caller (`principal` is the
/// audit name of a non-visitor): the item's view and whether it was new.
/// Shared by `PUT /_rustybin/portal/items/{id}` and the management tool
/// `publish_portal_item`.
pub fn publish_item(
    state: &AppState,
    svc: &Svc,
    principal: &str,
    raw_id: &str,
    meta: Meta,
) -> Result<(Value, bool), ServiceError> {
    let id = ItemId::parse(raw_id).map_err(ServiceError::bad_request)?;
    let key = id.as_string();
    if let ItemId::Mock(name) = &id {
        if state.mocks.get(name).is_none() {
            // Admin first (an unknown name says nothing to others), then 404.
            if !svc.caller.admin {
                svc.manageable(name)?;
            }
            return Err(ServiceError::not_found(format!("mock {name:?} not found")));
        }
    }
    let (owner, by) = authorize_item(state, &id, svc, principal)?;
    if file_defined(state, &key) {
        return Err(conflict_file(&key));
    }
    let meta = super::clean_meta(&meta).map_err(ServiceError::bad_request)?;
    let access = meta
        .access
        .or_else(|| state.portal.get(&key).map(|p| p.access))
        .unwrap_or_default();
    if access == Access::Closed {
        check_closable(state, &id)?;
    }
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
    let entries = items::entries(state);
    let entry = entries.iter().find(|e| e.key == key);
    let cand = items::candidate(state, &id).ok_or_else(|| ServiceError::not_found("gone"))?;
    let view = item_view(state, &cand, entry, &state.portal.insights());
    Ok((view, created))
}

/// May `id` be closed? A mock only with its mocked security on (a call
/// without the credentials is refused by the mock); `409` names the fix.
fn check_closable(state: &AppState, id: &ItemId) -> Result<(), ServiceError> {
    let ItemId::Mock(name) = id else {
        return Ok(());
    };
    let secured = state
        .mocks
        .get(name)
        .and_then(|s| s.live.clone())
        .is_some_and(|l| super::mock_secured(&state.config, &l));
    if secured {
        return Ok(());
    }
    Err(ServiceError::with_body(
        StatusCode::CONFLICT,
        json!({
            "error": format!("mock {name:?} has no mocked security, so a closed entry would still answer every call without credentials: turn its security on first (or send secure: true to turn it on and close it in one step)"),
            "reason": "security_off",
            "fix": { "secure": true },
        }),
    ))
}

/// Turn on the mocked security of mock `name` and deploy it, for `secure:
/// true` (the console's "Turn on security and close"): the mode the spec
/// declares, else `api_key` (a mock with a mode of its own keeps it).
/// Refused when the mock has a draft that is not deployed (deploying it
/// would put other changes live). A mock already secured is left alone.
pub async fn secure_mock(svc: &Svc, name: &str) -> Result<bool, ServiceError> {
    use crate::mocks::model::SecurityMode;
    let snap = svc.manageable(name)?;
    let Some(live) = snap.live.clone() else {
        return Err(ServiceError::new(
            StatusCode::CONFLICT,
            format!("mock {name:?} is not deployed"),
        ));
    };
    if super::mock_secured(&svc.state.config, &live) {
        return Ok(false);
    }
    if snap.draft.revision != live.revision {
        return Err(ServiceError::new(
            StatusCode::CONFLICT,
            format!("mock {name:?} has a draft that is not deployed (revision {}): deploy or discard it first, then turn the security on", snap.draft.revision),
        ));
    }
    let mut sec = live.settings.security.clone();
    sec.enabled = true;
    let declares = live.spec.operations.iter().any(|o| !o.security.is_empty());
    if declares {
        sec.mode = SecurityMode::Spec;
    } else if sec.mode == SecurityMode::Spec {
        sec.mode = SecurityMode::ApiKey;
    }
    if sec.api_key.keys.is_empty() {
        sec.api_key.keys = crate::mocks::model::ApiKeySettings::default().keys;
    }
    let body = crate::mocks::service::PatchBody {
        expected_revision: Some(snap.draft.revision),
        name: None,
        spec: None,
        strip_server_prefix: None,
        responses: None,
        security: Some(sec),
        operations: None,
        mcp: None,
        deploy: Some(true),
        force: None,
    };
    let owned = name.to_string();
    svc.clone()
        .compiling(move |svc| svc.update(&owned, body))
        .await?;
    tracing::info!(target: "rustybin::portal", mock = %name, "mocked security turned on to close the item");
    Ok(true)
}

/// [`publish_item`], after turning on a mock's security when `secure` asks
/// for it (only when the entry is to be closed).
pub async fn publish_secured(
    state: &AppState,
    svc: &Svc,
    principal: &str,
    raw_id: &str,
    meta: Meta,
    secure: bool,
) -> Result<(Value, bool, bool), ServiceError> {
    let id = ItemId::parse(raw_id).map_err(ServiceError::bad_request)?;
    let mut secured = false;
    if secure {
        let ItemId::Mock(name) = &id else {
            return Err(ServiceError::bad_request(
                "secure applies to a mock (mock:<name>) only",
            ));
        };
        if meta.access != Some(Access::Closed) {
            return Err(ServiceError::bad_request(
                "secure turns on a mock's security to close it: send it with access: closed",
            ));
        }
        // The caller may publish it before anything changes.
        authorize_item(state, &id, svc, principal)?;
        if file_defined(state, &id.as_string()) {
            return Err(conflict_file(&id.as_string()));
        }
        secured = secure_mock(svc, name).await?;
    }
    let (view, created) = publish_item(state, svc, principal, raw_id, meta)?;
    Ok((view, created, secured))
}

/// Remove `id` from the portal as `svc`'s caller.
pub fn unpublish_item(
    state: &AppState,
    svc: &Svc,
    principal: &str,
    raw_id: &str,
) -> Result<Value, ServiceError> {
    let id = ItemId::parse(raw_id).map_err(ServiceError::bad_request)?;
    let key = id.as_string();
    let (_, by) = authorize_item(state, &id, svc, principal)?;
    if file_defined(state, &key) {
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
    Ok(json!({ "id": key, "published": false }))
}

async fn publish(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ServiceError> {
    let svc = Svc::from_request(&state, &headers, &extensions, &uri);
    let principal = crate::settings::principal(&headers, &state.config);
    let (meta, secure) = if body.iter().all(u8::is_ascii_whitespace) {
        (Meta::default(), false)
    } else {
        let mut v: Value = serde_json::from_slice(&body)
            .map_err(|e| ServiceError::bad_request(format!("invalid request: {e}")))?;
        let secure = match v.as_object_mut().and_then(|m| m.remove("secure")) {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => b,
            Some(_) => return Err(ServiceError::bad_request("secure is true or false")),
        };
        let meta: Meta = serde_json::from_value(v)
            .map_err(|e| ServiceError::bad_request(format!("invalid request: {e}")))?;
        (meta, secure)
    };
    let (mut view, created, secured) =
        publish_secured(&state, &svc, &principal, &id, meta, secure).await?;
    if secured {
        view["secured_now"] = json!(true);
    }
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
    let svc = Svc::from_request(&state, &headers, &extensions, &uri);
    let principal = crate::settings::principal(&headers, &state.config);
    Ok(Json(unpublish_item(&state, &svc, &principal, &id)?).into_response())
}

/// The insights of the published items the caller may see (counts only).
pub fn insights_json(state: &AppState, caller: &crate::mocks::selfservice::Caller) -> Value {
    let today = Utc::now().date_naive();
    let insights = state.portal.insights();
    let list: Vec<Value> = items::entries(state)
        .iter()
        .filter(|e| {
            e.published
                .owner
                .as_ref()
                .is_none_or(|o| !o.is_visitor() || caller.admin || caller.owns(o))
        })
        .map(|e| {
            let title = items::candidate(state, &e.id)
                .map(|c| e.published.title.clone().unwrap_or(c.title))
                .unwrap_or_else(|| e.key.clone());
            let mut v = insights.item_json(&e.key, today);
            v["id"] = json!(e.key);
            v["title"] = json!(title);
            v
        })
        .collect();
    json!({
        "items": list,
        "days": super::insights::DAYS,
        "persisted": state.portal.persisted(),
        "note": "Counts only: no visitor identifiers are stored.",
    })
}

async fn insights_view(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let caller = crate::mocks::selfservice::caller(&state, &headers, &extensions);
    Json(insights_json(&state, &caller)).into_response()
}

/// The catalogue context for a request's links (the portal's base URL
/// setting, else the request's origin). Also the management resource
/// `rustybin://portal/catalog`.
pub fn ctx_for(state: &AppState, links: Links) -> Ctx {
    match super::base_url_setting(&state.settings) {
        Some(base) => Ctx {
            base,
            gateway: true,
            links,
            reader: false,
        },
        None => Ctx {
            base: links.base_url().to_string(),
            gateway: false,
            links,
            reader: false,
        },
    }
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
        .route(
            super::session::LOGIN_PATH,
            post(super::session::login_handler),
        )
        .route(
            super::session::LOGOUT_PATH,
            post(super::session::logout_handler),
        )
        .route(
            super::session::SESSION_PATH,
            get(super::session::session_handler),
        )
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
        .permission(crate::rbac::Permission::InstanceRead)
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
        .permission(crate::rbac::Permission::PortalPublish)
        .self_service()
        .description(
            "Ids: mock:<name>, mcp:<server>, llm, a2a:<agent>, tools, registry. A mock needs \
             manage rights (visitors only with portal.visitors), everything else admin. Body: \
             title, summary (Markdown), tags, contact, order, credentials (show a mock's demo \
             credentials), access (open, or closed: everyone reads it, only portal readers use \
             it). A mock closes only with its mocked security on (409 security_off otherwise); \
             secure: true turns it on and deploys it first (needs mocks.write).",
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
        .example(
            Example::new(
                "Close the sample mock, turning its security on",
                "PUT",
                "/_rustybin/portal/items/mock:petstore",
            )
            .json(r#"{"access": "closed", "secure": true}"#),
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
        .permission(crate::rbac::Permission::InstanceRead)
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
        Endpoint::new(
            super::session::LOGIN_PATH,
            &["POST"],
            category::DOCS,
            "Dev portal sign-in: a user name and password for the portal's session cookie",
        )
        .description(
            "The console sign-in's checks, throttle and answers; sets rustybin_portal (HttpOnly, \
             SameSite=Strict, Secure behind HTTPS, Path=/portal, 12 hours), accepted under /portal \
             only. A user holding portal.read then uses closed items. 404 without the password \
             sign-in or with the portal off.",
        )
        .example(
            Example::post("Sign in to the portal", super::session::LOGIN_PATH)
                .json(r#"{"username":"rustybin","password":"rustybin"}"#)
                .expect_status(404),
        ),
        Endpoint::new(
            super::session::LOGOUT_PATH,
            &["POST"],
            category::DOCS,
            "Dev portal sign-out: end the session and clear its cookie",
        )
        .example(Example::post(
            "Sign out of the portal",
            super::session::LOGOUT_PATH,
        )),
        Endpoint::new(
            super::session::SESSION_PATH,
            &["GET"],
            category::DOCS,
            "Who is signed in to the portal, and whether they read closed items",
        )
        .example(Example::get(
            "The portal session",
            super::session::SESSION_PATH,
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
        "credentials": { "type": "boolean", "description": "Show a mock's demo credentials (default true)" },
        "access": { "type": "string", "enum": ["open", "closed"], "description": "open (default): anyone uses it; closed: everyone reads it, only portal readers (portal.read) use it" },
        "secure": { "type": "boolean", "description": "With access closed on a mock without mocked security: turn it on and deploy it first (needs mocks.write)" }
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
                    "404": { "description": "Unknown mock" }, "409": { "description": "Defined in the config file, the portal is full, a mock closed without mocked security (security_off), or a draft not deployed (secure)" }
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
            "responses": { "204": { "description": "Counted" }, "400": { "description": "Invalid event" }, "401": { "description": "A Try it of a closed item, not signed in" }, "404": { "description": "No such published item" } }
        } },
        "/portal/api/items/{id}/collections/{format}": { "get": {
            "tags": ["Docs"], "summary": "A published mock as a collection", "operationId": "getPortalCollection",
            "parameters": [id_param, { "name": "format", "in": "path", "required": true, "schema": { "type": "string", "enum": crate::mocks::collections::FORMATS.iter().map(|(f, _, _)| *f).collect::<Vec<_>>() } }],
            "responses": { "200": { "description": "The collection (attachment)" }, "401": { "description": "A closed item, not signed in (collections carry the credentials)" }, "403": { "description": "Signed in without portal.read" }, "404": { "description": "Not published, or an unknown format" } }
        } },
        "/portal/reference/{id}": { "get": {
            "tags": ["Docs"], "summary": "A published mock's API reference", "operationId": "getPortalReference",
            "parameters": [id_param],
            "responses": { "200": { "description": "HTML" }, "401": { "description": "portal.auth=password, not signed in" }, "404": { "description": "Not published" } }
        } },
        "/portal/api/login": { "post": {
            "tags": ["Docs"], "summary": "Dev portal sign-in", "operationId": "portalLogin",
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["username", "password"], "properties": {
                "username": { "type": "string" }, "password": { "type": "string", "format": "password" }
            } } } } },
            "responses": {
                "200": { "description": "Signed in: Set-Cookie rustybin_portal", "content": { "application/json": { "schema": { "type": "object", "properties": {
                    "signed_in": { "type": "boolean" }, "user": { "type": "string" }, "display_name": { "type": "string" }, "reader": { "type": "boolean" }, "expires_in": { "type": "integer" }
                } } } } },
                "400": { "description": "Invalid body" }, "401": { "description": "Invalid user name or password" },
                "404": { "description": "The portal or its sign-in is off" }, "429": { "description": "Too many failed attempts (shared with the console sign-in)" }
            }
        } },
        "/portal/api/logout": { "post": {
            "tags": ["Docs"], "summary": "Dev portal sign-out", "operationId": "portalLogout",
            "responses": { "200": { "description": "Signed out, the cookie cleared" }, "404": { "description": "The portal is off" } }
        } },
        "/portal/api/session": { "get": {
            "tags": ["Docs"], "summary": "Dev portal session", "operationId": "portalSession",
            "responses": {
                "200": { "description": "Who is signed in", "content": { "application/json": { "schema": { "type": "object", "properties": {
                    "signed_in": { "type": "boolean" }, "via": { "type": "string", "enum": ["cookie", "bearer"] }, "user": { "type": "string" },
                    "display_name": { "type": "string" }, "reader": { "type": "boolean" }, "signin": { "type": "boolean" }
                } } } } },
                "404": { "description": "The portal is off" }
            }
        } }
    })
}
