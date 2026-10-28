//! Phase 6 tests (plan section 15): mock-driven traffic and identities. The
//! done-when test runs the petstore mock through the test proxy and checks
//! that every operation was exercised and the report breaks the answers down
//! per credential; the others cover invalid requests and create-then-read
//! against a stateful mock, minted JWTs the IdP key verifies, OAuth client
//! credentials against the built-in IdP, forwarded addresses, source
//! addresses, and what a node refuses before it starts.

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};

use super::test_proxy::{self, ProxyConfig};
use super::tests::{harness_config, running, send, wait_for_report};
use super::Role;

/// A loop-through node behind a proxy: the report of `scenario` (its
/// targets set to the proxy), the proxy, and the node's state.
async fn through(
    proxy: ProxyConfig,
    setup: impl AsyncFnOnce(&axum::Router),
    mut scenario: Value,
) -> (Value, test_proxy::Proxy, crate::state::AppState) {
    let (server, app, state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    setup(&app).await;
    let proxy = test_proxy::start(server.http_addr, proxy).await;
    scenario["targets"] = json!([proxy.url()]);
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
    (report, proxy, state)
}

async fn nothing(_: &axum::Router) {}

fn rows<'a>(report: &'a Value, kind: &str) -> &'a Vec<Value> {
    report[kind]
        .as_array()
        .unwrap_or_else(|| panic!("no {kind} in the report: {report}"))
}

#[tokio::test]
async fn petstore_through_the_proxy_exercises_every_operation_per_credential() {
    let (report, proxy, state) = through(
        ProxyConfig {
            record_header: Some("apikey".into()),
            ..ProxyConfig::default()
        },
        nothing,
        json!({
            "name": "petstore",
            "layers": [{
                "name": "customers",
                "source": { "mock": "petstore" },
                "identities": { "api_keys": { "mint": 5 }, "forwarded": { "count": 5 } },
                "pattern": { "type": "constant", "rate": 60, "for": "2s" }
            }]
        }),
    )
    .await;
    assert_eq!(report["state"], "finished", "{}", report["error"]);
    let ops = rows(&report, "operations");
    let live = state.mocks.get("petstore").unwrap().live.unwrap();
    for op in &live.spec.operations {
        let row = ops
            .iter()
            .find(|r| r["name"] == op.id.as_str())
            .unwrap_or_else(|| panic!("{} was not exercised: {ops:?}", op.id));
        assert!(row["answered"].as_u64().unwrap() > 0, "{row}");
        assert_eq!(row["layer"], "customers");
    }
    // Valid requests the mock answers as documented (a pet id of 0 is the
    // sample's own 404).
    let ok: u64 = ops.iter().map(|r| r["ok"].as_u64().unwrap()).sum();
    let answered: u64 = ops.iter().map(|r| r["answered"].as_u64().unwrap()).sum();
    assert!(ok * 10 >= answered * 9, "{ok} of {answered} ok: {ops:?}");
    // Five credentials, each with its share, named without the key itself.
    let creds = rows(&report, "credentials");
    assert_eq!(creds.len(), 5, "{creds:?}");
    for c in creds {
        assert!(
            c["name"].as_str().unwrap().starts_with("harness-key-"),
            "{c}"
        );
        assert!(c["answered"].as_u64().unwrap() >= 15, "{c}");
    }
    // The gateway saw the keys.
    let seen = proxy.counters.recorded.lock().unwrap().clone();
    assert!(
        seen.iter().all(|k| k.starts_with("harness-key-")),
        "{:?}",
        &seen[..5]
    );
    assert_eq!(
        seen.iter().collect::<std::collections::BTreeSet<_>>().len(),
        5
    );
}

