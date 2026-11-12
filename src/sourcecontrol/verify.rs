//! Pulled commits verified against allowed signers (step 19 of
//! `docs/plans/core-next/master-plan.md`).
//!
//! With `git.verify` on, the instance takes the repository's side only when
//! every commit between the last verified head and the new one passes:
//!
//! - a commit with at most one parent passes when an allowed signer signed
//!   it (SSH signatures only: `git log` with `%G?` and `%GF` against
//!   `<state>/repo.allowed_signers`, and no OpenPGP or X.509 program);
//! - a merge commit of two parents that no allowed signer signed passes
//!   only when its tree is exactly what merging the parents gives
//!   (`git merge-tree --write-tree`, git 2.38 or later), so a host's merge
//!   button works and nothing rides in on it. Both parents are in the range
//!   or below a verified head, so they pass on their own;
//! - anything else is refused: a squash or a rebase merge made by the host
//!   is a new commit signed by the host or by nobody.
//!
//! The allowed signers are the setting `git.allowed_signers` (public keys
//! with their names, not a secret) plus the instance's own signing key while
//! it is the current key. The last verified head of each branch is kept in
//! `<state>/repo.verified.json`, so a replaced key never reopens history.
//! Turning `git.verify` on trusts the head of that moment and says so (the
//! timeline, the status); with no commit yet nothing is trusted and the
//! whole branch has to verify.
//!
//! A refusal is `409` with `reason: unverified`, the commit and why;
//! nothing is applied and the working copy does not move. Every path that
//! takes the repository's side calls [`SourceControl::verify_rev`] first:
//! the pull (and with it the webhook, the automatic mode and startup),
//! `resolve` with `repository` and its preview, a switch of branch and its
//! preview, the first checkout of `init`, and a checkpoint read from git.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Instant;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::signing::{self, PublicKey};
use super::{GitEnv, GitError, GitResult, SourceControl, GIT_TIMEOUT};

/// Whether pulled commits are verified.
pub const VERIFY_KEY: &str = "git.verify";
/// The allowed signers: `name key-type base64-key`, one each.
pub const ALLOWED_SIGNERS_KEY: &str = "git.allowed_signers";
/// The verified heads, in the state directory.
pub const VERIFIED_FILE: &str = "repo.verified.json";
/// The allowed signers file git verifies against, in the state directory.
pub const ALLOWED_SIGNERS_FILE: &str = "repo.allowed_signers";
/// The first git with `merge-tree --write-tree`.
pub const MIN_GIT: (u32, u32) = (2, 38);
/// Most allowed signers.
pub const MAX_SIGNERS: usize = 50;
/// Longest allowed signer line.
pub const MAX_SIGNER_LEN: usize = 2048;
/// Most commits verified in one go (since the last verified head).
pub const MAX_VERIFY_COMMITS: usize = 500;
/// Most branches whose verified head is kept.
pub const MAX_HEADS: usize = 50;
/// The name the instance's own signing key is allowed under.
pub const OWN_NAME: &str = "rustybin-instance";

const MAX_TRUST_BYTES: u64 = 64 * 1024;
const MAX_LOG_BYTES: usize = 1024 * 1024;

/// The key types an allowed signer may have (no certificates).
const KEY_TYPES: &[&str] = &[
    "ssh-ed25519",
    "ssh-rsa",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "sk-ssh-ed25519@openssh.com",
    "sk-ecdsa-sha2-nistp256@openssh.com",
];

const SIGNER_WHAT: &str =
    "an allowed signer is a name and a public key: ann ssh-ed25519 AAAA... (or the line of a .pub file)";

/// One allowed signer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signer {
    pub name: String,
    pub key: PublicKey,
}

impl Signer {
    /// The line the setting keeps: `name key-type base64-key`.
    pub fn line(&self) -> String {
        format!("{} {}", self.name, self.public())
    }

    /// `key-type base64-key`.
    fn public(&self) -> String {
        format!(
            "{} {}",
            self.key.key_type,
            base64::engine::general_purpose::STANDARD.encode(&self.key.blob)
        )
    }

    /// The line of the allowed signers file: good for git signatures only.
    fn file_line(&self) -> String {
        format!("{} namespaces=\"git\" {}", self.name, self.public())
    }
}

fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@' | '+')
}

