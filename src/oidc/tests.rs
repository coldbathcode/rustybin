use super::*;
use crate::jwt_state::decode_unverified;
use crate::test_support::{body_json, body_string, module_app};
use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

const CB: &str = "http://localhost:8080/callback";
/// RFC 7636 appendix B example.
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn app() -> Router {
    module_app(router)
}

fn form(pairs: &[(&str, &str)]) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

fn basic(id: &str, secret: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{id}:{secret}"))
    )
}

async fn post(app: &Router, uri: &str, body: String, auth: Option<String>) -> Response {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("host", "localhost");
    if let Some(a) = auth {
        b = b.header("authorization", a);
    }
    app.clone()
        .oneshot(b.body(Body::from(body)).expect("request"))
        .await
        .expect("response")
}

async fn token(app: &Router, pairs: &[(&str, &str)]) -> (StatusCode, Value) {
    let resp = post(app, "/oauth/token", form(pairs), None).await;
    let status = resp.status();
    (status, body_json(resp).await)
}

fn location(resp: &Response) -> String {
    resp.headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

fn claims(token: &str) -> Value {
    decode_unverified(token).expect("jwt").1
}

/// Log in as demo/demo and return the authorization code.
async fn login(app: &Router, extra: &[(&str, &str)]) -> String {
    let mut pairs = vec![
        ("response_type", "code"),
        ("client_id", "rustybin"),
        ("redirect_uri", CB),
        ("username", "demo"),
        ("password", "demo"),
    ];
    pairs.extend_from_slice(extra);
    let resp = post(app, "/oauth/authorize", form(&pairs), None).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    query_param(&location(&resp), "code").expect("code in redirect")
}

#[tokio::test]
async fn discovery_documents_advertise_implemented_features() {
    for path in [
        "/.well-known/openid-configuration",
        "/.well-known/oauth-authorization-server",
    ] {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "localhost")
                    .header("x-forwarded-proto", "https")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        // X-Forwarded-Proto is ignored unless trust_forward is set.
        assert_eq!(json["issuer"], "http://localhost");
        assert_eq!(
            json["registration_endpoint"],
            "http://localhost/oauth/register"
        );
        assert_eq!(json["revocation_endpoint"], "http://localhost/oauth/revoke");
        assert_eq!(json["response_types_supported"], json!(["code"]));
        assert!(json["code_challenge_methods_supported"]
            .as_array()
            .expect("array")
            .contains(&json!("S256")));
        assert!(json["grant_types_supported"]
            .as_array()
            .expect("array")
            .contains(&json!(GRANT_TOKEN_EXCHANGE)));
    }
}

#[test]
fn issuer_honours_forwarded_headers_only_when_trusted() {
    let mut h = HeaderMap::new();
    h.insert("host", HeaderValue::from_static("internal:8080"));
    h.insert("x-forwarded-proto", HeaderValue::from_static("https"));
    h.insert(
        "x-forwarded-host",
        HeaderValue::from_static("api.example.com"),
    );
    h.insert("x-forwarded-prefix", HeaderValue::from_static("/idp/"));
    assert_eq!(issuer_for(&h, false, false), "http://internal:8080");
    assert_eq!(issuer_for(&h, true, false), "https://internal:8080");
    assert_eq!(issuer_for(&h, false, true), "https://api.example.com/idp");
    h.insert("host", HeaderValue::from_static("evil\"host"));
    h.insert("x-forwarded-host", HeaderValue::from_static("a/b"));
    assert_eq!(issuer_for(&h, false, true), "https://localhost/idp");
}

