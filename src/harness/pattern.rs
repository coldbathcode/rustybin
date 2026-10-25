//! Run patterns: the shape of a layer's load over time (`docs/plans/harness.md`,
//! section 6.2). A pattern is a function of time giving the target rate (open
//! model) or the number of active users (closed model). Patterns chain with
//! `then:` and the chain is flattened into [`Stage`]s.
//!
//! Phase 1 ships `constant` and `ramp`; the other patterns of the plan
//! (step, target, breakpoint, spike, pulse, burst, wave, burst attacks)
//! arrive with phases 4 and 5 and will extend this enum, the schema in
//! `scenario.rs` and the console builder in the same change.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::scenario::Dur;

/// Most stages in one `then:` chain.
pub const MAX_STAGES: usize = 16;
/// Highest rate or user count a pattern may name (the caps cut lower).
pub const MAX_VALUE: f64 = 10_000_000.0;

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

/// The shape of a layer's load over time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Pattern {
    /// A steady rate (or user count) for a while: baselines and soaks.
    Constant {
        rate: f64,
        #[serde(rename = "for")]
        hold: Dur,
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
        then: Option<Box<Pattern>>,
    },
}

/// One flattened stage of a pattern chain.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Stage {
    pub index: usize,
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Offset from the layer's start.
    pub start_secs: f64,
    pub end_secs: f64,
    pub from: f64,
    pub to: f64,
    pub curve: &'static str,
}

impl Stage {
    fn value_at(&self, t: f64) -> f64 {
        let len = self.end_secs - self.start_secs;
        if len <= 0.0 {
            return self.to;
        }
        let frac = ((t - self.start_secs) / len).clamp(0.0, 1.0);
        match self.curve {
            "exponential" if self.from > 0.0 && self.to > 0.0 => {
                self.from * (self.to / self.from).powf(frac)
            }
            _ => self.from + (self.to - self.from) * frac,
        }
    }
}

impl Pattern {
    fn next(&self) -> Option<&Pattern> {
        match self {
            Pattern::Constant { then, .. } | Pattern::Ramp { then, .. } => then.as_deref(),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Pattern::Constant { .. } => "constant",
            Pattern::Ramp { .. } => "ramp",
        }
    }

    /// The chain flattened into stages with absolute offsets.
    pub fn stages(&self) -> Vec<Stage> {
        let mut out = Vec::new();
        let mut at = 0.0;
        let mut current = Some(self);
        while let Some(p) = current {
            if out.len() >= MAX_STAGES {
                break;
            }
            let (kind, len, from, to, curve) = match p {
                Pattern::Constant { rate, hold, .. } => {
                    ("constant", hold.0.as_secs_f64(), *rate, *rate, "linear")
                }
                Pattern::Ramp {
                    from,
                    to,
                    over,
                    curve,
                    ..
                } => ("ramp", over.0.as_secs_f64(), *from, *to, curve.as_str()),
            };
            out.push(Stage {
                index: out.len(),
                kind,
                start_secs: at,
                end_secs: at + len,
                from,
                to,
                curve,
            });
            at += len;
            current = p.next();
        }
        out
    }

    /// Number of stages in the chain (unbounded chains are counted up to
    /// one over the limit, so validation can refuse them).
    pub fn depth(&self) -> usize {
        let mut n = 0;
        let mut current = Some(self);
        while let Some(p) = current {
            n += 1;
            if n > MAX_STAGES {
                break;
            }
            current = p.next();
        }
        n
    }

