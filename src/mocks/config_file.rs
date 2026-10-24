//! `rustybin.yaml`: mocks as code.
//!
//! ```yaml
//! version: 1
//! mocks:
//!   - name: petstore
//!     spec: ./specs/petstore.yaml      # relative to this file (or spec_inline)
//!     enabled: true
//!     responses: { mode: examples }
//!     security: { enabled: true, mode: spec }
//!     operations:
//!       getPetById:
//!         rules:
//!           - when: { path.petId: { equals: 0 } }
//!             respond: { status: 404 }
//! ```
//!
//! `${VAR}` and `${VAR:-default}` are expanded from the environment when a
//! file is loaded from disk (never for documents sent to the API).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::model::{
    McpSettings, MockSettings, OperationSettings, ResponseSettings, SecuritySettings,
};
use super::spec::{parse_document, slugify};
use super::store::{Definition, Snapshot};

/// Largest config file read from disk.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

fn default_true() -> bool {
    true
}

fn default_version() -> u32 {
    1
}

/// The whole file.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MocksFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub mocks: Vec<MockDoc>,
}

/// One mock in the file.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MockDoc {
    pub name: String,
    /// Path of the spec file, relative to the config file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    /// The spec itself (YAML/JSON object, or text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_inline: Option<Value>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strip_server_prefix: bool,
    #[serde(default)]
    pub responses: ResponseSettings,
    #[serde(default)]
    pub security: SecuritySettings,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub operations: BTreeMap<String, OperationSettings>,
    #[serde(default, skip_serializing_if = "McpSettings::is_default")]
    pub mcp: McpSettings,
}

impl MockDoc {
    pub fn settings(&self) -> MockSettings {
        MockSettings {
            strip_server_prefix: self.strip_server_prefix,
            responses: self.responses.clone(),
            security: self.security.clone(),
            operations: self.operations.clone(),
            mcp: self.mcp.clone(),
        }
    }
}

/// Parse a config document (YAML or JSON).
pub fn parse(text: &str) -> Result<MocksFile, String> {
    let value = parse_document(text)?;
    let file: MocksFile =
        serde_json::from_value(value).map_err(|e| format!("invalid mocks document: {e}"))?;
    if file.version != 1 {
        return Err(format!("unsupported version {} (expected 1)", file.version));
    }
    Ok(file)
}

/// A mock entry of a file that could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failed {
    /// The entry's name, when it has a usable one.
    pub name: Option<String>,
    pub error: String,
}

/// [`parse`] for files on disk: a bad entry is skipped (and returned) instead
/// of failing the whole document. Errors that leave nothing to read (not
/// YAML or JSON, wrong version, `mocks` not a list) still fail.
pub fn parse_lenient(text: &str) -> Result<(MocksFile, Vec<Failed>), String> {
    let value = parse_document(text)?;
    let Value::Object(mut top) = value else {
        return Err("invalid mocks document: expected a mapping with `version` and `mocks`".into());
    };
    let version = match top.remove("version") {
        None => default_version(),
        Some(v) => v
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| format!("invalid version {v} (expected 1)"))?,
    };
    if version != 1 {
        return Err(format!("unsupported version {version} (expected 1)"));
    }
    let entries = match top.remove("mocks") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a,
        Some(_) => return Err("invalid mocks document: `mocks` must be a list".into()),
    };
    let mut failed: Vec<Failed> = top
        .keys()
        .map(|k| Failed {
            name: None,
            error: format!("unknown field {k:?} ignored (expected `version` and `mocks`)"),
        })
        .collect();
    let mut mocks = Vec::new();
    for (i, entry) in entries.into_iter().enumerate() {
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string);
        match serde_json::from_value::<MockDoc>(entry) {
            Ok(doc) => mocks.push(doc),
            Err(e) => failed.push(Failed {
                error: match &name {
                    Some(n) => format!("mock {n}: {e}"),
                    None => format!("mocks[{i}]: {e}"),
                },
                name,
            }),
        }
    }
    Ok((MocksFile { version, mocks }, failed))
}

/// Mocks read from disk.
#[derive(Default)]
pub struct Loaded {
    pub defs: Vec<Definition>,
    /// Everything worth a warning (unset variables, skipped entries, ...).
    pub problems: Vec<String>,
    /// Named entries that could not be read: a reload keeps their mocks
    /// instead of deleting them.
    pub failed: Vec<(String, String)>,
}

