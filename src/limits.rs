//! Plan limits: rate, concurrency, streams and period quotas, for hosted
//! offerings (a shared free demo instance, one dedicated instance per paying
//! workspace).
//!
//! `RUSTYBIN_PLAN` picks a preset ([`LimitsConfig::preset`]); the
//! `RUSTYBIN_LIMIT_*` variables override single dimensions (0 or `unlimited`
//! turns one off). Plan `none` (the default) with no override enforces
//! nothing and adds no header: the middleware returns immediately.
//!
//! Dimensions, per scope (the instance, or each `session::session_key`):
//! - `rps`: token bucket (`rps` per second, `burst` capacity).
//! - `concurrency`: requests in flight (until the response body ends).
//! - `streams`: open SSE responses and WebSocket connections, each ended
//!   after the stream lifetime (SSE: a final comment, then the body ends;
//!   WebSocket: close 1008 with a reason).
//! - `requests` and `egress`: quotas per calendar UTC day or month. Egress
//!   counts response body bytes; an exhausted quota rejects new requests and
//!   never cuts a response in flight.
//!
//! Instance quota counters can be persisted (`RUSTYBIN_USAGE_FILE`) so a
//! restart (scale to zero) does not reset a monthly quota. Session counters
//! live in a bounded map and are not persisted.
//!
//! Exempt from every limit: `/` (platform health check), `/ui/*`,
//! `GET /_rustybin/usage` and `GET /_rustybin/status` (polled by the console). Other control plane requests that carry a valid
//! admin token are counted but never rejected.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

/// The usage endpoint (always exempt from limits).
pub const USAGE_PATH: &str = "/_rustybin/usage";
/// The status endpoint the console polls every few seconds (exempt so an
/// open console cannot use up a plan's quota).
pub const STATUS_PATH: &str = "/_rustybin/status";
/// Response header naming the active plan.
pub const PLAN_HEADER: &str = "x-rustybin-plan";
/// Response header with the requests left in the current period.
pub const QUOTA_REMAINING_HEADER: &str = "x-rustybin-quota-remaining";
/// Response header naming the dimension that rejected a request.
pub const LIMIT_HEADER: &str = "x-rustybin-limit";
/// Error code of every plan limit rejection body.
pub const ERROR_CODE: &str = "rustybin_plan_limit";
/// Maximum number of session-scoped counter sets kept in memory.
pub const MAX_SESSIONS: usize = 10_000;
/// At the session cap, sets idle for longer than this are evicted first.
pub const SESSION_IDLE_TTL: Duration = Duration::from_secs(3600);
/// How often the usage file is written (when it changed).
pub const PERSIST_INTERVAL: Duration = Duration::from_secs(30);
/// SSE comment written before a stream is ended at the lifetime limit.
pub const STREAM_END_COMMENT: &[u8] = b": rustybin plan stream lifetime reached\n\n";
/// WebSocket close reason at the lifetime limit (close code 1008).
pub const STREAM_END_REASON: &str = "rustybin plan stream lifetime reached";

// ── Configuration ───────────────────────────────────────────────────

/// `RUSTYBIN_PLAN`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Plan {
    /// No plan: nothing is enforced and no header is added.
    #[default]
    None,
    Free,
    Pro,
    Team,
    /// No limits, but the plan name is reported.
    Enterprise,
}

impl Plan {
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw.trim().to_ascii_lowercase().as_str() {
            "none" | "off" => Plan::None,
            "free" => Plan::Free,
            "pro" => Plan::Pro,
            "team" => Plan::Team,
            "enterprise" => Plan::Enterprise,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Plan::None => "none",
            Plan::Free => "free",
            Plan::Pro => "pro",
            Plan::Team => "team",
            Plan::Enterprise => "enterprise",
        }
    }
}

/// `RUSTYBIN_LIMIT_SCOPE`: who shares one set of counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    /// One set of counters for the whole instance (a dedicated instance).
    #[default]
    Instance,
    /// One set per `session::session_key` (the shared public instance).
    Session,
}

impl Scope {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "instance" | "global" => Some(Scope::Instance),
            "session" | "client" => Some(Scope::Session),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Instance => "instance",
            Scope::Session => "session",
        }
    }
}

/// `RUSTYBIN_LIMIT_PERIOD`: the quota period (calendar UTC).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Period {
    Day,
    #[default]
    Month,
}

impl Period {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "day" | "daily" => Some(Period::Day),
            "month" | "monthly" => Some(Period::Month),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Period::Day => "day",
            Period::Month => "month",
        }
    }

    /// Start of the period containing `now` (00:00 UTC of the day or of the
    /// first day of the month).
    pub fn start(self, now: DateTime<Utc>) -> DateTime<Utc> {
        let (y, m, d) = match self {
            Period::Day => (now.year(), now.month(), now.day()),
            Period::Month => (now.year(), now.month(), 1),
        };
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0)
            .single()
            .unwrap_or(now)
    }

    /// Start of the next period.
    pub fn end(self, now: DateTime<Utc>) -> DateTime<Utc> {
        let start = self.start(now);
        match self {
            Period::Day => start + chrono::Duration::days(1),
            Period::Month => {
                let (y, m) = if start.month() == 12 {
                    (start.year() + 1, 1)
                } else {
                    (start.year(), start.month() + 1)
                };
                Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0)
                    .single()
                    .unwrap_or(start + chrono::Duration::days(31))
            }
        }
    }
}

