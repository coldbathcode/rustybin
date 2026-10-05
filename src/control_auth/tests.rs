use super::*;
use crate::test_support::{
    bearer_request, body_json, control_jwt, control_keys, jwt_config, sign_control_jwt,
    test_app_with, test_state_with,
};
use axum::body::Body;
use axum::http::Request as HttpRequest;
use axum::Router;
use tower::ServiceExt;

fn token_config() -> Config {
    let mut c = Config::for_tests();
    c.control_auth = ControlAuth::Token;
    c.admin_token = Some("admin-secret".to_string());
    c
}

fn app(config: Config) -> Router {
    test_app_with(test_state_with(config))
}

async fn status_of(app: &Router, method: &str, uri: &str, token: Option<&str>) -> StatusCode {
    app.clone()
        .oneshot(bearer_request(method, uri, token))
        .await
        .expect("response")
        .status()
}

async fn reason_of(app: &Router, uri: &str, token: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(bearer_request("GET", uri, Some(token)))
        .await
        .expect("response");
    let status = resp.status();
    (status, body_json(resp).await)
}

const CONTROL_ROUTES: &[&str] = &[
    "/_rustybin/config",
    "/_rustybin/version",
    "/_rustybin/status",
    "/_rustybin/catalog",
    "/_rustybin/usage",
    "/_rustybin/metrics",
    "/_rustybin/requests",
    "/_rustybin/requests/some-id",
];

#[test]
fn modes_parse() {
    assert_eq!(ControlAuth::parse("JWT"), Some(ControlAuth::Jwt));
    assert_eq!(ControlAuth::parse("token"), Some(ControlAuth::Token));
    assert_eq!(ControlAuth::parse("open"), Some(ControlAuth::Open));
    assert_eq!(ControlAuth::parse("maybe"), None);
}

#[test]
fn public_key_formats() {
    let expected = parse_public_key(control_keys::CONTROL_JWT_PUBLIC_PEM).expect("pem");
    // PEM with literal \n (as often written in env files) and on one line.
    let escaped = control_keys::CONTROL_JWT_PUBLIC_PEM.replace('\n', "\\n");
    assert_eq!(parse_public_key(&escaped), Some(expected));
    let one_line = control_keys::CONTROL_JWT_PUBLIC_PEM.replace('\n', " ");
    assert_eq!(parse_public_key(&one_line), Some(expected));
    // Raw 32 bytes, standard and url-safe base64, with or without padding.
    let raw = control_keys::CONTROL_JWT_PUBLIC_RAW_B64;
    assert_eq!(parse_public_key(raw), Some(expected));
    let url = raw.replace('+', "-").replace('/', "_");
    assert_eq!(parse_public_key(url.trim_end_matches('=')), Some(expected));
    // Base64 DER without armor.
    let der_b64: String = control_keys::CONTROL_JWT_PUBLIC_PEM
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect();
    assert_eq!(parse_public_key(&der_b64), Some(expected));
    // Garbage, a private key and wrong lengths are refused.
    assert_eq!(parse_public_key("not a key"), None);
    assert_eq!(parse_public_key("AAAA"), None);
    assert_eq!(
        parse_public_key(control_keys::CONTROL_JWT_PRIVATE_PEM),
        None,
        "a PKCS#8 private key is not a public key"
    );
}

#[test]
fn scope_claims() {
    let g = Grant::from_scope_claim(None);
    assert!(g.console && g.inspector && !g.admin);
    let g = Grant::from_scope_claim(Some(&json!("inspector")));
    assert!(g.inspector && !g.console && !g.admin);
    let g = Grant::from_scope_claim(Some(&json!("console inspector")));
    assert!(g.console && !g.admin);
    let g = Grant::from_scope_claim(Some(&json!(["admin"])));
    assert!(g.admin && g.console && g.inspector);
    let g = Grant::from_scope_claim(Some(&json!("billing")));
    assert!(!g.admin && !g.console && !g.inspector);
    let g = Grant::from_scope_claim(Some(&json!(42)));
    assert!(!g.inspector);

    let inspector = Grant::from_scope_claim(Some(&json!("inspector")));
    assert!(inspector.allows(&Method::GET, "/_rustybin/requests"));
    assert!(inspector.allows(&Method::GET, "/_rustybin/requests/stream"));
    assert!(!inspector.allows(&Method::GET, "/_rustybin/requestsx"));
    assert!(!inspector.allows(&Method::GET, "/_rustybin/config"));
    assert!(!inspector.allows(&Method::DELETE, "/_rustybin/requests"));
    let console = Grant::from_scope_claim(Some(&json!("console")));
    assert!(console.allows(&Method::GET, "/_rustybin/metrics"));
    assert!(!console.allows(&Method::DELETE, "/_rustybin/requests"));
    assert!(Grant::ADMIN.allows(&Method::DELETE, "/_rustybin/requests"));
}

