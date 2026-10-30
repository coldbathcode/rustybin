//! The instance settings store (`docs/plans/next/step-01-foundations.md`).
//!
//! Features own dotted keys (`console.title`, `egress.allow`,
//! `leads.webhook_token`) and register them with a [`Spec`]: the kind, the
//! default, whether the value is a secret, which environment variable seeds
//! it and whether `RUSTYBIN_SETTINGS_LOCK` locks it. [`specs`] joins the
//! tables of every feature for the API and the console's Management forms.
//!
//! Resolution: a stored value wins over the environment seed, which wins
//! over the default ([`Source`]). With `RUSTYBIN_SETTINGS_LOCK=true` the
//! environment is authoritative for the lockable keys: stored values are
//! ignored and writes answer 409.
//!
//! Secrets are sealed with ChaCha20-Poly1305 under a 32-byte key from
//! `RUSTYBIN_SECRETS_KEY` (base64), else `<state>/secrets.key` (generated
//! once, mode 0600), else a process-lifetime key. The API only ever returns
//! `{ set, hint }` for a secret, never the value; a feature reads it with
//! [`Settings::secret`]. A secret sealed under another key is reported
//! `unreadable` (the console asks for it again) and the environment seed is
//! used meanwhile.
//!
//! With `RUSTYBIN_STATE_DIR` the document lives in `<dir>/settings.json`
//! (written atomically: a temporary file renamed into place, mode 0600);
//! without it changes last until restart (`persisted: false`). Every change
//! is audited on the `rustybin::settings` target with the key, the source
//! and the principal, never the value.
//!
//! Routes (admin): `GET /_rustybin/settings`, `PATCH /_rustybin/settings`
//! (`{ "values": {key: value}, "secrets": {key: value | null} }`) and
//! `DELETE /_rustybin/settings/{key}` (back to the seed or the default).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use axum::extract::{Path as RoutePath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::state::AppState;

/// Most keys a document holds.
pub const MAX_KEYS: usize = 256;
/// Largest value (as JSON text).
pub const MAX_VALUE_BYTES: usize = 64 * 1024;
/// Most secrets a document holds.
pub const MAX_SECRETS: usize = 64;
/// Largest secret.
pub const MAX_SECRET_BYTES: usize = 4096;
/// The document's file name under the state directory.
pub const SETTINGS_FILE: &str = "settings.json";
/// The secrets key's file name under the state directory.
pub const KEY_FILE: &str = "secrets.key";
/// The document format version written by this build.
pub const DOC_VERSION: u32 = 1;

// ── Specs ───────────────────────────────────────────────────────────

/// The kind of a setting's value, validated on every write.
#[derive(Clone, Debug)]
pub enum Kind {
    /// Free text: control characters removed, trimmed, at most `max_len` chars.
    Text {
        max_len: usize,
    },
    /// Markdown: like `Text` but newlines are kept (CRLF becomes LF).
    Markdown {
        max_len: usize,
    },
    /// An absolute `http://` or `https://` URL.
    Url,
    Bool,
    Integer {
        min: i64,
        max: i64,
    },
    /// A list of strings (a comma-separated string is accepted and split).
    List {
        max_items: usize,
        max_len: usize,
    },
    /// One of a fixed set of strings (a select in the console).
    Choice {
        options: &'static [&'static str],
    },
}

impl Kind {
    /// The JSON Schema fragment the console form is generated from.
    pub fn schema(&self) -> Value {
        match self {
            Kind::Text { max_len } => json!({ "type": "string", "maxLength": max_len }),
            Kind::Markdown { max_len } => {
                json!({ "type": "string", "format": "markdown", "maxLength": max_len })
            }
            Kind::Url => json!({ "type": "string", "format": "uri", "maxLength": 2048 }),
            Kind::Bool => json!({ "type": "boolean" }),
            Kind::Integer { min, max } => {
                json!({ "type": "integer", "minimum": min, "maximum": max })
            }
            Kind::List { max_items, max_len } => json!({
                "type": "array",
                "items": { "type": "string", "maxLength": max_len },
                "maxItems": max_items
            }),
            Kind::Choice { options } => json!({ "type": "string", "enum": options }),
        }
    }

    /// Normalise and check a value; the result is what gets stored.
    pub fn validate(&self, value: &Value) -> Result<Value, String> {
        match self {
            Kind::Text { max_len } => {
                let s = value.as_str().ok_or("expected a string")?;
                let clean: String = s.chars().filter(|c| !c.is_control()).collect();
                let clean = clean.trim();
                if clean.chars().count() > *max_len {
                    return Err(format!("longer than {max_len} characters"));
                }
                Ok(Value::String(clean.to_string()))
            }
            Kind::Markdown { max_len } => {
                let s = value
                    .as_str()
                    .ok_or("expected a string")?
                    .replace("\r\n", "\n");
                let clean: String = s
                    .chars()
                    .filter(|c| *c == '\n' || !c.is_control())
                    .collect();
                let clean = clean.trim();
                if clean.chars().count() > *max_len {
                    return Err(format!("longer than {max_len} characters"));
                }
                Ok(Value::String(clean.to_string()))
            }
            Kind::Url => {
                let s = value.as_str().ok_or("expected a string")?.trim();
                if s.len() > 2048 {
                    return Err("longer than 2048 characters".into());
                }
                if s.chars()
                    .any(|c| c.is_whitespace() || c.is_control() || "\"'<>`\\".contains(c))
                {
                    return Err("contains whitespace or quotes".into());
                }
                let parsed = reqwest::Url::parse(s).map_err(|e| format!("not a URL: {e}"))?;
                if !matches!(parsed.scheme(), "http" | "https") {
                    return Err("must start with http:// or https://".into());
                }
                if parsed.host_str().is_none() {
                    return Err("has no host".into());
                }
                Ok(Value::String(s.to_string()))
            }
            Kind::Bool => match value {
                Value::Bool(b) => Ok(Value::Bool(*b)),
                Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" | "on" => Ok(Value::Bool(true)),
                    "false" | "0" | "no" | "off" => Ok(Value::Bool(false)),
                    _ => Err("expected true or false".into()),
                },
                _ => Err("expected true or false".into()),
            },
            Kind::Integer { min, max } => {
                let n = match value {
                    Value::Number(n) => n.as_i64(),
                    Value::String(s) => s.trim().parse::<i64>().ok(),
                    _ => None,
                }
                .ok_or("expected an integer")?;
                if n < *min || n > *max {
                    return Err(format!("must be between {min} and {max}"));
                }
                Ok(Value::from(n))
            }
            Kind::List { max_items, max_len } => {
                let items: Vec<String> = match value {
                    Value::Array(items) => items
                        .iter()
                        .map(|v| v.as_str().map(str::to_string).ok_or("expected strings"))
                        .collect::<Result<_, _>>()?,
                    Value::String(s) => s.split(',').map(str::to_string).collect(),
                    _ => return Err("expected a list of strings".into()),
                };
                let items: Vec<String> = items
                    .into_iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if items.len() > *max_items {
                    return Err(format!("more than {max_items} items"));
                }
                if let Some(long) = items.iter().find(|s| s.chars().count() > *max_len) {
                    return Err(format!("{long:?} is longer than {max_len} characters"));
                }
                Ok(Value::Array(items.into_iter().map(Value::String).collect()))
            }
            Kind::Choice { options } => {
                let s = value.as_str().ok_or("expected a string")?.trim();
                if options.contains(&s) {
                    Ok(Value::String(s.to_string()))
                } else {
                    Err(format!("expected one of {}", options.join(", ")))
                }
            }
        }
    }
}

