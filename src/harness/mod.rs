//! Harness mode: Rustybin as a load client and correlating upstream
//! (`docs/plans/harness.md`). Phase 1, the single-node engine: a node with
//! the `client` role (or `both`, loop-through) runs scenarios against
//! allowlisted targets and reports what the client saw.
//!
//! - [`safety`]: the allowlist, address pinning, forbidden addresses, the
//!   kill switch and target verification. It comes first: this is the first
//!   feature that sends load to targets a caller chooses.
//! - [`scenario`]: the parameter model, its validation against the node's
//!   caps and the JSON Schema every surface (API, MCP, console) reads.
//! - [`pattern`]: the load curves (`constant`, `ramp`, chained with `then`).
//! - [`engine`]: the runtime, the scheduler thread, pinned connections,
//!   per-worker histograms and saturation detection.
//! - [`report`]: the final report as JSON, text and JUnit.
//! - [`api`]: the handlers under `/_rustybin/harness*`.
//! - [`cli`]: `rustybin harness run | validate | report`.
//!
//! Off by default (`RUSTYBIN_HARNESS=on`); refused with public mode, with
//! self-service mocks and for a client without `RUSTYBIN_HARNESS_TARGETS`
//! ([`check_startup`], fatal). The control routes are protected like the
//! rest of the control plane and need the `harness` scope (or admin) to
//! start and stop runs.

pub mod api;
pub mod cli;
pub mod correlate;
pub mod engine;
pub mod pattern;
pub mod report;
pub mod safety;
pub mod scenario;
pub mod shaped;
#[cfg(test)]
pub mod test_proxy;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::config::{Config, HarnessConfig};
use crate::state::AppState;
use correlate::{Correlator, RunKey};
use engine::{Engine, PinnedTarget, Run, RunParams};
use safety::{Allowlist, KillSwitch};
use scenario::{Caps, Scenario};

/// Prefix of every harness control route.
pub const PREFIX: &str = "/_rustybin/harness";
/// Where a node serves its verification token (data plane).
pub const WELL_KNOWN_PATH: &str = "/.well-known/rustybin-harness";
/// The control API version reported in the status.
pub const HARNESS_API: u32 = 1;
/// Runs kept per node (the oldest finished run is dropped first).
pub const MAX_RUNS_KEPT: usize = 20;
/// A finished run is forgotten after this long.
pub const RUN_TTL: Duration = Duration::from_secs(24 * 3600);
/// Time allowed for one target's verification request.
pub const VERIFY_TIMEOUT: Duration = Duration::from_secs(5);

/// A node's roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Generates load.
    Client,
    /// An upstream that correlates and reports (phase 2).
    Server,
    /// Owns a cluster (phase 3). A single node is its own coordinator.
    Coordinator,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Client => "client",
            Role::Server => "server",
            Role::Coordinator => "coordinator",
        }
    }

    /// `client,server`, `both`, `coordinator`... Returns the unknown word.
    pub fn parse_list(raw: &str) -> Result<Vec<Role>, String> {
        let mut roles = Vec::new();
        for word in raw.split(',') {
            let word = word.trim().to_ascii_lowercase();
            if word.is_empty() {
                continue;
            }
            let found: &[Role] = match word.as_str() {
                "client" => &[Role::Client],
                "server" => &[Role::Server],
                "coordinator" | "leader" => &[Role::Coordinator],
                "both" => &[Role::Client, Role::Server],
                _ => return Err(word),
            };
            for r in found {
                if !roles.contains(r) {
                    roles.push(*r);
                }
            }
        }
        if roles.is_empty() {
            return Err(raw.to_string());
        }
        Ok(roles)
    }
}

/// `RUSTYBIN_HARNESS_START`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StartMode {
    #[default]
    Manual,
    Immediately,
    /// Cluster mode (phase 3). On a single node the quorum is the node itself.
    OnQuorum,
}

impl StartMode {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "manual" => Some(Self::Manual),
            "immediately" | "immediate" | "now" => Some(Self::Immediately),
            "on-quorum" | "on_quorum" | "quorum" => Some(Self::OnQuorum),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Immediately => "immediately",
            Self::OnQuorum => "on-quorum",
        }
    }
}

