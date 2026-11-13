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
//!
//! The caller may give values for variables ([`Variables`]), one by one or
//! as a Postman environment file: collections keep their base URL and ids
//! in an environment the document does not carry. A given value is put in
//! where the document's own would be, and nothing else about the import
//! changes: credentials are removed after it, and what comes out is
//! validated like any other source (`fits`). A value is never in an
//! error, a note or `left_out`, only its name.
//!
//! A variable named like a harness placeholder (`{{id}}`, `{{seq}}`) is
//! the document's variable, never the harness's: without a value its
//! request is left out like any other, and with one (the document's own,
//! or the caller's, which a note then names) the value is put in.

use std::collections::{BTreeMap, BTreeSet};
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
/// Variables given with an import, at most.
pub const MAX_VARIABLES: usize = 64;
/// The largest value of a given variable.
pub const MAX_VARIABLE_BYTES: usize = 1024;
/// The longest name of a given variable (a name in `variables` is cut here
/// too).
pub const MAX_VARIABLE_NAME: usize = 64;
/// The largest Postman environment file handed over as the variables.
pub const MAX_ENVIRONMENT_BYTES: usize = 256 * 1024;
/// Entries of an environment file read, at most.
pub const MAX_ENVIRONMENT_ENTRIES: usize = 1000;

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
    /// An Arazzo 1.0 document: its workflows' steps against the instance's
    /// mocks (`crate::replay::arazzo`).
    Arazzo,
}

impl Format {
    pub const ALL: &'static [&'static str] = &["har", "captures", "postman", "artillery", "arazzo"];

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "har" => Some(Format::Har),
            "captures" => Some(Format::Captures),
            "postman" => Some(Format::Postman),
            "artillery" => Some(Format::Artillery),
            "arazzo" => Some(Format::Arazzo),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Har => "har",
            Format::Captures => "captures",
            Format::Postman => "postman",
            Format::Artillery => "artillery",
            Format::Arazzo => "arazzo",
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

/// Why an import made nothing, with the variables that had no value: a
/// caller can ask for exactly those and import again.
#[derive(Debug)]
pub struct Refusal {
    pub error: String,
    /// Names only, as [`Imported::variables`] holds them.
    pub variables: Vec<String>,
}

impl From<String> for Refusal {
    fn from(error: String) -> Self {
        Self {
            error,
            variables: Vec::new(),
        }
    }
}

impl From<&str> for Refusal {
    fn from(error: &str) -> Self {
        error.to_string().into()
    }
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

/// Values the caller gives for variables of the document, by name in the
/// document's own syntax (`baseUrl` for `{{baseUrl}}`, `:id` for a Postman
/// path variable), typed one by one or handed over as a Postman environment
/// file. Checked when made, so one that exists is within the caps and has
/// no name that looks like a credential. `Debug` shows the names only.
///
/// A name of a harness placeholder (`scenario::PLACEHOLDERS`) may be given:
/// the value is put in at import time, and a note says, by name, that the
/// requests carry a constant where the harness would generate a value.
#[derive(Clone, Default)]
pub struct Variables {
    values: BTreeMap<String, String>,
    /// The names that came from an environment file and were not given
    /// one by one as well: one the document never uses is no typo.
    from_environment: BTreeSet<String>,
    /// An environment was handed over, whatever was taken from it.
    environment: bool,
    /// What was not taken from the environment, by name.
    notes: Vec<String>,
}

impl std::fmt::Debug for Variables {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.values.keys()).finish()
    }
}

/// Why a name and its value are not a variable an import takes. Never
/// holds the value.
enum Unfit {
    Name,
    Credential,
    NotScalar,
    TooLong,
    Braces,
}

/// One given variable, checked: its name as the document writes it and its
/// value as text. The same for a variable typed and for an entry of an
/// environment file.
fn check_variable(name: &str, value: &Value) -> Result<(String, String), Unfit> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_VARIABLE_NAME
        || name.contains("{{")
        || name.contains("}}")
        || name.chars().any(char::is_control)
    {
        return Err(Unfit::Name);
    }
    if looks_secret(name) {
        return Err(Unfit::Credential);
    }
    let text = scalar(value).ok_or(Unfit::NotScalar)?;
    if text.len() > MAX_VARIABLE_BYTES {
        return Err(Unfit::TooLong);
    }
    if text.contains("{{") {
        return Err(Unfit::Braces);
    }
    Ok((name.to_string(), text))
}

