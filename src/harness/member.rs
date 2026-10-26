//! A node's membership in a cluster (`docs/plans/harness.md`, section 9):
//! the agent that joins the leader, heartbeats, keeps the clock estimate,
//! runs the client assignments it gets (its share of the rate, its virtual
//! clients, the synchronised start, under a lease) and, on a server node,
//! correlates the cluster's runs and answers the `lost` lookup.
//!
//! Joining happens from the environment at startup (`RUSTYBIN_HARNESS_JOIN`,
//! `RUSTYBIN_HARNESS_JOIN_TOKEN`, retried until the leader answers) or when
//! a controller calls `POST /_rustybin/harness/join`. A node is a member of
//! one cluster at a time. The node that leads a cluster and also has the
//! client or server role is a member of its own cluster, over an in-process
//! link instead of HTTP.
//!
//! Safety: every assignment is validated against this node's own caps and
//! pinned against its own allowlist (`RUSTYBIN_HARNESS_TARGETS`), so a
//! leader can never aim a node anywhere its operator did not allow. A run
//! stops when the leader has not answered for the lease period.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::watch;

use super::cluster::{NodeError, MAX_WAIT};
use super::correlate::{now_ns, RunEntry, RunKey};
use super::engine::{Run, Share};
use super::report::{decode_numbers, encode_numbers, WireHist};
use super::safety::Lease;
use super::wire::{
    Capacity, ClientAssignment, ClockReport, Collected, HeartbeatRequest, HeartbeatResponse,
    JoinRequest, JoinResponse, Leader, NodeStats, ServerAssignment, ServerStatsWire,
};
use super::{Harness, Role, RunOptions, HARNESS_API, PREFIX};

/// Clock samples kept (the one with the shortest round trip wins).
pub const CLOCK_SAMPLES: usize = 8;
/// Runs an agent tracks at once (client and server side each).
pub const MAX_AGENT_RUNS: usize = 8;
/// Longest pause between retries when the leader cannot be reached.
pub const MAX_BACKOFF: Duration = Duration::from_secs(5);
/// Time allowed for a join call.
pub const JOIN_TIMEOUT: Duration = Duration::from_secs(10);

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
}

/// How the agent reaches the leader.
enum Link {
    /// Over HTTP: the cluster URL (`<base>/_rustybin/harness/clusters/<id>`).
    Http {
        client: reqwest::Client,
        url: String,
    },
    /// In process: this node leads the cluster.
    Local { leader: Weak<Harness> },
}

#[derive(Debug)]
enum LinkError {
    /// The credential or token was refused: join again.
    Auth(String),
    /// No such cluster (any more).
    Gone(String),
    Other(String),
}

impl LinkError {
    fn text(&self) -> &str {
        match self {
            LinkError::Auth(s) | LinkError::Gone(s) | LinkError::Other(s) => s,
        }
    }
}

fn from_node_error(e: NodeError) -> LinkError {
    match e.status {
        401 => LinkError::Auth(e.error),
        404 => LinkError::Gone(e.error),
        _ => LinkError::Other(format!("{} ({})", e.error, e.status)),
    }
}

impl Link {
    async fn post<T: serde::de::DeserializeOwned>(
        client: &reqwest::Client,
        url: &str,
        bearer: &str,
        body: &impl serde::Serialize,
        timeout: Duration,
    ) -> Result<T, LinkError> {
        let resp = client
            .post(url)
            .bearer_auth(bearer)
            .timeout(timeout)
            .json(body)
            .send()
            .await
            .map_err(|e| LinkError::Other(format!("cannot reach the leader: {e}")))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| LinkError::Other(format!("the leader's answer broke off: {e}")))?;
        if status.is_success() {
            return serde_json::from_slice(&bytes)
                .map_err(|e| LinkError::Other(format!("the leader's answer does not parse: {e}")));
        }
        let message = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("the leader answered {status}"));
        Err(match status.as_u16() {
            401 | 403 => LinkError::Auth(message),
            404 => LinkError::Gone(message),
            _ => LinkError::Other(format!("{message} ({status})")),
        })
    }

    async fn join(
        &self,
        cluster_id: &str,
        token: &str,
        req: &JoinRequest,
    ) -> Result<JoinResponse, LinkError> {
        match self {
            Link::Http { client, url } => {
                Self::post(client, &format!("{url}/nodes"), token, req, JOIN_TIMEOUT).await
            }
            Link::Local { leader } => {
                let h = leader
                    .upgrade()
                    .ok_or_else(|| LinkError::Gone("the leader is gone".into()))?;
                let cluster = h
                    .coordinator
                    .get(cluster_id)
                    .ok_or_else(|| LinkError::Gone(format!("no cluster {cluster_id}")))?;
                cluster
                    .join(token, req.clone(), now_ns())
                    .map_err(from_node_error)
            }
        }
    }

    async fn heartbeat(
        &self,
        cluster_id: &str,
        node_id: &str,
        credential: &str,
        req: &HeartbeatRequest,
    ) -> Result<HeartbeatResponse, LinkError> {
        let timeout = Duration::from_millis(req.wait_ms) + Duration::from_secs(10);
        match self {
            Link::Http { client, url } => {
                Self::post(
                    client,
                    &format!("{url}/nodes/{node_id}/heartbeat"),
                    credential,
                    req,
                    timeout,
                )
                .await
            }
            Link::Local { leader } => {
                let cluster = {
                    let h = leader
                        .upgrade()
                        .ok_or_else(|| LinkError::Gone("the leader is gone".into()))?;
                    h.coordinator
                        .get(cluster_id)
                        .ok_or_else(|| LinkError::Gone(format!("no cluster {cluster_id}")))?
                };
                let t2 = now_ns();
                tokio::time::timeout(
                    timeout,
                    cluster.heartbeat(node_id, credential, req.clone(), t2),
                )
                .await
                .map_err(|_| LinkError::Other("heartbeat timed out".into()))?
                .map_err(from_node_error)
            }
        }
    }
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

