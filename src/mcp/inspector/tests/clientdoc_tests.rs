//! The inspector's client ID metadata document: the route, when it can be
//! used, the check, and the choice a saved connection keeps.

use std::sync::{Arc, Mutex};

use super::*;
use crate::control_auth::ControlAuth;

const PUBLIC_HOST: &str = "rustybin.example";
const PUBLIC_BASE: &str = "https://rustybin.example";

/// An instance behind a proxy that terminates TLS (so a request can say it
/// came in over https), with a control plane that asks for credentials.
fn public_config() -> Config {
    let mut c = Config::for_tests();
    c.trust_forward = true;
    c.egress_allow = vec!["127.0.0.1".into()];
    c.egress_allow_loopback = true;
    c
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    host: &str,
    https: bool,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("host", host);
    if https {
        b = b.header("x-forwarded-proto", "https");
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
    (status, body_json(resp).await)
}

#[tokio::test]
async fn the_document_is_open_and_its_redirect_is_the_one_the_walk_sends() {
    // Open even where the control plane asks for credentials.
    let mut c = public_config();
    c.control_auth = ControlAuth::Token;
    c.admin_token = Some("admin-secret".into());
    let app = test_app_with(test_state_with(c));
    let (status, doc) = call(
        &app,
        "GET",
        clientdoc::DOCUMENT_PATH,
        PUBLIC_HOST,
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(
        doc["client_id"],
        format!("{PUBLIC_BASE}/_rustybin/oauth/inspector-client.json")
    );
    assert_eq!(doc["client_name"], clientdoc::CLIENT_NAME);
    assert_eq!(
        doc["redirect_uris"],
        json!([format!("{PUBLIC_BASE}/ui/callback")])
    );
    assert_eq!(doc["token_endpoint_auth_method"], "none");
    assert_eq!(
        doc["grant_types"],
        json!(["authorization_code", "refresh_token"])
    );
    // Nothing of the instance in it.
    let keys: Vec<&str> = doc
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "client_id",
            "client_name",
            "client_uri",
            "redirect_uris",
            "grant_types",
            "response_types",
            "token_endpoint_auth_method"
        ]
    );

    // What the console is told: the same document, and the redirect URI the
    // walk sends is the document's, byte for byte.
    let app = test_app_with(test_state_with(public_config()));
    let (status, v) = call(&app, "GET", SERVERS_PATH, PUBLIC_HOST, true, None).await;
    assert_eq!(status, StatusCode::OK);
    let cd = &v["client_document"];
    assert_eq!(cd["usable"], true, "{cd}");
    assert_eq!(cd["reason"], Value::Null);
    assert_eq!(cd["url"], doc["client_id"]);
    assert_eq!(cd["document"], doc);
    assert_eq!(
        cd["redirect_uri"].as_str().map(str::as_bytes),
        doc["redirect_uris"][0].as_str().map(str::as_bytes)
    );
    assert_eq!(cd["ways"], json!(clientdoc::CLIENT_WAYS));
}

#[tokio::test]
async fn a_loopback_or_plain_http_instance_says_why_its_document_cannot_be_used() {
    let app = test_app_with(test_state_with(public_config()));
    let (_, v) = call(&app, "GET", SERVERS_PATH, "127.0.0.1:8080", false, None).await;
    let cd = &v["client_document"];
    assert_eq!(cd["usable"], false);
    assert_eq!(
        cd["url"],
        "http://127.0.0.1:8080/_rustybin/oauth/inspector-client.json"
    );
    assert!(
        cd["reason"]
            .as_str()
            .is_some_and(|r| r.contains("loopback")),
        "{cd}"
    );
    for host in ["localhost:8080", "[::1]:8080", "app.localhost"] {
        let (_, v) = call(&app, "GET", SERVERS_PATH, host, true, None).await;
        assert_eq!(v["client_document"]["usable"], false, "{host}");
    }
    let (_, v) = call(&app, "GET", SERVERS_PATH, PUBLIC_HOST, false, None).await;
    let cd = &v["client_document"];
    assert_eq!(cd["usable"], false);
    assert!(
        cd["reason"].as_str().is_some_and(|r| r.contains("https")),
        "{cd}"
    );
    // The document is served all the same: a person can read it.
    let (status, doc) = call(
        &app,
        "GET",
        clientdoc::DOCUMENT_PATH,
        "127.0.0.1:8080",
        false,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["redirect_uris"][0], "http://127.0.0.1:8080/ui/callback");
}

