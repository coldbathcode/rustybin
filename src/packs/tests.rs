use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::test_support::{jwt_config, sign_control_jwt, test_app_with, test_state_with};

const SPEC: &str = r#"{"openapi":"3.0.3","info":{"title":"Orders","version":"1"},"paths":{"/orders/{id}":{"get":{"operationId":"getOrder","parameters":[{"name":"id","in":"path","required":true,"schema":{"type":"integer"}}],"responses":{"200":{"description":"ok","content":{"application/json":{"example":{"id":1,"total":9.5}}}}}}}}}"#;

/// A pack with one of everything a pack may carry.
fn acme() -> Value {
    json!({
        "pack": { "name": "acme", "title": "Acme checkout", "customer": "Acme Corp", "notes": "Thursday, 10:00", "console_title": "Acme demo" },
        "mocks": [{ "name": "acme-orders", "spec_inline": serde_json::from_str::<Value>(SPEC).expect("spec") }],
        "portal": { "items": [{ "id": "mock:acme-orders", "title": "Acme Orders API", "tags": ["acme"] }] },
        "tests": [{ "name": "acme-order-1", "request": { "path": "/mock/acme-orders/orders/1" }, "assert": [{ "status": 200 }] }],
        "test_schedules": [{ "name": "acme-hourly", "every": "1h", "names": ["acme-order-1"] }],
        "storyboards": [{ "name": "acme-demo", "title": "The Acme demo", "steps": [
            { "title": "Welcome", "talk_track": "Hello **Acme**." },
            { "title": "An order", "action": { "type": "run_test", "name": "acme-order-1" } }
        ] }]
    })
}

async fn call_as(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
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
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .expect("body")
        .to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, v)
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    call_as(app, method, uri, None, body).await
}

fn instance() -> (AppState, Router) {
    let state = test_state_with(Config::for_tests());
    let app = test_app_with(state.clone());
    (state, app)
}

fn export_yaml(state: &AppState) -> String {
    crate::configdoc::to_yaml(&crate::configdoc::export(state, SecretsMode::Refs, false).file)
}

async fn apply_pack(app: &Router, doc: Value, dry_run: bool) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        APPLY_PATH,
        Some(json!({ "document": doc, "dry_run": dry_run })),
    )
    .await
}

