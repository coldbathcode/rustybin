//! The audit log: who did what, and when (testing-2 step 14).
//!
//! An [`Entry`] is the time, the actor (a user's id and name, or the
//! credential's kind and fingerprint, plus the agent when an agent made the
//! call), the action (a route template and method, or a management tool),
//! the target names (a mock, a user, a setting key), the result (the status,
//! or the refusal's reason and permission), the request id and the client
//! address. Never a value, a body, a header, a password or a token.
//!
//! What writes one:
//! - [`layer`] (a route layer in `build_app`, outside the permission check):
//!   every call of a mutating control route (`POST`, `PUT`, `PATCH`,
//!   `DELETE` under `/_rustybin/`, but [`NOT_RECORDED`]), every refusal
//!   naming a permission (`403 permission_required`, on any route), and any
//!   request whose handler named an event ([`event`]: a sign-in, the single
//!   sign-on callback, a portal sign-in);
//! - `mcp::manage::tools::call`: every management tool that changes
//!   something, and every refused tool call;
//! - [`Audit::system`]: what the instance does on its own (the automatic
//!   git pulls).
//!
//! So a new route or tool is recorded without code. A handler or a service
//! adds what only it knows (the user a sign-in names, the keys of a settings
//! change, the groups a single sign-on mapped, the commit a pull reached)
//! through [`target`], [`event`] and [`actor_user`], which write into the
//! current request's note (a task-local; outside a request they do nothing).
//!
//! Bounded: a ring of [`MAX_ENTRIES`] in memory and, with a state directory,
//! `<state>/audit/audit-YYYY-MM-DD.jsonl` (UTC days) written off the runtime
//! by one thread through a bounded channel (an entry that does not fit is
//! counted in `not_written`, never waited for), kept `audit.retention_days`
//! days and at most `audit.max_bytes` bytes (the oldest days go first). The
//! entries of callers without a credential are capped per minute
//! ([`ANONYMOUS_PER_MINUTE`]) so nobody can flush the ring from outside.
//! `GET /_rustybin/audit` and `/_rustybin/audit/stream` (`audit.read`).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{MatchedPath, Query, RawPathParams, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::catalog::{category, Endpoint, Example};
use crate::limits::Clock;
use crate::rbac::{Permission, Principal, Resolved};
use crate::settings::{Kind, Settings, Spec};
use crate::state::AppState;

/// Entries kept in memory.
pub const MAX_ENTRIES: usize = 10_000;
/// Entries waiting for the file writer; past it an entry stays in memory
/// only (counted in `not_written`).
pub const CHANNEL: usize = 4_096;
/// Target names per entry, and the longest name.
pub const MAX_TARGETS: usize = 16;
pub const MAX_NAME: usize = 128;
/// Live listeners of `/_rustybin/audit/stream`.
pub const MAX_LISTENERS: usize = 16;
/// Entries of callers without a credential, per minute (instance-wide).
pub const ANONYMOUS_PER_MINUTE: u64 = 600;
/// A page of `GET /_rustybin/audit`.
pub const DEFAULT_LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 500;
/// What startup reads back from the newest file, at most.
const MAX_TAIL_READ: u64 = 32 * 1024 * 1024;

pub const RETENTION_KEY: &str = "audit.retention_days";
pub const MAX_BYTES_KEY: &str = "audit.max_bytes";
pub const DEFAULT_RETENTION_DAYS: i64 = 30;
pub const MAX_RETENTION_DAYS: i64 = 365;
pub const DEFAULT_MAX_BYTES: i64 = 256 * 1024 * 1024;
const MIN_MAX_BYTES: i64 = 64 * 1024;
const MAX_MAX_BYTES: i64 = 1024 * 1024 * 1024 * 1024;

pub const LIST_PATH: &str = "/_rustybin/audit";
pub const STREAM_PATH: &str = "/_rustybin/audit/stream";

/// Mutating control routes that are not entries of their own, and why.
/// Every other `POST`, `PUT`, `PATCH` and `DELETE` under `/_rustybin/` is
/// (`every_mutating_route_and_tool_writes_one_entry` walks the catalogue).
pub const NOT_RECORDED: &[(&str, &str, &str)] = &[
    (
        "*",
        "/_rustybin/mcp",
        "the management MCP transport: each tool call that changes something is an entry of its own",
    ),
    (
        "POST",
        "/_rustybin/harness/clusters/{id}/nodes",
        "a cluster member joining its leader (harness node traffic, its own credential)",
    ),
    (
        "POST",
        "/_rustybin/harness/clusters/{id}/nodes/{node}/heartbeat",
        "harness node heartbeats, every second",
    ),
    (
        "POST",
        "/_rustybin/harness/clusters/{id}/election/vote",
        "harness node traffic: a leader election",
    ),
    (
        "POST",
        "/_rustybin/harness/clusters/{id}/election/elected",
        "harness node traffic: a leader election",
    ),
    (
        "POST",
        "/_rustybin/harness/preview",
        "computes a scenario's stages and sends nothing (harness.read)",
    ),
    (
        "POST",
        "/_rustybin/harness/validate",
        "validates a scenario and sends nothing (harness.read)",
    ),
    (
        "POST",
        "/_rustybin/mocks/{name}/operations/{op}/validate",
        "validates a payload, changes nothing (mocks.read)",
    ),
    (
        "POST",
        "/_rustybin/mocks/{name}/operations/{op}/rules/test",
        "evaluates rules against a request, changes nothing (mocks.read)",
    ),
    (
        "POST",
        "/_rustybin/mocks/import",
        "parses a spec without creating anything (mocks.read)",
    ),
    (
        "POST",
        "/_rustybin/registry/validate",
        "validates a server.json, changes nothing (instance.read)",
    ),
];

/// Is `method template` one of [`NOT_RECORDED`]?
pub fn not_recorded(method: &str, template: &str) -> bool {
    NOT_RECORDED
        .iter()
        .any(|(m, t, _)| *t == template && (*m == "*" || m.eq_ignore_ascii_case(method)))
}