/// Effective limits. A value of 0 (or a zero duration) turns that dimension off.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LimitsConfig {
    pub plan: Plan,
    pub scope: Scope,
    pub period: Period,
    /// Sustained requests per second (token bucket refill rate).
    pub rps: u64,
    /// Token bucket capacity (0 = same as `rps`).
    pub burst: u64,
    /// Requests in flight.
    pub concurrency: u64,
    /// Open streams (SSE responses, WebSocket connections).
    pub streams: u64,
    /// Maximum stream lifetime.
    pub stream_lifetime: Duration,
    /// Requests per period.
    pub requests: u64,
    /// Response body bytes per period.
    pub egress_bytes: u64,
    /// Where instance quota counters are persisted (`RUSTYBIN_USAGE_FILE`).
    pub usage_file: Option<PathBuf>,
    /// At least one `RUSTYBIN_LIMIT_*` variable was set.
    pub overridden: bool,
}

impl LimitsConfig {
    /// The preset of a plan (no overrides).
    pub fn preset(plan: Plan) -> Self {
        let base = Self {
            plan,
            ..Self::default()
        };
        match plan {
            Plan::None | Plan::Enterprise => base,
            Plan::Free => Self {
                scope: Scope::Session,
                period: Period::Day,
                rps: 5,
                burst: 20,
                concurrency: 10,
                streams: 3,
                stream_lifetime: Duration::from_secs(300),
                requests: 10_000,
                egress_bytes: 1_000_000_000,
                ..base
            },
            Plan::Pro => Self {
                scope: Scope::Instance,
                period: Period::Month,
                rps: 50,
                burst: 200,
                concurrency: 100,
                streams: 25,
                stream_lifetime: Duration::from_secs(3600),
                requests: 1_000_000,
                egress_bytes: 10_000_000_000,
                ..base
            },
            Plan::Team => Self {
                scope: Scope::Instance,
                period: Period::Month,
                rps: 250,
                burst: 1000,
                concurrency: 500,
                streams: 200,
                stream_lifetime: Duration::from_secs(14_400),
                requests: 10_000_000,
                egress_bytes: 100_000_000_000,
                ..base
            },
        }
    }

    /// Is the limiter on (a plan other than `none`, or any override)?
    pub fn active(&self) -> bool {
        self.plan != Plan::None || self.overridden
    }

    /// The reported plan name (`custom` for overrides without a plan).
    pub fn plan_name(&self) -> &'static str {
        if self.plan == Plan::None && self.overridden {
            "custom"
        } else {
            self.plan.as_str()
        }
    }

    /// Effective bucket capacity.
    pub fn bucket_capacity(&self) -> u64 {
        if self.burst == 0 {
            self.rps
        } else {
            self.burst
        }
    }

    /// Configured limits as JSON (`null` = unlimited).
    pub fn public_view(&self) -> Value {
        let opt = |v: u64| if v == 0 { Value::Null } else { json!(v) };
        let rps_on = self.active() && self.rps > 0;
        json!({
            "active": self.active(),
            "scope": self.scope.as_str(),
            "period": self.period.as_str(),
            "rps": if rps_on { json!(self.rps) } else { Value::Null },
            "burst": if rps_on { json!(self.bucket_capacity()) } else { Value::Null },
            "concurrency": opt(self.concurrency),
            "streams": opt(self.streams),
            "stream_lifetime_secs": opt(self.stream_lifetime.as_secs()),
            "requests": opt(self.requests),
            "egress_bytes": opt(self.egress_bytes),
            "usage_file_configured": self.usage_file.is_some(),
        })
    }
}

// ── Clock ───────────────────────────────────────────────────────────

/// Time source (tests inject a manual clock).
pub trait Clock: Send + Sync {
    /// Wall clock, for quota periods.
    fn wall(&self) -> DateTime<Utc>;
    /// Monotonic time since an arbitrary origin, for the token bucket.
    fn mono(&self) -> Duration;
}

