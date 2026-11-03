use super::*;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::Router;
use tower::ServiceExt;

use crate::config::Config;
use crate::control_auth::ControlAuth;
use crate::limits::ManualClock;
use crate::login::{hash_password, DEFAULT_PASSWORD, DEFAULT_USER, HASH_KEY, USER_KEY};
use crate::test_support::{
    bearer_request, body_json, json_request, sign_control_jwt, test_app_with, test_state_with,
};

// ── Permissions and roles ───────────────────────────────────────────

/// The whole table, in order: a change here is a deliberate one (and
/// belongs in the docs' table).
const TABLE: &[(&str, &str)] = &[
    ("instance.read", "Instance"),
    ("settings.read", "Instance"),
    ("settings.write", "Instance"),
    ("settings.security", "Instance"),
    ("secrets.write", "Instance"),
    ("mocks.read", "Mocks"),
    ("mocks.write", "Mocks"),
    ("mocks.delete", "Mocks"),
    ("mocks.data", "Mocks"),
    ("traffic.read", "Traffic"),
    ("traffic.clear", "Traffic"),
    ("tests.read", "Testing"),
    ("tests.write", "Testing"),
    ("tests.run", "Testing"),
    ("scenarios.run", "Testing"),
    ("checkpoints.write", "Testing"),
    ("checkpoints.restore", "Testing"),
    ("harness.read", "Load"),
    ("harness.run", "Load"),
    ("harness.cluster", "Load"),
    ("assist.use", "AI"),
    ("assist.jobs", "AI"),
    ("mcp.inspect", "MCP"),
    ("mcp.connections", "MCP"),
    ("registry.write", "MCP"),
    ("portal.read", "Portal"),
    ("portal.publish", "Portal"),
    ("portal.settings", "Portal"),
    ("config.export", "As code"),
    ("config.apply", "As code"),
    ("git.read", "As code"),
    ("git.write", "As code"),
    ("agents.read", "People"),
    ("agents.manage", "People"),
    ("users.read", "People"),
    ("users.manage", "People"),
    ("audit.read", "People"),
    ("visits.operate", "Self-service"),
];

#[test]
fn the_permission_table_is_complete_and_stable() {
    let got: Vec<(&str, &str)> = Permission::ALL
        .iter()
        .map(|p| (p.name(), p.group().title()))
        .collect();
    assert_eq!(got, TABLE);
    let mut names = std::collections::BTreeSet::new();
    for p in Permission::ALL {
        assert!(names.insert(p.name()), "{} twice", p.name());
        assert_eq!(Permission::parse(p.name()), Some(*p));
        assert!(!p.summary().is_empty() && !p.summary().ends_with('.'));
        let (prefix, verb) = p.name().split_once('.').expect("dotted");
        assert!(!prefix.is_empty() && !verb.is_empty());
        assert!(p
            .name()
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_'));
    }
    for g in Group::ALL {
        assert!(
            Permission::ALL.iter().any(|p| p.group() == *g),
            "{} is empty",
            g.title()
        );
    }
    assert_eq!(Permission::parse("mocks.everything"), None);
    assert_eq!(
        Permission::table().as_array().map(Vec::len),
        Some(TABLE.len())
    );
    // Every permission is granted by some role (admin holds them all; the
    // routes and tools that check each one come with step 08).
    for p in Permission::ALL {
        assert!(BuiltIn::ALL.iter().any(|r| r.permissions().has(*p)));
    }
}

fn names(r: BuiltIn) -> Vec<&'static str> {
    r.permissions().names()
}

#[test]
fn the_built_in_roles_have_their_sets() {
    let viewer = [
        "instance.read",
        "settings.read",
        "mocks.read",
        "tests.read",
        "harness.read",
        "portal.read",
        "git.read",
        "agents.read",
    ];
    assert_eq!(names(BuiltIn::Viewer), viewer);
    let tester = [
        "instance.read",
        "settings.read",
        "mocks.read",
        "traffic.read",
        "tests.read",
        "tests.run",
        "scenarios.run",
        "harness.read",
        "harness.run",
        "assist.use",
        "mcp.inspect",
        "portal.read",
        "git.read",
        "agents.read",
    ];
    assert_eq!(names(BuiltIn::Tester), tester);
    let editor = [
        "instance.read",
        "settings.read",
        "mocks.read",
        "mocks.write",
        "mocks.delete",
        "mocks.data",
        "traffic.read",
        "traffic.clear",
        "tests.read",
        "tests.write",
        "tests.run",
        "scenarios.run",
        "checkpoints.write",
        "checkpoints.restore",
        "harness.read",
        "harness.run",
        "assist.use",
        "assist.jobs",
        "mcp.inspect",
        "registry.write",
        "portal.read",
        "portal.publish",
        "config.export",
        "git.read",
        "agents.read",
    ];
    assert_eq!(names(BuiltIn::Editor), editor);
    assert_eq!(BuiltIn::Admin.permissions(), PermSet::all());
    assert_eq!(BuiltIn::Admin.permissions().len(), TABLE.len());
    assert_eq!(names(BuiltIn::PortalReader), ["portal.read"]);
    // Each role holds the one before it.
    assert!(BuiltIn::Tester
        .permissions()
        .contains(BuiltIn::Viewer.permissions()));
    assert!(BuiltIn::Editor
        .permissions()
        .contains(BuiltIn::Tester.permissions()));
    // Only admin is privileged among the built-in roles.
    let privileged: Vec<&str> = BuiltIn::ALL
        .iter()
        .filter(|r| role::is_privileged(r.permissions()))
        .map(|r| r.name())
        .collect();
    assert_eq!(privileged, ["admin"]);
    let names: Vec<&str> = BuiltIn::ALL.iter().map(|r| r.name()).collect();
    assert_eq!(
        names,
        ["viewer", "tester", "editor", "admin", "portal_reader"]
    );
}

fn custom(name: &str, perms: &[&str]) -> CustomRole {
    CustomRole {
        name: name.to_string(),
        description: String::new(),
        permissions: perms.iter().map(|p| p.to_string()).collect(),
    }
}

