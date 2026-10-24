//! The MCP server of an OpenAPI mock through the full application.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::config::{Config, SelfServiceConfig};
use crate::mcp::protocol;
use crate::test_support::{body_string, json_request, test_app, test_app_with, test_state_with};

const PETSTORE: &str = "/mock/petstore/_rustybin/mcp";
const ACCEPT_BOTH: &str = "application/json, text/event-stream";

fn modern_params(mut params: Value) -> Value {
    if params.is_null() {
        params = json!({});
    }
    params["_meta"] = json!({
        protocol::META_PROTOCOL_VERSION: "2026-07-28",
        protocol::META_CLIENT_CAPABILITIES: {},
        protocol::META_CLIENT_INFO: { "name": "test", "version": "1" },
    });
    params
}

fn modern_request(
    path: &str,
    method: &str,
    params: Value,
    headers: &[(&str, &str)],
) -> Request<Body> {
    let params = modern_params(params);
    let mut b = Request::builder()
        .method("POST")
        .uri(path)
        .header("host", "rustybin.test")
        .header("content-type", "application/json")
        .header("accept", ACCEPT_BOTH)
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", method);
    let name = match method {
        "tools/call" => params.get("name").and_then(Value::as_str),
        "resources/read" => params.get("uri").and_then(Value::as_str),
        _ => None,
    };
    if let Some(n) = name {
        b = b.header("mcp-name", n);
    }
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    b.body(Body::from(body.to_string())).expect("request")
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    (status, body_string(resp).await)
}

async fn rpc(
    app: &Router,
    path: &str,
    method: &str,
    params: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let (status, text) = send(app, modern_request(path, method, params, headers)).await;
    let body = serde_json::from_str(&text).unwrap_or_else(|_| {
        // An SSE answer: the last message is the response.
        sse_messages(&text).pop().unwrap_or(Value::String(text))
    });
    (status, body)
}

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

async fn call_tool(app: &Router, path: &str, name: &str, args: Value) -> Value {
    call_tool_with(app, path, name, args, &[]).await
}

async fn call_tool_with(
    app: &Router,
    path: &str,
    name: &str,
    args: Value,
    headers: &[(&str, &str)],
) -> Value {
    let (status, body) = rpc(
        app,
        path,
        "tools/call",
        json!({ "name": name, "arguments": args }),
        headers,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["result"].clone()
}

async fn patch_petstore(app: &Router, body: Value) {
    let resp = app
        .clone()
        .oneshot(json_request("PATCH", "/_rustybin/mocks/petstore", &body))
        .await
        .expect("response");
    let status = resp.status();
    let text = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "{text}");
}