/// An extra check on a validated value.
pub type Check = fn(&Value) -> Result<(), String>;

/// One registered setting.
#[derive(Clone)]
pub struct Spec {
    /// The dotted key (`console.title`).
    pub key: &'static str,
    /// The group the console shows it under (`console`, `egress`, ...).
    pub group: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub kind: Kind,
    /// Used when nothing is stored and the environment seeds nothing.
    pub default: Value,
    /// A secret: sealed at rest, returned as `{set, hint}` only.
    pub secret: bool,
    /// The environment variable that seeds it (documentation and the console).
    pub env: Option<&'static str>,
    /// Reads the seed from the parsed configuration (validated once there).
    pub seed: Option<fn(&Config) -> Option<Value>>,
    /// `RUSTYBIN_SETTINGS_LOCK` makes the environment authoritative for it.
    pub lockable: bool,
    /// An extra check on a validated value (the egress allowlist grammar).
    pub validate: Option<Check>,
}

impl Spec {
    /// A plain, lockable, environment-seeded setting.
    pub const fn new(
        key: &'static str,
        group: &'static str,
        title: &'static str,
        description: &'static str,
        kind: Kind,
        default: Value,
    ) -> Self {
        Self {
            key,
            group,
            title,
            description,
            kind,
            default,
            secret: false,
            env: None,
            seed: None,
            lockable: true,
            validate: None,
        }
    }

    pub const fn env(mut self, var: &'static str, seed: fn(&Config) -> Option<Value>) -> Self {
        self.env = Some(var);
        self.seed = Some(seed);
        self
    }

    pub const fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    pub const fn check(mut self, f: Check) -> Self {
        self.validate = Some(f);
        self
    }

    fn describe(&self) -> Value {
        json!({
            "key": self.key,
            "group": self.group,
            "title": self.title,
            "description": self.description,
            "schema": self.kind.schema(),
            "default": self.default,
            "secret": self.secret,
            "env": self.env,
            "lockable": self.lockable,
        })
    }
}

/// A group of settings (a tab or a card in the console).
pub struct Group {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
}

/// The groups in display order.
pub fn groups() -> Vec<Group> {
    vec![
        Group {
            id: "console",
            title: "Console",
            description: "What the console shows to everyone who opens it.",
        },
        Group {
            id: "signin",
            title: "Sign-in",
            description: "Who may open the console and the control plane (RUSTYBIN_CONTROL_AUTH=password).",
        },
        Group {
            id: "assist",
            title: "AI assist",
            description: "Your own model provider and key for the console's AI features. Nothing calls a model until a key is set; keys never leave the instance.",
        },
        Group {
            id: "egress",
            title: "Outbound connections",
            description: "Where this instance may send requests of its own (AI providers, webhooks, registries, git remotes).",
        },
        Group {
            id: "registry",
            title: "MCP registry",
            description: "The mock MCP registry at /mcp-registry (its scenario) and the real registries the console browses.",
        },
        Group {
            id: "portal",
            title: "Dev portal",
            description: "The read-only portal at /portal where testers find what was published: its branding, who may read it and the URL its snippets use.",
        },
        Group {
            id: "leads",
            title: "Leads webhook",
            description: "The webhook that receives the email leads of self-service mocks.",
        },
    ]
}

/// The settings of the console itself.
fn console_specs() -> Vec<Spec> {
    vec![
        Spec::new(
            "console.title",
            "console",
            "Console title",
            "Shown in the console header, the browser tab and the sign-in screen. At most 80 characters.",
            Kind::Text { max_len: crate::config::MAX_CONSOLE_TITLE_CHARS },
            Value::Null,
        )
        .env("RUSTYBIN_CONSOLE_TITLE", |c| {
            c.console_title.clone().map(Value::String)
        }),
        Spec::new(
            "console.backlink",
            "console",
            "Back link",
            "A portal URL shown as a \"Back to ...\" link in the console header and sign-in screen.",
            Kind::Url,
            Value::Null,
        )
        .env("RUSTYBIN_CONSOLE_BACKLINK", |c| {
            c.console_backlink.clone().map(Value::String)
        }),
    ]
}

/// Every registered setting, in group order.
pub fn specs() -> Vec<Spec> {
    let mut all = console_specs();
    all.extend(crate::login::specs());
    all.extend(crate::assist::specs());
    all.extend(crate::egress::specs());
    all.extend(crate::mcp::registry::specs());
    all.extend(crate::portal::specs());
    all.extend(crate::mocks::leads::specs());
    all
}

// ── Secrets ─────────────────────────────────────────────────────────

/// A sealed secret as stored in the document.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Sealed {
    /// 12 random bytes, base64.
    pub nonce: String,
    /// Ciphertext plus tag, base64.
    pub ciphertext: String,
}

/// Where the secrets key came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    /// `RUSTYBIN_SECRETS_KEY`.
    Env,
    /// `<state>/secrets.key`.
    File,
    /// Random for this process (no state directory): secrets do not survive
    /// a restart, which is fine since nothing persists without the directory.
    Process,
}

impl KeySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            KeySource::Env => "env",
            KeySource::File => "file",
            KeySource::Process => "process",
        }
    }
}

/// Seals and opens stored secrets.
pub struct Secrets {
    key: Zeroizing<[u8; 32]>,
    source: KeySource,
}

/// Decode a `RUSTYBIN_SECRETS_KEY` value: base64 (standard or URL-safe, with
/// or without padding) or hex of exactly 32 bytes.
pub fn decode_key(raw: &str) -> Result<[u8; 32], String> {
    let raw = raw.trim();
    let bytes = if raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        (0..32)
            .map(|i| u8::from_str_radix(&raw[2 * i..2 * i + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .map_err(|_| "not hex".to_string())?
    } else {
        let engines = [
            base64::engine::general_purpose::STANDARD,
            base64::engine::general_purpose::STANDARD_NO_PAD,
            base64::engine::general_purpose::URL_SAFE,
            base64::engine::general_purpose::URL_SAFE_NO_PAD,
        ];
        engines
            .iter()
            .find_map(|e| e.decode(raw).ok())
            .ok_or("expected base64 or hex")?
    };
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("expected 32 bytes, got {}", v.len()))?;
    Ok(key)
}

fn random_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

/// Read the key file, or generate one (mode 0600) when it is missing.
fn load_or_create_key(path: &Path) -> Result<[u8; 32], String> {
    match std::fs::read_to_string(path) {
        Ok(text) => decode_key(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = random_key();
            let text = base64::engine::general_purpose::STANDARD.encode(key);
            write_private(path, text.as_bytes())?;
            tracing::info!("secrets key generated at {}", path.display());
            Ok(key)
        }
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Create the parent directory (mode 0700) and write the file atomically
/// (mode 0600): a temporary file next to it, renamed into place.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = options
        .open(&tmp)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(bytes)?;
            f.sync_all()
        })
        .and_then(|_| std::fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot write {}: {e}", path.display()));
    }
    Ok(())
}

