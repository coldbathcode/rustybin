//! The history of saved-test runs: every run (console, CLI, API, MCP, and
//! later a schedule) kept with its trigger, the caller's principal (never a
//! credential), the tests, the counts, each test's result and the change
//! since the previous run. One JSON file per run in `<state>/test-runs/`, at
//! most [`MAX_RUNS`] and [`MAX_AGE_DAYS`] days, oldest out; without a state
//! directory, in memory ([`MAX_MEMORY_RUNS`]). Variables given to a run are
//! never kept: a value is replaced by its `{{NAME}}` before the run is.
//!
//! With a state directory only a run's summary is held in memory (the
//! details are read from its file when asked for), so the history's memory
//! stays small whatever a run's failures say.

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::run::Report;

/// Most runs kept with a state directory.
pub const MAX_RUNS: usize = 200;
/// Most runs kept in memory (no state directory).
pub const MAX_MEMORY_RUNS: usize = 50;
/// The oldest run kept, in days.
pub const MAX_AGE_DAYS: i64 = 30;
/// A page of the list: the default and the most.
pub const DEFAULT_PAGE: usize = 20;
pub const MAX_PAGE: usize = 100;
/// The directory under the state directory.
pub const DIR: &str = "test-runs";
/// The largest run file read back.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// The id's time part: `20261103T101500123Z`.
const ID_TIME: &str = "%Y%m%dT%H%M%S%3fZ";
/// The longest trigger and principal kept.
const MAX_LABEL: usize = 120;

/// Who or what started a run: `console`, `cli`, `api`, `mcp` or
/// `schedule:<name>`. A label for people, never trusted for anything else.
pub fn trigger_from_headers(headers: &axum::http::HeaderMap) -> &'static str {
    match headers
        .get(CLIENT_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
    {
        Some(v) if v.eq_ignore_ascii_case("console") => "console",
        Some(v) if v.eq_ignore_ascii_case("cli") => "cli",
        _ => "api",
    }
}

/// The header the console and the CLI send to say which they are.
pub const CLIENT_HEADER: &str = "x-rustybin-client";

/// What changed since the previous run of the same tests (else the
/// previous run): the tests that failed there and pass now, and the other way.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// The run compared with (`None`: the first run kept).
    #[serde(default)]
    pub previous: Option<String>,
    #[serde(default)]
    pub newly_failing: Vec<String>,
    #[serde(default)]
    pub newly_passing: Vec<String>,
}

/// One kept run, as in its file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestRun {
    pub id: String,
    pub trigger: String,
    pub principal: String,
    /// The tests asked for; empty: every saved test.
    #[serde(default)]
    pub selection: Vec<String>,
    #[serde(default)]
    pub change: Change,
    /// The projects of the tests it ran (step 11).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,
    /// A test of the instance's (without a project) ran; every run kept
    /// before projects says so.
    #[serde(default = "instance_default")]
    pub instance: bool,
    #[serde(flatten)]
    pub report: Report,
}

fn instance_default() -> bool {
    true
}

/// Whose tests a run ran: the projects, and whether the instance's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub projects: Vec<String>,
    pub instance: bool,
}

impl Scope {
    /// The instance's tests only (every run before projects).
    pub fn instance() -> Self {
        Self {
            projects: Vec::new(),
            instance: true,
        }
    }

    /// The scope of the tests run.
    pub fn of(tests: &[super::SavedTest]) -> Self {
        let mut projects: Vec<String> = Vec::new();
        let mut instance = false;
        for t in tests {
            match &t.project {
                Some(p) if !projects.contains(p) => projects.push(p.clone()),
                Some(_) => {}
                None => instance = true,
            }
        }
        // An empty selection ran nothing of anyone's: the instance's.
        if tests.is_empty() {
            instance = true;
        }
        projects.sort();
        Self { projects, instance }
    }
}