#[tokio::test]
async fn discover_and_list_the_petstore_tools() {
    let app = test_app();
    let (status, body) = rpc(&app, PETSTORE, "server/discover", json!({}), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let result = &body["result"];
    assert!(result["capabilities"]["tools"].is_object());
    assert!(result["capabilities"]["resources"].is_object());
    assert!(result["capabilities"].get("prompts").is_none());
    assert!(result["instructions"]
        .as_str()
        .is_some_and(|t| t.contains("Petstore") && t.contains("mock")));
    let info = &result["_meta"][protocol::META_SERVER_INFO];
    assert_eq!(info["name"], "rustybin-mock-petstore");
    assert_eq!(info["title"], "Petstore");

    let (_, body) = rpc(&app, PETSTORE, "tools/list", json!({}), &[]).await;
    let tools = body["result"]["tools"].as_array().expect("tools");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(
        names,
        vec![
            "listPets",
            "createPet",
            "getPetById",
            "updatePet",
            "deletePet",
            "placeOrder"
        ]
    );
    let get = &tools[2];
    assert_eq!(get["annotations"]["readOnlyHint"], true);
    assert_eq!(get["annotations"]["openWorldHint"], false);
    assert_eq!(get["inputSchema"]["required"], json!(["petId"]));
    assert_eq!(get["outputSchema"]["type"], "object");
    assert_eq!(
        get["_meta"]["rustybin.dev/operation"]["path"],
        "/pets/{petId}"
    );
    assert_eq!(tools[4]["annotations"]["destructiveHint"], true);
    // None of the demo server's tools leak in.
    assert!(!names.contains(&"get_weather"));

    // Paginated with the transport's page size.
    let (_, page) = rpc(
        &app,
        "/mock/petstore/_rustybin/mcp?page_size=4",
        "tools/list",
        json!({}),
        &[],
    )
    .await;
    assert_eq!(page["result"]["tools"].as_array().map(Vec::len), Some(4));
    let cursor = page["result"]["nextCursor"]
        .as_str()
        .expect("cursor")
        .to_string();
    let (_, rest) = rpc(
        &app,
        "/mock/petstore/_rustybin/mcp?page_size=4",
        "tools/list",
        json!({ "cursor": cursor }),
        &[],
    )
    .await;
    assert_eq!(rest["result"]["tools"].as_array().map(Vec::len), Some(2));
}

#[tokio::test]
async fn calls_answer_like_the_rest_endpoints() {
    let app = test_app();
    let r = call_tool(&app, PETSTORE, "getPetById", json!({ "petId": 7 })).await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["status"], 200);
    assert_eq!(r["structuredContent"]["body"]["id"], 7);
    // The text is the same JSON (for clients without structured output).
    let text: Value =
        serde_json::from_str(r["content"][0]["text"].as_str().expect("text")).expect("json text");
    assert_eq!(text, r["structuredContent"]);

    // The sample's rule: pet 0 is a 404, so a tool error with the problem body.
    let r = call_tool(&app, PETSTORE, "getPetById", json!({ "petId": 0 })).await;
    assert_eq!(r["isError"], true);
    assert_eq!(r["structuredContent"]["status"], 404);
    assert_eq!(r["structuredContent"]["body"]["message"], "pet not found");

    let r = call_tool(
        &app,
        PETSTORE,
        "listPets",
        json!({ "limit": 2, "status": "sold" }),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert!(r["structuredContent"]["body"].is_array());

    // Captures show the MCP channel and the tool.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/_rustybin/mocks/petstore/requests")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let log: Value = serde_json::from_str(&body_string(resp).await).expect("json");
    let entries = log["requests"].as_array().expect("requests");
    let mcp: Vec<&Value> = entries.iter().filter(|e| e["channel"] == "mcp").collect();
    assert_eq!(mcp.len(), 3, "{log}");
    assert!(mcp
        .iter()
        .any(|e| e["tool"] == "listPets"
            && e["query"].as_str().is_some_and(|q| q.contains("limit=2"))));
    assert!(mcp.iter().any(|e| e["path"] == "/v1/pets/7"));
}

#[tokio::test]
async fn bad_arguments_and_unknown_tools() {
    let app = test_app();
    let r = call_tool(&app, PETSTORE, "getPetById", json!({})).await;
    assert_eq!(r["isError"], true);
    let msg = r["content"][0]["text"].as_str().unwrap_or_default();
    assert!(msg.contains("missing required argument \"petId\""), "{msg}");
    let r = call_tool(
        &app,
        PETSTORE,
        "getPetById",
        json!({ "petId": 1, "nope": 2 }),
    )
    .await;
    assert!(r["content"][0]["text"]
        .as_str()
        .is_some_and(|m| m.contains("unknown argument \"nope\"")));
    let r = call_tool(&app, PETSTORE, "getPetById", json!({ "petId": { "a": 1 } })).await;
    assert_eq!(r["isError"], true);
    let r = call_tool(&app, PETSTORE, "createPet", json!({})).await;
    assert!(r["content"][0]["text"]
        .as_str()
        .is_some_and(|m| m.contains("\"body\"")));

    // Request validation is the mock's: enforce answers 400 with the errors.
    patch_petstore(
        &app,
        json!({ "responses": { "validate_requests": "enforce" }, "deploy": true }),
    )
    .await;
    let r = call_tool(
        &app,
        PETSTORE,
        "createPet",
        json!({ "body": { "tag": "x" } }),
    )
    .await;
    assert_eq!(r["isError"], true);
    assert_eq!(r["structuredContent"]["status"], 400);
    assert!(r["structuredContent"]["body"]["errors"].is_array(), "{r}");

    let (status, body) = rpc(
        &app,
        PETSTORE,
        "tools/call",
        json!({ "name": "get_weather", "arguments": {} }),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], protocol::INVALID_PARAMS);
}

