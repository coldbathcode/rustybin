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

#[test]
fn edit_draft_refuses_a_stale_revision_and_merges_racing_edits() {
    let state = test_state_with(Config::for_tests());
    let c = state
        .mocks
        .compile(SPEC, Default::default())
        .expect("compile");
    state
        .mocks
        .create("e", c, crate::mocks::store::Origin::Api, true, true, false)
        .expect("create");
    let rev = state.mocks.get("e").expect("mock").draft.revision;
    // From the current revision: passes; from it again: stale.
    let store = &state.mocks;
    store
        .edit_draft("e", Some(rev), |d| {
            let mut s = d.settings.clone();
            s.responses.delay_ms = 5;
            store
                .recompile(d, None, s)
                .map_err(crate::mocks::store::StoreError::Invalid)
        })
        .map_err(|e| e.message())
        .expect("first edit");
    let stale = store.edit_draft("e", Some(rev), |d| {
        store
            .recompile(d, None, d.settings.clone())
            .map_err(crate::mocks::store::StoreError::Invalid)
    });
    assert!(
        matches!(stale, Err(crate::mocks::store::StoreError::Stale { .. })),
        "a stale edit went through"
    );
    // The AI examples path and an operation edit racing: both kept.
    std::thread::scope(|s| {
        s.spawn(|| {
            for _ in 0..10 {
                store
                    .edit_draft("e", None, |d| {
                        let mut st = d.settings.clone();
                        st.responses.seed = 99;
                        store
                            .recompile(d, None, st)
                            .map_err(crate::mocks::store::StoreError::Invalid)
                    })
                    .map_err(|e| e.message())
                    .expect("seed");
            }
        });
        s.spawn(|| {
            for _ in 0..10 {
                store
                    .set_operation(
                        "e",
                        "listOrders",
                        Some(crate::mocks::model::OperationSettings {
                            delay_ms: Some(7),
                            ..Default::default()
                        }),
                        None,
                    )
                    .map_err(|e| e.message())
                    .expect("op");
            }
        });
    });
    let d = store.get("e").expect("mock").draft;
    assert_eq!(d.settings.responses.seed, 99);
    assert_eq!(d.settings.responses.delay_ms, 5);
    assert_eq!(
        d.settings
            .operations
            .get("listOrders")
            .and_then(|o| o.delay_ms),
        Some(7)
    );
}

