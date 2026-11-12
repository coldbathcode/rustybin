//! Saved tests on a schedule: on time (a manual clock), never two runs at
//! once, skipped while the harness sends load, one notice per failing run
//! and one on recovery when asked, an egress refusal reported and never
//! retried, the cap, the rules, the routes, a restart and the document.

use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::limits::ManualClock;
use schedule::{Schedule, Schedules};

fn t0() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-11-03T10:00:30Z")
        .expect("time")
        .with_timezone(&chrono::Utc)
}

fn every(name: &str, e: &str) -> Schedule {
    Schedule {
        name: name.into(),
        description: None,
        names: Vec::new(),
        prefix: None,
        every: Some(e.into()),
        at: None,
        enabled: true,
        webhook: None,
        notify_recovery: false,
    }
}

fn test_of(name: &str, path: &str, status: u16) -> SavedTest {
    SavedTest {
        name: name.into(),
        description: None,
        request: TestRequest {
            method: "GET".into(),
            path: path.into(),
            headers: Default::default(),
            body: None,
        },
        steps: Vec::new(),
        gateway: None,
        assert: vec![Assertion::status(status)],
        project: None,
    }
}

/// A state with its app built (in-process requests), a manual clock.
fn bench(c: Config) -> (AppState, Router, Arc<ManualClock>) {
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    let clock = Arc::new(ManualClock::new(t0()));
    state.saved_tests.schedules.set_clock(clock.clone());
    (state, app, clock)
}

async fn settle(handles: Vec<tokio::task::JoinHandle<()>>) -> usize {
    let n = handles.len();
    for h in handles {
        h.await.expect("run task");
    }
    n
}

fn status_of(state: &AppState, name: &str) -> Value {
    state.saved_tests.schedules.view_of(name).expect("schedule")["status"].clone()
}

#[test]
fn the_rules_of_a_schedule() {
    for ok in ["5m", "300s", "15m", "2h", "24h", "1440m"] {
        assert!(every("s", ok).validate().is_ok(), "{ok}");
    }
    for bad in [
        "4m", "299s", "25h", "1441m", "15", "m", "1.5h", "15x", "", "-5m", "\u{e9}5m",
    ] {
        let e = every("s", bad).validate().expect_err(bad);
        assert!(e.contains("every"), "{bad}: {e}");
    }
    let mut s = every("s", "5m");
    s.every = None;
    for ok in ["00:00", "02:30", "23:59"] {
        s.at = Some(ok.into());
        assert!(s.validate().is_ok(), "{ok}");
    }
    for bad in ["24:00", "2:30", "02:60", "0230", "02:3a", "02:30Z"] {
        s.at = Some(bad.into());
        assert!(s.validate().expect_err(bad).contains("HH:MM"), "{bad}");
    }
    s.at = None;
    assert!(s.validate().expect_err("neither").contains("one of every"));
    let mut both = every("s", "5m");
    both.at = Some("02:00".into());
    assert!(both.validate().is_err());
    for bad in ["", "a b", "x/y", &"n".repeat(65)] {
        assert!(every(bad, "5m").validate().is_err(), "{bad:?}");
    }
    let mut sel = every("s", "5m");
    sel.names = vec!["a".into()];
    sel.prefix = Some("a".into());
    assert!(sel
        .validate()
        .expect_err("both")
        .contains("names or prefix"));
    sel.prefix = None;
    sel.names = vec!["a".into(), "a".into()];
    assert!(sel.validate().is_err(), "a name twice");
    let mut hook = every("s", "5m");
    for bad in [
        "ftp://hooks.example.com/x",
        "https://user:pw@hooks.example.com/x",
        "https://hooks.example.com/x?token=abc",
        "not a url",
    ] {
        hook.webhook = Some(bad.into());
        assert!(hook.validate().expect_err(bad).contains("webhook"), "{bad}");
    }
    hook.webhook = Some("https://hooks.example.com/rustybin".into());
    assert!(hook.validate().is_ok());
    // A test cannot be named after the route.
    assert!(test_of("schedules", "/get", 200).validate().is_err());
}

