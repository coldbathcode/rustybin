//! Source control (step 09 of `docs/plans/next/master-plan.md`): the
//! instance's configuration in a git repository.
//!
//! Every operation runs the `git` binary as a subprocess (with a timeout,
//! `GIT_TERMINAL_PROMPT=0`, no global or system configuration) in a working
//! copy at `<state>/repo`, so `RUSTYBIN_STATE_DIR` is required. The remote
//! comes from the `git.*` settings (seeded by `RUSTYBIN_CONFIG_GIT*`); its
//! host must pass the egress policy under `Purpose::Git`, where it is
//! allowed once configured. Authentication is a token for `https://` remotes
//! (handed to git through `GIT_ASKPASS`, never on a command line or in a
//! URL) or a deploy key for `ssh://` remotes (ed25519, generated with
//! `ssh-keygen` into `<state>/repo.key`; the public key is shown to add to
//! the repository). A local path or `file://` remote is for tests and
//! demos and needs the loopback allowance of the egress policy.
//!
//! Commit exports the instance as a bundle into `<repo>/<git.path>/`
//! (`rustybin.yaml` plus `specs/`), push and pull move it; pull applies the
//! repository's document through `crate::configdoc` (a dry run shows the
//! diff first). The instance never merges: a pull that is not a fast
//! forward reports the conflict and the two ways out (the instance's copy
//! wins, or the repository's). `git.auto` keeps the two in sync: push every
//! change (checked every 30 s) or pull at an interval. At startup
//! `RUSTYBIN_CONFIG_GIT` clones and applies before the instance serves.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde::Serialize;
use serde_json::{json, Value};

use crate::config::Config;
use crate::configdoc::{self, ApplyOptions, Change, SecretsMode, DOCUMENT_NAME};
use crate::egress::{Allowlist, Egress, Purpose, Rule};
use crate::settings::{Kind, Settings, Spec};
use crate::state::AppState;

pub mod api;
#[cfg(test)]
mod tests;

/// Most operations kept in the timeline.
pub const MAX_TIMELINE: usize = 50;
/// Most commits the history shows.
pub const MAX_HISTORY: usize = 50;
/// How long one git command may take.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// How often the automatic mode looks for changes.
pub const AUTO_TICK: Duration = Duration::from_secs(30);
/// Longest commit message.
pub const MAX_MESSAGE: usize = 2000;
/// Longest output kept from a git command.
const MAX_OUTPUT: usize = 8 * 1024;

pub const REPO_DIR: &str = "repo";
pub const KEY_FILE: &str = "repo.key";
pub const ASKPASS_FILE: &str = "repo.askpass";
pub const KNOWN_HOSTS_FILE: &str = "repo.known_hosts";

pub const REMOTE_KEY: &str = "git.remote";
pub const BRANCH_KEY: &str = "git.branch";
pub const PATH_KEY: &str = "git.path";
pub const AUTH_KEY: &str = "git.auth";
pub const USERNAME_KEY: &str = "git.username";
pub const TOKEN_KEY: &str = "git.token";
pub const AUTHOR_KEY: &str = "git.author";
pub const AUTO_KEY: &str = "git.auto";
pub const PULL_EVERY_KEY: &str = "git.pull_every";

pub const AUTH_OPTIONS: &[&str] = &["token", "deploy_key"];
pub const AUTO_OPTIONS: &[&str] = &["off", "push_on_change", "pull"];
pub const DEFAULT_AUTHOR: &str = "Rustybin <rustybin@example.invalid>";

// ── Settings ────────────────────────────────────────────────────────

pub fn specs() -> Vec<Spec> {
    vec![
        Spec::new(
            REMOTE_KEY,
            "git",
            "Repository",
            "The remote: https://host/org/repo.git (with a token), ssh://git@host/org/repo.git or git@host:org/repo.git (with the deploy key). Its host is allowed by the outbound policy once set.",
            Kind::Text { max_len: 512 },
            Value::Null,
        )
        .env("RUSTYBIN_CONFIG_GIT", |c| c.config_git.clone().map(Value::String))
        .check(check_remote),
        Spec::new(
            BRANCH_KEY,
            "git",
            "Branch",
            "The branch the instance commits to and pulls from.",
            Kind::Text { max_len: 128 },
            json!("main"),
        )
        .env("RUSTYBIN_CONFIG_GIT_REF", |c| {
            c.config_git_ref.clone().map(Value::String)
        })
        .check(check_branch),
        Spec::new(
            PATH_KEY,
            "git",
            "Directory in the repository",
            "Where rustybin.yaml and specs/ live in the repository (empty for its root).",
            Kind::Text { max_len: 256 },
            json!("rustybin"),
        )
        .env("RUSTYBIN_CONFIG_GIT_PATH", |c| {
            c.config_git_path.clone().map(Value::String)
        })
        .check(check_path),
        Spec::new(
            AUTH_KEY,
            "git",
            "Authentication",
            "token: a personal access or deploy token for an https remote; deploy_key: the ed25519 key this instance generates, for an ssh remote.",
            Kind::Choice {
                options: AUTH_OPTIONS,
            },
            json!("token"),
        )
        .env("RUSTYBIN_CONFIG_GIT_AUTH", |c| {
            c.config_git_auth.clone().map(Value::String)
        }),
        Spec::new(
            USERNAME_KEY,
            "git",
            "Token user name",
            "The user name sent with the token (GitHub accepts any, GitLab deploy tokens have their own, Bitbucket wants the account).",
            Kind::Text { max_len: 128 },
            json!("x-access-token"),
        ),
        Spec::new(
            TOKEN_KEY,
            "git",
            "Token",
            "The token of an https remote. Never shown again; the instance hands it to git through GIT_ASKPASS, never on a command line.",
            Kind::Text { max_len: 512 },
            Value::Null,
        )
        .env("RUSTYBIN_CONFIG_GIT_TOKEN", |c| {
            c.config_git_token.clone().map(Value::String)
        })
        .secret(),
        Spec::new(
            AUTHOR_KEY,
            "git",
            "Commit author",
            "Name <email> of the commits this instance makes.",
            Kind::Text { max_len: 200 },
            json!(DEFAULT_AUTHOR),
        )
        .check(check_author),
        Spec::new(
            AUTO_KEY,
            "git",
            "Automatic mode",
            "off: commit, push and pull by hand; push_on_change: every change to the instance is committed and pushed (checked every 30 seconds); pull: the repository is pulled and applied (with prune) at the interval below.",
            Kind::Choice {
                options: AUTO_OPTIONS,
            },
            json!("off"),
        )
        .env("RUSTYBIN_CONFIG_GIT_AUTO", |c| {
            c.config_git_auto.clone().map(Value::String)
        }),
        Spec::new(
            PULL_EVERY_KEY,
            "git",
            "Pull interval",
            "How often the pull mode pulls: 1m to 24h (5m by default).",
            Kind::Text { max_len: 16 },
            json!("5m"),
        )
        .check(check_interval),
    ]
}

fn check_remote(v: &Value) -> Result<(), String> {
    match v.as_str().map(str::trim) {
        None | Some("") => Ok(()),
        Some(s) => parse_remote(s).map(|_| ()),
    }
}

