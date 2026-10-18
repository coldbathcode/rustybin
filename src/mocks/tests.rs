//! Mocks through the full application (middleware, control auth, routing).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::config::Config;
use crate::test_support::{
    body_string, get_request, json_request, test_app, test_app_with, test_state_with,
};

const ORDERS: &str = r##"{
  "openapi": "3.0.3",
  "info": {"title": "Orders API", "version": "2"},
  "paths": {
    "/orders": {
      "post": {
        "operationId": "createOrder",
        "security": [{"key": []}],
        "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/NewOrder"}}}},
        "responses": {"201": {"description": "created", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Order"}}}}}
      }
    },
    "/orders/{orderId}": {
      "get": {
        "operationId": "getOrder",
        "parameters": [{"name": "orderId", "in": "path", "required": true, "schema": {"type": "integer", "minimum": 1}}],
        "responses": {
          "200": {"description": "ok", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Order"}}}},
          "404": {"description": "missing"}
        }
      }
    }
  },
  "components": {
    "securitySchemes": {"key": {"type": "apiKey", "in": "header", "name": "X-Key"}},
    "schemas": {
      "NewOrder": {"type": "object", "required": ["sku", "quantity"], "properties": {
        "sku": {"type": "string"}, "quantity": {"type": "integer", "minimum": 1}}},
      "Order": {"allOf": [{"$ref": "#/components/schemas/NewOrder"}, {"type": "object", "required": ["id"], "properties": {"id": {"type": "integer", "readOnly": true}}}]}
    }
  }
}"##;

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let headers = resp.headers().clone();
    let text = body_string(resp).await;
    let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, headers, body)
}

fn req(method: &str, uri: &str, headers: &[(&str, &str)], body: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "mocks.test");
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(
        body.map(|s| Body::from(s.to_string()))
            .unwrap_or_else(Body::empty),
    )
    .expect("request")
}

async fn create(app: &Router, body: Value) -> Value {
    let (status, _, out) = send(app, json_request("POST", "/_rustybin/mocks", &body)).await;
    assert_eq!(status, StatusCode::CREATED, "{out}");
    out
}

