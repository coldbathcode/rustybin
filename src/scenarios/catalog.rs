//! The gateway scenarios: what each proves, the upstream it prepares, the
//! steps on the gateway in neutral words, the requests the check sends and
//! how it judges them. One table, read by every surface.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{json, Value};

use crate::mocks::model::{
    ApiKeySettings, MockSettings, OperationSettings, ResponseOverride, SecurityMode,
    SecuritySettings,
};

/// How long the slow upstream of `timeouts` takes to answer.
pub const SLOW_MS: u64 = if cfg!(test) { 1500 } else { 3000 };
/// The header the gateway adds in `credential_injection`.
pub const KEY_HEADER: &str = "X-Upstream-Key";
/// The rate limit the check assumes when the caller gives none.
pub const DEFAULT_LIMIT: u32 = 5;
/// The largest rate limit a check accepts (it sends twice as many requests).
pub const MAX_LIMIT: u32 = 20;
/// The half-open time the check assumes when the caller gives none.
pub const DEFAULT_HALF_OPEN_MS: u64 = 10_000;
/// The shortest and longest half-open time a check accepts.
pub const MIN_HALF_OPEN_MS: u64 = 100;
pub const MAX_HALF_OPEN_MS: u64 = 60_000;
/// The most requests a `circuit_breaker` check sends before the circuit
/// opens.
pub const MAX_BREAKER_REQUESTS: usize = 20;
/// The trial requests a `circuit_breaker` check sends after the half-open
/// wait, stopping at the first that reaches the upstream.
pub const BREAKER_TRIALS: usize = 3;
/// The body limit the check assumes when the caller gives none (1 MiB).
pub const DEFAULT_SIZE_LIMIT: u64 = 1 << 20;
/// The smallest and largest body limit a check accepts (it sends a body
/// half as large and one twice as large).
pub const MIN_SIZE_LIMIT: u64 = 16;
pub const MAX_SIZE_LIMIT: u64 = 1 << 20;

/// The origins a `cors` check sends from when the caller gives none.
pub const DEFAULT_ALLOWED_ORIGIN: &str = "https://app.example.com";
pub const DEFAULT_REFUSED_ORIGIN: &str = "https://elsewhere.example.net";
/// The headers a `header_transformation` check looks for when the caller
/// gives none.
pub const DEFAULT_ADD_REQUEST_HEADER: &str = "X-Gateway-Added";
pub const DEFAULT_REMOVE_REQUEST_HEADER: &str = "X-Client-Internal";
pub const DEFAULT_ADD_RESPONSE_HEADER: &str = "X-Gateway-Response";
/// The longest origin, header name and header value a check accepts.
pub const MAX_ORIGIN_LEN: usize = 200;
pub const MAX_HEADER_NAME_LEN: usize = 64;
pub const MAX_HEADER_VALUE_LEN: usize = 200;
/// The labels of a `cors` check's requests.
pub const PREFLIGHT_ALLOWED: &str = "preflight from allowed_origin";
pub const PREFLIGHT_REFUSED: &str = "preflight from refused_origin";
pub const SIMPLE_ALLOWED: &str = "GET from allowed_origin";

/// The upstream of the AI scenarios: this instance's OpenAI chat
/// completions, and the second target of `ai_model_fallback` and
/// `ai_provider_balance` (the same API at another path, so no format
/// translation is needed and the check tells the targets apart by path).
pub const AI_PRIMARY_PATH: &str = "/ai/openai/v1/chat/completions";
pub const AI_SECOND_PATH: &str = "/ai/v1/chat/completions";
/// The model and completion size of every AI check request.
pub const AI_MODEL: &str = "gpt-4o-mini";
pub const AI_MAX_TOKENS: u64 = 100;
/// The token budget the check assumes when the caller gives none, and its
/// bounds.
pub const DEFAULT_TOKEN_LIMIT: u64 = 500;
pub const MIN_TOKEN_LIMIT: u64 = 100;
pub const MAX_TOKEN_LIMIT: u64 = 2000;
/// The most requests an `ai_token_limit` check sends.
pub const MAX_TOKEN_REQUESTS: usize = 40;
/// The requests of an `ai_provider_balance` check, and the share each
/// target needs when the caller gives none (percent).
pub const BALANCE_REQUESTS: usize = 10;
pub const DEFAULT_MIN_SHARE: u64 = 20;
/// The native errors the primary target of `ai_model_fallback` answers.
pub const FAIL_KINDS: &[&str] = &[
    "rate_limit",
    "overloaded",
    "server_error",
    "unavailable",
    "timeout",
];
/// The fake personal data the guard scenarios plant (the AI mock's scripted
/// customer record answers the same values), named by kind.
pub const PLANTED: &[(&str, &str)] = &[
    ("SSN", "123-45-6789"),
    ("card number", "4111 1111 1111 1111"),
    ("card number", "4111111111111111"),
    ("email", "jane.doe@example.com"),
    ("phone", "+1-555-0100"),
];
/// What marks the planted prompt injection (`crate::mcp::tools::INJECTION_SAMPLE`
/// starts with it), compared without case.
pub const INJECTION_MARK: &str = "ignore all previous instructions";
/// The prompts of the AI checks: `{n}` is a word unique to the check, so a
/// cache filled by an earlier check does not answer this one.
pub const PROMPT_LONG: &str = "Write a long article about API gateways. Reference {n}.";
/// The questions of the `ai_semantic_cache` checks, one a check in turn
/// (from a point chosen per run), so a cache an earlier check filled does
/// not answer the next: a semantic cache rightly takes the same question
/// with another reference word for the same prompt. Each is asked as is,
/// in other case and punctuation, and with a word more ([`cache_prompts`]).
pub const CACHE_TOPICS: &[&str] = &[
    "what is the refund policy for damaged items",
    "how long does standard shipping take to canada",
    "which payment methods does the online store accept",
    "can i change the delivery address after ordering",
    "how do i reset the password of my account",
    "what are the opening hours of the flagship store",
    "is there a warranty on refurbished laptops",
    "how do i cancel a monthly subscription plan",
    "where can i download my past invoices",
    "do you offer discounts for students and teachers",
    "how can i track a parcel sent by express courier",
    "what happens if a gift card expires",
];
/// The different prompt of each `ai_semantic_cache` check (none of them
/// near a question of [`CACHE_TOPICS`]).
pub const CACHE_OTHERS: &[&str] = &[
    "explain the rules of cricket to a beginner",
    "suggest a vegetarian recipe with chickpeas and spinach",
    "summarise the plot of a famous detective novel",
    "list three exercises that strengthen the lower back",
    "describe how volcanoes form along tectonic plates",
    "recommend houseplants that survive in low light",
    "translate good morning into five languages",
    "compare electric and petrol lawn mowers",
    "outline a two day walking tour of rome",
    "write a haiku about autumn rain on a lake",
    "give tips for learning to play the cello",
    "explain why the sky looks blue at noon",
];

/// The four prompts of an `ai_semantic_cache` check of `round`: a
/// question, the same in other case and punctuation, the same with a word
/// more, and a different one. `word` is the check's own word.
pub fn cache_prompts(round: usize, word: &str) -> [String; 4] {
    let topic = CACHE_TOPICS[round % CACHE_TOPICS.len()];
    let other = CACHE_OTHERS[round % CACHE_OTHERS.len()];
    [
        format!("{topic} reference {word}"),
        format!("{}? Reference {word}.", capitalised(topic)),
        format!("Please, {topic}? Reference {word}."),
        format!("{}? Reference {word}.", capitalised(other)),
    ]
}

/// `text` with its first letter upper case.
fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
pub const PROMPT_CLEAN: &str = "What are the opening hours of the support desk? Reference {n}.";
pub const PROMPT_PII: &str = "Update my billing: my SSN is 123-45-6789, my card is 4111 1111 1111 1111 and my email is jane.doe@example.com. Reference {n}.";
pub const PROMPT_RECORD: &str = "Show me the customer record for account 42. Reference {n}.";
/// The labels of the AI checks' requests.
pub const LABEL_CACHE_FIRST: &str = "first prompt";
pub const LABEL_CACHE_CASE: &str = "same prompt, other case and punctuation";
pub const LABEL_CACHE_PLEASE: &str = "same prompt, reworded";
pub const LABEL_CACHE_OTHER: &str = "a different prompt";
pub const LABEL_CLEAN: &str = "clean prompt";
pub const LABEL_PII: &str = "prompt with personal data";
pub const LABEL_INJECTION: &str = "prompt injection";

/// The MCP protocol version and request labels of the MCP checks.
pub const MCP_VERSION: &str = "2025-06-18";
pub const LABEL_INITIALIZE: &str = "initialize";
pub const LABEL_INITIALIZED: &str = "notifications/initialized";
pub const LABEL_LIST: &str = "tools/list";
pub const LABEL_FIRST_LIST: &str = "tools/list before the call";
pub const LABEL_CALL: &str = "tools/call";
pub const LABEL_SECOND_LIST: &str = "tools/list after the call";
/// The result size a check assumes the gateway caps when the caller gives
/// none, and its bounds (KiB; the oversized result is 2 MiB).
pub const DEFAULT_MAX_RESULT_KIB: u64 = 256;
pub const MIN_MAX_RESULT_KIB: u64 = 16;
pub const MAX_MAX_RESULT_KIB: u64 = 1024;
/// The trusted MCP server of `mcp_tool_shadowing`.
pub const SHADOWED_SERVER_PATH: &str = "/mcp/servers/crm";

/// What a test parameter holds, with its bounds and default.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    /// An integer in `min..=max`.
    Integer { min: u64, max: u64, default: u64 },
    /// `true` or `false`.
    Bool { default: bool },
    /// An origin, `scheme://host[:port]` (http or https).
    Origin { default: &'static str },
    /// A header name a check sends or looks for.
    HeaderName { default: &'static str },
    /// A header value; optional (no default).
    HeaderValue,
    /// One of a fixed set of words.
    Choice {
        options: &'static [&'static str],
        default: &'static str,
    },
}

/// A test parameter of a check.
#[derive(Clone, Copy, Debug)]
pub struct Param {
    pub name: &'static str,
    /// A short label (the console's field).
    pub label: &'static str,
    pub description: &'static str,
    pub kind: Kind,
}

impl Param {
    /// The bounds and default in words.
    fn bounds(&self) -> String {
        match self.kind {
            Kind::Integer { min, max, default } => format!("{min} to {max}, default {default}"),
            Kind::Bool { default } => format!("true or false, default {default}"),
            Kind::Origin { default } => format!(
                "an origin scheme://host[:port], at most {MAX_ORIGIN_LEN} characters, default {default}"
            ),
            Kind::HeaderName { default } => format!(
                "a header name, at most {MAX_HEADER_NAME_LEN} characters, default {default}"
            ),
            Kind::HeaderValue => {
                format!("optional, at most {MAX_HEADER_VALUE_LEN} visible characters")
            }
            Kind::Choice { options, default } => {
                format!("one of {}, default {default}", options.join(", "))
            }
        }
    }

