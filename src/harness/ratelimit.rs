//! The rate-limit analysis (`docs/plans/harness.md`, section 6.3): what the
//! gateway's limiter did with a layer's traffic, judged against the layer's
//! `expect`.
//!
//! While the run runs, every answer of a run that analyses rate limits is
//! counted (by the roll, from the workers' bounded buffers) in a
//! [`LimitAcc`] per layer: admitted, refused and failed
//! requests in fine cells of send time (a twentieth of the window, 10 ms to
//! 1 s, on a grid whose windows start where the limiter's do), per identity
//! (the first [`MAX_TRACKED`]), per target and per gateway node (a response
//! header), and what the rate-limit headers said. Accumulators add up
//! exactly, so a cluster's leader merges its client nodes' like the
//! histograms. After the run, [`analyse`] turns them into a
//! [`RateLimitReport`] per layer: admission per window and in the worst
//! window-long span, over- and under-admission, the time to the first
//! refusal, recovery, the headers, collateral damage to the other layers,
//! upstream protection, and findings in words.

use std::collections::BTreeMap;

use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::limit_test::TestKind;
use super::live::Controls;
use super::pattern::{Compiled, Ctx};
use super::report::ThresholdResult;
use super::scenario::{Expect, Layer, Scenario};

/// Most cells per layer (an hour at 100 ms).
pub const MAX_CELLS: usize = 36_000;
/// Identities followed one by one (the rest are added up).
pub const MAX_TRACKED: usize = 64;
/// Windows followed per identity and per gateway node.
pub const MAX_WINDOWS: usize = 3_600;
/// Gateway nodes (values of the node header) followed.
pub const MAX_NODES: usize = 16;
/// Distinct advertised limits kept.
pub const MAX_ADVERTISED: usize = 8;
/// Identities and nodes shown in a report.
pub const MAX_SHOWN: usize = 16;

// ── Response headers ────────────────────────────────────────────────

/// What one answer's rate-limit headers said.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Observed {
    /// `RateLimit` (any IETF draft form, `RateLimit-Limit` and friends too).
    pub ratelimit: bool,
    pub ratelimit_policy: bool,
    /// `X-RateLimit-*`.
    pub x_ratelimit: bool,
    pub retry_after: bool,
    /// The `Retry-After` value, in seconds, when it parses.
    pub retry_after_secs: Option<f64>,
    pub remaining: Option<i64>,
    /// The advertised limit (requests per window).
    pub limit: Option<i64>,
    /// The gateway node (the `expect.node_header` value).
    pub node: Option<String>,
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
}

fn leading_int(s: &str) -> Option<i64> {
    let s = s.trim().trim_matches('"');
    let digits: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

/// A `key=value` (or structured-field `;key=value`) parameter of a header.
fn param(value: &str, keys: &[&str]) -> Option<i64> {
    value
        .split([',', ';'])
        .filter_map(|part| part.split_once('='))
        .find(|(k, _)| keys.contains(&k.trim().to_ascii_lowercase().as_str()))
        .and_then(|(_, v)| leading_int(v))
}

/// Parse `Retry-After`: delay seconds or an HTTP date.
pub fn retry_after_secs(value: &str) -> Option<f64> {
    if let Ok(n) = value.trim().parse::<f64>() {
        return (n.is_finite() && n >= 0.0).then_some(n);
    }
    let at = chrono::DateTime::parse_from_rfc2822(value.trim()).ok()?;
    let delta = at.signed_duration_since(chrono::Utc::now());
    Some((delta.num_milliseconds() as f64 / 1000.0).max(0.0))
}

/// Read the rate-limit headers of an answer.
pub fn observe(headers: &HeaderMap, node_header: Option<&str>) -> Observed {
    let mut o = Observed::default();
    // The IETF drafts: `RateLimit: limit=10, remaining=5, reset=3` (early),
    // `RateLimit: "default";r=5;t=3` with `RateLimit-Policy: "default";q=10;w=60`
    // (later), and `RateLimit-Limit` / `-Remaining` / `-Reset` (first).
    if let Some(v) = header(headers, "ratelimit") {
        o.ratelimit = true;
        o.remaining = param(v, &["remaining", "r"]);
        o.limit = param(v, &["limit", "q"]);
    }
    for (name, slot) in [
        ("ratelimit-remaining", 0usize),
        ("ratelimit-limit", 1),
        ("ratelimit-reset", 2),
    ] {
        if let Some(v) = header(headers, name) {
            o.ratelimit = true;
            match slot {
                0 => o.remaining = o.remaining.or_else(|| leading_int(v)),
                1 => o.limit = o.limit.or_else(|| leading_int(v)),
                _ => {}
            }
        }
    }
    if let Some(v) = header(headers, "ratelimit-policy") {
        o.ratelimit_policy = true;
        o.limit = o
            .limit
            .or_else(|| param(v, &["q", "limit"]))
            .or_else(|| leading_int(v));
    }
    for (name, slot) in [
        ("x-ratelimit-remaining", 0usize),
        ("x-ratelimit-limit", 1),
        ("x-ratelimit-reset", 2),
    ] {
        if let Some(v) = header(headers, name) {
            o.x_ratelimit = true;
            match slot {
                0 => o.remaining = o.remaining.or_else(|| leading_int(v)),
                1 => o.limit = o.limit.or_else(|| leading_int(v)),
                _ => {}
            }
        }
    }
    if let Some(v) = header(headers, "retry-after") {
        o.retry_after = true;
        o.retry_after_secs = retry_after_secs(v);
    }
    if let Some(name) = node_header {
        o.node = header(headers, name)
            .filter(|v| !v.is_empty())
            .map(|v| v.chars().take(64).collect());
    }
    o
}

// ── Accumulation ────────────────────────────────────────────────────

/// Where a layer's windows lie: windows of `window` seconds start at run
/// times `origin + k * window`; cells are `window / cells_per_window` long,
/// cell 0 starting one window before `origin`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Grid {
    pub window: f64,
    pub origin: f64,
    pub cells_per_window: u32,
}

impl Grid {
    /// A grid for `window` seconds, its windows starting at `origin` (run
    /// time, folded into one window).
    pub fn new(window: f64, origin: f64) -> Self {
        let window = window.max(1e-3);
        // A twentieth of the window, 10 ms to 1 s.
        let at_least = window.ceil() as u32;
        let at_most = (window / 0.010).floor() as u32;
        let n = 20u32.max(at_least).min(at_most).max(1);
        Self {
            window,
            origin: origin.rem_euclid(window),
            cells_per_window: n,
        }
    }

    pub fn res(&self) -> f64 {
        self.window / f64::from(self.cells_per_window.max(1))
    }

    fn base(&self) -> f64 {
        self.origin - self.window
    }

