use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::assist::provider::{Event, Problem};
use crate::assist::tasks;
use crate::config::Config;
use crate::test_support::{body_bytes, body_json, json_request, test_app_with, test_state_with};

/// A spec with one operation that has an example and two that have none.
const SHOP: &str = r##"{
  "openapi": "3.0.3", "info": {"title": "Shop", "version": "1.0"},
  "paths": {
    "/products": {
      "get": {"operationId": "listProducts", "responses": {"200": {"description": "ok", "content": {"application/json": {
        "schema": {"type": "array", "items": {"$ref": "#/components/schemas/Product"}}}}}}},
      "post": {"operationId": "createProduct",
        "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Product"}}}},
        "responses": {"201": {"description": "created", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Product"}}}}}}
    },
    "/products/{id}": {
      "get": {"operationId": "getProduct",
        "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "integer"}}],
        "responses": {"200": {"description": "ok", "content": {"application/json": {
          "schema": {"$ref": "#/components/schemas/Product"},
          "example": {"id": 1, "name": "Mug", "price": 9.5}}}},
          "404": {"description": "missing"}}},
      "delete": {"operationId": "deleteProduct",
        "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "integer"}}],
        "responses": {"204": {"description": "gone"}}}
    }
  },
  "components": {"schemas": {"Product": {"type": "object", "required": ["id", "name", "price"],
    "properties": {"id": {"type": "integer", "minimum": 1}, "name": {"type": "string", "minLength": 1}, "price": {"type": "number", "minimum": 0}}}}}
}"##;

fn mock_config() -> Config {
    let mut c = Config::for_tests();
    c.assist_provider = Some("mock".into());
    c
}

async fn create(app: &Router, name: &str, spec: &str) -> Value {
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/mocks",
            &json!({ "name": name, "spec": spec, "deploy": true }),
        ))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::CREATED);
    body_json(resp).await
}

async fn post(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(json_request("POST", uri, &body))
        .await
        .expect("r");
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).expect("req"))
        .await
        .expect("r");
    let status = resp.status();
    (status, body_json(resp).await)
}

fn script(entries: Vec<Result<Vec<Event>, Problem>>) -> crate::assist::provider::Script {
    Arc::new(Mutex::new(VecDeque::from(entries)))
}

// ── Coverage, stored examples, precedence ───────────────────────────

