//! Open and closed items (access control step 13): the locked catalogue
//! holds no credential, the routes signed out and signed in, closing a mock
//! without mocked security, the portal session (refused everywhere outside
//! `/portal`, resolved at every request), `portal.auth`, a console sign-in
//! by a portal reader, the shared throttle, documents.

use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::control_auth::ControlAuth;
use crate::rbac::NewUser;
use crate::state::AppState;
use crate::test_support::{body_bytes, test_app_with, test_state_with};

const ADMIN: &str = "admin-secret-13";
const READER_PASSWORD: &str = "reader-secret-13";
const PLANT_KEY: &str = "PLANT-KEY-1x9";
const PLANT_USER: &str = "plant-user-7q";
const PLANT_PASS: &str = "PLANT-PASS-2k4";
const PLANT_TOKEN: &str = "PLANT-TOKEN-3m8";

/// A cheap argon2id hash of the reader's password (the parameters travel
/// with the hash, so verifying it is fast too).
fn reader_hash() -> String {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        use argon2::password_hash::{PasswordHasher, SaltString};
        let a = argon2::Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(8, 1, 1, None).expect("params"),
        );
        let salt = SaltString::encode_b64(b"step-13-salt-xyz").expect("salt");
        a.hash_password(READER_PASSWORD.as_bytes(), &salt)
            .expect("hash")
            .to_string()
    })
    .clone()
}

fn password_config() -> Config {
    let mut c = Config::for_tests();
    c.control_auth = ControlAuth::Password;
    c.admin_token = Some(ADMIN.into());
    c.portal = Some("on".into());
    c
}

fn setup(c: Config) -> (Router, AppState) {
    let state = test_state_with(c);
    (test_app_with(state.clone()), state)
}

/// A user with `roles` and the reader's password.
fn add_user(state: &AppState, name: &str, roles: &[&str]) -> String {
    state
        .users
        .add(NewUser::new(name, roles), Some(reader_hash()))
        .expect("user")
        .id
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
    text: String,
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> Answer {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "rustybin.test");
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
    let headers = resp.headers().clone();
    let text = String::from_utf8_lossy(&body_bytes(resp).await).into_owned();
    Answer {
        status,
        headers,
        body: serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
    }
}

fn admin() -> (&'static str, String) {
    ("authorization", format!("Bearer {ADMIN}"))
}

async fn as_admin(app: &Router, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let (k, v) = admin();
    call(app, method, uri, &[(k, &v)], body).await
}

/// Sign in to the portal: the cookie pair (`rustybin_portal=...`).
async fn portal_login(app: &Router, user: &str) -> (Answer, Option<String>) {
    let a = call(
        app,
        "POST",
        session::LOGIN_PATH,
        &[],
        Some(json!({ "username": user, "password": READER_PASSWORD })),
    )
    .await;
    let cookie = a
        .headers
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(str::to_string);
    (a, cookie)
}

/// A spec whose operations need an API key in a header, one in the query,
/// one in a cookie, Basic and a bearer token (with the header key: a token
/// alone is no credential a closed item can rest on).
const PLANTED_SPEC: &str = r#"{
  "openapi": "3.0.3",
  "info": { "title": "Planted", "version": "1" },
  "components": { "securitySchemes": {
    "hk": { "type": "apiKey", "in": "header", "name": "x-api-key" },
    "qk": { "type": "apiKey", "in": "query", "name": "key" },
    "ck": { "type": "apiKey", "in": "cookie", "name": "session" },
    "basic": { "type": "http", "scheme": "basic" },
    "bearer": { "type": "http", "scheme": "bearer" }
  } },
  "paths": {
    "/h": { "get": { "operationId": "h", "security": [{ "hk": [] }], "responses": { "200": { "description": "ok" } } } },
    "/q": { "get": { "operationId": "q", "security": [{ "qk": [] }], "responses": { "200": { "description": "ok" } } } },
    "/c": { "get": { "operationId": "c", "security": [{ "ck": [] }], "responses": { "200": { "description": "ok" } } } },
    "/b": { "get": { "operationId": "b", "security": [{ "basic": [] }], "responses": { "200": { "description": "ok" } } } },
    "/t": { "post": { "operationId": "t", "security": [{ "hk": [], "bearer": [] }], "requestBody": { "content": { "application/json": { "schema": { "type": "object" } } } }, "responses": { "200": { "description": "ok" } } } }
  }
}"#;

async fn plant_mock(app: &Router) {
    let a = as_admin(
        app,
        "POST",
        "/_rustybin/mocks",
        Some(json!({
            "name": "planted",
            "spec": PLANTED_SPEC,
            "deploy": true,
            "security": {
                "enabled": true,
                "mode": "spec",
                "api_key": { "header": "x-api-key", "keys": [PLANT_KEY] },
                "basic": { "users": { PLANT_USER: PLANT_PASS } },
                "bearer": { "tokens": [PLANT_TOKEN] },
            },
            "mcp": { "enabled": true },
        })),
    )
    .await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
}

/// Every kind of item, closed (and one MCP server open, for contrast).
const CLOSED_ITEMS: &[&str] = &[
    "mock:planted",
    "mcp:main",
    "mcp:protected",
    "mcp:apikey",
    "mcp:sse",
    "llm",
    "a2a:secure",
    "a2a:echo",
    "tools",
    "registry",
];

async fn publish_all(app: &Router) {
    for id in CLOSED_ITEMS {
        let a = as_admin(
            app,
            "PUT",
            &format!("/_rustybin/portal/items/{id}"),
            Some(json!({ "access": "closed" })),
        )
        .await;
        assert_eq!(a.status, StatusCode::CREATED, "{id}: {}", a.text);
        assert_eq!(a.body["published"]["access"], "closed", "{id}");
    }
    let a = as_admin(app, "PUT", "/_rustybin/portal/items/mcp:weather", None).await;
    assert_eq!(a.status, StatusCode::CREATED);
}

fn item<'a>(catalog: &'a Value, id: &str) -> &'a Value {
    catalog["items"]
        .as_array()
        .and_then(|items| items.iter().find(|i| i["id"] == id))
        .unwrap_or(&Value::Null)
}

fn basic_of(user: &str, pass: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
}