#[tokio::test]
async fn a_deploy_names_the_revision_it_publishes() {
    let (state, app, rev) = app().await;
    let tag = format!("\"{rev}\"");
    // Someone edits the draft meanwhile.
    let (s, _, v) = call(
        &app,
        "PATCH",
        "/_rustybin/mocks/orders",
        None,
        Some(json!({ "responses": { "mode": "generated" }, "deploy": false })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let live_before = v["live_revision"].clone();
    // A deploy of the revision the page showed is refused, with what changed.
    let (s, _, v) = call(
        &app,
        "POST",
        "/_rustybin/mocks/orders/deploy",
        Some(&tag),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"], "rustybin_stale_revision");
    assert_eq!(v["expected_revision"], rev);
    assert!(v["changed"].to_string().contains("responses"), "{v}");
    let mock = state.mocks.get("orders").expect("orders");
    assert_eq!(
        json!(mock.live.as_ref().map(|l| l.revision)),
        live_before,
        "nothing went live"
    );
    // The body form, with the current revision, publishes it.
    let current = mock.draft.revision;
    let (s, _, v) = call(
        &app,
        "POST",
        "/_rustybin/mocks/orders/deploy",
        None,
        Some(json!({ "expected_revision": current })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["revision"], current);
    // Both forms must agree; an unknown field is refused; no body still deploys.
    let (s, _, _) = call(
        &app,
        "POST",
        "/_rustybin/mocks/orders/deploy",
        Some(&tag),
        Some(json!({ "expected_revision": current })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _, _) = call(
        &app,
        "POST",
        "/_rustybin/mocks/orders/deploy",
        None,
        Some(json!({ "revision": current })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _, v) = call(&app, "POST", "/_rustybin/mocks/orders/deploy", None, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn a_patch_with_deploy_publishes_the_revision_it_wrote() {
    let (_, app, rev) = app().await;
    let (s, etag, v) = call(
        &app,
        "PATCH",
        "/_rustybin/mocks/orders",
        Some(&format!("\"{rev}\"")),
        Some(json!({ "responses": { "mode": "static" }, "deploy": true })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let written = v["draft_revision"].as_u64().expect("draft");
    assert_eq!(v["live_revision"], written, "{v}");
    assert_eq!(etag, Some(format!("\"{written}\"")));
}

// ── Kept revisions (testing workflows 2, step 06: the compare view) ──

async fn get_revision(app: &Router, name: &str, rev: u64) -> (StatusCode, Value) {
    let (s, _, v) = call(
        app,
        "GET",
        &format!("/_rustybin/mocks/{name}/revisions/{rev}"),
        None,
        None,
    )
    .await;
    (s, v)
}

#[tokio::test]
async fn a_kept_revision_answers_its_spec_and_settings() {
    let (_, app, live) = app().await;
    // Two edits of the draft, neither deployed: the first is an earlier draft.
    let (s, _, v) = call(
        &app,
        "PATCH",
        "/_rustybin/mocks/orders",
        None,
        Some(json!({ "responses": { "mode": "generated" }, "deploy": false })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let first = v["draft_revision"].as_u64().expect("rev");
    let (s, _, v) = call(
        &app,
        "PATCH",
        "/_rustybin/mocks/orders",
        None,
        Some(json!({ "responses": { "mode": "generated", "seed": 3 }, "deploy": false })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let draft = v["draft_revision"].as_u64().expect("rev");

    let (s, v) = get_revision(&app, "orders", first).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["name"], "orders");
    assert_eq!(v["revision"], first);
    assert_eq!(v["draft"], false);
    assert_eq!(v["live"], false);
    assert_eq!(v["current_draft_revision"], draft);
    assert_eq!(v["settings"]["responses"]["mode"], "generated");
    assert_ne!(v["settings"]["responses"]["seed"], 3, "{v}");
    assert_eq!(v["changes_to_draft"], json!(["responses"]));
    assert_eq!(v["spec"]["format"], "json");
    assert!(
        v["spec"]["text"]
            .as_str()
            .is_some_and(|t| t.contains("listOrders")),
        "{v}"
    );
    assert_eq!(v["operations"][0]["id"], "listOrders");
    // Every section is there, the default MCP server too.
    assert_eq!(v["settings"]["mcp"]["enabled"], true, "{v}");

    // The live revision and the draft are kept revisions too.
    let (s, v) = get_revision(&app, "orders", live).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["live"], true);
    assert_eq!(v["settings"]["responses"]["mode"], "examples");
    let (_, v) = get_revision(&app, "orders", draft).await;
    assert_eq!(v["draft"], true);
    assert_eq!(v["changes_to_draft"], json!([]));

    // An edit from the earlier draft is stale and says what changed since.
    let (s, _, v) = call(
        &app,
        "PATCH",
        "/_rustybin/mocks/orders",
        Some(&format!("\"{first}\"")),
        Some(json!({ "mcp": { "enabled": false } })),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["changed"], json!(["responses"]), "{v}");

    // Unknown revisions and unknown mocks are 404.
    let (s, _) = get_revision(&app, "orders", 7).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = get_revision(&app, "nope", first).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_revision_gone_from_the_history_is_404() {
    let (state, app, _) = app().await;
    let keep = state.mocks.limits().revisions;
    let mut seen = Vec::new();
    for seed in 0..(keep as u64 + 2) {
        let (s, _, v) = call(
            &app,
            "PATCH",
            "/_rustybin/mocks/orders",
            None,
            Some(json!({ "responses": { "seed": seed + 1 }, "deploy": false })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        seen.push(v["draft_revision"].as_u64().expect("rev"));
    }
    // The oldest earlier drafts are evicted, the newest are kept.
    let (s, v) = get_revision(&app, "orders", seen[0]).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    assert!(
        v["error"].as_str().unwrap_or("").contains("no longer kept"),
        "{v}"
    );
    let (s, v) = get_revision(&app, "orders", seen[seen.len() - 2]).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // A stale edit from an evicted revision still answers 409, saying so.
    let (s, _, v) = call(
        &app,
        "PATCH",
        "/_rustybin/mocks/orders",
        Some(&format!("\"{}\"", seen[0])),
        Some(json!({ "mcp": { "enabled": false } })),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(
        v["changed"].as_str().unwrap_or("").contains("no longer"),
        "{v}"
    );
}