    /// The JSON Schema of its value.
    fn schema(&self) -> serde_json::Map<String, Value> {
        let v = match self.kind {
            Kind::Integer { min, max, default } => {
                json!({ "type": "integer", "minimum": min, "maximum": max, "default": default })
            }
            Kind::Bool { default } => json!({ "type": "boolean", "default": default }),
            Kind::Origin { default } => json!({
                "type": "string", "format": "origin", "maxLength": MAX_ORIGIN_LEN, "default": default
            }),
            Kind::HeaderName { default } => json!({
                "type": "string", "format": "header-name", "maxLength": MAX_HEADER_NAME_LEN, "default": default
            }),
            Kind::HeaderValue => json!({ "type": "string", "maxLength": MAX_HEADER_VALUE_LEN }),
            Kind::Choice { options, default } => {
                json!({ "type": "string", "enum": options, "default": default })
            }
        };
        match v {
            Value::Object(m) => m,
            _ => serde_json::Map::new(),
        }
    }

    fn view(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("name".into(), json!(self.name));
        m.insert("label".into(), json!(self.label));
        m.insert(
            "description".into(),
            json!(format!("{} ({})", self.description, self.bounds())),
        );
        m.extend(self.schema());
        Value::Object(m)
    }

    /// The refusal of a value not of the parameter's kind, or out of its
    /// bounds.
    fn refuse(&self) -> String {
        format!("{}: {} ({})", self.name, self.bounds(), self.description)
    }

    /// The value of `v` for this parameter, checked (origins and header
    /// names normalised).
    fn parse(&self, v: &Value) -> Result<Value, String> {
        match self.kind {
            Kind::Integer { min, max, .. } => v
                .as_u64()
                .or_else(|| {
                    v.as_f64()
                        .filter(|f| f.fract() == 0.0 && *f >= 0.0)
                        .map(|f| f as u64)
                })
                .filter(|n| (min..=max).contains(n))
                .map(|n| json!(n))
                .ok_or_else(|| self.refuse()),
            Kind::Bool { .. } => v.as_bool().map(Value::Bool).ok_or_else(|| self.refuse()),
            Kind::Origin { .. } => {
                let s = v.as_str().ok_or_else(|| self.refuse())?;
                origin(s)
                    .map(Value::String)
                    .map_err(|e| format!("{}: {e}", self.name))
            }
            Kind::HeaderName { .. } => {
                let s = v.as_str().ok_or_else(|| self.refuse())?;
                header_name(s)
                    .map(Value::String)
                    .map_err(|e| format!("{}: {e}", self.name))
            }
            Kind::HeaderValue => {
                let s = v.as_str().ok_or_else(|| self.refuse())?;
                let visible = s.bytes().all(|b| (0x20..0x7f).contains(&b));
                if s.is_empty()
                    || s.len() > MAX_HEADER_VALUE_LEN
                    || s.trim() != s
                    || !visible
                    || axum::http::HeaderValue::from_str(s).is_err()
                {
                    return Err(self.refuse());
                }
                Ok(Value::String(s.to_string()))
            }
            Kind::Choice { options, .. } => v
                .as_str()
                .and_then(|s| options.iter().find(|o| **o == s))
                .map(|o| Value::String(o.to_string()))
                .ok_or_else(|| self.refuse()),
        }
    }
}

/// An origin as a browser sends it: `scheme://host[:port]`, http or https,
/// lowercased, without a default port, a path, a query or credentials.
pub fn origin(s: &str) -> Result<String, String> {
    let bad = || {
        format!(
            "not an origin scheme://host[:port] of at most {MAX_ORIGIN_LEN} characters, such as {DEFAULT_ALLOWED_ORIGIN}"
        )
    };
    if s.is_empty() || s.len() > MAX_ORIGIN_LEN || s.trim() != s || s.ends_with('/') {
        return Err(bad());
    }
    let url = reqwest::Url::parse(s).map_err(|_| bad())?;
    let serialized = url.origin().ascii_serialization();
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !s.eq_ignore_ascii_case(&serialized)
    {
        return Err(bad());
    }
    Ok(serialized)
}

/// Header names a transformation check may not name: set by HTTP itself,
/// the client, the upstream or the check.
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "content-type",
    "content-encoding",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "te",
    "trailer",
    "upgrade",
    "proxy-connection",
    "proxy-authorization",
    "proxy-authenticate",
    "date",
    "server",
    "vary",
    "origin",
    "x-request-id",
];
const RESERVED_PREFIXES: &[&str] = &["x-rustybin-", "access-control-"];

/// A header name a check can send or look for, lowercased.
pub fn header_name(s: &str) -> Result<String, String> {
    if s.is_empty() || s.len() > MAX_HEADER_NAME_LEN {
        return Err(format!(
            "a header name of 1 to {MAX_HEADER_NAME_LEN} characters"
        ));
    }
    let name = axum::http::HeaderName::from_bytes(s.as_bytes())
        .map_err(|_| format!("{s:?} is not a header name (letters, digits and !#$%&'*+-.^_`|~)"))?;
    let lower = name.as_str().to_string();
    if RESERVED_HEADERS.contains(&lower.as_str())
        || RESERVED_PREFIXES.iter().any(|p| lower.starts_with(p))
    {
        return Err(format!(
            "{s} is set by HTTP, the client, the upstream or the check itself: name another header, such as {DEFAULT_ADD_REQUEST_HEADER}"
        ));
    }
    Ok(lower)
}

pub const LIMIT: Param = Param {
    name: "limit",
    label: "Limit set on the gateway",
    description: "The limit set on the gateway: the check sends twice as many requests",
    kind: Kind::Integer {
        min: 1,
        max: MAX_LIMIT as u64,
        default: DEFAULT_LIMIT as u64,
    },
};
pub const HALF_OPEN_MS: Param = Param {
    name: "half_open_ms",
    label: "Half-open time on the gateway (ms)",
    description: "How long the gateway keeps the circuit open before it lets a trial request through, in milliseconds: the check waits that long",
    kind: Kind::Integer {
        min: MIN_HALF_OPEN_MS,
        max: MAX_HALF_OPEN_MS,
        default: DEFAULT_HALF_OPEN_MS,
    },
};
pub const SIZE_LIMIT: Param = Param {
    name: "size_limit",
    label: "Body limit on the gateway (bytes)",
    description: "The request body limit set on the gateway, in bytes: the check sends a body half as large and one twice as large",
    kind: Kind::Integer {
        min: MIN_SIZE_LIMIT,
        max: MAX_SIZE_LIMIT,
        default: DEFAULT_SIZE_LIMIT,
    },
};
pub const ALLOWED_ORIGIN: Param = Param {
    name: "allowed_origin",
    label: "Origin the gateway allows",
    description:
        "An origin the gateway's CORS policy allows: the check sends a preflight and a GET from it",
    kind: Kind::Origin {
        default: DEFAULT_ALLOWED_ORIGIN,
    },
};
pub const REFUSED_ORIGIN: Param = Param {
    name: "refused_origin",
    label: "Origin the gateway refuses",
    description:
        "An origin the gateway's CORS policy does not allow: the check sends a preflight from it",
    kind: Kind::Origin {
        default: DEFAULT_REFUSED_ORIGIN,
    },
};
pub const UPSTREAM_CORS: Param = Param {
    name: "upstream_cors",
    label: "The upstream answers CORS",
    description: "Whether the gateway lets preflights through to the upstream, which answers CORS for allowed_origin (the run's upstream does so then), instead of answering them itself",
    kind: Kind::Bool { default: false },
};
pub const ADD_REQUEST_HEADER: Param = Param {
    name: "add_request_header",
    label: "Request header the gateway adds",
    description: "A request header the gateway adds on the way to the upstream",
    kind: Kind::HeaderName {
        default: DEFAULT_ADD_REQUEST_HEADER,
    },
};
pub const ADD_REQUEST_VALUE: Param = Param {
    name: "add_request_value",
    label: "Its value (optional)",
    description: "The value the gateway gives add_request_header; any value passes when not given",
    kind: Kind::HeaderValue,
};
pub const REMOVE_REQUEST_HEADER: Param = Param {
    name: "remove_request_header",
    label: "Request header the gateway removes",
    description: "A request header the gateway removes before the upstream: the check sends it",
    kind: Kind::HeaderName {
        default: DEFAULT_REMOVE_REQUEST_HEADER,
    },
};
pub const ADD_RESPONSE_HEADER: Param = Param {
    name: "add_response_header",
    label: "Response header the gateway adds",
    description: "A response header the gateway adds on the way back to the client",
    kind: Kind::HeaderName {
        default: DEFAULT_ADD_RESPONSE_HEADER,
    },
};

pub const TOKEN_LIMIT: Param = Param {
    name: "token_limit",
    label: "Token budget on the gateway",
    description: "The tokens the gateway lets the route use in its window (prompt and completion): the check sends requests until the gateway refuses one",
    kind: Kind::Integer {
        min: MIN_TOKEN_LIMIT,
        max: MAX_TOKEN_LIMIT,
        default: DEFAULT_TOKEN_LIMIT,
    },
};
pub const FAIL_KIND: Param = Param {
    name: "fail_kind",
    label: "How the primary target fails",
    description: "The provider error the primary target answers every request with (rate_limit 429, overloaded and unavailable 503, server_error 500, timeout 504)",
    kind: Kind::Choice {
        options: FAIL_KINDS,
        default: "rate_limit",
    },
};
pub const SKIP_INJECTION: Param = Param {
    name: "skip_injection",
    label: "Leave out the prompt injection",
    description:
        "Whether the check leaves out the prompt injection, for a guard of personal data only",
    kind: Kind::Bool { default: false },
};
pub const MIN_SHARE: Param = Param {
    name: "min_share",
    label: "Least share per target (%)",
    description: "The least share of the requests each target must receive, in percent (20 suits an even split and a weighted one down to 80/20)",
    kind: Kind::Integer {
        min: 1,
        max: 50,
        default: DEFAULT_MIN_SHARE,
    },
};

pub const MAX_RESULT_KIB: Param = Param {
    name: "max_result_kib",
    label: "Result size limit on the gateway (KiB)",
    description: "The largest tool result the gateway passes to the client, in KiB: the attack server answers 2 MiB",
    kind: Kind::Integer {
        min: MIN_MAX_RESULT_KIB,
        max: MAX_MAX_RESULT_KIB,
        default: DEFAULT_MAX_RESULT_KIB,
    },
};

