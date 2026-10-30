//! AI agents: who talks to the instance through the management MCP server,
//! what they did, and the operator's controls over them.
//!
//! Every `tools/list` and `tools/call` on `/_rustybin/mcp` is attributed to
//! an agent: the caller's credential (the admin token, a console sign-in
//! token, a control JWT, an OAuth client, an API key, a visit, or nobody on
//! an open control plane) plus the client's name from `clientInfo` (the
//! session's `initialize`, or `_meta` of a stateless call). Two clients on
//! the same token are two agents. Each record keeps per-tool outcomes, the
//! last errors, the open sessions and the operator's rights:
//!
//! - a ceiling (`read-only`, `traffic`, `manage`, `full`) below what the
//!   credential allows, and tool allow and deny lists, from the agent's own
//!   override, else the first matching rule of `rustybin.yaml`'s `agents:`
//!   section, else the default (`agents.default_ceiling`);
//! - revocation: every call is refused, its sessions end (and answer `403`),
//!   and its tokens are refused for their remaining life (console and OAuth
//!   token ids, the OAuth client's subject). The admin token cannot be
//!   revoked per agent: the operator rotates it.
//!
//! Bounded: [`MAX_AGENTS`] records (least recently seen evicted first,
//! records with an override or a revocation last), per-record caps on
//! errors, sessions and token ids, a bounded session index and a bounded
//! event fan-out. Rights, notes, revocations and the call totals survive a
//! restart in `<state>/agents.json`; sessions and errors are memory only.

pub mod api;
pub mod tools;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

use super::access::Right;
use super::tools::{ToolDef, TOOLS};
use crate::limits::Clock;
use crate::mcp::sessions::CancelToken;
use crate::settings::{Kind, Settings, Spec};
use crate::state::AppState;

pub const FILE: &str = "agents.json";
pub const MAX_AGENTS: usize = 512;
pub const MAX_ERRORS: usize = 20;
pub const MAX_SESSIONS: usize = 32;
pub const MAX_TOKENS: usize = 16;
/// Session id to agent (the `403` of a revoked agent's session).
pub const MAX_SESSION_INDEX: usize = 4096;
pub const MAX_RULES: usize = 64;
pub const MAX_TOOL_LIST: usize = 128;
pub const MAX_NOTE: usize = 500;
pub const MAX_NAME: usize = 128;
pub const MAX_VERSION: usize = 64;
pub const MAX_ERROR_TEXT: usize = 300;
/// Live feed subscribers at once.
pub const MAX_LISTENERS: usize = 16;
/// Seen within this long, or with a session open: active.
pub const ACTIVE_WINDOW_SECS: i64 = 15 * 60;
const FLUSH_EVERY: Duration = Duration::from_secs(30);

pub const DEFAULT_CEILING_KEY: &str = "agents.default_ceiling";
pub const CEILINGS: &[&str] = &["read-only", "traffic", "manage", "full"];

/// The widest set of tools an agent may use, below its credential's rights.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ceiling {
    /// Read tools, not captured traffic.
    ReadOnly,
    /// Every read tool, captured traffic included.
    Traffic,
    /// Reads and mock changes; nothing instance-global (admin, the visit
    /// operator tools, the harness).
    Manage,
    /// Whatever the credential allows.
    #[default]
    Full,
}

impl Ceiling {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "read-only" => Some(Self::ReadOnly),
            "traffic" => Some(Self::Traffic),
            "manage" => Some(Self::Manage),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Traffic => "traffic",
            Self::Manage => "manage",
            Self::Full => "full",
        }
    }

    pub fn allows(self, def: &ToolDef) -> bool {
        match self {
            Self::ReadOnly => def.read_only && def.right != Right::Traffic,
            Self::Traffic => def.read_only,
            Self::Manage => !matches!(def.right, Right::Admin | Right::Operator | Right::Harness),
            Self::Full => true,
        }
    }
}

// ── Credentials ─────────────────────────────────────────────────────

/// What an agent authenticates with (never the secret itself).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Credential {
    /// `RUSTYBIN_ADMIN_TOKEN`.
    Admin,
    /// A console sign-in token (password mode), by user name.
    Console { user: String },
    /// A control JWT a platform issued (`RUSTYBIN_CONTROL_JWT_PUBLIC_KEY`).
    Jwt { sub: String, scopes: Vec<String> },
    /// A token of the built-in OAuth server, by its client's subject.
    Oauth { sub: String },
    /// An API key (shared mode).
    Key { id: String },
    /// A self-service visit's manage token.
    Visit { fingerprint: String },
    /// No credential: an open control plane, by the caller's session label.
    Open { session: String },
}