/// Create a directory (and its parents) with mode 0700 when it is missing.
pub fn create_private_dir(dir: &Path) -> Result<(), String> {
    if dir.as_os_str().is_empty() || dir.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))
}

impl Secrets {
    /// The key from the configuration, the state directory, or this process.
    pub fn from_config(config: &Config) -> Self {
        if let Some(key) = config.secrets_key {
            return Self {
                key: Zeroizing::new(key),
                source: KeySource::Env,
            };
        }
        if let Some(dir) = &config.state_dir {
            match load_or_create_key(&dir.join(KEY_FILE)) {
                Ok(key) => {
                    return Self {
                        key: Zeroizing::new(key),
                        source: KeySource::File,
                    }
                }
                Err(e) => tracing::warn!(
                    "secrets key unavailable ({e}): using a process-lifetime key, stored secrets will not survive a restart"
                ),
            }
        }
        Self {
            key: Zeroizing::new(random_key()),
            source: KeySource::Process,
        }
    }

    /// A fresh random key (tests).
    pub fn random() -> Self {
        Self {
            key: Zeroizing::new(random_key()),
            source: KeySource::Process,
        }
    }

    pub fn source(&self) -> KeySource {
        self.source
    }

    pub fn seal(&self, plain: &[u8]) -> Result<Sealed, String> {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&*self.key));
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), plain)
            .map_err(|_| "sealing failed".to_string())?;
        let b64 = base64::engine::general_purpose::STANDARD;
        Ok(Sealed {
            nonce: b64.encode(nonce),
            ciphertext: b64.encode(ciphertext),
        })
    }

    pub fn open(&self, sealed: &Sealed) -> Result<Zeroizing<Vec<u8>>, String> {
        let b64 = base64::engine::general_purpose::STANDARD;
        let nonce = b64
            .decode(&sealed.nonce)
            .map_err(|_| "bad nonce".to_string())?;
        if nonce.len() != 12 {
            return Err("bad nonce".into());
        }
        let ciphertext = b64
            .decode(&sealed.ciphertext)
            .map_err(|_| "bad ciphertext".to_string())?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&*self.key));
        cipher
            .decrypt(Nonce::from_slice(&nonce), ciphertext.as_slice())
            .map(Zeroizing::new)
            .map_err(|_| "sealed under another key".to_string())
    }
}

// ── The document ────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Doc {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    values: BTreeMap<String, Value>,
    #[serde(default)]
    secrets: BTreeMap<String, Sealed>,
}

/// Where a resolved value came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Stored,
    Env,
    Default,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Stored => "stored",
            Source::Env => "env",
            Source::Default => "default",
        }
    }
}

/// Why a write was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateError {
    /// `RUSTYBIN_SETTINGS_LOCK` holds these keys.
    Locked(Vec<String>),
    /// Unknown keys, wrong kinds, values over the bounds.
    Invalid(Vec<String>),
    /// The document could not be written.
    Io(String),
}

