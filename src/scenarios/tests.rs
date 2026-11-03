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

/// A stand-in gateway: forwards to one upstream (request and response
/// headers included) and, when told, retries a 503 once, limits requests,
/// adds a header, times out, caches GETs, opens a circuit breaker, refuses
/// large bodies, answers CORS or transforms headers.
#[derive(Default)]
struct Gw {
    upstream: String,
    retry: bool,
    limit: Option<u32>,
    add: Option<(&'static str, String)>,
    timeout: Option<Duration>,
    cache: bool,
    /// Open after this many failures in a row, for this long.
    breaker: Option<(u32, Duration)>,
    /// Refuse bodies larger than this with 413.
    max_body: Option<usize>,
    /// CORS for one origin.
    cors: Option<Cors>,
    /// A request header removed before the upstream.
    remove: Option<&'static str>,
    /// A response header added on the way back.
    add_response: Option<(&'static str, &'static str)>,
    count: AtomicU32,
    cached: Mutex<HashMap<String, (u16, bytes::Bytes)>>,
    circuit: Mutex<Circuit>,
    client: reqwest::Client,
}

/// The gateway's CORS policy: the one origin it allows; preflights it
/// answers itself, or lets through to the upstream.
struct Cors {
    origin: String,
    answer_preflights: bool,
}

/// Headers a proxy never forwards as they are.
const HOP: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
];

