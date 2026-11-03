//! Checkpoints in the configuration repository (step 05 of
//! `docs/plans/testing-2`): `<repo>/<git.path>/checkpoints/<name>.json`, the
//! format of `<state>/checkpoints/`. A checkpoint holds mock data, so it is
//! written only by an explicit commit that names it, never by the automatic
//! mode (whose commits never stage the directory). It is read back from the
//! commit `HEAD` names, each file's size checked before it is read; the
//! parse, the checks and the restore are `crate::checkpoints`'.
//!
//! Secrets: a checkpoint holds no instance secret (never settings). The
//! mocks' demo credentials are part of their settings and follow the rule of
//! the document a commit writes next to it (`SecretsMode::Refs`: as they
//! are), so a checkpoint in the repository shows nothing its
//! `rustybin.yaml` does not.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::Value;

use super::{busy, GitEnv, GitError, GitResult, SourceControl};
use crate::checkpoints::{valid_name, Checkpoint, MAX_CHECKPOINTS, MAX_CHECKPOINT_BYTES};
use crate::state::AppState;

/// The directory next to `rustybin.yaml`.
pub const CHECKPOINTS_DIR: &str = "checkpoints";

/// A checkpoint file of a commit: its name (the file's stem) and size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub bytes: u64,
}

/// The repository's checkpoints at `HEAD`, each with its bytes or why they
/// were not read.
pub struct Listing {
    pub head: Option<Value>,
    /// `<git.path>/checkpoints`.
    pub path: String,
    pub entries: Vec<(Entry, Result<Vec<u8>, String>)>,
    /// More checkpoint files than a listing reads ([`MAX_CHECKPOINTS`]).
    pub truncated: bool,
}

/// The local checkpoints a commit names, checked before anything is
/// written: valid names, each saved on this instance, at most
/// [`MAX_CHECKPOINTS`].
pub fn to_write(state: &AppState, names: &[String]) -> GitResult<Vec<Arc<Checkpoint>>> {
    let mut unique: Vec<&str> = Vec::new();
    for n in names {
        let n = n.trim();
        if !valid_name(n) {
            return Err(GitError::new(
                StatusCode::BAD_REQUEST,
                format!("checkpoint {n:?}: 1 to 64 letters, digits, . _ -"),
            ));
        }
        if !unique.contains(&n) {
            unique.push(n);
        }
    }
    if unique.len() > MAX_CHECKPOINTS {
        return Err(GitError::new(
            StatusCode::BAD_REQUEST,
            format!("at most {MAX_CHECKPOINTS} checkpoints in a commit"),
        ));
    }
    unique
        .into_iter()
        .map(|n| {
            state.checkpoints.get(n).ok_or_else(|| {
                GitError::new(
                    StatusCode::NOT_FOUND,
                    format!(
                        "no checkpoint named {n:?} on this instance: save it first (POST /_rustybin/checkpoints)"
                    ),
                )
            })
        })
        .collect()
}

fn refuse_link(p: &Path) -> GitResult<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => Err(GitError::new(
            StatusCode::CONFLICT,
            format!(
                "{} is a symbolic link: refused (the working copy must hold plain files)",
                p.display()
            ),
        )),
        _ => Ok(()),
    }
}