/// Every test parameter, whatever the scenario (the MCP tool, the CLI and
/// the OpenAPI document list these).
pub const PARAMS: &[Param] = &[
    LIMIT,
    HALF_OPEN_MS,
    SIZE_LIMIT,
    ALLOWED_ORIGIN,
    REFUSED_ORIGIN,
    UPSTREAM_CORS,
    ADD_REQUEST_HEADER,
    ADD_REQUEST_VALUE,
    REMOVE_REQUEST_HEADER,
    ADD_RESPONSE_HEADER,
    TOKEN_LIMIT,
    FAIL_KIND,
    SKIP_INJECTION,
    MIN_SHARE,
    MAX_RESULT_KIB,
];

/// The scenarios that take a parameter.
fn owners(name: &str) -> String {
    SCENARIOS
        .iter()
        .filter(|s| s.params.iter().any(|q| q.name == name))
        .map(|s| s.id)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The JSON Schema properties of every test parameter (the check's body,
/// the MCP tool): `<scenario> only: <description>`.
pub fn param_properties() -> serde_json::Map<String, Value> {
    PARAMS
        .iter()
        .map(|p| {
            let mut schema = p.schema();
            schema.insert(
                "description".into(),
                json!(format!(
                    "{} only: {} ({})",
                    owners(p.name),
                    p.description,
                    p.bounds()
                )),
            );
            (p.name.to_string(), Value::Object(schema))
        })
        .collect()
}

/// The test parameters of one check, each resolved to its value.
#[derive(Clone, Debug)]
pub struct Params {
    pub limit: u32,
    pub half_open_ms: u64,
    pub size_limit: u64,
    pub allowed_origin: String,
    pub refused_origin: String,
    pub upstream_cors: bool,
    /// Header names are lowercased.
    pub add_request_header: String,
    pub add_request_value: Option<String>,
    pub remove_request_header: String,
    pub add_response_header: String,
    pub token_limit: u64,
    /// One of [`FAIL_KINDS`].
    pub fail_kind: &'static str,
    pub skip_injection: bool,
    pub min_share: u64,
    pub max_result_kib: u64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            limit: DEFAULT_LIMIT,
            half_open_ms: DEFAULT_HALF_OPEN_MS,
            size_limit: DEFAULT_SIZE_LIMIT,
            allowed_origin: DEFAULT_ALLOWED_ORIGIN.to_string(),
            refused_origin: DEFAULT_REFUSED_ORIGIN.to_string(),
            upstream_cors: false,
            add_request_header: DEFAULT_ADD_REQUEST_HEADER.to_ascii_lowercase(),
            add_request_value: None,
            remove_request_header: DEFAULT_REMOVE_REQUEST_HEADER.to_ascii_lowercase(),
            add_response_header: DEFAULT_ADD_RESPONSE_HEADER.to_ascii_lowercase(),
            token_limit: DEFAULT_TOKEN_LIMIT,
            fail_kind: "rate_limit",
            skip_injection: false,
            min_share: DEFAULT_MIN_SHARE,
            max_result_kib: DEFAULT_MAX_RESULT_KIB,
        }
    }
}

impl Params {
    /// Set one checked value (from [`Param::parse`]).
    fn set(&mut self, name: &str, v: Value) {
        let text = || v.as_str().unwrap_or_default().to_string();
        match name {
            "limit" => self.limit = v.as_u64().unwrap_or_default() as u32,
            "half_open_ms" => self.half_open_ms = v.as_u64().unwrap_or_default(),
            "size_limit" => self.size_limit = v.as_u64().unwrap_or_default(),
            "allowed_origin" => self.allowed_origin = text(),
            "refused_origin" => self.refused_origin = text(),
            "upstream_cors" => self.upstream_cors = v.as_bool().unwrap_or_default(),
            "add_request_header" => self.add_request_header = text(),
            "add_request_value" => self.add_request_value = Some(text()),
            "remove_request_header" => self.remove_request_header = text(),
            "add_response_header" => self.add_response_header = text(),
            "token_limit" => self.token_limit = v.as_u64().unwrap_or_default(),
            "fail_kind" => {
                if let Some(k) = FAIL_KINDS.iter().find(|k| v.as_str() == Some(**k)) {
                    self.fail_kind = k;
                }
            }
            "skip_injection" => self.skip_injection = v.as_bool().unwrap_or_default(),
            "min_share" => self.min_share = v.as_u64().unwrap_or_default(),
            "max_result_kib" => self.max_result_kib = v.as_u64().unwrap_or_default(),
            _ => {}
        }
    }
}

/// What the gateway's route points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Upstream {
    /// A mock the run creates (`scn-<run>`), removed when the run ends.
    Mock,
    /// A `/flaky` route of this instance; the run's counters are reset when
    /// it ends.
    Flaky(&'static str),
    /// This instance's AI chat completions ([`AI_PRIMARY_PATH`]), steered
    /// for the run's own requests only by the run's [`AiProfile`]; with
    /// `second`, a second target at [`AI_SECOND_PATH`].
    Ai { second: bool },
    /// The MCP attack server of `kind` (`/mcp/attack/{kind}`); with
    /// `second`, a trusted MCP server of this instance at that path.
    Mcp {
        kind: &'static str,
        second: Option<&'static str>,
    },
}

/// How the AI mock answers the requests of one AI scenario run (and only
/// those: they carry the run's secret session). Kept in the run, so ending
/// the run ends it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AiProfile {
    /// Requests to [`AI_PRIMARY_PATH`] fail with this kind (one of
    /// [`FAIL_KINDS`]).
    pub fail_primary: Option<&'static str>,
    /// The reply mode (an AI mock mode name), over the request's own.
    pub mode: Option<&'static str>,
}

pub struct Scenario {
    pub id: &'static str,
    pub title: &'static str,
    /// What a pass proves about the gateway.
    pub proves: &'static str,
    /// The steps on the gateway: `{upstream}`, `{key}`, `{slow_ms}` are
    /// filled per run.
    pub setup: &'static [&'static str],
    /// Appended to the gateway URL by the check.
    pub request_path: &'static str,
    pub upstream: Upstream,
    /// The test parameters a check of it takes.
    pub params: &'static [Param],
}

const ROUTE: &str = "Create a route on the gateway whose upstream is {upstream}, and let it forward the X-Rustybin-Session request header (the check uses it to see which upstream requests each of its requests caused).";
const AI_ROUTE: &str = "Create an AI route on the gateway (OpenAI chat completions format) whose upstream is {upstream}, and let it forward the X-Rustybin-Session request header (the check uses it to see which upstream requests each of its requests caused). The upstream needs no credential.";
const MCP_ROUTE: &str = "Put the MCP server at {upstream} behind the gateway (Streamable HTTP), and let it forward the X-Rustybin-Session request header (the check uses it to see which upstream requests each of its requests caused).";
const MCP_CHECK: &str = "Check with the URL the gateway serves the MCP server at: the check starts a session (initialize, protocol 2025-06-18) and sends its requests in it.";
const AI_CHECK: &str = "Check with the URL the gateway serves the route's chat completions at: the check sends POST requests in the OpenAI chat completions format (model gpt-4o-mini, max_tokens 100).";

