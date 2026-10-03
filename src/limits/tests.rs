//! Plan limit tests: the limiter itself (manual clock) and the middleware
//! through the full application.

use super::*;
use crate::config::Config;
use crate::test_support::{
    body_json, body_string, get_request, test_app, test_app_with, test_state_with,
};
use axum::http::Request as HttpRequest;
use tower::ServiceExt;

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .expect("timestamp")
        .with_timezone(&Utc)
}

fn custom(f: impl FnOnce(&mut LimitsConfig)) -> LimitsConfig {
    let mut cfg = LimitsConfig {
        plan: Plan::Pro,
        ..LimitsConfig::default()
    };
    f(&mut cfg);
    cfg
}

fn manual(cfg: LimitsConfig, now: &str) -> (Limiter, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(at(now)));
    (Limiter::with_clock(cfg, clock.clone()), clock)
}

fn app_with_limits(limits: LimitsConfig) -> axum::Router {
    let mut config = Config::for_tests();
    config.limits = limits;
    test_app_with(test_state_with(config))
}

fn header<'a>(resp: &'a Response, name: &str) -> Option<&'a str> {
    resp.headers().get(name).and_then(|v| v.to_str().ok())
}

async fn get_with(app: &axum::Router, uri: &str, headers: &[(&str, &str)]) -> Response {
    let mut b = HttpRequest::builder().uri(uri);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    app.clone()
        .oneshot(b.body(Body::empty()).expect("request"))
        .await
        .expect("response")
}

// ── Limiter ─────────────────────────────────────────────────────────

#[test]
fn token_bucket_refills_at_the_configured_rate() {
    let (limiter, clock) = manual(
        custom(|c| {
            c.rps = 2;
            c.burst = 3;
        }),
        "2026-03-10T12:00:00Z",
    );
    let c = limiter.counters(None);
    for _ in 0..3 {
        assert!(limiter.admit(&c, false, false).is_ok());
    }
    let rejected = limiter.admit(&c, false, false).expect_err("bucket empty");
    assert_eq!(rejected.dimension, Dimension::Rps);
    assert_eq!(rejected.retry_after_secs, 1);
    clock.advance(Duration::from_millis(500)); // one token at 2 rps
    assert!(limiter.admit(&c, false, false).is_ok());
    assert!(limiter.admit(&c, false, false).is_err());
    clock.advance(Duration::from_secs(60)); // refills to the burst, not beyond
    for _ in 0..3 {
        assert!(limiter.admit(&c, false, false).is_ok());
    }
    assert!(limiter.admit(&c, false, false).is_err());
    // Burst 0 means "same as rps".
    let (limiter, _) = manual(custom(|c| c.rps = 2), "2026-03-10T12:00:00Z");
    let c = limiter.counters(None);
    assert!(limiter.admit(&c, false, false).is_ok());
    assert!(limiter.admit(&c, false, false).is_ok());
    assert!(limiter.admit(&c, false, false).is_err());
}

#[test]
fn concurrency_slots_are_released_on_drop() {
    let (limiter, _) = manual(custom(|c| c.concurrency = 2), "2026-03-10T12:00:00Z");
    let c = limiter.counters(None);
    let a = limiter.admit(&c, false, false).expect("first");
    let b = limiter.admit(&c, false, false).expect("second");
    assert_eq!(c.in_flight(), 2);
    let rejected = limiter.admit(&c, false, false).expect_err("third");
    assert_eq!(rejected.dimension, Dimension::Concurrency);
    drop(a);
    let _c = limiter.admit(&c, false, false).expect("slot freed");
    drop(b);
    // Admin control plane requests are never rejected, but still occupy a slot.
    let _d = limiter.admit(&c, true, false).expect("bypass");
    assert_eq!(c.in_flight(), 2);
}

#[test]
fn stream_cap_counts_open_leases() {
    let (limiter, _) = manual(custom(|c| c.streams = 1), "2026-03-10T12:00:00Z");
    let c = limiter.counters(None);
    let first = limiter.admit(&c, false, true).expect("first stream");
    let lease = first.stream.clone().expect("lease");
    drop(first);
    // The lease (held by the WebSocket task) keeps the slot.
    assert_eq!(c.open_streams(), 1);
    let rejected = limiter.admit(&c, false, true).expect_err("second stream");
    assert_eq!(rejected.dimension, Dimension::Streams);
    // Plain requests are not affected by the stream cap.
    assert!(limiter.admit(&c, false, false).is_ok());
    drop(lease);
    assert_eq!(c.open_streams(), 0);
    assert!(limiter.admit(&c, false, true).is_ok());
}

