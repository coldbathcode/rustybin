//! The models a provider offers for the user's key, and what each one is.
//!
//! `POST /_rustybin/assist/models` asks the configured provider for its list
//! (OpenAI and compatible endpoints: `GET {base}/models`; Anthropic: `GET
//! /v1/models`; Gemini: `GET /v1beta/models`, generateContent models only; the
//! mock: its one model) through the outbound policy, with the stored key, and
//! keeps it in memory (bounded). `GET` answers that list merged with the
//! curated table: every model carries its tier, context window, list price,
//! note, whether it reasons, whether the provider listed it, and the jobs it
//! is chosen for. The console's Management, AI shows it and the job picker.

use serde_json::{json, Value};

use super::models::{self, ModelInfo, Tier};
use super::provider::{self, Kind, Problem};
use crate::egress::Egress;

/// Most entries kept from a provider's list.
pub const MAX_MODELS: usize = 500;

/// One model as the provider listed it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub name: Option<String>,
    /// Context window (input tokens), when the provider says.
    pub context: Option<u32>,
    /// Output token limit, when the provider says.
    pub output_limit: Option<u32>,
    /// When the provider says the model was created (RFC 3339 or a date).
    pub created: Option<String>,
}

/// A provider's list, as loaded.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub kind: Kind,
    pub at: String,
    pub entries: Vec<Entry>,
    /// How many the provider listed before the cap.
    pub listed: usize,
    /// The provider, base URL and key it was loaded with (hashed): the list
    /// is served only while they are unchanged.
    pub fingerprint: [u8; 32],
    /// When it was loaded (the clock's monotonic time), for the TTL.
    pub mono: std::time::Duration,
}

fn cap(mut entries: Vec<Entry>) -> (Vec<Entry>, usize) {
    let listed = entries.len();
    entries.truncate(MAX_MODELS);
    (entries, listed)
}

