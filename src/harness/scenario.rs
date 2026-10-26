//! The scenario: every parameter of a run (`docs/plans/harness.md`, section
//! 6). There is one model, one validation and one JSON Schema generated next
//! to it ([`schema`]); the API, scenario files, the MCP tools and the console
//! builder all read and write that same model.
//!
//! Phase 1 covers targets with weights, a request template source (with
//! `{{seq}}`, `{{vu}}`, `{{run}}`, `{{node}}`, `{{layer}}` and `{{rand}}`
//! placeholders), open and closed load models, layers with `constant` and
//! `ramp` patterns, the scale block (virtual clients, a per-node rate cap),
//! the request timeout and the thresholds a run is judged by. Mock and LLM
//! sources, identities, chaos and the other patterns are later phases and
//! are refused by name rather than silently ignored (`deny_unknown_fields`).

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::pattern::{Pattern, Stage};
use super::safety;
use crate::config::HarnessConfig;

pub const MAX_LAYERS: usize = 8;
pub const MAX_TARGETS: usize = 16;
pub const MAX_REQUESTS_PER_LAYER: usize = 64;
pub const MAX_BODY_BYTES: usize = 64 * 1024;
pub const MAX_HEADERS: usize = 32;
pub const MAX_VIRTUAL_CLIENTS: u32 = 100_000;
/// Most nodes in a cluster (plan section 10).
pub const MAX_CLUSTER_NODES: u32 = 64;
pub const MAX_NAME_LEN: usize = 64;
pub const MAX_DESCRIPTION_LEN: usize = 1024;
/// Longest scenario document accepted (JSON or YAML text).
pub const MAX_SCENARIO_BYTES: usize = 256 * 1024;
pub const MIN_TIMEOUT: Duration = Duration::from_millis(1);
pub const MAX_TIMEOUT: Duration = Duration::from_secs(60);
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_THINK_TIME: Duration = Duration::from_secs(60);
pub const MAX_STARTS_AFTER: Duration = Duration::from_secs(24 * 3600);
/// Placeholders a request template may use.
pub const PLACEHOLDERS: &[&str] = &["seq", "vu", "run", "node", "layer", "rand"];

// ── Durations ───────────────────────────────────────────────────────

/// A duration written as `500ms`, `30s`, `2m`, `1h` or a number of seconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dur(pub Duration);

impl Dur {
    pub fn secs(s: u64) -> Self {
        Dur(Duration::from_secs(s))
    }
}

/// Parse `1.5s`, `250ms`, `2m`, `1h`, `1d` or a plain number of seconds.
pub fn parse_duration(raw: &str) -> Option<Duration> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 32 {
        return None;
    }
    let (number, unit) = match raw.find(|c: char| c.is_ascii_alphabetic()) {
        Some(i) => (&raw[..i], raw[i..].trim()),
        None => (raw, "s"),
    };
    let value: f64 = number.trim().parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let secs = match unit {
        "ms" => value / 1000.0,
        "s" | "sec" | "secs" => value,
        "m" | "min" | "mins" => value * 60.0,
        "h" | "hr" | "hrs" => value * 3600.0,
        "d" => value * 86_400.0,
        _ => return None,
    };
    if secs > 366.0 * 86_400.0 {
        return None;
    }
    Some(Duration::from_secs_f64(secs))
}

/// The shortest exact rendering: `2h`, `90s`, `1500ms`, `0.5ms`.
pub fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if !d.as_nanos().is_multiple_of(1_000_000) {
        return format!("{}ms", d.as_secs_f64() * 1000.0);
    }
    if ms.is_multiple_of(3_600_000) && ms > 0 {
        format!("{}h", ms / 3_600_000)
    } else if ms.is_multiple_of(60_000) && ms > 0 {
        format!("{}m", ms / 60_000)
    } else if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{ms}ms")
    }
}

impl Serialize for Dur {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format_duration(self.0))
    }
}

impl<'de> Deserialize<'de> for Dur {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Number(f64),
            Text(String),
        }
        match Raw::deserialize(d)? {
            Raw::Number(n) if n.is_finite() && (0.0..=366.0 * 86_400.0).contains(&n) => {
                Ok(Dur(Duration::from_secs_f64(n)))
            }
            Raw::Number(n) => Err(serde::de::Error::custom(format!(
                "{n} is not a duration (seconds, 0 or more)"
            ))),
            Raw::Text(t) => parse_duration(&t).map(Dur).ok_or_else(|| {
                serde::de::Error::custom(format!(
                    "{t:?} is not a duration (e.g. 500ms, 30s, 2m, 1h)"
                ))
            }),
        }
    }
}

// ── Caps ────────────────────────────────────────────────────────────

/// The node's limits, the bounds of every scenario it accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    /// Requests per second (the sum of every layer's peak).
    pub max_rate: u64,
    /// Requests in flight (also the most virtual users of a closed layer).
    pub max_inflight: usize,
    /// Longest run.
    pub max_duration_secs: u64,
}

impl Caps {
    pub fn from_config(config: &HarnessConfig) -> Self {
        Self {
            max_rate: config.max_rate,
            max_inflight: config.max_inflight,
            max_duration_secs: config.max_duration.as_secs(),
        }
    }

    pub fn max_duration(&self) -> Duration {
        Duration::from_secs(self.max_duration_secs)
    }
}

impl Default for Caps {
    fn default() -> Self {
        Self::from_config(&HarnessConfig::default())
    }
}

// ── Model ───────────────────────────────────────────────────────────

/// How a target is reached.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpVersion {
    /// HTTP/1.1 on plain connections, ALPN (h2 or http/1.1) on TLS.
    #[default]
    Auto,
    /// HTTP/1.1 only.
    H1,
    /// HTTP/2 over TLS (ALPN h2 only).
    H2,
    /// HTTP/2 with prior knowledge on a plain connection.
    H2c,
}

impl HttpVersion {
    pub fn as_str(self) -> &'static str {
        match self {
            HttpVersion::Auto => "auto",
            HttpVersion::H1 => "h1",
            HttpVersion::H2 => "h2",
            HttpVersion::H2c => "h2c",
        }
    }
}

/// One gateway (or gateway node) the load goes to.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Target {
    pub url: String,
    pub weight: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub http: HttpVersion,
    pub insecure: bool,
}

impl Target {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            weight: 1,
            host: None,
            http: HttpVersion::Auto,
            insecure: false,
        }
    }
}

impl<'de> Deserialize<'de> for Target {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Full {
            url: String,
            #[serde(default = "one")]
            weight: u32,
            #[serde(default)]
            host: Option<String>,
            #[serde(default)]
            http: HttpVersion,
            #[serde(default)]
            insecure: bool,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Url(String),
            Full(Full),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Url(url) => Target::new(url),
            Raw::Full(f) => Target {
                url: f.url,
                weight: f.weight,
                host: f.host,
                http: f.http,
                insecure: f.insecure,
            },
        })
    }
}

fn one() -> u32 {
    1
}

fn get() -> String {
    "GET".into()
}

fn root() -> String {
    "/".into()
}

/// How big the run is, apart from how hard it pushes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scale {
    /// Distinct virtual clients (`{{vu}}` cycles through them; closed layers
    /// never run more users than this).
    #[serde(default = "one")]
    pub virtual_clients: u32,
    /// A rate cap below the node's own, to keep the client well under saturation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rate_per_node: Option<u64>,
    /// Cluster runs: the client nodes the run waits for before it starts
    /// (the rate is split over the client nodes by declared capacity), or
    /// `auto`: as many as the peak rate needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_nodes: Option<NodeCount>,
    /// Cluster runs: the server nodes (the gateway's upstream pool) the run
    /// waits for before it starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_nodes: Option<u32>,
    /// Cluster runs: a node that takes over as leader during the run stops
    /// its own client part and its share is re-split over the other client
    /// nodes (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader_sheds_load: Option<bool>,
    /// Keep-alive connections per virtual client (the connections the
    /// gateway sees: this node's virtual clients times this, at most the
    /// in-flight cap).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connections_per_client: Option<u32>,
    /// With `client_nodes: auto`: how long to wait for enough client nodes
    /// before starting undersized, with a warning (default 5m).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale_timeout: Option<Dur>,
}

/// `scale.client_nodes`: a number of nodes, or `auto`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NodeCount {
    Count(u32),
    Auto(AutoWord),
}

/// The word `auto`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoWord {
    Auto,
}

impl NodeCount {
    /// The number of nodes to wait for (`auto`: at least one).
    pub fn count(self) -> u32 {
        match self {
            NodeCount::Count(n) => n,
            NodeCount::Auto(_) => 1,
        }
    }

    pub fn is_auto(self) -> bool {
        matches!(self, NodeCount::Auto(_))
    }
}

/// Most connections per virtual client.
pub const MAX_CONNECTIONS_PER_CLIENT: u32 = 64;
/// The default and the longest `scale.scale_timeout`.
pub const DEFAULT_SCALE_TIMEOUT: Duration = Duration::from_secs(300);
pub const MAX_SCALE_TIMEOUT: Duration = Duration::from_secs(3600);

