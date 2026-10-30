//! `server.json` validation: the vendored schema plus the semantic checks
//! the registry makes on publish.
//!
//! The schema is `schema/server.schema.json`, vendored from
//! <https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json>
//! (the version the registry's `/v0.1` API, 1.8.6, serves in `$schema`).
//! Issues have the shape of the registry's `ValidationResult`:
//! `{type, path, message, severity, reference}`.

use std::sync::OnceLock;

use serde_json::{json, Value};

use crate::mocks::schema::{self, Direction};

/// The `$schema` of documents this build writes and expects.
pub const SCHEMA_URL: &str =
    "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json";

/// The schema text, served as it was vendored.
pub const SCHEMA_TEXT: &str = include_str!("schema/server.schema.json");

/// Largest `server.json` accepted (bytes of its JSON).
pub const MAX_DOC_BYTES: usize = 64 * 1024;

/// The parsed schema (`Value::Null` if the vendored file were broken; the
/// unit tests make sure it is not).
pub fn schema_doc() -> &'static Value {
    static DOC: OnceLock<Value> = OnceLock::new();
    DOC.get_or_init(|| serde_json::from_str(SCHEMA_TEXT).unwrap_or(Value::Null))
}

fn issue(
    kind: &str,
    path: &str,
    message: impl Into<String>,
    severity: &str,
    reference: &str,
) -> Value {
    json!({ "type": kind, "path": path, "message": message.into(), "severity": severity, "reference": reference })
}

/// `/remotes/0/url` as the registry writes it: `remotes[0].url`.
fn dotted(pointer: &str) -> String {
    let mut out = String::new();
    for part in pointer.split('/').filter(|p| !p.is_empty()) {
        let part = part.replace("~1", "/").replace("~0", "~");
        if part.chars().all(|c| c.is_ascii_digit()) {
            out.push_str(&format!("[{part}]"));
        } else {
            if !out.is_empty() {
                out.push('.');
            }
            out.push_str(&part);
        }
    }
    out
}

fn alnum_edges(s: &str) -> bool {
    let first = s.chars().next();
    let last = s.chars().last();
    first.is_some_and(|c| c.is_ascii_alphanumeric())
        && last.is_some_and(|c| c.is_ascii_alphanumeric())
}

/// A version range or tag the registry refuses (`^1.2`, `>=1`, `1.x`, `latest`).
fn is_range(version: &str) -> bool {
    let v = version.trim();
    v == "latest"
        || v.starts_with(['^', '~', '>', '<', '='])
        || v.contains("||")
        || v.contains(" - ")
        || v.split('.').any(|p| matches!(p, "x" | "X" | "*"))
}

fn semantic(doc: &Value, out: &mut Vec<Value>) {
    if let Some(name) = doc.get("name").and_then(Value::as_str) {
        if let Some((namespace, server)) = name.split_once('/') {
            if !alnum_edges(namespace) || !alnum_edges(server) || server.len() < 2 {
                out.push(issue("semantic", "name", format!("server name format is invalid: {name:?}. Both parts must start and end with a letter or digit, and the server part needs at least two characters"), "error", "invalid-server-name"));
            }
        }
    }
    if let Some(version) = doc.get("version").and_then(Value::as_str) {
        if is_range(version) {
            out.push(issue(
                "semantic",
                "version",
                format!(
                    "version {version:?} looks like a range or a tag: publish an exact version"
                ),
                "error",
                "version-looks-like-range",
            ));
        }
    }
    match doc.get("$schema").and_then(Value::as_str) {
        None => out.push(issue(
            "schema",
            "$schema",
            "$schema is missing: the registry requires it",
            "error",
            "schema-missing",
        )),
        Some(s) if s != SCHEMA_URL => out.push(issue(
            "semantic",
            "$schema",
            format!("$schema is {s:?}; this registry validates against {SCHEMA_URL}"),
            "warning",
            "schema-version",
        )),
        Some(_) => {}
    }
    let remotes = doc.get("remotes").and_then(Value::as_array);
    let packages = doc.get("packages").and_then(Value::as_array);
    if remotes.is_none_or(|r| r.is_empty()) && packages.is_none_or(|p| p.is_empty()) {
        out.push(issue(
            "semantic",
            "",
            "the server has neither remotes nor packages: clients have no way to run it",
            "warning",
            "no-remotes-or-packages",
        ));
    }
    for (i, r) in remotes.into_iter().flatten().enumerate() {
        let Some(url) = r.get("url").and_then(Value::as_str) else {
            continue;
        };
        let path = format!("remotes[{i}].url");
        match reqwest::Url::parse(url) {
            Err(e) => out.push(issue(
                "semantic",
                &path,
                format!("invalid remote URL {url:?}: {e}"),
                "error",
                "invalid-remote-url",
            )),
            Ok(u) => {
                let host = u.host_str().unwrap_or("");
                if matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1") {
                    // The public registry refuses these; a mock registry for
                    // local tests lists them (its own servers on localhost).
                    out.push(issue(
                        "semantic",
                        &path,
                        format!("{url} is a local address: the public registry would refuse it"),
                        "warning",
                        "local-remote-url",
                    ));
                }
            }
        }
    }
    for (i, p) in packages.into_iter().flatten().enumerate() {
        if p.get("version")
            .and_then(Value::as_str)
            .is_some_and(is_range)
        {
            out.push(issue(
                "semantic",
                &format!("packages[{i}].version"),
                "a package version must be exact",
                "error",
                "version-looks-like-range",
            ));
        }
    }
}

