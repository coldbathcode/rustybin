//! Self-service visits (`docs/plans/hosted-mocks.md`): a visitor without the
//! admin token owns the mocks they create through a manage token
//! (`X-Rustybin-Mock-Token`, stored as its SHA-256 only), and a visit lasts
//! a time allowance: the first period, free extensions up to the anonymous
//! hours, then an email address for up to the daily hours (per UTC day).
//!
//! Everything is bounded: visits (twice the visitor mock cap), the daily
//! ledger (email hashes and anonymous visits per session key) and the
//! creation rate windows. Time comes from the injectable [`Clock`].

use chrono::{DateTime, Duration as TimeDelta, NaiveDate, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use super::leads::{Lead, Leads};
use crate::config::SelfServiceConfig;
use crate::limits::Clock;

/// Header carrying manage tokens (may repeat).
pub const TOKEN_HEADER: &str = "x-rustybin-mock-token";
/// Prefix of every manage token.
pub const TOKEN_PREFIX: &str = "rbm_";
/// Manage tokens read from one request.
pub const MAX_PRESENTED_TOKENS: usize = 20;
/// Extend is offered from this long before the visit ends.
pub const EXTEND_WINDOW_MINUTES: i64 = 15;
/// Where the come-back answers point for an instance without limits.
pub const SELF_HOST_URL: &str = "https://docs.rustybin.dev/quick-start.html";
/// Entries in each daily ledger map.
const MAX_LEDGER: usize = 100_000;
/// Instance-wide creations per minute (a scripted flood).
const CREATES_PER_MINUTE: usize = 30;

/// Who may change a mock.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "lowercase")]
pub enum Owner {
    /// Admin, file and sample mocks: changed with admin rights only.
    #[default]
    Instance,
    /// A visit, by the SHA-256 (hex) of its manage token.
    Visit(String),
    /// An API key in shared mode, by key id.
    Key(String),
}

impl Owner {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Instance => "instance",
            Self::Visit(_) => "visit",
            Self::Key(_) => "key",
        }
    }

    /// Visitor content (a visit or a key): restricted responses, own caps.
    pub fn is_visitor(&self) -> bool {
        !matches!(self, Self::Instance)
    }

    /// A short, non-secret label for operators (`v:1a2b3c4d`, `k:<id>`).
    pub fn fingerprint(&self) -> String {
        match self {
            Self::Instance => "instance".into(),
            Self::Visit(h) => format!("v:{}", &h[..h.len().min(8)]),
            Self::Key(id) => format!("k:{id}"),
        }
    }
}

/// Lowercase hex SHA-256.
pub fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

fn base32(bytes: &[u8], chars: usize) -> String {
    let mut out = String::with_capacity(chars);
    let mut acc: u64 = 0;
    let mut bits = 0;
    for &b in bytes {
        acc = (acc << 8) | u64::from(b);
        bits += 8;
        while bits >= 5 && out.len() < chars {
            bits -= 5;
            out.push(BASE32[((acc >> bits) & 31) as usize] as char);
        }
    }
    out
}

/// A new manage token: `rbm_` and 160 random bits in base32.
pub fn new_token() -> String {
    let mut bytes = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("{TOKEN_PREFIX}{}", base32(&bytes, 32))
}

/// Six random base32 characters (30 bits) for a visitor mock name.
pub fn name_suffix() -> String {
    let mut bytes = [0u8; 4];
    rand::thread_rng().fill_bytes(&mut bytes);
    base32(&bytes, 6)
}

/// Does `name` match one of the blocked globs (`*` matches any run)?
pub fn blocked(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| glob(p.as_bytes(), name.as_bytes()))
}

fn glob(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| glob(rest, &s[i..])),
        Some((c, rest)) => s.first() == Some(c) && glob(rest, &s[1..]),
    }
}

