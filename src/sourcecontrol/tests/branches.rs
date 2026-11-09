//! Step 11 of the core next plan: a diverged branch says what differs and
//! is resolved only by a person (a side with a preview, or a pull request),
//! branches per environment, and what a document from the repository may
//! never change. Against bare repositories in a temporary directory.

use super::*;

/// Run git in `dir` as a person at the git host would (never the instance).
fn host_git(dir: &Path, args: &[&str]) -> String {
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
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn bare_path(root: &Path) -> std::path::PathBuf {
    root.join("remote.git")
}

/// A clone of the bare repository for the "someone else" of a test.
fn host_clone(root: &Path, name: &str) -> std::path::PathBuf {
    let dir = root.join(name);
    host_git(
        root,
        &[
            "clone",
            "--quiet",
            &format!("file://{}", bare_path(root).display()),
            name,
        ],
    );
    dir
}

/// Two instances on one branch, each with a commit the other lacks: A's is
/// on the remote, B's is local.
async fn diverged(root: &Path) -> (AppState, AppState) {
    let remote = bare_repo(root);
    let a = instance(root, "a", &remote);
    let b = instance(root, "b", &remote);
    add_mock(&a, "orders");
    a.git.init("ann").await.expect("init a");
    a.git.commit(&a, "First", "ann").await.expect("commit a");
    a.git.push("ann").await.expect("push a");
    b.git.init("bob").await.expect("init b");
    b.git
        .pull(&b, true, false, true, "bob")
        .await
        .expect("pull b");
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
    (a, b)
}

fn resolve_with(with: &str) -> Resolve {
    Resolve {
        with: with.into(),
        ..Default::default()
    }
}

fn lines(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| l.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn set(state: &AppState, key: &str, value: Value) {
    let mut values = BTreeMap::new();
    values.insert(key.to_string(), value);
    state
        .settings
        .update(values, BTreeMap::new(), "test")
        .expect("setting");
}

#[tokio::test]
async fn a_divergence_says_what_differs_and_the_automatic_mode_never_resolves() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let (_a, b) = diverged(root.path()).await;
    let remote_head = host_git(&bare_path(root.path()), &["rev-parse", "main"]);

    // The status says what differs, section by section, and the ways out.
    let status = b.git.status(&b, true).await;
    assert_eq!(
        (status["ahead"].clone(), status["behind"].clone()),
        (json!(1), json!(1))
    );
    let d = &status["divergence"];
    assert_eq!(
        d["options"],
        json!(["instance", "repository", "pull_request"])
    );
    assert!(
        lines(&d["local_commits"])
            .iter()
            .any(|l| l.ends_with("B's change")),
        "{d}"
    );
    assert!(
        lines(&d["remote_commits"])
            .iter()
            .any(|l| l.ends_with("A's change")),
        "{d}"
    );
    // The instance's side would write "B wins" over the repository's
    // document; the repository's side would bring "A wins" here.
    let instance = lines(&d["instance"]["summary"]);
    let repository = lines(&d["repository"]["summary"]);
    assert!(
        instance
            .iter()
            .any(|l| l.contains("title") && l.contains("B wins")),
        "{d}"
    );
    assert!(
        repository
            .iter()
            .any(|l| l.contains("title") && l.contains("A wins")),
        "{d}"
    );
    assert_eq!(b.git.diverged(), Some((1, 1)));

    // A pull's 409 carries the same, once explained for the answer.
    let e = b
        .git
        .pull(&b, true, false, true, "bob")
        .await
        .expect_err("diverged");
    assert_eq!(e.status, StatusCode::CONFLICT);
    assert_eq!(
        e.extra["options"],
        json!(["instance", "repository", "pull_request"])
    );
    let e = b.git.explain(&b, e).await;
    assert!(
        e.extra["divergence"]["repository"]["summary"].is_array(),
        "{}",
        e.extra
    );

    // The automatic mode, either way, never resolves: nothing is applied
    // here, nothing changes on the remote, and it keeps saying so.
    for mode in ["push_on_change", "pull"] {
        set(&b, AUTO_KEY, json!(mode));
        if mode == "push_on_change" {
            // Something more to push, so the tick has work.
            add_mock(&b, "more");
        }
        b.git.auto_tick(&b).await;
        assert_eq!(
            b.settings.string("console.title").as_deref(),
            Some("B wins"),
            "{mode}: nothing of the repository was applied"
        );
        assert_eq!(
            host_git(&bare_path(root.path()), &["rev-parse", "main"]),
            remote_head,
            "{mode}: the remote did not move"
        );
        let status = b.git.status(&b, true).await;
        assert!(status["divergence"].is_object(), "{mode}: {status}");
        assert_eq!(status["last"]["ok"], json!(false), "{mode}: {status}");
    }
    assert!(b.git.diverged().is_some());
    let report = crate::diagnostics::report(&b);
    let check = report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|c| c["id"] == json!("source_control_diverged"))
        .cloned()
        .expect("the diverged check");
    assert_eq!(check["status"], json!("warn"), "{check}");
}

