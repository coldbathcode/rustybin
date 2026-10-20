//! API keys: shared mode (`RUSTYBIN_SHARED_MODE`) and the optional key on a
//! normal instance (`RUSTYBIN_REQUIRE_KEY`).
//!
//! Shared mode runs one multi-tenant instance for many API keys:
//! - every data-plane route needs a key, read from `X-Rustybin-Key`, else
//!   the `rb_key` query parameter (exempt: `/`, `GET /_rustybin/ready`,
//!   `/ui/*`). No key: `401 {"error":"rustybin_key_required"}`; an unknown,
//!   revoked or disabled key: `401 {"error":"rustybin_key_invalid"}`; before
//!   the registry was first loaded: `503 {"error":"rustybin_keys_unavailable"}`;
//! - the key id (never the secret) is the session key of the limiter and
//!   the inspector, and the key record's limits replace the instance limits;
//! - daily quotas (requests, response bytes) reset at 00:00 UTC: over a
//!   quota, `429` with `Retry-After` and
//!   `{"error":"rustybin_quota_exceeded","quota":..,"reset_at":..,"upgrade_url":..}`;
//! - control plane: `GET /_rustybin/requests*` and `GET /_rustybin/usage`
//!   accept the data key (or a control JWT with scope `inspector` and claim
//!   `rb_key=<key id>`) and only show that key's data; everything else under
//!   `/_rustybin/*` needs the admin token (or a JWT with scope `admin`).
//!
//! With `RUSTYBIN_REQUIRE_KEY` a normal instance only checks the key on the
//! data plane: no per-key limits, control-plane auth unchanged.
//!
//! Keys come from a registry document ([`registry`]) in a file
//! (`RUSTYBIN_KEYS_FILE`, reloaded when it changes) or at a URL
//! (`RUSTYBIN_KEYS_URL` with `RUSTYBIN_KEYS_TOKEN`, polled). Per-key daily
//! counters live in a [`ledger::Ledger`], optionally shared between
//! replicas through a quota store ([`store`], `RUSTYBIN_QUOTA_STORE`).

pub mod ledger;
pub mod registry;
pub mod store;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::limits::{Clock, Dimension, LimitRejected, LIMIT_HEADER, PLAN_HEADER};
use crate::metrics::Metrics;
use crate::state::AppState;

pub use ledger::{Ledger, QuotaKind};
pub use registry::{key_hash_hex, KeyLimits, KeyRecord, KeySet};
pub use store::QuotaStore;

/// Request header carrying the API key.
pub const KEY_HEADER: &str = "x-rustybin-key";
/// Query parameter carrying the API key (when the header is absent).
pub const KEY_QUERY: &str = "rb_key";
/// Per-key daily totals for the platform collector (admin).
pub const KEYS_USAGE_PATH: &str = "/_rustybin/keys/usage";
/// Error codes.
pub const ERR_REQUIRED: &str = "rustybin_key_required";
pub const ERR_INVALID: &str = "rustybin_key_invalid";
pub const ERR_UNAVAILABLE: &str = "rustybin_keys_unavailable";
pub const ERR_QUOTA: &str = "rustybin_quota_exceeded";
pub const ERR_STORE_UNAVAILABLE: &str = "rustybin_quota_store_unavailable";
/// How often the registry file is checked for changes.
pub const FILE_POLL: Duration = Duration::from_secs(5);
/// Bound on one registry download.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Bytes allowed per registry entry (the document limit is this times
/// `RUSTYBIN_KEYS_MAX`, plus 1 MiB).
pub const BYTES_PER_KEY: usize = 1024;
/// Largest page of `/_rustybin/keys/usage`.
pub const MAX_USAGE_PAGE: usize = 1000;
/// Longest wait between attempts while the quota store is down.
pub const MAX_STORE_BACKOFF: Duration = Duration::from_secs(60);
/// How often old rows are deleted from the quota store.
pub const CLEANUP_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// What the key layer does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyMode {
    /// No keys (the default).
    #[default]
    Off,
    /// `RUSTYBIN_SHARED_MODE`: keys, per-key limits and scoping.
    Shared,
    /// `RUSTYBIN_REQUIRE_KEY`: a valid key on the data plane, nothing else.
    Required,
}

impl KeyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyMode::Off => "off",
            KeyMode::Shared => "shared",
            KeyMode::Required => "required",
        }
    }
}

/// Where the registry comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeySource {
    File(PathBuf),
    Url { url: String, token: Option<String> },
}

/// Request extension set by [`enforce`] on requests made with a valid key.
#[derive(Clone, Debug)]
pub struct KeyContext {
    pub key: Arc<KeyRecord>,
    /// Shared mode: the key's limits apply and data is scoped by key.
    pub shared: bool,
    /// A control-plane read scoped to this key (inspector, usage).
    pub control: bool,
}