/// An email address as given on the form, checked and normalised (trimmed,
/// domain lowercased).
pub fn normalise_email(raw: &str) -> Result<String, String> {
    let e = raw.trim();
    let bad = || Err("enter a valid email address".to_string());
    if e.is_empty() || e.len() > 254 || e.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return bad();
    }
    let Some((local, domain)) = e.split_once('@') else {
        return bad();
    };
    if local.is_empty()
        || local.len() > 64
        || domain.contains('@')
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.contains("..")
    {
        return bad();
    }
    Ok(format!("{local}@{}", domain.to_ascii_lowercase()))
}

/// The email step of a request (`{"email", "marketing_consent"}`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailInput {
    pub email: Option<String>,
    #[serde(default)]
    pub marketing_consent: bool,
}

#[derive(Debug, PartialEq)]
pub enum VisitError {
    /// Unknown or ended visit.
    NotFound,
    /// The next period needs an email address (capture is on).
    EmailRequired,
    /// The free time is used and email capture is off.
    TimeUsed {
        resets_at: DateTime<Utc>,
    },
    /// This email's daily allowance is used.
    DailyLimit {
        resets_at: DateTime<Utc>,
    },
    /// Extend opens later.
    TooEarly {
        at: DateTime<Utc>,
    },
    /// The instance holds as many visits or visitor mocks as it may.
    Busy {
        retry_after: u64,
    },
    /// Creations too fast (per session key, or the whole instance).
    RateLimited {
        retry_after: u64,
    },
    InvalidEmail(String),
    /// An operator paused self-service.
    Paused,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Visit {
    session: String,
    started_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    /// Minutes granted to this visit so far.
    granted: u64,
    /// Salted hash of the email address given, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Inner {
    /// By manage token hash.
    visits: HashMap<String, Visit>,
    day: Option<NaiveDate>,
    /// Minutes granted today, by email hash.
    email_minutes: HashMap<String, u64>,
    /// Anonymous visits started today, by session key.
    anon_visits: HashMap<String, u32>,
    #[serde(skip)]
    creations: HashMap<String, VecDeque<DateTime<Utc>>>,
    #[serde(skip)]
    instance_creations: VecDeque<DateTime<Utc>>,
}

/// What a visit looks like to its owner (`GET /_rustybin/mocks/visit`).
#[derive(Clone, Debug, Serialize)]
pub struct VisitView {
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub expires_in_secs: i64,
    pub extendable: bool,
    pub extendable_at: DateTime<Utc>,
    /// What the next Extend needs: `free`, `email`, `allowance` (the
    /// email's daily hours) or `none` (come back tomorrow).
    pub next_extension: &'static str,
    pub minutes_used: u64,
    pub minutes_allowed: u64,
    pub email_given: bool,
    pub resets_at: DateTime<Utc>,
}

/// The visit registry (in [`super::store::MockStore`]).
pub struct Visits {
    cfg: SelfServiceConfig,
    clock: Arc<dyn Clock>,
    salt: String,
    inner: Mutex<Inner>,
    paused: AtomicBool,
    leads: Arc<Leads>,
    persist: Option<PathBuf>,
}

/// File (in `RUSTYBIN_MOCKS_STATE_DIR`) holding visits and the day's ledger.
pub const VISITS_FILE: &str = "_visits.json";

impl Visits {
    pub fn new(cfg: SelfServiceConfig, clock: Arc<dyn Clock>, leads: Arc<Leads>) -> Self {
        let salt = cfg.leads_salt.clone().unwrap_or_else(|| {
            let mut bytes = [0u8; 16];
            rand::thread_rng().fill_bytes(&mut bytes);
            base32(&bytes, 26)
        });
        Self {
            cfg,
            clock,
            salt,
            inner: Mutex::new(Inner::default()),
            paused: AtomicBool::new(false),
            leads,
            persist: None,
        }
    }

