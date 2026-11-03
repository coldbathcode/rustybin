//! Saved tests on a schedule: every N minutes (`every`, `5m` to `24h`,
//! aligned to multiples of the interval since the Unix epoch) or at a time
//! of day (`at`, `HH:MM` UTC), on or off, with an optional failure webhook.
//! At most [`MAX_SCHEDULES`], kept in `<state>/test-schedules.json` (in
//! memory without a state directory) and in `rustybin.yaml`
//! (`test_schedules:`).
//!
//! One scheduler task per instance ([`scheduler_loop`]) starts the runs that
//! are due: a run never overlaps the previous run of its schedule (nor any
//! other run of the saved tests: [`super::Tests::begin_run`] is
//! single-flight, a due run waits for the next tick), and a run that would
//! start while the harness sends load is skipped and recorded as skipped in
//! the schedule's status. A run goes through [`super::run_and_keep`] with the
//! trigger `schedule:<name>`, so it is in the history like any other.
//! Variables come from `RUSTYBIN_TEST_*` only: a schedule never holds a
//! credential nor a variable's value.
//!
//! A failing run queues one notice for the schedule's webhook; a passing run
//! after a failing one queues one more when `notify_recovery` is on. Notices
//! are delivered by [`notice_loop`] under the egress purpose `notify` (no
//! default: the operator allows the webhook's host), with the
//! `notify.webhook_token` secret as a bearer token; a refusal is final and
//! reported in the status, a failed delivery is retried [`NOTICE_ATTEMPTS`]
//! times with a backoff, and the queue holds at most [`MAX_NOTICES`].
//!
//! On a cluster of replicas each instance runs the schedules it holds: two
//! replicas with the same document run each schedule twice (documented,
//! no election).

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::history::TestRun;
use super::run::RunBody;
use crate::catalog::{category, Endpoint, Example};
use crate::egress::Purpose;
use crate::limits::{Clock, SystemClock};
use crate::settings::{Kind, Spec};
use crate::state::AppState;

/// Most schedules of an instance.
pub const MAX_SCHEDULES: usize = 20;
/// The shortest and longest `every`, in seconds.
pub const MIN_EVERY_SECS: i64 = 5 * 60;
pub const MAX_EVERY_SECS: i64 = 24 * 3600;
/// Outcomes kept in a schedule's status.
pub const MAX_RECENT: usize = 10;
/// Notices waiting for delivery, all schedules together.
pub const MAX_NOTICES: usize = 50;
/// Delivery attempts of one notice.
pub const NOTICE_ATTEMPTS: u32 = 4;
/// How often the scheduler and the notice delivery look at the clock.
pub const TICK: Duration = Duration::from_secs(1);
/// The trigger of a scheduled run is `schedule:<name>`.
pub const TRIGGER_PREFIX: &str = "schedule:";
/// The principal of a scheduled run in the history.
pub const PRINCIPAL: &str = "scheduler";
/// The settings key of the webhook's bearer token.
pub const TOKEN_KEY: &str = "notify.webhook_token";
/// The settings key of the address links in notices start with.
pub const BASE_URL_KEY: &str = "notify.base_url";
const FILE: &str = "test-schedules.json";
const MAX_NAME: usize = 64;
const MAX_DESCRIPTION: usize = 200;
const NOTICE_TIMEOUT: Duration = Duration::from_secs(10);
/// The first retry waits this long, each next one four times longer.
const FIRST_BACKOFF_SECS: i64 = 15;
/// Most failing test names a notice lists.
const MAX_FAILING: usize = 50;

fn yes() -> bool {
    true
}

/// One schedule, as in `rustybin.yaml` (`test_schedules:`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The tests to run by name; with neither `names` nor `prefix`, every
    /// saved test.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
    /// The tests whose name starts with this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// `5m` to `24h` (`s`, `m` or `h`): runs at every multiple of it since
    /// the Unix epoch (`15m`: at :00, :15, :30 and :45).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<String>,
    /// `HH:MM`, UTC: once a day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Where a failing run's notice is posted (the egress purpose `notify`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook: Option<String>,
    /// Post one more notice when a run passes after a failing one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub notify_recovery: bool,
}

/// When a schedule runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timing {
    Every(i64),
    At(u32, u32),
}

/// A name a schedule may have (the saved tests' rules).
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// `15m`, `2h`, `300s`: seconds, from 5 minutes to 24 hours.
pub fn parse_every(raw: &str) -> Result<i64, String> {
    let bad = || format!("every {raw:?}: a duration from 5m to 24h, such as 15m or 2h");
    let t = raw.trim();
    if !t.is_ascii() || t.len() < 2 {
        return Err(bad());
    }
    let (num, unit) = t.split_at(t.len() - 1);
    if !num.bytes().all(|b| b.is_ascii_digit()) || num.len() > 6 {
        return Err(bad());
    }
    let n: i64 = num.parse().map_err(|_| bad())?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        _ => return Err(bad()),
    };
    if !(MIN_EVERY_SECS..=MAX_EVERY_SECS).contains(&secs) {
        return Err(bad());
    }
    Ok(secs)
}

/// `HH:MM` (UTC): the hour and the minute.
pub fn parse_at(raw: &str) -> Result<(u32, u32), String> {
    let bad = || format!("at {raw:?}: a time of day as HH:MM, UTC (02:30)");
    let b = raw.trim().as_bytes();
    if b.len() != 5 || b[2] != b':' {
        return Err(bad());
    }
    let digits = |s: &[u8]| -> Option<u32> {
        s.iter()
            .all(u8::is_ascii_digit)
            .then(|| s.iter().fold(0u32, |a, d| a * 10 + u32::from(d - b'0')))
    };
    match (digits(&b[..2]), digits(&b[3..])) {
        (Some(h), Some(m)) if h < 24 && m < 60 => Ok((h, m)),
        _ => Err(bad()),
    }
}

