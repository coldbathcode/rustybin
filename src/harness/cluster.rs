//! The coordinator (`docs/plans/harness.md`, sections 9 and 10, phase 3):
//! clusters this node leads, their members, runs split over the client
//! nodes and merged into one report.
//!
//! - **Membership**: a node joins with the cluster's join token and gets a
//!   node credential (an HMAC of the cluster secret, never stored) and a
//!   lease. It then heartbeats: a long poll (up to 10 s) that the leader
//!   answers at once when the cluster changed. Nodes pull; the leader never
//!   calls them.
//! - **Clock sync**: NTP-style stamps on every join and heartbeat; each
//!   node reports its offset to the leader's clock and the error bound.
//! - **Runs**: a scenario is submitted (validated, not started), then
//!   started once the quorum is there (`scale.client_nodes`,
//!   `scale.server_nodes`, the cluster's `min_nodes`). The rate is split
//!   over the client nodes by declared capacity, each gets a range of the
//!   virtual clients, the run key and a synchronised `start_at`; server
//!   nodes get the run key and the server profile.
//! - **Leases**: a client stops sending when its leader has not answered
//!   for the lease period (the engine checks [`super::safety::Lease`]).
//! - **Merging**: client nodes send cumulative counters and exact
//!   histograms; the leader merges them into one report (no percentile
//!   averaging). When the clients are done, the server nodes are asked
//!   which of the failed requests reached them, which gives `lost`.
//!
//! - **Failover** (section 9.1, phase 3b): every message carries the
//!   leader's term. The leader replicates a versioned snapshot (the public
//!   part to every node; members, runs with a checkpoint of their stats and
//!   the secrets to the eligible nodes only) and orders the eligible nodes
//!   into a succession by work. When it is gone, the first live successor
//!   is elected by a majority of the eligible nodes and resumes from the
//!   snapshot ([`Cluster::from_snapshot`]); the election code is in
//!   [`super::election`]. A leader that cannot hear a majority of the
//!   eligible nodes stops renewing leases (with three or more of them), so a
//!   minority partition stops sending; one that sees a newer term steps down.
//!
//! Bounds: 4 clusters per node, 64 nodes per cluster, 20 runs kept per
//! cluster (24 hours), timelines and samples capped like single-node runs.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use super::correlate::{now_ns, ChangesReport, RunKey, ServerReport};
use super::engine::Share;
use super::report::{
    self, decode_numbers, encode_numbers, histogram, Breakdown, Bucket, ClusterReport, Counts,
    NodeReport, Percentiles, Report, Sample, Saturation, TargetReport, MAX_SAMPLES,
};
use super::scenario::{Caps, Dur, Scenario, MAX_CLUSTER_NODES};
use super::wire::{
    Capacity, ClientAssignment, ClockReport, Collected, HeartbeatRequest, HeartbeatResponse,
    JoinRequest, JoinResponse, Leader, LeaderChange, LeaderPolicy, NodeStats, RunControl,
    ServerAssignment, ServerStatsWire, SnapClient, SnapMember, SnapRun, SnapSecrets, SnapServer,
    SnapState, Snapshot, SnapshotVersion, Successor, WorkReport,
};
use super::{Role, RunError, HARNESS_API, PREFIX};

type HmacSha256 = Hmac<Sha256>;

/// Clusters one node leads.
pub const MAX_CLUSTERS: usize = 4;
/// Nodes per cluster.
pub const MAX_NODES: usize = MAX_CLUSTER_NODES as usize;
/// Runs kept per cluster (the oldest finished run is dropped first).
pub const MAX_CLUSTER_RUNS: usize = 20;
/// A cluster with no member, no run and no call for this long is dropped.
pub const CLUSTER_IDLE_TTL: Duration = Duration::from_secs(24 * 3600);
/// A finished run is forgotten after this long.
pub const RUN_TTL: Duration = Duration::from_secs(24 * 3600);
pub const DEFAULT_LEASE: Duration = Duration::from_secs(5);
pub const MIN_LEASE: Duration = Duration::from_millis(500);
pub const MAX_LEASE: Duration = Duration::from_secs(60);
/// Longest heartbeat long poll.
pub const MAX_WAIT: Duration = Duration::from_secs(10);
/// How long the leader waits for the server nodes' `lost` answers.
pub const COLLECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Most failed sequence numbers looked up on the server nodes per run.
pub const MAX_COLLECT_TOTAL: usize = 262_144;
/// Most timeline buckets kept per node and run (24 hours: 3,600 one-second
/// buckets, then ten-second buckets).
pub const MAX_NODE_BUCKETS: usize = 12_000;
/// How often the leader advances its runs (start, completion, timeouts).
pub const TICK: Duration = Duration::from_millis(100);
/// Shortest join token accepted.
pub const MIN_TOKEN_LEN: usize = 16;
/// Leader changes, joins and leaves kept per cluster.
pub const MAX_EVENTS: usize = 64;
/// The synchronised start is at least this far ahead.
pub const MIN_START_LEAD: Duration = Duration::from_millis(500);
pub const MAX_START_LEAD: Duration = Duration::from_secs(5);
/// Election timeout bounds (`RUSTYBIN_HARNESS_ELECTION_TIMEOUT`, per
/// cluster; always below the lease).
pub const MIN_ELECTION_TIMEOUT: Duration = Duration::from_millis(200);
pub const DEFAULT_ELECTION_TIMEOUT: Duration = Duration::from_secs(3);
/// A running run is checkpointed into the snapshot this often (or every
/// lease, when that is shorter).
pub const CHECKPOINT_EVERY: Duration = Duration::from_secs(5);
/// Leader changes kept in the history.
pub const MAX_HISTORY: usize = 32;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

pub(super) fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Cluster ids, node ids: letters, digits, `.`, `_`, `-`, at most 64.
pub fn safe_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn rfc3339_of_ns(ns: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(
        (ns / 1_000_000_000) as i64,
        (ns % 1_000_000_000) as u32,
    )
    .map(|d| d.to_rfc3339())
    .unwrap_or_default()
}

/// Why a node route refused a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeError {
    pub status: u16,
    pub error: String,
}

impl NodeError {
    pub fn new(status: u16, error: impl Into<String>) -> Self {
        Self {
            status,
            error: error.into(),
        }
    }
}

/// `min_nodes` of a cluster: the run waits for this many of each role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MinNodes {
    #[serde(default)]
    pub client: u32,
    #[serde(default)]
    pub server: u32,
}

/// `POST /_rustybin/harness/clusters`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCluster {
    #[serde(default)]
    pub cluster_id: Option<String>,
    #[serde(default)]
    pub min_nodes: Option<MinNodes>,
    /// How long a client keeps sending without an answer from the leader.
    #[serde(default)]
    pub lease: Option<Dur>,
    /// A join token chosen by the caller (a platform pod secret); random
    /// when absent. At least 16 characters.
    #[serde(default)]
    pub join_token: Option<String>,
    /// How long without the leader before an election starts (200ms, below
    /// the lease; default `RUSTYBIN_HARNESS_ELECTION_TIMEOUT`, 3 s).
    #[serde(default)]
    pub election_timeout: Option<Dur>,
}

/// The node that creates (or takes over) a cluster.
#[derive(Clone, Debug, Default)]
pub struct LeaderInfo {
    pub node_id: String,
    /// Its control URL as the other nodes reach it.
    pub control_url: Option<String>,
    pub report_dir: Option<PathBuf>,
    /// The default election timeout (`RUSTYBIN_HARNESS_ELECTION_TIMEOUT`).
    pub election_timeout: Duration,
}

/// One node of a cluster.
#[derive(Clone, Debug)]
struct Member {
    node_id: String,
    roles: Vec<Role>,
    advertise: Option<String>,
    capacity: Capacity,
    version: String,
    incarnation: String,
    joined_at: chrono::DateTime<chrono::Utc>,
    last_seen: Instant,
    /// Heartbeats being held (long polls): the node is there.
    polling: u32,
    clock: Option<ClockReport>,
    left: bool,
    in_flight: u64,
    control_url: Option<String>,
    policy: LeaderPolicy,
    work: WorkReport,
}

impl Member {
    fn has(&self, role: Role) -> bool {
        self.roles.contains(&role)
    }

    /// May lead: a control URL, a policy other than `never`, still a member.
    fn eligible(&self) -> bool {
        self.control_url.is_some() && self.policy != LeaderPolicy::Never && !self.left
    }

    /// Heard within `window` (a held long poll counts).
    fn heard_within(&self, window: Duration) -> bool {
        self.polling > 0 || self.last_seen.elapsed() <= window
    }

    fn snap(&self) -> SnapMember {
        SnapMember {
            node_id: self.node_id.clone(),
            roles: self.roles.clone(),
            advertise: self.advertise.clone(),
            control_url: self.control_url.clone(),
            leader: self.policy,
            capacity: self.capacity,
            version: self.version.clone(),
            incarnation: self.incarnation.clone(),
            joined_at: self.joined_at.to_rfc3339(),
            left: self.left,
        }
    }

    fn health(&self, lease: Duration) -> &'static str {
        if self.left {
            "left"
        } else if self.polling > 0 || self.last_seen.elapsed() <= lease {
            "healthy"
        } else if self.last_seen.elapsed() <= lease * 2 {
            "late"
        } else {
            "lost"
        }
    }

    fn alive(&self, lease: Duration) -> bool {
        !self.left && (self.polling > 0 || self.last_seen.elapsed() <= lease * 2)
    }

    fn describe(&self, lease: Duration) -> Value {
        json!({
            "node_id": self.node_id,
            "roles": self.roles.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
            "health": self.health(lease),
            "advertise": self.advertise,
            "capacity": self.capacity,
            "version": self.version,
            "joined_at": self.joined_at.to_rfc3339(),
            "last_seen_ms": self.last_seen.elapsed().as_millis() as u64,
            "clock_offset_ms": self.clock.map(|c| c.offset_ns as f64 / 1e6),
            "clock_error_ms": self.clock.map(|c| c.error_ns as f64 / 1e6),
            "in_flight": self.in_flight,
            "control_url": self.control_url,
            "leader_policy": self.policy.as_str(),
            "eligible": self.eligible(),
            "work": self.work,
        })
    }
}

/// What the run logic needs of a member, copied out of the cluster lock.
#[derive(Clone, Debug)]
struct MemberView {
    roles: Vec<Role>,
    alive: bool,
    clock: Option<ClockReport>,
    capacity: Capacity,
    /// Time since the leader last heard from the node (zero while a long
    /// poll is held).
    silent: Duration,
    left: bool,
}

/// Where a cluster run is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    /// Validated, not started.
    Created,
    /// Assigned; the synchronised start is ahead.
    Starting,
    Running,
    /// The clients are done; the server nodes are asked about failures.
    Collecting,
    Finished,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Created => "created",
            RunState::Starting => "starting",
            RunState::Running => "running",
            RunState::Collecting => "collecting",
            RunState::Finished => "finished",
        }
    }

    pub fn active(self) -> bool {
        matches!(
            self,
            RunState::Starting | RunState::Running | RunState::Collecting
        )
    }
}

/// A client node's part in a run.
struct ClientPart {
    node_id: String,
    share: Share,
    vu_from: u32,
    vu_to: u32,
    /// The last cumulative stats (timeline kept apart).
    stats: Option<NodeStats>,
    timeline: Vec<Bucket>,
    /// The node's buckets so far (the timeline may still be arriving).
    timeline_total: usize,
    /// When the stats arrived.
    stats_at: Option<Instant>,
    /// The stats came with a snapshot (a new leader's starting point), not
    /// from the node: whatever the node sends replaces them.
    from_checkpoint: bool,
    /// Its share was re-split over the others: the node went silent past
    /// its lease, or it took over as leader and handed its share back.
    gone: bool,
    gone_at: Option<Instant>,
    /// It handed its share back as the new leader.
    shed: bool,
}

impl ClientPart {
    fn new(node_id: String, share: Share, vu_from: u32, vu_to: u32) -> Self {
        Self {
            node_id,
            share,
            vu_from,
            vu_to,
            stats: None,
            timeline: Vec::new(),
            timeline_total: 0,
            stats_at: None,
            from_checkpoint: false,
            gone: false,
            gone_at: None,
            shed: false,
        }
    }

    /// The node's final stats arrived (from the node itself).
    fn done(&self) -> bool {
        !self.from_checkpoint && self.stats.as_ref().is_some_and(|s| s.done)
    }

    /// The final stats and the whole timeline arrived.
    fn complete(&self) -> bool {
        self.done() && self.timeline.len() >= self.timeline_total
    }
}

/// A server node's part in a run.
struct ServerPart {
    node_id: String,
    stats: Option<ServerStatsWire>,
    collected: Option<Collected>,
}

struct CollectState {
    since: Instant,
    /// Failed sequence numbers by client node, and their encoded form for
    /// the server nodes.
    failed: BTreeMap<String, Vec<u64>>,
    encoded: BTreeMap<String, String>,
    /// Failures not looked up (over the caps).
    untracked: u64,
}

struct RunInner {
    state: RunState,
    stop_reason: Option<String>,
    /// The synchronised start on the leader's clock (Unix ns).
    start_at_ns: u64,
    start_at: Option<Instant>,
    started: Option<Instant>,
    /// After this the clients that sent no final stats count as lost.
    deadline: Option<Instant>,
    clients: Vec<ClientPart>,
    servers: Vec<ServerPart>,
    collect: Option<CollectState>,
    finished: Option<Instant>,
}

/// One run of a scenario on a cluster.
pub struct ClusterRun {
    pub id: String,
    pub scenario: Scenario,
    pub hash: String,
    key: RunKey,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub created: Instant,
    inner: Mutex<RunInner>,
    report: OnceLock<Report>,
    done: watch::Sender<bool>,
}