    /// Save visits and the ledger under `dir` on every change, loading what
    /// is there first.
    pub fn with_persistence(mut self, dir: PathBuf) -> Self {
        let path = dir.join(VISITS_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Inner>(&bytes) {
                Ok(saved) => {
                    let now = self.clock.wall();
                    let mut inner = saved;
                    inner.visits.retain(|_, v| v.expires_at > now);
                    if inner.day != Some(now.date_naive()) {
                        inner.email_minutes.clear();
                        inner.anon_visits.clear();
                        inner.day = Some(now.date_naive());
                    }
                    self.inner = Mutex::new(inner);
                }
                Err(e) => tracing::warn!("saved visits not loaded from {}: {e}", path.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("saved visits not loaded from {}: {e}", path.display()),
        }
        self.persist = Some(path);
        self
    }

    pub fn config(&self) -> &SelfServiceConfig {
        &self.cfg
    }

    pub fn leads(&self) -> &Arc<Leads> {
        &self.leads
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.wall()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn step(&self) -> u64 {
        self.cfg.visit_minutes
    }

    fn anon_minutes(&self) -> u64 {
        self.cfg.anon_hours * 60
    }

    fn daily_minutes(&self) -> u64 {
        self.cfg.daily_hours * 60
    }

    fn email_hash(&self, email: &str) -> String {
        sha256_hex(&format!("{}:{}", self.salt, email.to_lowercase()))
    }

    fn next_midnight(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        let tomorrow = now.date_naive().succ_opt().unwrap_or(now.date_naive());
        tomorrow
            .and_hms_opt(0, 0, 0)
            .map(|t| t.and_utc())
            .unwrap_or(now)
    }

    /// Start a new UTC day: the ledger empties.
    fn roll(&self, inner: &mut Inner, now: DateTime<Utc>) {
        let today = now.date_naive();
        if inner.day != Some(today) {
            inner.day = Some(today);
            inner.email_minutes.clear();
            inner.anon_visits.clear();
        }
    }

    fn save(&self, inner: &Inner) {
        let Some(path) = &self.persist else {
            return;
        };
        let text = match serde_json::to_vec(inner) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("visits not saved: {e}");
                return;
            }
        };
        let tmp = path.with_extension("json.tmp");
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&tmp, &text))
            .and_then(|_| std::fs::rename(&tmp, path));
        if let Err(e) = result {
            tracing::warn!("visits not saved to {}: {e}", path.display());
        }
    }

    pub fn pause(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Is the visit with this token hash running?
    pub fn alive(&self, hash: &str) -> bool {
        let now = self.clock.wall();
        self.lock()
            .visits
            .get(hash)
            .is_some_and(|v| v.expires_at > now)
    }

    /// The running visits among the presented tokens, as token hashes.
    pub fn resolve<'a>(&self, tokens: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let now = self.clock.wall();
        let inner = self.lock();
        let mut out = Vec::new();
        for t in tokens.into_iter().take(MAX_PRESENTED_TOKENS) {
            let h = sha256_hex(t.trim());
            if inner.visits.get(&h).is_some_and(|v| v.expires_at > now) && !out.contains(&h) {
                out.push(h);
            }
        }
        out
    }

    /// Drop ended visits; returns their token hashes.
    pub fn sweep(&self) -> Vec<String> {
        let now = self.clock.wall();
        let mut inner = self.lock();
        let ended: Vec<String> = inner
            .visits
            .iter()
            .filter(|(_, v)| v.expires_at <= now)
            .map(|(h, _)| h.clone())
            .collect();
        if !ended.is_empty() {
            for h in &ended {
                inner.visits.remove(h);
            }
            self.save(&inner);
        }
        ended
    }

    pub fn running(&self) -> usize {
        let now = self.clock.wall();
        self.lock()
            .visits
            .values()
            .filter(|v| v.expires_at > now)
            .count()
    }

    fn max_visits(&self) -> usize {
        self.cfg.max_mocks.saturating_mul(2).max(10)
    }

    fn retry_after(&self, inner: &Inner, now: DateTime<Utc>) -> u64 {
        inner
            .visits
            .values()
            .map(|v| (v.expires_at - now).num_seconds())
            .filter(|s| *s > 0)
            .min()
            .unwrap_or(60)
            .max(60) as u64
    }

