//! Request inspector: captures recent requests into a bounded ring buffer and
//! a live broadcast feed.
//!
//! - Middleware [`capture`] records method, URI, headers, client IP, body (up
//!   to [`MAX_CAPTURED_BODY`] bytes), status and latency of every request
//!   except control-plane paths (`/_rustybin/*`, `/ui/*`) and the landing page.
//! - Public mode: only requests carrying `X-Rustybin-Session` are captured, and
//!   the API only returns entries matching the caller's `?session=` value.
//! - API keys (`crate::keys`): entries are tagged with the key id; in shared
//!   mode every keyed request is captured, each key keeps at most its
//!   `inspector_entries` entries for `inspector_ttl_secs` (inside the global
//!   capacity), and a caller authenticated by a key only sees that key's
//!   entries. `X-Rustybin-Key` and the `rb_key` query value are always
//!   redacted (`rbk_...<last 4>`).
//! - Other modules (request bin, AI inspector, UI) use the public API on
//!   [`Inspector`]: [`Inspector::list`], [`Inspector::get`],
//!   [`Inspector::for_session`], [`Inspector::find_by_request_id`],
//!   [`Inspector::subscribe`].

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::catalog::{category, Endpoint, Example};
use crate::keys::KeyContext;
use crate::state::AppState;

/// Bytes of request body kept per captured entry.
pub const MAX_CAPTURED_BODY: usize = 64 * 1024;
/// Hard upper bound on the ring buffer, whatever the configuration says.
pub const MAX_CAPACITY: usize = 10_000;
/// Default and maximum page size of the list endpoint.
pub const DEFAULT_LIMIT: usize = 100;

/// One captured header.
#[derive(Clone, Debug, Serialize)]
pub struct CapturedHeader {
    pub name: String,
    pub value: String,
}

/// One captured request/response exchange.
#[derive(Clone, Debug, Serialize)]
pub struct CapturedRequest {
    /// Inspector entry id (UUID).
    pub id: String,
    /// `X-Request-Id` of the request (propagated or generated).
    pub request_id: Option<String>,
    /// RFC 3339 timestamp of arrival.
    pub timestamp: String,
    pub method: String,
    pub uri: String,
    pub path: String,
    pub query: Option<String>,
    pub version: String,
    pub headers: Vec<CapturedHeader>,
    pub client_ip: Option<String>,
    /// Value of `X-Rustybin-Session`, if sent.
    pub session: Option<String>,
    /// API key id of the request (shared mode, `RUSTYBIN_REQUIRE_KEY`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    /// Body as text when it is valid UTF-8.
    pub body: Option<String>,
    /// Body as base64 when it is not valid UTF-8.
    pub body_base64: Option<String>,
    /// Full body size in bytes (before truncation).
    pub body_size: usize,
    pub body_truncated: bool,
    /// Response status (headers time).
    pub status: u16,
    /// Time to response headers, in milliseconds.
    pub latency_ms: f64,
    /// Insertion order (set by [`Inspector::record`]).
    #[serde(skip)]
    pub seq: u64,
    /// When the entry expires (per-key TTL), if ever.
    #[serde(skip)]
    pub expires_at: Option<Instant>,
}

/// Per-key retention of inspector entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyRetention {
    /// Entries kept for the key (`None`: only the global capacity).
    pub entries: Option<usize>,
    /// Entry lifetime (`None`: until evicted).
    pub ttl: Option<Duration>,
}

/// The ring buffer plus the per-key index (sequence numbers per key id).
#[derive(Default)]
struct Store {
    ring: VecDeque<Arc<CapturedRequest>>,
    per_key: HashMap<String, VecDeque<u64>>,
    next_seq: u64,
    has_ttl: bool,
    last_purge: Option<Instant>,
}

impl Store {
    /// Drop the oldest entry (global capacity).
    fn pop_oldest(&mut self) {
        if let Some(old) = self.ring.pop_front() {
            self.unindex(&old);
        }
    }

    fn unindex(&mut self, entry: &CapturedRequest) {
        let Some(key) = &entry.key_id else {
            return;
        };
        if let Some(seqs) = self.per_key.get_mut(key) {
            if let Ok(pos) = seqs.binary_search(&entry.seq) {
                seqs.remove(pos);
            }
            if seqs.is_empty() {
                self.per_key.remove(key);
            }
        }
    }

