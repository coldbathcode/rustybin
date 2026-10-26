//! The cluster protocol on the wire (`docs/plans/harness.md`, section 9):
//! what a node sends when it joins and on every heartbeat, and what the
//! leader answers. Nodes pull: they call the leader, the leader never calls
//! them, so nodes behind NAT or in other networks work.
//!
//! Stats are **cumulative** since the run started (counters, and histograms
//! as exact bucket lists, [`WireHist`]): the leader replaces a node's last
//! stats with the new ones, so a lost or repeated heartbeat never counts
//! twice or drops anything. Only the timeline travels as the buckets the
//! leader has not acknowledged yet. A node that follows a new leader sends
//! everything again, so the new leader's view fills the gap exactly.
//!
//! Failover (section 9.1): every message carries the leader's term, the
//! leader replicates a versioned [`Snapshot`] (with the state and secrets a
//! successor needs, to eligible nodes only), and eligible nodes elect a new
//! leader with [`VoteRequest`] and announce it with [`ElectedRequest`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::cluster::MinNodes;
use super::engine::Share;
use super::report::{Bucket, Counts, Sample, Saturation, TargetReport, WireHist};
use super::scenario::{Scenario, ServerProfile};
use super::Role;
use crate::harness::correlate::ServerReport;

/// The leader of a cluster as a node sees it. A term has one leader; a
/// newer term always wins.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leader {
    pub node_id: String,
    pub term: u64,
    /// Where the other nodes reach it (unset: the URL they joined).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_url: Option<String>,
    /// When its term began (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
}

/// Whether a node may lead (`RUSTYBIN_HARNESS_LEADER`). A node is eligible
/// only with a control URL the other nodes reach.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaderPolicy {
    /// Eligible when the control URL is set.
    #[default]
    Auto,
    /// A standby: first in the succession, whatever its work.
    Prefer,
    /// Never leads.
    Never,
}

impl LeaderPolicy {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "prefer" | "standby" => Some(Self::Prefer),
            "never" | "off" => Some(Self::Never),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Prefer => "prefer",
            Self::Never => "never",
        }
    }
}

/// A snapshot's version: newer terms first, then the leader's sequence.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct SnapshotVersion {
    pub term: u64,
    pub seq: u64,
}

/// What a node reports of its load on every heartbeat; the leader turns it
/// into the work score that orders the succession.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkReport {
    /// Virtual clients of its running client parts.
    #[serde(default)]
    pub virtual_clients: u32,
    /// Marked requests arriving per second (server role).
    #[serde(default)]
    pub arrivals_per_sec: f64,
    /// CPU busy, in cores (Linux only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<f64>,
    /// The scheduler lag p99 of its running client part, in milliseconds.
    #[serde(default)]
    pub lag_p99_ms: f64,
}

/// What a node declares about itself when it joins: the rate split over
/// client nodes follows `max_rate`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capacity {
    pub max_rate: u64,
    pub max_inflight: usize,
    pub max_duration_secs: u64,
    #[serde(default)]
    pub threads: usize,
}

/// `POST .../clusters/{id}/nodes` (join token).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinRequest {
    pub node_id: String,
    pub roles: Vec<Role>,
    /// Server nodes: the data-plane URL the gateway reaches them at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertise: Option<String>,
    pub capacity: Capacity,
    #[serde(default)]
    pub version: String,
    pub harness_api: u32,
    /// The node's clock when it sent the request (Unix ns), for clock sync.
    #[serde(default)]
    pub t1: u64,
    /// The node's control URL as the other nodes reach it: set means it may
    /// lead (unless `leader` is `never`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_url: Option<String>,
    #[serde(default)]
    pub leader: LeaderPolicy,
}

