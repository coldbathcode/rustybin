//! The history of saved-test runs: kept, capped, read back, compared,
//! paged, cleared, and never holding a variable's value.

use super::*;

fn report_of(results: &[(&str, bool)]) -> run::Report {
    let results: Vec<run::TestResult> = results
        .iter()
        .map(|(n, p)| run::TestResult {
            name: n.to_string(),
            target: "direct".into(),
            passed: *p,
            status: Some(if *p { 200 } else { 500 }),
            ms: Some(1),
            failures: if *p {
                vec![]
            } else {
                vec![run::Failure {
                    assertion: "status 200".into(),
                    expected: "200".into(),
                    seen: "500".into(),
                }]
            },
            error: None,
            steps: Vec::new(),
        })
        .collect();
    let failed = results.iter().filter(|r| !r.passed).count();
    run::Report {
        passed: failed == 0,
        total: results.len(),
        failed,
        errors: 0,
        duration_ms: 3,
        ran_at: "2026-11-03T10:00:00Z".into(),
        results,
    }
}

fn at(day: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-10-01T00:00:00Z")
        .expect("time")
        .with_timezone(&chrono::Utc)
        + chrono::Duration::days(day)
}

fn files_in(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir.join(history::DIR))
        .map(|d| d.count())
        .unwrap_or(0)
}

fn keep(
    h: &history::History,
    report: run::Report,
    when: chrono::DateTime<chrono::Utc>,
) -> history::TestRun {
    h.record_at("api", "open", vec![], report, when)
}

#[test]
fn runs_are_kept_and_capped_by_count_and_by_age() {
    // By count, in memory.
    let h = history::History::with_cap(None, 3);
    let ids: Vec<String> = (0..5)
        .map(|i| keep(&h, report_of(&[("a", true)]), at(i)).id)
        .collect();
    assert_eq!(h.len(), 3);
    assert!(h.get(&ids[0]).is_none() && h.get(&ids[1]).is_none());
    assert!(h.get(&ids[4]).is_some());
    // By count and by age, with their files.
    let dir = tempfile::tempdir().expect("dir");
    let h = history::History::with_cap(Some(dir.path()), 3);
    let first = keep(&h, report_of(&[("a", true)]), at(0));
    keep(&h, report_of(&[("a", true)]), at(10));
    assert_eq!(files_in(dir.path()), 2);
    // 31 days after the first: it is gone, file and all.
    keep(&h, report_of(&[("a", true)]), at(31));
    assert_eq!(h.len(), 2);
    assert!(h.get(&first.id).is_none());
    assert_eq!(files_in(dir.path()), 2);
    for d in 32..35 {
        keep(&h, report_of(&[("a", true)]), at(d));
    }
    assert_eq!(h.len(), 3);
    assert_eq!(files_in(dir.path()), 3);
    // The defaults: 200 and 30 days with a state directory, 50 without.
    assert_eq!(
        history::History::new(Some(dir.path())).cap(),
        history::MAX_RUNS
    );
    assert_eq!(history::History::new(None).cap(), history::MAX_MEMORY_RUNS);
    assert_eq!(
        (
            history::MAX_RUNS,
            history::MAX_MEMORY_RUNS,
            history::MAX_AGE_DAYS
        ),
        (200, 50, 30)
    );
}

#[test]
fn a_restart_reads_the_runs_back() {
    let dir = tempfile::tempdir().expect("dir");
    let now = chrono::Utc::now();
    let a = history::History::new(Some(dir.path()));
    let one = a.record_at(
        "cli",
        "admin-token",
        vec![],
        report_of(&[("a", true), ("b", true)]),
        now,
    );
    let two = a.record_at(
        "console",
        "jwt:ana",
        vec!["b".into()],
        report_of(&[("a", true), ("b", false)]),
        now + chrono::Duration::seconds(1),
    );
    assert_eq!(two.change.previous.as_deref(), Some(one.id.as_str()));
    let b = history::History::new(Some(dir.path()));
    assert_eq!(b.len(), 2);
    let back = b.get(&two.id).expect("kept");
    assert_eq!(back.trigger, "console");
    assert_eq!(back.principal, "jwt:ana");
    assert_eq!(back.selection, vec!["b".to_string()]);
    assert_eq!(back.change.newly_failing, vec!["b".to_string()]);
    assert_eq!(back.report.results[1].failures[0].seen, "500");
    let page = b.page(10, None);
    assert_eq!(page.runs[0]["id"], json!(two.id), "newest first");
    assert_eq!(page.runs[0]["failing"], json!(["b"]));
    // Ids are never paths.
    assert!(b.get("../tests").is_none());
    assert!(!history::valid_id("../x") && !history::valid_id(""));
}

