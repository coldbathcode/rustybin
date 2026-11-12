//! Step 19 of the core next plan: pulled commits verified against allowed
//! signers. A person at the git host (a clone in the temporary directory)
//! pushes signed, unsigned and merged commits, and every path that takes
//! the repository's side is refused or let through: the pull and its dry
//! run, the automatic mode, the webhook, `resolve` with `repository`, a
//! switch of branch, the first checkout and a checkpoint from git. The
//! keys are made by `ssh-keygen`; the tests skip with a notice without it
//! or with a git older than 2.38.

use std::path::PathBuf;

use super::signing::{key_pair, run};
use super::*;
use crate::sourcecontrol::verify;

/// git, ssh-keygen and `merge-tree --write-tree` (git 2.38).
fn can_verify() -> bool {
    if !git_installed() || !ssh_keygen_installed() {
        return false;
    }
    let version = std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let ok = crate::sourcecontrol::signing::git_version_of(&version)
        .is_some_and(|v| v >= verify::MIN_GIT);
    if !ok {
        eprintln!("git is older than 2.38: verification tests skipped");
    }
    ok
}

/// Run git as a person at the git host would, signing with `key` or not.
fn dev_git(dir: &Path, key: Option<&Path>, args: &[&str]) -> String {
    let mut cmd = std::process::Command::new("git");
    cmd.args([
        "-c",
        "user.name=Dev",
        "-c",
        "user.email=dev@example.invalid",
        "-c",
        "protocol.file.allow=always",
    ]);
    match key {
        Some(k) => {
            cmd.args(["-c", "gpg.format=ssh", "-c", "commit.gpgsign=true", "-c"]);
            cmd.arg(format!("user.signingkey={}", k.display()));
        }
        None => {
            cmd.args(["-c", "commit.gpgsign=false"]);
        }
    }
    let out = cmd
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn dev_clone(root: &Path, name: &str) -> PathBuf {
    let url = format!("file://{}", root.join("remote.git").display());
    dev_git(root, None, &["clone", "--quiet", &url, name]);
    root.join(name)
}

fn document(dev: &Path) -> PathBuf {
    dev.join("rustybin").join(DOCUMENT_NAME)
}

/// Change the console title in the clone's document.
fn retitle(dev: &Path, from: &str, to: &str) {
    let path = document(dev);
    let text = std::fs::read_to_string(&path).expect("document");
    assert!(text.contains(from), "{from:?} is not in {text}");
    std::fs::write(&path, text.replace(from, to)).expect("document");
}

/// Commit everything in the clone, signed with `key` or unsigned; the sha.
fn dev_commit(dev: &Path, key: Option<&Path>, message: &str) -> String {
    dev_git(dev, None, &["add", "-A"]);
    dev_git(dev, key, &["commit", "--quiet", "-m", message]);
    dev_git(dev, None, &["rev-parse", "HEAD"])
}

fn dev_push(dev: &Path) {
    dev_git(dev, None, &["push", "--quiet", "--force", "origin", "HEAD"]);
}

/// `name key-type base64-key` of a public key line.
fn signer(name: &str, public: &str) -> String {
    let mut parts = public.split_whitespace();
    format!(
        "{name} {} {}",
        parts.next().expect("type"),
        parts.next().expect("blob")
    )
}

fn set_signers(state: &AppState, signers: &[String]) {
    let mut values = BTreeMap::new();
    values.insert(verify::ALLOWED_SIGNERS_KEY.to_string(), json!(signers));
    state
        .settings
        .update(values, BTreeMap::new(), "test")
        .expect("git.allowed_signers");
}

fn set_verify(state: &AppState, on: bool) {
    let mut values = BTreeMap::new();
    values.insert(verify::VERIFY_KEY.to_string(), json!(on));
    state
        .settings
        .update(values, BTreeMap::new(), "test")
        .expect("git.verify");
}

fn title(state: &AppState) -> String {
    state.settings.string("console.title").unwrap_or_default()
}

async fn head(state: &AppState) -> String {
    state.git.status(state, false).await["head"]["sha"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn check(state: &AppState, id: &str) -> Option<Value> {
    crate::diagnostics::report(state)["checks"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["id"] == json!(id)).cloned())
}

/// An instance with one pushed commit (title "Title-One"), two key pairs
/// (ann, eve), ann allowed and verification on, and a clone for the person
/// at the host.
struct World {
    root: tempfile::TempDir,
    a: AppState,
    dev: PathBuf,
    ann: PathBuf,
    eve: PathBuf,
    eve_public: String,
}

async fn world() -> World {
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let a = instance(root.path(), "a", &remote);
    add_mock(&a, "orders");
    set_title(&a, "Title-One");
    a.git.init("ann").await.expect("init");
    a.git.commit(&a, "First", "ann").await.expect("commit");
    a.git.push("ann").await.expect("push");
    let (_, ann_public) = key_pair(root.path(), "ann", "");
    let (_, eve_public) = key_pair(root.path(), "eve", "");
    set_signers(&a, &[signer("ann", &ann_public)]);
    set_verify(&a, true);
    let dev = dev_clone(root.path(), "dev");
    World {
        ann: root.path().join("ann"),
        eve: root.path().join("eve"),
        eve_public,
        dev,
        a,
        root,
    }
}

/// A refusal: `409`, `reason: unverified`, the commit and a reason that
/// holds `why`.
fn refused(e: &GitError, commit: Option<&str>, why: &str) {
    assert_eq!(e.status, StatusCode::CONFLICT, "{}", e.message);
    let body = e.body();
    assert_eq!(body["reason"], json!("unverified"), "{body}");
    if let Some(c) = commit {
        assert_eq!(body["commit"], json!(c), "{body}");
    }
    assert!(
        body["why"].as_str().is_some_and(|w| w.contains(why)),
        "{why:?} is not why: {body}"
    );
    assert!(e.message.contains("not verified"), "{}", e.message);
}

#[test]
fn allowed_signers_are_names_and_public_keys() {
    if !ssh_keygen_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let (_, public) = key_pair(root.path(), "k", "");
    let blob = public.split_whitespace().nth(1).expect("blob");
    // A name and a key; a .pub line (its comment becomes the name).
    let named =
        verify::parse_signer(&format!("ann@example.com ssh-ed25519 {blob}")).expect("named");
    assert_eq!(named.name, "ann@example.com");
    assert_eq!(named.line(), format!("ann@example.com ssh-ed25519 {blob}"));
    let bare = verify::parse_signer(&format!("ssh-ed25519 {blob} Ann's laptop")).expect("bare");
    assert_eq!(bare.name, "Ann-s-laptop");
    assert_eq!(bare.key, named.key);
    assert_eq!(
        verify::parse_signer(&format!("ssh-ed25519 {blob}"))
            .expect("no comment")
            .name,
        "signer"
    );
    let (ok, listed) = run(root.path(), "ssh-keygen", &["-l", "-f", "k.pub"]);
    assert!(ok && listed.contains(&named.key.fingerprint()), "{listed}");
    // What is refused: options, a wildcard name, a key of another type,
    // something that is no key.
    for bad in [
        format!("ann cert-authority ssh-ed25519 {blob}"),
        format!("ann namespaces=\"file\" ssh-ed25519 {blob}"),
        format!("* ssh-ed25519 {blob}"),
        format!("a,b ssh-ed25519 {blob}"),
        format!("ann ssh-rsa {blob}"),
        "ann ssh-ed25519 not-base64!".to_string(),
        "ann".to_string(),
        format!("ann ssh-ed25519-cert-v01@openssh.com {blob}"),
    ] {
        assert!(verify::parse_signer(&bad).is_err(), "{bad:?} accepted");
    }
    // The setting keeps one normalised line each, from a list or from text
    // with comments, and refuses a line that is no signer.
    let text = format!("# the team\nssh-ed25519 {blob} ann\n\nbob ssh-ed25519 {blob}\n");
    assert_eq!(
        verify::normalize_signers(&json!(text)),
        json!([
            format!("ann ssh-ed25519 {blob}"),
            format!("bob ssh-ed25519 {blob}")
        ])
    );
    let state = test_state_with(Config::for_tests());
    let mut values = BTreeMap::new();
    values.insert(
        verify::ALLOWED_SIGNERS_KEY.to_string(),
        json!([format!("ann ssh-ed25519 {blob}"), "nonsense"]),
    );
    let e = state
        .settings
        .update(values, BTreeMap::new(), "test")
        .expect_err("a line that is no signer");
    assert!(format!("{e:?}").contains("not an allowed signer"), "{e:?}");
    // Off by default, and not a secret: the names and fingerprints are answered.
    let v = state.git.verification();
    assert_eq!(v["enabled"], json!(false));
    assert_eq!(v["signers"], json!([]));
    set_signers(&state, &[text]);
    let v = state.git.verification();
    assert_eq!(v["signers"][0]["name"], json!("ann"));
    assert_eq!(v["signers"][1]["name"], json!("bob"));
    assert_eq!(
        v["signers"][0]["fingerprint"],
        json!(named.key.fingerprint())
    );
}

#[tokio::test]
async fn a_signed_commit_pulls_and_an_unsigned_or_wrongly_signed_one_is_refused() {
    if !can_verify() {
        return;
    }
    let w = world().await;
    let a = &w.a;
    let first = head(a).await;
    // Turning it on trusted the head of that moment, and said so.
    let status = a.git.status(a, false).await;
    let v = &status["verification"];
    assert_eq!(v["enabled"], json!(true), "{v}");
    assert_eq!(v["trusted"]["sha"], json!(first), "{v}");
    assert_eq!(v["verified_head"]["sha"], json!(first), "{v}");
    assert_eq!(v["problem"], Value::Null, "{v}");
    let said = status["timeline"]
        .as_array()
        .expect("timeline")
        .iter()
        .find(|op| op["op"] == json!("verify"))
        .cloned()
        .expect("the timeline says so");
    assert!(
        said["detail"]
            .as_str()
            .expect("detail")
            .contains("is trusted as it is"),
        "{said}"
    );

    // Unsigned: refused, by the dry run as by the pull, and nothing changes.
    retitle(&w.dev, "Title-One", "Title-Two");
    let unsigned = dev_commit(&w.dev, None, "Unsigned");
    dev_push(&w.dev);
    for dry_run in [true, false] {
        let e = a
            .git
            .pull(a, true, dry_run, true, "ann")
            .await
            .expect_err("unsigned");
        refused(&e, Some(&unsigned), "no SSH signature");
    }
    // Without an apply too: the working copy does not move.
    let e = a
        .git
        .pull(a, false, false, false, "ann")
        .await
        .expect_err("unsigned, no apply");
    refused(&e, Some(&unsigned), "no SSH signature");
    assert_eq!(title(a), "Title-One", "nothing was applied");
    assert_eq!(head(a).await, first, "the working copy did not move");
    let v = a.git.verification();
    assert_eq!(v["last_refusal"]["commit"], json!(unsigned), "{v}");
    let c = check(a, "source_control_verification").expect("check");
    assert_eq!(c["status"], json!("warn"), "{c}");

    // The same change signed by a key that is not allowed.
    dev_git(
        &w.dev,
        Some(&w.eve),
        &["commit", "--quiet", "--amend", "-m", "Signed by eve"],
    );
    let by_eve = dev_git(&w.dev, None, &["rev-parse", "HEAD"]);
    dev_push(&w.dev);
    let e = a
        .git
        .pull(a, true, false, true, "ann")
        .await
        .expect_err("another key");
    refused(&e, Some(&by_eve), "not an allowed signer");
    assert_eq!(title(a), "Title-One");
    assert_eq!(head(a).await, first);

    // Signed by an allowed signer: pulled and applied.
    dev_git(
        &w.dev,
        Some(&w.ann),
        &["commit", "--quiet", "--amend", "-m", "Signed by ann"],
    );
    let by_ann = dev_git(&w.dev, None, &["rev-parse", "HEAD"]);
    dev_push(&w.dev);
    let pull = a
        .git
        .pull(a, true, true, true, "ann")
        .await
        .expect("dry run");
    assert_eq!(pull["dry_run"], json!(true));
    assert_eq!(
        a.git.verification()["verified_head"]["sha"],
        json!(first),
        "a dry run keeps nothing"
    );
    let pull = a.git.pull(a, true, false, true, "ann").await.expect("pull");
    assert_eq!(pull["fast_forwarded"], json!(true), "{pull}");
    assert_eq!(title(a), "Title-Two");
    assert_eq!(head(a).await, by_ann);
    let v = a.git.verification();
    assert_eq!(v["verified_head"]["sha"], json!(by_ann), "{v}");
    assert_eq!(v["trusted"]["sha"], json!(first), "{v}");
    assert_eq!(v["last_refusal"], Value::Null, "{v}");
    assert_eq!(
        check(a, "source_control_verification").expect("check")["status"],
        json!("ok")
    );

    // A tampered commit: ann's signature over another message.
    retitle(&w.dev, "Title-Two", "Title-Three");
    let good = dev_commit(&w.dev, Some(&w.ann), "Honest");
    let object = dev_git(&w.dev, None, &["cat-file", "commit", &good]);
    let forged = w.root.path().join("forged");
    std::fs::write(&forged, format!("{}\n", object.replace("Honest", "Forged"))).expect("object");
    let forged_sha = dev_git(
        &w.dev,
        None,
        &[
            "hash-object",
            "-t",
            "commit",
            "-w",
            &forged.display().to_string(),
        ],
    );
    dev_git(&w.dev, None, &["reset", "--quiet", "--hard", &forged_sha]);
    dev_push(&w.dev);
    let e = a
        .git
        .pull(a, true, false, true, "ann")
        .await
        .expect_err("forged");
    refused(&e, Some(&forged_sha), "does not match its content");
    assert_eq!(title(a), "Title-Two");

    // Off: the same pull goes through, as before this step.
    set_verify(a, false);
    a.git
        .pull(a, true, false, true, "ann")
        .await
        .expect("verification off");
    assert_eq!(title(a), "Title-Three");
    assert_eq!(a.git.verification()["verified_head"], Value::Null);
    assert!(check(a, "source_control_verification").is_none());
    let _ = w.eve_public;
}

#[tokio::test]
async fn every_commit_is_verified_not_only_the_tip() {
    if !can_verify() {
        return;
    }
    let w = world().await;
    let a = &w.a;
    let first = head(a).await;
    retitle(&w.dev, "Title-One", "Title-Two");
    let unsigned = dev_commit(&w.dev, None, "Unsigned, underneath");
    retitle(&w.dev, "Title-Two", "Title-Three");
    dev_commit(&w.dev, Some(&w.ann), "Signed, on top");
    dev_push(&w.dev);
    let e = a
        .git
        .pull(a, true, false, true, "ann")
        .await
        .expect_err("a signed tip on an unsigned commit");
    refused(&e, Some(&unsigned), "no SSH signature");
    assert_eq!(title(a), "Title-One");
    assert_eq!(head(a).await, first);
    // A verified head the working copy does not hold changes nothing (it
    // is skipped, not an error), and a file that cannot be read trusts
    // nothing.
    let file = w.root.path().join("a").join(verify::VERIFIED_FILE);
    let mut trust: Value =
        serde_json::from_slice(&std::fs::read(&file).expect("file")).expect("json");
    trust["heads"]["gone"] =
        json!({ "sha": "0123456789abcdef0123456789abcdef01234567", "at": "2026-01-01T00:00:00Z" });
    std::fs::write(&file, trust.to_string()).expect("file");
    let e = a
        .git
        .pull(a, true, false, true, "ann")
        .await
        .expect_err("still refused");
    refused(&e, Some(&unsigned), "no SSH signature");
    std::fs::write(&file, "not json").expect("file");
    let e = a
        .git
        .pull(a, true, false, true, "ann")
        .await
        .expect_err("nothing trusted");
    refused(&e, None, "no SSH signature");
    assert_eq!(title(a), "Title-One");
}

#[tokio::test]
async fn a_clean_merge_passes_and_a_merge_with_a_change_of_its_own_is_refused() {
    if !can_verify() {
        return;
    }
    let w = world().await;
    let a = &w.a;
    // Two signed commits on two branches, merged at the host without a
    // signature (a merge button): passes.
    dev_git(&w.dev, None, &["checkout", "--quiet", "-b", "feature"]);
    retitle(&w.dev, "Title-One", "Title-Feature");
    dev_commit(&w.dev, Some(&w.ann), "The feature");
    dev_git(&w.dev, None, &["checkout", "--quiet", "main"]);
    std::fs::write(w.dev.join("NOTES.md"), "notes\n").expect("notes");
    dev_commit(&w.dev, Some(&w.ann), "Notes on main");
    dev_git(
        &w.dev,
        None,
        &[
            "merge",
            "--quiet",
            "--no-ff",
            "-m",
            "Merge feature",
            "feature",
        ],
    );
    let merge = dev_git(&w.dev, None, &["rev-parse", "HEAD"]);
    dev_push(&w.dev);
    let pull = a
        .git
        .pull(a, true, false, true, "ann")
        .await
        .expect("a clean merge of verified parents");
    assert_eq!(pull["fast_forwarded"], json!(true), "{pull}");
    assert_eq!(title(a), "Title-Feature");
    assert_eq!(a.git.verification()["verified_head"]["sha"], json!(merge));

    // A merge that carries a change of its own: refused.
    dev_git(&w.dev, None, &["checkout", "--quiet", "-b", "more"]);
    std::fs::write(w.dev.join("MORE.md"), "more\n").expect("more");
    dev_commit(&w.dev, Some(&w.ann), "More");
    dev_git(&w.dev, None, &["checkout", "--quiet", "main"]);
    std::fs::write(w.dev.join("NOTES.md"), "notes, again\n").expect("notes");
    dev_commit(&w.dev, Some(&w.ann), "Notes again");
    dev_git(
        &w.dev,
        None,
        &["merge", "--quiet", "--no-ff", "--no-commit", "more"],
    );
    retitle(&w.dev, "Title-Feature", "Title-Evil");
    let evil = dev_commit(&w.dev, None, "Merge more");
    dev_push(&w.dev);
    let e = a
        .git
        .pull(a, true, false, true, "ann")
        .await
        .expect_err("a merge with a change of its own");
    refused(&e, Some(&evil), "carries a change of its own");
    assert_eq!(title(a), "Title-Feature");
    assert_eq!(head(a).await, merge);

    // A merge whose parent is unsigned: the parent is what is refused.
    dev_git(&w.dev, None, &["reset", "--quiet", "--hard", "HEAD^"]);
    dev_git(&w.dev, None, &["checkout", "--quiet", "-b", "unsigned"]);
    std::fs::write(w.dev.join("UNSIGNED.md"), "x\n").expect("file");
    let parent = dev_commit(&w.dev, None, "Unsigned work");
    dev_git(&w.dev, None, &["checkout", "--quiet", "main"]);
    dev_git(
        &w.dev,
        None,
        &[
            "merge",
            "--quiet",
            "--no-ff",
            "-m",
            "Merge unsigned",
            "unsigned",
        ],
    );
    dev_push(&w.dev);
    let e = a
        .git
        .pull(a, true, false, true, "ann")
        .await
        .expect_err("a merge of an unsigned parent");
    refused(&e, Some(&parent), "no SSH signature");

    // A squash made at the host (one new commit, signed by nobody here): refused.
    dev_git(&w.dev, None, &["reset", "--quiet", "--hard", "HEAD^"]);
    dev_git(&w.dev, None, &["merge", "--quiet", "--squash", "more"]);
    let squash = dev_commit(&w.dev, None, "More (squashed)");
    dev_push(&w.dev);
    let e = a
        .git
        .pull(a, true, false, true, "ann")
        .await
        .expect_err("a squash merge");
    refused(&e, Some(&squash), "no SSH signature");

    // The merge with its own change, signed by an allowed signer, passes:
    // a signer vouches for it.
    dev_git(&w.dev, None, &["reset", "--quiet", "--hard", "HEAD^"]);
    dev_git(
        &w.dev,
        None,
        &["merge", "--quiet", "--no-ff", "--no-commit", "more"],
    );
    retitle(&w.dev, "Title-Feature", "Title-Vouched");
    dev_commit(&w.dev, Some(&w.ann), "Merge more, signed");
    dev_push(&w.dev);
    a.git
        .pull(a, true, false, true, "ann")
        .await
        .expect("a signed merge");
    assert_eq!(title(a), "Title-Vouched");
}

#[tokio::test]
async fn a_replaced_key_keeps_old_history_and_new_commits_need_the_new_key() {
    if !can_verify() {
        return;
    }
    let w = world().await;
    let a = &w.a;
    retitle(&w.dev, "Title-One", "Title-Two");
    let by_ann = dev_commit(&w.dev, Some(&w.ann), "Signed by ann");
    dev_push(&w.dev);
    a.git.pull(a, true, false, true, "t").await.expect("pull");
    // Ann's key is replaced by eve's.
    set_signers(a, &[signer("eve", &w.eve_public)]);
    retitle(&w.dev, "Title-Two", "Title-Three");
    let stale = dev_commit(&w.dev, Some(&w.ann), "Signed by ann again");
    dev_push(&w.dev);
    let e = a
        .git
        .pull(a, true, false, true, "t")
        .await
        .expect_err("the old key");
    refused(&e, Some(&stale), "not an allowed signer");
    assert_eq!(head(a).await, by_ann);
    // The same change by the new key passes, on top of history only the
    // old key signed: the verified head keeps it trusted.
    dev_git(
        &w.dev,
        Some(&w.eve),
        &["commit", "--quiet", "--amend", "-m", "Signed by eve"],
    );
    dev_push(&w.dev);
    a.git
        .pull(a, true, false, true, "t")
        .await
        .expect("the new key");
    assert_eq!(title(a), "Title-Three");

    // The instance's own signing key is allowed without being listed,
    // while it is the current key.
    set_signers(a, &[]);
    let mut values = BTreeMap::new();
    values.insert(SIGN_KEY.to_string(), json!(true));
    a.settings
        .update(values, BTreeMap::new(), "test")
        .expect("git.sign");
    a.git
        .generate_signing_key(false, "t")
        .await
        .expect("signing key");
    set_title(a, "Title-Four");
    a.git.commit(a, "Own, signed", "t").await.expect("commit");
    a.git.push("t").await.expect("push");
    let own = head(a).await;
    let v = a.git.verification();
    assert_eq!(v["verified_head"]["sha"], json!(own), "an own commit: {v}");
    assert_eq!(v["own_key"]["name"], json!(verify::OWN_NAME), "{v}");
    assert_eq!(v["problem"], Value::Null, "{v}");
    let key_file = w.root.path().join("a").join(SIGNING_KEY_FILE);
    let old_key = w.root.path().join("old-own-key");
    std::fs::copy(&key_file, &old_key).expect("copy");
    dev_git(&w.dev, None, &["pull", "--quiet", "origin", "main"]);
    retitle(&w.dev, "Title-Four", "Title-Five");
    dev_commit(&w.dev, Some(&old_key), "Signed with the instance's key");
    dev_push(&w.dev);
    a.git
        .pull(a, true, false, true, "t")
        .await
        .expect("the instance's own key");
    assert_eq!(title(a), "Title-Five");
    // Replaced: the old own key signs nothing new any more.
    a.git
        .generate_signing_key(true, "t")
        .await
        .expect("replace");
    retitle(&w.dev, "Title-Five", "Title-Six");
    let late = dev_commit(&w.dev, Some(&old_key), "Signed with the replaced key");
    dev_push(&w.dev);
    let e = a
        .git
        .pull(a, true, false, true, "t")
        .await
        .expect_err("the replaced own key");
    refused(&e, Some(&late), "not an allowed signer");
    assert_eq!(title(a), "Title-Five");
}

#[tokio::test]
async fn the_automatic_mode_and_the_webhook_meet_a_refusal_and_apply_nothing() {
    if !can_verify() {
        return;
    }
    let w = world().await;
    let a = &w.a;
    let first = head(a).await;
    retitle(&w.dev, "Title-One", "Title-Two");
    let unsigned = dev_commit(&w.dev, None, "Unsigned");
    dev_push(&w.dev);
    // The automatic mode.
    let mut values = BTreeMap::new();
    values.insert(AUTO_KEY.to_string(), json!("pull"));
    a.settings
        .update(values, BTreeMap::new(), "test")
        .expect("auto");
    a.git.auto_tick(a).await;
    assert_eq!(title(a), "Title-One");
    assert_eq!(head(a).await, first);
    let last = a.git.last_op().expect("an operation");
    assert_eq!(
        (last.op.as_str(), last.by.as_str(), last.ok),
        ("pull", "auto", false)
    );
    assert!(last.detail.contains("not verified"), "{}", last.detail);
    assert_eq!(
        a.git.verification()["last_refusal"]["commit"],
        json!(unsigned)
    );
    let mut values = BTreeMap::new();
    values.insert(AUTO_KEY.to_string(), json!("off"));
    a.settings
        .update(values, BTreeMap::new(), "test")
        .expect("auto off");
    // The webhook.
    set_secret(a, "hook-secret-of-some-length");
    let app = test_app_with(a.clone());
    let body = r#"{"ref":"refs/heads/main"}"#;
    let good = sign("hook-secret-of-some-length", body.as_bytes());
    let (status, v) = hook(&app, &[("x-hub-signature-256", good)], body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let done = a
            .git
            .last_op()
            .is_some_and(|op| op.op == "pull" && op.by == "webhook");
        if done || std::time::Instant::now() > until {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let last = a.git.last_op().expect("an operation");
    assert_eq!((last.by.as_str(), last.ok), ("webhook", false), "{last:?}");
    assert!(last.detail.contains("not verified"), "{}", last.detail);
    assert_eq!(title(a), "Title-One");
    assert_eq!(head(a).await, first);

    // The route says it the same way: 409, reason unverified, the commit.
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/git/pull",
            &json!({ "apply": true }),
        ))
        .await
        .expect("pull");
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let v = body_json(resp).await;
    assert_eq!(v["reason"], json!("unverified"), "{v}");
    assert_eq!(v["commit"], json!(unsigned), "{v}");
    assert!(v["why"].is_string() && v["error"].is_string(), "{v}");
    let resp = app
        .clone()
        .oneshot(get_request("/_rustybin/git"))
        .await
        .expect("status");
    let v = body_json(resp).await;
    assert_eq!(v["verification"]["enabled"], json!(true), "{v}");
    assert_eq!(v["verification"]["signers"][0]["name"], json!("ann"));
    assert_eq!(v["verification"]["last_refusal"]["commit"], json!(unsigned));
}

#[tokio::test]
async fn resolve_a_switch_and_a_checkpoint_from_git_are_verified_too() {
    if !can_verify() {
        return;
    }
    let w = world().await;
    let a = &w.a;
    // A branch of the remote with an unsigned commit: no switch, no preview.
    dev_git(&w.dev, None, &["checkout", "--quiet", "-b", "staging"]);
    retitle(&w.dev, "Title-One", "Title-Staging");
    let staging = dev_commit(&w.dev, None, "Staging, unsigned");
    dev_push(&w.dev);
    for dry_run in [true, false] {
        for apply in [true, false] {
            let e = a
                .git
                .switch_branch(
                    a,
                    &BranchRequest {
                        branch: "staging".into(),
                        apply,
                        dry_run,
                        ..Default::default()
                    },
                    "ann",
                )
                .await
                .expect_err("an unverified branch");
            refused(&e, Some(&staging), "no SSH signature");
        }
    }
    assert_eq!(a.git.branch(), "main");
    assert_eq!(title(a), "Title-One");
    // Signed: the switch goes through.
    dev_git(
        &w.dev,
        Some(&w.ann),
        &["commit", "--quiet", "--amend", "-m", "Staging, signed"],
    );
    dev_push(&w.dev);
    a.git
        .switch_branch(
            a,
            &BranchRequest {
                branch: "staging".into(),
                apply: true,
                ..Default::default()
            },
            "ann",
        )
        .await
        .expect("a verified branch");
    assert_eq!(title(a), "Title-Staging");
    assert_eq!(a.git.branch(), "staging");

    // A diverged branch: the repository's side is unsigned, so resolving
    // with it (and its preview) is refused, and the local commit stays.
    set_title(a, "Title-Mine");
    a.git.commit(a, "Mine", "ann").await.expect("commit");
    let mine = head(a).await;
    retitle(&w.dev, "Title-Staging", "Title-Theirs");
    let theirs = dev_commit(&w.dev, None, "Theirs, unsigned");
    dev_push(&w.dev);
    for dry_run in [true, false] {
        let e = a
            .git
            .resolve(
                a,
                &Resolve {
                    with: "repository".into(),
                    dry_run,
                    ..Default::default()
                },
                "ann",
            )
            .await
            .expect_err("the repository's unverified side");
        refused(&e, Some(&theirs), "no SSH signature");
    }
    assert_eq!(head(a).await, mine);
    assert_eq!(title(a), "Title-Mine");
    // A checkpoint is read from a verified head: the instance's own commit
    // on a verified head is one.
    crate::checkpoints::save(a, "demo", false).expect("save");
    let listing = a
        .git
        .repository_checkpoints()
        .await
        .expect("a verified head");
    assert!(listing.entries.is_empty());

    // The instance's side wins: nothing of the repository is applied, so
    // it is not refused. Its commit now stands on the unsigned one, and so
    // is not a verified head: a checkpoint is not read from it, by the
    // list or by the restore, and a pull applies nothing.
    a.git
        .resolve(
            a,
            &Resolve {
                with: "instance".into(),
                message: "Mine wins".into(),
                ..Default::default()
            },
            "ann",
        )
        .await
        .expect("the instance's side");
    let e = a
        .git
        .repository_checkpoints()
        .await
        .err()
        .expect("an unverified head");
    refused(&e, Some(&theirs), "no SSH signature");
    let e = a
        .git
        .repository_checkpoint("demo")
        .await
        .expect_err("an unverified head");
    refused(&e, Some(&theirs), "no SSH signature");
    let app = test_app_with(a.clone());
    for (method, path) in [
        ("GET", "/_rustybin/checkpoints?from=git"),
        ("POST", "/_rustybin/checkpoints/demo/restore?from=git"),
    ] {
        let resp = app
            .clone()
            .oneshot(json_request(method, path, &json!({})))
            .await
            .expect(path);
        assert_eq!(resp.status(), StatusCode::CONFLICT, "{path}");
        let v = body_json(resp).await;
        assert_eq!(v["reason"], json!("unverified"), "{path}: {v}");
        assert_eq!(v["commit"], json!(theirs), "{path}: {v}");
    }
    let e = a
        .git
        .pull(a, true, false, true, "ann")
        .await
        .expect_err("an unverified head");
    refused(&e, Some(&theirs), "no SSH signature");

    // A person accepts the head as it is: off and on again trusts the head
    // of that moment, with no operation in between.
    set_verify(a, false);
    set_verify(a, true);
    a.git
        .pull(a, true, false, true, "ann")
        .await
        .expect("the head was trusted again");
    let v = a.git.verification();
    assert_eq!(v["trusted"]["sha"], json!(head(a).await), "{v}");
    a.git
        .repository_checkpoints()
        .await
        .expect("a trusted head");
}

#[tokio::test]
async fn a_first_checkout_with_verification_on_trusts_nothing() {
    if !can_verify() {
        return;
    }
    let w = world().await;
    // A second instance with verification on before it is connected: there
    // is no head to trust, so the branch's unsigned first commit refuses
    // the checkout, and startup applies nothing.
    let remote = format!("file://{}", w.root.path().join("remote.git").display());
    let b = instance(w.root.path(), "b", &remote);
    set_signers(&b, std::slice::from_ref(&signer("eve", &w.eve_public)));
    set_verify(&b, true);
    let e = b.git.init("bob").await.expect_err("an unverified branch");
    refused(&e, None, "no SSH signature");
    assert_eq!(head(&b).await, "");
    let v = b.git.verification();
    assert_eq!(v["trusted"], Value::Null, "{v}");
    assert_eq!(v["verified_head"], Value::Null, "{v}");
    let e = b
        .git
        .pull(&b, true, false, true, "bob")
        .await
        .expect_err("an unverified branch");
    refused(&e, None, "no SSH signature");
    assert!(b.mocks.get("orders").is_none(), "nothing was applied");
    // No signers at all is a problem the status and the diagnostics name.
    set_signers(&b, &[]);
    let v = b.git.verification();
    assert!(
        v["problem"]
            .as_str()
            .is_some_and(|p| p.contains("nobody is an allowed signer")),
        "{v}"
    );
    let c = check(&b, "source_control_verification").expect("check");
    assert_eq!(c["status"], json!("warn"), "{c}");
    assert!(
        c["fix"].as_str().is_some_and(|f| f.contains("signers")),
        "{c}"
    );
}

#[tokio::test]
async fn a_document_from_the_repository_never_turns_verification_off() {
    if !can_verify() {
        return;
    }
    let w = world().await;
    let a = &w.a;
    let path = document(&w.dev);
    let text = std::fs::read_to_string(&path).expect("document");
    let eve = signer("eve", &w.eve_public);
    let added = format!("settings:\n  git.verify: false\n  git.allowed_signers:\n  - {eve}\n");
    let section = |l: &str| l == "settings:" || l == "settings: {}";
    let with = if text.lines().any(section) {
        text.lines()
            .map(|l| {
                if section(l) {
                    added.clone()
                } else {
                    format!("{l}\n")
                }
            })
            .collect::<String>()
    } else {
        format!("{text}{added}")
    };
    std::fs::write(&path, with).expect("document");
    dev_commit(&w.dev, Some(&w.ann), "Turn verification off");
    dev_push(&w.dev);
    let pull = a.git.pull(a, true, false, true, "ann").await.expect("pull");
    let problems = pull["applied"]["problems"].to_string();
    assert!(problems.contains("git.verify"), "{pull}");
    assert!(problems.contains("git.allowed_signers"), "{pull}");
    let v = a.git.verification();
    assert_eq!(v["enabled"], json!(true), "{v}");
    assert_eq!(v["signers"].as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(v["signers"][0]["name"], json!("ann"), "{v}");
    // Its own export never names them.
    let yaml = crate::configdoc::export(a, crate::configdoc::SecretsMode::Refs, false).yaml();
    assert!(!yaml.contains("git."), "{yaml}");
}
