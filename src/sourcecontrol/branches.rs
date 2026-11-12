//! Branches and a diverged branch (step 11 of
//! `docs/plans/core-next/master-plan.md`).
//!
//! The instance still never merges. A diverged branch answers `409`, and
//! what is new are the ways out as calls: a side named with a preview
//! (`resolve` in `mod.rs`), or the instance's side pushed to a new branch
//! for a pull request on the host ([`SourceControl::resolve_pull_request`]).
//! [`SourceControl::divergence`] says what differs, section by section,
//! through `configdoc::diff`. Nothing here force-pushes or rewrites the
//! remote's history, and the automatic mode calls none of it.
//!
//! A branch per environment: the branch stays the `git.branch` setting.
//! [`SourceControl::branches`] lists the remote's, and
//! [`SourceControl::switch_branch`] moves the instance to one (a preview
//! first: the apply's own dry run of that branch's document) or starts a new
//! one from the instance. A document never names a branch, and a document
//! from the repository never changes a `git.*` setting
//! (`configdoc::REPOSITORY_REFUSED_PREFIXES`).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use axum::http::StatusCode;
use serde_json::{json, Value};

use super::signing::settings_error;
use super::{
    busy, check_branch, instance_document, read_document, GitEnv, GitError, GitResult,
    SourceControl, TempDir, BRANCH_KEY, GIT_TIMEOUT,
};
use crate::configdoc::{self, Change, DOCUMENT_NAME};
use crate::state::AppState;

/// Most branches a listing answers.
pub const MAX_BRANCHES: usize = 200;
/// Most bytes read from `git ls-remote`.
const MAX_LS_REMOTE: usize = 512 * 1024;
/// Most commits a divergence names per side.
pub const MAX_SIDE_COMMITS: usize = 10;
/// The ways out of a diverged branch.
pub const RESOLVE_OPTIONS: [&str; 3] = ["instance", "repository", "pull_request"];

/// `POST /_rustybin/git/branch`.
#[derive(Clone, Debug, Default)]
pub struct BranchRequest {
    pub branch: String,
    /// Start the branch from the instance (it must not exist on the remote).
    pub create: bool,
    /// Apply the branch's document after a switch.
    pub apply: bool,
    pub dry_run: bool,
    pub prune: bool,
    /// The commit message of a new branch's first commit.
    pub message: String,
}

fn branch_name(raw: &str) -> GitResult<String> {
    let name = raw.trim().to_string();
    check_branch(&json!(name)).map_err(|e| GitError::new(StatusCode::BAD_REQUEST, e))?;
    Ok(name)
}

fn changes_json(changes: &[Change]) -> Value {
    json!({
        "changes": changes,
        "summary": changes.iter().map(Change::line).collect::<Vec<_>>(),
    })
}