fn check_branch(v: &Value) -> Result<(), String> {
    let s = v.as_str().unwrap_or("").trim();
    if s.is_empty() || s.len() > 128 {
        return Err("a branch name is 1 to 128 characters".into());
    }
    if s.starts_with('-')
        || s.starts_with('/')
        || s.ends_with('/')
        || s.ends_with('.')
        || s.contains("..")
        || s.contains("//")
        || s.contains("@{")
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
    {
        return Err("a branch name: letters, digits, . _ - and /, not starting with - or /".into());
    }
    Ok(())
}

fn check_path(v: &Value) -> Result<(), String> {
    let s = v.as_str().unwrap_or("");
    repo_path(s).map(|_| ())
}

/// The directory in the repository, normalised (`None` for the root).
pub fn repo_path(raw: &str) -> Result<Option<String>, String> {
    let s = raw.trim().trim_matches('/');
    if s.is_empty() || s == "." {
        return Ok(None);
    }
    if s.len() > 256 {
        return Err("the directory is longer than 256 characters".into());
    }
    for part in s.split('/') {
        if part.is_empty() || part == "." || part == ".." || part == ".git" {
            return Err("the directory: no empty, . or .. parts, never .git".into());
        }
        if !part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err("the directory: letters, digits, . _ - and / only".into());
        }
    }
    Ok(Some(s.to_string()))
}

fn check_author(v: &Value) -> Result<(), String> {
    author_parts(v.as_str().unwrap_or("")).map(|_| ())
}

/// `Name <email>` taken apart.
fn author_parts(raw: &str) -> Result<(String, String), String> {
    let s = raw.trim();
    let (name, rest) = s
        .split_once('<')
        .ok_or("the author: Name <email@example.com>")?;
    let email = rest
        .strip_suffix('>')
        .ok_or("the author: Name <email@example.com>")?;
    let name = name.trim();
    let email = email.trim();
    if name.is_empty()
        || email.is_empty()
        || !email.contains('@')
        || s.chars().any(|c| c.is_control())
        || name.contains('<')
        || email.contains(['<', '>', ' '])
    {
        return Err("the author: Name <email@example.com>".into());
    }
    Ok((name.to_string(), email.to_string()))
}

fn check_interval(v: &Value) -> Result<(), String> {
    parse_interval(v.as_str().unwrap_or("")).map(|_| ())
}

/// `30s`, `5m`, `1h`: 1 minute to 24 hours.
pub fn parse_interval(raw: &str) -> Result<Duration, String> {
    let s = raw.trim();
    let (num, unit) = s.split_at(s.trim_end_matches(|c: char| c.is_ascii_alphabetic()).len());
    let n: u64 = num
        .trim()
        .parse()
        .map_err(|_| "the interval: a number and s, m or h (5m)".to_string())?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        _ => return Err("the interval: a number and s, m or h (5m)".into()),
    };
    if !(60..=86_400).contains(&secs) {
        return Err("the interval: between 1m and 24h".into());
    }
    Ok(Duration::from_secs(secs))
}

// ── The remote ──────────────────────────────────────────────────────

/// What a remote reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteKind {
    Https,
    Ssh,
    /// A path or `file://` URL on this machine (tests and demos).
    Local,
}

/// A parsed remote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Remote {
    pub url: String,
    pub kind: RemoteKind,
    /// The host of a network remote.
    pub host: Option<String>,
    pub port: u16,
}

/// Take a remote apart: `https://host/...`, `ssh://[user@]host[:port]/...`,
/// `[user@]host:path` (scp style), `file:///path` or `/path`.
pub fn parse_remote(raw: &str) -> Result<Remote, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("the remote is empty".into());
    }
    if s.len() > 512 {
        return Err("the remote is longer than 512 characters".into());
    }
    if s.chars().any(|c| c.is_control() || c.is_whitespace()) || s.starts_with('-') {
        return Err("the remote: no spaces or control characters, not starting with -".into());
    }
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("http://") {
        return Err("the remote: use https:// (or an ssh remote)".into());
    }
    if lower.starts_with("https://") || lower.starts_with("ssh://") {
        let parsed = reqwest::Url::parse(s).map_err(|e| format!("the remote: {e}"))?;
        let tls = parsed.scheme().eq_ignore_ascii_case("https");
        if tls && (!parsed.username().is_empty() || parsed.password().is_some()) {
            return Err("the remote must not carry credentials: set the token (git.token)".into());
        }
        if parsed.password().is_some() {
            return Err("the remote must not carry a password".into());
        }
        let host = parsed
            .host_str()
            .ok_or("the remote has no host")?
            .trim_matches(['[', ']'])
            .to_ascii_lowercase();
        check_host(&host)?;
        let port = parsed.port().unwrap_or(if tls { 443 } else { 22 });
        return Ok(Remote {
            url: s.to_string(),
            kind: if tls {
                RemoteKind::Https
            } else {
                RemoteKind::Ssh
            },
            host: Some(host),
            port,
        });
    }
    if let Some(path) = lower.strip_prefix("file://") {
        if !path.starts_with('/') {
            return Err("a file:// remote needs an absolute path (file:///path)".into());
        }
        return Ok(Remote {
            url: s.to_string(),
            kind: RemoteKind::Local,
            host: None,
            port: 0,
        });
    }
    if s.starts_with('/') {
        return Ok(Remote {
            url: s.to_string(),
            kind: RemoteKind::Local,
            host: None,
            port: 0,
        });
    }
    if s.contains("://") {
        return Err("the remote: https://, ssh://, file:// or git@host:path".into());
    }
    // scp style: [user@]host:path, the host before the first colon.
    let (head, path) = s
        .split_once(':')
        .ok_or("the remote: https://host/..., ssh://host/..., git@host:path or a path")?;
    if head.contains('/') || path.is_empty() {
        return Err("the remote: https://host/..., ssh://host/..., git@host:path or a path".into());
    }
    let host = head.rsplit('@').next().unwrap_or(head).to_ascii_lowercase();
    check_host(&host)?;
    Ok(Remote {
        url: s.to_string(),
        kind: RemoteKind::Ssh,
        host: Some(host),
        port: 22,
    })
}

fn check_host(host: &str) -> Result<(), String> {
    if host.is_empty() {
        return Err("the remote has no host".into());
    }
    if host.parse::<IpAddr>().is_ok() || crate::egress::valid_hostname(host) {
        Ok(())
    } else {
        Err(format!("{host:?} is not a host name"))
    }
}

// ── The state ───────────────────────────────────────────────────────

/// One operation of the timeline.
#[derive(Clone, Debug, Serialize)]
pub struct Op {
    pub at: String,
    pub op: String,
    pub by: String,
    pub ok: bool,
    pub detail: String,
    pub ms: u64,
}

/// Why an operation was refused or failed.
#[derive(Clone, Debug)]
pub struct GitError {
    pub status: StatusCode,
    pub message: String,
    /// Extra fields of the answer (a conflict's ahead and behind).
    pub extra: Value,
}

impl GitError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            extra: Value::Null,
        }
    }

    /// Not configured, not initialised, busy: the caller has to act first.
    fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    /// git itself failed (the remote, the network).
    fn failed(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, message)
    }

    pub fn body(&self) -> Value {
        let mut body = json!({ "error": self.message });
        if let (Value::Object(b), Value::Object(e)) = (&mut body, &self.extra) {
            for (k, v) in e {
                b.insert(k.clone(), v.clone());
            }
        }
        body
    }
}

type GitResult<T> = Result<T, GitError>;

