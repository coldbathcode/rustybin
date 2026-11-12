//! Step 11 of the core next plan: signed commits. A signing key of its own
//! (a secret, never the deploy key, never answered), its public half and
//! fingerprint, signatures git itself verifies, and a commit refused rather
//! than made unsigned. The keys are made by `ssh-keygen` in a temporary
//! directory; the tests skip with a notice without it.

use super::*;
use crate::sourcecontrol::signing;

pub(super) fn run(dir: &Path, program: &str, args: &[&str]) -> (bool, String) {
    let out = std::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .expect(program);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// A key pair from `ssh-keygen`: the private file's text and the public line.
pub(super) fn key_pair(dir: &Path, name: &str, passphrase: &str) -> (String, String) {
    let (ok, out) = run(
        dir,
        "ssh-keygen",
        &[
            "-q", "-t", "ed25519", "-N", passphrase, "-C", "someone", "-f", name,
        ],
    );
    assert!(ok, "{out}");
    (
        std::fs::read_to_string(dir.join(name)).expect("private"),
        std::fs::read_to_string(dir.join(format!("{name}.pub"))).expect("public"),
    )
}

fn set_secret(state: &AppState, key: &str, value: &str) -> Result<(), String> {
    let mut secrets = BTreeMap::new();
    secrets.insert(key.to_string(), Some(value.to_string()));
    state
        .settings
        .update(BTreeMap::new(), secrets, "test")
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

fn sign(state: &AppState, on: bool) {
    let mut values = BTreeMap::new();
    values.insert(SIGN_KEY.to_string(), json!(on));
    state
        .settings
        .update(values, BTreeMap::new(), "test")
        .expect("git.sign");
}

fn check(state: &AppState, id: &str) -> Option<Value> {
    crate::diagnostics::report(state)["checks"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["id"] == json!(id)).cloned())
}

#[test]
fn the_git_version_is_read_for_signing() {
    assert_eq!(
        signing::git_version_of("git version 2.39.5 (Apple Git-154)"),
        Some((2, 39))
    );
    assert_eq!(signing::git_version_of("git version 2.34.1"), Some((2, 34)));
    assert_eq!(
        signing::git_version_of("git version 2.33.0.windows.2"),
        Some((2, 33))
    );
    assert_eq!(signing::git_version_of("nothing"), None);
    assert!(signing::git_version_of("git version 2.33.8") < Some(signing::MIN_GIT));
}

#[tokio::test]
async fn the_signing_key_is_a_secret_with_a_public_half() {
    if !git_installed() || !ssh_keygen_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let state = instance(root.path(), "k", &remote);
    assert_eq!(state.git.signing()["key_set"], json!(false));
    assert_eq!(
        state.git.signing()["enabled"],
        json!(false),
        "off by default"
    );

    // A pasted key, with its lines, is kept as one line and read back as
    // the same public key and fingerprint ssh-keygen shows.
    let (private, public) = key_pair(root.path(), "pasted", "");
    set_secret(&state, SIGNING_KEY_KEY, &private).expect("a pasted key is taken");
    let s = state.git.signing();
    assert_eq!(s["key_set"], json!(true));
    assert_eq!(s["key_type"], json!("ssh-ed25519"));
    let blob = public.split_whitespace().nth(1).expect("blob");
    assert_eq!(
        s["public"],
        json!(format!("ssh-ed25519 {blob} rustybin-signing-test-k"))
    );
    let (ok, listed) = run(root.path(), "ssh-keygen", &["-l", "-f", "pasted.pub"]);
    assert!(ok, "{listed}");
    let fingerprint = s["fingerprint"].as_str().expect("fingerprint");
    assert!(
        fingerprint.starts_with("SHA256:") && listed.contains(fingerprint),
        "{fingerprint} is not in {listed}"
    );
    // The same key pasted through a one-line field (its line breaks gone).
    set_secret(&state, SIGNING_KEY_KEY, &private.replace('\n', " ")).expect("one line");
    assert_eq!(state.git.signing()["fingerprint"], json!(fingerprint));

    // Never answered: not by the signing answer, the status, the settings.
    let stored = state.settings.secret(SIGNING_KEY_KEY).expect("stored");
    assert!(!stored.contains('\n') && !stored.contains("BEGIN"));
    for answer in [
        state.git.signing().to_string(),
        state.git.status(&state, false).await.to_string(),
        state.settings.view().to_string(),
    ] {
        assert!(!answer.contains(stored.as_str()), "the key is in an answer");
        // Not a long piece of it either (the hint is its last characters).
        assert!(!answer.contains(&stored[..40]) && !answer.contains(&stored[stored.len() - 40..]));
    }

    // What is refused: a key with a passphrase, something that is no key.
    let (locked, _) = key_pair(root.path(), "locked", "a passphrase");
    let e = set_secret(&state, SIGNING_KEY_KEY, &locked).expect_err("passphrase");
    assert!(e.contains("passphrase"), "{e}");
    let e = set_secret(&state, SIGNING_KEY_KEY, "not a key at all").expect_err("no key");
    assert!(e.contains("not a signing key"), "{e}");
    assert_eq!(
        state.git.signing()["fingerprint"],
        json!(fingerprint),
        "a refused key changes nothing"
    );

    // Generated here: refused while one is set, replaced on request.
    let e = state
        .git
        .generate_signing_key(false, "t")
        .await
        .expect_err("one is set");
    assert_eq!(e.status, StatusCode::CONFLICT);
    let made = state
        .git
        .generate_signing_key(true, "t")
        .await
        .expect("replace");
    assert_eq!(made["generated"], json!(true));
    assert_ne!(made["fingerprint"], json!(fingerprint));
    assert!(made["public"]
        .as_str()
        .expect("public")
        .starts_with("ssh-ed25519 "));
    let stored = state.settings.secret(SIGNING_KEY_KEY).expect("stored");
    assert!(!made.to_string().contains(stored.as_str()));
    // Nothing of the generation is left in the state directory.
    let left: Vec<String> = std::fs::read_dir(root.path().join("k"))
        .expect("state dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".signing-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test]
async fn commits_are_signed_with_the_signing_key_and_never_the_deploy_key() {
    if !git_installed() || !ssh_keygen_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let state = instance(root.path(), "s", &remote);
    add_mock(&state, "orders");
    state.git.init("ann").await.expect("init");
    let repo = state.git.repo_dir().expect("repo");

    // Off by default: a key alone signs nothing.
    let key = state
        .git
        .generate_signing_key(false, "ann")
        .await
        .expect("signing key");
    let commit = state
        .git
        .commit(&state, "Unsigned", "ann")
        .await
        .expect("commit");
    assert_eq!(commit["signed"], json!(false));
    let (_, object) = run(&repo, "git", &["cat-file", "commit", "HEAD"]);
    assert!(!object.contains("gpgsig"), "{object}");

    // On: every commit carries an SSH signature that git verifies against
    // the public key the instance answers.
    sign(&state, true);
    assert_eq!(state.git.signing()["problem"], Value::Null);
    set_title(&state, "Signed");
    let commit = state
        .git
        .commit(&state, "Signed", "ann")
        .await
        .expect("signed commit");
    assert_eq!(commit["signed"], json!(true), "{commit}");
    assert!(commit["detail"]
        .as_str()
        .expect("detail")
        .contains("signed"));
    let (_, object) = run(&repo, "git", &["cat-file", "commit", "HEAD"]);
    assert!(object.contains("-----BEGIN SSH SIGNATURE-----"), "{object}");
    let allowed = root.path().join("allowed_signers");
    let public = key["public"].as_str().expect("public");
    let mut parts = public.split_whitespace();
    std::fs::write(
        &allowed,
        format!(
            "rustybin@example.invalid {} {}\n",
            parts.next().expect("type"),
            parts.next().expect("blob")
        ),
    )
    .expect("allowed signers");
    let (ok, out) = run(
        &repo,
        "git",
        &[
            "-c",
            "gpg.format=ssh",
            "-c",
            &format!("gpg.ssh.allowedSignersFile={}", allowed.display()),
            "verify-commit",
            "HEAD",
        ],
    );
    assert!(ok, "git does not verify the signature: {out}");

    // The key file: in the state directory, private, and what the secret holds.
    let file = root.path().join("s").join(SIGNING_KEY_FILE);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file)
            .expect("key file")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Never the deploy key: a deploy key next to it is another key, and the
    // signature still verifies only against the signing key.
    let deploy = state
        .git
        .generate_key(false, "ann")
        .await
        .expect("deploy key");
    let deploy_blob = deploy["public"]
        .as_str()
        .expect("public")
        .split_whitespace()
        .nth(1)
        .expect("blob")
        .to_string();
    assert!(!public.contains(&deploy_blob));
    std::fs::write(
        &allowed,
        format!("rustybin@example.invalid ssh-ed25519 {deploy_blob}\n"),
    )
    .expect("allowed signers");
    set_title(&state, "Signed again");
    state
        .git
        .commit(&state, "Signed again", "ann")
        .await
        .expect("commit");
    let (ok, _) = run(
        &repo,
        "git",
        &[
            "-c",
            "gpg.format=ssh",
            "-c",
            &format!("gpg.ssh.allowedSignersFile={}", allowed.display()),
            "verify-commit",
            "HEAD",
        ],
    );
    assert!(!ok, "the deploy key must not be what signed");

    // The automatic mode signs too, and a replaced key takes over at once.
    let replaced = state
        .git
        .generate_signing_key(true, "ann")
        .await
        .expect("replace");
    let mut values = BTreeMap::new();
    values.insert(AUTO_KEY.to_string(), json!("push_on_change"));
    state
        .settings
        .update(values, BTreeMap::new(), "test")
        .expect("auto");
    add_mock(&state, "more");
    state.git.auto_tick(&state).await;
    let mut parts = replaced["public"]
        .as_str()
        .expect("public")
        .split_whitespace();
    std::fs::write(
        &allowed,
        format!(
            "rustybin@example.invalid {} {}\n",
            parts.next().expect("type"),
            parts.next().expect("blob")
        ),
    )
    .expect("allowed signers");
    let (ok, out) = run(
        &repo,
        "git",
        &[
            "-c",
            "gpg.format=ssh",
            "-c",
            &format!("gpg.ssh.allowedSignersFile={}", allowed.display()),
            "verify-commit",
            "HEAD",
        ],
    );
    assert!(
        ok,
        "the automatic mode's commit is not signed with the new key: {out}"
    );
    assert_eq!(
        check(&state, "source_control_signing").expect("check")["status"],
        json!("ok")
    );
}

#[tokio::test]
async fn signing_without_a_key_refuses_the_commit() {
    if !git_installed() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let remote = bare_repo(root.path());
    let state = instance(root.path(), "n", &remote);
    add_mock(&state, "orders");
    state.git.init("ann").await.expect("init");
    assert!(
        check(&state, "source_control_signing").is_none(),
        "off: no check"
    );
    sign(&state, true);
    let s = state.git.signing();
    assert_eq!(s["enabled"], json!(true));
    assert!(
        s["problem"]
            .as_str()
            .expect("problem")
            .contains("no signing key"),
        "{s}"
    );
    // Refused, never made unsigned.
    let e = state
        .git
        .commit(&state, "Unsigned?", "ann")
        .await
        .expect_err("no key");
    assert_eq!(e.status, StatusCode::CONFLICT);
    assert!(e.message.contains("no signing key"), "{}", e.message);
    let status = state.git.status(&state, false).await;
    assert_eq!(status["head"], Value::Null, "no commit was made");
    assert!(status["signing"]["problem"].is_string());
    // The automatic mode is refused the same way.
    let mut values = BTreeMap::new();
    values.insert(AUTO_KEY.to_string(), json!("push_on_change"));
    state
        .settings
        .update(values, BTreeMap::new(), "test")
        .expect("auto");
    state.git.auto_tick(&state).await;
    assert_eq!(state.git.status(&state, false).await["head"], Value::Null);
    // The diagnostics say what to do.
    let c = check(&state, "source_control_signing").expect("check");
    assert_eq!(c["status"], json!("warn"), "{c}");
    assert!(c["fix"].as_str().expect("fix").contains("signing"), "{c}");
    // Off again: commits are made as before.
    sign(&state, false);
    let commit = state
        .git
        .commit(&state, "Plain", "ann")
        .await
        .expect("commit");
    assert_eq!(commit["committed"], json!(true));
    assert_eq!(commit["signed"], json!(false));
}