/// The breaker's state: failures in a row, when it opened, and whether a
/// trial request is out.
#[derive(Default)]
struct Circuit {
    failures: u32,
    opened: Option<std::time::Instant>,
    trial: bool,
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
    if let Some((_, half_open)) = g.breaker {
        let mut c = g.circuit.lock().expect("lock");
        if let Some(t) = c.opened {
            // Open (or a trial out): answer on its own.
            if c.trial || t.elapsed() < half_open {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            // Half open: this request is the trial.
            c.trial = true;
        }
    }
    let method = req.method().clone();
    let origin = req
        .headers()
        .get("origin")
        .and_then(|o| o.to_str().ok())
        .map(str::to_string);
    let allowed = |c: &Cors| origin.as_deref() == Some(c.origin.as_str());
    let preflight = method == axum::http::Method::OPTIONS
        && req.headers().contains_key("access-control-request-method");
    if let Some(c) = g.cors.as_ref().filter(|c| preflight && c.answer_preflights) {
        // Answered here: CORS headers for the allowed origin only.
        let mut resp = StatusCode::NO_CONTENT.into_response();
        if allowed(c) {
            let h = resp.headers_mut();
            h.insert(
                "access-control-allow-origin",
                HeaderValue::from_str(&c.origin).expect("origin"),
            );
            h.insert(
                "access-control-allow-methods",
                HeaderValue::from_static("GET, POST"),
            );
        }
        return resp;
    }
    let mut headers = req.headers().clone();
    for h in HOP {
        headers.remove(*h);
    }
    if let Some(r) = g.remove {
        headers.remove(r);
    }
    let body = axum::body::to_bytes(req.into_body(), 4 << 20)
        .await
        .unwrap_or_default();
    if g.max_body.is_some_and(|m| body.len() > m) {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let send = || {
        let mut r = g
            .client
            .request(method.clone(), &url)
            .headers(headers.clone())
            .body(body.clone());
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
    if let Some((threshold, _)) = g.breaker {
        let mut c = g.circuit.lock().expect("lock");
        if matches!(&resp, Ok(r) if r.status().is_success()) {
            *c = Circuit::default();
        } else if c.trial {
            // The trial failed: open again.
            c.trial = false;
            c.opened = Some(std::time::Instant::now());
        } else {
            c.failures += 1;
            if c.failures >= threshold {
                c.opened = Some(std::time::Instant::now());
            }
        }
    }
    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            let mut back = r.headers().clone();
            for h in HOP {
                back.remove(*h);
            }
            let body = r.bytes().await.unwrap_or_default();
            if g.cache && status == 200 {
                g.cached
                    .lock()
                    .expect("lock")
                    .insert(url, (status, body.clone()));
            }
            if let Some(c) = g.cors.as_ref().filter(|c| c.answer_preflights) {
                // The gateway's CORS answer, never the upstream's.
                back.remove("access-control-allow-origin");
                if allowed(c) {
                    back.insert(
                        "access-control-allow-origin",
                        HeaderValue::from_str(&c.origin).expect("origin"),
                    );
                }
            }
            if let Some((k, v)) = g.add_response {
                back.insert(k, HeaderValue::from_static(v));
            }
            let mut resp = (StatusCode::from_u16(status).expect("status"), body).into_response();
            resp.headers_mut().extend(back);
            resp
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

/// The stand-in gateway's half-open time and body limit.
const HALF_OPEN_MS: u64 = 300;
const SIZE_LIMIT: u64 = 1024;

async fn check_via(state: &AppState, run: &Value, gateway: &str) -> Value {
    let id = run["scenario"].as_str().expect("scenario");
    let body = match id {
        "circuit_breaker" => CheckBody::new(gateway).with("half_open_ms", HALF_OPEN_MS),
        "request_size_limit" => CheckBody::new(gateway).with("size_limit", SIZE_LIMIT),
        _ => CheckBody::new(gateway),
    };
    check_with(state, run, body).await
}

async fn check_with(state: &AppState, run: &Value, body: CheckBody) -> Value {
    let id = run["scenario"].as_str().expect("scenario");
    check(state, "me", id, run["id"].as_str().expect("id"), body)
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
        "circuit_breaker" => g.breaker = Some((3, Duration::from_millis(HALF_OPEN_MS))),
        "request_size_limit" => g.max_body = Some(SIZE_LIMIT as usize),
        "cors" => {
            g.cors = Some(Cors {
                origin: catalog::DEFAULT_ALLOWED_ORIGIN.into(),
                answer_preflights: true,
            })
        }
        "header_transformation" => {
            g.add = Some((catalog::DEFAULT_ADD_REQUEST_HEADER, "added".into()));
            g.remove = Some(catalog::DEFAULT_REMOVE_REQUEST_HEADER);
            g.add_response = Some((catalog::DEFAULT_ADD_RESPONSE_HEADER, "yes"));
        }
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
        (
            "circuit_breaker",
            "The gateway opened the circuit after upstream failures",
        ),
        (
            "request_size_limit",
            "The body over the limit never reached the upstream",
        ),
        (
            "cors",
            "The preflight from the allowed origin got CORS headers",
        ),
        (
            "header_transformation",
            "The added request header reached the upstream",
        ),
    ];
    assert_eq!(expected.len(), SCENARIOS.len(), "a scenario without a case");
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
        CheckBody::new("http://127.0.0.1:9/gw"),
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
        CheckBody::new("http://127.0.0.1:9/gw").with("limit", 3),
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

#[tokio::test]
async fn a_run_cannot_be_guessed_nor_its_check_skewed_by_forged_sessions() {
    let (_server, state, base) = instance(config()).await;
    let run = run_of(&state, &base, "caching").await;
    let id = run["id"].as_str().expect("id").to_string();
    assert_eq!(id.len(), 24, "96-bit run ids: {id}");
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    // Requests sent to the upstream under the sessions a check would use
    // when the run id is known: they must not count.
    let client = reqwest::Client::new();
    for n in 0..3 {
        for k in 1..3 {
            let forged = format!("scn-{id}-{k}-{n}");
            let _ = client
                .get(format!(
                    "{upstream}{}",
                    catalog::find("caching").expect("caching").request_path
                ))
                .header(SESSION_HEADER, forged)
                .send()
                .await;
        }
    }
    let gateway = start_gw(behaving("caching", &upstream, None)).await;
    let v = check_via(&state, &run, &gateway).await;
    assert_eq!(v["passed"], true, "{v:#}");
    // The secret in the sessions is never shown.
    let shown = get_run(&state, "me", "caching", &id, true).expect("run");
    let secret = state
        .scenarios
        .runs()
        .iter()
        .find(|r| r.id == id)
        .map(|r| r.secret.clone())
        .expect("run");
    assert!(!shown.to_string().contains(&secret), "{shown}");
    assert!(!v.to_string().contains(&secret), "{v}");
    end(&state, "me", "caching", &id).expect("end");
}

#[tokio::test]
async fn the_injected_credential_is_shown_to_admins_only() {
    let (_server, state, base) = instance(config()).await;
    let run = run_of(&state, &base, "credential_injection").await;
    let key = key_of(&run).expect("the start answer holds the key");
    let id = run["id"].as_str().expect("id");
    let admin = get_run(&state, "me", "credential_injection", id, true).expect("run");
    assert!(admin.to_string().contains(&key));
    assert_eq!(admin["key_hidden"], false);
    // Someone else behind the same address (the same session key) without
    // admin rights: the run, never its key.
    let other = get_run(&state, "me", "credential_injection", id, false).expect("run");
    assert!(!other.to_string().contains(&key), "{other}");
    assert_eq!(other["key_hidden"], true);
    let listed = list(&state, "me", false);
    assert!(!listed.to_string().contains(&key), "{listed}");
    assert!(list(&state, "me", true).to_string().contains(&key));
    end(&state, "me", "credential_injection", id).expect("end");
}

#[tokio::test]
async fn test_parameters_are_bounded_and_belong_to_their_scenario() {
    let state = test_state_with(config());
    let breaker = run_of(&state, "http://127.0.0.1:9", "circuit_breaker").await;
    let size = run_of(&state, "http://127.0.0.1:9", "request_size_limit").await;
    let b = breaker["id"].as_str().expect("id").to_string();
    let s = size["id"].as_str().expect("id").to_string();
    let refused = |r: Result<Value, Refused>| match r {
        Err(Refused(StatusCode::BAD_REQUEST, m)) => m,
        Err(Refused(status, m)) => panic!("refused with {status}: {m}"),
        Ok(v) => panic!("checked: {v}"),
    };
    let body = |half_open_ms: Option<u64>, size_limit: Option<u64>| {
        let mut b = CheckBody::new("http://127.0.0.1:9/gw");
        if let Some(v) = half_open_ms {
            b = b.with("half_open_ms", v);
        }
        if let Some(v) = size_limit {
            b = b.with("size_limit", v);
        }
        b
    };
    for v in [0, 99, 60_001] {
        let m = refused(check(&state, "me", "circuit_breaker", &b, body(Some(v), None)).await);
        assert!(m.contains("half_open_ms: 100 to 60000"), "{m}");
    }
    for v in [0, 15, catalog::MAX_SIZE_LIMIT + 1] {
        let m = refused(check(&state, "me", "request_size_limit", &s, body(None, Some(v))).await);
        assert!(m.contains("size_limit: 16 to"), "{m}");
    }
    let m = refused(check(&state, "me", "circuit_breaker", &b, body(None, Some(1024))).await);
    assert!(
        m.contains("size_limit applies to request_size_limit only"),
        "{m}"
    );
    let m = refused(
        check(
            &state,
            "me",
            "request_size_limit",
            &s,
            body(Some(500), None),
        )
        .await,
    );
    assert!(
        m.contains("half_open_ms applies to circuit_breaker only"),
        "{m}"
    );
    // A refused check is not counted.
    let shown = get_run(&state, "me", "circuit_breaker", &b, true).expect("run");
    assert_eq!(shown["checks"], 0, "{shown}");
    // The listing and the run describe each parameter with its bounds.
    let listed = list(&state, "me", true);
    let p = listed["scenarios"]
        .as_array()
        .expect("scenarios")
        .iter()
        .find(|s| s["id"] == "circuit_breaker")
        .map(|s| s["parameters"][0].clone())
        .expect("circuit_breaker");
    assert_eq!(p["name"], "half_open_ms");
    assert_eq!(p["minimum"], 100);
    assert_eq!(p["maximum"], 60_000);
    assert_eq!(shown["parameters"][0]["name"], "half_open_ms", "{shown}");
}

#[tokio::test]
async fn a_breaker_check_waits_the_half_open_time_and_names_the_trial() {
    let (_server, state, base) = instance(config()).await;
    let run = run_of(&state, &base, "circuit_breaker").await;
    assert!(
        run["upstream"]
            .as_str()
            .expect("upstream")
            .ends_with("/flaky/after/0"),
        "{run}"
    );
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    let gateway = start_gw(behaving("circuit_breaker", &upstream, None)).await;
    let t = std::time::Instant::now();
    let v = check_via(&state, &run, &gateway).await;
    assert!(t.elapsed() >= Duration::from_millis(HALF_OPEN_MS), "{v:#}");
    assert_eq!(v["passed"], true, "{v:#}");
    let requests = v["requests"].as_array().expect("requests");
    // Three failures reach the upstream, the fourth is answered by the gateway,
    // the trial reaches the upstream again.
    let reached: Vec<bool> = requests
        .iter()
        .map(|r| !r["upstream"].as_array().expect("u").is_empty())
        .collect();
    assert_eq!(reached, [true, true, true, false, true], "{v:#}");
    assert_eq!(requests[4]["trial"], true, "{v:#}");
    let out = end(
        &state,
        "me",
        "circuit_breaker",
        run["id"].as_str().expect("id"),
    )
    .expect("end");
    assert_eq!(out["removed"][0]["kind"], "flaky_counters", "{out}");
}

fn label<'a>(v: &'a Value, label: &str) -> &'a Value {
    v["requests"]
        .as_array()
        .expect("requests")
        .iter()
        .find(|r| r["label"] == label)
        .expect("request")
}

#[tokio::test]
async fn a_cors_check_sees_the_preflights_and_what_reached_the_upstream() {
    let (_server, state, base) = instance(config()).await;
    let run = run_of(&state, &base, "cors").await;
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    let gateway = start_gw(behaving("cors", &upstream, None)).await;
    let v = check_via(&state, &run, &gateway).await;
    assert_eq!(v["passed"], true, "{v:#}");
    // The gateway answered both preflights; the GET reached the upstream.
    let ups = |l: &str| label(&v, l)["upstream"].as_array().expect("u").len();
    assert_eq!(ups(catalog::PREFLIGHT_ALLOWED), 0, "{v:#}");
    assert_eq!(ups(catalog::PREFLIGHT_REFUSED), 0, "{v:#}");
    assert_eq!(ups(catalog::SIMPLE_ALLOWED), 1, "{v:#}");
    // The instance's own CORS layer does not answer for the run's mock: a
    // preflight sent straight to it reaches the mock and has no CORS.
    let client = reqwest::Client::new();
    let r = client
        .request(reqwest::Method::OPTIONS, format!("{upstream}/cors"))
        .header("origin", "https://somewhere.example.org")
        .header("access-control-request-method", "GET")
        .send()
        .await
        .expect("preflight");
    assert_eq!(r.status(), 204);
    assert!(r.headers().get("access-control-allow-origin").is_none());
    // Any other mock keeps the instance's CORS.
    let r = client
        .request(
            reqwest::Method::OPTIONS,
            format!("{base}/mock/petstore/pets"),
        )
        .header("origin", "https://somewhere.example.org")
        .header("access-control-request-method", "GET")
        .send()
        .await
        .expect("preflight");
    assert!(r.headers().get("access-control-allow-origin").is_some());

    // The upstream answers CORS: a gateway that lets everything through
    // passes, one that answers preflights itself does not.
    let body = || CheckBody::new(gateway.clone()).with("upstream_cors", true);
    let plain = start_gw(Gw {
        upstream: upstream.clone(),
        ..Gw::default()
    })
    .await;
    let v = check_with(
        &state,
        &run,
        CheckBody::new(plain.clone()).with("upstream_cors", true),
    )
    .await;
    assert_eq!(v["passed"], true, "{v:#}");
    assert_eq!(
        label(&v, catalog::PREFLIGHT_ALLOWED)["upstream"],
        json!([204]),
        "{v:#}"
    );
    let shown = get_run(&state, "me", "cors", run["id"].as_str().expect("id"), true).expect("run");
    assert_eq!(shown["upstream_answers_cors"], true, "{shown}");
    let v = check_with(&state, &run, body()).await;
    assert_eq!(v["passed"], false, "{v:#}");
    assert_eq!(
        failed(&v),
        ["The preflight reached the upstream, which answers CORS"],
        "{v:#}"
    );
    // Back to upstream_cors: false, the mock answers no CORS again.
    let v = check_with(&state, &run, CheckBody::new(plain)).await;
    assert!(
        failed(&v)
            .iter()
            .any(|f| f == "The preflight from the allowed origin got CORS headers"),
        "{v:#}"
    );
    // The gateway's CORS values are described, never echoed.
    assert!(!v.to_string().contains("GET, HEAD, OPTIONS"), "{v}");
    end(&state, "me", "cors", run["id"].as_str().expect("id")).expect("end");
}

#[tokio::test]
async fn a_cors_run_works_with_the_instance_cors_off() {
    let mut c = config();
    c.cors_allow_origins = Vec::new();
    let (_server, state, base) = instance(c).await;
    let run = run_of(&state, &base, "cors").await;
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    let gateway = start_gw(behaving("cors", &upstream, None)).await;
    let v = check_via(&state, &run, &gateway).await;
    assert_eq!(v["passed"], true, "{v:#}");
    let plain = start_gw(Gw {
        upstream: upstream.clone(),
        ..Gw::default()
    })
    .await;
    let v = check_with(
        &state,
        &run,
        CheckBody::new(plain.clone()).with("upstream_cors", true),
    )
    .await;
    assert_eq!(v["passed"], true, "{v:#}");
    let v = check_with(&state, &run, CheckBody::new(upstream.clone())).await;
    assert!(
        failed(&v)
            .iter()
            .any(|f| f == "The preflight from the allowed origin got CORS headers"),
        "{v:#}"
    );
    end(&state, "me", "cors", run["id"].as_str().expect("id")).expect("end");
}

#[tokio::test]
async fn cors_and_header_checks_take_their_parameters() {
    let (_server, state, base) = instance(config()).await;
    // Origins are normalised: the check sends the origin as a browser does.
    let run = run_of(&state, &base, "cors").await;
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    let gateway = start_gw(Gw {
        upstream: upstream.clone(),
        cors: Some(Cors {
            origin: "http://localhost:3000".into(),
            answer_preflights: true,
        }),
        ..Gw::default()
    })
    .await;
    let v = check_with(
        &state,
        &run,
        CheckBody::new(gateway.clone())
            .with("allowed_origin", "HTTP://LocalHost:3000")
            .with("refused_origin", "http://localhost:3001"),
    )
    .await;
    assert_eq!(v["passed"], true, "{v:#}");
    // With the defaults the gateway refuses the allowed origin.
    let v = check_via(&state, &run, &gateway).await;
    assert!(
        failed(&v)
            .iter()
            .any(|f| f == "The preflight from the allowed origin got CORS headers"),
        "{v:#}"
    );
    end(&state, "me", "cors", run["id"].as_str().expect("id")).expect("end");

    let run = run_of(&state, &base, "header_transformation").await;
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    let gateway = start_gw(Gw {
        upstream: upstream.clone(),
        add: Some(("X-Team", "blue".into())),
        remove: Some("x-debug"),
        add_response: Some(("X-Served-By", "gw-1")),
        ..Gw::default()
    })
    .await;
    let body = |value: &str| {
        CheckBody::new(gateway.clone())
            .with("add_request_header", "X-Team")
            .with("add_request_value", value)
            .with("remove_request_header", "X-Debug")
            .with("add_response_header", "x-served-by")
    };
    let v = check_with(&state, &run, body("blue")).await;
    assert_eq!(v["passed"], true, "{v:#}");
    let v = check_with(&state, &run, body("red")).await;
    assert_eq!(
        failed(&v),
        ["The added request header reached the upstream"],
        "{v:#}"
    );
    // The header the client sent for removal, through a pass-through proxy.
    let plain = start_gw(Gw {
        upstream,
        ..Gw::default()
    })
    .await;
    let v = check_with(
        &state,
        &run,
        CheckBody::new(plain).with("remove_request_header", "X-Debug"),
    )
    .await;
    let removed = v["expectations"]
        .as_array()
        .expect("e")
        .iter()
        .find(|e| e["name"] == "The removed request header did not reach the upstream")
        .expect("removed")
        .clone();
    assert_eq!(removed["ok"], false, "{v:#}");
    assert_eq!(
        removed["seen"], "present with the value the client sent",
        "{v:#}"
    );
    end(
        &state,
        "me",
        "header_transformation",
        run["id"].as_str().expect("id"),
    )
    .expect("end");
}

#[tokio::test]
async fn origins_and_header_names_are_validated() {
    let state = test_state_with(config());
    let cors = run_of(&state, "http://127.0.0.1:9", "cors").await;
    let hdr = run_of(&state, "http://127.0.0.1:9", "header_transformation").await;
    let refused = |id: &'static str, run: &Value, name: &str, value: Value| {
        let state = state.clone();
        let run = run["id"].as_str().expect("id").to_string();
        let body = CheckBody::new("http://127.0.0.1:9/gw").with(name, value);
        async move {
            match check(&state, "me", id, &run, body).await {
                Err(Refused(StatusCode::BAD_REQUEST, m)) => m,
                Err(Refused(status, m)) => panic!("refused with {status}: {m}"),
                Ok(v) => panic!("checked: {v}"),
            }
        }
    };
    for bad in [
        json!("app.example.com"),
        json!("https://app.example.com/path"),
        json!("https://app.example.com/"),
        json!("ftp://app.example.com"),
        json!("https://user@app.example.com"),
        json!("https://app.example.com?q=1"),
        json!(format!("https://{}.example.com", "a".repeat(200))),
        json!(42),
    ] {
        let m = refused("cors", &cors, "allowed_origin", bad.clone()).await;
        assert!(m.starts_with("allowed_origin: "), "{bad}: {m}");
    }
    let m = refused(
        "cors",
        &cors,
        "refused_origin",
        json!("https://app.example.com"),
    )
    .await;
    assert!(m.contains("another origin than allowed_origin"), "{m}");
    let m = refused("cors", &cors, "upstream_cors", json!("yes")).await;
    assert!(m.contains("upstream_cors: true or false"), "{m}");
    for bad in [
        json!("bad header"),
        json!(""),
        json!("Host"),
        json!("X-Rustybin-Session"),
        json!("Access-Control-Allow-Origin"),
        json!("x".repeat(65)),
        json!(true),
    ] {
        let m = refused(
            "header_transformation",
            &hdr,
            "add_response_header",
            bad.clone(),
        )
        .await;
        assert!(m.starts_with("add_response_header: "), "{bad}: {m}");
    }
    for bad in [
        json!("a\nb"),
        json!(""),
        json!(" padded"),
        json!("v".repeat(201)),
    ] {
        let m = refused(
            "header_transformation",
            &hdr,
            "add_request_value",
            bad.clone(),
        )
        .await;
        assert!(m.starts_with("add_request_value: "), "{bad}: {m}");
    }
    let m = refused(
        "header_transformation",
        &hdr,
        "remove_request_header",
        json!(catalog::DEFAULT_ADD_REQUEST_HEADER),
    )
    .await;
    assert!(m.contains("another header than add_request_header"), "{m}");
    let m = refused(
        "header_transformation",
        &hdr,
        "allowed_origin",
        json!("https://a.example.com"),
    )
    .await;
    assert!(m.contains("allowed_origin applies to cors only"), "{m}");
    let m = refused("cors", &cors, "nope", json!(1)).await;
    assert!(m.contains("unknown test parameter \"nope\""), "{m}");
    // Nothing was counted, and the listing describes the new parameters.
    for (id, run) in [("cors", &cors), ("header_transformation", &hdr)] {
        let shown = get_run(&state, "me", id, run["id"].as_str().expect("id"), true).expect("run");
        assert_eq!(shown["checks"], 0, "{shown}");
    }
    let listed = list(&state, "me", true);
    let params = |id: &str| {
        listed["scenarios"]
            .as_array()
            .expect("scenarios")
            .iter()
            .find(|s| s["id"] == id)
            .map(|s| s["parameters"].clone())
            .expect("scenario")
    };
    let p = params("cors");
    assert_eq!(p[0]["name"], "allowed_origin");
    assert_eq!(p[0]["type"], "string");
    assert_eq!(p[0]["format"], "origin");
    assert_eq!(p[2]["type"], "boolean");
    assert_eq!(p[2]["default"], false);
    let p = params("header_transformation");
    assert_eq!(p.as_array().map(Vec::len), Some(4));
    assert_eq!(p[1]["name"], "add_request_value");
    assert!(p[1].get("default").is_none(), "{p}");
}

#[test]
fn every_test_parameter_has_a_cli_flag_and_a_tool_property() {
    let check = crate::cli::commands::COMMANDS
        .iter()
        .find(|c| c.noun == "scenario" && c.verb == "check")
        .expect("scenario check");
    let tool = crate::mcp::manage::tools::TOOLS
        .iter()
        .find(|t| t.name == "check_gateway_scenario")
        .map(|t| (t.input_schema)())
        .expect("check_gateway_scenario");
    for p in catalog::PARAMS {
        let flag = format!("--{}", p.name.replace('_', "-"));
        let f = check
            .flags
            .iter()
            .find(|f| f.name == flag)
            .unwrap_or_else(|| panic!("no {flag} on scenario check"));
        assert_eq!(
            f.value,
            !matches!(p.kind, catalog::Kind::Bool { .. }),
            "{flag}: a switch for a boolean, a value otherwise"
        );
        assert!(check.usage.contains(&flag), "{flag} not in the usage");
        assert!(
            tool["properties"].get(p.name).is_some(),
            "{} not in the tool schema",
            p.name
        );
        // Every scenario parameter belongs to a scenario.
        assert!(
            SCENARIOS
                .iter()
                .any(|s| s.params.iter().any(|q| q.name == p.name)),
            "{} belongs to no scenario",
            p.name
        );
    }
}
