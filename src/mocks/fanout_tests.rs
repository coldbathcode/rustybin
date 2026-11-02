//! Specs whose levels each reference the next many times (review 0.3.1,
//! step 01): the operation detail's `inline_refs` and the `allOf` walks
//! of validation and echo stay bounded.

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Map, Value};
use tower::ServiceExt;

use crate::config::Config;
use crate::test_support::{body_json, test_app_with, test_state_with};

/// Levels L0..L8: each has `width` properties (or `allOf` parts) that all
/// reference the next level.
fn fanout_spec(width: usize, all_of: bool) -> Value {
    let mut schemas = Map::new();
    for level in 0..9 {
        let next = format!("#/components/schemas/L{}", level + 1);
        let schema = if level == 8 {
            json!({ "type": "object", "properties": { "leaf": { "type": "string" } } })
        } else if all_of {
            json!({ "allOf": (0..width).map(|_| json!({ "$ref": next })).collect::<Vec<_>>() })
        } else {
            let props: Map<String, Value> = (0..width)
                .map(|i| (format!("p{i}"), json!({ "$ref": next })))
                .collect();
            json!({ "type": "object", "properties": props })
        };
        schemas.insert(format!("L{level}"), schema);
    }
    json!({
        "openapi": "3.0.3",
        "info": { "title": "Fan-out", "version": "1" },
        "paths": { "/things": { "post": {
            "operationId": "makeThing",
            "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/L0" } } } },
            "responses": { "200": { "description": "ok", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/L0" }
            } } } }
        } } },
        "components": { "schemas": schemas }
    })
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
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
    (status, body_json(resp).await)
}

#[tokio::test]
async fn a_property_fan_out_does_not_grow_the_operation_detail() {
    let app = test_app_with(test_state_with(Config::for_tests()));
    let started = Instant::now();
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/mocks",
        Some(json!({ "name": "fan", "spec": fanout_spec(10, false), "force": true })),
    )
    .await;
    assert!(s.is_success(), "{s}: {v}");
    let (s, v) = call(
        &app,
        "GET",
        "/_rustybin/mocks/fan/operations/makeThing",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        v.to_string().len() < 8 * 1024 * 1024,
        "the detail is {} bytes",
        v.to_string().len()
    );
    assert!(v.to_string().contains("x-not-expanded"), "nothing was cut");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn an_all_of_fan_out_is_walked_once_per_reference() {
    let mut c = Config::for_tests();
    c.mocks_sample = false;
    let app = test_app_with(test_state_with(c));
    let started = Instant::now();
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/mocks",
        Some(json!({
            "name": "allof",
            "spec": fanout_spec(20, true),
            "responses": { "validate_requests": "enforce" },
            "force": true
        })),
    )
    .await;
    assert!(s.is_success(), "{s}: {v}");
    let (s, _) = call(
        &app,
        "POST",
        "/mock/allof/things",
        Some(json!({ "leaf": "x", "extra": 1 })),
    )
    .await;
    assert!(s.as_u16() < 500, "{s}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_path_key_with_a_newline_never_reaches_a_collection() {
    let app = test_app_with(test_state_with(Config::for_tests()));
    let spec = json!({
        "openapi": "3.0.3",
        "info": { "title": "Inject", "version": "1" },
        "paths": {
            "/ok": { "get": { "operationId": "ok", "responses": { "200": { "description": "ok" } } } },
            "/a\nGET http://evil.example/\n[Options]\noutput: /tmp/x": {
                "get": { "operationId": "evil", "summary": "line\u{2028}// break", "responses": { "200": { "description": "ok" } } }
            }
        }
    });
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/mocks",
        Some(json!({ "name": "inj", "spec": spec })),
    )
    .await;
    assert!(s.is_success(), "{s}: {v}");
    let req = Request::get("/_rustybin/mocks/inj/collections/hurl")
        .body(Body::empty())
        .expect("req");
    let resp = app.clone().oneshot(req).await.expect("resp");
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .expect("body")
        .to_bytes();
    let hurl = String::from_utf8_lossy(&bytes);
    assert!(!hurl.contains("evil.example"), "{hurl}");
    assert!(!hurl.contains('\u{2028}'));
}

#[tokio::test]
async fn an_informational_status_cannot_be_asked_for() {
    let app = test_app_with(test_state_with(Config::for_tests()));
    for (name, value) in [("prefer", "code=101"), ("x-rustybin-mock-status", "100")] {
        let req = Request::get("/mock/petstore/v1/pets")
            .header(name, value)
            .body(Body::empty())
            .expect("req");
        let resp = app.clone().oneshot(req).await.expect("resp");
        assert_eq!(resp.status(), StatusCode::OK, "{name}: {value}");
    }
}
