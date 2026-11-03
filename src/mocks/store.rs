//! The mock store: named mocks, each with a draft revision, an optional
//! live revision, a short history for rollback, an on/off switch and a
//! capture log. Bounded by [`MockLimits`]; mocks created through the API
//! expire in public mode.
//!
//! Every mock has an [`Owner`]. Self-service mocks (a visit or an API key)
//! have their own caps next to `max_mocks`, are compiled with the visitor
//! restrictions, and a visit's mocks end with the visit ([`Visits`]).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use super::capture::CaptureLog;
use super::checks::Report;
use super::compile::{CompileLimits, Compiled};
use super::model::{valid_name, MockSettings, OperationSettings, SettingsLimits, MAX_RULES};
use super::spec::ParseLimits;
use super::state::MockState;
use super::visits::{Owner, Visits};
use crate::config::Config;

/// Capacity limits (lower in public mode).
#[derive(Clone, Copy, Debug, Serialize)]
pub struct MockLimits {
    pub max_mocks: usize,
    pub max_spec_bytes: usize,
    pub max_operations: usize,
    pub max_rules: usize,
    pub max_body_bytes: usize,
    pub revisions: usize,
    pub captures: usize,
    pub capture_bytes: usize,
    /// Lifetime of API-created mocks (public mode only).
    pub ttl_secs: Option<u64>,
    pub max_captured_body: usize,
    pub max_delay_ms: u64,
    /// Items and bytes kept per mock in `stateful` mode.
    pub state_items: usize,
    pub state_bytes: usize,
}

impl MockLimits {
    pub fn for_config(config: &Config) -> Self {
        if config.public_mode {
            Self {
                max_mocks: 50,
                max_spec_bytes: 256 * 1024,
                max_operations: 200,
                max_rules: 10,
                max_body_bytes: 64 * 1024,
                revisions: 2,
                captures: 50,
                capture_bytes: 256 * 1024,
                ttl_secs: Some(24 * 3600),
                max_captured_body: 64 * 1024,
                max_delay_ms: config.max_delay_ms(),
                state_items: 200,
                state_bytes: 256 * 1024,
            }
        } else {
            Self {
                max_mocks: 100,
                max_spec_bytes: 1024 * 1024,
                max_operations: 500,
                max_rules: MAX_RULES,
                max_body_bytes: 256 * 1024,
                revisions: 5,
                captures: 200,
                capture_bytes: 1024 * 1024,
                ttl_secs: None,
                max_captured_body: 64 * 1024,
                max_delay_ms: config.max_delay_ms(),
                state_items: 1000,
                state_bytes: 2 * 1024 * 1024,
            }
        }
    }

    pub fn compile_limits(&self) -> CompileLimits {
        self.compile_limits_for(false)
    }

    /// Limits for an instance mock or (`visitor`) a self-service one.
    pub fn compile_limits_for(&self, visitor: bool) -> CompileLimits {
        CompileLimits {
            parse: ParseLimits {
                max_bytes: self.max_spec_bytes,
                max_operations: self.max_operations,
            },
            settings: SettingsLimits {
                max_delay_ms: self.max_delay_ms,
                max_body_bytes: self.max_body_bytes,
                max_rules: self.max_rules,
                visitor,
            },
        }
    }
}

/// Where a mock was defined.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// Control API or console.
    Api,
    /// `rustybin.yaml` or the mocks directory (reloaded from disk).
    File,
    /// The built-in sample.
    Sample,
}

#[derive(Debug)]
pub enum StoreError {
    NotFound(String),
    Conflict(String),
    Invalid(String),
    Full(String),
    /// The owner already has as many mocks as it may.
    OwnerCap(String),
    /// Deploy refused: the checks found errors.
    ChecksFailed(Box<Report>),
    /// The edit was made from another draft revision than the current one.
    Stale {
        name: String,
        expected: u64,
        current: u64,
    },
}

impl StoreError {
    pub fn message(&self) -> String {
        match self {
            Self::NotFound(n) => format!("mock {n:?} not found"),
            Self::Conflict(m) | Self::Invalid(m) | Self::Full(m) | Self::OwnerCap(m) => m.clone(),
            Self::ChecksFailed(r) => format!(
                "the deploy checks found {} error(s): fix them or deploy with force",
                r.errors
            ),
            Self::Stale {
                name,
                expected,
                current,
            } => format!(
                "mock {name:?} changed since revision {expected}: its draft is revision {current} (reload, then edit again)"
            ),
        }
    }
}

struct Entry {
    origin: Origin,
    owner: Owner,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    enabled: bool,
    draft: Arc<Compiled>,
    live: Option<Arc<Compiled>>,
    /// Previously live revisions, newest last.
    history: VecDeque<Arc<Compiled>>,
    /// Drafts replaced before they were deployed, newest last (as many as
    /// `history`): a stale edit's base can still be compared.
    earlier: VecDeque<Arc<Compiled>>,
    capture: CaptureLog,
    state: MockState,
    expires: Option<(Instant, DateTime<Utc>)>,
    /// A gateway scenario's upstream ([`MockStore::create_scratch`]): never
    /// saved, never exported.
    scratch: bool,
    /// Why the last reload of this file mock failed (its last good
    /// revision, if any, keeps answering).
    load_error: Option<String>,
    /// The project the mock belongs to (step 11); `None`: the instance's.
    project: Option<String>,
}

impl Entry {
    /// Keep the draft about to be replaced among the earlier drafts, unless
    /// it is live or in the history already (bounded by `cap`).
    fn keep_earlier_draft(&mut self, cap: usize) {
        let d = &self.draft;
        let kept = self.live.as_ref().is_some_and(|l| l.revision == d.revision)
            || self.history.iter().any(|c| c.revision == d.revision)
            || self.earlier.iter().any(|c| c.revision == d.revision);
        if !kept {
            self.earlier.push_back(d.clone());
            while self.earlier.len() > cap {
                self.earlier.pop_front();
            }
        }
    }

    /// A revision this mock still keeps: the draft, the live revision, a
    /// previously live one or an earlier draft.
    fn kept(&self, revision: u64) -> Option<Arc<Compiled>> {
        std::iter::once(&self.draft)
            .chain(self.live.iter())
            .chain(self.history.iter())
            .chain(self.earlier.iter())
            .find(|c| c.revision == revision)
            .cloned()
    }
}

/// What a request handler needs from a mock.
pub struct LiveMock {
    pub compiled: Arc<Compiled>,
    pub enabled: bool,
    pub capture: CaptureLog,
    pub state: MockState,
    /// A self-service mock: restricted responses, the mocks domain only.
    pub visitor: bool,
}

pub enum Lookup {
    Missing,
    /// Exists but was never deployed (with the load error of a file mock
    /// whose spec could not be used).
    NotDeployed(CaptureLog, Option<String>),
    Ready(LiveMock),
}

/// A snapshot of one mock for the control API.
pub struct Snapshot {
    pub name: String,
    pub origin: Origin,
    pub owner: Owner,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub enabled: bool,
    pub draft: Arc<Compiled>,
    pub live: Option<Arc<Compiled>>,
    pub history: Vec<Arc<Compiled>>,
    pub capture: CaptureLog,
    pub state: MockState,
    pub expires_at: Option<DateTime<Utc>>,
    /// A gateway scenario's upstream: never saved, exported nor kept in a
    /// checkpoint.
    pub scratch: bool,
    pub load_error: Option<String>,
    /// The project the mock belongs to; `None`: the instance's.
    pub project: Option<String>,
}

