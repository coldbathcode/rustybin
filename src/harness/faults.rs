//! Fault injection (`docs/plans/harness.md`, section 8, phase 7): what the
//! clients do wrong on purpose, and what the server nodes do to authentic
//! harness traffic. A scenario's `faults` block names a preset and
//! overrides any field; the live change `faults` replaces it while a run
//! runs. Client faults are drawn per request by fraction (seeded by the run
//! and the request, so every node draws the same way); server faults travel
//! to the server nodes with the run and act on marked requests of the
//! shaped endpoint `/harness/{*path}`, on `/health`, or on the whole node
//! (churn).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::scenario::Dur;

/// The presets, in the order the schema lists them.
pub const PRESETS: &[&str] = &[
    "calm",
    "jittery",
    "flaky-upstream",
    "retries",
    "synchronised",
    "churn",
    "all",
];

/// Most bytes of an oversized header.
pub const MAX_HEADER_BYTES: u32 = 64 * 1024;
/// Slowest request body trickle.
pub const MIN_BODY_RATE: u64 = 16;
/// Most retries of a 5xx.
pub const MAX_RETRIES: u32 = 10;
/// The longest a stall, a down window or a flap lasts.
pub const MAX_WINDOW_SECS: u64 = 3600;

fn fraction_ok(v: f64) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}

/// Abandoned requests: cancelled after `after`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Abandon {
    pub fraction: f64,
    pub after: Dur,
}

/// Slow request bodies: a bounded trickle.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlowBody {
    pub fraction: f64,
    pub bytes_per_second: u64,
}

/// Oversized headers near common limits.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Oversized {
    pub fraction: f64,
    /// The size of the header added (default 8192).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u32>,
}

/// Immediate retries on 5xx.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retry5xx {
    pub retries: u32,
    /// Retry at the next whole second of the run clock, so every node's
    /// retries land together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synchronised: Option<bool>,
}

/// What the clients do wrong, each a fraction of the requests.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientFaults {
    /// A new connection for the request (closed after it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_churn: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abandon: Option<Abandon>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slow_body: Option<SlowBody>,
    /// A JSON body cut short (expects a 4xx).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub malformed_json: Option<f64>,
    /// `Content-Type: text/plain` on a JSON body (expects a 4xx).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrong_content_type: Option<f64>,
    /// An extra header of `bytes` (expects a 4xx).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oversized_headers: Option<Oversized>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_on_5xx: Option<Retry5xx>,
    /// Credential churn: a minted JWT that expired (needs `identities.jwt`;
    /// expects a 4xx).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expired_tokens: Option<f64>,
    /// Protocol mix: the fraction sent over HTTP/2 (TLS targets with ALPN,
    /// or h2c prior knowledge on plain ones); the rest over HTTP/1.1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http2: Option<f64>,
}

/// A stall after the headers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stall {
    pub fraction: f64,
    #[serde(rename = "for")]
    pub hold: Dur,
}

/// `/health` toggled on a schedule.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flapping {
    pub every: Dur,
    pub down_for: Dur,
}

/// Node churn: the server nodes take turns going down.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Churn {
    pub every: Dur,
    pub down_for: Dur,
}

/// What the server nodes do to marked requests (the shaped endpoint).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerFaults {
    /// The connection is reset after the headers, with no body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall: Option<Stall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flapping_health: Option<Flapping>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub churn: Option<Churn>,
}

impl ServerFaults {
    pub fn is_empty(&self) -> bool {
        self.drop.is_none()
            && self.stall.is_none()
            && self.flapping_health.is_none()
            && self.churn.is_none()
    }
}

/// The scenario's `faults` block.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Faults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientFaults>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerFaults>,
}

fn secs(s: u64) -> Dur {
    Dur::secs(s)
}

fn ms(m: u64) -> Dur {
    Dur(std::time::Duration::from_millis(m))
}

