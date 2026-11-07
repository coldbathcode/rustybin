//! A finished run's results as OpenTelemetry metrics: OTLP over HTTP with
//! JSON bodies, so no client library and no gRPC, to the collector named by
//! the setting `harness.otlp_endpoint`.
//!
//! A task watches for finished runs and sends each once. It reads reports
//! after the fact, so the engine knows nothing of it and a slow or dead
//! collector can never touch a run. It goes out under `Purpose::Otlp`,
//! whose rule is the endpoint's host, and a report's content stays numbers,
//! the scenario's name and its id: never a target, a header or a body.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use serde_json::{json, Value};

use crate::egress::{Allowlist, Purpose, Rule};
use crate::settings::{Kind, Spec};
use crate::state::AppState;

pub const ENDPOINT_KEY: &str = "harness.otlp_endpoint";
pub const TOKEN_KEY: &str = "harness.otlp_token";
/// How often finished runs are looked for.
pub const EVERY: Duration = Duration::from_secs(5);
const TIMEOUT: Duration = Duration::from_secs(10);
/// Run ids remembered as sent.
const REMEMBERED: usize = 1_000;

pub fn specs() -> Vec<Spec> {
    vec![
        Spec::new(
            ENDPOINT_KEY,
            "harness",
            "OTLP endpoint",
            "An OpenTelemetry collector's HTTP base URL (https://otel.example.com:4318): each finished load test run is sent to /v1/metrics. Empty: nothing is sent.",
            Kind::Text { max_len: 512 },
            Value::Null,
        ),
        Spec::new(
            TOKEN_KEY,
            "harness",
            "OTLP token",
            "Sent as Authorization: Bearer to the collector, when it asks for one.",
            Kind::Text { max_len: 512 },
            Value::Null,
        )
        .secret(),
    ]
}

/// Run ids already sent, oldest out first.
#[derive(Default)]
pub struct Sent {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl Sent {
    /// Note `id`: false when it was sent before.
    fn first_time(&mut self, id: &str) -> bool {
        if !self.ids.insert(id.to_string()) {
            return false;
        }
        self.order.push_back(id.to_string());
        if self.order.len() > REMEMBERED {
            if let Some(old) = self.order.pop_front() {
                self.ids.remove(&old);
            }
        }
        true
    }
}

fn attribute(key: &str, value: &str) -> Value {
    json!({ "key": key, "value": { "stringValue": value } })
}

/// The report as an OTLP `ExportMetricsServiceRequest`. Works on the JSON of
/// a node's report and a cluster's alike.
pub fn body(report: &Value, instance: &str, now_nanos: u128) -> Value {
    let attributes = json!([
        attribute("run.id", report["run_id"].as_str().unwrap_or("")),
        attribute(
            "scenario.name",
            report["scenario_name"].as_str().unwrap_or("")
        ),
        attribute("run.state", report["state"].as_str().unwrap_or("")),
    ]);
    let time = now_nanos.to_string();
    let int = |name: &str, unit: &str, value: u64| {
        json!({ "name": name, "unit": unit, "gauge": { "dataPoints": [
            { "asInt": value.to_string(), "timeUnixNano": time, "attributes": attributes }
        ] } })
    };
    let double = |name: &str, unit: &str, value: f64| {
        json!({ "name": name, "unit": unit, "gauge": { "dataPoints": [
            { "asDouble": value, "timeUnixNano": time, "attributes": attributes }
        ] } })
    };
    let count = |k: &str| report["totals"][k].as_u64().unwrap_or(0);
    let ms = |k: &str| report["latency"][k].as_f64().unwrap_or(0.0);
    json!({ "resourceMetrics": [{
        "resource": { "attributes": [
            attribute("service.name", "rustybin-harness"),
            attribute("service.instance.id", instance),
        ] },
        "scopeMetrics": [{
            "scope": { "name": "rustybin.harness", "version": env!("CARGO_PKG_VERSION") },
            "metrics": [
                int("rustybin.harness.requests.sent", "{request}", count("sent")),
                int("rustybin.harness.requests.completed", "{request}", count("completed")),
                int("rustybin.harness.requests.ok", "{request}", count("ok")),
                int("rustybin.harness.requests.transport_errors", "{request}", count("transport")),
                int("rustybin.harness.requests.lost", "{request}", count("lost")),
                double("rustybin.harness.latency.p50", "ms", ms("p50_ms")),
                double("rustybin.harness.latency.p99", "ms", ms("p99_ms")),
                double("rustybin.harness.rate.achieved", "{request}/s", report["achieved_rate"].as_f64().unwrap_or(0.0)),
                int("rustybin.harness.passed", "1", u64::from(report["passed"] == json!(true))),
            ],
        }],
    }] })
}

/// The collector's `/v1/metrics`, and the rule that lets the policy reach it.
fn target(endpoint: &str) -> Option<(String, Rule)> {
    let url = reqwest::Url::parse(endpoint.trim()).ok()?;
    let host = url.host_str()?.to_string();
    let entry = match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host,
    };
    let (allow, _) = Allowlist::parse(&[entry]);
    let base = endpoint.trim().trim_end_matches('/');
    Some((
        format!("{base}/v1/metrics"),
        Rule {
            allow,
            ..Rule::default()
        },
    ))
}

/// Send one report. The failure is the caller's to log.
pub async fn send(state: &AppState, endpoint: &str, report: &Value) -> Result<(), String> {
    let (url, rule) = target(endpoint).ok_or_else(|| format!("{endpoint}: not a URL"))?;
    state.egress.permit(Purpose::Otlp, rule);
    let pinned = state
        .egress
        .resolve(&url, Purpose::Otlp)
        .await
        .map_err(|e| e.0)?;
    let client = state.egress.client(&pinned, TIMEOUT)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut req = client
        .post(&pinned.url)
        .json(&body(report, &state.config.instance_id, now));
    if let Some(token) = state.settings.secret(TOKEN_KEY).filter(|t| !t.is_empty()) {
        req = req.bearer_auth(token.as_str());
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("the collector answered {}", resp.status()))
    }
}

/// Finished reports on this node: its own runs and the clusters it leads.
fn finished(state: &AppState) -> Vec<Value> {
    let Some(h) = &state.harness else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for run in h.runs() {
        if let Some(r) = run.report().and_then(|r| serde_json::to_value(r).ok()) {
            out.push(r);
        }
    }
    for cluster in h.coordinator.list() {
        for run in cluster.runs() {
            if let Some(r) = run.report().and_then(|r| serde_json::to_value(r).ok()) {
                out.push(r);
            }
        }
    }
    out
}

/// One look: send what finished since the last. A report that cannot be
/// sent is not tried again: a run's results are in its report either way.
pub async fn tick(state: &AppState, sent: &mut Sent) {
    let endpoint = state.settings.string(ENDPOINT_KEY).unwrap_or_default();
    if endpoint.trim().is_empty() {
        return;
    }
    for report in finished(state) {
        let id = report["run_id"].as_str().unwrap_or("").to_string();
        if id.is_empty() || !sent.first_time(&id) {
            continue;
        }
        if let Err(e) = send(state, &endpoint, &report).await {
            tracing::warn!(target: "rustybin::harness", run = %id, "harness: the run's metrics were not sent: {e}");
        }
    }
}

#[cfg(test)]
#[path = "otlp_tests.rs"]
mod tests;
