//! Serialisation of the canonical model to the three wire shapes and lenient
//! parsing of client input.
//!
//! - v1.0 (`Wire::V1`): ProtoJSON of `lf.a2a.v1`: camelCase, enums as their
//!   proto names (`TASK_STATE_COMPLETED`, `ROLE_USER`), `oneof` members as
//!   discriminators (`{"text": ..}`, `{"statusUpdate": {..}}`), default
//!   values omitted.
//! - v0.3 JSON-RPC (`Wire::V03Rpc`): `kind` discriminators, kebab-case
//!   states (`input-required`), lowercase roles, `file` parts.
//! - v0.3 HTTP+JSON (`Wire::V03Rest`): ProtoJSON of the v0.3 proto
//!   (`content` instead of `parts`, `TASK_STATE_CANCELLED`, `fileWithUri`).

use base64::Engine;
use serde_json::{json, Map, Value};

use super::model::*;

// ── Serialisation ───────────────────────────────────────────────────

fn put_opt(obj: &mut Map<String, Value>, key: &str, v: &Option<String>) {
    if let Some(s) = v {
        obj.insert(key.to_string(), Value::String(s.clone()));
    }
}

fn put_meta(obj: &mut Map<String, Value>, meta: &Meta) {
    if let Some(m) = meta {
        if !m.is_empty() {
            obj.insert("metadata".into(), Value::Object(m.clone()));
        }
    }
}

fn put_strings(obj: &mut Map<String, Value>, key: &str, list: &[String]) {
    if !list.is_empty() {
        obj.insert(key.to_string(), json!(list));
    }
}

/// v0.3 data parts must be objects: other JSON values are wrapped the way
/// the reference SDK does (`{"value": x}` plus `data_part_compat`).
fn v03_data(v: &Value, meta: &Meta) -> (Value, Meta) {
    match v {
        Value::Object(_) => (v.clone(), meta.clone()),
        other => {
            let mut m = meta.clone().unwrap_or_default();
            m.insert("data_part_compat".into(), Value::Bool(true));
            (json!({ "value": other }), Some(m))
        }
    }
}

pub fn part(p: &Part, w: Wire) -> Value {
    let mut o = Map::new();
    match w {
        Wire::V1 => {
            match &p.content {
                PartContent::Text(t) => o.insert("text".into(), json!(t)),
                PartContent::Raw(b) => o.insert("raw".into(), json!(b)),
                PartContent::Url(u) => o.insert("url".into(), json!(u)),
                PartContent::Data(d) => o.insert("data".into(), d.clone()),
            };
            put_meta(&mut o, &p.metadata);
            put_opt(&mut o, "filename", &p.filename);
            put_opt(&mut o, "mediaType", &p.media_type);
        }
        Wire::V03Rpc => match &p.content {
            PartContent::Text(t) => {
                o.insert("kind".into(), json!("text"));
                o.insert("text".into(), json!(t));
                put_meta(&mut o, &p.metadata);
            }
            PartContent::Raw(_) | PartContent::Url(_) => {
                let mut f = Map::new();
                match &p.content {
                    PartContent::Raw(b) => f.insert("bytes".into(), json!(b)),
                    PartContent::Url(u) => f.insert("uri".into(), json!(u)),
                    _ => None,
                };
                put_opt(&mut f, "mimeType", &p.media_type);
                put_opt(&mut f, "name", &p.filename);
                o.insert("kind".into(), json!("file"));
                o.insert("file".into(), Value::Object(f));
                put_meta(&mut o, &p.metadata);
            }
            PartContent::Data(d) => {
                let (d, meta) = v03_data(d, &p.metadata);
                o.insert("kind".into(), json!("data"));
                o.insert("data".into(), d);
                put_meta(&mut o, &meta);
            }
        },
        Wire::V03Rest => match &p.content {
            PartContent::Text(t) => {
                o.insert("text".into(), json!(t));
                put_meta(&mut o, &p.metadata);
            }
            PartContent::Raw(_) | PartContent::Url(_) => {
                let mut f = Map::new();
                match &p.content {
                    // The v0.3 proto carries the base64 *text* in a bytes
                    // field, so ProtoJSON base64-encodes it again (this is
                    // what the reference SDK sends and expects).
                    PartContent::Raw(b) => f.insert(
                        "fileWithBytes".into(),
                        json!(base64::engine::general_purpose::STANDARD.encode(b.as_bytes())),
                    ),
                    PartContent::Url(u) => f.insert("fileWithUri".into(), json!(u)),
                    _ => None,
                };
                put_opt(&mut f, "mimeType", &p.media_type);
                put_opt(&mut f, "name", &p.filename);
                o.insert("file".into(), Value::Object(f));
                put_meta(&mut o, &p.metadata);
            }
            PartContent::Data(d) => {
                let (d, meta) = v03_data(d, &p.metadata);
                o.insert("data".into(), json!({ "data": d }));
                put_meta(&mut o, &meta);
            }
        },
    }
    Value::Object(o)
}

