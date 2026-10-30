//! The whole instance as one `rustybin.yaml` (version 2, step 09 of
//! `docs/plans/next/master-plan.md`): export it, diff two documents, apply
//! one with a dry run first, write it as a bundle (the document plus the
//! spec files it references) for a git working copy or a download.
//!
//! The document model is [`MocksFile`] (`crate::mocks::config_file`), the
//! version 1 mocks file grown by the sections `instance`, `settings`,
//! `secrets`, `harness` and `health`. A section that is absent is left
//! alone; one that is present is the whole truth for its part of the
//! instance when pruning. Secrets are never exported as values: `${VAR}`
//! references (resolved from the environment at load) or `secret:stored`
//! (the instance keeps its own).
//!
//! Each section is applied through its own service: the settings store,
//! the mock store (`Origin::Api`, as `POST /_rustybin/mocks/apply`), the
//! registry's file copy and published entries, the portal, the harness's
//! saved scenarios, the agents policy and the health toggle. The registry
//! and agents sections applied through the API are kept in
//! `<state>/config.json` so they survive a restart (`RUSTYBIN_CONFIG_FILE`
//! and `RUSTYBIN_CONFIG_GIT` reload theirs at startup anyway).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::mcp::manage::agents;
use crate::mocks::config_file::{self, MockDoc, MocksFile, RegistrySection, CURRENT_VERSION};
use crate::mocks::store::{Origin, Snapshot};
use crate::state::AppState;

pub mod api;
#[cfg(test)]
mod tests;

/// Most changes a diff lists (the rest is counted).
pub const MAX_CHANGES: usize = 500;
/// The file the applied registry and agents sections are kept in.
pub const APPLIED_FILE: &str = "config.json";
/// The document's file name in a bundle or a working copy.
pub const DOCUMENT_NAME: &str = "rustybin.yaml";
/// The directory of spec files in a bundle or a working copy.
pub const SPECS_DIR: &str = "specs";
/// The top directory of a downloaded bundle.
pub const BUNDLE_DIR: &str = "rustybin-config";
/// What a stored secret exports as.
pub const SECRET_STORED: &str = "secret:stored";

const TITLE_KEY: &str = "console.title";
/// Setting groups a document never carries: where the instance keeps its
/// own configuration is the deployment's business, and a pull must not
/// repoint or drop it.
pub const SKIPPED_GROUPS: [&str; 1] = ["git"];
const USER_KEY: &str = crate::login::USER_KEY;
const HEALTH_STATES: [&str; 2] = ["healthy", "unhealthy"];

// ── The sections beyond the mocks ───────────────────────────────────

/// `instance:`: a few settings under friendlier names.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceSection {
    /// The console title (`console.title`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console: Option<ConsoleSection>,
}

/// `instance.console`: the console user (`console.user`); the password is
/// never here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsoleSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// `harness:`: the saved scenarios, inline.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessSection {
    #[serde(default)]
    pub scenarios: Vec<ScenarioDoc>,
}

/// One saved scenario.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioDoc {
    pub name: String,
    pub scenario: Value,
}

/// `health:`: the state of the `/health` toggle.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthSection {
    /// `healthy` or `unhealthy`.
    pub initial: String,
}

/// The sections a file load applies beyond the mocks, registry, portal and
/// agents (which `crate::mocks::reload_files` applies itself).
#[derive(Debug, Default)]
pub struct Extra {
    pub instance: Option<InstanceSection>,
    pub settings: Option<BTreeMap<String, Value>>,
    pub secrets: Option<BTreeMap<String, Option<String>>>,
    pub harness: Option<HarnessSection>,
    pub health: Option<HealthSection>,
}

impl Extra {
    /// Move the extra sections out of a parsed file.
    pub fn take(file: &mut MocksFile) -> Self {
        Self {
            instance: file.instance.take(),
            settings: file.settings.take(),
            secrets: file.secrets.take(),
            harness: file.harness.take(),
            health: file.health.take(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.instance.is_none()
            && self.settings.is_none()
            && self.secrets.is_none()
            && self.harness.is_none()
            && self.health.is_none()
    }

    /// Put the sections back into a file (to apply them with the rest).
    fn into_file(self) -> MocksFile {
        MocksFile {
            version: CURRENT_VERSION,
            instance: self.instance,
            settings: self.settings,
            secrets: self.secrets,
            harness: self.harness,
            health: self.health,
            ..MocksFile::default()
        }
    }
}

// ── JSON Schema fragments (joined by `config_file::json_schema`) ────

pub fn instance_schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false,
        "description": "The console title and user (the settings console.title and console.user; the password is never in a document)",
        "properties": {
            "title": { "type": "string", "maxLength": crate::config::MAX_CONSOLE_TITLE_CHARS },
            "console": { "type": "object", "additionalProperties": false, "properties": {
                "user": { "type": "string", "maxLength": 64 }
            } }
        }
    })
}

pub fn settings_schema() -> Value {
    let keys: Vec<&str> = crate::settings::specs()
        .iter()
        .filter(|s| !s.secret)
        .map(|s| s.key)
        .collect();
    json!({
        "type": "object",
        "description": "Non-secret settings by dotted key (GET /_rustybin/settings lists them with their schema); a null value unsets the stored one",
        "propertyNames": { "enum": keys },
        "additionalProperties": true
    })
}

pub fn secrets_schema() -> Value {
    let keys: Vec<&str> = crate::settings::specs()
        .iter()
        .filter(|s| s.secret)
        .map(|s| s.key)
        .collect();
    json!({
        "type": "object",
        "description": "Secret settings as references, never values: ${VAR} reads the variable from the environment when the document is loaded or applied, secret:stored keeps what the instance holds, null unsets it",
        "propertyNames": { "enum": keys },
        "additionalProperties": { "type": ["string", "null"], "maxLength": crate::settings::MAX_SECRET_BYTES }
    })
}

pub fn harness_schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false,
        "description": "Saved harness scenarios (applied when harness mode is on)",
        "properties": {
            "scenarios": { "type": "array", "maxItems": crate::harness::saved::MAX_SAVED, "items": {
                "type": "object", "additionalProperties": false, "required": ["name", "scenario"],
                "properties": {
                    "name": { "type": "string", "maxLength": 64, "description": "Letters, digits, . _ - only" },
                    "scenario": { "type": "object", "description": "A scenario (GET /_rustybin/harness/schema)" }
                }
            } }
        }
    })
}

