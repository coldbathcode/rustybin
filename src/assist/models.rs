//! The curated model table: per provider, a few current models with a tier
//! and, where known, the list price per million tokens. Free-text model ids
//! are allowed everywhere; the table is what the console offers and what
//! the cost estimate uses.

use serde_json::{json, Value};

use super::provider::Kind;

/// `fast` for cheap tasks (suggestions, explanations), `capable` for the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Fast,
    Capable,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Fast => "fast",
            Tier::Capable => "capable",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub provider: Kind,
    pub id: &'static str,
    pub tier: Tier,
    /// Context window, in tokens.
    pub context: u32,
    /// List price per million input and output tokens, when known. `None`:
    /// the console says "see the provider's price list" and no cost is
    /// estimated.
    pub price: Option<(f64, f64)>,
    pub note: &'static str,
}

impl ModelInfo {
    pub fn describe(&self) -> Value {
        json!({
            "provider": self.provider.as_str(),
            "id": self.id,
            "tier": self.tier.as_str(),
            "context": self.context,
            "price_per_million": self.price.map(|(i, o)| json!({ "input": i, "output": o })),
            "note": self.note,
        })
    }
}

/// The table. Ids are the providers' current ones at build time; prices are
/// listed only where the list price is known.
pub const MODELS: &[ModelInfo] = &[
    ModelInfo {
        provider: Kind::Anthropic,
        id: "claude-sonnet-5-5",
        tier: Tier::Capable,
        context: 200_000,
        price: None,
        note: "The default: specs, edits and explanations",
    },
    ModelInfo {
        provider: Kind::Anthropic,
        id: "claude-opus-5-5",
        tier: Tier::Capable,
        context: 200_000,
        price: None,
        note: "The most capable; slower and dearer",
    },
    ModelInfo {
        provider: Kind::Anthropic,
        id: "claude-haiku-5-5",
        tier: Tier::Fast,
        context: 200_000,
        price: None,
        note: "Suggestions and quick explanations",
    },
    ModelInfo {
        provider: Kind::OpenAi,
        id: "gpt-5",
        tier: Tier::Capable,
        context: 400_000,
        price: Some((1.25, 10.0)),
        note: "The default",
    },
    ModelInfo {
        provider: Kind::OpenAi,
        id: "gpt-5-mini",
        tier: Tier::Fast,
        context: 400_000,
        price: Some((0.25, 2.0)),
        note: "Suggestions and quick explanations",
    },
    ModelInfo {
        provider: Kind::OpenAi,
        id: "gpt-5-nano",
        tier: Tier::Fast,
        context: 400_000,
        price: Some((0.05, 0.4)),
        note: "The cheapest",
    },
    ModelInfo {
        provider: Kind::Gemini,
        id: "gemini-2.5-pro",
        tier: Tier::Capable,
        context: 1_048_576,
        price: Some((1.25, 10.0)),
        note: "The default (price for prompts up to 200k tokens)",
    },
    ModelInfo {
        provider: Kind::Gemini,
        id: "gemini-2.5-flash",
        tier: Tier::Fast,
        context: 1_048_576,
        price: Some((0.3, 2.5)),
        note: "Suggestions and quick explanations",
    },
    ModelInfo {
        provider: Kind::Gemini,
        id: "gemini-2.5-flash-lite",
        tier: Tier::Fast,
        context: 1_048_576,
        price: Some((0.1, 0.4)),
        note: "The cheapest",
    },
    ModelInfo {
        provider: Kind::Mock,
        id: "rustybin-mock",
        tier: Tier::Capable,
        context: 32_000,
        price: Some((0.0, 0.0)),
        note: "Deterministic answers from the instance itself",
    },
];

/// The models of one provider, in table order.
pub fn for_provider(kind: Kind) -> Vec<&'static ModelInfo> {
    MODELS.iter().filter(|m| m.provider == kind).collect()
}

/// The provider's default model: its first capable entry (`None` for the
/// compatible endpoint, where the model is the user's).
pub fn default_model(kind: Kind) -> Option<&'static str> {
    for_provider(kind)
        .into_iter()
        .find(|m| m.tier == Tier::Capable)
        .map(|m| m.id)
}

/// The provider's default fast model: its first fast entry, else the default.
pub fn default_fast_model(kind: Kind) -> Option<&'static str> {
    for_provider(kind)
        .into_iter()
        .find(|m| m.tier == Tier::Fast)
        .map(|m| m.id)
        .or_else(|| default_model(kind))
}

/// The list price of a model, when the table knows it.
pub fn price(kind: Kind, model: &str) -> Option<(f64, f64)> {
    if kind == Kind::Mock {
        return Some((0.0, 0.0));
    }
    MODELS
        .iter()
        .find(|m| m.provider == kind && m.id == model)
        .and_then(|m| m.price)
}

/// Estimated cost in USD of a call, when the price is known.
pub fn cost(kind: Kind, model: &str, input: u64, output: u64) -> Option<f64> {
    let (i, o) = price(kind, model)?;
    Some(input as f64 * i / 1_000_000.0 + output as f64 * o / 1_000_000.0)
}

/// The table as JSON, grouped by provider.
pub fn table() -> Value {
    Value::Array(MODELS.iter().map(ModelInfo::describe).collect())
}
