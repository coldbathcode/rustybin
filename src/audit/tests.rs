//! The audit log (testing-2 step 14): one entry per mutating route and
//! tool, nothing secret in an entry or a file, rotation and retention on a
//! manual clock, the filters and the cursor, the stream, sign-ins and
//! refusals.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use chrono::{TimeZone, Utc};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::control_auth::ControlAuth;
use crate::limits::ManualClock;
use crate::test_support::{body_json, control_jwt, jwt_config, test_app_with, test_state_with};

const ADMIN: &str = "audit-admin-token";
const SPEC: &str = r#"{"openapi":"3.0.3","info":{"title":"T","version":"1"},"paths":{"/a":{"get":{"operationId":"a","responses":{"200":{"description":"ok","content":{"application/json":{"schema":{"type":"object","properties":{"n":{"type":"integer"}}}}}}}}}}}"#;

fn password_config() -> Config {
    let mut c = Config::for_tests();
    c.control_auth = ControlAuth::Password;
    c.admin_token = Some(ADMIN.into());
    c
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
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
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22)
        .await
        .unwrap_or_default();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()));
    (status, v)
}

fn last(state: &AppState) -> Arc<Entry> {
    state.audit.all().last().cloned().expect("an entry")
}

/// A concrete path for a route template (parameters as `zz1`).
fn concrete(template: &str) -> String {
    template
        .split('/')
        .map(|s| if s.starts_with('{') { "zz1" } else { s })
        .collect::<Vec<_>>()
        .join("/")
}

fn draft(kind: &str, name: &str, action: &str, status: u16) -> Draft {
    Draft {
        actor: Actor::user(&format!("u_{name}"), name),
        via: "api",
        action: action.to_string(),
        targets: vec![Target {
            kind: kind.into(),
            name: format!("{kind}-{name}"),
        }],
        result: Outcome::of_status(status),
        ..Draft::default()
    }
}

fn mcp_body(name: &str, args: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": name, "arguments": args,
        "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {} }
    } })
}