pub fn health_schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["initial"],
        "description": "The /health toggle",
        "properties": { "initial": { "type": "string", "enum": HEALTH_STATES } }
    })
}

// ── Export ──────────────────────────────────────────────────────────

/// How secrets appear in an export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretsMode {
    /// `${VAR}` for an environment-seeded secret, `secret:stored` for a
    /// stored one; the mocks' demo credentials as they are.
    Refs,
    /// No `secrets` section; the mocks' demo credentials redacted.
    Omit,
}

impl SecretsMode {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "refs" => Some(Self::Refs),
            "omit" => Some(Self::Omit),
            _ => None,
        }
    }
}

/// A spec file of a bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpecFile {
    /// Relative to the document (`specs/petstore.yaml`).
    pub path: String,
    pub text: String,
}

/// An exported instance: the document and, for a bundle, the spec files it
/// references (`spec:`) instead of inline specs.
#[derive(Debug)]
pub struct Export {
    pub file: MocksFile,
    pub specs: Vec<SpecFile>,
}

impl Export {
    /// The document as YAML with its header comment.
    pub fn yaml(&self) -> String {
        to_yaml(&self.file)
    }
}

/// A document as YAML, with a header that says how to use it.
pub fn to_yaml(file: &MocksFile) -> String {
    let body = serde_yaml::to_string(file).unwrap_or_default();
    format!(
        "# Rustybin configuration (rustybin.yaml, version 2): the whole instance.\n\
         # Apply with POST /_rustybin/config/apply (dry_run first), or load it at startup\n\
         # with RUSTYBIN_CONFIG_FILE. Secrets are references, never values: a $VAR-style\n\
         # reference reads the variable when the file is loaded, secret:stored keeps what\n\
         # the instance holds.\n{body}"
    )
}

/// Everything the instance is, as a document. `bundle` writes the specs as
/// files next to the document instead of inline.
pub fn export(state: &AppState, secrets: SecretsMode, bundle: bool) -> Export {
    let mut specs = Vec::new();
    let reveal = secrets == SecretsMode::Refs;
    let mut snapshots: Vec<Snapshot> = state
        .mocks
        .list()
        .into_iter()
        .filter(|s| s.origin != Origin::Sample && !s.owner.is_visitor())
        .collect();
    snapshots.sort_by(|a, b| a.name.cmp(&b.name));
    let mocks: Vec<MockDoc> = snapshots
        .iter()
        .map(|s| {
            let (doc, spec) = mock_doc(s, reveal, bundle);
            specs.extend(spec);
            doc
        })
        .collect();
    let (instance, settings, secret_refs) = settings_sections(state, secrets);
    let file = MocksFile {
        version: CURRENT_VERSION,
        instance,
        settings: Some(settings),
        secrets: secret_refs,
        mocks: Some(mocks),
        registry: Some(RegistrySection {
            servers: registry_servers(state),
        }),
        portal: Some(crate::portal::FileSection {
            items: portal_items(state),
        }),
        harness: state.harness.as_ref().map(|h| HarnessSection {
            scenarios: h
                .saved
                .list()
                .into_iter()
                .map(|s| ScenarioDoc {
                    name: s.name,
                    scenario: serde_json::to_value(&s.scenario).unwrap_or(Value::Null),
                })
                .collect(),
        }),
        agents: Some(agents_section(&state.mocks.file_agents())),
        health: Some(HealthSection {
            initial: if state.health.is_healthy() {
                "healthy".into()
            } else {
                "unhealthy".into()
            },
        }),
    };
    Export { file, specs }
}

fn mock_doc(s: &Snapshot, reveal: bool, bundle: bool) -> (MockDoc, Option<SpecFile>) {
    let c = &s.draft;
    let settings = &c.settings;
    let mut doc = MockDoc {
        name: s.name.clone(),
        spec: None,
        spec_inline: None,
        enabled: s.enabled,
        strip_server_prefix: settings.strip_server_prefix,
        responses: settings.responses.clone(),
        security: if reveal {
            settings.security.clone()
        } else {
            settings.security.redacted()
        },
        operations: settings.operations.clone(),
        mcp: settings.mcp.clone(),
    };
    if bundle {
        let source: &str = &c.source;
        let ext = if source.trim_start().starts_with('{') {
            "json"
        } else {
            "yaml"
        };
        let path = format!("{SPECS_DIR}/{}.{ext}", s.name);
        doc.spec = Some(path.clone());
        let mut text = source.to_string();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        (doc, Some(SpecFile { path, text }))
    } else {
        doc.spec_inline = Some(c.spec.root.clone());
        (doc, None)
    }
}

/// The stored settings: `instance` for the title and user, `settings` for
/// the rest, `secrets` as references.
#[allow(clippy::type_complexity)]
fn settings_sections(
    state: &AppState,
    secrets: SecretsMode,
) -> (
    Option<InstanceSection>,
    BTreeMap<String, Value>,
    Option<BTreeMap<String, Option<String>>>,
) {
    let mut instance = InstanceSection::default();
    let mut settings = BTreeMap::new();
    let mut refs = BTreeMap::new();
    for spec in state.settings.specs() {
        if SKIPPED_GROUPS.contains(&spec.group) {
            continue;
        }
        if spec.secret {
            if secrets != SecretsMode::Refs {
                continue;
            }
            let st = state.settings.secret_state(spec.key);
            if st["set"] != json!(true) {
                continue;
            }
            let reference = match (st["source"].as_str(), spec.env) {
                (Some("env"), Some(var)) => format!("${{{var}}}"),
                _ => SECRET_STORED.to_string(),
            };
            refs.insert(spec.key.to_string(), Some(reference));
            continue;
        }
        let (value, source) = state.settings.get(spec.key);
        if source != crate::settings::Source::Stored {
            continue;
        }
        match spec.key {
            TITLE_KEY => instance.title = value.as_str().map(str::to_string),
            USER_KEY => {
                instance.console = Some(ConsoleSection {
                    user: value.as_str().map(str::to_string),
                })
            }
            _ => {
                settings.insert(spec.key.to_string(), value);
            }
        }
    }
    let instance = (instance != InstanceSection::default()).then_some(instance);
    let refs = (secrets == SecretsMode::Refs).then_some(refs);
    (instance, settings, refs)
}