/// An MCP server behind OAuth on a free loopback port, with its
/// authorization server: every request's path and query is logged.
/// `supported`: the metadata says client ID metadata documents are;
/// `accept`: the authorization endpoint takes a URL as the client.
async fn oauth_server(supported: bool, accept: bool) -> (String, Arc<Mutex<Vec<String>>>) {
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = log.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let own = base.clone();
    // A fallback, not routes: the catalogue test reads every `.route` literal.
    let router = Router::new().fallback(move |uri: axum::http::Uri| {
        let own = own.clone();
        let seen = seen.clone();
        async move {
            seen.lock().expect("log").push(uri.to_string());
            match uri.path() {
                "/mcp" => (
                    StatusCode::UNAUTHORIZED,
                    [(
                        "www-authenticate",
                        format!("Bearer resource_metadata=\"{own}/.well-known/oauth-protected-resource/mcp\""),
                    )],
                    "",
                )
                    .into_response(),
                "/.well-known/oauth-protected-resource/mcp" => axum::Json(json!({
                    "resource": format!("{own}/mcp"),
                    "authorization_servers": [format!("{own}/as")],
                    "scopes_supported": ["mcp:tools"],
                }))
                .into_response(),
                "/.well-known/oauth-authorization-server/as" => axum::Json(json!({
                    "issuer": format!("{own}/as"),
                    "authorization_endpoint": format!("{own}/as/authorize"),
                    "token_endpoint": format!("{own}/as/token"),
                    "registration_endpoint": format!("{own}/as/register"),
                    "client_id_metadata_document_supported": supported,
                }))
                .into_response(),
                "/as/authorize" if accept => (
                    StatusCode::FOUND,
                    [("location", format!("{own}/as/login?session=abc"))],
                    "",
                )
                    .into_response(),
                "/as/authorize" => (
                    StatusCode::BAD_REQUEST,
                    axum::Json(json!({ "error": "invalid_client" })),
                )
                    .into_response(),
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }
    });
    tokio::spawn(async move { axum::serve(listener, router).await });
    (base, log)
}

fn authorize_request(log: &Mutex<Vec<String>>) -> Option<String> {
    log.lock()
        .expect("log")
        .iter()
        .find(|u| u.starts_with("/as/authorize"))
        .cloned()
}

fn query_param(url: &str, name: &str) -> Option<String> {
    form_urlencoded::parse(url.split_once('?')?.1.as_bytes())
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

async fn run_check(app: &Router, target: &str, host: &str, https: bool) -> Value {
    let (status, report) = call(
        app,
        "POST",
        CHECKS_PATH,
        host,
        https,
        Some(json!({ "target": target, "save": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    check(&report, checks::CLIENT_DOCUMENT).clone()
}

#[tokio::test]
async fn the_check_offers_the_document_and_never_registers() {
    let (server, log) = oauth_server(true, true).await;
    let app = test_app_with(test_state_with(public_config()));
    let (status, report) = call(
        &app,
        "POST",
        CHECKS_PATH,
        PUBLIC_HOST,
        true,
        Some(json!({ "target": format!("{server}/mcp"), "save": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Nobody is signed in: the connection fails, this check runs all the same.
    assert_eq!(check(&report, "connect")["status"], "fail");
    assert_eq!(check(&report, "tools_list")["status"], "skip");
    let c = check(&report, checks::CLIENT_DOCUMENT);
    assert_eq!(c["status"], "pass", "{c}");
    let sent = authorize_request(&log).expect("an authorization request");
    assert_eq!(
        query_param(&sent, "client_id").as_deref(),
        Some("https://rustybin.example/_rustybin/oauth/inspector-client.json")
    );
    // The redirect URI is the document's, byte for byte.
    let (_, doc) = call(
        &app,
        "GET",
        clientdoc::DOCUMENT_PATH,
        PUBLIC_HOST,
        true,
        None,
    )
    .await;
    assert_eq!(
        query_param(&sent, "redirect_uri")
            .as_deref()
            .map(str::as_bytes),
        doc["redirect_uris"][0].as_str().map(str::as_bytes)
    );
    assert_eq!(
        query_param(&sent, "code_challenge_method").as_deref(),
        Some("S256")
    );
    assert_eq!(
        query_param(&sent, "resource"),
        Some(format!("{server}/mcp"))
    );
    assert_eq!(query_param(&sent, "scope").as_deref(), Some("mcp:tools"));
    let seen = log.lock().expect("log").clone();
    assert!(!seen.iter().any(|u| u.contains("/register")), "{seen:?}");
    assert!(
        !seen.iter().any(|u| u.contains("/as/login")),
        "the redirect was followed: {seen:?}"
    );
}

#[tokio::test]
async fn the_check_fails_a_server_that_says_yes_and_refuses() {
    let (server, _) = oauth_server(true, false).await;
    let app = test_app_with(test_state_with(public_config()));
    let c = run_check(&app, &format!("{server}/mcp"), PUBLIC_HOST, true).await;
    assert_eq!(c["status"], "fail", "{c}");
    assert_eq!(c["details"]["status"], 400);
}

#[tokio::test]
async fn the_check_is_skipped_with_its_reason() {
    let app = test_app_with(test_state_with(public_config()));
    // A server that does not say it supports documents.
    let (server, log) = oauth_server(false, true).await;
    let c = run_check(&app, &format!("{server}/mcp"), PUBLIC_HOST, true).await;
    assert_eq!(c["status"], "skip", "{c}");
    assert!(c["message"]
        .as_str()
        .is_some_and(|m| m.contains("does not say")));
    assert!(authorize_request(&log).is_none());

    // A server that does, asked from an instance on loopback: why not, and
    // no authorization request.
    let (server, log) = oauth_server(true, true).await;
    let c = run_check(&app, &format!("{server}/mcp"), "127.0.0.1:8080", false).await;
    assert_eq!(c["status"], "skip", "{c}");
    assert!(
        c["message"]
            .as_str()
            .is_some_and(|m| m.contains("loopback")),
        "{c}"
    );
    assert!(authorize_request(&log).is_none());

    // A server of this instance without OAuth, and the mock one behind it
    // (whose authorization server does not offer documents).
    let c = run_check(&app, "/mcp", PUBLIC_HOST, true).await;
    assert_eq!(c["status"], "skip", "{c}");
    assert!(
        c["message"]
            .as_str()
            .is_some_and(|m| m.contains("RFC 9728")),
        "{c}"
    );
    let c = run_check(&app, "/mcp/protected", PUBLIC_HOST, true).await;
    assert_eq!(c["status"], "skip", "{c}");
    assert!(
        c["message"]
            .as_str()
            .is_some_and(|m| m.contains("does not say")),
        "{c}"
    );
}

#[tokio::test]
async fn a_saved_connection_keeps_the_chosen_way_and_no_token_it_was_not_given() {
    let app = test_app_with(test_state_with(Config::for_tests()));
    let put = |id: &'static str, way: Value| {
        let app = app.clone();
        async move {
            call(
                &app,
                "PUT",
                &format!("{CONNECTIONS_PATH}/{id}"),
                "rustybin.test",
                false,
                Some(json!({ "name": id, "target": "https://mcp.example/mcp",
                    "auth": { "kind": "oauth", "oauth": { "client": way, "issuer": "https://as.example" } } })),
            )
            .await
        }
    };
    for way in clientdoc::CLIENT_WAYS {
        let (status, v) = put("by-document", json!(way)).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["auth"]["oauth"]["client"], *way);
        assert_eq!(v["secrets"], json!({}), "a credential nobody gave: {v}");
    }
    let (status, v) = put("bogus", json!("carrier-pigeon")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        v["error"]
            .as_str()
            .is_some_and(|e| e.contains("auth.oauth.client")),
        "{v}"
    );
    let (status, _) = put("bogus", json!(7)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
