//! Failover tests (plan section 15, phase 3b): in-process clusters whose
//! leader becomes unreachable mid-run ([`super::test_net`] cuts the cluster
//! protocol, the load keeps flowing). The least loaded eligible node takes
//! over within the lease and the clients never stop; a `prefer` standby
//! wins over busier nodes; the totals match what the nodes sent apart from
//! the reported `unreported` bound; the old leader steps down when it
//! returns; a minority partition stops sending; a two-node cluster stops.
//! Leases of 2 s and an election timeout of 600 ms keep the tests short.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};

use super::cluster_tests::{
    client_config, join, node_named, scenario, wait_for_alive, wait_for_cluster_report, Node, TOKEN,
};
use super::engine::Run;
use super::test_net;
use super::tests::{harness_config, send};
use super::wire::LeaderPolicy;
use super::{Harness, Role};
use crate::config::Config;

const LEASE: Duration = Duration::from_secs(2);

/// A node that may lead: its control URL is its listener's address.
fn eligible(mut c: Config, policy: LeaderPolicy) -> Config {
    c.harness.control_url = Some("auto".into());
    c.harness.leader = policy;
    c
}

fn coordinator() -> Config {
    eligible(
        harness_config(&[Role::Coordinator], &[]),
        LeaderPolicy::Auto,
    )
}

fn harness(n: &Node) -> Arc<Harness> {
    n.state.harness.clone().expect("harness mode")
}