/// Parse `git ls-tree -l -z` output: blobs named `<valid name>.json`, in
/// git's order, at most [`MAX_CHECKPOINTS`] (the flag says there were more).
pub fn parse_ls_tree(out: &str) -> (Vec<Entry>, bool) {
    let mut entries = Vec::new();
    let mut truncated = false;
    for record in out.split('\0') {
        let Some((meta, file)) = record.split_once('\t') else {
            continue;
        };
        let mut parts = meta.split_whitespace();
        let (Some(_mode), Some(kind), Some(_object), Some(size)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Some(name) = file.strip_suffix(".json") else {
            continue;
        };
        if kind != "blob" || !valid_name(name) {
            continue;
        }
        let Ok(bytes) = size.parse::<u64>() else {
            continue;
        };
        if entries.len() >= MAX_CHECKPOINTS {
            truncated = true;
            break;
        }
        entries.push(Entry {
            name: name.to_string(),
            bytes,
        });
    }
    (entries, truncated)
}

impl SourceControl {
    /// `<git.path>/checkpoints`, relative to the repository's root.
    pub fn checkpoints_rel(&self) -> String {
        match self.path() {
            Some(p) => format!("{p}/{CHECKPOINTS_DIR}"),
            None => CHECKPOINTS_DIR.to_string(),
        }
    }

    /// The largest checkpoint file read from the repository.
    pub fn max_checkpoint_bytes(&self) -> usize {
        self.max_checkpoint_bytes.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn set_max_checkpoint_bytes(&self, max: usize) {
        self.max_checkpoint_bytes.store(max, Ordering::Relaxed);
    }

    /// Write checkpoints into the working copy (an explicit commit only),
    /// pretty-printed for a reviewer's diff. Returns their paths relative
    /// to the repository's root.
    pub(super) fn write_checkpoints(
        &self,
        kept: &[Arc<Checkpoint>],
        repo: &Path,
    ) -> GitResult<Vec<String>> {
        if kept.is_empty() {
            return Ok(Vec::new());
        }
        let io = |p: &Path, e: std::io::Error| {
            GitError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("{}: {e}", p.display()),
            )
        };
        let dir = self.document_dir(repo).join(CHECKPOINTS_DIR);
        refuse_link(&dir)?;
        std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
        let root = std::fs::canonicalize(repo).map_err(|e| io(repo, e))?;
        let real = std::fs::canonicalize(&dir).map_err(|e| io(&dir, e))?;
        if !real.starts_with(&root) {
            return Err(GitError::new(
                StatusCode::CONFLICT,
                format!(
                    "{} resolves outside {}: refused",
                    dir.display(),
                    repo.display()
                ),
            ));
        }
        let rel = self.checkpoints_rel();
        let mut written = Vec::new();
        for cp in kept {
            let mut bytes = serde_json::to_vec_pretty(cp.as_ref())
                .map_err(|e| GitError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            bytes.push(b'\n');
            if bytes.len() > MAX_CHECKPOINT_BYTES {
                return Err(GitError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!(
                        "checkpoint {} would be {} bytes in the repository: at most {MAX_CHECKPOINT_BYTES}",
                        cp.name,
                        bytes.len()
                    ),
                ));
            }
            let file = dir.join(format!("{}.json", cp.name));
            refuse_link(&file)?;
            std::fs::write(&file, &bytes).map_err(|e| io(&file, e))?;
            written.push(format!("{rel}/{}.json", cp.name));
        }
        Ok(written)
    }

    /// The checkpoint files of the commit `rev` names (none when it has no
    /// checkpoints directory).
    pub(super) async fn checkpoint_entries(
        &self,
        env: &GitEnv,
        repo: &Path,
        rev: &str,
    ) -> (Vec<Entry>, bool) {
        let tree = format!("{rev}:{}", self.checkpoints_rel());
        let out = self.git(env, repo, &["ls-tree", "-l", "-z", &tree]).await;
        if !out.ok {
            return (Vec::new(), false);
        }
        // The diagnostic cut of `git` (8 KiB) holds far more than the
        // entries a listing keeps; a cut is said as more entries.
        let (entries, truncated) = parse_ls_tree(&out.stdout);
        (entries, truncated || out.stdout.ends_with("..."))
    }

    /// `GET /_rustybin/checkpoints?from=git`: the checkpoint files at
    /// `HEAD`, each read whole within the caps (a file over the cap is
    /// listed with the reason, never read).
    pub async fn repository_checkpoints(&self) -> GitResult<Listing> {
        let (repo, remote) = self.ready().await?;
        let env = self.git_env(&remote)?;
        let _guard = self.lock.try_lock().map_err(|_| busy())?;
        self.need_init(&repo)?;
        let path = self.checkpoints_rel();
        let head = self.head(&env, &repo).await;
        if head.is_none() {
            return Ok(Listing {
                head,
                path,
                entries: Vec::new(),
                truncated: false,
            });
        }
        let (entries, truncated) = self.checkpoint_entries(&env, &repo, "HEAD").await;
        let max = self.max_checkpoint_bytes();
        let mut budget = crate::checkpoints::MAX_TOTAL_BYTES as u64;
        let mut read = Vec::with_capacity(entries.len());
        for e in entries {
            let bytes = if e.bytes > max as u64 {
                Err(format!(
                    "{} bytes: larger than {max} bytes, not read (the supported size)",
                    e.bytes
                ))
            } else if e.bytes > budget {
                Err(format!(
                    "not read: the checkpoints listed before it fill the {} bytes a listing reads",
                    crate::checkpoints::MAX_TOTAL_BYTES
                ))
            } else {
                budget -= e.bytes;
                self.git_bytes(&env, &repo, &format!("HEAD:{path}/{}.json", e.name), max)
                    .await
            };
            read.push((e, bytes));
        }
        Ok(Listing {
            head,
            path,
            entries: read,
            truncated,
        })
    }

    /// One checkpoint file at `HEAD`, whole: its size is checked first, a
    /// larger file is refused, never read or cut.
    pub async fn repository_checkpoint(&self, name: &str) -> GitResult<(Vec<u8>, Option<Value>)> {
        if !valid_name(name) {
            return Err(GitError::new(
                StatusCode::BAD_REQUEST,
                format!("checkpoint {name:?}: 1 to 64 letters, digits, . _ -"),
            ));
        }
        let (repo, remote) = self.ready().await?;
        let env = self.git_env(&remote)?;
        let _guard = self.lock.try_lock().map_err(|_| busy())?;
        self.need_init(&repo)?;
        let head = self.head(&env, &repo).await;
        if head.is_none() {
            return Err(GitError::new(
                StatusCode::NOT_FOUND,
                "the working copy has no commit yet: pull first (POST /_rustybin/git/pull)",
            ));
        }
        let object = format!("HEAD:{}/{name}.json", self.checkpoints_rel());
        let max = self.max_checkpoint_bytes();
        let bytes = self
            .git_bytes(&env, &repo, &object, max)
            .await
            .map_err(|e| {
                if e.contains("not in the repository") {
                    GitError::new(
                        StatusCode::NOT_FOUND,
                        format!(
                            "the repository has no checkpoint {name:?} at HEAD ({object}): commit it on an instance (git commit --checkpoint {name}), push, then pull here"
                        ),
                    )
                } else if e.contains("larger than") {
                    GitError::new(StatusCode::PAYLOAD_TOO_LARGE, e)
                } else {
                    GitError::failed(e)
                }
            })?;
        Ok((bytes, head))
    }
}
