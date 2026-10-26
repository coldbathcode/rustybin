//! Cluster tests (plan section 15, phase 3): several real servers in one
//! process form a cluster. Nodes join over HTTP, the rate is split by
//! declared capacity, a gateway (the test proxy) sits in front of the
//! server nodes, and the merged report must equal what the nodes and the
//! proxy counted. A lost leader expires the leases, so no client keeps
//! sending. Rates stay low to keep the suite fast.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use axum::Router;
use serde_json::{json, Value};

use super::test_proxy::{self, ProxyConfig};
use super::tests::{harness_config, running, send};
use super::Role;
use crate::config::Config;
use crate::state::AppState;

pub(super) const TOKEN: &str = "cluster-test-token-0123456789";

pub(super) fn client_config(max_rate: u64) -> Config {
    let mut c = harness_config(&[Role::Client], &["127.0.0.1"]);
    c.harness.max_rate = max_rate;
    c
}

/// A node of the test cluster.
pub(super) struct Node {
    pub(super) server: crate::RunningServer,
    pub(super) app: Router,
    pub(super) state: AppState,
}

/// Every node gets its own id (the instance id of a real deployment).
pub(super) async fn node(config: Config) -> Node {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    node_named(
        config,
        &format!("node-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
    )
    .await
}

/// A node with a chosen id.
pub(super) async fn node_named(mut config: Config, id: &str) -> Node {
    config.instance_id = id.to_string();
    let (server, app, state) = running(config).await;
    Node { server, app, state }
}

impl Node {
    pub(super) fn base(&self) -> String {
        format!("http://{}", self.server.http_addr)
    }

    pub(super) async fn stop(self) {
        self.server.shutdown();
        let _ = self.server.wait().await;
    }
}

/// The leader (coordinator role only) with a cluster created over the API.
async fn leader(id: &str, extra: Value) -> (Node, String) {
    let l = node(harness_config(&[Role::Coordinator], &[])).await;
    let mut body = json!({ "cluster_id": id, "join_token": TOKEN });
    if let (Value::Object(b), Value::Object(e)) = (&mut body, extra) {
        b.extend(e);
    }
    let (status, cluster) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters",
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{cluster}");
    assert_eq!(cluster["join_token"], TOKEN);
    let url = format!("{}/_rustybin/harness/clusters/{id}", l.base());
    (l, url)
}

pub(super) async fn join(n: &Node, url: &str) {
    let (status, v) = send(
        &n.app,
        Method::POST,
        "/_rustybin/harness/join",
        Some(json!({ "url": url, "join_token": TOKEN })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["joined"], true);
}

pub(super) async fn wait_for_alive(l: &Node, id: &str, clients: u64, servers: u64) -> Value {
    for _ in 0..200 {
        let (_, c) = send(
            &l.app,
            Method::GET,
            &format!("/_rustybin/harness/clusters/{id}"),
            None,
        )
        .await;
        if c["alive"]["clients"] == clients && c["alive"]["servers"] == servers {
            return c;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the cluster did not reach {clients} clients and {servers} servers");
}

pub(super) async fn wait_for_cluster_report(l: &Node, id: &str, run: &str) -> Value {
    let path = format!("/_rustybin/harness/clusters/{id}/runs/{run}/report");
    for _ in 0..800 {
        let (status, report) = send(&l.app, Method::GET, &path, None).await;
        if status == StatusCode::OK {
            return report;
        }
        assert_eq!(status, StatusCode::CONFLICT, "{report}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("cluster run {run} did not finish");
}

pub(super) fn scenario(target: &str, rate: u64, secs: &str) -> Value {
    json!({
        "name": "cluster",
        "target": target,
        "source": { "request": { "path": "/status/200" } },
        "pattern": { "type": "constant", "rate": rate, "for": secs },
        "scale": { "virtual_clients": 40, "client_nodes": 2, "server_nodes": 2 },
        "timeout": "1s"
    })
}

#[tokio::test]
async fn a_cluster_splits_the_rate_and_merges_one_report() {
    let (l, url) = leader("perf-1", json!({})).await;
    // Capacities 1000 and 3000: shares of a quarter and three quarters.
    let c1 = node(client_config(1000)).await;
    let c2 = node(client_config(3000)).await;
    let s1 = node(harness_config(&[Role::Server], &[])).await;
    let s2 = node(harness_config(&[Role::Server], &[])).await;
    // The gateway: every 5th request answered by itself, every 9th dropped.
    let proxy = test_proxy::start_pool(
        vec![s1.server.http_addr, s2.server.http_addr],
        ProxyConfig {
            own_answer_every: Some((5, 429)),
            drop_every: Some(9),
            ..ProxyConfig::default()
        },
    )
    .await;

    // A scenario submitted before the nodes joined waits for its quorum.
    let (status, run) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters/perf-1/runs",
        Some(scenario(&proxy.url(), 200, "1s")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{run}");
    assert_eq!(run["quorum"]["met"], false);
    let run_id = run["run_id"].as_str().unwrap().to_string();
    let start = format!("/_rustybin/harness/clusters/perf-1/runs/{run_id}/start");
    let (status, refused) = send(&l.app, Method::POST, &start, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(refused["error"]
        .as_str()
        .unwrap()
        .contains("waiting for nodes"));

    for n in [&c1, &c2, &s1, &s2] {
        join(n, &url).await;
    }
    let cluster = wait_for_alive(&l, "perf-1", 2, 2).await;
    // Four nodes, and the leader itself (a member of its own cluster).
    assert_eq!(cluster["members"].as_array().unwrap().len(), 5);
    // A wrong join token is refused; the node routes need no control token.
    let (status, _) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters/perf-1/nodes",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // The upstream export: the server nodes' URLs.
    let (_, ups) = send(
        &l.app,
        Method::GET,
        "/_rustybin/harness/clusters/perf-1/upstreams",
        None,
    )
    .await;
    let mut want = vec![s1.base(), s2.base()];
    want.sort();
    let mut got: Vec<String> = ups
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u.as_str().unwrap().to_string())
        .collect();
    got.sort();
    assert_eq!(got, want);
    // A member that is not the leader redirects controllers to it.
    let (status, redirect) = send(
        &c1.app,
        Method::GET,
        "/_rustybin/harness/clusters/perf-1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT, "{redirect}");
    assert_eq!(redirect["location"], url);
    // The node status names the cluster and its leader.
    let (_, st) = send(&c1.app, Method::GET, "/_rustybin/harness", None).await;
    assert_eq!(st["cluster"]["cluster_id"], "perf-1");
    assert_eq!(
        st["leader"]["node_id"],
        l.state.harness.as_ref().unwrap().node_id
    );

    let (status, started) = send(&l.app, Method::POST, &start, None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let report = wait_for_cluster_report(&l, "perf-1", &run_id).await;
    assert_eq!(report["state"], "finished", "{report}");

    // The merge equals what the nodes sent and what the proxy saw.
    let t = &report["totals"];
    let sent = t["sent"].as_u64().unwrap();
    assert!(
        (196..=204).contains(&sent),
        "sent {sent} (within 2 percent of 200)"
    );
    let nodes = report["cluster"]["nodes"].as_array().unwrap();
    let node_sent = |id: &str| {
        nodes
            .iter()
            .find(|n| n["node_id"] == id)
            .map(|n| n["sent"].as_u64().unwrap())
            .unwrap()
    };
    let id1 = c1.state.harness.as_ref().unwrap().node_id.clone();
    let id2 = c2.state.harness.as_ref().unwrap().node_id.clone();
    let (sent1, sent2) = (node_sent(&id1), node_sent(&id2));
    assert_eq!(sent1 + sent2, sent, "{nodes:?}");
    assert!(
        (45..=55).contains(&sent1) && (145..=155).contains(&sent2),
        "split {sent1} / {sent2}"
    );
    // Each client's local run used its share and its own virtual clients.
    let local1 = c1
        .state
        .harness
        .as_ref()
        .unwrap()
        .run(&run_id)
        .expect("c1 run");
    let local2 = c2
        .state
        .harness
        .as_ref()
        .unwrap()
        .run(&run_id)
        .expect("c2 run");
    assert!((local1.share().fraction() - 0.25).abs() < 1e-9);
    // Disjoint ranges of the 40 virtual clients, in node id order.
    let (vu1, vu2) = (local1.virtual_clients(), local2.virtual_clients());
    assert_eq!((vu1.1, vu2.1), (10, 30));
    let (first, second) = if id1 < id2 { (vu1, vu2) } else { (vu2, vu1) };
    assert_eq!(first.0, 0);
    assert_eq!(second.0, first.1);
    assert_eq!(local1.report().unwrap().totals.sent, sent1);
    assert_eq!(local2.report().unwrap().totals.sent, sent2);

    let own = proxy.counters.own_answers.load(Ordering::Relaxed);
    let dropped = proxy.counters.dropped.load(Ordering::Relaxed);
    let forwarded = proxy.counters.forwarded.load(Ordering::Relaxed);
    assert_eq!(t["gateway"], own, "{t}");
    assert_eq!(t["upstream"], forwarded, "{t}");
    assert_eq!(t["transport"], dropped, "{t}");
    // The server nodes saw none of the dropped requests: lost, exactly.
    assert_eq!(t["lost"], dropped, "{t}");
    assert_eq!(report["cluster"]["lost_check"], "complete");
    assert_eq!(report["server"]["arrivals"], forwarded);
    let arrivals: u64 = nodes.iter().filter_map(|n| n["arrivals"].as_u64()).sum();
    assert_eq!(arrivals, forwarded);
    // Histograms merge exactly: one latency sample per completed request.
    assert_eq!(report["latency"]["count"], t["completed"]);
    let (_, text) = send(
        &l.app,
        Method::GET,
        &format!("/_rustybin/harness/clusters/perf-1/runs/{run_id}/report?format=text"),
        None,
    )
    .await;
    assert!(text.as_str().unwrap().contains("perf-1"), "{text}");

    drop(proxy);
    for n in [c1, c2, s1, s2, l] {
        n.stop().await;
    }
}

#[tokio::test]
async fn losing_the_only_leader_expires_the_leases_and_no_client_keeps_sending() {
    let (l, url) = leader("perf-2", json!({ "lease": "1s" })).await;
    let c = node(client_config(1000)).await;
    let s = node(harness_config(&[Role::Server], &[])).await;
    join(&c, &url).await;
    join(&s, &url).await;
    wait_for_alive(&l, "perf-2", 1, 1).await;
    let mut sc = scenario(&s.base(), 50, "30s");
    sc["scale"] = json!({ "virtual_clients": 4, "client_nodes": 1, "server_nodes": 1 });
    let (status, run) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters/perf-2/runs",
        Some(sc),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{run}");
    let run_id = run["run_id"].as_str().unwrap().to_string();
    let (status, started) = send(
        &l.app,
        Method::POST,
        &format!("/_rustybin/harness/clusters/perf-2/runs/{run_id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let ch = c.state.harness.clone().unwrap();
    let local = {
        let mut found = None;
        for _ in 0..200 {
            if let Some(r) = ch.run(&run_id).filter(|r| r.sent() > 20) {
                found = Some(r);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        found.expect("the client started its part")
    };
    // The leader goes away; there is no successor in phase 3.
    let lost_at = Instant::now();
    l.stop().await;
    for _ in 0..200 {
        if !local.is_active() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        !local.is_active(),
        "the client still sends without a leader"
    );
    assert!(
        lost_at.elapsed() < Duration::from_secs(4),
        "stopped after {:?}",
        lost_at.elapsed()
    );
    assert_eq!(local.stop_reason().as_deref(), Some("lease_expired"));
    let after = local.sent();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        local.sent(),
        after,
        "nothing is sent after the lease expired"
    );
    assert!(after < 1000, "the run stopped long before its 30 s");
    c.stop().await;
    s.stop().await;
}

#[tokio::test]
async fn an_assignment_outside_the_nodes_own_allowlist_is_refused() {
    let (l, url) = leader("perf-3", json!({})).await;
    // This client allows only 10.9.9.9; the leader aims it at 127.0.0.1.
    let c = node(harness_config(&[Role::Client], &["10.9.9.9"])).await;
    join(&c, &url).await;
    wait_for_alive(&l, "perf-3", 1, 0).await;
    let mut sc = scenario(&l.base(), 10, "1s");
    sc["scale"] = json!({ "virtual_clients": 1, "client_nodes": 1 });
    let (_, run) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters/perf-3/runs",
        Some(sc),
    )
    .await;
    let run_id = run["run_id"].as_str().unwrap().to_string();
    let (status, _) = send(
        &l.app,
        Method::POST,
        &format!("/_rustybin/harness/clusters/perf-3/runs/{run_id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let report = wait_for_cluster_report(&l, "perf-3", &run_id).await;
    assert_eq!(report["totals"]["sent"], 0, "{report}");
    let node = &report["cluster"]["nodes"][0];
    assert_eq!(node["state"], "failed", "{node}");
    assert!(c.state.harness.as_ref().unwrap().run(&run_id).is_none());
    assert_ne!(report["exit_code"], 0);
    c.stop().await;
    l.stop().await;
}

#[tokio::test]
async fn headless_coordinator_starts_on_quorum() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A client and server in one node; it joins once the coordinator waits.
    let member = node(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let target = member.base();
    let path = dir.path().join("scenario.json");
    let mut sc = scenario(&target, 40, "1s");
    sc["scale"] = json!({ "virtual_clients": 2, "client_nodes": 1, "server_nodes": 1 });
    sc["thresholds"] = json!({ "error_rate": 0 });
    std::fs::write(&path, sc.to_string()).expect("scenario file");
    let report_path = dir.path().join("report.json");
    let mut lc = harness_config(&[Role::Coordinator], &[]);
    lc.harness.cluster = Some("perf-h".into());
    lc.harness.join_token = Some(TOKEN.into());
    lc.harness.scenario = Some(path);
    lc.harness.start = super::StartMode::OnQuorum;
    lc.harness.report = Some(report_path.display().to_string());
    let l = node(lc).await;
    let lh = l.state.harness.clone().unwrap();
    let headless = tokio::spawn(async move { super::run_headless(&lh).await });
    // The member joins after the coordinator is waiting.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !headless.is_finished(),
        "the coordinator waits for its quorum"
    );
    let mh = member.state.harness.clone().unwrap();
    let url = format!("{}/_rustybin/harness/clusters/perf-h", l.base());
    super::member::join(&mh, super::member::JoinTarget::Url(url), TOKEN.into(), None)
        .await
        .expect("join");
    let code = tokio::time::timeout(Duration::from_secs(20), headless)
        .await
        .expect("headless run ends")
        .expect("task");
    assert_eq!(code, 0);
    let report: Value =
        serde_json::from_slice(&std::fs::read(&report_path).expect("report")).unwrap();
    assert_eq!(report["cluster"]["cluster_id"], "perf-h");
    let sent = report["totals"]["sent"].as_u64().unwrap();
    assert!((39..=41).contains(&sent), "sent {sent}");
    assert_eq!(report["server"]["arrivals"], sent);
    member.stop().await;
    l.stop().await;
}

#[tokio::test]
async fn cluster_settings_from_the_environment() {
    use super::is_node_path;
    let post = Method::POST;
    assert!(is_node_path(&post, "/_rustybin/harness/clusters/a/nodes"));
    assert!(is_node_path(
        &post,
        "/_rustybin/harness/clusters/a/nodes/n1/heartbeat"
    ));
    assert!(!is_node_path(
        &Method::GET,
        "/_rustybin/harness/clusters/a/nodes"
    ));
    assert!(!is_node_path(&post, "/_rustybin/harness/clusters/a/runs"));
    assert!(!is_node_path(
        &post,
        "/_rustybin/harness/clusters/a/nodes/n1/heartbeat/x"
    ));
    // A coordinator naming a cluster needs a join token; so does a joiner.
    let mut c = harness_config(&[Role::Coordinator], &[]);
    c.harness.cluster = Some("x".into());
    assert!(super::check_startup(&c).unwrap_err().contains("JOIN_TOKEN"));
    let mut c = harness_config(&[Role::Server], &[]);
    c.harness.join = Some("http://leader/_rustybin/harness/clusters/x".into());
    assert!(super::check_startup(&c).unwrap_err().contains("JOIN_TOKEN"));
    c.harness.join_token = Some(TOKEN.into());
    assert!(super::check_startup(&c).is_ok());
    let mut c = harness_config(&[Role::Coordinator], &[]);
    c.harness.join = Some("http://leader/_rustybin/harness/clusters/x".into());
    c.harness.join_token = Some(TOKEN.into());
    assert!(super::check_startup(&c)
        .unwrap_err()
        .contains("client or server"));
    // The environment: invalid values warn and are ignored.
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    };
    let (cfg, warnings) = Config::from_lookup(env(&[
        ("RUSTYBIN_HARNESS", "on"),
        ("RUSTYBIN_HARNESS_CLUSTER", "perf-1"),
        ("RUSTYBIN_HARNESS_JOIN_TOKEN", TOKEN),
        ("RUSTYBIN_HARNESS_ADVERTISE", "http://s1:8080/"),
        ("RUSTYBIN_HARNESS_JOIN", "http://leader:8080"),
    ]));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(cfg.harness.cluster.as_deref(), Some("perf-1"));
    assert_eq!(cfg.harness.advertise.as_deref(), Some("http://s1:8080"));
    assert_eq!(cfg.harness.join.as_deref(), Some("http://leader:8080"));
    assert_eq!(cfg.harness.public_view()["join_token_configured"], true);
    assert!(!cfg.harness.public_view().to_string().contains(TOKEN));
    let (cfg, warnings) = Config::from_lookup(env(&[
        ("RUSTYBIN_HARNESS_CLUSTER", "no spaces please"),
        ("RUSTYBIN_HARNESS_JOIN_TOKEN", "short"),
        ("RUSTYBIN_HARNESS_ADVERTISE", "s1:8080"),
    ]));
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(
        cfg.harness.cluster.is_none()
            && cfg.harness.join_token.is_none()
            && cfg.harness.advertise.is_none()
    );
}
