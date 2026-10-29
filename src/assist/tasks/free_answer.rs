//! `free_answer`: the assist bar's chat. A question, the view it was asked
//! from and a context document the console attaches (the captured request,
//! the mock, the status); the answer is markdown that may end with action
//! blocks.

use serde_json::{json, Value};

use super::ABOUT;
use crate::assist::models::Tier;
use crate::assist::task::{context_field, text_field, view_field, Task, MAX_TEXT_CHARS};

pub const TASK: Task = Task {
    name: "free_answer",
    title: "Ask Rustybin",
    description: "A question from the assist bar, with the current view's context; markdown with action blocks",
    writes: false,
    tier: Tier::Capable,
    system: SYSTEM,
    schema: None,
    prompt,
    mock,
    cache_key: None,
    post: None,
};

const SYSTEM: &str = concat!(
    "You are the assistant built into Rustybin, an HTTP stub service for API and AI gateway \
     demos. The user is in the web console; the message names the view and attaches what it \
     shows. Answer the question for that context, briefly, in markdown. ",
    "When a concrete next step exists, end the answer with one or more action blocks, each a \
     fenced code block with the language `action` holding one JSON object: \
     {\"action\": \"open_view\", \"label\": \"Open Live traffic\", \"payload\": {\"view\": \"traffic\"}} \
     opens a console view (overview, traffic, mocks, bins, explorer, manage, ai, mcp, a2a, chaos, \
     harness, tokens); \
     {\"action\": \"copy\", \"label\": \"Copy the curl command\", \"payload\": {\"text\": \"curl ...\"}} \
     offers text to copy; \
     {\"action\": \"run_request\", \"label\": \"Send GET /echo\", \"payload\": {\"method\": \"GET\", \
     \"path\": \"/echo\", \"headers\": {}, \"body\": null}} sends a request to this instance's own \
     data plane (paths must start with /). Never propose anything else, never invent credentials, \
     and never put secrets in an action."
);

fn prompt(inputs: &Value) -> Result<String, String> {
    let question = text_field(inputs, "question", MAX_TEXT_CHARS, true)?.unwrap_or_default();
    let view = view_field(inputs).unwrap_or("overview");
    let context = context_field(inputs, "context")?;
    let mut out = format!("{ABOUT}\n\nThe user is on the console view: {view}.\n");
    if let Some(ctx) = context {
        out.push_str("\nWhat the view shows (JSON):\n");
        out.push_str(&ctx);
        out.push('\n');
    }
    out.push_str("\nQuestion: ");
    out.push_str(&question);
    Ok(out)
}

/// The deterministic answer: names the view and the question, offers the
/// view the question most likely concerns.
fn mock(inputs: &Value) -> Value {
    let question = inputs
        .get("question")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let view = view_field(inputs).unwrap_or("overview");
    let lower = question.to_ascii_lowercase();
    let (target, label) = if lower.contains("mock") || lower.contains("openapi") {
        ("mocks", "Open Mock APIs")
    } else if lower.contains("traffic") || lower.contains("header") || lower.contains("gateway") {
        ("traffic", "Open Live traffic")
    } else if lower.contains("mcp") {
        ("mcp", "Open the MCP inspector")
    } else if lower.contains("load") || lower.contains("harness") {
        ("harness", "Open the Harness")
    } else if lower.contains("token") || lower.contains("jwt") || lower.contains("oidc") {
        ("tokens", "Open the Token lab")
    } else {
        ("explorer", "Open the API explorer")
    };
    // A description of an API to mock ("make me a petstore clone with OAuth")
    // gets the wizard's Describe tab instead of the generic actions.
    let describes_api = (lower.contains("mock") || lower.contains(" api"))
        && [
            "make", "create", "build", "describe", "want", "need", "clone", "generate", "write",
        ]
        .iter()
        .any(|w| lower.contains(w));
    let actions = if describes_api {
        format!(
            "```action\n{}\n```\n",
            json!({ "action": "create_mock", "label": "Create this mock (opens the wizard's Describe tab)", "payload": { "description": question, "openapi_version": "3.1.0" } })
        )
    } else {
        format!(
            "```action\n{}\n```\n```action\n{}\n```\n",
            json!({ "action": "open_view", "label": label, "payload": { "view": target } }),
            json!({ "action": "run_request", "label": "Send GET /anything/hello?demo=1", "payload": { "method": "GET", "path": "/anything/hello?demo=1", "headers": { "X-Demo": "rustybin" } } }),
        )
    };
    let text = format!(
        "**Mock assistant.** No model was called and nothing left this instance: the built-in \
         provider answers deterministically, which is what demos and tests use.\n\n\
         You asked from the **{view}** view: *{question}*\n\n\
         - Every endpoint echoes what it received; `GET /anything/hello?demo=1` shows the \
         gateway's headers.\n\
         - Mock APIs serve an OpenAPI spec as a working API at `/mock/<name>/...`.\n\
         - Faults work on any route: send `X-Rustybin-Fail: 503` or `X-Rustybin-Delay: 2000`.\n\n\
         Add a real key under Management, AI for answers about *your* question.\n\n{actions}"
    );
    Value::String(text)
}