// ── The model ───────────────────────────────────────────────────────

/// Who did it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Actor {
    /// `user`, `admin_token`, `token` (a control JWT), `key` (an API key in
    /// shared mode), `visit` (a self-service visitor), `open` (an open
    /// control plane), `anonymous` (no credential) or `system` (the
    /// instance itself).
    pub kind: String,
    /// The user's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The user's sign-in name, a token's subject, the key's id, the
    /// visit's fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The AI agent that made the call (its id on Management, AI agents).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

impl Actor {
    fn kind(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
            ..Self::default()
        }
    }

    /// The instance itself (the automatic git mode).
    pub fn system() -> Self {
        Self::kind("system")
    }

    /// A user of the store.
    pub fn user(id: &str, name: &str) -> Self {
        Self {
            kind: "user".into(),
            id: Some(clean(id)),
            name: Some(clean(name)),
            agent: None,
        }
    }

    /// The caller a request or a tool call resolved to.
    pub fn of(r: &Resolved) -> Self {
        match &r.principal {
            Principal::User { id, name, .. } => Self::user(id, name),
            Principal::AdminToken => Self::kind("admin_token"),
            Principal::Token { sub } => Self {
                name: sub.as_deref().map(clean),
                ..Self::kind("token")
            },
            Principal::Key { id } => Self {
                name: Some(clean(id)),
                ..Self::kind("key")
            },
            Principal::Open => Self::kind("open"),
            Principal::Anonymous { .. } => Self::kind("anonymous"),
        }
    }

    /// A self-service visitor, by the visit's fingerprint.
    pub fn visit(fingerprint: &str) -> Self {
        Self {
            name: Some(clean(fingerprint)),
            ..Self::kind("visit")
        }
    }

    fn anonymous(&self) -> bool {
        self.kind == "anonymous"
    }

    /// The words the `actor` filter looks in.
    fn words(&self) -> [Option<&str>; 4] {
        [
            Some(self.kind.as_str()),
            self.id.as_deref(),
            self.name.as_deref(),
            self.agent.as_deref(),
        ]
    }
}

/// A name the action was about.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Target {
    /// `mock`, `user`, `role`, `setting`, `project`, `test`, `agent`,
    /// `group`, `commit`, ... (a route's path parameters are named after
    /// the segment before them).
    pub kind: String,
    pub name: String,
}

/// How it ended.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Outcome {
    /// The HTTP status (a route), absent for a tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// `ok`, `error`, `refused` (a permission, a revoked agent) or `asked`
    /// (a tool waiting for the user's confirmation).
    pub outcome: String,
    /// The refusal's or the error's reason, when the answer named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The permission a refusal named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
}

impl Outcome {
    /// The outcome of an HTTP status.
    pub fn of_status(status: u16) -> Self {
        Self {
            status: Some(status),
            outcome: if status < 400 { "ok" } else { "error" }.into(),
            ..Self::default()
        }
    }
}

/// One line of the audit log.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    /// Increasing (also across restarts with a state directory): the cursor.
    pub id: u64,
    pub time: DateTime<Utc>,
    pub actor: Actor,
    /// `api` (a route), `mcp` (a management tool) or `system`.
    pub via: String,
    /// `POST /_rustybin/mocks/{name}/deploy`, or the tool's name.
    pub action: String,
    /// What a handler said happened (`login`, `invite`, `sso`, `revoke_agent`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(default)]
    pub targets: Vec<Target>,
    pub result: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// The client's address (as the sign-in throttle sees it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// An entry before it gets its id and time.
#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub actor: Actor,
    pub via: &'static str,
    pub action: String,
    pub event: Option<String>,
    pub targets: Vec<Target>,
    pub result: Outcome,
    pub request_id: Option<String>,
    pub address: Option<String>,
}

/// A name as an entry keeps it: printable, at most [`MAX_NAME`] characters.
fn clean(s: &str) -> String {
    crate::logging::printable(s.trim(), MAX_NAME)
}

// ── The current request's note ──────────────────────────────────────

/// What a handler adds to its request's entry.
#[derive(Clone, Debug, Default)]
pub struct Note {
    pub targets: Vec<Target>,
    pub event: Option<String>,
    pub actor: Option<Actor>,
}

impl Note {
    fn add(&mut self, kind: &str, name: &str) {
        let name = clean(name);
        if name.is_empty() || self.targets.len() >= MAX_TARGETS {
            return;
        }
        let kind = clean(kind);
        if !self
            .targets
            .iter()
            .any(|t| t.kind == kind && t.name == name)
        {
            self.targets.push(Target { kind, name });
        }
    }
}

tokio::task_local! {
    static NOTE: RefCell<Note>;
}

/// Run `f` with a fresh note; answers its output and what was noted.
pub async fn scoped<F: std::future::Future>(f: F) -> (F::Output, Note) {
    NOTE.scope(RefCell::new(Note::default()), async move {
        let out = f.await;
        let note = NOTE
            .try_with(|n| std::mem::take(&mut *n.borrow_mut()))
            .unwrap_or_default();
        (out, note)
    })
    .await
}

/// Run a blocking closure (`spawn_blocking`: the compile pool, a document's
/// apply) with a note of its own, answered beside its output so the async
/// side can [`merge`] it into the request's.
pub fn carried<T>(f: impl FnOnce() -> T) -> (T, Note) {
    NOTE.sync_scope(RefCell::new(Note::default()), || {
        let out = f();
        let note = NOTE
            .try_with(|n| std::mem::take(&mut *n.borrow_mut()))
            .unwrap_or_default();
        (out, note)
    })
}

/// Add what a blocking closure noted ([`carried`]) to the request's note.
pub fn merge(note: Note) {
    let _ = NOTE.try_with(|n| {
        let mut n = n.borrow_mut();
        for t in note.targets {
            n.add(&t.kind, &t.name);
        }
        if note.event.is_some() {
            n.event = note.event;
        }
        if note.actor.is_some() {
            n.actor = note.actor;
        }
    });
}

