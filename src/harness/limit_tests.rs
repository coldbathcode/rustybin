//! Phase 5 tests (plan section 15): the rate-limit tests against the test
//! proxy's reference limiters. `window_edge` finds the fixed window's double
//! dip, `distributed` the local counters' over-admission, `limit_probe`
//! measures the limit within 5 percent, `just_under` reports no false
//! positives on a correct limiter; and the analysis around them (headers,
//! recovery, upstream protection, forwarded addresses, retries, the rate
//! where enforcement starts, collateral damage). Limits are a few dozen
//! requests per second, runs a few seconds.

use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};

use super::test_proxy::{self, Algorithm, LimitKey, Limiter, ProxyConfig};
use super::tests::{harness_config, running, send, wait_for_report};
use super::Role;

/// A loop-through node behind proxies; the report of `scenario` run
/// through them (`targets` filled in).
async fn through(proxies: Vec<ProxyConfig>, mut scenario: Value) -> Value {
    let (server, app, _state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let mut started = Vec::new();
    for config in proxies {
        started.push(test_proxy::start(server.http_addr, config).await);
    }
    scenario["targets"] = started.iter().map(|p| json!(p.url())).collect();
    scenario["timeout"] = json!("2s");
    let (status, run) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(scenario),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    server.shutdown();
    let _ = server.wait().await;
    report
}

fn limited(limiter: Limiter) -> ProxyConfig {
    ProxyConfig {
        limiter: Some(Arc::new(limiter)),
        ..ProxyConfig::default()
    }
}

fn layer(name: &str, pattern: Value, expect: Value) -> Value {
    json!({
        "name": name,
        "source": { "request": { "path": "/status/200" } },
        "pattern": pattern,
        "expect": expect,
    })
}

/// The rate-limit analysis of layer `name`.
fn analysis<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["rate_limits"]
        .as_array()
        .and_then(|a| a.iter().find(|r| r["layer"] == name))
        .unwrap_or_else(|| panic!("no analysis of {name}: {}", report["rate_limits"]))
}

fn findings(r: &Value) -> String {
    r["findings"].to_string()
}

fn second() -> Duration {
    Duration::from_secs(1)
}

#[tokio::test]
async fn window_edge_finds_the_fixed_windows_double_dip() {
    let scenario = json!({
        "name": "edge",
        "layers": [layer(
            "heavy",
            json!({ "type": "limit_test", "test": "window_edge", "cycles": 2 }),
            json!({ "limit": 10, "window": "1s" }),
        )],
    });
    let (fixed, sliding) = tokio::join!(
        through(
            vec![limited(Limiter::new(Algorithm::FixedWindow, 10, second()))],
            scenario.clone()
        ),
        through(
            vec![limited(Limiter::new(
                Algorithm::SlidingWindow,
                10,
                second()
            ))],
            scenario
        ),
    );
    for r in [&fixed, &sliding] {
        assert_eq!(r["state"], "finished", "{}", r["error"]);
    }
    let f = analysis(&fixed, "heavy");
    // Both bursts of every pair got through: twice the limit within one
    // window length, though no single window went over.
    assert!(f["windows"]["sliding_max"].as_u64().unwrap() >= 18, "{f}");
    assert!(f["windows"]["admitted_max"].as_u64().unwrap() <= 10, "{f}");
    assert!(f["over_admission"].as_f64().unwrap() >= 0.8, "{f}");
    assert!(
        findings(f).contains("fixed-window double dip"),
        "{}",
        findings(f)
    );
    let s = analysis(&sliding, "heavy");
    assert!(s["windows"]["sliding_max"].as_u64().unwrap() <= 11, "{s}");
    assert!(s["over_admission"].as_f64().unwrap() <= 0.1, "{s}");
    assert!(!findings(s).contains("double dip"), "{}", findings(s));
}

