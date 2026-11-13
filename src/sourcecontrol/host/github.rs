//! GitHub, and GitHub Enterprise Server through `git.host_api`: one call,
//! `POST {api}/repos/{owner}/{repo}/pulls`.

use serde_json::{json, Value};

use super::{Call, Host, Opened, PullRequest};

/// The REST API version asked for.
const API_VERSION: &str = "2022-11-28";

pub struct Github;

impl Host for Github {
    fn public_host(&self) -> &'static str {
        "github.com"
    }

    fn public_api(&self) -> &'static str {
        "https://api.github.com"
    }

    fn self_hosted_api(&self, host: &str) -> String {
        format!("https://{host}/api/v3")
    }

    /// `owner/repo`: exactly two parts.
    fn repository(&self, path: &str) -> Result<String, String> {
        if path.split('/').count() != 2 {
            return Err(format!(
                "{path} is not owner/repo, which is how this host kind names a repository"
            ));
        }
        Ok(path.to_string())
    }

    fn open(&self, api: &str, token: &str, pr: &PullRequest) -> Call {
        Call {
            url: format!("{api}/repos/{}/pulls", pr.repository),
            headers: vec![
                ("Authorization", format!("Bearer {token}")),
                ("Accept", "application/vnd.github+json".to_string()),
                ("X-GitHub-Api-Version", API_VERSION.to_string()),
            ],
            body: json!({
                "title": pr.title,
                "head": pr.head,
                "base": pr.base,
                "body": pr.body,
            }),
        }
    }

    fn opened(&self, answer: &Value) -> Opened {
        Opened {
            number: answer["number"].as_u64(),
            url: answer["html_url"].as_str().map(str::to_string),
        }
    }

    /// `message`, and the first of `errors` that says something.
    fn message(&self, answer: &Value) -> Option<String> {
        let message = answer["message"].as_str()?;
        let first = answer["errors"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|e| {
                e["message"].as_str().map(str::to_string).or_else(|| {
                    match (e["field"].as_str(), e["code"].as_str()) {
                        (Some(field), Some(code)) => Some(format!("{field}: {code}")),
                        _ => None,
                    }
                })
            });
        Some(match first {
            Some(detail) => format!("{message}: {detail}"),
            None => message.to_string(),
        })
    }
}
