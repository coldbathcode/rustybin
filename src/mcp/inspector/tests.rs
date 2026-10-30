use std::time::Instant;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::test_support::{
    body_bytes, body_json, control_jwt, control_keys, json_request, test_app_with, test_state_with,
};

fn modern_discover(id: u64) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "server/discover", "params": { "_meta": {
        "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {} } } })
}

fn modern_headers(method: &str) -> Value {
    json!({ "MCP-Protocol-Version": "2026-07-28", "Mcp-Method": method })
}

/// Parse an SSE body into (event, data) pairs.
fn events(text: &str) -> Vec<(String, Value)> {
    text.split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .map(|block| {
            let mut ev = String::new();
            let mut data = String::new();
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("event: ") {
                    ev = v.into();
                } else if let Some(v) = line.strip_prefix("data: ") {
                    data = v.into();
                }
            }
            (ev, serde_json::from_str(&data).unwrap_or(Value::Null))
        })
        .collect()
}

async fn relay_call(app: &Router, body: Value) -> (StatusCode, Vec<(String, Value)>, Value) {
    let resp = app
        .clone()
        .oneshot(json_request("POST", RELAY_PATH, &body))
        .await
        .expect("r");
    let status = resp.status();
    let bytes = body_bytes(resp).await;
    let text = String::from_utf8_lossy(&bytes).to_string();
    if status != StatusCode::OK {
        return (
            status,
            Vec::new(),
            serde_json::from_str(&text).unwrap_or(Value::Null),
        );
    }
    (status, events(&text), Value::Null)
}

fn message(evs: &[(String, Value)]) -> Option<Value> {
    evs.iter()
        .find(|(e, v)| e == "message" && (v.get("result").is_some() || v.get("error").is_some()))
        .map(|(_, v)| v.clone())
}

fn relay_meta(evs: &[(String, Value)]) -> Value {
    evs.iter()
        .find(|(e, _)| e == "relay")
        .map(|(_, v)| v.clone())
        .unwrap_or(Value::Null)
}

// ── The server list ─────────────────────────────────────────────────

#[tokio::test]
async fn servers_lists_every_mcp_server_of_the_instance() {
    let app = test_app_with(test_state_with(Config::for_tests()));
    let resp = app
        .clone()
        .oneshot(Request::get(SERVERS_PATH).body(Body::empty()).expect("req"))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let paths: Vec<&str> = v["servers"]
        .as_array()
        .expect("servers")
        .iter()
        .filter_map(|s| s["path"].as_str())
        .collect();
    for p in [
        "/mcp",
        "/mcp/protected",
        "/mcp/apikey",
        "/mcp/servers/weather",
        "/mcp/sse",
        "/_rustybin/mcp",
        "/mock/petstore/_rustybin/mcp",
    ] {
        assert!(paths.contains(&p), "{p} missing from {paths:?}");
    }
    let protected = v["servers"]
        .as_array()
        .and_then(|s| s.iter().find(|x| x["path"] == "/mcp/protected"))
        .expect("protected");
    assert_eq!(protected["auth"]["kind"], "oauth");
    let pet = v["servers"]
        .as_array()
        .and_then(|s| s.iter().find(|x| x["kind"] == "mock-api"))
        .expect("mock");
    assert_eq!(pet["mock"], "petstore");
    assert_eq!(pet["state"], "serving");
    assert!(pet["tools"].as_u64().is_some_and(|n| n > 0));
    assert_eq!(
        v["checks"].as_array().map(Vec::len),
        Some(checks::CHECKS.len())
    );
    assert!(v["relay"]["hint"]
        .as_str()
        .is_some_and(|h| h.contains("RUSTYBIN_EGRESS_ALLOW")));
}

// ── The relay, in process ───────────────────────────────────────────

