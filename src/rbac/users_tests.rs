//! Step 09: users and roles on every surface, with invitations. The
//! invitation life, the guard rails, a disabled user's sign-in answer,
//! `/me` for every role, the escalation rule, the routes outside the
//! password sign-in, and the `users:` and `roles:` sections of a document
//! (round trip, prune, the untrusted refusals).

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::users::{self, Users, MAX_OPEN_INVITATIONS};
use super::{BuiltIn, CustomRole, NewUser, UsersError};
use crate::config::Config;
use crate::control_auth::ControlAuth;
use crate::limits::ManualClock;
use crate::login::{DEFAULT_PASSWORD, DEFAULT_USER};
use crate::test_support::{body_json, test_app_with, test_state_with};

/// A PHC string the store accepts (never verified in these tests).
const FAKE_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$YWJjZGVmZ2hpamtsbW5vcA$YWJjZGVmZ2hpamtsbW5vcGFiY2RlZmdoaWprbG1ub3A";
const ADMIN_TOKEN: &str = "admin-secret-09";

fn password_config() -> Config {
    let mut c = Config::for_tests();
    c.control_auth = ControlAuth::Password;
    c.admin_token = Some(ADMIN_TOKEN.into());
    c
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = axum::http::Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
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
    (status, body_json(resp).await)
}

async fn login(app: &Router, user: &str, password: &str) -> (StatusCode, Value) {
    send(
        app,
        "POST",
        crate::login::LOGIN_PATH,
        None,
        Some(json!({ "username": user, "password": password })),
    )
    .await
}

async fn token(app: &Router, user: &str, password: &str) -> String {
    let (status, v) = login(app, user, password).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["token"].as_str().expect("token").to_string()
}

fn token_of(v: &Value) -> String {
    v["invitation"]["token"]
        .as_str()
        .expect("invitation token")
        .to_string()
}

async fn join(app: &Router, token: &str, password: &str) -> (StatusCode, Value) {
    send(
        app,
        "POST",
        crate::login::JOIN_PATH,
        None,
        Some(json!({ "token": token, "password": password })),
    )
    .await
}

// ── The store ───────────────────────────────────────────────────────

#[test]
fn an_invitation_is_hashed_used_once_expires_and_is_capped() {
    let clock = Arc::new(ManualClock::new(chrono::Utc::now()));
    let store = Users::with_clock(clock.clone());
    let inv = store
        .invite_new(NewUser::new("alice", &["editor"]), "test")
        .expect("invited");
    assert!(users::is_invitation_token(&inv.token), "{}", inv.token);
    assert!(inv.user.pending && inv.user.disabled);
    assert_eq!(inv.user.status(), "invited");
    // Never the token in the store, its debug form or a user's view.
    assert!(!format!("{inv:?}").contains(&inv.token));
    assert!(!inv.user.view().to_string().contains(&inv.token));
    assert!(store.invitation(&inv.user.id).is_some());
    // One use.
    let accepted = store
        .accept_invitation(&inv.token, FAKE_HASH.into())
        .expect("accepted");
    assert!(!accepted.reset && !accepted.first_admin);
    let joined = store.get(&inv.user.id).expect("user");
    assert!(!joined.pending && !joined.disabled);
    assert_eq!(joined.status(), "active");
    assert!(joined.generation > inv.user.generation);
    assert_eq!(
        store.accept_invitation(&inv.token, FAKE_HASH.into()).err(),
        Some(UsersError::NotFound)
    );
    assert!(store.invitation(&inv.user.id).is_none());
    // A reset: a new link; it expires after 72 hours.
    let reset = store.invite(&inv.user.id, "test").expect("reset");
    assert!(store.invitation(&inv.user.id).expect("open").reset);
    clock.advance(Duration::from_secs(users::INVITATION_TTL_SECS as u64 + 1));
    assert!(store.invitation_user(&reset.token).is_none());
    assert_eq!(
        store
            .accept_invitation(&reset.token, FAKE_HASH.into())
            .err(),
        Some(UsersError::NotFound)
    );
    // A newer link replaces the older one.
    let a = store.invite(&inv.user.id, "test").expect("again");
    let b = store.invite(&inv.user.id, "test").expect("again");
    assert!(store.invitation_user(&a.token).is_none());
    assert!(store.invitation_user(&b.token).is_some());
    assert_eq!(store.open_invitations(), 1);
    // The cap: 100 open at once; a refused invitation leaves no user.
    for i in 1..MAX_OPEN_INVITATIONS {
        store
            .invite_new(NewUser::new(&format!("u{i}"), &["viewer"]), "test")
            .expect("invited");
    }
    assert_eq!(store.open_invitations(), MAX_OPEN_INVITATIONS);
    let before = store.len();
    assert!(matches!(
        store.invite_new(NewUser::new("one-too-many", &["viewer"]), "test"),
        Err(UsersError::Full(_))
    ));
    assert_eq!(store.len(), before);
    // Expired invitations free their place.
    clock.advance(Duration::from_secs(users::INVITATION_TTL_SECS as u64 + 1));
    assert_eq!(store.open_invitations(), 0);
    store
        .invite_new(NewUser::new("one-more", &["viewer"]), "test")
        .expect("room again");
    // Shapes: a token that is not one is never looked up.
    assert!(!users::is_invitation_token("rbi_short"));
    assert!(store.invitation_user("not-a-token").is_none());
}