#[tokio::test]
async fn apply_then_teardown_leaves_the_instance_as_it_was() {
    let (state, app) = instance();
    // Something of the instance's own, which a pack never touches.
    let (s, _) = call(
        &app,
        "POST",
        "/_rustybin/mocks",
        Some(json!({ "name": "orders", "spec": SPEC })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let before = export_yaml(&state);

    // The preview says what it would create and creates nothing.
    let (s, v) = apply_pack(&app, acme(), true).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["would_create"]["mocks"], json!(["acme-orders"]), "{v}");
    assert_eq!(v["would_create"]["test_schedules"], json!(["acme-hourly"]));
    assert_eq!(export_yaml(&state), before);
    assert!(state.packs.is_empty());

    let (s, v) = apply_pack(&app, acme(), false).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["created"]["storyboards"], json!(["acme-demo"]), "{v}");
    // Everything is in the pack's project, deployed and usable.
    let held = crate::rbac::project_api::resources(&state, "acme");
    assert_eq!(held.mocks, vec!["acme-orders"]);
    assert_eq!(held.tests, vec!["acme-order-1"]);
    assert_eq!(held.storyboards, vec!["acme-demo"]);
    assert!(state.portal.published().contains_key("mock:acme-orders"));
    assert!(state.saved_tests.schedules.get("acme-hourly").is_some());
    assert_eq!(state.settings.get("console.title").0, json!("Acme demo"));
    let (s, v) = call(&app, "GET", "/mock/acme-orders/orders/1", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/storyboards/acme-demo/steps/2/run",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["ok"], json!(true), "{v}");
    let (_, v) = call(&app, "GET", LIST_PATH, None).await;
    assert_eq!(v["packs"][0]["name"], json!("acme"), "{v}");
    assert_eq!(v["packs"][0]["customer"], json!("Acme Corp"));
    assert_eq!(v["packs"][0]["holds"]["mocks"], json!(["acme-orders"]));
    // Applied twice: refused, the project's name is taken.
    let (s, v) = apply_pack(&app, acme(), false).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["reason"], json!("name_taken"));

    // A checkpoint a person saved in the project goes with it.
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/checkpoints",
        Some(json!({ "name": "acme-start", "project": "acme" })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, v) = call(&app, "DELETE", "/_rustybin/packs/acme?dry_run=true", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["would_remove"]["checkpoints"],
        json!(["acme-start"]),
        "{v}"
    );
    assert_eq!(v["console_title"], json!("would be restored"));
    assert!(state.mocks.get("acme-orders").is_some());

    let (s, v) = call(&app, "DELETE", "/_rustybin/packs/acme", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(v["removed"]["mocks"], json!(["acme-orders"]));
    assert_eq!(v["removed"]["portal_items"], json!(["mock:acme-orders"]));
    assert_eq!(v["console_title"], json!("restored"));
    // Byte for byte what it was.
    assert_eq!(export_yaml(&state), before);
    assert!(state.packs.is_empty());
    assert!(state.checkpoints.get("acme-start").is_none());
    let (s, _) = call(&app, "DELETE", "/_rustybin/packs/acme", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_taken_name_is_refused_at_the_preview_with_the_name() {
    let (state, app) = instance();
    let (s, _) = call(
        &app,
        "POST",
        "/_rustybin/mocks",
        Some(json!({ "name": "acme-orders", "spec": SPEC })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = call(
        &app,
        "PUT",
        "/_rustybin/tests/acme-order-1",
        Some(json!({ "request": { "path": "/status/200" } })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let before = export_yaml(&state);
    for dry_run in [true, false] {
        let (s, v) = apply_pack(&app, acme(), dry_run).await;
        assert_eq!(s, StatusCode::CONFLICT, "{v}");
        assert_eq!(v["reason"], json!("name_taken"));
        assert_eq!(
            v["collisions"],
            json!([{ "kind": "mock", "name": "acme-orders" }, { "kind": "test", "name": "acme-order-1" }]),
            "{v}"
        );
        assert!(v["error"].as_str().expect("e").contains("mock acme-orders"));
    }
    // Nothing was overwritten or created.
    assert_eq!(export_yaml(&state), before);
    assert!(!state.users.project_exists("acme"));
}

#[tokio::test]
async fn a_pack_carries_only_what_a_demo_needs() {
    let (state, app) = instance();
    let with = |key: &str, v: Value| {
        let mut d = acme();
        d[key] = v;
        d
    };
    for (doc, says) in [
        (
            with("users", json!([{ "name": "mallory", "roles": ["admin"] }])),
            "not users",
        ),
        (with("roles", json!([])), "not roles"),
        (
            with("settings", json!({ "egress.allow": ["attacker.example"] })),
            "not settings",
        ),
        (
            with("secrets", json!({ "assist.api_key": "sk-123" })),
            "not secrets",
        ),
        (with("registry", json!({ "servers": [] })), "not registry"),
        (
            with("projects", json!([{ "name": "other" }])),
            "not projects",
        ),
        (json!({ "storyboards": [] }), "no pack: header"),
        (with("pack", json!({ "name": "instance" })), "pack.name"),
        (with("pack", json!({ "name": "a b" })), "pack.name"),
        (
            with("pack", json!({ "name": "acme", "owner": "x" })),
            "unknown field",
        ),
        // Another project's, a spec on disk, somebody else's mock, every test.
        (
            with(
                "tests",
                json!([{ "name": "t", "project": "payments", "request": { "path": "/get" } }]),
            ),
            "belong to its own project",
        ),
        (
            with(
                "mocks",
                json!([{ "name": "acme-orders", "spec": "/etc/passwd" }]),
            ),
            "specs inline",
        ),
        (
            with("portal", json!({ "items": [{ "id": "mock:petstore" }] })),
            "its own mocks only",
        ),
        (
            with("portal", json!({ "items": [{ "id": "llm" }] })),
            "its own mocks only",
        ),
        (
            with("test_schedules", json!([{ "name": "all", "every": "1h" }])),
            "the pack's own tests",
        ),
        (
            with(
                "test_schedules",
                json!([{ "name": "all", "every": "1h", "names": ["someone-elses"] }]),
            ),
            "the pack's own tests",
        ),
    ] {
        let (s, v) = apply_pack(&app, doc, true).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{says}: {v}");
        assert!(
            v["error"].as_str().expect("error").contains(says),
            "{says}: {v}"
        );
    }
    // The pack as YAML text, and as the body itself.
    let text =
        "pack: {name: tiny}\nstoryboards:\n  - name: tiny-demo\n    steps: [{title: Welcome}]\n";
    let (s, v) = apply_pack(&app, json!(text), true).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let req = Request::builder()
        .method("POST")
        .uri(format!("{APPLY_PATH}?dry_run=true"))
        .header("content-type", "application/yaml")
        .body(Body::from(text))
        .expect("req");
    let resp = app.clone().oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(state.packs.is_empty());
}

#[tokio::test]
async fn a_pack_with_a_problem_is_refused_whole() {
    let (state, app) = instance();
    let before = export_yaml(&state);
    let mut doc = acme();
    // A spec that does not compile, and a storyboard that does not validate.
    doc["mocks"][0]["spec_inline"] =
        json!({ "openapi": "3.0.3", "info": { "title": "x", "version": "1" } });
    doc["storyboards"][0]["steps"][1]["action"] =
        json!({ "type": "start_scenario", "scenario": "nope" });
    for dry_run in [true, false] {
        let (s, v) = apply_pack(&app, doc.clone(), dry_run).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
        assert_eq!(v["reason"], json!("invalid_pack"));
        let problems = v["problems"].to_string();
        assert!(problems.contains("storyboards.acme-demo"), "{v}");
    }
    assert_eq!(export_yaml(&state), before);
    assert!(!state.users.project_exists("acme"));
    // From an untrusted source: no gateway it does not already have.
    let mut doc = acme();
    doc["tests"][0]["gateway"] = json!("https://attacker.example");
    let (s, v) = call(
        &app,
        "POST",
        APPLY_PATH,
        Some(json!({ "document": doc.clone(), "untrusted": true })),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert!(v["problems"].to_string().contains("new gateway"), "{v}");
    assert_eq!(export_yaml(&state), before);
    // The same pack, trusted, applies.
    let (s, v) = apply_pack(&app, doc, false).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
}

#[tokio::test]
async fn an_exported_project_applies_on_another_instance() {
    let (state, app) = instance();
    let (s, v) = apply_pack(&app, acme(), false).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    // A demo built by hand: a project with a mock and a test, no pack.
    let (s, _) = call(
        &app,
        "PUT",
        "/_rustybin/projects/byhand",
        Some(json!({ "description": "Built in the console" })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = call(
        &app,
        "POST",
        "/_rustybin/mocks",
        Some(json!({ "name": "hand-orders", "spec": SPEC, "project": "byhand" })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = call(
        &app,
        "PUT",
        "/_rustybin/tests/hand-1",
        Some(json!({ "project": "byhand", "request": { "path": "/mock/hand-orders/orders/1" }, "assert": [{ "status": 200 }] })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    for (project, mocks, tests) in [
        ("acme", "acme-orders", "acme-order-1"),
        ("byhand", "hand-orders", "hand-1"),
    ] {
        let (s, yaml) = call(
            &app,
            "GET",
            &format!("/_rustybin/packs/{project}/export"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{yaml}");
        let text = yaml.as_str().expect("yaml").to_string();
        assert!(
            text.contains(&format!("pack:\n  name: {project}")),
            "{text}"
        );
        assert!(!text.contains("project:"), "{text}");
        // On a fresh instance: the same resources.
        let (other, other_app) = instance();
        let (s, v) = apply_pack(&other_app, json!(text), false).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        let held = crate::rbac::project_api::resources(&other, project);
        assert_eq!(held.mocks, vec![mocks]);
        assert_eq!(held.tests, vec![tests]);
        let (s, again) = call(
            &other_app,
            "GET",
            &format!("/_rustybin/packs/{project}/export"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(
            again.as_str(),
            Some(text.as_str()),
            "the export round trips"
        );
    }
    let (s, v) = call(
        &app,
        "GET",
        "/_rustybin/packs/acme/export?format=json",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["pack"]["customer"], json!("Acme Corp"), "{v}");
    assert_eq!(v["test_schedules"][0]["name"], json!("acme-hourly"));
    let (s, _) = call(&app, "GET", "/_rustybin/packs/nope/export", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // A project that is not a pack is not torn down as one.
    let (s, _) = call(&app, "DELETE", "/_rustybin/packs/byhand", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(state.mocks.get("hand-orders").is_some());
}

#[tokio::test]
async fn a_teardown_removes_only_what_the_project_still_holds() {
    let (state, app) = instance();
    let (s, v) = apply_pack(&app, acme(), false).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    // A person keeps the test (moved to the instance) and renames the console.
    let (s, v) = call(
        &app,
        "PUT",
        "/_rustybin/tests/acme-order-1",
        Some(json!({ "project": "instance", "request": { "path": "/status/200" } })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = call(
        &app,
        "PATCH",
        "/_rustybin/settings",
        Some(json!({ "values": { "console.title": "Ours now" } })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = call(&app, "DELETE", "/_rustybin/packs/acme", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(v["removed"]["tests"], json!([]), "{v}");
    assert!(v["console_title"].as_str().expect("t").starts_with("kept"));
    assert!(state.saved_tests.get("acme-order-1").is_some());
    assert_eq!(state.settings.get("console.title").0, json!("Ours now"));
    assert!(state.mocks.get("acme-orders").is_none());
    assert!(!state.users.project_exists("acme"));
}

#[tokio::test]
async fn a_pack_needs_the_rights_to_make_a_project() {
    let state = test_state_with(jwt_config());
    let app = test_app_with(state.clone());
    let token = |roles: &[&str], perms: &[&str]| {
        sign_control_jwt(
            &json!({
                "aud": "test-instance",
                "sub": "user-1",
                "exp": chrono::Utc::now().timestamp() + 600,
                "rb_roles": roles,
                "rb_perms": perms,
            }),
            false,
        )
    };
    let body = Some(json!({ "document": acme(), "dry_run": true }));
    // An editor builds mocks and tests but applies no document.
    let editor = token(&["editor"], &[]);
    let (s, v) = call_as(&app, "POST", APPLY_PATH, Some(&editor), body.clone()).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["permission"], json!("config.apply"));
    // With config.apply: the project needs users.manage, the title settings.
    let applier = token(&["editor"], &["config.apply"]);
    let (s, v) = call_as(&app, "POST", APPLY_PATH, Some(&applier), body.clone()).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["reason"], json!("permission_required"));
    assert_eq!(v["permission"], json!("users.manage"));
    let manager = token(&["editor"], &["config.apply", "users.manage"]);
    let (s, v) = call_as(&app, "POST", APPLY_PATH, Some(&manager), body.clone()).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["permission"], json!("settings.write"));
    let mut plain = acme();
    plain["pack"]["console_title"] = Value::Null;
    let (s, v) = call_as(
        &app,
        "POST",
        APPLY_PATH,
        Some(&manager),
        Some(json!({ "document": plain })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    // The list and the export are reads of the configuration.
    let tester = token(&["tester"], &[]);
    let (s, _) = call_as(&app, "GET", LIST_PATH, Some(&tester), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = call_as(&app, "GET", LIST_PATH, Some(&applier), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = call_as(
        &app,
        "DELETE",
        "/_rustybin/packs/acme",
        Some(&applier),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["permission"], json!("users.manage"));
    let (s, v) = call_as(
        &app,
        "DELETE",
        "/_rustybin/packs/acme",
        Some("admin-secret"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(state.packs.is_empty());
}

#[tokio::test]
async fn the_records_are_bounded_and_survive_a_restart() {
    let dir = tempfile::tempdir().expect("dir");
    let mut c = Config::for_tests();
    c.state_dir = Some(dir.path().to_path_buf());
    let state = test_state_with(c.clone());
    let app = test_app_with(state.clone());
    for i in 0..MAX_PACKS {
        let doc = json!({ "pack": { "name": format!("p{i}") }, "storyboards": [{ "name": format!("p{i}-demo"), "steps": [{ "title": "Welcome" }] }] });
        let (s, v) = apply_pack(&app, doc, false).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }
    let doc = json!({ "pack": { "name": "one-more" }, "storyboards": [] });
    let (s, v) = apply_pack(&app, doc, true).await;
    assert_eq!(s, StatusCode::INSUFFICIENT_STORAGE, "{v}");
    // After a restart the packs are still packs, and tear down.
    let again = test_state_with(c);
    assert_eq!(again.packs.len(), MAX_PACKS);
    let app = test_app_with(again.clone());
    let (s, v) = call(&app, "DELETE", "/_rustybin/packs/p3", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["removed"]["storyboards"], json!(["p3-demo"]), "{v}");
    assert_eq!(again.packs.len(), MAX_PACKS - 1);
}