impl std::fmt::Debug for ClusterRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterRun")
            .field("id", &self.id)
            .field("scenario", &self.scenario.name)
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl ClusterRun {
    pub fn state(&self) -> RunState {
        lock(&self.inner).state
    }

    pub fn is_active(&self) -> bool {
        self.state().active()
    }

    pub fn report(&self) -> Option<&Report> {
        self.report.get()
    }

    /// Resolves when the run finished.
    pub async fn wait(&self) {
        let mut rx = self.done.subscribe();
        if self.report.get().is_some() {
            return;
        }
        let _ = rx.wait_for(|v| *v).await;
    }

    pub fn summary(&self) -> Value {
        let inner = lock(&self.inner);
        json!({
            "run_id": self.id,
            "scenario": self.scenario.name,
            "state": inner.state.as_str(),
            "created_at": self.created_at.to_rfc3339(),
            "elapsed_secs": inner.started.map_or(0.0, |s| s.elapsed().as_secs_f64()),
            "planned_secs": self.scenario.duration().as_secs_f64(),
            "clients": inner.clients.len(),
            "servers": inner.servers.len(),
            "passed": self.report.get().map(|r| r.passed),
            "exit_code": self.report.get().map(|r| r.exit_code),
        })
    }

    /// The run in the snapshot: assignments and the checkpoint of the
    /// stats. Counters for every client part (its node reports again to a
    /// new leader); histograms and failures too for the parts that will not
    /// (this leader's own, and nodes gone silent).
    fn snap(&self, leader: &str, silent: &HashSet<String>) -> SnapRun {
        let inner = lock(&self.inner);
        let now_mono = Instant::now();
        let now_wall = now_ns();
        let wall_of =
            |t: Instant| now_wall.saturating_sub(now_mono.duration_since(t).as_nanos() as u64);
        SnapRun {
            id: self.id.clone(),
            scenario: self.scenario.clone(),
            hash: self.hash.clone(),
            key: self.key.to_hex(),
            created_at: self.created_at.to_rfc3339(),
            state: inner.state.as_str().into(),
            stop_reason: inner.stop_reason.clone(),
            start_at_ns: inner.start_at_ns,
            clients: inner
                .clients
                .iter()
                .map(|p| {
                    let whole = p.node_id == leader || p.gone || silent.contains(&p.node_id);
                    SnapClient {
                        node_id: p.node_id.clone(),
                        share: p.share,
                        vu_from: p.vu_from,
                        vu_to: p.vu_to,
                        gone: p.gone,
                        stats: p.stats.as_ref().map(|st| {
                            if whole {
                                st.clone()
                            } else {
                                counters_only(st)
                            }
                        }),
                        stats_at_ns: p.stats_at.map_or(0, wall_of),
                    }
                })
                .collect(),
            servers: inner
                .servers
                .iter()
                .map(|p| SnapServer {
                    node_id: p.node_id.clone(),
                    stats: p.stats.clone(),
                })
                .collect(),
        }
    }

    /// A run of a snapshot, resumed by a new leader.
    fn from_snap(r: &SnapRun, lease: Duration) -> Result<Arc<ClusterRun>, String> {
        let key = RunKey::from_hex(&r.key).ok_or("the snapshot's run key is not valid")?;
        let mut scenario = r.scenario.clone();
        scenario.normalise();
        let state = match r.state.as_str() {
            "created" => RunState::Created,
            "starting" => RunState::Starting,
            // A run that was collecting collects again: its clients send
            // their final stats to the new leader.
            "running" | "collecting" => RunState::Running,
            other => return Err(format!("run {}: unknown state {other:?}", r.id)),
        };
        let now = Instant::now();
        let now_wall = now_ns();
        let mono_of = |ns: u64| {
            if ns >= now_wall {
                now + Duration::from_nanos(ns - now_wall)
            } else {
                now.checked_sub(Duration::from_nanos(now_wall - ns))
                    .unwrap_or(now)
            }
        };
        let start_at = (r.start_at_ns > 0).then(|| mono_of(r.start_at_ns));
        let deadline = start_at.map(|at| {
            at + scenario.duration()
                + scenario.timeout()
                + super::engine::DRAIN_GRACE
                + lease * 2
                + Duration::from_secs(10)
        });
        let clients = r
            .clients
            .iter()
            .take(MAX_NODES)
            .map(|c| ClientPart {
                node_id: c.node_id.clone(),
                share: if c.share.valid() {
                    c.share
                } else {
                    Share::default()
                },
                vu_from: c.vu_from,
                vu_to: c.vu_to,
                stats: c.stats.clone(),
                timeline: Vec::new(),
                timeline_total: 0,
                stats_at: (c.stats_at_ns > 0).then(|| mono_of(c.stats_at_ns)),
                from_checkpoint: c.stats.is_some(),
                gone: c.gone,
                gone_at: c.gone.then_some(now),
                shed: false,
            })
            .collect();
        let servers = r
            .servers
            .iter()
            .take(MAX_NODES)
            .map(|p| ServerPart {
                node_id: p.node_id.clone(),
                stats: p.stats.clone(),
                collected: None,
            })
            .collect();
        let created_at = chrono::DateTime::parse_from_rfc3339(&r.created_at)
            .map(|d| d.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now());
        let (done, _) = watch::channel(false);
        Ok(Arc::new(ClusterRun {
            id: r.id.clone(),
            hash: scenario.hash(),
            scenario,
            key,
            created_at,
            created: now,
            inner: Mutex::new(RunInner {
                state,
                stop_reason: r.stop_reason.clone(),
                start_at_ns: r.start_at_ns,
                start_at,
                started: (state == RunState::Running).then(|| start_at.unwrap_or(now)),
                deadline,
                clients,
                servers,
                collect: None,
                finished: None,
            }),
            report: OnceLock::new(),
            done,
        }))
    }

    /// This node no longer leads: whoever waits for the run (a headless
    /// coordinator) stops waiting; the run carries on with the new leader.
    pub fn hand_over(&self) {
        let _ = self.done.send(true);
    }

    fn accept_client(&self, node_id: &str, mut stats: NodeStats) {
        let mut inner = lock(&self.inner);
        if inner.state == RunState::Finished || inner.state == RunState::Created {
            return;
        }
        let Some(part) = inner.clients.iter_mut().find(|p| p.node_id == node_id) else {
            return;
        };
        if part.complete() {
            return;
        }
        let from = stats.timeline_from;
        if from <= part.timeline.len() {
            part.timeline.truncate(from);
            let room = MAX_NODE_BUCKETS.saturating_sub(part.timeline.len());
            let take = stats.timeline.len().min(room);
            part.timeline.extend(stats.timeline.drain(..take));
        }
        // Over the bucket cap the rest never comes: count what is kept.
        part.timeline_total = stats.timeline_total.min(MAX_NODE_BUCKETS);
        stats.timeline = Vec::new();
        stats.samples.truncate(super::engine::MAX_WIRE_SAMPLES);
        part.stats = Some(stats);
        part.stats_at = Some(Instant::now());
        part.from_checkpoint = false;
    }

    fn accept_server(&self, node_id: &str, mut stats: ServerStatsWire) {
        let mut inner = lock(&self.inner);
        if inner.state == RunState::Finished || inner.state == RunState::Created {
            return;
        }
        let collecting = inner.state == RunState::Collecting;
        let Some(part) = inner.servers.iter_mut().find(|p| p.node_id == node_id) else {
            return;
        };
        if let Some(c) = stats.collected.take() {
            if collecting {
                part.collected = Some(c);
            }
        }
        part.stats = Some(stats);
    }

    /// Move the run along: start, completion of the clients, the `lost`
    /// lookup, the report. Returns true when the cluster changed (the
    /// version is bumped by the caller, outside every lock).
    fn advance(&self, cluster: &ClusterInfo, members: &HashMap<String, MemberView>) -> bool {
        let mut inner = lock(&self.inner);
        let mut changed = false;
        let now = Instant::now();
        if inner.state == RunState::Starting && inner.start_at.is_some_and(|at| now >= at) {
            inner.state = RunState::Running;
            inner.started = Some(now);
        }
        // A client node silent past its lease has stopped sending: its share
        // goes to the others, so the cluster-wide rate stays as planned.
        if matches!(inner.state, RunState::Starting | RunState::Running)
            && inner.stop_reason.is_none()
        {
            let grace = cluster.lease + cluster.heartbeat_every;
            let mut gone = Vec::new();
            for p in inner.clients.iter_mut() {
                if p.gone || p.done() {
                    continue;
                }
                let silent = members
                    .get(&p.node_id)
                    .is_none_or(|m| m.left || m.silent > grace);
                if silent {
                    p.gone = true;
                    p.gone_at = Some(now);
                    gone.push(p.node_id.clone());
                }
            }
            if !gone.is_empty() {
                let note = resplit(self, &mut inner, members);
                tracing::warn!(
                    target: "rustybin::harness",
                    cluster = %cluster.id,
                    run = %self.id,
                    nodes = ?gone,
                    "harness client node(s) silent past the lease: their share is re-split{}",
                    note.map(|n| format!(" ({n})")).unwrap_or_default()
                );
                changed = true;
            }
        }
        if matches!(inner.state, RunState::Starting | RunState::Running) {
            let past_deadline = inner.deadline.is_some_and(|d| now >= d);
            let done = inner.clients.iter().all(|p| {
                p.complete()
                    || (p.gone && !p.shed)
                    || !members.get(&p.node_id).is_some_and(|m| m.alive)
                    || past_deadline
            });
            if done {
                if inner.started.is_none() {
                    inner.started = Some(now);
                }
                let mut failed = BTreeMap::new();
                let mut untracked = 0u64;
                let mut budget = MAX_COLLECT_TOTAL;
                for p in &inner.clients {
                    let Some(stats) = &p.stats else { continue };
                    untracked += stats.failed_untracked;
                    let mut seqs =
                        decode_numbers(&stats.failed_seqs, super::engine::MAX_WIRE_FAILED)
                            .unwrap_or_default();
                    if seqs.len() > budget {
                        untracked += (seqs.len() - budget) as u64;
                        seqs.truncate(budget);
                    }
                    budget -= seqs.len();
                    if !seqs.is_empty() {
                        failed.insert(p.node_id.clone(), seqs);
                    }
                }
                let encoded = failed
                    .iter()
                    .map(|(k, v)| (k.clone(), encode_numbers(v)))
                    .collect();
                inner.collect = Some(CollectState {
                    since: now,
                    failed,
                    encoded,
                    untracked,
                });
                inner.state = RunState::Collecting;
                changed = true;
            }
        }
        if inner.state == RunState::Collecting {
            let since = inner.collect.as_ref().map_or(now, |c| c.since);
            let answered = inner.servers.iter().all(|p| {
                p.collected.is_some() || !members.get(&p.node_id).is_some_and(|m| m.alive)
            });
            if answered || since.elapsed() >= COLLECT_TIMEOUT {
                let report = build_report(self, &inner, cluster, members);
                inner.state = RunState::Finished;
                inner.finished = Some(now);
                // The report holds what matters; the per-node data goes.
                for p in &mut inner.clients {
                    p.timeline = Vec::new();
                    if let Some(s) = &mut p.stats {
                        s.failed_seqs = String::new();
                        s.samples = Vec::new();
                    }
                }
                inner.collect = None;
                drop(inner);
                tracing::info!(
                    target: "rustybin::harness",
                    cluster = %cluster.id,
                    run = %self.id,
                    state = %report.state,
                    sent = report.totals.sent,
                    completed = report.totals.completed,
                    upstream = report.totals.upstream,
                    gateway = report.totals.gateway,
                    lost = report.totals.lost,
                    p99_ms = report.latency.p99_ms,
                    exit_code = report.exit_code,
                    "harness cluster run ended"
                );
                if let Some(dir) = &cluster.report_dir {
                    let path = dir.join(format!("{}.json", self.id));
                    match serde_json::to_vec_pretty(&report) {
                        Ok(bytes) => {
                            if let Err(e) = std::fs::create_dir_all(dir)
                                .and_then(|()| std::fs::write(&path, bytes))
                            {
                                tracing::warn!(
                                    "harness report {} not written: {e}",
                                    path.display()
                                );
                            }
                        }
                        Err(e) => tracing::warn!("harness report not serialised: {e}"),
                    }
                }
                let _ = self.report.set(report);
                let _ = self.done.send(true);
                return true;
            }
        }
        changed
    }

    /// Stop: a run not started yet ends at once; a running one tells its
    /// clients on their next heartbeat. Returns whether anything changed.
    fn stop(&self, reason: &str, cluster: &ClusterInfo) -> bool {
        let mut inner = lock(&self.inner);
        match inner.state {
            RunState::Created => {
                inner.stop_reason = Some(reason.to_string());
                inner.state = RunState::Collecting;
                inner.collect = Some(CollectState {
                    since: Instant::now(),
                    failed: BTreeMap::new(),
                    encoded: BTreeMap::new(),
                    untracked: 0,
                });
                drop(inner);
                self.advance(cluster, &HashMap::new());
                true
            }
            RunState::Starting | RunState::Running => {
                if inner.stop_reason.is_none() {
                    inner.stop_reason = Some(reason.to_string());
                }
                true
            }
            RunState::Collecting | RunState::Finished => false,
        }
    }

    /// Live view: the merged stats so far, the nodes, the state.
    fn snapshot(&self, members: &HashMap<String, MemberView>) -> Value {
        let inner = lock(&self.inner);
        let m = merge(self, &inner);
        let planned = self.scenario.duration().as_secs_f64();
        let elapsed = inner.started.map_or(0.0, |s| match inner.finished {
            Some(f) => f.saturating_duration_since(s).as_secs_f64(),
            None => s.elapsed().as_secs_f64(),
        });
        let latency = Percentiles::from_histogram(&m.corrected);
        // The last bucket every node has reported.
        let complete = inner
            .clients
            .iter()
            .filter(|p| p.stats.is_some())
            .map(|p| p.timeline.len())
            .min()
            .unwrap_or(0);
        let last = complete
            .checked_sub(1)
            .and_then(|i| m.timeline.get(i))
            .cloned();
        let layers: Vec<Value> = self
            .scenario
            .layers
            .iter()
            .zip(&m.layers)
            .map(|(l, p)| {
                let t = Duration::from_secs_f64(elapsed)
                    .saturating_sub(l.starts_after.map_or(Duration::ZERO, |d| d.0));
                json!({
                    "name": l.name,
                    "model": l.model.as_str(),
                    "completed": p.counts.completed,
                    "ok": p.counts.ok,
                    "errors": p.counts.errors(),
                    "p99_ms": Percentiles::from_histogram(&p.corrected).p99_ms,
                    "target_now": if inner.started.is_some() && inner.state != RunState::Finished { l.pattern.value_at(t) } else { None },
                })
            })
            .collect();
        let arrivals: u64 = inner
            .servers
            .iter()
            .filter_map(|p| p.stats.as_ref().map(|s| s.report.arrivals))
            .sum();
        let nodes: Vec<Value> = inner
            .clients
            .iter()
            .map(|p| {
                let s = p.stats.as_ref();
                json!({
                    "node_id": p.node_id,
                    "role": "client",
                    "share": p.share.fraction(),
                    "virtual_clients": [p.vu_from, p.vu_to],
                    "state": s.map_or("assigned", |s| s.state.as_str()),
                    "final": s.is_some_and(|s| s.done),
                    "sent": s.map_or(0, |s| s.counts.sent),
                    "completed": s.map_or(0, |s| s.counts.completed),
                    "in_flight": s.map_or(0, |s| s.in_flight),
                    "saturated": s.is_some_and(|s| s.saturation.saturated),
                    "start_lag_ms": s.and_then(|s| s.start_lag_ms),
                    "error": s.and_then(|s| s.error.clone()),
                    "alive": members.get(&p.node_id).is_some_and(|m| m.alive),
                })
            })
            .chain(inner.servers.iter().map(|p| {
                json!({
                    "node_id": p.node_id,
                    "role": "server",
                    "arrivals": p.stats.as_ref().map_or(0, |s| s.report.arrivals),
                    "answered": p.collected.is_some(),
                    "alive": members.get(&p.node_id).is_some_and(|m| m.alive),
                })
            }))
            .collect();
        json!({
            "run_id": self.id,
            "cluster": true,
            "scenario": self.scenario.name,
            "hash": self.hash,
            "state": inner.state.as_str(),
            "stop_reason": inner.stop_reason,
            "created_at": self.created_at.to_rfc3339(),
            "start_at": (inner.start_at_ns > 0).then(|| rfc3339_of_ns(inner.start_at_ns)),
            "elapsed_secs": elapsed,
            "planned_secs": planned,
            "progress": if planned > 0.0 { (elapsed / planned).min(1.0) } else { 1.0 },
            "requested": m.counts.requested,
            "sent": m.counts.sent,
            "not_sent": m.counts.not_sent,
            "in_flight": m.in_flight,
            "completed": m.counts.completed,
            "ok": m.counts.ok,
            "errors": m.counts.errors(),
            "upstream": m.counts.upstream,
            "gateway": m.counts.gateway,
            "transport": m.counts.transport,
            "statuses": m.counts.statuses,
            "gateway_statuses": m.counts.gateway_statuses,
            "transport_kinds": m.counts.transport_kinds,
            "latency": latency,
            "upstream_latency": Percentiles::from_histogram(&m.upstream),
            "overhead": Percentiles::from_histogram(&m.overhead),
            "server_arrivals": (!inner.servers.is_empty()).then_some(arrivals),
            "last_second": last,
            "saturated": m.saturation.saturated,
            "layers": layers,
            "nodes": nodes,
            "report_ready": self.report.get().is_some(),
            "exit_code": self.report.get().map(|r| r.exit_code),
            "passed": self.report.get().map(|r| r.passed),
        })
    }
}

