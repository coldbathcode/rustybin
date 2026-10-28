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
pub mod cluster;
pub mod correlate;
pub mod election;
pub mod engine;
pub mod limit_test;
pub mod live;
pub mod member;
pub mod pattern;
pub mod ratelimit;
pub mod report;
pub mod safety;
pub mod saved;
pub mod scenario;
pub mod shaped;
#[cfg(test)]
pub mod test_net;
#[cfg(test)]
pub mod test_proxy;
pub mod wire;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::config::{Config, HarnessConfig};
use crate::state::AppState;
use correlate::{Correlator, RunKey};
use engine::{Engine, PinnedTarget, Run, RunParams, Share};
use member::Membership;
use safety::{Allowlist, KillSwitch, Lease};
use scenario::{Caps, Scenario};

/// Prefix of every harness control route.
pub const PREFIX: &str = "/_rustybin/harness";
/// Where a node serves its verification token (data plane).
pub const WELL_KNOWN_PATH: &str = "/.well-known/rustybin-harness";
/// On a redirect to the leader: `<node_id>; term=<n>`.
pub const LEADER_HEADER: &str = "x-rustybin-harness-leader";
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

/// The routes cluster nodes call on each other: join (with the join token),
/// heartbeat, vote and the new leader's announcement (with the node
/// credential). They authenticate themselves, so the control-plane auth,
/// the key layer and the plan limits leave them alone.
pub fn is_node_path(method: &axum::http::Method, path: &str) -> bool {
    if method != axum::http::Method::POST {
        return false;
    }
    let Some(rest) = path
        .strip_prefix(PREFIX)
        .and_then(|r| r.strip_prefix("/clusters/"))
    else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        [id, "nodes"] => cluster::safe_id(id),
        [id, "nodes", node, "heartbeat"] => cluster::safe_id(id) && cluster::safe_id(node),
        [id, "election", "vote" | "elected"] => cluster::safe_id(id),
        _ => false,
    }
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
    if h.has_role(Role::Coordinator) && h.cluster.is_some() && h.join_token.is_none() {
        return Err(
            "RUSTYBIN_HARNESS_CLUSTER on a coordinator needs RUSTYBIN_HARNESS_JOIN_TOKEN, the secret its nodes join with"
                .into(),
        );
    }
    if h.join.is_some() && h.join_token.is_none() {
        return Err("RUSTYBIN_HARNESS_JOIN needs RUSTYBIN_HARNESS_JOIN_TOKEN".into());
    }
    if h.join.is_some()
        && !h.has_role(Role::Client)
        && !h.has_role(Role::Server)
        && (h.control_url.is_none() || h.leader == wire::LeaderPolicy::Never)
    {
        return Err(
            "RUSTYBIN_HARNESS_JOIN needs the client or server role, or RUSTYBIN_HARNESS_CONTROL_URL to stand by as leader"
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

/// How a run starts on this node. A single-node run takes the defaults; a
/// cluster member's part of a run carries what its leader assigned.
#[derive(Default)]
pub struct RunOptions {
    /// The cluster's run id (a single-node run gets a random one).
    pub id: Option<String>,
    /// The cluster's run key (a single-node run gets a random one).
    pub key: Option<RunKey>,
    /// This node's share of the rate and the users.
    pub share: Share,
    /// The synchronised start.
    pub start_at: Option<Instant>,
    /// The dead man's switch: no leader answer for its period stops the run.
    pub lease: Option<Arc<Lease>>,
    pub cluster: Option<String>,
    /// The token the targets must serve (the cluster's, served by its
    /// server nodes), instead of this node's own.
    pub verification_token: Option<String>,
    /// Live changes made before this node started its part.
    pub controls: Option<live::Controls>,
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
    /// The clusters this node leads (phase 3: the node a cluster was
    /// created on leads it).
    pub coordinator: cluster::Coordinator,
    /// The cluster this node is a member of (one at a time).
    membership: Mutex<Option<Arc<Membership>>>,
    /// The HTTP listener's address, once bound (the default advertise URL).
    bound: OnceLock<SocketAddr>,
    /// `RUSTYBIN_HARNESS_CONTROL_URL` with `auto` resolved once bound.
    control: OnceLock<Option<String>>,
    /// Scenarios saved by name (`RUSTYBIN_HARNESS_STATE_DIR` keeps them).
    pub saved: saved::SavedScenarios,
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
            coordinator: cluster::Coordinator::default(),
            membership: Mutex::new(None),
            bound: OnceLock::new(),
            control: OnceLock::new(),
            saved: saved::SavedScenarios::new(hc.state_dir.as_deref()),
        })
    }

    /// This node's control URL as the other nodes reach it: set means it
    /// may lead. `auto` is the listener's address when it binds a specific
    /// one (unset before the listener is bound).
    pub fn control_url(&self) -> Option<String> {
        match self.config.control_url.as_deref() {
            None => None,
            Some("auto") => self.control.get().cloned().flatten(),
            Some(url) => Some(url.to_string()),
        }
    }

    /// Whether this node may lead a cluster it is a member of.
    pub fn may_lead(&self) -> bool {
        self.config.leader != wire::LeaderPolicy::Never && self.control_url().is_some()
    }

    /// This node as the leader of a cluster.
    pub fn leader_info(&self) -> cluster::LeaderInfo {
        cluster::LeaderInfo {
            node_id: self.node_id.clone(),
            control_url: self.control_url(),
            report_dir: self.config.report_dir.clone(),
            election_timeout: self.config.election_timeout,
        }
    }

    /// The HTTP listener's port, once bound.
    pub fn bound_port(&self) -> Option<u16> {
        self.bound.get().map(|a| a.port())
    }

    /// The cluster this node is a member of.
    pub fn membership(&self) -> Option<Arc<Membership>> {
        self.membership
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .filter(|m| !m.is_stopped())
    }

    /// Become a member; false when this node is a member of a cluster already.
    pub fn set_membership(&self, m: Arc<Membership>) -> bool {
        let mut slot = self.membership.lock().unwrap_or_else(|p| p.into_inner());
        if slot.as_ref().is_some_and(|old| !old.is_stopped()) {
            return false;
        }
        *slot = Some(m);
        true
    }

    /// Leave the cluster this node is a member of. Returns its id.
    pub async fn leave(self: &Arc<Self>) -> Option<String> {
        let m = self
            .membership
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()?;
        m.leave(self).await;
        Some(m.cluster_id.clone())
    }

    /// The server is shutting down: leave the cluster this node is a
    /// member of (its runs of it stop) and release the heartbeats held by
    /// the clusters it leads.
    pub async fn shutdown_clusters(self: &Arc<Self>) {
        self.coordinator.close();
        self.leave().await;
    }

    /// Record the HTTP listener's address (the server calls this once bound).
    pub fn set_bound(&self, addr: SocketAddr) {
        let _ = self.bound.set(addr);
        if self.config.control_url.as_deref() == Some("auto") {
            let url = (!addr.ip().is_unspecified()).then(|| format!("http://{addr}"));
            if url.is_none() {
                tracing::warn!(
                    "RUSTYBIN_HARNESS_CONTROL_URL=auto needs a listener on a specific address (RUSTYBIN_HOST), not {}: this node never leads",
                    addr.ip()
                );
            }
            let _ = self.control.set(url);
        }
    }

    /// A server node's URL when `RUSTYBIN_HARNESS_ADVERTISE` is unset: the
    /// listener's address, when it is a specific one (not `0.0.0.0`).
    pub fn default_advertise(&self) -> Option<String> {
        let addr = self.bound.get()?;
        if addr.ip().is_unspecified() {
            return None;
        }
        Some(format!("http://{addr}"))
    }

    /// The engine's worker threads (0 without the client role).
    pub fn engine_threads(&self) -> usize {
        self.engine.as_ref().map_or(0, |e| e.threads)
    }

    /// The token served at [`WELL_KNOWN_PATH`]: the cluster's when this
    /// node is a member (its clients verify the gateway routes to the
    /// cluster's servers), else this node's own.
    pub fn served_verification_token(&self) -> String {
        self.membership()
            .and_then(|m| m.verification_token())
            .unwrap_or_else(|| self.verification_token.clone())
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
        let mut stopped: Vec<String> = self
            .runs()
            .iter()
            .filter(|r| r.is_active())
            .map(|r| {
                r.stop("kill_switch");
                r.id.clone()
            })
            .collect();
        // The leader stops its clusters' runs; a member tells its leader on
        // the next heartbeat.
        for id in self.coordinator.stop_all("kill_switch") {
            if !stopped.contains(&id) {
                stopped.push(id);
            }
        }
        if let Some(m) = self.membership() {
            m.request_kill();
        }
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
            "roles": self.roles().iter().map(|r| r.as_str()).collect::<Vec<_>>(),
            "control_url": self.control_url(),
            "may_lead": self.may_lead(),
            "leader_policy": self.config.leader.as_str(),
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
            "cluster": self.membership().map(|m| m.describe()),
            "leader": self.leader_view(),
            "clusters": self.coordinator.list().iter().map(|c| c.summary()).collect::<Vec<_>>(),
        })
    }

    /// The configured roles, plus `coordinator` while it leads a cluster
    /// (a node that won an election takes the role on).
    pub fn roles(&self) -> Vec<Role> {
        let mut roles = self.config.roles.clone();
        if !roles.contains(&Role::Coordinator) && !self.coordinator.is_empty() {
            roles.push(Role::Coordinator);
        }
        roles
    }

    /// The leader as this node sees it: itself for the clusters it leads,
    /// else its membership's leader.
    fn leader_view(&self) -> Value {
        if let Some(m) = self.membership() {
            if let Some(c) = self.coordinator.get(&m.cluster_id) {
                return json!(c.leader_ref());
            }
            return json!(m.leader());
        }
        match self.coordinator.list().first() {
            Some(c) => json!(c.leader_ref()),
            None => Value::Null,
        }
    }

    /// Validate a scenario (normalised in place) against this node.
    pub fn validate(&self, scenario: &mut Scenario) -> Vec<String> {
        scenario.normalise();
        scenario.validate(&self.caps())
    }

    /// Validate, pin and verify the targets, then start the run on its
    /// scheduler thread. Only one run is active at a time per node.
    pub async fn create_run(self: &Arc<Self>, scenario: Scenario) -> Result<Arc<Run>, RunError> {
        self.start_local_run(scenario, RunOptions::default()).await
    }

    /// [`Harness::create_run`] with the options of a cluster member's part:
    /// the share is validated against this node's caps, the targets against
    /// this node's own allowlist.
    pub async fn start_local_run(
        self: &Arc<Self>,
        mut scenario: Scenario,
        options: RunOptions,
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
        if !options.share.valid() {
            return Err(RunError::new(400, "the share must be within 0 to 1"));
        }
        scenario.normalise();
        let problems = scenario.validate_scaled(&self.caps(), options.share.fraction());
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
        // scale.connections_per_client: the keep-alive pool per target is
        // this node's virtual clients times it (at most the in-flight cap).
        let pool = scenario
            .scale
            .connections_per_client
            .map_or(engine.inflight_cap, |c| {
                let vus = options
                    .share
                    .virtual_clients(scenario.scale.virtual_clients)
                    .1;
                (vus as usize * c as usize).clamp(1, engine.inflight_cap)
            });
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
                Ok(addrs) => {
                    match PinnedTarget::new(&engine.handle(), t.clone(), parsed, addrs, pool) {
                        Ok(pt) => targets.push(pt),
                        Err(e) => refused.push(format!("targets[{i}]: {e}")),
                    }
                }
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
            let token = options
                .verification_token
                .clone()
                .unwrap_or_else(|| self.verification_token.clone());
            for target in &mut targets {
                self.verify_target(target, &token)
                    .await
                    .map_err(|e| RunError {
                        status: 403,
                        error: "target verification failed".into(),
                        problems: vec![e],
                    })?;
                target.verified = true;
            }
        }
        let id = options.id.clone().unwrap_or_else(|| random_hex(6));
        // The run key signs the markers. On a single node its server side
        // learns it now; the server nodes of a cluster get it from the
        // leader, which also does the `lost` lookup.
        let key = options.key.clone().unwrap_or_else(RunKey::random);
        let entry = if options.cluster.is_none() && self.is_server() {
            Some(
                self.correlator
                    .register(&id, key.clone(), scenario.servers.clone()),
            )
        } else {
            None
        };
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
            entry,
            correlator: self.correlator.clone(),
            metrics: self.metrics.clone(),
            share: options.share,
            start_at: options.start_at,
            lease: options.lease,
            cluster: options.cluster,
            controls: options.controls,
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
    async fn verify_target(&self, target: &PinnedTarget, token: &str) -> Result<(), String> {
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
        if !crate::control_auth::constant_time_eq(&text, token) {
            return Err(format!(
                "{}: {WELL_KNOWN_PATH} does not serve this node's verification token (another instance, or not routed to this cluster)",
                target.target.url
            ));
        }
        Ok(())
    }
}