#[tokio::test]
async fn a_side_is_previewed_and_the_instance_side_is_never_a_forced_push() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let (_a, b) = diverged(root.path()).await;
    let bare = bare_path(root.path());
    let remote_head = host_git(&bare, &["rev-parse", "main"]);
    let local_head = b.git.status(&b, false).await["head"]["sha"].clone();

    // The repository's side, previewed: the apply's own dry run, read from
    // the remote's commit. Nothing moves.
    let mut req = resolve_with("repository");
    req.dry_run = true;
    let preview = b.git.resolve(&b, &req, "bob").await.expect("preview");
    assert_eq!(preview["dry_run"], json!(true));
    assert_eq!(preview["would_resolve"], json!("repository"));
    assert_eq!(preview["applied"]["dry_run"], json!(true));
    assert!(
        lines(&preview["applied"]["summary"])
            .iter()
            .any(|l| l.contains("A wins")),
        "{preview}"
    );
    assert_eq!(
        b.settings.string("console.title").as_deref(),
        Some("B wins")
    );
    let status = b.git.status(&b, false).await;
    assert_eq!(status["head"]["sha"], local_head, "the working copy stayed");
    assert_eq!(
        status["timeline"][0]["op"],
        json!("commit"),
        "a preview is not an operation"
    );

    // The instance's side, previewed: what the commit would change there.
    let mut req = resolve_with("instance");
    req.dry_run = true;
    let preview = b.git.resolve(&b, &req, "bob").await.expect("preview");
    assert_eq!(preview["would_resolve"], json!("instance"));
    assert!(
        lines(&preview["summary"])
            .iter()
            .any(|l| l.contains("B wins")),
        "{preview}"
    );
    assert_eq!(host_git(&bare, &["rev-parse", "main"]), remote_head);

    // The instance's side for real: a new commit whose parent is the remote
    // head. The remote's history is all still there.
    let r = b
        .git
        .resolve(&b, &resolve_with("instance"), "bob")
        .await
        .expect("resolve");
    assert_eq!(r["resolved"], json!("instance"));
    assert_eq!(r["push"]["pushed"], json!(true));
    assert_eq!(
        host_git(&bare, &["rev-parse", "main^"]),
        remote_head,
        "the new commit sits on the remote head"
    );
    assert_eq!(
        host_git(&bare, &["rev-list", "--count", "main"]),
        "3",
        "First, A's change, and the instance's commit: nothing rewritten"
    );
    assert!(b.git.status(&b, true).await["divergence"].is_null());
    assert_eq!(b.git.diverged(), None);

    // A remote that moved in between refuses the push: never forced.
    let host = host_clone(root.path(), "host");
    std::fs::write(host.join("README.md"), "# notes\n").expect("readme");
    host_git(&host, &["add", "README.md"]);
    host_git(&host, &["commit", "--quiet", "-m", "A README"]);
    set_title(&b, "B again");
    b.git.commit(&b, "B again", "bob").await.expect("commit");
    host_git(&host, &["push", "--quiet", "origin", "main"]);
    let moved = host_git(&bare, &["rev-parse", "main"]);
    let e = b.git.push("bob").await.expect_err("the remote moved");
    assert_eq!(e.status, StatusCode::CONFLICT);
    assert_eq!(host_git(&bare, &["rev-parse", "main"]), moved);
}