#[test]
fn custom_roles_are_validated_capped_and_marked() {
    let ok = custom("release-manager_2", &["mocks.read", "mocks.write"]);
    assert!(role::validate_custom(&ok).is_ok());
    let refused = |r: CustomRole, needle: &str| {
        let problems = role::validate_custom(&r).expect_err("refused");
        assert!(
            problems.iter().any(|p| p.contains(needle)),
            "{needle}: {problems:?}"
        );
    };
    refused(
        custom("ops", &["mocks.read", "mocks.everything"]),
        "mocks.everything is not a permission",
    );
    refused(custom("ops", &[]), "at least one permission");
    refused(custom("ops", &["mocks.read", "mocks.read"]), "twice");
    refused(custom("admin", &["mocks.read"]), "built-in");
    refused(custom("viewer", &["mocks.read"]), "built-in");
    refused(custom("Ops", &["mocks.read"]), "lower case");
    refused(custom("1ops", &["mocks.read"]), "starts with");
    refused(
        custom("ops team", &["mocks.read"]),
        "lower case letters, digits",
    );
    refused(custom("", &["mocks.read"]), "1 to 40");
    refused(custom(&"o".repeat(41), &["mocks.read"]), "1 to 40");
    let mut long = custom("ops", &["mocks.read"]);
    long.description = "d".repeat(201);
    refused(long, "description");

    // The store: built-in roles cannot be changed, custom ones are capped.
    let users = Users::in_memory();
    assert!(matches!(
        users.save_role(custom("editor", &["mocks.read"])),
        Err(UsersError::Invalid(_))
    ));
    for i in 0..role::MAX_CUSTOM_ROLES {
        users
            .save_role(custom(&format!("role-{i}"), &["mocks.read"]))
            .expect("saved");
    }
    assert!(matches!(
        users.save_role(custom("one-more", &["mocks.read"])),
        Err(UsersError::Full(_))
    ));
    // A change of an existing one is not one more.
    users
        .save_role(custom("role-0", &["mocks.read", "users.manage"]))
        .expect("changed");
    assert_eq!(users.custom_roles().len(), role::MAX_CUSTOM_ROLES);
    // Privileged roles are marked.
    let view = users.roles_view();
    let find = |n: &str| view.iter().find(|r| r["name"] == n).cloned().expect(n);
    assert_eq!(find("role-0")["privileged"], true);
    assert_eq!(find("role-1")["privileged"], false);
    assert_eq!(find("admin")["privileged"], true);
    assert_eq!(find("admin")["built_in"], true);
    assert_eq!(find("role-1")["built_in"], false);
    users
        .save_role(custom("role-2", &["settings.security"]))
        .expect("changed");
    assert!(role::is_privileged(
        users.role_permissions("role-2").expect("role")
    ));
    // A role in use is not deleted; a built-in one never.
    let u = users
        .add(NewUser::new("holder", &["role-1"]), None)
        .expect("user");
    assert!(matches!(
        users.delete_role("role-1"),
        Err(UsersError::Refused(users::ROLE_IN_USE, _))
    ));
    assert!(matches!(
        users.delete_role("viewer"),
        Err(UsersError::Invalid(_))
    ));
    assert!(matches!(
        users.delete_role("nope"),
        Err(UsersError::NotFound)
    ));
    users
        .set_roles(&u.id, vec!["viewer".into()])
        .expect("roles");
    users.delete_role("role-1").expect("deleted");
    // A user's roles must exist.
    assert!(matches!(
        users.set_roles(&u.id, vec!["role-1".into()]),
        Err(UsersError::Invalid(_))
    ));
}

// ── The old scopes ──────────────────────────────────────────────────

/// The 0.3 implementation of `Grant::from_scope_claim`, kept as the oracle.
fn grant_03(claim: Option<&Value>) -> Grant {
    let names: Vec<String> = match claim {
        None | Some(Value::Null) => vec!["console".to_string()],
        Some(Value::String(s)) => s.split_whitespace().map(str::to_string).collect(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(_) => Vec::new(),
    };
    let has = |n: &str| names.iter().any(|s| s == n);
    let admin = has("admin");
    Grant {
        admin,
        console: admin || has("console"),
        inspector: admin || has("console") || has("inspector"),
        harness: admin || has("harness"),
    }
}

#[test]
fn the_old_scopes_keep_their_grants() {
    let scopes = ["admin", "console", "inspector", "harness", "unknown"];
    let mut claims = vec![None, Some(Value::Null), Some(json!(42)), Some(json!(""))];
    for mask in 0u32..(1 << scopes.len()) {
        let chosen: Vec<&str> = scopes
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, s)| *s)
            .collect();
        claims.push(Some(json!(chosen.join(" "))));
        claims.push(Some(json!(chosen)));
    }
    for claim in &claims {
        assert_eq!(
            Grant::from_scope_claim(claim.as_ref()),
            grant_03(claim.as_ref()),
            "{claim:?}"
        );
    }
    // The scopes as roles.
    let perms = |s: &str| from_scope_claim(Some(&json!(s))).1;
    assert_eq!(perms("admin"), PermSet::all());
    // The console scope keeps its 0.4 reach: the viewer plus captured
    // traffic, the assist's read tasks and the MCP inspector.
    let console = BuiltIn::Viewer
        .permissions()
        .union(PermSet::of(super::CONSOLE_SCOPE_EXTRA));
    assert_eq!(perms("console"), console);
    assert_eq!(
        PermSet::of(super::CONSOLE_SCOPE_EXTRA).names(),
        ["traffic.read", "assist.use", "mcp.inspect"]
    );
    assert_eq!(perms("inspector").names(), ["traffic.read"]);
    assert_eq!(
        perms("harness").names(),
        ["harness.read", "harness.run", "harness.cluster"]
    );
    assert_eq!(from_scope_claim(None).1, console);
    assert_eq!(from_scope_claim(None).0, ["viewer"]);
    assert_eq!(
        from_scope_claim(Some(&json!("admin console"))).0,
        ["admin", "viewer"]
    );
    // The admin token and an open control plane are every permission.
    assert_eq!(Resolved::admin_token().grant(), Grant::ADMIN);
    assert!(Resolved::open().permissions.is_all());
}

