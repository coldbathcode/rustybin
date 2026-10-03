//! The demo agents: their descriptions (used for the Agent Cards) and their
//! scripted behaviour.
//!
//! An agent turns an incoming message into a [`Plan`]: either a direct
//! [`Message`] reply (no task) or a list of [`Step`]s that the runner plays
//! back (with delays) as task status and artifact updates.

use serde_json::{json, Value};

use super::model::*;

pub struct SkillDef {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub tags: &'static [&'static str],
    pub examples: &'static [&'static str],
    pub input_modes: &'static [&'static str],
    pub output_modes: &'static [&'static str],
}

pub struct AgentDef {
    /// Path segment: `/a2a/{id}`.
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub input_modes: &'static [&'static str],
    pub output_modes: &'static [&'static str],
    pub skills: &'static [SkillDef],
    /// Requires a bearer token from the built-in IdP; has an extended card.
    pub secured: bool,
}

/// Skills only listed on the authenticated extended card of `secure`.
pub const SECURE_EXTENDED_SKILLS: &[SkillDef] = &[SkillDef {
    id: "audit-log",
    name: "Audit log",
    description: "Lists the identities that called this agent recently (extended card only, requires a token).",
    tags: &["security", "audit"],
    examples: &["show the audit log"],
    input_modes: &["text/plain"],
    output_modes: &["application/json"],
}];