#[tokio::test]
async fn coverage_counts_spec_and_stored_examples() {
    let state = test_state_with(Config::for_tests());
    let app = test_app_with(state.clone());
    let d = create(&app, "shop", SHOP).await;
    // getProduct has one; listProducts and createProduct lack one; deleteProduct has no body.
    assert_eq!(d["coverage"]["operations"], 4);
    assert_eq!(d["coverage"]["with_examples"], 2);
    assert_eq!(d["coverage"]["complete"], false);
    let without: Vec<&str> = d["coverage"]["without"]
        .as_array()
        .map(|a| a.iter().filter_map(|o| o["id"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(without, vec!["listProducts", "createProduct"]);
    assert_eq!(
        d["report"]["findings"]
            .as_array()
            .map(|f| f.iter().filter(|x| x["class"] == "no_examples").count()),
        Some(1)
    );
    let rows = d["operations"].as_array().expect("rows");
    let list = rows
        .iter()
        .find(|r| r["id"] == "listProducts")
        .expect("row");
    assert_eq!(list["missing_examples"], json!([200]));
    assert_eq!(list["stored_examples"], 0);
    let (status, cov) = get(&app, "/_rustybin/mocks/shop/coverage").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cov["without"].as_array().map(Vec::len), Some(2));
    assert_eq!(cov["live"]["with_examples"], 2);
    // The report route and the MCP check_mock carry it too.
    let (_, report) = get(&app, "/_rustybin/mocks/shop/report").await;
    assert_eq!(report["coverage"]["with_examples"], 2);

    // Stored examples count, by status; a bad one is refused by the settings.
    let bad = json!({ "operations": { "listProducts": { "examples": { "200": [{ "name": "", "value": [] }] } } } });
    let resp = app
        .clone()
        .oneshot(json_request("PATCH", "/_rustybin/mocks/shop", &bad))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let good = json!({ "operations": {
        "listProducts": { "examples": { "200": [{ "name": "two", "value": [{"id": 1, "name": "Mug", "price": 9.5}, {"id": 2, "name": "Pen", "price": 1.5}], "source": "user" }] } } } });
    let resp = app
        .clone()
        .oneshot(json_request("PATCH", "/_rustybin/mocks/shop", &good))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::OK);
    let d = body_json(resp).await;
    assert_eq!(d["coverage"]["with_examples"], 3);
    assert_eq!(d["coverage"]["without"][0]["id"], "createProduct");
    // The stored example is served where the spec has none (after a deploy), by name too.
    let (status, _) = post(&app, "/_rustybin/mocks/shop/deploy", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(
            Request::get("/mock/shop/products")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("x-rustybin-mock-source")
            .and_then(|v| v.to_str().ok()),
        Some("example:user:two")
    );
    let v = body_json(resp).await;
    assert_eq!(v[1]["name"], "Pen");
    let resp = app
        .clone()
        .oneshot(
            Request::get("/mock/shop/products")
                .header("prefer", "example=two")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("r");
    assert_eq!(
        resp.headers()
            .get("x-rustybin-mock-source")
            .and_then(|v| v.to_str().ok()),
        Some("prefer example:user:two")
    );
    // The spec's own example still wins where it exists.
    let resp = app
        .clone()
        .oneshot(
            Request::get("/mock/shop/products/1")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("r");
    assert_eq!(
        resp.headers()
            .get("x-rustybin-mock-source")
            .and_then(|v| v.to_str().ok()),
        Some("example")
    );
}

#[tokio::test]
async fn generated_examples_need_no_key_and_fill_the_coverage() {
    let state = test_state_with(Config::for_tests());
    let app = test_app_with(state.clone());
    create(&app, "shop", SHOP).await;
    // Preview only.
    let (status, v) = post(
        &app,
        "/_rustybin/mocks/shop/ai/examples",
        json!({ "source": "generated", "per_response": 2, "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["applied"], false);
    assert_eq!(
        v["examples"]["listProducts"]["200"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert!(v["examples"]["createProduct"]["201"][0]["price"].is_number());
    assert!(v.get("coverage").is_none());
    // Apply: stored as source generated; the coverage is complete; the mock serves them.
    let (status, v) = post(
        &app,
        "/_rustybin/mocks/shop/ai/examples",
        json!({ "source": "generated", "apply": true, "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["applied"], true);
    assert_eq!(v["stored"], 2);
    assert_eq!(v["coverage"]["complete"], true);
    let (_, d) = get(&app, "/_rustybin/mocks/shop").await;
    assert_eq!(
        d["settings"]["operations"]["listProducts"]["examples"]["200"][0]["source"],
        "generated"
    );
    assert_eq!(
        d["settings"]["operations"]["listProducts"]["examples"]["200"][0]["name"],
        "generated-1"
    );
    assert!(d["has_draft_changes"].as_bool().unwrap_or(false));
    // Nothing left to do.
    let (status, v) = post(
        &app,
        "/_rustybin/mocks/shop/ai/examples",
        json!({ "source": "generated", "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["reason"], "nothing_to_do");
    // Named operations are redone even when covered; the generator's earlier
    // examples are replaced, not stacked.
    let (status, v) = post(&app, "/_rustybin/mocks/shop/ai/examples", json!({ "source": "generated", "operations": ["listProducts"], "per_response": 3, "apply": true, "stream": false })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (_, d) = get(&app, "/_rustybin/mocks/shop").await;
    assert_eq!(
        d["settings"]["operations"]["listProducts"]["examples"]["200"]
            .as_array()
            .map(Vec::len),
        Some(3)
    );
    // The MCP tool of the operation answers with the stored example.
    post(&app, "/_rustybin/mocks/shop/deploy", json!({})).await;
    let req = Request::builder()
        .method("POST")
        .uri("/mock/shop/_rustybin/mcp")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", "tools/call")
        .header("mcp-name", "listProducts")
        .body(Body::from(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "listProducts", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}).to_string()))
        .expect("req");
    let resp = app.clone().oneshot(req).await.expect("r");
    let v = body_json(resp).await;
    assert_eq!(v["result"]["isError"], false, "{v}");
    let body = &v["result"]["structuredContent"]["body"];
    assert!(body.is_array() && body[0]["price"].is_number(), "{v}");
}

// ── The AI routes through the mock provider ─────────────────────────

#[tokio::test]
async fn describe_creates_a_checked_spec_on_every_version() {
    let app = test_app_with(test_state_with(mock_config()));
    for (version, auth, marker) in [
        ("3.1.0", "api_key", "3.1"),
        ("3.0.3", "bearer", "3.0"),
        ("2.0", "oauth2", "2.0"),
    ] {
        let (status, v) = post(&app, SPEC_PATH, json!({
            "description": "An orders API for a coffee shop: customers, products, orders with line items",
            "openapi_version": version,
            "style": { "auth": auth, "pagination": "cursor", "errors": "problem" },
            "stream": false,
        })).await;
        assert_eq!(status, StatusCode::OK, "{version}: {v}");
        // Swagger 2.0 is converted to 3.0 on parse: the preview says 3.0, the text keeps 2.0.
        let text = v["text"].as_str().unwrap_or("");
        if version == "2.0" {
            assert!(text.contains("\"swagger\": \"2.0\""), "{version}: {text}");
            assert!(
                v["preview"]["openapi"]
                    .as_str()
                    .is_some_and(|o| o.starts_with("3.0")),
                "{version}: {}",
                v["preview"]["openapi"]
            );
        } else {
            assert!(
                v["preview"]["openapi"]
                    .as_str()
                    .is_some_and(|o| o.starts_with(marker)),
                "{version}: {}",
                v["preview"]["openapi"]
            );
        }
        assert_eq!(
            v["preview"]["report"]["errors"], 0,
            "{version}: {}",
            v["preview"]
        );
        assert_eq!(v["preview"]["coverage"]["complete"], true, "{version}");
        assert!(
            v["preview"]["operations"]
                .as_array()
                .is_some_and(|o| o.len() >= 8),
            "{version}"
        );
        assert_eq!(v["preview"]["declares_security"], true, "{version}");
        assert!(v["text"].as_str().is_some_and(|t| t.contains("customers")));
        assert_eq!(v["usage"]["provider"], "mock");
        // The document creates a mock that answers.
        let name = format!("coffee-{}", version.replace('.', "-"));
        let d = create(&app, &name, v["text"].as_str().unwrap_or("")).await;
        assert_eq!(d["report"]["ok"], true);
        let resp = app
            .clone()
            .oneshot(
                Request::get(format!("/mock/{name}/v1/customers"))
                    .header("x-api-key", "demo")
                    .body(Body::empty())
                    .expect("req"),
            )
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::OK, "{version}");
    }
    // Streamed: progress, usage, result, done.
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            SPEC_PATH,
            &json!({ "description": "A notes API: notes with tags", "stream": true }),
        ))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::OK);
    let text = String::from_utf8(body_bytes(resp).await.to_vec()).expect("utf8");
    assert!(text.contains("event: progress"), "{text}");
    assert!(text.contains("event: result"), "{text}");
    assert!(text.ends_with("event: done\ndata: {}\n\n"), "{text}");
    // Bad inputs.
    let (status, v) = post(
        &app,
        SPEC_PATH,
        json!({ "description": "", "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let (status, _) = post(
        &app,
        SPEC_PATH,
        json!({ "description": "x", "openapi_version": "4.0", "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ai_examples_and_edit_through_the_mock_provider() {
    let state = test_state_with(mock_config());
    let app = test_app_with(state.clone());
    create(&app, "shop", SHOP).await;
    let (status, v) = post(
        &app,
        "/_rustybin/mocks/shop/ai/examples",
        json!({ "apply": true, "persona": "edge cases", "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["source"], "ai");
    assert_eq!(v["coverage"]["complete"], true);
    assert_eq!(v["usage"]["provider"], "mock");
    let (_, d) = get(&app, "/_rustybin/mocks/shop").await;
    assert_eq!(
        d["settings"]["operations"]["createProduct"]["examples"]["201"][0]["source"],
        "ai"
    );
    assert_eq!(
        d["settings"]["operations"]["createProduct"]["examples"]["201"][0]["name"],
        "ai-1"
    );

    // Edit: a diff with the added 404s and the DELETE; apply saves the draft.
    let (status, v) = post(
        &app,
        "/_rustybin/mocks/shop/ai/edit",
        json!({ "instruction": "add a 404 response to every operation", "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["applied"], false);
    assert!(
        v["diff"]["changed"]
            .as_array()
            .is_some_and(|c| c.len() >= 2),
        "{}",
        v["diff"]
    );
    assert_eq!(v["diff"]["info_changed"], true);
    assert_eq!(v["preview"]["report"]["errors"], 0);
    assert!(v["before_text"]
        .as_str()
        .is_some_and(|t| !t.contains("AI edit")));
    assert!(v["text"].as_str().is_some_and(|t| t.contains("AI edit")));
    let (_, d) = get(&app, "/_rustybin/mocks/shop").await;
    assert!(
        d["draft_changes"]
            .as_array()
            .is_none_or(|c| !c.iter().any(|x| x == "spec")),
        "preview changed nothing"
    );
    let (status, v) = post(&app, "/_rustybin/mocks/shop/ai/edit", json!({ "instruction": "add a 404 response to every operation", "apply": true, "stream": false })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["applied"], true);
    assert!(v["detail"]["draft_changes"]
        .as_array()
        .is_some_and(|c| c.iter().any(|x| x == "spec")));
    let (_, d) = get(&app, "/_rustybin/mocks/shop").await;
    let list = d["operations"]
        .as_array()
        .and_then(|o| o.iter().find(|r| r["id"] == "listProducts"))
        .cloned()
        .expect("row");
    assert!(list["statuses"]
        .as_array()
        .is_some_and(|s| s.contains(&json!("404"))));
}

#[tokio::test]
async fn edit_rejects_unchanged_and_destructive_answers() {
    let state = test_state_with(mock_config());
    let app = test_app_with(state.clone());
    create(&app, "shop", SHOP).await;
    let before: Value = serde_json::from_str(SHOP).expect("json");
    // Unchanged twice, then a removal of every operation without an ask, then a valid change.
    let mut gutted = before.clone();
    gutted["paths"] =
        json!({ "/products": { "get": before["paths"]["/products"]["get"].clone() } });
    let mut fine = before.clone();
    fine["info"]["description"] = json!("edited");
    state.assist.set_script(Some(script(vec![
        Ok(vec![Event::Delta(before.to_string())]),
        Ok(vec![Event::Delta(gutted.to_string())]),
        Ok(vec![Event::Delta(fine.to_string())]),
    ])));
    let (status, v) = post(
        &app,
        "/_rustybin/mocks/shop/ai/edit",
        json!({ "instruction": "describe it", "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["diff"]["info_changed"], true);
    assert_eq!(v["diff"]["removed"].as_array().map(Vec::len), Some(0));
    // A removal the instruction asks for passes.
    state
        .assist
        .set_script(Some(script(vec![Ok(vec![Event::Delta(
            gutted.to_string(),
        )])])));
    let (status, v) = post(
        &app,
        "/_rustybin/mocks/shop/ai/edit",
        json!({ "instruction": "remove everything but the product list", "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["diff"]["removed"].as_array().map(Vec::len), Some(3));
    // Three bad answers: the error names the last problem.
    state.assist.set_script(Some(script(vec![
        Ok(vec![Event::Delta("{\"openapi\": \"3.0.3\"}".into())]),
        Ok(vec![Event::Delta(before.to_string())]),
        Ok(vec![Event::Delta("nonsense".into())]),
    ])));
    let (status, v) = post(
        &app,
        "/_rustybin/mocks/shop/ai/edit",
        json!({ "instruction": "describe it", "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{v}");
    assert!(
        v["error"]
            .as_str()
            .is_some_and(|e| e.contains("after 3 attempts")),
        "{v}"
    );
    state.assist.set_script(None);
}

#[tokio::test]
async fn examples_task_validates_and_repairs() {
    let state = test_state_with(mock_config());
    let app = test_app_with(state.clone());
    create(&app, "shop", SHOP).await;
    // The first answer has an invalid example (price negative, name missing); the second is fine.
    state.assist.set_script(Some(script(vec![
        Ok(vec![Event::Delta(json!({ "examples": { "listProducts": { "200": [[{"id": 1, "price": -1}]] }, "createProduct": { "201": [{"id": 2, "name": "Pen", "price": 1.5}] } } }).to_string())]),
        Ok(vec![Event::Delta(json!({ "examples": { "listProducts": { "200": [[{"id": 1, "name": "Mug", "price": 9.5}]] }, "createProduct": { "201": [{"id": 2, "name": "Pen", "price": 1.5}] } } }).to_string())]),
    ])));
    let (status, v) = post(
        &app,
        "/_rustybin/mocks/shop/ai/examples",
        json!({ "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["examples"]["listProducts"]["200"][0][0]["name"], "Mug");
    // The task itself: a missing operation is reported.
    let doc: Value = serde_json::from_str(SHOP).expect("json");
    let task = crate::assist::task::find("examples_for_operations").expect("task");
    let err = (task.post.expect("post"))(
        &json!({ "spec": doc }),
        json!({ "examples": { "listProducts": { "200": [[]] } } }),
    )
    .expect_err("missing");
    assert!(err.contains("createProduct"), "{err}");
    state.assist.set_script(None);
}

#[tokio::test]
async fn request_examples_task_generates_valid_requests() {
    let doc: Value = serde_json::from_str(SHOP).expect("json");
    let inputs = json!({ "spec": doc, "operation": "createProduct", "count": 3 });
    let v = tasks::request_examples::generate(&inputs).expect("generated");
    assert_eq!(v["requests"].as_array().map(Vec::len), Some(3));
    assert!(v["requests"][0]["body"]["price"].is_number());
    let task = crate::assist::task::find("request_examples").expect("task");
    assert!((task.post.expect("post"))(&inputs, v).is_ok());
    let bad = json!({ "requests": [{ "body": { "id": 1, "name": "x" } }] });
    let err = (task.post.expect("post"))(&inputs, bad).expect_err("invalid");
    assert!(err.contains("price"), "{err}");
    let inputs = json!({ "spec": doc, "operation": "getProduct", "count": 1 });
    let v = tasks::request_examples::generate(&inputs).expect("generated");
    assert!(v["requests"][0]["path"]["id"].is_number());
    let missing = json!({ "requests": [{ "query": {} }] });
    let err = (task.post.expect("post"))(&inputs, missing).expect_err("missing path param");
    assert!(err.contains("path.id"), "{err}");
    // Through the generic run route too.
    let state = test_state_with(mock_config());
    let app = test_app_with(state);
    let (status, v) = post(&app, "/_rustybin/assist/run", json!({ "task": "request_examples", "inputs": { "spec": doc, "operation": "getProduct" }, "stream": false })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v["json"]["requests"].is_array());
}

#[tokio::test]
async fn visitors_are_refused_without_the_switch() {
    // A self-service instance: the visitor creates a mock, then asks the AI.
    let mut c = mock_config();
    c.public_mode = true;
    c.admin_token = Some("admin-secret".into());
    c.self_service = crate::config::SelfServiceConfig {
        enabled: true,
        ..Default::default()
    };
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/mocks",
            &json!({ "name": "mine", "spec": SHOP }),
        ))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = body_json(resp).await;
    let token = created["manage_token"]
        .as_str()
        .expect("manage token")
        .to_string();
    let name = created["name"].as_str().expect("name").to_string();
    let with_token = |uri: String, body: Value| {
        let app = app.clone();
        let token = token.clone();
        async move {
            let req = Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-rustybin-mock-token", token)
                .body(Body::from(body.to_string()))
                .expect("req");
            let resp = app.oneshot(req).await.expect("r");
            let status = resp.status();
            (status, body_json(resp).await)
        }
    };
    let (status, v) = with_token(
        SPEC_PATH.to_string(),
        json!({ "description": "a notes api", "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    let (status, v) = with_token(
        format!("/_rustybin/mocks/{name}/ai/edit"),
        json!({ "instruction": "add 404", "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    // The generator needs no key: visitors may use it on their own mock.
    let (status, v) = with_token(
        format!("/_rustybin/mocks/{name}/ai/examples"),
        json!({ "source": "generated", "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    // With the switch the AI works for the visitor.
    let mut c = mock_config();
    c.public_mode = true;
    c.admin_token = Some("admin-secret".into());
    c.assist_visitors = true;
    c.self_service = crate::config::SelfServiceConfig {
        enabled: true,
        ..Default::default()
    };
    let app2 = test_app_with(test_state_with(c));
    let resp = app2
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/mocks",
            &json!({ "name": "mine", "spec": SHOP }),
        ))
        .await
        .expect("r");
    let created = body_json(resp).await;
    let token2 = created["manage_token"].as_str().expect("token").to_string();
    let req = Request::builder()
        .method("POST")
        .uri(SPEC_PATH)
        .header("content-type", "application/json")
        .header("x-rustybin-mock-token", token2)
        .body(Body::from(
            json!({ "description": "a notes api", "stream": false }).to_string(),
        ))
        .expect("req");
    let resp = app2.oneshot(req).await.expect("r");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn without_a_provider_the_ai_routes_say_so() {
    let app = test_app_with(test_state_with(Config::for_tests()));
    create(&app, "shop", SHOP).await;
    let (status, v) = post(
        &app,
        SPEC_PATH,
        json!({ "description": "a notes api", "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    let (status, _) = post(
        &app,
        "/_rustybin/mocks/shop/ai/edit",
        json!({ "instruction": "x", "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = post(
        &app,
        "/_rustybin/mocks/shop/ai/examples",
        json!({ "stream": false }),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = post(
        &app,
        "/_rustybin/mocks/nope/ai/examples",
        json!({ "source": "generated" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The spec task's mock answer and the diff helper stand on their own.
    assert_eq!(
        tasks::spec_from_description::resources(
            "An orders API: customers, products, orders with line items"
        ),
        vec!["orders", "customers", "products"]
    );
    let d = tasks::edit_spec::diff(
        &json!({"paths": {"/a": {"get": {"operationId": "a"}}}}),
        &json!({"paths": {"/a": {"get": {"operationId": "a", "summary": "x"}}, "/b": {"post": {"operationId": "b"}}}}),
    );
    assert_eq!(d.changed, vec!["a"]);
    assert_eq!(d.added, vec!["b"]);
    assert!(d.removed.is_empty());
    let _ = BTreeMap::<String, Value>::new();
}
