use super::*;
use crate::limits::ManualClock;
use crate::test_support::{
    bearer_request, body_json, control_jwt, control_keys, json_request, test_app_with,
    test_state_with,
};
use axum::body::Body;
use axum::http::Request;
use axum::Router;
use tower::ServiceExt;

fn password_config() -> Config {
    let mut c = Config::for_tests();
    c.control_auth = ControlAuth::Password;
    c
}

fn app(config: Config) -> Router {
    test_app_with(test_state_with(config))
}

async fn login(app: &Router, user: &str, password: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            LOGIN_PATH,
            &json!({ "username": user, "password": password }),
        ))
        .await
        .expect("response");
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn token_for(app: &Router) -> String {
    let (status, body) = login(app, DEFAULT_USER, DEFAULT_PASSWORD).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["token"].as_str().expect("token").to_string()
}

async fn status_of(app: &Router, method: &str, uri: &str, token: Option<&str>) -> StatusCode {
    app.clone()
        .oneshot(bearer_request(method, uri, token))
        .await
        .expect("response")
        .status()
}

async fn with_token(
    app: &Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    (status, body_json(resp).await)
}

#[test]
fn password_rules_and_hashes() {
    assert!(valid_new_password("short").is_err());
    assert!(valid_new_password(DEFAULT_PASSWORD).is_err());
    assert!(valid_new_password("with\u{7}bell").is_err());
    assert!(valid_new_password(&"x".repeat(257)).is_err());
    assert!(valid_new_password("a-longer-secret").is_ok());
    assert!(valid_user("rustybin").is_ok());
    assert!(valid_user("ops.team@acme-1").is_ok());
    assert!(valid_user("").is_err());
    assert!(valid_user("no spaces").is_err());
    assert!(valid_user(&"u".repeat(65)).is_err());

    let hash = hash_password("a-longer-secret").expect("hash");
    assert!(hash.starts_with("$argon2id$"));
    assert!(valid_hash(&hash));
    assert!(verify_hash(&hash, "a-longer-secret"));
    assert!(!verify_hash(&hash, "another-secret"));
    assert!(!valid_hash("not a hash"));
    assert!(!valid_hash("$2b$12$abcdefghijklmnopqrstuv"));
    assert!(!verify_hash("not a hash", "whatever"));
}

#[test]
fn mode_is_the_default_and_parses() {
    assert_eq!(ControlAuth::parse("password"), Some(ControlAuth::Password));
    assert_eq!(ControlAuth::Password.as_str(), "password");
    let (c, _) = Config::from_lookup(|_| None);
    assert_eq!(c.control_auth, ControlAuth::Password);
    assert!(enabled(&c));
    // Hosted mode keeps jwt, shared mode keeps open (keys authenticate there).
    let (c, _) = Config::from_lookup(|k| (k == "RUSTYBIN_HOSTED_MODE").then(|| "true".into()));
    assert_eq!(c.control_auth, ControlAuth::Jwt);
    let (c, _) = Config::from_lookup(|k| (k == "RUSTYBIN_SHARED_MODE").then(|| "true".into()));
    assert_eq!(c.control_auth, ControlAuth::Open);
    // The explicit value wins, and the sign-in is off in shared mode.
    let (c, _) = Config::from_lookup(|k| match k {
        "RUSTYBIN_SHARED_MODE" => Some("true".into()),
        "RUSTYBIN_CONTROL_AUTH" => Some("password".into()),
        _ => None,
    });
    assert_eq!(c.control_auth, ControlAuth::Password);
    assert!(!enabled(&c));
    // The variables: an invalid user name and hash warn and are ignored.
    let (c, warnings) = Config::from_lookup(|k| match k {
        "RUSTYBIN_CONSOLE_USER" => Some("no spaces".into()),
        "RUSTYBIN_CONSOLE_PASSWORD_HASH" => Some("garbage".into()),
        "RUSTYBIN_CONSOLE_PASSWORD" => Some("plain-secret".into()),
        _ => None,
    });
    assert!(c.console_user.is_none());
    assert!(c.console_password_hash.is_none());
    assert_eq!(c.console_password.as_deref(), Some("plain-secret"));
    assert!(warnings.iter().any(|w| w.contains("RUSTYBIN_CONSOLE_USER")));
    assert!(warnings
        .iter()
        .any(|w| w.contains("RUSTYBIN_CONSOLE_PASSWORD_HASH")));
}

