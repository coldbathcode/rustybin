//! Projects (access control step 11): the union rule, every project-scoped
//! route keeping a member to its project (lists filtered, another project's
//! resource `404`), moves, deletion, the guard rails, documents, single
//! sign-on and an agent acting for a member.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::control_auth::ControlAuth;
use crate::rbac::{Member, NewUser, Resolved};
use crate::test_support::{body_json, json_request, test_app_with, test_state_with};

const ADMIN: &str = "projects-admin-token";
const SPEC: &str = r#"{"openapi":"3.0.3","info":{"title":"T","version":"1"},"paths":{"/a":{"get":{"operationId":"a","responses":{"200":{"description":"ok","content":{"application/json":{"schema":{"type":"object","properties":{"n":{"type":"integer"}}}}}}}}}}}"#;

fn password_config() -> Config {
    let mut c = Config::for_tests();
    c.control_auth = ControlAuth::Password;
    c.admin_token = Some(ADMIN.into());
    c
}

fn admin() -> String {
    format!("Bearer {ADMIN}")
}

fn add_user(state: &AppState, name: &str, roles: &[&str]) -> String {
    let hash = crate::login::hash_password(&format!("{name}-secret-1")).expect("hash");
    state
        .users
        .add(NewUser::new(name, roles), Some(hash))
        .expect("user")
        .id
}

fn bind(state: &AppState, project: &str, user: &str, roles: &[&str]) {
    state
        .users
        .set_binding(project, user, roles.iter().map(|r| r.to_string()).collect())
        .expect("binding");
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

async fn send_raw(
    app: &Router,
    method: &str,
    uri: &str,
    auth: &str,
    body: Option<Value>,
) -> axum::http::Response<Body> {
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
    app.clone().oneshot(req).await.expect("response")
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    auth: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let resp = send_raw(app, method, uri, auth, body).await;
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22)
        .await
        .expect("body");
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()));
    (status, v)
}

fn names(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["name"].as_str().map(str::to_string))
        .collect()
}

/// Two projects, `a` and `b`, each with a mock, a saved test and a
/// checkpoint, plus an instance mock `mi` and test `ti`; `alice` is an
/// editor in `a` and holds no instance role; `bob` an editor in `b`.
struct Fixture {
    state: AppState,
    app: Router,
    alice: String,
    run_b: String,
}

