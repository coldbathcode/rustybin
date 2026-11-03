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

use super::tools::{ToolDef, TOOLS};
use crate::limits::Clock;
use crate::mcp::sessions::CancelToken;
use crate::rbac::Permission;
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
/// The default ceiling per role of the agent's user (`role=ceiling`
/// entries; the first entry whose role the user holds applies).
pub const DEFAULT_CEILING_FOR_KEY: &str = "agents.default_ceiling_for";
/// The 0.3 ceilings (a role name is a ceiling too).
pub const CEILINGS: &[&str] = &["read-only", "traffic", "manage", "full"];
/// Entries of `agents.default_ceiling_for`.
pub const MAX_DEFAULT_FOR: usize = 20;

/// The widest set of tools an agent may use, below what its credential
/// (its user) allows: one of the four 0.3 ceilings, which keep their 0.4
/// tool lists exactly, or a role (built-in or custom), which lets an agent
/// use a tool whose permission the role grants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Ceiling {
    /// Read tools, not captured traffic.
    ReadOnly,
    /// Every read tool, captured traffic included.
    Traffic,
    /// Reads and mock changes; nothing instance-global (admin, the visit
    /// operator tools, the harness).
    Manage,
    /// Whatever the credential allows (a user's agent: the user's own).
    #[default]
    Full,
    /// A role: the tools whose permission it grants (an unknown role, a
    /// custom role deleted since, allows nothing).
    Role(String),
}

/// Shaped like a role name (lower case letters, digits, `_` and `-`,
/// starting with a letter, at most 40 characters).
pub fn role_shaped(s: &str) -> bool {
    let mut chars = s.chars();
    s.len() <= crate::rbac::role::MAX_ROLE_NAME_CHARS
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

impl Ceiling {
    /// One of the four 0.3 ceilings.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "read-only" => Some(Self::ReadOnly),
            "traffic" => Some(Self::Traffic),
            "manage" => Some(Self::Manage),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    /// A 0.3 ceiling (its name wins over a custom role of the same name),
    /// else a role-shaped name. Whether the role exists is checked where a
    /// ceiling is set ([`Ceiling::check`]).
    pub fn parse_any(s: &str) -> Option<Self> {
        let s = s.trim();
        Self::parse(s).or_else(|| role_shaped(s).then(|| Self::Role(s.to_string())))
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Traffic => "traffic",
            Self::Manage => "manage",
            Self::Full => "full",
            Self::Role(r) => r,
        }
    }

    /// The role a 0.3 ceiling is nearest to (the console and the docs show
    /// it): `read-only` the viewer, `traffic` the viewer plus
    /// `traffic.read`, `manage` the editor, `full` the user's own.
    pub fn nearest_role(&self) -> Option<&'static str> {
        match self {
            Self::ReadOnly => Some("viewer"),
            Self::Traffic => Some("viewer + traffic.read"),
            Self::Manage => Some("editor"),
            Self::Full => Some("the owner's own"),
            Self::Role(_) => None,
        }
    }

    /// A ceiling that names a role must name one that exists.
    pub fn check(&self, users: &crate::rbac::Users) -> Result<(), String> {
        match self {
            Self::Role(r) if users.role_permissions(r).is_none() => Err(format!(
                "ceiling {r:?} is neither one of {} nor a role (list_roles, GET /_rustybin/roles)",
                CEILINGS.join(", ")
            )),
            _ => Ok(()),
        }
    }

    /// Does the ceiling let an agent use `def`? By the tool's permission
    /// and annotations: `read-only` is a read-only tool that does not need
    /// `traffic.read`, `traffic` any read-only tool, `manage` a tool whose
    /// permission is in [`MANAGE_CEILING`] (or a visit's own), `full`
    /// anything (the tool counts of the 0.4 ceilings,
    /// `ceilings_keep_their_tool_counts`). A role needs its permissions
    /// ([`Ceiling::allows_with`]); without them it allows nothing.
    pub fn allows(&self, def: &ToolDef) -> bool {
        self.allows_with(def, None)
    }

    /// [`Ceiling::allows`] given the permissions of the role a ceiling
    /// names (`None`: unknown, nothing allowed).
    pub fn allows_with(&self, def: &ToolDef, role: Option<crate::rbac::PermSet>) -> bool {
        match self {
            Self::ReadOnly => {
                def.read_only && def.permission.permission() != Some(Permission::TrafficRead)
            }
            Self::Traffic => def.read_only,
            Self::Manage => def
                .permission
                .permission()
                .is_none_or(|p| MANAGE_CEILING.contains(&p)),
            Self::Full => true,
            Self::Role(_) => {
                role.is_some_and(|perms| def.permission.permission().is_none_or(|p| perms.has(p)))
            }
        }
    }
}

impl Serialize for Ceiling {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Ceiling {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ceiling::parse_any(&s).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "ceiling {s:?} is not one of {} nor a role name",
                CEILINGS.join(", ")
            ))
        })
    }
}

