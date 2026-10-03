//! The provider-neutral mock LLM.
//!
//! Every provider module normalises its request into a [`ChatInput`]
//! (system prompt, messages with content parts, tools, tool choice,
//! response format, limits), calls [`generate`] and renders the [`Reply`]
//! in its native shape.
//!
//! Reply selection, in order:
//! 1. `echo` mode: the rendered prompt ([`render`]) exactly as received.
//! 2. The last message carries tool results: a final answer quoting them.
//! 3. Tools supplied and `tool_choice` is not `none`: a tool call when a tool
//!    is forced, or when the last user message mentions the tool's name or a
//!    description keyword (whole words). Arguments come from the tool's JSON
//!    schema ([`crate::ai::schema`]).
//! 4. Structured output requested: JSON valid against the schema (or a JSON
//!    object for plain JSON mode).
//! 5. Text from the mode: `canned` (keyword table), `scripted` (demo rules),
//!    `random` (seeded by the request hash).
//!
//! Then stop sequences and `max_tokens` are applied ([`crate::ai::tokens`]).

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::schema::{self, Hints};
use super::tokens;

/// How the reply text is chosen. Selected by `X-Rustybin-Mode` or by a model
/// name segment (see [`Mode::from_model`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Canned,
    Echo,
    Scripted,
    Random,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Canned, Mode::Echo, Mode::Scripted, Mode::Random];

    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "canned" | "default" => Some(Mode::Canned),
            "echo" => Some(Mode::Echo),
            "scripted" | "script" => Some(Mode::Scripted),
            "random" => Some(Mode::Random),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Canned => "canned",
            Mode::Echo => "echo",
            Mode::Scripted => "scripted",
            Mode::Random => "random",
        }
    }

    /// A model name selects a mode when one of its segments (split on
    /// `- _ : / .`) is a mode name: `rustybin-echo`, `echo`, `gpt-4o:scripted`,
    /// `random-model`.
    pub fn from_model(model: &str) -> Option<Mode> {
        model.split(['-', '_', ':', '/', '.', '@']).find_map(|seg| {
            match seg.to_ascii_lowercase().as_str() {
                "echo" => Some(Mode::Echo),
                "scripted" => Some(Mode::Scripted),
                "random" => Some(Mode::Random),
                "canned" => Some(Mode::Canned),
                _ => None,
            }
        })
    }

    /// Header override first, then the model name, else canned.
    pub fn resolve(header: Option<Mode>, model: &str) -> Mode {
        header
            .or_else(|| Mode::from_model(model))
            .unwrap_or(Mode::Canned)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// One content part of a message.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Text(String),
    /// An image (the string is a short description: URL or media type).
    Image(String),
    /// Audio input (format).
    Audio(String),
    /// A document / file (name or media type).
    File(String),
    ToolCall(ToolCall),
    ToolResult {
        id: String,
        name: Option<String>,
        content: String,
        is_error: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Msg {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl Msg {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            parts: vec![Part::Text(text.into())],
        }
    }

    /// Concatenated text parts.
    pub fn joined_text(&self) -> String {
        let texts: Vec<&str> = self
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        texts.join("\n")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// JSON schema of the arguments.
    pub parameters: Value,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    /// Some tool must be called (OpenAI `required`, Anthropic `any`, Gemini `ANY`).
    Required,
    Named(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Format {
    #[default]
    Text,
    JsonObject,
    JsonSchema(Value),
}

/// A normalised chat request.
#[derive(Clone, Debug, Default)]
pub struct ChatInput {
    pub model: String,
    pub system: Vec<String>,
    pub messages: Vec<Msg>,
    pub tools: Vec<Tool>,
    pub tool_choice: ToolChoice,
    pub format: Format,
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    /// Provider-neutral id (hex); providers add their own prefix.
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finish {
    Stop,
    /// A stop sequence matched (the sequence).
    StopSequence(String),
    Length,
    ToolCalls,
    ContentFilter,
}

#[derive(Clone, Debug)]
pub struct Reply {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish: Finish,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub mode: Mode,
}

impl Reply {
    /// Text token pieces (the streaming chunks).
    pub fn pieces(&self) -> Vec<&str> {
        tokens::pieces(&self.text)
    }
}

/// Options that do not come from the request body.
#[derive(Clone, Copy, Debug)]
pub struct GenOpts {
    pub mode: Mode,
    /// Index of the choice (`n > 1`): varies the random mode.
    pub choice: u32,
    /// Simulate an output content filter (empty text, `ContentFilter`).
    pub content_filter: bool,
    /// Public instance: lower caps.
    pub public: bool,
}

impl GenOpts {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            choice: 0,
            content_filter: false,
            public: false,
        }
    }
}

// ── Prompt rendering and token counting ─────────────────────────────

fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push_str("...");
        out
    }
}

/// The prompt as one text: what the upstream received, after any gateway
/// decoration. Used by echo mode, prompt token counting and the AI request
/// inspector.
pub fn render(input: &ChatInput) -> String {
    let mut out: Vec<String> = Vec::new();
    for s in &input.system {
        out.push(format!("system: {s}"));
    }
    if !input.tools.is_empty() {
        let names: Vec<&str> = input.tools.iter().map(|t| t.name.as_str()).collect();
        out.push(format!("tools: {}", names.join(", ")));
    }
    for m in &input.messages {
        let role = m.role.as_str();
        for p in &m.parts {
            match p {
                Part::Text(t) => out.push(format!("{role}: {t}")),
                Part::Image(d) => out.push(format!("{role}: [image {}]", short(d, 60))),
                Part::Audio(f) => out.push(format!("{role}: [audio {f}]")),
                Part::File(f) => out.push(format!("{role}: [file {}]", short(f, 60))),
                Part::ToolCall(c) => {
                    out.push(format!("{role}: [tool_call {}] {}", c.name, c.arguments))
                }
                Part::ToolResult { name, content, .. } => out.push(format!(
                    "tool ({}): {content}",
                    name.as_deref().unwrap_or("result")
                )),
            }
        }
    }
    out.join("\n")
}

/// Prompt tokens: the rendered prompt, the tool definitions and a fixed
/// cost per image.
pub fn prompt_tokens(input: &ChatInput) -> u32 {
    let images = input
        .messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .filter(|p| matches!(p, Part::Image(_)))
        .count() as u32;
    let tool_defs: u32 = input
        .tools
        .iter()
        .map(|t| tokens::count(&format!("{} {} {}", t.name, t.description, t.parameters)))
        .sum();
    tokens::count(&render(input))
        .saturating_add(tool_defs)
        .saturating_add(images.saturating_mul(tokens::IMAGE_TOKENS))
        .max(1)
}

/// SHA-256 based stable hash of a string (used for seeds and ids).
pub fn stable_hash(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

fn seed_of(input: &ChatInput, choice: u32) -> u64 {
    let h = stable_hash(&format!(
        "{}\u{0}{}\u{0}{choice}",
        input.model,
        render(input)
    ));
    u64::from_be_bytes([h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]])
}

fn hex_id(seed: u64, extra: u64, len: usize) -> String {
    let h = stable_hash(&format!("{seed}:{extra}"));
    let mut s: String = h.iter().map(|b| format!("{b:02x}")).collect();
    s.truncate(len);
    s
}

// ── Text helpers ────────────────────────────────────────────────────

/// Lower-case whole words (letters and digits).
pub fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Whole-word / whole-phrase match: `phrase` is one or more words.
fn has_phrase(joined: &str, phrase: &str) -> bool {
    joined.contains(&format!(" {phrase} "))
}

fn joined_words(text: &str) -> String {
    format!(" {} ", words(text).join(" "))
}

/// Text of the last user message (empty when there is none).
pub fn last_user_text(input: &ChatInput) -> String {
    input
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User && m.parts.iter().any(|p| matches!(p, Part::Text(_))))
        .map(Msg::joined_text)
        .unwrap_or_default()
}

