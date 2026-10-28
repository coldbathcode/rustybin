//! Harness tests: startup refusals, the control API on and off, a smoke run
//! through the real listener (rates within 2 percent, coordinated omission
//! corrected), allowlist and metadata refusals, verification, the kill
//! switch, the closed model, control-plane scopes and headless mode. Rates
//! stay low (hundreds per second for a second or two) to keep the suite fast.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::scenario::Scenario;
use super::{check_startup, is_harness_path, Harness, Role, StartMode};
use crate::config::Config;
use crate::control_auth::Grant;
use crate::state::AppState;
use crate::test_support::{body_string, control_jwt, jwt_config, test_app_with};

pub(super) fn harness_config(roles: &[Role], targets: &[&str]) -> Config {
    let mut c = Config::for_tests();
    c.harness.enabled = true;
    c.harness.roles = roles.to_vec();
    c.harness.targets = targets.iter().map(|s| s.to_string()).collect();
    c.harness.threads = Some(1);
    c
}

/// A real server (the load goes over TCP) plus its app for control calls.
pub(super) async fn running(config: Config) -> (crate::RunningServer, Router, AppState) {
    let state = AppState::for_tests(config);
    let server = crate::server::start_with_state(state.clone())
        .await
        .expect("server");
    let app = test_app_with(state.clone());
    (server, app, state)
}

pub(super) async fn send(
    app: &Router,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path);
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = app
        .clone()
        .oneshot(b.body(body).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    let text = body_string(resp).await;
    let json = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, json)
}

fn smoke(url: &str, rate: u64, secs: &str) -> Value {
    json!({
        "name": "smoke",
        "target": url,
        "source": { "request": { "path": "/status/200" } },
        "pattern": { "type": "constant", "rate": rate, "for": secs },
        "timeout": "5s",
        "thresholds": { "error_rate": 0, "not_sent": 0, "saturated": false }
    })
}

