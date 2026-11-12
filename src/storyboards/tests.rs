use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::test_support::{jwt_config, sign_control_jwt, test_app_with, test_state_with};

const SPEC: &str = r#"{"openapi":"3.0.3","info":{"title":"Orders","version":"1"},"paths":{"/orders/{id}":{"get":{"operationId":"getOrder","parameters":[{"name":"id","in":"path","required":true,"schema":{"type":"integer"}}],"responses":{"200":{"description":"ok","content":{"application/json":{"example":{"id":1,"total":9.5}}}}}}}}}"#;

async fn call_as(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-rustybin-session", "presenter");
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    call_as(app, method, uri, None, body).await
}

async fn with_orders(c: Config) -> (AppState, Router) {
    let admin = c.admin_token.clone();
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    let (s, v) = call_as(
        &app,
        "POST",
        "/_rustybin/mocks",
        admin.as_deref(),
        Some(json!({ "name": "orders", "spec": SPEC })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    (state, app)
}

fn step(title: &str, action: Value) -> Value {
    json!({ "title": title, "action": action })
}

fn parse(v: Value) -> Result<Storyboard, String> {
    serde_json::from_value::<Storyboard>(v)
        .map_err(|e| e.to_string())
        .and_then(|s| s.validate().map(|_| s))
}

#[test]
fn a_storyboard_is_validated_step_by_step() {
    let ok = parse(json!({
        "name": "retry-demo",
        "title": "Retries",
        "steps": [
            { "title": "Say hello", "talk_track": "**Welcome.**" },
            step("Start clean", json!({ "type": "restore_checkpoint", "name": "demo-start" })),
            step("Break it", json!({ "type": "set_health", "healthy": false })),
            step("Call it", json!({ "type": "send", "request": { "path": "/health", "headers": { "authorization": "Bearer {{TOKEN}}" } }, "assert": [{ "status": 503 }] })),
            step("Look", json!({ "type": "open_view", "route": "#/requests" })),
            { "title": "Prove it", "action": { "type": "check_scenario", "scenario": "retries", "gateway_url": "https://gateway.example.com/flaky", "params": {} }, "reset": { "type": "set_health", "healthy": true } },
        ]
    }))
    .expect("valid");
    assert_eq!(ok.variables(), vec!["TOKEN".to_string()]);
    assert_eq!(
        ok.outbound().into_iter().collect::<Vec<_>>(),
        vec!["https://gateway.example.com".to_string()]
    );
    // A talking point is the default, and is left out of the document.
    let text = serde_json::to_string(&ok).expect("json");
    assert!(!text.contains("\"none\""), "{text}");
    let back: Storyboard = serde_json::from_str(&text).expect("back");
    assert_eq!(back, ok);

    for (what, bad) in [
        ("name", json!({ "name": "no spaces" })),
        (
            "steps[1].title",
            json!({ "name": "a", "steps": [{ "title": " " }] }),
        ),
        (
            "unknown variant",
            json!({ "name": "a", "steps": [step("x", json!({ "type": "run_shell" }))] }),
        ),
        (
            "unknown field",
            json!({ "name": "a", "steps": [step("x", json!({ "type": "set_health", "healthy": true, "also": 1 }))] }),
        ),
        (
            "scenario",
            json!({ "name": "a", "steps": [step("x", json!({ "type": "start_scenario", "scenario": "nope" }))] }),
        ),
        (
            "route",
            json!({ "name": "a", "steps": [step("x", json!({ "type": "open_view", "route": "https://elsewhere.example" }))] }),
        ),
        (
            "control-plane",
            json!({ "name": "a", "steps": [step("x", json!({ "type": "send", "request": { "path": "/_rustybin/settings" } }))] }),
        ),
        // A credential is never saved: a variable, never a value.
        (
            "credential",
            json!({ "name": "a", "steps": [step("x", json!({ "type": "send", "request": { "path": "/get", "headers": { "authorization": "Bearer abc" } } }))] }),
        ),
        (
            "steps[1].reset",
            json!({ "name": "a", "steps": [{ "title": "x", "reset": { "type": "restore_checkpoint", "name": "../x" } }] }),
        ),
        (
            "talk_track",
            json!({ "name": "a", "steps": [{ "title": "x", "talk_track": "x".repeat(MAX_TALK_TRACK + 1) }] }),
        ),
    ] {
        let e = parse(bad).expect_err(what);
        assert!(e.contains(what), "{what}: {e}");
    }
    let many: Vec<Value> = (0..=MAX_STEPS)
        .map(|i| json!({ "title": format!("s{i}") }))
        .collect();
    let e = parse(json!({ "name": "a", "steps": many })).expect_err("too many");
    assert!(e.contains("at most 40"), "{e}");
}

#[test]
fn the_store_is_bounded_and_survives_a_restart() {
    let dir = tempfile::tempdir().expect("dir");
    let store = Storyboards::new(Some(dir.path()));
    let sb = |name: &str| Storyboard {
        name: name.into(),
        title: Some("T".into()),
        description: None,
        steps: vec![Step {
            title: "one".into(),
            talk_track: Some("say this".into()),
            action: Action::SetHealth { healthy: false },
            reset: Some(Action::SetHealth { healthy: true }),
        }],
        project: None,
    };
    for i in 0..MAX_STORYBOARDS {
        store.put(sb(&format!("s{i}")), false).expect("put");
    }
    assert!(matches!(
        store.put(sb("more"), false),
        Err(StoreError::Full)
    ));
    assert!(matches!(
        store.put(sb("s0"), false),
        Err(StoreError::Exists(_))
    ));
    store.put(sb("s0"), true).expect("replace");
    assert!(store.delete("s1").expect("delete"));
    assert!(!store.delete("s1").expect("gone"));
    let again = Storyboards::new(Some(dir.path()));
    assert_eq!(again.len(), MAX_STORYBOARDS - 1);
    assert_eq!(again.get("s0"), Some(sb("s0")));
}

#[test]
fn progress_is_one_per_session_and_bounded() {
    let runs = run::Runs::default();
    let result = |step: usize| run::StepResult {
        step,
        title: "t".into(),
        action: "none",
        ok: true,
        summary: "a talking point".into(),
        detail: None,
        ran_at: "2026-11-12T00:00:00Z".into(),
    };
    for i in 0..run::MAX_RUNS + 10 {
        runs.record_for_tests(&format!("s{i}"), "a", "t1", result(1));
    }
    assert_eq!(runs.len(), run::MAX_RUNS);
    // A step of another storyboard, or of a newer save, starts over.
    runs.record_for_tests("me", "a", "t1", result(1));
    runs.record_for_tests("me", "a", "t1", result(2));
    assert_eq!(runs.view("me", "a", "t1", 3)["current"], json!(3));
    assert_eq!(runs.view("me", "a", "t2", 3)["current"], json!(1));
    runs.record_for_tests("me", "b", "t1", result(1));
    assert_eq!(
        runs.view("me", "a", "t1", 3)["results"],
        json!([]),
        "one storyboard a session"
    );
    assert_eq!(runs.view("me", "b", "t1", 1)["done"], json!(true));
}

#[tokio::test]
async fn the_steps_of_a_storyboard_run_with_their_verdicts() {
    let (state, app) = with_orders(Config::for_tests()).await;
    let (s, v) = call(
        &app,
        "PUT",
        "/_rustybin/tests/order-1",
        Some(json!({ "request": { "path": "/mock/orders/orders/1" }, "assert": [{ "status": 200 }, { "json": "$.id", "equals": 1 }] })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let body = json!({
        "name": "orders-demo",
        "title": "Orders",
        "steps": [
            { "title": "The mock", "talk_track": "Here is the **orders** API.", "action": { "type": "open_view", "route": "#/mocks/orders" } },
            step("Call it", json!({ "type": "send", "request": { "path": "/mock/orders/orders/1" }, "assert": [{ "status": 200 }] })),
            step("The saved test", json!({ "type": "run_test", "name": "order-1" })),
            step("Break the upstream", json!({ "type": "set_health", "healthy": false })),
            step("It is down", json!({ "type": "send", "request": { "path": "/health" }, "assert": [{ "status": 200 }] })),
            step("Generated answers", json!({ "type": "set_mock_mode", "mock": "orders", "mode": "generated" })),
            step("A checkpoint nobody saved", json!({ "type": "restore_checkpoint", "name": "never-saved" })),
            { "title": "Any questions?" },
        ]
    });
    let (s, v) = call(&app, "POST", "/_rustybin/storyboards", Some(body.clone())).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert!(v["saved_at"].is_string());
    let (s, v) = call(&app, "POST", "/_rustybin/storyboards", Some(body)).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let (_, v) = call(&app, "GET", "/_rustybin/storyboards", None).await;
    assert_eq!(v["storyboards"][0]["name"], json!("orders-demo"));
    assert_eq!(v["max_steps"], json!(MAX_STEPS));

    let run = |n: usize| {
        let app = app.clone();
        async move {
            call(
                &app,
                "POST",
                &format!("/_rustybin/storyboards/orders-demo/steps/{n}/run"),
                Some(json!({})),
            )
            .await
        }
    };
    let expected = [
        (true, "open_view", "#/mocks/orders"),
        (true, "send", "passed"),
        (true, "run_test", "passed"),
        (true, "set_health", "503"),
        // The assertion wanted 200: the step's verdict, not an error status.
        (false, "send", "expected 200, saw 503"),
        (true, "set_mock_mode", "generated"),
        (false, "restore_checkpoint", "no checkpoint named"),
        (true, "none", "talking point"),
    ];
    for (i, (ok, action, says)) in expected.iter().enumerate() {
        let (s, v) = run(i + 1).await;
        assert_eq!(s, StatusCode::OK, "step {}: {v}", i + 1);
        assert_eq!(v["ok"], json!(ok), "step {}: {v}", i + 1);
        assert_eq!(v["action"], json!(action), "{v}");
        assert!(
            v["summary"].as_str().expect("summary").contains(says),
            "step {}: {v}",
            i + 1
        );
        let next = if i + 1 < expected.len() {
            json!(i + 2)
        } else {
            Value::Null
        };
        assert_eq!(v["next"], next, "{v}");
    }
    assert!(!state.health.is_healthy());
    let draft = state
        .mocks
        .get("orders")
        .expect("mock")
        .draft
        .settings
        .clone();
    assert_eq!(draft.responses.mode, ResponseMode::Generated);
    // The saved test's run is in the history, named after the storyboard.
    let (_, runs) = call(&app, "GET", "/_rustybin/tests/runs", None).await;
    assert_eq!(runs["runs"][0]["trigger"], json!("storyboard:orders-demo"));

    // The presenter's progress: each step's last result, where they are.
    let (s, v) = call(&app, "GET", "/_rustybin/storyboards/orders-demo", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["progress"]["done"], json!(true), "{v}");
    assert_eq!(v["progress"]["results"].as_array().map(Vec::len), Some(8));
    assert_eq!(v["progress"]["results"][4]["ok"], json!(false));
    // Another session starts at step 1.
    let req = Request::builder()
        .uri("/_rustybin/storyboards/orders-demo")
        .header("x-rustybin-session", "someone-else")
        .body(Body::empty())
        .expect("req");
    let resp = app.clone().oneshot(req).await.expect("resp");
    let v = crate::test_support::body_json(resp).await;
    assert_eq!(v["progress"]["current"], json!(1));
    assert_eq!(v["progress"]["results"], json!([]));

    // No such step, storyboard or number.
    let (s, v) = run(9).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    let (s, _) = run(0).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app,
        "POST",
        "/_rustybin/storyboards/orders-demo/steps/x/run",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(
        &app,
        "POST",
        "/_rustybin/storyboards/nope/steps/1/run",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // A replaced storyboard starts over; a deleted one is gone.
    let (s, v) = call(
        &app,
        "PUT",
        "/_rustybin/storyboards/orders-demo",
        Some(json!({ "steps": [{ "title": "Only this" }] })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = call(&app, "DELETE", "/_rustybin/storyboards/orders-demo", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = call(&app, "GET", "/_rustybin/storyboards/orders-demo", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

fn token(roles: &[&str]) -> String {
    sign_control_jwt(
        &json!({
            "aud": "test-instance",
            "sub": "user-1",
            "exp": chrono::Utc::now().timestamp() + 600,
            "rb_roles": roles,
        }),
        false,
    )
}

#[tokio::test]
async fn a_step_needs_the_permission_of_its_action_at_run_time() {
    let (state, app) = with_orders(jwt_config()).await;
    let admin = Some("admin-secret");
    let (s, v) = call_as(
        &app,
        "POST",
        "/_rustybin/checkpoints",
        admin,
        Some(json!({ "name": "start" })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, v) = call_as(
        &app,
        "POST",
        "/_rustybin/storyboards",
        admin,
        Some(json!({
            "name": "demo",
            "steps": [
                step("Call it", json!({ "type": "send", "request": { "path": "/mock/orders/orders/1" }, "assert": [{ "status": 200 }] })),
                step("Break it", json!({ "type": "set_health", "healthy": false })),
                { "title": "Back", "action": { "type": "restore_checkpoint", "name": "start" }, "reset": { "type": "restore_checkpoint", "name": "start" } },
                step("Mode", json!({ "type": "set_mock_mode", "mock": "orders", "mode": "static" })),
            ]
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    // A tester runs tests and nothing else: the step it may run runs, the
    // others are refused naming the permission, and nothing changed.
    let tester = token(&["tester"]);
    let run = |n: usize, t: String| {
        let app = app.clone();
        async move {
            call_as(
                &app,
                "POST",
                &format!("/_rustybin/storyboards/demo/steps/{n}/run"),
                Some(&t),
                None,
            )
            .await
        }
    };
    let (s, v) = run(1, tester.clone()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["ok"], json!(true), "{v}");
    for (n, permission) in [
        (2, "settings.write"),
        (3, "checkpoints.restore"),
        (4, "mocks.write"),
    ] {
        let (s, v) = run(n, tester.clone()).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "step {n}: {v}");
        assert_eq!(v["reason"], json!("permission_required"), "{v}");
        assert_eq!(v["permission"], json!(permission), "{v}");
    }
    assert!(state.health.is_healthy());
    let (s, v) = call_as(
        &app,
        "POST",
        "/_rustybin/storyboards/demo/reset",
        Some(&tester),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["permission"], json!("checkpoints.restore"), "{v}");
    // A viewer reads it and runs nothing; writing needs tests.write.
    let viewer = token(&["viewer"]);
    let (s, _) = call_as(
        &app,
        "GET",
        "/_rustybin/storyboards/demo",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = run(1, viewer.clone()).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["permission"], json!("tests.run"), "{v}");
    let (s, v) = call_as(
        &app,
        "DELETE",
        "/_rustybin/storyboards/demo",
        Some(&tester),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["permission"], json!("tests.write"), "{v}");
    // The admin runs every step.
    for n in 2..=4 {
        let (s, v) = run(n, "admin-secret".to_string()).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["ok"], json!(true), "step {n}: {v}");
    }
}

#[tokio::test]
async fn reset_runs_each_steps_reset_and_clears_the_progress() {
    let (state, app) = with_orders(Config::for_tests()).await;
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/checkpoints",
        Some(json!({ "name": "demo-start" })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/storyboards",
        Some(json!({
            "name": "demo",
            "steps": [
                { "title": "Start", "reset": { "type": "restore_checkpoint", "name": "demo-start" } },
                { "title": "Break it", "action": { "type": "set_health", "healthy": false }, "reset": { "type": "set_health", "healthy": true } },
                step("A scenario", json!({ "type": "start_scenario", "scenario": "caching" })),
                step("Check before a gateway exists", json!({ "type": "check_scenario", "scenario": "retries", "gateway_url": "https://gateway.example.com/x" })),
            ]
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    // The demo drifts: another mock, the health toggle, a scenario run.
    let (s, _) = call(
        &app,
        "POST",
        "/_rustybin/mocks",
        Some(json!({ "name": "extra", "spec": SPEC })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    for n in [2, 3, 3] {
        let (s, v) = call(
            &app,
            "POST",
            &format!("/_rustybin/storyboards/demo/steps/{n}/run"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["ok"], json!(true), "{v}");
    }
    // The scenario step ran twice: one run, not two.
    assert_eq!(state.scenarios.len(), 1);
    let (_, v) = call(
        &app,
        "POST",
        "/_rustybin/storyboards/demo/steps/4/run",
        None,
    )
    .await;
    assert_eq!(v["ok"], json!(false), "{v}");
    assert!(
        v["summary"]
            .as_str()
            .expect("s")
            .contains("start_scenario step comes first"),
        "{v}"
    );
    assert!(!state.health.is_healthy());

    let (s, v) = call(&app, "POST", "/_rustybin/storyboards/demo/reset", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["ok"], json!(true), "{v}");
    let resets = v["reset"].as_array().expect("reset");
    assert_eq!(resets.len(), 2, "{v}");
    assert_eq!(resets[0]["action"], json!("restore_checkpoint"));
    assert_eq!(v["ended_scenarios"][0]["scenario"], json!("caching"), "{v}");
    // The checkpoint of the first step's reset is back, health is up, the
    // run is ended and the presenter is at step 1 again.
    assert!(state.mocks.get("extra").is_none());
    assert!(state.mocks.get("orders").is_some());
    assert!(state.health.is_healthy());
    assert!(state.scenarios.is_empty());
    let (_, v) = call(&app, "GET", "/_rustybin/storyboards/demo", None).await;
    assert_eq!(
        v["progress"],
        json!({ "current": 1, "done": false, "results": [] })
    );
    assert!(state.storyboards.runs.is_empty());
}

#[test]
fn a_document_carries_storyboards_and_one_from_git_is_untrusted() {
    use crate::configdoc::{apply, export, to_yaml, ApplyOptions, SecretsMode};
    let state = test_state_with(Config::for_tests());
    let sb = parse(json!({
        "name": "retry-demo",
        "title": "Retries",
        "description": "Break the upstream, show the retry policy, restore",
        "steps": [
            { "title": "Start clean", "talk_track": "We start from a **known state**.", "action": { "type": "restore_checkpoint", "name": "demo-start" } },
            { "title": "Break it", "action": { "type": "set_health", "healthy": false }, "reset": { "type": "set_health", "healthy": true } },
            step("Through the gateway", json!({ "type": "send", "gateway": "https://gateway.example.com", "request": { "method": "POST", "path": "/orders", "headers": { "x-api-key": "{{GATEWAY_KEY}}" }, "body": "{}" }, "assert": [{ "status": 201 }] })),
        ]
    }))
    .expect("valid");
    state.storyboards.put(sb.clone(), false).expect("put");
    let text = to_yaml(&export(&state, SecretsMode::Refs, false).file);
    assert!(text.contains("storyboards:\n- name: retry-demo"), "{text}");
    assert!(text.contains("type: restore_checkpoint"), "{text}");

    // Applied on another instance, every field is there.
    let other = test_state_with(Config::for_tests());
    let options = |untrusted: bool| ApplyOptions {
        prune: true,
        dry_run: false,
        principal: "test".into(),
        base: None,
        untrusted,
        caller: None,
        base_url: None,
    };
    let opts = options(false);
    let doc = crate::mocks::config_file::parse(&text).expect("parse");
    let r = apply(&other, doc, &opts);
    assert_eq!(
        r["results"]["storyboards"]["saved"],
        json!(["retry-demo"]),
        "{r}"
    );
    assert_eq!(other.storyboards.get("retry-demo"), Some(sb.clone()));

    // From the repository: a step never gains a gateway, a storyboard is
    // never moved, and one that is invalid is skipped and kept.
    let git = options(true);
    let third = test_state_with(Config::for_tests());
    let r = apply(
        &third,
        crate::mocks::config_file::parse(&text).expect("parse"),
        &git,
    );
    assert!(r["problems"].to_string().contains("storyboards.retry-demo: a document from the repository cannot point a step at a new gateway"), "{r}");
    assert!(third.storyboards.is_empty());
    // The same gateway its saved copy names is fine, with a new talk track.
    let edited = text.replace("We start from", "Today we start from");
    let r = apply(
        &other,
        crate::mocks::config_file::parse(&edited).expect("parse"),
        &git,
    );
    assert_eq!(
        r["results"]["storyboards"]["saved"],
        json!(["retry-demo"]),
        "{r}"
    );
    let got = other.storyboards.get("retry-demo").expect("kept");
    assert!(got.steps[0]
        .talk_track
        .as_deref()
        .expect("t")
        .starts_with("Today"));
    // A prune without the section leaves them alone; with an empty one,
    // they go.
    let r = apply(
        &other,
        crate::mocks::config_file::parse("version: 2\n").expect("p"),
        &opts,
    );
    assert!(r["results"]["storyboards"].is_null(), "{r}");
    assert_eq!(other.storyboards.len(), 1);
    let r = apply(
        &other,
        crate::mocks::config_file::parse("version: 2\nstoryboards: []\n").expect("p"),
        &opts,
    );
    assert_eq!(
        r["results"]["storyboards"]["deleted"],
        json!(["retry-demo"]),
        "{r}"
    );
}