/// Expand `${VAR}` and `${VAR:-default}`; unknown variables without a
/// default become empty and are reported.
pub fn expand_env<F>(text: &str, lookup: F) -> (String, Vec<String>)
where
    F: Fn(&str) -> Option<String>,
{
    let mut out = String::with_capacity(text.len());
    let mut missing = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let expr = &after[..end];
        let (name, default) = match expr.split_once(":-") {
            Some((n, d)) => (n, Some(d)),
            None => (expr, None),
        };
        let valid = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            out.push_str(&rest[start..start + 2 + end + 1]);
        } else {
            match (lookup(name), default) {
                (Some(v), _) => out.push_str(&v),
                (None, Some(d)) => out.push_str(d),
                (None, None) => missing.push(name.to_string()),
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    (out, missing)
}

fn read_limited(path: &Path, max: u64) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > max {
        return Err(format!("{}: larger than {max} bytes", path.display()));
    }
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The spec text of a document entry.
fn spec_text(doc: &MockDoc, base: Option<&Path>, max_spec: u64) -> Result<String, String> {
    match (&doc.spec, &doc.spec_inline) {
        (Some(_), Some(_)) => Err("set either spec or spec_inline, not both".into()),
        (None, None) => Err("spec (a file path) or spec_inline is required".into()),
        (None, Some(Value::String(s))) => Ok(s.clone()),
        (None, Some(v)) => serde_json::to_string_pretty(v).map_err(|e| e.to_string()),
        (Some(path), None) => {
            let Some(base) = base else {
                return Err(
                    "spec file paths are only read from local files: send spec_inline \
                     (rustybin mock push inlines them)"
                        .into(),
                );
            };
            let p = Path::new(path);
            let full = if p.is_absolute() {
                p.to_path_buf()
            } else {
                base.join(p)
            };
            read_limited(&full, max_spec)
        }
    }
}

/// Turn documents into definitions; entries that fail are returned as errors.
pub fn definitions(
    file: MocksFile,
    base: Option<&Path>,
    max_spec: u64,
) -> (Vec<Definition>, Vec<Value>) {
    let mut defs = Vec::new();
    let mut errors = Vec::new();
    for doc in file.mocks {
        match spec_text(&doc, base, max_spec) {
            Ok(source) => defs.push(Definition {
                settings: doc.settings(),
                name: doc.name,
                source,
                enabled: doc.enabled,
            }),
            Err(e) => errors.push(json!({ "name": doc.name, "error": e })),
        }
    }
    (defs, errors)
}

/// Load a config file from disk. Bad entries are skipped (see
/// [`Loaded::failed`]); `Err` means nothing could be read.
pub fn load_file<F>(path: &Path, lookup: F, max_spec: u64) -> Result<Loaded, String>
where
    F: Fn(&str) -> Option<String>,
{
    let raw = read_limited(path, MAX_FILE_BYTES)?;
    let (text, missing) = expand_env(&raw, lookup);
    let mut out = Loaded {
        problems: missing
            .into_iter()
            .map(|v| format!("{}: environment variable {v} is not set", path.display()))
            .collect(),
        ..Loaded::default()
    };
    let (file, skipped) = parse_lenient(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    for f in skipped {
        out.problems
            .push(format!("{}: {}", path.display(), f.error));
        if let Some(name) = f.name {
            out.failed.push((name, f.error));
        }
    }
    let base = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let (defs, errors) = definitions(file, Some(&base), max_spec);
    out.defs = defs;
    for e in errors {
        let name = e["name"].as_str().unwrap_or("?").to_string();
        let error = e["error"].as_str().unwrap_or("?").to_string();
        out.problems
            .push(format!("{}: mock {name}: {error}", path.display()));
        out.failed.push((name, error));
    }
    Ok(out)
}

/// Every `*.yaml`, `*.yml` and `*.json` spec in a directory becomes a mock
/// with default settings, named after the file. `Err`: the directory cannot
/// be read.
pub fn load_dir(dir: &Path, max_spec: u64) -> Result<Loaded, String> {
    let mut out = Loaded::default();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| matches!(e, "yaml" | "yml" | "json"))
        })
        .collect();
    paths.sort();
    for path in paths.into_iter().take(200) {
        let name = slugify(
            &path
                .file_stem()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default(),
        );
        match read_limited(&path, max_spec) {
            Ok(source) => out.defs.push(Definition {
                name,
                source,
                settings: MockSettings::default(),
                enabled: true,
            }),
            Err(e) => {
                out.problems.push(e.clone());
                out.failed.push((name, e));
            }
        }
    }
    Ok(out)
}

