//! Signed commits (step 11 of `docs/plans/core-next/master-plan.md`): SSH
//! signatures with a key of their own.
//!
//! The key is the secret setting `git.signing_key`: an OpenSSH private key
//! without a passphrase, kept as one line (the base64 between the `BEGIN`
//! and `END` lines). Never the deploy key: a key that pushes and a key that
//! vouches are registered in different places at a git host. With `git.sign`
//! on, a commit runs with `gpg.format=ssh`, `commit.gpgsign=true` and
//! `user.signingkey` pointing at `<state>/repo.signing.key`, which is written
//! from the secret when it differs (mode 0600, never logged). The public key
//! and its fingerprint are read from the key itself (the public half is a
//! field of the OpenSSH key format) and answered so the owner registers them
//! at the host; the private half is never answered.
//!
//! Signing on without a usable key, or with a git older than 2.34, refuses
//! the commit with the reason: it never falls back to an unsigned commit.

use std::collections::BTreeMap;
use std::time::Instant;

use axum::http::StatusCode;
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{busy, GitEnv, GitError, GitResult, SourceControl, GIT_TIMEOUT};

/// Whether commits are signed.
pub const SIGN_KEY: &str = "git.sign";
/// The signing key (a secret).
pub const SIGNING_KEY_KEY: &str = "git.signing_key";
/// The signing key's file in the state directory.
pub const SIGNING_KEY_FILE: &str = "repo.signing.key";
/// The first git that signs with SSH keys.
pub const MIN_GIT: (u32, u32) = (2, 34);

const BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
const END: &str = "-----END OPENSSH PRIVATE KEY-----";
const MAGIC: &[u8] = b"openssh-key-v1\0";
const WHAT: &str =
    "an OpenSSH private key without a passphrase (ssh-keygen -t ed25519 -N \"\"), or generate one here";

/// A pasted key as one line: the `BEGIN` and `END` lines and every space
/// and line break removed (the settings store keeps secrets on one line).
pub fn one_line(value: &Value) -> Value {
    match value.as_str() {
        Some(s) => Value::String(
            s.replace(BEGIN, "")
                .replace(END, "")
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect(),
        ),
        None => value.clone(),
    }
}

/// The setting's check: a key git can sign with.
pub fn check_key(value: &Value) -> Result<(), String> {
    parse(value.as_str().unwrap_or("")).map(|_| ())
}

/// The public half of a signing key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey {
    /// `ssh-ed25519`, `ecdsa-sha2-nistp256`, `ssh-rsa`, ...
    pub key_type: String,
    /// The public key blob (what a host stores, what the fingerprint hashes).
    pub blob: Vec<u8>,
}

impl PublicKey {
    /// `ssh-ed25519 AAAA... comment`, the line a git host takes.
    pub fn line(&self, comment: &str) -> String {
        format!(
            "{} {} {comment}",
            self.key_type,
            base64::engine::general_purpose::STANDARD.encode(&self.blob)
        )
    }

    /// `SHA256:...`, as `ssh-keygen -l` and the hosts show it.
    pub fn fingerprint(&self) -> String {
        format!(
            "SHA256:{}",
            base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(&self.blob))
        )
    }
}

/// One length-prefixed field of the key format.
fn field<'a>(bytes: &'a [u8], at: &mut usize) -> Option<&'a [u8]> {
    let len = u32::from_be_bytes(bytes.get(*at..*at + 4)?.try_into().ok()?) as usize;
    let start = *at + 4;
    let end = start.checked_add(len)?;
    let out = bytes.get(start..end)?;
    *at = end;
    Some(out)
}

/// The public half of a one-line OpenSSH private key, or why git could not
/// sign with it.
pub fn parse(one_line: &str) -> Result<PublicKey, String> {
    let bad = || format!("not a signing key: {WHAT}");
    let text = one_line.trim();
    if text.is_empty() {
        return Err(bad());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(text))
        .map_err(|_| bad())?;
    let rest = bytes.strip_prefix(MAGIC).ok_or_else(bad)?;
    let mut at = 0;
    let cipher = field(rest, &mut at).ok_or_else(bad)?;
    if cipher != b"none" {
        return Err(format!(
            "the signing key has a passphrase, and the instance cannot type one: {WHAT}"
        ));
    }
    let _kdf = field(rest, &mut at).ok_or_else(bad)?;
    let _kdf_options = field(rest, &mut at).ok_or_else(bad)?;
    let keys = u32::from_be_bytes(
        rest.get(at..at + 4)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(bad)?,
    );
    at += 4;
    if keys != 1 {
        return Err(bad());
    }
    let blob = field(rest, &mut at).ok_or_else(bad)?;
    let mut inner = 0;
    let key_type = field(blob, &mut inner)
        .and_then(|t| std::str::from_utf8(t).ok())
        .filter(|t| {
            !t.is_empty()
                && t.len() <= 64
                && t.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'@' | b'.'))
        })
        .ok_or_else(bad)?;
    Ok(PublicKey {
        key_type: key_type.to_string(),
        blob: blob.to_vec(),
    })
}