/// The store.
pub struct Settings {
    specs: Vec<Spec>,
    seeds: BTreeMap<String, Value>,
    secret_seeds: BTreeMap<String, Zeroizing<String>>,
    doc: RwLock<Doc>,
    path: Option<PathBuf>,
    secrets: Secrets,
    locked: bool,
    unreadable: RwLock<BTreeSet<String>>,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Settings {
    /// The production store: every registered spec, seeds from `config`,
    /// the document from `<state>/settings.json` when a state directory is set.
    pub fn new(config: &Config) -> Self {
        Self::with_specs(config, specs())
    }

    /// A store over a given spec table (tests).
    pub fn with_specs(config: &Config, specs: Vec<Spec>) -> Self {
        let mut seeds = BTreeMap::new();
        let mut secret_seeds = BTreeMap::new();
        for spec in &specs {
            let Some(seed) = spec.seed else { continue };
            let Some(value) = seed(config) else { continue };
            if spec.secret {
                if let Some(s) = value.as_str() {
                    secret_seeds.insert(spec.key.to_string(), Zeroizing::new(s.to_string()));
                }
            } else {
                seeds.insert(spec.key.to_string(), value);
            }
        }
        let path = config.state_dir.as_ref().map(|d| d.join(SETTINGS_FILE));
        if let Some(dir) = &config.state_dir {
            if let Err(e) = create_private_dir(dir) {
                tracing::warn!("state directory unavailable ({e}): settings are kept in memory");
            }
        }
        let secrets = Secrets::from_config(config);
        let doc = match path.as_deref().map(load_doc) {
            Some(Ok(doc)) => doc,
            Some(Err(e)) => {
                tracing::warn!("{e}: starting with empty settings");
                Doc::default()
            }
            None => Doc::default(),
        };
        let store = Self {
            specs,
            seeds,
            secret_seeds,
            doc: RwLock::new(doc),
            path,
            secrets,
            locked: config.settings_lock,
            unreadable: RwLock::new(BTreeSet::new()),
        };
        store.check_secrets();
        store
    }

    /// Every stored secret is opened once: the ones sealed under another
    /// key are reported, and the console asks for them again.
    fn check_secrets(&self) {
        let doc = self.read();
        let mut bad = BTreeSet::new();
        for (key, sealed) in &doc.secrets {
            if self.secrets.open(sealed).is_err() {
                bad.insert(key.clone());
            }
        }
        drop(doc);
        if !bad.is_empty() {
            tracing::warn!(
                target: "rustybin::settings",
                "{} stored secret(s) cannot be read with this secrets key ({}): set them again, or restore RUSTYBIN_SECRETS_KEY",
                bad.len(),
                bad.iter().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        *self.write_unreadable() = bad;
    }

    fn read(&self) -> RwLockReadGuard<'_, Doc> {
        self.doc.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, Doc> {
        self.doc.write().unwrap_or_else(|e| e.into_inner())
    }

    fn write_unreadable(&self) -> RwLockWriteGuard<'_, BTreeSet<String>> {
        self.unreadable.write().unwrap_or_else(|e| e.into_inner())
    }

    fn is_unreadable(&self, key: &str) -> bool {
        self.unreadable
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(key)
    }

    pub fn spec(&self, key: &str) -> Option<&Spec> {
        self.specs.iter().find(|s| s.key == key)
    }

    pub fn specs(&self) -> &[Spec] {
        &self.specs
    }

    /// Changes are written to disk (a state directory is set).
    pub fn persisted(&self) -> bool {
        self.path.is_some()
    }

    /// `RUSTYBIN_SETTINGS_LOCK` is on.
    pub fn locked(&self) -> bool {
        self.locked
    }

    pub fn key_source(&self) -> KeySource {
        self.secrets.source()
    }

    /// Seal a secret with the store's key, for features that keep secrets of
    /// their own outside the settings table (the MCP inspector's saved
    /// connections).
    pub fn seal(&self, plain: &[u8]) -> Result<Sealed, String> {
        self.secrets.seal(plain)
    }

    /// Open a secret sealed with [`Settings::seal`].
    pub fn open(&self, sealed: &Sealed) -> Result<Zeroizing<Vec<u8>>, String> {
        self.secrets.open(sealed)
    }

    /// The state directory's file `name` when one is set (features keeping
    /// their own documents next to `settings.json`).
    pub fn state_file(&self, name: &str) -> Option<PathBuf> {
        self.path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|d| d.join(name))
    }

    fn stored_wins(&self, spec: &Spec) -> bool {
        !(self.locked && spec.lockable)
    }

    /// The effective value of a non-secret key and where it comes from.
    /// Unknown keys give `(Null, Default)`.
    pub fn get(&self, key: &str) -> (Value, Source) {
        let Some(spec) = self.spec(key) else {
            return (Value::Null, Source::Default);
        };
        if spec.secret {
            return (Value::Null, Source::Default);
        }
        if self.stored_wins(spec) {
            if let Some(v) = self.read().values.get(key) {
                return (v.clone(), Source::Stored);
            }
        }
        if let Some(v) = self.seeds.get(key) {
            return (v.clone(), Source::Env);
        }
        (spec.default.clone(), Source::Default)
    }

    /// A string setting (`None` when null or empty).
    pub fn string(&self, key: &str) -> Option<String> {
        self.get(key)
            .0
            .as_str()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    }

    pub fn bool(&self, key: &str) -> bool {
        self.get(key).0.as_bool().unwrap_or(false)
    }

    pub fn list(&self, key: &str) -> Vec<String> {
        self.get(key)
            .0
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A secret's value for the feature that uses it: the stored one when it
    /// opens, else the environment seed.
    pub fn secret(&self, key: &str) -> Option<Zeroizing<String>> {
        let spec = self.spec(key)?;
        if !spec.secret {
            return None;
        }
        if self.stored_wins(spec) {
            if let Some(sealed) = self.read().secrets.get(key) {
                if let Ok(bytes) = self.secrets.open(sealed) {
                    return String::from_utf8(bytes.to_vec()).ok().map(Zeroizing::new);
                }
            }
        }
        self.secret_seeds.get(key).cloned()
    }

    /// What the API says about a secret: set or not, the last characters,
    /// the source, and whether the stored copy is unreadable.
    pub fn secret_state(&self, key: &str) -> Value {
        let Some(spec) = self.spec(key).filter(|s| s.secret) else {
            return json!({ "set": false });
        };
        let unreadable = self.is_unreadable(key);
        let stored = self.stored_wins(spec) && self.read().secrets.contains_key(key);
        let (value, source) = if stored && !unreadable {
            (self.secret(key), Source::Stored)
        } else if let Some(seed) = self.secret_seeds.get(key) {
            (Some(seed.clone()), Source::Env)
        } else {
            (None, Source::Default)
        };
        json!({
            "set": value.is_some(),
            "hint": value.as_deref().and_then(|v| hint(v)),
            "source": if value.is_some() { Some(source.as_str()) } else { None },
            "unreadable": unreadable,
        })
    }

    /// Apply a change: validated against the specs, bounded, written to disk,
    /// audited. Returns the keys that changed.
    pub fn update(
        &self,
        values: BTreeMap<String, Value>,
        secrets: BTreeMap<String, Option<String>>,
        principal: &str,
    ) -> Result<Vec<String>, UpdateError> {
        let mut problems = Vec::new();
        let mut locked = Vec::new();
        let mut new_values = BTreeMap::new();
        for (key, value) in values {
            let Some(spec) = self.spec(&key) else {
                problems.push(format!("{key}: unknown setting"));
                continue;
            };
            if spec.secret {
                problems.push(format!("{key}: a secret, send it under \"secrets\""));
                continue;
            }
            if self.locked && spec.lockable {
                locked.push(key.clone());
                continue;
            }
            if value.is_null() {
                new_values.insert(key, None);
                continue;
            }
            match spec.kind.validate(&value) {
                Ok(v) => {
                    if let Some(check) = spec.validate {
                        if let Err(e) = check(&v) {
                            problems.push(format!("{key}: {e}"));
                            continue;
                        }
                    }
                    if v.to_string().len() > MAX_VALUE_BYTES {
                        problems.push(format!("{key}: larger than {MAX_VALUE_BYTES} bytes"));
                        continue;
                    }
                    new_values.insert(key, Some(v));
                }
                Err(e) => problems.push(format!("{key}: {e}")),
            }
        }
        let mut new_secrets = BTreeMap::new();
        for (key, value) in secrets {
            let Some(spec) = self.spec(&key) else {
                problems.push(format!("{key}: unknown setting"));
                continue;
            };
            if !spec.secret {
                problems.push(format!("{key}: not a secret, send it under \"values\""));
                continue;
            }
            if self.locked && spec.lockable {
                locked.push(key.clone());
                continue;
            }
            match value {
                None => {
                    new_secrets.insert(key, None);
                }
                Some(plain) => {
                    let plain = plain.trim().to_string();
                    if plain.is_empty() {
                        new_secrets.insert(key, None);
                    } else if plain.len() > MAX_SECRET_BYTES {
                        problems.push(format!("{key}: larger than {MAX_SECRET_BYTES} bytes"));
                    } else if plain.chars().any(|c| c.is_control()) {
                        problems.push(format!("{key}: contains control characters"));
                    } else if let Some(Err(e)) = spec
                        .validate
                        .map(|check| check(&Value::String(plain.clone())))
                    {
                        problems.push(format!("{key}: {e}"));
                    } else {
                        match self.secrets.seal(plain.as_bytes()) {
                            Ok(sealed) => {
                                new_secrets.insert(key, Some(sealed));
                            }
                            Err(e) => problems.push(format!("{key}: {e}")),
                        }
                    }
                }
            }
        }
        if !locked.is_empty() {
            return Err(UpdateError::Locked(locked));
        }
        if !problems.is_empty() {
            return Err(UpdateError::Invalid(problems));
        }
        let mut changed = Vec::new();
        let mut doc = self.write();
        let mut next = doc.clone();
        for (key, value) in new_values {
            let before = next.values.remove(&key);
            if let Some(v) = value {
                next.values.insert(key.clone(), v.clone());
                if before.as_ref() != Some(&v) {
                    changed.push(key);
                }
            } else if before.is_some() {
                changed.push(key);
            }
        }
        for (key, sealed) in new_secrets {
            let before = next.secrets.remove(&key);
            match sealed {
                Some(s) => {
                    next.secrets.insert(key.clone(), s);
                    changed.push(key);
                }
                None if before.is_some() => changed.push(key),
                None => {}
            }
        }
        if next.values.len() > MAX_KEYS {
            return Err(UpdateError::Invalid(vec![format!(
                "more than {MAX_KEYS} settings"
            )]));
        }
        if next.secrets.len() > MAX_SECRETS {
            return Err(UpdateError::Invalid(vec![format!(
                "more than {MAX_SECRETS} secrets"
            )]));
        }
        if changed.is_empty() {
            return Ok(changed);
        }
        next.version = DOC_VERSION;
        next.updated_at = now();
        self.persist(&next).map_err(UpdateError::Io)?;
        *doc = next;
        drop(doc);
        {
            let mut bad = self.write_unreadable();
            for key in &changed {
                bad.remove(key);
            }
        }
        for key in &changed {
            tracing::info!(
                target: "rustybin::settings",
                key = %key,
                source = "stored",
                principal = %principal,
                "setting changed"
            );
        }
        Ok(changed)
    }

    /// Forget the stored value of a key (back to the seed or the default).
    /// `Ok(false)` when nothing was stored.
    pub fn unset(&self, key: &str, principal: &str) -> Result<bool, UpdateError> {
        let Some(spec) = self.spec(key) else {
            return Err(UpdateError::Invalid(vec![format!(
                "{key}: unknown setting"
            )]));
        };
        if self.locked && spec.lockable {
            return Err(UpdateError::Locked(vec![key.to_string()]));
        }
        let mut doc = self.write();
        let mut next = doc.clone();
        let had = next.values.remove(key).is_some() | next.secrets.remove(key).is_some();
        if !had {
            return Ok(false);
        }
        next.version = DOC_VERSION;
        next.updated_at = now();
        self.persist(&next).map_err(UpdateError::Io)?;
        *doc = next;
        drop(doc);
        self.write_unreadable().remove(key);
        tracing::info!(
            target: "rustybin::settings",
            key = %key,
            source = "reset",
            principal = %principal,
            "setting reset"
        );
        Ok(true)
    }

    fn persist(&self, doc: &Doc) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let text = serde_json::to_vec_pretty(doc).map_err(|e| e.to_string())?;
        write_private(path, &text)
    }

    /// The API's view: every registered setting with its value, source and
    /// lock state, the groups, and the store's own state.
    pub fn view(&self) -> Value {
        let settings: Vec<Value> = self
            .specs
            .iter()
            .map(|spec| {
                let mut v = spec.describe();
                let locked = self.locked && spec.lockable;
                if let Value::Object(m) = &mut v {
                    if spec.secret {
                        m.insert("value".into(), self.secret_state(spec.key));
                        m.insert(
                            "source".into(),
                            self.secret_state(spec.key)["source"].clone(),
                        );
                    } else {
                        let (value, source) = self.get(spec.key);
                        m.insert("value".into(), value);
                        m.insert("source".into(), json!(source.as_str()));
                    }
                    m.insert("locked".into(), json!(locked));
                    m.insert(
                        "stored".into(),
                        json!(
                            self.read().values.contains_key(spec.key)
                                || self.read().secrets.contains_key(spec.key)
                        ),
                    );
                }
                v
            })
            .collect();
        let doc = self.read();
        json!({
            "persisted": self.persisted(),
            "locked": self.locked,
            "secrets_key": self.secrets.source().as_str(),
            "updated_at": if doc.updated_at.is_empty() { None } else { Some(doc.updated_at.clone()) },
            "unreadable_secrets": self.unreadable.read().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect::<Vec<_>>(),
            "groups": groups().iter().map(|g| json!({ "id": g.id, "title": g.title, "description": g.description })).collect::<Vec<_>>(),
            "settings": settings,
        })
    }
}

/// The last four characters of a secret, for secrets long enough that they
/// give nothing away.
fn hint(value: &str) -> Option<String> {
    let n = value.chars().count();
    if n < 8 {
        return None;
    }
    Some(value.chars().skip(n - 4).collect())
}

fn load_doc(path: &Path) -> Result<Doc, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            if bytes.len() > MAX_KEYS * MAX_VALUE_BYTES {
                return Err(format!("{} is too large", path.display()));
            }
            serde_json::from_slice::<Doc>(&bytes)
                .map_err(|e| format!("{} is not a settings document: {e}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Doc::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

// ── Routes ──────────────────────────────────────────────────────────

/// Who is making a control-plane change, for the audit line: the `sub` of
/// a control JWT, the admin token, or nobody (an open control plane).
pub fn principal(headers: &HeaderMap, config: &Config) -> String {
    if let Some(token) = crate::control_auth::presented_token(headers) {
        if let Ok((_, claims)) = crate::control_auth::verify_jwt_claims(token, config) {
            if let Some(sub) = claims.get("sub").and_then(|s| s.as_str()) {
                return format!("jwt:{}", sub.chars().take(80).collect::<String>());
            }
            return "jwt".into();
        }
        if config
            .admin_token
            .as_deref()
            .is_some_and(|t| crate::control_auth::constant_time_eq(token, t))
        {
            return "admin-token".into();
        }
    }
    "open".into()
}

fn refusal(e: UpdateError) -> Response {
    match e {
        UpdateError::Locked(keys) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "these settings are locked by RUSTYBIN_SETTINGS_LOCK: the environment is authoritative",
                "locked": keys,
            })),
        )
            .into_response(),
        UpdateError::Invalid(problems) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid settings", "problems": problems })),
        )
            .into_response(),
        UpdateError::Io(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("settings not saved: {e}") })),
        )
            .into_response(),
    }
}

