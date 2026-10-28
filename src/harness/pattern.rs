//! Run patterns: the shape of a layer's load over time (`docs/plans/harness.md`,
//! section 6.2). A pattern is a function of time giving the target rate (open
//! model) or the number of active users (closed model). Patterns chain with
//! `then:`; the chain compiles into [`Stage`]s, each with a [`Shape`] that
//! gives its value at any instant and, exactly, the requests it asks for
//! between two instants. The preview, the validation and the scheduler use
//! the same math.
//!
//! - `constant`, `ramp` (linear or exponential), `step` (plateaus), `spike`,
//!   `pulse` (a square wave), `burst` (`size` requests within `within`, every
//!   `every`, on top of a base rate) and `wave` (sine, triangle, sawtooth)
//!   are functions of time;
//! - `target` reaches and holds a rate or a number of users; with
//!   `overhead_p99_ms` it is a closed loop: the run's controller adjusts the
//!   rate every second until the gateway overhead sits at the target;
//! - `breakpoint` grows the rate plateau by plateau until a threshold of its
//!   `until` fails, then the run stops and reports the last good rate.
//!
//! Every pattern takes `jitter` (each second's value varies by up to that
//! fraction either way; the same on every node with `sync: cluster`) and
//! `sync`: `cluster` (the default) keeps every client node in lockstep, so a
//! burst reaches the gateway as one burst; `node` gives each node its own
//! phase of a periodic pattern and its own jitter, which spreads the load.
//! - `limit_test` probes a rate limiter (section 6.3, [`super::limit_test`]):
//!   each test compiles into ordinary stages, some aligned to the wall
//!   clock's windows (`window_edge`, `distributed`).

use std::f64::consts::PI;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::limit_test::LimitTest;
use super::scenario::Dur;

/// Most stages in one `then:` chain.
pub const MAX_STAGES: usize = 16;
/// Highest rate or user count a pattern may name (the caps cut lower).
pub const MAX_VALUE: f64 = 10_000_000.0;
/// Most plateaus of a `step` or `breakpoint` pattern.
pub const MAX_PLATEAUS: u64 = 10_000;
/// Most cycles of a `pulse`.
pub const MAX_CYCLES: u32 = 100_000;
/// Most requests in one burst.
pub const MAX_BURST: u32 = 1_000_000;
/// Shortest period of a periodic pattern.
pub const MIN_PERIOD: Duration = Duration::from_millis(10);

/// How a ramp moves from `from` to `to`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    #[default]
    Linear,
    Exponential,
}

impl Curve {
    pub fn as_str(self) -> &'static str {
        match self {
            Curve::Linear => "linear",
            Curve::Exponential => "exponential",
        }
    }
}

/// The form of a `wave`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaveShape {
    #[default]
    Sine,
    Triangle,
    Sawtooth,
}

/// Whether the client nodes of a cluster follow one curve in lockstep
/// (`cluster`, the default) or each their own phase (`node`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    #[default]
    Cluster,
    Node,
}

/// When a `breakpoint` stops: a plateau whose results fail one of these.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Until {
    /// Errors (transport and unexpected statuses) per answer, 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_rate: Option<f64>,
    /// Latency p99 from the intended send, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_p99_ms: Option<f64>,
    /// Gateway overhead p99, in milliseconds (needs tagged answers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overhead_p99_ms: Option<f64>,
}

impl Until {
    pub fn is_empty(&self) -> bool {
        self.error_rate.is_none() && self.latency_p99_ms.is_none() && self.overhead_p99_ms.is_none()
    }
}

/// The shape of a layer's load over time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Pattern {
    /// A steady rate (or user count) for a while: baselines and soaks.
    Constant {
        rate: f64,
        #[serde(rename = "for")]
        hold: Dur,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        jitter: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sync: Option<SyncMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<Box<Pattern>>,
    },
    /// From one rate to another over a period: where latency bends as load grows.
    Ramp {
        from: f64,
        to: f64,
        over: Dur,
        #[serde(default)]
        curve: Curve,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        jitter: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sync: Option<SyncMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<Box<Pattern>>,
    },
    /// Plateaus `start`, `start + step`, ... up to `max`, each held for
    /// `every`, then `hold` at the last one: capacity plateaus.
    Step {
        start: f64,
        step: f64,
        every: Dur,
        max: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hold: Option<Dur>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        jitter: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sync: Option<SyncMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<Box<Pattern>>,
    },
    /// Reach a rate (or a number of users) `within` a time and hold it. With
    /// `overhead_p99_ms` the rate is found by a closed loop, between `start`
    /// and `max`.
    Target {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rate: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        users: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        overhead_p99_ms: Option<f64>,
        within: Dur,
        hold: Dur,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        jitter: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sync: Option<SyncMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<Box<Pattern>>,
    },
    /// Grow the rate by `growth` (a fraction) every `every`, from `start` up
    /// to `max`, until a plateau fails a threshold of `until`: the run then
    /// stops and reports the last good rate.
    Breakpoint {
        start: f64,
        growth: f64,
        every: Dur,
        until: Until,
        max: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        jitter: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sync: Option<SyncMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<Box<Pattern>>,
    },
    /// A surge: `base` to `peak` over `rise`, held, back over `fall`, then
    /// `recover` at `base` (how long the gateway takes to recover).
    Spike {
        base: f64,
        peak: f64,
        rise: Dur,
        hold: Dur,
        fall: Dur,
        recover: Dur,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        jitter: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sync: Option<SyncMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<Box<Pattern>>,
    },
    /// A square wave: `high` for `on`, `low` for `off`, `cycles` times.
    Pulse {
        high: f64,
        low: f64,
        on: Dur,
        off: Dur,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cycles: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        jitter: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sync: Option<SyncMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<Box<Pattern>>,
    },
    /// `size` requests sent within `within`, every `every`, on top of
    /// `base`, for a while: queueing and burst allowances.
    Burst {
        base: f64,
        size: u32,
        within: Dur,
        every: Dur,
        #[serde(rename = "for")]
        hold: Dur,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        jitter: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sync: Option<SyncMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<Box<Pattern>>,
    },
    /// Smooth periodic load between `min` and `max` (a daily curve
    /// compressed in time).
    Wave {
        min: f64,
        max: f64,
        period: Dur,
        #[serde(default)]
        shape: WaveShape,
        #[serde(rename = "for")]
        hold: Dur,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        jitter: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sync: Option<SyncMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<Box<Pattern>>,
    },
    /// A rate-limit test (section 6.3): `test` names the weak spot it
    /// probes.
    LimitTest(LimitTest),
}

/// What a stage's value looks like over its own time `x` (0 at its start).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum Shape {
    Line {
        from: f64,
        to: f64,
    },
    Exp {
        from: f64,
        to: f64,
    },
    /// Plateaus `start + k * step` (`plateaus` of them), each `every_secs`,
    /// the last held to the end of the stage.
    Steps {
        start: f64,
        step: f64,
        every_secs: f64,
        plateaus: u64,
    },
    /// Plateaus `start * (1 + growth)^k`, capped at `max`.
    Growth {
        start: f64,
        growth: f64,
        every_secs: f64,
        max: f64,
        plateaus: u64,
    },
    Square {
        high: f64,
        low: f64,
        on_secs: f64,
        off_secs: f64,
    },
    Burst {
        base: f64,
        size: f64,
        within_secs: f64,
        every_secs: f64,
    },
    Wave {
        min: f64,
        max: f64,
        period_secs: f64,
        form: WaveShape,
    },
    /// Two bursts of `size` around every other window boundary
    /// (`window_edge`): one within `within_secs` ending `lead_secs` before
    /// the boundary, one starting `lead_secs` after it. The period is two
    /// windows, so each window holds one burst only and a fixed window
    /// admits both.
    Edge {
        size: f64,
        window_secs: f64,
        within_secs: f64,
        lead_secs: f64,
    },
    /// A closed loop decides the value (`initial` until it does).
    Closed {
        initial: f64,
        max: f64,
    },
}