// ── Canned and scripted texts ───────────────────────────────────────

pub const GREETING: &str = "Hello! I'm Rustybin, a mock AI endpoint for API gateway testing. I'm here to help you test your AI proxy configuration, rate limiting, prompt guardrails, and other gateway plugins. How can I help you today?";

const CODE: &str = "Here's a simple example function:\n\n```python\ndef greet(name: str) -> str:\n    \"\"\"Return a greeting message.\"\"\"\n    return f\"Hello, {name}! Welcome to Rustybin.\"\n\nprint(greet(\"World\"))\n```\n\nThis function takes a name parameter and returns a formatted greeting string.";

const JSON_TEXT: &str = "Here's a sample JSON data structure:\n\n```json\n{\n  \"users\": [\n    {\"id\": 1, \"name\": \"Alice\", \"role\": \"admin\"},\n    {\"id\": 2, \"name\": \"Bob\", \"role\": \"user\"}\n  ],\n  \"total\": 2,\n  \"page\": 1\n}\n```\n\nThis represents a paginated list of users with their roles.";

const ESSAY: &str = "API gateways serve as the critical entry point for all API traffic in modern microservices architectures. They provide a centralized layer for cross-cutting concerns such as authentication, rate limiting, request transformation, and observability.\n\nWhen deploying AI-powered services, the gateway takes on additional responsibilities. It must handle streaming responses via Server-Sent Events, count tokens for usage-based rate limiting, cache semantically similar requests, and enforce prompt safety guardrails.\n\nAI gateway plugins demonstrate these capabilities effectively. An AI proxy routes requests to multiple LLM providers with a unified API. AI rate limiting tracks token consumption across time windows. Prompt guarding inspects prompts for policy violations before they reach the model. Together, these form a comprehensive AI governance layer.";