/// The key file's text: the one line wrapped as OpenSSH writes it.
fn pem(one_line: &str) -> String {
    let mut out = String::with_capacity(one_line.len() + 128);
    out.push_str(BEGIN);
    out.push('\n');
    let chars: Vec<char> = one_line.chars().collect();
    for line in chars.chunks(70) {
        out.extend(line.iter());
        out.push('\n');
    }
    out.push_str(END);
    out.push('\n');
    out
}

/// `(major, minor)` of `git version 2.39.5 (...)`.
pub fn git_version_of(text: &str) -> Option<(u32, u32)> {
    let v = text
        .split_whitespace()
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()) && w.contains('.'))?;
    let mut parts = v.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

impl SourceControl {
    /// Are commits signed (`git.sign`)?
    pub fn signing_enabled(&self) -> bool {
        self.settings.bool(SIGN_KEY)
    }

    /// The signing key's public half, when a usable key is set.
    fn signing_public(&self) -> Result<Option<PublicKey>, String> {
        match self.settings.secret(SIGNING_KEY_KEY) {
            None => Ok(None),
            Some(key) => parse(&key).map(Some),
        }
    }

    /// Why signing is on and cannot work (`None`: off, or it works). The
    /// git version is the one `git_version` found, when it has run.
    pub fn signing_problem(&self) -> Option<String> {
        if !self.signing_enabled() {
            return None;
        }
        match self.signing_public() {
            Ok(None) => {
                return Some(
                    "git.sign is on and no signing key is set: generate one (POST /_rustybin/git/signing-key, or Management, Source control) or set the secret git.signing_key, then register its public key at the git host".into(),
                )
            }
            Err(e) => return Some(format!("git.signing_key: {e}")),
            Ok(Some(_)) => {}
        }
        if self.state_dir.is_none() {
            return Some(
                "git.sign is on and there is no RUSTYBIN_STATE_DIR: the key file git signs with lives there".into(),
            );
        }
        if let Some(Some(version)) = self.git_version.get() {
            if let Some(found) = git_version_of(version) {
                if found < MIN_GIT {
                    return Some(format!(
                        "git.sign is on and git {}.{} cannot sign with an SSH key: git {}.{} or later is needed",
                        found.0, found.1, MIN_GIT.0, MIN_GIT.1
                    ));
                }
            }
        }
        None
    }

    /// `GET /_rustybin/git/signing-key` and the status: whether commits are
    /// signed, the public key and its fingerprint (never the private half),
    /// and why signing cannot work when it is on and cannot.
    pub fn signing(&self) -> Value {
        let public = self.signing_public();
        let key = public.as_ref().ok().cloned().flatten();
        json!({
            "enabled": self.signing_enabled(),
            "key_set": self.settings.secret_state(SIGNING_KEY_KEY)["set"] == json!(true),
            "public": key.as_ref().map(|k| k.line(&format!("rustybin-signing-{}", self.instance_id))),
            "fingerprint": key.as_ref().map(PublicKey::fingerprint),
            "key_type": key.as_ref().map(|k| k.key_type.clone()),
            "problem": self.signing_problem().or_else(|| public.err().map(|e| format!("git.signing_key: {e}"))),
        })
    }

