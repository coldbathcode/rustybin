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

use super::model::{MockSettings, OperationSettings, ResponseSettings, SecuritySettings};
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
}

impl MockDoc {
    pub fn settings(&self) -> MockSettings {
        MockSettings {
            strip_server_prefix: self.strip_server_prefix,
            responses: self.responses.clone(),
            security: self.security.clone(),
            operations: self.operations.clone(),
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

/// Load a config file from disk.
pub fn load_file<F>(
    path: &Path,
    lookup: F,
    max_spec: u64,
) -> Result<(Vec<Definition>, Vec<String>), String>
where
    F: Fn(&str) -> Option<String>,
{
    let raw = read_limited(path, MAX_FILE_BYTES)?;
    let (text, missing) = expand_env(&raw, lookup);
    let mut problems: Vec<String> = missing
        .into_iter()
        .map(|v| format!("{}: environment variable {v} is not set", path.display()))
        .collect();
    let file = parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let base = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let (defs, errors) = definitions(file, Some(&base), max_spec);
    problems.extend(errors.into_iter().map(|e| {
        format!(
            "{}: mock {}: {}",
            path.display(),
            e["name"].as_str().unwrap_or("?"),
            e["error"].as_str().unwrap_or("?")
        )
    }));
    Ok((defs, problems))
}

/// Every `*.yaml`, `*.yml` and `*.json` spec in a directory becomes a mock
/// with default settings, named after the file.
pub fn load_dir(dir: &Path, max_spec: u64) -> (Vec<Definition>, Vec<String>) {
    let mut defs = Vec::new();
    let mut problems = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => return (defs, vec![format!("{}: {e}", dir.display())]),
    };
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
            Ok(source) => defs.push(Definition {
                name,
                source,
                settings: MockSettings::default(),
                enabled: true,
            }),
            Err(e) => problems.push(e),
        }
    }
    (defs, problems)
}

/// A mock as a `rustybin.yaml` document (spec inlined).
pub fn export(snapshots: &[Snapshot]) -> String {
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
            doc.insert(
                "security".into(),
                serde_json::to_value(&settings.security).unwrap_or(Value::Null),
            );
            if !settings.operations.is_empty() {
                doc.insert(
                    "operations".into(),
                    serde_json::to_value(&settings.operations).unwrap_or(Value::Null),
                );
            }
            doc.insert("spec_inline".into(), c.spec.root.clone());
            Value::Object(doc)
        })
        .collect();
    let file = json!({ "version": 1, "mocks": mocks });
    let body = serde_yaml::to_string(&file).unwrap_or_default();
    format!("# Rustybin mocks (rustybin.yaml). Apply with: rustybin mock push <file>\n{body}")
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
        }
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
        let (defs, problems) =
            load_file(&cfg, |k| (k == "K").then(|| "k1".to_string()), 1 << 20).expect("load");
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].settings.security.api_key.keys, vec!["k1"]);
        assert!(defs[0].source.contains("openapi"));
        assert!(!defs[1].enabled);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("spec_inline is required"));

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

        let (defs, problems) = load_dir(dir.path(), 1 << 20);
        assert_eq!(
            defs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["rustybin", "spec"]
        );
        assert!(problems.is_empty());
    }
}
