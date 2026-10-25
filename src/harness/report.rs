//! The final report of a run (`docs/plans/harness.md`, section 12): totals,
//! latency percentiles, per-layer and per-target breakdowns, the per-second
//! timeline, saturation, thresholds and samples, as JSON (the API, the report
//! directory), text (the CLI) and JUnit (CI).
//!
//! Phase 1 reports what the client saw: expected and unexpected statuses and
//! transport failures. Phase 2 adds the correlation classes (`upstream`,
//! `gateway`, `lost`) and the gateway overhead.

use std::collections::BTreeMap;

use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::scenario::{Caps, Thresholds};

/// Most samples kept per run.
pub const MAX_SAMPLES: usize = 1_000;
/// Scheduler lag p99 over which a one-second bucket counts as saturated.
pub const LAG_SATURATION_MS: f64 = 5.0;

/// Latency percentiles in milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Percentiles {
    pub count: u64,
    pub min_ms: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub p99_ms: f64,
    pub p999_ms: f64,
    pub max_ms: f64,
}

fn ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

impl Percentiles {
    /// From a histogram of microseconds.
    pub fn from_histogram(h: &Histogram<u64>) -> Self {
        if h.is_empty() {
            return Self::default();
        }
        Self {
            count: h.len(),
            min_ms: ms(h.min()),
            mean_ms: h.mean() / 1000.0,
            p50_ms: ms(h.value_at_quantile(0.5)),
            p90_ms: ms(h.value_at_quantile(0.9)),
            p99_ms: ms(h.value_at_quantile(0.99)),
            p999_ms: ms(h.value_at_quantile(0.999)),
            max_ms: ms(h.max()),
        }
    }
}

/// Request counts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Counts {
    /// Requests the patterns asked for (sent plus not sent).
    pub requested: u64,
    pub sent: u64,
    /// Skipped at the in-flight cap (client saturation, never a gateway result).
    pub not_sent: u64,
    /// Answered or failed.
    pub completed: u64,
    /// Answered with an expected status.
    pub ok: u64,
    /// Answered with a status the template did not expect.
    pub unexpected_status: u64,
    /// Connect, TLS, timeout, reset and protocol failures.
    pub transport: u64,
    pub bytes_received: u64,
    /// Answers by status code.
    #[serde(default)]
    pub statuses: BTreeMap<String, u64>,
    /// Transport failures by kind.
    #[serde(default)]
    pub transport_kinds: BTreeMap<String, u64>,
}

impl Counts {
    pub fn errors(&self) -> u64 {
        self.unexpected_status + self.transport
    }

    /// Errors as a fraction of the requests sent.
    pub fn error_rate(&self) -> f64 {
        if self.sent == 0 {
            0.0
        } else {
            self.errors() as f64 / self.sent as f64
        }
    }

    pub fn add(&mut self, other: &Counts) {
        self.requested += other.requested;
        self.sent += other.sent;
        self.not_sent += other.not_sent;
        self.completed += other.completed;
        self.ok += other.ok;
        self.unexpected_status += other.unexpected_status;
        self.transport += other.transport;
        self.bytes_received += other.bytes_received;
        for (k, v) in &other.statuses {
            *self.statuses.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &other.transport_kinds {
            *self.transport_kinds.entry(k.clone()).or_default() += v;
        }
    }
}

/// Counts and latency for one layer or one target.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Breakdown {
    pub name: String,
    pub counts: Counts,
    pub latency: Percentiles,
}

/// One bucket of the timeline.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bucket {
    /// Offset of the bucket's start from the run's start.
    pub t_secs: f64,
    pub width_secs: f64,
    pub requested: u64,
    pub sent: u64,
    pub not_sent: u64,
    pub completed: u64,
    pub ok: u64,
    pub errors: u64,
    pub transport: u64,
    pub status_2xx: u64,
    pub status_3xx: u64,
    pub status_4xx: u64,
    pub status_5xx: u64,
    /// Answers per second, counted by completion time.
    pub rate: f64,
    /// From the intended send time.
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    /// From the actual send time.
    pub service_p99_ms: f64,
    pub lag_p99_ms: f64,
    pub lag_max_ms: f64,
    /// Requests in flight at the end of the bucket.
    pub in_flight: u64,
    pub saturated: bool,
}

