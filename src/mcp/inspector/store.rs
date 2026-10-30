//! Saved connections and check runs of the MCP inspector: bounded, kept in
//! memory and, with `RUSTYBIN_STATE_DIR`, in `<state>/mcp-inspector.json`.
//! A connection's credentials are sealed with the settings store's key and
//! returned as `{set, hint}` only; the relay and the checks open them when
//! a request names the connection.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::settings::{Sealed, Settings};

pub const FILE: &str = "mcp-inspector.json";
pub const MAX_CONNECTIONS: usize = 50;
pub const MAX_RUNS: usize = 50;
/// Largest stored run (larger runs drop their per-check details).
pub const MAX_RUN_BYTES: usize = 256 * 1024;
pub const MAX_SECRET_BYTES: usize = 8 * 1024;
/// Secret keys a connection may hold (custom headers are `header:<name>`).
pub const SECRET_KEYS: &[&str] = &[
    "token",
    "api_key",
    "password",
    "refresh_token",
    "client_secret",
];

/// How a connection authenticates.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthSpec {
    /// `none`, `bearer`, `api_key`, `basic`, `headers` or `oauth`.
    pub kind: String,
    /// The API key's header or query parameter name (default `X-API-Key`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `header` (default) or `query`, for API keys.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Names of the custom headers (their values are secrets `header:<name>`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<String>,
    /// The OAuth facts worth keeping (issuer, token endpoint, client id,
    /// scopes, resource): never a secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<Value>,
}

pub const AUTH_KINDS: &[&str] = &["none", "bearer", "api_key", "basic", "headers", "oauth"];

impl AuthSpec {
    pub fn validate(&self) -> Result<(), String> {
        let kind = if self.kind.is_empty() {
            "none"
        } else {
            self.kind.as_str()
        };
        if !AUTH_KINDS.contains(&kind) {
            return Err(format!(
                "auth.kind must be one of {}",
                AUTH_KINDS.join(", ")
            ));
        }
        if let Some(n) = &self.name {
            if n.is_empty()
                || n.len() > 128
                || axum::http::HeaderName::from_bytes(n.as_bytes()).is_err()
            {
                return Err("auth.name must be a valid header or parameter name".into());
            }
        }
        if let Some(l) = &self.location {
            if l != "header" && l != "query" {
                return Err("auth.location must be header or query".into());
            }
        }
        if self.headers.len() > 16 {
            return Err("at most 16 custom headers".into());
        }
        for h in &self.headers {
            if axum::http::HeaderName::from_bytes(h.as_bytes()).is_err() {
                return Err(format!("invalid header name {h:?}"));
            }
        }
        if let Some(o) = &self.oauth {
            if o.to_string().len() > 8 * 1024 {
                return Err("auth.oauth is larger than 8 KiB".into());
            }
        }
        Ok(())
    }
}

/// Name and value pairs (headers or query parameters).
pub type Pairs = Vec<(String, String)>;

/// The headers and query parameters a connection sends.
pub fn build_auth(
    spec: &AuthSpec,
    secrets: &BTreeMap<String, Zeroizing<String>>,
) -> (Pairs, Pairs) {
    let mut headers = Vec::new();
    let mut query = Vec::new();
    let get = |k: &str| secrets.get(k).map(|v| v.to_string());
    match spec.kind.as_str() {
        "bearer" | "oauth" => {
            if let Some(t) = get("token") {
                headers.push(("authorization".into(), format!("Bearer {t}")));
            }
        }
        "api_key" => {
            if let Some(k) = get("api_key") {
                let name = spec.name.clone().unwrap_or_else(|| "X-API-Key".into());
                if spec.location.as_deref() == Some("query") {
                    query.push((name, k));
                } else {
                    headers.push((name, k));
                }
            }
        }
        "basic" => {
            let user = spec.username.clone().unwrap_or_default();
            let pass = get("password").unwrap_or_default();
            let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
            headers.push(("authorization".into(), format!("Basic {token}")));
        }
        "headers" => {
            for name in &spec.headers {
                if let Some(v) = get(&format!("header:{name}")) {
                    headers.push((name.clone(), v));
                }
            }
        }
        _ => {}
    }
    (headers, query)
}

