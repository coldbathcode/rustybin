//! AsyncAPI documents (2.x and 3.0) as event mocks: the channels, the
//! messages Rustybin publishes on each and the ones it accepts.
//!
//! An AsyncAPI document describes an application. Rustybin plays that
//! application: a `send` operation (3.0), or a `subscribe` one (2.x: what a
//! client may subscribe to), is a message Rustybin publishes; a `receive`
//! operation (3.0), or a `publish` one (2.x), is a message a client
//! publishes and Rustybin validates. Both versions parse into one model.
//!
//! Payloads are generated and validated by [`super::schema`], the generator
//! of the OpenAPI mocks. A schema in another format (Avro, Protobuf) is
//! named in the deploy report and never generated or validated.
//! [`crate::events`] serves the channels.

use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::Arc;

use super::schema::{self, Direction, GenOptions, Issue};
use super::spec::{self, ParseLimits, ParsedSpec};

/// Most messages of one channel in one direction.
pub const MAX_MESSAGES: usize = 50;
/// Most examples kept of one message.
pub const MAX_EXAMPLES: usize = 20;
/// Most headers sent with one message.
pub const MAX_HEADERS: usize = 20;
/// The bindings Rustybin serves a channel over.
pub const SERVED_BINDINGS: [&str; 2] = ["ws", "http"];
/// The server protocols those bindings cover.
const SERVED_PROTOCOLS: [&str; 4] = ["ws", "wss", "http", "https"];

/// An AsyncAPI document's channels.
#[derive(Clone, Debug, Serialize)]
pub struct EventSpec {
    /// The document's `asyncapi` version.
    pub asyncapi: String,
    pub channels: Vec<Channel>,
    /// The protocols of the document's servers.
    pub protocols: Vec<String>,
}