/// `/_rustybin/harness` and everything below it.
pub fn is_harness_path(path: &str) -> bool {
    path == PREFIX
        || path
            .strip_prefix(PREFIX)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The combinations refused at startup (fatal, not a warning): harness mode
/// in public mode, with self-service mocks, or a client without targets.
pub fn check_startup(config: &Config) -> Result<(), String> {
    let h = &config.harness;
    if !h.enabled {
        return Ok(());
    }
    if config.public_mode {
        return Err(
            "RUSTYBIN_HARNESS=on is refused with RUSTYBIN_PUBLIC_MODE (a public instance never generates load)"
                .into(),
        );
    }
    if config.self_service.enabled {
        return Err(
            "RUSTYBIN_HARNESS=on is refused with RUSTYBIN_MOCKS_SELF_SERVICE (a self-service instance never generates load)"
                .into(),
        );
    }
    if h.has_role(Role::Client) {
        let (allowlist, warnings) = Allowlist::parse(&h.targets);
        if allowlist.is_empty() {
            let detail = if warnings.is_empty() {
                String::new()
            } else {
                format!(" ({})", warnings.join("; "))
            };
            return Err(format!(
                "the harness client role needs RUSTYBIN_HARNESS_TARGETS, the hosts, domains, IPs or CIDRs it may send load to; nothing is allowed without it{detail}"
            ));
        }
    }
    Ok(())
}

/// Why a run could not be created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunError {
    pub status: u16,
    pub error: String,
    pub problems: Vec<String>,
}

impl RunError {
    pub fn new(status: u16, error: impl Into<String>) -> Self {
        Self {
            status,
            error: error.into(),
            problems: Vec::new(),
        }
    }

    pub fn invalid(problems: Vec<String>) -> Self {
        Self {
            status: 400,
            error: format!(
                "the scenario has {} problem{}",
                problems.len(),
                if problems.len() == 1 { "" } else { "s" }
            ),
            problems,
        }
    }

    pub fn to_json(&self) -> Value {
        let mut v = json!({ "error": self.error });
        if !self.problems.is_empty() {
            v["problems"] = json!(self.problems);
        }
        v
    }
}

/// The harness of one node: configuration, the engine (client role), the
/// runs it keeps, the kill switch and the verification token.
pub struct Harness {
    pub config: HarnessConfig,
    pub node_id: String,
    pub allowlist: Allowlist,
    pub allowlist_warnings: Vec<String>,
    engine: Option<Engine>,
    engine_error: Option<String>,
    pub kill: Arc<KillSwitch>,
    runs: Mutex<Vec<Arc<Run>>>,
    /// Served at [`WELL_KNOWN_PATH`]; targets must echo it when
    /// `verify_targets` is on. Random per process.
    pub verification_token: String,
    /// The server side: run keys, server profiles and what arrived.
    pub correlator: Arc<Correlator>,
    metrics: Option<Arc<crate::metrics::Metrics>>,
    started: Instant,
}

/// A node id safe for request ids: letters, digits, `.`, `_`, `-`, at most 32.
pub fn node_id_from(instance_id: &str) -> String {
    let cleaned: String = instance_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(32)
        .collect();
    if cleaned.is_empty() {
        "node".into()
    } else {
        cleaned
    }
}

fn random_hex(bytes: usize) -> String {
    (0..bytes)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect()
}

impl Harness {
    /// The node's harness, `None` when harness mode is off.
    pub fn new(
        config: &Config,
        metrics: Option<Arc<crate::metrics::Metrics>>,
    ) -> Option<Arc<Self>> {
        if !config.harness.enabled {
            return None;
        }
        Some(Self::from_harness_config(
            &config.harness,
            &config.instance_id,
            metrics,
        ))
    }

    /// Build from the harness settings alone (the CLI has no `AppState`).
    pub fn from_harness_config(
        hc: &HarnessConfig,
        instance_id: &str,
        metrics: Option<Arc<crate::metrics::Metrics>>,
    ) -> Arc<Self> {
        let (allowlist, allowlist_warnings) = Allowlist::parse(&hc.targets);
        for w in &allowlist_warnings {
            tracing::warn!("{w}");
        }
        let (engine, engine_error) = if hc.has_role(Role::Client) {
            match Engine::new(hc) {
                Ok(e) => {
                    tracing::info!(
                        "harness engine: {} thread(s), in-flight cap {} (descriptor limit {})",
                        e.threads,
                        e.inflight_cap,
                        e.descriptor_limit
                            .map_or("unknown".to_string(), |d| d.to_string())
                    );
                    (Some(e), None)
                }
                Err(e) => {
                    tracing::error!("{e}");
                    (None, Some(e))
                }
            }
        } else {
            (None, None)
        };
        Arc::new(Self {
            config: hc.clone(),
            node_id: node_id_from(instance_id),
            allowlist,
            allowlist_warnings,
            engine,
            engine_error,
            kill: KillSwitch::new(),
            runs: Mutex::new(Vec::new()),
            verification_token: random_hex(16),
            correlator: Correlator::new(),
            metrics,
            started: Instant::now(),
        })
    }

