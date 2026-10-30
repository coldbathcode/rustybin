//! The agents registry: keying, bounds, rights, rules, revocation, and the
//! whole application (two clients on one token, a ceiling on `tools/list`
//! and `tools/call`, revoke and restore with sessions and console tokens,
//! the stdio bridge, the live feed, the routes).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::control_auth::ControlAuth;
use crate::limits::ManualClock;
use crate::login::Tokens;
use crate::mcp::manage::tools::find;
use crate::mcp::manage::MANAGE_PATH;
use crate::mcp::protocol;
use crate::state::AppState;
use crate::test_support::{body_json, json_request, test_app_with, test_state_with};

const ADMIN: &str = "agents-admin-token";

fn def(name: &str) -> &'static ToolDef {
    find(name).expect("a management tool")
}

fn registry() -> Agents {
    let config = Config::for_tests();
    let settings = Settings::new(&config);
    Agents::new(
        &settings,
        &Tokens::new(),
        Arc::new(ManualClock::new(Utc::now())),
    )
}

fn seen(credential: Credential, name: &str) -> Seen {
    Seen {
        credential,
        token: None,
        name: name.into(),
        version: "1.0".into(),
        transport: "streamable-2026",
        session: None,
    }
}

// ── Unit ──

#[test]
fn records_are_keyed_by_credential_and_client_name() {
    let a = registry();
    let p = Policy::default();
    let one = a.observe(seen(Credential::Admin, "claude-code"), &p);
    let two = a.observe(seen(Credential::Admin, "cursor"), &p);
    let again = a.observe(seen(Credential::Admin, "claude-code"), &p);
    let other = a.observe(
        seen(
            Credential::Console {
                user: "rustybin".into(),
            },
            "claude-code",
        ),
        &p,
    );
    assert_ne!(one.id, two.id);
    assert_eq!(one.id, again.id);
    assert_ne!(one.id, other.id);
    assert_eq!(a.len(), 3);
    let nameless = a.observe(seen(Credential::Admin, ""), &p);
    assert_eq!(nameless.id, agent_id(&Credential::Admin, "unknown"));
}

#[test]
fn the_registry_is_bounded_and_keeps_configured_agents() {
    let clock = Arc::new(ManualClock::new(Utc::now()));
    let a = Agents::new(
        &Settings::new(&Config::for_tests()),
        &Tokens::new(),
        clock.clone(),
    );
    let p = Policy::default();
    let first = a.observe(seen(Credential::Admin, "pinned"), &p);
    a.set_rights(&first.id, None, Some(Some("keep me".into())))
        .expect("note");
    for i in 0..MAX_AGENTS + 10 {
        clock.advance(std::time::Duration::from_millis(1));
        a.observe(seen(Credential::Admin, &format!("agent-{i}")), &p);
    }
    assert_eq!(a.len(), MAX_AGENTS);
    assert!(a.get(&first.id, &p).is_some(), "the configured agent stays");
    assert!(
        a.get(&agent_id(&Credential::Admin, "agent-0"), &p)
            .is_none(),
        "the least recently seen went"
    );
}

#[test]
fn per_record_lists_are_bounded() {
    let a = registry();
    let p = Policy::default();
    let s = a.observe(seen(Credential::Admin, "noisy"), &p);
    for i in 0..MAX_ERRORS + 5 {
        a.record_call(
            &s.id,
            "get_mock",
            "error",
            Some(&format!("failure {i}\n{}", "x".repeat(1000))),
        );
    }
    for i in 0..MAX_TOKENS + 5 {
        let mut x = seen(Credential::Console { user: "u".into() }, "noisy");
        x.token = Some((format!("jti-{i}"), i64::MAX));
        a.observe(x, &p);
    }
    let v = a.get(&s.id, &p).expect("agent");
    let errors = v["last_errors"].as_array().expect("errors");
    assert_eq!(errors.len(), MAX_ERRORS);
    assert!(errors[0]["message"].as_str().expect("m").len() <= MAX_ERROR_TEXT);
    assert!(!errors[0]["message"].as_str().expect("m").contains('\n'));
    assert_eq!(v["calls"]["error"], (MAX_ERRORS + 5) as u64);
    assert_eq!(v["calls"]["today"], (MAX_ERRORS + 5) as u64);
    let console = agent_id(&Credential::Console { user: "u".into() }, "noisy");
    let inner = lock(&a.inner);
    assert_eq!(inner.records[&console].tokens.len(), MAX_TOKENS);
}

