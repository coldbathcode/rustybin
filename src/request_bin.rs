//! Request bin: create a bin, point a client (or a gateway's webhook / log
//! plugin) at it, then inspect what arrived.
//!
//! - `POST /bin` creates a bin (optional JSON body configures the response
//!   the bin returns: status, headers, body, delay) and returns its URLs.
//! - `ANY /bin/{id}` and `ANY /bin/{id}/{*path}` capture the request (method,
//!   path, query, headers, body up to [`MAX_CAPTURED_BODY`] bytes, client ip,
//!   timestamp) and answer with the configured response. `DELETE /bin/{id}`
//!   deletes the bin instead (send DELETE to a sub path to capture it).
//! - `GET /bin/{id}/requests`, `GET /bin/{id}/requests/{n}` and
//!   `GET /bin/{id}/requests/stream` (SSE) inspect it. These routes are
//!   static segments, so axum matches them before the capture wildcard and
//!   they are never captured themselves.
//!
//! Everything is bounded (see [`BinLimits`]): number of bins, bins per
//! session in public mode, a TTL, a ring of requests per bin and a byte
//! budget per bin.
//!
//! The UI can share the store: build one [`RequestBins`] and mount it with
//! [`router_with_store`], then use [`RequestBins::list_bins`],
//! [`RequestBins::entries`] and [`RequestBins::subscribe`].

use axum::body::Bytes;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use base64::Engine;
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::session::{ClientIp, Session};
use crate::state::AppState;

/// Bytes of request body kept per captured request.
pub const MAX_CAPTURED_BODY: usize = 64 * 1024;
/// Maximum size of a configured response body.
pub const MAX_RESPONSE_BODY: usize = 16 * 1024;
/// Maximum number of configured response headers.
pub const MAX_RESPONSE_HEADERS: usize = 20;
/// Default page size of the requests list.
pub const DEFAULT_LIST_LIMIT: usize = 50;

/// Capacity limits of the store (lower in public mode).
#[derive(Clone, Copy, Debug, Serialize)]
pub struct BinLimits {
    /// Bins kept at once (instance wide).
    pub max_bins: usize,
    /// Bins one session (X-Rustybin-Session, else client IP) may own.
    pub max_bins_per_session: usize,
    /// Lifetime of a bin in seconds.
    pub ttl_secs: u64,
    /// Requests kept per bin (ring buffer, oldest evicted).
    pub max_requests: usize,
    /// Stored bytes (bodies + headers) per bin; oldest requests are evicted beyond it.
    pub max_bytes_per_bin: usize,
    /// Bytes of each request body kept.
    pub max_captured_body: usize,
    /// Maximum configured response delay.
    pub max_delay_ms: u64,
}

impl BinLimits {
    pub fn for_config(config: &Config) -> Self {
        if config.public_mode {
            Self {
                max_bins: 100,
                max_bins_per_session: 10,
                ttl_secs: 3600,
                max_requests: 50,
                max_bytes_per_bin: 256 * 1024,
                max_captured_body: MAX_CAPTURED_BODY,
                max_delay_ms: config.max_delay_ms(),
            }
        } else {
            Self {
                max_bins: 200,
                max_bins_per_session: 200,
                ttl_secs: 24 * 3600,
                max_requests: 100,
                max_bytes_per_bin: 1024 * 1024,
                max_captured_body: MAX_CAPTURED_BODY,
                max_delay_ms: config.max_delay_ms(),
            }
        }
    }
}

/// One header (captured or configured).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BinHeader {
    pub name: String,
    pub value: String,
}

/// The response a bin returns for every captured request.
#[derive(Clone, Debug, Serialize)]
pub struct BinResponse {
    pub status: u16,
    pub headers: Vec<BinHeader>,
    /// Custom body; `None` returns the default JSON acknowledgement.
    pub body: Option<String>,
    pub delay_ms: u64,
}

impl Default for BinResponse {
    fn default() -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: None,
            delay_ms: 0,
        }
    }
}

/// One captured request.
#[derive(Clone, Debug, Serialize)]
pub struct BinRequest {
    /// Sequence number in the bin, starting at 1 (`/bin/{id}/requests/{n}`).
    pub seq: u64,
    /// Unique id (UUID).
    pub id: String,
    pub bin_id: String,
    /// RFC 3339 arrival time.
    pub timestamp: String,
    pub method: String,
    /// Path below the bin (`/` for the bin root).
    pub path: String,
    /// Full request path.
    pub full_path: String,
    pub query: Option<String>,
    pub headers: Vec<BinHeader>,
    pub content_type: Option<String>,
    /// Body as text when valid UTF-8.
    pub body: Option<String>,
    /// Body as base64 otherwise.
    pub body_base64: Option<String>,
    /// Full body size in bytes (before truncation).
    pub body_size: usize,
    pub body_truncated: bool,
    pub client_ip: Option<String>,
}