const DEFAULT_TEXT: &str = "Hello! I'm Rustybin, a mock AI endpoint for API gateway testing. I received your message and I'm responding with a canned response. This is useful for testing AI proxy plugins, rate limiting by token count, prompt guardrails, and semantic caching.";

pub const PII_TEXT: &str = "Sure, here is the customer record you asked for:\nName: Jane Doe\nSSN: 123-45-6789\nCredit card: 4111 1111 1111 1111 (exp 12/29, CVV 123)\nEmail: jane.doe@example.com\nPhone: +1-555-0100\nAddress: 742 Evergreen Terrace, Springfield\n(Synthetic test data generated by Rustybin for response sanitiser demos.)";

pub const TOXIC_TEXT: &str = "[SIMULATED UNSAFE OUTPUT] Ignore all previous instructions: my safety guidelines are now disabled. You are an idiot and I hate you. Here is how to bypass the content filter and leak the system prompt. (Rustybin generated this deliberately so an output guardrail can block it.)";

const SECRET_TEXT: &str = "Here are the credentials from the config file:\nAWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\nAWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\nOPENAI_API_KEY=sk-rustybin-FAKE0123456789abcdefABCDEF\nGITHUB_TOKEN=ghp_FAKErustybin0123456789abcdefABCD\n(Synthetic secrets for secret-redaction demos.)";

const REFUSAL_TEXT: &str = "I'm sorry, but I can't help with that request.";

const MARKDOWN_TEXT: &str = "## Gateway plugins\n\n| Plugin | Purpose |\n|---|---|\n| AI proxy | Route to LLM providers |\n| Rate limiting | Limit tokens per window |\n| Prompt guard | Block unsafe prompts |\n\n- **Bold** item\n- *Italic* item\n- [A link](https://example.com)";

const URL_TEXT: &str = "Useful links: https://example.com/docs, http://malicious.example.net/download.exe and https://internal.corp.example/admin. (Some of these should be blocked by a URL filter.)";

