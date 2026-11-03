//! Step 14, the security pass (area A): the model and its enforcement.
//! Each test shows one finding of the review (`docs/plans/testing-2/
//! step-14-audit-log.md`, "Security pass build notes (area A)").

use axum::http::StatusCode;
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::{CustomRole, NewUser};
use crate::config::{Config, SelfServiceConfig};
use crate::control_auth::ControlAuth;
use crate::login::{hash_password, DEFAULT_PASSWORD, DEFAULT_USER};
use crate::test_support::{
    body_json, json_request, jwt_config, sign_control_jwt, test_app_with, test_state_with,
};

fn jwt(perms: &[&str]) -> String {
    sign_control_jwt(
        &json!({
            "aud": "test-instance",
            "exp": chrono::Utc::now().timestamp() + 600,
            "rb_perms": perms,
        }),
        false,
    )
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            axum::body::Body::from(v.to_string())
        }
        None => axum::body::Body::empty(),
    };
    let resp = app
        .clone()
        .oneshot(b.body(body).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22)
        .await
        .unwrap_or_default();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The permission a `403 permission_required` names, else `None`.
fn refused(status: StatusCode, v: &Value) -> Option<String> {
    (status == StatusCode::FORBIDDEN && v["reason"] == "permission_required")
        .then(|| v["permission"].as_str().unwrap_or("?").to_string())
}

fn password_config() -> Config {
    let mut c = Config::for_tests();
    c.control_auth = ControlAuth::Password;
    c
}

async fn token(app: &Router, user: &str, password: &str) -> String {
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            crate::login::LOGIN_PATH,
            &json!({ "username": user, "password": password }),
        ))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp).await["token"]
        .as_str()
        .expect("token")
        .to_string()
}

fn user_with(state: &crate::state::AppState, name: &str, roles: &[&str]) {
    let hash = hash_password(&format!("{name}-secret-1")).expect("hash");
    state
        .users
        .add(NewUser::new(name, roles), Some(hash))
        .expect("user");
}

fn role(state: &crate::state::AppState, name: &str, perms: &[&str]) {
    state
        .users
        .save_role(CustomRole {
            name: name.into(),
            description: String::new(),
            permissions: perms.iter().map(|p| p.to_string()).collect(),
        })
        .expect("role");
}

/// Finding M2: the source control settings (where the whole instance is
/// pushed, what a pull applies) need `git.write`, not `settings.write`
/// alone.
#[tokio::test]
async fn git_settings_need_git_write() {
    let app = test_app_with(test_state_with(jwt_config()));
    let body = json!({ "values": { "git.remote": "https://attacker.example/x.git", "git.auto": "push_on_change" } });
    let writer = jwt(&["settings.read", "settings.write"]);
    let (s, v) = send(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &writer,
        Some(body.clone()),
    )
    .await;
    assert_eq!(refused(s, &v).as_deref(), Some("git.write"), "{v}");
    let (s, v) = send(
        &app,
        "DELETE",
        "/_rustybin/settings/git.remote",
        &writer,
        None,
    )
    .await;
    assert_eq!(refused(s, &v).as_deref(), Some("git.write"), "{v}");
    let git = jwt(&["settings.read", "settings.write", "git.write"]);
    let (s, v) = send(&app, "PATCH", "/_rustybin/settings", &git, Some(body)).await;
    assert_eq!(refused(s, &v), None, "{s} {v}");
}

