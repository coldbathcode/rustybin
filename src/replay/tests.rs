use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::test_support::{test_app_with, test_state_with};

/// The history of runs.
mod history_runs;

const SPEC: &str = r#"{"openapi":"3.0.3","info":{"title":"Orders","version":"1"},"paths":{"/orders/{id}":{"get":{"operationId":"getOrder","parameters":[{"name":"id","in":"path","required":true,"schema":{"type":"integer"}}],"responses":{"200":{"description":"ok","content":{"application/json":{"example":{"id":1,"total":9.5}}}}}}}}}"#;

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    session: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-rustybin-session", session);
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

async fn with_orders(c: Config) -> (AppState, Router) {
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/mocks",
        "ops",
        Some(json!({ "name": "orders", "spec": SPEC })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    (state, app)
}

/// The id of the last capture of a session.
fn last_capture(state: &AppState, session: &str) -> String {
    state
        .inspector
        .for_session(session, 1)
        .first()
        .map(|c| c.id.clone())
        .expect("a capture")
}

#[tokio::test]
async fn a_capture_becomes_a_test_that_passes_then_names_the_failing_assertion() {
    let (state, app) = with_orders(Config::for_tests()).await;
    let (s, _) = call(&app, "GET", "/mock/orders/orders/1", "alice", None).await;
    assert_eq!(s, StatusCode::OK);
    let id = last_capture(&state, "alice");
    let (s, v) = call(
        &app,
        "POST",
        LIST_PATH,
        "alice",
        Some(json!({ "name": "order-1", "capture": id, "assert": [
            { "status": 200 },
            { "json": "$.total", "equals": 9.5 },
            { "header": "content-type", "equals": "application/json" },
            { "body_contains": "\"id\"" },
            { "max_ms": 5000 }
        ] })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["request"]["path"], "/mock/orders/orders/1");
    assert!(
        v["request"]["headers"].get("x-rustybin-session").is_none(),
        "{v}"
    );

    let (s, r) = call(&app, "POST", RUN_PATH, "alice", Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["passed"], true, "{r:#}");

    // The mock changes: the run names the assertion that no longer holds.
    let (s, v) = call(
        &app,
        "PUT",
        "/_rustybin/mocks/orders/operations/getOrder",
        "ops",
        Some(json!({ "respond": { "status": 200, "body": { "id": 1, "total": 12 } } })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/mocks/orders/deploy",
        "ops",
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, r) = call(
        &app,
        "POST",
        RUN_PATH,
        "alice",
        Some(json!({ "names": ["order-1"] })),
    )
    .await;
    assert_eq!(r["passed"], false, "{r:#}");
    let failures = &r["results"][0]["failures"];
    assert_eq!(failures.as_array().map(Vec::len), Some(1), "{r:#}");
    assert_eq!(failures[0]["assertion"], "json $.total");
    assert_eq!(failures[0]["expected"], "9.5");
    assert_eq!(failures[0]["seen"], "12");

    // JUnit names it too.
    let req = Request::post(format!("{RUN_PATH}?format=junit"))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .expect("req");
    let resp = app.clone().oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()["content-type"]
        .to_str()
        .unwrap_or("")
        .starts_with("application/xml"));
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .expect("b")
        .to_bytes();
    let xml = String::from_utf8_lossy(&bytes);
    assert!(
        xml.contains("<testsuite name=\"rustybin\" tests=\"1\" failures=\"1\""),
        "{xml}"
    );
    assert!(
        xml.contains("<failure message=\"json $.total: expected 9.5, saw 12\""),
        "{xml}"
    );

    // The Hurl export carries the assertions.
    let req = Request::get(EXPORT_PATH).body(Body::empty()).expect("req");
    let resp = app.clone().oneshot(req).await.expect("resp");
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .expect("b")
        .to_bytes();
    let hurl = String::from_utf8_lossy(&bytes);
    for line in [
        "GET {{base_url}}/mock/orders/orders/1",
        "status == 200",
        "jsonpath \"$.total\" == 9.5",
        "header \"content-type\" == \"application/json\"",
        "body contains \"\\\"id\\\"\"",
        "duration <= 5000",
    ] {
        assert!(hurl.contains(line), "{line} in\n{hurl}");
    }
}

#[tokio::test]
async fn captured_credentials_become_variables_never_values() {
    let (state, app) = with_orders(Config::for_tests()).await;
    let req = Request::get("/mock/orders/orders/1?token=tok-777&page=2")
        .header("x-rustybin-session", "alice")
        .header("x-api-key", "secret-value-123")
        .header("accept", "application/json")
        .body(Body::empty())
        .expect("req");
    app.clone().oneshot(req).await.expect("resp");
    let id = last_capture(&state, "alice");
    let (s, v) = call(
        &app,
        "POST",
        LIST_PATH,
        "alice",
        Some(json!({ "name": "keyed", "capture": id })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let text = v.to_string();
    assert!(
        !text.contains("secret-value-123") && !text.contains("tok-777"),
        "{text}"
    );
    assert_eq!(v["request"]["headers"]["x-api-key"], "{{X_API_KEY}}");
    assert_eq!(
        v["request"]["path"],
        "/mock/orders/orders/1?token={{TOKEN}}&page=2"
    );
    assert_eq!(v["introduced_variables"], json!(["TOKEN", "X_API_KEY"]));
    assert_eq!(v["assert"], json!([{ "status": 200 }]));
    // Without the variables the test does not run; with them it passes.
    let (_, r) = call(&app, "POST", RUN_PATH, "alice", Some(json!({}))).await;
    assert!(
        r["results"][0]["error"]
            .as_str()
            .unwrap_or("")
            .contains("needs the variable"),
        "{r}"
    );
    let (_, r) = call(
        &app,
        "POST",
        RUN_PATH,
        "alice",
        Some(json!({ "variables": { "TOKEN": "t", "X_API_KEY": "k" } })),
    )
    .await;
    assert_eq!(r["passed"], true, "{r:#}");
}

#[tokio::test]
async fn another_sessions_capture_cannot_be_saved_on_a_public_instance() {
    let mut c = Config::for_tests();
    c.public_mode = true;
    c.admin_token = Some("admin-secret".into());
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    let req = Request::get("/echo")
        .header("x-rustybin-session", "alice")
        .body(Body::empty())
        .expect("req");
    app.clone().oneshot(req).await.expect("resp");
    let id = last_capture(&state, "alice");
    let save = |session: &'static str| {
        let app = app.clone();
        let id = id.clone();
        async move {
            let req = Request::post(LIST_PATH)
                .header("x-rustybin-session", session)
                .header("authorization", "Bearer admin-secret")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "name": format!("from-{session}"), "capture": id }).to_string(),
                ))
                .expect("req");
            app.oneshot(req).await.expect("resp").status()
        }
    };
    assert_eq!(save("bob").await, StatusCode::NOT_FOUND);
    assert_eq!(save("alice").await, StatusCode::CREATED);
    // Without the admin token nothing is saved.
    let req = Request::post(LIST_PATH)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "name": "x", "capture": id }).to_string(),
        ))
        .expect("req");
    assert_eq!(
        app.oneshot(req).await.expect("resp").status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_gateway_run_is_refused_until_the_egress_policy_allows_it() {
    // A real instance stands in for the gateway (it serves the same mock).
    let upstream_state = crate::state::AppState::for_tests(Config::for_tests());
    let server = crate::server::start_with_state(upstream_state.clone())
        .await
        .expect("start");
    let gateway = format!("http://{}", server.http_addr);
    let test = SavedTest {
        name: "via-gateway".into(),
        description: None,
        request: TestRequest {
            method: "GET".into(),
            path: "/echo".into(),
            headers: Default::default(),
            body: None,
        },
        gateway: Some(gateway.clone()),
        assert: vec![Assertion::status(200)],
    };
    for (allowed, passed) in [(false, false), (true, true)] {
        let mut c = Config::for_tests();
        if allowed {
            c.egress_allow = vec!["127.0.0.1".into()];
            c.egress_allow_loopback = true;
        }
        let state = test_state_with(c);
        state.saved_tests.put(test.clone(), false).expect("put");
        let r = run::run(&state, "s", run::RunBody::default())
            .await
            .expect("run");
        assert_eq!(r.passed, passed, "{r:?}");
        if !allowed {
            let e = r.results[0].error.clone().unwrap_or_default();
            assert!(e.contains("egress") && e.contains("replay"), "{e}");
        }
    }
}

#[test]
fn tests_are_validated_and_bounded() {
    let store = Tests::new(None);
    let t = |name: &str| SavedTest {
        name: name.into(),
        description: None,
        request: TestRequest {
            method: "GET".into(),
            path: "/echo".into(),
            headers: Default::default(),
            body: None,
        },
        gateway: None,
        assert: vec![],
    };
    for bad in ["run", "export", "", "a b"] {
        assert!(store.put(t(bad), false).is_err(), "{bad}");
    }
    let mut control = t("control");
    control.request.path = "/_rustybin/settings".into();
    assert!(store.put(control, false).is_err());
    let mut url = t("url");
    url.request.path = "https://example.com/".into();
    assert!(store.put(url, false).is_err());
    // A newline or space would turn one exported Hurl request into several.
    let mut injected = t("injected");
    injected.request.path = "/a\nGET http://evil.example/".into();
    assert!(store.put(injected, false).is_err());
    let mut spaced = t("spaced");
    spaced.gateway = Some("http://gw.example.com/a b".into());
    assert!(store.put(spaced, false).is_err());
    let mut two = t("two-kinds");
    two.assert = vec![Assertion {
        status: Some(200),
        max_ms: Some(1),
        ..Assertion::default()
    }];
    assert!(store.put(two, false).is_err());
    let mut path = t("bad-path");
    path.assert = vec![Assertion {
        json: Some("items[0]".into()),
        ..Assertion::default()
    }];
    assert!(store.put(path, false).is_err());
    for i in 0..MAX_TESTS {
        store.put(t(&format!("t{i}")), false).expect("put");
    }
    assert!(matches!(
        store.put(t("one-more"), false),
        Err(StoreError::Full)
    ));
    assert!(matches!(
        store.put(t("t1"), false),
        Err(StoreError::Exists(_))
    ));
    assert!(store.put(t("t1"), true).is_ok());
}

#[test]
fn json_paths() {
    let v = json!({ "items": [{ "id": 7, "a b": true }], "n": null });
    let get = |p: &str| run::lookup(&v, &run::parse_path(p).expect("path")).cloned();
    assert_eq!(get("$.items[0].id"), Some(json!(7)));
    assert_eq!(get("$.items[0]['a b']"), Some(json!(true)));
    assert_eq!(get("$.n"), Some(Value::Null));
    assert_eq!(get("$.missing"), None);
    assert_eq!(get("$"), Some(v.clone()));
    assert!(run::parse_path("items").is_err());
    assert!(run::parse_path("$..x").is_err());
}

#[test]
fn saved_tests_survive_a_restart() {
    let dir = tempfile::tempdir().expect("dir");
    let a = Tests::new(Some(dir.path()));
    let mut t = SavedTest {
        name: "kept".into(),
        description: Some("d".into()),
        request: TestRequest {
            method: "POST".into(),
            path: "/echo".into(),
            headers: [("content-type".to_string(), "application/json".to_string())].into(),
            body: Some("{\"x\":1}".into()),
        },
        gateway: None,
        assert: vec![Assertion::status(200)],
    };
    a.put(t.clone(), false).expect("put");
    let b = Tests::new(Some(dir.path()));
    assert_eq!(b.get("kept"), Some(t.clone()));
    t.name = "other".into();
    b.put(t, false).expect("put");
    assert!(b.delete("kept").expect("deleted"));
    let c = Tests::new(Some(dir.path()));
    assert_eq!(
        c.list().iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["other"]
    );
}

#[test]
fn a_document_carries_the_tests_and_prunes_only_what_it_does_not_name() {
    let state = test_state_with(Config::for_tests());
    let t = |name: &str, path: &str| SavedTest {
        name: name.into(),
        description: None,
        request: TestRequest {
            method: "GET".into(),
            path: path.into(),
            headers: Default::default(),
            body: None,
        },
        gateway: None,
        assert: vec![Assertion::status(200)],
    };
    for n in ["keep", "gone", "broken"] {
        state.saved_tests.put(t(n, "/echo"), false).expect("put");
    }
    let doc = crate::configdoc::export(&state, crate::configdoc::SecretsMode::Refs, false).file;
    assert_eq!(doc.tests.as_ref().map(Vec::len), Some(3));
    // A document naming keep (changed) and broken (invalid now): gone is
    // pruned, broken is reported and kept.
    let file = crate::mocks::config_file::MocksFile {
        version: 2,
        tests: Some(vec![
            t("keep", "/status/201"),
            t("broken", "/_rustybin/settings"),
        ]),
        ..Default::default()
    };
    let opts = crate::configdoc::ApplyOptions {
        prune: true,
        dry_run: false,
        principal: "test".into(),
        base: None,
        untrusted: false,
    };
    let r = crate::configdoc::apply(&state, file, &opts);
    assert_eq!(r["results"]["tests"]["deleted"], json!(["gone"]), "{r:#}");
    assert!(
        r["problems"][0]
            .as_str()
            .unwrap_or("")
            .starts_with("tests.broken"),
        "{r:#}"
    );
    let names: Vec<String> = state
        .saved_tests
        .list()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, vec!["broken", "keep"]);
    assert_eq!(
        state.saved_tests.get("keep").map(|t| t.request.path),
        Some("/status/201".into())
    );
}