impl Variables {
    /// Nothing given, and no environment handed over.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty() && !self.environment
    }

    /// The names given.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }

    /// A JSON object of name to value (text, a number, true or false). A
    /// refusal names the variable, never its value.
    pub fn from_value(v: &Value) -> Result<Self, String> {
        let map = match v {
            Value::Null => return Ok(Self::default()),
            Value::Object(map) => map,
            _ => return Err("variables: a map of name to value".into()),
        };
        if map.len() > MAX_VARIABLES {
            return Err(format!("variables: at most {MAX_VARIABLES}"));
        }
        let mut values = BTreeMap::new();
        for (name, value) in map {
            let shown = name.trim();
            let (name, text) = check_variable(name, value).map_err(|unfit| match unfit {
                Unfit::Name => format!(
                    "variables: a name is 1 to {MAX_VARIABLE_NAME} characters as the document writes it, without the braces"
                ),
                Unfit::Credential => format!(
                    "variables.{shown}: a name that looks like a credential is not taken: an import removes credentials, give the scenario an identity instead"
                ),
                Unfit::NotScalar => {
                    format!("variables.{shown}: a value is text, a number, true or false")
                }
                Unfit::TooLong => {
                    format!("variables.{shown}: a value is at most {MAX_VARIABLE_BYTES} bytes")
                }
                Unfit::Braces => format!(
                    "variables.{shown}: a value does not hold {{{{, which would read as another variable or a placeholder"
                ),
            })?;
            values.insert(name, text);
        }
        Ok(Self {
            values,
            ..Self::default()
        })
    }

    /// [`Variables::from_value`] with a Postman environment (or globals)
    /// export beside it: the document `{"values": [{"key", "value",
    /// "enabled", "type"}]}` or its text. A variable given one by one wins
    /// over the environment's.
    ///
    /// Every entry meets the checks a given variable meets. An environment
    /// is a file of somebody's whole setup, so an entry that fails one is
    /// not taken and is named in a note (never its value) where a typed
    /// variable would be refused: one marked `secret` (a credential), one
    /// whose name looks like a credential, one without a value, one whose
    /// value is no variable's. A disabled entry is passed over. The cap on
    /// the number of variables counts what is taken with what is given.
    ///
    /// A refusal quotes nothing of the environment.
    pub fn with_environment(variables: &Value, environment: &Value) -> Result<Self, String> {
        const SHAPE: &str = "environment: a Postman environment or globals export (an object with a values list), or its text";
        let mut out = Self::from_value(variables)?;
        let parsed;
        let doc = match environment {
            Value::Null => return Ok(out),
            Value::String(text) => {
                // Bounded before it is parsed.
                if text.len() > MAX_ENVIRONMENT_BYTES {
                    return Err(format!(
                        "environment: at most {} KiB",
                        MAX_ENVIRONMENT_BYTES / 1024
                    ));
                }
                parsed = serde_json::from_str::<Value>(text).map_err(|_| SHAPE.to_string())?;
                &parsed
            }
            other => other,
        };
        let entries = doc
            .as_object()
            .and_then(|o| o.get("values"))
            .and_then(Value::as_array)
            .ok_or_else(|| SHAPE.to_string())?;
        if entries.len() > MAX_ENVIRONMENT_ENTRIES {
            return Err(format!(
                "environment: at most {MAX_ENVIRONMENT_ENTRIES} entries"
            ));
        }
        out.environment = true;
        let mut taken: BTreeMap<String, String> = BTreeMap::new();
        let (mut secret, mut credential, mut empty, mut unfit) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut unnamed = 0usize;
        for (i, entry) in entries.iter().enumerate() {
            let Some(key) = entry
                .as_object()
                .and_then(|e| e.get("key"))
                .and_then(Value::as_str)
            else {
                return Err(format!(
                    "environment.values[{i}]: an entry is an object with a key and a value"
                ));
            };
            if entry["enabled"] == json!(false) || entry["disabled"] == json!(true) {
                continue;
            }
            // The name first, with a value that passes: an entry is named
            // in a note only when its name is one a variable could have.
            let marked_secret = entry["type"] == json!("secret");
            let name = match check_variable(key, &Value::Bool(true)) {
                Ok((name, _)) => name,
                Err(Unfit::Credential) => {
                    let shown = key.trim().to_string();
                    taken.remove(&shown);
                    let list = if marked_secret {
                        &mut secret
                    } else {
                        &mut credential
                    };
                    name_once(list, shown);
                    continue;
                }
                Err(_) => {
                    unnamed += 1;
                    continue;
                }
            };
            // A later entry of the same name stands for the earlier one.
            taken.remove(&name);
            if marked_secret {
                name_once(&mut secret, name);
                continue;
            }
            let value = &entry["value"];
            if value.is_null() || value.as_str() == Some("") {
                name_once(&mut empty, name);
                continue;
            }
            match check_variable(key, value) {
                Ok((name, text)) => {
                    taken.insert(name, text);
                }
                Err(_) => name_once(&mut unfit, name),
            }
        }
        for (name, text) in taken {
            if out.values.contains_key(&name) {
                continue;
            }
            if out.values.len() >= MAX_VARIABLES {
                return Err(format!(
                    "variables: at most {MAX_VARIABLES}, the environment's included: disable the entries the document does not use"
                ));
            }
            out.from_environment.insert(name.clone());
            out.values.insert(name, text);
        }
        let unfit_why = format!(
            "a value that is not text, a number, true or false of at most {MAX_VARIABLE_BYTES} bytes without {{{{"
        );
        for (names, why) in [
            (secret, "marked secret (a credential is never taken)"),
            (
                credential,
                "a name that looks like a credential (an import removes credentials)",
            ),
            (empty, "no value"),
            (unfit, unfit_why.as_str()),
        ] {
            if !names.is_empty() {
                out.notes.push(format!(
                    "environment: not taken, {why}: {}",
                    names.join(", ")
                ));
            }
        }
        if unnamed > 0 {
            out.notes.push(format!(
                "environment: not taken, a name that is not a variable's: {unnamed} entries"
            ));
        }
        Ok(out)
    }
}

