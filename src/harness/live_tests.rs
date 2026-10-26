//! Phase 4 tests (plan section 15): every pattern followed second by
//! second, a burst that lands within its window at the gateway, a
//! breakpoint that stops at the gateway's capacity, live changes on a
//! running run and its event feed, `client_nodes: auto` with its scale
//! request, saved scenarios and the preview. Rates stay at a few hundred
//! per second for a few seconds.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use super::cluster_tests::{client_config, join, node_named, Node, TOKEN};
use super::pattern::Pattern;
use super::test_proxy::{self, ProxyConfig};
use super::tests::{harness_config, running, send, wait_for_report};
use super::Role;

/// A loop-through node (client and server).
fn both() -> crate::config::Config {
    harness_config(&[Role::Client, Role::Server], &["127.0.0.1"])
}

async fn start_run(app: &axum::Router, scenario: Value) -> String {
    let (status, run) = send(app, Method::POST, "/_rustybin/harness/runs", Some(scenario)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    run["run_id"].as_str().unwrap().to_string()
}

/// Run `pattern` on its own node against itself; the report.
async fn run_pattern(name: &str, pattern: Value) -> (Value, Pattern) {
    let (server, app, _state) = running(both()).await;
    let url = format!("http://{}", server.http_addr);
    let p: Pattern = serde_json::from_value(pattern.clone()).unwrap();
    let id = start_run(
        &app,
        json!({
            "name": name,
            "target": url,
            "source": { "request": { "path": "/status/200" } },
            "pattern": pattern,
            "timeout": "2s"
        }),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    server.shutdown();
    let _ = server.wait().await;
    (report, p)
}

#[tokio::test]
async fn every_pattern_is_followed_second_by_second() {
    // Rates stay low: the suite runs these next to every other test.
    let patterns = [
        (
            "constant",
            json!({ "type": "constant", "rate": 120, "for": "3s" }),
        ),
        (
            "ramp",
            json!({ "type": "ramp", "from": 30, "to": 150, "over": "3s" }),
        ),
        (
            "step",
            json!({ "type": "step", "start": 30, "step": 45, "every": "1s", "max": 120 }),
        ),
        (
            "spike",
            json!({ "type": "spike", "base": 30, "peak": 150, "rise": "1s", "hold": "1s", "fall": "1s", "recover": "1s" }),
        ),
        (
            "pulse",
            json!({ "type": "pulse", "high": 150, "low": 30, "on": "1s", "off": "1s", "cycles": 2 }),
        ),
        (
            "wave",
            json!({ "type": "wave", "min": 30, "max": 150, "period": "2s", "shape": "triangle", "for": "4s" }),
        ),
        (
            "burst",
            json!({ "type": "burst", "base": 30, "size": 60, "within": "100ms", "every": "1s", "for": "3s" }),
        ),
        (
            "target",
            json!({ "type": "target", "rate": 120, "within": "1s", "hold": "2s" }),
        ),
    ];
    let runs = futures_util::future::join_all(
        patterns
            .iter()
            .map(|(name, p)| run_pattern(name, p.clone())),
    )
    .await;
    for ((name, _), (report, pattern)) in patterns.iter().zip(runs) {
        assert_eq!(report["state"], "finished", "{name}: {}", report["error"]);
        assert_eq!(report["totals"]["not_sent"], 0, "{name}");
        let timeline = report["timeline"].as_array().unwrap();
        let full: Vec<&Value> = timeline
            .iter()
            .filter(|b| b["width_secs"].as_f64().unwrap() >= 0.999)
            .collect();
        assert!(full.len() >= 3, "{name}: {} full seconds", full.len());
        for b in full {
            let t = b["t_secs"].as_f64().unwrap();
            let expected = pattern
                .expected_between(Duration::from_secs_f64(t), Duration::from_secs_f64(t + 1.0));
            let got = b["requested"].as_f64().unwrap();
            // Within 2 percent, or 2 requests for the low seconds.
            let room = (expected * 0.02).max(2.0);
            assert!(
                (got - expected).abs() <= room,
                "{name} second {t}: {got} requests, the pattern says {expected:.1}"
            );
        }
        // The report shows the stages (and plateaus) the layer went through.
        if ["step", "spike", "pulse"].contains(name) {
            assert!(
                report["stages"].as_array().unwrap().len() >= 3,
                "{name}: {}",
                report["stages"]
            );
        }
    }
}

#[tokio::test]
async fn a_burst_lands_within_its_window_at_the_gateway() {
    let (server, app, _state) = running(both()).await;
    let proxy = test_proxy::start(server.http_addr, ProxyConfig::default()).await;
    let id = start_run(
        &app,
        json!({
            "name": "burst",
            "target": proxy.url(),
            "source": { "request": { "path": "/status/200" } },
            "pattern": { "type": "burst", "base": 0, "size": 200, "within": "100ms", "every": "1s", "for": "3s" },
            "timeout": "2s"
        }),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    assert_eq!(report["totals"]["sent"], 600, "{}", report["totals"]);
    let arrivals = proxy.counters.arrivals.lock().unwrap().clone();
    // Bursts are a second apart: split where the gap is long.
    let mut groups: Vec<Vec<Instant>> = Vec::new();
    for a in arrivals {
        match groups.last_mut() {
            Some(g) if a.duration_since(*g.last().unwrap()) < Duration::from_millis(400) => {
                g.push(a)
            }
            _ => groups.push(vec![a]),
        }
    }
    assert_eq!(groups.len(), 3, "three bursts");
    for g in &groups {
        assert_eq!(g.len(), 200, "a burst is 200 requests");
        let span = g.last().unwrap().duration_since(g[0]);
        assert!(
            span <= Duration::from_millis(100 + 60),
            "the burst took {span:?} to arrive, its window is 100ms"
        );
    }
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn a_breakpoint_stops_at_the_gateways_capacity() {
    let (server, app, _state) = running(both()).await;
    // The gateway answers 503 over 160 requests per second.
    let proxy = test_proxy::start(
        server.http_addr,
        ProxyConfig {
            capacity_per_sec: Some(160),
            ..ProxyConfig::default()
        },
    )
    .await;
    let started = Instant::now();
    let id = start_run(
        &app,
        json!({
            "name": "breakpoint",
            "target": proxy.url(),
            "source": { "request": { "path": "/status/200" } },
            "pattern": { "type": "breakpoint", "start": 50, "growth": 1, "every": "2s", "max": 1000, "until": { "error_rate": 0.02 } },
            "timeout": "2s"
        }),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    // Plateaus of 50, 100, then 200 per second: 200 is over the capacity.
    let f = &report["findings"][0];
    assert_eq!(f["kind"], "breakpoint", "{report:#}");
    assert_eq!(f["rate"], 100.0, "{f}");
    assert_eq!(f["failed_at"], 200.0, "{f}");
    assert!(report["stopped_by"]
        .as_str()
        .unwrap()
        .contains("breakpoint"));
    // It stopped well before its last plateau (1000 per second at 12 s).
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert!(
        proxy
            .counters
            .over_capacity
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
    );
    let text = {
        let (_, t) = send(
            &app,
            Method::GET,
            &format!("/_rustybin/harness/runs/{id}/report?format=text"),
            None,
        )
        .await;
        t.as_str().unwrap().to_string()
    };
    assert!(
        text.contains("Findings:") && text.contains("last good rate is 100.0"),
        "{text}"
    );
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn live_changes_apply_on_the_next_pass_and_the_feed_shows_them() {
    let (server, app, state) = running(both()).await;
    let url = format!("http://{}", server.http_addr);
    let id = start_run(
        &app,
        json!({
            "name": "live",
            "target": url,
            "layers": [
                { "name": "main", "source": { "request": { "path": "/status/200" } }, "pattern": { "type": "constant", "rate": 100, "for": "5s" } }
            ],
            "timeout": "2s"
        }),
    )
    .await;
    // The feed, read over HTTP like a controller.
    let feed_url = format!("{url}/_rustybin/harness/runs/{id}/events");
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = events.clone();
    let reader = tokio::spawn(async move {
        let mut resp = reqwest::Client::new().get(&feed_url).send().await.unwrap();
        let mut buf = String::new();
        while let Ok(Some(chunk)) = resp.chunk().await {
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(i) = buf.find("\n\n") {
                let event: String = buf[..i].to_string();
                buf.drain(..i + 2);
                if let Some(name) = event.lines().find_map(|l| l.strip_prefix("event: ")) {
                    seen.lock().unwrap().push(name.to_string());
                    if name == "finished" {
                        return;
                    }
                }
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let path = format!("/_rustybin/harness/runs/{id}");
    let patch = |body: Value| send(&app, Method::PATCH, &path, Some(body));
    let (status, v) = patch(json!({ "rate_multiplier": 2 })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["controls"]["multiplier"], 2.0);
    // Refused: over the multiplier's range, and an unknown layer.
    let (status, v) = patch(json!({ "rate_multiplier": 11 })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let (status, _) = patch(json!({ "layer": "nope", "pause": true })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // A second layer joins.
    let (status, v) = patch(json!({ "add_layer": { "name": "late", "source": { "request": { "path": "/status/201" } }, "pattern": { "type": "constant", "rate": 20, "for": "2s" } } })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let report = wait_for_report(&app, &id).await;
    let changes = report["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert_eq!(changes[0]["what"], "rate x2");
    assert!(report["layers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["name"] == "late"));
    assert!(report["totals"]["statuses"]["201"].as_u64().unwrap() >= 30);
    // Twice the rate from the next second on, and the new layer's 20 a
    // second, doubled too (the multiplier is the run's).
    let timeline = report["timeline"].as_array().unwrap();
    let at = |t: usize| timeline[t]["requested"].as_f64().unwrap();
    assert!((95.0..=105.0).contains(&at(0)), "{}", at(0));
    assert!((235.0..=245.0).contains(&at(2)), "{}", at(2));
    // A finished run takes no change.
    let (status, _) = patch(json!({ "rate_multiplier": 1 })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    tokio::time::timeout(Duration::from_secs(5), reader)
        .await
        .expect("the feed ends with the run")
        .unwrap();
    let names = events.lock().unwrap().clone();
    assert!(names.contains(&"snapshot".to_string()), "{names:?}");
    assert!(names.contains(&"change".to_string()), "{names:?}");
    assert_eq!(names.last().map(String::as_str), Some("finished"));
    drop(state);
    server.shutdown();
    let _ = server.wait().await;
}

/// A hold stretches the plateau it holds, in the report too: the stages are
/// noted as the run goes, not worked out from the controls at the end.
#[tokio::test]
async fn a_hold_stretches_its_plateau_in_the_report() {
    let (server, app, state) = running(both()).await;
    let url = format!("http://{}", server.http_addr);
    // Plateaus 20, 40, 60 of 1 s each.
    let id = start_run(
        &app,
        json!({
            "name": "held",
            "target": url,
            "layers": [
                { "name": "main", "source": { "request": { "path": "/status/200" } }, "pattern": { "type": "step", "start": 20, "step": 20, "every": "1s", "max": 60 } }
            ],
            "timeout": "2s"
        }),
    )
    .await;
    let path = format!("/_rustybin/harness/runs/{id}");
    // Held 1.5 s to 2.5 s into the run: plateau 1 lasts 2 s.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let (status, v) = send(&app, Method::PATCH, &path, Some(json!({ "hold": true }))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let (status, v) = send(&app, Method::PATCH, &path, Some(json!({ "hold": false }))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let report = wait_for_report(&app, &id).await;
    let stages: Vec<(u64, f64, f64, f64)> = report["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["part"].as_u64().unwrap(),
                s["target"].as_f64().unwrap(),
                s["from_secs"].as_f64().unwrap(),
                s["to_secs"].as_f64().unwrap(),
            )
        })
        .collect();
    assert_eq!(stages.len(), 3, "{stages:?}");
    assert_eq!(
        (stages[0].0, stages[0].1, stages[0].2, stages[0].3),
        (0, 20.0, 0.0, 1.0)
    );
    assert_eq!(
        (stages[1].0, stages[1].1, stages[1].2, stages[1].3),
        (1, 40.0, 1.0, 3.0)
    );
    assert_eq!((stages[2].0, stages[2].1, stages[2].2), (2, 60.0, 3.0));
    drop(state);
    server.shutdown();
    let _ = server.wait().await;
}

/// A controller hook that keeps what it gets, with whether the signature
/// checks out (HMAC-SHA256 keyed with the join token's SHA-256 hex).
async fn hook_server() -> (String, Arc<Mutex<Vec<(bool, Value)>>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let keep = got.clone();
    // Any path (a fallback: the catalogue test reads every route literal).
    let app = axum::Router::new().fallback(
        move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
            let keep = keep.clone();
            async move {
                let sig = headers
                    .get("x-rustybin-signature")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                let key = super::cluster::sha256_hex(TOKEN);
                let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
                mac.update(&body);
                let expected: String = mac
                    .finalize()
                    .into_bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                let valid = sig == format!("sha256={expected}");
                let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                keep.lock().unwrap().push((valid, v));
                StatusCode::ACCEPTED
            }
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/scale"), got)
}

#[tokio::test]
async fn client_nodes_auto_waits_asks_for_nodes_and_starts() {
    let (hook, got) = hook_server().await;
    let mut lc = harness_config(&[Role::Coordinator], &["127.0.0.1"]);
    lc.harness.scale_hook = Some(hook);
    let l: Node = node_named(lc, "auto-leader").await;
    let (status, cluster) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters",
        Some(json!({ "cluster_id": "auto", "join_token": TOKEN })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{cluster}");
    let url = format!("{}/_rustybin/harness/clusters/auto", l.base());
    let c1 = node_named(client_config(100), "auto-c1").await;
    join(&c1, &url).await;
    let target = c1.base();
    let scenario = |rate: u64, extra: Value| {
        let mut s = json!({
            "name": "auto",
            "target": target,
            "source": { "request": { "path": "/status/200" } },
            "pattern": { "type": "constant", "rate": rate, "for": "2s" },
            "scale": { "virtual_clients": 30, "client_nodes": "auto" },
            "timeout": "2s"
        });
        if let (Value::Object(sc), Value::Object(e)) = (&mut s["scale"], extra) {
            sc.extend(e);
        }
        s
    };
    // 300 per second needs three nodes of 100.
    let (status, run) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters/auto/runs",
        Some(scenario(300, json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    let (status, started) = send(
        &l.app,
        Method::POST,
        &format!("/_rustybin/harness/clusters/auto/runs/{id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    assert_eq!(started["state"], "waiting", "{started}");
    let until = Instant::now() + Duration::from_secs(3);
    while got.lock().unwrap().is_empty() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    {
        let got = got.lock().unwrap();
        let (valid, event) = got.first().expect("the hook got the scale request");
        assert_eq!(event["event"], "scale_request");
        assert_eq!(event["needed_client_nodes"], 3, "{event}");
        assert!(valid, "the signature checks out with the join token");
    }
    // Two more nodes join: the run starts on its own.
    let c2 = node_named(client_config(100), "auto-c2").await;
    let c3 = node_named(client_config(100), "auto-c3").await;
    join(&c2, &url).await;
    join(&c3, &url).await;
    let mut state = String::new();
    for _ in 0..100 {
        let (_, v) = send(
            &l.app,
            Method::GET,
            &format!("/_rustybin/harness/clusters/auto/runs/{id}"),
            None,
        )
        .await;
        state = v["state"].as_str().unwrap_or_default().to_string();
        if state != "waiting" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(state == "starting" || state == "running", "{state}");
    let report = super::cluster_tests::wait_for_cluster_report(&l, "auto", &id).await;
    let sent = report["totals"]["sent"].as_u64().unwrap();
    assert!((588..=606).contains(&sent), "sent {sent} of 600");
    // 500 per second needs five: after the scale timeout the run starts
    // undersized, at the three nodes' 300, with a warning.
    let (_, run) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters/auto/runs",
        Some(scenario(500, json!({ "scale_timeout": "1s" }))),
    )
    .await;
    let id = run["run_id"].as_str().unwrap().to_string();
    let (_, started) = send(
        &l.app,
        Method::POST,
        &format!("/_rustybin/harness/clusters/auto/runs/{id}/start"),
        None,
    )
    .await;
    assert_eq!(started["state"], "waiting");
    let report = super::cluster_tests::wait_for_cluster_report(&l, "auto", &id).await;
    let warnings = report["cluster"]["warnings"].as_array().unwrap();
    assert!(
        warnings[0].as_str().unwrap().contains("undersized"),
        "{warnings:?}"
    );
    let sent = report["totals"]["sent"].as_u64().unwrap();
    assert!(
        (588..=606).contains(&sent),
        "sent {sent}: the three nodes' 300 a second"
    );
    for n in [c1, c2, c3, l] {
        n.stop().await;
    }
}

#[tokio::test]
async fn saved_scenarios_and_the_preview() {
    let state = crate::test_support::test_state_with(both());
    let app = crate::test_support::test_app_with(state);
    let burst = json!({
        "name": "my burst",
        "target": "http://127.0.0.1:9",
        "source": { "request": { "path": "/" } },
        "pattern": { "type": "burst", "base": 50, "size": 100, "within": "100ms", "every": "1s", "for": "3s" }
    });
    // The preview: the burst shows in its second, from the exact integral.
    let (status, p) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/preview",
        Some(burst.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{p}");
    assert_eq!(p["valid"], true);
    assert_eq!(p["points"], 3);
    assert_eq!(p["layers"][0]["series"], json!([150.0, 150.0, 150.0]));
    assert_eq!(p["total"], json!([150.0, 150.0, 150.0]));
    assert_eq!(p["layers"][0]["expected_requests"], 450.0);
    assert_eq!(p["layers"][0]["peak"], 1050.0);
    // Saved by name, listed next to the built-ins, read back, deleted.
    let (status, v) = send(
        &app,
        Method::PUT,
        "/_rustybin/harness/scenarios/my-burst",
        Some(burst.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let (status, _) = send(
        &app,
        Method::PUT,
        "/_rustybin/harness/scenarios/my-burst",
        Some(burst.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &app,
        Method::PUT,
        "/_rustybin/harness/scenarios/smoke",
        Some(burst.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, v) = send(
        &app,
        Method::PUT,
        "/_rustybin/harness/scenarios/bad",
        Some(json!({ "name": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let (_, list) = send(&app, Method::GET, "/_rustybin/harness/scenarios", None).await;
    assert_eq!(list["saved"][0]["name"], "my-burst");
    assert!(list["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["name"] == "breakpoint"));
    let (status, v) = send(
        &app,
        Method::GET,
        "/_rustybin/harness/scenarios/my-burst",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["pattern"]["type"], "burst");
    let (status, _) = send(
        &app,
        Method::DELETE,
        "/_rustybin/harness/scenarios/my-burst",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        &app,
        Method::DELETE,
        "/_rustybin/harness/scenarios/my-burst",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Every built-in scenario validates and previews.
    for (name, text) in super::builtin_scenarios() {
        let mut s = super::scenario::Scenario::parse(text).unwrap();
        s.normalise();
        let problems = s.validate(&super::scenario::Caps::default());
        assert!(problems.is_empty(), "{name}: {problems:?}");
        assert!(s.preview()["points"].as_u64().unwrap() > 0, "{name}");
    }
}
