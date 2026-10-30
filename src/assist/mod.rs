//! AI assist: the user's own model provider and key behind the console's AI
//! features (`docs/plans/next/step-03-ai-assist.md`).
//!
//! - Settings (`assist.*`, group `assist`): the provider (`anthropic`,
//!   `openai`, `gemini`, `compatible`, `mock`), the capable and fast models,
//!   the compatible base URL, and one secret key per provider; seeded by
//!   `RUSTYBIN_ASSIST_*`. Nothing calls a model until a key is set (the mock
//!   needs none). Keys never leave the instance: not in the API, not in
//!   captures, not in prompts (the redact pass refuses them).
//! - Every call is a [`task::Task`]: a system prompt, a user prompt rendered
//!   from validated inputs, an optional output schema validated with the
//!   mocks' JSON Schema validator and repaired at most twice, and the mock
//!   provider's deterministic answer.
//! - Egress through the policy under `Purpose::Assist`: a provider's host is
//!   allowed once its key is set; 60 s, 2 MiB, pinned, no redirects.
//! - Caps per caller (calls per minute) and per answer (output tokens);
//!   usage per provider and model in memory (bounded) and in the metrics.
//!
//! Routes: `GET /_rustybin/assist`, `POST /_rustybin/assist/test`,
//! `POST /_rustybin/assist/run` (SSE or JSON), `GET /_rustybin/assist/usage`.

pub mod anthropic;
pub mod api;
pub mod gemini;
pub mod mock;
pub mod models;
pub mod openai;
pub mod provider;
pub mod redact;
pub mod task;
pub mod tasks;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};
use zeroize::Zeroizing;

pub use api::{catalog, openapi_paths, router, RUN_PATH, STATUS_PATH, TEST_PATH, USAGE_PATH};
use models::Tier;
use provider::{Client, Event, Kind, Request};
use task::Task;

use crate::config::Config;
use crate::egress::{Allowlist, Egress, Purpose, Rule};
use crate::limits::Clock;
use crate::metrics::Metrics;
use crate::settings::{Kind as SettingKind, Settings, Spec};

/// `RUSTYBIN_ASSIST_MAX_CALLS_PER_MINUTE`.
pub const DEFAULT_CALLS_PER_MINUTE: u32 = 20;
/// `RUSTYBIN_ASSIST_MAX_OUTPUT_TOKENS`.
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 8192;
/// Most (provider, model) usage series kept.
pub const MAX_USAGE_SERIES: usize = 64;
/// Most callers in the rate table.
pub const MAX_PRINCIPALS: usize = 256;
/// Most cached answers (`suggest`).
pub const MAX_CACHE: usize = 64;
pub const CACHE_TTL: Duration = Duration::from_secs(3600);
/// Repairs of a structured answer that failed validation.
pub const MAX_REPAIRS: u32 = 2;
const WINDOW: Duration = Duration::from_secs(60);

// ── Settings ────────────────────────────────────────────────────────

fn valid_provider(v: &Value) -> Result<(), String> {
    match v.as_str() {
        Some(s) if Kind::parse(s).is_some() => Ok(()),
        Some(_) => Err("expected anthropic, openai, gemini, compatible or mock".to_string()),
        None => Ok(()),
    }
}