    /// The cell of run time `t`.
    pub fn cell(&self, t: f64) -> usize {
        ((t - self.base()) / self.res()).floor().max(0.0) as usize
    }

    /// The run time cell `j` starts at.
    pub fn cell_start(&self, j: usize) -> f64 {
        self.base() + j as f64 * self.res()
    }

    pub fn window_of_cell(&self, j: usize) -> usize {
        j / self.cells_per_window.max(1) as usize
    }
}

/// One answer, as the accumulator needs it.
#[derive(Clone, Debug, Default)]
pub struct Obs {
    /// Send time on the run clock.
    pub at: f64,
    /// The identity of a pool (`None`: the layer has one identity).
    pub id: Option<u32>,
    pub target: usize,
    pub limited: bool,
    /// A transport failure (no answer).
    pub error: bool,
    pub retry: bool,
    /// The answer carries this run's served-by tag: the upstream made it.
    pub tagged: bool,
    pub latency_us: u64,
    pub headers: Observed,
}

/// Counts in one cell of send time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell(pub u64, pub u64, pub u64, pub u64);

impl Cell {
    pub fn admitted(&self) -> u64 {
        self.0
    }
    pub fn limited(&self) -> u64 {
        self.1
    }
    pub fn errors(&self) -> u64 {
        self.2
    }
    pub fn latency_us(&self) -> u64 {
        self.3
    }
    pub fn answered(&self) -> u64 {
        self.0 + self.1
    }
    fn add(&mut self, o: &Cell) {
        self.0 += o.0;
        self.1 += o.1;
        self.2 += o.2;
        self.3 += o.3;
    }
}

/// An identity, a target or a gateway node: totals and admitted / refused
/// per window.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Party {
    pub admitted: u64,
    pub limited: u64,
    pub errors: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<[u32; 2]>,
}

impl Party {
    fn record(&mut self, w: Option<usize>, o: &Obs) {
        if o.error {
            self.errors += 1;
            return;
        }
        if o.limited {
            self.limited += 1;
        } else {
            self.admitted += 1;
        }
        if let Some(w) = w.filter(|w| *w < MAX_WINDOWS) {
            if self.windows.len() <= w {
                self.windows.resize(w + 1, [0, 0]);
            }
            self.windows[w][usize::from(o.limited)] += 1;
        }
    }

    fn merge(&mut self, o: &Party) {
        self.admitted += o.admitted;
        self.limited += o.limited;
        self.errors += o.errors;
        if self.windows.len() < o.windows.len() {
            self.windows.resize(o.windows.len(), [0, 0]);
        }
        for (a, b) in self.windows.iter_mut().zip(&o.windows) {
            a[0] += b[0];
            a[1] += b[1];
        }
    }

    /// The most admitted in one window.
    pub fn worst_window(&self) -> u64 {
        self.windows
            .iter()
            .map(|w| u64::from(w[0]))
            .max()
            .unwrap_or(0)
    }
}

/// What the rate-limit headers said, counted.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HeaderStats {
    /// Admitted answers carrying each family (`ratelimit`,
    /// `ratelimit_policy`, `x_ratelimit`, `retry_after`).
    pub admitted_with: BTreeMap<String, u64>,
    pub limited_with: BTreeMap<String, u64>,
    /// Refusals with a `Retry-After` that does not parse.
    pub retry_after_invalid: u64,
    /// Refusals that said requests remain.
    pub remaining_on_limited: u64,
    /// Advertised limits and how often each was seen.
    pub advertised: BTreeMap<String, u64>,
    /// From one admitted answer to the next (completion order, worker by
    /// worker within each roll): the remaining count went down, up or stayed.
    pub remaining_down: u64,
    pub remaining_up: u64,
    pub remaining_same: u64,
    /// The last remaining count seen (not merged: completion order is a
    /// node's own).
    #[serde(skip)]
    last_remaining: Option<i64>,
}

impl HeaderStats {
    fn record(&mut self, o: &Obs, single_identity: bool) {
        if o.error {
            return;
        }
        let h = &o.headers;
        let into = if o.limited {
            &mut self.limited_with
        } else {
            &mut self.admitted_with
        };
        for (name, on) in [
            ("ratelimit", h.ratelimit),
            ("ratelimit_policy", h.ratelimit_policy),
            ("x_ratelimit", h.x_ratelimit),
            ("retry_after", h.retry_after),
        ] {
            if on {
                *into.entry(name.into()).or_default() += 1;
            }
        }
        if o.limited {
            if h.retry_after && h.retry_after_secs.is_none() {
                self.retry_after_invalid += 1;
            }
            if h.remaining.is_some_and(|r| r > 0) {
                self.remaining_on_limited += 1;
            }
        } else if single_identity {
            if let Some(r) = h.remaining {
                match self.last_remaining {
                    Some(last) if r < last => self.remaining_down += 1,
                    Some(last) if r > last => self.remaining_up += 1,
                    Some(_) => self.remaining_same += 1,
                    None => {}
                }
                self.last_remaining = Some(r);
            }
        }
        if let Some(l) = h.limit {
            let key = l.to_string();
            if let Some(n) = self.advertised.get_mut(&key) {
                *n += 1;
            } else if self.advertised.len() < MAX_ADVERTISED {
                self.advertised.insert(key, 1);
            }
        }
    }

    fn merge(&mut self, o: &HeaderStats) {
        for (into, from) in [
            (&mut self.admitted_with, &o.admitted_with),
            (&mut self.limited_with, &o.limited_with),
        ] {
            for (k, v) in from {
                *into.entry(k.clone()).or_default() += v;
            }
        }
        for (k, v) in &o.advertised {
            if let Some(n) = self.advertised.get_mut(k) {
                *n += v;
            } else if self.advertised.len() < MAX_ADVERTISED {
                self.advertised.insert(k.clone(), *v);
            }
        }
        self.retry_after_invalid += o.retry_after_invalid;
        self.remaining_on_limited += o.remaining_on_limited;
        self.remaining_down += o.remaining_down;
        self.remaining_up += o.remaining_up;
        self.remaining_same += o.remaining_same;
    }
}

/// One layer's rate-limit counters (per node, then merged by the leader).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LimitAcc {
    pub grid: Grid,
    pub admitted: u64,
    pub limited: u64,
    pub errors: u64,
    pub cells: Vec<Cell>,
    /// Answers outside the cells (over [`MAX_CELLS`]).
    #[serde(default)]
    pub cells_dropped: u64,
    /// The first [`MAX_TRACKED`] identities, by identity.
    #[serde(default)]
    pub identities: BTreeMap<u32, Party>,
    /// Every other identity, added up.
    #[serde(default)]
    pub other_identities: Party,
    /// The highest identity seen plus one.
    #[serde(default)]
    pub identities_seen: u32,
    /// By target index.
    #[serde(default)]
    pub targets: Vec<Party>,
    /// By the node header's value.
    #[serde(default)]
    pub nodes: BTreeMap<String, Party>,
    #[serde(default)]
    pub headers: HeaderStats,
    /// Retries sent (`retry_wave`) and how many were admitted.
    #[serde(default)]
    pub retries: u64,
    #[serde(default)]
    pub retries_admitted: u64,
    /// Refusals the upstream made (tagged answers), not the gateway.
    #[serde(default)]
    pub limited_by_upstream: u64,
}