#[tokio::test]
async fn token_errors_are_json_oauth_errors() {
    let app = app();
    // Missing grant_type.
    let (status, json) = token(&app, &[("client_id", "rustybin")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "invalid_request");
    // Wrong content type.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/oauth/token")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(body_json(resp).await["error"], "invalid_request");
    // Unsupported grant.
    let (status, json) = token(&app, &[("grant_type", "magic")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "unsupported_grant_type");
    // Repeated parameter.
    let (status, _) = token(
        &app,
        &[
            ("grant_type", "client_credentials"),
            ("grant_type", "password"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn client_secret_is_validated() {
    let app = app();
    let (status, json) = token(
        &app,
        &[
            ("grant_type", "client_credentials"),
            ("client_id", "rustybin"),
            ("client_secret", "wrong"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"], "invalid_client");
    let (status, _) = token(&app, &[("grant_type", "client_credentials")]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // client_secret_basic works; public client cannot use client_credentials.
    let resp = post(
        &app,
        "/oauth/token",
        form(&[("grant_type", "client_credentials")]),
        Some(basic("rustybin", "secret")),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("pragma").and_then(|v| v.to_str().ok()),
        Some("no-cache")
    );
    let (status, json) = token(
        &app,
        &[
            ("grant_type", "client_credentials"),
            ("client_id", "rustybin-public"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "unauthorized_client");
}

#[tokio::test]
async fn client_credentials_with_resource_sets_audience() {
    let (status, json) = token(
        &app(),
        &[
            ("grant_type", "client_credentials"),
            ("client_id", "rustybin"),
            ("client_secret", "secret"),
            ("resource", "https://mcp.example.com/mcp"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json.get("id_token").is_none());
    assert!(json.get("refresh_token").is_none());
    let c = claims(json["access_token"].as_str().expect("token"));
    assert_eq!(c["aud"], "https://mcp.example.com/mcp");
    assert_eq!(c["client_id"], "rustybin");
    assert_eq!(c["sub"], "rustybin");
    // Access tokens verify through the shared entry point.
    let jwt = JwtState::shared_for_tests();
    assert!(jwt
        .verify_rs256(json["access_token"].as_str().expect("token"))
        .is_ok());
}

#[tokio::test]
async fn password_grant_validates_users() {
    let app = app();
    let creds = [
        ("grant_type", "password"),
        ("client_id", "rustybin"),
        ("client_secret", "secret"),
        ("username", "alice"),
    ];
    let mut ok = creds.to_vec();
    ok.push(("password", "alice"));
    let (status, json) = token(&app, &ok).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["scope"], "openid profile email");
    let access = claims(json["access_token"].as_str().expect("access"));
    assert_eq!(access["aud"], DEFAULT_AUDIENCE);
    assert_eq!(access["groups"], json!(["users", "admins"]));
    let id = claims(json["id_token"].as_str().expect("id"));
    assert_eq!(id["aud"], "rustybin");
    assert_eq!(id["azp"], "rustybin");
    assert!(id["auth_time"].is_i64());
    assert!(json["refresh_token"].is_string());

    let mut bad = creds.to_vec();
    bad.push(("password", "nope"));
    let (status, json) = token(&app, &bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "invalid_grant");
}

#[tokio::test]
async fn authorization_code_flow_with_nonce_and_single_use_code() {
    let app = app();
    let code = login(&app, &[("nonce", "n-0S6"), ("state", "xyz")]).await;
    let exchange = [
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", CB),
        ("client_id", "rustybin"),
        ("client_secret", "secret"),
    ];
    let (status, json) = token(&app, &exchange).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let id = claims(json["id_token"].as_str().expect("id_token"));
    assert_eq!(id["nonce"], "n-0S6");
    assert_eq!(id["aud"], "rustybin");
    assert_eq!(id["name"], "Demo User");
    let access = claims(json["access_token"].as_str().expect("access"));
    assert_eq!(access["aud"], DEFAULT_AUDIENCE);
    assert_eq!(access["client_id"], "rustybin");
    assert!(access.get("nonce").is_none());
    let header = decode_unverified(json["access_token"].as_str().expect("t"))
        .expect("jwt")
        .0;
    assert_eq!(header["typ"], "at+jwt");

    // Reuse fails.
    let (status, json) = token(&app, &exchange).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "invalid_grant");
}

#[tokio::test]
async fn redirect_uri_and_client_must_match_the_code() {
    let app = app();
    let code = login(&app, &[]).await;
    let (status, json) = token(
        &app,
        &[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", "http://localhost:8080/other"),
            ("client_id", "rustybin"),
            ("client_secret", "secret"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "invalid_grant");

    // A code issued to rustybin cannot be redeemed by another client.
    let reg = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/oauth/register")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"redirect_uris": [CB], "grant_types": ["authorization_code"]})
                        .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    let reg = body_json(reg).await;
    let code = login(&app, &[]).await;
    let (status, _) = token(
        &app,
        &[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", CB),
            ("client_id", reg["client_id"].as_str().expect("id")),
            (
                "client_secret",
                reg["client_secret"].as_str().expect("secret"),
            ),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn expired_code_is_rejected() {
    let state = crate::test_support::test_state();
    let oidc = OidcState::new(state.jwt.clone(), false, false);
    oidc.codes.insert_until(
        "old".into(),
        AuthCode {
            client_id: "rustybin".into(),
            redirect_uri: CB.into(),
            redirect_uri_given: true,
            username: "demo".into(),
            scope: "openid".into(),
            nonce: None,
            code_challenge: None,
            resources: Vec::new(),
            auth_time: 0,
        },
        chrono::Utc::now().timestamp() - 1,
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    let body = form(&[
        ("grant_type", "authorization_code"),
        ("code", "old"),
        ("redirect_uri", CB),
        ("client_id", "rustybin"),
        ("client_secret", "secret"),
    ]);
    let err = token_inner(&oidc, "http://localhost", &headers, body.as_bytes())
        .expect_err("expired code rejected");
    assert_eq!(err.error, "invalid_grant");
}

#[tokio::test]
async fn pkce_public_client_success_and_failure() {
    let app = app();
    let pkce = [
        ("code_challenge", CHALLENGE),
        ("code_challenge_method", "S256"),
    ];
    let mut pairs = vec![
        ("response_type", "code"),
        ("client_id", "rustybin-public"),
        ("redirect_uri", CB),
        ("username", "demo"),
        ("password", "demo"),
        ("resource", "https://mcp.example.com/mcp"),
    ];
    pairs.extend_from_slice(&pkce);
    let resp = post(&app, "/oauth/authorize", form(&pairs), None).await;
    let code = query_param(&location(&resp), "code").expect("code");
    let (status, json) = token(
        &app,
        &[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", CB),
            ("client_id", "rustybin-public"),
            ("code_verifier", VERIFIER),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let access = claims(json["access_token"].as_str().expect("access"));
    assert_eq!(access["aud"], "https://mcp.example.com/mcp");

    // Wrong verifier.
    let resp = post(&app, "/oauth/authorize", form(&pairs), None).await;
    let code = query_param(&location(&resp), "code").expect("code");
    let wrong = "x".repeat(43);
    let (status, json) = token(
        &app,
        &[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", CB),
            ("client_id", "rustybin-public"),
            ("code_verifier", wrong.as_str()),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "invalid_grant");

    // Public client without PKCE: error redirect.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/oauth/authorize?response_type=code&client_id=rustybin-public&redirect_uri={CB}&state=s1"
                ))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let loc = location(&resp);
    assert_eq!(
        query_param(&loc, "error").as_deref(),
        Some("invalid_request")
    );
    assert_eq!(query_param(&loc, "state").as_deref(), Some("s1"));

    // Plain method also works for a confidential client.
    let plain = "p".repeat(50);
    let code = login(
        &app,
        &[
            ("code_challenge", plain.as_str()),
            ("code_challenge_method", "plain"),
        ],
    )
    .await;
    let (status, _) = token(
        &app,
        &[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", CB),
            ("client_id", "rustybin"),
            ("client_secret", "secret"),
            ("code_verifier", plain.as_str()),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn authorize_page_escapes_everything() {
    let resp = app()
        .oneshot(
            Request::builder()
                .uri("/oauth/authorize?response_type=code&client_id=rustybin&redirect_uri=http%3A%2F%2Flocalhost%2Fcb&state=%22%3E%3Cscript%3Ealert(1)%3C%2Fscript%3E&scope=openid&login_hint=%3Cimg%20src%3Dx%3E&nonce=%27%22%3E")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().contains_key("content-security-policy"));
    let html = body_string(resp).await;
    assert!(!html.contains("<script>"), "{html}");
    assert!(!html.contains("<img"), "{html}");
    assert!(html.contains("&lt;script&gt;"));
    assert!(html.contains("&#39;&quot;&gt;"));
}

#[tokio::test]
async fn authorize_rejects_bad_clients_and_redirect_uris_without_redirecting() {
    let app = app();
    for uri in [
        "/oauth/authorize?response_type=code&client_id=nope&redirect_uri=http://localhost/cb",
        "/oauth/authorize?response_type=code&client_id=rustybin&redirect_uri=javascript:alert(1)",
        "/oauth/authorize?response_type=code&client_id=rustybin",
        "/oauth/authorize?response_type=code&client_id=rustybin&redirect_uri=http://localhost/cb%23frag",
    ] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
        assert!(!resp.headers().contains_key("location"));
    }
    // Unsupported response type redirects with an error.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/oauth/authorize?response_type=token&client_id=rustybin&redirect_uri=http://localhost/cb")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        query_param(&location(&resp), "error").as_deref(),
        Some("unsupported_response_type")
    );
}

#[tokio::test]
async fn location_is_encoded_and_never_panics() {
    let app = app();
    let resp = post(
        &app,
        "/oauth/authorize",
        form(&[
            ("response_type", "code"),
            ("client_id", "rustybin"),
            ("redirect_uri", "http://localhost/cb?existing=1"),
            ("username", "demo"),
            ("password", "demo"),
            ("state", "line1\r\nSet-Cookie: evil=1&x=<y>"),
        ]),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let loc = location(&resp);
    assert!(
        loc.starts_with("http://localhost/cb?existing=1&code="),
        "{loc}"
    );
    assert!(!loc.contains('\n') && !loc.contains('\r'));
    assert_eq!(
        query_param(&loc, "state").as_deref(),
        Some("line1\r\nSet-Cookie: evil=1&x=<y>")
    );
    // Deny redirects with access_denied.
    let resp = post(
        &app,
        "/oauth/authorize",
        form(&[
            ("response_type", "code"),
            ("client_id", "rustybin"),
            ("redirect_uri", CB),
            ("action", "deny"),
        ]),
        None,
    )
    .await;
    assert_eq!(
        query_param(&location(&resp), "error").as_deref(),
        Some("access_denied")
    );
    // Wrong password shows the form again.
    let resp = post(
        &app,
        "/oauth/authorize",
        form(&[
            ("response_type", "code"),
            ("client_id", "rustybin"),
            ("redirect_uri", CB),
            ("username", "demo"),
            ("password", "bad"),
        ]),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(!resp.headers().contains_key("location"));
}

#[tokio::test]
async fn refresh_tokens_rotate() {
    let app = app();
    let (_, json) = token(
        &app,
        &[
            ("grant_type", "password"),
            ("client_id", "rustybin"),
            ("client_secret", "secret"),
            ("username", "demo"),
            ("password", "demo"),
        ],
    )
    .await;
    let first = json["refresh_token"].as_str().expect("refresh").to_string();
    let refresh = |rt: String, scope: Option<&'static str>| {
        let app = app.clone();
        async move {
            let mut pairs = vec![
                ("grant_type", "refresh_token".to_string()),
                ("client_id", "rustybin".to_string()),
                ("client_secret", "secret".to_string()),
                ("refresh_token", rt),
            ];
            if let Some(s) = scope {
                pairs.push(("scope", s.to_string()));
            }
            let pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
            token(&app, &pairs).await
        }
    };
    let (status, json) = refresh(first.clone(), Some("openid")).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["scope"], "openid");
    let second = json["refresh_token"].as_str().expect("rotated").to_string();
    assert_ne!(first, second);
    // The old refresh token is gone.
    let (status, json) = refresh(first, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "invalid_grant");
    // Scope cannot grow beyond the original grant.
    let (status, json) = refresh(second, Some("openid admin")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "invalid_scope");
}

async fn introspect_token(app: &Router, token: &str, auth: Option<String>) -> (StatusCode, Value) {
    let resp = post(app, "/oauth/introspect", form(&[("token", token)]), auth).await;
    let status = resp.status();
    (status, body_json(resp).await)
}

#[tokio::test]
async fn introspection_and_revocation() {
    let app = app();
    let (_, json) = token(
        &app,
        &[
            ("grant_type", "password"),
            ("client_id", "rustybin"),
            ("client_secret", "secret"),
            ("username", "bob"),
            ("password", "bob"),
            ("resource", "https://a.example"),
            ("resource", "https://b.example"),
        ],
    )
    .await;
    let access = json["access_token"].as_str().expect("access").to_string();
    let refresh = json["refresh_token"].as_str().expect("refresh").to_string();
    let creds = || Some(basic("rustybin", "secret"));

    // Client authentication is required.
    let (status, json) = introspect_token(&app, &access, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"], "invalid_client");

    let (status, json) = introspect_token(&app, &access, creds()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["active"], true);
    assert_eq!(json["client_id"], "rustybin");
    assert_eq!(json["username"], "bob");
    assert_eq!(json["token_type"], "Bearer");
    assert_eq!(
        json["aud"],
        json!(["https://a.example", "https://b.example"])
    );
    assert_eq!(json["iss"], "http://localhost");
    assert!(json["iat"].is_i64());

    let (_, json) = introspect_token(&app, &refresh, creds()).await;
    assert_eq!(json["active"], true);
    assert_eq!(json["token_type"], "refresh_token");

    for bad in [
        "invalid.token.here",
        crate::auth_jwt::SAMPLE_JWT.trim_end_matches('w'),
    ] {
        let (status, json) = introspect_token(&app, bad, creds()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!({"active": false}));
    }

    // Revoke both, then they are inactive.
    for t in [&access, &refresh] {
        let resp = post(&app, "/oauth/revoke", form(&[("token", t)]), creds()).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }
    for t in [&access, &refresh] {
        let (_, json) = introspect_token(&app, t, creds()).await;
        assert_eq!(json["active"], false);
    }
    // Revoked access token no longer works at userinfo.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/oauth/userinfo")
                .header("authorization", format!("Bearer {access}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn userinfo_get_and_post() {
    let app = app();
    let (_, json) = token(
        &app,
        &[
            ("grant_type", "password"),
            ("client_id", "rustybin"),
            ("client_secret", "secret"),
            ("username", "alice"),
            ("password", "alice"),
        ],
    )
    .await;
    let access = json["access_token"].as_str().expect("access").to_string();
    let id_token = json["id_token"].as_str().expect("id").to_string();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/oauth/userinfo")
                .header("authorization", format!("bearer {access}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let info = body_json(resp).await;
    assert_eq!(info["sub"], "alice");
    assert_eq!(info["name"], "Alice Admin");
    assert_eq!(info["email"], "alice@rustybin.local");

    let resp = post(
        &app,
        "/oauth/userinfo",
        form(&[("access_token", &access)]),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // An ID token is not an access token; missing token is 401.
    let resp = post(
        &app,
        "/oauth/userinfo",
        String::new(),
        Some(format!("Bearer {id_token}")),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let resp = post(&app, "/oauth/userinfo", String::new(), None).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(resp.headers().contains_key("www-authenticate"));
}

#[tokio::test]
async fn token_exchange_requires_verified_subject() {
    let app = app();
    let exchange = |subject: String, extra: Vec<(&'static str, String)>| {
        let app = app.clone();
        async move {
            let mut pairs = vec![
                ("grant_type", GRANT_TOKEN_EXCHANGE.to_string()),
                ("client_id", "rustybin".to_string()),
                ("client_secret", "secret".to_string()),
                ("subject_token", subject),
                ("subject_token_type", TOKEN_TYPE_ACCESS_TOKEN.to_string()),
            ];
            pairs.extend(extra);
            let pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
            token(&app, &pairs).await
        }
    };

    // The HS256 sample token (demo secret) verifies.
    let (status, json) = exchange(
        crate::auth_jwt::SAMPLE_JWT.to_string(),
        vec![("audience", "my-service".into())],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["issued_token_type"], TOKEN_TYPE_ACCESS_TOKEN);
    assert_eq!(json["scope"], "openid profile");
    let c = claims(json["access_token"].as_str().expect("token"));
    assert_eq!(c["sub"], "1234567890");
    assert_eq!(c["aud"], "my-service");

    // Forged / third-party tokens are rejected with invalid_request.
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let forged = format!(
        "{}.{}.fakesignature",
        b64.encode(r#"{"alg":"RS256","typ":"JWT"}"#),
        b64.encode(r#"{"sub":"external-user","exp":4102444800,"scope":"admin"}"#)
    );
    let (status, json) = exchange(forged.clone(), vec![]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "invalid_request");

    // Delegation with a verified actor token.
    let (_, cc) = token(
        &app,
        &[
            ("grant_type", "client_credentials"),
            ("client_id", "rustybin"),
            ("client_secret", "secret"),
        ],
    )
    .await;
    let actor = cc["access_token"].as_str().expect("actor").to_string();
    let (status, json) = exchange(
        crate::auth_jwt::SAMPLE_JWT.to_string(),
        vec![
            ("actor_token", actor.clone()),
            ("actor_token_type", TOKEN_TYPE_ACCESS_TOKEN.into()),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let c = claims(json["access_token"].as_str().expect("token"));
    assert_eq!(c["act"]["sub"], "rustybin");

    // Forged actor, missing actor type, scope escalation.
    let (status, _) = exchange(
        crate::auth_jwt::SAMPLE_JWT.to_string(),
        vec![
            ("actor_token", forged),
            ("actor_token_type", TOKEN_TYPE_ACCESS_TOKEN.into()),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, json) = exchange(
        crate::auth_jwt::SAMPLE_JWT.to_string(),
        vec![("actor_token", actor)],
    )
    .await;
    assert!(json["error_description"]
        .as_str()
        .unwrap_or("")
        .contains("actor_token_type"));
    let (_, json) = exchange(
        crate::auth_jwt::SAMPLE_JWT.to_string(),
        vec![("scope", "admin".into())],
    )
    .await;
    assert_eq!(json["error"], "invalid_scope");

    // Requested ID token.
    let (status, json) = exchange(
        crate::auth_jwt::SAMPLE_JWT.to_string(),
        vec![("requested_token_type", TOKEN_TYPE_ID_TOKEN.into())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["issued_token_type"], TOKEN_TYPE_ID_TOKEN);
    assert_eq!(json["token_type"], "N_A");
}

#[tokio::test]
async fn dynamic_client_registration_end_to_end() {
    let app = app();
    let register = |body: Value, content_type: &'static str| {
        let app = app.clone();
        async move {
            let resp = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/oauth/register")
                        .header("content-type", content_type)
                        .body(Body::from(body.to_string()))
                        .expect("request"),
                )
                .await
                .expect("response");
            let status = resp.status();
            (status, body_json(resp).await)
        }
    };
    let (status, reg) = register(
        json!({
            "client_name": "mcp client",
            "redirect_uris": [CB],
            "grant_types": ["authorization_code", "refresh_token"],
            "token_endpoint_auth_method": "none"
        }),
        "application/json",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{reg}");
    let client_id = reg["client_id"].as_str().expect("id").to_string();
    assert!(reg.get("client_secret").is_none());

    // Registered redirect URI is enforced exactly.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri=http://localhost:8080/evil&code_challenge={CHALLENGE}&code_challenge_method=S256"
                ))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = post(
        &app,
        "/oauth/authorize",
        form(&[
            ("response_type", "code"),
            ("client_id", &client_id),
            ("username", "demo"),
            ("password", "demo"),
            ("code_challenge", CHALLENGE),
            ("code_challenge_method", "S256"),
        ]),
        None,
    )
    .await;
    // redirect_uri omitted: the single registered one is used.
    assert!(location(&resp).starts_with(CB), "{}", location(&resp));
    let code = query_param(&location(&resp), "code").expect("code");
    let (status, json) = token(
        &app,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client_id),
            ("code_verifier", VERIFIER),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert!(json["refresh_token"].is_string());

    let (status, json) =
        register(json!({"redirect_uris": ["ftp://x/cb"]}), "application/json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "invalid_redirect_uri");
    let (status, json) = register(json!({}), "text/plain").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "invalid_client_metadata");
}

#[tokio::test]
async fn jwks_has_the_signing_key() {
    let resp = app()
        .oneshot(
            Request::builder()
                .uri("/oauth/jwks")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let json = body_json(resp).await;
    assert_eq!(json["keys"][0]["kid"], crate::jwt_state::RS256_KID);
    assert_eq!(json["keys"][0]["alg"], "RS256");
}