pub const AGENTS: &[AgentDef] = &[
    AgentDef {
        id: "echo",
        name: "Echo Agent",
        description: "Echoes every part it receives (text, data and files). Returns a completed task with an `echo` artifact, or a direct message reply when the text starts with `msg:` or metadata.reply is \"message\".",
        input_modes: &["text/plain", "application/json", "*/*"],
        output_modes: &["text/plain", "application/json", "*/*"],
        skills: &[
            SkillDef {
                id: "echo",
                name: "Echo",
                description: "Returns the input parts unchanged as a task artifact (immediately completed).",
                tags: &["echo", "test", "debug"],
                examples: &["hello agent", "{\"ping\": true}"],
                input_modes: &["text/plain", "application/json", "*/*"],
                output_modes: &["text/plain", "application/json", "*/*"],
            },
            SkillDef {
                id: "echo-message",
                name: "Echo as message",
                description: "Replies with a direct Message instead of a Task (prefix the text with `msg:`).",
                tags: &["echo", "message"],
                examples: &["msg: hello"],
                input_modes: &["text/plain"],
                output_modes: &["text/plain"],
            },
        ],
        secured: false,
    },
    AgentDef {
        id: "weather",
        name: "Weather Agent",
        description: "Returns a deterministic (fake) forecast as a structured JSON data artifact plus a text summary.",
        input_modes: &["text/plain", "application/json"],
        output_modes: &["application/json", "text/plain"],
        skills: &[SkillDef {
            id: "forecast",
            name: "Weather forecast",
            description: "Three day forecast for a city, as `application/json` data and a text summary. Send text like \"weather in Paris\" or data {\"city\": \"Paris\"}.",
            tags: &["weather", "forecast", "structured-data"],
            examples: &["What is the weather in Paris?", "{\"city\": \"Tokyo\"}"],
            input_modes: &["text/plain", "application/json"],
            output_modes: &["application/json", "text/plain"],
        }],
        secured: false,
    },
    AgentDef {
        id: "travel-planner",
        name: "Travel Planner Agent",
        description: "Long-running agent: SUBMITTED -> WORKING with streamed status updates and artifact chunks (append / lastChunk) -> COMPLETED. Produces text, JSON data, a file URL and inline file bytes. metadata.stepDelayMs (0-5000) tunes the pace.",
        input_modes: &["text/plain"],
        output_modes: &["text/markdown", "application/json", "image/png", "text/plain"],
        skills: &[SkillDef {
            id: "plan-trip",
            name: "Plan a trip",
            description: "Plans a multi-day trip: streams the itinerary in chunks, then adds a JSON itinerary, a map image URL and an inline boarding pass.",
            tags: &["travel", "streaming", "long-running"],
            examples: &["Plan a 3 day trip to Lisbon", "Weekend to Kyoto"],
            input_modes: &["text/plain"],
            output_modes: &["text/markdown", "application/json", "image/png", "text/plain"],
        }],
        secured: false,
    },
    AgentDef {
        id: "approval",
        name: "Approval Agent",
        description: "Multi-turn agent: answers with TASK_STATE_INPUT_REQUIRED and continues when the client sends a follow-up message with the same taskId (\"approve\" or \"deny\").",
        input_modes: &["text/plain"],
        output_modes: &["text/plain", "application/json"],
        skills: &[SkillDef {
            id: "expense-approval",
            name: "Expense approval",
            description: "Asks for confirmation (input-required) before approving an expense.",
            tags: &["multi-turn", "input-required", "human-in-the-loop"],
            examples: &["Approve my $120 taxi expense", "approve"],
            input_modes: &["text/plain"],
            output_modes: &["text/plain", "application/json"],
        }],
        secured: false,
    },
    AgentDef {
        id: "flaky",
        name: "Flaky Agent",
        description: "Fails every task with TASK_STATE_FAILED (simulated upstream outage) unless the text contains \"succeed\".",
        input_modes: &["text/plain"],
        output_modes: &["text/plain"],
        skills: &[SkillDef {
            id: "unreliable",
            name: "Unreliable operation",
            description: "Demonstrates failed tasks for retry and alerting policies.",
            tags: &["failure", "testing", "reliability"],
            examples: &["do something", "please succeed this time"],
            input_modes: &["text/plain"],
            output_modes: &["text/plain"],
        }],
        secured: false,
    },
    AgentDef {
        id: "secure",
        name: "Secure Agent",
        description: "Requires a bearer token from the built-in OIDC provider (POST /oauth/token). Without one the task stops in TASK_STATE_AUTH_REQUIRED; resend with the same taskId and an Authorization header to continue. Has an authenticated extended card.",
        input_modes: &["text/plain"],
        output_modes: &["application/json", "text/plain"],
        skills: &[SkillDef {
            id: "whoami",
            name: "Who am I",
            description: "Returns the verified claims of the caller's access token.",
            tags: &["auth", "oauth2", "auth-required"],
            examples: &["who am I?"],
            input_modes: &["text/plain"],
            output_modes: &["application/json", "text/plain"],
        }],
        secured: true,
    },
    AgentDef {
        id: "reject",
        name: "Reject Agent",
        description: "Rejects every request with TASK_STATE_REJECTED.",
        input_modes: &["text/plain"],
        output_modes: &["text/plain"],
        skills: &[SkillDef {
            id: "reject",
            name: "Reject",
            description: "Always declines (TASK_STATE_REJECTED).",
            tags: &["rejected", "testing"],
            examples: &["book a flight"],
            input_modes: &["text/plain"],
            output_modes: &["text/plain"],
        }],
        secured: false,
    },
];

/// The agent served at `/a2a` and `/.well-known/agent-card.json`.
pub const DEFAULT_AGENT: &str = "echo";

pub fn find(id: &str) -> Option<&'static AgentDef> {
    AGENTS.iter().find(|a| a.id == id)
}

/// Whether `offered` (a media type of the agent) satisfies `wanted` (a
/// client media range such as `text/*` or `*/*`).
pub fn media_matches(wanted: &str, offered: &str) -> bool {
    let wanted = wanted
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let offered = offered.to_ascii_lowercase();
    if wanted == "*/*" || offered == "*/*" || wanted == offered {
        return true;
    }
    let (wt, ws) = wanted.split_once('/').unwrap_or((&wanted, ""));
    let (ot, os) = offered.split_once('/').unwrap_or((&offered, ""));
    (ws == "*" && wt == ot) || (os == "*" && wt == ot)
}

// ── Plans ───────────────────────────────────────────────────────────

/// Result of token verification for the request.
#[derive(Clone, Debug)]
pub enum AuthOutcome {
    Missing,
    Invalid(String),
    Valid(Value),
}