impl Default for Scale {
    fn default() -> Self {
        Self {
            virtual_clients: 1,
            max_rate_per_node: None,
            client_nodes: None,
            server_nodes: None,
            leader_sheds_load: None,
            connections_per_client: None,
            scale_timeout: None,
        }
    }
}

/// Open: requests leave at their intended times whatever the answers.
/// Closed: a number of users, each waiting for its answer plus a think time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Model {
    #[default]
    Open,
    Closed,
}

impl Model {
    pub fn as_str(self) -> &'static str {
        match self {
            Model::Open => "open",
            Model::Closed => "closed",
        }
    }
}

/// One request to send, with placeholders.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestTemplate {
    #[serde(default = "get")]
    pub method: String,
    /// Path with an optional query, appended to the target URL.
    #[serde(default = "root")]
    pub path: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// A string is sent as is; any other JSON value is sent as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    /// Statuses that count as expected (default: 2xx and 3xx).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expect_status: Vec<u16>,
    /// Relative share when a layer has several requests.
    #[serde(default = "one")]
    pub weight: u32,
}

impl Default for RequestTemplate {
    fn default() -> Self {
        Self {
            method: get(),
            path: root(),
            headers: BTreeMap::new(),
            body: None,
            expect_status: Vec::new(),
            weight: 1,
        }
    }
}

impl RequestTemplate {
    /// Does `status` count as the expected answer?
    pub fn expects(&self, status: u16) -> bool {
        if self.expect_status.is_empty() {
            (200..400).contains(&status)
        } else {
            self.expect_status.contains(&status)
        }
    }
}

/// Where a layer's requests come from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// One request template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestTemplate>,
    /// Several templates, picked by weight.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<RequestTemplate>,
}

impl Source {
    /// Every template of the source, in order.
    pub fn templates(&self) -> Vec<&RequestTemplate> {
        self.request.iter().chain(self.requests.iter()).collect()
    }
}

/// One stream of traffic inside a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub name: String,
    pub source: Source,
    pub pattern: Pattern,
    #[serde(default)]
    pub model: Model,
    /// Delay before the layer starts (relative to the run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_after: Option<Dur>,
    /// Closed model: the pause of a user between its answer and its next
    /// request (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think_time: Option<Dur>,
}

/// Upstream latency of the shaped endpoint, as a log-normal distribution
/// given by its median and its 99th percentile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerLatency {
    #[serde(default)]
    pub p50: Dur,
    #[serde(default)]
    pub p99: Dur,
}

fn default_sample() -> u32 {
    100
}

fn default_body_bytes() -> u32 {
    256
}

fn default_status() -> BTreeMap<String, u32> {
    BTreeMap::from([("200".to_string(), 1)])
}

/// The server side of a run: how the shaped endpoint `/harness/{*path}`
/// answers marked requests (latency, status mix, body, streaming) and how
/// the inspector samples harness traffic. Pushed to server nodes with the
/// run (single node: the node itself).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerProfile {
    #[serde(default)]
    pub latency: ServerLatency,
    /// Status codes with weights (`{"200": 97, "503": 3}`).
    #[serde(default = "default_status")]
    pub status: BTreeMap<String, u32>,
    /// Response body size (JSON padded to this length).
    #[serde(default = "default_body_bytes")]
    pub body_bytes: u32,
    /// Stream the body in chunks instead of one piece.
    #[serde(default)]
    pub chunked: bool,
    /// Delay before the first byte (on top of the latency).
    #[serde(default)]
    pub first_byte_delay: Dur,
    /// The inspector captures 1 in N marked requests (plus every anomaly);
    /// 0 captures none.
    #[serde(default = "default_sample")]
    pub capture_sample: u32,
}

impl Default for ServerProfile {
    fn default() -> Self {
        Self {
            latency: ServerLatency::default(),
            status: default_status(),
            body_bytes: default_body_bytes(),
            chunked: false,
            first_byte_delay: Dur::default(),
            capture_sample: default_sample(),
        }
    }
}

/// Longest shaped latency, first-byte delay or p99.
pub const MAX_SERVER_DELAY: Duration = Duration::from_secs(30);
/// Largest shaped body.
pub const MAX_SERVER_BODY: u32 = 1024 * 1024;
pub const MAX_SERVER_STATUSES: usize = 16;

/// Pass criteria. A run that misses any of them exits with code 1.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
    /// Transport errors plus unexpected statuses, as a fraction of the
    /// requests sent (0 to 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_rate: Option<f64>,
    /// Most requests the client may skip at its in-flight cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_sent: Option<u64>,
    /// Latency from the intended send time, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_p50_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_p99_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_max_ms: Option<f64>,
    /// Least fraction of the requested requests that must have been
    /// answered (0 to 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_achieved: Option<f64>,
    /// `false`: fail when the client saturated (the report would be about
    /// the client, not the gateway).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saturated: Option<bool>,
    /// Highest p99 of the gateway overhead (total minus upstream processing)
    /// in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_overhead_p99_ms: Option<f64>,
    /// Most requests the gateway may swallow (sent, seen by no server, no
    /// answer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lost: Option<u64>,
    /// Least fraction of the sent requests that must have reached the
    /// server side (0 to 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_reach: Option<f64>,
}

/// A scenario as written: the shorthand fields (`target`, `source`,
/// `pattern`, `model`, `think_time`) describe a single layer and are folded
/// into `targets` and `layers` by [`Scenario::normalise`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Shorthand for one target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<Target>,
    #[serde(default)]
    pub scale: Scale,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<Layer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<Pattern>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think_time: Option<Dur>,
    /// Per-request timeout (default 10s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<Dur>,
    #[serde(default)]
    pub thresholds: Thresholds,
    /// The server side: the shaped endpoint's profile and inspector sampling.
    #[serde(default)]
    pub servers: ServerProfile,
}

/// What a scenario amounts to, for previews, listings and the report.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Summary {
    pub name: String,
    pub duration_secs: f64,
    /// Sum of the layers' peak rates (open layers only).
    pub peak_rate: f64,
    /// Expected requests over the whole run (open layers only).
    pub expected_requests: f64,
    pub layers: Vec<LayerSummary>,
    pub targets: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LayerSummary {
    pub name: String,
    pub model: Model,
    pub starts_after_secs: f64,
    pub duration_secs: f64,
    pub peak: f64,
    pub stages: Vec<Stage>,
}