#[tokio::test]
async fn the_instance_side_goes_to_a_new_branch_for_a_pull_request() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let (_a, b) = diverged(root.path()).await;
    let bare = bare_path(root.path());
    let remote_head = host_git(&bare, &["rev-parse", "main"]);

    // A preview names the branch and the commits, and pushes nothing.
    let mut req = resolve_with("pull_request");
    req.branch = Some("review/b-side".into());
    req.dry_run = true;
    let preview = b.git.resolve(&b, &req, "bob").await.expect("preview");
    assert_eq!(preview["branch"], json!("review/b-side"));
    assert_eq!(preview["base"], json!("main"));
    assert!(
        lines(&preview["commits"])
            .iter()
            .any(|l| l.ends_with("B's change")),
        "{preview}"
    );
    assert_eq!(
        host_git(&bare, &["branch", "--list", "review/b-side"]),
        "",
        "nothing pushed by a preview"
    );

    req.dry_run = false;
    let r = b.git.resolve(&b, &req, "bob").await.expect("pull request");
    assert_eq!(r["resolved"], json!("pull_request"));
    assert_eq!(r["branch"], json!("review/b-side"));
    assert_eq!(r["base"], json!("main"));
    assert_eq!(r["applied"], Value::Null);
    assert!(
        r["detail"]
            .as_str()
            .expect("detail")
            .contains("review/b-side"),
        "{r}"
    );
    // The new branch holds B's side; main on the remote is untouched,
    // nothing was applied here, and B is still on main and still diverged.
    assert_eq!(
        host_git(&bare, &["log", "-1", "--format=%s", "review/b-side"]),
        "B's change"
    );
    assert_eq!(host_git(&bare, &["rev-parse", "main"]), remote_head);
    assert_eq!(
        b.settings.string("console.title").as_deref(),
        Some("B wins")
    );
    assert_eq!(b.git.branch(), "main");
    assert!(b.git.status(&b, true).await["divergence"].is_object());

    // A branch of that name with other commits is refused, never forced.
    host_git(&bare, &["branch", "taken", "main"]);
    let mut taken = resolve_with("pull_request");
    taken.branch = Some("taken".into());
    let e = b.git.resolve(&b, &taken, "bob").await.expect_err("taken");
    assert_eq!(e.status, StatusCode::CONFLICT);
    assert!(e.message.contains("name another branch"), "{}", e.message);
    assert_eq!(host_git(&bare, &["rev-parse", "taken"]), remote_head);
    // The branch the instance is on is not a pull request's branch.
    let mut same = resolve_with("pull_request");
    same.branch = Some("main".into());
    let e = b.git.resolve(&b, &same, "bob").await.expect_err("same");
    assert_eq!(e.status, StatusCode::BAD_REQUEST);
    // Without a name one is made; what is not committed yet goes with it.
    set_title(&b, "B wins, again");
    let r = b
        .git
        .resolve(&b, &resolve_with("pull_request"), "bob")
        .await
        .expect("made name");
    let made = r["branch"].as_str().expect("branch").to_string();
    assert!(made.starts_with("main-instance-"), "{made}");
    assert_eq!(r["commit"]["committed"], json!(true));
    assert_eq!(
        host_git(&bare, &["log", "-1", "--format=%s", &made]),
        "This instance's configuration, for a pull request"
    );

    // A person merges the pull request at the host (taking B's side where
    // both changed the title): the local commits are now in main's history,
    // so an ordinary pull fast-forwards and applies.
    let host = host_clone(root.path(), "host");
    host_git(
        &host,
        &[
            "merge",
            "--quiet",
            "--no-ff",
            "-X",
            "theirs",
            "-m",
            "Merge the instance's side",
            &format!("origin/{made}"),
        ],
    );
    host_git(&host, &["push", "--quiet", "origin", "main"]);
    let pull = b
        .git
        .pull(&b, true, false, true, "bob")
        .await
        .expect("pull after the merge");
    assert_eq!(pull["fast_forwarded"], json!(true));
    assert_eq!(pull["ahead"], json!(0));
    assert_eq!(
        b.settings.string("console.title").as_deref(),
        Some("B wins, again")
    );
    assert!(b.git.status(&b, true).await["divergence"].is_null());
}

