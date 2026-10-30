//! The dev portal (`/portal`): a read-only site where testers find what the
//! operator published (mock APIs and their MCP servers, the mock MCP servers,
//! the mock LLM, the A2A agents, the HTTP tools, the mock MCP registry), read
//! the docs, try requests, download collections and copy connection snippets,
//! without the console or its password.
//!
//! - Publishing is explicit: [`Published`] metadata per item id (`mock:<name>`,
//!   `mcp:<server>`, `llm`, `a2a:<agent>`, `tools`, `registry`), set with
//!   `PUT /_rustybin/portal/items/{id}` (manage rights for a mock, admin for
//!   the rest) or the `portal.items` of `rustybin.yaml`, removed with `DELETE`.
//!   Saved in `<state>/portal.json` with the insights.
//! - The portal itself is a second embedded SPA (`ui-portal/`, served by
//!   [`api`] under `/portal`) that reads one endpoint, `GET
//!   /portal/api/catalog` (published items only, built by [`items`], cached
//!   with an ETag), plus the public spec of each mock and the portal's own
//!   download route.
//! - Settings (group `portal`): `portal.enabled` (`auto`: on once something is
//!   published), `portal.auth` (`open`, `password`: any control credential
//!   such as the console sign-in, `key`: an API key of the key registry),
//!   `portal.base_url` (a gateway in front of the instance: what snippets and
//!   Try it use), the branding and `portal.visitors`.
//! - Insights ([`insights`]): bounded counters per item (views per day for 30
//!   days, downloads by format, Try it calls), no visitor identifiers.

pub mod api;
pub mod insights;
pub mod items;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::mocks::visits::Owner;
use crate::settings::{Kind, Settings, Spec};
use crate::state::AppState;

/// The portal's URL prefix.
pub const PREFIX: &str = "/portal";
/// The one endpoint the portal reads.
pub const CATALOG_PATH: &str = "/portal/api/catalog";
/// Views and Try it calls the portal reports.
pub const EVENTS_PATH: &str = "/portal/api/events";
/// A published mock as a collection.
pub const DOWNLOAD_PATH: &str = "/portal/api/items/{id}/collections/{format}";
/// A published mock's spec in the full API reference viewer.
pub const REFERENCE_PATH: &str = "/portal/reference/{id}";
/// The console's view of the portal.
pub const CONTROL_PATH: &str = "/_rustybin/portal";
/// Publish (`PUT`) and unpublish (`DELETE`) one item.
pub const CONTROL_ITEM_PATH: &str = "/_rustybin/portal/items/{id}";
/// Counters per item.
pub const INSIGHTS_PATH: &str = "/_rustybin/portal/insights";
/// The portal's document under the state directory.
pub const STATE_FILE: &str = "portal.json";

/// Most published items.
pub const MAX_ITEMS: usize = 256;
/// An item published or changed this many days ago is "new".
pub const NEW_DAYS: i64 = 7;
pub const MAX_TITLE: usize = 120;
pub const MAX_SUMMARY: usize = 2000;
pub const MAX_TAGS: usize = 10;
pub const MAX_TAG: usize = 40;
pub const MAX_CONTACT: usize = 200;
/// Counters reach the disk at most this often (a restart may lose the rest).
const FLUSH_EVERY: Duration = Duration::from_secs(30);
/// Catalogues kept (one per base URL and auth mode).
const CACHE_ENTRIES: usize = 8;

/// `portal.enabled`.
pub const ENABLED_OPTIONS: &[&str] = &["auto", "on", "off"];
/// `portal.auth`.
pub const AUTH_OPTIONS: &[&str] = &["open", "password", "key"];
/// `portal.accent`: colour tokens of the portal's stylesheet.
pub const ACCENTS: &[&str] = &["rust", "blue", "green", "purple", "teal", "slate"];