/// Validate one document: `{valid, issues}` (valid when no issue is an error).
pub fn validate(doc: &Value) -> Value {
    let mut issues = Vec::new();
    if !doc.is_object() {
        issues.push(issue(
            "schema",
            "",
            "a server.json document is a JSON object",
            "error",
            "not-an-object",
        ));
    } else if doc.to_string().len() > MAX_DOC_BYTES {
        issues.push(issue(
            "schema",
            "",
            format!("the document is larger than {} KiB", MAX_DOC_BYTES / 1024),
            "error",
            "too-large",
        ));
    } else {
        let root = schema_doc();
        let top = root
            .pointer("/definitions/ServerDetail")
            .unwrap_or(&Value::Null);
        for i in schema::validate(doc, top, root, Direction::Request) {
            issues.push(issue(
                "schema",
                &dotted(&i.pointer),
                i.message,
                "error",
                "server.schema.json",
            ));
        }
        semantic(doc, &mut issues);
    }
    let valid = !issues.iter().any(|i| i["severity"] == "error");
    json!({ "valid": valid, "issues": issues })
}

/// The error lines of a validation result (for repair messages and 400s).
pub fn errors(result: &Value) -> Vec<String> {
    result["issues"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|i| i["severity"] == "error")
                .map(|i| {
                    let path = i["path"].as_str().unwrap_or("");
                    let msg = i["message"].as_str().unwrap_or("");
                    if path.is_empty() {
                        msg.to_string()
                    } else {
                        format!("{path}: {msg}")
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> Value {
        json!({
            "$schema": SCHEMA_URL,
            "name": "io.example/weather",
            "description": "Weather forecasts",
            "version": "1.0.2",
            "remotes": [{ "type": "streamable-http", "url": "https://mcp.example.com/mcp" }]
        })
    }

    #[test]
    fn the_vendored_schema_parses() {
        assert_eq!(schema_doc()["$id"], SCHEMA_URL);
        assert!(schema_doc().pointer("/definitions/ServerDetail").is_some());
    }

    #[test]
    fn a_good_document_passes() {
        let r = validate(&good());
        assert_eq!(r["valid"], true, "{r}");
        assert!(r["issues"].as_array().unwrap().is_empty(), "{r}");
    }

    #[test]
    fn schema_violations_are_errors() {
        let mut d = good();
        d["name"] = json!("noslash");
        d["description"] = json!("x".repeat(101));
        d["remotes"][0]["type"] = json!("websocket");
        let r = validate(&d);
        assert_eq!(r["valid"], false);
        let paths: Vec<&str> = r["issues"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|i| i["path"].as_str())
            .collect();
        assert!(paths.contains(&"name"), "{r}");
        assert!(paths.contains(&"description"), "{r}");
        assert!(paths.iter().any(|p| p.starts_with("remotes[0]")), "{r}");
        let d = json!({ "$schema": SCHEMA_URL, "name": "io.example/x1" });
        let r = validate(&d);
        assert!(errors(&r).iter().any(|e| e.contains("description")), "{r}");
        assert!(errors(&r).iter().any(|e| e.contains("version")), "{r}");
    }

    #[test]
    fn semantic_checks() {
        let mut d = good();
        d["version"] = json!("^1.0");
        d["name"] = json!("io.example/-x");
        let r = validate(&d);
        let refs: Vec<&str> = r["issues"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|i| i["reference"].as_str())
            .collect();
        assert!(refs.contains(&"version-looks-like-range"), "{r}");
        assert!(refs.contains(&"invalid-server-name"), "{r}");
        let mut d = good();
        d["remotes"][0]["url"] = json!("http://localhost:8080/mcp");
        d.as_object_mut().unwrap().remove("$schema");
        let r = validate(&d);
        assert_eq!(r["valid"], false, "a missing $schema is an error: {r}");
        let mut d = good();
        d["remotes"][0]["url"] = json!("http://localhost:8080/mcp");
        let r = validate(&d);
        assert_eq!(r["valid"], true, "a local remote is a warning: {r}");
        assert_eq!(r["issues"][0]["severity"], "warning");
        assert_eq!(validate(&json!([1]))["valid"], false);
        let mut d = good();
        d["packages"] = json!([{ "registryType": "npm", "identifier": "@x/y", "version": "latest", "transport": { "type": "stdio" } }]);
        assert_eq!(validate(&d)["valid"], false);
    }
}
