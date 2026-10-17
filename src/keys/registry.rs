//! The key registry: the JSON document listing every API key, parsed into a
//! lookup table by SHA-256 of the presented key.
//!
//! ```json
//! {"version":"etag","keys":[{"id":"k7f3a9c2d1e0b","hash":"<sha256 hex>",
//!   "label":"starter","status":"active","limits":{"rps":100,"requests":100000}}]}
//! ```
//!
//! Only `active` keys (or keys without a `status`) are loaded; any other
//! status (`revoked`, `disabled`, ...) behaves as an unknown key. Entries
//! with an invalid id or hash are skipped with a warning. A document with
//! more keys than the configured maximum is refused as a whole.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::limits::{LimitsConfig, Period, Scope};

/// Longest key accepted on a request (anything longer is invalid).
pub const MAX_PRESENTED_KEY_LEN: usize = 256;
/// Longest label kept (it becomes the `X-Rustybin-Plan` header value).
pub const MAX_LABEL_LEN: usize = 64;

/// Per-key limits as written in the registry. Absent fields use the
/// instance value (`RUSTYBIN_PLAN`, `RUSTYBIN_LIMIT_*`); 0 means unlimited.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct KeyLimits {
    pub rps: Option<u64>,
    pub burst: Option<u64>,
    pub concurrency: Option<u64>,
    pub streams: Option<u64>,
    pub stream_secs: Option<u64>,
    /// Requests per UTC day.
    pub requests: Option<u64>,
    /// Response body bytes per UTC day.
    pub egress_bytes: Option<u64>,
    /// Inspector entries kept for this key.
    pub inspector_entries: Option<u64>,
    /// Inspector entry lifetime for this key, in seconds.
    pub inspector_ttl_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RawRegistry {
    #[serde(default)]
    version: Option<serde_json::Value>,
    keys: Vec<RawKey>,
}

#[derive(Debug, Deserialize)]
struct RawKey {
    id: String,
    hash: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    limits: Option<KeyLimits>,
}

/// One active key.
#[derive(Debug)]
pub struct KeyRecord {
    /// Public key id (never secret): the session key for limits, inspector
    /// entries and usage counters.
    pub id: Arc<str>,
    hash: [u8; 32],
    /// Free-text label echoed in `X-Rustybin-Plan`.
    pub label: Option<String>,
    /// The limits as written in the registry.
    pub raw_limits: KeyLimits,
    /// Effective rate, concurrency and stream limits of this key (instance
    /// limits overridden by the record; daily quotas are enforced by the key
    /// layer, so `requests` and `egress_bytes` are 0 here).
    pub limits: Arc<LimitsConfig>,
    /// Requests per UTC day (0 = unlimited).
    pub requests_per_day: u64,
    /// Response bytes per UTC day (0 = unlimited).
    pub egress_per_day: u64,
    /// Inspector entries kept for this key (`None`: only the global capacity).
    pub inspector_entries: Option<usize>,
    /// Inspector entry lifetime (`None`: until evicted).
    pub inspector_ttl: Option<Duration>,
}