#[tokio::test]
async fn branches_are_listed_and_switched_with_a_preview() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let bare = bare_path(root.path());
    let a = instance(root.path(), "a", &remote);
    add_mock(&a, "orders");
    set_title(&a, "Production");
    a.git.init("ann").await.expect("init");
    a.git.commit(&a, "Production", "ann").await.expect("commit");
    a.git.push("ann").await.expect("push");

    // A second instance starts the staging branch from what it holds.
    let s = instance(root.path(), "s", &remote);
    s.git.init("sam").await.expect("init s");
    s.git
        .pull(&s, true, false, true, "sam")
        .await
        .expect("pull s");
    set_title(&s, "Staging");
    add_mock(&s, "beta");
    let start = BranchRequest {
        branch: "staging".into(),
        create: true,
        apply: true,
        ..Default::default()
    };
    let mut preview = start.clone();
    preview.dry_run = true;
    let would = s
        .git
        .switch_branch(&s, &preview, "sam")
        .await
        .expect("preview of a new branch");
    assert_eq!(would["would_create"], json!("staging"));
    assert!(
        lines(&would["summary"])
            .iter()
            .any(|l| l == "mocks.beta: added"),
        "{would}"
    );
    assert_eq!(s.git.branch(), "main", "a preview changes nothing");
    assert_eq!(host_git(&bare, &["branch", "--list", "staging"]), "");
    let created = s
        .git
        .switch_branch(&s, &start, "sam")
        .await
        .expect("start staging");
    assert_eq!(created["created"], json!(true));
    assert_eq!(created["push"]["pushed"], json!(true));
    assert_eq!(s.git.branch(), "staging");
    assert_eq!(
        host_git(&bare, &["log", "-1", "--format=%s", "staging"]),
        "Start staging from this instance"
    );
    assert_eq!(
        host_git(&bare, &["log", "-1", "--format=%s", "main"]),
        "Production",
        "main is as it was"
    );
    // It exists now: starting it again is refused.
    let other = instance(root.path(), "o", &remote);
    other.git.init("o").await.expect("init o");
    let e = other
        .git
        .switch_branch(&other, &start, "o")
        .await
        .expect_err("exists");
    assert_eq!(e.status, StatusCode::CONFLICT);
    assert!(e.message.contains("switch to it"), "{}", e.message);
    assert_eq!(other.git.branch(), "main");

    // The first instance lists the remote's branches.
    let list = a.git.branches().await.expect("branches");
    assert_eq!(list["branch"], json!("main"));
    let names: Vec<(&str, bool)> = list["branches"]
        .as_array()
        .expect("branches")
        .iter()
        .map(|b| {
            (
                b["name"].as_str().unwrap_or(""),
                b["current"].as_bool().unwrap_or(false),
            )
        })
        .collect();
    assert_eq!(names, vec![("main", true), ("staging", false)]);
    assert_eq!(list["truncated"], json!(false));

    // The preview of a switch: the apply's dry run of that branch's
    // document. Nothing changes: not the setting, not the working copy.
    let switch = BranchRequest {
        branch: "staging".into(),
        apply: true,
        prune: true,
        ..Default::default()
    };
    let mut preview = switch.clone();
    preview.dry_run = true;
    let would = a
        .git
        .switch_branch(&a, &preview, "ann")
        .await
        .expect("preview");
    assert_eq!(would["dry_run"], json!(true));
    assert_eq!(would["applied"]["dry_run"], json!(true));
    assert_eq!(
        would["applied"]["results"]["mocks"]["would_create"],
        json!(["beta"])
    );
    assert_eq!(a.git.branch(), "main");
    assert!(a.mocks.get("beta").is_none());
    assert_eq!(
        a.git.status(&a, false).await["head"]["subject"],
        json!("Production")
    );

    // The switch: the setting is stored, the working copy moves, the
    // document is applied.
    let done = a
        .git
        .switch_branch(&a, &switch, "ann")
        .await
        .expect("switch");
    assert_eq!(done["switched"], json!(true));
    assert_eq!(done["from"], json!("main"));
    assert_eq!(done["applied"]["ok"], json!(true), "{done}");
    assert_eq!(a.git.branch(), "staging");
    assert_eq!(
        a.settings.get(BRANCH_KEY),
        (json!("staging"), crate::settings::Source::Stored)
    );
    assert!(a.mocks.get("beta").is_some());
    assert_eq!(
        a.settings.string("console.title").as_deref(),
        Some("Staging")
    );
    let status = a.git.status(&a, true).await;
    assert_eq!(
        status["head"]["subject"],
        json!("Start staging from this instance")
    );
    assert_eq!(status["branch"], json!("staging"));
    assert_eq!(
        (status["ahead"].clone(), status["behind"].clone()),
        (json!(0), json!(0))
    );
    // Commits and pushes now go to staging; main stays.
    set_title(&a, "Staging, edited");
    a.git
        .commit(&a, "Edit staging", "ann")
        .await
        .expect("commit");
    a.git.push("ann").await.expect("push");
    assert_eq!(
        host_git(&bare, &["log", "-1", "--format=%s", "staging"]),
        "Edit staging"
    );
    assert_eq!(
        host_git(&bare, &["log", "-1", "--format=%s", "main"]),
        "Production"
    );

    // What is refused: the branch it is on, one the remote lacks, a bad name.
    let e = a
        .git
        .switch_branch(&a, &switch, "ann")
        .await
        .expect_err("same");
    assert_eq!(e.status, StatusCode::CONFLICT);
    let mut missing = switch.clone();
    missing.branch = "nope".into();
    let e = a
        .git
        .switch_branch(&a, &missing, "ann")
        .await
        .expect_err("missing");
    assert_eq!(e.status, StatusCode::CONFLICT);
    assert!(e.message.contains("create: true"), "{}", e.message);
    assert_eq!(a.git.branch(), "staging");
    let mut bad = switch.clone();
    bad.branch = "--upload-pack=x".into();
    let e = a
        .git
        .switch_branch(&a, &bad, "ann")
        .await
        .expect_err("bad name");
    assert_eq!(e.status, StatusCode::BAD_REQUEST);

    // Back to main without applying: the branch only.
    let back = BranchRequest {
        branch: "main".into(),
        apply: false,
        ..Default::default()
    };
    let done = a.git.switch_branch(&a, &back, "ann").await.expect("back");
    assert_eq!(done["applied"], Value::Null);
    assert_eq!(a.git.branch(), "main");
    assert_eq!(
        a.settings.string("console.title").as_deref(),
        Some("Staging, edited"),
        "nothing applied"
    );
    assert_eq!(
        a.git.status(&a, false).await["head"]["subject"],
        json!("Production")
    );
}