impl Scenario {
    /// Parse JSON or YAML text.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > MAX_SCENARIO_BYTES {
            return Err(format!(
                "the scenario is longer than {MAX_SCENARIO_BYTES} bytes"
            ));
        }
        if text.trim().is_empty() {
            return Err("the scenario is empty".into());
        }
        serde_yaml::from_str(text).map_err(|e| format!("invalid scenario: {e}"))
    }

    /// Parse a JSON value (the API and the MCP tools).
    pub fn from_value(value: &Value) -> Result<Self, String> {
        if !value.is_object() {
            return Err("the scenario must be an object".into());
        }
        serde_json::from_value(value.clone()).map_err(|e| format!("invalid scenario: {e}"))
    }

    /// Fold the single-layer shorthand into `targets` and `layers`.
    pub fn normalise(&mut self) {
        if let Some(url) = self.target.take() {
            self.targets.insert(0, Target::new(url));
        }
        let has_shorthand = self.source.is_some()
            || self.pattern.is_some()
            || self.model.is_some()
            || self.think_time.is_some();
        if has_shorthand {
            let layer = Layer {
                name: "main".into(),
                source: self.source.take().unwrap_or_default(),
                pattern: self.pattern.take().unwrap_or(Pattern::Constant {
                    rate: 0.0,
                    hold: Dur::secs(0),
                    jitter: None,
                    sync: None,
                    then: None,
                }),
                model: self.model.take().unwrap_or_default(),
                starts_after: None,
                think_time: self.think_time.take(),
            };
            self.layers.insert(0, layer);
        }
    }

    /// The request timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout.map_or(DEFAULT_TIMEOUT, |d| d.0)
    }

    /// Total run time: the longest layer end.
    pub fn duration(&self) -> Duration {
        self.layers
            .iter()
            .map(|l| l.starts_after.map_or(Duration::ZERO, |d| d.0) + l.pattern.duration())
            .max()
            .unwrap_or(Duration::ZERO)
    }

    /// Sum of the open layers' peak rates.
    pub fn peak_rate(&self) -> f64 {
        self.layers
            .iter()
            .filter(|l| l.model == Model::Open)
            .map(|l| l.pattern.peak())
            .sum()
    }

    /// The expected load over time without sending anything: per layer
    /// and in total, the requests per second (open layers, from the exact
    /// integral, so a 50 ms burst shows) or the users (closed layers), one
    /// point per second (coarser past an hour: at most 3600 points).
    pub fn preview(&self) -> Value {
        let total = self.duration().as_secs_f64();
        let step = if total <= 3600.0 {
            1.0
        } else {
            (total / 3600.0).ceil()
        };
        let points = (total / step).ceil() as usize;
        let mut sum = vec![0.0; points];
        let round = |v: f64| (v * 100.0).round() / 100.0;
        let layers: Vec<Value> = self
            .layers
            .iter()
            .map(|l| {
                let compiled = l.pattern.compile();
                let after = l.starts_after.map_or(0.0, |d| d.0.as_secs_f64());
                let series: Vec<f64> = (0..points)
                    .map(|k| {
                        let t0 = k as f64 * step - after;
                        let v = match l.model {
                            Model::Open => compiled.integral(t0.max(0.0), (t0 + step).max(0.0)) / step,
                            Model::Closed => compiled
                                .value_at(t0 + step / 2.0, &super::pattern::Ctx::default())
                                .unwrap_or(0.0),
                        };
                        round(v)
                    })
                    .collect();
                if l.model == Model::Open {
                    for (acc, v) in sum.iter_mut().zip(&series) {
                        *acc += v;
                    }
                }
                json!({
                    "name": l.name,
                    "model": l.model.as_str(),
                    "unit": if l.model == Model::Open { "requests per second" } else { "users" },
                    "starts_after_secs": after,
                    "peak": round(l.pattern.peak()),
                    "expected_requests": match l.model {
                        Model::Open => Some(round(compiled.integral(0.0, compiled.total_secs))),
                        Model::Closed => None,
                    },
                    "closed_loop": l.pattern.has_closed_loop() || l.pattern.chain().iter().any(|p| p.kind() == "breakpoint"),
                    "stages": compiled.stages,
                    "series": series,
                })
            })
            .collect();
        json!({
            "name": self.name,
            "duration_secs": total,
            "step_secs": step,
            "points": points,
            "peak_rate": round(self.peak_rate()),
            "layers": layers,
            "total": sum.into_iter().map(round).collect::<Vec<_>>(),
        })
    }

    pub fn summary(&self) -> Summary {
        Summary {
            name: self.name.clone(),
            duration_secs: self.duration().as_secs_f64(),
            peak_rate: self.peak_rate(),
            expected_requests: self
                .layers
                .iter()
                .filter(|l| l.model == Model::Open)
                .map(|l| {
                    l.pattern
                        .expected_between(Duration::ZERO, l.pattern.duration())
                })
                .sum(),
            layers: self
                .layers
                .iter()
                .map(|l| LayerSummary {
                    name: l.name.clone(),
                    model: l.model,
                    starts_after_secs: l.starts_after.map_or(0.0, |d| d.0.as_secs_f64()),
                    duration_secs: l.pattern.duration().as_secs_f64(),
                    peak: l.pattern.peak(),
                    stages: l.pattern.stages(),
                })
                .collect(),
            targets: self.targets.len(),
        }
    }

    /// SHA-256 of the normalised scenario's JSON, as hex.
    pub fn hash(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        let digest = Sha256::digest(&bytes);
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Every problem with a normalised scenario, against the node's caps.
    /// An empty list means the scenario may run.
    pub fn validate(&self, caps: &Caps) -> Vec<String> {
        self.validate_scaled(caps, 1.0)
    }

    /// [`Scenario::validate`] for a node that runs `share` (0 to 1) of the
    /// load: the rates and users it would send are checked against `caps`.
    /// A cluster run splits the load over its client nodes this way.
    pub fn validate_scaled(&self, caps: &Caps, share: f64) -> Vec<String> {
        let share = if share.is_finite() && share > 0.0 {
            share.min(1.0)
        } else {
            1.0
        };
        let mut p = Vec::new();
        if self.target.is_some()
            || self.source.is_some()
            || self.pattern.is_some()
            || self.model.is_some()
            || self.think_time.is_some()
        {
            p.push("internal: validate a normalised scenario".into());
        }
        if self.name.trim().is_empty() {
            p.push("name: required".into());
        } else if self.name.chars().count() > MAX_NAME_LEN
            || !self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        {
            p.push(format!(
                "name: letters, digits, space, - _ . only, at most {MAX_NAME_LEN} characters"
            ));
        }
        if self
            .description
            .as_ref()
            .is_some_and(|d| d.chars().count() > MAX_DESCRIPTION_LEN)
        {
            p.push(format!(
                "description: at most {MAX_DESCRIPTION_LEN} characters"
            ));
        }
        if self.targets.is_empty() {
            p.push("targets: at least one target URL is required".into());
        } else if self.targets.len() > MAX_TARGETS {
            p.push(format!("targets: at most {MAX_TARGETS}"));
        }
        let mut prefixes = Vec::new();
        for (i, t) in self.targets.iter().enumerate() {
            match safety::parse_target_url(&t.url) {
                Ok(parsed) => {
                    if t.http == HttpVersion::H2 && !parsed.tls() {
                        p.push(format!(
                            "targets[{i}].http: h2 needs an https target (use h2c for a plain connection)"
                        ));
                    }
                    if t.http == HttpVersion::H2c && parsed.tls() {
                        p.push(format!(
                            "targets[{i}].http: h2c is for plain connections (use h2 with https)"
                        ));
                    }
                    prefixes.push(parsed.path);
                }
                Err(e) => {
                    p.push(format!("targets[{i}].url: {e}"));
                    prefixes.push(String::new());
                }
            }
            if t.weight == 0 || t.weight > 1000 {
                p.push(format!("targets[{i}].weight: 1 to 1000"));
            }
            if let Some(host) = &t.host {
                let (name, port) = match host.rsplit_once(':') {
                    Some((n, port)) if port.parse::<u16>().is_ok() => (n, true),
                    _ => (host.as_str(), false),
                };
                let _ = port;
                if !safety::valid_hostname(name.trim_end_matches('.'))
                    && name.parse::<std::net::IpAddr>().is_err()
                {
                    p.push(format!("targets[{i}].host: not a host name"));
                }
            }
        }
        if self.scale.virtual_clients == 0 || self.scale.virtual_clients > MAX_VIRTUAL_CLIENTS {
            p.push(format!("scale.virtual_clients: 1 to {MAX_VIRTUAL_CLIENTS}"));
        }
        if let Some(NodeCount::Count(n)) = self.scale.client_nodes {
            if n == 0 || n > MAX_CLUSTER_NODES {
                p.push(format!(
                    "scale.client_nodes: 1 to {MAX_CLUSTER_NODES}, or auto"
                ));
            }
        }
        if let Some(c) = self.scale.connections_per_client {
            if c == 0 || c > MAX_CONNECTIONS_PER_CLIENT {
                p.push(format!(
                    "scale.connections_per_client: 1 to {MAX_CONNECTIONS_PER_CLIENT}"
                ));
            }
        }
        if let Some(t) = self.scale.scale_timeout {
            if t.0 > MAX_SCALE_TIMEOUT {
                p.push("scale.scale_timeout: at most 1h".into());
            }
            if !self.scale.client_nodes.is_some_and(|c| c.is_auto()) {
                p.push("scale.scale_timeout: only with client_nodes: auto".into());
            }
        }
        if let Some(n) = self.scale.server_nodes {
            if n > MAX_CLUSTER_NODES {
                p.push(format!("scale.server_nodes: 0 to {MAX_CLUSTER_NODES}"));
            }
        }
        let node_rate = match self.scale.max_rate_per_node {
            Some(0) => {
                p.push("scale.max_rate_per_node: must be 1 or more".into());
                caps.max_rate
            }
            Some(r) if r > caps.max_rate => {
                p.push(format!(
                    "scale.max_rate_per_node: {r} is over this node's cap of {} requests per second",
                    caps.max_rate
                ));
                caps.max_rate
            }
            Some(r) => r,
            None => caps.max_rate,
        };
        if self.layers.is_empty() {
            p.push(
                "layers: at least one layer (or a top-level source and pattern) is required".into(),
            );
        } else if self.layers.len() > MAX_LAYERS {
            p.push(format!("layers: at most {MAX_LAYERS}"));
        }
        let mut names = Vec::new();
        for (i, layer) in self.layers.iter().enumerate() {
            let here = format!("layers[{i}]");
            if layer.name.trim().is_empty() {
                p.push(format!("{here}.name: required"));
            } else if layer.name.chars().count() > MAX_NAME_LEN
                || !layer
                    .name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            {
                p.push(format!(
                    "{here}.name: letters, digits, - _ . only, at most {MAX_NAME_LEN} characters"
                ));
            } else if names.contains(&layer.name) {
                p.push(format!(
                    "{here}.name: duplicate layer name {:?}",
                    layer.name
                ));
            } else {
                names.push(layer.name.clone());
            }
            let templates = layer.source.templates();
            if templates.is_empty() {
                p.push(format!(
                    "{here}.source: a request (or requests) template is required (mock and llm sources come in a later phase)"
                ));
            } else if templates.len() > MAX_REQUESTS_PER_LAYER {
                p.push(format!(
                    "{here}.source.requests: at most {MAX_REQUESTS_PER_LAYER}"
                ));
            }
            for (j, t) in templates.iter().enumerate() {
                let tp = if j == 0 && layer.source.request.is_some() {
                    format!("{here}.source.request")
                } else {
                    let k = j - usize::from(layer.source.request.is_some());
                    format!("{here}.source.requests[{k}]")
                };
                validate_template(t, &tp, &prefixes, &mut p);
            }
            layer.pattern.validate(&format!("{here}.pattern"), &mut p);
            // Which model a pattern needs: users for closed layers, rates
            // (and the closed loops) for open ones.
            for (k, part) in layer
                .pattern
                .chain()
                .into_iter()
                .take(super::pattern::MAX_STAGES)
                .enumerate()
            {
                let at = format!("{here}.pattern{}", ".then".repeat(k));
                let open_only = match part {
                    Pattern::Target { users: Some(_), .. } => {
                        if layer.model == Model::Open {
                            p.push(format!(
                                "{at}.users: a target of users needs model: closed (an open layer's target is a rate)"
                            ));
                        }
                        false
                    }
                    Pattern::Target { .. } | Pattern::Breakpoint { .. } | Pattern::Burst { .. } => {
                        true
                    }
                    _ => false,
                };
                if open_only && layer.model == Model::Closed {
                    p.push(format!(
                        "{at}: a {} pattern needs the open model",
                        part.kind()
                    ));
                }
            }
            if let Some(after) = layer.starts_after {
                if after.0 > MAX_STARTS_AFTER {
                    p.push(format!("{here}.starts_after: at most 24h"));
                }
            }
            if let Some(think) = layer.think_time {
                if think.0 > MAX_THINK_TIME {
                    p.push(format!("{here}.think_time: at most 60s"));
                }
                if layer.model == Model::Open {
                    p.push(format!(
                        "{here}.think_time: only a closed layer has a think time"
                    ));
                }
            }
            if layer.model == Model::Closed {
                let peak = layer.pattern.peak() * share;
                if layer.pattern.peak() > self.scale.virtual_clients as f64 {
                    p.push(format!(
                        "{here}.pattern: a closed layer runs at most scale.virtual_clients ({}) users, the pattern asks for {}",
                        self.scale.virtual_clients,
                        layer.pattern.peak()
                    ));
                }
                if peak > caps.max_inflight as f64 {
                    p.push(format!(
                        "{here}.pattern: {peak} users is over this node's in-flight cap of {}",
                        caps.max_inflight
                    ));
                }
            }
        }
        let peak = self.peak_rate() * share;
        if peak > node_rate as f64 + 1e-9 {
            let whose = if share < 1.0 {
                format!(" (this node's share of {:.1}%)", share * 100.0)
            } else {
                String::new()
            };
            p.push(format!(
                "layers: the peak rates add up to {peak} requests per second{whose}, over the cap of {node_rate} (RUSTYBIN_HARNESS_MAX_RATE, scale.max_rate_per_node)"
            ));
        }
        let duration = self.duration();
        if duration > caps.max_duration() {
            p.push(format!(
                "layers: the run would last {}, over the cap of {} (RUSTYBIN_HARNESS_MAX_DURATION)",
                format_duration(duration),
                format_duration(caps.max_duration())
            ));
        }
        if !self.layers.is_empty() && duration.is_zero() {
            p.push("layers: the run would last 0 seconds".into());
        }
        let timeout = self.timeout();
        if timeout < MIN_TIMEOUT || timeout > MAX_TIMEOUT {
            p.push("timeout: 1ms to 60s".into());
        }
        let sv = &self.servers;
        if sv.latency.p50.0 > sv.latency.p99.0 {
            p.push("servers.latency: p50 must not be above p99".into());
        }
        if sv.latency.p99.0 > MAX_SERVER_DELAY || sv.first_byte_delay.0 > MAX_SERVER_DELAY {
            p.push("servers: latencies and the first-byte delay are at most 30s".into());
        }
        if sv.status.is_empty() {
            p.push("servers.status: at least one status with a weight".into());
        } else if sv.status.len() > MAX_SERVER_STATUSES {
            p.push(format!(
                "servers.status: at most {MAX_SERVER_STATUSES} statuses"
            ));
        }
        for (code, weight) in &sv.status {
            if !code.parse::<u16>().is_ok_and(|c| (100..=599).contains(&c)) {
                p.push(format!("servers.status.{code}: statuses are 100 to 599"));
            }
            if *weight == 0 || *weight > 1000 {
                p.push(format!("servers.status.{code}: weights are 1 to 1000"));
            }
        }
        if sv.body_bytes > MAX_SERVER_BODY {
            p.push(format!("servers.body_bytes: at most {MAX_SERVER_BODY}"));
        }
        if sv.capture_sample > 1_000_000 {
            p.push("servers.capture_sample: at most 1000000".into());
        }
        let t = &self.thresholds;
        let fraction = |name: &str, v: Option<f64>, p: &mut Vec<String>| {
            if let Some(v) = v {
                if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                    p.push(format!("thresholds.{name}: a fraction from 0 to 1"));
                }
            }
        };
        fraction("error_rate", t.error_rate, &mut p);
        fraction("min_achieved", t.min_achieved, &mut p);
        fraction("upstream_reach", t.upstream_reach, &mut p);
        for (name, v) in [
            ("latency_p50_ms", t.latency_p50_ms),
            ("latency_p99_ms", t.latency_p99_ms),
            ("latency_max_ms", t.latency_max_ms),
            ("gateway_overhead_p99_ms", t.gateway_overhead_p99_ms),
        ] {
            if let Some(v) = v {
                if !v.is_finite() || !(0.0..=3_600_000.0).contains(&v) {
                    p.push(format!("thresholds.{name}: 0 to 3600000 milliseconds"));
                }
            }
        }
        p
    }
}