impl KeyRecord {
    /// Constant-time comparison of a presented key's hash with this record.
    fn matches(&self, hash: &[u8; 32]) -> bool {
        self.hash
            .iter()
            .zip(hash.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

/// A parsed registry.
#[derive(Debug, Default)]
pub struct KeySet {
    /// The document's `version` (also sent back as `If-None-Match` when no
    /// `ETag` header was received).
    pub version: Option<String>,
    /// Buckets by the first 8 bytes of the hash; the full hash is then
    /// compared in constant time.
    by_prefix: HashMap<u64, Vec<Arc<KeyRecord>>>,
    by_id: HashMap<Arc<str>, Arc<KeyRecord>>,
    /// Entries skipped (inactive or invalid).
    pub skipped: usize,
}

impl KeySet {
    /// Number of active keys.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// The active key matching a presented key, if any.
    pub fn find(&self, presented: &str) -> Option<Arc<KeyRecord>> {
        if presented.is_empty() || presented.len() > MAX_PRESENTED_KEY_LEN {
            return None;
        }
        let hash = sha256(presented.as_bytes());
        self.by_prefix
            .get(&prefix(&hash))?
            .iter()
            .find(|r| r.matches(&hash))
            .cloned()
    }

    /// The active key with this id.
    pub fn by_id(&self, id: &str) -> Option<Arc<KeyRecord>> {
        self.by_id.get(id).cloned()
    }

    /// Parse a registry document. `base` are the instance limits (absent
    /// key fields fall back to them); more than `max_keys` entries is an
    /// error. Invalid entries are skipped and counted in `skipped`.
    pub fn parse(raw: &[u8], base: &LimitsConfig, max_keys: usize) -> Result<Self, String> {
        let doc: RawRegistry =
            serde_json::from_slice(raw).map_err(|e| format!("not a key registry: {e}"))?;
        if doc.keys.len() > max_keys {
            return Err(format!(
                "{} keys exceed RUSTYBIN_KEYS_MAX ({max_keys})",
                doc.keys.len()
            ));
        }
        let version = doc.version.map(|v| match v {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        });
        let mut set = KeySet {
            version,
            ..KeySet::default()
        };
        for raw in doc.keys {
            let active = raw
                .status
                .as_deref()
                .is_none_or(|s| s.eq_ignore_ascii_case("active"));
            if !active {
                set.skipped += 1;
                continue;
            }
            let Some(hash) = parse_hash(&raw.hash) else {
                tracing::warn!(
                    "key registry: key {:?} has an invalid hash, skipped",
                    raw.id
                );
                set.skipped += 1;
                continue;
            };
            if !crate::session::is_valid_session(&raw.id) {
                tracing::warn!("key registry: invalid key id {:?}, skipped", raw.id);
                set.skipped += 1;
                continue;
            }
            if set.by_id.contains_key(raw.id.as_str()) {
                tracing::warn!("key registry: duplicate key id {:?}, skipped", raw.id);
                set.skipped += 1;
                continue;
            }
            let record = Arc::new(build_record(raw, hash, base));
            set.by_prefix
                .entry(prefix(&hash))
                .or_default()
                .push(record.clone());
            set.by_id.insert(record.id.clone(), record);
        }
        Ok(set)
    }
}

fn build_record(raw: RawKey, hash: [u8; 32], base: &LimitsConfig) -> KeyRecord {
    let l = raw.limits.unwrap_or_default();
    let label = raw.label.map(|s| clean_label(&s)).filter(|s| !s.is_empty());
    // Without a label the instance plan name is reported (`custom` when no
    // plan is set).
    let cfg = LimitsConfig {
        scope: Scope::Session,
        period: Period::Day,
        rps: l.rps.unwrap_or(base.rps),
        burst: l.burst.unwrap_or(base.burst),
        concurrency: l.concurrency.unwrap_or(base.concurrency),
        streams: l.streams.unwrap_or(base.streams),
        stream_lifetime: l
            .stream_secs
            .map(Duration::from_secs)
            .unwrap_or(base.stream_lifetime),
        // Daily quotas live in the key layer (shared across replicas).
        requests: 0,
        egress_bytes: 0,
        usage_file: None,
        overridden: true,
        label: label.clone(),
        per_key: true,
        ..base.clone()
    };
    let positive = |v: Option<u64>| v.filter(|n| *n > 0);
    KeyRecord {
        id: Arc::from(raw.id.as_str()),
        hash,
        label,
        requests_per_day: l.requests.unwrap_or(base.requests),
        egress_per_day: l.egress_bytes.unwrap_or(base.egress_bytes),
        inspector_entries: positive(l.inspector_entries)
            .map(|n| usize::try_from(n).unwrap_or(usize::MAX)),
        inspector_ttl: positive(l.inspector_ttl_secs).map(Duration::from_secs),
        raw_limits: l,
        limits: Arc::new(cfg),
    }
}

/// Printable ASCII only (it becomes a header value), at most
/// [`MAX_LABEL_LEN`] characters.
fn clean_label(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(MAX_LABEL_LEN)
        .collect::<String>()
        .trim()
        .to_string()
}

/// SHA-256 of `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Lowercase hex SHA-256 of a key (the registry `hash` field).
pub fn key_hash_hex(key: &str) -> String {
    sha256(key.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn prefix(hash: &[u8; 32]) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&hash[..8]);
    u64::from_be_bytes(b)
}

/// 64 hex characters (either case) to 32 bytes.
fn parse_hash(raw: &str) -> Option<[u8; 32]> {
    let raw = raw.trim().as_bytes();
    if raw.len() != 64 {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (i, pair) in raw.chunks(2).enumerate() {
        out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(out)
}