/// Create a cluster led by this node (the coordinator role). A node that
/// also has the client or server role joins it over the in-process link.
pub async fn create_cluster(
    h: &Arc<Harness>,
    req: cluster::CreateCluster,
) -> Result<(Arc<cluster::Cluster>, String), RunError> {
    if !h.config.has_role(Role::Coordinator) {
        return Err(RunError::new(
            403,
            "this node has no coordinator role (RUSTYBIN_HARNESS_ROLES=coordinator, with client or server as well if it should send or serve)",
        ));
    }
    if let Some(m) = h.membership() {
        return Err(RunError::new(
            409,
            format!(
                "this node is a member of cluster {} already: leave it first (POST {PREFIX}/leave)",
                m.cluster_id
            ),
        ));
    }
    let (cluster, token) = h.coordinator.create(&h.leader_info(), req)?;
    election::spawn_lead(h, &cluster);
    // The leader is a member of its own cluster (in process): it counts
    // among the eligible nodes, and after a failover it carries on as an
    // ordinary member of the new leader.
    let advertise = h.config.advertise.clone();
    if let Err(e) = member::join(
        h,
        member::JoinTarget::Local(cluster.id.clone()),
        token.clone(),
        advertise,
    )
    .await
    {
        tracing::warn!(target: "rustybin::harness", cluster = %cluster.id, "harness: the leader could not join its own cluster: {e}");
    }
    Ok((cluster, token))
}