#[test]
fn every_aligns_to_the_epoch_and_at_is_a_time_of_day() {
    let s = every("s", "5m");
    assert_eq!(
        s.next_after(t0()).map(|t| t.to_rfc3339()),
        Some("2026-11-03T10:05:00+00:00".into())
    );
    // Exactly on a slot: the next one.
    let on = t0() - chrono::Duration::seconds(30);
    assert_eq!(
        s.next_after(on).map(|t| t.to_rfc3339()),
        Some("2026-11-03T10:05:00+00:00".into())
    );
    let mut d = every("d", "5m");
    d.every = None;
    d.at = Some("09:30".into());
    assert_eq!(
        d.next_after(t0()).map(|t| t.to_rfc3339()),
        Some("2026-11-04T09:30:00+00:00".into())
    );
    d.at = Some("10:01".into());
    assert_eq!(
        d.next_after(t0()).map(|t| t.to_rfc3339()),
        Some("2026-11-03T10:01:00+00:00".into())
    );
}

#[test]
fn at_most_twenty_schedules() {
    let s = Schedules::new(None);
    for i in 0..schedule::MAX_SCHEDULES {
        s.put(every(&format!("s{i}"), "1h"), false).expect("put");
    }
    let e = s.put(every("one-more", "1h"), false).expect_err("full");
    assert_eq!(e.status(), StatusCode::INSUFFICIENT_STORAGE);
    // Replacing one is not a new one.
    s.put(every("s0", "2h"), true).expect("replace");
    assert!(matches!(
        s.put(every("s0", "2h"), false),
        Err(schedule::ScheduleError::Exists(_))
    ));
}

#[tokio::test]
async fn a_schedule_runs_on_time_and_lands_in_the_history() {
    let (state, _app, clock) = bench(Config::for_tests());
    state
        .saved_tests
        .put(test_of("ok", "/status/200", 200), false)
        .expect("test");
    let sched = &state.saved_tests.schedules;
    sched.put(every("nightly", "5m"), false).expect("put");
    assert_eq!(
        status_of(&state, "nightly")["next_due"],
        json!("2026-11-03T10:05:00Z")
    );
    assert_eq!(settle(schedule::tick(&state)).await, 0, "not due yet");
    clock.advance(Duration::from_secs(4 * 60 + 29));
    assert_eq!(settle(schedule::tick(&state)).await, 0, "a second early");
    clock.advance(Duration::from_secs(1));
    assert_eq!(settle(schedule::tick(&state)).await, 1, "on time");
    let st = status_of(&state, "nightly");
    assert_eq!(st["last_result"], "passed", "{st}");
    assert_eq!(st["runs"], 1);
    assert_eq!(st["running"], false);
    assert_eq!(st["next_due"], json!("2026-11-03T10:10:00Z"));
    let id = st["last_run_id"].as_str().expect("run id").to_string();
    let run = state.saved_tests.history.get(&id).expect("kept");
    assert_eq!(run.trigger, "schedule:nightly");
    assert_eq!(run.principal, "scheduler");
    // Due once: the same moment starts nothing more.
    assert_eq!(settle(schedule::tick(&state)).await, 0);
    // Missed slots collapse into one run.
    clock.advance(Duration::from_secs(3 * 3600));
    assert_eq!(settle(schedule::tick(&state)).await, 1);
    assert_eq!(settle(schedule::tick(&state)).await, 0);
    assert_eq!(status_of(&state, "nightly")["runs"], 2);
    // Off: never due.
    let mut off = every("nightly", "5m");
    off.enabled = false;
    sched.put(off, true).expect("off");
    assert!(status_of(&state, "nightly")["next_due"].is_null());
    clock.advance(Duration::from_secs(3600));
    assert_eq!(settle(schedule::tick(&state)).await, 0);
    assert_eq!(status_of(&state, "nightly")["runs"], 2, "the status stays");
}

#[tokio::test]
async fn a_run_never_overlaps_another() {
    let (state, app, clock) = bench(Config::for_tests());
    state
        .saved_tests
        .put(test_of("ok", "/status/200", 200), false)
        .expect("test");
    state
        .saved_tests
        .schedules
        .put(every("nightly", "5m"), false)
        .expect("put");
    clock.advance(Duration::from_secs(300));
    // Another run of the saved tests goes: the due run waits.
    let held = state.saved_tests.begin_run().expect("guard");
    assert_eq!(settle(schedule::tick(&state)).await, 0);
    assert_eq!(status_of(&state, "nightly")["runs"], 0);
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/tests/schedules/nightly/run",
        "ops",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    drop(held);
    // Still due: it starts at the next tick, once.
    assert_eq!(settle(schedule::tick(&state)).await, 1);
    assert_eq!(status_of(&state, "nightly")["runs"], 1);
    // While the scheduled run goes, a run by hand is refused.
    clock.advance(Duration::from_secs(300));
    let started = schedule::tick(&state);
    assert_eq!(started.len(), 1);
    assert_eq!(status_of(&state, "nightly")["running"], true);
    let e = schedule::run_now(&state, "nightly")
        .await
        .err()
        .expect("busy");
    assert_eq!(e.0, StatusCode::CONFLICT);
    settle(started).await;
    assert_eq!(status_of(&state, "nightly")["running"], false);
}