    /// Remove one entry by sequence number (the ring is sorted by it).
    fn remove_seq(&mut self, seq: u64) {
        if let Ok(pos) = self.ring.binary_search_by_key(&seq, |e| e.seq) {
            if let Some(old) = self.ring.remove(pos) {
                self.unindex(&old);
            }
        }
    }

    /// Drop expired entries (at most once a second).
    fn purge(&mut self, now: Instant) {
        if !self.has_ttl
            || self
                .last_purge
                .is_some_and(|t| now.duration_since(t) < Duration::from_secs(1))
        {
            return;
        }
        self.last_purge = Some(now);
        let before = self.ring.len();
        self.ring.retain(|e| !expired(e, now));
        if self.ring.len() != before {
            self.reindex();
        }
    }

    fn reindex(&mut self) {
        self.per_key.clear();
        for e in &self.ring {
            if let Some(k) = &e.key_id {
                self.per_key.entry(k.clone()).or_default().push_back(e.seq);
            }
        }
    }
}

fn expired(entry: &CapturedRequest, now: Instant) -> bool {
    entry.expires_at.is_some_and(|at| at <= now)
}

struct Inner {
    capacity: usize,
    public_mode: bool,
    /// Shared mode: callers without a key are admins (no `?session=` needed).
    keyed: bool,
    entries: Mutex<Store>,
    tx: broadcast::Sender<Arc<CapturedRequest>>,
    /// Requests captured since start (including evicted ones).
    total: std::sync::atomic::AtomicU64,
}

/// Handle to the shared inspector (cheap to clone).
#[derive(Clone)]
pub struct Inspector {
    inner: Arc<Inner>,
}

/// Filters for [`Inspector::list`] (also the query string of the list endpoint).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct InspectorQuery {
    pub session: Option<String>,
    pub path_prefix: Option<String>,
    pub limit: Option<usize>,
    /// Only this API key's entries (set from the caller's key, never from
    /// the query string).
    #[serde(skip)]
    pub key_id: Option<String>,
}

impl InspectorQuery {
    fn matches(&self, entry: &CapturedRequest) -> bool {
        if let Some(k) = &self.key_id {
            if entry.key_id.as_deref() != Some(k.as_str()) {
                return false;
            }
        }
        if let Some(s) = &self.session {
            if entry.session.as_deref() != Some(s.as_str()) {
                return false;
            }
        }
        if let Some(prefix) = &self.path_prefix {
            if !entry.path.starts_with(prefix.as_str()) {
                return false;
            }
        }
        true
    }
}

impl Inspector {
    pub fn new(capacity: usize, public_mode: bool) -> Self {
        Self::with_keys(capacity, public_mode, false)
    }

