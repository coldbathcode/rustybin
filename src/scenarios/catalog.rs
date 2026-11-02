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
pub const SLOW_MS: u64 = if cfg!(test) { 600 } else { 3000 };
/// The header the gateway adds in `credential_injection`.
pub const KEY_HEADER: &str = "X-Upstream-Key";
/// The rate limit the check assumes when the caller gives none.
pub const DEFAULT_LIMIT: u32 = 5;
/// The largest rate limit a check accepts (it sends twice as many requests).
pub const MAX_LIMIT: u32 = 20;

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

    /// How many requests a check sends.
    pub fn requests(&self, limit: u32) -> usize {
        match self.id {
            "retries" | "caching" => 3,
            "rate_limiting" => (limit * 2) as usize,
            "credential_injection" => 2,
            _ => 1,
        }
    }

    /// The catalogue entry.
    pub fn view(&self) -> Value {
        let mut v = json!({
            "id": self.id,
            "title": self.title,
            "proves": self.proves,
            "upstream": match self.upstream {
                Upstream::Mock => "a mock created for the run".to_string(),
                Upstream::Flaky(p) => format!("{p} on this instance"),
            },
            "setup": self.setup_for("{upstream}", Some("{key}")),
            "requests": self.requests(DEFAULT_LIMIT),
        });
        if self.id == "rate_limiting" {
            v["parameters"] = json!([{ "name": "limit", "description": format!("The limit set on the gateway (1 to {MAX_LIMIT}, default {DEFAULT_LIMIT}): the check sends twice as many requests") }]);
        }
        v
    }

    /// The upstream mock's spec and settings (`Upstream::Mock` scenarios).
    pub fn mock(&self, key: &str) -> Option<(String, MockSettings)> {
        let (path, op, summary) = match self.id {
            "rate_limiting" => ("/items", "listItems", "Items"),
            "credential_injection" => ("/profile", "getProfile", "The caller's profile"),
            "timeouts" => ("/slow", "slowAnswer", "A slow answer"),
            "caching" => ("/catalog", "getCatalog", "A cacheable catalogue"),
            _ => return None,
        };
        let spec = json!({
            "openapi": "3.0.3",
            "info": { "title": format!("Gateway scenario upstream: {}", self.title), "version": "1.0.0" },
            "paths": { path: { "get": {
                "operationId": op,
                "summary": summary,
                "responses": { "200": {
                    "description": summary,
                    "content": { "application/json": { "example": { "scenario": self.id, "ok": true } } }
                } }
            } } }
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
    pub fn evaluate(&self, sent: &[Sent], limit: u32) -> Vec<Expectation> {
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
            _ => Vec::new(),
        }
    }
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