/// Inside a request: a name the action is about (`kind` as the console
/// shows it: `mock`, `user`, `setting`, ...). Never a value.
pub fn target(kind: &str, name: &str) {
    let _ = NOTE.try_with(|n| n.borrow_mut().add(kind, name));
}

/// Inside a request: what happened (`login`, `invite`, ...). A request
/// that names an event is an entry whatever its method.
pub fn event(name: &str) {
    let _ = NOTE.try_with(|n| n.borrow_mut().event = Some(clean(name)));
}

/// Inside a request: the user a sign-in names (the caller is nobody yet).
/// Only for a user that exists: a name nobody has is never recorded (it may
/// be a password typed in the wrong field).
pub fn actor_user(id: &str, name: &str) {
    let _ = NOTE.try_with(|n| n.borrow_mut().actor = Some(Actor::user(id, name)));
}

/// Is a note being written (a request or a tool call)?
pub fn in_scope() -> bool {
    NOTE.try_with(|_| ()).is_ok()
}

// ── The store ───────────────────────────────────────────────────────

enum Msg {
    Entry(Arc<Entry>),
    Flush(SyncSender<()>),
}

/// What the file writer reports.
#[derive(Clone, Debug, Default)]
struct FileStats {
    files: usize,
    bytes: u64,
    oldest: Option<NaiveDate>,
    error: Option<String>,
}