    /// An inspector for shared mode when `keyed` (see the module docs).
    pub fn with_keys(capacity: usize, public_mode: bool, keyed: bool) -> Self {
        let capacity = capacity.clamp(1, MAX_CAPACITY);
        let (tx, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(Inner {
                capacity,
                public_mode,
                keyed,
                entries: Mutex::new(Store {
                    ring: VecDeque::with_capacity(capacity.min(1024)),
                    ..Store::default()
                }),
                tx,
                total: std::sync::atomic::AtomicU64::new(0),
            }),
        }
    }

    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }

    pub fn public_mode(&self) -> bool {
        self.inner.public_mode
    }

    /// Shared mode (entries are scoped by API key).
    pub fn keyed(&self) -> bool {
        self.inner.keyed
    }

    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.inner
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Store an entry (evicting the oldest when full) and publish it.
    pub fn record(&self, entry: CapturedRequest) -> Arc<CapturedRequest> {
        self.record_with(entry, KeyRetention::default())
    }

    /// Store an entry with a per-key retention (used when the entry has a
    /// `key_id`): at most `retention.entries` entries of that key are kept
    /// (its oldest is dropped first) and the entry expires after
    /// `retention.ttl`.
    pub fn record_with(
        &self,
        mut entry: CapturedRequest,
        retention: KeyRetention,
    ) -> Arc<CapturedRequest> {
        self.inner
            .total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let now = Instant::now();
        let entry = {
            let mut store = self.store();
            store.purge(now);
            entry.seq = store.next_seq;
            store.next_seq += 1;
            entry.expires_at = entry.key_id.as_ref().and(retention.ttl).map(|t| now + t);
            if entry.expires_at.is_some() {
                store.has_ttl = true;
            }
            if let (Some(key), Some(cap)) = (&entry.key_id, retention.entries) {
                let cap = cap.max(1);
                loop {
                    let oldest = match store.per_key.get(key) {
                        Some(seqs) if seqs.len() >= cap => seqs.front().copied(),
                        _ => None,
                    };
                    match oldest {
                        Some(seq) => store.remove_seq(seq),
                        None => break,
                    }
                }
            }
            while store.ring.len() >= self.inner.capacity {
                store.pop_oldest();
            }
            let entry = Arc::new(entry);
            if let Some(key) = &entry.key_id {
                store
                    .per_key
                    .entry(key.clone())
                    .or_default()
                    .push_back(entry.seq);
            }
            store.ring.push_back(entry.clone());
            entry
        };
        // No receivers is fine.
        let _ = self.inner.tx.send(entry.clone());
        entry
    }

    /// Entries matching `query`, newest first, at most `query.limit`
    /// (default [`DEFAULT_LIMIT`], capped at the capacity).
    pub fn list(&self, query: &InspectorQuery) -> Vec<Arc<CapturedRequest>> {
        self.list_at(query, Instant::now())
    }

    /// [`Inspector::list`] as seen at `now` (expired entries are hidden).
    pub fn list_at(&self, query: &InspectorQuery, now: Instant) -> Vec<Arc<CapturedRequest>> {
        let limit = query
            .limit
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, self.inner.capacity);
        self.store()
            .ring
            .iter()
            .rev()
            .filter(|e| !expired(e, now) && query.matches(e))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Number of entries held for one API key.
    pub fn key_len(&self, key_id: &str) -> usize {
        self.store().per_key.get(key_id).map_or(0, VecDeque::len)
    }

    /// Number of API keys with entries.
    pub fn key_count(&self) -> usize {
        self.store().per_key.len()
    }

    /// Entries of one session, newest first.
    pub fn for_session(&self, session: &str, limit: usize) -> Vec<Arc<CapturedRequest>> {
        self.list(&InspectorQuery {
            session: Some(session.to_string()),
            limit: Some(limit),
            ..InspectorQuery::default()
        })
    }

    /// Entry by inspector id.
    pub fn get(&self, id: &str) -> Option<Arc<CapturedRequest>> {
        let now = Instant::now();
        self.store()
            .ring
            .iter()
            .rev()
            .find(|e| e.id == id && !expired(e, now))
            .cloned()
    }

    /// Newest entry with this `X-Request-Id`.
    pub fn find_by_request_id(&self, request_id: &str) -> Option<Arc<CapturedRequest>> {
        let now = Instant::now();
        self.store()
            .ring
            .iter()
            .rev()
            .find(|e| e.request_id.as_deref() == Some(request_id) && !expired(e, now))
            .cloned()
    }

    /// Remove everything; returns the number of removed entries.
    pub fn clear(&self) -> usize {
        let mut store = self.store();
        let n = store.ring.len();
        store.ring.clear();
        store.per_key.clear();
        n
    }

    /// Remove one session's entries; returns the number removed.
    pub fn clear_session(&self, session: &str) -> usize {
        let mut store = self.store();
        let before = store.ring.len();
        store.ring.retain(|e| e.session.as_deref() != Some(session));
        let removed = before - store.ring.len();
        if removed > 0 {
            store.reindex();
        }
        removed
    }

    pub fn len(&self) -> usize {
        self.store().ring.len()
    }

    /// Requests captured since start, including evicted ones.
    pub fn total_recorded(&self) -> u64 {
        self.inner.total.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Live feed of new entries (lagging receivers skip entries).
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<CapturedRequest>> {
        self.inner.tx.subscribe()
    }
}

// ── Middleware ──────────────────────────────────────────────────────

