//! Source control against a bare repository in a temporary directory: the
//! remote grammar and the policy, init, commit, push, pull and apply, a
//! diverged branch and its two resolutions, the deploy key, the automatic
//! mode, startup from `RUSTYBIN_CONFIG_GIT`, the routes. The git tests
//! skip with a notice when `git` is not installed.

use std::collections::BTreeMap;
use std::path::Path;

use axum::http::StatusCode;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::mocks::store::{Definition, Origin};
use crate::test_support::{body_json, get_request, json_request, test_app_with, test_state_with};

const SPEC: &str = r#"{"openapi":"3.0.3","info":{"title":"Orders","version":"1"},"paths":{"/orders":{"get":{"operationId":"listOrders","responses":{"200":{"description":"ok"}}}}}}"#;

fn git_installed() -> bool {
    let ok = std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("git is not installed: source control tests skipped");
    }
    ok
}

/// A bare repository and its `file://` URL.
fn bare_repo(dir: &Path) -> String {
    let path = dir.join("remote.git");
    let out = std::process::Command::new("git")
        .args(["init", "--quiet", "--bare", "-b", "main"])
        .arg(&path)
        .output()
        .expect("git init --bare");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    format!("file://{}", path.display())
}

/// An instance with its own state directory, the loopback allowance and
/// the remote configured.
fn instance(root: &Path, name: &str, remote: &str) -> AppState {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("state dir");
    let mut config = Config::for_tests();
    config.state_dir = Some(dir);
    config.egress_allow_loopback = true;
    config.instance_id = format!("test-{name}");
    let state = test_state_with(config);
    let mut values = BTreeMap::new();
    values.insert(REMOTE_KEY.to_string(), json!(remote));
    state
        .settings
        .update(values, BTreeMap::new(), "test")
        .expect("remote");
    state
}

fn add_mock(state: &AppState, name: &str) {
    let report = state.mocks.apply(
        vec![Definition {
            name: name.into(),
            source: SPEC.into(),
            settings: Default::default(),
            enabled: true,
        }],
        Origin::Api,
        None,
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}

fn set_title(state: &AppState, title: &str) {
    let mut values = BTreeMap::new();
    values.insert("console.title".to_string(), json!(title));
    state
        .settings
        .update(values, BTreeMap::new(), "test")
        .expect("title");
}

#[test]
fn remotes_are_parsed_and_checked() {
    let https = parse_remote("https://github.com/acme/mocks.git").expect("https");
    assert_eq!(
        (https.kind, https.host.as_deref(), https.port),
        (RemoteKind::Https, Some("github.com"), 443)
    );
    let ssh = parse_remote("ssh://git@gitlab.example.com:2222/acme/mocks.git").expect("ssh");
    assert_eq!(
        (ssh.kind, ssh.host.as_deref(), ssh.port),
        (RemoteKind::Ssh, Some("gitlab.example.com"), 2222)
    );
    let scp = parse_remote("git@github.com:acme/mocks.git").expect("scp");
    assert_eq!(
        (scp.kind, scp.host.as_deref(), scp.port),
        (RemoteKind::Ssh, Some("github.com"), 22)
    );
    let local = parse_remote("file:///srv/repo.git").expect("file");
    assert_eq!((local.kind, local.host), (RemoteKind::Local, None));
    assert_eq!(
        parse_remote("/srv/repo.git").expect("path").kind,
        RemoteKind::Local
    );
    for bad in [
        "",
        "http://github.com/acme/mocks.git",
        "https://user:token@github.com/acme/mocks.git",
        "ftp://x/y",
        "-oProxyCommand=evil",
        "git@github.com",
        "file://relative/path",
        "https://bad host/x",
    ] {
        assert!(parse_remote(bad).is_err(), "{bad:?} accepted");
    }
    assert_eq!(
        repo_path("/rustybin/").expect("path"),
        Some("rustybin".into())
    );
    assert_eq!(repo_path("").expect("root"), None);
    assert!(repo_path("../x").is_err());
    assert!(repo_path("a/.git/b").is_err());
    assert_eq!(parse_interval("5m").expect("5m").as_secs(), 300);
    assert!(parse_interval("30s").is_err());
    assert!(parse_interval("25h").is_err());
    assert_eq!(
        author_parts("Ann <ann@example.com>").expect("author").1,
        "ann@example.com"
    );
    assert!(author_parts("Ann").is_err());
    assert!(check_branch(&json!("release/1.0")).is_ok());
    assert!(check_branch(&json!("-x")).is_err());
    assert!(check_branch(&json!("a..b")).is_err());
}

#[tokio::test]
async fn a_local_remote_needs_the_loopback_allowance_and_a_state_dir() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    // No state directory.
    let state = test_state_with(Config::for_tests());
    let e = state.git.init("t").await.expect_err("no state dir");
    assert_eq!(e.status, StatusCode::CONFLICT);
    assert!(e.message.contains("RUSTYBIN_STATE_DIR"), "{}", e.message);
    // A state directory, loopback refused.
    let mut config = Config::for_tests();
    config.state_dir = Some(root.path().join("no-loopback"));
    let state = test_state_with(config);
    let mut values = BTreeMap::new();
    values.insert(REMOTE_KEY.to_string(), json!(remote));
    state
        .settings
        .update(values, BTreeMap::new(), "test")
        .expect("remote");
    let e = state.git.init("t").await.expect_err("loopback refused");
    assert!(
        e.message.contains("RUSTYBIN_EGRESS_ALLOW_LOOPBACK"),
        "{}",
        e.message
    );
    // A network remote at a forbidden address.
    let state = instance(root.path(), "forbidden", "https://169.254.169.254/repo.git");
    let e = state.git.init("t").await.expect_err("forbidden");
    assert_eq!(e.status, StatusCode::CONFLICT);
    assert!(e.message.contains("purpose git"), "{}", e.message);
    // No remote at all.
    let state = instance(root.path(), "none", "");
    let e = state
        .git
        .commit(&state, "x", "t")
        .await
        .expect_err("no remote");
    assert!(e.message.contains("git.remote"), "{}", e.message);
    // The status answers without any of it.
    let status = state.git.status(&state, false).await;
    assert_eq!(status["initialised"], json!(false));
    assert_eq!(status["git"]["available"], json!(true));
}

