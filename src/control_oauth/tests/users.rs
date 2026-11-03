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