#[test]
fn the_change_is_against_the_previous_run_of_the_same_tests() {
    let h = history::History::new(None);
    let t = |m: i64| at(0) + chrono::Duration::minutes(m);
    let all = keep(&h, report_of(&[("a", true), ("b", false)]), t(0));
    assert_eq!(all.change, history::Change::default());
    // Only a: compared with the previous run (no run of a alone yet).
    let only_a = h.record_at(
        "api",
        "open",
        vec!["a".into()],
        report_of(&[("a", false)]),
        t(1),
    );
    assert_eq!(only_a.change.previous.as_deref(), Some(all.id.as_str()));
    assert_eq!(only_a.change.newly_failing, vec!["a".to_string()]);
    // Both again: compared with the run of both, not with only_a.
    let again = keep(&h, report_of(&[("a", false), ("b", true)]), t(2));
    assert_eq!(again.change.previous.as_deref(), Some(all.id.as_str()));
    assert_eq!(again.change.newly_failing, vec!["a".to_string()]);
    assert_eq!(again.change.newly_passing, vec!["b".to_string()]);
}

#[test]
fn the_history_pages_newest_first() {
    // Recent runs: a read prunes what is past 30 days.
    let h = history::History::new(None);
    let base = chrono::Utc::now() - chrono::Duration::minutes(10);
    let ids: Vec<String> = (0..5)
        .map(|i| {
            keep(
                &h,
                report_of(&[("a", true)]),
                base + chrono::Duration::seconds(i),
            )
            .id
        })
        .collect();
    let got = |p: &history::Page| -> Vec<String> {
        p.runs
            .iter()
            .filter_map(|r| r["id"].as_str().map(str::to_string))
            .collect()
    };
    let p1 = h.page(2, None);
    assert_eq!(p1.total, 5);
    assert_eq!(got(&p1), vec![ids[4].clone(), ids[3].clone()]);
    let p2 = h.page(2, p1.next_cursor.as_deref());
    assert_eq!(got(&p2), vec![ids[2].clone(), ids[1].clone()]);
    let p3 = h.page(2, p2.next_cursor.as_deref());
    assert_eq!(got(&p3), vec![ids[0].clone()]);
    assert!(p3.next_cursor.is_none());
}

fn with_state_dir(dir: &std::path::Path) -> Config {
    let mut c = Config::for_tests();
    c.state_dir = Some(dir.to_path_buf());
    c
}

