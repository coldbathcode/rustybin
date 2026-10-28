//! The `limit_test` pattern (`docs/plans/harness.md`, section 6.3): traffic
//! shapes that probe a rate limiter for its known weak spots. Each test
//! compiles into ordinary stages (the scheduler, the preview and the caps
//! treat it like any pattern) plus what the engine does on top: an identity
//! pool (`many_keys`, `many_ips`), rotating forwarded addresses
//! (`many_ips`), retries of refused requests (`retry_wave`), and a closed
//! loop (`limit_probe`). What the gateway did is judged afterwards by the
//! rate-limit analysis (`ratelimit.rs`).
//!
//! The limit is `limit` requests per `window`; both default to the layer's
//! `expect` (filled in by [`super::scenario::Scenario::normalise`]).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::pattern::{Pattern, Shape, SyncMode, MAX_VALUE, MIN_PERIOD};
use super::scenario::Dur;

/// Most identities of a pool (`many_keys`, `many_ips`): the addresses of
/// `many_ips` come from 198.18.0.0/15 (RFC 2544, set aside for benchmarks).
pub const MAX_IDENTITIES: u32 = 100_000;
/// Most windows (or bursts) a test repeats.
pub const MAX_CYCLES: u32 = 10_000;
/// Most retries of one refused request (`retry_wave`).
pub const MAX_RETRIES: u32 = 10;
/// Most probes of a `limit_probe`.
pub const MAX_STEPS: u32 = 20;
/// The longest a `retry_wave` client waits for a `Retry-After`.
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(60);
/// Seconds a `limit_probe` adds after its last probe, for the loop's last
/// decision to land.
pub const PROBE_SLACK_SECS: f64 = 2.0;

/// Which weak spot a `limit_test` probes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestKind {
    OverLimit,
    WindowEdge,
    JustUnder,
    Distributed,
    ManyKeys,
    ManyIps,
    RetryWave,
    Concurrency,
    SlowRise,
    LimitProbe,
}

impl TestKind {
    pub const ALL: [TestKind; 10] = [
        TestKind::OverLimit,
        TestKind::WindowEdge,
        TestKind::JustUnder,
        TestKind::Distributed,
        TestKind::ManyKeys,
        TestKind::ManyIps,
        TestKind::RetryWave,
        TestKind::Concurrency,
        TestKind::SlowRise,
        TestKind::LimitProbe,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TestKind::OverLimit => "over_limit",
            TestKind::WindowEdge => "window_edge",
            TestKind::JustUnder => "just_under",
            TestKind::Distributed => "distributed",
            TestKind::ManyKeys => "many_keys",
            TestKind::ManyIps => "many_ips",
            TestKind::RetryWave => "retry_wave",
            TestKind::Concurrency => "concurrency",
            TestKind::SlowRise => "slow_rise",
            TestKind::LimitProbe => "limit_probe",
        }
    }

    /// What the test sends and what it finds, for the schema and the docs.
    pub fn describe(self) -> &'static str {
        match self {
            TestKind::OverLimit => "over_limit: one identity at factor times its limit for a while. Finds basic enforcement, the time to the first refusal and how many got through.",
            TestKind::WindowEdge => "window_edge: size requests just before a window boundary and size again just after, aligned to the wall clock, at every other boundary (cycles times). Finds fixed-window double dipping (up to twice the limit within one window length).",
            TestKind::JustUnder => "just_under: fraction (90 percent) of the limit for a long time. Finds false positives: legitimate users refused under their limit.",
            TestKind::Distributed => "distributed: every client node sends factor times the limit for one identity at the same window start, through every target. Finds counters local to each gateway node (over-admission per node).",
            TestKind::ManyKeys => "many_keys: identities ({{id}} in a header or the path), each under its limit, adding up to factor times a global or per-IP limit. Finds whether global, per-IP and per-route limits hold.",
            TestKind::ManyIps => "many_ips: rotating forwarded addresses (header, default X-Forwarded-For, from 198.18.0.0/15) at factor times a per-IP limit. Finds per-IP limits that trust a forwarded header they should not.",
            TestKind::RetryWave => "retry_wave: factor times the limit, and every refused request is retried at once (or after Retry-After with honour_retry_after). Finds how the limiter recovers under retries.",
            TestKind::Concurrency => "concurrency: size requests at once every every (new connections and requests in flight, not a rate). Finds connection and concurrency limits.",
            TestKind::SlowRise => "slow_rise: the rate rising slowly from under the limit (from) to over it (to). Finds the rate where enforcement starts, against the configured limit.",
            TestKind::LimitProbe => "limit_probe: a binary search on the rate between min and max, one probe every every. Finds the effective limit (and window) when the configuration is unknown.",
        }
    }
}

