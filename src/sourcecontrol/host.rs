//! A pull request opened through the git host's API (step 20 of
//! `docs/plans/core-next/master-plan.md`).
//!
//! `resolve` with `pull_request` pushes the instance's side to a new branch
//! (`branches.rs`). With `open: true` the instance then asks the git host
//! to open the pull request and answers its address. That is all it asks of
//! a host: it never merges, reviews or comments.
//!
//! - **Which host**: the setting `git.host`. `auto` recognises the
//!   well-known public host names only ([`Host::public_host`]) and is
//!   `none` for anything else, so a self-hosted host is named on purpose.
//!   The API's base is `git.host_api`, else the public API of a public
//!   host, else the kind's usual path on the remote's host.
//! - **One trait, one small module per kind** ([`Host`]; `github.rs`,
//!   `gitlab.rs`, `gitea.rs` for Gitea and Forgejo): a kind says where a
//!   call goes, what it carries and how its answer reads. The calls
//!   themselves are made here, in one place, for every kind.
//! - **The network**: through `egress::Egress` under `Purpose::GitHost`,
//!   whose rule permits exactly the API host the settings resolve to
//!   ([`SourceControl::sync_host_egress`]). Pinned addresses, no redirect
//!   followed, a bounded answer, a timeout.
//! - **The token** is the secret `git.host_token`, else `git.token` when
//!   that is the authentication of the remote and the API is the remote's
//!   own host. It is sent in a header to the API and is in no answer, log
//!   line or audit entry.
//! - **What is sent**: the title and the body are the instance's own text
//!   ([`pull_request_text`]): the caller's message and the names of what
//!   the instance holds. Nothing of the repository's side is copied.
//! - **What comes back** is the host's: the address and the number are
//!   answered as `untrusted_pull_request`, a refusal's words as
//!   `untrusted_host_message`, cleaned and cut, never in `detail`.
//! - **A failure to open** leaves the pushed branch where it is and says
//!   why ([`NotOpened`]): a person opens the pull request by hand.
//! - **One already open**: when the host refuses the opening call, it is
//!   asked once which pull requests are open for that head and base
//!   ([`SourceControl::existing_at_host`]: one read, the same path out),
//!   and the one found is answered instead of the refusal. The opening
//!   call itself is never made twice.

use std::time::Duration;

use serde_json::{json, Value};
use zeroize::Zeroizing;

use super::{
    GitError, GitResult, Remote, RemoteKind, SourceControl, AUTH_KEY, REMOTE_KEY, TOKEN_KEY,
};
use crate::configdoc::Change;
use crate::egress::{Allowlist, Purpose, Rule};

mod gitea;
mod github;
mod gitlab;

pub const HOST_KEY: &str = "git.host";
pub const HOST_API_KEY: &str = "git.host_api";
pub const HOST_TOKEN_KEY: &str = "git.host_token";

/// `git.host`: `auto`, `none`, or a kind by name.
pub const HOST_OPTIONS: &[&str] = &["auto", "none", "github", "gitlab", "gitea"];

/// How long the host may take to answer.
pub const HOST_TIMEOUT: Duration = Duration::from_secs(20);
/// The largest answer read from the host.
pub const MAX_ANSWER_BYTES: usize = 256 * 1024;
/// Longest title sent.
pub const MAX_TITLE: usize = 120;
/// Longest body sent.
pub const MAX_BODY: usize = 4000;
/// Most entries the body names.
pub const MAX_BODY_ENTRIES: usize = 40;
/// Longest address taken from the host's answer.
pub const MAX_ADDRESS: usize = 500;
/// Longest refusal text kept from the host's answer.
pub const MAX_HOST_MESSAGE: usize = 300;

/// The host kinds with a module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Github,
    Gitlab,
    /// Gitea and Forgejo: one API shape.
    Gitea,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Github, Kind::Gitlab, Kind::Gitea];

    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Github => "github",
            Kind::Gitlab => "gitlab",
            Kind::Gitea => "gitea",
        }
    }

    fn host(&self) -> &'static dyn Host {
        match self {
            Kind::Github => &github::Github,
            Kind::Gitlab => &gitlab::Gitlab,
            Kind::Gitea => &gitea::Gitea,
        }
    }

    pub fn named(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == name)
    }
}