/// At startup, once the HTTP listener is bound: a coordinator with
/// `RUSTYBIN_HARNESS_CLUSTER` creates its cluster, a node with
/// `RUSTYBIN_HARNESS_JOIN` starts joining (retried until the leader answers).
pub async fn start_cluster_from_config(h: &Arc<Harness>) -> Result<(), String> {
    let hc = &h.config;
    // A coordinator with a join URL is a standby: it joins.
    if hc.has_role(Role::Coordinator) && hc.join.is_none() {
        if let (Some(id), Some(token)) = (&hc.cluster, &hc.join_token) {
            let req = cluster::CreateCluster {
                cluster_id: Some(id.clone()),
                join_token: Some(token.clone()),
                ..Default::default()
            };
            create_cluster(h, req)
                .await
                .map_err(|e| format!("harness cluster {id}: {}", e.error))?;
            return Ok(());
        }
    }
    member::spawn_from_config(h)
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
    if let Some(id) = &hc.cluster {
        if let Some(cluster) = harness.coordinator.get(id) {
            return headless_cluster(harness, &cluster, scenario).await;
        }
    }
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
    write_headless_report(hc, &run.id, report)
}

/// Headless on a coordinator: submit the scenario to its cluster, start it
/// (`on-quorum`: once the nodes it waits for have joined; `immediately`:
/// now, an error without them), wait and report.
async fn headless_cluster(
    harness: &Arc<Harness>,
    cluster: &Arc<cluster::Cluster>,
    scenario: Scenario,
) -> u8 {
    let hc = &harness.config;
    let run = match cluster.create_run(scenario, &harness.caps()) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("headless harness: {} {}", e.error, e.problems.join("; "));
            return 2;
        }
    };
    let mut waited = Instant::now();
    loop {
        let q = cluster.quorum(&run.scenario);
        if q.met || hc.start == StartMode::Immediately {
            break;
        }
        if waited.elapsed() >= Duration::from_secs(10) {
            tracing::info!(
                "headless harness: waiting for the quorum of cluster {}: {} of {} client node(s), {} of {} server node(s)",
                cluster.id, q.clients, q.need_clients, q.servers, q.need_servers
            );
            waited = Instant::now();
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if let Err(e) = cluster.start_run(&run.id) {
        tracing::error!("headless harness: {} {}", e.error, e.problems.join("; "));
        return 2;
    }
    run.wait().await;
    let Some(report) = run.report() else {
        return 2;
    };
    write_headless_report(hc, &run.id, report)
}

/// Write a headless report where `RUSTYBIN_HARNESS_REPORT` says and return
/// the exit code.
fn write_headless_report(hc: &HarnessConfig, run_id: &str, report: &report::Report) -> u8 {
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
        run_id,
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
        (
            "step-plateaus",
            include_str!("scenarios/step-plateaus.yaml"),
        ),
        ("breakpoint", include_str!("scenarios/breakpoint.yaml")),
        (
            "spike-and-recover",
            include_str!("scenarios/spike-and-recover.yaml"),
        ),
        ("pulse", include_str!("scenarios/pulse.yaml")),
        ("burst", include_str!("scenarios/burst.yaml")),
        ("daily-wave", include_str!("scenarios/daily-wave.yaml")),
        ("rate-limit", include_str!("scenarios/rate-limit.yaml")),
        ("limit-probe", include_str!("scenarios/limit-probe.yaml")),
        ("many-ips", include_str!("scenarios/many-ips.yaml")),
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
            get(api::get_scenario)
                .put(api::put_scenario)
                .delete(api::delete_scenario),
        )
        .route("/_rustybin/harness/preview", post(api::preview))
        .route("/_rustybin/harness/validate", post(api::validate))
        .route(
            "/_rustybin/harness/runs",
            get(api::list_runs).post(api::create_run),
        )
        .route(
            "/_rustybin/harness/runs/{run}",
            get(api::get_run).patch(api::patch_run),
        )
        .route("/_rustybin/harness/runs/{run}/events", get(api::run_events))
        .route("/_rustybin/harness/runs/{run}/stop", post(api::stop_run))
        .route("/_rustybin/harness/runs/{run}/report", get(api::run_report))
        .route("/_rustybin/harness/stop", post(api::stop_all))
        .route("/_rustybin/harness/join", post(api::join))
        .route("/_rustybin/harness/leave", post(api::leave))
        .route(
            "/_rustybin/harness/clusters",
            get(api::list_clusters).post(api::create_cluster),
        )
        .route(
            "/_rustybin/harness/clusters/{id}",
            get(api::get_cluster).delete(api::delete_cluster),
        )
        .route(
            "/_rustybin/harness/clusters/{id}/upstreams",
            get(api::upstreams),
        )
        .route(
            "/_rustybin/harness/clusters/{id}/leader",
            get(api::get_leader),
        )
        .route(
            "/_rustybin/harness/clusters/{id}/election/vote",
            post(api::node_vote),
        )
        .route(
            "/_rustybin/harness/clusters/{id}/election/elected",
            post(api::node_elected),
        )
        .route(
            "/_rustybin/harness/clusters/{id}/nodes",
            post(api::node_join),
        )
        .route(
            "/_rustybin/harness/clusters/{id}/nodes/{node}/heartbeat",
            post(api::node_heartbeat),
        )
        .route(
            "/_rustybin/harness/clusters/{id}/runs",
            get(api::list_cluster_runs).post(api::create_cluster_run),
        )
        .route(
            "/_rustybin/harness/clusters/{id}/runs/{run}",
            get(api::get_cluster_run).patch(api::patch_cluster_run),
        )
        .route(
            "/_rustybin/harness/clusters/{id}/runs/{run}/events",
            get(api::cluster_run_events),
        )
        .route(
            "/_rustybin/harness/clusters/{id}/runs/{run}/start",
            post(api::start_cluster_run),
        )
        .route(
            "/_rustybin/harness/clusters/{id}/runs/{run}/stop",
            post(api::stop_cluster_run),
        )
        .route(
            "/_rustybin/harness/clusters/{id}/runs/{run}/report",
            get(api::cluster_run_report),
        )
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
            "The built-in scenarios (smoke, ramp-to-target, soak, closed-users, step-plateaus, breakpoint, spike-and-recover, pulse, burst, daily-wave) and the saved ones",
        )
        .example(Example::get("Built-in scenarios", "/_rustybin/harness/scenarios")),
        Endpoint::new(
            "/_rustybin/harness/scenarios/{name}",
            &["GET", "PUT", "DELETE"],
            category::HARNESS,
            "A built-in or saved scenario (?format=yaml for YAML); PUT saves one under a name, DELETE removes it",
        )
        .description(
            "Saved scenarios are validated against this node's caps, kept by name (at most 100; \
             RUSTYBIN_HARNESS_STATE_DIR writes them to disk) and listed next to the built-in ones, \
             whose names are taken.",
        )
        .example(Example::get("The smoke scenario", "/_rustybin/harness/scenarios/smoke"))
        .example(
            Example::get("As YAML", "/_rustybin/harness/scenarios/smoke?format=yaml"),
        )
        .example(
            Example::new("Save a scenario", "PUT", "/_rustybin/harness/scenarios/my-smoke")
                .json(SMOKE_JSON)
                .expect_status(404),
        )
        .example(
            Example::delete("Delete a saved scenario", "/_rustybin/harness/scenarios/my-smoke")
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/preview",
            &["POST"],
            category::HARNESS,
            "The expected rate per second per layer and in total, without sending anything",
        )
        .description(
            "Patterns are integrated exactly, so a 50 ms burst shows in its second. One point per \
             second (coarser past an hour, at most 3600). Works with harness mode off.",
        )
        .example(Example::post("Preview", "/_rustybin/harness/preview").json(SMOKE_JSON)),
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
            &["GET", "PATCH"],
            category::HARNESS,
            "Live stats of a run; PATCH tunes it while it runs (rate_multiplier, pause, resume, hold, skip_stage, set, add_layer, remove_layer)",
        )
        .description(
            "A change is checked like a new scenario (the caps included), applied on the next \
             scheduler pass and recorded in the run's log and report.",
        )
        .example(Example::get("A run", "/_rustybin/harness/runs/missing").expect_status(404))
        .example(
            Example::new("Halve the rate", "PATCH", "/_rustybin/harness/runs/missing")
                .json(r#"{"rate_multiplier":0.5}"#)
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/runs/{run}/events",
            &["GET"],
            category::HARNESS,
            "The run's feed: a snapshot every second, change and finding events, finished at the end",
        )
        .sse()
        .example(
            Example::get("Run feed", "/_rustybin/harness/runs/missing/events").expect_status(404),
        ),
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
            "/_rustybin/harness/join",
            &["POST"],
            category::HARNESS,
            "Join a cluster: {url, join_token, advertise?} (orchestrated)",
        )
        .description(
            "Tells this node (client or server role) to join the cluster at url \
             (<leader>/_rustybin/harness/clusters/<id>). The node then heartbeats to the leader; \
             assignments are checked against this node's own caps and allowlist.",
        )
        .example(
            Example::post("Join", "/_rustybin/harness/join")
                .json(r#"{"url":"http://127.0.0.1:8080/_rustybin/harness/clusters/perf-1","join_token":"0123456789abcdef"}"#)
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/leave",
            &["POST"],
            category::HARNESS,
            "Leave the cluster (this node's runs of it stop)",
        )
        .example(Example::post("Leave", "/_rustybin/harness/leave").expect_status(404)),
        Endpoint::new(
            "/_rustybin/harness/clusters",
            &["GET", "POST"],
            category::HARNESS,
            "Clusters this node leads; POST {cluster_id?, min_nodes?, lease?, election_timeout?, join_token?} creates one",
        )
        .description(
            "Needs the coordinator role. The answer carries the join token once (only its hash is \
             kept). A coordinator that is also a client or server joins its own cluster.",
        )
        .example(Example::get("List clusters", "/_rustybin/harness/clusters").expect_status(404))
        .example(
            Example::post("Create a cluster", "/_rustybin/harness/clusters")
                .json(r#"{"cluster_id":"perf-1","min_nodes":{"client":2,"server":1}}"#)
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/clusters/{id}",
            &["GET", "DELETE"],
            category::HARNESS,
            "A cluster: members, roles, health, clock offsets, the current run",
        )
        .description(
            "On a member of the cluster that is not its leader: 307 to the leader, with \
             X-Rustybin-Harness-Leader. DELETE needs no active run.",
        )
        .example(
            Example::get("A cluster", "/_rustybin/harness/clusters/perf-1").expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/clusters/{id}/upstreams",
            &["GET"],
            category::HARNESS,
            "The server nodes' advertised URLs, for the gateway's upstream pool",
        )
        .example(
            Example::get("Upstreams", "/_rustybin/harness/clusters/perf-1/upstreams")
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/clusters/{id}/leader",
            &["GET"],
            category::HARNESS,
            "The leader, its term, the succession with work scores and the history of leader changes",
        )
        .description(
            "Answered by every member, never redirected, so a controller can always find the \
             leader. failover is available with three or more eligible nodes \
             (RUSTYBIN_HARNESS_CONTROL_URL).",
        )
        .example(
            Example::get("The leader", "/_rustybin/harness/clusters/perf-1/leader")
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/clusters/{id}/election/vote",
            &["POST"],
            category::HARNESS,
            "Node route: a candidate asks an eligible node for its vote (node credential)",
        )
        .example(
            Example::post("Vote (node)", "/_rustybin/harness/clusters/perf-1/election/vote")
                .json(r#"{"term":2,"candidate":"n2","snapshot":{"term":1,"seq":7}}"#)
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/clusters/{id}/election/elected",
            &["POST"],
            category::HARNESS,
            "Node route: a new leader announces its term (node credential); an older leader steps down",
        )
        .example(
            Example::post(
                "Announce (node)",
                "/_rustybin/harness/clusters/perf-1/election/elected",
            )
            .json(r#"{"leader":{"node_id":"n2","term":2}}"#)
            .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/clusters/{id}/nodes",
            &["POST"],
            category::HARNESS,
            "Node route: join with the join token (bearer); returns a node credential and a lease",
        )
        .example(
            Example::post("Join (node)", "/_rustybin/harness/clusters/perf-1/nodes")
                .json("{}")
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/clusters/{id}/nodes/{node}/heartbeat",
            &["POST"],
            category::HARNESS,
            "Node route: heartbeat with the node credential; a long poll carrying stats, returning assignments",
        )
        .example(
            Example::post(
                "Heartbeat (node)",
                "/_rustybin/harness/clusters/perf-1/nodes/n1/heartbeat",
            )
            .json(r#"{"t1":0}"#)
            .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/clusters/{id}/runs",
            &["GET", "POST"],
            category::HARNESS,
            "Cluster runs; POST a scenario to submit it (validated, not started)",
        )
        .example(
            Example::get("Cluster runs", "/_rustybin/harness/clusters/perf-1/runs")
                .expect_status(404),
        )
        .example(
            Example::post("Submit a scenario", "/_rustybin/harness/clusters/perf-1/runs")
                .json(SMOKE_JSON)
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/clusters/{id}/runs/{run}",
            &["GET", "PATCH"],
            category::HARNESS,
            "Live merged stats of a cluster run, with its quorum; PATCH tunes it on every client node",
        )
        .description(
            "A change is checked against every client node's declared capacity for its share, \
             recorded in the run's log, carried in the cluster snapshot (it survives a failover), \
             and followed by the nodes on their next heartbeat.",
        )
        .example(
            Example::get("A cluster run", "/_rustybin/harness/clusters/perf-1/runs/missing")
                .expect_status(404),
        )
        .example(
            Example::new(
                "Hold the pattern",
                "PATCH",
                "/_rustybin/harness/clusters/perf-1/runs/missing",
            )
            .json(r#"{"hold":true}"#)
            .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/clusters/{id}/runs/{run}/events",
            &["GET"],
            category::HARNESS,
            "The cluster run's feed: snapshots, changes, findings, scale_request, leader_changed, finished",
        )
        .sse()
        .example(
            Example::get(
                "Cluster run feed",
                "/_rustybin/harness/clusters/perf-1/runs/missing/events",
            )
            .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/clusters/{id}/runs/{run}/start",
            &["POST"],
            category::HARNESS,
            "Start a cluster run: the rate split by capacity and a synchronised start (409 without the quorum)",
        )
        .example(
            Example::post(
                "Start a cluster run",
                "/_rustybin/harness/clusters/perf-1/runs/missing/start",
            )
            .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/clusters/{id}/runs/{run}/stop",
            &["POST"],
            category::HARNESS,
            "Stop a cluster run on every client node",
        )
        .example(
            Example::post(
                "Stop a cluster run",
                "/_rustybin/harness/clusters/perf-1/runs/missing/stop",
            )
            .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/harness/clusters/{id}/runs/{run}/report",
            &["GET"],
            category::HARNESS,
            "The merged report of a cluster run (JSON; ?format=text or junit)",
        )
        .example(
            Example::get(
                "A cluster report",
                "/_rustybin/harness/clusters/perf-1/runs/missing/report",
            )
            .expect_status(404),
        ),
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
    let mut paths = json!({
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
    });
    if let (Value::Object(all), Value::Object(more)) = (
        &mut paths,
        cluster_openapi_paths(&tag, &off, &scenario_body),
    ) {
        all.extend(more);
    }
    live_openapi_paths(&mut paths, &tag, &off, &scenario_body);
    paths
}

/// The OpenAPI of phase 4: live changes, the preview, saved scenarios and
/// the run feeds (operations added to the paths above, and new paths).
fn live_openapi_paths(paths: &mut Value, tag: &Value, off: &Value, scenario_body: &Value) {
    let change_body = json!({
        "required": true,
        "content": { "application/json": { "schema": { "type": "object", "description": "A live change: rate_multiplier, pause, resume, hold, skip_stage, set, add_layer, remove_layer, and layer to aim one (the schema is $defs.change of GET /_rustybin/harness/schema)." } } }
    });
    let sse = json!({ "description": "Server-Sent Events", "content": { "text/event-stream": { "schema": { "type": "string" } } } });
    paths["/_rustybin/harness/scenarios/{name}"]["put"] = json!({
        "tags": tag,
        "summary": "Save a scenario",
        "description": "Validated against this node's caps and kept under the name (at most 100; RUSTYBIN_HARNESS_STATE_DIR writes them to disk). Built-in names are taken.",
        "operationId": "saveHarnessScenario",
        "parameters": [path_param("name")],
        "requestBody": scenario_body,
        "responses": { "201": json_ok("Saved"), "200": json_ok("Replaced"), "400": { "description": "Invalid scenario or name" }, "409": { "description": "A built-in name" }, "404": off }
    });
    paths["/_rustybin/harness/scenarios/{name}"]["delete"] = json!({
        "tags": tag,
        "summary": "Delete a saved scenario",
        "operationId": "deleteHarnessScenario",
        "parameters": [path_param("name")],
        "responses": { "204": { "description": "Deleted" }, "409": { "description": "A built-in scenario" }, "404": off }
    });
    paths["/_rustybin/harness/runs/{run}"]["patch"] = json!({
        "tags": tag,
        "summary": "Change a running run",
        "description": "Checked like a new scenario (the caps included), applied on the next scheduler pass, recorded in the run's log and report.",
        "operationId": "changeHarnessRun",
        "parameters": [path_param("run")],
        "requestBody": change_body,
        "responses": { "200": json_ok("The run's controls after the change"), "400": { "description": "Refused (`problems`)" }, "409": { "description": "Finished, or a cluster run (change it on the leader)" }, "404": off }
    });
    paths["/_rustybin/harness/clusters/{id}/runs/{run}"]["patch"] = json!({
        "tags": tag,
        "summary": "Change a running cluster run",
        "description": "Checked against every client node's capacity for its share, carried in the cluster snapshot, followed by the nodes on their next heartbeat.",
        "operationId": "changeHarnessClusterRun",
        "parameters": [path_param("id"), path_param("run")],
        "requestBody": change_body,
        "responses": { "200": json_ok("The run's controls after the change"), "400": { "description": "Refused (`problems`)" }, "409": { "description": "Finished" }, "307": { "description": "This node is a member; the leader answers" }, "404": off }
    });
    if let Value::Object(all) = paths {
        all.insert("/_rustybin/harness/preview".into(), json!({
            "post": {
                "tags": tag,
                "summary": "Preview a scenario",
                "description": "The expected rate per second per layer and in total (users for closed layers), from the patterns' exact integrals, with the validation's problems. Sends nothing; works with harness mode off.",
                "operationId": "previewHarnessScenario",
                "requestBody": scenario_body,
                "responses": { "200": json_ok("`layers[].series`, `total`, `step_secs`, `valid`, `problems`"), "400": { "description": "Unparseable scenario" } }
            }
        }));
        all.insert("/_rustybin/harness/runs/{run}/events".into(), json!({
            "get": {
                "tags": tag,
                "summary": "Run feed",
                "description": "Server-Sent Events: `snapshot` every second, `change` and `finding`, then `finished` with the report's summary.",
                "operationId": "harnessRunEvents",
                "parameters": [path_param("run")],
                "responses": { "200": sse, "404": off }
            }
        }));
        all.insert("/_rustybin/harness/clusters/{id}/runs/{run}/events".into(), json!({
            "get": {
                "tags": tag,
                "summary": "Cluster run feed",
                "description": "Server-Sent Events: `snapshot` every second, `change`, `finding`, `scale_request`, `leader_changed` (this node no longer leads; reconnect to the URL in the event), then `finished`.",
                "operationId": "harnessClusterRunEvents",
                "parameters": [path_param("id"), path_param("run")],
                "responses": { "200": sse, "307": { "description": "This node is a member; the leader answers" }, "404": off }
            }
        }));
    }
}

/// The OpenAPI paths of the cluster routes (phases 3 and 3b).
fn cluster_openapi_paths(tag: &Value, off: &Value, scenario_body: &Value) -> Value {
    json!({
        "/_rustybin/harness/join": {
            "post": {
                "tags": tag,
                "summary": "Join a cluster",
                "description": "Tells this node to join the cluster at `url` with `join_token` (orchestrated). The URL must not resolve to a forbidden address.",
                "operationId": "joinHarnessCluster",
                "requestBody": { "required": true, "content": { "application/json": { "schema": {
                    "type": "object", "required": ["url", "join_token"],
                    "properties": { "url": { "type": "string" }, "join_token": { "type": "string" }, "advertise": { "type": "string" } }
                } } } },
                "responses": { "200": json_ok("Joined"), "400": { "description": "Invalid body or URL" }, "403": { "description": "Forbidden join address" }, "409": { "description": "Already a member, or the leader refused" }, "404": off }
            }
        },
        "/_rustybin/harness/leave": {
            "post": {
                "tags": tag,
                "summary": "Leave the cluster",
                "operationId": "leaveHarnessCluster",
                "responses": { "200": json_ok("Left"), "404": off }
            }
        },
        "/_rustybin/harness/clusters": {
            "get": {
                "tags": tag,
                "summary": "Clusters this node leads",
                "operationId": "listHarnessClusters",
                "responses": { "200": json_ok("Clusters"), "404": off }
            },
            "post": {
                "tags": tag,
                "summary": "Create a cluster",
                "description": "Needs the coordinator role. Returns the cluster and its join token (once).",
                "operationId": "createHarnessCluster",
                "requestBody": { "required": false, "content": { "application/json": { "schema": {
                    "type": "object",
                    "properties": {
                        "cluster_id": { "type": "string" },
                        "min_nodes": { "type": "object", "properties": { "client": { "type": "integer" }, "server": { "type": "integer" } } },
                        "lease": { "type": "string", "description": "500ms to 60s, default 5s" },
                        "election_timeout": { "type": "string", "description": "How long without the leader before an election starts: 200ms, below the lease (default RUSTYBIN_HARNESS_ELECTION_TIMEOUT, 3s)" },
                        "join_token": { "type": "string", "description": "At least 16 characters; random when absent" }
                    }
                } } } },
                "responses": { "201": json_ok("The cluster and its join token"), "400": { "description": "Invalid body" }, "403": { "description": "No coordinator role" }, "409": { "description": "The id exists, or too many clusters" }, "404": off }
            }
        },
        "/_rustybin/harness/clusters/{id}": {
            "get": {
                "tags": tag,
                "summary": "A cluster",
                "operationId": "getHarnessCluster",
                "parameters": [path_param("id")],
                "responses": { "200": json_ok("Members, health, clock offsets, the current run"), "307": { "description": "This node is a member; the leader answers" }, "404": off }
            },
            "delete": {
                "tags": tag,
                "summary": "Delete a cluster",
                "operationId": "deleteHarnessCluster",
                "parameters": [path_param("id")],
                "responses": { "204": { "description": "Deleted" }, "409": { "description": "A run is active" }, "404": off }
            }
        },
        "/_rustybin/harness/clusters/{id}/upstreams": {
            "get": {
                "tags": tag,
                "summary": "Server nodes' URLs",
                "operationId": "getHarnessUpstreams",
                "parameters": [path_param("id")],
                "responses": { "200": { "description": "Advertised URLs", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } }, "307": { "description": "This node is a member; the leader answers" }, "404": off }
            }
        },
        "/_rustybin/harness/clusters/{id}/leader": {
            "get": {
                "tags": tag,
                "summary": "The cluster's leader",
                "description": "The leader (node, control URL, term, since), the eligible nodes, whether failover is available (three or more), the succession with work scores and the history of leader changes. Every member answers; never redirected.",
                "operationId": "getHarnessLeader",
                "parameters": [path_param("id")],
                "responses": { "200": json_ok("The leader as this node sees it"), "404": off }
            }
        },
        "/_rustybin/harness/clusters/{id}/election/vote": {
            "post": {
                "tags": tag,
                "summary": "Node vote",
                "description": "A candidate asks an eligible node for its vote: `{term, candidate, snapshot}` with the candidate's node credential as bearer. Granted when the node lost the leader too, has not voted in the term and the candidate's snapshot is at least as recent.",
                "operationId": "harnessNodeVote",
                "security": [],
                "parameters": [path_param("id")],
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } },
                "responses": { "200": json_ok("`{granted, term, leader?, reason}`"), "401": { "description": "Invalid node credential" }, "403": { "description": "This node is not eligible" }, "404": off }
            }
        },
        "/_rustybin/harness/clusters/{id}/election/elected": {
            "post": {
                "tags": tag,
                "summary": "New leader announcement",
                "description": "A new leader announces its term (its node credential as bearer). A node that led an older term steps down; a member follows the new leader.",
                "operationId": "harnessNodeElected",
                "security": [],
                "parameters": [path_param("id")],
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } },
                "responses": { "200": json_ok("`{accepted}`"), "401": { "description": "Invalid node credential" }, "404": off }
            }
        },
        "/_rustybin/harness/clusters/{id}/nodes": {
            "post": {
                "tags": tag,
                "summary": "Node join",
                "description": "Called by nodes with the join token as bearer. Returns a node credential and a lease.",
                "operationId": "harnessNodeJoin",
                "security": [],
                "parameters": [path_param("id")],
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } },
                "responses": { "200": json_ok("Credential, lease, leader, clock stamps"), "401": { "description": "Invalid join token" }, "404": off }
            }
        },
        "/_rustybin/harness/clusters/{id}/nodes/{node}/heartbeat": {
            "post": {
                "tags": tag,
                "summary": "Node heartbeat",
                "description": "Called by nodes with their node credential as bearer: a long poll (up to 10 s, at most a third of the election timeout) carrying cumulative stats, the term and a work report; returns assignments, shares, stops, the lease and the snapshot when it changed. A node that does not lead answers 409 naming the leader it knows; a leader without a majority of the eligible nodes answers 503 (no lease renewal).",
                "operationId": "harnessNodeHeartbeat",
                "security": [],
                "parameters": [path_param("id"), path_param("node")],
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } },
                "responses": { "200": json_ok("Assignments and the lease"), "401": { "description": "Invalid node credential" }, "404": off }
            }
        },
        "/_rustybin/harness/clusters/{id}/runs": {
            "get": {
                "tags": tag,
                "summary": "Cluster runs",
                "operationId": "listHarnessClusterRuns",
                "parameters": [path_param("id")],
                "responses": { "200": json_ok("Runs"), "307": { "description": "This node is a member; the leader answers" }, "404": off }
            },
            "post": {
                "tags": tag,
                "summary": "Submit a scenario",
                "description": "Validated against the cluster (each client's share against the caps), not started.",
                "operationId": "createHarnessClusterRun",
                "parameters": [path_param("id")],
                "requestBody": scenario_body.clone(),
                "responses": { "201": json_ok("The run and its quorum"), "400": { "description": "Invalid scenario (`problems`)" }, "307": { "description": "This node is a member; the leader answers" }, "404": off }
            }
        },
        "/_rustybin/harness/clusters/{id}/runs/{run}": {
            "get": {
                "tags": tag,
                "summary": "Live merged stats of a cluster run",
                "operationId": "getHarnessClusterRun",
                "parameters": [path_param("id"), path_param("run")],
                "responses": { "200": json_ok("Live view"), "307": { "description": "This node is a member; the leader answers" }, "404": off }
            }
        },
        "/_rustybin/harness/clusters/{id}/runs/{run}/start": {
            "post": {
                "tags": tag,
                "summary": "Start a cluster run",
                "operationId": "startHarnessClusterRun",
                "parameters": [path_param("id"), path_param("run")],
                "responses": { "202": json_ok("Starting (synchronised)"), "409": { "description": "No quorum, a run is active, or the clients cannot carry the rate" }, "404": off }
            }
        },
        "/_rustybin/harness/clusters/{id}/runs/{run}/stop": {
            "post": {
                "tags": tag,
                "summary": "Stop a cluster run",
                "operationId": "stopHarnessClusterRun",
                "parameters": [path_param("id"), path_param("run")],
                "responses": { "202": json_ok("Stopping"), "200": json_ok("Already stopped or finished"), "404": off }
            }
        },
        "/_rustybin/harness/clusters/{id}/runs/{run}/report": {
            "get": {
                "tags": tag,
                "summary": "Merged report of a cluster run",
                "operationId": "getHarnessClusterReport",
                "parameters": [
                    path_param("id"),
                    path_param("run"),
                    { "name": "format", "in": "query", "required": false, "schema": { "type": "string", "enum": ["json", "text", "junit"] } }
                ],
                "responses": {
                    "200": { "description": "The report", "content": { "application/json": { "schema": { "type": "object" } }, "text/plain": { "schema": { "type": "string" } }, "application/xml": { "schema": { "type": "string" } } } },
                    "409": { "description": "The run has not finished" },
                    "404": off
                }
            }
        }
    })
}

fn json_ok(description: &str) -> Value {
    json!({ "description": description, "content": { "application/json": { "schema": { "type": "object" } } } })
}

fn path_param(name: &str) -> Value {
    json!({ "name": name, "in": "path", "required": true, "schema": { "type": "string" } })
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
mod cluster_tests;
#[cfg(test)]
mod failover_tests;
#[cfg(test)]
mod limit_tests;
#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod tests;
