//! Agents act for a user (access control step 12): an agent never exceeds
//! its user, a role change applies to its next call, removing the user
//! revokes it, a user manages their own agents, ceilings name roles, the
//! defaults per role, the 0.4 records move to their user, and a pulled
//! document cannot raise an agent above its user.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::control_auth::ControlAuth;
use crate::mcp::manage::access::ToolNeed;
use crate::mcp::manage::MANAGE_PATH;
use crate::mcp::protocol;
use crate::rbac::{NewUser, PermSet};
use crate::test_support::{body_json, json_request, test_app_with, test_state_with};

const ADMIN: &str = "agents-users-admin";

fn password_config() -> Config {
    let mut c = Config::for_tests();
    c.control_auth = ControlAuth::Password;
    c.admin_token = Some(ADMIN.into());
    c
}

fn add_user(state: &AppState, name: &str, roles: &[&str]) -> String {
    let hash = crate::login::hash_password(&format!("{name}-secret-1")).expect("hash");
    state
        .users
        .add(NewUser::new(name, roles), Some(hash))
        .expect("user")
        .id
}

async fn sign_in(app: &Router, name: &str) -> String {
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            crate::login::LOGIN_PATH,
            &json!({ "username": name, "password": format!("{name}-secret-1") }),
        ))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK, "sign in as {name}");
    format!(
        "Bearer {}",
        body_json(resp).await["token"].as_str().expect("token")
    )
}