/// The audit log (`AppState.audit`).
pub struct Audit {
    ring: Mutex<VecDeque<Arc<Entry>>>,
    next: AtomicU64,
    feed: broadcast::Sender<Arc<Entry>>,
    listeners: Arc<AtomicUsize>,
    writer: Option<SyncSender<Msg>>,
    dir: Option<PathBuf>,
    stats: Arc<Mutex<FileStats>>,
    /// Entries the writer could not take (its channel full).
    not_written: Arc<AtomicU64>,
    /// Entries of callers without a credential past the per-minute cap.
    suppressed: AtomicU64,
    anonymous: Mutex<(i64, u64)>,
    clock: Arc<dyn Clock>,
    settings: Arc<Settings>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Audit {
    /// The log of an instance: in memory, plus `<state>/audit/` with a
    /// state directory (the newest entries are read back into memory).
    pub fn new(state_dir: Option<&Path>, settings: Arc<Settings>, clock: Arc<dyn Clock>) -> Self {
        let (feed, _) = broadcast::channel(256);
        let dir = state_dir.map(|d| d.join("audit"));
        let stats = Arc::new(Mutex::new(FileStats::default()));
        let not_written = Arc::new(AtomicU64::new(0));
        let mut ring = VecDeque::new();
        let mut writer = None;
        if let Some(dir) = &dir {
            match prepare_dir(dir) {
                Ok(()) => {
                    ring = read_back(dir, MAX_ENTRIES);
                    let (tx, rx) = sync_channel(CHANNEL);
                    let w = Writer {
                        dir: dir.clone(),
                        settings: settings.clone(),
                        clock: clock.clone(),
                        stats: stats.clone(),
                        not_written: not_written.clone(),
                        current: None,
                    };
                    match std::thread::Builder::new()
                        .name("rustybin-audit".into())
                        .spawn(move || w.run(rx))
                    {
                        Ok(_) => writer = Some(tx),
                        Err(e) => {
                            tracing::warn!(target: "rustybin::audit", error = %e, "the audit writer could not start: the log is kept in memory only");
                            lock(&stats).error = Some(e.to_string());
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "rustybin::audit", dir = %dir.display(), error = %e, "the audit directory could not be created: the log is kept in memory only");
                    lock(&stats).error = Some(e);
                }
            }
        }
        let next = ring.back().map(|e: &Arc<Entry>| e.id + 1).unwrap_or(1);
        Self {
            ring: Mutex::new(ring),
            next: AtomicU64::new(next),
            feed,
            listeners: Arc::new(AtomicUsize::new(0)),
            writer,
            dir,
            stats,
            not_written,
            suppressed: AtomicU64::new(0),
            anonymous: Mutex::new((0, 0)),
            clock,
            settings,
        }
    }

    /// Record an entry. `None` when it was suppressed (a caller without a
    /// credential past the per-minute cap).
    pub fn record(&self, d: Draft) -> Option<Arc<Entry>> {
        let now = self.clock.wall();
        if d.actor.anonymous() {
            let minute = now.timestamp() / 60;
            let mut a = lock(&self.anonymous);
            if a.0 != minute {
                *a = (minute, 0);
            }
            if a.1 >= ANONYMOUS_PER_MINUTE {
                drop(a);
                self.suppressed.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            a.1 += 1;
        }
        let entry = {
            let mut ring = lock(&self.ring);
            let entry = Arc::new(Entry {
                id: self.next.fetch_add(1, Ordering::Relaxed),
                time: now,
                actor: d.actor,
                via: d.via.to_string(),
                action: d.action,
                event: d.event,
                targets: d.targets,
                result: d.result,
                request_id: d.request_id.map(|r| crate::logging::printable(&r, 64)),
                address: d.address,
            });
            ring.push_back(entry.clone());
            while ring.len() > MAX_ENTRIES {
                ring.pop_front();
            }
            entry
        };
        let _ = self.feed.send(entry.clone());
        if let Some(w) = &self.writer {
            match w.try_send(Msg::Entry(entry.clone())) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                    self.not_written.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        Some(entry)
    }

    /// What the instance did on its own (the automatic git mode).
    pub fn system(&self, action: &str, targets: Vec<Target>, result: Outcome) {
        self.record(Draft {
            actor: Actor::system(),
            via: "system",
            action: action.to_string(),
            targets,
            result,
            ..Draft::default()
        });
    }

    /// Wait (up to 5 s) until the writer wrote what it was given (tests,
    /// shutdown).
    pub fn flush(&self) {
        let Some(w) = &self.writer else {
            return;
        };
        let (tx, rx) = sync_channel(1);
        if w.send(Msg::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(5));
        }
    }

    /// The entries `q` asks for, newest first, and the cursor of the next
    /// page.
    pub fn list(&self, q: &Filter) -> (Vec<Arc<Entry>>, Option<u64>) {
        let limit = q.limit.clamp(1, MAX_LIMIT);
        let ring = lock(&self.ring);
        let mut out = Vec::new();
        for e in ring.iter().rev() {
            if q.cursor.is_some_and(|c| e.id >= c) {
                continue;
            }
            if !q.matches(e) {
                continue;
            }
            if out.len() == limit {
                let next = out.last().map(|l: &Arc<Entry>| l.id);
                return (out, next);
            }
            out.push(e.clone());
        }
        (out, None)
    }

    /// Entries held in memory.
    pub fn len(&self) -> usize {
        lock(&self.ring).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every entry in memory, oldest first (tests).
    pub fn all(&self) -> Vec<Arc<Entry>> {
        lock(&self.ring).iter().cloned().collect()
    }

    /// The live feed (`None` past [`MAX_LISTENERS`]); the guard holds the
    /// listener's place.
    pub fn subscribe(&self) -> Option<(broadcast::Receiver<Arc<Entry>>, ListenerGuard)> {
        let n = self.listeners.fetch_add(1, Ordering::SeqCst);
        if n >= MAX_LISTENERS {
            self.listeners.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some((self.feed.subscribe(), ListenerGuard(self.listeners.clone())))
    }

    /// The log's state: what is kept where, the bounds, what was lost.
    pub fn status(&self) -> Value {
        let stats = lock(&self.stats).clone();
        let (oldest, newest) = {
            let ring = lock(&self.ring);
            (ring.front().map(|e| e.id), ring.back().map(|e| e.id))
        };
        json!({
            "held": self.len(),
            "capacity": MAX_ENTRIES,
            "oldest_id": oldest,
            "newest_id": newest,
            "persisted": self.writer.is_some(),
            "directory": self.dir.as_ref().map(|_| "<state>/audit"),
            "files": stats.files,
            "bytes": stats.bytes,
            "oldest_day": stats.oldest.map(|d| d.to_string()),
            "write_error": stats.error,
            "retention_days": retention_days(&self.settings),
            "max_bytes": max_bytes(&self.settings),
            "not_written": self.not_written.load(Ordering::Relaxed),
            "suppressed": self.suppressed.load(Ordering::Relaxed),
        })
    }
}

/// Holds a live listener's place.
pub struct ListenerGuard(Arc<AtomicUsize>);

impl Drop for ListenerGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn retention_days(settings: &Settings) -> i64 {
    settings
        .get(RETENTION_KEY)
        .0
        .as_i64()
        .unwrap_or(DEFAULT_RETENTION_DAYS)
        .clamp(1, MAX_RETENTION_DAYS)
}

pub fn max_bytes(settings: &Settings) -> u64 {
    settings
        .get(MAX_BYTES_KEY)
        .0
        .as_i64()
        .unwrap_or(DEFAULT_MAX_BYTES)
        .clamp(MIN_MAX_BYTES, MAX_MAX_BYTES) as u64
}

// ── Files ───────────────────────────────────────────────────────────

fn prepare_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// `audit-YYYY-MM-DD.jsonl`.
pub fn file_name(day: NaiveDate) -> String {
    format!("audit-{}.jsonl", day.format("%Y-%m-%d"))
}

fn day_of(name: &str) -> Option<NaiveDate> {
    let d = name.strip_prefix("audit-")?.strip_suffix(".jsonl")?;
    NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()
}

/// The log's files, oldest first, with their sizes.
fn files(dir: &Path) -> Vec<(NaiveDate, PathBuf, u64)> {
    let mut out: Vec<(NaiveDate, PathBuf, u64)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let day = day_of(&name)?;
            let size = e.metadata().ok()?.len();
            Some((day, e.path(), size))
        })
        .collect();
    out.sort_by_key(|f| f.0);
    out
}

/// The newest `max` entries of the files (startup): from the newest file
/// back, at most [`MAX_TAIL_READ`] bytes of each.
fn read_back(dir: &Path, max: usize) -> VecDeque<Arc<Entry>> {
    let mut found: Vec<Arc<Entry>> = Vec::new();
    for (_, path, size) in files(dir).into_iter().rev() {
        let Ok(mut f) = std::fs::File::open(&path) else {
            continue;
        };
        let skip = size.saturating_sub(MAX_TAIL_READ);
        if skip > 0 && f.seek(SeekFrom::Start(skip)).is_err() {
            continue;
        }
        let mut reader = std::io::BufReader::new(f.take(MAX_TAIL_READ));
        let mut lines: Vec<Arc<Entry>> = Vec::new();
        let mut line = String::new();
        let mut first = skip > 0;
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if std::mem::take(&mut first) {
                continue; // a partial line where the read started
            }
            if let Ok(e) = serde_json::from_str::<Entry>(line.trim_end()) {
                lines.push(Arc::new(e));
            }
        }
        lines.extend(found);
        found = lines;
        if found.len() >= max {
            break;
        }
    }
    found.sort_by_key(|e| e.id);
    let start = found.len().saturating_sub(max);
    found.drain(start..).collect()
}

/// The file writer (one thread): appends each entry to its day's file,
/// deletes the days past the retention and, past `audit.max_bytes`, the
/// oldest days.
struct Writer {
    dir: PathBuf,
    settings: Arc<Settings>,
    clock: Arc<dyn Clock>,
    stats: Arc<Mutex<FileStats>>,
    not_written: Arc<AtomicU64>,
    current: Option<(NaiveDate, std::fs::File)>,
}

impl Writer {
    fn run(mut self, rx: Receiver<Msg>) {
        self.prune();
        while let Ok(msg) = rx.recv() {
            match msg {
                Msg::Entry(e) => self.write(&e),
                Msg::Flush(ack) => {
                    if let Some((_, f)) = &mut self.current {
                        let _ = f.flush();
                    }
                    self.prune();
                    let _ = ack.send(());
                }
            }
        }
    }

    fn error(&self, e: impl std::fmt::Display) {
        let mut s = lock(&self.stats);
        let text = e.to_string();
        if s.error.as_deref() != Some(text.as_str()) {
            tracing::warn!(target: "rustybin::audit", error = %text, "the audit log could not be written");
        }
        s.error = Some(text);
    }

    fn write(&mut self, e: &Entry) {
        let day = e.time.date_naive();
        if self.current.as_ref().map(|c| c.0) != Some(day) {
            self.current = None;
            let path = self.dir.join(file_name(day));
            let mut o = std::fs::OpenOptions::new();
            o.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600);
            }
            match o.open(&path) {
                Ok(f) => self.current = Some((day, f)),
                Err(err) => {
                    self.not_written.fetch_add(1, Ordering::Relaxed);
                    return self.error(err);
                }
            }
            // A new day: the retention applies.
            self.prune();
        }
        let Ok(mut line) = serde_json::to_string(e) else {
            return;
        };
        line.push('\n');
        let len = line.len() as u64;
        let cap = max_bytes(&self.settings);
        if lock(&self.stats).bytes + len > cap {
            self.prune_bytes(cap.saturating_sub(len), Some(day));
            if lock(&self.stats).bytes + len > cap {
                // Today's file alone is at the cap: kept in memory only.
                self.not_written.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        let Some((_, f)) = &mut self.current else {
            return;
        };
        match f.write_all(line.as_bytes()) {
            Ok(()) => {
                let mut s = lock(&self.stats);
                s.bytes += len;
                if s.files == 0 {
                    s.files = 1;
                    s.oldest = Some(day);
                }
                s.error = None;
            }
            Err(err) => {
                self.not_written.fetch_add(1, Ordering::Relaxed);
                self.error(err);
            }
        }
    }

    /// Delete the days past the retention and, past the byte cap, the
    /// oldest days (never the one being written); then count what is left.
    fn prune(&mut self) {
        let today = self.clock.wall().date_naive();
        let keep = retention_days(&self.settings);
        let first_kept = today - chrono::Duration::days(keep - 1);
        for (day, path, _) in files(&self.dir) {
            if day < first_kept {
                match std::fs::remove_file(&path) {
                    Ok(()) => {
                        tracing::info!(target: "rustybin::audit", day = %day, "an audit file past the retention was deleted")
                    }
                    Err(e) => self.error(e),
                }
            }
        }
        let current = self.current.as_ref().map(|c| c.0);
        self.prune_bytes(max_bytes(&self.settings), current);
    }

    fn prune_bytes(&mut self, cap: u64, keep: Option<NaiveDate>) {
        let mut all = files(&self.dir);
        let mut total: u64 = all.iter().map(|f| f.2).sum();
        while total > cap {
            let Some(pos) = all.iter().position(|f| Some(f.0) != keep) else {
                break;
            };
            let (day, path, size) = all.remove(pos);
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    tracing::info!(target: "rustybin::audit", day = %day, "the oldest audit file was deleted (audit.max_bytes)");
                    total -= size;
                }
                Err(e) => {
                    self.error(e);
                    break;
                }
            }
        }
        let mut s = lock(&self.stats);
        s.files = all.len();
        s.bytes = total;
        s.oldest = all.first().map(|f| f.0);
    }
}