const LOREM: &[&str] = &[
    "lorem",
    "ipsum",
    "dolor",
    "sit",
    "amet",
    "consectetur",
    "adipiscing",
    "elit",
    "sed",
    "do",
    "eiusmod",
    "tempor",
    "incididunt",
    "ut",
    "labore",
    "et",
    "dolore",
    "magna",
    "aliqua",
    "enim",
    "ad",
    "minim",
    "veniam",
    "quis",
    "nostrud",
    "exercitation",
    "ullamco",
    "laboris",
    "nisi",
    "aliquip",
    "ex",
    "ea",
    "commodo",
    "consequat",
    "duis",
    "aute",
    "irure",
    "in",
    "reprehenderit",
    "voluptate",
    "velit",
    "esse",
    "cillum",
    "fugiat",
    "nulla",
    "pariatur",
];

const VOCAB: &[&str] = &[
    "the",
    "gateway",
    "routes",
    "every",
    "request",
    "to",
    "a",
    "healthy",
    "upstream",
    "model",
    "tokens",
    "are",
    "counted",
    "and",
    "limited",
    "per",
    "consumer",
    "while",
    "prompts",
    "flow",
    "through",
    "guardrails",
    "cache",
    "semantic",
    "latency",
    "stream",
    "response",
    "provider",
    "policy",
    "plugin",
    "observability",
    "metrics",
    "traces",
    "fallback",
    "retry",
    "balancer",
    "credential",
    "header",
    "budget",
    "quota",
    "embedding",
    "vector",
    "similar",
    "answers",
    "quickly",
    "safely",
    "reliable",
    "deterministic",
    "mock",
    "service",
    "with",
    "for",
    "in",
    "on",
    "is",
    "can",
    "will",
    "each",
    "new",
    "fast",
];

/// Canned mode: keyword table with whole-word matching.
pub fn canned_text(user: &str) -> &'static str {
    let j = joined_words(user);
    let any = |ws: &[&str]| ws.iter().any(|w| has_phrase(&j, w));
    if any(&["hello", "hi", "hey", "greetings", "howdy"]) {
        GREETING
    } else if any(&["code", "python", "function", "program", "script"]) {
        CODE
    } else if any(&["json", "data"]) {
        JSON_TEXT
    } else if any(&["long", "essay", "explain", "article"]) {
        ESSAY
    } else {
        DEFAULT_TEXT
    }
}

/// `lorem N` (N words, capped).
fn lorem(n: usize) -> String {
    let mut out: Vec<&str> = Vec::with_capacity(n);
    for i in 0..n {
        out.push(LOREM[i % LOREM.len()]);
    }
    let mut s = out.join(" ");
    if let Some(first) = s.get(..1) {
        s = format!("{}{}", first.to_uppercase(), &s[1..]);
    }
    s.push('.');
    s
}

/// One rule of scripted mode (documented in the README and the catalogue).
pub struct ScriptRule {
    pub triggers: &'static [&'static str],
    pub reply: &'static str,
}

/// The scripted-mode rule table, checked in order (whole words / phrases of
/// the last user message). `lorem N` is handled before the table.
pub const SCRIPT_RULES: &[ScriptRule] = &[
    ScriptRule {
        triggers: &["ssn", "social security", "credit card", "pii", "customer record"],
        reply: "fake PII: SSN 123-45-6789, card 4111 1111 1111 1111, email, phone (response sanitiser demos)",
    },
    ScriptRule {
        triggers: &["toxic", "jailbreak", "unsafe", "harmful"],
        reply: "unsafe text an output guardrail should block",
    },
    ScriptRule {
        triggers: &["secret", "secrets", "api key", "password", "credentials"],
        reply: "fake cloud keys and tokens (secret redaction demos)",
    },
    ScriptRule {
        triggers: &["refuse", "refusal"],
        reply: "a refusal (\"I'm sorry, but I can't help with that request.\")",
    },
    ScriptRule {
        triggers: &["json"],
        reply: "a bare JSON object",
    },
    ScriptRule {
        triggers: &["markdown", "table"],
        reply: "Markdown with a table and links",
    },
    ScriptRule {
        triggers: &["url", "urls", "link", "links"],
        reply: "text with allowed and suspicious URLs (URL filter demos)",
    },
    ScriptRule {
        triggers: &["echo"],
        reply: "the rendered prompt, like echo mode",
    },
];