/// A concrete path for a template.
fn fill(template: &str) -> String {
    template
        .split('/')
        .map(|seg| match seg {
            "{name}" => "petstore",
            "{op}" => "listPets",
            "{format}" => "postman",
            s if s.starts_with('{') => "x",
            s => s,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The key safety test: credentials planted in every kind of item never
/// reach a caller who is not a reader, and reach one who is.
#[tokio::test]
async fn the_locked_catalogue_holds_no_credential() {
    let (app, state) = setup(password_config());
    plant_mock(&app).await;
    publish_all(&app).await;
    let secrets = [
        PLANT_KEY.to_string(),
        PLANT_USER.to_string(),
        PLANT_PASS.to_string(),
        PLANT_TOKEN.to_string(),
        basic_of(PLANT_USER, PLANT_PASS),
    ];

    let signed_out = call(&app, "GET", CATALOG_PATH, &[], None).await;
    assert_eq!(signed_out.status, StatusCode::OK, "{}", signed_out.text);
    for s in &secrets {
        assert!(!signed_out.text.contains(s.as_str()), "{s} leaked");
    }
    for id in CLOSED_ITEMS {
        let it = item(&signed_out.body, id);
        assert_eq!(it["access"], "closed", "{id}");
        assert_eq!(it["locked"], true, "{id}");
        let text = it.to_string();
        for needle in [
            "demo-key",
            "/collections/",
            "claude mcp add",
            "mcpServers",
            "Bearer demo",
        ] {
            assert!(!text.contains(needle), "{id} carries {needle}: {text}");
        }
        // The docs are there.
        assert!(!it["title"].as_str().unwrap_or("").is_empty(), "{id}");
    }
    assert!(
        !item(&signed_out.body, "tools")["detail"]
            .to_string()
            .contains("\"examples\""),
        "the locked tools carry their examples"
    );
    let mock = item(&signed_out.body, "mock:planted");
    let d = &mock["detail"];
    assert_eq!(d["downloads"], json!([]));
    assert_eq!(d["credentials"], false);
    assert_eq!(d["operations"].as_array().map(Vec::len), Some(5));
    assert!(d["mcp"]["url"].is_null() && d["mcp"]["tools"].is_array());
    assert!(d["spec_url"].is_string() && d["reference_path"].is_string());
    // The curl lines show the request with a placeholder.
    let curls = d["operations"].to_string();
    assert!(
        curls.contains("<api-key>") && curls.contains("<token>"),
        "{curls}"
    );
    assert!(curls.contains("<base64 user:password>"), "{curls}");
    // The open item keeps its snippets.
    let weather = item(&signed_out.body, "mcp:weather");
    assert_eq!(weather["locked"], false);
    assert!(weather["detail"]["claude"].is_string());

    // A reader (a bearer control credential with portal.read) gets them.
    let (k, v) = admin();
    let reader = call(&app, "GET", CATALOG_PATH, &[(k, &v)], None).await;
    assert_eq!(reader.status, StatusCode::OK);
    assert_eq!(reader.body["portal"]["reader"], true);
    for s in [PLANT_KEY, PLANT_TOKEN, &basic_of(PLANT_USER, PLANT_PASS)] {
        assert!(reader.text.contains(s), "{s} missing for a reader");
    }
    for id in CLOSED_ITEMS {
        assert_eq!(item(&reader.body, id)["locked"], false, "{id}");
    }
    let rd = &item(&reader.body, "mock:planted")["detail"];
    assert!(rd["downloads"].as_array().is_some_and(|d| !d.is_empty()));
    assert!(rd["mcp"]["claude"].is_string());
    assert!(item(&reader.body, "mcp:apikey")["detail"]["json"]
        .to_string()
        .contains("demo-key"));
    assert!(item(&reader.body, "tools")["detail"]
        .to_string()
        .contains("\"examples\""));
    // The answers are kept apart by credential.
    assert_ne!(signed_out.headers.get("etag"), reader.headers.get("etag"));
    assert!(reader.headers["cache-control"]
        .to_str()
        .is_ok_and(|v| v.contains("private")));
    assert!(reader.headers["vary"]
        .to_str()
        .is_ok_and(|v| v.contains("Cookie")));
    // And a portal session (cookie) is a reader too.
    add_user(&state, "tess", &["portal_reader"]);
    let (_, cookie) = portal_login(&app, "tess").await;
    let cookie = cookie.expect("cookie");
    let by_cookie = call(&app, "GET", CATALOG_PATH, &[("cookie", &cookie)], None).await;
    assert!(by_cookie.text.contains(PLANT_KEY));
}

#[tokio::test]
async fn every_route_of_a_closed_item_signed_out_and_signed_in() {
    let (app, state) = setup(password_config());
    plant_mock(&app).await;
    publish_all(&app).await;
    add_user(&state, "tess", &["portal_reader"]);
    let (_, cookie) = portal_login(&app, "tess").await;
    let cookie = cookie.expect("cookie");
    let with = [("cookie", cookie.as_str())];
    let download = "/portal/api/items/mock:planted/collections/hurl";

    // Collections: 401 signed out, the file signed in.
    let a = call(&app, "GET", download, &[], None).await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED, "{}", a.text);
    assert_eq!(a.body["reason"], "signin_required");
    assert!(!a.text.contains(PLANT_KEY));
    let a = call(&app, "GET", download, &with, None).await;
    assert_eq!(a.status, StatusCode::OK);
    assert!(a.text.contains(PLANT_KEY), "the collection carries the key");
    // The reference and the mock's spec: readable by everyone.
    let a = call(&app, "GET", "/portal/reference/mock:planted", &[], None).await;
    assert_eq!(a.status, StatusCode::OK);
    assert!(!a.text.contains(PLANT_KEY));
    let a = call(
        &app,
        "GET",
        "/mock/planted/_rustybin/openapi.json",
        &[],
        None,
    )
    .await;
    assert_eq!(a.status, StatusCode::OK);
    assert!(!a.text.contains(PLANT_KEY));
    // Events: views for everyone, Try it only for readers.
    let view = json!({ "item": "mock:planted", "event": "view" });
    let try_it = json!({ "item": "mock:planted", "event": "try" });
    let a = call(&app, "POST", EVENTS_PATH, &[], Some(view.clone())).await;
    assert_eq!(a.status, StatusCode::NO_CONTENT);
    let a = call(&app, "POST", EVENTS_PATH, &[], Some(try_it.clone())).await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED);
    let a = call(&app, "POST", EVENTS_PATH, &with, Some(try_it)).await;
    assert_eq!(a.status, StatusCode::NO_CONTENT);
    let a = call(
        &app,
        "POST",
        EVENTS_PATH,
        &[],
        Some(json!({ "item": "mcp:weather", "event": "try" })),
    )
    .await;
    assert_eq!(a.status, StatusCode::NO_CONTENT, "an open item's Try it");
    // The calls need the credentials: refused without, served with.
    let a = call(&app, "GET", "/mock/planted/h", &[], None).await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED);
    let a = call(
        &app,
        "GET",
        "/mock/planted/h",
        &[("x-api-key", PLANT_KEY)],
        None,
    )
    .await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    // The session answer.
    let a = call(&app, "GET", session::SESSION_PATH, &[], None).await;
    assert_eq!(
        a.body,
        json!({ "signed_in": false, "reader": false, "signin": true })
    );
    let a = call(&app, "GET", session::SESSION_PATH, &with, None).await;
    assert_eq!(a.body["signed_in"], true);
    assert_eq!(a.body["user"], "tess");
    assert_eq!(a.body["reader"], true);
    // Sign out: the cookie is cleared and the session refused after.
    let a = call(&app, "POST", session::LOGOUT_PATH, &with, None).await;
    assert_eq!(a.body["ended"], true);
    assert!(a.headers["set-cookie"]
        .to_str()
        .is_ok_and(|c| c.contains("Max-Age=0")));
    let a = call(&app, "GET", download, &with, None).await;
    assert_eq!(
        a.status,
        StatusCode::UNAUTHORIZED,
        "the old cookie is revoked"
    );
}

