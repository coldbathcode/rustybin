//! Gitea and Forgejo (a fork of it that serves the same API), always a
//! self-hosted host: one call, `POST {api}/repos/{owner}/{repo}/pulls`,
//! and one read when that is refused,
//! `GET {api}/repos/{owner}/{repo}/pulls?state=open`.
//!
//! The kind has no public host for `auto`: a team's repository is on a
//! host of its own, so the kind is always named in `git.host`.

use serde_json::{json, Value};

use super::{with_query, Call, Host, Opened, PullRequest};

/// How many open pull requests the read asks for: this API's list has no
/// filter by head, so the one looked for is found among the most recently
/// updated ones (the refused call was about it a moment ago).
const LISTED: &str = "50";

pub struct Gitea;

/// The token in this API's own scheme (`token`, not `Bearer`).
fn headers(token: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Authorization", format!("token {token}")),
        ("Accept", "application/json".to_string()),
    ]
}

impl Host for Gitea {
    fn public(&self) -> Option<(&'static str, &'static str)> {
        None
    }

    fn self_hosted_api(&self, host: &str) -> String {
        format!("https://{host}/api/v1")
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
            headers: headers(token),
            body: Some(json!({
                "title": pr.title,
                "head": pr.head,
                "base": pr.base,
                "body": pr.body,
            })),
        }
    }

    fn opened(&self, answer: &Value) -> Opened {
        Opened {
            number: answer["number"].as_u64(),
            url: answer["html_url"].as_str().map(str::to_string),
        }
    }

    /// `message`.
    fn message(&self, answer: &Value) -> Option<String> {
        answer["message"].as_str().map(str::to_string)
    }

    fn existing(&self, api: &str, token: &str, pr: &PullRequest) -> Call {
        Call {
            url: with_query(
                &format!("{api}/repos/{}/pulls", pr.repository),
                &[
                    ("state", "open"),
                    ("sort", "recentupdate"),
                    ("limit", LISTED),
                ],
            ),
            headers: headers(token),
            body: None,
        }
    }

    fn found(&self, answer: &Value, pr: &PullRequest) -> Option<Opened> {
        answer
            .as_array()?
            .iter()
            .find(|p| {
                p["head"]["ref"].as_str() == Some(pr.head)
                    && p["base"]["ref"].as_str() == Some(pr.base)
                    && p["state"].as_str().is_none_or(|s| s == "open")
            })
            .map(|p| self.opened(p))
    }
}