async fn get_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    Json(state.settings.view()).into_response()
}

#[derive(Deserialize, Default)]
struct Patch {
    #[serde(default)]
    values: BTreeMap<String, Value>,
    #[serde(default)]
    secrets: BTreeMap<String, Option<String>>,
}

async fn patch_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Patch>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let Json(patch) = match body {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("expected {{ \"values\": {{}}, \"secrets\": {{}} }}: {}", e.body_text()) })),
            )
                .into_response()
        }
    };
    let who = principal(&headers, &state.config);
    match state.settings.update(patch.values, patch.secrets, &who) {
        Ok(changed) => {
            let mut view = state.settings.view();
            if let Value::Object(m) = &mut view {
                m.insert("changed".into(), json!(changed));
            }
            Json(view).into_response()
        }
        Err(e) => refusal(e),
    }
}

async fn delete_setting(
    State(state): State<AppState>,
    headers: HeaderMap,
    RoutePath(key): RoutePath<String>,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    if state.settings.spec(&key).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("no setting named {key:?}") })),
        )
            .into_response();
    }
    let who = principal(&headers, &state.config);
    match state.settings.unset(&key, &who) {
        Ok(reset) => {
            let mut view = state.settings.view();
            if let Value::Object(m) = &mut view {
                m.insert(
                    "changed".into(),
                    json!(if reset { vec![key] } else { vec![] }),
                );
            }
            Json(view).into_response()
        }
        Err(e) => refusal(e),
    }
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/_rustybin/settings",
            get(get_settings).patch(patch_settings),
        )
        .route(
            "/_rustybin/settings/{key}",
            axum::routing::delete(delete_setting),
        )
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/_rustybin/settings",
            &["GET", "PATCH"],
            category::CONTROL,
            "Instance settings: every registered key with its value, source and lock state; PATCH changes values and secrets",
        )
        .description(
            "Admin. A stored value wins over its environment variable, which wins over the default \
             (`source` says which). Secrets are returned as `{set, hint}`, never as values. With \
             RUSTYBIN_SETTINGS_LOCK the lockable keys are read-only (writes answer 409). With \
             RUSTYBIN_STATE_DIR changes persist (`persisted`), else they last until restart.",
        )
        .example(Example::get("Settings", "/_rustybin/settings"))
        .example(
            Example::new("Set the console title", "PATCH", "/_rustybin/settings")
                .json(r#"{ "values": { "console.title": "Payments team" } }"#),
        ),
        Endpoint::new(
            "/_rustybin/settings/{key}",
            &["DELETE"],
            category::CONTROL,
            "Forget a stored setting: back to its environment variable or default",
        )
        .example(Example::delete(
            "Reset the console title",
            "/_rustybin/settings/console.title",
        )),
    ]
}