#[test]
fn rb_roles_and_rb_perms_join_the_scope_and_unknown_names_are_ignored() {
    let users = Users::in_memory();
    users
        .save_role(custom("mock-builder", &["mocks.write", "mocks.delete"]))
        .expect("role");
    // rb_roles with a scope: both count.
    let r = from_jwt_claims(
        &json!({"sub": "svc", "scope": "inspector", "rb_roles": ["mock-builder", "no-such-role-x1"]}),
        &users,
    );
    assert_eq!(r.roles, ["mock-builder"]);
    assert_eq!(
        r.permissions.names(),
        ["mocks.write", "mocks.delete", "traffic.read"]
    );
    assert!(was_noted("role", "no-such-role-x1"));
    assert_eq!(
        r.principal,
        Principal::Token {
            sub: Some("svc".into())
        }
    );
    // rb_perms, a space separated string; an unknown name is ignored.
    let r = from_jwt_claims(
        &json!({"rb_perms": "tests.run traffic.read nope.never-x2"}),
        &users,
    );
    assert_eq!(r.permissions.names(), ["traffic.read", "tests.run"]);
    assert!(r.roles.is_empty(), "no console default beside rb_*");
    assert!(was_noted("permission", "nope.never-x2"));
    // A built-in role by name, with no scope: exactly that role.
    let r = from_jwt_claims(&json!({"rb_roles": "admin"}), &users);
    assert!(r.permissions.is_all());
    assert_eq!(r.grant(), Grant::ADMIN);
    // Neither: the scope alone, with its console default.
    let r = from_jwt_claims(&json!({"sub": "x"}), &users);
    assert_eq!(r.permissions, from_scope_claim(Some(&json!("console"))).1);
    // Junk shapes grant nothing and never fail.
    let r = from_jwt_claims(&json!({"rb_roles": 7, "rb_perms": {"a": 1}}), &users);
    assert!(r.permissions.is_empty());
}

fn jwt(claims: Value) -> String {
    let mut c = claims;
    c["aud"] = json!("test-instance");
    c["exp"] = json!(chrono::Utc::now().timestamp() + 600);
    sign_control_jwt(&c, false)
}

async fn call(app: &Router, method: &str, uri: &str, token: Option<&str>) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(bearer_request(method, uri, token))
        .await
        .expect("response");
    let status = resp.status();
    (status, body_json(resp).await)
}

#[tokio::test]
async fn rb_claims_on_a_control_jwt_reach_the_grant_and_the_session() {
    let app = test_app_with(test_state_with(crate::test_support::jwt_config()));
    // rb_roles admin without a scope: a mutation is allowed.
    let admin = jwt(json!({"sub": "platform", "rb_roles": ["admin", "future-role-x3"]}));
    assert_eq!(
        call(&app, "DELETE", "/_rustybin/requests", Some(&admin))
            .await
            .0,
        StatusCode::OK
    );
    // rb_perms traffic.read alone: the captures, nothing else.
    let traffic = jwt(json!({"sub": "reader", "rb_perms": ["traffic.read", "future.perm-x4"]}));
    assert_eq!(
        call(&app, "GET", "/_rustybin/requests", Some(&traffic))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/_rustybin/config", Some(&traffic))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // The session answers any caller the control plane accepts (0.5).
    let (status, v) = call(&app, "GET", crate::login::SESSION_PATH, Some(&traffic)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["permissions"], json!(["traffic.read"]));
    let tester = jwt(json!({"sub": "ci", "rb_roles": "tester"}));
    let (status, v) = call(&app, "GET", crate::login::SESSION_PATH, Some(&tester)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["obtained"], "control_jwt");
    assert_eq!(v["sub"], "ci");
    assert_eq!(v["roles"], json!(["tester"]));
    assert_eq!(v["permissions"], json!(names(BuiltIn::Tester)));
    assert_eq!(v["scopes"], json!(["console", "inspector", "harness"]));
    assert_eq!(v["user_id"], Value::Null);
    // A console-scoped token: the viewer role, plus the scope's 0.4 extras.
    let console = crate::test_support::control_jwt(Some("console"));
    let (_, v) = call(&app, "GET", crate::login::SESSION_PATH, Some(&console)).await;
    assert_eq!(v["roles"], json!(["viewer"]));
    assert_eq!(
        v["permissions"],
        json!(from_scope_claim(Some(&json!("console"))).1.names())
    );
    // The admin token: every permission, no user.
    let (_, v) = call(
        &app,
        "GET",
        crate::login::SESSION_PATH,
        Some("admin-secret"),
    )
    .await;
    assert_eq!(v["obtained"], "admin_token");
    assert_eq!(v["roles"], json!(["admin"]));
    assert_eq!(v["permissions"].as_array().map(Vec::len), Some(TABLE.len()));
    assert_eq!(v["user_id"], Value::Null);
}

// ── Sign-in, users and resolution ───────────────────────────────────

fn password_config() -> Config {
    let mut c = Config::for_tests();
    c.control_auth = ControlAuth::Password;
    c
}

async fn login(app: &Router, user: &str, password: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            crate::login::LOGIN_PATH,
            &json!({ "username": user, "password": password }),
        ))
        .await
        .expect("response");
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn token(app: &Router, user: &str, password: &str) -> String {
    let (status, v) = login(app, user, password).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["token"].as_str().expect("token").to_string()
}