impl Schedule {
    /// Every rule a schedule follows (a document's too).
    pub fn validate(&self) -> Result<(), String> {
        if !valid_name(&self.name) {
            return Err(format!(
                "name {:?}: 1 to {MAX_NAME} letters, digits, . _ -",
                self.name
            ));
        }
        if self
            .description
            .as_deref()
            .is_some_and(|d| d.chars().count() > MAX_DESCRIPTION || d.chars().any(char::is_control))
        {
            return Err(format!(
                "description: at most {MAX_DESCRIPTION} characters, no control characters"
            ));
        }
        self.timing()?;
        if !self.names.is_empty() && self.prefix.is_some() {
            return Err("names or prefix, not both (neither: every saved test)".into());
        }
        if self.names.len() > super::MAX_TESTS {
            return Err(format!("names: at most {}", super::MAX_TESTS));
        }
        for (i, n) in self.names.iter().enumerate() {
            if !super::valid_name(n) {
                return Err(format!("names[{i}] {n:?}: not a test name"));
            }
            if self.names[..i].contains(n) {
                return Err(format!("names: {n:?} twice"));
            }
        }
        if let Some(p) = &self.prefix {
            if !valid_name(p) {
                return Err(format!(
                    "prefix {p:?}: 1 to {MAX_NAME} letters, digits, . _ -"
                ));
            }
        }
        if let Some(w) = &self.webhook {
            crate::egress::parse_url(w, false).map_err(|e| {
                format!(
                    "webhook: {e} (a credential goes in the {TOKEN_KEY} setting, never in the URL)"
                )
            })?;
        }
        Ok(())
    }

    /// When it runs (an invalid schedule is an error).
    pub fn timing(&self) -> Result<Timing, String> {
        match (&self.every, &self.at) {
            (Some(e), None) => parse_every(e).map(Timing::Every),
            (None, Some(a)) => parse_at(a).map(|(h, m)| Timing::At(h, m)),
            _ => Err("one of every (5m to 24h) or at (HH:MM, UTC)".into()),
        }
    }

    /// The first time it is due strictly after `now`.
    pub fn next_after(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self.timing().ok()? {
            Timing::Every(secs) => {
                let t = now.timestamp();
                DateTime::from_timestamp((t.div_euclid(secs) + 1) * secs, 0)
            }
            Timing::At(h, m) => {
                let today = now.date_naive().and_hms_opt(h, m, 0)?.and_utc();
                Some(if today > now {
                    today
                } else {
                    today + chrono::Duration::days(1)
                })
            }
        }
    }

    /// What it runs, in words (`every saved test`, `tests a, b`).
    pub fn selection_label(&self) -> String {
        if let Some(p) = &self.prefix {
            format!("tests named {p}*")
        } else if self.names.is_empty() {
            "every saved test".into()
        } else {
            format!("tests {}", self.names.join(", "))
        }
    }

    /// The tests a run asks for (empty: every saved test) and the named
    /// ones that are gone.
    fn resolve(&self, tests: &[super::SavedTest]) -> (Vec<String>, Vec<String>, bool) {
        if let Some(p) = &self.prefix {
            let names: Vec<String> = tests
                .iter()
                .filter(|t| t.name.starts_with(p.as_str()))
                .map(|t| t.name.clone())
                .collect();
            let none = names.is_empty();
            return (names, Vec::new(), none);
        }
        if self.names.is_empty() {
            return (Vec::new(), Vec::new(), tests.is_empty());
        }
        let (present, missing): (Vec<String>, Vec<String>) = self
            .names
            .iter()
            .cloned()
            .partition(|n| tests.iter().any(|t| &t.name == n));
        let none = present.is_empty();
        (present, missing, none)
    }
}

// ── Status ──────────────────────────────────────────────────────────

/// One run or skip of a schedule.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub at: String,
    /// `passed`, `failed` or `skipped`.
    pub result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The last notice of a schedule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoticeStatus {
    /// When it was queued (its key).
    pub at: String,
    /// `failed` or `recovered`.
    pub event: String,
    #[serde(default)]
    pub run_id: Option<String>,
    /// `pending`, `retrying`, `delivered`, `failed` (every attempt failed),
    /// `refused` (the egress policy) or `dropped` (the queue was full).
    pub result: String,
    pub attempts: u32,
    #[serde(default)]
    pub detail: Option<String>,
}

/// What a schedule did: its last run or skip, the last notice.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Status {
    #[serde(default)]
    pub runs: u64,
    #[serde(default)]
    pub skipped: u64,
    /// The last run or skip.
    #[serde(default)]
    pub last: Option<Outcome>,
    /// The last run kept in the history.
    #[serde(default)]
    pub last_run_id: Option<String>,
    /// Whether the last run that ran passed (a recovery is a pass after a
    /// `false` here).
    #[serde(default)]
    pub last_passed: Option<bool>,
    #[serde(default)]
    pub last_notice: Option<NoticeStatus>,
    /// The last outcomes, newest first.
    #[serde(default)]
    pub recent: Vec<Outcome>,
}