/// The join answer: the node credential for heartbeats and the lease.
#[derive(Clone, Serialize, Deserialize)]
pub struct JoinResponse {
    pub cluster_id: String,
    pub node_id: String,
    /// Sent once; heartbeats present it as a bearer token. Never logged.
    pub credential: String,
    pub lease_ms: u64,
    /// How often a node heartbeats while it has load to report.
    pub heartbeat_ms: u64,
    /// How long without the leader before an election starts.
    #[serde(default)]
    pub election_timeout_ms: u64,
    pub leader: Leader,
    /// Served at `/.well-known/rustybin-harness` by the cluster's server
    /// nodes, and expected there by its clients (`RUSTYBIN_HARNESS_VERIFY_TARGETS`).
    pub verification_token: String,
    /// The leader's clock at arrival and at answer (Unix ns).
    pub t2: u64,
    pub t3: u64,
}

impl std::fmt::Debug for JoinResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinResponse")
            .field("cluster_id", &self.cluster_id)
            .field("node_id", &self.node_id)
            .field("credential", &"[redacted]")
            .finish_non_exhaustive()
    }
}

/// The node's estimate of the leader's clock: `offset_ns` is the leader's
/// clock minus the node's, `error_ns` its bound (half the round trip).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockReport {
    pub offset_ns: i64,
    pub error_ns: u64,
}

/// `POST .../clusters/{id}/nodes/{node}/heartbeat` (node credential).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatRequest {
    /// The node's clock when it sent the request (Unix ns).
    pub t1: u64,
    /// The cluster version the node last saw: the leader answers at once
    /// when its version is newer, else holds the request up to `wait_ms`.
    #[serde(default)]
    pub known_version: u64,
    /// Long poll, at most 10 s.
    #[serde(default)]
    pub wait_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<ClockReport>,
    /// Client side: the cumulative stats of the node's runs of this cluster.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<NodeStats>,
    /// Server side: what arrived of the cluster's runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<ServerStatsWire>,
    /// The node's kill switch was pulled: stop the cluster's runs.
    #[serde(default)]
    pub kill: bool,
    /// The node leaves the cluster.
    #[serde(default)]
    pub leave: bool,
    /// Requests in flight on the node now (its work report).
    #[serde(default)]
    pub in_flight: u64,
    /// The term of the leader this node follows: a leader that sees a newer
    /// one steps down.
    #[serde(default)]
    pub term: u64,
    /// The snapshot this node holds; a newer one comes with the answer.
    #[serde(default)]
    pub snapshot: SnapshotVersion,
    #[serde(default)]
    pub work: WorkReport,
}

/// The heartbeat answer.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HeartbeatResponse {
    pub version: u64,
    pub leader: Leader,
    pub lease_ms: u64,
    pub heartbeat_ms: u64,
    #[serde(default)]
    pub election_timeout_ms: u64,
    pub t2: u64,
    pub t3: u64,
    /// The cluster snapshot, when it changed since the node's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Box<Snapshot>>,
    /// Client side: one entry per run this node has a part in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RunControl>,
    /// Server side: the runs to correlate (key and profile) and, once the
    /// clients are done, the failed requests to look up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<ServerAssignment>,
    /// The node left (answer to `leave`).
    #[serde(default)]
    pub left: bool,
}

/// A client node's part in a run.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RunControl {
    pub run_id: String,
    /// Stop sending now (drain and report).
    #[serde(default)]
    pub stop: bool,
    /// The full assignment, until the node reports stats for the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment: Option<ClientAssignment>,
    /// Timeline buckets the leader holds for this node (send from here).
    #[serde(default)]
    pub timeline_ack: usize,
    /// The leader has the node's final stats: stop sending them.
    #[serde(default)]
    pub final_ack: bool,
    /// The node's share now: it changes when the rate is re-split (a client
    /// node gone, or a new leader handing its own share back). Empty: stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<Share>,
}

/// What a client node runs: the scenario, its share of the rate, its
/// virtual clients, the run key and the synchronised start.
#[derive(Clone, Serialize, Deserialize)]
pub struct ClientAssignment {
    pub cluster_id: String,
    pub scenario: Scenario,
    pub hash: String,
    /// The run key (hex): signs the markers, verifies the tags. Secret.
    pub key: String,
    pub share: Share,
    /// Virtual clients `[from, to)` of the cluster-wide `scale.virtual_clients`.
    pub vu_from: u32,
    pub vu_to: u32,
    /// The synchronised start on the leader's clock (Unix ns); the node
    /// converts it with its clock offset.
    pub start_at_ns: u64,
    pub verification_token: String,
}