#[tokio::test]
async fn mocked_security_uses_the_mcp_request_credentials() {
    let app = test_app();
    patch_petstore(
        &app,
        json!({ "security": { "enabled": true }, "deploy": true }),
    )
    .await;
    let pet = json!({ "body": { "name": "Rex" } });
    let r = call_tool(&app, PETSTORE, "createPet", pet.clone()).await;
    assert_eq!(r["isError"], true);
    assert_eq!(r["structuredContent"]["status"], 401);
    let r = call_tool_with(
        &app,
        PETSTORE,
        "createPet",
        pet.clone(),
        &[("x-api-key", "demo-key")],
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["status"], 201);
    assert_eq!(r["structuredContent"]["body"]["name"], "Rex");
    // A token from the built-in identity provider.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/oauth/token")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "grant_type=client_credentials&client_id=rustybin&client_secret=secret",
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    let token: Value = serde_json::from_str(&body_string(resp).await).expect("json");
    let auth = format!("Bearer {}", token["access_token"].as_str().expect("token"));
    let r = call_tool_with(
        &app,
        PETSTORE,
        "createPet",
        pet,
        &[("authorization", &auth)],
    )
    .await;
    assert_eq!(r["structuredContent"]["status"], 201, "{r}");
    // The credential headers are not tool arguments.
    let (_, body) = rpc(&app, PETSTORE, "tools/list", json!({}), &[]).await;
    assert!(!body.to_string().contains("X-API-Key\""));
}

#[tokio::test]
async fn stateful_mocks_keep_what_tools_create() {
    let app = test_app();
    patch_petstore(
        &app,
        json!({ "responses": { "mode": "stateful" }, "deploy": true }),
    )
    .await;
    let r = call_tool(
        &app,
        PETSTORE,
        "createPet",
        json!({ "body": { "name": "Nemo", "tag": "fish" } }),
    )
    .await;
    assert_eq!(r["structuredContent"]["status"], 201, "{r}");
    let id = r["structuredContent"]["body"]["id"].clone();
    assert!(id.is_number(), "{r}");
    let r = call_tool(&app, PETSTORE, "getPetById", json!({ "petId": id })).await;
    assert_eq!(r["structuredContent"]["body"]["name"], "Nemo", "{r}");
    // The same data over REST.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/mock/petstore/v1/pets/{id}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let rest: Value = serde_json::from_str(&body_string(resp).await).expect("json");
    assert_eq!(rest["name"], "Nemo");
}

#[tokio::test]
async fn off_schema_bodies_are_tool_errors() {
    let app = test_app();
    patch_petstore(
        &app,
        json!({
            "operations": { "getPetById": { "respond": { "status": 200, "body": { "oops": true } } } },
            "deploy": true,
            "force": true
        }),
    )
    .await;
    let r = call_tool(&app, PETSTORE, "getPetById", json!({ "petId": 3 })).await;
    assert_eq!(r["isError"], true, "{r}");
    assert!(r["content"][0]["text"]
        .as_str()
        .is_some_and(|t| t.contains("does not match the response schema")));
}

#[tokio::test]
async fn slow_answers_report_progress() {
    let app = test_app();
    patch_petstore(
        &app,
        json!({ "operations": { "listPets": { "delay_ms": 1200 } }, "deploy": true }),
    )
    .await;
    let mut params = modern_params(json!({ "name": "listPets", "arguments": {} }));
    params["_meta"]["progressToken"] = json!("p1");
    let body = json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": params });
    let req = Request::builder()
        .method("POST")
        .uri(PETSTORE)
        .header("content-type", "application/json")
        .header("accept", ACCEPT_BOTH)
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", "tools/call")
        .header("mcp-name", "listPets")
        .body(Body::from(body.to_string()))
        .expect("request");
    let (status, text) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let messages = sse_messages(&text);
    assert!(
        messages.iter().any(
            |m| m["method"] == "notifications/progress" && m["params"]["progressToken"] == "p1"
        ),
        "{text}"
    );
    assert_eq!(
        messages.last().map(|m| m["result"]["isError"].clone()),
        Some(json!(false))
    );
}

#[tokio::test]
async fn the_spec_is_a_resource() {
    let app = test_app();
    let (_, body) = rpc(&app, PETSTORE, "resources/list", json!({}), &[]).await;
    let uri = body["result"]["resources"][0]["uri"]
        .as_str()
        .expect("uri")
        .to_string();
    assert_eq!(uri, "rustybin://mock/petstore/openapi.json");
    let (_, body) = rpc(&app, PETSTORE, "resources/read", json!({ "uri": uri }), &[]).await;
    let text = body["result"]["contents"][0]["text"]
        .as_str()
        .expect("text");
    let doc: Value = serde_json::from_str(text).expect("spec");
    assert_eq!(doc["info"]["title"], "Petstore");
    assert_eq!(
        doc["servers"][0]["url"],
        "http://rustybin.test/mock/petstore/v1"
    );
    assert_eq!(
        doc["x-rustybin-mcp"]["url"],
        "http://rustybin.test/mock/petstore/_rustybin/mcp"
    );
    let (status, _) = rpc(
        &app,
        PETSTORE,
        "resources/read",
        json!({ "uri": "rustybin://nope" }),
        &[],
    )
    .await;
    assert_ne!(status, StatusCode::OK);
}