#[test]
fn ceilings_cut_the_tool_set() {
    use Ceiling::*;
    assert!(ReadOnly.allows(def("list_mocks")));
    assert!(!ReadOnly.allows(def("list_requests")), "captured traffic");
    assert!(!ReadOnly.allows(def("create_mock")));
    assert!(Traffic.allows(def("list_requests")));
    assert!(!Traffic.allows(def("create_bin")));
    assert!(Manage.allows(def("create_mock")));
    assert!(Manage.allows(def("delete_mock")));
    assert!(!Manage.allows(def("apply_config")), "admin");
    assert!(!Manage.allows(def("create_run")), "harness");
    assert!(!Manage.allows(def("end_visit")), "operator");
    assert!(Full.allows(def("apply_config")));
    assert!(ReadOnly < Traffic && Traffic < Manage && Manage < Full);
    for c in CEILINGS {
        assert_eq!(Ceiling::parse(c).map(Ceiling::as_str), Some(*c));
    }
    assert!(Ceiling::parse("admin").is_none());
}

#[test]
fn the_override_wins_field_by_field_over_the_rule_and_the_default() {
    let (file, problems) = file_policy(Some(FileSection {
        default_ceiling: Some("traffic".into()),
        rules: vec![FileRule {
            matcher: Match {
                name: Some("claude-*".into()),
                credential: Some("console".into()),
            },
            ceiling: Some("manage".into()),
            allow: vec![],
            deny: vec!["delete_mock".into()],
            note: Some("the team's assistant".into()),
        }],
    }));
    assert!(problems.is_empty(), "{problems:?}");
    let policy = Policy {
        default_ceiling: file.default_ceiling.unwrap_or_default(),
        default_source: "file",
        rules: file.rules,
    };
    let a = registry();
    let console = Credential::Console {
        user: "rustybin".into(),
    };
    let matched = a.observe(seen(console.clone(), "Claude-Code"), &policy);
    assert_eq!(matched.rights.source, "rule");
    assert_eq!(matched.rights.ceiling, Ceiling::Manage);
    assert!(matched.rights.allows(def("create_mock")));
    assert!(!matched.rights.allows(def("delete_mock")));
    assert!(matched.rights.denial(def("delete_mock")).contains("denied"));
    let other = a.observe(seen(Credential::Admin, "claude-code"), &policy);
    assert_eq!(other.rights.source, "default");
    assert_eq!(other.rights.ceiling, Ceiling::Traffic);
    assert!(other.rights.denial(def("create_mock")).contains("traffic"));

    // The agent's own ceiling over the rule; the rule's deny list stays.
    a.set_rights(
        &matched.id,
        Some(Some(Rights {
            ceiling: Some(Ceiling::ReadOnly),
            allow: None,
            deny: None,
        })),
        None,
    )
    .expect("rights");
    let now = a.observe(seen(console.clone(), "Claude-Code"), &policy);
    assert_eq!(now.rights.source, "agent");
    assert_eq!(now.rights.ceiling, Ceiling::ReadOnly);
    assert_eq!(now.rights.deny, vec!["delete_mock".to_string()]);
    // An allow list narrows to its tools.
    a.set_rights(
        &matched.id,
        Some(Some(Rights {
            ceiling: Some(Ceiling::Full),
            allow: Some(vec!["get_mock".into(), "get_mock".into()]),
            deny: Some(vec![]),
        })),
        None,
    )
    .expect("rights");
    let now = a.observe(seen(console.clone(), "Claude-Code"), &policy);
    assert_eq!(now.rights.allow, vec!["get_mock".to_string()]);
    assert!(now.rights.allows(def("get_mock")));
    assert!(!now.rights.allows(def("list_mocks")));
    // null goes back to the rule.
    a.set_rights(&matched.id, Some(None), None)
        .expect("cleared");
    let now = a.observe(seen(console, "Claude-Code"), &policy);
    assert_eq!(now.rights.source, "rule");
    // Unknown tools are refused.
    assert!(matches!(
        a.set_rights(
            &matched.id,
            Some(Some(Rights {
                deny: Some(vec!["rm_rf".into()]),
                ..Rights::default()
            })),
            None
        ),
        Err(AgentError::Invalid(_))
    ));
    assert_eq!(
        a.set_rights("nope", Some(None), None),
        Err(AgentError::NotFound)
    );
}