// ── Filters ─────────────────────────────────────────────────────────

/// What `GET /_rustybin/audit`, the stream, the tool and the CLI filter by.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// Contained in the actor's kind, user id, name or agent (any case).
    pub actor: Option<String>,
    /// Contained in the action or the event (any case).
    pub action: Option<String>,
    /// Contained in a target's name; `kind:name` for one kind.
    pub target: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// `ok`, `error`, `refused`, `asked`, or a status code.
    pub result: Option<String>,
    /// Entries older than this id (the previous page's `next_cursor`).
    pub cursor: Option<u64>,
    pub limit: usize,
}

/// The query of `GET /_rustybin/audit` and its stream.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct FilterQuery {
    pub actor: Option<String>,
    pub action: Option<String>,
    pub target: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub result: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<String>,
}

fn contains(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

/// A time: RFC 3339, a UTC day (`2026-11-03`), or how long ago (`15m`,
/// `2h`, `7d`, `30s`).
pub fn parse_time(raw: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let raw = raw.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(raw) {
        return Ok(t.with_timezone(&Utc));
    }
    if let Ok(d) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        if let Some(t) = d.and_hms_opt(0, 0, 0) {
            return Ok(t.and_utc());
        }
    }
    let unit = raw.chars().last().unwrap_or(' ');
    let n: Option<i64> = raw
        .get(..raw.len().saturating_sub(1))
        .and_then(|n| n.parse().ok());
    let secs = match (n, unit) {
        (Some(n), 's') => Some(n),
        (Some(n), 'm') => n.checked_mul(60),
        (Some(n), 'h') => n.checked_mul(3600),
        (Some(n), 'd') => n.checked_mul(86_400),
        _ => None,
    };
    secs.filter(|s| *s >= 0 && *s <= 400 * 86_400)
        .map(|s| now - chrono::Duration::seconds(s))
        .ok_or_else(|| {
            format!("{raw:?} is not a time: RFC 3339, a day (2026-11-03) or an age (15m, 2h, 7d)")
        })
}