/// The MCP servers that can be published: (key, path, title, summary).
pub const MCP_SERVERS: &[(&str, &str, &str, &str)] = &[
    ("main", "/mcp", "Mock MCP server", "Every demo tool, resource and prompt over Streamable HTTP, no authentication."),
    ("protected", "/mcp/protected", "MCP server behind OAuth 2.1", "The same server behind OAuth: discovery from the 401 (RFC 9728) and tokens from the built-in identity provider."),
    ("apikey", "/mcp/apikey", "MCP server behind an API key", "The same server behind an X-API-Key header."),
    ("sse", "/mcp/sse", "MCP server, legacy HTTP+SSE", "The 2024-11-05 transport for older clients."),
    ("weather", "/mcp/servers/weather", "Weather MCP server", "A small server with the weather tools only."),
    ("crm", "/mcp/servers/crm", "CRM MCP server", "A small server with the CRM tools only."),
    ("devtools", "/mcp/servers/devtools", "Developer tools MCP server", "A small server with the developer tools only."),
];

// ── Settings ────────────────────────────────────────────────────────

pub fn specs() -> Vec<Spec> {
    vec![
        Spec::new(
            "portal.enabled",
            "portal",
            "Portal",
            "auto: /portal is on once at least one item is published; on: always (an empty portal says nothing is published); off: /portal answers 404.",
            Kind::Choice { options: ENABLED_OPTIONS },
            json!("auto"),
        )
        .env("RUSTYBIN_PORTAL", |c| c.portal.clone().map(Value::String)),
        Spec::new(
            "portal.auth",
            "portal",
            "Who may read the portal",
            "open: anyone who reaches /portal; password: the console sign-in (or any control-plane credential); key: an API key of the key registry (shared mode). The page itself always loads; its data needs the credential.",
            Kind::Choice { options: AUTH_OPTIONS },
            json!("open"),
        )
        .env("RUSTYBIN_PORTAL_AUTH", |c| c.portal_auth.clone().map(Value::String)),
        Spec::new(
            "portal.base_url",
            "portal",
            "Base URL for testers",
            "The URL testers should call, when a gateway sits in front of this instance (https://gateway.example.com/rustybin). Snippets, collections and Try it use it; empty means the URL the portal was opened with.",
            Kind::Url,
            Value::Null,
        )
        .env("RUSTYBIN_PORTAL_BASE_URL", |c| {
            c.portal_base_url.clone().map(Value::String)
        }),
        Spec::new(
            "portal.title",
            "portal",
            "Portal title",
            "Shown in the portal header and the browser tab (default: API portal).",
            Kind::Text { max_len: 80 },
            Value::Null,
        ),
        Spec::new(
            "portal.intro",
            "portal",
            "Introduction",
            "The home page's introduction, in Markdown (paragraphs, headings, lists, bold, code, http(s) links).",
            Kind::Markdown { max_len: 4000 },
            Value::Null,
        ),
        Spec::new(
            "portal.accent",
            "portal",
            "Accent colour",
            "The portal's accent colour.",
            Kind::Choice { options: ACCENTS },
            json!("rust"),
        ),
        Spec::new(
            "portal.contact",
            "portal",
            "Contact",
            "Who testers ask for help (a name, an address or a URL), shown in the footer.",
            Kind::Text { max_len: MAX_CONTACT },
            Value::Null,
        ),
        Spec::new(
            "portal.visitors",
            "portal",
            "Visitors may publish",
            "On a self-service instance, visitors may publish their own mocks (with a visitor badge; they leave with the visit).",
            Kind::Bool,
            json!(false),
        )
        .env("RUSTYBIN_PORTAL_VISITORS", |c| {
            c.portal_visitors.then_some(Value::Bool(true))
        }),
    ]
}

/// `portal.auth`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    Open,
    Password,
    Key,
}

impl AuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Password => "password",
            Self::Key => "key",
        }
    }
}

pub fn auth_mode(settings: &Settings) -> AuthMode {
    match settings.string("portal.auth").as_deref() {
        Some("password") => AuthMode::Password,
        Some("key") => AuthMode::Key,
        _ => AuthMode::Open,
    }
}