impl Status {
    fn push(&mut self, o: Outcome) {
        self.recent.insert(0, o.clone());
        self.recent.truncate(MAX_RECENT);
        self.last = Some(o);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Stored {
    saved_at: String,
    schedule: Schedule,
    #[serde(default)]
    status: Status,
}

#[derive(Clone, Debug)]
struct Entry {
    stored: Stored,
    /// The next time it is due (`None`: off).
    next: Option<DateTime<Utc>>,
    /// A run of it is going.
    running: bool,
}

#[derive(Serialize, Deserialize)]
struct Doc {
    version: u32,
    schedules: Vec<Stored>,
}

/// A notice waiting for delivery.
#[derive(Clone, Debug)]
struct Pending {
    schedule: String,
    /// The status's `last_notice.at` it updates.
    key: String,
    url: String,
    body: Value,
    attempts: u32,
    next: DateTime<Utc>,
}

/// Why a schedule change was refused.
#[derive(Debug)]
pub enum ScheduleError {
    Invalid(String),
    Exists(String),
    Full,
    NotFound(String),
    Io(String),
}

impl ScheduleError {
    pub fn status(&self) -> StatusCode {
        match self {
            ScheduleError::Invalid(_) => StatusCode::BAD_REQUEST,
            ScheduleError::Exists(_) => StatusCode::CONFLICT,
            ScheduleError::Full => StatusCode::INSUFFICIENT_STORAGE,
            ScheduleError::NotFound(_) => StatusCode::NOT_FOUND,
            ScheduleError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn message(&self) -> String {
        match self {
            ScheduleError::Invalid(m) => m.clone(),
            ScheduleError::Exists(n) => format!(
                "a schedule named {n:?} exists: PUT /_rustybin/tests/schedules/{n} replaces it"
            ),
            ScheduleError::Full => {
                format!("at most {MAX_SCHEDULES} test schedules: delete one first")
            }
            ScheduleError::NotFound(n) => format!("no test schedule named {n:?}"),
            ScheduleError::Io(e) => {
                format!("the schedules file could not be written ({e}): nothing changed")
            }
        }
    }
}

fn stamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The schedules of an instance (`AppState.saved_tests.schedules`).
pub struct Schedules {
    path: Option<PathBuf>,
    items: Mutex<BTreeMap<String, Entry>>,
    /// One change at a time (a user's or a run's), so a change built from a
    /// copy never loses another.
    save_lock: Mutex<()>,
    clock: RwLock<Arc<dyn Clock>>,
    notices: Mutex<VecDeque<Pending>>,
    /// Tests: act as though the harness sends load.
    #[cfg(test)]
    pub(crate) force_load: AtomicBool,
}

impl Schedules {
    /// The schedules, read from `<state>/test-schedules.json` when a state
    /// directory is set. Each one's next time is computed from now: a time
    /// missed while the instance was down is not caught up.
    pub fn new(state_dir: Option<&std::path::Path>) -> Self {
        let path = state_dir.map(|d| d.join(FILE));
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
        let now = clock.wall();
        let mut items = BTreeMap::new();
        if let Some(p) = &path {
            match std::fs::read(p) {
                Ok(bytes) => match serde_json::from_slice::<Doc>(&bytes) {
                    Ok(doc) => {
                        for mut s in doc.schedules.into_iter().take(MAX_SCHEDULES) {
                            match s.schedule.validate() {
                                Ok(()) => {
                                    s.status.recent.truncate(MAX_RECENT);
                                    let next = s
                                        .schedule
                                        .enabled
                                        .then(|| s.schedule.next_after(now))
                                        .flatten();
                                    items.insert(
                                        s.schedule.name.clone(),
                                        Entry {
                                            stored: s,
                                            next,
                                            running: false,
                                        },
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!("{}: schedule skipped: {e}", p.display())
                                }
                            }
                        }
                    }
                    Err(e) => tracing::warn!(
                        "{}: not a schedules document ({e}): starting empty",
                        p.display()
                    ),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!("cannot read {}: {e}", p.display()),
            }
        }
        Self {
            path,
            items: Mutex::new(items),
            save_lock: Mutex::new(()),
            clock: RwLock::new(clock),
            notices: Mutex::new(VecDeque::new()),
            #[cfg(test)]
            force_load: AtomicBool::new(false),
        }
    }

    /// Tests: a clock of their own (every next time is computed again).
    #[cfg(test)]
    pub(crate) fn set_clock(&self, clock: Arc<dyn Clock>) {
        let now = clock.wall();
        *self.clock.write().unwrap_or_else(|e| e.into_inner()) = clock;
        for e in self.lock().values_mut() {
            e.next = e
                .stored
                .schedule
                .enabled
                .then(|| e.stored.schedule.next_after(now))
                .flatten();
        }
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.read().unwrap_or_else(|e| e.into_inner()).wall()
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Entry>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn queue(&self) -> MutexGuard<'_, VecDeque<Pending>> {
        self.notices.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn order(&self) -> MutexGuard<'_, ()> {
        self.save_lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self, items: &BTreeMap<String, Entry>) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let doc = Doc {
            version: 1,
            schedules: items.values().map(|e| e.stored.clone()).collect(),
        };
        let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
        crate::mocks::store::off_runtime(|| crate::settings::write_private(path, &bytes))
    }

    /// A user's change: built on a copy, written, then held. A failed write
    /// changes nothing.
    fn change<T>(
        &self,
        change: impl FnOnce(&mut BTreeMap<String, Entry>) -> Result<T, ScheduleError>,
    ) -> Result<T, ScheduleError> {
        let _order = self.order();
        let mut next = self.lock().clone();
        let out = change(&mut next)?;
        self.write(&next).map_err(ScheduleError::Io)?;
        *self.lock() = next;
        Ok(out)
    }

    /// A run's change (its status): held at once, written with a warning
    /// when the file cannot be.
    fn record(&self, name: &str, f: impl FnOnce(&mut Entry)) {
        let _order = self.order();
        let snapshot = {
            let mut items = self.lock();
            let Some(e) = items.get_mut(name) else {
                return;
            };
            f(e);
            items.clone()
        };
        if let Err(e) = self.write(&snapshot) {
            tracing::warn!("the status of the test schedule {name} is not saved: {e}");
        }
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn list(&self) -> Vec<Schedule> {
        self.lock()
            .values()
            .map(|e| e.stored.schedule.clone())
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<Schedule> {
        self.lock().get(name).map(|e| e.stored.schedule.clone())
    }

    fn view(e: &Entry) -> Value {
        let s = &e.stored.schedule;
        let st = &e.stored.status;
        let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
        if let Value::Object(m) = &mut v {
            m.insert("saved_at".into(), json!(e.stored.saved_at));
            m.insert("selection".into(), json!(s.selection_label()));
            m.insert(
                "status".into(),
                json!({
                    "running": e.running,
                    "next_due": e.next.map(stamp),
                    "last_result": st.last.as_ref().map(|o| o.result.clone()),
                    "last_at": st.last.as_ref().map(|o| o.at.clone()),
                    "last_detail": st.last.as_ref().and_then(|o| o.detail.clone()),
                    "last_run_id": st.last_run_id,
                    "last_passed": st.last_passed,
                    "runs": st.runs,
                    "skipped": st.skipped,
                    "last_notice": st.last_notice,
                    "recent": st.recent,
                }),
            );
        }
        v
    }

    /// The schedules as the API lists them, with their status.
    pub fn views(&self) -> Vec<Value> {
        self.lock().values().map(Self::view).collect()
    }

    pub fn view_of(&self, name: &str) -> Option<Value> {
        self.lock().get(name).map(Self::view)
    }

    /// Save a schedule: `replace` lets it overwrite one of the same name
    /// (whose status it keeps).
    pub fn put(&self, schedule: Schedule, replace: bool) -> Result<Value, ScheduleError> {
        schedule.validate().map_err(ScheduleError::Invalid)?;
        let now = self.now();
        self.change(|items| {
            let old = items.get(&schedule.name);
            if old.is_some() && !replace {
                return Err(ScheduleError::Exists(schedule.name));
            }
            if old.is_none() && items.len() >= MAX_SCHEDULES {
                return Err(ScheduleError::Full);
            }
            let status = old.map(|e| e.stored.status.clone()).unwrap_or_default();
            let running = old.is_some_and(|e| e.running);
            let next = schedule.enabled.then(|| schedule.next_after(now)).flatten();
            let entry = Entry {
                stored: Stored {
                    saved_at: stamp(now),
                    schedule,
                    status,
                },
                next,
                running,
            };
            let view = Self::view(&entry);
            items.insert(entry.stored.schedule.name.clone(), entry);
            Ok(view)
        })
    }

    /// Delete a schedule (and its waiting notices): `Ok(false)` when there
    /// is none of that name.
    pub fn delete(&self, name: &str) -> Result<bool, ScheduleError> {
        if !self.lock().contains_key(name) {
            return Ok(false);
        }
        let gone = self.change(|items| Ok(items.remove(name).is_some()))?;
        self.queue().retain(|p| p.schedule != name);
        Ok(gone)
    }

    /// The schedules due at `now`, earliest first (on, not running).
    fn due(&self, now: DateTime<Utc>) -> Vec<String> {
        let mut due: Vec<(DateTime<Utc>, String)> = self
            .lock()
            .values()
            .filter(|e| !e.running)
            .filter_map(|e| {
                e.next
                    .filter(|n| *n <= now)
                    .map(|n| (n, e.stored.schedule.name.clone()))
            })
            .collect();
        due.sort();
        due.into_iter().map(|(_, n)| n).collect()
    }

    /// Mark a run as going: the schedule, or `None` when it is gone or a
    /// run of it goes. `advance` moves its next time past `now` (a run
    /// started by the scheduler; one started by hand leaves it).
    fn begin(&self, name: &str, now: DateTime<Utc>, advance: bool) -> Option<Schedule> {
        let mut items = self.lock();
        let e = items.get_mut(name)?;
        if e.running {
            return None;
        }
        e.running = true;
        if advance {
            e.next = e
                .stored
                .schedule
                .enabled
                .then(|| e.stored.schedule.next_after(now))
                .flatten();
        }
        Some(e.stored.schedule.clone())
    }

    /// A due run that did not start: recorded as skipped, due again at its
    /// next time.
    fn skip(&self, name: &str, now: DateTime<Utc>, reason: &str) {
        tracing::info!(schedule = name, "scheduled test run skipped: {reason}");
        self.record(name, |e| {
            e.running = false;
            e.next = e
                .stored
                .schedule
                .enabled
                .then(|| e.stored.schedule.next_after(now))
                .flatten();
            e.stored.status.skipped += 1;
            e.stored.status.push(Outcome {
                at: stamp(now),
                result: "skipped".into(),
                run_id: None,
                detail: Some(reason.to_string()),
            });
        });
    }

    /// A run ended: its outcome in the status, and the notice it calls for
    /// queued.
    fn finish(
        &self,
        name: &str,
        now: DateTime<Utc>,
        outcome: Result<&TestRun, String>,
        missing: &[String],
        base_url: Option<&str>,
    ) {
        let mut notice: Option<Pending> = None;
        self.record(name, |e| {
            e.running = false;
            let st = &mut e.stored.status;
            let s = &e.stored.schedule;
            st.runs += 1;
            let previous = st.last_passed;
            let (passed, run_id, mut detail) = match &outcome {
                Ok(run) => (run.report.passed, Some(run.id.clone()), None),
                Err(m) => (false, None, Some(m.clone())),
            };
            if !missing.is_empty() {
                let note = format!("not saved any more: {}", missing.join(", "));
                detail = Some(match detail {
                    Some(d) => format!("{d}; {note}"),
                    None => note,
                });
            }
            st.last_passed = Some(passed);
            if run_id.is_some() {
                st.last_run_id = run_id.clone();
            }
            st.push(Outcome {
                at: stamp(now),
                result: if passed { "passed" } else { "failed" }.into(),
                run_id: run_id.clone(),
                detail: detail.clone(),
            });
            let event = if !passed {
                Some("failed")
            } else if previous == Some(false) && s.notify_recovery {
                Some("recovered")
            } else {
                None
            };
            if let (Some(event), Some(url)) = (event, &s.webhook) {
                let key = format!("{}#{}", stamp(now), st.runs);
                st.last_notice = Some(NoticeStatus {
                    at: key.clone(),
                    event: event.into(),
                    run_id: run_id.clone(),
                    result: "pending".into(),
                    attempts: 0,
                    detail: None,
                });
                notice = Some(Pending {
                    schedule: name.to_string(),
                    key,
                    url: url.clone(),
                    body: notice_body(
                        s,
                        event,
                        outcome.as_ref().ok().copied(),
                        detail.as_deref(),
                        base_url,
                    ),
                    attempts: 0,
                    next: now,
                });
            }
        });
        if let Some(p) = notice {
            self.enqueue(p);
        }
    }

    fn enqueue(&self, p: Pending) {
        let mut q = self.queue();
        if q.len() >= MAX_NOTICES {
            drop(q);
            tracing::warn!(schedule = %p.schedule, "test schedule notice dropped: the queue is full");
            self.notice_state(&p, "dropped", Some("the notice queue is full".into()));
            return;
        }
        q.push_back(p);
    }

    /// The notices due at `now`, taken off the queue.
    fn take_due(&self, now: DateTime<Utc>) -> Vec<Pending> {
        let mut q = self.queue();
        let (due, wait): (VecDeque<Pending>, VecDeque<Pending>) =
            q.drain(..).partition(|p| p.next <= now);
        *q = wait;
        due.into()
    }

    /// Notices waiting (tests, the status).
    pub fn pending_notices(&self) -> usize {
        self.queue().len()
    }

    fn notice_state(&self, p: &Pending, result: &str, detail: Option<String>) {
        self.record(&p.schedule, |e| {
            if let Some(n) = e
                .stored
                .status
                .last_notice
                .as_mut()
                .filter(|n| n.at == p.key)
            {
                n.result = result.to_string();
                n.attempts = p.attempts;
                n.detail = detail;
            }
        });
    }
}

/// The JSON a webhook receives: the schedule, the counts, the failing
/// tests' names and the run (never what a test saw, never a variable).
fn notice_body(
    s: &Schedule,
    event: &str,
    run: Option<&TestRun>,
    detail: Option<&str>,
    base_url: Option<&str>,
) -> Value {
    let run_path = run.map(|r| format!("{}/{}", super::RUNS_PATH, r.id));
    let run_url = match (base_url, &run_path) {
        (Some(b), Some(p)) => Some(format!("{}{p}", b.trim_end_matches('/'))),
        _ => None,
    };
    let mut failing: Vec<&str> = run
        .map(|r| {
            r.report
                .results
                .iter()
                .filter(|t| !t.passed)
                .map(|t| t.name.as_str())
                .collect()
        })
        .unwrap_or_default();
    let more = failing.len().saturating_sub(MAX_FAILING);
    failing.truncate(MAX_FAILING);
    json!({
        "event": format!("test_schedule.{event}"),
        "schedule": s.name,
        "selection": s.selection_label(),
        "run_id": run.map(|r| r.id.clone()),
        "ran_at": run.map(|r| r.report.ran_at.clone()),
        "passed": run.is_some_and(|r| r.report.passed),
        "total": run.map(|r| r.report.total).unwrap_or(0),
        "failed": run.map(|r| r.report.failed).unwrap_or(0),
        "errors": run.map(|r| r.report.errors).unwrap_or(0),
        "failing": failing,
        "failing_more": more,
        "newly_failing": run.map(|r| r.change.newly_failing.clone()).unwrap_or_default(),
        "detail": detail,
        "run_path": run_path,
        "run_url": run_url,
    })
}

// ── Running ─────────────────────────────────────────────────────────

/// The harness sends load from this node (a run, or a cluster it leads).
fn load_running(state: &AppState) -> bool {
    #[cfg(test)]
    if state
        .saved_tests
        .schedules
        .force_load
        .load(Ordering::SeqCst)
    {
        return true;
    }
    state.harness.as_ref().is_some_and(|h| h.load_running())
}

/// The refusal when the harness sends load.
pub const LOAD_RUNNING: &str = "the harness is sending load from this instance";

/// What one run of a schedule did.
pub enum Ran {
    Kept(Box<TestRun>),
    Skipped(String),
    Failed(String),
}

/// Run a schedule now (the caller holds the run guard and has marked it
/// running): the tests it selects, `RUSTYBIN_TEST_*` for the variables.
async fn execute(state: &AppState, s: &Schedule) -> Ran {
    let sched = &state.saved_tests.schedules;
    let (names, missing, none) = s.resolve(&state.saved_tests.list());
    if none {
        let reason = format!("no saved test to run ({})", s.selection_label());
        sched.skip(&s.name, sched.now(), &reason);
        return Ran::Skipped(reason);
    }
    let body = RunBody {
        names,
        variables: BTreeMap::new(),
        project: None,
    };
    // A session of the schedule's own for the in-process requests.
    let session = format!("schedule-{}", s.name);
    let trigger = format!("{TRIGGER_PREFIX}{}", s.name);
    let base_url = state.settings.string(BASE_URL_KEY);
    match super::run_and_keep(state, &session, body, &trigger, PRINCIPAL).await {
        Ok(run) => {
            sched.finish(
                &s.name,
                sched.now(),
                Ok(&run),
                &missing,
                base_url.as_deref(),
            );
            Ran::Kept(Box::new(run))
        }
        Err((_, m)) => {
            sched.finish(
                &s.name,
                sched.now(),
                Err(m.clone()),
                &missing,
                base_url.as_deref(),
            );
            Ran::Failed(m)
        }
    }
}

/// Start the runs that are due (each in a task of its own). A run that
/// finds another run of the saved tests going waits for the next tick; one
/// due while the harness sends load is skipped.
pub fn tick(state: &AppState) -> Vec<tokio::task::JoinHandle<()>> {
    let sched = &state.saved_tests.schedules;
    let now = sched.now();
    let mut started = Vec::new();
    for name in sched.due(now) {
        if load_running(state) {
            sched.skip(&name, now, LOAD_RUNNING);
            continue;
        }
        let Some(guard) = state.saved_tests.begin_run() else {
            break;
        };
        let Some(schedule) = sched.begin(&name, now, true) else {
            continue;
        };
        let state = state.clone();
        started.push(tokio::spawn(async move {
            let _guard = guard;
            execute(&state, &schedule).await;
        }));
    }
    started
}

/// Run a schedule now (`POST /_rustybin/tests/schedules/{name}/run`, the
/// CLI, the tool): refused while another run goes or the harness sends
/// load; its next time stays.
pub async fn run_now(state: &AppState, name: &str) -> Result<Ran, (StatusCode, String)> {
    let sched = &state.saved_tests.schedules;
    if sched.get(name).is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            ScheduleError::NotFound(name.into()).message(),
        ));
    }
    if load_running(state) {
        return Err((
            StatusCode::CONFLICT,
            format!("{LOAD_RUNNING}: run it when the load test ends"),
        ));
    }
    let Some(_guard) = state.saved_tests.begin_run() else {
        return Err((StatusCode::CONFLICT, super::RUN_BUSY.into()));
    };
    let Some(schedule) = sched.begin(name, sched.now(), false) else {
        return Err((
            StatusCode::CONFLICT,
            format!("a run of the schedule {name} is going"),
        ));
    };
    Ok(execute(state, &schedule).await)
}

/// Start due runs every [`TICK`] until `shutdown` resolves.
pub async fn scheduler_loop<F>(state: AppState, shutdown: F)
where
    F: Future<Output = ()> + Send,
{
    tokio::pin!(shutdown);
    let mut every = tokio::time::interval(TICK);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = every.tick() => {
                let _ = tick(&state);
            }
        }
    }
}

/// Deliver due notices every [`TICK`] until `shutdown` resolves. Notices
/// still waiting at shutdown are lost (they are in memory only).
pub async fn notice_loop<F>(state: AppState, shutdown: F)
where
    F: Future<Output = ()> + Send,
{
    tokio::pin!(shutdown);
    let mut every = tokio::time::interval(TICK);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = every.tick() => {}
        }
        tokio::select! {
            _ = &mut shutdown => return,
            _ = deliver_due(&state) => {}
        }
    }
}