async fn raw(app: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = app.clone().oneshot(req).await.expect("resp");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

#[tokio::test]
async fn a_run_answers_its_id_and_a_kept_run_names_a_newly_failing_test() {
    let dir = tempfile::tempdir().expect("dir");
    let state = test_state_with(with_state_dir(dir.path()));
    let app = test_app_with(state.clone());
    state
        .saved_tests
        .put(plain_test("flips"), false)
        .expect("put");
    state
        .saved_tests
        .put(plain_test("steady"), false)
        .expect("put");
    let req = Request::post(RUN_PATH)
        .header("x-rustybin-session", "ops")
        .header("x-rustybin-client", "console")
        .body(Body::empty())
        .expect("req");
    let (s, _, text) = raw(&app, req).await;
    assert_eq!(s, StatusCode::OK, "{text}");
    let first: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(first["passed"], true, "{first}");
    assert_eq!(first["trigger"], "console", "{first}");
    assert_eq!(first["principal"], "open", "{first}");
    let first_id = first["run_id"].as_str().expect("run_id").to_string();
    // The test changes and fails: the next run says so.
    let mut t = plain_test("flips");
    t.assert = vec![Assertion::status(201)];
    state.saved_tests.put(t, true).expect("put");
    let (s, second) = call(&app, "POST", RUN_PATH, "ops", Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK, "{second}");
    assert_eq!(second["trigger"], "api", "{second}");
    assert_eq!(second["change"]["previous"], json!(first_id), "{second}");
    assert_eq!(
        second["change"]["newly_failing"],
        json!(["flips"]),
        "{second}"
    );
    assert_eq!(second["change"]["newly_passing"], json!([]), "{second}");
    // The list, one run, and its JUnit.
    let (s, list) = call(&app, "GET", RUNS_PATH, "ops", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["total"], 2, "{list}");
    assert_eq!(list["runs"][0]["id"], second["run_id"], "{list}");
    assert_eq!(list["runs"][0]["failing"], json!(["flips"]), "{list}");
    let id = second["run_id"].as_str().expect("id").to_string();
    let (s, one) = call(&app, "GET", &format!("{RUNS_PATH}/{id}"), "ops", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(one["results"].as_array().map(Vec::len), Some(2), "{one}");
    assert_eq!(one["change"]["newly_failing"], json!(["flips"]));
    let req = Request::get(format!("{RUNS_PATH}/{id}?format=junit"))
        .body(Body::empty())
        .expect("req");
    let (s, headers, xml) = raw(&app, req).await;
    assert_eq!(s, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE]
        .to_str()
        .unwrap_or("")
        .starts_with("application/xml"));
    assert!(xml.contains("tests=\"2\" failures=\"1\""), "{xml}");
    assert!(
        xml.contains("<failure message=\"status 201: expected 201, saw 200\""),
        "{xml}"
    );
    // A JUnit run names its kept run in a header.
    let req = Request::post(format!("{RUN_PATH}?format=junit"))
        .body(Body::empty())
        .expect("req");
    let (s, headers, _) = raw(&app, req).await;
    assert_eq!(s, StatusCode::OK);
    assert!(headers.get(RUN_ID_HEADER).is_some());
    let (s, _) = call(&app, "GET", &format!("{RUNS_PATH}/nope"), "ops", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // A restart reads them back.
    let again = history::History::new(Some(dir.path()));
    assert_eq!(again.len(), 3);
    assert!(again.get(&id).is_some());
}

#[tokio::test]
async fn a_kept_run_never_holds_a_variable_value() {
    let dir = tempfile::tempdir().expect("dir");
    let state = test_state_with(with_state_dir(dir.path()));
    let app = test_app_with(state.clone());
    let test = SavedTest {
        name: "echoes".into(),
        description: None,
        request: TestRequest {
            method: "POST".into(),
            path: "/anything".into(),
            headers: [("x-api-key".to_string(), "{{API_KEY}}".to_string())].into(),
            body: Some("token is {{API_KEY}}".into()),
        },
        steps: Vec::new(),
        gateway: None,
        assert: vec![Assertion {
            body_contains: Some("never there".into()),
            ..Assertion::default()
        }],
        project: None,
    };
    state.saved_tests.put(test, false).expect("put");
    let secret = "sk-live-planted-7781";
    let (s, v) = call(
        &app,
        "POST",
        RUN_PATH,
        "ops",
        Some(json!({ "variables": { "API_KEY": secret, "UNUSED": "planted-unused-9915" } })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["passed"], false, "{v}");
    assert!(!v.to_string().contains(secret), "{v}");
    let mut seen = 0;
    for f in std::fs::read_dir(dir.path().join(history::DIR)).expect("runs dir") {
        let body = std::fs::read_to_string(f.expect("entry").path()).expect("read");
        assert!(!body.contains(secret), "{body}");
        assert!(!body.contains("planted-unused-9915"), "{body}");
        assert!(!body.contains("variables"), "{body}");
        assert!(body.contains("{{API_KEY}}"), "{body}");
        seen += 1;
    }
    assert_eq!(seen, 1);
}

#[tokio::test]
async fn the_history_is_read_and_cleared_by_an_admin_only() {
    let mut c = Config::for_tests();
    c.admin_token = Some("admin-secret".into());
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    state
        .saved_tests
        .history
        .record("api", "open", vec![], report_of(&[("a", true)]));
    for (method, uri) in [
        ("GET", RUNS_PATH.to_string()),
        ("DELETE", RUNS_PATH.to_string()),
        ("GET", format!("{RUNS_PATH}/x")),
    ] {
        let req = Request::builder()
            .method(method)
            .uri(&uri)
            .body(Body::empty())
            .expect("req");
        let (s, _, _) = raw(&app, req).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
    assert_eq!(state.saved_tests.history.len(), 1);
    let req = Request::delete(RUNS_PATH)
        .header("authorization", "Bearer admin-secret")
        .body(Body::empty())
        .expect("req");
    let (s, _, text) = raw(&app, req).await;
    assert_eq!(s, StatusCode::OK, "{text}");
    assert!(state.saved_tests.history.is_empty());
}

#[test]
fn runs_is_not_a_test_name() {
    assert!(!valid_name("runs"));
    assert!(valid_name("runs-1"));
}
