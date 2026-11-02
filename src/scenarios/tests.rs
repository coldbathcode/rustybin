use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Extensions, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::test_support::{body_json, test_app_with, test_state_with};

/// A stand-in gateway: forwards to one upstream and, when told, retries a
/// 503 once, limits requests, adds a header, times out or caches GETs.
#[derive(Default)]
struct Gw {
    upstream: String,
    retry: bool,
    limit: Option<u32>,
    add: Option<(&'static str, String)>,
    timeout: Option<Duration>,
    cache: bool,
    count: AtomicU32,
    cached: Mutex<HashMap<String, (u16, bytes::Bytes)>>,
    client: reqwest::Client,
}

async fn gw(State(g): State<Arc<Gw>>, req: Request) -> Response {
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    let url = if path == "/" {
        g.upstream.clone()
    } else {
        format!("{}{path}", g.upstream)
    };
    if let Some(l) = g.limit {
        if g.count.fetch_add(1, Ordering::SeqCst) + 1 > l {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
    }
    if g.cache {
        if let Some((s, b)) = g.cached.lock().expect("lock").get(&url).cloned() {
            return (StatusCode::from_u16(s).expect("status"), b).into_response();
        }
    }
    let session = req.headers().get(SESSION_HEADER).cloned();
    let send = || {
        let mut r = g.client.get(&url);
        if let Some(s) = &session {
            r = r.header(SESSION_HEADER, s.clone());
        }
        if let Some((k, v)) = &g.add {
            r = r.header(*k, v);
        }
        if let Some(t) = g.timeout {
            r = r.timeout(t);
        }
        r.send()
    };
    let mut resp = send().await;
    if g.retry && matches!(&resp, Ok(r) if r.status() == 503) {
        resp = send().await;
    }
    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            let body = r.bytes().await.unwrap_or_default();
            if g.cache && status == 200 {
                g.cached
                    .lock()
                    .expect("lock")
                    .insert(url, (status, body.clone()));
            }
            (StatusCode::from_u16(status).expect("status"), body).into_response()
        }
        Err(e) if e.is_timeout() => StatusCode::GATEWAY_TIMEOUT.into_response(),
        Err(_) => StatusCode::BAD_GATEWAY.into_response(),
    }
}

async fn start_gw(g: Gw) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = axum::Router::new().fallback(gw).with_state(Arc::new(g));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn config() -> Config {
    let mut c = Config::for_tests();
    c.egress_allow = vec!["127.0.0.1".into()];
    c.egress_allow_loopback = true;
    c
}

/// A real instance (the gateway's upstream) and its state.
async fn instance(c: Config) -> (crate::server::RunningServer, AppState, String) {
    let state = crate::state::AppState::for_tests(c);
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
    let host = base.trim_start_matches("http://");
    headers.insert("host", HeaderValue::from_str(host).expect("host"));
    let uri: Uri = "/_rustybin/scenarios".parse().expect("uri");
    Svc::from_request(state, &headers, &Extensions::new(), &uri)
}

async fn run_of(state: &AppState, base: &str, id: &str) -> Value {
    start(state, svc(state, base), "me", id)
        .await
        .expect("start")
}

async fn check_via(state: &AppState, run: &Value, gateway: &str) -> Value {
    check(
        state,
        "me",
        run["scenario"].as_str().expect("scenario"),
        run["id"].as_str().expect("id"),
        CheckBody {
            gateway_url: gateway.to_string(),
            limit: None,
        },
    )
    .await
    .expect("check")
}

fn failed(verdict: &Value) -> Vec<String> {
    verdict["expectations"]
        .as_array()
        .expect("expectations")
        .iter()
        .filter(|e| e["ok"] == false)
        .map(|e| e["name"].as_str().unwrap_or("").to_string())
        .collect()
}

