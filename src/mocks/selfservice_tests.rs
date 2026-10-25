//! Self-service mocks through the full application: ownership, caps, the
//! visit clock, the email step, the mocks domain, response rules, operators,
//! restarts and shared-mode keys.

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

use crate::config::{Config, SelfServiceConfig};
use crate::limits::ManualClock;
use crate::state::AppState;
use crate::test_support::{body_string, test_app_with};

const ADMIN: (&str, &str) = ("authorization", "Bearer admin-secret");

const SHOP: &str = r#"{"openapi":"3.0.3","info":{"title":"Shop","version":"1"},
  "paths":{
    "/page":{"get":{"operationId":"page","responses":{"200":{"description":"ok",
      "headers":{"Set-Cookie":{"schema":{"type":"string"},"example":"sid=1"},
                 "Location":{"schema":{"type":"string"},"example":"https://evil.example/"},
                 "X-Rate":{"schema":{"type":"integer"},"example":5}},
      "content":{"text/html":{"example":"<script>alert(1)</script>"}}}}}},
    "/items/{id}":{"get":{"operationId":"item","parameters":[{"name":"id","in":"path","required":true,
      "schema":{"type":"integer"}}],"responses":{"200":{"description":"ok","content":{"application/json":
      {"schema":{"type":"object","properties":{"id":{"type":"integer"}}}}}}}}}}}"#;

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .expect("time")
        .with_timezone(&Utc)
}

fn config(host: Option<&str>) -> Config {
    let mut c = Config::for_tests();
    c.public_mode = true;
    c.mocks_sample = false;
    c.admin_token = Some("admin-secret".into());
    c.self_service = SelfServiceConfig {
        enabled: true,
        host: host.map(str::to_string),
        home_url: Some("https://home.example/".into()),
        leads_salt: Some("test-salt".into()),
        ..SelfServiceConfig::default()
    };
    c
}