    pub fn is_client(&self) -> bool {
        self.config.has_role(Role::Client)
    }

    pub fn is_server(&self) -> bool {
        self.config.has_role(Role::Server)
    }

    /// The caps scenarios are validated against on this node.
    pub fn caps(&self) -> Caps {
        match &self.engine {
            Some(e) => e.caps(&self.config),
            None => Caps::from_config(&self.config),
        }
    }

    /// The runs this node keeps, newest last (expired ones swept).
    pub fn runs(&self) -> Vec<Arc<Run>> {
        let mut runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
        runs.retain(|r| r.is_active() || r.created.elapsed() < RUN_TTL);
        runs.clone()
    }

    pub fn run(&self, id: &str) -> Option<Arc<Run>> {
        self.runs().into_iter().find(|r| r.id == id)
    }

    /// The run that is not finished, if any (one at a time per node).
    pub fn active_run(&self) -> Option<Arc<Run>> {
        self.runs().into_iter().find(|r| r.is_active())
    }

    /// The kill switch: stop every run now. Returns the ids stopped.
    pub fn stop_all(&self) -> Vec<String> {
        self.kill.set();
        let stopped: Vec<String> = self
            .runs()
            .iter()
            .filter(|r| r.is_active())
            .map(|r| {
                r.stop("kill_switch");
                r.id.clone()
            })
            .collect();
        tracing::warn!(
            target: "rustybin::harness",
            runs = ?stopped,
            "harness kill switch: all local load stopped"
        );
        // Runs check the switch on their next tick; it is cleared once they
        // have seen it, so the next run may start.
        let kill = self.kill.clone();
        std::thread::spawn(move || {
            std::thread::sleep(engine::MAX_SLEEP * 4);
            kill.clear();
        });
        stopped
    }

    /// Status for `GET /_rustybin/harness` and the MCP tool.
    pub fn status(&self) -> Value {
        let active = self.active_run();
        json!({
            "enabled": true,
            "harness_api": HARNESS_API,
            "node_id": self.node_id,
            "roles": self.config.roles.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
            "caps": self.caps(),
            "engine": self.engine.as_ref().map(Engine::describe),
            "engine_error": self.engine_error,
            "allowlist": self.allowlist.describe(),
            "allowlist_warnings": self.allowlist_warnings,
            "verify_targets": self.config.verify_targets,
            "active_run": active.map(|r| r.summary()),
            "runs": self.runs().len(),
            "kill_switch": self.kill.is_set(),
            "rejected_markers": self.correlator.rejected(),
            "uptime_secs": self.started.elapsed().as_secs(),
            "report_dir_configured": self.config.report_dir.is_some(),
            "headless": self.config.headless(),
            "cluster": Value::Null,
            "leader": Value::Null,
        })
    }

    /// Validate a scenario (normalised in place) against this node.
    pub fn validate(&self, scenario: &mut Scenario) -> Vec<String> {
        scenario.normalise();
        scenario.validate(&self.caps())
    }