impl EventSpec {
    pub fn channel(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Channel {
    /// The name in URLs (`/events/{mock}/{id}`): the address as a slug.
    pub id: String,
    /// The channel's address in the document (`orders/created`).
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What Rustybin publishes.
    pub send: Vec<Message>,
    /// What a client may publish.
    pub receive: Vec<Message>,
    /// The bindings the document names for the channel.
    pub bindings: Vec<String>,
    /// The document declares a reply to what the channel receives.
    pub reply: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Message {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// The payload schema (JSON Schema, `$ref`s into the document).
    #[serde(skip)]
    pub payload: Option<Value>,
    #[serde(skip)]
    pub headers: Option<Value>,
    #[serde(skip)]
    pub examples: Vec<MessageExample>,
    /// A schema format Rustybin neither generates nor validates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsupported_format: Option<String>,
}

#[derive(Clone, Debug)]
pub struct MessageExample {
    pub payload: Value,
    pub headers: Option<Value>,
}

impl Message {
    /// The schema to generate from and validate against, if any.
    fn schema(&self) -> Option<&Value> {
        if self.unsupported_format.is_some() {
            return None;
        }
        self.payload.as_ref()
    }
}

/// Is this document AsyncAPI?
pub fn is_asyncapi(root: &Value) -> bool {
    root.get("asyncapi").is_some()
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// The last token of a JSON pointer reference, unescaped.
fn last_token(reference: &str) -> String {
    reference
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .replace("~1", "/")
        .replace("~0", "~")
}

/// The channel key a `#/channels/<key>` reference names.
fn channel_key(reference: &str) -> Option<String> {
    let rest = reference.strip_prefix("#/channels/")?;
    if rest.contains('/') {
        return None;
    }
    Some(rest.replace("~1", "/").replace("~0", "~"))
}

/// Does a schema format name something [`super::schema`] reads?
fn supported_format(format: &str) -> bool {
    let f = format.to_ascii_lowercase();
    f.contains("vnd.aai.asyncapi")
        || f.contains("schema+json")
        || f.contains("schema+yaml")
        || f.contains("vnd.oai.openapi")
}

fn parse_message(root: &Value, raw: &Value, fallback: &str, warnings: &mut Vec<String>) -> Message {
    let m = match spec::resolve(root, raw) {
        Ok(m) => m,
        Err(e) => {
            warnings.push(format!("message {fallback}: {e}"));
            &Value::Null
        }
    };
    let mut format = str_of(m, "schemaFormat");
    let mut payload = m.get("payload").cloned();
    // 3.0's multi format schema: `{schemaFormat, schema}`.
    if let Some(p) = &payload {
        if let (Some(f), Some(inner)) = (
            p.get("schemaFormat").and_then(Value::as_str),
            p.get("schema"),
        ) {
            format = Some(f.to_string());
            payload = Some(inner.clone());
        }
    }
    let examples = m
        .get("examples")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|e| {
                    Some(MessageExample {
                        payload: e.get("payload")?.clone(),
                        headers: e.get("headers").cloned(),
                    })
                })
                .take(MAX_EXAMPLES)
                .collect()
        })
        .unwrap_or_default();
    Message {
        name: str_of(m, "name")
            .or_else(|| str_of(m, "messageId"))
            .unwrap_or_else(|| fallback.to_string()),
        title: str_of(m, "title").or_else(|| str_of(m, "summary")),
        content_type: str_of(m, "contentType"),
        payload,
        headers: m.get("headers").cloned(),
        examples,
        unsupported_format: format.filter(|f| !supported_format(f)),
    }
}

fn push_message(list: &mut Vec<Message>, m: Message) {
    if list.len() < MAX_MESSAGES && !list.iter().any(|x| x.name == m.name) {
        list.push(m);
    }
}

fn binding_names(item: &Value, root: &Value) -> Vec<String> {
    item.get("bindings")
        .and_then(|b| spec::resolve(root, b).ok())
        .and_then(Value::as_object)
        .map(|b| b.keys().cloned().collect())
        .unwrap_or_default()
}

/// A channel id nobody has yet: the address as a slug, numbered on a clash.
fn unique_id(address: &str, taken: &mut HashSet<String>) -> String {
    let base = spec::slugify(address);
    let mut id = base.clone();
    let mut n = 2;
    while !taken.insert(id.clone()) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

/// AsyncAPI 2.x: a channel item's `subscribe` is what the application
/// sends, its `publish` what it receives.
fn channels_v2(root: &Value, limits: ParseLimits, warnings: &mut Vec<String>) -> Vec<Channel> {
    let mut out = Vec::new();
    let mut taken = HashSet::new();
    let Some(channels) = root.get("channels").and_then(Value::as_object) else {
        return out;
    };
    for (address, item) in channels {
        if out.len() >= limits.max_operations {
            warnings.push(format!(
                "more than {} channels: the rest are left out",
                limits.max_operations
            ));
            break;
        }
        let item = match spec::resolve(root, item) {
            Ok(i) => i,
            Err(e) => {
                warnings.push(format!("channel {address}: {e}"));
                continue;
            }
        };
        let mut ch = Channel {
            id: unique_id(address, &mut taken),
            address: address.clone(),
            description: str_of(item, "description"),
            send: Vec::new(),
            receive: Vec::new(),
            bindings: binding_names(item, root),
            reply: false,
        };
        for (key, sends) in [("subscribe", true), ("publish", false)] {
            let Some(op) = item.get(key) else { continue };
            let fallback = str_of(op, "operationId").unwrap_or_else(|| format!("{}-{key}", ch.id));
            let Some(message) = op.get("message") else {
                continue;
            };
            let message = spec::resolve(root, message).unwrap_or(message);
            let list = if sends { &mut ch.send } else { &mut ch.receive };
            match message.get("oneOf").and_then(Value::as_array) {
                Some(several) => {
                    for (i, m) in several.iter().enumerate() {
                        let name = format!("{fallback}-{}", i + 1);
                        push_message(list, parse_message(root, m, &name, warnings));
                    }
                }
                None => push_message(list, parse_message(root, message, &fallback, warnings)),
            }
        }
        out.push(ch);
    }
    out
}

/// AsyncAPI 3.0: channels hold the messages, operations say which way they
/// go.
fn channels_v3(root: &Value, limits: ParseLimits, warnings: &mut Vec<String>) -> Vec<Channel> {
    let mut out: Vec<Channel> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    // Each channel's messages by their key in the channel.
    let mut messages: Vec<Vec<(String, Message)>> = Vec::new();
    let mut taken = HashSet::new();
    if let Some(channels) = root.get("channels").and_then(Value::as_object) {
        for (key, item) in channels {
            if out.len() >= limits.max_operations {
                warnings.push(format!(
                    "more than {} channels: the rest are left out",
                    limits.max_operations
                ));
                break;
            }
            let item = match spec::resolve(root, item) {
                Ok(i) => i,
                Err(e) => {
                    warnings.push(format!("channel {key}: {e}"));
                    continue;
                }
            };
            let address = str_of(item, "address").unwrap_or_else(|| key.clone());
            let own: Vec<(String, Message)> = item
                .get("messages")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .take(MAX_MESSAGES)
                        .map(|(k, v)| (k.clone(), parse_message(root, v, k, warnings)))
                        .collect()
                })
                .unwrap_or_default();
            out.push(Channel {
                id: unique_id(&address, &mut taken),
                address,
                description: str_of(item, "description"),
                send: Vec::new(),
                receive: Vec::new(),
                bindings: binding_names(item, root),
                reply: false,
            });
            keys.push(key.clone());
            messages.push(own);
        }
    }
    let index_of = |op: &Value| -> Option<usize> {
        let reference = op.get("channel")?.get("$ref")?.as_str()?;
        let key = channel_key(reference)?;
        keys.iter().position(|k| *k == key)
    };
    // The messages an operation (or its reply) names on channel `i`: the
    // listed ones, else all of the channel's.
    let named = |i: usize, holder: &Value, warnings: &mut Vec<String>| -> Vec<Message> {
        let own = &messages[i];
        match holder.get("messages").and_then(Value::as_array) {
            Some(list) => list
                .iter()
                .filter_map(|m| {
                    let reference = m.get("$ref").and_then(Value::as_str)?;
                    let key = last_token(reference);
                    match own.iter().find(|(k, _)| *k == key) {
                        Some((_, found)) => Some(found.clone()),
                        // A reference past the channel (the components).
                        None => Some(parse_message(root, m, &key, warnings)),
                    }
                })
                .collect(),
            None => own.iter().map(|(_, m)| m.clone()).collect(),
        }
    };
    let mut used = vec![false; out.len()];
    if let Some(operations) = root.get("operations").and_then(Value::as_object) {
        for (id, op) in operations {
            let op = spec::resolve(root, op).unwrap_or(op);
            let Some(i) = index_of(op) else {
                warnings.push(format!(
                    "operation {id}: its channel is not one of the document's channels, skipped"
                ));
                continue;
            };
            used[i] = true;
            let sends = match op.get("action").and_then(Value::as_str) {
                Some("send") => true,
                Some("receive") => false,
                other => {
                    warnings.push(format!(
                        "operation {id}: action {other:?} is not send or receive, skipped"
                    ));
                    continue;
                }
            };
            for m in named(i, op, warnings) {
                let ch = &mut out[i];
                push_message(if sends { &mut ch.send } else { &mut ch.receive }, m);
            }
            if let Some(reply) = op.get("reply").filter(|_| !sends) {
                let reply = spec::resolve(root, reply).unwrap_or(reply);
                let to = index_of(reply).unwrap_or(i);
                used[to] = true;
                for m in named(to, reply, warnings) {
                    push_message(&mut out[to].send, m);
                }
                out[i].reply = true;
            }
        }
    }
    for (i, ch) in out.iter_mut().enumerate() {
        if !used[i] && !messages[i].is_empty() {
            warnings.push(format!(
                "channel {}: no operation names it, so its messages are published",
                ch.address
            ));
            for (_, m) in &messages[i] {
                push_message(&mut ch.send, m.clone());
            }
        }
    }
    out
}