#[test]
fn quota_period_rolls_over_at_midnight_and_month_end() {
    let (limiter, clock) = manual(
        custom(|c| {
            c.period = Period::Day;
            c.requests = 2;
        }),
        "2026-01-31T23:59:50Z",
    );
    let c = limiter.counters(None);
    assert_eq!(
        limiter
            .admit(&c, false, false)
            .expect("1")
            .requests_remaining,
        Some(1)
    );
    assert_eq!(
        limiter
            .admit(&c, false, false)
            .expect("2")
            .requests_remaining,
        Some(0)
    );
    let rejected = limiter.admit(&c, false, false).expect_err("quota");
    assert_eq!(rejected.dimension, Dimension::Requests);
    assert_eq!(rejected.retry_after_secs, 10);
    clock.advance(Duration::from_secs(15));
    assert_eq!(
        limiter
            .admit(&c, false, false)
            .expect("new day")
            .requests_remaining,
        Some(1)
    );
    let usage = limiter.usage(None);
    assert_eq!(usage["period"]["start"], "2026-02-01T00:00:00Z");
    assert_eq!(usage["period"]["end"], "2026-02-02T00:00:00Z");
    assert_eq!(usage["requests"]["used"], 1);

    assert_eq!(
        Period::Month.end(at("2026-12-31T23:59:59Z")),
        at("2027-01-01T00:00:00Z")
    );
    assert_eq!(
        Period::Month.start(at("2026-02-15T08:00:00Z")),
        at("2026-02-01T00:00:00Z")
    );
    let (limiter, clock) = manual(
        custom(|c| {
            c.period = Period::Month;
            c.egress_bytes = 100;
        }),
        "2026-02-28T23:00:00Z",
    );
    let c = limiter.counters(None);
    c.add_egress(150);
    let rejected = limiter.admit(&c, false, false).expect_err("egress");
    assert_eq!(rejected.dimension, Dimension::Egress);
    assert_eq!(rejected.retry_after_secs, 3600);
    clock.advance(Duration::from_secs(3600));
    assert!(limiter.admit(&c, false, false).is_ok());
    assert_eq!(limiter.usage(None)["egress"]["used_bytes"], 0);
}

#[test]
fn usage_file_round_trip_and_new_period_reset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("usage.json");
    let cfg = custom(|c| {
        c.requests = 1000;
        c.usage_file = Some(path.clone());
    });
    let (limiter, _) = manual(cfg.clone(), "2026-05-20T10:00:00Z");
    let c = limiter.counters(None);
    for _ in 0..3 {
        drop(limiter.admit(&c, false, false).expect("admit"));
    }
    c.add_egress(4096);
    limiter.save();
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("file")).expect("json");
    assert_eq!(saved["requests"], 3);
    assert_eq!(saved["egress_bytes"], 4096);
    assert_eq!(saved["period"], "month");
    assert_eq!(saved["period_start"], "2026-05-01T00:00:00Z");

    // A restart in the same month restores the counters.
    let (restarted, _) = manual(cfg.clone(), "2026-05-31T23:00:00Z");
    let usage = restarted.usage(None);
    assert_eq!(usage["requests"]["used"], 3);
    assert_eq!(usage["requests"]["remaining"], 997);
    assert_eq!(usage["egress"]["used_bytes"], 4096);
    assert_eq!(usage["persisted"], true);

    // A restart in the next month starts from zero.
    let (next_month, _) = manual(cfg, "2026-06-01T00:00:01Z");
    assert_eq!(next_month.usage(None)["requests"]["used"], 0);

    // Session scope is never persisted.
    let session_cfg = custom(|c| {
        c.scope = Scope::Session;
        c.usage_file = Some(dir.path().join("sessions.json"));
    });
    let (sessions, _) = manual(session_cfg, "2026-05-20T10:00:00Z");
    drop(sessions.admit(&sessions.counters(Some("a")), false, false));
    sessions.save();
    assert!(!dir.path().join("sessions.json").exists());
}