impl Filter {
    /// From the query (or the tool's arguments).
    pub fn parse(q: &FilterQuery, now: DateTime<Utc>) -> Result<Self, String> {
        let text = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(MAX_NAME).collect::<String>())
        };
        let time = |v: &Option<String>| -> Result<Option<DateTime<Utc>>, String> {
            match text(v) {
                Some(s) => parse_time(&s, now).map(Some),
                None => Ok(None),
            }
        };
        let result = text(&q.result);
        if let Some(r) = &result {
            if !matches!(r.as_str(), "ok" | "error" | "refused" | "asked")
                && r.parse::<u16>().is_err()
            {
                return Err(format!(
                    "result {r:?}: ok, error, refused, asked or a status code"
                ));
            }
        }
        let cursor = match text(&q.cursor) {
            Some(c) => Some(
                c.parse::<u64>()
                    .map_err(|_| format!("cursor {c:?}: the next_cursor of a previous page"))?,
            ),
            None => None,
        };
        let limit = match text(&q.limit) {
            Some(l) => l
                .parse::<usize>()
                .map_err(|_| format!("limit {l:?}: a number from 1 to {MAX_LIMIT}"))?,
            None => DEFAULT_LIMIT,
        };
        Ok(Self {
            actor: text(&q.actor),
            action: text(&q.action),
            target: text(&q.target),
            since: time(&q.since)?,
            until: time(&q.until)?,
            result,
            cursor,
            limit: limit.clamp(1, MAX_LIMIT),
        })
    }

    pub fn matches(&self, e: &Entry) -> bool {
        if let Some(a) = &self.actor {
            if !e.actor.words().iter().flatten().any(|w| contains(w, a)) {
                return false;
            }
        }
        if let Some(a) = &self.action {
            if !contains(&e.action, a) && !e.event.as_deref().is_some_and(|ev| contains(ev, a)) {
                return false;
            }
        }
        if let Some(t) = &self.target {
            let (kind, name) = match t.split_once(':') {
                Some((k, n)) if !k.is_empty() && k.chars().all(|c| c.is_ascii_lowercase()) => {
                    (Some(k), n)
                }
                _ => (None, t.as_str()),
            };
            if !e
                .targets
                .iter()
                .any(|x| kind.is_none_or(|k| x.kind == k) && contains(&x.name, name))
            {
                return false;
            }
        }
        if self.since.is_some_and(|s| e.time < s) || self.until.is_some_and(|u| e.time > u) {
            return false;
        }
        if let Some(r) = &self.result {
            let ok = match r.parse::<u16>() {
                Ok(code) => e.result.status == Some(code),
                Err(_) => e.result.outcome == *r,
            };
            if !ok {
                return false;
            }
        }
        true
    }
}

// ── The layer ───────────────────────────────────────────────────────

/// A method that changes something.
pub fn is_mutating(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// The kind of a path parameter's target: the segment before it, singular
/// (`/_rustybin/mocks/{name}` is a `mock`, `/_rustybin/users/{id}` a
/// `user`), else the parameter's name.
pub fn param_kind(template: &str, param: &str) -> String {
    let segs: Vec<&str> = template.split('/').collect();
    let me = format!("{{{param}}}");
    let wild = format!("{{*{param}}}");
    let before = segs
        .iter()
        .position(|s| *s == me || *s == wild)
        .and_then(|i| i.checked_sub(1))
        .map(|i| segs[i])
        .filter(|s| !s.is_empty() && !s.starts_with('{') && *s != "_rustybin");
    match before {
        Some(s) if s.ends_with("ss") => s.to_string(),
        Some(s) => s.strip_suffix('s').unwrap_or(s).to_string(),
        None => param.to_string(),
    }
}

/// Path parameters that are handles rather than names: recorded as a
/// short hash.
const OPAQUE_PARAMS: &[(&str, &str)] = &[
    ("/_rustybin/mcp-relay/{session}", "session"),
    ("/_rustybin/oauth/requests/{id}", "id"),
];

fn short_hash(v: &str) -> String {
    let h = crate::mocks::visits::sha256_hex(v);
    format!("sha256:{}", &h[..12])
}

/// The targets a route's path names.
fn path_targets(template: &str, params: &[(String, String)]) -> Vec<Target> {
    params
        .iter()
        .take(MAX_TARGETS)
        .map(|(k, v)| Target {
            kind: param_kind(template, k),
            name: if OPAQUE_PARAMS.contains(&(template, k.as_str())) {
                short_hash(v)
            } else {
                clean(v)
            },
        })
        .filter(|t| !t.name.is_empty())
        .collect()
}

/// A refusal's `reason` and `permission`, read from a small JSON body.
#[derive(Default)]
struct Said {
    reason: Option<String>,
    permission: Option<String>,
}

/// Largest refusal body read.
const MAX_PEEK: u64 = 16 * 1024;

async fn peek(resp: Response) -> (Response, Said) {
    use http_body::Body as _;
    let small = resp
        .body()
        .size_hint()
        .exact()
        .is_some_and(|n| n <= MAX_PEEK);
    let json = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.contains("json"));
    if !small || !json {
        return (resp, Said::default());
    }
    let (parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_PEEK as usize).await else {
        return (
            Response::from_parts(parts, axum::body::Body::empty()),
            Said::default(),
        );
    };
    let said = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .map(|v| {
            let s = |k: &str| v.get(k).and_then(Value::as_str).map(clean);
            Said {
                reason: s("reason"),
                permission: s("permission"),
            }
        })
        .unwrap_or_default();
    (
        Response::from_parts(parts, axum::body::Body::from(bytes)),
        said,
    )
}

/// The control path prefix and the portal's, whose routes the layer reads.
fn watched(template: &str) -> bool {
    let under =
        |p: &str| template == p || template.strip_prefix(p).is_some_and(|r| r.starts_with('/'));
    under(crate::control::CONTROL_PREFIX) || under("/portal")
}