/// What the instance asks the host to open.
pub struct PullRequest<'a> {
    /// The repository as the kind writes it ([`Host::repository`]).
    pub repository: &'a str,
    /// The branch the pull request is into.
    pub base: &'a str,
    /// The branch the instance pushed.
    pub head: &'a str,
    pub title: &'a str,
    pub body: &'a str,
}

/// One call to the host's API. It holds the token: never printed.
pub struct Call {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    /// What a `POST` carries; a read (`GET`) carries nothing.
    pub body: Option<Value>,
}

/// What the host answered for an opened pull request. Both are the host's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Opened {
    pub number: Option<u64>,
    pub url: Option<String>,
}

/// A git host kind: open a pull request, read the answer. Nothing else.
pub trait Host: Send + Sync {
    /// The public host's name, which `auto` recognises, and its API. A
    /// kind without one public host that a repository is certain to be on
    /// has none, and is always named in `git.host`.
    fn public(&self) -> Option<(&'static str, &'static str)>;
    /// Where a self-hosted host of this kind usually serves its API.
    fn self_hosted_api(&self, host: &str) -> String;
    /// The repository as this kind's API names it, from the remote's path
    /// (`org/repo`, no `.git`).
    fn repository(&self, path: &str) -> Result<String, String>;
    /// The call that opens a pull request.
    fn open(&self, api: &str, token: &str, pr: &PullRequest) -> Call;
    /// The number and the address in the answer of an accepted call.
    fn opened(&self, answer: &Value) -> Opened;
    /// The host's own words in the answer of a refused call.
    fn message(&self, answer: &Value) -> Option<String>;
    /// The read that lists the open pull requests for `pr`'s head and
    /// base (as far as the kind's API can narrow it).
    fn existing(&self, api: &str, token: &str, pr: &PullRequest) -> Call;
    /// The open pull request from `pr`'s head into its base in the answer
    /// of that read, when there is one. An entry that names other branches
    /// is never taken.
    fn found(&self, answer: &Value, pr: &PullRequest) -> Option<Opened>;
}

/// `url` with a query of `pairs`, each value encoded.
fn with_query(url: &str, pairs: &[(&str, &str)]) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut parsed) => {
            parsed.query_pairs_mut().extend_pairs(pairs);
            parsed.to_string()
        }
        // Never for an API that passed `check_api`: the call then goes
        // nowhere the settings did not name, and finds nothing.
        Err(_) => url.to_string(),
    }
}

/// The host the settings resolve to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub kind: Kind,
    /// The API's base, without a trailing slash.
    pub api: String,
    /// The repository as the kind's API names it.
    pub repository: String,
    /// The setting the token comes from, when one is set.
    pub token_from: Option<&'static str>,
}

/// What `git.host` and the remote give.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Setup {
    /// No host: `none`, or `auto` with a remote it does not recognise.
    Off(String),
    /// A host is named and something is missing.
    Problem(Option<Kind>, String),
    Ready(Target),
}

/// Why a pull request was not opened. The branch stays pushed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotOpened {
    /// `egress_refused`, `timeout`, `unreachable`, `redirected`,
    /// `token_refused`, `not_permitted`, `not_found`, `refused`,
    /// `rate_limited`, `host_error` or `bad_answer`.
    pub reason: &'static str,
    /// In the instance's own words.
    pub why: String,
    /// The host's HTTP status, when it answered.
    pub status: Option<u16>,
    /// The host's words, cleaned and cut: third-party content.
    pub host_message: Option<String>,
}

/// The reason of a pull request the host itself refused (`400`, `409`,
/// `422`): a branch it does not know, nothing to merge, or one already
/// open, which is then looked for.
const REFUSED: &str = "refused";

impl NotOpened {
    /// Whether the refusal may mean that a pull request is already open
    /// for the two branches: the host answered and refused the request
    /// itself, not the token or the repository.
    pub fn may_be_open(&self) -> bool {
        self.reason == REFUSED
    }

    fn new(reason: &'static str, why: impl Into<String>) -> Self {
        Self {
            reason,
            why: why.into(),
            status: None,
            host_message: None,
        }
    }

    pub fn json(&self) -> Value {
        let mut v = json!({ "reason": self.reason, "why": self.why });
        if let Some(s) = self.status {
            v["status"] = json!(s);
        }
        if let Some(m) = &self.host_message {
            v["untrusted_host_message"] = json!(m);
        }
        v
    }
}