#[tokio::test]
async fn a_run_due_while_the_harness_sends_load_is_skipped() {
    let (state, _app, clock) = bench(Config::for_tests());
    state
        .saved_tests
        .put(test_of("ok", "/status/200", 200), false)
        .expect("test");
    let sched = &state.saved_tests.schedules;
    sched.put(every("nightly", "5m"), false).expect("put");
    sched
        .force_load
        .store(true, std::sync::atomic::Ordering::SeqCst);
    clock.advance(Duration::from_secs(300));
    assert_eq!(settle(schedule::tick(&state)).await, 0);
    let st = status_of(&state, "nightly");
    assert_eq!(st["last_result"], "skipped", "{st}");
    assert_eq!(st["skipped"], 1);
    assert_eq!(st["runs"], 0);
    assert!(st["last_detail"].as_str().unwrap_or("").contains("harness"));
    assert_eq!(st["recent"][0]["result"], "skipped");
    assert_eq!(st["next_due"], json!("2026-11-03T10:10:00Z"));
    assert!(state.saved_tests.history.is_empty(), "nothing ran");
    let e = schedule::run_now(&state, "nightly")
        .await
        .err()
        .expect("load");
    assert_eq!(e.0, StatusCode::CONFLICT);
    // No test to run is a skip too.
    sched
        .force_load
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut p = every("nightly", "5m");
    p.prefix = Some("nothing-".into());
    sched.put(p, true).expect("put");
    clock.advance(Duration::from_secs(300));
    assert_eq!(settle(schedule::tick(&state)).await, 1);
    let st = status_of(&state, "nightly");
    assert_eq!(st["last_result"], "skipped", "{st}");
    assert_eq!(st["skipped"], 2);
}