/// What a run needs to know of its cluster.
struct ClusterInfo {
    id: String,
    leader: String,
    term: u64,
    history: Vec<LeaderChange>,
    report_dir: Option<PathBuf>,
    lease: Duration,
    heartbeat_every: Duration,
}

/// The counters of a node's stats, without histograms, samples or failures
/// (the snapshot carries these for nodes that report again anyway).
fn counters_only(st: &NodeStats) -> NodeStats {
    let strip = |parts: &[super::wire::WirePart]| {
        parts
            .iter()
            .map(|w| super::wire::WirePart {
                counts: w.counts.clone(),
                ..Default::default()
            })
            .collect()
    };
    NodeStats {
        run_id: st.run_id.clone(),
        state: st.state.clone(),
        done: st.done,
        error: st.error.clone(),
        stopped_by: st.stopped_by.clone(),
        elapsed_secs: st.elapsed_secs,
        start_lag_ms: st.start_lag_ms,
        in_flight: st.in_flight,
        counts: st.counts.clone(),
        layers: strip(&st.layers),
        targets: strip(&st.targets),
        target_reports: st.target_reports.clone(),
        saturation: st.saturation.clone(),
        failed_untracked: st.failed_untracked,
        ..NodeStats::default()
    }
}

/// A node's credential: an HMAC of the cluster secret over the cluster, the
/// node and its join (incarnation). Never stored; recomputed to check.
pub fn node_credential(
    secret: &[u8; 32],
    cluster: &str,
    node_id: &str,
    incarnation: &str,
) -> String {
    let Ok(mut mac) = HmacSha256::new_from_slice(secret) else {
        return String::new();
    };
    mac.update(format!("node|{cluster}|{node_id}|{incarnation}").as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The cluster secret from its hex form.
pub fn decode_secret(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 {
        return Err("the snapshot's cluster secret is not valid".into());
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| "the snapshot's cluster secret is not valid".to_string())?;
    }
    Ok(out)
}

/// A node's work score (lower leads first): the rate it is assigned to
/// send, the marked requests arriving per second, requests in flight, plus
/// 100 per busy CPU core and 1 per millisecond of scheduler lag p99.
pub fn work_score(assigned_rate: f64, in_flight: u64, w: &WorkReport) -> f64 {
    let score = assigned_rate
        + w.arrivals_per_sec.max(0.0)
        + in_flight as f64
        + 100.0 * w.cpu.unwrap_or(0.0).max(0.0)
        + w.lag_p99_ms.max(0.0);
    if score.is_finite() {
        (score * 100.0).round() / 100.0
    } else {
        f64::MAX
    }
}

/// Re-split the rate over the client parts still sending, by declared
/// capacity like the start (capped by `scale.max_rate_per_node`). When their
/// capacity is short of the peak, each runs at its cap and the rest of the
/// rate is not carried; the note says how much.
fn resplit(
    run: &ClusterRun,
    inner: &mut RunInner,
    members: &HashMap<String, MemberView>,
) -> Option<String> {
    let per_node_cap = run.scenario.scale.max_rate_per_node;
    let active: Vec<usize> = inner
        .clients
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.gone && !p.done())
        .map(|(i, _)| i)
        .collect();
    if active.is_empty() {
        return Some("no client node is left to carry the rate".into());
    }
    let caps: Vec<u64> = active
        .iter()
        .map(|&i| {
            let declared = members
                .get(&inner.clients[i].node_id)
                .map_or(1, |m| m.capacity.max_rate);
            per_node_cap.map_or(declared, |c| declared.min(c)).max(1)
        })
        .collect();
    let total: u64 = caps.iter().sum();
    let peak = run.scenario.peak_rate();
    let carried = if peak > total as f64 {
        total as f64 / peak
    } else {
        1.0
    };
    let vc = run.scenario.scale.virtual_clients;
    let mut acc = 0u64;
    for (k, &i) in active.iter().enumerate() {
        let from = acc as f64 / total as f64 * carried;
        acc += caps[k];
        let to = if acc == total && carried >= 1.0 {
            1.0
        } else {
            acc as f64 / total as f64 * carried
        };
        let share = Share { from, to };
        let (base, count) = share.virtual_clients(vc);
        let p = &mut inner.clients[i];
        p.share = share;
        p.vu_from = base;
        p.vu_to = base + count;
    }
    (carried < 1.0).then(|| {
        format!(
            "the client nodes left carry {:.0}% of the peak rate",
            carried * 100.0
        )
    })
}

/// The client-side stats of a run merged over its nodes.
struct Merged {
    counts: Counts,
    corrected: hdrhistogram::Histogram<u64>,
    service: hdrhistogram::Histogram<u64>,
    upstream: hdrhistogram::Histogram<u64>,
    overhead: hdrhistogram::Histogram<u64>,
    request_leg: hdrhistogram::Histogram<u64>,
    response_leg: hdrhistogram::Histogram<u64>,
    lag: hdrhistogram::Histogram<u64>,
    layers: Vec<MergedPart>,
    targets: Vec<MergedPart>,
    target_reports: Vec<TargetReport>,
    timeline: Vec<Bucket>,
    saturation: Saturation,
    samples: Vec<Sample>,
    in_flight: u64,
    duration_secs: f64,
}

struct MergedPart {
    counts: Counts,
    corrected: hdrhistogram::Histogram<u64>,
}

/// Add one bucket of another node (same second) into `into`.
fn merge_bucket(into: &mut Bucket, b: &Bucket) {
    into.width_secs = into.width_secs.max(b.width_secs);
    into.requested += b.requested;
    into.sent += b.sent;
    into.not_sent += b.not_sent;
    into.completed += b.completed;
    into.ok += b.ok;
    into.errors += b.errors;
    into.transport += b.transport;
    into.status_2xx += b.status_2xx;
    into.status_3xx += b.status_3xx;
    into.status_4xx += b.status_4xx;
    into.status_5xx += b.status_5xx;
    into.upstream += b.upstream;
    into.gateway += b.gateway;
    into.rate = into.completed as f64 / into.width_secs.max(1e-9);
    // Per-second percentiles are not shipped as histograms: the timeline
    // shows the slowest node's. The run's percentiles are exact.
    into.p50_ms = into.p50_ms.max(b.p50_ms);
    into.p99_ms = into.p99_ms.max(b.p99_ms);
    into.max_ms = into.max_ms.max(b.max_ms);
    into.service_p99_ms = into.service_p99_ms.max(b.service_p99_ms);
    into.upstream_p99_ms = into.upstream_p99_ms.max(b.upstream_p99_ms);
    into.overhead_p99_ms = into.overhead_p99_ms.max(b.overhead_p99_ms);
    into.lag_p99_ms = into.lag_p99_ms.max(b.lag_p99_ms);
    into.lag_max_ms = into.lag_max_ms.max(b.lag_max_ms);
    into.in_flight += b.in_flight;
    into.saturated |= b.saturated;
}

fn merge(run: &ClusterRun, inner: &RunInner) -> Merged {
    let nlayers = run.scenario.layers.len();
    let ntargets = run.scenario.targets.len();
    let part = || MergedPart {
        counts: Counts::default(),
        corrected: histogram(2),
    };
    let mut m = Merged {
        counts: Counts::default(),
        corrected: histogram(3),
        service: histogram(3),
        upstream: histogram(3),
        overhead: histogram(3),
        request_leg: histogram(3),
        response_leg: histogram(3),
        lag: histogram(3),
        layers: (0..nlayers).map(|_| part()).collect(),
        targets: (0..ntargets).map(|_| part()).collect(),
        target_reports: Vec::new(),
        timeline: Vec::new(),
        saturation: Saturation::default(),
        samples: Vec::new(),
        in_flight: 0,
        duration_secs: 0.0,
    };
    let mut buckets_secs: Vec<f64> = Vec::new();
    for p in &inner.clients {
        let Some(st) = &p.stats else { continue };
        m.counts.add(&st.counts);
        m.in_flight += st.in_flight;
        m.duration_secs = m.duration_secs.max(st.elapsed_secs);
        for (wire, h) in [
            (&st.hist.corrected, &mut m.corrected),
            (&st.hist.service, &mut m.service),
            (&st.hist.upstream, &mut m.upstream),
            (&st.hist.overhead, &mut m.overhead),
            (&st.hist.request_leg, &mut m.request_leg),
            (&st.hist.response_leg, &mut m.response_leg),
            (&st.hist.lag, &mut m.lag),
        ] {
            let _ = wire.add_to(h);
        }
        for (dst, src) in m.layers.iter_mut().zip(&st.layers) {
            dst.counts.add(&src.counts);
            let _ = src.corrected.add_to(&mut dst.corrected);
        }
        for (dst, src) in m.targets.iter_mut().zip(&st.targets) {
            dst.counts.add(&src.counts);
            let _ = src.corrected.add_to(&mut dst.corrected);
        }
        for (i, t) in st.target_reports.iter().enumerate() {
            match m.target_reports.get_mut(i) {
                Some(existing) => {
                    for a in &t.pinned {
                        if !existing.pinned.contains(a) && existing.pinned.len() < 64 {
                            existing.pinned.push(a.clone());
                        }
                    }
                    existing.verified &= t.verified;
                }
                None => m.target_reports.push(t.clone()),
            }
        }
        for (i, b) in p.timeline.iter().enumerate() {
            match m.timeline.get_mut(i) {
                Some(into) => merge_bucket(into, b),
                None => m.timeline.push(b.clone()),
            }
        }
        let s = &st.saturation;
        if s.saturated {
            m.saturation.saturated = true;
        }
        for r in &s.reasons {
            if m.saturation.reasons.len() < 64 {
                m.saturation.reasons.push(format!("{}: {r}", p.node_id));
            }
        }
        for b in &s.buckets_secs {
            if !buckets_secs.contains(b) && buckets_secs.len() < 100 {
                buckets_secs.push(*b);
            }
        }
        m.saturation.inflight_cap += s.inflight_cap;
        m.saturation.threads += s.threads;
        m.saturation.descriptor_limit = match (m.saturation.descriptor_limit, s.descriptor_limit) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if let Some(c) = s.cpu_seconds {
            m.saturation.cpu_seconds = Some(m.saturation.cpu_seconds.unwrap_or(0.0) + c);
        }
        m.samples.extend(st.samples.iter().cloned());
    }
    buckets_secs.sort_by(f64::total_cmp);
    m.saturation.buckets_secs = buckets_secs;
    m.saturation.scheduler_lag = Percentiles::from_histogram(&m.lag);
    m
}

fn merge_map(into: &mut BTreeMap<String, u64>, from: &BTreeMap<String, u64>, cap: usize) {
    for (k, v) in from {
        if let Some(n) = into.get_mut(k) {
            *n += v;
        } else if into.len() < cap {
            into.insert(k.clone(), *v);
        } else {
            *into.entry("other".into()).or_default() += v;
        }
    }
}