/// The outcome of looking up a presented key.
#[derive(Debug)]
pub enum Lookup {
    /// The registry was never loaded.
    Unavailable,
    Missing,
    Invalid,
    Found(Arc<KeyRecord>),
}

/// API key state (one per instance, in `AppState`).
pub struct Keys {
    mode: KeyMode,
    source: Option<KeySource>,
    base: crate::limits::LimitsConfig,
    max_keys: usize,
    poll: Duration,
    set: RwLock<Option<Arc<KeySet>>>,
    ledger: Ledger,
    clock: Arc<dyn Clock>,
    metrics: Arc<Metrics>,
    store: Option<Arc<dyn QuotaStore>>,
    flush_interval: Duration,
    upgrade_url: Option<String>,
    /// mtime and length of the registry file at the last load.
    file_seen: Mutex<Option<(SystemTime, u64)>>,
    /// `ETag` (or quoted `version`) of the last registry download.
    etag: Mutex<Option<String>>,
    /// The last registry check failed (warn on transitions only).
    failing: AtomicBool,
    store_up: AtomicBool,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keys")
            .field("mode", &self.mode)
            .field("source", &self.source.as_ref().map(source_kind))
            .finish_non_exhaustive()
    }
}

fn source_kind(s: &KeySource) -> &'static str {
    match s {
        KeySource::File(_) => "file",
        KeySource::Url { .. } => "url",
    }
}

impl Keys {
    /// Build the key state from the configuration. A registry file is read
    /// at once; a URL is fetched by [`Keys::registry_loop`].
    pub fn new(config: &Config, metrics: Arc<Metrics>, clock: Arc<dyn Clock>) -> Self {
        let mode = if config.shared_mode {
            KeyMode::Shared
        } else if config.require_key {
            KeyMode::Required
        } else {
            KeyMode::Off
        };
        let source = if mode == KeyMode::Off {
            None
        } else if let Some(path) = &config.keys_file {
            Some(KeySource::File(path.clone()))
        } else {
            config.keys_url.as_ref().map(|url| KeySource::Url {
                url: url.clone(),
                token: config.keys_token.clone(),
            })
        };
        let store = match (&config.quota_store, mode) {
            (Some(url), KeyMode::Shared | KeyMode::Required) => {
                store::open(url, config.quota_store_ca.as_deref())
            }
            _ => None,
        };
        let keys = Self {
            mode,
            source,
            base: config.limits.clone(),
            max_keys: config.keys_max.max(1),
            poll: Duration::from_secs(config.keys_poll_secs.max(1)),
            set: RwLock::new(None),
            ledger: Ledger::new(config.keys_max.saturating_mul(3).max(1024)),
            clock,
            metrics,
            store,
            flush_interval: Duration::from_millis(config.quota_flush_ms.max(10)),
            upgrade_url: config.quota_upgrade_url.clone(),
            file_seen: Mutex::new(None),
            etag: Mutex::new(None),
            failing: AtomicBool::new(false),
            store_up: AtomicBool::new(false),
        };
        if keys.enabled() {
            keys.metrics.set_keys_active(0);
            keys.metrics.set_keys_synced_at(0);
            if keys.store.is_some() {
                keys.metrics.set_quota_store_up(false);
            }
        }
        if let Some(KeySource::File(_)) = &keys.source {
            keys.poll_file_once();
        }
        keys
    }

    /// Use this quota store (tests and embedders).
    pub fn with_store(mut self, store: Arc<dyn QuotaStore>) -> Self {
        self.store = Some(store);
        self.metrics.set_quota_store_up(false);
        self
    }

    pub fn mode(&self) -> KeyMode {
        self.mode
    }

    /// Shared mode or `RUSTYBIN_REQUIRE_KEY`.
    pub fn enabled(&self) -> bool {
        self.mode != KeyMode::Off
    }

    pub fn shared(&self) -> bool {
        self.mode == KeyMode::Shared
    }

    /// The registry was loaded at least once.
    pub fn loaded(&self) -> bool {
        self.current().is_some()
    }

    /// The current registry.
    pub fn current(&self) -> Option<Arc<KeySet>> {
        self.set.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// The quota store kind (`postgres`, `memory`) or `local`.
    pub fn store_kind(&self) -> &'static str {
        self.store.as_ref().map_or("local", |s| s.kind())
    }

    /// The current UTC day.
    pub fn today(&self) -> NaiveDate {
        self.clock.wall().date_naive()
    }