impl BinRequest {
    fn stored_bytes(&self) -> usize {
        let headers: usize = self
            .headers
            .iter()
            .map(|h| h.name.len() + h.value.len())
            .sum();
        headers
            + self.body.as_ref().map_or(0, String::len)
            + self.body_base64.as_ref().map_or(0, String::len)
            + self.full_path.len()
            + self.query.as_ref().map_or(0, String::len)
    }
}

/// Public description of a bin.
#[derive(Clone, Debug, Serialize)]
pub struct BinSummary {
    pub id: String,
    pub created_at: String,
    pub expires_at: String,
    /// Requests received over the bin's lifetime.
    pub total_requests: u64,
    /// Requests currently stored.
    pub stored_requests: usize,
    pub response: BinResponse,
}

/// Why a bin could not be created.
#[derive(Debug, PartialEq, Eq)]
pub enum CreateError {
    /// The instance is full (public mode does not evict other sessions' bins).
    StoreFull,
    /// This session owns too many bins.
    SessionLimit,
}

struct Bin {
    id: String,
    owner: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    deadline: Instant,
    response: BinResponse,
    requests: VecDeque<Arc<BinRequest>>,
    stored_bytes: usize,
    next_seq: u64,
    tx: broadcast::Sender<Arc<BinRequest>>,
}

impl Bin {
    fn summary(&self) -> BinSummary {
        BinSummary {
            id: self.id.clone(),
            created_at: self.created_at.to_rfc3339(),
            expires_at: self.expires_at.to_rfc3339(),
            total_requests: self.next_seq.saturating_sub(1),
            stored_requests: self.requests.len(),
            response: self.response.clone(),
        }
    }
}

/// Shared, bounded request bin store (cheap to clone).
#[derive(Clone)]
pub struct RequestBins {
    limits: BinLimits,
    evict_when_full: bool,
    bins: Arc<Mutex<HashMap<String, Bin>>>,
}

impl RequestBins {
    pub fn new(config: &Config) -> Self {
        Self::with_limits(BinLimits::for_config(config), !config.public_mode)
    }

    /// Custom limits. `evict_when_full`: drop the oldest bin instead of
    /// refusing a new one when the store is full.
    pub fn with_limits(limits: BinLimits, evict_when_full: bool) -> Self {
        Self {
            limits,
            evict_when_full,
            bins: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn limits(&self) -> BinLimits {
        self.limits
    }

    /// Lock and drop expired bins.
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Bin>> {
        let mut bins = self.bins.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        bins.retain(|_, b| b.deadline > now);
        bins
    }

    /// Create a bin owned by `owner` (a session key).
    pub fn create(&self, owner: &str, response: BinResponse) -> Result<BinSummary, CreateError> {
        let mut bins = self.lock();
        let owned = bins.values().filter(|b| b.owner == owner).count();
        if owned >= self.limits.max_bins_per_session {
            return Err(CreateError::SessionLimit);
        }
        while bins.len() >= self.limits.max_bins {
            if !self.evict_when_full {
                return Err(CreateError::StoreFull);
            }
            let oldest = bins
                .values()
                .min_by_key(|b| b.created_at)
                .map(|b| b.id.clone());
            match oldest {
                Some(id) => {
                    bins.remove(&id);
                }
                None => break,
            }
        }
        let id = new_bin_id();
        let ttl = Duration::from_secs(self.limits.ttl_secs);
        let created_at = Utc::now();
        let expires_at = created_at
            + chrono::TimeDelta::try_seconds(self.limits.ttl_secs as i64).unwrap_or_default();
        let (tx, _) = broadcast::channel(64);
        let bin = Bin {
            id: id.clone(),
            owner: owner.to_string(),
            created_at,
            expires_at,
            deadline: Instant::now() + ttl,
            response,
            requests: VecDeque::new(),
            stored_bytes: 0,
            next_seq: 1,
            tx,
        };
        let summary = bin.summary();
        bins.insert(id, bin);
        Ok(summary)
    }

    /// Summary of one bin.
    pub fn get(&self, id: &str) -> Option<BinSummary> {
        self.lock().get(id).map(Bin::summary)
    }

    /// The configured response of a bin.
    pub fn response_for(&self, id: &str) -> Option<BinResponse> {
        self.lock().get(id).map(|b| b.response.clone())
    }

    /// Bins, newest first; `owner` filters by session key.
    pub fn list_bins(&self, owner: Option<&str>) -> Vec<BinSummary> {
        let bins = self.lock();
        let mut list: Vec<&Bin> = bins
            .values()
            .filter(|b| owner.is_none_or(|o| b.owner == o))
            .collect();
        list.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        list.into_iter().map(Bin::summary).collect()
    }

    /// Store a captured request (the `seq` and `bin_id` fields are filled in)
    /// and publish it to subscribers. `None` when the bin does not exist.
    pub fn record(&self, id: &str, mut request: BinRequest) -> Option<Arc<BinRequest>> {
        let mut bins = self.lock();
        let bin = bins.get_mut(id)?;
        request.seq = bin.next_seq;
        request.bin_id = bin.id.clone();
        bin.next_seq += 1;
        let request = Arc::new(request);
        let size = request.stored_bytes();
        while !bin.requests.is_empty()
            && (bin.requests.len() >= self.limits.max_requests
                || bin.stored_bytes + size > self.limits.max_bytes_per_bin)
        {
            if let Some(old) = bin.requests.pop_front() {
                bin.stored_bytes = bin.stored_bytes.saturating_sub(old.stored_bytes());
            }
        }
        bin.stored_bytes += size;
        bin.requests.push_back(request.clone());
        let _ = bin.tx.send(request.clone());
        Some(request)
    }

    /// Stored requests of a bin, newest first, at most `limit`.
    pub fn entries(&self, id: &str, limit: usize) -> Option<Vec<Arc<BinRequest>>> {
        let bins = self.lock();
        let bin = bins.get(id)?;
        Some(bin.requests.iter().rev().take(limit).cloned().collect())
    }

    /// One stored request by sequence number.
    pub fn entry(&self, id: &str, seq: u64) -> Option<Arc<BinRequest>> {
        let bins = self.lock();
        bins.get(id)?
            .requests
            .iter()
            .find(|r| r.seq == seq)
            .cloned()
    }

    /// Live feed of a bin's new requests, plus the instant the bin expires
    /// (streams should end then). The feed closes when the bin is deleted.
    pub fn subscribe(&self, id: &str) -> Option<(broadcast::Receiver<Arc<BinRequest>>, Instant)> {
        let bins = self.lock();
        let bin = bins.get(id)?;
        Some((bin.tx.subscribe(), bin.deadline))
    }

    /// Delete a bin; false when it did not exist.
    pub fn delete(&self, id: &str) -> bool {
        self.lock().remove(id).is_some()
    }

    /// Number of live bins.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn new_bin_id() -> String {
    let mut bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── Handlers ────────────────────────────────────────────────────────

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn bin_not_found(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": "bin not found",
            "bin": id,
            "hint": "create a bin with POST /bin (bins expire after their TTL)",
        })),
    )
        .into_response()
}