/// OpenAI and compatible: `{"data": [{"id", "created", "owned_by"}]}`.
pub fn parse_openai(v: &Value) -> Vec<Entry> {
    let mut out: Vec<Entry> = v
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m.get("id").and_then(Value::as_str)?.trim();
            (!id.is_empty()).then(|| Entry {
                id: id.to_string(),
                name: None,
                context: None,
                output_limit: None,
                created: m.get("created").and_then(Value::as_i64).and_then(|t| {
                    chrono::DateTime::from_timestamp(t, 0).map(|d| d.format("%Y-%m-%d").to_string())
                }),
            })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// Anthropic: `{"data": [{"id", "display_name", "created_at"}]}`.
pub fn parse_anthropic(v: &Value) -> Vec<Entry> {
    let mut out: Vec<Entry> = v
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m.get("id").and_then(Value::as_str)?.trim();
            (!id.is_empty()).then(|| Entry {
                id: id.to_string(),
                name: m
                    .get("display_name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                context: None,
                output_limit: None,
                created: m
                    .get("created_at")
                    .and_then(Value::as_str)
                    .map(|s| s.chars().take(10).collect()),
            })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// Gemini: `{"models": [{"name": "models/x", "displayName", "inputTokenLimit",
/// "outputTokenLimit", "supportedGenerationMethods"}]}`; only the models that
/// answer `generateContent` are of use here.
pub fn parse_gemini(v: &Value) -> Vec<Entry> {
    let mut out: Vec<Entry> = v
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| {
            m.get("supportedGenerationMethods")
                .and_then(Value::as_array)
                .is_none_or(|a| a.iter().any(|x| x.as_str() == Some("generateContent")))
        })
        .filter_map(|m| {
            let name = m.get("name").and_then(Value::as_str)?.trim();
            let id = name.strip_prefix("models/").unwrap_or(name);
            (!id.is_empty()).then(|| Entry {
                id: id.to_string(),
                name: m
                    .get("displayName")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                context: m
                    .get("inputTokenLimit")
                    .and_then(Value::as_u64)
                    .map(|n| n.min(u32::MAX as u64) as u32),
                output_limit: m
                    .get("outputTokenLimit")
                    .and_then(Value::as_u64)
                    .map(|n| n.min(u32::MAX as u64) as u32),
                created: None,
            })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// The list URL and headers of a provider.
fn request(kind: Kind, base: &str, key: Option<&str>) -> (String, Vec<(&'static str, String)>) {
    let base = base.trim_end_matches('/');
    match kind {
        Kind::Anthropic => (
            format!("{base}/v1/models?limit=1000"),
            vec![
                ("x-api-key", key.unwrap_or_default().to_string()),
                ("anthropic-version", super::anthropic::VERSION.to_string()),
            ],
        ),
        Kind::Gemini => (
            format!("{base}/v1beta/models?pageSize=1000"),
            vec![("x-goog-api-key", key.unwrap_or_default().to_string())],
        ),
        Kind::OpenAi | Kind::Compatible => (
            format!("{base}/models"),
            key.map(|k| vec![("authorization", format!("Bearer {k}"))])
                .unwrap_or_default(),
        ),
        Kind::Mock => (String::new(), Vec::new()),
    }
}

/// Ask the provider for its models (the mock answers its own entry).
pub async fn fetch(
    egress: &Egress,
    kind: Kind,
    base: &str,
    key: Option<&str>,
) -> Result<Loaded, Problem> {
    let entries = if kind == Kind::Mock {
        models::for_provider(Kind::Mock)
            .into_iter()
            .map(|m| Entry {
                id: m.id.to_string(),
                name: Some("Built-in mock LLM".to_string()),
                context: Some(m.context),
                output_limit: None,
                created: None,
            })
            .collect()
    } else {
        let (url, headers) = request(kind, base, key);
        let v = provider::get_json(egress, &url, headers).await?;
        match kind {
            Kind::Anthropic => parse_anthropic(&v),
            Kind::Gemini => parse_gemini(&v),
            _ => parse_openai(&v),
        }
    };
    let (entries, listed) = cap(entries);
    Ok(Loaded {
        kind,
        at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        entries,
        listed,
        fingerprint: [0; 32],
        mono: std::time::Duration::ZERO,
    })
}

/// A guess at the tier of a model the curated table does not know: the
/// cheap, small ones are fast.
fn guess_tier(id: &str) -> Tier {
    let l = id.to_ascii_lowercase();
    if ["mini", "nano", "flash", "haiku", "lite", "small", "tiny"]
        .iter()
        .any(|w| l.contains(w))
    {
        Tier::Fast
    } else {
        Tier::Capable
    }
}

/// Whether a model reasons before it answers (spends output tokens thinking).
pub fn reasons(kind: Kind, id: &str) -> bool {
    match kind {
        Kind::OpenAi | Kind::Compatible => super::openai::is_reasoning_model(id),
        Kind::Gemini => {
            let l = id.to_ascii_lowercase();
            l.contains("thinking") || l.starts_with("gemini-2.5") || l.starts_with("gemini-3")
        }
        Kind::Anthropic | Kind::Mock => false,
    }
}

/// One model for the API: the provider's entry, the curated row, or both.
pub fn describe(
    kind: Kind,
    id: &str,
    entry: Option<&Entry>,
    curated: Option<&ModelInfo>,
    listed: bool,
) -> Value {
    let tier = curated.map(|c| c.tier).unwrap_or_else(|| guess_tier(id));
    let context = entry.and_then(|e| e.context).or(curated.map(|c| c.context));
    json!({
        "id": id,
        "provider": kind.as_str(),
        "name": entry.and_then(|e| e.name.clone()),
        "tier": tier.as_str(),
        "context": context,
        "output_limit": entry.and_then(|e| e.output_limit),
        "price_per_million": models::price(kind, id).map(|(i, o)| json!({ "input": i, "output": o })),
        "note": curated.map(|c| c.note),
        "reasoning": reasons(kind, id),
        "curated": curated.is_some(),
        "listed": listed,
        "created": entry.and_then(|e| e.created.clone()),
    })
}

/// The models of a provider for the API: what it listed (when loaded) merged
/// with the curated rows, curated first in table order, then the rest by id.
pub fn merged(kind: Kind, loaded: Option<&Loaded>) -> Vec<Value> {
    let curated = models::for_provider(kind);
    let entries: &[Entry] = loaded
        .filter(|l| l.kind == kind)
        .map(|l| l.entries.as_slice())
        .unwrap_or(&[]);
    let find = |id: &str| entries.iter().find(|e| e.id == id);
    let mut out: Vec<Value> = curated
        .iter()
        .map(|c| {
            let e = find(c.id);
            describe(kind, c.id, e, Some(c), loaded.is_none() || e.is_some())
        })
        .collect();
    for e in entries {
        if curated.iter().any(|c| c.id == e.id) {
            continue;
        }
        out.push(describe(kind, &e.id, Some(e), None, true));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_provider_shape() {
        let o = parse_openai(&json!({"data": [
            {"id": "gpt-5", "created": 1_754_000_000, "owned_by": "system"},
            {"id": "gpt-4o", "created": 1_715_000_000},
            {"id": "gpt-5"},
            {"nope": true}
        ]}));
        assert_eq!(
            o.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["gpt-4o", "gpt-5"]
        );
        assert_eq!(o[1].created.as_deref(), Some("2025-07-31"));
        let a = parse_anthropic(&json!({"data": [
            {"id": "claude-sonnet-5-5", "display_name": "Claude Sonnet 5.5", "created_at": "2026-04-01T00:00:00Z"}
        ], "has_more": false}));
        assert_eq!(a[0].name.as_deref(), Some("Claude Sonnet 5.5"));
        assert_eq!(a[0].created.as_deref(), Some("2026-04-01"));
        let g = parse_gemini(&json!({"models": [
            {"name": "models/gemini-2.5-pro", "displayName": "Gemini 2.5 Pro", "inputTokenLimit": 1048576, "outputTokenLimit": 65536, "supportedGenerationMethods": ["generateContent"]},
            {"name": "models/embedding-001", "supportedGenerationMethods": ["embedContent"]}
        ]}));
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].id, "gemini-2.5-pro");
        assert_eq!(g[0].context, Some(1_048_576));
        assert_eq!(g[0].output_limit, Some(65_536));
    }

    #[test]
    fn the_list_is_capped_and_merged_with_the_table() {
        let many: Vec<Value> = (0..MAX_MODELS + 20)
            .map(|i| json!({"id": format!("m-{i:04}")}))
            .collect();
        let (entries, listed) = cap(parse_openai(&json!({"data": many})));
        assert_eq!(entries.len(), MAX_MODELS);
        assert_eq!(listed, MAX_MODELS + 20);
        let loaded = Loaded {
            kind: Kind::OpenAi,
            at: "now".into(),
            entries: parse_openai(&json!({"data": [{"id": "gpt-5"}, {"id": "gpt-4.1"}]})),
            listed: 2,
            fingerprint: [0; 32],
            mono: std::time::Duration::ZERO,
        };
        let m = merged(Kind::OpenAi, Some(&loaded));
        let gpt5 = m.iter().find(|v| v["id"] == "gpt-5").expect("gpt-5");
        assert_eq!(gpt5["curated"], true);
        assert_eq!(gpt5["listed"], true);
        assert_eq!(gpt5["reasoning"], true);
        let mini = m.iter().find(|v| v["id"] == "gpt-5-mini").expect("mini");
        assert_eq!(
            mini["listed"], false,
            "curated but not in the provider's list"
        );
        let extra = m.iter().find(|v| v["id"] == "gpt-4.1").expect("extra");
        assert_eq!(extra["curated"], false);
        assert_eq!(extra["tier"], "capable");
        assert_eq!(extra["reasoning"], false);
        // Without a loaded list every curated row counts as listed.
        let m = merged(Kind::Anthropic, None);
        assert!(m
            .iter()
            .all(|v| v["listed"] == true && v["curated"] == true));
        assert_eq!(guess_tier("claude-haiku-9"), Tier::Fast);
        assert!(reasons(Kind::Gemini, "gemini-2.5-flash"));
        let (url, headers) = request(Kind::Anthropic, "https://api.anthropic.com/", Some("k"));
        assert_eq!(url, "https://api.anthropic.com/v1/models?limit=1000");
        assert_eq!(headers[0], ("x-api-key", "k".to_string()));
        let (url, headers) = request(Kind::Compatible, "http://localhost:11434/v1", None);
        assert_eq!(url, "http://localhost:11434/v1/models");
        assert!(headers.is_empty());
    }
}