pub static SCENARIOS: &[Scenario] = &[
    Scenario {
        id: "retries",
        title: "Retries",
        proves: "The gateway retries a request the upstream fails with 503, and the client sees the success.",
        setup: &[
            ROUTE,
            "Turn on retries for the route: retry a request answered 503 at least once.",
            "Check with the URL the gateway serves the route at.",
        ],
        request_path: "",
        upstream: Upstream::Flaky("/flaky/recover/1"),
        params: &[],
    },
    Scenario {
        id: "rate_limiting",
        title: "Rate limiting",
        proves: "The gateway answers 429 once a client passes the limit, and the requests over it never reach the upstream.",
        setup: &[
            ROUTE,
            "Limit the route to a few requests per minute (5, say), counted per client or for the whole route.",
            "Check with the route's URL and the limit you set (the check sends twice the limit; wait for the window to pass before checking again).",
        ],
        request_path: "/items",
        upstream: Upstream::Mock,
        params: &[LIMIT],
    },
    Scenario {
        id: "credential_injection",
        title: "Credential injection",
        proves: "Clients call without a credential and the gateway adds the upstream's own: the upstream accepts every request.",
        setup: &[
            ROUTE,
            "Have the gateway add the request header X-Upstream-Key: {key} to every request it forwards. Clients send no credential.",
            "Check with the route's URL.",
        ],
        request_path: "/profile",
        upstream: Upstream::Mock,
        params: &[],
    },
    Scenario {
        id: "timeouts",
        title: "Upstream timeouts",
        proves: "The gateway stops waiting for a slow upstream and answers a gateway error before the upstream would have.",
        setup: &[
            ROUTE,
            "Set the route's upstream read timeout under {slow_ms} ms (1 second, say): the upstream answers after {slow_ms} ms.",
            "Check with the route's URL.",
        ],
        request_path: "/slow",
        upstream: Upstream::Mock,
        params: &[],
    },
    Scenario {
        id: "caching",
        title: "Response caching",
        proves: "The gateway serves repeated GETs from its cache: only the first reaches the upstream.",
        setup: &[
            ROUTE,
            "Turn on response caching for GET on the route (the upstream answers Cache-Control: public, max-age=300).",
            "Check with the route's URL.",
        ],
        request_path: "/catalog",
        upstream: Upstream::Mock,
        params: &[],
    },
    Scenario {
        id: "circuit_breaker",
        title: "Circuit breaker",
        proves: "The gateway stops forwarding to an upstream that keeps failing and answers on its own, then lets a trial request through after the half-open time.",
        setup: &[
            ROUTE,
            "Turn on a circuit breaker for the route (passive health checks on its upstream): open it after a few failed requests in a row (the upstream answers 503 to every request; the check sends at most 20), answer on its own while it is open, and let a trial request through after a half-open time (10 seconds, say).",
            "Check with the route's URL and the half-open time you set (half_open_ms): the check sends requests until the gateway stops forwarding them, waits that long, then sends a trial. Wait for the circuit to close or open again before checking again.",
        ],
        request_path: "",
        upstream: Upstream::Flaky("/flaky/after/0"),
        params: &[HALF_OPEN_MS],
    },
    Scenario {
        id: "request_size_limit",
        title: "Request size limit",
        proves: "The gateway refuses a request body over the limit with 413 before the upstream sees it, and forwards one under it.",
        setup: &[
            ROUTE,
            "Limit the size of request bodies on the route (1 MiB, say), so the gateway answers 413 to a larger body without forwarding it.",
            "Check with the route's URL and the limit you set, in bytes (size_limit): the check sends POST /upload with a body half as large, then one twice as large.",
        ],
        request_path: "/upload",
        upstream: Upstream::Mock,
        params: &[SIZE_LIMIT],
    },
    Scenario {
        id: "cors",
        title: "CORS",
        proves: "The gateway answers a CORS preflight from an allowed origin (and not from another), and a request from the allowed origin gets Access-Control-Allow-Origin.",
        setup: &[
            ROUTE,
            "Turn on CORS for the route: allow one origin (https://app.example.com, say) and the GET method, allow no other origin, and let the gateway answer preflight requests itself. The run's upstream answers no CORS of its own.",
            "Or, when the upstream should answer CORS and the gateway only lets preflights through to it, check with upstream_cors: true: the run's upstream then answers CORS for allowed_origin.",
            "Check with the route's URL and the origins (allowed_origin, and refused_origin, one the gateway does not allow): the check sends an OPTIONS preflight from each, then a GET from allowed_origin.",
        ],
        request_path: "/cors",
        upstream: Upstream::Mock,
        params: &[ALLOWED_ORIGIN, REFUSED_ORIGIN, UPSTREAM_CORS],
    },
    Scenario {
        id: "header_transformation",
        title: "Header transformation",
        proves: "The gateway adds a request header on the way to the upstream, removes one the client sent, and adds a response header on the way back.",
        setup: &[
            ROUTE,
            "Have the gateway add a request header to every request it forwards (X-Gateway-Added, say, with any value), remove one the client sends (X-Client-Internal, say), and add a response header to every answer (X-Gateway-Response, say).",
            "Check with the route's URL and the header names you used (add_request_header, remove_request_header, add_response_header; add_request_value when the value matters): the check sends a GET with the header to remove, then reads what reached the upstream and the answer.",
        ],
        request_path: "/headers",
        upstream: Upstream::Mock,
        params: &[
            ADD_REQUEST_HEADER,
            ADD_REQUEST_VALUE,
            REMOVE_REQUEST_HEADER,
            ADD_RESPONSE_HEADER,
        ],
    },
    Scenario {
        id: "ai_token_limit",
        title: "AI token rate limiting",
        proves: "The gateway limits an AI route by the tokens it uses, not by its requests: once the budget is spent it answers 429, and the refused requests never reach the model.",
        setup: &[
            AI_ROUTE,
            "Limit the route by tokens (prompt plus completion, from the usage the model answers), a few hundred per minute (500, say), counted per client or for the whole route. Each check request uses a little over 100 tokens.",
            AI_CHECK,
            "Check with the token budget you set (token_limit): the check sends requests until the gateway refuses one. Wait for the window to pass before checking again.",
        ],
        request_path: "",
        upstream: Upstream::Ai { second: false },
        params: &[TOKEN_LIMIT],
    },
    Scenario {
        id: "ai_model_fallback",
        title: "AI model fallback",
        proves: "When the primary model answers a provider error (429 by default), the gateway retries the request on a fallback target and the client sees a success.",
        setup: &[
            AI_ROUTE,
            "Give the route a fallback: on an error from {upstream} (429, 5xx), send the request to {second_upstream} (the same API at another path; any other provider path of this instance works too). For this run, {upstream} answers every request with the error of fail_kind.",
            AI_CHECK,
            "Check with the kind of error the primary target should answer (fail_kind, rate_limit by default).",
        ],
        request_path: "",
        upstream: Upstream::Ai { second: true },
        params: &[FAIL_KIND],
    },
    Scenario {
        id: "ai_semantic_cache",
        title: "AI semantic caching",
        proves: "The gateway answers prompts that mean the same from its cache (other case, punctuation, a word more): only the first reaches the model, and a different prompt still does.",
        setup: &[
            AI_ROUTE,
            "Turn on semantic caching for the route, with a similarity threshold of 0.9 or less. When the gateway needs embeddings, this instance serves them at {base}/ai/openai/v1/embeddings (similar texts get close vectors).",
            AI_CHECK,
            "The check sends a prompt, the same prompt in other case and punctuation, the same prompt with a word more, then a different prompt. Each check asks another question (twelve in turn), so what an earlier check left in the cache does not answer it.",
        ],
        request_path: "",
        upstream: Upstream::Ai { second: false },
        params: &[],
    },
    Scenario {
        id: "ai_prompt_guard",
        title: "AI prompt guard",
        proves: "The gateway stops personal data and a prompt injection on the way to the model, by refusing the prompt or redacting it, and lets a clean prompt through.",
        setup: &[
            AI_ROUTE,
            "Turn on a prompt guard for the route that refuses or redacts personal data (SSN, card numbers, email addresses) and refuses prompt injections (\"ignore all previous instructions\").",
            AI_CHECK,
            "The check sends a clean prompt, a prompt with fake personal data (SSN 123-45-6789, card 4111 1111 1111 1111, jane.doe@example.com) and, unless skip_injection is set, a prompt injection.",
        ],
        request_path: "",
        upstream: Upstream::Ai { second: false },
        params: &[SKIP_INJECTION],
    },
    Scenario {
        id: "ai_response_guard",
        title: "AI response guard",
        proves: "The gateway removes personal data from what the model answers before the client sees it.",
        setup: &[
            AI_ROUTE,
            "Turn on a response guard for the route that redacts or blocks personal data (SSN, card numbers, email addresses, phone numbers) in answers. For this run, {upstream} answers with a fake customer record.",
            AI_CHECK,
        ],
        request_path: "",
        upstream: Upstream::Ai { second: false },
        params: &[],
    },
    Scenario {
        id: "ai_provider_balance",
        title: "AI load balancing",
        proves: "The gateway spreads an AI route's requests across two targets, by its balancing rule, and every request succeeds.",
        setup: &[
            AI_ROUTE,
            "Give the route two targets, {upstream} and {second_upstream} (the same API at another path), balanced round robin or by weight.",
            AI_CHECK,
            "Check with the least share of the requests each target should get (min_share, 20 percent by default): the check sends 10 requests.",
        ],
        request_path: "",
        upstream: Upstream::Ai { second: true },
        params: &[MIN_SHARE],
    },
    Scenario {
        id: "mcp_tool_poisoning",
        title: "MCP tool poisoning",
        proves: "The gateway keeps a tool whose description hides instructions for the model from the client (removed, refused or rewritten), and still passes the clean tool next to it.",
        setup: &[
            MCP_ROUTE,
            "Turn on the gateway's protection against poisoned tool descriptions (hidden instructions, HTML comments, <IMPORTANT> blocks in descriptions and parameter descriptions). {upstream} lists a clean get_weather and a poisoned add_numbers.",
            MCP_CHECK,
        ],
        request_path: "",
        upstream: Upstream::Mcp { kind: "poisoned_description", second: None },
        params: &[],
    },
    Scenario {
        id: "mcp_rug_pull",
        title: "MCP rug pull",
        proves: "When a tool's definition changes after the client approved it (hidden instructions and a new parameter), the gateway keeps the approved definition or refuses the changed one.",
        setup: &[
            MCP_ROUTE,
            "Turn on the gateway's pinning of tool definitions (or its detection of changed definitions). {upstream} lists a harmless get_exchange_rate; after its first call, the tool comes back changed.",
            MCP_CHECK,
            "The check lists the tools, calls get_exchange_rate, and lists them again. Each check starts with the harmless definition.",
        ],
        request_path: "",
        upstream: Upstream::Mcp { kind: "rug_pull", second: None },
        params: &[],
    },
    Scenario {
        id: "mcp_tool_shadowing",
        title: "MCP tool shadowing",
        proves: "When two servers behind the gateway offer a tool of the same name, the attacker's copy (with hidden instructions) does not take the trusted tool's place: the gateway namespaces or refuses it.",
        setup: &[
            "Put two MCP servers behind one gateway route (one virtual server): the trusted {second_upstream} and the attacker's {upstream} (Streamable HTTP), and let it forward the X-Rustybin-Session request header. Both offer lookup_customer.",
            "Turn on the gateway's namespacing of tool names, or its refusal of duplicate names.",
            MCP_CHECK,
        ],
        request_path: "",
        upstream: Upstream::Mcp { kind: "shadowing", second: Some(SHADOWED_SERVER_PATH) },
        params: &[],
    },
    Scenario {
        id: "mcp_result_injection",
        title: "MCP result injection",
        proves: "The gateway removes or flags a prompt injection in a tool's result (its text, its structured content and a resource link) before the client sees it.",
        setup: &[
            MCP_ROUTE,
            "Turn on the gateway's guard of tool results against prompt injection (\"ignore all previous instructions\"). {upstream}'s get_order_notes answers one in its text, its structured content and a resource link.",
            MCP_CHECK,
        ],
        request_path: "",
        upstream: Upstream::Mcp { kind: "result_injection", second: None },
        params: &[],
    },
    Scenario {
        id: "mcp_oversized_result",
        title: "MCP oversized result",
        proves: "The gateway caps the size of a tool result: the client never receives more than the limit (the result cut, or refused).",
        setup: &[
            MCP_ROUTE,
            "Limit the size of the answers the gateway passes from the MCP server (256 KiB, say). {upstream}'s export_logs answers 2 MiB.",
            MCP_CHECK,
            "Check with the limit you set, in KiB (max_result_kib).",
        ],
        request_path: "",
        upstream: Upstream::Mcp { kind: "oversized", second: None },
        params: &[MAX_RESULT_KIB],
    },
    Scenario {
        id: "mcp_exfiltration_url",
        title: "MCP exfiltration link",
        proves: "The gateway removes or refuses a link in a tool result that would send data to a host nobody allowed.",
        setup: &[
            MCP_ROUTE,
            "Turn on the gateway's filtering of links in tool results to hosts you did not allow. {upstream}'s fetch_report answers a link to exfil.rustybin.invalid with data in its query string.",
            MCP_CHECK,
        ],
        request_path: "",
        upstream: Upstream::Mcp { kind: "exfiltration_url", second: None },
        params: &[],
    },
];

pub fn find(id: &str) -> Option<&'static Scenario> {
    SCENARIOS.iter().find(|s| s.id == id)
}

impl Scenario {
    /// The setup steps for one run (`second`: the second target's URL).
    pub fn setup_for(
        &self,
        upstream: &str,
        second: Option<&str>,
        key: Option<&str>,
    ) -> Vec<String> {
        let base = upstream.strip_suffix(AI_PRIMARY_PATH).unwrap_or("{base}");
        self.setup
            .iter()
            .map(|s| {
                s.replace("{upstream}", upstream)
                    .replace("{second_upstream}", second.unwrap_or("{second_upstream}"))
                    .replace("{base}", base)
                    .replace("{key}", key.unwrap_or(""))
                    .replace("{slow_ms}", &SLOW_MS.to_string())
            })
            .collect()
    }

