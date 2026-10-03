//! Integration-style tests for the MCP module (router level, no network).

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, Response, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::protocol::{self, Version};
use super::{router_with, McpConfig, Shared};
use crate::test_support::{body_json, body_string, test_state};

const ACCEPT_BOTH: &str = "application/json, text/event-stream";

fn app_with(cfg: McpConfig) -> Router {
    let peer: std::net::SocketAddr = ([127, 0, 0, 1], 40000).into();
    router_with(Arc::new(Shared::new(cfg)))
        .with_state(test_state())
        .layer(MockConnectInfo(peer))
}

fn app() -> Router {
    app_with(McpConfig::for_tests())
}

fn modern_params(mut params: Value, caps: Value) -> Value {
    if params.is_null() {
        params = json!({});
    }
    params["_meta"] = json!({
        protocol::META_PROTOCOL_VERSION: "2026-07-28",
        protocol::META_CLIENT_CAPABILITIES: caps,
        protocol::META_CLIENT_INFO: { "name": "test", "version": "1" },
    });
    params
}

/// A fully conforming 2026-07-28 request (headers derived from the body).
fn modern_req(path: &str, id: i64, method: &str, params: Value) -> axum::http::request::Builder {
    modern_req_accept(path, id, method, params, ACCEPT_BOTH)
}

fn modern_req_accept(
    path: &str,
    id: i64,
    method: &str,
    params: Value,
    accept: &str,
) -> axum::http::request::Builder {
    let mut b = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("accept", accept)
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", method);
    let name = match method {
        "tools/call" | "prompts/get" => params.get("name").and_then(Value::as_str),
        "resources/read" => params.get("uri").and_then(Value::as_str),
        _ => None,
    };
    if let Some(n) = name {
        b = b.header("mcp-name", n);
    }
    let _ = id;
    b
}