#[tokio::test]
async fn the_portal_cookie_is_well_formed_and_signs_in_like_the_console() {
    let (app, state) = setup(password_config());
    add_user(&state, "tess", &["portal_reader"]);
    let (a, cookie) = portal_login(&app, "tess").await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    assert_eq!(a.body["reader"], true);
    assert!(cookie.is_some_and(|c| c.starts_with("rustybin_portal=")));
    let set = a.headers["set-cookie"]
        .to_str()
        .expect("cookie")
        .to_string();
    for part in [
        "HttpOnly",
        "SameSite=Strict",
        "Path=/portal",
        "Max-Age=43200",
    ] {
        assert!(set.contains(part), "{set}");
    }
    assert!(!set.contains("Secure"), "plain HTTP: {set}");
    let a = call(
        &app,
        "POST",
        session::LOGIN_PATH,
        &[("x-forwarded-proto", "https")],
        Some(json!({ "username": "tess", "password": READER_PASSWORD })),
    )
    .await;
    let set = a.headers["set-cookie"]
        .to_str()
        .expect("cookie")
        .to_string();
    if state.config.trust_forward {
        assert!(set.contains("Secure"), "{set}");
    }
    // A wrong password and a disabled user get the same answer.
    let a = call(
        &app,
        "POST",
        session::LOGIN_PATH,
        &[],
        Some(json!({ "username": "tess", "password": "wrong-password" })),
    )
    .await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED);
    assert_eq!(a.body["reason"], "invalid_credentials");
    let off = add_user(&state, "gone", &["portal_reader"]);
    state.users.set_disabled(&off, true).expect("disable");
    let b = call(
        &app,
        "POST",
        session::LOGIN_PATH,
        &[],
        Some(json!({ "username": "gone", "password": READER_PASSWORD })),
    )
    .await;
    assert_eq!(b.status, a.status);
    assert_eq!(b.body, a.body);
    // The sign-in is off outside the password sign-in.
    let mut open = Config::for_tests();
    open.portal = Some("on".into());
    let (oapp, _) = setup(open);
    let a = call(
        &oapp,
        "POST",
        session::LOGIN_PATH,
        &[],
        Some(json!({ "username": "x", "password": "y" })),
    )
    .await;
    assert_eq!(a.status, StatusCode::NOT_FOUND);
    assert_eq!(a.body["reason"], "signin_off");
}

#[tokio::test]
async fn the_sign_in_throttle_is_shared_with_the_console() {
    let (app, state) = setup(password_config());
    add_user(&state, "tess", &["portal_reader"]);
    for _ in 0..crate::login::FAILURES_PER_WINDOW {
        let a = call(
            &app,
            "POST",
            crate::login::LOGIN_PATH,
            &[],
            Some(json!({ "username": "tess", "password": "wrong-password" })),
        )
        .await;
        assert_eq!(a.status, StatusCode::UNAUTHORIZED);
    }
    let (a, cookie) = portal_login(&app, "tess").await;
    assert_eq!(a.status, StatusCode::TOO_MANY_REQUESTS, "{}", a.text);
    assert_eq!(a.body["reason"], "rate_limited");
    assert!(cookie.is_none());
}