/// `git.host_api`: empty, or an `https://` base without a query (plain
/// `http://` only for a loopback address, which is for tests and demos).
pub fn check_api(v: &Value) -> Result<(), String> {
    let s = v.as_str().unwrap_or("").trim();
    if s.is_empty() {
        return Ok(());
    }
    let target = crate::egress::parse_url(s, false)
        .map_err(|e| format!("the API's base URL (https://git.example.com/api/v3): {e}"))?;
    let loopback = target.ip.is_some_and(|ip| ip.is_loopback()) || target.host == "localhost";
    if !target.tls() && !loopback {
        return Err("the API's base URL must use https (the token is sent to it)".into());
    }
    Ok(())
}

/// `git.host_api` without surrounding space or trailing slashes.
pub fn normalize_api(v: &Value) -> Value {
    match v.as_str() {
        Some(s) => json!(s.trim().trim_end_matches('/')),
        None => v.clone(),
    }
}

/// The path of a remote as a host's API would name the repository:
/// `org/repo` of `https://host/org/repo.git`, `git@host:org/repo.git` or
/// `ssh://git@host/org/repo.git`. A local remote (tests and demos) gives
/// its last two directories.
pub fn remote_path(remote: &Remote) -> Result<String, String> {
    let url = remote.url.trim();
    let raw: String = match remote.kind {
        RemoteKind::Https => reqwest::Url::parse(url)
            .map_err(|e| format!("the remote: {e}"))?
            .path()
            .to_string(),
        RemoteKind::Ssh if url.to_ascii_lowercase().starts_with("ssh://") => {
            reqwest::Url::parse(url)
                .map_err(|e| format!("the remote: {e}"))?
                .path()
                .to_string()
        }
        // scp style: [user@]host:path
        RemoteKind::Ssh => url
            .split_once(':')
            .map(|(_, p)| p.to_string())
            .unwrap_or_default(),
        RemoteKind::Local => {
            let path = url.strip_prefix("file://").unwrap_or(url);
            let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
            let from = parts.len().saturating_sub(2);
            parts[from..].join("/")
        }
    };
    let trimmed = raw.trim_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let parts: Vec<&str> = trimmed.split('/').collect();
    let part_ok = |p: &&str| {
        !p.is_empty()
            && *p != "."
            && *p != ".."
            && p.len() <= 255
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    if parts.len() < 2 || parts.len() > 20 || !parts.iter().all(part_ok) {
        return Err(
            "the remote's path does not read as a repository at a git host (owner/name)".into(),
        );
    }
    Ok(parts.join("/"))
}

/// The host of a URL, lower case, as the remote's host is kept.
fn url_host(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .host_str()
        .map(|h| h.trim_matches(['[', ']']).to_ascii_lowercase())
}

/// Text from a git host for an answer: no control characters, cut.
fn clean(text: &str, max: usize) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() > max {
        format!("{}...", cleaned.chars().take(max).collect::<String>())
    } else {
        cleaned
    }
}

/// An address from a git host, kept only when it reads as a plain web
/// address: `http` or `https`, a host, no credentials, bounded.
fn clean_address(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty()
        || s.len() > MAX_ADDRESS
        || s.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    let parsed = reqwest::Url::parse(s).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    Some(parsed.to_string())
}