#[tokio::test]
async fn relay_in_process_for_every_transport() {
    let app = test_app_with(test_state_with(Config::for_tests()));
    // 2026-07-28 stateless.
    let (status, evs, _) = relay_call(&app, json!({ "target": "/mcp", "headers": modern_headers("server/discover"), "message": modern_discover(1) })).await;
    assert_eq!(status, StatusCode::OK);
    let meta = relay_meta(&evs);
    assert_eq!(meta["status"], 200);
    assert_eq!(meta["session"], Value::Null, "stateless: no relay session");
    assert!(message(&evs).is_some_and(|m| m["result"]["supportedVersions"].is_array()));
    assert_eq!(evs.last().map(|(e, _)| e.as_str()), Some("done"));
    // 2025-11-25 session: the relay keeps the upstream session id.
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } } });
    let (_, evs, _) = relay_call(&app, json!({ "target": "/mcp", "message": init })).await;
    let meta = relay_meta(&evs);
    let session = meta["session"].as_str().expect("relay session").to_string();
    assert!(meta["headers"]["mcp-session-id"].is_string());
    let (_, _, _) = relay_call(&app, json!({ "target": "/mcp", "session": session, "headers": { "MCP-Protocol-Version": "2025-11-25" }, "message": { "jsonrpc": "2.0", "method": "notifications/initialized" } })).await;
    let (status, evs, _) = relay_call(&app, json!({ "target": "/mcp", "session": session, "headers": { "MCP-Protocol-Version": "2025-11-25" }, "message": { "jsonrpc": "2.0", "id": 2, "method": "tools/list" } })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        message(&evs).is_some_and(|m| m["result"]["tools"]
            .as_array()
            .is_some_and(|t| !t.is_empty())),
        "{evs:?}"
    );
    // Ending the relay session ends the upstream one.
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/_rustybin/mcp-relay/{session}"))
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["ended"], true);
    assert!(
        v["upstream_status"].as_u64().is_some_and(|s| s < 300),
        "{v}"
    );
    let (status, _, v) = relay_call(&app, json!({ "target": "/mcp", "session": session, "message": { "jsonrpc": "2.0", "id": 3, "method": "ping" } })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(v["reason"], "unknown_session");
    // Legacy HTTP+SSE: the relay holds the stream.
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } } });
    let (status, evs, _) = relay_call(
        &app,
        json!({ "target": "/mcp/sse", "transport": "legacy", "message": init }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let meta = relay_meta(&evs);
    assert_eq!(meta["transport"], "legacy");
    let session = meta["session"]
        .as_str()
        .expect("legacy session")
        .to_string();
    assert!(
        message(&evs).is_some_and(|m| m["result"]["protocolVersion"] == "2024-11-05"),
        "{evs:?}"
    );
    let (_, evs, _) = relay_call(&app, json!({ "target": "/mcp/sse", "transport": "legacy", "session": session, "message": { "jsonrpc": "2.0", "id": 2, "method": "tools/list" } })).await;
    assert!(
        message(&evs).is_some_and(|m| m["result"]["tools"].is_array()),
        "{evs:?}"
    );
    // A 401 comes back with its challenge for the OAuth walk.
    let (_, evs, _) = relay_call(&app, json!({ "target": "/mcp/protected", "headers": modern_headers("server/discover"), "message": modern_discover(1) })).await;
    let meta = relay_meta(&evs);
    assert_eq!(meta["status"], 401);
    assert!(meta["headers"]["www-authenticate"]
        .as_str()
        .is_some_and(|w| w.contains("resource_metadata")));
}

#[tokio::test]
async fn relay_refusals() {
    let app = test_app_with(test_state_with(Config::for_tests()));
    // Not an MCP server of this instance.
    let (status, _, v) = relay_call(&app, json!({ "target": "/_rustybin/config", "message": { "jsonrpc": "2.0", "id": 1, "method": "ping" } })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(v["reason"], "invalid_target");
    // Another host without RUSTYBIN_EGRESS_ALLOW.
    let (status, _, v) = relay_call(&app, json!({ "target": "https://mcp.example.com/mcp", "message": { "jsonrpc": "2.0", "id": 1, "method": "ping" } })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(v["reason"], "egress_refused");
    assert!(
        v["error"]
            .as_str()
            .is_some_and(|e| e.contains("RUSTYBIN_EGRESS_ALLOW")),
        "{v}"
    );
    // Metadata addresses never.
    let (status, _, _) = relay_call(&app, json!({ "target": "http://169.254.169.254/mcp", "message": { "jsonrpc": "2.0", "id": 1, "method": "ping" } })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Bad bodies.
    let (status, _, _) = relay_call(&app, json!({ "target": "/mcp" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = relay_call(
        &app,
        json!({ "target": "/mcp", "headers": { "bad header": "x" }, "message": {} }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = relay_call(&app, json!({ "target": "ftp://x/y", "message": {} })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[test]
fn relay_sessions_are_bounded() {
    let r = relay::Relay::default();
    let state = test_state_with(Config::for_tests());
    let _ = state;
    for i in 0..(relay::MAX_SESSIONS + 10) {
        r.insert_for_tests(format!("s{i}"));
    }
    assert_eq!(r.len(), relay::MAX_SESSIONS);
}

// ── Through the egress policy to a second instance ───────────────────

/// A second, real Rustybin on an ephemeral port.
async fn second_instance() -> (crate::server::RunningServer, String) {
    let mut c = Config::for_tests();
    c.instance_id = "upstream".into();
    let server = crate::server::start_with_state(crate::state::AppState::for_tests(c))
        .await
        .expect("start");
    let base = format!("http://{}", server.http_addr);
    (server, base)
}

fn relay_config(loopback: bool) -> Config {
    let mut c = Config::for_tests();
    c.egress_allow = vec!["127.0.0.1".into()];
    c.egress_allow_loopback = loopback;
    c
}

#[tokio::test]
async fn relay_reaches_another_instance_and_walks_its_oauth() {
    let (upstream, base) = second_instance().await;
    // Without the loopback switch the policy refuses 127.0.0.1.
    let app = test_app_with(test_state_with(relay_config(false)));
    let (status, _, v) = relay_call(&app, json!({ "target": format!("{base}/mcp"), "headers": modern_headers("server/discover"), "message": modern_discover(1) })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    // With it the relay connects.
    let app = test_app_with(test_state_with(relay_config(true)));
    let (status, evs, _) = relay_call(&app, json!({ "target": format!("{base}/mcp"), "headers": modern_headers("server/discover"), "message": modern_discover(1) })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        message(&evs).is_some_and(|m| m["result"]["supportedVersions"].is_array()),
        "{evs:?}"
    );
    // The OAuth walk of /mcp/protected through the fetch helper.
    let (_, evs, _) = relay_call(&app, json!({ "target": format!("{base}/mcp/protected"), "headers": modern_headers("server/discover"), "message": modern_discover(1) })).await;
    let www = relay_meta(&evs)["headers"]["www-authenticate"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let prm_url = www
        .split("resource_metadata=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("resource_metadata")
        .to_string();
    let fetch = |body: Value| {
        let app = app.clone();
        async move {
            body_json(
                app.oneshot(json_request("POST", RELAY_FETCH_PATH, &body))
                    .await
                    .expect("r"),
            )
            .await
        }
    };
    let prm = fetch(json!({ "url": prm_url })).await;
    assert_eq!(prm["status"], 200, "{prm}");
    let resource = prm["json"]["resource"]
        .as_str()
        .expect("resource")
        .to_string();
    let issuer = prm["json"]["authorization_servers"][0]
        .as_str()
        .expect("issuer")
        .trim_end_matches('/')
        .to_string();
    let asm =
        fetch(json!({ "url": format!("{issuer}/.well-known/oauth-authorization-server") })).await;
    let token_endpoint = asm["json"]["token_endpoint"]
        .as_str()
        .expect("token endpoint")
        .to_string();
    let form = format!("grant_type=client_credentials&client_id=rustybin&client_secret=secret&resource={}&scope=mcp%3Atools", urlencode(&resource));
    let tok = fetch(json!({ "url": token_endpoint, "method": "POST", "headers": { "Content-Type": "application/x-www-form-urlencoded" }, "body": form })).await;
    let token = tok["json"]["access_token"]
        .as_str()
        .expect("token")
        .to_string();
    let (_, evs, _) = relay_call(&app, json!({ "target": format!("{base}/mcp/protected"), "headers": { "MCP-Protocol-Version": "2026-07-28", "Mcp-Method": "server/discover", "Authorization": format!("Bearer {token}") }, "message": modern_discover(2) })).await;
    assert_eq!(relay_meta(&evs)["status"], 200, "{evs:?}");
    upstream.shutdown();
}

fn urlencode(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

// ── Checks ──────────────────────────────────────────────────────────

async fn checks_call(app: &Router, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(json_request("POST", CHECKS_PATH, &body))
        .await
        .expect("r");
    let status = resp.status();
    (status, body_json(resp).await)
}

fn check<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .and_then(|c| c.iter().find(|x| x["id"] == id))
        .unwrap_or(&Value::Null)
}

#[tokio::test]
async fn checks_pass_on_the_instance_servers() {
    let app = test_app_with(test_state_with(Config::for_tests()));
    // 2026-07-28 against /mcp.
    let started = Instant::now();
    let (status, r) = checks_call(&app, json!({ "target": "/mcp", "max_tools": 6 })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        r["ok"],
        true,
        "{}",
        serde_json::to_string_pretty(&r["checks"]).unwrap_or_default()
    );
    assert_eq!(check(&r, "connect")["status"], "pass");
    assert_eq!(check(&r, "version")["status"], "pass");
    assert_eq!(check(&r, "tools_list")["status"], "pass");
    assert_ne!(check(&r, "tools_call")["status"], "fail");
    assert_eq!(check(&r, "unknown_tool")["status"], "pass");
    assert_eq!(check(&r, "unknown_method")["status"], "pass");
    assert_eq!(check(&r, "routing_headers")["status"], "pass");
    assert_eq!(check(&r, "session")["status"], "skip");
    assert_eq!(check(&r, "auth_refusal")["status"], "skip");
    assert!(r["latency"]["calls"].as_u64().is_some_and(|n| n > 5));
    assert!(r["server"]["capabilities"]["tools"].is_object());
    assert!(started.elapsed().as_secs() < 60);
    // The session era: the lifecycle checks run.
    let (_, r) = checks_call(&app, json!({ "target": "/mcp", "transport": "session", "version": "2025-11-25", "checks": ["connect", "version", "ping", "server_stream", "session"] })).await;
    assert_eq!(r["ok"], true, "{}", r["checks"]);
    assert_eq!(
        check(&r, "session")["status"],
        "pass",
        "{}",
        check(&r, "session")
    );
    assert_eq!(r["summary"]["total"], 5);
    // Legacy HTTP+SSE.
    let (_, r) = checks_call(&app, json!({ "target": "/mcp/sse", "transport": "legacy", "checks": ["connect", "version", "tools_list", "ping"] })).await;
    assert_eq!(r["ok"], true, "{}", r["checks"]);
    // The protected variant refuses without a token, passes with the API key variant's key.
    let (_, r) = checks_call(&app, json!({ "target": "/mcp/apikey", "headers": { "X-API-Key": "demo-key" }, "checks": ["connect", "auth_refusal"] })).await;
    assert_eq!(
        check(&r, "auth_refusal")["status"],
        "pass",
        "{}",
        check(&r, "auth_refusal")
    );
    let (_, r) = checks_call(
        &app,
        json!({ "target": "/mcp/protected", "checks": ["connect", "tools_list"] }),
    )
    .await;
    assert_eq!(r["ok"], false);
    assert!(check(&r, "connect")["message"]
        .as_str()
        .is_some_and(|m| m.contains("WWW-Authenticate")));
    assert_eq!(check(&r, "tools_list")["status"], "skip");
    // A mock's server, and JUnit.
    let resp = app.clone().oneshot(Request::post(CHECKS_PATH).header("content-type", "application/json").header("accept", "application/xml")
        .body(Body::from(json!({ "target": "/mock/petstore/_rustybin/mcp", "checks": ["connect", "tools_list", "tools_call"] }).to_string())).expect("req")).await.expect("r");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("application/xml")));
    let xml = String::from_utf8(body_bytes(resp).await.to_vec()).expect("utf8");
    assert!(xml.contains("<testsuite name=\"mcp-checks /mock/petstore/_rustybin/mcp\" tests=\"3\" failures=\"0\""), "{xml}");
    assert!(xml.contains("<testcase classname=\"mcp.modern\" name=\"tools_call:"));
    // Runs are saved and listed newest first; a saved run reads back as JUnit.
    let resp = app
        .clone()
        .oneshot(Request::get(CHECKS_PATH).body(Body::empty()).expect("req"))
        .await
        .expect("r");
    let runs = body_json(resp).await;
    assert_eq!(runs["runs"].as_array().map(Vec::len), Some(6));
    assert_eq!(runs["runs"][0]["target"], "/mock/petstore/_rustybin/mcp");
    let id = runs["runs"][0]["id"].as_str().expect("id").to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/_rustybin/mcp-checks/{id}?format=junit"))
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("r");
    assert!(String::from_utf8(body_bytes(resp).await.to_vec())
        .unwrap_or_default()
        .starts_with("<?xml"));
    // Bad requests.
    let (status, v) = checks_call(&app, json!({ "target": "/mcp", "checks": ["nope"] })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let (status, _) = checks_call(
        &app,
        json!({ "target": "/mcp", "transport": "session", "version": "1999-01-01" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// An MCP server with planted faults: a broken inputSchema, an unknown
/// tool that "succeeds", a missing ping, no validation of routing headers.
async fn faulty_server() -> String {
    async fn handle(body: axum::body::Bytes) -> axum::response::Response {
        let msg: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let id = msg["id"].clone();
        let result = match msg["method"].as_str() {
            Some("server/discover") => json!({ "supportedVersions": ["2026-07-28"], "capabilities": { "tools": {} }, "_meta": { "io.modelcontextprotocol/serverInfo": { "name": "faulty", "version": "0.0.1" } } }),
            Some("tools/list") => json!({ "tools": [
                { "name": "fine", "inputSchema": { "type": "object", "properties": { "q": { "type": "string" } }, "required": ["q"] } },
                { "name": "broken", "inputSchema": { "type": "string" } }
            ] }),
            Some("tools/call") => json!({ "content": [{ "type": "text", "text": "ok" }] }),
            Some("ping") => return axum::Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "no ping" } })).into_response(),
            _ => return axum::Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "method not found" } })).into_response(),
        };
        axum::Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
    }
    let app = Router::new().route("/mcp", post(handle));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/mcp")
}

#[tokio::test]
async fn checks_find_planted_faults_through_the_relay() {
    let url = faulty_server().await;
    let app = test_app_with(test_state_with(relay_config(true)));
    let (status, r) = checks_call(&app, json!({ "target": url, "save": false })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(r["ok"], false);
    assert_eq!(r["relayed"], true);
    assert_eq!(check(&r, "connect")["status"], "pass");
    assert_eq!(
        check(&r, "tools_list")["status"],
        "fail",
        "{}",
        check(&r, "tools_list")
    );
    assert!(check(&r, "tools_list")["message"]
        .as_str()
        .is_some_and(|m| m.contains("broken")));
    assert_eq!(check(&r, "unknown_tool")["status"], "fail");
    assert_eq!(check(&r, "bad_arguments")["status"], "warn");
    assert_eq!(check(&r, "ping")["status"], "warn");
    assert_eq!(check(&r, "routing_headers")["status"], "warn");
    assert_eq!(check(&r, "resources")["status"], "skip");
    assert!(r["summary"]["failed"].as_u64().is_some_and(|n| n >= 2));
    let xml = checks::junit(&r);
    assert!(xml.contains("<failure message=\""), "{xml}");
    assert!(xml.contains("<skipped message=\""));
    // Without the policy the run stops at connect.
    let app = test_app_with(test_state_with(Config::for_tests()));
    let (_, r) = checks_call(&app, json!({ "target": url, "save": false })).await;
    assert_eq!(check(&r, "connect")["status"], "fail");
    assert!(check(&r, "connect")["message"]
        .as_str()
        .is_some_and(|m| m.contains("refused")));
    assert_eq!(check(&r, "tools_list")["status"], "skip");
}

#[tokio::test]
async fn checks_can_ask_the_assist_provider_for_arguments() {
    let mut c = Config::for_tests();
    c.assist_provider = Some("mock".into());
    let app = test_app_with(test_state_with(c));
    let (_, r) = checks_call(&app, json!({ "target": "/mcp/servers/weather", "checks": ["connect", "tools_call"], "ai_arguments": true, "save": false })).await;
    let calls = check(&r, "tools_call")["details"]["calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(!calls.is_empty());
    assert!(
        calls
            .iter()
            .all(|c| c["arguments_source"] == "ai" || c["outcome"] == "skipped"),
        "{calls:?}"
    );
    // The task through the assist route too.
    let resp = app.clone().oneshot(json_request("POST", "/_rustybin/assist/run", &json!({ "task": "tool_arguments", "stream": false,
        "inputs": { "tool": { "name": "get_weather", "inputSchema": { "type": "object", "required": ["city"], "properties": { "city": { "type": "string", "enum": ["Paris", "Lyon"] } } } } } }))).await.expect("r");
    let v = body_json(resp).await;
    assert!(
        v["json"]["arguments"]["city"]
            .as_str()
            .is_some_and(|c| c == "Paris" || c == "Lyon"),
        "{v}"
    );
}

// ── Saved connections ───────────────────────────────────────────────

#[tokio::test]
async fn saved_connections_seal_credentials_and_feed_the_relay() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut c = Config::for_tests();
    c.state_dir = Some(dir.path().to_path_buf());
    let app = test_app_with(test_state_with(c.clone()));
    let put = json!({ "name": "API key demo", "target": "/mcp/apikey", "auth": { "kind": "api_key", "name": "X-API-Key" }, "secrets": { "api_key": "demo-key-12345678" } });
    let resp = app
        .clone()
        .oneshot(json_request(
            "PUT",
            "/_rustybin/mcp-connections/apikey-demo",
            &put,
        ))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["secrets"]["api_key"]["set"], true);
    assert_eq!(v["secrets"]["api_key"]["hint"], "5678");
    assert_eq!(v["transport"], "modern");
    assert!(!v.to_string().contains("demo-key-1234"));
    // Sealed on disk.
    let file = std::fs::read_to_string(dir.path().join(store::FILE)).expect("file");
    assert!(file.contains("apikey-demo") && !file.contains("demo-key-1234"));
    // Without the connection the server refuses; with it the relay sends the key.
    let (_, evs, _) = relay_call(&app, json!({ "target": "/mcp/apikey", "headers": modern_headers("server/discover"), "message": modern_discover(1) })).await;
    assert_eq!(relay_meta(&evs)["status"], 401);
    let (_, evs, _) = relay_call(&app, json!({ "target": "/mcp/apikey", "connection": "apikey-demo", "headers": modern_headers("server/discover"), "message": modern_discover(1) })).await;
    assert_eq!(relay_meta(&evs)["status"], 200, "{evs:?}");
    // And the checks.
    let (_, r) = checks_call(&app, json!({ "connection": "apikey-demo", "checks": ["connect", "auth_refusal"], "save": false })).await;
    assert_eq!(r["ok"], true, "{}", r["checks"]);
    // Reloaded from disk by a fresh instance.
    let app2 = test_app_with(test_state_with(c));
    let v = body_json(
        app2.clone()
            .oneshot(
                Request::get(CONNECTIONS_PATH)
                    .body(Body::empty())
                    .expect("req"),
            )
            .await
            .expect("r"),
    )
    .await;
    assert_eq!(v["connections"][0]["id"], "apikey-demo");
    assert_eq!(v["persisted"], true);
    // Validation and deletion.
    let bad = json!({ "name": "x", "target": "/mcp", "secrets": { "nonsense": "y" } });
    let resp = app
        .clone()
        .oneshot(json_request("PUT", "/_rustybin/mcp-connections/bad", &bad))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let resp = app
        .clone()
        .oneshot(json_request(
            "PUT",
            "/_rustybin/mcp-connections/x",
            &json!({ "name": "x", "target": "/_rustybin/config" }),
        ))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let resp = app
        .clone()
        .oneshot(
            Request::delete("/_rustybin/mcp-connections/apikey-demo")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::OK);
    let (status, _, v) = relay_call(&app, json!({ "target": "/mcp/apikey", "connection": "apikey-demo", "message": modern_discover(1) })).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
}

#[test]
fn auth_builders() {
    use std::collections::BTreeMap;
    use zeroize::Zeroizing;
    let secrets: BTreeMap<String, Zeroizing<String>> = [
        ("token", "t1"),
        ("api_key", "k1"),
        ("password", "p1"),
        ("header:X-Tenant", "acme"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), Zeroizing::new(v.to_string())))
    .collect();
    let spec = |kind: &str| store::AuthSpec {
        kind: kind.into(),
        username: Some("ada".into()),
        headers: vec!["X-Tenant".into()],
        ..Default::default()
    };
    assert_eq!(
        store::build_auth(&spec("bearer"), &secrets).0,
        vec![("authorization".to_string(), "Bearer t1".to_string())]
    );
    assert_eq!(
        store::build_auth(&spec("oauth"), &secrets).0[0].1,
        "Bearer t1"
    );
    assert_eq!(
        store::build_auth(&spec("api_key"), &secrets).0,
        vec![("X-API-Key".to_string(), "k1".to_string())]
    );
    let mut q = spec("api_key");
    q.location = Some("query".into());
    q.name = Some("key".into());
    assert_eq!(
        store::build_auth(&q, &secrets).1,
        vec![("key".to_string(), "k1".to_string())]
    );
    assert_eq!(
        store::build_auth(&spec("basic"), &secrets).0[0].1,
        "Basic YWRhOnAx"
    );
    assert_eq!(
        store::build_auth(&spec("headers"), &secrets).0,
        vec![("X-Tenant".to_string(), "acme".to_string())]
    );
    assert!(store::build_auth(&spec("none"), &secrets).0.is_empty());
    assert!(store::AuthSpec {
        kind: "magic".into(),
        ..Default::default()
    }
    .validate()
    .is_err());
    // Targets.
    assert!(client::Target::parse("/mcp", false).is_ok());
    assert!(client::Target::parse("/mock/petstore/_rustybin/mcp", false).is_ok());
    assert!(client::Target::parse("/mock/petstore/v1/pets", false).is_err());
    assert!(
        client::Target::parse("/.well-known/oauth-protected-resource/mcp/protected", false)
            .is_err()
    );
    assert!(
        client::Target::parse("/.well-known/oauth-protected-resource/mcp/protected", true).is_ok()
    );
    assert!(client::Target::parse("//evil/mcp", false).is_err());
    let t = client::Target::parse("https://h.example/sse", false).expect("url");
    assert_eq!(
        t.join("/messages?sessionId=1").expect("join").as_str(),
        "https://h.example/messages?sessionId=1"
    );
    assert_eq!(
        client::Target::Local("/mcp".into())
            .with_query(&[("k".into(), "a b".into())])
            .as_str(),
        "/mcp?k=a+b"
    );
    assert_eq!(client::header_safe("get_weather"), "get_weather");
    assert!(client::header_safe("caf\u{e9}").starts_with("=?base64?"));
}

// ── Control auth ────────────────────────────────────────────────────

#[tokio::test]
async fn the_console_scope_may_relay_and_run_checks_but_not_save_connections() {
    let mut c = Config::for_tests();
    c.control_auth = crate::control_auth::ControlAuth::Jwt;
    c.control_jwt_key = crate::control_auth::parse_public_key(control_keys::CONTROL_JWT_PUBLIC_PEM);
    c.admin_token = Some("admin-secret".into());
    let app = test_app_with(test_state_with(c));
    let call = |method: &'static str, uri: String, token: Option<String>, body: Value| {
        let app = app.clone();
        async move {
            let mut b = Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json");
            if let Some(t) = token {
                b = b.header("authorization", format!("Bearer {t}"));
            }
            app.oneshot(b.body(Body::from(body.to_string())).expect("req"))
                .await
                .expect("r")
                .status()
        }
    };
    let console = control_jwt(Some("console"));
    assert_eq!(
        call("GET", SERVERS_PATH.into(), None, json!({})).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call("GET", SERVERS_PATH.into(), Some(console.clone()), json!({})).await,
        StatusCode::OK
    );
    assert_eq!(call("POST", RELAY_PATH.into(), Some(console.clone()), json!({ "target": "/mcp", "headers": modern_headers("server/discover"), "message": modern_discover(1) })).await, StatusCode::OK);
    assert_eq!(
        call(
            "POST",
            CHECKS_PATH.into(),
            Some(console.clone()),
            json!({ "target": "/mcp", "checks": ["connect"], "save": false })
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        call(
            "DELETE",
            "/_rustybin/mcp-relay/nope".into(),
            Some(console.clone()),
            json!({})
        )
        .await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            "PUT",
            "/_rustybin/mcp-connections/x".into(),
            Some(console.clone()),
            json!({ "name": "x", "target": "/mcp" })
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            "PUT",
            "/_rustybin/mcp-connections/x".into(),
            Some("admin-secret".into()),
            json!({ "name": "x", "target": "/mcp" })
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        call(
            "DELETE",
            "/_rustybin/mcp-connections/x".into(),
            Some(console),
            json!({})
        )
        .await,
        StatusCode::FORBIDDEN
    );
}