#[test]
fn the_guard_rails_keep_an_admin_and_user_one() {
    let store = Users::in_memory();
    assert!(store.ensure_first_admin("root"));
    let first = store.first_admin().expect("user one");
    // The last enabled admin is never disabled, demoted or deleted.
    for r in [
        store.set_disabled(&first.id, true).err(),
        store.set_roles(&first.id, vec!["viewer".into()]).err(),
    ] {
        assert!(
            matches!(r, Some(UsersError::Refused(users::LAST_ADMIN, _))),
            "{r:?}"
        );
    }
    // User one is never deleted, even with another admin.
    let other = store
        .add(NewUser::new("other", &["admin"]), Some(FAKE_HASH.into()))
        .expect("admin");
    assert!(matches!(
        store.delete(&first.id),
        Err(UsersError::Refused(users::FIRST_ADMIN, _))
    ));
    // With two, one may go; then the other is the last.
    store
        .set_disabled(&first.id, true)
        .expect("disable user one");
    assert!(matches!(
        store.delete(&other.id),
        Err(UsersError::Refused(users::LAST_ADMIN, _))
    ));
    // An invited user (not joined) is no admin yet and is never enabled.
    let invited = store
        .invite_new(NewUser::new("new-admin", &["admin"]), "test")
        .expect("invited");
    assert!(matches!(
        store.set_disabled(&invited.user.id, false),
        Err(UsersError::Refused(users::PENDING, _))
    ));
    // Disabling an invited user drops the invitation.
    store
        .set_disabled(&invited.user.id, true)
        .expect("disable invited");
    assert!(store.invitation_user(&invited.token).is_none());
    // A custom role in use is not deleted.
    store
        .save_role(CustomRole {
            name: "auditor".into(),
            description: String::new(),
            permissions: vec!["mocks.read".into()],
        })
        .expect("role");
    store
        .set_roles(&other.id, vec!["admin".into(), "auditor".into()])
        .expect("roles");
    assert!(matches!(
        store.delete_role("auditor"),
        Err(UsersError::Refused(users::ROLE_IN_USE, _))
    ));
    assert_eq!(store.holders("auditor"), 1);
}

// ── The API ─────────────────────────────────────────────────────────