impl LimitAcc {
    pub fn new(grid: Grid) -> Self {
        Self {
            grid,
            ..Self::default()
        }
    }

    pub fn record(&mut self, o: &Obs) {
        if o.error {
            self.errors += 1;
        } else if o.limited {
            self.limited += 1;
        } else {
            self.admitted += 1;
        }
        if o.limited && o.tagged {
            self.limited_by_upstream += 1;
        }
        if o.retry {
            self.retries += 1;
            if !o.error && !o.limited {
                self.retries_admitted += 1;
            }
        }
        let j = self.grid.cell(o.at);
        if j < MAX_CELLS {
            if self.cells.len() <= j {
                self.cells.resize(j + 1, Cell::default());
            }
            let c = &mut self.cells[j];
            if o.error {
                c.2 += 1;
            } else if o.limited {
                c.1 += 1;
            } else {
                c.0 += 1;
            }
            c.3 += o.latency_us;
        } else {
            self.cells_dropped += 1;
        }
        let w = (j < MAX_CELLS).then(|| self.grid.window_of_cell(j));
        if let Some(id) = o.id {
            self.identities_seen = self.identities_seen.max(id.saturating_add(1));
            if let Some(p) = self.identities.get_mut(&id) {
                p.record(w, o);
            } else if self.identities.len() < MAX_TRACKED {
                let mut p = Party::default();
                p.record(w, o);
                self.identities.insert(id, p);
            } else {
                self.other_identities.record(None, o);
            }
        }
        if self.targets.len() <= o.target {
            self.targets.resize(o.target + 1, Party::default());
        }
        self.targets[o.target].record(w, o);
        if let Some(node) = &o.headers.node {
            if let Some(p) = self.nodes.get_mut(node) {
                p.record(w, o);
            } else if self.nodes.len() < MAX_NODES {
                let mut p = Party::default();
                p.record(w, o);
                self.nodes.insert(node.clone(), p);
            }
        }
        self.headers.record(o, o.id.is_none());
    }

    /// Add another worker's or node's counters (same grid).
    pub fn merge(&mut self, o: &LimitAcc) {
        if self.grid.cells_per_window == 0 {
            self.grid = o.grid;
        }
        self.admitted += o.admitted;
        self.limited += o.limited;
        self.errors += o.errors;
        if self.cells.len() < o.cells.len() {
            self.cells.resize(o.cells.len(), Cell::default());
        }
        for (a, b) in self.cells.iter_mut().zip(&o.cells) {
            a.add(b);
        }
        self.cells_dropped += o.cells_dropped;
        for (id, p) in &o.identities {
            if let Some(mine) = self.identities.get_mut(id) {
                mine.merge(p);
            } else if self.identities.len() < MAX_TRACKED {
                self.identities.insert(*id, p.clone());
            } else {
                let mut flat = p.clone();
                flat.windows.clear();
                self.other_identities.merge(&flat);
            }
        }
        self.other_identities.merge(&o.other_identities);
        self.identities_seen = self.identities_seen.max(o.identities_seen);
        if self.targets.len() < o.targets.len() {
            self.targets.resize(o.targets.len(), Party::default());
        }
        for (a, b) in self.targets.iter_mut().zip(&o.targets) {
            a.merge(b);
        }
        for (k, p) in &o.nodes {
            if let Some(mine) = self.nodes.get_mut(k) {
                mine.merge(p);
            } else if self.nodes.len() < MAX_NODES {
                self.nodes.insert(k.clone(), p.clone());
            }
        }
        self.headers.merge(&o.headers);
        self.retries += o.retries;
        self.retries_admitted += o.retries_admitted;
        self.limited_by_upstream += o.limited_by_upstream;
    }

    pub fn is_empty(&self) -> bool {
        self.admitted + self.limited + self.errors == 0
    }
}

/// The grid of a layer: the window it is judged in (`expect.window`, else
/// its first limit test's, else 1 s), its windows on the wall clock unless
/// a test aligns them to the layer's start.
pub fn grid_for(layer: &Layer, starts_at: f64, wall_start: f64) -> Grid {
    let tests = layer.pattern.limit_tests();
    let first = tests.iter().flatten().next();
    let window = layer
        .expect
        .as_ref()
        .map(|e| e.window.0.as_secs_f64())
        .or_else(|| first.map(|t| t.window_secs()))
        .unwrap_or(1.0);
    let start_aligned = tests
        .iter()
        .flatten()
        .any(|t| t.align() == super::limit_test::Align::Start);
    let origin = if start_aligned {
        starts_at
    } else {
        -wall_start
    };
    Grid::new(window, origin)
}

// ── The analysis ────────────────────────────────────────────────────

/// Admission per window.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowStats {
    /// Windows with answers.
    pub count: u64,
    pub admitted_max: u64,
    pub admitted_mean: f64,
    /// Windows with a refusal.
    pub limited: u64,
    /// Windows that admitted more than the limit (plus burst).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over_limit: Option<u64>,
    /// The most admitted within any window-long span (not only the
    /// limiter's own windows), to within a cell, and where it started.
    pub sliding_max: u64,
    pub sliding_at_secs: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PartyReport {
    pub name: String,
    pub admitted: u64,
    pub limited: u64,
    pub errors: u64,
    /// The most admitted in one window.
    pub worst_window: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HeaderReport {
    /// Fractions of the admitted and of the refused answers carrying each
    /// header family.
    pub on_admitted: BTreeMap<String, f64>,
    pub on_limited: BTreeMap<String, f64>,
    /// Refusals with a `Retry-After` (a fraction of the refusals).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_on_limited: Option<f64>,
    pub retry_after_invalid: u64,
    /// Refusals that still said requests remain.
    pub remaining_on_limited: u64,
    pub advertised_limits: Vec<String>,
    /// Whether the advertised limit is the expected one (`None`: no
    /// expectation or nothing advertised).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_matches: Option<bool>,
    pub remaining_down: u64,
    pub remaining_up: u64,
    pub remaining_same: u64,
    /// Whether the remaining count counts down (`None`: not shown, or a pool
    /// of identities).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counting_down: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UpstreamReport {
    /// Requests of the layer that reached the server side.
    pub reached: u64,
    pub admitted: u64,
    pub limited: u64,
    /// Refusals the upstream made itself (tagged answers).
    #[serde(default)]
    pub limited_by_upstream: u64,
    /// The gateway's refusals stayed away from the upstream.
    pub shielded: bool,
}

/// Another layer during this layer's test.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Collateral {
    pub layer: String,
    pub answered_during: u64,
    pub limited_during: u64,
    pub limited_fraction_during: f64,
    pub limited_fraction_outside: f64,
    pub latency_ms_during: f64,
    pub latency_ms_outside: f64,
}