    /// Start of the next UTC day.
    pub fn next_reset(&self) -> DateTime<Utc> {
        let next = self.today() + chrono::Duration::days(1);
        Utc.from_utc_datetime(&next.and_time(chrono::NaiveTime::MIN))
    }

    /// Look up a presented key (`None`: no key was presented).
    pub fn lookup(&self, presented: Option<&str>) -> Lookup {
        let Some(set) = self.current() else {
            return Lookup::Unavailable;
        };
        match presented {
            None => Lookup::Missing,
            Some(k) => match set.find(k) {
                Some(r) => Lookup::Found(r),
                None => Lookup::Invalid,
            },
        }
    }

    /// Replace the registry with a parsed document.
    pub fn install(&self, set: KeySet) {
        let (n, skipped, version) = (set.len(), set.skipped, set.version.clone());
        *self.set.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(set));
        self.metrics.set_keys_active(n as i64);
        self.mark_synced();
        tracing::info!(
            "key registry loaded: {n} active keys, {skipped} skipped (version {})",
            version.as_deref().unwrap_or("-")
        );
    }

    fn mark_synced(&self) {
        self.metrics
            .set_keys_synced_at(self.clock.wall().timestamp());
        if self.failing.swap(false, Ordering::Relaxed) {
            tracing::info!("key registry source reachable again");
        }
    }

    fn mark_failed(&self, why: &str) {
        if !self.failing.swap(true, Ordering::Relaxed) {
            tracing::warn!("key registry not refreshed ({why}); keeping the last good set");
        } else {
            tracing::debug!("key registry not refreshed ({why})");
        }
    }

    /// Parse and install a registry document.
    pub fn load_bytes(&self, raw: &[u8]) -> Result<usize, String> {
        let set = KeySet::parse(raw, &self.base, self.max_keys)?;
        let n = set.len();
        self.install(set);
        Ok(n)
    }

    fn max_document_bytes(&self) -> usize {
        self.max_keys
            .saturating_mul(BYTES_PER_KEY)
            .saturating_add(1 << 20)
    }

    /// Reload the registry file when its modification time or size changed.
    /// Returns true when a new set was installed.
    pub fn poll_file_once(&self) -> bool {
        let Some(KeySource::File(path)) = &self.source else {
            return false;
        };
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                self.mark_failed(&format!("cannot read {}: {e}", path.display()));
                return false;
            }
        };
        let stamp = (
            meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            meta.len(),
        );
        if *lock(&self.file_seen) == Some(stamp) {
            self.mark_synced();
            return false;
        }
        if meta.len() as usize > self.max_document_bytes() {
            self.mark_failed(&format!("{} is too large", path.display()));
            return false;
        }
        let raw = match std::fs::read(path) {
            Ok(raw) => raw,
            Err(e) => {
                self.mark_failed(&format!("cannot read {}: {e}", path.display()));
                return false;
            }
        };
        match self.load_bytes(&raw) {
            Ok(_) => {
                *lock(&self.file_seen) = Some(stamp);
                true
            }
            Err(e) => {
                *lock(&self.file_seen) = Some(stamp);
                self.mark_failed(&format!("{}: {e}", path.display()));
                false
            }
        }
    }

    /// Fetch the registry URL once (`If-None-Match` with the last `ETag`).
    /// Returns true when a new set was installed.
    pub async fn poll_url_once(&self, client: &reqwest::Client) -> bool {
        let Some(KeySource::Url { url, token }) = &self.source else {
            return false;
        };
        let mut req = client
            .get(url.as_str())
            .header(header::ACCEPT, "application/json");
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(tag) = lock(&self.etag).clone() {
            req = req.header(header::IF_NONE_MATCH, tag);
        }
        let mut resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                self.mark_failed(&format!("request failed: {}", e.without_url()));
                return false;
            }
        };
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            self.mark_synced();
            return false;
        }
        if !resp.status().is_success() {
            self.mark_failed(&format!("HTTP {}", resp.status().as_u16()));
            return false;
        }
        let etag = resp
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let max = self.max_document_bytes();
        let mut body = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > max {
                        self.mark_failed("registry document too large");
                        return false;
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) => {
                    self.mark_failed(&format!("reading the body failed: {}", e.without_url()));
                    return false;
                }
            }
        }
        match KeySet::parse(&body, &self.base, self.max_keys) {
            Ok(set) => {
                let tag = etag.or_else(|| set.version.as_ref().map(|v| format!("\"{v}\"")));
                *lock(&self.etag) = tag;
                self.install(set);
                true
            }
            Err(e) => {
                self.mark_failed(&e);
                false
            }
        }
    }

    /// Keep the registry current until `shutdown` resolves: the file is
    /// checked every [`FILE_POLL`], the URL fetched every
    /// `RUSTYBIN_KEYS_POLL_SECS` (the first fetch at once).
    pub async fn registry_loop<F: Future<Output = ()>>(self: Arc<Self>, shutdown: F) {
        tokio::pin!(shutdown);
        match self.source.clone() {
            None => {
                if self.enabled() {
                    tracing::warn!(
                        "{} without RUSTYBIN_KEYS_FILE or RUSTYBIN_KEYS_URL: every data-plane request gets 503 {ERR_UNAVAILABLE}",
                        if self.shared() { "RUSTYBIN_SHARED_MODE" } else { "RUSTYBIN_REQUIRE_KEY" }
                    );
                }
            }
            Some(KeySource::File(_)) => loop {
                tokio::select! {
                    _ = tokio::time::sleep(FILE_POLL) => { self.poll_file_once(); }
                    _ = &mut shutdown => return,
                }
            },
            Some(KeySource::Url { .. }) => {
                let client = match registry_client() {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("key registry client unavailable: {e}");
                        return;
                    }
                };
                let mut delay = Duration::ZERO;
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = &mut shutdown => return,
                    }
                    self.poll_url_once(&client).await;
                    // Retry sooner while nothing was ever loaded.
                    delay = if self.loaded() {
                        self.poll
                    } else {
                        self.poll.min(Duration::from_secs(5))
                    };
                }
            }
        }
    }

    // ── Quota store ─────────────────────────────────────────────────

    /// Flush local counts to the quota store once (no-op without a store).
    /// With nothing to flush, checks that the store is reachable while it
    /// is marked down.
    pub async fn flush_now(&self) -> Result<(), String> {
        let Some(store) = self.store.clone() else {
            return Ok(());
        };
        let result = tokio::time::timeout(FETCH_TIMEOUT, async {
            let batch = self.ledger.pending();
            if batch.is_empty() {
                if !self.store_up.load(Ordering::Relaxed) {
                    store.ping().await?;
                }
                return Ok(());
            }
            for chunk in batch.chunks(store::MAX_BATCH_ROWS) {
                let totals = store.flush(chunk).await?;
                self.ledger.apply(chunk, &totals);
            }
            Ok(())
        })
        .await
        .unwrap_or_else(|_| Err("quota store timed out".to_string()));
        self.ledger.prune(self.today());
        let up = result.is_ok();
        let was = self.store_up.swap(up, Ordering::Relaxed);
        self.metrics.set_quota_store_up(up);
        match (&result, was) {
            (Err(e), true) => tracing::warn!(
                "quota store unavailable ({e}): enforcing quotas with local counters until it is back"
            ),
            (Err(e), false) => tracing::debug!("quota store still unavailable ({e})"),
            (Ok(()), false) => tracing::info!("quota store connected ({})", store.kind()),
            (Ok(()), true) => {}
        }
        result
    }

    /// Flush every `RUSTYBIN_QUOTA_FLUSH_MS` (backing off up to
    /// [`MAX_STORE_BACKOFF`] while the store is down) and delete rows older
    /// than 35 days once a day. A last flush runs at shutdown.
    pub async fn flush_loop<F: Future<Output = ()>>(self: Arc<Self>, shutdown: F) {
        let Some(store) = self.store.clone() else {
            return;
        };
        tokio::pin!(shutdown);
        let mut delay = Duration::ZERO;
        let mut last_cleanup: Option<Instant> = None;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = &mut shutdown => {
                    let _ = self.flush_now().await;
                    return;
                }
            }
            match self.flush_now().await {
                Ok(()) => {
                    delay = self.flush_interval;
                    if last_cleanup.is_none_or(|t| t.elapsed() >= CLEANUP_INTERVAL) {
                        last_cleanup = Some(Instant::now());
                        let before = self.today() - chrono::Duration::days(store::RETENTION_DAYS);
                        match store.cleanup(before).await {
                            Ok(n) if n > 0 => {
                                tracing::info!("quota store: deleted {n} rows before {before}")
                            }
                            Ok(_) => {}
                            Err(e) => tracing::warn!("quota store cleanup failed: {e}"),
                        }
                    }
                }
                Err(_) => {
                    delay = (delay.max(self.flush_interval) * 2).min(MAX_STORE_BACKOFF);
                }
            }
        }
    }

    /// One page of a day's per-key totals: from the quota store when one is
    /// configured, else from this replica's counters.
    pub async fn usage_page(
        &self,
        day: NaiveDate,
        after: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<ledger::Total>, Option<String>), String> {
        let limit = limit.clamp(1, MAX_USAGE_PAGE);
        let Some(store) = &self.store else {
            return Ok(self.ledger.page(day, after, limit));
        };
        let mut rows = tokio::time::timeout(FETCH_TIMEOUT, store.page(day, after, limit + 1))
            .await
            .unwrap_or_else(|_| Err("quota store timed out".to_string()))?;
        let next = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().map(|r| r.key_id.clone())
        } else {
            None
        };
        Ok((rows, next))
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn registry_client() -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder();
    match crate::a2a::push::tls_config() {
        Ok(tls) => builder = builder.use_preconfigured_tls(tls),
        Err(e) => tracing::warn!("key registry: https unavailable ({e})"),
    }
    builder
        .redirect(reqwest::redirect::Policy::none())
        .timeout(FETCH_TIMEOUT)
        .user_agent(concat!("rustybin/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())
}

// ── Presented keys and redaction ────────────────────────────────────

/// The key a request presents: `X-Rustybin-Key`, else `?rb_key=`.
pub fn presented_key(headers: &HeaderMap, uri: &Uri) -> Option<String> {
    if let Some(v) = headers.get(KEY_HEADER) {
        match v.to_str().map(str::trim) {
            Ok(s) if !s.is_empty() => return Some(s.to_string()),
            Ok(_) => {}
            // Not text: an invalid key rather than a missing one.
            Err(_) => return Some(String::new()),
        }
    }
    let query = uri.query()?;
    form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == KEY_QUERY)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The redacted form of a key: `rbk_...` plus its last 4 characters.
pub fn redact_key(key: &str) -> String {
    let n = key.chars().count();
    if n >= 12 {
        let tail: String = key.chars().skip(n - 4).collect();
        format!("rbk_...{tail}")
    } else {
        "rbk_...".to_string()
    }
}

/// A query string with every `rb_key` value redacted (`None`: nothing to
/// redact).
pub fn redact_query(query: &str) -> Option<String> {
    let mut changed = false;
    let parts: Vec<String> = query
        .split('&')
        .map(|part| {
            let name = part.split_once('=').map_or(part, |(n, _)| n);
            let decoded = form_urlencoded::parse(part.as_bytes()).next();
            match decoded {
                Some((k, v)) if k == KEY_QUERY => {
                    changed = true;
                    format!("{name}={}", redact_key(&v))
                }
                _ => part.to_string(),
            }
        })
        .collect();
    changed.then(|| parts.join("&"))
}

/// A URI as text with the `rb_key` value redacted.
pub fn redact_uri(uri: &Uri) -> String {
    let text = uri.to_string();
    match (uri.query().and_then(redact_query), text.find('?')) {
        (Some(q), Some(pos)) => format!("{}?{q}", &text[..pos]),
        _ => text,
    }
}

/// A header value as captured: `X-Rustybin-Key` is redacted to its tail,
/// the admin token and visit manage token headers entirely (they are
/// Rustybin credentials, never something a gateway test needs to read).
pub fn redact_header(name: &str, value: &[u8]) -> String {
    let text = String::from_utf8_lossy(value);
    if name.eq_ignore_ascii_case(KEY_HEADER) {
        redact_key(text.trim())
    } else if name.eq_ignore_ascii_case(crate::admin::ADMIN_TOKEN_HEADER)
        || name.eq_ignore_ascii_case(crate::mocks::visits::TOKEN_HEADER)
    {
        "[redacted]".to_string()
    } else {
        text.into_owned()
    }
}

// ── Middleware ──────────────────────────────────────────────────────

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

fn key_error(lookup: &Lookup) -> Response {
    match lookup {
        Lookup::Unavailable => error(
            StatusCode::SERVICE_UNAVAILABLE,
            ERR_UNAVAILABLE,
            "the key registry has not been loaded yet; retry shortly",
        ),
        Lookup::Missing => {
            let mut resp = error(
                StatusCode::UNAUTHORIZED,
                ERR_REQUIRED,
                "send your API key in the X-Rustybin-Key header (or the rb_key query parameter)",
            );
            resp.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("X-Rustybin-Key"),
            );
            resp
        }
        Lookup::Invalid | Lookup::Found(_) => error(
            StatusCode::UNAUTHORIZED,
            ERR_INVALID,
            "this API key is unknown, revoked or disabled",
        ),
    }
}