#[tokio::test]
async fn the_console_user_is_user_one_and_the_session_says_who() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    let first = state.users.first_admin().expect("user one");
    assert_eq!(first.name, DEFAULT_USER);
    assert_eq!(first.roles, ["admin"]);
    assert!(users::is_user_id(&first.id), "{}", first.id);
    let (status, v) = login(&app, DEFAULT_USER, DEFAULT_PASSWORD).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["user_id"], first.id);
    assert_eq!(
        v["scopes"],
        json!(["admin", "console", "inspector", "harness"])
    );
    let t = v["token"].as_str().expect("token").to_string();
    // The token: the user id, the sign-in name, both generations.
    let claims = crate::control_auth::unverified_claims(&t).expect("claims");
    assert_eq!(claims["sub"], first.id);
    assert_eq!(claims["rb_user"], DEFAULT_USER);
    assert_eq!(claims["rb_ugen"], 1);
    assert_eq!(claims["rb_gen"], state.config.console_tokens.generation());
    assert_eq!(claims["scope"], "admin");
    // The session answer.
    let (status, v) = call(&app, "GET", crate::login::SESSION_PATH, Some(&t)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["obtained"], "password");
    assert_eq!(v["user"], DEFAULT_USER);
    assert_eq!(v["user_id"], first.id);
    assert_eq!(v["display_name"], DEFAULT_USER);
    assert_eq!(v["roles"], json!(["admin"]));
    assert_eq!(v["permissions"].as_array().map(Vec::len), Some(TABLE.len()));
    assert!(!v.to_string().contains("argon2"));
    // The sign-in is noted; the sessions list names the user id.
    assert!(state
        .users
        .get(&first.id)
        .and_then(|u| u.last_sign_in)
        .is_some());
    let (_, v) = call(&app, "GET", crate::login::SESSIONS_PATH, Some(&t)).await;
    assert_eq!(v["sessions"][0]["user_id"], first.id);
}