/// The real clock.
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn wall(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn mono(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// A clock that only moves when told to (tests).
pub struct ManualClock {
    now: Mutex<(DateTime<Utc>, Duration)>,
}

impl ManualClock {
    pub fn new(wall: DateTime<Utc>) -> Self {
        Self {
            now: Mutex::new((wall, Duration::ZERO)),
        }
    }

    /// Move both clocks forward.
    pub fn advance(&self, by: Duration) {
        let mut now = lock(&self.now);
        now.0 += chrono::Duration::from_std(by).unwrap_or_default();
        now.1 += by;
    }
}

impl Clock for ManualClock {
    fn wall(&self) -> DateTime<Utc> {
        lock(&self.now).0
    }

    fn mono(&self) -> Duration {
        lock(&self.now).1
    }
}

/// Lock a mutex, recovering from poisoning (never panics).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ── Counters ────────────────────────────────────────────────────────

/// One scope's counters (the instance, or one session).
#[derive(Debug)]
pub struct Counters {
    state: Mutex<CounterState>,
    in_flight: AtomicU64,
    streams: AtomicU64,
    /// Monotonic seconds of the last request (session eviction).
    last_seen: AtomicU64,
}

#[derive(Debug, Clone)]
struct CounterState {
    tokens: f64,
    refilled_at: Duration,
    period_start: DateTime<Utc>,
    requests: u64,
    egress: u64,
}

impl Counters {
    fn new(capacity: u64, now: Duration, period_start: DateTime<Utc>) -> Self {
        Self {
            state: Mutex::new(CounterState {
                tokens: capacity as f64,
                refilled_at: now,
                period_start,
                requests: 0,
                egress: 0,
            }),
            in_flight: AtomicU64::new(0),
            streams: AtomicU64::new(0),
            last_seen: AtomicU64::new(now.as_secs()),
        }
    }

    fn add_egress(&self, bytes: usize) {
        let mut st = lock(&self.state);
        st.egress = st.egress.saturating_add(bytes as u64);
    }

    pub fn in_flight(&self) -> u64 {
        self.in_flight.load(Ordering::Relaxed)
    }

    pub fn open_streams(&self) -> u64 {
        self.streams.load(Ordering::Relaxed)
    }
}

/// Decrements a counter when dropped.
#[derive(Debug)]
struct SlotGuard {
    counters: Arc<Counters>,
    stream: bool,
}

impl SlotGuard {
    fn new(counters: Arc<Counters>, stream: bool) -> Self {
        let counter = if stream {
            &counters.streams
        } else {
            &counters.in_flight
        };
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counters, stream }
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let counter = if self.stream {
            &self.counters.streams
        } else {
            &self.counters.in_flight
        };
        counter.fetch_sub(1, Ordering::Relaxed);
    }
}

/// An open stream slot. The limiter inserts one into the extensions of a
/// WebSocket upgrade request; the WebSocket handler moves it into its
/// connection task (so the slot stays taken while the socket is open) and
/// ends the connection at [`StreamLease::deadline`].
///
/// ```ignore
/// async fn ws(ws: WebSocketUpgrade, lease: Option<Extension<StreamLease>>) -> Response {
///     let lease = lease.map(|Extension(l)| l);
///     ws.on_upgrade(move |socket| serve(socket, lease))
/// }
/// ```
#[derive(Clone, Debug)]
pub struct StreamLease(Arc<LeaseInner>);

#[derive(Debug)]
struct LeaseInner {
    _slot: SlotGuard,
    deadline: Option<tokio::time::Instant>,
}

impl StreamLease {
    fn new(counters: Arc<Counters>, lifetime: Duration) -> Self {
        let deadline = (!lifetime.is_zero()).then(|| tokio::time::Instant::now() + lifetime);
        Self(Arc::new(LeaseInner {
            _slot: SlotGuard::new(counters, true),
            deadline,
        }))
    }

    /// When the plan's stream lifetime ends (`None`: no lifetime limit).
    pub fn deadline(&self) -> Option<tokio::time::Instant> {
        self.0.deadline
    }

    /// Resolves at the deadline; never when there is none.
    pub async fn expired(&self) {
        match self.0.deadline {
            Some(at) => tokio::time::sleep_until(at).await,
            None => std::future::pending().await,
        }
    }
}

/// Resolves when an optional lease expires; never without a lease.
pub async fn lease_expired(lease: &Option<StreamLease>) {
    match lease {
        Some(l) => l.expired().await,
        None => std::future::pending().await,
    }
}

// ── Limiter ─────────────────────────────────────────────────────────

/// A limit dimension (also the `X-Rustybin-Limit` value).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dimension {
    Rps,
    Concurrency,
    Streams,
    Requests,
    Egress,
}

impl Dimension {
    pub fn as_str(self) -> &'static str {
        match self {
            Dimension::Rps => "rps",
            Dimension::Concurrency => "concurrency",
            Dimension::Streams => "streams",
            Dimension::Requests => "requests",
            Dimension::Egress => "egress",
        }
    }
}

/// Why a request was rejected.
#[derive(Clone, Debug, PartialEq)]
pub struct Rejection {
    pub dimension: Dimension,
    pub retry_after_secs: u64,
}

/// A request let through: holds its in-flight slot (and a stream slot for a
/// WebSocket upgrade) until dropped.
#[derive(Debug)]
pub struct Admission {
    counters: Arc<Counters>,
    in_flight: Option<SlotGuard>,
    stream: Option<StreamLease>,
    /// Requests left in the period after this one (`None`: no request quota).
    pub requests_remaining: Option<u64>,
}

/// Shared limiter state (one per instance).
pub struct Limiter {
    cfg: LimitsConfig,
    clock: Arc<dyn Clock>,
    instance: Arc<Counters>,
    sessions: Mutex<HashMap<String, Arc<Counters>>>,
    /// Quota values last written to the usage file (skip unchanged writes).
    persisted: Mutex<Option<(DateTime<Utc>, u64, u64)>>,
}