pub fn message(m: &Message, w: Wire) -> Value {
    let mut o = Map::new();
    if w == Wire::V03Rpc {
        o.insert("kind".into(), json!("message"));
    }
    o.insert("messageId".into(), json!(m.message_id));
    put_opt(&mut o, "contextId", &m.context_id);
    put_opt(&mut o, "taskId", &m.task_id);
    o.insert("role".into(), json!(m.role.wire(w)));
    let parts: Vec<Value> = m.parts.iter().map(|p| part(p, w)).collect();
    let key = if w == Wire::V03Rest {
        "content"
    } else {
        "parts"
    };
    o.insert(key.into(), Value::Array(parts));
    put_meta(&mut o, &m.metadata);
    put_strings(&mut o, "extensions", &m.extensions);
    if w != Wire::V03Rest {
        put_strings(&mut o, "referenceTaskIds", &m.reference_task_ids);
    }
    Value::Object(o)
}

pub fn artifact(a: &Artifact, w: Wire) -> Value {
    let mut o = Map::new();
    o.insert("artifactId".into(), json!(a.artifact_id));
    put_opt(&mut o, "name", &a.name);
    put_opt(&mut o, "description", &a.description);
    o.insert(
        "parts".into(),
        Value::Array(a.parts.iter().map(|p| part(p, w)).collect()),
    );
    put_meta(&mut o, &a.metadata);
    put_strings(&mut o, "extensions", &a.extensions);
    Value::Object(o)
}

pub fn status(s: &TaskStatus, w: Wire) -> Value {
    let mut o = Map::new();
    o.insert("state".into(), json!(s.state.wire(w)));
    if let Some(m) = &s.message {
        o.insert("message".into(), message(m, w));
    }
    o.insert("timestamp".into(), json!(s.timestamp));
    Value::Object(o)
}

/// How much of a task to render.
#[derive(Clone, Copy, Debug)]
pub struct TaskView {
    /// `None`: all history; `Some(0)`: no history.
    pub history_length: Option<usize>,
    pub include_artifacts: bool,
}

impl TaskView {
    pub const FULL: TaskView = TaskView {
        history_length: None,
        include_artifacts: true,
    };
}

pub fn task(t: &Task, w: Wire, view: TaskView) -> Value {
    let mut o = Map::new();
    if w == Wire::V03Rpc {
        o.insert("kind".into(), json!("task"));
    }
    o.insert("id".into(), json!(t.id));
    o.insert("contextId".into(), json!(t.context_id));
    o.insert("status".into(), status(&t.status, w));
    if view.include_artifacts && !t.artifacts.is_empty() {
        o.insert(
            "artifacts".into(),
            Value::Array(t.artifacts.iter().map(|a| artifact(a, w)).collect()),
        );
    }
    let skip = match view.history_length {
        Some(n) => t.history.len().saturating_sub(n),
        None => 0,
    };
    let history: Vec<Value> = t.history.iter().skip(skip).map(|m| message(m, w)).collect();
    if !history.is_empty() {
        o.insert("history".into(), Value::Array(history));
    }
    put_meta(&mut o, &t.metadata);
    Value::Object(o)
}

fn status_update(s: &StatusUpdate, w: Wire) -> Value {
    let mut o = Map::new();
    if w == Wire::V03Rpc {
        o.insert("kind".into(), json!("status-update"));
    }
    o.insert("taskId".into(), json!(s.task_id));
    o.insert("contextId".into(), json!(s.context_id));
    o.insert("status".into(), status(&s.status, w));
    if w != Wire::V1 {
        // v0.3 marks the event that ends the stream.
        o.insert("final".into(), json!(s.status.state.ends_stream()));
    }
    Value::Object(o)
}