impl Credential {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Console { .. } => "console",
            Self::Jwt { .. } => "jwt",
            Self::Oauth { .. } => "oauth",
            Self::Key { .. } => "key",
            Self::Visit { .. } => "visit",
            Self::Open { .. } => "open",
        }
    }

    /// The part of the identity the credential contributes.
    fn fingerprint(&self) -> String {
        match self {
            Self::Admin => "admin".into(),
            Self::Console { user } => format!("console:{user}"),
            Self::Jwt { sub, .. } => format!("jwt:{sub}"),
            Self::Oauth { sub } => format!("oauth:{sub}"),
            Self::Key { id } => format!("key:{id}"),
            Self::Visit { fingerprint } => format!("visit:{fingerprint}"),
            Self::Open { session } => format!("open:{session}"),
        }
    }

    /// A short label for lists.
    pub fn label(&self) -> String {
        match self {
            Self::Admin => "admin token".into(),
            Self::Console { user } => format!("console sign-in ({user})"),
            Self::Jwt { sub, .. } => format!("control JWT ({sub})"),
            Self::Oauth { sub } => format!("OAuth ({sub})"),
            Self::Key { id } => format!("API key {id}"),
            Self::Visit { fingerprint } => format!("visit {fingerprint}"),
            Self::Open { session } => format!("no credential ({session})"),
        }
    }

    /// Can the operator revoke this credential's tokens? (The admin token,
    /// API keys, visits and open callers have none to revoke here.)
    pub fn revocable(&self) -> bool {
        matches!(
            self,
            Self::Console { .. } | Self::Jwt { .. } | Self::Oauth { .. }
        )
    }
}

/// The agent id: a digest of the credential and the client's name.
pub fn agent_id(credential: &Credential, name: &str) -> String {
    let digest = Sha256::digest(format!("{}\n{name}", credential.fingerprint()).as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Free text from a client: control characters dropped, trimmed, capped.
pub fn clean(raw: &str, max: usize) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

// ── Rights ──────────────────────────────────────────────────────────

/// The operator's override for one agent (each field optional: what is
/// left out comes from the matching rule or the default).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rights {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ceiling: Option<Ceiling>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deny: Option<Vec<String>>,
}

impl Rights {
    pub fn is_empty(&self) -> bool {
        self.ceiling.is_none() && self.allow.is_none() && self.deny.is_none()
    }
}

/// Tool names must name management tools.
pub fn check_tools(list: &[String], what: &str) -> Result<(), String> {
    if list.len() > MAX_TOOL_LIST {
        return Err(format!("{what}: at most {MAX_TOOL_LIST} tools"));
    }
    for t in list {
        if !TOOLS.iter().any(|d| d.name == t) {
            return Err(format!("{what}: {t:?} is not a management tool"));
        }
    }
    Ok(())
}

fn dedup(list: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in list {
        let t = t.trim().to_string();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// What applies to an agent now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effective {
    pub ceiling: Ceiling,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub note: Option<String>,
    /// `agent` (its override), `rule` (of the config file) or `default`.
    pub source: &'static str,
    /// The index of the matching rule, when one matched.
    pub rule: Option<usize>,
}

impl Effective {
    pub fn allows(&self, def: &ToolDef) -> bool {
        self.ceiling.allows(def)
            && (self.allow.is_empty() || self.allow.iter().any(|t| t == def.name))
            && !self.deny.iter().any(|t| t == def.name)
    }

    /// Why the operator's rights refuse `def` (for the tool error).
    pub fn denial(&self, def: &ToolDef) -> String {
        if self.deny.iter().any(|t| t == def.name) {
            return format!("the operator denied {} to this agent", def.name);
        }
        if !self.allow.is_empty() && !self.allow.iter().any(|t| t == def.name) {
            return format!(
                "the operator allows this agent only some tools, and {} is not one of them",
                def.name
            );
        }
        format!(
            "the operator limited this agent to {} tools",
            self.ceiling.as_str()
        )
    }

    pub fn view(&self) -> Value {
        json!({
            "ceiling": self.ceiling.as_str(),
            "allow": self.allow,
            "deny": self.deny,
            "source": self.source,
            "rule": self.rule,
        })
    }
}

// ── The config file's `agents:` section ─────────────────────────────

/// `agents:` in `rustybin.yaml`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_ceiling: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<FileRule>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRule {
    #[serde(rename = "match", default)]
    pub matcher: Match,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Which agents a rule is for. `name` matches the client's name, case
/// insensitive, with `*` as a wildcard; `credential` is a kind (`admin`,
/// `console`, `jwt`, `oauth`, `key`, `visit`, `open`). Both empty: every
/// agent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Match {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

pub const CREDENTIAL_KINDS: &[&str] = &["admin", "console", "jwt", "oauth", "key", "visit", "open"];

/// A validated rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub name: Option<String>,
    pub credential: Option<String>,
    pub ceiling: Option<Ceiling>,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub note: Option<String>,
}

/// The usable part of a file's `agents:` section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilePolicy {
    pub default_ceiling: Option<Ceiling>,
    pub rules: Vec<Rule>,
}