#[test]
fn file_rules_are_validated() {
    let (policy, problems) = file_policy(Some(FileSection {
        default_ceiling: Some("root".into()),
        rules: vec![
            FileRule {
                ceiling: Some("everything".into()),
                ..FileRule::default()
            },
            FileRule {
                matcher: Match {
                    name: None,
                    credential: Some("password".into()),
                },
                ..FileRule::default()
            },
            FileRule {
                deny: vec!["no_such_tool".into()],
                ..FileRule::default()
            },
            FileRule {
                matcher: Match {
                    name: Some("*".into()),
                    credential: None,
                },
                ceiling: Some("read-only".into()),
                ..FileRule::default()
            },
        ],
    }));
    assert_eq!(policy.default_ceiling, None);
    assert_eq!(policy.rules.len(), 1);
    assert_eq!(problems.len(), 4, "{problems:?}");
    let (none, p) = file_policy(None);
    assert_eq!(none, FilePolicy::default());
    assert!(p.is_empty());
}

#[test]
fn glob_matches_names() {
    assert!(glob("claude-code", "Claude-Code"));
    assert!(glob("claude-*", "claude-code"));
    assert!(glob("*code*", "claude-code-cli"));
    assert!(glob("*", ""));
    assert!(glob("a*b*c", "aXXbYYc"));
    assert!(!glob("a*b*c", "aXXbYY"));
    assert!(!glob("cursor", "claude"));
}

#[test]
fn revocation_takes_tokens_and_sessions_and_restore_gives_them_back() {
    let tokens = Tokens::new();
    let a = registry();
    let p = Policy::default();
    let closed = crate::mcp::sessions::CancelToken::new();
    let mut x = seen(
        Credential::Console {
            user: "rustybin".into(),
        },
        "claude-code",
    );
    x.token = Some(("jti-1".into(), i64::MAX / 2));
    x.session = Some(("session-1".into(), closed.clone()));
    let s = a.observe(x, &p);
    let report = a.revoke(&s.id, "admin", &tokens).expect("revoked");
    assert_eq!(report["sessions_ended"], 1);
    assert_eq!(report["tokens_revoked"], 1);
    assert!(closed.is_cancelled());
    assert!(tokens.is_revoked("jti-1"));
    assert!(a.session_revoked("session-1"));
    assert!(
        a.observe(
            seen(
                Credential::Console {
                    user: "rustybin".into()
                },
                "claude-code"
            ),
            &p
        )
        .revoked
    );
    a.restore(&s.id, &tokens).expect("restored");
    assert!(!tokens.is_revoked("jti-1"));
    assert!(!a.session_revoked("session-1"));

    // OAuth: the client's subject, refresh tokens included.
    let oauth = Credential::Oauth {
        sub: "client:abc".into(),
    };
    let s = a.observe(seen(oauth, "cursor"), &p);
    a.revoke(&s.id, "admin", &tokens).expect("revoked");
    assert!(tokens.is_subject_revoked("client:abc"));
    a.restore(&s.id, &tokens).expect("restored");
    assert!(!tokens.is_subject_revoked("client:abc"));

    // The admin token has nothing to revoke; the note says so.
    let s = a.observe(seen(Credential::Admin, "x"), &p);
    let r = a.revoke(&s.id, "admin", &tokens).expect("revoked");
    assert_eq!(r["tokens_revoked"], 0);
    assert!(r["note"].as_str().expect("note").contains("rotate"));
    assert_eq!(
        a.revoke("nope", "admin", &tokens).err(),
        Some(AgentError::NotFound)
    );
}