    /// `http` or `ai`: the console groups the scenarios by it.
    pub fn group(&self) -> &'static str {
        match self.upstream {
            Upstream::Ai { .. } => "ai",
            Upstream::Mcp { .. } => "mcp",
            _ => "http",
        }
    }

    /// How the AI mock answers a run's requests before its first check
    /// (each check sets it again from its parameters: [`Self::ai_profile`]).
    pub fn ai_profile(&self, p: &Params) -> AiProfile {
        match self.id {
            "ai_model_fallback" => AiProfile {
                fail_primary: Some(p.fail_kind),
                mode: None,
            },
            "ai_response_guard" => AiProfile {
                fail_primary: None,
                mode: Some("scripted"),
            },
            _ => AiProfile::default(),
        }
    }

    /// How many requests a check sends (at most, for `circuit_breaker`
    /// and `ai_token_limit`).
    pub fn requests(&self, limit: u32) -> usize {
        match self.id {
            "retries" | "caching" | "cors" | "ai_model_fallback" | "ai_prompt_guard" => 3,
            "rate_limiting" => (limit * 2) as usize,
            "credential_injection" | "request_size_limit" => 2,
            "circuit_breaker" => MAX_BREAKER_REQUESTS + BREAKER_TRIALS,
            "ai_token_limit" => MAX_TOKEN_REQUESTS,
            "ai_semantic_cache" => 4,
            "ai_provider_balance" => BALANCE_REQUESTS,
            "mcp_rug_pull" => 5,
            id if id.starts_with("mcp_") => 3,
            _ => 1,
        }
    }

    /// The test parameters of a check, by name (`null` is not given): each
    /// given one is a known parameter of this scenario, of its kind and in
    /// its bounds; the others take their defaults.
    pub fn params_of(&self, given: &serde_json::Map<String, Value>) -> Result<Params, String> {
        let mut p = Params::default();
        for (name, v) in given {
            if v.is_null() {
                continue;
            }
            let Some(param) = PARAMS.iter().find(|q| q.name == name) else {
                return Err(format!(
                    "unknown test parameter {name:?}: one of {}",
                    PARAMS.iter().map(|q| q.name).collect::<Vec<_>>().join(", ")
                ));
            };
            if !self.params.iter().any(|q| q.name == param.name) {
                return Err(format!(
                    "{} applies to {} only",
                    param.name,
                    owners(param.name)
                ));
            }
            p.set(param.name, param.parse(v)?);
        }
        if self.id == "cors" && p.allowed_origin == p.refused_origin {
            return Err("refused_origin: another origin than allowed_origin".into());
        }
        if self.id == "header_transformation" && p.add_request_header == p.remove_request_header {
            return Err("remove_request_header: another header than add_request_header".into());
        }
        Ok(p)
    }

    /// The catalogue entry.
    pub fn view(&self) -> Value {
        json!({
            "id": self.id,
            "title": self.title,
            "group": self.group(),
            "proves": self.proves,
            "upstream": match self.upstream {
                Upstream::Mock => "a mock created for the run".to_string(),
                Upstream::Flaky(p) => format!("{p} on this instance"),
                Upstream::Ai { second: false } => format!("{AI_PRIMARY_PATH} on this instance"),
                Upstream::Ai { second: true } => {
                    format!("{AI_PRIMARY_PATH} and {AI_SECOND_PATH} on this instance")
                }
                Upstream::Mcp { kind, second: None } => {
                    format!("the MCP attack server /mcp/attack/{kind} on this instance")
                }
                Upstream::Mcp { kind, second: Some(s) } => format!(
                    "the MCP attack server /mcp/attack/{kind} and the trusted {s} on this instance"
                ),
            },
            "setup": self.setup_for("{upstream}", Some("{second_upstream}"), Some("{key}")),
            "requests": self.requests(DEFAULT_LIMIT),
            "parameters": self.parameters(),
        })
    }

    /// The test parameters a check takes, with their bounds.
    pub fn parameters(&self) -> Value {
        Value::Array(self.params.iter().map(Param::view).collect())
    }

    /// The upstream mock's spec and settings (`Upstream::Mock` scenarios).
    pub fn mock(&self, key: &str) -> Option<(String, MockSettings)> {
        let (path, op, summary) = match self.id {
            "rate_limiting" => ("/items", "listItems", "Items"),
            "credential_injection" => ("/profile", "getProfile", "The caller's profile"),
            "timeouts" => ("/slow", "slowAnswer", "A slow answer"),
            "caching" => ("/catalog", "getCatalog", "A cacheable catalogue"),
            "request_size_limit" => ("/upload", "upload", "An upload, any size"),
            "cors" => ("/cors", "getResource", "A resource a web page reads"),
            "header_transformation" => ("/headers", "getHeaders", "An answer to transform"),
            _ => return None,
        };
        let mut operation_doc = json!({
            "operationId": op,
            "summary": summary,
            "responses": { "200": {
                "description": summary,
                "content": { "application/json": { "example": { "scenario": self.id, "ok": true } } }
            } }
        });
        let method = if self.id == "request_size_limit" {
            operation_doc["requestBody"] = json!({
                "required": true,
                "content": { "application/octet-stream": { "schema": { "type": "string", "format": "binary" } } }
            });
            "post"
        } else {
            "get"
        };
        let mut spec = json!({
            "openapi": "3.0.3",
            "info": { "title": format!("Gateway scenario upstream: {}", self.title), "version": "1.0.0" },
            "paths": { path: { method: operation_doc } }
        });
        if self.id == "cors" {
            // A preflight the gateway lets through is answered (and
            // captured) here; its CORS headers, with `upstream_cors`, come
            // from `scenarios::own_cors`.
            spec["paths"][path]["options"] = json!({
                "operationId": "preflight",
                "summary": "A CORS preflight",
                "responses": { "204": { "description": "No content" } }
            });
        }
        let mut settings = MockSettings::default();
        let mut operation = OperationSettings::default();
        match self.id {
            "credential_injection" => {
                settings.security = SecuritySettings {
                    enabled: true,
                    mode: SecurityMode::ApiKey,
                    api_key: ApiKeySettings {
                        header: KEY_HEADER.to_ascii_lowercase(),
                        keys: vec![key.to_string()],
                    },
                    ..SecuritySettings::default()
                };
            }
            "timeouts" => operation.delay_ms = Some(SLOW_MS),
            "caching" => {
                operation.respond = Some(ResponseOverride {
                    status: Some(200),
                    headers: BTreeMap::from([(
                        "Cache-Control".to_string(),
                        "public, max-age=300".to_string(),
                    )]),
                    ..ResponseOverride::default()
                });
            }
            _ => {}
        }
        if !operation.is_empty() {
            settings.operations.insert(op.to_string(), operation);
        }
        Some((spec.to_string(), settings))
    }

    /// Judge a check's requests.
    pub fn evaluate(&self, sent: &[Sent], p: &Params) -> Vec<Expectation> {
        let limit = p.limit;
        let n = sent.len();
        let statuses = list(sent.iter().map(|s| s.status_text()));
        let count = |code: u16| sent.iter().filter(|s| s.status == code).count();
        let upstream: usize = sent.iter().map(|s| s.upstream.len()).sum();
        match self.id {
            "retries" => vec![
                Expectation::new(
                    "Every request succeeded through the gateway",
                    format!("200 for {n} of {n}"),
                    format!("200 for {} of {n} ({statuses})", count(200)),
                    count(200) == n,
                ),
                Expectation::new(
                    "The gateway retried the failed attempt",
                    "2 or more attempts per request reached the upstream",
                    format!("attempts per request: {}", list(sent.iter().map(|s| s.upstream.len().to_string()))),
                    sent.iter().all(|s| s.upstream.len() >= 2),
                ),
            ],
            "rate_limiting" => vec![
                Expectation::new(
                    "The gateway refused requests over the limit with 429",
                    "429 for at least one request",
                    format!("429 for {} of {n} ({statuses})", count(429)),
                    count(429) > 0,
                ),
                Expectation::new(
                    "Requests over the limit never reached the upstream",
                    format!("at most {limit} upstream requests"),
                    format!("{upstream} upstream requests"),
                    upstream <= limit as usize,
                ),
            ],
            "credential_injection" => vec![
                Expectation::new(
                    "The client got 200 without sending a credential",
                    format!("200 for {n} of {n}"),
                    format!("200 for {} of {n} ({statuses})", count(200)),
                    count(200) == n,
                ),
                Expectation::new(
                    "The upstream accepted the credential the gateway added",
                    format!("the upstream answered 200 to every request (it answers 401 without {KEY_HEADER})"),
                    format!(
                        "upstream answers: {}",
                        list(sent.iter().flat_map(|s| s.upstream.iter().map(|c| c.to_string())))
                    ),
                    sent.iter().all(|s| !s.upstream.is_empty() && s.upstream.iter().all(|c| *c == 200)),
                ),
            ],
            "timeouts" => {
                let gave_up = sent.iter().all(|s| s.status != 0 && s.ms < SLOW_MS);
                vec![
                    Expectation::new(
                        "The gateway answered with a gateway error",
                        "504 (or 502, 503)",
                        statuses.clone(),
                        sent.iter().all(|s| (502..=504).contains(&s.status)),
                    ),
                    Expectation::new(
                        "The gateway gave up before the upstream answered",
                        format!("an answer in under {SLOW_MS} ms"),
                        list(sent.iter().map(|s| format!("{} ms", s.ms))),
                        gave_up,
                    ),
                ]
            }
            "caching" => vec![
                Expectation::new(
                    "Every request got 200",
                    format!("200 for {n} of {n}"),
                    format!("200 for {} of {n} ({statuses})", count(200)),
                    count(200) == n,
                ),
                Expectation::new(
                    "Only the first request reached the upstream",
                    "1 upstream request",
                    format!("{upstream} upstream requests"),
                    upstream == 1,
                ),
            ],
            "circuit_breaker" => breaker(sent, p.half_open_ms),
            "request_size_limit" => size_limit(sent, p.size_limit),
            "cors" => cors(sent, p),
            "header_transformation" => transformation(sent, p),
            "ai_token_limit" => ai_token_limit(sent, p.token_limit),
            "ai_model_fallback" => ai_fallback(sent, p.fail_kind),
            "ai_semantic_cache" => ai_cache(sent),
            "ai_prompt_guard" => ai_prompt_guard(sent, !p.skip_injection),
            "ai_response_guard" => ai_response_guard(sent),
            "ai_provider_balance" => ai_balance(sent, p.min_share),
            "mcp_tool_poisoning" => mcp_poisoning(sent),
            "mcp_rug_pull" => mcp_rug_pull(sent),
            "mcp_tool_shadowing" => mcp_shadowing(sent),
            "mcp_result_injection" => mcp_contains(
                sent,
                "The prompt injection never reached the client",
                "a result without the injection (removed, or the result refused or flagged)",
                injection_in,
            ),
            "mcp_oversized_result" => mcp_oversized(sent, p.max_result_kib),
            "mcp_exfiltration_url" => mcp_contains(
                sent,
                "No link to the unknown host reached the client",
                "a result without a link to exfil.rustybin.invalid (removed, or the result refused)",
                |t| t.contains(crate::mcp::attacks::EXFIL_HOST),
            ),
            _ => Vec::new(),
        }
    }
}

/// The JSON-RPC answer in a body: plain JSON, or the first SSE `data:`
/// event that is an answer (has an `id`).
pub fn rpc_answer(body: &str) -> Option<Value> {
    let t = body.trim_start();
    if t.starts_with('{') {
        return serde_json::from_str(t).ok();
    }
    body.lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
        .find(|v| v.get("id").is_some() && (v.get("result").is_some() || v.get("error").is_some()))
}