#[tokio::test]
async fn init_commit_push_pull_and_apply_round_trip() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let a = instance(root.path(), "a", &remote);
    add_mock(&a, "orders");
    set_title(&a, "Team A");
    let init = a.git.init("ann").await.expect("init");
    assert_eq!(init["fresh"], json!(true));
    assert_eq!(init["remote_branch"], json!(false), "the remote is empty");
    // Drift before the first commit: everything.
    let status = a.git.status(&a, true).await;
    assert_eq!(status["initialised"], json!(true));
    assert_eq!(status["drift"]["missing_document"], json!(true));
    assert!(
        status["drift"]["summary"]
            .as_array()
            .expect("summary")
            .iter()
            .any(|l| l == "mocks.orders: added"),
        "{status}"
    );
    assert_eq!(status["upstream"], json!(false));
    let e = a.git.push("ann").await.expect_err("nothing to push");
    assert!(e.message.contains("commit first"));
    let commit = a
        .git
        .commit(&a, "Add the orders mock", "ann")
        .await
        .expect("commit");
    assert_eq!(commit["committed"], json!(true));
    assert_eq!(commit["head"]["subject"], json!("Add the orders mock"));
    assert_eq!(commit["head"]["author"], json!("Rustybin"));
    let repo = a.git.repo_dir().expect("repo");
    assert!(repo.join("rustybin/rustybin.yaml").exists());
    assert!(repo.join("rustybin/specs/orders.json").exists());
    let again = a
        .git
        .commit(&a, "Nothing", "ann")
        .await
        .expect("commit again");
    assert_eq!(again["committed"], json!(false));
    let push = a.git.push("ann").await.expect("push");
    assert_eq!(push["pushed"], json!(true));
    let status = a.git.status(&a, true).await;
    assert_eq!(
        (status["ahead"].clone(), status["behind"].clone()),
        (json!(0), json!(0))
    );
    assert_eq!(status["drift"]["changes"], json!([]));
    let history = a.git.history().await.expect("history");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["subject"], json!("Add the orders mock"));
    assert!(
        history[0]["stat"]
            .as_str()
            .expect("stat")
            .contains("2 files changed"),
        "{history:?}"
    );
    assert_eq!(
        a.git.status(&a, false).await["timeline"]
            .as_array()
            .expect("timeline")
            .len(),
        5
    );

    // A second instance clones and applies.
    let b = instance(root.path(), "b", &remote);
    let init = b.git.init("bob").await.expect("init b");
    assert_eq!(init["remote_branch"], json!(true));
    assert_eq!(init["checked_out"], json!(true));
    let dry = b
        .git
        .pull(&b, true, true, true, "bob")
        .await
        .expect("dry run");
    assert_eq!(dry["dry_run"], json!(true));
    assert_eq!(
        dry["applied"]["results"]["mocks"]["would_create"],
        json!(["orders"])
    );
    assert!(b.mocks.get("orders").is_none(), "a dry run changes nothing");
    let pull = b
        .git
        .pull(&b, true, false, true, "bob")
        .await
        .expect("pull");
    assert_eq!(pull["applied"]["ok"], json!(true), "{pull}");
    assert_eq!(
        pull["applied"]["results"]["mocks"]["created"],
        json!(["orders"])
    );
    assert_eq!(
        b.settings.string("console.title").as_deref(),
        Some("Team A")
    );
    let export_a = crate::configdoc::export(&a, crate::configdoc::SecretsMode::Refs, false).yaml();
    let export_b = crate::configdoc::export(&b, crate::configdoc::SecretsMode::Refs, false).yaml();
    assert_eq!(export_a, export_b);
    assert_eq!(b.git.status(&b, true).await["drift"]["changes"], json!([]));

    // A pushes a change; B pulls it (fast forward) and applies it.
    set_title(&a, "Team A, renamed");
    a.git.commit(&a, "Rename", "ann").await.expect("commit 2");
    a.git.push("ann").await.expect("push 2");
    let status = b.git.status(&b, true).await;
    assert_eq!(status["behind"], json!(1));
    let pull = b
        .git
        .pull(&b, true, false, true, "bob")
        .await
        .expect("pull 2");
    assert_eq!(pull["fast_forwarded"], json!(true));
    assert_eq!(pull["behind"], json!(1));
    assert_eq!(
        b.settings.string("console.title").as_deref(),
        Some("Team A, renamed")
    );
    assert_eq!(b.git.history().await.expect("history").len(), 2);
}