fn artifact_update(a: &ArtifactUpdate, w: Wire) -> Value {
    let mut o = Map::new();
    if w == Wire::V03Rpc {
        o.insert("kind".into(), json!("artifact-update"));
    }
    o.insert("taskId".into(), json!(a.task_id));
    o.insert("contextId".into(), json!(a.context_id));
    o.insert("artifact".into(), artifact(&a.artifact, w));
    if a.append || w == Wire::V03Rpc {
        o.insert("append".into(), json!(a.append));
    }
    if a.last_chunk || w == Wire::V03Rpc {
        o.insert("lastChunk".into(), json!(a.last_chunk));
    }
    Value::Object(o)
}

/// A `StreamResponse` (v1.0 / v0.3 REST) or the bare v0.3 JSON-RPC result.
pub fn event(ev: &StreamEvent, w: Wire, view: TaskView) -> Value {
    let (key, body) = match ev {
        StreamEvent::Task(t) => ("task", task(t, w, view)),
        StreamEvent::Message(m) => ("message", message(m, w)),
        StreamEvent::Status(s) => ("statusUpdate", status_update(s, w)),
        StreamEvent::Artifact(a) => ("artifactUpdate", artifact_update(a, w)),
    };
    match w {
        Wire::V03Rpc => body,
        _ => json!({ key: body }),
    }
}

/// `SendMessageResponse`: `{"task": ..}` / `{"message": ..}` (v1.0, v0.3
/// REST) or the bare Task / Message (v0.3 JSON-RPC).
pub fn send_result(ev: &StreamEvent, w: Wire, view: TaskView) -> Value {
    event(ev, w, view)
}

pub fn push_config(c: &PushConfig, w: Wire) -> Value {
    match w {
        Wire::V1 => {
            let mut o = Map::new();
            o.insert("id".into(), json!(c.id));
            o.insert("taskId".into(), json!(c.task_id));
            o.insert("url".into(), json!(c.url));
            put_opt(&mut o, "token", &c.token);
            if let Some(a) = &c.authentication {
                let mut auth = Map::new();
                auth.insert("scheme".into(), json!(a.scheme));
                put_opt(&mut auth, "credentials", &a.credentials);
                o.insert("authentication".into(), Value::Object(auth));
            }
            Value::Object(o)
        }
        Wire::V03Rpc | Wire::V03Rest => {
            let mut inner = Map::new();
            inner.insert("id".into(), json!(c.id));
            inner.insert("url".into(), json!(c.url));
            put_opt(&mut inner, "token", &c.token);
            if let Some(a) = &c.authentication {
                let mut auth = Map::new();
                auth.insert("schemes".into(), json!([a.scheme]));
                put_opt(&mut auth, "credentials", &a.credentials);
                inner.insert("authentication".into(), Value::Object(auth));
            }
            if w == Wire::V03Rpc {
                json!({ "taskId": c.task_id, "pushNotificationConfig": inner })
            } else {
                json!({
                    "name": format!("tasks/{}/pushNotificationConfigs/{}", c.task_id, c.id),
                    "pushNotificationConfig": inner,
                })
            }
        }
    }
}

// ── Parsing ─────────────────────────────────────────────────────────

/// Maximum parts accepted in one message.
pub const MAX_PARTS: usize = 100;
/// Maximum length of client supplied identifiers (contextId, ids).
pub const MAX_ID_LEN: usize = 128;

/// First present key among camelCase / snake_case spellings.
pub fn get<'a>(obj: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .find_map(|k| obj.get(*k))
        .filter(|v| !v.is_null())
}

pub fn get_str(obj: &Map<String, Value>, keys: &[&str]) -> Result<Option<String>, String> {
    match get(obj, keys) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("{} must be a string", keys[0])),
    }
}

pub fn get_bool(obj: &Map<String, Value>, keys: &[&str]) -> Result<Option<bool>, String> {
    match get(obj, keys) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(Value::String(s)) if s == "true" || s == "false" => Ok(Some(s == "true")),
        Some(_) => Err(format!("{} must be a boolean", keys[0])),
    }
}

/// Integer field (ProtoJSON also allows numeric strings).
pub fn get_int(obj: &Map<String, Value>, keys: &[&str]) -> Result<Option<i64>, String> {
    match get(obj, keys) {
        None => Ok(None),
        Some(Value::Number(n)) => n
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("{} must be an integer", keys[0])),
        Some(Value::String(s)) => s
            .trim()
            .parse::<i64>()
            .map(Some)
            .map_err(|_| format!("{} must be an integer", keys[0])),
        Some(_) => Err(format!("{} must be an integer", keys[0])),
    }
}