impl Bucket {
    /// Fold a 1 s bucket into a wider one (10 s buckets after the first hour).
    pub fn absorb(&mut self, other: &Bucket) {
        self.width_secs = (other.t_secs + other.width_secs) - self.t_secs;
        self.requested += other.requested;
        self.sent += other.sent;
        self.not_sent += other.not_sent;
        self.completed += other.completed;
        self.ok += other.ok;
        self.errors += other.errors;
        self.transport += other.transport;
        self.status_2xx += other.status_2xx;
        self.status_3xx += other.status_3xx;
        self.status_4xx += other.status_4xx;
        self.status_5xx += other.status_5xx;
        self.rate = self.completed as f64 / self.width_secs.max(1e-9);
        self.p50_ms = self.p50_ms.max(other.p50_ms);
        self.p99_ms = self.p99_ms.max(other.p99_ms);
        self.max_ms = self.max_ms.max(other.max_ms);
        self.service_p99_ms = self.service_p99_ms.max(other.service_p99_ms);
        self.lag_p99_ms = self.lag_p99_ms.max(other.lag_p99_ms);
        self.lag_max_ms = self.lag_max_ms.max(other.lag_max_ms);
        self.in_flight = other.in_flight;
        self.saturated |= other.saturated;
    }
}

/// Why and when the client itself was the bottleneck.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Saturation {
    /// Two saturated buckets in a row happened.
    pub saturated: bool,
    pub reasons: Vec<String>,
    /// Start offsets of the saturated buckets (the first 100).
    pub buckets_secs: Vec<f64>,
    /// The in-flight cap in force (the configured cap or the descriptor limit).
    pub inflight_cap: usize,
    pub descriptor_limit: Option<u64>,
    pub threads: usize,
    /// Intended versus actual send time.
    pub scheduler_lag: Percentiles,
    /// Process CPU seconds used during the run (Linux), server side included
    /// in loop-through.
    pub cpu_seconds: Option<f64>,
}

/// One threshold, with what was measured.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThresholdResult {
    pub name: String,
    pub limit: Value,
    pub measured: Value,
    pub pass: bool,
}

/// A noteworthy request: one of the slowest, or an error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// `slowest` or `error`.
    pub kind: String,
    pub seq: u64,
    /// The `X-Request-Id` sent, to look up in gateway logs.
    pub request_id: String,
    pub layer: String,
    pub target: String,
    pub method: String,
    pub path: String,
    pub status: Option<u16>,
    pub error: Option<String>,
    /// From the intended send time.
    pub latency_ms: f64,
    pub service_ms: f64,
    pub at_secs: f64,
}

/// A target as the run saw it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TargetReport {
    pub url: String,
    pub weight: u32,
    pub http: String,
    /// The addresses the run was pinned to.
    pub pinned: Vec<String>,
    /// Served the verification token before the run.
    pub verified: bool,
}

/// The final report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub run_id: String,
    pub scenario_name: String,
    pub scenario_hash: String,
    pub node_id: String,
    pub rustybin_version: String,
    pub started_at: String,
    pub finished_at: String,
    /// From the start to the last answer.
    pub duration_secs: f64,
    /// What the patterns planned.
    pub planned_secs: f64,
    /// `finished`, `stopped` or `failed`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub caps: Caps,
    pub targets: Vec<TargetReport>,
    pub totals: Counts,
    /// From the intended send time (coordinated omission corrected).
    pub latency: Percentiles,
    /// From the actual send time.
    pub service_latency: Percentiles,
    /// Answers per second over the send window.
    pub achieved_rate: f64,
    pub requested_rate: f64,
    pub layers: Vec<Breakdown>,
    pub targets_breakdown: Vec<Breakdown>,
    pub timeline: Vec<Bucket>,
    pub saturation: Saturation,
    pub thresholds: Vec<ThresholdResult>,
    pub passed: bool,
    pub exit_code: u8,
    pub samples: Vec<Sample>,
    /// The normalised scenario (credential-looking header values redacted).
    pub scenario: Value,
}