impl TestRun {
    fn outcomes(&self) -> Vec<(String, bool)> {
        self.report
            .results
            .iter()
            .map(|r| (r.name.clone(), r.passed))
            .collect()
    }
}

/// A run's summary, what the list shows (and what the next run compares with).
#[derive(Clone, Debug)]
struct Kept {
    id: String,
    at: DateTime<Utc>,
    trigger: String,
    principal: String,
    passed: bool,
    total: usize,
    failed: usize,
    errors: usize,
    duration_ms: u64,
    ran_at: String,
    change: Change,
    outcomes: Vec<(String, bool)>,
    scope: Scope,
    /// The whole run, when it is not (or could not be) in a file.
    full: Option<Arc<TestRun>>,
}

impl Kept {
    fn of(run: &TestRun, at: DateTime<Utc>, full: bool) -> Self {
        let r = &run.report;
        Self {
            id: run.id.clone(),
            at,
            trigger: run.trigger.clone(),
            principal: run.principal.clone(),
            passed: r.passed,
            total: r.total,
            failed: r.failed,
            errors: r.errors,
            duration_ms: r.duration_ms,
            ran_at: r.ran_at.clone(),
            change: run.change.clone(),
            outcomes: run.outcomes(),
            scope: Scope {
                projects: run.projects.clone(),
                instance: run.instance,
            },
            full: full.then(|| Arc::new(run.clone())),
        }
    }

    fn view(&self) -> Value {
        let failing: Vec<&str> = self
            .outcomes
            .iter()
            .filter(|(_, p)| !p)
            .map(|(n, _)| n.as_str())
            .collect();
        json!({
            "id": self.id,
            "ran_at": self.ran_at,
            "trigger": self.trigger,
            "principal": self.principal,
            "passed": self.passed,
            "total": self.total,
            "failed": self.failed,
            "errors": self.errors,
            "duration_ms": self.duration_ms,
            "failing": failing,
            "change": self.change,
        })
    }

    fn names(&self) -> BTreeSet<&str> {
        self.outcomes.iter().map(|(n, _)| n.as_str()).collect()
    }
}

/// An id a run may have: its time and a few hex digits (never a path).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn time_of(id: &str) -> Option<DateTime<Utc>> {
    let t = id.split('-').next()?;
    NaiveDateTime::parse_from_str(t, ID_TIME)
        .ok()
        .map(|n| n.and_utc())
}

fn new_id(now: DateTime<Utc>) -> String {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!("{}-{}", now.format(ID_TIME), &suffix[..6])
}

fn label(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL)
        .collect()
}

/// A page of the history, newest first.
pub struct Page {
    pub runs: Vec<Value>,
    pub next_cursor: Option<String>,
    pub total: usize,
}

/// The history (`AppState.saved_tests.history`).
pub struct History {
    dir: Option<PathBuf>,
    cap: usize,
    max_age: chrono::Duration,
    /// Oldest first.
    runs: Mutex<VecDeque<Kept>>,
    /// One record or clear at a time (the change is computed under it).
    order: Mutex<()>,
}

impl History {
    /// The history, read back from `<state>/test-runs/` when a state
    /// directory is set.
    pub fn new(state_dir: Option<&std::path::Path>) -> Self {
        let dir = state_dir.map(|d| d.join(DIR));
        let cap = if dir.is_some() {
            MAX_RUNS
        } else {
            MAX_MEMORY_RUNS
        };
        let h = Self {
            dir,
            cap,
            max_age: chrono::Duration::days(MAX_AGE_DAYS),
            runs: Mutex::new(VecDeque::new()),
            order: Mutex::new(()),
        };
        h.load();
        h.prune(Utc::now());
        h
    }