#[tokio::test]
async fn a_diverged_branch_is_reported_and_resolved_either_way() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let a = instance(root.path(), "a", &remote);
    let b = instance(root.path(), "b", &remote);
    add_mock(&a, "orders");
    a.git.init("ann").await.expect("init a");
    a.git.commit(&a, "First", "ann").await.expect("commit a");
    a.git.push("ann").await.expect("push a");
    b.git.init("bob").await.expect("init b");
    b.git
        .pull(&b, true, false, true, "bob")
        .await
        .expect("pull b");
    // Both change and commit; A pushes first.
    set_title(&a, "A wins");
    a.git
        .commit(&a, "A's change", "ann")
        .await
        .expect("commit a2");
    a.git.push("ann").await.expect("push a2");
    set_title(&b, "B wins");
    b.git
        .commit(&b, "B's change", "bob")
        .await
        .expect("commit b2");
    let e = b.git.push("bob").await.expect_err("non fast-forward");
    assert_eq!(e.status, StatusCode::CONFLICT);
    assert_eq!(e.extra["conflict"], json!(true));
    assert_eq!(
        (e.extra["ahead"].clone(), e.extra["behind"].clone()),
        (json!(1), json!(1))
    );
    let e = b
        .git
        .pull(&b, true, false, true, "bob")
        .await
        .expect_err("diverged");
    assert_eq!(e.extra["options"], json!(["instance", "repository"]));
    assert!(e.message.contains("diverged"), "{}", e.message);
    // The repository wins: B takes A's configuration.
    let r = b
        .git
        .resolve(&b, "repository", "", "bob")
        .await
        .expect("resolve");
    assert_eq!(r["resolved"], json!("repository"));
    assert_eq!(
        b.settings.string("console.title").as_deref(),
        Some("A wins")
    );
    assert_eq!(r["head"]["subject"], json!("A's change"));
    // Now B's instance wins: its configuration replaces the repository's.
    set_title(&b, "B wins after all");
    set_title(&a, "A drifts");
    a.git.commit(&a, "A again", "ann").await.expect("commit a3");
    a.git.push("ann").await.expect("push a3");
    b.git.commit(&b, "B again", "bob").await.expect("commit b3");
    assert!(b.git.push("bob").await.is_err());
    let r = b
        .git
        .resolve(&b, "instance", "", "bob")
        .await
        .expect("resolve instance");
    assert_eq!(r["resolved"], json!("instance"));
    assert_eq!(r["push"]["pushed"], json!(true));
    let pull = a
        .git
        .pull(&a, true, false, true, "ann")
        .await
        .expect("a pulls");
    assert_eq!(pull["fast_forwarded"], json!(true));
    assert_eq!(
        a.settings.string("console.title").as_deref(),
        Some("B wins after all")
    );
    assert!(b.git.resolve(&b, "nope", "", "bob").await.is_err());
}