/// A mock as a `rustybin.yaml` document (spec inlined). Without `reveal`
/// the security secrets are redacted.
pub fn export(snapshots: &[Snapshot], reveal: bool) -> String {
    let mocks: Vec<Value> = snapshots
        .iter()
        .map(|s| {
            let c = &s.draft;
            let settings = &c.settings;
            let mut doc = serde_json::Map::new();
            doc.insert("name".into(), json!(s.name));
            doc.insert("enabled".into(), json!(s.enabled));
            if settings.strip_server_prefix {
                doc.insert("strip_server_prefix".into(), json!(true));
            }
            doc.insert(
                "responses".into(),
                serde_json::to_value(&settings.responses).unwrap_or(Value::Null),
            );
            let security = if reveal {
                serde_json::to_value(&settings.security)
            } else {
                serde_json::to_value(settings.security.redacted())
            };
            doc.insert("security".into(), security.unwrap_or(Value::Null));
            if !settings.operations.is_empty() {
                doc.insert(
                    "operations".into(),
                    serde_json::to_value(&settings.operations).unwrap_or(Value::Null),
                );
            }
            if !settings.mcp.is_default() {
                doc.insert(
                    "mcp".into(),
                    serde_json::to_value(&settings.mcp).unwrap_or(Value::Null),
                );
            }
            doc.insert("spec_inline".into(), c.spec.root.clone());
            Value::Object(doc)
        })
        .collect();
    let file = json!({ "version": 1, "mocks": mocks });
    let body = serde_yaml::to_string(&file).unwrap_or_default();
    let note = if reveal {
        ""
    } else {
        "# Secrets are shown as \"<redacted>\": export with admin rights to include them.\n"
    };
    format!("# Rustybin mocks (rustybin.yaml). Apply with: rustybin mock push <file>\n{note}{body}")
}