/// The note that says which harness placeholder names carry a given value.
/// Names only.
fn fixed_placeholders<'a>(used: impl Iterator<Item = &'a str>) -> Option<String> {
    let fixed: Vec<&str> = used
        .filter(|name| super::scenario::PLACEHOLDERS.contains(name))
        .collect();
    (!fixed.is_empty()).then(|| {
        format!(
            "variables: a harness placeholder name was given a fixed value: {}. The imported requests carry that constant where the harness would generate a value for each request",
            fixed.join(", ")
        )
    })
}

/// The variables a document defines with one plain value or the caller
/// gave, and the ones it uses without either.
#[derive(Default)]
struct Vars {
    /// The caller's: they win over the document's.
    given: BTreeMap<String, String>,
    /// The given ones that came from an environment file.
    from_environment: BTreeSet<String>,
    defined: BTreeMap<String, String>,
    missing: Vec<String>,
    /// The given ones the document used.
    used: BTreeSet<String>,
    /// The request being read uses a variable that has no value. Reading
    /// goes on to its end, so every such variable is named at once.
    incomplete: bool,
}

/// Why a request with a variable that has no value is left out.
const UNRESOLVED: &str = "an unresolved variable";

impl Vars {
    fn with(given: &Variables) -> Self {
        Self {
            given: given.values.clone(),
            from_environment: given.from_environment.clone(),
            ..Self::default()
        }
    }

    /// The value of `name`: the caller's, else the document's.
    fn value(&mut self, name: &str) -> Option<String> {
        if let Some(value) = self.given.get(name) {
            self.used.insert(name.to_string());
            return Some(value.clone());
        }
        self.defined.get(name).cloned()
    }

    /// Only the caller's value of `name`.
    fn given(&mut self, name: &str) -> Option<String> {
        let value = self.given.get(name)?.clone();
        self.used.insert(name.to_string());
        Some(value)
    }

