//! Fetching a spec: who may ask, what the policy allows, what comes back.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::config::{Config, SelfServiceConfig};
use crate::test_support::{body_json, test_app_with, test_state_with};

const SPEC: &str = "openapi: 3.0.3\ninfo: {title: Orders, version: '1'}\npaths:\n  /orders:\n    get:\n      operationId: list\n      responses: {'200': {description: ok}}\n";

/// A server of specs on a free port: `/spec`, `/moved`, `/gone`, `/big`.
async fn upstream() -> String {
    // A fallback, not routes: the catalogue test reads every `.route` literal.
    let router = Router::new().fallback(|uri: axum::http::Uri| async move {
        match uri.path() {
            "/spec" => SPEC.into_response(),
            "/moved" => (
                StatusCode::FOUND,
                [("location", "http://169.254.169.254/")],
                "",
            )
                .into_response(),
            "/big" => "x".repeat(4 * 1024 * 1024).into_response(),
            "/binary" => vec![0xffu8, 0xfe, 0x00].into_response(),
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, router).await });
    format!("http://{addr}")
}

fn allowed() -> Config {
    let mut c = Config::for_tests();
    c.egress_allow = vec!["127.0.0.1".into()];
    c.egress_allow_loopback = true;
    c
}

async fn post(app: &Router, path: &str, body: Value, token: Option<&str>) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri(path)
        .header("host", "rustybin.test")
        .header("content-type", "application/json");
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    let req = b.body(Body::from(body.to_string())).expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    (status, body_json(resp).await)
}

const FETCH: &str = "/_rustybin/mocks/import/fetch";

#[tokio::test]
async fn a_spec_is_fetched_and_a_mock_made_from_its_url() {
    let base = upstream().await;
    let app = test_app_with(test_state_with(allowed()));
    let url = format!("{base}/spec");
    let (status, v) = post(&app, FETCH, json!({ "url": url }), None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["spec"], SPEC);
    assert_eq!(v["bytes"], SPEC.len());

    let (status, v) = post(
        &app,
        "/_rustybin/mocks",
        json!({ "name": "from-a-url", "spec_url": url }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert_eq!(v["name"], "from-a-url");

    let (status, v) = post(
        &app,
        "/_rustybin/mocks",
        json!({ "name": "both", "spec_url": url, "spec": SPEC }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
}

#[tokio::test]
async fn the_policy_decides_and_has_no_default() {
    let base = upstream().await;
    let mut c = Config::for_tests();
    c.egress_allow_loopback = true;
    let app = test_app_with(test_state_with(c));
    let (status, v) = post(&app, FETCH, json!({ "url": format!("{base}/spec") }), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["reason"], "egress_refused");
    assert_eq!(v["purpose"], "spec_fetch");
    // Metadata addresses, whatever is listed.
    let mut c = allowed();
    c.egress_allow.push("169.254.169.254".into());
    let app = test_app_with(test_state_with(c));
    let (status, v) = post(
        &app,
        FETCH,
        json!({ "url": "http://169.254.169.254/latest/meta-data/" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
}

#[tokio::test]
async fn a_redirect_is_not_followed_and_what_comes_back_is_bounded() {
    let base = upstream().await;
    let app = test_app_with(test_state_with(allowed()));
    let (status, v) = post(&app, FETCH, json!({ "url": format!("{base}/moved") }), None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{v}");
    assert!(
        v["error"].as_str().expect("error").contains("redirect"),
        "{v}"
    );
    let (status, _) = post(&app, FETCH, json!({ "url": format!("{base}/gone") }), None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let (status, v) = post(&app, FETCH, json!({ "url": format!("{base}/big") }), None).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
    let (status, v) = post(
        &app,
        FETCH,
        json!({ "url": format!("{base}/binary") }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let (status, _) = post(&app, FETCH, json!({}), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_visitor_never_makes_the_instance_reach_out() {
    let base = upstream().await;
    let mut c = allowed();
    c.public_mode = true;
    c.admin_token = Some("adm".into());
    c.self_service = SelfServiceConfig {
        enabled: true,
        ..SelfServiceConfig::default()
    };
    let app = test_app_with(test_state_with(c));
    let url = format!("{base}/spec");
    let (status, v) = post(&app, FETCH, json!({ "url": url }), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");
    let (status, v) = post(
        &app,
        "/_rustybin/mocks",
        json!({ "name": "theirs", "spec_url": url }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");
    // The operator may.
    let (status, v) = post(&app, FETCH, json!({ "url": url }), Some("adm")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
}
