use super::*;

use std::time::Duration;

use axum::body::Body;
use axum::http::Request as HttpRequest;
use chrono::DateTime;
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::config::Config;
use crate::inspector::{CapturedRequest, Inspector, InspectorQuery, KeyRetention};
use crate::limits::{ManualClock, Period, Scope, MAX_SESSIONS};
use crate::test_support::{body_json, body_string, sign_control_jwt, test_app_with};

pub(crate) const KEY_A: &str = "rbk_ka1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1";
pub(crate) const KEY_B: &str = "rbk_kb2_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb2";
const ADMIN: &str = "admin-secret";

/// One registry entry: (id, presented key, label, status, limits).
pub(crate) fn entry(id: &str, key: &str, label: &str, status: &str, limits: Value) -> Value {
    json!({
        "id": id,
        "hash": key_hash_hex(key),
        "label": label,
        "status": status,
        "limits": limits,
    })
}

pub(crate) fn doc(entries: Vec<Value>) -> String {
    json!({ "version": "v1", "keys": entries }).to_string()
}

/// Keys A (`ka1`, label starter) and B (`kb2`, label free).
fn two_keys() -> String {
    doc(vec![
        entry(
            "ka1",
            KEY_A,
            "starter",
            "active",
            json!({ "rps": 100, "burst": 200 }),
        ),
        entry("kb2", KEY_B, "free", "active", json!({})),
    ])
}

pub(crate) fn shared_config() -> Config {
    let mut c = Config::for_tests();
    c.shared_mode = true;
    c.public_mode = true;
    c.admin_token = Some(ADMIN.to_string());
    c.limits.scope = Scope::Session;
    c.limits.period = Period::Day;
    c
}

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .expect("timestamp")
        .with_timezone(&Utc)
}

fn shared_state(registry: &str) -> AppState {
    let state = crate::test_support::test_state_with(shared_config());
    state
        .keys
        .load_bytes(registry.as_bytes())
        .expect("registry");
    state
}

fn clocked_state(config: Config, registry: &str, now: &str) -> (AppState, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(at(now)));
    let state = AppState::for_tests_with_clock(config, clock.clone());
    state
        .keys
        .load_bytes(registry.as_bytes())
        .expect("registry");
    (state, clock)
}

pub(crate) async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
) -> Response {
    let mut b = HttpRequest::builder().method(method).uri(uri);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    app.clone()
        .oneshot(b.body(Body::empty()).expect("request"))
        .await
        .expect("response")
}

fn hdr<'a>(resp: &'a Response, name: &str) -> Option<&'a str> {
    resp.headers().get(name).and_then(|v| v.to_str().ok())
}

// ── Registry ────────────────────────────────────────────────────────

#[test]
fn registry_parses_active_keys_and_skips_the_rest() {
    let base = crate::limits::LimitsConfig::preset(crate::limits::Plan::Free);
    let raw = doc(vec![
        entry(
            "ka1",
            KEY_A,
            "starter",
            "active",
            json!({ "rps": 100, "requests": 100000 }),
        ),
        entry("kb2", KEY_B, "free", "revoked", json!({})),
        entry("kc3", "rbk_kc3_c", "x", "disabled", json!({})),
        json!({ "id": "kd4", "hash": key_hash_hex("rbk_kd4_d") }),
        json!({ "id": "bad id!", "hash": key_hash_hex("rbk_x") }),
        json!({ "id": "ke5", "hash": "nothex" }),
        entry("ka1", "rbk_dup", "dup", "active", json!({})),
    ]);
    let set = KeySet::parse(raw.as_bytes(), &base, 100).expect("parse");
    assert_eq!(set.len(), 2);
    assert_eq!(set.skipped, 5);
    assert_eq!(set.version.as_deref(), Some("v1"));

    let a = set.find(KEY_A).expect("key A");
    assert_eq!(a.id.as_ref(), "ka1");
    assert_eq!(a.label.as_deref(), Some("starter"));
    assert_eq!(a.limits.rps, 100);
    // Absent fields fall back to the instance limits.
    assert_eq!(a.limits.burst, base.burst);
    assert_eq!(a.limits.concurrency, base.concurrency);
    assert_eq!(a.requests_per_day, 100_000);
    assert_eq!(a.egress_per_day, base.egress_bytes);
    // Quotas are the key layer's job, not the limiter's.
    assert_eq!((a.limits.requests, a.limits.egress_bytes), (0, 0));
    assert_eq!(a.limits.plan_name(), "starter");
    assert!(a.limits.per_key);

    assert!(set.find(KEY_B).is_none(), "revoked keys are unknown");
    assert!(set.find("rbk_kd4_d").is_some(), "no status means active");
    assert!(set.find("rbk_nope").is_none());
    assert!(set.find("").is_none());
    assert!(set.find(&"x".repeat(1000)).is_none());
    assert!(set.by_id("kd4").is_some());
    assert!(KeySet::parse(b"not json", &base, 100).is_err());
}

#[test]
fn registry_larger_than_the_cap_is_refused_and_the_last_set_kept() {
    let base = crate::limits::LimitsConfig::default();
    assert!(KeySet::parse(two_keys().as_bytes(), &base, 1).is_err());

    let mut config = shared_config();
    config.keys_max = 2;
    let state = crate::test_support::test_state_with(config);
    state
        .keys
        .load_bytes(two_keys().as_bytes())
        .expect("two keys fit");
    let three = doc(vec![
        entry("ka1", KEY_A, "a", "active", json!({})),
        entry("kb2", KEY_B, "b", "active", json!({})),
        entry("kc3", "rbk_kc3_cccccccccccccccc", "c", "active", json!({})),
    ]);
    assert!(state.keys.load_bytes(three.as_bytes()).is_err());
    assert_eq!(state.keys.current().map(|s| s.len()), Some(2));
}