/// Where the windows of `window_edge` and `distributed` start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    /// At multiples of the window on the wall clock (Unix time), as fixed
    /// window limiters usually count (minute 12:00:00 to 12:01:00).
    #[default]
    Clock,
    /// At the layer's start (limiters whose window opens with the first request).
    Start,
}

/// The parameters of a `limit_test` pattern. Which apply depends on `test`;
/// validation names the ones that do not.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitTest {
    pub test: TestKind,
    /// Requests per window the limiter allows (default: the layer's
    /// `expect.limit`). `limit_probe` measures it instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// The limiter's window (default: the layer's `expect.window`, else 1s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<Dur>,
    /// Multiple of the limit to send (`over_limit` 10, `distributed` 4,
    /// `many_keys` and `many_ips` 5, `retry_wave` 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub factor: Option<f64>,
    /// Fraction of the limit to send (`just_under`, default 0.9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f64>,
    /// How long the test sends.
    #[serde(rename = "for", default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<Dur>,
    /// Window boundaries (`window_edge`), window starts (`distributed`) or
    /// bursts (`concurrency`), default 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycles: Option<u32>,
    /// Requests per burst (`window_edge`, `concurrency`; default the limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    /// How quickly a burst is sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<Dur>,
    /// How often a burst (`concurrency`, default the window) or a probe
    /// (`limit_probe`, default 2s) comes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<Dur>,
    /// The size of the identity pool (`many_keys`, `many_ips`, default 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identities: Option<u32>,
    /// The forwarded-address header (`many_ips`, default x-forwarded-for).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// Retries of a refused request (`retry_wave`, default 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
    /// Wait for `Retry-After` before retrying (`retry_wave`, default false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub honour_retry_after: Option<bool>,
    /// Pause before a retry that ignores `Retry-After` (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_delay: Option<Dur>,
    /// `slow_rise`: the first and last rate (default half and one and a
    /// half times the limit's rate) and how long the rise takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over: Option<Dur>,
    /// `limit_probe`: the rates the search starts between (min default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// `limit_probe`: how many probes (default 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<Align>,
    /// After the test, send at half the limit for this long: the report
    /// measures the time to recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recover: Option<Dur>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jitter: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<Box<Pattern>>,
}

/// One stage a test compiles into.
pub struct Piece {
    pub len: f64,
    pub shape: Shape,
    /// Shifted to the wall clock's windows.
    pub aligned: bool,
    /// The test name, or `recover`.
    pub test: &'static str,
}

/// What the engine does on top of the rate while a test's stage runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Behaviour {
    pub test: TestKind,
    /// The identity pool size (1: one identity).
    pub identities: u32,
    /// `many_ips`: the header that carries the forwarded address.
    pub forwarded_header: Option<String>,
    /// `retry_wave`: retries per refused request.
    pub retries: u32,
    pub honour_retry_after: bool,
    pub retry_delay: Duration,
}

/// The address of identity `id` in 198.18.0.0/15.
pub fn identity_ip(id: u32) -> String {
    let id = id % (2 * 65_536);
    format!("198.{}.{}.{}", 18 + id / 65_536, (id / 256) % 256, id % 256)
}

impl LimitTest {
    /// A test with only its kind set (the rest default).
    pub fn new(test: TestKind) -> Self {
        Self {
            test,
            limit: None,
            window: None,
            factor: None,
            fraction: None,
            hold: None,
            cycles: None,
            size: None,
            within: None,
            every: None,
            identities: None,
            header: None,
            retries: None,
            honour_retry_after: None,
            retry_delay: None,
            from: None,
            to: None,
            over: None,
            min: None,
            max: None,
            steps: None,
            align: None,
            recover: None,
            jitter: None,
            sync: None,
            then: None,
        }
    }

