//! A node's membership in a cluster (`docs/plans/harness.md`, sections 9
//! and 9.1): the agent that joins the leader, heartbeats, keeps the clock
//! estimate, runs the client assignments it gets (its share of the rate,
//! its virtual clients, the synchronised start, under a lease) and, on a
//! server node, correlates the cluster's runs and answers the `lost` lookup.
//!
//! Joining happens from the environment at startup (`RUSTYBIN_HARNESS_JOIN`,
//! `RUSTYBIN_HARNESS_JOIN_TOKEN`, retried until the leader answers) or when
//! a controller calls `POST /_rustybin/harness/join`. A node is a member of
//! one cluster at a time. The node that leads a cluster is a member of its
//! own cluster, over an in-process link instead of HTTP.
//!
//! Failover: the agent follows the leader with the newest term. It keeps
//! the latest snapshot; when the leader has not answered for the election
//! timeout it asks the nodes of the snapshot's succession list, one per
//! retry, until one names a newer leader. An eligible node also stands for
//! election at its position in that list ([`Membership::stand`]), votes for
//! other candidates ([`Membership::vote`]) and follows a new leader's
//! announcement ([`Membership::elected`]).
//!
//! Safety: every assignment is validated against this node's own caps and
//! pinned against its own allowlist (`RUSTYBIN_HARNESS_TARGETS`), so a
//! leader can never aim a node anywhere its operator did not allow; a new
//! share is checked the same way. A run stops when no leader of the current
//! or a newer term has answered for the lease period.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::watch;

use super::cluster::{decode_secret, node_credential, NodeError, MAX_WAIT};
use super::correlate::{now_ns, RunEntry, RunKey};
use super::engine::{Run, Share};
use super::report::{decode_numbers, encode_numbers, WireHist};
use super::safety::Lease;
use super::wire::{
    Capacity, ClientAssignment, ClockReport, Collected, ElectedRequest, HeartbeatRequest,
    HeartbeatResponse, JoinRequest, JoinResponse, Leader, NodeStats, NotLeader, ServerAssignment,
    ServerStatsWire, SnapState, Snapshot, VoteRequest, VoteResponse, WorkReport,
};
use super::{Harness, RunOptions, HARNESS_API, PREFIX};

/// Clock samples kept (the one with the shortest round trip wins).
pub const CLOCK_SAMPLES: usize = 8;
/// Runs an agent tracks at once (client and server side each).
pub const MAX_AGENT_RUNS: usize = 8;
/// Longest pause between retries when the leader cannot be reached.
pub const MAX_BACKOFF: Duration = Duration::from_secs(5);
/// Time allowed for a join call.
pub const JOIN_TIMEOUT: Duration = Duration::from_secs(10);
/// Pause between calls while the leader is lost (looking for a new one).
pub const LOST_RETRY: Duration = Duration::from_millis(100);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// NTP-style clock estimate from the four stamps of a call: `t1` node
/// send, `t2` leader receive, `t3` leader send, `t4` node receive.
#[derive(Clone, Debug, Default)]
pub struct ClockEstimate {
    samples: VecDeque<(i64, u64)>,
}

impl ClockEstimate {
    /// Add one round: offset `((t2 - t1) + (t3 - t4)) / 2` (the leader's
    /// clock minus this node's), delay `(t4 - t1) - (t3 - t2)`.
    pub fn add(&mut self, t1: u64, t2: u64, t3: u64, t4: u64) {
        if t1 == 0 || t2 == 0 || t3 == 0 || t4 < t1 || t3 < t2 {
            return;
        }
        let offset = ((t2 as i128 - t1 as i128) + (t3 as i128 - t4 as i128)) / 2;
        let delay = (t4 - t1).saturating_sub(t3 - t2);
        if self.samples.len() >= CLOCK_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back((
            offset.clamp(i64::MIN as i128, i64::MAX as i128) as i64,
            delay,
        ));
    }

    /// The sample with the shortest round trip: its offset, and half the
    /// round trip as the error bound.
    pub fn best(&self) -> Option<ClockReport> {
        self.samples
            .iter()
            .min_by_key(|(_, d)| *d)
            .map(|(o, d)| ClockReport {
                offset_ns: *o,
                error_ns: d / 2,
            })
    }

    /// A new leader has another clock: start over.
    fn reset(&mut self) {
        self.samples.clear();
    }
}

/// Where the leader is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    /// The cluster URL on the leader (`<control>/_rustybin/harness/clusters/<id>`).
    Url(String),
    /// This node leads the cluster: calls stay in process.
    Local,
}

#[derive(Debug)]
pub(super) enum LinkError {
    /// The credential or token was refused.
    Auth(String),
    /// No such cluster there (any more).
    Gone(String),
    /// The node called does not lead the cluster; the leader it knows.
    NotLeader(Option<Leader>),
    /// The leader answered without renewing: it does not hear a majority.
    Refused(String),
    Other(String),
}

impl LinkError {
    fn text(&self) -> String {
        match self {
            LinkError::Auth(s)
            | LinkError::Gone(s)
            | LinkError::Refused(s)
            | LinkError::Other(s) => s.clone(),
            LinkError::NotLeader(Some(l)) => format!(
                "not the leader; the leader it knows is {} (term {})",
                l.node_id, l.term
            ),
            LinkError::NotLeader(None) => "not the leader (it no longer leads)".into(),
        }
    }
}

fn from_node_error(e: NodeError) -> LinkError {
    match e.status {
        401 | 403 => LinkError::Auth(e.error),
        404 => LinkError::Gone(e.error),
        409 => LinkError::NotLeader(None),
        503 => LinkError::Refused(e.error),
        _ => LinkError::Other(format!("{} ({})", e.error, e.status)),
    }
}

/// The cluster URL on a node with this control URL.
pub fn cluster_url_of(control_url: &str, cluster: &str) -> String {
    format!(
        "{}{PREFIX}/clusters/{cluster}",
        control_url.trim_end_matches('/')
    )
}

/// One cluster protocol call over HTTP: JSON in and out, a bearer token
/// (the join token or the node credential). `from` is the calling node's
/// port (test partitions).
pub(super) async fn post_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    from: Option<u16>,
    url: &str,
    bearer: &str,
    body: &impl serde::Serialize,
    timeout: Duration,
) -> Result<T, LinkError> {
    #[cfg(test)]
    match super::test_net::blocked(from, url) {
        Some(super::test_net::Cut::Drop) => {
            tokio::time::sleep(timeout).await;
            return Err(LinkError::Other("timed out (test partition)".into()));
        }
        Some(super::test_net::Cut::Refuse) => {
            return Err(LinkError::Other("unreachable (test partition)".into()));
        }
        None => {}
    }
    #[cfg(not(test))]
    let _ = from;
    let resp = client
        .post(url)
        .bearer_auth(bearer)
        .timeout(timeout)
        .json(body)
        .send()
        .await
        .map_err(|e| LinkError::Other(format!("cannot reach {url}: {e}")))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| LinkError::Other(format!("the answer broke off: {e}")))?;
    if status.is_success() {
        return serde_json::from_slice(&bytes)
            .map_err(|e| LinkError::Other(format!("the answer does not parse: {e}")));
    }
    let parsed = serde_json::from_slice::<Value>(&bytes).ok();
    let message = parsed
        .as_ref()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| format!("answered {status}"));
    Err(match status.as_u16() {
        401 | 403 => LinkError::Auth(message),
        404 => LinkError::Gone(message),
        409 => LinkError::NotLeader(
            parsed
                .and_then(|v| serde_json::from_value::<NotLeader>(v).ok())
                .and_then(|n| n.leader),
        ),
        503 => LinkError::Refused(message),
        _ => LinkError::Other(format!("{message} ({status})")),
    })
}