pub fn openapi_paths() -> Value {
    let view = json!({ "$ref": "#/components/schemas/RustybinSettings" });
    json!({
        "/_rustybin/settings": {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Instance settings",
                "description": "Every registered setting with its value, where it comes from (`stored`, `env`, `default`) and whether RUSTYBIN_SETTINGS_LOCK holds it. Secrets are `{set, hint, source, unreadable}`. Admin.",
                "operationId": "getRustybinSettings",
                "responses": {
                    "200": { "description": "Settings", "content": { "application/json": { "schema": view } } },
                    "401": { "description": "Admin token required" },
                    "403": { "description": "Disabled in public mode without an admin token" }
                }
            },
            "patch": {
                "tags": ["Control Plane"],
                "summary": "Change settings",
                "description": "Values are validated against each setting's schema; a `null` value forgets the stored one. A secret value is sealed at rest; `null` clears it. Admin.",
                "operationId": "patchRustybinSettings",
                "requestBody": { "required": true, "content": { "application/json": { "schema": {
                    "type": "object",
                    "properties": {
                        "values": { "type": "object", "additionalProperties": {}, "description": "Non-secret settings by key" },
                        "secrets": { "type": "object", "additionalProperties": { "type": ["string", "null"] }, "description": "Secret settings by key" }
                    }
                } } } },
                "responses": {
                    "200": { "description": "The new settings, with `changed` listing the keys that changed", "content": { "application/json": { "schema": view } } },
                    "400": { "description": "Unknown keys or invalid values (`problems`)" },
                    "401": { "description": "Admin token required" },
                    "409": { "description": "Locked by RUSTYBIN_SETTINGS_LOCK (`locked`)" }
                }
            }
        },
        "/_rustybin/settings/{key}": {
            "delete": {
                "tags": ["Control Plane"],
                "summary": "Forget a stored setting",
                "operationId": "deleteRustybinSetting",
                "parameters": [{ "name": "key", "in": "path", "required": true, "schema": { "type": "string" }, "example": "console.title" }],
                "responses": {
                    "200": { "description": "The settings after the reset", "content": { "application/json": { "schema": view } } },
                    "404": { "description": "No such setting" },
                    "409": { "description": "Locked by RUSTYBIN_SETTINGS_LOCK" }
                }
            }
        }
    })
}

pub fn openapi_components() -> Value {
    json!({
        "RustybinSettings": {
            "type": "object",
            "properties": {
                "persisted": { "type": "boolean", "description": "Changes are written to RUSTYBIN_STATE_DIR" },
                "locked": { "type": "boolean", "description": "RUSTYBIN_SETTINGS_LOCK is on" },
                "secrets_key": { "type": "string", "enum": ["env", "file", "process"] },
                "updated_at": { "type": ["string", "null"], "format": "date-time" },
                "unreadable_secrets": { "type": "array", "items": { "type": "string" } },
                "groups": { "type": "array", "items": { "type": "object", "properties": {
                    "id": { "type": "string" }, "title": { "type": "string" }, "description": { "type": "string" } } } },
                "settings": { "type": "array", "items": { "type": "object", "properties": {
                    "key": { "type": "string" },
                    "group": { "type": "string" },
                    "title": { "type": "string" },
                    "description": { "type": "string" },
                    "schema": { "type": "object", "description": "JSON Schema of the value" },
                    "default": {},
                    "secret": { "type": "boolean" },
                    "env": { "type": ["string", "null"] },
                    "lockable": { "type": "boolean" },
                    "locked": { "type": "boolean" },
                    "stored": { "type": "boolean" },
                    "value": { "description": "The effective value, or `{set, hint, source, unreadable}` for a secret" },
                    "source": { "type": ["string", "null"], "enum": ["stored", "env", "default", null] }
                } } },
                "changed": { "type": "array", "items": { "type": "string" }, "description": "After a write: the keys that changed" }
            }
        }
    })
}