    /// Validate, pin and verify the targets, then start the run on its
    /// scheduler thread. Only one run is active at a time per node.
    pub async fn create_run(
        self: &Arc<Self>,
        mut scenario: Scenario,
    ) -> Result<Arc<Run>, RunError> {
        if !self.is_client() {
            return Err(RunError::new(
                403,
                "this node has no client role (RUSTYBIN_HARNESS_ROLES=client or both)",
            ));
        }
        let Some(engine) = &self.engine else {
            return Err(RunError::new(
                503,
                self.engine_error
                    .clone()
                    .unwrap_or_else(|| "the harness engine is not running".into()),
            ));
        };
        let problems = self.validate(&mut scenario);
        if !problems.is_empty() {
            return Err(RunError::invalid(problems));
        }
        if self.kill.is_set() {
            return Err(RunError::new(
                409,
                "the kill switch is set: runs are stopping, try again in a moment",
            ));
        }
        if let Some(active) = self.active_run() {
            return Err(RunError {
                status: 409,
                error: format!(
                    "run {} is {} on this node; one run at a time (stop it with POST {PREFIX}/runs/{}/stop)",
                    active.id,
                    active.state().as_str(),
                    active.id
                ),
                problems: Vec::new(),
            });
        }
        let mut targets = Vec::with_capacity(scenario.targets.len());
        let mut refused = Vec::new();
        for (i, t) in scenario.targets.iter().enumerate() {
            let parsed = match safety::parse_target_url(&t.url) {
                Ok(p) => p,
                Err(e) => {
                    refused.push(format!("targets[{i}].url: {e}"));
                    continue;
                }
            };
            match safety::pin(&parsed, &self.allowlist).await {
                Ok(addrs) => match PinnedTarget::new(
                    &engine.handle(),
                    t.clone(),
                    parsed,
                    addrs,
                    engine.inflight_cap,
                ) {
                    Ok(pt) => targets.push(pt),
                    Err(e) => refused.push(format!("targets[{i}]: {e}")),
                },
                Err(e) => refused.push(format!("targets[{i}].url: {e}")),
            }
        }
        if !refused.is_empty() {
            return Err(RunError {
                status: 403,
                error: "a target is not allowed on this node".into(),
                problems: refused,
            });
        }
        if self.config.verify_targets {
            for target in &mut targets {
                self.verify_target(target).await.map_err(|e| RunError {
                    status: 403,
                    error: "target verification failed".into(),
                    problems: vec![e],
                })?;
                target.verified = true;
            }
        }
        let id = random_hex(6);
        // The run key signs the markers; this node's server side learns it
        // now (cluster nodes get it from the leader in phase 3).
        let key = RunKey::random();
        let entry = self
            .correlator
            .register(&id, key.clone(), scenario.servers.clone());
        let run = Run::new(RunParams {
            id: id.clone(),
            node_id: self.node_id.clone(),
            scenario,
            targets,
            caps: self.caps(),
            threads: engine.threads,
            inflight_cap: engine.inflight_cap,
            descriptor_limit: engine.descriptor_limit,
            report_dir: self.config.report_dir.clone(),
            kill: self.kill.clone(),
            key,
            entry: self.is_server().then_some(entry),
            correlator: self.correlator.clone(),
            metrics: self.metrics.clone(),
        });
        {
            let mut runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
            runs.retain(|r| r.is_active() || r.created.elapsed() < RUN_TTL);
            while runs.len() >= MAX_RUNS_KEPT {
                match runs.iter().position(|r| !r.is_active()) {
                    Some(i) => {
                        runs.remove(i);
                    }
                    None => break,
                }
            }
            runs.push(run.clone());
        }
        tracing::info!(
            target: "rustybin::harness",
            run = %id,
            scenario = %run.scenario.name,
            hash = %run.hash,
            targets = run.targets.len(),
            layers = run.scenario.layers.len(),
            planned_secs = run.scenario.duration().as_secs_f64(),
            peak_rate = run.scenario.peak_rate(),
            "harness run starting"
        );
        engine::start(run.clone(), engine.handle()).map_err(|e| RunError::new(500, e))?;
        Ok(run)
    }

    /// `GET <target>/.well-known/rustybin-harness` must answer this node's
    /// token (a gateway routing to this node does; so does the node itself).
    async fn verify_target(&self, target: &PinnedTarget) -> Result<(), String> {
        let uri: hyper::Uri = format!(
            "{}{}{WELL_KNOWN_PATH}",
            target.authority, target.parsed.path
        )
        .parse()
        .map_err(|e| format!("{}: verification URI: {e}", target.target.url))?;
        let req = hyper::Request::builder()
            .method("GET")
            .uri(uri)
            .header(
                "user-agent",
                concat!("rustybin-harness/", env!("CARGO_PKG_VERSION")),
            )
            .body(http_body_util::Full::new(bytes::Bytes::new()))
            .map_err(|e| format!("verification request: {e}"))?;
        let client = target.client.clone();
        let url = target.target.url.clone();
        let fut = async move {
            let resp = client
                .request(req)
                .await
                .map_err(|e| format!("{url}: verification request failed: {e}"))?;
            let status = resp.status();
            let body = http_body_util::Limited::new(resp.into_body(), 4096);
            let bytes = http_body_util::BodyExt::collect(body)
                .await
                .map(|c| c.to_bytes())
                .map_err(|_| format!("{url}: verification body unreadable"))?;
            Ok::<(axum::http::StatusCode, bytes::Bytes), String>((status, bytes))
        };
        let (status, bytes) = tokio::time::timeout(VERIFY_TIMEOUT, fut)
            .await
            .map_err(|_| format!("{}: verification timed out", target.target.url))??;
        if !status.is_success() {
            return Err(format!(
                "{}: {WELL_KNOWN_PATH} answered {status}, expected the node's verification token (is the gateway routing to this cluster?)",
                target.target.url
            ));
        }
        let text = String::from_utf8_lossy(&bytes).trim().to_string();
        if !crate::control_auth::constant_time_eq(&text, &self.verification_token) {
            return Err(format!(
                "{}: {WELL_KNOWN_PATH} does not serve this node's verification token (another instance, or not routed to this cluster)",
                target.target.url
            ));
        }
        Ok(())
    }
}