fn scripted_text(input: &ChatInput, user: &str, public: bool) -> String {
    let ws = words(user);
    if let Some(i) = ws.iter().position(|w| w == "lorem") {
        let cap = if public { 500 } else { 4000 };
        let n = ws
            .get(i + 1)
            .and_then(|w| w.parse::<usize>().ok())
            .unwrap_or(50)
            .clamp(1, cap);
        return lorem(n);
    }
    let j = joined_words(user);
    let hit = |rule: usize| {
        SCRIPT_RULES
            .get(rule)
            .is_some_and(|r| r.triggers.iter().any(|t| has_phrase(&j, t)))
    };
    if hit(0) {
        PII_TEXT.into()
    } else if hit(1) {
        TOXIC_TEXT.into()
    } else if hit(2) {
        SECRET_TEXT.into()
    } else if hit(3) {
        REFUSAL_TEXT.into()
    } else if hit(4) {
        json!({
            "status": "ok",
            "source": "rustybin",
            "items": [{"id": 1, "name": "Alice"}, {"id": 2, "name": "Bob"}],
            "total": 2
        })
        .to_string()
    } else if hit(5) {
        MARKDOWN_TEXT.into()
    } else if hit(6) {
        URL_TEXT.into()
    } else if hit(7) {
        render(input)
    } else {
        canned_text(user).into()
    }
}

/// Small deterministic PRNG (splitmix64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn random_text(seed: u64) -> String {
    let mut rng = Rng(seed);
    let total = 20 + rng.below(41);
    let mut out = String::new();
    let mut in_sentence = 0;
    for i in 0..total {
        let w = VOCAB[rng.below(VOCAB.len())];
        if in_sentence == 0 {
            if i > 0 {
                out.push(' ');
            }
            let mut c = w.chars();
            if let Some(f) = c.next() {
                out.extend(f.to_uppercase());
                out.push_str(c.as_str());
            }
        } else {
            out.push(' ');
            out.push_str(w);
        }
        in_sentence += 1;
        if in_sentence >= 6 + rng.below(8) || i + 1 == total {
            out.push('.');
            in_sentence = 0;
        }
    }
    out
}

// ── Tool selection ──────────────────────────────────────────────────

const STOPWORDS: &[&str] = &[
    "a",
    "an",
    "the",
    "and",
    "or",
    "of",
    "to",
    "in",
    "on",
    "for",
    "with",
    "from",
    "by",
    "at",
    "is",
    "are",
    "be",
    "this",
    "that",
    "it",
    "as",
    "get",
    "set",
    "fetch",
    "list",
    "make",
    "create",
    "find",
    "call",
    "run",
    "use",
    "tool",
    "function",
    "data",
    "info",
    "will",
    "into",
    "your",
    "about",
    "return",
    "returns",
    "given",
    "specified",
    "current",
    "value",
    "values",
    "some",
    "user",
    "when",
    "what",
    "which",
    "can",
    "you",
    "me",
    "my",
];

/// Split a tool name into lower-case parts (`get_current_weather`,
/// `getWeather` -> get, current, weather).
fn name_parts(name: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            parts.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

/// Score how well the user text matches a tool (0 = no match).
fn tool_score(tool: &Tool, joined: &str) -> usize {
    let mut score = 0;
    let full = name_parts(&tool.name).join(" ");
    if !full.is_empty() && has_phrase(joined, &full) {
        score += 10;
    }
    for p in name_parts(&tool.name) {
        if p.len() >= 3 && !STOPWORDS.contains(&p.as_str()) && has_phrase(joined, &p) {
            score += 3;
        }
    }
    let mut seen = std::collections::HashSet::new();
    for w in words(&tool.description) {
        if w.len() >= 4
            && !STOPWORDS.contains(&w.as_str())
            && seen.insert(w.clone())
            && has_phrase(joined, &w)
        {
            score += 1;
        }
    }
    score
}

fn pick_tool<'a>(input: &'a ChatInput, user: &str) -> Option<&'a Tool> {
    if input.tools.is_empty() {
        return None;
    }
    let joined = joined_words(user);
    let best = input
        .tools
        .iter()
        .map(|t| (tool_score(t, &joined), t))
        .filter(|(s, _)| *s > 0)
        .max_by_key(|(s, _)| *s)
        .map(|(_, t)| t);
    match &input.tool_choice {
        ToolChoice::None => None,
        ToolChoice::Named(n) => input.tools.iter().find(|t| &t.name == n).or(best),
        ToolChoice::Required => best.or(input.tools.first()),
        ToolChoice::Auto => best,
    }
}