impl std::fmt::Debug for ClientAssignment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientAssignment")
            .field("cluster_id", &self.cluster_id)
            .field("hash", &self.hash)
            .field("share", &self.share)
            .field("key", &"[redacted]")
            .finish_non_exhaustive()
    }
}

/// What a server node correlates.
#[derive(Clone, Serialize, Deserialize)]
pub struct ServerAssignment {
    pub run_id: String,
    pub key: String,
    pub profile: ServerProfile,
    /// The failed requests of the run's clients, by client node (encoded
    /// sequence numbers): answer which ones arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collect: Option<BTreeMap<String, String>>,
}

impl std::fmt::Debug for ServerAssignment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerAssignment")
            .field("run_id", &self.run_id)
            .field("key", &"[redacted]")
            .field("collect", &self.collect.is_some())
            .finish_non_exhaustive()
    }
}

/// Per-layer or per-target cumulative stats.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WirePart {
    pub counts: Counts,
    pub corrected: WireHist,
    pub overhead: WireHist,
}

/// The run-wide histograms of a node.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HistSet {
    pub corrected: WireHist,
    pub service: WireHist,
    pub upstream: WireHist,
    pub overhead: WireHist,
    pub request_leg: WireHist,
    pub response_leg: WireHist,
    pub lag: WireHist,
}

/// A client node's cumulative stats of one run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeStats {
    pub run_id: String,
    /// `starting`, `running`, `draining`, `finished`, or `failed` (the node
    /// refused or could not start its assignment, see `error`).
    pub state: String,
    /// The node's run ended and these are its last stats.
    #[serde(default, rename = "final")]
    pub done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_by: Option<String>,
    #[serde(default)]
    pub elapsed_secs: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_lag_ms: Option<f64>,
    #[serde(default)]
    pub in_flight: u64,
    #[serde(default)]
    pub counts: Counts,
    #[serde(default)]
    pub hist: HistSet,
    #[serde(default)]
    pub layers: Vec<WirePart>,
    #[serde(default)]
    pub targets: Vec<WirePart>,
    #[serde(default)]
    pub target_reports: Vec<TargetReport>,
    /// The index of the first bucket in `timeline`.
    #[serde(default)]
    pub timeline_from: usize,
    #[serde(default)]
    pub timeline: Vec<Bucket>,
    /// The node's buckets so far (`timeline` may carry only a part, the
    /// rest comes on the next heartbeats).
    #[serde(default)]
    pub timeline_total: usize,
    #[serde(default)]
    pub saturation: Saturation,
    /// Final stats only: the node's most telling samples.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<Sample>,
    /// Final stats only: the sequence numbers of transport failures
    /// (encoded), for the `lost` check against the server nodes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub failed_seqs: String,
    /// Transport failures not in `failed_seqs` (over the cap).
    #[serde(default)]
    pub failed_untracked: u64,
}

/// A server node's cumulative view of one run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerStatsWire {
    pub run_id: String,
    pub report: ServerReport,
    pub processing: WireHist,
    /// Markers this node refused since it learned the run.
    #[serde(default)]
    pub rejected: u64,
    /// The answer to the leader's `collect`: by client node, the failed
    /// sequence numbers that did arrive, and the ones this node could not
    /// tell (encoded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collected: Option<Collected>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Collected {
    #[serde(default)]
    pub seen: BTreeMap<String, String>,
    #[serde(default)]
    pub untracked: BTreeMap<String, String>,
}

// ── Failover (section 9.1) ──────────────────────────────────────────