    fn email_step(
        &self,
        inner: &mut Inner,
        input: &EmailInput,
        now: DateTime<Utc>,
        extra: u64,
    ) -> Result<Option<(String, String)>, VisitError> {
        let Some(raw) = input.email.as_deref().filter(|e| !e.trim().is_empty()) else {
            return Ok(None);
        };
        if !self.cfg.leads_enabled() {
            return Ok(None);
        }
        let email = normalise_email(raw).map_err(VisitError::InvalidEmail)?;
        let hash = self.email_hash(&email);
        let used = inner.email_minutes.get(&hash).copied().unwrap_or(0);
        if used + extra > self.daily_minutes() {
            return Err(VisitError::DailyLimit {
                resets_at: self.next_midnight(now),
            });
        }
        if !inner.email_minutes.contains_key(&hash) && inner.email_minutes.len() >= MAX_LEDGER {
            return Err(VisitError::Busy { retry_after: 60 });
        }
        Ok(Some((email, hash)))
    }

    fn lead(
        &self,
        email: String,
        input: &EmailInput,
        source: &'static str,
        minutes: u64,
        mocks: &[String],
    ) {
        self.leads.submit(Lead {
            email,
            marketing_consent: input.marketing_consent
                && self.cfg.leads_consent == crate::config::LeadsConsent::Checkbox,
            consent_mode: self.cfg.leads_consent.as_str(),
            source,
            captured_at: self.clock.wall(),
            visit_minutes: minutes,
            mocks: mocks.to_vec(),
        });
    }

    /// Start a visit for `session`: returns the manage token (shown once)
    /// and the token hash.
    pub fn start(&self, session: &str, input: &EmailInput) -> Result<(String, String), VisitError> {
        if self.paused() {
            return Err(VisitError::Paused);
        }
        let now = self.clock.wall();
        let mut inner = self.lock();
        self.roll(&mut inner, now);
        inner.visits.retain(|_, v| v.expires_at > now);
        if inner.visits.len() >= self.max_visits() {
            return Err(VisitError::Busy {
                retry_after: self.retry_after(&inner, now),
            });
        }
        let step = self.step();
        let email = self.email_step(&mut inner, input, now, step)?;
        if email.is_none() {
            let used = inner.anon_visits.get(session).copied().unwrap_or(0);
            if used >= self.cfg.anon_visits_per_day {
                return Err(if self.cfg.leads_enabled() {
                    VisitError::EmailRequired
                } else {
                    VisitError::TimeUsed {
                        resets_at: self.next_midnight(now),
                    }
                });
            }
            if !inner.anon_visits.contains_key(session) && inner.anon_visits.len() >= MAX_LEDGER {
                return Err(VisitError::Busy { retry_after: 60 });
            }
            *inner.anon_visits.entry(session.to_string()).or_insert(0) += 1;
        }
        let token = new_token();
        let hash = sha256_hex(&token);
        let email_hash = email.as_ref().map(|(_, h)| h.clone());
        if let Some(h) = &email_hash {
            *inner.email_minutes.entry(h.clone()).or_insert(0) += step;
        }
        inner.visits.insert(
            hash.clone(),
            Visit {
                session: session.to_string(),
                started_at: now,
                expires_at: now + TimeDelta::minutes(step as i64),
                granted: step,
                email: email_hash,
            },
        );
        self.save(&inner);
        drop(inner);
        if let Some((address, _)) = email {
            self.lead(address, input, "mock_start", step, &[]);
        }
        Ok((token, hash))
    }

