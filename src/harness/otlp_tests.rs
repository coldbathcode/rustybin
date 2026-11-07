//! The body, the policy and sending once.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use axum::Router;
use serde_json::{json, Value};

use super::*;
use crate::config::Config;
use crate::test_support::test_state_with;

fn report(id: &str) -> Value {
    json!({
        "run_id": id, "scenario_name": "smoke", "state": "finished", "passed": true,
        "achieved_rate": 49.5,
        "totals": { "sent": 500, "completed": 499, "ok": 498, "transport": 1, "lost": 0 },
        "latency": { "p50_ms": 2.5, "p99_ms": 9.0 },
        "targets": [{ "url": "https://secret-target.example.com/" }],
    })
}

type Got = Arc<Mutex<Vec<(String, Option<String>, Value)>>>;

/// A collector on a free port, keeping what it was sent.
async fn collector() -> (String, Got) {
    let got: Got = Arc::default();
    let seen = got.clone();
    let router = Router::new().fallback(
        move |uri: axum::http::Uri, headers: HeaderMap, body: axum::body::Bytes| {
            let seen = seen.clone();
            async move {
                let auth = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let v = serde_json::from_slice(&body).unwrap_or(Value::Null);
                seen.lock()
                    .expect("lock")
                    .push((uri.path().to_string(), auth, v));
                "{}"
            }
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, router).await });
    (format!("http://{addr}"), got)
}

fn state() -> crate::state::AppState {
    let mut c = Config::for_tests();
    c.egress_allow_loopback = true;
    test_state_with(c)
}

#[test]
fn the_body_is_numbers_and_names_only() {
    let v = body(&report("r1"), "node-1", 1_700_000_000_000_000_000);
    let scope = &v["resourceMetrics"][0]["scopeMetrics"][0];
    let names: Vec<&str> = scope["metrics"]
        .as_array()
        .expect("metrics")
        .iter()
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert!(names.contains(&"rustybin.harness.requests.sent"));
    assert!(names.contains(&"rustybin.harness.latency.p99"));
    let sent = &scope["metrics"][0]["gauge"]["dataPoints"][0];
    assert_eq!(
        sent["asInt"], "500",
        "OTLP JSON writes 64 bit integers as strings"
    );
    assert_eq!(sent["timeUnixNano"], "1700000000000000000");
    assert!(!v.to_string().contains("secret-target"), "{v}");
}

#[tokio::test]
async fn a_report_reaches_the_collector_with_the_token() {
    let (url, got) = collector().await;
    let s = state();
    let mut secrets = BTreeMap::new();
    secrets.insert(TOKEN_KEY.to_string(), Some("otel-token".to_string()));
    s.settings
        .update(BTreeMap::new(), secrets, "test")
        .expect("token");
    send(&s, &format!("{url}/"), &report("r1"))
        .await
        .expect("sent");
    let got = got.lock().expect("lock");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0, "/v1/metrics");
    assert_eq!(got[0].1.as_deref(), Some("Bearer otel-token"));
    assert!(got[0].2["resourceMetrics"].is_array());
}

#[tokio::test]
async fn the_policy_still_refuses_what_it_always_refuses() {
    let s = state();
    let refused = send(&s, "http://169.254.169.254:4318", &report("r1")).await;
    assert!(refused.is_err(), "{refused:?}");
    assert!(send(&s, "not a url", &report("r1")).await.is_err());
}

#[test]
fn a_run_is_sent_once_and_the_memory_is_bounded() {
    let mut sent = Sent::default();
    assert!(sent.first_time("a"));
    assert!(!sent.first_time("a"));
    for i in 0..REMEMBERED + 5 {
        sent.first_time(&format!("r{i}"));
    }
    assert_eq!(sent.ids.len(), REMEMBERED);
    assert_eq!(sent.order.len(), REMEMBERED);
}