fn get_meta(obj: &Map<String, Value>) -> Result<Meta, String> {
    match obj.get("metadata") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(m)) => Ok(Some(m.clone())),
        Some(_) => Err("metadata must be an object".into()),
    }
}

fn get_string_list(obj: &Map<String, Value>, keys: &[&str]) -> Result<Vec<String>, String> {
    match get(obj, keys) {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("{} must be a list of strings", keys[0]))
            })
            .collect(),
        Some(_) => Err(format!("{} must be a list of strings", keys[0])),
    }
}

/// Validate a client supplied identifier.
pub fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_ID_LEN && s.chars().all(|c| c.is_ascii_graphic())
}

/// Normalise base64 (standard or URL-safe, padded or not) to standard.
fn normalize_base64(s: &str) -> Result<String, String> {
    let cleaned: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let engines = [
        base64::engine::general_purpose::STANDARD,
        base64::engine::general_purpose::STANDARD_NO_PAD,
        base64::engine::general_purpose::URL_SAFE,
        base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ];
    for e in engines {
        if let Ok(bytes) = e.decode(cleaned.as_bytes()) {
            return Ok(base64::engine::general_purpose::STANDARD.encode(bytes));
        }
    }
    Err("raw/bytes must be valid base64".into())
}

/// v0.3 ProtoJSON `fileWithBytes`: base64 of the base64 text (see the
/// serialiser). Plain single base64 is accepted too.
fn unwrap_v03_proto_bytes(s: &str) -> Result<String, String> {
    let once = normalize_base64(s)?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(once.as_bytes())
        .map_err(|_| "fileWithBytes must be valid base64".to_string())?;
    match std::str::from_utf8(&decoded) {
        Ok(inner) if !inner.is_empty() => Ok(normalize_base64(inner).unwrap_or(once)),
        _ => Ok(once),
    }
}