    /// [`Vars::resolve`], without naming what is missing: for a place whose
    /// variable does not leave the request out (the host).
    fn resolve_quiet(&mut self, text: &str) -> Option<String> {
        let (before, incomplete) = (self.missing.len(), self.incomplete);
        let out = self.resolve(text);
        self.missing.truncate(before);
        self.incomplete = incomplete;
        out
    }

    /// Start a request.
    fn begin(&mut self) {
        self.incomplete = false;
    }

    /// The request read so far is whole, or the reason it is left out.
    fn whole(&self) -> Result<(), &'static str> {
        if self.incomplete {
            Err(UNRESOLVED)
        } else {
            Ok(())
        }
    }

    /// [`Vars::resolve`], or nothing with the request marked incomplete.
    fn put(&mut self, text: &str) -> String {
        self.resolve(text).unwrap_or_default()
    }

    /// Say which given variables the document never used and which
    /// placeholder names carry a given value (names only), and hand over
    /// the names that have no value. An environment's entry the document
    /// never uses is not said: the file is a whole setup, not a typo.
    fn finish(self, out: &mut Imported) {
        out.notes
            .extend(fixed_placeholders(self.used.iter().map(String::as_str)));
        let unused: Vec<&str> = self
            .given
            .keys()
            .filter(|name| !self.used.contains(*name) && !self.from_environment.contains(*name))
            .map(String::as_str)
            .collect();
        if !unused.is_empty() {
            out.notes.push(format!(
                "variables: given and not used by the document: {}",
                unused.join(", ")
            ));
        }
        out.variables = self.missing;
    }

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

    /// `text` with every `{{name}}` the caller gave or the document defines
    /// put in. `None` when it uses one that has no value (named in `missing`):
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
            match self.value(name) {
                Some(value) => out.push_str(&value),
                None => {
                    whole = false;
                    let shown: String = name.chars().take(MAX_VARIABLE_NAME).collect();
                    name_once(&mut self.missing, shown);
                }
            }
            rest = &after[end + 2..];
        }
        out.push_str(rest);
        if !whole {
            self.incomplete = true;
        }
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
    let (host, path, query) = split_raw(raw);
    (host.and_then(clean_host), path, query)
}

/// A host as written (maybe with credentials and a port) as its name. One
/// that is a variable is no name.
fn clean_host(host: String) -> Option<String> {
    Some(host)
        .filter(|h| !h.contains("{{") && !h.is_empty())
        .map(|h| h.rsplit('@').next().unwrap_or("").to_string())
        .map(|h| h.split(':').next().unwrap_or("").to_string())
        .filter(|h| !h.is_empty())
}