/// A name from a public key's comment: what is not a name character
/// becomes `-`.
fn name_from(comment: &str) -> String {
    let name: String = comment
        .chars()
        .map(|c| if name_char(c) { c } else { '-' })
        .take(64)
        .collect();
    let name = name.trim_matches('-').to_string();
    if name.is_empty() {
        "signer".to_string()
    } else {
        name
    }
}

/// One line of the setting: `name key-type base64-key`, or a public key as
/// a `.pub` file or a git host shows it (`key-type base64-key comment`,
/// the comment becomes the name). Options (`cert-authority`, `namespaces`)
/// are not accepted: every signer is good for git signatures only.
pub fn parse_signer(line: &str) -> Result<Signer, String> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    let is_type = |t: &str| KEY_TYPES.contains(&t);
    let (name, key_type, blob) = match parts.as_slice() {
        [t, blob, comment @ ..] if is_type(t) => (name_from(&comment.join("-")), *t, *blob),
        [name, t, blob, ..] if is_type(t) => {
            if name.is_empty() || name.len() > 64 || !name.chars().all(name_char) {
                return Err(format!(
                    "{name:?} is not a signer's name (1 to 64 letters, digits, . _ - @ +): {SIGNER_WHAT}"
                ));
            }
            (name.to_string(), *t, *blob)
        }
        _ => {
            return Err(format!(
                "not an allowed signer: {SIGNER_WHAT}; key types: {}",
                KEY_TYPES.join(", ")
            ))
        }
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(blob)
        .map_err(|_| format!("{name}: the key is not base64: {SIGNER_WHAT}"))?;
    let mut at = 0;
    if signing::field(&bytes, &mut at) != Some(key_type.as_bytes()) {
        return Err(format!(
            "{name}: the key is not a {key_type} public key: {SIGNER_WHAT}"
        ));
    }
    Ok(Signer {
        name,
        key: PublicKey {
            key_type: key_type.to_string(),
            blob: bytes,
        },
    })
}