async fn fixture() -> Fixture {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    for p in ["a", "b"] {
        state.users.save_project(p, None, None).expect("project");
    }
    let alice = add_user(&state, "alice", &[]);
    let bob = add_user(&state, "bob", &[]);
    bind(&state, "a", &alice, &["editor"]);
    bind(&state, "b", &bob, &["editor"]);
    for (mock, project) in [("ma", Some("a")), ("mb", Some("b")), ("mi", None)] {
        let mut body = json!({ "name": mock, "spec": SPEC });
        if let Some(p) = project {
            body["project"] = json!(p);
        }
        let (s, v) = send(&app, "POST", "/_rustybin/mocks", &admin(), Some(body)).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        assert_eq!(v["project"], json!(project), "{v}");
        let mut test = json!({ "request": { "method": "GET", "path": format!("/mock/{mock}/a") }, "assert": [{ "status": 200 }] });
        if let Some(p) = project {
            test["project"] = json!(p);
        }
        let t = format!("t{}", &mock[1..]);
        let (s, v) = send(
            &app,
            "PUT",
            &format!("/_rustybin/tests/{t}"),
            &admin(),
            Some(test),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    for (cp, project) in [("ca", Some("a")), ("cb", Some("b")), ("ci", None)] {
        let mut body = json!({ "name": cp });
        if let Some(p) = project {
            body["project"] = json!(p);
        }
        let (s, v) = send(&app, "POST", "/_rustybin/checkpoints", &admin(), Some(body)).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }
    // A kept run of b's test only.
    let bob_token = sign_in(&app, "bob").await;
    let (s, run) = send(
        &app,
        "POST",
        "/_rustybin/tests/run",
        &bob_token,
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{run}");
    assert_eq!(run["total"], json!(1), "bob runs b's test only: {run}");
    let run_b = run["run_id"].as_str().expect("run id").to_string();
    let alice_token = sign_in(&app, "alice").await;
    Fixture {
        state,
        app,
        alice: alice_token,
        run_b,
    }
}

// ── The model ───────────────────────────────────────────────────────

/// A permission on a resource of a project is the union of the instance
/// roles and the roles in that project; elsewhere only the instance roles.
#[test]
fn the_union_rule_gives_project_roles_on_project_resources() {
    let users = crate::rbac::Users::in_memory();
    users.save_project("pay", None, None).expect("project");
    users.save_project("ops", None, None).expect("project");
    let carol = users
        .add(NewUser::new("carol", &["viewer"]), None)
        .expect("u")
        .id;
    let dave = users.add(NewUser::new("dave", &[]), None).expect("u").id;
    let erin = users
        .add(NewUser::new("erin", &["editor"]), None)
        .expect("u")
        .id;
    let frank = users
        .add(NewUser::new("frank", &["viewer"]), None)
        .expect("u")
        .id;
    for (u, p, r) in [
        (&carol, "pay", "editor"),
        (&dave, "pay", "editor"),
        (&frank, "ops", "tester"),
    ] {
        users.set_binding(p, u, vec![r.into()]).expect("bind");
    }
    let resolved = |id: &str| {
        let r = users.resolve(id).expect("user");
        Resolved {
            principal: crate::rbac::Principal::User {
                id: id.into(),
                name: r.user.name.clone(),
                display_name: r.user.name.clone(),
            },
            roles: r.roles.clone(),
            permissions: r.permissions,
            projects: r.projects.clone(),
        }
    };
    use Permission as P;
    let table: &[(&str, P, Option<&str>, bool)] = &[
        // carol: viewer everywhere, editor in pay.
        (&carol, P::MocksRead, None, true),
        (&carol, P::MocksRead, Some("ops"), true),
        (&carol, P::MocksWrite, Some("pay"), true),
        (&carol, P::MocksWrite, Some("ops"), false),
        (&carol, P::MocksWrite, None, false),
        (&carol, P::SettingsWrite, None, false),
        // dave: editor in pay, nothing else (instance.read for the console).
        (&dave, P::MocksRead, Some("pay"), true),
        (&dave, P::MocksWrite, Some("pay"), true),
        (&dave, P::TestsRun, Some("pay"), true),
        (&dave, P::MocksRead, None, false),
        (&dave, P::MocksRead, Some("ops"), false),
        (&dave, P::InstanceRead, None, true),
        (&dave, P::SettingsRead, None, false),
        (&dave, P::UsersRead, None, false),
        // erin: an instance editor, every project.
        (&erin, P::MocksWrite, Some("pay"), true),
        (&erin, P::MocksWrite, Some("ops"), true),
        (&erin, P::MocksWrite, None, true),
        // frank: viewer, tester in ops.
        (&frank, P::TestsRun, Some("ops"), true),
        (&frank, P::TestsRun, Some("pay"), false),
        (&frank, P::MocksWrite, Some("ops"), false),
    ];
    for (id, p, project, want) in table {
        let r = resolved(id);
        assert_eq!(
            r.has_in(*p, *project),
            *want,
            "{} {} in {project:?}",
            users.get(id).expect("u").name,
            p.name()
        );
    }
    assert!(resolved(&dave).has_somewhere(P::MocksWrite));
    assert!(!resolved(&dave).has(P::MocksWrite));
    assert!(resolved(&dave).has_projects());
    assert!(!resolved(&erin).has_projects());
    // A capped caller (an agent's ceiling) is capped in its projects too.
    let capped = resolved(&dave).capped(crate::rbac::BuiltIn::Viewer.permissions());
    assert!(capped.has_in(P::MocksRead, Some("pay")));
    assert!(!capped.has_in(P::MocksWrite, Some("pay")));
}

/// The catalogue's project-scoped routes are exactly `PROJECT_ROUTES`.
#[test]
fn the_project_routes_are_listed() {
    let mut scoped: Vec<&str> = crate::catalog::all()
        .iter()
        .filter(|e| e.project)
        .map(|e| e.path)
        .collect();
    scoped.sort();
    let mut listed: Vec<&str> = crate::rbac::need::PROJECT_ROUTES.to_vec();

    listed.sort();
    assert_eq!(scoped, listed);
}

/// Every project-scoped route, as a member of project `a`, on project
/// `b`'s resources: never an answer that reads or changes them (`404` for
/// a read), and every list leaves `b`'s and the instance's out.
#[tokio::test]
async fn every_project_route_checks_each_resource() {
    let f = fixture().await;
    let (app, alice) = (&f.app, f.alice.as_str());
    let concrete = |template: &str| -> String {
        let mut p = template.to_string();
        let name = if p.starts_with("/_rustybin/tests/") {
            "tb"
        } else if p.starts_with("/_rustybin/checkpoints/") {
            "cb"
        } else {
            "mb"
        };
        for (k, v) in [
            ("{name}", name),
            ("{op}", "a"),
            ("{revision}", "1"),
            ("{format}", "postman"),
            ("{n}", "1"),
            ("{id}", ""),
        ] {
            p = p.replace(k, v);
        }
        if template == "/_rustybin/tests/runs/{id}" {
            p = format!("/_rustybin/tests/runs/{}", f.run_b);
        }
        if template == "/_rustybin/portal/items/{id}" {
            p = "/_rustybin/portal/items/mock:mb".into();
        }
        p
    };
    let mut checked = 0;
    for e in crate::catalog::all().iter().filter(|e| e.project) {
        if !e.path.contains('{') {
            continue; // the lists and creations, below
        }
        for m in e.expanded_methods() {
            let uri = concrete(e.path);
            let resp = send_raw(app, m, &uri, alice, Some(json!({}))).await;
            let s = resp.status();
            assert!(
                matches!(s.as_u16(), 400 | 403 | 404 | 409 | 422),
                "{m} {uri}: {s} (a member of a reached b's resource)"
            );
            if m == "GET" {
                assert_eq!(s, StatusCode::NOT_FOUND, "GET {uri}");
            }
            checked += 1;
        }
    }
    assert!(checked > 30, "{checked}");
    // The instance's mock is not hers either.
    let (s, _) = send(app, "GET", "/_rustybin/mocks/mi", alice, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Lists: her project's only.
    let (s, v) = send(app, "GET", "/_rustybin/mocks", alice, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(names(&v, "mocks"), vec!["ma"]);
    assert_eq!(v["mocks"][0]["can_manage"], json!(true));
    let (_, v) = send(app, "GET", "/_rustybin/mocks/export", alice, None).await;
    let text = v.as_str().expect("yaml").to_string();
    assert!(
        text.contains("name: ma") && !text.contains("name: mb") && !text.contains("name: mi"),
        "{text}"
    );
    let (_, v) = send(app, "GET", "/_rustybin/tests", alice, None).await;
    assert_eq!(names(&v, "tests"), vec!["ta"]);
    let (_, v) = send(app, "GET", "/_rustybin/tests/export", alice, None).await;
    let text = v.as_str().expect("hurl").to_string();
    assert!(
        text.contains("/mock/ma/a") && !text.contains("/mock/mb/a"),
        "{text}"
    );
    let (_, v) = send(app, "GET", "/_rustybin/checkpoints", alice, None).await;
    assert_eq!(names(&v, "checkpoints"), vec!["ca"]);
    let (_, v) = send(app, "GET", "/_rustybin/tests/runs", alice, None).await;
    assert_eq!(v["total"], json!(0), "b's run is not hers: {v}");
    // Creations: not in b, and in a without naming it (her one project).
    for (uri, body) in [
        (
            "/_rustybin/mocks",
            json!({ "name": "nb", "spec": SPEC, "project": "b" }),
        ),
        (
            "/_rustybin/tests",
            json!({ "name": "nb", "request": { "path": "/mock/mb/a" }, "project": "b" }),
        ),
        (
            "/_rustybin/checkpoints",
            json!({ "name": "nb", "project": "b" }),
        ),
    ] {
        let (s, v) = send(app, "POST", uri, alice, Some(body)).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "POST {uri}: {v}");
        assert_eq!(v["reason"], json!("permission_required"), "{v}");
    }
    let (s, v) = send(
        app,
        "POST",
        "/_rustybin/mocks",
        alice,
        Some(json!({ "name": "na", "spec": SPEC })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["project"], json!("a"));
    // A run: her tests only; b's by name is 404; clearing everyone's runs
    // needs the instance-wide permission.
    let (s, v) = send(app, "POST", "/_rustybin/tests/run", alice, Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["total"], json!(1));
    let (s, _) = send(
        app,
        "POST",
        "/_rustybin/tests/run",
        alice,
        Some(json!({ "names": ["tb"] })),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, v) = send(app, "GET", "/_rustybin/tests/runs", alice, None).await;
    assert_eq!(v["total"], json!(1), "her run: {v}");
    let (s, _) = send(app, "DELETE", "/_rustybin/tests/runs", alice, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Instance-wide things need instance roles.
    for uri in [
        "/_rustybin/settings",
        "/_rustybin/users",
        "/_rustybin/egress",
        "/_rustybin/git",
        "/_rustybin/requests",
    ] {
        let (s, _) = send(app, "GET", uri, alice, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "GET {uri}");
    }
    // The session says what she holds where.
    let (_, v) = send(app, "GET", "/_rustybin/session", alice, None).await;
    assert_eq!(v["projects"][0]["project"], json!("a"), "{v}");
    assert!(v["projects"][0]["permissions"]
        .as_array()
        .expect("perms")
        .contains(&json!("mocks.write")));
    // Her own project's mock: read, change, its traffic.
    for (m, uri) in [
        ("GET", "/_rustybin/mocks/ma"),
        ("GET", "/_rustybin/mocks/ma/requests"),
        ("POST", "/_rustybin/mocks/ma/disable"),
        ("GET", "/_rustybin/checkpoints/ca"),
        ("GET", "/_rustybin/tests/ta"),
    ] {
        let (s, v) = send(app, m, uri, alice, None).await;
        assert!(s.is_success(), "{m} {uri}: {s} {v}");
    }
    let (s, v) = send(
        app,
        "POST",
        "/_rustybin/checkpoints/ca/restore?dry_run=true",
        alice,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let _ = &f.state;
}

/// A caller with instance roles only sees what it saw before projects: an
/// instance editor every project's mocks, an instance viewer with a role in
/// a project changes that project's mocks only.
#[tokio::test]
async fn instance_roles_reach_every_project() {
    let f = fixture().await;
    let erin = add_user(&f.state, "erin", &["editor"]);
    let carol = add_user(&f.state, "carol", &["viewer"]);
    bind(&f.state, "a", &carol, &["editor"]);
    let _ = erin;
    let erin = sign_in(&f.app, "erin").await;
    let carol = sign_in(&f.app, "carol").await;
    let (_, v) = send(&f.app, "GET", "/_rustybin/mocks", &erin, None).await;
    let mut n = names(&v, "mocks");
    n.sort();
    assert!(
        n.contains(&"ma".to_string())
            && n.contains(&"mb".to_string())
            && n.contains(&"mi".to_string()),
        "{n:?}"
    );
    let (s, _) = send(&f.app, "POST", "/_rustybin/mocks/mb/disable", &erin, None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = send(&f.app, "GET", "/_rustybin/mocks/mb", &carol, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["can_manage"], json!(false));
    let (s, v) = send(&f.app, "POST", "/_rustybin/mocks/mb/disable", &carol, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["permission"], json!("mocks.write"));
    let (s, _) = send(&f.app, "POST", "/_rustybin/mocks/ma/disable", &carol, None).await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = send(&f.app, "GET", "/_rustybin/mocks?project=b", &carol, None).await;
    assert_eq!(names(&v, "mocks"), vec!["mb"]);
    let (_, v) = send(
        &f.app,
        "GET",
        "/_rustybin/mocks?project=instance",
        &carol,
        None,
    )
    .await;
    assert!(!names(&v, "mocks").contains(&"ma".to_string()));
}

/// A move between projects needs `mocks.write` (or `tests.write`) in both.
#[tokio::test]
async fn a_move_needs_write_in_both_projects() {
    let f = fixture().await;
    let alice = f.state.users.find("alice").expect("alice").id;
    let (s, v) = send(
        &f.app,
        "PATCH",
        "/_rustybin/mocks/ma",
        &f.alice,
        Some(json!({ "project": "b" })),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    bind(&f.state, "b", &alice, &["editor"]);
    let (s, v) = send(
        &f.app,
        "PATCH",
        "/_rustybin/mocks/ma",
        &f.alice,
        Some(json!({ "project": "b" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        f.state.mocks.get("ma").expect("ma").project.as_deref(),
        Some("b")
    );
    // To the instance needs the instance-wide permission.
    let (s, _) = send(
        &f.app,
        "PATCH",
        "/_rustybin/mocks/ma",
        &f.alice,
        Some(json!({ "project": null })),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // A test moves the same way: PUT with another project.
    let (s, v) = send(
        &f.app,
        "PUT",
        "/_rustybin/tests/ta",
        &f.alice,
        Some(json!({ "request": { "path": "/mock/ma/a" }, "project": "b" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        f.state
            .saved_tests
            .get("ta")
            .expect("ta")
            .project
            .as_deref(),
        Some("b")
    );
    // An unknown project is a 400.
    let (s, v) = send(
        &f.app,
        "PATCH",
        "/_rustybin/mocks/ma",
        &admin(),
        Some(json!({ "project": "nope" })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["reason"], json!("unknown_project"));
}

/// A project is deleted only when empty, or with `move_to`.
#[tokio::test]
async fn a_project_is_deleted_only_when_empty_or_with_move_to() {
    let f = fixture().await;
    let (s, v) = send(&f.app, "DELETE", "/_rustybin/projects/a", &admin(), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["reason"], json!("not_empty"));
    assert_eq!(v["resources"]["mocks"], json!(["ma"]));
    // A member without users.manage cannot.
    let (s, _) = send(
        &f.app,
        "DELETE",
        "/_rustybin/projects/a?move_to=b",
        &f.alice,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = send(
        &f.app,
        "DELETE",
        "/_rustybin/projects/a?move_to=b",
        &admin(),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["moved"]["mocks"], json!(["ma"]));
    assert_eq!(
        f.state.mocks.get("ma").expect("ma").project.as_deref(),
        Some("b")
    );
    assert_eq!(
        f.state
            .saved_tests
            .get("ta")
            .expect("ta")
            .project
            .as_deref(),
        Some("b")
    );
    assert_eq!(
        f.state
            .checkpoints
            .get("ca")
            .expect("ca")
            .project
            .as_deref(),
        Some("b")
    );
    assert!(!f.state.users.project_exists("a"));
    // alice lost her roles there: nothing to see now.
    let (s, v) = send(&f.app, "GET", "/_rustybin/projects", &f.alice, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["count"], json!(0), "{v}");
    // b to the instance, then the empty project goes.
    let (s, _) = send(
        &f.app,
        "DELETE",
        "/_rustybin/projects/b?move_to=instance",
        &admin(),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(f.state.mocks.get("mb").expect("mb").project, None);
    f.state.users.save_project("c", None, None).expect("c");
    let (s, _) = send(&f.app, "DELETE", "/_rustybin/projects/c", &admin(), None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = send(&f.app, "DELETE", "/_rustybin/projects/c", &admin(), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// The routes: create, members, the rules of users and roles.
#[tokio::test]
async fn projects_are_saved_with_members_and_the_rules_hold() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    let ivy = add_user(&state, "ivy", &["viewer"]);
    let (s, v) = send(
        &app,
        "PUT",
        "/_rustybin/projects/payments",
        &admin(),
        Some(json!({ "description": "The payments team", "members": [{ "user": "ivy", "roles": ["editor"] }] })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["members"][0]["user"], json!("ivy"));
    let (s, v) = send(
        &app,
        "PUT",
        "/_rustybin/projects/payments",
        &admin(),
        Some(json!({ "members": [{ "user": "nobody", "roles": ["editor"] }] })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, _) = send(
        &app,
        "PUT",
        "/_rustybin/projects/instance",
        &admin(),
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = send(
        &app,
        "PUT",
        "/_rustybin/projects/Bad_Name",
        &admin(),
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // A role in use in a project is never deleted.
    state
        .users
        .save_role(crate::rbac::CustomRole {
            name: "pay-reader".into(),
            description: String::new(),
            permissions: vec!["mocks.read".into()],
        })
        .expect("role");
    bind(&state, "payments", &ivy, &["pay-reader"]);
    assert!(state.users.delete_role("pay-reader").is_err());
    // A custom manager of users grants in a project only what it holds there.
    state
        .users
        .save_role(crate::rbac::CustomRole {
            name: "people".into(),
            description: String::new(),
            permissions: vec![
                "users.manage".into(),
                "users.read".into(),
                "mocks.read".into(),
            ],
        })
        .expect("role");
    add_user(&state, "pat", &["people"]);
    let pat = sign_in(&app, "pat").await;
    let (s, v) = send(
        &app,
        "PUT",
        "/_rustybin/projects/payments",
        &pat,
        Some(json!({ "members": [{ "user": "ivy", "roles": ["editor"] }] })),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    // Nobody changes their own project roles.
    let ivy_token = {
        state
            .users
            .set_roles(&ivy, vec!["admin".into()])
            .expect("admin");
        sign_in(&app, "ivy").await
    };
    let (s, v) = send(
        &app,
        "PUT",
        "/_rustybin/projects/payments",
        &ivy_token,
        Some(json!({ "members": [{ "user": "ivy", "roles": ["admin"] }] })),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["reason"], json!("own_roles"));
    // The users view shows the bindings; an invitation may give project
    // roles only.
    let (_, v) = send(&app, "GET", "/_rustybin/users/ivy", &admin(), None).await;
    assert_eq!(v["projects"][0]["project"], json!("payments"), "{v}");
    let (s, v) = send(&app, "POST", "/_rustybin/users", &admin(), Some(json!({ "name": "quinn", "projects": [{ "project": "payments", "roles": ["tester"] }] }))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let quinn = state.users.find("quinn").expect("quinn").id;
    assert_eq!(state.users.bindings(&quinn)[0].roles, vec!["tester"]);
    let (s, _) = send(
        &app,
        "POST",
        "/_rustybin/users",
        &admin(),
        Some(json!({ "name": "rex" })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // A deleted user leaves every project.
    state.users.delete(&quinn).expect("delete");
    assert!(state
        .users
        .project("payments")
        .expect("p")
        .members
        .iter()
        .all(|m| m.user != quinn));
}

/// The cap of 100 projects.
#[test]
fn at_most_a_hundred_projects() {
    let users = crate::rbac::Users::in_memory();
    for i in 0..projects::MAX_PROJECTS {
        users
            .save_project(&format!("p{i}"), None, None)
            .expect("project");
    }
    assert!(matches!(
        users.save_project("one-more", None, None),
        Err(crate::rbac::UsersError::Full(_))
    ));
}

/// A project's `admin` never counts as an instance admin: the last enabled
/// admin stays, instance-wide.
#[test]
fn project_roles_never_count_as_an_instance_admin() {
    let users = crate::rbac::Users::in_memory();
    users.save_project("pay", None, None).expect("project");
    let root = users
        .add(NewUser::new("root", &["admin"]), None)
        .expect("u")
        .id;
    let other = users
        .add(NewUser::new("other", &["viewer"]), None)
        .expect("u")
        .id;
    users
        .set_binding("pay", &other, vec!["admin".into()])
        .expect("bind");
    assert_eq!(users.enabled_admins(), 1);
    let err = users
        .set_roles(&root, vec!["viewer".into()])
        .expect_err("last admin");
    assert!(matches!(
        err,
        crate::rbac::UsersError::Refused(crate::rbac::users::LAST_ADMIN, _)
    ));
    // The project admin holds every permission in pay only.
    let r = users.resolve(&other).expect("other");
    assert!(r.projects["pay"].permissions.is_all());
    assert!(!r.permissions.has(Permission::UsersManage));
}

/// `projects:` and `project:` round trip through a document; a pulled one
/// creates projects and describes them, never changes members nor moves a
/// mock.
#[tokio::test]
async fn the_document_round_trips_projects_and_a_pull_cannot_rebind() {
    let f = fixture().await;
    let doc = crate::configdoc::export(&f.state, crate::configdoc::SecretsMode::Refs, false).file;
    let projects = doc.projects.clone().expect("projects");
    assert_eq!(projects.len(), 2);
    assert_eq!(projects[0].members[0].user, "alice");
    let yaml = serde_yaml::to_string(&doc).expect("yaml");
    assert!(yaml.contains("project: a"), "{yaml}");
    // Onto a fresh instance with the same users: the same document again.
    let state = test_state_with(password_config());
    add_user(&state, "alice", &[]);
    add_user(&state, "bob", &[]);
    let opts = crate::configdoc::ApplyOptions {
        prune: true,
        dry_run: false,
        principal: "test".into(),
        base: None,
        untrusted: false,
        caller: None,
        base_url: None,
    };
    let file = crate::mocks::config_file::parse(&yaml).expect("parse");
    let out = crate::configdoc::apply(&state, file, &opts);
    let again = crate::configdoc::export(&state, crate::configdoc::SecretsMode::Refs, false).file;
    assert_eq!(again.projects, doc.projects, "{out}");
    assert_eq!(
        state.mocks.get("ma").expect("ma").project.as_deref(),
        Some("a")
    );
    assert_eq!(
        state.saved_tests.get("tb").expect("tb").project.as_deref(),
        Some("b")
    );
    // From the repository: a new project and a description apply; a member
    // change, a move and a prune are reported and kept.
    let pulled = yaml
        .replace("project: a", "project: b")
        .replace("- name: b\n", "- name: b\n  description: Bob's team\n")
        + "";
    let mut file = crate::mocks::config_file::parse(&pulled).expect("parse");
    if let Some(p) = file.projects.as_mut() {
        p.push(crate::configdoc::ProjectDoc {
            name: "c".into(),
            description: String::new(),
            members: vec![crate::configdoc::projects::MemberDoc {
                user: "bob".into(),
                roles: vec!["admin".into()],
            }],
        });
        p[0].members.clear();
    }
    let opts = crate::configdoc::ApplyOptions {
        untrusted: true,
        ..opts
    };
    let out = crate::configdoc::apply(&state, file, &opts);
    let problems = out["problems"].to_string();
    assert!(
        problems.contains(
            "projects.a: a document from the repository cannot change a project's members"
        ),
        "{problems}"
    );
    assert!(
        problems.contains("mocks.ma: a document from the repository cannot move it"),
        "{problems}"
    );
    assert_eq!(
        state.mocks.get("ma").expect("ma").project.as_deref(),
        Some("a")
    );
    assert!(state.users.project_exists("c"));
    assert!(state.users.project("c").expect("c").members.is_empty());
    assert_eq!(state.users.project("a").expect("a").members.len(), 1);
}

/// `sso.role_map` entries name a project (`{group, project, roles}`, kept
/// as `group=project:role`).
#[test]
fn sso_groups_give_roles_in_projects() {
    assert!(crate::sso::parse_role_map_entry("pay-team=payments:editor viewer").is_ok());
    assert!(crate::sso::parse_role_map_entry("pay-team=Bad:editor").is_err());
    let users = crate::rbac::Users::in_memory();
    users.save_project("payments", None, None).expect("project");
    let setup = crate::sso::Setup {
        role_map: vec![
            ("pay-team".into(), vec!["payments:editor".into()]),
            ("all".into(), vec!["viewer".into(), "gone:editor".into()]),
        ],
        default_roles: vec!["viewer".into()],
        issuer: "https://idp.example".into(),
        client_id: "rustybin".into(),
        client_secret: None,
        scopes: vec!["openid".into()],
        groups_claim: "groups".into(),
        allowed_domains: Vec::new(),
        provision: true,
        name: "IdP".into(),
    };
    let known_role = |r: &str| users.role_permissions(r).is_some();
    let (roles, _) = crate::sso::map_roles(&setup, &["pay-team".into()], known_role);
    assert!(
        roles.is_empty(),
        "a group with project roles takes no default roles: {roles:?}"
    );
    let (mapped, unknown) = crate::sso::map_project_roles(
        &setup,
        &["pay-team".into(), "all".into()],
        known_role,
        |p| users.project_exists(p),
    );
    assert_eq!(mapped["payments"], vec!["editor"]);
    assert_eq!(unknown, vec!["gone:editor"]);
    // A user of the provider bound only through a group signs in, holding
    // the project's roles.
    let user = users
        .sso_sign_in(
            &crate::rbac::SsoIdentity {
                issuer: "https://idp.example".into(),
                subject: "s-1".into(),
                name: "pia".into(),
                display_name: None,
                email: None,
                email_verified: false,
                mapped_roles: Vec::new(),
                mapped_projects: mapped,
            },
            true,
        )
        .expect("signed in");
    let r = users.resolve(&user.id).expect("pia");
    assert!(r.projects["payments"]
        .permissions
        .has(Permission::MocksWrite));
    assert!(users.bindings(&user.id)[0].sso);
}

/// An agent acting for a member (the member's console token on the
/// management server) lists the tools the member holds somewhere and
/// reaches the member's project only.
#[tokio::test]
async fn an_agent_acting_for_a_member_follows_their_bindings() {
    let f = fixture().await;
    let call = |tool: &'static str, args: Value| {
        let app = f.app.clone();
        let auth = f.alice.clone();
        async move {
            let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": tool, "arguments": args,
                "_meta": {
                    crate::mcp::protocol::META_PROTOCOL_VERSION: "2026-07-28",
                    crate::mcp::protocol::META_CLIENT_CAPABILITIES: {},
                    crate::mcp::protocol::META_CLIENT_INFO: { "name": "member-agent", "version": "1" },
                }
            } });
            let req = Request::builder()
                .method("POST")
                .uri(crate::mcp::manage::MANAGE_PATH)
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", "tools/call")
                .header("mcp-name", tool)
                .header("authorization", auth)
                .body(Body::from(body.to_string()))
                .expect("request");
            let resp = app.oneshot(req).await.expect("response");
            body_json(resp).await["result"].clone()
        }
    };
    let r = call("list_mocks", json!({})).await;
    assert_ne!(r["isError"], json!(true), "{r}");
    let listed: Vec<&str> = r["structuredContent"]["mocks"]
        .as_array()
        .expect("mocks")
        .iter()
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert_eq!(listed, vec!["ma"], "{r}");
    let r = call("get_mock", json!({ "name": "mb" })).await;
    assert_eq!(r["isError"], json!(true), "{r}");
    let r = call("delete_mock", json!({ "name": "mb", "confirm": true })).await;
    assert_eq!(r["isError"], json!(true), "{r}");
    assert!(f.state.mocks.get("mb").is_some());
    let r = call(
        "set_mock_enabled",
        json!({ "name": "ma", "enabled": false }),
    )
    .await;
    assert_ne!(r["isError"], json!(true), "{r}");
    let r = call("list_tests", json!({})).await;
    assert_eq!(
        r["structuredContent"]["untrusted_tests"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "{r}"
    );
    let r = call("list_checkpoints", json!({})).await;
    assert_eq!(
        r["structuredContent"]["checkpoints"][0]["name"],
        json!("ca"),
        "{r}"
    );
    let r = call("list_projects", json!({})).await;
    assert_eq!(r["structuredContent"]["count"], json!(1), "{r}");
    // An instance-wide tool stays out of reach.
    let r = call("get_settings", json!({})).await;
    assert_eq!(r["isError"], json!(true), "{r}");
    // A role taken away in the project applies at the next call.
    let alice = f.state.users.find("alice").expect("alice").id;
    bind(&f.state, "a", &alice, &["viewer"]);
    let r = call("set_mock_enabled", json!({ "name": "ma", "enabled": true })).await;
    assert_eq!(r["isError"], json!(true), "{r}");
    let _ = Member {
        user: String::new(),
        roles: Vec::new(),
    };
}
