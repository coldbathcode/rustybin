use std::collections::BTreeMap;

use axum::http::{Extensions, HeaderMap, HeaderValue, Uri};
use axum::response::IntoResponse;
use axum::routing::get;
use serde_json::{json, Value};

use super::*;
use crate::config::Config;
use crate::state::AppState;

fn config() -> Config {
    let mut c = Config::for_tests();
    c.egress_allow = vec!["127.0.0.1".into()];
    c.egress_allow_loopback = true;
    c
}

async fn instance(c: Config) -> (crate::server::RunningServer, AppState, String) {
    let state = AppState::for_tests(c);
    let server = crate::server::start_with_state(state.clone())
        .await
        .expect("start");
    let base = format!("http://{}", server.http_addr);
    (server, state, base)
}

fn svc(state: &AppState, base: &str) -> Svc {
    let mut headers = HeaderMap::new();
    if state.config.admin_token.is_some() {
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer admin-secret"),
        );
    }
    headers.insert(
        "host",
        HeaderValue::from_str(base.trim_start_matches("http://")).expect("host"),
    );
    let uri: Uri = "/_rustybin/mocks".parse().expect("uri");
    Svc::from_request(state, &headers, &Extensions::new(), &uri)
}

const SPEC: &str = r##"{"openapi":"3.0.3","info":{"title":"Orders","version":"1"},"servers":[{"url":"/v1"}],
"paths":{
 "/orders/{id}":{"get":{"operationId":"getOrder","parameters":[{"name":"id","in":"path","required":true,"schema":{"type":"integer","example":7}}],
  "responses":{"200":{"description":"ok","content":{"application/json":{"schema":{"$ref":"#/components/schemas/Order"}}}}}}},
 "/orders":{"get":{"operationId":"listOrders","responses":{"200":{"description":"ok","content":{"application/json":{"schema":{"type":"array","items":{"$ref":"#/components/schemas/Order"}}}}}}}},
 "/legacy":{"get":{"operationId":"legacy","responses":{"200":{"description":"ok"}}}}
},
"components":{"schemas":{"Order":{"type":"object","required":["id","total"],"properties":{"id":{"type":"integer"},"total":{"type":"number"}}}}}}"##;

/// The stand-in real API's routes (constants: the catalogue's route scan
/// reads string literals passed to `.route`).
const ORDER_PATH: &str = "/v1/orders/{id}";
const ORDERS_PATH: &str = "/v1/orders";
const LEGACY_PATH: &str = "/v1/legacy";

/// A stand-in real API at `/v1`; `drifted` plants one difference of each kind.
async fn real_api(drifted: bool) -> String {
    let order = move |id: i64| {
        if drifted {
            // total is gone (a required field), currency is new.
            json!({ "id": id, "currency": "EUR" })
        } else {
            json!({ "id": id, "total": 12.5 })
        }
    };
    let app = axum::Router::new()
        .route(
            ORDER_PATH,
            get(
                move |axum::extract::Path(id): axum::extract::Path<i64>, h: HeaderMap| async move {
                    // The credential the check was given reaches the API.
                    if h.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer real") {
                        return (
                            axum::http::StatusCode::UNAUTHORIZED,
                            axum::Json(json!({ "error": "no" })),
                        )
                            .into_response();
                    }
                    axum::Json(order(id)).into_response()
                },
            ),
        )
        .route(
            ORDERS_PATH,
            get(move || async move {
                if drifted {
                    (axum::http::StatusCode::IM_A_TEAPOT, axum::Json(json!([]))).into_response()
                } else {
                    axum::Json(json!([{ "id": 1, "total": 3.0 }])).into_response()
                }
            }),
        );
    let app = if drifted {
        app
    } else {
        app.route(LEGACY_PATH, get(|| async { "ok" }))
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/v1")
}