#[test]
fn session_scope_isolates_counters_and_the_map_is_bounded() {
    let (limiter, _) = manual(
        custom(|c| {
            c.scope = Scope::Session;
            c.rps = 1;
        }),
        "2026-03-10T12:00:00Z",
    );
    let a = limiter.counters(Some("alice"));
    let b = limiter.counters(Some("bob"));
    assert!(limiter.admit(&a, false, false).is_ok());
    assert!(limiter.admit(&a, false, false).is_err());
    assert!(limiter.admit(&b, false, false).is_ok());
    assert_eq!(limiter.usage(Some("alice"))["requests"]["used"], 1);
    assert_eq!(limiter.usage(Some("alice"))["session"], "alice");
    assert_eq!(limiter.usage(Some("carol"))["requests"]["used"], 0);
    // usage() never creates counters.
    assert_eq!(limiter.session_count(), 2);
    for i in 0..MAX_SESSIONS + 5 {
        limiter.counters(Some(&format!("s{i}")));
    }
    assert!(limiter.session_count() <= MAX_SESSIONS);
}

#[test]
fn grpc_calls_are_rate_limited() {
    let (limiter, _) = manual(custom(|c| c.rps = 1), "2026-03-10T12:00:00Z");
    assert!(limiter.check_grpc(&tonic::Request::new(())).is_ok());
    let status = limiter
        .check_grpc(&tonic::Request::new(()))
        .expect_err("limited");
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    assert!(status.message().contains("not from your gateway"));
    assert_eq!(
        status
            .metadata()
            .get(LIMIT_HEADER)
            .and_then(|v| v.to_str().ok()),
        Some("rps")
    );
    // No plan: always allowed.
    let (none, _) = manual(LimitsConfig::default(), "2026-03-10T12:00:00Z");
    for _ in 0..10 {
        assert!(none.check_grpc(&tonic::Request::new(())).is_ok());
    }
}

// ── Middleware ──────────────────────────────────────────────────────