/// The registry's file copy plus the entries published through the API.
fn registry_servers(state: &AppState) -> Vec<Value> {
    let mut all = state.mocks.file_registry().0;
    for s in state.registry.published_servers() {
        let dup = all
            .iter()
            .any(|o| o["name"] == s["name"] && o["version"] == s["version"]);
        if !dup {
            all.push(s);
        }
    }
    all
}

/// The portal's file items plus the ones published through the API (never
/// a visitor's).
fn portal_items(state: &AppState) -> Vec<crate::portal::FileItem> {
    let mut items = state.mocks.file_portal().0;
    let mut published: Vec<_> = state
        .portal
        .published()
        .into_iter()
        .filter(|(_, p)| !p.visitor())
        .collect();
    published.sort_by(|a, b| a.0.cmp(&b.0));
    for (id, p) in published {
        if items.iter().any(|i| i.id == id) {
            continue;
        }
        items.push(crate::portal::FileItem {
            id,
            title: p.title,
            summary: p.summary,
            tags: (!p.tags.is_empty()).then_some(p.tags),
            contact: p.contact,
            order: (p.order != 0).then_some(p.order),
            credentials: (!p.credentials).then_some(false),
        });
    }
    items
}

/// A usable agents policy back as a file section.
pub fn agents_section(policy: &agents::FilePolicy) -> agents::FileSection {
    agents::FileSection {
        default_ceiling: policy.default_ceiling.map(|c| c.as_str().to_string()),
        rules: policy
            .rules
            .iter()
            .map(|r| agents::FileRule {
                matcher: agents::Match {
                    name: r.name.clone(),
                    credential: r.credential.clone(),
                },
                ceiling: r.ceiling.map(|c| c.as_str().to_string()),
                allow: r.allow.clone(),
                deny: r.deny.clone(),
                note: r.note.clone(),
            })
            .collect(),
    }
}

// ── Bundles ─────────────────────────────────────────────────────────

/// Write an export into a directory: `rustybin.yaml` and `specs/`. Spec
/// files of mocks that are gone are removed; nothing else in the directory
/// is touched.
pub fn write_dir(export: &Export, dir: &Path) -> Result<(), String> {
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    std::fs::create_dir_all(dir).map_err(|e| err(dir, e))?;
    let specs = dir.join(SPECS_DIR);
    std::fs::create_dir_all(&specs).map_err(|e| err(&specs, e))?;
    let keep: BTreeSet<&str> = export
        .specs
        .iter()
        .filter_map(|s| s.path.strip_prefix("specs/"))
        .collect();
    if let Ok(entries) = std::fs::read_dir(&specs) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_file() && !keep.contains(name.as_str()) {
                std::fs::remove_file(&path).map_err(|e| err(&path, e))?;
            }
        }
    }
    for spec in &export.specs {
        let path = dir.join(&spec.path);
        std::fs::write(&path, spec.text.as_bytes()).map_err(|e| err(&path, e))?;
    }
    let doc = dir.join(DOCUMENT_NAME);
    std::fs::write(&doc, export.yaml().as_bytes()).map_err(|e| err(&doc, e))
}

/// An export as a gzipped tar (`rustybin-config/rustybin.yaml` and
/// `rustybin-config/specs/*`).
pub fn tar_gz(export: &Export) -> Vec<u8> {
    use std::io::Write;
    let mtime = u64::try_from(chrono::Utc::now().timestamp()).unwrap_or(0);
    let mut tar = Vec::new();
    tar_entry(
        &mut tar,
        &format!("{BUNDLE_DIR}/{DOCUMENT_NAME}"),
        export.yaml().as_bytes(),
        mtime,
    );
    for spec in &export.specs {
        tar_entry(
            &mut tar,
            &format!("{BUNDLE_DIR}/{}", spec.path),
            spec.text.as_bytes(),
            mtime,
        );
    }
    tar.extend_from_slice(&[0u8; 1024]);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = gz.write_all(&tar);
    gz.finish().unwrap_or_default()
}

/// One ustar entry (names stay well under 100 bytes: a mock name is at
/// most 40 characters).
fn tar_entry(out: &mut Vec<u8>, name: &str, data: &[u8], mtime: u64) {
    fn put(h: &mut [u8; 512], at: usize, len: usize, s: &str) {
        let b = s.as_bytes();
        let n = b.len().min(len);
        h[at..at + n].copy_from_slice(&b[..n]);
    }
    let mut h = [0u8; 512];
    put(&mut h, 0, 100, name);
    put(&mut h, 100, 8, "0000644\0");
    put(&mut h, 108, 8, "0000000\0");
    put(&mut h, 116, 8, "0000000\0");
    put(&mut h, 124, 12, &format!("{:011o}\0", data.len()));
    put(&mut h, 136, 12, &format!("{mtime:011o}\0"));
    h[156] = b'0';
    put(&mut h, 257, 6, "ustar\0");
    put(&mut h, 263, 2, "00");
    put(&mut h, 265, 32, "rustybin");
    put(&mut h, 297, 32, "rustybin");
    for b in &mut h[148..156] {
        *b = b' ';
    }
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    put(&mut h, 148, 8, &format!("{sum:06o}\0 "));
    out.extend_from_slice(&h);
    out.extend_from_slice(data);
    let pad = (512 - data.len() % 512) % 512;
    out.extend(std::iter::repeat_n(0u8, pad));
}

// ── Diff ────────────────────────────────────────────────────────────

/// One difference between two documents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Change {
    pub section: &'static str,
    /// The entry within the section (a mock name, a setting key, an item
    /// id); empty for the section itself.
    pub key: String,
    /// `added`, `removed` or `changed`.
    pub action: &'static str,
    pub detail: String,
}

impl Change {
    fn new(
        section: &'static str,
        key: impl Into<String>,
        action: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            section,
            key: key.into(),
            action,
            detail: detail.into(),
        }
    }

    /// `mocks.petstore: spec changed (3 operations added)`.
    pub fn line(&self) -> String {
        let what = if self.detail.is_empty() {
            self.action.to_string()
        } else {
            self.detail.clone()
        };
        if self.key.is_empty() {
            format!("{}: {what}", self.section)
        } else {
            format!("{}.{}: {what}", self.section, self.key)
        }
    }
}

