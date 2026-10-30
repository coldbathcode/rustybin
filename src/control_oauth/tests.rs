//! OAuth sign-in to the management MCP server, through the full application.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::test_support::{
    body_json, body_string, control_jwt, jwt_config, test_app_with, test_state_with,
};

const HOST: &str = "rb.test";
const BASE: &str = "http://rb.test";
const ADMIN: &str = "admin-secret";
const REDIRECT: &str = "http://127.0.0.1:33418/callback";
// RFC 7636 appendix B.
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn token_config() -> Config {
    let mut c = Config::for_tests();
    c.control_auth = ControlAuth::Token;
    c.admin_token = Some(ADMIN.into());
    c.control_oauth_key = OAuthKey::from_settings(None, Some(ADMIN));
    c
}

fn app(config: Config) -> Router {
    test_app_with(test_state_with(config))
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<(&str, String)>,
) -> axum::response::Response {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", HOST);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let req = match body {
        Some((ct, text)) => b.header("content-type", ct).body(Body::from(text)),
        None => b.body(Body::empty()),
    }
    .expect("request");
    app.clone().oneshot(req).await.expect("response")
}

fn location(resp: &axum::response::Response) -> String {
    resp.headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

fn param(url: &str, name: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    form_urlencoded::parse(q.as_bytes())
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

async fn register(app: &Router, uris: &[&str]) -> (StatusCode, Value) {
    let resp = send(
        app,
        "POST",
        REGISTER_PATH,
        &[],
        Some((
            "application/json",
            json!({ "redirect_uris": uris, "client_name": "Test agent" }).to_string(),
        )),
    )
    .await;
    let status = resp.status();
    (status, body_json(resp).await)
}

fn authorize_url(client_id: &str, redirect: &str, scope: Option<&str>) -> String {
    let mut q = form_urlencoded::Serializer::new(String::new());
    q.append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect)
        .append_pair("code_challenge", CHALLENGE)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", "st-123")
        .append_pair("resource", &format!("{BASE}{MANAGE_PATH}"));
    if let Some(s) = scope {
        q.append_pair("scope", s);
    }
    format!("{AUTHORIZE_PATH}?{}", q.finish())
}

/// Register, authorize and return the pending request id.
async fn start(app: &Router, scope: Option<&str>) -> (String, String) {
    let (status, reg) = register(app, &[REDIRECT]).await;
    assert_eq!(status, StatusCode::CREATED, "{reg}");
    let client_id = reg["client_id"].as_str().expect("client_id").to_string();
    // The loopback port may differ from the registered one (RFC 8252 7.3).
    let redirect = REDIRECT.replace("33418", "40001");
    let resp = send(
        app,
        "GET",
        &authorize_url(&client_id, &redirect, scope),
        &[],
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FOUND);
    let loc = location(&resp);
    let id = loc
        .strip_prefix(CONSENT_PAGE)
        .expect("consent page")
        .to_string();
    (client_id, id)
}

async fn answer(app: &Router, id: &str, auth: &str, body: Value) -> (StatusCode, Value) {
    let bearer = format!("Bearer {auth}");
    let resp = send(
        app,
        "POST",
        &format!("{REQUESTS_PREFIX}{id}"),
        &[("authorization", bearer.as_str())],
        Some(("application/json", body.to_string())),
    )
    .await;
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn exchange(
    app: &Router,
    client_id: &str,
    code: &str,
    verifier: &str,
) -> (StatusCode, Value) {
    let mut f = form_urlencoded::Serializer::new(String::new());
    f.append_pair("grant_type", "authorization_code")
        .append_pair("client_id", client_id)
        .append_pair("code", code)
        .append_pair("code_verifier", verifier)
        .append_pair("redirect_uri", &REDIRECT.replace("33418", "40001"));
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

/// The whole flow with `approver` choosing `scope`; returns the token response.
async fn sign_in(app: &Router, approver: &str, scope: Option<&str>) -> (String, Value) {
    let (client_id, id) = start(app, None).await;
    let body = match scope {
        Some(s) => json!({ "decision": "approve", "scope": s }),
        None => json!({ "decision": "approve" }),
    };
    let (status, ans) = answer(app, &id, approver, body).await;
    assert_eq!(status, StatusCode::OK, "{ans}");
    let to = ans["redirect_to"].as_str().expect("redirect_to");
    let code = param(to, "code").expect("code");
    let (status, tokens) = exchange(app, &client_id, &code, VERIFIER).await;
    assert_eq!(status, StatusCode::OK, "{tokens}");
    (client_id, tokens)
}

async fn mcp(
    app: &Router,
    token: Option<&str>,
    method: &str,
    params: Value,
) -> axum::response::Response {
    let bearer = token.map(|t| format!("Bearer {t}"));
    let mut headers = vec![
        ("accept", "application/json, text/event-stream"),
        ("mcp-protocol-version", "2026-07-28"),
        ("mcp-method", method),
    ];
    if let Some(b) = &bearer {
        headers.push(("authorization", b.as_str()));
    }
    let mut params = params;
    params["_meta"] = json!({
        crate::mcp::protocol::META_PROTOCOL_VERSION: "2026-07-28",
        crate::mcp::protocol::META_CLIENT_CAPABILITIES: {},
    });
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(n) = &name {
        headers.push(("mcp-name", n.as_str()));
    }
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    send(
        app,
        "POST",
        MANAGE_PATH,
        &headers,
        Some(("application/json", body.to_string())),
    )
    .await
}

async fn tool_names(app: &Router, token: &str) -> Vec<String> {
    let resp = mcp(app, Some(token), "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    v["result"]["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn off_on_an_open_control_plane() {
    let app = app(Config::for_tests());
    for path in [PRM_PATH, AS_METADATA_PATH] {
        let resp = send(&app, "GET", path, &[], None).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
    }
    let (status, _) = register(&app, &[REDIRECT]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(mode(&Config::for_tests()), "off");
}

#[tokio::test]
async fn off_when_disabled_or_in_shared_mode() {
    let mut c = token_config();
    c.control_oauth = false;
    assert!(!enabled(&c));
    let app = app(c);
    assert_eq!(
        send(&app, "GET", PRM_PATH, &[], None).await.status(),
        StatusCode::NOT_FOUND
    );
    let resp = mcp(&app, None, "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let challenge = resp.headers()["www-authenticate"]
        .to_str()
        .unwrap_or("")
        .to_string();
    assert!(!challenge.contains("resource_metadata"), "{challenge}");

    let mut c = token_config();
    c.shared_mode = true;
    assert!(!enabled(&c));
}

#[tokio::test]
async fn discovery_from_the_401() {
    let app = app(token_config());
    let resp = mcp(&app, None, "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let challenge = resp.headers()["www-authenticate"]
        .to_str()
        .unwrap_or("")
        .to_string();
    assert!(
        challenge.contains(&format!("resource_metadata=\"{BASE}{PRM_PATH}\"")),
        "{challenge}"
    );
    // Other control-plane routes keep their plain challenge.
    let resp = send(&app, "GET", "/_rustybin/config", &[], None).await;
    let challenge = resp.headers()["www-authenticate"]
        .to_str()
        .unwrap_or("")
        .to_string();
    assert!(!challenge.contains("resource_metadata"), "{challenge}");

    let prm = body_json(send(&app, "GET", PRM_PATH, &[], None).await).await;
    assert_eq!(prm["resource"], format!("{BASE}{MANAGE_PATH}"));
    assert_eq!(
        prm["authorization_servers"],
        json!([format!("{BASE}{ISSUER_PATH}")])
    );
    assert_eq!(prm["scopes_supported"], json!(SCOPES));

    for path in [AS_METADATA_PATH, OIDC_METADATA_PATH] {
        let meta = body_json(send(&app, "GET", path, &[], None).await).await;
        assert_eq!(meta["issuer"], format!("{BASE}{ISSUER_PATH}"));
        assert_eq!(
            meta["registration_endpoint"],
            format!("{BASE}{REGISTER_PATH}")
        );
        assert_eq!(meta["code_challenge_methods_supported"], json!(["S256"]));
        assert_eq!(
            meta["token_endpoint_auth_methods_supported"],
            json!(["none"])
        );
    }
}

#[tokio::test]
async fn full_flow_with_a_read_only_grant() {
    let app = app(token_config());
    let (client_id, id) = start(&app, None).await;

    // The consent API needs control-plane credentials.
    let resp = send(&app, "GET", &format!("{REQUESTS_PREFIX}{id}"), &[], None).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let bearer = format!("Bearer {ADMIN}");
    let resp = send(
        &app,
        "GET",
        &format!("{REQUESTS_PREFIX}{id}"),
        &[("authorization", bearer.as_str())],
        None,
    )
    .await;
    let req = body_json(resp).await;
    assert_eq!(req["client_name"], "Test agent");
    assert_eq!(req["redirect_kind"], "loopback");
    assert_eq!(
        req["scopes_requested"],
        json!(["admin", "console", "inspector", "harness"])
    );
    assert_eq!(
        req["scopes_grantable"],
        json!(["admin", "console", "inspector", "harness"])
    );

    let (status, ans) = answer(
        &app,
        &id,
        ADMIN,
        json!({ "decision": "approve", "scope": "console" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ans}");
    let to = ans["redirect_to"].as_str().expect("redirect_to");
    assert!(to.starts_with("http://127.0.0.1:40001/callback?"), "{to}");
    assert_eq!(param(to, "state").as_deref(), Some("st-123"));
    assert_eq!(param(to, "iss"), Some(format!("{BASE}{ISSUER_PATH}")));
    let code = param(to, "code").expect("code");

    // Answered once only.
    let (status, _) = answer(&app, &id, ADMIN, json!({ "decision": "approve" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // PKCE: the wrong verifier is refused (and burns the code).
    let (status, err) = exchange(&app, &client_id, &code, &"x".repeat(43)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"], "invalid_grant");
    let (status, _) = exchange(&app, &client_id, &code, VERIFIER).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "codes are single use");

    let (_, tokens) = sign_in(&app, ADMIN, Some("console")).await;
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(tokens["scope"], "console");
    let access = tokens["access_token"].as_str().expect("access");
    let names = tool_names(&app, access).await;
    assert!(names.contains(&"list_mocks".to_string()));
    assert!(!names.contains(&"create_mock".to_string()), "{names:?}");

    // The token opens /_rustybin/mcp only.
    let bearer = format!("Bearer {access}");
    let resp = send(
        &app,
        "GET",
        "/_rustybin/config",
        &[("authorization", bearer.as_str())],
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let resp = send(
        &app,
        "POST",
        "/health/toggle",
        &[("authorization", bearer.as_str())],
        None,
    )
    .await;
    assert_ne!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn admin_scope_is_admin_on_the_management_server() {
    let app = app(token_config());
    let (_, tokens) = sign_in(&app, ADMIN, None).await;
    assert_eq!(tokens["scope"], "admin console inspector harness");
    let access = tokens["access_token"].as_str().expect("access");
    let names = tool_names(&app, access).await;
    for n in [
        "create_mock",
        "apply_config",
        "set_health",
        "clear_requests",
    ] {
        assert!(names.contains(&n.to_string()), "{n} missing: {names:?}");
    }
    let resp = mcp(
        &app,
        Some(access),
        "tools/call",
        json!({ "name": "set_health", "arguments": { "healthy": true } }),
    )
    .await;
    let v = body_json(resp).await;
    assert_eq!(v["result"]["isError"], false, "{v}");
}

#[tokio::test]
async fn refresh_rotates_and_never_widens() {
    let app = app(token_config());
    let (client_id, tokens) = sign_in(&app, ADMIN, Some("console inspector")).await;
    let refresh = tokens["refresh_token"].as_str().expect("refresh");
    let form = |extra: &[(&str, &str)]| {
        let mut f = form_urlencoded::Serializer::new(String::new());
        f.append_pair("grant_type", "refresh_token")
            .append_pair("client_id", &client_id)
            .append_pair("refresh_token", refresh);
        for (k, v) in extra {
            f.append_pair(k, v);
        }
        f.finish()
    };
    let resp = send(
        &app,
        "POST",
        TOKEN_PATH,
        &[],
        Some(("application/x-www-form-urlencoded", form(&[]))),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["cache-control"], "no-store");
    let fresh = body_json(resp).await;
    assert_eq!(fresh["scope"], "console inspector");
    assert!(
        !tool_names(&app, fresh["access_token"].as_str().expect("a"))
            .await
            .is_empty()
    );

    let resp = send(
        &app,
        "POST",
        TOKEN_PATH,
        &[],
        Some((
            "application/x-www-form-urlencoded",
            form(&[("scope", "admin")]),
        )),
    )
    .await;
    assert_eq!(body_json(resp).await["error"], "invalid_scope");

    // An access token is not a refresh token.
    let mut f = form_urlencoded::Serializer::new(String::new());
    f.append_pair("grant_type", "refresh_token")
        .append_pair("client_id", &client_id)
        .append_pair("refresh_token", tokens["access_token"].as_str().expect("a"));
    let resp = send(
        &app,
        "POST",
        TOKEN_PATH,
        &[],
        Some(("application/x-www-form-urlencoded", f.finish())),
    )
    .await;
    assert_eq!(body_json(resp).await["error"], "invalid_grant");
}

#[tokio::test]
async fn the_approver_caps_the_grant() {
    let app = app(jwt_config());
    let console = control_jwt(Some("console"));
    let (_, id) = start(&app, None).await;
    let bearer = format!("Bearer {console}");
    let req = body_json(
        send(
            &app,
            "GET",
            &format!("{REQUESTS_PREFIX}{id}"),
            &[("authorization", bearer.as_str())],
            None,
        )
        .await,
    )
    .await;
    assert_eq!(req["scopes_grantable"], json!(["console", "inspector"]));
    let (status, _) = answer(
        &app,
        &id,
        &console,
        json!({ "decision": "approve", "scope": "admin" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Without a choice it grants what it may.
    let (_, tokens) = sign_in(&app, &console, None).await;
    assert_eq!(tokens["scope"], "console inspector");
}

#[tokio::test]
async fn deny_sends_access_denied() {
    let app = app(token_config());
    let (_, id) = start(&app, None).await;
    let (status, ans) = answer(&app, &id, ADMIN, json!({ "decision": "deny" })).await;
    assert_eq!(status, StatusCode::OK);
    let to = ans["redirect_to"].as_str().expect("redirect_to");
    assert_eq!(param(to, "error").as_deref(), Some("access_denied"));
    assert_eq!(param(to, "state").as_deref(), Some("st-123"));
    let (status, _) = answer(&app, &id, ADMIN, json!({ "decision": "approve" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn redirect_uri_rules() {
    let app = app(token_config());
    for bad in [
        "http://evil.example/cb",
        "javascript:alert(1)",
        "data:text/html,x",
        "https://app.example/cb#frag",
        "/relative",
    ] {
        let (status, v) = register(&app, &[bad]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(v["error"], "invalid_redirect_uri", "{bad}");
    }
    for good in [
        "http://localhost:8765/cb",
        "http://[::1]/cb",
        "https://vscode.dev/redirect",
        "cursor://anysphere.cursor-retrieval/oauth/callback",
    ] {
        let (status, _) = register(&app, &[good]).await;
        assert_eq!(status, StatusCode::CREATED, "{good}");
    }
    let (status, _) = register(&app, &[REDIRECT, REDIRECT, REDIRECT, REDIRECT]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, reg) = register(&app, &[REDIRECT]).await;
    let id = reg["client_id"].as_str().expect("id");
    // Not registered: a page, never a redirect.
    let resp = send(
        &app,
        "GET",
        &authorize_url(id, "http://127.0.0.1:1/other", None),
        &[],
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(location(&resp).is_empty());
    // A tampered client id is unknown.
    let tampered = format!("{}x", &id[..id.len() - 1]);
    let resp = send(
        &app,
        "GET",
        &authorize_url(&tampered, REDIRECT, None),
        &[],
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(body_string(resp).await.contains("Unknown client"));
    // A client id from another key (another instance) is unknown too.
    let other = app_with_other_key();
    let resp = send(&other, "GET", &authorize_url(id, REDIRECT, None), &[], None).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

fn app_with_other_key() -> Router {
    let mut c = token_config();
    c.control_oauth_key = OAuthKey::from_settings(Some(&"k".repeat(40)), None);
    app(c)
}

#[tokio::test]
async fn request_errors_go_back_to_the_client() {
    let app = app(token_config());
    let (_, reg) = register(&app, &[REDIRECT]).await;
    let id = reg["client_id"].as_str().expect("id");
    let no_pkce = format!(
        "{AUTHORIZE_PATH}?response_type=code&client_id={id}&state=s1&redirect_uri={}",
        form_urlencoded::byte_serialize(REDIRECT.as_bytes()).collect::<String>()
    );
    let resp = send(&app, "GET", &no_pkce, &[], None).await;
    assert_eq!(resp.status(), StatusCode::FOUND);
    let loc = location(&resp);
    assert!(loc.starts_with(REDIRECT), "{loc}");
    assert_eq!(param(&loc, "error").as_deref(), Some("invalid_request"));
    assert_eq!(param(&loc, "state").as_deref(), Some("s1"));

    let resp = send(
        &app,
        "GET",
        &authorize_url(id, REDIRECT, Some("root")),
        &[],
        None,
    )
    .await;
    assert_eq!(
        param(&location(&resp), "error").as_deref(),
        Some("invalid_scope")
    );

    // Not to an https redirect URI: anyone can register one, so an error
    // there would be an open redirect. A page instead.
    let web = "https://app.example/cb";
    let (_, reg) = register(&app, &[web]).await;
    let id = reg["client_id"].as_str().expect("id");
    let resp = send(
        &app,
        "GET",
        &authorize_url(id, web, Some("root")),
        &[],
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(location(&resp).is_empty());
}

#[tokio::test]
async fn discovery_needs_no_api_key() {
    let mut c = token_config();
    c.require_key = true;
    let app = app(c);
    for path in [PRM_PATH, AS_METADATA_PATH] {
        let resp = send(&app, "GET", path, &[], None).await;
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
    }
}

#[tokio::test]
async fn tokens_are_bound_to_the_key_and_the_host() {
    let app = app(token_config());
    let (_, tokens) = sign_in(&app, ADMIN, Some("console")).await;
    let access = tokens["access_token"].as_str().expect("access");
    // Another instance (another key) refuses it, with a challenge.
    let resp = mcp(&app_with_other_key(), Some(access), "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(resp.headers()["www-authenticate"]
        .to_str()
        .unwrap_or("")
        .contains("resource_metadata"));
    // Another host is another resource.
    let bearer = format!("Bearer {access}");
    let req = Request::builder()
        .method("POST")
        .uri(MANAGE_PATH)
        .header("host", "other.test")
        .header("authorization", bearer)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .body(Body::from(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }).to_string(),
        ))
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(resp).await["reason"], "invalid_audience");
    // The admin token still works as before.
    assert!(!tool_names(&app, ADMIN).await.is_empty());
}

#[tokio::test]
async fn external_issuer() {
    let mut c = jwt_config();
    c.control_oauth_issuer = Some("https://auth.example".into());
    assert_eq!(mode(&c), "external");
    let app = app(c);
    let prm = body_json(send(&app, "GET", PRM_PATH, &[], None).await).await;
    assert_eq!(
        prm["authorization_servers"],
        json!(["https://auth.example"])
    );
    assert_eq!(
        send(&app, "GET", AS_METADATA_PATH, &[], None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let (status, _) = register(&app, &[REDIRECT]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The challenge still points at the metadata; control JWTs keep working.
    let resp = mcp(&app, None, "tools/list", json!({})).await;
    assert!(resp.headers()["www-authenticate"]
        .to_str()
        .unwrap_or("")
        .contains("resource_metadata"));
    assert!(!tool_names(&app, &control_jwt(Some("console")))
        .await
        .is_empty());
}

#[test]
fn key_sources_and_debug() {
    assert_eq!(
        OAuthKey::from_settings(None, None).source(),
        KeySource::Random
    );
    assert_eq!(
        OAuthKey::from_settings(None, Some("t")).source(),
        KeySource::AdminToken
    );
    let k = OAuthKey::from_settings(Some("s"), Some("t"));
    assert_eq!(k.source(), KeySource::Setting);
    assert!(format!("{k:?}").contains("redacted"));
    // Derived keys are stable across restarts.
    let a = OAuthKey::from_settings(None, Some("t"));
    let b = OAuthKey::from_settings(None, Some("t"));
    assert_eq!(a.mac("x", b"y"), b.mac("x", b"y"));
}

#[test]
fn scope_parsing() {
    assert_eq!(
        parse_scopes(None),
        Some(vec!["admin", "console", "inspector", "harness"])
    );
    assert_eq!(
        parse_scopes(Some("inspector console")),
        Some(vec!["console", "inspector"])
    );
    assert_eq!(parse_scopes(Some("admin root")), None);
    assert!(is_consent_path("/_rustybin/oauth/requests/abc"));
    assert!(!is_consent_path("/_rustybin/oauth/requests/"));
    assert!(!is_consent_path("/_rustybin/oauth/requests/a/b"));
}

#[tokio::test]
async fn a_revoked_agent_loses_its_tokens_until_restored() {
    let app = app(token_config());
    let (client_id, tokens) = sign_in(&app, ADMIN, None).await;
    let access = tokens["access_token"].as_str().expect("access").to_string();
    let refresh = tokens["refresh_token"]
        .as_str()
        .expect("refresh")
        .to_string();
    assert!(!tool_names(&app, &access).await.is_empty());
    let admin = format!("Bearer {ADMIN}");
    let list = body_json(
        send(
            &app,
            "GET",
            "/_rustybin/agents",
            &[("authorization", &admin)],
            None,
        )
        .await,
    )
    .await;
    let agent = list["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["credential"]["kind"] == "oauth")
        .expect("the OAuth agent");
    assert!(agent["credential"]["sub"]
        .as_str()
        .expect("sub")
        .starts_with("client:"));
    let id = agent["id"].as_str().expect("id").to_string();
    let resp = send(
        &app,
        "POST",
        &format!("/_rustybin/agents/{id}/revoke"),
        &[("authorization", &admin)],
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // The access token and the refresh token are refused.
    let resp = mcp(&app, Some(&access), "tools/list", json!({})).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let mut f = form_urlencoded::Serializer::new(String::new());
    f.append_pair("grant_type", "refresh_token")
        .append_pair("client_id", &client_id)
        .append_pair("refresh_token", &refresh);
    let resp = send(
        &app,
        "POST",
        TOKEN_PATH,
        &[],
        Some(("application/x-www-form-urlencoded", f.finish())),
    )
    .await;
    assert_eq!(body_json(resp).await["error"], "invalid_grant");

    let resp = send(
        &app,
        "POST",
        &format!("/_rustybin/agents/{id}/restore"),
        &[("authorization", &admin)],
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(!tool_names(&app, &access).await.is_empty());
}
