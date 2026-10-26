//! Leadership and failover (`docs/plans/harness.md`, section 9.1): the
//! ticker of a cluster this node leads, the promotion of a member that won
//! an election, and stepping down when a newer term exists.
//!
//! - [`lead`] advances the runs, checkpoints them into the snapshot, keeps
//!   the succession current and checks the quorum. A leader that does not
//!   hear a majority of the eligible nodes stops renewing leases (the
//!   heartbeat answers 503) and asks the eligible nodes, every half
//!   election timeout, whether a newer leader exists.
//! - [`promote`] rebuilds the cluster from the snapshot at the new term,
//!   hands this node's own client share back (`scale.leader_sheds_load`)
//!   and announces the term to the other eligible nodes.
//! - [`step_down`] drops a cluster this node no longer leads; its own
//!   membership follows the new leader as an ordinary member.
//!
//! The candidate and voter rules are in [`super::member`].

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use super::cluster::{Cluster, TICK};
use super::correlate::now_ns;
use super::member::{cluster_url_of, post_json, LinkError, Membership};
use super::wire::{ElectedRequest, HeartbeatRequest, HeartbeatResponse, Leader, Snapshot};
use super::Harness;

/// Run a cluster this node leads, until it no longer does.
pub async fn lead(h: Weak<Harness>, cluster: Weak<Cluster>) {
    let mut last_probe: Option<Instant> = None;
    let mut had_quorum = true;
    loop {
        tokio::time::sleep(TICK).await;
        let (Some(h), Some(c)) = (h.upgrade(), cluster.upgrade()) else {
            break;
        };
        if h.coordinator
            .get(&c.id)
            .is_none_or(|led| !Arc::ptr_eq(&led, &c))
        {
            break;
        }
        if c.is_stale() {
            let leader = probe(&h, &c).await;
            step_down(&h, &c, leader);
            break;
        }
        c.tick();
        hand_back(&h, &c);
        let quorum = c.has_quorum();
        if quorum != had_quorum {
            had_quorum = quorum;
            if quorum {
                tracing::info!(target: "rustybin::harness", cluster = %c.id, term = c.term, "harness: the leader hears a majority of the eligible nodes again");
            } else {
                tracing::warn!(target: "rustybin::harness", cluster = %c.id, term = c.term, "harness: the leader no longer hears a majority of the eligible nodes: no lease renewals until it does");
            }
        }
        if !quorum && last_probe.is_none_or(|t| t.elapsed() >= c.election_timeout / 2) {
            last_probe = Some(Instant::now());
            if let Some(l) = probe(&h, &c).await {
                step_down(&h, &c, Some(l));
                break;
            }
        }
    }
}

/// A new leader hands its own client share back once another client node
/// carries the run with it: its own part stops now, the others take the
/// share on their next heartbeat (a short dip, never more than planned).
fn hand_back(h: &Arc<Harness>, c: &Cluster) {
    c.shed_own_load(|run_id| {
        if let Some(run) = h.run(run_id).filter(|r| r.is_active()) {
            run.stop("handed its share back: this node leads the cluster now");
        }
    });
}

/// Start [`lead`] for a cluster this node leads.
pub fn spawn_lead(h: &Arc<Harness>, cluster: &Arc<Cluster>) {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(lead(Arc::downgrade(h), Arc::downgrade(cluster)));
    }
}

/// Ask the eligible nodes whether a newer leader exists: a node that leads
/// a newer term answers the heartbeat, a member names the leader it follows.
async fn probe(h: &Arc<Harness>, c: &Cluster) -> Option<Leader> {
    let credential = c.self_credential()?;
    let client = match h.membership() {
        Some(m) => m.client.clone(),
        None => super::member::http_client().ok()?,
    };
    let timeout = (c.election_timeout / 2).min(Duration::from_secs(2));
    for (_, control) in c.peer_urls() {
        let url = cluster_url_of(&control, &c.id);
        if super::safety::check_join_url(&url).await.is_err() {
            continue;
        }
        let req = HeartbeatRequest {
            t1: now_ns(),
            term: c.term,
            ..HeartbeatRequest::default()
        };
        let answer = post_json::<HeartbeatResponse>(
            &client,
            h.bound_port(),
            &format!("{url}/nodes/{}/heartbeat", c.leader),
            &credential,
            &req,
            timeout,
        )
        .await;
        match answer {
            Ok(resp) if resp.leader.term > c.term => {
                let mut l = resp.leader;
                if l.control_url.is_none() {
                    l.control_url = Some(control);
                }
                return Some(l);
            }
            Err(LinkError::NotLeader(Some(l))) if l.term > c.term => return Some(l),
            _ => {}
        }
    }
    None
}

/// Stop leading `c` (a newer term exists). This node stays a member and
/// follows `leader`, or looks for it along the succession list.
pub fn step_down(h: &Arc<Harness>, c: &Cluster, leader: Option<Leader>) {
    if h.coordinator.step_down(&c.id, c.term).is_none() {
        return;
    }
    tracing::warn!(
        target: "rustybin::harness",
        cluster = %c.id,
        term = c.term,
        new_leader = ?leader.as_ref().map(|l| (&l.node_id, l.term)),
        "harness: a newer term exists; this node steps down and follows the new leader"
    );
    if let Some(m) = h.membership().filter(|m| m.cluster_id == c.id) {
        match leader {
            Some(l) => m.follow(l),
            None => m.lose_leader(),
        }
    }
}

/// An election won: lead the cluster from the snapshot at `term`, hand
/// this node's own client share back, announce the term.
pub async fn promote(
    h: &Arc<Harness>,
    me: &Arc<Membership>,
    snap: &Snapshot,
    term: u64,
    credential: &str,
) -> Result<Arc<Cluster>, String> {
    let cluster = Cluster::from_snapshot(snap, &h.leader_info(), term)?;
    h.coordinator.insert(cluster.clone()).map_err(|e| e.error)?;
    me.lead_locally(cluster.leader_ref());
    spawn_lead(h, &cluster);
    tracing::warn!(
        target: "rustybin::harness",
        cluster = %cluster.id,
        term,
        previous = %snap.leader.node_id,
        "harness: elected leader; the runs resume from the snapshot"
    );
    let req = ElectedRequest {
        leader: cluster.leader_ref(),
    };
    let timeout = (cluster.election_timeout / 2).min(Duration::from_secs(2));
    let from = h.bound_port();
    for (_, control) in cluster.peer_urls() {
        let client = me.client.clone();
        let req = req.clone();
        let credential = credential.to_string();
        let url = format!("{}/election/elected", cluster_url_of(&control, &cluster.id));
        tokio::spawn(async move {
            if super::safety::check_join_url(&url).await.is_ok() {
                let _ =
                    post_json::<serde_json::Value>(&client, from, &url, &credential, &req, timeout)
                        .await;
            }
        });
    }
    Ok(cluster)
}