/// `portal.enabled` (`auto`, `on` or `off`).
pub fn mode(settings: &Settings) -> String {
    settings
        .string("portal.enabled")
        .filter(|m| ENABLED_OPTIONS.contains(&m.as_str()))
        .unwrap_or_else(|| "auto".into())
}

/// `portal.base_url` without a trailing slash.
pub fn base_url_setting(settings: &Settings) -> Option<String> {
    settings
        .string("portal.base_url")
        .map(|u| u.trim_end_matches('/').to_string())
}

/// Is `/portal` served? `auto` needs one published item whose target exists.
pub fn active(state: &AppState) -> bool {
    match mode(&state.settings).as_str() {
        "on" => true,
        "off" => false,
        _ => !items::public_entries(state).is_empty(),
    }
}

/// The branding the portal shows (escaped wherever it lands in HTML).
pub fn branding(settings: &Settings) -> Value {
    json!({
        "title": settings.string("portal.title").unwrap_or_else(|| "API portal".into()),
        "intro": settings.string("portal.intro"),
        "accent": settings
            .string("portal.accent")
            .filter(|a| ACCENTS.contains(&a.as_str()))
            .unwrap_or_else(|| "rust".into()),
        "contact": settings.string("portal.contact"),
    })
}

// ── Item ids ────────────────────────────────────────────────────────

/// What an item id names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemId {
    Mock(String),
    /// A key of [`MCP_SERVERS`].
    Mcp(&'static str),
    Llm,
    /// An A2A agent id.
    Agent(&'static str),
    Tools,
    Registry,
}

/// The kinds in display order: (kind, title).
pub const KINDS: &[(&str, &str)] = &[
    ("api", "APIs"),
    ("mcp", "MCP servers"),
    ("llm", "LLM"),
    ("agent", "Agents"),
    ("tools", "Tools"),
    ("registry", "Registry"),
];

impl ItemId {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        match raw {
            "llm" => return Ok(Self::Llm),
            "tools" => return Ok(Self::Tools),
            "registry" => return Ok(Self::Registry),
            _ => {}
        }
        if let Some(name) = raw.strip_prefix("mock:") {
            if crate::mocks::model::valid_name(name) {
                return Ok(Self::Mock(name.to_string()));
            }
            return Err(format!("{name:?} is not a valid mock name"));
        }
        if let Some(key) = raw.strip_prefix("mcp:") {
            return MCP_SERVERS
                .iter()
                .find(|(k, ..)| *k == key)
                .map(|(k, ..)| Self::Mcp(k))
                .ok_or_else(|| {
                    let keys: Vec<&str> = MCP_SERVERS.iter().map(|(k, ..)| *k).collect();
                    format!("unknown MCP server {key:?} (one of {})", keys.join(", "))
                });
        }
        if let Some(id) = raw.strip_prefix("a2a:") {
            return crate::a2a::agents::find(id)
                .map(|a| Self::Agent(a.id))
                .ok_or_else(|| format!("unknown agent {id:?}"));
        }
        Err(format!(
            "unknown item id {raw:?}: use mock:<name>, mcp:<server>, llm, a2a:<agent>, tools or registry"
        ))
    }

    pub fn as_string(&self) -> String {
        match self {
            Self::Mock(n) => format!("mock:{n}"),
            Self::Mcp(k) => format!("mcp:{k}"),
            Self::Llm => "llm".into(),
            Self::Agent(a) => format!("a2a:{a}"),
            Self::Tools => "tools".into(),
            Self::Registry => "registry".into(),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Mock(_) => "api",
            Self::Mcp(_) => "mcp",
            Self::Llm => "llm",
            Self::Agent(_) => "agent",
            Self::Tools => "tools",
            Self::Registry => "registry",
        }
    }
}

// ── Published metadata ──────────────────────────────────────────────

