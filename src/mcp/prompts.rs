//! Prompts: `summarize`, `code_review`, `incident_report`.

use serde_json::{json, Map, Value};

use super::core::CallCtx;
use super::data;
use super::protocol::RpcError;
use super::Profile;

struct Arg {
    name: &'static str,
    description: &'static str,
    required: bool,
    completions: &'static [&'static str],
}

struct PromptDef {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    servers: &'static [&'static str],
    args: &'static [Arg],
}

const STYLES: &[&str] = &["bullets", "paragraph", "tldr"];
const LANGUAGES: &[&str] = &[
    "c",
    "cpp",
    "csharp",
    "go",
    "java",
    "javascript",
    "kotlin",
    "python",
    "ruby",
    "rust",
    "swift",
    "typescript",
];
const FOCUS: &[&str] = &["correctness", "performance", "readability", "security"];
const SEVERITIES: &[&str] = &["sev1", "sev2", "sev3", "sev4"];

static PROMPTS: &[PromptDef] = &[
    PromptDef {
        name: "summarize",
        title: "Summarize text",
        description: "Summarize a piece of text in the requested style.",
        servers: &["devtools"],
        args: &[
            Arg {
                name: "text",
                description: "The text to summarize",
                required: true,
                completions: &[],
            },
            Arg {
                name: "style",
                description: "bullets, paragraph or tldr",
                required: false,
                completions: STYLES,
            },
        ],
    },
    PromptDef {
        name: "code_review",
        title: "Code review",
        description: "Review a code snippet with an optional focus area.",
        servers: &["devtools"],
        args: &[
            Arg {
                name: "code",
                description: "The code to review",
                required: true,
                completions: &[],
            },
            Arg {
                name: "language",
                description: "Programming language",
                required: false,
                completions: LANGUAGES,
            },
            Arg {
                name: "focus",
                description: "correctness, performance, readability or security",
                required: false,
                completions: FOCUS,
            },
        ],
    },
    PromptDef {
        name: "incident_report",
        title: "Incident report",
        description: "Draft an incident report for a service; embeds the runbook resource.",
        servers: &["crm", "devtools"],
        args: &[
            Arg {
                name: "service",
                description: "Affected service",
                required: true,
                completions: data::SERVICES,
            },
            Arg {
                name: "severity",
                description: "sev1 (critical) to sev4 (minor)",
                required: false,
                completions: SEVERITIES,
            },
            Arg {
                name: "summary",
                description: "What happened, in one or two sentences",
                required: false,
                completions: &[],
            },
        ],
    },
];

fn visible(profile: &Profile, p: &PromptDef) -> bool {
    profile.server.is_none_or(|s| p.servers.contains(&s))
}

fn prompt_json(p: &PromptDef) -> Value {
    let args: Vec<Value> = p
        .args
        .iter()
        .map(|a| json!({ "name": a.name, "description": a.description, "required": a.required }))
        .collect();
    json!({ "name": p.name, "title": p.title, "description": p.description, "arguments": args })
}

pub fn list(profile: &Profile) -> Vec<Value> {
    PROMPTS
        .iter()
        .filter(|p| visible(profile, p))
        .map(prompt_json)
        .collect()
}

pub fn completions(profile: &Profile, prompt: &str, arg: &str) -> Option<Vec<&'static str>> {
    let p = PROMPTS
        .iter()
        .find(|p| p.name == prompt && visible(profile, p))?;
    Some(
        p.args
            .iter()
            .find(|a| a.name == arg)
            .map(|a| a.completions.to_vec())
            .unwrap_or_default(),
    )
}

fn user_text(text: String) -> Value {
    json!({ "role": "user", "content": { "type": "text", "text": text } })
}

/// `prompts/get`.
pub fn get(ctx: &CallCtx, params: &Value) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("Missing required parameter: name"))?;
    let p = PROMPTS
        .iter()
        .find(|p| p.name == name && visible(&ctx.profile, p))
        .ok_or_else(|| RpcError::invalid_params(format!("Unknown prompt: {name}")))?;
    let empty = Map::new();
    let given = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(m)) => m,
        Some(_) => return Err(RpcError::invalid_params("arguments must be an object")),
    };
    let mut args = std::collections::HashMap::new();
    for (k, v) in given {
        let s = v
            .as_str()
            .ok_or_else(|| RpcError::invalid_params(format!("Argument {k} must be a string")))?;
        args.insert(k.as_str(), s.chars().take(20_000).collect::<String>());
    }
    for a in p.args {
        if a.required && args.get(a.name).is_none_or(|v| v.is_empty()) {
            return Err(RpcError::invalid_params(format!(
                "Missing required argument: {}",
                a.name
            )));
        }
    }
    let get = |k: &str| args.get(k).cloned().unwrap_or_default();
    let messages = match name {
        "summarize" => {
            let style = args
                .get("style")
                .cloned()
                .unwrap_or_else(|| "bullets".into());
            vec![user_text(format!(
                "Summarize the following text as {style}. Keep it short and factual.\n\n{}",
                get("text")
            ))]
        }
        "code_review" => {
            let lang = args.get("language").cloned().unwrap_or_default();
            let focus = args
                .get("focus")
                .cloned()
                .unwrap_or_else(|| "correctness".into());
            vec![
                user_text(format!(
                    "Please review this {lang} code with a focus on {focus}. List concrete issues \
                     with line references and suggested fixes.\n\n```{lang}\n{}\n```",
                    get("code")
                )),
                json!({ "role": "assistant", "content": { "type": "text",
                    "text": "Understood. I will review the code and report issues by severity." } }),
            ]
        }
        _ => {
            let severity = args
                .get("severity")
                .cloned()
                .unwrap_or_else(|| "sev3".into());
            let summary = args
                .get("summary")
                .cloned()
                .unwrap_or_else(|| "(no summary given)".into());
            vec![
                user_text(format!(
                    "Draft an incident report for service {} ({severity}). Summary: {summary}\n\
                     Include timeline, impact, root cause, remediation and follow-ups. \
                     Follow the runbook below.",
                    get("service")
                )),
                json!({ "role": "user", "content": { "type": "resource", "resource": {
                    "uri": "rustybin://docs/runbook",
                    "mimeType": "text/plain",
                    "text": data::RUNBOOK,
                } } }),
            ]
        }
    };
    Ok(json!({ "description": p.description, "messages": messages }))
}