    pub fn window_secs(&self) -> f64 {
        self.window.map_or(1.0, |w| w.0.as_secs_f64()).max(1e-3)
    }

    fn limit_f(&self) -> f64 {
        self.limit.unwrap_or(0) as f64
    }

    /// The limit as a rate (requests per second).
    pub fn limit_rate(&self) -> f64 {
        self.limit_f() / self.window_secs()
    }

    fn factor(&self) -> f64 {
        self.factor.unwrap_or(match self.test {
            TestKind::Distributed => 4.0,
            TestKind::ManyKeys | TestKind::ManyIps => 5.0,
            TestKind::RetryWave => 3.0,
            _ => 10.0,
        })
    }

    fn cycles(&self) -> f64 {
        f64::from(self.cycles.unwrap_or(3))
    }

    fn within_secs(&self) -> f64 {
        match self.within {
            Some(w) => w.0.as_secs_f64(),
            None if self.test == TestKind::Concurrency => 0.010,
            None => (self.window_secs() / 10.0).min(0.1),
        }
    }

    /// `concurrency`: the burst period; `limit_probe`: the probe length.
    pub fn every_secs(&self) -> f64 {
        match self.every {
            Some(e) => e.0.as_secs_f64(),
            None if self.test == TestKind::LimitProbe => 2.0,
            None => self.window_secs(),
        }
    }

    pub fn steps(&self) -> u32 {
        self.steps.unwrap_or(8)
    }

    pub fn probe_min(&self) -> f64 {
        self.min.unwrap_or(1.0)
    }

    pub fn probe_max(&self) -> f64 {
        self.max.unwrap_or(0.0)
    }

    pub fn align(&self) -> Align {
        self.align.unwrap_or_default()
    }

    /// Whether the windows of the test are on the wall clock.
    pub fn aligned(&self) -> bool {
        matches!(self.test, TestKind::WindowEdge | TestKind::Distributed)
            && self.align() == Align::Clock
    }

    /// The engine's part while the test runs.
    pub fn behaviour(&self) -> Behaviour {
        let pooled = matches!(self.test, TestKind::ManyKeys | TestKind::ManyIps);
        Behaviour {
            test: self.test,
            identities: if pooled {
                self.identities.unwrap_or(20).clamp(1, MAX_IDENTITIES)
            } else {
                1
            },
            forwarded_header: (self.test == TestKind::ManyIps).then(|| {
                self.header
                    .clone()
                    .unwrap_or_else(|| "x-forwarded-for".into())
                    .to_ascii_lowercase()
            }),
            retries: if self.test == TestKind::RetryWave {
                self.retries.unwrap_or(3).min(MAX_RETRIES)
            } else {
                0
            },
            honour_retry_after: self.honour_retry_after.unwrap_or(false),
            retry_delay: self.retry_delay.map_or(Duration::ZERO, |d| d.0),
        }
    }