impl Snapshot {
    /// `live`, `off` or `draft` (never deployed).
    pub fn state(&self) -> &'static str {
        match (&self.live, self.enabled) {
            (None, _) => "draft",
            (Some(_), true) => "live",
            (Some(_), false) => "off",
        }
    }

    /// May a prune (a document applied with `prune`, a reload) delete this
    /// mock when the document does not name it? Only the instance's own:
    /// never a visitor's or an API key's mock, nor a gateway scenario's
    /// scratch mock (its run is going). The origin is checked by the caller.
    pub fn prunable(&self) -> bool {
        self.owner == Owner::Instance && !self.scratch
    }

    pub fn has_draft_changes(&self) -> bool {
        self.live
            .as_ref()
            .is_none_or(|l| l.revision != self.draft.revision)
    }

    /// The list view of a mock.
    pub fn summary(&self) -> Value {
        let shown = self.live.as_ref().unwrap_or(&self.draft);
        let sec = &shown.settings.security;
        json!({
            "name": self.name,
            "path": format!("/mock/{}", self.name),
            "title": shown.spec.title,
            "version": shown.spec.version,
            "state": self.state(),
            "enabled": self.enabled,
            "has_draft_changes": self.has_draft_changes(),
            "origin": self.origin,
            "owner": self.owner.kind(),
            "operations": shown.spec.operations.len(),
            "security": { "enabled": sec.enabled, "mode": sec.mode.as_str() },
            "spec_declares_security": shown.spec.declares_security(),
            "mode": shown.settings.responses.mode,
            "validate_requests": shown.settings.responses.validate_requests,
            "requests_total": self.capture.total(),
            "requests_last_hour": self.capture.last_hour(),
            "state_items": self.state.item_count(),
            "last_hit": self.capture.last_hit(),
            "live_revision": self.live.as_ref().map(|l| l.revision),
            "draft_revision": self.draft.revision,
            "check": { "ok": self.draft.report.ok, "errors": self.draft.report.errors, "warnings": self.draft.report.warnings },
            "created_at": self.created_at.to_rfc3339(),
            "updated_at": self.updated_at.to_rfc3339(),
            "expires_at": self.expires_at.map(|e| e.to_rfc3339()),
            "load_error": self.load_error,
            "project": self.project,
        })
    }
}

/// Outcome of a declarative apply.
#[derive(Debug, Default, Serialize)]
pub struct ApplyReport {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub unchanged: Vec<String>,
    pub deleted: Vec<String>,
    pub errors: Vec<Value>,
}

/// One mock definition to apply.
pub struct Definition {
    pub name: String,
    pub source: String,
    pub settings: MockSettings,
    pub enabled: bool,
    /// The project the mock belongs to (`None`: the instance's).
    pub project: Option<String>,
}

#[derive(Clone)]
pub struct MockStore {
    inner: Arc<RwLock<BTreeMap<String, Entry>>>,
    revision: Arc<AtomicU64>,
    load_errors: Arc<RwLock<Vec<String>>>,
    limits: MockLimits,
    /// Where API-created mocks are saved (`RUSTYBIN_MOCKS_STATE_DIR`).
    persist_dir: Option<Arc<PathBuf>>,
    /// Self-service visits (`RUSTYBIN_MOCKS_SELF_SERVICE`).
    visits: Option<Arc<Visits>>,
    /// The `registry.servers` of `rustybin.yaml` (validated `server.json`
    /// documents, at most `mcp::registry::MAX_USER_ENTRIES`) and when they
    /// were loaded: the mock MCP registry lists them.
    file_registry: Arc<RwLock<(Vec<Value>, DateTime<Utc>)>>,
    /// The `portal.items` of `rustybin.yaml` (validated) and when they were
    /// loaded: published on the dev portal.
    file_portal: Arc<RwLock<(Vec<crate::portal::FileItem>, DateTime<Utc>)>>,
    /// The `agents:` section of `rustybin.yaml` (validated): the default
    /// ceiling and the rules of the management MCP server's agents.
    file_agents: Arc<RwLock<crate::mcp::manage::agents::FilePolicy>>,
    /// Compiles that may run at once off the async runtime (`Svc::compiling`).
    compiles: Arc<tokio::sync::Semaphore>,
    /// One save at a time: each writes the mock as it is now, so the last
    /// write is the latest state whatever order the mutations finish in.
    persist_lock: Arc<std::sync::Mutex<()>>,
    /// Serialises the read, recompile and write of an edit (`set_operation`,
    /// the service's update), so two edits never interleave and lose one.
    edit_lock: Arc<std::sync::Mutex<()>>,
    /// Recent compiles per visitor (rate limit), bounded by `MAX_RECOMPILE_KEYS`.
    recompiles: Arc<std::sync::Mutex<std::collections::HashMap<String, VecDeque<Instant>>>>,
}

/// Compiles that run at once on the blocking pool.
pub const COMPILE_PERMITS: usize = 4;
/// How long a compile waits for a permit before the answer is 503.
pub const COMPILE_WAIT: Duration = Duration::from_secs(5);
/// Compiles one visitor may start per minute (preview, create, edits).
pub const VISITOR_COMPILES_PER_MINUTE: usize = 30;
/// Visitors tracked by the compile rate limit.
pub const MAX_RECOMPILE_KEYS: usize = 4096;

/// 2026-01-01T00:00:00Z in Unix milliseconds.
const REVISION_EPOCH_MS: i64 = 1_767_225_600_000;

/// The first revision number of a store: the milliseconds since 2026, so
/// numbering continues across restarts instead of starting at 1 again, and
/// an `If-Match` named before a restart (a console left open) never matches
/// a draft compiled after it. A process would have to make more than a
/// thousand revisions a second, every second it ran, to catch up with the
/// clock.
fn first_revision() -> u64 {
    let ms = Utc::now()
        .timestamp_millis()
        .saturating_sub(REVISION_EPOCH_MS);
    u64::try_from(ms).unwrap_or(0).max(1)
}