#[test]
fn rights_and_revocations_survive_a_restart() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = Config::for_tests();
    config.state_dir = Some(tmp.path().to_path_buf());
    let settings = Settings::new(&config);
    let tokens = Tokens::new();
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(Utc::now()));
    let a = Agents::new(&settings, &tokens, clock.clone());
    assert!(a.persisted());
    let p = Policy::default();
    let s = a.observe(
        seen(
            Credential::Oauth {
                sub: "client:k".into(),
            },
            "cursor",
        ),
        &p,
    );
    a.record_call(&s.id, "get_mock", "ok", None);
    a.set_rights(
        &s.id,
        Some(Some(Rights {
            ceiling: Some(Ceiling::Traffic),
            ..Rights::default()
        })),
        Some(Some("ci bot".into())),
    )
    .expect("rights");
    a.revoke(&s.id, "admin", &tokens).expect("revoked");

    let fresh_tokens = Tokens::new();
    let b = Agents::new(&settings, &fresh_tokens, clock);
    let v = b.get(&s.id, &p).expect("kept");
    assert_eq!(v["rights"]["ceiling"], "traffic");
    assert_eq!(v["note"], "ci bot");
    assert_eq!(v["status"], "revoked");
    assert_eq!(v["calls"]["ok"], 1);
    assert!(
        fresh_tokens.is_subject_revoked("client:k"),
        "refused again at once"
    );
}

// ── The whole application ──

fn admin_config() -> Config {
    let mut c = Config::for_tests();
    c.control_auth = ControlAuth::Token;
    c.admin_token = Some(ADMIN.into());
    c.mocks_sample = true;
    c
}

fn bearer() -> String {
    format!("Bearer {ADMIN}")
}