fn short(v: &Value) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if s.chars().count() > 48 {
        format!("{}...", s.chars().take(45).collect::<String>())
    } else {
        s
    }
}

/// What `after` changes against `before` (an export of the instance), for
/// the sections `after` carries. `base` resolves `spec:` file paths.
pub fn diff(before: &MocksFile, after: &MocksFile, base: Option<&Path>) -> Vec<Change> {
    let mut out = Vec::new();
    if let Some(a) = &after.instance {
        let b = before.instance.clone().unwrap_or_default();
        value_change(
            &mut out,
            "instance",
            "title",
            b.title.as_deref(),
            a.title.as_deref(),
        );
        let bu = b.console.and_then(|c| c.user);
        let au = a.console.as_ref().and_then(|c| c.user.clone());
        value_change(
            &mut out,
            "instance",
            "console.user",
            bu.as_deref(),
            au.as_deref(),
        );
    }
    if let Some(a) = &after.settings {
        let empty = BTreeMap::new();
        let b = before.settings.as_ref().unwrap_or(&empty);
        map_changes(&mut out, "settings", b, a, short);
    }
    if let Some(a) = &after.secrets {
        let empty = BTreeMap::new();
        let b = before.secrets.as_ref().unwrap_or(&empty);
        map_changes(&mut out, "secrets", b, a, |v| {
            v.clone().unwrap_or_else(|| "null".into())
        });
    }
    if let Some(a) = &after.mocks {
        mock_changes(&mut out, before.mocks(), a, base);
    }
    if let Some(a) = &after.registry {
        let b: BTreeMap<String, &Value> = before
            .registry
            .iter()
            .flat_map(|r| &r.servers)
            .map(|s| (server_key(s), s))
            .collect();
        let a: BTreeMap<String, &Value> = a.servers.iter().map(|s| (server_key(s), s)).collect();
        map_changes(&mut out, "registry", &b, &a, |_| String::new());
    }
    if let Some(a) = &after.portal {
        let b: BTreeMap<String, Value> = before
            .portal
            .iter()
            .flat_map(|p| &p.items)
            .map(|i| (i.id.clone(), serde_json::to_value(i).unwrap_or(Value::Null)))
            .collect();
        let a: BTreeMap<String, Value> = a
            .items
            .iter()
            .map(|i| (i.id.clone(), serde_json::to_value(i).unwrap_or(Value::Null)))
            .collect();
        map_changes(&mut out, "portal", &b, &a, |_| String::new());
    }
    if let Some(a) = &after.harness {
        let b: BTreeMap<String, &Value> = before
            .harness
            .iter()
            .flat_map(|h| &h.scenarios)
            .map(|s| (s.name.clone(), &s.scenario))
            .collect();
        let a: BTreeMap<String, &Value> = a
            .scenarios
            .iter()
            .map(|s| (s.name.clone(), &s.scenario))
            .collect();
        map_changes(&mut out, "harness", &b, &a, |_| String::new());
    }
    if let Some(a) = &after.agents {
        let b = before.agents.clone().unwrap_or_default();
        value_change(
            &mut out,
            "agents",
            "default_ceiling",
            b.default_ceiling.as_deref(),
            a.default_ceiling.as_deref(),
        );
        if b.rules != a.rules {
            out.push(Change::new(
                "agents",
                "rules",
                "changed",
                format!(
                    "rules changed ({} before, {} after)",
                    b.rules.len(),
                    a.rules.len()
                ),
            ));
        }
    }
    if let Some(a) = &after.health {
        let b = before.health.as_ref().map(|h| h.initial.as_str());
        value_change(&mut out, "health", "initial", b, Some(a.initial.as_str()));
    }
    if out.len() > MAX_CHANGES {
        let more = out.len() - MAX_CHANGES;
        out.truncate(MAX_CHANGES);
        out.push(Change::new(
            "document",
            "",
            "changed",
            format!("{more} more change(s) not listed"),
        ));
    }
    out
}

fn value_change(
    out: &mut Vec<Change>,
    section: &'static str,
    key: &str,
    before: Option<&str>,
    after: Option<&str>,
) {
    match (before, after) {
        (None, Some(a)) => out.push(Change::new(section, key, "added", format!("set to {a:?}"))),
        (Some(b), None) => out.push(Change::new(
            section,
            key,
            "removed",
            format!("{b:?} removed"),
        )),
        (Some(b), Some(a)) if b != a => out.push(Change::new(
            section,
            key,
            "changed",
            format!("changed from {b:?} to {a:?}"),
        )),
        _ => {}
    }
}

fn map_changes<V: PartialEq>(
    out: &mut Vec<Change>,
    section: &'static str,
    before: &BTreeMap<String, V>,
    after: &BTreeMap<String, V>,
    show: impl Fn(&V) -> String,
) {
    for (k, v) in after {
        match before.get(k) {
            None => {
                let s = show(v);
                out.push(Change::new(
                    section,
                    k.as_str(),
                    "added",
                    if s.is_empty() {
                        "added".to_string()
                    } else {
                        format!("added ({s})")
                    },
                ));
            }
            Some(b) if b != v => {
                let s = show(v);
                out.push(Change::new(
                    section,
                    k.as_str(),
                    "changed",
                    if s.is_empty() {
                        "changed".to_string()
                    } else {
                        format!("changed to {s}")
                    },
                ));
            }
            _ => {}
        }
    }
    for k in before.keys() {
        if !after.contains_key(k) {
            out.push(Change::new(section, k.as_str(), "removed", "removed"));
        }
    }
}

fn server_key(s: &Value) -> String {
    format!(
        "{}@{}",
        s["name"].as_str().unwrap_or("?"),
        s["version"].as_str().unwrap_or("?")
    )
}