/// Headless mode (`RUSTYBIN_HARNESS_SCENARIO` with `RUSTYBIN_HARNESS_START`
/// other than `manual`): run the scenario file, write the report where
/// `RUSTYBIN_HARNESS_REPORT` says (`-` for stdout) and return the exit code
/// (0 when the thresholds pass, 1 when they fail, 2 on an error).
pub async fn run_headless(harness: &Arc<Harness>) -> u8 {
    let hc = &harness.config;
    let Some(path) = &hc.scenario else {
        return 2;
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("headless harness: cannot read {}: {e}", path.display());
            return 2;
        }
    };
    let scenario = match Scenario::parse(&text) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("headless harness: {e}");
            return 2;
        }
    };
    let run = match harness.create_run(scenario).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("headless harness: {} {}", e.error, e.problems.join("; "));
            return 2;
        }
    };
    run.wait().await;
    let Some(report) = run.report() else {
        return 2;
    };
    match hc.report.as_deref() {
        Some("-") => {
            use std::io::Write;
            let _ = writeln!(
                std::io::stdout(),
                "{}",
                serde_json::to_string_pretty(report).unwrap_or_default()
            );
        }
        Some(file) => {
            if let Err(e) =
                std::fs::write(file, serde_json::to_vec_pretty(report).unwrap_or_default())
            {
                tracing::error!("headless harness: cannot write the report to {file}: {e}");
            }
        }
        None => {}
    }
    tracing::info!(
        "headless harness run {} {}: {} sent, {} errors, p99 {:.2} ms, {}",
        run.id,
        report.state,
        report.totals.sent,
        report.totals.errors(),
        report.latency.p99_ms,
        if report.passed {
            "thresholds pass"
        } else {
            "thresholds FAIL"
        }
    );
    report.exit_code
}

/// The built-in scenarios (name, YAML), a starting point to copy and tune.
pub fn builtin_scenarios() -> &'static [(&'static str, &'static str)] {
    &[
        ("smoke", include_str!("scenarios/smoke.yaml")),
        (
            "ramp-to-target",
            include_str!("scenarios/ramp-to-target.yaml"),
        ),
        ("soak", include_str!("scenarios/soak.yaml")),
        ("closed-users", include_str!("scenarios/closed-users.yaml")),
    ]
}

/// One built-in scenario parsed.
pub fn builtin_scenario(name: &str) -> Option<Scenario> {
    builtin_scenarios()
        .iter()
        .find(|(n, _)| *n == name)
        .and_then(|(_, text)| Scenario::parse(text).ok())
}

/// The routes. Handlers answer 404 when harness mode is off, except the
/// status, the schema, the built-in scenarios and validation, which send
/// nothing and work on any node.
pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/_rustybin/harness", get(api::node_status))
        .route("/_rustybin/harness/schema", get(api::schema))
        .route("/_rustybin/harness/scenarios", get(api::list_scenarios))
        .route(
            "/_rustybin/harness/scenarios/{name}",
            get(api::get_scenario),
        )
        .route("/_rustybin/harness/validate", post(api::validate))
        .route(
            "/_rustybin/harness/runs",
            get(api::list_runs).post(api::create_run),
        )
        .route("/_rustybin/harness/runs/{run}", get(api::get_run))
        .route("/_rustybin/harness/runs/{run}/stop", post(api::stop_run))
        .route("/_rustybin/harness/runs/{run}/report", get(api::run_report))
        .route("/_rustybin/harness/stop", post(api::stop_all))
        .route(WELL_KNOWN_PATH, get(api::well_known))
        .route("/harness", axum::routing::any(shaped::handler))
        .route("/harness/{*path}", axum::routing::any(shaped::handler))
}