/// Finding M7: the single sign-on role map and default roles grant only
/// roles whose permissions the setter holds.
#[tokio::test]
async fn the_sso_role_map_grants_only_what_the_setter_holds() {
    let state = test_state_with(jwt_config());
    role(&state, "reader", &["settings.read"]);
    let app = test_app_with(state.clone());
    let security = jwt(&["settings.read", "settings.write", "settings.security"]);
    for values in [
        json!({ "sso.role_map": ["x=admin"] }),
        json!({ "sso.role_map": [{ "group": "x", "roles": ["admin"] }] }),
        json!({ "sso.default_roles": ["admin"] }),
        json!({ "sso.default_roles": "editor" }),
    ] {
        let (s, v) = send(
            &app,
            "PATCH",
            "/_rustybin/settings",
            &security,
            Some(json!({ "values": values })),
        )
        .await;
        assert!(refused(s, &v).is_some(), "{values}: {s} {v}");
    }
    // A role within the setter's permissions maps; unknown roles grant
    // nothing.
    let (s, v) = send(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &security,
        Some(json!({ "values": { "sso.role_map": ["x=reader", "y=no-such-role"] } })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // A document says the same.
    let apply = jwt(&[
        "settings.read",
        "settings.write",
        "settings.security",
        "config.apply",
    ]);
    let (s, v) = send(
        &app,
        "POST",
        "/_rustybin/config/apply",
        &apply,
        Some(json!({ "document": { "version": 2, "settings": { "sso.default_roles": ["admin"] } }, "dry_run": true })),
    )
    .await;
    assert!(refused(s, &v).is_some(), "{s} {v}");
}

/// Finding M3: ending a session needs what its user holds; signing out
/// everywhere needs every permission.
#[tokio::test]
async fn ending_a_session_needs_what_its_user_holds() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    role(&state, "helpdesk", &["users.read", "users.manage"]);
    user_with(&state, "hal", &["helpdesk"]);
    user_with(&state, "vic", &["viewer"]);
    let admin = token(&app, DEFAULT_USER, DEFAULT_PASSWORD).await;
    let hal = token(&app, "hal", "hal-secret-1").await;
    let _vic = token(&app, "vic", "vic-secret-1").await;
    let (_, list) = send(&app, "GET", "/_rustybin/sessions", &hal, None).await;
    let id_of = |user: &str| {
        list["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .find(|s| s["user"] == user)
            .and_then(|s| s["id"].as_str())
            .expect("listed")
            .to_string()
    };
    let admin_id = id_of(DEFAULT_USER);
    let vic_id = id_of("vic");
    let (s, v) = send(
        &app,
        "DELETE",
        &format!("/_rustybin/sessions/{admin_id}"),
        &hal,
        None,
    )
    .await;
    assert!(refused(s, &v).is_some(), "{s} {v}");
    let (s, v) = send(&app, "DELETE", "/_rustybin/sessions", &hal, None).await;
    assert!(refused(s, &v).is_some(), "{s} {v}");
    assert_eq!(
        send(&app, "GET", "/_rustybin/session", &admin, None)
            .await
            .0,
        StatusCode::OK,
        "the admin is still signed in"
    );
    // hal holds what a viewer holds? No: a viewer reads more than hal.
    let (s, v) = send(
        &app,
        "DELETE",
        &format!("/_rustybin/sessions/{vic_id}"),
        &hal,
        None,
    )
    .await;
    assert!(refused(s, &v).is_some(), "{s} {v}");
    // The admin ends both.
    let (s, v) = send(
        &app,
        "DELETE",
        &format!("/_rustybin/sessions/{vic_id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(
        (s, v["ended"].clone()),
        (StatusCode::OK, json!(true)),
        "{v}"
    );
    let (s, v) = send(&app, "DELETE", "/_rustybin/sessions", &admin, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

/// Finding M4: an export without `users.read` carries no user directory,
/// roles or projects (an editor holds `config.export` but not
/// `users.read`).
#[tokio::test]
async fn an_export_without_users_read_has_no_users() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    user_with(&state, "eddie", &["editor"]);
    let editor = token(&app, "eddie", "eddie-secret-1").await;
    let (s, v) = send(
        &app,
        "GET",
        "/_rustybin/config/export?format=json",
        &editor,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    for key in ["users", "roles", "projects"] {
        assert!(v.get(key).is_none(), "{key}: {v}");
    }
    let admin = token(&app, DEFAULT_USER, DEFAULT_PASSWORD).await;
    let (_, v) = send(
        &app,
        "GET",
        "/_rustybin/config/export?format=json",
        &admin,
        None,
    )
    .await;
    assert!(v["users"].as_array().is_some_and(|u| u.len() == 2), "{v}");
}

fn self_service_jwt_config() -> Config {
    let mut c = jwt_config();
    c.mocks_sample = true;
    c.self_service = SelfServiceConfig {
        enabled: true,
        host: Some("mocks.example".into()),
        leads_salt: Some("test-salt".into()),
        ..SelfServiceConfig::default()
    };
    c
}

/// Finding M5: on a self-service instance the visitor rules are for
/// visitors; a user or a control JWT is held to the route's permission.
#[tokio::test]
async fn self_service_holds_team_callers_to_the_route_permission() {
    let state = test_state_with(self_service_jwt_config());
    assert!(state.mocks.visits().is_some());
    let app = test_app_with(state.clone());
    let reader = jwt(&["mocks.read"]);
    let (s, v) = send(
        &app,
        "GET",
        "/_rustybin/mocks/petstore/requests",
        &reader,
        None,
    )
    .await;
    assert_eq!(refused(s, &v).as_deref(), Some("traffic.read"), "{v}");
    let writer = jwt(&["mocks.read", "mocks.write"]);
    let (s, v) = send(&app, "DELETE", "/_rustybin/mocks/petstore", &writer, None).await;
    assert_eq!(refused(s, &v).as_deref(), Some("mocks.delete"), "{v}");
    assert!(state.mocks.get("petstore").is_some());
    let portal_reader = jwt(&["portal.read"]);
    let (s, v) = send(&app, "GET", "/_rustybin/mocks", &portal_reader, None).await;
    assert_eq!(refused(s, &v).as_deref(), Some("mocks.read"), "{v}");
    // Who holds the permission still reaches the route.
    let traffic = jwt(&["mocks.read", "traffic.read"]);
    let (s, v) = send(
        &app,
        "GET",
        "/_rustybin/mocks/petstore/requests",
        &traffic,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

/// Finding M6: a document from the repository gives no role a permission
/// it did not grant, and creates no role single sign-on names.
#[test]
fn a_pull_never_widens_a_role() {
    let state = crate::test_support::test_state();
    role(&state, "ops", &["mocks.read"]);
    state
        .settings
        .update(
            [("sso.role_map".to_string(), json!(["ops-team=deployer"]))]
                .into_iter()
                .collect(),
            Default::default(),
            "test",
        )
        .expect("role map");
    let doc = crate::mocks::config_file::parse(
        "version: 2\nroles:\n  - name: ops\n    description: wider\n    permissions: [mocks.read, settings.write, config.apply]\n  - name: deployer\n    permissions: [mocks.write]\n  - name: newcomer\n    permissions: [mocks.read, tests.read]\n",
    )
    .expect("parse");
    let opts = crate::configdoc::ApplyOptions {
        prune: false,
        dry_run: false,
        principal: "git:test".into(),
        base: None,
        untrusted: true,
        caller: None,
        base_url: None,
    };
    let r = crate::configdoc::apply(&state, doc, &opts);
    let problems = r["problems"].to_string();
    assert!(problems.contains("roles.ops"), "{r}");
    assert!(problems.contains("roles.deployer"), "{r}");
    let ops = state
        .users
        .custom_roles()
        .into_iter()
        .find(|r| r.name == "ops")
        .expect("ops");
    assert_eq!(ops.permissions, vec!["mocks.read".to_string()]);
    let names: Vec<String> = state
        .users
        .custom_roles()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert!(!names.contains(&"deployer".to_string()), "{names:?}");
    assert!(
        names.contains(&"newcomer".to_string()),
        "a new role nothing names: {names:?}"
    );
    // Narrowing a role still applies.
    let doc = crate::mocks::config_file::parse(
        "version: 2\nroles:\n  - name: newcomer\n    permissions: [mocks.read]\n",
    )
    .expect("parse");
    let r = crate::configdoc::apply(&state, doc, &opts);
    assert!(!r["problems"].to_string().contains("roles.newcomer"), "{r}");
}