#[derive(Clone, Debug)]
pub enum StepKind {
    Status(TaskState, Option<String>),
    Artifact {
        artifact: Artifact,
        append: bool,
        last_chunk: bool,
    },
}

#[derive(Clone, Debug)]
pub struct Step {
    pub delay_ms: u64,
    pub kind: StepKind,
}

#[derive(Clone, Debug)]
pub enum Plan {
    /// Direct message reply, no task.
    Reply(Vec<Part>),
    /// Create (or continue) a task and play these steps.
    Run(Vec<Step>),
}

pub struct PlanInput<'a> {
    pub message: &'a Message,
    /// Id of the task the plan runs in.
    pub task_id: &'a str,
    /// The task being continued (follow-up message), if any.
    pub existing: Option<&'a Task>,
    pub auth: &'a AuthOutcome,
    pub base_url: &'a str,
    /// 1-based position of this task in its context.
    pub turn: usize,
    pub context_id: &'a str,
}

fn status(delay_ms: u64, state: TaskState, text: &str) -> Step {
    Step {
        delay_ms,
        kind: StepKind::Status(state, Some(text.to_string())),
    }
}

fn artifact(delay_ms: u64, a: Artifact, append: bool, last_chunk: bool) -> Step {
    Step {
        delay_ms,
        kind: StepKind::Artifact {
            artifact: a,
            append,
            last_chunk,
        },
    }
}

/// `metadata.stepDelayMs` (0..=5000) or the default.
fn step_delay(m: &Message, default: u64) -> u64 {
    m.metadata
        .as_ref()
        .and_then(|md| md.get("stepDelayMs"))
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .map(|v| v.min(5000))
        .unwrap_or(default)
}

pub fn plan(agent: &AgentDef, input: &PlanInput) -> Plan {
    match agent.id {
        "echo" => plan_echo(input),
        "weather" => plan_weather(input),
        "travel-planner" => plan_travel(input),
        "approval" => plan_approval(input),
        "flaky" => plan_flaky(input),
        "secure" => plan_secure(input),
        _ => plan_reject(input),
    }
}

fn plan_echo(input: &PlanInput) -> Plan {
    let m = input.message;
    let text = m.text();
    let wants_message = text.trim_start().to_ascii_lowercase().starts_with("msg:")
        || m.metadata
            .as_ref()
            .and_then(|md| md.get("reply"))
            .and_then(|v| v.as_str())
            == Some("message");
    if wants_message && input.existing.is_none() {
        return Plan::Reply(m.parts.clone());
    }
    let n = m.parts.len();
    Plan::Run(vec![
        artifact(
            0,
            Artifact::new(
                "echo",
                "echo",
                "The input parts, unchanged",
                m.parts.clone(),
            ),
            false,
            true,
        ),
        status(
            0,
            TaskState::Completed,
            &format!(
                "Echoed {n} part(s) (turn {} in context {})",
                input.turn, input.context_id
            ),
        ),
    ])
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn clean_words(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '\'')
        .collect();
    let mut out = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().count() > max {
        out = out.chars().take(max).collect();
    }
    out
}

fn capitalize(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Text after the last occurrence of `marker` (case-insensitive), cut at
/// the first of `stops`.
fn after(text: &str, marker: &str, stops: &[&str]) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let idx = lower.rfind(marker)?;
    let mut rest = &text[idx + marker.len()..];
    let rest_lower = rest.to_ascii_lowercase();
    let mut cut = rest.len();
    for s in stops {
        if let Some(i) = rest_lower.find(s) {
            cut = cut.min(i);
        }
    }
    rest = &rest[..cut];
    let w = clean_words(rest, 48);
    if w.is_empty() {
        None
    } else {
        Some(capitalize(&w))
    }
}

pub fn weather_city(m: &Message) -> String {
    for p in &m.parts {
        if let PartContent::Data(Value::Object(o)) = &p.content {
            if let Some(c) = o.get("city").and_then(|v| v.as_str()) {
                let c = clean_words(c, 48);
                if !c.is_empty() {
                    return capitalize(&c);
                }
            }
        }
    }
    let text = m.text();
    if let Some(c) = after(
        &text,
        " in ",
        &["?", ".", "!", ",", " on ", " for ", " today", " tomorrow"],
    ) {
        return c;
    }
    "Amsterdam".to_string()
}