fn app_on(config: Config) -> (Router, AppState, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(at("2026-10-18T09:00:00Z")));
    let state = AppState::for_tests_with_clock(config, clock.clone());
    (test_app_with(state.clone()), state, clock)
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        b = b.header("host", "rustybin.test");
    }
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let req = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string())),
        None => b.body(Body::empty()),
    }
    .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let headers = resp.headers().clone();
    let text = body_string(resp).await;
    (
        status,
        headers,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

/// Create a mock as the visitor with `session` (and `token` when held).
async fn visitor_create(
    app: &Router,
    session: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut headers = vec![("x-rustybin-session", session)];
    if let Some(t) = token {
        headers.push(("x-rustybin-mock-token", t));
    }
    let (status, _, out) = call(app, "POST", "/_rustybin/mocks", &headers, Some(body)).await;
    (status, out)
}

fn shop() -> Value {
    json!({ "spec": SHOP })
}

fn minutes(m: u64) -> Duration {
    Duration::from_secs(m * 60)
}

#[tokio::test]
async fn visitors_own_their_mocks() {
    let (app, _, _) = app_on(config(None));
    let (status, a1) = visitor_create(&app, "a", None, shop()).await;
    assert_eq!(status, StatusCode::CREATED, "{a1}");
    let token_a = a1["manage_token"].as_str().expect("token").to_string();
    assert!(token_a.starts_with("rbm_"));
    let name_a = a1["name"].as_str().expect("name").to_string();
    assert!(
        name_a.starts_with("shop-") && name_a.len() == "shop-".len() + 6,
        "{name_a}"
    );
    assert_eq!(a1["owner"], "visit");
    assert_eq!(a1["yours"], true);
    assert_eq!(a1["visit"]["expires_in_secs"], 3600);
    assert_eq!(a1["visit"]["next_extension"], "free");
    let ta = [("x-rustybin-mock-token", token_a.as_str())];

    // A second mock joins the same visit (no new token).
    let (status, a2) = visitor_create(
        &app,
        "a",
        Some(&token_a),
        json!({ "spec": SHOP, "name": "My Shop!" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{a2}");
    assert!(a2.get("manage_token").is_none());
    assert!(a2["name"].as_str().expect("name").starts_with("my-shop-"));

    let (_, b1) = visitor_create(&app, "b", None, shop()).await;
    let token_b = b1["manage_token"].as_str().expect("token").to_string();
    let tb = [("x-rustybin-mock-token", token_b.as_str())];

    // Lists show your own mocks only.
    let (_, _, list) = call(&app, "GET", "/_rustybin/mocks", &ta, None).await;
    assert_eq!(list["count"], 2);
    assert_eq!(list["visit"]["mocks"].as_array().map(Vec::len), Some(2));
    assert_eq!(list["self_service"]["enabled"], true);
    assert_eq!(list["can_manage_instance"], false);
    let (_, _, list) = call(&app, "GET", "/_rustybin/mocks", &tb, None).await;
    assert_eq!(list["count"], 1);
    let (_, _, list) = call(&app, "GET", "/_rustybin/mocks", &[], None).await;
    assert_eq!(list["count"], 0);

    // Someone else's mock does not exist for B.
    let mine = format!("/_rustybin/mocks/{name_a}");
    for (method, uri) in [
        ("GET", mine.clone()),
        ("DELETE", mine.clone()),
        ("POST", format!("{mine}/disable")),
        ("GET", format!("{mine}/requests")),
        ("GET", format!("{mine}/spec")),
        ("GET", format!("{mine}/report")),
    ] {
        let (status, _, _) = call(&app, method, &uri, &tb, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
    }
    let (status, _, _) = call(&app, "PATCH", &mine, &tb, Some(json!({"deploy": true}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A manages it.
    let (status, _, detail) = call(&app, "GET", &mine, &ta, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["can_manage"], true);
    assert_eq!(detail["secrets_hidden"], false);
    let (status, _, _) = call(&app, "POST", &format!("{mine}/disable"), &ta, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(&app, "POST", &format!("{mine}/enable"), &ta, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, out) = call(&app, "PATCH", &mine, &ta, Some(json!({"name": "renamed"}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{out}");

    // Caps and restrictions.
    let (status, _) = visitor_create(&app, "a", Some(&token_a), shop()).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, out) = visitor_create(&app, "a", Some(&token_a), shop()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{out}");
    assert_eq!(out["error"], "rustybin_mock_cap");
    let (status, out) = visitor_create(
        &app,
        "c",
        None,
        json!({ "spec": SHOP, "responses": { "mode": "stateful" } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        out["error"].as_str().unwrap_or("").contains("stateful"),
        "{out}"
    );
    let (status, out) = visitor_create(
        &app,
        "c",
        None,
        json!({ "spec": SHOP, "operations": { "item": { "respond": { "headers": { "content-type": "text/html" } } } } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{out}");
    let (status, out) =
        visitor_create(&app, "c", None, json!({ "spec": SHOP, "name": "visits" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{out}");

    // Admin: the instance and its own by default, everyone's on request.
    let (_, _, list) = call(&app, "GET", "/_rustybin/mocks", &[ADMIN], None).await;
    assert_eq!(list["count"], 0);
    let (_, _, list) = call(&app, "GET", "/_rustybin/mocks?owner=all", &[ADMIN], None).await;
    assert_eq!(list["count"], 4);
    assert!(list["mocks"][0]["owner_fingerprint"]
        .as_str()
        .is_some_and(|f| f.starts_with("v:")));
    let (status, _, _) = call(&app, "DELETE", &mine, &[ADMIN], None).await;
    assert_eq!(status, StatusCode::OK);

    // An instance mock is read-only for visitors.
    let (status, _, _) = call(
        &app,
        "POST",
        "/_rustybin/mocks",
        &[ADMIN],
        Some(json!({ "spec": SHOP, "name": "shop" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, _, list) = call(&app, "GET", "/_rustybin/mocks", &ta, None).await;
    assert!(list["mocks"]
        .as_array()
        .expect("mocks")
        .iter()
        .any(|m| m["name"] == "shop" && m["yours"] == false));
    let (status, _, _) = call(&app, "GET", "/_rustybin/mocks/shop", &ta, None).await;
    assert_eq!(status, StatusCode::OK);
    // No admin token presented: the admin guard asks for one.
    let (status, _, _) = call(&app, "POST", "/_rustybin/mocks/shop/disable", &ta, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn visitor_responses_are_restricted() {
    let (app, _, _) = app_on(config(None));
    let (_, mock) = visitor_create(&app, "a", None, shop()).await;
    let name = mock["name"].as_str().expect("name").to_string();
    let (status, h, body) = call(&app, "GET", &format!("/mock/{name}/page"), &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h["content-type"], "text/plain; charset=utf-8");
    assert!(h.get("set-cookie").is_none());
    assert!(h.get("location").is_none());
    assert_eq!(h["x-rate"], "5");
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["content-security-policy"], crate::mocks::restrict::CSP);
    assert_eq!(body, "<script>alert(1)</script>");
    let findings = mock["report"]["findings"].to_string();
    assert!(findings.contains("Set-Cookie"), "{findings}");
    assert!(findings.contains("text/plain"), "{findings}");

    // The same spec as an instance mock is served as written.
    call(
        &app,
        "POST",
        "/_rustybin/mocks",
        &[ADMIN],
        Some(json!({ "spec": SHOP, "name": "shop" })),
    )
    .await;
    let (_, h, _) = call(&app, "GET", "/mock/shop/page", &[], None).await;
    assert_eq!(h["content-type"], "text/html");
    assert_eq!(h["set-cookie"], "sid=1");
}

#[tokio::test]
async fn the_mocks_domain_serves_only_mocks() {
    let (app, _, _) = app_on(config(Some("mocks.example")));
    let (_, mock) = visitor_create(&app, "a", None, shop()).await;
    let name = mock["name"].as_str().expect("name").to_string();
    let host = format!("{name}.mocks.example");
    assert_eq!(mock["url"], format!("http://{host}"));

    // The main host points at the mocks domain.
    let (status, _, out) = call(&app, "GET", &format!("/mock/{name}/items/1"), &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(out["url"], format!("https://{host}"));
    // So does its served spec, like its REST and MCP endpoints.
    for spec in ["openapi.json", "openapi.yaml"] {
        let path = format!("/mock/{name}/_rustybin/{spec}");
        let (status, _, _) = call(&app, "GET", &path, &[], None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
    // The mocks domain serves it, and nothing else.
    let (status, h, out) = call(&app, "GET", "/items/1", &[("host", &host)], None).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(h["x-rustybin-mock"], name.as_str());
    let (status, _, out) = call(
        &app,
        "GET",
        "/_rustybin/openapi.json",
        &[("host", &host)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["servers"][0]["url"], format!("http://{host}"));
    for path in ["/_rustybin/mocks", "/_rustybin/config", "/ui/"] {
        let (status, _, _) = call(&app, "GET", path, &[("host", &host)], None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
    let (status, h, _) = call(&app, "GET", "/", &[("host", "mocks.example")], None).await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(h["location"], "https://home.example/");
    let (status, _, _) = call(&app, "GET", "/", &[("host", "a.b.mocks.example")], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn one_hour_extend_to_three_email_then_six_a_day() {
    let tmp = tempfile::tempdir().expect("tmp");
    let leads = tmp.path().join("leads.jsonl");
    let mut c = config(None);
    c.self_service.leads_file = Some(leads.clone());
    c.self_service.privacy_url = Some("https://example.com/privacy".into());
    let (app, state, clock) = app_on(c);
    let (_, mock) = visitor_create(&app, "a", None, shop()).await;
    let name = mock["name"].as_str().expect("name").to_string();
    let token = mock["manage_token"].as_str().expect("token").to_string();
    let t = [("x-rustybin-mock-token", token.as_str())];
    let extend = |body: Option<Value>| {
        let app = app.clone();
        let token = token.clone();
        async move {
            call(
                &app,
                "POST",
                "/_rustybin/mocks/visit/extend",
                &[("x-rustybin-mock-token", token.as_str())],
                body,
            )
            .await
        }
    };

    let (status, _, out) = extend(None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{out}");
    assert_eq!(out["error"], "rustybin_extend_too_early");
    clock.advance(minutes(50));
    let (status, _, out) = extend(None).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["minutes_used"], 120);
    clock.advance(minutes(60));
    let (_, _, out) = extend(None).await;
    assert_eq!(out["minutes_used"], 180);
    assert_eq!(out["next_extension"], "email");
    clock.advance(minutes(60));
    let (status, _, out) = extend(None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(out["error"], "rustybin_email_required");
    assert_eq!(out["privacy_url"], "https://example.com/privacy");
    assert_eq!(out["consent"], "checkbox");
    let (status, _, out) = extend(Some(json!({ "email": "not an address" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{out}");
    let (status, _, out) = extend(Some(
        json!({ "email": "Ada@Example.com", "marketing_consent": true }),
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["email_given"], true);
    assert_eq!(out["minutes_used"], 240);
    assert!(!out.to_string().to_lowercase().contains("ada@"), "{out}");
    let written = std::fs::read_to_string(&leads).expect("leads file");
    let lead: Value = serde_json::from_str(written.trim()).expect("lead");
    assert_eq!(lead["email"], "Ada@example.com");
    assert_eq!(lead["marketing_consent"], true);
    assert_eq!(lead["source"], "mock_extend");
    assert_eq!(lead["mocks"], json!([name]));
    for _ in 0..2 {
        clock.advance(minutes(60));
        let (status, _, out) = extend(None).await;
        assert_eq!(status, StatusCode::OK, "{out}");
    }
    clock.advance(minutes(60));
    let (status, h, out) = extend(None).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(out["error"], "rustybin_daily_limit");
    assert!(h.contains_key("retry-after"));
    let (_, _, visit) = call(&app, "GET", "/_rustybin/mocks/visit", &t, None).await;
    assert_eq!(visit["next_extension"], "none");

    // The visit ends: its mock goes.
    clock.advance(minutes(16));
    let (status, _, _) = call(&app, "GET", "/_rustybin/mocks/visit", &t, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(&app, "GET", &format!("/mock/{name}/items/1"), &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(state.mocks.sweep(), 1);
    let metrics = state.metrics.render();
    assert!(
        metrics.contains("rustybin_mock_visit_events_total{event=\"extended\"} 5"),
        "{metrics}"
    );
    assert!(metrics.contains("rustybin_mock_visit_events_total{event=\"email_required\"} 1"));
    assert!(metrics.contains("rustybin_mock_leads_total{result=\"captured\"} 1"));
    assert!(!metrics.to_lowercase().contains("ada@"));
}

#[tokio::test]
async fn email_up_front_after_the_anonymous_visits() {
    let mut c = config(None);
    c.self_service.leads_file = Some(std::path::PathBuf::from("/dev/null"));
    c.self_service.privacy_url = Some("https://example.com/privacy".into());
    let (app, _, _) = app_on(c);
    for _ in 0..3 {
        let (status, _) = visitor_create(&app, "s", None, shop()).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (status, out) = visitor_create(&app, "s", None, shop()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{out}");
    assert_eq!(out["error"], "rustybin_email_required");
    let (status, _, out) = call(
        &app,
        "POST",
        "/_rustybin/mocks/visit",
        &[("x-rustybin-session", "s")],
        Some(json!({ "email": "bob@example.org" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{out}");
    assert_eq!(out["visit"]["email_given"], true);
    let token = out["manage_token"].as_str().expect("token");
    let (status, _) = visitor_create(&app, "s", Some(token), shop()).await;
    assert_eq!(status, StatusCode::CREATED);

    // Without email capture the free visits are all there is.
    let (app, _, _) = app_on(config(None));
    for _ in 0..3 {
        visitor_create(&app, "s", None, shop()).await;
    }
    let (status, out) = visitor_create(&app, "s", None, shop()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{out}");
    assert_eq!(out["error"], "rustybin_visit_limit");
    assert!(out["self_host_url"].as_str().is_some());
}

#[tokio::test]
async fn tokens_rotate_visits_end_and_operators_step_in() {
    let (app, _, _) = app_on(config(None));
    let (_, a) = visitor_create(&app, "a", None, shop()).await;
    let old = a["manage_token"].as_str().expect("token").to_string();
    let name = a["name"].as_str().expect("name").to_string();
    let (status, _, out) = call(
        &app,
        "POST",
        "/_rustybin/mocks/visit/token",
        &[("x-rustybin-mock-token", &old)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let new = out["manage_token"].as_str().expect("token").to_string();
    let (status, _, _) = call(
        &app,
        "GET",
        &format!("/_rustybin/mocks/{name}"),
        &[("x-rustybin-mock-token", &old)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(
        &app,
        "GET",
        &format!("/_rustybin/mocks/{name}"),
        &[("x-rustybin-mock-token", &new)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, b) = visitor_create(&app, "b", None, shop()).await;
    let token_b = b["manage_token"].as_str().expect("token").to_string();

    // Operators only.
    let (status, _, _) = call(&app, "GET", "/_rustybin/mocks/visits", &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, visits) = call(&app, "GET", "/_rustybin/mocks/visits", &[ADMIN], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(visits["count"], 2);
    assert_eq!(visits["visitor_mocks"], 2);
    let (status, _, _) = call(
        &app,
        "POST",
        "/_rustybin/mocks/visits/pause",
        &[ADMIN],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, out) = visitor_create(&app, "c", None, shop()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(out["error"], "rustybin_self_service_paused");
    call(
        &app,
        "POST",
        "/_rustybin/mocks/visits/resume",
        &[ADMIN],
        None,
    )
    .await;
    let id = visits["visits"]
        .as_array()
        .and_then(|v| v.iter().find(|r| r["mocks"] == json!([name])))
        .and_then(|r| r["id"].as_str())
        .expect("A's visit")
        .to_string();
    let (status, _, out) = call(
        &app,
        "DELETE",
        &format!("/_rustybin/mocks/visits/{id}"),
        &[ADMIN],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["deleted"], json!([name]));

    // The visitor ends their own visit.
    let (status, _, out) = call(
        &app,
        "DELETE",
        "/_rustybin/mocks/visit",
        &[("x-rustybin-mock-token", &token_b)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["deleted"].as_array().map(Vec::len), Some(1));
    let (_, _, visits) = call(&app, "GET", "/_rustybin/mocks/visits", &[ADMIN], None).await;
    assert_eq!(visits["count"], 0);
}

#[tokio::test]
async fn visitor_mocks_last_as_long_as_the_visit_across_restarts() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut c = config(None);
    c.mocks_state_dir = Some(tmp.path().display().to_string());
    let clock = Arc::new(ManualClock::new(at("2026-10-18T09:00:00Z")));
    let start = |c: &Config| {
        let state = AppState::for_tests_with_clock(c.clone(), clock.clone());
        test_app_with(state)
    };
    let app = start(&c);
    let (_, mock) = visitor_create(&app, "a", None, shop()).await;
    let name = mock["name"].as_str().expect("name").to_string();
    let token = mock["manage_token"].as_str().expect("token").to_string();
    assert!(tmp.path().join(format!("{name}.json")).exists());

    let app = start(&c);
    let (status, _, out) = call(
        &app,
        "GET",
        &format!("/_rustybin/mocks/{name}"),
        &[("x-rustybin-mock-token", &token)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["owner"], "visit");

    clock.advance(minutes(61));
    let app = start(&c);
    let (status, _, _) = call(&app, "GET", &format!("/mock/{name}/items/1"), &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!tmp.path().join(format!("{name}.json")).exists());
}

#[tokio::test]
async fn shared_mode_keys_own_their_mocks() {
    use crate::keys::tests::{doc, entry, shared_config, KEY_A, KEY_B};
    let mut c = shared_config();
    c.mocks_sample = false;
    c.self_service = SelfServiceConfig {
        enabled: true,
        ..SelfServiceConfig::default()
    };
    let state = crate::test_support::test_state_with(c);
    state
        .keys
        .load_bytes(
            doc(vec![
                entry("ka1", KEY_A, "starter", "active", json!({})),
                entry("kb2", KEY_B, "free", "active", json!({})),
            ])
            .as_bytes(),
        )
        .expect("registry");
    let app = test_app_with(state);
    let ka = [("x-rustybin-key", KEY_A)];
    let kb = [("x-rustybin-key", KEY_B)];
    let (status, _, mock) = call(&app, "POST", "/_rustybin/mocks", &ka, Some(shop())).await;
    assert_eq!(status, StatusCode::CREATED, "{mock}");
    assert_eq!(mock["owner"], "key");
    assert!(mock.get("manage_token").is_none());
    assert!(mock.get("visit").is_none());
    let name = mock["name"].as_str().expect("name").to_string();
    let (_, _, list) = call(&app, "GET", "/_rustybin/mocks", &kb, None).await;
    assert_eq!(list["count"], 0);
    let (status, _, _) = call(&app, "GET", &format!("/_rustybin/mocks/{name}"), &kb, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(&app, "GET", &format!("/_rustybin/mocks/{name}"), &ka, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(&app, "GET", &format!("/mock/{name}/items/3"), &ka, None).await;
    assert_eq!(status, StatusCode::OK);
    // Instance-wide actions still need the admin token.
    let (status, _, _) = call(&app, "POST", "/_rustybin/mocks/reload", &ka, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