fn validate_template(t: &RequestTemplate, path: &str, prefixes: &[String], p: &mut Vec<String>) {
    let method = t.method.to_ascii_uppercase();
    if !matches!(
        method.as_str(),
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
    ) {
        p.push(format!(
            "{path}.method: one of GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS"
        ));
    }
    if !t.path.starts_with('/') {
        p.push(format!("{path}.path: must start with /"));
    } else if t.path.len() > 2048 {
        p.push(format!("{path}.path: at most 2048 characters"));
    } else if t.path.contains(['\r', '\n', ' ']) {
        p.push(format!(
            "{path}.path: must not contain spaces or line breaks"
        ));
    } else {
        let bare = strip_placeholders(&t.path);
        if safety::is_control_prefix(&bare)
            || prefixes
                .iter()
                .any(|pre| safety::is_control_prefix(&format!("{pre}{bare}")))
        {
            p.push(format!(
                "{path}.path: a run never targets a Rustybin control plane (/_rustybin, /ui)"
            ));
        }
    }
    if t.headers.len() > MAX_HEADERS {
        p.push(format!("{path}.headers: at most {MAX_HEADERS}"));
    }
    for (name, value) in &t.headers {
        if axum::http::HeaderName::from_bytes(name.as_bytes()).is_err() {
            p.push(format!("{path}.headers.{name}: not a header name"));
        }
        if value.contains(['\r', '\n']) || value.len() > 8192 {
            p.push(format!(
                "{path}.headers.{name}: no line breaks, at most 8192 characters"
            ));
        }
        for ph in placeholders(value) {
            if !PLACEHOLDERS.contains(&ph.as_str()) {
                p.push(format!(
                    "{path}.headers.{name}: unknown placeholder {{{{{ph}}}}} (known: {})",
                    PLACEHOLDERS.join(", ")
                ));
            }
        }
    }
    for ph in placeholders(&t.path) {
        if !PLACEHOLDERS.contains(&ph.as_str()) {
            p.push(format!(
                "{path}.path: unknown placeholder {{{{{ph}}}}} (known: {})",
                PLACEHOLDERS.join(", ")
            ));
        }
    }
    if let Some(body) = &t.body {
        let text = match body {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if text.len() > MAX_BODY_BYTES {
            p.push(format!("{path}.body: at most {MAX_BODY_BYTES} bytes"));
        }
        for ph in placeholders(&text) {
            if !PLACEHOLDERS.contains(&ph.as_str()) {
                p.push(format!(
                    "{path}.body: unknown placeholder {{{{{ph}}}}} (known: {})",
                    PLACEHOLDERS.join(", ")
                ));
            }
        }
    }
    for s in &t.expect_status {
        if !(100..=599).contains(s) {
            p.push(format!("{path}.expect_status: statuses are 100 to 599"));
            break;
        }
    }
    if t.weight == 0 || t.weight > 1000 {
        p.push(format!("{path}.weight: 1 to 1000"));
    }
}

/// The `{{name}}` placeholders in a template string.
pub fn placeholders(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                out.push(after[..end].trim().to_string());
                rest = &after[end + 2..];
            }
            None => break,
        }
    }
    out
}