impl SourceControl {
    /// The subjects of the commits in `range`, newest first.
    async fn subjects(&self, env: &GitEnv, repo: &Path, range: &str) -> Vec<String> {
        let n = format!("-n{MAX_SIDE_COMMITS}");
        let out = self
            .git(env, repo, &["log", &n, "--format=%h %s", range])
            .await;
        if !out.ok {
            return Vec::new();
        }
        out.stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// The repository's document at `from_ref` against the instance:
    /// `(instance, repository)`, what resolving with each side would change
    /// on the other. A repository without a document compares as empty.
    async fn sides(
        &self,
        state: &AppState,
        env: &GitEnv,
        repo: &Path,
        from_ref: &str,
    ) -> GitResult<(Vec<Change>, Vec<Change>)> {
        let temp = TempDir::new(self.state_dir.as_deref().unwrap_or(Path::new(".")))?;
        let mut ours = instance_document(state);
        let mut theirs = match self.extract(env, repo, from_ref, &temp.path).await {
            Ok(dir) => {
                read_document(&dir.join(DOCUMENT_NAME))
                    .map_err(GitError::failed)?
                    .0
            }
            Err(e) if e.status == StatusCode::CONFLICT => {
                crate::mocks::config_file::MocksFile::default()
            }
            Err(e) => return Err(e),
        };
        // An export leaves `instance:` out when it holds nothing, and a diff
        // looks only at the sections its second document carries: compared
        // as an empty section, so a title only one side has differs on both.
        if ours.instance.is_some() != theirs.instance.is_some() {
            ours.instance.get_or_insert_with(Default::default);
            theirs.instance.get_or_insert_with(Default::default);
        }
        let base = Some(temp.path.as_path());
        Ok((
            configdoc::diff(&theirs, &ours, base),
            configdoc::diff(&ours, &theirs, base),
        ))
    }

    /// What differs on a diverged branch: the commits of each side and what
    /// each way out would change, never resolved here.
    pub(super) async fn divergence(
        &self,
        state: &AppState,
        env: &GitEnv,
        repo: &Path,
        branch: &str,
        ahead: u64,
        behind: u64,
    ) -> Value {
        let upstream = format!("origin/{branch}");
        let mut body = json!({
            "ahead": ahead,
            "behind": behind,
            "branch": branch,
            "local_commits": self.subjects(env, repo, &format!("{upstream}..HEAD")).await,
            "remote_commits": self.subjects(env, repo, &format!("HEAD..{upstream}")).await,
            "options": RESOLVE_OPTIONS,
        });
        match self.sides(state, env, repo, &upstream).await {
            Ok((instance, repository)) => {
                // What the instance's side writes over the repository's
                // document, and what the repository's side changes here.
                body["instance"] = changes_json(&instance);
                body["repository"] = changes_json(&repository);
            }
            Err(e) => body["error"] = json!(e.message),
        }
        body
    }

    /// The preview of `resolve` with `instance`: what the commit would
    /// change in the repository's document. Nothing is written.
    pub(super) async fn preview_instance(&self, state: &AppState) -> GitResult<Value> {
        let (repo, remote) = self.ready().await?;
        let env = self.git_env(&remote)?;
        let _guard = self.lock.try_lock().map_err(|_| busy())?;
        self.need_init(&repo)?;
        let branch = self.branch();
        if !self.fetch(&env, &repo, &branch).await? {
            return Err(GitError::conflict(format!(
                "the remote has no branch {branch}: there is nothing to resolve, push"
            )));
        }
        let (ahead, behind) = self.ahead_behind(&env, &repo, &branch).await;
        let (instance, _) = self
            .sides(state, &env, &repo, &format!("origin/{branch}"))
            .await?;
        let mut body = changes_json(&instance);
        body["dry_run"] = json!(true);
        body["would_resolve"] = json!("instance");
        body["ahead"] = json!(ahead);
        body["behind"] = json!(behind);
        body["detail"] = json!(format!(
            "would commit this instance's configuration on top of the remote's {branch} ({} change(s) to its document) and push; never a forced push",
            instance.len()
        ));
        Ok(body)
    }

    /// The preview of `resolve` with `repository`: the apply's own dry run
    /// of the remote branch's document, read from the commit. The working
    /// copy does not move.
    pub(super) async fn preview_repository(&self, state: &AppState, by: &str) -> GitResult<Value> {
        let (repo, remote) = self.ready().await?;
        let env = self.git_env(&remote)?;
        let _guard = self.lock.try_lock().map_err(|_| busy())?;
        self.need_init(&repo)?;
        let branch = self.branch();
        if !self.fetch(&env, &repo, &branch).await? {
            return Err(GitError::conflict(format!(
                "the remote has no branch {branch}: there is nothing to resolve, push"
            )));
        }
        let (ahead, behind) = self.ahead_behind(&env, &repo, &branch).await;
        // The preview is refused like the resolve it previews.
        self.verify_rev(&env, &repo, &format!("origin/{branch}"), false)
            .await?;
        let report = self
            .apply_working_copy(
                state,
                &env,
                &repo,
                &format!("origin/{branch}"),
                true,
                true,
                by,
            )
            .await?;
        Ok(json!({
            "dry_run": true,
            "would_resolve": "repository",
            "ahead": ahead,
            "behind": behind,
            "detail": format!(
                "would take the remote's {branch} and apply its document with prune: {} change(s), {} problem(s)",
                report["changes"].as_array().map(Vec::len).unwrap_or(0),
                report["problems"].as_array().map(Vec::len).unwrap_or(0)
            ),
            "applied": report,
        }))
    }

    /// `resolve` with `pull_request`: the instance's side on a new remote
    /// branch, for a person to merge on the host. The instance's export is
    /// committed locally when it differs, the local branch is pushed under
    /// the new name (never forced), nothing is applied and `git.branch`
    /// stays: the branch reads as diverged until the merge is pulled.
    pub(super) async fn resolve_pull_request(
        &self,
        state: &AppState,
        target: Option<&str>,
        message: &str,
        dry_run: bool,
    ) -> GitResult<Value> {
        let (repo, remote) = self.ready().await?;
        let env = self.git_env(&remote)?;
        let base = self.branch();
        let target = match target.map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => branch_name(t)?,
            None => branch_name(&format!(
                "{base}-instance-{}",
                chrono::Utc::now().format("%Y%m%d-%H%M%S")
            ))?,
        };
        if target == base {
            return Err(GitError::new(
                StatusCode::BAD_REQUEST,
                format!("{target} is the branch this instance is on: a pull request needs a branch of its own"),
            ));
        }
        {
            let _guard = self.lock.try_lock().map_err(|_| busy())?;
            self.need_init(&repo)?;
            // The remote's side as it is now (a missing base is fine: the
            // new branch is then the first).
            let _ = self.fetch(&env, &repo, &base).await?;
            if dry_run {
                let (ahead, behind) = self.ahead_behind(&env, &repo, &base).await;
                let uncommitted = match self.drift(state, &repo) {
                    Ok((changes, _)) => changes,
                    Err(e) => return Err(GitError::failed(e)),
                };
                return Ok(json!({
                    "dry_run": true,
                    "would_resolve": "pull_request",
                    "branch": target,
                    "base": base,
                    "ahead": ahead,
                    "behind": behind,
                    "commits": self.subjects(&env, &repo, &format!("origin/{base}..HEAD")).await,
                    "uncommitted": changes_json(&uncommitted),
                    "detail": format!(
                        "would push this instance's side ({ahead} commit(s){}) to the new branch {target}; nothing applied",
                        if uncommitted.is_empty() { "" } else { " plus one for what is not committed yet" }
                    ),
                }));
            }
        }
        let message = if message.trim().is_empty() {
            "This instance's configuration, for a pull request".to_string()
        } else {
            message.to_string()
        };
        let committed = self.commit_inner(state, &message, &[]).await?;
        let _guard = self.lock.try_lock().map_err(|_| busy())?;
        if self.head(&env, &repo).await.is_none() {
            return Err(GitError::conflict(
                "nothing to put in a pull request: this instance has no commit",
            ));
        }
        let (ahead, behind) = self.ahead_behind(&env, &repo, &base).await;
        let upstream_exists = self
            .git(
                &env,
                &repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("origin/{base}"),
                ],
            )
            .await
            .ok;
        if upstream_exists && ahead == 0 {
            return Err(GitError::conflict(format!(
                "this instance has no commit the remote's {base} lacks: there is nothing to put in a pull request"
            )));
        }
        // Never forced: a remote branch of that name with other commits is
        // refused by the remote, and nothing there changes.
        let refspec = format!("HEAD:refs/heads/{target}");
        let out = self
            .git(&env, &repo, &["push", "--quiet", "origin", &refspec])
            .await;
        if !out.ok {
            let text = out.stderr.to_ascii_lowercase();
            if text.contains("non-fast-forward")
                || text.contains("fetch first")
                || text.contains("rejected")
            {
                return Err(GitError::conflict(format!(
                    "the remote already has a branch {target} with other commits: name another branch"
                )));
            }
            return Err(GitError::failed(format!("git push: {}", out.stderr)));
        }
        crate::audit::event("git_pull_request");
        crate::audit::target("branch", &target);
        Ok(json!({
            "resolved": "pull_request",
            "branch": target,
            "base": base,
            "pushed": true,
            "commit": committed,
            "head": self.head(&env, &repo).await,
            "ahead": ahead,
            "behind": behind,
            "applied": null,
            "detail": format!(
                "pushed this instance's side to {target}: open a pull request from {target} into {base} at the git host. Nothing was applied, and {base} reads as diverged here until the merge is pulled"
            ),
        }))
    }

    /// `GET /_rustybin/git/branches`: the remote's branches.
    pub async fn branches(&self) -> GitResult<Value> {
        let (repo, remote) = self.ready().await?;
        let env = self.git_env(&remote)?;
        let _guard = self.lock.try_lock().map_err(|_| busy())?;
        self.need_init(&repo)?;
        let current = self.branch();
        let mut cmd = self.command(&env, &repo, &["ls-remote", "--heads", "origin"]);
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let out = match tokio::time::timeout(GIT_TIMEOUT, cmd.output()).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(GitError::failed(format!("git could not be started: {e}"))),
            Err(_) => {
                return Err(GitError::failed(format!(
                    "git ls-remote did not finish within {} s",
                    GIT_TIMEOUT.as_secs()
                )))
            }
        };
        if !out.status.success() {
            return Err(GitError::failed(format!(
                "git ls-remote: {}",
                self.scrub(&out.stderr)
            )));
        }
        let cut = out.stdout.len() > MAX_LS_REMOTE;
        let text = String::from_utf8_lossy(&out.stdout[..out.stdout.len().min(MAX_LS_REMOTE)])
            .into_owned();
        let mut names: BTreeMap<String, String> = BTreeMap::new();
        let mut unlisted = 0usize;
        let mut lines: Vec<&str> = text.lines().collect();
        if cut {
            // The last line may be half a line.
            lines.pop();
        }
        for line in lines {
            let Some((sha, name)) = line.split_once('\t') else {
                continue;
            };
            let Some(name) = name.trim().strip_prefix("refs/heads/") else {
                continue;
            };
            // A name this instance could not be set to is not offered.
            if check_branch(&json!(name)).is_err()
                || !sha.trim().bytes().all(|b| b.is_ascii_hexdigit())
            {
                unlisted += 1;
                continue;
            }
            names.insert(name.to_string(), sha.trim().to_string());
        }
        let total = names.len();
        let on_remote = names.contains_key(&current);
        let branches: Vec<Value> = names
            .into_iter()
            .take(MAX_BRANCHES)
            .map(|(name, sha)| {
                json!({
                    "current": name == current,
                    "short": sha.chars().take(7).collect::<String>(),
                    "sha": sha,
                    "name": name,
                })
            })
            .collect();
        Ok(json!({
            "branch": current,
            "on_remote": on_remote,
            "branches": branches,
            "truncated": cut || total > MAX_BRANCHES,
            "unlisted": unlisted,
        }))
    }

    /// Store `git.branch` (the stored value wins over the variable's seed).
    fn store_branch(&self, branch: &str, by: &str) -> GitResult<()> {
        let mut values = BTreeMap::new();
        values.insert(BRANCH_KEY.to_string(), json!(branch));
        self.settings
            .update(values, BTreeMap::new(), by)
            .map(|_| ())
            .map_err(|e| settings_error(BRANCH_KEY, e))
    }

    /// Put `git.branch` back to what it was before a switch that failed.
    fn restore_branch(&self, before: &(Value, crate::settings::Source), by: &str) {
        let result = if before.1 == crate::settings::Source::Stored {
            let mut values = BTreeMap::new();
            values.insert(BRANCH_KEY.to_string(), before.0.clone());
            self.settings
                .update(values, BTreeMap::new(), by)
                .map(|_| ())
        } else {
            self.settings.unset(BRANCH_KEY, by).map(|_| ())
        };
        if let Err(e) = result {
            tracing::warn!(target: "rustybin::git", "git.branch could not be put back after a failed switch: {e:?}");
        }
    }

    /// `POST /_rustybin/git/branch`: switch to a branch of the remote (with
    /// a preview), or start a new one from the instance.
    pub async fn switch_branch(
        &self,
        state: &AppState,
        req: &BranchRequest,
        by: &str,
    ) -> GitResult<Value> {
        let started = Instant::now();
        let result = self.switch_branch_inner(state, req, by).await;
        if !req.dry_run {
            self.record(
                if req.create { "branch" } else { "switch" },
                by,
                started,
                &result
                    .as_ref()
                    .map(|v| v["detail"].as_str().unwrap_or("").to_string())
                    .map_err(|e| e.message.clone()),
            );
        }
        result
    }

    async fn switch_branch_inner(
        &self,
        state: &AppState,
        req: &BranchRequest,
        by: &str,
    ) -> GitResult<Value> {
        let target = branch_name(&req.branch)?;
        let current = self.branch();
        if target == current {
            return Err(GitError::conflict(format!(
                "this instance is already on {target}: pull to take what the remote has"
            )));
        }
        let (repo, remote) = self.ready().await?;
        let env = self.git_env(&remote)?;
        let upstream = format!("origin/{target}");
        {
            let _guard = self.lock.try_lock().map_err(|_| busy())?;
            self.need_init(&repo)?;
            let exists = self.fetch(&env, &repo, &target).await?;
            if req.create && exists {
                return Err(GitError::conflict(format!(
                    "the remote already has a branch {target}: switch to it (create: false)"
                )));
            }
            if !req.create && !exists {
                return Err(GitError::conflict(format!(
                    "the remote has no branch {target}: start it from this instance (create: true)"
                )));
            }
            if req.dry_run && req.create {
                let changes = match self.drift(state, &repo) {
                    Ok((changes, _)) => changes,
                    Err(e) => return Err(GitError::failed(e)),
                };
                let mut body = changes_json(&changes);
                body["dry_run"] = json!(true);
                body["would_create"] = json!(target);
                body["from"] = json!(current);
                body["detail"] = json!(format!(
                    "would start {target} from this instance at {current}'s local head, commit {} change(s) and push; {current} stays as it is",
                    changes.len()
                ));
                return Ok(body);
            }
            if req.dry_run {
                // The same read and the same checks as the switch's apply.
                self.verify_rev(&env, &repo, &upstream, false).await?;
                let report = self
                    .apply_working_copy(state, &env, &repo, &upstream, true, req.prune, by)
                    .await?;
                return Ok(json!({
                    "dry_run": true,
                    "branch": target,
                    "from": current,
                    "detail": format!(
                        "would switch from {current} to {target} and apply its document: {} change(s), {} problem(s)",
                        report["changes"].as_array().map(Vec::len).unwrap_or(0),
                        report["problems"].as_array().map(Vec::len).unwrap_or(0)
                    ),
                    "applied": report,
                }));
            }
        }
        if !self.settings.writable(BRANCH_KEY) {
            return Err(GitError::conflict(
                "git.branch is locked by RUSTYBIN_SETTINGS_LOCK: change RUSTYBIN_CONFIG_GIT_REF and restart",
            ));
        }
        let before = self.settings.get(BRANCH_KEY);
        self.store_branch(&target, by)?;
        let result = if req.create {
            self.start_branch(state, &env, &repo, &target, &current, &req.message)
                .await
        } else {
            self.take_branch(state, &env, &repo, &target, &current, req, by)
                .await
        };
        match result {
            Ok(v) => {
                crate::audit::event(if req.create {
                    "git_branch_start"
                } else {
                    "git_branch_switch"
                });
                crate::audit::target("branch", &target);
                Ok(v)
            }
            Err(mut e) => {
                // The setting and the working copy go back together.
                self.restore_branch(&before, by);
                if let Ok(_guard) = self.lock.try_lock() {
                    let _ = self
                        .git(&env, &repo, &["checkout", "--quiet", "--force", &current])
                        .await;
                }
                e.message = format!("{} (still on {current})", e.message);
                Err(e)
            }
        }
    }

    /// The switch itself: the working copy on the remote's branch, then its
    /// document applied through the path a pull uses.
    #[allow(clippy::too_many_arguments)]
    async fn take_branch(
        &self,
        state: &AppState,
        env: &GitEnv,
        repo: &Path,
        target: &str,
        current: &str,
        req: &BranchRequest,
        by: &str,
    ) -> GitResult<Value> {
        let upstream = format!("origin/{target}");
        let _guard = self.lock.try_lock().map_err(|_| busy())?;
        // With git.verify on, the branch passes before the working copy
        // moves (also when its document is not applied).
        self.verify_rev(env, repo, &upstream, true).await?;
        // The document is validated before the working copy moves: a branch
        // whose document cannot be read is not switched to.
        if req.apply {
            let check = self
                .apply_working_copy(state, env, repo, &upstream, true, req.prune, by)
                .await?;
            let _ = check;
        }
        self.must(
            env,
            repo,
            &["checkout", "--quiet", "--force", "-B", target, &upstream],
        )
        .await?;
        self.synced(target);
        let applied = if req.apply {
            Some(
                self.apply_working_copy(state, env, repo, "HEAD", false, req.prune, by)
                    .await?,
            )
        } else {
            None
        };
        let detail = match &applied {
            Some(r) => format!(
                "switched from {current} to {target}; applied {} change(s){}",
                r["changes"].as_array().map(Vec::len).unwrap_or(0),
                match r["problems"].as_array().map(Vec::len).unwrap_or(0) {
                    0 => String::new(),
                    n => format!(", {n} problem(s)"),
                }
            ),
            None => format!("switched from {current} to {target}; its document was not applied"),
        };
        Ok(json!({
            "switched": true,
            "branch": target,
            "from": current,
            "head": self.head(env, repo).await,
            "applied": applied,
            "detail": detail,
        }))
    }

    /// A new branch from the instance: created at the local head, the
    /// instance's export committed, pushed.
    async fn start_branch(
        &self,
        state: &AppState,
        env: &GitEnv,
        repo: &Path,
        target: &str,
        current: &str,
        message: &str,
    ) -> GitResult<Value> {
        {
            let _guard = self.lock.try_lock().map_err(|_| busy())?;
            if self.head(env, repo).await.is_some() {
                self.must(env, repo, &["checkout", "--quiet", "-B", target])
                    .await?;
            } else {
                // No commit yet: the unborn branch takes the new name.
                self.must(
                    env,
                    repo,
                    &["symbolic-ref", "HEAD", &format!("refs/heads/{target}")],
                )
                .await?;
            }
        }
        let message = if message.trim().is_empty() {
            format!("Start {target} from this instance")
        } else {
            message.to_string()
        };
        let committed = self.commit_inner(state, &message, &[]).await?;
        let pushed = self.push_inner().await?;
        Ok(json!({
            "created": true,
            "branch": target,
            "from": current,
            "commit": committed,
            "push": pushed,
            "head": pushed["head"],
            "detail": format!("started {target} from this instance (was on {current}) and pushed it"),
        }))
    }
}