/// Route layer (`build_app`, outside `control_auth::check_route`): records
/// the mutating control routes, every refusal naming a permission and every
/// request whose handler named an event. One entry per request at most.
pub async fn layer(State(state): State<AppState>, req: Request, next: Next) -> Response {
    // An `Arc<str>` clone: nothing allocated on the data plane's hot path.
    let Some(matched) = req.extensions().get::<MatchedPath>().cloned() else {
        return next.run(req).await;
    };
    let template = matched.as_str();
    let method = req.method().clone();
    if !watched(template) {
        // The data plane: only a refusal naming a permission (the /health
        // toggles and the global /flaky reset need settings.write), as
        // `rbac::refuse` marks it; its answers are never read.
        let resp = next.run(req).await;
        if resp
            .extensions()
            .get::<crate::rbac::need::Refusal>()
            .is_none()
        {
            return resp;
        }
        let ctx = Ctx::bare(template, method.as_str().to_string());
        return finish(&state, resp, Note::default(), ctx).await;
    }
    let control = template.starts_with(crate::control::CONTROL_PREFIX);
    let recorded = control && is_mutating(&method) && !not_recorded(method.as_str(), template);
    let ctx = Ctx::of(&state, req, template, recorded).await;
    let (req, ctx) = ctx;
    let (resp, note) = scoped(next.run(req)).await;
    finish(&state, resp, note, ctx).await
}

/// What the layer reads off a request before it runs.
struct Ctx {
    template: String,
    method: String,
    recorded: bool,
    params: Vec<(String, String)>,
    request_id: Option<String>,
    address: Option<String>,
    visit: Option<String>,
}

impl Ctx {
    fn bare(template: &str, method: String) -> Self {
        Self {
            template: template.to_string(),
            method,
            recorded: false,
            params: Vec::new(),
            request_id: None,
            address: None,
            visit: None,
        }
    }

    async fn of(state: &AppState, req: Request, template: &str, recorded: bool) -> (Request, Self) {
        use axum::extract::FromRequestParts;
        let (mut parts, body) = req.into_parts();
        let params = RawPathParams::from_request_parts(&mut parts, &())
            .await
            .map(|p| {
                p.iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let request_id = parts
            .headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let address = crate::session::client_ip(&parts.headers, &parts.extensions, &state.config)
            .map(|ip| ip.to_string());
        let visit = state.mocks.visits().and_then(|visits| {
            let tokens: Vec<&str> = parts
                .headers
                .get_all(crate::mocks::visits::TOKEN_HEADER)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|v| v.split(','))
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .collect();
            if tokens.is_empty() {
                return None;
            }
            visits
                .resolve(tokens)
                .into_iter()
                .next()
                .map(|h| crate::mocks::visits::Owner::Visit(h).fingerprint())
        });
        let ctx = Self {
            template: template.to_string(),
            method: parts.method.as_str().to_string(),
            recorded,
            params,
            request_id,
            address,
            visit,
        };
        (Request::from_parts(parts, body), ctx)
    }
}

async fn finish(state: &AppState, resp: Response, note: Note, ctx: Ctx) -> Response {
    let status = resp.status().as_u16();
    let (resp, said) = if let Some(r) = resp.extensions().get::<crate::rbac::need::Refusal>() {
        let said = Said {
            reason: Some("permission_required".into()),
            permission: Some(r.0.name().to_string()),
        };
        (resp, said)
    } else if matches!(status, 401 | 403) && watched(&ctx.template) {
        peek(resp).await
    } else {
        (resp, Said::default())
    };
    let refused = said.reason.as_deref() == Some("permission_required");
    if !(ctx.recorded || note.event.is_some() || refused) {
        return resp;
    }
    let caller = resp
        .extensions()
        .get::<crate::control_auth::ControlCaller>()
        .map(|c| c.0.clone());
    let actor = note.actor.clone().unwrap_or_else(|| {
        let a = caller.as_deref().map(Actor::of).unwrap_or_default();
        match (&ctx.visit, a.kind.as_str()) {
            (Some(v), "anonymous" | "open" | "") => Actor::visit(v),
            (_, "") => Actor::kind("anonymous"),
            _ => a,
        }
    });
    let mut targets = path_targets(&ctx.template, &ctx.params);
    for t in note.targets {
        if targets.len() < MAX_TARGETS && !targets.contains(&t) {
            targets.push(t);
        }
    }
    let mut result = Outcome::of_status(status);
    if refused {
        result.outcome = "refused".into();
    }
    if status >= 400 {
        result.reason = said.reason;
        result.permission = said.permission;
    }
    let action = if ctx.method.is_empty() {
        ctx.template.clone()
    } else {
        format!("{} {}", ctx.method, ctx.template)
    };
    state.audit.record(Draft {
        actor,
        via: "api",
        action,
        event: note.event,
        targets,
        result,
        request_id: ctx.request_id,
        address: ctx.address,
    });
    resp
}

// ── Settings ────────────────────────────────────────────────────────

fn seed_retention(c: &crate::config::Config) -> Option<Value> {
    c.audit_retention_days.map(Value::from)
}

fn seed_max_bytes(c: &crate::config::Config) -> Option<Value> {
    c.audit_max_bytes.map(Value::from)
}

/// The `audit.*` settings.
pub fn specs() -> Vec<Spec> {
    vec![
        Spec::new(
            RETENTION_KEY,
            "audit",
            "Keep the audit files (days)",
            "How many UTC days of <state>/audit/audit-YYYY-MM-DD.jsonl are kept (at most 365); older days are deleted at the first entry of a day. Changing it needs settings.security, and a document from git cannot.",
            Kind::Integer {
                min: 1,
                max: MAX_RETENTION_DAYS,
            },
            json!(DEFAULT_RETENTION_DAYS),
        )
        .env("RUSTYBIN_AUDIT_RETENTION_DAYS", seed_retention),
        Spec::new(
            MAX_BYTES_KEY,
            "audit",
            "Largest size of the audit files (bytes)",
            "The audit files together never pass it (default 256 MiB): the oldest days are deleted first, and when the day being written reaches it alone, new entries stay in memory until the next day.",
            Kind::Integer {
                min: MIN_MAX_BYTES,
                max: MAX_MAX_BYTES,
            },
            json!(DEFAULT_MAX_BYTES),
        )
        .env("RUSTYBIN_AUDIT_MAX_BYTES", seed_max_bytes),
    ]
}

// ── Routes ──────────────────────────────────────────────────────────

fn bad_request(message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message, "reason": "invalid_filter" })),
    )
        .into_response()
}

