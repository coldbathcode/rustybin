//! Recorded traffic as a scenario's source: a HAR file, or the captures of
//! `GET /_rustybin/requests`, turned into weighted request templates.
//!
//! An import, not a new kind of source: what comes out is `source.requests`,
//! so a replay meets the same validation, allowlist and pinning as anything
//! else a scenario sends, and the engine has nothing new to get wrong. Hosts
//! are dropped too, since a scenario names its own target.
//!
//! A recording holds credentials, and a scenario is a document people share
//! and commit. So a secret-looking header or query parameter is removed, and
//! an entry whose body has a secret-looking field is skipped whole; what was
//! taken out is listed by name, never by value.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{json, Value};

use super::scenario::RequestTemplate;
use crate::keys::looks_secret;

/// Entries read from a recording.
pub const MAX_ENTRIES: usize = 5_000;
/// Different requests kept: the most frequent, when there are more.
pub const MAX_TEMPLATES: usize = 200;
/// The largest body kept.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Never replayed: set by the client, or wrong for another target.
const DROPPED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "transfer-encoding",
    "upgrade",
    "keep-alive",
    "te",
    "trailer",
    "accept-encoding",
    "x-request-id",
    "x-rustybin-session",
];

/// What an import made, and what it left out.
#[derive(Debug, Default, Serialize)]
pub struct Imported {
    /// `source.requests` of a scenario.
    pub requests: Vec<RequestTemplate>,
    /// Entries read.
    pub entries: usize,
    /// Entries left out, with the reason and how many.
    pub skipped: BTreeMap<String, usize>,
    /// Names of the secret-looking headers and query parameters removed.
    pub removed: Vec<String>,
    /// Hosts of the recording: a scenario names its own target.
    pub hosts: Vec<String>,
}

/// One recorded request, whichever format it came from.
struct Entry {
    method: String,
    path: String,
    query: Option<String>,
    host: Option<String>,
    headers: Vec<(String, String)>,
    body: Option<String>,
    binary: bool,
    status: Option<u16>,
}

fn pairs(v: &Value) -> Vec<(String, String)> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|h| {
                    Some((
                        h["name"].as_str()?.to_string(),
                        h["value"].as_str().unwrap_or("").to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn from_har_entry(e: &Value) -> Option<Entry> {
    let req = &e["request"];
    let url = reqwest::Url::parse(req["url"].as_str()?).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let post = &req["postData"];
    Some(Entry {
        method: req["method"].as_str().unwrap_or("GET").to_ascii_uppercase(),
        path: url.path().to_string(),
        query: url.query().map(str::to_string),
        host: url.host_str().map(str::to_string),
        headers: pairs(&req["headers"]),
        body: post["text"].as_str().map(str::to_string),
        binary: post["encoding"].as_str() == Some("base64"),
        status: e["response"]["status"].as_u64().map(|s| s as u16),
    })
}

fn from_capture(c: &Value) -> Option<Entry> {
    Some(Entry {
        method: c["method"].as_str()?.to_ascii_uppercase(),
        path: c["path"].as_str()?.to_string(),
        query: c["query"].as_str().map(str::to_string),
        host: None,
        headers: pairs(&c["headers"]),
        body: c["body"].as_str().map(str::to_string),
        binary: c["body_base64"].is_string() || c["body_truncated"] == json!(true),
        status: c["status"].as_u64().map(|s| s as u16),
    })
}

/// Does a JSON value hold a field whose name looks secret, at any depth?
fn holds_secret(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.iter().any(|(k, v)| looks_secret(k) || holds_secret(v)),
        Value::Array(a) => a.iter().any(holds_secret),
        _ => false,
    }
}

/// The body as a template holds it, or why the entry is left out.
fn body_of(entry: &Entry) -> Result<Option<Value>, &'static str> {
    let Some(text) = entry.body.as_deref().filter(|b| !b.is_empty()) else {
        return Ok(None);
    };
    if entry.binary {
        return Err("a binary or cut body");
    }
    if text.len() > MAX_BODY_BYTES {
        return Err("a body over the limit");
    }
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        if holds_secret(&v) {
            return Err("a body with a secret-looking field");
        }
        return Ok(Some(v));
    }
    if form_urlencoded::parse(text.as_bytes()).any(|(k, _)| looks_secret(&k)) {
        return Err("a body with a secret-looking field");
    }
    Ok(Some(Value::String(text.to_string())))
}