#[tokio::test]
async fn no_server_for_missing_undeployed_off_or_disabled_mocks() {
    let app = test_app();
    let (status, body) = rpc(
        &app,
        "/mock/nope/_rustybin/mcp",
        "tools/list",
        json!({}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"]["message"]
        .as_str()
        .is_some_and(|m| m.contains("no MCP server")));

    patch_petstore(&app, json!({ "mcp": { "enabled": false }, "deploy": true })).await;
    let (status, body) = rpc(&app, PETSTORE, "tools/list", json!({}), &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.to_string().contains("mcp.enabled"));
    patch_petstore(
        &app,
        json!({ "mcp": { "enabled": true, "exclude": ["deletePet"] }, "deploy": true }),
    )
    .await;
    let (_, body) = rpc(&app, PETSTORE, "tools/list", json!({}), &[]).await;
    assert_eq!(body["result"]["tools"].as_array().map(Vec::len), Some(5));

    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/mocks/petstore/disable",
            &json!({}),
        ))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let (status, body) = rpc(&app, PETSTORE, "tools/list", json!({}), &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.to_string().contains("switched off"));

    // A draft that was never deployed.
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/mocks",
            &json!({ "name": "draft", "spec": crate::mocks::SAMPLE_SPEC, "deploy": false }),
        ))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let (status, _) = rpc(
        &app,
        "/mock/draft/_rustybin/mcp",
        "tools/list",
        json!({}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn handshake_sessions_belong_to_one_mock() {
    let app = test_app();
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/mocks",
            &json!({ "name": "other", "spec": crate::mocks::SAMPLE_SPEC }),
        ))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let init = json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } } });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(PETSTORE)
                .header("content-type", "application/json")
                .header("accept", ACCEPT_BOTH)
                .body(Body::from(init.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let sid = resp.headers()["mcp-session-id"]
        .to_str()
        .expect("session")
        .to_string();
    let init_result: Value = serde_json::from_str(&body_string(resp).await).expect("json");
    assert_eq!(
        init_result["result"]["serverInfo"]["name"],
        "rustybin-mock-petstore"
    );
    let list = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} });
    let req = |path: &str| {
        Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("accept", ACCEPT_BOTH)
            .header("mcp-session-id", sid.as_str())
            .header("mcp-protocol-version", "2025-06-18")
            .body(Body::from(list.to_string()))
            .expect("request")
    };
    let (status, text) = send(&app, req(PETSTORE)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("getPetById"));
    let (status, _) = send(&app, req("/mock/other/_rustybin/mcp")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&app, req("/mcp")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// `req` sent to another host.
fn on_host(req: Request<Body>, host: &str) -> Request<Body> {
    let (mut parts, body) = req.into_parts();
    parts.headers.insert("host", host.parse().expect("host"));
    Request::from_parts(parts, body)
}

#[tokio::test]
async fn host_based_urls_reach_the_mcp_server() {
    let mut config = Config::for_tests();
    config.mocks_host = Some("mock.test".into());
    let app = test_app_with(test_state_with(config));
    // petstore.mock.test/_rustybin/mcp is the mock's server, not the
    // management server at /_rustybin/mcp.
    let req = modern_request("/_rustybin/mcp", "tools/list", json!({}), &[]);
    let (status, text) = send(&app, on_host(req, "petstore.mock.test")).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("getPetById"));
    let req = modern_request(
        "/_rustybin/mcp",
        "tools/call",
        json!({ "name": "getPetById", "arguments": { "petId": 2 } }),
        &[],
    );
    let (_, text) = send(&app, on_host(req, "petstore.mock.test")).await;
    let r: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(r["result"]["structuredContent"]["body"]["id"], 2, "{r}");
}

const SHOP: &str = r#"{"openapi":"3.0.3","info":{"title":"Shop","version":"1"},
  "paths":{"/items/{id}":{"get":{"operationId":"item","parameters":[{"name":"id","in":"path","required":true,
    "schema":{"type":"integer"}}],"responses":{"200":{"description":"ok","content":{"application/json":
    {"schema":{"type":"object","properties":{"id":{"type":"integer"}}}}}}}}}}}"#;