/// One compiled stage of a pattern chain.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Stage {
    pub index: usize,
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Offset from the layer's start.
    pub start_secs: f64,
    pub end_secs: f64,
    /// The value at the start and the end of the stage, and its highest.
    pub from: f64,
    pub to: f64,
    pub peak: f64,
    pub shape: Shape,
    #[serde(skip_serializing_if = "is_zero")]
    pub jitter: f64,
    /// Which pattern of the `then:` chain the stage comes from.
    pub pattern: usize,
    /// A periodic stage shifted so its period starts on the wall clock's
    /// window boundaries (`window_edge`, `distributed`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub aligned: bool,
    /// The rate-limit test the stage runs, or `recover` after one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test: Option<&'static str>,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// How a node evaluates a pattern: the seed of its jitter and its phase
/// (with `sync: node`, a fraction of each periodic stage's period).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ctx {
    pub seed: u64,
    pub phase: f64,
    /// Unix time (seconds) of the layer's pattern time 0, for stages
    /// aligned to the wall clock (0 when unknown: previews).
    pub wall: f64,
}

/// A pseudo-random number in `[0, 1)` from a seed (splitmix64).
fn unit(seed: u64) -> f64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// A stable 64-bit hash of a string (the jitter seed of a run and layer).
pub fn seed_of(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for p in parts {
        for b in p.bytes().chain(std::iter::once(0xFF)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
    h
}

impl Shape {
    /// The period of a periodic shape.
    fn period(&self) -> Option<f64> {
        match self {
            Shape::Square {
                on_secs, off_secs, ..
            } => Some(on_secs + off_secs),
            Shape::Burst { every_secs, .. } => Some(*every_secs),
            Shape::Wave { period_secs, .. } => Some(*period_secs),
            Shape::Edge { window_secs, .. } => Some(2.0 * window_secs),
            _ => None,
        }
    }

    /// The value at `x` seconds into the stage (`len` long).
    fn value(&self, x: f64, len: f64) -> f64 {
        match self {
            Shape::Line { from, to } => {
                if len <= 0.0 {
                    *to
                } else {
                    from + (to - from) * (x / len).clamp(0.0, 1.0)
                }
            }
            Shape::Exp { from, to } => {
                if len <= 0.0 || *from <= 0.0 || *to <= 0.0 {
                    *to
                } else {
                    from * (to / from).powf((x / len).clamp(0.0, 1.0))
                }
            }
            Shape::Steps {
                every_secs,
                plateaus,
                ..
            }
            | Shape::Growth {
                every_secs,
                plateaus,
                ..
            } => self.plateau_value(plateau(x, *every_secs, *plateaus)),
            Shape::Square {
                high,
                low,
                on_secs,
                off_secs,
            } => {
                let y = x.rem_euclid(on_secs + off_secs);
                if y < *on_secs {
                    *high
                } else {
                    *low
                }
            }
            Shape::Burst {
                base,
                size,
                within_secs,
                every_secs,
            } => {
                if x.rem_euclid(*every_secs) < *within_secs {
                    base + size / within_secs
                } else {
                    *base
                }
            }
            Shape::Wave {
                min,
                max,
                period_secs,
                form,
            } => {
                let frac = x.rem_euclid(*period_secs) / period_secs;
                min + (max - min) * wave(*form, frac)
            }
            Shape::Edge {
                size,
                window_secs,
                within_secs,
                lead_secs,
            } => {
                let p = 2.0 * window_secs;
                let u = x.rem_euclid(p);
                let after = u >= *lead_secs && u < lead_secs + within_secs;
                let before = u >= p - lead_secs - within_secs && u < p - lead_secs;
                if after || before {
                    size / within_secs
                } else {
                    0.0
                }
            }
            Shape::Closed { initial, .. } => *initial,
        }
    }

    /// The integral of the value over `[0, y]` within one period (periodic
    /// shapes), and over a whole period.
    fn periodic_integral(&self, y: f64) -> (f64, f64) {
        match self {
            Shape::Square {
                high,
                low,
                on_secs,
                off_secs,
            } => (
                high * y.min(*on_secs) + low * (y - on_secs).max(0.0),
                high * on_secs + low * off_secs,
            ),
            Shape::Burst {
                base,
                size,
                within_secs,
                every_secs,
            } => (
                base * y + size / within_secs * y.min(*within_secs),
                base * every_secs + size,
            ),
            Shape::Wave {
                min,
                max,
                period_secs: p,
                form,
            } => {
                let a = max - min;
                let part = match form {
                    WaveShape::Sine => a / 2.0 * (y - p / (2.0 * PI) * (2.0 * PI * y / p).sin()),
                    WaveShape::Triangle => {
                        if y <= p / 2.0 {
                            a * y * y / p
                        } else {
                            a * p / 4.0 + a * (2.0 * (y - p / 2.0) - (y * y - p * p / 4.0) / p)
                        }
                    }
                    WaveShape::Sawtooth => a * y * y / (2.0 * p),
                };
                (min * y + part, min * p + a * p / 2.0)
            }
            Shape::Edge {
                size,
                window_secs,
                within_secs,
                lead_secs,
            } => {
                let p = 2.0 * window_secs;
                let r = size / within_secs;
                let after = (y - lead_secs).clamp(0.0, *within_secs);
                let before = (y - (p - lead_secs - within_secs)).clamp(0.0, *within_secs);
                (r * (after + before), 2.0 * size)
            }
            _ => (0.0, 0.0),
        }
    }

    /// The integral of the value over `[a, b]` of the stage (`len` long),
    /// exact for every shape.
    fn integral(&self, a: f64, b: f64, len: f64) -> f64 {
        if b <= a {
            return 0.0;
        }
        match self {
            Shape::Line { .. } => (self.value(a, len) + self.value(b, len)) / 2.0 * (b - a),
            Shape::Exp { from, to } => {
                if len <= 0.0 || *from <= 0.0 || *to <= 0.0 || (to / from - 1.0).abs() < 1e-12 {
                    return self.value(a, len) * (b - a);
                }
                let r = to / from;
                from * len / r.ln() * (r.powf(b / len) - r.powf(a / len))
            }
            Shape::Steps {
                every_secs,
                plateaus,
                ..
            }
            | Shape::Growth {
                every_secs,
                plateaus,
                ..
            } => {
                // Piecewise constant: plateau by plateau (the last one
                // holds to the end of the stage).
                let k0 = plateau(a, *every_secs, *plateaus);
                let k1 = plateau(b, *every_secs, *plateaus);
                let mut total = 0.0;
                for k in k0..=k1 {
                    let lo = if k == k0 { a } else { k as f64 * every_secs };
                    let hi = if k == k1 {
                        b
                    } else {
                        (k + 1) as f64 * every_secs
                    };
                    if hi > lo {
                        total += self.plateau_value(k) * (hi - lo);
                    }
                }
                total
            }
            Shape::Square { .. }
            | Shape::Burst { .. }
            | Shape::Wave { .. }
            | Shape::Edge { .. } => {
                let p = self.period().unwrap_or(1.0);
                let f = |x: f64| {
                    let full = (x / p).floor();
                    let (part, per) = self.periodic_integral(x - full * p);
                    full * per + part
                };
                f(b) - f(a)
            }
            Shape::Closed { initial, .. } => initial * (b - a),
        }
    }

    /// The value of plateau `k` of a `step` or `breakpoint` stage.
    fn plateau_value(&self, k: u64) -> f64 {
        match self {
            Shape::Steps {
                start,
                step,
                plateaus,
                ..
            } => start + step * k.min(plateaus.saturating_sub(1)) as f64,
            Shape::Growth {
                start,
                growth,
                max,
                plateaus,
                ..
            } => (start
                * (1.0 + growth)
                    .powi(k.min(plateaus.saturating_sub(1)).min(i32::MAX as u64) as i32))
            .min(*max),
            _ => 0.0,
        }
    }

    fn peak(&self, len: f64) -> f64 {
        match self {
            Shape::Line { from, to } | Shape::Exp { from, to } => from.max(*to),
            Shape::Steps { plateaus, .. } | Shape::Growth { plateaus, .. } => {
                let _ = len;
                self.plateau_value(plateaus.saturating_sub(1))
            }
            Shape::Square { high, low, .. } => high.max(*low),
            Shape::Burst {
                base,
                size,
                within_secs,
                ..
            } => base + size / within_secs,
            Shape::Wave { min, max, .. } => min.max(*max),
            Shape::Edge {
                size, within_secs, ..
            } => size / within_secs,
            Shape::Closed { max, .. } => *max,
        }
    }

    /// The next point after `x` where the value jumps (a plateau, pulse or
    /// burst edge), if the shape has such points.
    fn next_edge(&self, x: f64) -> Option<f64> {
        let next_multiple = |x: f64, every: f64, offset: f64| {
            let k = ((x - offset) / every).floor() + 1.0;
            offset + k * every
        };
        match self {
            Shape::Steps { every_secs, .. } | Shape::Growth { every_secs, .. } => {
                Some(next_multiple(x, *every_secs, 0.0))
            }
            Shape::Square {
                on_secs, off_secs, ..
            } => {
                let p = on_secs + off_secs;
                Some(next_multiple(x, p, 0.0).min(next_multiple(x, p, *on_secs)))
            }
            Shape::Burst {
                within_secs,
                every_secs,
                ..
            } => Some(next_multiple(x, *every_secs, 0.0).min(next_multiple(
                x,
                *every_secs,
                *within_secs,
            ))),
            Shape::Edge {
                window_secs,
                within_secs,
                lead_secs,
                ..
            } => {
                let p = 2.0 * window_secs;
                [
                    *lead_secs,
                    lead_secs + within_secs,
                    p - lead_secs - within_secs,
                    p - lead_secs,
                ]
                .into_iter()
                .map(|o| next_multiple(x, p, o))
                .reduce(f64::min)
            }
            _ => None,
        }
    }

    /// Which plateau, cycle half or burst period `x` is in (0 for shapes
    /// without parts): stages report part by part.
    fn segment(&self, x: f64) -> u64 {
        match self {
            Shape::Steps {
                every_secs,
                plateaus,
                ..
            }
            | Shape::Growth {
                every_secs,
                plateaus,
                ..
            } => plateau(x, *every_secs, *plateaus),
            Shape::Square {
                on_secs, off_secs, ..
            } => {
                let p = on_secs + off_secs;
                let k = (x / p).floor().max(0.0) as u64;
                k * 2 + u64::from(x.rem_euclid(p) >= *on_secs)
            }
            Shape::Burst { every_secs, .. } => (x / every_secs).floor().max(0.0) as u64,
            Shape::Edge { window_secs, .. } => (x / (2.0 * window_secs)).floor().max(0.0) as u64,
            _ => 0,
        }
    }
}

/// The plateau `x` falls in, `0..plateaus` (the last one holds).
fn plateau(x: f64, every: f64, plateaus: u64) -> u64 {
    if every <= 0.0 {
        return 0;
    }
    ((x / every).floor().max(0.0) as u64).min(plateaus.saturating_sub(1))
}

/// A wave's level, 0 to 1, at `frac` of its period (starting at the bottom).
fn wave(form: WaveShape, frac: f64) -> f64 {
    match form {
        WaveShape::Sine => (1.0 - (2.0 * PI * frac).cos()) / 2.0,
        WaveShape::Triangle => 1.0 - (2.0 * frac - 1.0).abs(),
        WaveShape::Sawtooth => frac,
    }
}

impl Stage {
    fn len(&self) -> f64 {
        self.end_secs - self.start_secs
    }

    /// How far a periodic stage's period is shifted for `ctx`: `phase` of
    /// the period (`sync: node`), plus, for a stage aligned to the wall
    /// clock, the offset of its start within a period of Unix time.
    pub fn shift(&self, ctx: &Ctx) -> f64 {
        match self.shape.period() {
            Some(p) => {
                let wall = if self.aligned {
                    (ctx.wall + self.start_secs).rem_euclid(p)
                } else {
                    0.0
                };
                ctx.phase * p + wall
            }
            None => 0.0,
        }
    }

    /// The nominal value at `x` into the stage, its period shifted by
    /// `shift` seconds (see [`Stage::shift`]).
    pub fn value_local(&self, x: f64, shift: f64) -> f64 {
        self.shape.value(x + shift, self.len())
    }

    /// The part of the stage `x` is in (a plateau, a pulse half, a burst).
    pub fn segment(&self, x: f64) -> u64 {
        self.shape.segment(x)
    }

    /// The nominal value of a part (the plateau's rate), for reports.
    pub fn segment_value(&self, segment: u64) -> f64 {
        match &self.shape {
            Shape::Steps { .. } | Shape::Growth { .. } => self.shape.plateau_value(segment),
            Shape::Square { high, low, .. } => {
                if segment.is_multiple_of(2) {
                    *high
                } else {
                    *low
                }
            }
            _ => self.peak,
        }
    }

    pub fn is_closed(&self) -> bool {
        matches!(self.shape, Shape::Closed { .. })
    }
}

/// A pattern chain compiled once (the scheduler evaluates it many times
/// per second).
#[derive(Clone, Debug, PartialEq)]
pub struct Compiled {
    pub stages: Vec<Stage>,
    pub total_secs: f64,
    pub sync: SyncMode,
}

impl Compiled {
    /// The stage that runs at `t` seconds into the pattern.
    pub fn stage_at(&self, t: f64) -> Option<&Stage> {
        self.stages
            .iter()
            .find(|s| t >= s.start_secs && t < s.end_secs)
    }

    /// The value at `t`, with this node's jitter and phase. `None` once
    /// the pattern is over.
    pub fn value_at(&self, t: f64, ctx: &Ctx) -> Option<f64> {
        let s = self.stage_at(t)?;
        let v = s.value_local(t - s.start_secs, s.shift(ctx));
        if s.jitter > 0.0 {
            let tick = t.max(0.0).floor() as u64;
            let u = unit(ctx.seed ^ (s.index as u64) << 56 ^ tick);
            Some((v * (1.0 + s.jitter * (2.0 * u - 1.0))).max(0.0))
        } else {
            Some(v)
        }
    }

    /// The nominal requests (or user-seconds) between two offsets.
    pub fn integral(&self, a: f64, b: f64) -> f64 {
        if b <= a {
            return 0.0;
        }
        self.stages
            .iter()
            .map(|s| {
                let lo = a.max(s.start_secs);
                let hi = b.min(s.end_secs);
                if hi <= lo {
                    0.0
                } else {
                    s.shape
                        .integral(lo - s.start_secs, hi - s.start_secs, s.len())
                }
            })
            .sum()
    }

    /// The next point after `t` where the value may jump: an edge inside
    /// the stage (with this node's phase) or the stage's end. The scheduler
    /// never steps across one.
    pub fn next_boundary(&self, t: f64, ctx: &Ctx) -> f64 {
        let Some(s) = self.stage_at(t) else {
            return f64::INFINITY;
        };
        let x = t - s.start_secs;
        let shift = s.shift(ctx);
        let edge = s
            .shape
            .next_edge(x + shift)
            .map_or(f64::INFINITY, |e| s.start_secs + e - shift);
        edge.min(s.end_secs).max(t)
    }

    /// Where the stage after the one at `t` starts (`skip_stage`), or the
    /// end of the pattern.
    pub fn next_stage_start(&self, t: f64) -> f64 {
        self.stage_at(t).map_or(self.total_secs, |s| s.end_secs)
    }

    /// The stage and part at `t` (for the reports and `breakpoint`).
    pub fn position(&self, t: f64) -> Option<(usize, u64)> {
        let s = self.stage_at(t)?;
        Some((s.index, s.segment(t - s.start_secs)))
    }
}

impl Pattern {
    fn next(&self) -> Option<&Pattern> {
        match self {
            Pattern::Constant { then, .. }
            | Pattern::Ramp { then, .. }
            | Pattern::Step { then, .. }
            | Pattern::Target { then, .. }
            | Pattern::Breakpoint { then, .. }
            | Pattern::Spike { then, .. }
            | Pattern::Pulse { then, .. }
            | Pattern::Burst { then, .. }
            | Pattern::Wave { then, .. } => then.as_deref(),
            Pattern::LimitTest(t) => t.then.as_deref(),
        }
    }

    fn next_mut(&mut self) -> Option<&mut Pattern> {
        match self {
            Pattern::Constant { then, .. }
            | Pattern::Ramp { then, .. }
            | Pattern::Step { then, .. }
            | Pattern::Target { then, .. }
            | Pattern::Breakpoint { then, .. }
            | Pattern::Spike { then, .. }
            | Pattern::Pulse { then, .. }
            | Pattern::Burst { then, .. }
            | Pattern::Wave { then, .. } => then.as_deref_mut(),
            Pattern::LimitTest(t) => t.then.as_deref_mut(),
        }
    }

    fn jitter(&self) -> f64 {
        match self {
            Pattern::Constant { jitter, .. }
            | Pattern::Ramp { jitter, .. }
            | Pattern::Step { jitter, .. }
            | Pattern::Target { jitter, .. }
            | Pattern::Breakpoint { jitter, .. }
            | Pattern::Spike { jitter, .. }
            | Pattern::Pulse { jitter, .. }
            | Pattern::Burst { jitter, .. }
            | Pattern::Wave { jitter, .. } => jitter.unwrap_or(0.0),
            Pattern::LimitTest(t) => t.jitter.unwrap_or(0.0),
        }
    }

    /// The pattern's sync mode (its first stage says it for the chain).
    pub fn sync(&self) -> SyncMode {
        match self {
            Pattern::Constant { sync, .. }
            | Pattern::Ramp { sync, .. }
            | Pattern::Step { sync, .. }
            | Pattern::Target { sync, .. }
            | Pattern::Breakpoint { sync, .. }
            | Pattern::Spike { sync, .. }
            | Pattern::Pulse { sync, .. }
            | Pattern::Burst { sync, .. }
            | Pattern::Wave { sync, .. } => sync.unwrap_or_default(),
            Pattern::LimitTest(t) => t.sync.unwrap_or_default(),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Pattern::Constant { .. } => "constant",
            Pattern::Ramp { .. } => "ramp",
            Pattern::Step { .. } => "step",
            Pattern::Target { .. } => "target",
            Pattern::Breakpoint { .. } => "breakpoint",
            Pattern::Spike { .. } => "spike",
            Pattern::Pulse { .. } => "pulse",
            Pattern::Burst { .. } => "burst",
            Pattern::Wave { .. } => "wave",
            Pattern::LimitTest(_) => "limit_test",
        }
    }

    /// The pattern types, for the docs, the MCP `list_patterns` tool and
    /// the console.
    pub fn kinds() -> &'static [&'static str] {
        &[
            "constant",
            "ramp",
            "step",
            "target",
            "breakpoint",
            "spike",
            "pulse",
            "burst",
            "wave",
            "limit_test",
        ]
    }

    /// The patterns of the chain, in order (at most one over the limit).
    pub fn chain(&self) -> Vec<&Pattern> {
        let mut out = Vec::new();
        let mut current = Some(self);
        while let Some(p) = current {
            out.push(p);
            if out.len() > MAX_STAGES {
                break;
            }
            current = p.next();
        }
        out
    }

    /// The `n`th pattern of the chain (0 is this one).
    pub fn nth_mut(&mut self, n: usize) -> Option<&mut Pattern> {
        let mut current = Some(self);
        for _ in 0..n {
            current = current?.next_mut();
        }
        current
    }

    /// The chain compiled into stages with absolute offsets.
    pub fn compile(&self) -> Compiled {
        let mut stages: Vec<Stage> = Vec::new();
        let mut at = 0.0;
        for (pattern, p) in self.chain().into_iter().take(MAX_STAGES).enumerate() {
            let jitter = p.jitter().clamp(0.0, 1.0);
            let mut push_with = |kind: &'static str,
                                 len: f64,
                                 shape: Shape,
                                 aligned: bool,
                                 test: Option<&'static str>| {
                let len = len.max(0.0);
                if len <= 0.0 {
                    return;
                }
                let from = shape.value(0.0, len);
                let to = shape.value(len, len);
                let peak = shape.peak(len) * (1.0 + jitter);
                stages.push(Stage {
                    index: stages.len(),
                    kind,
                    start_secs: at,
                    end_secs: at + len,
                    from,
                    to,
                    peak,
                    shape,
                    jitter,
                    pattern,
                    aligned,
                    test,
                });
                at += len;
            };
            macro_rules! push {
                ($kind:expr, $len:expr, $shape:expr $(,)?) => {
                    push_with($kind, $len, $shape, false, None)
                };
            }
            match p {
                Pattern::Constant { rate, hold, .. } => push!(
                    "constant",
                    hold.0.as_secs_f64(),
                    Shape::Line {
                        from: *rate,
                        to: *rate,
                    },
                ),
                Pattern::Ramp {
                    from,
                    to,
                    over,
                    curve,
                    ..
                } => push!(
                    "ramp",
                    over.0.as_secs_f64(),
                    match curve {
                        Curve::Linear => Shape::Line {
                            from: *from,
                            to: *to,
                        },
                        Curve::Exponential => Shape::Exp {
                            from: *from,
                            to: *to,
                        },
                    },
                ),
                Pattern::Step {
                    start,
                    step,
                    every,
                    max,
                    hold,
                    ..
                } => {
                    let plateaus = step_plateaus(*start, *step, *max);
                    let every = every.0.as_secs_f64();
                    push!(
                        "step",
                        plateaus as f64 * every + hold.map_or(0.0, |h| h.0.as_secs_f64()),
                        Shape::Steps {
                            start: *start,
                            step: *step,
                            every_secs: every,
                            plateaus,
                        },
                    );
                }
                Pattern::Target {
                    rate,
                    users,
                    overhead_p99_ms,
                    within,
                    hold,
                    start,
                    max,
                    ..
                } => {
                    let within = within.0.as_secs_f64();
                    let hold = hold.0.as_secs_f64();
                    match (rate.or(*users), overhead_p99_ms) {
                        (Some(goal), _) => {
                            push!(
                                "target",
                                within,
                                Shape::Line {
                                    from: start.unwrap_or(0.0),
                                    to: goal,
                                },
                            );
                            push!(
                                "target",
                                hold,
                                Shape::Line {
                                    from: goal,
                                    to: goal,
                                },
                            );
                        }
                        (None, _) => push!(
                            "target",
                            within + hold,
                            Shape::Closed {
                                initial: start.unwrap_or(DEFAULT_LOOP_START),
                                max: max.unwrap_or(0.0),
                            },
                        ),
                    }
                }
                Pattern::Breakpoint {
                    start,
                    growth,
                    every,
                    max,
                    ..
                } => {
                    let plateaus = growth_plateaus(*start, *growth, *max);
                    let every = every.0.as_secs_f64();
                    push!(
                        "breakpoint",
                        plateaus as f64 * every,
                        Shape::Growth {
                            start: *start,
                            growth: *growth,
                            every_secs: every,
                            max: *max,
                            plateaus,
                        },
                    );
                }
                Pattern::Spike {
                    base,
                    peak,
                    rise,
                    hold,
                    fall,
                    recover,
                    ..
                } => {
                    push!(
                        "spike",
                        rise.0.as_secs_f64(),
                        Shape::Line {
                            from: *base,
                            to: *peak,
                        },
                    );
                    push!(
                        "spike",
                        hold.0.as_secs_f64(),
                        Shape::Line {
                            from: *peak,
                            to: *peak,
                        },
                    );
                    push!(
                        "spike",
                        fall.0.as_secs_f64(),
                        Shape::Line {
                            from: *peak,
                            to: *base,
                        },
                    );
                    push!(
                        "spike",
                        recover.0.as_secs_f64(),
                        Shape::Line {
                            from: *base,
                            to: *base,
                        },
                    );
                }
                Pattern::Pulse {
                    high,
                    low,
                    on,
                    off,
                    cycles,
                    ..
                } => {
                    let (on, off) = (on.0.as_secs_f64(), off.0.as_secs_f64());
                    push!(
                        "pulse",
                        f64::from(cycles.unwrap_or(1)) * (on + off),
                        Shape::Square {
                            high: *high,
                            low: *low,
                            on_secs: on,
                            off_secs: off,
                        },
                    );
                }
                Pattern::Burst {
                    base,
                    size,
                    within,
                    every,
                    hold,
                    ..
                } => push!(
                    "burst",
                    hold.0.as_secs_f64(),
                    Shape::Burst {
                        base: *base,
                        size: f64::from(*size),
                        within_secs: within.0.as_secs_f64(),
                        every_secs: every.0.as_secs_f64(),
                    },
                ),
                Pattern::Wave {
                    min,
                    max,
                    period,
                    shape,
                    hold,
                    ..
                } => push!(
                    "wave",
                    hold.0.as_secs_f64(),
                    Shape::Wave {
                        min: *min,
                        max: *max,
                        period_secs: period.0.as_secs_f64(),
                        form: *shape,
                    },
                ),
                Pattern::LimitTest(t) => {
                    for piece in t.pieces() {
                        push_with(
                            "limit_test",
                            piece.len,
                            piece.shape,
                            piece.aligned,
                            Some(piece.test),
                        );
                    }
                }
            }
        }
        Compiled {
            total_secs: stages.last().map_or(0.0, |s| s.end_secs),
            stages,
            sync: self.sync(),
        }
    }

    /// The chain flattened into stages with absolute offsets.
    pub fn stages(&self) -> Vec<Stage> {
        self.compile().stages
    }

    /// Number of patterns in the chain (unbounded chains are counted up to
    /// one over the limit, so validation can refuse them).
    pub fn depth(&self) -> usize {
        self.chain().len()
    }

    /// Total duration of the chain.
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.compile().total_secs.max(0.0))
    }

    /// The nominal value at `t` since the layer started, `None` once the
    /// pattern is over.
    pub fn value_at(&self, t: Duration) -> Option<f64> {
        self.compile().value_at(t.as_secs_f64(), &Ctx::default())
    }

    /// The highest value the pattern reaches (with its jitter).
    pub fn peak(&self) -> f64 {
        self.compile()
            .stages
            .iter()
            .map(|s| s.peak)
            .fold(0.0, f64::max)
    }

    /// Whether a stage is decided by a closed loop (`target` on overhead,
    /// `limit_probe`).
    pub fn has_closed_loop(&self) -> bool {
        self.chain().iter().any(|p| {
            matches!(
                p,
                Pattern::Target {
                    overhead_p99_ms: Some(_),
                    rate: None,
                    users: None,
                    ..
                }
            ) || matches!(p, Pattern::LimitTest(t) if t.test == super::limit_test::TestKind::LimitProbe)
        })
    }

    /// The rate-limit test of each pattern of the chain (`None` for other
    /// patterns), indexed like [`Stage::pattern`].
    pub fn limit_tests(&self) -> Vec<Option<&LimitTest>> {
        self.chain()
            .into_iter()
            .take(MAX_STAGES)
            .map(|p| match p {
                Pattern::LimitTest(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    /// Expected number of requests between two offsets (the integral of
    /// the rate), exact for every shape.
    pub fn expected_between(&self, from: Duration, to: Duration) -> f64 {
        self.compile()
            .integral(from.as_secs_f64(), to.as_secs_f64())
    }

    /// Samples of the curve every `step` (plus the end point).
    pub fn preview(&self, step: Duration) -> Vec<PreviewPoint> {
        let c = self.compile();
        let total = c.total_secs;
        let step = step.max(Duration::from_millis(100)).as_secs_f64();
        let mut out = Vec::new();
        let mut t = 0.0;
        while t < total && out.len() < 100_000 {
            out.push(PreviewPoint {
                t_secs: t,
                value: c.value_at(t, &Ctx::default()).unwrap_or(0.0),
            });
            t += step;
        }
        if let Some(last) = c.stages.last() {
            out.push(PreviewPoint {
                t_secs: total,
                value: last.to,
            });
        }
        out
    }

    /// Every problem with the pattern, prefixed with `path` (`layers[0].pattern`).
    pub fn validate(&self, path: &str, problems: &mut Vec<String>) {
        if self.depth() > MAX_STAGES {
            problems.push(format!(
                "{path}: a pattern chain may have at most {MAX_STAGES} stages (then:)"
            ));
        }
        for (i, p) in self.chain().into_iter().take(MAX_STAGES).enumerate() {
            let here = if i == 0 {
                path.to_string()
            } else {
                format!("{path}{}", ".then".repeat(i))
            };
            let value = |name: &str, v: f64, problems: &mut Vec<String>| {
                if !v.is_finite() || v < 0.0 {
                    problems.push(format!("{here}.{name}: must be a number of 0 or more"));
                } else if v > MAX_VALUE {
                    problems.push(format!(
                        "{here}.{name}: {v} is over the maximum {MAX_VALUE}"
                    ));
                }
            };
            let positive = |name: &str, d: &Dur, problems: &mut Vec<String>| {
                if d.0.is_zero() {
                    problems.push(format!("{here}.{name}: must be longer than 0"));
                }
            };
            let jitter = p.jitter();
            if !(0.0..=1.0).contains(&jitter) {
                problems.push(format!("{here}.jitter: 0 to 1 (a fraction of the value)"));
            }
            match p {
                Pattern::Constant { rate, hold, .. } => {
                    value("rate", *rate, problems);
                    positive("for", hold, problems);
                }
                Pattern::Ramp {
                    from,
                    to,
                    over,
                    curve,
                    ..
                } => {
                    value("from", *from, problems);
                    value("to", *to, problems);
                    positive("over", over, problems);
                    if *curve == Curve::Exponential && (*from <= 0.0 || *to <= 0.0) {
                        problems.push(format!(
                            "{here}.curve: an exponential ramp needs from and to above 0"
                        ));
                    }
                }
                Pattern::Step {
                    start,
                    step,
                    every,
                    max,
                    ..
                } => {
                    value("start", *start, problems);
                    value("max", *max, problems);
                    if !step.is_finite() || *step <= 0.0 {
                        problems.push(format!("{here}.step: must be above 0"));
                    } else if *max >= *start
                        && ((max - start) / step).floor() + 1.0 > MAX_PLATEAUS as f64
                    {
                        problems.push(format!(
                            "{here}: at most {MAX_PLATEAUS} plateaus (raise step or lower max)"
                        ));
                    }
                    if max < start {
                        problems.push(format!("{here}.max: must be at least start"));
                    }
                    positive("every", every, problems);
                }
                Pattern::Target {
                    rate,
                    users,
                    overhead_p99_ms,
                    within,
                    hold,
                    start,
                    max,
                    ..
                } => {
                    let goals = [rate.is_some(), users.is_some(), overhead_p99_ms.is_some()]
                        .iter()
                        .filter(|g| **g)
                        .count();
                    if goals != 1 {
                        problems.push(format!(
                            "{here}: exactly one of rate, users or overhead_p99_ms"
                        ));
                    }
                    if let Some(v) = rate {
                        value("rate", *v, problems);
                    }
                    if let Some(v) = users {
                        value("users", *v, problems);
                    }
                    if let Some(v) = start {
                        value("start", *v, problems);
                    }
                    positive("hold", hold, problems);
                    if let Some(ms) = overhead_p99_ms {
                        if !ms.is_finite() || *ms <= 0.0 || *ms > 60_000.0 {
                            problems
                                .push(format!("{here}.overhead_p99_ms: above 0, at most 60000"));
                        }
                        positive("within", within, problems);
                        match max {
                            None => problems.push(format!(
                                "{here}.max: required with overhead_p99_ms (the highest rate the loop may reach)"
                            )),
                            Some(m) => {
                                value("max", *m, problems);
                                if start.unwrap_or(DEFAULT_LOOP_START) > *m {
                                    problems.push(format!("{here}.start: at most max"));
                                }
                            }
                        }
                    } else if max.is_some() {
                        problems.push(format!(
                            "{here}.max: only with overhead_p99_ms (the goal is the rate or users)"
                        ));
                    }
                }
                Pattern::Breakpoint {
                    start,
                    growth,
                    every,
                    until,
                    max,
                    ..
                } => {
                    if !start.is_finite() || *start <= 0.0 {
                        problems.push(format!("{here}.start: must be above 0"));
                    }
                    value("max", *max, problems);
                    if max < start {
                        problems.push(format!("{here}.max: must be at least start"));
                    }
                    if !growth.is_finite() || *growth <= 0.0 || *growth > 10.0 {
                        problems.push(format!(
                            "{here}.growth: above 0, at most 10 (0.1 grows by 10 percent per plateau)"
                        ));
                    } else if *start > 0.0
                        && *max >= *start
                        && growth_plateaus(*start, *growth, *max) >= MAX_PLATEAUS
                    {
                        problems.push(format!(
                            "{here}: at most {MAX_PLATEAUS} plateaus (raise growth or lower max)"
                        ));
                    }
                    positive("every", every, problems);
                    if until.is_empty() {
                        problems.push(format!(
                            "{here}.until: at least one of error_rate, latency_p99_ms, overhead_p99_ms"
                        ));
                    }
                    if until.error_rate.is_some_and(|r| !(0.0..=1.0).contains(&r)) {
                        problems.push(format!("{here}.until.error_rate: 0 to 1"));
                    }
                    for (name, v) in [
                        ("latency_p99_ms", until.latency_p99_ms),
                        ("overhead_p99_ms", until.overhead_p99_ms),
                    ] {
                        if v.is_some_and(|v| !v.is_finite() || v <= 0.0 || v > 600_000.0) {
                            problems.push(format!("{here}.until.{name}: above 0, at most 600000"));
                        }
                    }
                }
                Pattern::Spike {
                    base,
                    peak,
                    rise,
                    hold,
                    fall,
                    recover,
                    ..
                } => {
                    value("base", *base, problems);
                    value("peak", *peak, problems);
                    positive("hold", hold, problems);
                    let _ = (rise, fall, recover);
                }
                Pattern::Pulse {
                    high,
                    low,
                    on,
                    off,
                    cycles,
                    ..
                } => {
                    value("high", *high, problems);
                    value("low", *low, problems);
                    positive("on", on, problems);
                    if on.0 + off.0 < MIN_PERIOD {
                        problems.push(format!("{here}: on plus off must be at least 10ms"));
                    }
                    if cycles.is_some_and(|c| c == 0 || c > MAX_CYCLES) {
                        problems.push(format!("{here}.cycles: 1 to {MAX_CYCLES}"));
                    }
                }
                Pattern::Burst {
                    base,
                    size,
                    within,
                    every,
                    hold,
                    ..
                } => {
                    value("base", *base, problems);
                    if *size == 0 || *size > MAX_BURST {
                        problems.push(format!("{here}.size: 1 to {MAX_BURST} requests"));
                    }
                    positive("within", within, problems);
                    positive("for", hold, problems);
                    if every.0 < MIN_PERIOD {
                        problems.push(format!("{here}.every: at least 10ms"));
                    }
                    if within.0 >= every.0 {
                        problems.push(format!(
                            "{here}.within: must be shorter than every (the burst repeats every `every`)"
                        ));
                    }
                    if !within.0.is_zero()
                        && base + f64::from(*size) / within.0.as_secs_f64() > MAX_VALUE
                    {
                        problems.push(format!(
                            "{here}: size / within is a rate over the maximum {MAX_VALUE}"
                        ));
                    }
                }
                Pattern::Wave {
                    min,
                    max,
                    period,
                    hold,
                    ..
                } => {
                    value("min", *min, problems);
                    value("max", *max, problems);
                    if max < min {
                        problems.push(format!("{here}.max: must be at least min"));
                    }
                    if period.0 < MIN_PERIOD {
                        problems.push(format!("{here}.period: at least 10ms"));
                    }
                    positive("for", hold, problems);
                }
                Pattern::LimitTest(t) => t.validate(&here, problems),
            }
        }
    }
}

/// Where a `target` on overhead starts when `start` is not given.
pub const DEFAULT_LOOP_START: f64 = 10.0;

/// Plateaus of a `step`: `start`, `start + step`, ... up to `max`.
fn step_plateaus(start: f64, step: f64, max: f64) -> u64 {
    if !(step.is_finite() && step > 0.0) || max < start {
        return 1;
    }
    (((max - start) / step).floor() as u64 + 1).clamp(1, MAX_PLATEAUS)
}

/// Plateaus of a `breakpoint`: until `start * (1 + growth)^k` reaches `max`.
fn growth_plateaus(start: f64, growth: f64, max: f64) -> u64 {
    if !(growth.is_finite() && growth > 0.0 && start > 0.0) || max <= start {
        return 1;
    }
    ((max / start).ln() / (1.0 + growth).ln()).ceil() as u64 + 1
}

/// One point of a pattern preview.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PreviewPoint {
    pub t_secs: f64,
    pub value: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: f64) -> Dur {
        Dur(Duration::from_secs_f64(s))
    }

    fn ramp_then_hold() -> Pattern {
        Pattern::Ramp {
            from: 10.0,
            to: 110.0,
            over: secs(10.0),
            curve: Curve::Linear,
            jitter: None,
            sync: None,
            then: Some(Box::new(Pattern::Constant {
                rate: 110.0,
                hold: secs(5.0),
                jitter: None,
                sync: None,
                then: None,
            })),
        }
    }

    fn parse(yaml: &str) -> Pattern {
        serde_yaml::from_str(yaml).unwrap()
    }

    /// The integral against a fine midpoint sum of the value.
    fn check_integral(p: &Pattern) {
        let c = p.compile();
        let total = c.total_secs;
        let mut t = 0.0;
        while t < total {
            let b = (t + 0.37).min(total);
            let exact = c.integral(t, b);
            let n = 20_000;
            let w = (b - t) / n as f64;
            let sum: f64 = (0..n)
                .map(|i| {
                    c.value_at(t + w * (i as f64 + 0.5), &Ctx::default())
                        .unwrap_or(0.0)
                        * w
                })
                .sum();
            assert!(
                (exact - sum).abs() <= 0.002 * sum.max(1.0) + 0.05,
                "{} [{t}, {b}]: {exact} vs {sum}",
                p.kind()
            );
            t = b;
        }
    }

    #[test]
    fn stages_and_values() {
        let p = ramp_then_hold();
        let stages = p.stages();
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[1].start_secs, 10.0);
        assert_eq!(stages[1].end_secs, 15.0);
        assert_eq!(p.duration(), Duration::from_secs(15));
        assert_eq!(p.value_at(Duration::ZERO), Some(10.0));
        assert_eq!(p.value_at(Duration::from_secs(5)), Some(60.0));
        assert_eq!(p.value_at(Duration::from_secs(12)), Some(110.0));
        assert_eq!(p.value_at(Duration::from_secs(15)), None);
        assert_eq!(p.peak(), 110.0);
        assert_eq!(p.depth(), 2);
    }

    #[test]
    fn expected_requests_match_the_math() {
        let p = ramp_then_hold();
        // Trapezoid 10..110 over 10 s = 600, plus 110 * 5 = 550.
        let total = p.expected_between(Duration::ZERO, Duration::from_secs(15));
        assert!((total - 1150.0).abs() < 1e-6, "{total}");
        let first = p.expected_between(Duration::ZERO, Duration::from_secs(1));
        assert!((first - 15.0).abs() < 1e-9, "{first}");
    }

    #[test]
    fn exponential_ramp() {
        let p = parse("type: ramp\nfrom: 10\nto: 1000\nover: 2s\ncurve: exponential");
        let mid = p.value_at(Duration::from_secs(1)).unwrap();
        assert!((mid - 100.0).abs() < 0.01, "{mid}");
        check_integral(&p);
    }

    #[test]
    fn step_plateaus_and_hold() {
        let p = parse("type: step\nstart: 10\nstep: 20\nevery: 2s\nmax: 55\nhold: 3s");
        // Plateaus 10, 30, 50 (70 would pass max), then held 3 s.
        let c = p.compile();
        assert_eq!(c.total_secs, 9.0);
        let at = |t: f64| c.value_at(t, &Ctx::default()).unwrap();
        assert_eq!(
            (at(0.5), at(2.5), at(4.5), at(8.0)),
            (10.0, 30.0, 50.0, 50.0)
        );
        assert_eq!(p.peak(), 50.0);
        assert_eq!(c.position(4.5), Some((0, 2)));
        assert_eq!(c.stages[0].segment_value(1), 30.0);
        assert!((c.integral(0.0, 9.0) - (20.0 + 60.0 + 100.0 + 150.0)).abs() < 1e-9);
        check_integral(&p);
    }

    #[test]
    fn spike_pulse_burst_and_wave() {
        let spike =
            parse("type: spike\nbase: 10\npeak: 100\nrise: 1s\nhold: 2s\nfall: 1s\nrecover: 3s");
        assert_eq!(spike.duration(), Duration::from_secs(7));
        assert_eq!(spike.peak(), 100.0);
        assert_eq!(spike.value_at(Duration::from_millis(500)), Some(55.0));
        assert_eq!(spike.value_at(Duration::from_secs(6)), Some(10.0));
        check_integral(&spike);

        let pulse = parse("type: pulse\nhigh: 100\nlow: 10\non: 1s\noff: 3s\ncycles: 2");
        assert_eq!(pulse.duration(), Duration::from_secs(8));
        let c = pulse.compile();
        assert_eq!(c.integral(0.0, 8.0), 2.0 * (100.0 + 30.0));
        assert_eq!(c.position(4.5), Some((0, 2)));
        assert_eq!(c.position(5.5), Some((0, 3)));
        check_integral(&pulse);

        // 100 requests within 50 ms, every second, on 20/s.
        let burst = parse("type: burst\nbase: 20\nsize: 100\nwithin: 50ms\nevery: 1s\nfor: 3s");
        let c = burst.compile();
        assert!((c.integral(0.0, 1.0) - 120.0).abs() < 1e-9);
        assert!((c.integral(0.0, 0.05) - 101.0).abs() < 1e-9);
        assert!((burst.peak() - 2020.0).abs() < 1e-9);
        check_integral(&burst);

        for shape in ["sine", "triangle", "sawtooth"] {
            let wave = parse(&format!(
                "type: wave\nmin: 10\nmax: 110\nperiod: 4s\nshape: {shape}\nfor: 10s"
            ));
            let c = wave.compile();
            assert_eq!(c.value_at(0.0, &Ctx::default()), Some(10.0));
            // Every shape averages halfway over a whole period.
            assert!((c.integral(0.0, 8.0) - 8.0 * 60.0).abs() < 1e-6, "{shape}");
            check_integral(&wave);
        }
    }

    #[test]
    fn target_and_breakpoint() {
        let t = parse("type: target\nrate: 100\nwithin: 2s\nhold: 3s");
        assert_eq!(t.duration(), Duration::from_secs(5));
        assert_eq!(t.value_at(Duration::from_secs(1)), Some(50.0));
        assert_eq!(t.value_at(Duration::from_secs(3)), Some(100.0));
        assert!(!t.has_closed_loop());
        let o = parse("type: target\noverhead_p99_ms: 5\nwithin: 10s\nhold: 5s\nmax: 500");
        assert!(o.has_closed_loop());
        assert!(o.compile().stages[0].is_closed());
        assert_eq!(o.peak(), 500.0);
        assert_eq!(o.value_at(Duration::ZERO), Some(DEFAULT_LOOP_START));

        let b = parse(
            "type: breakpoint\nstart: 100\ngrowth: 0.5\nevery: 2s\nmax: 400\nuntil: { error_rate: 0.01 }",
        );
        // 100, 150, 225, 337.5, then 400 (capped): 5 plateaus.
        let c = b.compile();
        assert_eq!(c.total_secs, 10.0);
        let at = |t: f64| c.value_at(t, &Ctx::default()).unwrap();
        assert_eq!(
            (at(1.0), at(3.0), at(5.0), at(9.0)),
            (100.0, 150.0, 225.0, 400.0)
        );
        assert_eq!(b.peak(), 400.0);
        check_integral(&b);
    }

    #[test]
    fn jitter_and_sync() {
        let p = parse("type: constant\nrate: 100\nfor: 20s\njitter: 0.2");
        let c = p.compile();
        assert!((p.peak() - 120.0).abs() < 1e-9);
        let a = Ctx {
            wall: 0.0,
            seed: 1,
            phase: 0.0,
        };
        let b = Ctx {
            wall: 0.0,
            seed: 2,
            phase: 0.0,
        };
        let va: Vec<f64> = (0..20)
            .map(|t| c.value_at(t as f64 + 0.5, &a).unwrap())
            .collect();
        // Within the jitter, the same within a second, varied across seconds,
        // the same for the same seed (every node with sync: cluster).
        assert!(va.iter().all(|v| (80.0..=120.0).contains(v)));
        assert_eq!(c.value_at(3.1, &a), c.value_at(3.9, &a));
        assert!(va.windows(2).any(|w| w[0] != w[1]));
        assert_eq!(c.value_at(3.5, &a), c.value_at(3.5, &a));
        assert_ne!(c.value_at(3.5, &a), c.value_at(3.5, &b));
        // sync: node shifts the phase of periodic stages.
        let pulse = parse("type: pulse\nhigh: 100\nlow: 0\non: 1s\noff: 1s\ncycles: 3\nsync: node");
        assert_eq!(pulse.sync(), SyncMode::Node);
        let c = pulse.compile();
        let shifted = Ctx {
            wall: 0.0,
            seed: 0,
            phase: 0.5,
        };
        assert_eq!(c.value_at(0.25, &Ctx::default()), Some(100.0));
        assert_eq!(c.value_at(0.75, &shifted), Some(0.0));
        assert_eq!(seed_of(&["run", "layer"]), seed_of(&["run", "layer"]));
        assert_ne!(seed_of(&["run", "a"]), seed_of(&["run", "b"]));
    }

    #[test]
    fn skip_and_positions() {
        let p = ramp_then_hold();
        let c = p.compile();
        assert_eq!(c.next_stage_start(3.0), 10.0);
        assert_eq!(c.next_stage_start(12.0), 15.0);
        assert_eq!(c.next_stage_start(20.0), 15.0);
        let mut q = p.clone();
        if let Some(Pattern::Constant { rate, .. }) = q.nth_mut(1) {
            *rate = 200.0;
        }
        assert_eq!(q.peak(), 200.0);
        assert!(q.nth_mut(2).is_none());
    }

    #[test]
    fn preview_samples_the_curve() {
        let p = ramp_then_hold();
        let pts = p.preview(Duration::from_secs(5));
        let values: Vec<f64> = pts.iter().map(|x| x.value).collect();
        assert_eq!(values, vec![10.0, 60.0, 110.0, 110.0]);
        assert_eq!(pts.last().unwrap().t_secs, 15.0);
    }

    #[test]
    fn validation_lists_every_problem() {
        let p = Pattern::Ramp {
            from: -1.0,
            to: 0.0,
            over: secs(0.0),
            curve: Curve::Exponential,
            jitter: Some(2.0),
            sync: None,
            then: Some(Box::new(Pattern::Constant {
                rate: f64::NAN,
                hold: secs(0.0),
                jitter: None,
                sync: None,
                then: None,
            })),
        };
        let mut problems = Vec::new();
        p.validate("pattern", &mut problems);
        assert_eq!(problems.len(), 6, "{problems:#?}");
        assert!(problems.iter().any(|p| p.starts_with("pattern.then.rate")));
        assert!(problems.iter().any(|p| p.starts_with("pattern.jitter")));
        for (yaml, want) in [
            ("type: step\nstart: 10\nstep: 0\nevery: 1s\nmax: 5", 2),
            ("type: target\nrate: 1\nusers: 2\nwithin: 1s\nhold: 0s", 2),
            ("type: target\noverhead_p99_ms: 5\nwithin: 1s\nhold: 1s", 1),
            (
                "type: breakpoint\nstart: 0\ngrowth: 0\nevery: 0s\nmax: 1\nuntil: {}",
                4,
            ),
            (
                "type: burst\nbase: 1\nsize: 0\nwithin: 2s\nevery: 1s\nfor: 1s",
                2,
            ),
            (
                "type: pulse\nhigh: 1\nlow: 0\non: 0s\noff: 0s\ncycles: 0",
                3,
            ),
            ("type: wave\nmin: 5\nmax: 1\nperiod: 1ms\nfor: 1s", 2),
        ] {
            let mut problems = Vec::new();
            parse(yaml).validate("p", &mut problems);
            assert_eq!(problems.len(), want, "{yaml}: {problems:#?}");
        }
    }

    #[test]
    fn limit_tests_round_trip_and_follow_the_math() {
        let p = parse("type: limit_test\ntest: window_edge\nlimit: 10\nwindow: 1s\ncycles: 2\nthen: { type: constant, rate: 1, for: 1s }");
        let back: Pattern = serde_yaml::from_str(&serde_yaml::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
        check_integral(&p);
        let c = p.compile();
        assert_eq!(c.stages[0].test, Some("window_edge"));
        assert!(c.stages[0].aligned);
        // Two bursts of 10 around every other boundary.
        let edge = &c.stages[0];
        assert!(
            (c.integral(0.0, 4.0) - 40.0).abs() < 1e-6,
            "{}",
            c.integral(0.0, 4.0)
        );
        // On the wall clock: a run that starts 0.5 s into a two-window
        // period has its next even boundary 1.5 s in.
        let ctx = Ctx {
            wall: 1_000.5,
            ..Ctx::default()
        };
        assert!((edge.shift(&ctx) - 0.5).abs() < 1e-9);
        assert_eq!(c.value_at(1.53, &ctx), Some(100.0));
        assert_eq!(c.value_at(1.40, &ctx), Some(100.0));
        assert_eq!(c.value_at(0.53, &ctx), Some(0.0));
        assert_eq!(c.value_at(1.30, &ctx), Some(0.0));
        // The scheduler never steps over an edge of the bursts.
        assert!((c.next_boundary(0.2, &ctx) - 1.375).abs() < 1e-9);
        let err = serde_yaml::from_str::<Pattern>("type: limit_test\ntest: nope").unwrap_err();
        assert!(err.to_string().contains("over_limit"), "{err}");
    }

    #[test]
    fn serde_round_trip_with_tag() {
        let p = ramp_then_hold();
        let text = serde_yaml::to_string(&p).unwrap();
        assert!(text.contains("type: ramp"));
        let back: Pattern = serde_yaml::from_str(&text).unwrap();
        assert_eq!(back, p);
        let err = serde_yaml::from_str::<Pattern>("type: replay\nbase: 1").unwrap_err();
        assert!(err.to_string().contains("unknown variant"), "{err}");
        assert_eq!(Pattern::kinds().len(), 10);
        for kind in Pattern::kinds() {
            assert!(
                err.to_string().contains(kind),
                "{kind} is not a known pattern type: {err}"
            );
        }
    }
}