/// The settings of AI assist.
pub fn specs() -> Vec<Spec> {
    vec![
        Spec::new(
            "assist.provider",
            "assist",
            "Provider",
            "anthropic (Messages API), openai (Chat Completions), gemini (generateContent), compatible (any OpenAI-compatible endpoint, see the base URL) or mock (the built-in answers, for demos and tests). Empty: AI assist is off.",
            SettingKind::Text { max_len: 32 },
            Value::Null,
        )
        .env("RUSTYBIN_ASSIST_PROVIDER", |c| {
            c.assist_provider.clone().map(Value::String)
        })
        .check(valid_provider),
        Spec::new(
            "assist.model",
            "assist",
            "Model",
            "The model for specs, edits and answers. Empty: the provider's default from the curated table (the compatible endpoint needs one).",
            SettingKind::Text { max_len: 128 },
            Value::Null,
        )
        .env("RUSTYBIN_ASSIST_MODEL", |c| {
            c.assist_model.clone().map(Value::String)
        }),
        Spec::new(
            "assist.fast_model",
            "assist",
            "Fast model",
            "The model for cheap tasks (suggestions, explanations). Empty: the provider's fast default, else the model above.",
            SettingKind::Text { max_len: 128 },
            Value::Null,
        )
        .env("RUSTYBIN_ASSIST_FAST_MODEL", |c| {
            c.assist_fast_model.clone().map(Value::String)
        }),
        Spec::new(
            "assist.base_url",
            "assist",
            "Base URL (compatible)",
            "The OpenAI-compatible endpoint's base, without /chat/completions (for example http://localhost:11434/v1 for Ollama; loopback needs the outbound loopback switch).",
            SettingKind::Url,
            Value::Null,
        )
        .env("RUSTYBIN_ASSIST_BASE_URL", |c| {
            c.assist_base_url.clone().map(Value::String)
        }),
        Spec::new(
            "assist.anthropic_key",
            "assist",
            "Anthropic API key",
            "Sent as x-api-key to api.anthropic.com. Stored sealed; never returned.",
            SettingKind::Text { max_len: 512 },
            Value::Null,
        )
        .env("RUSTYBIN_ASSIST_ANTHROPIC_KEY", |c| {
            c.assist_anthropic_key.clone().map(Value::String)
        })
        .secret(),
        Spec::new(
            "assist.openai_key",
            "assist",
            "OpenAI API key",
            "Sent as a bearer to api.openai.com. Stored sealed; never returned.",
            SettingKind::Text { max_len: 512 },
            Value::Null,
        )
        .env("RUSTYBIN_ASSIST_OPENAI_KEY", |c| {
            c.assist_openai_key.clone().map(Value::String)
        })
        .secret(),
        Spec::new(
            "assist.gemini_key",
            "assist",
            "Gemini API key",
            "Sent as x-goog-api-key to generativelanguage.googleapis.com. Stored sealed; never returned.",
            SettingKind::Text { max_len: 512 },
            Value::Null,
        )
        .env("RUSTYBIN_ASSIST_GEMINI_KEY", |c| {
            c.assist_gemini_key.clone().map(Value::String)
        })
        .secret(),
        Spec::new(
            "assist.compatible_key",
            "assist",
            "Compatible endpoint key",
            "Sent as a bearer to the base URL, when the endpoint wants one. Stored sealed; never returned.",
            SettingKind::Text { max_len: 512 },
            Value::Null,
        )
        .env("RUSTYBIN_ASSIST_COMPATIBLE_KEY", |c| {
            c.assist_compatible_key.clone().map(Value::String)
        })
        .secret(),
    ]
}

// ── State ───────────────────────────────────────────────────────────