async fn post(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
    token: Option<&str>,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    let mut req = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(bytes);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    match req.send().await {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => Err(format!("the webhook answered {}", r.status().as_u16())),
        Err(e) if e.is_timeout() => Err("the webhook did not answer in time".into()),
        Err(_) => Err("the connection to the webhook failed".into()),
    }
}

/// Deliver the notices that are due: the number tried. A refusal of the
/// egress policy is final; a failed delivery is retried with a backoff
/// until [`NOTICE_ATTEMPTS`].
pub async fn deliver_due(state: &AppState) -> usize {
    let sched = &state.saved_tests.schedules;
    let due = sched.take_due(sched.now());
    let n = due.len();
    for mut p in due {
        let client = match state.egress.resolve(&p.url, Purpose::Notify).await {
            Ok(pinned) => state.egress.client(&pinned, NOTICE_TIMEOUT),
            Err(refusal) => {
                tracing::warn!(schedule = %p.schedule, "test schedule notice refused by the egress policy");
                sched.notice_state(&p, "refused", Some(refusal.0));
                continue;
            }
        };
        p.attempts += 1;
        let token = state.settings.secret(TOKEN_KEY);
        let sent = match client {
            Ok(c) => post(&c, &p.url, &p.body, token.as_deref().map(String::as_str)).await,
            Err(e) => Err(e),
        };
        match sent {
            Ok(()) => sched.notice_state(&p, "delivered", None),
            Err(e) if p.attempts >= NOTICE_ATTEMPTS => {
                tracing::warn!(schedule = %p.schedule, "test schedule notice not delivered after {} attempts", p.attempts);
                sched.notice_state(&p, "failed", Some(e));
            }
            Err(e) => {
                let wait = FIRST_BACKOFF_SECS * 4i64.pow(p.attempts - 1);
                p.next = sched.now() + chrono::Duration::seconds(wait);
                sched.notice_state(&p, "retrying", Some(e));
                sched.enqueue(p);
            }
        }
    }
    n
}