/// Paths never captured: control plane, UI and the landing page.
pub fn is_excluded(path: &str) -> bool {
    crate::control::is_control_path(path) || path == "/" || path == "/favicon.ico"
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// Capture middleware (installed by `build_app`).
pub async fn capture(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if is_excluded(req.uri().path()) {
        return next.run(req).await;
    }
    let session = crate::session::session_header(req.headers());
    let key = req.extensions().get::<KeyContext>().cloned();
    if state.config.public_mode && session.is_none() && key.is_none() {
        return next.run(req).await;
    }

    let start = Instant::now();
    let timestamp = chrono::Utc::now().to_rfc3339();
    let (parts, body) = req.into_parts();

    // Buffer the body (bounded by the configured body limit) so it can be both
    // recorded and passed on.
    let limit = state.config.body_limit;
    let collected = http_body_util::Limited::new(body, limit).collect().await;
    let (bytes, early) = match collected {
        Ok(c) => (c.to_bytes(), None),
        Err(e) => {
            let resp = if e.is::<http_body_util::LengthLimitError>() {
                json_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &format!("request body exceeds the {limit} byte limit"),
                )
            } else {
                json_error(StatusCode::BAD_REQUEST, "failed to read request body")
            };
            (bytes::Bytes::new(), Some(resp))
        }
    };

    let client_ip = crate::session::client_ip(&parts.headers, &parts.extensions, &state.config)
        .map(|ip| ip.to_string());
    let request_id = parts
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let headers = parts
        .headers
        .iter()
        .map(|(k, v)| CapturedHeader {
            name: k.as_str().to_string(),
            value: crate::keys::redact_header(k.as_str(), v.as_bytes()),
        })
        .collect();
    let body_size = bytes.len();
    let kept = &bytes[..body_size.min(MAX_CAPTURED_BODY)];
    let (body_text, body_base64) = if kept.is_empty() {
        (None, None)
    } else {
        match std::str::from_utf8(kept) {
            Ok(s) => (Some(s.to_string()), None),
            Err(_) => (
                None,
                Some(base64::engine::general_purpose::STANDARD.encode(kept)),
            ),
        }
    };

    let mut entry = CapturedRequest {
        id: uuid::Uuid::new_v4().to_string(),
        request_id,
        timestamp,
        method: parts.method.to_string(),
        uri: crate::keys::redact_uri(&parts.uri),
        path: parts.uri.path().to_string(),
        query: parts
            .uri
            .query()
            .map(|q| crate::keys::redact_query(q).unwrap_or_else(|| q.to_string())),
        version: format!("{:?}", parts.version),
        headers,
        client_ip,
        session,
        key_id: key.as_ref().map(|k| k.key.id.to_string()),
        body: body_text,
        body_base64,
        body_size,
        body_truncated: body_size > MAX_CAPTURED_BODY,
        status: 0,
        latency_ms: 0.0,
        seq: 0,
        expires_at: None,
    };
    let retention = match &key {
        Some(k) if k.shared => KeyRetention {
            entries: k.key.inspector_entries,
            ttl: k.key.inspector_ttl,
        },
        _ => KeyRetention::default(),
    };

    let resp = match early {
        Some(resp) => resp,
        None => {
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
    };
    entry.status = resp.status().as_u16();
    entry.latency_ms = start.elapsed().as_secs_f64() * 1000.0;
    state.inspector.record_with(entry, retention);
    resp
}

// ── Control-plane endpoints ─────────────────────────────────────────

const SESSION_REQUIRED: &str =
    "this instance runs in public mode: pass ?session=<your X-Rustybin-Session value>";

/// The API key a control-plane caller is scoped to (shared mode).
type CallerKey = Option<axum::Extension<KeyContext>>;

fn caller_key(key: CallerKey) -> Option<String> {
    key.filter(|k| k.shared && k.control)
        .map(|axum::Extension(k)| k.key.id.to_string())
}

/// In public mode a valid `?session=` is mandatory, except in shared mode,
/// where a key caller only sees its key's entries and other callers are
/// admins.
#[allow(clippy::result_large_err)]
fn scoped_query(
    inspector: &Inspector,
    mut q: InspectorQuery,
    key: Option<String>,
) -> Result<InspectorQuery, Response> {
    if let Some(s) = &q.session {
        if !crate::session::is_valid_session(s) {
            return Err(json_error(StatusCode::BAD_REQUEST, "invalid session value"));
        }
    }
    if inspector.public_mode() && !inspector.keyed() && q.session.is_none() {
        return Err(json_error(StatusCode::BAD_REQUEST, SESSION_REQUIRED));
    }
    q.key_id = key;
    q.limit = Some(q.limit.unwrap_or(DEFAULT_LIMIT));
    Ok(q)
}

async fn list_handler(
    State(inspector): State<Inspector>,
    key: CallerKey,
    Query(q): Query<InspectorQuery>,
) -> Response {
    let q = match scoped_query(&inspector, q, caller_key(key)) {
        Ok(q) => q,
        Err(resp) => return resp,
    };
    let entries = inspector.list(&q);
    Json(json!({
        "count": entries.len(),
        "capacity": inspector.capacity(),
        "public_mode": inspector.public_mode(),
        "requests": entries,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct SessionParam {
    session: Option<String>,
}

async fn get_handler(
    State(inspector): State<Inspector>,
    key: CallerKey,
    Path(id): Path<String>,
    Query(p): Query<SessionParam>,
) -> Response {
    let key = caller_key(key);
    let entry = inspector.get(&id).filter(|e| match &key {
        // Shared mode: a key caller only sees its key's entries.
        Some(k) => e.key_id.as_deref() == Some(k.as_str()),
        // Shared mode admin.
        None if inspector.keyed() => true,
        // Public mode: the caller must prove the session the entry belongs to.
        None => !inspector.public_mode() || (p.session.is_some() && e.session == p.session),
    });
    match entry {
        Some(e) => Json(e.as_ref().clone()).into_response(),
        None => json_error(StatusCode::NOT_FOUND, "no such captured request"),
    }
}

async fn clear_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(p): Query<SessionParam>,
) -> Response {
    // Clearing one session's entries only needs the session id; clearing
    // everything is a global mutation and needs admin rights.
    if let Some(session) = p.session {
        if !crate::session::is_valid_session(&session) {
            return json_error(StatusCode::BAD_REQUEST, "invalid session value");
        }
        let removed = state.inspector.clear_session(&session);
        return Json(json!({ "cleared": removed, "session": session })).into_response();
    }
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let removed = state.inspector.clear();
    Json(json!({ "cleared": removed })).into_response()
}

async fn stream_handler(
    State(inspector): State<Inspector>,
    key: CallerKey,
    Query(q): Query<InspectorQuery>,
) -> Response {
    let q = match scoped_query(&inspector, q, caller_key(key)) {
        Ok(q) => q,
        Err(resp) => return resp,
    };
    let stream = BroadcastStream::new(inspector.subscribe()).filter_map(move |item| {
        let entry = item.ok()?;
        if !q.matches(&entry) || expired(&entry, Instant::now()) {
            return None;
        }
        let data = serde_json::to_string(entry.as_ref()).ok()?;
        Some(Ok::<Event, Infallible>(
            Event::default()
                .event("request")
                .id(entry.id.clone())
                .data(data),
        ))
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/_rustybin/requests",
            get(list_handler).delete(clear_handler),
        )
        .route("/_rustybin/requests/stream", get(stream_handler))
        .route("/_rustybin/requests/{id}", get(get_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/_rustybin/requests",
            &["GET", "DELETE"],
            category::CONTROL,
            "List captured requests (newest first); DELETE clears them",
        )
        .description(
            "Filters: ?session=, ?path_prefix=, ?limit=. Public mode requires ?session= and only \
             returns that session's requests. Shared mode: a caller authenticated with an API key \
             (X-Rustybin-Key or ?rb_key=) only sees that key's requests. DELETE with ?session= \
             clears one session; without it, clearing everything requires the admin token.",
        )
        .example(Example::get("List captured requests", "/_rustybin/requests?limit=20"))
        .example(Example::get(
            "List one session's requests",
            "/_rustybin/requests?session=demo&limit=20",
        ))
        .example(Example::delete(
            "Clear one session's requests",
            "/_rustybin/requests?session=demo",
        )),
        Endpoint::new(
            "/_rustybin/requests/stream",
            &["GET"],
            category::CONTROL,
            "Live feed of captured requests",
        )
        .description("Server-Sent Events, one `request` event per captured request. Same filters as the list.")
        .sse()
        .example(
            Example::get("Live request feed", "/_rustybin/requests/stream")
                .skip_check("never-ending SSE stream"),
        ),
        Endpoint::new(
            "/_rustybin/requests/{id}",
            &["GET"],
            category::CONTROL,
            "One captured request by id",
        )
        .example(
            Example::get(
                "Captured request by id",
                "/_rustybin/requests/00000000-0000-0000-0000-000000000000",
            )
            .expect_status(404),
        ),
    ]
}

pub fn openapi_paths() -> Value {
    let session = json!({ "name": "session", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Only entries captured with this X-Rustybin-Session value (required in public mode)" });
    let path_prefix = json!({ "name": "path_prefix", "in": "query", "required": false, "schema": { "type": "string" } });
    let limit = json!({ "name": "limit", "in": "query", "required": false, "schema": { "type": "integer", "default": DEFAULT_LIMIT } });
    json!({
        "/_rustybin/requests": {
            "get": {
                "tags": ["Control Plane"],
                "summary": "List captured requests",
                "operationId": "listCapturedRequests",
                "parameters": [session, path_prefix, limit],
                "responses": {
                    "200": { "description": "Captured requests, newest first", "content": { "application/json": { "schema": {
                        "type": "object",
                        "properties": {
                            "count": { "type": "integer" },
                            "capacity": { "type": "integer" },
                            "public_mode": { "type": "boolean" },
                            "requests": { "type": "array", "items": { "$ref": "#/components/schemas/CapturedRequest" } }
                        }
                    } } } },
                    "400": { "description": "Missing ?session= in public mode" }
                }
            },
            "delete": {
                "tags": ["Control Plane"],
                "summary": "Clear captured requests",
                "description": "With ?session= clears that session only; otherwise requires the admin token.",
                "operationId": "clearCapturedRequests",
                "parameters": [session],
                "responses": {
                    "200": { "description": "Number of cleared entries" },
                    "401": { "description": "Admin token required" },
                    "403": { "description": "Disabled in public mode without an admin token" }
                }
            }
        },
        "/_rustybin/requests/stream": {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Live captured-request feed (SSE)",
                "operationId": "streamCapturedRequests",
                "parameters": [session, path_prefix],
                "responses": { "200": { "description": "text/event-stream of `request` events", "content": { "text/event-stream": {} } } }
            }
        },
        "/_rustybin/requests/{id}": {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Get one captured request",
                "operationId": "getCapturedRequest",
                "parameters": [
                    { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } },
                    session
                ],
                "responses": {
                    "200": { "description": "Captured request", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/CapturedRequest" } } } },
                    "404": { "description": "Unknown id (or not visible to this session)" }
                }
            }
        }
    })
}

pub fn openapi_components() -> Value {
    json!({
        "schemas": {
            "CapturedRequest": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "request_id": { "type": "string", "nullable": true },
                    "timestamp": { "type": "string", "format": "date-time" },
                    "method": { "type": "string" },
                    "uri": { "type": "string" },
                    "path": { "type": "string" },
                    "query": { "type": "string", "nullable": true },
                    "version": { "type": "string" },
                    "headers": { "type": "array", "items": { "type": "object", "properties": { "name": { "type": "string" }, "value": { "type": "string" } } } },
                    "client_ip": { "type": "string", "nullable": true },
                    "session": { "type": "string", "nullable": true },
                    "key_id": { "type": "string", "description": "API key id (shared mode, RUSTYBIN_REQUIRE_KEY); absent otherwise" },
                    "body": { "type": "string", "nullable": true },
                    "body_base64": { "type": "string", "nullable": true },
                    "body_size": { "type": "integer" },
                    "body_truncated": { "type": "boolean" },
                    "status": { "type": "integer" },
                    "latency_ms": { "type": "number" }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_json, get_request, test_app_with, test_state, test_state_with};
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    fn entry(path: &str, session: Option<&str>) -> CapturedRequest {
        CapturedRequest {
            id: uuid::Uuid::new_v4().to_string(),
            request_id: None,
            timestamp: String::new(),
            method: "GET".into(),
            uri: path.into(),
            path: path.into(),
            query: None,
            version: "HTTP/1.1".into(),
            headers: vec![],
            client_ip: None,
            session: session.map(str::to_string),
            key_id: None,
            body: None,
            body_base64: None,
            body_size: 0,
            body_truncated: false,
            status: 200,
            latency_ms: 0.0,
            seq: 0,
            expires_at: None,
        }
    }

    #[test]
    fn ring_buffer_is_bounded_and_newest_first() {
        let i = Inspector::new(3, false);
        for n in 0..5 {
            i.record(entry(&format!("/p{n}"), None));
        }
        assert_eq!(i.len(), 3);
        let paths: Vec<_> = i
            .list(&InspectorQuery::default())
            .iter()
            .map(|e| e.path.clone())
            .collect();
        assert_eq!(paths, vec!["/p4", "/p3", "/p2"]);
    }

    #[test]
    fn filters_by_session_and_prefix() {
        let i = Inspector::new(10, false);
        i.record(entry("/echo", Some("a")));
        i.record(entry("/status/200", Some("b")));
        i.record(entry("/echo/x", Some("b")));
        assert_eq!(i.for_session("b", 10).len(), 2);
        let q = InspectorQuery {
            session: Some("b".into()),
            path_prefix: Some("/echo".into()),
            ..InspectorQuery::default()
        };
        assert_eq!(i.list(&q).len(), 1);
        assert_eq!(i.clear_session("b"), 2);
        assert_eq!(i.len(), 1);
    }

    #[tokio::test]
    async fn captures_requests_through_the_app() {
        let state = test_state();
        let app = test_app_with(state.clone());
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/echo?x=1")
                    .header("x-rustybin-session", "s1")
                    .body(Body::from("hello"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(get_request("/_rustybin/requests?session=s1"))
            .await
            .expect("response");
        let v = body_json(resp).await;
        assert_eq!(v["count"], 1);
        let first = &v["requests"][0];
        assert_eq!(first["path"], "/echo");
        assert_eq!(first["body"], "hello");
        assert_eq!(first["status"], 200);
        assert!(first["request_id"].is_string());

        // Control-plane requests are not captured.
        assert_eq!(state.inspector.len(), 1);

        let id = first["id"].as_str().expect("id").to_string();
        let resp = app
            .oneshot(get_request(&format!("/_rustybin/requests/{id}")))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn public_mode_scopes_to_session() {
        let mut config = crate::config::Config::for_tests();
        config.public_mode = true;
        let state = test_state_with(config);
        let app = test_app_with(state.clone());

        // Without a session header: not captured.
        let _ = app.clone().oneshot(get_request("/echo")).await;
        assert_eq!(state.inspector.len(), 0);

        let req = HttpRequest::builder()
            .uri("/echo")
            .header("x-rustybin-session", "mine")
            .body(Body::empty())
            .expect("request");
        let _ = app.clone().oneshot(req).await;
        assert_eq!(state.inspector.len(), 1);

        let resp = app
            .clone()
            .oneshot(get_request("/_rustybin/requests"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let resp = app
            .clone()
            .oneshot(get_request("/_rustybin/requests?session=other"))
            .await
            .expect("response");
        assert_eq!(body_json(resp).await["count"], 0);

        let id = state.inspector.for_session("mine", 1)[0].id.clone();
        let resp = app
            .clone()
            .oneshot(get_request(&format!("/_rustybin/requests/{id}")))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // Global clear is disabled in public mode without an admin token.
        let req = HttpRequest::builder()
            .method("DELETE")
            .uri("/_rustybin/requests")
            .body(Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn oversized_body_is_rejected_and_recorded() {
        let mut config = crate::config::Config::for_tests();
        config.body_limit = 8;
        let state = test_state_with(config);
        let app = test_app_with(state.clone());
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/echo")
            .body(Body::from("0123456789abcdef"))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            state.inspector.list(&InspectorQuery::default())[0].status,
            413
        );
    }
}