/// The effective configuration, resolved from the settings at every call.
#[derive(Clone, Debug)]
pub struct Status {
    pub kind: Option<Kind>,
    pub model: String,
    pub fast_model: String,
    pub base_url: Option<String>,
    pub key_set: bool,
    pub enabled: bool,
    /// Why it is off, when it is.
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct Counters {
    calls: u64,
    errors: u64,
    input: u64,
    output: u64,
    cost: f64,
    /// A call whose cost is unknown (no list price) happened.
    unpriced: bool,
}

impl Counters {
    fn describe(&self) -> Value {
        json!({
            "calls": self.calls,
            "errors": self.errors,
            "input_tokens": self.input,
            "output_tokens": self.output,
            "estimated_cost_usd": if self.unpriced && self.cost == 0.0 { Value::Null } else { json!((self.cost * 1e6).round() / 1e6) },
            "unpriced_calls": self.unpriced,
        })
    }
}

#[derive(Default)]
struct Usage {
    by_model: BTreeMap<(String, String), Counters>,
    by_task: BTreeMap<&'static str, Counters>,
    total: Counters,
}

/// The assist engine, in `AppState.assist`.
pub struct Assist {
    settings: Arc<Settings>,
    egress: Arc<Egress>,
    metrics: Arc<Metrics>,
    clock: Arc<dyn Clock>,
    calls_per_minute: u32,
    max_output_tokens: u32,
    admin_token: Option<String>,
    public_mode: bool,
    usage: Mutex<Usage>,
    calls: Mutex<HashMap<String, VecDeque<Duration>>>,
    cache: Mutex<HashMap<String, (Duration, Value)>>,
    /// Endpoint overrides (tests: a stand-in server for a real provider).
    overrides: Mutex<HashMap<Kind, String>>,
    /// A scripted provider in place of the mock (tests).
    scripted: Mutex<Option<provider::Script>>,
    started: String,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Assist {
    pub fn new(
        config: &Config,
        settings: Arc<Settings>,
        egress: Arc<Egress>,
        metrics: Arc<Metrics>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let assist = Self {
            settings,
            egress,
            metrics,
            clock,
            calls_per_minute: config.assist_max_calls_per_minute.max(1),
            max_output_tokens: config.assist_max_output_tokens.max(1),
            admin_token: config.admin_token.clone(),
            public_mode: config.public_mode,
            usage: Mutex::new(Usage::default()),
            calls: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
            overrides: Mutex::new(HashMap::new()),
            scripted: Mutex::new(None),
            started: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        };
        assist.sync_egress();
        assist
    }

    pub fn calls_per_minute(&self) -> u32 {
        self.calls_per_minute
    }

    pub fn max_output_tokens(&self) -> u32 {
        self.max_output_tokens
    }

    /// Point a real provider's API at another base URL (tests).
    #[doc(hidden)]
    pub fn override_endpoint(&self, kind: Kind, base: &str) {
        lock(&self.overrides).insert(kind, base.trim_end_matches('/').to_string());
    }

    /// Answer from a script while the provider is `mock` (tests).
    #[doc(hidden)]
    pub fn set_script(&self, script: Option<provider::Script>) {
        *lock(&self.scripted) = script;
    }

    fn base_url(&self, kind: Kind) -> Option<String> {
        if let Some(o) = lock(&self.overrides).get(&kind) {
            return Some(o.clone());
        }
        match kind {
            Kind::Compatible => self.settings.string("assist.base_url"),
            other => other.default_base_url().map(str::to_string),
        }
    }

    fn key(&self, kind: Kind) -> Option<Zeroizing<String>> {
        kind.key_setting().and_then(|k| self.settings.secret(k))
    }

    /// The effective configuration.
    pub fn status(&self) -> Status {
        let kind = self
            .settings
            .string("assist.provider")
            .and_then(|s| Kind::parse(&s));
        let model = self
            .settings
            .string("assist.model")
            .or_else(|| kind.and_then(models::default_model).map(str::to_string))
            .unwrap_or_default();
        let fast_model = self
            .settings
            .string("assist.fast_model")
            .or_else(|| {
                kind.and_then(models::default_fast_model)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| model.clone());
        let base_url = self.settings.string("assist.base_url");
        let key_set = kind.is_some_and(|k| self.key(k).is_some());
        let reason = match kind {
            None => Some("No AI provider is set: pick one under Management, AI".to_string()),
            Some(Kind::Mock) => None,
            Some(Kind::Compatible) if base_url.is_none() => {
                Some("Set the base URL of the OpenAI-compatible endpoint".to_string())
            }
            Some(Kind::Compatible) if model.is_empty() => {
                Some("Set the model the compatible endpoint serves".to_string())
            }
            Some(Kind::Compatible) => None,
            Some(k) if !key_set => Some(format!("Add the {} key", k.as_str())),
            Some(_) => None,
        };
        Status {
            kind,
            model,
            fast_model,
            base_url,
            key_set,
            enabled: reason.is_none(),
            reason,
        }
    }

    /// Allow the hosts of every provider that has a key (and the compatible
    /// endpoint's host when its URL is set) under `Purpose::Assist`.
    pub fn sync_egress(&self) {
        let mut entries = Vec::new();
        for kind in Kind::ALL {
            let configured = match kind {
                Kind::Mock => false,
                Kind::Compatible => self.settings.string("assist.base_url").is_some(),
                other => self.key(other).is_some(),
            };
            if !configured {
                continue;
            }
            if let Some(entry) = self
                .base_url(kind)
                .and_then(|b| crate::egress::url_entry(&b))
            {
                entries.push(entry);
            }
        }
        let (allow, _) = Allowlist::parse(&entries);
        self.egress.permit(
            Purpose::Assist,
            Rule {
                allow,
                any: false,
                loopback: false,
            },
        );
    }

    /// The configured provider client, or why there is none.
    pub fn client(&self) -> Result<(Client, Status), String> {
        self.sync_egress();
        let status = self.status();
        if let Some(reason) = &status.reason {
            return Err(reason.clone());
        }
        let kind = status.kind.ok_or("no provider")?;
        let base = self.base_url(kind).unwrap_or_default();
        let client = match kind {
            Kind::Mock => match lock(&self.scripted).clone() {
                Some(script) => Client::Scripted(script),
                None => Client::Mock,
            },
            Kind::Anthropic => Client::Anthropic {
                key: self.key(kind).ok_or("no key")?,
                base,
            },
            Kind::OpenAi => Client::OpenAi {
                key: Some(self.key(kind).ok_or("no key")?),
                base,
                compatible: false,
            },
            Kind::Compatible => Client::OpenAi {
                key: self.key(kind),
                base,
                compatible: true,
            },
            Kind::Gemini => Client::Gemini {
                key: self.key(kind).ok_or("no key")?,
                base,
            },
        };
        Ok((client, status))
    }

    // ── Caps ──

    /// May `principal` make a call now? `Err(seconds)` until it may.
    pub fn admit(&self, principal: &str) -> Result<(), u64> {
        let now = self.clock.mono();
        let mut calls = lock(&self.calls);
        if !calls.contains_key(principal) && calls.len() >= MAX_PRINCIPALS {
            calls.retain(|_, q| q.back().is_some_and(|t| now.saturating_sub(*t) < WINDOW));
            if calls.len() >= MAX_PRINCIPALS {
                if let Some(oldest) = calls
                    .iter()
                    .min_by_key(|(_, q)| q.back().copied().unwrap_or_default())
                    .map(|(k, _)| k.clone())
                {
                    calls.remove(&oldest);
                }
            }
        }
        let q = calls.entry(principal.to_string()).or_default();
        while q.front().is_some_and(|t| now.saturating_sub(*t) >= WINDOW) {
            q.pop_front();
        }
        if q.len() >= self.calls_per_minute as usize {
            let wait = q
                .front()
                .map(|t| {
                    WINDOW
                        .saturating_sub(now.saturating_sub(*t))
                        .as_secs()
                        .max(1)
                })
                .unwrap_or(1);
            return Err(wait);
        }
        q.push_back(now);
        Ok(())
    }

    // ── Usage ──

    fn record(
        &self,
        kind: Kind,
        model: &str,
        task: &'static str,
        outcome: &'static str,
        input: u64,
        output: u64,
    ) {
        self.metrics
            .count_assist_call(kind.as_str(), model, task, outcome);
        self.metrics
            .record_assist_tokens(kind.as_str(), model, input, output);
        let cost = models::cost(kind, model, input, output);
        let mut usage = lock(&self.usage);
        let key = (kind.as_str().to_string(), model.to_string());
        if !usage.by_model.contains_key(&key) && usage.by_model.len() >= MAX_USAGE_SERIES {
            // Keep the series with the most calls: drop the smallest.
            if let Some(smallest) = usage
                .by_model
                .iter()
                .min_by_key(|(_, c)| c.calls)
                .map(|(k, _)| k.clone())
            {
                usage.by_model.remove(&smallest);
            }
        }
        let bump = |c: &mut Counters| {
            c.calls += 1;
            if outcome != "ok" {
                c.errors += 1;
            }
            c.input += input;
            c.output += output;
            match cost {
                Some(v) => c.cost += v,
                None => c.unpriced = true,
            }
        };
        bump(usage.by_model.entry(key).or_default());
        bump(usage.by_task.entry(task).or_default());
        bump(&mut usage.total);
    }

    /// The counters since start.
    pub fn usage(&self) -> Value {
        let usage = lock(&self.usage);
        json!({
            "since": self.started,
            "total": usage.total.describe(),
            "by_model": usage.by_model.iter().map(|((provider, model), c)| {
                let mut v = c.describe();
                if let Value::Object(m) = &mut v {
                    m.insert("provider".into(), json!(provider));
                    m.insert("model".into(), json!(model));
                }
                v
            }).collect::<Vec<_>>(),
            "by_task": usage.by_task.iter().map(|(task, c)| {
                let mut v = c.describe();
                if let Value::Object(m) = &mut v {
                    m.insert("task".into(), json!(task));
                }
                v
            }).collect::<Vec<_>>(),
            "caps": {
                "calls_per_minute": self.calls_per_minute,
                "max_output_tokens": self.max_output_tokens,
            },
        })
    }

    // ── Cache ──

    fn cached(&self, key: &str) -> Option<Value> {
        let now = self.clock.mono();
        let cache = lock(&self.cache);
        cache
            .get(key)
            .filter(|(at, _)| now.saturating_sub(*at) < CACHE_TTL)
            .map(|(_, v)| v.clone())
    }

    fn store(&self, key: String, value: Value) {
        let now = self.clock.mono();
        let mut cache = lock(&self.cache);
        cache.retain(|_, (at, _)| now.saturating_sub(*at) < CACHE_TTL);
        if cache.len() >= MAX_CACHE {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(key, (now, value));
    }

    /// Forget cached answers (a provider or model change).
    pub fn clear_cache(&self) {
        lock(&self.cache).clear();
    }
}

// ── Running a task ──────────────────────────────────────────────────

/// What a run streams to its caller.
#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    /// Exactly what goes to the provider (never the key).
    Sent(Value),
    /// Where a structured run is: calling the model, validating, repairing.
    Progress(String),
    Delta(String),
    /// The validated structured answer.
    Json(Value),
    /// An action a text answer proposed.
    Action(Value),
    Usage {
        provider: &'static str,
        model: String,
        input: u64,
        output: u64,
        cost: Option<f64>,
        cached: bool,
    },
    Error {
        message: String,
        status: u16,
    },
    Done,
}

impl Out {
    /// The SSE event name and payload.
    pub fn sse(&self) -> (&'static str, Value) {
        match self {
            Out::Sent(v) => ("sent", v.clone()),
            Out::Progress(m) => ("progress", json!({ "message": m })),
            Out::Delta(t) => ("delta", json!({ "text": t })),
            Out::Json(v) => ("json", v.clone()),
            Out::Action(v) => ("action", v.clone()),
            Out::Usage {
                provider,
                model,
                input,
                output,
                cost,
                cached,
            } => (
                "usage",
                json!({
                    "provider": provider,
                    "model": model,
                    "input_tokens": input,
                    "output_tokens": output,
                    "estimated_cost_usd": cost.map(|c| (c * 1e6).round() / 1e6),
                    "cached": cached,
                }),
            ),
            Out::Error { message, status } => {
                ("error", json!({ "error": message, "status": status }))
            }
            Out::Done => ("done", json!({})),
        }
    }
}

pub type OutStream = Pin<Box<dyn Stream<Item = Out> + Send>>;

/// Why a run did not start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No provider, or no key: the message says what to set.
    Disabled(String),
    /// The caller's calls per minute: seconds to wait.
    RateLimited(u64),
    /// Bad inputs.
    Invalid(String),
    /// The inputs hold a stored secret (named) or the admin token.
    Secret(&'static str),
}

pub struct RunParams {
    pub task: &'static Task,
    pub inputs: Value,
    /// A model id instead of the task's tier default.
    pub model: Option<String>,
    /// Who calls (for the caps).
    pub principal: String,
}

impl Assist {
    /// Start a task: the event stream, or why not.
    pub fn run(self: &Arc<Self>, params: RunParams) -> Result<OutStream, Refusal> {
        let (client, status) = self.client().map_err(Refusal::Disabled)?;
        let kind = status.kind.unwrap_or(Kind::Mock);
        let task = params.task;
        let user = (task.prompt)(&params.inputs).map_err(Refusal::Invalid)?;
        let guarded = redact::guarded(&self.settings, self.admin_token.as_deref());
        redact::check(&user, &guarded).map_err(Refusal::Secret)?;
        redact::check(task.system, &guarded).map_err(Refusal::Secret)?;
        let model = params
            .model
            .filter(|m| !m.trim().is_empty() && m.len() <= 128)
            .unwrap_or_else(|| match task.tier {
                Tier::Fast => status.fast_model.clone(),
                Tier::Capable => status.model.clone(),
            });
        if model.is_empty() {
            return Err(Refusal::Disabled("no model is set".to_string()));
        }
        self.admit(&params.principal)
            .map_err(Refusal::RateLimited)?;
        let schema = task.schema.map(|f| f());
        let cache_key = task
            .cache_key
            .and_then(|f| f(&params.inputs))
            .map(|k| format!("{}|{}|{}|{k}", task.name, kind.as_str(), model));
        let sent = json!({
            "provider": kind.as_str(),
            "model": model,
            "task": task.name,
            "system": task.system,
            "user": user,
            "schema": schema,
            "max_output_tokens": self.max_output_tokens,
        });
        if let Some(cached) = cache_key.as_ref().and_then(|k| self.cached(k)) {
            let provider = kind.as_str();
            let model_c = model.clone();
            let events = vec![
                Out::Sent(sent),
                Out::Json(cached),
                Out::Usage {
                    provider,
                    model: model_c,
                    input: 0,
                    output: 0,
                    cost: Some(0.0),
                    cached: true,
                },
                Out::Done,
            ];
            return Ok(Box::pin(futures_util::stream::iter(events)));
        }
        let mock_answer = (kind == Kind::Mock).then(|| (task.mock)(&params.inputs));
        let inputs = params.inputs;
        let this = self.clone();
        let egress = self.egress.clone();
        let max_output_tokens = self.max_output_tokens;
        let stream = async_stream::stream! {
            yield Out::Sent(sent);
            let mut attempt = 0u32;
            let mut prompt = user.clone();
            let (mut total_in, mut total_out) = (0u64, 0u64);
            loop {
                if schema.is_some() {
                    yield Out::Progress(if attempt == 0 {
                        format!("asking {} ({})", kind.as_str(), model)
                    } else {
                        format!("asking {} again (repair {attempt} of {MAX_REPAIRS})", kind.as_str())
                    });
                }
                let req = Request {
                    model: model.clone(),
                    system: task.system.to_string(),
                    user: prompt.clone(),
                    schema: schema.clone(),
                    max_output_tokens,
                    mock_answer: mock_answer.clone(),
                };
                let mut events = match client.complete(&egress, &req).await {
                    Ok(e) => e,
                    Err(p) => {
                        this.record(kind, &model, task.name, "error", total_in, total_out);
                        yield Out::Error { message: p.message, status: p.status };
                        yield Out::Done;
                        return;
                    }
                };
                let mut text = String::new();
                let (mut input, mut output) = (0u64, 0u64);
                let mut failed: Option<String> = None;
                while let Some(ev) = events.next().await {
                    match ev {
                        Event::Delta(d) => {
                            if schema.is_none() {
                                yield Out::Delta(d.clone());
                            }
                            text.push_str(&d);
                        }
                        Event::Usage { input: i, output: o } => {
                            input = input.max(i);
                            output = output.max(o);
                        }
                        Event::Error(m) => {
                            failed = Some(m);
                            break;
                        }
                    }
                }
                if output == 0 {
                    output = provider::estimate_tokens(&text);
                }
                total_in += input;
                total_out += output;
                let usage = Out::Usage {
                    provider: kind.as_str(),
                    model: model.clone(),
                    input: total_in,
                    output: total_out,
                    cost: models::cost(kind, &model, total_in, total_out),
                    cached: false,
                };
                if let Some(m) = failed {
                    this.record(kind, &model, task.name, "error", total_in, total_out);
                    yield Out::Error { message: m, status: 0 };
                    yield usage;
                    yield Out::Done;
                    return;
                }
                let Some(schema_v) = &schema else {
                    let (_, actions) = task::extract_actions(&text);
                    for a in actions {
                        yield Out::Action(a);
                    }
                    this.record(kind, &model, task.name, "ok", total_in, total_out);
                    yield usage;
                    yield Out::Done;
                    return;
                };
                yield Out::Progress("checking the answer".to_string());
                let problem = match task::parse_json_answer(&text) {
                    Ok(v) => match task::validate(&v, schema_v) {
                        Ok(()) => {
                            let checked = match task.post {
                                Some(post) => post(&inputs, v),
                                None => Ok(v),
                            };
                            match checked {
                                Ok(v) => {
                                    if let Some(k) = &cache_key {
                                        this.store(k.clone(), v.clone());
                                    }
                                    this.record(kind, &model, task.name, "ok", total_in, total_out);
                                    yield Out::Json(v);
                                    yield usage;
                                    yield Out::Done;
                                    return;
                                }
                                Err(issues) => issues,
                            }
                        }
                        Err(issues) => issues,
                    },
                    Err(e) => e,
                };
                yield Out::Progress(format!("the answer needs repair: {}", task::truncate_bytes(&problem, 400)));
                attempt += 1;
                if attempt > MAX_REPAIRS {
                    this.record(kind, &model, task.name, "invalid", total_in, total_out);
                    yield Out::Error {
                        message: format!(
                            "the model's answer did not match the task's schema after {attempt} attempts: {problem}"
                        ),
                        status: 0,
                    };
                    yield usage;
                    yield Out::Done;
                    return;
                }
                prompt = format!(
                    "{user}\n\nYour previous answer was not valid ({problem}). It was:\n{}\n\nAnswer again with one JSON object that follows the schema exactly.",
                    task::truncate_bytes(&text, 8 * 1024)
                );
            }
        };
        Ok(Box::pin(stream))
    }

    /// One short call to the provider (`POST /_rustybin/assist/test`).
    pub async fn test(self: &Arc<Self>) -> Result<Value, String> {
        let (client, status) = self.client()?;
        let kind = status.kind.unwrap_or(Kind::Mock);
        let started = std::time::Instant::now();
        let req = Request {
            model: status.model.clone(),
            system: "You are a connectivity check.".to_string(),
            user: "Reply with the single word OK.".to_string(),
            schema: None,
            // Room for a reasoning model to think and still answer.
            max_output_tokens: 256,
            mock_answer: Some(json!("OK")),
        };
        let mut events = client.complete(&self.egress, &req).await.map_err(|p| {
            self.record(kind, &status.model, "test", "error", 0, 0);
            p.message
        })?;
        let mut text = String::new();
        let (mut input, mut output) = (0, 0);
        while let Some(ev) = events.next().await {
            match ev {
                Event::Delta(d) => text.push_str(&d),
                Event::Usage {
                    input: i,
                    output: o,
                } => {
                    input = input.max(i);
                    output = output.max(o);
                }
                Event::Error(m) => {
                    self.record(kind, &status.model, "test", "error", input, output);
                    return Err(m);
                }
            }
        }
        self.record(kind, &status.model, "test", "ok", input, output);
        Ok(json!({
            "ok": true,
            "provider": kind.as_str(),
            "model": status.model,
            "latency_ms": started.elapsed().as_millis() as u64,
            "answer": text.trim().chars().take(200).collect::<String>(),
            "input_tokens": input,
            "output_tokens": output,
        }))
    }

    pub fn public_mode(&self) -> bool {
        self.public_mode
    }
}

#[cfg(test)]
mod tests;