/// Parse `agents.default_ceiling_for`: `role=ceiling` entries.
pub fn default_for_entries(list: &[String]) -> Result<Vec<(String, Ceiling)>, String> {
    let mut out = Vec::new();
    for e in list {
        let Some((role, ceiling)) = e.split_once('=') else {
            return Err(format!("{e:?}: expected role=ceiling (admin=editor)"));
        };
        let role = role.trim();
        if !role_shaped(role) {
            return Err(format!("{role:?} is not a role name"));
        }
        let Some(c) = Ceiling::parse_any(ceiling) else {
            return Err(format!(
                "{:?} is not one of {} nor a role name",
                ceiling.trim(),
                CEILINGS.join(", ")
            ));
        };
        out.push((role.to_string(), c));
    }
    Ok(out)
}

fn valid_default_for(v: &Value) -> Result<(), String> {
    let list: Vec<String> = v
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    default_for_entries(&list).map(|_| ())
}

/// What the `manage` ceiling lets an agent use: the reads, captured
/// traffic, and changes to mocks (theirs on a self-service instance),
/// AI-built mocks and portal publishing; nothing instance-wide (settings,
/// documents, source control, tests, checkpoints, agents, sessions, the
/// visit operator tools, load test runs).
pub const MANAGE_CEILING: &[Permission] = &[
    Permission::InstanceRead,
    Permission::MocksRead,
    Permission::MocksWrite,
    Permission::MocksDelete,
    Permission::MocksData,
    Permission::TrafficRead,
    Permission::HarnessRead,
    Permission::AssistUse,
    Permission::AssistJobs,
    Permission::McpInspect,
    Permission::PortalPublish,
];

// ── Credentials ─────────────────────────────────────────────────────