/// A git command's outcome.
#[derive(Clone, Debug, Default)]
struct Output {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// What a git command runs with.
struct GitEnv {
    vars: Vec<(String, String)>,
    /// `-c key=value` pairs.
    config: Vec<String>,
}

/// The source control state (`AppState.git`).
pub struct SourceControl {
    settings: Arc<Settings>,
    egress: Arc<Egress>,
    state_dir: Option<PathBuf>,
    instance_id: String,
    lock: tokio::sync::Mutex<()>,
    timeline: Mutex<VecDeque<Op>>,
    last_pull: Mutex<Option<Instant>>,
    git_version: OnceLock<Option<String>>,
    /// The working copy's document could not be read on the last auto
    /// tick: logged once.
    auto_warned: Mutex<Option<String>>,
}

impl SourceControl {
    pub fn new(config: &Config, settings: Arc<Settings>, egress: Arc<Egress>) -> Self {
        Self {
            settings,
            egress,
            state_dir: config.state_dir.clone(),
            instance_id: config.instance_id.clone(),
            lock: tokio::sync::Mutex::new(()),
            timeline: Mutex::new(VecDeque::new()),
            last_pull: Mutex::new(None),
            git_version: OnceLock::new(),
            auto_warned: Mutex::new(None),
        }
    }

    /// The working copy (`<state>/repo`).
    pub fn repo_dir(&self) -> Option<PathBuf> {
        self.state_dir.as_ref().map(|d| d.join(REPO_DIR))
    }

    fn state_file(&self, name: &str) -> Option<PathBuf> {
        self.state_dir.as_ref().map(|d| d.join(name))
    }

    /// `git --version`, once; `None` when git is not installed.
    pub async fn git_version(&self) -> Option<String> {
        if let Some(v) = self.git_version.get() {
            return v.clone();
        }
        let found = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new("git")
                .arg("--version")
                .env("GIT_TERMINAL_PROMPT", "0")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let _ = self.git_version.set(found.clone());
        found
    }