#[tokio::test]
async fn sample_mock_answers_from_examples_rules_and_prefer() {
    let app = test_app();
    let (status, headers, body) = send(&app, get_request("/mock/petstore/v1/pets")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["name"], "Rex");
    assert_eq!(headers["x-rustybin-mock"], "petstore");
    assert_eq!(headers["x-rustybin-mock-operation"], "listPets");
    assert_eq!(headers["x-total-count"], "3");

    let (status, _, body) = send(&app, get_request("/mock/petstore/v1/pets/42")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], 42, "{body}");
    assert_eq!(body["name"], "Rex");

    let (status, headers, body) = send(&app, get_request("/mock/petstore/v1/pets/0")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["message"], "pet not found");
    assert_eq!(
        headers["x-rustybin-mock-source"],
        "rule:unknown pet example:notFound"
    );

    let (status, _, _) = send(
        &app,
        req(
            "GET",
            "/mock/petstore/v1/pets/1",
            &[("prefer", "code=404")],
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _, body) = send(
        &app,
        req(
            "POST",
            "/mock/petstore/v1/pets",
            &[("content-type", "application/json")],
            Some(r#"{"name":"Rex"}"#),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["id"], 4);

    let (status, _, _) = send(&app, req("DELETE", "/mock/petstore/v1/pets/1", &[], None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, headers, _) = send(&app, req("PATCH", "/mock/petstore/v1/pets", &[], None)).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(headers["allow"], "GET, POST");

    let (status, _, body) = send(&app, get_request("/mock/petstore/v1/nothing")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["base_path"], "/mock/petstore/v1");
    let (status, _, body) = send(&app, get_request("/mock/nope/x")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "mock not found");

    // Every call is captured with the decision.
    let (_, _, list) = send(
        &app,
        get_request("/_rustybin/mocks/petstore/requests?limit=50"),
    )
    .await;
    assert!(list["total"].as_u64().expect("total") >= 8, "{list}");
    let newest = &list["requests"][0];
    assert_eq!(newest["outcome"], "not_found");
    let rule_hit = list["requests"]
        .as_array()
        .expect("requests")
        .iter()
        .find(|r| r["path"] == "/v1/pets/0")
        .expect("rule hit");
    assert_eq!(rule_hit["operation"], "getPetById");
    assert_eq!(rule_hit["status"], 404);
    assert!(rule_hit["response_body"]
        .as_str()
        .expect("body")
        .contains("pet not found"));
}

#[tokio::test]
async fn validation_modes() {
    let app = test_app();
    // Warn (default): answered, flagged, captured.
    let (status, headers, _) = send(
        &app,
        req(
            "POST",
            "/mock/petstore/v1/pets",
            &[("content-type", "application/json")],
            Some(r#"{"tag":1}"#),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(headers["x-rustybin-mock-validation"], "failed");
    let (_, _, list) = send(&app, get_request("/_rustybin/mocks/petstore/requests")).await;
    let issues = &list["requests"][0]["validation"];
    let pointers: Vec<&str> = issues
        .as_array()
        .expect("issues")
        .iter()
        .filter_map(|i| i["pointer"].as_str())
        .collect();
    assert_eq!(pointers, vec!["/body", "/body/tag"], "{issues}");

    // Enforce: 400 problem+json.
    let (status, _, _) = send(
        &app,
        json_request(
            "PATCH",
            "/_rustybin/mocks/petstore",
            &json!({
                "responses": { "validate_requests": "enforce" }, "deploy": true
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, headers, body) = send(
        &app,
        req("GET", "/mock/petstore/v1/pets?limit=500", &[], None),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(headers["content-type"], "application/problem+json");
    assert_eq!(body["errors"][0]["pointer"], "/query/limit");
}

#[tokio::test]
async fn create_secure_disable_and_delete() {
    let app = test_app();
    let created = create(&app, json!({
        "spec": ORDERS,
        "security": { "enabled": true, "api_key": { "keys": ["k-123"] } },
        "operations": { "getOrder": { "rules": [ { "when": { "path.orderId": { "equals": 404 } }, "respond": { "status": 404 } } ] } }
    }))
    .await;
    assert_eq!(created["name"], "orders-api");
    assert_eq!(created["state"], "live");
    assert!(created["url"]
        .as_str()
        .expect("url")
        .ends_with("/mock/orders-api"));
    assert_eq!(created["operations"][0]["secured"], true);
    assert_eq!(created["operations"][1]["secured"], false);

    let order = r#"{"sku":"A-1","quantity":2}"#;
    let (status, headers, body) = send(
        &app,
        req(
            "POST",
            "/mock/orders-api/orders",
            &[("content-type", "application/json")],
            Some(order),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(body["detail"]
        .as_str()
        .expect("detail")
        .contains("missing API key"));
    assert!(headers.get("www-authenticate").is_none());
    let (status, _, body) = send(
        &app,
        req(
            "POST",
            "/mock/orders-api/orders",
            &[("content-type", "application/json"), ("x-key", "k-123")],
            Some(order),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["id"].is_i64());

    let (status, _, body) = send(&app, get_request("/mock/orders-api/orders/7")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], 7);
    let (status, _, _) = send(&app, get_request("/mock/orders-api/orders/404")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Name taken: a second create without a name gets a suffix.
    let again = create(&app, json!({ "spec": ORDERS })).await;
    assert_eq!(again["name"], "orders-api-2");
    let (status, _, _) = send(
        &app,
        json_request(
            "POST",
            "/_rustybin/mocks",
            &json!({ "spec": ORDERS, "name": "orders-api" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _, _) = send(
        &app,
        json_request(
            "POST",
            "/_rustybin/mocks",
            &json!({ "spec": ORDERS, "name": "export" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, body) = send(
        &app,
        json_request("POST", "/_rustybin/mocks", &json!({ "spec": "nope: [" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Off: 503, kept; on again: 200.
    let (status, _, body) = send(
        &app,
        req("POST", "/_rustybin/mocks/orders-api/disable", &[], None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "off");
    let (status, _, _) = send(&app, get_request("/mock/orders-api/orders/7")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    send(
        &app,
        req("POST", "/_rustybin/mocks/orders-api/enable", &[], None),
    )
    .await;
    let (status, _, _) = send(&app, get_request("/mock/orders-api/orders/7")).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = send(
        &app,
        req("DELETE", "/_rustybin/mocks/orders-api", &[], None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(&app, get_request("/mock/orders-api/orders/7")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn drafts_checks_deploy_and_rollback() {
    let app = test_app();
    create(&app, json!({ "spec": ORDERS, "name": "o" })).await;
    // An invalid custom body is a check error: deploy refuses, force works.
    let (status, _, op) = send(
        &app,
        json_request(
            "PUT",
            "/_rustybin/mocks/o/operations/getOrder",
            &json!({
                "respond": { "body": { "id": "not a number", "sku": "x", "quantity": 1 } }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(op["report"]["status"], "error");
    let (_, _, detail) = send(&app, get_request("/_rustybin/mocks/o")).await;
    assert_eq!(detail["has_draft_changes"], true);
    assert_eq!(detail["draft_changes"], json!(["operation getOrder"]));
    assert_eq!(detail["report"]["ok"], false);
    let (status, _, body) = send(&app, req("POST", "/_rustybin/mocks/o/deploy", &[], None)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["report"]["errors"], 1);
    // Live still serves the previous revision.
    let (_, _, body) = send(&app, get_request("/mock/o/orders/3")).await;
    assert_eq!(body["id"], 3);
    let (status, _, _) = send(
        &app,
        req("POST", "/_rustybin/mocks/o/deploy?force=true", &[], None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, body) = send(&app, get_request("/mock/o/orders/3")).await;
    assert_eq!(body["id"], "not a number");
    let (status, _, _) = send(&app, req("POST", "/_rustybin/mocks/o/rollback", &[], None)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, body) = send(&app, get_request("/mock/o/orders/3")).await;
    assert_eq!(body["id"], 3);

    // Sample and validate helpers.
    let (_, _, sample) = send(
        &app,
        get_request("/_rustybin/mocks/o/operations/createOrder/sample?mode=generated"),
    )
    .await;
    assert_eq!(sample["request"]["method"], "POST");
    assert!(sample["request"]["body"]["sku"].is_string());
    assert_eq!(sample["response"]["status"], 201);
    assert_eq!(sample["response"]["valid"], true);
    let (_, _, v) = send(
        &app,
        json_request(
            "POST",
            "/_rustybin/mocks/o/operations/createOrder/validate",
            &json!({
                "kind": "request", "body": { "sku": "x", "quantity": 0 }
            }),
        ),
    )
    .await;
    assert_eq!(v["valid"], false);
    assert_eq!(v["issues"][0]["pointer"], "/quantity");

    // Operation detail has inlined schemas for form building.
    let (_, _, op) = send(&app, get_request("/_rustybin/mocks/o/operations/getOrder")).await;
    let schema = &op["operation"]["responses"][0]["content"][0]["schema"];
    assert!(schema.get("$ref").is_none(), "{schema}");
    assert_eq!(schema["title"], "Order");
}

#[tokio::test]
async fn served_spec_export_and_apply() {
    let app = test_app();
    let (status, _, spec) = send(
        &app,
        req("GET", "/mock/petstore/_rustybin/openapi.json", &[], None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        spec["servers"][0]["url"],
        "http://mocks.test/mock/petstore/v1"
    );
    let (status, headers, yaml) =
        send(&app, get_request("/mock/petstore/_rustybin/openapi.yaml")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/yaml");
    assert!(yaml.as_str().expect("yaml").contains("openapi: 3.0.3"));

    create(
        &app,
        json!({ "spec": ORDERS, "name": "o", "responses": { "mode": "generated" } }),
    )
    .await;
    let (status, _, exported) = send(&app, get_request("/_rustybin/mocks/export")).await;
    assert_eq!(status, StatusCode::OK);
    let text = exported.as_str().expect("yaml").to_string();
    assert!(text.contains("name: o"), "{text}");
    assert!(!text.contains("name: petstore"));

    // The export applied onto the mock it came from changes nothing (same
    // spec, other formatting).
    let (_, _, report) = send(
        &app,
        req("POST", "/_rustybin/mocks/apply", &[], Some(&text)),
    )
    .await;
    assert_eq!(report["unchanged"], json!(["o"]), "{report}");

    // Round trip: delete, apply the export, same mock back.
    send(&app, req("DELETE", "/_rustybin/mocks/o", &[], None)).await;
    let (status, _, report) = send(
        &app,
        req(
            "POST",
            "/_rustybin/mocks/apply",
            &[("content-type", "application/yaml")],
            Some(&text),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["created"], json!(["o"]));
    let (_, _, detail) = send(&app, get_request("/_rustybin/mocks/o")).await;
    assert_eq!(detail["settings"]["responses"]["mode"], "generated");
    let (_, _, report) = send(
        &app,
        req("POST", "/_rustybin/mocks/apply", &[], Some(&text)),
    )
    .await;
    assert_eq!(report["unchanged"], json!(["o"]));
    let (_, _, report) = send(
        &app,
        req(
            "POST",
            "/_rustybin/mocks/apply?prune=true",
            &[],
            Some(r#"{"mocks": []}"#),
        ),
    )
    .await;
    assert_eq!(report["deleted"], json!(["o"]));

    // Spec paths are refused through the API.
    let (_, _, report) = send(
        &app,
        req(
            "POST",
            "/_rustybin/mocks/apply",
            &[],
            Some(r#"{"mocks": [{"name": "x", "spec": "/etc/passwd"}]}"#),
        ),
    )
    .await;
    assert!(report["errors"][0]["error"]
        .as_str()
        .expect("error")
        .contains("only read from local files"));

    let (status, _, preview) = send(
        &app,
        json_request(
            "POST",
            "/_rustybin/mocks/import",
            &json!({ "spec": ORDERS }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview["suggested_name"], "orders-api");
    assert_eq!(preview["declares_security"], true);
    assert_eq!(preview["operations"].as_array().map(Vec::len), Some(2));
    let (status, _, _) = send(
        &app,
        req(
            "POST",
            "/_rustybin/mocks/import",
            &[("content-type", "application/yaml")],
            Some("openapi: 3.1.0\ninfo: {title: y}\npaths: {}\n"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn public_mode_and_control_auth() {
    // Public mode without an admin token: mocks are read-only.
    let mut config = Config::for_tests();
    config.public_mode = true;
    config.mocks_sample = false;
    let app = test_app_with(test_state_with(config));
    let (status, _, _) = send(
        &app,
        json_request("POST", "/_rustybin/mocks", &json!({ "spec": ORDERS })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, _, list) = send(&app, get_request("/_rustybin/mocks")).await;
    assert_eq!(list["count"], 0);
    assert_eq!(list["limits"]["max_mocks"], 50);

    // jwt control auth: console scope reads, admin scope writes; the mock
    // itself (data plane) is never behind control auth.
    let state = test_state_with(crate::test_support::jwt_config());
    let app = test_app_with(state);
    let console = crate::test_support::control_jwt(Some("console"));
    let admin = crate::test_support::control_jwt(Some("admin"));
    let bearer = |t: &str| format!("Bearer {t}");
    let (status, _, _) = send(&app, get_request("/_rustybin/mocks")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send(
        &app,
        req(
            "GET",
            "/_rustybin/mocks",
            &[("authorization", &bearer(&console))],
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &app,
        req(
            "POST",
            "/_rustybin/mocks/petstore/disable",
            &[("authorization", &bearer(&console))],
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(
        &app,
        req(
            "POST",
            "/_rustybin/mocks/petstore/disable",
            &[("authorization", &bearer(&admin))],
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(&app, get_request("/mock/petstore/v1/pets")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn secrets_are_hidden_from_callers_who_cannot_change_mocks() {
    let mut config = crate::test_support::jwt_config();
    config.mocks_sample = false;
    let app = test_app_with(test_state_with(config));
    let console = format!(
        "Bearer {}",
        crate::test_support::control_jwt(Some("console"))
    );
    let admin = format!("Bearer {}", crate::test_support::control_jwt(Some("admin")));
    let secrets = ["sekret-key", "sekret-token", "hunter2", "YW5uOmh1bnRlcjI"];
    let security = json!({
        "enabled": true,
        "api_key": { "keys": ["sekret-key"] },
        "bearer": { "tokens": ["sekret-token"] },
        "basic": { "users": { "ann": "hunter2" } },
    });
    let mut create_req = json_request(
        "POST",
        "/_rustybin/mocks",
        &json!({ "spec": ORDERS, "security": security }),
    );
    create_req
        .headers_mut()
        .insert("authorization", admin.parse().expect("header"));
    let (status, _, created) = send(&app, create_req).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["secrets_hidden"], false);
    assert_eq!(
        created["settings"]["security"]["basic"]["users"]["ann"],
        "hunter2"
    );

    let reads = [
        "/_rustybin/mocks/orders-api",
        "/_rustybin/mocks/orders-api/report",
        "/_rustybin/mocks/orders-api/operations/createOrder",
        "/_rustybin/mocks/orders-api/operations/createOrder/sample",
        "/_rustybin/mocks/orders-api/export",
        "/_rustybin/mocks/export",
        "/_rustybin/mocks/orders-api/collections/curl",
        "/_rustybin/mocks/orders-api/collections/postman",
    ];
    for uri in reads {
        let (status, _, body) =
            send(&app, req("GET", uri, &[("authorization", &console)], None)).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        let text = body.to_string();
        for secret in secrets {
            assert!(
                !text.contains(secret),
                "{uri} shows {secret} to a console token: {text}"
            );
        }
        // The admin token sees the real API key (the spec's only scheme).
        let (status, _, body) =
            send(&app, req("GET", uri, &[("authorization", &admin)], None)).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        assert!(body.to_string().contains("sekret-key"), "{uri}: {body}");
    }

    let (_, _, d) = send(
        &app,
        req(
            "GET",
            "/_rustybin/mocks/orders-api",
            &[("authorization", &console)],
            None,
        ),
    )
    .await;
    assert_eq!(d["secrets_hidden"], true);
    assert_eq!(
        d["settings"]["security"]["api_key"]["keys"],
        json!(["<redacted>"])
    );
    assert_eq!(
        d["settings"]["security"]["basic"]["users"],
        json!({ "ann": "<redacted>" })
    );
    assert_eq!(d["credentials"]["bearer"]["tokens"], json!(["<redacted>"]));
    assert_eq!(d["credentials"]["hidden"], true);
    let (_, _, op) = send(
        &app,
        req(
            "GET",
            "/_rustybin/mocks/orders-api/operations/createOrder",
            &[("authorization", &console)],
            None,
        ),
    )
    .await;
    assert!(
        op["sample_request"]["headers"]
            .as_array()
            .expect("headers")
            .contains(&json!(["X-Key", "<api-key>"])),
        "{op}"
    );

    // Redacted settings cannot be saved back.
    let mut patch = json_request(
        "PATCH",
        "/_rustybin/mocks/orders-api",
        &json!({ "security": d["settings"]["security"] }),
    );
    patch
        .headers_mut()
        .insert("authorization", admin.parse().expect("header"));
    let (status, _, body) = send(&app, patch).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("hidden secret"), "{body}");
}

#[tokio::test]
async fn sample_security_with_idp_tokens() {
    let app = test_app();
    let (status, _, _) = send(
        &app,
        json_request(
            "PATCH",
            "/_rustybin/mocks/petstore",
            &json!({
                "security": { "enabled": true }, "deploy": true
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = r#"{"name":"Rex"}"#;
    let ct = ("content-type", "application/json");
    let (status, headers, _) = send(
        &app,
        req("POST", "/mock/petstore/v1/pets", &[ct], Some(body)),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers["www-authenticate"], "Bearer realm=\"mock\"");
    let (status, _, _) = send(
        &app,
        req(
            "POST",
            "/mock/petstore/v1/pets",
            &[ct, ("x-api-key", "demo-key")],
            Some(body),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    // A client-credentials token from the built-in IdP.
    let (status, _, token) = send(
        &app,
        req(
            "POST",
            "/oauth/token",
            &[("content-type", "application/x-www-form-urlencoded")],
            Some("grant_type=client_credentials&client_id=rustybin&client_secret=secret"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{token}");
    let auth = format!("Bearer {}", token["access_token"].as_str().expect("token"));
    let (status, _, _) = send(
        &app,
        req(
            "POST",
            "/mock/petstore/v1/pets",
            &[ct, ("authorization", &auth)],
            Some(body),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    // Reads stay public (the spec declares no security on them).
    let (status, _, _) = send(&app, get_request("/mock/petstore/v1/pets")).await;
    assert_eq!(status, StatusCode::OK);
    // The detail shows what a client needs.
    let (_, _, detail) = send(&app, get_request("/_rustybin/mocks/petstore")).await;
    assert_eq!(
        detail["credentials"]["api_key"]["keys"],
        json!(["demo-key"])
    );
    assert_eq!(
        detail["credentials"]["schemes"].as_array().map(Vec::len),
        Some(2)
    );
}

#[tokio::test]
async fn duplicate_stateful_crud_and_hit_counts() {
    let app = test_app();
    let (status, _, copy) = send(
        &app,
        json_request(
            "POST",
            "/_rustybin/mocks/petstore/duplicate",
            &json!({ "name": "pets" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{copy}");
    assert_eq!(copy["state"], "live");
    assert_eq!(copy["origin"], "api");
    let (status, _, out) = send(
        &app,
        json_request(
            "PATCH",
            "/_rustybin/mocks/pets",
            &json!({ "responses": { "mode": "stateful" }, "deploy": true }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");

    let (_, _, list) = send(&app, get_request("/mock/pets/v1/pets")).await;
    assert_eq!(list.as_array().map(Vec::len), Some(3), "{list}");
    let (status, headers, created) = send(
        &app,
        json_request(
            "POST",
            "/mock/pets/v1/pets",
            &json!({ "name": "Bo", "tag": "dog" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["id"], 4);
    assert_eq!(created["name"], "Bo");
    assert_eq!(headers["x-rustybin-mock-source"], "state create");
    let (_, _, read) = send(&app, get_request("/mock/pets/v1/pets/4")).await;
    assert_eq!(read["name"], "Bo");
    let (status, _, _) = send(&app, req("DELETE", "/mock/pets/v1/pets/1", &[], None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, missing) = send(&app, get_request("/mock/pets/v1/pets/1")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(missing["message"], "pet not found");
    // Rules still come first: petId 0 is the sample's 404 rule.
    let (status, headers, _) = send(&app, get_request("/mock/pets/v1/pets/0")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(headers["x-rustybin-mock-source"]
        .to_str()
        .is_ok_and(|s| s.starts_with("rule:")));

    let (_, _, state) = send(&app, get_request("/_rustybin/mocks/pets/state")).await;
    assert_eq!(state["items"], 3, "{state}");
    let (status, _, _) = send(
        &app,
        req("DELETE", "/_rustybin/mocks/pets/state", &[], None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, list) = send(&app, get_request("/mock/pets/v1/pets")).await;
    assert_eq!(
        list.as_array().map(Vec::len),
        Some(3),
        "reset reseeds: {list}"
    );

    let (_, _, detail) = send(&app, get_request("/_rustybin/mocks/pets")).await;
    assert_eq!(detail["requests_last_hour"], 7, "{detail}");
    let get_by_id = detail["operations"]
        .as_array()
        .and_then(|ops| ops.iter().find(|o| o["id"] == "getPetById"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(get_by_id["hits"], 3, "{get_by_id}");
    assert!(get_by_id["last_hit"].is_string());
}

#[tokio::test]
async fn rule_test_spec_source_and_collections() {
    let app = test_app();
    let (_, _, none) = send(
        &app,
        req(
            "POST",
            "/_rustybin/mocks/petstore/operations/getPetById/rules/test",
            &[],
            None,
        ),
    )
    .await;
    assert!(none["request"].is_null(), "{none}");
    send(&app, get_request("/mock/petstore/v1/pets/0")).await;
    let (status, _, tested) = send(
        &app,
        req(
            "POST",
            "/_rustybin/mocks/petstore/operations/getPetById/rules/test",
            &[],
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tested}");
    assert_eq!(tested["winner"], 0, "{tested}");
    assert_eq!(tested["rules"][0]["conditions"][0]["value"], "0");
    assert_eq!(tested["respond"]["status"], 404);
    let rules = json!({ "rules": [{ "when": { "path.petId": { "equals": "7" } }, "respond": { "status": 500 } }] });
    let (_, _, other) = send(
        &app,
        json_request(
            "POST",
            "/_rustybin/mocks/petstore/operations/getPetById/rules/test",
            &rules,
        ),
    )
    .await;
    assert!(other["winner"].is_null(), "{other}");
    assert_eq!(other["rules"][0]["matched"], false);

    let resp = app
        .clone()
        .oneshot(get_request("/_rustybin/mocks/petstore/spec"))
        .await
        .expect("spec");
    assert_eq!(resp.headers()["content-type"], "application/yaml");
    assert!(body_string(resp).await.starts_with("openapi: 3.0.3"));

    let resp = app
        .clone()
        .oneshot(get_request(
            "/_rustybin/mocks/petstore/collections/postman?base_url=https://gw.example.com/pets",
        ))
        .await
        .expect("postman");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()["content-disposition"]
        .to_str()
        .is_ok_and(|d| d.contains("petstore.postman.json")));
    let doc: Value = serde_json::from_str(&body_string(resp).await).expect("json");
    assert_eq!(doc["variable"][0]["value"], "https://gw.example.com/pets");
    let (status, _, _) = send(
        &app,
        get_request("/_rustybin/mocks/petstore/collections/word"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn accept_reserved_paths_params_and_metrics() {
    let state = test_state_with(Config::for_tests());
    let metrics = state.metrics.clone();
    let app = test_app_with(state);

    // Warn mode answers JSON whatever Accept says; enforce answers 406.
    let xml = [("accept", "application/xml")];
    let (status, _, _) = send(&app, req("GET", "/mock/petstore/v1/pets/7", &xml, None)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &app,
        json_request(
            "PATCH",
            "/_rustybin/mocks/petstore",
            &json!({ "responses": { "validate_requests": "enforce" }, "deploy": true }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, headers, body) =
        send(&app, req("GET", "/mock/petstore/v1/pets/7", &xml, None)).await;
    assert_eq!(status, StatusCode::NOT_ACCEPTABLE, "{body}");
    assert_eq!(headers["content-type"], "application/problem+json");
    assert_eq!(body["available"], json!(["application/json"]));
    for accept in [
        "application/json",
        "application/*",
        "*/*",
        "text/html, */*;q=0.1",
    ] {
        let (status, _, _) = send(
            &app,
            req(
                "GET",
                "/mock/petstore/v1/pets/7",
                &[("accept", accept)],
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{accept}");
    }

    // `/_rustybin` below a mock is never an operation and never captured.
    let (_, _, before) = send(&app, get_request("/_rustybin/mocks/petstore/requests")).await;
    let (status, _, _) = send(&app, get_request("/mock/petstore/_rustybin/other")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = send(&app, get_request("/mock/petstore/_rustybin/openapi.json")).await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, after) = send(&app, get_request("/_rustybin/mocks/petstore/requests")).await;
    assert!(before["total"].is_u64(), "{before}");
    assert_eq!(before["total"], after["total"], "{after}");

    // Captures carry the matched path parameters.
    let latest = &after["requests"][0];
    assert_eq!(
        latest["params"],
        json!([{ "name": "petId", "value": "7" }]),
        "{latest}"
    );

    // Metrics count mock requests by mock and outcome.
    let text = metrics.render();
    assert!(
        text.contains("rustybin_mock_requests_total{mock=\"petstore\",outcome=\"mocked\"}"),
        "{text}"
    );
    assert!(text.contains("outcome=\"not_acceptable\"} 1"), "{text}");
}