/// What an agent authenticates with (never the secret itself).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Credential {
    /// `RUSTYBIN_ADMIN_TOKEN`.
    Admin,
    /// A user's: their sign-in token (`client` empty), or a token of the
    /// built-in OAuth server the user consented to (`client` its subject,
    /// `client:<fingerprint>`). The agent acts for the user and never holds
    /// more than the user does.
    User {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client: Option<String>,
    },
    /// A console sign-in token by user name: an `agents.json` record from
    /// before agents acted for a user, kept readable (moved to [`User`] at
    /// startup when the user is known).
    ///
    /// [`User`]: Credential::User
    Console { user: String },
    /// A control JWT a platform issued (`RUSTYBIN_CONTROL_JWT_PUBLIC_KEY`).
    Jwt { sub: String, scopes: Vec<String> },
    /// A token of the built-in OAuth server approved by a credential that
    /// is no user (the admin token, a control JWT), by its client's subject.
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
            Self::User { .. } => "user",
            Self::Console { .. } => "console",
            Self::Jwt { .. } => "jwt",
            Self::Oauth { .. } => "oauth",
            Self::Key { .. } => "key",
            Self::Visit { .. } => "visit",
            Self::Open { .. } => "open",
        }
    }

    /// How a user's agent signed in: `sign-in` (a sign-in token) or
    /// `oauth` (the built-in OAuth server); `None` for the others.
    pub fn via(&self) -> Option<&'static str> {
        match self {
            Self::User { client: None, .. } | Self::Console { .. } => Some("sign-in"),
            Self::User {
                client: Some(_), ..
            }
            | Self::Oauth { .. } => Some("oauth"),
            _ => None,
        }
    }

    /// The user the agent acts for, when it is a user's.
    pub fn owner(&self) -> Option<&str> {
        match self {
            Self::User { id, .. } => Some(id),
            _ => None,
        }
    }

    /// The OAuth client's subject, for a token of the built-in server.
    pub fn oauth_subject(&self) -> Option<&str> {
        match self {
            Self::Oauth { sub }
            | Self::User {
                client: Some(sub), ..
            } => Some(sub),
            _ => None,
        }
    }

    /// Does a rule's `match.credential` name this credential? `user` is
    /// any user's agent; `console` one on a sign-in token (as in 0.4);
    /// `oauth` one on a token of the built-in OAuth server, a user's or
    /// not; the other kinds by name.
    pub fn is_kind(&self, kind: &str) -> bool {
        match kind {
            "user" => matches!(self, Self::User { .. } | Self::Console { .. }),
            "console" => self.via() == Some("sign-in"),
            "oauth" => self.via() == Some("oauth"),
            k => self.kind() == k,
        }
    }

    /// The part of the identity the credential contributes (rights,
    /// revocations and the assist caps follow it).
    pub(crate) fn fingerprint(&self) -> String {
        match self {
            Self::Admin => "admin".into(),
            Self::User { id, client: None } => format!("user:{id}"),
            Self::User {
                id,
                client: Some(sub),
            } => format!("user:{id}:oauth:{sub}"),
            Self::Console { user } => format!("console:{user}"),
            Self::Jwt { sub, .. } => format!("jwt:{sub}"),
            Self::Oauth { sub } => format!("oauth:{sub}"),
            Self::Key { id } => format!("key:{id}"),
            Self::Visit { fingerprint } => format!("visit:{fingerprint}"),
            Self::Open { session } => format!("open:{session}"),
        }
    }

    /// A short label for lists (a user's by id: [`Policy`] names them).
    pub fn label(&self) -> String {
        match self {
            Self::Admin => "admin token".into(),
            Self::User { id, client: None } => format!("sign-in ({id})"),
            Self::User {
                id,
                client: Some(sub),
            } => format!("OAuth ({sub}) for {id}"),
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
            Self::User { .. } | Self::Console { .. } | Self::Jwt { .. } | Self::Oauth { .. }
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
    /// The permissions of the role the ceiling names (`None` for a 0.3
    /// ceiling, and for a role that does not exist: nothing allowed).
    pub ceiling_perms: Option<crate::rbac::PermSet>,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub note: Option<String>,
    /// `agent` (its override), `rule` (of the config file), `role` (the
    /// default for a role of its user, `agents.default_ceiling_for`) or
    /// `default`.
    pub source: &'static str,
    /// The index of the matching rule, when one matched.
    pub rule: Option<usize>,
    /// The operator's overrides of the other agents of the same credential
    /// (other client names): each must allow a tool too, so a new client
    /// name never starts over with fewer limits.
    pub also: Vec<Effective>,
}

impl Effective {
    pub fn allows(&self, def: &ToolDef) -> bool {
        self.allows_own(def) && self.also.iter().all(|e| e.allows_own(def))
    }

    fn allows_own(&self, def: &ToolDef) -> bool {
        self.ceiling.allows_with(def, self.ceiling_perms)
            && (self.allow.is_empty() || self.allow.iter().any(|t| t == def.name))
            && !self.deny.iter().any(|t| t == def.name)
    }

    /// Why the operator's rights refuse `def` (for the tool error).
    pub fn denial(&self, def: &ToolDef) -> String {
        if self.allows_own(def) {
            if let Some(other) = self.also.iter().find(|e| !e.allows_own(def)) {
                return format!(
                    "{} (a limit set on another client name of the same credential)",
                    other.denial(def)
                );
            }
        }
        if self.deny.iter().any(|t| t == def.name) {
            return format!("the operator denied {} to this agent", def.name);
        }
        if !self.allow.is_empty() && !self.allow.iter().any(|t| t == def.name) {
            return format!(
                "the operator allows this agent only some tools, and {} is not one of them",
                def.name
            );
        }
        match &self.ceiling {
            Ceiling::Role(r) if self.ceiling_perms.is_none() => format!(
                "the operator limited this agent to the role {r}, which does not exist (nothing is allowed)"
            ),
            Ceiling::Role(r) => format!(
                "the operator limited this agent to the role {r}, which does not grant {}",
                def.permission.label()
            ),
            c => format!("the operator limited this agent to {} tools", c.as_str()),
        }
    }

    pub fn view(&self) -> Value {
        json!({
            "ceiling": self.ceiling.as_str(),
            "ceiling_kind": if matches!(self.ceiling, Ceiling::Role(_)) { "role" } else { "ceiling" },
            "ceiling_role": self.ceiling.nearest_role(),
            "ceiling_known": !matches!(self.ceiling, Ceiling::Role(_)) || self.ceiling_perms.is_some(),
            "allow": self.allow,
            "deny": self.deny,
            "source": self.source,
            "rule": self.rule,
        })
    }

    /// The tools it allows, in `TOOLS` order.
    pub fn allowed_tools(&self) -> Vec<&'static str> {
        TOOLS
            .iter()
            .filter(|d| self.allows(d))
            .map(|d| d.name)
            .collect()
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
/// `user`, `console`, `jwt`, `oauth`, `key`, `visit`, `open`); `user` is
/// the user the agent acts for (a sign-in name, `*` as a wildcard, or a
/// user id); `role` a role that user holds. All empty: every agent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Match {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

pub const CREDENTIAL_KINDS: &[&str] = &[
    "admin", "user", "console", "jwt", "oauth", "key", "visit", "open",
];

/// A validated rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub name: Option<String>,
    pub credential: Option<String>,
    pub user: Option<String>,
    pub role: Option<String>,
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
        match Ceiling::parse_any(c) {
            Some(c) => out.default_ceiling = Some(c),
            None => problems.push(format!(
                "agents.default_ceiling: {c:?} is not one of {} nor a role name (ignored)",
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
    let user = r
        .matcher
        .user
        .map(|n| clean(&n, MAX_NAME))
        .filter(|n| !n.is_empty());
    let role = match r.matcher.role.map(|n| n.trim().to_string()) {
        None => None,
        Some(n) if role_shaped(&n) => Some(n),
        Some(n) => return Err(format!("match.role {n:?} is not a role name")),
    };
    let ceiling = match r.ceiling {
        None => None,
        Some(c) => Some(Ceiling::parse_any(&c).ok_or_else(|| {
            format!(
                "ceiling {c:?} is not one of {} nor a role name",
                CEILINGS.join(", ")
            )
        })?),
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
        user,
        role,
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
    /// Does the rule apply to an agent named `name` on `credential`, acting
    /// for `owner` (its user, resolved now)? A rule naming a user or a role
    /// never matches an agent that acts for nobody.
    pub fn matches(
        &self,
        name: &str,
        credential: &Credential,
        owner: Option<&crate::rbac::users::Resolution>,
    ) -> bool {
        self.name.as_deref().is_none_or(|p| glob(p, name))
            && self
                .credential
                .as_deref()
                .is_none_or(|c| credential.is_kind(c))
            && self
                .user
                .as_deref()
                .is_none_or(|u| owner.is_some_and(|o| o.user.id == u || glob(u, &o.user.name)))
            && self
                .role
                .as_deref()
                .is_none_or(|r| owner.is_some_and(|o| o.roles.iter().any(|x| x == r)))
    }

    pub fn view(&self) -> Value {
        json!({
            "match": { "name": self.name, "credential": self.credential, "user": self.user, "role": self.role },
            "ceiling": self.ceiling.as_ref().map(Ceiling::as_str),
            "allow": self.allow,
            "deny": self.deny,
            "note": self.note,
        })
    }
}

/// The default ceiling and the rules, as they apply now.
#[derive(Clone, Default)]
pub struct Policy {
    pub default_ceiling: Ceiling,
    /// `stored`, `env`, `file` or `default`.
    pub default_source: &'static str,
    /// `agents.default_ceiling_for`: the default per role of the agent's
    /// user, in order (the first role the user holds applies).
    pub default_for: Vec<(String, Ceiling)>,
    pub rules: Vec<Rule>,
    /// The user store: owners, their roles, and the roles a ceiling names.
    users: Option<Arc<crate::rbac::Users>>,
}

impl std::fmt::Debug for Policy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Policy")
            .field("default_ceiling", &self.default_ceiling)
            .field("default_source", &self.default_source)
            .field("default_for", &self.default_for)
            .field("rules", &self.rules)
            .finish_non_exhaustive()
    }
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
            default_for: default_for_entries(&state.settings.list(DEFAULT_CEILING_FOR_KEY))
                .unwrap_or_default(),
            rules: file.rules,
            users: Some(state.users.clone()),
        }
    }

    /// The same policy reading `users` (tests).
    pub fn with_users(mut self, users: Arc<crate::rbac::Users>) -> Self {
        self.users = Some(users);
        self
    }

    /// The user an agent acts for, resolved now (`None`: no user, or one
    /// that is gone).
    pub fn owner_of(&self, c: &Credential) -> Option<Arc<crate::rbac::users::Resolution>> {
        let id = c.owner()?;
        self.users.as_ref()?.resolve(id)
    }

    /// Does the user an agent acts for meet `def`'s need? (`true` for an
    /// agent that acts for nobody: its credential decides at the call.)
    pub fn owner_allows(&self, c: &Credential, def: &ToolDef) -> bool {
        use super::access::ToolNeed;
        if c.owner().is_none() {
            return true;
        }
        let Some(o) = self.owner_of(c) else {
            return false;
        };
        match def.permission {
            ToolNeed::Own(_) => true,
            ToolNeed::Visitor => false,
            ToolNeed::Perm(p) => o.permissions.has(p),
            // Held in a project counts: the service checks each resource's.
            ToolNeed::Owner(p) | ToolNeed::Project(p) => {
                o.permissions.has(p) || o.projects.values().any(|g| g.permissions.has(p))
            }
        }
    }

    fn role_perms(&self, c: &Ceiling) -> Option<crate::rbac::PermSet> {
        match c {
            Ceiling::Role(r) => self.users.as_ref()?.role_permissions(r),
            _ => None,
        }
    }

    /// `owner` as answers show it: the user's id, sign-in name and display
    /// name, and whether they still exist (and are enabled).
    pub fn owner_view(&self, c: &Credential) -> Value {
        let Some(id) = c.owner() else {
            return Value::Null;
        };
        match self.owner_of(c) {
            Some(o) => json!({
                "user_id": o.user.id,
                "name": o.user.name,
                "display_name": o.user.display_name,
                "roles": o.roles,
                "status": o.user.status(),
            }),
            None => {
                json!({ "user_id": id, "name": null, "display_name": null, "roles": [], "status": "deleted" })
            }
        }
    }

    /// Who an agent is for, in a word: the user's sign-in name, or what the
    /// credential is when it acts for nobody.
    pub fn owner_label(&self, c: &Credential) -> String {
        match c {
            Credential::User { id, .. } => self
                .owner_of(c)
                .map(|o| o.user.name.clone())
                .unwrap_or_else(|| format!("{id} (deleted)")),
            Credential::Console { user } => format!("{user} (before 0.5)"),
            Credential::Admin => "admin token (no user)".into(),
            Credential::Jwt { sub, .. } => format!("control JWT {sub} (no user)"),
            Credential::Oauth { .. } => "OAuth, approved by a credential with no user".into(),
            Credential::Key { id } => format!("API key {id}"),
            Credential::Visit { .. } => "a self-service visit".into(),
            Credential::Open { .. } => "nobody (open control plane)".into(),
        }
    }

    /// The credential's label with the user's name in place of their id.
    pub fn credential_label(&self, c: &Credential) -> String {
        match c {
            Credential::User { client, .. } => {
                let who = self.owner_label(c);
                match client {
                    None => format!("sign-in ({who})"),
                    Some(sub) => format!("OAuth ({sub}) for {who}"),
                }
            }
            other => other.label(),
        }
    }

    /// The rights of an agent: its override field by field, else the
    /// first matching rule's, else the default for a role of its user,
    /// else the default.
    pub fn effective(&self, rec: &Record) -> Effective {
        let owner = self.owner_of(&rec.credential);
        let matched = self
            .rules
            .iter()
            .enumerate()
            .find(|(_, r)| r.matches(&rec.name, &rec.credential, owner.as_deref()));
        let rule = matched.map(|(_, r)| r);
        let role_default = owner.as_deref().and_then(|o| {
            self.default_for
                .iter()
                .find(|(role, _)| o.roles.iter().any(|r| r == role))
                .map(|(_, c)| c.clone())
        });
        let o = rec.rights.as_ref();
        let source = if o.is_some_and(|o| !o.is_empty()) || rec.note.is_some() {
            "agent"
        } else if rule.is_some() {
            "rule"
        } else if role_default.is_some() {
            "role"
        } else {
            "default"
        };
        let ceiling = o
            .and_then(|o| o.ceiling.clone())
            .or_else(|| rule.and_then(|r| r.ceiling.clone()))
            .or(role_default)
            .unwrap_or_else(|| self.default_ceiling.clone());
        Effective {
            ceiling_perms: self.role_perms(&ceiling),
            ceiling,
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
            also: Vec::new(),
        }
    }

    pub fn view(&self) -> Value {
        json!({
            "default_ceiling": self.default_ceiling.as_str(),
            "default_source": self.default_source,
            "default_ceiling_for": self.default_for.iter().map(|(r, c)| json!({ "role": r, "ceiling": c.as_str() })).collect::<Vec<_>>(),
            "rules": self.rules.iter().map(Rule::view).collect::<Vec<_>>(),
        })
    }
}

/// The settings this feature registers.
pub fn specs() -> Vec<Spec> {
    vec![
        Spec::new(
            DEFAULT_CEILING_KEY,
            "agents",
            "Default ceiling for AI agents",
            "The most an agent on the management MCP server may do unless a rule of rustybin.yaml, the default for a role of its user or its own rights say otherwise: read-only (read tools, no captured traffic), traffic (every read tool), manage (reads and mock changes), full (whatever its credential allows). An agent that acts for a user never does more than that user, whatever the ceiling.",
            Kind::Choice { options: CEILINGS },
            json!("full"),
        )
        .env("RUSTYBIN_AGENTS_DEFAULT_CEILING", |c| {
            c.agents_default_ceiling.clone().map(Value::String)
        }),
        Spec::new(
            DEFAULT_CEILING_FOR_KEY,
            "agents",
            "Default ceiling per role",
            "The default ceiling of the agents of users who hold a role, as role=ceiling (admin=editor: an admin's agents default to what an editor may do). The ceiling is one of read-only, traffic, manage, full or a role name; the first entry whose role the user holds applies, before the default ceiling and after the rules of rustybin.yaml and the agent's own rights.",
            Kind::List {
                max_items: MAX_DEFAULT_FOR,
                max_len: 100,
            },
            json!([]),
        )
        .check(valid_default_for),
    ]
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
    /// The user who revoked it, when one did (they may restore their own
    /// agent without `agents.manage`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_user: Option<String>,
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
    /// The caller lacks the permission (with why it was needed).
    Needs(Permission, String),
}

// ── Whose agents a caller sees and manages ──────────────────────────

/// Which agents a caller sees: every agent with `agents.read` (`Ok(None)`),
/// else a user's own (`Ok(Some(user id))`); a caller who is neither (a
/// control JWT, nobody on an open control plane) needs `agents.read`.
pub fn visible_to(caller: &crate::rbac::Resolved) -> Result<Option<String>, AgentError> {
    if caller.has(Permission::AgentsRead) {
        return Ok(None);
    }
    match caller.user_id() {
        Some(id) => Ok(Some(id.to_string())),
        None => Err(AgentError::Needs(
            Permission::AgentsRead,
            "listing AI agents needs the agents.read permission".into(),
        )),
    }
}

/// May `caller` read agent `id`? Every agent with `agents.read`, a user
/// their own.
pub fn may_read(
    agents: &Agents,
    caller: &crate::rbac::Resolved,
    id: &str,
) -> Result<(), AgentError> {
    let visible = visible_to(caller)?;
    let owner = agents.owner_of(id)?;
    match visible {
        None => Ok(()),
        Some(me) if owner.as_deref() == Some(me.as_str()) => Ok(()),
        Some(_) => Err(AgentError::Needs(
            Permission::AgentsRead,
            "this agent acts for someone else: reading other people's agents needs the agents.read permission".into(),
        )),
    }
}

/// An operator action on one agent.
pub enum Action {
    /// New rights (`Some(None)` clears them, `None` keeps them) and a note.
    Rights(Option<Option<Rights>>),
    Revoke,
    Restore,
    EndSessions,
}

/// May `caller` take `action` on agent `id`? Any agent with
/// `agents.manage`; without it, a user's own agent only: limit it (rights
/// that never allow a tool it may not use now), revoke it, end its
/// sessions, and restore it when they revoked it themselves.
pub fn may_manage(
    agents: &Agents,
    policy: &Policy,
    caller: &crate::rbac::Resolved,
    id: &str,
    action: &Action,
) -> Result<(), AgentError> {
    let needs = || {
        AgentError::Needs(
            Permission::AgentsManage,
            "changing an AI agent that is not yours needs the agents.manage permission".into(),
        )
    };
    if !caller.has(Permission::AgentsManage) && caller.user_id().is_none() {
        return Err(needs());
    }
    let owner = agents.owner_of(id)?;
    if caller.has(Permission::AgentsManage) {
        return Ok(());
    }
    let Some(me) = caller.user_id().filter(|me| owner.as_deref() == Some(*me)) else {
        return Err(needs());
    };
    match action {
        Action::Rights(rights) => {
            let (then, now) = agents.allowed_with(id, rights.clone(), policy)?;
            match then.iter().find(|t| !now.contains(t)) {
                None => Ok(()),
                Some(t) => Err(AgentError::Needs(
                    Permission::AgentsManage,
                    format!(
                        "you may limit your own agent, not widen it ({t} would be allowed again): widening an agent's rights needs the agents.manage permission"
                    ),
                )),
            }
        }
        Action::Restore => {
            if agents.revoked_by_user(id).as_deref() == Some(me) {
                Ok(())
            } else {
                Err(AgentError::Needs(
                    Permission::AgentsManage,
                    "someone else revoked this agent (or your user was disabled): restoring it needs the agents.manage permission".into(),
                ))
            }
        }
        Action::Revoke | Action::EndSessions => Ok(()),
    }
}

/// The role names a ceiling may take, for a caller choosing one: every
/// role with `users.read` or `agents.manage`, else the built-in ones (a
/// custom role's name is the users' business).
pub fn ceiling_roles(state: &AppState, caller: &crate::rbac::Resolved) -> Vec<String> {
    let mut out: Vec<String> = crate::rbac::BuiltIn::ALL
        .iter()
        .map(|r| r.name().to_string())
        .collect();
    if caller.has(Permission::UsersRead) || caller.has(Permission::AgentsManage) {
        out.extend(state.users.custom_roles().into_iter().map(|r| r.name));
    }
    out
}

/// A ceiling naming a role must name one that exists.
pub fn check_rights(
    rights: &Option<Option<Rights>>,
    users: &crate::rbac::Users,
) -> Result<(), AgentError> {
    match rights {
        Some(Some(Rights {
            ceiling: Some(c), ..
        })) => c.check(users).map_err(AgentError::Invalid),
        _ => Ok(()),
    }
}

/// The user id a filter names: an id, or a sign-in name the store knows
/// (an unknown value is taken as an id, a deleted user's).
pub fn owner_filter(users: &crate::rbac::Users, value: &str) -> String {
    users
        .find(value)
        .map(|u| u.id)
        .unwrap_or_else(|| value.to_string())
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
    /// `agents.json` was written before agents acted for a user (version
    /// 1): its console and OAuth records are moved to their user once
    /// ([`Agents::adopt_users`]).
    legacy: std::sync::atomic::AtomicBool,
}

/// The version `agents.json` is written with: 2 since agents act for a
/// user (a version 1 file's OAuth records were all approved before).
pub const DOC_VERSION: u32 = 2;

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
            if let (Some(_), Some(sub)) = (&r.revoked, r.credential.oauth_subject()) {
                tokens.revoke_subject(sub, now + crate::control_oauth::REFRESH_TTL_SECS);
            }
            // The token ids revoked with the agent, refused again after a
            // restart and never cleared by a sign out everywhere.
            if r.revoked.is_some() {
                for jti in &r.revoked_tokens {
                    let exp = r
                        .tokens
                        .iter()
                        .find(|(j, _)| j == jti)
                        .map(|(_, e)| *e)
                        .unwrap_or(now + crate::login::TOKEN_TTL_SECS);
                    tokens.remember_agent_revocation(jti, exp);
                }
            }
            records.insert(r.id.clone(), r);
        }
        let (events, _) = broadcast::channel(256);
        let legacy = !records.is_empty() && doc.version < DOC_VERSION;
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
            legacy: std::sync::atomic::AtomicBool::new(legacy),
        }
    }

    /// At startup with the password sign-in on, once for an `agents.json`
    /// written before agents acted for a user: a console record (by user
    /// name) becomes its user's, by id; an OAuth record becomes the first
    /// admin's (0.4 consents were the console user's, and its tokens are
    /// verified as the first admin's: `crate::control_oauth`). Rights,
    /// notes, revocations and totals move with the record. Answers how
    /// many moved.
    pub fn adopt_users(&self, users: &crate::rbac::Users) -> usize {
        if !self.legacy.swap(false, Ordering::Relaxed) {
            return 0;
        }
        let first = users.first_admin();
        let moved = {
            let mut inner = lock(&self.inner);
            let old: Vec<String> = inner
                .records
                .values()
                .filter(|r| {
                    matches!(
                        r.credential,
                        Credential::Console { .. } | Credential::Oauth { .. }
                    )
                })
                .map(|r| r.id.clone())
                .collect();
            let mut moved = 0;
            for id in old {
                let Some(r) = inner.records.get(&id) else {
                    continue;
                };
                let credential = match &r.credential {
                    Credential::Console { user } => match users.find_by_name(user) {
                        Some(u) => Credential::User {
                            id: u.id,
                            client: None,
                        },
                        None => continue,
                    },
                    Credential::Oauth { sub } => match &first {
                        Some(u) => Credential::User {
                            id: u.id.clone(),
                            client: Some(sub.clone()),
                        },
                        None => continue,
                    },
                    _ => continue,
                };
                let new_id = agent_id(&credential, &r.name);
                if inner.records.contains_key(&new_id) {
                    continue;
                }
                let Some(mut r) = inner.records.remove(&id) else {
                    continue;
                };
                r.credential = credential;
                r.id = new_id.clone();
                inner.records.insert(new_id, r);
                moved += 1;
            }
            moved
        };
        if moved > 0 {
            tracing::info!(target: "rustybin::agents", moved, "agents from before 0.5 now act for their user");
            if let Err(e) = self.save() {
                tracing::warn!("agents: {e}");
            }
        }
        moved
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
        // Revocation and the operator's limits follow the credential, not the
        // client name it sends: the other agents of the same credential count.
        let mut snapshot = snapshot;
        let fingerprint = seen.credential.fingerprint();
        for other in inner
            .records
            .values()
            .filter(|r| r.id != id && r.credential.fingerprint() == fingerprint)
        {
            if other.revoked.is_some() {
                snapshot.revoked = true;
            }
            if other.rights.as_ref().is_some_and(|o| !o.is_empty()) {
                snapshot.rights.also.push(policy.effective(other));
            }
        }
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
                "owner": r.credential.owner(),
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

    /// Every agent, most recently seen first; with `owner`, only the agents
    /// acting for that user (by id).
    pub fn list(&self, policy: &Policy, owner: Option<&str>) -> Vec<Value> {
        let now = self.now();
        let inner = lock(&self.inner);
        let mut all: Vec<&Record> = inner
            .records
            .values()
            .filter(|r| owner.is_none_or(|o| r.credential.owner() == Some(o)))
            .collect();
        all.sort_by(|a, b| b.last_seen.cmp(&a.last_seen).then(a.id.cmp(&b.id)));
        all.into_iter().map(|r| summary(r, policy, now)).collect()
    }

    /// The user an agent acts for (`Ok(None)`: nobody), `Err` when there is
    /// no such agent.
    pub fn owner_of(&self, id: &str) -> Result<Option<String>, AgentError> {
        lock(&self.inner)
            .records
            .get(id)
            .map(|r| r.credential.owner().map(str::to_string))
            .ok_or(AgentError::NotFound)
    }

    /// Who revoked an agent, when a user did (and it is revoked).
    pub fn revoked_by_user(&self, id: &str) -> Option<String> {
        lock(&self.inner)
            .records
            .get(id)
            .and_then(|r| r.revoked.as_ref())
            .and_then(|v| v.by_user.clone())
    }

    /// What an agent's rights would allow with `rights` in place of its own
    /// (`Some(None)` clears them; `None` keeps them), and what they allow
    /// now: the tool names, so a change can be told to only narrow.
    pub fn allowed_with(
        &self,
        id: &str,
        rights: Option<Option<Rights>>,
        policy: &Policy,
    ) -> Result<(Vec<&'static str>, Vec<&'static str>), AgentError> {
        let inner = lock(&self.inner);
        let r = inner.records.get(id).ok_or(AgentError::NotFound)?;
        let now = policy.effective(r);
        let mut changed = r.clone();
        if let Some(rights) = rights {
            changed.rights = rights.filter(|r| !r.is_empty());
        }
        let then = policy.effective(&changed);
        Ok((then.allowed_tools(), now.allowed_tools()))
    }

    /// The credential of an agent.
    pub fn credential_of(&self, id: &str) -> Option<Credential> {
        lock(&self.inner)
            .records
            .get(id)
            .map(|r| r.credential.clone())
    }

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
                        "permission": d.permission.label(),
                        "read_only": d.read_only,
                        "allowed": eff.allows(d) && policy.owner_allows(&r.credential, d),
                        "rights_allow": eff.allows(d),
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
        self.revoke_as(id, by, None, tokens)
    }

    /// [`Agents::revoke`] by a user (`by_user`, who may restore it without
    /// `agents.manage` when the agent is theirs).
    pub fn revoke_as(
        &self,
        id: &str,
        by: &str,
        by_user: Option<&str>,
        tokens: &crate::login::Tokens,
    ) -> Result<Value, AgentError> {
        let now = self.now();
        let (report, owner) = {
            let mut inner = lock(&self.inner);
            let r = inner.records.get_mut(id).ok_or(AgentError::NotFound)?;
            (
                revoke_record(r, now, by, by_user, tokens),
                r.credential.owner().map(str::to_string),
            )
        };
        tracing::info!(target: "rustybin::agents", agent = id, by, "agent revoked");
        let _ = self.events.send(
            json!({ "at": now.to_rfc3339(), "agent": id, "owner": owner, "event": "revoked" }),
        );
        self.save().map_err(AgentError::Io)?;
        Ok(report)
    }

    /// A user was disabled or deleted: revoke every agent acting for them
    /// (their calls are refused, their sessions end, their tokens are
    /// revoked). Answers how many agents were revoked now.
    pub fn revoke_owned(&self, user_id: &str, by: &str, tokens: &crate::login::Tokens) -> usize {
        let now = self.now();
        let ids: Vec<String> = {
            let mut inner = lock(&self.inner);
            let mut ids = Vec::new();
            for r in inner.records.values_mut() {
                if r.credential.owner() != Some(user_id) {
                    continue;
                }
                if r.revoked.is_none() {
                    revoke_record(r, now, by, None, tokens);
                    ids.push(r.id.clone());
                } else {
                    // Already revoked: its sessions still end.
                    end_sessions(r);
                }
            }
            ids
        };
        for id in &ids {
            let _ = self.events.send(json!({ "at": now.to_rfc3339(), "agent": id, "owner": user_id, "event": "revoked" }));
        }
        if !ids.is_empty() {
            tracing::info!(target: "rustybin::agents", agents = ids.len(), by, "the agents of a user were revoked");
            if let Err(e) = self.save() {
                tracing::warn!("agents: {e}");
            }
        }
        ids.len()
    }

    /// A user's sessions ended (signed out everywhere, a password change):
    /// the MCP sessions of every agent acting for them end too (their
    /// tokens are refused by the user's moved generation). Answers how
    /// many sessions ended.
    pub fn end_owned_sessions(&self, user_id: &str) -> usize {
        let mut inner = lock(&self.inner);
        inner
            .records
            .values_mut()
            .filter(|r| r.credential.owner() == Some(user_id))
            .map(end_sessions)
            .sum()
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
            if let Some(sub) = r.credential.oauth_subject() {
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
            version: DOC_VERSION,
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
            ceiling: self.default_ceiling.clone(),
            ceiling_perms: self.role_perms(&self.default_ceiling),
            allow: Vec::new(),
            deny: Vec::new(),
            note: None,
            source: "default",
            rule: None,
            also: Vec::new(),
        }
    }
}