#[tokio::test]
async fn a_role_change_and_a_disable_apply_on_the_next_request() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    let first = state.users.first_admin().expect("user one");
    // Another admin, so user one may be demoted and disabled (the guard
    // rails keep one enabled admin).
    state
        .users
        .add(NewUser::new("second-admin", &["admin"]), None)
        .expect("user");
    let t = token(&app, DEFAULT_USER, DEFAULT_PASSWORD).await;
    assert_eq!(
        call(&app, "DELETE", "/_rustybin/requests", Some(&t))
            .await
            .0,
        StatusCode::OK
    );
    // Demoted to viewer: reads still work, mutations do not, at once.
    state
        .users
        .set_roles(&first.id, vec!["viewer".into()])
        .expect("roles");
    assert_eq!(
        call(&app, "GET", "/_rustybin/config", Some(&t)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "DELETE", "/_rustybin/requests", Some(&t))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (_, v) = call(&app, "GET", crate::login::SESSION_PATH, Some(&t)).await;
    assert_eq!(v["roles"], json!(["viewer"]));
    assert_eq!(v["scopes"], json!(["console", "inspector"]));
    // Admin again.
    state
        .users
        .set_roles(&first.id, vec!["admin".into()])
        .expect("roles");
    assert_eq!(
        call(&app, "DELETE", "/_rustybin/requests", Some(&t))
            .await
            .0,
        StatusCode::OK
    );
    // Disabled: the token is refused, the sign-in answers as a wrong
    // password; enabled again, the old token stays signed out.
    state.users.set_disabled(&first.id, true).expect("disable");
    let (status, v) = call(&app, "GET", "/_rustybin/config", Some(&t)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(v["reason"], "revoked");
    let disabled = login(&app, DEFAULT_USER, DEFAULT_PASSWORD).await;
    let wrong = login(&app, DEFAULT_USER, "not-the-password").await;
    assert_eq!(disabled.0, StatusCode::UNAUTHORIZED);
    assert_eq!(disabled, wrong);
    state.users.set_disabled(&first.id, false).expect("enable");
    assert_eq!(
        call(&app, "GET", "/_rustybin/config", Some(&t)).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Signed out everywhere, one user at a time.
    let t = token(&app, DEFAULT_USER, DEFAULT_PASSWORD).await;
    assert_eq!(
        call(&app, "GET", "/_rustybin/config", Some(&t)).await.0,
        StatusCode::OK
    );
    state.users.rotate_generation(&first.id).expect("rotate");
    assert_eq!(
        call(&app, "GET", "/_rustybin/config", Some(&t)).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn another_user_signs_in_with_their_own_password_and_role() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    let hash = hash_password("tess-secret-1").expect("hash");
    let tess = state
        .users
        .add(NewUser::new("tess", &["tester"]), Some(hash))
        .expect("user");
    // Never the hash in a user.
    assert!(!serde_json::to_string(&state.users.list())
        .expect("json")
        .contains("argon2"));
    assert!(!tess.view().to_string().contains("argon2"));
    let t = token(&app, "tess", "tess-secret-1").await;
    let (_, v) = call(&app, "GET", crate::login::SESSION_PATH, Some(&t)).await;
    assert_eq!(v["user"], "tess");
    assert_eq!(v["user_id"], tess.id);
    assert_eq!(v["roles"], json!(["tester"]));
    assert_eq!(v["permissions"], json!(names(BuiltIn::Tester)));
    assert_eq!(
        call(&app, "GET", "/_rustybin/requests", Some(&t)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "DELETE", "/_rustybin/requests", Some(&t))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // Wrong password, the first admin's password, no password of their own.
    assert_eq!(
        login(&app, "tess", DEFAULT_PASSWORD).await.0,
        StatusCode::UNAUTHORIZED
    );
    let nopass = state
        .users
        .add(NewUser::new("nopass", &["viewer"]), None)
        .expect("user");
    assert_eq!(
        login(&app, &nopass.name, "").await.0,
        StatusCode::UNAUTHORIZED
    );
    // Disabled: the same answer as a wrong password.
    state.users.set_disabled(&tess.id, true).expect("disable");
    let disabled = login(&app, "tess", "tess-secret-1").await;
    assert_eq!(disabled, login(&app, "tess", "wrong-one-2").await);
    // A portal reader is no console user: the control plane refuses reads.
    let hash = hash_password("pat-secret-1").expect("hash");
    state
        .users
        .add(NewUser::new("pat", &["portal_reader"]), Some(hash))
        .expect("user");
    let t = token(&app, "pat", "pat-secret-1").await;
    assert_eq!(
        call(&app, "GET", "/_rustybin/config", Some(&t)).await.0,
        StatusCode::FORBIDDEN
    );
}

/// Sign a token the way 0.3 did: `sub: "user:<name>"`, no user id, no
/// user generation.
fn token_03(config: &Config, user: &str) -> String {
    let iat = chrono::Utc::now().timestamp();
    let claims = json!({
        "iss": "rustybin",
        "aud": config.control_jwt_audience,
        "sub": format!("user:{user}"),
        "scope": "admin",
        "iat": iat,
        "exp": iat + 3600,
        "jti": "0123456789abcdef0123456789abcdef",
        "rb_use": "console",
        "rb_gen": config.console_tokens.generation(),
    });
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
    header.typ = Some("JWT".into());
    jsonwebtoken::encode(
        &header,
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(&config.control_oauth_key.console_secret()),
    )
    .expect("sign")
}

#[tokio::test]
async fn a_03_token_still_signs_in_as_user_one() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    let first = state.users.first_admin().expect("user one");
    // Another admin, so user one may be demoted (the guard rails keep one).
    state
        .users
        .add(NewUser::new("second-admin", &["admin"]), None)
        .expect("user");
    let old = token_03(&state.config, DEFAULT_USER);
    assert_eq!(
        call(&app, "DELETE", "/_rustybin/requests", Some(&old))
            .await
            .0,
        StatusCode::OK
    );
    let (_, v) = call(&app, "GET", crate::login::SESSION_PATH, Some(&old)).await;
    assert_eq!(v["user_id"], first.id);
    assert_eq!(v["user"], DEFAULT_USER);
    assert_eq!(v["roles"], json!(["admin"]));
    // The user's own rules apply to it: a demotion, a sign-out everywhere.
    state
        .users
        .set_roles(&first.id, vec!["viewer".into()])
        .expect("roles");
    assert_eq!(
        call(&app, "DELETE", "/_rustybin/requests", Some(&old))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    state.users.rotate_generation(&first.id).expect("rotate");
    assert_eq!(
        call(&app, "GET", "/_rustybin/config", Some(&old)).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[test]
fn the_resolution_cache_lasts_a_second_and_a_change_drops_it() {
    let clock = Arc::new(ManualClock::new(chrono::Utc::now()));
    let users = Users::with_clock(clock.clone());
    let u = users
        .add(NewUser::new("cache", &["viewer"]), None)
        .expect("user");
    let before = users.cache_misses();
    let a = users.resolve(&u.id).expect("resolved");
    let b = users.resolve(&u.id).expect("resolved");
    assert!(Arc::ptr_eq(&a, &b), "the second came from the cache");
    assert_eq!(users.cache_misses(), before + 1);
    clock.advance(Duration::from_millis(999));
    users.resolve(&u.id).expect("resolved");
    assert_eq!(users.cache_misses(), before + 1);
    clock.advance(Duration::from_millis(1));
    users.resolve(&u.id).expect("resolved");
    assert_eq!(
        users.cache_misses(),
        before + 2,
        "a second later it is resolved again"
    );
    // A change is seen at once, cache or not.
    users
        .set_roles(&u.id, vec!["editor".into()])
        .expect("roles");
    let c = users.resolve(&u.id).expect("resolved");
    assert_eq!(c.permissions, BuiltIn::Editor.permissions());
    assert_eq!(users.cache_misses(), before + 3);
    // A custom role's change too.
    users
        .save_role(custom("narrow", &["mocks.read"]))
        .expect("role");
    users
        .set_roles(&u.id, vec!["narrow".into()])
        .expect("roles");
    users.resolve(&u.id).expect("resolved");
    users
        .save_role(custom("narrow", &["mocks.read", "mocks.write"]))
        .expect("role");
    assert!(users
        .resolve(&u.id)
        .expect("resolved")
        .permissions
        .has(Permission::MocksWrite));
    assert!(users.resolve("usr_0000000000000000").is_none());
}

#[test]
fn the_store_is_capped_and_validates_users() {
    let users = Users::in_memory();
    for i in 0..users::MAX_USERS {
        users
            .add(NewUser::new(&format!("u{i}"), &["viewer"]), None)
            .expect("user");
    }
    assert!(matches!(
        users.add(NewUser::new("one-more", &["viewer"]), None),
        Err(UsersError::Full(_))
    ));
    assert_eq!(users.len(), users::MAX_USERS);
    let users = Users::in_memory();
    users
        .add(NewUser::new("ana", &["viewer"]), None)
        .expect("user");
    assert!(matches!(
        users.add(NewUser::new("ANA", &["viewer"]), None),
        Err(UsersError::Conflict(_))
    ));
    assert!(matches!(
        users.add(NewUser::new("no spaces", &["viewer"]), None),
        Err(UsersError::Invalid(_))
    ));
    assert!(matches!(
        users.add(NewUser::new("bo", &["superuser"]), None),
        Err(UsersError::Invalid(_))
    ));
    let mut bad = NewUser::new("cy", &["viewer"]);
    bad.email = Some("not an email".into());
    assert!(matches!(users.add(bad, None), Err(UsersError::Invalid(_))));
    assert!(matches!(
        users.add(NewUser::new("di", &["viewer"]), Some("plain".into())),
        Err(UsersError::Invalid(_))
    ));
    assert!(matches!(
        users.set_roles("usr_ffffffffffffffff", vec![]),
        Err(UsersError::NotFound)
    ));
    assert_eq!(users.len(), 1);
}

#[cfg(unix)]
#[test]
fn the_file_is_private_and_a_failed_write_changes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tmp");
    let users = Users::persistent(Some(dir.path()));
    assert!(users.persisted());
    let hash = hash_password("file-secret-1").expect("hash");
    let u = users
        .add(NewUser::new("filed", &["editor"]), Some(hash))
        .expect("user");
    let path = dir.path().join(users::USERS_FILE);
    let mode = std::fs::metadata(&path).expect("file").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    // A restart reads it back, the password included.
    let again = Users::persistent(Some(dir.path()));
    assert_eq!(
        again.get(&u.id).map(|x| x.roles),
        Some(vec!["editor".into()])
    );
    assert!(again.password_hash(&u.id).is_some());
    // The file cannot be replaced (a directory took its name): the change
    // is refused and nothing changes.
    std::fs::remove_file(&path).expect("rm");
    std::fs::create_dir(&path).expect("dir");
    let err = users
        .add(NewUser::new("lost", &["viewer"]), None)
        .expect_err("not saved");
    assert!(matches!(err, UsersError::Io(_)), "{err:?}");
    assert!(users.find_by_name("lost").is_none());
    assert!(matches!(
        users.set_roles(&u.id, vec!["viewer".into()]),
        Err(UsersError::Io(_))
    ));
    assert_eq!(
        users.get(&u.id).map(|x| x.roles),
        Some(vec!["editor".into()])
    );
    assert!(users.write_error().is_some());
    // Writable again: the next change goes through and clears the error.
    std::fs::remove_dir(&path).expect("rmdir");
    users
        .add(NewUser::new("found", &["viewer"]), None)
        .expect("saved");
    assert!(users.write_error().is_none());
    assert!(Users::persistent(Some(dir.path()))
        .find_by_name("found")
        .is_some());
}

#[test]
fn an_unreadable_file_is_never_written_over() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join(users::USERS_FILE);
    std::fs::write(&path, b"{ not json").expect("write");
    let users = Users::persistent(Some(dir.path()));
    assert!(!users.persisted());
    assert!(users.load_error().is_some());
    assert!(users.ensure_first_admin("rustybin"));
    assert!(users.first_admin().is_some());
    assert_eq!(std::fs::read(&path).expect("file"), b"{ not json");
}

// ── Migration ───────────────────────────────────────────────────────

fn state_in(dir: &std::path::Path, configure: impl Fn(&mut Config)) -> crate::state::AppState {
    let mut c = password_config();
    c.state_dir = Some(dir.to_path_buf());
    c.users = Arc::new(Users::persistent(Some(dir)));
    configure(&mut c);
    test_state_with(c)
}

#[tokio::test]
async fn a_03_state_directory_becomes_user_one() {
    let dir = tempfile::tempdir().expect("tmp");
    // 0.3: the console user renamed and its password changed, kept in the
    // settings; no users.json.
    {
        let mut c = password_config();
        c.state_dir = Some(dir.path().to_path_buf());
        let settings = crate::settings::Settings::new(&c);
        let mut values = std::collections::BTreeMap::new();
        values.insert(USER_KEY.to_string(), json!("ops"));
        let mut secrets = std::collections::BTreeMap::new();
        secrets.insert(
            HASH_KEY.to_string(),
            Some(hash_password("ops-secret-1").expect("hash")),
        );
        settings
            .update_sign_in(values, secrets, "test")
            .expect("0.3 settings");
    }
    assert!(!dir.path().join(users::USERS_FILE).exists());
    let state = state_in(dir.path(), |_| {});
    let first = state.users.first_admin().expect("user one");
    assert_eq!(first.name, "ops");
    assert_eq!(first.roles, ["admin"]);
    assert!(dir.path().join(users::USERS_FILE).exists());
    // The settings stay readable for a downgrade.
    assert!(state.settings.secret(HASH_KEY).is_some());
    assert_eq!(crate::login::effective_user(&state.settings), "ops");
    let app = test_app_with(state.clone());
    let t = token(&app, "ops", "ops-secret-1").await;
    let (_, v) = call(&app, "GET", crate::login::SESSION_PATH, Some(&t)).await;
    assert_eq!(v["user_id"], first.id);
    assert_eq!(
        login(&app, DEFAULT_USER, DEFAULT_PASSWORD).await.0,
        StatusCode::UNAUTHORIZED
    );
    // A restart keeps user one (same id), no second user.
    let again = state_in(dir.path(), |_| {});
    assert_eq!(
        again.users.first_admin().map(|u| u.id),
        Some(first.id.clone())
    );
    assert_eq!(again.users.len(), 1);
    // A rename through the sign-in settings follows into the store.
    let mut values = std::collections::BTreeMap::new();
    values.insert(USER_KEY.to_string(), json!("ops2"));
    again
        .settings
        .update_sign_in(values, Default::default(), "test")
        .expect("renamed");
    assert_eq!(
        again.users.first_admin().map(|u| u.name),
        Some("ops2".into())
    );
}

#[tokio::test]
async fn the_environment_seeds_user_one_on_every_start() {
    let dir = tempfile::tempdir().expect("tmp");
    let state = state_in(dir.path(), |c| {
        c.console_user = Some("envadmin".into());
        c.console_password = Some("env-secret-1".into());
        c.public_mode = true;
    });
    assert!(crate::login::check_startup(&state).is_ok());
    let first = state.users.first_admin().expect("user one");
    assert_eq!(first.name, "envadmin");
    let app = test_app_with(state.clone());
    token(&app, "envadmin", "env-secret-1").await;
    // The next start with another user name in the environment: the same
    // user one, renamed.
    let state = state_in(dir.path(), |c| {
        c.console_user = Some("envadmin2".into());
        c.console_password = Some("env-secret-2".into());
    });
    let again = state.users.first_admin().expect("user one");
    assert_eq!(again.id, first.id);
    assert_eq!(again.name, "envadmin2");
    let app = test_app_with(state.clone());
    token(&app, "envadmin2", "env-secret-2").await;
    assert_eq!(
        login(&app, "envadmin", "env-secret-1").await.0,
        StatusCode::UNAUTHORIZED
    );
    // The default password rule applies to user one.
    let dir = tempfile::tempdir().expect("tmp");
    let state = state_in(dir.path(), |c| c.public_mode = true);
    assert!(state.users.first_admin().is_some());
    assert!(crate::login::check_startup(&state).is_err());
}

#[test]
fn other_modes_have_no_users_and_the_diagnostics_check_the_store() {
    let state = test_state_with(Config::for_tests());
    assert!(state.users.is_empty());
    assert!(diagnostics(&state).is_none());
    // Password mode without a state directory: in memory, a warning.
    let state = test_state_with(password_config());
    let (status, message, fix) = diagnostics(&state).expect("check");
    assert_eq!(status, "warn");
    assert!(message.contains("in memory"), "{message}");
    assert!(fix.is_some());
    let report = crate::diagnostics::report(&state);
    assert!(report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .any(|c| c["id"] == "users" && c["status"] == "warn"));
    let dir = tempfile::tempdir().expect("tmp");
    let state = state_in(dir.path(), |_| {});
    assert_eq!(diagnostics(&state).map(|d| d.0), Some("ok"));
}

// ── Step 08: every route checks a permission ────────────────────────

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
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap_or_default();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// `Ok` when the route let the caller through, else the permission the
/// 403 named.
async fn reach(
    app: &Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> Result<StatusCode, String> {
    let (status, v) = send(app, method, uri, token, body).await;
    if status == StatusCode::FORBIDDEN && v["reason"] == "permission_required" {
        // Never what the caller holds.
        assert!(
            v.get("permissions").is_none() && v.get("scopes").is_none(),
            "{v}"
        );
        return Err(v["permission"].as_str().unwrap_or("?").to_string());
    }
    assert_ne!(status, StatusCode::UNAUTHORIZED, "{method} {uri}: {v}");
    Ok(status)
}

fn user_with(state: &crate::state::AppState, name: &str, roles: &[&str]) -> String {
    let hash = hash_password(&format!("{name}-secret-1")).expect("hash");
    state
        .users
        .add(NewUser::new(name, roles), Some(hash))
        .expect("user")
        .id
}

#[tokio::test]
async fn a_viewer_a_tester_and_an_editor_reach_what_their_roles_grant() {
    let mut config = password_config();
    config.harness.enabled = true;
    config.harness.roles = vec![crate::harness::Role::Client];
    config.harness.targets = vec!["127.0.0.1".into()];
    let state = test_state_with(config);
    let app = test_app_with(state.clone());
    let viewer_id = user_with(&state, "vera", &["viewer"]);
    user_with(&state, "tess", &["tester"]);
    user_with(&state, "eddy", &["editor"]);
    let viewer = token(&app, "vera", "vera-secret-1").await;
    let tester = token(&app, "tess", "tess-secret-1").await;
    let editor = token(&app, "eddy", "eddy-secret-1").await;
    let none = json!({});
    // (method, route, body, viewer, tester, editor): None reaches the
    // handler, Some(p) is refused naming p.
    type Row<'a> = (&'a str, &'a str, Option<Value>, [Option<&'a str>; 3]);
    let rows: Vec<Row> = vec![
        ("GET", "/_rustybin/config", None, [None, None, None]),
        ("GET", "/_rustybin/session", None, [None, None, None]),
        ("GET", "/_rustybin/mocks", None, [None, None, None]),
        (
            "POST",
            "/_rustybin/mocks",
            Some(none.clone()),
            [Some("mocks.write"), Some("mocks.write"), None],
        ),
        (
            "POST",
            "/_rustybin/mocks/petstore/disable",
            None,
            [Some("mocks.write"), Some("mocks.write"), None],
        ),
        (
            "DELETE",
            "/_rustybin/mocks/petstore/state",
            None,
            [Some("mocks.data"), Some("mocks.data"), None],
        ),
        (
            "POST",
            "/_rustybin/mocks/reload",
            None,
            [
                Some("config.apply"),
                Some("config.apply"),
                Some("config.apply"),
            ],
        ),
        (
            "GET",
            "/_rustybin/requests",
            None,
            [Some("traffic.read"), None, None],
        ),
        (
            "GET",
            "/_rustybin/mocks/petstore/requests",
            None,
            [Some("traffic.read"), None, None],
        ),
        ("GET", "/_rustybin/settings", None, [None, None, None]),
        (
            "PATCH",
            "/_rustybin/settings",
            Some(json!({"values": {}})),
            [
                Some("settings.write"),
                Some("settings.write"),
                Some("settings.write"),
            ],
        ),
        ("GET", "/_rustybin/tests", None, [None, None, None]),
        (
            "POST",
            "/_rustybin/tests",
            Some(none.clone()),
            [Some("tests.write"), Some("tests.write"), None],
        ),
        (
            "POST",
            "/_rustybin/tests/run",
            Some(json!({"names": ["none-x"]})),
            [Some("tests.run"), None, None],
        ),
        ("GET", "/_rustybin/checkpoints", None, [None, None, None]),
        (
            "POST",
            "/_rustybin/checkpoints",
            Some(none.clone()),
            [Some("checkpoints.write"), Some("checkpoints.write"), None],
        ),
        (
            "POST",
            "/_rustybin/checkpoints/none-x/restore",
            Some(none.clone()),
            [
                Some("checkpoints.restore"),
                Some("checkpoints.restore"),
                None,
            ],
        ),
        ("GET", "/_rustybin/git", None, [None, None, None]),
        (
            "POST",
            "/_rustybin/git/commit",
            Some(none.clone()),
            [Some("git.write"), Some("git.write"), Some("git.write")],
        ),
        ("GET", "/_rustybin/agents", None, [None, None, None]),
        (
            "POST",
            "/_rustybin/agents/none-x/revoke",
            None,
            [
                Some("agents.manage"),
                Some("agents.manage"),
                Some("agents.manage"),
            ],
        ),
        (
            "GET",
            "/_rustybin/config/export",
            None,
            [Some("config.export"), Some("config.export"), None],
        ),
        (
            "POST",
            "/_rustybin/config/apply",
            Some(none.clone()),
            [
                Some("config.apply"),
                Some("config.apply"),
                Some("config.apply"),
            ],
        ),
        ("GET", "/_rustybin/harness", None, [None, None, None]),
        (
            "POST",
            "/_rustybin/harness/runs",
            Some(none.clone()),
            [Some("harness.run"), None, None],
        ),
        (
            "POST",
            "/_rustybin/harness/clusters",
            Some(none.clone()),
            [
                Some("harness.cluster"),
                Some("harness.cluster"),
                Some("harness.cluster"),
            ],
        ),
        (
            "POST",
            "/_rustybin/scenarios/caching/runs",
            None,
            [Some("scenarios.run"), None, None],
        ),
        (
            "PUT",
            "/_rustybin/portal/items/llm",
            Some(none.clone()),
            [Some("portal.publish"), Some("portal.publish"), None],
        ),
        (
            "GET",
            "/_rustybin/sessions",
            None,
            [Some("users.read"), Some("users.read"), Some("users.read")],
        ),
        ("GET", "/_rustybin/diagnostics", None, [None, None, None]),
        (
            "GET",
            "/_rustybin/mcp-servers",
            None,
            [Some("mcp.inspect"), None, None],
        ),
        (
            "PUT",
            "/_rustybin/mcp-connections/none-x",
            Some(none.clone()),
            [
                Some("mcp.connections"),
                Some("mcp.connections"),
                Some("mcp.connections"),
            ],
        ),
        // A body check: clearing everyone's captures needs traffic.clear.
        (
            "DELETE",
            "/_rustybin/requests",
            None,
            [Some("traffic.read"), Some("traffic.clear"), None],
        ),
        (
            "DELETE",
            "/_rustybin/requests?session=mine-1",
            None,
            [Some("traffic.read"), None, None],
        ),
        (
            "POST",
            "/_rustybin/assist/run",
            Some(json!({"task": "explain", "inputs": {}})),
            [Some("assist.use"), None, None],
        ),
    ];
    let tokens = [&viewer, &tester, &editor];
    for (method, uri, body, expected) in rows {
        for (i, t) in tokens.iter().enumerate() {
            let got = reach(&app, method, uri, t, body.clone()).await.err();
            assert_eq!(
                got.as_deref(),
                expected[i],
                "{} {method} {uri}",
                ["viewer", "tester", "editor"][i]
            );
        }
    }
    // The management server lists what the roles allow.
    let list = |t: String| {
        let app = app.clone();
        async move {
            let auth = format!("Bearer {t}");
            crate::mcp::manage::tests::tool_names(&app, &[("authorization", auth.as_str())]).await
        }
    };
    let (v, e) = (list(viewer.clone()).await, list(editor.clone()).await);
    assert!(
        v.contains(&"list_mocks".to_string()) && !v.contains(&"create_mock".to_string()),
        "{v:?}"
    );
    assert!(!v.contains(&"list_requests".to_string()), "{v:?}");
    assert!(
        e.contains(&"create_mock".to_string()) && !e.contains(&"apply_config".to_string()),
        "{e:?}"
    );
    // A disabled user is refused at the next request (signed out).
    state.users.set_disabled(&viewer_id, true).expect("disable");
    let (status, v) = send(&app, "GET", "/_rustybin/config", &viewer, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");
}

#[tokio::test]
async fn a_settings_change_needs_what_its_keys_need() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    state
        .users
        .save_role(CustomRole {
            name: "operator".into(),
            description: "Instance settings, not the security groups".into(),
            permissions: vec![
                "settings.read".into(),
                "settings.write".into(),
                "config.apply".into(),
            ],
        })
        .expect("role");
    user_with(&state, "olly", &["operator"]);
    let t = token(&app, "olly", "olly-secret-1").await;
    let patch = |v: Value| Some(v);
    assert_eq!(
        reach(
            &app,
            "PATCH",
            "/_rustybin/settings",
            &t,
            patch(json!({"values": {"mocks.require_revision": true}}))
        )
        .await,
        Ok(StatusCode::OK)
    );
    for (body, missing) in [
        (
            json!({"values": {"egress.allow": "example.com"}}),
            "settings.security",
        ),
        (
            json!({"values": {"assist.base_url": "https://llm.example.com"}}),
            "settings.security",
        ),
        (
            json!({"values": {"console.title": "Ours"}}),
            "settings.security",
        ),
        (
            json!({"values": {"portal.title": "Our APIs"}}),
            "portal.settings",
        ),
        (
            json!({"secrets": {"assist.openai_key": "sk-test-1"}}),
            "secrets.write",
        ),
    ] {
        assert_eq!(
            reach(&app, "PATCH", "/_rustybin/settings", &t, Some(body.clone())).await,
            Err(missing.to_string()),
            "{body}"
        );
    }
    assert_eq!(
        reach(&app, "DELETE", "/_rustybin/settings/egress.allow", &t, None).await,
        Err("settings.security".to_string())
    );
    assert_eq!(
        reach(
            &app,
            "DELETE",
            "/_rustybin/settings/mocks.require_revision",
            &t,
            None
        )
        .await,
        Ok(StatusCode::OK)
    );
    // A document: settings and secrets are checked by key; agents need
    // agents.manage.
    let apply = |doc: Value| Some(json!({"document": doc, "dry_run": true}));
    assert_eq!(
        reach(
            &app,
            "POST",
            "/_rustybin/config/apply",
            &t,
            apply(json!({"version": 2, "settings": {"mocks.require_revision": false}}))
        )
        .await,
        Ok(StatusCode::OK)
    );
    for (doc, missing) in [
        (
            json!({"version": 2, "settings": {"egress.allow": "example.com"}}),
            "settings.security",
        ),
        (
            json!({"version": 2, "secrets": {"assist.openai_key": "${OPENAI_KEY}"}}),
            "secrets.write",
        ),
        (
            json!({"version": 2, "agents": {"default_ceiling": "traffic"}}),
            "agents.manage",
        ),
    ] {
        assert_eq!(
            reach(
                &app,
                "POST",
                "/_rustybin/config/apply",
                &t,
                apply(doc.clone())
            )
            .await,
            Err(missing.to_string()),
            "{doc}"
        );
    }
}