struct AgentState {
    credential: Option<String>,
    lease: Duration,
    heartbeat_every: Duration,
    leader: Leader,
    verification_token: Option<String>,
    version: u64,
    last_ok: Option<Instant>,
    last_error: Option<String>,
    failures: u32,
    clock: ClockEstimate,
    client_runs: BTreeMap<String, ClientRunState>,
    server_runs: BTreeMap<String, ServerRunState>,
    kill_pending: bool,
}

/// This node's membership in a cluster.
pub struct Membership {
    pub cluster_id: String,
    /// The cluster URL; `None` when this node leads the cluster itself.
    pub url: Option<String>,
    link: Link,
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
        Some(id) if super::cluster::safe_id(id) => Ok((format!("{raw}{marker}{id}"), id.to_string())),
        _ => Err(format!(
            "the join URL must be <leader>{PREFIX}/clusters/<id>, or the leader's base URL with RUSTYBIN_HARNESS_CLUSTER"
        )),
    }
}

fn http_client() -> Result<reqwest::Client, String> {
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

impl Membership {
    fn new(
        h: &Arc<Harness>,
        target: JoinTarget,
        token: String,
        advertise: Option<String>,
    ) -> Result<Arc<Self>, String> {
        let (link, url, cluster_id) = match target {
            JoinTarget::Url(raw) => {
                let (url, id) = cluster_url(&raw, None)?;
                (
                    Link::Http {
                        client: http_client()?,
                        url: url.clone(),
                    },
                    Some(url),
                    id,
                )
            }
            JoinTarget::Local(id) => (
                Link::Local {
                    leader: Arc::downgrade(h),
                },
                None,
                id,
            ),
        };
        let (stop, _) = watch::channel(false);
        Ok(Arc::new(Self {
            cluster_id,
            url,
            link,
            join_token: token,
            advertise,
            joined_at: chrono::Utc::now(),
            state: Mutex::new(AgentState {
                credential: None,
                lease: super::cluster::DEFAULT_LEASE,
                heartbeat_every: Duration::from_secs(1),
                leader: Leader::default(),
                verification_token: None,
                version: 0,
                last_ok: None,
                last_error: None,
                failures: 0,
                clock: ClockEstimate::default(),
                client_runs: BTreeMap::new(),
                server_runs: BTreeMap::new(),
                kill_pending: false,
            }),
            stop,
        }))
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
            "url": self.url,
            "local": self.url.is_none(),
            "joined": st.credential.is_some(),
            "joined_at": self.joined_at.to_rfc3339(),
            "advertise": self.advertise,
            "leader": st.leader,
            "lease_ms": st.lease.as_millis() as u64,
            "last_answer_ms": st.last_ok.map(|t| t.elapsed().as_millis() as u64),
            "last_error": st.last_error,
            "clock_offset_ms": clock.map(|c| c.offset_ns as f64 / 1e6),
            "clock_error_ms": clock.map(|c| c.error_ns as f64 / 1e6),
            "client_runs": st.client_runs.keys().collect::<Vec<_>>(),
            "server_runs": st.server_runs.keys().collect::<Vec<_>>(),
        })
    }

    fn join_request(&self, h: &Harness) -> JoinRequest {
        let caps = h.caps();
        JoinRequest {
            node_id: h.node_id.clone(),
            roles: h
                .config
                .roles
                .iter()
                .copied()
                .filter(|r| matches!(r, Role::Client | Role::Server))
                .collect(),
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
        }
    }

    async fn try_join(&self, h: &Arc<Harness>) -> Result<(), String> {
        let req = self.join_request(h);
        let t1 = req.t1;
        match self
            .link
            .join(&self.cluster_id, &self.join_token, &req)
            .await
        {
            Ok(resp) => {
                let t4 = now_ns();
                let mut st = lock(&self.state);
                st.clock.add(t1, resp.t2, resp.t3, t4);
                st.credential = Some(resp.credential);
                st.lease = Duration::from_millis(resp.lease_ms);
                st.heartbeat_every = Duration::from_millis(resp.heartbeat_ms.max(50));
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
                    "harness: joined the cluster"
                );
                Ok(())
            }
            Err(e) => {
                let mut st = lock(&self.state);
                st.last_error = Some(e.text().to_string());
                st.failures += 1;
                Err(e.text().to_string())
            }
        }
    }

    /// The heartbeat request: stats of every run, the clock, the kill switch.
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
        HeartbeatRequest {
            t1: now_ns(),
            known_version: st.version,
            wait_ms: if active || leave || kill {
                0
            } else {
                MAX_WAIT.as_millis() as u64
            },
            clock: st.clock.best(),
            runs,
            servers,
            kill,
            leave,
            in_flight,
        }
    }

    /// Act on the leader's answer.
    fn process(self: &Arc<Self>, h: &Arc<Harness>, resp: HeartbeatResponse) {
        let mut to_start: Vec<(String, ClientAssignment, Arc<Lease>, i64)> = Vec::new();
        let mut st = lock(&self.state);
        st.version = resp.version;
        st.leader = resp.leader;
        st.lease = Duration::from_millis(resp.lease_ms.max(1));
        st.heartbeat_every = Duration::from_millis(resp.heartbeat_ms.max(50));
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
                    if rc.stop {
                        if let Some(run) = &cr.run {
                            if run.is_active() {
                                run.stop("leader");
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

    /// Leave: tell the leader (best effort), stop this node's runs of the
    /// cluster, end the agent.
    pub async fn leave(self: &Arc<Self>, h: &Arc<Harness>) {
        let _ = self.stop.send(true);
        let credential = lock(&self.state).credential.clone();
        if let Some(cred) = credential {
            let req = self.heartbeat_request(h, true);
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                self.link
                    .heartbeat(&self.cluster_id, &h.node_id, &cred, &req),
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
    };
    h.start_local_run(asg.scenario, options).await.map_err(|e| {
        if e.problems.is_empty() {
            e.error
        } else {
            format!("{}: {}", e.error, e.problems.join("; "))
        }
    })
}

/// The agent: join (retrying), then heartbeat until the node leaves.
async fn agent(me: Arc<Membership>, h: Weak<Harness>) {
    loop {
        if me.is_stopped() {
            break;
        }
        let Some(harness) = h.upgrade() else { break };
        let credential = lock(&me.state).credential.clone();
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
            r = me.link.heartbeat(&me.cluster_id, &harness.node_id, &credential, &req) => Some(r),
            _ = stop_rx.wait_for(|v| *v) => None,
        };
        let Some(result) = result else { break };
        match result {
            Ok(resp) => {
                let t4 = now_ns();
                {
                    let mut st = lock(&me.state);
                    st.clock.add(t1, resp.t2, resp.t3, t4);
                    st.last_ok = Some(Instant::now());
                    st.last_error = None;
                    st.failures = 0;
                }
                me.process(&harness, resp);
            }
            Err(LinkError::Auth(e)) => {
                tracing::warn!(target: "rustybin::harness", cluster = %me.cluster_id, "harness: the leader refused the node credential ({e}); joining again");
                let mut st = lock(&me.state);
                st.credential = None;
                st.last_error = Some(e);
            }
            Err(e) => {
                let failures = {
                    let mut st = lock(&me.state);
                    st.last_error = Some(e.text().to_string());
                    st.failures += 1;
                    st.failures
                };
                if failures == 1 || failures % 30 == 0 {
                    tracing::warn!(target: "rustybin::harness", cluster = %me.cluster_id, "harness: heartbeat failed: {}", e.text());
                }
                if matches!(e, LinkError::Gone(_)) {
                    lock(&me.state).credential = None;
                }
                drop(harness);
                me.pause(backoff(failures).min(Duration::from_millis(500)))
                    .await;
                continue;
            }
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
    if !h.is_client() && !h.is_server() {
        return Err("only a node with the client or server role joins a cluster".into());
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
    if !h.is_client() && !h.is_server() {
        return Err("RUSTYBIN_HARNESS_JOIN needs the client or server role".into());
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
    }
}