/// What the limiter did with one layer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RateLimitReport {
    pub layer: String,
    /// The tests the layer ran.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Expect>,
    pub window_secs: f64,
    pub resolution_secs: f64,
    pub admitted: u64,
    pub limited: u64,
    pub errors: u64,
    /// Refusals per answer.
    pub limited_fraction: f64,
    pub windows: WindowStats,
    /// How far the worst window-long span went over limit plus burst, as a
    /// fraction of the limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over_admission: Option<f64>,
    /// How far admission fell short of the limit in windows that sent more,
    /// on average, as a fraction of the limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub under_admission: Option<f64>,
    /// From the layer's start to the first refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_limited_secs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted_before_first_limited: Option<u64>,
    /// The pattern's rate when the first refusal came.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_at_first_limited: Option<f64>,
    /// From the start of `recover` to the first answer after which nothing
    /// was refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_secs: Option<f64>,
    /// The spacing of admission resuming after refusals (the limiter's
    /// window, as measured).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_window_secs: Option<f64>,
    /// A `limit_probe`'s result: the highest rate with no refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_limit_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<PartyReport>,
    #[serde(default)]
    pub identities_seen: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gateway_nodes: Vec<PartyReport>,
    pub headers: HeaderReport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<PartyReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<UpstreamReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collateral: Vec<Collateral>,
    pub findings: Vec<String>,
    /// Answers outside the cells (a run over the cell limit).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cells_dropped: u64,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

fn round(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

fn ratio(a: u64, b: u64) -> f64 {
    if b == 0 {
        0.0
    } else {
        round(a as f64 / b as f64)
    }
}

/// A layer as the analysis sees it.
struct Analysed<'a> {
    name: String,
    layer: &'a Layer,
    compiled: Compiled,
    starts_at: f64,
    acc: Option<&'a LimitAcc>,
}

impl Analysed<'_> {
    /// Run-time spans of the stages running a test (not `recover`).
    fn test_spans(&self) -> Vec<(f64, f64)> {
        self.compiled
            .stages
            .iter()
            .filter(|s| s.test.is_some_and(|t| t != "recover"))
            .map(|s| (self.starts_at + s.start_secs, self.starts_at + s.end_secs))
            .collect()
    }

    fn recover_span(&self) -> Option<(f64, f64)> {
        self.compiled
            .stages
            .iter()
            .find(|s| s.test == Some("recover"))
            .map(|s| (self.starts_at + s.start_secs, self.starts_at + s.end_secs))
    }
}

/// Answered, refused and latency of a layer's cells inside / outside spans.
fn split(acc: &LimitAcc, spans: &[(f64, f64)]) -> ([u64; 3], [u64; 3]) {
    let (mut inside, mut outside) = ([0u64; 3], [0u64; 3]);
    for (j, c) in acc.cells.iter().enumerate() {
        let mid = acc.grid.cell_start(j) + acc.grid.res() / 2.0;
        let into = if spans.iter().any(|(a, b)| mid >= *a && mid < *b) {
            &mut inside
        } else {
            &mut outside
        };
        into[0] += c.answered() + c.errors();
        into[1] += c.limited();
        into[2] += c.latency_us();
    }
    (inside, outside)
}

fn party(name: String, p: &Party) -> PartyReport {
    PartyReport {
        name,
        admitted: p.admitted,
        limited: p.limited,
        errors: p.errors,
        worst_window: p.worst_window(),
    }
}

/// The rate-limit analysis of every analysed layer, and the layers'
/// thresholds. `accs` is by layer index (the scenario's, then added ones);
/// `arrivals` the server side's marked requests by layer index, when the
/// run had a server side.
pub fn analyse(
    s: &Scenario,
    controls: &Controls,
    accs: &[Option<LimitAcc>],
    arrivals: Option<&BTreeMap<String, u64>>,
) -> (Vec<RateLimitReport>, Vec<ThresholdResult>) {
    let effective = controls.effective_layers(s);
    let layers: Vec<Analysed> = effective
        .iter()
        .enumerate()
        .map(|(i, (l, starts_at))| Analysed {
            name: l.name.clone(),
            layer: l,
            compiled: l.pattern.compile(),
            starts_at: *starts_at,
            acc: accs.get(i).and_then(Option::as_ref),
        })
        .collect();
    let probes: Vec<(String, f64)> = controls
        .findings
        .iter()
        .filter(|f| f.kind == "limit_probe")
        .filter_map(|f| f.rate.map(|r| (f.layer.clone(), r)))
        .collect();
    let mut reports = Vec::new();
    let mut thresholds = Vec::new();
    for (i, a) in layers.iter().enumerate() {
        if !a.layer.analysed() {
            continue;
        }
        let empty = LimitAcc::new(Grid::new(1.0, 0.0));
        let acc = a.acc.unwrap_or(&empty);
        let mut r = layer_report(a, acc, &layers, i);
        r.measured_limit_rate = probes
            .iter()
            .find(|(l, _)| *l == a.name)
            .map(|(_, rate)| round(*rate));
        if let Some(rate) = r.measured_limit_rate {
            let w = r.window_secs;
            let mut f = format!(
                "limit_probe: the effective limit is about {rate:.1} requests per second ({:.0} per {}s window)",
                rate * w,
                w
            );
            if let Some(e) = &a.layer.expect {
                let expected = e.limit as f64 / e.window.0.as_secs_f64();
                f.push_str(&format!(
                    "; expected {expected:.1} per second ({:+.1} percent)",
                    (rate - expected) / expected * 100.0
                ));
            }
            r.findings.insert(0, f);
        }
        if let Some(by_layer) = arrivals {
            let reached = by_layer.get(&i.to_string()).copied().unwrap_or(0);
            // Refusals the upstream made itself reached it by definition.
            let expected = acc.admitted + acc.limited_by_upstream + acc.errors;
            let slack = (acc.admitted + acc.limited) / 100 + 1;
            let shielded = reached <= expected + slack;
            if acc.limited_by_upstream > 0 {
                r.findings.push(format!(
                    "{} refusals came from the upstream itself (answers tagged by a Rustybin server), not from the gateway",
                    acc.limited_by_upstream
                ));
            }
            if !shielded && acc.limited > acc.limited_by_upstream {
                r.findings.push(format!(
                    "upstream not shielded: {reached} requests reached the upstream for {} admitted; the limiter forwarded refused requests and rewrote the answers",
                    acc.admitted
                ));
            }
            r.upstream = Some(UpstreamReport {
                reached,
                admitted: acc.admitted,
                limited: acc.limited,
                limited_by_upstream: acc.limited_by_upstream,
                shielded,
            });
        }
        let t = &a.layer.thresholds;
        let mut push = |name: &str, limit: f64, measured: Option<f64>, pass: bool| {
            thresholds.push(ThresholdResult {
                name: format!("layers.{}.{name}", a.name),
                limit: limit.into(),
                measured: measured.map_or(Value::Null, |m| round(m).into()),
                pass,
            });
        };
        if let Some(limit) = t.rate_limited {
            let m = r.limited_fraction;
            push("rate_limited", limit, Some(m), m <= limit);
        }
        if let Some(limit) = t.over_admission {
            let m = r.over_admission;
            push("over_admission", limit, m, m.is_some_and(|m| m <= limit));
        }
        if let Some(limit) = t.under_admission {
            // No window sent more than the limit: nothing fell short.
            let m = r.under_admission.unwrap_or(0.0);
            push("under_admission", limit, Some(m), m <= limit);
        }
        reports.push(r);
    }
    (reports, thresholds)
}