fn admin() -> String {
    format!("Bearer {ADMIN}")
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    auth: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let b = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", auth);
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
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn rpc(
    app: &Router,
    auth: &str,
    client: &str,
    method: &str,
    params: Value,
) -> (StatusCode, Value) {
    let mut params = if params.is_null() { json!({}) } else { params };
    params["_meta"] = json!({
        protocol::META_PROTOCOL_VERSION: "2026-07-28",
        protocol::META_CLIENT_CAPABILITIES: {},
        protocol::META_CLIENT_INFO: { "name": client, "version": "1" },
    });
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
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let resp = app
        .clone()
        .oneshot(b.body(Body::from(body.to_string())).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    (status, body_json(resp).await)
}

/// Every page of `tools/list`.
async fn tools(app: &Router, auth: &str, client: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut params = json!({});
    loop {
        let (status, j) = rpc(app, auth, client, "tools/list", params).await;
        assert_eq!(status, StatusCode::OK, "{j}");
        out.extend(
            j["result"]["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .filter_map(|t| t["name"].as_str().map(str::to_string)),
        );
        match j["result"]["nextCursor"].as_str() {
            Some(c) => params = json!({ "cursor": c }),
            None => return out,
        }
    }
}

async fn call(app: &Router, auth: &str, client: &str, tool: &str, args: Value) -> Value {
    let (status, j) = rpc(
        app,
        auth,
        client,
        "tools/call",
        json!({ "name": tool, "arguments": args }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{j}");
    j["result"].clone()
}

fn text(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The id of `client`'s agent acting for `owner`.
async fn agent_of(app: &Router, owner: &str, client: &str) -> String {
    let (status, list) = send(
        app,
        "GET",
        &format!("/_rustybin/agents?owner={owner}"),
        &admin(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    list["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find(|a| a["name"] == client)
        .and_then(|a| a["id"].as_str())
        .unwrap_or_else(|| panic!("no agent {client} of {owner}: {list}"))
        .to_string()
}

/// What a signed-in user's agent may list: the tools whose need the user
/// meets (self-service off), within the ceiling.
fn expected(perms: PermSet, ceiling: &Ceiling, role: Option<PermSet>) -> Vec<String> {
    TOOLS
        .iter()
        .filter(|d| match d.permission {
            ToolNeed::Perm(Permission::VisitsOperate) | ToolNeed::Visitor => false,
            ToolNeed::Perm(p) | ToolNeed::Owner(p) | ToolNeed::Project(p) => perms.has(p),
            ToolNeed::Own(_) => true,
        })
        .filter(|d| ceiling.allows_with(d, role))
        .map(|d| d.name.to_string())
        .collect()
}

#[tokio::test]
async fn an_agent_never_exceeds_its_user() {
    let state = test_state_with(password_config());
    state
        .users
        .save_role(crate::rbac::CustomRole {
            name: "builder".into(),
            description: "Builds mocks".into(),
            permissions: vec![
                "instance.read".into(),
                "mocks.read".into(),
                "mocks.write".into(),
            ],
        })
        .expect("role");
    let app = test_app_with(state.clone());
    let ceilings: &[Option<&str>] = &[
        None,
        Some("read-only"),
        Some("traffic"),
        Some("manage"),
        Some("viewer"),
        Some("editor"),
        Some("admin"),
    ];
    let mut compared = 0;
    for role in ["viewer", "tester", "editor", "admin", "builder"] {
        let name = format!("u-{role}");
        let id = add_user(&state, &name, &[role]);
        let auth = sign_in(&app, &name).await;
        let perms = state.users.resolve(&id).expect("user").permissions;
        tools(&app, &auth, "table").await;
        let agent = agent_of(&app, &name, "table").await;
        for c in ceilings {
            let rights = match c {
                Some(c) => json!({ "rights": { "ceiling": c } }),
                None => json!({ "rights": null }),
            };
            let (status, v) = send(
                &app,
                "PATCH",
                &format!("/_rustybin/agents/{agent}"),
                &admin(),
                Some(rights),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{v}");
            let ceiling = c.and_then(Ceiling::parse_any).unwrap_or_default();
            let role_perms = match &ceiling {
                Ceiling::Role(r) => state.users.role_permissions(r),
                _ => None,
            };
            let listed = tools(&app, &auth, "table").await;
            assert_eq!(
                listed,
                expected(perms, &ceiling, role_perms),
                "{role} under {}",
                ceiling.as_str()
            );
            // Never more than the user: every tool listed needs nothing the
            // user lacks.
            for t in &listed {
                let d = crate::mcp::manage::tools::find(t).expect("tool");
                if let ToolNeed::Perm(p) | ToolNeed::Owner(p) = d.permission {
                    assert!(perms.has(p), "{role}: {t} needs {}", p.name());
                }
            }
            compared += 1;
        }
    }
    assert_eq!(compared, 35);
}

#[tokio::test]
async fn a_role_change_applies_to_the_agents_next_call() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    let id = add_user(&state, "ed", &["editor"]);
    let auth = sign_in(&app, "ed").await;
    assert!(tools(&app, &auth, "ed-agent")
        .await
        .contains(&"create_mock".to_string()));
    state
        .users
        .set_roles(&id, vec!["viewer".into()])
        .expect("roles");
    assert!(!tools(&app, &auth, "ed-agent")
        .await
        .contains(&"create_mock".to_string()));
    let r = call(
        &app,
        &auth,
        "ed-agent",
        "create_mock",
        json!({ "name": "nope", "spec": "openapi: 3.0.0" }),
    )
    .await;
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("permission_required: mocks.write"), "{r}");
    // get_instance says whom it acts for.
    let r = call(&app, &auth, "ed-agent", "get_instance", json!({})).await;
    assert_eq!(
        r["structuredContent"]["caller"]["user"]["name"], "ed",
        "{r}"
    );
    assert_eq!(r["structuredContent"]["caller"]["via"], "sign-in");
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

/// `tools/list` on a 2025 session (the call attributes the agent).
async fn session_list(app: &Router, auth: &str, session: &str) {
    let body = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} });
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
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn disabling_or_deleting_a_user_revokes_their_agents_and_ends_their_sessions() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    let id = add_user(&state, "dee", &["editor"]);
    let auth = sign_in(&app, "dee").await;
    let session = initialize(&app, &auth, "dee-agent").await;
    session_list(&app, &auth, &session).await;
    let agent = agent_of(&app, "dee", "dee-agent").await;
    let (_, v) = send(
        &app,
        "GET",
        &format!("/_rustybin/agents/{agent}"),
        &admin(),
        None,
    )
    .await;
    assert_eq!(v["sessions_open"], 1, "{v}");
    assert_eq!(v["owner"]["name"], "dee");

    let (status, v) = send(
        &app,
        "PATCH",
        &format!("/_rustybin/users/{id}"),
        &admin(),
        Some(json!({ "disabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["agents_revoked"], 1);
    let (_, v) = send(
        &app,
        "GET",
        &format!("/_rustybin/agents/{agent}"),
        &admin(),
        None,
    )
    .await;
    assert_eq!(v["status"], "revoked");
    assert_eq!(v["sessions_open"], 0);
    assert!(v["revoked"]["by"]
        .as_str()
        .expect("by")
        .contains("user removed"));
    assert_eq!(v["owner"]["status"], "disabled");
    let (status, _) = rpc(&app, &auth, "dee-agent", "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Enabled again: the agent stays revoked until someone restores it.
    send(
        &app,
        "PATCH",
        &format!("/_rustybin/users/{id}"),
        &admin(),
        Some(json!({ "disabled": false })),
    )
    .await;
    let auth = sign_in(&app, "dee").await;
    let r = call(&app, &auth, "dee-agent", "get_instance", json!({})).await;
    assert!(text(&r).contains("revoked"), "{r}");

    // Deleting a user revokes theirs too.
    add_user(&state, "gone", &["viewer"]);
    let gone = sign_in(&app, "gone").await;
    tools(&app, &gone, "gone-agent").await;
    let agent = agent_of(&app, "gone", "gone-agent").await;
    let (status, v) = send(&app, "DELETE", "/_rustybin/users/gone", &admin(), None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["agents_revoked"], 1);
    let (_, v) = send(
        &app,
        "GET",
        &format!("/_rustybin/agents/{agent}"),
        &admin(),
        None,
    )
    .await;
    assert_eq!(v["status"], "revoked");
    assert_eq!(v["owner"]["status"], "deleted");
    assert!(v["owner_label"]
        .as_str()
        .expect("label")
        .contains("deleted"));
}

#[tokio::test]
async fn a_user_manages_their_own_agents_without_agents_manage() {
    let state = test_state_with(password_config());
    state
        .users
        .save_role(crate::rbac::CustomRole {
            name: "builder".into(),
            description: String::new(),
            permissions: vec!["instance.read".into(), "mocks.read".into()],
        })
        .expect("role");
    let app = test_app_with(state.clone());
    let bob_id = add_user(&state, "bob", &["builder"]);
    add_user(&state, "carol", &["builder"]);
    let bob = sign_in(&app, "bob").await;
    // His agent signs in on a token of its own: revoking the agent revokes
    // the tokens it used.
    let bob_mcp = sign_in(&app, "bob").await;
    let carol = sign_in(&app, "carol").await;
    tools(&app, &bob_mcp, "bob-agent").await;
    tools(&app, &carol, "carol-agent").await;
    let mine = agent_of(&app, "bob", "bob-agent").await;
    let theirs = agent_of(&app, "carol", "carol-agent").await;

    // Bob sees his own agents only.
    let (status, v) = send(&app, "GET", "/_rustybin/agents", &bob, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["owner"], bob_id.as_str());
    assert_eq!(v["you"], bob_id.as_str());
    assert_eq!(v["agents"].as_array().expect("agents").len(), 1);
    let (status, v) = send(&app, "GET", "/_rustybin/agents?owner=carol", &bob, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(v["permission"], "agents.read");
    let (status, v) = send(
        &app,
        "GET",
        &format!("/_rustybin/agents/{theirs}"),
        &bob,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["permission"], "agents.read");
    let (status, _) = send(
        &app,
        "GET",
        &format!("/_rustybin/agents/{mine}"),
        &bob,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // He limits his own, never widens it again.
    let (status, v) = send(
        &app,
        "PATCH",
        &format!("/_rustybin/agents/{mine}"),
        &bob,
        Some(json!({ "rights": { "deny": ["list_mocks"] } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, v) = send(
        &app,
        "PATCH",
        &format!("/_rustybin/agents/{mine}"),
        &bob,
        Some(json!({ "rights": null })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["permission"], "agents.manage");
    // A note alone changes no tool.
    let (status, _) = send(
        &app,
        "PATCH",
        &format!("/_rustybin/agents/{mine}"),
        &bob,
        Some(json!({ "note": "my laptop" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Carol's agent is not his.
    for (method, path, body) in [
        (
            "PATCH",
            format!("/_rustybin/agents/{theirs}"),
            Some(json!({ "rights": { "ceiling": "read-only" } })),
        ),
        ("POST", format!("/_rustybin/agents/{theirs}/revoke"), None),
        ("POST", format!("/_rustybin/agents/{theirs}/restore"), None),
        (
            "DELETE",
            format!("/_rustybin/agents/{theirs}/sessions"),
            None,
        ),
    ] {
        let (status, v) = send(&app, method, &path, &bob, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {v}");
        assert_eq!(v["permission"], "agents.manage");
    }
    // He revokes his own and restores it; one the operator revoked he cannot.
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/_rustybin/agents/{mine}/sessions"),
        &bob,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &app,
        "POST",
        &format!("/_rustybin/agents/{mine}/revoke"),
        &bob,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, v) = send(
        &app,
        "GET",
        &format!("/_rustybin/agents/{mine}"),
        &admin(),
        None,
    )
    .await;
    assert_eq!(v["revoked"]["by_owner"], true, "{v}");
    let (status, _) = send(
        &app,
        "POST",
        &format!("/_rustybin/agents/{mine}/restore"),
        &bob,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    send(
        &app,
        "POST",
        &format!("/_rustybin/agents/{mine}/revoke"),
        &admin(),
        None,
    )
    .await;
    let (status, v) = send(
        &app,
        "POST",
        &format!("/_rustybin/agents/{mine}/restore"),
        &bob,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["permission"], "agents.manage");
    let (status, _) = send(
        &app,
        "POST",
        &format!("/_rustybin/agents/{mine}/restore"),
        &admin(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // On the management server too: his agent lists his agents only, and
    // cannot touch Carol's.
    let r = call(&app, &bob_mcp, "bob-cli", "list_agents", json!({})).await;
    assert_ne!(r["isError"], true, "{r}");
    let listed = r["structuredContent"]["agents"].as_array().expect("agents");
    assert!(
        listed
            .iter()
            .all(|a| a["owner"]["user_id"] == bob_id.as_str()),
        "{r}"
    );
    let r = call(
        &app,
        &bob_mcp,
        "bob-cli",
        "list_agents",
        json!({ "owner": "carol" }),
    )
    .await;
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("permission_required: agents.read"), "{r}");
    let r = call(
        &app,
        &bob_mcp,
        "bob-cli",
        "get_agent",
        json!({ "id": theirs }),
    )
    .await;
    assert_eq!(r["isError"], true);
    let r = call(
        &app,
        &bob_mcp,
        "bob-cli",
        "revoke_agent",
        json!({ "id": theirs, "confirm": true }),
    )
    .await;
    assert_eq!(r["isError"], true);
    assert!(
        text(&r).contains("permission_required: agents.manage"),
        "{r}"
    );
    // A user's tool list carries the agents tools (their own agents).
    assert!(tools(&app, &bob_mcp, "bob-cli")
        .await
        .contains(&"revoke_agent".to_string()));

    // A credential that is no user still needs the permissions.
    let japp = test_app_with(test_state_with(crate::test_support::jwt_config()));
    let inspector = format!(
        "Bearer {}",
        crate::test_support::control_jwt(Some("inspector"))
    );
    let (status, v) = send(&japp, "GET", "/_rustybin/agents", &inspector, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["permission"], "agents.read");
    let (status, v) = send(
        &japp,
        "POST",
        "/_rustybin/agents/0000000000000000/revoke",
        &inspector,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["permission"], "agents.manage");
}

#[tokio::test]
async fn the_old_ceilings_map_to_their_nearest_roles_and_roles_are_ceilings() {
    assert_eq!(Ceiling::ReadOnly.nearest_role(), Some("viewer"));
    assert_eq!(
        Ceiling::Traffic.nearest_role(),
        Some("viewer + traffic.read")
    );
    assert_eq!(Ceiling::Manage.nearest_role(), Some("editor"));
    assert_eq!(Ceiling::Full.nearest_role(), Some("the owner's own"));
    assert_eq!(
        Ceiling::parse_any("manage"),
        Some(Ceiling::Manage),
        "a 0.3 name wins"
    );
    assert_eq!(
        Ceiling::parse_any("editor"),
        Some(Ceiling::Role("editor".into()))
    );
    assert_eq!(Ceiling::parse_any("Not A Role"), None);
    // A role ceiling is the role's permissions; an unknown role is nothing.
    let viewer = crate::rbac::BuiltIn::Viewer.permissions();
    for d in TOOLS {
        let role = Ceiling::Role("viewer".into());
        assert_eq!(
            role.allows_with(d, Some(viewer)),
            d.permission.permission().is_none_or(|p| viewer.has(p)),
            "{}",
            d.name
        );
        assert!(
            !role.allows_with(d, None),
            "{}: an unknown role allows nothing",
            d.name
        );
    }
    // Each 0.3 ceiling's tools against its nearest role: read-only and the
    // viewer agree on the viewer's reads; manage holds the editor's mock
    // changes; full is the owner's own.
    let editor = crate::rbac::BuiltIn::Editor.permissions();
    for name in ["list_mocks", "get_mock", "list_tests"] {
        let d = crate::mcp::manage::tools::find(name).expect("tool");
        assert!(
            Ceiling::ReadOnly.allows(d)
                && Ceiling::Role("viewer".into()).allows_with(d, Some(viewer)),
            "{name}"
        );
    }
    for name in ["create_mock", "delete_mock", "publish_portal_item"] {
        let d = crate::mcp::manage::tools::find(name).expect("tool");
        assert!(
            Ceiling::Manage.allows(d)
                && Ceiling::Role("editor".into()).allows_with(d, Some(editor)),
            "{name}"
        );
    }

    // The default per role: an admin's agents default to an editor's tools;
    // the admin token's agents act for nobody and keep everything.
    let state = test_state_with(password_config());
    state
        .settings
        .update(
            [(DEFAULT_CEILING_FOR_KEY.to_string(), json!(["admin=editor"]))].into(),
            Default::default(),
            "test",
        )
        .expect("setting");
    assert!(state
        .settings
        .update(
            [(DEFAULT_CEILING_FOR_KEY.to_string(), json!(["admin"]))].into(),
            Default::default(),
            "test",
        )
        .is_err());
    let app = test_app_with(state.clone());
    let first = state.users.first_admin().expect("first admin");
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            crate::login::LOGIN_PATH,
            &json!({ "username": first.name, "password": crate::login::DEFAULT_PASSWORD }),
        ))
        .await
        .expect("response");
    let token = format!(
        "Bearer {}",
        body_json(resp).await["token"].as_str().expect("token")
    );
    let listed = tools(&app, &token, "first-agent").await;
    assert!(listed.contains(&"create_mock".to_string()));
    assert!(!listed.contains(&"apply_config".to_string()));
    assert!(!listed.contains(&"update_settings".to_string()));
    let (_, list) = send(&app, "GET", "/_rustybin/agents", &admin(), None).await;
    let a = list["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find(|a| a["name"] == "first-agent")
        .expect("agent");
    assert_eq!(a["rights"]["source"], "role");
    assert_eq!(a["rights"]["ceiling"], "editor");
    assert_eq!(list["policy"]["default_ceiling_for"][0]["role"], "admin");
    assert!(tools(&app, &admin(), "token-agent")
        .await
        .contains(&"apply_config".to_string()));
    let (_, list) = send(&app, "GET", "/_rustybin/agents", &admin(), None).await;
    let a = list["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find(|a| a["name"] == "token-agent")
        .expect("agent");
    assert_eq!(a["owner"], Value::Null);
    assert_eq!(a["owner_label"], "admin token (no user)");
    // A role named as a ceiling must exist.
    let id = a["id"].as_str().expect("id");
    let (status, v) = send(
        &app,
        "PATCH",
        &format!("/_rustybin/agents/{id}"),
        &admin(),
        Some(json!({ "rights": { "ceiling": "no-such-role" } })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
}

#[test]
fn rules_match_a_user_or_a_role() {
    let (policy, problems) = file_policy(Some(FileSection {
        default_ceiling: None,
        rules: vec![
            FileRule {
                matcher: Match {
                    role: Some("tester".into()),
                    ..Match::default()
                },
                ceiling: Some("viewer".into()),
                ..FileRule::default()
            },
            FileRule {
                matcher: Match {
                    user: Some("ci-*".into()),
                    credential: Some("console".into()),
                    ..Match::default()
                },
                ceiling: Some("read-only".into()),
                ..FileRule::default()
            },
            FileRule {
                matcher: Match {
                    role: Some("Bad Role".into()),
                    ..Match::default()
                },
                ..FileRule::default()
            },
        ],
    }));
    assert_eq!(problems.len(), 1, "{problems:?}");
    let users = std::sync::Arc::new(crate::rbac::Users::in_memory());
    let tess = users
        .add(NewUser::new("tess", &["tester"]), None)
        .expect("user")
        .id;
    let ci = users
        .add(NewUser::new("ci-bot", &["editor"]), None)
        .expect("user")
        .id;
    let policy = Policy {
        rules: policy.rules,
        ..Policy::default()
    }
    .with_users(users.clone());
    let a = Agents::new(
        &Settings::new(&Config::for_tests()),
        &crate::login::Tokens::new(),
        std::sync::Arc::new(crate::limits::ManualClock::new(Utc::now())),
    );
    let seen = |id: &str, client: Option<&str>| Seen {
        credential: Credential::User {
            id: id.into(),
            client: client.map(str::to_string),
        },
        token: None,
        name: "x".into(),
        version: String::new(),
        transport: "streamable-2026",
        session: None,
    };
    let t = a.observe(seen(&tess, None), &policy);
    assert_eq!(t.rights.ceiling, Ceiling::Role("viewer".into()));
    assert_eq!(t.rights.source, "rule");
    let c = a.observe(seen(&ci, None), &policy);
    assert_eq!(c.rights.ceiling, Ceiling::ReadOnly);
    // `console` is a user's sign-in agent, not their OAuth one.
    let o = a.observe(seen(&ci, Some("client:ab")), &policy);
    assert_eq!(o.rights.source, "default");
    let admin = a.observe(
        Seen {
            credential: Credential::Admin,
            ..seen(&ci, None)
        },
        &policy,
    );
    assert_eq!(
        admin.rights.source, "default",
        "a user rule never matches the admin token"
    );
    assert!(Credential::User {
        id: "u".into(),
        client: None
    }
    .is_kind("user"));
    assert!(Credential::User {
        id: "u".into(),
        client: None
    }
    .is_kind("console"));
    assert!(Credential::User {
        id: "u".into(),
        client: Some("c".into())
    }
    .is_kind("oauth"));
    assert!(Credential::Oauth { sub: "c".into() }.is_kind("oauth"));
    assert!(!Credential::Oauth { sub: "c".into() }.is_kind("user"));
}

#[tokio::test]
async fn a_pulled_document_cannot_raise_an_agent_above_its_user() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    let id = add_user(&state, "vic", &["viewer"]);
    let auth = sign_in(&app, "vic").await;
    let before = tools(&app, &auth, "vic-agent").await;
    let doc = "version: 2\nagents:\n  default_ceiling: full\n  rules:\n    - match: { user: vic }\n      ceiling: admin\n    - match: { role: viewer }\n      ceiling: full\n";
    let file = crate::mocks::config_file::parse(doc).expect("doc");
    let report = crate::configdoc::apply(
        &state,
        file,
        &crate::configdoc::ApplyOptions {
            prune: false,
            dry_run: false,
            principal: "git".into(),
            base: None,
            untrusted: true,
            caller: None,
            base_url: None,
        },
    );
    assert_eq!(report["problems"], json!([]), "{report}");
    let after = tools(&app, &auth, "vic-agent").await;
    assert_eq!(after, before, "a ceiling only takes away");
    let perms = state.users.resolve(&id).expect("user").permissions;
    assert_eq!(after, expected(perms, &Ceiling::Full, None));
    // A rule naming a role that does not exist is said, and allows nothing.
    let doc = "version: 2\nagents:\n  rules:\n    - match: { user: vic }\n      ceiling: ghost\n";
    let file = crate::mocks::config_file::parse(doc).expect("doc");
    let report = crate::configdoc::apply(
        &state,
        file,
        &crate::configdoc::ApplyOptions {
            prune: false,
            dry_run: false,
            principal: "git".into(),
            base: None,
            untrusted: true,
            caller: None,
            base_url: None,
        },
    );
    assert!(
        report["problems"][0]
            .as_str()
            .expect("problem")
            .contains("ghost"),
        "{report}"
    );
    let after = tools(&app, &auth, "vic-agent").await;
    assert!(after.is_empty(), "{after:?}");
}

#[test]
fn agents_from_before_0_5_move_to_their_user() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = password_config();
    config.state_dir = Some(dir.path().to_path_buf());
    let state = test_state_with(config.clone());
    let first = state.users.first_admin().expect("first admin");
    let policy = Policy::of(&state);
    let console = Credential::Console {
        user: first.name.clone(),
    };
    let oauth = Credential::Oauth {
        sub: "client:0123456789ab".into(),
    };
    let mk = |credential: Credential, name: &str| Seen {
        credential,
        token: None,
        name: name.into(),
        version: String::new(),
        transport: "streamable-2026",
        session: None,
    };
    let c = state.agents.observe(mk(console, "laptop"), &policy);
    let o = state.agents.observe(mk(oauth, "desktop"), &policy);
    state
        .agents
        .set_rights(
            &c.id,
            Some(Some(Rights {
                ceiling: Some(Ceiling::ReadOnly),
                ..Rights::default()
            })),
            Some(Some("kept".into())),
        )
        .expect("rights");
    state
        .agents
        .revoke(&o.id, "admin", &state.config.console_tokens)
        .expect("revoke");
    // Written as 0.4 wrote it.
    let path = dir.path().join(FILE);
    let mut doc: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("file")).expect("json");
    doc["version"] = json!(1);
    std::fs::write(&path, doc.to_string()).expect("write");

    let again = test_state_with(config);
    let policy = Policy::of(&again);
    let list = again.agents.list(&policy, Some(&first.id));
    assert_eq!(list.len(), 2, "{list:?}");
    let laptop = list.iter().find(|a| a["name"] == "laptop").expect("laptop");
    assert_eq!(laptop["credential"]["kind"], "user");
    assert_eq!(laptop["credential"]["via"], "sign-in");
    assert_eq!(laptop["rights"]["ceiling"], "read-only");
    assert_eq!(laptop["note"], "kept");
    let desktop = list
        .iter()
        .find(|a| a["name"] == "desktop")
        .expect("desktop");
    assert_eq!(desktop["credential"]["via"], "oauth");
    assert_eq!(desktop["credential"]["client"], "client:0123456789ab");
    assert_eq!(desktop["status"], "revoked");
    assert!(again
        .config
        .console_tokens
        .is_subject_revoked("client:0123456789ab"));
    // Once: the file is version 2 now, and a later OAuth record approved by
    // the admin token stays its own.
    let written: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("file")).expect("json");
    assert_eq!(written["version"], DOC_VERSION);
    assert_eq!(again.agents.adopt_users(&again.users), 0);
}