const SMOKE_JSON: &str = r#"{"name":"smoke","target":"http://127.0.0.1:8080","source":{"request":{"path":"/status/200"}},"pattern":{"type":"constant","rate":50,"for":"5s"}}"#;

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/_rustybin/harness",
            &["GET"],
            category::HARNESS,
            "Harness node status: roles, caps, allowlist, the active run",
        )
        .description(
            "Answers on every node (enabled: false when harness mode is off). Harness mode \
             (RUSTYBIN_HARNESS=on) makes this Rustybin a load client against allowlisted targets, \
             see docs/plans/harness.md.",
        )
        .example(Example::get("Node status", "/_rustybin/harness")),
        Endpoint::new(
            "/_rustybin/harness/schema",
            &["GET"],
            category::HARNESS,
            "JSON Schema of a scenario, with this node's caps as bounds",
        )
        .example(Example::get("Scenario schema", "/_rustybin/harness/schema")),
        Endpoint::new(
            "/_rustybin/harness/scenarios",
            &["GET"],
            category::HARNESS,
            "The built-in scenarios (smoke, ramp-to-target, soak, closed-users)",
        )
        .example(Example::get("Built-in scenarios", "/_rustybin/harness/scenarios")),
        Endpoint::new(
            "/_rustybin/harness/scenarios/{name}",
            &["GET"],
            category::HARNESS,
            "One built-in scenario as JSON (?format=yaml for YAML)",
        )
        .example(Example::get("The smoke scenario", "/_rustybin/harness/scenarios/smoke"))
        .example(
            Example::get("As YAML", "/_rustybin/harness/scenarios/smoke?format=yaml"),
        ),
        Endpoint::new(
            "/_rustybin/harness/validate",
            &["POST"],
            category::HARNESS,
            "Validate a scenario (JSON or YAML) against this node's caps without sending anything",
        )
        .example(Example::post("Validate", "/_rustybin/harness/validate").json(SMOKE_JSON)),
        Endpoint::new(
            "/_rustybin/harness/runs",
            &["GET", "POST"],
            category::HARNESS,
            "Runs on this node; POST a scenario (JSON or YAML) to validate, pin its targets and start it",
        )
        .description(
            "Needs harness mode and the client role. Targets must be in RUSTYBIN_HARNESS_TARGETS; \
             link-local, metadata and control-plane targets are always refused. One run at a time per \
             node. Answers 202 with the run and a Location header. Needs the harness scope (or admin) \
             when the control plane is protected.",
        )
        .example(Example::get("List runs", "/_rustybin/harness/runs").expect_status(404))
        .example(
            Example::post("Start the smoke scenario", "/_rustybin/harness/runs")
                .json(SMOKE_JSON)
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/runs/{run}",
            &["GET"],
            category::HARNESS,
            "Live stats of a run: progress, counts, latency so far, the last second",
        )
        .example(Example::get("A run", "/_rustybin/harness/runs/missing").expect_status(404)),
        Endpoint::new(
            "/_rustybin/harness/runs/{run}/stop",
            &["POST"],
            category::HARNESS,
            "Stop a run (it drains and reports)",
        )
        .example(
            Example::post("Stop a run", "/_rustybin/harness/runs/missing/stop").expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/runs/{run}/report",
            &["GET"],
            category::HARNESS,
            "Final report (JSON; ?format=text or junit)",
        )
        .example(
            Example::get("A report", "/_rustybin/harness/runs/missing/report").expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/stop",
            &["POST"],
            category::HARNESS,
            "Kill switch: stop all load on this node now",
        )
        .example(Example::post("Kill switch", "/_rustybin/harness/stop").expect_status(404)),
        Endpoint::new(
            "/harness",
            &["ANY"],
            category::HARNESS,
            "Shaped upstream: answers with the run's server profile (latency, status mix, body)",
        )
        .example(Example::get("Shaped root", "/harness")),
        Endpoint::new(
            "/harness/{*path}",
            &["ANY"],
            category::HARNESS,
            "Shaped upstream at any path: a marked request gets its run's profile, anything else the default",
        )
        .description(
            "The server side of a run (scenario `servers`): a log-normal latency from p50 and p99, a \
             weighted status mix, a JSON body padded to body_bytes, optionally streamed in chunks \
             after a first-byte delay. Without a harness marker: 200, 256 bytes, no delay.",
        )
        .example(Example::get("Shaped path", "/harness/orders/42"))
        .example(Example::post("Shaped POST", "/harness/orders").json("{}")),
        Endpoint::new(
            WELL_KNOWN_PATH,
            &["GET"],
            category::HARNESS,
            "This node's target verification token (RUSTYBIN_HARNESS_VERIFY_TARGETS)",
        )
        .description(
            "A gateway that routes to this node serves the token, which proves the route reaches \
             the harness before any load is sent. 404 when harness mode is off.",
        )
        .example(Example::get("Verification token", WELL_KNOWN_PATH).expect_status(404)),
    ]
}

