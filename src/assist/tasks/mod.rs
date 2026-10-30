//! The tasks: one file each. Step 03 brought `free_answer`, `explain` and
//! `suggest`; step 04 the mock-building tasks (`spec_from_description`,
//! `examples_for_operations`, `edit_spec`, `request_examples`); step 05
//! adds the MCP ones, step 06 `server_json`, step 07
//! `summarise_for_portal`.

pub mod edit_spec;
pub mod examples_for_operations;
pub mod explain;
pub mod free_answer;
pub mod request_examples;
pub mod sampling;
pub mod server_json;
pub mod spec_from_description;
pub mod suggest;
pub mod summarise_for_portal;
pub mod tool_arguments;

use serde_json::Value;

use super::task::Task;
use crate::mocks::compile::{CompileLimits, Compiled};
use crate::mocks::model::{MockSettings, SettingsLimits, MAX_RULES};
use crate::mocks::spec::ParseLimits;

pub static ALL: &[Task] = &[
    free_answer::TASK,
    explain::TASK,
    suggest::TASK,
    spec_from_description::TASK,
    examples_for_operations::TASK,
    edit_spec::TASK,
    request_examples::TASK,
    tool_arguments::TASK,
    sampling::TASK,
    server_json::TASK,
    summarise_for_portal::TASK,
];

/// The limits a task compiles a document under (an instance mock's).
pub fn compile_limits() -> CompileLimits {
    CompileLimits {
        parse: ParseLimits {
            max_bytes: 1024 * 1024,
            max_operations: 500,
        },
        settings: SettingsLimits {
            max_delay_ms: 60_000,
            max_body_bytes: 256 * 1024,
            max_rules: MAX_RULES,
            visitor: false,
        },
    }
}

/// Parse and compile a document (a JSON object or its text) with the default
/// settings: the parsed spec and the deploy report.
pub fn compile_doc(doc: &Value) -> Result<Compiled, String> {
    let text = match doc {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).map_err(|e| e.to_string())?,
    };
    Compiled::build(
        0,
        std::sync::Arc::from(text),
        MockSettings::default(),
        &compile_limits(),
    )
}

/// A document a task produced must parse and pass the deploy checks; the
/// errors are the repair message.
pub fn check_spec(doc: &Value) -> Result<(), String> {
    let compiled = compile_doc(doc).map_err(|e| format!("the document does not parse: {e}"))?;
    let report = &compiled.report;
    if report.ok {
        return Ok(());
    }
    let mut errors: Vec<String> = report
        .findings
        .iter()
        .filter(|f| f.level == crate::mocks::checks::Level::Error)
        .map(|f| f.message.clone())
        .collect();
    for op in &report.operations {
        for f in op
            .findings
            .iter()
            .filter(|f| f.level == crate::mocks::checks::Level::Error)
        {
            errors.push(format!("{} {}: {}", op.method, op.path, f.message));
        }
    }
    errors.truncate(12);
    Err(format!(
        "the deploy checks found {} error(s): {}",
        report.errors,
        errors.join("; ")
    ))
}

/// What Rustybin is, for every system prompt.
pub const ABOUT: &str = "\
You are the assistant built into Rustybin, an HTTP stub service for API and AI gateway demos. \
One instance offers: an echo and request inspector (Live traffic shows what a gateway forwarded), \
OpenAPI mocks (upload a spec, get a mock API with rules, stateful mode and mocked security, plus \
an MCP server per mock), request bins, a mock LLM speaking the OpenAI, Anthropic, Gemini, \
Bedrock, Ollama and Cohere wire formats, guardrail mocks, an MCP server and A2A agents, an OIDC \
identity provider, fault injection headers (X-Rustybin-Delay, X-Rustybin-Fail), a load test \
harness, and a control plane under /_rustybin with a web console at /ui. Answer briefly and \
concretely, in plain English, with curl examples where they help. Never ask the user for \
credentials and never include secrets in answers.";