/// The title and the body of the pull request: the instance's own text.
/// `message` is what the caller wrote for the commit; `changes` is what the
/// instance's side changes in the repository's document. Only the names of
/// what the instance itself holds are written (an entry it adds or holds
/// differently); what only the repository's side has is counted, never
/// named, and no value of either side is copied.
pub fn pull_request_text(
    base: &str,
    head: &str,
    message: &str,
    changes: Option<&[Change]>,
) -> (String, String) {
    let first = message.lines().next().unwrap_or("").trim();
    let title = if first.is_empty() {
        format!("Rustybin: this instance's configuration into {base}")
    } else {
        clean(first, MAX_TITLE)
    };
    let mut body = format!(
        "Opened by a Rustybin instance. Its configuration and `{base}` have diverged, and a person chose to settle it in a pull request: `{head}` holds the instance's side. The instance never merges: merge or close this at the git host, then pull.\n"
    );
    if let Some(changes) = changes {
        let own: Vec<&Change> = changes.iter().filter(|c| c.action != "removed").collect();
        let theirs = changes.len() - own.len();
        if own.is_empty() {
            body.push_str("\nThe instance holds nothing that the repository's document lacks or holds differently.\n");
        } else {
            body.push_str(&format!(
                "\nWhat the instance holds that the repository's document lacks or holds differently ({}):\n\n",
                own.len()
            ));
            for c in own.iter().take(MAX_BODY_ENTRIES) {
                let name = if c.key.is_empty() {
                    c.section.to_string()
                } else {
                    format!("{}.{}", c.section, c.key)
                };
                body.push_str(&format!("- `{}` ({})\n", clean(&name, 120), c.action));
            }
            if own.len() > MAX_BODY_ENTRIES {
                body.push_str(&format!("- and {} more\n", own.len() - MAX_BODY_ENTRIES));
            }
        }
        if theirs > 0 {
            body.push_str(&format!(
                "\nThe repository's document holds {theirs} entr{} the instance does not.\n",
                if theirs == 1 { "y" } else { "ies" }
            ));
        }
    }
    if body.chars().count() > MAX_BODY {
        body = format!("{}...\n", body.chars().take(MAX_BODY).collect::<String>());
    }
    (title, body)
}

fn refused(message: String, reason: &str) -> GitError {
    let mut e = GitError::conflict(message);
    e.extra = json!({ "reason": reason });
    e
}