impl std::fmt::Debug for Limiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Limiter").field("cfg", &self.cfg).finish()
    }
}

impl Limiter {
    /// Limiter on the system clock; loads the usage file when configured.
    pub fn new(cfg: LimitsConfig) -> Self {
        Self::with_clock(cfg, Arc::new(SystemClock::new()))
    }

    pub fn with_clock(cfg: LimitsConfig, clock: Arc<dyn Clock>) -> Self {
        let now = clock.wall();
        let instance = Arc::new(Counters::new(
            cfg.bucket_capacity(),
            clock.mono(),
            cfg.period.start(now),
        ));
        let limiter = Self {
            cfg,
            clock,
            instance,
            sessions: Mutex::new(HashMap::new()),
            persisted: Mutex::new(None),
        };
        limiter.load();
        limiter
    }

    pub fn config(&self) -> &LimitsConfig {
        &self.cfg
    }

    pub fn active(&self) -> bool {
        self.cfg.active()
    }

    /// The counters of a scope key (`None` for instance scope). Session sets
    /// are created on first use; the map is capped at [`MAX_SESSIONS`].
    pub fn counters(&self, session_key: Option<&str>) -> Arc<Counters> {
        let (Scope::Session, Some(key)) = (self.cfg.scope, session_key) else {
            return self.instance.clone();
        };
        let now = self.clock.mono();
        let mut map = lock(&self.sessions);
        if let Some(c) = map.get(key) {
            c.last_seen.store(now.as_secs(), Ordering::Relaxed);
            return c.clone();
        }
        if map.len() >= MAX_SESSIONS {
            evict(&mut map, now);
        }
        let c = Arc::new(Counters::new(
            self.cfg.bucket_capacity(),
            now,
            self.cfg.period.start(self.clock.wall()),
        ));
        map.insert(key.to_string(), c.clone());
        c
    }

    /// Existing counters of a session (never creates one).
    fn peek(&self, session_key: &str) -> Option<Arc<Counters>> {
        lock(&self.sessions).get(session_key).cloned()
    }

    /// Number of session counter sets held.
    pub fn session_count(&self) -> usize {
        lock(&self.sessions).len()
    }

    fn roll(&self, st: &mut CounterState, now: DateTime<Utc>) {
        let start = self.cfg.period.start(now);
        if start != st.period_start {
            st.period_start = start;
            st.requests = 0;
            st.egress = 0;
        }
    }

    fn secs_to_period_end(&self, now: DateTime<Utc>) -> u64 {
        let end = self.cfg.period.end(now);
        (end - now).num_seconds().max(1) as u64
    }

    /// Admit or reject one request. `bypass` (admin control plane requests)
    /// counts the request without enforcing anything; `stream` takes a
    /// stream slot (WebSocket upgrade).
    pub fn admit(
        &self,
        counters: &Arc<Counters>,
        bypass: bool,
        stream: bool,
    ) -> Result<Admission, Rejection> {
        let cfg = &self.cfg;
        let now = self.clock.wall();
        let mono = self.clock.mono();
        let mut st = lock(&counters.state);
        self.roll(&mut st, now);
        if cfg.rps > 0 {
            let elapsed = mono.saturating_sub(st.refilled_at).as_secs_f64();
            let cap = cfg.bucket_capacity() as f64;
            st.tokens = (st.tokens + elapsed * cfg.rps as f64).min(cap);
            st.refilled_at = mono;
        }
        if !bypass {
            let reject = |dimension, retry_after_secs| {
                Err(Rejection {
                    dimension,
                    retry_after_secs,
                })
            };
            if cfg.requests > 0 && st.requests >= cfg.requests {
                return reject(Dimension::Requests, self.secs_to_period_end(now));
            }
            if cfg.egress_bytes > 0 && st.egress >= cfg.egress_bytes {
                return reject(Dimension::Egress, self.secs_to_period_end(now));
            }
            if cfg.rps > 0 && st.tokens < 1.0 {
                let wait = ((1.0 - st.tokens) / cfg.rps as f64).ceil().max(1.0);
                return reject(Dimension::Rps, wait as u64);
            }
            if cfg.concurrency > 0 && counters.in_flight() >= cfg.concurrency {
                return reject(Dimension::Concurrency, 1);
            }
            if stream && cfg.streams > 0 && counters.open_streams() >= cfg.streams {
                return reject(Dimension::Streams, 1);
            }
            if cfg.rps > 0 {
                st.tokens -= 1.0;
            }
        }
        st.requests = st.requests.saturating_add(1);
        let requests_remaining =
            (cfg.requests > 0).then(|| cfg.requests.saturating_sub(st.requests));
        drop(st);
        counters.last_seen.store(mono.as_secs(), Ordering::Relaxed);
        Ok(Admission {
            counters: counters.clone(),
            in_flight: Some(SlotGuard::new(counters.clone(), false)),
            stream: stream.then(|| StreamLease::new(counters.clone(), cfg.stream_lifetime)),
            requests_remaining,
        })
    }