/// A template path with every placeholder replaced by `x` (for the control
/// plane fence).
fn strip_placeholders(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                out.push('x');
                rest = &after[end + 2..];
            }
            None => {
                rest = after;
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

// ── JSON Schema ─────────────────────────────────────────────────────

/// Schema id served by `GET /_rustybin/harness/schema`.
pub const SCHEMA_ID: &str = "https://rustybin.dev/schemas/harness-scenario.json";

fn prop(ty: &str, description: &str) -> Value {
    json!({ "type": ty, "description": description })
}

fn number(description: &str, min: f64, max: f64) -> Value {
    json!({ "type": "number", "description": description, "minimum": min, "maximum": max })
}

fn integer(description: &str, min: u64, max: u64) -> Value {
    json!({ "type": "integer", "description": description, "minimum": min, "maximum": max })
}

fn string(description: &str, max_len: usize) -> Value {
    json!({ "type": "string", "description": description, "maxLength": max_len })
}

fn duration(description: &str, max_secs: u64) -> Value {
    json!({
        "type": ["string", "number"],
        "format": "duration",
        "description": format!("{description} A duration: 500ms, 30s, 2m, 1h, or a number of seconds."),
        "pattern": "^\\s*[0-9]+(\\.[0-9]+)?\\s*(ms|s|m|h|d)?\\s*$",
        "minimum": 0,
        "maximum": max_secs
    })
}

/// Mark a top-level property as shorthand for a layer field: the console
/// builder edits the normalised form and skips it.
fn shorthand(mut v: Value) -> Value {
    v["x-shorthand"] = json!("layers");
    v
}

fn enumeration(description: &str, values: &[&str], default: &str) -> Value {
    json!({ "type": "string", "description": description, "enum": values, "default": default })
}

/// One pattern type of the schema: its `type` constant, its own
/// properties, and `jitter`, `sync` and `then`, which every type takes.
fn pattern_variant(
    kind: &str,
    description: &str,
    props: Value,
    required: &[&str],
    then: &Value,
) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "type".into(),
        json!({ "const": kind, "type": "string", "description": "Pattern type." }),
    );
    if let Value::Object(own) = props {
        properties.extend(own);
    }
    properties.insert(
        "jitter".into(),
        number(
            "Random variation of each second's value, a fraction either way (0.1: up to 10 percent).",
            0.0,
            1.0,
        ),
    );
    properties.insert(
        "sync".into(),
        enumeration(
            "Cluster runs: cluster keeps every client node in lockstep (a burst reaches the gateway as one burst); node gives each node its own phase of a periodic pattern and its own jitter, which spreads the load.",
            &["cluster", "node"],
            "cluster",
        ),
    );
    properties.insert("then".into(), then.clone());
    let mut req = vec!["type"];
    req.extend_from_slice(required);
    json!({
        "title": kind,
        "type": "object",
        "description": description,
        "properties": properties,
        "required": req,
        "additionalProperties": false
    })
}

/// The schema of a pattern: one branch per type, told apart by `type`.
fn pattern_schema(max_rate: f64, max_dur: u64, then: &Value) -> Value {
    use super::pattern::{MAX_BURST, MAX_CYCLES};
    let rate = |d: &str| number(d, 0.0, max_rate);
    json!({
        "type": "object",
        "description": "The shape of the layer's load over time: the request rate per second (open model) or the number of active users (closed model).",
        "oneOf": [
            pattern_variant("constant", "A steady rate for a while: baselines and soaks.", json!({
                "rate": rate("Requests per second (open) or users (closed)."),
                "for": duration("How long to hold the rate.", max_dur)
            }), &["rate", "for"], then),
            pattern_variant("ramp", "From one rate to another over a period: where latency bends as load grows.", json!({
                "from": rate("Rate (or users) at the start."),
                "to": rate("Rate (or users) at the end."),
                "over": duration("How long the ramp takes.", max_dur),
                "curve": enumeration("Linear, or exponential (from and to above 0).", &["linear", "exponential"], "linear")
            }), &["from", "to", "over"], then),
            pattern_variant("step", "Capacity plateaus: start, start + step, ... up to max, each held for every and reported on its own, then hold on the last one.", json!({
                "start": rate("Rate (or users) of the first plateau."),
                "step": number("Increase from one plateau to the next.", 0.0, max_rate),
                "every": duration("How long each plateau lasts.", max_dur),
                "max": rate("The highest plateau: the steps stop at or below it."),
                "hold": duration("How long to stay on the last plateau after the steps.", max_dur)
            }), &["start", "step", "every", "max"], then),
            pattern_variant("target", "Reach a load target within a time and hold it. With overhead_p99_ms it is a closed loop: the rate is adjusted every second until the gateway overhead sits at the target.", json!({
                "rate": rate("The rate to reach (open model)."),
                "users": rate("The users to reach (closed model)."),
                "overhead_p99_ms": number("A closed loop on the gateway overhead p99 (from answers tagged by Rustybin servers), in milliseconds: the rate goes up while the overhead is below it and down while it is above.", 0.0, 60_000.0),
                "within": duration("How long to reach the target: a ramp from start, or the time the loop has to settle.", max_dur),
                "hold": duration("How long to hold the target once there.", max_dur),
                "start": rate("Where to start: the ramp's first value (default 0), or the loop's first rate (default 10)."),
                "max": rate("The loop's highest rate (required with overhead_p99_ms).")
            }), &["within", "hold"], then),
            pattern_variant("breakpoint", "Find the maximum sustainable rate: grow plateau by plateau until one fails a threshold, then stop the run and report the last good rate.", json!({
                "start": rate("The first plateau's rate."),
                "growth": number("Growth per plateau, a fraction (0.1: each plateau 10 percent above the last).", 0.0, 10.0),
                "every": duration("How long each plateau lasts; its results decide.", max_dur),
                "max": rate("The highest rate tried."),
                "until": {
                    "type": "object",
                    "description": "The thresholds: the first plateau that fails one stops the run.",
                    "properties": {
                        "error_rate": number("Errors (transport and unexpected statuses) per answer.", 0.0, 1.0),
                        "latency_p99_ms": number("Latency p99 from the intended send, in milliseconds.", 0.0, 600_000.0),
                        "overhead_p99_ms": number("Gateway overhead p99, in milliseconds (needs answers tagged by Rustybin servers).", 0.0, 600_000.0)
                    },
                    "additionalProperties": false
                }
            }), &["start", "growth", "every", "until", "max"], then),
            pattern_variant("spike", "A sudden surge and the recovery after it: base to peak over rise, held, back over fall, then recover at base.", json!({
                "base": rate("The rate before and after the spike."),
                "peak": rate("The rate at the top of the spike."),
                "rise": duration("How long the rise takes (0 for an instant jump).", max_dur),
                "hold": duration("How long the peak lasts.", max_dur),
                "fall": duration("How long the fall takes.", max_dur),
                "recover": duration("How long to watch the recovery at base.", max_dur)
            }), &["base", "peak", "rise", "hold", "fall", "recover"], then),
            pattern_variant("pulse", "A square wave: high for on, low for off, cycles times (autoscaling, pools and limiters that refill between pulses).", json!({
                "high": rate("The rate while on."),
                "low": rate("The rate while off."),
                "on": duration("How long each pulse lasts.", max_dur),
                "off": duration("How long the pause between pulses lasts.", max_dur),
                "cycles": integer("How many pulses (default 1).", 1, MAX_CYCLES as u64)
            }), &["high", "low", "on", "off"], then),
            pattern_variant("burst", "Short spikes on a base rate: size requests sent within within (1000 within 50ms), every every. Queueing and burst allowances such as token buckets.", json!({
                "base": rate("The rate between bursts."),
                "size": integer("Requests in each burst.", 1, MAX_BURST as u64),
                "within": duration("How quickly a burst is sent.", max_dur),
                "every": duration("How often a burst comes (longer than within).", max_dur),
                "for": duration("How long the pattern lasts.", max_dur)
            }), &["base", "size", "within", "every", "for"], then),
            pattern_variant("wave", "Smooth periodic load between min and max, starting at min: a daily curve compressed in time.", json!({
                "min": rate("The lowest rate."),
                "max": rate("The highest rate."),
                "period": duration("How long one wave takes.", max_dur),
                "shape": enumeration("Sine, triangle or sawtooth.", &["sine", "triangle", "sawtooth"], "sine"),
                "for": duration("How long the pattern lasts.", max_dur)
            }), &["min", "max", "period", "for"], then)
        ]
    })
}