    /// The stages the test compiles into, in order.
    pub fn pieces(&self) -> Vec<Piece> {
        let w = self.window_secs();
        let lr = self.limit_rate();
        let hold = self.hold.map_or(0.0, |d| d.0.as_secs_f64());
        let flat = |rate: f64, len: f64| Piece {
            len,
            shape: Shape::Line {
                from: rate,
                to: rate,
            },
            aligned: false,
            test: self.test.as_str(),
        };
        let mut out = vec![match self.test {
            TestKind::OverLimit | TestKind::ManyKeys | TestKind::ManyIps | TestKind::RetryWave => {
                flat(lr * self.factor(), hold)
            }
            TestKind::JustUnder => flat(lr * self.fraction.unwrap_or(0.9), hold),
            TestKind::WindowEdge => {
                let within = self.within_secs();
                let lead = within / 4.0;
                Piece {
                    len: self.cycles() * 2.0 * w + lead + within,
                    shape: Shape::Edge {
                        size: f64::from(self.size.unwrap_or(self.limit.unwrap_or(0) as u32)),
                        window_secs: w,
                        within_secs: within,
                        lead_secs: lead,
                    },
                    aligned: self.aligned(),
                    test: self.test.as_str(),
                }
            }
            TestKind::Distributed => {
                let within = self.within_secs();
                Piece {
                    len: self.cycles() * w + within,
                    shape: Shape::Burst {
                        base: 0.0,
                        size: (self.limit_f() * self.factor()).round(),
                        within_secs: within,
                        every_secs: w,
                    },
                    aligned: self.aligned(),
                    test: self.test.as_str(),
                }
            }
            TestKind::Concurrency => {
                let every = self.every_secs();
                Piece {
                    len: self.cycles() * every,
                    shape: Shape::Burst {
                        base: 0.0,
                        size: f64::from(self.size.unwrap_or(self.limit.unwrap_or(0) as u32)),
                        within_secs: self.within_secs(),
                        every_secs: every,
                    },
                    aligned: false,
                    test: self.test.as_str(),
                }
            }
            TestKind::SlowRise => Piece {
                len: self.over.map_or(0.0, |d| d.0.as_secs_f64()),
                shape: Shape::Line {
                    from: self.from.unwrap_or(lr * 0.5),
                    to: self.to.unwrap_or(lr * 1.5),
                },
                aligned: false,
                test: self.test.as_str(),
            },
            TestKind::LimitProbe => Piece {
                len: f64::from(self.steps()) * self.every_secs() + PROBE_SLACK_SECS,
                shape: Shape::Closed {
                    initial: (self.probe_min() + self.probe_max()) / 2.0,
                    max: self.probe_max(),
                },
                aligned: false,
                test: self.test.as_str(),
            },
        }];
        if let Some(r) = self.recover {
            out.push(Piece {
                len: r.0.as_secs_f64(),
                shape: Shape::Line {
                    from: lr * 0.5,
                    to: lr * 0.5,
                },
                aligned: false,
                test: "recover",
            });
        }
        out
    }