#[test]
fn protected_paths() {
    assert!(is_protected_path("/_rustybin"));
    assert!(is_protected_path("/_rustybin/config"));
    assert!(is_protected_path("/_rustybin/ready/x"));
    assert!(!is_protected_path("/_rustybin/ready"));
    assert!(!is_protected_path("/_rustybinx"));
    assert!(!is_protected_path("/ui/"));
    assert!(!is_protected_path("/echo"));
}

#[tokio::test]
async fn open_mode_is_unchanged() {
    let app = app(Config::for_tests());
    for uri in CONTROL_ROUTES {
        let s = status_of(&app, "GET", uri, None).await;
        assert_ne!(s, StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn token_mode_requires_the_admin_token() {
    let app = app(token_config());
    for uri in CONTROL_ROUTES {
        let resp = app
            .clone()
            .oneshot(bearer_request("GET", uri, None))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{uri}");
        assert!(resp
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("Bearer")));
        let v = body_json(resp).await;
        assert_eq!(v["reason"], "missing_token");
        assert_eq!(v["control_auth"], "token");

        assert_eq!(
            status_of(&app, "GET", uri, Some("wrong")).await,
            StatusCode::UNAUTHORIZED,
            "{uri}"
        );
        let ok = status_of(&app, "GET", uri, Some("admin-secret")).await;
        assert!(
            ok == StatusCode::OK || ok == StatusCode::NOT_FOUND,
            "{uri}: {ok}"
        );
    }
    // The alternative header works too, and so do mutations.
    let req = HttpRequest::builder()
        .uri("/_rustybin/config")
        .header("x-rustybin-admin-token", "admin-secret")
        .body(Body::empty())
        .expect("request");
    assert_eq!(
        app.clone().oneshot(req).await.expect("response").status(),
        StatusCode::OK
    );
    assert_eq!(
        status_of(&app, "DELETE", "/_rustybin/requests", Some("admin-secret")).await,
        StatusCode::OK
    );
    // A JWT is not accepted in token mode, even a valid one.
    assert_eq!(
        status_of(
            &app,
            "GET",
            "/_rustybin/config",
            Some(&control_jwt(Some("admin")))
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn token_mode_without_admin_token_refuses_everything() {
    let mut c = token_config();
    c.admin_token = None;
    let app = app(c);
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some("anything")).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/ready", None).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn data_plane_ready_and_console_files_stay_open() {
    for config in [token_config(), jwt_config()] {
        let app = app(config);
        for uri in [
            "/",
            "/echo",
            "/status/204",
            "/_rustybin/ready",
            "/ui/",
            "/ui/app.js",
        ] {
            let s = status_of(&app, "GET", uri, None).await;
            assert!(s.is_success(), "{uri}: {s}");
        }
        // Even with a wrong token: data-plane routes never look at it.
        assert_eq!(
            status_of(&app, "GET", "/echo", Some("garbage")).await,
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn cors_preflight_is_answered_without_credentials() {
    let app = app(jwt_config());
    let req = HttpRequest::builder()
        .method("OPTIONS")
        .uri("/_rustybin/status")
        .header("origin", "https://portal.example.com")
        .header("access-control-request-method", "GET")
        .header("access-control-request-headers", "authorization")
        .body(Body::empty())
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    // And a 401 still carries the CORS header so browsers can read it.
    let req = HttpRequest::builder()
        .uri("/_rustybin/status")
        .header("origin", "https://portal.example.com")
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(resp.headers().contains_key("access-control-allow-origin"));
}

#[tokio::test]
async fn jwt_mode_scopes() {
    let app = app(jwt_config());
    let console = control_jwt(Some("console"));
    let inspector = control_jwt(Some("inspector"));
    let admin = control_jwt(Some("admin"));
    let no_scope = control_jwt(None);

    for uri in CONTROL_ROUTES {
        assert_eq!(
            status_of(&app, "GET", uri, None).await,
            StatusCode::UNAUTHORIZED,
            "{uri}"
        );
        let s = status_of(&app, "GET", uri, Some(&console)).await;
        assert!(
            s == StatusCode::OK || s == StatusCode::NOT_FOUND,
            "{uri}: {s}"
        );
        let s = status_of(&app, "GET", uri, Some(&no_scope)).await;
        assert!(
            s == StatusCode::OK || s == StatusCode::NOT_FOUND,
            "{uri}: {s}"
        );
    }
    // Inspector scope: only /_rustybin/requests*.
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/requests", Some(&inspector)).await,
        StatusCode::OK
    );
    let (s, v) = reason_of(&app, "/_rustybin/config", &inspector).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(v["reason"], "insufficient_scope");
    assert_eq!(v["scopes"], json!(["inspector"]));
    // Mutations need the admin token or the admin scope.
    for t in [&console, &inspector, &no_scope] {
        assert_eq!(
            status_of(&app, "DELETE", "/_rustybin/requests", Some(t)).await,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        status_of(&app, "DELETE", "/_rustybin/requests", Some(&admin)).await,
        StatusCode::OK
    );
    assert_eq!(
        status_of(&app, "DELETE", "/_rustybin/requests", Some("admin-secret")).await,
        StatusCode::OK
    );
    // The admin token keeps working in jwt mode.
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some("admin-secret")).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn jwt_refusals() {
    let app = app(jwt_config());
    let now = chrono::Utc::now().timestamp();
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "expired",
            sign_control_jwt(&json!({"aud": "test-instance", "exp": now - 120}), false),
            "expired",
        ),
        (
            "wrong audience",
            sign_control_jwt(&json!({"aud": "other-pod", "exp": now + 600}), false),
            "invalid_audience",
        ),
        (
            "wrong key",
            sign_control_jwt(&json!({"aud": "test-instance", "exp": now + 600}), true),
            "invalid_signature",
        ),
        (
            "no exp",
            sign_control_jwt(&json!({"aud": "test-instance"}), false),
            "missing_claim",
        ),
        (
            "no aud",
            sign_control_jwt(&json!({"exp": now + 600}), false),
            "missing_claim",
        ),
        (
            "not yet valid",
            sign_control_jwt(
                &json!({"aud": "test-instance", "exp": now + 600, "nbf": now + 300}),
                false,
            ),
            "not_yet_valid",
        ),
        ("garbage", "a.b.c".to_string(), "malformed"),
        ("not a jwt", "admin-wrong".to_string(), "invalid_token"),
    ];
    for (name, token, reason) in cases {
        let resp = app
            .clone()
            .oneshot(bearer_request("GET", "/_rustybin/status", Some(&token)))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{name}");
        assert!(resp
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("invalid_token")));
        assert_eq!(body_json(resp).await["reason"], reason, "{name}");
    }

    // Algorithm confusion: an HS256 token "signed" with the public key bytes.
    let hs = jsonwebtoken::encode(
        &jsonwebtoken::Header::new(Algorithm::HS256),
        &json!({"aud": "test-instance", "exp": now + 600, "scope": "admin"}),
        &jsonwebtoken::EncodingKey::from_secret(control_keys::CONTROL_JWT_PUBLIC_PEM.as_bytes()),
    )
    .expect("sign");
    let (s, v) = reason_of(&app, "/_rustybin/status", &hs).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(v["reason"], "invalid_algorithm");
}

#[tokio::test]
async fn jwt_mode_without_key_accepts_only_the_admin_token() {
    let mut c = jwt_config();
    c.control_jwt_key = None;
    let app = app(c);
    assert_eq!(
        status_of(
            &app,
            "GET",
            "/_rustybin/config",
            Some(&control_jwt(Some("admin")))
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some("admin-secret")).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn admin_scope_jwt_passes_require_admin_on_the_data_plane() {
    let state = test_state_with(jwt_config());
    let app = test_app_with(state.clone());
    let console = control_jwt(Some("console"));
    assert_eq!(
        status_of(&app, "POST", "/health/unhealthy", Some(&console)).await,
        StatusCode::UNAUTHORIZED
    );
    assert!(state.health.is_healthy());
    let admin = control_jwt(Some("admin"));
    assert!(status_of(&app, "POST", "/health/unhealthy", Some(&admin))
        .await
        .is_success());
    assert!(!state.health.is_healthy());
    // Readiness does not follow the demo toggle.
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/ready", None).await,
        StatusCode::OK
    );
    assert_eq!(
        status_of(&app, "GET", "/health", None).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn unauthenticated_control_requests_cost_no_quota() {
    let mut c = token_config();
    c.limits = crate::limits::LimitsConfig::preset(crate::limits::Plan::Pro);
    c.limits.requests = 2;
    let app = app(c);
    for _ in 0..5 {
        assert_eq!(
            status_of(&app, "GET", "/_rustybin/config", None).await,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(status_of(&app, "GET", "/echo", None).await, StatusCode::OK);
    assert_eq!(status_of(&app, "GET", "/echo", None).await, StatusCode::OK);
    assert_eq!(
        status_of(&app, "GET", "/echo", None).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Authenticated control requests and readiness are never rejected.
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/config", Some("admin-secret")).await,
        StatusCode::OK
    );
    assert_eq!(
        status_of(&app, "GET", "/_rustybin/ready", None).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn hosted_mode_preset() {
    let lookup = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
        }
    };
    let (c, w) = Config::from_lookup(lookup(&[
        ("RUSTYBIN_HOSTED_MODE", "true"),
        ("RUSTYBIN_INSTANCE_ID", "acme"),
        ("RUSTYBIN_ADMIN_TOKEN", "t"),
        (
            "RUSTYBIN_CONTROL_JWT_PUBLIC_KEY",
            "zD7+5ZDAX/ZxKGaO1Bm32ZSI1/eWEnNKkeiV8s2lyu0=",
        ),
    ]));
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(c.control_auth, ControlAuth::Jwt);
    assert_eq!(c.log_format, crate::logging::LogFormat::Json);
    assert_eq!(c.control_jwt_audience, "acme");
    assert!(c.control_jwt_key.is_some());

    // Explicit values win over the preset.
    let (c, _) = Config::from_lookup(lookup(&[
        ("RUSTYBIN_HOSTED_MODE", "true"),
        ("RUSTYBIN_CONTROL_AUTH", "token"),
        ("RUSTYBIN_LOG_FORMAT", "text"),
        ("RUSTYBIN_CONTROL_JWT_AUDIENCE", "aud-1"),
    ]));
    assert_eq!(c.control_auth, ControlAuth::Token);
    assert_eq!(c.log_format, crate::logging::LogFormat::Text);
    assert_eq!(c.control_jwt_audience, "aud-1");

    // Misconfigurations warn.
    let (_, w) = Config::from_lookup(lookup(&[("RUSTYBIN_CONTROL_AUTH", "jwt")]));
    assert_eq!(w.len(), 1, "{w:?}");
    let (_, w) = Config::from_lookup(lookup(&[
        ("RUSTYBIN_CONTROL_AUTH", "sometimes"),
        ("RUSTYBIN_CONTROL_JWT_PUBLIC_KEY", "nope"),
        ("RUSTYBIN_LOG_FORMAT", "yaml"),
    ]));
    assert_eq!(w.len(), 3, "{w:?}");
    let (c, _) = Config::from_lookup(lookup(&[]));
    assert_eq!(c.control_auth, ControlAuth::Open);
    assert_eq!(c.log_format, crate::logging::LogFormat::Text);
}