/// A store shared by the state and the modules that read settings.
pub type Shared = Arc<Settings>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        body_json, get_request, json_request, test_app_with, test_state_with,
    };
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_specs() -> Vec<Spec> {
        vec![
            Spec::new(
                "t.title",
                "console",
                "Title",
                "",
                Kind::Text { max_len: 10 },
                Value::Null,
            )
            .env("RUSTYBIN_CONSOLE_TITLE", |c| {
                c.console_title.clone().map(Value::String)
            }),
            Spec::new("t.flag", "console", "Flag", "", Kind::Bool, json!(false)),
            Spec::new(
                "t.n",
                "console",
                "N",
                "",
                Kind::Integer { min: 1, max: 10 },
                json!(3),
            ),
            Spec::new(
                "t.list",
                "egress",
                "List",
                "",
                Kind::List {
                    max_items: 3,
                    max_len: 20,
                },
                json!([]),
            ),
            Spec::new("t.url", "console", "Url", "", Kind::Url, Value::Null),
            Spec::new(
                "t.token",
                "leads",
                "Token",
                "",
                Kind::Text { max_len: 100 },
                Value::Null,
            )
            .secret()
            .env("RUSTYBIN_LEADS_WEBHOOK_TOKEN", |c| {
                c.self_service
                    .leads_webhook_token
                    .clone()
                    .map(Value::String)
            }),
        ]
    }

    fn store(config: &Config) -> Settings {
        Settings::with_specs(config, test_specs())
    }

    fn values(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn secrets(pairs: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.map(str::to_string)))
            .collect()
    }

    #[test]
    fn seal_and_open_round_trip_and_a_wrong_key() {
        let a = Secrets::random();
        let sealed = a.seal(b"hunter2-hunter2").expect("seal");
        assert_eq!(&*a.open(&sealed).expect("open"), b"hunter2-hunter2");
        let again = a.seal(b"hunter2-hunter2").expect("seal");
        assert_ne!(sealed, again, "a fresh nonce every time");
        let b = Secrets::random();
        assert!(b.open(&sealed).is_err());
        let mut broken = sealed.clone();
        broken.ciphertext.replace_range(0..2, "zz");
        assert!(a.open(&broken).is_err());
    }

    #[test]
    fn key_decoding() {
        let key = [7u8; 32];
        let b64 = base64::engine::general_purpose::STANDARD.encode(key);
        assert_eq!(decode_key(&b64).expect("b64"), key);
        let url = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key);
        assert_eq!(decode_key(&url).expect("url"), key);
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(decode_key(&hex).expect("hex"), key);
        assert!(decode_key("short").is_err());
        assert!(decode_key(&base64::engine::general_purpose::STANDARD.encode([1u8; 16])).is_err());
    }

    #[test]
    fn env_seed_stored_value_and_default_precedence() {
        let mut config = Config::for_tests();
        config.console_title = Some("From env".into());
        let s = store(&config);
        assert_eq!(s.get("t.title"), (json!("From env"), Source::Env));
        assert_eq!(s.get("t.n"), (json!(3), Source::Default));
        assert_eq!(s.get("nope"), (Value::Null, Source::Default));
        let changed = s
            .update(
                values(&[("t.title", json!(" Stored ")), ("t.n", json!("7"))]),
                secrets(&[]),
                "test",
            )
            .expect("update");
        assert_eq!(changed, vec!["t.n", "t.title"]);
        assert_eq!(s.get("t.title"), (json!("Stored"), Source::Stored));
        assert_eq!(s.get("t.n"), (json!(7), Source::Stored));
        // A null value forgets the stored one.
        s.update(values(&[("t.title", Value::Null)]), secrets(&[]), "test")
            .expect("null");
        assert_eq!(s.get("t.title"), (json!("From env"), Source::Env));
        assert!(s.unset("t.n", "test").expect("unset"));
        assert!(!s.unset("t.n", "test").expect("unset twice"));
        assert_eq!(s.get("t.n"), (json!(3), Source::Default));
        assert!(!s.persisted());
    }

    #[test]
    fn validation_refuses_bad_values_and_unknown_keys() {
        let s = store(&Config::for_tests());
        let err = s
            .update(
                values(&[
                    ("t.title", json!("far too long a title")),
                    ("t.n", json!(11)),
                    ("t.flag", json!("maybe")),
                    ("t.url", json!("ftp://x")),
                    ("t.list", json!(["a", "b", "c", "d"])),
                    ("t.token", json!("x")),
                    ("other", json!(1)),
                ]),
                secrets(&[("t.title", Some("x"))]),
                "test",
            )
            .expect_err("invalid");
        let UpdateError::Invalid(problems) = err else {
            panic!("{err:?}")
        };
        assert_eq!(problems.len(), 8, "{problems:?}");
        assert!(problems.iter().any(|p| p.starts_with("other: unknown")));
        assert!(problems.iter().any(|p| p.contains("t.token: a secret")));
        assert!(problems.iter().any(|p| p.contains("t.title: not a secret")));
        // Lists accept a comma string; booleans accept words.
        s.update(
            values(&[("t.list", json!(" a, b ,,c ")), ("t.flag", json!("yes"))]),
            secrets(&[]),
            "test",
        )
        .expect("ok");
        assert_eq!(s.list("t.list"), vec!["a", "b", "c"]);
        assert!(s.bool("t.flag"));
    }

    #[test]
    fn secrets_are_hinted_never_returned_and_seeded_from_env() {
        let mut config = Config::for_tests();
        config.self_service.leads_webhook_token = Some("env-secret-value".into());
        let s = store(&config);
        assert_eq!(&**s.secret("t.token").expect("seed"), "env-secret-value");
        let st = s.secret_state("t.token");
        assert_eq!(st["set"], true);
        assert_eq!(st["hint"], "alue");
        assert_eq!(st["source"], "env");
        s.update(
            values(&[]),
            secrets(&[("t.token", Some("stored-secret-k3Qx"))]),
            "test",
        )
        .expect("set");
        assert_eq!(
            &**s.secret("t.token").expect("stored"),
            "stored-secret-k3Qx"
        );
        let st = s.secret_state("t.token");
        assert_eq!(st["hint"], "k3Qx");
        assert_eq!(st["source"], "stored");
        let view = s.view().to_string();
        assert!(
            !view.contains("stored-secret"),
            "a value leaked into the view"
        );
        assert!(!view.contains("env-secret"), "a seed leaked into the view");
        // Short secrets get no hint; clearing falls back to the seed.
        s.update(values(&[]), secrets(&[("t.token", Some("tiny"))]), "test")
            .expect("set");
        assert_eq!(s.secret_state("t.token")["hint"], Value::Null);
        s.update(values(&[]), secrets(&[("t.token", None)]), "test")
            .expect("clear");
        assert_eq!(s.secret_state("t.token")["source"], "env");
        assert!(s.secret("t.title").is_none(), "not a secret");
    }

    #[test]
    fn lock_makes_the_environment_authoritative() {
        let mut config = Config::for_tests();
        config.console_title = Some("Env".into());
        let s = store(&config);
        s.update(
            values(&[("t.title", json!("Stored"))]),
            secrets(&[]),
            "test",
        )
        .expect("set");
        assert_eq!(s.get("t.title").1, Source::Stored);
        // The same document under the lock (the store is memory-only here, so
        // build one with a stored value first through a state directory).
        let tmp = tempfile::tempdir().expect("tmp");
        config.state_dir = Some(tmp.path().to_path_buf());
        let s = store(&config);
        s.update(
            values(&[("t.title", json!("Stored"))]),
            secrets(&[]),
            "test",
        )
        .expect("set");
        config.settings_lock = true;
        let locked = store(&config);
        assert_eq!(locked.get("t.title"), (json!("Env"), Source::Env));
        let err = locked
            .update(values(&[("t.title", json!("x"))]), secrets(&[]), "test")
            .expect_err("locked");
        assert_eq!(err, UpdateError::Locked(vec!["t.title".into()]));
        assert_eq!(
            locked.unset("t.title", "test").expect_err("locked"),
            UpdateError::Locked(vec!["t.title".into()])
        );
        assert_eq!(locked.view()["settings"][0]["locked"], true);
    }

    #[test]
    fn persists_across_a_restart_and_a_rotated_key_marks_secrets_unreadable() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut config = Config::for_tests();
        config.state_dir = Some(tmp.path().to_path_buf());
        let s = store(&config);
        assert!(s.persisted());
        assert_eq!(s.key_source(), KeySource::File);
        s.update(
            values(&[("t.title", json!("Kept"))]),
            secrets(&[("t.token", Some("sealed-value-abcd"))]),
            "test",
        )
        .expect("set");
        let on_disk = std::fs::read_to_string(tmp.path().join(SETTINGS_FILE)).expect("file");
        assert!(on_disk.contains("Kept"));
        assert!(
            !on_disk.contains("sealed-value"),
            "secret in the clear on disk"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(tmp.path().join(SETTINGS_FILE))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
            let mode = std::fs::metadata(tmp.path().join(KEY_FILE))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A restart: the same directory, the same generated key.
        let again = store(&config);
        assert_eq!(again.get("t.title"), (json!("Kept"), Source::Stored));
        assert_eq!(
            &**again.secret("t.token").expect("secret"),
            "sealed-value-abcd"
        );
        assert_eq!(again.secret_state("t.token")["hint"], "abcd");
        // A rotated key: the secret is unreadable, the value is kept, and
        // setting it again clears the flag.
        config.secrets_key = Some([9u8; 32]);
        let rotated = store(&config);
        assert_eq!(rotated.key_source(), KeySource::Env);
        assert_eq!(rotated.get("t.title"), (json!("Kept"), Source::Stored));
        assert!(rotated.secret("t.token").is_none());
        let st = rotated.secret_state("t.token");
        assert_eq!(st["unreadable"], true);
        assert_eq!(st["set"], false);
        assert_eq!(rotated.view()["unreadable_secrets"], json!(["t.token"]));
        rotated
            .update(
                values(&[]),
                secrets(&[("t.token", Some("new-value-wxyz"))]),
                "test",
            )
            .expect("set again");
        assert_eq!(rotated.secret_state("t.token")["unreadable"], false);
        assert_eq!(
            &**rotated.secret("t.token").expect("secret"),
            "new-value-wxyz"
        );
    }

    #[test]
    fn a_broken_document_starts_empty_without_being_lost() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::write(tmp.path().join(SETTINGS_FILE), b"{ not json").expect("write");
        let mut config = Config::for_tests();
        config.state_dir = Some(tmp.path().to_path_buf());
        let s = store(&config);
        assert_eq!(s.get("t.title").1, Source::Default);
        assert!(s.view()["updated_at"].is_null());
    }

    #[test]
    fn bounds_hold() {
        let s = store(&Config::for_tests());
        let long = "x".repeat(MAX_SECRET_BYTES + 1);
        let err = s
            .update(values(&[]), secrets(&[("t.token", Some(&long))]), "test")
            .expect_err("too long");
        assert!(matches!(err, UpdateError::Invalid(_)));
        let err = s
            .update(
                values(&[]),
                secrets(&[("t.token", Some("has\ncontrol"))]),
                "test",
            )
            .expect_err("control");
        assert!(matches!(err, UpdateError::Invalid(_)));
    }

    #[test]
    fn the_production_specs_are_consistent() {
        let all = specs();
        let mut keys = BTreeSet::new();
        for spec in &all {
            assert!(keys.insert(spec.key), "duplicate key {}", spec.key);
            assert!(
                groups().iter().any(|g| g.id == spec.group),
                "{} has an unknown group {}",
                spec.key,
                spec.group
            );
            assert!(spec.key.contains('.'), "{} is not dotted", spec.key);
            if !spec.secret && !spec.default.is_null() {
                spec.kind
                    .validate(&spec.default)
                    .unwrap_or_else(|e| panic!("{}: default invalid: {e}", spec.key));
            }
        }
        assert!(keys.contains("console.title"));
        assert!(keys.contains("egress.allow"));
        assert!(keys.contains("leads.webhook_token"));
    }

    #[tokio::test]
    async fn the_routes_read_write_and_reset_with_the_admin_token() {
        let mut config = Config::for_tests();
        config.admin_token = Some("adm".into());
        config.console_title = Some("Env title".into());
        let app = test_app_with(test_state_with(config));
        let resp = app
            .clone()
            .oneshot(get_request("/_rustybin/settings"))
            .await
            .expect("resp");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let admin = |mut req: Request<Body>| {
            req.headers_mut().insert(
                "x-rustybin-admin-token",
                axum::http::HeaderValue::from_static("adm"),
            );
            req
        };
        let resp = app
            .clone()
            .oneshot(admin(get_request("/_rustybin/settings")))
            .await
            .expect("resp");
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["persisted"], false);
        let title = v["settings"]
            .as_array()
            .expect("settings")
            .iter()
            .find(|s| s["key"] == "console.title")
            .expect("console.title")
            .clone();
        assert_eq!(title["value"], "Env title");
        assert_eq!(title["source"], "env");
        assert_eq!(title["env"], "RUSTYBIN_CONSOLE_TITLE");

        let resp = app
            .clone()
            .oneshot(admin(json_request(
                "PATCH",
                "/_rustybin/settings",
                &json!({ "values": { "console.title": "Payments" }, "secrets": { "leads.webhook_token": "hook-secret-9z9z" } }),
            )))
            .await
            .expect("resp");
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(
            v["changed"],
            json!(["console.title", "leads.webhook_token"])
        );
        let by_key = |v: &Value, key: &str| -> Value {
            v["settings"]
                .as_array()
                .expect("settings")
                .iter()
                .find(|s| s["key"] == key)
                .cloned()
                .expect("key")
        };
        assert_eq!(by_key(&v, "console.title")["source"], "stored");
        assert_eq!(by_key(&v, "leads.webhook_token")["value"]["hint"], "9z9z");
        assert!(!v.to_string().contains("hook-secret"));

        let resp = app
            .clone()
            .oneshot(admin(json_request(
                "PATCH",
                "/_rustybin/settings",
                &json!({ "values": { "console.title": "x".repeat(200) } }),
            )))
            .await
            .expect("resp");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(resp).await["problems"][0]
            .as_str()
            .expect("problem")
            .starts_with("console.title"));

        let resp = app
            .clone()
            .oneshot(admin(
                Request::builder()
                    .method("DELETE")
                    .uri("/_rustybin/settings/console.title")
                    .body(Body::empty())
                    .expect("req"),
            ))
            .await
            .expect("resp");
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(by_key(&v, "console.title")["source"], "env");
        let resp = app
            .clone()
            .oneshot(admin(
                Request::builder()
                    .method("DELETE")
                    .uri("/_rustybin/settings/no.such")
                    .body(Body::empty())
                    .expect("req"),
            ))
            .await
            .expect("resp");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_lock_answers_409_and_the_control_plane_auth_applies() {
        let mut config = crate::test_support::jwt_config();
        config.settings_lock = true;
        let app = test_app_with(test_state_with(config));
        let token = crate::test_support::control_jwt(Some("admin"));
        let resp = app
            .clone()
            .oneshot(crate::test_support::bearer_request(
                "GET",
                "/_rustybin/settings",
                None,
            ))
            .await
            .expect("resp");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let mut req = json_request(
            "PATCH",
            "/_rustybin/settings",
            &json!({ "values": { "console.title": "x" } }),
        );
        req.headers_mut().insert(
            "authorization",
            axum::http::HeaderValue::from_str(&format!("Bearer {token}")).expect("hv"),
        );
        let resp = app.clone().oneshot(req).await.expect("resp");
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(resp).await["locked"], json!(["console.title"]));
    }

    #[tokio::test]
    async fn the_console_title_setting_reaches_the_console() {
        let mut config = Config::for_tests();
        config.admin_token = Some("adm".into());
        let state = test_state_with(config);
        state
            .settings
            .update(
                values(&[("console.title", json!("Set <title>"))]),
                secrets(&[]),
                "test",
            )
            .expect("set");
        let app = test_app_with(state);
        let resp = app
            .clone()
            .oneshot(get_request("/ui/"))
            .await
            .expect("resp");
        let html = crate::test_support::body_string(resp).await;
        assert!(html.contains("rustybin-console-title\" content=\"Set &lt;title&gt;\""));
        let mut req = get_request("/_rustybin/status");
        req.headers_mut().insert(
            "x-rustybin-admin-token",
            axum::http::HeaderValue::from_static("adm"),
        );
        let v = body_json(app.oneshot(req).await.expect("resp")).await;
        assert_eq!(v["console"]["title"], "Set <title>");
    }
}