    /// One more period for the visit `hash` (section 7 of the plan).
    pub fn extend(
        &self,
        hash: &str,
        input: &EmailInput,
        mocks: &[String],
    ) -> Result<VisitView, VisitError> {
        let now = self.clock.wall();
        let mut inner = self.lock();
        self.roll(&mut inner, now);
        let step = self.step();
        let Some(v) = inner
            .visits
            .get(hash)
            .filter(|v| v.expires_at > now)
            .cloned()
        else {
            return Err(VisitError::NotFound);
        };
        let opens = v.expires_at - TimeDelta::minutes(EXTEND_WINDOW_MINUTES);
        if now < opens {
            return Err(VisitError::TooEarly { at: opens });
        }
        let mut captured = None;
        let email_hash = match &v.email {
            Some(h) => h.clone(),
            None if v.granted + step <= self.anon_minutes() => {
                self.grant(&mut inner, hash, now, step);
                self.save(&inner);
                return self
                    .view_locked(&inner, hash, now)
                    .ok_or(VisitError::NotFound);
            }
            None => {
                let Some((address, h)) =
                    self.email_step(&mut inner, input, now, v.granted + step)?
                else {
                    return Err(if self.cfg.leads_enabled() {
                        VisitError::EmailRequired
                    } else {
                        VisitError::TimeUsed {
                            resets_at: self.next_midnight(now),
                        }
                    });
                };
                // The anonymous time counts towards the day.
                *inner.email_minutes.entry(h.clone()).or_insert(0) += v.granted;
                if let Some(visit) = inner.visits.get_mut(hash) {
                    visit.email = Some(h.clone());
                }
                captured = Some(address);
                h
            }
        };
        let used = inner.email_minutes.get(&email_hash).copied().unwrap_or(0);
        if used + step > self.daily_minutes() {
            self.save(&inner);
            return Err(VisitError::DailyLimit {
                resets_at: self.next_midnight(now),
            });
        }
        *inner.email_minutes.entry(email_hash).or_insert(0) += step;
        self.grant(&mut inner, hash, now, step);
        self.save(&inner);
        let view = self.view_locked(&inner, hash, now);
        drop(inner);
        if let Some(address) = captured {
            self.lead(address, input, "mock_extend", v.granted + step, mocks);
        }
        view.ok_or(VisitError::NotFound)
    }

    fn grant(&self, inner: &mut Inner, hash: &str, now: DateTime<Utc>, step: u64) {
        if let Some(v) = inner.visits.get_mut(hash) {
            v.expires_at = v.expires_at.max(now) + TimeDelta::minutes(step as i64);
            v.granted += step;
        }
    }

    pub fn view(&self, hash: &str) -> Option<VisitView> {
        let now = self.clock.wall();
        let mut inner = self.lock();
        self.roll(&mut inner, now);
        self.view_locked(&inner, hash, now)
    }

    fn view_locked(&self, inner: &Inner, hash: &str, now: DateTime<Utc>) -> Option<VisitView> {
        let v = inner.visits.get(hash).filter(|v| v.expires_at > now)?;
        let step = self.step();
        let opens = v.expires_at - TimeDelta::minutes(EXTEND_WINDOW_MINUTES);
        let (used, allowed, next) = match &v.email {
            Some(h) => {
                let used = inner.email_minutes.get(h).copied().unwrap_or(0);
                let next = if used + step <= self.daily_minutes() {
                    "allowance"
                } else {
                    "none"
                };
                (used, self.daily_minutes(), next)
            }
            None => {
                let next = if v.granted + step <= self.anon_minutes() {
                    "free"
                } else if self.cfg.leads_enabled() {
                    "email"
                } else {
                    "none"
                };
                (v.granted, self.anon_minutes(), next)
            }
        };
        Some(VisitView {
            started_at: v.started_at,
            expires_at: v.expires_at,
            expires_in_secs: (v.expires_at - now).num_seconds().max(0),
            extendable: now >= opens && next != "none",
            extendable_at: opens,
            next_extension: next,
            minutes_used: used,
            minutes_allowed: allowed,
            email_given: v.email.is_some(),
            resets_at: self.next_midnight(now),
        })
    }

    /// Replace the visit's token; returns the new token and its hash.
    pub fn rotate(&self, hash: &str) -> Option<(String, String)> {
        let now = self.clock.wall();
        let mut inner = self.lock();
        let v = inner.visits.remove(hash).filter(|v| v.expires_at > now)?;
        let token = new_token();
        let new_hash = sha256_hex(&token);
        inner.visits.insert(new_hash.clone(), v);
        self.save(&inner);
        Some((token, new_hash))
    }

