//! What others recorded or wrote as a scenario's source: a HAR file, the
//! captures of `GET /_rustybin/requests`, a Postman collection (v2.1) or an
//! Artillery script, turned into weighted request templates.
//!
//! An import, not a new kind of source: what comes out is `source.requests`,
//! so it meets the same validation, allowlist and pinning as anything else a
//! scenario sends, and the engine has nothing new to get wrong. Hosts are
//! dropped too, since a scenario names its own target.
//!
//! A recording or a collection holds credentials, and a scenario is a
//! document people share and commit. So a secret-looking header or query
//! parameter is removed, and an entry whose body has a secret-looking field
//! is skipped whole; what was taken out is listed by name, never by value.
//!
//! The other tools' formats say more than a request list. What has an exact
//! counterpart is mapped (an Artillery script's phases, when its flows are
//! plain); what has none (scripts, hooks, plugins, captures, think times) is
//! never run and never guessed: it is named in `left_out`. A variable the
//! document does not define leaves its request out and is named in
//! `variables`. A program in a scripting language is refused.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Map, Value};

use super::pattern::{Curve, Pattern, MAX_STAGES, MAX_VALUE};
use super::scenario::{Dur, RequestTemplate, MAX_HEADERS, MAX_TIMEOUT, MIN_TIMEOUT};
use crate::keys::looks_secret;

/// Entries read from a recording, a collection or a script.
pub const MAX_ENTRIES: usize = 5_000;
/// Different requests kept (a layer's limit): the most frequent, when there
/// are more.
pub const MAX_TEMPLATES: usize = super::scenario::MAX_REQUESTS_PER_LAYER;
/// The largest body kept.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// The largest collection or script (a recording keeps the instance's body
/// limit: HAR files are large).
pub const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
/// Folders of a collection walked, deep.
pub const MAX_FOLDER_DEPTH: usize = 16;
/// Names kept in `left_out` and in `variables`.
pub const MAX_NAMES: usize = 100;
/// The largest weight a template takes.
const MAX_WEIGHT: u32 = 1000;

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

/// The methods a request template sends.
const METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// The formats read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// A HAR file (`log.entries`).
    #[default]
    Har,
    /// The answer of `GET /_rustybin/requests`.
    Captures,
    /// A Postman collection, v2.1.
    Postman,
    /// An Artillery script.
    Artillery,
}

impl Format {
    pub const ALL: &'static [&'static str] = &["har", "captures", "postman", "artillery"];

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "har" => Some(Format::Har),
            "captures" => Some(Format::Captures),
            "postman" => Some(Format::Postman),
            "artillery" => Some(Format::Artillery),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Har => "har",
            Format::Captures => "captures",
            Format::Postman => "postman",
            Format::Artillery => "artillery",
        }
    }

    /// A recording replays; a collection or a script is imported.
    fn recording(self) -> bool {
        matches!(self, Format::Har | Format::Captures)
    }
}

/// What an import made, and what it left out.
#[derive(Debug, Default, Serialize)]
pub struct Imported {
    /// The format the document was read as.
    pub format: Format,
    /// `source.requests` of a scenario.
    pub requests: Vec<RequestTemplate>,
    /// Entries read.
    pub entries: usize,
    /// Entries left out, with the reason and how many.
    pub skipped: BTreeMap<String, usize>,
    /// Names of the secret-looking headers and query parameters removed.
    pub removed: Vec<String>,
    /// Hosts of the document: a scenario names its own target.
    pub hosts: Vec<String>,
    /// What the document says that has no counterpart here, by its place in
    /// the document. Never run, never guessed.
    pub left_out: Vec<String>,
    /// Variables the document uses and does not define: their requests are
    /// in `skipped`.
    pub variables: Vec<String>,
    /// An Artillery script's phases as a pattern, when they map exactly.
    pub pattern: Option<Pattern>,
    /// An Artillery script's `config.http.timeout`.
    pub timeout: Option<Dur>,
    /// What to know about a mapped value.
    pub notes: Vec<String>,
}

/// One request, whichever format it came from.
#[derive(Default)]
struct Entry {
    method: String,
    path: String,
    query: Option<String>,
    host: Option<String>,
    headers: Vec<(String, String)>,
    body: Option<String>,
    binary: bool,
    status: Option<u16>,
    /// Statuses the document says to expect.
    expect: Vec<u16>,
    /// The share of the entry (an Artillery scenario's weight).
    weight: u32,
}

/// An entry, or why it is left out.
type Read = Result<Entry, &'static str>;