fn behaving(id: &str, upstream: &str, key: Option<&str>) -> Gw {
    let mut g = Gw {
        upstream: upstream.to_string(),
        ..Gw::default()
    };
    match id {
        "retries" => g.retry = true,
        "rate_limiting" => g.limit = Some(catalog::DEFAULT_LIMIT),
        "credential_injection" => {
            g.add = Some((catalog::KEY_HEADER, key.unwrap_or("").to_string()))
        }
        "timeouts" => g.timeout = Some(Duration::from_millis(150)),
        "caching" => g.cache = true,
        _ => {}
    }
    g
}

fn key_of(run: &Value) -> Option<String> {
    run["setup"]
        .as_array()
        .and_then(|s| {
            s.iter()
                .find_map(|l| l.as_str()?.split("X-Upstream-Key: ").nth(1))
        })
        .and_then(|rest| rest.split(' ').next())
        .map(str::to_string)
}

#[tokio::test]
async fn every_scenario_passes_through_a_gateway_that_behaves() {
    let (_server, state, base) = instance(config()).await;
    for sc in SCENARIOS {
        let run = run_of(&state, &base, sc.id).await;
        let upstream = run["upstream"].as_str().expect("upstream").to_string();
        let gateway = start_gw(behaving(sc.id, &upstream, key_of(&run).as_deref())).await;
        let v = check_via(&state, &run, &gateway).await;
        assert_eq!(v["passed"], true, "{}: {v:#}", sc.id);
        end(&state, "me", sc.id, run["id"].as_str().expect("id")).expect("end");
    }
}

#[tokio::test]
async fn a_gateway_without_the_feature_fails_with_the_expectation_named() {
    let (_server, state, base) = instance(config()).await;
    let expected: &[(&str, &str)] = &[
        ("retries", "The gateway retried the failed attempt"),
        (
            "rate_limiting",
            "The gateway refused requests over the limit with 429",
        ),
        (
            "credential_injection",
            "The upstream accepted the credential the gateway added",
        ),
        (
            "timeouts",
            "The gateway gave up before the upstream answered",
        ),
        ("caching", "Only the first request reached the upstream"),
    ];
    for (id, name) in expected {
        let run = run_of(&state, &base, id).await;
        let upstream = run["upstream"].as_str().expect("upstream").to_string();
        // A pass-through proxy, then no gateway at all (the instance itself).
        let plain = start_gw(Gw {
            upstream: upstream.clone(),
            ..Gw::default()
        })
        .await;
        for gateway in [plain.as_str(), upstream.as_str()] {
            let v = check_via(&state, &run, gateway).await;
            assert_eq!(v["passed"], false, "{id}: {v:#}");
            assert!(failed(&v).iter().any(|f| f == name), "{id}: {v:#}");
        }
        end(&state, "me", id, run["id"].as_str().expect("id")).expect("end");
    }
}

#[tokio::test]
async fn ending_a_run_removes_what_it_created_and_nothing_else() {
    let dir = tempfile::tempdir().expect("dir");
    let mut c = config();
    c.mocks_state_dir = Some(dir.path().to_string_lossy().into_owned());
    let (_server, state, base) = instance(c).await;
    let run = run_of(&state, &base, "caching").await;
    let mock = run["resources"][0]["name"]
        .as_str()
        .expect("mock")
        .to_string();
    assert!(state.mocks.get(&mock).is_some());
    // Never saved: a restart does not bring it back without its run.
    assert!(
        !std::fs::read_dir(dir.path()).expect("dir").any(|e| e
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .contains(&mock)),
        "the scenario mock was saved"
    );
    // The petstore sample and someone else's flaky counters stay.
    state.flaky.next("someone", "recover:1");
    let retries = run_of(&state, &base, "retries").await;
    let gateway = start_gw(behaving(
        "retries",
        retries["upstream"].as_str().expect("u"),
        None,
    ))
    .await;
    check_via(&state, &retries, &gateway).await;
    let out = end(&state, "me", "retries", retries["id"].as_str().expect("id")).expect("end");
    assert_eq!(out["removed"][0]["count"], 3, "{out}");
    assert_eq!(state.flaky.session_counters("someone").len(), 1);
    let out = end(&state, "me", "caching", run["id"].as_str().expect("id")).expect("end");
    assert_eq!(out["removed"][0]["name"], mock.as_str());
    assert!(state.mocks.get(&mock).is_none());
    assert!(state.mocks.get("petstore").is_some());
    // Ended means gone.
    assert!(end(&state, "me", "caching", run["id"].as_str().expect("id")).is_err());
}

