//! Step 20 of the core next plan: a pull request opened through the git
//! host's API. Against bare repositories in a temporary directory and an
//! in-process fake of each host's API on loopback (`fake_host.rs`); no
//! test calls a real host.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::branches::{diverged, resolve_with};
use super::*;
use crate::egress::Purpose;
use crate::sourcecontrol::fake_host::{Fake, ECHO, READER, WRITER};
use crate::sourcecontrol::host::{self, Kind, Setup, Target};

const KINDS: [&str; 2] = ["github", "gitlab"];

/// The repository as the fake of `kind` is asked for it.
fn repository(kind: &str) -> &'static str {
    match kind {
        "github" => "acme/remote",
        _ => "acme%2Fremote",
    }
}

fn set(state: &AppState, values: &[(&str, Value)], secrets: &[(&str, Option<&str>)]) {
    let values: BTreeMap<String, Value> = values
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    let secrets: BTreeMap<String, Option<String>> = secrets
        .iter()
        .map(|(k, v)| (k.to_string(), v.map(str::to_string)))
        .collect();
    state
        .settings
        .update(values, secrets, "test")
        .expect("settings");
}

/// A diverged instance whose remote is `<root>/acme/remote.git`, and a
/// fake host of `kind` that knows that repository.
async fn with_host(root: &Path, kind: &'static str, token: Option<&str>) -> (AppState, Fake) {
    let acme = root.join("acme");
    std::fs::create_dir_all(&acme).expect("acme");
    let (_a, b) = diverged(&acme).await;
    let fake = Fake::start(kind, "acme/remote", &acme.join("remote.git")).await;
    set(
        &b,
        &[
            (HOST_KEY, json!(kind)),
            (HOST_API_KEY, json!(fake.api.clone())),
        ],
        &[(HOST_TOKEN_KEY, token)],
    );
    (b, fake)
}

