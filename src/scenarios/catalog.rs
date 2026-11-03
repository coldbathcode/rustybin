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

/// A test parameter of a check: an integer with bounds and a default.
#[derive(Clone, Copy, Debug)]
pub struct Param {
    pub name: &'static str,
    /// A short label (the console's field).
    pub label: &'static str,
    pub description: &'static str,
    pub min: u64,
    pub max: u64,
    pub default: u64,
}

impl Param {
    fn view(&self) -> Value {
        json!({
            "name": self.name,
            "label": self.label,
            "description": format!("{} ({} to {}, default {})", self.description, self.min, self.max, self.default),
            "minimum": self.min,
            "maximum": self.max,
            "default": self.default,
        })
    }
}

pub const LIMIT: Param = Param {
    name: "limit",
    label: "Limit set on the gateway",
    description: "The limit set on the gateway: the check sends twice as many requests",
    min: 1,
    max: MAX_LIMIT as u64,
    default: DEFAULT_LIMIT as u64,
};
pub const HALF_OPEN_MS: Param = Param {
    name: "half_open_ms",
    label: "Half-open time on the gateway (ms)",
    description: "How long the gateway keeps the circuit open before it lets a trial request through, in milliseconds: the check waits that long",
    min: MIN_HALF_OPEN_MS,
    max: MAX_HALF_OPEN_MS,
    default: DEFAULT_HALF_OPEN_MS,
};
pub const SIZE_LIMIT: Param = Param {
    name: "size_limit",
    label: "Body limit on the gateway (bytes)",
    description: "The request body limit set on the gateway, in bytes: the check sends a body half as large and one twice as large",
    min: MIN_SIZE_LIMIT,
    max: MAX_SIZE_LIMIT,
    default: DEFAULT_SIZE_LIMIT,
};

/// Every test parameter, whatever the scenario (the MCP tool, the CLI and
/// the OpenAPI document list these).
pub const PARAMS: &[Param] = &[LIMIT, HALF_OPEN_MS, SIZE_LIMIT];

/// The JSON Schema properties of every test parameter (the check's body,
/// the MCP tool): `<scenario> only: <description>`.
pub fn param_properties() -> serde_json::Map<String, Value> {
    PARAMS
        .iter()
        .map(|p| {
            let owners: Vec<&str> = SCENARIOS
                .iter()
                .filter(|s| s.params.iter().any(|q| q.name == p.name))
                .map(|s| s.id)
                .collect();
            (
                p.name.to_string(),
                json!({
                    "type": "integer",
                    "minimum": p.min,
                    "maximum": p.max,
                    "default": p.default,
                    "description": format!("{} only: {}", owners.join(", "), p.description),
                }),
            )
        })
        .collect()
}

/// The test parameters of one check, each resolved to its value.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub limit: u32,
    pub half_open_ms: u64,
    pub size_limit: u64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            limit: DEFAULT_LIMIT,
            half_open_ms: DEFAULT_HALF_OPEN_MS,
            size_limit: DEFAULT_SIZE_LIMIT,
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
            "retries" | "caching" => 3,
            "rate_limiting" => (limit * 2) as usize,
            "credential_injection" | "request_size_limit" => 2,
            "circuit_breaker" => MAX_BREAKER_REQUESTS + BREAKER_TRIALS,
            _ => 1,
        }
    }

    /// The test parameters of a check: each given one belongs to this
    /// scenario and is in its bounds; the others take their defaults.
    pub fn params_of(
        &self,
        limit: Option<u64>,
        half_open_ms: Option<u64>,
        size_limit: Option<u64>,
    ) -> Result<Params, String> {
        let mut p = Params::default();
        for (param, given) in [
            (LIMIT, limit),
            (HALF_OPEN_MS, half_open_ms),
            (SIZE_LIMIT, size_limit),
        ] {
            let Some(v) = given else { continue };
            if !self.params.iter().any(|q| q.name == param.name) {
                let owners: Vec<&str> = SCENARIOS
                    .iter()
                    .filter(|s| s.params.iter().any(|q| q.name == param.name))
                    .map(|s| s.id)
                    .collect();
                return Err(format!(
                    "{} applies to {} only",
                    param.name,
                    owners.join(", ")
                ));
            }
            if !(param.min..=param.max).contains(&v) {
                return Err(format!(
                    "{}: {} to {} ({})",
                    param.name, param.min, param.max, param.description
                ));
            }
            match param.name {
                "limit" => p.limit = v as u32,
                "half_open_ms" => p.half_open_ms = v,
                _ => p.size_limit = v,
            }
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
        let spec = json!({
            "openapi": "3.0.3",
            "info": { "title": format!("Gateway scenario upstream: {}", self.title), "version": "1.0.0" },
            "paths": { path: { method: operation_doc } }
        });
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
            _ => Vec::new(),
        }
    }
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
}

impl Sent {
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