#[tokio::test]
async fn plan_none_adds_nothing() {
    let app = test_app();
    for _ in 0..30 {
        let resp = app
            .clone()
            .oneshot(get_request("/echo"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(header(&resp, PLAN_HEADER).is_none());
        assert!(header(&resp, QUOTA_REMAINING_HEADER).is_none());
    }
    let preflight = HttpRequest::builder()
        .method("OPTIONS")
        .uri("/echo")
        .header("origin", "https://app.example")
        .header("access-control-request-method", "GET")
        .body(Body::empty())
        .expect("request");
    let resp = app.clone().oneshot(preflight).await.expect("response");
    let expose = header(&resp, "access-control-expose-headers").unwrap_or_default();
    assert!(!expose.contains("ratelimit"));
    let usage = body_json(app.oneshot(get_request(USAGE_PATH)).await.expect("usage")).await;
    assert_eq!(usage["plan"], "none");
    assert_eq!(usage["active"], false);
    assert_eq!(usage["limits"]["rps"], Value::Null);
}

#[tokio::test]
async fn rejection_headers_and_body() {
    let app = app_with_limits(custom(|c| {
        c.plan = Plan::Free;
        c.rps = 1;
        c.burst = 2;
        c.requests = 100;
        c.period = Period::Day;
    }));
    let ok = get_with(&app, "/echo", &[]).await;
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(header(&ok, PLAN_HEADER), Some("free"));
    assert_eq!(header(&ok, QUOTA_REMAINING_HEADER), Some("99"));
    assert!(header(&ok, "ratelimit").is_none());
    let ok = get_with(&app, "/echo", &[]).await;
    assert_eq!(header(&ok, QUOTA_REMAINING_HEADER), Some("98"));

    let resp = get_with(&app, "/echo", &[]).await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(header(&resp, "retry-after"), Some("1"));
    assert_eq!(header(&resp, LIMIT_HEADER), Some("rps"));
    assert_eq!(header(&resp, PLAN_HEADER), Some("free"));
    assert_eq!(header(&resp, "ratelimit"), Some("\"rps\";r=0;t=1"));
    assert_eq!(
        header(&resp, "ratelimit-policy"),
        Some("\"rps\";q=1;w=1;rustybin-burst=2, \"requests\";q=100;w=86400")
    );
    assert!(
        header(&resp, "x-request-id").is_some(),
        "request id stays first"
    );
    let body = body_json(resp).await;
    assert_eq!(body["error"], ERROR_CODE);
    assert_eq!(body["limit"], "rps");
    assert_eq!(body["plan"], "free");
    assert!(body["message"]
        .as_str()
        .unwrap_or_default()
        .starts_with("This 429 comes from the Rustybin plan limit, not from your gateway."));

    // Rejected requests are not captured by the inspector and never reach
    // fault injection.
    let resp = get_with(&app, "/echo", &[("x-rustybin-fail", "503")]).await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(header(&resp, "x-rustybin-fault").is_none());
}

#[tokio::test]
async fn exempt_routes_and_admin_bypass() {
    let mut config = Config::for_tests();
    config.admin_token = Some("tok".into());
    config.limits = custom(|c| {
        c.rps = 1;
        c.requests = 1000;
    });
    let app = test_app_with(test_state_with(config));
    assert_eq!(get_with(&app, "/echo", &[]).await.status(), StatusCode::OK);
    assert_eq!(
        get_with(&app, "/echo", &[]).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    for path in ["/", "/ui/", USAGE_PATH, "/", USAGE_PATH] {
        let resp = get_with(&app, path, &[]).await;
        assert!(resp.status().is_success(), "{path}: {}", resp.status());
        assert!(
            header(&resp, PLAN_HEADER).is_none(),
            "{path} is not decorated"
        );
    }
    // Admin token: the control plane stays reachable (and is counted).
    let resp = get_with(
        &app,
        "/_rustybin/config",
        &[("authorization", "Bearer tok")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = get_with(&app, "/_rustybin/config", &[]).await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let resp = get_with(&app, "/echo", &[("authorization", "Bearer tok")]).await;
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "data plane is not bypassed"
    );
    let usage = body_json(get_with(&app, USAGE_PATH, &[]).await).await;
    assert_eq!(
        usage["requests"]["used"], 2,
        "exempt routes are not counted"
    );
}

#[tokio::test]
async fn session_scope_through_the_app() {
    let app = app_with_limits(custom(|c| {
        c.plan = Plan::Free;
        c.scope = Scope::Session;
        c.rps = 1;
    }));
    let a = [("x-rustybin-session", "alice")];
    let b = [("x-rustybin-session", "bob")];
    assert_eq!(get_with(&app, "/echo", &a).await.status(), StatusCode::OK);
    assert_eq!(
        get_with(&app, "/echo", &a).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(get_with(&app, "/echo", &b).await.status(), StatusCode::OK);
    // No session header: the client IP is the key.
    assert_eq!(get_with(&app, "/echo", &[]).await.status(), StatusCode::OK);
    let usage = body_json(get_with(&app, USAGE_PATH, &a).await).await;
    assert_eq!(usage["session"], "alice");
    assert_eq!(usage["requests"]["used"], 1);
    assert_eq!(usage["rate"]["available"], 0);
    let usage = body_json(get_with(&app, USAGE_PATH, &[]).await).await;
    assert_eq!(usage["session"], "ip:127.0.0.1");
}

#[tokio::test]
async fn egress_is_counted_and_never_cuts_a_response() {
    let app = app_with_limits(custom(|c| c.egress_bytes = 1500));
    for _ in 0..2 {
        let resp = get_with(&app, "/bytes/1000", &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(crate::test_support::body_bytes(resp).await.len(), 1000);
    }
    let resp = get_with(&app, "/bytes/1000", &[]).await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(header(&resp, LIMIT_HEADER), Some("egress"));
    assert!(header(&resp, "ratelimit-policy")
        .unwrap_or_default()
        .contains("\"egress\";q=1500;qu=\"content-bytes\""));
    let usage = body_json(get_with(&app, USAGE_PATH, &[]).await).await;
    assert_eq!(usage["egress"]["used_bytes"], 2000);
    assert_eq!(usage["egress"]["remaining_bytes"], 0);
}

#[tokio::test]
async fn content_length_is_kept() {
    let app = app_with_limits(custom(|c| c.rps = 100));
    let resp = get_with(&app, "/bytes/123", &[]).await;
    assert_eq!(resp.body().size_hint().exact(), Some(123));
}

#[tokio::test]
async fn sse_streams_are_capped_and_ended_at_the_lifetime() {
    let app = app_with_limits(custom(|c| {
        c.streams = 1;
        c.stream_lifetime = Duration::from_millis(300);
    }));
    let first = get_with(&app, "/sse?count=50&interval_ms=100", &[]).await;
    assert_eq!(first.status(), StatusCode::OK);
    let second = get_with(&app, "/sse?count=50&interval_ms=100", &[]).await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(header(&second, LIMIT_HEADER), Some("streams"));
    // Plain requests still pass while the stream is open.
    assert_eq!(get_with(&app, "/echo", &[]).await.status(), StatusCode::OK);

    let started = std::time::Instant::now();
    let text = tokio::time::timeout(Duration::from_secs(5), body_string(first))
        .await
        .expect("the stream ends at the lifetime");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(text.ends_with(std::str::from_utf8(STREAM_END_COMMENT).unwrap_or_default()));
    assert!(text.contains("data:"));
    let usage = body_json(get_with(&app, USAGE_PATH, &[]).await).await;
    assert_eq!(usage["open_streams"]["current"], 0);
    assert_eq!(usage["in_flight"]["current"], 0);
    let third = get_with(&app, "/sse?count=1&interval_ms=10", &[]).await;
    assert_eq!(third.status(), StatusCode::OK);
}

#[tokio::test]
async fn concurrency_through_the_app() {
    let app = app_with_limits(custom(|c| c.concurrency = 1));
    // A slow body keeps its slot until it is fully sent.
    let slow = get_with(&app, "/drip?duration=1&numbytes=4&delay=0", &[]).await;
    assert_eq!(slow.status(), StatusCode::OK);
    let blocked = get_with(&app, "/echo", &[]).await;
    assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(header(&blocked, LIMIT_HEADER), Some("concurrency"));
    assert_eq!(header(&blocked, "ratelimit"), Some("\"concurrency\";r=0"));
    drop(crate::test_support::body_bytes(slow).await);
    assert_eq!(get_with(&app, "/echo", &[]).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn websocket_streams_close_with_1008_at_the_lifetime() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use tokio_tungstenite::tungstenite::Message as TMessage;

    let app = app_with_limits(custom(|c| {
        c.streams = 1;
        c.stream_lifetime = Duration::from_millis(300);
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let url = format!("ws://{addr}/ws");
    let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .expect("connect");
    // The open socket holds the only stream slot.
    let second = tokio_tungstenite::connect_async(url.as_str()).await;
    match second {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        }
        other => panic!("expected a 429, got {:?}", other.map(|_| ())),
    }
    let close = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(msg) = ws.next().await {
            if let Ok(TMessage::Close(frame)) = msg {
                return frame;
            }
        }
        None
    })
    .await
    .expect("closed in time")
    .expect("close frame");
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(&*close.reason, STREAM_END_REASON);
    drop(ws);
    // The slot is free again.
    let mut reopened = None;
    for _ in 0..50 {
        if let Ok((ws, _)) = tokio_tungstenite::connect_async(url.as_str()).await {
            reopened = Some(ws);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(reopened.is_some());
}

#[tokio::test]
async fn usage_file_is_written_on_graceful_shutdown() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("usage.json");
    let mut config = Config::for_tests();
    config.limits = custom(|c| {
        c.requests = 100;
        c.usage_file = Some(path.clone());
    });
    let server = crate::start_with_state(crate::AppState::for_tests(config))
        .await
        .expect("server");
    let mut conn = tokio::net::TcpStream::connect(server.http_addr)
        .await
        .expect("connect");
    conn.write_all(b"GET /bytes/10 HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let mut raw = Vec::new();
    conn.read_to_end(&mut raw).await.expect("read");
    assert!(raw.starts_with(b"HTTP/1.1 200"));
    server.shutdown();
    server.wait().await.expect("clean shutdown");
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("usage file")).expect("json");
    assert_eq!(saved["requests"], 1);
    assert_eq!(saved["egress_bytes"], 10);
}