/// Run blocking file work (a synced write, a removal) off the async
/// workers: on a worker of the multi-threaded runtime the thread hands its
/// tasks over and blocks as a blocking-pool thread would
/// (`block_in_place`); on the blocking pool, outside a runtime or on a
/// current-thread runtime (tests) it runs where it is. Mutations stay
/// synchronous: a save is on disk when the call returns.
pub(crate) fn off_runtime<R>(f: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

impl MockStore {
    pub fn new(config: &Config) -> Self {
        Self::with_limits(MockLimits::for_config(config))
    }

    pub fn with_limits(limits: MockLimits) -> Self {
        Self {
            inner: Arc::new(RwLock::new(BTreeMap::new())),
            revision: Arc::new(AtomicU64::new(first_revision())),
            load_errors: Arc::new(RwLock::new(Vec::new())),
            limits,
            persist_dir: None,
            visits: None,
            file_registry: Arc::new(RwLock::new((Vec::new(), Utc::now()))),
            file_portal: Arc::new(RwLock::new((Vec::new(), Utc::now()))),
            file_agents: Arc::new(RwLock::new(Default::default())),
            compiles: Arc::new(tokio::sync::Semaphore::new(COMPILE_PERMITS)),
            persist_lock: Arc::new(std::sync::Mutex::new(())),
            edit_lock: Arc::new(std::sync::Mutex::new(())),
            recompiles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// The permits of compiles off the runtime.
    pub fn compile_permits(&self) -> Arc<tokio::sync::Semaphore> {
        self.compiles.clone()
    }

    /// Count a visitor's compile; false when the minute's allowance is spent.
    pub fn allow_visitor_compile(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut map = self.recompiles.lock().unwrap_or_else(|p| p.into_inner());
        if !map.contains_key(key) && map.len() >= MAX_RECOMPILE_KEYS {
            map.retain(|_, q| {
                q.back()
                    .is_some_and(|t| now.duration_since(*t) < Duration::from_secs(60))
            });
            if map.len() >= MAX_RECOMPILE_KEYS {
                return false;
            }
        }
        let q = map.entry(key.to_string()).or_default();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            q.pop_front();
        }
        if q.len() >= VISITOR_COMPILES_PER_MINUTE {
            return false;
        }
        q.push_back(now);
        true
    }

    /// Replace the portal items of the config file (the load time is kept
    /// when they did not change).
    pub fn set_file_portal(&self, items: Vec<crate::portal::FileItem>) {
        let mut slot = self.file_portal.write().unwrap_or_else(|p| p.into_inner());
        if slot.0 != items {
            *slot = (items, Utc::now());
        }
    }

    /// The portal items of the config file and when they were loaded.
    pub fn file_portal(&self) -> (Vec<crate::portal::FileItem>, DateTime<Utc>) {
        self.file_portal
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Replace the agents policy of the config file.
    pub fn set_file_agents(&self, policy: crate::mcp::manage::agents::FilePolicy) {
        *self.file_agents.write().unwrap_or_else(|p| p.into_inner()) = policy;
    }

    /// The agents policy of the config file.
    pub fn file_agents(&self) -> crate::mcp::manage::agents::FilePolicy {
        self.file_agents
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Replace the registry entries of the config file.
    pub fn set_file_registry(&self, servers: Vec<Value>) {
        *self
            .file_registry
            .write()
            .unwrap_or_else(|p| p.into_inner()) = (servers, Utc::now());
    }

    /// The registry entries of the config file and when they were loaded.
    pub fn file_registry(&self) -> (Vec<Value>, DateTime<Utc>) {
        self.file_registry
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Turn on self-service: visit-owned mocks live as long as their visit.
    pub fn with_visits(mut self, visits: Arc<Visits>) -> Self {
        self.visits = Some(visits);
        self
    }

    pub fn visits(&self) -> Option<&Arc<Visits>> {
        self.visits.as_ref()
    }

    /// Save API-created mocks under `dir` on every change.
    pub fn with_persistence(mut self, dir: PathBuf) -> Self {
        self.persist_dir = Some(Arc::new(dir));
        self
    }

    pub fn persist_dir(&self) -> Option<&PathBuf> {
        self.persist_dir.as_deref()
    }

    /// Save the mock a mutation returned (persistence on).
    fn saved(&self, r: Result<Snapshot, StoreError>) -> Result<Snapshot, StoreError> {
        if let Ok(s) = &r {
            self.persist_now(&s.name);
        }
        r
    }

    /// Write the saved copy of `name` as the store holds it now (or remove
    /// it when the mock is gone). Called after the mutation's lock is
    /// released; the persistence lock orders the writes, and each one reads
    /// the current state, so a slow earlier save never overwrites a later one
    /// nor brings a deleted mock back. A gateway scenario's scratch mock is
    /// never saved. The file work (a synced write) runs off the async
    /// workers ([`off_runtime`]).
    fn persist_now(&self, name: &str) {
        let Some(dir) = &self.persist_dir else { return };
        off_runtime(|| {
            let _order = self.persist_lock.lock().unwrap_or_else(|p| p.into_inner());
            let current = self.read().get(name).map(|e| snapshot(name, e));
            match current {
                Some(s) if s.scratch => {}
                Some(s) => super::persist::save(dir, &s),
                None => super::persist::remove(dir, name),
            }
        });
    }

    /// Held for the read, recompile and write of one edit (blocking: call it
    /// on the blocking pool, as every edit is).
    pub fn edit_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.edit_lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Write the saved copy of `name` as it is now (after a restore).
    pub fn persist(&self, name: &str) {
        self.persist_now(name);
    }

    pub fn limits(&self) -> MockLimits {
        self.limits
    }

    fn read(&self) -> RwLockReadGuard<'_, BTreeMap<String, Entry>> {
        self.inner.read().unwrap_or_else(|p| p.into_inner())
    }

    /// Write lock; expired mocks (and the mocks of ended visits) are
    /// dropped first, with their saved copies.
    fn write(&self) -> RwLockWriteGuard<'_, BTreeMap<String, Entry>> {
        let mut map = self.inner.write().unwrap_or_else(|p| p.into_inner());
        let mut gone = Vec::new();
        map.retain(|name, e| {
            let keep = self.live_entry(e);
            if !keep {
                gone.push(name.clone());
            }
            keep
        });
        if let (Some(dir), false) = (&self.persist_dir, gone.is_empty()) {
            off_runtime(|| {
                for name in &gone {
                    super::persist::remove(dir, name);
                }
            });
        }
        map
    }

    fn live_entry(&self, e: &Entry) -> bool {
        let timed = e
            .expires
            .is_none_or(|(deadline, _)| deadline > Instant::now());
        timed
            && match &e.owner {
                Owner::Visit(hash) => self.visits.as_ref().is_some_and(|v| v.alive(hash)),
                _ => true,
            }
    }

    /// Drop the mocks of ended visits now (the sweep task); returns how
    /// many went.
    pub fn sweep(&self) -> usize {
        if let Some(v) = &self.visits {
            v.sweep();
        }
        let before = self.read().len();
        let after = self.write().len();
        before.saturating_sub(after)
    }

    /// Compile a definition (outside any lock).
    pub fn compile(&self, source: &str, settings: MockSettings) -> Result<Compiled, String> {
        self.compile_for(source, settings, false)
    }

    /// [`Self::compile`]; `visitor` applies the self-service restrictions.
    pub fn compile_for(
        &self,
        source: &str,
        settings: MockSettings,
        visitor: bool,
    ) -> Result<Compiled, String> {
        let revision = self.revision.fetch_add(1, Ordering::Relaxed);
        Compiled::build(
            revision,
            Arc::from(source),
            settings.normalised(),
            &self.limits.compile_limits_for(visitor),
        )
    }

    /// Compile new settings (and optionally a new spec) for an existing
    /// revision; an unchanged spec is shared, not parsed again. A visitor
    /// revision stays a visitor revision.
    pub fn recompile(
        &self,
        base: &Compiled,
        source: Option<&str>,
        settings: MockSettings,
    ) -> Result<Compiled, String> {
        match source {
            Some(s) if s != &*base.source => self.compile_for(s, settings, base.visitor),
            _ => {
                let revision = self.revision.fetch_add(1, Ordering::Relaxed);
                Compiled::from_parsed(
                    revision,
                    base.source.clone(),
                    base.spec.clone(),
                    settings.normalised(),
                    &self.limits.compile_limits_for(base.visitor),
                )
            }
        }
    }

    pub fn record_load_error(&self, message: String) {
        let mut errors = self.load_errors.write().unwrap_or_else(|p| p.into_inner());
        if errors.len() >= 50 {
            errors.remove(0);
        }
        errors.push(message);
    }

    pub fn load_errors(&self) -> Vec<String> {
        self.load_errors
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn clear_load_errors(&self) {
        self.load_errors
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    /// Create a mock. With `deploy`, the draft goes live (refused on check
    /// errors unless `force`, in which case nothing is created).
    pub fn create(
        &self,
        name: &str,
        compiled: Compiled,
        origin: Origin,
        enabled: bool,
        deploy: bool,
        force: bool,
    ) -> Result<Snapshot, StoreError> {
        self.create_owned(
            name,
            compiled,
            origin,
            Owner::Instance,
            enabled,
            deploy,
            force,
        )
    }

    /// [`Self::create`] for an owner (self-service caps apply to visitors).
    #[allow(clippy::too_many_arguments)]
    pub fn create_owned(
        &self,
        name: &str,
        compiled: Compiled,
        origin: Origin,
        owner: Owner,
        enabled: bool,
        deploy: bool,
        force: bool,
    ) -> Result<Snapshot, StoreError> {
        let r = self.create_inner(name, compiled, origin, owner, enabled, deploy, force, None);
        self.saved(r)
    }

    /// [`Self::create_owned`] in a project (an instance mock only).
    #[allow(clippy::too_many_arguments)]
    pub fn create_in(
        &self,
        name: &str,
        compiled: Compiled,
        origin: Origin,
        owner: Owner,
        enabled: bool,
        deploy: bool,
        force: bool,
        project: Option<String>,
    ) -> Result<Snapshot, StoreError> {
        let project = project.filter(|_| !owner.is_visitor());
        let r = self.create_inner(
            name, compiled, origin, owner, enabled, deploy, force, project,
        );
        self.saved(r)
    }

    /// Move a mock to `project` (`None`: the instance's). Visitors' mocks
    /// stay where they are. Saved.
    pub fn set_project(&self, name: &str, project: Option<String>) -> Result<Snapshot, StoreError> {
        let r = self.set_project_inner(name, project, true);
        self.saved(r)
    }

    /// [`Self::set_project`] without touching the saved copy (a restore
    /// from it).
    pub fn set_project_quietly(
        &self,
        name: &str,
        project: Option<String>,
    ) -> Result<Snapshot, StoreError> {
        self.set_project_inner(name, project, false)
    }

    fn set_project_inner(
        &self,
        name: &str,
        project: Option<String>,
        touch: bool,
    ) -> Result<Snapshot, StoreError> {
        let mut map = self.write();
        let e = map
            .get_mut(name)
            .ok_or_else(|| StoreError::NotFound(name.to_string()))?;
        if e.owner.is_visitor() {
            return Err(StoreError::Invalid(
                "a self-service mock belongs to its visit, never to a project".into(),
            ));
        }
        e.project = project;
        if touch {
            e.updated_at = Utc::now();
        }
        Ok(snapshot(name, e))
    }

    /// The names of the mocks of `project`.
    pub fn in_project(&self, project: &str) -> Vec<String> {
        self.read()
            .iter()
            .filter(|(_, e)| e.project.as_deref() == Some(project) && self.live_entry(e))
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// A mock that lives at most `ttl` and is never saved (a gateway
    /// scenario's upstream, `crate::scenarios`): instance-owned, deployed and
    /// enabled, refused on check errors. It ends with its run, not with a
    /// restart's saved copy.
    pub fn create_scratch(
        &self,
        name: &str,
        compiled: Compiled,
        ttl: Duration,
    ) -> Result<Snapshot, StoreError> {
        Self::check_new(name, &compiled, true, false)?;
        let owner = Owner::Instance;
        let mut map = self.write();
        self.admit(&map, name, &owner, false)?;
        let mut entry = self.new_entry(compiled, Origin::Api, owner, true, true);
        // Marked in the same write that adds it: no save, export or
        // checkpoint ever sees it unmarked.
        entry.scratch = true;
        let at = Instant::now() + ttl;
        let wall = Utc::now() + chrono::TimeDelta::from_std(ttl).unwrap_or_default();
        entry.expires = Some(match entry.expires {
            Some((i, w)) if i < at => (i, w),
            _ => (at, wall),
        });
        let snap = snapshot(name, &entry);
        map.insert(name.to_string(), entry);
        Ok(snap)
    }

    /// Mocks of `owner`, by name.
    pub fn owned_names(&self, owner: &Owner) -> Vec<String> {
        self.read()
            .iter()
            .filter(|(_, e)| &e.owner == owner && self.live_entry(e))
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// Self-service mocks on the instance.
    pub fn visitor_count(&self) -> usize {
        self.read()
            .values()
            .filter(|e| e.owner.is_visitor() && self.live_entry(e))
            .count()
    }

    /// Give `from`'s mocks to `to` (a rotated manage token).
    /// Move every mock of `from` to the owner `swap` returns, with the store
    /// locked throughout. `swap` (a manage token rotation) runs inside the
    /// lock, so no write in between (another creation, the sweep) sees `from`
    /// as ended and drops its mocks. Lock order is the store, then the visits,
    /// as everywhere else. `None` (nothing moved) when `swap` returns `None`.
    pub fn reassign_with<T>(
        &self,
        from: &Owner,
        swap: impl FnOnce() -> Option<(Owner, T)>,
    ) -> Option<T> {
        let (changed, out): (Vec<Snapshot>, T) = {
            let mut map = self.inner.write().unwrap_or_else(|p| p.into_inner());
            let (to, out) = swap()?;
            let changed = map
                .iter_mut()
                .filter(|(_, e)| &e.owner == from)
                .map(|(n, e)| {
                    e.owner = to.clone();
                    snapshot(n, e)
                })
                .collect();
            (changed, out)
        };
        for s in &changed {
            self.persist_now(&s.name);
        }
        Some(out)
    }

    /// Delete every mock of `owner`; returns their names.
    pub fn delete_owned(&self, owner: &Owner) -> Vec<String> {
        let names: Vec<String> = self
            .read()
            .iter()
            .filter(|(_, e)| &e.owner == owner)
            .map(|(n, _)| n.clone())
            .collect();
        for n in &names {
            self.delete(n);
        }
        names
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn create_inner(
        &self,
        name: &str,
        compiled: Compiled,
        origin: Origin,
        owner: Owner,
        enabled: bool,
        deploy: bool,
        force: bool,
        project: Option<String>,
    ) -> Result<Snapshot, StoreError> {
        Self::check_new(name, &compiled, deploy, force)?;
        let mut map = self.write();
        self.admit(&map, name, &owner, false)?;
        let mut entry = self.new_entry(compiled, origin, owner, enabled, deploy);
        entry.project = project;
        let snap = snapshot(name, &entry);
        map.insert(name.to_string(), entry);
        Ok(snap)
    }

    /// A new mock's name and (when it goes live) its checks.
    fn check_new(
        name: &str,
        compiled: &Compiled,
        deploy: bool,
        force: bool,
    ) -> Result<(), StoreError> {
        if !valid_name(name) {
            return Err(StoreError::Invalid(format!(
                "invalid name {name:?}: use 1 to 40 characters a-z, 0-9 and '-' (not at either end)"
            )));
        }
        if deploy && !force && !compiled.report.ok {
            return Err(StoreError::ChecksFailed(Box::new(compiled.report.clone())));
        }
        Ok(())
    }

    /// May `name` (of `owner`) go into `map`: free (unless it `replaces` the
    /// mock of that name) and within the caps, a replaced mock not counted.
    fn admit(
        &self,
        map: &BTreeMap<String, Entry>,
        name: &str,
        owner: &Owner,
        replaces: bool,
    ) -> Result<(), StoreError> {
        if !replaces && map.contains_key(name) {
            return Err(StoreError::Conflict(format!(
                "a mock named {name:?} already exists"
            )));
        }
        let others = map.iter().filter(|(n, _)| n.as_str() != name);
        if owner.is_visitor() {
            let cfg = self
                .visits
                .as_ref()
                .map(|v| v.config().clone())
                .unwrap_or_default();
            let (mine, visitors) = others.fold((0, 0), |(mine, all), (_, e)| {
                (
                    mine + usize::from(&e.owner == owner),
                    all + usize::from(e.owner.is_visitor()),
                )
            });
            if mine >= cfg.per_owner {
                return Err(StoreError::OwnerCap(format!(
                    "you have {mine} mocks, the most one visitor may keep: delete one first"
                )));
            }
            if visitors >= cfg.max_mocks {
                return Err(StoreError::Full(
                    "the demo holds as many visitor mocks as it can: try again later".into(),
                ));
            }
        } else {
            let instance = others.filter(|(_, e)| !e.owner.is_visitor()).count();
            if instance >= self.limits.max_mocks {
                return Err(StoreError::Full(format!(
                    "this instance holds at most {} mocks: delete one first",
                    self.limits.max_mocks
                )));
            }
        }
        Ok(())
    }

    /// The entry of a new mock (live when `deploy`), with the public-mode
    /// expiry of API mocks.
    fn new_entry(
        &self,
        compiled: Compiled,
        origin: Origin,
        owner: Owner,
        enabled: bool,
        deploy: bool,
    ) -> Entry {
        let draft = Arc::new(compiled);
        let now = Utc::now();
        // A visit's mocks end with the visit, not after the public-mode TTL.
        let ttl = match owner {
            Owner::Visit(_) => None,
            _ => self.limits.ttl_secs,
        };
        let expires = match (origin, ttl) {
            (Origin::Api, Some(ttl)) => Some((
                Instant::now() + Duration::from_secs(ttl),
                now + chrono::TimeDelta::try_seconds(ttl as i64).unwrap_or_default(),
            )),
            _ => None,
        };
        Entry {
            origin,
            owner,
            created_at: now,
            updated_at: now,
            enabled,
            live: deploy.then(|| draft.clone()),
            draft,
            history: VecDeque::new(),
            earlier: VecDeque::new(),
            capture: CaptureLog::new(self.limits.captures, self.limits.capture_bytes),
            state: MockState::new(self.limits.state_items, self.limits.state_bytes),
            expires,
            scratch: false,
            load_error: None,
            project: None,
        }
    }

    /// Request-path lookup.
    pub fn lookup(&self, name: &str) -> Lookup {
        let map = self.read();
        let Some(e) = map.get(name).filter(|e| self.live_entry(e)) else {
            return Lookup::Missing;
        };
        match &e.live {
            None => Lookup::NotDeployed(e.capture.clone(), e.load_error.clone()),
            Some(live) => Lookup::Ready(LiveMock {
                compiled: live.clone(),
                enabled: e.enabled,
                capture: e.capture.clone(),
                state: e.state.clone(),
                visitor: e.owner.is_visitor(),
            }),
        }
    }

    pub fn get(&self, name: &str) -> Option<Snapshot> {
        let map = self.read();
        map.get(name)
            .filter(|e| self.live_entry(e))
            .map(|e| snapshot(name, e))
    }

    pub fn list(&self) -> Vec<Snapshot> {
        self.read()
            .iter()
            .filter(|(_, e)| self.live_entry(e))
            .map(|(n, e)| snapshot(n, e))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.list().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A revision of the mock the store still keeps (the draft, the live
    /// revision, the history of deployed ones, the drafts replaced before
    /// they were deployed), or `None` when it is gone.
    pub fn kept_revision(&self, name: &str, revision: u64) -> Option<Arc<Compiled>> {
        let map = self.read();
        map.get(name)
            .filter(|e| self.live_entry(e))
            .and_then(|e| e.kept(revision))
    }

    /// Replace the draft with a new compiled revision (and optionally rename).
    pub fn set_draft(
        &self,
        name: &str,
        compiled: Option<Compiled>,
        rename: Option<&str>,
    ) -> Result<Snapshot, StoreError> {
        self.set_draft_if(name, compiled, rename, None)
    }

    /// [`Self::set_draft`], refused with [`StoreError::Stale`] unless the
    /// draft is still revision `base` (checked under the write lock).
    pub fn set_draft_if(
        &self,
        name: &str,
        compiled: Option<Compiled>,
        rename: Option<&str>,
        base: Option<u64>,
    ) -> Result<Snapshot, StoreError> {
        let r = self.set_draft_inner(name, compiled, rename, base);
        if let Ok(s) = &r {
            if s.name != name {
                self.persist_now(name);
            }
        }
        self.saved(r)
    }

    fn set_draft_inner(
        &self,
        name: &str,
        compiled: Option<Compiled>,
        rename: Option<&str>,
        base: Option<u64>,
    ) -> Result<Snapshot, StoreError> {
        let mut map = self.write();
        if let (Some(b), Some(e)) = (base, map.get(name)) {
            if e.draft.revision != b {
                return Err(StoreError::Stale {
                    name: name.to_string(),
                    expected: b,
                    current: e.draft.revision,
                });
            }
        }
        if let Some(new_name) = rename.filter(|n| *n != name) {
            if !valid_name(new_name) {
                return Err(StoreError::Invalid(format!("invalid name {new_name:?}")));
            }
            if map.contains_key(new_name) {
                return Err(StoreError::Conflict(format!(
                    "a mock named {new_name:?} already exists"
                )));
            }
        }
        let mut entry = map
            .remove(name)
            .ok_or_else(|| StoreError::NotFound(name.to_string()))?;
        if let Some(c) = compiled {
            // A new spec replaces whatever failed to load.
            if c.source != entry.draft.source {
                entry.load_error = None;
            }
            entry.keep_earlier_draft(self.limits.revisions);
            entry.draft = Arc::new(c);
        }
        entry.updated_at = Utc::now();
        let key = rename.unwrap_or(name).to_string();
        let snap = snapshot(&key, &entry);
        map.insert(key, entry);
        Ok(snap)
    }

    /// The current draft (to derive an edited definition from).
    pub fn draft(&self, name: &str) -> Result<Arc<Compiled>, StoreError> {
        self.get(name)
            .map(|s| s.draft)
            .ok_or_else(|| StoreError::NotFound(name.to_string()))
    }

    /// Change a draft as one edit: under the edit lock, read the draft,
    /// build the new one with `edit`, and write it only if the draft is still
    /// the one read. With `expected`, refused unless the draft is that
    /// revision; without, an edit that raced another is made again on top of
    /// it (never lost). Blocking (`edit` compiles): call it on the blocking
    /// pool, as every edit is.
    pub fn edit_draft(
        &self,
        name: &str,
        expected: Option<u64>,
        edit: impl Fn(&Compiled) -> Result<Compiled, StoreError>,
    ) -> Result<Snapshot, StoreError> {
        let _edit = self.edit_lock();
        let mut attempt = 0;
        loop {
            attempt += 1;
            let draft = self.draft(name)?;
            if let Some(e) = expected.filter(|e| *e != draft.revision) {
                return Err(StoreError::Stale {
                    name: name.to_string(),
                    expected: e,
                    current: draft.revision,
                });
            }
            let compiled = edit(&draft)?;
            match self.set_draft_if(name, Some(compiled), None, Some(draft.revision)) {
                Err(StoreError::Stale { .. }) if expected.is_none() && attempt < 3 => continue,
                r => return r,
            }
        }
    }

    /// Set one operation's settings (an [`Self::edit_draft`]).
    pub fn set_operation(
        &self,
        name: &str,
        op: &str,
        settings: Option<OperationSettings>,
        expected: Option<u64>,
    ) -> Result<Snapshot, StoreError> {
        self.edit_draft(name, expected, |draft| {
            if draft.spec.operation(op).is_none() {
                return Err(StoreError::NotFound(format!("{name}/{op}")));
            }
            let mut new_settings = draft.settings.clone();
            match &settings {
                Some(s) if !s.is_empty() => {
                    new_settings.operations.insert(op.to_string(), s.clone());
                }
                _ => {
                    new_settings.operations.remove(op);
                }
            }
            self.recompile(draft, None, new_settings)
                .map_err(StoreError::Invalid)
        })
    }

    /// Make the draft live.
    pub fn deploy(&self, name: &str, force: bool) -> Result<Snapshot, StoreError> {
        self.deploy_if(name, force, None)
    }

    /// [`Self::deploy`], refused with [`StoreError::Stale`] unless the draft
    /// is revision `expected` (checked under the write lock, so the revision
    /// that goes live is the one named).
    pub fn deploy_if(
        &self,
        name: &str,
        force: bool,
        expected: Option<u64>,
    ) -> Result<Snapshot, StoreError> {
        let r = self.deploy_inner(name, force, expected);
        self.saved(r)
    }

    fn deploy_inner(
        &self,
        name: &str,
        force: bool,
        expected: Option<u64>,
    ) -> Result<Snapshot, StoreError> {
        let mut map = self.write();
        let e = map
            .get_mut(name)
            .ok_or_else(|| StoreError::NotFound(name.to_string()))?;
        if let Some(x) = expected.filter(|x| *x != e.draft.revision) {
            return Err(StoreError::Stale {
                name: name.to_string(),
                expected: x,
                current: e.draft.revision,
            });
        }
        if e.live.is_none() {
            if let Some(error) = &e.load_error {
                return Err(StoreError::Conflict(format!(
                    "this mock could not be loaded from its file ({error}): fix the definition and reload"
                )));
            }
        }
        if !force && !e.draft.report.ok {
            return Err(StoreError::ChecksFailed(Box::new(e.draft.report.clone())));
        }
        if e.live
            .as_ref()
            .is_some_and(|l| l.revision == e.draft.revision)
        {
            return Ok(snapshot(name, e));
        }
        if let Some(prev) = e.live.take() {
            e.history.push_back(prev);
            while e.history.len() > self.limits.revisions {
                e.history.pop_front();
            }
        }
        e.live = Some(e.draft.clone());
        e.updated_at = Utc::now();
        Ok(snapshot(name, e))
    }

    /// Make an earlier revision live again (default: the previous one). The
    /// draft follows so edits continue from it.
    pub fn rollback(&self, name: &str, revision: Option<u64>) -> Result<Snapshot, StoreError> {
        let r = self.rollback_inner(name, revision);
        self.saved(r)
    }

    fn rollback_inner(&self, name: &str, revision: Option<u64>) -> Result<Snapshot, StoreError> {
        let mut map = self.write();
        let e = map
            .get_mut(name)
            .ok_or_else(|| StoreError::NotFound(name.to_string()))?;
        let idx = match revision {
            Some(r) => e.history.iter().position(|c| c.revision == r),
            None => e.history.len().checked_sub(1),
        }
        .ok_or_else(|| StoreError::Invalid("no such revision to roll back to".into()))?;
        let Some(target) = e.history.remove(idx) else {
            return Err(StoreError::Invalid(
                "no such revision to roll back to".into(),
            ));
        };
        if let Some(prev) = e.live.take() {
            e.history.push_back(prev);
            while e.history.len() > self.limits.revisions {
                e.history.pop_front();
            }
        }
        e.keep_earlier_draft(self.limits.revisions);
        e.draft = target.clone();
        e.live = Some(target);
        e.updated_at = Utc::now();
        Ok(snapshot(name, e))
    }

    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<Snapshot, StoreError> {
        let r = self.set_enabled_inner(name, enabled);
        self.saved(r)
    }

    fn set_enabled_inner(&self, name: &str, enabled: bool) -> Result<Snapshot, StoreError> {
        let mut map = self.write();
        let e = map
            .get_mut(name)
            .ok_or_else(|| StoreError::NotFound(name.to_string()))?;
        e.enabled = enabled;
        e.updated_at = Utc::now();
        Ok(snapshot(name, e))
    }

    /// Copy a mock under a new name: its draft (shared spec, new revision),
    /// deployed when the source is, switched on, with empty traffic and state.
    pub fn duplicate(&self, name: &str, new_name: &str) -> Result<Snapshot, StoreError> {
        let source = self
            .get(name)
            .ok_or_else(|| StoreError::NotFound(name.to_string()))?;
        let compiled = self
            .recompile(&source.draft, None, source.draft.settings.clone())
            .map_err(StoreError::Invalid)?;
        let origin = match source.origin {
            Origin::Sample | Origin::File => Origin::Api,
            o => o,
        };
        self.create_in(
            new_name,
            compiled,
            origin,
            source.owner.clone(),
            true,
            source.live.is_some(),
            true,
            source.project.clone(),
        )
    }

    pub fn delete(&self, name: &str) -> bool {
        let removed = self.delete_quietly(name);
        if removed {
            self.persist_now(name);
        }
        removed
    }

    /// Delete without touching the saved copy.
    pub fn delete_quietly(&self, name: &str) -> bool {
        self.write().remove(name).is_some()
    }

    /// Write `compiled` as the draft of `name`, deploy it (forced) and set it
    /// on or off, in one write: refused with [`StoreError::Stale`] unless
    /// the draft is still revision `base` (an apply's update).
    fn replace_deployed(
        &self,
        name: &str,
        compiled: Compiled,
        enabled: bool,
        base: u64,
    ) -> Result<Snapshot, StoreError> {
        let r = {
            let mut map = self.write();
            match map.get_mut(name) {
                None => Err(StoreError::NotFound(name.to_string())),
                Some(e) if e.draft.revision != base => Err(StoreError::Stale {
                    name: name.to_string(),
                    expected: base,
                    current: e.draft.revision,
                }),
                Some(e) => {
                    if compiled.source != e.draft.source {
                        e.load_error = None;
                    }
                    let draft = Arc::new(compiled);
                    if let Some(prev) = e.live.take() {
                        e.history.push_back(prev);
                        while e.history.len() > self.limits.revisions {
                            e.history.pop_front();
                        }
                    }
                    e.keep_earlier_draft(self.limits.revisions);
                    e.live = Some(draft.clone());
                    e.draft = draft;
                    e.enabled = enabled;
                    e.updated_at = Utc::now();
                    Ok(snapshot(name, e))
                }
            }
        };
        self.saved(r)
    }

    /// Put a checkpoint's mock in place of whatever has its name, in one
    /// write: `live` deployed (with `draft` on top when it differs), else
    /// `draft` undeployed. Built before the swap, so a mock that cannot be
    /// restored is left as it was, never lost. Not saved (the caller saves
    /// it once its data is back).
    pub fn restore_replacing(
        &self,
        name: &str,
        live: Option<Compiled>,
        draft: Option<Compiled>,
        enabled: bool,
    ) -> Result<Snapshot, StoreError> {
        let (first, on_top, deploy) = match (live, draft) {
            (Some(l), d) => (l, d, true),
            (None, Some(d)) => (d, None, false),
            (None, None) => {
                return Err(StoreError::Invalid(format!(
                    "{name}: no revision to restore"
                )))
            }
        };
        Self::check_new(name, &first, deploy, true)?;
        let owner = Owner::Instance;
        let mut map = self.write();
        self.admit(&map, name, &owner, true)?;
        let mut entry = self.new_entry(first, Origin::Api, owner, enabled, deploy);
        if let Some(d) = on_top {
            entry.draft = Arc::new(d);
        }
        let snap = snapshot(name, &entry);
        map.insert(name.to_string(), entry);
        Ok(snap)
    }

    /// Recreate a saved mock: `live` deployed, with `draft` on top when it
    /// differs. Not saved again.
    pub fn restore(
        &self,
        name: &str,
        live: Compiled,
        draft: Option<Compiled>,
        origin: Origin,
        owner: Owner,
        enabled: bool,
    ) -> Result<Snapshot, StoreError> {
        let snap = self.create_inner(name, live, origin, owner, enabled, true, true, None)?;
        match draft {
            Some(d) => self.set_draft_inner(name, Some(d), None, None),
            None => Ok(snap),
        }
    }

    /// Recreate a saved mock that was never deployed.
    pub fn restore_undeployed(
        &self,
        name: &str,
        draft: Compiled,
        origin: Origin,
        owner: Owner,
        enabled: bool,
    ) -> Result<Snapshot, StoreError> {
        self.create_inner(name, draft, origin, owner, enabled, false, false, None)
    }

    fn set_load_error(&self, name: &str, error: Option<&str>) {
        if let Some(e) = self.write().get_mut(name) {
            e.load_error = error.map(str::to_string);
        }
    }

    /// Record that the definition of `name` could not be loaded. An existing
    /// mock keeps its revisions (and keeps answering) with the error shown;
    /// a new one is created switched off and not deployed, so the console
    /// lists it with the error instead of hiding it.
    pub fn mark_failed(&self, name: &str, origin: Origin, error: &str) {
        let error: String = error.chars().take(2000).collect();
        if self.get(name).is_none() {
            if !valid_name(name) {
                return;
            }
            let title = serde_json::to_string(name).unwrap_or_default();
            let stub = format!(
                r#"{{"openapi":"3.0.3","info":{{"title":{title},"version":"0"}},"paths":{{}}}}"#
            );
            let Ok(compiled) = self.compile(&stub, MockSettings::default()) else {
                return;
            };
            if self
                .create_inner(
                    name,
                    compiled,
                    origin,
                    Owner::Instance,
                    false,
                    false,
                    false,
                    None,
                )
                .is_err()
            {
                return;
            }
        }
        self.set_load_error(name, Some(&error));
    }

    /// Apply definitions: create, update (draft + deploy, forced) or leave
    /// unchanged; with `prune`, delete mocks of `prune_origin` that are not
    /// listed.
    pub fn apply(
        &self,
        defs: Vec<Definition>,
        origin: Origin,
        prune: Option<Origin>,
    ) -> ApplyReport {
        self.apply_keeping(defs, origin, prune, &[])
    }

    /// [`Self::apply`]; the mocks named in `keep` are never pruned (file
    /// entries that failed to load).
    pub fn apply_keeping(
        &self,
        defs: Vec<Definition>,
        origin: Origin,
        prune: Option<Origin>,
        keep: &[String],
    ) -> ApplyReport {
        let mut report = ApplyReport::default();
        let mut listed: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
        listed.extend(keep.iter().cloned());
        for def in defs {
            // One edit per definition: read, recompile and write under the
            // edit lock, so a concurrent edit is never deployed by it nor
            // lost to it.
            let _edit = self.edit_lock();
            let existing = self.get(&def.name);
            // The same document in another formatting (an export re-applied)
            // is the same spec.
            let same_spec = existing.as_ref().is_some_and(|s| {
                &*s.draft.source == def.source.as_str()
                    || super::spec::parse_document(&def.source).ok().as_ref()
                        == Some(&s.draft.spec.root)
            });
            let moved = existing
                .as_ref()
                .is_some_and(|s| !s.owner.is_visitor() && s.project != def.project);
            if moved {
                let _ = self.set_project(&def.name, def.project.clone());
            }
            if let Some(s) = &existing {
                let same = same_spec
                    && s.draft.settings == def.settings.clone().normalised()
                    && s.live.as_ref().map(|l| l.revision) == Some(s.draft.revision);
                if same {
                    self.set_load_error(&def.name, None);
                    if s.enabled != def.enabled {
                        let _ = self.set_enabled(&def.name, def.enabled);
                        report.updated.push(def.name);
                    } else if moved {
                        report.updated.push(def.name);
                    } else {
                        report.unchanged.push(def.name);
                    }
                    continue;
                }
            }
            let compiled = match &existing {
                Some(s) if same_spec => self.recompile(&s.draft, None, def.settings),
                Some(s) => self.recompile(&s.draft, Some(&def.source), def.settings),
                None => self.compile(&def.source, def.settings),
            };
            let compiled = match compiled {
                Ok(c) => c,
                Err(e) => {
                    report.errors.push(json!({ "name": def.name, "error": e }));
                    continue;
                }
            };
            let warnings = compiled.report.errors;
            let result = match existing {
                Some(s) => self
                    .replace_deployed(&def.name, compiled, def.enabled, s.draft.revision)
                    .map(|_| report.updated.push(def.name.clone())),
                None => self
                    .create_in(
                        &def.name,
                        compiled,
                        origin,
                        Owner::Instance,
                        def.enabled,
                        true,
                        true,
                        def.project.clone(),
                    )
                    .map(|_| report.created.push(def.name.clone())),
            };
            if result.is_ok() {
                self.set_load_error(&def.name, None);
            }
            match result {
                Err(e) => report
                    .errors
                    .push(json!({ "name": def.name, "error": e.message() })),
                Ok(()) if warnings > 0 => report.errors.push(json!({
                    "name": def.name,
                    "error": format!("deployed with {warnings} check error(s), see the deploy report"),
                    "deployed": true,
                })),
                Ok(()) => {}
            }
        }
        if let Some(prune_origin) = prune {
            let stale: Vec<String> = self
                .list()
                .into_iter()
                .filter(|s| s.origin == prune_origin && s.prunable() && !listed.contains(&s.name))
                .map(|s| s.name)
                .collect();
            for name in stale {
                if self.delete(&name) {
                    report.deleted.push(name);
                }
            }
        }
        report
    }
}

fn snapshot(name: &str, e: &Entry) -> Snapshot {
    Snapshot {
        name: name.to_string(),
        origin: e.origin,
        owner: e.owner.clone(),
        created_at: e.created_at,
        updated_at: e.updated_at,
        enabled: e.enabled,
        draft: e.draft.clone(),
        live: e.live.clone(),
        history: e.history.iter().cloned().collect(),
        capture: e.capture.clone(),
        state: e.state.clone(),
        expires_at: e.expires.map(|(_, at)| at),
        scratch: e.scratch,
        load_error: e.load_error.clone(),
        project: e.project.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SPEC: &str = r#"{"openapi":"3.0.3","info":{"title":"T","version":"1"},
      "paths":{"/a":{"get":{"operationId":"a","responses":{"200":{"description":"ok",
        "content":{"application/json":{"schema":{"type":"object","properties":{"n":{"type":"integer"}}}}}}}}}}}"#;

    fn store() -> MockStore {
        MockStore::new(&Config::for_tests())
    }

    fn bad_settings() -> MockSettings {
        serde_json::from_value(json!({"operations": {"a": {"respond": {"body": {"n": "x"}}}}}))
            .expect("settings")
    }

    #[test]
    fn lifecycle() {
        let s = store();
        let c = s.compile(SPEC, MockSettings::default()).expect("compile");
        let snap = s
            .create("t", c, Origin::Api, true, false, false)
            .expect("create");
        assert_eq!(snap.state(), "draft");
        assert!(matches!(s.lookup("t"), Lookup::NotDeployed(..)));
        let first = s.deploy("t", false).expect("deploy");
        assert_eq!(first.state(), "live");
        assert!(!first.has_draft_changes());
        let r1 = first.draft.revision;

        // A failing draft does not deploy without force.
        let bad = s.compile(SPEC, bad_settings()).expect("compile");
        s.set_draft("t", Some(bad), None).expect("draft");
        assert!(matches!(
            s.deploy("t", false),
            Err(StoreError::ChecksFailed(_))
        ));
        let forced = s.deploy("t", true).expect("forced");
        assert_eq!(forced.history.len(), 1);

        let back = s.rollback("t", None).expect("rollback");
        assert_eq!(back.live.as_ref().expect("live").revision, r1);
        assert_eq!(back.draft.revision, r1);

        let off = s.set_enabled("t", false).expect("off");
        assert_eq!(off.state(), "off");
        assert!(matches!(
            s.lookup("t"),
            Lookup::Ready(LiveMock { enabled: false, .. })
        ));

        s.set_draft("t", None, Some("u")).expect("rename");
        assert!(s.get("t").is_none());
        assert!(s.get("u").is_some());
        assert!(s.delete("u"));
        assert!(matches!(s.lookup("u"), Lookup::Missing));
    }

    #[test]
    fn names_conflicts_and_caps() {
        let mut limits = MockLimits::for_config(&Config::for_tests());
        limits.max_mocks = 1;
        let s = MockStore::with_limits(limits);
        let c = || s.compile(SPEC, MockSettings::default()).expect("compile");
        assert!(matches!(
            s.create("Bad Name", c(), Origin::Api, true, true, false),
            Err(StoreError::Invalid(_))
        ));
        s.create("a", c(), Origin::Api, true, true, false)
            .expect("a");
        assert!(matches!(
            s.create("a", c(), Origin::Api, true, true, false),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            s.create("b", c(), Origin::Api, true, true, false),
            Err(StoreError::Full(_))
        ));
        let bad = s.compile(SPEC, bad_settings()).expect("compile");
        s.delete("a");
        assert!(matches!(
            s.create("b", bad, Origin::Api, true, true, false),
            Err(StoreError::ChecksFailed(_))
        ));
        assert!(s.is_empty());
    }

    #[test]
    fn api_mocks_expire_in_public_mode() {
        let mut limits = MockLimits::for_config(&Config::for_tests());
        limits.ttl_secs = Some(0);
        let s = MockStore::with_limits(limits);
        let c = s.compile(SPEC, MockSettings::default()).expect("compile");
        s.create("t", c, Origin::Api, true, true, false)
            .expect("create");
        assert!(s.get("t").is_none());
        let c = s.compile(SPEC, MockSettings::default()).expect("compile");
        s.create("f", c, Origin::File, true, true, false)
            .expect("file mock");
        assert!(s.get("f").is_some());
    }

    #[test]
    fn apply_creates_updates_and_prunes() {
        let s = store();
        let def = |name: &str, enabled: bool| Definition {
            name: name.into(),
            source: SPEC.into(),
            settings: MockSettings::default(),
            enabled,
            project: None,
        };
        let r = s.apply(vec![def("a", true), def("b", true)], Origin::File, None);
        assert_eq!(r.created, vec!["a", "b"]);
        let r = s.apply(vec![def("a", false)], Origin::File, Some(Origin::File));
        assert_eq!(r.updated, vec!["a"]);
        assert_eq!(r.deleted, vec!["b"]);
        let r = s.apply(vec![def("a", false)], Origin::File, None);
        assert_eq!(r.unchanged, vec!["a"]);
        let broken = Definition {
            name: "c".into(),
            source: "nope".into(),
            settings: MockSettings::default(),
            enabled: true,
            project: None,
        };
        let r = s.apply(vec![broken], Origin::File, None);
        assert_eq!(r.errors.len(), 1);
    }

    #[test]
    fn duplicate_copies_the_draft() {
        let s = store();
        let c = s.compile(SPEC, MockSettings::default()).expect("compile");
        s.create("a", c, Origin::File, false, true, false)
            .expect("a");
        let copy = s.duplicate("a", "b").expect("duplicate");
        assert_eq!(copy.origin, Origin::Api);
        assert_eq!(copy.state(), "live");
        let a = s.get("a").expect("a");
        assert!(Arc::ptr_eq(&a.draft.spec, &copy.draft.spec));
        assert_ne!(a.draft.revision, copy.draft.revision);
        assert!(matches!(
            s.duplicate("a", "b"),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            s.duplicate("x", "y"),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn a_visitor_gets_a_bounded_number_of_compiles_per_minute() {
        let s = store();
        for _ in 0..VISITOR_COMPILES_PER_MINUTE {
            assert!(s.allow_visitor_compile("visit-a"));
        }
        assert!(!s.allow_visitor_compile("visit-a"));
        assert!(s.allow_visitor_compile("visit-b"), "per visitor");
    }

    // ── Review 0.3.1 step 06 ────────────────────────────────────────

    #[test]
    fn revision_numbers_continue_across_a_restart() {
        // Two stores stand for one instance before and after a restart: an
        // If-Match from before can never name a draft compiled after.
        let before = store();
        let mut last = 0;
        for _ in 0..3 {
            last = before
                .compile(SPEC, MockSettings::default())
                .expect("compile")
                .revision;
        }
        std::thread::sleep(Duration::from_millis(5));
        let after = store();
        let first = after
            .compile(SPEC, MockSettings::default())
            .expect("compile")
            .revision;
        assert!(
            first > last,
            "revision {first} after a restart, {last} before"
        );
    }

    #[test]
    fn a_deploy_naming_a_revision_refuses_another_draft() {
        let s = store();
        let c = s.compile(SPEC, MockSettings::default()).expect("compile");
        let made = s
            .create("t", c, Origin::Api, true, false, false)
            .expect("create");
        let seen = made.draft.revision;
        let newer = s.compile(SPEC, MockSettings::default()).expect("compile");
        s.set_draft("t", Some(newer), None).expect("draft");
        match s.deploy_if("t", false, Some(seen)) {
            Err(StoreError::Stale {
                expected, current, ..
            }) => {
                assert_eq!(expected, seen);
                assert!(current > seen);
            }
            other => panic!("expected a stale refusal, got {:?}", other.map(|s| s.name)),
        }
        assert!(s.get("t").expect("t").live.is_none(), "nothing went live");
        let current = s.get("t").expect("t").draft.revision;
        let live = s.deploy_if("t", false, Some(current)).expect("deploy");
        assert_eq!(live.live.as_ref().map(|l| l.revision), Some(current));
    }

    #[test]
    fn a_scratch_mock_is_never_saved() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let s = MockStore::new(&Config::for_tests()).with_persistence(tmp.path().to_path_buf());
        let c = s.compile(SPEC, MockSettings::default()).expect("compile");
        let snap = s
            .create_scratch("scn-x", c, Duration::from_secs(60))
            .expect("scratch");
        assert!(snap.scratch);
        // A later mutation saves the mock it changed: never this one.
        s.set_enabled("scn-x", false).expect("off");
        s.deploy("scn-x", true).expect("deploy");
        assert!(
            !tmp.path().join("scn-x.json").exists(),
            "the scratch mock was saved"
        );
        assert!(s.get("scn-x").expect("listed").scratch);
        // An ordinary mock still is.
        let c = s.compile(SPEC, MockSettings::default()).expect("compile");
        s.create("kept", c, Origin::Api, true, true, false)
            .expect("create");
        assert!(tmp.path().join("kept.json").exists());
    }

    #[test]
    fn an_apply_waits_for_an_edit_in_progress() {
        let s = store();
        let c = s.compile(SPEC, MockSettings::default()).expect("compile");
        s.create("a", c, Origin::File, true, true, false)
            .expect("a");
        let before = s.get("a").expect("a").draft.revision;
        let edit = s.edit_lock();
        let applying = {
            let s = s.clone();
            std::thread::spawn(move || {
                let settings: MockSettings =
                    serde_json::from_value(json!({"responses": {"mode": "static"}}))
                        .expect("settings");
                s.apply(
                    vec![Definition {
                        name: "a".into(),
                        source: SPEC.into(),
                        settings,
                        enabled: false,
                        project: None,
                    }],
                    Origin::File,
                    None,
                )
            })
        };
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            s.get("a").expect("a").draft.revision,
            before,
            "the apply wrote while an edit held the lock"
        );
        drop(edit);
        let report = applying.join().expect("apply");
        assert_eq!(report.updated, vec!["a"]);
        let a = s.get("a").expect("a");
        // Draft, live and off in one write.
        assert_eq!(a.live.as_ref().map(|l| l.revision), Some(a.draft.revision));
        assert!(!a.enabled);
        assert_eq!(a.history.len(), 1);
    }

    #[test]
    fn a_restore_replaces_a_mock_in_one_write_within_the_cap() {
        let mut limits = MockLimits::for_config(&Config::for_tests());
        limits.max_mocks = 1;
        let s = MockStore::with_limits(limits);
        let c = s.compile(SPEC, MockSettings::default()).expect("compile");
        s.create("a", c, Origin::Api, true, true, false).expect("a");
        // At the cap: the replaced mock does not count.
        let live = s.compile(SPEC, MockSettings::default()).expect("compile");
        let draft = s.compile(SPEC, bad_settings()).expect("compile");
        let (lr, dr) = (live.revision, draft.revision);
        let back = s
            .restore_replacing("a", Some(live), Some(draft), false)
            .expect("restored");
        assert_eq!(back.live.as_ref().map(|l| l.revision), Some(lr));
        assert_eq!(back.draft.revision, dr);
        assert!(!back.enabled);
        // A second name is still refused.
        let other = s.compile(SPEC, MockSettings::default()).expect("compile");
        assert!(matches!(
            s.restore_replacing("b", Some(other), None, true),
            Err(StoreError::Full(_))
        ));
        assert!(matches!(
            s.restore_replacing("a", None, None, true),
            Err(StoreError::Invalid(_))
        ));
        assert!(s.get("a").is_some(), "a failed restore keeps the mock");
    }

    #[test]
    fn file_work_leaves_the_async_workers() {
        // One worker: without block_in_place a blocking save would stop the
        // other task on it.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let ticker = tokio::spawn(async {
                let start = Instant::now();
                tokio::time::sleep(Duration::from_millis(20)).await;
                start.elapsed()
            });
            let blocker = tokio::spawn(async {
                tokio::task::yield_now().await;
                off_runtime(|| std::thread::sleep(Duration::from_millis(400)));
            });
            let waited = ticker.await.expect("ticker");
            blocker.await.expect("blocker");
            assert!(
                waited < Duration::from_millis(300),
                "the worker was held for {waited:?}"
            );
        });
        // Outside a runtime and on a current-thread one it runs in place.
        assert_eq!(off_runtime(|| 7), 7);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        assert_eq!(rt.block_on(async { off_runtime(|| 8) }), 8);
    }
}