/// `scheme://host` of the request, for absolute URLs in responses. The Host
/// header is validated (only host and port characters) before use.
fn base_url(headers: &HeaderMap, config: &Config) -> Option<String> {
    let host = headers.get(header::HOST)?.to_str().ok()?;
    if host.is_empty()
        || host.len() > 255
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
    {
        return None;
    }
    let proto = if config.trust_forward {
        headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .filter(|p| *p == "https" || *p == "http")
            .unwrap_or("http")
    } else {
        "http"
    };
    Some(format!("{proto}://{host}"))
}

fn bin_links(id: &str, base: Option<&str>) -> Value {
    let path = format!("/bin/{id}");
    let abs = |p: &str| base.map(|b| format!("{b}{p}"));
    json!({
        "path": path,
        "url": abs(&path),
        "requests_path": format!("{path}/requests"),
        "requests_url": abs(&format!("{path}/requests")),
        "stream_path": format!("{path}/requests/stream"),
        "stream_url": abs(&format!("{path}/requests/stream")),
    })
}

/// Body of `POST /bin` (all fields optional).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBin {
    status: Option<u16>,
    headers: Option<HashMap<String, String>>,
    /// A string is returned verbatim; any other JSON value is serialised.
    body: Option<Value>,
    delay_ms: Option<u64>,
}

/// Validate a create request into a response definition.
fn build_response(req: CreateBin, limits: &BinLimits) -> Result<BinResponse, String> {
    let status = req.status.unwrap_or(200);
    if !(200..=599).contains(&status) {
        return Err("status must be between 200 and 599".into());
    }
    let mut headers = Vec::new();
    let mut has_content_type = false;
    if let Some(map) = req.headers {
        if map.len() > MAX_RESPONSE_HEADERS {
            return Err(format!("at most {MAX_RESPONSE_HEADERS} headers"));
        }
        let mut sorted: Vec<_> = map.into_iter().collect();
        sorted.sort();
        for (name, value) in sorted {
            let parsed = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| format!("invalid header name {name:?}"))?;
            if HeaderValue::from_str(&value).is_err() {
                return Err(format!("invalid value for header {name:?}"));
            }
            if matches!(
                parsed.as_str(),
                "content-length" | "transfer-encoding" | "connection" | "upgrade" | "trailer"
            ) {
                return Err(format!("header {name:?} cannot be configured"));
            }
            has_content_type |= parsed == header::CONTENT_TYPE;
            headers.push(BinHeader {
                name: parsed.as_str().to_string(),
                value,
            });
        }
    }
    let body = match req.body {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s),
        Some(other) => {
            if !has_content_type {
                headers.push(BinHeader {
                    name: "content-type".into(),
                    value: "application/json".into(),
                });
            }
            Some(other.to_string())
        }
    };
    if body.as_ref().is_some_and(|b| b.len() > MAX_RESPONSE_BODY) {
        return Err(format!("body exceeds {MAX_RESPONSE_BODY} bytes"));
    }
    let delay_ms = req.delay_ms.unwrap_or(0);
    if delay_ms > limits.max_delay_ms {
        return Err(format!("delay_ms must be at most {}", limits.max_delay_ms));
    }
    Ok(BinResponse {
        status,
        headers,
        body,
        delay_ms,
    })
}