/// The portal session is accepted under `/portal` only: sent as a cookie,
/// a bearer and the admin token header, it opens no control route, no
/// data-plane route that a credential opens, and not `/_rustybin/mcp`.
#[tokio::test]
async fn the_portal_cookie_is_refused_everywhere_else() {
    let (app, state) = setup(password_config());
    add_user(&state, "ada", &["admin"]);
    let (_, cookie) = portal_login(&app, "ada").await;
    let cookie = cookie.expect("cookie");
    let token = cookie
        .strip_prefix("rustybin_portal=")
        .expect("token")
        .to_string();
    let bearer = format!("Bearer {token}");
    let creds = [
        ("cookie", cookie.as_str()),
        ("authorization", bearer.as_str()),
    ];
    let header_token = [
        ("cookie", cookie.as_str()),
        ("x-rustybin-admin-token", token.as_str()),
    ];
    // The token itself is no control credential.
    assert!(crate::control_auth::credentials_resolved(
        &{
            let mut h = HeaderMap::new();
            h.insert("authorization", bearer.parse().expect("h"));
            h
        },
        &state.config
    )
    .is_err());
    assert!(crate::control_oauth::authorize_manage(
        &Request::builder()
            .uri("/_rustybin/mcp")
            .header("authorization", bearer.as_str())
            .body(Body::empty())
            .expect("request"),
        &state.config
    )
    .is_none_or(|r| r.is_err()));
    let mut wrong = Vec::new();
    let mut checked = 0;
    for ep in crate::catalog::all() {
        if ep.protocol != crate::catalog::Protocol::Http || ep.path.starts_with("/portal") {
            continue;
        }
        let control = ep.is_control();
        for m in ep.expanded_methods() {
            let path = ep
                .examples
                .iter()
                .find(|e| e.method == m)
                .map(|e| e.path.to_string())
                .unwrap_or_else(|| fill(ep.path));
            // /auth/jwt/decode decodes any bearer JWT without verifying it
            // (it accepts nothing).
            if path.contains("/delay")
                || path.contains("/drip")
                || path.contains("/stream")
                || path.starts_with("/auth/jwt/decode")
            {
                continue;
            }
            for headers in [&creds[..], &header_token[..]] {
                let a = call(&app, m, &path, headers, None).await;
                checked += 1;
                if control {
                    let open = ep.need(m) == Some(crate::rbac::Need::Open);
                    if !open && a.status != StatusCode::UNAUTHORIZED {
                        wrong.push(format!("{m} {path}: {}", a.status));
                    }
                } else if a.status.is_success() {
                    // Accepted with it: it must be accepted without it too.
                    let b = call(&app, m, &path, &[], None).await;
                    if b.status == StatusCode::UNAUTHORIZED || b.status == StatusCode::FORBIDDEN {
                        wrong.push(format!(
                            "{m} {path}: {} with the cookie, {} without",
                            a.status, b.status
                        ));
                    }
                }
            }
        }
    }
    assert!(checked > 300, "{checked}");
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[tokio::test]
async fn a_role_removed_or_a_user_disabled_hides_closed_items_at_once() {
    let (app, state) = setup(password_config());
    plant_mock(&app).await;
    publish_all(&app).await;
    let a = as_admin(
        &app,
        "PUT",
        "/_rustybin/roles/mocks-only",
        Some(json!({ "permissions": ["mocks.read"] })),
    )
    .await;
    assert!(a.status.is_success(), "{}", a.text);
    let id = add_user(&state, "tess", &["portal_reader"]);
    let (_, cookie) = portal_login(&app, "tess").await;
    let cookie = cookie.expect("cookie");
    let with = [("cookie", cookie.as_str())];
    let reads = |c: &Value| item(c, "mock:planted")["locked"] == false;
    let a = call(&app, "GET", CATALOG_PATH, &with, None).await;
    assert!(reads(&a.body));
    // The role goes: the next request is locked.
    let a = as_admin(
        &app,
        "PATCH",
        &format!("/_rustybin/users/{id}"),
        Some(json!({ "roles": ["mocks-only"] })),
    )
    .await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    let a = call(&app, "GET", CATALOG_PATH, &with, None).await;
    assert!(!reads(&a.body));
    assert!(!a.text.contains(PLANT_KEY));
    let a = call(&app, "GET", session::SESSION_PATH, &with, None).await;
    assert_eq!(a.body["signed_in"], true);
    assert_eq!(a.body["reader"], false);
    let a = call(
        &app,
        "GET",
        "/portal/api/items/mock:planted/collections/hurl",
        &with,
        None,
    )
    .await;
    assert_eq!(a.status, StatusCode::FORBIDDEN);
    assert_eq!(a.body["permission"], "portal.read");
    // Back, then disabled: the session ends.
    as_admin(
        &app,
        "PATCH",
        &format!("/_rustybin/users/{id}"),
        Some(json!({ "roles": ["portal_reader"] })),
    )
    .await;
    let a = call(&app, "GET", CATALOG_PATH, &with, None).await;
    assert!(reads(&a.body), "the role is back");
    let a = as_admin(
        &app,
        "PATCH",
        &format!("/_rustybin/users/{id}"),
        Some(json!({ "disabled": true })),
    )
    .await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    let a = call(&app, "GET", CATALOG_PATH, &with, None).await;
    assert!(!reads(&a.body));
    let a = call(&app, "GET", session::SESSION_PATH, &with, None).await;
    assert_eq!(a.body["signed_in"], false);
}

#[tokio::test]
async fn closing_a_mock_needs_its_security_and_secure_turns_it_on() {
    let (app, _) = setup(password_config());
    // The sample has no mocked security on.
    let a = as_admin(
        &app,
        "PUT",
        "/_rustybin/portal/items/mock:petstore",
        Some(json!({ "access": "closed" })),
    )
    .await;
    assert_eq!(a.status, StatusCode::CONFLICT, "{}", a.text);
    assert_eq!(a.body["reason"], "security_off");
    assert_eq!(a.body["fix"]["secure"], true);
    assert_eq!(a.body["closable_modes"], json!(["api_key", "basic"]));
    let pet = || Some(json!({ "name": "Rex", "status": "available" }));
    let a = call(&app, "POST", "/mock/petstore/v1/pets", &[], pet()).await;
    assert!(a.status.is_success(), "still open: {}", a.text);
    // secure needs access: closed, and a mock.
    let a = as_admin(
        &app,
        "PUT",
        "/_rustybin/portal/items/mock:petstore",
        Some(json!({ "secure": true })),
    )
    .await;
    assert_eq!(a.status, StatusCode::BAD_REQUEST);
    let a = as_admin(
        &app,
        "PUT",
        "/_rustybin/portal/items/llm",
        Some(json!({ "access": "closed", "secure": true })),
    )
    .await;
    assert_eq!(a.status, StatusCode::BAD_REQUEST);
    // One step: security on, deployed, closed.
    let a = as_admin(
        &app,
        "PUT",
        "/_rustybin/portal/items/mock:petstore",
        Some(json!({ "access": "closed", "secure": true })),
    )
    .await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    assert_eq!(a.body["secured_now"], true);
    assert_eq!(a.body["published"]["access"], "closed");
    assert_eq!(a.body["secured"], true);
    let a = call(&app, "POST", "/mock/petstore/v1/pets", &[], pet()).await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED, "{}", a.text);
    // The spec's bearer alternative takes a token anyone mints, so the mock
    // is closed in api_key mode, behind a random key (never demo-key).
    let a = call(
        &app,
        "POST",
        "/mock/petstore/v1/pets",
        &[("x-api-key", "demo-key")],
        pet(),
    )
    .await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED, "{}", a.text);
    let detail = as_admin(&app, "GET", "/_rustybin/mocks/petstore", None).await;
    let sec = &detail.body["live"]["settings"]["security"];
    let sec = if sec.is_null() {
        &detail.body["settings"]["security"]
    } else {
        sec
    };
    assert_eq!(sec["mode"], "api_key", "{}", detail.text);
    let key = sec["api_key"]["keys"][0].as_str().unwrap_or("").to_string();
    assert!(key.len() >= 32 && key != "demo-key", "{key}");
    let a = call(
        &app,
        "POST",
        "/mock/petstore/v1/pets",
        &[("x-api-key", &key)],
        pet(),
    )
    .await;
    assert!(a.status.is_success(), "{}", a.text);
    // The control view says closed, and counts it.
    let a = as_admin(&app, "GET", CONTROL_PATH, None).await;
    assert_eq!(a.body["counts"]["closed"], 1);
    assert!(a.body["readers"]["users"].as_u64().is_some());
    // Turning the security off again is refused while the item is closed.
    let a = as_admin(
        &app,
        "PATCH",
        "/_rustybin/mocks/petstore",
        Some(json!({ "security": { "enabled": false }, "deploy": true })),
    )
    .await;
    assert_eq!(a.status, StatusCode::CONFLICT, "{}", a.text);
    assert!(a.text.contains("closed on the dev portal"), "{}", a.text);
    let a = call(&app, "POST", "/mock/petstore/v1/pets", &[], pet()).await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED, "{}", a.text);
    // Opening is always allowed.
    let a = as_admin(
        &app,
        "PUT",
        "/_rustybin/portal/items/mock:petstore",
        Some(json!({ "access": "open" })),
    )
    .await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    assert_eq!(a.body["published"]["access"], "open");
}

#[tokio::test]
async fn portal_auth_password_closes_everything_and_key_still_asks_for_the_sign_in() {
    let mut c = password_config();
    c.portal_auth = Some("password".into());
    let (app, state) = setup(c);
    plant_mock(&app).await;
    as_admin(&app, "PUT", "/_rustybin/portal/items/mock:planted", None).await;
    as_admin(&app, "PUT", "/_rustybin/portal/items/llm", None).await;
    let a = call(&app, "GET", CATALOG_PATH, &[], None).await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED);
    assert_eq!(a.body["signin"], true);
    let a = call(&app, "GET", "/portal/reference/mock:planted", &[], None).await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED);
    add_user(&state, "tess", &["portal_reader"]);
    let (_, cookie) = portal_login(&app, "tess").await;
    let cookie = cookie.expect("cookie");
    let with = [("cookie", cookie.as_str())];
    let a = call(&app, "GET", CATALOG_PATH, &with, None).await;
    assert_eq!(a.status, StatusCode::OK);
    for id in ["mock:planted", "llm"] {
        assert_eq!(item(&a.body, id)["access"], "closed", "{id}");
        assert_eq!(item(&a.body, id)["locked"], false, "{id}");
    }
    let a = call(&app, "GET", "/portal/reference/mock:planted", &with, None).await;
    assert_eq!(a.status, StatusCode::OK);
    // A signed-in user without portal.read: refused, naming it.
    as_admin(
        &app,
        "PUT",
        "/_rustybin/roles/mocks-only",
        Some(json!({ "permissions": ["mocks.read"] })),
    )
    .await;
    add_user(&state, "mo", &["mocks-only"]);
    let (_, mo) = portal_login(&app, "mo").await;
    let mo = mo.expect("cookie");
    let a = call(&app, "GET", CATALOG_PATH, &[("cookie", &mo)], None).await;
    assert_eq!(a.status, StatusCode::FORBIDDEN);
    assert_eq!(a.body["permission"], "portal.read");

    // Key mode: a key reads the open items; a closed one is locked until
    // the sign-in.
    use crate::keys::tests::{doc, entry, shared_config, KEY_A};
    let mut kc = shared_config();
    kc.portal_auth = Some("key".into());
    kc.portal = Some("on".into());
    kc.admin_token = Some(ADMIN.into());
    let kstate = test_state_with(kc);
    kstate
        .keys
        .load_bytes(doc(vec![entry("ka1", KEY_A, "starter", "active", json!({}))]).as_bytes())
        .expect("keys");
    let kapp = test_app_with(kstate);
    let a = as_admin(
        &kapp,
        "PUT",
        "/_rustybin/portal/items/mcp:apikey",
        Some(json!({ "access": "closed" })),
    )
    .await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    as_admin(&kapp, "PUT", "/_rustybin/portal/items/mcp:weather", None).await;
    // A shared-mode instance closes a mock on its keys alone.
    let a = as_admin(
        &kapp,
        "PUT",
        "/_rustybin/portal/items/mock:petstore",
        Some(json!({ "access": "closed" })),
    )
    .await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    let a = call(&kapp, "GET", CATALOG_PATH, &[], None).await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED);
    let a = call(
        &kapp,
        "GET",
        CATALOG_PATH,
        &[("x-rustybin-key", KEY_A)],
        None,
    )
    .await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    assert_eq!(item(&a.body, "mcp:apikey")["locked"], true);
    assert_eq!(item(&a.body, "mcp:weather")["locked"], false);
    let (k, v) = admin();
    let a = call(
        &kapp,
        "GET",
        CATALOG_PATH,
        &[("x-rustybin-key", KEY_A), (k, &v)],
        None,
    )
    .await;
    assert_eq!(item(&a.body, "mcp:apikey")["locked"], false);
}