#[tokio::test]
async fn distributed_finds_counters_local_to_each_gateway_node() {
    let scenario = json!({
        "name": "distributed",
        "layers": [layer(
            "heavy",
            json!({ "type": "limit_test", "test": "distributed", "factor": 4, "cycles": 2 }),
            json!({ "limit": 10, "window": "1s", "node_header": "via" }),
        )],
    });
    let node = |name: &str, limiter: Arc<Limiter>| ProxyConfig {
        limiter: Some(limiter),
        add_response_headers: vec![("via".into(), format!("1.1 {name}"))],
        ..ProxyConfig::default()
    };
    let shared = Limiter::new(Algorithm::FixedWindow, 10, second()).shared();
    let (local, together) = tokio::join!(
        through(
            vec![
                node(
                    "gw-a",
                    Limiter::new(Algorithm::FixedWindow, 10, second()).shared()
                ),
                node(
                    "gw-b",
                    Limiter::new(Algorithm::FixedWindow, 10, second()).shared()
                ),
            ],
            scenario.clone()
        ),
        through(
            vec![node("gw-a", shared.clone()), node("gw-b", shared)],
            scenario
        ),
    );
    let l = analysis(&local, "heavy");
    assert!(l["windows"]["admitted_max"].as_u64().unwrap() >= 18, "{l}");
    assert!(
        findings(l).contains("counters local to each gateway node"),
        "{}",
        findings(l)
    );
    let nodes = l["gateway_nodes"].as_array().unwrap();
    assert!(nodes.iter().any(|n| n["name"] == "1.1 gw-a"), "{nodes:?}");
    assert!(
        nodes
            .iter()
            .all(|n| n["worst_window"].as_u64().unwrap() <= 10),
        "{nodes:?}"
    );
    let t = analysis(&together, "heavy");
    assert!(t["windows"]["admitted_max"].as_u64().unwrap() <= 10, "{t}");
    assert!(!findings(t).contains("local to each"), "{}", findings(t));
}

#[tokio::test]
async fn limit_probe_measures_the_limit_within_five_percent() {
    let report = through(
        vec![limited(Limiter::new(Algorithm::SlidingWindow, 10, second()))],
        json!({
            "name": "probe",
            "layers": [layer(
                "probe",
                json!({ "type": "limit_test", "test": "limit_probe", "min": 1, "max": 20, "steps": 6, "every": "2s" }),
                json!({ "limit": 10, "window": "1s" }),
            )],
        }),
    )
    .await;
    assert_eq!(report["state"], "finished", "{}", report["error"]);
    let r = analysis(&report, "probe");
    let rate = r["measured_limit_rate"]
        .as_f64()
        .unwrap_or_else(|| panic!("{r}"));
    assert!(
        (rate - 10.0).abs() <= 0.5,
        "measured {rate}: {}",
        report["findings"]
    );
    assert!(
        findings(r).contains("limit_probe: the effective limit"),
        "{}",
        findings(r)
    );
    assert!(
        report["findings"][0]["kind"] == "limit_probe",
        "{}",
        report["findings"]
    );
}

#[tokio::test]
async fn just_under_reports_no_false_positives_on_a_correct_limiter() {
    let report = through(
        vec![limited(Limiter::new(
            Algorithm::SlidingWindow,
            20,
            second(),
        ))],
        json!({
            "name": "under",
            "layers": [{
                "name": "steady",
                "source": { "request": { "path": "/status/200" } },
                "pattern": { "type": "limit_test", "test": "just_under", "for": "3s" },
                "expect": { "limit": 20, "window": "1s" },
                "thresholds": { "rate_limited": 0 }
            }],
        }),
    )
    .await;
    let r = analysis(&report, "steady");
    assert_eq!(r["limited"], 0, "{r}");
    assert!(r["admitted"].as_u64().unwrap() >= 50, "{r}");
    assert!(
        findings(r).contains("no false positives"),
        "{}",
        findings(r)
    );
    assert_eq!(report["passed"], true, "{}", report["thresholds"]);
}

