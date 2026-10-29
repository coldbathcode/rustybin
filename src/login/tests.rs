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
            tokens.failed(Some(ip));
        }
        let wait = tokens.check(Some(ip)).expect_err("blocked");
        let expected = (FIRST_BLOCK_SECS << round).min(MAX_BLOCK_SECS);
        assert_eq!(wait, expected, "round {round}");
        clock.advance(Duration::from_secs(expected));
        assert!(tokens.check(Some(ip)).is_ok());
    }
    // Attempts without a client address are never blocked, and the table is bounded.
    assert!(tokens.check(None).is_ok());
    for i in 0..(MAX_ATTEMPT_ADDRESSES as u32 + 50) {
        let ip = IpAddr::from([10, 1, (i >> 8) as u8, (i & 255) as u8]);
        tokens.failed(Some(ip));
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
    let (status, v) = with_token(&app, "DELETE", SESSIONS_PATH, &b, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["ended"], 2);
    assert_eq!(v["generation"], 2);
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
        json!({"secrets": {HASH_KEY: "garbage"}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let (status, v) = with_token(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &token,
        json!({"values": {USER_KEY: "no spaces"}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let hash = hash_password("patched-secret").expect("hash");
    let (status, v) = with_token(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &token,
        json!({"values": {USER_KEY: "patched"}, "secrets": {HASH_KEY: hash}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    // A PATCH does not rotate tokens; the new credentials sign in.
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some(&token)).await,
        StatusCode::OK
    );
    assert_eq!(
        login(&app, "patched", "patched-secret").await.0,
        StatusCode::OK
    );
    assert_eq!(
        login(&app, DEFAULT_USER, DEFAULT_PASSWORD).await.0,
        StatusCode::UNAUTHORIZED
    );
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
        .update(
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
    assert_eq!(tokens.rotate(), MAX_SESSIONS - 1);
    assert_eq!(tokens.generation(), 2);
    assert!(tokens.sessions().is_empty());
}