/// The spec of a document entry as a value: inline, or read next to the
/// document when `base` is known.
fn spec_value(doc: &MockDoc, base: Option<&Path>) -> Result<Value, String> {
    match (&doc.spec, &doc.spec_inline) {
        (_, Some(Value::String(s))) => crate::mocks::spec::parse_document(s),
        (_, Some(v)) => Ok(v.clone()),
        (Some(path), None) => {
            let Some(base) = base else {
                return Err(format!("spec {path} is a file: not readable here"));
            };
            let p = Path::new(path);
            let full = if p.is_absolute() {
                p.to_path_buf()
            } else {
                base.join(p)
            };
            let text = std::fs::read_to_string(&full).map_err(|e| format!("spec {path}: {e}"))?;
            crate::mocks::spec::parse_document(&text)
        }
        (None, None) => Err("no spec".into()),
    }
}

const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// `METHOD /path` of every operation of a spec.
fn operations(spec: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if let Some(paths) = spec.get("paths").and_then(Value::as_object) {
        for (path, item) in paths {
            if let Some(item) = item.as_object() {
                for m in METHODS {
                    if item.contains_key(m) {
                        out.insert(format!("{} {path}", m.to_ascii_uppercase()));
                    }
                }
            }
        }
    }
    out
}

fn mock_changes(out: &mut Vec<Change>, before: &[MockDoc], after: &[MockDoc], base: Option<&Path>) {
    for a in after {
        let Some(b) = before.iter().find(|b| b.name == a.name) else {
            out.push(Change::new("mocks", a.name.as_str(), "added", "added"));
            continue;
        };
        let mut parts = Vec::new();
        match (spec_value(b, base), spec_value(a, base)) {
            (Ok(bs), Ok(as_)) => {
                if bs != as_ {
                    let (bo, ao) = (operations(&bs), operations(&as_));
                    let added = ao.difference(&bo).count();
                    let removed = bo.difference(&ao).count();
                    let mut d = Vec::new();
                    if added > 0 {
                        d.push(format!("{added} operation(s) added"));
                    }
                    if removed > 0 {
                        d.push(format!("{removed} operation(s) removed"));
                    }
                    parts.push(if d.is_empty() {
                        "spec changed".to_string()
                    } else {
                        format!("spec changed ({})", d.join(", "))
                    });
                }
            }
            (_, Err(e)) => parts.push(e),
            (Err(_), Ok(_)) => parts.push("spec changed".into()),
        }
        if b.enabled != a.enabled {
            parts.push(if a.enabled { "enabled" } else { "disabled" }.to_string());
        }
        if b.strip_server_prefix != a.strip_server_prefix {
            parts.push("strip_server_prefix changed".into());
        }
        if jv(&b.responses) != jv(&a.responses) {
            parts.push("responses changed".into());
        }
        if jv(&b.security) != jv(&a.security) {
            parts.push("security changed".into());
        }
        if jv(&b.mcp) != jv(&a.mcp) {
            parts.push("mcp changed".into());
        }
        let ops: BTreeSet<&String> = b
            .operations
            .keys()
            .chain(a.operations.keys())
            .filter(|k| jv(&b.operations.get(*k)) != jv(&a.operations.get(*k)))
            .collect();
        if !ops.is_empty() {
            let names: Vec<&str> = ops.iter().take(5).map(|s| s.as_str()).collect();
            parts.push(format!(
                "operations changed ({}{})",
                names.join(", "),
                if ops.len() > 5 { ", ..." } else { "" }
            ));
        }
        if !parts.is_empty() {
            out.push(Change::new(
                "mocks",
                a.name.as_str(),
                "changed",
                parts.join(", "),
            ));
        }
    }
    for b in before {
        if !after.iter().any(|a| a.name == b.name) {
            out.push(Change::new("mocks", b.name.as_str(), "removed", "removed"));
        }
    }
}

fn jv<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

// ── Apply ───────────────────────────────────────────────────────────

/// How a document is applied.
#[derive(Clone, Debug)]
pub struct ApplyOptions {
    /// Remove what a present section does not name.
    pub prune: bool,
    /// Report only.
    pub dry_run: bool,
    /// Who applies (audit lines, the portal's `by`).
    pub principal: String,
    /// Where `spec:` file paths resolve (a working copy, a file's directory).
    pub base: Option<PathBuf>,
}

/// Apply a document through every section's service. The answer carries
/// the diff computed before anything changed (`changes`), each section's
/// result and the problems (an entry that could not be applied never stops
/// the others).
pub fn apply(state: &AppState, file: MocksFile, opts: &ApplyOptions) -> Value {
    if !config_file::VERSIONS.contains(&file.version) {
        return json!({
            "ok": false,
            "dry_run": opts.dry_run,
            "problems": [format!("unsupported version {} (expected 1 or 2)", file.version)],
            "changes": [],
        });
    }
    let current = export(state, SecretsMode::Refs, false).file;
    let changes = diff(&current, &file, opts.base.as_deref());
    let sections = file.sections();
    let mut problems = Vec::new();
    let mut results = serde_json::Map::new();
    let mut put = |key: &str, v: Option<Value>| {
        if let Some(v) = v {
            results.insert(key.to_string(), v);
        }
    };
    put(
        "settings",
        apply_settings(state, &file, opts, &mut problems),
    );
    put("secrets", apply_secrets(state, &file, opts, &mut problems));
    put("mocks", apply_mocks(state, file.mocks, opts, &mut problems));
    put(
        "registry",
        apply_registry(state, file.registry, opts, &mut problems),
    );
    put(
        "portal",
        apply_portal(state, file.portal, opts, &mut problems),
    );
    put(
        "harness",
        apply_harness(state, file.harness, opts, &mut problems),
    );
    put(
        "agents",
        apply_agents(state, file.agents, opts, &mut problems),
    );
    put(
        "health",
        apply_health(state, file.health, opts, &mut problems),
    );
    if !opts.dry_run {
        if let Err(e) = save_applied(state) {
            problems.push(e);
        }
        tracing::info!(
            target: "rustybin::config",
            principal = %opts.principal,
            prune = opts.prune,
            sections = %sections.join(","),
            changes = changes.len(),
            problems = problems.len(),
            "configuration document applied"
        );
    }
    let summary: Vec<String> = changes.iter().map(Change::line).collect();
    json!({
        "ok": problems.is_empty(),
        "dry_run": opts.dry_run,
        "prune": opts.prune,
        "sections": sections,
        "changes": changes,
        "summary": summary,
        "results": results,
        "problems": problems,
    })
}