/// A client run of the cluster on this node.
struct ClientRunState {
    run: Option<Arc<Run>>,
    error: Option<String>,
    lease: Arc<Lease>,
    timeline_ack: usize,
    final_acked: bool,
}

/// A run this node correlates as a server of the cluster.
struct ServerRunState {
    entry: Arc<RunEntry>,
    rejected_before: u64,
    collected: Option<Collected>,
}

/// Counters at the last heartbeat, for the per-second rates of the work
/// report.
struct WorkMarks {
    at: Instant,
    arrivals: u64,
    cpu: Option<f64>,
}

struct AgentState {
    target: Target,
    credential: Option<String>,
    lease: Duration,
    heartbeat_every: Duration,
    election_timeout: Duration,
    leader: Leader,
    verification_token: Option<String>,
    version: u64,
    /// The latest snapshot from the leader (with the cluster state and
    /// secrets on an eligible node).
    snapshot: Option<Snapshot>,
    /// The term this node last voted in, and for whom.
    voted: Option<(u64, String)>,
    /// Not before this: it voted for another candidate, or lost a round.
    next_stand: Option<Instant>,
    last_ok: Option<Instant>,
    last_error: Option<String>,
    failures: u32,
    clock: ClockEstimate,
    client_runs: BTreeMap<String, ClientRunState>,
    server_runs: BTreeMap<String, ServerRunState>,
    kill_pending: bool,
    marks: Option<WorkMarks>,
}

/// This node's membership in a cluster.
pub struct Membership {
    pub cluster_id: String,
    node_id: String,
    pub(super) client: reqwest::Client,
    join_token: String,
    advertise: Option<String>,
    pub joined_at: chrono::DateTime<chrono::Utc>,
    state: Mutex<AgentState>,
    stop: watch::Sender<bool>,
}

/// Where to join: a cluster URL, or a cluster this node leads.
pub enum JoinTarget {
    Url(String),
    Local(String),
}

/// Split a join URL into the cluster URL and its id. A base URL
/// (`https://coord:8080`) takes the id from `cluster`.
pub fn cluster_url(raw: &str, cluster: Option<&str>) -> Result<(String, String), String> {
    let raw = raw.trim().trim_end_matches('/');
    let marker = format!("{PREFIX}/clusters/");
    if let Some(i) = raw.find(&marker) {
        let id = &raw[i + marker.len()..];
        if !super::cluster::safe_id(id) {
            return Err(format!("the join URL names no valid cluster id ({id:?})"));
        }
        if cluster.is_some_and(|c| c != id) {
            return Err(format!(
                "the join URL names cluster {id}, RUSTYBIN_HARNESS_CLUSTER says {}",
                cluster.unwrap_or_default()
            ));
        }
        return Ok((raw.to_string(), id.to_string()));
    }
    match cluster {
        Some(id) if super::cluster::safe_id(id) => {
            Ok((format!("{raw}{marker}{id}"), id.to_string()))
        }
        _ => Err(format!(
            "the join URL must be <leader>{PREFIX}/clusters/<id>, or the leader's base URL with RUSTYBIN_HARNESS_CLUSTER"
        )),
    }
}

pub(super) fn http_client() -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder();
    match crate::a2a::push::tls_config() {
        Ok(tls) => builder = builder.use_preconfigured_tls(tls),
        Err(e) => tracing::warn!("harness join: https unavailable ({e})"),
    }
    builder
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("rustybin-harness/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())
}

/// Check a node credential against the snapshot's secret.
fn check_credential(
    state: &SnapState,
    cluster: &str,
    node_id: &str,
    credential: &str,
) -> Result<(), NodeError> {
    let Some(m) = state.members.iter().find(|m| m.node_id == node_id) else {
        return Err(NodeError::new(401, "unknown node"));
    };
    let secret = decode_secret(&state.secrets.secret).map_err(|e| NodeError::new(500, e))?;
    let expected = node_credential(&secret, cluster, node_id, &m.incarnation);
    if crate::control_auth::constant_time_eq(&expected, credential) {
        Ok(())
    } else {
        Err(NodeError::new(401, "invalid node credential"))
    }
}

impl Membership {
    fn new(
        h: &Arc<Harness>,
        target: JoinTarget,
        token: String,
        advertise: Option<String>,
    ) -> Result<Arc<Self>, String> {
        let (target, cluster_id) = match target {
            JoinTarget::Url(raw) => {
                let (url, id) = cluster_url(&raw, None)?;
                (Target::Url(url), id)
            }
            JoinTarget::Local(id) => (Target::Local, id),
        };
        let (stop, _) = watch::channel(false);
        Ok(Arc::new(Self {
            cluster_id,
            node_id: h.node_id.clone(),
            client: http_client()?,
            join_token: token,
            advertise,
            joined_at: chrono::Utc::now(),
            state: Mutex::new(AgentState {
                target,
                credential: None,
                lease: super::cluster::DEFAULT_LEASE,
                heartbeat_every: Duration::from_secs(1),
                election_timeout: super::cluster::DEFAULT_ELECTION_TIMEOUT,
                leader: Leader::default(),
                verification_token: None,
                version: 0,
                snapshot: None,
                voted: None,
                next_stand: None,
                last_ok: None,
                last_error: None,
                failures: 0,
                clock: ClockEstimate::default(),
                client_runs: BTreeMap::new(),
                server_runs: BTreeMap::new(),
                kill_pending: false,
                marks: None,
            }),
            stop,
        }))
    }

    /// The cluster URL on the leader; `None` when this node leads.
    pub fn url(&self) -> Option<String> {
        match &lock(&self.state).target {
            Target::Url(u) => Some(u.clone()),
            Target::Local => None,
        }
    }

    /// The leader this node follows.
    pub fn leader(&self) -> Leader {
        lock(&self.state).leader.clone()
    }

    /// The cluster's verification token (served by its server nodes).
    pub fn verification_token(&self) -> Option<String> {
        lock(&self.state).verification_token.clone()
    }

    /// The kill switch was pulled on this node: tell the leader.
    pub fn request_kill(&self) {
        lock(&self.state).kill_pending = true;
    }

    pub fn is_stopped(&self) -> bool {
        *self.stop.borrow()
    }