#[tokio::test]
async fn invalid_requests_and_create_then_read_against_a_stateful_mock() {
    // A stateful petstore that enforces request validation.
    let setup = async |app: &axum::Router| {
        let (status, body) = send(
            app,
            Method::POST,
            "/_rustybin/mocks",
            Some(json!({
                "name": "pets",
                "spec": crate::mocks::SAMPLE_SPEC,
                "deploy": true,
                "responses": { "mode": "stateful", "validate_requests": "enforce" }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    };
    let (report, _proxy, _state) = through(
        ProxyConfig::default(),
        setup,
        json!({
            "name": "stateful",
            "layers": [{
                "name": "writers",
                "source": {
                    "mock": "pets",
                    "operations": { "include": ["createPet", "getPetById", "listPets"], "weights": { "createPet": 3 } },
                    "invalid_fraction": 0.2,
                    "stateful": true
                },
                "pattern": { "type": "constant", "rate": 40, "for": "2s" }
            }]
        }),
    )
    .await;
    assert_eq!(report["state"], "finished", "{}", report["error"]);
    let ops = rows(&report, "operations");
    let row = |name: &str| {
        ops.iter()
            .find(|r| r["name"] == name)
            .unwrap_or_else(|| panic!("no {name}: {ops:?}"))
    };
    // Every create was read back, and found.
    let creates = row("createPet");
    let reads = row("getPetById (after create)");
    assert!(creates["ok"].as_u64().unwrap() > 10, "{creates}");
    assert_eq!(reads["answered"], creates["ok"], "{reads} {creates}");
    assert_eq!(reads["ok"], reads["answered"], "{reads}");
    // Invalid requests were refused with a 4xx, as they expect.
    let invalid = row("createPet (invalid)");
    assert!(invalid["answered"].as_u64().unwrap() > 0, "{invalid}");
    assert_eq!(invalid["ok"], invalid["answered"], "{invalid}");
    // Item reads with ids nobody created accept a 404.
    assert_eq!(row("getPetById")["errors"], 0, "{ops:?}");
}

#[tokio::test]
async fn minted_jwts_verify_with_the_nodes_idp_key() {
    let (report, proxy, state) = through(
        ProxyConfig {
            record_header: Some("authorization".into()),
            ..ProxyConfig::default()
        },
        nothing,
        json!({
            "name": "jwt",
            "layers": [{
                "name": "users",
                "source": { "request": { "path": "/status/200" } },
                "identities": { "jwt": { "mint": 3, "audience": "orders", "scope": "orders:read" } },
                "pattern": { "type": "constant", "rate": 30, "for": "1s" }
            }]
        }),
    )
    .await;
    assert_eq!(report["state"], "finished", "{}", report["error"]);
    let seen = proxy.counters.recorded.lock().unwrap().clone();
    assert!(seen.len() >= 25, "{}", seen.len());
    let mut subjects = std::collections::BTreeSet::new();
    for bearer in &seen {
        let token = bearer.strip_prefix("Bearer ").expect("a bearer token");
        let claims = state
            .jwt
            .verify_rs256(token)
            .expect("signed by the node's IdP key");
        assert_eq!(claims["aud"], "orders");
        subjects.insert(claims["sub"].as_str().unwrap().to_string());
    }
    assert_eq!(subjects.len(), 3, "{subjects:?}");
    // Three tokens, one per subject: signed once, reused.
    assert_eq!(
        seen.iter().collect::<std::collections::BTreeSet<_>>().len(),
        3
    );
    let creds = rows(&report, "credentials");
    assert!(
        creds.iter().any(|c| c["name"] == "harness-user-2"),
        "{creds:?}"
    );
    assert!(!report["scenario"].to_string().contains("eyJ"));
}

#[tokio::test]
async fn oauth_client_credentials_against_the_built_in_idp() {
    let (server, app, state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let base = format!("http://{}", server.http_addr);
    let proxy = test_proxy::start(
        server.http_addr,
        ProxyConfig {
            record_header: Some("authorization".into()),
            ..ProxyConfig::default()
        },
    )
    .await;
    let scenario = |token_url: String, secret: &str| {
        json!({
            "name": "oauth",
            "targets": [proxy.url()],
            "timeout": "2s",
            "layers": [{
                "name": "services",
                "source": { "request": { "path": "/status/200" } },
                "identities": { "oauth": { "token_url": token_url, "clients": [{ "client_id": "rustybin", "client_secret": secret }], "audience": "orders" } },
                "pattern": { "type": "constant", "rate": 20, "for": "1s" }
            }]
        })
    };
    // A secret the IdP refuses: the run does not start.
    let (status, refused) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(scenario(format!("{base}/oauth/token"), "wrong")),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(
        refused["problems"][0]
            .as_str()
            .unwrap()
            .contains("answered 401"),
        "{refused}"
    );
    // A token URL outside the allowlist is refused too.
    let (status, refused) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(scenario("http://10.255.255.1/oauth/token".into(), "secret")),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    // The demo client: one token, fetched before the run, sent as a bearer.
    let (status, run) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/runs",
        Some(scenario(format!("{base}/oauth/token"), "secret")),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    assert_eq!(report["state"], "finished", "{}", report["error"]);
    let seen = proxy.counters.recorded.lock().unwrap().clone();
    assert!(seen.len() >= 15, "{}", seen.len());
    let token = seen[0].strip_prefix("Bearer ").unwrap();
    assert!(seen.iter().all(|s| *s == seen[0]), "one cached token");
    let claims = state.jwt.verify_rs256(token).unwrap();
    assert_eq!(
        claims["client_id"].as_str().or(claims["sub"].as_str()),
        Some("rustybin")
    );
    assert_eq!(rows(&report, "credentials")[0]["name"], "client rustybin");
    let text = report["scenario"].to_string();
    assert!(!text.contains("\"secret\""), "{text}");
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn forwarded_and_source_addresses() {
    let (report, proxy, _state) = through(
        ProxyConfig {
            record_header: Some("x-forwarded-for".into()),
            ..ProxyConfig::default()
        },
        nothing,
        json!({
            "name": "addresses",
            "layers": [{
                "name": "clients",
                "source": { "request": { "path": "/status/200", "headers": { "x-client": "{{ip}}" } } },
                "identities": { "forwarded": { "count": 4, "cidr": "203.0.113.0/24" }, "source_addresses": ["127.0.0.1"] },
                "pattern": { "type": "constant", "rate": 20, "for": "1s" }
            }]
        }),
    )
    .await;
    assert_eq!(report["state"], "finished", "{}", report["error"]);
    let seen: std::collections::BTreeSet<String> = proxy
        .counters
        .recorded
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .collect();
    let want: std::collections::BTreeSet<String> =
        (0..4).map(|i| format!("203.0.113.{i}")).collect();
    assert_eq!(seen, want);
    assert!(proxy
        .counters
        .peers
        .lock()
        .unwrap()
        .iter()
        .all(|p| p.to_string() == "127.0.0.1"));
    assert_eq!(rows(&report, "credentials").len(), 4);
}

#[tokio::test]
async fn a_node_refuses_what_it_cannot_send() {
    let (server, app, _state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let target = format!("http://{}", server.http_addr);
    let post = |scenario: Value| {
        send(
            &app,
            Method::POST,
            "/_rustybin/harness/runs",
            Some(scenario),
        )
    };
    // An address this host does not have.
    let (status, refused) = post(json!({
        "name": "x", "target": target,
        "layers": [{ "name": "l", "source": { "request": { "path": "/" } },
            "identities": { "source_addresses": ["192.0.2.1"] },
            "pattern": { "type": "constant", "rate": 1, "for": "1s" } }]
    }))
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(
        refused.to_string().contains("not an address of this host"),
        "{refused}"
    );
    // An unknown mock, and an operation the mock does not have: validation
    // says so before anything starts.
    for (source, says) in [
        (json!({ "mock": "nope" }), "no mock nope"),
        (
            json!({ "mock": "petstore", "operations": { "include": ["fly"] } }),
            "no operation fly",
        ),
    ] {
        let scenario = json!({
            "name": "x", "target": target,
            "layers": [{ "name": "l", "source": source, "pattern": { "type": "constant", "rate": 1, "for": "1s" } }]
        });
        let (status, v) = send(
            &app,
            Method::POST,
            "/_rustybin/harness/validate",
            Some(scenario.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["valid"], false, "{v}");
        assert!(v["problems"].to_string().contains(says), "{v}");
        let (status, refused) = post(scenario).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    }
    // A mock and templates at once, and jwt with oauth, are refused offline.
    let (_, v) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/validate",
        Some(json!({
            "name": "x", "target": target,
            "layers": [{ "name": "l",
                "source": { "mock": "petstore", "request": { "path": "/" } },
                "identities": { "jwt": {}, "oauth": { "token_url": target, "clients": [{ "client_id": "a", "client_secret": "b" }] } },
                "pattern": { "type": "constant", "rate": 1, "for": "1s" } }]
        })),
    )
    .await;
    let problems = v["problems"].to_string();
    assert!(problems.contains("not both"), "{problems}");
    assert!(problems.contains("jwt or oauth"), "{problems}");
    server.shutdown();
    let _ = server.wait().await;
}

#[tokio::test]
async fn a_cluster_merges_the_breakdowns_of_its_client_nodes() {
    use super::cluster_tests::{
        client_config, join, node, wait_for_alive, wait_for_cluster_report, TOKEN,
    };
    let l = node(harness_config(&[Role::Coordinator], &[])).await;
    let (status, cluster) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters",
        Some(json!({ "cluster_id": "mocks-1", "join_token": TOKEN })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{cluster}");
    let url = format!("{}/_rustybin/harness/clusters/mocks-1", l.base());
    let c1 = node(client_config(1000)).await;
    let c2 = node(client_config(1000)).await;
    let s1 = node(harness_config(&[Role::Server], &[])).await;
    let proxy = test_proxy::start(s1.server.http_addr, ProxyConfig::default()).await;
    for n in [&c1, &c2, &s1] {
        join(n, &url).await;
    }
    wait_for_alive(&l, "mocks-1", 2, 1).await;
    let (status, run) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters/mocks-1/runs",
        Some(json!({
            "name": "cluster-mocks",
            "target": proxy.url(),
            "layers": [{
                "name": "customers",
                "source": { "mock": "petstore", "operations": { "include": ["listPets", "getPetById"] } },
                "identities": { "api_keys": { "values": ["a", "b", "c"] } },
                "pattern": { "type": "constant", "rate": 40, "for": "1s" }
            }],
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
        &format!("/_rustybin/harness/clusters/mocks-1/runs/{run_id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let report = wait_for_cluster_report(&l, "mocks-1", &run_id).await;
    assert_eq!(report["state"], "finished", "{}", report["error"]);
    // Both nodes' answers, by operation and by credential, add up to the
    // run's.
    let completed = report["totals"]["completed"].as_u64().unwrap();
    for kind in ["operations", "credentials"] {
        let sum: u64 = rows(&report, kind)
            .iter()
            .map(|r| r["answered"].as_u64().unwrap())
            .sum();
        assert_eq!(sum, completed, "{kind}: {}", report[kind]);
    }
    assert_eq!(rows(&report, "operations").len(), 2);
    let creds: Vec<&str> = rows(&report, "credentials")
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(creds.len(), 3, "{creds:?}");
    assert!(creds.iter().all(|c| c.starts_with("key ")), "{creds:?}");
    assert!(!report["scenario"].to_string().contains("\"a\""));
    for n in [l, c1, c2, s1] {
        n.stop().await;
    }
}