/// A saved connection (what the API returns, with secret states).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub target: String,
    /// `modern`, `session` or `legacy`.
    pub transport: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `browser` or `relay`.
    pub mode: String,
    pub auth: AuthSpec,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Stored {
    #[serde(flatten)]
    conn: Connection,
    secrets: BTreeMap<String, Sealed>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Doc {
    connections: Vec<Stored>,
    runs: Vec<Value>,
}

/// The inspector's store.
pub struct Store {
    settings: Arc<Settings>,
    doc: RwLock<Doc>,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn hint(v: &str) -> Option<String> {
    let n = v.chars().count();
    (n >= 8).then(|| v.chars().skip(n - 4).collect())
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Store {
    pub fn new(settings: Arc<Settings>) -> Self {
        let doc = settings
            .state_file(FILE)
            .and_then(|p| std::fs::read(&p).ok())
            .and_then(|b| serde_json::from_slice::<Doc>(&b).ok())
            .unwrap_or_default();
        Self {
            settings,
            doc: RwLock::new(doc),
        }
    }

    fn persist(&self, doc: &Doc) -> Result<(), String> {
        let Some(path) = self.settings.state_file(FILE) else {
            return Ok(());
        };
        let bytes = serde_json::to_vec_pretty(doc).map_err(|e| e.to_string())?;
        crate::settings::write_private(&path, &bytes)
    }

    pub fn persisted(&self) -> bool {
        self.settings.state_file(FILE).is_some()
    }

    fn view(&self, s: &Stored) -> Value {
        let mut v = serde_json::to_value(&s.conn).unwrap_or(Value::Null);
        let secrets: serde_json::Map<String, Value> = s
            .secrets
            .iter()
            .map(|(k, sealed)| {
                let state = match self.settings.open(sealed) {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes).to_string();
                        json!({ "set": true, "hint": hint(&text) })
                    }
                    Err(_) => json!({ "set": true, "unreadable": true }),
                };
                (k.clone(), state)
            })
            .collect();
        v["secrets"] = Value::Object(secrets);
        v
    }

    /// Every saved connection, secrets as states.
    pub fn list(&self) -> Vec<Value> {
        let doc = self.doc.read().unwrap_or_else(|e| e.into_inner());
        doc.connections.iter().map(|s| self.view(s)).collect()
    }

    pub fn get(&self, id: &str) -> Option<Value> {
        let doc = self.doc.read().unwrap_or_else(|e| e.into_inner());
        doc.connections
            .iter()
            .find(|s| s.conn.id == id)
            .map(|s| self.view(s))
    }

    /// The connection and its opened secrets (for the relay and the checks).
    pub fn resolve(&self, id: &str) -> Option<(Connection, BTreeMap<String, Zeroizing<String>>)> {
        let doc = self.doc.read().unwrap_or_else(|e| e.into_inner());
        let s = doc.connections.iter().find(|s| s.conn.id == id)?;
        let secrets = s
            .secrets
            .iter()
            .filter_map(|(k, sealed)| {
                let bytes = self.settings.open(sealed).ok()?;
                Some((
                    k.clone(),
                    Zeroizing::new(String::from_utf8_lossy(&bytes).to_string()),
                ))
            })
            .collect();
        Some((s.conn.clone(), secrets))
    }

    /// Create or replace a connection. `secrets`: a value replaces, `null`
    /// clears, an absent key keeps.
    pub fn put(
        &self,
        id: &str,
        mut conn: Connection,
        secrets: BTreeMap<String, Option<String>>,
    ) -> Result<Value, String> {
        if !valid_id(id) {
            return Err("the id is 1 to 64 letters, digits, - and _".into());
        }
        if conn.name.trim().is_empty() || conn.name.chars().count() > 80 {
            return Err("name: 1 to 80 characters".into());
        }
        super::client::Target::parse(&conn.target, false)?;
        super::client::Transport::parse(
            Some(if conn.transport.is_empty() {
                "modern"
            } else {
                &conn.transport
            }),
            conn.version.as_deref(),
        )?;
        if !conn.mode.is_empty() && conn.mode != "browser" && conn.mode != "relay" {
            return Err("mode must be browser or relay".into());
        }
        conn.auth.validate()?;
        for (k, v) in &secrets {
            let known = SECRET_KEYS.contains(&k.as_str())
                || k.strip_prefix("header:")
                    .is_some_and(|n| conn.auth.headers.iter().any(|h| h == n));
            if !known {
                return Err(format!(
                    "unknown secret {k:?} (expected {} or header:<a custom header>)",
                    SECRET_KEYS.join(", ")
                ));
            }
            if v.as_ref()
                .is_some_and(|v| v.len() > MAX_SECRET_BYTES || v.chars().any(char::is_control))
            {
                return Err(format!(
                    "secret {k:?}: at most {MAX_SECRET_BYTES} bytes, no control characters"
                ));
            }
        }
        let mut doc = self.doc.write().unwrap_or_else(|e| e.into_inner());
        let mut next = doc.clone();
        let existing = next.connections.iter().position(|s| s.conn.id == id);
        if existing.is_none() && next.connections.len() >= MAX_CONNECTIONS {
            return Err(format!("at most {MAX_CONNECTIONS} saved connections"));
        }
        let mut stored = existing
            .map(|i| next.connections[i].clone())
            .unwrap_or_default();
        conn.id = id.to_string();
        conn.created_at = if stored.conn.created_at.is_empty() {
            now()
        } else {
            stored.conn.created_at.clone()
        };
        conn.updated_at = now();
        if conn.transport.is_empty() {
            conn.transport = "modern".into();
        }
        if conn.mode.is_empty() {
            conn.mode = "browser".into();
        }
        for (k, v) in secrets {
            match v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
                Some(plain) => {
                    let sealed = self.settings.seal(plain.as_bytes())?;
                    stored.secrets.insert(k, sealed);
                }
                None => {
                    stored.secrets.remove(&k);
                }
            }
        }
        // Secrets of custom headers that were removed go too.
        stored.secrets.retain(|k, _| {
            k.strip_prefix("header:")
                .is_none_or(|n| conn.auth.headers.iter().any(|h| h == n))
        });
        stored.conn = conn;
        match existing {
            Some(i) => next.connections[i] = stored.clone(),
            None => next.connections.push(stored.clone()),
        }
        self.persist(&next)?;
        *doc = next;
        drop(doc);
        tracing::info!(target: "rustybin::mcp_inspector", id, "connection saved");
        Ok(self.view(&stored))
    }

    pub fn delete(&self, id: &str) -> Result<bool, String> {
        let mut doc = self.doc.write().unwrap_or_else(|e| e.into_inner());
        let mut next = doc.clone();
        let before = next.connections.len();
        next.connections.retain(|s| s.conn.id != id);
        if next.connections.len() == before {
            return Ok(false);
        }
        self.persist(&next)?;
        *doc = next;
        Ok(true)
    }

    // ── Runs ──

    pub fn save_run(&self, report: &Value) -> Result<(), String> {
        let mut r = report.clone();
        if r.to_string().len() > MAX_RUN_BYTES {
            if let Some(checks) = r.get_mut("checks").and_then(Value::as_array_mut) {
                for c in checks {
                    if let Some(m) = c.as_object_mut() {
                        m.remove("details");
                    }
                }
            }
        }
        let mut doc = self.doc.write().unwrap_or_else(|e| e.into_inner());
        let mut next = doc.clone();
        next.runs.push(r);
        while next.runs.len() > MAX_RUNS {
            next.runs.remove(0);
        }
        self.persist(&next)?;
        *doc = next;
        Ok(())
    }

    /// Summaries of the saved runs, newest first.
    pub fn runs(&self) -> Vec<Value> {
        let doc = self.doc.read().unwrap_or_else(|e| e.into_inner());
        doc.runs
            .iter()
            .rev()
            .map(|r| {
                json!({
                    "id": r["id"], "started_at": r["started_at"], "target": r["target"],
                    "transport": r["transport"], "summary": r["summary"], "duration_ms": r["duration_ms"],
                })
            })
            .collect()
    }

    pub fn run(&self, id: &str) -> Option<Value> {
        let doc = self.doc.read().unwrap_or_else(|e| e.into_inner());
        doc.runs.iter().find(|r| r["id"] == id).cloned()
    }

    pub fn delete_run(&self, id: &str) -> Result<bool, String> {
        let mut doc = self.doc.write().unwrap_or_else(|e| e.into_inner());
        let mut next = doc.clone();
        let before = next.runs.len();
        next.runs.retain(|r| r["id"] != id);
        if next.runs.len() == before {
            return Ok(false);
        }
        self.persist(&next)?;
        *doc = next;
        Ok(true)
    }
}