    /// Status for `GET /_rustybin/harness`.
    pub fn describe(&self) -> Value {
        let st = lock(&self.state);
        let clock = st.clock.best();
        json!({
            "cluster_id": self.cluster_id,
            "url": match &st.target { Target::Url(u) => Some(u.clone()), Target::Local => None },
            "local": st.target == Target::Local,
            "joined": st.credential.is_some(),
            "joined_at": self.joined_at.to_rfc3339(),
            "advertise": self.advertise,
            "leader": st.leader,
            "term": st.leader.term,
            "lease_ms": st.lease.as_millis() as u64,
            "election_timeout_ms": st.election_timeout.as_millis() as u64,
            "last_answer_ms": st.last_ok.map(|t| t.elapsed().as_millis() as u64),
            "last_error": st.last_error,
            "clock_offset_ms": clock.map(|c| c.offset_ns as f64 / 1e6),
            "clock_error_ms": clock.map(|c| c.error_ns as f64 / 1e6),
            "snapshot": st.snapshot.as_ref().map(|s| json!({
                "version": s.version,
                "eligible": s.eligible.len(),
                "holds_state": s.state.is_some(),
            })),
            "voted_term": st.voted.as_ref().map(|(t, _)| *t),
            "client_runs": st.client_runs.keys().collect::<Vec<_>>(),
            "server_runs": st.server_runs.keys().collect::<Vec<_>>(),
        })
    }

    /// `GET .../clusters/{id}/leader` on a member: the leader, the
    /// succession and the history as its snapshot says.
    pub fn leader_view(&self) -> Value {
        let st = lock(&self.state);
        let eligible = st
            .snapshot
            .as_ref()
            .map(|s| s.eligible.clone())
            .unwrap_or_default();
        json!({
            "cluster_id": self.cluster_id,
            "leader": st.leader,
            "term": st.leader.term,
            "answered_by": self.node_id,
            "following": match &st.target { Target::Url(u) => Some(u.clone()), Target::Local => None },
            "last_answer_ms": st.last_ok.map(|t| t.elapsed().as_millis() as u64),
            "eligible": eligible,
            "failover": if eligible.len() >= 3 {
                "available".to_string()
            } else {
                format!("unavailable: {} eligible node(s), failover needs three (a majority of two is both)", eligible.len())
            },
            "election_timeout_ms": st.election_timeout.as_millis() as u64,
            "lease_ms": st.lease.as_millis() as u64,
            "succession": st.snapshot.as_ref().map(|s| s.succession.clone()).unwrap_or_default(),
            "history": st.snapshot.as_ref().map(|s| s.history.clone()).unwrap_or_default(),
            "snapshot": st.snapshot.as_ref().map(|s| s.version),
        })
    }

    fn join_request(&self, h: &Harness) -> JoinRequest {
        let caps = h.caps();
        JoinRequest {
            node_id: h.node_id.clone(),
            roles: h.config.roles.clone(),
            advertise: if h.is_server() {
                self.advertise.clone().or_else(|| h.default_advertise())
            } else {
                None
            },
            capacity: Capacity {
                max_rate: caps.max_rate,
                max_inflight: caps.max_inflight,
                max_duration_secs: caps.max_duration_secs,
                threads: h.engine_threads(),
            },
            version: env!("CARGO_PKG_VERSION").into(),
            harness_api: HARNESS_API,
            t1: now_ns(),
            control_url: h.control_url(),
            leader: h.config.leader,
        }
    }

    /// Call the leader (or another node) at `target`.
    async fn call_join(
        &self,
        h: &Arc<Harness>,
        target: &Target,
        req: &JoinRequest,
    ) -> Result<JoinResponse, LinkError> {
        match target {
            Target::Url(url) => {
                post_json(
                    &self.client,
                    h.bound_port(),
                    &format!("{url}/nodes"),
                    &self.join_token,
                    req,
                    JOIN_TIMEOUT,
                )
                .await
            }
            Target::Local => {
                let cluster = h
                    .coordinator
                    .get(&self.cluster_id)
                    .ok_or_else(|| LinkError::Gone(format!("no cluster {}", self.cluster_id)))?;
                cluster
                    .join(&self.join_token, req.clone(), now_ns())
                    .map_err(from_node_error)
            }
        }
    }

    async fn call_heartbeat(
        &self,
        h: &Arc<Harness>,
        target: &Target,
        credential: &str,
        req: &HeartbeatRequest,
    ) -> Result<HeartbeatResponse, LinkError> {
        // A leader that does not answer within half the election timeout
        // (past the long poll) counts as silent: the node must notice a lost
        // leader well inside its lease, even when packets are dropped.
        let e = lock(&self.state).election_timeout;
        let timeout = Duration::from_millis(req.wait_ms)
            + (e / 2).clamp(Duration::from_millis(200), Duration::from_secs(5));
        match target {
            Target::Url(url) => {
                post_json(
                    &self.client,
                    h.bound_port(),
                    &format!("{url}/nodes/{}/heartbeat", self.node_id),
                    credential,
                    req,
                    timeout,
                )
                .await
            }
            Target::Local => {
                let cluster = h
                    .coordinator
                    .get(&self.cluster_id)
                    .ok_or_else(|| LinkError::Gone(format!("no cluster {}", self.cluster_id)))?;
                let t2 = now_ns();
                tokio::time::timeout(
                    timeout,
                    cluster.heartbeat(&self.node_id, credential, req.clone(), t2),
                )
                .await
                .map_err(|_| LinkError::Other("heartbeat timed out".into()))?
                .map_err(from_node_error)
            }
        }
    }

    async fn try_join(&self, h: &Arc<Harness>) -> Result<(), String> {
        let req = self.join_request(h);
        let t1 = req.t1;
        let target = lock(&self.state).target.clone();
        let mut answer = self.call_join(h, &target, &req).await;
        // A member that does not lead names the leader: join there.
        if let Err(LinkError::NotLeader(Some(l))) = &answer {
            if let Some(url) = &l.control_url {
                let target = Target::Url(cluster_url_of(url, &self.cluster_id));
                lock(&self.state).target = target.clone();
                answer = self.call_join(h, &target, &req).await;
            }
        }
        match answer {
            Ok(resp) => {
                let t4 = now_ns();
                let mut st = lock(&self.state);
                st.clock.add(t1, resp.t2, resp.t3, t4);
                st.credential = Some(resp.credential);
                st.lease = Duration::from_millis(resp.lease_ms);
                st.heartbeat_every = Duration::from_millis(resp.heartbeat_ms.max(50));
                if resp.election_timeout_ms > 0 {
                    st.election_timeout = Duration::from_millis(resp.election_timeout_ms);
                }
                st.leader = resp.leader;
                st.verification_token = Some(resp.verification_token);
                st.version = 0;
                st.last_ok = Some(Instant::now());
                st.last_error = None;
                st.failures = 0;
                tracing::info!(
                    target: "rustybin::harness",
                    cluster = %self.cluster_id,
                    node = %h.node_id,
                    leader = %st.leader.node_id,
                    term = st.leader.term,
                    "harness: joined the cluster"
                );
                Ok(())
            }
            Err(e) => {
                let mut st = lock(&self.state);
                st.last_error = Some(e.text());
                st.failures += 1;
                Err(e.text())
            }
        }
    }