/// Add a name once, up to [`MAX_NAMES`].
fn name_once(list: &mut Vec<String>, name: impl Into<String>) {
    let name = name.into();
    if list.len() < MAX_NAMES && !list.contains(&name) {
        list.push(name);
    }
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

fn from_har_entry(e: &Value) -> Read {
    let req = &e["request"];
    let url = req["url"]
        .as_str()
        .and_then(|u| reqwest::Url::parse(u).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .ok_or("not an http request")?;
    let post = &req["postData"];
    Ok(Entry {
        method: req["method"].as_str().unwrap_or("GET").to_ascii_uppercase(),
        path: url.path().to_string(),
        query: url.query().map(str::to_string),
        host: url.host_str().map(str::to_string),
        headers: pairs(&req["headers"]),
        body: post["text"].as_str().map(str::to_string),
        binary: post["encoding"].as_str() == Some("base64"),
        status: e["response"]["status"].as_u64().map(|s| s as u16),
        weight: 1,
        ..Entry::default()
    })
}

fn from_capture(c: &Value) -> Read {
    let (Some(method), Some(path)) = (c["method"].as_str(), c["path"].as_str()) else {
        return Err("not an http request");
    };
    Ok(Entry {
        method: method.to_ascii_uppercase(),
        path: path.to_string(),
        query: c["query"].as_str().map(str::to_string),
        headers: pairs(&c["headers"]),
        body: c["body"].as_str().map(str::to_string),
        binary: c["body_base64"].is_string() || c["body_truncated"] == json!(true),
        status: c["status"].as_u64().map(|s| s as u16),
        weight: 1,
        ..Entry::default()
    })
}

// ── Variables ───────────────────────────────────────────────────────

/// The variables a document defines with one plain value, and the ones it
/// uses without defining.
#[derive(Default)]
struct Vars {
    defined: BTreeMap<String, String>,
    missing: Vec<String>,
}

impl Vars {
    /// Define `name`, unless its name looks secret or the value is not plain.
    fn define(&mut self, name: &str, value: &Value) {
        let text = match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => return,
        };
        if name.is_empty() || looks_secret(name) || text.contains("{{") {
            return;
        }
        self.defined.insert(name.to_string(), text);
    }

    /// `text` with every `{{name}}` the document defines put in. `None`
    /// when it uses one the document does not define (named in `missing`):
    /// a value is never guessed, and a name is never left for the harness
    /// to read as one of its own placeholders.
    fn resolve(&mut self, text: &str) -> Option<String> {
        if !text.contains("{{") {
            return Some(text.to_string());
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        let mut whole = true;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else {
                // An opening without its end: not a variable.
                out.push_str(&rest[start..]);
                rest = "";
                break;
            };
            let name = after[..end].trim();
            match self.defined.get(name) {
                Some(value) => out.push_str(value),
                None => {
                    whole = false;
                    let shown: String = name.chars().take(64).collect();
                    name_once(&mut self.missing, shown);
                }
            }
            rest = &after[end + 2..];
        }
        out.push_str(rest);
        whole.then_some(out)
    }

    /// [`Vars::resolve`] on every string of a JSON value.
    fn resolve_value(&mut self, v: &Value) -> Option<Value> {
        Some(match v {
            Value::String(s) => Value::String(self.resolve(s)?),
            Value::Array(a) => {
                let mut out = Vec::with_capacity(a.len());
                for item in a {
                    out.push(self.resolve_value(item)?);
                }
                Value::Array(out)
            }
            Value::Object(m) => {
                let mut out = Map::new();
                for (k, item) in m {
                    out.insert(k.clone(), self.resolve_value(item)?);
                }
                Value::Object(out)
            }
            other => other.clone(),
        })
    }

    /// Header pairs resolved. A secret-looking one keeps its text: it is
    /// removed by name later, whatever it holds.
    fn resolve_pairs(&mut self, pairs: Vec<(String, String)>) -> Option<Vec<(String, String)>> {
        let mut out = Vec::with_capacity(pairs.len());
        let mut whole = true;
        for (name, value) in pairs {
            if looks_secret(&name) {
                out.push((name, String::new()));
                continue;
            }
            match (self.resolve(&name), self.resolve(&value)) {
                (Some(n), Some(v)) => out.push((n, v)),
                _ => whole = false,
            }
        }
        whole.then_some(out)
    }

    /// Query pairs as a query string, resolved and encoded.
    fn query(&mut self, pairs: Vec<(String, String)>) -> Option<Option<String>> {
        let pairs = self.resolve_pairs(pairs)?;
        if pairs.is_empty() {
            return Some(None);
        }
        let mut s = form_urlencoded::Serializer::new(String::new());
        for (k, v) in &pairs {
            s.append_pair(k, v);
        }
        Some(Some(s.finish()))
    }
}

/// A scalar as the text a tool would send.
fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Split what follows the host of a URL written by hand into its path and
/// query. The host (with its scheme, or a `{{variable}}` standing for it)
/// is returned apart.
fn split_raw_url(raw: &str) -> (Option<String>, String, Option<String>) {
    let raw = raw.trim();
    let raw = raw.split('#').next().unwrap_or("");
    let (host, rest) = if let Some(after) = raw
        .strip_prefix("http://")
        .or_else(|| raw.strip_prefix("https://"))
    {
        match after.find(['/', '?']) {
            Some(i) => (Some(after[..i].to_string()), &after[i..]),
            None => (Some(after.to_string()), ""),
        }
    } else if raw.starts_with('/') {
        (None, raw)
    } else {
        match raw.find(['/', '?']) {
            Some(i) => (Some(raw[..i].to_string()), &raw[i..]),
            None => (Some(raw.to_string()), ""),
        }
    };
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q.to_string()).filter(|q| !q.is_empty())),
        None => (rest, None),
    };
    let path = if path.is_empty() { "/" } else { path };
    // A host that is a variable or holds a port keeps only its name.
    let host = host
        .filter(|h| !h.contains("{{") && !h.is_empty())
        .map(|h| h.rsplit('@').next().unwrap_or("").to_string())
        .map(|h| h.split(':').next().unwrap_or("").to_string())
        .filter(|h| !h.is_empty());
    (host, path.to_string(), query)
}

/// Join two query strings.
fn join_query(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a}&{b}")),
        (a, b) => a.or(b),
    }
}

/// A urlencoded form body.
fn form_body(pairs: &[(String, String)]) -> String {
    let mut s = form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        s.append_pair(k, v);
    }
    s.finish()
}

fn has_header(headers: &[(String, String)], name: &str) -> bool {
    headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
}

// ── Postman collections (v2.1) ──────────────────────────────────────