/// Parse an AsyncAPI document into a spec with channels and no operations.
pub fn parse(root: Value, limits: ParseLimits) -> Result<ParsedSpec, String> {
    let version = match root.get("asyncapi") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Err("\"asyncapi\" must be a version (2.x or 3.0.x)".into()),
    };
    let three = version.starts_with("3.");
    if !three && !version.starts_with("2.") {
        return Err(format!(
            "unsupported AsyncAPI version {version:?} (expected 2.x or 3.0.x)"
        ));
    }
    let mut warnings = Vec::new();
    if three && !version.starts_with("3.0") {
        warnings.push(format!(
            "AsyncAPI {version} is newer than 3.0; parsed as 3.0"
        ));
    }
    let mut refs = Vec::new();
    spec::collect_refs(&root, &mut refs, 0);
    if let Some(ext) = refs.iter().find(|r| !r.starts_with('#')) {
        return Err(format!(
            "external $ref {ext:?} is not supported: bundle the document into one file first"
        ));
    }
    let mut unresolved: Vec<&str> = refs
        .iter()
        .copied()
        .filter(|r| spec::resolve_pointer(&root, r).is_err())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if !unresolved.is_empty() {
        unresolved.sort_unstable();
        let listed: Vec<&str> = unresolved
            .iter()
            .copied()
            .take(spec::MAX_LISTED_REFS)
            .collect();
        warnings.push(format!(
            "{} unresolved $ref(s): {}",
            unresolved.len(),
            listed.join(", ")
        ));
    }
    match root.get("channels") {
        None | Some(Value::Object(_)) => {}
        Some(_) => return Err("\"channels\" must be an object".into()),
    }
    let channels = if three {
        channels_v3(&root, limits, &mut warnings)
    } else {
        channels_v2(&root, limits, &mut warnings)
    };
    let mut protocols: Vec<String> = root
        .get("servers")
        .and_then(Value::as_object)
        .map(|servers| {
            servers
                .values()
                .filter_map(|s| spec::resolve(&root, s).ok())
                .filter_map(|s| str_of(s, "protocol"))
                .map(|p| p.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    protocols.sort();
    protocols.dedup();
    protocols.truncate(20);
    let info = root.get("info").cloned().unwrap_or(Value::Null);
    let tags = info
        .get("tags")
        .or_else(|| root.get("tags"))
        .and_then(Value::as_array)
        .map(|t| t.iter().filter_map(|t| str_of(t, "name")).collect())
        .unwrap_or_default();
    Ok(ParsedSpec {
        openapi: String::new(),
        title: str_of(&info, "title").unwrap_or_else(|| "Untitled API".into()),
        version: str_of(&info, "version").unwrap_or_default(),
        description: str_of(&info, "description"),
        server_prefix: String::new(),
        tags,
        operations: Vec::new(),
        security_schemes: Vec::new(),
        warnings,
        events: Some(Arc::new(EventSpec {
            asyncapi: version,
            channels,
            protocols,
        })),
        root,
    })
}

// ── Messages ────────────────────────────────────────────────────────

/// A message Rustybin publishes.
#[derive(Clone, Debug)]
pub struct Outgoing {
    pub message: String,
    pub payload: Value,
    /// `example` (the document's), `stored` (the mock's) or `generated`.
    pub source: &'static str,
    pub headers: Vec<(String, String)>,
}

fn mix(seed: u64, id: &str, n: u64) -> u64 {
    let mut h = seed ^ 0x9e37_79b9_7f4a_7c15;
    for b in id.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    (h ^ n).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// A header map as pairs a transport can send (names and values that are
/// valid headers, strings and numbers only).
fn header_pairs(v: &Value) -> Vec<(String, String)> {
    let Some(map) = v.as_object() else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(k, v)| {
            let value = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => return None,
            };
            let ok = axum::http::HeaderName::from_bytes(k.as_bytes()).is_ok()
                && axum::http::HeaderValue::from_str(&value).is_ok()
                && value.len() <= 1024;
            ok.then(|| (k.clone(), value))
        })
        .take(MAX_HEADERS)
        .collect()
}

/// The `n`th message (from 1) Rustybin publishes on a channel: its send
/// messages in turn; of each, the document's examples, then the mock's
/// stored ones, then a payload generated from the schema. The same seed
/// and `n` give the same message.
pub fn outgoing(
    root: &Value,
    ch: &Channel,
    stored: &[Value],
    seed: u64,
    n: u64,
) -> Option<Outgoing> {
    if ch.send.is_empty() {
        return None;
    }
    let at = n.saturating_sub(1);
    let len = ch.send.len() as u64;
    let m = &ch.send[(at % len) as usize];
    let round = at / len;
    let seed = mix(seed, &ch.id, n);
    let generated_headers = || {
        m.headers.as_ref().map_or_else(Vec::new, |h| {
            let opts = GenOptions {
                root,
                direction: Direction::Response,
                seed,
                use_examples: true,
                echo: &[],
            };
            header_pairs(&schema::generate(h, &opts))
        })
    };
    let (payload, source, headers) = if !m.examples.is_empty() {
        let e = &m.examples[(round % m.examples.len() as u64) as usize];
        let headers = e
            .headers
            .as_ref()
            .map_or_else(generated_headers, header_pairs);
        (e.payload.clone(), "example", headers)
    } else if !stored.is_empty() {
        let e = &stored[(at % stored.len() as u64) as usize];
        (e.clone(), "stored", generated_headers())
    } else {
        let payload = match m.schema() {
            Some(s) => {
                let opts = GenOptions {
                    root,
                    direction: Direction::Response,
                    seed,
                    use_examples: true,
                    echo: &[],
                };
                schema::generate(s, &opts)
            }
            None => json!({}),
        };
        (payload, "generated", generated_headers())
    };
    Some(Outgoing {
        message: m.name.clone(),
        payload,
        source,
        headers,
    })
}

/// A valid message a client could publish on a channel (its first receive
/// message), for the console's composer and the deploy report.
pub fn sample_incoming(root: &Value, ch: &Channel, seed: u64) -> Option<(String, Value)> {
    let m = ch.receive.first()?;
    if let Some(e) = m.examples.first() {
        return Some((m.name.clone(), e.payload.clone()));
    }
    let payload = match m.schema() {
        Some(s) => {
            let opts = GenOptions {
                root,
                direction: Direction::Request,
                seed: mix(seed, &ch.id, 0),
                use_examples: true,
                echo: &[],
            };
            schema::generate(s, &opts)
        }
        None => json!({}),
    };
    Some((m.name.clone(), payload))
}

/// What Rustybin makes of a message a client published.
#[derive(Clone, Debug)]
pub struct Verdict {
    /// The receive message it is valid as.
    pub message: Option<String>,
    /// Why it is none of them (empty: valid).
    pub issues: Vec<Issue>,
}

impl Verdict {
    pub fn valid(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Check a published message against the channel's receive messages: it
/// is valid as the first one whose payload schema it meets.
pub fn check_incoming(root: &Value, ch: &Channel, value: &Value) -> Verdict {
    if ch.receive.is_empty() {
        return Verdict {
            message: None,
            issues: vec![Issue::new(
                "",
                "this channel accepts no messages (the document has no operation that receives on it)",
            )],
        };
    }
    let mut first: Option<Vec<Issue>> = None;
    for m in &ch.receive {
        let issues = match m.schema() {
            Some(s) => schema::validate(value, s, root, Direction::Request),
            None => Vec::new(),
        };
        if issues.is_empty() {
            return Verdict {
                message: Some(m.name.clone()),
                issues,
            };
        }
        first.get_or_insert(issues);
    }
    Verdict {
        message: None,
        issues: first.unwrap_or_default(),
    }
}

/// What a channel's parsed model keeps beyond the document, as JSON values
/// (for [`super::compile::Compiled`]'s memory estimate).
pub fn kept_values(events: &EventSpec) -> impl Iterator<Item = &Value> {
    events.channels.iter().flat_map(|c| {
        c.send.iter().chain(c.receive.iter()).flat_map(|m| {
            m.payload
                .iter()
                .chain(m.headers.iter())
                .chain(m.examples.iter().map(|e| &e.payload))
                .chain(m.examples.iter().filter_map(|e| e.headers.as_ref()))
        })
    })
}

/// The bindings and protocols of a document Rustybin does not serve.
pub fn not_served(events: &EventSpec) -> Vec<String> {
    let mut out: Vec<String> = events
        .channels
        .iter()
        .flat_map(|c| c.bindings.iter())
        .filter(|b| !SERVED_BINDINGS.contains(&b.as_str()))
        .chain(
            events
                .protocols
                .iter()
                .filter(|p| !SERVED_PROTOCOLS.contains(&p.as_str())),
        )
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A channel as the control API answers it (no schemas).
pub fn channel_json(ch: &Channel) -> Value {
    let names = |list: &[Message]| -> Vec<Value> {
        list.iter()
            .map(|m| {
                let mut o = Map::new();
                o.insert("name".into(), json!(m.name));
                if let Some(t) = &m.title {
                    o.insert("title".into(), json!(t));
                }
                if let Some(c) = &m.content_type {
                    o.insert("content_type".into(), json!(c));
                }
                o.insert("examples".into(), json!(m.examples.len()));
                if let Some(f) = &m.unsupported_format {
                    o.insert("unsupported_format".into(), json!(f));
                }
                Value::Object(o)
            })
            .collect()
    };
    json!({
        "id": ch.id,
        "address": ch.address,
        "description": ch.description,
        "send": names(&ch.send),
        "receive": names(&ch.receive),
        "bindings": ch.bindings,
        "reply": ch.reply,
    })
}