    /// Every problem with the test, prefixed with `here`.
    pub fn validate(&self, here: &str, problems: &mut Vec<String>) {
        use TestKind::*;
        let t = self.test;
        let name = t.as_str();
        // Which parameters the test takes.
        let takes: &[&str] = match t {
            OverLimit => &["limit", "window", "factor", "for", "recover"],
            WindowEdge => &[
                "limit", "window", "size", "within", "cycles", "align", "recover",
            ],
            JustUnder => &["limit", "window", "fraction", "for", "recover"],
            Distributed => &[
                "limit", "window", "factor", "within", "cycles", "align", "recover",
            ],
            ManyKeys => &["limit", "window", "factor", "identities", "for", "recover"],
            ManyIps => &[
                "limit",
                "window",
                "factor",
                "identities",
                "header",
                "for",
                "recover",
            ],
            RetryWave => &[
                "limit",
                "window",
                "factor",
                "for",
                "retries",
                "honour_retry_after",
                "retry_delay",
                "recover",
            ],
            Concurrency => &[
                "limit", "window", "size", "within", "every", "cycles", "recover",
            ],
            SlowRise => &["limit", "window", "from", "to", "over", "recover"],
            LimitProbe => &["limit", "window", "min", "max", "every", "steps"],
        };
        let set: [(&str, bool); 20] = [
            ("factor", self.factor.is_some()),
            ("fraction", self.fraction.is_some()),
            ("for", self.hold.is_some()),
            ("cycles", self.cycles.is_some()),
            ("size", self.size.is_some()),
            ("within", self.within.is_some()),
            ("every", self.every.is_some()),
            ("identities", self.identities.is_some()),
            ("header", self.header.is_some()),
            ("retries", self.retries.is_some()),
            ("honour_retry_after", self.honour_retry_after.is_some()),
            ("retry_delay", self.retry_delay.is_some()),
            ("from", self.from.is_some()),
            ("to", self.to.is_some()),
            ("over", self.over.is_some()),
            ("min", self.min.is_some()),
            ("max", self.max.is_some()),
            ("steps", self.steps.is_some()),
            ("align", self.align.is_some()),
            ("recover", self.recover.is_some()),
        ];
        for (field, present) in set {
            if present && !takes.contains(&field) {
                problems.push(format!(
                    "{here}.{field}: not a parameter of the {name} test (it takes {})",
                    takes.join(", ")
                ));
            }
        }
        if let Some(w) = self.window {
            if w.0 < MIN_PERIOD {
                problems.push(format!("{here}.window: at least 10ms"));
            }
        }
        match self.limit {
            None if t != LimitProbe => problems.push(format!(
                "{here}.limit: required (requests per window), or set expect.limit on the layer"
            )),
            Some(0) => problems.push(format!("{here}.limit: at least 1 request per window")),
            Some(l) if l as f64 > MAX_VALUE => {
                problems.push(format!("{here}.limit: at most {MAX_VALUE}"))
            }
            _ => {}
        }
        let positive = |field: &str, d: Option<Dur>, problems: &mut Vec<String>| match d {
            None => problems.push(format!("{here}.{field}: required by the {name} test")),
            Some(d) if d.0.is_zero() => {
                problems.push(format!("{here}.{field}: must be longer than 0"))
            }
            _ => {}
        };
        if matches!(t, OverLimit | JustUnder | ManyKeys | ManyIps | RetryWave) {
            positive("for", self.hold, problems);
        }
        if let Some(f) = self.factor {
            if !f.is_finite() || !(1.0..=1000.0).contains(&f) {
                problems.push(format!(
                    "{here}.factor: 1 to 1000 (a multiple of the limit)"
                ));
            }
        }
        if let Some(f) = self.fraction {
            if !f.is_finite() || !(0.1..=1.0).contains(&f) {
                problems.push(format!(
                    "{here}.fraction: 0.1 to 1 (a fraction of the limit)"
                ));
            }
        }
        if let Some(c) = self.cycles {
            if c == 0 || c > MAX_CYCLES {
                problems.push(format!("{here}.cycles: 1 to {MAX_CYCLES}"));
            }
        }
        if let Some(s) = self.size {
            if s == 0 || s > super::pattern::MAX_BURST {
                problems.push(format!(
                    "{here}.size: 1 to {} requests",
                    super::pattern::MAX_BURST
                ));
            }
        }
        if let Some(n) = self.identities {
            if n == 0 || n > MAX_IDENTITIES {
                problems.push(format!("{here}.identities: 1 to {MAX_IDENTITIES}"));
            }
        }
        if let Some(h) = &self.header {
            if axum::http::HeaderName::from_bytes(h.as_bytes()).is_err() {
                problems.push(format!("{here}.header: not a header name"));
            }
        }
        if self.retries.is_some_and(|r| r == 0 || r > MAX_RETRIES) {
            problems.push(format!("{here}.retries: 1 to {MAX_RETRIES}"));
        }
        if self.retry_delay.is_some_and(|d| d.0 > MAX_RETRY_WAIT) {
            problems.push(format!("{here}.retry_delay: at most 60s"));
        }
        let w = self.window_secs();
        match t {
            WindowEdge => {
                let within = self.within_secs();
                if within <= 0.0 {
                    problems.push(format!("{here}.within: must be longer than 0"));
                } else if within * 2.5 > w {
                    problems.push(format!(
                        "{here}.within: at most 40 percent of the window (a burst on each side of a boundary)"
                    ));
                }
            }
            Distributed => {
                let within = self.within_secs();
                if within <= 0.0 || within >= w {
                    problems.push(format!(
                        "{here}.within: longer than 0 and shorter than the window"
                    ));
                }
            }
            Concurrency => {
                let every = self.every_secs();
                if every < MIN_PERIOD.as_secs_f64() {
                    problems.push(format!("{here}.every: at least 10ms"));
                }
                let within = self.within_secs();
                if within <= 0.0 || within >= every {
                    problems.push(format!(
                        "{here}.within: longer than 0 and shorter than every"
                    ));
                }
            }
            SlowRise => {
                positive("over", self.over, problems);
                for (field, v) in [("from", self.from), ("to", self.to)] {
                    if v.is_some_and(|v| !v.is_finite() || !(0.0..=MAX_VALUE).contains(&v)) {
                        problems.push(format!(
                            "{here}.{field}: 0 to {MAX_VALUE} requests per second"
                        ));
                    }
                }
                let lr = self.limit_rate();
                if self.from.unwrap_or(lr * 0.5) >= self.to.unwrap_or(lr * 1.5) {
                    problems.push(format!("{here}.to: must be above from (the rate rises)"));
                }
            }
            LimitProbe => {
                match self.max {
                    None => problems.push(format!(
                        "{here}.max: required (the highest rate the search tries)"
                    )),
                    Some(m) if !m.is_finite() || m <= 0.0 || m > MAX_VALUE => {
                        problems.push(format!("{here}.max: above 0, at most {MAX_VALUE}"))
                    }
                    Some(m) if self.probe_min() >= m => {
                        problems.push(format!("{here}.min: must be below max"))
                    }
                    _ => {}
                }
                if self.min.is_some_and(|m| !m.is_finite() || m < 0.0) {
                    problems.push(format!("{here}.min: 0 or more"));
                }
                let every = self.every_secs();
                if every < 2.0 || every.fract() != 0.0 {
                    problems.push(format!(
                        "{here}.every: whole seconds, at least 2s (the first second of each probe settles)"
                    ));
                }
                if self.steps.is_some_and(|s| s == 0 || s > MAX_STEPS) {
                    problems.push(format!("{here}.steps: 1 to {MAX_STEPS}"));
                }
            }
            _ => {}
        }
    }
}