    /// Make the next commits of `env` signed when `git.sign` is on: the key
    /// file written from the secret, the three settings added. `Ok(false)`
    /// when signing is off; an error when it is on and cannot work.
    pub(super) async fn sign_into(&self, env: &mut GitEnv) -> GitResult<bool> {
        if !self.signing_enabled() {
            return Ok(false);
        }
        let _ = self.git_version().await;
        if let Some(problem) = self.signing_problem() {
            return Err(GitError::conflict(format!(
                "{problem} (the commit was not made: a signed repository never gets an unsigned commit from here; turn git.sign off to commit without a signature)"
            )));
        }
        let (Some(key), Some(path)) = (
            self.settings.secret(SIGNING_KEY_KEY),
            self.state_file(SIGNING_KEY_FILE),
        ) else {
            return Err(GitError::conflict(
                "git.sign is on and no signing key is set",
            ));
        };
        let text = zeroize::Zeroizing::new(pem(&key));
        let current = std::fs::read_to_string(&path)
            .ok()
            .map(zeroize::Zeroizing::new);
        if current.as_ref().map(|c| c.as_str()) != Some(text.as_str()) {
            crate::settings::write_private(&path, text.as_bytes())
                .map_err(|e| GitError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        }
        env.config.push("gpg.format=ssh".to_string());
        env.config
            .push(format!("user.signingkey={}", path.display()));
        env.config.push("commit.gpgsign=true".to_string());
        Ok(true)
    }

    /// `POST /_rustybin/git/signing-key`: generate an ed25519 signing key
    /// and keep it as the secret `git.signing_key` (`replace` an existing
    /// one). The answer is the public half and its fingerprint.
    pub async fn generate_signing_key(&self, replace: bool, by: &str) -> GitResult<Value> {
        let started = Instant::now();
        let result = self.generate_signing_key_inner(replace, by).await;
        self.record(
            "signing-key",
            by,
            started,
            &result
                .as_ref()
                .map(|_| "signing key generated".to_string())
                .map_err(|e| e.message.clone()),
        );
        result
    }

    async fn generate_signing_key_inner(&self, replace: bool, by: &str) -> GitResult<Value> {
        let Some(state) = self.state_dir.clone() else {
            return Err(GitError::conflict(
                "source control needs RUSTYBIN_STATE_DIR: the signing key's file lives there",
            ));
        };
        let _guard = self.lock.try_lock().map_err(|_| busy())?;
        if self.settings.secret_state(SIGNING_KEY_KEY)["set"] == json!(true) && !replace {
            return Err(GitError::conflict(
                "a signing key is set: send {\"replace\": true} to generate a new one (commits signed with the old one stay verified only while the host keeps its public key)",
            ));
        }
        if !self.settings.writable(SIGNING_KEY_KEY) {
            return Err(GitError::conflict(
                "git.signing_key is locked by RUSTYBIN_SETTINGS_LOCK: not changed",
            ));
        }
        crate::settings::create_private_dir(&state)
            .map_err(|e| GitError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        // Generated next to the state, read into the secret, removed: the
        // secret is the one copy until a commit writes the key file.
        let temp = state.join(format!(".signing-{}", uuid::Uuid::new_v4().simple()));
        let public = temp.with_extension("pub");
        let comment = format!("rustybin-signing-{}", self.instance_id);
        let out = tokio::time::timeout(
            GIT_TIMEOUT,
            tokio::process::Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C", &comment, "-f"])
                .arg(&temp)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await;
        let read = match out {
            Ok(Ok(o)) if o.status.success() => std::fs::read_to_string(&temp)
                .map(zeroize::Zeroizing::new)
                .map_err(|e| GitError::failed(format!("the signing key was not written: {e}"))),
            Ok(Ok(o)) => Err(GitError::failed(format!(
                "ssh-keygen: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ))),
            Ok(Err(e)) => Err(GitError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("ssh-keygen is not available ({e}): the container image includes it; elsewhere install OpenSSH"),
            )),
            Err(_) => Err(GitError::failed("ssh-keygen did not finish")),
        };
        let _ = std::fs::remove_file(&temp);
        let _ = std::fs::remove_file(&public);
        let text = read?;
        let mut secrets = BTreeMap::new();
        secrets.insert(SIGNING_KEY_KEY.to_string(), Some(text.to_string()));
        self.settings
            .update(BTreeMap::new(), secrets, by)
            .map_err(|e| settings_error(SIGNING_KEY_KEY, e))?;
        let mut body = self.signing();
        body["generated"] = json!(true);
        Ok(body)
    }
}

/// A refused settings change as a git answer.
pub(super) fn settings_error(key: &str, e: crate::settings::UpdateError) -> GitError {
    use crate::settings::UpdateError;
    match e {
        UpdateError::Locked(keys) => GitError::conflict(format!(
            "{key} is locked by RUSTYBIN_SETTINGS_LOCK ({}): not changed",
            keys.join(", ")
        )),
        UpdateError::Invalid(list) => GitError::new(
            StatusCode::BAD_REQUEST,
            format!("{key}: {}", list.join("; ")),
        ),
        UpdateError::Io(e) => GitError::new(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}