/// The final report of a cluster run.
fn build_report(
    run: &ClusterRun,
    inner: &RunInner,
    cluster: &ClusterInfo,
    members: &HashMap<String, MemberView>,
) -> Report {
    let mut m = merge(run, inner);
    // The server side, merged over the server nodes.
    let mut server: Option<ServerReport> = None;
    let mut processing = histogram(2);
    let mut rejected = 0u64;
    for p in &inner.servers {
        let Some(st) = &p.stats else { continue };
        rejected += st.rejected;
        let _ = st.processing.add_to(&mut processing);
        let s = server.get_or_insert_with(|| ServerReport {
            seen_tracked_up_to: st.report.seen_tracked_up_to,
            ..ServerReport::default()
        });
        let r = &st.report;
        s.arrivals += r.arrivals;
        merge_map(&mut s.by_node, &r.by_node, super::correlate::MAX_LABELS);
        merge_map(&mut s.by_route, &r.by_route, super::correlate::MAX_ROUTES);
        merge_map(&mut s.statuses, &r.statuses, 600);
        let c: &mut ChangesReport = &mut s.changes;
        c.headers_changed += r.changes.headers_changed;
        c.body_changed += r.changes.body_changed;
        c.path_changed += r.changes.path_changed;
        merge_map(
            &mut c.headers_missing,
            &r.changes.headers_missing,
            super::correlate::MAX_LABELS,
        );
        merge_map(
            &mut c.headers_added,
            &r.changes.headers_added,
            super::correlate::MAX_LABELS,
        );
        s.captured += r.captured;
        s.seen_overflow += r.seen_overflow;
    }
    if let Some(s) = &mut server {
        s.processing = Percentiles::from_histogram(&processing);
    }
    // Lost: a transport failure no server node saw.
    let lost_check = if inner.servers.is_empty() {
        "no server nodes in the run: lost is not counted".to_string()
    } else {
        let collect = inner.collect.as_ref();
        let answered: Vec<&Collected> = inner
            .servers
            .iter()
            .filter_map(|p| p.collected.as_ref())
            .collect();
        let all = answered.len() == inner.servers.len();
        let (mut lost, mut after, mut untracked) = (0u64, 0u64, collect.map_or(0, |c| c.untracked));
        if let Some(c) = collect {
            for (node, seqs) in &c.failed {
                let mut seen: HashSet<u64> = HashSet::new();
                let mut unknown: HashSet<u64> = HashSet::new();
                for a in &answered {
                    if let Some(enc) = a.seen.get(node) {
                        seen.extend(decode_numbers(enc, MAX_COLLECT_TOTAL).unwrap_or_default());
                    }
                    if let Some(enc) = a.untracked.get(node) {
                        unknown.extend(decode_numbers(enc, MAX_COLLECT_TOTAL).unwrap_or_default());
                    }
                }
                for s in seqs {
                    if seen.contains(s) {
                        after += 1;
                    } else if unknown.contains(s) || !all {
                        untracked += 1;
                    } else {
                        lost += 1;
                    }
                }
            }
        }
        m.counts.lost = lost;
        m.counts.transport_after_upstream = after;
        m.counts.transport_untracked = untracked;
        if all {
            "complete".to_string()
        } else {
            format!(
                "partial: {} of {} server node(s) answered; failures they could not rule out are untracked",
                answered.len(),
                inner.servers.len()
            )
        }
    };
    let latency = Percentiles::from_histogram(&m.corrected);
    let overhead = Percentiles::from_histogram(&m.overhead);
    let planned = run.scenario.duration().as_secs_f64();
    let elapsed = m.duration_secs.max(1e-9);
    let tail = latency.p50_ms / 1000.0;
    let achieved_rate = m.counts.completed as f64 / (elapsed - tail).max(1e-9);
    let requested_rate = if planned > 0.0 {
        m.counts.requested as f64 / planned.min(elapsed)
    } else {
        0.0
    };
    let thresholds = report::evaluate(
        &run.scenario.thresholds,
        &m.counts,
        &latency,
        &overhead,
        server.as_ref().map(|s| s.arrivals),
        &m.saturation,
    );
    // Node errors: a refused or failed assignment, or a node that went
    // silent before its final stats.
    let mut errors = Vec::new();
    let mut nodes = Vec::new();
    // Requests no stats cover: a client part without final stats from its
    // node (it went silent, or it is the old leader's own part, resumed from
    // a checkpoint) may have sent more since its last report, until it
    // surely stopped (its share re-split once its lease expired). The bound:
    // its rate over that time plus one second (a node's counters are rolled
    // up once per second, so its last report trails its sends by up to one
    // bucket), plus what it had in flight.
    let all_open = run
        .scenario
        .layers
        .iter()
        .all(|l| l.model == super::scenario::Model::Open);
    let peak = run.scenario.peak_rate();
    let report_at = Instant::now();
    let mut unreported_total = 0u64;
    for p in &inner.clients {
        let view = members.get(&p.node_id);
        let s = p.stats.as_ref();
        let unreported = (!p.done())
            .then(|| {
                let since = p.stats_at.or(inner.started)?;
                let until = p.gone_at.unwrap_or(report_at).max(since);
                let secs = until.duration_since(since).as_secs_f64() + 1.0;
                let rate = if all_open {
                    peak * p.share.fraction()
                } else {
                    view.map_or(0, |v| v.capacity.max_rate) as f64
                };
                Some((rate * secs).ceil() as u64 + s.map_or(0, |s| s.in_flight))
            })
            .flatten();
        unreported_total += unreported.unwrap_or(0);
        let state = match s {
            Some(s) if p.done() => s.state.clone(),
            Some(_) | None => "lost".to_string(),
        };
        let error = match s {
            Some(s) if s.error.is_some() => s.error.clone(),
            Some(_) if p.done() => None,
            _ => Some(match unreported {
                Some(n) => format!(
                    "no final stats from the node (it went silent or missed the deadline): up to {n} requests unreported"
                ),
                None => "no final stats from the node (it went silent or missed the deadline)".into(),
            }),
        };
        if let Some(e) = &error {
            errors.push(format!("{}: {e}", p.node_id));
        }
        let arrivals = inner
            .servers
            .iter()
            .find(|sp| sp.node_id == p.node_id)
            .and_then(|sp| sp.stats.as_ref().map(|st| st.report.arrivals));
        nodes.push(NodeReport {
            node_id: p.node_id.clone(),
            roles: view
                .map(|v| v.roles.iter().map(|r| r.as_str().to_string()).collect())
                .unwrap_or_else(|| vec!["client".into()]),
            share: p.share.fraction(),
            virtual_clients: Some([p.vu_from, p.vu_to]),
            state,
            sent: s.map_or(0, |s| s.counts.sent),
            completed: s.map_or(0, |s| s.counts.completed),
            errors: s.map_or(0, |s| s.counts.errors()),
            saturated: s.is_some_and(|s| s.saturation.saturated),
            arrivals,
            clock_offset_ms: view.and_then(|v| v.clock).map(|c| c.offset_ns as f64 / 1e6),
            clock_error_ms: view.and_then(|v| v.clock).map(|c| c.error_ns as f64 / 1e6),
            start_lag_ms: s.and_then(|s| s.start_lag_ms),
            error,
            unreported,
            note: if p.shed {
                Some("handed its share back when it took over as leader".into())
            } else if p.gone {
                Some("went silent past its lease: its share was re-split over the others".into())
            } else {
                None
            },
        });
    }
    for p in &inner.servers {
        if inner.clients.iter().any(|c| c.node_id == p.node_id) {
            continue;
        }
        let view = members.get(&p.node_id);
        nodes.push(NodeReport {
            node_id: p.node_id.clone(),
            roles: view
                .map(|v| v.roles.iter().map(|r| r.as_str().to_string()).collect())
                .unwrap_or_else(|| vec!["server".into()]),
            state: if p.collected.is_some() {
                "server".into()
            } else {
                "server (no lost answer)".into()
            },
            arrivals: p.stats.as_ref().map(|s| s.report.arrivals),
            clock_offset_ms: view.and_then(|v| v.clock).map(|c| c.offset_ns as f64 / 1e6),
            clock_error_ms: view.and_then(|v| v.clock).map(|c| c.error_ns as f64 / 1e6),
            ..NodeReport::default()
        });
    }
    let state = if !errors.is_empty() {
        "failed"
    } else if inner.stop_reason.is_some() || inner.clients.is_empty() {
        "stopped"
    } else {
        "finished"
    };
    let exit_code = Report::exit_code_for(state, &thresholds);
    let layer_names: Vec<String> = run.scenario.layers.iter().map(|l| l.name.clone()).collect();
    let breakdown = |names: Vec<String>, parts: &[MergedPart]| -> Vec<Breakdown> {
        names
            .into_iter()
            .zip(parts)
            .map(|(name, p)| Breakdown {
                name,
                counts: p.counts.clone(),
                latency: Percentiles::from_histogram(&p.corrected),
            })
            .collect()
    };
    let layers = breakdown(layer_names, &m.layers);
    let targets_breakdown = breakdown(
        run.scenario.targets.iter().map(|t| t.url.clone()).collect(),
        &m.targets,
    );
    let mut samples = std::mem::take(&mut m.samples);
    samples.sort_by(|a, b| {
        (b.kind == "error")
            .cmp(&(a.kind == "error"))
            .then_with(|| b.latency_ms.total_cmp(&a.latency_ms))
    });
    samples.truncate(MAX_SAMPLES);
    let clock_error_ms = inner
        .clients
        .iter()
        .map(|p| &p.node_id)
        .chain(inner.servers.iter().map(|p| &p.node_id))
        .filter_map(|n| members.get(n).and_then(|v| v.clock))
        .map(|c| c.error_ns as f64 / 1e6)
        .fold(0.0, f64::max);
    // The cluster's caps: the client nodes' declared capacities added up.
    let declared: Vec<Capacity> = inner
        .clients
        .iter()
        .filter_map(|p| members.get(&p.node_id).map(|v| v.capacity))
        .collect();
    let caps = Caps {
        max_rate: declared.iter().map(|c| c.max_rate).sum(),
        max_inflight: declared.iter().map(|c| c.max_inflight).sum(),
        max_duration_secs: declared
            .iter()
            .map(|c| c.max_duration_secs)
            .min()
            .unwrap_or(0),
    };
    Report {
        run_id: run.id.clone(),
        scenario_name: run.scenario.name.clone(),
        scenario_hash: run.hash.clone(),
        node_id: cluster.leader.clone(),
        rustybin_version: env!("CARGO_PKG_VERSION").into(),
        started_at: if inner.start_at_ns > 0 {
            rfc3339_of_ns(inner.start_at_ns)
        } else {
            run.created_at.to_rfc3339()
        },
        finished_at: chrono::Utc::now().to_rfc3339(),
        duration_secs: m.duration_secs,
        planned_secs: planned,
        state: state.into(),
        stopped_by: inner.stop_reason.clone(),
        error: (!errors.is_empty()).then(|| errors.join("; ")),
        caps,
        targets: if m.target_reports.is_empty() {
            run.scenario
                .targets
                .iter()
                .map(|t| TargetReport {
                    url: t.url.clone(),
                    weight: t.weight,
                    http: t.http.as_str().into(),
                    pinned: Vec::new(),
                    verified: false,
                })
                .collect()
        } else {
            std::mem::take(&mut m.target_reports)
        },
        totals: m.counts.clone(),
        latency,
        service_latency: Percentiles::from_histogram(&m.service),
        upstream_latency: Percentiles::from_histogram(&m.upstream),
        overhead,
        request_leg: Percentiles::from_histogram(&m.request_leg),
        response_leg: Percentiles::from_histogram(&m.response_leg),
        server,
        rejected_markers: rejected,
        achieved_rate,
        requested_rate,
        layers,
        targets_breakdown,
        timeline: std::mem::take(&mut m.timeline),
        saturation: m.saturation.clone(),
        passed: exit_code == 0,
        exit_code,
        thresholds,
        samples,
        scenario: report::redact_scenario(
            &serde_json::to_value(&run.scenario).unwrap_or(Value::Null),
        ),
        cluster: Some(ClusterReport {
            cluster_id: cluster.id.clone(),
            leader: cluster.leader.clone(),
            term: cluster.term,
            start_at: rfc3339_of_ns(inner.start_at_ns),
            nodes,
            clock_error_ms,
            lost_check,
            unreported: unreported_total,
            history: cluster.history.clone(),
        }),
    }
}

struct ClusterInner {
    members: Vec<Member>,
    runs: Vec<Arc<ClusterRun>>,
    events: Vec<Value>,
    last_activity: Instant,
    /// Leader changes, oldest first.
    history: Vec<LeaderChange>,
    /// Who leads next (eligible members, this leader excluded).
    succession: Vec<Successor>,
    last_checkpoint: Instant,
}

/// A cluster this node leads, for one term.
pub struct Cluster {
    pub id: String,
    /// This node: the leader of the term.
    pub leader: String,
    pub term: u64,
    /// Where the other nodes reach this leader.
    pub control_url: Option<String>,
    /// When the term began.
    pub since: chrono::DateTime<chrono::Utc>,
    join_hash: String,
    secret: [u8; 32],
    pub lease: Duration,
    pub election_timeout: Duration,
    pub min_nodes: MinNodes,
    pub created_at: chrono::DateTime<chrono::Utc>,
    verification_token: String,
    report_dir: Option<PathBuf>,
    inner: Mutex<ClusterInner>,
    version: watch::Sender<u64>,
    /// A newer term exists: this node no longer leads (it steps down).
    stale: std::sync::atomic::AtomicBool,
    /// Taken over from a snapshot: the leader before, and when.
    previous_leader: Option<String>,
    promoted_at: Option<Instant>,
    /// The new leader still has to hand its own client share back (once
    /// another client node reports to it).
    shed_pending: std::sync::atomic::AtomicBool,
    /// The node is shutting down: heartbeats answer at once (no long poll
    /// holds the graceful shutdown up).
    closing: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for Cluster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the secret or the join token's hash.
        f.debug_struct("Cluster")
            .field("id", &self.id)
            .field("leader", &self.leader)
            .field("lease", &self.lease)
            .finish_non_exhaustive()
    }
}

/// Decrements a member's long-poll counter when the heartbeat answers.
struct PollGuard<'a> {
    cluster: &'a Cluster,
    node: String,
}

impl Drop for PollGuard<'_> {
    fn drop(&mut self) {
        let mut inner = lock(&self.cluster.inner);
        if let Some(m) = inner.members.iter_mut().find(|m| m.node_id == self.node) {
            m.polling = m.polling.saturating_sub(1);
            m.last_seen = Instant::now();
        }
    }
}

/// How many nodes of each role a run waits for, and how many are there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Quorum {
    pub clients: usize,
    pub servers: usize,
    pub need_clients: usize,
    pub need_servers: usize,
    pub met: bool,
}

impl Cluster {
    fn info(&self) -> ClusterInfo {
        ClusterInfo {
            id: self.id.clone(),
            leader: self.leader.clone(),
            term: self.term,
            history: lock(&self.inner).history.clone(),
            report_dir: self.report_dir.clone(),
            lease: self.lease,
            heartbeat_every: self.heartbeat_every(),
        }
    }

    /// This leader, as every answer carries it.
    pub fn leader_ref(&self) -> Leader {
        Leader {
            node_id: self.leader.clone(),
            term: self.term,
            control_url: self.control_url.clone(),
            since: Some(self.since.to_rfc3339()),
        }
    }