    /// Take back the request count of an admission that was rejected late.
    fn uncount(&self, counters: &Counters) {
        let mut st = lock(&counters.state);
        st.requests = st.requests.saturating_sub(1);
    }

    /// Take a stream slot for a response that turned out to be a stream (SSE).
    fn open_stream(&self, counters: &Arc<Counters>, bypass: bool) -> Option<StreamLease> {
        if !bypass && self.cfg.streams > 0 && counters.open_streams() >= self.cfg.streams {
            return None;
        }
        Some(StreamLease::new(counters.clone(), self.cfg.stream_lifetime))
    }

    /// The `RateLimit-Policy` field value: every configured policy
    /// (draft-ietf-httpapi-ratelimit-headers, Structured Fields list).
    pub fn policy_header(&self) -> String {
        let cfg = &self.cfg;
        let window = self.period_secs();
        let mut items = Vec::new();
        if cfg.rps > 0 {
            items.push(format!(
                "\"rps\";q={};w=1;rustybin-burst={}",
                cfg.rps,
                cfg.bucket_capacity()
            ));
        }
        if cfg.concurrency > 0 {
            items.push(format!(
                "\"concurrency\";q={};qu=\"concurrent-requests\"",
                cfg.concurrency
            ));
        }
        if cfg.streams > 0 {
            items.push(format!(
                "\"streams\";q={};qu=\"concurrent-requests\"",
                cfg.streams
            ));
        }
        if cfg.requests > 0 {
            items.push(format!("\"requests\";q={};w={window}", cfg.requests));
        }
        if cfg.egress_bytes > 0 {
            items.push(format!(
                "\"egress\";q={};qu=\"content-bytes\";w={window}",
                cfg.egress_bytes
            ));
        }
        items.join(", ")
    }

    /// Length of the current period in seconds.
    fn period_secs(&self) -> u64 {
        let now = self.clock.wall();
        (self.cfg.period.end(now) - self.cfg.period.start(now))
            .num_seconds()
            .max(1) as u64
    }

    /// The human explanation of a rejection.
    fn message(&self, rejection: &Rejection) -> String {
        let cfg = &self.cfg;
        let who = match cfg.scope {
            Scope::Instance => "for this instance",
            Scope::Session => "per session (X-Rustybin-Session header, else client IP)",
        };
        let period = cfg.period.as_str();
        let what = match rejection.dimension {
            Dimension::Rps => format!(
                "allows {} requests per second (burst {}) {who}",
                cfg.rps,
                cfg.bucket_capacity()
            ),
            Dimension::Concurrency => format!(
                "allows {} requests in flight at once {who}",
                cfg.concurrency
            ),
            Dimension::Streams => {
                format!("allows {} open streams (SSE, WebSocket) {who}", cfg.streams)
            }
            Dimension::Requests => format!(
                "allows {} requests per {period} {who} and they are used up",
                cfg.requests
            ),
            Dimension::Egress => format!(
                "allows {} response bytes per {period} {who} and they are used up",
                cfg.egress_bytes
            ),
        };
        format!(
            "This 429 comes from the Rustybin plan limit, not from your gateway. The {} plan {what}; \
             retry in {} s. GET /_rustybin/usage shows the current usage.",
            cfg.plan_name(),
            rejection.retry_after_secs
        )
    }

    /// The 429 response for a rejection.
    pub fn rejection_response(&self, rejection: &Rejection) -> Response {
        let body = json!({
            "error": ERROR_CODE,
            "limit": rejection.dimension.as_str(),
            "plan": self.cfg.plan_name(),
            "scope": self.cfg.scope.as_str(),
            "retry_after_secs": rejection.retry_after_secs,
            "message": self.message(rejection),
            "usage": USAGE_PATH,
        });
        let mut resp = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
        let h = resp.headers_mut();
        h.insert(
            header::RETRY_AFTER,
            HeaderValue::from(rejection.retry_after_secs),
        );
        if let Ok(v) = HeaderValue::from_str(&self.policy_header()) {
            if !v.is_empty() {
                h.insert(HeaderName::from_static("ratelimit-policy"), v);
            }
        }
        let name = rejection.dimension.as_str();
        let item = match rejection.dimension {
            Dimension::Concurrency | Dimension::Streams => format!("\"{name}\";r=0"),
            _ => format!("\"{name}\";r=0;t={}", rejection.retry_after_secs),
        };
        if let Ok(v) = HeaderValue::from_str(&item) {
            h.insert(HeaderName::from_static("ratelimit"), v);
        }
        h.insert(
            HeaderName::from_static(LIMIT_HEADER),
            HeaderValue::from_static(name),
        );
        h.insert(
            HeaderName::from_static(PLAN_HEADER),
            HeaderValue::from_static(self.cfg.plan_name()),
        );
        resp
    }