/// The sections of a loaded file beyond the mocks, registry, portal and
/// agents (`RUSTYBIN_CONFIG_FILE` at startup and on reload): applied
/// without pruning. Returns the problems.
pub fn apply_extra(state: &AppState, extra: Extra, principal: &str) -> Vec<String> {
    if extra.is_empty() {
        return Vec::new();
    }
    let file = extra.into_file();
    let opts = ApplyOptions {
        prune: false,
        dry_run: false,
        principal: principal.to_string(),
        base: None,
    };
    let mut problems = Vec::new();
    apply_settings(state, &file, &opts, &mut problems);
    apply_secrets(state, &file, &opts, &mut problems);
    apply_harness(state, file.harness, &opts, &mut problems);
    apply_health(state, file.health, &opts, &mut problems);
    problems
}

fn apply_settings(
    state: &AppState,
    file: &MocksFile,
    opts: &ApplyOptions,
    problems: &mut Vec<String>,
) -> Option<Value> {
    if file.instance.is_none() && file.settings.is_none() {
        return None;
    }
    let mut values: BTreeMap<String, Value> = BTreeMap::new();
    if let Some(i) = &file.instance {
        if let Some(t) = &i.title {
            values.insert(TITLE_KEY.into(), json!(t));
        }
        if let Some(u) = i.console.as_ref().and_then(|c| c.user.clone()) {
            values.insert(USER_KEY.into(), json!(u));
        }
    }
    for (k, v) in file.settings.iter().flatten() {
        match state.settings.spec(k) {
            None => problems.push(format!("settings.{k}: unknown setting (skipped)")),
            Some(spec) if spec.secret => problems.push(format!(
                "settings.{k}: a secret: put a reference under secrets (skipped)"
            )),
            Some(_) => {
                values.insert(k.clone(), v.clone());
            }
        }
    }
    let mut unset = Vec::new();
    if opts.prune && file.settings.is_some() {
        for spec in state.settings.specs() {
            if spec.secret || values.contains_key(spec.key) || SKIPPED_GROUPS.contains(&spec.group)
            {
                continue;
            }
            if state.settings.get(spec.key).1 == crate::settings::Source::Stored {
                unset.push(spec.key.to_string());
            }
        }
        unset.sort();
    }
    let set: Vec<String> = values.keys().cloned().collect();
    if opts.dry_run {
        return Some(json!({ "set": set, "unset": unset }));
    }
    let mut changed = Vec::new();
    match state
        .settings
        .update(values, BTreeMap::new(), &opts.principal)
    {
        Ok(c) => changed = c,
        Err(e) => settings_error("settings", e, problems),
    }
    let mut removed = Vec::new();
    for key in &unset {
        match state.settings.unset(key, &opts.principal) {
            Ok(true) => removed.push(key.clone()),
            Ok(false) => {}
            Err(e) => settings_error(&format!("settings.{key}"), e, problems),
        }
    }
    Some(json!({ "set": set, "changed": changed, "unset": removed }))
}

fn settings_error(what: &str, e: crate::settings::UpdateError, problems: &mut Vec<String>) {
    use crate::settings::UpdateError;
    match e {
        UpdateError::Locked(keys) => problems.push(format!(
            "{what}: locked by RUSTYBIN_SETTINGS_LOCK ({}): not applied",
            keys.join(", ")
        )),
        UpdateError::Invalid(list) => {
            for p in list {
                problems.push(format!("{what}: {p} (none of the batch applied)"));
            }
        }
        UpdateError::Io(e) => problems.push(format!("{what}: {e}")),
    }
}

fn apply_secrets(
    state: &AppState,
    file: &MocksFile,
    opts: &ApplyOptions,
    problems: &mut Vec<String>,
) -> Option<Value> {
    let section = file.secrets.as_ref()?;
    let mut set: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut kept = Vec::new();
    for (k, v) in section {
        match state.settings.spec(k) {
            None => {
                problems.push(format!("secrets.{k}: unknown setting (skipped)"));
                continue;
            }
            Some(spec) if !spec.secret => {
                problems.push(format!(
                    "secrets.{k}: not a secret: put it under settings (skipped)"
                ));
                continue;
            }
            Some(_) => {}
        }
        match v.as_deref().map(str::trim) {
            None => {
                set.insert(k.clone(), None);
            }
            Some(SECRET_STORED) => kept.push(k.clone()),
            Some("") => problems.push(format!(
                "secrets.{k}: empty (an unset variable?): kept as it is"
            )),
            Some(raw) => {
                let (value, missing) = config_file::expand_env(raw, |n| std::env::var(n).ok());
                if !missing.is_empty() {
                    problems.push(format!(
                        "secrets.{k}: {} not set in the environment: kept as it is",
                        missing.join(", ")
                    ));
                } else if value.trim().is_empty() {
                    problems.push(format!(
                        "secrets.{k}: the reference is empty: kept as it is"
                    ));
                } else {
                    set.insert(k.clone(), Some(value));
                }
            }
        }
    }
    let mut unset = Vec::new();
    if opts.prune {
        for spec in state.settings.specs() {
            if !spec.secret
                || section.contains_key(spec.key)
                || SKIPPED_GROUPS.contains(&spec.group)
            {
                continue;
            }
            let st = state.settings.secret_state(spec.key);
            if st["source"] == json!("stored") {
                unset.push(spec.key.to_string());
                set.insert(spec.key.to_string(), None);
            }
        }
    }
    let names: Vec<String> = set
        .iter()
        .filter(|(_, v)| v.is_some())
        .map(|(k, _)| k.clone())
        .collect();
    let cleared: Vec<String> = set
        .iter()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.clone())
        .collect();
    if opts.dry_run {
        return Some(json!({ "set": names, "kept": kept, "unset": cleared }));
    }
    let mut changed = Vec::new();
    if !set.is_empty() {
        match state.settings.update(BTreeMap::new(), set, &opts.principal) {
            Ok(c) => changed = c,
            Err(e) => settings_error("secrets", e, problems),
        }
    }
    Some(json!({ "set": names, "kept": kept, "unset": cleared, "changed": changed }))
}