#[tokio::test]
async fn over_limit_reads_headers_recovery_and_upstream_protection() {
    let scenario = json!({
        "name": "over",
        "layers": [layer(
            "heavy",
            json!({ "type": "limit_test", "test": "over_limit", "factor": 3, "for": "2s", "recover": "2s" }),
            json!({ "limit": 10, "window": "1s" }),
        )],
    });
    let (shielding, leaking) = tokio::join!(
        through(
            vec![limited(
                Limiter::new(Algorithm::FixedWindow, 10, second()).with_headers()
            )],
            scenario.clone()
        ),
        through(
            vec![limited(
                Limiter::new(Algorithm::FixedWindow, 10, second())
                    .with_headers()
                    .forwarding_refused()
            )],
            scenario
        ),
    );
    let r = analysis(&shielding, "heavy");
    assert!(r["limited"].as_u64().unwrap() > 20, "{r}");
    assert!(r["first_limited_secs"].as_f64().unwrap() < 1.0, "{r}");
    // A window that turns over early in the run lets up to twice the limit
    // through before the first refusal.
    assert!(
        r["admitted_before_first_limited"].as_u64().unwrap() <= 20,
        "{r}"
    );
    let h = &r["headers"];
    assert_eq!(h["retry_after_on_limited"], 1.0, "{h}");
    assert_eq!(h["limit_matches"], true, "{h}");
    assert_eq!(h["counting_down"], true, "{h}");
    assert_eq!(h["remaining_on_limited"], 0, "{h}");
    assert!(r["recovery_secs"].as_f64().unwrap() <= 1.0, "{r}");
    assert_eq!(r["upstream"]["shielded"], true, "{}", r["upstream"]);
    // The flawed limiter forwards what it refuses.
    let l = analysis(&leaking, "heavy");
    assert_eq!(l["upstream"]["shielded"], false, "{}", l["upstream"]);
    assert!(
        l["upstream"]["reached"].as_u64().unwrap() > l["admitted"].as_u64().unwrap() + 10,
        "{}",
        l["upstream"]
    );
    assert!(
        findings(l).contains("upstream not shielded"),
        "{}",
        findings(l)
    );
}

#[tokio::test]
async fn many_ips_finds_a_limiter_that_trusts_forwarded_addresses() {
    let scenario = json!({
        "name": "ips",
        "layers": [layer(
            "rotating",
            json!({ "type": "limit_test", "test": "many_ips", "factor": 3, "identities": 10, "for": "2s" }),
            json!({ "limit": 10, "window": "1s", "per": "ip" }),
        )],
    });
    let (trusting, careful) = tokio::join!(
        through(
            vec![limited(
                Limiter::new(Algorithm::FixedWindow, 10, second()).keyed(LimitKey::Forwarded)
            )],
            scenario.clone()
        ),
        through(
            vec![limited(Limiter::new(Algorithm::FixedWindow, 10, second()))],
            scenario
        ),
    );
    let t = analysis(&trusting, "rotating");
    assert!(t["windows"]["admitted_max"].as_u64().unwrap() >= 25, "{t}");
    assert!(findings(t).contains("forwarded header"), "{}", findings(t));
    assert_eq!(t["identities_seen"], 10, "{t}");
    assert!(
        t["identities"][0]["name"]
            .as_str()
            .unwrap()
            .starts_with("198.18.0."),
        "{t}"
    );
    let c = analysis(&careful, "rotating");
    // The analysis's windows follow the run clock, the limiter's its own
    // first request, so a window of the analysis can straddle two of the
    // limiter's and count one admission from each side on a slow machine.
    assert!(c["windows"]["admitted_max"].as_u64().unwrap() <= 12, "{c}");
}

