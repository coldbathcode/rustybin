//! The tasks of this release: one file each. Step 04 adds the mock-building
//! tasks, step 05 the MCP ones.

pub mod explain;
pub mod free_answer;
pub mod suggest;

use super::task::Task;

pub static ALL: &[Task] = &[free_answer::TASK, explain::TASK, suggest::TASK];

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