/// Revoke one record: refuse its calls, end its sessions, revoke the
/// tokens it used and its OAuth client. Answers the report.
fn revoke_record(
    r: &mut Record,
    now: DateTime<Utc>,
    by: &str,
    by_user: Option<&str>,
    tokens: &crate::login::Tokens,
) -> Value {
    r.revoked = Some(Revocation {
        at: now,
        by: clean(by, 64),
        by_user: by_user.map(str::to_string),
    });
    let ended = end_sessions(r);
    let mut revoked = 0;
    if r.credential.revocable() {
        for (jti, exp) in &r.tokens {
            if *exp > now.timestamp() {
                tokens.revoke_for_agent(jti, *exp);
                if !r.revoked_tokens.contains(jti) {
                    r.revoked_tokens.push(jti.clone());
                }
                revoked += 1;
            }
        }
    }
    if let Some(sub) = r.credential.oauth_subject() {
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
            "The admin token cannot be revoked per agent: rotate RUSTYBIN_ADMIN_TOKEN. Meanwhile every agent that uses the admin token is refused on the management server, whatever client name it sends.",
        ),
        Credential::Console { .. }
        | Credential::User { client: None, .. } => Some(
            "The console sign-in tokens this agent used are revoked, and every agent on this user's sign-in tokens is refused on the management server until you restore it: if it used your own console token, your console is signed out too.",
        ),
        Credential::User { client: Some(_), .. } => Some(
            "The OAuth client's tokens and refresh tokens are refused from now on; the user's other agents and their console are not touched.",
        ),
        Credential::Key { .. } => Some(
            "API keys are revoked in the key registry; meanwhile every agent that uses this key is refused on the management server, whatever client name it sends.",
        ),
        Credential::Visit { .. } => Some("The visit continues; every agent of this visit is refused on the management server, whatever client name it sends."),
        Credential::Open { .. } => Some(
            "The control plane is open: an agent is known only by its session label (X-Rustybin-Session or its address), which it can change. Turn on the console sign-in to control agents for real.",
        ),
        Credential::Jwt { .. } | Credential::Oauth { .. } => None,
    }
}

fn summary(r: &Record, policy: &Policy, now: DateTime<Utc>) -> Value {
    let eff = policy.effective(r);
    let total = r.total();
    let mut cred = serde_json::to_value(&r.credential).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut cred {
        m.insert(
            "label".into(),
            json!(policy.credential_label(&r.credential)),
        );
        m.insert("revocable".into(), json!(r.credential.revocable()));
        m.insert("via".into(), json!(r.credential.via()));
    }
    json!({
        "id": r.id,
        "name": r.name,
        "version": r.version,
        "credential": cred,
        "owner": policy.owner_view(&r.credential),
        "owner_label": policy.owner_label(&r.credential),
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
        "tools_allowed": TOOLS.iter().filter(|d| eff.allows(d) && policy.owner_allows(&r.credential, d)).count(),
        "note": eff.note,
        "revoked": r.revoked.as_ref().map(|v| json!({ "at": v.at.to_rfc3339(), "by": v.by, "by_owner": v.by_user.is_some() && v.by_user.as_deref() == r.credential.owner() })),
    })
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod user_tests;