/// Parse a part in any of the three shapes.
pub fn parse_part(v: &Value, w: Wire) -> Result<Part, String> {
    let Value::Object(o) = v else {
        return Err("each part must be an object".into());
    };
    let mut metadata = get_meta(o)?;
    let mut filename = get_str(o, &["filename"])?;
    let mut media_type = get_str(o, &["mediaType", "media_type"])?;

    let file_obj = match o.get("file") {
        Some(Value::Object(f)) => Some(f),
        Some(Value::Null) | None => None,
        Some(_) => return Err("part.file must be an object".into()),
    };
    let kind = get_str(o, &["kind"])?;

    let content = if let Some(f) = file_obj {
        if let Some(m) = get_str(f, &["mimeType", "mime_type"])? {
            media_type = Some(m);
        }
        if let Some(n) = get_str(f, &["name"])? {
            filename = Some(n);
        }
        if let Some(b) = get_str(f, &["fileWithBytes", "file_with_bytes"])? {
            PartContent::Raw(unwrap_v03_proto_bytes(&b)?)
        } else if let Some(b) = get_str(f, &["bytes"])? {
            PartContent::Raw(normalize_base64(&b)?)
        } else if let Some(u) = get_str(f, &["uri", "fileWithUri", "file_with_uri"])? {
            PartContent::Url(u)
        } else {
            return Err("file part needs bytes or uri".into());
        }
    } else {
        let present: Vec<&str> = ["text", "raw", "url", "data"]
            .into_iter()
            .filter(|k| o.contains_key(*k))
            .collect();
        if present.len() > 1 && w == Wire::V1 {
            return Err(format!(
                "a part must set exactly one of text, raw, url, data (got {})",
                present.join(", ")
            ));
        }
        match present.first().copied() {
            Some("text") => PartContent::Text(
                get_str(o, &["text"])?.ok_or_else(|| "text must be a string".to_string())?,
            ),
            Some("raw") => PartContent::Raw(normalize_base64(
                &get_str(o, &["raw"])?.ok_or_else(|| "raw must be a string".to_string())?,
            )?),
            Some("url") => PartContent::Url(
                get_str(o, &["url"])?.ok_or_else(|| "url must be a string".to_string())?,
            ),
            Some("data") => {
                let d = o.get("data").cloned().unwrap_or(Value::Null);
                // v0.3 REST wraps the struct: {"data": {"data": {...}}}.
                let d = match (w, &d) {
                    (Wire::V03Rest, Value::Object(inner)) if inner.len() == 1 => {
                        inner.get("data").cloned().unwrap_or(d)
                    }
                    _ => d,
                };
                if (w != Wire::V1 || kind.is_some()) && !d.is_object() {
                    return Err("v0.3 data parts must be objects".into());
                }
                // Undo the v0.3 wrapping of non-object values.
                let compat = metadata
                    .as_mut()
                    .and_then(|m| m.remove("data_part_compat"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if metadata.as_ref().is_some_and(|m| m.is_empty()) {
                    metadata = None;
                }
                let value = if compat {
                    d.get("value").cloned().unwrap_or(Value::Null)
                } else {
                    d
                };
                PartContent::Data(value)
            }
            _ => {
                return Err(
                    "a part must contain one of text, raw, url, data (or a v0.3 file)".into(),
                )
            }
        }
    };
    if let Some(k) = kind {
        let expected = match content {
            PartContent::Text(_) => "text",
            PartContent::Raw(_) | PartContent::Url(_) => "file",
            PartContent::Data(_) => "data",
        };
        if k != expected {
            return Err(format!("part kind {k:?} does not match its content"));
        }
    }
    Ok(Part {
        content,
        metadata,
        filename,
        media_type,
    })
}

/// Parse a client message (role must be user).
pub fn parse_message(v: &Value, w: Wire) -> Result<Message, String> {
    let Value::Object(o) = v else {
        return Err("message must be an object".into());
    };
    let message_id = get_str(o, &["messageId", "message_id"])?.unwrap_or_else(new_id);
    if !valid_id(&message_id) {
        return Err("message.messageId must be 1..128 printable characters".into());
    }
    let role_raw = get_str(o, &["role"])?.ok_or("message.role is required")?;
    let role = Role::parse(&role_raw).ok_or_else(|| format!("invalid role {role_raw:?}"))?;
    if role != Role::User {
        return Err("client messages must use the user role".into());
    }
    let parts_v = get(o, &["parts", "content"]).ok_or("message.parts is required")?;
    let Value::Array(items) = parts_v else {
        return Err("message.parts must be an array".into());
    };
    if items.is_empty() {
        return Err("message.parts must contain at least one part".into());
    }
    if items.len() > MAX_PARTS {
        return Err(format!("at most {MAX_PARTS} parts are accepted"));
    }
    let parts = items
        .iter()
        .map(|p| parse_part(p, w))
        .collect::<Result<Vec<_>, _>>()?;
    let context_id = get_str(o, &["contextId", "context_id"])?.filter(|s| !s.is_empty());
    let task_id = get_str(o, &["taskId", "task_id"])?.filter(|s| !s.is_empty());
    for (name, id) in [("contextId", &context_id), ("taskId", &task_id)] {
        if let Some(id) = id {
            if !valid_id(id) {
                return Err(format!(
                    "message.{name} must be 1..128 printable characters"
                ));
            }
        }
    }
    Ok(Message {
        message_id,
        context_id,
        task_id,
        role,
        parts,
        metadata: get_meta(o)?,
        extensions: get_string_list(o, &["extensions"])?,
        reference_task_ids: get_string_list(o, &["referenceTaskIds", "reference_task_ids"])?,
    })
}

/// Push configuration as supplied by a client (before validation).
#[derive(Clone, Debug, PartialEq)]
pub struct PushInput {
    pub id: Option<String>,
    pub url: String,
    pub token: Option<String>,
    pub authentication: Option<PushAuth>,
}

/// Parse a v1.0 `TaskPushNotificationConfig` or a v0.3
/// `PushNotificationConfig` object (both have `url`, `token`, `id`).
pub fn parse_push_input(v: &Value) -> Result<PushInput, String> {
    let Value::Object(o) = v else {
        return Err("push notification config must be an object".into());
    };
    let url = get_str(o, &["url"])?.ok_or("push notification config needs a url")?;
    let id = get_str(o, &["id"])?.filter(|s| !s.is_empty());
    if let Some(id) = &id {
        if !valid_id(id) {
            return Err("push notification config id must be 1..128 printable characters".into());
        }
    }
    let token = get_str(o, &["token"])?.filter(|s| !s.is_empty());
    let authentication = match o.get("authentication") {
        None | Some(Value::Null) => None,
        Some(Value::Object(a)) => {
            let scheme = match get_str(a, &["scheme"])? {
                Some(s) => Some(s),
                None => match get(a, &["schemes"]) {
                    Some(Value::Array(list)) => {
                        list.first().and_then(|s| s.as_str()).map(str::to_string)
                    }
                    _ => None,
                },
            };
            let scheme = scheme.ok_or("authentication needs a scheme")?;
            if scheme.is_empty()
                || !scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-')
            {
                return Err("authentication scheme must be a token like Bearer or Basic".into());
            }
            Some(PushAuth {
                scheme,
                credentials: get_str(a, &["credentials"])?,
            })
        }
        Some(_) => return Err("authentication must be an object".into()),
    };
    for (name, value) in [
        ("token", &token),
        (
            "credentials",
            &authentication.as_ref().and_then(|a| a.credentials.clone()),
        ),
    ] {
        if let Some(v) = value {
            if v.len() > 4096 || v.chars().any(|c| c.is_control()) {
                return Err(format!(
                    "{name} must be printable and at most 4096 characters"
                ));
            }
        }
    }
    if url.len() > 2048 {
        return Err("push notification url is too long".into());
    }
    Ok(PushInput {
        id,
        url,
        token,
        authentication,
    })
}

/// A parsed SendMessage / message/send request.
#[derive(Clone, Debug)]
pub struct SendRequest {
    pub message: Message,
    pub accepted_output_modes: Vec<String>,
    pub push: Option<PushInput>,
    pub history_length: Option<usize>,
    pub return_immediately: bool,
}

pub fn parse_history_length(v: Option<i64>) -> Result<Option<usize>, String> {
    match v {
        None => Ok(None),
        Some(n) if n < 0 => Err(format!(
            "historyLength must be a non-negative integer, got {n}"
        )),
        Some(n) => Ok(Some(usize::try_from(n).unwrap_or(usize::MAX))),
    }
}

pub fn parse_send(params: &Value, w: Wire) -> Result<SendRequest, String> {
    let Value::Object(o) = params else {
        return Err("params must be an object".into());
    };
    let msg_v = get(o, &["message", "request"]).ok_or("params.message is required")?;
    let message = parse_message(msg_v, w)?;
    let mut req = SendRequest {
        message,
        accepted_output_modes: Vec::new(),
        push: None,
        history_length: None,
        // v0.3 proto: no configuration means blocking == false.
        return_immediately: w == Wire::V03Rest,
    };
    match o.get("configuration") {
        None | Some(Value::Null) => {}
        Some(Value::Object(c)) => {
            req.accepted_output_modes =
                get_string_list(c, &["acceptedOutputModes", "accepted_output_modes"])?;
            if let Some(p) = get(
                c,
                &[
                    "taskPushNotificationConfig",
                    "pushNotificationConfig",
                    "pushNotification",
                    "push_notification",
                    "task_push_notification_config",
                ],
            ) {
                req.push = Some(parse_push_input(p)?);
            }
            let hl = get_int(c, &["historyLength", "history_length"])?;
            // The v0.3 proto field is not optional: 0 means unset there.
            let hl = if w == Wire::V03Rest && hl == Some(0) {
                None
            } else {
                hl
            };
            req.history_length = parse_history_length(hl)?;
            req.return_immediately = match w {
                Wire::V1 => {
                    get_bool(c, &["returnImmediately", "return_immediately"])?.unwrap_or(false)
                }
                // v0.3 JSON-RPC: `blocking` defaults to true.
                Wire::V03Rpc => !get_bool(c, &["blocking"])?.unwrap_or(true),
                // v0.3 proto: a plain bool, so absent means false (this is
                // also how the reference SDK reads it).
                Wire::V03Rest => !get_bool(c, &["blocking"])?.unwrap_or(false),
            };
        }
        Some(_) => return Err("configuration must be an object".into()),
    }
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_task() -> Task {
        let ctx = "ctx-1";
        let user = Message {
            message_id: "m1".into(),
            context_id: Some(ctx.into()),
            task_id: Some("t1".into()),
            role: Role::User,
            parts: vec![Part::text("hi")],
            metadata: None,
            extensions: vec![],
            reference_task_ids: vec![],
        };
        Task {
            id: "t1".into(),
            context_id: ctx.into(),
            status: TaskStatus::new(TaskState::Canceled, None),
            artifacts: vec![Artifact::new(
                "a1",
                "out",
                "",
                vec![
                    Part::text("x"),
                    Part::data(json!([1, 2])),
                    Part::url("http://h/image/png", "map.png", "image/png"),
                    Part::raw(b"hello", "h.txt", "text/plain"),
                ],
            )],
            history: vec![user],
            metadata: None,
        }
    }

    #[test]
    fn v1_task_shape() {
        let v = task(&sample_task(), Wire::V1, TaskView::FULL);
        assert!(v.get("kind").is_none());
        assert_eq!(v["status"]["state"], "TASK_STATE_CANCELED");
        assert_eq!(v["history"][0]["role"], "ROLE_USER");
        assert_eq!(v["history"][0]["parts"][0]["text"], "hi");
        let parts = &v["artifacts"][0]["parts"];
        assert_eq!(parts[1]["data"], json!([1, 2]));
        assert_eq!(parts[1]["mediaType"], "application/json");
        assert_eq!(parts[2]["url"], "http://h/image/png");
        assert_eq!(parts[3]["raw"], "aGVsbG8=");
        assert_eq!(parts[3]["filename"], "h.txt");
    }

    #[test]
    fn v03_rpc_task_shape() {
        let v = task(&sample_task(), Wire::V03Rpc, TaskView::FULL);
        assert_eq!(v["kind"], "task");
        assert_eq!(v["status"]["state"], "canceled");
        assert_eq!(v["history"][0]["kind"], "message");
        assert_eq!(v["history"][0]["role"], "user");
        let parts = &v["artifacts"][0]["parts"];
        assert_eq!(parts[0]["kind"], "text");
        assert_eq!(parts[1]["kind"], "data");
        assert_eq!(parts[1]["data"]["value"], json!([1, 2]));
        assert_eq!(parts[1]["metadata"]["data_part_compat"], true);
        assert_eq!(parts[2]["file"]["uri"], "http://h/image/png");
        assert_eq!(parts[2]["file"]["mimeType"], "image/png");
        assert_eq!(parts[3]["file"]["bytes"], "aGVsbG8=");
        assert_eq!(parts[3]["file"]["name"], "h.txt");
    }

    #[test]
    fn v03_rest_task_shape() {
        let v = task(&sample_task(), Wire::V03Rest, TaskView::FULL);
        assert_eq!(v["status"]["state"], "TASK_STATE_CANCELLED");
        assert_eq!(v["history"][0]["content"][0]["text"], "hi");
        let parts = &v["artifacts"][0]["parts"];
        assert_eq!(parts[1]["data"]["data"]["value"], json!([1, 2]));
        assert_eq!(parts[2]["file"]["fileWithUri"], "http://h/image/png");
        assert_eq!(parts[3]["file"]["fileWithBytes"], "YUdWc2JHOD0=");
        let back = parse_part(&parts[3], Wire::V03Rest).expect("round trip");
        assert_eq!(back.content, PartContent::Raw("aGVsbG8=".into()));
    }

    #[test]
    fn history_length_and_artifacts_view() {
        let t = sample_task();
        let v = task(
            &t,
            Wire::V1,
            TaskView {
                history_length: Some(0),
                include_artifacts: false,
            },
        );
        assert!(v.get("history").is_none());
        assert!(v.get("artifacts").is_none());
    }

    #[test]
    fn events() {
        let su = StreamEvent::Status(StatusUpdate {
            task_id: "t".into(),
            context_id: "c".into(),
            status: TaskStatus::new(TaskState::InputRequired, None),
        });
        let v1 = event(&su, Wire::V1, TaskView::FULL);
        assert_eq!(
            v1["statusUpdate"]["status"]["state"],
            "TASK_STATE_INPUT_REQUIRED"
        );
        assert!(v1["statusUpdate"].get("final").is_none());
        let v03 = event(&su, Wire::V03Rpc, TaskView::FULL);
        assert_eq!(v03["kind"], "status-update");
        assert_eq!(v03["final"], true);
        let au = StreamEvent::Artifact(ArtifactUpdate {
            task_id: "t".into(),
            context_id: "c".into(),
            artifact: Artifact::new("a", "n", "", vec![Part::text("x")]),
            append: true,
            last_chunk: false,
        });
        let v1 = event(&au, Wire::V1, TaskView::FULL);
        assert_eq!(v1["artifactUpdate"]["append"], true);
        assert!(v1["artifactUpdate"].get("lastChunk").is_none());
        let v03 = event(&au, Wire::V03Rpc, TaskView::FULL);
        assert_eq!(v03["kind"], "artifact-update");
        assert_eq!(v03["lastChunk"], false);
    }

    #[test]
    fn parse_parts_in_every_shape() {
        let p = parse_part(&json!({"text": "a"}), Wire::V1).expect("v1 text");
        assert_eq!(p.content, PartContent::Text("a".into()));
        let p = parse_part(&json!({"kind": "text", "text": "a"}), Wire::V03Rpc).expect("v03");
        assert_eq!(p.content, PartContent::Text("a".into()));
        let p = parse_part(
            &json!({"kind": "file", "file": {"uri": "http://x", "mimeType": "image/png", "name": "x.png"}}),
            Wire::V03Rpc,
        )
        .expect("v03 file");
        assert_eq!(p.content, PartContent::Url("http://x".into()));
        assert_eq!(p.media_type.as_deref(), Some("image/png"));
        assert_eq!(p.filename.as_deref(), Some("x.png"));
        let p = parse_part(&json!({"raw": "aGVsbG8"}), Wire::V1).expect("raw no pad");
        assert_eq!(p.content, PartContent::Raw("aGVsbG8=".into()));
        let p = parse_part(&json!({"data": {"data": {"a": 1}}}), Wire::V03Rest).expect("rest data");
        assert_eq!(p.content, PartContent::Data(json!({"a": 1})));
        let p = parse_part(
            &json!({"kind": "data", "data": {"value": 5}, "metadata": {"data_part_compat": true}}),
            Wire::V03Rpc,
        )
        .expect("compat data");
        assert_eq!(p.content, PartContent::Data(json!(5)));
        assert!(p.metadata.is_none());
        assert!(parse_part(&json!({"text": "a", "url": "b"}), Wire::V1).is_err());
        assert!(parse_part(&json!({}), Wire::V1).is_err());
        assert!(parse_part(&json!({"raw": "!!!"}), Wire::V1).is_err());
        assert!(parse_part(&json!({"kind": "data", "text": "x"}), Wire::V03Rpc).is_err());
    }

    #[test]
    fn parse_send_versions() {
        let v1 = json!({
            "message": {"messageId": "m", "role": "ROLE_USER", "parts": [{"text": "hi"}]},
            "configuration": {"returnImmediately": true, "historyLength": 2,
                "taskPushNotificationConfig": {"url": "/a2a/webhook-sink/x", "authentication": {"scheme": "Bearer", "credentials": "c"}}}
        });
        let r = parse_send(&v1, Wire::V1).expect("v1");
        assert!(r.return_immediately);
        assert_eq!(r.history_length, Some(2));
        assert_eq!(
            r.push
                .as_ref()
                .and_then(|p| p.authentication.clone())
                .map(|a| a.scheme),
            Some("Bearer".into())
        );
        let v03 = json!({
            "message": {"kind": "message", "messageId": "m", "role": "user", "parts": [{"kind": "text", "text": "hi"}]},
            "configuration": {"blocking": false, "pushNotificationConfig": {"url": "u", "authentication": {"schemes": ["Basic"]}}}
        });
        let r = parse_send(&v03, Wire::V03Rpc).expect("v03");
        assert!(r.return_immediately);
        assert_eq!(
            r.push.and_then(|p| p.authentication).map(|a| a.scheme),
            Some("Basic".into())
        );
        let rest = json!({"request": {"message_id": "m", "role": "ROLE_USER", "content": [{"text": "hi"}]},
            "configuration": {"history_length": 0}});
        let r = parse_send(&rest, Wire::V03Rest).expect("rest");
        assert_eq!(r.history_length, None);
        assert!(
            r.return_immediately,
            "v0.3 proto bool blocking defaults to false"
        );
        assert!(parse_send(
            &json!({"message": {"role": "ROLE_AGENT", "parts": [{"text": "x"}]}}),
            Wire::V1
        )
        .is_err());
        assert!(parse_send(
            &json!({"message": {"role": "ROLE_USER", "parts": []}}),
            Wire::V1
        )
        .is_err());
        assert!(parse_send(&json!({}), Wire::V1).is_err());
    }

    #[test]
    fn push_config_shapes() {
        let c = PushConfig {
            id: "p".into(),
            task_id: "t".into(),
            url: "u".into(),
            token: Some("tok".into()),
            authentication: Some(PushAuth {
                scheme: "Bearer".into(),
                credentials: Some("c".into()),
            }),
            version: Version::V10,
            sink_id: None,
        };
        let v1 = push_config(&c, Wire::V1);
        assert_eq!(v1["taskId"], "t");
        assert_eq!(v1["authentication"]["scheme"], "Bearer");
        let v03 = push_config(&c, Wire::V03Rpc);
        assert_eq!(
            v03["pushNotificationConfig"]["authentication"]["schemes"][0],
            "Bearer"
        );
        let rest = push_config(&c, Wire::V03Rest);
        assert_eq!(rest["name"], "tasks/t/pushNotificationConfigs/p");
    }
}