#[tokio::test]
async fn a_run_never_shows_or_captures_a_filled_secret() {
    let state = test_state_with(Config::for_tests());
    let _app = test_app_with(state.clone());
    let test = SavedTest {
        name: "echoes".into(),
        description: None,
        request: TestRequest {
            method: "POST".into(),
            path: "/anything".into(),
            headers: [("x-api-key".to_string(), "{{API_KEY}}".to_string())].into(),
            body: Some("token is {{API_KEY}}".into()),
        },
        gateway: None,
        assert: vec![Assertion {
            body_contains: Some("never there".into()),
            ..Assertion::default()
        }],
    };
    state.saved_tests.put(test, false).expect("put");
    let r = run::run(
        &state,
        "scrub-session",
        run::RunBody {
            names: vec![],
            variables: [("API_KEY".to_string(), "sk-live-secret-42".to_string())].into(),
        },
    )
    .await
    .expect("run");
    let text = serde_json::to_string(&r).expect("json");
    assert!(!text.contains("sk-live-secret-42"), "{text}");
    assert!(text.contains("{{API_KEY}}"), "{text}");
    // The in-process request's capture is redacted too.
    let caps = state.inspector.for_session("scrub-session", 5);
    assert!(!caps.is_empty());
    for c in caps {
        let key = c
            .headers
            .iter()
            .find(|h| h.name == "x-api-key")
            .map(|h| h.value.clone());
        assert_eq!(key.as_deref(), Some("[redacted]"));
    }
}

