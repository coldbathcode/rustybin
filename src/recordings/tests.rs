use serde_json::{json, Value};

use super::*;
use crate::config::Config;

#[test]
fn secrets_are_named_never_kept() {
    let mut v = json!({ "user": "jane", "password": "hunter2", "nested": { "api_key": "k-1", "items": [{ "access_token": "t" }] } });
    redact_json(&mut v);
    assert_eq!(v["password"], "{{PASSWORD}}");
    assert_eq!(v["nested"]["api_key"], "{{API_KEY}}");
    assert_eq!(v["nested"]["items"][0]["access_token"], "{{ACCESS_TOKEN}}");
    assert_eq!(v["user"], "jane");
}

fn config() -> Config {
    let mut c = Config::for_tests();
    c.egress_allow = vec!["127.0.0.1".into()];
    c.egress_allow_loopback = true;
    c
}

/// A real instance: the upstream (its petstore mock) and the recorder.
async fn instance(c: Config) -> (crate::server::RunningServer, AppState, String) {
    let state = AppState::for_tests(c);
    let server = crate::server::start_with_state(state.clone())
        .await
        .expect("start");
    let base = format!("http://{}", server.http_addr);
    (server, state, base)
}

fn svc(state: &AppState, base: &str) -> Svc {
    let mut headers = HeaderMap::new();
    if state.config.admin_token.is_some() {
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer admin-secret"),
        );
    }
    headers.insert(
        "host",
        HeaderValue::from_str(base.trim_start_matches("http://")).expect("host"),
    );
    let uri: Uri = "/_rustybin/recordings".parse().expect("uri");
    Svc::from_request(state, &headers, &Extensions::new(), &uri)
}

fn start_body(name: &str, upstream: &str) -> StartBody {
    StartBody {
        name: name.into(),
        upstream: upstream.into(),
        mock: None,
    }
}

#[tokio::test]
async fn a_recording_through_the_proxy_builds_a_working_mock() {
    let (_server, state, base) = instance(config()).await;
    let upstream = format!("{base}/mock/petstore/v1");
    let rec = start(&state, "me", &base, start_body("pets", &upstream))
        .await
        .expect("start");
    assert_eq!(rec["egress"]["allowed"], true, "{rec}");
    let proxy = rec["proxy_url"].as_str().expect("proxy").to_string();
    assert!(
        !proxy.contains("pets"),
        "the proxy URL names the recording: {proxy}"
    );
    let client = reqwest::Client::new();
    // The client gets what the upstream answers, byte for byte.
    let direct = client
        .get(format!("{upstream}/pets/1"))
        .send()
        .await
        .expect("direct");
    assert_eq!(direct.status().as_u16(), 200);
    let direct = direct.text().await.expect("text");
    for path in ["/pets/1", "/pets/2", "/pets?limit=5&api_key=s3cr3t"] {
        let r = client
            .get(format!("{proxy}{path}"))
            .header("authorization", "Bearer real-credential")
            .header("cookie", "session=abc")
            .send()
            .await
            .expect("send");
        assert_eq!(r.status().as_u16(), 200, "{path}");
        let text = r.text().await.expect("text");
        if path == "/pets/1" {
            assert_eq!(text, direct);
        }
    }
    let got = get_one(&state, "me", &base, "pets").expect("get");
    assert_eq!(got["exchanges"], 3);
    let kept = got["recorded"].to_string();
    for secret in ["real-credential", "session=abc", "s3cr3t"] {
        assert!(!kept.contains(secret), "{secret} was kept: {kept}");
    }
    assert!(kept.contains("{{API_KEY}}"), "{kept}");
    // Build: /pets/1 and /pets/2 are one operation, the mock answers.
    let dry = build(
        &state,
        svc(&state, &base),
        "me",
        "pets",
        BuildBody {
            mock: None,
            dry_run: true,
        },
    )
    .await
    .expect("dry");
    assert!(
        dry["spec"]["paths"]["/pets/{petId}"]["get"].is_object(),
        "{dry}"
    );
    assert!(state.mocks.get("pets").is_none(), "a dry run made the mock");
    let built = build(
        &state,
        svc(&state, &base),
        "me",
        "pets",
        BuildBody::default(),
    )
    .await
    .expect("build");
    assert_eq!(
        built["operations"].as_array().map(Vec::len),
        Some(2),
        "{built}"
    );
    let mock = client
        .get(format!("{base}/mock/pets/pets/7"))
        .send()
        .await
        .expect("mock");
    assert_eq!(mock.status().as_u16(), 200);
    let body: Value = mock.json().await.expect("json");
    assert!(body.get("id").is_some(), "{body}");
    // Built again under the same name: refused, the mock untouched.
    let again = build(
        &state,
        svc(&state, &base),
        "me",
        "pets",
        BuildBody::default(),
    )
    .await;
    assert!(again.is_err());
    // Stopped: the proxy refuses; deleted: gone, the mock stays.
    stop(&state, "me", &base, "pets").expect("stop");
    let r = client
        .get(format!("{proxy}/pets/1"))
        .send()
        .await
        .expect("send");
    assert_eq!(r.status().as_u16(), 410);
    delete(&state, "me", "pets").expect("delete");
    assert!(state.mocks.get("pets").is_some());
    let r = client
        .get(format!("{proxy}/pets/1"))
        .send()
        .await
        .expect("send");
    assert_eq!(r.status().as_u16(), 404);
}

#[tokio::test]
async fn a_host_outside_the_policy_is_refused_and_recordings_are_bounded() {
    let mut c = Config::for_tests();
    c.egress_allow = vec![];
    let (_server, state, base) = instance(c).await;
    let rec = start(
        &state,
        "me",
        &base,
        start_body("blocked", "https://api.example.com"),
    )
    .await
    .expect("start");
    assert_eq!(rec["egress"]["allowed"], false, "{rec}");
    let proxy = rec["proxy_url"].as_str().expect("proxy").to_string();
    let r = reqwest::get(format!("{proxy}/x")).await.expect("send");
    assert_eq!(r.status().as_u16(), 403);
    let v: Value = r.json().await.expect("json");
    assert_eq!(v["purpose"], "record");
    assert_eq!(
        get_one(&state, "me", &base, "blocked").expect("get")["refused"],
        1
    );
    // Another session neither sees nor ends it.
    assert!(get_one(&state, "other", &base, "blocked").is_err());
    assert!(delete(&state, "other", "blocked").is_err());
    // Names and caps.
    assert!(start(
        &state,
        "me",
        &base,
        start_body("Bad Name", "https://a.example.com")
    )
    .await
    .is_err());
    assert!(start(
        &state,
        "me",
        &base,
        start_body("blocked", "https://a.example.com")
    )
    .await
    .is_err());
    for i in 1..MAX_RECORDINGS {
        start(
            &state,
            "me",
            &base,
            start_body(&format!("r{i}"), "https://a.example.com"),
        )
        .await
        .expect("start");
    }
    let e = start(
        &state,
        "me",
        &base,
        start_body("one-more", "https://a.example.com"),
    )
    .await
    .expect_err("full");
    assert_eq!(e.0, StatusCode::TOO_MANY_REQUESTS);
}