/// An item's portal metadata. Empty title and summary fall back to the
/// item's own (the spec's title, the agent card, ...).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Published {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,
    /// Lower comes first within a kind.
    #[serde(default)]
    pub order: i64,
    /// Show the demo credentials a mock accepts (mocks only).
    #[serde(default = "default_true")]
    pub credentials: bool,
    pub published_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Who published it (`admin-token`, `jwt:<sub>`, `open`, `v:1a2b3c4d`,
    /// `file`).
    pub by: String,
    /// The mock's owner when published: a visitor's item leaves with the
    /// visit, and a mock of another owner under the same name is not
    /// published by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Owner>,
}

fn default_true() -> bool {
    true
}

impl Published {
    pub fn visitor(&self) -> bool {
        self.owner.as_ref().is_some_and(Owner::is_visitor)
    }
}

/// The metadata a publish sends (the `PUT` body, a `portal.items` entry).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<bool>,
}

/// One entry of the `portal.items` section of `rustybin.yaml`.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileItem {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<bool>,
}

impl FileItem {
    pub fn meta(&self) -> Meta {
        Meta {
            title: self.title.clone(),
            summary: self.summary.clone(),
            tags: self.tags.clone(),
            contact: self.contact.clone(),
            order: self.order,
            credentials: self.credentials,
        }
    }
}

/// The `portal:` section of `rustybin.yaml`.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileSection {
    #[serde(default)]
    pub items: Vec<FileItem>,
}

/// The usable `portal.items` of a config file (validated ids and metadata,
/// no duplicates, at most [`MAX_ITEMS`]) and a problem line per entry that
/// is not.
pub fn file_items(section: Option<FileSection>) -> (Vec<FileItem>, Vec<String>) {
    let mut ok: Vec<FileItem> = Vec::new();
    let mut problems = Vec::new();
    for item in section.into_iter().flat_map(|s| s.items) {
        let id = match ItemId::parse(&item.id) {
            Ok(id) => id.as_string(),
            Err(e) => {
                problems.push(format!("portal.items: {e} (skipped)"));
                continue;
            }
        };
        if let Err(e) = clean_meta(&item.meta()) {
            problems.push(format!("portal.items {id}: {e} (skipped)"));
            continue;
        }
        if ok.iter().any(|o| o.id == id) {
            problems.push(format!(
                "portal.items: {id} is listed twice (the first is kept)"
            ));
            continue;
        }
        if ok.len() >= MAX_ITEMS {
            problems.push(format!(
                "portal.items: more than {MAX_ITEMS} items (the rest are skipped)"
            ));
            break;
        }
        ok.push(FileItem { id, ..item });
    }
    (ok, problems)
}

/// Free text without control characters, trimmed, `None` when empty.
fn clean_text(
    raw: &Option<String>,
    max: usize,
    what: &str,
    keep_newlines: bool,
) -> Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let raw = raw.replace("\r\n", "\n");
    let clean: String = raw
        .chars()
        .filter(|c| (keep_newlines && *c == '\n') || !c.is_control())
        .collect();
    let clean = clean.trim();
    if clean.chars().count() > max {
        return Err(format!("{what} is longer than {max} characters"));
    }
    Ok((!clean.is_empty()).then(|| clean.to_string()))
}

/// The validated, normalised metadata.
pub fn clean_meta(meta: &Meta) -> Result<Meta, String> {
    let tags = match &meta.tags {
        None => None,
        Some(tags) => {
            let mut out: Vec<String> = Vec::new();
            for t in tags {
                if let Some(t) = clean_text(&Some(t.clone()), MAX_TAG, "a tag", false)? {
                    if !out.contains(&t) {
                        out.push(t);
                    }
                }
            }
            if out.len() > MAX_TAGS {
                return Err(format!("more than {MAX_TAGS} tags"));
            }
            Some(out)
        }
    };
    if let Some(order) = meta.order {
        if !(-1000..=1000).contains(&order) {
            return Err("order must be between -1000 and 1000".into());
        }
    }
    Ok(Meta {
        title: clean_text(&meta.title, MAX_TITLE, "the title", false)?,
        summary: clean_text(&meta.summary, MAX_SUMMARY, "the summary", true)?,
        tags,
        contact: clean_text(&meta.contact, MAX_CONTACT, "the contact", false)?,
        order: meta.order,
        credentials: meta.credentials,
    })
}