    /// Usage of one scope as JSON (`/_rustybin/usage`).
    pub fn usage(&self, session_key: Option<&str>) -> Value {
        let cfg = &self.cfg;
        let now = self.clock.wall();
        let counters = match (cfg.scope, session_key) {
            (Scope::Session, Some(key)) => self.peek(key),
            _ => Some(self.instance.clone()),
        };
        let (requests, egress, tokens, in_flight, streams) = match &counters {
            Some(c) => {
                let mut st = lock(&c.state);
                self.roll(&mut st, now);
                let elapsed = self
                    .clock
                    .mono()
                    .saturating_sub(st.refilled_at)
                    .as_secs_f64();
                let tokens = (st.tokens + elapsed * cfg.rps as f64)
                    .min(cfg.bucket_capacity() as f64)
                    .floor() as u64;
                (
                    st.requests,
                    st.egress,
                    tokens,
                    c.in_flight(),
                    c.open_streams(),
                )
            }
            None => (0, 0, cfg.bucket_capacity(), 0, 0),
        };
        let left = |limit: u64, used: u64| {
            if limit == 0 {
                Value::Null
            } else {
                json!(limit.saturating_sub(used))
            }
        };
        let opt = |v: u64| if v == 0 { Value::Null } else { json!(v) };
        let start = cfg.period.start(now);
        let end = cfg.period.end(now);
        let mut v = json!({
            "plan": cfg.plan_name(),
            "active": cfg.active(),
            "scope": cfg.scope.as_str(),
            "limits": cfg.public_view(),
            "period": {
                "kind": cfg.period.as_str(),
                "start": start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "end": end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "resets_in_secs": (end - now).num_seconds().max(0),
            },
            "requests": { "used": requests, "limit": opt(cfg.requests), "remaining": left(cfg.requests, requests) },
            "egress": { "used_bytes": egress, "limit_bytes": opt(cfg.egress_bytes), "remaining_bytes": left(cfg.egress_bytes, egress) },
            "rate": {
                "rps": opt(cfg.rps),
                "burst": if cfg.rps > 0 { json!(cfg.bucket_capacity()) } else { Value::Null },
                "available": if cfg.rps > 0 { json!(tokens) } else { Value::Null },
            },
            "in_flight": { "current": in_flight, "limit": opt(cfg.concurrency) },
            "open_streams": { "current": streams, "limit": opt(cfg.streams), "lifetime_secs": opt(cfg.stream_lifetime.as_secs()) },
            "persisted": cfg.scope == Scope::Instance && cfg.usage_file.is_some(),
        });
        if cfg.scope == Scope::Session {
            v["session"] = json!(session_key);
        }
        v
    }

    // ── Persistence ─────────────────────────────────────────────────

    /// Restore instance quota counters from the usage file (same period only).
    fn load(&self) {
        let Some(path) = self.persist_path() else {
            return;
        };
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!("cannot read usage file {}: {e}", path.display());
                return;
            }
        };
        let Ok(saved) = serde_json::from_str::<Value>(&raw) else {
            tracing::warn!(
                "usage file {} is not valid JSON, starting from zero",
                path.display()
            );
            return;
        };
        let saved_start = saved
            .get("period_start")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));
        let same_kind =
            saved.get("period").and_then(Value::as_str) == Some(self.cfg.period.as_str());
        let mut st = lock(&self.instance.state);
        self.roll(&mut st, self.clock.wall());
        if !same_kind || saved_start != Some(st.period_start) {
            tracing::info!(
                "usage file {} is from an earlier period, starting from zero",
                path.display()
            );
            return;
        }
        st.requests = saved.get("requests").and_then(Value::as_u64).unwrap_or(0);
        st.egress = saved
            .get("egress_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        *lock(&self.persisted) = Some((st.period_start, st.requests, st.egress));
        tracing::info!(
            "restored usage from {}: {} requests, {} egress bytes",
            path.display(),
            st.requests,
            st.egress
        );
    }

    fn persist_path(&self) -> Option<&Path> {
        if !self.cfg.active() || self.cfg.scope != Scope::Instance {
            return None;
        }
        self.cfg.usage_file.as_deref()
    }

    /// Write the instance quota counters to the usage file when they changed
    /// since the last write. Errors are logged, never fatal.
    pub fn save(&self) {
        let Some(path) = self.persist_path() else {
            return;
        };
        let (start, requests, egress) = {
            let mut st = lock(&self.instance.state);
            self.roll(&mut st, self.clock.wall());
            (st.period_start, st.requests, st.egress)
        };
        let mut last = lock(&self.persisted);
        if *last == Some((start, requests, egress)) {
            return;
        }
        let body = json!({
            "version": 1,
            "plan": self.cfg.plan_name(),
            "period": self.cfg.period.as_str(),
            "period_start": start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "requests": requests,
            "egress_bytes": egress,
            "saved_at": self.clock.wall().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        });
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let written =
            std::fs::write(&tmp, format!("{body:#}\n")).and_then(|()| std::fs::rename(&tmp, path));
        match written {
            Ok(()) => *last = Some((start, requests, egress)),
            Err(e) => tracing::warn!("cannot write usage file {}: {e}", path.display()),
        }
    }

    /// Save every [`PERSIST_INTERVAL`] until `shutdown` resolves (the final
    /// save happens in `RunningServer::wait`, after connections drained).
    pub async fn persist_loop<F: Future<Output = ()>>(self: Arc<Self>, shutdown: F) {
        if self.persist_path().is_none() {
            return;
        }
        let mut tick = tokio::time::interval(PERSIST_INTERVAL);
        tick.tick().await;
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = tick.tick() => self.save(),
                _ = &mut shutdown => return,
            }
        }
    }

    // ── gRPC ────────────────────────────────────────────────────────

    /// Rate and quota check for a gRPC call (tonic interceptor). Concurrency,
    /// streams and egress are enforced on the HTTP listeners only.
    #[allow(clippy::result_large_err)]
    pub fn check_grpc(&self, req: &tonic::Request<()>) -> Result<(), tonic::Status> {
        if !self.active() {
            return Ok(());
        }
        let key = (self.cfg.scope == Scope::Session).then(|| {
            req.metadata()
                .get(crate::session::SESSION_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| crate::session::is_valid_session(s))
                .map(str::to_string)
                .unwrap_or_else(|| match req.remote_addr() {
                    Some(addr) => format!("ip:{}", addr.ip()),
                    None => "anonymous".to_string(),
                })
        });
        let counters = self.counters(key.as_deref());
        // The admission (its in-flight slot) ends with the interceptor call.
        match self.admit(&counters, false, false) {
            Ok(_) => Ok(()),
            Err(rejection) => {
                let mut status = tonic::Status::resource_exhausted(self.message(&rejection));
                let md = status.metadata_mut();
                if let Ok(v) = rejection.retry_after_secs.to_string().parse() {
                    md.insert("retry-after", v);
                }
                if let Ok(v) = rejection.dimension.as_str().parse() {
                    md.insert(LIMIT_HEADER, v);
                }
                if let Ok(v) = self.cfg.plan_name().parse() {
                    md.insert(PLAN_HEADER, v);
                }
                Err(status)
            }
        }
    }
}