/// JSON Schema of the file (editor completion and validation).
pub fn json_schema() -> Value {
    let object = |properties: Value| json!({ "type": "object", "additionalProperties": false, "properties": properties });
    let strings = json!({ "type": "array", "items": { "type": "string" } });
    let condition = object(json!({
        "equals": {}, "not_equals": {}, "exists": { "type": "boolean" },
        "matches": { "type": "string", "maxLength": 256 }
    }));
    let respond = object(json!({
        "status": { "type": "integer", "minimum": 100, "maximum": 599 },
        "example": { "type": "string", "description": "Name of a spec example" },
        "body": { "description": "Literal body: JSON values are serialised, strings sent as is" },
        "headers": { "type": "object", "additionalProperties": { "type": "string" } }
    }));
    let mode = json!({ "type": "string", "enum": ["examples", "generated", "static", "stateful"] });
    let responses = object(json!({
        "mode": mode,
        "seed": { "type": "integer", "minimum": 0 },
        "delay_ms": { "type": "integer", "minimum": 0 },
        "validate_requests": { "type": "string", "enum": ["off", "warn", "enforce"] },
        "disabled_status": { "type": "integer", "minimum": 400, "maximum": 599 }
    }));
    let security = object(json!({
        "enabled": { "type": "boolean" },
        "mode": { "type": "string", "enum": ["spec", "api_key", "basic", "bearer", "jwt"] },
        "api_key": object(json!({ "header": { "type": "string" }, "keys": strings })),
        "basic": object(json!({ "users": { "type": "object", "additionalProperties": { "type": "string" } } })),
        "bearer": object(json!({ "tokens": strings })),
        "jwt": object(json!({ "issuer": { "type": "string" }, "audience": { "type": "string" } }))
    }));
    let mut rule = object(json!({
        "name": { "type": "string" },
        "when": {
            "type": "object",
            "propertyNames": { "pattern": "^(body|(path|query|header|body)\\..+)$" },
            "additionalProperties": condition
        },
        "respond": respond
    }));
    if let Value::Object(m) = &mut rule {
        m.insert("required".into(), json!(["when"]));
    }
    let operation = object(json!({
        "respond": respond,
        "rules": { "type": "array", "items": rule },
        "mode": mode,
        "delay_ms": { "type": "integer", "minimum": 0 },
        "public": { "type": "boolean" }
    }));
    let mut mock = object(json!({
        "name": { "type": "string", "pattern": "^[a-z0-9]([a-z0-9-]{0,38}[a-z0-9])?$" },
        "spec": { "type": "string", "description": "Spec file path, relative to this file" },
        "spec_inline": { "description": "The OpenAPI document (object or text)" },
        "enabled": { "type": "boolean", "default": true },
        "strip_server_prefix": { "type": "boolean", "default": false },
        "responses": responses,
        "security": security,
        "operations": {
            "type": "object",
            "description": "Keyed by operationId",
            "additionalProperties": operation
        },
        "mcp": object(json!({
            "enabled": { "type": "boolean", "default": true, "description": "Serve the mock's MCP server at /mock/{name}/_rustybin/mcp" },
            "exclude": { "type": "array", "items": { "type": "string" }, "description": "operationIds that are not tools" }
        }))
    }));
    if let Value::Object(m) = &mut mock {
        m.insert("required".into(), json!(["name"]));
    }
    let mut root = object(json!({
        "version": { "const": 1 },
        "mocks": { "type": "array", "items": mock }
    }));
    if let Value::Object(m) = &mut root {
        m.insert(
            "$schema".into(),
            json!("https://json-schema.org/draft/2020-12/schema"),
        );
        m.insert("title".into(), json!("Rustybin mocks file (rustybin.yaml)"));
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_expansion() {
        let lookup = |k: &str| (k == "KEY").then(|| "secret".to_string());
        let (out, missing) = expand_env(
            "a ${KEY} b ${NOPE:-dflt} c ${GONE} d ${bad-name} e ${",
            lookup,
        );
        assert_eq!(out, "a secret b dflt c  d ${bad-name} e ${");
        assert_eq!(missing, vec!["GONE"]);
    }

    #[test]
    fn parse_and_resolve_specs() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("spec.yaml"),
            "openapi: 3.0.0\ninfo: {title: x}\npaths: {}\n",
        )
        .expect("write");
        let cfg = dir.path().join("rustybin.yaml");
        std::fs::write(
            &cfg,
            "version: 1\nmocks:\n  - name: a\n    spec: spec.yaml\n    security: { enabled: true, api_key: { keys: ['${K}'] } }\n  - name: b\n    spec_inline: { openapi: 3.0.0, info: { title: y }, paths: {} }\n    enabled: false\n  - name: c\n",
        )
        .expect("write");
        let loaded =
            load_file(&cfg, |k| (k == "K").then(|| "k1".to_string()), 1 << 20).expect("load");
        let (defs, problems) = (&loaded.defs, &loaded.problems);
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].settings.security.api_key.keys, vec!["k1"]);
        assert!(defs[0].source.contains("openapi"));
        assert!(!defs[1].enabled);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("spec_inline is required"));
        assert_eq!(loaded.failed.len(), 1);
        assert_eq!(loaded.failed[0].0, "c");

        // API documents cannot reference files.
        let file = parse("mocks: [{name: a, spec: /etc/passwd}]").expect("parse");
        let (defs, errors) = definitions(file, None, 1 << 20);
        assert!(defs.is_empty());
        assert!(errors[0]["error"]
            .as_str()
            .expect("e")
            .contains("only read from local files"));

        assert!(parse("version: 2").is_err());
        assert!(parse("mocks: [{name: a, nope: 1}]").is_err());

        let loaded = load_dir(dir.path(), 1 << 20).expect("dir");
        assert_eq!(
            loaded
                .defs
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            vec!["rustybin", "spec"]
        );
        assert!(loaded.problems.is_empty());
        assert!(load_dir(&dir.path().join("missing"), 1 << 20).is_err());
    }

    #[test]
    fn bad_entries_are_skipped_one_by_one() {
        let (file, failed) = parse_lenient(
            "version: 1\nextra: true\nmocks:\n  - name: good\n    spec_inline: {}\n  - name: typo\n    enabeld: false\n  - spec: x.yaml\n    nope: 1\n",
        )
        .expect("parse");
        assert_eq!(file.mocks.len(), 1);
        assert_eq!(file.mocks[0].name, "good");
        assert_eq!(failed.len(), 3, "{failed:?}");
        assert!(failed[0].error.contains("extra"));
        assert_eq!(failed[1].name.as_deref(), Some("typo"));
        assert!(failed[1].error.contains("enabeld"), "{failed:?}");
        assert_eq!(failed[2].name, None);
        assert!(failed[2].error.starts_with("mocks[2]"), "{failed:?}");

        assert!(parse_lenient("version: 2").is_err());
        assert!(parse_lenient("mocks: {a: 1}").is_err());
        assert!(parse_lenient("- just\n- a list").is_err());
        assert!(parse_lenient("version: 1")
            .expect("empty")
            .0
            .mocks
            .is_empty());
    }
}