    /// Undo [`Self::start`] when the creation it was for failed: the visit
    /// goes and the anonymous visit it used is given back.
    pub fn abandon(&self, hash: &str) {
        let mut inner = self.lock();
        if let Some(v) = inner.visits.remove(hash) {
            if v.email.is_none() {
                if let Some(n) = inner.anon_visits.get_mut(&v.session) {
                    *n = n.saturating_sub(1);
                }
            }
            self.save(&inner);
        }
    }

    /// End a visit now (an operator, or the visitor).
    pub fn end(&self, hash: &str) -> bool {
        let mut inner = self.lock();
        let removed = inner.visits.remove(hash).is_some();
        if removed {
            self.save(&inner);
        }
        removed
    }

    /// End every visit; returns their hashes.
    pub fn end_all(&self) -> Vec<String> {
        let mut inner = self.lock();
        let all: Vec<String> = inner.visits.drain().map(|(h, _)| h).collect();
        self.save(&inner);
        all
    }

    /// Count a creation for `session`; refuses when too fast.
    pub fn note_creation(&self, session: &str) -> Result<(), VisitError> {
        let now = self.clock.wall();
        let hour_ago = now - TimeDelta::hours(1);
        let minute_ago = now - TimeDelta::minutes(1);
        let mut inner = self.lock();
        while inner
            .instance_creations
            .front()
            .is_some_and(|t| *t <= minute_ago)
        {
            inner.instance_creations.pop_front();
        }
        if inner.instance_creations.len() >= CREATES_PER_MINUTE {
            let oldest = inner.instance_creations.front().copied().unwrap_or(now);
            return Err(VisitError::RateLimited {
                retry_after: (oldest - minute_ago).num_seconds().max(1) as u64,
            });
        }
        if !inner.creations.contains_key(session) && inner.creations.len() >= MAX_LEDGER {
            inner
                .creations
                .retain(|_, q| q.back().is_some_and(|t| *t > hour_ago));
            if inner.creations.len() >= MAX_LEDGER {
                return Err(VisitError::Busy { retry_after: 60 });
            }
        }
        let limit = self.cfg.create_per_hour as usize;
        let q = inner.creations.entry(session.to_string()).or_default();
        while q.front().is_some_and(|t| *t <= hour_ago) {
            q.pop_front();
        }
        if q.len() >= limit {
            let oldest = q.front().copied().unwrap_or(now);
            return Err(VisitError::RateLimited {
                retry_after: (oldest - hour_ago).num_seconds().max(1) as u64,
            });
        }
        q.push_back(now);
        inner.instance_creations.push_back(now);
        Ok(())
    }

    /// Every running visit (operators): fingerprint, times, email given.
    pub fn list(&self) -> Vec<Value> {
        let now = self.clock.wall();
        let inner = self.lock();
        let mut rows: Vec<Value> = inner
            .visits
            .iter()
            .filter(|(_, v)| v.expires_at > now)
            .map(|(h, v)| {
                json!({
                    "id": &h[..h.len().min(16)],
                    "owner": Owner::Visit(h.clone()).fingerprint(),
                    "started_at": v.started_at,
                    "expires_at": v.expires_at,
                    "minutes_granted": v.granted,
                    "email_given": v.email.is_some(),
                    "session": session_label(&v.session),
                })
            })
            .collect();
        rows.sort_by(|a, b| a["started_at"].as_str().cmp(&b["started_at"].as_str()));
        rows
    }

    /// The full token hash of a visit listed by its id prefix.
    pub fn hash_for_id(&self, id: &str) -> Option<String> {
        if id.len() < 8 {
            return None;
        }
        let inner = self.lock();
        let mut found = inner.visits.keys().filter(|h| h.starts_with(id));
        let first = found.next()?.clone();
        found.next().is_none().then_some(first)
    }
}