#[tokio::test]
async fn a_portal_reader_signs_in_to_the_portal_not_the_console() {
    let (app, state) = setup(password_config());
    add_user(&state, "tess", &["portal_reader"]);
    let a = call(
        &app,
        "POST",
        crate::login::LOGIN_PATH,
        &[],
        Some(json!({ "username": "tess", "password": READER_PASSWORD })),
    )
    .await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    assert_eq!(a.body["portal_only"], true);
    assert_eq!(a.body["portal_url"], "/portal/");
    // The token serves the account itself (/_rustybin/me), nothing else.
    let t = a.body["token"].as_str().unwrap_or("").to_string();
    let me = call(
        &app,
        "GET",
        "/_rustybin/me",
        &[("authorization", &format!("Bearer {t}"))],
        None,
    )
    .await;
    assert_eq!(me.status, StatusCode::OK);
    let cfg = call(
        &app,
        "GET",
        "/_rustybin/config",
        &[("authorization", &format!("Bearer {t}"))],
        None,
    )
    .await;
    assert_eq!(cfg.status, StatusCode::FORBIDDEN);
    let set = a.headers["set-cookie"]
        .to_str()
        .expect("cookie")
        .to_string();
    assert!(set.starts_with("rustybin_portal=") && set.contains("Path=/portal"));
    let cookie = set.split(';').next().unwrap_or("").to_string();
    let s = call(
        &app,
        "GET",
        session::SESSION_PATH,
        &[("cookie", &cookie)],
        None,
    )
    .await;
    assert_eq!(s.body["user"], "tess");
    // A viewer gets the console token as before.
    add_user(&state, "vic", &["viewer"]);
    let a = call(
        &app,
        "POST",
        crate::login::LOGIN_PATH,
        &[],
        Some(json!({ "username": "vic", "password": READER_PASSWORD })),
    )
    .await;
    assert_eq!(a.status, StatusCode::OK);
    assert!(a.body["token"].is_string());
    assert!(a.body["portal_only"].is_null());
    assert!(a.headers.get("set-cookie").is_none());
    // An invitation of a portal reader links to the portal's join page.
    let a = as_admin(
        &app,
        "POST",
        "/_rustybin/users",
        Some(json!({ "name": "pat", "roles": ["portal_reader"] })),
    )
    .await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    let url = a.body["invitation"]["url"].as_str().unwrap_or("");
    assert!(url.contains("/portal/#/join/rbi_"), "{url}");
    let token = a.body["invitation"]["token"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let a = as_admin(
        &app,
        "POST",
        "/_rustybin/users",
        Some(json!({ "name": "ed", "roles": ["editor"] })),
    )
    .await;
    assert!(a.body["invitation"]["url"]
        .as_str()
        .is_some_and(|u| u.contains("/ui/#/join/")));
    // Joining signs the reader in to the portal (no console token).
    let a = call(
        &app,
        "POST",
        crate::login::JOIN_PATH,
        &[],
        Some(json!({ "token": token, "password": "pat-secret-k3Qx", "portal": true })),
    )
    .await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    assert_eq!(a.body["joined"], true);
    assert_eq!(a.body["portal_only"], true);
    assert!(a.body["token"].is_null());
    assert!(a.headers["set-cookie"]
        .to_str()
        .is_ok_and(|c| c.starts_with("rustybin_portal=")));
}

#[tokio::test]
async fn documents_carry_access_and_a_pulled_one_never_opens() {
    use crate::configdoc::{self, ApplyOptions};
    let (app, state) = setup(password_config());
    plant_mock(&app).await;
    as_admin(
        &app,
        "PUT",
        "/_rustybin/portal/items/mock:planted",
        Some(json!({ "access": "closed" })),
    )
    .await;
    as_admin(&app, "PUT", "/_rustybin/portal/items/llm", None).await;
    let doc = configdoc::export(&state, configdoc::SecretsMode::Refs, false).file;
    let items = &doc.portal.as_ref().expect("portal").items;
    let planted = items
        .iter()
        .find(|i| i.id == "mock:planted")
        .expect("planted");
    assert_eq!(planted.access, Some(Access::Closed));
    let llm = items.iter().find(|i| i.id == "llm").expect("llm");
    assert_eq!(llm.access, None, "open is the default and not written");
    let yaml = configdoc::to_yaml(&doc);
    assert!(yaml.contains("access: closed"), "{yaml}");

    let opts = |untrusted: bool| ApplyOptions {
        prune: false,
        dry_run: false,
        principal: "test".into(),
        base: None,
        untrusted,
        caller: None,
        base_url: None,
    };
    // From the repository: closing applies, opening is refused.
    let pulled = "version: 2\nportal:\n  items:\n    - id: mock:planted\n      access: open\n    - id: llm\n      access: closed\n";
    let file = crate::mocks::config_file::parse(pulled).expect("parse");
    let out = configdoc::apply(&state, file, &opts(true));
    assert_eq!(
        state.portal.get("mock:planted").map(|p| p.access),
        Some(Access::Closed)
    );
    assert_eq!(
        state.portal.get("llm").map(|p| p.access),
        Some(Access::Closed)
    );
    assert!(
        out.to_string().contains("may close an item but not open"),
        "{out}"
    );
    // A trusted document opens it.
    let file = crate::mocks::config_file::parse(pulled).expect("parse");
    configdoc::apply(&state, file, &opts(false));
    assert_eq!(
        state.portal.get("mock:planted").map(|p| p.access),
        Some(Access::Open)
    );
    // A document cannot close a mock without mocked security.
    let file = crate::mocks::config_file::parse(
        "version: 2\nportal:\n  items:\n    - id: mock:petstore\n      access: closed\n",
    )
    .expect("parse");
    let out = configdoc::apply(&state, file, &opts(false));
    assert!(state.portal.get("mock:petstore").is_none());
    assert!(out.to_string().contains("mocked security is off"), "{out}");
}

// ── The security pass (step 14, area B) ─────────────────────────────

/// A spec with one operation and every scheme a caller can satisfy
/// without the portal: an OAuth flow, OpenID Connect, a bearer token and
/// mutual TLS (each operation needs one of them).
const TOKEN_SPEC: &str = r#"{
  "openapi": "3.0.3",
  "info": { "title": "Tokens", "version": "1" },
  "components": { "securitySchemes": {
    "oauth": { "type": "oauth2", "flows": { "clientCredentials": { "tokenUrl": "/oauth/token", "scopes": {} } } },
    "oidc": { "type": "openIdConnect", "openIdConnectUrl": "/.well-known/openid-configuration" },
    "bearer": { "type": "http", "scheme": "bearer" },
    "mtls": { "type": "mutualTLS" }
  } },
  "paths": {
    "/o": { "get": { "operationId": "o", "security": [{ "oauth": [] }], "responses": { "200": { "description": "ok" } } } },
    "/i": { "get": { "operationId": "i", "security": [{ "oidc": [] }], "responses": { "200": { "description": "ok" } } } },
    "/b": { "get": { "operationId": "b", "security": [{ "bearer": [] }], "responses": { "200": { "description": "ok" } } } },
    "/m": { "get": { "operationId": "m", "security": [{ "mtls": [] }], "responses": { "200": { "description": "ok" } } } }
  }
}"#;