/// Evaluate the thresholds against what was measured.
pub fn evaluate(
    th: &Thresholds,
    totals: &Counts,
    latency: &Percentiles,
    saturation: &Saturation,
) -> Vec<ThresholdResult> {
    let mut out = Vec::new();
    let mut push = |name: &str, limit: Value, measured: Value, pass: bool| {
        out.push(ThresholdResult {
            name: name.into(),
            limit,
            measured,
            pass,
        });
    };
    if let Some(limit) = th.error_rate {
        let m = totals.error_rate();
        push("error_rate", limit.into(), round(m).into(), m <= limit);
    }
    if let Some(limit) = th.not_sent {
        push(
            "not_sent",
            limit.into(),
            totals.not_sent.into(),
            totals.not_sent <= limit,
        );
    }
    for (name, limit, measured) in [
        ("latency_p50_ms", th.latency_p50_ms, latency.p50_ms),
        ("latency_p99_ms", th.latency_p99_ms, latency.p99_ms),
        ("latency_max_ms", th.latency_max_ms, latency.max_ms),
    ] {
        if let Some(limit) = limit {
            push(
                name,
                limit.into(),
                round(measured).into(),
                measured <= limit,
            );
        }
    }
    if let Some(limit) = th.min_achieved {
        let m = if totals.requested == 0 {
            1.0
        } else {
            totals.completed as f64 / totals.requested as f64
        };
        push("min_achieved", limit.into(), round(m).into(), m >= limit);
    }
    if let Some(allowed) = th.saturated {
        push(
            "saturated",
            allowed.into(),
            saturation.saturated.into(),
            allowed || !saturation.saturated,
        );
    }
    out
}

fn round(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

impl Report {
    /// 0 when the thresholds pass, 1 when they fail, 2 when the run failed
    /// or was stopped before it ended.
    pub fn exit_code_for(state: &str, thresholds: &[ThresholdResult]) -> u8 {
        if state != "finished" {
            2
        } else if thresholds.iter().all(|t| t.pass) {
            0
        } else {
            1
        }
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| format!("not a harness report: {e}"))
    }

    /// The text rendering (CLI, `?format=text`).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let t = &self.totals;
        out.push_str(&format!(
            "Harness run {} ({}), scenario {} [{}]\n",
            self.run_id,
            self.state,
            self.scenario_name,
            &self.scenario_hash[..self.scenario_hash.len().min(12)]
        ));
        out.push_str(&format!(
            "node {} | {} -> {} | {:.1} s of {:.1} s planned\n",
            self.node_id, self.started_at, self.finished_at, self.duration_secs, self.planned_secs
        ));
        if let Some(e) = &self.error {
            out.push_str(&format!("error: {e}\n"));
        }
        if let Some(by) = &self.stopped_by {
            out.push_str(&format!("stopped by: {by}\n"));
        }
        out.push_str("\nTargets\n");
        for target in &self.targets {
            out.push_str(&format!(
                "  {} (weight {}, {}) -> {}{}\n",
                target.url,
                target.weight,
                target.http,
                target.pinned.join(", "),
                if target.verified { ", verified" } else { "" }
            ));
        }
        out.push_str(&format!(
            "\nRequests: {} requested, {} sent, {} not sent, {} completed, {} ok, {} unexpected status, {} transport\n",
            t.requested, t.sent, t.not_sent, t.completed, t.ok, t.unexpected_status, t.transport
        ));
        out.push_str(&format!(
            "Rate: {:.1} answers/s achieved, {:.1}/s requested | error rate {:.2}% | {} bytes received\n",
            self.achieved_rate,
            self.requested_rate,
            t.error_rate() * 100.0,
            t.bytes_received
        ));
        if !t.statuses.is_empty() {
            let statuses: Vec<String> = t
                .statuses
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect();
            out.push_str(&format!("Statuses: {}\n", statuses.join(", ")));
        }
        if !t.transport_kinds.is_empty() {
            let kinds: Vec<String> = t
                .transport_kinds
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect();
            out.push_str(&format!("Transport failures: {}\n", kinds.join(", ")));
        }
        out.push_str("\nLatency (ms)       p50      p90      p99    p99.9      max     mean\n");
        for (name, p) in [
            ("intended send", &self.latency),
            ("actual send  ", &self.service_latency),
        ] {
            out.push_str(&format!(
                "  {name} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2}\n",
                p.p50_ms, p.p90_ms, p.p99_ms, p.p999_ms, p.max_ms, p.mean_ms
            ));
        }
        if self.layers.len() > 1 || self.targets_breakdown.len() > 1 {
            out.push_str("\nBreakdown            sent       ok   errors   p50 ms   p99 ms\n");
            for (kind, items) in [("layer", &self.layers), ("target", &self.targets_breakdown)] {
                for b in items {
                    out.push_str(&format!(
                        "  {kind} {:<12} {:>8} {:>8} {:>8} {:>8.2} {:>8.2}\n",
                        truncate(&b.name, 12),
                        b.counts.sent,
                        b.counts.ok,
                        b.counts.errors(),
                        b.latency.p50_ms,
                        b.latency.p99_ms
                    ));
                }
            }
        }
        let s = &self.saturation;
        out.push_str(&format!(
            "\nClient: {} | in-flight cap {} | {} threads | scheduler lag p99 {:.2} ms, max {:.2} ms{}\n",
            if s.saturated {
                "SATURATED (read the client, not the gateway)"
            } else {
                "not saturated"
            },
            s.inflight_cap,
            s.threads,
            s.scheduler_lag.p99_ms,
            s.scheduler_lag.max_ms,
            s.cpu_seconds
                .map(|c| format!(", {c:.2} CPU s"))
                .unwrap_or_default()
        ));
        for r in &s.reasons {
            out.push_str(&format!("  {r}\n"));
        }
        if self.thresholds.is_empty() {
            out.push_str("\nThresholds: none\n");
        } else {
            out.push_str("\nThresholds\n");
            for th in &self.thresholds {
                out.push_str(&format!(
                    "  {} {:<16} limit {} measured {}\n",
                    if th.pass { "PASS" } else { "FAIL" },
                    th.name,
                    th.limit,
                    th.measured
                ));
            }
        }
        out.push_str(&format!(
            "\nResult: {} (exit code {})\n",
            if self.passed { "PASS" } else { "FAIL" },
            self.exit_code
        ));
        out
    }

    /// A JUnit XML rendering: one test case per threshold (plus one for the
    /// run itself), so CI shows what failed.
    pub fn to_junit(&self) -> String {
        let esc = crate::content_negotiation::xml_escape;
        let mut cases = Vec::new();
        let run_ok = self.state == "finished";
        cases.push(format!(
            "    <testcase classname=\"rustybin.harness\" name=\"{}\" time=\"{:.3}\">{}</testcase>",
            esc(&format!("run {}", self.scenario_name)),
            self.duration_secs,
            if run_ok {
                String::new()
            } else {
                format!(
                    "<failure message=\"{}\"/>",
                    esc(&format!(
                        "run {}{}",
                        self.state,
                        self.error
                            .as_deref()
                            .map(|e| format!(": {e}"))
                            .unwrap_or_default()
                    ))
                )
            }
        ));
        for th in &self.thresholds {
            cases.push(format!(
                "    <testcase classname=\"rustybin.harness.thresholds\" name=\"{}\" time=\"0\">{}</testcase>",
                esc(&th.name),
                if th.pass {
                    String::new()
                } else {
                    format!(
                        "<failure message=\"{}\"/>",
                        esc(&format!("limit {} measured {}", th.limit, th.measured))
                    )
                }
            ));
        }
        let failures = usize::from(!run_ok) + self.thresholds.iter().filter(|t| !t.pass).count();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n  <testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"0\" time=\"{:.3}\">\n{}\n  </testsuite>\n</testsuites>\n",
            esc(&format!("rustybin harness {}", self.run_id)),
            cases.len(),
            failures,
            self.duration_secs,
            cases.join("\n")
        )
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect()
    }
}