    /// The work report: virtual clients and scheduler lag of the running
    /// client parts, arrivals per second and CPU since the last heartbeat.
    fn work_report(st: &mut AgentState) -> WorkReport {
        let mut w = WorkReport::default();
        for cr in st.client_runs.values() {
            if let Some(run) = cr.run.as_ref().filter(|r| r.is_active()) {
                w.virtual_clients += run.virtual_clients().1;
                w.lag_p99_ms = w.lag_p99_ms.max(run.last_lag_p99_ms());
            }
        }
        let arrivals: u64 = st
            .server_runs
            .values()
            .map(|sr| lock(&sr.entry.stats).arrivals)
            .sum();
        let cpu = super::engine::cpu_seconds();
        let now = Instant::now();
        if let Some(m) = &st.marks {
            let secs = now.duration_since(m.at).as_secs_f64();
            if secs > 0.0 {
                w.arrivals_per_sec = arrivals.saturating_sub(m.arrivals) as f64 / secs;
                if let (Some(a), Some(b)) = (m.cpu, cpu) {
                    w.cpu = Some(((b - a) / secs).max(0.0));
                }
            }
        }
        st.marks = Some(WorkMarks {
            at: now,
            arrivals,
            cpu,
        });
        w
    }

    /// The heartbeat request: stats of every run, the clock, the work
    /// report, the kill switch.
    fn heartbeat_request(&self, h: &Harness, leave: bool) -> HeartbeatRequest {
        let mut st = lock(&self.state);
        let mut runs = Vec::new();
        let mut in_flight = 0;
        for (id, cr) in &st.client_runs {
            if cr.final_acked {
                continue;
            }
            if let Some(run) = &cr.run {
                let stats = run.export(cr.timeline_ack.saturating_sub(1));
                in_flight += stats.in_flight;
                runs.push(stats);
            } else if let Some(e) = &cr.error {
                runs.push(NodeStats {
                    run_id: id.clone(),
                    state: "failed".into(),
                    done: true,
                    error: Some(e.clone()),
                    ..NodeStats::default()
                });
            }
        }
        let rejected = h.correlator.rejected();
        let servers = st
            .server_runs
            .iter()
            .map(|(id, sr)| {
                let stats = lock(&sr.entry.stats);
                ServerStatsWire {
                    run_id: id.clone(),
                    report: stats.report(),
                    processing: WireHist::encode(stats.processing()),
                    rejected: rejected.saturating_sub(sr.rejected_before),
                    collected: sr.collected.clone(),
                }
            })
            .collect();
        let kill = std::mem::take(&mut st.kill_pending);
        let active = st
            .client_runs
            .values()
            .any(|c| c.run.as_ref().is_none_or(|r| r.is_active()) && c.error.is_none());
        let work = Self::work_report(&mut st);
        HeartbeatRequest {
            t1: now_ns(),
            known_version: st.version,
            // An idle node long-polls, but never past a third of the
            // election timeout, so it notices a lost leader in time.
            wait_ms: if active || leave || kill {
                0
            } else {
                MAX_WAIT.min(st.election_timeout / 3).as_millis() as u64
            },
            clock: st.clock.best(),
            runs,
            servers,
            kill,
            leave,
            in_flight,
            term: st.leader.term,
            snapshot: st.snapshot.as_ref().map(|s| s.version).unwrap_or_default(),
            work,
        }
    }

    /// A heartbeat answer arrived. False when it came from a leader of an
    /// older term (no renewal).
    fn accept_answer(self: &Arc<Self>, h: &Arc<Harness>, resp: HeartbeatResponse, t1: u64) -> bool {
        let t4 = now_ns();
        {
            let mut st = lock(&self.state);
            if resp.leader.term < st.leader.term {
                st.last_error = Some(format!(
                    "an answer from {} of term {}, older than term {}: ignored",
                    resp.leader.node_id, resp.leader.term, st.leader.term
                ));
                return false;
            }
            if resp.leader.node_id != st.leader.node_id {
                st.clock.reset();
            }
            st.clock.add(t1, resp.t2, resp.t3, t4);
            st.last_ok = Some(Instant::now());
            st.last_error = None;
            st.failures = 0;
        }
        self.process(h, resp);
        true
    }