/// A 2026-07-28 call with `clientInfo` in `_meta`.
async fn rpc(
    app: &Router,
    auth: &str,
    client: Option<&str>,
    extra: &[(&str, &str)],
    method: &str,
    params: Value,
) -> (StatusCode, Value) {
    let mut params = if params.is_null() { json!({}) } else { params };
    params["_meta"] = json!({
        protocol::META_PROTOCOL_VERSION: "2026-07-28",
        protocol::META_CLIENT_CAPABILITIES: {},
    });
    if let Some(c) = client {
        params["_meta"][protocol::META_CLIENT_INFO] = json!({ "name": c, "version": "9.1" });
    }
    let mut b = Request::builder()
        .method("POST")
        .uri(MANAGE_PATH)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", method)
        .header("authorization", auth);
    if let Some(n) = params.get("name").and_then(Value::as_str) {
        b = b.header("mcp-name", n);
    }
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let resp = app
        .clone()
        .oneshot(b.body(Body::from(body.to_string())).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn names(app: &Router, auth: &str, client: &str) -> Vec<String> {
    let (status, j) = rpc(app, auth, Some(client), &[], "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{j}");
    j["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect()
}

async fn call(app: &Router, auth: &str, client: &str, tool: &str) -> Value {
    let (status, j) = rpc(
        app,
        auth,
        Some(client),
        &[],
        "tools/call",
        json!({ "name": tool, "arguments": {} }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{j}");
    j["result"].clone()
}

async fn control(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", bearer());
    let req = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string())),
        None => {
            b = b.header("accept", "application/json");
            b.body(Body::empty())
        }
    }
    .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn find_agent<'a>(list: &'a Value, name: &str) -> &'a Value {
    list["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find(|a| a["name"] == name)
        .unwrap_or_else(|| panic!("no agent {name} in {list}"))
}

#[tokio::test]
async fn two_clients_on_one_token_are_two_agents() {
    let app = test_app_with(test_state_with(admin_config()));
    let auth = bearer();
    assert!(names(&app, &auth, "claude-code")
        .await
        .contains(&"create_mock".to_string()));
    call(&app, &auth, "claude-code", "list_mocks").await;
    call(&app, &auth, "cursor", "get_instance").await;
    let (status, list) = control(&app, "GET", "/_rustybin/agents", None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["agents"].as_array().expect("agents").len(), 2);
    let claude = find_agent(&list, "claude-code");
    assert_eq!(claude["version"], "9.1");
    assert_eq!(claude["credential"]["kind"], "admin");
    assert_eq!(claude["transport"], "streamable-2026");
    assert_eq!(claude["calls"]["ok"], 1);
    assert_eq!(claude["calls"]["lists"], 1);
    assert_eq!(claude["status"], "active");
    assert_eq!(list["policy"]["default_ceiling"], "full");
    let id = claude["id"].as_str().expect("id").to_string();
    let (status, one) = control(&app, "GET", &format!("/_rustybin/agents/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["tools"][0]["name"], "list_mocks");
    assert!(one["available_tools"].as_array().expect("tools").len() > 40);

    // Admin only.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/_rustybin/agents")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_ceiling_hides_tools_and_refuses_calls() {
    let app = test_app_with(test_state_with(admin_config()));
    let auth = bearer();
    call(&app, &auth, "ci-bot", "get_instance").await;
    let (_, list) = control(&app, "GET", "/_rustybin/agents", None).await;
    let id = find_agent(&list, "ci-bot")["id"]
        .as_str()
        .expect("id")
        .to_string();
    let (status, v) = control(
        &app,
        "PATCH",
        &format!("/_rustybin/agents/{id}"),
        Some(json!({ "rights": { "ceiling": "read-only", "deny": ["find_endpoints"] }, "note": "ci" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["rights"]["ceiling"], "read-only");
    assert_eq!(v["rights"]["source"], "agent");
    assert_eq!(v["note"], "ci");

    let tools = names(&app, &auth, "ci-bot").await;
    assert!(tools.contains(&"list_mocks".to_string()));
    assert!(!tools.contains(&"create_mock".to_string()));
    assert!(!tools.contains(&"list_requests".to_string()));
    assert!(!tools.contains(&"find_endpoints".to_string()));
    let r = call(&app, &auth, "ci-bot", "set_health").await;
    assert_eq!(r["isError"], true);
    assert!(
        r["content"][0]["text"]
            .as_str()
            .expect("t")
            .contains("read-only"),
        "{r}"
    );
    // Another client on the same token is not limited.
    assert!(names(&app, &auth, "claude-code")
        .await
        .contains(&"create_mock".to_string()));

    let (_, v) = control(&app, "GET", &format!("/_rustybin/agents/{id}"), None).await;
    assert_eq!(v["calls"]["denied"], 1);
    let (status, _) = control(
        &app,
        "PATCH",
        &format!("/_rustybin/agents/{id}"),
        Some(json!({ "rights": { "ceiling": "root" } })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, v) = control(
        &app,
        "PATCH",
        &format!("/_rustybin/agents/{id}"),
        Some(json!({ "rights": null })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["rights"]["ceiling"], "full");
    assert_eq!(v["note"], "ci", "the note stays");
}

#[tokio::test]
async fn the_default_ceiling_and_file_rules_apply_on_sight() {
    let mut config = admin_config();
    config.agents_default_ceiling = Some("traffic".into());
    let state = test_state_with(config);
    let (policy, _) = file_policy(Some(FileSection {
        default_ceiling: None,
        rules: vec![FileRule {
            matcher: Match {
                name: Some("trusted-*".into()),
                credential: Some("admin".into()),
            },
            ceiling: Some("full".into()),
            ..FileRule::default()
        }],
    }));
    state.mocks.set_file_agents(policy);
    let app = test_app_with(state);
    let auth = bearer();
    let tools = names(&app, &auth, "someone").await;
    assert!(tools.contains(&"list_requests".to_string()));
    assert!(!tools.contains(&"create_mock".to_string()));
    assert!(names(&app, &auth, "trusted-agent")
        .await
        .contains(&"apply_config".to_string()));
    let (_, list) = control(&app, "GET", "/_rustybin/agents", None).await;
    assert_eq!(list["policy"]["default_source"], "env");
    assert_eq!(
        find_agent(&list, "trusted-agent")["rights"]["source"],
        "rule"
    );
}

async fn login(app: &Router) -> String {
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            crate::login::LOGIN_PATH,
            &json!({ "username": "rustybin", "password": "rustybin" }),
        ))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp).await["token"]
        .as_str()
        .expect("token")
        .to_string()
}

/// `initialize` on the 2025 handshake; returns the session id.
async fn initialize(app: &Router, auth: &str, client: &str) -> String {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": { "name": client, "version": "2" } } });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(MANAGE_PATH)
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("authorization", auth)
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    resp.headers()[protocol::HEADER_SESSION_ID]
        .to_str()
        .expect("id")
        .to_string()
}

async fn session_call(app: &Router, auth: &str, session: &str, tool: &str) -> (StatusCode, Value) {
    let body = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": tool, "arguments": {} } });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(MANAGE_PATH)
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2025-11-25")
                .header(protocol::HEADER_SESSION_ID, session)
                .header("authorization", auth)
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn revoke_ends_sessions_and_the_console_token_and_restore_brings_them_back() {
    let mut config = Config::for_tests();
    config.control_auth = ControlAuth::Password;
    config.admin_token = Some(ADMIN.into());
    let app = test_app_with(test_state_with(config));
    let token = login(&app).await;
    let auth = format!("Bearer {token}");
    let session = initialize(&app, &auth, "claude-code").await;
    let (status, j) = session_call(&app, &auth, &session, "get_instance").await;
    assert_eq!(status, StatusCode::OK, "{j}");

    let (_, list) = control(&app, "GET", "/_rustybin/agents", None).await;
    let agent = find_agent(&list, "claude-code");
    assert_eq!(agent["credential"]["kind"], "console");
    assert_eq!(agent["credential"]["user"], "rustybin");
    assert_eq!(agent["transport"], "streamable-2025");
    assert_eq!(agent["sessions_open"], 1);
    let id = agent["id"].as_str().expect("id").to_string();

    let (status, r) = control(
        &app,
        "POST",
        &format!("/_rustybin/agents/{id}/revoke"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["sessions_ended"], 1);
    assert_eq!(r["tokens_revoked"], 1);
    // The token is refused everywhere, its session included.
    let (status, _) = session_call(&app, &auth, &session, "get_instance").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = rpc(
        &app,
        &auth,
        Some("claude-code"),
        &[],
        "tools/list",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (_, v) = control(&app, "GET", &format!("/_rustybin/agents/{id}"), None).await;
    assert_eq!(v["status"], "revoked");
    assert_eq!(v["revoked"]["by"], "admin");
    assert_eq!(v["sessions_open"], 0);

    let (status, _) = control(
        &app,
        "POST",
        &format!("/_rustybin/agents/{id}/restore"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(names(&app, &auth, "claude-code")
        .await
        .contains(&"get_instance".to_string()));
}

#[tokio::test]
async fn a_revoked_admin_agent_is_refused_at_every_call() {
    let app = test_app_with(test_state_with(admin_config()));
    let auth = bearer();
    let session = initialize(&app, &auth, "rogue").await;
    let (status, _) = session_call(&app, &auth, &session, "get_instance").await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = control(&app, "GET", "/_rustybin/agents", None).await;
    let id = find_agent(&list, "rogue")["id"]
        .as_str()
        .expect("id")
        .to_string();
    let (_, r) = control(
        &app,
        "POST",
        &format!("/_rustybin/agents/{id}/revoke"),
        None,
    )
    .await;
    assert!(r["note"]
        .as_str()
        .expect("note")
        .contains("RUSTYBIN_ADMIN_TOKEN"));
    // Its session answers 403; a stateless call gets the tool error.
    let (status, _) = session_call(&app, &auth, &session, "get_instance").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(names(&app, &auth, "rogue").await.is_empty());
    let r = call(&app, &auth, "rogue", "get_instance").await;
    assert_eq!(r["isError"], true);
    assert!(r["content"][0]["text"]
        .as_str()
        .expect("t")
        .contains("revoked"));
    assert!(!names(&app, &auth, "other").await.is_empty());
}

#[tokio::test]
async fn end_sessions_closes_them_without_revoking() {
    let app = test_app_with(test_state_with(admin_config()));
    let auth = bearer();
    let session = initialize(&app, &auth, "zed").await;
    let (status, _) = session_call(&app, &auth, &session, "get_instance").await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = control(&app, "GET", "/_rustybin/agents", None).await;
    let id = find_agent(&list, "zed")["id"]
        .as_str()
        .expect("id")
        .to_string();
    let (status, r) = control(
        &app,
        "DELETE",
        &format!("/_rustybin/agents/{id}/sessions"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(r["sessions_ended"], 1);
    let (status, _) = session_call(&app, &auth, &session, "get_instance").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the session is gone");
    let session = initialize(&app, &auth, "zed").await;
    let (status, _) = session_call(&app, &auth, &session, "get_instance").await;
    assert_eq!(status, StatusCode::OK, "a new one works");
}

#[tokio::test]
async fn the_stdio_bridge_registers_as_an_agent() {
    let app = test_app_with(test_state_with(admin_config()));
    let ua = format!("{}0.2.0", crate::mcp::manage::stdio::USER_AGENT_PREFIX);
    let (status, _) = rpc(
        &app,
        &bearer(),
        Some("claude-desktop"),
        &[("user-agent", ua.as_str())],
        "tools/list",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = control(&app, "GET", "/_rustybin/agents", None).await;
    assert_eq!(find_agent(&list, "claude-desktop")["transport"], "stdio");
}

#[tokio::test]
async fn the_feed_carries_calls() {
    let state: AppState = test_state_with(admin_config());
    let (mut rx, _guard) = state.agents.subscribe().expect("a slot");
    let app = test_app_with(state.clone());
    call(&app, &bearer(), "watcher", "get_health").await;
    let ev = rx.recv().await.expect("an event");
    assert_eq!(ev["name"], "watcher");
    assert_eq!(ev["tool"], "get_health");
    assert_eq!(ev["outcome"], "ok");
    // The route answers a stream, bounded in listeners.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(api::EVENTS_PATH)
                .header("authorization", bearer())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()["content-type"]
        .to_str()
        .expect("ct")
        .starts_with("text/event-stream"));
    let held: Vec<_> = (0..MAX_LISTENERS)
        .filter_map(|_| state.agents.subscribe())
        .collect();
    assert!(
        held.len() < MAX_LISTENERS,
        "the route and the test hold slots"
    );
    assert!(state.agents.subscribe().is_none());
}

#[test]
fn the_config_file_section_loads_with_its_problems() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("rustybin.yaml");
    std::fs::write(
        &path,
        "version: 1\nagents:\n  default_ceiling: manage\n  rules:\n    - match: { name: \"claude-*\", credential: oauth }\n      ceiling: read-only\n      deny: [delete_mock]\n      note: from git\n    - match: { credential: robot }\n",
    )
    .expect("write");
    let loaded = crate::mocks::config_file::load_file(&path, |_| None, 1 << 20).expect("loaded");
    assert_eq!(loaded.agents.default_ceiling, Some(Ceiling::Manage));
    assert_eq!(loaded.agents.rules.len(), 1);
    assert_eq!(loaded.agents.rules[0].note.as_deref(), Some("from git"));
    assert!(
        loaded
            .problems
            .iter()
            .any(|p| p.contains("agents.rules[1]")),
        "{:?}",
        loaded.problems
    );
    let state = test_state_with(Config::for_tests());
    state.mocks.set_file_agents(loaded.agents);
    let policy = Policy::of(&state);
    assert_eq!(policy.default_ceiling, Ceiling::Manage);
    assert_eq!(policy.default_source, "file");
    // A saved setting wins over the file.
    let mut values = std::collections::BTreeMap::new();
    values.insert(DEFAULT_CEILING_KEY.into(), json!("traffic"));
    state
        .settings
        .update(values, Default::default(), "test")
        .expect("saved");
    let policy = Policy::of(&state);
    assert_eq!(policy.default_ceiling, Ceiling::Traffic);
    assert_eq!(policy.default_source, "stored");
}
