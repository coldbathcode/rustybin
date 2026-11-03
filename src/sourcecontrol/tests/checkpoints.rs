//! Checkpoints in the configuration repository: committed only when named,
//! pulled and restored on a second instance (the same mocks and data), a
//! file over the cap or malformed refused, never written by the automatic
//! mode.

use super::*;

const ITEMS: &str = r#"{"openapi":"3.0.3","info":{"title":"Items","version":"1"},"paths":{"/items":{"get":{"operationId":"listItems","responses":{"200":{"description":"ok","content":{"application/json":{"example":[]}}}}},"post":{"operationId":"addItem","requestBody":{"content":{"application/json":{"schema":{"type":"object"}}}},"responses":{"201":{"description":"ok","content":{"application/json":{"schema":{"type":"object"}}}}}}},"/items/{id}":{"get":{"operationId":"getItem","parameters":[{"name":"id","in":"path","required":true,"schema":{"type":"integer"}}],"responses":{"200":{"description":"ok","content":{"application/json":{"schema":{"type":"object"}}}}}}}}}"#;

/// git run directly against a working copy (what another writer of the
/// repository does), without the user's configuration.
fn raw_git(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=Someone",
            "-c",
            "user.email=someone@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = match body {
        Some(b) => json_request(method, uri, &b),
        None => axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .body(axum::body::Body::empty())
            .expect("request"),
    };
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = crate::test_support::body_bytes(resp).await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The instance's API mocks: deployed source, on or off, the stateful
/// data; and the health toggle.
fn picture(state: &AppState) -> Value {
    let mocks: Vec<Value> = state
        .mocks
        .list()
        .iter()
        .filter(|s| crate::checkpoints::covered(s))
        .map(|s| {
            json!({
                "name": s.name,
                "enabled": s.enabled,
                "live": s.live.as_ref().map(|c| c.source.to_string()),
                "draft": s.draft.source.to_string(),
                "state": serde_json::to_value(s.state.copy()).unwrap_or(Value::Null),
            })
        })
        .collect();
    json!({ "mocks": mocks, "healthy": state.health.is_healthy() })
}