#[tokio::test]
async fn the_deploy_key_is_generated_once() {
    if !git_installed() {
        return;
    }
    if !std::process::Command::new("ssh-keygen")
        .arg("-?")
        .output()
        .is_ok()
    {
        eprintln!("ssh-keygen is not installed: deploy key test skipped");
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let state = instance(root.path(), "k", "git@github.com:acme/mocks.git");
    assert!(state.git.public_key().is_none());
    let key = state.git.generate_key(false, "t").await.expect("generate");
    let public = key["public"].as_str().expect("public");
    assert!(
        public.starts_with("ssh-ed25519 ") && public.ends_with("rustybin-test-k"),
        "{public}"
    );
    let e = state
        .git
        .generate_key(false, "t")
        .await
        .expect_err("exists");
    assert_eq!(e.status, StatusCode::CONFLICT);
    let replaced = state.git.generate_key(true, "t").await.expect("replace");
    assert_ne!(replaced["public"], key["public"]);
    assert_eq!(
        state.git.status(&state, false).await["deploy_key"]["public"],
        replaced["public"]
    );
    // An ssh remote with token auth is refused before anything runs.
    let e = state
        .git
        .commit(&state, "x", "t")
        .await
        .expect_err("auth mismatch");
    assert!(e.message.contains("deploy_key"), "{}", e.message);
}

#[tokio::test]
async fn startup_clones_and_applies_the_repository() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let a = instance(root.path(), "a", &remote);
    add_mock(&a, "orders");
    a.git.init("ann").await.expect("init");
    a.git.commit(&a, "First", "ann").await.expect("commit");
    a.git.push("ann").await.expect("push");

    let mut config = Config::for_tests();
    config.state_dir = Some(root.path().join("fresh"));
    config.egress_allow_loopback = true;
    config.config_git = Some(remote.clone());
    config.config_git_required = true;
    let fresh = test_state_with(config);
    // A mock the repository does not name goes (the repository is the truth).
    add_mock(&fresh, "stale");
    fresh.git.startup(&fresh).await.expect("startup");
    assert!(fresh.mocks.get("orders").is_some());
    assert!(fresh.mocks.get("stale").is_none());

    // An empty repository: nothing applied, not an error.
    let empty = bare_repo(&root.path().join("empty-root"));
    let mut config = Config::for_tests();
    config.state_dir = Some(root.path().join("fresh2"));
    config.egress_allow_loopback = true;
    config.config_git = Some(empty);
    config.config_git_required = true;
    let state = test_state_with(config);
    state.git.startup(&state).await.expect("empty repository");

    // A remote that cannot be reached: fatal only when required.
    let mut config = Config::for_tests();
    config.state_dir = Some(root.path().join("fresh3"));
    config.egress_allow_loopback = true;
    config.config_git = Some(format!("file://{}/missing.git", root.path().display()));
    config.config_git_required = true;
    let state = test_state_with(config);
    let e = state.git.startup(&state).await.expect_err("missing remote");
    assert!(e.contains("RUSTYBIN_CONFIG_GIT_REQUIRED"), "{e}");
    let mut config = Config::for_tests();
    config.state_dir = Some(root.path().join("fresh4"));
    config.egress_allow_loopback = true;
    config.config_git = Some(format!("file://{}/missing.git", root.path().display()));
    let state = test_state_with(config);
    state.git.startup(&state).await.expect("a warning only");
}