#[tokio::test]
async fn retry_wave_retries_refused_requests() {
    let scenario = |honour: bool| {
        json!({
            "name": "retries",
            "layers": [layer(
                "wave",
                json!({ "type": "limit_test", "test": "retry_wave", "factor": 2, "for": "2s", "retries": 2, "honour_retry_after": honour }),
                json!({ "limit": 10, "window": "1s" }),
            )],
        })
    };
    let proxy = || {
        vec![limited(
            Limiter::new(Algorithm::FixedWindow, 10, second()).with_headers(),
        )]
    };
    let (eager, patient) = tokio::join!(
        through(proxy(), scenario(false)),
        through(proxy(), scenario(true))
    );
    let e = analysis(&eager, "wave");
    let p = analysis(&patient, "wave");
    let sent = |r: &Value| {
        r["retries"]["admitted"].as_u64().unwrap_or(0)
            + r["retries"]["limited"].as_u64().unwrap_or(0)
    };
    // Every refusal is retried twice at once; waiting for Retry-After sends
    // fewer within the run.
    assert!(sent(e) >= e["limited"].as_u64().unwrap() / 2, "{e}");
    assert!(sent(p) < sent(e), "patient {} eager {}", sent(p), sent(e));
    assert!(findings(e).contains("retries:"), "{}", findings(e));
    // Retries are requests of the layer: sent counts them.
    assert!(
        eager["totals"]["sent"].as_u64().unwrap() > 40,
        "{}",
        eager["totals"]
    );
}

#[tokio::test]
async fn slow_rise_finds_where_enforcement_starts() {
    let report = through(
        vec![limited(Limiter::new(Algorithm::SlidingWindow, 10, second()))],
        json!({
            "name": "rise",
            "layers": [layer(
                "rise",
                json!({ "type": "limit_test", "test": "slow_rise", "from": 5, "to": 15, "over": "4s" }),
                json!({ "limit": 10, "window": "1s" }),
            )],
        }),
    )
    .await;
    let r = analysis(&report, "rise");
    let rate = r["rate_at_first_limited"]
        .as_f64()
        .unwrap_or_else(|| panic!("{r}"));
    // A sliding window refuses once the last second's average passes the
    // limit: about 1.25 per second after the rate does on this slope.
    assert!((9.5..=13.0).contains(&rate), "{rate}: {r}");
    assert!(
        findings(r).contains("enforcement started at about"),
        "{}",
        findings(r)
    );
}