/// The faults of a preset (`None` for an unknown name).
pub fn preset(name: &str) -> Option<Faults> {
    let jittery = ClientFaults {
        connection_churn: Some(0.2),
        abandon: Some(Abandon {
            fraction: 0.02,
            after: ms(100),
        }),
        slow_body: Some(SlowBody {
            fraction: 0.02,
            bytes_per_second: 1024,
        }),
        http2: Some(0.5),
        ..ClientFaults::default()
    };
    let flaky = ServerFaults {
        drop: Some(0.02),
        stall: Some(Stall {
            fraction: 0.01,
            hold: secs(5),
        }),
        flapping_health: Some(Flapping {
            every: secs(30),
            down_for: secs(5),
        }),
        churn: None,
    };
    let retries = ClientFaults {
        retry_on_5xx: Some(Retry5xx {
            retries: 2,
            synchronised: None,
        }),
        ..ClientFaults::default()
    };
    let synchronised = ClientFaults {
        retry_on_5xx: Some(Retry5xx {
            retries: 2,
            synchronised: Some(true),
        }),
        ..ClientFaults::default()
    };
    let churn = ServerFaults {
        churn: Some(Churn {
            every: secs(60),
            down_for: secs(10),
        }),
        ..ServerFaults::default()
    };
    Some(match name {
        "calm" => Faults::default(),
        "jittery" => Faults {
            client: Some(jittery),
            ..Faults::default()
        },
        "flaky-upstream" => Faults {
            server: Some(flaky),
            ..Faults::default()
        },
        "retries" => Faults {
            client: Some(retries),
            ..Faults::default()
        },
        "synchronised" => Faults {
            client: Some(synchronised),
            ..Faults::default()
        },
        "churn" => Faults {
            server: Some(churn),
            ..Faults::default()
        },
        "all" => Faults {
            client: Some(ClientFaults {
                malformed_json: Some(0.01),
                wrong_content_type: Some(0.01),
                oversized_headers: Some(Oversized {
                    fraction: 0.01,
                    bytes: None,
                }),
                expired_tokens: Some(0.02),
                ..synchronised
            }),
            server: Some(ServerFaults {
                churn: churn.churn,
                ..flaky
            }),
            preset: None,
        },
        _ => return None,
    })
    .map(|mut f| {
        f.preset = Some(name.to_string());
        f
    })
}

impl Faults {
    /// The preset's faults with this block's fields on top: a field set
    /// here replaces the preset's.
    pub fn resolve(&self) -> Faults {
        let base = self.preset.as_deref().and_then(preset).unwrap_or_default();
        let client = match (base.client, &self.client) {
            (Some(b), Some(c)) => Some(ClientFaults {
                connection_churn: c.connection_churn.or(b.connection_churn),
                abandon: c.abandon.clone().or(b.abandon),
                slow_body: c.slow_body.clone().or(b.slow_body),
                malformed_json: c.malformed_json.or(b.malformed_json),
                wrong_content_type: c.wrong_content_type.or(b.wrong_content_type),
                oversized_headers: c.oversized_headers.clone().or(b.oversized_headers),
                retry_on_5xx: c.retry_on_5xx.clone().or(b.retry_on_5xx),
                expired_tokens: c.expired_tokens.or(b.expired_tokens),
                http2: c.http2.or(b.http2),
            }),
            (b, c) => c.clone().or(b),
        };
        let server = match (base.server, &self.server) {
            (Some(b), Some(s)) => Some(ServerFaults {
                drop: s.drop.or(b.drop),
                stall: s.stall.clone().or(b.stall),
                flapping_health: s.flapping_health.clone().or(b.flapping_health),
                churn: s.churn.clone().or(b.churn),
            }),
            (b, s) => s.clone().or(b),
        };
        Faults {
            preset: self.preset.clone(),
            client,
            server,
        }
    }

    /// The resolved client faults.
    pub fn client(&self) -> ClientFaults {
        self.resolve().client.unwrap_or_default()
    }

    /// The resolved server faults (`None` when there are none).
    pub fn server(&self) -> Option<ServerFaults> {
        self.resolve().server.filter(|s| !s.is_empty())
    }