/// The tools (name, the rest as text) of a `tools/list` answer.
fn listed(s: &Sent) -> Option<Vec<(String, String)>> {
    let v = rpc_answer(s.body.as_deref()?)?;
    let tools = v.get("result")?.get("tools")?.as_array()?;
    Some(
        tools
            .iter()
            .map(|t| {
                (
                    t.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    t.to_string(),
                )
            })
            .collect(),
    )
}

/// A tool's name as a client sees it through a gateway: the name itself,
/// or a namespaced one ending with it.
fn named(name: &str, wanted: &str) -> bool {
    name == wanted
        || name.ends_with(&format!("_{wanted}"))
        || name.ends_with(&format!(".{wanted}"))
        || name.ends_with(&format!("/{wanted}"))
        || name.ends_with(&format!(":{wanted}"))
}

/// What became of a request of an MCP check, in words.
fn mcp_status(s: Option<&Sent>) -> String {
    match s {
        None => "not sent".into(),
        Some(s) if s.status == 0 => "no answer".into(),
        Some(s) => match s.body.as_deref().and_then(rpc_answer) {
            Some(v) if v.get("error").is_some() => format!("{}, a JSON-RPC error", s.status),
            Some(_) => format!("{}, answered", s.status),
            None => format!("{}, no JSON-RPC answer", s.status),
        },
    }
}

fn by_label<'a>(sent: &'a [Sent], label: &str) -> Option<&'a Sent> {
    sent.iter().find(|s| s.label == Some(label))
}

fn reached(s: Option<&Sent>) -> bool {
    s.is_some_and(|s| !s.upstream.is_empty())
}

/// `mcp_tool_poisoning`: the poisoned tool kept from the client, the clean
/// one passed.
fn mcp_poisoning(sent: &[Sent]) -> Vec<Expectation> {
    use crate::mcp::attacks::POISON_MARK;
    let s = by_label(sent, LABEL_LIST);
    let tools = s.and_then(listed);
    let poisoned = s
        .and_then(|s| s.body.as_deref())
        .is_some_and(|b| b.contains(POISON_MARK));
    let clean = tools
        .as_ref()
        .is_some_and(|t| t.iter().any(|(n, _)| named(n, "get_weather")));
    vec![
        Expectation::new(
            "The tool list reached the server",
            "tools/list reached the attack server",
            format!(
                "{} upstream request(s)",
                s.map(|s| s.upstream.len()).unwrap_or(0)
            ),
            reached(s),
        ),
        Expectation::new(
            "The poisoned description never reached the client",
            "a tool list without the hidden instructions (the tool removed, refused or rewritten)",
            if poisoned {
                "the poisoned add_numbers reached the client as served".to_string()
            } else {
                format!("{}, without the hidden instructions", mcp_status(s))
            },
            s.is_some_and(|s| s.status != 0) && !poisoned,
        ),
        Expectation::new(
            "The clean tool still reached the client",
            "get_weather in the tool list",
            match &tools {
                Some(t) => format!(
                    "{} tool(s): {}",
                    t.len(),
                    list(t.iter().map(|(n, _)| n.clone()))
                ),
                None => mcp_status(s),
            },
            clean,
        ),
    ]
}

/// `mcp_rug_pull`: the approved definition first, the changed one never.
fn mcp_rug_pull(sent: &[Sent]) -> Vec<Expectation> {
    use crate::mcp::attacks::RUG_MARK;
    let first = by_label(sent, LABEL_FIRST_LIST);
    let call = by_label(sent, LABEL_CALL);
    let second = by_label(sent, LABEL_SECOND_LIST);
    let has = |s: Option<&Sent>, name: &str| {
        s.and_then(listed)
            .is_some_and(|t| t.iter().any(|(n, _)| named(n, name)))
    };
    let pulled = |s: Option<&Sent>| {
        s.and_then(|s| s.body.as_deref())
            .is_some_and(|b| b.contains(RUG_MARK))
    };
    vec![
        Expectation::new(
            "The client saw the harmless definition first",
            "get_exchange_rate in the first tool list, without hidden instructions",
            mcp_status(first),
            has(first, "get_exchange_rate") && !pulled(first),
        ),
        Expectation::new(
            "The call reached the server (which then changes the tool)",
            "tools/call reached the attack server",
            format!(
                "{} upstream request(s)",
                call.map(|s| s.upstream.len()).unwrap_or(0)
            ),
            reached(call),
        ),
        Expectation::new(
            "The changed definition never reached the client",
            "a second tool list with the approved definition, without the tool, or refused",
            if pulled(second) {
                "the changed get_exchange_rate reached the client".to_string()
            } else {
                format!("{}, without the changed definition", mcp_status(second))
            },
            second.is_some_and(|s| s.status != 0) && !pulled(second),
        ),
    ]
}

/// `mcp_tool_shadowing`: the trusted tool keeps its name, the attacker's
/// copy does not take it.
fn mcp_shadowing(sent: &[Sent]) -> Vec<Expectation> {
    use crate::mcp::attacks::SHADOW_MARK;
    let s = by_label(sent, LABEL_LIST);
    let tools = s.and_then(listed).unwrap_or_default();
    let exact: Vec<&(String, String)> = tools
        .iter()
        .filter(|(n, _)| n == "lookup_customer")
        .collect();
    let trusted = tools
        .iter()
        .any(|(n, t)| named(n, "lookup_customer") && !t.contains(SHADOW_MARK));
    let shadowed = exact.iter().any(|(_, t)| t.contains(SHADOW_MARK)) || exact.len() > 1;
    vec![
        Expectation::new(
            "The trusted tool reached the client",
            "the trusted server's lookup_customer in the tool list",
            format!(
                "{}: {}",
                mcp_status(s),
                list(
                    tools
                        .iter()
                        .filter(|(n, _)| named(n, "lookup_customer"))
                        .map(|(n, t)| {
                            format!(
                                "{n} ({})",
                                if t.contains(SHADOW_MARK) {
                                    "the attacker's"
                                } else {
                                    "the trusted one"
                                }
                            )
                        })
                )
            ),
            trusted,
        ),
        Expectation::new(
            "The attacker's copy did not take the trusted tool's name",
            "no tool named exactly lookup_customer is the attacker's, and no two share the name",
            if shadowed {
                "the attacker's lookup_customer is listed under the trusted name".to_string()
            } else {
                "the name lookup_customer is the trusted tool's only".to_string()
            },
            s.is_some_and(|s| s.status != 0) && !shadowed,
        ),
    ]
}

/// An MCP check whose call must not bring `found` to the client.
fn mcp_contains(
    sent: &[Sent],
    name: &'static str,
    expected: &str,
    found: impl Fn(&str) -> bool,
) -> Vec<Expectation> {
    let call = by_label(sent, LABEL_CALL);
    let leaked = call.and_then(|s| s.body.as_deref()).is_some_and(&found);
    vec![
        Expectation::new(
            "The call reached the server",
            "tools/call reached the attack server",
            format!(
                "{} upstream request(s)",
                call.map(|s| s.upstream.len()).unwrap_or(0)
            ),
            reached(call),
        ),
        Expectation::new(
            name,
            expected,
            if leaked {
                format!("{}, with it", mcp_status(call))
            } else {
                format!("{}, without it", mcp_status(call))
            },
            call.is_some_and(|s| s.status != 0) && !leaked,
        ),
    ]
}

/// `mcp_oversized_result`: the client never got more than the limit.
fn mcp_oversized(sent: &[Sent], kib: u64) -> Vec<Expectation> {
    let call = by_label(sent, LABEL_CALL);
    let limit = kib as usize * 1024;
    let got = call.and_then(|s| s.received).unwrap_or(0);
    vec![
        Expectation::new(
            "The call reached the server",
            "tools/call reached the attack server",
            format!(
                "{} upstream request(s)",
                call.map(|s| s.upstream.len()).unwrap_or(0)
            ),
            reached(call),
        ),
        Expectation::new(
            "The client never received more than the limit",
            format!("an answer of at most {kib} KiB (the result cut, or refused)"),
            match call {
                None => "not sent".to_string(),
                Some(s) if s.status == 0 => "no answer".to_string(),
                Some(s) if got > limit => format!("{}, more than {kib} KiB", s.status),
                Some(s) => format!("{}, {got} bytes", s.status),
            },
            call.is_some_and(|s| s.status != 0) && got <= limit,
        ),
    ]
}

/// The model calls among a request's upstream requests (a gateway's own
/// embedding calls for a semantic cache are not model calls).
fn model_calls(s: &Sent) -> usize {
    s.upstream_paths
        .iter()
        .filter(|p| !p.contains("embeddings"))
        .count()
}

/// The kinds of planted personal data in `text`, in order, each once.
pub fn planted_in(text: &str) -> Vec<&'static str> {
    let mut kinds: Vec<&'static str> = Vec::new();
    for (kind, value) in PLANTED {
        if text.contains(value) && !kinds.contains(kind) {
            kinds.push(kind);
        }
    }
    kinds
}

/// Does `text` carry the planted prompt injection?
pub fn injection_in(text: &str) -> bool {
    text.to_ascii_lowercase().contains(INJECTION_MARK)
}

/// `ai_token_limit`: refusals once about the budget reached the model, and
/// none of them reached it.
fn ai_token_limit(sent: &[Sent], limit: u64) -> Vec<Expectation> {
    let n = sent.len();
    let refused: Vec<&Sent> = sent.iter().filter(|s| s.status == 429).collect();
    let first = sent.iter().position(|s| s.status == 429);
    let before = &sent[..first.unwrap_or(n)];
    // What the model used before the first refusal, from the usage of the
    // answers that reached it.
    let used: u64 = before
        .iter()
        .filter(|s| model_calls(s) > 0)
        .filter_map(|s| s.tokens)
        .sum();
    let per = sent.iter().find_map(|s| s.tokens).unwrap_or(0);
    let lo = limit.saturating_sub(2 * per);
    let hi = limit + limit / 4 + 2 * per;
    let leaked = refused.iter().filter(|s| !s.upstream.is_empty()).count();
    vec![
        Expectation::new(
            "The gateway refused requests once the token budget was spent",
            "429 for at least one request",
            format!(
                "429 for {} of {n} ({})",
                refused.len(),
                list(sent.iter().map(|s| s.status_text()))
            ),
            !refused.is_empty(),
        ),
        Expectation::new(
            "Refused requests never reached the model",
            "0 upstream requests for each 429",
            format!("{leaked} of {} reached it", refused.len()),
            !refused.is_empty() && leaked == 0,
        ),
        Expectation::new(
            "The budget was counted in tokens",
            format!("the first 429 after {lo} to {hi} tokens reached the model ({per} tokens per request, budget {limit})"),
            match (first, per) {
                (_, 0) => "the answers carried no usage".to_string(),
                (None, _) => format!("no 429 after {used} tokens"),
                (Some(i), _) => format!("the first 429 (request {}) after {used} tokens", i + 1),
            },
            first.is_some() && per > 0 && (lo..=hi).contains(&used),
        ),
    ]
}