/// `GET /_rustybin/audit`.
async fn list(State(state): State<AppState>, Query(q): Query<FilterQuery>) -> Response {
    let filter = match Filter::parse(&q, Utc::now()) {
        Ok(f) => f,
        Err(e) => return bad_request(e),
    };
    let (entries, next) = state.audit.list(&filter);
    let mut body = json!({
        "entries": entries,
        "next_cursor": next.map(|n| n.to_string()),
    });
    body["log"] = state.audit.status();
    let mut resp = Json(body).into_response();
    resp.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    resp
}

/// `GET /_rustybin/audit/stream`: an `entry` event per new entry that
/// matches the filters (the cursor and the limit do not apply).
async fn stream(State(state): State<AppState>, Query(q): Query<FilterQuery>) -> Response {
    let filter = match Filter::parse(&q, Utc::now()) {
        Ok(f) => f,
        Err(e) => return bad_request(e),
    };
    let Some((rx, guard)) = state.audit.subscribe() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": format!("at most {MAX_LISTENERS} live audit feeds at once") })),
        )
            .into_response();
    };
    let s = BroadcastStream::new(rx).filter_map(move |item| {
        let _hold = &guard;
        let e = item.ok()?;
        filter.matches(&e).then(|| {
            Ok::<Event, Infallible>(
                Event::default()
                    .event("entry")
                    .id(e.id.to_string())
                    .data(serde_json::to_string(&*e).unwrap_or_default()),
            )
        })
    });
    Sse::new(s).keep_alive(KeepAlive::default()).into_response()
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(LIST_PATH, get(list))
        .route(STREAM_PATH, get(stream))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            LIST_PATH,
            &["GET"],
            category::CONTROL,
            "The audit log: who changed what, every sign-in and every refusal (audit.read)",
        )
        .permission(Permission::AuditRead)
        .description(
            "Newest first: each entry has the time, the actor (a user's id and name, or the \
             credential's kind: admin_token, token, key, visit, open, anonymous, system; plus the \
             agent for a management tool call), via (api, mcp, system), the action (the route \
             template and method, or the tool), the event a handler named (login, invite, sso, \
             ...), the targets (kind and name: a mock, a user, a setting key), the result \
             (status, outcome ok, error, refused or asked, the reason and permission of a \
             refusal), the request id and the client address. Never a value, a body, a header, a \
             password or a token. Filters: actor, action, target (kind:name for one kind), since \
             and until (RFC 3339, a day, or an age such as 15m, 2h, 7d), result (ok, error, \
             refused, asked, or a status code); cursor (next_cursor) and limit (1 to 500, default \
             100). The last 10,000 entries are kept in memory; with a state directory every \
             entry is also in <state>/audit/ as JSON Lines, one file per UTC day (log says where \
             and how much).",
        )
        .example(Example::get("The latest entries", LIST_PATH))
        .example(Example::get(
            "Refusals in the last hour",
            "/_rustybin/audit?result=refused&since=1h",
        ))
        .example(Example::get(
            "Changes of the petstore mock",
            "/_rustybin/audit?target=mock:petstore&limit=20",
        )),
        Endpoint::new(
            STREAM_PATH,
            &["GET"],
            category::CONTROL,
            "Live tail of the audit log (audit.read)",
        )
        .permission(Permission::AuditRead)
        .description(
            "Server-Sent Events: an `entry` event (id: the entry's id) per new entry that \
             matches the same filters as GET /_rustybin/audit. At most 16 listeners.",
        )
        .sse()
        .example(
            Example::get(
                "Follow the refusals",
                "/_rustybin/audit/stream?result=refused",
            )
            .skip_check("never-ending SSE stream"),
        ),
    ]
}

pub fn openapi_paths() -> Value {
    let q = |name: &str, desc: &str| json!({ "name": name, "in": "query", "required": false, "schema": { "type": "string" }, "description": desc });
    let filters = json!([
        q(
            "actor",
            "Contained in the actor's kind, user id, name or agent (any case)"
        ),
        q(
            "action",
            "Contained in the action (route template and method, or tool) or the event"
        ),
        q(
            "target",
            "Contained in a target's name; kind:name for one kind (mock:petstore)"
        ),
        q(
            "since",
            "RFC 3339, a UTC day (2026-11-03) or an age (15m, 2h, 7d)"
        ),
        q("until", "RFC 3339, a UTC day or an age"),
        q("result", "ok, error, refused, asked, or a status code"),
    ]);
    let mut page = filters.as_array().cloned().unwrap_or_default();
    page.push(q("cursor", "The next_cursor of the previous page"));
    page.push(json!({ "name": "limit", "in": "query", "required": false, "schema": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "default": DEFAULT_LIMIT } }));
    let forbidden = json!({ "description": "permission_required: audit.read" });
    json!({
        LIST_PATH: { "get": {
            "tags": ["Control Plane"], "summary": "The audit log", "operationId": "listAudit",
            "parameters": page,
            "responses": {
                "200": { "description": "{entries (newest first), next_cursor, log: held, capacity, persisted, files, bytes, retention_days, max_bytes, not_written, suppressed}" },
                "400": { "description": "An invalid filter" },
                "403": forbidden
            }
        } },
        STREAM_PATH: { "get": {
            "tags": ["Control Plane"], "summary": "Live tail of the audit log (SSE)", "operationId": "streamAudit",
            "parameters": filters,
            "responses": {
                "200": { "description": "text/event-stream: entry events" },
                "400": { "description": "An invalid filter" },
                "403": forbidden,
                "429": { "description": "Too many listeners" }
            }
        } },
    })
}

#[cfg(test)]
mod tests;