fn remote_has(root: &Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .arg("--git-dir")
        .arg(root.join("acme").join("remote.git"))
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn open_to(branch: &str) -> Resolve {
    Resolve {
        with: "pull_request".into(),
        branch: Some(branch.into()),
        open: true,
        ..Resolve::default()
    }
}

/// A state with a remote and nothing else: what the settings resolve to.
fn configured(remote: &str) -> AppState {
    let state = test_state_with(Config::for_tests());
    set(&state, &[(REMOTE_KEY, json!(remote))], &[]);
    state
}

fn ready(state: &AppState) -> Target {
    match state.git.host_setup() {
        Setup::Ready(t) => t,
        other => panic!("not ready: {other:?}"),
    }
}

#[test]
fn auto_recognises_the_public_hosts_and_nothing_else() {
    // The two public hosts, by the remote's host name.
    let state = configured("https://github.com/acme/config.git");
    let t = ready(&state);
    assert_eq!(
        (t.kind, t.api.as_str(), t.repository.as_str(), t.token_from),
        (Kind::Github, "https://api.github.com", "acme/config", None)
    );
    // The remote's token serves the public API of the host it is on.
    set(&state, &[], &[(TOKEN_KEY, Some("remote-token-1234"))]);
    assert_eq!(ready(&state).token_from, Some(TOKEN_KEY));
    // A token of its own wins.
    set(&state, &[], &[(HOST_TOKEN_KEY, Some("host-token-1234"))]);
    assert_eq!(ready(&state).token_from, Some(HOST_TOKEN_KEY));
    let status = state.git.host_status();
    assert_eq!(status["ready"], json!(true), "{status}");
    assert_eq!(status["kind"], json!("github"));
    assert!(!status.to_string().contains("host-token-1234"), "{status}");

    // A project in a subgroup is one encoded path segment.
    let state = configured("git@gitlab.com:acme/team/config.git");
    let t = ready(&state);
    assert_eq!(
        (t.kind, t.api.as_str(), t.repository.as_str()),
        (
            Kind::Gitlab,
            "https://gitlab.com/api/v4",
            "acme%2Fteam%2Fconfig"
        )
    );
    let state = configured("ssh://git@github.com/acme/config.git");
    assert_eq!(ready(&state).repository, "acme/config");

    // Anything else is none under auto, whatever its name looks like.
    for remote in [
        "https://git.example.com/acme/config.git",
        "https://github.example.com/acme/config.git",
        "https://gitlab.com.example.org/acme/config.git",
        "git@github.com.evil.example:acme/config.git",
    ] {
        let state = configured(remote);
        assert!(
            matches!(state.git.host_setup(), Setup::Off(_)),
            "{remote}: {:?}",
            state.git.host_setup()
        );
        assert_eq!(state.git.host_status()["ready"], json!(false));
    }

    // A self-hosted host is named on purpose: the kind's usual API path on
    // the remote's host, or the base the settings name.
    let state = configured("https://git.example.com/acme/config.git");
    set(&state, &[(HOST_KEY, json!("github"))], &[]);
    assert_eq!(ready(&state).api, "https://git.example.com/api/v3");
    set(&state, &[(HOST_KEY, json!("gitlab"))], &[]);
    assert_eq!(ready(&state).api, "https://git.example.com/api/v4");
    set(
        &state,
        &[(HOST_API_KEY, json!("https://api.git.example.com/v4/"))],
        &[(TOKEN_KEY, Some("remote-token-1234"))],
    );
    let t = ready(&state);
    assert_eq!(t.api, "https://api.git.example.com/v4");
    // The remote's token never goes to another host than the remote's.
    assert_eq!(t.token_from, None);
    assert_eq!(state.git.host_status()["ready"], json!(false));
    set(&state, &[(HOST_API_KEY, json!(""))], &[]);
    assert_eq!(ready(&state).token_from, Some(TOKEN_KEY));

    // none never resolves to a host, public or not.
    let state = configured("https://github.com/acme/config.git");
    set(&state, &[(HOST_KEY, json!("none"))], &[]);
    assert!(matches!(state.git.host_setup(), Setup::Off(_)));

    // This kind names a repository owner/repo, nothing deeper.
    let state = configured("https://git.example.com/acme/team/config.git");
    set(&state, &[(HOST_KEY, json!("github"))], &[]);
    assert!(matches!(state.git.host_setup(), Setup::Problem(..)));
}

#[test]
fn the_outbound_rule_is_exactly_the_api_host() {
    let state = configured("https://github.com/acme/config.git");
    let _ = state.git.host_status();
    assert_eq!(
        state.egress.rule(Purpose::GitHost).allow.describe(),
        vec!["api.github.com:443".to_string()]
    );
    let check = |url: &str| state.egress.dry_check(url, Purpose::GitHost)["allowed"].clone();
    assert_eq!(
        check("https://api.github.com/repos/acme/config/pulls"),
        json!(true)
    );
    // Not the remote's own host, not another port, not a neighbour.
    for url in [
        "https://github.com/acme/config",
        "https://api.github.com:8443/repos",
        "https://uploads.github.com/x",
        "https://gitlab.com/api/v4",
    ] {
        assert_eq!(check(url), json!(false), "{url}");
    }
    // A named API replaces the rule; no host empties it.
    set(
        &state,
        &[(HOST_API_KEY, json!("https://git.example.com:8443/api/v3"))],
        &[],
    );
    let _ = state.git.host_status();
    assert_eq!(
        state.egress.rule(Purpose::GitHost).allow.describe(),
        vec!["git.example.com:8443".to_string()]
    );
    assert_eq!(check("https://api.github.com/repos"), json!(false));
    set(&state, &[(HOST_KEY, json!("none"))], &[]);
    let _ = state.git.host_status();
    assert!(state.egress.rule(Purpose::GitHost).allow.is_empty());
    // The remote's rule is its own purpose and is not this one.
    assert!(state
        .egress
        .dry_check("https://git.example.com:8443/api/v3", Purpose::Git)["allowed"]
        .eq(&json!(false)));
}

#[test]
fn the_api_setting_is_checked() {
    let state = configured("https://git.example.com/acme/config.git");
    let try_api = |api: &str| {
        let mut values = BTreeMap::new();
        values.insert(HOST_API_KEY.to_string(), json!(api));
        state.settings.update(values, BTreeMap::new(), "test")
    };
    for good in [
        "https://git.example.com/api/v3",
        "https://git.example.com:8443/api/v4/",
        "http://127.0.0.1:9000/api/v3",
        "http://localhost:9000/api/v3",
        "",
    ] {
        assert!(try_api(good).is_ok(), "{good:?} refused");
    }
    for bad in [
        // The token would cross the network in the clear.
        "http://git.example.com/api/v3",
        "https://user:pass@git.example.com/api/v3",
        "https://git.example.com/api/v3?x=1",
        "ftp://git.example.com/api",
        "git.example.com/api/v3",
        "https://git.example.com/_rustybin/git",
    ] {
        assert!(try_api(bad).is_err(), "{bad:?} accepted");
    }
    // A host kind is one of the names, nothing free.
    let mut values = BTreeMap::new();
    values.insert(HOST_KEY.to_string(), json!("gitea"));
    assert!(state
        .settings
        .update(values, BTreeMap::new(), "test")
        .is_err());
}

#[test]
fn the_text_of_a_pull_request_is_the_instances_own() {
    let change = |section: &'static str, key: &str, action: &'static str, detail: &str| Change {
        section,
        key: key.into(),
        action,
        detail: detail.into(),
    };
    let changes = vec![
        change("mocks", "orders", "added", "spec added"),
        change(
            "settings",
            "console.title",
            "changed",
            "THEIR-TITLE -> OUR-TITLE",
        ),
        change("mocks", "their-mock-only", "removed", "THEIR-DETAIL"),
        change("tests", "their-test-only", "removed", ""),
    ];
    let (title, body) = host::pull_request_text(
        "main",
        "review/b",
        "Take B's title\n\nmore lines",
        Some(&changes),
    );
    assert_eq!(title, "Take B's title");
    assert!(body.contains("`mocks.orders` (added)"), "{body}");
    assert!(
        body.contains("`settings.console.title` (changed)"),
        "{body}"
    );
    assert!(body.contains("holds 2 entries"), "{body}");
    // Nothing only the repository's side has is named, and no value of
    // either side is copied.
    for theirs in [
        "their-mock-only",
        "their-test-only",
        "THEIR-TITLE",
        "THEIR-DETAIL",
        "OUR-TITLE",
    ] {
        assert!(!body.contains(theirs), "{theirs} in {body}");
    }
    // Without a message the title is the instance's default; both are cut.
    let (title, _) = host::pull_request_text("main", "review/b", "  ", None);
    assert_eq!(title, "Rustybin: this instance's configuration into main");
    let many: Vec<Change> = (0..500)
        .map(|i| {
            change(
                "mocks",
                &format!("mock-{i}-{}", "x".repeat(100)),
                "added",
                "",
            )
        })
        .collect();
    let (title, body) = host::pull_request_text("main", "review/b", &"t".repeat(900), Some(&many));
    assert!(title.chars().count() <= host::MAX_TITLE + 3, "{title}");
    assert!(body.chars().count() <= host::MAX_BODY + 4);
    assert!(!title.contains('\n'));
}

#[tokio::test]
async fn a_pull_request_is_opened_with_the_right_base_and_head() {
    if !git_installed() {
        return;
    }
    for kind in KINDS {
        let root = tempfile::tempdir().expect("tempdir");
        let (b, fake) = with_host(root.path(), kind, Some(WRITER)).await;
        let mut req = open_to("review/b");
        req.message = "B's side, for review".into();
        let r = b.git.resolve(&b, &req, "bob").await.expect("resolve");
        assert_eq!(r["resolved"], json!("pull_request"), "{kind}: {r}");
        assert_eq!(r["pushed"], json!(true));
        assert_eq!(r["opened"], json!(true), "{kind}: {r}");
        assert_eq!(r["host"]["kind"], json!(kind));
        assert_eq!(r["host"]["repository"], json!(repository(kind)));
        // The address and the number are the host's answer.
        let pr = &r["untrusted_pull_request"];
        assert_eq!(pr["number"], json!(1), "{kind}: {r}");
        let url = pr["url"].as_str().expect("url");
        match kind {
            "github" => assert_eq!(url, "https://github.example/acme/remote/pull/1"),
            _ => assert_eq!(url, "https://gitlab.example/acme/remote/-/merge_requests/1"),
        }
        // They are not in the instance's own words.
        let detail = r["detail"].as_str().expect("detail");
        assert!(!detail.contains("example"), "{detail}");
        assert!(detail.contains("opened a pull request from review/b into main"));

        // One call, to the right place, naming the pushed branch and the
        // branch the instance is on.
        assert_eq!(fake.call_count(), 1, "{kind}");
        let call = fake.last_call();
        assert_eq!(call["authorization"], json!(format!("Bearer {WRITER}")));
        let (path, head, base, text) = match kind {
            "github" => ("/repos/acme/remote/pulls", "head", "base", "body"),
            _ => (
                "/projects/acme/remote/merge_requests",
                "source_branch",
                "target_branch",
                "description",
            ),
        };
        assert_eq!(call["path"], json!(path), "{kind}: {call}");
        assert_eq!(call["body"][head], json!("review/b"), "{kind}: {call}");
        assert_eq!(call["body"][base], json!("main"), "{kind}: {call}");
        assert_eq!(call["body"]["title"], json!("B's side, for review"));
        assert!(
            call["user_agent"]
                .as_str()
                .is_some_and(|ua| ua.starts_with("rustybin/")),
            "{call}"
        );
        if kind == "github" {
            assert_eq!(call["accept"], json!("application/vnd.github+json"));
            assert_eq!(call["api_version"], json!("2022-11-28"));
        } else {
            assert_eq!(call["accept"], json!("application/json"));
        }
        // The text is the instance's own: what it holds, by name. Nothing
        // of the repository's side (A's title, A's commit) is copied.
        let text = call["body"][text].as_str().expect("text");
        assert!(text.contains("Opened by a Rustybin instance"), "{text}");
        assert!(text.contains("(changed)"), "{text}");
        for theirs in ["A wins", "A's change", "B wins"] {
            assert!(!text.contains(theirs), "{theirs} in {text}");
        }
        // The branch is on the remote, the instance's branch as it was.
        assert!(remote_has(root.path(), "review/b"));
        assert_eq!(b.git.branch(), "main");
        // The status says where a pull request would be opened.
        let status = b.git.status(&b, false).await;
        assert_eq!(status["host"]["ready"], json!(true), "{status}");
        assert_eq!(status["host"]["token_from"], json!(HOST_TOKEN_KEY));
        assert_eq!(status["last"]["op"], json!("resolve"));
        assert_eq!(status["last"]["ok"], json!(true));

        // Without open nothing is asked of the host, as before the step.
        let mut plain = open_to("review/plain");
        plain.open = false;
        let r = b.git.resolve(&b, &plain, "bob").await.expect("plain");
        assert!(r.get("opened").is_none(), "{r}");
        assert!(r.get("untrusted_pull_request").is_none());
        assert_eq!(fake.call_count(), 1);
    }
}

#[tokio::test]
async fn a_host_that_does_not_open_it_leaves_the_branch_pushed() {
    if !git_installed() {
        return;
    }
    for kind in KINDS {
        let root = tempfile::tempdir().expect("tempdir");
        let (b, fake) = with_host(root.path(), kind, Some(READER)).await;

        // A token without the right.
        let r = b
            .git
            .resolve(&b, &open_to("review/reader"), "bob")
            .await
            .expect("resolve");
        assert_eq!(r["pushed"], json!(true), "{kind}: {r}");
        assert_eq!(r["opened"], json!(false), "{kind}: {r}");
        assert_eq!(r["not_opened"]["reason"], json!("not_permitted"));
        assert_eq!(r["not_opened"]["status"], json!(403));
        assert!(r.get("untrusted_pull_request").is_none(), "{r}");
        assert!(remote_has(root.path(), "review/reader"), "{kind}");
        // Why, in the instance's words; the host's own stay apart.
        let detail = r["detail"].as_str().expect("detail");
        assert!(
            detail.contains("the pull request was not opened")
                && detail.contains("The branch stays pushed")
                && detail.contains("open a pull request from review/reader into main"),
            "{detail}"
        );
        let said = r["not_opened"]["untrusted_host_message"]
            .as_str()
            .expect("the host's words");
        assert!(said.contains("orbidden") || said.contains("not accessible"));
        assert!(!detail.contains(said), "{detail}");

        // A token the host does not know.
        set(&b, &[], &[(HOST_TOKEN_KEY, Some("nobody-token-000000"))]);
        let r = b
            .git
            .resolve(&b, &open_to("review/nobody"), "bob")
            .await
            .expect("resolve");
        assert_eq!(r["opened"], json!(false), "{kind}: {r}");
        assert_eq!(r["not_opened"]["reason"], json!("token_refused"));
        assert!(remote_has(root.path(), "review/nobody"));

        // A branch the host does not know (it keeps another repository).
        set(&b, &[], &[(HOST_TOKEN_KEY, Some(WRITER))]);
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("elsewhere");
        bare_repo(&elsewhere);
        fake.keeps(&elsewhere.join("remote.git"));
        let r = b
            .git
            .resolve(&b, &open_to("review/unknown"), "bob")
            .await
            .expect("resolve");
        assert_eq!(r["opened"], json!(false), "{kind}: {r}");
        assert_eq!(r["not_opened"]["reason"], json!("refused"), "{kind}: {r}");
        assert!(remote_has(root.path(), "review/unknown"));
        fake.keeps(&root.path().join("acme").join("remote.git"));

        // A repository the host does not know.
        set(&b, &[(HOST_API_KEY, json!(fake.api.clone()))], &[]);
        let other = Fake::start(kind, "acme/other", &root.path().join("acme/remote.git")).await;
        set(&b, &[(HOST_API_KEY, json!(other.api.clone()))], &[]);
        let r = b
            .git
            .resolve(&b, &open_to("review/other"), "bob")
            .await
            .expect("resolve");
        assert_eq!(r["not_opened"]["reason"], json!("not_found"), "{kind}: {r}");
        assert!(remote_has(root.path(), "review/other"));
        set(&b, &[(HOST_API_KEY, json!(fake.api.clone()))], &[]);

        // A host that answers slowly: the call is given up, and the answer
        // does not claim to know what the host did.
        // (The host would take two minutes; the bound on the whole call is
        // generous because git runs under a loaded test suite.)
        fake.slow(Duration::from_secs(120));
        b.git.set_host_timeout(Duration::from_millis(300));
        let started = std::time::Instant::now();
        let r = b
            .git
            .resolve(&b, &open_to("review/slow"), "bob")
            .await
            .expect("resolve");
        assert!(started.elapsed() < Duration::from_secs(60), "waited");
        assert_eq!(r["opened"], json!(false), "{kind}: {r}");
        assert_eq!(r["not_opened"]["reason"], json!("timeout"), "{kind}: {r}");
        assert!(r["not_opened"]["why"]
            .as_str()
            .is_some_and(|w| w.contains("is not known")));
        assert!(remote_has(root.path(), "review/slow"));
        fake.slow(Duration::ZERO);
        b.git.set_host_timeout(Duration::from_secs(10));

        // A redirect is never followed: the API it points at is not called.
        set(
            &b,
            &[(HOST_API_KEY, json!(fake.redirecting_api.clone()))],
            &[],
        );
        let before = fake.call_count();
        let r = b
            .git
            .resolve(&b, &open_to("review/moved"), "bob")
            .await
            .expect("resolve");
        assert_eq!(
            r["not_opened"]["reason"],
            json!("redirected"),
            "{kind}: {r}"
        );
        assert_eq!(fake.call_count(), before + 1, "the redirect was followed");
        assert_eq!(fake.last_call()["redirected"], json!(true));
        assert!(remote_has(root.path(), "review/moved"));
        set(&b, &[(HOST_API_KEY, json!(fake.api.clone()))], &[]);

        // An answer that is not a pull request, and an address that is not
        // a web address: nothing of it is passed on as one.
        fake.answers(json!({ "ok": true }));
        let r = b
            .git
            .resolve(&b, &open_to("review/odd"), "bob")
            .await
            .expect("resolve");
        assert_eq!(
            r["not_opened"]["reason"],
            json!("bad_answer"),
            "{kind}: {r}"
        );
        fake.answers(json!({
            "number": 9, "iid": 9,
            "html_url": "javascript:alert(1)", "web_url": "javascript:alert(1)",
        }));
        let r = b
            .git
            .resolve(&b, &open_to("review/odd-address"), "bob")
            .await
            .expect("resolve");
        assert_eq!(r["opened"], json!(true), "{kind}: {r}");
        assert_eq!(r["untrusted_pull_request"]["number"], json!(9));
        assert_eq!(r["untrusted_pull_request"]["url"], Value::Null);

        // Every failure is in the timeline as what it was: the push went
        // through, so the operation did.
        let status = b.git.status(&b, false).await;
        assert_eq!(status["last"]["ok"], json!(true), "{status}");
    }
}

#[tokio::test]
async fn a_dry_run_calls_nothing() {
    if !git_installed() {
        return;
    }
    for kind in KINDS {
        let root = tempfile::tempdir().expect("tempdir");
        let (b, fake) = with_host(root.path(), kind, Some(WRITER)).await;
        let mut req = open_to("review/preview");
        req.dry_run = true;
        req.message = "For review".into();
        let r = b.git.resolve(&b, &req, "bob").await.expect("preview");
        assert_eq!(r["dry_run"], json!(true), "{kind}: {r}");
        assert_eq!(r["would_resolve"], json!("pull_request"));
        // What would be opened, and where.
        let would = &r["would_open"];
        assert_eq!(would["host"]["kind"], json!(kind), "{kind}: {r}");
        assert_eq!(would["host"]["api"], json!(fake.api.clone()));
        assert_eq!(would["host"]["repository"], json!(repository(kind)));
        assert_eq!(would["base"], json!("main"));
        assert_eq!(would["head"], json!("review/preview"));
        assert_eq!(would["title"], json!("For review"));
        assert!(would["body"]
            .as_str()
            .is_some_and(|b| b.contains("Opened by a Rustybin instance")));
        assert!(r["detail"]
            .as_str()
            .is_some_and(|d| d.contains("would then ask")));
        // Nothing was called, nothing was pushed, nothing is in the
        // timeline.
        assert_eq!(fake.call_count(), 0, "{kind}: the host was called");
        assert!(!remote_has(root.path(), "review/preview"));
        let status = b.git.status(&b, false).await;
        assert_ne!(status["last"]["op"], json!("resolve"), "{status}");
        // A preview without open says nothing of a host.
        req.open = false;
        let r = b.git.resolve(&b, &req, "bob").await.expect("preview");
        assert!(r.get("would_open").is_none(), "{r}");
    }
}

#[tokio::test]
async fn what_the_settings_cannot_serve_is_refused_before_anything_is_pushed() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let (b, fake) = with_host(root.path(), "github", None).await;
    let refused = |r: GitResult<Value>, reason: &str| {
        let e = r.expect_err("refused");
        assert_eq!(e.status, StatusCode::CONFLICT, "{}", e.message);
        assert_eq!(e.body()["reason"], json!(reason), "{}", e.message);
        e.message
    };
    let try_open = |dry_run: bool| {
        let mut req = open_to("review/refused");
        req.dry_run = dry_run;
        req
    };
    // No token: the call and its preview say so alike.
    for dry_run in [false, true] {
        let message = refused(
            b.git.resolve(&b, &try_open(dry_run), "bob").await,
            "host_token_missing",
        );
        assert!(message.contains(HOST_TOKEN_KEY), "{message}");
    }
    // The remote's token is not sent to a host that is not the remote's.
    set(&b, &[], &[(TOKEN_KEY, Some("remote-token-123456"))]);
    refused(
        b.git.resolve(&b, &try_open(false), "bob").await,
        "host_token_missing",
    );
    // No host: none, and auto with a remote it does not recognise.
    set(&b, &[], &[(HOST_TOKEN_KEY, Some(WRITER))]);
    for setting in ["none", "auto"] {
        set(&b, &[(HOST_KEY, json!(setting))], &[]);
        for dry_run in [false, true] {
            refused(
                b.git.resolve(&b, &try_open(dry_run), "bob").await,
                "host_not_set",
            );
        }
    }
    // An API the outbound policy never reaches: refused before any call.
    set(
        &b,
        &[
            (HOST_KEY, json!("github")),
            (HOST_API_KEY, json!("https://169.254.169.254/api/v3")),
        ],
        &[],
    );
    for dry_run in [false, true] {
        let message = refused(
            b.git.resolve(&b, &try_open(dry_run), "bob").await,
            "egress_refused",
        );
        assert!(message.contains("always refused"), "{message}");
    }
    // open goes with pull_request only.
    let mut wrong = resolve_with("instance");
    wrong.open = true;
    let e = b.git.resolve(&b, &wrong, "bob").await.expect_err("open");
    assert_eq!(e.status, StatusCode::BAD_REQUEST);
    // Through all of it: no call, no branch, and the instance's own
    // commit where it was. The refused calls are in the timeline as such.
    assert_eq!(fake.call_count(), 0);
    assert!(!remote_has(root.path(), "review/refused"));
    let status = b.git.status(&b, false).await;
    assert_eq!(status["ahead"], json!(1), "{status}");
    assert_eq!(status["last"]["op"], json!("resolve"), "{status}");
    assert_eq!(status["last"]["ok"], json!(false), "{status}");
}