#[tokio::test]
async fn a_new_branch_starts_on_an_empty_repository_and_a_failed_switch_goes_back() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let bare = bare_path(root.path());
    let a = instance(root.path(), "a", &remote);
    add_mock(&a, "orders");
    let init = a.git.init("ann").await.expect("init");
    assert_eq!(init["remote_branch"], json!(false));
    // Nothing committed anywhere yet: the first branch can be any name.
    let created = a
        .git
        .switch_branch(
            &a,
            &BranchRequest {
                branch: "dev".into(),
                create: true,
                message: "The dev environment".into(),
                ..Default::default()
            },
            "ann",
        )
        .await
        .expect("start dev");
    assert_eq!(created["branch"], json!("dev"));
    assert_eq!(a.git.branch(), "dev");
    assert_eq!(
        host_git(&bare, &["log", "-1", "--format=%s", "dev"]),
        "The dev environment"
    );
    assert_eq!(host_git(&bare, &["branch", "--list", "main"]), "");

    // A branch whose document cannot be read is not switched to: the
    // setting and the working copy stay where they were.
    let host = host_clone(root.path(), "host");
    host_git(
        &host,
        &["checkout", "--quiet", "-b", "broken", "origin/dev"],
    );
    host_git(&host, &["rm", "-r", "--quiet", "rustybin"]);
    std::fs::write(host.join("README.md"), "no document here\n").expect("readme");
    host_git(&host, &["add", "README.md"]);
    host_git(&host, &["commit", "--quiet", "-m", "No document"]);
    host_git(&host, &["push", "--quiet", "origin", "broken"]);
    let e = a
        .git
        .switch_branch(
            &a,
            &BranchRequest {
                branch: "broken".into(),
                apply: true,
                ..Default::default()
            },
            "ann",
        )
        .await
        .expect_err("no document on that branch");
    assert_eq!(e.status, StatusCode::CONFLICT);
    assert!(e.message.contains("still on dev"), "{}", e.message);
    assert_eq!(a.git.branch(), "dev");
    assert_eq!(
        a.git.status(&a, false).await["head"]["subject"],
        json!("The dev environment")
    );
}

#[tokio::test]
async fn a_pulled_document_never_changes_git_settings() {
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
    // The instance has a webhook secret, a signing key and signing on.
    let mut values = BTreeMap::new();
    values.insert(SIGN_KEY.to_string(), json!(true));
    let mut secrets = BTreeMap::new();
    secrets.insert(
        WEBHOOK_SECRET_KEY.to_string(),
        Some("a-long-webhook-secret".to_string()),
    );
    a.settings
        .update(values, secrets, "test")
        .expect("git settings");
    let signing_key = ssh_keygen_installed();
    if signing_key {
        a.git
            .generate_signing_key(false, "test")
            .await
            .expect("signing key");
    }
    // Its own export never names the group.
    let yaml = crate::configdoc::export(&a, crate::configdoc::SecretsMode::Refs, false).yaml();
    assert!(!yaml.contains("git."), "{yaml}");

    // Someone who can push writes a document that tries every one.
    let host = host_clone(root.path(), "host");
    std::fs::write(
        host.join("rustybin/rustybin.yaml"),
        "version: 2\nsettings:\n  git.branch: evil\n  git.remote: https://evil.example/x.git\n  git.auto: push_on_change\n  git.sign: false\n  git.path: elsewhere\n  console.title: From the repository\nsecrets:\n  git.token: ${RUSTYBIN_CONFIG_GIT_TOKEN}\n  git.webhook_secret: null\n  git.signing_key: null\n",
    )
    .expect("document");
    host_git(&host, &["add", "-A"]);
    host_git(&host, &["commit", "--quiet", "-m", "Take over"]);
    host_git(&host, &["push", "--quiet", "origin", "main"]);

    // The dry run and the apply say the same, and nothing of git.* moves.
    for dry_run in [true, false] {
        let pull = a
            .git
            .pull(&a, true, dry_run, true, "ann")
            .await
            .expect("pull");
        let problems = lines(&pull["applied"]["problems"]);
        for key in [
            "settings.git.branch",
            "settings.git.remote",
            "settings.git.auto",
            "settings.git.sign",
            "settings.git.path",
            "secrets.git.token",
            "secrets.git.webhook_secret",
            "secrets.git.signing_key",
        ] {
            assert!(
                problems.iter().any(|p| p.starts_with(&format!("{key}:"))
                    && p.contains("a document from the repository cannot change it")),
                "dry_run {dry_run}: {key} is not refused: {problems:?}"
            );
        }
        assert_eq!(pull["applied"]["ok"], json!(false));
    }
    assert_eq!(a.git.branch(), "main");
    assert_eq!(
        a.settings.string(REMOTE_KEY).as_deref(),
        Some(remote.as_str())
    );
    assert_eq!(a.settings.string(AUTO_KEY).as_deref(), Some("off"));
    assert_eq!(a.git.path().as_deref(), Some("rustybin"));
    assert!(a.settings.bool(SIGN_KEY));
    assert!(a.git.webhook_secret().is_some());
    assert_eq!(a.git.signing()["key_set"], json!(signing_key));
    assert_eq!(a.settings.secret_state(TOKEN_KEY)["set"], json!(false));
    // The rest of the document applied.
    assert_eq!(
        a.settings.string("console.title").as_deref(),
        Some("From the repository")
    );
}