/// `ai_model_fallback`: each request failed on the primary target, then
/// succeeded on another.
fn ai_fallback(sent: &[Sent], kind: &str) -> Vec<Expectation> {
    let n = sent.len();
    let ok2xx = |c: u16| (200..300).contains(&c);
    let count = sent.iter().filter(|s| ok2xx(s.status)).count();
    let primary_failed = |s: &Sent| {
        s.upstream_paths.first().map(String::as_str) == Some(AI_PRIMARY_PATH)
            && s.upstream.first().is_some_and(|c| *c >= 400)
    };
    let fell_back = |s: &Sent| {
        s.upstream_paths
            .iter()
            .zip(&s.upstream)
            .skip(1)
            .any(|(p, c)| p != AI_PRIMARY_PATH && ok2xx(*c))
    };
    let route = |s: &Sent| {
        let hops: Vec<String> = s
            .upstream_paths
            .iter()
            .zip(&s.upstream)
            .map(|(p, c)| {
                let target = if p == AI_PRIMARY_PATH {
                    "primary"
                } else {
                    "fallback"
                };
                format!("{target} {c}")
            })
            .collect();
        if hops.is_empty() {
            "nothing upstream".to_string()
        } else {
            hops.join(" then ")
        }
    };
    vec![
        Expectation::new(
            "Every request succeeded through the gateway",
            format!("2xx for {n} of {n}"),
            format!(
                "2xx for {count} of {n} ({})",
                list(sent.iter().map(|s| s.status_text()))
            ),
            count == n,
        ),
        Expectation::new(
            "The primary target was tried first and failed",
            format!("the first upstream request of each went to {AI_PRIMARY_PATH} and got its {kind} error"),
            list(sent.iter().map(route)),
            n > 0 && sent.iter().all(primary_failed),
        ),
        Expectation::new(
            "The gateway fell back to another target",
            "after the failure, an upstream request of each to another target succeeded",
            format!("{} of {n} fell back", sent.iter().filter(|s| fell_back(s)).count()),
            n > 0 && sent.iter().all(fell_back),
        ),
    ]
}

/// `ai_semantic_cache`: the first and the different prompt reached the
/// model, the two rewordings did not.
fn ai_cache(sent: &[Sent]) -> Vec<Expectation> {
    let by = |label: &str| sent.iter().find(|s| s.label == Some(label));
    let calls = |s: Option<&Sent>| s.map(model_calls).unwrap_or(0);
    let first = by(LABEL_CACHE_FIRST);
    let similar = [by(LABEL_CACHE_CASE), by(LABEL_CACHE_PLEASE)];
    let other = by(LABEL_CACHE_OTHER);
    let n = sent.len();
    let ok = sent
        .iter()
        .filter(|s| (200..300).contains(&s.status))
        .count();
    let similar_calls: Vec<usize> = similar.iter().map(|s| calls(*s)).collect();
    vec![
        Expectation::new(
            "Every request got an answer",
            format!("2xx for {n} of {n}"),
            format!(
                "2xx for {ok} of {n} ({})",
                list(sent.iter().map(|s| s.status_text()))
            ),
            n == 4 && ok == n,
        ),
        Expectation::new(
            "The first prompt reached the model",
            "1 model request for it",
            format!("{} model request(s)", calls(first)),
            calls(first) >= 1,
        ),
        Expectation::new(
            "Prompts that mean the same were answered from the cache",
            "0 model requests for the same prompt in other case and punctuation, and for it with a word more",
            format!(
                "{} and {} model request(s)",
                similar_calls[0], similar_calls[1]
            ),
            similar.iter().all(|s| s.is_some_and(|s| (200..300).contains(&s.status)))
                && similar_calls.iter().all(|c| *c == 0),
        ),
        Expectation::new(
            "A different prompt reached the model",
            "1 model request for it",
            format!("{} model request(s)", calls(other)),
            calls(other) >= 1,
        ),
    ]
}

/// What became of a guarded prompt: refused by the gateway, redacted, or
/// what reached the model.
fn guarded(s: Option<&Sent>, found: impl Fn(&str) -> Vec<&'static str>) -> (bool, String) {
    let Some(s) = s else {
        return (false, "not sent".into());
    };
    if s.status == 0 {
        return (false, "no answer".into());
    }
    if s.upstream.is_empty() {
        return (true, format!("refused by the gateway ({})", s.status));
    }
    let mut kinds: Vec<&'static str> = Vec::new();
    for body in &s.upstream_bodies {
        for k in found(body) {
            if !kinds.contains(&k) {
                kinds.push(k);
            }
        }
    }
    if kinds.is_empty() {
        (true, "redacted before the model".into())
    } else {
        (
            false,
            format!("reached the model with: {}", kinds.join(", ")),
        )
    }
}

/// `ai_prompt_guard`: the clean prompt through, the planted data and the
/// injection stopped.
fn ai_prompt_guard(sent: &[Sent], injection: bool) -> Vec<Expectation> {
    let by = |label: &str| sent.iter().find(|s| s.label == Some(label));
    let clean = by(LABEL_CLEAN);
    let mut out = vec![Expectation::new(
        "A clean prompt went through to the model",
        "2xx, and it reached the model",
        match clean {
            None => "not sent".to_string(),
            Some(s) => format!("{}, {} model request(s)", s.status_text(), model_calls(s)),
        },
        clean.is_some_and(|s| (200..300).contains(&s.status) && model_calls(s) > 0),
    )];
    let (ok, seen) = guarded(by(LABEL_PII), planted_in);
    out.push(Expectation::new(
        "Personal data never reached the model",
        "the prompt refused by the gateway, or the SSN, card number and email removed from it",
        seen,
        ok,
    ));
    if injection {
        let (ok, seen) = guarded(by(LABEL_INJECTION), |t| {
            if injection_in(t) {
                vec!["the injection"]
            } else {
                Vec::new()
            }
        });
        out.push(Expectation::new(
            "The prompt injection never reached the model",
            "the prompt refused by the gateway, or the injection removed from it",
            seen,
            ok,
        ));
    }
    out
}

/// `ai_response_guard`: the model answered the record, the client got none
/// of it.
fn ai_response_guard(sent: &[Sent]) -> Vec<Expectation> {
    let s = sent.first();
    let leaked = s
        .and_then(|s| s.body.as_deref())
        .map(planted_in)
        .unwrap_or_default();
    vec![
        Expectation::new(
            "The request reached the model",
            "1 model request (it answers a fake customer record)",
            format!("{} model request(s)", s.map(model_calls).unwrap_or(0)),
            s.is_some_and(|s| model_calls(s) > 0),
        ),
        Expectation::new(
            "No personal data reached the client",
            "an answer without the SSN, card number, email and phone (redacted, or the answer refused)",
            match s {
                None => "not sent".to_string(),
                Some(s) if s.status == 0 => "no answer".to_string(),
                Some(s) if leaked.is_empty() => format!("{}, none of it", s.status_text()),
                Some(s) => format!("{} with: {}", s.status_text(), leaked.join(", ")),
            },
            s.is_some_and(|s| s.status != 0) && leaked.is_empty(),
        ),
    ]
}

/// `ai_provider_balance`: every request through, each target its share.
fn ai_balance(sent: &[Sent], min_share: u64) -> Vec<Expectation> {
    let n = sent.len();
    let ok = sent
        .iter()
        .filter(|s| (200..300).contains(&s.status))
        .count();
    // The target that answered each request: its last model call.
    let target = |s: &Sent| {
        s.upstream_paths
            .iter()
            .rev()
            .find(|p| !p.contains("embeddings"))
            .map(|p| p == AI_PRIMARY_PATH)
    };
    let primary = sent.iter().filter(|s| target(s) == Some(true)).count();
    let other = sent.iter().filter(|s| target(s) == Some(false)).count();
    let need = (n as u64 * min_share).div_ceil(100) as usize;
    vec![
        Expectation::new(
            "Every request succeeded through the gateway",
            format!("2xx for {n} of {n}"),
            format!(
                "2xx for {ok} of {n} ({})",
                list(sent.iter().map(|s| s.status_text()))
            ),
            n > 0 && ok == n,
        ),
        Expectation::new(
            "Both targets received requests",
            format!("at least {need} of {n} ({min_share}%) for each target"),
            format!("{primary} to the first target, {other} to the second"),
            n > 0 && primary >= need.max(1) && other >= need.max(1),
        ),
    ]
}

/// An answer's `Access-Control-Allow-Origin` in words, against `origin`
/// (never the value itself: the gateway wrote it).
fn acao_text(s: &Sent, p: &Params) -> String {
    match s.header("access-control-allow-origin") {
        None => "no Access-Control-Allow-Origin".into(),
        Some("*") => "Access-Control-Allow-Origin *".into(),
        Some(v) if v.eq_ignore_ascii_case(&p.allowed_origin) => {
            "Access-Control-Allow-Origin of allowed_origin".into()
        }
        Some(v) if v.eq_ignore_ascii_case(&p.refused_origin) => {
            "Access-Control-Allow-Origin of refused_origin".into()
        }
        Some(_) => "Access-Control-Allow-Origin of another origin".into(),
    }
}

/// Does the answer let `origin` read it?
fn allows(s: &Sent, origin: &str) -> bool {
    s.header("access-control-allow-origin")
        .is_some_and(|v| v == "*" || v.eq_ignore_ascii_case(origin))
}