/// A session key for operators, without the full client address.
pub fn session_label(session: &str) -> String {
    format!("s:{}", &sha256_hex(session)[..8])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::ManualClock;
    use std::time::Duration;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("time")
            .with_timezone(&Utc)
    }

    fn visits(leads: bool) -> (Visits, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new(at("2026-10-18T09:00:00Z")));
        let mut cfg = SelfServiceConfig {
            enabled: true,
            leads_salt: Some("salt".into()),
            ..SelfServiceConfig::default()
        };
        if leads {
            cfg.leads_file = Some(PathBuf::from("/dev/null"));
            cfg.privacy_url = Some("https://example.com/privacy".into());
        }
        let leads = Arc::new(Leads::disabled());
        (Visits::new(cfg, clock.clone(), leads), clock)
    }

    fn minutes(m: u64) -> Duration {
        Duration::from_secs(m * 60)
    }

    fn email(e: &str) -> EmailInput {
        EmailInput {
            email: Some(e.into()),
            marketing_consent: true,
        }
    }

    #[test]
    fn tokens_names_globs_and_emails() {
        let t = new_token();
        assert!(t.starts_with(TOKEN_PREFIX));
        assert_eq!(t.len(), TOKEN_PREFIX.len() + 32);
        assert_ne!(t, new_token());
        let s = name_suffix();
        assert_eq!(s.len(), 6);
        assert!(s.bytes().all(|b| BASE32.contains(&b)));
        let blocks = vec!["*paypal*".to_string(), "login".to_string()];
        assert!(blocked("my-paypal-api", &blocks));
        assert!(blocked("login", &blocks));
        assert!(!blocked("login-api", &blocks));
        assert_eq!(
            normalise_email("  Ada@Example.COM ").as_deref(),
            Ok("Ada@example.com")
        );
        for bad in [
            "",
            "ada",
            "ada@",
            "@example.com",
            "ada@example",
            "a b@c.d",
            "a@b@c.d",
            "a@b..c",
        ] {
            assert!(normalise_email(bad).is_err(), "{bad}");
        }
        assert_eq!(
            Owner::Visit("0123456789abcdef".into()).fingerprint(),
            "v:01234567"
        );
    }

    #[test]
    fn one_hour_then_extend_to_three_then_email_then_six_a_day() {
        let (v, clock) = visits(true);
        let (token, hash) = v.start("s1", &EmailInput::default()).expect("start");
        assert_eq!(v.resolve([token.as_str()]), vec![hash.clone()]);
        let view = v.view(&hash).expect("view");
        assert_eq!(view.expires_in_secs, 3600);
        assert!(!view.extendable);
        assert_eq!(view.next_extension, "free");
        // Too early: Extend opens 15 minutes before the end.
        assert!(matches!(
            v.extend(&hash, &EmailInput::default(), &[]),
            Err(VisitError::TooEarly { .. })
        ));
        clock.advance(minutes(50));
        let view = v
            .extend(&hash, &EmailInput::default(), &[])
            .expect("2nd hour");
        assert_eq!(view.minutes_used, 120);
        assert_eq!(view.expires_in_secs, 70 * 60);
        clock.advance(minutes(60));
        let view = v
            .extend(&hash, &EmailInput::default(), &[])
            .expect("3rd hour");
        assert_eq!(view.minutes_used, 180);
        assert_eq!(view.next_extension, "email");
        clock.advance(minutes(60));
        assert!(matches!(
            v.extend(&hash, &EmailInput::default(), &[]),
            Err(VisitError::EmailRequired)
        ));
        assert!(matches!(
            v.extend(&hash, &email("nope"), &[]),
            Err(VisitError::InvalidEmail(_))
        ));
        let view = v
            .extend(&hash, &email("ada@example.com"), &["a".into()])
            .expect("4th");
        assert!(view.email_given);
        assert_eq!(view.minutes_used, 240);
        assert_eq!(view.minutes_allowed, 360);
        for _ in 0..2 {
            clock.advance(minutes(60));
            v.extend(&hash, &EmailInput::default(), &[])
                .expect("5th and 6th");
        }
        clock.advance(minutes(60));
        assert!(matches!(
            v.extend(&hash, &EmailInput::default(), &[]),
            Err(VisitError::DailyLimit { .. })
        ));
        assert_eq!(v.view(&hash).expect("view").next_extension, "none");
        // The same address cannot start another visit today.
        assert!(matches!(
            v.start("s2", &email("ADA@example.com")),
            Err(VisitError::DailyLimit { .. })
        ));
        // The visit ends on time.
        clock.advance(minutes(15));
        assert!(!v.alive(&hash));
        assert_eq!(v.sweep(), vec![hash]);
        // A new day: the address may come back.
        clock.advance(minutes(9 * 60));
        v.start("s2", &email("ada@example.com")).expect("next day");
    }

    #[test]
    fn anonymous_visits_per_session_and_no_capture() {
        let (v, _) = visits(true);
        for _ in 0..3 {
            v.start("s1", &EmailInput::default()).expect("anonymous");
        }
        assert_eq!(
            v.start("s1", &EmailInput::default()),
            Err(VisitError::EmailRequired)
        );
        v.start("s1", &email("bob@example.org"))
            .expect("with email");
        v.start("s2", &EmailInput::default())
            .expect("other session");

        // Without email capture the free time is all there is.
        let (v, clock) = visits(false);
        let (_, hash) = v.start("s1", &email("ignored@example.org")).expect("start");
        for _ in 0..2 {
            clock.advance(minutes(50));
            v.extend(&hash, &EmailInput::default(), &[]).expect("free");
            clock.advance(minutes(10));
        }
        clock.advance(minutes(50));
        assert!(matches!(
            v.extend(&hash, &email("ada@example.com"), &[]),
            Err(VisitError::TimeUsed { .. })
        ));
        assert_eq!(v.view(&hash).expect("view").next_extension, "none");
    }

    #[test]
    fn rotate_end_pause_and_rates() {
        let (v, clock) = visits(true);
        let (old, hash) = v.start("s1", &EmailInput::default()).expect("start");
        let (new, new_hash) = v.rotate(&hash).expect("rotate");
        assert!(v.resolve([old.as_str()]).is_empty());
        assert_eq!(v.resolve([new.as_str()]), vec![new_hash.clone()]);
        assert_eq!(
            v.hash_for_id(&new_hash[..16]).as_deref(),
            Some(new_hash.as_str())
        );
        assert!(v.end(&new_hash));
        assert!(!v.alive(&new_hash));
        v.pause(true);
        assert_eq!(
            v.start("s1", &EmailInput::default()),
            Err(VisitError::Paused)
        );
        v.pause(false);

        for _ in 0..10 {
            v.note_creation("s9").expect("within the hour");
        }
        assert!(matches!(
            v.note_creation("s9"),
            Err(VisitError::RateLimited { .. })
        ));
        clock.advance(minutes(61));
        v.note_creation("s9").expect("an hour later");
        for i in 0..29 {
            v.note_creation(&format!("flood{i}"))
                .expect("instance rate");
        }
        assert!(matches!(
            v.note_creation("one-more"),
            Err(VisitError::RateLimited { .. })
        ));
    }

    #[test]
    fn visits_survive_a_restart() {
        let tmp = tempfile::tempdir().expect("tmp");
        let clock = Arc::new(ManualClock::new(at("2026-10-18T09:00:00Z")));
        let cfg = SelfServiceConfig {
            enabled: true,
            leads_salt: Some("salt".into()),
            ..SelfServiceConfig::default()
        };
        let make = || {
            Visits::new(cfg.clone(), clock.clone(), Arc::new(Leads::disabled()))
                .with_persistence(tmp.path().to_path_buf())
        };
        let first = make();
        let (token, hash) = first.start("s1", &EmailInput::default()).expect("start");
        let second = make();
        assert_eq!(second.resolve([token.as_str()]), vec![hash.clone()]);
        assert_eq!(second.view(&hash).expect("view").minutes_used, 60);
        clock.advance(Duration::from_secs(3601));
        assert!(make().resolve([token.as_str()]).is_empty());
    }
}