/// Make room in a full session map: drop idle sets first, then the least
/// recently used tenth. Sets with requests or streams in progress stay.
fn evict(map: &mut HashMap<String, Arc<Counters>>, now: Duration) {
    let busy = |c: &Counters| c.in_flight() > 0 || c.open_streams() > 0;
    let cutoff = now.saturating_sub(SESSION_IDLE_TTL).as_secs();
    map.retain(|_, c| busy(c) || c.last_seen.load(Ordering::Relaxed) > cutoff);
    if map.len() < MAX_SESSIONS {
        return;
    }
    let mut by_age: Vec<(u64, String)> = map
        .iter()
        .filter(|(_, c)| !busy(c))
        .map(|(k, c)| (c.last_seen.load(Ordering::Relaxed), k.clone()))
        .collect();
    by_age.sort_unstable();
    let drop_n = (MAX_SESSIONS / 10).max(1);
    for (_, key) in by_age.into_iter().take(drop_n) {
        map.remove(&key);
    }
}

// ── Middleware ──────────────────────────────────────────────────────

/// Routes that are never limited, counted or decorated.
pub fn is_exempt(method: &Method, path: &str) -> bool {
    path == "/"
        || path == crate::control::UI_PREFIX
        || path
            .strip_prefix(crate::control::UI_PREFIX)
            .is_some_and(|rest| rest.starts_with('/'))
        || ((path == USAGE_PATH || path == STATUS_PATH)
            && (method == Method::GET || method == Method::HEAD))
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

fn is_event_stream(resp: &Response) -> bool {
    resp.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.trim_start()
                .get(..17)
                .is_some_and(|p| p.eq_ignore_ascii_case("text/event-stream"))
        })
}

/// The session key used for session-scoped limits.
fn scope_key(state: &AppState, req: &Request) -> Option<String> {
    (state.limits.config().scope == Scope::Session).then(|| {
        let ip = crate::session::client_ip(req.headers(), req.extensions(), &state.config);
        crate::session::session_key(req.headers(), ip)
    })
}