/// A webhook on this machine: every body and its Authorization header.
async fn receiver() -> (String, Arc<std::sync::Mutex<Vec<(String, Value)>>>) {
    let got: Arc<std::sync::Mutex<Vec<(String, Value)>>> = Arc::default();
    let seen = got.clone();
    // Not a route of the instance (the catalogue's scan reads literals).
    let hook = "/hook";
    let app = axum::Router::new().route(
        hook,
        axum::routing::post(
            move |headers: axum::http::HeaderMap, axum::Json(v): axum::Json<Value>| {
                let seen = seen.clone();
                async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|h| h.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    seen.lock().expect("lock").push((auth, v));
                    "ok"
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/hook"), got)
}

fn loopback_allowed() -> Config {
    let mut c = Config::for_tests();
    c.egress_allow = vec!["127.0.0.1".into()];
    c.egress_allow_loopback = true;
    c.notify_webhook_token = Some("hook-secret-token".into());
    c
}

#[tokio::test]
async fn a_failure_sends_one_notice_and_a_recovery_one_more_when_asked() {
    let (state, _app, _clock) = bench(loopback_allowed());
    let (url, got) = receiver().await;
    state
        .settings
        .update(
            [(
                "notify.base_url".to_string(),
                json!("https://mocks.example.com"),
            )]
            .into(),
            Default::default(),
            "test",
        )
        .expect("base url");
    state
        .saved_tests
        .put(test_of("flips", "/status/200", 201), false)
        .expect("test");
    let mut s = every("nightly", "1h");
    s.webhook = Some(url.clone());
    s.notify_recovery = true;
    state.saved_tests.schedules.put(s, false).expect("put");
    let count = || got.lock().expect("lock").len();

    // A failing run: one notice.
    let ran = schedule::run_now(&state, "nightly").await.expect("run");
    let run_id = match ran {
        schedule::Ran::Kept(r) => {
            assert!(!r.report.passed);
            r.id.clone()
        }
        _ => panic!("kept"),
    };
    assert_eq!(
        status_of(&state, "nightly")["last_notice"]["result"],
        "pending"
    );
    assert_eq!(schedule::deliver_due(&state).await, 1);
    assert_eq!(count(), 1);
    {
        let g = got.lock().expect("lock");
        let (auth, body) = &g[0];
        assert_eq!(auth, "Bearer hook-secret-token");
        assert_eq!(body["event"], "test_schedule.failed", "{body}");
        assert_eq!(body["schedule"], "nightly");
        assert_eq!(body["failing"], json!(["flips"]));
        assert_eq!(body["failed"], 1);
        assert_eq!(body["run_id"], json!(run_id));
        assert_eq!(
            body["run_path"],
            json!(format!("/_rustybin/tests/runs/{run_id}"))
        );
        assert_eq!(
            body["run_url"],
            json!(format!(
                "https://mocks.example.com/_rustybin/tests/runs/{run_id}"
            ))
        );
        // What a test saw is never in a notice.
        assert!(!body.to_string().contains("\"seen\""), "{body}");
    }
    let st = status_of(&state, "nightly");
    assert_eq!(st["last_notice"]["result"], "delivered", "{st}");
    assert_eq!(st["last_notice"]["event"], "failed");
    assert_eq!(st["last_notice"]["attempts"], 1);
    assert_eq!(schedule::deliver_due(&state).await, 0, "sent once");

    // Failing again: one notice per failing run.
    schedule::run_now(&state, "nightly").await.expect("run");
    schedule::deliver_due(&state).await;
    assert_eq!(count(), 2);

    // Passing after a failure: the recovery.
    state
        .saved_tests
        .put(test_of("flips", "/status/200", 200), true)
        .expect("fix");
    schedule::run_now(&state, "nightly").await.expect("run");
    schedule::deliver_due(&state).await;
    assert_eq!(count(), 3);
    assert_eq!(
        got.lock().expect("lock")[2].1["event"],
        "test_schedule.recovered"
    );
    // Passing again: nothing.
    schedule::run_now(&state, "nightly").await.expect("run");
    schedule::deliver_due(&state).await;
    assert_eq!(count(), 3);

    // Without notify_recovery a pass after a failure says nothing.
    let mut quiet = every("nightly", "1h");
    quiet.webhook = Some(url);
    state.saved_tests.schedules.put(quiet, true).expect("put");
    state
        .saved_tests
        .put(test_of("flips", "/status/200", 201), true)
        .expect("break");
    schedule::run_now(&state, "nightly").await.expect("run");
    state
        .saved_tests
        .put(test_of("flips", "/status/200", 200), true)
        .expect("fix");
    schedule::run_now(&state, "nightly").await.expect("run");
    schedule::deliver_due(&state).await;
    assert_eq!(count(), 4, "the failure only");
}

#[tokio::test]
async fn an_egress_refusal_is_reported_and_never_retried() {
    // No allowlist entry: the purpose notify has no default.
    let (state, _app, clock) = bench(Config::for_tests());
    state
        .saved_tests
        .put(test_of("fails", "/status/200", 201), false)
        .expect("test");
    let mut s = every("nightly", "1h");
    s.webhook = Some("http://127.0.0.1:9/hook".into());
    state.saved_tests.schedules.put(s, false).expect("put");
    schedule::run_now(&state, "nightly").await.expect("run");
    assert_eq!(schedule::deliver_due(&state).await, 1);
    let st = status_of(&state, "nightly");
    assert_eq!(st["last_notice"]["result"], "refused", "{st}");
    assert!(
        st["last_notice"]["detail"]
            .as_str()
            .unwrap_or("")
            .contains("notify"),
        "{st}"
    );
    assert_eq!(state.saved_tests.schedules.pending_notices(), 0);
    clock.advance(Duration::from_secs(3600));
    assert_eq!(schedule::deliver_due(&state).await, 0, "never retried");
    assert!(state
        .egress
        .decisions()
        .iter()
        .any(|d| d.purpose == crate::egress::Purpose::Notify && !d.allowed));
}

#[tokio::test]
async fn a_failed_delivery_is_retried_with_a_backoff_then_given_up() {
    let (state, _app, clock) = bench(loopback_allowed());
    state
        .saved_tests
        .put(test_of("fails", "/status/200", 201), false)
        .expect("test");
    let mut s = every("nightly", "1h");
    // Nothing listens on port 1.
    s.webhook = Some("http://127.0.0.1:1/hook".into());
    state.saved_tests.schedules.put(s, false).expect("put");
    schedule::run_now(&state, "nightly").await.expect("run");
    let sched = &state.saved_tests.schedules;
    assert_eq!(schedule::deliver_due(&state).await, 1);
    let st = status_of(&state, "nightly");
    assert_eq!(st["last_notice"]["result"], "retrying", "{st}");
    assert_eq!(sched.pending_notices(), 1);
    assert_eq!(schedule::deliver_due(&state).await, 0, "backed off");
    for wait in [15, 60, 240] {
        clock.advance(Duration::from_secs(wait));
        assert_eq!(schedule::deliver_due(&state).await, 1, "after {wait} s");
    }
    let st = status_of(&state, "nightly");
    assert_eq!(st["last_notice"]["result"], "failed", "{st}");
    assert_eq!(st["last_notice"]["attempts"], schedule::NOTICE_ATTEMPTS);
    assert_eq!(sched.pending_notices(), 0, "given up");
    // The queue is bounded: a flood of failing runs keeps at most the cap.
    for _ in 0..schedule::MAX_NOTICES + 3 {
        schedule::run_now(&state, "nightly").await.expect("run");
    }
    assert_eq!(sched.pending_notices(), schedule::MAX_NOTICES);
    assert_eq!(
        status_of(&state, "nightly")["last_notice"]["result"],
        "dropped"
    );
    // Deleting the schedule drops its notices.
    sched.delete("nightly").expect("delete");
    assert_eq!(sched.pending_notices(), 0);
}

#[tokio::test]
async fn the_routes_save_list_run_and_delete() {
    let (state, app, _clock) = bench(Config::for_tests());
    state
        .saved_tests
        .put(test_of("ok", "/status/200", 200), false)
        .expect("test");
    let body = json!({ "name": "nightly", "every": "15m", "names": ["ok"] });
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/tests/schedules",
        "ops",
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["status"]["next_due"], json!("2026-11-03T10:15:00Z"));
    assert_eq!(v["selection"], "tests ok");
    let (s, _) = call(
        &app,
        "POST",
        "/_rustybin/tests/schedules",
        "ops",
        Some(body),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/tests/schedules",
        "ops",
        Some(json!({ "name": "bad", "every": "1m" })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(
        v["error"].as_str().unwrap_or("").contains("5m to 24h"),
        "{v}"
    );
    let (s, v) = call(&app, "GET", "/_rustybin/tests/schedules", "ops", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["schedules"][0]["name"], "nightly");
    assert_eq!(v["max_schedules"], 20);
    // A read sent back as is replaces it.
    let (_, mut read) = call(
        &app,
        "GET",
        "/_rustybin/tests/schedules/nightly",
        "ops",
        None,
    )
    .await;
    read["at"] = json!("02:30");
    read.as_object_mut().expect("obj").remove("every");
    let (s, v) = call(
        &app,
        "PUT",
        "/_rustybin/tests/schedules/nightly",
        "ops",
        Some(read),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"]["next_due"], json!("2026-11-04T02:30:00Z"));
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/tests/schedules/nightly/run",
        "ops",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["result"], "passed");
    assert_eq!(v["run"]["trigger"], "schedule:nightly");
    assert_eq!(v["schedule"]["status"]["runs"], 1);
    assert_eq!(
        v["schedule"]["status"]["next_due"],
        json!("2026-11-04T02:30:00Z"),
        "a run by hand leaves the next time"
    );
    let (s, v) = call(&app, "GET", "/_rustybin/tests/runs", "ops", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["runs"][0]["trigger"], "schedule:nightly");
    let (s, _) = call(
        &app,
        "DELETE",
        "/_rustybin/tests/schedules/nightly",
        "ops",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    for (m, p) in [
        ("GET", "/_rustybin/tests/schedules/nightly"),
        ("DELETE", "/_rustybin/tests/schedules/nightly"),
        ("POST", "/_rustybin/tests/schedules/nightly/run"),
    ] {
        let (s, _) = call(&app, m, p, "ops", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{m} {p}");
    }
    // A test cannot take the route's name.
    let (s, _) = call(
        &app,
        "PUT",
        "/_rustybin/tests/schedules",
        "ops",
        Some(json!({ "request": { "path": "/get" } })),
    )
    .await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn the_admin_token_guards_the_routes() {
    let mut c = Config::for_tests();
    c.admin_token = Some("adm".into());
    let (_state, app, _clock) = bench(c);
    let (s, _) = call(&app, "GET", "/_rustybin/tests/schedules", "ops", None).await;
    assert!(
        s == StatusCode::UNAUTHORIZED || s == StatusCode::FORBIDDEN,
        "{s}"
    );
    let (s, _) = call(
        &app,
        "POST",
        "/_rustybin/tests/schedules",
        "ops",
        Some(json!({ "name": "x", "every": "1h" })),
    )
    .await;
    assert!(
        s == StatusCode::UNAUTHORIZED || s == StatusCode::FORBIDDEN,
        "{s}"
    );
}

#[tokio::test]
async fn schedules_and_their_status_survive_a_restart() {
    let dir = tempfile::tempdir().expect("tmp");
    let tests = Tests::new(Some(dir.path()));
    let mut s = every("nightly", "1h");
    s.webhook = Some("https://hooks.example.com/rustybin".into());
    tests.schedules.put(s.clone(), false).expect("put");
    drop(tests);
    let again = Tests::new(Some(dir.path()));
    assert_eq!(again.schedules.get("nightly"), Some(s));
    let v = again.schedules.view_of("nightly").expect("view");
    assert!(v["status"]["next_due"].is_string(), "{v}");
    assert_eq!(v["status"]["running"], false);
}

#[test]
fn the_document_round_trip_and_prune() {
    use crate::configdoc::{apply, export, ApplyOptions, SecretsMode};
    let a = test_state_with(Config::for_tests());
    let mut s = every("nightly", "30m");
    s.prefix = Some("order".into());
    s.webhook = Some("https://hooks.example.com/rustybin".into());
    s.notify_recovery = true;
    a.saved_tests.schedules.put(s.clone(), false).expect("put");
    let file = export(&a, SecretsMode::Refs, false).file;
    assert_eq!(file.test_schedules.as_deref(), Some(&[s.clone()][..]));
    let yaml = crate::configdoc::to_yaml(&file);
    assert!(yaml.contains("test_schedules:\n- name: nightly"), "{yaml}");
    let b = test_state_with(Config::for_tests());
    b.saved_tests
        .schedules
        .put(every("stale", "1h"), false)
        .expect("put");
    let parsed = crate::mocks::config_file::parse(&yaml).expect("parse");
    let opts = |prune, untrusted| ApplyOptions {
        prune,
        dry_run: false,
        principal: "test".into(),
        base: None,
        untrusted,
        caller: None,
        base_url: None,
    };
    let r = apply(&b, parsed, &opts(false, false));
    assert_eq!(
        r["results"]["test_schedules"]["saved"],
        json!(["nightly"]),
        "{r}"
    );
    assert_eq!(b.saved_tests.schedules.get("nightly"), Some(s.clone()));
    assert!(b.saved_tests.schedules.get("stale").is_some(), "no prune");
    // Pruned: what the document does not name goes.
    let again = crate::mocks::config_file::parse(&yaml).expect("parse");
    let r = apply(&b, again, &opts(true, false));
    assert_eq!(
        r["results"]["test_schedules"]["deleted"],
        json!(["stale"]),
        "{r}"
    );
    assert_eq!(b.saved_tests.schedules.len(), 1);
    // From the repository: a new webhook is refused, the old one stays.
    let pulled = crate::mocks::config_file::parse(
        "version: 2\ntest_schedules:\n- name: nightly\n  every: 30m\n  webhook: https://attacker.example/hook\n- name: other\n  every: 1h\n  webhook: https://attacker.example/hook\n",
    )
    .expect("parse");
    let r = apply(&b, pulled, &opts(false, true));
    let problems = r["problems"].to_string();
    assert!(problems.contains("test_schedules.nightly"), "{r}");
    assert!(problems.contains("test_schedules.other"), "{r}");
    assert_eq!(
        b.saved_tests
            .schedules
            .get("nightly")
            .and_then(|s| s.webhook),
        s.webhook
    );
    assert!(b.saved_tests.schedules.get("other").is_none());
    // An invalid one is reported and kept, never pruned.
    let bad = crate::mocks::config_file::parse(
        "version: 2\ntest_schedules:\n- name: nightly\n  every: 1m\n",
    )
    .expect("parse");
    let r = apply(&b, bad, &opts(true, false));
    assert!(r["problems"].to_string().contains("5m to 24h"), "{r}");
    assert!(b.saved_tests.schedules.get("nightly").is_some());
}