#[tokio::test]
async fn visitor_mocks_serve_mcp_on_the_mocks_domain_only() {
    let mut config = Config::for_tests();
    config.public_mode = true;
    config.mocks_sample = false;
    config.admin_token = Some("admin-secret".into());
    config.self_service = SelfServiceConfig {
        enabled: true,
        host: Some("mocks.example".into()),
        home_url: Some("https://home.example/".into()),
        leads_salt: Some("test-salt".into()),
        ..SelfServiceConfig::default()
    };
    let app = test_app_with(test_state_with(config));
    let req = Request::builder()
        .method("POST")
        .uri("/_rustybin/mocks")
        .header("host", "rustybin.test")
        .header("x-rustybin-session", "visitor-a")
        .header("content-type", "application/json")
        .body(Body::from(json!({ "spec": SHOP }).to_string()))
        .expect("request");
    let (status, text) = send(&app, req).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let mock: Value = serde_json::from_str(&text).expect("json");
    let name = mock["name"].as_str().expect("name").to_string();

    // Not on the main host: the same answer as for a mock that does not exist.
    let path = format!("/mock/{name}/_rustybin/mcp");
    let (status, body) = rpc(&app, &path, "tools/list", json!({}), &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, missing) = rpc(
        &app,
        "/mock/nobody-1234/_rustybin/mcp",
        "tools/list",
        json!({}),
        &[],
    )
    .await;
    assert_eq!(body["error"]["message"], missing["error"]["message"]);

    // On the mocks domain it works.
    let host = format!("{name}.mocks.example");
    let req = modern_request(
        "/_rustybin/mcp",
        "tools/call",
        json!({ "name": "item", "arguments": { "id": 5 } }),
        &[],
    );
    let (status, text) = send(&app, on_host(req, &host)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let r: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(r["result"]["structuredContent"]["body"]["id"], 5, "{r}");
}

#[tokio::test]
async fn the_mock_detail_describes_its_mcp_server() {
    let mut config = Config::for_tests();
    config.mocks_host = Some("mock.test".into());
    let app = test_app_with(test_state_with(config));
    let get = |uri: &str| {
        Request::builder()
            .uri(uri)
            .header("host", "rustybin.test")
            .body(Body::empty())
            .expect("request")
    };
    let (_, text) = send(&app, get("/_rustybin/mocks/petstore")).await;
    let d: Value = serde_json::from_str(&text).expect("json");
    let mcp = &d["mcp"];
    assert_eq!(
        mcp["url"],
        "http://rustybin.test/mock/petstore/_rustybin/mcp"
    );
    assert_eq!(mcp["host_url"], "http://petstore.mock.test/_rustybin/mcp");
    assert_eq!(mcp["enabled"], true);
    assert_eq!(mcp["serving"], true);
    assert_eq!(mcp["tools"].as_array().map(Vec::len), Some(6));
    assert_eq!(mcp["tools"][0]["name"], "listPets");
    assert_eq!(mcp["skipped"], json!([]));

    patch_petstore(
        &app,
        json!({ "mcp": { "enabled": false, "exclude": ["listPets"] } }),
    )
    .await;
    let (_, text) = send(&app, get("/_rustybin/mocks/petstore")).await;
    let d: Value = serde_json::from_str(&text).expect("json");
    // The live revision still serves; the draft is not deployed yet.
    assert_eq!(d["mcp"]["serving"], true);
    assert!(d["draft_changes"].to_string().contains("mcp"));
    assert_eq!(d["settings"]["mcp"]["enabled"], false);

    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/mocks/petstore/deploy",
            &json!({}),
        ))
        .await
        .expect("response");
    let out: Value = serde_json::from_str(&body_string(resp).await).expect("json");
    assert_eq!(out["mcp_url"], Value::Null);
    let (_, text) = send(&app, get("/_rustybin/mocks/petstore")).await;
    let d: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(d["mcp"]["serving"], false);
    assert_eq!(d["mcp"]["skipped"][0]["operation"], "listPets");
    let (_, text) = send(&app, get("/mock/petstore/_rustybin/openapi.json")).await;
    let doc: Value = serde_json::from_str(&text).expect("json");
    assert!(doc.get("x-rustybin-mcp").is_none());

    // Exported as configuration.
    let (_, text) = send(&app, get("/_rustybin/mocks/petstore/export")).await;
    assert!(text.contains("mcp:"), "{text}");
    assert!(text.contains("- listPets"), "{text}");
}