/// An instance in harness mode (saved scenarios apply only there).
fn harness_instance(root: &Path, name: &str, remote: &str) -> AppState {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("state dir");
    let mut config = Config::for_tests();
    config.state_dir = Some(dir);
    config.egress_allow_loopback = true;
    config.instance_id = format!("test-{name}");
    config.harness.enabled = true;
    config.harness.roles = vec![crate::harness::Role::Client, crate::harness::Role::Server];
    config.harness.targets = vec!["127.0.0.1".into()];
    config.harness.threads = Some(1);
    let state = test_state_with(config);
    set(&state, REMOTE_KEY, json!(remote));
    state
}

const SCENARIO: &str = "name: smoke\ntarget: http://127.0.0.1:8080\nsource: { request: { path: /status/200 } }\npattern: { type: constant, rate: 1, for: 1s }\n";

fn saved(state: &AppState, name: &str) -> Option<crate::harness::saved::Saved> {
    state.harness.as_ref().and_then(|h| h.saved.get(name))
}

#[tokio::test]
async fn a_scenario_file_is_read_like_a_spec_file() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let a = harness_instance(root.path(), "a", &remote);
    a.git.init("ann").await.expect("init");
    a.git.commit(&a, "First", "ann").await.expect("commit");
    a.git.push("ann").await.expect("push");
    // A person keeps the scenario in a file of its own next to the document.
    let host = host_clone(root.path(), "host");
    std::fs::create_dir_all(host.join("rustybin/scenarios")).expect("dir");
    std::fs::write(host.join("rustybin/scenarios/smoke.yaml"), SCENARIO).expect("scenario");
    std::fs::write(
        host.join("rustybin/rustybin.yaml"),
        "version: 2\nharness:\n  scenarios:\n    - name: smoke\n      file: scenarios/smoke.yaml\n",
    )
    .expect("document");
    host_git(&host, &["add", "-A"]);
    host_git(&host, &["commit", "--quiet", "-m", "A scenario file"]);
    host_git(&host, &["push", "--quiet", "origin", "main"]);

    // The dry run reads the file from the commit (the working copy has not
    // moved) and says what the apply says.
    let dry = a
        .git
        .pull(&a, true, true, false, "ann")
        .await
        .expect("dry run");
    assert_eq!(dry["applied"]["problems"], json!([]), "{dry}");
    assert_eq!(
        dry["applied"]["results"]["harness"]["save"],
        json!(["smoke"])
    );
    assert!(
        lines(&dry["applied"]["summary"])
            .iter()
            .any(|l| l == "harness.smoke: added"),
        "{dry}"
    );
    assert!(saved(&a, "smoke").is_none());
    let pull = a
        .git
        .pull(&a, true, false, false, "ann")
        .await
        .expect("pull");
    assert_eq!(pull["applied"]["ok"], json!(true), "{pull}");
    assert_eq!(
        pull["applied"]["results"]["harness"]["saved"],
        json!(["smoke"])
    );
    let kept = saved(&a, "smoke").expect("the scenario is saved");
    assert_eq!(kept.scenario.name, "smoke");
    // The drift against a document with a file reads the file too.
    let status = a.git.status(&a, false).await;
    assert!(status["drift"]["error"].is_null(), "{status}");
}