fn layer_report(
    a: &Analysed,
    acc: &LimitAcc,
    layers: &[Analysed],
    index: usize,
) -> RateLimitReport {
    let g = acc.grid;
    let n = g.cells_per_window.max(1) as usize;
    let tests: Vec<TestKind> = a
        .layer
        .pattern
        .limit_tests()
        .into_iter()
        .flatten()
        .map(|t| t.test)
        .collect();
    let expect = a.layer.expect.clone();
    let allowed = expect.as_ref().map(|e| e.limit + e.burst.unwrap_or(0));
    let mut findings = Vec::new();
    // Per window, the limiter's own (aligned) windows.
    let mut windows: Vec<[u64; 2]> = Vec::new();
    for (j, c) in acc.cells.iter().enumerate() {
        let w = j / n;
        if windows.len() <= w {
            windows.resize(w + 1, [0, 0]);
        }
        windows[w][0] += c.admitted();
        windows[w][1] += c.limited();
    }
    let active: Vec<&[u64; 2]> = windows.iter().filter(|w| w[0] + w[1] > 0).collect();
    let admitted_max = active.iter().map(|w| w[0]).max().unwrap_or(0);
    let admitted_mean = if active.is_empty() {
        0.0
    } else {
        active.iter().map(|w| w[0]).sum::<u64>() as f64 / active.len() as f64
    };
    // The worst window-long span, cell by cell.
    let (mut sliding_max, mut sliding_at, mut sum) = (0u64, 0usize, 0u64);
    for j in 0..acc.cells.len() {
        sum += acc.cells[j].admitted();
        if j >= n {
            sum -= acc.cells[j - n].admitted();
        }
        if sum > sliding_max {
            sliding_max = sum;
            sliding_at = (j + 1).saturating_sub(n);
        }
    }
    let over_limit = allowed.map(|al| active.iter().filter(|w| w[0] > al).count() as u64);
    let window_stats = WindowStats {
        count: active.len() as u64,
        admitted_max,
        admitted_mean: round(admitted_mean),
        limited: active.iter().filter(|w| w[1] > 0).count() as u64,
        over_limit,
        sliding_max,
        sliding_at_secs: round((g.cell_start(sliding_at) - a.starts_at).max(0.0)),
    };
    let over_admission = expect.as_ref().map(|e| {
        let limit = e.limit.max(1) as f64;
        round((sliding_max as f64 - allowed.unwrap_or(e.limit) as f64).max(0.0) / limit)
    });
    let under_admission = expect.as_ref().and_then(|e| {
        let short: Vec<f64> = windows
            .iter()
            .filter(|w| w[0] + w[1] > e.limit)
            .map(|w| (e.limit as f64 - w[0] as f64).max(0.0) / e.limit.max(1) as f64)
            .collect();
        (!short.is_empty()).then(|| round(short.iter().sum::<f64>() / short.len() as f64))
    });
    // The first refusal.
    let first = acc.cells.iter().position(|c| c.limited() > 0);
    let first_limited_secs = first.map(|j| round((g.cell_start(j) - a.starts_at).max(0.0)));
    let admitted_before = first.map(|j| acc.cells[..j].iter().map(Cell::admitted).sum());
    let rate_at_first = first.and_then(|j| {
        let t = g.cell_start(j) + g.res() / 2.0 - a.starts_at;
        a.compiled.value_at(t, &Ctx::default()).map(round)
    });
    // Recovery after the test (when something was refused to recover from).
    let recovery_secs = a
        .recover_span()
        .filter(|_| acc.limited > 0)
        .and_then(|(from, to)| {
            let cells: Vec<(usize, &Cell)> = acc
                .cells
                .iter()
                .enumerate()
                .filter(|(j, _)| {
                    let mid = g.cell_start(*j) + g.res() / 2.0;
                    mid >= from && mid < to
                })
                .collect();
            let last_refused = cells.iter().rposition(|(_, c)| c.limited() > 0);
            let start = last_refused.map_or(0, |k| k + 1);
            cells[start..]
                .iter()
                .find(|(_, c)| c.admitted() > 0)
                .map(|(j, _)| round((g.cell_start(*j) - from).max(0.0)))
        });
    // Admission resuming after refusals, in the tests' spans: its spacing
    // is the window of a limiter that refills all at once.
    let spans = a.test_spans();
    let mut resumes = Vec::new();
    let mut blocked = false;
    for (j, c) in acc.cells.iter().enumerate() {
        let mid = g.cell_start(j) + g.res() / 2.0;
        if !spans.iter().any(|(x, y)| mid >= *x && mid < *y) {
            continue;
        }
        if c.limited() > 0 && c.admitted() == 0 {
            blocked = true;
        } else if c.admitted() > 0 && blocked {
            resumes.push(g.cell_start(j));
            blocked = false;
        }
    }
    let measured_window_secs = (resumes.len() >= 3).then(|| {
        let mut gaps: Vec<f64> = resumes.windows(2).map(|w| w[1] - w[0]).collect();
        gaps.sort_by(f64::total_cmp);
        round(gaps[gaps.len() / 2])
    });
    // Identities and gateway nodes.
    let mut identities: Vec<PartyReport> = acc
        .identities
        .iter()
        .map(|(id, p)| {
            let name = if tests.contains(&TestKind::ManyIps) {
                super::limit_test::identity_ip(*id)
            } else {
                id.to_string()
            };
            party(name, p)
        })
        .collect();
    identities.sort_by(|x, y| {
        y.worst_window
            .cmp(&x.worst_window)
            .then(y.admitted.cmp(&x.admitted))
    });
    identities.truncate(MAX_SHOWN);
    let mut gateway_nodes: Vec<PartyReport> = Vec::new();
    if acc
        .targets
        .iter()
        .filter(|p| p.admitted + p.limited > 0)
        .count()
        > 1
    {
        for (k, p) in acc.targets.iter().enumerate() {
            gateway_nodes.push(party(format!("target {k}"), p));
        }
    }
    for (k, p) in &acc.nodes {
        gateway_nodes.push(party(k.clone(), p));
    }
    gateway_nodes.truncate(MAX_SHOWN);
    // Headers.
    let h = &acc.headers;
    let frac = |m: &BTreeMap<String, u64>, of: u64| -> BTreeMap<String, f64> {
        m.iter().map(|(k, v)| (k.clone(), ratio(*v, of))).collect()
    };
    let advertised: Vec<String> = h.advertised.keys().cloned().collect();
    let limit_matches = match (&expect, advertised.is_empty()) {
        (Some(e), false) => Some(advertised.iter().any(|v| *v == e.limit.to_string())),
        _ => None,
    };
    let moves = h.remaining_down + h.remaining_up + h.remaining_same;
    let headers = HeaderReport {
        on_admitted: frac(&h.admitted_with, acc.admitted),
        on_limited: frac(&h.limited_with, acc.limited),
        retry_after_on_limited: (acc.limited > 0).then(|| {
            ratio(
                h.limited_with.get("retry_after").copied().unwrap_or(0),
                acc.limited,
            )
        }),
        retry_after_invalid: h.retry_after_invalid,
        remaining_on_limited: h.remaining_on_limited,
        advertised_limits: advertised,
        limit_matches,
        remaining_down: h.remaining_down,
        remaining_up: h.remaining_up,
        remaining_same: h.remaining_same,
        counting_down: (moves >= 2)
            .then_some(h.remaining_down > 0 && h.remaining_down >= h.remaining_up),
    };
    // Collateral damage: the other layers while this one's tests ran.
    let mut collateral = Vec::new();
    if !spans.is_empty() {
        for (k, other) in layers.iter().enumerate() {
            if k == index
                || other
                    .layer
                    .pattern
                    .limit_tests()
                    .iter()
                    .any(Option::is_some)
            {
                continue;
            }
            let Some(oacc) = other.acc else { continue };
            let (inside, outside) = split(oacc, &spans);
            if inside[0] == 0 {
                continue;
            }
            let mean = |x: &[u64; 3]| {
                if x[0] == 0 {
                    0.0
                } else {
                    round(x[2] as f64 / x[0] as f64 / 1000.0)
                }
            };
            let c = Collateral {
                layer: other.name.clone(),
                answered_during: inside[0],
                limited_during: inside[1],
                limited_fraction_during: ratio(inside[1], inside[0]),
                limited_fraction_outside: ratio(outside[1], outside[0]),
                latency_ms_during: mean(&inside),
                latency_ms_outside: mean(&outside),
            };
            if c.limited_during > 0 {
                findings.push(format!(
                    "collateral damage: {} of {}'s requests were refused while {} ran its test ({:.1} percent, {:.1} percent outside it)",
                    c.limited_during,
                    c.layer,
                    a.name,
                    c.limited_fraction_during * 100.0,
                    c.limited_fraction_outside * 100.0
                ));
            }
            collateral.push(c);
        }
    }
    // Findings, test by test.
    if let Some(e) = &expect {
        let w = e.window.0.as_secs_f64();
        let al = allowed.unwrap_or(e.limit);
        if sliding_max > al {
            let edge = tests.contains(&TestKind::WindowEdge);
            let what = if edge && admitted_max <= al {
                "fixed-window double dip"
            } else {
                "over-admission"
            };
            findings.push(format!(
                "{what}: {sliding_max} admitted within one {w}s span (at {:.2}s), against a limit of {} per {w}s{}",
                window_stats.sliding_at_secs,
                e.limit,
                if e.burst.unwrap_or(0) > 0 {
                    format!(" plus a burst of {}", e.burst.unwrap_or(0))
                } else {
                    String::new()
                }
            ));
        }
        if tests.contains(&TestKind::Distributed) && admitted_max > al {
            let nodes: Vec<&PartyReport> =
                gateway_nodes.iter().filter(|p| p.admitted > 0).collect();
            if nodes.len() > 1 && nodes.iter().all(|p| p.worst_window <= al) {
                findings.push(format!(
                    "counters local to each gateway node: {} nodes admitted up to {} each per window, {admitted_max} together against a limit of {}",
                    nodes.len(),
                    nodes.iter().map(|p| p.worst_window).max().unwrap_or(0),
                    e.limit
                ));
            }
        }
        if (tests.contains(&TestKind::ManyIps) || tests.contains(&TestKind::ManyKeys))
            && sliding_max > al
        {
            let how = if tests.contains(&TestKind::ManyIps) {
                "rotating forwarded addresses: the gateway counts a forwarded header it should not trust"
            } else {
                "many identities"
            };
            findings.push(format!(
                "the {} limit did not hold against {how} ({sliding_max} admitted within one {w}s span, limit {})",
                e.per.as_str(),
                e.limit
            ));
        }
        if let Some(u) = under_admission.filter(|u| *u > 0.05) {
            findings.push(format!(
                "under-admission: windows that sent more than the limit admitted {:.1} percent less than it on average",
                u * 100.0
            ));
        }
    }
    if tests.contains(&TestKind::JustUnder) {
        if acc.limited == 0 {
            findings.push("just_under: no refusals under the limit (no false positives)".into());
        } else {
            findings.push(format!(
                "false positives: {} requests refused under the limit (first after {:.2}s)",
                acc.limited,
                first_limited_secs.unwrap_or(0.0)
            ));
        }
    } else if !tests.is_empty() && acc.limited == 0 && acc.admitted > 0 {
        findings.push("no request was refused: no limiter enforced a limit on this traffic".into());
    }
    if let (Some(rate), true) = (rate_at_first, tests.contains(&TestKind::SlowRise)) {
        let mut f = format!("enforcement started at about {rate:.1} requests per second");
        if let Some(e) = &expect {
            let lr = e.limit as f64 / e.window.0.as_secs_f64();
            f.push_str(&format!(
                " (the limit is {lr:.1} per second, {:+.1} percent)",
                (rate - lr) / lr * 100.0
            ));
        }
        findings.push(f);
    }
    if acc.retries > 0 {
        findings.push(format!(
            "retries: {} sent, {} admitted",
            acc.retries, acc.retries_admitted
        ));
    }
    if acc.limited > 0 {
        if headers.retry_after_on_limited.is_some_and(|f| f < 1.0) {
            findings.push(format!(
                "Retry-After missing on {:.1} percent of the refusals",
                (1.0 - headers.retry_after_on_limited.unwrap_or(0.0)) * 100.0
            ));
        }
        if h.remaining_on_limited > 0 {
            findings.push(format!(
                "{} refusals said requests remain (the remaining count is inconsistent)",
                h.remaining_on_limited
            ));
        }
    }
    if limit_matches == Some(false) {
        findings.push(format!(
            "the advertised limit ({}) is not the expected {}",
            headers.advertised_limits.join(", "),
            expect.as_ref().map_or(0, |e| e.limit)
        ));
    }
    if headers.counting_down == Some(false) {
        findings.push("the remaining count does not count down".into());
    }
    if let Some(r) = recovery_secs {
        findings.push(format!("recovered {r:.2}s after the test stopped"));
    } else if a.recover_span().is_some() && acc.limited > 0 {
        findings.push("did not recover while recover ran (refusals to the end)".into());
    }
    let retries = (acc.retries > 0).then(|| PartyReport {
        name: "retries".into(),
        admitted: acc.retries_admitted,
        limited: acc.retries - acc.retries_admitted,
        errors: 0,
        worst_window: 0,
    });
    RateLimitReport {
        layer: a.name.clone(),
        tests: tests.iter().map(|t| t.as_str().to_string()).collect(),
        expect,
        window_secs: round(g.window),
        resolution_secs: round(g.res()),
        admitted: acc.admitted,
        limited: acc.limited,
        errors: acc.errors,
        limited_fraction: ratio(acc.limited, acc.admitted + acc.limited),
        windows: window_stats,
        over_admission,
        under_admission,
        first_limited_secs,
        admitted_before_first_limited: admitted_before,
        rate_at_first_limited: rate_at_first,
        recovery_secs,
        measured_window_secs,
        measured_limit_rate: None,
        identities,
        identities_seen: acc.identities_seen,
        gateway_nodes,
        headers,
        retries,
        upstream: None,
        collateral,
        findings,
        cells_dropped: acc.cells_dropped,
    }
}

