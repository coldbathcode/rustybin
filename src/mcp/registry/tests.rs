use std::collections::BTreeMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::test_support::{body_json, get_request, test_app_with, test_state_with};

fn server(name: &str, version: &str) -> Value {
    json!({
        "$schema": validate::SCHEMA_URL,
        "name": name,
        "description": "A server published by a test",
        "version": version,
        "remotes": [{ "type": "streamable-http", "url": "https://mcp.example.com/mcp", "headers": [{ "name": "X-Api-Key", "isRequired": true, "isSecret": true }] }]
    })
}

fn app() -> (Router, AppState) {
    let state = test_state_with(Config::for_tests());
    (test_app_with(state.clone()), state)
}

fn set_scenario(state: &AppState, id: &str) {
    let mut values = BTreeMap::new();
    values.insert("registry.scenario".to_string(), json!(id));
    state
        .settings
        .update(values, BTreeMap::new(), "test")
        .expect("scenario");
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(get_request(uri)).await.expect("r");
    let status = resp.status();
    let bytes = crate::test_support::body_bytes(resp).await;
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| panic!("{uri}: {status} {}", String::from_utf8_lossy(&bytes)));
    (status, v)
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: &Value,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    let resp = app
        .clone()
        .oneshot(b.body(Body::from(body.to_string())).expect("req"))
        .await
        .expect("r");
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn token(app: &Router, method: &str, body: Value) -> String {
    let (s, v) = send(
        app,
        "POST",
        &format!("{API_PREFIX}/auth/{method}"),
        None,
        &body,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{method}: {v}");
    assert!(v["expires_at"].as_i64().unwrap() > Utc::now().timestamp());
    v["registry_token"].as_str().expect("token").to_string()
}

fn names(v: &Value) -> Vec<String> {
    v["servers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["server"]["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn lists_this_instances_servers_and_mocks() {
    let (app, _) = app();
    let (s, v) = get(&app, &format!("{API_PREFIX}/servers?limit=100")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let n = names(&v);
    for want in [
        "io.rustybin/mcp",
        "io.rustybin/mcp-protected",
        "io.rustybin/mcp-apikey",
        "io.rustybin/mcp-sse",
        "io.rustybin/manage",
        "io.rustybin/mock-petstore",
    ] {
        assert!(n.contains(&want.to_string()), "{want} missing from {n:?}");
    }
    let mut sorted = n.clone();
    sorted.sort();
    assert_eq!(n, sorted, "ordered by name");
    assert_eq!(v["metadata"]["count"], n.len());
    assert!(v["metadata"].get("nextCursor").is_none());
    // Every generated entry is valid and points at this instance.
    for item in v["servers"].as_array().unwrap() {
        let check = validate::validate(&item["server"]);
        assert!(
            validate::errors(&check).is_empty(),
            "{}: {check}",
            item["server"]["name"]
        );
        assert_eq!(item["_meta"][entries::OFFICIAL_META]["status"], "active");
        assert_eq!(item["_meta"][entries::OFFICIAL_META]["isLatest"], true);
    }
    let (s, pet) = get(
        &app,
        &format!("{API_PREFIX}/servers/io.rustybin%2Fmock-petstore/versions/latest"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{pet}");
    let remote = &pet["server"]["remotes"][0];
    assert_eq!(remote["type"], "streamable-http");
    assert!(
        remote["url"]
            .as_str()
            .unwrap()
            .ends_with("/mock/petstore/_rustybin/mcp"),
        "{remote}"
    );
    assert_eq!(
        pet["server"]["_meta"][entries::PUBLISHER_META]["rustybin"]["path"],
        "/mock/petstore/_rustybin/mcp"
    );
    let (_, sse) = get(
        &app,
        &format!("{API_PREFIX}/servers/io.rustybin%2Fmcp-sse/versions/latest"),
    )
    .await;
    assert_eq!(sse["server"]["remotes"][0]["type"], "sse");
    let (_, key) = get(
        &app,
        &format!("{API_PREFIX}/servers/io.rustybin%2Fmcp-apikey/versions/latest"),
    )
    .await;
    assert_eq!(
        key["server"]["remotes"][0]["headers"][0]["name"],
        "X-API-Key"
    );
    // The version given and the versions list.
    let v = pet["server"]["version"].as_str().unwrap().to_string();
    let (s, _) = get(
        &app,
        &format!("{API_PREFIX}/servers/io.rustybin%2Fmock-petstore/versions/{v}"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, versions) = get(
        &app,
        &format!("{API_PREFIX}/servers/io.rustybin%2Fmock-petstore/versions"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(versions["metadata"]["count"], 1);
    let (s, e) = get(
        &app,
        &format!("{API_PREFIX}/servers/io.example%2Fnope/versions"),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(e["detail"], "Server not found");
    // The same under /v0.
    let (s, v0) = get(&app, "/mcp-registry/v0/servers?limit=2").await;
    assert_eq!(s, StatusCode::OK, "{v0}");
    assert_eq!(v0["metadata"]["count"], 2);
    let (s, _) = get(
        &app,
        "/mcp-registry/v0/servers/io.rustybin%2Fmcp/versions/latest",
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    for p in ["health", "ping", "version"] {
        let (s, _) = get(&app, &format!("{API_PREFIX}/{p}")).await;
        assert_eq!(s, StatusCode::OK, "{p}");
    }
    let (_, idx) = get(&app, ROOT_PATH).await;
    assert_eq!(idx["scenario"], "normal");
}

#[tokio::test]
async fn query_validation_and_search() {
    let (app, _) = app();
    for bad in [
        "limit=0",
        "limit=101",
        "limit=x",
        "search=ab",
        "updated_since=yesterday",
        "include_deleted=maybe",
    ] {
        let resp = app
            .clone()
            .oneshot(get_request(&format!("{API_PREFIX}/servers?{bad}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        assert_eq!(resp.headers()["content-type"], "application/problem+json");
    }
    let (_, v) = get(&app, &format!("{API_PREFIX}/servers?search=MOCK-pet")).await;
    assert_eq!(names(&v), vec!["io.rustybin/mock-petstore"]);
    let future = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let (_, v) = get(
        &app,
        &format!("{API_PREFIX}/servers?updated_since={}", urlenc(&future)),
    )
    .await;
    assert_eq!(v["metadata"]["count"], 0);
}

fn urlenc(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

#[tokio::test]
async fn publish_with_every_auth_stub() {
    let (app, _) = app();
    let doc = server("io.example/weather", "1.0.0");
    // No token, an unknown token.
    let (s, e) = send(&app, "POST", &format!("{API_PREFIX}/publish"), None, &doc).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{e}");
    let (s, _) = send(
        &app,
        "POST",
        &format!("{API_PREFIX}/publish"),
        Some("rbreg_nope"),
        &doc,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // The stubs, with their required fields.
    let bodies = [
        ("github-at", json!({ "github_token": "gho_x" })),
        ("github-oidc", json!({ "oidc_token": "ey" })),
        ("oidc", json!({ "oidc_token": "ey" })),
        (
            "dns",
            json!({ "domain": "example.com", "timestamp": "2026-10-30T00:00:00Z", "signed_timestamp": "ab" }),
        ),
        (
            "http",
            json!({ "domain": "example.com", "timestamp": "2026-10-30T00:00:00Z", "signed_timestamp": "ab" }),
        ),
        ("none", json!({})),
    ];
    for (i, (method, body)) in bodies.iter().enumerate() {
        let t = token(&app, method, body.clone()).await;
        let doc = server("io.example/weather", &format!("1.0.{i}"));
        let (s, v) = send(
            &app,
            "POST",
            &format!("{API_PREFIX}/publish"),
            Some(&t),
            &doc,
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{method}: {v}");
        assert_eq!(v["server"]["version"], format!("1.0.{i}"));
    }
    let (s, e) = send(
        &app,
        "POST",
        &format!("{API_PREFIX}/auth/github-at"),
        None,
        &json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{e}");
    let (s, _) = send(
        &app,
        "POST",
        &format!("{API_PREFIX}/auth/magic"),
        None,
        &json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Six versions, the last published is latest.
    let (_, versions) = get(
        &app,
        &format!("{API_PREFIX}/servers/io.example%2Fweather/versions"),
    )
    .await;
    assert_eq!(versions["metadata"]["count"], 6);
    let (_, latest) = get(
        &app,
        &format!("{API_PREFIX}/servers/io.example%2Fweather/versions/latest"),
    )
    .await;
    assert_eq!(latest["server"]["version"], "1.0.5");
    let t = token(&app, "none", json!({})).await;
    // Duplicate, invalid, reserved.
    let (s, _) = send(
        &app,
        "POST",
        &format!("{API_PREFIX}/publish"),
        Some(&t),
        &server("io.example/weather", "1.0.0"),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, e) = send(
        &app,
        "POST",
        &format!("{API_PREFIX}/publish"),
        Some(&t),
        &json!({ "name": "x" }),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        e["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["location"] == "body.name"),
        "{e}"
    );
    let (s, _) = send(
        &app,
        "POST",
        &format!("{API_PREFIX}/publish"),
        Some(&t),
        &server("io.rustybin/mcp", "9.9.9"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // The published entries show in the console's view and can be removed.
    let (_, st) = get(&app, STATUS_PATH).await;
    assert_eq!(st["counts"]["published"], 6);
    assert!(st["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["source"] == "published"));
    let (s, d) = send(
        &app,
        "DELETE",
        "/_rustybin/registry/servers/io.example%2Fweather?version=1.0.0",
        None,
        &json!({}),
    )
    .await;
    assert_eq!((s, d["versions"].as_u64()), (StatusCode::OK, Some(1)));
    let (s, d) = send(
        &app,
        "DELETE",
        "/_rustybin/registry/servers/io.example%2Fweather",
        None,
        &json!({}),
    )
    .await;
    assert_eq!((s, d["versions"].as_u64()), (StatusCode::OK, Some(5)));
    let (s, _) = send(
        &app,
        "DELETE",
        "/_rustybin/registry/servers/io.example%2Fweather",
        None,
        &json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn status_changes_and_deleted_entries() {
    let (app, _) = app();
    let t = token(&app, "none", json!({})).await;
    for v in ["1.0.0", "2.0.0"] {
        let (s, _) = send(
            &app,
            "POST",
            &format!("{API_PREFIX}/publish"),
            Some(&t),
            &server("io.example/sync", v),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
    }
    let before_changes = Utc::now();
    let path = format!("{API_PREFIX}/servers/io.example%2Fsync/versions/1.0.0/status");
    let (s, v) = send(
        &app,
        "PATCH",
        &path,
        Some(&t),
        &json!({ "status": "deprecated", "statusMessage": "use 2.0.0" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["_meta"][entries::OFFICIAL_META]["statusMessage"],
        "use 2.0.0"
    );
    let (s, _) = send(&app, "PATCH", &path, Some(&t), &json!({ "status": "gone" })).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = send(&app, "PATCH", &path, None, &json!({ "status": "active" })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Generated entries follow the instance.
    let (s, _) = send(
        &app,
        "PATCH",
        &format!("{API_PREFIX}/servers/io.rustybin%2Fmcp/status"),
        Some(&t),
        &json!({ "status": "deleted" }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = send(
        &app,
        "PATCH",
        &format!("{API_PREFIX}/servers/io.example%2Fsync/status"),
        Some(&t),
        &json!({ "status": "deleted" }),
    )
    .await;
    assert_eq!((s, v["updatedCount"].as_u64()), (StatusCode::OK, Some(2)));
    // Deleted entries leave the list unless asked for, and always show to updated_since.
    let (_, list) = get(&app, &format!("{API_PREFIX}/servers?search=example")).await;
    assert_eq!(list["metadata"]["count"], 0);
    let (_, list) = get(
        &app,
        &format!("{API_PREFIX}/servers?search=example&include_deleted=true"),
    )
    .await;
    assert_eq!(list["metadata"]["count"], 2);
    let since = urlenc(&before_changes.to_rfc3339());
    let (_, list) = get(&app, &format!("{API_PREFIX}/servers?updated_since={since}")).await;
    assert_eq!(names(&list), vec!["io.example/sync", "io.example/sync"]);
    let (s, _) = get(
        &app,
        &format!("{API_PREFIX}/servers/io.example%2Fsync/versions"),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn scenarios_break_the_sync_on_purpose() {
    let (app, state) = app();
    set_scenario(&state, "empty");
    let (_, v) = get(&app, &format!("{API_PREFIX}/servers")).await;
    assert_eq!(v["metadata"]["count"], 0);

    set_scenario(&state, "large");
    let mut url = format!("{API_PREFIX}/servers?limit=100&version=latest");
    let mut total = 0;
    let mut pages = 0;
    loop {
        let (s, v) = get(&app, &url).await;
        assert_eq!(s, StatusCode::OK);
        total += v["metadata"]["count"].as_u64().unwrap();
        pages += 1;
        match v["metadata"]["nextCursor"].as_str() {
            Some(c) => {
                url = format!(
                    "{API_PREFIX}/servers?limit=100&version=latest&cursor={}",
                    urlenc(c)
                )
            }
            None => break,
        }
    }
    assert!(total as usize > entries::LARGE_COUNT, "{total}");
    assert!(pages >= 21, "{pages}");
    let (_, st) = get(&app, STATUS_PATH).await;
    assert_eq!(
        st["counts"]["scenario"],
        entries::LARGE_COUNT + entries::LARGE_COUNT / 10
    );
    assert!(
        st["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["source"] != "scenario"),
        "synthetic entries are counted, not listed"
    );

    set_scenario(&state, "flaky");
    let mut statuses = Vec::new();
    for _ in 0..6 {
        let (s, _) = get(&app, &format!("{API_PREFIX}/servers?limit=1")).await;
        statuses.push(s.as_u16());
    }
    assert_eq!(
        statuses.iter().filter(|s| **s == 503).count(),
        2,
        "{statuses:?}"
    );
    let (s, _) = get(
        &app,
        &format!("{API_PREFIX}/servers/io.rustybin%2Fmcp/versions/latest"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "only lists are flaky");

    set_scenario(&state, "stale");
    let (_, v) = get(&app, &format!("{API_PREFIX}/servers?limit=1")).await;
    assert_eq!(
        v["servers"][0]["_meta"][entries::OFFICIAL_META]["updatedAt"],
        "2024-01-15T09:30:00.000Z"
    );
    let (_, v) = get(
        &app,
        &format!("{API_PREFIX}/servers?updated_since=2025-01-01T00:00:00Z"),
    )
    .await;
    assert_eq!(v["metadata"]["count"], 0);

    set_scenario(&state, "invalid");
    let (_, st) = get(&app, STATUS_PATH).await;
    let bad: Vec<&Value> = st["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["valid"] == false)
        .collect();
    assert_eq!(bad.len(), 1, "{st}");

    set_scenario(&state, "auth_fail");
    let (s, e) = send(
        &app,
        "POST",
        &format!("{API_PREFIX}/auth/dns"),
        None,
        &json!({ "domain": "example.com", "timestamp": "t", "signed_timestamp": "s" }),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{e}");
    let (s, _) = send(
        &app,
        "POST",
        &format!("{API_PREFIX}/publish"),
        Some("rbreg_x"),
        &server("io.example/a1", "1"),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    set_scenario(&state, "slow");
    let started = std::time::Instant::now();
    let (s, _) = get(&app, &format!("{API_PREFIX}/ping")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(started.elapsed() >= SLOW_DELAY);
}

#[tokio::test]
async fn public_mode_needs_the_admin_token_to_publish() {
    let mut c = Config::for_tests();
    c.public_mode = true;
    c.admin_token = Some("adm".into());
    let app = test_app_with(test_state_with(c));
    let t = token(&app, "none", json!({})).await;
    let (s, _) = send(
        &app,
        "POST",
        &format!("{API_PREFIX}/publish"),
        Some(&t),
        &server("io.example/pub", "1.0.0"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let req = Request::builder()
        .method("POST")
        .uri(format!("{API_PREFIX}/publish"))
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {t}"))
        .header("x-rustybin-admin-token", "adm")
        .body(Body::from(server("io.example/pub", "1.0.0").to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn validate_routes() {
    let (app, _) = app();
    for path in [format!("{API_PREFIX}/validate"), VALIDATE_PATH.to_string()] {
        let (s, v) = send(&app, "POST", &path, None, &server("io.example/ok", "1.0.0")).await;
        assert_eq!(
            (s, &v["valid"]),
            (StatusCode::OK, &json!(true)),
            "{path}: {v}"
        );
        let (s, v) = send(&app, "POST", &path, None, &json!({ "name": "bad" })).await;
        assert_eq!(
            (s, &v["valid"]),
            (StatusCode::OK, &json!(false)),
            "{path}: {v}"
        );
        assert!(!v["issues"].as_array().unwrap().is_empty());
    }
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(VALIDATE_PATH)
                .body(Body::from("not json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn file_entries_are_validated_and_bounded() {
    let mut servers = vec![
        server("io.example/a1", "1.0.0"),
        server("io.example/a1", "1.0.0"),
        server("io.rustybin/mine", "1.0.0"),
        json!({ "name": "broken" }),
    ];
    for i in 0..300 {
        servers.push(server(&format!("io.example/s{i:03}"), "1.0.0"));
    }
    let (ok, problems) = file_entries(Some(servers));
    assert_eq!(ok.len(), MAX_USER_ENTRIES);
    assert!(
        problems.iter().any(|p| p.contains("duplicate")),
        "{problems:?}"
    );
    assert!(problems.iter().any(|p| p.contains("namespace")));
    assert!(problems.iter().any(|p| p.contains("registry.servers[3]")));
    assert!(problems.iter().any(|p| p.contains("more than 256")));
}

#[tokio::test]
async fn the_config_file_adds_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("rustybin.yaml");
    let doc = json!({
        "version": 1,
        "mocks": [],
        "registry": { "servers": [server("io.example/from-file", "3.1.4"), { "name": "nope" }] }
    });
    std::fs::write(&path, serde_yaml::to_string(&doc).unwrap()).unwrap();
    let mut c = Config::for_tests();
    c.config_file = Some(path.display().to_string());
    let state = test_state_with(c);
    crate::mocks::load_startup(&state);
    assert!(
        state
            .mocks
            .load_errors()
            .iter()
            .any(|e| e.contains("registry.servers[1]")),
        "{:?}",
        state.mocks.load_errors()
    );
    let app = test_app_with(state);
    let (s, v) = get(
        &app,
        &format!("{API_PREFIX}/servers/io.example%2Ffrom-file/versions/3.1.4"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, st) = get(&app, STATUS_PATH).await;
    assert_eq!(st["counts"]["file"], 1);
    // File entries change with the file, not the status API.
    let t = token(&app, "none", json!({})).await;
    let (s, _) = send(
        &app,
        "PATCH",
        &format!("{API_PREFIX}/servers/io.example%2Ffrom-file/status"),
        Some(&t),
        &json!({ "status": "deprecated" }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn browse_this_instance_in_process() {
    let (app, _) = app();
    let (s, v) = get(
        &app,
        &format!("{BROWSE_PATH}?url=/mcp-registry&limit=3&version=latest"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["registry"], "/mcp-registry/v0.1");
    assert_eq!(v["count"], 3);
    assert_eq!(v["cached"], false);
    assert!(v["next_cursor"].is_string());
    let first = &v["servers"][0];
    assert_eq!(first["valid"], true);
    assert!(first["remotes"][0]["url"].is_string());
    let (_, again) = get(
        &app,
        &format!("{BROWSE_PATH}?url=/mcp-registry&limit=3&version=latest"),
    )
    .await;
    assert_eq!(again["cached"], true);
    let (s, _) = get(&app, &format!("{BROWSE_PATH}?url=/mcp")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

/// A second, real Rustybin on an ephemeral port.
async fn second_instance() -> (crate::server::RunningServer, String) {
    let mut c = Config::for_tests();
    c.instance_id = "upstream-registry".into();
    let server = crate::server::start_with_state(crate::state::AppState::for_tests(c))
        .await
        .expect("start");
    let base = format!("http://{}", server.http_addr);
    (server, base)
}

#[tokio::test]
async fn browse_another_instance_through_the_egress_policy() {
    let (upstream, base) = second_instance().await;
    let url = format!(
        "{BROWSE_PATH}?url={}&search=mock-pet",
        urlenc(&format!("{base}/mcp-registry"))
    );
    // Not in the allowlist: refused.
    let (app, _) = app();
    let (s, v) = get(&app, &url).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["reason"], "egress_refused");
    // Saved (its host is allowed for browsing), but loopback stays off.
    let mut c = Config::for_tests();
    c.registry_saved = vec![format!("{base}/mcp-registry")];
    let app = test_app_with(test_state_with(c));
    let (s, v) = get(&app, &url).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert!(v["error"].as_str().unwrap().contains("loopback"), "{v}");
    // With the loopback switch.
    let mut c = Config::for_tests();
    c.registry_saved = vec![format!("{base}/mcp-registry")];
    c.egress_allow_loopback = true;
    let app = test_app_with(test_state_with(c));
    let (s, v) = get(&app, &url).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["count"], 1);
    let entry = &v["servers"][0];
    assert_eq!(entry["name"], "io.rustybin/mock-petstore");
    assert_eq!(
        entry["remotes"][0]["url"],
        format!("{base}/mock/petstore/_rustybin/mcp")
    );
    // An upstream error is a 502 with its status.
    let (s, v) = get(
        &app,
        &format!(
            "{BROWSE_PATH}?url={}&search=ab",
            urlenc(&format!("{base}/mcp-registry"))
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{v}");
    assert_eq!(v["status"], 422);
    upstream.shutdown();
}

#[test]
fn scenario_ids_match_the_table() {
    assert_eq!(
        scenario_ids(),
        SCENARIOS.iter().map(|(id, _)| *id).collect::<Vec<_>>()
    );
    for id in scenario_ids() {
        assert_eq!(Scenario::parse(id).map(Scenario::id), Some(id));
    }
}

#[test]
fn the_scenario_setting_has_a_select() {
    let spec = specs()
        .into_iter()
        .find(|s| s.key == "registry.scenario")
        .unwrap();
    assert_eq!(spec.kind.schema()["enum"], json!(SCENARIO_IDS));
    assert!(spec.kind.validate(&json!("large")).is_ok());
    assert!(spec.kind.validate(&json!("huge")).is_err());
    let saved = specs()
        .into_iter()
        .find(|s| s.key == "registry.saved")
        .unwrap();
    assert!(saved.validate.unwrap()(&json!(["https://r.example", "/mcp-registry"])).is_ok());
    assert!(saved.validate.unwrap()(&json!(["ftp://r.example"])).is_err());
}

#[test]
fn the_mint_is_bounded() {
    let reg = Registry::default();
    for _ in 0..(MAX_TOKENS + 20) {
        reg.mint();
    }
    assert_eq!(reg.tokens.lock().unwrap().len(), MAX_TOKENS);
}

#[tokio::test]
async fn the_server_json_task_writes_a_valid_entry() {
    use crate::assist::tasks::{explain, server_json};
    let (app, _) = app();
    let (_, pet) = get(
        &app,
        &format!("{API_PREFIX}/servers/io.rustybin%2Fmock-petstore/versions/latest"),
    )
    .await;
    let inputs = json!({ "draft": pet["server"], "tools": [{ "name": "getPetById", "description": "Find a pet" }, { "name": "addPet" }] });
    assert!((server_json::TASK.prompt)(&inputs)
        .unwrap()
        .contains(validate::SCHEMA_URL));
    let answer = (server_json::TASK.mock)(&inputs);
    let checked =
        (server_json::TASK.post.unwrap())(&inputs, answer).expect("the mock answer validates");
    assert!(checked["server"]["description"]
        .as_str()
        .unwrap()
        .contains("getPetById"));
    assert!(checked["server"].get("_meta").is_none());
    // A broken answer is sent back for repair with the schema's errors.
    let err = (server_json::TASK.post.unwrap())(&inputs, json!({ "server": { "name": "x" } }))
        .unwrap_err();
    assert!(err.contains("name"), "{err}");
    assert!((server_json::TASK.prompt)(&json!({})).is_err());
    // Explain knows registry entries.
    let e = (explain::TASK.mock)(
        &json!({ "kind": "server_entry", "subject": { "name": "io.example/weather" } }),
    );
    assert!(e["summary"]
        .as_str()
        .unwrap()
        .contains("io.example/weather"));
}