async fn mock(state: &AppState, base: &str) {
    let body: crate::mocks::service::CreateBody =
        serde_json::from_value(json!({ "name": "orders", "spec": SPEC })).expect("body");
    svc(state, base)
        .compiling(move |s| s.create(body))
        .await
        .expect("create");
}

fn body(upstream: &str) -> DriftBody {
    DriftBody {
        upstream: Some(upstream.to_string()),
        headers: BTreeMap::from([("Authorization".to_string(), "Bearer {{TOKEN}}".to_string())]),
        variables: BTreeMap::from([("TOKEN".to_string(), "real".to_string())]),
        operations: None,
    }
}

fn kinds(run: &Value, op: &str) -> Vec<String> {
    run["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|r| r["operation"] == op)
        .map(|r| {
            r["findings"]
                .as_array()
                .expect("findings")
                .iter()
                .filter_map(|f| f["kind"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn an_api_that_matches_has_no_findings_and_each_drift_is_named() {
    let (_server, state, base) = instance(config()).await;
    mock(&state, &base).await;
    let same = real_api(false).await;
    let r = run(svc(&state, &base), "orders", body(&same))
        .await
        .expect("run");
    assert_eq!(r["passed"], true, "{r:#}");
    assert_eq!(r["operations"], 3);
    let drifted = real_api(true).await;
    let r = run(svc(&state, &base), "orders", body(&drifted))
        .await
        .expect("run");
    assert_eq!(r["passed"], false);
    let order = kinds(&r, "getOrder");
    assert!(order.contains(&"missing_field".to_string()), "{r:#}");
    assert!(order.contains(&"undocumented_field".to_string()), "{r:#}");
    assert_eq!(kinds(&r, "listOrders"), ["status_differs"], "{r:#}");
    assert_eq!(kinds(&r, "legacy"), ["unreachable"], "{r:#}");
    // The credential is never kept: not in the run, not in the kept runs.
    let kept = runs(&svc(&state, &base), "orders").expect("runs");
    assert_eq!(kept["runs"].as_array().map(Vec::len), Some(2));
    assert!(!kept.to_string().contains("Bearer"), "{kept}");
    // JUnit: a test case per operation, a failure per drifted one.
    let xml = junit(&r);
    assert!(xml.contains("tests=\"3\" failures=\"3\""), "{xml}");
    assert!(xml.contains("status_differs: 418"), "{xml}");
}

#[tokio::test]
async fn a_check_needs_its_variables_and_an_allowed_host() {
    let (_server, state, base) = instance(config()).await;
    mock(&state, &base).await;
    // A variable not given: refused before anything is sent.
    let mut b = body("http://127.0.0.1:9/v1");
    b.variables.clear();
    let e = run(svc(&state, &base), "orders", b)
        .await
        .expect_err("refused");
    assert!(e.message().contains("variable TOKEN"), "{}", e.message());
    // No upstream at all.
    let e = run(svc(&state, &base), "orders", DriftBody::default())
        .await
        .expect_err("refused");
    assert!(e.message().contains("drift.upstream"), "{}", e.message());
    // A host outside the policy.
    let (_s2, state2, base2) = instance(Config::for_tests()).await;
    mock(&state2, &base2).await;
    let e = run(
        svc(&state2, &base2),
        "orders",
        body("https://api.example.com/v1"),
    )
    .await
    .expect_err("refused");
    assert_eq!(e.status.as_u16(), 403);
    // The mock's own drift.upstream is the default.
    let same = real_api(false).await;
    let patch: crate::mocks::service::PatchBody =
        serde_json::from_value(json!({ "drift": { "upstream": same } })).expect("patch");
    svc(&state, &base)
        .compiling(move |s| s.update("orders", patch))
        .await
        .expect("patch");
    let mut b = body("");
    b.upstream = None;
    let r = run(svc(&state, &base), "orders", b).await.expect("run");
    assert_eq!(r["passed"], true, "{r:#}");
}