    /// A history with a cap of its own (tests).
    #[cfg(test)]
    pub(crate) fn with_cap(state_dir: Option<&std::path::Path>, cap: usize) -> Self {
        let mut h = Self::new(None);
        h.dir = state_dir.map(|d| d.join(DIR));
        h.cap = cap;
        h.load();
        h
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<Kept>> {
        self.runs.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn load(&self) {
        let Some(dir) = &self.dir else { return };
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!("cannot read {}: {e}", dir.display());
                return;
            }
        };
        let mut ids: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let id = name.strip_suffix(".json")?.to_string();
                valid_id(&id).then_some(id)
            })
            .collect();
        ids.sort();
        // Only the newest are read: the rest go at the first prune.
        let skip = ids.len().saturating_sub(self.cap);
        for old in &ids[..skip] {
            let _ = std::fs::remove_file(dir.join(format!("{old}.json")));
        }
        let mut runs = self.lock();
        for id in &ids[skip..] {
            let path = dir.join(format!("{id}.json"));
            let Some(at) = time_of(id) else {
                tracing::warn!("{}: not a run's name: skipped", path.display());
                continue;
            };
            let too_big = std::fs::metadata(&path)
                .map(|m| m.len() > MAX_FILE_BYTES)
                .unwrap_or(true);
            if too_big {
                tracing::warn!("{}: too large or unreadable: skipped", path.display());
                continue;
            }
            match std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|b| serde_json::from_slice::<TestRun>(&b).map_err(|e| e.to_string()))
            {
                Ok(run) if run.id == *id => runs.push_back(Kept::of(&run, at, false)),
                Ok(_) => tracing::warn!("{}: names another run: skipped", path.display()),
                Err(e) => tracing::warn!("{}: not a test run ({e}): skipped", path.display()),
            }
        }
    }

    /// Drop what is past the age or the count, with its file.
    fn prune(&self, now: DateTime<Utc>) {
        let gone: Vec<String> = {
            let mut runs = self.lock();
            let mut gone = Vec::new();
            while let Some(front) = runs.front() {
                if runs.len() > self.cap || now - front.at > self.max_age {
                    if let Some(k) = runs.pop_front() {
                        gone.push(k.id);
                    }
                } else {
                    break;
                }
            }
            gone
        };
        if let (Some(dir), false) = (&self.dir, gone.is_empty()) {
            crate::mocks::store::off_runtime(|| {
                for id in &gone {
                    let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
                }
            });
        }
    }

    /// The change against the previous run of the same tests, else the
    /// previous run.
    fn change_for(runs: &VecDeque<Kept>, outcomes: &[(String, bool)]) -> Change {
        let names: BTreeSet<&str> = outcomes.iter().map(|(n, _)| n.as_str()).collect();
        let prev = runs
            .iter()
            .rev()
            .find(|k| k.names() == names)
            .or_else(|| runs.back());
        let Some(prev) = prev else {
            return Change::default();
        };
        let before = |n: &str| prev.outcomes.iter().find(|(m, _)| m == n).map(|(_, p)| *p);
        let mut c = Change {
            previous: Some(prev.id.clone()),
            ..Change::default()
        };
        for (n, passed) in outcomes {
            match (before(n), passed) {
                (Some(true), false) => c.newly_failing.push(n.clone()),
                (Some(false), true) => c.newly_passing.push(n.clone()),
                _ => {}
            }
        }
        c
    }

    /// Keep a run: its id and change are set here. A file that cannot be
    /// written keeps the run in memory (with a warning): a run is never
    /// refused for its history.
    pub fn record(
        &self,
        trigger: &str,
        principal: &str,
        selection: Vec<String>,
        report: Report,
    ) -> TestRun {
        self.record_at(trigger, principal, selection, report, Utc::now())
    }

    pub(crate) fn record_at(
        &self,
        trigger: &str,
        principal: &str,
        selection: Vec<String>,
        report: Report,
        now: DateTime<Utc>,
    ) -> TestRun {
        self.record_in(
            trigger,
            principal,
            selection,
            report,
            Scope::instance(),
            now,
        )
    }

    /// [`Self::record`] of a run of `scope`'s tests.
    pub fn record_in(
        &self,
        trigger: &str,
        principal: &str,
        selection: Vec<String>,
        report: Report,
        scope: Scope,
        now: DateTime<Utc>,
    ) -> TestRun {
        let _order = self.order.lock().unwrap_or_else(|e| e.into_inner());
        let mut run = TestRun {
            id: new_id(now),
            trigger: label(trigger),
            principal: label(principal),
            selection,
            change: Change::default(),
            projects: scope.projects,
            instance: scope.instance,
            report,
        };
        run.change = Self::change_for(&self.lock(), &run.outcomes());
        let mut in_memory = self.dir.is_none();
        if let Some(dir) = &self.dir {
            let path = dir.join(format!("{}.json", run.id));
            let written = serde_json::to_vec_pretty(&run)
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    crate::mocks::store::off_runtime(|| {
                        crate::settings::write_private(&path, &bytes)
                    })
                });
            if let Err(e) = written {
                tracing::warn!("the test run {} is kept in memory only: {e}", run.id);
                in_memory = true;
            }
        }
        self.lock().push_back(Kept::of(&run, now, in_memory));
        drop(_order);
        self.prune(now);
        run
    }

    /// A page, newest first: `cursor` is the id of the last run of the
    /// previous page.
    pub fn page(&self, limit: usize, cursor: Option<&str>) -> Page {
        self.page_where(limit, cursor, |_| true)
    }

    /// [`Self::page`] of the runs whose scope `visible` accepts (step 11: a
    /// member of a project sees the runs of its tests).
    pub fn page_where(
        &self,
        limit: usize,
        cursor: Option<&str>,
        visible: impl Fn(&Scope) -> bool,
    ) -> Page {
        self.prune(Utc::now());
        let limit = limit.clamp(1, MAX_PAGE);
        let runs = self.lock();
        let newest_first: Vec<&Kept> = runs.iter().rev().filter(|k| visible(&k.scope)).collect();
        let after: &[&Kept] = match cursor {
            // Past the cursor's run; one gone since (pruned) by its time.
            Some(c) => match newest_first.iter().position(|k| k.id == c) {
                Some(i) => &newest_first[i + 1..],
                None => {
                    let i = newest_first
                        .iter()
                        .position(|k| k.id.as_str() < c)
                        .unwrap_or(newest_first.len());
                    &newest_first[i..]
                }
            },
            None => &newest_first,
        };
        let page: Vec<Value> = after.iter().take(limit).map(|k| k.view()).collect();
        let next_cursor = (after.len() > limit)
            .then(|| after.get(limit - 1).map(|k| k.id.clone()))
            .flatten();
        Page {
            runs: page,
            next_cursor,
            total: newest_first.len(),
        }
    }

    /// One run, whole.
    pub fn get(&self, id: &str) -> Option<TestRun> {
        if !valid_id(id) {
            return None;
        }
        let full = {
            let runs = self.lock();
            let k = runs.iter().find(|k| k.id == id)?;
            k.full.clone()
        };
        if let Some(f) = full {
            return Some((*f).clone());
        }
        let dir = self.dir.as_ref()?;
        let path = dir.join(format!("{id}.json"));
        crate::mocks::store::off_runtime(|| std::fs::read(&path))
            .ok()
            .and_then(|b| serde_json::from_slice::<TestRun>(&b).ok())
            .filter(|r| r.id == id)
    }

    /// Forget every run: the number forgotten.
    pub fn clear(&self) -> usize {
        let _order = self.order.lock().unwrap_or_else(|e| e.into_inner());
        let gone: Vec<String> = self.lock().drain(..).map(|k| k.id).collect();
        if let Some(dir) = &self.dir {
            crate::mocks::store::off_runtime(|| {
                for id in &gone {
                    let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
                }
            });
        }
        gone.len()
    }
}