pub fn forecast(city: &str) -> Value {
    const CONDITIONS: [&str; 6] = [
        "sunny",
        "partly cloudy",
        "cloudy",
        "light rain",
        "windy",
        "thunderstorms",
    ];
    let h = fnv(&city.to_ascii_lowercase());
    let base = (h % 26) as i64 + 5;
    let days: Vec<Value> = (0..3u64)
        .map(|d| {
            let hh = h.rotate_left((d * 7) as u32);
            json!({
                "day": d,
                "condition": CONDITIONS[(hh % CONDITIONS.len() as u64) as usize],
                "highC": base + (hh % 5) as i64,
                "lowC": base - 6 + (hh % 3) as i64,
                "precipitationChance": (hh % 100) as i64,
            })
        })
        .collect();
    json!({
        "city": city,
        "units": "metric",
        "current": {
            "temperatureC": base,
            "condition": CONDITIONS[(h % CONDITIONS.len() as u64) as usize],
            "humidity": 40 + (h % 50) as i64,
            "windKph": (h % 40) as i64,
        },
        "forecast": days,
        "source": "rustybin mock weather (deterministic, not real data)",
    })
}

fn plan_weather(input: &PlanInput) -> Plan {
    let city = weather_city(input.message);
    let data = forecast(&city);
    let summary = format!(
        "{}: {} and {} C now. Highs of {}, {}, {} C over the next three days.",
        city,
        data["current"]["condition"].as_str().unwrap_or("fine"),
        data["current"]["temperatureC"],
        data["forecast"][0]["highC"],
        data["forecast"][1]["highC"],
        data["forecast"][2]["highC"],
    );
    let d = step_delay(input.message, 100);
    Plan::Run(vec![
        status(
            d,
            TaskState::Working,
            &format!("Looking up the forecast for {city}"),
        ),
        artifact(
            d,
            Artifact::new(
                "forecast",
                "forecast",
                "Structured forecast (application/json) and a text summary",
                vec![Part::data(data), Part::text(summary.clone())],
            ),
            false,
            true,
        ),
        status(0, TaskState::Completed, &summary),
    ])
}

/// A tiny valid 1x1 PNG.
pub const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0xF0,
    0x1F, 0x00, 0x05, 0x00, 0x01, 0xFF, 0x89, 0x99, 0x3D, 0x1D, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

fn trip_days(text: &str) -> u64 {
    let lower = text.to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        if w.starts_with("day") && i > 0 {
            let prev = words[i - 1].trim_end_matches('-');
            if let Ok(n) = prev.parse::<u64>() {
                return n.clamp(1, 7);
            }
        }
        if let Some(num) = w.strip_suffix("-day") {
            if let Ok(n) = num.parse::<u64>() {
                return n.clamp(1, 7);
            }
        }
    }
    if lower.contains("weekend") {
        return 2;
    }
    3
}