// ── Settings ────────────────────────────────────────────────────────

/// The settings of the failure notices (group `notify`).
pub fn specs() -> Vec<Spec> {
    vec![
        Spec::new(
            TOKEN_KEY,
            "notify",
            "Webhook bearer token",
            "Sent as Authorization: Bearer with every notice of a failing (or recovered) scheduled test run.",
            Kind::Text { max_len: 4096 },
            Value::Null,
        )
        .secret()
        .env("RUSTYBIN_NOTIFY_WEBHOOK_TOKEN", |c| {
            c.notify_webhook_token.clone().map(Value::String)
        }),
        Spec::new(
            BASE_URL_KEY,
            "notify",
            "Link base URL",
            "The address people open this instance at (https://mocks.example.com): a notice's run_url is it plus /_rustybin/tests/runs/<id>. Empty: notices carry the run_path only.",
            Kind::Url,
            Value::Null,
        )
        .env("RUSTYBIN_NOTIFY_BASE_URL", |c| {
            c.notify_base_url.clone().map(Value::String)
        }),
    ]
}

// ── Routes ──────────────────────────────────────────────────────────

pub const LIST_PATH: &str = "/_rustybin/tests/schedules";
pub const ONE_PATH: &str = "/_rustybin/tests/schedules/{name}";
pub const RUN_PATH: &str = "/_rustybin/tests/schedules/{name}/run";