// ── State ───────────────────────────────────────────────────────────

/// The document in `<state>/portal.json`.
#[derive(Default, Deserialize, Serialize)]
struct Doc {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    items: BTreeMap<String, Published>,
    #[serde(default)]
    insights: insights::Insights,
}

/// One rendered catalogue.
pub struct Cached {
    pub fingerprint: String,
    pub body: Arc<Vec<u8>>,
    pub etag: String,
}

/// Published items, insights and the catalogue cache (`AppState.portal`).
pub struct Portal {
    items: RwLock<BTreeMap<String, Published>>,
    insights: Mutex<insights::Insights>,
    path: Option<PathBuf>,
    last_flush: Mutex<Option<Instant>>,
    /// Bumped on every publish and unpublish (part of the cache key).
    generation: AtomicU64,
    cache: Mutex<VecDeque<Arc<Cached>>>,
}

/// Why a publish was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum PublishError {
    Full,
    Io(String),
}

impl Portal {
    /// Load `<state>/portal.json` when a state directory is set (a broken
    /// file warns and starts empty).
    pub fn new(settings: &Settings) -> Self {
        let path = settings.state_file(STATE_FILE);
        let doc = path
            .as_ref()
            .filter(|p| p.exists())
            .and_then(|p| match std::fs::read(p) {
                Ok(bytes) => match serde_json::from_slice::<Doc>(&bytes) {
                    Ok(doc) => Some(doc),
                    Err(e) => {
                        tracing::warn!(
                            "portal: {} is not readable ({e}); starting empty",
                            p.display()
                        );
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!("portal: {}: {e}; starting empty", p.display());
                    None
                }
            })
            .unwrap_or_default();
        let mut items = doc.items;
        items.retain(|id, _| ItemId::parse(id).is_ok_and(|p| p.as_string() == *id));
        while items.len() > MAX_ITEMS {
            items.pop_last();
        }
        let mut insights = doc.insights;
        insights.bound();
        Self {
            items: RwLock::new(items),
            insights: Mutex::new(insights),
            path,
            last_flush: Mutex::new(None),
            generation: AtomicU64::new(1),
            cache: Mutex::new(VecDeque::new()),
        }
    }

    /// An in-memory portal (tests).
    pub fn in_memory() -> Self {
        Self {
            items: RwLock::new(BTreeMap::new()),
            insights: Mutex::new(insights::Insights::default()),
            path: None,
            last_flush: Mutex::new(None),
            generation: AtomicU64::new(1),
            cache: Mutex::new(VecDeque::new()),
        }
    }

    pub fn persisted(&self) -> bool {
        self.path.is_some()
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Items published through the API (not the config file).
    pub fn published(&self) -> BTreeMap<String, Published> {
        self.items.read().map(|m| m.clone()).unwrap_or_default()
    }

    pub fn get(&self, id: &str) -> Option<Published> {
        self.items.read().ok().and_then(|m| m.get(id).cloned())
    }

    /// Publish (or update) `id` with `meta` on top of what it had.
    pub fn publish(
        &self,
        id: &str,
        meta: Meta,
        by: String,
        owner: Option<Owner>,
        now: DateTime<Utc>,
    ) -> Result<(Published, bool), PublishError> {
        let (item, created) = {
            let Ok(mut items) = self.items.write() else {
                return Err(PublishError::Io("the portal state is unavailable".into()));
            };
            let previous = items.get(id).cloned();
            if previous.is_none() && items.len() >= MAX_ITEMS {
                return Err(PublishError::Full);
            }
            let base = previous.clone().unwrap_or(Published {
                title: None,
                summary: None,
                tags: Vec::new(),
                contact: None,
                order: 0,
                credentials: true,
                published_at: now,
                updated_at: now,
                by: by.clone(),
                owner: owner.clone(),
            });
            let item = Published {
                title: meta.title.or(base.title),
                summary: meta.summary.or(base.summary),
                tags: meta.tags.unwrap_or(base.tags),
                contact: meta.contact.or(base.contact),
                order: meta.order.unwrap_or(base.order),
                credentials: meta.credentials.unwrap_or(base.credentials),
                published_at: base.published_at,
                updated_at: now,
                by,
                owner,
            };
            items.insert(id.to_string(), item.clone());
            (item, previous.is_none())
        };
        self.changed();
        self.save().map_err(PublishError::Io)?;
        Ok((item, created))
    }

    /// Remove `id`; false when it was not published.
    pub fn unpublish(&self, id: &str) -> Result<bool, String> {
        let removed = self
            .items
            .write()
            .map(|mut m| m.remove(id).is_some())
            .unwrap_or(false);
        if removed {
            self.changed();
            self.save()?;
        }
        Ok(removed)
    }

    /// Drop the items of visitor mocks that are gone (the visit ended).
    pub fn sweep(&self, state: &AppState) -> usize {
        let gone: Vec<String> = self
            .published()
            .into_iter()
            .filter(|(id, p)| {
                p.visitor()
                    && match ItemId::parse(id) {
                        Ok(ItemId::Mock(name)) => state
                            .mocks
                            .get(&name)
                            .is_none_or(|s| Some(&s.owner) != p.owner.as_ref()),
                        _ => false,
                    }
            })
            .map(|(id, _)| id)
            .collect();
        if gone.is_empty() {
            return 0;
        }
        if let Ok(mut items) = self.items.write() {
            for id in &gone {
                items.remove(id);
            }
        }
        self.changed();
        if let Err(e) = self.save() {
            tracing::warn!("portal: {e}");
        }
        gone.len()
    }

    fn changed(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut c) = self.cache.lock() {
            c.clear();
        }
    }

    // ── Insights ──

    /// Count an event on a published item (`view`, `try`, or a download
    /// format). Unknown events are ignored.
    pub fn record(&self, id: &str, event: insights::Event, now: DateTime<Utc>) {
        if let Ok(mut i) = self.insights.lock() {
            i.record(id, event, now.date_naive());
        }
        self.flush_soon();
    }

    pub fn insights(&self) -> insights::Insights {
        self.insights.lock().map(|i| i.clone()).unwrap_or_default()
    }

    fn flush_soon(&self) {
        if self.path.is_none() {
            return;
        }
        let due = self
            .last_flush
            .lock()
            .map(|t| t.is_none_or(|t| t.elapsed() >= FLUSH_EVERY))
            .unwrap_or(false);
        if due {
            if let Err(e) = self.save() {
                tracing::warn!("portal: {e}");
            }
        }
    }

    /// Write `portal.json` (a no-op in memory).
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let doc = Doc {
            version: 1,
            items: self.published(),
            insights: self.insights(),
        };
        let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
        crate::settings::write_private(path, &bytes)?;
        if let Ok(mut t) = self.last_flush.lock() {
            *t = Some(Instant::now());
        }
        Ok(())
    }

    // ── Catalogue cache ──

    pub fn cached(&self, fingerprint: &str) -> Option<Arc<Cached>> {
        self.cache
            .lock()
            .ok()
            .and_then(|c| c.iter().find(|e| e.fingerprint == fingerprint).cloned())
    }

    pub fn store_cached(&self, entry: Arc<Cached>) {
        if let Ok(mut c) = self.cache.lock() {
            c.retain(|e| e.fingerprint != entry.fingerprint);
            if c.len() >= CACHE_ENTRIES {
                c.pop_front();
            }
            c.push_back(entry);
        }
    }
}

pub use api::{catalog, openapi_paths, router};