#[tokio::test]
async fn the_automatic_mode_pushes_changes_and_pulls() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let a = instance(root.path(), "a", &remote);
    let b = instance(root.path(), "b", &remote);
    a.git.init("ann").await.expect("init a");
    b.git.init("bob").await.expect("init b");
    let mut values = BTreeMap::new();
    values.insert(AUTO_KEY.to_string(), json!("push_on_change"));
    a.settings
        .update(values, BTreeMap::new(), "test")
        .expect("auto");
    let mut values = BTreeMap::new();
    values.insert(AUTO_KEY.to_string(), json!("pull"));
    values.insert(PULL_EVERY_KEY.to_string(), json!("1m"));
    b.settings
        .update(values, BTreeMap::new(), "test")
        .expect("auto");
    add_mock(&a, "orders");
    a.git.auto_tick(&a).await;
    let status = a.git.status(&a, true).await;
    assert_eq!(status["head"]["author"], json!("Rustybin"));
    assert!(
        status["head"]["subject"]
            .as_str()
            .expect("subject")
            .starts_with("Update mocks.orders: added"),
        "{status}"
    );
    assert_eq!(status["ahead"], json!(0));
    // Nothing changed: no new commit.
    a.git.auto_tick(&a).await;
    assert_eq!(a.git.history().await.expect("history").len(), 1);
    // B pulls on its first tick, then waits for the interval.
    b.git.auto_tick(&b).await;
    assert!(b.mocks.get("orders").is_some());
    add_mock(&a, "more");
    a.git.auto_tick(&a).await;
    b.git.auto_tick(&b).await;
    assert!(b.mocks.get("more").is_none(), "the interval has not passed");
}

#[tokio::test]
async fn the_routes_answer_and_say_what_is_missing() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let state = instance(root.path(), "r", &remote);
    add_mock(&state, "orders");
    let app = test_app_with(state.clone());
    let resp = app
        .clone()
        .oneshot(get_request("/_rustybin/git"))
        .await
        .expect("status");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["remote"]["kind"], json!("local"));
    assert_eq!(body["initialised"], json!(false));
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/git/commit",
            &json!({ "message": "x" }),
        ))
        .await
        .expect("commit");
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert!(body_json(resp).await["error"]
        .as_str()
        .expect("error")
        .contains("init"));
    let resp = app
        .clone()
        .oneshot(json_request("POST", "/_rustybin/git/init", &json!({})))
        .await
        .expect("init");
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/git/commit",
            &json!({ "message": "" }),
        ))
        .await
        .expect("empty");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/git/commit",
            &json!({ "message": "Via the API" }),
        ))
        .await
        .expect("commit");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["committed"], json!(true));
    let resp = app
        .clone()
        .oneshot(json_request("POST", "/_rustybin/git/push", &json!({})))
        .await
        .expect("push");
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/git/pull",
            &json!({ "apply": true, "dry_run": true }),
        ))
        .await
        .expect("pull");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["applied"]["dry_run"], json!(true));
    assert_eq!(body["applied"]["changes"], json!([]));
    let resp = app
        .clone()
        .oneshot(get_request("/_rustybin/git/history"))
        .await
        .expect("history");
    assert_eq!(
        body_json(resp).await["commits"]
            .as_array()
            .expect("commits")
            .len(),
        1
    );
    let resp = app
        .clone()
        .oneshot(get_request("/_rustybin/git/key"))
        .await
        .expect("key");
    assert_eq!(body_json(resp).await["deploy_key"], Value::Null);
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/git/resolve",
            &json!({ "with": "x" }),
        ))
        .await
        .expect("resolve");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let resp = app
        .oneshot(get_request("/_rustybin/git?fetch=true"))
        .await
        .expect("status");
    let body = body_json(resp).await;
    assert_eq!(body["fetched"], json!(true));
    assert_eq!(
        body["timeline"][0]["op"],
        json!("push"),
        "a dry run is not recorded"
    );
    assert_eq!(body["timeline"][0]["by"], json!("open"));
}