/// Walks a collection: requests in order, what is left out by its place.
struct Postman<'a> {
    vars: Vars,
    out: &'a mut Imported,
    entries: Vec<Read>,
}

impl Postman<'_> {
    fn note_item(&mut self, item: &Value, at: &str) {
        if item["event"].as_array().is_some_and(|e| !e.is_empty()) {
            name_once(&mut self.out.left_out, format!("{at}.event (scripts)"));
        }
        if let Some(kind) = item["auth"]["type"].as_str() {
            if kind != "noauth" {
                name_once(&mut self.out.left_out, format!("{at}.auth ({kind})"));
            }
        }
        if item.get("protocolProfileBehavior").is_some() {
            name_once(
                &mut self.out.left_out,
                format!("{at}.protocolProfileBehavior"),
            );
        }
    }

    fn walk(&mut self, items: &[Value], at: &str, depth: usize) {
        for (i, item) in items.iter().enumerate() {
            if self.entries.len() >= MAX_ENTRIES {
                return;
            }
            let here = format!("{at}item[{i}]");
            self.note_item(item, &here);
            if let Some(children) = item["item"].as_array() {
                if depth >= MAX_FOLDER_DEPTH {
                    name_once(
                        &mut self.out.left_out,
                        format!("{here} (folders nested deeper than {MAX_FOLDER_DEPTH})"),
                    );
                    continue;
                }
                self.walk(children, &format!("{here}."), depth + 1);
            } else if let Some(request) = item.get("request") {
                self.note_item(request, &format!("{here}.request"));
                let read = self.request(request);
                self.entries.push(read);
            }
        }
    }

    fn request(&mut self, request: &Value) -> Read {
        // A request may be only its URL.
        let (method, url) = match request {
            Value::String(url) => ("GET".to_string(), Value::String(url.clone())),
            _ => (
                request["method"]
                    .as_str()
                    .unwrap_or("GET")
                    .to_ascii_uppercase(),
                request["url"].clone(),
            ),
        };
        let mut entry = Entry {
            method,
            weight: 1,
            ..Entry::default()
        };
        self.url(&url, &mut entry)?;
        let headers: Vec<(String, String)> = request["header"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|h| h["disabled"] != json!(true))
                    .filter_map(|h| Some((h["key"].as_str()?.to_string(), scalar(&h["value"])?)))
                    .collect()
            })
            .unwrap_or_default();
        entry.headers = self
            .vars
            .resolve_pairs(headers)
            .ok_or("an unresolved variable")?;
        self.body(&request["body"], &mut entry)?;
        Ok(entry)
    }

    fn url(&mut self, url: &Value, entry: &mut Entry) -> Result<(), &'static str> {
        let raw = match url {
            Value::String(s) => Some(s.as_str()),
            _ => url["raw"].as_str(),
        };
        let segments = url["path"].as_array();
        let (host, path, raw_query) = match (segments, raw) {
            (Some(segments), _) => {
                // Path variables (`:id`) with a value in `url.variable`.
                let path_vars: BTreeMap<&str, &str> = url["variable"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| Some((v["key"].as_str()?, v["value"].as_str()?)))
                            .filter(|(_, v)| !v.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                let mut path = String::new();
                for seg in segments {
                    let seg = match seg {
                        Value::String(s) => s.as_str(),
                        other => other["value"].as_str().unwrap_or(""),
                    };
                    let seg = match seg.strip_prefix(':') {
                        Some(name) if !name.is_empty() => match path_vars.get(name) {
                            Some(value) => value,
                            None => {
                                name_once(&mut self.vars.missing, format!(":{name}"));
                                return Err("an unresolved variable");
                            }
                        },
                        _ => seg,
                    };
                    path.push('/');
                    path.push_str(seg);
                }
                if path.is_empty() {
                    path.push('/');
                }
                let host = match &url["host"] {
                    Value::Array(parts) => Some(
                        parts
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("."),
                    ),
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                }
                .filter(|h| !h.is_empty() && !h.contains("{{"));
                (host, path, None)
            }
            (None, Some(raw)) => split_raw_url(raw),
            (None, None) => return Err("not an http request"),
        };
        if url["protocol"]
            .as_str()
            .is_some_and(|p| !matches!(p, "http" | "https"))
        {
            return Err("not an http request");
        }
        entry.host = host;
        entry.path = self.vars.resolve(&path).ok_or("an unresolved variable")?;
        // The query as the collection lists it (enabled parameters), else
        // as the raw URL holds it.
        entry.query = match url["query"].as_array() {
            Some(list) => {
                let pairs: Vec<(String, String)> = list
                    .iter()
                    .filter(|q| q["disabled"] != json!(true))
                    .filter_map(|q| {
                        Some((
                            q["key"].as_str()?.to_string(),
                            scalar(&q["value"]).unwrap_or_default(),
                        ))
                    })
                    .collect();
                self.vars.query(pairs).ok_or("an unresolved variable")?
            }
            None => match raw_query {
                Some(q) => {
                    let pairs: Vec<(String, String)> = form_urlencoded::parse(q.as_bytes())
                        .map(|(k, v)| (k.into_owned(), v.into_owned()))
                        .collect();
                    self.vars.query(pairs).ok_or("an unresolved variable")?
                }
                None => None,
            },
        };
        Ok(())
    }

    fn body(&mut self, body: &Value, entry: &mut Entry) -> Result<(), &'static str> {
        if body["disabled"] == json!(true) {
            return Ok(());
        }
        match body["mode"].as_str() {
            None | Some("none") => {}
            Some("raw") => {
                let text = body["raw"].as_str().unwrap_or("");
                entry.body = Some(self.vars.resolve(text).ok_or("an unresolved variable")?);
            }
            Some("urlencoded") => {
                let pairs: Vec<(String, String)> = body["urlencoded"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter(|p| p["disabled"] != json!(true))
                            .filter_map(|p| {
                                Some((
                                    p["key"].as_str()?.to_string(),
                                    scalar(&p["value"]).unwrap_or_default(),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                // A secret-looking field leaves the entry out later, by
                // name: its value is not resolved, only kept out.
                let mut resolved = Vec::with_capacity(pairs.len());
                for (k, v) in pairs {
                    if looks_secret(&k) {
                        resolved.push((k, String::new()));
                    } else {
                        resolved.push((k, self.vars.resolve(&v).ok_or("an unresolved variable")?));
                    }
                }
                entry.body = Some(form_body(&resolved));
                if !has_header(&entry.headers, "content-type") {
                    entry.headers.push((
                        "content-type".into(),
                        "application/x-www-form-urlencoded".into(),
                    ));
                }
            }
            Some("graphql") => {
                let query = body["graphql"]["query"].as_str().unwrap_or("");
                let mut doc = Map::new();
                doc.insert(
                    "query".into(),
                    Value::String(self.vars.resolve(query).ok_or("an unresolved variable")?),
                );
                if let Some(text) = body["graphql"]["variables"].as_str() {
                    let text = self.vars.resolve(text).ok_or("an unresolved variable")?;
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        doc.insert("variables".into(), v);
                    }
                }
                entry.body = Some(Value::Object(doc).to_string());
            }
            Some(_) => return Err("a multipart or file body"),
        }
        Ok(())
    }
}

fn read_postman(doc: &Value, out: &mut Imported) -> Result<Vec<Read>, String> {
    let schema = doc["info"]["schema"].as_str().unwrap_or("");
    if !schema.contains("/v2.1") {
        return Err(
            "only Postman collections v2.1 are imported: export the collection as Collection v2.1"
                .into(),
        );
    }
    let mut vars = Vars::default();
    for v in doc["variable"].as_array().into_iter().flatten() {
        if v["disabled"] == json!(true) {
            continue;
        }
        if let Some(name) = v["key"].as_str().or(v["id"].as_str()) {
            vars.define(name, &v["value"]);
        }
    }
    let mut walker = Postman {
        vars,
        out,
        entries: Vec::new(),
    };
    walker.note_item(doc, "collection");
    let items = doc["item"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    walker.walk(items, "", 0);
    let Postman { vars, entries, .. } = walker;
    out.variables = vars.missing;
    Ok(entries)
}

// ── Artillery scripts ───────────────────────────────────────────────

/// Steps of a flow that change how many requests an arrival sends, or when.
const IRREGULAR_STEPS: &[&str] = &["think", "loop", "parallel", "function"];

/// Keys of a request step that are read.
const REQUEST_KEYS: &[&str] = &[
    "url", "headers", "qs", "json", "body", "form", "expect", "auth", "cookie", "name",
];

struct Artillery<'a> {
    vars: Vars,
    out: &'a mut Imported,
    entries: Vec<Read>,
    /// The path of `config.target`, before every request.
    prefix: String,
    defaults: Vec<(String, String)>,
    /// Why the phases cannot become a pattern, when they cannot.
    irregular: Option<String>,
    /// Per scenario read: its weight and its requests.
    flows: Vec<(u32, usize)>,
}

impl Artillery<'_> {
    fn config(&mut self, config: &Value) {
        let Some(config) = config.as_object() else {
            return;
        };
        for (key, value) in config {
            match key.as_str() {
                "target" => {
                    let (host, path, _) = split_raw_url(value.as_str().unwrap_or(""));
                    if let Some(h) = host {
                        name_once(&mut self.out.hosts, h);
                    }
                    self.prefix = path.trim_end_matches('/').to_string();
                }
                "phases" => {}
                "defaults" => {
                    for (k, v) in value.as_object().into_iter().flatten() {
                        if k == "headers" {
                            self.defaults = v
                                .as_object()
                                .map(|m| {
                                    m.iter()
                                        .filter_map(|(n, v)| Some((n.clone(), scalar(v)?)))
                                        .collect()
                                })
                                .unwrap_or_default();
                        } else {
                            name_once(&mut self.out.left_out, format!("config.defaults.{k}"));
                        }
                    }
                }
                "variables" => {
                    for (name, v) in value.as_object().into_iter().flatten() {
                        match v {
                            Value::Array(list) if list.len() == 1 => {
                                self.vars.define(name, &list[0]);
                            }
                            Value::Array(_) => name_once(
                                &mut self.out.left_out,
                                format!("config.variables.{name} (several values)"),
                            ),
                            other => self.vars.define(name, other),
                        }
                    }
                }
                "http" => {
                    for (k, v) in value.as_object().into_iter().flatten() {
                        let timeout = v
                            .as_f64()
                            .filter(|t| t.is_finite() && *t > 0.0)
                            .map(Duration::from_secs_f64)
                            .filter(|t| (MIN_TIMEOUT..=MAX_TIMEOUT).contains(t));
                        match (k.as_str(), timeout) {
                            ("timeout", Some(t)) => self.out.timeout = Some(Dur(t)),
                            _ => name_once(&mut self.out.left_out, format!("config.http.{k}")),
                        }
                    }
                }
                "processor" => {
                    name_once(&mut self.out.left_out, "config.processor (code, never run)")
                }
                other => name_once(&mut self.out.left_out, format!("config.{other}")),
            }
        }
    }

    fn scenarios(&mut self, scenarios: &[Value]) {
        for (i, scenario) in scenarios.iter().enumerate() {
            let here = format!("scenarios[{i}]");
            if let Some(engine) = scenario["engine"].as_str().filter(|e| *e != "http") {
                name_once(&mut self.out.left_out, format!("{here} (engine {engine})"));
                self.irregular
                    .get_or_insert_with(|| format!("{here} is not an http scenario"));
                continue;
            }
            for hook in ["beforeScenario", "afterScenario"] {
                if scenario.get(hook).is_some() {
                    name_once(
                        &mut self.out.left_out,
                        format!("{here}.{hook} (code, never run)"),
                    );
                }
            }
            let weight = match scenario.get("weight") {
                None => 1,
                Some(w) => match w
                    .as_u64()
                    .filter(|w| (1..=u64::from(MAX_WEIGHT)).contains(w))
                {
                    Some(w) => w as u32,
                    None => {
                        name_once(&mut self.out.left_out, format!("{here}.weight"));
                        self.irregular
                            .get_or_insert_with(|| format!("{here}.weight is not 1 to 1000"));
                        1
                    }
                },
            };
            let mut requests = 0;
            for (j, step) in scenario["flow"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                if self.entries.len() >= MAX_ENTRIES {
                    return;
                }
                let at = format!("{here}.flow[{j}]");
                let Some((kind, spec)) = step.as_object().and_then(|m| m.iter().next()) else {
                    name_once(&mut self.out.left_out, at);
                    continue;
                };
                let method = kind.to_ascii_uppercase();
                if !METHODS.contains(&method.as_str()) {
                    let code = if kind == "function" {
                        " (code, never run)"
                    } else {
                        ""
                    };
                    name_once(&mut self.out.left_out, format!("{at}.{kind}{code}"));
                    if IRREGULAR_STEPS.contains(&kind.as_str()) {
                        self.irregular.get_or_insert_with(|| {
                            format!("{at}.{kind} changes the requests of an arrival")
                        });
                    }
                    continue;
                }
                let read = self.request(&method, spec, &at, weight);
                requests += 1;
                self.entries.push(read);
            }
            self.flows.push((weight, requests));
        }
    }

    fn request(&mut self, method: &str, spec: &Value, at: &str, weight: u32) -> Read {
        let empty = Map::new();
        let spec = spec.as_object().unwrap_or(&empty);
        let mut conditional = false;
        let mut multipart = false;
        for key in spec.keys() {
            match key.as_str() {
                k if REQUEST_KEYS.contains(&k) => {}
                "ifTrue" => {
                    conditional = true;
                    name_once(&mut self.out.left_out, format!("{at}.ifTrue"));
                }
                "formData" => multipart = true,
                "beforeRequest" | "afterResponse" => name_once(
                    &mut self.out.left_out,
                    format!("{at}.{key} (code, never run)"),
                ),
                other => name_once(&mut self.out.left_out, format!("{at}.{other}")),
            }
        }
        if conditional {
            self.irregular
                .get_or_insert_with(|| format!("{at}.ifTrue makes the request conditional"));
            return Err("a conditional request");
        }
        if multipart {
            return Err("a multipart or file body");
        }
        let url = spec
            .get("url")
            .and_then(Value::as_str)
            .ok_or("not an http request")?;
        // The query apart first (a secret-looking parameter is removed by
        // name, whatever variable it holds), then the rest resolved: a
        // variable at the start of a URL is a path here, never a host.
        let url = url.split('#').next().unwrap_or("");
        let (head, raw_query) = match url.split_once('?') {
            Some((h, q)) => (h, Some(q.to_string()).filter(|q| !q.is_empty())),
            None => (url, None),
        };
        let head = self.vars.resolve(head).ok_or("an unresolved variable")?;
        // An absolute URL names its own host and path; a relative one
        // follows the target's.
        let (host, path) = if head.starts_with("http://") || head.starts_with("https://") {
            let (host, path, _) = split_raw_url(&head);
            (host, path)
        } else {
            let slash = if head.starts_with('/') { "" } else { "/" };
            (None, format!("{}{slash}{head}", self.prefix))
        };
        let mut entry = Entry {
            method: method.to_string(),
            host,
            path,
            weight,
            ..Entry::default()
        };
        let from_url: Vec<(String, String)> = raw_query
            .map(|q| {
                form_urlencoded::parse(q.as_bytes())
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let from_qs: Vec<(String, String)> = spec
            .get("qs")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), scalar(v)?)))
                    .collect()
            })
            .unwrap_or_default();
        entry.query = join_query(
            self.vars.query(from_url).ok_or("an unresolved variable")?,
            self.vars.query(from_qs).ok_or("an unresolved variable")?,
        );
        // The defaults, then the request's own (which replace them).
        let own: Vec<(String, String)> = spec
            .get("headers")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), scalar(v)?)))
                    .collect()
            })
            .unwrap_or_default();
        let mut headers: Vec<(String, String)> = self
            .defaults
            .iter()
            .filter(|(n, _)| !has_header(&own, n))
            .cloned()
            .collect();
        headers.extend(own);
        entry.headers = self
            .vars
            .resolve_pairs(headers)
            .ok_or("an unresolved variable")?;
        // Credentials a step carries outside its headers: removed, named.
        if spec.contains_key("auth") {
            name_once(&mut self.out.removed, "authorization");
        }
        if spec.contains_key("cookie") {
            name_once(&mut self.out.removed, "cookie");
        }
        if let Some(v) = spec.get("json") {
            let resolved = self.vars.resolve_value(v).ok_or("an unresolved variable")?;
            entry.body = Some(resolved.to_string());
            if !has_header(&entry.headers, "content-type") {
                entry
                    .headers
                    .push(("content-type".into(), "application/json".into()));
            }
        } else if let Some(text) = spec.get("body").and_then(scalar) {
            entry.body = Some(self.vars.resolve(&text).ok_or("an unresolved variable")?);
        } else if let Some(form) = spec.get("form").and_then(Value::as_object) {
            let mut pairs = Vec::with_capacity(form.len());
            for (k, v) in form {
                let v = scalar(v).unwrap_or_default();
                if looks_secret(k) {
                    pairs.push((k.clone(), String::new()));
                } else {
                    pairs.push((
                        k.clone(),
                        self.vars.resolve(&v).ok_or("an unresolved variable")?,
                    ));
                }
            }
            entry.body = Some(form_body(&pairs));
            if !has_header(&entry.headers, "content-type") {
                entry.headers.push((
                    "content-type".into(),
                    "application/x-www-form-urlencoded".into(),
                ));
            }
        }
        // The expect plugin's status; its other checks have no counterpart.
        let expects: Vec<&Value> = match spec.get("expect") {
            Some(Value::Array(list)) => list.iter().collect(),
            Some(one @ Value::Object(_)) => vec![one],
            _ => Vec::new(),
        };
        for check in expects {
            for (k, v) in check.as_object().into_iter().flatten() {
                if k == "statusCode" {
                    let codes: Vec<u64> = match v {
                        Value::Array(a) => a.iter().filter_map(Value::as_u64).collect(),
                        other => other.as_u64().into_iter().collect(),
                    };
                    entry.expect.extend(
                        codes
                            .into_iter()
                            .filter(|c| (100..=599).contains(c))
                            .map(|c| c as u16),
                    );
                } else {
                    name_once(&mut self.out.left_out, format!("{at}.expect.{k}"));
                }
            }
        }
        entry.expect.sort_unstable();
        entry.expect.dedup();
        entry.expect.truncate(32);
        Ok(entry)
    }

    /// The phases as a pattern chain: `Err` says why they do not map.
    fn pattern(&self, phases: &[Value]) -> Result<(Pattern, f64), String> {
        if let Some(why) = &self.irregular {
            return Err(why.clone());
        }
        if phases.len() > MAX_STAGES {
            return Err(format!("more than {MAX_STAGES} phases"));
        }
        let total_weight: u64 = self.flows.iter().map(|(w, _)| u64::from(*w)).sum();
        let weighted: u64 = self
            .flows
            .iter()
            .map(|(w, n)| u64::from(*w) * *n as u64)
            .sum();
        if total_weight == 0 || weighted == 0 {
            return Err("the flows hold no requests".into());
        }
        // An arrival runs one flow: its requests, weighted over the flows.
        let per_arrival = weighted as f64 / total_weight as f64;
        let number = |phase: &Value, key: &str, i: usize| -> Result<Option<f64>, String> {
            match phase.get(key) {
                None => Ok(None),
                Some(v) => v
                    .as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.0)
                    .map(Some)
                    .ok_or_else(|| format!("phases[{i}].{key} is not a number")),
            }
        };
        let seconds = |phase: &Value, key: &str, i: usize| -> Result<Option<Duration>, String> {
            match phase.get(key) {
                None => Ok(None),
                Some(Value::String(text)) => super::scenario::parse_duration(text)
                    .map(Some)
                    .ok_or_else(|| format!("phases[{i}].{key} is not a duration")),
                Some(v) => v
                    .as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.0 && *n <= 366.0 * 86_400.0)
                    .map(|n| Some(Duration::from_secs_f64(n)))
                    .ok_or_else(|| format!("phases[{i}].{key} is not a duration")),
            }
        };
        let mut stages: Vec<Pattern> = Vec::with_capacity(phases.len());
        for (i, phase) in phases.iter().enumerate() {
            let map = phase
                .as_object()
                .ok_or_else(|| format!("phases[{i}] is not a mapping"))?;
            if let Some(other) = map.keys().find(|k| {
                !matches!(
                    k.as_str(),
                    "duration" | "arrivalRate" | "rampTo" | "arrivalCount" | "pause" | "name"
                )
            }) {
                return Err(format!("phases[{i}].{other} has no counterpart"));
            }
            let duration = seconds(phase, "duration", i)?;
            let pause = seconds(phase, "pause", i)?;
            let rate = number(phase, "arrivalRate", i)?;
            let ramp_to = number(phase, "rampTo", i)?;
            let count = number(phase, "arrivalCount", i)?;
            let stage = match (pause, duration, rate, ramp_to, count) {
                (Some(p), None, None, None, None) => constant(0.0, p),
                (None, Some(d), Some(r), None, None) => constant(r * per_arrival, d),
                (None, Some(d), Some(r), Some(to), None) => Pattern::Ramp {
                    from: r * per_arrival,
                    to: to * per_arrival,
                    over: Dur(d),
                    curve: Curve::Linear,
                    jitter: None,
                    sync: None,
                    then: None,
                },
                (None, Some(d), None, None, Some(n)) if !d.is_zero() => {
                    constant(n * per_arrival / d.as_secs_f64(), d)
                }
                _ => return Err(format!("phases[{i}] has no counterpart")),
            };
            let (peak, length) = match &stage {
                Pattern::Ramp { from, to, over, .. } => (from.max(*to), over.0),
                Pattern::Constant { rate, hold, .. } => (*rate, hold.0),
                _ => (0.0, Duration::ZERO),
            };
            if length.is_zero() {
                return Err(format!("phases[{i}] lasts 0 seconds"));
            }
            if peak > MAX_VALUE {
                return Err(format!("phases[{i}] is over the largest rate"));
            }
            stages.push(stage);
        }
        // The chain, from its end.
        let mut chain: Option<Pattern> = None;
        for mut stage in stages.into_iter().rev() {
            if let Some(next) = chain.take() {
                match &mut stage {
                    Pattern::Constant { then, .. } | Pattern::Ramp { then, .. } => {
                        *then = Some(Box::new(next));
                    }
                    _ => {}
                }
            }
            chain = Some(stage);
        }
        chain
            .map(|p| (p, per_arrival))
            .ok_or_else(|| "the script has no phases".to_string())
    }
}

