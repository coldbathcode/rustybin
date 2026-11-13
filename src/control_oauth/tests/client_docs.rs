//! Client ID metadata documents on the built-in authorization server: off
//! by default, the outbound policy first, what a document must be.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::response::IntoResponse;

use super::*;
use crate::control_oauth::client_docs::{self, SETTING};
use crate::state::AppState;

/// A host of client documents on a free loopback port, counting the
/// requests it gets: `/client.json` (a good one), `/moved.json` (a
/// redirect to it), `/big.json` (a good one over the size limit),
/// `/other.json` (names another URL as its client id), `/secret.json`
/// (carries a client secret), `/web.json` (a redirect URI nobody may
/// register).
async fn documents() -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let own = base.clone();
    // A fallback, not routes: the catalogue test reads every `.route` literal.
    let router = Router::new().fallback(move |uri: axum::http::Uri| {
        let own = own.clone();
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            let id = format!("{own}{}", uri.path());
            let good = json!({
                "client_id": id,
                "client_name": "Doc agent",
                "redirect_uris": [REDIRECT],
                "grant_types": ["authorization_code", "refresh_token"],
                "token_endpoint_auth_method": "none",
            });
            match uri.path() {
                "/client.json" => axum::Json(good).into_response(),
                "/moved.json" => (
                    StatusCode::FOUND,
                    [("location", format!("{own}/client.json"))],
                    "",
                )
                    .into_response(),
                "/big.json" => {
                    let mut v = good;
                    // A literal, not the constant: 6 KiB is over the 5 KiB cap.
                    v["padding"] = json!("x".repeat(6 * 1024));
                    axum::Json(v).into_response()
                }
                "/other.json" => {
                    let mut v = good;
                    v["client_id"] = json!("https://elsewhere.example/client.json");
                    axum::Json(v).into_response()
                }
                "/secret.json" => {
                    let mut v = good;
                    v["client_secret"] = json!("shh");
                    axum::Json(v).into_response()
                }
                "/web.json" => {
                    let mut v = good;
                    v["redirect_uris"] = json!(["http://attacker.example/cb"]);
                    axum::Json(v).into_response()
                }
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }
    });
    tokio::spawn(async move { axum::serve(listener, router).await });
    (base, hits)
}

/// An instance whose outbound policy names the loopback host (or not).
fn instance(allow_host: bool) -> (Router, AppState) {
    let mut c = token_config();
    if allow_host {
        c.egress_allow = vec!["127.0.0.1".into()];
    }
    c.egress_allow_loopback = true;
    let state = test_state_with(c);
    (test_app_with(state.clone()), state)
}

fn turn(state: &AppState, on: bool) {
    state
        .settings
        .update(
            BTreeMap::from([(SETTING.to_string(), json!(on))]),
            BTreeMap::new(),
            "test",
        )
        .expect("setting");
}

async fn supported(app: &Router) -> Value {
    let resp = send(app, "GET", AS_METADATA_PATH, &[], None).await;
    body_json(resp).await["client_id_metadata_document_supported"].clone()
}

async fn authorize(app: &Router, client_id: &str) -> (StatusCode, String, String) {
    let redirect = REDIRECT.replace("33418", "40001");
    let resp = send(
        app,
        "GET",
        &authorize_url(client_id, &redirect, Some("console")),
        &[],
        None,
    )
    .await;
    let status = resp.status();
    let loc = location(&resp);
    (status, loc, body_string(resp).await)
}

#[tokio::test]
async fn off_by_default_and_nothing_is_fetched() {
    let (docs, hits) = documents().await;
    let (app, state) = instance(true);
    assert!(!client_docs::on(&state.settings));
    assert_eq!(supported(&app).await, json!(false));
    let (status, _, page) = authorize(&app, &format!("{docs}/client.json")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(page.contains("oauth.client_metadata_documents"), "{page}");
    // The token endpoint knows no such client either.
    let (status, v) = exchange(&app, &format!("{docs}/client.json"), "nope", VERIFIER).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(v["error"], "invalid_client");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "a call went out while off");
    // A document from git never turns it on.
    assert!(crate::configdoc::repository_refused(SETTING));
}