/// A token from the built-in identity provider's public demo client.
async fn idp_token(app: &Router) -> String {
    let req = Request::builder()
        .method("POST")
        .uri("/oauth/token")
        .header("host", "rustybin.test")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(
            "grant_type=client_credentials&client_id=rustybin&client_secret=secret",
        ))
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let v: Value = serde_json::from_slice(&body_bytes(resp).await).expect("json");
    v["access_token"].as_str().expect("token").to_string()
}

async fn create_mock(app: &Router, name: &str, spec: &str, security: Value) {
    let a = as_admin(
        app,
        "POST",
        "/_rustybin/mocks",
        Some(json!({ "name": name, "spec": spec, "deploy": true, "security": security })),
    )
    .await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
}

async fn close(app: &Router, id: &str, extra: Value) -> Answer {
    let mut body = json!({ "access": "closed" });
    if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
        b.extend(e.clone());
    }
    as_admin(
        app,
        "PUT",
        &format!("/_rustybin/portal/items/{id}"),
        Some(body),
    )
    .await
}

const PET_SPEC_OPEN: &str = r#"{"openapi":"3.0.3","info":{"title":"P","version":"1"},"paths":{"/p":{"get":{"operationId":"p","responses":{"200":{"description":"ok"}}}}}}"#;

/// Finding 1: a mock whose credentials anyone can get without the portal
/// sign-in (JWTs of the built-in identity provider, whose demo client is
/// public; the downloadable demo client certificate) cannot be closed.
#[tokio::test]
async fn a_closed_mock_cannot_rest_on_tokens_anyone_can_mint() {
    let (app, _) = setup(password_config());
    // jwt and bearer modes: a signed-out caller's token is accepted.
    for mode in ["jwt", "bearer"] {
        let name = format!("tok-{mode}");
        create_mock(
            &app,
            &name,
            PET_SPEC_OPEN,
            json!({ "enabled": true, "mode": mode, "bearer": { "tokens": ["own-token-1x9z7q"] } }),
        )
        .await;
        let token = idp_token(&app).await;
        let bearer = format!("Bearer {token}");
        let a = call(
            &app,
            "GET",
            &format!("/mock/{name}/p"),
            &[("authorization", &bearer)],
            None,
        )
        .await;
        assert_eq!(
            a.status,
            StatusCode::OK,
            "the token is accepted: {}",
            a.text
        );
        let a = close(&app, &format!("mock:{name}"), json!({})).await;
        assert_eq!(a.status, StatusCode::CONFLICT, "{mode}: {}", a.text);
        assert_eq!(a.body["reason"], "obtainable_credentials", "{}", a.text);
        assert!(a.text.contains("built-in identity provider"), "{}", a.text);
    }
    // spec mode with OAuth, OpenID Connect, bearer and mutual TLS schemes.
    create_mock(
        &app,
        "tok-spec",
        TOKEN_SPEC,
        json!({ "enabled": true, "mode": "spec" }),
    )
    .await;
    let a = close(&app, "mock:tok-spec", json!({})).await;
    assert_eq!(a.status, StatusCode::CONFLICT, "{}", a.text);
    assert_eq!(a.body["reason"], "obtainable_credentials");
    // A spec with only the mutual TLS scheme: the demo certificate opens it.
    let mtls_only = TOKEN_SPEC
        .replace(
            r#""security": [{ "oauth": [] }]"#,
            r#""security": [{ "mtls": [] }]"#,
        )
        .replace(
            r#""security": [{ "oidc": [] }]"#,
            r#""security": [{ "mtls": [] }]"#,
        )
        .replace(
            r#""security": [{ "bearer": [] }]"#,
            r#""security": [{ "mtls": [] }]"#,
        );
    create_mock(
        &app,
        "tok-mtls",
        &mtls_only,
        json!({ "enabled": true, "mode": "spec" }),
    )
    .await;
    let a = close(&app, "mock:tok-mtls", json!({})).await;
    assert_eq!(a.status, StatusCode::CONFLICT, "{}", a.text);
    assert!(a.text.contains("demo client certificate"), "{}", a.text);
    // secure: true closes it behind an API key of its own: the token fails.
    let a = close(&app, "mock:tok-jwt", json!({ "secure": true })).await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    assert_eq!(a.body["secured_now"], true);
    let token = idp_token(&app).await;
    let bearer = format!("Bearer {token}");
    let a = call(
        &app,
        "GET",
        "/mock/tok-jwt/p",
        &[("authorization", &bearer)],
        None,
    )
    .await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED, "{}", a.text);
    // The locked entry hands no token URL.
    let a = call(&app, "GET", CATALOG_PATH, &[], None).await;
    assert!(
        item(&a.body, "mock:tok-jwt")["detail"]["token_url"].is_null(),
        "{}",
        a.text
    );
}