/// The leader node, with a cluster of short lease and election timeout.
async fn lead(config: Config, name: &str, id: &str) -> (Node, String) {
    let l = node_named(config, name).await;
    let (status, cluster) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters",
        Some(json!({ "cluster_id": id, "join_token": TOKEN, "lease": "2s", "election_timeout": "600ms" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{cluster}");
    assert_eq!(cluster["term"], 1);
    let url = format!("{}/_rustybin/harness/clusters/{id}", l.base());
    (l, url)
}

/// Whether `n` leads cluster `id` now.
fn leads(n: &Node, id: &str) -> bool {
    harness(n)
        .coordinator
        .get(id)
        .is_some_and(|c| !c.is_stale())
}

/// The succession as `n` sees it (its leader's, or its snapshot's).
async fn succession(n: &Node, id: &str) -> Vec<String> {
    let (_, v) = send(
        &n.app,
        Method::GET,
        &format!("/_rustybin/harness/clusters/{id}/leader"),
        None,
    )
    .await;
    v["succession"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s["node_id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

async fn wait_until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + within;
    while Instant::now() < until {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(f(), "timed out after {within:?}: {what}");
}

/// Cut a node off the cluster protocol (group 1): calls across are refused.
fn isolate(n: &Node) {
    test_net::isolate(n.server.http_addr.port(), 1, test_net::Cut::Refuse);
}

/// The same, but calls across hang until their timeout (dropped packets).
fn black_hole(n: &Node) {
    test_net::isolate(n.server.http_addr.port(), 1, test_net::Cut::Drop);
}

fn heal(n: &Node) {
    test_net::heal(n.server.http_addr.port());
}

/// Submit and start a cluster run; returns its id.
async fn start(l: &Node, id: &str, sc: Value) -> String {
    let (status, run) = send(
        &l.app,
        Method::POST,
        &format!("/_rustybin/harness/clusters/{id}/runs"),
        Some(sc),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{run}");
    let run_id = run["run_id"].as_str().unwrap().to_string();
    let (status, started) = send(
        &l.app,
        Method::POST,
        &format!("/_rustybin/harness/clusters/{id}/runs/{run_id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    run_id
}

/// A node's local part of a cluster run, once it sends.
async fn local_run(n: &Node, run_id: &str) -> Arc<Run> {
    let h = harness(n);
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if let Some(r) = h.run(run_id).filter(|r| r.sent() > 0) {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{} never started its part of {run_id}", h.node_id);
}

#[tokio::test]
async fn losing_the_leader_the_least_loaded_eligible_node_takes_over_and_the_old_leader_steps_down()
{
    let id = "fo-1";
    let (l, url) = lead(coordinator(), "f1-leader", id).await;
    // c1 sends a quarter of the rate (50/s), s receives all of it (200/s):
    // c1 is the least loaded eligible node. c2 may not lead.
    let c1 = node_named(eligible(client_config(1000), LeaderPolicy::Auto), "f1-c1").await;
    let c2 = node_named(client_config(3000), "f1-c2").await;
    let s = node_named(
        eligible(harness_config(&[Role::Server], &[]), LeaderPolicy::Auto),
        "f1-s",
    )
    .await;
    for n in [&c1, &c2, &s] {
        join(n, &url).await;
    }
    wait_for_alive(&l, id, 2, 1).await;
    let mut sc = scenario(&s.base(), 200, "5s");
    sc["scale"] = json!({ "virtual_clients": 40, "client_nodes": 2, "server_nodes": 1 });
    let run_id = start(&l, id, sc).await;
    let c2_run = local_run(&c2, &run_id).await;
    // A checkpoint orders the succession by work, and the eligible nodes
    // hold it.
    let want = vec!["f1-c1".to_string(), "f1-s".to_string()];
    let until = Instant::now() + Duration::from_secs(6);
    while Instant::now() < until
        && (succession(&c1, id).await != want || succession(&s, id).await != want)
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(succession(&c1, id).await, want);
    assert_eq!(succession(&s, id).await, want);
    let (_, view) = send(
        &l.app,
        Method::GET,
        &format!("/_rustybin/harness/clusters/{id}/leader"),
        None,
    )
    .await;
    assert_eq!(view["failover"], "available", "{view}");

    // The leader becomes unreachable: its process stays, every call to it
    // hangs until its timeout (dropped packets, the slow way to fail).
    black_hole(&l);
    let cut = Instant::now();
    wait_until("c1 leads", LEASE, || leads(&c1, id)).await;
    let took = cut.elapsed();
    assert!(
        took < LEASE,
        "the failover took {took:?}, longer than the lease"
    );
    assert!(!leads(&s, id));
    // The other client never stopped: its lease never expired.
    assert!(c2_run.is_active(), "{:?}", c2_run.stop_reason());
    // c1 hands its share back: c2 carries the whole rate.
    wait_until("c2 carries the whole rate", Duration::from_secs(3), || {
        (c2_run.share().fraction() - 1.0).abs() < 1e-9
    })
    .await;
    // Every node names the new leader.
    let (_, v) = send(
        &s.app,
        Method::GET,
        &format!("/_rustybin/harness/clusters/{id}/leader"),
        None,
    )
    .await;
    assert_eq!(v["leader"]["node_id"], "f1-c1", "{v}");
    assert_eq!(v["term"], 2);

    // One report, from the new leader.
    let report = wait_for_cluster_report(&c1, id, &run_id).await;
    assert_eq!(report["state"], "finished", "{}", report["error"]);
    assert!(c2_run.stop_reason().is_none(), "{:?}", c2_run.stop_reason());
    let cluster = &report["cluster"];
    assert_eq!(cluster["term"], 2);
    assert_eq!(cluster["leader"], "f1-c1");
    assert_eq!(cluster["unreported"], 0, "{cluster}");
    assert_eq!(cluster["history"].as_array().unwrap().len(), 2, "{cluster}");
    let sent = report["totals"]["sent"].as_u64().unwrap();
    // Never more than planned: the new leader's part stops before the
    // others take its share (a dip of at most one heartbeat). Each client
    // node rounds its own share, which may add one request per node.
    assert!((950..=1002).contains(&sent), "sent {sent} of 1000 planned");
    // The totals are what the nodes sent.
    let local1 = harness(&c1).run(&run_id).expect("c1 part");
    assert_eq!(sent, local1.sent() + c2_run.sent());
    assert_eq!(report["server"]["arrivals"], sent);

    // The old leader comes back: it learns the newer term, steps down and
    // follows the new leader as a member.
    heal(&l);
    let lh = harness(&l);
    wait_until("the old leader steps down", Duration::from_secs(5), || {
        !leads(&l, id) && lh.membership().is_some_and(|m| m.leader().term == 2)
    })
    .await;
    let (status, redirect) = send(
        &l.app,
        Method::GET,
        &format!("/_rustybin/harness/clusters/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT, "{redirect}");
    assert!(redirect["location"]
        .as_str()
        .unwrap()
        .starts_with(&c1.base()));
    for n in [c1, c2, s, l] {
        n.stop().await;
    }
}

#[tokio::test]
async fn a_preferred_standby_wins_over_busier_nodes() {
    let id = "fo-2";
    let (l, url) = lead(coordinator(), "f2-leader", id).await;
    // An idle eligible node whose id sorts first, and a busy server that is
    // the preferred standby.
    let x = node_named(coordinator(), "aaa-f2-standby").await;
    let p = node_named(
        eligible(harness_config(&[Role::Server], &[]), LeaderPolicy::Prefer),
        "zzz-f2-prefer",
    )
    .await;
    let c = node_named(client_config(1000), "f2-client").await;
    for n in [&x, &p, &c] {
        join(n, &url).await;
    }
    wait_for_alive(&l, id, 1, 1).await;
    let mut sc = scenario(&p.base(), 100, "4s");
    sc["scale"] = json!({ "virtual_clients": 4, "client_nodes": 1, "server_nodes": 1 });
    let run_id = start(&l, id, sc).await;
    let c_run = local_run(&c, &run_id).await;
    let want = vec!["zzz-f2-prefer".to_string(), "aaa-f2-standby".to_string()];
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until && succession(&x, id).await != want {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(succession(&x, id).await, want);
    isolate(&l);
    wait_until("the preferred standby leads", LEASE, || leads(&p, id)).await;
    assert!(!leads(&x, id));
    assert!(c_run.is_active(), "{:?}", c_run.stop_reason());
    // A node joins through a member that does not lead: it is sent on to
    // the leader (a crashed leader restarted as a standby rejoins this way).
    let late = node_named(harness_config(&[Role::Server], &[]), "f2-late").await;
    join(
        &late,
        &format!("{}/_rustybin/harness/clusters/{id}", x.base()),
    )
    .await;
    let lh = harness(&late);
    wait_until(
        "the late node follows the new leader",
        Duration::from_secs(2),
        || {
            lh.membership()
                .is_some_and(|m| m.leader().node_id == "zzz-f2-prefer" && m.leader().term == 2)
        },
    )
    .await;
    // The new leader keeps its server role: the gateway's pool is unchanged.
    let (_, ups) = send(
        &p.app,
        Method::GET,
        &format!("/_rustybin/harness/clusters/{id}/upstreams"),
        None,
    )
    .await;
    assert_eq!(ups, json!([late.base(), p.base()]));
    heal(&l);
    for n in [late, c, p, x, l] {
        n.stop().await;
    }
}

#[tokio::test]
async fn the_old_leaders_own_load_is_reported_or_bounded() {
    let id = "fo-3";
    // The leader sends half of the load itself.
    let mut lc = eligible(
        harness_config(&[Role::Coordinator, Role::Client], &["127.0.0.1"]),
        LeaderPolicy::Auto,
    );
    lc.harness.max_rate = 1000;
    let (l, url) = lead(lc, "f3-leader", id).await;
    let a = node_named(eligible(client_config(1000), LeaderPolicy::Auto), "f3-a").await;
    let b = node_named(
        eligible(harness_config(&[Role::Server], &[]), LeaderPolicy::Auto),
        "f3-b",
    )
    .await;
    for n in [&a, &b] {
        join(n, &url).await;
    }
    wait_for_alive(&l, id, 2, 1).await;
    // Long enough for the three waits below on a slow machine (a lease
    // and a checkpoint, the election, two leases for the old part to be
    // surely gone and the re-split): the run must still be on when the
    // share moves.
    let mut sc = scenario(&b.base(), 200, "12s");
    sc["scale"] = json!({ "virtual_clients": 20, "client_nodes": 2, "server_nodes": 1 });
    let run_id = start(&l, id, sc).await;
    let l_run = local_run(&l, &run_id).await;
    let a_run = local_run(&a, &run_id).await;
    // After a checkpoint (every lease), cut the leader off.
    tokio::time::sleep(LEASE + Duration::from_millis(300)).await;
    isolate(&l);
    wait_until("a leads", LEASE, || leads(&a, id)).await;
    // The old leader cannot hear a majority: no renewal, its part stops at
    // the end of its lease. a keeps its share (its only peer is the old
    // leader), then takes the old leader's share once that lease surely
    // expired.
    wait_until("the old leader stops", LEASE * 2, || !l_run.is_active()).await;
    assert_eq!(l_run.stop_reason().as_deref(), Some("lease_expired"));
    // The share moves at the checkpoint after the old leader's part is
    // surely gone (its lease plus a heartbeat after its last stats, then
    // the ticker); a slow machine needs the full margin.
    wait_until("a carries the whole rate", LEASE * 4, || {
        (a_run.share().fraction() - 1.0).abs() < 1e-9
    })
    .await;
    let report = wait_for_cluster_report(&a, id, &run_id).await;
    let cluster = &report["cluster"];
    let old = cluster["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node_id"] == "f3-leader")
        .cloned()
        .unwrap();
    // What the old leader sent after its last checkpoint is not in the
    // totals, but within the reported bound.
    let reported = old["sent"].as_u64().unwrap();
    let actual = l_run.sent();
    let unreported = cluster["unreported"].as_u64().unwrap();
    assert!(actual >= reported, "{actual} < {reported}");
    assert!(
        actual - reported <= unreported,
        "{} unreported requests, over the bound of {unreported}",
        actual - reported
    );
    assert_eq!(old["unreported"].as_u64(), Some(unreported), "{old}");
    let total = report["totals"]["sent"].as_u64().unwrap();
    assert_eq!(total + (actual - reported), actual + a_run.sent());
    // A node without final stats fails the run, with the bound in the error.
    assert_eq!(report["state"], "failed");
    assert!(
        report["error"].as_str().unwrap().contains("unreported"),
        "{}",
        report["error"]
    );
    heal(&l);
    wait_until("the old leader steps down", Duration::from_secs(5), || {
        !leads(&l, id)
    })
    .await;
    for n in [a, b, l] {
        n.stop().await;
    }
}

#[tokio::test]
async fn a_minority_partition_stops_sending() {
    let id = "fo-4";
    let (l, url) = lead(coordinator(), "f4-leader", id).await;
    let a = node_named(eligible(client_config(1000), LeaderPolicy::Auto), "f4-a").await;
    let b = node_named(eligible(client_config(1000), LeaderPolicy::Auto), "f4-b").await;
    let c = node_named(client_config(1000), "f4-c").await;
    let s = node_named(harness_config(&[Role::Server], &[]), "f4-s").await;
    for n in [&a, &b, &c, &s] {
        join(n, &url).await;
    }
    wait_for_alive(&l, id, 3, 1).await;
    let mut sc = scenario(&s.base(), 150, "8s");
    sc["scale"] = json!({ "virtual_clients": 30, "client_nodes": 3, "server_nodes": 1 });
    let run_id = start(&l, id, sc).await;
    let a_run = local_run(&a, &run_id).await;
    let b_run = local_run(&b, &run_id).await;
    let c_run = local_run(&c, &run_id).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    // {leader, c} | {a, b, s}: the majority of the eligible nodes elects.
    isolate(&l);
    isolate(&c);
    wait_until("a or b leads", LEASE, || leads(&a, id) || leads(&b, id)).await;
    // The minority side stops at the end of its lease: its leader cannot
    // renew without a majority.
    wait_until("c stops", LEASE * 2, || !c_run.is_active()).await;
    assert_eq!(c_run.stop_reason().as_deref(), Some("lease_expired"));
    // The majority side carries on.
    let majority = [&a_run, &b_run];
    assert!(
        majority.iter().any(|r| r.is_active()),
        "{:?} {:?}",
        a_run.stop_reason(),
        b_run.stop_reason()
    );
    for r in majority {
        assert_ne!(r.stop_reason().as_deref(), Some("lease_expired"));
    }
    heal(&l);
    heal(&c);
    for n in [a, b, c, s, l] {
        n.stop().await;
    }
}

#[tokio::test]
async fn a_two_node_cluster_stops_on_leader_loss() {
    let id = "fo-5";
    let (l, url) = lead(coordinator(), "f5-leader", id).await;
    let a = node_named(eligible(client_config(1000), LeaderPolicy::Auto), "f5-a").await;
    join(&a, &url).await;
    wait_for_alive(&l, id, 1, 0).await;
    let mut sc = scenario(&a.base(), 50, "20s");
    sc["scale"] = json!({ "virtual_clients": 2, "client_nodes": 1 });
    let run_id = start(&l, id, sc).await;
    let a_run = local_run(&a, &run_id).await;
    // Two eligible nodes: a majority is both, so there is no failover.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (_, v) = send(
        &a.app,
        Method::GET,
        &format!("/_rustybin/harness/clusters/{id}/leader"),
        None,
    )
    .await;
    assert!(
        v["failover"].as_str().unwrap().starts_with("unavailable"),
        "{v}"
    );
    isolate(&l);
    wait_until("a stops", LEASE * 2, || !a_run.is_active()).await;
    assert_eq!(a_run.stop_reason().as_deref(), Some("lease_expired"));
    assert!(!leads(&a, id));
    heal(&l);
    for n in [a, l] {
        n.stop().await;
    }
}

#[tokio::test]
async fn live_changes_reach_every_node_and_survive_a_failover() {
    let id = "fo-6";
    let (l, url) = lead(coordinator(), "f6-leader", id).await;
    let a = node_named(eligible(client_config(1000), LeaderPolicy::Auto), "f6-a").await;
    let b = node_named(eligible(client_config(1000), LeaderPolicy::Auto), "f6-b").await;
    let s = node_named(harness_config(&[Role::Server], &[]), "f6-s").await;
    for n in [&a, &b, &s] {
        join(n, &url).await;
    }
    wait_for_alive(&l, id, 2, 1).await;
    let mut sc = scenario(&s.base(), 200, "8s");
    sc["scale"] = json!({ "virtual_clients": 20, "client_nodes": 2, "server_nodes": 1 });
    let run_id = start(&l, id, sc).await;
    let a_run = local_run(&a, &run_id).await;
    let b_run = local_run(&b, &run_id).await;
    // A change on the leader: both nodes follow on their next heartbeat.
    let (status, v) = send(
        &l.app,
        Method::PATCH,
        &format!("/_rustybin/harness/clusters/{id}/runs/{run_id}"),
        Some(json!({ "rate_multiplier": 0.5 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let changed = Instant::now();
    wait_until(
        "both nodes run at half the rate",
        Duration::from_secs(1),
        || a_run.controls().multiplier == 0.5 && b_run.controls().multiplier == 0.5,
    )
    .await;
    assert!(
        changed.elapsed() < Duration::from_millis(800),
        "{:?}",
        changed.elapsed()
    );
    // A change over a node's caps is refused (two nodes of 1000: x10 is
    // 2000 for 1000 each, fine; x10 on a 1000-a-second rate would not be).
    let (status, v) = send(
        &l.app,
        Method::PATCH,
        &format!("/_rustybin/harness/clusters/{id}/runs/{run_id}"),
        Some(json!({ "set": { "main.pattern.rate": 5000 } })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    // The leader goes: the change survives in the snapshot.
    isolate(&l);
    wait_until("a or b leads", LEASE, || leads(&a, id) || leads(&b, id)).await;
    let new_leader = if leads(&a, id) { &a } else { &b };
    let (_, run) = send(
        &new_leader.app,
        Method::GET,
        &format!("/_rustybin/harness/clusters/{id}/runs/{run_id}"),
        None,
    )
    .await;
    assert_eq!(run["layers"][0]["multiplier"], 1.0, "{run}");
    let c = harness(new_leader).coordinator.get(id).unwrap();
    let controls = c.run_controls(&c.run(&run_id).unwrap());
    assert_eq!(controls.multiplier, 0.5, "the change survived the failover");
    assert_eq!(controls.log.len(), 1);
    // And the new leader takes changes: the other node follows.
    let (status, v) = send(
        &new_leader.app,
        Method::PATCH,
        &format!("/_rustybin/harness/clusters/{id}/runs/{run_id}"),
        Some(json!({ "rate_multiplier": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let other = if std::ptr::eq(new_leader, &a) {
        &b_run
    } else {
        &a_run
    };
    wait_until(
        "the other node follows the new leader's change",
        Duration::from_secs(1),
        || other.controls().multiplier == 1.0,
    )
    .await;
    heal(&l);
    for n in [a, b, s, l] {
        n.stop().await;
    }
}

/// An `elected` announcement is followed only from an eligible node, at a
/// plausible term, from the URL it joined with.
#[tokio::test]
async fn a_forged_election_announcement_is_refused() {
    let id = "fo-forged";
    let (l, url) = lead(coordinator(), "f9-leader", id).await;
    let c = node_named(client_config(1000), "f9-c").await;
    let s = node_named(
        eligible(harness_config(&[Role::Server], &[]), LeaderPolicy::Auto),
        "f9-s",
    )
    .await;
    for n in [&c, &s] {
        join(n, &url).await;
    }
    wait_for_alive(&l, id, 1, 1).await;
    let until = Instant::now() + Duration::from_secs(6);
    while Instant::now() < until && succession(&s, id).await.is_empty() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let term = harness(&s).membership().expect("member").leader().term;
    let announce = |cred: String, node: &str, term: u64, control: String| {
        let base = s.base();
        let body = json!({ "leader": { "node_id": node, "term": term, "control_url": control } });
        async move {
            let resp = reqwest::Client::new()
                .post(format!(
                    "{base}/_rustybin/harness/clusters/{id}/election/elected"
                ))
                .bearer_auth(cred)
                .json(&body)
                .send()
                .await
                .expect("sent");
            resp.status().as_u16()
        }
    };
    // A client node (not eligible) with its own valid credential.
    let c_cred = harness(&c)
        .membership()
        .and_then(|m| m.credential_for_tests())
        .expect("credential");
    assert_eq!(announce(c_cred, "f9-c", term + 1, c.base()).await, 403);
    // An eligible node, with a term far ahead (u64::MAX once wrapped).
    let s_cred = harness(&s)
        .membership()
        .and_then(|m| m.credential_for_tests())
        .expect("credential");
    assert_eq!(announce(s_cred, "f9-s", u64::MAX, s.base()).await, 409);
    // The leader is unchanged.
    assert_eq!(
        harness(&s).membership().expect("member").leader().node_id,
        "f9-leader"
    );
    for n in [c, s, l] {
        n.stop().await;
    }
}

/// A member cannot depose the leader. A heartbeat that claims a newer term
/// makes the leader look for that term's leader (none answers, so it keeps
/// leading); an announcement to the leader is followed only from an
/// eligible node, at a plausible term, from the URL it joined with; and a
/// member checks a leader it is told to follow the same way.
#[tokio::test]
async fn a_member_cannot_depose_the_leader() {
    let id = "fo-depose";
    let (l, url) = lead(coordinator(), "f10-leader", id).await;
    let c = node_named(client_config(1000), "f10-c").await;
    let s = node_named(
        eligible(harness_config(&[Role::Server], &[]), LeaderPolicy::Auto),
        "f10-s",
    )
    .await;
    for n in [&c, &s] {
        join(n, &url).await;
    }
    wait_for_alive(&l, id, 1, 1).await;
    let until = Instant::now() + Duration::from_secs(6);
    while Instant::now() < until && succession(&s, id).await.is_empty() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let credential = |n: &Node| {
        harness(n)
            .membership()
            .and_then(|m| m.credential_for_tests())
            .expect("credential")
    };
    let (c_cred, s_cred) = (credential(&c), credential(&s));
    let post = |path: String, cred: String, body: Value| async move {
        let resp = reqwest::Client::new()
            .post(path)
            .bearer_auth(cred)
            .json(&body)
            .send()
            .await
            .expect("sent");
        resp.status().as_u16()
    };
    // A client node (not eligible) claims a newer term in its heartbeat:
    // it is told this leader is not its leader, and nothing else happens.
    let status = post(
        format!("{url}/nodes/f10-c/heartbeat"),
        c_cred.clone(),
        json!({ "t1": 0, "term": 5 }),
    )
    .await;
    assert_eq!(status, 409);
    // The client announces itself to the leader.
    let elected = format!("{url}/election/elected");
    let announce = |node: &str, term: u64, control: String| json!({ "leader": { "node_id": node, "term": term, "control_url": control } });
    assert_eq!(
        post(elected.clone(), c_cred, announce("f10-c", 2, c.base())).await,
        403
    );
    // An eligible node: from another URL than it joined with, then at a
    // term far ahead.
    assert_eq!(
        post(
            elected.clone(),
            s_cred.clone(),
            announce("f10-s", 2, "http://127.0.0.1:9".into())
        )
        .await,
        403
    );
    assert_eq!(
        post(elected, s_cred, announce("f10-s", u64::MAX, s.base())).await,
        409
    );
    // The leader looked for a newer leader and found none: it still leads.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(leads(&l, id), "a member deposed the leader");
    // A member follows only a leader that passes the same checks.
    let m = harness(&s).membership().expect("member");
    let term = m.leader().term;
    let leader = |node: &str, term: u64, control: String| super::wire::Leader {
        node_id: node.into(),
        term,
        control_url: Some(control),
        since: None,
    };
    assert!(!m.follow(leader("f10-c", term + 1, c.base())));
    assert!(!m.follow(leader("f10-leader", term + 1, "http://127.0.0.1:9".into())));
    assert!(!m.follow(leader("f10-leader", term + 100, l.base())));
    assert_eq!(m.leader().node_id, "f10-leader");
    assert_eq!(m.leader().term, term);
    for n in [c, s, l] {
        n.stop().await;
    }
}

/// A node counts its turn from its own last answer, and a node's answers
/// come a heartbeat apart (at most `E/3`), so two nodes' clocks differ by up
/// to that much: the slots of the line have to be wider, or a later node
/// stands first. `prefer` then means nothing, and the conformance run's
/// standby loses to a client now and then.
#[test]
fn the_line_holds_whatever_the_heartbeats_phase() {
    for ms in [200, 600, 2_000, 3_000, 60_000] {
        let e = std::time::Duration::from_millis(ms);
        let spread = e / 3;
        for k in 0..4 {
            let gap = super::member::stand_after(e, k + 1) - super::member::stand_after(e, k);
            assert!(gap > spread, "E {e:?}: position {k} is {gap:?} ahead");
        }
        assert_eq!(
            super::member::stand_after(e, 0),
            e,
            "the first in line waits E"
        );
    }
}

/// A leader asked to step aside before maintenance: the first in line takes
/// over by the ordinary election, the run goes on, and nothing is lost.
#[tokio::test]
async fn a_leader_steps_aside_and_the_line_takes_over_with_the_run_on() {
    let id = "fo-aside";
    let (l, url) = lead(coordinator(), "fa-leader", id).await;
    let standby = node_named(
        eligible(
            harness_config(&[Role::Coordinator], &[]),
            LeaderPolicy::Prefer,
        ),
        "fa-standby",
    )
    .await;
    let c1 = node_named(eligible(client_config(1000), LeaderPolicy::Auto), "fa-c1").await;
    let s = node_named(harness_config(&[Role::Server], &[]), "fa-s").await;
    for n in [&standby, &c1, &s] {
        join(n, &url).await;
    }
    wait_for_alive(&l, id, 1, 1).await;
    let mut sc = scenario(&s.base(), 100, "6s");
    sc["scale"] = json!({ "virtual_clients": 20, "client_nodes": 1, "server_nodes": 1 });
    let run_id = start(&l, id, sc).await;
    let c1_run = local_run(&c1, &run_id).await;
    let want = vec!["fa-standby".to_string(), "fa-c1".to_string()];
    let until = Instant::now() + Duration::from_secs(6);
    while Instant::now() < until && succession(&standby, id).await != want {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(succession(&standby, id).await, want);

    let path = format!("/_rustybin/harness/clusters/{id}/leader/step-aside");
    let (status, v) = send(&l.app, Method::POST, &path, None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["successor"], "fa-standby", "{v}");
    wait_until("the standby leads", LEASE, || leads(&standby, id)).await;
    wait_until("the old leader follows", LEASE, || !leads(&l, id)).await;
    assert!(!leads(&c1, id));
    assert!(c1_run.is_active(), "{:?}", c1_run.stop_reason());
    let report = wait_for_cluster_report(&standby, id, &run_id).await;
    assert_eq!(report["state"], "finished", "{report}");
    assert_eq!(report["cluster"]["leader"], "fa-standby", "{report}");
    assert_eq!(report["totals"]["lost"], 0, "{report}");
}

/// Nobody to take over: refused, and the leader keeps leading.
#[tokio::test]
async fn a_leader_with_no_line_stays() {
    let id = "fo-alone";
    let (l, url) = lead(coordinator(), "fb-leader", id).await;
    let c1 = node_named(eligible(client_config(1000), LeaderPolicy::Auto), "fb-c1").await;
    join(&c1, &url).await;
    let path = format!("/_rustybin/harness/clusters/{id}/leader/step-aside");
    let (status, v) = send(&l.app, Method::POST, &path, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert!(v["error"].as_str().expect("error").contains("three"), "{v}");
    assert!(leads(&l, id));
    // On a member: it does not lead, and says who does.
    let (status, v) = send(&c1.app, Method::POST, &path, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["leader"]["node_id"], "fb-leader", "{v}");
    let unknown = "/_rustybin/harness/clusters/nope/leader/step-aside";
    let (status, _) = send(&l.app, Method::POST, unknown, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Stepping aside ends by itself when nobody took over in time, so a leader
/// is never left refusing its members.
#[test]
fn stepping_aside_expires() {
    use super::cluster::aside_expired;
    let e = Duration::from_millis(600);
    let asked = Instant::now();
    assert!(!aside_expired(asked, e, asked + e * 3));
    assert!(aside_expired(asked, e, asked + e * 5));
}