#[tokio::test]
async fn on_a_document_names_the_client_and_the_sign_in_completes() {
    let (docs, hits) = documents().await;
    let (app, state) = instance(true);
    turn(&state, true);
    assert_eq!(supported(&app).await, json!(true));
    let client_id = format!("{docs}/client.json");
    let (status, loc, page) = authorize(&app, &client_id).await;
    assert_eq!(status, StatusCode::FOUND, "{page}");
    let id = loc.strip_prefix(CONSENT_PAGE).expect("consent page");

    // The consent view gets the document's host and the name as its claim.
    let bearer = format!("Bearer {ADMIN}");
    let resp = send(
        &app,
        "GET",
        &format!("{REQUESTS_PREFIX}{id}"),
        &[("authorization", bearer.as_str())],
        None,
    )
    .await;
    let v = body_json(resp).await;
    assert_eq!(v["client_name"], "Doc agent");
    assert_eq!(v["client_document"]["url"], client_id.as_str());
    assert_eq!(
        v["client_document"]["host"],
        docs.trim_start_matches("http://")
    );

    // Approved: the code goes to the document's client, and to no other.
    let (status, ans) = answer(&app, id, ADMIN, json!({ "decision": "approve" })).await;
    assert_eq!(status, StatusCode::OK, "{ans}");
    let code = param(ans["redirect_to"].as_str().expect("redirect"), "code").expect("code");
    let (status, doc_tokens) = exchange(&app, &client_id, &code, VERIFIER).await;
    assert_eq!(status, StatusCode::OK, "{doc_tokens}");
    let token = doc_tokens["access_token"].as_str().expect("token");
    assert!(!tool_names(&app, token).await.is_empty());

    // A second sign-in within the cache's life makes no second call.
    let (status, _, _) = authorize(&app, &client_id).await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // A registered client still signs in as before.
    let (_, tokens) = sign_in(&app, ADMIN, Some("console")).await;
    assert!(tokens["access_token"].is_string());

    // Off again: the document's client is no client, its refresh token too.
    turn(&state, false);
    let mut f = form_urlencoded::Serializer::new(String::new());
    f.append_pair("grant_type", "refresh_token")
        .append_pair("client_id", &client_id)
        .append_pair(
            "refresh_token",
            doc_tokens["refresh_token"].as_str().expect("refresh token"),
        );
    let resp = send(
        &app,
        "POST",
        TOKEN_PATH,
        &[],
        Some(("application/x-www-form-urlencoded", f.finish())),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_host_outside_the_allowlist_is_refused_before_any_call() {
    let (docs, hits) = documents().await;
    let (app, state) = instance(false);
    turn(&state, true);
    let (status, _, page) = authorize(&app, &format!("{docs}/client.json")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(page.contains("outbound policy"), "{page}");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "a call went out");
    // The policy's own record names the purpose.
    let refused = state
        .egress
        .decisions()
        .into_iter()
        .any(|d| d.purpose == crate::egress::Purpose::ClientMetadata && !d.allowed);
    assert!(refused);
    // The purpose has no rule of its own: only what the operator names.
    assert!(state
        .egress
        .rule(crate::egress::Purpose::ClientMetadata)
        .allow
        .is_empty());
}

#[tokio::test]
async fn a_redirect_a_large_body_and_a_foreign_client_id_are_refused() {
    let (docs, hits) = documents().await;
    let (app, state) = instance(true);
    turn(&state, true);
    for (path, why) in [
        ("/moved.json", "redirect is not followed"),
        ("/big.json", "larger than"),
        ("/other.json", "not the URL it was fetched from"),
        ("/secret.json", "client secret"),
        ("/web.json", "loopback"),
        ("/missing.json", "answered 404"),
    ] {
        let (status, _, page) = authorize(&app, &format!("{docs}{path}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert!(page.contains(why), "{path}: {page}");
    }
    // One call each: the redirect's target was never asked for.
    assert_eq!(hits.load(Ordering::SeqCst), 6);
    // A refusal is remembered for a moment: asking again is not a call.
    let (status, _, _) = authorize(&app, &format!("{docs}/other.json")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(hits.load(Ordering::SeqCst), 6);
}

/// A whole second instance on a free loopback port: its base URL.
async fn other_instance() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let app = test_app_with(test_state_with(Config::for_tests()));
    tokio::spawn(async move { axum::serve(listener, app).await });
    base
}

#[tokio::test]
async fn one_rustybin_takes_the_inspector_document_of_another() {
    use crate::mcp::inspector::clientdoc;
    // The other Rustybin, whose console's inspector signs in here.
    let other = other_instance().await;
    let (app, state) = instance(true);
    turn(&state, true);

    // What the other one's walk sends: its document's address outside the
    // control plane, and the document's own redirect URI.
    let client_id = clientdoc::client_id(&other);
    let redirect = clientdoc::redirect_uri(&other);
    assert_eq!(
        client_id,
        format!("{other}/.well-known/rustybin-inspector-client.json")
    );
    let resp = send(
        &app,
        "GET",
        &authorize_url(&client_id, &redirect, Some("console")),
        &[],
        None,
    )
    .await;
    let status = resp.status();
    let loc = location(&resp);
    let page = body_string(resp).await;
    assert_eq!(status, StatusCode::FOUND, "{page}");
    let id = loc.strip_prefix(CONSENT_PAGE).expect("consent page");

    // The consent step: the other instance's inspector, by its own claim.
    let bearer = format!("Bearer {ADMIN}");
    let resp = send(
        &app,
        "GET",
        &format!("{REQUESTS_PREFIX}{id}"),
        &[("authorization", bearer.as_str())],
        None,
    )
    .await;
    let v = body_json(resp).await;
    assert_eq!(v["client_name"], clientdoc::CLIENT_NAME);
    assert_eq!(v["client_document"]["url"], client_id.as_str());
    assert_eq!(
        v["client_document"]["host"],
        other.trim_start_matches("http://")
    );

    // The address 0.7.0 sent is under the other one's control plane: the
    // outbound policy refuses it as it always did, before any call, in its
    // own words.
    let legacy = clientdoc::legacy_client_id(&other);
    let policy = state
        .egress
        .resolve(&legacy, crate::egress::Purpose::ClientMetadata)
        .await
        .expect_err("the policy never calls a control plane");
    assert_eq!(
        policy.0,
        "the target URL points at a Rustybin control plane (/_rustybin, /ui)"
    );
    let (status, _, page) = {
        let resp = send(
            &app,
            "GET",
            &authorize_url(&legacy, &redirect, Some("console")),
            &[],
            None,
        )
        .await;
        (resp.status(), location(&resp), body_string(resp).await)
    };
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(page.contains("outbound policy"), "{page}");
    assert!(page.contains("Rustybin control plane"), "{page}");
    // The document is there all the same, for a server that may fetch it.
    let served: Value = reqwest::get(&legacy)
        .await
        .expect("the other instance")
        .json()
        .await
        .expect("json");
    assert_eq!(served["client_id"], legacy.as_str());
}

#[test]
fn a_client_id_url_follows_the_rules() {
    assert!(client_docs::check_url("https://agent.example/client.json").is_ok());
    assert!(client_docs::check_url("http://127.0.0.1:9000/client.json").is_ok());
    for bad in [
        "http://agent.example/client.json",
        "https://agent.example",
        "https://agent.example/",
        "https://agent.example/a/../client.json",
        "https://agent.example/./client.json",
        "https://agent.example/client.json#frag",
        "https://user:pw@agent.example/client.json",
        "https://agent.example/cli ent.json",
        "ftp://agent.example/client.json",
    ] {
        assert!(client_docs::check_url(bad).is_err(), "{bad}");
    }
    let long = format!(
        "https://agent.example/{}",
        "a".repeat(client_docs::MAX_URL_LEN)
    );
    assert!(client_docs::check_url(&long).is_err());
    assert!(client_docs::looks_like_document(
        "HTTPS://agent.example/c.json"
    ));
    assert!(!client_docs::looks_like_document("rbc.abc.def"));
}

#[test]
fn the_cache_is_bounded() {
    let cache = client_docs::Cache::default();
    for i in 0..(client_docs::MAX_CACHED * 3) {
        cache.put_for_tests(&format!("https://agent.example/{i}.json"));
    }
    assert_eq!(cache.len(), client_docs::MAX_CACHED);
}