    /// Act on the leader's answer.
    fn process(self: &Arc<Self>, h: &Arc<Harness>, resp: HeartbeatResponse) {
        let mut to_start: Vec<(String, ClientAssignment, Arc<Lease>, i64)> = Vec::new();
        let mut reshare: Vec<(Arc<Run>, Share)> = Vec::new();
        let mut follow: Vec<(Arc<Run>, super::live::Controls)> = Vec::new();
        let mut st = lock(&self.state);
        if resp.leader.term > st.leader.term || resp.leader.node_id != st.leader.node_id {
            // A new leader: it gets everything again (cumulative stats).
            if st.leader.term > 0 {
                tracing::info!(
                    target: "rustybin::harness",
                    cluster = %self.cluster_id,
                    leader = %resp.leader.node_id,
                    term = resp.leader.term,
                    "harness: following a new leader"
                );
            }
            for cr in st.client_runs.values_mut() {
                cr.final_acked = false;
                cr.timeline_ack = 0;
            }
        }
        st.version = resp.version;
        st.leader = resp.leader;
        st.lease = Duration::from_millis(resp.lease_ms.max(1));
        st.heartbeat_every = Duration::from_millis(resp.heartbeat_ms.max(50));
        if resp.election_timeout_ms > 0 {
            st.election_timeout = Duration::from_millis(resp.election_timeout_ms);
        }
        if let Some(snap) = resp.snapshot {
            if st.snapshot.as_ref().is_none_or(|s| {
                snap.version > s.version || snap.state.is_some() != s.state.is_some()
            }) {
                st.snapshot = Some(*snap);
            }
        }
        // Every answer renews the leases: the leader is there.
        for cr in st.client_runs.values() {
            cr.lease.renew();
        }
        let offset = st.clock.best().map_or(0, |c| c.offset_ns);
        let lease_period = st.lease;
        let mut listed = Vec::new();
        for rc in resp.runs {
            listed.push(rc.run_id.clone());
            match st.client_runs.get_mut(&rc.run_id) {
                Some(cr) => {
                    cr.timeline_ack = rc.timeline_ack;
                    cr.final_acked = rc.final_ack;
                    if let Some(run) = &cr.run {
                        if let Some(c) = rc.controls {
                            follow.push((run.clone(), *c));
                        }
                        if rc.stop {
                            if run.is_active() {
                                run.stop(if rc.share.is_none() {
                                    "the leader re-split this node's share over the others"
                                } else {
                                    "leader"
                                });
                            }
                        } else if let Some(share) = rc.share {
                            if share != run.share() {
                                reshare.push((run.clone(), share));
                            }
                        }
                    }
                }
                None => {
                    let Some(asg) = rc.assignment else { continue };
                    if st.client_runs.len() >= MAX_AGENT_RUNS {
                        let done: Vec<String> = st
                            .client_runs
                            .iter()
                            .filter(|(_, c)| {
                                c.run.as_ref().is_some_and(|r| !r.is_active()) || c.error.is_some()
                            })
                            .map(|(k, _)| k.clone())
                            .collect();
                        for k in done {
                            st.client_runs.remove(&k);
                        }
                    }
                    if st.client_runs.len() >= MAX_AGENT_RUNS {
                        continue;
                    }
                    let lease = Lease::new(lease_period);
                    st.client_runs.insert(
                        rc.run_id.clone(),
                        ClientRunState {
                            run: None,
                            error: None,
                            lease: lease.clone(),
                            timeline_ack: 0,
                            final_acked: false,
                        },
                    );
                    if rc.stop {
                        if let Some(cr) = st.client_runs.get_mut(&rc.run_id) {
                            cr.error = Some("stopped before it started".into());
                        }
                        continue;
                    }
                    to_start.push((rc.run_id, asg, lease, offset));
                }
            }
        }
        // Runs the leader no longer lists: stop what still sends, forget
        // what is done.
        let unlisted: Vec<String> = st
            .client_runs
            .keys()
            .filter(|k| !listed.contains(k))
            .cloned()
            .collect();
        for k in unlisted {
            let keep = match st.client_runs.get(&k) {
                Some(cr) => match &cr.run {
                    Some(run) if run.is_active() => {
                        run.stop("the leader no longer lists the run");
                        true
                    }
                    None if cr.error.is_none() => true, // still starting
                    _ => false,
                },
                None => false,
            };
            if !keep {
                st.client_runs.remove(&k);
            }
        }
        // The server side: learn new runs, answer the lost lookup.
        let mut server_listed = Vec::new();
        for sa in resp.servers {
            server_listed.push(sa.run_id.clone());
            self.serve(h, &mut st, sa);
        }
        st.server_runs.retain(|k, _| server_listed.contains(k));
        drop(st);
        // Live changes: checked against this node's caps for its share.
        for (run, controls) in follow {
            let version = controls.version;
            if let Err(problems) = run.follow_controls(controls) {
                tracing::warn!(
                    target: "rustybin::harness",
                    cluster = %self.cluster_id,
                    run = %run.id,
                    "harness: a live change over this node's caps is refused, its part stops: {}",
                    problems.join("; ")
                );
                run.stop("the leader's live change is over this node's caps");
            } else {
                tracing::debug!(target: "rustybin::harness", run = %run.id, version, "harness: following a live change");
            }
        }
        // A new share is checked against this node's caps like the first.
        for (run, share) in reshare {
            let problems = if share.valid() {
                run.scenario.validate_scaled(&h.caps(), share.fraction())
            } else {
                vec!["the share is not valid".into()]
            };
            if problems.is_empty() {
                run.set_share(share);
                tracing::info!(
                    target: "rustybin::harness",
                    cluster = %self.cluster_id,
                    run = %run.id,
                    share = share.fraction(),
                    "harness: the leader re-split the rate, this node's share changed"
                );
            } else {
                tracing::warn!(
                    target: "rustybin::harness",
                    cluster = %self.cluster_id,
                    run = %run.id,
                    "harness: a new share over this node's caps is refused, its part stops: {}",
                    problems.join("; ")
                );
                run.stop("the leader assigned a share over this node's caps");
            }
        }
        for (run_id, asg, lease, offset) in to_start {
            let me = self.clone();
            let h = h.clone();
            tokio::spawn(async move {
                let result = start_assignment(&h, &run_id, asg, lease, offset).await;
                let mut st = lock(&me.state);
                if let Some(cr) = st.client_runs.get_mut(&run_id) {
                    match result {
                        Ok(run) => cr.run = Some(run),
                        Err(e) => {
                            tracing::warn!(
                                target: "rustybin::harness",
                                cluster = %me.cluster_id,
                                run = %run_id,
                                "harness assignment refused: {e}"
                            );
                            cr.error = Some(e);
                        }
                    }
                }
            });
        }
    }

    fn serve(&self, h: &Harness, st: &mut AgentState, sa: ServerAssignment) {
        if !st.server_runs.contains_key(&sa.run_id) {
            if !h.is_server() || st.server_runs.len() >= MAX_AGENT_RUNS {
                return;
            }
            let Some(key) = RunKey::from_hex(&sa.key) else {
                return;
            };
            let entry = h.correlator.register(&sa.run_id, key, sa.profile.clone());
            st.server_runs.insert(
                sa.run_id.clone(),
                ServerRunState {
                    entry,
                    rejected_before: h.correlator.rejected(),
                    collected: None,
                },
            );
        }
        let Some(sr) = st.server_runs.get_mut(&sa.run_id) else {
            return;
        };
        if let (Some(collect), None) = (sa.collect, &sr.collected) {
            let stats = lock(&sr.entry.stats);
            let mut answer = Collected::default();
            for (node, encoded) in &collect {
                let seqs =
                    decode_numbers(encoded, super::cluster::MAX_COLLECT_TOTAL).unwrap_or_default();
                let mut seen = Vec::new();
                let mut unknown = Vec::new();
                for seq in seqs {
                    match stats.was_seen(node, seq) {
                        Some(true) => seen.push(seq),
                        Some(false) => {}
                        None => unknown.push(seq),
                    }
                }
                if !seen.is_empty() {
                    answer.seen.insert(node.clone(), encode_numbers(&seen));
                }
                if !unknown.is_empty() {
                    answer
                        .untracked
                        .insert(node.clone(), encode_numbers(&unknown));
                }
            }
            drop(stats);
            sr.collected = Some(answer);
        }
    }

    /// Whether a client run is active (or about to start) on this node.
    fn sending(&self) -> bool {
        lock(&self.state)
            .client_runs
            .values()
            .any(|c| c.error.is_none() && c.run.as_ref().is_none_or(|r| r.is_active()))
    }

    /// Follow `leader` (an announcement, or a node that names it): calls go
    /// to its control URL, and it gets every run's stats again.
    pub fn follow(&self, leader: Leader) {
        let mut st = lock(&self.state);
        if leader.term < st.leader.term
            || (leader.term == st.leader.term && leader.node_id == st.leader.node_id)
        {
            return;
        }
        if leader.node_id == self.node_id {
            return;
        }
        if let Some(url) = &leader.control_url {
            st.target = Target::Url(cluster_url_of(url, &self.cluster_id));
        }
        tracing::info!(
            target: "rustybin::harness",
            cluster = %self.cluster_id,
            leader = %leader.node_id,
            term = leader.term,
            "harness: following the leader of a newer term"
        );
        st.leader = leader;
        st.version = 0;
        st.clock.reset();
        st.next_stand = None;
        for cr in st.client_runs.values_mut() {
            cr.final_acked = false;
            cr.timeline_ack = 0;
        }
    }

    /// This node leads now (it won an election): calls stay in process.
    pub fn lead_locally(&self, leader: Leader) {
        let mut st = lock(&self.state);
        st.target = Target::Local;
        st.leader = leader;
        st.version = 0;
        st.clock.reset();
        st.last_ok = Some(Instant::now());
        st.failures = 0;
        for cr in st.client_runs.values_mut() {
            cr.final_acked = false;
            cr.timeline_ack = 0;
        }
    }