fn plan_travel(input: &PlanInput) -> Plan {
    let text = input.message.text();
    let dest = after(
        &text,
        " to ",
        &["?", ".", "!", ",", " for ", " in ", " next", " this"],
    )
    .unwrap_or_else(|| "Lisbon".to_string());
    let days = trip_days(&text);
    let d = step_delay(input.message, 400);
    let id = "itinerary";
    let mk = |text: String| {
        Artifact::new(
            id,
            "itinerary.md",
            "Day by day itinerary (streamed in chunks)",
            vec![{
                let mut p = Part::text(text);
                p.media_type = Some("text/markdown".into());
                p
            }],
        )
    };
    let mut steps = vec![
        status(
            d,
            TaskState::Working,
            &format!("Searching flights to {dest}"),
        ),
        artifact(
            d,
            mk(format!("# {days} day trip to {dest}\n\n")),
            false,
            false,
        ),
        status(d, TaskState::Working, &format!("Finding hotels in {dest}")),
    ];
    const ACTIVITIES: [&str; 7] = [
        "old town walking tour",
        "local food market",
        "museum afternoon",
        "day trip to the coast",
        "cooking class",
        "sunset viewpoint",
        "free day",
    ];
    let h = fnv(&dest.to_ascii_lowercase()) as usize;
    for day in 1..=days {
        let act = ACTIVITIES[(h + day as usize) % ACTIVITIES.len()];
        steps.push(artifact(
            d / 2,
            mk(format!("- Day {day}: {act}\n")),
            true,
            false,
        ));
    }
    steps.push(artifact(
        d / 2,
        mk("\nHave a great trip!\n".into()),
        true,
        true,
    ));
    let itinerary = json!({
        "destination": dest,
        "days": days,
        "flight": {"code": format!("RB{}", 100 + h % 900), "departure": "08:15", "arrival": "11:40"},
        "hotel": {"name": format!("Hotel {dest} Central"), "nights": days.saturating_sub(1).max(1)},
        "estimatedCost": {"currency": "EUR", "amount": 350 + days * 120},
    });
    steps.push(artifact(
        d,
        Artifact::new(
            "itinerary-json",
            "itinerary.json",
            "Machine readable itinerary",
            vec![Part::data(itinerary)],
        ),
        false,
        true,
    ));
    steps.push(artifact(
        0,
        Artifact::new(
            "route-map",
            "map.png",
            "Route map (file part with a URL)",
            vec![Part::url(
                format!("{}/image/png", input.base_url),
                "map.png",
                "image/png",
            )],
        ),
        false,
        true,
    ));
    let pass = format!(
        "BOARDING PASS\nPassenger: A2A CLIENT\nTo: {dest}\nFlight: RB{}\nSeat: 12A\n",
        100 + h % 900
    );
    steps.push(artifact(
        0,
        Artifact::new(
            "boarding-pass",
            "boarding-pass.txt",
            "Boarding pass (file part with inline bytes)",
            vec![
                Part::raw(pass.as_bytes(), "boarding-pass.txt", "text/plain"),
                Part::raw(TINY_PNG, "qr.png", "image/png"),
            ],
        ),
        false,
        true,
    ));
    steps.push(status(
        d,
        TaskState::Completed,
        &format!("Your {days} day trip to {dest} is planned"),
    ));
    Plan::Run(steps)
}

fn plan_approval(input: &PlanInput) -> Plan {
    let text = input.message.text().to_ascii_lowercase();
    let d = step_delay(input.message, 50);
    let Some(existing) = input.existing else {
        let request = clean_words(&input.message.text(), 120);
        return Plan::Run(vec![
            status(d, TaskState::Working, "Reviewing the request"),
            status(
                0,
                TaskState::InputRequired,
                &format!(
                    "Approval needed for: \"{request}\". Reply \"approve\" or \"deny\" in a message with taskId {}",
                    input.task_id
                ),
            ),
        ]);
    };
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let has = |list: &[&str]| words.iter().any(|w| list.contains(w));
    let request = existing
        .history
        .first()
        .map(|m| clean_words(&m.text(), 120))
        .unwrap_or_default();
    let decision = |approved: bool| {
        Artifact::new(
            "decision",
            "decision",
            "Approval decision",
            vec![Part::data(json!({
                "request": request,
                "approved": approved,
                "decidedAt": now_ts(),
                "turns": existing.history.iter().filter(|m| m.role == Role::User).count() + 1,
            }))],
        )
    };
    if has(&["approve", "approved", "yes", "ok", "confirm"]) {
        Plan::Run(vec![
            status(d, TaskState::Working, "Processing the approval"),
            artifact(d, decision(true), false, true),
            status(0, TaskState::Completed, "Approved"),
        ])
    } else if has(&["deny", "denied", "no", "reject", "decline"]) {
        Plan::Run(vec![
            artifact(d, decision(false), false, true),
            status(0, TaskState::Completed, "Denied"),
        ])
    } else {
        Plan::Run(vec![status(
            d,
            TaskState::InputRequired,
            "Please answer \"approve\" or \"deny\"",
        )])
    }
}