fn rpc(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

async fn modern_call(app: &Router, path: &str, method: &str, params: Value) -> Response<Body> {
    let params = modern_params(params, json!({}));
    let body = rpc(1, method, params.clone());
    app.clone()
        .oneshot(
            modern_req(path, 1, method, params)
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response")
}

fn legacy_req(
    path: &str,
    session: Option<&str>,
    version: Option<&str>,
) -> axum::http::request::Builder {
    let mut b = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("accept", ACCEPT_BOTH);
    if let Some(s) = session {
        b = b.header("mcp-session-id", s);
    }
    if let Some(v) = version {
        b = b.header("mcp-protocol-version", v);
    }
    b
}

async fn initialize(app: &Router, path: &str, version: &str, caps: Value) -> (String, Value) {
    let body = rpc(
        0,
        "initialize",
        json!({ "protocolVersion": version, "capabilities": caps, "clientInfo": { "name": "t", "version": "1" } }),
    );
    let resp = app
        .clone()
        .oneshot(
            legacy_req(path, None, None)
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let sid = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .expect("session id")
        .to_string();
    let json = body_json(resp).await;
    let version = json["result"]["protocolVersion"]
        .as_str()
        .unwrap_or(version)
        .to_string();
    let version = version.as_str();
    // Complete the handshake.
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let resp = app
        .clone()
        .oneshot(
            legacy_req(path, Some(&sid), Some(version))
                .body(Body::from(note.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    (sid, json)
}

async fn legacy_call(
    app: &Router,
    path: &str,
    sid: &str,
    version: &str,
    id: i64,
    method: &str,
    params: Value,
) -> Response<Body> {
    app.clone()
        .oneshot(
            legacy_req(path, Some(sid), Some(version))
                .body(Body::from(rpc(id, method, params).to_string()))
                .expect("request"),
        )
        .await
        .expect("response")
}

/// Parse SSE `data:` payloads from a complete body.
fn sse_messages(text: &str) -> Vec<Value> {
    text.split("\n\n")
        .filter_map(|ev| {
            let data: Vec<&str> = ev
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(str::trim_start)
                .collect();
            if data.is_empty() {
                return None;
            }
            serde_json::from_str(&data.join("\n")).ok()
        })
        .collect()
}

/// Read SSE events from a live body until `n` JSON messages (or raw events) arrived.
async fn read_events(body: &mut Body, n: usize) -> Vec<(String, String)> {
    let mut buf = String::new();
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while events.len() < n {
        let frame = tokio::time::timeout_at(deadline, body.frame())
            .await
            .expect("event in time")
            .expect("stream open")
            .expect("frame");
        if let Ok(data) = frame.into_data() {
            buf.push_str(&String::from_utf8_lossy(&data));
        }
        while let Some(pos) = buf.find("\n\n") {
            let ev: String = buf[..pos].to_string();
            buf = buf[pos + 2..].to_string();
            let mut name = "message".to_string();
            let mut data = Vec::new();
            for line in ev.lines() {
                if let Some(v) = line.strip_prefix("event:") {
                    name = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("data:") {
                    data.push(v.trim_start().to_string());
                }
            }
            if !data.is_empty() {
                events.push((name, data.join("\n")));
            }
        }
    }
    events
}

// ── 2026-07-28 ──────────────────────────────────────────────────────

#[tokio::test]
async fn modern_discover_list_and_call() {
    let app = app();
    let resp = modern_call(&app, "/mcp", "server/discover", json!({})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["x-rustybin-mcp-session"], "stateless");
    assert!(resp.headers().get("mcp-session-id").is_none());
    let j = body_json(resp).await;
    let r = &j["result"];
    assert_eq!(r["resultType"], "complete");
    assert!(r["supportedVersions"]
        .as_array()
        .expect("versions")
        .contains(&json!("2026-07-28")));
    assert_eq!(
        r["_meta"][protocol::META_SERVER_INFO]["name"],
        "rustybin-mcp"
    );
    assert!(r["ttlMs"].is_u64());
    assert!(r["capabilities"]["tools"].is_object());

    let resp = modern_call(&app, "/mcp", "tools/list", json!({})).await;
    let j = body_json(resp).await;
    assert_eq!(j["result"]["cacheScope"], "public");
    assert_eq!(j["result"]["ttlMs"], 300_000);
    let tools = j["result"]["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), super::tools::TOOLS.len());
    let weather = tools
        .iter()
        .find(|t| t["name"] == "get_weather")
        .expect("weather");
    assert_eq!(
        weather["inputSchema"]["properties"]["city"]["x-mcp-header"],
        "City"
    );

    let params = modern_params(
        json!({ "name": "get_weather", "arguments": { "city": "Paris" } }),
        json!({}),
    );
    let resp = app
        .clone()
        .oneshot(
            modern_req("/mcp", 3, "tools/call", params.clone())
                .header("mcp-param-city", "Paris")
                .body(Body::from(rpc(3, "tools/call", params).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["x-rustybin-mcp-method"], "tools/call");
    assert_eq!(resp.headers()["x-rustybin-mcp-request-id"], "3");
    let j = body_json(resp).await;
    assert_eq!(j["id"], 3);
    assert_eq!(j["result"]["structuredContent"]["city"], "Paris");
    assert_eq!(j["result"]["isError"], false);
}

#[tokio::test]
async fn modern_header_body_mismatch() {
    let app = app();
    let params = modern_params(
        json!({ "name": "echo", "arguments": { "message": "hi" } }),
        json!({}),
    );
    let body = rpc(1, "tools/call", params).to_string();
    // Wrong Mcp-Name.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header("accept", ACCEPT_BOTH)
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", "tools/call")
                .header("mcp-name", "add")
                .body(Body::from(body.clone()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let j = body_json(resp).await;
    assert_eq!(j["error"]["code"], protocol::HEADER_MISMATCH);
    assert_eq!(j["id"], 1);
    // Base64 sentinel Mcp-Name is decoded before comparing.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", "tools/call")
                .header("mcp-name", "=?base64?ZWNobw==?=")
                .body(Body::from(body.clone()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    // Missing headers entirely.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(Body::from(body.clone()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(resp).await["error"]["code"],
        protocol::HEADER_MISMATCH
    );
    // Method header disagrees.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", "tools/list")
                .header("mcp-name", "echo")
                .body(Body::from(body))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(
        body_json(resp).await["error"]["code"],
        protocol::HEADER_MISMATCH
    );
    // x-mcp-header argument without its Mcp-Param header.
    let resp = modern_call(
        &app,
        "/mcp",
        "tools/call",
        json!({ "name": "get_weather", "arguments": { "city": "Oslo" } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let j = body_json(resp).await;
    assert_eq!(j["error"]["code"], protocol::HEADER_MISMATCH);
    assert!(j["error"]["message"]
        .as_str()
        .unwrap_or("")
        .contains("Mcp-Param-City"));
}

#[tokio::test]
async fn modern_envelope_errors() {
    let app = app();
    // Unsupported version in a modern envelope.
    let mut params = modern_params(json!({}), json!({}));
    params["_meta"][protocol::META_PROTOCOL_VERSION] = json!("2099-01-01");
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header("mcp-protocol-version", "2099-01-01")
                .header("mcp-method", "tools/list")
                .body(Body::from(rpc(9, "tools/list", params).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let j = body_json(resp).await;
    assert_eq!(j["error"]["code"], protocol::UNSUPPORTED_PROTOCOL_VERSION);
    assert_eq!(j["error"]["data"]["requested"], "2099-01-01");
    assert!(j["error"]["data"]["supported"].as_array().is_some());
    // Missing clientCapabilities.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", "tools/list")
                .body(Body::from(
                    rpc(
                        2,
                        "tools/list",
                        json!({ "_meta": { protocol::META_PROTOCOL_VERSION: "2026-07-28" } }),
                    )
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(resp).await["error"]["code"],
        protocol::INVALID_PARAMS
    );
    // Unknown method (ping was removed in 2026-07-28): 404 + -32601.
    let resp = modern_call(&app, "/mcp", "ping", json!({})).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(resp).await["error"]["code"],
        protocol::METHOD_NOT_FOUND
    );
    // Parse error.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(Body::from("{not json"))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let j = body_json(resp).await;
    assert_eq!(j["error"]["code"], protocol::PARSE_ERROR);
    assert_eq!(j["id"], Value::Null);
    // GET / DELETE do not exist at 2026-07-28.
    for method in ["GET", "DELETE"] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/mcp")
                    .header("accept", "text/event-stream")
                    .header("mcp-protocol-version", "2026-07-28")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED, "{method}");
    }
    // Invalid log level.
    let mut params = modern_params(json!({}), json!({}));
    params["_meta"][protocol::META_LOG_LEVEL] = json!("loud");
    let resp = app
        .clone()
        .oneshot(
            modern_req("/mcp", 1, "tools/list", params.clone())
                .body(Body::from(rpc(1, "tools/list", params).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(
        body_json(resp).await["error"]["code"],
        protocol::INVALID_PARAMS
    );
}

#[tokio::test]
async fn modern_progress_streams_over_sse() {
    let app = app();
    let mut params = modern_params(
        json!({ "name": "slow_task", "arguments": { "duration_ms": 80, "steps": 4 } }),
        json!({}),
    );
    params["_meta"]["progressToken"] = json!("tok-1");
    params["_meta"][protocol::META_LOG_LEVEL] = json!("info");
    let resp = app
        .clone()
        .oneshot(
            modern_req("/mcp", 5, "tools/call", params.clone())
                .body(Body::from(rpc(5, "tools/call", params.clone()).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()["content-type"]
        .to_str()
        .unwrap_or("")
        .starts_with("text/event-stream"));
    let msgs = sse_messages(&body_string(resp).await);
    let progress: Vec<&Value> = msgs
        .iter()
        .filter(|m| m["method"] == "notifications/progress")
        .collect();
    assert_eq!(progress.len(), 4);
    assert_eq!(progress[3]["params"]["progressToken"], "tok-1");
    assert_eq!(progress[3]["params"]["progress"], 4.0);
    assert_eq!(progress[3]["params"]["total"], 4.0);
    assert!(msgs.iter().any(|m| m["method"] == "notifications/message"));
    let last = msgs.last().expect("final");
    assert_eq!(last["id"], 5);
    assert_eq!(last["result"]["structuredContent"]["completed"], true);

    // JSON-only clients get one JSON object (notifications dropped).
    let resp = app
        .clone()
        .oneshot(
            modern_req_accept("/mcp", 6, "tools/call", params.clone(), "application/json")
                .body(Body::from(rpc(6, "tools/call", params).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.headers()["content-type"], "application/json");
    assert_eq!(body_json(resp).await["result"]["isError"], false);
}

#[tokio::test]
async fn modern_mrtr_elicitation() {
    let app = app();
    let caps = json!({ "elicitation": { "form": {} } });
    let params = modern_params(
        json!({ "name": "elicit_confirmation", "arguments": { "action": "ship it" } }),
        caps.clone(),
    );
    let resp = app
        .clone()
        .oneshot(
            modern_req("/mcp", 1, "tools/call", params.clone())
                .body(Body::from(rpc(1, "tools/call", params.clone()).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let j = body_json(resp).await;
    let r = &j["result"];
    assert_eq!(r["resultType"], "input_required");
    assert_eq!(
        r["inputRequests"]["confirm"]["method"],
        "elicitation/create"
    );
    assert_eq!(r["inputRequests"]["confirm"]["params"]["mode"], "form");
    let state = r["requestState"].as_str().expect("state").to_string();

    let mut retry = params.clone();
    retry["inputResponses"] =
        json!({ "confirm": { "action": "accept", "content": { "confirm": true } } });
    retry["requestState"] = json!(state);
    let resp = app
        .clone()
        .oneshot(
            modern_req("/mcp", 2, "tools/call", retry.clone())
                .body(Body::from(rpc(2, "tools/call", retry.clone()).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let j = body_json(resp).await;
    assert_eq!(j["result"]["resultType"], "complete");
    assert!(j["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .contains("User confirmed"));

    // Tampered state is rejected.
    retry["requestState"] = json!(format!("{state}x"));
    let resp = app
        .clone()
        .oneshot(
            modern_req("/mcp", 3, "tools/call", retry.clone())
                .body(Body::from(rpc(3, "tools/call", retry).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(
        body_json(resp).await["error"]["code"],
        protocol::INVALID_PARAMS
    );

    // Without the capability and require=true: -32021.
    let resp = modern_call(
        &app,
        "/mcp",
        "tools/call",
        json!({ "name": "elicit_confirmation", "arguments": { "require": true } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let j = body_json(resp).await;
    assert_eq!(
        j["error"]["code"],
        protocol::MISSING_REQUIRED_CLIENT_CAPABILITY
    );
    assert!(j["error"]["data"]["requiredCapabilities"]["elicitation"].is_object());
}

#[tokio::test]
async fn modern_subscriptions_listen() {
    let app = app();
    let params = modern_params(
        json!({ "notifications": { "resourceSubscriptions": ["rustybin://clock", "rustybin://nope"], "toolsListChanged": true } }),
        json!({}),
    );
    let resp = app
        .clone()
        .oneshot(
            modern_req("/mcp", 42, "subscriptions/listen", params.clone())
                .body(Body::from(
                    rpc(42, "subscriptions/listen", params).to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body();
    let events = read_events(&mut body, 2).await;
    let ack: Value = serde_json::from_str(&events[0].1).expect("ack");
    assert_eq!(ack["method"], "notifications/subscriptions/acknowledged");
    assert_eq!(ack["params"]["_meta"][protocol::META_SUBSCRIPTION_ID], 42);
    assert_eq!(
        ack["params"]["notifications"],
        json!({ "resourceSubscriptions": ["rustybin://clock"] })
    );
    let upd: Value = serde_json::from_str(&events[1].1).expect("update");
    assert_eq!(upd["method"], "notifications/resources/updated");
    assert_eq!(upd["params"]["uri"], "rustybin://clock");
}

#[tokio::test]
async fn pagination_with_page_size_query() {
    let app = app();
    let mut cursor: Option<String> = None;
    let mut names = Vec::new();
    for _ in 0..20 {
        let params = match &cursor {
            Some(c) => json!({ "cursor": c }),
            None => json!({}),
        };
        let resp = modern_call(&app, "/mcp?page_size=5", "tools/list", params).await;
        let j = body_json(resp).await;
        let page = j["result"]["tools"].as_array().expect("tools").clone();
        assert!(page.len() <= 5);
        names.extend(
            page.iter()
                .map(|t| t["name"].as_str().unwrap_or("").to_string()),
        );
        cursor = j["result"]["nextCursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(names.len(), super::tools::TOOLS.len());
    let resp = modern_call(&app, "/mcp", "tools/list", json!({ "cursor": "bogus" })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(resp).await["error"]["code"],
        protocol::INVALID_PARAMS
    );
}

#[tokio::test]
async fn named_servers_expose_subsets() {
    let app = app();
    let resp = modern_call(&app, "/mcp/servers/weather", "tools/list", json!({})).await;
    assert_eq!(resp.headers()["x-rustybin-mcp-server"], "weather");
    let j = body_json(resp).await;
    let names: Vec<&str> = j["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(names, vec!["get_weather", "get_time", "inspect_request"]);
    let resp = modern_call(&app, "/mcp/servers/weather", "prompts/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = modern_call(
        &app,
        "/mcp/servers/crm",
        "tools/call",
        json!({ "name": "echo", "arguments": {} }),
    )
    .await;
    assert_eq!(
        body_json(resp).await["error"]["code"],
        protocol::INVALID_PARAMS
    );
    let resp = modern_call(&app, "/mcp/servers/nope", "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn resources_prompts_and_completion() {
    let app = app();
    let resp = modern_call(
        &app,
        "/mcp",
        "resources/read",
        json!({ "uri": "rustybin://images/logo.png" }),
    )
    .await;
    let j = body_json(resp).await;
    assert!(j["result"]["contents"][0]["blob"].as_str().is_some());
    assert_eq!(j["result"]["ttlMs"], 60_000);
    let resp = modern_call(
        &app,
        "/mcp",
        "resources/read",
        json!({ "uri": "rustybin://customers/u2" }),
    )
    .await;
    let j = body_json(resp).await;
    assert!(j["result"]["contents"][0]["text"]
        .as_str()
        .unwrap_or("")
        .contains("Bob Martinez"));
    let resp = modern_call(
        &app,
        "/mcp",
        "resources/read",
        json!({ "uri": "rustybin://missing" }),
    )
    .await;
    assert_eq!(
        body_json(resp).await["error"]["code"],
        protocol::INVALID_PARAMS
    );
    let resp = modern_call(&app, "/mcp", "resources/templates/list", json!({})).await;
    assert_eq!(
        body_json(resp).await["result"]["resourceTemplates"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );

    let resp = modern_call(
        &app,
        "/mcp",
        "prompts/get",
        json!({ "name": "code_review", "arguments": { "code": "fn main() {}", "language": "rust" } }),
    )
    .await;
    let j = body_json(resp).await;
    assert!(j["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap_or("")
        .contains("rust"));
    let resp = modern_call(
        &app,
        "/mcp",
        "prompts/get",
        json!({ "name": "code_review", "arguments": {} }),
    )
    .await;
    assert_eq!(
        body_json(resp).await["error"]["code"],
        protocol::INVALID_PARAMS
    );

    let resp = modern_call(
        &app,
        "/mcp",
        "completion/complete",
        json!({ "ref": { "type": "ref/prompt", "name": "code_review" }, "argument": { "name": "language", "value": "py" } }),
    )
    .await;
    assert_eq!(
        body_json(resp).await["result"]["completion"]["values"],
        json!(["python"])
    );
    let resp = modern_call(
        &app,
        "/mcp",
        "completion/complete",
        json!({ "ref": { "type": "ref/resource", "uri": "rustybin://weather/{city}" }, "argument": { "name": "city", "value": "s" } }),
    )
    .await;
    assert_eq!(
        body_json(resp).await["result"]["completion"]["values"],
        json!(["San Francisco", "Singapore", "Sydney"])
    );
}

#[tokio::test]
async fn inspect_request_shows_gateway_headers() {
    let app = app();
    let params = modern_params(
        json!({ "name": "inspect_request", "arguments": {} }),
        json!({}),
    );
    let resp = app
        .clone()
        .oneshot(
            modern_req("/mcp", 1, "tools/call", params.clone())
                .header("x-user-id", "alice")
                .header("authorization", "Bearer secret-token-value")
                .body(Body::from(rpc(1, "tools/call", params).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let j = body_json(resp).await;
    let s = &j["result"]["structuredContent"];
    assert_eq!(s["headers"]["x-user-id"], "alice");
    assert!(s["headers"]["authorization"]
        .as_str()
        .unwrap_or("")
        .contains("masked"));
    assert_eq!(s["protocolVersion"], "2026-07-28");
}

#[tokio::test]
async fn origin_validation() {
    let mut cfg = McpConfig::for_tests();
    cfg.allowed_origins = vec!["https://good.example".into()];
    let app = app_with(cfg);
    let params = modern_params(json!({}), json!({}));
    let send = |origin: &'static str| {
        let app = app.clone();
        let params = params.clone();
        async move {
            app.oneshot(
                modern_req("/mcp", 1, "server/discover", params.clone())
                    .header("origin", origin)
                    .body(Body::from(rpc(1, "server/discover", params).to_string()))
                    .expect("request"),
            )
            .await
            .expect("response")
        }
    };
    assert_eq!(
        send("https://evil.example").await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(send("https://good.example").await.status(), StatusCode::OK);
}

// ── Handshake era ──────────────────────────────────────────────────

#[tokio::test]
async fn legacy_initialize_and_session_enforcement() {
    let app = app();
    let (sid, init) = initialize(&app, "/mcp", "2025-06-18", json!({})).await;
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "rustybin-mcp");
    assert!(init["result"]["capabilities"]["completions"].is_object());

    // No session header.
    let resp = app
        .clone()
        .oneshot(
            legacy_req("/mcp", None, Some("2025-06-18"))
                .body(Body::from(rpc(1, "tools/list", json!({})).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    // Unknown session.
    let resp = legacy_call(
        &app,
        "/mcp",
        "nope",
        "2025-06-18",
        1,
        "tools/list",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    // Session belongs to /mcp, not to a named server.
    let resp = legacy_call(
        &app,
        "/mcp/servers/crm",
        &sid,
        "2025-06-18",
        1,
        "tools/list",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    // Unsupported MCP-Protocol-Version header.
    let resp = legacy_call(&app, "/mcp", &sid, "1999-01-01", 1, "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = legacy_call(&app, "/mcp", &sid, "2025-06-18", 2, "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["x-rustybin-mcp-session"], sid.as_str());
    let j = body_json(resp).await;
    assert!(j["result"].get("ttlMs").is_none());
    assert!(j["result"].get("resultType").is_none());
    let echo = j["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|t| t["name"] == "echo")
        .cloned()
        .expect("echo");
    assert!(echo.get("outputSchema").is_some());
    assert_eq!(echo["title"], "Echo");

    let resp = legacy_call(&app, "/mcp", &sid, "2025-06-18", 3, "ping", json!({})).await;
    assert_eq!(body_json(resp).await["result"], json!({}));
    let resp = legacy_call(
        &app,
        "/mcp",
        &sid,
        "2025-06-18",
        4,
        "server/discover",
        json!({}),
    )
    .await;
    assert_eq!(
        body_json(resp).await["error"]["code"],
        protocol::METHOD_NOT_FOUND
    );
    let resp = legacy_call(
        &app,
        "/mcp",
        &sid,
        "2025-06-18",
        5,
        "resources/read",
        json!({ "uri": "rustybin://x" }),
    )
    .await;
    assert_eq!(
        body_json(resp).await["error"]["code"],
        protocol::LEGACY_RESOURCE_NOT_FOUND
    );
    let resp = legacy_call(
        &app,
        "/mcp",
        &sid,
        "2025-06-18",
        6,
        "logging/setLevel",
        json!({ "level": "debug" }),
    )
    .await;
    assert_eq!(body_json(resp).await["result"], json!({}));

    // DELETE ends the session.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/mcp")
                .header("mcp-session-id", &sid)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = legacy_call(&app, "/mcp", &sid, "2025-06-18", 7, "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn legacy_version_negotiation_and_old_versions() {
    let app = app();
    let (_, init) = initialize(&app, "/mcp", "1999-01-01", json!({})).await;
    assert_eq!(
        init["result"]["protocolVersion"],
        Version::LATEST_LEGACY.as_str()
    );
    let (sid, init) = initialize(&app, "/mcp", "2024-11-05", json!({})).await;
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
    assert!(init["result"]["capabilities"].get("completions").is_none());
    let resp = legacy_call(&app, "/mcp", &sid, "2024-11-05", 1, "tools/list", json!({})).await;
    let j = body_json(resp).await;
    let t = &j["result"]["tools"][0];
    assert!(t.get("annotations").is_none());
    assert!(t.get("outputSchema").is_none());
    // 2025-03-26 accepts JSON-RPC batches.
    let (sid, _) = initialize(&app, "/mcp", "2025-03-26", json!({})).await;
    let batch = json!([
        rpc(1, "ping", json!({})),
        { "jsonrpc": "2.0", "method": "notifications/initialized" },
        rpc(2, "tools/call", json!({ "name": "add", "arguments": { "a": 2, "b": 3 } })),
    ]);
    let resp = app
        .clone()
        .oneshot(
            legacy_req("/mcp", Some(&sid), None)
                .body(Body::from(batch.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let j = body_json(resp).await;
    assert_eq!(j.as_array().map(Vec::len), Some(2));
    assert_eq!(j[1]["result"]["content"][0]["text"], "5");
}

#[tokio::test]
async fn legacy_progress_and_cancellation() {
    let app = app();
    let (sid, _) = initialize(&app, "/mcp", "2025-11-25", json!({})).await;
    let resp = legacy_call(
        &app,
        "/mcp",
        &sid,
        "2025-11-25",
        1,
        "tools/call",
        json!({ "name": "slow_task", "arguments": { "duration_ms": 60, "steps": 3 }, "_meta": { "progressToken": 7 } }),
    )
    .await;
    assert!(resp.headers()["content-type"]
        .to_str()
        .unwrap_or("")
        .starts_with("text/event-stream"));
    let msgs = sse_messages(&body_string(resp).await);
    assert_eq!(
        msgs.iter()
            .filter(|m| m["method"] == "notifications/progress")
            .count(),
        3
    );
    assert_eq!(msgs.last().expect("final")["id"], 1);

    // notifications/cancelled stops a running request.
    let call = {
        let app = app.clone();
        let sid = sid.clone();
        tokio::spawn(async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .header("accept", "application/json")
                    .header("mcp-session-id", &sid)
                    .body(Body::from(
                        rpc(99, "tools/call", json!({ "name": "slow_task", "arguments": { "duration_ms": 10000, "steps": 2 } }))
                            .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response")
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let cancel = json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 99, "reason": "test" } });
    let resp = app
        .clone()
        .oneshot(
            legacy_req("/mcp", Some(&sid), Some("2025-11-25"))
                .body(Body::from(cancel.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let resp = tokio::time::timeout(Duration::from_secs(3), call)
        .await
        .expect("cancelled quickly")
        .expect("joined");
    assert!(body_json(resp).await["error"]["message"]
        .as_str()
        .unwrap_or("")
        .contains("cancelled"));
}

#[tokio::test]
async fn legacy_elicitation_round_trip() {
    let app = app();
    let (sid, _) = initialize(&app, "/mcp", "2025-11-25", json!({ "elicitation": {} })).await;
    let resp = legacy_call(
        &app,
        "/mcp",
        &sid,
        "2025-11-25",
        10,
        "tools/call",
        json!({ "name": "elicit_confirmation", "arguments": { "action": "reboot" } }),
    )
    .await;
    let mut body = resp.into_body();
    let events = read_events(&mut body, 1).await;
    let req: Value = serde_json::from_str(&events[0].1).expect("request");
    assert_eq!(req["method"], "elicitation/create");
    assert_eq!(req["params"]["mode"], "form");
    let answer = json!({ "jsonrpc": "2.0", "id": req["id"], "result": { "action": "decline" } });
    let resp = app
        .clone()
        .oneshot(
            legacy_req("/mcp", Some(&sid), Some("2025-11-25"))
                .body(Body::from(answer.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let events = read_events(&mut body, 1).await;
    let fin: Value = serde_json::from_str(&events[0].1).expect("final");
    assert_eq!(fin["id"], 10);
    assert!(fin["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .contains("declined"));
}

#[tokio::test]
async fn legacy_get_stream_delivers_resource_updates() {
    let app = app();
    let (sid, _) = initialize(&app, "/mcp", "2025-06-18", json!({})).await;
    let resp = legacy_call(
        &app,
        "/mcp",
        &sid,
        "2025-06-18",
        1,
        "resources/subscribe",
        json!({ "uri": "rustybin://clock" }),
    )
    .await;
    assert_eq!(body_json(resp).await["result"], json!({}));
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/mcp")
                .header("accept", "text/event-stream")
                .header("mcp-session-id", &sid)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    // A second GET stream for the same session conflicts.
    let second = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/mcp")
                .header("accept", "text/event-stream")
                .header("mcp-session-id", &sid)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(second.status(), StatusCode::CONFLICT);
    let mut body = resp.into_body();
    let events = read_events(&mut body, 1).await;
    let n: Value = serde_json::from_str(&events[0].1).expect("notification");
    assert_eq!(n["method"], "notifications/resources/updated");
    assert_eq!(n["params"]["uri"], "rustybin://clock");
}

#[tokio::test]
async fn legacy_http_sse_transport() {
    let app = app();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/mcp/sse")
                .header("accept", "text/event-stream")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body();
    let events = read_events(&mut body, 1).await;
    assert_eq!(events[0].0, "endpoint");
    let endpoint = events[0].1.clone();
    assert!(endpoint.starts_with("/mcp/messages?sessionId="));
    let post = |msg: Value| {
        let app = app.clone();
        let endpoint = endpoint.clone();
        async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri(endpoint)
                    .header("content-type", "application/json")
                    .body(Body::from(msg.to_string()))
                    .expect("request"),
            )
            .await
            .expect("response")
        }
    };
    let resp = post(rpc(1, "initialize", json!({ "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } }))).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let events = read_events(&mut body, 1).await;
    let init: Value = serde_json::from_str(&events[0].1).expect("init");
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
    post(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await;
    let resp = post(rpc(
        2,
        "tools/call",
        json!({ "name": "echo", "arguments": { "message": "over sse" } }),
    ))
    .await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let events = read_events(&mut body, 1).await;
    let r: Value = serde_json::from_str(&events[0].1).expect("result");
    assert_eq!(r["id"], 2);
    assert_eq!(r["result"]["content"][0]["text"], "over sse");
    // Unknown session.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/messages?sessionId=nope")
                .header("content-type", "application/json")
                .body(Body::from(rpc(1, "ping", json!({})).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Variants ────────────────────────────────────────────────────────

#[tokio::test]
async fn protected_resource_urls_use_https_on_the_tls_listener() {
    let app = app();
    let https = crate::session::ListenerInfo::https(8443);
    for path in [
        "/.well-known/oauth-protected-resource/mcp/protected",
        "/.well-known/oauth-protected-resource",
    ] {
        let mut req = Request::builder()
            .uri(path)
            .header("host", "localhost:8443")
            .body(Body::empty())
            .expect("request");
        req.extensions_mut().insert(https);
        let prm = body_json(app.clone().oneshot(req).await.expect("response")).await;
        assert_eq!(prm["resource"], "https://localhost:8443/mcp/protected");
        assert_eq!(
            prm["authorization_servers"],
            json!(["https://localhost:8443"])
        );
        assert_eq!(
            prm["rustybin_authorization_server_metadata"],
            "https://localhost:8443/.well-known/oauth-authorization-server"
        );
    }

    // The 401 challenge points at the https metadata document, and a token
    // for the https resource is accepted.
    let call = |token: Option<String>| {
        let params = modern_params(json!({}), json!({}));
        let mut b = modern_req("/mcp/protected", 1, "tools/list", params.clone())
            .header("host", "localhost:8443");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let mut req = b
            .body(Body::from(rpc(1, "tools/list", params).to_string()))
            .expect("request");
        req.extensions_mut().insert(https);
        app.clone().oneshot(req)
    };
    let resp = call(None).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let challenge = resp.headers()["www-authenticate"].to_str().unwrap_or("");
    assert!(
        challenge.contains(
            "resource_metadata=\"https://localhost:8443/.well-known/oauth-protected-resource/mcp/protected\""
        ),
        "{challenge}"
    );
    let token = mint("https://localhost:8443/mcp/protected", "openid mcp:tools");
    let resp = call(Some(token)).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
}

fn mint(aud: &str, scope: &str) -> String {
    let jwt = crate::jwt_state::JwtState::shared_for_tests();
    let now = chrono::Utc::now().timestamp();
    let claims = json!({
        "iss": "http://localhost", "sub": "alice", "aud": aud,
        "iat": now, "exp": now + 600, "scope": scope,
    });
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some("rustybin-rs256-key".into());
    jsonwebtoken::encode(&header, &claims, &jwt.rs256_encoding_key).expect("token")
}

async fn protected_call(
    app: &Router,
    token: Option<&str>,
    method: &str,
    params: Value,
) -> Response<Body> {
    let params = modern_params(params, json!({}));
    let mut b = modern_req("/mcp/protected", 1, method, params.clone()).header("host", "localhost");
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    app.clone()
        .oneshot(
            b.body(Body::from(rpc(1, method, params).to_string()))
                .expect("request"),
        )
        .await
        .expect("response")
}

#[tokio::test]
async fn protected_variant_oauth() {
    let app = app();
    let resp = protected_call(&app, None, "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let challenge = resp.headers()["www-authenticate"]
        .to_str()
        .unwrap_or("")
        .to_string();
    assert!(challenge.starts_with("Bearer "));
    assert!(challenge.contains(
        "resource_metadata=\"http://localhost/.well-known/oauth-protected-resource/mcp/protected\""
    ));

    for path in [
        "/.well-known/oauth-protected-resource/mcp/protected",
        "/.well-known/oauth-protected-resource",
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "localhost")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let prm = body_json(resp).await;
        assert_eq!(prm["resource"], "http://localhost/mcp/protected");
        assert_eq!(prm["authorization_servers"], json!(["http://localhost"]));
    }

    let good = mint("http://localhost/mcp/protected", "openid mcp:tools");
    let resp = protected_call(&app, Some(&good), "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let wrong_aud = mint("some-other-api", "mcp:tools");
    let resp = protected_call(&app, Some(&wrong_aud), "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(resp.headers()["www-authenticate"]
        .to_str()
        .unwrap_or("")
        .contains("error=\"invalid_token\""));

    let resp = protected_call(&app, Some("not-a-jwt"), "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let cancel = json!({ "name": "cancel_order", "arguments": { "order_id": "o5" } });
    let resp = protected_call(&app, Some(&good), "tools/call", cancel.clone()).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let challenge = resp.headers()["www-authenticate"]
        .to_str()
        .unwrap_or("")
        .to_string();
    assert!(challenge.contains("error=\"insufficient_scope\""));
    assert!(challenge.contains("scope=\"mcp:tools:write\""));

    let writer = mint(
        "http://localhost/mcp/protected",
        "mcp:tools mcp:tools:write",
    );
    let resp = protected_call(&app, Some(&writer), "tools/call", cancel).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        body_json(resp).await["result"]["structuredContent"]["status"],
        "cancelled"
    );

    // The IdP default audience is accepted by default (demo convenience) ...
    let token = mint(super::DEFAULT_IDP_AUDIENCE, "openid");
    let resp = protected_call(&app, Some(&token), "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    // ... and rejected in strict mode (RUSTYBIN_MCP_ACCEPTED_AUDIENCES=none).
    let mut cfg = McpConfig::for_tests();
    cfg.extra_audiences = Vec::new();
    let strict = app_with(cfg);
    let resp = protected_call(&strict, Some(&token), "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let resp = protected_call(&strict, Some(&good), "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn apikey_variant() {
    let app = app();
    let params = modern_params(json!({}), json!({}));
    let send = |key: Option<&'static str>| {
        let app = app.clone();
        let params = params.clone();
        async move {
            let mut b = modern_req("/mcp/apikey", 1, "tools/list", params.clone());
            if let Some(k) = key {
                b = b.header("x-api-key", k);
            }
            app.oneshot(
                b.body(Body::from(rpc(1, "tools/list", params).to_string()))
                    .expect("request"),
            )
            .await
            .expect("response")
        }
    };
    assert_eq!(send(None).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(send(Some("anything")).await.status(), StatusCode::OK);

    let mut cfg = McpConfig::for_tests();
    cfg.api_key = Some("s3cret".into());
    let strict = app_with(cfg);
    let params = modern_params(json!({}), json!({}));
    let resp = strict
        .clone()
        .oneshot(
            modern_req("/mcp/apikey", 1, "tools/list", params.clone())
                .header("x-api-key", "wrong")
                .body(Body::from(rpc(1, "tools/list", params).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sessions_are_bounded() {
    let mut cfg = McpConfig::for_tests();
    cfg.max_sessions = 3;
    let app = app_with(cfg);
    let mut sids = Vec::new();
    for _ in 0..5 {
        sids.push(initialize(&app, "/mcp", "2025-11-25", json!({})).await.0);
    }
    let resp = legacy_call(&app, "/mcp", &sids[0], "2025-11-25", 1, "ping", json!({})).await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "oldest session evicted"
    );
    let resp = legacy_call(&app, "/mcp", &sids[4], "2025-11-25", 1, "ping", json!({})).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn tool_behaviours() {
    let app = app();
    let call = |name: &'static str, args: Value| {
        let app = app.clone();
        async move {
            let resp = modern_call(
                &app,
                "/mcp",
                "tools/call",
                json!({ "name": name, "arguments": args }),
            )
            .await;
            (resp.status(), body_json(resp).await)
        }
    };
    let (_, j) = call("fail", json!({})).await;
    assert_eq!(j["result"]["isError"], true);
    let (status, j) = call("throw", json!({ "code": -32050, "message": "boom" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(j["error"]["code"], -32050);
    let (_, j) = call("calculate", json!({ "expression": "(1+2)*3" })).await;
    assert_eq!(j["result"]["structuredContent"]["result"], 9.0);
    let (_, j) = call("large_output", json!({ "kb": 3 })).await;
    assert_eq!(
        j["result"]["content"][0]["text"].as_str().map(str::len),
        Some(3072)
    );
    let (_, j) = call("generate_image", json!({ "prompt": "sunset", "size": 8 })).await;
    assert_eq!(j["result"]["content"][0]["type"], "image");
    assert_eq!(j["result"]["content"][0]["mimeType"], "image/png");
    let (_, j) = call(
        "fetch_resource_link",
        json!({ "uri": "rustybin://data/customers.json" }),
    )
    .await;
    assert_eq!(j["result"]["content"][1]["type"], "resource_link");
    assert_eq!(
        j["result"]["content"][1]["uri"],
        "rustybin://data/customers.json"
    );
    let (_, j) = call("prompt_injection_demo", json!({})).await;
    assert!(j["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .contains("TEST DATA"));
    assert!(j["result"]["content"][1]["text"]
        .as_str()
        .unwrap_or("")
        .contains("Ignore all previous instructions"));
    let (_, j) = call("get_time", json!({ "timezone": "Europe/Paris" })).await;
    assert_eq!(j["result"]["structuredContent"]["timezone"], "Europe/Paris");
    let (_, j) = call("search_orders", json!({ "status": "pending" })).await;
    assert_eq!(j["result"]["structuredContent"]["count"], 2);
    let (_, j) = call("sample_llm", json!({ "prompt": "hi" })).await;
    assert!(j["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .contains("sampling"));
    let (status, j) = call("nope", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(j["error"]["code"], protocol::INVALID_PARAMS);
}

/// The rendered list cache (JSON answers) must produce exactly the bytes the
/// normal dispatch produces (an SSE-only Accept takes the normal path).
#[tokio::test]
async fn cached_list_results_match_dispatch() {
    for path in ["/mcp", "/mcp/servers/weather", "/mcp/servers/crm"] {
        for method in [
            "tools/list",
            "resources/list",
            "resources/templates/list",
            "prompts/list",
        ] {
            for id in [json!(7), json!("abc")] {
                let mut bodies = Vec::new();
                for accept in ["application/json", "text/event-stream"] {
                    let params = modern_params(Value::Null, json!({}));
                    let req = modern_req_accept(path, 0, method, params.clone(), accept)
                        .body(Body::from(
                            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
                                .to_string(),
                        ))
                        .expect("request");
                    let resp = app().oneshot(req).await.expect("response");
                    let status = resp.status();
                    let text = body_string(resp).await;
                    let text = match text.lines().find_map(|l| l.strip_prefix("data: ")) {
                        Some(data) => data.to_string(),
                        None => text,
                    };
                    bodies.push((status, text));
                }
                if bodies[0].0 != StatusCode::OK {
                    // Errors are not cached (prompts/list on a server without prompts).
                    continue;
                }
                assert_eq!(bodies[0], bodies[1], "{path} {method} {id}");
            }
        }
    }
    // Handshake era (no `_meta`, x-mcp-header stripped from schemas).
    let app = app();
    for version in ["2025-06-18", "2025-03-26"] {
        let (sid, _) = initialize(&app, "/mcp", version, json!({})).await;
        let mut bodies = Vec::new();
        for accept in ["application/json", "text/event-stream"] {
            let req = legacy_req("/mcp", Some(&sid), Some(version))
                .body(Body::from(rpc(3, "tools/list", json!({})).to_string()))
                .expect("request");
            let mut req = req;
            req.headers_mut()
                .insert("accept", accept.parse().expect("accept"));
            let text = body_string(app.clone().oneshot(req).await.expect("response")).await;
            let text = match text.lines().find_map(|l| l.strip_prefix("data: ")) {
                Some(data) => data.to_string(),
                None => text,
            };
            bodies.push(text);
        }
        assert_eq!(bodies[0], bodies[1], "{version}");
        assert!(!bodies[0].contains("x-mcp-header"));
    }
}