#[tokio::test]
async fn password_mode_locks_the_control_plane_and_signs_in_with_the_defaults() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    // Locked without a token; readiness, the data plane and the console's
    // files stay open; the sign-in route is never behind the lock.
    let resp = app
        .clone()
        .oneshot(bearer_request("GET", "/_rustybin/config", None))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let v = body_json(resp).await;
    assert_eq!(v["reason"], "missing_token");
    assert_eq!(v["control_auth"], "password");
    assert!(v["error"]
        .as_str()
        .unwrap_or("")
        .contains("/_rustybin/login"));
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/ready", None).await,
        StatusCode::OK
    );
    assert_eq!(status_of(&app, "GET", "/echo", None).await, StatusCode::OK);
    assert_eq!(status_of(&app, "GET", "/ui/", None).await, StatusCode::OK);
    assert!(is_public_path(LOGIN_PATH));
    assert!(!is_public_path(LOGOUT_PATH));
    assert!(default_password(&state));

    // Wrong credentials.
    let (status, v) = login(&app, DEFAULT_USER, "wrong").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(v["reason"], "invalid_credentials");
    let (status, _) = login(&app, "someone", DEFAULT_PASSWORD).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The defaults sign in and the answer says so.
    let (status, v) = login(&app, DEFAULT_USER, DEFAULT_PASSWORD).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["token_type"], "Bearer");
    assert_eq!(v["user"], "rustybin");
    assert_eq!(v["default_password"], true);
    assert_eq!(
        v["scopes"],
        json!(["admin", "console", "inspector", "harness"])
    );
    assert_eq!(v["expires_in"], TOKEN_TTL_SECS);
    let token = v["token"].as_str().expect("token").to_string();
    assert!(is_console_token(&token));

    // The token opens the whole control plane, including mutations, the
    // management MCP server and the data plane's admin guard.
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some(&token)).await,
        StatusCode::OK
    );
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/requests", Some(&token)).await,
        StatusCode::OK
    );
    assert_eq!(
        status_of(&app, "DELETE", "/_rustybin/requests", Some(&token)).await,
        StatusCode::OK
    );
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/settings", Some(&token)).await,
        StatusCode::OK
    );
    assert!(status_of(&app, "POST", "/health/unhealthy", Some(&token))
        .await
        .is_success());
    assert!(!state.health.is_healthy());
    let req = Request::builder()
        .method("POST")
        .uri("/_rustybin/mcp")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", "tools/list")
        .body(Body::from(
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}).to_string(),
        ))
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let v = body_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let tools: Vec<&str> = v["result"]["tools"]
        .as_array()
        .map(|t| t.iter().filter_map(|x| x["name"].as_str()).collect())
        .unwrap_or_default();
    assert!(tools.contains(&"set_health"), "admin tools: {tools:?}");

    // The status flags the default password; the session describes the token.
    let (status, v) = with_token(&app, "GET", "/_rustybin/status", &token, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["default_password"], true);
    assert_eq!(v["signin"]["user"], "rustybin");
    let (status, v) = with_token(&app, "GET", SESSION_PATH, &token, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["obtained"], "password");
    assert_eq!(v["user"], "rustybin");
    assert!(v["scopes"]
        .as_array()
        .is_some_and(|s| s.contains(&json!("admin"))));
    // Captures redact it like any control credential.
    assert!(crate::keys::authorization_is_control(
        &{
            let mut h = HeaderMap::new();
            h.insert(
                "authorization",
                HeaderValue::from_str(&format!("Bearer {token}")).expect("v"),
            );
            h
        },
        &state.config
    ));
}