/// An instance with a stateful mock holding one item, saved as `demo`.
async fn with_demo(root: &Path, name: &str, remote: &str) -> (AppState, axum::Router) {
    let state = instance(root, name, remote);
    let app = test_app_with(state.clone());
    let (s, v) = call(
        &app,
        "POST",
        "/_rustybin/mocks",
        Some(json!({ "name": "items", "spec": ITEMS, "responses": { "mode": "stateful" } })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, v) = call(
        &app,
        "POST",
        "/mock/items/items",
        Some(json!({ "id": 1, "label": "first" })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    crate::checkpoints::save(&state, "demo", false).expect("save");
    (state, app)
}

#[tokio::test]
async fn a_checkpoint_travels_through_the_repository() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let (a, _app_a) = with_demo(root.path(), "a", &remote).await;
    a.git.init("ann").await.expect("init");
    // A plain commit holds no checkpoint.
    let plain = a.git.commit(&a, "The mocks", "ann").await.expect("commit");
    assert_eq!(plain["checkpoints"], json!([]));
    let repo = a.git.repo_dir().expect("repo");
    assert!(!repo.join("rustybin/checkpoints").exists());
    // A checkpoint that does not exist: refused before anything is written.
    let e = a
        .git
        .commit_with(&a, "Nope", &["nope".into()], "ann")
        .await
        .expect_err("unknown");
    assert_eq!(e.status, StatusCode::NOT_FOUND);
    let e = a
        .git
        .commit_with(&a, "Bad", &["../x".into()], "ann")
        .await
        .expect_err("bad name");
    assert_eq!(e.status, StatusCode::BAD_REQUEST);
    assert!(!repo.join("rustybin/checkpoints").exists());
    let c = a
        .git
        .commit_with(
            &a,
            "The demo checkpoint",
            &["demo".into(), "demo".into()],
            "ann",
        )
        .await
        .expect("commit");
    assert_eq!(c["committed"], json!(true), "{c}");
    assert_eq!(c["checkpoints"], json!(["demo"]));
    assert_eq!(c["files"], json!(1), "only the checkpoint changed: {c}");
    let file = repo.join("rustybin/checkpoints/demo.json");
    let written: crate::checkpoints::Checkpoint =
        serde_json::from_slice(&std::fs::read(&file).expect("file")).expect("a checkpoint");
    let local = a.checkpoints.get("demo").expect("local");
    assert_eq!(
        written.mocks, local.mocks,
        "the same format as the state directory's"
    );
    // The same checkpoint again: nothing to commit.
    let again = a
        .git
        .commit_with(&a, "Again", &["demo".into()], "ann")
        .await
        .expect("again");
    assert_eq!(again["committed"], json!(false));
    a.git.push("ann").await.expect("push");

    // A second instance pulls: the checkpoint is listed, not applied.
    let b = instance(root.path(), "b", &remote);
    let app_b = test_app_with(b.clone());
    b.git.init("bob").await.expect("init b");
    let pull = b
        .git
        .pull(&b, true, false, true, "bob")
        .await
        .expect("pull");
    assert_eq!(pull["checkpoints"], json!(["demo"]), "{pull}");
    assert!(b.checkpoints.is_empty(), "a pull saves no checkpoint");
    let (s, list) = call(&app_b, "GET", "/_rustybin/checkpoints?from=git", None).await;
    assert_eq!(s, StatusCode::OK, "{list}");
    assert_eq!(list["from"], json!("git"));
    assert_eq!(list["path"], json!("rustybin/checkpoints"));
    assert_eq!(list["checkpoints"][0]["name"], json!("demo"), "{list}");
    assert_eq!(list["checkpoints"][0]["ok"], json!(true), "{list}");
    assert_eq!(list["checkpoints"][0]["mocks"], json!(["items"]));
    let (s, _) = call(&app_b, "GET", "/_rustybin/checkpoints?from=elsewhere", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // B's copy differs: the document has the mock, not its data; and B is
    // unhealthy.
    b.health.set(false);
    assert_ne!(picture(&a), picture(&b));
    let before = picture(&b);
    let (s, dry) = call(
        &app_b,
        "POST",
        "/_rustybin/checkpoints/demo/restore?from=git&dry_run=true",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{dry}");
    assert_eq!(dry["from"], json!("git"));
    assert_eq!(dry["dry_run"], json!(true));
    assert_eq!(dry["changed"], json!(true), "{dry}");
    assert_eq!(picture(&b), before, "a dry run changes nothing");
    let (s, done) = call(
        &app_b,
        "POST",
        "/_rustybin/checkpoints/demo/restore?from=git",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{done}");
    assert_eq!(done["problems"], json!([]), "{done}");
    assert_eq!(picture(&a), picture(&b), "the same mocks and data");
    let (s, items) = call(&app_b, "GET", "/mock/items/items", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(items.to_string().contains("first"), "{items}");
    // Restoring again changes nothing; a name the repository lacks is 404.
    let (_, again) = call(
        &app_b,
        "POST",
        "/_rustybin/checkpoints/demo/restore?from=git",
        None,
    )
    .await;
    assert_eq!(again["changed"], json!(false), "{again}");
    let (s, missing) = call(
        &app_b,
        "POST",
        "/_rustybin/checkpoints/other/restore?from=git",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{missing}");
    assert!(
        b.checkpoints.is_empty(),
        "a restore from git saves nothing locally"
    );
}

#[tokio::test]
async fn a_repository_checkpoint_over_the_cap_or_malformed_is_refused() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let (a, _) = with_demo(root.path(), "a", &remote).await;
    a.git.init("ann").await.expect("init");
    a.git
        .commit_with(&a, "The demo checkpoint", &["demo".into()], "ann")
        .await
        .expect("commit");
    // Someone writes files of their own into the repository.
    let repo = a.git.repo_dir().expect("repo");
    let dir = repo.join("rustybin/checkpoints");
    std::fs::write(dir.join("broken.json"), "{ not json").expect("broken");
    let mut other = serde_json::to_value(&*a.checkpoints.get("demo").expect("demo")).expect("json");
    other["name"] = json!("somewhere-else");
    std::fs::write(dir.join("renamed.json"), other.to_string()).expect("renamed");
    let mut twice = serde_json::to_value(&*a.checkpoints.get("demo").expect("demo")).expect("json");
    twice["name"] = json!("twice");
    let m = twice["mocks"][0].clone();
    twice["mocks"] = json!([m.clone(), m]);
    std::fs::write(dir.join("twice.json"), twice.to_string()).expect("twice");
    std::fs::write(dir.join("not a name.json"), "{}").expect("odd");
    raw_git(&repo, &["add", "-A"]);
    raw_git(&repo, &["commit", "--quiet", "-m", "Files of my own"]);
    a.git.push("ann").await.expect("push");

    let b = instance(root.path(), "b", &remote);
    let app_b = test_app_with(b.clone());
    b.git.init("bob").await.expect("init b");
    let (s, list) = call(&app_b, "GET", "/_rustybin/checkpoints?from=git", None).await;
    assert_eq!(s, StatusCode::OK, "{list}");
    let by_name = |n: &str| {
        list["checkpoints"]
            .as_array()
            .expect("list")
            .iter()
            .find(|c| c["name"] == json!(n))
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(by_name("demo")["ok"], json!(true), "{list}");
    assert!(
        by_name("not a name").is_null(),
        "a file that is not a checkpoint name is not listed"
    );
    for (name, says) in [
        ("broken", "is not a checkpoint"),
        ("renamed", "named after its checkpoint"),
        ("twice", "appears twice"),
    ] {
        let c = by_name(name);
        assert_eq!(c["ok"], json!(false), "{c}");
        assert!(c["error"].as_str().unwrap_or("").contains(says), "{c}");
        let (s, v) = call(
            &app_b,
            "POST",
            &format!("/_rustybin/checkpoints/{name}/restore?from=git&dry_run=true"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{name}: {v}");
        assert!(v["error"].as_str().unwrap_or("").contains(says), "{v}");
    }
    // Over the cap: refused before it is read, listed with the reason.
    b.git.set_max_checkpoint_bytes(100);
    let (s, v) = call(
        &app_b,
        "POST",
        "/_rustybin/checkpoints/demo/restore?from=git",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
    assert!(
        v["error"]
            .as_str()
            .unwrap_or("")
            .contains("larger than 100 bytes"),
        "{v}"
    );
    let (_, list) = call(&app_b, "GET", "/_rustybin/checkpoints?from=git", None).await;
    let demo = list["checkpoints"]
        .as_array()
        .expect("list")
        .iter()
        .find(|c| c["name"] == json!("demo"))
        .cloned()
        .expect("demo");
    assert_eq!(demo["ok"], json!(false));
    assert!(
        demo["error"]
            .as_str()
            .unwrap_or("")
            .contains("larger than 100 bytes"),
        "{demo}"
    );
    assert!(b.mocks.get("items").is_none(), "nothing was restored");
}

#[tokio::test]
async fn the_automatic_mode_never_writes_a_checkpoint() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let (a, _) = with_demo(root.path(), "a", &remote).await;
    a.git.init("ann").await.expect("init");
    let mut values = BTreeMap::new();
    values.insert(AUTO_KEY.to_string(), json!("push_on_change"));
    a.settings
        .update(values, BTreeMap::new(), "test")
        .expect("auto");
    let repo = a.git.repo_dir().expect("repo");
    let files = |rev: &str| raw_git(&repo, &["ls-tree", "-r", "--name-only", rev]);
    a.git.auto_tick(&a).await;
    assert_eq!(a.git.history().await.expect("history").len(), 1);
    assert!(!files("HEAD").contains("checkpoints/"), "{}", files("HEAD"));
    // An explicit commit writes one; the automatic mode then leaves the
    // directory as that commit left it, whatever the working copy holds.
    a.git
        .commit_with(&a, "The demo checkpoint", &["demo".into()], "ann")
        .await
        .expect("commit");
    let committed = raw_git(&repo, &["show", "HEAD:rustybin/checkpoints/demo.json"]);
    add_mock(&a, "orders");
    crate::checkpoints::save(&a, "demo", true).expect("save again");
    let dir = repo.join("rustybin/checkpoints");
    std::fs::write(dir.join("demo.json"), "{ changed in the working copy }").expect("write");
    std::fs::write(dir.join("stray.json"), "{}").expect("stray");
    a.git.auto_tick(&a).await;
    let history = a.git.history().await.expect("history");
    assert_eq!(history.len(), 3, "{history:?}");
    assert!(
        history[0]["subject"]
            .as_str()
            .unwrap_or("")
            .starts_with("Update mocks.orders: added"),
        "{history:?}"
    );
    let changed = raw_git(&repo, &["diff", "--name-only", "HEAD~1", "HEAD"]);
    assert!(!changed.contains("checkpoints/"), "{changed}");
    assert!(!files("HEAD").contains("stray.json"));
    assert_eq!(
        raw_git(&repo, &["show", "HEAD:rustybin/checkpoints/demo.json"]),
        committed
    );
}