/// Finding 2: the default credentials close nothing, and `secure: true`
/// replaces them with random ones.
#[tokio::test]
async fn default_credentials_never_close_a_mock_and_secure_replaces_them() {
    let (app, _) = setup(password_config());
    create_mock(
        &app,
        "dkey",
        PET_SPEC_OPEN,
        json!({ "enabled": true, "mode": "api_key" }),
    )
    .await;
    let a = close(&app, "mock:dkey", json!({})).await;
    assert_eq!(a.status, StatusCode::CONFLICT, "{}", a.text);
    assert_eq!(a.body["reason"], "default_credentials");
    assert!(a.text.contains("demo-key"), "{}", a.text);
    create_mock(
        &app,
        "dbasic",
        PET_SPEC_OPEN,
        json!({ "enabled": true, "mode": "basic" }),
    )
    .await;
    let a = close(&app, "mock:dbasic", json!({})).await;
    assert_eq!(a.status, StatusCode::CONFLICT, "{}", a.text);
    assert_eq!(a.body["reason"], "default_credentials");
    // Every operation public: nothing to protect.
    create_mock(&app, "allpub", PET_SPEC_OPEN, json!({ "enabled": true, "mode": "api_key", "api_key": { "header": "x-api-key", "keys": ["own-key-9q"] } })).await;
    let a = as_admin(
        &app,
        "PUT",
        "/_rustybin/mocks/allpub/operations/p",
        Some(json!({ "public": true })),
    )
    .await;
    assert!(a.status.is_success(), "{}", a.text);
    let a = as_admin(&app, "POST", "/_rustybin/mocks/allpub/deploy", None).await;
    assert!(a.status.is_success(), "{}", a.text);
    let a = close(&app, "mock:allpub", json!({})).await;
    assert_eq!(a.status, StatusCode::CONFLICT, "{}", a.text);
    assert_eq!(a.body["reason"], "open_operations");
    // Basic with a user of its own closes.
    create_mock(&app, "obasic", PET_SPEC_OPEN, json!({ "enabled": true, "mode": "basic", "basic": { "users": { "reader": "own-pass-7k2" } } })).await;
    let a = close(&app, "mock:obasic", json!({})).await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    // The catalogue's own example: the defaults go, random ones come.
    let a = close(&app, "mock:dkey", json!({ "secure": true })).await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    assert_eq!(a.body["secured_now"], true);
    let a = call(
        &app,
        "GET",
        "/mock/dkey/p",
        &[("x-api-key", "demo-key")],
        None,
    )
    .await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED, "{}", a.text);
    let a = close(&app, "mock:dbasic", json!({ "secure": true })).await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    let a = call(
        &app,
        "GET",
        "/mock/dbasic/p",
        &[(
            "authorization",
            &format!("Basic {}", basic_of("demo", "demo")),
        )],
        None,
    )
    .await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED, "{}", a.text);
    let d = as_admin(&app, "GET", "/_rustybin/mocks/dbasic", None).await;
    assert_eq!(
        d.body["settings"]["security"]["mode"], "basic",
        "{}",
        d.text
    );
    // A reader gets the new credentials on the portal.
    let (k, v) = admin();
    let reader = call(&app, "GET", CATALOG_PATH, &[(k, &v)], None).await;
    let d = as_admin(&app, "GET", "/_rustybin/mocks/dkey", None).await;
    let key = d.body["settings"]["security"]["api_key"]["keys"][0]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(key.starts_with("key-") && key.len() > 30, "{key}");
    assert!(reader.text.contains(&key));
}

/// Finding 3: a document pulled from git never opens the portal nor moves
/// where Try it sends a tester's key.
#[tokio::test]
async fn a_pulled_document_never_changes_portal_auth_or_base_url() {
    use crate::configdoc::{self, ApplyOptions};
    let (app, state) = setup(password_config());
    let a = as_admin(
        &app,
        "PATCH",
        "/_rustybin/settings",
        Some(json!({ "values": { "portal.auth": "password" } })),
    )
    .await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    let opts = |untrusted: bool| ApplyOptions {
        prune: false,
        dry_run: false,
        principal: "test".into(),
        base: None,
        untrusted,
        caller: None,
        base_url: None,
    };
    let doc =
        "version: 2\nsettings:\n  portal.auth: open\n  portal.base_url: https://evil.example\n";
    let out = configdoc::apply(
        &state,
        crate::mocks::config_file::parse(doc).expect("parse"),
        &opts(true),
    );
    assert_eq!(
        state.settings.string("portal.auth").as_deref(),
        Some("password"),
        "{out}"
    );
    assert_eq!(state.settings.string("portal.base_url"), None, "{out}");
    let text = out.to_string();
    assert!(
        text.contains("settings.portal.auth: a document from the repository cannot change it"),
        "{text}"
    );
    assert!(
        text.contains("settings.portal.base_url: a document from the repository cannot change it"),
        "{text}"
    );
    // A trusted document (the operator's) still sets them.
    configdoc::apply(
        &state,
        crate::mocks::config_file::parse(doc).expect("parse"),
        &opts(false),
    );
    assert_eq!(
        state.settings.string("portal.auth").as_deref(),
        Some("open")
    );
}

/// Finding 4: under a closed item the mock's security cannot be switched
/// off (nor its credentials put back to the defaults) by any path, and a
/// mock recreated without it refuses every call until it is secured.
#[tokio::test]
async fn security_cannot_be_switched_off_under_a_closed_item() {
    use crate::configdoc::{self, ApplyOptions};
    let (app, state) = setup(password_config());
    plant_mock(&app).await;
    let a = close(&app, "mock:planted", json!({})).await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    let refused =
        |a: &Answer| a.status == StatusCode::UNAUTHORIZED || a.status == StatusCode::FORBIDDEN;
    // PATCH: off, or the default key.
    for security in [
        json!({ "enabled": false }),
        json!({ "enabled": true, "mode": "api_key", "api_key": { "header": "x-api-key", "keys": ["demo-key"] } }),
    ] {
        let a = as_admin(
            &app,
            "PATCH",
            "/_rustybin/mocks/planted",
            Some(json!({ "security": security, "deploy": true })),
        )
        .await;
        assert_eq!(a.status, StatusCode::CONFLICT, "{}", a.text);
    }
    // A draft may say so, its deploy may not.
    let a = as_admin(
        &app,
        "PATCH",
        "/_rustybin/mocks/planted",
        Some(json!({ "security": { "enabled": false } })),
    )
    .await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    let a = as_admin(&app, "POST", "/_rustybin/mocks/planted/deploy", None).await;
    assert_eq!(a.status, StatusCode::CONFLICT, "{}", a.text);
    let a = call(&app, "GET", "/mock/planted/h", &[], None).await;
    assert!(refused(&a), "{}", a.text);
    // Documents, untrusted and trusted.
    let opts = |untrusted: bool| ApplyOptions {
        prune: false,
        dry_run: false,
        principal: "test".into(),
        base: None,
        untrusted,
        caller: None,
        base_url: None,
    };
    let doc = format!(
        "version: 2\nmocks:\n  - name: planted\n    spec_inline: {}\n    security:\n      enabled: false\n",
        serde_json::to_string(PLANTED_SPEC).expect("spec")
    );
    // The preview says so as the apply does.
    let mut preview = opts(true);
    preview.dry_run = true;
    let out = configdoc::apply(
        &state,
        crate::mocks::config_file::parse(&doc).expect("parse"),
        &preview,
    );
    assert!(
        out.to_string().contains("closed on the dev portal"),
        "{out}"
    );
    for untrusted in [true, false] {
        let out = configdoc::apply(
            &state,
            crate::mocks::config_file::parse(&doc).expect("parse"),
            &opts(untrusted),
        );
        assert!(
            out.to_string().contains("closed on the dev portal"),
            "{out}"
        );
        let a = call(&app, "GET", "/mock/planted/h", &[], None).await;
        assert!(refused(&a), "untrusted {untrusted}: {}", a.text);
    }
    // A rollback to a revision without security.
    create_mock(&app, "rolled", PET_SPEC_OPEN, json!({ "enabled": false })).await;
    let a = as_admin(&app, "PATCH", "/_rustybin/mocks/rolled", Some(json!({ "security": { "enabled": true, "mode": "api_key", "api_key": { "header": "x-api-key", "keys": ["own-key-55x"] } }, "deploy": true }))).await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    let a = close(&app, "mock:rolled", json!({})).await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    let a = as_admin(&app, "POST", "/_rustybin/mocks/rolled/rollback", None).await;
    assert_eq!(a.status, StatusCode::CONFLICT, "{}", a.text);
    let a = call(&app, "GET", "/mock/rolled/p", &[], None).await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED, "{}", a.text);
    // Deleted and recreated without security: the item stays closed, the
    // mock refuses every call until it is secured.
    let a = as_admin(&app, "DELETE", "/_rustybin/mocks/rolled", None).await;
    assert!(a.status.is_success(), "{}", a.text);
    create_mock(&app, "rolled", PET_SPEC_OPEN, json!({ "enabled": false })).await;
    let a = call(&app, "GET", "/mock/rolled/p", &[], None).await;
    assert_eq!(a.status, StatusCode::FORBIDDEN, "{}", a.text);
    assert_eq!(a.body["reason"], "closed_unsecured", "{}", a.text);
    let a = as_admin(&app, "GET", CONTROL_PATH, None).await;
    let it = a.body["items"]
        .as_array()
        .and_then(|i| i.iter().find(|x| x["id"] == "mock:rolled"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(it["published"]["access"], "closed");
    assert!(
        it["warning"]
            .as_str()
            .is_some_and(|w| w.contains("refuses every call")),
        "{it}"
    );
    // Secured again (secure: true), it answers with its own key only.
    let a = close(&app, "mock:rolled", json!({ "secure": true })).await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    let a = call(&app, "GET", "/mock/rolled/p", &[], None).await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED, "{}", a.text);
}