/// [`split_raw_url`] with the host as it is written, a variable included.
fn split_raw(raw: &str) -> (Option<String>, String, Option<String>) {
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
    /// Variables in the place of a host that have no value.
    host_vars: Vec<String>,
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
        // Read to the end even when a variable has no value, so the answer
        // names every one the request needs; it is left out all the same.
        self.vars.begin();
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
        entry.headers = self.vars.resolve_pairs(headers).unwrap_or_default();
        let body = self.body(&request["body"], &mut entry);
        self.vars.whole()?;
        body?;
        Ok(entry)
    }

    /// The host of a URL as its name, and the path that leads the request
    /// when a variable in the host's place holds one (`{{baseUrl}}` as
    /// `https://api.example.com/v1`). A variable without a value is dropped
    /// with the host, as every host is, and named in a note.
    fn host(&mut self, raw: Option<String>) -> (Option<String>, String) {
        let Some(raw) = raw.filter(|h| !h.is_empty()) else {
            return (None, String::new());
        };
        if !raw.contains("{{") {
            return (clean_host(raw), String::new());
        }
        match self.vars.resolve_quiet(&raw) {
            Some(resolved) => {
                let (host, path, _) = split_raw_url(&resolved);
                (host, path.trim_end_matches('/').to_string())
            }
            None => {
                let mut rest = raw.as_str();
                while let Some(start) = rest.find("{{") {
                    let after = &rest[start + 2..];
                    let Some(end) = after.find("}}") else { break };
                    let name = after[..end].trim();
                    if self.vars.value(name).is_none() {
                        let shown: String = name.chars().take(MAX_VARIABLE_NAME).collect();
                        name_once(&mut self.host_vars, shown);
                    }
                    rest = &after[end + 2..];
                }
                (None, String::new())
            }
        }
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
                    let given;
                    let seg = match seg.strip_prefix(':') {
                        Some(name) if !name.is_empty() => {
                            given = self.vars.given(&format!(":{name}"));
                            match (given.as_deref(), path_vars.get(name)) {
                                (Some(value), _) => value,
                                (None, Some(value)) => value,
                                (None, None) => {
                                    let shown: String =
                                        name.chars().take(MAX_VARIABLE_NAME - 1).collect();
                                    name_once(&mut self.vars.missing, format!(":{shown}"));
                                    self.vars.incomplete = true;
                                    ""
                                }
                            }
                        }
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
                };
                (host, path, None)
            }
            (None, Some(raw)) => split_raw(raw),
            (None, None) => return Err("not an http request"),
        };
        let (host, lead) = self.host(host);
        let path = match (lead.is_empty(), path.as_str()) {
            (true, _) => path,
            (false, "/") => lead,
            (false, _) => format!("{lead}{path}"),
        };
        if url["protocol"]
            .as_str()
            .is_some_and(|p| !matches!(p, "http" | "https"))
        {
            return Err("not an http request");
        }
        entry.host = host;
        entry.path = self.vars.put(&path);
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
                self.vars.query(pairs).unwrap_or_default()
            }
            None => match raw_query {
                Some(q) => {
                    let pairs: Vec<(String, String)> = form_urlencoded::parse(q.as_bytes())
                        .map(|(k, v)| (k.into_owned(), v.into_owned()))
                        .collect();
                    self.vars.query(pairs).unwrap_or_default()
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
                entry.body = Some(self.vars.put(text));
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
                        resolved.push((k, self.vars.put(&v)));
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
                doc.insert("query".into(), Value::String(self.vars.put(query)));
                if let Some(text) = body["graphql"]["variables"].as_str() {
                    let text = self.vars.put(text);
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

fn read_postman(doc: &Value, given: &Variables, out: &mut Imported) -> Result<Vec<Read>, String> {
    let schema = doc["info"]["schema"].as_str().unwrap_or("");
    if !schema.contains("/v2.1") {
        return Err(
            "only Postman collections v2.1 are imported: export the collection as Collection v2.1"
                .into(),
        );
    }
    let mut vars = Vars::with(given);
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
        host_vars: Vec::new(),
    };
    walker.note_item(doc, "collection");
    let items = doc["item"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    walker.walk(items, "", 0);
    let Postman {
        vars,
        entries,
        host_vars,
        ..
    } = walker;
    if !host_vars.is_empty() {
        out.notes.push(format!(
            "hosts: a variable in the place of a host has no value and is dropped with the host ({}): give it in variables when it holds a path that leads the requests",
            host_vars.join(", ")
        ));
    }
    vars.finish(out);
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
                            // The caller's value stands for the list.
                            Value::Array(_) if self.vars.given.contains_key(name) => {}
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
        // Read to the end even when a variable has no value, so the answer
        // names every one the request needs; it is left out all the same.
        self.vars.begin();
        let head = self.vars.put(head);
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
            self.vars.query(from_url).unwrap_or_default(),
            self.vars.query(from_qs).unwrap_or_default(),
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
        entry.headers = self.vars.resolve_pairs(headers).unwrap_or_default();
        // Credentials a step carries outside its headers: removed, named.
        if spec.contains_key("auth") {
            name_once(&mut self.out.removed, "authorization");
        }
        if spec.contains_key("cookie") {
            name_once(&mut self.out.removed, "cookie");
        }
        if let Some(v) = spec.get("json") {
            let resolved = self.vars.resolve_value(v).unwrap_or_default();
            entry.body = Some(resolved.to_string());
            if !has_header(&entry.headers, "content-type") {
                entry
                    .headers
                    .push(("content-type".into(), "application/json".into()));
            }
        } else if let Some(text) = spec.get("body").and_then(scalar) {
            entry.body = Some(self.vars.put(&text));
        } else if let Some(form) = spec.get("form").and_then(Value::as_object) {
            let mut pairs = Vec::with_capacity(form.len());
            for (k, v) in form {
                let v = scalar(v).unwrap_or_default();
                if looks_secret(k) {
                    pairs.push((k.clone(), String::new()));
                } else {
                    pairs.push((k.clone(), self.vars.put(&v)));
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
        self.vars.whole()?;
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

fn read_artillery(
    doc: &Value,
    given: &Variables,
    out: &mut Imported,
) -> Result<(Vec<Read>, Phases), String> {
    let Some(scenarios) = doc["scenarios"].as_array() else {
        return Err("an Artillery script needs scenarios with a flow each".into());
    };
    for key in doc.as_object().into_iter().flat_map(|m| m.keys()) {
        if !matches!(key.as_str(), "config" | "scenarios") {
            name_once(&mut out.left_out, key.clone());
        }
    }
    let mut reader = Artillery {
        vars: Vars::with(given),
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
    vars.finish(out);
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
    if crate::replay::arazzo::is_arazzo(doc) {
        return Ok(Format::Arazzo);
    }
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
        "not a document this import reads: expected a HAR file (log.entries), the answer of GET /_rustybin/requests, a Postman collection v2.1, an Artillery script or an Arazzo document (formats: {})",
        Format::ALL.join(", ")
    ))
}

/// Turn a recording into request templates ([`import_as`], the format read
/// from the document).
pub fn import(doc: &Value) -> Result<Imported, String> {
    import_as(doc, None)
}

/// Turn a document into request templates ([`import_with`], without the
/// instance's mocks: an Arazzo document finds no source description).
pub fn import_as(doc: &Value, format: Option<Format>) -> Result<Imported, String> {
    import_with(doc, format, &[])
}

/// The steps of an Arazzo document's workflows as entries. A step that
/// needs a value the document does not give (a workflow input, an earlier
/// step's output) is left out and the value named: request templates are
/// sent by weight, not as a journey, so nothing can be passed on.
///
/// A workflow input is a variable (`{{NAME}}`, as `variables` names it), so
/// the caller may give its value. An earlier step's output is made by a
/// run and is never given: a scenario has no journeys.
fn read_arazzo(
    doc: &Value,
    mocks: &[crate::replay::arazzo::Mock],
    given: &Variables,
    out: &mut Imported,
) -> Result<Vec<Read>, String> {
    if let Some(name) = given.names().find(|n| n.starts_with("steps.")) {
        return Err(format!(
            "variables.{name}: the output of a step is made by a run and is never given (run the workflow as a saved test for the journey)"
        ));
    }
    let imported = crate::replay::arazzo::import(doc, mocks)?;
    for l in imported.left_out {
        name_once(&mut out.left_out, l);
    }
    let used = std::cell::RefCell::new(BTreeSet::new());
    // A given input put in; any other reference stays as it is written.
    let fill = |text: &str| -> String {
        crate::replay::run::fill(text, &|name| {
            Some(match given.values.get(name) {
                Some(value) => {
                    used.borrow_mut().insert(name.to_string());
                    value.clone()
                }
                None => format!("{{{{{name}}}}}"),
            })
        })
        .unwrap_or_else(|_| text.to_string())
    };
    let mut entries = Vec::new();
    for t in &imported.tests {
        for step in &t.steps {
            let mut r = step.request.clone();
            if !given.values.is_empty() {
                r.path = fill(&r.path);
                for (name, value) in r.headers.iter_mut() {
                    // A secret-looking header is removed by name later,
                    // whatever it holds: nothing is put in it.
                    if !looks_secret(name) {
                        *value = fill(value);
                    }
                }
                r.body = r.body.as_deref().map(fill);
            }
            let texts = std::iter::once(r.path.as_str())
                .chain(
                    r.headers
                        .iter()
                        .filter(|(name, _)| !looks_secret(name))
                        .map(|(_, value)| value.as_str()),
                )
                .chain(r.body.as_deref());
            let mut needs = false;
            for text in texts {
                for v in crate::replay::run::variables_in(text) {
                    needs = true;
                    name_once(&mut out.variables, v);
                }
                for (id, name) in crate::replay::run::step_refs(text) {
                    needs = true;
                    name_once(&mut out.variables, format!("steps.{id}.outputs.{name}"));
                }
            }
            if needs {
                entries.push(Err(
                    "a value the document does not give (a workflow input, or an earlier step's output)",
                ));
                continue;
            }
            let (path, query) = match r.path.split_once('?') {
                Some((p, q)) => (p.to_string(), Some(q.to_string())),
                None => (r.path.clone(), None),
            };
            entries.push(Ok(Entry {
                method: r.method.clone(),
                path,
                query,
                headers: r
                    .headers
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                body: r.body.clone(),
                expect: step.assert.iter().filter_map(|a| a.status).collect(),
                weight: 1,
                ..Entry::default()
            }));
        }
    }
    let used = used.into_inner();
    out.notes
        .extend(fixed_placeholders(used.iter().map(String::as_str)));
    let unused: Vec<&str> = given
        .names()
        .filter(|n| !used.contains(*n) && !given.from_environment.contains(*n))
        .collect();
    if !unused.is_empty() {
        out.notes.push(format!(
            "variables: given and not used by the document: {}",
            unused.join(", ")
        ));
    }
    if !out.variables.is_empty() {
        out.notes.push(
            "arazzo: a scenario's requests are sent by weight, not in the order of a workflow: steps that need an input or an earlier step's output are left out (run the workflow as a saved test for the journey)".into(),
        );
    }
    Ok(entries)
}

/// Turn a document into request templates. Identical requests become one
/// template with their count as its weight, so the mix is the document's.
/// `mocks`: the instance's mocks an Arazzo document's source descriptions
/// may be.
pub fn import_with(
    doc: &Value,
    format: Option<Format>,
    mocks: &[crate::replay::arazzo::Mock],
) -> Result<Imported, String> {
    import_given(doc, format, mocks, &Variables::default())
}

/// [`import_with`], with the values the caller gives for variables of the
/// document. A given value wins over the document's own. Everything after
/// the reading is the same: credentials are removed by name, and a request
/// is kept only when a scenario would take it (`fits`).
pub fn import_given(
    doc: &Value,
    format: Option<Format>,
    mocks: &[crate::replay::arazzo::Mock],
    given: &Variables,
) -> Result<Imported, String> {
    import_doc(doc, format, mocks, given).map_err(|r| r.error)
}

/// [`import_given`], its refusal with the variables that had no value.
fn import_doc(
    doc: &Value,
    format: Option<Format>,
    mocks: &[crate::replay::arazzo::Mock],
    given: &Variables,
) -> Result<Imported, Refusal> {
    let format = match format {
        Some(f) => f,
        None => detect(doc)?,
    };
    if format.recording() && !given.is_empty() {
        return Err(format!(
            "{}: a recording ({}) has no variables",
            if given.values.is_empty() {
                "environment"
            } else {
                "variables"
            },
            format.as_str()
        )
        .into());
    }
    let mut out = Imported {
        format,
        // What an environment file held that was not taken, by name.
        notes: given.notes.clone(),
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
        Format::Postman => read_postman(doc, given, &mut out)?,
        Format::Arazzo => read_arazzo(doc, mocks, given, &mut out)?,
        Format::Artillery => {
            let (entries, p) = read_artillery(doc, given, &mut out)?;
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
        let error = if format.recording() {
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
        };
        return Err(Refusal {
            error,
            variables: out.variables,
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
    import_text_with(text, format, &[])
}

/// [`import_text`] with the instance's mocks, which an Arazzo document's
/// source descriptions may be.
pub fn import_text_with(
    text: &str,
    format: Option<Format>,
    mocks: &[crate::replay::arazzo::Mock],
) -> Result<Imported, String> {
    import_text_given(text, format, mocks, &Variables::default())
}

/// [`import_text_with`], with the values the caller gives for variables
/// ([`import_given`]).
pub fn import_text_given(
    text: &str,
    format: Option<Format>,
    mocks: &[crate::replay::arazzo::Mock],
    given: &Variables,
) -> Result<Imported, String> {
    import_text_doc(text, format, mocks, given).map_err(|r| r.error)
}

/// [`import_text_given`], its refusal with the variables that had no value.
fn import_text_doc(
    text: &str,
    format: Option<Format>,
    mocks: &[crate::replay::arazzo::Mock],
    given: &Variables,
) -> Result<Imported, Refusal> {
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
            return import_doc(&doc, format, mocks, given);
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
        )
        .into());
    }
    match serde_yaml::from_str::<Value>(text) {
        // A mapping that is none of the formats is as likely a program
        // that happens to read as YAML (a line ending in a colon).
        Ok(doc) if doc.is_object() && (format.is_some() || detect(&doc).is_ok()) => {
            import_doc(&doc, format, mocks, given)
        }
        _ => Err(refused().into()),
    }
}

/// The keys of an import's envelope.
const ENVELOPE_KEYS: &[&str] = &["document", "format", "variables", "environment"];

/// A format by its name, or the refusal that lists them.
pub fn format_named(name: Option<&str>) -> Result<Option<Format>, String> {
    match name.map(str::trim).filter(|f| !f.is_empty()) {
        None => Ok(None),
        Some(name) => Format::parse(name)
            .map(Some)
            .ok_or_else(|| format!("format: one of {}", Format::ALL.join(", "))),
    }
}

/// Turn the body of an import request into request templates: the document
/// bare ([`import_text_given`]), or a JSON envelope `{document, format,
/// variables, environment}` whose `document` is the document's text or the
/// document itself and whose `environment` is a Postman environment file
/// read as more variables ([`Variables::with_environment`]). `format` is the one the request names beside the body; the
/// envelope's own wins. A refusal says which variables had no value.
pub fn import_body(
    body: &str,
    format: Option<&str>,
    mocks: &[crate::replay::arazzo::Mock],
) -> Result<Imported, Refusal> {
    // An envelope is a JSON object with `document` that is none of the
    // formats itself. Only a body that could be one is parsed for it.
    let envelope = (body.trim_start().starts_with('{') && body.contains("\"document\""))
        .then(|| serde_json::from_str::<Value>(body).ok())
        .flatten()
        .and_then(|v| match v {
            Value::Object(map) if map.contains_key("document") => Some(map),
            _ => None,
        })
        .filter(|map| {
            map.keys().all(|k| ENVELOPE_KEYS.contains(&k.as_str()))
                || detect(&Value::Object(map.clone())).is_err()
        });
    let Some(mut envelope) = envelope else {
        return import_text_doc(body, format_named(format)?, mocks, &Variables::default());
    };
    if let Some(other) = envelope
        .keys()
        .find(|k| !ENVELOPE_KEYS.contains(&k.as_str()))
    {
        let shown: String = other.chars().take(64).collect();
        return Err(format!(
            "{shown}: an import takes document, format, variables and environment"
        )
        .into());
    }
    let format = match envelope.get("format") {
        None | Some(Value::Null) => format_named(format)?,
        Some(Value::String(name)) => format_named(Some(name))?,
        Some(_) => return Err(format!("format: one of {}", Format::ALL.join(", ")).into()),
    };
    let given = Variables::with_environment(
        envelope.get("variables").unwrap_or(&Value::Null),
        envelope.get("environment").unwrap_or(&Value::Null),
    )?;
    match envelope.remove("document") {
        Some(Value::String(text)) => import_text_doc(&text, format, mocks, &given),
        Some(doc @ (Value::Object(_) | Value::Array(_))) => {
            let read_as = match format {
                Some(f) => f,
                None => detect(&doc)?,
            };
            if !read_as.recording() && body.len() > MAX_TEXT_BYTES {
                return Err(format!(
                    "a collection or a script is at most {} MiB",
                    MAX_TEXT_BYTES / (1024 * 1024)
                )
                .into());
            }
            import_doc(&doc, format, mocks, &given)
        }
        _ => Err("document: the document, or its text".into()),
    }
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod tests;