    /// Total duration of the chain.
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.stages().last().map_or(0.0, |s| s.end_secs).max(0.0))
    }

    /// The target value at `t` since the layer started, `None` once the
    /// pattern is over.
    pub fn value_at(&self, t: Duration) -> Option<f64> {
        let secs = t.as_secs_f64();
        self.stages()
            .iter()
            .find(|s| secs >= s.start_secs && secs < s.end_secs)
            .map(|s| s.value_at(secs))
    }

    /// The highest value the pattern reaches.
    pub fn peak(&self) -> f64 {
        self.stages()
            .iter()
            .map(|s| s.from.max(s.to))
            .fold(0.0, f64::max)
    }

    /// Expected number of requests between two offsets (the integral of the
    /// rate), by midpoint sums: exact for constants, within a fraction of a
    /// percent for ramps.
    pub fn expected_between(&self, from: Duration, to: Duration) -> f64 {
        let (a, b) = (from.as_secs_f64(), to.as_secs_f64());
        if b <= a {
            return 0.0;
        }
        let stages = self.stages();
        let mut total = 0.0;
        for s in &stages {
            let lo = a.max(s.start_secs);
            let hi = b.min(s.end_secs);
            if hi <= lo {
                continue;
            }
            if s.kind == "constant" || s.from == s.to {
                total += s.from * (hi - lo);
                continue;
            }
            let steps = 64;
            let width = (hi - lo) / steps as f64;
            for i in 0..steps {
                let mid = lo + width * (i as f64 + 0.5);
                total += s.value_at(mid) * width;
            }
        }
        total
    }

    /// Samples of the curve every `step` (plus the end point), for the
    /// console's preview chart and the pattern tests.
    pub fn preview(&self, step: Duration) -> Vec<PreviewPoint> {
        let total = self.duration();
        let step = step.max(Duration::from_millis(100));
        let mut out = Vec::new();
        let mut t = Duration::ZERO;
        while t < total && out.len() < 100_000 {
            out.push(PreviewPoint {
                t_secs: t.as_secs_f64(),
                value: self.value_at(t).unwrap_or(0.0),
            });
            t += step;
        }
        if let Some(last) = self.stages().last() {
            out.push(PreviewPoint {
                t_secs: total.as_secs_f64(),
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
        let mut current = Some(self);
        let mut i = 0;
        while let Some(p) = current {
            if i > MAX_STAGES {
                break;
            }
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
            match p {
                Pattern::Constant { rate, hold, .. } => {
                    value("rate", *rate, problems);
                    if hold.0.is_zero() {
                        problems.push(format!("{here}.for: must be longer than 0"));
                    }
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
                    if over.0.is_zero() {
                        problems.push(format!("{here}.over: must be longer than 0"));
                    }
                    if *curve == Curve::Exponential && (*from <= 0.0 || *to <= 0.0) {
                        problems.push(format!(
                            "{here}.curve: an exponential ramp needs from and to above 0"
                        ));
                    }
                }
            }
            current = p.next();
            i += 1;
        }
    }
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
            then: Some(Box::new(Pattern::Constant {
                rate: 110.0,
                hold: secs(5.0),
                then: None,
            })),
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
        assert!((total - 1150.0).abs() < 1.0, "{total}");
        let first = p.expected_between(Duration::ZERO, Duration::from_secs(1));
        assert!((first - 15.0).abs() < 0.1, "{first}");
    }

    #[test]
    fn exponential_ramp() {
        let p = Pattern::Ramp {
            from: 10.0,
            to: 1000.0,
            over: secs(2.0),
            curve: Curve::Exponential,
            then: None,
        };
        let mid = p.value_at(Duration::from_secs(1)).unwrap();
        assert!((mid - 100.0).abs() < 0.01, "{mid}");
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
            then: Some(Box::new(Pattern::Constant {
                rate: f64::NAN,
                hold: secs(0.0),
                then: None,
            })),
        };
        let mut problems = Vec::new();
        p.validate("pattern", &mut problems);
        assert_eq!(problems.len(), 5, "{problems:#?}");
        assert!(problems.iter().any(|p| p.starts_with("pattern.then.rate")));
    }

    #[test]
    fn serde_round_trip_with_tag() {
        let p = ramp_then_hold();
        let text = serde_yaml::to_string(&p).unwrap();
        assert!(text.contains("type: ramp"));
        let back: Pattern = serde_yaml::from_str(&text).unwrap();
        assert_eq!(back, p);
        let err = serde_yaml::from_str::<Pattern>("type: spike\nbase: 1").unwrap_err();
        assert!(err.to_string().contains("unknown variant"), "{err}");
    }
}