#[tokio::test]
async fn failed_attempts_are_rate_limited_per_address() {
    let clock = Arc::new(ManualClock::new(chrono::Utc::now()));
    let mut c = password_config();
    c.console_tokens = Arc::new(Tokens::with_clock(clock.clone()));
    let app = app(c);
    for _ in 0..FAILURES_PER_WINDOW {
        let (status, _) = login(&app, DEFAULT_USER, "wrong").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    // The sixth attempt waits, right credentials or not.
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            LOGIN_PATH,
            &json!({ "username": DEFAULT_USER, "password": DEFAULT_PASSWORD }),
        ))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry: u64 = resp
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .expect("retry-after");
    assert!((1..=FIRST_BLOCK_SECS).contains(&retry), "{retry}");
    assert_eq!(body_json(resp).await["reason"], "rate_limited");
    // After the wait the address may try again, and a success clears it.
    clock.advance(Duration::from_secs(FIRST_BLOCK_SECS + 1));
    let (status, _) = login(&app, DEFAULT_USER, DEFAULT_PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
    // A second round of failures waits twice as long, up to the cap.
    for _ in 0..FAILURES_PER_WINDOW {
        login(&app, DEFAULT_USER, "wrong").await;
    }
    clock.advance(Duration::from_secs(FIRST_BLOCK_SECS + 1));
    let (status, _) = login(&app, DEFAULT_USER, DEFAULT_PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
    for _ in 0..FAILURES_PER_WINDOW {
        login(&app, DEFAULT_USER, "wrong").await;
    }
    for _ in 0..FAILURES_PER_WINDOW {
        login(&app, DEFAULT_USER, "wrong").await;
    }
    let tokens = Tokens::with_clock(clock.clone());
    let ip: IpAddr = "10.0.0.9".parse().expect("ip");
    for round in 0..6u32 {
        for _ in 0..FAILURES_PER_WINDOW {
            tokens.failed(Some(ip), "u");
        }
        let wait = tokens.check(Some(ip), "u").expect_err("blocked");
        let expected = (FIRST_BLOCK_SECS << round).min(MAX_BLOCK_SECS);
        assert_eq!(wait, expected, "round {round}");
        clock.advance(Duration::from_secs(expected));
        assert!(tokens.check(Some(ip), "u").is_ok());
    }
    // Attempts without a client address are never blocked, and the table is bounded.
    assert!(tokens.check(None, "u").is_ok());
    for i in 0..(MAX_ATTEMPT_ADDRESSES as u32 + 50) {
        let ip = IpAddr::from([10, 1, (i >> 8) as u8, (i & 255) as u8]);
        tokens.failed(Some(ip), "u");
    }
    assert!(lock(&tokens.attempts).len() <= MAX_ATTEMPT_ADDRESSES);
}

#[tokio::test]
async fn logout_revokes_and_sign_out_everywhere_rotates() {
    // An admin token makes the data plane's admin guard strict, so a revoked
    // sign-in token is refused there too.
    let mut c = password_config();
    c.admin_token = Some("admin-secret".into());
    let app = app(c);
    let a = token_for(&app).await;
    let b = token_for(&app).await;
    assert_ne!(a, b);
    let (status, v) = with_token(&app, "POST", LOGOUT_PATH, &a, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["revoked"], true);
    let resp = app
        .clone()
        .oneshot(bearer_request("GET", "/_rustybin/config", Some(&a)))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(resp).await["reason"], "revoked");
    // The data plane's admin guard refuses it too.
    assert_eq!(
        status_of(&app, "POST", "/health/unhealthy", Some(&a)).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some(&b)).await,
        StatusCode::OK
    );

    // Sessions list the live token only.
    let (status, v) = with_token(&app, "GET", SESSIONS_PATH, &b, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["count"], 1);
    assert_eq!(v["sessions"][0]["id"], v["current"]);
    assert_eq!(v["sessions"][0]["user"], "rustybin");
    assert_eq!(v["sessions"][0]["ip"], "127.0.0.1");
    // End one by id.
    let c = token_for(&app).await;
    let (_, v) = with_token(&app, "GET", SESSIONS_PATH, &b, json!({})).await;
    assert_eq!(v["count"], 2);
    let other = v["sessions"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["id"] != v["current"]))
        .map(|s| s["id"].as_str().unwrap_or("").to_string())
        .expect("other session");
    let (status, v) = with_token(
        &app,
        "DELETE",
        &format!("/_rustybin/sessions/{other}"),
        &b,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["ended"], true);
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some(&c)).await,
        StatusCode::UNAUTHORIZED
    );
    let (_, v) = with_token(
        &app,
        "DELETE",
        &format!("/_rustybin/sessions/{other}"),
        &b,
        json!({}),
    )
    .await;
    assert_eq!(v["ended"], false);

    // Sign out everywhere: the caller included.
    let d = token_for(&app).await;
    let (_, before) = with_token(&app, "GET", SESSION_PATH, &b, json!({})).await;
    let (status, v) = with_token(&app, "DELETE", SESSIONS_PATH, &b, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["ended"], 2);
    assert!(
        v["generation"].as_u64() > before["generation"].as_u64(),
        "{v} {before}"
    );
    for t in [&b, &d] {
        let resp = app
            .clone()
            .oneshot(bearer_request("GET", "/_rustybin/config", Some(t)))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(resp).await["reason"], "revoked");
    }
    // Signing in again works, and the new token is of the new generation.
    let e = token_for(&app).await;
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some(&e)).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn the_password_changes_and_old_tokens_end() {
    let state = test_state_with(password_config());
    let app = test_app_with(state.clone());
    let a = token_for(&app).await;
    let b = token_for(&app).await;
    // The current password is required; the rules hold.
    let (status, v) = with_token(
        &app,
        "PUT",
        PASSWORD_PATH,
        &a,
        json!({"current_password": "nope", "new_password": "a-longer-secret"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["reason"], "invalid_credentials");
    let (status, v) = with_token(
        &app,
        "PUT",
        PASSWORD_PATH,
        &a,
        json!({"current_password": DEFAULT_PASSWORD, "new_password": "short"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let (status, _) = with_token(
        &app,
        "PUT",
        PASSWORD_PATH,
        &a,
        json!({"current_password": DEFAULT_PASSWORD, "new_password": DEFAULT_PASSWORD}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, v) = with_token(&app, "PUT", PASSWORD_PATH, &a, json!({"current_password": DEFAULT_PASSWORD, "new_password": "a-longer-secret", "username": "bad name"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(v["reason"], "invalid_user");
    // A read-only control JWT may not change it (admin needed).
    let mut c = password_config();
    c.control_jwt_key = crate::control_auth::parse_public_key(control_keys::CONTROL_JWT_PUBLIC_PEM);
    let jwt_app = app_for(c);
    let (status, _) = with_token(
        &jwt_app,
        "PUT",
        PASSWORD_PATH,
        &control_jwt(Some("console")),
        json!({"current_password": DEFAULT_PASSWORD, "new_password": "a-longer-secret"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The change: a new user name and password, every other session ends,
    // the caller gets a fresh token.
    let (status, v) = with_token(&app, "PUT", PASSWORD_PATH, &a, json!({"current_password": DEFAULT_PASSWORD, "new_password": "a-longer-secret", "username": "ops"})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["changed"], true);
    assert_eq!(v["user"], "ops");
    assert_eq!(v["default_password"], false);
    assert_eq!(v["signed_out"], 2);
    let fresh = v["token"].as_str().expect("fresh token").to_string();
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some(&fresh)).await,
        StatusCode::OK
    );
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some(&a)).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some(&b)).await,
        StatusCode::UNAUTHORIZED
    );
    assert!(!default_password(&state));
    assert_eq!(effective_user(&state.settings), "ops");
    // The hash is a secret in the settings store: set, hinted, never returned.
    let (_, v) = with_token(&app, "GET", "/_rustybin/settings", &fresh, json!({})).await;
    let hash = v["settings"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["key"] == HASH_KEY))
        .cloned()
        .expect("spec");
    assert_eq!(hash["value"]["set"], true);
    assert_eq!(hash["value"]["source"], "stored");
    assert!(!v.to_string().contains("a-longer-secret"));
    assert!(!v.to_string().contains("$argon2"));
    // Old credentials fail, new ones sign in.
    assert_eq!(
        login(&app, DEFAULT_USER, DEFAULT_PASSWORD).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, "ops", DEFAULT_PASSWORD).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, v) = login(&app, "ops", "a-longer-secret").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["default_password"], false);
    assert_eq!(v["user"], "ops");
}

fn app_for(config: Config) -> Router {
    test_app_with(test_state_with(config))
}

#[tokio::test]
async fn environment_credentials_and_precedence() {
    // A plain password from the environment.
    let mut c = password_config();
    c.console_user = Some("admin".into());
    c.console_password = Some("env-secret".into());
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    assert!(!default_password(&state));
    assert_eq!(
        login(&app, DEFAULT_USER, DEFAULT_PASSWORD).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, "admin", DEFAULT_PASSWORD).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(login(&app, "admin", "env-secret").await.0, StatusCode::OK);
    // The plain password set to the default is still the default.
    let mut c = password_config();
    c.console_password = Some(DEFAULT_PASSWORD.into());
    assert!(default_password(&test_state_with(c)));
    // A hash from the environment wins over the plain password.
    let mut c = password_config();
    c.console_password = Some("env-secret".into());
    c.console_password_hash = Some(hash_password("hashed-secret").expect("hash"));
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    assert!(!default_password(&state));
    assert_eq!(
        login(&app, DEFAULT_USER, "env-secret").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, DEFAULT_USER, "hashed-secret").await.0,
        StatusCode::OK
    );
    // A stored hash wins over both; the environment hash is the seed.
    let (_, v) = with_token(
        &app,
        "GET",
        "/_rustybin/settings",
        &token_for_user(&app, DEFAULT_USER, "hashed-secret").await,
        json!({}),
    )
    .await;
    let spec = v["settings"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["key"] == HASH_KEY))
        .cloned()
        .expect("spec");
    assert_eq!(spec["value"]["source"], "env");
    let token = token_for_user(&app, DEFAULT_USER, "hashed-secret").await;
    let (status, v) = with_token(
        &app,
        "PUT",
        PASSWORD_PATH,
        &token,
        json!({"current_password": "hashed-secret", "new_password": "stored-secret"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        login(&app, DEFAULT_USER, "hashed-secret").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, DEFAULT_USER, "stored-secret").await.0,
        StatusCode::OK
    );
    // With the settings lock the environment is authoritative: no change.
    let mut c = password_config();
    c.settings_lock = true;
    let app = app_for(c);
    let token = token_for(&app).await;
    let (status, v) = with_token(
        &app,
        "PUT",
        PASSWORD_PATH,
        &token,
        json!({"current_password": DEFAULT_PASSWORD, "new_password": "a-longer-secret"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["reason"], "locked");
    assert_eq!(
        login(&app, DEFAULT_USER, DEFAULT_PASSWORD).await.0,
        StatusCode::OK
    );
}

async fn token_for_user(app: &Router, user: &str, password: &str) -> String {
    let (status, body) = login(app, user, password).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["token"].as_str().expect("token").to_string()
}

#[tokio::test]
async fn settings_patch_validates_the_hash() {
    let app = app(password_config());
    let token = token_for(&app).await;
    let (status, v) = with_token(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &token,
        json!({"secrets": {HASH_KEY: "garbage"}, "current_password": DEFAULT_PASSWORD}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let (status, v) = with_token(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &token,
        json!({"values": {USER_KEY: "no spaces"}, "current_password": DEFAULT_PASSWORD}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
}

#[tokio::test]
async fn the_sign_in_changes_through_the_settings_only_with_the_current_password() {
    let app = app(password_config());
    let token = token_for(&app).await;
    let hash = hash_password("patched-secret").expect("hash");
    // Without the current password (an admin-scoped token is not enough),
    // and with a wrong one: refused, nothing changes.
    let (status, v) = with_token(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &token,
        json!({"values": {USER_KEY: "patched"}, "secrets": {HASH_KEY: hash}}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["reason"], "current_password_required");
    let (status, v) = with_token(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &token,
        json!({"secrets": {HASH_KEY: hash}, "current_password": "wrong"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["reason"], "invalid_credentials");
    let (status, _) = with_token(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &token,
        json!({"values": {USER_KEY: "patched"}}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        login(&app, DEFAULT_USER, DEFAULT_PASSWORD).await.0,
        StatusCode::OK
    );
    // Other settings need nothing more.
    let (status, v) = with_token(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &token,
        json!({"values": {"console.title": "Ops"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    // With it: changed, and every session is signed out.
    let (status, v) = with_token(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &token,
        json!({"values": {USER_KEY: "patched"}, "secrets": {HASH_KEY: hash}, "current_password": DEFAULT_PASSWORD}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some(&token)).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, "patched", "patched-secret").await.0,
        StatusCode::OK
    );
    assert_eq!(
        login(&app, DEFAULT_USER, DEFAULT_PASSWORD).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Unsetting the hash: not with a sign-in token alone; PATCH with the
    // current password clears it, and signs everyone out again.
    let fresh = token_for_user(&app, "patched", "patched-secret").await;
    let (status, v) = with_token(
        &app,
        "DELETE",
        &format!("/_rustybin/settings/{HASH_KEY}"),
        &fresh,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["problems"][0]
        .as_str()
        .unwrap_or("")
        .contains("current password"));
    let (status, v) = with_token(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &fresh,
        json!({"secrets": {HASH_KEY: null}, "current_password": "patched-secret"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some(&fresh)).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, "patched", DEFAULT_PASSWORD).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn the_admin_token_resets_a_forgotten_password() {
    let mut c = password_config();
    c.admin_token = Some("admin-secret".into());
    let state = test_state_with(c);
    let hash = hash_password("forgotten-secret").expect("hash");
    let mut secrets = BTreeMap::new();
    secrets.insert(HASH_KEY.to_string(), Some(hash));
    state
        .settings
        .update_sign_in(BTreeMap::new(), secrets, "test")
        .expect("set");
    let app = test_app_with(state.clone());
    let token = token_for_user(&app, DEFAULT_USER, "forgotten-secret").await;
    let (status, v) = with_token(
        &app,
        "DELETE",
        &format!("/_rustybin/settings/{HASH_KEY}"),
        "admin-secret",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some(&token)).await,
        StatusCode::UNAUTHORIZED,
        "the reset signs every session out"
    );
    assert_eq!(
        login(&app, DEFAULT_USER, DEFAULT_PASSWORD).await.0,
        StatusCode::OK
    );
}

#[test]
fn a_plain_settings_update_never_changes_the_sign_in_in_password_mode() {
    // The rule sits in the store, so the management MCP tools and
    // documents meet it too.
    let state = test_state_with(password_config());
    let generation = state.config.console_tokens.generation();
    let first_generation = state.users.first_admin().expect("first admin").generation;
    let mut values = BTreeMap::new();
    values.insert(USER_KEY.to_string(), json!("someone"));
    assert!(state
        .settings
        .update(values.clone(), BTreeMap::new(), "test")
        .is_err());
    let mut same = BTreeMap::new();
    same.insert(USER_KEY.to_string(), json!(DEFAULT_USER));
    assert!(
        state.settings.update(same, BTreeMap::new(), "test").is_ok(),
        "the value in force is no change"
    );
    let mut secrets = BTreeMap::new();
    secrets.insert(
        HASH_KEY.to_string(),
        Some(hash_password("another-secret").expect("hash")),
    );
    assert!(state
        .settings
        .update(BTreeMap::new(), secrets.clone(), "test")
        .is_err());
    assert_eq!(state.config.console_tokens.generation(), generation);
    // The sign-in path changes it and rotates the generation.
    state
        .settings
        .update_sign_in(values, secrets, "test")
        .expect("sign-in update");
    // The first admin's generation, not the instance's (step 12): the
    // other users stay signed in.
    assert_eq!(state.config.console_tokens.generation(), generation);
    assert!(state.users.first_admin().expect("first admin").generation > first_generation);
    assert!(state.settings.unset(HASH_KEY, "test").is_err());
    assert!(state.settings.unset(USER_KEY, "test").is_err());
    // Outside password mode nothing is guarded.
    let state = test_state_with(Config::for_tests());
    let mut values = BTreeMap::new();
    values.insert(USER_KEY.to_string(), json!("someone"));
    assert!(state
        .settings
        .update(values, BTreeMap::new(), "test")
        .is_ok());
}

#[tokio::test]
async fn admin_token_and_control_jwts_still_work_in_password_mode() {
    let mut c = password_config();
    c.admin_token = Some("admin-secret".into());
    c.control_jwt_key = crate::control_auth::parse_public_key(control_keys::CONTROL_JWT_PUBLIC_PEM);
    let app = app(c);
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some("admin-secret")).await,
        StatusCode::OK
    );
    assert_eq!(
        status_of(
            &app,
            "GET",
            "/_rustybin/config",
            Some(&control_jwt(Some("console")))
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        status_of(
            &app,
            "DELETE",
            "/_rustybin/requests",
            Some(&control_jwt(Some("console")))
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some("wrong")).await,
        StatusCode::UNAUTHORIZED
    );
    // The session route tells them apart.
    let (_, v) = with_token(&app, "GET", SESSION_PATH, "admin-secret", json!({})).await;
    assert_eq!(v["obtained"], "admin_token");
    let (_, v) = with_token(
        &app,
        "GET",
        SESSION_PATH,
        &control_jwt(Some("console")),
        json!({}),
    )
    .await;
    assert_eq!(v["obtained"], "control_jwt");
    assert_eq!(v["sub"], "user-1");
    // Logout of a non sign-in token revokes nothing.
    let (status, v) = with_token(&app, "POST", LOGOUT_PATH, "admin-secret", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["revoked"], false);
    // A sign-in token signed with another key, or an OAuth-shaped token, is refused.
    let mut other = password_config();
    other.control_oauth_key = crate::control_oauth::OAuthKey::from_settings(
        Some("another-key-of-thirty-two-characters!"),
        None,
    );
    let foreign = token_for(&app_for(other)).await;
    let resp = app
        .clone()
        .oneshot(bearer_request("GET", "/_rustybin/config", Some(&foreign)))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(resp).await["reason"], "invalid_signature");
}

#[tokio::test]
async fn other_modes_have_no_sign_in() {
    for mode in [ControlAuth::Open, ControlAuth::Token, ControlAuth::Jwt] {
        let mut c = Config::for_tests();
        c.control_auth = mode;
        c.admin_token = Some("admin-secret".into());
        let app = app(c);
        let (status, v) = login(&app, DEFAULT_USER, DEFAULT_PASSWORD).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{mode:?}");
        assert_eq!(v["control_auth"], mode.as_str());
        let expect = if mode == ControlAuth::Open {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::OK
        };
        assert_eq!(
            status_of(&app, "GET", SESSION_PATH, Some("admin-secret")).await,
            expect,
            "{mode:?}"
        );
        assert_eq!(
            status_of(&app, "GET", SESSIONS_PATH, Some("admin-secret")).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status_of(&app, "PUT", PASSWORD_PATH, Some("admin-secret")).await,
            StatusCode::NOT_FOUND
        );
    }
    // Shared mode: keys authenticate, the sign-in is off.
    let mut c = password_config();
    c.shared_mode = true;
    assert!(!enabled(&c));
}

#[tokio::test]
async fn public_mode_refuses_the_default_password() {
    let mut c = password_config();
    c.public_mode = true;
    let state = test_state_with(c);
    let err = check_startup(&state).expect_err("refused");
    assert!(err.contains("RUSTYBIN_CONSOLE_PASSWORD"), "{err}");
    let mut c = password_config();
    c.public_mode = true;
    c.console_password = Some("a-real-secret".into());
    assert!(check_startup(&test_state_with(c)).is_ok());
    let mut c = Config::for_tests();
    c.public_mode = true;
    assert!(check_startup(&test_state_with(c)).is_ok());
    // A stored hash counts too.
    let mut c = password_config();
    c.public_mode = true;
    let state = test_state_with(c);
    state
        .settings
        .update_sign_in(
            BTreeMap::new(),
            BTreeMap::from([(
                HASH_KEY.to_string(),
                Some(hash_password("stored-secret").expect("hash")),
            )]),
            "test",
        )
        .expect("stored");
    assert!(check_startup(&state).is_ok());
}

#[test]
fn tokens_are_bounded() {
    let tokens = Tokens::new();
    let far = chrono::Utc::now().timestamp() + 3600;
    for i in 0..(MAX_REVOKED + 20) {
        tokens.revoke(&format!("jti-{i}"), far + i as i64);
    }
    assert_eq!(lock(&tokens.revoked).len(), MAX_REVOKED);
    // The ones that expire first were dropped.
    assert!(!tokens.is_revoked("jti-0"));
    assert!(tokens.is_revoked(&format!("jti-{}", MAX_REVOKED + 19)));
    // Expired entries are not revoked (and are pruned).
    tokens.revoke("old", chrono::Utc::now().timestamp() - 1);
    assert!(!tokens.is_revoked("old"));
    // Sessions: a bounded list, newest first; ending one revokes it.
    let tokens = Tokens::new();
    for i in 0..(MAX_SESSIONS + 10) {
        tokens.record(Issued {
            jti: format!("s-{i}"),
            user: "u".into(),
            user_id: "usr_0123456789abcdef".into(),
            issued_at: 0,
            expires_at: far,
            ip: None,
            last_used: 0,
        });
    }
    assert_eq!(tokens.sessions().len(), MAX_SESSIONS);
    assert_eq!(
        tokens.sessions()[0]["id"],
        format!("s-{}", MAX_SESSIONS + 9)
    );
    assert!(!tokens.end("s-5"), "evicted from the bounded list");
    assert!(tokens.end("s-50"));
    assert!(!tokens.end("s-50"));
    assert!(tokens.is_revoked("s-50"));
    let generation = tokens.generation();
    assert_eq!(tokens.rotate(), MAX_SESSIONS - 1);
    assert_eq!(tokens.generation(), generation + 1);
    assert!(tokens.sessions().is_empty());
}

#[test]
fn revocations_and_the_generation_survive_a_restart() {
    let dir = tempfile::tempdir().expect("tmp");
    let far = chrono::Utc::now().timestamp() + 3600;
    let a = Tokens::persistent(Some(dir.path()));
    a.revoke("signed-out", far);
    a.revoke_for_agent("agent-token", far);
    a.revoke_subject("client:abc", far);
    a.rotate();
    let generation = a.generation();
    drop(a);
    let file = dir.path().join(SESSIONS_FILE);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).expect("file").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let b = Tokens::persistent(Some(dir.path()));
    assert_eq!(b.generation(), generation, "the generation survives");
    assert!(b.is_revoked("agent-token"));
    assert!(b.is_subject_revoked("client:abc"));
    // The sign-out was cleared by the rotation (its generation is gone).
    assert!(!b.is_revoked("signed-out"));
    b.revoke("signed-out-2", far);
    drop(b);
    assert!(Tokens::persistent(Some(dir.path())).is_revoked("signed-out-2"));
    // Without a state directory the generation only grows across starts.
    let t = Tokens::new();
    assert!(t.generation() > 1_600_000_000);
}

#[test]
fn a_sign_out_everywhere_keeps_an_agent_revocation() {
    let tokens = Tokens::new();
    let far = chrono::Utc::now().timestamp() + 3600;
    tokens.revoke_for_agent("agent-jti", far);
    tokens.revoke("plain-jti", far);
    tokens.rotate();
    assert!(tokens.is_revoked("agent-jti"));
    assert!(!tokens.is_revoked("plain-jti"));
    assert!(tokens.unrevoke("agent-jti"));
    assert!(!tokens.is_revoked("agent-jti"));
}

#[test]
fn parallel_attempts_count_before_their_answers() {
    let tokens = Tokens::new();
    let ip = Some("203.0.113.7".parse().expect("ip"));
    // Fifty guesses in flight at once: only the window's allowance starts.
    let started = (0..50).filter(|_| tokens.check(ip, "u").is_ok()).count();
    assert_eq!(started, FAILURES_PER_WINDOW as usize);
    for _ in 0..started {
        tokens.failed(ip, "u");
    }
    assert!(tokens.check(ip, "u").is_err(), "blocked once they failed");
    // Another address is not affected; an abandoned attempt frees its slot.
    let other = Some("203.0.113.8".parse().expect("ip"));
    assert!(tokens.check(other, "u").is_ok());
    tokens.abandoned(other, "u");
    assert!(tokens.check(other, "u").is_ok());
}

#[tokio::test]
async fn the_hash_pool_is_bounded() {
    let tokens = Tokens::new();
    let held: Vec<_> = (0..HASH_PERMITS)
        .filter_map(|_| tokens.hash_permit())
        .collect();
    assert_eq!(held.len(), HASH_PERMITS);
    assert!(tokens.hash_permit().is_none());
    drop(held);
    assert!(tokens.hash_permit().is_some());
}

#[test]
fn one_client_trying_other_names_does_not_lock_the_user_out() {
    // Behind a proxy without RUSTYBIN_TRUST_FORWARD every client shares one
    // address: failures under another user name leave the real one alone.
    let tokens = Tokens::new();
    let proxy = Some("10.0.0.1".parse().expect("ip"));
    for _ in 0..(FAILURES_PER_WINDOW * 3) {
        if tokens.check(proxy, "admin").is_ok() {
            tokens.failed(proxy, "admin");
        }
    }
    assert!(tokens.check(proxy, "admin").is_err());
    assert!(
        tokens.check(proxy, "Admin").is_err(),
        "names count in lower case"
    );
    assert!(tokens.check(proxy, DEFAULT_USER).is_ok());
}

#[test]
fn an_ipv6_host_rotating_in_its_slash_64_is_one_address() {
    let tokens = Tokens::new();
    for i in 0..FAILURES_PER_WINDOW {
        let ip = std::net::Ipv6Addr::new(0x2001, 0xdb8, 7, 7, i as u16, 0, 0, 1);
        let ip = Some(IpAddr::V6(ip));
        assert!(tokens.check(ip, DEFAULT_USER).is_ok());
        tokens.failed(ip, DEFAULT_USER);
    }
    let next = Some(IpAddr::V6(std::net::Ipv6Addr::new(
        0x2001, 0xdb8, 7, 7, 0xbeef, 0, 0, 9,
    )));
    assert!(tokens.check(next, DEFAULT_USER).is_err());
    let elsewhere = Some(IpAddr::V6(std::net::Ipv6Addr::new(
        0x2001, 0xdb8, 7, 8, 0, 0, 0, 1,
    )));
    assert!(tokens.check(elsewhere, DEFAULT_USER).is_ok());
}

#[test]
fn failures_from_every_address_share_a_budget() {
    let clock = Arc::new(ManualClock::new(chrono::Utc::now()));
    let tokens = Tokens::with_clock(clock.clone());
    for i in 0..GLOBAL_FAILURES_PER_WINDOW {
        let ip = Some(IpAddr::from([198, 51, (i >> 8) as u8, (i & 255) as u8]));
        assert!(tokens.check(ip, DEFAULT_USER).is_ok(), "attempt {i}");
        tokens.failed(ip, DEFAULT_USER);
    }
    // A fresh address waits for the window's end, then may try again.
    let fresh = Some("203.0.113.200".parse().expect("ip"));
    let wait = tokens
        .check(fresh, DEFAULT_USER)
        .expect_err("global budget");
    assert!((1..=WINDOW_SECS).contains(&wait), "{wait}");
    clock.advance(Duration::from_secs(WINDOW_SECS + 1));
    assert!(tokens.check(fresh, DEFAULT_USER).is_ok());
}

#[test]
fn concurrent_revocations_all_reach_the_file() {
    let dir = tempfile::tempdir().expect("tmp");
    let tokens = Arc::new(Tokens::persistent(Some(dir.path())));
    let far = chrono::Utc::now().timestamp() + 3600;
    let threads: Vec<_> = (0..32)
        .map(|t| {
            let tokens = tokens.clone();
            std::thread::spawn(move || {
                for i in 0..10 {
                    tokens.revoke_for_agent(&format!("t{t}-{i}"), far);
                }
            })
        })
        .collect();
    for t in threads {
        t.join().expect("thread");
    }
    drop(tokens);
    let reloaded = Tokens::persistent(Some(dir.path()));
    for t in 0..32 {
        for i in 0..10 {
            assert!(reloaded.is_revoked(&format!("t{t}-{i}")), "t{t}-{i} lost");
        }
    }
}