    /// A newer term was seen: this node must step down.
    pub fn is_stale(&self) -> bool {
        self.stale.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn mark_stale(&self) {
        if !self.stale.swap(true, std::sync::atomic::Ordering::Relaxed) {
            tracing::warn!(
                target: "rustybin::harness",
                cluster = %self.id,
                term = self.term,
                "harness: a newer term exists, this node steps down as leader"
            );
            self.bump();
        }
    }

    /// The nodes that may lead (this leader included): elections need a
    /// majority of them.
    pub fn eligible(&self) -> Vec<String> {
        let inner = lock(&self.inner);
        Self::eligible_of(&inner, &self.leader)
    }

    fn eligible_of(inner: &ClusterInner, leader: &str) -> Vec<String> {
        let mut ids = vec![leader.to_string()];
        for m in &inner.members {
            if m.node_id != leader && m.eligible() {
                ids.push(m.node_id.clone());
            }
        }
        ids
    }

    /// Whether this leader hears a majority of the eligible nodes within
    /// the election timeout. With fewer than three eligible nodes no other
    /// node can be elected, so it always has.
    pub fn has_quorum(&self) -> bool {
        let inner = lock(&self.inner);
        let eligible = Self::eligible_of(&inner, &self.leader);
        if eligible.len() < 3 {
            return true;
        }
        let heard = 1 + inner
            .members
            .iter()
            .filter(|m| {
                m.node_id != self.leader && m.eligible() && m.heard_within(self.election_timeout)
            })
            .count();
        heard * 2 > eligible.len()
    }

    /// `failover`, as `GET .../leader` and the cluster view report it.
    fn failover_text(eligible: usize) -> String {
        if eligible >= 3 {
            "available".into()
        } else {
            format!(
                "unavailable: {eligible} eligible node(s), failover needs three (a majority of two is both)"
            )
        }
    }

    pub fn version(&self) -> u64 {
        *self.version.borrow()
    }

    fn bump(&self) {
        self.version.send_modify(|v| *v += 1);
    }

    /// How often a node heartbeats while it sends load: often enough to
    /// renew the lease and to notice a lost leader well before it expires.
    pub fn heartbeat_every(&self) -> Duration {
        (self.lease / 4)
            .min(self.election_timeout / 3)
            .clamp(Duration::from_millis(50), Duration::from_secs(1))
    }

    fn credential(&self, node_id: &str, incarnation: &str) -> String {
        node_credential(&self.secret, &self.id, node_id, incarnation)
    }

    /// This leader's own node credential (it is a member of its cluster),
    /// to call other nodes as a member (probing for a newer leader).
    pub fn self_credential(&self) -> Option<String> {
        let inner = lock(&self.inner);
        inner
            .members
            .iter()
            .find(|m| m.node_id == self.leader)
            .map(|m| self.credential(&m.node_id, &m.incarnation))
    }

    /// The eligible members' control URLs (this leader excluded).
    pub fn peer_urls(&self) -> Vec<(String, String)> {
        let inner = lock(&self.inner);
        inner
            .members
            .iter()
            .filter(|m| m.node_id != self.leader && m.eligible())
            .filter_map(|m| m.control_url.clone().map(|u| (m.node_id.clone(), u)))
            .collect()
    }

    /// `POST .../election/vote` on the leader: it is alive, so it refuses
    /// and names itself.
    pub fn answer_vote(
        &self,
        credential: &str,
        req: &super::wire::VoteRequest,
    ) -> Result<super::wire::VoteResponse, NodeError> {
        self.authenticate(&req.candidate, credential)?;
        Ok(super::wire::VoteResponse {
            granted: false,
            term: self.term,
            leader: (!self.is_stale()).then(|| self.leader_ref()),
            reason: "the leader is alive".into(),
        })
    }

    fn token_ok(&self, token: &str) -> bool {
        crate::control_auth::constant_time_eq(&sha256_hex(token), &self.join_hash)
    }

    fn event(inner: &mut ClusterInner, kind: &str, node: &str) {
        if inner.events.len() >= MAX_EVENTS {
            inner.events.remove(0);
        }
        inner.events.push(json!({
            "at": chrono::Utc::now().to_rfc3339(),
            "event": kind,
            "node_id": node,
        }));
    }

    fn members_view(&self) -> HashMap<String, MemberView> {
        let inner = lock(&self.inner);
        inner
            .members
            .iter()
            .map(|m| {
                (
                    m.node_id.clone(),
                    MemberView {
                        roles: m.roles.clone(),
                        alive: m.alive(self.lease),
                        clock: m.clock,
                        capacity: m.capacity,
                        silent: if m.polling > 0 {
                            Duration::ZERO
                        } else {
                            m.last_seen.elapsed()
                        },
                        left: m.left,
                    },
                )
            })
            .collect()
    }

    pub fn runs(&self) -> Vec<Arc<ClusterRun>> {
        let mut inner = lock(&self.inner);
        inner
            .runs
            .retain(|r| r.state() != RunState::Finished || r.created.elapsed() < RUN_TTL);
        inner.runs.clone()
    }

    pub fn run(&self, id: &str) -> Option<Arc<ClusterRun>> {
        self.runs().into_iter().find(|r| r.id == id)
    }

    pub fn active_run(&self) -> Option<Arc<ClusterRun>> {
        self.runs().into_iter().find(|r| r.is_active())
    }

    /// Advance every run (the ticker, heartbeats and reads call it), keep
    /// the succession current and, while a run is active, checkpoint it
    /// into the snapshot every [`CHECKPOINT_EVERY`].
    pub fn tick(&self) {
        let runs: Vec<Arc<ClusterRun>> = {
            let inner = lock(&self.inner);
            inner
                .runs
                .iter()
                .filter(|r| r.is_active())
                .cloned()
                .collect()
        };
        let checkpoint = {
            let mut inner = lock(&self.inner);
            let due = inner.last_checkpoint.elapsed() >= CHECKPOINT_EVERY.min(self.lease);
            if due {
                inner.last_checkpoint = Instant::now();
            }
            due
        };
        let mut changed = false;
        if !runs.is_empty() {
            let members = self.members_view();
            let info = self.info();
            for run in runs.iter() {
                changed |= run.advance(&info, &members);
            }
            // A new snapshot version: eligible nodes fetch the checkpoint.
            changed |= checkpoint;
        }
        if checkpoint || changed {
            changed |= self.recompute_succession();
        }
        if changed {
            self.bump();
        }
    }

    /// `POST .../clusters/{id}/nodes`: a node joins with the join token.
    pub fn join(&self, token: &str, req: JoinRequest, t2: u64) -> Result<JoinResponse, NodeError> {
        if !self.token_ok(token) {
            return Err(NodeError::new(401, "invalid join token"));
        }
        if req.harness_api != HARNESS_API {
            return Err(NodeError::new(
                400,
                format!(
                    "harness_api {} is not supported by this leader (it speaks {HARNESS_API})",
                    req.harness_api
                ),
            ));
        }
        if !safe_id(&req.node_id) {
            return Err(NodeError::new(
                400,
                "node_id: letters, digits, . _ - only, at most 64 characters",
            ));
        }
        // Valid control URL, as advertise below.
        let url_ok = |u: &str| {
            u.len() <= 2048
                && reqwest::Url::parse(u).is_ok_and(|u| {
                    (u.scheme() == "http" || u.scheme() == "https")
                        && u.host_str().is_some()
                        && u.username().is_empty()
                        && u.password().is_none()
                })
        };
        if let Some(cu) = &req.control_url {
            if !url_ok(cu) {
                return Err(NodeError::new(
                    400,
                    "control_url: an http or https URL without credentials",
                ));
            }
        }
        let eligible = req.control_url.is_some() && req.leader != LeaderPolicy::Never;
        if !req
            .roles
            .iter()
            .any(|r| matches!(r, Role::Client | Role::Server))
            && !eligible
            && req.node_id != self.leader
        {
            return Err(NodeError::new(
                400,
                "a member needs the client or the server role, or a control URL to stand by as leader",
            ));
        }
        if let Some(adv) = &req.advertise {
            let ok = adv.len() <= 2048
                && reqwest::Url::parse(adv).is_ok_and(|u| {
                    (u.scheme() == "http" || u.scheme() == "https")
                        && u.host_str().is_some()
                        && u.username().is_empty()
                        && u.password().is_none()
                });
            if !ok {
                return Err(NodeError::new(
                    400,
                    "advertise: an http or https URL without credentials",
                ));
            }
        }
        let mut roles: Vec<Role> = Vec::new();
        for r in &req.roles {
            if !roles.contains(r) && roles.len() < 3 {
                roles.push(*r);
            }
        }
        let incarnation = super::random_hex(8);
        let credential = self.credential(&req.node_id, &incarnation);
        let member = Member {
            node_id: req.node_id.clone(),
            roles,
            advertise: req.advertise.clone(),
            capacity: Capacity {
                max_rate: req.capacity.max_rate.clamp(1, 10_000_000),
                max_inflight: req.capacity.max_inflight.clamp(1, 10_000_000),
                max_duration_secs: req.capacity.max_duration_secs,
                threads: req.capacity.threads.min(1024),
            },
            version: req.version.chars().take(32).collect(),
            incarnation,
            joined_at: chrono::Utc::now(),
            last_seen: Instant::now(),
            polling: 0,
            clock: None,
            left: false,
            in_flight: 0,
            control_url: req.control_url.clone(),
            policy: req.leader,
            work: WorkReport::default(),
        };
        let is_server = member.has(Role::Server);
        {
            let mut inner = lock(&self.inner);
            inner.last_activity = Instant::now();
            let lease = self.lease;
            if let Some(i) = inner.members.iter().position(|m| m.node_id == req.node_id) {
                inner.members[i] = member;
                Self::event(&mut inner, "rejoined", &req.node_id);
            } else {
                if inner.members.len() >= MAX_NODES {
                    // Make room from a node that left or went silent.
                    let gone = inner
                        .members
                        .iter()
                        .enumerate()
                        .filter(|(_, m)| !m.alive(lease))
                        .min_by_key(|(_, m)| m.last_seen)
                        .map(|(i, _)| i);
                    match gone {
                        Some(i) => {
                            inner.members.remove(i);
                        }
                        None => {
                            return Err(NodeError::new(
                                409,
                                format!("the cluster has {MAX_NODES} nodes, the most it takes"),
                            ))
                        }
                    }
                }
                inner.members.push(member);
                Self::event(&mut inner, "joined", &req.node_id);
            }
        }
        // A server node that joins during a run correlates it too.
        if is_server {
            if let Some(run) = self.active_run() {
                let mut ri = lock(&run.inner);
                if ri.state != RunState::Collecting
                    && !ri.servers.iter().any(|p| p.node_id == req.node_id)
                    && ri.servers.len() < MAX_NODES
                {
                    ri.servers.push(ServerPart {
                        node_id: req.node_id.clone(),
                        stats: None,
                        collected: None,
                    });
                }
            }
        }
        self.recompute_succession();
        self.bump();
        tracing::info!(
            target: "rustybin::harness",
            cluster = %self.id,
            node = %req.node_id,
            roles = ?req.roles,
            advertise = ?req.advertise,
            "harness node joined"
        );
        Ok(JoinResponse {
            cluster_id: self.id.clone(),
            node_id: req.node_id,
            credential,
            lease_ms: self.lease.as_millis() as u64,
            heartbeat_ms: self.heartbeat_every().as_millis() as u64,
            election_timeout_ms: self.election_timeout.as_millis() as u64,
            leader: self.leader_ref(),
            verification_token: self.verification_token.clone(),
            t2,
            t3: now_ns(),
        })
    }

    pub(super) fn authenticate(&self, node_id: &str, credential: &str) -> Result<(), NodeError> {
        let inner = lock(&self.inner);
        let Some(m) = inner.members.iter().find(|m| m.node_id == node_id) else {
            return Err(NodeError::new(
                401,
                "unknown node: join again with the join token",
            ));
        };
        if m.left {
            return Err(NodeError::new(401, "the node left: join again"));
        }
        let expected = self.credential(&m.node_id, &m.incarnation);
        if !crate::control_auth::constant_time_eq(&expected, credential) {
            return Err(NodeError::new(401, "invalid node credential"));
        }
        Ok(())
    }

    /// `POST .../nodes/{node}/heartbeat`.
    pub async fn heartbeat(
        &self,
        node_id: &str,
        credential: &str,
        req: HeartbeatRequest,
        t2: u64,
    ) -> Result<HeartbeatResponse, NodeError> {
        self.authenticate(node_id, credential)?;
        if req.term > self.term {
            // The node follows a newer leader: this one is out of date.
            self.mark_stale();
        }
        if self.is_stale() {
            return Err(NodeError::new(
                409,
                "this node no longer leads the cluster: a newer term exists",
            ));
        }
        {
            let mut inner = lock(&self.inner);
            inner.last_activity = Instant::now();
            let leaving = req.leave;
            if let Some(m) = inner.members.iter_mut().find(|m| m.node_id == node_id) {
                m.last_seen = Instant::now();
                if req.clock.is_some() {
                    m.clock = req.clock;
                }
                m.in_flight = req.in_flight;
                m.work = req.work;
                if leaving {
                    m.left = true;
                }
            }
            if leaving {
                Self::event(&mut inner, "left", node_id);
            }
        }
        if req.leave {
            self.bump();
            tracing::info!(target: "rustybin::harness", cluster = %self.id, node = %node_id, "harness node left");
            return Ok(HeartbeatResponse {
                version: self.version(),
                leader: self.leader_ref(),
                lease_ms: self.lease.as_millis() as u64,
                heartbeat_ms: self.heartbeat_every().as_millis() as u64,
                election_timeout_ms: self.election_timeout.as_millis() as u64,
                t2,
                t3: now_ns(),
                left: true,
                ..HeartbeatResponse::default()
            });
        }
        // Without a majority of the eligible nodes this leader may be the
        // minority side of a partition: no answer, so no lease renewal, and
        // the clients that only hear it stop sending.
        if !self.has_quorum() {
            return Err(NodeError::new(
                503,
                "the leader does not hear a majority of the eligible nodes (no lease renewal)",
            ));
        }
        if req.kill {
            let stopped = self.stop_active(&format!("kill_switch on {node_id}"));
            tracing::warn!(
                target: "rustybin::harness",
                cluster = %self.id,
                node = %node_id,
                runs = ?stopped,
                "harness kill switch on a member: the cluster's runs stop"
            );
        }
        for stats in req.runs {
            if let Some(run) = self.run(&stats.run_id) {
                run.accept_client(node_id, stats);
            }
        }
        for stats in req.servers {
            if let Some(run) = self.run(&stats.run_id) {
                run.accept_server(node_id, stats);
            }
        }
        self.tick();
        // A long poll never outlasts a third of the election timeout, so
        // members notice a lost leader in time.
        let wait = Duration::from_millis(req.wait_ms)
            .min(MAX_WAIT)
            .min(self.election_timeout / 3);
        let closing = self.closing.load(std::sync::atomic::Ordering::Relaxed);
        if !wait.is_zero() && !closing && self.version() <= req.known_version {
            {
                let mut inner = lock(&self.inner);
                if let Some(m) = inner.members.iter_mut().find(|m| m.node_id == node_id) {
                    m.polling += 1;
                }
            }
            let _guard = PollGuard {
                cluster: self,
                node: node_id.to_string(),
            };
            let mut rx = self.version.subscribe();
            let known = req.known_version;
            let _ = tokio::time::timeout(wait, rx.wait_for(|v| *v > known)).await;
        }
        if self.is_stale() {
            return Err(NodeError::new(
                409,
                "this node no longer leads the cluster: a newer term exists",
            ));
        }
        Ok(self.response_for(node_id, t2, req.snapshot))
    }

    /// The snapshot version members compare.
    pub fn snapshot_version(&self) -> SnapshotVersion {
        SnapshotVersion {
            term: self.term,
            seq: self.version(),
        }
    }

    fn response_for(&self, node_id: &str, t2: u64, known: SnapshotVersion) -> HeartbeatResponse {
        let version = self.version();
        let mut runs = Vec::new();
        let mut servers = Vec::new();
        for run in self.runs() {
            let inner = lock(&run.inner);
            if inner.state == RunState::Created || inner.state == RunState::Finished {
                continue;
            }
            if let Some(p) = inner.clients.iter().find(|p| p.node_id == node_id) {
                // Until the node itself reports (a new leader starts from a
                // checkpoint): a node that already runs its part ignores it.
                let assignment = ((p.stats.is_none() || p.from_checkpoint) && !p.gone).then(|| {
                    ClientAssignment {
                        cluster_id: self.id.clone(),
                        scenario: run.scenario.clone(),
                        hash: run.hash.clone(),
                        key: run.key.to_hex(),
                        share: p.share,
                        vu_from: p.vu_from,
                        vu_to: p.vu_to,
                        start_at_ns: inner.start_at_ns,
                        verification_token: self.verification_token.clone(),
                    }
                });
                runs.push(RunControl {
                    run_id: run.id.clone(),
                    stop: inner.stop_reason.is_some() || p.gone,
                    assignment,
                    timeline_ack: p.timeline.len(),
                    final_ack: p.complete(),
                    share: (!p.gone).then_some(p.share),
                });
            }
            if let Some(p) = inner.servers.iter().find(|p| p.node_id == node_id) {
                let collect =
                    (inner.state == RunState::Collecting && p.collected.is_none()).then(|| {
                        inner
                            .collect
                            .as_ref()
                            .map(|c| c.encoded.clone())
                            .unwrap_or_default()
                    });
                servers.push(ServerAssignment {
                    run_id: run.id.clone(),
                    key: run.key.to_hex(),
                    profile: run.scenario.servers.clone(),
                    collect,
                });
            }
        }
        let snapshot = (known < self.snapshot_version()).then(|| {
            let full = lock(&self.inner)
                .members
                .iter()
                .any(|m| m.node_id == node_id && m.eligible());
            Box::new(self.snapshot(full))
        });
        HeartbeatResponse {
            version,
            leader: self.leader_ref(),
            lease_ms: self.lease.as_millis() as u64,
            heartbeat_ms: self.heartbeat_every().as_millis() as u64,
            election_timeout_ms: self.election_timeout.as_millis() as u64,
            t2,
            t3: now_ns(),
            snapshot,
            runs,
            servers,
            left: false,
        }
    }

    /// The replicated snapshot: the public part for every node; `full` adds
    /// the members, the runs with the checkpoint of their stats, and the
    /// secrets (eligible nodes only).
    pub fn snapshot(&self, full: bool) -> Snapshot {
        let version = self.snapshot_version();
        let (eligible, succession, history, members, silent) = {
            let inner = lock(&self.inner);
            let grace = self.lease + self.heartbeat_every();
            (
                Self::eligible_of(&inner, &self.leader),
                inner.succession.clone(),
                inner.history.clone(),
                if full {
                    inner.members.iter().map(Member::snap).collect()
                } else {
                    Vec::new()
                },
                inner
                    .members
                    .iter()
                    .filter(|m| m.left || !m.heard_within(grace))
                    .map(|m| m.node_id.clone())
                    .collect::<HashSet<String>>(),
            )
        };
        let state = full.then(|| SnapState {
            created_at: self.created_at.to_rfc3339(),
            min_nodes: self.min_nodes,
            members,
            runs: self
                .runs()
                .iter()
                .filter(|r| r.state() != RunState::Finished)
                .map(|r| r.snap(&self.leader, &silent))
                .collect(),
            secrets: SnapSecrets {
                secret: self.secret.iter().map(|b| format!("{b:02x}")).collect(),
                join_hash: self.join_hash.clone(),
                verification_token: self.verification_token.clone(),
            },
        });
        Snapshot {
            cluster_id: self.id.clone(),
            version,
            leader: self.leader_ref(),
            lease_ms: self.lease.as_millis() as u64,
            election_timeout_ms: self.election_timeout.as_millis() as u64,
            eligible,
            succession,
            history,
            state,
        }
    }

    /// Take over a cluster from its snapshot: this node leads `term` from
    /// now on. Members get a grace period (they heartbeat the new leader
    /// within the election timeout); runs resume from the checkpoint, and
    /// every node reports its cumulative stats again, which fills the gap.
    pub fn from_snapshot(
        snap: &Snapshot,
        me: &LeaderInfo,
        term: u64,
    ) -> Result<Arc<Cluster>, String> {
        let state = snap
            .state
            .as_ref()
            .ok_or("the snapshot holds no cluster state (only eligible nodes get it)")?;
        let secret = decode_secret(&state.secrets.secret)?;
        let lease = Duration::from_millis(snap.lease_ms).clamp(MIN_LEASE, MAX_LEASE);
        let election_timeout =
            Duration::from_millis(snap.election_timeout_ms).clamp(MIN_ELECTION_TIMEOUT, lease);
        let now = Instant::now();
        let parse_time = |t: &str| {
            chrono::DateTime::parse_from_rfc3339(t)
                .map(|d| d.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now())
        };
        let members: Vec<Member> = state
            .members
            .iter()
            .take(MAX_NODES)
            .map(|m| Member {
                node_id: m.node_id.clone(),
                roles: m.roles.clone(),
                advertise: m.advertise.clone(),
                capacity: m.capacity,
                version: m.version.clone(),
                incarnation: m.incarnation.clone(),
                joined_at: parse_time(&m.joined_at),
                last_seen: now,
                polling: 0,
                clock: None,
                left: m.left,
                in_flight: 0,
                control_url: m.control_url.clone(),
                policy: m.leader,
                work: WorkReport::default(),
            })
            .collect();
        let runs = state
            .runs
            .iter()
            .take(MAX_CLUSTER_RUNS)
            .map(|r| ClusterRun::from_snap(r, lease))
            .collect::<Result<Vec<_>, String>>()?;
        let since = chrono::Utc::now();
        let mut history = snap.history.clone();
        history.push(LeaderChange {
            term,
            node_id: me.node_id.clone(),
            at: since.to_rfc3339(),
            reason: "elected".into(),
        });
        if history.len() > MAX_HISTORY {
            history.drain(..history.len() - MAX_HISTORY);
        }
        let (version, _) = watch::channel(1);
        let mut events = Vec::new();
        events.push(json!({ "at": since.to_rfc3339(), "event": "elected", "node_id": me.node_id, "term": term }));
        Ok(Arc::new(Cluster {
            id: snap.cluster_id.clone(),
            leader: me.node_id.clone(),
            term,
            control_url: me.control_url.clone(),
            since,
            join_hash: state.secrets.join_hash.clone(),
            secret,
            lease,
            election_timeout,
            min_nodes: state.min_nodes,
            created_at: parse_time(&state.created_at),
            verification_token: state.secrets.verification_token.clone(),
            report_dir: me.report_dir.clone(),
            inner: Mutex::new(ClusterInner {
                members,
                runs,
                events,
                last_activity: now,
                history,
                succession: Vec::new(),
                last_checkpoint: now,
            }),
            version,
            stale: std::sync::atomic::AtomicBool::new(false),
            previous_leader: Some(snap.leader.node_id.clone()),
            promoted_at: Some(now),
            shed_pending: std::sync::atomic::AtomicBool::new(true),
            closing: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    /// The new leader hands its own client share back (`scale.
    /// leader_sheds_load`, default true) once another client node carries
    /// the run with it: one that has reported to this leader, never the old
    /// leader's part. `stop_own` stops this node's own part first, then the
    /// rate is re-split over the others (they take the share on their next
    /// heartbeat), so the cluster never sends more than planned. The ticker
    /// calls it until nothing is left to hand back. Returns the runs.
    pub fn shed_own_load(&self, stop_own: impl Fn(&str)) -> Vec<String> {
        use std::sync::atomic::Ordering;
        if !self.shed_pending.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let members = self.members_view();
        let reported: HashSet<String> = {
            let inner = lock(&self.inner);
            inner
                .members
                .iter()
                .filter(|m| self.promoted_at.is_some_and(|at| m.last_seen > at))
                .map(|m| m.node_id.clone())
                .collect()
        };
        let mut shed = Vec::new();
        let mut pending = false;
        for run in self.runs() {
            if run.scenario.scale.leader_sheds_load == Some(false) {
                continue;
            }
            let mut inner = lock(&run.inner);
            if !matches!(inner.state, RunState::Starting | RunState::Running)
                || inner.stop_reason.is_some()
            {
                continue;
            }
            if !inner
                .clients
                .iter()
                .any(|p| p.node_id == self.leader && !p.gone && !p.done())
            {
                continue;
            }
            let others = inner.clients.iter().any(|p| {
                p.node_id != self.leader
                    && Some(&p.node_id) != self.previous_leader.as_ref()
                    && !p.gone
                    && !p.done()
                    && reported.contains(&p.node_id)
            });
            if !others {
                pending = true;
                continue;
            }
            stop_own(&run.id);
            if let Some(own) = inner
                .clients
                .iter_mut()
                .find(|p| p.node_id == self.leader && !p.gone && !p.done())
            {
                own.gone = true;
                own.shed = true;
                own.gone_at = Some(Instant::now());
            }
            let note = resplit(&run, &mut inner, &members);
            drop(inner);
            tracing::info!(
                target: "rustybin::harness",
                cluster = %self.id,
                run = %run.id,
                "harness: the new leader hands its client share back{}",
                note.map(|n| format!(" ({n})")).unwrap_or_default()
            );
            shed.push(run.id.clone());
        }
        if !pending {
            self.shed_pending.store(false, Ordering::Relaxed);
        }
        if !shed.is_empty() {
            self.bump();
        }
        shed
    }

    /// Order the live eligible members into the succession: standbys first,
    /// then by work score, then by node id. Called on membership changes and
    /// at every checkpoint. Returns whether the order changed.
    fn recompute_succession(&self) -> bool {
        // The rate each client node is assigned now.
        let mut assigned: HashMap<String, f64> = HashMap::new();
        for run in self.runs() {
            let inner = lock(&run.inner);
            if !matches!(inner.state, RunState::Starting | RunState::Running) {
                continue;
            }
            let peak = run.scenario.peak_rate();
            for p in inner.clients.iter().filter(|p| !p.gone) {
                *assigned.entry(p.node_id.clone()).or_default() += peak * p.share.fraction();
            }
        }
        let mut inner = lock(&self.inner);
        // Only live nodes are in line (a silent one would only delay the
        // others); every eligible node still counts for the majority.
        let mut list: Vec<Successor> = inner
            .members
            .iter()
            .filter(|m| m.node_id != self.leader && m.eligible() && m.alive(self.lease))
            .map(|m| Successor {
                node_id: m.node_id.clone(),
                control_url: m.control_url.clone().unwrap_or_default(),
                prefer: m.policy == LeaderPolicy::Prefer,
                work_score: work_score(
                    assigned.get(&m.node_id).copied().unwrap_or(0.0),
                    m.in_flight,
                    &m.work,
                ),
            })
            .collect();
        list.sort_by(|a, b| {
            b.prefer
                .cmp(&a.prefer)
                .then_with(|| a.work_score.total_cmp(&b.work_score))
                .then_with(|| a.node_id.cmp(&b.node_id))
        });
        let order = |l: &[Successor]| l.iter().map(|s| s.node_id.clone()).collect::<Vec<_>>();
        let changed = order(&list) != order(&inner.succession);
        inner.succession = list;
        changed
    }

    /// `GET .../clusters/{id}/leader` on the leader.
    pub fn leader_view(&self) -> Value {
        let eligible = self.eligible();
        let (succession, history) = {
            let inner = lock(&self.inner);
            (inner.succession.clone(), inner.history.clone())
        };
        json!({
            "cluster_id": self.id,
            "leader": self.leader_ref(),
            "term": self.term,
            "answered_by": self.leader,
            "quorum": self.has_quorum(),
            "eligible": eligible,
            "failover": Self::failover_text(eligible.len()),
            "election_timeout_ms": self.election_timeout.as_millis() as u64,
            "lease_ms": self.lease.as_millis() as u64,
            "succession": succession,
            "history": history,
        })
    }

    /// The quorum of a scenario: the nodes it waits for and the nodes here.
    pub fn quorum(&self, scenario: &Scenario) -> Quorum {
        let inner = lock(&self.inner);
        let alive = |role: Role| {
            inner
                .members
                .iter()
                .filter(|m| m.has(role) && m.alive(self.lease))
                .count()
        };
        let clients = alive(Role::Client);
        let servers = alive(Role::Server);
        let need_clients = (scenario
            .scale
            .client_nodes
            .unwrap_or(1)
            .max(self.min_nodes.client))
        .max(1) as usize;
        let need_servers = scenario
            .scale
            .server_nodes
            .unwrap_or(0)
            .max(self.min_nodes.server) as usize;
        Quorum {
            clients,
            servers,
            need_clients,
            need_servers,
            met: clients >= need_clients && servers >= need_servers,
        }
    }

    /// `POST .../clusters/{id}/runs`: validate and keep (not started).
    pub fn create_run(
        &self,
        mut scenario: Scenario,
        caps: &Caps,
    ) -> Result<Arc<ClusterRun>, RunError> {
        scenario.normalise();
        let expected = {
            let q = self.quorum(&scenario);
            q.need_clients.max(q.clients).max(1)
        };
        let problems = scenario.validate_scaled(caps, 1.0 / expected as f64);
        if !problems.is_empty() {
            return Err(RunError::invalid(problems));
        }
        let id = super::random_hex(6);
        let (done, _) = watch::channel(false);
        let run = Arc::new(ClusterRun {
            hash: scenario.hash(),
            id: id.clone(),
            scenario,
            key: RunKey::random(),
            created_at: chrono::Utc::now(),
            created: Instant::now(),
            inner: Mutex::new(RunInner {
                state: RunState::Created,
                stop_reason: None,
                start_at_ns: 0,
                start_at: None,
                started: None,
                deadline: None,
                clients: Vec::new(),
                servers: Vec::new(),
                collect: None,
                finished: None,
            }),
            report: OnceLock::new(),
            done,
        });
        {
            let mut inner = lock(&self.inner);
            inner
                .runs
                .retain(|r| r.state() != RunState::Finished || r.created.elapsed() < RUN_TTL);
            while inner.runs.len() >= MAX_CLUSTER_RUNS {
                // The oldest finished run goes, else the oldest never started.
                let pos = inner
                    .runs
                    .iter()
                    .position(|r| r.state() == RunState::Finished)
                    .or_else(|| {
                        inner
                            .runs
                            .iter()
                            .position(|r| r.state() == RunState::Created)
                    });
                match pos {
                    Some(i) => {
                        inner.runs.remove(i);
                    }
                    None => {
                        return Err(RunError::new(
                            409,
                            format!("the cluster keeps {MAX_CLUSTER_RUNS} runs and none can go"),
                        ))
                    }
                }
            }
            inner.runs.push(run.clone());
            inner.last_activity = Instant::now();
        }
        tracing::info!(
            target: "rustybin::harness",
            cluster = %self.id,
            run = %id,
            scenario = %run.scenario.name,
            hash = %run.hash,
            "harness cluster run created"
        );
        Ok(run)
    }

    /// `POST .../runs/{run}/start`: assign the shares and the synchronised
    /// start, once the quorum is there.
    pub fn start_run(&self, run_id: &str) -> Result<Arc<ClusterRun>, RunError> {
        let Some(run) = self.run(run_id) else {
            return Err(RunError::new(
                404,
                format!("no run {run_id:?} in cluster {}", self.id),
            ));
        };
        if let Some(active) = self.active_run() {
            return Err(RunError::new(
                409,
                format!(
                    "run {} is {} in this cluster; one run at a time",
                    active.id,
                    active.state().as_str()
                ),
            ));
        }
        if run.state() != RunState::Created {
            return Err(RunError::new(
                409,
                format!("run {run_id} is {}, not created", run.state().as_str()),
            ));
        }
        let q = self.quorum(&run.scenario);
        if !q.met {
            return Err(RunError::new(
                409,
                format!(
                    "waiting for nodes: {} of {} client node(s) and {} of {} server node(s) have joined",
                    q.clients, q.need_clients, q.servers, q.need_servers
                ),
            ));
        }
        // The client and server nodes now, in a stable order.
        let (mut clients, mut servers, max_error) = {
            let inner = lock(&self.inner);
            let alive: Vec<&Member> = inner
                .members
                .iter()
                .filter(|m| m.alive(self.lease))
                .collect();
            let clients: Vec<(String, u64)> = alive
                .iter()
                .filter(|m| m.has(Role::Client))
                .map(|m| (m.node_id.clone(), m.capacity.max_rate))
                .collect();
            let servers: Vec<String> = alive
                .iter()
                .filter(|m| m.has(Role::Server))
                .map(|m| m.node_id.clone())
                .collect();
            let max_error = alive
                .iter()
                .filter_map(|m| m.clock.map(|c| c.error_ns))
                .max()
                .unwrap_or(0);
            (clients, servers, max_error)
        };
        clients.sort();
        servers.sort();
        // The rate split by declared capacity (each capped by the scenario's
        // max_rate_per_node), and each node's share checked against its cap.
        let per_node_cap = run.scenario.scale.max_rate_per_node;
        let caps: Vec<u64> = clients
            .iter()
            .map(|(_, c)| per_node_cap.map_or(*c, |p| (*c).min(p)).max(1))
            .collect();
        let total: u64 = caps.iter().sum();
        let peak = run.scenario.peak_rate();
        let mut problems = Vec::new();
        let mut cuts = Vec::with_capacity(clients.len());
        let mut acc = 0u64;
        for ((node, _), cap) in clients.iter().zip(&caps) {
            let from = acc as f64 / total as f64;
            acc += cap;
            let to = if acc == total {
                1.0
            } else {
                acc as f64 / total as f64
            };
            let share = Share { from, to };
            let rate = peak * share.fraction();
            if rate > *cap as f64 + 1e-9 {
                problems.push(format!(
                    "{node}: its share is {rate:.1} requests per second, over its cap of {cap}"
                ));
            }
            cuts.push(share);
        }
        if !problems.is_empty() {
            return Err(RunError {
                status: 409,
                error: format!(
                    "the cluster's client nodes cannot carry a peak of {peak} requests per second (add client nodes or lower the rate)"
                ),
                problems,
            });
        }
        let lead = (Duration::from_nanos(max_error) * 4 + Duration::from_millis(300))
            .clamp(MIN_START_LEAD, MAX_START_LEAD);
        let start_at = Instant::now() + lead;
        let start_at_ns = now_ns() + lead.as_nanos() as u64;
        let vc = run.scenario.scale.virtual_clients;
        let deadline = start_at
            + run.scenario.duration()
            + run.scenario.timeout()
            + super::engine::DRAIN_GRACE
            + self.lease * 2
            + Duration::from_secs(10);
        {
            let mut inner = lock(&run.inner);
            if inner.state != RunState::Created {
                return Err(RunError::new(409, format!("run {run_id} already started")));
            }
            inner.clients = clients
                .iter()
                .zip(&cuts)
                .map(|((node, _), share)| {
                    let (base, count) = share.virtual_clients(vc);
                    ClientPart::new(node.clone(), *share, base, base + count)
                })
                .collect();
            inner.servers = servers
                .iter()
                .map(|n| ServerPart {
                    node_id: n.clone(),
                    stats: None,
                    collected: None,
                })
                .collect();
            inner.state = RunState::Starting;
            inner.start_at = Some(start_at);
            inner.start_at_ns = start_at_ns;
            inner.deadline = Some(deadline);
        }
        self.recompute_succession();
        self.bump();
        tracing::info!(
            target: "rustybin::harness",
            cluster = %self.id,
            run = %run_id,
            clients = clients.len(),
            servers = servers.len(),
            peak_rate = peak,
            lead_ms = lead.as_millis() as u64,
            "harness cluster run starting"
        );
        Ok(run)
    }

    /// `POST .../runs/{run}/stop`.
    pub fn stop_run(
        &self,
        run_id: &str,
        reason: &str,
    ) -> Result<(Arc<ClusterRun>, bool), RunError> {
        let Some(run) = self.run(run_id) else {
            return Err(RunError::new(
                404,
                format!("no run {run_id:?} in cluster {}", self.id),
            ));
        };
        let changed = run.stop(reason, &self.info());
        if changed {
            self.bump();
            tracing::info!(target: "rustybin::harness", cluster = %self.id, run = %run_id, reason, "harness cluster run stop requested");
        }
        Ok((run, changed))
    }

    /// Stop every run that is not finished. Returns their ids.
    pub fn stop_active(&self, reason: &str) -> Vec<String> {
        let info = self.info();
        let mut stopped = Vec::new();
        for run in self.runs() {
            if matches!(run.state(), RunState::Starting | RunState::Running)
                && run.stop(reason, &info)
            {
                stopped.push(run.id.clone());
            }
        }
        if !stopped.is_empty() {
            self.bump();
        }
        stopped
    }

    /// Live view of a run.
    pub fn run_snapshot(&self, run: &ClusterRun) -> Value {
        self.tick();
        let mut v = run.snapshot(&self.members_view());
        if let Value::Object(m) = &mut v {
            m.insert("cluster_id".into(), json!(self.id));
            m.insert("leader".into(), json!(self.leader_ref()));
            m.insert("links".into(), run_links(&self.id, &run.id));
        }
        v
    }

    /// The server nodes' advertised URLs (alive ones, by node id).
    pub fn upstreams(&self) -> Vec<String> {
        let inner = lock(&self.inner);
        let mut members: Vec<&Member> = inner
            .members
            .iter()
            .filter(|m| m.has(Role::Server) && m.alive(self.lease))
            .collect();
        members.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        members.iter().filter_map(|m| m.advertise.clone()).collect()
    }

    /// `GET .../clusters/{id}`.
    pub fn describe(&self) -> Value {
        self.tick();
        let current = self.active_run().or_else(|| {
            self.runs()
                .into_iter()
                .rev()
                .find(|r| r.state() != RunState::Created)
        });
        let quorum = self.has_quorum();
        let inner = lock(&self.inner);
        let eligible = Self::eligible_of(&inner, &self.leader);
        let members: Vec<Value> = inner
            .members
            .iter()
            .map(|m| {
                let mut v = m.describe(self.lease);
                v["leader"] = json!(m.node_id == self.leader);
                if let Some((i, s)) = inner
                    .succession
                    .iter()
                    .enumerate()
                    .find(|(_, s)| s.node_id == m.node_id)
                {
                    v["succession"] = json!(i + 1);
                    v["work_score"] = json!(s.work_score);
                }
                v
            })
            .collect();
        let history = inner.history.clone();
        let count = |role: Role| {
            inner
                .members
                .iter()
                .filter(|m| m.has(role) && m.alive(self.lease))
                .count()
        };
        let clients = count(Role::Client);
        let servers = count(Role::Server);
        let events = inner.events.clone();
        let runs = inner.runs.len();
        drop(inner);
        json!({
            "cluster_id": self.id,
            "leader": self.leader_ref(),
            "term": self.term,
            "quorum": quorum,
            "eligible": eligible.len(),
            "failover": Self::failover_text(eligible.len()),
            "history": history,
            "created_at": self.created_at.to_rfc3339(),
            "lease_ms": self.lease.as_millis() as u64,
            "heartbeat_ms": self.heartbeat_every().as_millis() as u64,
            "election_timeout_ms": self.election_timeout.as_millis() as u64,
            "min_nodes": self.min_nodes,
            "version": self.version(),
            "alive": { "clients": clients, "servers": servers },
            "members": members,
            "current_run": current.map(|r| r.summary()),
            "runs": runs,
            "events": events,
            "links": {
                "self": format!("{PREFIX}/clusters/{}", self.id),
                "join": format!("{PREFIX}/clusters/{}/nodes", self.id),
                "upstreams": format!("{PREFIX}/clusters/{}/upstreams", self.id),
                "runs": format!("{PREFIX}/clusters/{}/runs", self.id),
                "leader": format!("{PREFIX}/clusters/{}/leader", self.id),
            },
        })
    }

    pub fn summary(&self) -> Value {
        let inner = lock(&self.inner);
        let alive = inner.members.iter().filter(|m| m.alive(self.lease)).count();
        let total = inner.members.len();
        drop(inner);
        json!({
            "cluster_id": self.id,
            "leader": self.leader,
            "term": self.term,
            "members": total,
            "alive": alive,
            "active_run": self.active_run().map(|r| r.id.clone()),
            "links": { "self": format!("{PREFIX}/clusters/{}", self.id) },
        })
    }

    fn idle(&self) -> bool {
        let inner = lock(&self.inner);
        inner.last_activity.elapsed() > CLUSTER_IDLE_TTL
            && !inner.members.iter().any(|m| m.alive(self.lease))
            && !inner.runs.iter().any(|r| r.is_active())
    }
}

pub fn run_links(cluster: &str, run: &str) -> Value {
    let base = format!("{PREFIX}/clusters/{cluster}/runs/{run}");
    json!({
        "self": base,
        "start": format!("{base}/start"),
        "stop": format!("{base}/stop"),
        "report": format!("{base}/report"),
    })
}

/// The clusters this node leads.
#[derive(Default)]
pub struct Coordinator {
    clusters: Mutex<Vec<Arc<Cluster>>>,
}

impl Coordinator {
    /// Create a cluster led by `leader`. Returns it and its join token
    /// (shown once; only its hash is kept).
    pub fn create(
        &self,
        me: &LeaderInfo,
        req: CreateCluster,
    ) -> Result<(Arc<Cluster>, String), RunError> {
        let leader = me.node_id.as_str();
        let id = match req.cluster_id {
            Some(id) if safe_id(&id) => id,
            Some(_) => {
                return Err(RunError::new(
                    400,
                    "cluster_id: letters, digits, . _ - only, at most 64 characters",
                ))
            }
            None => format!("cluster-{}", super::random_hex(4)),
        };
        let lease = match req.lease {
            Some(d) if d.0 >= MIN_LEASE && d.0 <= MAX_LEASE => d.0,
            Some(_) => return Err(RunError::new(400, "lease: 500ms to 60s")),
            None => DEFAULT_LEASE,
        };
        // An election must finish inside the lease, so the clients never
        // stop during a failover.
        let election_timeout = match req.election_timeout {
            Some(d) if d.0 >= MIN_ELECTION_TIMEOUT && d.0 < lease => d.0,
            Some(_) => {
                return Err(RunError::new(
                    400,
                    "election_timeout: 200ms, and below the lease",
                ))
            }
            None => {
                let d = if me.election_timeout.is_zero() {
                    DEFAULT_ELECTION_TIMEOUT
                } else {
                    me.election_timeout
                };
                if d < lease {
                    d.max(MIN_ELECTION_TIMEOUT)
                } else {
                    (lease * 3 / 5).max(MIN_ELECTION_TIMEOUT)
                }
            }
        };
        let min_nodes = req.min_nodes.unwrap_or_default();
        if min_nodes.client > MAX_CLUSTER_NODES || min_nodes.server > MAX_CLUSTER_NODES {
            return Err(RunError::new(
                400,
                format!("min_nodes: at most {MAX_CLUSTER_NODES} per role"),
            ));
        }
        let token = match req.join_token {
            Some(t)
                if t.len() >= MIN_TOKEN_LEN
                    && t.len() <= 256
                    && !t.contains(char::is_whitespace) =>
            {
                t
            }
            Some(_) => {
                return Err(RunError::new(
                    400,
                    format!("join_token: {MIN_TOKEN_LEN} to 256 characters, no spaces"),
                ))
            }
            None => super::random_hex(24),
        };
        let mut secret = [0u8; 32];
        for b in &mut secret {
            *b = rand::random();
        }
        let (version, _) = watch::channel(1);
        let now = chrono::Utc::now();
        let cluster = Arc::new(Cluster {
            id: id.clone(),
            leader: leader.to_string(),
            term: 1,
            control_url: me.control_url.clone(),
            since: now,
            join_hash: sha256_hex(&token),
            secret,
            lease,
            election_timeout,
            min_nodes,
            created_at: now,
            verification_token: super::random_hex(16),
            report_dir: me.report_dir.clone(),
            inner: Mutex::new(ClusterInner {
                members: Vec::new(),
                runs: Vec::new(),
                events: Vec::new(),
                last_activity: Instant::now(),
                history: vec![LeaderChange {
                    term: 1,
                    node_id: leader.to_string(),
                    at: now.to_rfc3339(),
                    reason: "created".into(),
                }],
                succession: Vec::new(),
                last_checkpoint: Instant::now(),
            }),
            version,
            stale: std::sync::atomic::AtomicBool::new(false),
            previous_leader: None,
            promoted_at: None,
            shed_pending: std::sync::atomic::AtomicBool::new(false),
            closing: std::sync::atomic::AtomicBool::new(false),
        });
        {
            let mut clusters = lock(&self.clusters);
            clusters.retain(|c| !c.idle());
            if clusters.iter().any(|c| c.id == id) {
                return Err(RunError::new(409, format!("cluster {id} exists")));
            }
            if clusters.len() >= MAX_CLUSTERS {
                return Err(RunError::new(
                    409,
                    format!("this node leads {MAX_CLUSTERS} clusters, the most it takes (delete one first)"),
                ));
            }
            clusters.push(cluster.clone());
        }
        // The caller runs its ticker (`election::lead`): starts,
        // completions, timeouts, checkpoints and the quorum check happen
        // even when no node calls.
        tracing::info!(
            target: "rustybin::harness",
            cluster = %id,
            leader,
            lease_ms = lease.as_millis() as u64,
            election_timeout_ms = election_timeout.as_millis() as u64,
            "harness cluster created"
        );
        Ok((cluster, token))
    }

    /// Lead a cluster taken over from its snapshot (an election won).
    pub fn insert(&self, cluster: Arc<Cluster>) -> Result<(), RunError> {
        let mut clusters = lock(&self.clusters);
        clusters.retain(|c| !c.idle() && c.id != cluster.id);
        if clusters.len() >= MAX_CLUSTERS {
            return Err(RunError::new(
                409,
                format!("this node leads {MAX_CLUSTERS} clusters, the most it takes"),
            ));
        }
        clusters.push(cluster);
        Ok(())
    }

    /// Stop leading a cluster (a newer term exists). Whoever waits for its
    /// runs stops waiting; the new leader carries them on.
    pub fn step_down(&self, id: &str, term: u64) -> Option<Arc<Cluster>> {
        let mut clusters = lock(&self.clusters);
        let i = clusters.iter().position(|c| c.id == id && c.term == term)?;
        let c = clusters.remove(i);
        drop(clusters);
        c.mark_stale();
        for run in c.runs() {
            run.hand_over();
        }
        Some(c)
    }

    pub fn get(&self, id: &str) -> Option<Arc<Cluster>> {
        lock(&self.clusters).iter().find(|c| c.id == id).cloned()
    }

    pub fn list(&self) -> Vec<Arc<Cluster>> {
        let mut clusters = lock(&self.clusters);
        clusters.retain(|c| !c.idle());
        clusters.clone()
    }

    pub fn is_empty(&self) -> bool {
        lock(&self.clusters).is_empty()
    }

    /// Delete a cluster with no active run.
    pub fn remove(&self, id: &str) -> Result<(), RunError> {
        let mut clusters = lock(&self.clusters);
        let Some(i) = clusters.iter().position(|c| c.id == id) else {
            return Err(RunError::new(
                404,
                format!("no cluster {id:?} on this node"),
            ));
        };
        if let Some(run) = clusters[i].active_run() {
            return Err(RunError::new(
                409,
                format!("run {} is active in the cluster: stop it first", run.id),
            ));
        }
        clusters.remove(i);
        tracing::info!(target: "rustybin::harness", cluster = %id, "harness cluster deleted");
        Ok(())
    }

    /// A cluster run by id, in whichever cluster has it.
    pub fn find_run(&self, run_id: &str) -> Option<(Arc<Cluster>, Arc<ClusterRun>)> {
        self.list()
            .into_iter()
            .find_map(|c| c.run(run_id).map(|r| (c.clone(), r)))
    }

    /// The node is shutting down: release every held heartbeat now and
    /// answer the next ones at once.
    pub fn close(&self) {
        for c in self.list() {
            c.closing.store(true, std::sync::atomic::Ordering::Relaxed);
            c.bump();
        }
    }

    /// The kill switch on the leader: every cluster's runs stop.
    pub fn stop_all(&self, reason: &str) -> Vec<String> {
        self.list()
            .iter()
            .flat_map(|c| c.stop_active(reason))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me(id: &str) -> LeaderInfo {
        LeaderInfo {
            node_id: id.into(),
            ..LeaderInfo::default()
        }
    }

    fn join_req(node: &str, roles: &[Role], rate: u64) -> JoinRequest {
        JoinRequest {
            node_id: node.into(),
            roles: roles.to_vec(),
            advertise: Some(format!("http://{node}.example:8080")),
            capacity: Capacity {
                max_rate: rate,
                max_inflight: 100,
                max_duration_secs: 3600,
                threads: 1,
            },
            version: "test".into(),
            harness_api: HARNESS_API,
            t1: now_ns(),
            control_url: None,
            leader: LeaderPolicy::Auto,
        }
    }

    fn scenario(rate: f64) -> Scenario {
        let mut s = Scenario::parse(&format!(
            "name: c\ntarget: http://127.0.0.1:1\nsource: {{ request: {{ path: /status/200 }} }}\npattern: {{ type: constant, rate: {rate}, for: 1s }}\nscale: {{ virtual_clients: 10, client_nodes: 2, server_nodes: 1 }}\n"
        ))
        .unwrap();
        s.normalise();
        s
    }

    #[tokio::test]
    async fn join_tokens_credentials_and_bounds() {
        let co = Coordinator::default();
        let (c, token) = co
            .create(
                &me("leader"),
                CreateCluster {
                    cluster_id: Some("perf-1".into()),
                    ..CreateCluster::default()
                },
            )
            .unwrap();
        assert!(token.len() >= MIN_TOKEN_LEN);
        assert!(co
            .create(
                &me("leader"),
                CreateCluster {
                    cluster_id: Some("perf-1".into()),
                    ..CreateCluster::default()
                },
            )
            .is_err());
        assert_eq!(
            c.join("wrong", join_req("a", &[Role::Client], 10), 0)
                .unwrap_err()
                .status,
            401
        );
        let mut bad = join_req("a", &[Role::Client], 10);
        bad.harness_api = 99;
        assert_eq!(c.join(&token, bad, 0).unwrap_err().status, 400);
        assert_eq!(
            c.join(&token, join_req("bad id!", &[Role::Client], 10), 0)
                .unwrap_err()
                .status,
            400
        );
        assert_eq!(
            c.join(&token, join_req("coord", &[Role::Coordinator], 10), 0)
                .unwrap_err()
                .status,
            400
        );
        let a = c
            .join(&token, join_req("a", &[Role::Client], 10), 0)
            .unwrap();
        assert_eq!(a.leader.node_id, "leader");
        assert_eq!(a.lease_ms, 5000);
        assert!(format!("{a:?}").contains("[redacted]"));
        // A wrong credential or an unknown node is refused.
        let hb = HeartbeatRequest {
            t1: now_ns(),
            ..HeartbeatRequest::default()
        };
        assert_eq!(
            c.heartbeat("a", "nope", hb.clone(), 0)
                .await
                .unwrap_err()
                .status,
            401
        );
        assert_eq!(
            c.heartbeat("zz", &a.credential, hb.clone(), 0)
                .await
                .unwrap_err()
                .status,
            401
        );
        let r = c
            .heartbeat("a", &a.credential, hb.clone(), 0)
            .await
            .unwrap();
        assert!(r.version >= 1);
        // A rejoin gets a new credential; the old one stops working.
        let a2 = c
            .join(&token, join_req("a", &[Role::Client], 10), 0)
            .unwrap();
        assert_ne!(a.credential, a2.credential);
        assert_eq!(
            c.heartbeat("a", &a.credential, hb.clone(), 0)
                .await
                .unwrap_err()
                .status,
            401
        );
        // Nodes are bounded.
        for i in 0..MAX_NODES - 1 {
            c.join(&token, join_req(&format!("n{i}"), &[Role::Server], 10), 0)
                .unwrap();
        }
        assert_eq!(
            c.join(&token, join_req("one-more", &[Role::Server], 10), 0)
                .unwrap_err()
                .status,
            409
        );
        // Leaving frees nothing until the node is gone, but a left node
        // makes room.
        let r = c
            .heartbeat(
                "a",
                &a2.credential,
                HeartbeatRequest {
                    leave: true,
                    ..hb.clone()
                },
                0,
            )
            .await
            .unwrap();
        assert!(r.left);
        assert!(c
            .join(&token, join_req("one-more", &[Role::Server], 10), 0)
            .is_ok());
        // Clusters are bounded.
        for i in 0..MAX_CLUSTERS - 1 {
            co.create(
                &me("leader"),
                CreateCluster {
                    cluster_id: Some(format!("c{i}")),
                    ..CreateCluster::default()
                },
            )
            .unwrap();
        }
        assert_eq!(
            co.create(&me("leader"), CreateCluster::default())
                .unwrap_err()
                .status,
            409
        );
        assert!(co.remove("c0").is_ok());
        assert!(co.create(&me("leader"), CreateCluster::default()).is_ok());
        // Options are checked.
        assert_eq!(
            co.create(
                &me("l"),
                CreateCluster {
                    lease: Some(Dur::secs(600)),
                    ..CreateCluster::default()
                },
            )
            .unwrap_err()
            .status,
            400
        );
        assert_eq!(
            co.create(
                &me("l"),
                CreateCluster {
                    join_token: Some("short".into()),
                    ..CreateCluster::default()
                },
            )
            .unwrap_err()
            .status,
            400
        );
    }

    #[tokio::test]
    async fn quorum_split_and_assignments() {
        let co = Coordinator::default();
        let (c, token) = co.create(&me("leader"), CreateCluster::default()).unwrap();
        let run = c.create_run(scenario(100.0), &Caps::default()).unwrap();
        assert_eq!(run.state(), RunState::Created);
        // Two client nodes and one server are needed.
        let a = c
            .join(&token, join_req("a", &[Role::Client], 1000), 0)
            .unwrap();
        let err = c.start_run(&run.id).unwrap_err();
        assert_eq!(err.status, 409);
        assert!(err.error.contains("1 of 2 client"), "{}", err.error);
        let b = c
            .join(&token, join_req("b", &[Role::Client], 3000), 0)
            .unwrap();
        let s = c
            .join(&token, join_req("s", &[Role::Server], 1), 0)
            .unwrap();
        assert!(c.quorum(&run.scenario).met);
        assert_eq!(c.upstreams(), vec!["http://s.example:8080".to_string()]);
        c.start_run(&run.id).unwrap();
        assert_eq!(run.state(), RunState::Starting);
        assert!(c.start_run(&run.id).is_err(), "one start");
        let hb = HeartbeatRequest {
            t1: now_ns(),
            ..HeartbeatRequest::default()
        };
        let ra = c
            .heartbeat("a", &a.credential, hb.clone(), 0)
            .await
            .unwrap();
        let rb = c
            .heartbeat("b", &b.credential, hb.clone(), 0)
            .await
            .unwrap();
        let rs = c
            .heartbeat("s", &s.credential, hb.clone(), 0)
            .await
            .unwrap();
        let aa = ra.runs[0].assignment.clone().unwrap();
        let ab = rb.runs[0].assignment.clone().unwrap();
        // Shares follow the capacities 1000 and 3000.
        assert!((aa.share.fraction() - 0.25).abs() < 1e-9);
        assert!((ab.share.fraction() - 0.75).abs() < 1e-9);
        assert_eq!(ab.share.to, 1.0);
        assert_eq!((aa.vu_from, aa.vu_to, ab.vu_from, ab.vu_to), (0, 2, 2, 10));
        assert_eq!(aa.key, ab.key);
        assert_eq!(aa.start_at_ns, ab.start_at_ns);
        assert!(ra.runs[0].assignment.is_some() && rs.runs.is_empty());
        assert_eq!(rs.servers.len(), 1);
        assert_eq!(rs.servers[0].key, aa.key);
        assert!(rs.servers[0].collect.is_none());
        // A share over a node's cap is refused at start.
        let (c2, t2) = co.create(&me("leader"), CreateCluster::default()).unwrap();
        c2.join(&t2, join_req("x", &[Role::Client], 10), 0).unwrap();
        let mut sc = scenario(100.0);
        sc.scale.client_nodes = None;
        sc.scale.server_nodes = None;
        let r2 = c2.create_run(sc, &Caps::default()).unwrap();
        let err = c2.start_run(&r2.id).unwrap_err();
        assert!(err.problems[0].contains("over its cap of 10"), "{err:?}");
        // Stopping a created run finishes it with a stopped report.
        let (r2, changed) = c2.stop_run(&r2.id, "user").unwrap();
        assert!(changed);
        assert_eq!(r2.state(), RunState::Finished);
        assert_eq!(r2.report().unwrap().state, "stopped");
        assert_eq!(r2.report().unwrap().exit_code, 2);
    }

    #[tokio::test]
    async fn long_poll_answers_on_change() {
        let co = Coordinator::default();
        let (c, token) = co.create(&me("leader"), CreateCluster::default()).unwrap();
        let a = c
            .join(&token, join_req("a", &[Role::Client], 10), 0)
            .unwrap();
        let known = c.version();
        let c2 = c.clone();
        let cred = a.credential.clone();
        let t = Instant::now();
        let poll = tokio::spawn(async move {
            c2.heartbeat(
                "a",
                &cred,
                HeartbeatRequest {
                    t1: now_ns(),
                    known_version: known,
                    wait_ms: 5000,
                    ..HeartbeatRequest::default()
                },
                0,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        // The node is held: healthy while it waits.
        assert_eq!(c.describe()["members"][0]["health"], "healthy");
        c.join(&token, join_req("b", &[Role::Client], 10), 0)
            .unwrap();
        let r = poll.await.unwrap().unwrap();
        assert!(r.version > known);
        assert!(t.elapsed() < Duration::from_secs(2));
    }
    #[tokio::test]
    async fn a_snapshot_carries_what_a_successor_needs() {
        let co = Coordinator::default();
        let (c, token) = co
            .create(
                &me("leader"),
                CreateCluster {
                    lease: Some(Dur::secs(2)),
                    ..CreateCluster::default()
                },
            )
            .unwrap();
        assert_eq!(
            c.election_timeout,
            Duration::from_millis(1200),
            "below the lease"
        );
        let with_url = |id: &str, roles: &[Role]| {
            let mut r = join_req(id, roles, 1000);
            r.control_url = Some(format!("http://{id}.example:9"));
            r
        };
        // The leader is a member of its own cluster; a and b may lead.
        c.join(&token, with_url("leader", &[Role::Coordinator]), 0)
            .unwrap();
        let a = c.join(&token, with_url("a", &[Role::Client]), 0).unwrap();
        c.join(&token, with_url("b", &[Role::Server]), 0).unwrap();
        let cn = c
            .join(&token, join_req("c", &[Role::Client], 3000), 0)
            .unwrap();
        // A coordinator without a control URL is no member.
        assert_eq!(
            c.join(&token, join_req("d", &[Role::Coordinator], 1), 0)
                .unwrap_err()
                .status,
            400
        );
        assert_eq!(c.eligible().len(), 3);
        assert!(c.has_quorum());
        let run = c.create_run(scenario(100.0), &Caps::default()).unwrap();
        c.start_run(&run.id).unwrap();
        c.stop_run(&run.id, "user").unwrap();
        // The public part for everyone, the state for eligible nodes only.
        let public = c.snapshot(false);
        assert!(public.state.is_none());
        assert_eq!(public.eligible.len(), 3);
        let order: Vec<&str> = public
            .succession
            .iter()
            .map(|s| s.node_id.as_str())
            .collect();
        assert_eq!(order.len(), 2);
        assert!(order.contains(&"a") && order.contains(&"b"));
        let full = c.snapshot(true);
        let state = full.state.as_ref().unwrap();
        assert_eq!(state.members.len(), 4);
        assert_eq!(state.runs.len(), 1);
        assert_eq!(state.runs[0].stop_reason.as_deref(), Some("user"));
        assert!(!format!("{full:?}").contains(&state.secrets.secret));
        // A successor resumes at term 2: the run, the stop, the members.
        let next = Cluster::from_snapshot(&full, &me("a"), 2).unwrap();
        assert_eq!(next.term, 2);
        assert_eq!(next.leader_ref().node_id, "a");
        let resumed = next.run(&run.id).unwrap();
        assert!(resumed.is_active());
        assert_eq!(lock(&resumed.inner).stop_reason.as_deref(), Some("user"));
        assert_eq!(lock(&resumed.inner).clients.len(), 2);
        // Node credentials stay valid; the history grows.
        assert!(next.authenticate("a", &a.credential).is_ok());
        assert!(next.authenticate("a", "nope").is_err());
        assert_eq!(next.leader_view()["history"].as_array().unwrap().len(), 2);
        // A member of the old term is answered with the new one.
        let resp = next
            .heartbeat(
                "c",
                &cn.credential,
                HeartbeatRequest {
                    term: 1,
                    ..HeartbeatRequest::default()
                },
                now_ns(),
            )
            .await
            .unwrap();
        assert_eq!(resp.leader.term, 2);
        assert!(resp.snapshot.is_some());
        // A member that knows a newer term makes this leader stale.
        let err = next
            .heartbeat(
                "c",
                &cn.credential,
                HeartbeatRequest {
                    term: 3,
                    ..HeartbeatRequest::default()
                },
                now_ns(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.status, 409);
        assert!(next.is_stale());
        // The old leader refuses votes: it is alive.
        let v = c
            .answer_vote(
                &a.credential,
                &crate::harness::wire::VoteRequest {
                    term: 2,
                    candidate: "a".into(),
                    snapshot: full.version,
                },
            )
            .unwrap();
        assert!(!v.granted);
        assert_eq!(v.term, 1);
    }

    #[test]
    fn work_scores_order_the_succession() {
        let idle = work_score(0.0, 0, &WorkReport::default());
        let sender = work_score(50.0, 3, &WorkReport::default());
        let server = work_score(
            0.0,
            0,
            &WorkReport {
                arrivals_per_sec: 200.0,
                ..WorkReport::default()
            },
        );
        assert!(idle < sender && sender < server);
        assert_eq!(
            work_score(f64::INFINITY, 0, &WorkReport::default()),
            f64::MAX
        );
    }
}
