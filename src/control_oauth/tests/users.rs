//! A user approves an agent's OAuth sign-in: the consent page lists what it
//! will hold, the token acts for the user (never more than the user, the
//! user's role change, disable and password change apply to it), and the
//! first admin's password change ends a compromised admin's agent tokens,
//! 0.4 ones included, without signing the other users out.

use super::*;
use crate::rbac::NewUser;

fn password_config() -> Config {
    let mut c = Config::for_tests();
    c.control_auth = ControlAuth::Password;
    c.admin_token = Some(ADMIN.into());
    c.control_oauth_key = OAuthKey::from_settings(None, None);
    c
}

async fn sign_in_user(app: &Router, name: &str, password: &str) -> String {
    let resp = send(
        app,
        "POST",
        crate::login::LOGIN_PATH,
        &[],
        Some((
            "application/json",
            json!({ "username": name, "password": password }).to_string(),
        )),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "sign in as {name}");
    body_json(resp).await["token"]
        .as_str()
        .expect("token")
        .to_string()
}

async fn consent(app: &Router, id: &str, auth: &str) -> (StatusCode, Value) {
    let bearer = format!("Bearer {auth}");
    let resp = send(
        app,
        "GET",
        &format!("{REQUESTS_PREFIX}{id}"),
        &[("authorization", bearer.as_str())],
        None,
    )
    .await;
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn refresh(app: &Router, client_id: &str, token: &str) -> (StatusCode, Value) {
    let mut f = form_urlencoded::Serializer::new(String::new());
    f.append_pair("grant_type", "refresh_token")
        .append_pair("client_id", client_id)
        .append_pair("refresh_token", token);
    let resp = send(
        app,
        "POST",
        TOKEN_PATH,
        &[],
        Some(("application/x-www-form-urlencoded", f.finish())),
    )
    .await;
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn caller(app: &Router, token: &str) -> Value {
    let resp = mcp(
        app,
        Some(token),
        "tools/call",
        json!({ "name": "get_instance", "arguments": {} }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp).await["result"]["structuredContent"]["caller"].clone()
}

async fn status_of(app: &Router, uri: &str, token: &str) -> StatusCode {
    let bearer = format!("Bearer {token}");
    send(app, "GET", uri, &[("authorization", bearer.as_str())], None)
        .await
        .status()
}

#[tokio::test]
async fn a_users_agent_holds_the_users_permissions_within_the_scope_never_more() {
    let state = test_state_with(password_config());
    state
        .users
        .save_role(crate::rbac::CustomRole {
            name: "builder".into(),
            description: String::new(),
            permissions: vec![
                "instance.read".into(),
                "mocks.read".into(),
                "mocks.write".into(),
            ],
        })
        .expect("role");
    let hash = crate::login::hash_password("bob-secret-1").expect("hash");
    let bob_id = state
        .users
        .add(NewUser::new("bob", &["builder"]), Some(hash))
        .expect("user")
        .id;
    let app = test_app_with(state.clone());
    let bob = sign_in_user(&app, "bob", "bob-secret-1").await;

    // The consent page: what each scope gives, never more than Bob holds.
    let (_, id) = start(&app, Some("admin console inspector")).await;
    let (status, req) = consent(&app, &id, &bob).await;
    assert_eq!(status, StatusCode::OK, "{req}");
    assert_eq!(req["approver"]["kind"], "user");
    assert_eq!(req["approver"]["name"], "bob");
    assert_eq!(req["acts_for_user"], true);
    assert_eq!(
        req["scopes_grantable"],
        json!(["admin", "console"]),
        "inspector gives Bob nothing"
    );
    assert_eq!(
        req["permissions"],
        json!(["instance.read", "mocks.read", "mocks.write"])
    );
    assert_eq!(req["grants"][0]["scope"], "admin");
    assert_eq!(
        req["grants"][0]["permissions"],
        json!(["instance.read", "mocks.read", "mocks.write"])
    );
    assert_eq!(
        req["grants"][1]["permissions"],
        json!(["instance.read", "mocks.read"])
    );
    // A scope that gives him nothing cannot be approved.
    let (status, _) = answer(
        &app,
        &id,
        &bob,
        json!({ "decision": "approve", "scope": "inspector" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The whole flow with the admin scope: still Bob's permissions.
    let (client_id, tokens) = sign_in(&app, &bob, Some("admin")).await;
    let access = tokens["access_token"].as_str().expect("access");
    let c = caller(&app, access).await;
    assert_eq!(c["user"]["name"], "bob", "{c}");
    assert_eq!(c["via"], "oauth");
    assert_eq!(
        c["permissions"],
        json!(["instance.read", "mocks.read", "mocks.write"])
    );
    let names = tool_names(&app, access).await;
    assert!(names.contains(&"create_mock".to_string()));
    assert!(!names.contains(&"apply_config".to_string()));
    let (_, list) = {
        let resp = send(
            &app,
            "GET",
            &format!("/_rustybin/agents?owner={bob_id}"),
            &[("authorization", &format!("Bearer {ADMIN}"))],
            None,
        )
        .await;
        let s = resp.status();
        (s, body_json(resp).await)
    };
    let agent = list["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find(|a| a["credential"]["via"] == "oauth")
        .unwrap_or_else(|| panic!("{list}"));
    assert_eq!(agent["owner"]["name"], "bob");
    assert!(agent["credential"]["client"]
        .as_str()
        .expect("client")
        .starts_with("client:"));

    // A refresh keeps acting for Bob.
    let (status, again) = refresh(
        &app,
        &client_id,
        tokens["refresh_token"].as_str().expect("refresh"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    let access = again["access_token"].as_str().expect("access").to_string();
    assert_eq!(caller(&app, &access).await["user"]["name"], "bob");

    // His role changes: the agent's next call follows.
    state
        .users
        .set_roles(&bob_id, vec!["viewer".into()])
        .expect("roles");
    assert!(!tool_names(&app, &access)
        .await
        .contains(&"create_mock".to_string()));

    // Disabled: the token and the refresh token are refused.
    state.users.set_disabled(&bob_id, true).expect("disable");
    let resp = mcp(&app, Some(&access), "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let (status, v) = refresh(
        &app,
        &client_id,
        again["refresh_token"].as_str().expect("refresh"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"], "invalid_grant");
}

#[tokio::test]
async fn a_compromised_first_admins_agent_token_dies_on_the_password_change() {
    let state = test_state_with(password_config());
    let hash = crate::login::hash_password("vee-secret-1").expect("hash");
    state
        .users
        .add(NewUser::new("vee", &["viewer"]), Some(hash))
        .expect("user");
    let app = test_app_with(state.clone());
    let first = sign_in_user(
        &app,
        crate::login::DEFAULT_USER,
        crate::login::DEFAULT_PASSWORD,
    )
    .await;
    let vee = sign_in_user(&app, "vee", "vee-secret-1").await;

    // The attacker's agent, approved with the first admin's console.
    let (client_id, tokens) = sign_in(&app, &first, Some("admin")).await;
    let access = tokens["access_token"].as_str().expect("access").to_string();
    let refresh_token = tokens["refresh_token"]
        .as_str()
        .expect("refresh")
        .to_string();
    assert_eq!(
        caller(&app, &access).await["user"]["name"],
        crate::login::DEFAULT_USER
    );
    // A 0.4 token (no rb_uid): the first admin's.
    let now = chrono::Utc::now().timestamp();
    let legacy = crate::control_oauth::sign(
        &json!({
            "iss": format!("{BASE}{ISSUER_PATH}"),
            "aud": format!("{BASE}{MANAGE_PATH}"),
            "sub": "client:legacy00000",
            "client_id": client_id,
            "scope": "admin",
            "iat": now,
            "exp": now + 600,
            "jti": "legacy-jti",
            "rb_use": "access",
            "rb_gen": state.config.console_tokens.generation(),
        }),
        &state.config,
    )
    .expect("token");
    assert_eq!(
        caller(&app, &legacy).await["user"]["name"],
        crate::login::DEFAULT_USER
    );
    // An agent the admin token approved acts for nobody.
    let (_, by_token) = sign_in(&app, ADMIN, Some("admin")).await;
    let by_token = by_token["access_token"]
        .as_str()
        .expect("access")
        .to_string();
    assert_eq!(caller(&app, &by_token).await["user"], Value::Null);

    // The first admin's password changes (the owner took the account back).
    let bearer = format!("Bearer {first}");
    let resp = send(
        &app,
        "PUT",
        crate::login::ME_PASSWORD_PATH,
        &[("authorization", bearer.as_str())],
        Some((
            "application/json",
            json!({ "current_password": crate::login::DEFAULT_PASSWORD, "new_password": "taken-back-1" })
                .to_string(),
        )),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Every token acting for the first admin is refused, 0.4 ones too.
    for t in [&access, &legacy] {
        let resp = mcp(&app, Some(t), "tools/list", json!({})).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
    let (status, v) = refresh(&app, &client_id, &refresh_token).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(
        status_of(&app, crate::login::SESSION_PATH, &first).await,
        StatusCode::UNAUTHORIZED
    );
    // The other users stay signed in.
    assert_eq!(
        status_of(&app, crate::login::SESSION_PATH, &vee).await,
        StatusCode::OK
    );
    // The admin token's agent outlives it; signing everyone out ends it.
    let resp = mcp(&app, Some(&by_token), "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let admin = format!("Bearer {ADMIN}");
    let resp = send(
        &app,
        "DELETE",
        crate::login::SESSIONS_PATH,
        &[("authorization", admin.as_str())],
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = mcp(&app, Some(&by_token), "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        status_of(&app, crate::login::SESSION_PATH, &vee).await,
        StatusCode::UNAUTHORIZED
    );
}

/// The OAuth agent of the user `name` (its id), after one call.
async fn oauth_agent_of(app: &Router, owner_id: &str) -> String {
    let resp = send(
        app,
        "GET",
        &format!("/_rustybin/agents?owner={owner_id}"),
        &[("authorization", &format!("Bearer {ADMIN}"))],
        None,
    )
    .await;
    let list = body_json(resp).await;
    list["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["credential"]["via"] == "oauth")
        .and_then(|a| a["id"].as_str())
        .unwrap_or_else(|| panic!("{list}"))
        .to_string()
}

/// Registration is open and the client id follows from the metadata, so
/// two users share a client: revoking one user's agent refuses that user's
/// tokens only.
#[tokio::test]
async fn revoking_one_users_oauth_agent_leaves_another_users_tokens_alone() {
    let state = test_state_with(password_config());
    let mut ids = Vec::new();
    for name in ["alice", "bob"] {
        let hash = crate::login::hash_password(&format!("{name}-secret-1")).expect("hash");
        ids.push(
            state
                .users
                .add(NewUser::new(name, &["editor"]), Some(hash))
                .expect("user")
                .id,
        );
    }
    let app = test_app_with(state.clone());
    let alice = sign_in_user(&app, "alice", "alice-secret-1").await;
    let bob = sign_in_user(&app, "bob", "bob-secret-1").await;
    let (alice_client, alice_tokens) = sign_in(&app, &alice, Some("admin")).await;
    let (bob_client, bob_tokens) = sign_in(&app, &bob, Some("admin")).await;
    assert_eq!(
        alice_client, bob_client,
        "the same metadata, the same client"
    );
    let alice_access = alice_tokens["access_token"].as_str().expect("access");
    let bob_access = bob_tokens["access_token"].as_str().expect("access");
    for t in [alice_access, bob_access] {
        let resp = mcp(&app, Some(t), "tools/list", json!({})).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }
    // Bob revokes his own agent.
    let agent = oauth_agent_of(&app, &ids[1]).await;
    let resp = send(
        &app,
        "POST",
        &format!("/_rustybin/agents/{agent}/revoke"),
        &[("authorization", &format!("Bearer {bob}"))],
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = mcp(&app, Some(bob_access), "tools/list", json!({})).await;
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "his tokens are refused"
    );
    let (status, _) = refresh(
        &app,
        &bob_client,
        bob_tokens["refresh_token"].as_str().expect("refresh"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Alice's tokens of the same client still work.
    let resp = mcp(&app, Some(alice_access), "tools/list", json!({})).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "another user's agent is untouched"
    );
    let (status, v) = refresh(
        &app,
        &alice_client,
        alice_tokens["refresh_token"].as_str().expect("refresh"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    // Disabling Bob revokes his agents, never hers.
    state.users.set_disabled(&ids[1], true).expect("disable");
    state
        .agents
        .revoke_owned(&ids[1], "test", &state.config.console_tokens);
    let resp = mcp(&app, Some(alice_access), "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

/// The consent page shows what the agent gets in the approver's projects,
/// and a member of a project only can grant a scope that gives them
/// something there.
#[tokio::test]
async fn consent_shows_and_counts_the_approvers_project_grants() {
    let state = test_state_with(password_config());
    state
        .users
        .save_project("shop", None, None)
        .expect("project");
    let hash = crate::login::hash_password("erin-secret-1").expect("hash");
    let erin = state
        .users
        .add(NewUser::new("erin", &[]), Some(hash))
        .expect("user")
        .id;
    state
        .users
        .set_binding("shop", &erin, vec!["editor".into()])
        .expect("binding");
    let app = test_app_with(state.clone());
    let token = sign_in_user(&app, "erin", "erin-secret-1").await;
    let (_, id) = start(&app, Some("admin inspector")).await;
    let (status, req) = consent(&app, &id, &token).await;
    assert_eq!(status, StatusCode::OK, "{req}");
    let grantable = req["scopes_grantable"].to_string();
    assert!(grantable.contains("admin"), "{req}");
    assert!(
        grantable.contains("inspector"),
        "traffic.read in the project: {req}"
    );
    let projects = &req["grants"][0]["projects"];
    assert_eq!(projects[0]["project"], "shop", "{req}");
    let perms = projects[0]["permissions"].to_string();
    assert!(perms.contains("mocks.write"), "{req}");
    assert_eq!(req["projects"][0]["project"], "shop", "{req}");
    // Approved, the agent holds them in the project.
    let (status, ans) = answer(
        &app,
        &id,
        &token,
        json!({ "decision": "approve", "scope": "admin" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ans}");
}

async fn tool_call(app: &Router, token: &str, name: &str, args: Value) -> Value {
    let resp = mcp(
        app,
        Some(token),
        "tools/call",
        json!({ "name": name, "arguments": args }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp).await["result"].clone()
}

/// An OAuth agent of a user (another credential than their sign-in) cannot
/// undo the operator's revocation of the user's sign-in agent by revoking
/// it again and restoring it.
#[tokio::test]
async fn an_oauth_agent_cannot_undo_an_operator_revocation_through_the_tools() {
    let state = test_state_with(password_config());
    let hash = crate::login::hash_password("bob-secret-1").expect("hash");
    let bob_id = state
        .users
        .add(NewUser::new("bob", &["editor"]), Some(hash))
        .expect("user")
        .id;
    let app = test_app_with(state.clone());
    let bob = sign_in_user(&app, "bob", "bob-secret-1").await;
    // His sign-in agent, revoked by the operator.
    let resp = mcp(&app, Some(&bob), "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_json(
        send(
            &app,
            "GET",
            &format!("/_rustybin/agents?owner={bob_id}"),
            &[("authorization", &format!("Bearer {ADMIN}"))],
            None,
        )
        .await,
    )
    .await;
    let signin = list["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["credential"]["via"] == "sign-in")
        .and_then(|a| a["id"].as_str())
        .unwrap_or_else(|| panic!("{list}"))
        .to_string();
    let resp = send(
        &app,
        "POST",
        &format!("/_rustybin/agents/{signin}/revoke"),
        &[("authorization", &format!("Bearer {ADMIN}"))],
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    // His OAuth agent tries to bring it back.
    let fresh = sign_in_user(&app, "bob", "bob-secret-1").await;
    let (_, tokens) = sign_in(&app, &fresh, Some("admin")).await;
    let access = tokens["access_token"].as_str().expect("access");
    let r = tool_call(
        &app,
        access,
        "revoke_agent",
        json!({ "id": signin, "confirm": true }),
    )
    .await;
    assert_ne!(r["isError"], true, "{r}");
    let r = tool_call(&app, access, "restore_agent", json!({ "id": signin })).await;
    assert_eq!(r["isError"], true, "{r}");
    assert!(r.to_string().contains("agents.manage"), "{r}");
    let one = body_json(
        send(
            &app,
            "GET",
            &format!("/_rustybin/agents/{signin}"),
            &[("authorization", &format!("Bearer {ADMIN}"))],
            None,
        )
        .await,
    )
    .await;
    assert_eq!(one["revoked"]["by_owner"], false, "{one}");
}