fn constant(rate: f64, hold: Duration) -> Pattern {
    Pattern::Constant {
        rate,
        hold: Dur(hold),
        jitter: None,
        sync: None,
        then: None,
    }
}

/// The phases as a pattern (or why not), to use once the requests are known
/// to be all kept. `None`: the script has no phases.
type Phases = Option<Result<(Pattern, f64), String>>;

fn read_artillery(doc: &Value, out: &mut Imported) -> Result<(Vec<Read>, Phases), String> {
    let Some(scenarios) = doc["scenarios"].as_array() else {
        return Err("an Artillery script needs scenarios with a flow each".into());
    };
    for key in doc.as_object().into_iter().flat_map(|m| m.keys()) {
        if !matches!(key.as_str(), "config" | "scenarios") {
            name_once(&mut out.left_out, key.clone());
        }
    }
    let mut reader = Artillery {
        vars: Vars::default(),
        out,
        entries: Vec::new(),
        prefix: String::new(),
        defaults: Vec::new(),
        irregular: None,
        flows: Vec::new(),
    };
    reader.config(&doc["config"]);
    reader.scenarios(scenarios);
    let phases = doc["config"]["phases"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let pattern = (!phases.is_empty()).then(|| reader.pattern(phases));
    let Artillery { vars, entries, .. } = reader;
    out.variables = vars.missing;
    Ok((entries, pattern))
}

// ── What every format shares ────────────────────────────────────────

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

/// Whether a scenario would take the template: what `validate` refuses is
/// left out here, with the reason, so an import always validates.
fn fits(t: &RequestTemplate) -> Result<(), &'static str> {
    if !METHODS.contains(&t.method.as_str()) {
        return Err("a method the harness does not send");
    }
    if !t.path.starts_with('/') || t.path.len() > 2048 || t.path.contains(['\r', '\n', ' ']) {
        return Err("a path that is not a URL path");
    }
    if super::safety::is_control_prefix(&t.path) {
        return Err("a control plane path");
    }
    if t.headers.len() > MAX_HEADERS {
        return Err("more headers than a request takes");
    }
    let reads_as_placeholder = |text: &str| {
        text.find("{{")
            .is_some_and(|start| text[start..].contains("}}"))
    };
    for (name, value) in &t.headers {
        if axum::http::HeaderName::from_bytes(name.as_bytes()).is_err()
            || value.contains(['\r', '\n'])
            || value.len() > 8192
        {
            return Err("a header that is not a header");
        }
        if reads_as_placeholder(value) {
            return Err("text that reads as a placeholder");
        }
    }
    let body = match &t.body {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    if body.len() > MAX_BODY_BYTES {
        return Err("a body over the limit");
    }
    if reads_as_placeholder(&t.path) || reads_as_placeholder(&body) {
        return Err("text that reads as a placeholder");
    }
    Ok(())
}

/// The format of a document, by its shape.
pub fn detect(doc: &Value) -> Result<Format, String> {
    if doc["log"]["entries"].is_array() {
        return Ok(Format::Har);
    }
    let info = &doc["info"];
    if info["schema"]
        .as_str()
        .is_some_and(|s| s.contains("postman"))
        || info.get("_postman_id").is_some()
        || (info.is_object() && doc["item"].is_array())
    {
        return Ok(Format::Postman);
    }
    if doc["scenarios"].is_array() && (doc["config"].is_object() || doc.get("config").is_none()) {
        return Ok(Format::Artillery);
    }
    if doc["requests"].is_array() && doc["order"].is_array() {
        return Err(
            "a Postman collection v1: only v2.1 is imported, export the collection as Collection v2.1"
                .into(),
        );
    }
    if doc["requests"].is_array() || doc.is_array() {
        return Ok(Format::Captures);
    }
    Err(format!(
        "not a document this import reads: expected a HAR file (log.entries), the answer of GET /_rustybin/requests, a Postman collection v2.1 or an Artillery script (formats: {})",
        Format::ALL.join(", ")
    ))
}

/// Turn a recording into request templates ([`import_as`], the format read
/// from the document).
pub fn import(doc: &Value) -> Result<Imported, String> {
    import_as(doc, None)
}

/// Turn a document into request templates. Identical requests become one
/// template with their count as its weight, so the mix is the document's.
pub fn import_as(doc: &Value, format: Option<Format>) -> Result<Imported, String> {
    let format = match format {
        Some(f) => f,
        None => detect(doc)?,
    };
    let mut out = Imported {
        format,
        ..Imported::default()
    };
    let mut phases: Phases = None;
    let entries: Vec<Read> = match format {
        Format::Har => doc["log"]["entries"]
            .as_array()
            .ok_or("not a HAR file: log.entries is missing")?
            .iter()
            .take(MAX_ENTRIES)
            .map(from_har_entry)
            .collect(),
        Format::Captures => doc["requests"]
            .as_array()
            .or(doc.as_array())
            .ok_or("not the answer of GET /_rustybin/requests: requests is missing")?
            .iter()
            .take(MAX_ENTRIES)
            .map(from_capture)
            .collect(),
        Format::Postman => read_postman(doc, &mut out)?,
        Format::Artillery => {
            let (entries, p) = read_artillery(doc, &mut out)?;
            phases = p;
            entries
        }
    };
    out.entries = entries.len();
    let mut seen: BTreeMap<String, RequestTemplate> = BTreeMap::new();
    for entry in entries {
        let read = entry.and_then(|entry| {
            let body = body_of(&entry)?;
            let template = RequestTemplate {
                path: path_of(&entry, &mut out.removed),
                headers: headers_of(&entry, &mut out.removed),
                method: entry.method.clone(),
                body,
                // What the document says to expect, else a recorded
                // failure: the replay should expect it too.
                expect_status: if entry.expect.is_empty() {
                    entry
                        .status
                        .filter(|s| *s >= 400)
                        .map(|s| vec![s])
                        .unwrap_or_default()
                } else {
                    entry.expect.clone()
                },
                weight: entry.weight.max(1),
            };
            fits(&template)?;
            Ok((entry.host, template))
        });
        let (host, template) = match read {
            Ok(kept) => kept,
            Err(why) => {
                *out.skipped.entry(why.into()).or_default() += 1;
                continue;
            }
        };
        if let Some(h) = host {
            name_once(&mut out.hosts, h);
        }
        let key = serde_json::to_string(&json!([
            template.method,
            template.path,
            template.headers,
            template.body,
            template.expect_status
        ]))
        .map_err(|e| e.to_string())?;
        seen.entry(key)
            .and_modify(|t| t.weight = t.weight.saturating_add(template.weight))
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
            .insert(format!("rarer than the {MAX_TEMPLATES} kept"), dropped);
    }
    if requests.is_empty() {
        return Err(if format.recording() {
            "nothing to replay: every entry was left out".to_string()
        } else {
            let mut why: Vec<String> = out
                .skipped
                .iter()
                .map(|(reason, n)| format!("{n} with {reason}"))
                .collect();
            if !out.variables.is_empty() {
                why.push(format!(
                    "variables not defined: {}",
                    out.variables.join(", ")
                ));
            }
            format!(
                "nothing to import: every request was left out{}{}",
                if why.is_empty() { "" } else { " (" },
                if why.is_empty() {
                    String::new()
                } else {
                    format!("{})", why.join("; "))
                }
            )
        });
    }
    // A template's weight is 1 to 1000: a larger mix keeps its proportions.
    let largest = requests.iter().map(|t| t.weight).max().unwrap_or(1);
    if largest > MAX_WEIGHT {
        for t in &mut requests {
            let scaled = (f64::from(t.weight) * f64::from(MAX_WEIGHT) / f64::from(largest)).round();
            t.weight = (scaled as u32).clamp(1, MAX_WEIGHT);
        }
        out.notes.push(format!(
            "weights: scaled to the largest a request takes ({MAX_WEIGHT}), the proportions kept"
        ));
    }
    // The phases describe arrivals of whole flows: they are a pattern only
    // when every request of every flow was kept.
    if let Some(pattern) = phases {
        let pattern = if out.skipped.is_empty() {
            pattern
        } else {
            Err(
                "requests of the flows were left out, so the requests of an arrival are not known"
                    .into(),
            )
        };
        match pattern {
            Ok((p, per_arrival)) => {
                out.pattern = Some(p);
                out.notes.push(format!(
                    "pattern: an Artillery phase counts arrivals and each runs one flow, so every rate is the phase's arrivals per second times {per_arrival} requests per arrival; requests are picked by weight, not in the order of a flow"
                ));
            }
            Err(why) => name_once(&mut out.left_out, format!("config.phases ({why})")),
        }
    }
    out.requests = requests;
    Ok(out)
}

/// Turn the text of a document into request templates: JSON, or YAML (an
/// Artillery script). Anything else, a program in a scripting language
/// above all, is refused: nothing here runs or translates code.
pub fn import_text(text: &str, format: Option<Format>) -> Result<Imported, String> {
    let bounded = |doc: &Value, format: Option<Format>| -> Result<(), String> {
        let format = match format {
            Some(f) => f,
            None => detect(doc)?,
        };
        if !format.recording() && text.len() > MAX_TEXT_BYTES {
            return Err(format!(
                "a collection or a script is at most {} MiB",
                MAX_TEXT_BYTES / (1024 * 1024)
            ));
        }
        Ok(())
    };
    if let Ok(doc) = serde_json::from_str::<Value>(text) {
        if doc.is_object() || doc.is_array() {
            bounded(&doc, format)?;
            return import_as(&doc, format);
        }
    }
    let refused = || {
        format!(
            "not a document this import reads. A program (a k6, Locust or Gatling script, a JavaScript processor) is never run or translated: export a HAR file, a Postman collection v2.1 or an Artillery script instead (formats: {})",
            Format::ALL.join(", ")
        )
    };
    if text.len() > MAX_TEXT_BYTES {
        return Err(format!(
            "a collection or a script is at most {} MiB",
            MAX_TEXT_BYTES / (1024 * 1024)
        ));
    }
    match serde_yaml::from_str::<Value>(text) {
        // A mapping that is none of the formats is as likely a program
        // that happens to read as YAML (a line ending in a colon).
        Ok(doc) if doc.is_object() && (format.is_some() || detect(&doc).is_ok()) => {
            import_as(&doc, format)
        }
        _ => Err(refused()),
    }
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod tests;
