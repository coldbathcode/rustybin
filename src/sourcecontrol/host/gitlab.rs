//! GitLab, and a self-managed GitLab through `git.host_api`: one call,
//! `POST {api}/projects/{path}/merge_requests` (a merge request is this
//! host's pull request).

use serde_json::{json, Value};

use super::{Call, Host, Opened, PullRequest};

pub struct Gitlab;

impl Host for Gitlab {
    fn public_host(&self) -> &'static str {
        "gitlab.com"
    }

    fn public_api(&self) -> &'static str {
        "https://gitlab.com/api/v4"
    }

    fn self_hosted_api(&self, host: &str) -> String {
        format!("https://{host}/api/v4")
    }

    /// The project's full path (groups and subgroups included) as one
    /// path segment. The parts hold letters, digits, `.`, `_` and `-` only
    /// (`remote_path`), so the slashes are all there is to encode.
    fn repository(&self, path: &str) -> Result<String, String> {
        Ok(path.replace('/', "%2F"))
    }

    fn open(&self, api: &str, token: &str, pr: &PullRequest) -> Call {
        Call {
            url: format!("{api}/projects/{}/merge_requests", pr.repository),
            headers: vec![
                ("Authorization", format!("Bearer {token}")),
                ("Accept", "application/json".to_string()),
            ],
            body: json!({
                "source_branch": pr.head,
                "target_branch": pr.base,
                "title": pr.title,
                "description": pr.body,
            }),
        }
    }

    fn opened(&self, answer: &Value) -> Opened {
        Opened {
            number: answer["iid"].as_u64(),
            url: answer["web_url"].as_str().map(str::to_string),
        }
    }

    /// `message` (a string, a list, or a map of field to reasons), else
    /// `error_description` or `error`.
    fn message(&self, answer: &Value) -> Option<String> {
        let list = |v: &Value| -> String {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("; ")
        };
        match &answer["message"] {
            Value::String(s) => Some(s.clone()),
            v @ Value::Array(_) => Some(list(v)),
            Value::Object(fields) => Some(
                fields
                    .iter()
                    .map(|(field, reasons)| match reasons {
                        Value::String(s) => format!("{field}: {s}"),
                        other => format!("{field}: {}", list(other)),
                    })
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            _ => answer["error_description"]
                .as_str()
                .or(answer["error"].as_str())
                .map(str::to_string),
        }
    }
}