#[tokio::test]
async fn a_scenario_file_never_leaves_the_repository() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let a = harness_instance(root.path(), "a", &remote);
    // A file of the host that a repository writer would like to read: it
    // parses as a scenario, so only the path check keeps it out.
    let outside = root.path().join("outside.yaml");
    std::fs::write(&outside, SCENARIO.replace("smoke", "stolen")).expect("outside");
    a.git.init("ann").await.expect("init");
    a.git.commit(&a, "First", "ann").await.expect("commit");
    a.git.push("ann").await.expect("push");
    // A scenario saved here that the document will name and fail to load.
    let h = a.harness.as_ref().expect("harness");
    let scenario = crate::harness::scenario::Scenario::parse(SCENARIO).expect("scenario");
    h.saved.put("kept", scenario).expect("save");

    let host = host_clone(root.path(), "host");
    std::fs::create_dir_all(host.join("rustybin/scenarios")).expect("dir");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, host.join("rustybin/scenarios/link.yaml"))
        .expect("symlink");
    let big = format!(
        "{SCENARIO}description: \"{}\"\n",
        "x".repeat(crate::harness::scenario::MAX_SCENARIO_BYTES)
    );
    std::fs::write(host.join("rustybin/scenarios/big.yaml"), big).expect("big");
    std::fs::write(
        host.join("rustybin/rustybin.yaml"),
        format!(
            "version: 2\nharness:\n  scenarios:\n    - name: up\n      file: ../../outside.yaml\n    - name: absolute\n      file: {}\n    - name: link\n      file: scenarios/link.yaml\n    - name: kept\n      file: scenarios/missing.yaml\n    - name: both\n      file: scenarios/missing.yaml\n      scenario: {{}}\n",
            outside.display()
        ),
    )
    .expect("document");
    host_git(&host, &["add", "-A"]);
    host_git(&host, &["commit", "--quiet", "-m", "Reach out"]);
    host_git(&host, &["push", "--quiet", "origin", "main"]);

    for dry_run in [true, false] {
        let pull = a
            .git
            .pull(&a, true, dry_run, true, "ann")
            .await
            .expect("pull");
        let problems = lines(&pull["applied"]["problems"]);
        let has = |name: &str, what: &str| {
            problems
                .iter()
                .any(|p| p.starts_with(&format!("harness.scenarios.{name}:")) && p.contains(what))
        };
        assert!(has("up", "inside the repository"), "{problems:?}");
        assert!(has("absolute", "inside the repository"), "{problems:?}");
        assert!(has("both", "not both"), "{problems:?}");
        assert!(has("kept", "scenarios/missing.yaml"), "{problems:?}");
        // A link is checked out as a plain file holding its target path
        // (core.symlinks=false): not a scenario, and never followed.
        assert!(
            problems
                .iter()
                .any(|p| p.starts_with("harness.scenarios.link:")),
            "{problems:?}"
        );
    }
    for name in ["up", "absolute", "link", "both", "stolen"] {
        assert!(saved(&a, name).is_none(), "{name} was saved");
    }
    // Named but not loaded: never pruned.
    assert!(
        saved(&a, "kept").is_some(),
        "a prune took what it could not load"
    );

    // A file over a scenario's size is an error of the read, never a cut file.
    std::fs::write(
        host.join("rustybin/rustybin.yaml"),
        "version: 2\nharness:\n  scenarios:\n    - name: big\n      file: scenarios/big.yaml\n",
    )
    .expect("document");
    host_git(&host, &["add", "-A"]);
    host_git(&host, &["commit", "--quiet", "-m", "A huge scenario"]);
    host_git(&host, &["push", "--quiet", "origin", "main"]);
    let e = a
        .git
        .pull(&a, true, true, false, "ann")
        .await
        .expect_err("too large");
    assert!(e.message.contains("larger than"), "{}", e.message);
    assert!(saved(&a, "big").is_none());
}