/// The lines of a value of the setting: a list, or text with one signer a
/// line (or comma separated); empty lines and `#` comments dropped.
fn lines_of(value: &Value) -> Vec<String> {
    let split = |s: &str| -> Vec<String> {
        s.split(['\n', ','])
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect()
    };
    match value {
        Value::String(s) => split(s),
        Value::Array(items) => items
            .iter()
            .flat_map(|i| match i.as_str() {
                Some(s) => split(s),
                None => vec![i.to_string()],
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The setting's value as it is kept: every line that is a signer written
/// `name key-type base64-key` (a line that is not one stays, for the check
/// to refuse by name).
pub fn normalize_signers(value: &Value) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    Value::Array(
        lines_of(value)
            .into_iter()
            .map(|l| match parse_signer(&l) {
                Ok(s) => Value::String(s.line()),
                Err(_) => Value::String(l),
            })
            .collect(),
    )
}

/// The setting's check: every line is a signer.
pub fn check_signers(value: &Value) -> Result<(), String> {
    for line in lines_of(value) {
        parse_signer(&line)?;
    }
    Ok(())
}

/// A full object name: 40 (SHA-1) or 64 (SHA-256) lowercase hex digits.
fn is_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A head and when it was trusted or verified.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Mark {
    sha: String,
    at: String,
}

/// `<state>/repo.verified.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Trust {
    /// When verification was turned on.
    #[serde(default)]
    since: String,
    /// The head trusted as it was at that moment (none: no commit yet).
    #[serde(default)]
    trusted: Option<Mark>,
    /// The last verified head of each branch.
    #[serde(default)]
    heads: BTreeMap<String, Mark>,
}

impl Trust {
    /// Every commit that needs no verification any more.
    fn known(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for mark in self.trusted.iter().chain(self.heads.values()) {
            if is_sha(&mark.sha) && !out.contains(&mark.sha) {
                out.push(mark.sha.clone());
            }
        }
        out
    }
}

/// Why a commit, or the whole check, did not pass.
#[derive(Clone, Debug)]
struct Refusal {
    commit: Option<String>,
    why: String,
}

impl Refusal {
    fn plain(why: impl Into<String>) -> Self {
        Self {
            commit: None,
            why: why.into(),
        }
    }

    fn of(commit: &str, why: impl Into<String>) -> Self {
        Self {
            commit: Some(commit.to_string()),
            why: why.into(),
        }
    }

    fn into_error(self) -> GitError {
        let subject = match &self.commit {
            Some(c) => format!("commit {} {}", short(c), self.why),
            None => self.why.clone(),
        };
        let mut e = GitError::conflict(format!(
            "not verified: {subject}. Nothing was applied and the working copy did not move (git.verify is on: every commit since the last verified head needs an allowed signer's SSH signature; a merge commit passes unsigned only as the clean merge of two verified parents, a squash or rebase merge made by the git host never does)"
        ));
        e.extra = json!({
            "reason": "unverified",
            "commit": self.commit,
            "why": self.why,
        });
        e
    }
}

/// One commit of the range, as `git log` printed it.
struct Seen {
    sha: String,
    parents: Vec<String>,
    tree: String,
    status: String,
    fingerprint: String,
}

fn parse_log(out: &str) -> Result<Vec<Seen>, String> {
    let mut commits = Vec::new();
    for record in out.split('\u{1e}') {
        let record = record.trim();
        if record.is_empty() {
            continue;
        }
        let mut f = record.split('\u{1f}');
        let (Some(sha), Some(parents), Some(tree), Some(status), Some(fingerprint)) =
            (f.next(), f.next(), f.next(), f.next(), f.next())
        else {
            return Err("git log answered something that is not a commit".into());
        };
        let parents: Vec<String> = parents.split_whitespace().map(str::to_string).collect();
        if !is_sha(sha) || !is_sha(tree) || parents.iter().any(|p| !is_sha(p)) {
            return Err("git log answered something that is not a commit".into());
        }
        commits.push(Seen {
            sha: sha.to_string(),
            parents,
            tree: tree.to_string(),
            status: status.trim().to_string(),
            fingerprint: fingerprint.trim().to_string(),
        });
    }
    Ok(commits)
}

/// A fingerprint as git prints it, safe to answer.
fn fingerprint_text(f: &str) -> Option<&str> {
    let rest = f.strip_prefix("SHA256:")?;
    (rest.len() <= 64
        && !rest.is_empty()
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')))
    .then_some(f)
}

impl SourceControl {
    /// Are pulled commits verified (`git.verify`)?
    pub fn verify_enabled(&self) -> bool {
        self.settings.bool(VERIFY_KEY)
    }

    /// The signers of the setting (a line that is not one is left out: the
    /// setting's check refuses it when it is written).
    fn listed_signers(&self) -> Vec<Signer> {
        lines_of(&self.settings.get(ALLOWED_SIGNERS_KEY).0)
            .iter()
            .filter_map(|l| parse_signer(l).ok())
            .take(MAX_SIGNERS)
            .collect()
    }

    /// The instance's own signing key, allowed while it is the current key.
    fn own_signer(&self) -> Option<Signer> {
        self.signing_public().ok().flatten().map(|key| Signer {
            name: OWN_NAME.to_string(),
            key,
        })
    }

    /// Why verification is on and cannot pass anything new (`None`: off,
    /// or it works).
    pub fn verify_problem(&self) -> Option<String> {
        if !self.verify_enabled() {
            return None;
        }
        self.verify_cannot().or_else(|| {
            (self.listed_signers().is_empty() && self.own_signer().is_none()).then(|| {
                "git.verify is on and nobody is an allowed signer: every new commit of the repository is refused until git.allowed_signers names a public key (rustybin git signers --add) or this instance has a signing key".to_string()
            })
        })
    }

    /// Why nothing can be verified at all.
    fn verify_cannot(&self) -> Option<String> {
        if self.state_dir.is_none() {
            return Some(
                "git.verify is on and there is no RUSTYBIN_STATE_DIR: the verified heads and the allowed signers file live there".into(),
            );
        }
        if let Some(Some(version)) = self.git_version.get() {
            if let Some(found) = signing::git_version_of(version) {
                if found < MIN_GIT {
                    return Some(format!(
                        "git.verify is on and git {}.{} cannot verify a merge commit: git {}.{} or later is needed (git merge-tree --write-tree)",
                        found.0, found.1, MIN_GIT.0, MIN_GIT.1
                    ));
                }
            }
        }
        None
    }

    fn trust_path(&self) -> Option<PathBuf> {
        self.state_file(VERIFIED_FILE)
    }

    /// The verified heads: `None` when there is no file (verification was
    /// never turned on here), nothing trusted when the file cannot be read.
    fn load_trust(&self) -> Option<Trust> {
        let path = self.trust_path()?;
        let meta = std::fs::metadata(&path).ok()?;
        let read = (meta.len() <= MAX_TRUST_BYTES)
            .then(|| std::fs::read(&path).ok())
            .flatten()
            .and_then(|b| serde_json::from_slice::<Trust>(&b).ok());
        match read {
            Some(mut trust) => {
                trust.trusted = trust.trusted.filter(|m| is_sha(&m.sha));
                trust.heads.retain(|_, m| is_sha(&m.sha));
                Some(trust)
            }
            None => {
                tracing::warn!(target: "rustybin::git", "{} cannot be read: no head is trusted until a pull verifies", path.display());
                Some(Trust::default())
            }
        }
    }

    fn save_trust(&self, trust: &Trust) -> Result<(), String> {
        let path = self
            .trust_path()
            .ok_or_else(|| "no RUSTYBIN_STATE_DIR".to_string())?;
        if let Some(dir) = path.parent() {
            crate::settings::create_private_dir(dir)?;
        }
        let mut bytes = serde_json::to_vec_pretty(trust).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        crate::settings::write_private(&path, &bytes)
    }

    /// The working copy's head, read without a remote.
    async fn local_head(&self) -> Option<String> {
        let repo = self.repo_dir()?;
        if !self.initialised(&repo) {
            return None;
        }
        let env = self.base_env();
        let out = self
            .git(
                &env,
                &repo,
                &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
            )
            .await;
        let sha = out.stdout.trim().to_string();
        (out.ok && is_sha(&sha)).then_some(sha)
    }

    /// Keep the verified heads in step with `git.verify`: turned off, they
    /// are forgotten; turned on (also off and on again between two looks),
    /// the head of this moment is trusted as it is, and the timeline says
    /// so. The caller holds the operations' lock.
    pub(super) async fn verify_sync(&self) {
        let count = self.settings.changes_of(VERIFY_KEY);
        let seen = self.verify_seen.swap(count, Ordering::AcqRel);
        let Some(path) = self.trust_path() else {
            return;
        };
        if !self.verify_enabled() {
            if path.exists() {
                let _ = std::fs::remove_file(&path);
            }
            if let Ok(mut last) = self.last_refusal.lock() {
                *last = None;
            }
            return;
        }
        if path.exists() && seen == count {
            return;
        }
        let started = Instant::now();
        let head = self.local_head().await;
        let at = now();
        let trust = Trust {
            since: at.clone(),
            trusted: head.clone().map(|sha| Mark { sha, at }),
            heads: BTreeMap::new(),
        };
        let result = self.save_trust(&trust).map(|_| match &head {
            Some(sha) => format!(
                "verification turned on: the head {} is trusted as it is, every commit after it needs an allowed signer",
                short(sha)
            ),
            None => "verification turned on: there is no commit yet, so nothing is trusted and every commit of the branch needs an allowed signer".to_string(),
        });
        self.record("verify", "settings", started, &result);
    }

    /// Write the allowed signers file; the fingerprints it allows.
    fn write_signers(&self) -> Result<(PathBuf, BTreeSet<String>), String> {
        let path = self
            .state_file(ALLOWED_SIGNERS_FILE)
            .ok_or_else(|| "no RUSTYBIN_STATE_DIR".to_string())?;
        let mut text = String::new();
        let mut allowed = BTreeSet::new();
        for signer in self.listed_signers().iter().chain(self.own_signer().iter()) {
            text.push_str(&signer.file_line());
            text.push('\n');
            allowed.insert(signer.key.fingerprint());
        }
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            if let Some(dir) = path.parent() {
                crate::settings::create_private_dir(dir)?;
            }
            crate::settings::write_private(&path, text.as_bytes())?;
        }
        Ok((path, allowed))
    }

    /// A git command's output, whole (never the diagnostic cut of `git`).
    async fn raw(&self, env: &GitEnv, cwd: &Path, args: &[&str]) -> Result<String, String> {
        let mut cmd = self.command(env, cwd, args);
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        match tokio::time::timeout(GIT_TIMEOUT, cmd.output()).await {
            Ok(Ok(out)) if out.status.success() && out.stdout.len() <= MAX_LOG_BYTES => {
                Ok(String::from_utf8_lossy(&out.stdout).into_owned())
            }
            Ok(Ok(out)) if out.status.success() => Err("git answered more than it may".into()),
            Ok(Ok(_)) => Err(format!("git {} failed", args.first().unwrap_or(&""))),
            Ok(Err(e)) => Err(format!("git could not be started: {e}")),
            Err(_) => Err(format!(
                "git {} did not finish within {} s",
                args.first().unwrap_or(&""),
                GIT_TIMEOUT.as_secs()
            )),
        }
    }

    /// Does every commit of `rev` that is not below a verified head pass?
    /// The commit `rev` names, or the first commit (oldest first) that
    /// does not pass and why. Changes nothing.
    async fn check_rev(&self, env: &GitEnv, repo: &Path, rev: &str) -> Result<String, Refusal> {
        let _ = self.git_version().await;
        if let Some(problem) = self.verify_cannot() {
            return Err(Refusal::plain(problem));
        }
        let named = format!("{rev}^{{commit}}");
        let out = self
            .git(env, repo, &["rev-parse", "--verify", "--quiet", &named])
            .await;
        let sha = out.stdout.trim().to_string();
        if !out.ok || !is_sha(&sha) {
            return Err(Refusal::plain(format!(
                "{rev} is not a commit of the working copy"
            )));
        }
        let known = self.load_trust().unwrap_or_default().known();
        if known.contains(&sha) {
            return Ok(sha);
        }
        let (signers, allowed) = self.write_signers().map_err(|e| {
            Refusal::plain(format!(
                "the allowed signers file could not be written: {e}"
            ))
        })?;
        // SSH signatures only: no OpenPGP or X.509 program is ever run, so
        // such a signature reads as none.
        let mut config = env.config.clone();
        config.extend([
            format!("gpg.ssh.allowedSignersFile={}", signers.display()),
            "gpg.program=false".to_string(),
            "gpg.openpgp.program=false".to_string(),
            "gpg.x509.program=false".to_string(),
        ]);
        let verify_env = GitEnv {
            vars: env.vars.clone(),
            config,
        };
        let limit = format!("-n{}", MAX_VERIFY_COMMITS + 1);
        let mut args = vec![
            "log",
            "--ignore-missing",
            "--topo-order",
            limit.as_str(),
            "--format=%H%x1f%P%x1f%T%x1f%G?%x1f%GF%x1e",
            sha.as_str(),
            "--not",
        ];
        args.extend(known.iter().map(String::as_str));
        let commits = match self.raw(&verify_env, repo, &args).await {
            Ok(text) => parse_log(&text),
            Err(e) => Err(e),
        }
        .map_err(|e| Refusal::plain(format!("the commits could not be read: {e}")))?;
        if commits.len() > MAX_VERIFY_COMMITS {
            return Err(Refusal::plain(format!(
                "more than {MAX_VERIFY_COMMITS} commits since the last verified head: more than one pull verifies"
            )));
        }
        // Oldest first, so the refusal names where the trouble starts.
        for c in commits.iter().rev() {
            let signed = c.status == "G" && allowed.contains(&c.fingerprint);
            if signed {
                continue;
            }
            match c.parents.as_slice() {
                [first, second] => {
                    let merged = self
                        .git(env, repo, &["merge-tree", "--write-tree", first, second])
                        .await;
                    let clean =
                        merged.ok && merged.stdout.lines().next().map(str::trim) == Some(&c.tree);
                    if !clean {
                        return Err(Refusal::of(
                            &c.sha,
                            "is a merge commit that carries a change of its own (its tree is not what merging its two parents gives) and no allowed signer signed it",
                        ));
                    }
                }
                [_, _, _, ..] => {
                    return Err(Refusal::of(
                        &c.sha,
                        "is a merge of more than two parents and no allowed signer signed it",
                    ))
                }
                _ => {
                    let why = match (c.status.as_str(), fingerprint_text(&c.fingerprint)) {
                        ("N", _) => "has no SSH signature".to_string(),
                        ("B", _) => "has a signature that does not match its content".to_string(),
                        ("G" | "U", Some(f)) => {
                            format!("is signed with a key that is not an allowed signer ({f})")
                        }
                        _ => "has a signature that could not be checked against the allowed signers (is ssh-keygen installed?)".to_string(),
                    };
                    return Err(Refusal::of(&c.sha, why));
                }
            }
        }
        Ok(sha)
    }

    /// The gate of every path that takes the repository's side: with
    /// `git.verify` on, `rev` passes or the answer is the `409` with
    /// `reason: unverified`. `keep` remembers a passed head as the
    /// branch's verified head (not for a dry run). The caller holds the
    /// operations' lock and has moved nothing yet.
    pub(super) async fn verify_rev(
        &self,
        env: &GitEnv,
        repo: &Path,
        rev: &str,
        keep: bool,
    ) -> GitResult<()> {
        self.verify_sync().await;
        if !self.verify_enabled() {
            return Ok(());
        }
        match self.check_rev(env, repo, rev).await {
            Ok(sha) => {
                if let Ok(mut last) = self.last_refusal.lock() {
                    *last = None;
                }
                if keep {
                    self.keep_verified(&sha);
                }
                Ok(())
            }
            Err(refusal) => {
                if let Ok(mut last) = self.last_refusal.lock() {
                    *last = Some(json!({
                        "at": now(),
                        "branch": self.branch(),
                        "commit": refusal.commit,
                        "why": refusal.why,
                    }));
                }
                Err(refusal.into_error())
            }
        }
    }

    /// `sha` is the configured branch's verified head from now on.
    fn keep_verified(&self, sha: &str) {
        let mut trust = self.load_trust().unwrap_or_default();
        let branch = self.branch();
        if trust.heads.get(&branch).is_some_and(|m| m.sha == sha) {
            return;
        }
        trust.heads.insert(
            branch,
            Mark {
                sha: sha.to_string(),
                at: now(),
            },
        );
        while trust.heads.len() > MAX_HEADS {
            let oldest = trust
                .heads
                .iter()
                .min_by(|a, b| a.1.at.cmp(&b.1.at))
                .map(|(name, _)| name.clone());
            match oldest {
                Some(name) => trust.heads.remove(&name),
                None => break,
            };
        }
        if let Err(e) = self.save_trust(&trust) {
            tracing::warn!(target: "rustybin::git", "the verified head could not be kept: {e}");
        }
    }

    /// The instance committed on top of `before`: its own commit is the
    /// verified head when what it stands on passes (it made the commit
    /// itself, signed or not). On top of something unverified it is not.
    pub(super) async fn own_commit(&self, env: &GitEnv, repo: &Path, before: Option<&str>) {
        if !self.verify_enabled() {
            return;
        }
        let stands = match before {
            None => true,
            Some(sha) => self.check_rev(env, repo, sha).await.is_ok(),
        };
        if !stands {
            return;
        }
        if let Some(head) = self.local_head().await {
            self.keep_verified(&head);
        }
    }

    /// The status' `verification`: whether pulled commits are verified,
    /// the allowed signers (names, key types, fingerprints), the head
    /// trusted when verification was turned on, the branch's last verified
    /// head, the last refusal, and why it cannot work when it is on and
    /// cannot.
    pub fn verification(&self) -> Value {
        let enabled = self.verify_enabled();
        let signers: Vec<Value> = self
            .listed_signers()
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "key_type": s.key.key_type,
                    "fingerprint": s.key.fingerprint(),
                })
            })
            .collect();
        let own = self.own_signer().map(|s| {
            json!({
                "name": s.name,
                "key_type": s.key.key_type,
                "fingerprint": s.key.fingerprint(),
            })
        });
        let trust = if enabled { self.load_trust() } else { None };
        let mark = |m: &Mark| json!({ "sha": m.sha, "short": short(&m.sha), "at": m.at });
        let branch = self.branch();
        let trusted = trust.as_ref().and_then(|t| t.trusted.as_ref());
        let verified = trust
            .as_ref()
            .and_then(|t| t.heads.get(&branch))
            .or(trusted);
        json!({
            "enabled": enabled,
            "signers": signers,
            "own_key": own,
            "since": trust.as_ref().map(|t| t.since.clone()).filter(|s| !s.is_empty()),
            "trusted": trusted.map(mark),
            "verified_head": verified.map(mark),
            "last_refusal": self.last_refusal.lock().ok().and_then(|l| l.clone()),
            "problem": self.verify_problem(),
        })
    }

    /// The last refusal, for the diagnostics.
    pub fn last_refusal(&self) -> Option<Value> {
        self.last_refusal.lock().ok().and_then(|l| l.clone())
    }
}