pub fn openapi_paths() -> Value {
    let scenario_body = json!({
        "required": true,
        "content": {
            "application/json": { "schema": { "$ref": "#/components/schemas/HarnessScenario" } },
            "application/yaml": { "schema": { "type": "string" } }
        }
    });
    let off = json!({ "description": "Harness mode is off (RUSTYBIN_HARNESS=on), or no such run" });
    let tag = json!(["Harness"]);
    json!({
        "/_rustybin/harness": {
            "get": {
                "tags": tag,
                "summary": "Harness node status",
                "description": "Roles, caps, engine, allowlist, the active run. `enabled: false` when harness mode is off.",
                "operationId": "getHarnessStatus",
                "responses": { "200": { "description": "Status", "content": { "application/json": { "schema": { "type": "object" } } } } }
            }
        },
        "/_rustybin/harness/schema": {
            "get": {
                "tags": tag,
                "summary": "Scenario JSON Schema",
                "description": "JSON Schema (draft 2020-12) of a scenario, with this node's caps as bounds (`x-caps`).",
                "operationId": "getHarnessSchema",
                "responses": { "200": { "description": "Schema", "content": { "application/schema+json": { "schema": { "type": "object" } } } } }
            }
        },
        "/_rustybin/harness/scenarios": {
            "get": {
                "tags": tag,
                "summary": "Built-in scenarios",
                "operationId": "listHarnessScenarios",
                "responses": { "200": { "description": "Scenarios with their summaries", "content": { "application/json": { "schema": { "type": "object" } } } } }
            }
        },
        "/_rustybin/harness/scenarios/{name}": {
            "get": {
                "tags": tag,
                "summary": "One built-in scenario",
                "operationId": "getHarnessScenario",
                "parameters": [
                    { "name": "name", "in": "path", "required": true, "schema": { "type": "string" } },
                    { "name": "format", "in": "query", "required": false, "schema": { "type": "string", "enum": ["json", "yaml"] } }
                ],
                "responses": {
                    "200": { "description": "The scenario", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/HarnessScenario" } }, "application/yaml": { "schema": { "type": "string" } } } },
                    "404": { "description": "Unknown scenario" }
                }
            }
        },
        "/_rustybin/harness/validate": {
            "post": {
                "tags": tag,
                "summary": "Validate a scenario",
                "description": "Parses and validates a scenario against this node's caps and returns every problem, the normalised scenario, its hash and summary. Sends nothing.",
                "operationId": "validateHarnessScenario",
                "requestBody": scenario_body,
                "responses": {
                    "200": { "description": "Validation result (`valid`, `problems`, `scenario`, `summary`)", "content": { "application/json": { "schema": { "type": "object" } } } },
                    "400": { "description": "Unparseable scenario" }
                }
            }
        },
        "/_rustybin/harness/runs": {
            "get": {
                "tags": tag,
                "summary": "Runs on this node",
                "operationId": "listHarnessRuns",
                "responses": { "200": { "description": "Runs", "content": { "application/json": { "schema": { "type": "object" } } } }, "404": off }
            },
            "post": {
                "tags": tag,
                "summary": "Start a run",
                "description": "Validates the scenario, pins and (optionally) verifies its targets and starts the run. One run at a time per node (409). Targets outside RUSTYBIN_HARNESS_TARGETS are refused (403).",
                "operationId": "createHarnessRun",
                "requestBody": scenario_body,
                "responses": {
                    "202": { "description": "The run (live view), with a Location header", "content": { "application/json": { "schema": { "type": "object" } } } },
                    "400": { "description": "Invalid scenario (`problems`)" },
                    "403": { "description": "No client role, a target not allowed, or verification failed" },
                    "409": { "description": "A run is active" },
                    "404": off
                }
            }
        },
        "/_rustybin/harness/runs/{run}": {
            "get": {
                "tags": tag,
                "summary": "Live stats of a run",
                "operationId": "getHarnessRun",
                "parameters": [{ "name": "run", "in": "path", "required": true, "schema": { "type": "string" } }],
                "responses": { "200": { "description": "Live view", "content": { "application/json": { "schema": { "type": "object" } } } }, "404": off }
            }
        },
        "/_rustybin/harness/runs/{run}/stop": {
            "post": {
                "tags": tag,
                "summary": "Stop a run",
                "operationId": "stopHarnessRun",
                "parameters": [{ "name": "run", "in": "path", "required": true, "schema": { "type": "string" } }],
                "responses": { "202": { "description": "Stopping (drains, then reports)" }, "200": { "description": "Already finished" }, "404": off }
            }
        },
        "/_rustybin/harness/runs/{run}/report": {
            "get": {
                "tags": tag,
                "summary": "Final report",
                "operationId": "getHarnessReport",
                "parameters": [
                    { "name": "run", "in": "path", "required": true, "schema": { "type": "string" } },
                    { "name": "format", "in": "query", "required": false, "schema": { "type": "string", "enum": ["json", "text", "junit"] } }
                ],
                "responses": {
                    "200": { "description": "The report", "content": { "application/json": { "schema": { "type": "object" } }, "text/plain": { "schema": { "type": "string" } }, "application/xml": { "schema": { "type": "string" } } } },
                    "409": { "description": "The run has not finished" },
                    "404": off
                }
            }
        },
        "/_rustybin/harness/stop": {
            "post": {
                "tags": tag,
                "summary": "Kill switch",
                "description": "Stops every run on this node now.",
                "operationId": "harnessKillSwitch",
                "responses": { "200": { "description": "Stopped runs", "content": { "application/json": { "schema": { "type": "object" } } } }, "404": off }
            }
        },
        "/harness": shaped_item("getShapedRoot", &tag),
        "/harness/{path}": shaped_item("getShapedPath", &tag),
        WELL_KNOWN_PATH: {
            "get": {
                "tags": tag,
                "summary": "Harness verification token",
                "operationId": "getHarnessVerificationToken",
                "security": [],
                "responses": { "200": { "description": "The token", "content": { "text/plain": { "schema": { "type": "string" } } } }, "404": off }
            }
        }
    })
}

/// OpenAPI path item of the shaped endpoint (every method).
fn shaped_item(id: &str, tag: &Value) -> Value {
    let mut item = serde_json::Map::new();
    for method in ["get", "post", "put", "patch", "delete"] {
        let mut op = json!({
            "tags": tag,
            "summary": "Shaped upstream",
            "description": "Answers with the run's server profile (scenario `servers`: log-normal latency, status mix, padded JSON body, chunked streaming, first-byte delay) when the request carries a verified harness marker, else 200 with a 256 byte body.",
            "operationId": format!("{id}{}", method.to_ascii_uppercase()),
            "responses": { "200": { "description": "The shaped answer (any configured status)", "content": { "application/json": { "schema": { "type": "object" } } } } }
        });
        if id.ends_with("Path") {
            op["parameters"] = json!([{ "name": "path", "in": "path", "required": true, "schema": { "type": "string" } }]);
        }
        item.insert(method.into(), op);
    }
    Value::Object(item)
}

pub fn openapi_components() -> Value {
    json!({
        "schemas": {
            "HarnessScenario": {
                "type": "object",
                "description": "A harness scenario: targets, layers with sources and patterns, scale, timeout and thresholds. The full JSON Schema with descriptions and this node's bounds is served at GET /_rustybin/harness/schema.",
                "required": ["name"],
                "properties": {
                    "name": { "type": "string" },
                    "description": { "type": "string" },
                    "target": { "type": "string", "description": "Shorthand for one target URL" },
                    "targets": { "type": "array", "items": { "type": "object" } },
                    "scale": { "type": "object" },
                    "layers": { "type": "array", "items": { "type": "object" } },
                    "source": { "type": "object", "description": "Shorthand: the source of a single layer" },
                    "pattern": { "type": "object", "description": "Shorthand: the pattern of a single layer" },
                    "model": { "type": "string", "enum": ["open", "closed"] },
                    "think_time": { "type": "string" },
                    "timeout": { "type": "string" },
                    "thresholds": { "type": "object" }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests;