/// The schema properties of `limit_test` (the `type`, `jitter`, `sync` and
/// `then` come from the pattern variant).
pub fn schema_properties(
    max_rate: f64,
    max_dur: u64,
    duration: impl Fn(&str, u64) -> Value,
) -> Value {
    let tests: Vec<&str> = TestKind::ALL.iter().map(|t| t.as_str()).collect();
    let described = TestKind::ALL
        .iter()
        .map(|t| t.describe())
        .collect::<Vec<_>>()
        .join(" ");
    let number = |d: &str, min: f64, max: f64| json!({ "type": "number", "description": d, "minimum": min, "maximum": max });
    let integer = |d: &str, min: u64, max: u64| json!({ "type": "integer", "description": d, "minimum": min, "maximum": max });
    json!({
        "test": { "type": "string", "description": format!("The test to run. {described}"), "enum": tests },
        "limit": integer("Requests per window the limiter allows (default: the layer's expect.limit). limit_probe measures it instead.", 1, MAX_VALUE as u64),
        "window": duration("The limiter's window (default: the layer's expect.window, else 1s).", 24 * 3600),
        "factor": number("A multiple of the limit to send (over_limit 10, distributed 4, many_keys and many_ips 5, retry_wave 3).", 1.0, 1000.0),
        "fraction": number("just_under: the fraction of the limit to send (default 0.9).", 0.1, 1.0),
        "for": duration("How long the test sends (over_limit, just_under, many_keys, many_ips, retry_wave).", max_dur),
        "cycles": integer("Window boundaries (window_edge), window starts (distributed) or bursts (concurrency), default 3.", 1, MAX_CYCLES as u64),
        "size": integer("Requests per burst (window_edge, concurrency; default the limit).", 1, super::pattern::MAX_BURST as u64),
        "within": duration("How quickly a burst is sent (default a tenth of the window, at most 100ms; concurrency 10ms).", max_dur),
        "every": duration("concurrency: how often a burst comes (default the window); limit_probe: how long each probe lasts (whole seconds, default 2s).", max_dur),
        "identities": integer("many_keys and many_ips: the size of the identity pool (default 20). {{id}} is the identity, {{ip}} its address.", 1, MAX_IDENTITIES as u64),
        "header": { "type": "string", "description": "many_ips: the header that carries the forwarded address (default x-forwarded-for).", "maxLength": 64 },
        "retries": integer("retry_wave: retries of each refused request (default 3).", 1, MAX_RETRIES as u64),
        "honour_retry_after": { "type": "boolean", "description": "retry_wave: wait for Retry-After before retrying (default false: retry at once)." },
        "retry_delay": duration("retry_wave: the pause before a retry that ignores Retry-After (default 0).", MAX_RETRY_WAIT.as_secs()),
        "from": number("slow_rise: the first rate, requests per second (default half the limit's rate).", 0.0, max_rate),
        "to": number("slow_rise: the last rate (default one and a half times the limit's rate).", 0.0, max_rate),
        "over": duration("slow_rise: how long the rise takes.", max_dur),
        "min": number("limit_probe: the lowest rate the search tries (default 1).", 0.0, max_rate),
        "max": number("limit_probe: the highest rate the search tries.", 0.0, max_rate),
        "steps": integer("limit_probe: how many probes (default 8).", 1, MAX_STEPS as u64),
        "align": { "type": "string", "description": "window_edge and distributed: where windows start: clock (multiples of the window on the wall clock, the default) or start (the layer's start).", "enum": ["clock", "start"] },
        "recover": duration("After the test, send at half the limit for this long: the report measures the time to recovery.", max_dur)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test(yaml: &str) -> LimitTest {
        match serde_yaml::from_str::<Pattern>(yaml).expect("pattern") {
            Pattern::LimitTest(t) => t,
            other => panic!("not a limit test: {other:?}"),
        }
    }

    fn problems(t: &LimitTest) -> Vec<String> {
        let mut p = Vec::new();
        t.validate("p", &mut p);
        p
    }

    #[test]
    fn identity_addresses_stay_in_the_benchmark_range() {
        assert_eq!(identity_ip(0), "198.18.0.0");
        assert_eq!(identity_ip(257), "198.18.1.1");
        assert_eq!(identity_ip(65_536 + 2), "198.19.0.2");
        assert_eq!(identity_ip(2 * 65_536), "198.18.0.0");
    }

    #[test]
    fn tests_compile_into_stages() {
        let t =
            test("type: limit_test\ntest: over_limit\nlimit: 10\nwindow: 1s\nfor: 3s\nrecover: 2s");
        let p = t.pieces();
        assert_eq!(p.len(), 2);
        assert_eq!(
            p[0].shape,
            Shape::Line {
                from: 100.0,
                to: 100.0
            }
        );
        assert_eq!(p[1].test, "recover");
        assert_eq!(p[1].shape, Shape::Line { from: 5.0, to: 5.0 });

        let t = test("type: limit_test\ntest: window_edge\nlimit: 20\nwindow: 1s\ncycles: 2");
        let p = t.pieces();
        assert!(p[0].aligned);
        let Shape::Edge {
            size,
            within_secs,
            lead_secs,
            ..
        } = p[0].shape
        else {
            panic!("{:?}", p[0].shape)
        };
        assert_eq!(size, 20.0);
        assert!((within_secs - 0.1).abs() < 1e-9 && (lead_secs - 0.025).abs() < 1e-9);

        let t = test("type: limit_test\ntest: limit_probe\nmax: 100\nsteps: 4\nevery: 2s");
        let p = t.pieces();
        assert_eq!(p[0].len, 8.0 + PROBE_SLACK_SECS);
        assert!(
            matches!(p[0].shape, Shape::Closed { initial, max } if initial == 50.5 && max == 100.0)
        );

        let t = test("type: limit_test\ntest: many_ips\nlimit: 5\nfor: 1s\nidentities: 7");
        let b = t.behaviour();
        assert_eq!(b.identities, 7);
        assert_eq!(b.forwarded_header.as_deref(), Some("x-forwarded-for"));
    }

    #[test]
    fn validation_names_what_a_test_does_not_take() {
        let t =
            test("type: limit_test\ntest: over_limit\nlimit: 10\nfor: 1s\nidentities: 3\nsteps: 2");
        let p = problems(&t);
        assert!(
            p.iter()
                .any(|m| m.contains("p.identities: not a parameter of the over_limit")),
            "{p:?}"
        );
        assert!(p.iter().any(|m| m.contains("p.steps")), "{p:?}");

        let t = test("type: limit_test\ntest: just_under\nwindow: 1s");
        let p = problems(&t);
        assert!(p.iter().any(|m| m.contains("p.limit: required")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("p.for: required")), "{p:?}");

        let t = test("type: limit_test\ntest: limit_probe\nmin: 5\nmax: 5\nevery: 1500ms");
        let p = problems(&t);
        assert!(
            p.iter().any(|m| m.contains("p.min: must be below max")),
            "{p:?}"
        );
        assert!(
            p.iter().any(|m| m.contains("p.every: whole seconds")),
            "{p:?}"
        );

        let t = test("type: limit_test\ntest: window_edge\nlimit: 5\nwindow: 100ms\nwithin: 50ms");
        assert!(problems(&t).iter().any(|m| m.contains("40 percent")));

        let ok = test("type: limit_test\ntest: slow_rise\nlimit: 10\nover: 10s");
        assert!(problems(&ok).is_empty(), "{:?}", problems(&ok));
    }
}