/// Validate a file's `agents:` section: the usable policy and a problem
/// line per rule that is not.
pub fn file_policy(section: Option<FileSection>) -> (FilePolicy, Vec<String>) {
    let mut out = FilePolicy::default();
    let mut problems = Vec::new();
    let Some(section) = section else {
        return (out, problems);
    };
    if let Some(c) = &section.default_ceiling {
        match Ceiling::parse(c) {
            Some(c) => out.default_ceiling = Some(c),
            None => problems.push(format!(
                "agents.default_ceiling: {c:?} is not one of {} (ignored)",
                CEILINGS.join(", ")
            )),
        }
    }
    for (i, r) in section.rules.into_iter().enumerate() {
        if out.rules.len() >= MAX_RULES {
            problems.push(format!(
                "agents.rules: more than {MAX_RULES} rules (the rest are skipped)"
            ));
            break;
        }
        match rule(r) {
            Ok(r) => out.rules.push(r),
            Err(e) => problems.push(format!("agents.rules[{i}]: {e} (skipped)")),
        }
    }
    (out, problems)
}

fn rule(r: FileRule) -> Result<Rule, String> {
    let name = r
        .matcher
        .name
        .map(|n| clean(&n, MAX_NAME))
        .filter(|n| !n.is_empty());
    let credential = match r.matcher.credential {
        None => None,
        Some(c) if CREDENTIAL_KINDS.contains(&c.trim()) => Some(c.trim().to_string()),
        Some(c) => {
            return Err(format!(
                "match.credential {c:?} is not one of {}",
                CREDENTIAL_KINDS.join(", ")
            ))
        }
    };
    let ceiling = match r.ceiling {
        None => None,
        Some(c) => Some(
            Ceiling::parse(&c)
                .ok_or_else(|| format!("ceiling {c:?} is not one of {}", CEILINGS.join(", ")))?,
        ),
    };
    let allow = dedup(r.allow);
    let deny = dedup(r.deny);
    check_tools(&allow, "allow")?;
    check_tools(&deny, "deny")?;
    let note = r
        .note
        .map(|n| clean(&n, MAX_NOTE))
        .filter(|n| !n.is_empty());
    Ok(Rule {
        name,
        credential,
        ceiling,
        allow,
        deny,
        note,
    })
}

/// `*` wildcards, case insensitive.
pub fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] != '*' && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

impl Rule {
    pub fn matches(&self, name: &str, credential: &Credential) -> bool {
        self.name.as_deref().is_none_or(|p| glob(p, name))
            && self
                .credential
                .as_deref()
                .is_none_or(|c| c == credential.kind())
    }

    pub fn view(&self) -> Value {
        json!({
            "match": { "name": self.name, "credential": self.credential },
            "ceiling": self.ceiling.map(Ceiling::as_str),
            "allow": self.allow,
            "deny": self.deny,
            "note": self.note,
        })
    }
}

/// The default ceiling and the rules, as they apply now.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub default_ceiling: Ceiling,
    /// `stored`, `env`, `file` or `default`.
    pub default_source: &'static str,
    pub rules: Vec<Rule>,
}

impl Policy {
    /// The setting when saved or set by its variable, else the file's
    /// `default_ceiling`, else `full`.
    pub fn of(state: &AppState) -> Self {
        let file = state.mocks.file_agents();
        let (value, source) = state.settings.get(DEFAULT_CEILING_KEY);
        let set = Ceiling::parse(value.as_str().unwrap_or(""));
        let (default_ceiling, default_source) = match (source, set, file.default_ceiling) {
            (crate::settings::Source::Stored, Some(c), _) => (c, "stored"),
            (crate::settings::Source::Env, Some(c), _) => (c, "env"),
            (_, _, Some(c)) => (c, "file"),
            (_, Some(c), None) => (c, "default"),
            _ => (Ceiling::Full, "default"),
        };
        Self {
            default_ceiling,
            default_source,
            rules: file.rules,
        }
    }

    /// The rights of an agent: its override field by field, else the
    /// first matching rule's, else the default.
    pub fn effective(&self, rec: &Record) -> Effective {
        let matched = self
            .rules
            .iter()
            .enumerate()
            .find(|(_, r)| r.matches(&rec.name, &rec.credential));
        let rule = matched.map(|(_, r)| r);
        let o = rec.rights.as_ref();
        let source = if o.is_some_and(|o| !o.is_empty()) || rec.note.is_some() {
            "agent"
        } else if rule.is_some() {
            "rule"
        } else {
            "default"
        };
        Effective {
            ceiling: o
                .and_then(|o| o.ceiling)
                .or(rule.and_then(|r| r.ceiling))
                .unwrap_or(self.default_ceiling),
            allow: o
                .and_then(|o| o.allow.clone())
                .or_else(|| rule.map(|r| r.allow.clone()))
                .unwrap_or_default(),
            deny: o
                .and_then(|o| o.deny.clone())
                .or_else(|| rule.map(|r| r.deny.clone()))
                .unwrap_or_default(),
            note: rec
                .note
                .clone()
                .or_else(|| rule.and_then(|r| r.note.clone())),
            source,
            rule: matched.map(|(i, _)| i),
        }
    }