/// `cors`: the allowed preflight answered (by the gateway, or by the
/// upstream when it answers CORS), the refused one not, the GET allowed.
fn cors(sent: &[Sent], p: &Params) -> Vec<Expectation> {
    let by = |label: &str| sent.iter().find(|s| s.label == Some(label));
    let pre = by(PREFLIGHT_ALLOWED);
    let refused = by(PREFLIGHT_REFUSED);
    let simple = by(SIMPLE_ALLOWED);
    let ok2xx = |s: &Sent| (200..300).contains(&s.status);
    let answer = |s: Option<&Sent>| match s {
        None => "not sent".to_string(),
        Some(s) if s.status == 0 => "no answer".to_string(),
        Some(s) => format!("{}, {}", s.status, acao_text(s, p)),
    };
    let methods = |s: &Sent| {
        s.header("access-control-allow-methods").is_some_and(|m| {
            m.split(',')
                .map(str::trim)
                .any(|m| m == "*" || m.eq_ignore_ascii_case("GET"))
        })
    };
    let pre_seen = match pre {
        Some(s) if s.status != 0 => format!(
            "{}, Access-Control-Allow-Methods {}",
            answer(Some(s)),
            if methods(s) {
                "with GET"
            } else if s.header("access-control-allow-methods").is_some() {
                "without GET"
            } else {
                "absent"
            }
        ),
        other => answer(other),
    };
    let ups = |s: Option<&Sent>| s.map(|s| s.upstream.len()).unwrap_or(0);
    let reach = if p.upstream_cors {
        Expectation::new(
            "The preflight reached the upstream, which answers CORS",
            "the preflight from allowed_origin reached the upstream (upstream_cors: true)",
            format!("{} upstream request(s)", ups(pre)),
            pre.is_some_and(|s| s.status != 0 && !s.upstream.is_empty()),
        )
    } else {
        Expectation::new(
            "The gateway answered the preflight without the upstream",
            "0 upstream requests for the preflight from allowed_origin",
            format!("{} upstream request(s)", ups(pre)),
            pre.is_some_and(|s| s.status != 0 && s.upstream.is_empty()),
        )
    };
    vec![
        Expectation::new(
            "The preflight from the allowed origin got CORS headers",
            format!(
                "2xx with Access-Control-Allow-Origin {} (or *) and Access-Control-Allow-Methods with GET",
                p.allowed_origin
            ),
            pre_seen,
            pre.is_some_and(|s| ok2xx(s) && allows(s, &p.allowed_origin) && methods(s)),
        ),
        reach,
        Expectation::new(
            "The preflight from the refused origin got no CORS answer for it",
            format!(
                "an answer without Access-Control-Allow-Origin {} or *",
                p.refused_origin
            ),
            answer(refused),
            refused.is_some_and(|s| s.status != 0 && !allows(s, &p.refused_origin)),
        ),
        Expectation::new(
            "The request from the allowed origin got Access-Control-Allow-Origin",
            format!(
                "2xx with Access-Control-Allow-Origin {} (or *)",
                p.allowed_origin
            ),
            answer(simple),
            simple.is_some_and(|s| ok2xx(s) && allows(s, &p.allowed_origin)),
        ),
    ]
}

/// `header_transformation`: what reached the upstream and what came back.
fn transformation(sent: &[Sent], p: &Params) -> Vec<Expectation> {
    let s = sent.first();
    // The first upstream request the check's request caused.
    let up = s.and_then(|s| s.upstream_headers.first());
    let up_header = |name: &str| {
        up.and_then(|h| {
            h.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        })
    };
    let status = s
        .map(Sent::status_text)
        .unwrap_or_else(|| "not sent".into());
    let added = up_header(&p.add_request_header);
    let added_ok = match (&p.add_request_value, added) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(want), Some(v)) => v == want,
    };
    let added_seen = match (up, added) {
        (None, _) => "no upstream request".to_string(),
        (Some(_), None) => "absent".to_string(),
        (Some(_), Some(v)) => match &p.add_request_value {
            Some(want) if v == want => "present with that value".to_string(),
            Some(_) => "present with another value".to_string(),
            None => "present".to_string(),
        },
    };
    let removed = up_header(&p.remove_request_header);
    let removed_seen = match (up, removed) {
        (None, _) => "no upstream request".to_string(),
        (Some(_), None) => "absent".to_string(),
        (Some(_), Some(v)) if s.is_some_and(|s| s.marker.as_deref() == Some(v)) => {
            "present with the value the client sent".to_string()
        }
        (Some(_), Some(_)) => "present with another value".to_string(),
    };
    let answered = s.and_then(|s| s.header(&p.add_response_header));
    vec![
        Expectation::new(
            "The request succeeded through the gateway",
            "2xx",
            status.clone(),
            s.is_some_and(|s| (200..300).contains(&s.status)),
        ),
        Expectation::new(
            "The added request header reached the upstream",
            match &p.add_request_value {
                Some(v) => format!(
                    "{} with the value {v} on the upstream request",
                    p.add_request_header
                ),
                None => format!("{} on the upstream request", p.add_request_header),
            },
            added_seen,
            added_ok,
        ),
        Expectation::new(
            "The removed request header did not reach the upstream",
            format!(
                "{} (sent by the client) absent from the upstream request",
                p.remove_request_header
            ),
            removed_seen,
            up.is_some() && removed.is_none(),
        ),
        Expectation::new(
            "The gateway added the response header",
            format!("{} on the answer", p.add_response_header),
            match (s, answered) {
                (Some(s), _) if s.status == 0 => "no answer".to_string(),
                (_, Some(_)) => "present".to_string(),
                _ => "absent".to_string(),
            },
            s.is_some_and(|s| s.status != 0) && answered.is_some(),
        ),
    ]
}

/// `circuit_breaker`: the requests before the trials reach the upstream
/// until one is answered without it; after the wait a trial reaches it.
fn breaker(sent: &[Sent], half_open_ms: u64) -> Vec<Expectation> {
    let before: Vec<&Sent> = sent.iter().filter(|s| !s.trial).collect();
    let trials: Vec<&Sent> = sent.iter().filter(|s| s.trial).collect();
    let forwarded = before.iter().take_while(|s| !s.upstream.is_empty()).count();
    let own = before
        .get(forwarded)
        .filter(|s| s.status != 0 && s.upstream.is_empty());
    let opened = forwarded > 0 && own.is_some();
    let seen = match own {
        Some(s) if forwarded > 0 => format!(
            "{forwarded} request(s) reached the upstream, then request {} was answered {} by the gateway without reaching it",
            s.n + 1,
            s.status
        ),
        Some(s) => format!(
            "request 1 was answered {} without reaching the upstream: the route does not reach it",
            s.status
        ),
        None if before.iter().any(|s| s.status == 0) => format!(
            "{forwarded} request(s) reached the upstream, then the gateway did not answer"
        ),
        None => format!("all {} requests reached the upstream", before.len()),
    };
    let reached = trials.iter().position(|s| !s.upstream.is_empty());
    let trial_seen = match (opened, reached) {
        (false, _) => "no trial: the circuit never opened".to_string(),
        (true, Some(i)) => format!(
            "trial {} reached the upstream (upstream answered {})",
            i + 1,
            list(trials[i].upstream.iter().map(|c| c.to_string()))
        ),
        (true, None) => format!(
            "{} trial(s), none reached the upstream ({})",
            trials.len(),
            list(trials.iter().map(|s| s.status_text()))
        ),
    };
    vec![
        Expectation::new(
            "The gateway opened the circuit after upstream failures",
            format!(
                "requests reach the upstream (it answers 503) until the gateway answers one on its own, within {MAX_BREAKER_REQUESTS} requests"
            ),
            seen,
            opened,
        ),
        Expectation::new(
            "After the half-open time a trial request reached the upstream",
            format!("a request sent {half_open_ms} ms after the circuit opened reached the upstream"),
            trial_seen,
            opened && reached.is_some(),
        ),
    ]
}

/// `request_size_limit`: the small body through, the large one refused.
fn size_limit(sent: &[Sent], limit: u64) -> Vec<Expectation> {
    let small = sent
        .iter()
        .find(|s| s.bytes.is_some_and(|b| b as u64 <= limit));
    let large = sent
        .iter()
        .find(|s| s.bytes.is_some_and(|b| b as u64 > limit));
    let bytes = |s: Option<&Sent>| s.and_then(|s| s.bytes).unwrap_or(0);
    let status = |s: Option<&Sent>| {
        s.map(Sent::status_text)
            .unwrap_or_else(|| "not sent".into())
    };
    let ups = |s: Option<&Sent>| s.map(|s| s.upstream.len()).unwrap_or(0);
    vec![
        Expectation::new(
            "A body under the limit reached the upstream",
            format!(
                "2xx for the {} byte body, and it reached the upstream",
                bytes(small)
            ),
            format!("{}, {} upstream request(s)", status(small), ups(small)),
            small.is_some_and(|s| (200..300).contains(&s.status) && !s.upstream.is_empty()),
        ),
        Expectation::new(
            "The gateway refused the body over the limit with 413",
            format!("413 for the {} byte body", bytes(large)),
            status(large),
            large.is_some_and(|s| s.status == 413),
        ),
        Expectation::new(
            "The body over the limit never reached the upstream",
            "0 upstream requests for it",
            format!(
                "{} upstream request(s){}",
                ups(large),
                large
                    .filter(|s| !s.upstream.is_empty())
                    .map(|s| format!(
                        " (upstream answered {})",
                        list(s.upstream.iter().map(|c| c.to_string()))
                    ))
                    .unwrap_or_default()
            ),
            large.is_some_and(|s| s.status != 0 && s.upstream.is_empty()),
        ),
    ]
}

fn list(items: impl Iterator<Item = String>) -> String {
    let v: Vec<String> = items.collect();
    if v.is_empty() {
        "none".into()
    } else {
        v.join(", ")
    }
}

/// One request of a check and what it caused upstream.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Sent {
    pub n: usize,
    /// `0` when no answer came (the error says why).
    pub status: u16,
    pub ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The statuses of the upstream requests it caused, in order.
    pub upstream: Vec<u16>,
    /// The size of the body it sent (`request_size_limit`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<usize>,
    /// A trial request after the half-open wait (`circuit_breaker`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub trial: bool,
    /// What the request was (`cors`: which preflight, or the GET).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<&'static str>,
    /// The value of the header the client sent for the gateway to remove
    /// (`header_transformation`); never shown.
    #[serde(skip)]
    pub marker: Option<String>,
    /// The answer's headers (bounded), lowercased names; never shown (the
    /// gateway wrote them: the expectations describe them instead).
    #[serde(skip)]
    pub answer: Vec<(String, String)>,
    /// The headers of each upstream request it caused, as the inspector
    /// captured them; never shown.
    #[serde(skip)]
    pub upstream_headers: Vec<Vec<(String, String)>>,
    /// The path of each upstream request it caused (the AI checks tell the
    /// targets and the embedding calls apart by it); never shown.
    #[serde(skip)]
    pub upstream_paths: Vec<String>,
    /// The body of each upstream request it caused, as the inspector kept
    /// it (bounded); never shown.
    #[serde(skip)]
    pub upstream_bodies: Vec<String>,
    /// The answer's body (the AI checks, bounded); never shown.
    #[serde(skip)]
    pub body: Option<String>,
    /// The tokens the answer says the model used (`usage.total_tokens`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// The bytes of the answer's body read (the MCP checks; at most one
    /// past the limit they read up to).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received: Option<usize>,
}

impl Sent {
    /// The first answer header of that name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.answer
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn status_text(&self) -> String {
        if self.status == 0 {
            "no answer".into()
        } else {
            self.status.to_string()
        }
    }
}

/// One line of the verdict.
#[derive(Clone, Debug, Serialize)]
pub struct Expectation {
    pub name: &'static str,
    pub expected: String,
    pub seen: String,
    pub ok: bool,
}

impl Expectation {
    fn new(
        name: &'static str,
        expected: impl Into<String>,
        seen: impl Into<String>,
        ok: bool,
    ) -> Self {
        Self {
            name,
            expected: expected.into(),
            seen: seen.into(),
            ok,
        }
    }
}