fn req(method: &str, uri: &str, session: &str) -> axum::http::Request<Body> {
    axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("x-rustybin-session", session)
        .header("content-type", "application/json")
        .body(Body::empty())
        .expect("req")
}

#[tokio::test]
async fn another_session_cannot_see_or_end_a_run() {
    let app = test_app_with(test_state_with(config()));
    let resp = app
        .clone()
        .oneshot(req("POST", "/_rustybin/scenarios/retries/runs", "alice"))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let run = body_json(resp).await;
    let path = format!(
        "/_rustybin/scenarios/retries/runs/{}",
        run["id"].as_str().expect("id")
    );
    for method in ["GET", "DELETE"] {
        let resp = app
            .clone()
            .oneshot(req(method, &path, "bob"))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{method}");
    }
    let list = body_json(
        app.clone()
            .oneshot(req("GET", LIST_PATH, "bob"))
            .await
            .expect("r"),
    )
    .await;
    assert_eq!(list["runs"], json!([]));
    assert_eq!(
        list["scenarios"].as_array().map(Vec::len),
        Some(SCENARIOS.len())
    );
    let list = body_json(
        app.clone()
            .oneshot(req("GET", LIST_PATH, "alice"))
            .await
            .expect("r"),
    )
    .await;
    assert_eq!(list["runs"][0]["id"], run["id"]);
    let resp = app
        .clone()
        .oneshot(req("DELETE", &path, "alice"))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn mutations_are_admin_only_and_the_egress_policy_applies() {
    let mut c = Config::for_tests();
    c.admin_token = Some("admin-secret".into());
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    let resp = app
        .clone()
        .oneshot(req("POST", "/_rustybin/scenarios/retries/runs", "a"))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // Without the gateway's host in the egress allowlist the check is refused.
    let run = run_of(&state, "http://127.0.0.1:9", "retries").await;
    let r = check(
        &state,
        "me",
        "retries",
        run["id"].as_str().expect("id"),
        CheckBody {
            gateway_url: "http://127.0.0.1:9/gw".into(),
            limit: None,
        },
    )
    .await;
    match r {
        Err(Refused(status, msg)) => {
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert!(msg.contains("scenario"), "{msg}");
        }
        Ok(v) => panic!("checked: {v}"),
    }
    // A limit on a scenario that has none, and a query string, are refused.
    let bad = check(
        &state,
        "me",
        "retries",
        run["id"].as_str().expect("id"),
        CheckBody {
            gateway_url: "http://127.0.0.1:9/gw".into(),
            limit: Some(3),
        },
    )
    .await;
    assert!(matches!(bad, Err(Refused(StatusCode::BAD_REQUEST, _))));
}

#[tokio::test]
async fn runs_are_bounded() {
    let state = test_state_with(config());
    for _ in 0..MAX_RUNS {
        run_of(&state, "http://127.0.0.1:9", "retries").await;
    }
    let r = start(&state, svc(&state, "http://127.0.0.1:9"), "me", "retries").await;
    assert!(matches!(r, Err(Refused(StatusCode::TOO_MANY_REQUESTS, _))));
    assert_eq!(state.scenarios.len(), MAX_RUNS);
    let r = start(&state, svc(&state, "http://127.0.0.1:9"), "me", "nope").await;
    assert!(matches!(r, Err(Refused(StatusCode::NOT_FOUND, _))));
}