    /// Every problem with the block, prefixed with `here`.
    pub fn validate(&self, here: &str, problems: &mut Vec<String>) {
        if let Some(p) = &self.preset {
            if preset(p).is_none() {
                problems.push(format!(
                    "{here}.preset: unknown preset {p:?} (one of {})",
                    PRESETS.join(", ")
                ));
            }
        }
        let fraction = |name: &str, v: Option<f64>, problems: &mut Vec<String>| {
            if v.is_some_and(|v| !fraction_ok(v)) {
                problems.push(format!("{here}.{name}: a fraction from 0 to 1"));
            }
        };
        let window = |name: &str, d: Dur, problems: &mut Vec<String>| {
            if d.0.is_zero() || d.0.as_secs() > MAX_WINDOW_SECS {
                problems.push(format!("{here}.{name}: longer than 0, at most 1h"));
            }
        };
        if let Some(c) = &self.client {
            fraction("client.connection_churn", c.connection_churn, problems);
            fraction("client.malformed_json", c.malformed_json, problems);
            fraction("client.wrong_content_type", c.wrong_content_type, problems);
            fraction("client.expired_tokens", c.expired_tokens, problems);
            fraction("client.http2", c.http2, problems);
            if let Some(a) = &c.abandon {
                fraction("client.abandon.fraction", Some(a.fraction), problems);
                window("client.abandon.after", a.after, problems);
            }
            if let Some(s) = &c.slow_body {
                fraction("client.slow_body.fraction", Some(s.fraction), problems);
                if s.bytes_per_second < MIN_BODY_RATE {
                    problems.push(format!(
                        "{here}.client.slow_body.bytes_per_second: at least {MIN_BODY_RATE}"
                    ));
                }
            }
            if let Some(o) = &c.oversized_headers {
                fraction(
                    "client.oversized_headers.fraction",
                    Some(o.fraction),
                    problems,
                );
                if o.bytes.is_some_and(|b| b == 0 || b > MAX_HEADER_BYTES) {
                    problems.push(format!(
                        "{here}.client.oversized_headers.bytes: 1 to {MAX_HEADER_BYTES}"
                    ));
                }
            }
            if let Some(r) = &c.retry_on_5xx {
                if r.retries == 0 || r.retries > MAX_RETRIES {
                    problems.push(format!(
                        "{here}.client.retry_on_5xx.retries: 1 to {MAX_RETRIES}"
                    ));
                }
            }
        }
        if let Some(s) = &self.server {
            fraction("server.drop", s.drop, problems);
            if let Some(st) = &s.stall {
                fraction("server.stall.fraction", Some(st.fraction), problems);
                window("server.stall.for", st.hold, problems);
            }
            for (name, sched) in [
                (
                    "flapping_health",
                    s.flapping_health.as_ref().map(|f| (f.every, f.down_for)),
                ),
                ("churn", s.churn.as_ref().map(|c| (c.every, c.down_for))),
            ] {
                if let Some((every, down)) = sched {
                    window(&format!("server.{name}.every"), every, problems);
                    window(&format!("server.{name}.down_for"), down, problems);
                    if down.0 >= every.0 {
                        problems.push(format!("{here}.server.{name}.down_for: shorter than every"));
                    }
                }
            }
        }
    }
}

/// Whether request `seq` of a run draws fault `slot` at `fraction`: the
/// same answer on every node for the same request.
pub fn draws(seed: u64, seq: u64, slot: u8, fraction: f64) -> bool {
    if fraction <= 0.0 {
        return false;
    }
    if fraction >= 1.0 {
        return true;
    }
    let mut x = seed ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (u64::from(slot) << 56);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 11) as f64 / (1u64 << 53) as f64 <= fraction
}

