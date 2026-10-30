//! `suggest`: three or four things to ask on a console view, cached per
//! view and model for an hour. Without a key the console shows
//! [`STATIC`], which the mock provider answers too.

use serde_json::{json, Value};

use super::ABOUT;
use crate::assist::models::Tier;
use crate::assist::task::{context_field, view_field, Task, VIEWS};

pub const TASK: Task = Task {
    name: "suggest",
    title: "Suggestions",
    description: "Three or four prompts worth asking on the current console view",
    writes: false,
    tier: Tier::Fast,
    system: SYSTEM,
    schema: Some(schema),
    prompt,
    mock,
    cache_key: Some(cache_key),
    post: None,
};

const SYSTEM: &str = "You propose what a person testing an API or AI gateway would want to ask \
the Rustybin assistant on the console view they are looking at. Each suggestion is a short label \
(at most 40 characters) and the full prompt it stands for. Prefer suggestions that lead to an \
action in the console: describe a mock, explain the latest request, prepare a load test, inspect \
an MCP server.";

pub fn schema() -> Value {
    json!({
        "type": "object",
        "required": ["suggestions"],
        "additionalProperties": false,
        "properties": {
            "suggestions": {
                "type": "array",
                "minItems": 3,
                "maxItems": 4,
                "items": {
                    "type": "object",
                    "required": ["label", "prompt"],
                    "additionalProperties": false,
                    "properties": {
                        "label": { "type": "string", "maxLength": 60 },
                        "prompt": { "type": "string", "maxLength": 400 }
                    }
                }
            }
        }
    })
}

/// The suggestions per view when no model is asked.
pub const STATIC: &[(&str, &[(&str, &str)])] = &[
    ("overview", &[
        ("Describe an API to mock", "Describe an API I want to mock and tell me how to create it here."),
        ("What did the gateway send?", "Explain the last request this instance captured and what the gateway added."),
        ("Write a load test", "Write a harness scenario that sends 50 requests per second to /echo for a minute."),
    ]),
    ("traffic", &[
        ("Explain the latest request", "Explain the latest captured request: what it is and which headers the gateway added."),
        ("Compare two requests", "What differs between the last two captured requests?"),
        ("Why a 429?", "A request got a 429: which rate-limit headers tell me the policy and when to retry?"),
    ]),
    ("mocks", &[
        ("Describe an API to mock", "Describe an API in plain words and turn it into an OpenAPI mock."),
        ("Generate examples for the petstore mock", "Generate realistic example payloads for the petstore mock's operations."),
        ("Secure a mock with JWT", "How do I put JWT security on a mock and which token does the gateway need?"),
        ("Write the portal summary for this mock", "Write the dev portal title, summary and tags for the petstore mock, for the testers who will call it."),
    ]),
    ("bins", &[
        ("Catch a webhook", "How do I create a bin that answers 202 and shows me the webhook body?"),
        ("Replay a request", "Replay the last request a bin received through my gateway."),
        ("Explain the latest request", "Explain the latest request this bin received."),
    ]),
    ("explorer", &[
        ("Which endpoint shows my headers?", "Which endpoint echoes the full request so I can see what the gateway forwarded?"),
        ("Make a route fail", "How do I make any route answer 503 or add two seconds of delay?"),
        ("A curl for an endpoint", "Give me a curl command for the mock LLM chat endpoint in the OpenAI format."),
    ]),
    ("manage", &[
        ("What can this instance reach?", "Explain the outbound connection policy and what each feature may reach."),
        ("Who can sign in?", "How does the console sign-in work and how do I change the password?"),
        ("Where are the settings stored?", "Where does this instance persist its settings and secrets?"),
    ]),
    ("ai", &[
        ("Simulate a rate limit", "How do I make the mock LLM answer a 429 with retry headers?"),
        ("Stream a long answer", "Send a streaming chat completion with a slow token rate to test gateway timeouts."),
        ("Tool calls", "Show me a request that makes the mock LLM return a tool call."),
    ]),
    ("mcp", &[
        ("Explain the tools of this server", "Explain the tools of the MCP server I am inspecting and what each needs."),
        ("Generate arguments", "Generate valid arguments for the selected MCP tool."),
        ("Which auth does it use?", "Which authentication does this MCP server expect and how do I test it?"),
    ]),
    ("registry", &[
        ("Explain this server entry", "Explain the selected MCP registry entry: what the server does and how a client connects."),
        ("Write a server.json for my mock", "Write a server.json entry for one of my mock APIs so I can publish it to a registry."),
        ("Test a gateway's registry sync", "How do I use the mock registry scenarios to test a gateway that syncs MCP servers from a registry?"),
    ]),
    ("a2a", &[
        ("Explain the agent card", "Explain the selected agent card and what the agent can do."),
        ("Send a multi-turn task", "Walk me through a multi-turn A2A task with this agent."),
        ("Push notifications", "How do push notifications work here and what URL may receive them?"),
    ]),
    ("chaos", &[
        ("Make /health fail", "Flip the health toggle to 503 and explain what a gateway health check sees."),
        ("Flaky endpoints", "Which endpoints fail intermittently and how do I reset their counters?"),
        ("Inject latency", "How do I add a 2 second delay to every third request?"),
    ]),
    ("harness", &[
        ("Write this scenario", "Write a load test scenario: 100 users, ramp over 30 seconds, through my gateway."),
        ("Find the rate limit", "Which test pattern finds the gateway's rate limit and reports it?"),
        ("Explain the report", "Explain the latest harness report: classes, overhead, server side."),
    ]),
    ("tokens", &[
        ("Mint a JWT for a mock", "Mint a token the mocked JWT security accepts and show the curl."),
        ("Decode this token", "Decode the token I pasted and explain its claims."),
        ("Introspection", "How do I introspect a token against the built-in identity provider?"),
    ]),
];

/// The static suggestions of a view (the overview's for unknown views).
pub fn static_for(view: &str) -> Vec<Value> {
    let entry = STATIC
        .iter()
        .find(|(v, _)| *v == view)
        .or_else(|| STATIC.first());
    entry
        .map(|(_, items)| {
            items
                .iter()
                .map(|(label, prompt)| json!({ "label": label, "prompt": prompt }))
                .collect()
        })
        .unwrap_or_default()
}

/// Every view's static suggestions.
pub fn static_table() -> Value {
    let mut out = serde_json::Map::new();
    for (view, _) in STATIC {
        out.insert(view.to_string(), Value::Array(static_for(view)));
    }
    Value::Object(out)
}

fn prompt(inputs: &Value) -> Result<String, String> {
    let view = view_field(inputs)
        .ok_or_else(|| format!("inputs.view must be one of {}", VIEWS.join(", ")))?;
    let context = context_field(inputs, "context")?;
    let mut out = format!("{ABOUT}\n\nThe user is on the console view: {view}.");
    if let Some(ctx) = context {
        out.push_str("\nWhat the view shows (JSON):\n");
        out.push_str(&ctx);
    }
    out.push_str("\n\nPropose three or four suggestions for this view.");
    Ok(out)
}

fn mock(inputs: &Value) -> Value {
    json!({ "suggestions": static_for(view_field(inputs).unwrap_or("overview")) })
}

fn cache_key(inputs: &Value) -> Option<String> {
    view_field(inputs).map(str::to_string)
}
