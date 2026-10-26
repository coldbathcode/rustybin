//! The cluster protocol on the wire (`docs/plans/harness.md`, section 9):
//! what a node sends when it joins and on every heartbeat, and what the
//! leader answers. Nodes pull: they call the leader, the leader never calls
//! them, so nodes behind NAT or in other networks work.
//!
//! Stats are **cumulative** since the run started (counters, and histograms
//! as exact bucket lists, [`WireHist`]): the leader replaces a node's last
//! stats with the new ones, so a lost or repeated heartbeat never counts
//! twice or drops anything. Only the timeline travels as the buckets the
//! leader has not acknowledged yet.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::engine::Share;
use super::report::{Bucket, Counts, Sample, Saturation, TargetReport, WireHist};
use super::scenario::{Scenario, ServerProfile};
use super::Role;
use crate::harness::correlate::ServerReport;

/// The leader of a cluster as a node sees it (one term in phase 3: the
/// leader is the coordinator, and there is no failover yet).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leader {
    pub node_id: String,
    pub term: u64,
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
}

/// The heartbeat answer.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HeartbeatResponse {
    pub version: u64,
    pub leader: Leader,
    pub lease_ms: u64,
    pub heartbeat_ms: u64,
    pub t2: u64,
    pub t3: u64,
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