/// Routes that never need a key: `/`, `/ui/*` and the readiness probe.
pub fn is_exempt(method: &Method, path: &str) -> bool {
    path == "/"
        || path == crate::control::UI_PREFIX
        || path
            .strip_prefix(crate::control::UI_PREFIX)
            .is_some_and(|rest| rest.starts_with('/'))
        || (path == crate::control_auth::READY_PATH
            && (method == Method::GET || method == Method::HEAD))
}

fn is_control(path: &str) -> bool {
    let prefix = crate::control::CONTROL_PREFIX;
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Mock management routes (`/_rustybin/mocks*`): with self-service on, a
/// key manages its own mocks there (the handlers check ownership).
fn is_mock_route(path: &str) -> bool {
    path == "/_rustybin/mocks" || path.starts_with("/_rustybin/mocks/")
}

/// Control-plane routes a key may read in shared mode.
fn is_key_readable(method: &Method, path: &str) -> bool {
    let inspector = path == "/_rustybin/requests" || path.starts_with("/_rustybin/requests/");
    (method == Method::GET || method == Method::HEAD)
        && (inspector || path == crate::limits::USAGE_PATH)
}

/// The key layer (installed by `build_app` when keys are enabled), outside
/// the plan limiter.
pub async fn enforce(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let keys = state.keys.clone();
    if !keys.enabled() || is_exempt(req.method(), req.uri().path()) {
        return next.run(req).await;
    }
    if is_control(req.uri().path()) {
        if !keys.shared() {
            // RUSTYBIN_REQUIRE_KEY: control-plane auth is unchanged.
            return next.run(req).await;
        }
        return control(state, req, next).await;
    }

    let presented = presented_key(req.headers(), req.uri());
    let record = match keys.lookup(presented.as_deref()) {
        Lookup::Found(r) => r,
        other => return key_error(&other),
    };
    let shared = keys.shared();
    let day = keys.today();
    let (max_requests, max_egress) = if shared {
        (record.requests_per_day, record.egress_per_day)
    } else {
        (0, 0)
    };
    let remaining = match keys.ledger.admit(day, &record.id, max_requests, max_egress) {
        Ok(r) => r,
        Err(kind) => return quota_response(&state, &record, kind),
    };
    req.extensions_mut().insert(KeyContext {
        key: record.clone(),
        shared,
        control: false,
    });
    let mut resp = next.run(req).await;
    if resp.extensions().get::<LimitRejected>().is_some() {
        keys.ledger.uncount(day, &record.id);
        return resp;
    }
    if let Some(left) = remaining {
        resp.headers_mut().insert(
            HeaderName::from_static(crate::limits::QUOTA_REMAINING_HEADER),
            HeaderValue::from(left),
        );
    }
    let (parts, body) = resp.into_parts();
    let metered = KeyEgressBody {
        inner: body,
        keys,
        day,
        key: record.id.clone(),
    };
    Response::from_parts(parts, Body::new(metered))
}

/// Shared mode, `/_rustybin/*`: admin for everything, a key (or a JWT bound
/// to a key) for its own inspector entries and usage.
async fn control(state: AppState, mut req: Request, next: Next) -> Response {
    let config = &state.config;
    if crate::admin::require_admin(req.headers(), config).is_ok() {
        req.extensions_mut()
            .insert(crate::control_auth::ControlAuthorized(
                crate::control_auth::Grant::ADMIN,
            ));
        return next.run(req).await;
    }
    let self_service = config.self_service.enabled && is_mock_route(req.uri().path());
    // The management MCP server scopes a key holder to its own mocks,
    // inspector entries and usage itself.
    let manage = req.uri().path() == crate::mcp::manage::MANAGE_PATH;
    if !self_service && !manage && !is_key_readable(req.method(), req.uri().path()) {
        let message = "in shared mode this control-plane route needs the admin token";
        let status = if config.admin_token.is_some() {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::FORBIDDEN
        };
        return (
            status,
            Json(json!({ "error": "rustybin_admin_required", "message": message })),
        )
            .into_response();
    }
    let record = if let Some(k) = presented_key(req.headers(), req.uri()) {
        match state.keys.lookup(Some(&k)) {
            Lookup::Found(r) => r,
            other => return key_error(&other),
        }
    } else if let Some(token) = crate::control_auth::presented_token(req.headers()) {
        match crate::control_auth::verify_jwt_claims(token, config) {
            Ok((grant, claims)) if grant.inspector => {
                let Some(id) = claims.get("rb_key").and_then(Value::as_str) else {
                    return error(
                        StatusCode::FORBIDDEN,
                        "rustybin_admin_required",
                        "in shared mode a control JWT needs the admin scope, or the inspector scope and an rb_key claim",
                    );
                };
                match state.keys.current() {
                    None => return key_error(&Lookup::Unavailable),
                    Some(set) => match set.by_id(id) {
                        Some(r) => r,
                        None => return key_error(&Lookup::Invalid),
                    },
                }
            }
            Ok(_) => {
                return error(
                    StatusCode::FORBIDDEN,
                    "rustybin_admin_required",
                    "the token's scope does not include this control-plane route",
                )
            }
            Err(e) => {
                return error(
                    StatusCode::UNAUTHORIZED,
                    ERR_INVALID,
                    &format!("invalid control-plane token ({})", e.as_str()),
                )
            }
        }
    } else {
        return key_error(&Lookup::Missing);
    };
    req.extensions_mut().insert(KeyContext {
        key: record,
        shared: true,
        control: true,
    });
    next.run(req).await
}

/// The 429 of a used-up daily quota.
fn quota_response(state: &AppState, record: &KeyRecord, kind: QuotaKind) -> Response {
    let keys = &state.keys;
    let now = keys.clock.wall();
    let reset = keys.next_reset();
    let retry_after = (reset - now).num_seconds().max(1) as u64;
    state.metrics.count_limit_rejection(match kind {
        QuotaKind::Requests => Dimension::Requests,
        QuotaKind::Egress => Dimension::Egress,
    });
    let body = json!({
        "error": ERR_QUOTA,
        "quota": kind.as_str(),
        "reset_at": reset.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "upgrade_url": keys.upgrade_url,
    });
    let mut resp = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
    let h = resp.headers_mut();
    h.insert(header::RETRY_AFTER, HeaderValue::from(retry_after));
    h.insert(
        HeaderName::from_static(LIMIT_HEADER),
        HeaderValue::from_static(kind.as_str()),
    );
    h.insert(
        HeaderName::from_static(PLAN_HEADER),
        record.limits.plan_header(),
    );
    resp
}

/// Response body wrapper counting a key's response bytes.
struct KeyEgressBody {
    inner: Body,
    keys: Arc<Keys>,
    day: NaiveDate,
    key: Arc<str>,
}

impl HttpBody for KeyEgressBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_frame(cx);
        if let Poll::Ready(Some(Ok(frame))) = &polled {
            if let Some(data) = frame.data_ref() {
                this.keys
                    .ledger
                    .add_egress(this.day, &this.key, data.len() as u64);
            }
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

/// `/_rustybin/usage` of one key (shared mode).
pub fn key_usage(state: &AppState, record: &KeyRecord) -> Value {
    let keys = &state.keys;
    let mut v = state.limits.usage_for(&record.limits, Some(&record.id));
    let (requests, egress) = keys.ledger.totals(keys.today(), &record.id);
    let opt = |n: u64| if n == 0 { Value::Null } else { json!(n) };
    let left = |limit: u64, used: u64| {
        if limit == 0 {
            Value::Null
        } else {
            json!(limit.saturating_sub(used))
        }
    };
    v["key_id"] = json!(record.id.as_ref());
    v["limits"]["requests"] = opt(record.requests_per_day);
    v["limits"]["egress_bytes"] = opt(record.egress_per_day);
    v["requests"] = json!({
        "used": requests,
        "limit": opt(record.requests_per_day),
        "remaining": left(record.requests_per_day, requests),
    });
    v["egress"] = json!({
        "used_bytes": egress,
        "limit_bytes": opt(record.egress_per_day),
        "remaining_bytes": left(record.egress_per_day, egress),
    });
    v["inspector"] = json!({
        "entries": record.inspector_entries,
        "ttl_secs": record.inspector_ttl.map(|d| d.as_secs()),
    });
    v["quota_store"] = json!(keys.store_kind());
    v["instance_id"] = json!(state.config.instance_id);
    v["build"] = crate::control::build_info();
    v["control_auth"] = json!(state.config.control_auth.as_str());
    v
}

// ── gRPC ────────────────────────────────────────────────────────────

/// Key, daily quota and rate check of a gRPC call (tonic interceptor).
/// Without keys this is the limiter's own check.
#[allow(clippy::result_large_err)]
pub fn check_grpc(
    keys: &Keys,
    limiter: &crate::limits::Limiter,
    req: &tonic::Request<()>,
) -> Result<(), tonic::Status> {
    if !keys.enabled() {
        return limiter.check_grpc(req);
    }
    let presented = req
        .metadata()
        .get(KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let record = match keys.lookup(presented) {
        Lookup::Found(r) => r,
        Lookup::Unavailable => return Err(tonic::Status::unavailable(ERR_UNAVAILABLE)),
        Lookup::Missing => {
            return Err(tonic::Status::unauthenticated(
                "rustybin_key_required: send the x-rustybin-key metadata",
            ))
        }
        Lookup::Invalid => {
            return Err(tonic::Status::unauthenticated(
                "rustybin_key_invalid: this API key is unknown, revoked or disabled",
            ))
        }
    };
    if !keys.shared() {
        return limiter.check_grpc(req);
    }
    let day = keys.today();
    if let Err(kind) = keys.ledger.admit(
        day,
        &record.id,
        record.requests_per_day,
        record.egress_per_day,
    ) {
        let reset = keys
            .next_reset()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut status = tonic::Status::resource_exhausted(format!(
            "{ERR_QUOTA}: the daily {} quota of this key is used up until {reset}",
            kind.as_str()
        ));
        let retry = (keys.next_reset() - keys.clock.wall()).num_seconds().max(1);
        if let Ok(v) = retry.to_string().parse() {
            status.metadata_mut().insert("retry-after", v);
        }
        return Err(status);
    }
    limiter
        .check_grpc_for(&record.limits, Some(&record.id))
        .inspect_err(|_| keys.ledger.uncount(day, &record.id))
}

// ── Usage endpoint ──────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
struct UsageQuery {
    day: Option<String>,
    after: Option<String>,
    limit: Option<String>,
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

async fn keys_usage_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<UsageQuery>,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let day = match q.day.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        None => state.keys.today(),
        Some(raw) => match NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            Ok(d) => d,
            Err(_) => return bad_request("invalid day: expected YYYY-MM-DD"),
        },
    };
    let limit = match q.limit.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
        None => MAX_USAGE_PAGE,
        Some(raw) => match raw.parse::<usize>() {
            Ok(n) if n >= 1 => n.min(MAX_USAGE_PAGE),
            _ => return bad_request("invalid limit: expected a whole number from 1 to 1000"),
        },
    };
    let after = q.after.as_deref().map(str::trim).filter(|a| !a.is_empty());
    if after.is_some_and(|a| !crate::session::is_valid_session(a)) {
        return bad_request("invalid after: expected a key id");
    }
    match state.keys.usage_page(day, after, limit).await {
        Ok((rows, next)) => Json(json!({
            "day": day.format("%Y-%m-%d").to_string(),
            "keys": rows.iter().map(|r| json!({
                "key_id": r.key_id,
                "requests": r.requests,
                "egress_bytes": r.egress_bytes,
            })).collect::<Vec<_>>(),
            "next_after": next,
        }))
        .into_response(),
        Err(e) => {
            tracing::warn!("keys usage: quota store unavailable ({e})");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": ERR_STORE_UNAVAILABLE })),
            )
                .into_response()
        }
    }
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new().route(KEYS_USAGE_PATH, get(keys_usage_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![Endpoint::new(
        KEYS_USAGE_PATH,
        &["GET"],
        category::CONTROL,
        "Per-key daily totals (requests, response bytes), paginated (shared mode, admin)",
    )
    .description(
        "?day=YYYY-MM-DD (default today, UTC), ?after=<key id>, ?limit= (1 to 1000, default \
         1000). Rows are sorted by key id; next_after is the key id to continue from, or null. \
         From the quota store (RUSTYBIN_QUOTA_STORE) when configured, else from this replica's \
         counters (today and the two previous days). Needs the admin token.",
    )
    .example(Example::get("Per-key usage today", KEYS_USAGE_PATH))]
}

pub fn openapi_paths() -> Value {
    json!({
        KEYS_USAGE_PATH: {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Per-key daily usage",
                "description": "Per-key totals of one UTC day for a platform collector (shared mode or RUSTYBIN_REQUIRE_KEY). From the quota store when RUSTYBIN_QUOTA_STORE is set, else from this replica's in-memory counters (today and the two previous days). Sorted by key id; continue with ?after=<next_after>. Admin token required.",
                "operationId": "getRustybinKeysUsage",
                "parameters": [
                    { "name": "day", "in": "query", "required": false, "schema": { "type": "string", "format": "date" }, "description": "UTC day, default today" },
                    { "name": "after", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Return key ids greater than this one" },
                    { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer", "minimum": 1, "maximum": MAX_USAGE_PAGE, "default": MAX_USAGE_PAGE } }
                ],
                "responses": {
                    "200": { "description": "One page of per-key totals", "content": { "application/json": { "schema": {
                        "type": "object",
                        "properties": {
                            "day": { "type": "string", "format": "date" },
                            "keys": { "type": "array", "items": { "type": "object", "properties": {
                                "key_id": { "type": "string" },
                                "requests": { "type": "integer" },
                                "egress_bytes": { "type": "integer" }
                            } } },
                            "next_after": { "type": ["string", "null"] }
                        }
                    } } } },
                    "400": { "description": "Invalid day, after or limit" },
                    "401": { "description": "Admin token required" },
                    "403": { "description": "Public or shared mode without an admin token" },
                    "503": { "description": "The quota store is unavailable (rustybin_quota_store_unavailable)" }
                }
            }
        }
    })
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(all(test, feature = "quota-postgres"))]
mod pg_tests;