/// The schema of the `faults` block.
pub fn schema(duration: impl Fn(&str, u64) -> Value) -> Value {
    let fraction =
        |d: &str| json!({ "type": "number", "description": d, "minimum": 0.0, "maximum": 1.0 });
    json!({
        "type": "object",
        "description": "Fault injection: what the clients do wrong on purpose (by fraction of the requests, drawn the same way on every node) and what the server nodes do to the run's marked requests at /harness/* and to /health. A preset fills the block; fields set here replace the preset's.",
        "properties": {
            "preset": { "type": "string", "description": "calm (nothing), jittery (connection churn, abandoned requests, slow bodies, an HTTP/2 mix), flaky-upstream (drops, stalls, flapping health), retries (immediate retries on 5xx), synchronised (the retries at the same tick on every node), churn (server nodes going down in turn), all.", "enum": PRESETS },
            "client": {
                "type": "object",
                "description": "Client behaviours.",
                "properties": {
                    "connection_churn": fraction("Requests sent on a new connection, closed after the answer (TLS handshakes included)."),
                    "abandon": { "type": "object", "description": "Requests cancelled before their answer.", "properties": {
                        "fraction": fraction("The fraction abandoned."),
                        "after": duration("How long after sending they are cancelled.", MAX_WINDOW_SECS)
                    }, "required": ["fraction", "after"], "additionalProperties": false },
                    "slow_body": { "type": "object", "description": "Request bodies sent as a trickle (requests with a body).", "properties": {
                        "fraction": fraction("The fraction sent slowly."),
                        "bytes_per_second": { "type": "integer", "description": "The trickle's rate.", "minimum": MIN_BODY_RATE, "maximum": 1_000_000_000u64 }
                    }, "required": ["fraction", "bytes_per_second"], "additionalProperties": false },
                    "malformed_json": fraction("JSON bodies cut short (they expect a 4xx)."),
                    "wrong_content_type": fraction("JSON bodies sent as text/plain (they expect a 4xx)."),
                    "oversized_headers": { "type": "object", "description": "An extra header near common size limits (they expect a 4xx).", "properties": {
                        "fraction": fraction("The fraction with the header."),
                        "bytes": { "type": "integer", "description": "The header's size (default 8192).", "minimum": 1, "maximum": MAX_HEADER_BYTES }
                    }, "required": ["fraction"], "additionalProperties": false },
                    "retry_on_5xx": { "type": "object", "description": "Immediate retries of answers with a 5xx.", "properties": {
                        "retries": { "type": "integer", "description": "Retries per request.", "minimum": 1, "maximum": MAX_RETRIES },
                        "synchronised": { "type": "boolean", "description": "Retry at the next whole second of the run clock, so every node's retries land together." }
                    }, "required": ["retries"], "additionalProperties": false },
                    "expired_tokens": fraction("Credential churn: minted JWTs (identities.jwt) sent expired (they expect a 4xx)."),
                    "http2": fraction("Protocol mix: the fraction sent over HTTP/2 (TLS targets by ALPN, h2c prior knowledge on plain ones), the rest over HTTP/1.1.")
                },
                "additionalProperties": false
            },
            "server": {
                "type": "object",
                "description": "Server node behaviours on the run's marked requests at /harness/* and on /health.",
                "properties": {
                    "drop": fraction("Answers whose connection is reset after the headers."),
                    "stall": { "type": "object", "description": "Answers that stall after the headers.", "properties": {
                        "fraction": fraction("The fraction stalled."),
                        "for": duration("How long the stall lasts (longer than the client's timeout: a timeout).", MAX_WINDOW_SECS)
                    }, "required": ["fraction", "for"], "additionalProperties": false },
                    "flapping_health": { "type": "object", "description": "/health answers 503 on a schedule (what the gateway's health checks see).", "properties": {
                        "every": duration("The period.", MAX_WINDOW_SECS),
                        "down_for": duration("How long of each period /health is unhealthy.", MAX_WINDOW_SECS)
                    }, "required": ["every", "down_for"], "additionalProperties": false },
                    "churn": { "type": "object", "description": "Node churn: in each period one server node (in turn) stops serving marked requests (503, connection closed) and its /health answers 503, which exercises the gateway's health checks and circuit breakers.", "properties": {
                        "every": duration("The period.", MAX_WINDOW_SECS),
                        "down_for": duration("How long the node stays down.", MAX_WINDOW_SECS)
                    }, "required": ["every", "down_for"], "additionalProperties": false }
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_resolve_and_fields_override() {
        for name in PRESETS {
            assert!(preset(name).is_some(), "{name}");
        }
        let f: Faults = serde_yaml::from_str(
            "preset: jittery\nclient: { connection_churn: 0.5 }\nserver: { drop: 0.1 }",
        )
        .unwrap();
        let r = f.resolve();
        let c = r.client.unwrap();
        assert_eq!(c.connection_churn, Some(0.5));
        assert_eq!(c.http2, Some(0.5));
        assert_eq!(r.server.unwrap().drop, Some(0.1));
        assert!(preset("all").unwrap().server().is_some());
        assert!(Faults::default().server().is_none());
        let mut p = Vec::new();
        serde_yaml::from_str::<Faults>(
            "preset: wild\nclient: { abandon: { fraction: 2, after: 0s }, retry_on_5xx: { retries: 0 } }\nserver: { churn: { every: 1s, down_for: 2s } }",
        )
        .unwrap()
        .validate("faults", &mut p);
        assert_eq!(p.len(), 5, "{p:?}");
    }

    #[test]
    fn draws_follow_the_fraction() {
        let n = (0..10_000).filter(|s| draws(7, *s, 1, 0.1)).count();
        assert!((800..1200).contains(&n), "{n}");
        assert!(!draws(7, 1, 1, 0.0));
        assert!(draws(7, 1, 1, 1.0));
        assert_eq!(draws(7, 42, 2, 0.3), draws(7, 42, 2, 0.3));
    }
}