#[test]
fn a_literal_credential_is_never_saved() {
    let store = Tests::new(None);
    let mut t = SavedTest {
        name: "literal".into(),
        description: None,
        request: TestRequest {
            method: "GET".into(),
            path: "/echo".into(),
            headers: [("authorization".to_string(), "Bearer abc123".to_string())].into(),
            body: None,
        },
        gateway: None,
        assert: vec![],
    };
    assert!(store.put(t.clone(), false).is_err());
    t.request
        .headers
        .insert("authorization".into(), "Bearer {{TOKEN}}".into());
    assert!(store.put(t.clone(), false).is_ok());
    t.name = "query".into();
    t.request.headers.clear();
    t.request.path = "/echo?api_key=abc".into();
    assert!(store.put(t.clone(), false).is_err());
    t.request.path = "/echo?api_key={{API_KEY}}".into();
    assert!(store.put(t, false).is_ok());
}

#[tokio::test]
async fn captured_bodies_and_encoded_names_lose_their_secrets() {
    let (state, app) = with_orders(Config::for_tests()).await;
    let req = Request::post("/anything/login?to%6Ben=tok-9")
        .header("x-rustybin-session", "bodies")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"user":"ana","password":"hunter2","nested":{"client_secret":"cs-1"}}"#,
        ))
        .expect("req");
    app.clone().oneshot(req).await.expect("resp");
    let id = last_capture(&state, "bodies");
    let (s, v) = call(
        &app,
        "POST",
        LIST_PATH,
        "bodies",
        Some(json!({ "name": "login", "capture": id })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let text = v.to_string();
    for secret in ["hunter2", "cs-1", "tok-9"] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    assert!(
        v["request"]["body"]
            .as_str()
            .unwrap_or("")
            .contains("{{PASSWORD}}"),
        "{v}"
    );
}

#[tokio::test]
async fn saved_tests_are_read_by_an_admin_only() {
    let mut c = Config::for_tests();
    c.admin_token = Some("admin-secret".into());
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    for uri in [LIST_PATH, EXPORT_PATH, "/_rustybin/tests/x"] {
        let resp = app
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).expect("req"))
            .await
            .expect("resp");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
}