async fn tool(app: &Router, headers: &[(&str, &str)], name: &str, args: Value) -> Value {
    let mut h = vec![
        ("accept", "application/json, text/event-stream"),
        ("mcp-protocol-version", "2026-07-28"),
        ("mcp-method", "tools/call"),
        ("mcp-name", name),
    ];
    h.extend_from_slice(headers);
    let (status, v) = call(
        app,
        "POST",
        "/_rustybin/mcp",
        &h,
        Some(mcp_body(name, args)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{name}: {v}");
    v["result"].clone()
}

#[tokio::test]
async fn every_mutating_route_and_tool_writes_one_entry() {
    let state = test_state_with(Config::for_tests());
    let app = test_app_with(state.clone());
    let mut missing = Vec::new();
    let mut walked = 0;
    for ep in crate::catalog::all().iter().filter(|e| e.is_control()) {
        for m in ep.expanded_methods() {
            let method: Method = m.parse().expect("method");
            if !is_mutating(&method) {
                continue;
            }
            walked += 1;
            let before = state.audit.len();
            let req = Request::builder()
                .method(m)
                .uri(concrete(ep.path))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .expect("request");
            let resp = tokio::time::timeout(Duration::from_secs(10), app.clone().oneshot(req))
                .await
                .unwrap_or_else(|_| panic!("{m} {} did not answer", ep.path))
                .expect("response");
            drop(resp);
            let added: Vec<Arc<Entry>> = state.audit.all().into_iter().skip(before).collect();
            let want = if not_recorded(m, ep.path) { 0 } else { 1 };
            let action = format!("{m} {}", ep.path);
            if added.len() != want || (want == 1 && added[0].action != action) {
                missing.push(format!(
                    "{action}: {} entr(ies) {:?}",
                    added.len(),
                    added.iter().map(|e| e.action.clone()).collect::<Vec<_>>()
                ));
            }
        }
    }
    assert!(walked > 100, "too few routes walked: {walked}");
    assert!(missing.is_empty(), "routes:\n{}", missing.join("\n"));
    // Every NOT_RECORDED row is a route that exists.
    for (m, t, _) in NOT_RECORDED {
        let ep = crate::catalog::control_route(t).unwrap_or_else(|| panic!("{t}: no such route"));
        assert!(*m == "*" || ep.expanded_methods().contains(m), "{m} {t}");
    }

    // Every management tool that changes something: one entry, via mcp.
    let mut missing = Vec::new();
    for def in crate::mcp::manage::tools::TOOLS
        .iter()
        .filter(|t| !t.read_only)
    {
        let before = state.audit.len();
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            tool(&app, &[], def.name, json!({})),
        )
        .await
        .unwrap_or_else(|_| panic!("{} did not answer", def.name));
        let added: Vec<Arc<Entry>> = state.audit.all().into_iter().skip(before).collect();
        let want = 1;
        let mine = added
            .iter()
            .filter(|e| e.via == "mcp" && e.action == def.name)
            .count();
        if added.len() != want || mine != want {
            missing.push(format!(
                "{}: {} entr(ies) {:?}",
                def.name,
                added.len(),
                added.iter().map(|e| e.action.clone()).collect::<Vec<_>>()
            ));
        }
    }
    assert!(missing.is_empty(), "tools:\n{}", missing.join("\n"));
}

#[tokio::test]
async fn nothing_secret_reaches_an_entry_or_a_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut c = password_config();
    c.admin_token = Some("SECRET-admin-token-1".into());
    c.state_dir = Some(dir.path().to_path_buf());
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    let admin = "Bearer SECRET-admin-token-1";
    // A settings change with a secret value.
    let (status, _) = call(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &[("authorization", admin), ("x-custom", "SECRET-header-2")],
        Some(json!({ "values": { "console.title": "SECRET-title-3" }, "secrets": { "notify.webhook_token": "SECRET-token-4" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Sign-ins: a wrong password for a user who exists, a name nobody has.
    let (status, _) = call(
        &app,
        "POST",
        "/_rustybin/login",
        &[],
        Some(json!({ "username": "rustybin", "password": "SECRET-password-5" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let failed = last(&state);
    assert_eq!(failed.event.as_deref(), Some("login"));
    assert_eq!(failed.actor.kind, "user");
    assert_eq!(failed.actor.name.as_deref(), Some("rustybin"));
    assert_eq!(failed.result.reason.as_deref(), Some("invalid_credentials"));
    let (status, _) = call(
        &app,
        "POST",
        "/_rustybin/login",
        &[],
        Some(json!({ "username": "SECRET-ghost-6", "password": "SECRET-password-7" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let ghost = last(&state);
    assert_eq!(ghost.actor.kind, "anonymous");
    assert_eq!(ghost.actor.name, None);
    // A good sign-in, then its token on a change with a query and a body.
    let (status, signed) = call(
        &app,
        "POST",
        "/_rustybin/login",
        &[],
        Some(json!({ "username": "rustybin", "password": "rustybin" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let token = signed["token"].as_str().expect("token").to_string();
    let bearer = format!("Bearer {token}");
    let spec = SPEC.replace("\"T\"", "\"SECRET-spec-8\"");
    let (status, v) = call(
        &app,
        "POST",
        "/_rustybin/mocks?token=SECRET-query-9",
        &[
            ("authorization", &bearer),
            ("x-rustybin-key", "SECRET-key-10"),
        ],
        Some(json!({ "name": "orders", "spec": spec })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let created = last(&state);
    assert_eq!(created.actor.name.as_deref(), Some("rustybin"));
    assert!(created.targets.contains(&Target {
        kind: "mock".into(),
        name: "orders".into()
    }));
    // A tool call with a spec in its arguments.
    let r = tool(
        &app,
        &[("authorization", &bearer)],
        "create_mock",
        json!({ "name": "orders2", "spec": SPEC.replace("\"T\"", "\"SECRET-tool-11\"") }),
    )
    .await;
    assert_ne!(r["isError"], json!(true), "{r}");
    // A secret set at an agent's request (core next 12): the reason the
    // agent gave and the value the person typed are in no entry.
    state
        .settings
        .update(
            BTreeMap::from([(crate::secret_requests::SWITCH_KEY.to_string(), json!(true))]),
            BTreeMap::new(),
            "test",
        )
        .expect("switch");
    let r = tool(
        &app,
        &[("authorization", &bearer)],
        "request_secret",
        json!({ "setting": "git.token", "reason": "SECRET-reason-12 for the repository" }),
    )
    .await;
    assert_ne!(r["isError"], json!(true), "{r}");
    let request = r["structuredContent"]["request"]["id"]
        .as_str()
        .expect("id")
        .to_string();
    let asked = last(&state);
    assert_eq!(
        (asked.via.to_string().as_str(), asked.action.as_str()),
        ("mcp", "request_secret")
    );
    assert_eq!(asked.event.as_deref(), Some("secret_request"));
    for (kind, name) in [("setting", "git.token"), ("secret-request", &request)] {
        assert!(
            asked.targets.contains(&Target {
                kind: kind.into(),
                name: name.into()
            }),
            "{kind}: {:?}",
            asked.targets
        );
    }
    let (status, v) = call(
        &app,
        "POST",
        &format!("/_rustybin/secret-requests/{request}/fulfil"),
        &[("authorization", admin)],
        Some(json!({ "value": "SECRET-value-13" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        state
            .settings
            .secret("git.token")
            .as_deref()
            .map(String::as_str),
        Some("SECRET-value-13")
    );
    let fulfilled = last(&state);
    assert_eq!(fulfilled.event.as_deref(), Some("secret_fulfilled"));
    assert_eq!(fulfilled.actor.kind, "admin_token");
    assert_eq!(
        fulfilled
            .targets
            .iter()
            .map(|t| t.kind.as_str())
            .collect::<Vec<_>>(),
        ["secret-request", "setting", "agent"]
    );
    // A refused one (a body that is not the shape) quotes nothing either.
    let (status, v) = call(
        &app,
        "POST",
        &format!("/_rustybin/secret-requests/{request}/decline"),
        &[("authorization", admin)],
        Some(json!({ "value": "SECRET-value-14" })),
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{v}");
    assert!(!v.to_string().contains("SECRET"), "{v}");
    // A note on a decline (core next 17): a person who types the secret
    // there by mistake leaves it in no entry.
    let r = tool(
        &app,
        &[("authorization", &bearer)],
        "request_secret",
        json!({ "setting": "notify.webhook_token", "reason": "the schedule's notices" }),
    )
    .await;
    assert_ne!(r["isError"], json!(true), "{r}");
    let request = r["structuredContent"]["request"]["id"]
        .as_str()
        .expect("id")
        .to_string();
    let (status, v) = call(
        &app,
        "POST",
        &format!("/_rustybin/secret-requests/{request}/decline"),
        &[("authorization", admin)],
        Some(json!({ "note": "SECRET-note-15 typed in the wrong box" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["note"], json!("SECRET-note-15 typed in the wrong box"));
    let declined = last(&state);
    assert_eq!(declined.event.as_deref(), Some("secret_declined"));
    assert!(declined.targets.contains(&Target {
        kind: "secret-request".into(),
        name: request.clone()
    }));
    state.audit.flush();

    let ring = serde_json::to_string(&state.audit.all()).expect("json");
    let mut files = String::new();
    for f in std::fs::read_dir(dir.path().join("audit")).expect("dir") {
        files.push_str(&std::fs::read_to_string(f.expect("entry").path()).expect("read"));
    }
    assert!(files.lines().count() >= 5, "{files}");
    for (what, text) in [("ring", &ring), ("files", &files)] {
        assert!(!text.contains("SECRET"), "{what} holds a secret: {text}");
        assert!(!text.contains(&token), "{what} holds the token");
        assert!(!text.contains("rustybin\",\"password"), "{what}");
    }
    // The setting's key is named, never its value.
    assert!(ring.contains("notify.webhook_token") && ring.contains("console.title"));
}

#[tokio::test]
async fn rotation_and_retention_follow_the_clock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = Arc::new(Settings::new(&Config::for_tests()));
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0)
            .single()
            .expect("time"),
    ));
    let audit = Audit::new(Some(dir.path()), settings.clone(), clock.clone());
    let audit_dir = dir.path().join("audit");
    let names = || {
        let mut v: Vec<String> = std::fs::read_dir(&audit_dir)
            .expect("dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().to_string())
            .collect();
        v.sort();
        v
    };
    let set = |k: &str, v: Value| {
        settings
            .update(
                BTreeMap::from([(k.to_string(), v)]),
                BTreeMap::new(),
                "test",
            )
            .expect("setting");
    };
    audit.record(draft("mock", "a", "POST /_rustybin/mocks", 201));
    clock.advance(Duration::from_secs(86_400));
    audit.record(draft("mock", "b", "POST /_rustybin/mocks", 201));
    audit.flush();
    assert_eq!(
        names(),
        ["audit-2026-11-01.jsonl", "audit-2026-11-02.jsonl"]
    );
    // One day kept: the first entry of 3 November deletes the others.
    set(RETENTION_KEY, json!(1));
    clock.advance(Duration::from_secs(86_400));
    audit.record(draft("mock", "c", "POST /_rustybin/mocks", 201));
    audit.flush();
    assert_eq!(names(), ["audit-2026-11-03.jsonl"]);
    assert_eq!(audit.status()["oldest_day"], json!("2026-11-03"));
    // The byte cap: the oldest day goes first, then today's file stops at it.
    set(RETENTION_KEY, json!(30));
    set(MAX_BYTES_KEY, json!(MIN_MAX_BYTES));
    clock.advance(Duration::from_secs(86_400));
    let long = "n".repeat(MAX_NAME);
    for i in 0..400 {
        let mut d = draft("mock", &format!("{i}"), "PUT /_rustybin/mocks/{name}", 200);
        for k in 0..4 {
            d.targets.push(Target {
                kind: format!("k{k}"),
                name: long.clone(),
            });
        }
        audit.record(d);
    }
    audit.flush();
    assert_eq!(names(), ["audit-2026-11-04.jsonl"]);
    let st = audit.status();
    assert!(
        st["bytes"].as_u64().expect("bytes") <= MIN_MAX_BYTES as u64,
        "{st}"
    );
    assert!(st["not_written"].as_u64().expect("n") > 0, "{st}");
    // Every entry is still in memory, and a restart reads the file back.
    assert_eq!(audit.len(), 403);
    drop(audit);
    let again = Audit::new(Some(dir.path()), settings, clock.clone());
    assert!(!again.is_empty() && again.len() < 403);
    let written = again.all().last().expect("entry").id;
    let next = again
        .record(draft("mock", "z", "POST /_rustybin/mocks", 201))
        .expect("entry");
    assert_eq!(next.id, written + 1, "ids continue after the files' newest");
}

#[tokio::test]
async fn filters_and_the_cursor() {
    let state = test_state_with(Config::for_tests());
    for i in 0..30 {
        let (kind, name) = if i % 3 == 0 {
            ("user", "alice")
        } else {
            ("mock", "bob")
        };
        state.audit.record(draft(
            kind,
            name,
            if i % 2 == 0 {
                "POST /_rustybin/mocks/{name}/deploy"
            } else {
                "deploy_mock"
            },
            if i % 5 == 0 { 403 } else { 200 },
        ));
    }
    let now = Utc::now();
    let f = |q: FilterQuery| Filter::parse(&q, now).expect("filter");
    let count = |q: FilterQuery| {
        let mut f = f(q);
        f.limit = MAX_LIMIT;
        state.audit.list(&f).0.len()
    };
    assert_eq!(count(FilterQuery::default()), 30);
    assert_eq!(
        count(FilterQuery {
            actor: Some("ALICE".into()),
            ..Default::default()
        }),
        10
    );
    assert_eq!(
        count(FilterQuery {
            target: Some("user:alice".into()),
            ..Default::default()
        }),
        10
    );
    assert_eq!(
        count(FilterQuery {
            target: Some("mock:alice".into()),
            ..Default::default()
        }),
        0
    );
    assert_eq!(
        count(FilterQuery {
            action: Some("deploy_mock".into()),
            ..Default::default()
        }),
        15
    );
    assert_eq!(
        count(FilterQuery {
            result: Some("403".into()),
            ..Default::default()
        }),
        6
    );
    assert_eq!(
        count(FilterQuery {
            result: Some("error".into()),
            ..Default::default()
        }),
        6
    );
    assert_eq!(
        count(FilterQuery {
            since: Some("1h".into()),
            until: Some(now.to_rfc3339()),
            ..Default::default()
        }),
        30
    );
    assert_eq!(
        count(FilterQuery {
            until: Some("2020-01-01".into()),
            ..Default::default()
        }),
        0
    );
    for bad in [
        FilterQuery {
            since: Some("yesterday".into()),
            ..Default::default()
        },
        FilterQuery {
            result: Some("maybe".into()),
            ..Default::default()
        },
        FilterQuery {
            cursor: Some("x".into()),
            ..Default::default()
        },
    ] {
        assert!(Filter::parse(&bad, now).is_err());
    }
    // Pages of 7, newest first, without a gap or a repeat.
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let (page, next) = state.audit.list(&f(FilterQuery {
            limit: Some("7".into()),
            cursor: cursor.clone(),
            ..Default::default()
        }));
        seen.extend(page.iter().map(|e| e.id));
        match next {
            Some(n) => cursor = Some(n.to_string()),
            None => break,
        }
    }
    let mut want: Vec<u64> = state.audit.all().iter().map(|e| e.id).collect();
    want.reverse();
    assert_eq!(seen, want);

    // The same through the route, with audit.read.
    let app = test_app_with(state.clone());
    let (status, v) = call(
        &app,
        "GET",
        "/_rustybin/audit?target=user:alice&limit=4",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["entries"].as_array().expect("entries").len(), 4);
    assert!(v["next_cursor"].is_string());
    assert_eq!(v["log"]["capacity"], json!(MAX_ENTRIES));
    let (status, v) = call(&app, "GET", "/_rustybin/audit?since=soon", &[], None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    // Reading the log is never an entry of its own.
    assert_eq!(state.audit.len(), 30);
}

#[tokio::test]
async fn the_stream_sends_the_matching_entries() {
    let state = test_state_with(Config::for_tests());
    let app = test_app_with(state.clone());
    let req = Request::builder()
        .uri("/_rustybin/audit/stream?target=mock:wanted")
        .body(Body::empty())
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body().into_data_stream();
    state.audit.record(draft("mock", "other", "POST /x", 200));
    let wanted = Draft {
        targets: vec![Target {
            kind: "mock".into(),
            name: "wanted".into(),
        }],
        ..draft("mock", "x", "POST /y", 200)
    };
    let id = state.audit.record(wanted).expect("entry").id;
    let mut text = String::new();
    let got = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(chunk)) = body.next().await {
            text.push_str(&String::from_utf8_lossy(&chunk));
            if text.contains("event: entry") && text.contains("\n\n") {
                break;
            }
        }
        text
    })
    .await
    .expect("an event");
    assert!(got.contains(&format!("id: {id}")), "{got}");
    assert!(!got.contains("mock-other"), "{got}");
}

#[tokio::test]
async fn refusals_sign_ins_and_tool_calls() {
    // A control JWT without audit.read reading the log: refused and recorded.
    let state = test_state_with(jwt_config());
    let app = test_app_with(state.clone());
    let console = format!("Bearer {}", control_jwt(Some("console")));
    let (status, v) = call(
        &app,
        "GET",
        "/_rustybin/audit",
        &[("authorization", &console)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    let e = last(&state);
    assert_eq!(e.action, "GET /_rustybin/audit");
    assert_eq!(e.result.outcome, "refused");
    assert_eq!(e.result.permission.as_deref(), Some("audit.read"));
    assert_eq!(e.actor.kind, "token");
    // A refusal inside a handler (a body check) names its permission too.
    let writer = format!(
        "Bearer {}",
        crate::test_support::sign_control_jwt(
            &json!({ "aud": "test-instance", "sub": "ci", "exp": Utc::now().timestamp() + 600, "rb_perms": ["settings.read", "settings.write"] }),
            false
        )
    );
    let (status, _) = call(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &[("authorization", &writer)],
        Some(json!({ "values": { "egress.allow": ["a.example"] } })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let e = last(&state);
    assert_eq!(e.result.outcome, "refused");
    assert!(e
        .targets
        .iter()
        .any(|t| t.kind == "setting" && t.name == "egress.allow"));
    assert_eq!(e.result.permission.as_deref(), Some("settings.security"));
    assert_eq!(e.actor.name.as_deref(), Some("ci"));
    // A read-only tool is no entry; a refused one is.
    let before = state.audit.len();
    let r = tool(
        &app,
        &[("authorization", &console)],
        "list_mocks",
        json!({}),
    )
    .await;
    assert_ne!(r["isError"], json!(true), "{r}");
    assert_eq!(state.audit.len(), before);
    let r = tool(
        &app,
        &[("authorization", &console)],
        "list_audit",
        json!({}),
    )
    .await;
    assert_eq!(r["isError"], json!(true), "{r}");
    let e = last(&state);
    assert_eq!((e.via.as_str(), e.action.as_str()), ("mcp", "list_audit"));
    assert_eq!(e.result.outcome, "refused");
    assert!(e.actor.agent.is_some(), "{e:?}");
    assert_eq!(e.result.permission.as_deref(), Some("audit.read"));
    // A refusal inside a tool names its permission too.
    let applier = format!(
        "Bearer {}",
        crate::test_support::sign_control_jwt(
            &json!({ "aud": "test-instance", "sub": "ci-apply", "exp": Utc::now().timestamp() + 600, "rb_perms": ["config.apply", "mocks.read"] }),
            false
        )
    );
    let r = tool(
        &app,
        &[("authorization", &applier)],
        "apply_config",
        json!({ "document": "version: 2\nsecrets:\n  assist.api_key: ${NOPE}\n" }),
    )
    .await;
    assert_eq!(r["isError"], json!(true), "{r}");
    let e = last(&state);
    assert_eq!(e.action, "apply_config");
    assert_eq!(e.result.outcome, "refused", "{e:?}");
    assert_eq!(e.result.permission.as_deref(), Some("secrets.write"));
    assert_eq!(e.actor.name.as_deref(), Some("ci-apply"));
    // The admin's tool call: who, the agent, the names.
    let admin = "Bearer admin-secret";
    let r = tool(
        &app,
        &[("authorization", admin)],
        "create_mock",
        json!({ "name": "shop", "spec": SPEC }),
    )
    .await;
    assert_ne!(r["isError"], json!(true), "{r}");
    let e = last(&state);
    assert_eq!(e.actor.kind, "admin_token");
    assert!(e.targets.contains(&Target {
        kind: "mock".into(),
        name: "shop".into()
    }));
    // list_audit answers names under untrusted_*.
    let r = tool(
        &app,
        &[("authorization", admin)],
        "list_audit",
        json!({ "target": "mock:shop" }),
    )
    .await;
    let first = &r["structuredContent"]["entries"][0];
    assert_eq!(first["untrusted_targets"][0]["name"], json!("shop"), "{r}");
    assert!(first.get("targets").is_none());
    assert!(!r["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .contains("shop"));

    // A data-plane refusal: the /health toggle without the admin token.
    let mut c = Config::for_tests();
    c.admin_token = Some(ADMIN.into());
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    let (status, _) = call(&app, "POST", "/health/toggle", &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let e = last(&state);
    assert_eq!(e.action, "POST /health/toggle");
    assert_eq!(e.result.permission.as_deref(), Some("settings.write"));
    // Data-plane calls are no entries otherwise.
    let n = state.audit.len();
    let _ = call(&app, "GET", "/get", &[], None).await;
    let _ = call(&app, "POST", "/post", &[], Some(json!({ "a": 1 }))).await;
    assert_eq!(state.audit.len(), n);
}

#[tokio::test]
async fn users_roles_and_projects_name_their_targets() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    let admin = format!("Bearer {ADMIN}");
    let (status, v) = call(
        &app,
        "POST",
        "/_rustybin/users",
        &[("authorization", &admin)],
        Some(json!({ "name": "carol", "roles": ["viewer"] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let e = last(&state);
    assert_eq!(e.event.as_deref(), Some("invite"));
    assert!(e
        .targets
        .iter()
        .any(|t| t.kind == "user" && t.name == "carol"));
    // The invitation link's token is never in the entry.
    let url = v["invitation"]["url"].as_str().unwrap_or("").to_string();
    let token = url.rsplit('/').next().unwrap_or("");
    assert!(!token.is_empty());
    assert!(!serde_json::to_string(&*e).expect("json").contains(token));
    let (status, _) = call(
        &app,
        "PUT",
        "/_rustybin/projects/payments",
        &[("authorization", &admin)],
        Some(json!({ "description": "team" })),
    )
    .await;
    assert!(status.is_success(), "{status}");
    let e = last(&state);
    assert_eq!(e.event.as_deref(), Some("create_project"));
    assert!(e
        .targets
        .iter()
        .any(|t| t.kind == "project" && t.name == "payments"));
    // An entry's targets from the path: a mock and its operation.
    let n = state.audit.len();
    let _ = call(
        &app,
        "PUT",
        "/_rustybin/mocks/nope/operations/getA",
        &[("authorization", &admin)],
        Some(json!({})),
    )
    .await;
    assert_eq!(state.audit.len(), n + 1);
    let e = last(&state);
    assert_eq!(
        e.targets,
        vec![
            Target {
                kind: "mock".into(),
                name: "nope".into()
            },
            Target {
                kind: "operation".into(),
                name: "getA".into()
            }
        ]
    );
    assert_eq!(e.result.status, Some(404));
    let v = body_json(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/_rustybin/audit?action=invite")
                    .header("authorization", &admin)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response"),
    )
    .await;
    assert_eq!(v["entries"].as_array().map(Vec::len), Some(1), "{v}");
}

#[test]
fn param_kinds_and_bounds() {
    assert_eq!(param_kind("/_rustybin/mocks/{name}", "name"), "mock");
    assert_eq!(
        param_kind("/_rustybin/mocks/{name}/operations/{op}", "op"),
        "operation"
    );
    assert_eq!(param_kind("/_rustybin/users/{id}", "id"), "user");
    assert_eq!(param_kind("/_rustybin/settings/{key}", "key"), "setting");
    assert_eq!(param_kind("/_rustybin/harness/{verb}", "verb"), "harness");
    let t = path_targets(
        "/_rustybin/mcp-relay/{session}",
        &[("session".into(), "abc".into())],
    );
    assert!(t[0].name.starts_with("sha256:") && !t[0].name.contains("abc"));
    // The anonymous cap.
    let state = test_state_with(Config::for_tests());
    for _ in 0..(ANONYMOUS_PER_MINUTE + 5) {
        state.audit.record(Draft {
            actor: Actor::kind("anonymous"),
            ..Draft::default()
        });
    }
    assert_eq!(state.audit.len() as u64, ANONYMOUS_PER_MINUTE);
    assert_eq!(state.audit.status()["suppressed"], json!(5));
    // The ring keeps the newest MAX_ENTRIES.
    for i in 0..(MAX_ENTRIES + 3) {
        state
            .audit
            .record(draft("mock", &i.to_string(), "POST /x", 200));
    }
    assert_eq!(state.audit.len(), MAX_ENTRIES);
}

#[test]
fn a_document_from_the_repository_never_shortens_the_log() {
    let state = test_state_with(Config::for_tests());
    let doc = crate::mocks::config_file::parse(
        "version: 2\nsettings:\n  audit.retention_days: 1\n  audit.max_bytes: 65536\n  console.title: Pulled\n",
    )
    .expect("parse");
    let opts = crate::configdoc::ApplyOptions {
        prune: true,
        dry_run: false,
        principal: "repo:test".into(),
        base: None,
        untrusted: true,
        caller: None,
        base_url: None,
    };
    let r = crate::configdoc::apply(&state, doc, &opts);
    let problems = r["problems"].to_string();
    assert!(problems.contains("settings.audit.retention_days"), "{r}");
    assert!(problems.contains("settings.audit.max_bytes"), "{r}");
    assert_eq!(retention_days(&state.settings), DEFAULT_RETENTION_DAYS);
    assert_eq!(max_bytes(&state.settings), DEFAULT_MAX_BYTES as u64);
    assert_eq!(state.settings.get("console.title").0, json!("Pulled"));
    // Changing them needs settings.security (a security group).
    assert!(crate::settings::extra_permissions(RETENTION_KEY, false)
        .contains(&crate::rbac::Permission::SettingsSecurity));
}