fn apply_mocks(
    state: &AppState,
    mocks: Option<Vec<MockDoc>>,
    opts: &ApplyOptions,
    problems: &mut Vec<String>,
) -> Option<Value> {
    let mocks = mocks?;
    let mut docs = Vec::new();
    for doc in mocks {
        if let Err(e) = crate::mocks::service::check_name(&doc.name) {
            problems.push(format!(
                "mocks.{}: {} (skipped)",
                doc.name,
                crate::mocks::service::ServiceError::from(e).message()
            ));
            continue;
        }
        docs.push(doc);
    }
    let names: Vec<String> = docs.iter().map(|d| d.name.clone()).collect();
    let (defs, errors) = config_file::definitions(
        docs,
        opts.base.as_deref(),
        state.mocks.limits().max_spec_bytes as u64,
    );
    for e in &errors {
        problems.push(format!(
            "mocks.{}: {}",
            e["name"].as_str().unwrap_or("?"),
            e["error"].as_str().unwrap_or("?")
        ));
    }
    if opts.dry_run {
        let (mut create, mut update) = (Vec::new(), Vec::new());
        for d in &defs {
            if state.mocks.get(&d.name).is_some() {
                update.push(d.name.clone());
            } else {
                create.push(d.name.clone());
            }
        }
        let delete: Vec<String> = if opts.prune {
            state
                .mocks
                .list()
                .into_iter()
                .filter(|s| s.origin == Origin::Api && !names.contains(&s.name))
                .map(|s| s.name)
                .collect()
        } else {
            Vec::new()
        };
        return Some(json!({
            "would_create": create, "would_update": update, "would_delete": delete,
        }));
    }
    let report = state
        .mocks
        .apply(defs, Origin::Api, opts.prune.then_some(Origin::Api));
    for e in &report.errors {
        problems.push(format!(
            "mocks.{}: {}",
            e["name"].as_str().unwrap_or("?"),
            e["error"].as_str().unwrap_or("?")
        ));
    }
    Some(json!({
        "created": report.created, "updated": report.updated,
        "unchanged": report.unchanged, "deleted": report.deleted,
    }))
}

fn apply_registry(
    state: &AppState,
    section: Option<RegistrySection>,
    opts: &ApplyOptions,
    problems: &mut Vec<String>,
) -> Option<Value> {
    let section = section?;
    let (valid, section_problems) = crate::mcp::registry::file_entries(Some(section.servers));
    problems.extend(section_problems);
    let keys: BTreeSet<String> = valid.iter().map(server_key).collect();
    let published = state.registry.published_servers();
    let moved = published
        .iter()
        .filter(|s| keys.contains(&server_key(s)))
        .count();
    let pruned = if opts.prune {
        published.len() - moved
    } else {
        0
    };
    if opts.dry_run {
        return Some(
            json!({ "entries": valid.len(), "published_moved": moved, "published_removed": pruned }),
        );
    }
    state.mocks.set_file_registry(valid);
    let prune = opts.prune;
    let removed = state
        .registry
        .retain_published(|n, v| !prune && !keys.contains(&format!("{n}@{v}")));
    Some(
        json!({ "entries": keys.len(), "published_moved": moved, "published_removed": removed.saturating_sub(moved) }),
    )
}

fn apply_portal(
    state: &AppState,
    section: Option<crate::portal::FileSection>,
    opts: &ApplyOptions,
    problems: &mut Vec<String>,
) -> Option<Value> {
    let section = section?;
    let (items, section_problems) = crate::portal::file_items(Some(section));
    problems.extend(section_problems);
    let file_defined: BTreeSet<String> = state
        .mocks
        .file_portal()
        .0
        .into_iter()
        .map(|i| i.id)
        .collect();
    let published = state.portal.published();
    let mut publish = Vec::new();
    let mut kept = Vec::new();
    for item in items {
        if file_defined.contains(&item.id) {
            kept.push(item.id);
            continue;
        }
        let owner = match crate::portal::ItemId::parse(&item.id) {
            Ok(crate::portal::ItemId::Mock(name)) => match state.mocks.get(&name) {
                Some(s) if s.owner.is_visitor() => {
                    problems.push(format!(
                        "portal.items.{}: a visitor's mock (skipped)",
                        item.id
                    ));
                    continue;
                }
                Some(s) => Some(s.owner),
                None => {
                    problems.push(format!("portal.items.{}: no such mock (skipped)", item.id));
                    continue;
                }
            },
            Ok(_) => None,
            Err(e) => {
                problems.push(format!("portal.items.{}: {e} (skipped)", item.id));
                continue;
            }
        };
        publish.push((item, owner));
    }
    let named: BTreeSet<&str> = publish.iter().map(|(i, _)| i.id.as_str()).collect();
    let unpublish: Vec<String> = if opts.prune {
        published
            .iter()
            .filter(|(id, p)| {
                !p.visitor() && !named.contains(id.as_str()) && !file_defined.contains(*id)
            })
            .map(|(id, _)| id.clone())
            .collect()
    } else {
        Vec::new()
    };
    let ids: Vec<String> = publish.iter().map(|(i, _)| i.id.clone()).collect();
    if opts.dry_run {
        return Some(json!({ "publish": ids, "kept_by_file": kept, "unpublish": unpublish }));
    }
    let now = chrono::Utc::now();
    let mut created = Vec::new();
    let mut updated = Vec::new();
    for (item, owner) in publish {
        let meta = match crate::portal::clean_meta(&item.meta()) {
            Ok(m) => m,
            Err(e) => {
                problems.push(format!("portal.items.{}: {e} (skipped)", item.id));
                continue;
            }
        };
        match state
            .portal
            .publish(&item.id, meta, opts.principal.clone(), owner, now)
        {
            Ok((_, true)) => created.push(item.id),
            Ok((_, false)) => updated.push(item.id),
            Err(crate::portal::PublishError::Full) => problems.push(format!(
                "portal.items.{}: the portal holds at most {} items (skipped)",
                item.id,
                crate::portal::MAX_ITEMS
            )),
            Err(crate::portal::PublishError::Io(e)) => {
                problems.push(format!("portal.items.{}: {e}", item.id))
            }
        }
    }
    let mut removed = Vec::new();
    for id in unpublish {
        match state.portal.unpublish(&id) {
            Ok(true) => removed.push(id),
            Ok(false) => {}
            Err(e) => problems.push(format!("portal.items.{id}: {e}")),
        }
    }
    Some(
        json!({ "published": created, "updated": updated, "kept_by_file": kept, "unpublished": removed }),
    )
}