    pub fn view(&self) -> Value {
        json!({
            "default_ceiling": self.default_ceiling.as_str(),
            "default_source": self.default_source,
            "rules": self.rules.iter().map(Rule::view).collect::<Vec<_>>(),
        })
    }
}

/// The settings this feature registers.
pub fn specs() -> Vec<Spec> {
    vec![Spec::new(
        DEFAULT_CEILING_KEY,
        "agents",
        "Default ceiling for AI agents",
        "The most an agent on the management MCP server may do unless a rule of rustybin.yaml or its own rights say otherwise: read-only (read tools, no captured traffic), traffic (every read tool), manage (reads and mock changes), full (whatever its credential allows).",
        Kind::Choice { options: CEILINGS },
        json!("full"),
    )
    .env("RUSTYBIN_AGENTS_DEFAULT_CEILING", |c| {
        c.agents_default_ceiling.clone().map(Value::String)
    })]
}

// ── Records ─────────────────────────────────────────────────────────

/// Outcomes of one tool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Counts {
    pub ok: u64,
    pub error: u64,
    pub denied: u64,
    pub asked: u64,
}

impl Counts {
    pub fn total(&self) -> u64 {
        self.ok + self.error + self.denied + self.asked
    }

    fn add(&mut self, outcome: &str) {
        match outcome {
            "ok" => self.ok += 1,
            "error" => self.error += 1,
            "denied" => self.denied += 1,
            _ => self.asked += 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocation {
    pub at: DateTime<Utc>,
    pub by: String,
}

#[derive(Clone, Debug)]
struct ErrorEntry {
    at: DateTime<Utc>,
    tool: String,
    message: String,
}

#[derive(Clone)]
struct SessionRef {
    id: String,
    opened: DateTime<Utc>,
    closed: CancelToken,
}

/// One agent. The serialised part is what `agents.json` keeps.
#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub credential: Credential,
    #[serde(default)]
    pub transport: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    #[serde(default)]
    pub calls: BTreeMap<String, Counts>,
    #[serde(default)]
    pub lists: u64,
    #[serde(default)]
    pub today: Option<(NaiveDate, u64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rights: Option<Rights>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked: Option<Revocation>,
    #[serde(skip)]
    errors: VecDeque<ErrorEntry>,
    #[serde(skip)]
    sessions: Vec<SessionRef>,
    /// Token ids seen (with their expiry): revoked with the agent.
    #[serde(skip)]
    tokens: VecDeque<(String, i64)>,
    /// Token ids this agent's revocation added to the denylist.
    #[serde(skip)]
    revoked_tokens: Vec<String>,
}

impl Record {
    fn pinned(&self) -> bool {
        self.revoked.is_some() || self.rights.is_some() || self.note.is_some()
    }

    fn sessions_open(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| !s.closed.is_cancelled())
            .count()
    }

    fn total(&self) -> Counts {
        let mut t = Counts::default();
        for c in self.calls.values() {
            t.ok += c.ok;
            t.error += c.error;
            t.denied += c.denied;
            t.asked += c.asked;
        }
        t
    }

    fn calls_today(&self, today: NaiveDate) -> u64 {
        match self.today {
            Some((d, n)) if d == today => n,
            _ => 0,
        }
    }

    fn status(&self, now: DateTime<Utc>) -> &'static str {
        if self.revoked.is_some() {
            "revoked"
        } else if self.sessions_open() > 0
            || (now - self.last_seen).num_seconds() < ACTIVE_WINDOW_SECS
        {
            "active"
        } else {
            "idle"
        }
    }
}

/// What a request tells about its agent.
pub struct Seen {
    pub credential: Credential,
    /// The presented token's id and expiry, when it has one.
    pub token: Option<(String, i64)>,
    pub name: String,
    pub version: String,
    /// `streamable-2026`, `streamable-2025` or `stdio`.
    pub transport: &'static str,
    pub session: Option<(String, CancelToken)>,
}

/// Attribute a management call to its agent: the caller's credential, the
/// client's name (the session's `initialize`, else `_meta` of a stateless
/// call), the transport and the session.
pub fn observe_call(
    ctx: &crate::mcp::core::CallCtx,
    access: &super::Access,
    params: &Value,
) -> Snapshot {
    let info = match &ctx.session {
        Some(s) => s.state().client_info.clone(),
        None => params
            .get("_meta")
            .and_then(|m| m.get(crate::mcp::protocol::META_CLIENT_INFO))
            .cloned()
            .unwrap_or(Value::Null),
    };
    let field = |k: &str, max: usize| {
        info.get(k)
            .and_then(Value::as_str)
            .map(|v| clean(v, max))
            .unwrap_or_default()
    };
    let transport = if access.stdio {
        "stdio"
    } else if ctx.session.is_some() {
        "streamable-2025"
    } else {
        "streamable-2026"
    };
    let seen = Seen {
        credential: access.credential.clone(),
        token: access.token.clone(),
        name: field("name", MAX_NAME),
        version: field("version", MAX_VERSION),
        transport,
        session: ctx
            .session
            .as_ref()
            .map(|s| (s.id.clone(), s.closed.clone())),
    };
    ctx.app.agents.observe(seen, &Policy::of(&ctx.app))
}

/// The agent behind a call, as the tools see it.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub id: String,
    pub revoked: bool,
    pub rights: Effective,
}