pub(super) async fn wait_for_report(app: &Router, id: &str) -> Value {
    for _ in 0..600 {
        let (status, run) = send(
            app,
            Method::GET,
            &format!("/_rustybin/harness/runs/{id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{run}");
        if run["report_ready"] == true {
            let (status, report) = send(
                app,
                Method::GET,
                &format!("/_rustybin/harness/runs/{id}/report"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{report}");
            return report;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("run {id} did not finish");
}

#[test]
fn roles_start_modes_and_paths() {
    assert_eq!(
        Role::parse_list("both"),
        Ok(vec![Role::Client, Role::Server])
    );
    assert_eq!(
        Role::parse_list("client, coordinator"),
        Ok(vec![Role::Client, Role::Coordinator])
    );
    assert_eq!(Role::parse_list("client,bogus"), Err("bogus".into()));
    assert!(Role::parse_list("").is_err());
    assert_eq!(StartMode::parse("on-quorum"), Some(StartMode::OnQuorum));
    assert_eq!(StartMode::parse("now"), Some(StartMode::Immediately));
    assert_eq!(StartMode::parse("later"), None);
    assert!(is_harness_path("/_rustybin/harness"));
    assert!(is_harness_path("/_rustybin/harness/runs/x"));
    assert!(!is_harness_path("/_rustybin/harnessx"));
    assert!(!is_harness_path("/_rustybin/config"));
}

#[test]
fn startup_refusals() {
    assert!(check_startup(&Config::for_tests()).is_ok());
    let mut c = harness_config(&[Role::Client], &["gw.example"]);
    assert!(check_startup(&c).is_ok());
    c.public_mode = true;
    assert!(check_startup(&c).unwrap_err().contains("PUBLIC_MODE"));
    let mut c = harness_config(&[Role::Server], &[]);
    assert!(check_startup(&c).is_ok(), "a server needs no targets");
    c.self_service.enabled = true;
    assert!(check_startup(&c).unwrap_err().contains("SELF_SERVICE"));
    let c = harness_config(&[Role::Client], &[]);
    assert!(check_startup(&c)
        .unwrap_err()
        .contains("RUSTYBIN_HARNESS_TARGETS"));
    let c = harness_config(&[Role::Client], &["bad host"]);
    let err = check_startup(&c).unwrap_err();
    assert!(
        err.contains("RUSTYBIN_HARNESS_TARGETS") && err.contains("bad host"),
        "{err}"
    );
}

#[tokio::test]
async fn startup_refusal_is_fatal_for_the_server() {
    let mut c = harness_config(&[Role::Client], &["gw.example"]);
    c.public_mode = true;
    let err = crate::server::start_with_state(AppState::for_tests(c))
        .await
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(err.contains("PUBLIC_MODE"), "{err}");
}

#[test]
fn harness_scope_rules() {
    let harness = Grant::from_scope_claim(Some(&json!("harness")));
    assert!(harness.harness && !harness.console && !harness.admin);
    assert!(harness.allows(&Method::GET, "/_rustybin/harness"));
    assert!(harness.allows(&Method::POST, "/_rustybin/harness/runs"));
    assert!(!harness.allows(&Method::GET, "/_rustybin/config"));
    let console = Grant::from_scope_claim(None);
    assert!(console.allows(&Method::GET, "/_rustybin/harness/runs"));
    assert!(!console.allows(&Method::POST, "/_rustybin/harness/runs"));
    assert_eq!(
        Grant::ADMIN.scopes(),
        vec!["admin", "console", "inspector", "harness"]
    );
}

#[tokio::test]
async fn api_when_harness_is_off() {
    let app = crate::test_support::test_app();
    let (status, v) = send(&app, Method::GET, "/_rustybin/harness", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["enabled"], false);
    assert_eq!(v["harness_api"], 1);
    let (status, v) = send(&app, Method::GET, "/_rustybin/harness/runs", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(v["hint"].as_str().unwrap().contains("RUSTYBIN_HARNESS=on"));
    let (status, _) = send(&app, Method::POST, "/_rustybin/harness/stop", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&app, Method::GET, "/.well-known/rustybin-harness", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Static things work everywhere.
    let (status, schema) = send(&app, Method::GET, "/_rustybin/harness/schema", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(schema["x-caps"]["max_rate"], 50_000);
    assert!(schema["properties"]["layers"].is_object());
    let (status, list) = send(&app, Method::GET, "/_rustybin/harness/scenarios", None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = list["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "smoke",
            "ramp-to-target",
            "soak",
            "closed-users",
            "step-plateaus",
            "breakpoint",
            "spike-and-recover",
            "pulse",
            "burst",
            "daily-wave",
            "rate-limit",
            "limit-probe",
            "many-ips",
        ]
    );
    for s in list["scenarios"].as_array().unwrap() {
        assert!(
            s.get("error").is_none(),
            "built-in scenario does not parse: {s}"
        );
        assert!(s["summary"]["expected_requests"].as_f64().unwrap_or(0.0) >= 0.0);
    }
    let (status, smoke) = send(
        &app,
        Method::GET,
        "/_rustybin/harness/scenarios/smoke",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(smoke["name"], "smoke");
    let (status, yaml) = send(
        &app,
        Method::GET,
        "/_rustybin/harness/scenarios/smoke?format=yaml",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(yaml.as_str().unwrap().contains("name: smoke"));
    let (status, _) = send(&app, Method::GET, "/_rustybin/harness/scenarios/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Validation lists problems without sending anything.
    let (status, v) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/validate",
        Some(json!({ "name": "x", "target": "http://gw/_rustybin", "pattern": { "type": "constant", "rate": 1, "for": "1s" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["valid"], false);
    let problems = v["problems"].as_array().unwrap();
    assert!(problems
        .iter()
        .any(|p| p.as_str().unwrap().contains("control plane")));
    assert!(problems
        .iter()
        .any(|p| p.as_str().unwrap().contains("source")));
    let (status, v) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/validate",
        Some(json!("nope")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
}

#[tokio::test]
async fn built_in_scenarios_validate_against_default_caps() {
    for (name, text) in super::builtin_scenarios() {
        let mut s = Scenario::parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
        s.normalise();
        let problems = s.validate(&super::scenario::Caps::default());
        assert!(problems.is_empty(), "{name}: {problems:#?}");
        assert_eq!(s.name, *name);
    }
}

#[tokio::test]
async fn smoke_run_through_the_real_listener() {
    let (server, app, state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let (status, st) = send(&app, Method::GET, "/_rustybin/harness", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(st["enabled"], true);
    assert_eq!(st["roles"], json!(["client", "server"]));
    assert_eq!(st["allowlist"], json!(["127.0.0.1"]));
    assert!(st["engine"]["inflight_cap"].as_u64().unwrap() >= 1);
    let (status, token) = send(&app, Method::GET, "/.well-known/rustybin-harness", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        token.as_str().unwrap(),
        state.harness.as_ref().unwrap().verification_token
    );

    let url = format!("http://{}", server.http_addr);
    let (status, run) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(smoke(&url, 200, "1s")),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    assert_eq!(
        run["links"]["report"],
        format!("/_rustybin/harness/runs/{id}/report")
    );
    // A second run while one is active is refused.
    let (status, again) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(smoke(&url, 1, "1s")),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{again}");
    // The report is not there yet.
    let (status, _) = send(
        &app,
        Method::GET,
        &format!("/_rustybin/harness/runs/{id}/report"),
        None,
    )
    .await;
    assert!(status == StatusCode::CONFLICT || status == StatusCode::OK);

    let report = wait_for_report(&app, &id).await;
    assert_eq!(report["state"], "finished", "{report}");
    let totals = &report["totals"];
    let sent = totals["sent"].as_u64().unwrap();
    assert!(
        (196..=204).contains(&sent),
        "sent {sent} (within 2 percent of 200)"
    );
    assert_eq!(totals["not_sent"], 0);
    assert_eq!(totals["completed"], sent);
    assert_eq!(totals["ok"], sent, "{totals}");
    assert_eq!(totals["transport"], 0);
    assert_eq!(totals["statuses"]["200"], sent);
    assert!(report["latency"]["p99_ms"].as_f64().unwrap() > 0.0);
    // Measured from the intended send, latency never undercuts service time.
    assert!(
        report["latency"]["max_ms"].as_f64().unwrap()
            >= report["service_latency"]["max_ms"].as_f64().unwrap()
    );
    assert_eq!(report["passed"], true, "{:?}", report["thresholds"]);
    assert_eq!(report["exit_code"], 0);
    assert_eq!(report["saturation"]["saturated"], false);
    assert!(!report["timeline"].as_array().unwrap().is_empty());
    assert_eq!(report["layers"][0]["name"], "main");
    assert_eq!(
        report["targets"][0]["pinned"],
        json!([server.http_addr.to_string()])
    );
    assert!(report["samples"].as_array().unwrap().len() <= 1000);
    assert!(report["samples"][0]["request_id"]
        .as_str()
        .unwrap()
        .starts_with(&format!("{id}-")));
    assert!(report["scenario"]["layers"].is_array());
    let threshold_names: Vec<&str> = report["thresholds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(threshold_names, vec!["error_rate", "not_sent", "saturated"]);

    // Text and JUnit renderings, and the listing.
    let (status, text) = send(
        &app,
        Method::GET,
        &format!("/_rustybin/harness/runs/{id}/report?format=text"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(text
        .as_str()
        .unwrap()
        .contains("Result: PASS (exit code 0)"));
    let (_, junit) = send(
        &app,
        Method::GET,
        &format!("/_rustybin/harness/runs/{id}/report?format=junit"),
        None,
    )
    .await;
    assert!(junit.as_str().unwrap().starts_with("<?xml"));
    let (_, list) = send(&app, Method::GET, "/_rustybin/harness/runs", None).await;
    assert_eq!(list["runs"][0]["run_id"], id);
    assert_eq!(list["active"], Value::Null);
    // Stopping a finished run changes nothing.
    let (status, v) = send(
        &app,
        Method::POST,
        &format!("/_rustybin/harness/runs/{id}/stop"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["stopped"], false);
    // The inspector saw the load (every request went over TCP to this node).
    // Harness traffic is sampled into the inspector (1 in 100 by default).
    assert!(state.inspector.total_recorded() >= sent / 100);
    assert!(state.inspector.total_recorded() < sent);
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn unexpected_statuses_and_thresholds_fail_the_run() {
    let (server, app, _state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let url = format!("http://{}", server.http_addr);
    let scenario = json!({
        "name": "errors",
        "targets": [{ "url": url, "weight": 1 }],
        "layers": [
            { "name": "good", "source": { "request": { "path": "/status/200" } },
              "pattern": { "type": "constant", "rate": 50, "for": "1s" } },
            { "name": "bad", "source": { "request": { "path": "/status/503" } },
              "pattern": { "type": "constant", "rate": 50, "for": "1s" } },
            { "name": "expected", "source": { "request": { "path": "/status/404", "expect_status": [404] } },
              "pattern": { "type": "constant", "rate": 20, "for": "1s" } }
        ],
        "thresholds": { "error_rate": 0.1 }
    });
    let (status, run) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(scenario),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    let totals = &report["totals"];
    assert_eq!(
        totals["ok"].as_u64().unwrap() + totals["unexpected_status"].as_u64().unwrap(),
        totals["completed"].as_u64().unwrap()
    );
    assert!(
        totals["unexpected_status"].as_u64().unwrap() >= 45,
        "{totals}"
    );
    assert_eq!(report["passed"], false);
    assert_eq!(report["exit_code"], 1);
    let layers = report["layers"].as_array().unwrap();
    assert_eq!(layers.len(), 3);
    assert_eq!(
        layers[1]["counts"]["unexpected_status"],
        layers[1]["counts"]["completed"]
    );
    assert_eq!(layers[2]["counts"]["ok"], layers[2]["counts"]["completed"]);
    assert!(report["samples"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["kind"] == "error" && s["status"] == 503));
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn closed_model_users() {
    let (server, app, _state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let url = format!("http://{}", server.http_addr);
    let scenario = json!({
        "name": "users",
        "target": url,
        "scale": { "virtual_clients": 4 },
        "model": "closed",
        "think_time": "20ms",
        "source": { "request": { "path": "/delay/10" } },
        "pattern": { "type": "constant", "rate": 3, "for": "1s" }
    });
    let (status, run) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(scenario),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    assert_eq!(report["state"], "finished");
    let sent = report["totals"]["sent"].as_u64().unwrap();
    // 3 users, about 30 ms per cycle: tens of requests, not hundreds.
    assert!((20..=120).contains(&sent), "sent {sent}");
    assert_eq!(report["totals"]["completed"], sent);
    assert_eq!(report["totals"]["transport"], 0);
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn kill_switch_stops_a_run() {
    let (server, app, _state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let url = format!("http://{}", server.http_addr);
    let (status, run) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(smoke(&url, 100, "30s")),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (status, v) = send(&app, Method::POST, "/_rustybin/harness/stop", None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["stopped"], json!([id]));
    let report = wait_for_report(&app, &id).await;
    assert_eq!(report["state"], "stopped");
    assert_eq!(report["stopped_by"], "kill_switch");
    assert_eq!(report["exit_code"], 2);
    assert!(report["duration_secs"].as_f64().unwrap() < 10.0);
    assert!(report["totals"]["sent"].as_u64().unwrap() >= 10);
    // The switch clears and a new run may start; a user stop ends it too.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let (status, run) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(smoke(&url, 50, "30s")),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    let (status, v) = send(
        &app,
        Method::POST,
        &format!("/_rustybin/harness/runs/{id}/stop"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    let report = wait_for_report(&app, &id).await;
    assert_eq!(report["stopped_by"], "user");
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn targets_outside_the_allowlist_and_metadata_are_refused() {
    let (server, app, _state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1:1"],
    ))
    .await;
    for (url, needle) in [
        ("http://169.254.169.254/latest", "metadata"),
        ("http://10.255.255.1:9", "not in RUSTYBIN_HARNESS_TARGETS"),
        (
            &format!("http://{}", server.http_addr),
            "not in RUSTYBIN_HARNESS_TARGETS",
        ),
        (
            "http://gw.example.invalid",
            "not in RUSTYBIN_HARNESS_TARGETS",
        ),
    ] {
        let (status, v) = send(
            &app,
            Method::POST,
            "/_rustybin/harness/runs",
            Some(smoke(url, 1, "1s")),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{url}: {v}");
        assert!(
            v["problems"][0].as_str().unwrap().contains(needle),
            "{url}: {v}"
        );
    }
    let (status, v) = send(&app, Method::POST, "/_rustybin/harness/runs", Some(json!({ "name": "x", "target": "http://127.0.0.1:1/_rustybin", "source": { "request": { "path": "/" } }, "pattern": { "type": "constant", "rate": 1, "for": "1s" } }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["problems"][0].as_str().unwrap().contains("control plane"));
    let (_, list) = send(&app, Method::GET, "/_rustybin/harness/runs", None).await;
    assert_eq!(
        list["runs"].as_array().unwrap().len(),
        0,
        "nothing was started"
    );
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn a_node_without_the_client_role_refuses_runs() {
    let (server, app, _state) = running(harness_config(&[Role::Server], &[])).await;
    let url = format!("http://{}", server.http_addr);
    let (status, v) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(smoke(&url, 1, "1s")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert!(v["error"].as_str().unwrap().contains("client role"));
    let (status, st) = send(&app, Method::GET, "/_rustybin/harness", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(st["engine"], Value::Null);
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn target_verification() {
    let mut config = harness_config(&[Role::Client, Role::Server], &["127.0.0.1"]);
    config.harness.verify_targets = true;
    let (server, app, _state) = running(config).await;
    let url = format!("http://{}", server.http_addr);
    // The node serves its own token: a loop-through passes.
    let (status, run) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(smoke(&url, 20, "1s")),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    assert_eq!(report["targets"][0]["verified"], true);
    // Another instance serves another token: refused before any load.
    let (other, _, _) = running(harness_config(&[Role::Server], &[])).await;
    let other_url = format!("http://{}", other.http_addr);
    let (status, v) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(smoke(&other_url, 20, "1s")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert!(
        v["problems"][0]
            .as_str()
            .unwrap()
            .contains("verification token"),
        "{v}"
    );
    // A plain instance without harness mode answers 404 at the path.
    let (plain, _, _) = running(Config::for_tests()).await;
    let plain_url = format!("http://{}", plain.http_addr);
    let (status, v) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(smoke(&plain_url, 20, "1s")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert!(v["problems"][0].as_str().unwrap().contains("404"), "{v}");
    for s in [server, other, plain] {
        s.shutdown();
        let _ = s.wait().await;
    }
}

#[tokio::test]
async fn control_scopes_on_a_protected_control_plane() {
    let mut config = jwt_config();
    config.harness.enabled = true;
    config.harness.roles = vec![Role::Client, Role::Server];
    config.harness.targets = vec!["127.0.0.1".into()];
    config.harness.threads = Some(1);
    let (server, app, _state) = running(config).await;
    let url = format!("http://{}", server.http_addr);
    let call = |token: Option<String>, method: Method, path: &str, body: Option<Value>| {
        let app = app.clone();
        let path = path.to_string();
        async move {
            let mut b = Request::builder().method(method).uri(path);
            if let Some(t) = token {
                b = b.header("authorization", format!("Bearer {t}"));
            }
            let body = match body {
                Some(v) => {
                    b = b.header("content-type", "application/json");
                    Body::from(v.to_string())
                }
                None => Body::empty(),
            };
            let resp = app.oneshot(b.body(body).unwrap()).await.unwrap();
            let status = resp.status();
            let text = body_string(resp).await;
            (
                status,
                serde_json::from_str(&text).unwrap_or(Value::String(text)),
            )
        }
    };
    // No token: 401. Console: reads only. Harness: everything under the prefix.
    let (status, _) = call(None, Method::GET, "/_rustybin/harness", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let console = control_jwt(Some("console"));
    let (status, _) = call(
        Some(console.clone()),
        Method::GET,
        "/_rustybin/harness/runs",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, v) = call(
        Some(console),
        Method::POST,
        "/_rustybin/harness/runs",
        Some(smoke(&url, 1, "1s")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    let harness = control_jwt(Some("harness"));
    let (status, v) = call(
        Some(harness.clone()),
        Method::GET,
        "/_rustybin/config",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    let (status, run) = call(
        Some(harness.clone()),
        Method::POST,
        "/_rustybin/harness/runs",
        Some(smoke(&url, 20, "1s")),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    let (status, _) = call(
        Some(harness.clone()),
        Method::POST,
        &format!("/_rustybin/harness/runs/{id}/stop"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    // The admin token works too.
    let (status, _) = call(
        Some("admin-secret".into()),
        Method::POST,
        "/_rustybin/harness/stop",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The verification path is data plane: never authenticated.
    let (status, _) = call(None, Method::GET, "/.well-known/rustybin-harness", None).await;
    assert_eq!(status, StatusCode::OK);
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn open_control_plane_with_an_admin_token_guards_mutations() {
    let mut config = harness_config(&[Role::Client, Role::Server], &["127.0.0.1"]);
    config.admin_token = Some("s3cret".into());
    let (server, app, _state) = running(config).await;
    let url = format!("http://{}", server.http_addr);
    let (status, _) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(smoke(&url, 1, "1s")),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&app, Method::GET, "/_rustybin/harness", None).await;
    assert_eq!(status, StatusCode::OK);
    let req = Request::builder()
        .method(Method::POST)
        .uri("/_rustybin/harness/stop")
        .header("x-rustybin-admin-token", "s3cret")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn data_plane_off_shrinks_a_client_node() {
    let mut config = harness_config(&[Role::Client], &["127.0.0.1"]);
    config.harness.data_plane = false;
    let app = test_app_with(AppState::for_tests(config));
    for (path, expected) in [
        ("/", StatusCode::OK),
        ("/_rustybin/harness", StatusCode::OK),
        ("/_rustybin/ready", StatusCode::OK),
        ("/ui/", StatusCode::OK),
        ("/.well-known/rustybin-harness", StatusCode::OK),
        ("/echo", StatusCode::NOT_FOUND),
        ("/status/200", StatusCode::NOT_FOUND),
        ("/mock/petstore/v1/pets", StatusCode::NOT_FOUND),
    ] {
        let (status, _) = send(&app, Method::GET, path, None).await;
        assert_eq!(status, expected, "{path}");
    }
}

#[tokio::test]
async fn headless_run_returns_the_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = harness_config(&[Role::Client, Role::Server], &["127.0.0.1"]);
    config.harness.report_dir = Some(dir.path().join("reports"));
    let state = AppState::for_tests(config);
    let server = crate::server::start_with_state(state.clone())
        .await
        .unwrap();
    let url = format!("http://{}", server.http_addr);
    let path = dir.path().join("smoke.yaml");
    std::fs::write(&path, format!("name: headless\ntarget: {url}\nsource: {{ request: {{ path: /status/200 }} }}\npattern: {{ type: constant, rate: 20, for: 1s }}\nthresholds: {{ error_rate: 0 }}\n")).unwrap();
    let report_file = dir.path().join("report.json");
    // Headless settings live on the node's harness config; build a harness
    // that has them (the real server reads them from the environment).
    let mut hc = state.config.harness.clone();
    hc.scenario = Some(path.clone());
    hc.start = StartMode::Immediately;
    hc.report = Some(report_file.display().to_string());
    hc.exit = true;
    assert!(hc.headless());
    let h = Harness::from_harness_config(&hc, "headless-node", None);
    let code = super::run_headless(&h).await;
    assert_eq!(code, 0);
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&report_file).unwrap()).unwrap();
    assert_eq!(written["state"], "finished");
    assert_eq!(written["node_id"], "headless-node");
    // The report directory got a copy too.
    let entries: Vec<_> = std::fs::read_dir(dir.path().join("reports"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(entries.len(), 1);
    // A missing scenario file is an error (2).
    hc.scenario = Some(dir.path().join("missing.yaml"));
    let h = Harness::from_harness_config(&hc, "headless-node", None);
    assert_eq!(super::run_headless(&h).await, 2);
    // A failing threshold is 1.
    std::fs::write(&path, format!("name: failing\ntarget: {url}\nsource: {{ request: {{ path: /status/500 }} }}\npattern: {{ type: constant, rate: 20, for: 1s }}\nthresholds: {{ error_rate: 0 }}\n")).unwrap();
    hc.scenario = Some(path);
    hc.report = None;
    let h = Harness::from_harness_config(&hc, "headless-node", None);
    assert_eq!(super::run_headless(&h).await, 1);
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn runs_are_bounded() {
    let (server, app, state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let url = format!("http://{}", server.http_addr);
    let harness: Arc<Harness> = state.harness.clone().unwrap();
    for _ in 0..(super::MAX_RUNS_KEPT + 3) {
        let mut s = Scenario::from_value(&smoke(&url, 10, "100ms")).unwrap();
        s.normalise();
        let run = harness.create_run(s).await.expect("run");
        run.wait().await;
    }
    assert_eq!(harness.runs().len(), super::MAX_RUNS_KEPT);
    let (_, list) = send(&app, Method::GET, "/_rustybin/harness/runs", None).await;
    assert_eq!(list["runs"].as_array().unwrap().len(), super::MAX_RUNS_KEPT);
    server.shutdown();
    let _ = server.wait().await;
}

// ── Phase 2: correlation ────────────────────────────────────────────

use super::test_proxy::{self, ProxyConfig};

fn marked(url: &str, rate: u64, secs: &str, path: &str) -> Value {
    json!({
        "name": "correlated",
        "target": url,
        "source": { "request": { "path": path, "headers": { "X-Demo": "{{seq}}", "Accept": "application/json" } } },
        "pattern": { "type": "constant", "rate": rate, "for": secs },
        "timeout": "2s",
        "servers": { "capture_sample": 10 }
    })
}

#[tokio::test]
async fn loop_through_tags_every_answer_and_measures_the_legs() {
    let (server, app, state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let url = format!("http://{}", server.http_addr);
    state.inspector.clear();
    let (status, run) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(marked(&url, 100, "1s", "/status/200")),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    let report = wait_for_report(&app, &id).await;
    let t = &report["totals"];
    let sent = t["sent"].as_u64().unwrap();
    assert_eq!(t["upstream"], sent, "{t}");
    assert_eq!(t["gateway"], 0);
    assert_eq!(t["transport"], 0);
    assert_eq!(t["lost"], 0);
    // Upstream timing and the overhead (gateway plus network) exist.
    assert_eq!(report["upstream_latency"]["count"], sent);
    assert_eq!(report["overhead"]["count"], sent);
    assert!(report["overhead"]["p50_ms"].as_f64().unwrap() >= 0.0);
    assert!(
        report["upstream_latency"]["p99_ms"].as_f64().unwrap()
            < report["service_latency"]["p99_ms"].as_f64().unwrap() + 0.001
    );
    // One clock: both legs are measured.
    assert_eq!(report["request_leg"]["count"], sent);
    assert_eq!(report["response_leg"]["count"], sent);
    // The server side saw everything, nothing changed on the way.
    let server_side = &report["server"];
    assert_eq!(server_side["arrivals"], sent);
    assert_eq!(
        server_side["by_node"][&state.harness.as_ref().unwrap().node_id],
        sent
    );
    assert_eq!(server_side["by_route"]["/status/{code}"], sent);
    assert_eq!(server_side["changes"]["headers_changed"], 0);
    assert_eq!(server_side["changes"]["body_changed"], 0);
    assert_eq!(server_side["changes"]["path_changed"], 0);
    assert_eq!(server_side["changes"]["headers_added"], json!({}));
    assert_eq!(server_side["processing"]["count"], sent);
    assert_eq!(report["rejected_markers"], 0);
    // Inspector sampling: 1 in 10 (no anomalies), not every request.
    let captured = server_side["captured"].as_u64().unwrap();
    assert!(
        (sent / 10).saturating_sub(2) <= captured && captured <= sent / 10 + 2,
        "captured {captured} of {sent}"
    );
    assert!(
        state.inspector.len() as u64 <= captured + 5,
        "inspector holds {}",
        state.inspector.len()
    );
    assert_eq!(report["samples"][0]["class"], "upstream");
    assert!(report["samples"][0]["upstream_ms"].is_number());
    // Live view and metrics carry the classes.
    let (_, live) = send(
        &app,
        Method::GET,
        &format!("/_rustybin/harness/runs/{id}"),
        None,
    )
    .await;
    assert_eq!(live["upstream"], sent);
    assert_eq!(live["server_arrivals"], sent);
    let metrics = state.metrics.render();
    assert!(metrics.contains(&format!("rustybin_harness_requests_total{{run=\"{id}\",class=\"upstream\",status=\"200\"}} {sent}")), "{metrics}");
    assert!(metrics.contains(&format!(
        "rustybin_harness_server_arrivals_total{{run=\"{id}\"}} {sent}"
    )));
    assert!(metrics.contains(&format!(
        "rustybin_harness_latency_seconds_count{{run=\"{id}\",kind=\"overhead\"}} {sent}"
    )));
    assert!(metrics.contains(&format!("rustybin_harness_inflight{{run=\"{id}\"}} 0")));
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn through_a_gateway_every_class_and_lost_are_counted_exactly() {
    let (server, app, _state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    // The gateway adds and removes headers, delays 20 ms, answers every
    // 5th request itself with 429, and swallows every 7th.
    let proxy = test_proxy::start(
        server.http_addr,
        ProxyConfig {
            add_request_headers: vec![
                ("via".into(), "1.1 test-proxy".into()),
                ("x-forwarded-for".into(), "203.0.113.9".into()),
            ],
            remove_request_headers: vec!["accept".into()],
            rewrite_request_headers: vec![("x-demo".into(), "rewritten".into())],
            delay: Duration::from_millis(20),
            own_answer_every: Some((5, 429)),
            drop_every: Some(7),
            ..ProxyConfig::default()
        },
    )
    .await;
    let mut scenario = marked(&proxy.url(), 70, "1s", "/status/200");
    scenario["thresholds"] =
        json!({ "lost": 0, "upstream_reach": 0.6, "gateway_overhead_p99_ms": 1000 });
    let (status, run) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(scenario),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    let t = &report["totals"];
    let sent = t["sent"].as_u64().unwrap();
    let own = proxy
        .counters
        .own_answers
        .load(std::sync::atomic::Ordering::Relaxed);
    let dropped = proxy
        .counters
        .dropped
        .load(std::sync::atomic::Ordering::Relaxed);
    let forwarded = proxy
        .counters
        .forwarded
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(t["gateway"], own, "{t}");
    assert_eq!(t["gateway_statuses"]["429"], own);
    assert_eq!(t["upstream"], forwarded, "{t}");
    assert_eq!(t["transport"], dropped);
    assert_eq!(t["lost"], dropped, "swallowed requests are lost: {t}");
    assert_eq!(t["transport_after_upstream"], 0);
    assert_eq!(t["upstream"].as_u64().unwrap() + own + dropped, sent);
    assert_eq!(t["transport_kinds"]["timeout"], dropped);
    // The overhead is the proxy's 20 ms delay (plus a little network).
    let overhead = &report["overhead"];
    assert_eq!(overhead["count"], forwarded);
    let p50 = overhead["p50_ms"].as_f64().unwrap();
    assert!((19.0..=60.0).contains(&p50), "overhead p50 {p50} ms");
    assert!(report["upstream_latency"]["p50_ms"].as_f64().unwrap() < 10.0);
    // Legs are not measured through a gateway... but the node is the same, so they are.
    assert_eq!(report["request_leg"]["count"], forwarded);
    // The server side: what the gateway changed.
    let srv = &report["server"];
    assert_eq!(srv["arrivals"], forwarded);
    let changes = &srv["changes"];
    assert_eq!(changes["headers_changed"], forwarded, "{changes}");
    assert_eq!(changes["headers_missing"]["accept"], forwarded);
    assert_eq!(changes["headers_added"]["via"], forwarded);
    assert_eq!(changes["headers_added"]["x-forwarded-for"], forwarded);
    assert_eq!(changes["body_changed"], 0);
    assert_eq!(changes["path_changed"], 0);
    // Every anomaly is captured.
    assert_eq!(srv["captured"], forwarded);
    // Thresholds: lost failed, reach passed (every 7th swallowed), overhead passed.
    let th: std::collections::BTreeMap<&str, bool> = report["thresholds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (x["name"].as_str().unwrap(), x["pass"].as_bool().unwrap()))
        .collect();
    assert_eq!(th["lost"], dropped == 0);
    assert!(th["upstream_reach"], "{:?}", report["thresholds"]);
    assert!(th["gateway_overhead_p99_ms"]);
    let (_, text) = send(
        &app,
        Method::GET,
        &format!(
            "/_rustybin/harness/runs/{}/report?format=text",
            run["run_id"].as_str().unwrap()
        ),
        None,
    )
    .await;
    let text = text.as_str().unwrap();
    assert!(
        text.contains("Classes:")
            && text.contains("Server side:")
            && text.contains("gateway changed:"),
        "{text}"
    );
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn a_gateway_that_strips_the_tag_or_rewrites_the_path_is_seen() {
    let (server, app, _state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let proxy = test_proxy::start(
        server.http_addr,
        ProxyConfig {
            strip_response_headers: vec![
                super::correlate::SERVED_BY.into(),
                "server-timing".into(),
            ],
            path_prefix: "/echo".into(),
            ..ProxyConfig::default()
        },
    )
    .await;
    let scenario = json!({
        "name": "stripped",
        "target": proxy.url(),
        "source": { "request": { "method": "POST", "path": "/orders", "body": { "n": "{{seq}}" } } },
        "pattern": { "type": "constant", "rate": 40, "for": "1s" },
        "timeout": "2s"
    });
    let (status, run) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(scenario),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    let t = &report["totals"];
    let sent = t["sent"].as_u64().unwrap();
    // Without the tag every answer reads as the gateway's, even with a 200.
    assert_eq!(t["gateway"], sent, "{t}");
    assert_eq!(t["upstream"], 0);
    assert_eq!(t["gateway_statuses"]["200"], sent);
    assert_eq!(report["overhead"]["count"], 0);
    // The server side still saw them, with the path rewritten and the body intact.
    let srv = &report["server"];
    assert_eq!(srv["arrivals"], sent);
    assert_eq!(srv["changes"]["path_changed"], sent);
    assert_eq!(srv["changes"]["body_changed"], 0);
    assert_eq!(srv["by_route"]["/echo/{*path}"], sent);
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn spoofed_markers_are_rejected_and_never_counted() {
    let (server, app, state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let harness = state.harness.clone().unwrap();
    let url = format!("http://{}", server.http_addr);
    let (status, run) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(marked(&url, 10, "1s", "/status/200")),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    // A marker signed with another key, one for an unknown run, and garbage.
    let other = super::correlate::RunKey::random();
    let mut m = super::correlate::Marker {
        run: id.clone(),
        node: "spoof".into(),
        seq: 999_999,
        vu: 0,
        layer: 0,
        sent_ns: 0,
        names: Vec::new(),
        headers_digest: "00000000".into(),
        body_digest: String::new(),
        path_digest: "00000000".into(),
        sig: String::new(),
    };
    m.sign(&other);
    for raw in [m.to_header(), "v1; run=nope; node=x; seq=1; vu=0; sent=0; h=; hd=00000000; b=; p=00000000; sig=00000000000000000000000000000000".to_string(), "garbage".to_string()] {
        let req = Request::builder()
            .method(Method::GET)
            .uri("/status/200")
            .header(super::correlate::HEADER, raw)
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get(super::correlate::SERVED_BY).is_none(), "a spoofed request is never tagged");
    }
    let report = wait_for_report(&app, &id).await;
    assert_eq!(report["rejected_markers"], 3);
    assert_eq!(report["server"]["arrivals"], report["totals"]["sent"]);
    assert!(report["server"]["by_node"].get("spoof").is_none());
    assert_eq!(harness.correlator.rejected(), 3);
    // A genuine marker of the finished run still verifies (the key is kept).
    let entry = harness.correlator.entry(&id).expect("entry kept");
    let mut genuine = m.clone();
    genuine.sign(&entry.key);
    assert!(genuine.verify(&entry.key));
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn the_shaped_endpoint_follows_the_run_profile() {
    let (server, app, _state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let url = format!("http://{}", server.http_addr);
    // Without a marker: the default profile.
    let (status, body) = send(&app, Method::GET, "/harness/orders/1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["shaped"], true);
    assert_eq!(body["run"], Value::Null);
    assert_eq!(body.to_string().len(), 256);
    let scenario = json!({
        "name": "shaped",
        "target": url,
        "source": { "request": { "path": "/harness/orders/{{seq}}", "expect_status": [200, 503] } },
        "pattern": { "type": "constant", "rate": 100, "for": "1s" },
        "timeout": "5s",
        "servers": {
            "latency": { "p50": "10ms", "p99": "40ms" },
            "status": { "200": 3, "503": 1 },
            "body_bytes": 2048,
            "chunked": true,
            "first_byte_delay": "1ms",
            "capture_sample": 0
        },
        "thresholds": { "error_rate": 0, "lost": 0 }
    });
    let (status, run) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(scenario),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    let t = &report["totals"];
    let sent = t["sent"].as_u64().unwrap();
    assert_eq!(t["upstream"], sent, "{t}");
    assert_eq!(t["ok"], sent, "both statuses were expected");
    let errors = t["statuses"]["503"].as_u64().unwrap_or(0);
    assert!((5..=50).contains(&errors), "503s: {errors} of {sent}");
    assert_eq!(t["bytes_received"], sent * 2048);
    let up = &report["upstream_latency"];
    assert!(
        (6.0..=20.0).contains(&up["p50_ms"].as_f64().unwrap()),
        "{up}"
    );
    assert!(up["p99_ms"].as_f64().unwrap() >= 20.0, "{up}");
    // capture_sample 0: only anomalies (the 503s) are captured.
    assert_eq!(
        report["server"]["captured"], errors,
        "{:?}",
        report["server"]
    );
    assert_eq!(report["passed"], true, "{:?}", report["thresholds"]);
    server.shutdown();
    let _ = server.wait().await;
}