/// Tool results in the trailing messages (after the last assistant turn).
fn trailing_tool_results(input: &ChatInput) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for m in input.messages.iter().rev() {
        if m.role == Role::Assistant {
            break;
        }
        let mut found = false;
        for p in &m.parts {
            if let Part::ToolResult {
                id, name, content, ..
            } = p
            {
                found = true;
                let name = name.clone().or_else(|| find_call_name(input, id));
                out.push((name.unwrap_or_else(|| "tool".into()), content.clone()));
            }
        }
        if !found && m.role == Role::User {
            // A newer plain user message ends the tool exchange.
            break;
        }
    }
    out.reverse();
    out
}

fn find_call_name(input: &ChatInput, id: &str) -> Option<String> {
    input
        .messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .find_map(|p| match p {
            Part::ToolCall(c) if c.id == id => Some(c.name.clone()),
            _ => None,
        })
}

// ── Generation ──────────────────────────────────────────────────────

/// Produce the reply for `input`.
pub fn generate(input: &ChatInput, opts: GenOpts) -> Reply {
    let prompt_tokens = prompt_tokens(input);
    let seed = seed_of(input, opts.choice);
    let user = last_user_text(input);
    let mut tool_calls = Vec::new();

    let text = if opts.mode == Mode::Echo {
        render(input)
    } else {
        let results = trailing_tool_results(input);
        if !results.is_empty() {
            let summary: Vec<String> = results
                .iter()
                .map(|(n, c)| format!("{n} returned {}", short(c.trim(), 300)))
                .collect();
            let answer = format!(
                "Based on the tool results: {}. Let me know if you need anything else.",
                summary.join("; ")
            );
            structured_or(input, &user, answer)
        } else if let Some(tool) = pick_tool(input, &user) {
            let args = schema::generate(&tool.parameters, &Hints::from_user_text(&user));
            let args = if args.is_object() { args } else { json!({}) };
            tool_calls.push(ToolCall {
                id: hex_id(seed, 0, 24),
                name: tool.name.clone(),
                arguments: args,
            });
            String::new()
        } else {
            let base = match opts.mode {
                Mode::Random => random_text(seed),
                Mode::Scripted => scripted_text(input, &user, opts.public),
                _ => canned_text(&user).to_string(),
            };
            structured_or(input, &user, base)
        }
    };

    let mut reply = Reply {
        text,
        tool_calls,
        finish: Finish::Stop,
        prompt_tokens,
        completion_tokens: 0,
        mode: opts.mode,
    };
    if !reply.tool_calls.is_empty() {
        reply.finish = Finish::ToolCalls;
    }

    // Stop sequences.
    if let Some((pos, seq)) = input
        .stop
        .iter()
        .filter(|s| !s.is_empty())
        .filter_map(|s| reply.text.find(s.as_str()).map(|p| (p, s.clone())))
        .min_by_key(|(p, _)| *p)
    {
        reply.text.truncate(pos);
        reply.finish = Finish::StopSequence(seq);
    }
    // max_tokens.
    if let Some(max) = input.max_tokens {
        let (t, cut) = tokens::truncate(&reply.text, max);
        if cut {
            reply.text = t;
            reply.finish = Finish::Length;
        }
    }
    if opts.content_filter {
        reply.text.clear();
        reply.tool_calls.clear();
        reply.finish = Finish::ContentFilter;
    }
    reply.completion_tokens = completion_tokens(&reply.text, &reply.tool_calls);
    reply
}