async fn read_bounded(mut resp: reqwest::Response) -> Result<Vec<u8>, reqwest::Error> {
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        let room = MAX_ANSWER_BYTES.saturating_sub(body.len());
        if chunk.len() >= room {
            // More than is ever read: what was read is kept and judged.
            body.extend_from_slice(&chunk[..room]);
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

impl SourceControl {
    /// What `git.host`, `git.host_api`, the token and the remote resolve to.
    pub fn host_setup(&self) -> Setup {
        let setting = self
            .settings
            .string(HOST_KEY)
            .unwrap_or_else(|| "auto".into());
        if setting == "none" {
            return Setup::Off(
                "git.host is none: a pull request is opened by hand at the git host".into(),
            );
        }
        let remote = match self.remote() {
            Ok(Some(r)) => r,
            Ok(None) => return Setup::Off(format!("no repository is configured ({REMOTE_KEY})")),
            Err(e) => return Setup::Off(format!("{REMOTE_KEY}: {e}")),
        };
        let kind = if setting == "auto" {
            let found = Kind::ALL.into_iter().find(|k| {
                k.host()
                    .public()
                    .is_some_and(|(name, _)| remote.host.as_deref() == Some(name))
            });
            match found {
                Some(k) => k,
                None => {
                    let known: Vec<&str> = Kind::ALL
                        .iter()
                        .filter_map(|k| k.host().public().map(|(name, _)| name))
                        .collect();
                    return Setup::Off(format!(
                        "git.host is auto, which recognises {} only: name the kind in git.host for a self-hosted host",
                        known.join(" and ")
                    ));
                }
            }
        } else {
            match Kind::named(&setting) {
                Some(k) => k,
                None => return Setup::Off(format!("git.host: {setting} is not a host kind")),
            }
        };
        let host = kind.host();
        // The public API, when the repository is on the kind's public host.
        let public_api = host
            .public()
            .filter(|(name, _)| remote.host.as_deref() == Some(*name))
            .map(|(_, api)| api);
        let api =
            match self
                .settings
                .string(HOST_API_KEY)
                .map(|s| s.trim().trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| public_api.map(str::to_string))
            {
                Some(api) => api,
                None => match remote.host.as_deref() {
                    Some(h) if h.parse::<std::net::Ipv6Addr>().is_ok() => {
                        host.self_hosted_api(&format!("[{h}]"))
                    }
                    Some(h) => host.self_hosted_api(h),
                    None => return Setup::Problem(
                        Some(kind),
                        "a local repository has no host: set git.host_api to the API's base URL"
                            .into(),
                    ),
                },
            };
        if let Err(e) = check_api(&json!(api)) {
            return Setup::Problem(Some(kind), format!("{HOST_API_KEY}: {e}"));
        }
        let repository = match remote_path(&remote).and_then(|p| host.repository(&p)) {
            Ok(r) => r,
            Err(e) => return Setup::Problem(Some(kind), e),
        };
        // The remote's token goes only where the remote is: its own host,
        // or the public API of the public host it is on.
        let same_host = url_host(&api).is_some_and(|h| Some(h) == remote.host)
            || public_api == Some(api.as_str());
        let token_from = if self.settings.secret(HOST_TOKEN_KEY).is_some() {
            Some(HOST_TOKEN_KEY)
        } else if same_host
            && self.settings.string(AUTH_KEY).as_deref().unwrap_or("token") == "token"
            && self.settings.secret(TOKEN_KEY).is_some()
        {
            Some(TOKEN_KEY)
        } else {
            None
        };
        Setup::Ready(Target {
            kind,
            api,
            repository,
            token_from,
        })
    }

    /// Permit exactly the API host the settings resolve to under
    /// `Purpose::GitHost` (nothing when no host is set up).
    pub fn sync_host_egress(&self, setup: &Setup) {
        let entries: Vec<String> = match setup {
            Setup::Ready(t) => crate::egress::url_entry(&t.api).into_iter().collect(),
            _ => Vec::new(),
        };
        let (allow, _) = Allowlist::parse(&entries);
        self.egress.permit(
            Purpose::GitHost,
            Rule {
                allow,
                any: false,
                loopback: false,
            },
        );
    }

    /// `host` of `GET /_rustybin/git`: what a pull request would be opened
    /// at, and what is missing. Never the token.
    pub fn host_status(&self) -> Value {
        let setup = self.host_setup();
        self.sync_host_egress(&setup);
        let setting = self
            .settings
            .string(HOST_KEY)
            .unwrap_or_else(|| "auto".into());
        match setup {
            Setup::Off(why) => json!({
                "setting": setting, "kind": null, "api": null, "repository": null,
                "token_set": false, "token_from": null, "ready": false, "why": why,
            }),
            Setup::Problem(kind, why) => json!({
                "setting": setting, "kind": kind.map(|k| k.as_str()), "api": null,
                "repository": null, "token_set": false, "token_from": null,
                "ready": false, "why": why,
            }),
            Setup::Ready(t) => {
                let why = if t.token_from.is_none() {
                    Some(no_token())
                } else {
                    None
                };
                json!({
                    "setting": setting,
                    "kind": t.kind.as_str(),
                    "api": t.api,
                    "repository": t.repository,
                    "token_set": t.token_from.is_some(),
                    "token_from": t.token_from,
                    "ready": t.token_from.is_some(),
                    "why": why,
                })
            }
        }
    }

    /// The host of a call with `open: true`, or the `409` that says what is
    /// missing: no kind, no repository, no token, or an API the outbound
    /// policy refuses. Checked before anything is pushed and before any
    /// call; a dry run is refused like the call.
    pub(super) fn host_ready(&self) -> GitResult<(Target, Zeroizing<String>)> {
        let setup = self.host_setup();
        self.sync_host_egress(&setup);
        let target = match setup {
            Setup::Ready(t) => t,
            Setup::Off(why) => {
                return Err(refused(
                    format!("a pull request cannot be opened from here: {why}. Call again without open and open it at the git host"),
                    "host_not_set",
                ))
            }
            Setup::Problem(_, why) => {
                return Err(refused(
                    format!("a pull request cannot be opened from here: {why}"),
                    "host_not_set",
                ))
            }
        };
        let token = target
            .token_from
            .and_then(|key| self.settings.secret(key))
            .ok_or_else(|| {
                refused(
                    format!("a pull request cannot be opened from here: {}", no_token()),
                    "host_token_missing",
                )
            })?;
        if let Some(reasons) = self.egress_refusal(&target) {
            let mut e = refused(
                format!(
                    "the outbound policy refuses the git host's API ({}): {reasons}",
                    crate::egress::url_origin(&target.api),
                ),
                "egress_refused",
            );
            e.extra["purpose"] = json!(Purpose::GitHost.as_str());
            return Err(e);
        }
        Ok((target, token))
    }

    /// The one way out to a git host's API: under `Purpose::GitHost`,
    /// pinned addresses, no redirect followed, the timeout, a bounded
    /// answer. A `POST` when the call has a body, else a read. Answers the
    /// host's status and what it said (`Null` when that is not JSON).
    async fn call_host(&self, call: &Call) -> Result<(u16, Value), NotOpened> {
        let origin = crate::egress::url_origin(&call.url);
        let timeout = self.host_timeout();
        let slow = || {
            NotOpened::new(
                "timeout",
                format!(
                    "the git host did not answer within {} s; whether it opened the pull request is not known, look at the host",
                    timeout.as_secs_f32()
                ),
            )
        };
        let pinned = self
            .egress
            .resolve(&call.url, Purpose::GitHost)
            .await
            .map_err(|r| {
                NotOpened::new(
                    "egress_refused",
                    format!("the outbound policy refused {origin}: {r}"),
                )
            })?;
        let client = self
            .egress
            .client(&pinned, timeout)
            .map_err(|e| NotOpened::new("unreachable", format!("no client for {origin}: {e}")))?;
        let mut request = match &call.body {
            Some(body) => client.post(&call.url).json(body),
            None => client.get(&call.url),
        };
        for (name, value) in &call.headers {
            request = request.header(*name, value.as_str());
        }
        let sent = async {
            let resp = request.send().await?;
            let status = resp.status();
            let bytes = read_bounded(resp).await?;
            Ok::<_, reqwest::Error>((status, bytes))
        };
        // The client's own timeout covers the whole call; the outer one is
        // the same bound whatever the client was doing.
        let (status, bytes) = match tokio::time::timeout(timeout, sent).await {
            Err(_) => return Err(slow()),
            Ok(Err(e)) if e.is_timeout() => return Err(slow()),
            Ok(Err(e)) => {
                return Err(NotOpened::new(
                    "unreachable",
                    format!(
                        "{origin} could not be reached: {}",
                        self.scrub(e.without_url().to_string().as_bytes())
                    ),
                ))
            }
            Ok(Ok(answer)) => answer,
        };
        let answer: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Ok((status.as_u16(), answer))
    }

    /// Ask the host to open the pull request. One call, no retry.
    pub(super) async fn open_at_host(
        &self,
        target: &Target,
        token: &str,
        pr: &PullRequest<'_>,
    ) -> Result<Opened, NotOpened> {
        let host = target.kind.host();
        let call = host.open(&target.api, token, pr);
        let (code, answer) = self.call_host(&call).await?;
        if (200..300).contains(&code) {
            let opened = host.opened(&answer);
            let opened = Opened {
                number: opened.number,
                url: opened.url.as_deref().and_then(clean_address),
            };
            if opened.number.is_none() && opened.url.is_none() {
                let mut n = NotOpened::new(
                    "bad_answer",
                    format!("the git host answered {code} without the pull request's number or address; look at the host, it may have opened one"),
                );
                n.status = Some(code);
                return Err(n);
            }
            return Ok(opened);
        }
        let (reason, why) = match code {
            300..=399 => (
                "redirected",
                format!("the git host's API answered a redirect ({code}), which is never followed: check git.host_api"),
            ),
            401 => (
                "token_refused",
                format!("the git host refused the token ({code}): set a valid one ({HOST_TOKEN_KEY})"),
            ),
            403 => (
                "not_permitted",
                format!("the token may not open a pull request in {} ({code}): give it the right to write pull requests", pr.repository),
            ),
            404 => (
                "not_found",
                format!("the git host's API does not know {} or the token may not see it ({code}): check git.host, git.host_api and the token's reach", pr.repository),
            ),
            429 => (
                "rate_limited",
                format!("the git host answered {code}: too many calls for now, try again later"),
            ),
            400 | 409 | 422 => (
                REFUSED,
                format!("the git host refused the pull request from {} into {} ({code}): a branch it does not know, nothing to merge, or one already open that was not found", pr.head, pr.base),
            ),
            _ => (
                "host_error",
                format!("the git host answered {code}"),
            ),
        };
        let mut n = NotOpened::new(reason, why);
        n.status = Some(code);
        n.host_message = host
            .message(&answer)
            .map(|m| self.scrub(clean(&m, MAX_HOST_MESSAGE).as_bytes()))
            .filter(|m| !m.is_empty());
        Err(n)
    }

    /// The pull request that is already open from `pr`'s head into its
    /// base, when the host has one: one read of the host's API, made only
    /// after the host refused to open one ([`NotOpened::may_be_open`]).
    /// Whatever goes wrong with the read (the policy, the token, a slow
    /// host, an answer that does not read) is "none found": the refusal
    /// stands as it was.
    pub(super) async fn existing_at_host(
        &self,
        target: &Target,
        token: &str,
        pr: &PullRequest<'_>,
    ) -> Option<Opened> {
        let host = target.kind.host();
        let call = host.existing(&target.api, token, pr);
        let (code, answer) = self.call_host(&call).await.ok()?;
        if !(200..300).contains(&code) {
            return None;
        }
        let found = host.found(&answer, pr)?;
        let found = Opened {
            number: found.number,
            url: found.url.as_deref().and_then(clean_address),
        };
        (found.number.is_some() || found.url.is_some()).then_some(found)
    }

    /// The `source_control_host` check of the diagnostics: a git host that
    /// is named and could not be asked to open a pull request. `None` when
    /// there is nothing to say: no repository, `git.host` is `none`, or
    /// `auto` with a repository on no public host. Never the token.
    pub fn host_diagnostics(&self) -> Option<(&'static str, String, Option<&'static str>)> {
        let setup = self.host_setup();
        self.sync_host_egress(&setup);
        let named = self
            .settings
            .string(HOST_KEY)
            .is_some_and(|s| Kind::named(&s).is_some());
        match setup {
            Setup::Off(_) => None,
            Setup::Problem(kind, why) => Some((
                "warn",
                format!(
                    "git.host names a git host{} and a pull request cannot be opened there: {why}",
                    kind.map(|k| format!(" ({})", k.as_str())).unwrap_or_default()
                ),
                Some("set git.host_api to the base URL of the host's API (https), check that git.remote reads as a repository at the host (owner/name), or set git.host to none to open pull requests by hand (Management, Source control)"),
            )),
            Setup::Ready(t) => {
                let origin = crate::egress::url_origin(&t.api);
                if t.token_from.is_none() {
                    // `auto` recognised a public host and nobody set a
                    // token: nothing was asked for, so nothing is wrong.
                    if !named && self.settings.string(HOST_API_KEY).is_none() {
                        return Some((
                            "ok",
                            format!(
                                "the repository is on a git host this instance knows ({}) and no token is set for its API: a pull request is opened by hand ({HOST_TOKEN_KEY} lets the host open it)",
                                t.kind.as_str()
                            ),
                            None,
                        ));
                    }
                    return Some((
                        "warn",
                        format!(
                            "a git host is named ({}, {origin}) and a pull request cannot be opened there: {}",
                            t.kind.as_str(),
                            no_token()
                        ),
                        Some("set the secret git.host_token to a token that may open pull requests in the repository (Management, Source control, or rustybin settings set-secret git.host_token), or set git.host to none to open them by hand"),
                    ));
                }
                if let Some(reasons) = self.egress_refusal(&t) {
                    return Some((
                        "warn",
                        format!(
                            "a git host is named ({}) and the outbound policy refuses its API ({origin}): {reasons}",
                            t.kind.as_str()
                        ),
                        Some("allow the API's host in the outbound policy (Management, Network: egress.allow, and egress.allow_loopback for a host on this machine), or correct git.host_api; GET /_rustybin/egress/check with purpose git_host says what is refused"),
                    ));
                }
                Some((
                    "ok",
                    format!(
                        "a pull request can be opened at the git host ({}, {origin})",
                        t.kind.as_str()
                    ),
                    None,
                ))
            }
        }
    }

    /// Why the outbound policy refuses the API on a dry check, when it does.
    fn egress_refusal(&self, target: &Target) -> Option<String> {
        let check = self.egress.dry_check(&target.api, Purpose::GitHost);
        if check["allowed"] == json!(true) {
            return None;
        }
        let reasons: Vec<&str> = check["reasons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        Some(reasons.join("; "))
    }

    fn host_timeout(&self) -> Duration {
        Duration::from_millis(
            self.host_timeout_ms
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Tests wait less for a host that does not answer.
    #[cfg(test)]
    pub(crate) fn set_host_timeout(&self, timeout: Duration) {
        self.host_timeout_ms.store(
            u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

fn no_token() -> String {
    format!(
        "no token for the git host's API: set {HOST_TOKEN_KEY} (the remote's token, {TOKEN_KEY}, is used only when it is the remote's authentication and the API is the remote's own host)"
    )
}