/// The JSON Schema (draft 2020-12) of a scenario, with this node's caps as
/// bounds. Every property carries a description, every number its bounds:
/// the console builder, the MCP tools and the parity test rely on it.
pub fn schema(caps: &Caps) -> Value {
    let max_dur = caps.max_duration_secs;
    let max_rate = caps.max_rate as f64;
    let pattern_then = json!({
        "$ref": "#/$defs/pattern",
        "description": "The next stage, started when this one ends (at most 16 stages in a chain)."
    });
    let pattern = pattern_schema(max_rate, max_dur, &pattern_then);
    let template = json!({
        "type": "object",
        "description": "A request to send. Placeholders: {{seq}} (request number), {{vu}} (virtual client), {{run}}, {{node}}, {{layer}}, {{rand}}.",
        "properties": {
            "method": enumeration("HTTP method.", &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"], "GET"),
            "path": json!({ "type": "string", "description": "Path with an optional query, appended to the target URL. Never a Rustybin control plane path.", "default": "/", "maxLength": 2048, "pattern": "^/" }),
            "headers": json!({
                "type": "object",
                "description": "Request headers (values may use placeholders).",
                "additionalProperties": { "type": "string", "description": "A header value (placeholders allowed).", "maxLength": 8192 },
                "maxProperties": MAX_HEADERS
            }),
            "body": json!({ "description": "Request body: a string is sent as is, any other JSON value is sent as JSON (Content-Type application/json unless a header says otherwise). At most 64 KB.", "type": ["string", "object", "array", "number", "boolean", "null"] }),
            "expect_status": json!({
                "type": "array",
                "description": "Statuses that count as expected (default: 2xx and 3xx). Anything else is an error in the report.",
                "items": integer("An HTTP status.", 100, 599),
                "maxItems": 32
            }),
            "weight": integer("Relative share when a layer has several requests.", 1, 1000)
        },
        "additionalProperties": false
    });
    let source = json!({
        "type": "object",
        "description": "Where the layer's requests come from: one request template, or several picked by weight.",
        "properties": {
            "request": template,
            "requests": { "type": "array", "description": "Several templates, picked by weight.", "items": { "$ref": "#/$defs/request" }, "maxItems": MAX_REQUESTS_PER_LAYER }
        },
        "additionalProperties": false
    });
    let layer = json!({
        "type": "object",
        "description": "One stream of traffic with its own source, model and pattern. Layers run at the same time and are reported separately.",
        "properties": {
            "name": string("Layer name (letters, digits, - _ .).", MAX_NAME_LEN),
            "source": { "$ref": "#/$defs/source" },
            "pattern": { "$ref": "#/$defs/pattern" },
            "model": enumeration("Open: requests leave at their intended times whatever the answers (latency is measured from the intended send). Closed: the pattern gives the number of users, each waiting for its answer plus a think time.", &["open", "closed"], "open"),
            "starts_after": duration("Delay before the layer starts, relative to the run.", 24 * 3600),
            "think_time": duration("Closed model: a user's pause between its answer and its next request.", 60)
        },
        "required": ["name", "source", "pattern"],
        "additionalProperties": false
    });
    let target = json!({
        "description": "A target URL, or an object with the URL and its options. Every target must be allowed by RUSTYBIN_HARNESS_TARGETS on the node.",
        "oneOf": [
            json!({ "type": "string", "description": "The target URL (http or https).", "maxLength": 2048, "format": "uri" }),
            {
                "type": "object",
                "description": "A target with options.",
                "properties": {
                    "url": json!({ "type": "string", "description": "The target URL (http or https), without query or fragment.", "maxLength": 2048, "format": "uri" }),
                    "weight": integer("Share of the requests this target gets.", 1, 1000),
                    "host": string("Host header and TLS server name to send instead of the URL's host.", 253),
                    "http": enumeration("Protocol: auto (ALPN on TLS, HTTP/1.1 on plain), h1, h2 (TLS only), h2c (prior knowledge on plain connections).", &["auto", "h1", "h2", "h2c"], "auto"),
                    "insecure": prop("boolean", "Skip TLS certificate verification (demo gateways with self-signed certificates).")
                },
                "required": ["url"],
                "additionalProperties": false
            }
        ]
    });
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": SCHEMA_ID,
        "title": "Rustybin harness scenario",
        "description": "Every parameter of a harness run: where to send, what to send, the shape of the load over time, the scale and the pass criteria. A scenario with one layer can be written with top-level source and pattern fields instead of layers.",
        "type": "object",
        "properties": {
            "name": string("Scenario name (letters, digits, space, - _ .).", MAX_NAME_LEN),
            "description": string("What the scenario shows.", MAX_DESCRIPTION_LEN),
            "target": json!({ "type": "string", "description": "Shorthand for one target URL.", "maxLength": 2048, "format": "uri", "x-shorthand": "targets" }),
            "targets": { "type": "array", "description": "The gateways (or gateway nodes) the load goes to, with weights.", "items": target, "maxItems": MAX_TARGETS },
            "scale": {
                "type": "object",
                "description": "How big the run is, apart from how hard it pushes.",
                "properties": {
                    "virtual_clients": integer("Distinct virtual clients ({{vu}} cycles through them; closed layers never run more users).", 1, MAX_VIRTUAL_CLIENTS as u64),
                    "max_rate_per_node": integer("A rate cap below the node's own (RUSTYBIN_HARNESS_MAX_RATE).", 1, caps.max_rate),
                    "client_nodes": {
                        "type": ["integer", "string"],
                        "description": "Cluster runs: the client nodes to wait for before the run starts (the rate is split over them by declared capacity), or auto: as many as the peak rate needs; the leader emits a scale_request while it waits.",
                        "oneOf": [
                            integer("A number of client nodes.", 1, MAX_CLUSTER_NODES as u64),
                            { "type": "string", "const": "auto", "description": "As many as the peak rate needs." }
                        ]
                    },
                    "server_nodes": integer("Cluster runs: the server nodes (the gateway's upstream pool) to wait for before the run starts.", 0, MAX_CLUSTER_NODES as u64),
                    "leader_sheds_load": prop("boolean", "Cluster runs: a node that takes over as leader during the run stops its own client part, and its share is re-split over the other client nodes (default true)."),
                    "connections_per_client": integer("Keep-alive connections per virtual client: the connections the gateway sees are this node's virtual clients times this (at most the in-flight cap).", 1, MAX_CONNECTIONS_PER_CLIENT as u64),
                    "scale_timeout": duration("With client_nodes: auto, how long to wait for enough client nodes before starting undersized (default 5m).", MAX_SCALE_TIMEOUT.as_secs())
                },
                "additionalProperties": false
            },
            "layers": { "type": "array", "description": "The traffic layers of the run.", "items": { "$ref": "#/$defs/layer" }, "maxItems": MAX_LAYERS },
            "source": { "$ref": "#/$defs/source", "description": "Shorthand: the source of a single layer.", "x-shorthand": "layers" },
            "pattern": { "$ref": "#/$defs/pattern", "description": "Shorthand: the pattern of a single layer.", "x-shorthand": "layers" },
            "model": shorthand(enumeration("Shorthand: the load model of a single layer.", &["open", "closed"], "open")),
            "think_time": shorthand(duration("Shorthand: the think time of a single closed layer.", 60)),
            "timeout": duration("Per-request timeout (default 10s).", 60),
            "thresholds": {
                "type": "object",
                "description": "Pass criteria: a run that misses any of them fails (exit code 1).",
                "properties": {
                    "error_rate": number("Most transport errors plus unexpected statuses, as a fraction of the requests sent.", 0.0, 1.0),
                    "not_sent": integer("Most requests the client may skip at its in-flight cap.", 0, u64::MAX / 2),
                    "latency_p50_ms": number("Highest median latency from the intended send time, in milliseconds.", 0.0, 3_600_000.0),
                    "latency_p99_ms": number("Highest p99 latency from the intended send time, in milliseconds.", 0.0, 3_600_000.0),
                    "latency_max_ms": number("Highest latency of any request, in milliseconds.", 0.0, 3_600_000.0),
                    "min_achieved": number("Least fraction of the requested requests that must have been answered.", 0.0, 1.0),
                    "saturated": prop("boolean", "false: fail when the client itself saturated (the results would describe the client, not the gateway)."),
                    "gateway_overhead_p99_ms": number("Highest p99 of the gateway overhead (total latency minus upstream processing time), in milliseconds.", 0.0, 3_600_000.0),
                    "lost": integer("Most requests the gateway may swallow: sent, seen by no server, no answer.", 0, u64::MAX / 2),
                    "upstream_reach": number("Least fraction of the sent requests that must have reached the server side.", 0.0, 1.0)
                },
                "additionalProperties": false
            },
            "servers": {
                "type": "object",
                "description": "The server side: how the shaped endpoint /harness/{*path} answers marked requests, and how the inspector samples harness traffic.",
                "properties": {
                    "latency": {
                        "type": "object",
                        "description": "Upstream processing time as a log-normal distribution given by its median and 99th percentile.",
                        "properties": {
                            "p50": duration("Median processing time.", 30),
                            "p99": duration("99th percentile of the processing time (at least the median).", 30)
                        },
                        "additionalProperties": false
                    },
                    "status": json!({
                        "type": "object",
                        "description": "Status codes with weights, for example {\"200\": 97, \"503\": 3}.",
                        "additionalProperties": integer("The weight of this status.", 1, 1000),
                        "maxProperties": MAX_SERVER_STATUSES
                    }),
                    "body_bytes": integer("Response body size: a JSON object padded to this length.", 0, MAX_SERVER_BODY as u64),
                    "chunked": prop("boolean", "Stream the body in chunks instead of one piece."),
                    "first_byte_delay": duration("Delay before the first byte, on top of the latency.", 30),
                    "capture_sample": integer("The inspector captures 1 in N marked requests, plus every anomaly (0: none).", 0, 1_000_000)
                },
                "additionalProperties": false
            }
        },
        "required": ["name"],
        "additionalProperties": false,
        "$defs": {
            "pattern": pattern,
            "request": template,
            "source": source,
            "layer": layer,
            "change": super::live::change_schema()
        },
        "x-caps": caps
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::pattern::{Curve, SyncMode};

    pub(crate) fn smoke(url: &str) -> Scenario {
        let mut s = Scenario::parse(&format!(
            "name: smoke\ntarget: {url}\nsource: {{ request: {{ path: /status/200 }} }}\npattern: {{ type: constant, rate: 50, for: 2s }}\n"
        ))
        .unwrap();
        s.normalise();
        s
    }

    #[test]
    fn durations() {
        for (text, ms) in [
            ("500ms", 500),
            ("30s", 30_000),
            ("2m", 120_000),
            ("1h", 3_600_000),
            ("1.5s", 1500),
            ("90", 90_000),
            (" 2 m ", 120_000),
        ] {
            assert_eq!(
                parse_duration(text),
                Some(Duration::from_millis(ms)),
                "{text}"
            );
        }
        for bad in ["", "-1s", "1w", "abc", "1 2", "1e400"] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
        assert_eq!(format_duration(Duration::from_secs(7200)), "2h");
        assert_eq!(format_duration(Duration::from_secs(90)), "90s");
        assert_eq!(format_duration(Duration::from_millis(1500)), "1500ms");
        assert_eq!(format_duration(Duration::from_micros(500)), "0.5ms");
        let d: Dur = serde_yaml::from_str("30s").unwrap();
        assert_eq!(d, Dur::secs(30));
        let d: Dur = serde_yaml::from_str("45").unwrap();
        assert_eq!(d, Dur::secs(45));
        assert!(serde_yaml::from_str::<Dur>("\"1w\"").is_err());
        assert_eq!(serde_yaml::to_string(&Dur::secs(60)).unwrap().trim(), "1m");
    }

    #[test]
    fn shorthand_is_folded_into_one_layer() {
        let s = smoke("http://127.0.0.1:1");
        assert!(s.target.is_none() && s.source.is_none() && s.pattern.is_none());
        assert_eq!(s.targets.len(), 1);
        assert_eq!(s.layers.len(), 1);
        assert_eq!(s.layers[0].name, "main");
        assert_eq!(s.duration(), Duration::from_secs(2));
        assert_eq!(s.peak_rate(), 50.0);
        let sum = s.summary();
        assert!((sum.expected_requests - 100.0).abs() < 0.01);
        assert!(
            s.validate(&Caps::default()).is_empty(),
            "{:?}",
            s.validate(&Caps::default())
        );
        assert_eq!(s.hash().len(), 64);
    }

    #[test]
    fn target_strings_and_objects() {
        let s = Scenario::parse(
            "name: t\ntargets:\n  - http://a:8080\n  - { url: https://b, weight: 3, host: api.b, http: h2, insecure: true }\n",
        )
        .unwrap();
        assert_eq!(s.targets[0], Target::new("http://a:8080"));
        assert_eq!(s.targets[1].weight, 3);
        assert_eq!(s.targets[1].http, HttpVersion::H2);
        assert!(s.targets[1].insecure);
        let text = serde_yaml::to_string(&s).unwrap();
        let back = Scenario::parse(&text).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn unknown_fields_and_sources_are_refused_by_name() {
        let err = Scenario::parse("name: x\nchaos: { preset: mad }\n").unwrap_err();
        assert!(err.contains("chaos"), "{err}");
        let err = Scenario::parse("name: x\nsource: { mock: petstore }\n").unwrap_err();
        assert!(err.contains("mock"), "{err}");
        assert!(Scenario::parse("").is_err());
        assert!(Scenario::parse(&"x".repeat(MAX_SCENARIO_BYTES + 1)).is_err());
        assert!(Scenario::from_value(&json!([1])).is_err());
    }

    #[test]
    fn validation_lists_every_problem() {
        let mut s = Scenario::parse(
            "name: ''\ntargets: [ 'ftp://x', { url: 'http://gw/_rustybin', weight: 0 }, { url: 'http://plain', http: h2 } ]\nscale: { virtual_clients: 0, max_rate_per_node: 999999 }\nlayers:\n  - name: 'bad name!'\n    source: {}\n    pattern: { type: constant, rate: 100000, for: 30h }\n    think_time: 90s\n  - name: a\n    source: { request: { method: FETCH, path: 'nope', headers: { 'bad header': 'x', 'ok': \"line\\nbreak\" }, body: '{{nope}}', expect_status: [99], weight: 0 } }\n    pattern: { type: ramp, from: 1, to: 2, over: 1s }\n  - name: a\n    model: closed\n    source: { request: { path: '/ui/{{seq}}' } }\n    pattern: { type: constant, rate: 5, for: 1s }\ntimeout: 5m\nthresholds: { error_rate: 2, latency_p99_ms: -1, min_achieved: 1.5 }\n",
        )
        .unwrap();
        s.normalise();
        let caps = Caps {
            max_rate: 1000,
            max_inflight: 100,
            max_duration_secs: 3600,
        };
        let problems = s.validate(&caps);
        let text = problems.join("\n");
        for needle in [
            "name: required",
            "targets[0].url",
            "targets[1].url: the target URL points at a Rustybin control plane",
            "targets[1].weight",
            "targets[2].http: h2 needs an https target",
            "scale.virtual_clients",
            "scale.max_rate_per_node: 999999 is over",
            "layers[0].name",
            "layers[0].source: a request",
            "layers[0].think_time: only a closed layer",
            "layers[0].think_time: at most 60s",
            "layers[1].source.request.method",
            "layers[1].source.request.path: must start with /",
            "layers[1].source.request.headers.bad header",
            "layers[1].source.request.headers.ok",
            "layers[1].source.request.body: unknown placeholder {{nope}}",
            "layers[1].source.request.expect_status",
            "layers[1].source.request.weight",
            "layers[2].name: duplicate",
            "layers[2].source.request.path: a run never targets a Rustybin control plane",
            "layers[2].pattern: a closed layer runs at most scale.virtual_clients",
            "layers: the peak rates add up to",
            "layers: the run would last 30h",
            "timeout: 1ms to 60s",
            "thresholds.error_rate",
            "thresholds.latency_p99_ms",
            "thresholds.min_achieved",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
    }

    #[test]
    fn placeholders_are_found() {
        assert_eq!(placeholders("/a/{{seq}}/{{ vu }}"), vec!["seq", "vu"]);
        assert_eq!(placeholders("/a/{{open"), Vec::<String>::new());
        assert_eq!(strip_placeholders("/{{a}}/_rustybin"), "/x/_rustybin");
    }

    /// A scenario with every field set, for the schema parity test.
    pub(crate) fn full() -> Scenario {
        let template = RequestTemplate {
            method: "POST".into(),
            path: "/echo/{{seq}}".into(),
            headers: BTreeMap::from([("X-Vu".to_string(), "{{vu}}".to_string())]),
            body: Some(json!({ "n": 1 })),
            expect_status: vec![200, 404],
            weight: 2,
        };
        Scenario {
            name: "full".into(),
            description: Some("every field".into()),
            target: Some("http://127.0.0.1:1".into()),
            targets: vec![Target {
                url: "http://127.0.0.1:2".into(),
                weight: 2,
                host: Some("api.example".into()),
                http: HttpVersion::H1,
                insecure: true,
            }],
            scale: Scale {
                virtual_clients: 10,
                max_rate_per_node: Some(100),
                client_nodes: Some(NodeCount::Auto(AutoWord::Auto)),
                server_nodes: Some(1),
                leader_sheds_load: Some(false),
                connections_per_client: Some(2),
                scale_timeout: Some(Dur::secs(30)),
            },
            layers: vec![
                Layer {
                    name: "l".into(),
                    source: Source {
                        request: Some(template.clone()),
                        requests: vec![template.clone()],
                    },
                    pattern: Pattern::Ramp {
                        from: 1.0,
                        to: 2.0,
                        over: Dur::secs(1),
                        curve: Curve::Exponential,
                        jitter: Some(0.1),
                        sync: Some(SyncMode::Node),
                        then: Some(Box::new(Pattern::Constant {
                            rate: 2.0,
                            hold: Dur::secs(1),
                            jitter: None,
                            sync: None,
                            then: Some(Box::new(
                                serde_yaml::from_str(
                                    "type: target\nusers: 2\nwithin: 1s\nhold: 1s\nstart: 1",
                                )
                                .expect("target pattern"),
                            )),
                        })),
                    },
                    model: Model::Closed,
                    starts_after: Some(Dur::secs(1)),
                    think_time: Some(Dur::secs(1)),
                },
                Layer {
                    name: "m".into(),
                    source: Source {
                        request: Some(template.clone()),
                        requests: Vec::new(),
                    },
                    // Every other pattern type, chained.
                    pattern: serde_yaml::from_str(
                        "type: step\nstart: 1\nstep: 1\nevery: 1s\nmax: 3\nhold: 1s\nthen:\n  type: spike\n  base: 1\n  peak: 5\n  rise: 1s\n  hold: 1s\n  fall: 1s\n  recover: 1s\n  then:\n    type: pulse\n    high: 4\n    low: 1\n    on: 1s\n    off: 1s\n    cycles: 2\n    then:\n      type: burst\n      base: 1\n      size: 2\n      within: 100ms\n      every: 1s\n      for: 2s\n      then:\n        type: wave\n        min: 1\n        max: 4\n        period: 2s\n        shape: triangle\n        for: 2s\n        then:\n          type: breakpoint\n          start: 1\n          growth: 0.5\n          every: 1s\n          max: 5\n          until: { error_rate: 0.01, latency_p99_ms: 500, overhead_p99_ms: 50 }\n          then:\n            type: target\n            overhead_p99_ms: 5\n            within: 1s\n            hold: 1s\n            start: 1\n            max: 10\n",
                    )
                    .expect("chained patterns"),
                    model: Model::Open,
                    starts_after: None,
                    think_time: None,
                },
            ],
            source: Some(Source {
                request: Some(template),
                requests: Vec::new(),
            }),
            pattern: Some(Pattern::Constant {
                rate: 1.0,
                hold: Dur::secs(1),
                jitter: None,
                sync: None,
                then: None,
            }),
            model: Some(Model::Closed),
            think_time: Some(Dur::secs(1)),
            timeout: Some(Dur::secs(5)),
            thresholds: Thresholds {
                error_rate: Some(0.1),
                not_sent: Some(0),
                latency_p50_ms: Some(1.0),
                latency_p99_ms: Some(2.0),
                latency_max_ms: Some(3.0),
                min_achieved: Some(0.9),
                saturated: Some(false),
                gateway_overhead_p99_ms: Some(10.0),
                lost: Some(0),
                upstream_reach: Some(0.99),
            },
            servers: ServerProfile {
                latency: ServerLatency {
                    p50: Dur(Duration::from_millis(5)),
                    p99: Dur(Duration::from_millis(50)),
                },
                status: BTreeMap::from([("200".to_string(), 9), ("503".to_string(), 1)]),
                body_bytes: 512,
                chunked: true,
                first_byte_delay: Dur(Duration::from_millis(1)),
                capture_sample: 10,
            },
        }
    }

    /// Resolve `$ref`s against the root schema.
    fn resolve<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
        match node.get("$ref").and_then(Value::as_str) {
            Some(r) => {
                let path = r.trim_start_matches("#/");
                let mut cur = root;
                for seg in path.split('/') {
                    cur = &cur[seg];
                }
                resolve(root, cur)
            }
            None => node,
        }
    }

    /// The schema for a property name inside `node` (object properties,
    /// `additionalProperties`, or the matching `oneOf` branch).
    fn property<'a>(
        root: &'a Value,
        node: &'a Value,
        key: &str,
        value: &Value,
    ) -> Option<&'a Value> {
        let node = resolve(root, node);
        if let Some(p) = node.get("properties").and_then(|p| p.get(key)) {
            return Some(p);
        }
        if let Some(ap) = node.get("additionalProperties").filter(|v| v.is_object()) {
            return Some(ap);
        }
        if let Some(branches) = node.get("oneOf").and_then(Value::as_array) {
            for b in branches {
                let b = resolve(root, b);
                let matches_type = match value.get("type").and_then(Value::as_str) {
                    Some(t) => b
                        .pointer("/properties/type/const")
                        .and_then(Value::as_str)
                        .is_none_or(|c| c == t),
                    None => true,
                };
                if matches_type {
                    if let Some(p) = b.get("properties").and_then(|p| p.get(key)) {
                        return Some(p);
                    }
                }
            }
        }
        None
    }

    fn walk(root: &Value, node: &Value, value: &Value, path: &str, missing: &mut Vec<String>) {
        let node = resolve(root, node);
        match value {
            Value::Object(map) => {
                // The matching oneOf branch: by `type` when the value has one
                // (patterns), else the object branch (targets).
                let node = match node.get("oneOf").and_then(Value::as_array) {
                    Some(branches) if node.get("properties").is_none() => {
                        let wanted = value.get("type").and_then(Value::as_str);
                        branches
                            .iter()
                            .map(|b| resolve(root, b))
                            .find(|b| match wanted {
                                Some(t) => {
                                    b.pointer("/properties/type/const").and_then(Value::as_str)
                                        == Some(t)
                                }
                                None => b["type"] == "object" || b.get("properties").is_some(),
                            })
                            .unwrap_or(node)
                    }
                    _ => node,
                };
                // A free-form object (a request body): anything goes.
                if node.get("properties").is_none()
                    && node.get("additionalProperties").is_none()
                    && node.get("oneOf").is_none()
                {
                    return;
                }
                for (k, v) in map {
                    let here = format!("{path}.{k}");
                    match property(root, node, k, value) {
                        Some(p) => walk(root, p, v, &here, missing),
                        None => missing.push(here),
                    }
                }
            }
            Value::Array(items) => {
                let Some(item_schema) = node.get("items") else {
                    missing.push(format!("{path}[] (no items)"));
                    return;
                };
                for (i, v) in items.iter().enumerate() {
                    walk(root, item_schema, v, &format!("{path}[{i}]"), missing);
                }
            }
            _ => {}
        }
    }

    fn check_described(
        root: &Value,
        node: &Value,
        path: &str,
        problems: &mut Vec<String>,
        seen: &mut Vec<String>,
    ) {
        if let Some(r) = node.get("$ref").and_then(Value::as_str) {
            if seen.contains(&r.to_string()) {
                return;
            }
            seen.push(r.to_string());
            check_described(root, resolve(root, node), path, problems, seen);
            return;
        }
        if node.get("description").is_none() {
            problems.push(format!("{path}: no description"));
        }
        let ty = node.get("type");
        let is_number = ty.is_some_and(|t| t == "number" || t == "integer")
            || ty
                .and_then(Value::as_array)
                .is_some_and(|a| a.iter().all(|t| t == "number" || t == "integer"))
            || node.get("format").is_some_and(|f| f == "duration");
        if is_number && (node.get("minimum").is_none() || node.get("maximum").is_none()) {
            problems.push(format!("{path}: a number without minimum and maximum"));
        }
        if let Some(props) = node.get("properties").and_then(Value::as_object) {
            for (k, v) in props {
                check_described(root, v, &format!("{path}.{k}"), problems, seen);
            }
        }
        if let Some(ap) = node.get("additionalProperties").filter(|v| v.is_object()) {
            check_described(root, ap, &format!("{path}.*"), problems, seen);
        }
        if let Some(items) = node.get("items") {
            check_described(root, items, &format!("{path}[]"), problems, seen);
        }
        if let Some(branches) = node.get("oneOf").and_then(Value::as_array) {
            for (i, b) in branches.iter().enumerate() {
                check_described(root, b, &format!("{path}|{i}"), problems, seen);
            }
        }
    }

    /// Parity (plan section 6.6): every field of the model is in the schema,
    /// and every schema property has a description and, for numbers, bounds.
    #[test]
    fn schema_covers_the_model_and_describes_everything() {
        let root = schema(&Caps::default());
        let value = serde_json::to_value(full()).unwrap();
        let mut missing = Vec::new();
        walk(&root, &root, &value, "scenario", &mut missing);
        assert!(
            missing.is_empty(),
            "model fields missing from the schema: {missing:#?}"
        );
        let mut problems = Vec::new();
        let mut seen = Vec::new();
        check_described(&root, &root, "schema", &mut problems, &mut seen);
        for (name, def) in root["$defs"].as_object().unwrap() {
            check_described(
                &root,
                def,
                &format!("$defs.{name}"),
                &mut problems,
                &mut seen,
            );
        }
        assert!(problems.is_empty(), "{problems:#?}");
        assert_eq!(root["x-caps"]["max_rate"], 50_000);
    }

    #[test]
    fn full_scenario_normalises_and_validates() {
        let mut s = full();
        s.normalise();
        assert_eq!(s.targets.len(), 2);
        assert_eq!(s.layers.len(), 3);
        assert_eq!(s.layers[0].name, "main");
        let caps = Caps::default();
        let problems = s.validate(&caps);
        assert!(problems.is_empty(), "{problems:#?}");
    }
}