#[test]
fn redaction_helpers() {
    assert_eq!(redact_key(KEY_A), "rbk_...aaa1");
    assert_eq!(redact_key("short"), "rbk_...");
    assert_eq!(
        redact_query(&format!("x=1&rb_key={KEY_A}&y=2")).as_deref(),
        Some("x=1&rb_key=rbk_...aaa1&y=2")
    );
    assert_eq!(redact_query("x=1"), None);
    let uri: Uri = format!("/echo?rb_key={KEY_B}").parse().expect("uri");
    assert_eq!(redact_uri(&uri), "/echo?rb_key=rbk_...bbb2");
    assert_eq!(
        redact_header("X-Rustybin-Key", KEY_A.as_bytes()),
        "rbk_...aaa1"
    );
    assert_eq!(redact_header("x-other", b"v"), "v");
}

// ── Data plane ──────────────────────────────────────────────────────

#[tokio::test]
async fn data_plane_needs_a_valid_key_and_readiness_waits_for_the_registry() {
    let state = crate::test_support::test_state_with(shared_config());
    let app = test_app_with(state.clone());

    let resp = send(&app, "GET", "/echo", &[("x-rustybin-key", KEY_A)]).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body_json(resp).await["error"], ERR_UNAVAILABLE);
    let resp = send(&app, "GET", "/_rustybin/ready", &[]).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body_json(resp).await["reason"], ERR_UNAVAILABLE);
    // The landing page stays 200 (liveness).
    assert_eq!(send(&app, "GET", "/", &[]).await.status(), StatusCode::OK);

    state
        .keys
        .load_bytes(two_keys().as_bytes())
        .expect("registry");
    let resp = send(&app, "GET", "/_rustybin/ready", &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = send(&app, "GET", "/echo", &[]).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(resp).await["error"], ERR_REQUIRED);
    let resp = send(
        &app,
        "GET",
        "/echo",
        &[("x-rustybin-key", "rbk_wrong_key_123")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(resp).await["error"], ERR_INVALID);

    let resp = send(&app, "GET", "/echo", &[("x-rustybin-key", KEY_A)]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(hdr(&resp, "x-rustybin-plan"), Some("starter"));
    // Echo returns what the caller sent (its own key).
    assert!(body_string(resp).await.contains(KEY_A));

    let resp = send(&app, "GET", &format!("/uuid?rb_key={KEY_B}"), &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(hdr(&resp, "x-rustybin-plan"), Some("free"));

    // The header wins over the query parameter.
    let resp = send(
        &app,
        "GET",
        &format!("/uuid?rb_key={KEY_B}"),
        &[("x-rustybin-key", "rbk_wrong_key_123")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // The console's static files need no key.
    assert_ne!(
        send(&app, "GET", "/ui/", &[]).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn two_keys_never_see_each_others_requests_or_counters() {
    let state = shared_state(&two_keys());
    let app = test_app_with(state.clone());
    for _ in 0..3 {
        send(&app, "GET", "/echo/a", &[("x-rustybin-key", KEY_A)]).await;
    }
    send(&app, "GET", "/echo/b", &[("x-rustybin-key", KEY_B)]).await;

    // Inspector list.
    let resp = send(
        &app,
        "GET",
        "/_rustybin/requests",
        &[("x-rustybin-key", KEY_A)],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["count"], 3);
    assert!(v["requests"]
        .as_array()
        .expect("array")
        .iter()
        .all(|r| r["key_id"] == "ka1" && r["path"] == "/echo/a"));
    let a_id = v["requests"][0]["id"].as_str().expect("id").to_string();

    let resp = send(
        &app,
        "GET",
        &format!("/_rustybin/requests?rb_key={KEY_B}"),
        &[],
    )
    .await;
    let v = body_json(resp).await;
    assert_eq!(v["count"], 1);
    assert_eq!(v["requests"][0]["key_id"], "kb2");

    // One entry by id: only for its own key.
    let uri = format!("/_rustybin/requests/{a_id}");
    let resp = send(&app, "GET", &uri, &[("x-rustybin-key", KEY_B)]).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = send(&app, "GET", &uri, &[("x-rustybin-key", KEY_A)]).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Usage counters.
    let resp = send(
        &app,
        "GET",
        "/_rustybin/usage",
        &[("x-rustybin-key", KEY_A)],
    )
    .await;
    let v = body_json(resp).await;
    assert_eq!(v["key_id"], "ka1");
    assert_eq!(v["plan"], "starter");
    assert_eq!(v["scope"], "key");
    assert_eq!(v["requests"]["used"], 3);
    let resp = send(
        &app,
        "GET",
        "/_rustybin/usage",
        &[("x-rustybin-key", KEY_B)],
    )
    .await;
    let v = body_json(resp).await;
    assert_eq!(v["key_id"], "kb2");
    assert_eq!(v["requests"]["used"], 1);

    // The admin sees everything.
    let resp = send(
        &app,
        "GET",
        "/_rustybin/requests",
        &[("authorization", "Bearer admin-secret")],
    )
    .await;
    assert_eq!(body_json(resp).await["count"], 4);
}

#[tokio::test]
async fn the_live_feed_only_carries_the_callers_key() {
    let state = shared_state(&two_keys());
    let app = test_app_with(state.clone());
    let resp = send(
        &app,
        "GET",
        "/_rustybin/requests/stream",
        &[("x-rustybin-key", KEY_A)],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body();

    send(&app, "GET", "/echo/from-b", &[("x-rustybin-key", KEY_B)]).await;
    send(&app, "GET", "/echo/from-a", &[("x-rustybin-key", KEY_A)]).await;

    let mut text = String::new();
    while !text.contains("\ndata:") && !text.starts_with("data:") {
        let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .expect("an event in time")
            .expect("open stream")
            .expect("frame");
        if let Some(data) = frame.data_ref() {
            text.push_str(&String::from_utf8_lossy(data));
        }
    }
    assert!(text.contains("/echo/from-a"), "{text}");
    assert!(!text.contains("/echo/from-b"), "{text}");
    assert!(!text.contains(KEY_A), "the feed is redacted: {text}");
}

#[tokio::test]
async fn keys_are_redacted_in_the_inspector() {
    let state = shared_state(&two_keys());
    let app = test_app_with(state.clone());
    let resp = send(
        &app,
        "GET",
        &format!("/echo?x=1&rb_key={KEY_A}"),
        &[("x-rustybin-key", KEY_A)],
    )
    .await;
    assert!(body_string(resp).await.contains(KEY_A));
    let entries = state.inspector.list(&InspectorQuery::default());
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    let text = serde_json::to_string(e.as_ref()).expect("json");
    assert!(!text.contains(KEY_A), "{text}");
    assert!(e.uri.contains("rb_key=rbk_...aaa1"), "{}", e.uri);
    assert_eq!(e.query.as_deref(), Some("x=1&rb_key=rbk_...aaa1"));
    let h = e
        .headers
        .iter()
        .find(|h| h.name == "x-rustybin-key")
        .expect("header kept");
    assert_eq!(h.value, "rbk_...aaa1");

    // Request bins redact too.
    let resp = send(&app, "POST", "/bin", &[("x-rustybin-key", KEY_A)]).await;
    let bin = body_json(resp).await;
    let id = bin["id"].as_str().expect("bin id").to_string();
    send(
        &app,
        "POST",
        &format!("/bin/{id}?rb_key={KEY_A}"),
        &[("x-rustybin-key", KEY_A)],
    )
    .await;
    let resp = send(
        &app,
        "GET",
        &format!("/bin/{id}/requests"),
        &[("x-rustybin-key", KEY_A)],
    )
    .await;
    let text = body_string(resp).await;
    assert!(text.contains("rbk_...aaa1"), "{text}");
    assert!(!text.contains(KEY_A), "{text}");
}

// ── Quotas and limits ───────────────────────────────────────────────

#[tokio::test]
async fn daily_quota_rejects_until_midnight_utc() {
    let registry = doc(vec![
        entry("ka1", KEY_A, "starter", "active", json!({ "requests": 3 })),
        entry("kb2", KEY_B, "free", "active", json!({})),
    ]);
    let mut config = shared_config();
    config.quota_upgrade_url = Some("https://example.com/upgrade".to_string());
    let (state, clock) = clocked_state(config, &registry, "2026-10-17T23:59:00Z");
    let app = test_app_with(state.clone());
    for left in ["2", "1", "0"] {
        let resp = send(&app, "GET", "/uuid", &[("x-rustybin-key", KEY_A)]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(hdr(&resp, "x-rustybin-quota-remaining"), Some(left));
    }
    let resp = send(&app, "GET", "/uuid", &[("x-rustybin-key", KEY_A)]).await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(hdr(&resp, "retry-after"), Some("60"));
    assert_eq!(hdr(&resp, "x-rustybin-limit"), Some("requests"));
    assert_eq!(hdr(&resp, "x-rustybin-plan"), Some("starter"));
    let v = body_json(resp).await;
    assert_eq!(
        v,
        json!({
            "error": "rustybin_quota_exceeded",
            "quota": "requests",
            "reset_at": "2026-10-18T00:00:00Z",
            "upgrade_url": "https://example.com/upgrade",
        })
    );
    // Still rejected a little later, another key is not affected.
    clock.advance(Duration::from_secs(30));
    let resp = send(&app, "GET", "/uuid", &[("x-rustybin-key", KEY_A)]).await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(hdr(&resp, "retry-after"), Some("30"));
    let resp = send(&app, "GET", "/uuid", &[("x-rustybin-key", KEY_B)]).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // A new UTC day: the quota is back, nothing rolls over.
    clock.advance(Duration::from_secs(31));
    let resp = send(&app, "GET", "/uuid", &[("x-rustybin-key", KEY_A)]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(hdr(&resp, "x-rustybin-quota-remaining"), Some("2"));
    assert!(
        state
            .metrics
            .render()
            .contains("rustybin_limit_rejections_total{dimension=\"requests\"} 2"),
        "quota rejections are counted"
    );
}

#[tokio::test]
async fn egress_quota_never_cuts_a_response() {
    let registry = doc(vec![entry(
        "ka1",
        KEY_A,
        "starter",
        "active",
        json!({ "egress_bytes": 10 }),
    )]);
    let (state, _clock) = clocked_state(shared_config(), &registry, "2026-10-17T12:00:00Z");
    let app = test_app_with(state.clone());
    let resp = send(&app, "GET", "/bytes/100", &[("x-rustybin-key", KEY_A)]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(crate::test_support::body_bytes(resp).await.len(), 100);
    let resp = send(&app, "GET", "/bytes/100", &[("x-rustybin-key", KEY_A)]).await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let v = body_json(resp).await;
    assert_eq!(v["quota"], "egress");
    assert_eq!(v["upgrade_url"], Value::Null);
    let day = state.keys.today();
    assert_eq!(state.keys.ledger().totals(day, "ka1"), (1, 100));
}

#[tokio::test]
async fn rate_limit_rejections_keep_the_existing_429_and_do_not_use_the_quota() {
    let registry = doc(vec![entry(
        "ka1",
        KEY_A,
        "free",
        "active",
        json!({ "rps": 1, "burst": 1, "requests": 100 }),
    )]);
    let (state, _clock) = clocked_state(shared_config(), &registry, "2026-10-17T12:00:00Z");
    let app = test_app_with(state.clone());
    let resp = send(&app, "GET", "/uuid", &[("x-rustybin-key", KEY_A)]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = send(&app, "GET", "/uuid", &[("x-rustybin-key", KEY_A)]).await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(hdr(&resp, "retry-after"), Some("1"));
    assert_eq!(hdr(&resp, "x-rustybin-plan"), Some("free"));
    let v = body_json(resp).await;
    assert_eq!(v["error"], crate::limits::ERROR_CODE);
    assert_eq!(v["limit"], "rps");
    assert_eq!(v["scope"], "key");
    let day = state.keys.today();
    assert_eq!(state.keys.ledger().totals(day, "ka1").0, 1);
    // Counters are kept per key id, never per secret or IP.
    assert_eq!(state.limits.session_count(), 1);
}

// ── Revocation ──────────────────────────────────────────────────────

#[tokio::test]
async fn revoking_a_key_in_the_file_stops_it_within_one_poll() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("keys.json");
    std::fs::write(&path, two_keys()).expect("write");
    let mut config = shared_config();
    config.keys_file = Some(path.clone());
    let state = crate::test_support::test_state_with(config);
    assert!(state.keys.loaded(), "a registry file is read at startup");
    let app = test_app_with(state.clone());
    let resp = send(&app, "GET", "/uuid", &[("x-rustybin-key", KEY_B)]).await;
    assert_eq!(resp.status(), StatusCode::OK);

    assert!(!state.keys.poll_file_once(), "unchanged file: no reload");
    let revoked = doc(vec![
        entry("ka1", KEY_A, "starter", "active", json!({})),
        entry("kb2", KEY_B, "free", "revoked", json!({})),
    ]);
    std::fs::write(&path, revoked).expect("write");
    assert!(state.keys.poll_file_once());
    let resp = send(&app, "GET", "/uuid", &[("x-rustybin-key", KEY_B)]).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(resp).await["error"], ERR_INVALID);
    let resp = send(&app, "GET", "/uuid", &[("x-rustybin-key", KEY_A)]).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // A broken file keeps the last good set.
    std::fs::write(&path, "{ not json").expect("write");
    assert!(!state.keys.poll_file_once());
    let resp = send(&app, "GET", "/uuid", &[("x-rustybin-key", KEY_A)]).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

/// A registry server: serves `doc` with `etag`, requires the bearer token.
#[derive(Clone, Default)]
struct RegistryServer {
    doc: Arc<Mutex<(String, String)>>,
    not_modified: Arc<std::sync::atomic::AtomicUsize>,
    unauthorized: Arc<std::sync::atomic::AtomicUsize>,
}

async fn registry_handler(State(srv): State<RegistryServer>, headers: HeaderMap) -> Response {
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok());
    if auth != Some("Bearer reg-token") {
        srv.unauthorized.fetch_add(1, Ordering::SeqCst);
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let (body, etag) = lock(&srv.doc).clone();
    let inm = headers.get("if-none-match").and_then(|v| v.to_str().ok());
    if inm == Some(etag.as_str()) {
        srv.not_modified.fetch_add(1, Ordering::SeqCst);
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mut resp = body.into_response();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        resp.headers_mut().insert(header::ETAG, v);
    }
    resp
}

/// Path of the test registry server (not a Rustybin route).
const REGISTRY_PATH: &str = "/keys";

async fn start_registry(srv: RegistryServer) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = Router::new()
        .route(REGISTRY_PATH, get(registry_handler))
        .with_state(srv);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

#[tokio::test]
async fn url_registry_is_polled_with_the_token_and_etag() {
    let srv = RegistryServer::default();
    *lock(&srv.doc) = (two_keys(), "\"v1\"".to_string());
    let addr = start_registry(srv.clone()).await;

    let mut config = shared_config();
    config.keys_url = Some(format!("http://{addr}/keys"));
    config.keys_token = Some("reg-token".to_string());
    config.keys_poll_secs = 1;
    let state = crate::test_support::test_state_with(config);
    assert!(!state.keys.loaded());

    // The loop fetches at once, then every RUSTYBIN_KEYS_POLL_SECS.
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(state.keys.clone().registry_loop(async move {
        let _ = stop_rx.await;
    }));
    for _ in 0..100 {
        if state.keys.loaded() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(state.keys.loaded());
    let app = test_app_with(state.clone());
    let resp = send(&app, "GET", "/uuid", &[("x-rustybin-key", KEY_B)]).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Revoke key B on the platform: gone after the next poll.
    *lock(&srv.doc) = (
        doc(vec![
            entry("ka1", KEY_A, "starter", "active", json!({})),
            entry("kb2", KEY_B, "free", "revoked", json!({})),
        ]),
        "\"v2\"".to_string(),
    );
    let mut revoked = false;
    for _ in 0..150 {
        let resp = send(&app, "GET", "/uuid", &[("x-rustybin-key", KEY_B)]).await;
        if resp.status() == StatusCode::UNAUTHORIZED {
            revoked = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(revoked, "revocation applied within one poll interval");
    let _ = stop_tx.send(());
    let _ = task.await;
    assert_eq!(srv.unauthorized.load(Ordering::SeqCst), 0, "token sent");

    // Unchanged document: 304, the set stays.
    let client = registry_client().expect("client");
    assert!(!state.keys.poll_url_once(&client).await);
    assert!(srv.not_modified.load(Ordering::SeqCst) >= 1);
    assert_eq!(state.keys.current().map(|s| s.len()), Some(1));
    assert!(state.metrics.render().contains("rustybin_keys_active 1"));
}

#[tokio::test]
async fn url_registry_errors_keep_the_last_good_set() {
    let srv = RegistryServer::default();
    *lock(&srv.doc) = (two_keys(), "\"v1\"".to_string());
    let addr = start_registry(srv.clone()).await;
    let mut config = shared_config();
    config.keys_url = Some(format!("http://{addr}/keys"));
    config.keys_token = Some("wrong".to_string());
    let state = crate::test_support::test_state_with(config);
    let client = registry_client().expect("client");
    assert!(!state.keys.poll_url_once(&client).await);
    assert!(!state.keys.loaded(), "401: nothing loaded");
    assert_eq!(srv.unauthorized.load(Ordering::SeqCst), 1);
}

// ── Control plane ───────────────────────────────────────────────────

#[tokio::test]
async fn shared_control_plane_needs_admin_except_for_a_keys_own_data() {
    let state = shared_state(&two_keys());
    let app = test_app_with(state.clone());
    send(&app, "GET", "/echo", &[("x-rustybin-key", KEY_A)]).await;
    let key = [("x-rustybin-key", KEY_A)];
    let admin = [("authorization", "Bearer admin-secret")];

    for path in ["/_rustybin/requests", "/_rustybin/usage"] {
        assert_eq!(send(&app, "GET", path, &key).await.status(), StatusCode::OK);
        let resp = send(&app, "GET", path, &[]).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(resp).await["error"], ERR_REQUIRED);
    }
    for path in [
        "/_rustybin/config",
        "/_rustybin/metrics",
        "/_rustybin/catalog",
        KEYS_USAGE_PATH,
    ] {
        let resp = send(&app, "GET", path, &key).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(
            send(&app, "GET", path, &admin).await.status(),
            StatusCode::OK,
            "{path}"
        );
    }
    // A key cannot clear the inspector or flip instance-wide state.
    let resp = send(&app, "DELETE", "/_rustybin/requests", &key).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let resp = send(&app, "POST", "/health/toggle", &key).await;
    assert_ne!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn shared_control_plane_without_admin_token_is_forbidden() {
    let mut config = shared_config();
    config.admin_token = None;
    let state = crate::test_support::test_state_with(config);
    state
        .keys
        .load_bytes(two_keys().as_bytes())
        .expect("registry");
    let app = test_app_with(state);
    let resp = send(&app, "GET", "/_rustybin/config", &[]).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = send(
        &app,
        "GET",
        "/_rustybin/usage",
        &[("x-rustybin-key", KEY_A)],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn control_jwt_bound_to_a_key_sees_only_that_key() {
    let mut config = crate::test_support::jwt_config();
    config.shared_mode = true;
    config.public_mode = true;
    config.limits.scope = Scope::Session;
    config.limits.period = Period::Day;
    let state = crate::test_support::test_state_with(config);
    state
        .keys
        .load_bytes(two_keys().as_bytes())
        .expect("registry");
    let app = test_app_with(state);
    send(&app, "GET", "/echo/a", &[("x-rustybin-key", KEY_A)]).await;
    send(&app, "GET", "/echo/b", &[("x-rustybin-key", KEY_B)]).await;

    let jwt = |scope: &str, rb_key: Option<&str>| {
        let mut claims = json!({
            "aud": "test-instance",
            "exp": chrono::Utc::now().timestamp() + 600,
            "scope": scope,
        });
        if let Some(k) = rb_key {
            claims["rb_key"] = json!(k);
        }
        format!("Bearer {}", sign_control_jwt(&claims, false))
    };

    let bound = jwt("inspector", Some("kb2"));
    let resp = send(
        &app,
        "GET",
        "/_rustybin/requests",
        &[("authorization", &bound)],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["count"], 1);
    assert_eq!(v["requests"][0]["key_id"], "kb2");
    let resp = send(
        &app,
        "GET",
        "/_rustybin/usage",
        &[("authorization", &bound)],
    )
    .await;
    assert_eq!(body_json(resp).await["key_id"], "kb2");
    // Bound tokens are still read-only inspector and usage tokens.
    let resp = send(
        &app,
        "GET",
        "/_rustybin/config",
        &[("authorization", &bound)],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let unknown = jwt("inspector", Some("nope"));
    let resp = send(
        &app,
        "GET",
        "/_rustybin/requests",
        &[("authorization", &unknown)],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let unbound = jwt("inspector", None);
    let resp = send(
        &app,
        "GET",
        "/_rustybin/requests",
        &[("authorization", &unbound)],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let admin = jwt("admin", None);
    let resp = send(
        &app,
        "GET",
        "/_rustybin/requests",
        &[("authorization", &admin)],
    )
    .await;
    assert_eq!(body_json(resp).await["count"], 2);
    let console = jwt("console", None);
    let resp = send(
        &app,
        "GET",
        "/_rustybin/config",
        &[("authorization", &console)],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ── Bounded state ───────────────────────────────────────────────────

fn captured(path: &str, key: Option<&str>) -> CapturedRequest {
    CapturedRequest {
        id: uuid::Uuid::new_v4().to_string(),
        request_id: None,
        timestamp: String::new(),
        method: "GET".into(),
        uri: path.into(),
        path: path.into(),
        query: None,
        version: "HTTP/1.1".into(),
        headers: vec![],
        client_ip: None,
        session: None,
        key_id: key.map(str::to_string),
        body: None,
        body_base64: None,
        body_size: 0,
        body_truncated: false,
        status: 200,
        latency_ms: 0.0,
        seq: 0,
        expires_at: None,
    }
}

#[test]
fn inspector_entries_are_capped_per_key_and_expire() {
    let inspector = Inspector::with_keys(6, true, true);
    let two = KeyRetention {
        entries: Some(2),
        ttl: None,
    };
    for n in 0..5 {
        inspector.record_with(captured(&format!("/a{n}"), Some("ka1")), two);
    }
    inspector.record_with(captured("/b0", Some("kb2")), KeyRetention::default());
    assert_eq!(inspector.key_len("ka1"), 2);
    let q = InspectorQuery {
        key_id: Some("ka1".into()),
        ..InspectorQuery::default()
    };
    let paths: Vec<_> = inspector.list(&q).iter().map(|e| e.path.clone()).collect();
    assert_eq!(paths, vec!["/a4", "/a3"]);
    assert_eq!(inspector.len(), 3);

    // The global capacity still applies, and the per-key index follows it.
    for n in 0..10 {
        inspector.record_with(
            captured(&format!("/c{n}"), Some("kc3")),
            KeyRetention::default(),
        );
    }
    assert_eq!(inspector.len(), 6);
    assert_eq!(inspector.key_len("ka1"), 0);
    assert_eq!(inspector.key_len("kc3"), 6);
    assert_eq!(inspector.key_count(), 1);

    // TTL: entries are hidden once expired.
    let ttl = KeyRetention {
        entries: None,
        ttl: Some(Duration::from_secs(60)),
    };
    inspector.record_with(captured("/d0", Some("kd4")), ttl);
    let q = InspectorQuery {
        key_id: Some("kd4".into()),
        ..InspectorQuery::default()
    };
    let now = std::time::Instant::now();
    assert_eq!(inspector.list_at(&q, now).len(), 1);
    assert_eq!(
        inspector.list_at(&q, now + Duration::from_secs(61)).len(),
        0
    );
}

#[tokio::test]
async fn inspector_retention_comes_from_the_key_record() {
    let registry = doc(vec![entry(
        "ka1",
        KEY_A,
        "free",
        "active",
        json!({ "inspector_entries": 2, "inspector_ttl_secs": 3600 }),
    )]);
    let state = shared_state(&registry);
    let app = test_app_with(state.clone());
    for n in 0..4 {
        send(
            &app,
            "GET",
            &format!("/echo/{n}"),
            &[("x-rustybin-key", KEY_A)],
        )
        .await;
    }
    assert_eq!(state.inspector.key_len("ka1"), 2);
    let e = &state.inspector.list(&InspectorQuery::default())[0];
    assert!(e.expires_at.is_some());
}

#[test]
fn limiter_state_per_key_is_bounded() {
    let state = shared_state(&two_keys());
    let record = state
        .keys
        .current()
        .and_then(|s| s.by_id("ka1"))
        .expect("key");
    for n in 0..(MAX_SESSIONS + 50) {
        state
            .limits
            .counters_for(&record.limits, Some(&format!("k{n}")));
    }
    assert!(state.limits.session_count() <= MAX_SESSIONS);
}

#[test]
fn ledger_is_bounded_and_keeps_today_and_yesterday() {
    let day = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("day");
    let ledger = Ledger::new(3);
    let key = |s: &str| -> Arc<str> { Arc::from(s) };
    for k in ["a", "b", "c"] {
        assert_eq!(ledger.admit(day("2026-10-17"), &key(k), 0, 0), Ok(None));
    }
    // Full: a fourth key is let through without being counted.
    assert_eq!(ledger.admit(day("2026-10-17"), &key("d"), 1, 0), Ok(None));
    assert_eq!(ledger.len(), 3);
    assert_eq!(ledger.totals(day("2026-10-17"), "d"), (0, 0));

    let ledger = Ledger::new(100);
    ledger.admit(day("2026-10-15"), &key("a"), 0, 0).ok();
    ledger.admit(day("2026-10-16"), &key("a"), 0, 0).ok();
    ledger.admit(day("2026-10-17"), &key("a"), 0, 0).ok();
    ledger.admit(day("2026-10-18"), &key("a"), 0, 0).ok();
    assert_eq!(ledger.len(), 3, "older days are dropped");
    assert_eq!(ledger.totals(day("2026-10-16"), "a"), (1, 0));
    assert_eq!(ledger.totals(day("2026-10-15"), "a"), (0, 0));
}

// ── Per-key usage endpoint ──────────────────────────────────────────

#[tokio::test]
async fn keys_usage_endpoint_pages_without_a_store() {
    let keys: Vec<(String, String)> = (1..=5)
        .map(|n| (format!("k{n}"), format!("rbk_k{n}_{}", "x".repeat(32))))
        .collect();
    let registry = doc(keys
        .iter()
        .map(|(id, k)| entry(id, k, "free", "active", json!({})))
        .collect());
    let (state, clock) = clocked_state(shared_config(), &registry, "2026-10-17T10:00:00Z");
    let app = test_app_with(state.clone());
    for (n, (_, k)) in keys.iter().enumerate() {
        for _ in 0..=n {
            let resp = send(&app, "GET", "/bytes/10", &[("x-rustybin-key", k)]).await;
            crate::test_support::body_bytes(resp).await;
        }
    }
    let admin = [("authorization", "Bearer admin-secret")];
    let page = |uri: String| {
        let app = app.clone();
        async move {
            let resp = send(&app, "GET", &uri, &admin).await;
            assert_eq!(resp.status(), StatusCode::OK, "{uri}");
            body_json(resp).await
        }
    };
    let v = page(format!("{KEYS_USAGE_PATH}?limit=2")).await;
    assert_eq!(
        v,
        json!({
            "day": "2026-10-17",
            "keys": [
                { "key_id": "k1", "requests": 1, "egress_bytes": 10 },
                { "key_id": "k2", "requests": 2, "egress_bytes": 20 },
            ],
            "next_after": "k2",
        })
    );
    let v = page(format!("{KEYS_USAGE_PATH}?limit=2&after=k2")).await;
    assert_eq!(v["keys"][0]["key_id"], "k3");
    assert_eq!(v["keys"][1]["key_id"], "k4");
    assert_eq!(v["next_after"], "k4");
    let v = page(format!("{KEYS_USAGE_PATH}?limit=2&after=k4")).await;
    assert_eq!(v["keys"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        v["keys"][0],
        json!({ "key_id": "k5", "requests": 5, "egress_bytes": 50 })
    );
    assert_eq!(v["next_after"], Value::Null);
    let v = page(format!("{KEYS_USAGE_PATH}?limit=5000")).await;
    assert_eq!(v["keys"].as_array().map(Vec::len), Some(5));
    assert_eq!(v["next_after"], Value::Null);

    // The next day: yesterday is still there, today is empty.
    clock.advance(Duration::from_secs(24 * 3600));
    let v = page(KEYS_USAGE_PATH.to_string()).await;
    assert_eq!(v["day"], "2026-10-18");
    assert_eq!(v["keys"], json!([]));
    let v = page(format!("{KEYS_USAGE_PATH}?day=2026-10-17")).await;
    assert_eq!(v["keys"].as_array().map(Vec::len), Some(5));

    for bad in ["?day=17-10-2026", "?limit=0", "?limit=x", "?after=bad%20id"] {
        let resp = send(&app, "GET", &format!("{KEYS_USAGE_PATH}{bad}"), &admin).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    let resp = send(&app, "GET", KEYS_USAGE_PATH, &[]).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ── Quota store ─────────────────────────────────────────────────────

fn with_store(state: &mut AppState, store: Arc<dyn QuotaStore>) {
    let keys =
        Keys::new(&state.config, state.metrics.clone(), state.limits.clock()).with_store(store);
    keys.load_bytes(two_keys().as_bytes()).expect("registry");
    state.keys = Arc::new(keys);
}

#[tokio::test]
async fn quota_store_outage_never_fails_requests() {
    let mut state = crate::test_support::test_state_with(shared_config());
    let store = Arc::new(store::MemoryStore::new());
    with_store(&mut state, store.clone());
    let app = test_app_with(state.clone());

    store.set_down(true);
    for _ in 0..3 {
        let resp = send(&app, "GET", "/uuid", &[("x-rustybin-key", KEY_A)]).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }
    assert!(state.keys.flush_now().await.is_err());
    assert_eq!(state.metrics.quota_store_up(), Some(false));
    assert!(state.metrics.render().contains("rustybin_quota_store_up 0"));
    let resp = send(
        &app,
        "GET",
        KEYS_USAGE_PATH,
        &[("authorization", "Bearer admin-secret")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body_json(resp).await["error"], ERR_STORE_UNAVAILABLE);

    // Back: the local counts reach the store, nothing is lost.
    store.set_down(false);
    state.keys.flush_now().await.expect("flush");
    assert_eq!(state.metrics.quota_store_up(), Some(true));
    let resp = send(
        &app,
        "GET",
        KEYS_USAGE_PATH,
        &[("authorization", "Bearer admin-secret")],
    )
    .await;
    let v = body_json(resp).await;
    assert_eq!(v["keys"][0]["key_id"], "ka1");
    assert_eq!(v["keys"][0]["requests"], 3);
    // Nothing pending after a flush.
    assert!(state.keys.ledger().pending().is_empty());
    let resp = send(
        &app,
        "GET",
        "/_rustybin/usage",
        &[("x-rustybin-key", KEY_A)],
    )
    .await;
    let v = body_json(resp).await;
    assert_eq!(v["requests"]["used"], 3);
    assert_eq!(v["quota_store"], "memory");
}

/// Two replicas sharing one store enforce one daily quota within 1%.
#[tokio::test]
async fn two_replicas_share_one_daily_quota_within_one_percent() {
    let limit = 10_000u64;
    let registry = doc(vec![entry(
        "ka1",
        KEY_A,
        "starter",
        "active",
        json!({ "requests": limit, "rps": 0 }),
    )]);
    let store: Arc<dyn QuotaStore> = Arc::new(store::MemoryStore::new());
    let clock = Arc::new(ManualClock::new(at("2026-10-17T12:00:00Z")));
    let replicas: Vec<Arc<Keys>> = (0..2)
        .map(|_| {
            let keys = Keys::new(&shared_config(), Arc::new(Metrics::new()), clock.clone())
                .with_store(store.clone());
            keys.load_bytes(registry.as_bytes()).expect("registry");
            Arc::new(keys)
        })
        .collect();
    let id: Arc<str> = Arc::from("ka1");
    let day = replicas[0].today();
    let mut admitted = 0u64;
    let mut stopped = [false, false];
    // Each replica takes 20 requests between flushes (the flush interval
    // times the rate).
    while !(stopped[0] && stopped[1]) {
        for (i, keys) in replicas.iter().enumerate() {
            for _ in 0..20 {
                match keys.ledger().admit(day, &id, limit, 0) {
                    Ok(_) => admitted += 1,
                    Err(_) => stopped[i] = true,
                }
            }
        }
        for keys in &replicas {
            keys.flush_now().await.expect("flush");
        }
    }
    assert!(admitted >= limit, "{admitted}");
    assert!(
        admitted as f64 <= limit as f64 * 1.01,
        "{admitted} is more than 1% over {limit}"
    );
    let (rows, _) = replicas[1].usage_page(day, None, 10).await.expect("page");
    assert_eq!(rows[0].requests, admitted);
}

// ── gRPC ────────────────────────────────────────────────────────────

#[test]
fn grpc_calls_need_a_key_and_respect_the_daily_quota() {
    let registry = doc(vec![entry(
        "ka1",
        KEY_A,
        "free",
        "active",
        json!({ "requests": 1 }),
    )]);
    let state = shared_state(&registry);
    let call = |key: Option<&str>| {
        let mut req = tonic::Request::new(());
        if let Some(k) = key {
            req.metadata_mut()
                .insert(KEY_HEADER, k.parse().expect("metadata"));
        }
        check_grpc(&state.keys, &state.limits, &req).map_err(|s| s.code())
    };
    assert_eq!(call(None), Err(tonic::Code::Unauthenticated));
    assert_eq!(
        call(Some("rbk_wrong_key_123")),
        Err(tonic::Code::Unauthenticated)
    );
    assert_eq!(call(Some(KEY_A)), Ok(()));
    assert_eq!(call(Some(KEY_A)), Err(tonic::Code::ResourceExhausted));
}

// ── RUSTYBIN_REQUIRE_KEY ────────────────────────────────────────────

#[tokio::test]
async fn require_key_on_a_normal_instance() {
    let mut config = Config::for_tests();
    config.require_key = true;
    let state = crate::test_support::test_state_with(config);
    let app = test_app_with(state.clone());
    let resp = send(&app, "GET", "/echo", &[("x-rustybin-key", KEY_A)]).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    state
        .keys
        .load_bytes(two_keys().as_bytes())
        .expect("registry");

    let resp = send(&app, "GET", "/echo", &[]).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(resp).await["error"], ERR_REQUIRED);
    let resp = send(
        &app,
        "GET",
        "/echo",
        &[("x-rustybin-key", "rbk_wrong_key_123")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let resp = send(&app, "GET", "/echo", &[("x-rustybin-key", KEY_A)]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    // No per-key limits or plan header on a normal instance.
    assert!(hdr(&resp, "x-rustybin-plan").is_none());

    // Control-plane auth is unchanged (open here), no key needed.
    let resp = send(&app, "GET", "/_rustybin/config", &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = send(&app, "GET", "/_rustybin/requests", &[]).await;
    assert_eq!(body_json(resp).await["count"], 1);
    // Usage per key is still counted for the platform.
    let resp = send(&app, "GET", KEYS_USAGE_PATH, &[]).await;
    assert_eq!(body_json(resp).await["keys"][0]["key_id"], "ka1");
}

#[tokio::test]
async fn require_key_keeps_control_plane_jwt_auth() {
    let mut config = crate::test_support::jwt_config();
    config.require_key = true;
    let state = crate::test_support::test_state_with(config);
    state
        .keys
        .load_bytes(two_keys().as_bytes())
        .expect("registry");
    let app = test_app_with(state);
    let resp = send(
        &app,
        "GET",
        "/_rustybin/config",
        &[("x-rustybin-key", KEY_A)],
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "a data key is not a control token"
    );
    let resp = send(
        &app,
        "GET",
        "/_rustybin/config",
        &[("authorization", "Bearer admin-secret")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn keys_are_off_by_default() {
    let app = crate::test_support::test_app();
    let resp = send(&app, "GET", "/echo", &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = send(&app, "GET", KEYS_USAGE_PATH, &[]).await;
    assert_eq!(body_json(resp).await["keys"], json!([]));
}

#[test]
fn shared_mode_configuration() {
    let map: std::collections::HashMap<&str, &str> = [
        ("RUSTYBIN_SHARED_MODE", "true"),
        ("RUSTYBIN_KEYS_URL", "https://platform.example/keys"),
        ("RUSTYBIN_KEYS_TOKEN", "reg-token"),
        ("RUSTYBIN_KEYS_POLL_SECS", "10"),
        ("RUSTYBIN_KEYS_MAX", "5000"),
        ("RUSTYBIN_QUOTA_STORE", "postgres://u:secret@db/quota"),
        ("RUSTYBIN_QUOTA_FLUSH_MS", "500"),
        ("RUSTYBIN_QUOTA_UPGRADE_URL", "https://example.com/pricing"),
    ]
    .into_iter()
    .collect();
    let (c, w) = Config::from_lookup(|k| map.get(k).map(|v| v.to_string()));
    assert!(w.is_empty(), "{w:?}");
    assert!(c.shared_mode && c.public_mode);
    assert_eq!(c.limits.scope, Scope::Session);
    assert_eq!(c.limits.period, Period::Day);
    assert_eq!(c.keys_poll_secs, 10);
    assert_eq!(c.keys_max, 5000);
    assert_eq!(c.quota_flush_ms, 500);
    let view = c.public_view().to_string();
    assert!(
        !view.contains("reg-token") && !view.contains("secret"),
        "{view}"
    );
    assert!(view.contains("\"keys_source\":\"url\""));

    let (c, w) = Config::from_lookup(|k| (k == "RUSTYBIN_REQUIRE_KEY").then(|| "1".to_string()));
    assert!(c.require_key && !c.public_mode);
    assert_eq!(w.len(), 1, "no registry source: {w:?}");
    let (c, _) = Config::from_lookup(|_| None);
    assert!(!c.shared_mode && !c.require_key);
    assert_eq!(c.keys_poll_secs, 30);
    assert_eq!(c.keys_max, 200_000);
    assert_eq!(c.quota_flush_ms, 2_000);
}