/// The limiter middleware (outside fault injection and inspector capture).
pub async fn enforce(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let limiter = state.limits.clone();
    if !limiter.active() || is_exempt(req.method(), req.uri().path()) {
        return next.run(req).await;
    }
    let key = scope_key(&state, &req);
    let counters = limiter.counters(key.as_deref());
    let bypass = state.config.admin_token.is_some()
        && req.uri().path().starts_with(crate::control::CONTROL_PREFIX)
        && crate::admin::is_admin(req.headers(), &state.config);
    let websocket = is_websocket_upgrade(req.headers());
    let mut admission = match limiter.admit(&counters, bypass, websocket) {
        Ok(a) => a,
        Err(rejection) => return limiter.rejection_response(&rejection),
    };
    if let Some(lease) = &admission.stream {
        req.extensions_mut().insert(lease.clone());
    }
    let mut resp = next.run(req).await;

    let (in_flight, lease) = if resp.status() == StatusCode::SWITCHING_PROTOCOLS {
        // The WebSocket handler holds its own copy of the lease.
        (None, None)
    } else if is_event_stream(&resp) {
        match limiter.open_stream(&admission.counters, bypass) {
            Some(lease) => (None, Some(lease)),
            None => {
                // Rejected after all: the request does not count.
                limiter.uncount(&admission.counters);
                let rejection = Rejection {
                    dimension: Dimension::Streams,
                    retry_after_secs: 1,
                };
                return limiter.rejection_response(&rejection);
            }
        }
    } else {
        (admission.in_flight.take(), None)
    };

    let h = resp.headers_mut();
    h.insert(
        HeaderName::from_static(PLAN_HEADER),
        HeaderValue::from_static(limiter.config().plan_name()),
    );
    if let Some(left) = admission.requests_remaining {
        h.insert(
            HeaderName::from_static(QUOTA_REMAINING_HEADER),
            HeaderValue::from(left),
        );
    }
    let (parts, body) = resp.into_parts();
    let deadline = lease
        .as_ref()
        .and_then(StreamLease::deadline)
        .map(|at| Box::pin(tokio::time::sleep_until(at)));
    let metered = MeteredBody {
        inner: body,
        counters: admission.counters.clone(),
        _in_flight: in_flight,
        _lease: lease,
        deadline,
        ended: false,
    };
    Response::from_parts(parts, Body::new(metered))
}

/// Response body wrapper: counts egress bytes, holds the in-flight or stream
/// slot until the body ends, and ends a stream at its lifetime deadline.
struct MeteredBody {
    inner: Body,
    counters: Arc<Counters>,
    _in_flight: Option<SlotGuard>,
    _lease: Option<StreamLease>,
    deadline: Option<Pin<Box<tokio::time::Sleep>>>,
    ended: bool,
}

impl MeteredBody {
    fn finish(&mut self) {
        self.ended = true;
        self.deadline = None;
        self._in_flight = None;
        self._lease = None;
        self.inner = Body::empty();
    }
}

impl HttpBody for MeteredBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        if this.ended {
            return Poll::Ready(None);
        }
        if let Some(deadline) = this.deadline.as_mut() {
            if deadline.as_mut().poll(cx).is_ready() {
                this.finish();
                this.counters.add_egress(STREAM_END_COMMENT.len());
                let note = Bytes::from_static(STREAM_END_COMMENT);
                return Poll::Ready(Some(Ok(http_body::Frame::data(note))));
            }
        }
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.counters.add_egress(data.len());
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(None) => {
                this.finish();
                Poll::Ready(None)
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.ended || self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        if self.ended {
            http_body::SizeHint::with_exact(0)
        } else {
            self.inner.size_hint()
        }
    }
}

// ── Usage endpoint ──────────────────────────────────────────────────

async fn usage_handler(
    State(state): State<AppState>,
    crate::session::Session(key): crate::session::Session,
) -> Response {
    let limiter = &state.limits;
    let key = (limiter.config().scope == Scope::Session).then_some(key);
    Json(limiter.usage(key.as_deref())).into_response()
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new().route(USAGE_PATH, get(usage_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![Endpoint::new(
        USAGE_PATH,
        &["GET"],
        category::CONTROL,
        "Plan, limits and current usage (requests, egress, in flight, streams)",
    )
    .description(
        "Never limited. With session scope (the free plan) it reports the caller's own \
         counters (X-Rustybin-Session header, else client IP). Plan none reports active false.",
    )
    .example(Example::get("Plan and usage", USAGE_PATH))]
}

pub fn openapi_paths() -> Value {
    let nullable_int = json!({ "type": ["integer", "null"] });
    json!({
        USAGE_PATH: {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Plan limits and current usage",
                "description": "The active plan (RUSTYBIN_PLAN), its limits and the usage of the current period. Session scope reports only the caller's own counters. Exempt from every limit.",
                "operationId": "getRustybinUsage",
                "responses": { "200": { "description": "Usage", "content": { "application/json": { "schema": {
                    "type": "object",
                    "properties": {
                        "plan": { "type": "string", "enum": ["none", "free", "pro", "team", "enterprise", "custom"] },
                        "active": { "type": "boolean" },
                        "scope": { "type": "string", "enum": ["instance", "session"] },
                        "session": { "type": "string", "description": "Session scope only: the caller's session key" },
                        "limits": { "type": "object" },
                        "period": { "type": "object", "properties": {
                            "kind": { "type": "string", "enum": ["day", "month"] },
                            "start": { "type": "string", "format": "date-time" },
                            "end": { "type": "string", "format": "date-time" },
                            "resets_in_secs": { "type": "integer" }
                        } },
                        "requests": { "type": "object", "properties": {
                            "used": { "type": "integer" }, "limit": nullable_int, "remaining": nullable_int
                        } },
                        "egress": { "type": "object", "properties": {
                            "used_bytes": { "type": "integer" }, "limit_bytes": nullable_int, "remaining_bytes": nullable_int
                        } },
                        "rate": { "type": "object" },
                        "in_flight": { "type": "object" },
                        "open_streams": { "type": "object" },
                        "persisted": { "type": "boolean" }
                    }
                } } } } }
            }
        }
    })
}

#[cfg(test)]
mod tests;