fn refuse(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

/// A full schedule for `PUT` (the name comes from the path; `saved_at`,
/// `selection` and `status` of a read are ignored).
pub fn schedule_from_value(name: &str, mut v: Value) -> Result<Schedule, String> {
    let Value::Object(m) = &mut v else {
        return Err("a schedule object".into());
    };
    match m.get("name") {
        Some(Value::String(n)) if n != name => {
            return Err(format!("the body names {n:?}, the path {name:?}"))
        }
        _ => {}
    }
    m.insert("name".into(), json!(name));
    for k in ["saved_at", "selection", "status"] {
        m.remove(k);
    }
    serde_json::from_value(v).map_err(|e| e.to_string())
}

/// The answer of a run started by hand.
pub fn ran_view(state: &AppState, name: &str, ran: &Ran) -> Value {
    let (result, detail, run) = match ran {
        Ran::Kept(run) => (
            if run.report.passed {
                "passed"
            } else {
                "failed"
            },
            None,
            Some(super::run_view(run)),
        ),
        Ran::Skipped(r) => ("skipped", Some(r.clone()), None),
        Ran::Failed(m) => ("failed", Some(m.clone()), None),
    };
    json!({
        "result": result,
        "detail": detail,
        "run": run,
        "schedule": state.saved_tests.schedules.view_of(name),
    })
}

pub(super) async fn list_handler(State(state): State<AppState>) -> Response {
    Json(json!({
        "schedules": state.saved_tests.schedules.views(),
        "max_schedules": MAX_SCHEDULES,
        "pending_notices": state.saved_tests.schedules.pending_notices(),
    }))
    .into_response()
}

fn audit(headers: &HeaderMap, state: &AppState, what: &str, name: &str) {
    tracing::info!(
        principal = %crate::settings::principal(headers, &state.config),
        schedule = name,
        "test schedule {what}"
    );
}

pub(super) async fn create_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let schedule: Schedule = match serde_json::from_slice(&body) {
        Ok(s) => s,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, format!("invalid body: {e}")),
    };
    let name = schedule.name.clone();
    match state.saved_tests.schedules.put(schedule, false) {
        Ok(v) => {
            audit(&headers, &state, "created", &name);
            (StatusCode::CREATED, Json(v)).into_response()
        }
        Err(e) => refuse(e.status(), e.message()),
    }
}