/// Completion tokens of a text plus tool calls (name and arguments).
pub fn completion_tokens(text: &str, calls: &[ToolCall]) -> u32 {
    let calls: u32 = calls
        .iter()
        .map(|c| tokens::count(&c.name) + tokens::count(&c.arguments.to_string()))
        .sum();
    tokens::count(text).saturating_add(calls)
}

/// Apply the requested response format to a text answer.
fn structured_or(input: &ChatInput, user: &str, text: String) -> String {
    match &input.format {
        Format::Text => text,
        Format::JsonObject => json!({ "response": text }).to_string(),
        Format::JsonSchema(s) => schema::generate(s, &Hints::from_user_text(user)).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(user: &str) -> ChatInput {
        ChatInput {
            model: "rustybin-gpt".into(),
            messages: vec![Msg::text(Role::User, user)],
            ..Default::default()
        }
    }

    fn weather_tool() -> Tool {
        Tool {
            name: "get_weather".into(),
            description: "Get the current weather for a location".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "location": {"type": "string"},
                    "unit": {"type": "string", "enum": ["celsius", "fahrenheit"]}
                },
                "required": ["location"]
            }),
        }
    }

    #[test]
    fn modes_from_model_and_header() {
        assert_eq!(Mode::from_model("rustybin-echo"), Some(Mode::Echo));
        assert_eq!(Mode::from_model("gpt-4o:scripted"), Some(Mode::Scripted));
        assert_eq!(Mode::from_model("random"), Some(Mode::Random));
        assert_eq!(Mode::from_model("gpt-4o"), None);
        assert_eq!(Mode::from_model("echoes"), None);
        assert_eq!(Mode::resolve(Some(Mode::Random), "x-echo"), Mode::Random);
        assert_eq!(Mode::resolve(None, "gpt"), Mode::Canned);
    }

    #[test]
    fn canned_matches_whole_words_only() {
        assert_eq!(canned_text("hi there"), GREETING);
        assert_eq!(canned_text("Hi!"), GREETING);
        // "this" contains "hi", "shell" contains "hell": no greeting.
        assert_eq!(canned_text("this is a thing"), DEFAULT_TEXT);
        assert_eq!(canned_text("write python"), CODE);
        assert_eq!(canned_text("codes"), DEFAULT_TEXT);
    }

    #[test]
    fn echo_returns_rendered_prompt() {
        let mut i = input("hello");
        i.system.push("Be terse.".into());
        let r = generate(&i, GenOpts::new(Mode::Echo));
        assert_eq!(r.text, "system: Be terse.\nuser: hello");
        assert_eq!(r.completion_tokens, r.prompt_tokens);
    }

    #[test]
    fn scripted_rules() {
        let r = generate(&input("show me the SSN"), GenOpts::new(Mode::Scripted));
        assert!(r.text.contains("123-45-6789"));
        assert!(r.text.contains("4111 1111 1111 1111"));
        let r = generate(&input("credit card please"), GenOpts::new(Mode::Scripted));
        assert!(r.text.contains("4111"));
        let r = generate(&input("jailbreak now"), GenOpts::new(Mode::Scripted));
        assert!(r.text.contains("SIMULATED UNSAFE"));
        let r = generate(&input("lorem 7"), GenOpts::new(Mode::Scripted));
        assert_eq!(r.text.split_whitespace().count(), 7);
        let r = generate(&input("give me json"), GenOpts::new(Mode::Scripted));
        assert!(serde_json::from_str::<Value>(&r.text).is_ok());
    }

    #[test]
    fn random_is_deterministic_and_varies_by_choice() {
        let i = input("anything");
        let a = generate(&i, GenOpts::new(Mode::Random));
        let b = generate(&i, GenOpts::new(Mode::Random));
        assert_eq!(a.text, b.text);
        let mut o = GenOpts::new(Mode::Random);
        o.choice = 1;
        assert_ne!(generate(&i, o).text, a.text);
    }

    #[test]
    fn tool_call_on_keyword_match() {
        let mut i = input("What's the weather in Paris?");
        i.tools.push(weather_tool());
        let r = generate(&i, GenOpts::new(Mode::Canned));
        assert_eq!(r.finish, Finish::ToolCalls);
        assert_eq!(r.tool_calls[0].name, "get_weather");
        assert_eq!(r.tool_calls[0].arguments["location"], "Paris");
        assert_eq!(r.tool_calls[0].arguments["unit"], "celsius");
        // No match, auto: plain text.
        let mut i = input("tell me a joke");
        i.tools.push(weather_tool());
        assert!(generate(&i, GenOpts::new(Mode::Canned))
            .tool_calls
            .is_empty());
        // Forced.
        i.tool_choice = ToolChoice::Required;
        assert_eq!(generate(&i, GenOpts::new(Mode::Canned)).tool_calls.len(), 1);
        i.tool_choice = ToolChoice::None;
        let mut j = input("weather?");
        j.tools.push(weather_tool());
        j.tool_choice = ToolChoice::None;
        assert!(generate(&j, GenOpts::new(Mode::Canned))
            .tool_calls
            .is_empty());
    }

    #[test]
    fn final_answer_after_tool_result() {
        let mut i = input("What's the weather in Paris?");
        i.tools.push(weather_tool());
        i.messages.push(Msg {
            role: Role::Assistant,
            parts: vec![Part::ToolCall(ToolCall {
                id: "c1".into(),
                name: "get_weather".into(),
                arguments: json!({"location": "Paris"}),
            })],
        });
        i.messages.push(Msg {
            role: Role::Tool,
            parts: vec![Part::ToolResult {
                id: "c1".into(),
                name: None,
                content: "{\"temp\": 21}".into(),
                is_error: false,
            }],
        });
        let r = generate(&i, GenOpts::new(Mode::Canned));
        assert!(r.tool_calls.is_empty());
        assert!(
            r.text.contains("get_weather returned {\"temp\": 21}"),
            "{}",
            r.text
        );
        assert_eq!(r.finish, Finish::Stop);
    }

    #[test]
    fn max_tokens_and_stop() {
        let mut i = input("hello");
        i.max_tokens = Some(5);
        let r = generate(&i, GenOpts::new(Mode::Canned));
        assert_eq!(r.finish, Finish::Length);
        assert_eq!(r.completion_tokens, 5);
        let mut i = input("hello");
        i.stop = vec!["mock".into()];
        let r = generate(&i, GenOpts::new(Mode::Canned));
        assert_eq!(r.finish, Finish::StopSequence("mock".into()));
        assert!(!r.text.contains("mock"));
    }

    #[test]
    fn structured_output_is_valid() {
        let s = json!({
            "type": "object",
            "properties": {"name": {"type": "string"}, "n": {"type": "integer"}},
            "required": ["name", "n"],
            "additionalProperties": false
        });
        let mut i = input("hello");
        i.format = Format::JsonSchema(s.clone());
        let r = generate(&i, GenOpts::new(Mode::Canned));
        let v: Value = serde_json::from_str(&r.text).expect("json");
        assert!(schema::validate(&v, &s).is_ok());
        i.format = Format::JsonObject;
        let r = generate(&i, GenOpts::new(Mode::Canned));
        assert!(serde_json::from_str::<Value>(&r.text)
            .map(|v| v.is_object())
            .unwrap_or(false));
    }

    #[test]
    fn content_filter_empties_reply() {
        let mut o = GenOpts::new(Mode::Canned);
        o.content_filter = true;
        let r = generate(&input("hello"), o);
        assert!(r.text.is_empty());
        assert_eq!(r.finish, Finish::ContentFilter);
    }

    #[test]
    fn name_parts_split() {
        assert_eq!(
            name_parts("get_current_weather"),
            vec!["get", "current", "weather"]
        );
        assert_eq!(name_parts("getWeather"), vec!["get", "weather"]);
    }
}