    /// A former leader that stepped down without knowing the new leader:
    /// its next calls go to the succession list.
    pub fn lose_leader(&self) {
        let mut st = lock(&self.state);
        st.last_ok = None;
    }

    /// `POST .../election/vote` on a member: grant when this node has lost
    /// the leader too, has not voted for someone else in the term, and the
    /// candidate's snapshot is at least as recent as its own.
    pub fn vote(&self, credential: &str, req: &VoteRequest) -> Result<VoteResponse, NodeError> {
        let mut st = lock(&self.state);
        let Some(snap) = st.snapshot.as_ref() else {
            return Err(NodeError::new(
                403,
                "this node holds no snapshot: it does not vote",
            ));
        };
        let Some(state) = snap.state.as_ref() else {
            return Err(NodeError::new(
                403,
                "this node is not eligible: it does not vote",
            ));
        };
        check_credential(state, &self.cluster_id, &req.candidate, credential)?;
        let refuse = |st: &AgentState, reason: &str| {
            Ok(VoteResponse {
                granted: false,
                term: st.leader.term,
                leader: Some(st.leader.clone()),
                reason: reason.into(),
            })
        };
        if !snap.eligible.contains(&req.candidate) {
            return refuse(&st, "the candidate is not eligible");
        }
        if req.term <= st.leader.term {
            return refuse(&st, "the candidate's term is not newer than this node's");
        }
        if let Some((t, c)) = &st.voted {
            if *t > req.term || (*t == req.term && c != &req.candidate) {
                return refuse(&st, "this node voted in that term already");
            }
        }
        let e = st.election_timeout;
        if st.last_ok.is_some_and(|t| t.elapsed() < e / 2) {
            return refuse(&st, "this node still hears the leader");
        }
        if req.snapshot < snap.version {
            return refuse(&st, "the candidate's snapshot is older than this node's");
        }
        st.voted = Some((req.term, req.candidate.clone()));
        // Give the candidate time to win and announce before standing.
        st.next_stand = Some(Instant::now() + e);
        Ok(VoteResponse {
            granted: true,
            term: req.term,
            leader: None,
            reason: "granted".into(),
        })
    }

    /// `POST .../election/elected` on a member: follow the new leader when
    /// its term is newer. Returns whether it did.
    pub fn elected(&self, credential: &str, req: &ElectedRequest) -> Result<bool, NodeError> {
        let newer = {
            let st = lock(&self.state);
            let Some(state) = st.snapshot.as_ref().and_then(|s| s.state.as_ref()) else {
                return Err(NodeError::new(
                    403,
                    "this node is not eligible: it holds no secrets to check the announcement",
                ));
            };
            check_credential(state, &self.cluster_id, &req.leader.node_id, credential)?;
            req.leader.term > st.leader.term
        };
        if newer {
            self.follow(req.leader.clone());
        }
        Ok(newer)
    }

    /// A heartbeat that reached this node, which does not lead: name the
    /// leader it follows, so a member that lost its leader finds the new one.
    pub fn not_leader(&self, node_id: &str, credential: &str) -> Result<NotLeader, NodeError> {
        let st = lock(&self.state);
        let Some(state) = st.snapshot.as_ref().and_then(|s| s.state.as_ref()) else {
            return Err(NodeError::new(404, "this node does not lead the cluster"));
        };
        check_credential(state, &self.cluster_id, node_id, credential)?;
        Ok(NotLeader {
            error: format!("this node does not lead cluster {}", self.cluster_id),
            leader: (st.leader.term > 0).then(|| st.leader.clone()),
        })
    }

    /// A join that reached this node, which does not lead: an eligible
    /// member checks the join token against the snapshot and names the
    /// leader, so a node can join through any eligible member.
    pub fn join_redirect(&self, token: &str) -> Result<NotLeader, NodeError> {
        let st = lock(&self.state);
        let Some(state) = st.snapshot.as_ref().and_then(|s| s.state.as_ref()) else {
            return Err(NodeError::new(404, "this node does not lead the cluster"));
        };
        let hash = super::cluster::sha256_hex(token);
        if !crate::control_auth::constant_time_eq(&hash, &state.secrets.join_hash) {
            return Err(NodeError::new(401, "invalid join token"));
        }
        Ok(NotLeader {
            error: format!("this node does not lead cluster {}", self.cluster_id),
            leader: (st.leader.term > 0).then(|| st.leader.clone()),
        })
    }

    /// Whether it is this node's turn to stand: eligible, holding the
    /// cluster state, the leader silent for the election timeout plus
    /// `k x E/6` for its position `k` in the succession.
    fn my_turn(&self, h: &Harness) -> bool {
        if !h.may_lead() {
            return false;
        }
        let st = lock(&self.state);
        let Some(snap) = st.snapshot.as_ref() else {
            return false;
        };
        if snap.state.is_none() || snap.leader.node_id == self.node_id {
            return false;
        }
        let Some(k) = snap
            .succession
            .iter()
            .position(|s| s.node_id == self.node_id)
        else {
            return false;
        };
        let e = st.election_timeout;
        let Some(last) = st.last_ok else {
            return false;
        };
        let at = last + e + e / 6 * k as u32;
        let now = Instant::now();
        now >= at && st.next_stand.is_none_or(|n| now >= n)
    }