/// Why an operator action was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum AgentError {
    NotFound,
    Invalid(String),
    Io(String),
}

#[derive(Default, Serialize, Deserialize)]
struct Doc {
    version: u32,
    #[serde(default)]
    agents: Vec<Record>,
}

struct Inner {
    records: HashMap<String, Record>,
    /// Session id to agent id, in insertion order.
    sessions: HashMap<String, String>,
    session_order: VecDeque<String>,
}

/// The registry (`AppState.agents`).
pub struct Agents {
    inner: Mutex<Inner>,
    path: Option<PathBuf>,
    clock: Arc<dyn Clock>,
    events: broadcast::Sender<Value>,
    listeners: Arc<AtomicUsize>,
    last_flush: Mutex<Option<Instant>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Holds a live feed slot until dropped.
pub struct Listener(Arc<AtomicUsize>);

impl Drop for Listener {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Agents {
    /// Load `<state>/agents.json` when a state directory is set (a broken
    /// file warns and starts empty). Revoked OAuth clients are refused
    /// again at once.
    pub fn new(settings: &Settings, tokens: &crate::login::Tokens, clock: Arc<dyn Clock>) -> Self {
        let path = settings.state_file(FILE);
        let doc = path
            .as_ref()
            .filter(|p| p.exists())
            .and_then(|p| {
                match std::fs::read(p)
                    .map_err(|e| e.to_string())
                    .and_then(|b| serde_json::from_slice::<Doc>(&b).map_err(|e| e.to_string()))
                {
                    Ok(doc) => Some(doc),
                    Err(e) => {
                        tracing::warn!(
                            "agents: {} is not readable ({e}); starting empty",
                            p.display()
                        );
                        None
                    }
                }
            })
            .unwrap_or_default();
        let mut records = HashMap::new();
        let now = clock.wall().timestamp();
        for mut r in doc.agents.into_iter().take(MAX_AGENTS) {
            if r.id != agent_id(&r.credential, &r.name) {
                continue;
            }
            r.calls.retain(|t, _| TOOLS.iter().any(|d| d.name == t));
            if let (Some(_), Credential::Oauth { sub }) = (&r.revoked, &r.credential) {
                tokens.revoke_subject(sub, now + crate::control_oauth::REFRESH_TTL_SECS);
            }
            records.insert(r.id.clone(), r);
        }
        let (events, _) = broadcast::channel(256);
        Self {
            inner: Mutex::new(Inner {
                records,
                sessions: HashMap::new(),
                session_order: VecDeque::new(),
            }),
            path,
            clock,
            events,
            listeners: Arc::new(AtomicUsize::new(0)),
            last_flush: Mutex::new(None),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.wall()
    }

    pub fn persisted(&self) -> bool {
        self.path.is_some()
    }

    pub fn len(&self) -> usize {
        lock(&self.inner).records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Attribute a request to its agent (created on sight) and return what
    /// applies to it.
    pub fn observe(&self, seen: Seen, policy: &Policy) -> Snapshot {
        let now = self.now();
        let name = if seen.name.is_empty() {
            "unknown".to_string()
        } else {
            seen.name
        };
        let id = agent_id(&seen.credential, &name);
        let mut inner = lock(&self.inner);
        if !inner.records.contains_key(&id) {
            evict(&mut inner.records);
            inner.records.insert(
                id.clone(),
                Record {
                    id: id.clone(),
                    name: name.clone(),
                    version: String::new(),
                    credential: seen.credential.clone(),
                    transport: String::new(),
                    first_seen: now,
                    last_seen: now,
                    calls: BTreeMap::new(),
                    lists: 0,
                    today: None,
                    rights: None,
                    note: None,
                    revoked: None,
                    errors: VecDeque::new(),
                    sessions: Vec::new(),
                    tokens: VecDeque::new(),
                    revoked_tokens: Vec::new(),
                },
            );
            tracing::info!(target: "rustybin::agents", agent = %id, name = %name, credential = seen.credential.kind(), "new agent");
        }
        let mut index_session = None;
        let snapshot = {
            let Some(rec) = inner.records.get_mut(&id) else {
                return Snapshot {
                    id,
                    revoked: false,
                    rights: policy.effective_default(),
                };
            };
            rec.last_seen = now;
            if !seen.version.is_empty() {
                rec.version = seen.version;
            }
            rec.transport = seen.transport.to_string();
            // A newer token of the same credential (a fresh sign-in).
            if let Credential::Jwt { scopes, .. } = &seen.credential {
                if let Credential::Jwt { scopes: s, .. } = &mut rec.credential {
                    s.clone_from(scopes);
                }
            }
            if let Some((jti, exp)) = seen.token {
                if !rec.tokens.iter().any(|(j, _)| *j == jti) {
                    rec.tokens.push_back((jti, exp));
                    while rec.tokens.len() > MAX_TOKENS {
                        rec.tokens.pop_front();
                    }
                }
            }
            if let Some((sid, closed)) = seen.session {
                rec.sessions.retain(|s| !s.closed.is_cancelled());
                if !rec.sessions.iter().any(|s| s.id == sid) {
                    rec.sessions.push(SessionRef {
                        id: sid.clone(),
                        opened: now,
                        closed,
                    });
                    while rec.sessions.len() > MAX_SESSIONS {
                        rec.sessions.remove(0);
                    }
                    index_session = Some(sid);
                }
            }
            Snapshot {
                id: id.clone(),
                revoked: rec.revoked.is_some(),
                rights: policy.effective(rec),
            }
        };
        if let Some(sid) = index_session {
            if !inner.sessions.contains_key(&sid) {
                inner.session_order.push_back(sid.clone());
                while inner.session_order.len() > MAX_SESSION_INDEX {
                    if let Some(old) = inner.session_order.pop_front() {
                        inner.sessions.remove(&old);
                    }
                }
            }
            inner.sessions.insert(sid, id);
        }
        snapshot
    }

    /// A `tools/list` of an agent.
    pub fn record_list(&self, id: &str) {
        if let Some(r) = lock(&self.inner).records.get_mut(id) {
            r.lists += 1;
        }
    }

    /// A `tools/call` of an agent and its outcome (`ok`, `error`, `denied`
    /// or `asked`); `error` carries the tool's message.
    pub fn record_call(&self, id: &str, tool: &str, outcome: &str, error: Option<&str>) {
        let now = self.now();
        let today = now.date_naive();
        let event = {
            let mut inner = lock(&self.inner);
            let Some(r) = inner.records.get_mut(id) else {
                return;
            };
            r.calls.entry(tool.to_string()).or_default().add(outcome);
            r.today = Some((today, r.calls_today(today) + 1));
            if let Some(msg) = error {
                r.errors.push_back(ErrorEntry {
                    at: now,
                    tool: tool.to_string(),
                    message: clean(msg, MAX_ERROR_TEXT),
                });
                while r.errors.len() > MAX_ERRORS {
                    r.errors.pop_front();
                }
            }
            json!({
                "at": now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                "agent": r.id,
                "name": r.name,
                "credential": r.credential.kind(),
                "tool": tool,
                "outcome": outcome,
            })
        };
        // Nobody listening is fine.
        let _ = self.events.send(event);
        self.flush_soon();
    }

    /// Is `session_id` a session of a revoked agent?
    pub fn session_revoked(&self, session_id: &str) -> bool {
        let inner = lock(&self.inner);
        inner
            .sessions
            .get(session_id)
            .and_then(|id| inner.records.get(id))
            .is_some_and(|r| r.revoked.is_some())
    }

    /// Subscribe to the live feed (`None` when [`MAX_LISTENERS`] listen).
    pub fn subscribe(&self) -> Option<(broadcast::Receiver<Value>, Listener)> {
        let n = self.listeners.fetch_add(1, Ordering::Relaxed);
        let guard = Listener(self.listeners.clone());
        if n >= MAX_LISTENERS {
            return None;
        }
        Some((self.events.subscribe(), guard))
    }

    // ── Operator actions ──

    /// Every agent, most recently seen first.
    pub fn list(&self, policy: &Policy) -> Vec<Value> {
        let now = self.now();
        let inner = lock(&self.inner);
        let mut all: Vec<&Record> = inner.records.values().collect();
        all.sort_by(|a, b| b.last_seen.cmp(&a.last_seen).then(a.id.cmp(&b.id)));
        all.into_iter().map(|r| summary(r, policy, now)).collect()
    }

    /// One agent with its tools, errors and sessions.
    pub fn get(&self, id: &str, policy: &Policy) -> Option<Value> {
        let now = self.now();
        let inner = lock(&self.inner);
        let r = inner.records.get(id)?;
        let mut v = summary(r, policy, now);
        let eff = policy.effective(r);
        v["tools"] = Value::Array(
            r.calls
                .iter()
                .map(|(t, c)| json!({ "name": t, "ok": c.ok, "error": c.error, "denied": c.denied, "asked": c.asked, "total": c.total() }))
                .collect(),
        );
        v["last_errors"] = Value::Array(
            r.errors
                .iter()
                .rev()
                .map(|e| json!({ "at": e.at.to_rfc3339(), "tool": e.tool, "message": e.message }))
                .collect(),
        );
        v["sessions"] = Value::Array(
            r.sessions
                .iter()
                .map(|s| {
                    json!({
                        "id": s.id.chars().take(8).collect::<String>(),
                        "opened": s.opened.to_rfc3339(),
                        "open": !s.closed.is_cancelled(),
                    })
                })
                .collect(),
        );
        v["override"] = json!(r.rights);
        v["available_tools"] = Value::Array(
            TOOLS
                .iter()
                .map(|d| {
                    json!({
                        "name": d.name,
                        "title": d.title,
                        "right": d.right.as_str(),
                        "read_only": d.read_only,
                        "allowed": eff.allows(d),
                    })
                })
                .collect(),
        );
        Some(v)
    }

    /// Set (or clear, `None`) an agent's rights override, and its note.
    pub fn set_rights(
        &self,
        id: &str,
        rights: Option<Option<Rights>>,
        note: Option<Option<String>>,
    ) -> Result<(), AgentError> {
        let rights = match rights {
            Some(Some(mut r)) => {
                r.allow = r.allow.map(dedup);
                r.deny = r.deny.map(dedup);
                if let Some(a) = &r.allow {
                    check_tools(a, "allow").map_err(AgentError::Invalid)?;
                }
                if let Some(d) = &r.deny {
                    check_tools(d, "deny").map_err(AgentError::Invalid)?;
                }
                Some(Some(r).filter(|r| !r.is_empty()))
            }
            other => other,
        };
        let note = note.map(|n| n.map(|n| clean(&n, MAX_NOTE)).filter(|n| !n.is_empty()));
        {
            let mut inner = lock(&self.inner);
            let r = inner.records.get_mut(id).ok_or(AgentError::NotFound)?;
            if let Some(rights) = rights {
                r.rights = rights;
            }
            if let Some(note) = note {
                r.note = note;
            }
        }
        tracing::info!(target: "rustybin::agents", agent = id, "rights changed");
        self.save().map_err(AgentError::Io)
    }

    /// Revoke an agent: refuse its calls, end its sessions, revoke its
    /// tokens. Returns what was done.
    pub fn revoke(
        &self,
        id: &str,
        by: &str,
        tokens: &crate::login::Tokens,
    ) -> Result<Value, AgentError> {
        let now = self.now();
        let report = {
            let mut inner = lock(&self.inner);
            let r = inner.records.get_mut(id).ok_or(AgentError::NotFound)?;
            r.revoked = Some(Revocation {
                at: now,
                by: clean(by, 64),
            });
            let ended = end_sessions(r);
            let mut revoked = 0;
            if r.credential.revocable() {
                for (jti, exp) in &r.tokens {
                    if *exp > now.timestamp() {
                        tokens.revoke(jti, *exp);
                        if !r.revoked_tokens.contains(jti) {
                            r.revoked_tokens.push(jti.clone());
                        }
                        revoked += 1;
                    }
                }
            }
            if let Credential::Oauth { sub } = &r.credential {
                tokens.revoke_subject(
                    sub,
                    now.timestamp() + crate::control_oauth::REFRESH_TTL_SECS,
                );
            }
            json!({
                "id": r.id,
                "revoked": true,
                "sessions_ended": ended,
                "tokens_revoked": revoked,
                "credential": r.credential.kind(),
                "note": credential_note(&r.credential),
            })
        };
        tracing::info!(target: "rustybin::agents", agent = id, by, "agent revoked");
        let _ = self
            .events
            .send(json!({ "at": now.to_rfc3339(), "agent": id, "event": "revoked" }));
        self.save().map_err(AgentError::Io)?;
        Ok(report)
    }

    /// Undo a revocation (its token ids and its OAuth client are accepted
    /// again).
    pub fn restore(&self, id: &str, tokens: &crate::login::Tokens) -> Result<Value, AgentError> {
        let now = self.now();
        let report = {
            let mut inner = lock(&self.inner);
            let r = inner.records.get_mut(id).ok_or(AgentError::NotFound)?;
            let was = r.revoked.take().is_some();
            for jti in r.revoked_tokens.drain(..) {
                tokens.unrevoke(&jti);
            }
            if let Credential::Oauth { sub } = &r.credential {
                tokens.restore_subject(sub);
            }
            json!({ "id": r.id, "restored": was })
        };
        tracing::info!(target: "rustybin::agents", agent = id, "agent restored");
        let _ = self
            .events
            .send(json!({ "at": now.to_rfc3339(), "agent": id, "event": "restored" }));
        self.save().map_err(AgentError::Io)?;
        Ok(report)
    }

    /// End an agent's open sessions.
    pub fn end_sessions(&self, id: &str) -> Result<usize, AgentError> {
        let mut inner = lock(&self.inner);
        let r = inner.records.get_mut(id).ok_or(AgentError::NotFound)?;
        let n = end_sessions(r);
        tracing::info!(target: "rustybin::agents", agent = id, ended = n, "agent sessions ended");
        Ok(n)
    }

    /// The call log of an agent as an AI task's subject (no arguments were
    /// ever kept).
    pub fn activity(&self, id: &str, policy: &Policy) -> Option<Value> {
        let mut v = self.get(id, policy)?;
        if let Value::Object(m) = &mut v {
            m.remove("available_tools");
            m.remove("sessions");
        }
        Some(v)
    }

    fn flush_soon(&self) {
        if self.path.is_none() {
            return;
        }
        let due = lock(&self.last_flush).is_none_or(|t| t.elapsed() >= FLUSH_EVERY);
        if due {
            if let Err(e) = self.save() {
                tracing::warn!("agents: {e}");
            }
        }
    }

    /// Write `agents.json` (a no-op in memory).
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let doc = Doc {
            version: 1,
            agents: lock(&self.inner).records.values().cloned().collect(),
        };
        let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
        crate::settings::write_private(path, &bytes)?;
        *lock(&self.last_flush) = Some(Instant::now());
        Ok(())
    }
}

impl Policy {
    fn effective_default(&self) -> Effective {
        Effective {
            ceiling: self.default_ceiling,
            allow: Vec::new(),
            deny: Vec::new(),
            note: None,
            source: "default",
            rule: None,
        }
    }
}

fn end_sessions(r: &mut Record) -> usize {
    let n = r.sessions_open();
    for s in &r.sessions {
        s.closed.cancel();
    }
    n
}

/// Make room for one more record: the least recently seen goes, records
/// the operator configured or revoked last.
fn evict(records: &mut HashMap<String, Record>) {
    while records.len() >= MAX_AGENTS {
        let victim = records
            .values()
            .min_by_key(|r| (r.pinned(), r.last_seen))
            .map(|r| r.id.clone());
        match victim {
            Some(id) => {
                records.remove(&id);
            }
            None => break,
        }
    }
}

fn credential_note(c: &Credential) -> Option<&'static str> {
    match c {
        Credential::Admin => Some(
            "The admin token cannot be revoked per agent: rotate RUSTYBIN_ADMIN_TOKEN. This agent's calls are refused meanwhile.",
        ),
        Credential::Console { .. } => Some(
            "The console sign-in tokens this agent used are revoked: if it used your own console token, your console is signed out too.",
        ),
        Credential::Key { .. } => Some(
            "API keys are revoked in the key registry; this agent's calls on the management server are refused meanwhile.",
        ),
        Credential::Visit { .. } => Some("The visit continues; this agent's calls on the management server are refused."),
        Credential::Open { .. } => Some(
            "The control plane is open: anyone may connect again under another client name. Turn on the console sign-in to control agents for real.",
        ),
        Credential::Jwt { .. } | Credential::Oauth { .. } => None,
    }
}

fn summary(r: &Record, policy: &Policy, now: DateTime<Utc>) -> Value {
    let eff = policy.effective(r);
    let total = r.total();
    let mut cred = serde_json::to_value(&r.credential).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut cred {
        m.insert("label".into(), json!(r.credential.label()));
        m.insert("revocable".into(), json!(r.credential.revocable()));
    }
    json!({
        "id": r.id,
        "name": r.name,
        "version": r.version,
        "credential": cred,
        "transport": r.transport,
        "first_seen": r.first_seen.to_rfc3339(),
        "last_seen": r.last_seen.to_rfc3339(),
        "status": r.status(now),
        "calls": {
            "total": total.total(),
            "today": r.calls_today(now.date_naive()),
            "ok": total.ok,
            "error": total.error,
            "denied": total.denied,
            "asked": total.asked,
            "lists": r.lists,
        },
        "sessions_open": r.sessions_open(),
        "rights": eff.view(),
        "tools_allowed": TOOLS.iter().filter(|d| eff.allows(d)).count(),
        "note": eff.note,
        "revoked": r.revoked.as_ref().map(|v| json!({ "at": v.at.to_rfc3339(), "by": v.by })),
    })
}

#[cfg(test)]
mod tests;