fn plan_flaky(input: &PlanInput) -> Plan {
    let d = step_delay(input.message, 100);
    if input
        .message
        .text()
        .to_ascii_lowercase()
        .contains("succeed")
    {
        return Plan::Run(vec![
            status(d, TaskState::Working, "Calling the upstream service"),
            artifact(
                d,
                Artifact::new(
                    "result",
                    "result",
                    "",
                    vec![Part::text("Worked this time.")],
                ),
                false,
                true,
            ),
            status(0, TaskState::Completed, "Succeeded"),
        ]);
    }
    Plan::Run(vec![
        status(d, TaskState::Working, "Calling the upstream service"),
        status(
            d,
            TaskState::Failed,
            "Simulated upstream failure (503 from the inventory service). Include \"succeed\" in the text to make it pass.",
        ),
    ])
}

fn plan_secure(input: &PlanInput) -> Plan {
    let d = step_delay(input.message, 50);
    match input.auth {
        AuthOutcome::Valid(claims) => {
            let sub = claims
                .get("sub")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let pick = |k: &str| claims.get(k).cloned().unwrap_or(Value::Null);
            Plan::Run(vec![
                status(d, TaskState::Working, "Token verified"),
                artifact(
                    d,
                    Artifact::new(
                        "whoami",
                        "whoami",
                        "Verified access token claims",
                        vec![Part::data(json!({
                            "sub": pick("sub"),
                            "iss": pick("iss"),
                            "aud": pick("aud"),
                            "scope": pick("scope"),
                            "exp": pick("exp"),
                            "authenticated": true,
                        }))],
                    ),
                    false,
                    true,
                ),
                status(0, TaskState::Completed, &format!("Authenticated as {sub}")),
            ])
        }
        other => {
            let why = match other {
                AuthOutcome::Invalid(e) => format!("The bearer token was rejected ({e})."),
                _ => "No bearer token was presented.".to_string(),
            };
            Plan::Run(vec![status(
                0,
                TaskState::AuthRequired,
                &format!(
                    "{why} Get one with POST {}/oauth/token (grant_type=client_credentials, client_id=rustybin, client_secret=secret) and resend this message with the same taskId and an Authorization: Bearer header.",
                    input.base_url
                ),
            )])
        }
    }
}

fn plan_reject(input: &PlanInput) -> Plan {
    let _ = input;
    Plan::Run(vec![status(
        0,
        TaskState::Rejected,
        "Request declined: this agent rejects every task (demo of TASK_STATE_REJECTED).",
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(text: &str) -> Message {
        Message {
            message_id: "m".into(),
            context_id: None,
            task_id: None,
            role: Role::User,
            parts: vec![Part::text(text)],
            metadata: None,
            extensions: vec![],
            reference_task_ids: vec![],
        }
    }

    #[test]
    fn media_ranges() {
        assert!(media_matches("text/*", "text/plain"));
        assert!(media_matches("*/*", "image/png"));
        assert!(media_matches("application/json", "application/json"));
        assert!(media_matches("image/png", "*/*"));
        assert!(!media_matches("image/png", "text/plain"));
        assert!(media_matches("text/plain; charset=utf-8", "text/plain"));
    }

    #[test]
    fn extracts_cities_and_destinations() {
        assert_eq!(weather_city(&msg("What is the weather in paris?")), "Paris");
        assert_eq!(weather_city(&msg("hello")), "Amsterdam");
        assert_eq!(forecast("Paris")["forecast"], forecast("paris")["forecast"]);
        assert_eq!(trip_days("plan a 5 day trip"), 5);
        assert_eq!(trip_days("a 4-day trip"), 4);
        assert_eq!(trip_days("weekend"), 2);
        assert_eq!(trip_days("99 days"), 7);
    }

    #[test]
    fn every_agent_has_skills_and_unique_ids() {
        let mut ids = std::collections::HashSet::new();
        for a in AGENTS {
            assert!(ids.insert(a.id));
            assert!(!a.skills.is_empty());
        }
        assert!(find(DEFAULT_AGENT).is_some());
        assert_eq!(TINY_PNG[1..4], *b"PNG");
    }
}