    /// Stand for election: a newer term, a vote for itself, then votes from
    /// the other eligible nodes. With a majority this node leads from the
    /// snapshot. Returns whether it won.
    async fn stand(self: &Arc<Self>, h: &Arc<Harness>) -> bool {
        let (snap, term, credential, timeout) = {
            let mut st = lock(&self.state);
            let Some(snap) = st.snapshot.clone() else {
                return false;
            };
            let Some(credential) = st.credential.clone() else {
                return false;
            };
            let term = st
                .leader
                .term
                .max(st.voted.as_ref().map_or(0, |(t, _)| *t))
                .max(snap.version.term)
                + 1;
            st.voted = Some((term, self.node_id.clone()));
            let e = st.election_timeout;
            // The next try, if this one fails: after everyone else's turn.
            st.next_stand = Some(Instant::now() + e + e / 6 * (snap.succession.len() as u32 + 1));
            (snap, term, credential, (e / 2).min(Duration::from_secs(2)))
        };
        let Some(state) = snap.state.as_ref() else {
            return false;
        };
        tracing::info!(
            target: "rustybin::harness",
            cluster = %self.cluster_id,
            term,
            "harness: the leader is silent, this node stands for election"
        );
        let voters: Vec<String> = snap
            .eligible
            .iter()
            .filter(|n| **n != self.node_id)
            .filter_map(|n| {
                if *n == snap.leader.node_id {
                    snap.leader.control_url.clone()
                } else {
                    state
                        .members
                        .iter()
                        .find(|m| &m.node_id == n)
                        .and_then(|m| m.control_url.clone())
                }
            })
            .collect();
        let req = VoteRequest {
            term,
            candidate: self.node_id.clone(),
            snapshot: snap.version,
        };
        let calls = voters.iter().map(|url| {
            let url = format!("{}/election/vote", cluster_url_of(url, &self.cluster_id));
            let req = &req;
            let credential = &credential;
            async move {
                if super::safety::check_join_url(&url).await.is_err() {
                    return None;
                }
                post_json::<VoteResponse>(
                    &self.client,
                    h.bound_port(),
                    &url,
                    credential,
                    req,
                    timeout,
                )
                .await
                .ok()
            }
        });
        // Count the votes as they come: a silent voter (the old leader,
        // most likely) never holds a won election up.
        let mut pending: futures_util::stream::FuturesUnordered<_> = calls.collect();
        let mut granted = 1usize;
        while granted * 2 <= snap.eligible.len() {
            let Some(answer) = futures_util::StreamExt::next(&mut pending).await else {
                break;
            };
            let Some(a) = answer else { continue };
            if a.granted {
                granted += 1;
            } else if a.term > term {
                if let Some(l) = a.leader {
                    self.follow(l);
                }
                return false;
            } else if let Some(l) = a.leader.filter(|l| l.term >= term) {
                self.follow(l);
                return false;
            }
        }
        drop(pending);
        if granted * 2 <= snap.eligible.len() {
            tracing::info!(
                target: "rustybin::harness",
                cluster = %self.cluster_id,
                term,
                granted,
                eligible = snap.eligible.len(),
                "harness: no majority, this node gives way"
            );
            return false;
        }
        match super::election::promote(h, self, &snap, term, &credential).await {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(
                    target: "rustybin::harness",
                    cluster = %self.cluster_id,
                    term,
                    "harness: elected, but could not take over: {e}"
                );
                false
            }
        }
    }

    /// The leader is lost: ask the nodes of the succession list (all at
    /// once, each briefly) for the leader they know; follow one of a newer
    /// term. A node that leads it answers the heartbeat itself.
    async fn probe_next(self: &Arc<Self>, h: &Arc<Harness>) {
        let (urls, credential, term, timeout) = {
            let st = lock(&self.state);
            let Some(snap) = st.snapshot.as_ref() else {
                return;
            };
            let urls: Vec<String> = snap
                .succession
                .iter()
                .filter(|s| s.node_id != self.node_id && !s.control_url.is_empty())
                .map(|s| cluster_url_of(&s.control_url, &self.cluster_id))
                .collect();
            let Some(credential) = st.credential.clone() else {
                return;
            };
            let timeout =
                (st.election_timeout / 2).clamp(Duration::from_millis(200), Duration::from_secs(2));
            (urls, credential, st.leader.term, timeout)
        };
        if urls.is_empty() {
            return;
        }
        let mut req = self.heartbeat_request(h, false);
        req.wait_ms = 0;
        let t1 = req.t1;
        let calls = urls.into_iter().map(|url| {
            let req = &req;
            let credential = &credential;
            async move {
                if super::safety::check_join_url(&url).await.is_err() {
                    return (url, Err(LinkError::Other("forbidden address".into())));
                }
                let answer = post_json::<HeartbeatResponse>(
                    &self.client,
                    h.bound_port(),
                    &format!("{url}/nodes/{}/heartbeat", self.node_id),
                    credential,
                    req,
                    timeout,
                )
                .await;
                (url, answer)
            }
        });
        let mut pending: futures_util::stream::FuturesUnordered<_> = calls.collect();
        while let Some((url, answer)) = futures_util::StreamExt::next(&mut pending).await {
            match answer {
                // That node leads a newer term: follow it.
                Ok(resp) if resp.leader.term > term => {
                    lock(&self.state).target = Target::Url(url);
                    self.accept_answer(h, resp, t1);
                    return;
                }
                Err(LinkError::NotLeader(Some(l))) if l.term > term => {
                    self.follow(l);
                    return;
                }
                _ => {}
            }
        }
    }

    /// Leave: tell the leader (best effort), stop this node's runs of the
    /// cluster, end the agent.
    pub async fn leave(self: &Arc<Self>, h: &Arc<Harness>) {
        let _ = self.stop.send(true);
        let (credential, target) = {
            let st = lock(&self.state);
            (st.credential.clone(), st.target.clone())
        };
        if let Some(cred) = credential {
            let req = self.heartbeat_request(h, true);
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                self.call_heartbeat(h, &target, &cred, &req),
            )
            .await;
        }
        let runs: Vec<Arc<Run>> = lock(&self.state)
            .client_runs
            .values()
            .filter_map(|c| c.run.clone())
            .collect();
        for run in runs {
            if run.is_active() {
                run.stop("left the cluster");
            }
        }
        tracing::info!(target: "rustybin::harness", cluster = %self.cluster_id, node = %h.node_id, "harness: left the cluster");
    }

    /// Sleep `d`, or less when the agent is stopped.
    async fn pause(&self, d: Duration) {
        let mut rx = self.stop.subscribe();
        let _ = tokio::time::timeout(d, rx.wait_for(|v| *v)).await;
    }
}

/// Validate, pin and start this node's part of a cluster run.
async fn start_assignment(
    h: &Arc<Harness>,
    run_id: &str,
    asg: ClientAssignment,
    lease: Arc<Lease>,
    offset_ns: i64,
) -> Result<Arc<Run>, String> {
    if !asg.share.valid() {
        return Err("the assignment's share is not valid".into());
    }
    let key = RunKey::from_hex(&asg.key).ok_or("the assignment's run key is not valid")?;
    if asg.scenario.hash() != asg.hash {
        return Err("the scenario does not match its hash".into());
    }
    // The leader's clock to this node's: local = leader - offset.
    let local_ns = asg.start_at_ns as i128 - offset_ns as i128;
    let ahead = local_ns - now_ns() as i128;
    let start_at = Instant::now()
        + Duration::from_nanos(
            ahead.clamp(0, super::cluster::MAX_START_LEAD.as_nanos() as i128 * 2) as u64,
        );
    let options = RunOptions {
        id: Some(run_id.to_string()),
        key: Some(key),
        share: Share {
            from: asg.share.from,
            to: asg.share.to,
        },
        start_at: Some(start_at),
        lease: Some(lease),
        cluster: Some(asg.cluster_id.clone()),
        verification_token: Some(asg.verification_token.clone()),
        controls: asg.controls.clone(),
    };
    h.start_local_run(asg.scenario, options).await.map_err(|e| {
        if e.problems.is_empty() {
            e.error
        } else {
            format!("{}: {}", e.error, e.problems.join("; "))
        }
    })
}