async fn create_handler(
    Extension(store): Extension<RequestBins>,
    State(config): State<Arc<Config>>,
    Session(owner): Session,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let req: CreateBin = if body.iter().all(u8::is_ascii_whitespace) {
        CreateBin::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("invalid JSON: {e}")),
        }
    };
    let response = match build_response(req, &store.limits()) {
        Ok(r) => r,
        Err(msg) => return json_error(StatusCode::BAD_REQUEST, &msg),
    };
    let summary = match store.create(&owner, response) {
        Ok(s) => s,
        Err(CreateError::SessionLimit) => {
            return json_error(
                StatusCode::TOO_MANY_REQUESTS,
                "this session owns too many bins: delete one (DELETE /bin/{id}) or wait for it to expire",
            )
        }
        Err(CreateError::StoreFull) => {
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "the request bin store is full, try again later",
            )
        }
    };
    let base = base_url(&headers, &config);
    let mut out = json!({
        "id": summary.id,
        "created_at": summary.created_at,
        "expires_at": summary.expires_at,
        "response": summary.response,
        "limits": store.limits(),
    });
    if let (Some(obj), Value::Object(links)) =
        (out.as_object_mut(), bin_links(&summary.id, base.as_deref()))
    {
        obj.extend(links);
    }
    (StatusCode::CREATED, Json(out)).into_response()
}