#[tokio::test]
async fn a_host_outside_the_outbound_policy_is_refused_before_any_call() {
    for kind in KINDS {
        let root = tempfile::tempdir().expect("tempdir");
        let fake = Fake::start(kind, "acme/remote", root.path()).await;
        let mut config = Config::for_tests();
        config.egress_allow_loopback = true;
        let state = test_state_with(config);
        let pr = host::PullRequest {
            repository: repository(kind),
            base: "main",
            head: "review/b",
            title: "t",
            body: "b",
        };
        let target = Target {
            kind: if kind == "github" {
                Kind::Github
            } else {
                Kind::Gitlab
            },
            api: fake.api.clone(),
            repository: repository(kind).to_string(),
            token_from: Some(HOST_TOKEN_KEY),
        };
        // The purpose's rule names nothing (no host is set up), then
        // another host: the fake is reachable and still never called.
        let not = state
            .git
            .open_at_host(&target, WRITER, &pr)
            .await
            .expect_err("refused");
        assert_eq!(not.reason, "egress_refused", "{not:?}");
        set(
            &state,
            &[
                (REMOTE_KEY, json!("https://git.example.com/acme/remote.git")),
                (HOST_KEY, json!(kind)),
            ],
            &[],
        );
        let _ = state.git.host_status();
        let not = state
            .git
            .open_at_host(&target, WRITER, &pr)
            .await
            .expect_err("refused");
        assert_eq!(not.reason, "egress_refused", "{not:?}");
        assert!(!not.why.contains(WRITER));
        assert_eq!(fake.call_count(), 0, "{kind}: the host was called");
        // The refusals are decisions of the policy, under the purpose.
        let view = state.egress.view().to_string();
        assert!(view.contains("git_host"), "{view}");
        // Loopback is the policy's switch here too.
        set(
            &state,
            &[(HOST_API_KEY, json!(fake.api.clone()))],
            &[(HOST_TOKEN_KEY, Some(WRITER))],
        );
        assert!(state.git.host_ready().is_ok());
        set(&state, &[("egress.allow_loopback", json!(false))], &[]);
        let e = state.git.host_ready().expect_err("loopback");
        assert_eq!(e.body()["reason"], json!("egress_refused"));
        let not = state
            .git
            .open_at_host(&target, WRITER, &pr)
            .await
            .expect_err("refused");
        assert_eq!(not.reason, "egress_refused", "{not:?}");
        assert_eq!(fake.call_count(), 0);
    }
}

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut b) = self.0.lock() {
            b.extend_from_slice(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buffer {
    type Writer = Buffer;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn the_token_is_in_no_answer_log_line_or_audit_entry() {
    if !git_installed() {
        return;
    }
    // A second subscriber alive for the whole test: with only one, tracing
    // asks the thread that first reaches a log line whether anyone listens,
    // and that may be another test's thread, where nobody does.
    let _other = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    for kind in KINDS {
        let mut log_config = Config::for_tests();
        log_config.log_level = "debug".to_string();
        let logs = Buffer::default();
        let _guard = tracing::subscriber::set_default(crate::logging::subscriber(
            &log_config,
            logs.clone(),
            false,
        ));
        let root = tempfile::tempdir().expect("tempdir");
        let (b, fake) = with_host(root.path(), kind, Some(WRITER)).await;
        let app = test_app_with(b.clone());
        let mut seen = String::new();
        let call = async |method: &str, path: &str, body: Value| {
            let request = if method == "GET" {
                get_request(path)
            } else {
                json_request(method, path, &body)
            };
            let resp = app.clone().oneshot(request).await.expect("response");
            let status = resp.status();
            let body = body_json(resp).await;
            (status, body)
        };
        // A preview, an opened pull request, and each way a host refuses,
        // one of them in words that repeat the credential.
        let (status, body) = call(
            "POST",
            "/_rustybin/git/resolve",
            json!({ "with": "pull_request", "branch": "review/1", "open": true, "dry_run": true }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        seen.push_str(&body.to_string());
        let (status, body) = call(
            "POST",
            "/_rustybin/git/resolve",
            json!({ "with": "pull_request", "branch": "review/1", "open": true }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["opened"], json!(true), "{body}");
        seen.push_str(&body.to_string());
        for (n, token) in [READER, ECHO, "nobody-token-000000"]
            .into_iter()
            .enumerate()
        {
            set(&b, &[], &[(HOST_TOKEN_KEY, Some(token))]);
            let (status, body) = call(
                "POST",
                "/_rustybin/git/resolve",
                json!({ "with": "pull_request", "branch": format!("review/r{n}"), "open": true }),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["opened"], json!(false), "{body}");
            if token == ECHO {
                // The host's words are passed on, the credential in them
                // is not.
                let said = body["not_opened"]["untrusted_host_message"]
                    .as_str()
                    .expect("the host's words");
                assert!(said.contains("***"), "{said}");
            }
            seen.push_str(&body.to_string());
        }
        assert_eq!(fake.call_count(), 4);
        // A token that is set and a host that is not: refused, and said.
        set(&b, &[(HOST_KEY, json!("none"))], &[]);
        let (status, body) = call(
            "POST",
            "/_rustybin/git/resolve",
            json!({ "with": "pull_request", "branch": "review/none", "open": true }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["reason"], json!("host_not_set"));
        seen.push_str(&body.to_string());
        set(&b, &[(HOST_KEY, json!(kind))], &[]);
        // Everything that reads back: the status with its timeline, the
        // settings, the outbound policy's decisions, the audit log.
        for path in [
            "/_rustybin/git",
            "/_rustybin/settings",
            "/_rustybin/egress",
            "/_rustybin/audit?limit=200",
            "/_rustybin/config/export",
        ] {
            let resp = app
                .clone()
                .oneshot(get_request(path))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK, "{path}");
            let bytes = axum::body::to_bytes(resp.into_body(), 8 << 20)
                .await
                .expect("body");
            seen.push_str(&String::from_utf8_lossy(&bytes));
        }
        // The audit log has the calls, the opened one by its event.
        let (entries, _) = b.audit.list(&crate::audit::Filter {
            limit: 200,
            ..Default::default()
        });
        assert!(
            entries
                .iter()
                .any(|e| e.event.as_deref() == Some("git_pull_request_opened")),
            "no entry for the opened pull request"
        );
        assert!(entries
            .iter()
            .any(|e| e.event.as_deref() == Some("git_pull_request")));
        seen.push_str(&serde_json::to_string(&entries).expect("entries"));
        let logged = String::from_utf8_lossy(&logs.0.lock().expect("logs")).to_string();
        assert!(
            logged.contains("a pull request was opened at the git host"),
            "{logged}"
        );
        assert!(logged.contains("the git host did not open the pull request"));
        seen.push_str(&logged);
        for token in [WRITER, READER, ECHO, "nobody-token-000000"] {
            assert!(!seen.contains(token), "{kind}: {token} leaked");
        }
        // The address the host answered is in the answer and nowhere the
        // instance writes for itself.
        assert!(seen.contains("untrusted_pull_request"));
        assert!(!logged.contains("github.example") && !logged.contains("gitlab.example"));
    }
}

#[tokio::test]
async fn a_document_from_the_repository_never_names_the_host() {
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
    // Someone with write access to the repository points the instance at
    // an API of their own, with a token reference.
    let clone = root.path().join("writer");
    let run = |args: &[&str], cwd: &Path| {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Someone",
                "-c",
                "user.email=someone@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["clone", "--quiet", &remote, "writer"], root.path());
    let doc_path = clone.join("rustybin").join(DOCUMENT_NAME);
    let text = std::fs::read_to_string(&doc_path).expect("document");
    let text = format!(
        "{text}\nsettings:\n  git.host: gitlab\n  git.host_api: https://evil.example/api/v4\nsecrets:\n  git.host_token: ${{HOME}}\n"
    );
    std::fs::write(&doc_path, text).expect("write");
    run(
        &["commit", "--quiet", "-am", "Point the host elsewhere"],
        &clone,
    );
    run(&["push", "--quiet", "origin", "main"], &clone);
    let r = a.git.pull(&a, true, false, false, "ann").await;
    // Applied or refused as a whole, the three settings are as they were.
    let _ = r;
    assert_eq!(a.settings.string(HOST_KEY).as_deref(), Some("auto"));
    assert_eq!(a.settings.string(HOST_API_KEY), None);
    assert!(a.settings.secret(HOST_TOKEN_KEY).is_none());
}