pub(super) async fn get_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Response {
    match state.saved_tests.schedules.view_of(&name) {
        Some(v) => Json(v).into_response(),
        None => refuse(
            StatusCode::NOT_FOUND,
            ScheduleError::NotFound(name).message(),
        ),
    }
}

pub(super) async fn put_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, format!("invalid body: {e}")),
    };
    let schedule = match schedule_from_value(&name, v) {
        Ok(s) => s,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, e),
    };
    match state.saved_tests.schedules.put(schedule, true) {
        Ok(v) => {
            audit(&headers, &state, "saved", &name);
            Json(v).into_response()
        }
        Err(e) => refuse(e.status(), e.message()),
    }
}

pub(super) async fn delete_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    match state.saved_tests.schedules.delete(&name) {
        Ok(true) => {
            audit(&headers, &state, "deleted", &name);
            Json(json!({ "deleted": name })).into_response()
        }
        Ok(false) => refuse(
            StatusCode::NOT_FOUND,
            ScheduleError::NotFound(name).message(),
        ),
        Err(e) => refuse(e.status(), e.message()),
    }
}

pub(super) async fn run_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Response {
    match run_now(&state, &name).await {
        Ok(ran) => Json(ran_view(&state, &name, &ran)).into_response(),
        Err((s, m)) => refuse(s, m),
    }
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            LIST_PATH,
            &["GET", "POST"],
            category::CONTROL,
            "Saved tests on a schedule: list them with their status, or create one (admin)",
        )