#[tokio::test]
async fn invite_join_reset_and_the_guard_rails_on_the_api() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    let admin = token(&app, DEFAULT_USER, DEFAULT_PASSWORD).await;
    // Invite: 201, the link once.
    let (status, v) = send(
        &app,
        "POST",
        "/_rustybin/users",
        Some(&admin),
        Some(json!({ "name": "alice", "display_name": "Alice", "email": "alice@example.com", "roles": ["editor"] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let link = v["invitation"]["url"].as_str().expect("url").to_string();
    assert!(link.contains("/ui/#/join/rbi_"), "{link}");
    let invite = token_of(&v);
    assert_eq!(v["user"]["status"], "invited");
    // The list never carries the link.
    let (_, list) = send(&app, "GET", "/_rustybin/users", Some(&admin), None).await;
    assert!(!list.to_string().contains(&invite));
    assert_eq!(list["open_invitations"], 1);
    let alice = list["users"]
        .as_array()
        .expect("users")
        .iter()
        .find(|u| u["name"] == "alice")
        .expect("alice")
        .clone();
    assert!(alice["invitation"]["expires_at"].is_string());
    // Not signed in while invited: a disabled user's answer is a wrong
    // password's.
    let invited = login(&app, "alice", "anything-1").await;
    assert_eq!(invited, login(&app, "nobody", "anything-1").await);
    // Join: a password that breaks the rules keeps the invitation.
    let (status, v) = join(&app, &invite, "short").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(v["reason"], "invalid_password");
    let (status, joined) = join(&app, &invite, "alice-secret-1").await;
    assert_eq!(status, StatusCode::OK, "{joined}");
    assert_eq!(joined["user"], "alice");
    assert_eq!(joined["joined"], true);
    let alice_token = joined["token"].as_str().expect("token").to_string();
    let (_, me) = send(&app, "GET", "/_rustybin/me", Some(&alice_token), None).await;
    assert_eq!(me["name"], "alice");
    assert_eq!(me["roles"], json!(["editor"]));
    assert_eq!(
        me["permissions"],
        json!(BuiltIn::Editor.permissions().names())
    );
    // Used once.
    let (status, v) = join(&app, &invite, "alice-secret-2").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(v["reason"], "invalid_invitation");
    // An editor manages nobody.
    let (status, v) = send(&app, "GET", "/_rustybin/users", Some(&alice_token), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(v["permission"], "users.read");
    // A reset: the old password works until the new link is accepted, then
    // the old sessions end.
    let (status, v) = send(
        &app,
        "POST",
        "/_rustybin/users/alice/invite",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert_eq!(v["invitation"]["reset"], true);
    let reset = token_of(&v);
    let (status, joined) = join(&app, &reset, "alice-secret-3").await;
    assert_eq!(status, StatusCode::OK, "{joined}");
    assert_eq!(joined["reset"], true);
    assert_eq!(
        send(&app, "GET", "/_rustybin/me", Some(&alice_token), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED,
        "the reset ended the old session"
    );
    assert_eq!(
        login(&app, "alice", "alice-secret-1").await.0,
        StatusCode::UNAUTHORIZED
    );
    let alice_token = joined["token"].as_str().expect("token").to_string();
    // Guard rails: nobody changes their own roles; the last enabled admin
    // stays; user one is never deleted.
    let (status, v) = send(
        &app,
        "PATCH",
        "/_rustybin/users/rustybin",
        Some(&admin),
        Some(json!({ "roles": ["viewer"] })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["reason"], "own_roles");
    let (status, v) = send(
        &app,
        "PATCH",
        "/_rustybin/users/rustybin",
        Some(ADMIN_TOKEN),
        Some(json!({ "disabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["reason"], "last_admin");
    let (status, v) = send(
        &app,
        "DELETE",
        "/_rustybin/users/rustybin",
        Some(ADMIN_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["reason"], "first_admin");
    // A role change applies to the next request; a disable ends the
    // sessions and answers the sign-in as a wrong password.
    let (status, v) = send(
        &app,
        "PATCH",
        "/_rustybin/users/alice",
        Some(&admin),
        Some(json!({ "roles": ["viewer"], "display_name": "Alice V." })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["display_name"], "Alice V.");
    let (_, me) = send(&app, "GET", "/_rustybin/me", Some(&alice_token), None).await;
    assert_eq!(me["roles"], json!(["viewer"]));
    let (status, v) = send(
        &app,
        "PATCH",
        "/_rustybin/users/alice",
        Some(&admin),
        Some(json!({ "disabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "disabled");
    assert_eq!(v["sessions"], 0);
    assert_eq!(
        send(&app, "GET", "/_rustybin/me", Some(&alice_token), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let disabled = login(&app, "alice", "alice-secret-3").await;
    assert_eq!(disabled, login(&app, "alice", "not-the-password").await);
    // A disabled user gets no new link until enabled.
    let (status, v) = send(
        &app,
        "POST",
        "/_rustybin/users/alice/invite",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    // Custom roles: one in use is not deleted.
    let (status, v) = send(
        &app,
        "POST",
        "/_rustybin/roles",
        Some(&admin),
        Some(json!({ "name": "auditor", "permissions": ["mocks.read", "traffic.read"] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert_eq!(v["privileged"], false);
    let (status, _) = send(
        &app,
        "PATCH",
        "/_rustybin/users/alice",
        Some(&admin),
        Some(json!({ "roles": ["auditor"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, v) = send(
        &app,
        "DELETE",
        "/_rustybin/roles/auditor",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["reason"], "role_in_use");
    let (status, v) = send(
        &app,
        "PUT",
        "/_rustybin/roles/viewer",
        Some(&admin),
        Some(json!({ "permissions": ["mocks.read"] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    // Delete alice, then the role goes.
    let (status, v) = send(&app, "DELETE", "/_rustybin/users/alice", Some(&admin), None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, _) = send(
        &app,
        "DELETE",
        "/_rustybin/roles/auditor",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The admin token is no user: /me says so.
    let (status, v) = send(&app, "GET", "/_rustybin/me", Some(ADMIN_TOKEN), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(v["reason"], "not_a_user");
}

#[tokio::test]
async fn nobody_grants_or_manages_more_than_they_hold() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    state
        .users
        .save_role(CustomRole {
            name: "people".into(),
            description: "Manages users".into(),
            permissions: vec!["users.read".into(), "users.manage".into()],
        })
        .expect("role");
    let hash = crate::login::hash_password("pat-secret-1").expect("hash");
    state
        .users
        .add(NewUser::new("pat", &["people"]), Some(hash))
        .expect("pat");
    let pat = token(&app, "pat", "pat-secret-1").await;
    // An admin, or a viewer (settings.read, which pat lacks), is refused,
    // naming a permission pat does not hold.
    for roles in [json!(["admin"]), json!(["viewer"])] {
        let (status, v) = send(
            &app,
            "POST",
            "/_rustybin/users",
            Some(&pat),
            Some(json!({ "name": "eve", "roles": roles })),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
        assert_eq!(v["reason"], "permission_required");
        assert!(v.get("permissions").is_none());
    }
    // Another people manager is within pat's permissions.
    let (status, v) = send(
        &app,
        "POST",
        "/_rustybin/users",
        Some(&pat),
        Some(json!({ "name": "pam", "roles": ["people"] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    // The first admin's password is not pat's to reset, nor its roles.
    let (status, _) = send(
        &app,
        "POST",
        "/_rustybin/users/rustybin/invite",
        Some(&pat),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // A role beyond pat's permissions is not saved.
    let (status, _) = send(
        &app,
        "POST",
        "/_rustybin/roles",
        Some(&pat),
        Some(json!({ "name": "wide", "permissions": ["mocks.write"] })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Nor its own roles.
    let (status, v) = send(
        &app,
        "PATCH",
        "/_rustybin/users/pat",
        Some(&pat),
        Some(json!({ "roles": ["people", "admin"] })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(v["reason"], "own_roles");
}

#[tokio::test]
async fn me_works_for_every_role() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    // One hash for every user (argon2 is slow in a debug build).
    let hash = crate::login::hash_password("same-secret-1").expect("hash");
    for role in BuiltIn::ALL {
        let name = format!("u-{}", role.name().replace('_', "-"));
        state
            .users
            .add(NewUser::new(&name, &[role.name()]), Some(hash.clone()))
            .expect("user");
        let t = token(&app, &name, "same-secret-1").await;
        let (status, me) = send(&app, "GET", "/_rustybin/me", Some(&t), None).await;
        assert_eq!(status, StatusCode::OK, "{}: {me}", role.name());
        assert_eq!(me["name"], name);
        assert_eq!(me["roles"], json!([role.name()]));
        assert_eq!(me["permissions"], json!(role.permissions().names()));
        assert_eq!(me["password"]["kept_in"], "the user store");
        let (status, v) = send(
            &app,
            "PATCH",
            "/_rustybin/me",
            Some(&t),
            Some(json!({ "display_name": format!("The {}", role.name()), "email": "me@example.com" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["email"], "me@example.com");
        let (status, v) = send(
            &app,
            "PATCH",
            "/_rustybin/me",
            Some(&t),
            Some(json!({ "roles": ["admin"] })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
        let (status, s) = send(&app, "GET", "/_rustybin/me/sessions", Some(&t), None).await;
        assert_eq!(status, StatusCode::OK, "{s}");
        assert_eq!(s["count"], 1);
        assert_eq!(s["sessions"][0]["id"], s["current"]);
        // A wrong current password.
        let (status, v) = send(
            &app,
            "PUT",
            crate::login::ME_PASSWORD_PATH,
            Some(&t),
            Some(json!({ "current_password": "wrong-one-1", "new_password": "new-secret-2" })),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
        // Ending one's own session.
        let id = s["current"].as_str().expect("id").to_string();
        let (_, v) = send(
            &app,
            "DELETE",
            &format!("/_rustybin/me/sessions/{id}"),
            Some(&t),
            None,
        )
        .await;
        assert_eq!(v["ended"], true);
        assert_eq!(
            send(&app, "GET", "/_rustybin/me", Some(&t), None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    // A password change, once (as the 0.3 alias): the other sessions end,
    // the fresh token works, the new password signs in.
    let a = token(&app, "u-tester", "same-secret-1").await;
    let b = token(&app, "u-tester", "same-secret-1").await;
    let (status, v) = send(
        &app,
        "PUT",
        crate::login::PASSWORD_PATH,
        Some(&a),
        Some(json!({ "current_password": "same-secret-1", "new_password": "tester-secret-2" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["signed_out"], 2);
    let fresh = v["token"].as_str().expect("fresh").to_string();
    assert_eq!(
        send(&app, "GET", "/_rustybin/me", Some(&b), None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&app, "GET", "/_rustybin/me", Some(&fresh), None)
            .await
            .0,
        StatusCode::OK
    );
    // Another user's name never changes this way.
    let (status, v) = send(
        &app,
        "PUT",
        crate::login::ME_PASSWORD_PATH,
        Some(&fresh),
        Some(json!({ "current_password": "tester-secret-2", "new_password": "tester-secret-3", "username": "renamed" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    // Signed out everywhere by oneself.
    let (_, v) = send(&app, "DELETE", "/_rustybin/me/sessions", Some(&fresh), None).await;
    assert!(v["ended"].as_u64().is_some());
    assert_eq!(
        send(&app, "GET", "/_rustybin/me", Some(&fresh), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    // The first admin's own account (the sign-in settings): the change
    // signs every session out, as in 0.4 (the OAuth tokens carry only the
    // instance's generation).
    let viewer = token(&app, "u-viewer", "same-secret-1").await;
    let first = token(&app, DEFAULT_USER, DEFAULT_PASSWORD).await;
    let (_, me) = send(&app, "GET", "/_rustybin/me", Some(&first), None).await;
    assert_eq!(me["first_admin"], true);
    assert_eq!(
        me["password"]["kept_in"],
        "the sign-in settings (console.password_hash)"
    );
    let (status, v) = send(
        &app,
        "PUT",
        crate::login::ME_PASSWORD_PATH,
        Some(&first),
        Some(json!({ "current_password": DEFAULT_PASSWORD, "new_password": "root-secret-2" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        send(&app, "GET", "/_rustybin/me", Some(&viewer), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED,
        "the first admin's change signs every session out"
    );
    let fresh = v["token"].as_str().expect("fresh token");
    assert_eq!(
        send(&app, "GET", "/_rustybin/me", Some(fresh), None)
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn users_need_the_password_sign_in_and_roles_work_everywhere() {
    let mut c = Config::for_tests();
    c.control_auth = ControlAuth::Open;
    let app = test_app_with(test_state_with(c));
    for (m, p) in [
        ("GET", "/_rustybin/users"),
        ("GET", "/_rustybin/me"),
        ("POST", "/_rustybin/join"),
    ] {
        let body = (m == "POST").then(|| json!({ "token": "x", "password": "y" }));
        let (status, _) = send(&app, m, p, None, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{m} {p}");
    }
    let (status, v) = send(&app, "GET", "/_rustybin/roles", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        v["roles"].as_array().map(Vec::len),
        Some(BuiltIn::ALL.len())
    );
    assert_eq!(
        v["permissions"].as_array().map(Vec::len),
        Some(super::Permission::ALL.len())
    );
    let (status, v) = send(
        &app,
        "PUT",
        "/_rustybin/roles/people",
        None,
        Some(json!({ "permissions": ["users.read", "users.manage"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["privileged"], true);
    assert_eq!(v["created"], true);
}

#[tokio::test]
async fn join_is_open_and_rate_limited() {
    let state = test_state_with(password_config());
    let app = test_app_with(state);
    let fake = format!("{}{}", users::INVITATION_PREFIX, "A".repeat(43));
    let mut last = StatusCode::OK;
    for _ in 0..crate::login::FAILURES_PER_WINDOW + 1 {
        last = join(&app, &fake, "a-good-password-1").await.0;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
}

// ── Documents ───────────────────────────────────────────────────────

fn opts(
    state: &crate::state::AppState,
    prune: bool,
    untrusted: bool,
    caller: Option<&str>,
) -> crate::configdoc::ApplyOptions {
    let caller = caller.map(|name| {
        let u = state.users.find_by_name(name).expect("caller");
        let r = state.users.resolve(&u.id).expect("resolved");
        Arc::new(super::Resolved {
            principal: super::Principal::User {
                id: u.id.clone(),
                name: u.name.clone(),
                display_name: u.display_name.clone(),
            },
            roles: r.roles.clone(),
            permissions: r.permissions,
        })
    });
    crate::configdoc::ApplyOptions {
        prune,
        dry_run: false,
        principal: "test".into(),
        base: None,
        untrusted,
        caller,
        base_url: Some("https://rb.example.com".into()),
    }
}

#[test]
fn a_document_carries_users_and_roles_round_trip_prune_and_untrusted() {
    let a = test_state_with(password_config());
    a.users
        .save_role(CustomRole {
            name: "auditor".into(),
            description: "Reads".into(),
            permissions: vec!["mocks.read".into()],
        })
        .expect("role");
    let mut alice = NewUser::new("alice", &["editor", "auditor"]);
    alice.display_name = Some("Alice Doe".into());
    alice.email = Some("alice@example.com".into());
    a.users.add(alice, Some(FAKE_HASH.into())).expect("alice");
    let bob = a
        .users
        .add(NewUser::new("bob", &["viewer"]), Some(FAKE_HASH.into()))
        .expect("bob");
    a.users.set_disabled(&bob.id, true).expect("disable");
    let export = crate::configdoc::export(&a, crate::configdoc::SecretsMode::Refs, false).file;
    let yaml = crate::configdoc::to_yaml(&export);
    assert!(
        !yaml.contains("argon2") && !yaml.contains("password"),
        "{yaml}"
    );
    assert!(yaml.contains("users:") && yaml.contains("roles:"), "{yaml}");
    // A second instance: the users are created invited, with their links.
    let b = test_state_with(password_config());
    let file = crate::mocks::config_file::parse(&yaml).expect("parses");
    let r = crate::configdoc::apply(&b, file, &opts(&b, false, false, None));
    assert_eq!(r["ok"], true, "{r}");
    let invitations = r["results"]["users"]["invitations"]
        .as_array()
        .expect("invitations");
    // Alice is invited; bob, disabled, gets no link until enabled.
    assert_eq!(invitations.len(), 1, "{r}");
    assert_eq!(invitations[0]["name"], "alice");
    assert!(invitations[0]["url"]
        .as_str()
        .is_some_and(|u| u.starts_with("https://rb.example.com/ui/#/join/rbi_")));
    let b_alice = b.users.find_by_name("alice").expect("alice");
    assert_eq!(b_alice.roles, ["editor", "auditor"]);
    assert_eq!(b_alice.display_name, "Alice Doe");
    assert_eq!(b_alice.source, super::Source::Document);
    assert!(b_alice.pending);
    // The same document again changes nothing.
    let again = crate::configdoc::export(&b, crate::configdoc::SecretsMode::Refs, false).file;
    let (ua, ub) = (export.users.clone(), again.users.clone());
    assert_eq!(ua, ub, "the users round trip");
    assert_eq!(export.roles, again.roles, "the roles round trip");
    // A password in a document makes the section fail to load.
    let (bad, failed) = crate::mocks::config_file::parse_lenient(
        "version: 2\nusers:\n- name: mallory\n  password: hunter22\n  roles: [admin]\n",
    )
    .expect("parses");
    assert!(bad.users.is_none());
    assert!(
        failed.iter().any(|f| f.error.contains("password")),
        "{failed:?}"
    );
    // Prune: never the first admin, the applying user nor the last admin.
    let hash = FAKE_HASH.to_string();
    b.users
        .add(NewUser::new("carol", &["admin"]), Some(hash))
        .expect("carol");
    let doc = crate::mocks::config_file::parse("version: 2\nusers: []\nroles: []\n").expect("doc");
    let r = crate::configdoc::apply(&b, doc, &opts(&b, true, false, Some("carol")));
    let deleted = r["results"]["users"]["deleted"]
        .as_array()
        .expect("deleted");
    let names: Vec<&str> = deleted.iter().filter_map(Value::as_str).collect();
    assert!(names.contains(&"alice") && names.contains(&"bob"), "{r}");
    assert!(
        b.users.find_by_name(DEFAULT_USER).is_some(),
        "user one stays"
    );
    assert!(
        b.users.find_by_name("carol").is_some(),
        "the applying user stays"
    );
    assert!(
        b.users.custom_roles().is_empty(),
        "the role nobody holds went: {r}"
    );
    // From the repository: no new user, no role change, no privileged
    // role, no prune; a display name applies.
    let pulled = crate::mocks::config_file::parse(
        "version: 2\nroles:\n- name: boss\n  permissions: [users.manage]\n- name: reader\n  permissions: [mocks.read]\nusers:\n- name: carol\n  display_name: Carol C.\n  roles: [viewer]\n- name: mallory\n  roles: [admin]\n",
    )
    .expect("doc");
    let r = crate::configdoc::apply(&b, pulled, &opts(&b, true, true, None));
    let problems = r["problems"].to_string();
    assert!(problems.contains("roles.boss"), "{problems}");
    assert!(problems.contains("users.mallory"), "{problems}");
    assert!(problems.contains("users.carol"), "{problems}");
    assert!(b.users.find_by_name("mallory").is_none());
    let carol = b.users.find_by_name("carol").expect("carol");
    assert_eq!(carol.roles, ["admin"], "kept as it is");
    assert_eq!(carol.display_name, "Carol C.", "a display name applies");
    assert!(b.users.role_permissions("boss").is_none());
    assert!(b.users.role_permissions("reader").is_some());
    assert!(b.users.find_by_name(DEFAULT_USER).is_some());
    // A section needs users.manage.
    let editor = super::Resolved {
        principal: super::Principal::Open,
        roles: vec!["editor".into()],
        permissions: BuiltIn::Editor.permissions(),
    };
    let doc = crate::mocks::config_file::parse("version: 2\nroles: []\n").expect("doc");
    assert_eq!(
        crate::configdoc::sections_need(&editor, &b.settings, &doc),
        Some(super::Permission::UsersManage)
    );
}