    fn timeline(&self) -> Vec<Op> {
        self.timeline
            .lock()
            .map(|t| t.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn record(&self, op: &str, by: &str, started: Instant, result: &Result<String, String>) {
        let (ok, detail) = match result {
            Ok(d) => (true, d.clone()),
            Err(e) => (false, e.clone()),
        };
        let entry = Op {
            at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            op: op.to_string(),
            by: by.to_string(),
            ok,
            detail: truncate(&detail, 500),
            ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        };
        if ok {
            tracing::info!(target: "rustybin::git", op, by, detail = %entry.detail, "git operation");
        } else {
            tracing::warn!(target: "rustybin::git", op, by, detail = %entry.detail, "git operation failed");
        }
        if let Ok(mut t) = self.timeline.lock() {
            t.push_front(entry);
            while t.len() > MAX_TIMELINE {
                t.pop_back();
            }
        }
    }

    // ── Configuration ───────────────────────────────────────────────

    /// The configured remote, parsed.
    pub fn remote(&self) -> Result<Option<Remote>, String> {
        match self.settings.string(REMOTE_KEY) {
            None => Ok(None),
            Some(s) => parse_remote(&s).map(Some),
        }
    }

    pub fn branch(&self) -> String {
        self.settings
            .string(BRANCH_KEY)
            .unwrap_or_else(|| "main".into())
    }

    /// The directory in the repository (`None`: the root).
    pub fn path(&self) -> Option<String> {
        repo_path(&self.settings.string(PATH_KEY).unwrap_or_default()).unwrap_or(None)
    }

    /// Where the document lives in the working copy.
    fn document_dir(&self, repo: &Path) -> PathBuf {
        match self.path() {
            Some(p) => repo.join(p),
            None => repo.to_path_buf(),
        }
    }

    fn auth(&self) -> String {
        self.settings
            .string(AUTH_KEY)
            .unwrap_or_else(|| "token".into())
    }

    /// Allow the remote's host under `Purpose::Git` and check it resolves
    /// to an address the policy accepts.
    async fn permit(&self, remote: &Remote) -> Result<(), String> {
        match remote.kind {
            RemoteKind::Local => {
                if self.egress.loopback_allowed() {
                    Ok(())
                } else {
                    Err("a local repository (a path or file:// remote) needs RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true or the egress.allow_loopback setting".into())
                }
            }
            RemoteKind::Https | RemoteKind::Ssh => {
                let host = remote.host.clone().unwrap_or_default();
                let entry = if host.parse::<std::net::Ipv6Addr>().is_ok() {
                    format!("[{host}]:{}", remote.port)
                } else {
                    format!("{host}:{}", remote.port)
                };
                let (allow, _) = Allowlist::parse(&[entry]);
                self.egress.permit(
                    Purpose::Git,
                    Rule {
                        allow,
                        any: false,
                        loopback: false,
                    },
                );
                let target = crate::egress::ParsedTarget {
                    scheme: if remote.kind == RemoteKind::Https {
                        "https".into()
                    } else {
                        "ssh".into()
                    },
                    host: host.clone(),
                    ip: host.parse().ok(),
                    port: remote.port,
                    path: String::new(),
                };
                let (global, _) = self.egress.allowlist();
                let merged = global.merged(&self.egress.rule(Purpose::Git).allow);
                let addrs =
                    crate::egress::pin_with(&target, Some(&merged), "RUSTYBIN_EGRESS_ALLOW")
                        .await
                        .map_err(|e| format!("{e} (purpose git)"))?;
                if !self.egress.loopback_allowed() {
                    if let Some(lo) = addrs.iter().find(|a| a.ip().is_loopback()) {
                        return Err(format!(
                            "{host} resolves to the loopback address {}: set RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true (or the egress.allow_loopback setting) for a local remote",
                            lo.ip()
                        ));
                    }
                }
                Ok(())
            }
        }
    }

    /// Everything an operation needs: git installed, a state directory, a
    /// remote that passes the policy. Returns the working copy and remote.
    async fn ready(&self) -> GitResult<(PathBuf, Remote)> {
        if self.git_version().await.is_none() {
            return Err(GitError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "git is not installed on this instance (the container image includes it; elsewhere put git on the PATH)",
            ));
        }
        let Some(repo) = self.repo_dir() else {
            return Err(GitError::conflict(
                "source control needs RUSTYBIN_STATE_DIR: the working copy lives in <state>/repo",
            ));
        };
        let remote = match self.remote() {
            Ok(Some(r)) => r,
            Ok(None) => {
                return Err(GitError::conflict(
                    "no repository is configured: set git.remote (Management, Source control) or RUSTYBIN_CONFIG_GIT",
                ))
            }
            Err(e) => return Err(GitError::conflict(format!("git.remote: {e}"))),
        };
        self.permit(&remote).await.map_err(GitError::conflict)?;
        Ok((repo, remote))
    }

    /// The environment and configuration of a git command.
    fn git_env(&self, remote: &Remote) -> GitResult<GitEnv> {
        let state = self.state_dir.clone().unwrap_or_else(|| PathBuf::from("."));
        let mut vars = vec![
            ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
            ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
            ("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string()),
            ("LC_ALL".to_string(), "C".to_string()),
            ("HOME".to_string(), state.display().to_string()),
        ];
        let (name, email) = author_parts(
            &self
                .settings
                .string(AUTHOR_KEY)
                .unwrap_or_else(|| DEFAULT_AUTHOR.into()),
        )
        .unwrap_or_else(|_| ("Rustybin".into(), "rustybin@example.invalid".into()));
        let mut config = vec![
            format!("user.name={name}"),
            format!("user.email={email}"),
            "credential.helper=".to_string(),
            "core.hooksPath=/dev/null".to_string(),
        ];
        match (remote.kind, self.auth().as_str()) {
            (RemoteKind::Https, "deploy_key") => {
                return Err(GitError::conflict(
                    "a deploy key works with an ssh remote: use an https remote with a token, or an ssh:// remote",
                ))
            }
            (RemoteKind::Ssh, "token") => {
                return Err(GitError::conflict(
                    "a token works with an https remote: set git.auth to deploy_key for an ssh remote",
                ))
            }
            (RemoteKind::Https, _) => {
                if let Some(token) = self.settings.secret(TOKEN_KEY) {
                    let askpass = self.askpass_script()?;
                    vars.push(("GIT_ASKPASS".into(), askpass.display().to_string()));
                    vars.push((
                        "RUSTYBIN_GIT_USERNAME".into(),
                        self.settings
                            .string(USERNAME_KEY)
                            .unwrap_or_else(|| "x-access-token".into()),
                    ));
                    vars.push(("RUSTYBIN_GIT_PASSWORD".into(), token.to_string()));
                }
                config.push("http.followRedirects=initial".to_string());
            }
            (RemoteKind::Ssh, _) => {
                let key = self.state_file(KEY_FILE).unwrap_or_default();
                if !key.exists() {
                    return Err(GitError::conflict(
                        "no deploy key yet: generate one (POST /_rustybin/git/key, or Management, Source control) and add its public key to the repository",
                    ));
                }
                let known = self.state_file(KNOWN_HOSTS_FILE).unwrap_or_default();
                vars.push((
                    "GIT_SSH_COMMAND".into(),
                    format!(
                        "ssh -i {} -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile={}",
                        shell_quote(&key.display().to_string()),
                        shell_quote(&known.display().to_string())
                    ),
                ));
            }
            (RemoteKind::Local, _) => {}
        }
        Ok(GitEnv { vars, config })
    }

    /// The script git asks for credentials through (written once).
    fn askpass_script(&self) -> GitResult<PathBuf> {
        let path = self
            .state_file(ASKPASS_FILE)
            .ok_or_else(|| GitError::conflict("RUSTYBIN_STATE_DIR is required"))?;
        let script = "#!/bin/sh\ncase \"$1\" in\n  Username*) printf '%s\\n' \"$RUSTYBIN_GIT_USERNAME\" ;;\n  *) printf '%s\\n' \"$RUSTYBIN_GIT_PASSWORD\" ;;\nesac\n";
        let current = std::fs::read_to_string(&path).ok();
        if current.as_deref() != Some(script) {
            crate::settings::write_private(&path, script.as_bytes())
                .map_err(|e| GitError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| GitError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
        Ok(path)
    }

    /// Run one git command in `cwd`.
    async fn git(&self, env: &GitEnv, cwd: &Path, args: &[&str]) -> Output {
        let mut cmd = tokio::process::Command::new("git");
        for c in &env.config {
            cmd.arg("-c").arg(c);
        }
        cmd.args(args).current_dir(cwd).kill_on_drop(true);
        for (k, v) in &env.vars {
            cmd.env(k, v);
        }
        cmd.stdin(std::process::Stdio::null());
        let token = self.settings.secret(TOKEN_KEY);
        let scrub = |s: &[u8]| -> String {
            let mut text = String::from_utf8_lossy(s).to_string();
            if let Some(t) = &token {
                if t.len() >= 8 {
                    text = text.replace(t.as_str(), "***");
                }
            }
            truncate(text.trim(), MAX_OUTPUT)
        };
        match tokio::time::timeout(GIT_TIMEOUT, cmd.output()).await {
            Ok(Ok(out)) => Output {
                ok: out.status.success(),
                stdout: scrub(&out.stdout),
                stderr: scrub(&out.stderr),
            },
            Ok(Err(e)) => Output {
                ok: false,
                stdout: String::new(),
                stderr: format!("git could not be started: {e}"),
            },
            Err(_) => Output {
                ok: false,
                stdout: String::new(),
                stderr: format!(
                    "git {} did not finish within {} s",
                    args.first().unwrap_or(&""),
                    GIT_TIMEOUT.as_secs()
                ),
            },
        }
    }

    /// A command that must succeed.
    async fn must(&self, env: &GitEnv, cwd: &Path, args: &[&str]) -> GitResult<String> {
        let out = self.git(env, cwd, args).await;
        if out.ok {
            Ok(out.stdout)
        } else {
            Err(GitError::failed(format!(
                "git {}: {}",
                args.first().unwrap_or(&""),
                if out.stderr.is_empty() {
                    out.stdout
                } else {
                    out.stderr
                }
            )))
        }
    }

    fn initialised(&self, repo: &Path) -> bool {
        repo.join(".git").is_dir()
    }

    async fn head(&self, env: &GitEnv, repo: &Path) -> Option<Value> {
        let out = self
            .git(
                env,
                repo,
                &["log", "-1", "--format=%H%x1f%h%x1f%an%x1f%aI%x1f%s"],
            )
            .await;
        if !out.ok {
            return None;
        }
        let mut parts = out.stdout.split('\u{1f}');
        Some(json!({
            "sha": parts.next().unwrap_or(""),
            "short": parts.next().unwrap_or(""),
            "author": parts.next().unwrap_or(""),
            "at": parts.next().unwrap_or(""),
            "subject": parts.next().unwrap_or(""),
        }))
    }

    /// `git fetch origin <branch>`: whether the remote branch exists.
    async fn fetch(&self, env: &GitEnv, repo: &Path, branch: &str) -> GitResult<bool> {
        let out = self
            .git(env, repo, &["fetch", "--quiet", "origin", branch])
            .await;
        if out.ok {
            return Ok(true);
        }
        let text = format!("{}\n{}", out.stdout, out.stderr).to_ascii_lowercase();
        if text.contains("couldn't find remote ref") || text.contains("could not find remote ref") {
            return Ok(false);
        }
        Err(GitError::failed(format!("git fetch: {}", out.stderr)))
    }

    /// (ahead, behind) of HEAD against `origin/<branch>`; ahead is 0
    /// without a HEAD.
    async fn ahead_behind(&self, env: &GitEnv, repo: &Path, branch: &str) -> (u64, u64) {
        let upstream = format!("origin/{branch}");
        if self.head(env, repo).await.is_none() {
            let out = self
                .git(env, repo, &["rev-list", "--count", &upstream])
                .await;
            return (0, out.stdout.trim().parse().unwrap_or(0));
        }
        let range = format!("HEAD...{upstream}");
        let out = self
            .git(env, repo, &["rev-list", "--left-right", "--count", &range])
            .await;
        let mut parts = out.stdout.split_whitespace();
        (
            parts.next().and_then(|s| s.parse().ok()).unwrap_or(0),
            parts.next().and_then(|s| s.parse().ok()).unwrap_or(0),
        )
    }

    // ── Operations ──────────────────────────────────────────────────

    /// `GET /_rustybin/git`: the configuration, the working copy, the
    /// drift and the timeline. `fetch` also asks the remote for ahead and
    /// behind.
    pub async fn status(&self, state: &AppState, fetch: bool) -> Value {
        let git = self.git_version().await;
        let remote = self.remote();
        let branch = self.branch();
        let repo = self.repo_dir();
        let deploy_key = self.public_key();
        let mut body = json!({
            "git": { "available": git.is_some(), "version": git },
            "state_dir": self.state_dir.is_some(),
            "working_copy": repo.as_ref().map(|r| r.display().to_string()),
            "initialised": repo.as_ref().is_some_and(|r| self.initialised(r)),
            "remote": remote.as_ref().ok().cloned().flatten(),
            "remote_error": remote.as_ref().err().cloned(),
            "branch": branch,
            "path": self.path(),
            "auth": self.auth(),
            "token_set": self.settings.secret_state(TOKEN_KEY)["set"] == json!(true),
            "author": self.settings.string(AUTHOR_KEY).unwrap_or_else(|| DEFAULT_AUTHOR.into()),
            "auto": self.settings.string(AUTO_KEY).unwrap_or_else(|| "off".into()),
            "pull_every": self.settings.string(PULL_EVERY_KEY).unwrap_or_else(|| "5m".into()),
            "deploy_key": deploy_key,
            "head": null,
            "upstream": false,
            "ahead": null,
            "behind": null,
            "fetched": false,
            "fetch_error": null,
            "drift": null,
            "last": self.timeline().first().cloned(),
            "timeline": self.timeline(),
        });
        let (Some(repo), Ok(Some(remote))) = (repo, remote) else {
            return body;
        };
        if !self.initialised(&repo) {
            return body;
        }
        let Ok(env) = self.git_env(&remote) else {
            return body;
        };
        let _guard = match self.lock.try_lock() {
            Ok(g) => g,
            Err(_) => {
                body["busy"] = json!(true);
                return body;
            }
        };
        body["head"] = self.head(&env, &repo).await.unwrap_or(Value::Null);
        if fetch {
            match self.fetch(&env, &repo, &branch).await {
                Ok(true) => {
                    let (ahead, behind) = self.ahead_behind(&env, &repo, &branch).await;
                    body["fetched"] = json!(true);
                    body["upstream"] = json!(true);
                    body["ahead"] = json!(ahead);
                    body["behind"] = json!(behind);
                }
                Ok(false) => {
                    body["fetched"] = json!(true);
                    body["upstream"] = json!(false);
                }
                Err(e) => body["fetch_error"] = json!(e.message),
            }
        } else {
            let out = self
                .git(
                    &env,
                    &repo,
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("origin/{branch}"),
                    ],
                )
                .await;
            if out.ok {
                let (ahead, behind) = self.ahead_behind(&env, &repo, &branch).await;
                body["upstream"] = json!(true);
                body["ahead"] = json!(ahead);
                body["behind"] = json!(behind);
            }
        }
        body["drift"] = match self.drift(state, &repo) {
            Ok((changes, missing)) => json!({
                "missing_document": missing,
                "changes": changes,
                "summary": changes.iter().map(Change::line).collect::<Vec<_>>(),
            }),
            Err(e) => json!({ "error": e }),
        };
        body
    }

    /// What a commit would record: the changes from the working copy's
    /// document to the instance. The flag says the working copy has no
    /// document yet.
    fn drift(&self, state: &AppState, repo: &Path) -> Result<(Vec<Change>, bool), String> {
        let dir = self.document_dir(repo);
        let export = configdoc::export(state, SecretsMode::Refs, true).file;
        let doc_path = dir.join(DOCUMENT_NAME);
        if !doc_path.exists() {
            let empty = crate::mocks::config_file::MocksFile::default();
            return Ok((configdoc::diff(&empty, &export, Some(&dir)), true));
        }
        let (working, _) = read_document(&doc_path)?;
        Ok((configdoc::diff(&working, &export, Some(&dir)), false))
    }

    /// `POST /_rustybin/git/init`: create the working copy (or point an
    /// existing one at the remote) and fetch.
    pub async fn init(&self, by: &str) -> GitResult<Value> {
        let started = Instant::now();
        let result = self.init_inner().await;
        self.record(
            "init",
            by,
            started,
            &result
                .as_ref()
                .map(|v| v["detail"].as_str().unwrap_or("").to_string())
                .map_err(|e| e.message.clone()),
        );
        result
    }

    async fn init_inner(&self) -> GitResult<Value> {
        let (repo, remote) = self.ready().await?;
        let env = self.git_env(&remote)?;
        let _guard = self.lock.try_lock().map_err(|_| busy())?;
        let branch = self.branch();
        let fresh = !self.initialised(&repo);
        if fresh {
            if let Some(parent) = repo.parent() {
                crate::settings::create_private_dir(parent)
                    .map_err(|e| GitError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
            }
            std::fs::create_dir_all(&repo).map_err(|e| {
                GitError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("{}: {e}", repo.display()),
                )
            })?;
            self.must(&env, &repo, &["init", "--quiet", "-b", &branch])
                .await?;
            self.must(&env, &repo, &["remote", "add", "origin", &remote.url])
                .await?;
        } else {
            self.must(&env, &repo, &["remote", "set-url", "origin", &remote.url])
                .await?;
        }
        let exists = self.fetch(&env, &repo, &branch).await?;
        let upstream = format!("origin/{branch}");
        let mut checked_out = false;
        if exists {
            if self.head(&env, &repo).await.is_none() {
                // Nothing local yet: take the remote branch as it is.
                self.must(
                    &env,
                    &repo,
                    &["checkout", "--quiet", "-B", &branch, &upstream],
                )
                .await?;
                checked_out = true;
            } else {
                let _ = self
                    .git(
                        &env,
                        &repo,
                        &["branch", &format!("--set-upstream-to={upstream}"), &branch],
                    )
                    .await;
            }
        }
        let head = self.head(&env, &repo).await;
        let detail = match (fresh, exists) {
            (true, true) => format!("cloned {} ({branch})", remote.url),
            (true, false) => format!("initialised an empty working copy for {} ({branch} does not exist yet: the first push creates it)", remote.url),
            (false, true) => format!("working copy points at {} ({branch})", remote.url),
            (false, false) => format!("working copy points at {} ({branch} does not exist on the remote yet)", remote.url),
        };
        Ok(json!({
            "initialised": true,
            "fresh": fresh,
            "remote_branch": exists,
            "checked_out": checked_out,
            "head": head,
            "detail": detail,
        }))
    }

    /// `POST /_rustybin/git/commit`: export the instance into the working
    /// copy and commit.
    pub async fn commit(&self, state: &AppState, message: &str, by: &str) -> GitResult<Value> {
        let started = Instant::now();
        let result = self.commit_inner(state, message).await;
        self.record(
            "commit",
            by,
            started,
            &result
                .as_ref()
                .map(|v| v["detail"].as_str().unwrap_or("").to_string())
                .map_err(|e| e.message.clone()),
        );
        result
    }

    async fn commit_inner(&self, state: &AppState, message: &str) -> GitResult<Value> {
        let message = check_message(message)?;
        let (repo, remote) = self.ready().await?;
        let env = self.git_env(&remote)?;
        let _guard = self.lock.try_lock().map_err(|_| busy())?;
        self.need_init(&repo)?;
        let dir = self.document_dir(&repo);
        let export = configdoc::export(state, SecretsMode::Refs, true);
        configdoc::write_dir(&export, &dir)
            .map_err(|e| GitError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        let pathspec = self.path().unwrap_or_else(|| ".".into());
        self.must(&env, &repo, &["add", "-A", "--", &pathspec])
            .await?;
        let status = self
            .must(&env, &repo, &["status", "--porcelain", "--", &pathspec])
            .await?;
        if status.trim().is_empty() {
            return Ok(json!({
                "committed": false,
                "detail": "nothing to commit: the working copy already matches the instance",
                "head": self.head(&env, &repo).await,
            }));
        }
        let files = status.lines().count();
        self.must(&env, &repo, &["commit", "--quiet", "-m", &message])
            .await?;
        let head = self.head(&env, &repo).await;
        let short = head
            .as_ref()
            .and_then(|h| h["short"].as_str())
            .unwrap_or("")
            .to_string();
        Ok(json!({
            "committed": true,
            "files": files,
            "head": head,
            "detail": format!("{short}: {} ({files} file(s))", message.lines().next().unwrap_or("")),
        }))
    }

    fn need_init(&self, repo: &Path) -> GitResult<()> {
        if self.initialised(repo) {
            Ok(())
        } else {
            Err(GitError::conflict(
                "the working copy does not exist yet: POST /_rustybin/git/init first (Management, Source control, Connect)",
            ))
        }
    }

    /// `POST /_rustybin/git/push`.
    pub async fn push(&self, by: &str) -> GitResult<Value> {
        let started = Instant::now();
        let result = self.push_inner().await;
        self.record(
            "push",
            by,
            started,
            &result
                .as_ref()
                .map(|v| v["detail"].as_str().unwrap_or("").to_string())
                .map_err(|e| e.message.clone()),
        );
        result
    }

    async fn push_inner(&self) -> GitResult<Value> {
        let (repo, remote) = self.ready().await?;
        let env = self.git_env(&remote)?;
        let _guard = self.lock.try_lock().map_err(|_| busy())?;
        self.need_init(&repo)?;
        let branch = self.branch();
        if self.head(&env, &repo).await.is_none() {
            return Err(GitError::conflict("nothing to push: commit first"));
        }
        let out = self
            .git(&env, &repo, &["push", "--quiet", "-u", "origin", &branch])
            .await;
        if !out.ok {
            let text = out.stderr.to_ascii_lowercase();
            if text.contains("non-fast-forward")
                || text.contains("fetch first")
                || text.contains("rejected")
            {
                let (ahead, behind) = match self.fetch(&env, &repo, &branch).await {
                    Ok(true) => self.ahead_behind(&env, &repo, &branch).await,
                    _ => (0, 0),
                };
                let mut e = GitError::conflict(
                    "the remote has commits this instance does not have: pull first, or resolve with the instance's copy (POST /_rustybin/git/resolve)",
                );
                e.extra = json!({ "conflict": true, "ahead": ahead, "behind": behind });
                return Err(e);
            }
            return Err(GitError::failed(format!("git push: {}", out.stderr)));
        }
        let head = self.head(&env, &repo).await;
        Ok(json!({
            "pushed": true,
            "head": head,
            "detail": format!("pushed {branch} to {}", remote.url),
        }))
    }

    /// `POST /_rustybin/git/pull`: fetch and fast-forward; with `apply`,
    /// apply the repository's document (`dry_run` shows the diff only).
    pub async fn pull(
        &self,
        state: &AppState,
        apply: bool,
        dry_run: bool,
        prune: bool,
        by: &str,
    ) -> GitResult<Value> {
        let started = Instant::now();
        let result = self.pull_inner(state, apply, dry_run, prune, by).await;
        if !dry_run {
            self.record(
                "pull",
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

    async fn pull_inner(
        &self,
        state: &AppState,
        apply: bool,
        dry_run: bool,
        prune: bool,
        by: &str,
    ) -> GitResult<Value> {
        let (repo, remote) = self.ready().await?;
        let env = self.git_env(&remote)?;
        let _guard = self.lock.try_lock().map_err(|_| busy())?;
        self.need_init(&repo)?;
        let branch = self.branch();
        if !self.fetch(&env, &repo, &branch).await? {
            return Err(GitError::conflict(format!(
                "the remote has no branch {branch} yet: commit and push first"
            )));
        }
        let upstream = format!("origin/{branch}");
        let (ahead, behind) = self.ahead_behind(&env, &repo, &branch).await;
        let mut fast_forwarded = false;
        if self.head(&env, &repo).await.is_none() {
            if !dry_run {
                self.must(
                    &env,
                    &repo,
                    &["checkout", "--quiet", "-B", &branch, &upstream],
                )
                .await?;
                fast_forwarded = true;
            }
        } else if ahead > 0 && behind > 0 {
            let mut e = GitError::conflict(format!(
                "the repository and this instance have diverged ({ahead} commit(s) here, {behind} there): resolve with the instance's copy or the repository's (POST /_rustybin/git/resolve)"
            ));
            e.extra = json!({ "conflict": true, "ahead": ahead, "behind": behind, "options": ["instance", "repository"] });
            return Err(e);
        } else if behind > 0 && !dry_run {
            self.must(&env, &repo, &["merge", "--quiet", "--ff-only", &upstream])
                .await?;
            fast_forwarded = true;
        }
        let head = self.head(&env, &repo).await;
        let mut body = json!({
            "fast_forwarded": fast_forwarded,
            "ahead": ahead,
            "behind": behind,
            "head": head,
            "applied": null,
            "dry_run": dry_run,
        });
        let mut detail = if behind > 0 {
            format!("pulled {behind} commit(s)")
        } else {
            "up to date".to_string()
        };
        if apply {
            // A dry run diffs the remote's document without moving the
            // working copy: read it from the remote branch.
            let report = self
                .apply_working_copy(
                    state,
                    &env,
                    &repo,
                    if dry_run { Some(&upstream) } else { None },
                    dry_run,
                    prune,
                    by,
                )
                .await?;
            detail = format!(
                "{detail}; {} {} change(s){}",
                if dry_run { "would apply" } else { "applied" },
                report["changes"].as_array().map(Vec::len).unwrap_or(0),
                match report["problems"].as_array().map(Vec::len).unwrap_or(0) {
                    0 => String::new(),
                    n => format!(", {n} problem(s)"),
                }
            );
            body["applied"] = report;
        }
        body["detail"] = json!(detail);
        Ok(body)
    }

    /// Apply the document of the working copy (or, for a dry run, of
    /// `origin/<branch>` extracted into a temporary directory).
    #[allow(clippy::too_many_arguments)]
    async fn apply_working_copy(
        &self,
        state: &AppState,
        env: &GitEnv,
        repo: &Path,
        from_ref: Option<&str>,
        dry_run: bool,
        prune: bool,
        by: &str,
    ) -> GitResult<Value> {
        let _temp;
        let dir = match from_ref {
            Some(r) => {
                let temp = TempDir::new(self.state_dir.as_deref().unwrap_or(Path::new(".")))?;
                let dir = self.extract(env, repo, r, &temp.path).await?;
                _temp = temp;
                dir
            }
            None => self.document_dir(repo),
        };
        let doc_path = dir.join(DOCUMENT_NAME);
        if !doc_path.exists() {
            return Err(GitError::conflict(format!(
                "the repository has no {} under {}: commit and push from an instance first",
                DOCUMENT_NAME,
                self.path().unwrap_or_else(|| "its root".into())
            )));
        }
        let (file, problems) = read_document(&doc_path).map_err(GitError::failed)?;
        let opts = ApplyOptions {
            prune,
            dry_run,
            principal: format!("git:{by}"),
            base: Some(dir.clone()),
        };
        let mut report = configdoc::apply(state, file, &opts);
        if !problems.is_empty() {
            if let Some(list) = report["problems"].as_array_mut() {
                for p in problems {
                    list.insert(0, json!(p));
                }
            }
            report["ok"] = json!(false);
        }
        Ok(report)
    }

    /// The document of `<ref>:<path>/` and the spec files it references,
    /// written into `temp` (a dry run never moves the working copy).
    async fn extract(&self, env: &GitEnv, repo: &Path, r: &str, temp: &Path) -> GitResult<PathBuf> {
        let prefix = match self.path() {
            Some(p) => format!("{p}/"),
            None => String::new(),
        };
        let show = |object: String| async move {
            let out = self.git(env, repo, &["show", &object]).await;
            if out.ok {
                Ok(out.stdout)
            } else {
                Err(GitError::conflict(format!(
                    "the repository has no {object} on {r}: commit and push from an instance first"
                )))
            }
        };
        let text = show(format!("{r}:{prefix}{DOCUMENT_NAME}")).await?;
        let doc = temp.join(DOCUMENT_NAME);
        std::fs::write(&doc, text.as_bytes())
            .map_err(|e| GitError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let (file, _) =
            crate::mocks::config_file::parse_lenient(&text).map_err(GitError::failed)?;
        for spec in file.mocks().iter().filter_map(|m| m.spec.as_deref()) {
            let rel = Path::new(spec);
            if rel.is_absolute()
                || rel
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                continue; // reported by the apply as unreadable
            }
            let content = match show(format!("{r}:{prefix}{spec}")).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            let target = temp.join(rel);
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&target, content.as_bytes());
        }
        Ok(temp.to_path_buf())
    }

    /// `POST /_rustybin/git/resolve`: after a diverged pull, `instance`
    /// drops the local commits and commits the instance's copy on top of
    /// the remote (then pushes), `repository` takes the remote's branch
    /// and applies it with prune.
    pub async fn resolve(
        &self,
        state: &AppState,
        with: &str,
        message: &str,
        by: &str,
    ) -> GitResult<Value> {
        if with != "instance" && with != "repository" {
            return Err(GitError::new(
                StatusCode::BAD_REQUEST,
                "with must be instance (this instance's copy wins) or repository (the repository's copy wins)",
            ));
        }
        let started = Instant::now();
        let result = self.resolve_inner(state, with, message, by).await;
        self.record(
            "resolve",
            by,
            started,
            &result
                .as_ref()
                .map(|v| v["detail"].as_str().unwrap_or("").to_string())
                .map_err(|e| e.message.clone()),
        );
        result
    }

    async fn resolve_inner(
        &self,
        state: &AppState,
        with: &str,
        message: &str,
        by: &str,
    ) -> GitResult<Value> {
        let (repo, remote) = self.ready().await?;
        let env = self.git_env(&remote)?;
        let branch = self.branch();
        let upstream = format!("origin/{branch}");
        {
            let _guard = self.lock.try_lock().map_err(|_| busy())?;
            self.need_init(&repo)?;
            if !self.fetch(&env, &repo, &branch).await? {
                return Err(GitError::conflict(format!(
                    "the remote has no branch {branch}: there is nothing to resolve, push"
                )));
            }
            self.must(&env, &repo, &["reset", "--quiet", "--hard", &upstream])
                .await?;
        }
        if with == "instance" {
            let message = if message.trim().is_empty() {
                "Replace the repository's configuration with this instance's".to_string()
            } else {
                message.to_string()
            };
            let committed = self.commit_inner(state, &message).await?;
            let pushed = if committed["committed"] == json!(true) {
                Some(self.push_inner().await?)
            } else {
                None
            };
            Ok(json!({
                "resolved": "instance",
                "commit": committed,
                "push": pushed,
                "detail": "the repository now holds this instance's configuration",
            }))
        } else {
            let _guard = self.lock.try_lock().map_err(|_| busy())?;
            let report = self
                .apply_working_copy(state, &env, &repo, None, false, true, by)
                .await?;
            Ok(json!({
                "resolved": "repository",
                "applied": report,
                "head": self.head(&env, &repo).await,
                "detail": "this instance now holds the repository's configuration",
            }))
        }
    }

    /// `GET /_rustybin/git/history`: the last commits with their files.
    pub async fn history(&self) -> GitResult<Vec<Value>> {
        let (repo, remote) = self.ready().await?;
        let env = self.git_env(&remote)?;
        let _guard = self.lock.try_lock().map_err(|_| busy())?;
        self.need_init(&repo)?;
        if self.head(&env, &repo).await.is_none() {
            return Ok(Vec::new());
        }
        let n = format!("-n{MAX_HISTORY}");
        let out = self
            .must(
                &env,
                &repo,
                &[
                    "log",
                    &n,
                    "--format=%x1e%H%x1f%h%x1f%an%x1f%aI%x1f%s",
                    "--shortstat",
                ],
            )
            .await?;
        Ok(out
            .split('\u{1e}')
            .filter(|r| !r.trim().is_empty())
            .map(|record| {
                let (head, stat) = record.split_once('\n').unwrap_or((record, ""));
                let mut parts = head.split('\u{1f}');
                json!({
                    "sha": parts.next().unwrap_or("").trim(),
                    "short": parts.next().unwrap_or(""),
                    "author": parts.next().unwrap_or(""),
                    "at": parts.next().unwrap_or(""),
                    "subject": parts.next().unwrap_or(""),
                    "stat": stat.trim(),
                })
            })
            .collect())
    }

    // ── The deploy key ──────────────────────────────────────────────

    /// The deploy key's public half, when one exists.
    pub fn public_key(&self) -> Option<Value> {
        let path = self.state_file(KEY_FILE)?;
        let public = std::fs::read_to_string(path.with_extension("key.pub")).ok()?;
        Some(json!({
            "public": public.trim(),
            "path": path.display().to_string(),
        }))
    }

    /// `POST /_rustybin/git/key`: generate the deploy key (`replace` an
    /// existing one).
    pub async fn generate_key(&self, replace: bool, by: &str) -> GitResult<Value> {
        let started = Instant::now();
        let result = self.generate_key_inner(replace).await;
        self.record(
            "key",
            by,
            started,
            &result
                .as_ref()
                .map(|_| "deploy key generated".to_string())
                .map_err(|e| e.message.clone()),
        );
        result
    }

    async fn generate_key_inner(&self, replace: bool) -> GitResult<Value> {
        let path = self
            .state_file(KEY_FILE)
            .ok_or_else(|| GitError::conflict("source control needs RUSTYBIN_STATE_DIR"))?;
        let _guard = self.lock.try_lock().map_err(|_| busy())?;
        if path.exists() {
            if !replace {
                return Err(GitError::conflict(
                    "a deploy key exists: send {\"replace\": true} to generate a new one (the old public key stops working)",
                ));
            }
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(path.with_extension("key.pub"));
        }
        if let Some(dir) = path.parent() {
            crate::settings::create_private_dir(dir)
                .map_err(|e| GitError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        }
        let comment = format!("rustybin-{}", self.instance_id);
        let out = tokio::time::timeout(
            GIT_TIMEOUT,
            tokio::process::Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C", &comment, "-f"])
                .arg(&path)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await;
        match out {
            Ok(Ok(o)) if o.status.success() => {}
            Ok(Ok(o)) => {
                return Err(GitError::failed(format!(
                    "ssh-keygen: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                )))
            }
            Ok(Err(e)) => {
                return Err(GitError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("ssh-keygen is not available ({e}): the container image includes it; elsewhere install OpenSSH"),
                ))
            }
            Err(_) => return Err(GitError::failed("ssh-keygen did not finish")),
        }
        let mut body = self
            .public_key()
            .ok_or_else(|| GitError::failed("the public key was not written"))?;
        body["generated"] = json!(true);
        Ok(body)
    }

    // ── Automatic mode and startup ──────────────────────────────────

    /// One tick of the automatic mode (every 30 s): push the instance's
    /// changes, or pull at the interval.
    pub async fn auto_tick(&self, state: &AppState) {
        let mode = self
            .settings
            .string(AUTO_KEY)
            .unwrap_or_else(|| "off".into());
        if mode == "off" {
            return;
        }
        let Some(repo) = self.repo_dir() else {
            return;
        };
        if !self.initialised(&repo) || !matches!(self.remote(), Ok(Some(_))) {
            return;
        }
        match mode.as_str() {
            "push_on_change" => {
                let changes = match self.drift(state, &repo) {
                    Ok((changes, _)) => changes,
                    Err(e) => {
                        self.warn_once(&format!("auto push: {e}"));
                        return;
                    }
                };
                if changes.is_empty() {
                    return;
                }
                let mut message = String::from("Update ");
                let lines: Vec<String> = changes.iter().take(3).map(Change::line).collect();
                message.push_str(&lines.join("; "));
                if changes.len() > 3 {
                    message.push_str(&format!(" and {} more", changes.len() - 3));
                }
                let message = truncate(&message, 200);
                if let Err(e) = self.commit(state, &message, "auto").await {
                    self.warn_once(&format!("auto commit: {}", e.message));
                    return;
                }
                if let Err(e) = self.push("auto").await {
                    self.warn_once(&format!("auto push: {}", e.message));
                }
            }
            "pull" => {
                let every = parse_interval(
                    &self
                        .settings
                        .string(PULL_EVERY_KEY)
                        .unwrap_or_else(|| "5m".into()),
                )
                .unwrap_or(Duration::from_secs(300));
                let due = self
                    .last_pull
                    .lock()
                    .map(|l| l.is_none_or(|t| t.elapsed() >= every))
                    .unwrap_or(true);
                if !due {
                    return;
                }
                if let Ok(mut l) = self.last_pull.lock() {
                    *l = Some(Instant::now());
                }
                if let Err(e) = self.pull(state, true, false, true, "auto").await {
                    self.warn_once(&format!("auto pull: {}", e.message));
                }
            }
            _ => {}
        }
    }

    fn warn_once(&self, message: &str) {
        if let Ok(mut last) = self.auto_warned.lock() {
            if last.as_deref() == Some(message) {
                return;
            }
            *last = Some(message.to_string());
        }
        tracing::warn!(target: "rustybin::git", "{message}");
    }

    /// `RUSTYBIN_CONFIG_GIT` at startup: clone and apply (with prune)
    /// before the instance serves. Fatal only with
    /// `RUSTYBIN_CONFIG_GIT_REQUIRED=true`; otherwise a warning.
    pub async fn startup(&self, state: &AppState) -> Result<(), String> {
        if state.config.config_git.is_none() {
            return Ok(());
        }
        let required = state.config.config_git_required;
        let result = async {
            let init = self.init("startup").await.map_err(|e| e.message)?;
            if init["remote_branch"] == json!(false) {
                return Ok(format!(
                    "{}: the repository is empty, nothing applied",
                    init["detail"].as_str().unwrap_or("")
                ));
            }
            let pulled = self
                .pull(state, true, false, true, "startup")
                .await
                .map_err(|e| e.message)?;
            let problems = pulled["applied"]["problems"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for p in &problems {
                tracing::warn!(target: "rustybin::git", "startup apply: {}", p.as_str().unwrap_or(""));
            }
            Ok::<String, String>(format!(
                "applied {} from {} ({} change(s), {} problem(s))",
                DOCUMENT_NAME,
                state.config.config_git.as_deref().unwrap_or(""),
                pulled["applied"]["changes"].as_array().map(Vec::len).unwrap_or(0),
                problems.len()
            ))
        }
        .await;
        match result {
            Ok(detail) => {
                tracing::info!(target: "rustybin::git", "RUSTYBIN_CONFIG_GIT: {detail}");
                Ok(())
            }
            Err(e) if required => Err(format!(
                "RUSTYBIN_CONFIG_GIT: {e} (RUSTYBIN_CONFIG_GIT_REQUIRED=true: not starting)"
            )),
            Err(e) => {
                tracing::warn!(target: "rustybin::git", "RUSTYBIN_CONFIG_GIT: {e}; starting without the repository's configuration");
                Ok(())
            }
        }
    }
}

fn busy() -> GitError {
    GitError::conflict("another git operation is running: try again in a moment")
}

fn check_message(message: &str) -> GitResult<String> {
    let m = message.trim();
    if m.is_empty() {
        return Err(GitError::new(
            StatusCode::BAD_REQUEST,
            "the commit message is empty",
        ));
    }
    if m.chars().count() > MAX_MESSAGE {
        return Err(GitError::new(
            StatusCode::BAD_REQUEST,
            format!("the commit message is longer than {MAX_MESSAGE} characters"),
        ));
    }
    if m.chars().any(|c| c.is_control() && c != '\n') {
        return Err(GitError::new(
            StatusCode::BAD_REQUEST,
            "the commit message contains control characters",
        ));
    }
    Ok(m.to_string())
}

/// A working copy's document, with its problems (unset variables, skipped
/// entries); `Err` when nothing could be read.
pub fn read_document(
    path: &Path,
) -> Result<(crate::mocks::config_file::MocksFile, Vec<String>), String> {
    use crate::mocks::config_file;
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > config_file::MAX_FILE_BYTES {
        return Err(format!(
            "{}: larger than {} bytes",
            path.display(),
            config_file::MAX_FILE_BYTES
        ));
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (text, missing) = config_file::expand_env(&raw, |k| std::env::var(k).ok());
    let mut problems: Vec<String> = missing
        .into_iter()
        .map(|v| format!("{}: environment variable {v} is not set", DOCUMENT_NAME))
        .collect();
    let (file, skipped) =
        config_file::parse_lenient(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    problems.extend(
        skipped
            .into_iter()
            .map(|f| format!("{}: {}", DOCUMENT_NAME, f.error)),
    );
    Ok((file, problems))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max).collect::<String>())
    }
}

/// A single-quoted shell word (for `GIT_SSH_COMMAND`).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A directory under the state directory, removed when dropped.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(parent: &Path) -> GitResult<Self> {
        let path = parent.join(format!(".pull-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&path).map_err(|e| {
            GitError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("{}: {e}", path.display()),
            )
        })?;
        Ok(Self { path })
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