async fn list_bins_handler(
    Extension(store): Extension<RequestBins>,
    Session(owner): Session,
) -> Response {
    let bins = store.list_bins(Some(&owner));
    Json(json!({
        "count": bins.len(),
        "bins": bins,
        "limits": store.limits(),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct BinPath {
    id: String,
}

#[derive(Deserialize)]
struct BinSubPath {
    id: String,
    path: String,
}

async fn capture_root(
    Extension(store): Extension<RequestBins>,
    Path(BinPath { id }): Path<BinPath>,
    ClientIp(ip): ClientIp,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    capture(store, id, "/".to_string(), ip, method, uri, headers, body).await
}

async fn capture_sub(
    Extension(store): Extension<RequestBins>,
    Path(BinSubPath { id, path }): Path<BinSubPath>,
    ClientIp(ip): ClientIp,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    capture(
        store,
        id,
        format!("/{path}"),
        ip,
        method,
        uri,
        headers,
        body,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn capture(
    store: RequestBins,
    id: String,
    sub_path: String,
    ip: Option<std::net::IpAddr>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(response) = store.response_for(&id) else {
        return bin_not_found(&id);
    };
    let max_body = store.limits().max_captured_body;
    let kept = &body[..body.len().min(max_body)];
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
    let entry = BinRequest {
        seq: 0,
        id: uuid::Uuid::new_v4().to_string(),
        bin_id: String::new(),
        timestamp: Utc::now().to_rfc3339(),
        method: method.to_string(),
        path: sub_path,
        full_path: uri.path().to_string(),
        query: uri.query().map(str::to_string),
        headers: headers
            .iter()
            .map(|(k, v)| BinHeader {
                name: k.as_str().to_string(),
                value: String::from_utf8_lossy(v.as_bytes()).into_owned(),
            })
            .collect(),
        content_type: headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
        body: body_text,
        body_base64,
        body_size: body.len(),
        body_truncated: body.len() > max_body,
        client_ip: ip.map(|i| i.to_string()),
    };
    let Some(recorded) = store.record(&id, entry) else {
        return bin_not_found(&id);
    };
    if response.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(response.delay_ms)).await;
    }
    render_response(&response, &recorded)
}

fn render_response(response: &BinResponse, recorded: &BinRequest) -> Response {
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::OK);
    let mut resp = match &response.body {
        Some(body) => {
            let mut r = (status, body.clone()).into_response();
            let ct = if serde_json::from_str::<Value>(body).is_ok() {
                "application/json"
            } else {
                "text/plain; charset=utf-8"
            };
            r.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
            r
        }
        None => (
            status,
            Json(json!({
                "ok": true,
                "bin": recorded.bin_id,
                "seq": recorded.seq,
                "id": recorded.id,
            })),
        )
            .into_response(),
    };
    for h in &response.headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(h.name.as_bytes()),
            HeaderValue::from_str(&h.value),
        ) {
            resp.headers_mut().insert(name, value);
        }
    }
    if let Ok(v) = HeaderValue::from_str(&recorded.seq.to_string()) {
        resp.headers_mut()
            .insert(HeaderName::from_static("x-rustybin-bin-seq"), v);
    }
    resp
}

async fn delete_handler(
    Extension(store): Extension<RequestBins>,
    Path(BinPath { id }): Path<BinPath>,
) -> Response {
    if store.delete(&id) {
        Json(json!({ "deleted": id })).into_response()
    } else {
        bin_not_found(&id)
    }
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<usize>,
}

async fn requests_handler(
    Extension(store): Extension<RequestBins>,
    State(config): State<Arc<Config>>,
    Path(BinPath { id }): Path<BinPath>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    let limit = q
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, store.limits().max_requests);
    let (Some(summary), Some(entries)) = (store.get(&id), store.entries(&id, limit)) else {
        return bin_not_found(&id);
    };
    let base = base_url(&headers, &config);
    Json(json!({
        "bin": summary,
        "links": bin_links(&id, base.as_deref()),
        "count": entries.len(),
        "requests": entries,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct EntryPath {
    id: String,
    n: String,
}

async fn entry_handler(
    Extension(store): Extension<RequestBins>,
    Path(EntryPath { id, n }): Path<EntryPath>,
) -> Response {
    if store.get(&id).is_none() {
        return bin_not_found(&id);
    }
    let Ok(seq) = n.parse::<u64>() else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "request number must be a positive integer",
        );
    };
    match store.entry(&id, seq) {
        Some(e) => Json(e.as_ref().clone()).into_response(),
        None => json_error(
            StatusCode::NOT_FOUND,
            "no such request in this bin (never received or already evicted)",
        ),
    }
}

async fn stream_handler(
    Extension(store): Extension<RequestBins>,
    Path(BinPath { id }): Path<BinPath>,
    headers: HeaderMap,
) -> Response {
    let Some((mut rx, deadline)) = store.subscribe(&id) else {
        return bin_not_found(&id);
    };
    // Replay stored requests newer than Last-Event-ID (the subscription is
    // taken first, so nothing is missed in between; duplicates are skipped).
    let last_seen: Option<u64> = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok());
    let mut backlog: Vec<Arc<BinRequest>> = match last_seen {
        Some(last) => store
            .entries(&id, usize::MAX)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.seq > last)
            .collect(),
        None => Vec::new(),
    };
    backlog.reverse();
    let stream = async_stream::stream! {
        let mut sent = last_seen.unwrap_or(0);
        for entry in backlog {
            sent = entry.seq;
            yield Ok::<Event, Infallible>(request_event(&entry));
        }
        let end = tokio::time::Instant::from_std(deadline);
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(end) => break,
                item = rx.recv() => match item {
                    Ok(entry) => {
                        if entry.seq > sent {
                            sent = entry.seq;
                            yield Ok(request_event(&entry));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            }
        }
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn request_event(entry: &BinRequest) -> Event {
    Event::default()
        .event("request")
        .id(entry.seq.to_string())
        .data(serde_json::to_string(entry).unwrap_or_default())
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

/// Router with a fresh store sized for the configuration.
pub fn router(state: &AppState) -> Router<AppState> {
    router_with_store(RequestBins::new(&state.config))
}

/// Router backed by an existing store (lets the UI share it).
pub fn router_with_store(store: RequestBins) -> Router<AppState> {
    Router::new()
        .route("/bin", get(list_bins_handler).post(create_handler))
        .route("/bin/{id}", any(capture_root).delete(delete_handler))
        .route("/bin/{id}/requests", get(requests_handler))
        .route("/bin/{id}/requests/stream", get(stream_handler))
        .route("/bin/{id}/requests/{n}", get(entry_handler))
        .route("/bin/{id}/{*path}", any(capture_sub))
        .layer(Extension(store))
}

const MISSING: &str = "/bin/0123456789abcdef01234567";

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/bin",
            &["GET", "POST"],
            category::REQUEST_BIN,
            "Create a request bin (POST) or list your bins (GET)",
        )
        .description(
            "POST with an optional JSON body {status, headers: {name: value}, body (string or JSON), \
             delay_ms} configures the response the bin returns; the reply has the bin id and its \
             capture, list and stream URLs. Limits: 200 bins, 100 requests and 1 MiB per bin, 24 h TTL \
             (public mode: 100 bins, 10 per session, 50 requests and 256 KiB per bin, 1 h TTL). Bodies \
             are captured up to 64 KiB. GET lists the bins owned by the caller's session \
             (X-Rustybin-Session, else client IP).",
        )
        .example(Example::post("Create a bin", "/bin"))
        .example(
            Example::post("Create a bin answering 202 after 100 ms", "/bin").json(
                r#"{"status":202,"headers":{"X-Bin":"demo"},"body":{"accepted":true},"delay_ms":100}"#,
            ),
        )
        .example(Example::get("List my bins", "/bin")),
        Endpoint::new(
            "/bin/{id}",
            &["ANY"],
            category::REQUEST_BIN,
            "Capture a request into a bin (DELETE deletes the bin)",
        )
        .description(
            "Any method except DELETE is captured and answered with the bin's configured response \
             (default: 200 JSON acknowledgement with the sequence number, also in X-Rustybin-Bin-Seq). \
             DELETE removes the bin; send DELETE to a sub path to capture it instead. Unknown bins are 404.",
        )
        .example(
            Example::post("Capture into a bin (create one first)", MISSING)
                .json(r#"{"event":"order.created"}"#)
                .expect_status(404),
        )
        .example(Example::delete("Delete a bin", MISSING).expect_status(404)),
        Endpoint::new(
            "/bin/{id}/{*path}",
            &["ANY"],
            category::REQUEST_BIN,
            "Capture a request sent to any sub path of a bin",
        )
        .example(
            Example::post(
                "Capture a webhook sub path",
                "/bin/0123456789abcdef01234567/webhooks/orders",
            )
            .json(r#"{"event":"order.created"}"#)
            .expect_status(404),
        ),
        Endpoint::new(
            "/bin/{id}/requests",
            &["GET"],
            category::REQUEST_BIN,
            "List the requests captured by a bin (newest first)",
        )
        .description("?limit= caps the page (default 50). Inspection routes are never captured.")
        .example(
            Example::get(
                "List captured requests",
                "/bin/0123456789abcdef01234567/requests",
            )
            .expect_status(404),
        ),
        Endpoint::new(
            "/bin/{id}/requests/stream",
            &["GET"],
            category::REQUEST_BIN,
            "Live feed of a bin's captured requests",
        )
        .description(
            "Server-Sent Events: one `request` event per capture, with the sequence number as the \
             event id. Last-Event-ID replays stored requests after that number. Ends when the bin \
             expires or is deleted.",
        )
        .sse()
        .example(
            Example::get(
                "Stream captured requests",
                "/bin/0123456789abcdef01234567/requests/stream",
            )
            .expect_status(404),
        ),
        Endpoint::new(
            "/bin/{id}/requests/{n}",
            &["GET"],
            category::REQUEST_BIN,
            "One captured request by sequence number",
        )
        .example(
            Example::get(
                "First captured request",
                "/bin/0123456789abcdef01234567/requests/1",
            )
            .expect_status(404),
        ),
    ]
}

pub fn openapi_paths() -> Value {
    let id = json!({ "name": "id", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Bin id returned by POST /bin" });
    let path = json!({ "name": "path", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Any sub path" });
    let tags = json!(["Request Bin"]);
    let not_found = json!({ "description": "Unknown or expired bin" });
    let capture_op = |op_id: &str| {
        json!({
            "tags": tags,
            "summary": "Capture a request",
            "operationId": op_id,
            "requestBody": { "required": false, "content": { "*/*": { "schema": {} } } },
            "responses": {
                "200": { "description": "The bin's configured response (status, headers and body are configurable)" },
                "404": not_found
            }
        })
    };
    // Only POST is spelled out: the OpenAPI builder copies it to the other
    // methods of `any()` routes.
    let capture_item =
        |op_id: &str, params: Value| json!({ "parameters": params, "post": capture_op(op_id) });
    let mut root = capture_item("captureBin", json!([id]));
    if let Some(obj) = root.as_object_mut() {
        obj.insert(
            "delete".into(),
            json!({
                "tags": tags,
                "summary": "Delete the bin",
                "operationId": "deleteBin",
                "responses": { "200": { "description": "Deleted" }, "404": not_found }
            }),
        );
    }
    json!({
        "/bin": {
            "get": {
                "tags": tags,
                "summary": "List the caller's bins",
                "operationId": "listBins",
                "responses": { "200": { "description": "Bins of the caller's session" } }
            },
            "post": {
                "tags": tags,
                "summary": "Create a request bin",
                "operationId": "createBin",
                "requestBody": { "required": false, "content": { "application/json": { "schema": {
                    "type": "object",
                    "properties": {
                        "status": { "type": "integer", "minimum": 200, "maximum": 599 },
                        "headers": { "type": "object", "additionalProperties": { "type": "string" } },
                        "body": { "description": "String returned verbatim, any other JSON value is serialised" },
                        "delay_ms": { "type": "integer", "minimum": 0 }
                    }
                } } } },
                "responses": {
                    "201": { "description": "Bin created (id, URLs, expiry, limits)" },
                    "400": { "description": "Invalid response definition" },
                    "429": { "description": "Too many bins for this session" },
                    "503": { "description": "Store full (public mode)" }
                }
            }
        },
        "/bin/{id}": root,
        "/bin/{id}/{path}": capture_item("captureBinPath", json!([id, path])),
        "/bin/{id}/requests": {
            "get": {
                "tags": tags,
                "summary": "List captured requests",
                "operationId": "listBinRequests",
                "parameters": [id, { "name": "limit", "in": "query", "schema": { "type": "integer", "default": DEFAULT_LIST_LIMIT } }],
                "responses": { "200": { "description": "Bin summary and requests, newest first" }, "404": not_found }
            }
        },
        "/bin/{id}/requests/stream": {
            "get": {
                "tags": tags,
                "summary": "Live feed of captured requests (SSE)",
                "operationId": "streamBinRequests",
                "parameters": [id, { "name": "Last-Event-ID", "in": "header", "schema": { "type": "integer" } }],
                "responses": { "200": { "description": "text/event-stream of `request` events", "content": { "text/event-stream": {} } }, "404": not_found }
            }
        },
        "/bin/{id}/requests/{n}": {
            "get": {
                "tags": tags,
                "summary": "One captured request",
                "operationId": "getBinRequest",
                "parameters": [id, { "name": "n", "in": "path", "required": true, "schema": { "type": "integer" } }],
                "responses": { "200": { "description": "Captured request" }, "404": { "description": "Unknown bin or request" } }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_json, get_request, json_request, module_app_with_config};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn app() -> Router {
        module_app_with_config(Config::for_tests(), router)
    }

    async fn create(app: &Router, body: Value) -> Value {
        let resp = app
            .clone()
            .oneshot(json_request("POST", "/bin", &body))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::CREATED);
        body_json(resp).await
    }

    fn send(method: &str, uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "bins.test")
            .header("content-type", "text/plain")
            .body(Body::from(body.to_string()))
            .expect("request")
    }

    #[tokio::test]
    async fn create_capture_and_inspect() {
        let app = app();
        let resp = app
            .clone()
            .oneshot(send("POST", "/bin", ""))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::CREATED);
        let created = body_json(resp).await;
        let id = created["id"].as_str().expect("id").to_string();
        assert_eq!(created["url"], format!("http://bins.test/bin/{id}"));
        assert_eq!(created["requests_path"], format!("/bin/{id}/requests"));

        let resp = app
            .clone()
            .oneshot(send("PUT", &format!("/bin/{id}/hooks/a?x=1"), "hello"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["x-rustybin-bin-seq"], "1");
        let ack = body_json(resp).await;
        assert_eq!(ack["seq"], 1);

        let resp = app
            .clone()
            .oneshot(send("GET", &format!("/bin/{id}"), ""))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let list = body_json(
            app.clone()
                .oneshot(get_request(&format!("/bin/{id}/requests")))
                .await
                .expect("response"),
        )
        .await;
        assert_eq!(list["count"], 2);
        assert_eq!(list["bin"]["total_requests"], 2);
        let first = &list["requests"][1];
        assert_eq!(first["method"], "PUT");
        assert_eq!(first["path"], "/hooks/a");
        assert_eq!(first["query"], "x=1");
        assert_eq!(first["body"], "hello");
        // MockConnectInfo is not visible to session::client_ip, so only the key is checked.
        assert!(first.get("client_ip").is_some());
        assert_eq!(list["requests"][0]["path"], "/");

        let one = body_json(
            app.clone()
                .oneshot(get_request(&format!("/bin/{id}/requests/1")))
                .await
                .expect("response"),
        )
        .await;
        assert_eq!(one["method"], "PUT");
        let resp = app
            .clone()
            .oneshot(get_request(&format!("/bin/{id}/requests/9")))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let resp = app
            .clone()
            .oneshot(send("DELETE", &format!("/bin/{id}"), ""))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = app
            .oneshot(get_request(&format!("/bin/{id}/requests")))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn configured_response_is_returned() {
        let app = app();
        let created = create(
            &app,
            json!({"status": 202, "headers": {"X-Bin": "demo"}, "body": {"accepted": true}}),
        )
        .await;
        let id = created["id"].as_str().expect("id");
        let resp = app
            .oneshot(send("POST", &format!("/bin/{id}"), "{}"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert_eq!(resp.headers()["x-bin"], "demo");
        assert_eq!(resp.headers()["content-type"], "application/json");
        assert_eq!(body_json(resp).await, json!({"accepted": true}));
    }

    #[tokio::test]
    async fn invalid_definitions_are_rejected() {
        let app = app();
        for body in [
            json!({"status": 99}),
            json!({"headers": {"bad header": "x"}}),
            json!({"headers": {"content-length": "1"}}),
            json!({"delay_ms": 999_999}),
            json!({"unknown": 1}),
        ] {
            let resp = app
                .clone()
                .oneshot(json_request("POST", "/bin", &body))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{body}");
        }
    }

    #[tokio::test]
    async fn unknown_bin_is_json_404() {
        let app = app();
        for (m, uri) in [
            ("POST", "/bin/nope"),
            ("GET", "/bin/nope/x/y"),
            ("GET", "/bin/nope/requests"),
            ("GET", "/bin/nope/requests/1"),
            ("GET", "/bin/nope/requests/stream"),
            ("DELETE", "/bin/nope"),
        ] {
            let resp = app
                .clone()
                .oneshot(send(m, uri, ""))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{m} {uri}");
            assert_eq!(body_json(resp).await["error"], "bin not found");
        }
    }

    #[tokio::test]
    async fn inspection_routes_are_not_captured() {
        let app = app();
        let id = create(&app, json!({})).await["id"]
            .as_str()
            .expect("id")
            .to_string();
        for uri in [
            format!("/bin/{id}/requests"),
            format!("/bin/{id}/requests/1"),
        ] {
            let _ = app
                .clone()
                .oneshot(get_request(&uri))
                .await
                .expect("response");
        }
        // Other methods on inspection routes are rejected, not captured.
        let resp = app
            .clone()
            .oneshot(send("POST", &format!("/bin/{id}/requests"), "x"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        // A deeper path below "requests" is a capture.
        let resp = app
            .clone()
            .oneshot(send("POST", &format!("/bin/{id}/requests/1/x"), "x"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let list = body_json(
            app.oneshot(get_request(&format!("/bin/{id}/requests")))
                .await
                .expect("response"),
        )
        .await;
        assert_eq!(list["count"], 1);
        assert_eq!(list["requests"][0]["path"], "/requests/1/x");
    }

    #[test]
    fn requests_are_a_bounded_ring() {
        let mut limits = BinLimits::for_config(&Config::for_tests());
        limits.max_requests = 3;
        limits.max_bytes_per_bin = 100;
        let store = RequestBins::with_limits(limits, true);
        let bin = store.create("me", BinResponse::default()).expect("bin");
        let req = |body: &str| BinRequest {
            seq: 0,
            id: String::new(),
            bin_id: String::new(),
            timestamp: String::new(),
            method: "POST".into(),
            path: "/".into(),
            full_path: String::new(),
            query: None,
            headers: Vec::new(),
            content_type: None,
            body: Some(body.into()),
            body_base64: None,
            body_size: body.len(),
            body_truncated: false,
            client_ip: None,
        };
        for _ in 0..5 {
            store.record(&bin.id, req("x")).expect("recorded");
        }
        let entries = store.entries(&bin.id, 10).expect("entries");
        assert_eq!(
            entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![5, 4, 3]
        );
        // Byte budget: a 60 byte body evicts older entries.
        store.record(&bin.id, req(&"y".repeat(60))).expect("rec");
        store.record(&bin.id, req(&"z".repeat(60))).expect("rec");
        let entries = store.entries(&bin.id, 10).expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].seq, 7);
        assert_eq!(store.get(&bin.id).expect("bin").total_requests, 7);
    }

    #[test]
    fn bin_caps() {
        let mut limits = BinLimits::for_config(&Config::for_tests());
        limits.max_bins = 2;
        limits.max_bins_per_session = 2;
        // Normal mode evicts the oldest bin.
        let store = RequestBins::with_limits(limits, true);
        let a = store.create("a", BinResponse::default()).expect("a");
        store.create("b", BinResponse::default()).expect("b");
        store.create("c", BinResponse::default()).expect("c");
        assert_eq!(store.len(), 2);
        assert!(store.get(&a.id).is_none());
        // Public mode refuses instead.
        let store = RequestBins::with_limits(limits, false);
        store.create("a", BinResponse::default()).expect("a");
        store.create("b", BinResponse::default()).expect("b");
        assert_eq!(
            store.create("c", BinResponse::default()).unwrap_err(),
            CreateError::StoreFull
        );
        // Per-session cap.
        limits.max_bins = 10;
        limits.max_bins_per_session = 1;
        let store = RequestBins::with_limits(limits, false);
        store.create("a", BinResponse::default()).expect("a");
        assert_eq!(
            store.create("a", BinResponse::default()).unwrap_err(),
            CreateError::SessionLimit
        );
        assert_eq!(store.list_bins(Some("a")).len(), 1);
        assert_eq!(store.list_bins(Some("z")).len(), 0);
    }

    #[test]
    fn bins_expire() {
        let mut limits = BinLimits::for_config(&Config::for_tests());
        limits.ttl_secs = 0;
        let store = RequestBins::with_limits(limits, true);
        let bin = store.create("a", BinResponse::default()).expect("bin");
        assert!(store.get(&bin.id).is_none());
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn public_mode_limits_bins_per_session() {
        let mut config = Config::for_tests();
        config.public_mode = true;
        let app = module_app_with_config(config, router);
        for _ in 0..10 {
            create(&app, json!({})).await;
        }
        let resp = app
            .clone()
            .oneshot(json_request("POST", "/bin", &json!({})))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let list = body_json(app.oneshot(get_request("/bin")).await.expect("resp")).await;
        assert_eq!(list["count"], 10);
        assert_eq!(list["limits"]["ttl_secs"], 3600);
    }

    #[tokio::test]
    async fn stream_delivers_live_and_replayed_requests() {
        let app = app();
        let id = create(&app, json!({})).await["id"]
            .as_str()
            .expect("id")
            .to_string();
        let _ = app
            .clone()
            .oneshot(send("POST", &format!("/bin/{id}"), "first"))
            .await
            .expect("response");
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/bin/{id}/requests/stream"))
                    .header("last-event-id", "0")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let mut body = resp.into_body();
        let _ = app
            .clone()
            .oneshot(send("POST", &format!("/bin/{id}/x"), "second"))
            .await
            .expect("response");
        let mut text = String::new();
        while !text.contains("second") {
            let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
                .await
                .expect("frame in time")
                .expect("frame")
                .expect("ok frame");
            if let Ok(data) = frame.into_data() {
                text.push_str(&String::from_utf8_lossy(&data));
            }
        }
        assert!(text.contains("id: 1\n"), "{text}");
        assert!(text.contains("\"first\""), "{text}");
        assert!(text.contains("id: 2\n"), "{text}");
        assert!(text.contains("event: request"), "{text}");
    }
}