/// The agent: join (retrying), then heartbeat until the node leaves. When
/// the leader goes silent it looks for a newer one, and stands for election
/// when it is this node's turn.
async fn agent(me: Arc<Membership>, h: Weak<Harness>) {
    loop {
        if me.is_stopped() {
            break;
        }
        let Some(harness) = h.upgrade() else { break };
        let (credential, target) = {
            let st = lock(&me.state);
            (st.credential.clone(), st.target.clone())
        };
        let Some(credential) = credential else {
            if let Err(e) = me.try_join(&harness).await {
                let failures = lock(&me.state).failures;
                tracing::warn!(
                    target: "rustybin::harness",
                    cluster = %me.cluster_id,
                    "harness: cannot join the cluster yet: {e}"
                );
                drop(harness);
                me.pause(backoff(failures)).await;
            }
            continue;
        };
        let req = me.heartbeat_request(&harness, false);
        let t1 = req.t1;
        let mut stop_rx = me.stop.subscribe();
        let result = tokio::select! {
            r = me.call_heartbeat(&harness, &target, &credential, &req) => Some(r),
            _ = stop_rx.wait_for(|v| *v) => None,
        };
        let Some(result) = result else { break };
        let answered = match result {
            Ok(resp) => me.accept_answer(&harness, resp, t1),
            Err(LinkError::NotLeader(Some(l))) if l.term > lock(&me.state).leader.term => {
                me.follow(l);
                continue;
            }
            Err(LinkError::Auth(e)) => {
                // A rejoin needs the join token and a leader that knows it;
                // without one, keep looking for the leader.
                let mut st = lock(&me.state);
                if !me.join_token.is_empty() && st.target != Target::Local {
                    tracing::warn!(target: "rustybin::harness", cluster = %me.cluster_id, "harness: the leader refused the node credential ({e}); joining again");
                    st.credential = None;
                }
                st.last_error = Some(e);
                st.failures += 1;
                false
            }
            Err(e) => {
                let failures = {
                    let mut st = lock(&me.state);
                    st.last_error = Some(e.text());
                    st.failures += 1;
                    st.failures
                };
                if failures == 1 || failures % 30 == 0 {
                    tracing::warn!(target: "rustybin::harness", cluster = %me.cluster_id, "harness: heartbeat failed: {}", e.text());
                }
                false
            }
        };
        if !answered {
            // The leader did not answer. Past the election timeout it is
            // lost: stand when it is this node's turn, else look for the
            // leader along the succession list.
            let lost = {
                let st = lock(&me.state);
                st.last_ok
                    .is_none_or(|t| t.elapsed() >= st.election_timeout)
            };
            if lost {
                if me.my_turn(&harness) && me.stand(&harness).await {
                    continue;
                }
                me.probe_next(&harness).await;
                drop(harness);
                me.pause(LOST_RETRY).await;
            } else {
                let failures = lock(&me.state).failures;
                drop(harness);
                me.pause(backoff(failures).min(LOST_RETRY * 2)).await;
            }
            continue;
        }
        // While load runs, report every heartbeat period; otherwise the
        // long poll does the waiting.
        if me.sending() {
            let every = lock(&me.state).heartbeat_every;
            drop(harness);
            me.pause(every).await;
        }
    }
}

fn backoff(failures: u32) -> Duration {
    (Duration::from_millis(200) * 2u32.saturating_pow(failures.min(5))).min(MAX_BACKOFF)
}

/// Join a cluster now (orchestrated, or the leader joining its own
/// cluster): the first join is synchronous so the caller learns whether it
/// worked; the agent then heartbeats in the background.
pub async fn join(
    h: &Arc<Harness>,
    target: JoinTarget,
    token: String,
    advertise: Option<String>,
) -> Result<Arc<Membership>, String> {
    if let Some(m) = h.membership() {
        return Err(format!(
            "this node is a member of cluster {} already: leave it first (POST {PREFIX}/leave)",
            m.cluster_id
        ));
    }
    let local = matches!(target, JoinTarget::Local(_));
    if !local && !h.is_client() && !h.is_server() && !h.may_lead() {
        return Err(
            "only a node with the client or server role, or one that may lead (RUSTYBIN_HARNESS_CONTROL_URL), joins a cluster"
                .into(),
        );
    }
    let me = Membership::new(h, target, token, advertise)?;
    me.try_join(h).await?;
    if !h.set_membership(me.clone()) {
        return Err("this node joined another cluster meanwhile".into());
    }
    tokio::spawn(agent(me.clone(), Arc::downgrade(h)));
    Ok(me)
}

/// Headless: join the cluster `RUSTYBIN_HARNESS_JOIN` names, retrying until
/// the leader answers.
pub fn spawn_from_config(h: &Arc<Harness>) -> Result<(), String> {
    let hc = &h.config;
    let (Some(url), Some(token)) = (hc.join.clone(), hc.join_token.clone()) else {
        return Ok(());
    };
    if !h.is_client() && !h.is_server() && !h.may_lead() {
        return Err(
            "RUSTYBIN_HARNESS_JOIN needs the client or server role, or a control URL to stand by as leader"
                .into(),
        );
    }
    let (url, _) = cluster_url(&url, hc.cluster.as_deref())?;
    let me = Membership::new(h, JoinTarget::Url(url), token, hc.advertise.clone())?;
    if !h.set_membership(me.clone()) {
        return Ok(());
    }
    tracing::info!(target: "rustybin::harness", cluster = %me.cluster_id, "harness: joining the cluster at startup");
    tokio::spawn(agent(me, Arc::downgrade(h)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_estimate_takes_the_shortest_round_trip() {
        let mut c = ClockEstimate::default();
        assert!(c.best().is_none());
        // Leader 5 ms ahead; 2 ms each way, 1 ms at the leader.
        let ms = 1_000_000u64;
        let t1 = 1_000 * ms;
        c.add(t1, t1 + 7 * ms, t1 + 8 * ms, t1 + 5 * ms);
        let b = c.best().unwrap();
        assert_eq!(b.offset_ns, 5 * ms as i64);
        assert_eq!(b.error_ns, 2 * ms);
        // A slower, asymmetric round is not preferred.
        c.add(t1, t1 + 30 * ms, t1 + 31 * ms, t1 + 40 * ms);
        assert_eq!(c.best().unwrap().offset_ns, 5 * ms as i64);
        // Bad stamps are ignored; the window is bounded.
        c.add(0, 1, 2, 3);
        for _ in 0..20 {
            c.add(t1, t1 + 9 * ms, t1 + 9 * ms, t1 + 10 * ms);
        }
        assert_eq!(c.samples.len(), CLOCK_SAMPLES);
        assert!(c.best().unwrap().offset_ns > 0);
        c.reset();
        assert!(c.best().is_none());
    }

    #[test]
    fn join_urls() {
        assert_eq!(
            cluster_url("http://c:8080/_rustybin/harness/clusters/perf-1/", None).unwrap(),
            (
                "http://c:8080/_rustybin/harness/clusters/perf-1".into(),
                "perf-1".into()
            )
        );
        assert_eq!(
            cluster_url("https://c", Some("x")).unwrap(),
            ("https://c/_rustybin/harness/clusters/x".into(), "x".into())
        );
        assert!(cluster_url("https://c", None).is_err());
        assert!(cluster_url("http://c/_rustybin/harness/clusters/a", Some("b")).is_err());
        assert!(cluster_url("http://c/_rustybin/harness/clusters/a/b", None).is_err());
        assert!(backoff(0) < backoff(3) && backoff(30) == MAX_BACKOFF);
        assert_eq!(
            cluster_url_of("http://n:1/", "c"),
            "http://n:1/_rustybin/harness/clusters/c"
        );
    }
}