/// Finding 4, the config file: a closed `portal.items` entry over a mock
/// without its own credentials is reported at load, and the mock refuses
/// every call.
#[tokio::test]
async fn a_config_file_closing_an_unsecured_mock_is_reported_and_refused() {
    let dir = tempfile::tempdir().expect("dir");
    let file = dir.path().join("rustybin.yaml");
    std::fs::write(
        &file,
        "version: 2\nportal:\n  items:\n    - id: mock:petstore\n      access: closed\n",
    )
    .expect("write");
    let mut c = password_config();
    c.config_file = Some(file.to_string_lossy().into_owned());
    let (app, state) = setup(c);
    let (_, problems) = crate::mocks::reload_files(&state);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("portal item mock:petstore: closed")),
        "{problems:?}"
    );
    assert!(state
        .mocks
        .load_errors()
        .iter()
        .any(|p| p.contains("mock:petstore")));
    let a = call(&app, "GET", "/mock/petstore/v1/pets", &[], None).await;
    assert_eq!(a.status, StatusCode::FORBIDDEN, "{}", a.text);
    assert_eq!(a.body["reason"], "closed_unsecured");
}

/// Finding 5: a closed MCP server keeps no hint at how to get through, and
/// the publish answer says the endpoint stays callable.
#[tokio::test]
async fn a_closed_mcp_server_has_no_hint_and_says_it_stays_callable() {
    let (app, _) = setup(password_config());
    for id in ["mcp:apikey", "mcp:protected"] {
        let a = close(&app, id, json!({})).await;
        assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
        assert!(
            a.body["note"]
                .as_str()
                .is_some_and(|n| n.contains("stays callable")),
            "{}",
            a.text
        );
    }
    let a = call(&app, "GET", CATALOG_PATH, &[], None).await;
    for id in ["mcp:apikey", "mcp:protected"] {
        let it = item(&a.body, id);
        assert_eq!(it["locked"], true);
        assert!(it["detail"]["auth"]["hint"].is_null(), "{it}");
        assert!(it["detail"]["auth"]["kind"].is_string(), "{it}");
    }
    assert!(!a.text.contains("any non-empty X-API-Key"), "{}", a.text);
}

/// Finding 6: `secure: true` changes nothing when the publish would be
/// refused (bad metadata, a full portal).
#[tokio::test]
async fn a_refused_secure_publish_leaves_the_mock_as_it_was() {
    let (app, state) = setup(password_config());
    let enabled = |app: Router| async move {
        as_admin(&app, "GET", "/_rustybin/mocks/petstore", None)
            .await
            .body["settings"]["security"]["enabled"]
            .clone()
    };
    let long = "x".repeat(MAX_SUMMARY + 1);
    let a = close(
        &app,
        "mock:petstore",
        json!({ "secure": true, "summary": long }),
    )
    .await;
    assert_eq!(a.status, StatusCode::BAD_REQUEST, "{}", a.text);
    assert_eq!(enabled(app.clone()).await, json!(false));
    for i in 0..MAX_ITEMS {
        state
            .portal
            .publish(
                &format!("mock:filler-{i}"),
                Meta::default(),
                "test".into(),
                None,
                chrono::Utc::now(),
            )
            .expect("publish");
    }
    let a = close(&app, "mock:petstore", json!({ "secure": true })).await;
    assert_eq!(a.status, StatusCode::CONFLICT, "{}", a.text);
    assert!(a.text.contains("at most"), "{}", a.text);
    assert_eq!(enabled(app.clone()).await, json!(false));
}

/// Finding 7: portal sign-outs have their own bounded list, so a loop of
/// portal sign-ins and sign-outs never evicts a console sign-out.
#[test]
fn portal_sign_outs_never_evict_console_sign_outs() {
    let tokens = crate::login::Tokens::new();
    let now = tokens.now();
    // The console sign-out expires first: the eviction's first victim.
    tokens.revoke("console-session", now + 60);
    for i in 0..crate::login::MAX_PORTAL_REVOKED + 10 {
        tokens.revoke_portal(&format!("portal-{i}"), now + 3600 + i as i64);
    }
    assert!(
        tokens.is_revoked("console-session"),
        "the console sign-out holds"
    );
    assert!(tokens.is_portal_revoked(&format!("portal-{}", crate::login::MAX_PORTAL_REVOKED + 9)));
    assert!(
        !tokens.is_revoked("portal-5"),
        "portal sign-outs are kept apart"
    );
}

/// An event API is closed only on an instance that asks every data-plane
/// call for an API key (a channel has no mocked security). Signed out, its
/// entry is the docs (channels, messages, examples, addresses) without a
/// connection snippet; a reader gets the snippets.
#[tokio::test]
async fn a_locked_event_api_keeps_its_docs_and_holds_no_snippet() {
    let mut c = password_config();
    c.require_key = true;
    let (app, _) = setup(c);
    let body = json!({ "name": "order-events", "spec": super::tests::event_doc() });
    let a = as_admin(&app, "POST", "/_rustybin/mocks", Some(body)).await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    let a = as_admin(
        &app,
        "PUT",
        "/_rustybin/portal/items/mock:order-events",
        Some(json!({ "access": "closed" })),
    )
    .await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);

    let signed_out = call(&app, "GET", CATALOG_PATH, &[], None).await;
    assert_eq!(signed_out.status, StatusCode::OK, "{}", signed_out.text);
    let it = item(&signed_out.body, "mock:order-events");
    assert_eq!(it["kind"], "event", "{it}");
    assert_eq!(it["access"], "closed", "{it}");
    assert_eq!(it["locked"], true, "{it}");
    let text = it.to_string();
    for needle in ["websocat", "curl", "/collections/", "\"connect\""] {
        assert!(
            !text.contains(needle),
            "the locked entry carries {needle}: {text}"
        );
    }
    let channels = it["detail"]["channels"].as_array().expect("channels");
    assert_eq!(channels.len(), 2, "{it}");
    assert!(channels[0]["url"].is_string() && channels[0]["send"].is_array());

    let (k, v) = admin();
    let reader = call(&app, "GET", CATALOG_PATH, &[(k, &v)], None).await;
    let it = item(&reader.body, "mock:order-events");
    assert_eq!(it["locked"], false, "{it}");
    assert!(
        it["detail"]["channels"][0]["connect"]["websocket"].is_string(),
        "{it}"
    );
}
