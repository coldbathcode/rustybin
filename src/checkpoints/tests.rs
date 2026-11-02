use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::{Config, SelfServiceConfig};
use crate::test_support::{test_app_with, test_state_with};

const ITEMS: &str = r#"{"openapi":"3.0.3","info":{"title":"Items","version":"1"},"paths":{"/items":{"get":{"operationId":"listItems","responses":{"200":{"description":"ok","content":{"application/json":{"example":[]}}}}},"post":{"operationId":"addItem","requestBody":{"content":{"application/json":{"schema":{"type":"object"}}}},"responses":{"201":{"description":"ok","content":{"application/json":{"schema":{"type":"object"}}}}}}},"/items/{id}":{"get":{"operationId":"getItem","parameters":[{"name":"id","in":"path","required":true,"schema":{"type":"integer"}}],"responses":{"200":{"description":"ok","content":{"application/json":{"schema":{"type":"object"}}}}}}}}}"#;

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer admin-secret");
    if body.is_some() {
        b = b.header("content-type", "application/json");
    }
    let req = b
        .body(
            body.map(|v| Body::from(v.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .expect("req");
    let resp = app.clone().oneshot(req).await.expect("resp");
    let status = resp.status();
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn config() -> Config {
    let mut c = Config::for_tests();
    c.admin_token = Some("admin-secret".into());
    c
}

/// What the instance's API mocks are: the export's mocks, each deployed
/// revision's source, on or off, and the stateful data.
fn picture(state: &AppState) -> Value {
    let doc = crate::configdoc::export(state, crate::configdoc::SecretsMode::Refs, false).file;
    let live: Vec<Value> = state
        .mocks
        .list()
        .iter()
        .filter(|s| covered(s))
        .map(|s| {
            json!({
                "name": s.name,
                "enabled": s.enabled,
                "live": s.live.as_ref().map(|c| c.source.to_string()),
                "state": serde_json::to_value(s.state.copy()).unwrap_or(Value::Null),
            })
        })
        .collect();
    json!({ "mocks": serde_json::to_value(&doc.mocks).unwrap_or(Value::Null), "live": live, "healthy": state.health.is_healthy() })
}

async fn setup() -> (AppState, Router) {
    let state = test_state_with(config());
    let app = test_app_with(state.clone());
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/mocks",
        Some(json!({ "name": "items", "spec": ITEMS, "responses": { "mode": "stateful" } })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, v) = call(
        &app,
        "POST",
        "/mock/items/items",
        Some(json!({ "name": "first" })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/mocks",
        Some(json!({ "name": "orders", "spec": ITEMS })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    // A draft on top of the deployed revision.
    let (s, v) = call(
        &app,
        "PUT",
        "/_rustybin/mocks/orders/operations/listItems",
        Some(json!({ "respond": { "status": 200, "body": ["draft"] } })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    (state, app)
}

#[tokio::test]
async fn a_restore_brings_back_the_mocks_their_data_and_health() {
    let (state, app) = setup().await;
    let before = picture(&state);
    let (s, v) = call(
        &app,
        "POST",
        LIST_PATH,
        Some(json!({ "name": "demo-start" })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["mocks"], json!(["items", "orders"]));

    // Everything changes: data, a deploy, a new mock, a deleted mock, health,
    // flaky counters, and a setting.
    call(
        &app,
        "POST",
        "/mock/items/items",
        Some(json!({ "name": "second" })),
    )
    .await;
    let (s, _) = call(
        &app,
        "POST",
        "/_rustybin/mocks/orders/deploy",
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    call(
        &app,
        "POST",
        "/_rustybin/mocks",
        Some(json!({ "name": "extra", "spec": ITEMS })),
    )
    .await;
    call(&app, "DELETE", "/_rustybin/mocks/items", None).await;
    call(
        &app,
        "POST",
        "/_rustybin/mocks",
        Some(json!({ "name": "items", "spec": ITEMS })),
    )
    .await;
    state.health.set(false);
    state.flaky.next("someone", "after:3");
    state
        .settings
        .update(
            [("console.title".to_string(), json!("Changed"))]
                .into_iter()
                .collect(),
            Default::default(),
            "test",
        )
        .map_err(|e| format!("{e:?}"))
        .expect("setting");
    assert_ne!(picture(&state), before);

    // A dry run says what a restore would do and changes nothing.
    let mid = picture(&state);
    let (s, d) = call(
        &app,
        "POST",
        "/_rustybin/checkpoints/demo-start/restore?dry_run=true",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["mocks"]["deleted"], json!(["extra"]), "{d:#}");
    assert_eq!(d["mocks"]["replaced"], json!(["items", "orders"]), "{d:#}");
    assert_eq!(picture(&state), mid);

    let (s, r) = call(
        &app,
        "POST",
        "/_rustybin/checkpoints/demo-start/restore",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["changed"], true);
    assert_eq!(r["mocks"]["deleted"], json!(["extra"]));
    assert_eq!(r["health"]["after"], "healthy");
    assert_eq!(r["flaky_counters_reset"], 1);
    assert_eq!(picture(&state), before, "{r:#}");
    // The data answers as saved: one item.
    let (_, list) = call(&app, "GET", "/mock/items/items", None).await;
    assert_eq!(list.as_array().map(Vec::len), Some(1), "{list}");
    // Settings are not part of it.
    assert_eq!(
        state.settings.string("console.title").as_deref(),
        Some("Changed")
    );

    // Again: nothing to change, the same instance.
    let (_, again) = call(
        &app,
        "POST",
        "/_rustybin/checkpoints/demo-start/restore",
        None,
    )
    .await;
    assert_eq!(again["changed"], false, "{again:#}");
    assert_eq!(again["mocks"]["unchanged"], json!(["items", "orders"]));
    assert_eq!(picture(&state), before);

    // Only the data changed: only the data comes back.
    call(
        &app,
        "POST",
        "/mock/items/items",
        Some(json!({ "name": "third" })),
    )
    .await;
    let (_, d) = call(
        &app,
        "POST",
        "/_rustybin/checkpoints/demo-start/restore",
        None,
    )
    .await;
    assert_eq!(d["mocks"]["data_restored"], json!(["items"]), "{d:#}");
    assert_eq!(picture(&state), before);
}

#[tokio::test]
async fn a_visitors_mock_is_never_touched() {
    let mut c = config();
    c.public_mode = true;
    c.mocks_sample = false;
    c.self_service = SelfServiceConfig {
        enabled: true,
        home_url: Some("https://home.example/".into()),
        leads_salt: Some("test-salt".into()),
        ..SelfServiceConfig::default()
    };
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    let (s, _) = call(&app, "POST", LIST_PATH, Some(json!({ "name": "empty" }))).await;
    assert_eq!(s, StatusCode::CREATED);
    // A visitor creates a mock (no admin token).
    let req = Request::post("/_rustybin/mocks")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "name": "shop", "spec": ITEMS }).to_string(),
        ))
        .expect("req");
    let resp = app.clone().oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let visitor: Vec<String> = state
        .mocks
        .list()
        .into_iter()
        .filter(|s| s.owner.is_visitor())
        .map(|s| s.name)
        .collect();
    assert_eq!(visitor.len(), 1);
    let (_, r) = call(&app, "POST", "/_rustybin/checkpoints/empty/restore", None).await;
    assert_eq!(r["mocks"]["deleted"], json!([]), "{r:#}");
    assert!(state.mocks.get(&visitor[0]).is_some());
    // A checkpoint never holds it either.
    let (_, v) = call(
        &app,
        "POST",
        LIST_PATH,
        Some(json!({ "name": "with-visitor" })),
    )
    .await;
    assert_eq!(v["mocks"], json!([]));
}

#[tokio::test]
async fn checkpoints_are_admin_only_bounded_and_survive_a_restart() {
    let dir = tempfile::tempdir().expect("dir");
    let mut c = config();
    c.state_dir = Some(dir.path().to_path_buf());
    let state = test_state_with(c.clone());
    let app = test_app_with(state.clone());
    // Admin only.
    let req = Request::post(LIST_PATH)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"x"}"#))
        .expect("req");
    assert_eq!(
        app.clone().oneshot(req).await.expect("r").status(),
        StatusCode::UNAUTHORIZED
    );
    // Names and the cap.
    let (s, _) = call(&app, "POST", LIST_PATH, Some(json!({ "name": "a b" }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    for i in 0..MAX_CHECKPOINTS {
        let (s, v) = call(
            &app,
            "POST",
            LIST_PATH,
            Some(json!({ "name": format!("c{i}") })),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }
    let (s, _) = call(&app, "POST", LIST_PATH, Some(json!({ "name": "one-more" }))).await;
    assert_eq!(s, StatusCode::INSUFFICIENT_STORAGE);
    let (s, _) = call(&app, "POST", LIST_PATH, Some(json!({ "name": "c1" }))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &app,
        "POST",
        LIST_PATH,
        Some(json!({ "name": "c1", "replace": true })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = call(&app, "DELETE", "/_rustybin/checkpoints/c2", None).await;
    assert_eq!(s, StatusCode::OK);
    // A restart reads them back.
    let again = Checkpoints::new(Some(dir.path()));
    assert_eq!(again.len(), MAX_CHECKPOINTS - 1);
    assert!(again.get("c2").is_none() && again.get("c1").is_some());
    let (s, _) = call(&app, "POST", "/_rustybin/checkpoints/nope/restore", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
