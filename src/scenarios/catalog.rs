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
];

pub fn find(id: &str) -> Option<&'static Scenario> {
    SCENARIOS.iter().find(|s| s.id == id)
}

impl Scenario {
    /// The setup steps for one run.
    pub fn setup_for(&self, upstream: &str, key: Option<&str>) -> Vec<String> {
        self.setup
            .iter()
            .map(|s| {
                s.replace("{upstream}", upstream)
                    .replace("{key}", key.unwrap_or(""))
                    .replace("{slow_ms}", &SLOW_MS.to_string())
            })
            .collect()
    }

    /// How many requests a check sends (at most, for `circuit_breaker`).
    pub fn requests(&self, limit: u32) -> usize {
        match self.id {
            "retries" | "caching" | "cors" => 3,
            "rate_limiting" => (limit * 2) as usize,
            "credential_injection" | "request_size_limit" => 2,
            "circuit_breaker" => MAX_BREAKER_REQUESTS + BREAKER_TRIALS,
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
            "proves": self.proves,
            "upstream": match self.upstream {
                Upstream::Mock => "a mock created for the run".to_string(),
                Upstream::Flaky(p) => format!("{p} on this instance"),
            },
            "setup": self.setup_for("{upstream}", Some("{key}")),
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
            _ => Vec::new(),
        }
    }
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