fn plain_test(name: &str) -> SavedTest {
    SavedTest {
        name: name.into(),
        description: None,
        request: TestRequest {
            method: "GET".into(),
            path: "/status/200".into(),
            headers: Default::default(),
            body: None,
        },
        gateway: None,
        assert: vec![Assertion::status(200)],
    }
}

#[test]
fn a_save_or_delete_the_file_refuses_changes_nothing() {
    let dir = tempfile::tempdir().expect("tmp");
    let store = Tests::new(Some(dir.path()));
    store.put(plain_test("kept"), false).expect("put");
    // The file's place is taken by a directory: no write can land.
    let file = dir.path().join(FILE);
    std::fs::remove_file(&file).expect("rm");
    std::fs::create_dir(&file).expect("dir");
    match store.put(plain_test("new"), false) {
        Err(StoreError::Io(_)) => {}
        other => panic!("expected an I/O refusal, got {other:?}"),
    }
    assert!(
        store.get("new").is_none(),
        "the store took a change the file did not"
    );
    assert!(matches!(store.delete("kept"), Err(StoreError::Io(_))));
    assert!(
        store.get("kept").is_some(),
        "the delete was kept in memory only"
    );
    assert!(!store.delete("missing").expect("no file needed"));
    assert_eq!(
        StoreError::Io("x".into()).status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn one_run_of_the_saved_tests_at_a_time() {
    let (state, app) = with_orders(Config::for_tests()).await;
    state.saved_tests.put(plain_test("t"), false).expect("put");
    let held = state.saved_tests.begin_run().expect("first run");
    assert!(
        state.saved_tests.begin_run().is_none(),
        "a second run started"
    );
    let (s, v) = call(&app, "POST", "/_rustybin/tests/run", "ops", None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    drop(held);
    let (s, v) = call(&app, "POST", "/_rustybin/tests/run", "ops", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["passed"], true, "{v}");
}