/// Header names whose values never appear in a report.
pub fn sensitive_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("authorization")
        || n.contains("api-key")
        || n.contains("apikey")
        || n.contains("token")
        || n.contains("cookie")
        || n.contains("secret")
        || n.contains("password")
}

/// The scenario as stored in a report: credential-looking header values
/// replaced by `[redacted]`.
pub fn redact_scenario(scenario: &Value) -> Value {
    fn walk(v: &mut Value) {
        match v {
            Value::Object(map) => {
                if let Some(Value::Object(headers)) = map.get_mut("headers") {
                    for (name, value) in headers.iter_mut() {
                        if sensitive_header(name) {
                            *value = Value::String("[redacted]".into());
                        }
                    }
                }
                for value in map.values_mut() {
                    walk(value);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    let mut out = scenario.clone();
    walk(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(state: &str, thresholds: Vec<ThresholdResult>) -> Report {
        let passed = thresholds.iter().all(|t| t.pass) && state == "finished";
        Report {
            run_id: "r1".into(),
            scenario_name: "smoke".into(),
            scenario_hash: "abcdef0123456789".into(),
            node_id: "n1".into(),
            rustybin_version: "0.1.0".into(),
            started_at: "2026-10-25T10:00:00Z".into(),
            finished_at: "2026-10-25T10:00:02Z".into(),
            duration_secs: 2.0,
            planned_secs: 2.0,
            state: state.into(),
            stopped_by: None,
            error: None,
            caps: Caps::default(),
            targets: vec![TargetReport {
                url: "http://127.0.0.1:8080".into(),
                weight: 1,
                http: "auto".into(),
                pinned: vec!["127.0.0.1:8080".into()],
                verified: false,
            }],
            totals: Counts {
                requested: 100,
                sent: 100,
                completed: 100,
                ok: 98,
                unexpected_status: 2,
                statuses: BTreeMap::from([("200".to_string(), 98), ("500".to_string(), 2)]),
                ..Counts::default()
            },
            latency: Percentiles {
                count: 100,
                p50_ms: 1.0,
                p99_ms: 5.0,
                max_ms: 7.0,
                ..Percentiles::default()
            },
            service_latency: Percentiles::default(),
            achieved_rate: 50.0,
            requested_rate: 50.0,
            layers: Vec::new(),
            targets_breakdown: Vec::new(),
            timeline: Vec::new(),
            saturation: Saturation::default(),
            exit_code: Report::exit_code_for(state, &thresholds),
            thresholds,
            passed,
            samples: Vec::new(),
            scenario: json!({}),
        }
    }

    #[test]
    fn thresholds_are_evaluated() {
        let th = Thresholds {
            error_rate: Some(0.01),
            not_sent: Some(0),
            latency_p99_ms: Some(10.0),
            min_achieved: Some(0.99),
            saturated: Some(false),
            ..Thresholds::default()
        };
        let totals = Counts {
            requested: 100,
            sent: 100,
            not_sent: 0,
            completed: 100,
            ok: 98,
            unexpected_status: 2,
            ..Counts::default()
        };
        let latency = Percentiles {
            p99_ms: 4.0,
            ..Percentiles::default()
        };
        let results = evaluate(&th, &totals, &latency, &Saturation::default());
        let by_name: BTreeMap<_, _> = results.iter().map(|r| (r.name.as_str(), r.pass)).collect();
        assert!(!by_name["error_rate"]);
        assert!(by_name["not_sent"]);
        assert!(by_name["latency_p99_ms"]);
        assert!(by_name["min_achieved"]);
        assert!(by_name["saturated"]);
        assert_eq!(Report::exit_code_for("finished", &results), 1);
        assert_eq!(Report::exit_code_for("stopped", &results), 2);
        assert_eq!(Report::exit_code_for("finished", &[]), 0);
    }

    #[test]
    fn text_and_junit_renderings() {
        let r = report(
            "finished",
            vec![ThresholdResult {
                name: "error_rate".into(),
                limit: json!(0.01),
                measured: json!(0.02),
                pass: false,
            }],
        );
        let text = r.to_text();
        assert!(text.contains("FAIL error_rate"));
        assert!(text.contains("Result: FAIL (exit code 1)"));
        assert!(text.contains("200: 98, 500: 2"));
        let junit = r.to_junit();
        assert!(junit.contains("tests=\"2\" failures=\"1\""));
        assert!(junit.contains("<failure message=\"limit 0.01 measured 0.02\"/>"));
        let json = serde_json::to_string(&r).unwrap();
        let back = Report::from_json(&json).unwrap();
        assert_eq!(back, r);
        assert!(Report::from_json("{}").is_err());
    }

    #[test]
    fn buckets_absorb() {
        let mut a = Bucket {
            t_secs: 0.0,
            width_secs: 1.0,
            completed: 10,
            p99_ms: 2.0,
            ..Bucket::default()
        };
        let b = Bucket {
            t_secs: 1.0,
            width_secs: 1.0,
            completed: 30,
            p99_ms: 5.0,
            saturated: true,
            ..Bucket::default()
        };
        a.absorb(&b);
        assert_eq!(a.width_secs, 2.0);
        assert_eq!(a.completed, 40);
        assert_eq!(a.rate, 20.0);
        assert_eq!(a.p99_ms, 5.0);
        assert!(a.saturated);
    }

    #[test]
    fn redaction() {
        let s = json!({ "layers": [{ "source": { "request": { "headers": { "Authorization": "Bearer x", "X-Api-Key": "k", "Accept": "json" } } } }] });
        let r = redact_scenario(&s);
        let h = &r["layers"][0]["source"]["request"]["headers"];
        assert_eq!(h["Authorization"], "[redacted]");
        assert_eq!(h["X-Api-Key"], "[redacted]");
        assert_eq!(h["Accept"], "json");
    }
}