/// The analysis as text lines (the text report).
pub fn to_text(reports: &[RateLimitReport]) -> Vec<String> {
    let mut out = Vec::new();
    for r in reports {
        let tests = if r.tests.is_empty() {
            String::new()
        } else {
            format!(" ({})", r.tests.join(", "))
        };
        out.push(format!(
            "  {}{tests}: {} admitted, {} refused ({:.1}%), {} failed; window {}s",
            r.layer,
            r.admitted,
            r.limited,
            r.limited_fraction * 100.0,
            r.errors,
            r.window_secs
        ));
        let mut line = format!(
            "    per window: max {} admitted, mean {:.1}; worst {}s span {} admitted",
            r.windows.admitted_max, r.windows.admitted_mean, r.window_secs, r.windows.sliding_max
        );
        if let Some(e) = &r.expect {
            line.push_str(&format!(
                " (limit {} per {} per {})",
                e.limit,
                super::scenario::format_duration(e.window.0),
                e.per.as_str()
            ));
        }
        out.push(line);
        let mut facts = Vec::new();
        if let Some(v) = r.over_admission {
            facts.push(format!("over-admission {:.1}%", v * 100.0));
        }
        if let Some(v) = r.under_admission {
            facts.push(format!("under-admission {:.1}%", v * 100.0));
        }
        if let Some(v) = r.first_limited_secs {
            facts.push(format!("first refusal after {v:.2}s"));
        }
        if let Some(v) = r.recovery_secs {
            facts.push(format!("recovery {v:.2}s"));
        }
        if let Some(v) = r.measured_window_secs {
            facts.push(format!("measured window {v:.2}s"));
        }
        if let Some(u) = &r.upstream {
            facts.push(format!(
                "upstream reached {} ({})",
                u.reached,
                if u.shielded {
                    "shielded"
                } else {
                    "not shielded"
                }
            ));
        }
        if !facts.is_empty() {
            out.push(format!("    {}", facts.join(", ")));
        }
        for f in &r.findings {
            out.push(format!("    - {f}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn rate_limit_headers_in_every_draft_form() {
        let o = observe(
            &headers(&[("ratelimit", "limit=10, remaining=4, reset=3")]),
            None,
        );
        assert!(o.ratelimit && !o.x_ratelimit);
        assert_eq!((o.limit, o.remaining), (Some(10), Some(4)));
        let o = observe(
            &headers(&[
                ("ratelimit", "\"default\";r=7;t=30"),
                ("ratelimit-policy", "\"default\";q=100;w=60"),
            ]),
            None,
        );
        assert!(o.ratelimit && o.ratelimit_policy);
        assert_eq!((o.limit, o.remaining), (Some(100), Some(7)));
        let o = observe(
            &headers(&[
                ("x-ratelimit-limit", "50"),
                ("x-ratelimit-remaining", "0"),
                ("retry-after", "2"),
                ("via", "1.1 gw-a"),
            ]),
            Some("via"),
        );
        assert!(o.x_ratelimit && o.retry_after);
        assert_eq!(
            (o.limit, o.remaining, o.retry_after_secs),
            (Some(50), Some(0), Some(2.0))
        );
        assert_eq!(o.node.as_deref(), Some("1.1 gw-a"));
        let o = observe(&headers(&[("retry-after", "soon")]), None);
        assert!(o.retry_after && o.retry_after_secs.is_none());
        assert!(retry_after_secs("Wed, 21 Oct 2015 07:28:00 GMT").is_some());
    }

    #[test]
    fn grids_cut_windows_into_cells() {
        let g = Grid::new(1.0, 0.25);
        assert_eq!(g.cells_per_window, 20);
        assert!((g.res() - 0.05).abs() < 1e-12);
        // Cell 0 starts one window before the origin.
        assert!((g.cell_start(0) - -0.75).abs() < 1e-9);
        assert_eq!(g.cell(0.25), 20);
        assert_eq!(g.window_of_cell(g.cell(0.26)), 1);
        assert_eq!(g.window_of_cell(g.cell(0.24)), 0);
        assert_eq!(Grid::new(0.1, 0.0).cells_per_window, 10);
        assert_eq!(Grid::new(60.0, 0.0).cells_per_window, 60);
        assert_eq!(Grid::new(0.005, 0.0).cells_per_window, 1);
        // The origin folds into one window.
        assert!((Grid::new(1.0, -1_700_000_000.4).origin - 0.6).abs() < 1e-6);
    }

    fn obs(at: f64, limited: bool) -> Obs {
        Obs {
            at,
            limited,
            latency_us: 1000,
            ..Obs::default()
        }
    }

    #[test]
    fn accumulators_merge_exactly() {
        let g = Grid::new(1.0, 0.0);
        let mut a = LimitAcc::new(g);
        let mut b = LimitAcc::new(g);
        for k in 0..10 {
            a.record(&obs(k as f64 * 0.1, k >= 8));
            b.record(&Obs {
                id: Some(k),
                target: 1,
                ..obs(1.0 + k as f64 * 0.1, false)
            });
        }
        let mut m = LimitAcc::new(g);
        m.merge(&a);
        m.merge(&b);
        assert_eq!((m.admitted, m.limited, m.errors), (18, 2, 0));
        assert_eq!(m.cells.iter().map(Cell::admitted).sum::<u64>(), 18);
        assert_eq!(m.identities.len(), 10);
        assert_eq!(m.identities_seen, 10);
        assert_eq!(m.targets.len(), 2);
        assert_eq!(m.targets[0].worst_window(), 8);
        assert!(!a.is_empty());
    }

    fn layer(yaml: &str) -> Scenario {
        let mut s = Scenario::parse(yaml).unwrap();
        s.normalise();
        s
    }

    #[test]
    fn a_double_dip_is_found_in_the_worst_span() {
        let s = layer(
            "name: t\ntarget: http://127.0.0.1:1\nlayers:\n  - name: heavy\n    source: { request: { path: / } }\n    pattern: { type: limit_test, test: window_edge, cycles: 1 }\n    expect: { limit: 10, window: 1s }\n    thresholds: { over_admission: 0.1 }\n",
        );
        let c = Controls::for_scenario(&s);
        let mut acc = LimitAcc::new(Grid::new(1.0, 0.0));
        // 10 just before the boundary at 1.0 and 10 just after: every one admitted.
        for k in 0..10 {
            acc.record(&obs(0.85 + k as f64 * 0.01, false));
            acc.record(&obs(1.02 + k as f64 * 0.01, false));
        }
        let (r, th) = analyse(&s, &c, &[Some(acc)], None);
        let r = &r[0];
        assert_eq!(r.windows.admitted_max, 10);
        assert_eq!(r.windows.sliding_max, 20);
        assert_eq!(r.over_admission, Some(1.0));
        assert!(
            r.findings
                .iter()
                .any(|f| f.contains("fixed-window double dip")),
            "{:?}",
            r.findings
        );
        assert_eq!(th.len(), 1);
        assert!(!th[0].pass);
        assert_eq!(th[0].name, "layers.heavy.over_admission");
    }

    #[test]
    fn first_refusal_recovery_and_the_measured_window() {
        let s = layer(
            "name: t\ntarget: http://127.0.0.1:1\nlayers:\n  - name: heavy\n    source: { request: { path: / } }\n    pattern: { type: limit_test, test: over_limit, factor: 4, for: 4s, recover: 2s }\n    expect: { limit: 5, window: 1s }\n",
        );
        let c = Controls::for_scenario(&s);
        let mut acc = LimitAcc::new(Grid::new(1.0, 0.0));
        // 20 per second for 4 s: the first 5 of each second admitted.
        for k in 0..80 {
            let t = k as f64 * 0.05;
            acc.record(&obs(t, (k % 20) >= 5));
        }
        // Recovery: refused until 4.3 s, admitted after.
        for k in 0..10 {
            let t = 4.0 + k as f64 * 0.2;
            acc.record(&obs(t, t < 4.3));
        }
        let (r, _) = analyse(&s, &c, &[Some(acc)], None);
        let r = &r[0];
        assert_eq!(r.first_limited_secs, Some(0.25));
        assert_eq!(r.admitted_before_first_limited, Some(5));
        assert_eq!(r.measured_window_secs, Some(1.0));
        assert_eq!(r.recovery_secs, Some(0.4));
        assert_eq!(r.under_admission, Some(0.0));
        assert!(
            r.findings.iter().any(|f| f.contains("recovered 0.40s")),
            "{:?}",
            r.findings
        );
    }

    #[test]
    fn refusals_the_upstream_made_are_not_the_gateways() {
        let s = layer(
            "name: t\ntarget: http://127.0.0.1:1\nlayers:\n  - name: l\n    source: { request: { path: / } }\n    pattern: { type: constant, rate: 5, for: 2s }\n    expect: { limit: 5, window: 1s }\n",
        );
        let c = Controls::for_scenario(&s);
        let mut acc = LimitAcc::new(Grid::new(1.0, 0.0));
        for k in 0..10 {
            acc.record(&Obs {
                tagged: true,
                ..obs(k as f64 * 0.2, k % 2 == 0)
            });
        }
        let arrivals = BTreeMap::from([("0".to_string(), 10u64)]);
        let (r, _) = analyse(&s, &c, &[Some(acc)], Some(&arrivals));
        let u = r[0].upstream.as_ref().unwrap();
        assert_eq!(u.limited_by_upstream, 5);
        assert!(u.shielded, "{u:?}");
        assert!(r[0]
            .findings
            .iter()
            .any(|f| f.contains("came from the upstream itself")));
        assert!(!r[0].findings.iter().any(|f| f.contains("not shielded")));
    }

    #[test]
    fn collateral_damage_and_upstream_protection() {
        let s = layer(
            "name: t\ntarget: http://127.0.0.1:1\nlayers:\n  - name: heavy\n    source: { request: { path: / } }\n    pattern: { type: limit_test, test: over_limit, for: 2s }\n    expect: { limit: 5, window: 1s }\n  - name: customers\n    source: { request: { path: / } }\n    pattern: { type: constant, rate: 5, for: 4s }\n    thresholds: { rate_limited: 0 }\n",
        );
        let c = Controls::for_scenario(&s);
        let g = Grid::new(1.0, 0.0);
        let mut heavy = LimitAcc::new(g);
        let mut customers = LimitAcc::new(g);
        for k in 0..20 {
            heavy.record(&obs(k as f64 * 0.1, k % 10 >= 5));
        }
        for k in 0..20 {
            let t = k as f64 * 0.2;
            customers.record(&obs(t, t < 2.0 && k % 2 == 0));
        }
        let arrivals = BTreeMap::from([("0".to_string(), 20u64)]);
        let (r, th) = analyse(&s, &c, &[Some(heavy), Some(customers)], Some(&arrivals));
        let heavy = &r[0];
        let u = heavy.upstream.as_ref().unwrap();
        assert!(!u.shielded, "{u:?}");
        assert!(heavy
            .findings
            .iter()
            .any(|f| f.contains("upstream not shielded")));
        let col = &heavy.collateral[0];
        assert_eq!(col.layer, "customers");
        assert_eq!(col.limited_during, 5);
        assert_eq!(col.limited_fraction_outside, 0.0);
        assert!(heavy
            .findings
            .iter()
            .any(|f| f.contains("collateral damage")));
        // The customers' own threshold fails.
        let t = th
            .iter()
            .find(|t| t.name == "layers.customers.rate_limited")
            .unwrap();
        assert!(!t.pass);
    }
}