#[tokio::test]
async fn collateral_damage_shows_in_the_other_layers() {
    let scenario = json!({
        "name": "collateral",
        "layers": [
            {
                "name": "heavy",
                "source": { "request": { "path": "/status/200", "headers": { "apikey": "heavy" } } },
                "pattern": { "type": "limit_test", "test": "over_limit", "factor": 3, "for": "2s" },
                "expect": { "limit": 10, "window": "1s" }
            },
            {
                "name": "customers",
                "source": { "request": { "path": "/status/200", "headers": { "apikey": "customer" } } },
                "pattern": { "type": "constant", "rate": 4, "for": "3s" },
                "thresholds": { "rate_limited": 0 }
            }
        ],
    });
    let (global, per_key) = tokio::join!(
        through(
            vec![limited(Limiter::new(Algorithm::FixedWindow, 10, second()))],
            scenario.clone()
        ),
        through(
            vec![limited(
                Limiter::new(Algorithm::FixedWindow, 10, second())
                    .keyed(LimitKey::Header("apikey".into()))
            )],
            scenario
        ),
    );
    let g = analysis(&global, "heavy");
    let c = &g["collateral"][0];
    assert_eq!(c["layer"], "customers", "{g}");
    assert!(c["limited_during"].as_u64().unwrap() > 0, "{c}");
    assert!(findings(g).contains("collateral damage"), "{}", findings(g));
    assert_eq!(global["passed"], false, "{}", global["thresholds"]);
    let t = global["thresholds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "layers.customers.rate_limited")
        .unwrap();
    assert_eq!(t["pass"], false, "{t}");
    // A per-key limiter leaves the customers alone.
    let k = analysis(&per_key, "heavy");
    assert_eq!(k["collateral"][0]["limited_during"], 0, "{k}");
    assert_eq!(per_key["passed"], true, "{}", per_key["thresholds"]);
    // The layer breakdown counts the refusals too.
    assert!(global["layers"][0]["counts"]["limited"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn every_test_runs_and_is_analysed() {
    let tests = [
        json!({ "type": "limit_test", "test": "concurrency", "size": 20, "every": "500ms", "cycles": 2 }),
        json!({ "type": "limit_test", "test": "many_keys", "factor": 2, "identities": 5, "for": "1s" }),
    ];
    let runs = futures_util::future::join_all(tests.iter().map(|p| {
        let mut l = layer(
            "t",
            p.clone(),
            json!({ "limit": 10, "window": "1s", "per": "global" }),
        );
        l["source"]["request"]["headers"] = json!({ "apikey": "key-{{id}}" });
        through(
            vec![limited(
                Limiter::new(Algorithm::TokenBucket, 10, second())
                    .keyed(LimitKey::Header("apikey".into())),
            )],
            json!({ "name": "each", "layers": [l] }),
        )
    }))
    .await;
    let concurrency = analysis(&runs[0], "t");
    assert_eq!(concurrency["tests"][0], "concurrency");
    assert!(
        concurrency["limited"].as_u64().unwrap() > 0,
        "{concurrency}"
    );
    // Five keys under a per-key limit add up past the global one.
    let keys = analysis(&runs[1], "t");
    assert_eq!(keys["identities_seen"], 5, "{keys}");
    assert!(
        findings(keys).contains("the global limit did not hold"),
        "{}",
        findings(keys)
    );
}

#[tokio::test]
async fn a_cluster_merges_the_rate_limit_counters_of_its_client_nodes() {
    use super::cluster_tests::{
        client_config, join, node, wait_for_alive, wait_for_cluster_report,
    };
    let l = node(harness_config(&[Role::Coordinator], &[])).await;
    let (status, cluster) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters",
        Some(json!({ "cluster_id": "limits-1", "join_token": super::cluster_tests::TOKEN })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{cluster}");
    let url = format!("{}/_rustybin/harness/clusters/limits-1", l.base());
    let c1 = node(client_config(1000)).await;
    let c2 = node(client_config(1000)).await;
    let s1 = node(harness_config(&[Role::Server], &[])).await;
    // Two gateway nodes, each with counters of its own.
    let gw = |name: &str| ProxyConfig {
        limiter: Some(Limiter::new(Algorithm::FixedWindow, 10, second()).shared()),
        add_response_headers: vec![("via".into(), name.into())],
        ..ProxyConfig::default()
    };
    let a = test_proxy::start(s1.server.http_addr, gw("gw-a")).await;
    let b = test_proxy::start(s1.server.http_addr, gw("gw-b")).await;
    for n in [&c1, &c2, &s1] {
        join(n, &url).await;
    }
    wait_for_alive(&l, "limits-1", 2, 1).await;
    let (status, run) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters/limits-1/runs",
        Some(json!({
            "name": "distributed",
            "targets": [a.url(), b.url()],
            "layers": [layer(
                "heavy",
                json!({ "type": "limit_test", "test": "distributed", "factor": 4, "cycles": 2 }),
                json!({ "limit": 10, "window": "1s", "node_header": "via" }),
            )],
            "scale": { "virtual_clients": 20, "client_nodes": 2, "server_nodes": 1 },
            "timeout": "2s"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{run}");
    let run_id = run["run_id"].as_str().unwrap().to_string();
    let (status, started) = send(
        &l.app,
        Method::POST,
        &format!("/_rustybin/harness/clusters/limits-1/runs/{run_id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let report = wait_for_cluster_report(&l, "limits-1", &run_id).await;
    assert_eq!(report["state"], "finished", "{}", report["error"]);
    let r = analysis(&report, "heavy");
    // Both client nodes' answers, merged: what the gateways admitted.
    let admitted = a
        .counters
        .forwarded
        .load(std::sync::atomic::Ordering::Relaxed)
        + b.counters
            .forwarded
            .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(r["admitted"].as_u64().unwrap(), admitted, "{r}");
    assert!(r["windows"]["admitted_max"].as_u64().unwrap() >= 18, "{r}");
    assert!(
        findings(r).contains("counters local to each gateway node"),
        "{}",
        findings(r)
    );
    // The server node saw the layer: the refused requests stayed away.
    assert_eq!(
        r["upstream"]["reached"].as_u64().unwrap(),
        admitted,
        "{}",
        r["upstream"]
    );
    assert_eq!(r["upstream"]["shielded"], true);
    for n in [l, c1, c2, s1] {
        n.stop().await;
    }
}
