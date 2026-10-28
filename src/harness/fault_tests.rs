//! Phase 7 tests (plan section 15): every client fault observed at the
//! test proxy (new connections, HTTP/2, abandoned requests, slow and
//! malformed bodies, wrong content types, oversized headers, retries on
//! 5xx, expired tokens), every server fault observed by the client or at
//! `/health` (drops, stalls, flapping health, churn, on one node and in
//! turn across a cluster's server nodes), LLM streaming metrics checked
//! against the mock LLM's known pacing, and the SSE, WebSocket, gRPC and
//! MCP sources.

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};

use super::test_proxy::{self, ProxyConfig};
use super::tests::{harness_config, running, send, wait_for_report};
use super::Role;

type Node = (crate::RunningServer, axum::Router, crate::state::AppState);

async fn both() -> Node {
    running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await
}

async fn start(app: &axum::Router, scenario: Value) -> String {
    let (status, run) = send(app, Method::POST, "/_rustybin/harness/runs", Some(scenario)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    run["run_id"].as_str().unwrap().to_string()
}

/// A one-layer scenario of `rate` per second for `secs`.
fn scenario(name: &str, target: &str, source: Value, rate: u64, secs: &str, extra: Value) -> Value {
    let mut s = json!({
        "name": name,
        "target": target,
        "layers": [{ "name": "main", "source": source, "pattern": { "type": "constant", "rate": rate, "for": secs } }],
        "timeout": "2s",
    });
    if let (Value::Object(m), Value::Object(e)) = (&mut s, extra) {
        m.extend(e);
    }
    s
}

fn faults(client: Value) -> Value {
    json!({ "faults": { "client": client } })
}

fn status_200() -> Value {
    json!({ "request": { "path": "/status/200" } })
}

fn totals(report: &Value) -> &Value {
    &report["totals"]
}

#[tokio::test]
async fn connection_churn_http2_and_abandon_show_at_the_gateway() {
    let (server, app, _state) = both().await;
    let plain = test_proxy::start(server.http_addr, ProxyConfig::default()).await;
    let slow = test_proxy::start(
        server.http_addr,
        ProxyConfig {
            delay: Duration::from_millis(300),
            ..ProxyConfig::default()
        },
    )
    .await;
    // Without churn, a few connections carry everything; with it, one each.
    let id = start(
        &app,
        scenario("plain", &plain.url(), status_200(), 40, "1s", json!({})),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    let base = plain.counters.connections.lock().unwrap().len();
    assert!(base < 20, "{base} connections without churn");
    assert_eq!(totals(&report)["completed"], 40);
    let id = start(
        &app,
        scenario(
            "churn",
            &plain.url(),
            status_200(),
            40,
            "1s",
            faults(json!({ "connection_churn": 1.0 })),
        ),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    let churned = plain.counters.connections.lock().unwrap().len() - base;
    assert!(churned >= 35, "{churned} new connections with churn");
    assert_eq!(
        totals(&report)["faults"]["connection_churn"],
        40,
        "{}",
        totals(&report)
    );
    // HTTP/2 (prior knowledge on the plain proxy).
    let before = plain.counters.http2.load(Ordering::Relaxed);
    assert_eq!(before, 0);
    let id = start(
        &app,
        scenario(
            "h2",
            &plain.url(),
            status_200(),
            40,
            "1s",
            faults(json!({ "http2": 1.0 })),
        ),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    assert_eq!(
        plain.counters.http2.load(Ordering::Relaxed),
        40,
        "{}",
        totals(&report)
    );
    assert_eq!(totals(&report)["ok"], 40);
    // Abandoned before the slow proxy answers: sent, neither answered nor
    // failed, and the gateway still saw them.
    let id = start(
        &app,
        scenario(
            "abandon",
            &slow.url(),
            status_200(),
            20,
            "1s",
            faults(json!({ "abandon": { "fraction": 1.0, "after": "50ms" } })),
        ),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    let t = totals(&report);
    assert_eq!(t["abandoned"], 20, "{t}");
    assert_eq!(t["completed"], 0, "{t}");
    assert_eq!(t["transport"], 0, "{t}");
    assert_eq!(slow.counters.requests.load(Ordering::Relaxed), 20);
    assert!(report["layers"][0]["counts"]["abandoned"] == 20);
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn bad_bodies_and_headers_are_refused_as_expected() {
    let (server, app, _state) = both().await;
    // A mock that enforces request validation.
    let (status, body) = send(
        &app,
        Method::POST,
        "/_rustybin/mocks",
        Some(json!({
            "name": "strict",
            "spec": crate::mocks::SAMPLE_SPEC,
            "deploy": true,
            "responses": { "validate_requests": "enforce" }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let proxy = test_proxy::start(
        server.http_addr,
        ProxyConfig {
            record_header: Some("x-rustybin-padding".into()),
            ..ProxyConfig::default()
        },
    )
    .await;
    let create = json!({ "mock": "strict", "operations": { "include": ["createPet"] } });
    for (name, fault) in [
        ("malformed", json!({ "malformed_json": 1.0 })),
        ("wrong-type", json!({ "wrong_content_type": 1.0 })),
        (
            "oversized",
            json!({ "oversized_headers": { "fraction": 1.0, "bytes": 8192 } }),
        ),
    ] {
        let id = start(
            &app,
            scenario(name, &proxy.url(), create.clone(), 20, "1s", faults(fault)),
        )
        .await;
        let report = wait_for_report(&app, &id).await;
        let t = totals(&report);
        assert_eq!(t["completed"], 20, "{name}: {t}");
        if name == "oversized" {
            // The gateway saw the header; the mock (which does not mind it)
            // answered as documented.
            let seen = proxy.counters.recorded.lock().unwrap().clone();
            assert!(
                seen.iter().filter(|v| v.len() == 8192).count() >= 20,
                "{name}: {}",
                seen.len()
            );
            assert_eq!(t["faults"]["oversized_headers"], 20, "{t}");
        } else {
            // Refused with a 4xx: the expected answer of a request made to
            // be refused.
            assert_eq!(t["ok"], 20, "{name}: {t}");
            assert!(
                t["statuses"]["400"].as_u64().unwrap_or(0)
                    + t["statuses"]["415"].as_u64().unwrap_or(0)
                    == 20,
                "{name}: {t}"
            );
        }
    }
    // A slow body: three pieces of a 300-byte body at 500 bytes per second
    // take about 200 ms.
    let id = start(
        &app,
        scenario(
            "slow",
            &proxy.url(),
            json!({ "request": { "method": "POST", "path": "/anything", "body": { "pad": "x".repeat(280) } } }),
            10,
            "1s",
            faults(json!({ "slow_body": { "fraction": 1.0, "bytes_per_second": 500 } })),
        ),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    let t = totals(&report);
    assert_eq!(t["ok"], t["completed"], "{t}");
    assert_eq!(t["faults"]["slow_body"], t["completed"], "{t}");
    assert!(t["completed"].as_u64().unwrap() >= 9, "{t}");
    assert!(
        report["latency"]["p50_ms"].as_f64().unwrap() >= 150.0,
        "{}",
        report["latency"]
    );
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn retries_on_5xx_and_expired_tokens() {
    let (server, app, _state) = both().await;
    // Every second answer is the gateway's own 503.
    let flaky = test_proxy::start(
        server.http_addr,
        ProxyConfig {
            own_answer_every: Some((2, 503)),
            ..ProxyConfig::default()
        },
    )
    .await;
    for synchronised in [false, true] {
        let id = start(
            &app,
            scenario(
                "retries",
                &flaky.url(),
                status_200(),
                20,
                "3s",
                faults(json!({ "retry_on_5xx": { "retries": 1, "synchronised": synchronised } })),
            ),
        )
        .await;
        let report = wait_for_report(&app, &id).await;
        let t = totals(&report);
        let retried = t["faults"]["retry_on_5xx"].as_u64().unwrap_or(0);
        assert!(retried >= 24, "{t}");
        // Retries are requests of the layer: more were sent than the
        // pattern asked for on its own (a retry due after the pattern
        // ended is dropped, which on a slow machine is most of the last
        // second's, hence three seconds).
        assert!(t["sent"].as_u64().unwrap() > 60, "{t}");
        assert!(t["sent"].as_u64().unwrap() <= 60 + retried, "{t}");
    }
    // Credential churn: every token expired, refused by the JWT check.
    let plain = test_proxy::start(server.http_addr, ProxyConfig::default()).await;
    let id = start(
        &app,
        json!({
            "name": "expired",
            "target": plain.url(),
            "timeout": "2s",
            "layers": [{
                "name": "users",
                "source": { "request": { "path": "/auth/jwt" } },
                "identities": { "jwt": { "mint": 2 } },
                "pattern": { "type": "constant", "rate": 20, "for": "1s" }
            }],
            "faults": { "client": { "expired_tokens": 1.0 } }
        }),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    let t = totals(&report);
    assert_eq!(t["faults"]["expired_tokens"], 20, "{t}");
    assert_eq!(t["ok"], 20, "all refused with a 4xx: {t}");
    assert!(t["statuses"]["401"].as_u64().unwrap_or(0) > 0, "{t}");
    // Without the fault the same tokens pass.
    let id = start(
        &app,
        json!({
            "name": "valid",
            "target": plain.url(),
            "timeout": "2s",
            "layers": [{
                "name": "users",
                "source": { "request": { "path": "/auth/jwt" } },
                "identities": { "jwt": { "mint": 2 } },
                "pattern": { "type": "constant", "rate": 10, "for": "1s" }
            }]
        }),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    let t = totals(&report);
    assert_eq!(t["statuses"]["200"], t["completed"], "{t}");
    assert!(t["completed"].as_u64().unwrap() >= 9, "{t}");
    server.shutdown();
    let _ = server.wait().await;
}

async fn health(app: &axum::Router) -> u16 {
    send(app, Method::GET, "/health", None).await.0.as_u16()
}

#[tokio::test]
async fn server_faults_drop_stall_flap_and_churn() {
    let (server, app, _state) = both().await;
    let proxy = test_proxy::start(server.http_addr, ProxyConfig::default()).await;
    let node = format!("http://{}", server.http_addr);
    let shaped = json!({ "request": { "path": "/harness/orders" } });
    let server_faults = |f: Value| json!({ "faults": { "server": f } });
    // Dropped after the headers: a reset on the way back, straight from the
    // node and through the gateway alike.
    for target in [node.clone(), proxy.url()] {
        let id = start(
            &app,
            scenario(
                "drop",
                &target,
                shaped.clone(),
                20,
                "1s",
                server_faults(json!({ "drop": 1.0 })),
            ),
        )
        .await;
        let report = wait_for_report(&app, &id).await;
        let t = totals(&report);
        assert_eq!(t["transport"], 20, "{target}: {t}");
        assert!(
            t["transport_kinds"]
                .as_object()
                .unwrap()
                .values()
                .any(|v| v.as_u64().unwrap_or(0) == 20),
            "{t}"
        );
    }
    // Stalled longer than the timeout.
    let id = start(
        &app,
        scenario(
            "stall",
            &proxy.url(),
            shaped.clone(),
            10,
            "1s",
            json!({ "timeout": "300ms", "faults": { "server": { "stall": { "fraction": 1.0, "for": "2s" } } } }),
        ),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    let t = totals(&report);
    assert_eq!(t["transport_kinds"]["timeout"], t["completed"], "{t}");
    assert!(t["completed"].as_u64().unwrap() >= 9, "{t}");
    // Flapping health: /health answers 503 for 300 ms of every 600 ms while
    // the run runs, and is healthy again after it.
    let id = start(
        &app,
        scenario(
            "flap",
            &proxy.url(),
            shaped.clone(),
            5,
            "2s",
            server_faults(json!({ "flapping_health": { "every": "600ms", "down_for": "300ms" } })),
        ),
    )
    .await;
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..40 {
        seen.insert(health(&app).await);
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert_eq!(seen, [200, 503].into_iter().collect(), "{seen:?}");
    let report = wait_for_report(&app, &id).await;
    assert_eq!(report["state"], "finished");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(health(&app).await, 200);
    // Churn on one node: down for the first 400 ms of every second, marked
    // requests get a 503 with the connection closed.
    let id = start(
        &app,
        scenario(
            "churn",
            &proxy.url(),
            shaped,
            30,
            "2s",
            server_faults(json!({ "churn": { "every": "1s", "down_for": "400ms" } })),
        ),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    let t = totals(&report);
    let down = t["statuses"]["503"].as_u64().unwrap_or(0);
    let up = t["statuses"]["200"].as_u64().unwrap_or(0);
    assert!((15..=35).contains(&down), "{t}");
    assert!(up >= 25, "{t}");
    // The fault task restores /health on its next tick.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(health(&app).await, 200);
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn llm_streaming_metrics_match_the_mocks_pacing() {
    let (server, app, _state) = both().await;
    let proxy = test_proxy::start(server.http_addr, ProxyConfig::default()).await;
    let id = start(
        &app,
        json!({
            "name": "llm",
            "target": proxy.url(),
            "timeout": "5s",
            "layers": [{
                "name": "chat",
                "source": { "llm": { "tokens": 20, "headers": { "X-Rustybin-TTFT-Ms": "200", "X-Rustybin-Tokens-Per-Second": "50" } } },
                "pattern": { "type": "constant", "rate": 4, "for": "2s" }
            }]
        }),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    assert_eq!(report["state"], "finished", "{}", report["error"]);
    assert_eq!(totals(&report)["ok"], 8, "{}", totals(&report));
    let s = &report["streams"][0];
    assert_eq!(s["layer"], "chat");
    assert_eq!(s["protocol"], "llm");
    assert_eq!(s["streams"], 8, "{s}");
    let per = s["items_per_stream"].as_f64().unwrap();
    assert!((10.0..=25.0).contains(&per), "{s}");
    // Time to the first token: the 200 ms TTFT plus a little.
    let first = s["first_item"]["p50_ms"].as_f64().unwrap();
    assert!((190.0..=450.0).contains(&first), "{s}");
    // 50 tokens per second: 20 ms between tokens.
    let gap = s["gap"]["p50_ms"].as_f64().unwrap();
    assert!((15.0..=40.0).contains(&gap), "{s}");
    let tps = s["items_per_second"].as_f64().unwrap();
    assert!((15.0..=60.0).contains(&tps), "{s}");
    assert_eq!(s["stalled"], 0);
    // A stream that stalls: a pace slower than the timeout allows.
    let id = start(
        &app,
        json!({
            "name": "stalled",
            "target": proxy.url(),
            "timeout": "300ms",
            "layers": [{
                "name": "chat",
                "source": { "llm": { "tokens": 5, "headers": { "X-Rustybin-TTFT-Ms": "1500" } } },
                "pattern": { "type": "constant", "rate": 2, "for": "1s" }
            }]
        }),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    let s = &report["streams"][0];
    assert_eq!(s["stalled"], 2, "{s}");
    assert_eq!(s["items"], 0, "{s}");
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn sse_websocket_grpc_and_mcp_sources() {
    let (server, app, _state) = both().await;
    let proxy = test_proxy::start(server.http_addr, ProxyConfig::default()).await;
    let node = format!("http://{}", server.http_addr);
    // SSE: five events 50 ms apart, then a reader that never reads.
    let id = start(
        &app,
        json!({
            "name": "sse",
            "target": proxy.url(),
            "timeout": "2s",
            "layers": [
                { "name": "readers", "source": { "sse": { "path": "/sse?count=5&interval_ms=50" } }, "pattern": { "type": "constant", "rate": 4, "for": "1s" } },
                { "name": "stuck", "source": { "sse": { "path": "/sse?count=50&interval_ms=20", "hold_unread": "300ms" } }, "pattern": { "type": "constant", "rate": 2, "for": "1s" } }
            ]
        }),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    assert_eq!(report["state"], "finished", "{}", report["error"]);
    let streams = report["streams"].as_array().unwrap();
    let readers = streams.iter().find(|s| s["layer"] == "readers").unwrap();
    assert_eq!(readers["items_per_stream"], 5.0, "{readers}");
    let gap = readers["gap"]["p50_ms"].as_f64().unwrap();
    assert!((35.0..=90.0).contains(&gap), "{readers}");
    let stuck = streams.iter().find(|s| s["layer"] == "stuck").unwrap();
    assert_eq!(stuck["items"], 0, "{stuck}");
    assert!(
        stuck["total"]["p50_ms"].as_f64().unwrap() >= 290.0,
        "{stuck}"
    );
    assert_eq!(totals(&report)["ok"], 6);
    // WebSocket sessions on the node: five echoes each.
    let id = start(
        &app,
        json!({
            "name": "ws",
            "target": node,
            "timeout": "2s",
            "layers": [{ "name": "sockets", "source": { "websocket": { "messages": 5, "size": 32 } }, "pattern": { "type": "constant", "rate": 4, "for": "1s" } }]
        }),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    let t = totals(&report);
    assert_eq!(t["ok"], 4, "{t}");
    // The upgrade went through the node's server layer: tagged.
    assert_eq!(t["upstream"], 4, "{t}");
    let s = &report["streams"][0];
    assert_eq!(
        (s["protocol"].as_str(), s["items_per_stream"].as_f64()),
        (Some("websocket"), Some(5.0)),
        "{s}"
    );
    assert!(s["gap"]["p99_ms"].as_f64().unwrap() < 500.0, "{s}");
    // gRPC on the gRPC listener (h2c prior knowledge).
    let grpc = format!("http://{}", server.grpc_addr.expect("gRPC listener"));
    let id = start(
        &app,
        json!({
            "name": "grpc",
            "targets": [{ "url": grpc, "http": "h2c" }],
            "timeout": "2s",
            "layers": [{ "name": "calls", "source": { "grpc": { "message": "hi" } }, "pattern": { "type": "constant", "rate": 20, "for": "1s" } }]
        }),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    let t = totals(&report);
    assert_eq!(t["ok"], 20, "{t}");
    assert_eq!(t["unattributed"], 20, "{t}");
    // A method that fails: the grpc-status says so.
    let id = start(
        &app,
        json!({
            "name": "grpc-fail",
            "targets": [{ "url": format!("http://{}", server.grpc_addr.unwrap()), "http": "h2c" }],
            "timeout": "2s",
            "layers": [{ "name": "calls", "source": { "grpc": { "path": "/rustybin.echo.v1.EchoService/Nope" } }, "pattern": { "type": "constant", "rate": 5, "for": "1s" } }]
        }),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    assert_eq!(
        totals(&report)["unexpected_status"],
        5,
        "{}",
        totals(&report)
    );
    // MCP tool calls through the proxy: echo works, fail is an error.
    let id = start(
        &app,
        json!({
            "name": "mcp",
            "target": proxy.url(),
            "timeout": "2s",
            "layers": [
                { "name": "echo", "source": { "mcp": { "tool": "echo", "arguments": { "message": "hi {{seq}}" } } }, "pattern": { "type": "constant", "rate": 10, "for": "1s" } },
                { "name": "fail", "source": { "mcp": { "tool": "fail" } }, "pattern": { "type": "constant", "rate": 5, "for": "1s" } }
            ]
        }),
    )
    .await;
    let report = wait_for_report(&app, &id).await;
    let layers = report["layers"].as_array().unwrap();
    let echo = &layers[0]["counts"];
    assert_eq!(echo["ok"], echo["completed"], "{echo}");
    assert!(echo["completed"].as_u64().unwrap() >= 9, "{echo}");
    let fail = &layers[1]["counts"];
    assert_eq!(fail["unexpected_status"], fail["completed"], "{fail}");
    // Answers of the mock MCP server are tagged like any other.
    assert_eq!(echo["upstream"], echo["completed"], "{echo}");
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn churn_takes_turns_across_a_clusters_server_nodes() {
    use super::cluster_tests::{
        client_config, join, node, wait_for_alive, wait_for_cluster_report, TOKEN,
    };
    let l = node(harness_config(&[Role::Coordinator], &[])).await;
    let (status, cluster) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters",
        Some(json!({ "cluster_id": "churn-1", "join_token": TOKEN })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{cluster}");
    let url = format!("{}/_rustybin/harness/clusters/churn-1", l.base());
    let c1 = node(client_config(1000)).await;
    let s1 = node(harness_config(&[Role::Server], &[])).await;
    let s2 = node(harness_config(&[Role::Server], &[])).await;
    let proxy = test_proxy::start_pool(
        vec![s1.server.http_addr, s2.server.http_addr],
        ProxyConfig::default(),
    )
    .await;
    for n in [&c1, &s1, &s2] {
        join(n, &url).await;
    }
    wait_for_alive(&l, "churn-1", 1, 2).await;
    let (status, run) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters/churn-1/runs",
        Some(json!({
            "name": "churn",
            "target": proxy.url(),
            "source": { "request": { "path": "/harness/x" } },
            "pattern": { "type": "constant", "rate": 20, "for": "3s" },
            "scale": { "virtual_clients": 10, "client_nodes": 1, "server_nodes": 2 },
            "faults": { "preset": "churn", "server": { "churn": { "every": "1s", "down_for": "400ms" } } },
            "timeout": "2s"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{run}");
    let run_id = run["run_id"].as_str().unwrap().to_string();
    let (status, started) = send(
        &l.app,
        Method::POST,
        &format!("/_rustybin/harness/clusters/churn-1/runs/{run_id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    // Both server nodes go down at some point, never both at once.
    let (mut down1, mut down2, mut both_down) = (false, false, false);
    for _ in 0..70 {
        let (h1, h2) = (health(&s1.app).await, health(&s2.app).await);
        down1 |= h1 == 503;
        down2 |= h2 == 503;
        both_down |= h1 == 503 && h2 == 503;
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(down1 && down2, "s1 down {down1}, s2 down {down2}");
    assert!(!both_down);
    let report = wait_for_cluster_report(&l, "churn-1", &run_id).await;
    assert_eq!(report["state"], "finished", "{}", report["error"]);
    let t = totals(&report);
    assert!(t["statuses"]["503"].as_u64().unwrap_or(0) > 0, "{t}");
    assert!(t["statuses"]["200"].as_u64().unwrap_or(0) > 20, "{t}");
    for n in [l, c1, s1, s2] {
        n.stop().await;
    }
}