/// The replicated cluster state. Every node gets the public part (the
/// leader, the term, who leads next and where); eligible nodes also get
/// `state`, which holds what a successor needs to carry on, secrets included.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub cluster_id: String,
    pub version: SnapshotVersion,
    pub leader: Leader,
    pub lease_ms: u64,
    pub election_timeout_ms: u64,
    /// The nodes that may lead (the leader included): elections need a
    /// majority of them.
    pub eligible: Vec<String>,
    /// Who leads next, in order: standbys first, then by work, then by id.
    pub succession: Vec<Successor>,
    /// Leader changes, oldest first (bounded).
    #[serde(default)]
    pub history: Vec<LeaderChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<SnapState>,
}

impl Snapshot {
    /// A majority of the eligible nodes.
    pub fn majority(&self) -> usize {
        self.eligible.len() / 2 + 1
    }

    /// Failover needs three eligible nodes: with two, a majority is both.
    pub fn failover_available(&self) -> bool {
        self.eligible.len() >= 3
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Successor {
    pub node_id: String,
    pub control_url: String,
    #[serde(default)]
    pub prefer: bool,
    #[serde(default)]
    pub work_score: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaderChange {
    pub term: u64,
    pub node_id: String,
    /// RFC 3339.
    pub at: String,
    /// `created`, `elected`.
    pub reason: String,
}

/// What a successor needs to lead: members, runs with their assignments
/// and last stats (the checkpoint), and the secrets.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SnapState {
    pub created_at: String,
    pub min_nodes: MinNodes,
    pub members: Vec<SnapMember>,
    pub runs: Vec<SnapRun>,
    pub secrets: SnapSecrets,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SnapMember {
    pub node_id: String,
    pub roles: Vec<Role>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertise: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_url: Option<String>,
    #[serde(default)]
    pub leader: LeaderPolicy,
    pub capacity: Capacity,
    #[serde(default)]
    pub version: String,
    /// Part of the node credential (with the cluster secret).
    pub incarnation: String,
    pub joined_at: String,
    #[serde(default)]
    pub left: bool,
}

/// A run that has not finished, as a successor resumes it.
#[derive(Clone, Serialize, Deserialize)]
pub struct SnapRun {
    pub id: String,
    pub scenario: Scenario,
    pub hash: String,
    /// The run key (hex). Secret.
    pub key: String,
    pub created_at: String,
    /// `created`, `starting`, `running` or `collecting`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    pub start_at_ns: u64,
    pub clients: Vec<SnapClient>,
    pub servers: Vec<SnapServer>,
}

impl std::fmt::Debug for SnapRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapRun")
            .field("id", &self.id)
            .field("state", &self.state)
            .field("key", &"[redacted]")
            .finish_non_exhaustive()
    }
}

/// A client part with the checkpoint of its stats: counters for every
/// node (they report again to a new leader), histograms too for the nodes
/// that will not (gone, or the leader's own part).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SnapClient {
    pub node_id: String,
    pub share: Share,
    pub vu_from: u32,
    pub vu_to: u32,
    /// Its share was re-split over the others (the node is gone, or it
    /// leads now).
    #[serde(default)]
    pub gone: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<NodeStats>,
    /// When the leader got those stats (its clock, Unix ns).
    #[serde(default)]
    pub stats_at_ns: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SnapServer {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<ServerStatsWire>,
}

/// The cluster secret (node credentials derive from it), the join token's
/// hash and the verification token. Eligible nodes only; never logged.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SnapSecrets {
    pub secret: String,
    pub join_hash: String,
    pub verification_token: String,
}

impl std::fmt::Debug for SnapSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SnapSecrets([redacted])")
    }
}

/// `POST .../election/vote`: a candidate asks for a vote (its node
/// credential as bearer).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoteRequest {
    pub term: u64,
    pub candidate: String,
    pub snapshot: SnapshotVersion,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VoteResponse {
    pub granted: bool,
    /// The voter's term (newer than the candidate's: the candidate stands down).
    pub term: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<Leader>,
    #[serde(default)]
    pub reason: String,
}

/// `POST .../election/elected`: a new leader announces its term (its node
/// credential as bearer).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElectedRequest {
    pub leader: Leader,
}

/// A node that does not lead the cluster answers a heartbeat with the
/// leader it knows (409), so members find the new leader.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NotLeader {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<Leader>,
}