#[tokio::test]
async fn the_branch_and_resolve_routes_answer() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let (_a, b) = diverged(root.path()).await;
    let app = test_app_with(b.clone());
    let post = |path: &'static str, body: Value| {
        let app = app.clone();
        async move {
            let resp = app
                .oneshot(json_request("POST", path, &body))
                .await
                .expect("response");
            let status = resp.status();
            (status, body_json(resp).await)
        }
    };
    // A diverged pull names what differs and the three ways out.
    let (status, body) = post("/_rustybin/git/pull", json!({ "apply": true })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["conflict"], json!(true));
    assert_eq!(
        body["options"],
        json!(["instance", "repository", "pull_request"])
    );
    assert!(
        body["divergence"]["instance"]["summary"].is_array(),
        "{body}"
    );
    let (status, body) = post("/_rustybin/git/push", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["divergence"].is_object(), "{body}");
    // Previews through the route.
    let (status, body) = post(
        "/_rustybin/git/resolve",
        json!({ "with": "repository", "dry_run": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"]["dry_run"], json!(true));
    let (status, body) = post(
        "/_rustybin/git/resolve",
        json!({ "with": "pull_request", "branch": "review/b" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["branch"], json!("review/b"));
    let (status, _) = post("/_rustybin/git/resolve", json!({ "with": "merge" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Branches.
    let resp = app
        .clone()
        .oneshot(get_request("/_rustybin/git/branches"))
        .await
        .expect("branches");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let names: Vec<&str> = body["branches"]
        .as_array()
        .expect("branches")
        .iter()
        .filter_map(|b| b["name"].as_str())
        .collect();
    assert_eq!(names, vec!["main", "review/b"]);
    let (status, body) = post(
        "/_rustybin/git/branch",
        json!({ "branch": "review/b", "dry_run": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"]["dry_run"], json!(true));
    let (status, _) = post("/_rustybin/git/branch", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a branch is required");
    let (status, _) = post("/_rustybin/git/branch", json!({ "name": "x" })).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unknown fields are refused"
    );
    // The signing key: nothing yet, and nothing secret in the answer.
    let resp = app
        .clone()
        .oneshot(get_request("/_rustybin/git/signing-key"))
        .await
        .expect("signing key");
    let body = body_json(resp).await;
    assert_eq!(body["signing"]["enabled"], json!(false));
    assert_eq!(body["signing"]["key_set"], json!(false));
    assert_eq!(body["signing"]["public"], Value::Null);
}

const SPEC_TWO: &str = r#"{"openapi":"3.0.3","info":{"title":"Orders","version":"2"},"paths":{"/orders":{"get":{"operationId":"listOrders","responses":{"200":{"description":"ok"}}},"post":{"operationId":"createOrder","responses":{"201":{"description":"ok"}}}}}}"#;

/// What differs is only as good as the comparison: a spec that changed on
/// the instance is a change against the working copy's file (the automatic
/// mode commits it, a divergence names it).
#[tokio::test]
async fn a_changed_spec_is_a_change_against_the_working_copy() {
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
    assert_eq!(a.git.status(&a, false).await["drift"]["changes"], json!([]));
    let report = a.mocks.apply(
        vec![Definition {
            name: "orders".into(),
            source: SPEC_TWO.into(),
            settings: Default::default(),
            enabled: true,
            project: None,
        }],
        Origin::Api,
        None,
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let status = a.git.status(&a, false).await;
    assert_eq!(
        status["drift"]["summary"],
        json!(["mocks.orders: spec changed (1 operation(s) added)"]),
        "{status}"
    );
    // So the automatic mode commits and pushes it.
    set(&a, AUTO_KEY, json!("push_on_change"));
    a.git.auto_tick(&a).await;
    assert_eq!(
        host_git(&bare_path(root.path()), &["rev-list", "--count", "main"]),
        "2"
    );
    assert_eq!(a.git.status(&a, false).await["drift"]["changes"], json!([]));
}

/// A title only one side has: the other side's document has no `instance:`
/// section at all (an export leaves an empty one out), and the divergence
/// still names it on both sides.
#[tokio::test]
async fn a_divergence_names_what_only_one_side_has() {
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
    add_mock(&a, "more");
    a.git.commit(&a, "A mock", "ann").await.expect("commit a2");
    a.git.push("ann").await.expect("push a2");
    set_title(&b, "Only here");
    b.git.commit(&b, "A title", "bob").await.expect("commit b2");
    let status = b.git.status(&b, true).await;
    let d = &status["divergence"];
    let instance = lines(&d["instance"]["summary"]);
    let repository = lines(&d["repository"]["summary"]);
    assert!(
        instance
            .iter()
            .any(|l| l.starts_with("instance.title: set to") && l.contains("Only here")),
        "{d}"
    );
    assert!(instance.iter().any(|l| l == "mocks.more: removed"), "{d}");
    assert!(
        repository
            .iter()
            .any(|l| l.starts_with("instance.title:") && l.contains("removed")),
        "{d}"
    );
    assert!(repository.iter().any(|l| l == "mocks.more: added"), "{d}");
}