/// `path?query`, without secret-looking parameters.
fn path_of(entry: &Entry, removed: &mut Vec<String>) -> String {
    let Some(q) = entry.query.as_deref().filter(|q| !q.is_empty()) else {
        return entry.path.clone();
    };
    let kept: Vec<&str> = q
        .split('&')
        .filter(|pair| {
            let name = pair.split('=').next().unwrap_or("");
            let decoded = form_urlencoded::parse(format!("{name}=").as_bytes())
                .next()
                .map(|(k, _)| k.into_owned())
                .unwrap_or_else(|| name.to_string());
            let secret = looks_secret(&decoded);
            if secret && !removed.contains(&decoded) {
                removed.push(decoded);
            }
            !secret
        })
        .collect();
    if kept.is_empty() {
        entry.path.clone()
    } else {
        format!("{}?{}", entry.path, kept.join("&"))
    }
}

fn headers_of(entry: &Entry, removed: &mut Vec<String>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, value) in &entry.headers {
        let lower = name.to_ascii_lowercase();
        if lower.starts_with(':') || DROPPED_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        if looks_secret(&lower) {
            if !removed.contains(&lower) {
                removed.push(lower);
            }
            continue;
        }
        out.insert(lower, value.clone());
    }
    out
}

/// Turn a recording into request templates. Identical requests become one
/// template with their count as its weight, so the mix is the recording's.
pub fn import(doc: &Value) -> Result<Imported, String> {
    let entries: Vec<Option<Entry>> = if let Some(list) = doc["log"]["entries"].as_array() {
        list.iter().take(MAX_ENTRIES).map(from_har_entry).collect()
    } else if let Some(list) = doc["requests"].as_array().or(doc.as_array()) {
        list.iter().take(MAX_ENTRIES).map(from_capture).collect()
    } else {
        return Err(
            "not a recording: expected a HAR file (log.entries) or the answer of GET /_rustybin/requests"
                .into(),
        );
    };
    let mut out = Imported {
        entries: entries.len(),
        ..Imported::default()
    };
    let mut seen: BTreeMap<String, RequestTemplate> = BTreeMap::new();
    for entry in entries {
        let Some(entry) = entry else {
            *out.skipped.entry("not an http request".into()).or_default() += 1;
            continue;
        };
        let body = match body_of(&entry) {
            Ok(b) => b,
            Err(why) => {
                *out.skipped.entry(why.into()).or_default() += 1;
                continue;
            }
        };
        if let Some(h) = &entry.host {
            if !out.hosts.contains(h) {
                out.hosts.push(h.clone());
            }
        }
        let template = RequestTemplate {
            path: path_of(&entry, &mut out.removed),
            headers: headers_of(&entry, &mut out.removed),
            method: entry.method.clone(),
            body,
            // A recorded failure is what the replay should expect too.
            expect_status: entry
                .status
                .filter(|s| *s >= 400)
                .map(|s| vec![s])
                .unwrap_or_default(),
            weight: 1,
        };
        let key = serde_json::to_string(&json!([
            template.method,
            template.path,
            template.headers,
            template.body,
            template.expect_status
        ]))
        .map_err(|e| e.to_string())?;
        seen.entry(key)
            .and_modify(|t| t.weight = t.weight.saturating_add(1))
            .or_insert(template);
    }
    let mut requests: Vec<RequestTemplate> = seen.into_values().collect();
    requests.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.path.cmp(&b.path)));
    if requests.len() > MAX_TEMPLATES {
        let dropped: usize = requests[MAX_TEMPLATES..]
            .iter()
            .map(|t| t.weight as usize)
            .sum();
        requests.truncate(MAX_TEMPLATES);
        out.skipped
            .insert("rarer than the 200 kept".into(), dropped);
    }
    if requests.is_empty() {
        return Err("nothing to replay: every entry was left out".into());
    }
    out.requests = requests;
    Ok(out)
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod tests;