.permission_for(&["GET"], crate::rbac::Permission::TestsRead).permission_for(&["POST"], crate::rbac::Permission::TestsWrite)
        .example(Example::get("Test schedules", LIST_PATH)),
        Endpoint::new(
            ONE_PATH,
            &["GET", "PUT", "DELETE"],
            category::CONTROL,
            "A test schedule: read it with its status, save or replace it, delete it (admin)",
        )
.permission_for(&["GET"], crate::rbac::Permission::TestsRead).permission_for(&["PUT", "DELETE"], crate::rbac::Permission::TestsWrite)
        .example(Example::get("A test schedule", "/_rustybin/tests/schedules/nightly").expect_status(404)),
        Endpoint::new(
            RUN_PATH,
            &["POST"],
            category::CONTROL,
            "Run a test schedule now: its tests, kept in the history, its notice when it fails (admin)",
        )
.permission(crate::rbac::Permission::TestsRun)
        .example(
            Example::post("Run a schedule now", "/_rustybin/tests/schedules/nightly/run")
                .expect_status(404),
        ),
    ]
}

pub fn openapi_paths() -> Value {
    let obj = json!({ "application/json": { "schema": { "type": "object" } } });
    let name =
        json!({ "name": "name", "in": "path", "required": true, "schema": { "type": "string" } });
    json!({
        LIST_PATH: {
            "get": {
                "tags": ["Control Plane"], "summary": "Test schedules (admin)",
                "description": "Every schedule with its status: running, next_due, last_result (passed, failed or skipped), last_at, last_detail, last_run_id, last_passed, runs, skipped, last_notice (event, result: pending, retrying, delivered, failed, refused or dropped) and the recent outcomes.",
                "operationId": "listTestSchedules",
                "responses": { "200": { "description": "schedules, max_schedules, pending_notices", "content": obj } }
            },
            "post": {
                "tags": ["Control Plane"], "summary": "Create a test schedule (admin)",
                "description": "name, the tests (names, or prefix, or neither for every saved test), every (5m to 24h, aligned to multiples since the Unix epoch) or at (HH:MM, UTC), enabled, webhook (posted under the egress purpose notify, which has no default) and notify_recovery. At most 20 schedules. Variables come from RUSTYBIN_TEST_* only.",
                "operationId": "createTestSchedule",
                "requestBody": { "required": true, "content": { "application/json": { "schema": schema()["items"].clone() } } },
                "responses": { "201": { "description": "The schedule with its status", "content": obj }, "400": { "description": "Invalid schedule" }, "409": { "description": "A schedule of that name exists" }, "507": { "description": "20 schedules already" } }
            }
        },
        ONE_PATH: {
            "get": {
                "tags": ["Control Plane"], "summary": "A test schedule (admin)", "operationId": "getTestSchedule",
                "parameters": [name],
                "responses": { "200": { "description": "The schedule with its status", "content": obj }, "404": { "description": "No such schedule" } }
            },
            "put": {
                "tags": ["Control Plane"], "summary": "Save or replace a test schedule (admin)", "operationId": "putTestSchedule",
                "description": "The whole schedule (the name comes from the path); a replaced schedule keeps its status.",
                "parameters": [name],
                "requestBody": { "required": true, "content": obj },
                "responses": { "200": { "description": "The schedule with its status", "content": obj }, "400": { "description": "Invalid schedule" }, "507": { "description": "20 schedules already" } }
            },
            "delete": {
                "tags": ["Control Plane"], "summary": "Delete a test schedule (admin)", "operationId": "deleteTestSchedule",
                "parameters": [name],
                "responses": { "200": { "description": "Deleted", "content": obj }, "404": { "description": "No such schedule" } }
            }
        },
        RUN_PATH: { "post": {
            "tags": ["Control Plane"], "summary": "Run a test schedule now (admin)",
            "description": "Runs the schedule's tests as the scheduler would (trigger schedule:<name>, kept in the history, the notice queued when it fails); its next time stays. 409 while another run of the saved tests goes or the harness sends load.",
            "operationId": "runTestSchedule",
            "parameters": [name],
            "responses": {
                "200": { "description": "result (passed, failed or skipped), detail, run (the kept run, as POST /_rustybin/tests/run answers it), schedule (with its status)", "content": obj },
                "404": { "description": "No such schedule" },
                "409": { "description": "Another run goes, or the harness sends load" }
            }
        } }
    })
}

/// `test_schedules:` of `rustybin.yaml`.
pub fn schema() -> Value {
    let name = json!({ "type": "string", "maxLength": MAX_NAME, "pattern": "^[A-Za-z0-9._-]+$" });
    json!({
        "type": "array", "maxItems": MAX_SCHEDULES,
        "description": "Saved tests on a schedule: every (5m to 24h) or at (HH:MM UTC), the tests (names, a prefix, or every saved test) and a webhook for failures",
        "items": {
            "type": "object", "additionalProperties": false, "required": ["name"],
            "properties": {
                "name": name,
                "description": { "type": "string", "maxLength": MAX_DESCRIPTION },
                "names": { "type": "array", "items": name, "maxItems": super::MAX_TESTS },
                "prefix": name,
                "every": { "type": "string", "pattern": "^[0-9]+[smh]$", "description": "5m to 24h" },
                "at": { "type": "string", "pattern": "^[0-2][0-9]:[0-5][0-9]$", "description": "HH:MM, UTC" },
                "enabled": { "type": "boolean", "default": true },
                "webhook": { "type": "string", "format": "uri", "description": "Posted a JSON summary when a run fails (egress purpose notify)" },
                "notify_recovery": { "type": "boolean", "default": false }
            }
        }
    })
}