fn apply_harness(
    state: &AppState,
    section: Option<HarnessSection>,
    opts: &ApplyOptions,
    problems: &mut Vec<String>,
) -> Option<Value> {
    let section = section?;
    let Some(h) = &state.harness else {
        if !section.scenarios.is_empty() {
            problems.push(format!(
                "harness: harness mode is off (RUSTYBIN_HARNESS): {} scenario(s) not applied",
                section.scenarios.len()
            ));
        }
        return Some(json!({ "skipped": section.scenarios.len() }));
    };
    let caps = h.caps();
    let mut valid = Vec::new();
    for doc in section.scenarios {
        if !crate::harness::saved::valid_name(&doc.name) {
            problems.push(format!(
                "harness.scenarios.{}: the name: letters, digits, . _ - only, at most 64 characters (skipped)",
                doc.name
            ));
            continue;
        }
        let scenario = match crate::harness::scenario::Scenario::from_value(&doc.scenario) {
            Ok(s) => s,
            Err(e) => {
                problems.push(format!("harness.scenarios.{}: {e} (skipped)", doc.name));
                continue;
            }
        };
        let errors = scenario.validate(&caps);
        if !errors.is_empty() {
            problems.push(format!(
                "harness.scenarios.{}: {} (skipped)",
                doc.name,
                errors.join("; ")
            ));
            continue;
        }
        valid.push((doc.name, scenario));
    }
    let names: BTreeSet<String> = valid.iter().map(|(n, _)| n.clone()).collect();
    let delete: Vec<String> = if opts.prune {
        h.saved
            .list()
            .into_iter()
            .map(|s| s.name)
            .filter(|n| !names.contains(n))
            .collect()
    } else {
        Vec::new()
    };
    if opts.dry_run {
        return Some(json!({ "save": names, "delete": delete }));
    }
    let mut saved = Vec::new();
    for (name, scenario) in valid {
        match h.saved.put(&name, scenario) {
            Ok(_) => saved.push(name),
            Err(e) => problems.push(format!("harness.scenarios.{name}: {e}")),
        }
    }
    let deleted: Vec<String> = delete.into_iter().filter(|n| h.saved.delete(n)).collect();
    Some(json!({ "saved": saved, "deleted": deleted }))
}

fn apply_agents(
    state: &AppState,
    section: Option<agents::FileSection>,
    opts: &ApplyOptions,
    problems: &mut Vec<String>,
) -> Option<Value> {
    let section = section?;
    let (policy, section_problems) = agents::file_policy(Some(section));
    problems.extend(section_problems);
    let view = json!({
        "default_ceiling": policy.default_ceiling.map(|c| c.as_str()),
        "rules": policy.rules.len(),
    });
    if !opts.dry_run {
        state.mocks.set_file_agents(policy);
    }
    Some(view)
}

fn apply_health(
    state: &AppState,
    section: Option<HealthSection>,
    opts: &ApplyOptions,
    problems: &mut Vec<String>,
) -> Option<Value> {
    let section = section?;
    let healthy = match section.initial.as_str() {
        "healthy" => true,
        "unhealthy" => false,
        other => {
            problems.push(format!(
                "health.initial: {other:?} is not healthy or unhealthy (skipped)"
            ));
            return Some(json!({ "skipped": true }));
        }
    };
    if !opts.dry_run {
        state.health.set(healthy);
    }
    Some(json!({ "initial": section.initial }))
}

// ── The applied registry and agents sections ────────────────────────

#[derive(Default, Serialize, Deserialize)]
struct Applied {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    registry: Vec<Value>,
    #[serde(default)]
    agents: agents::FileSection,
}

/// Keep the registry's file copy and the agents policy in
/// `<state>/config.json` (nothing without a state directory).
pub fn save_applied(state: &AppState) -> Result<(), String> {
    let Some(path) = state.settings.state_file(APPLIED_FILE) else {
        return Ok(());
    };
    let doc = Applied {
        version: 1,
        registry: state.mocks.file_registry().0,
        agents: agents_section(&state.mocks.file_agents()),
    };
    let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
    crate::settings::write_private(&path, &bytes)
        .map_err(|e| format!("the applied configuration could not be saved: {e}"))
}

/// Load `<state>/config.json` at startup (before `RUSTYBIN_CONFIG_FILE`,
/// which wins for the sections it carries).
pub fn load_applied(state: &AppState) {
    let Some(path) = state.settings.state_file(APPLIED_FILE) else {
        return;
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!("config: {}: {e}; starting without it", path.display());
            return;
        }
    };
    if bytes.len() > config_file::MAX_FILE_BYTES as usize {
        tracing::warn!(
            "config: {} is too large; starting without it",
            path.display()
        );
        return;
    }
    let doc: Applied = match serde_json::from_slice(&bytes) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                "config: {} is not readable ({e}); starting without it",
                path.display()
            );
            return;
        }
    };
    let (registry, problems) = crate::mcp::registry::file_entries(Some(doc.registry));
    let (agents, agent_problems) = agents::file_policy(Some(doc.agents));
    for p in problems.into_iter().chain(agent_problems) {
        tracing::warn!("config: {}: {p}", path.display());
    }
    if !registry.is_empty() {
        state.mocks.set_file_registry(registry);
    }
    if agents != agents::FilePolicy::default() {
        state.mocks.set_file_agents(agents);
    }
    tracing::info!("applied configuration loaded from {}", path.display());
}

/// Parse a document sent to the API: YAML or JSON text, or a JSON object.
pub fn parse_document(document: &Value) -> Result<MocksFile, String> {
    match document {
        Value::String(text) => config_file::parse(text),
        Value::Object(_) => {
            let file: MocksFile = serde_json::from_value(document.clone())
                .map_err(|e| format!("invalid document: {e}"))?;
            if !config_file::VERSIONS.contains(&file.version) {
                return Err(format!(
                    "unsupported version {} (expected 1 or 2)",
                    file.version
                ));
            }
            Ok(file)
        }
        _ => Err("document must be YAML or JSON text, or a JSON object".into()),
    }
}
