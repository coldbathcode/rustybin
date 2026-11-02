//! Conflict protection for edits (reliability step 08): `If-Match` or
//! `expected_revision` on PATCH and PUT operation, 409 with what changed,
//! `mocks.require_revision`, and the ETag.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::config::Config;
use crate::test_support::{test_app_with, test_state_with};

const SPEC: &str = r#"{"openapi":"3.0.3","info":{"title":"Orders","version":"1"},"paths":{"/orders":{"get":{"operationId":"listOrders","responses":{"200":{"description":"ok","content":{"application/json":{"example":[]}}}}}}}}"#;

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    if_match: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Option<String>, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(m) = if_match {
        b = b.header("if-match", m);
    }
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
    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        etag,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn app() -> (crate::state::AppState, Router, u64) {
    let state = test_state_with(Config::for_tests());
    let app = test_app_with(state.clone());
    let (s, _, v) = call(
        &app,
        "POST",
        "/_rustybin/mocks",
        None,
        Some(json!({ "name": "orders", "spec": SPEC })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (_, etag, v) = call(&app, "GET", "/_rustybin/mocks/orders", None, None).await;
    let rev = v["draft_revision"].as_u64().expect("draft_revision");
    assert_eq!(etag, Some(format!("\"{rev}\"")));
    (state, app, rev)
}

#[tokio::test]
async fn a_second_edit_from_the_same_revision_is_refused_with_what_changed() {
    let (_, app, rev) = app().await;
    let tag = format!("\"{rev}\"");
    // Alice edits from the revision she read: it passes, with a new ETag.
    let (s, etag, v) = call(
        &app,
        "PATCH",
        "/_rustybin/mocks/orders",
        Some(&tag),
        Some(json!({ "responses": { "mode": "generated" }, "deploy": false })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let new = v["draft_revision"].as_u64().expect("rev");
    assert!(new > rev);
    assert_eq!(etag, Some(format!("\"{new}\"")));
    // Bob edits from the same old revision: 409 naming the current one and
    // what changed, and nothing is saved.
    let (s, _, v) = call(
        &app,
        "PUT",
        "/_rustybin/mocks/orders/operations/listOrders",
        None,
        Some(json!({ "delay_ms": 50, "expected_revision": rev })),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"], "rustybin_stale_revision");
    assert_eq!(v["current_revision"], new);
    assert_eq!(v["expected_revision"], rev);
    assert_eq!(v["changed"], json!(["responses"]), "{v}");
    let (_, _, op) = call(
        &app,
        "GET",
        "/_rustybin/mocks/orders/operations/listOrders",
        None,
        None,
    )
    .await;
    assert!(op["settings"].get("delay_ms").is_none(), "{op}");
    // The same through PATCH and If-Match.
    let (s, _, _) = call(
        &app,
        "PATCH",
        "/_rustybin/mocks/orders",
        Some(&tag),
        Some(json!({ "responses": { "mode": "static" } })),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // From the current revision both pass.
    let (s, _, v) = call(
        &app,
        "PUT",
        "/_rustybin/mocks/orders/operations/listOrders",
        Some(&format!("W/\"{new}\"")),
        Some(json!({ "delay_ms": 50 })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["settings"]["delay_ms"], 50);
    // If-Match and expected_revision must agree; a bad If-Match is a 400.
    let (s, _, _) = call(
        &app,
        "PATCH",
        "/_rustybin/mocks/orders",
        Some("\"1\""),
        Some(json!({ "expected_revision": 2, "deploy": false })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _, _) = call(
        &app,
        "PATCH",
        "/_rustybin/mocks/orders",
        Some("*"),
        Some(json!({ "deploy": false })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn without_a_revision_edits_pass_unless_the_instance_requires_one() {
    let (state, app, _) = app().await;
    let (s, _, v) = call(
        &app,
        "PATCH",
        "/_rustybin/mocks/orders",
        None,
        Some(json!({ "responses": { "mode": "generated" } })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _, _) = call(
        &app,
        "PUT",
        "/_rustybin/mocks/orders/operations/listOrders",
        None,
        Some(json!({ "delay_ms": 10 })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    state
        .settings
        .update(
            [("mocks.require_revision".to_string(), json!(true))]
                .into_iter()
                .collect(),
            Default::default(),
            "test",
        )
        .map_err(|e| format!("{e:?}"))
        .expect("setting");
    for (method, uri, body) in [
        (
            "PATCH",
            "/_rustybin/mocks/orders",
            json!({ "responses": { "mode": "static" } }),
        ),
        (
            "PUT",
            "/_rustybin/mocks/orders/operations/listOrders",
            json!({ "delay_ms": 20 }),
        ),
    ] {
        let (s, _, v) = call(&app, method, uri, None, Some(body.clone())).await;
        assert_eq!(s, StatusCode::PRECONDITION_REQUIRED, "{method}: {v}");
        assert_eq!(v["error"], "rustybin_revision_required");
        let (_, _, cur) = call(&app, "GET", "/_rustybin/mocks/orders", None, None).await;
        let tag = format!("\"{}\"", cur["draft_revision"]);
        let (s, _, v) = call(&app, method, uri, Some(&tag), Some(body)).await;
        assert_eq!(s, StatusCode::OK, "{method}: {v}");
    }
}

#[test]
fn concurrent_operation_edits_without_a_revision_are_both_kept() {
    // Two set_operation calls on different operations racing: the store
    // makes the second again on top of the first instead of losing it.
    let state = test_state_with(Config::for_tests());
    let two = r#"{"openapi":"3.0.3","info":{"title":"T","version":"1"},"paths":{"/a":{"get":{"operationId":"a","responses":{"200":{"description":"ok"}}}},"/b":{"get":{"operationId":"b","responses":{"200":{"description":"ok"}}}}}}"#;
    let c = state
        .mocks
        .compile(two, Default::default())
        .expect("compile");
    state
        .mocks
        .create("t", c, crate::mocks::store::Origin::Api, true, true, false)
        .expect("create");
    std::thread::scope(|s| {
        for (op, ms) in [("a", 1u64), ("b", 2u64)] {
            let st = state.clone();
            s.spawn(move || {
                for _ in 0..20 {
                    st.mocks
                        .set_operation(
                            "t",
                            op,
                            Some(crate::mocks::model::OperationSettings {
                                delay_ms: Some(ms),
                                ..Default::default()
                            }),
                            None,
                        )
                        .expect("set");
                }
            });
        }
    });
    let d = state.mocks.get("t").expect("mock").draft;
    assert_eq!(
        d.settings.operations.get("a").and_then(|o| o.delay_ms),
        Some(1)
    );
    assert_eq!(
        d.settings.operations.get("b").and_then(|o| o.delay_ms),
        Some(2)
    );
}
