//! `rustybin login --agent`: an agent signs in from the terminal by walking
//! the control plane's own authorization server (`crate::control_oauth`) in
//! the browser, as an MCP client with a browser does: registration, PKCE, a
//! loopback address for the answer, the console's consent view. The token
//! pair lands in the profile (`Profile.agent`) for `rustybin mcp-stdio`,
//! which renews the access token with the refresh token.
//!
//! The tokens are accepted on `/_rustybin/mcp` only. No other command sends
//! them, and one given as `--token` is refused here with [`ONLY_MCP`]
//! instead of the instance's `401`.

use std::path::PathBuf;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use reqwest::Method;
use serde_json::{json, Value};

use super::client::{Body, Client, Reply};
use super::profiles::{self, AgentLogin};
use super::{Ctx, Fail, Result};

/// The client name the consent view and the AI agents view show. The same
/// at every sign-in, with [`REDIRECT_URI`], so the client id (and the agent)
/// stays the same one: its rights and a revocation survive a new sign-in.
pub const CLIENT_NAME: &str = "Rustybin CLI";
/// Registered without a port: the server matches a loopback redirect URI
/// whatever its port (RFC 8252, 7.3).
pub const REDIRECT_URI: &str = "http://127.0.0.1/callback";
/// Renew an access token this long before it expires.
pub const RENEW_BEFORE_SECS: i64 = 60;

/// Why the other commands refuse an agent sign-in.
pub const ONLY_MCP: &str = "an agent sign-in (rustybin login --agent) works with rustybin mcp-stdio only: the instance accepts its tokens on /_rustybin/mcp and nowhere else. Sign in with rustybin login (or give --token) for the other commands";

const REGISTER: &str = "/_rustybin/oauth/register";
const AUTHORIZE: &str = "/_rustybin/oauth/authorize";
const TOKEN: &str = "/_rustybin/oauth/token";

fn random() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    B64.encode(b)
}

fn enc(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Is `token` one of the authorization server's (an access or a refresh
/// token of an agent sign-in)? Read from its claims without verifying: this
/// only chooses a message, the instance decides what it accepts.
pub fn is_agent_token(token: &str) -> bool {
    let mut parts = token.split('.');
    let (Some(_), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let Some(claims) = B64
        .decode(payload)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return false;
    };
    matches!(claims["rb_use"].as_str(), Some("access" | "refresh"))
        && claims["client_id"]
            .as_str()
            .is_some_and(|c| c.starts_with("rbc."))
}

/// What the token endpoint says in an error (printable, bounded).
fn oauth_error(reply: &Reply) -> String {
    let v = reply.value();
    let text = v["error_description"]
        .as_str()
        .or_else(|| v["error"].as_str())
        .unwrap_or("refused");
    crate::logging::printable(text, 300)
}

fn form(pairs: &[(&str, &str)]) -> Body {
    let mut s = form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        s.append_pair(k, v);
    }
    Body::Text(s.finish(), "application/x-www-form-urlencoded")
}

/// A token response as what the profile keeps.
fn kept(client_id: &str, v: &Value, before: Option<&AgentLogin>) -> Result<AgentLogin> {
    let access = v["access_token"]
        .as_str()
        .ok_or_else(|| Fail::Failed("the answer carries no access token".into()))?;
    let expires_at = v["expires_in"]
        .as_i64()
        .map(|s| (chrono::Utc::now() + chrono::Duration::seconds(s)).to_rfc3339());
    Ok(AgentLogin {
        client_id: client_id.to_string(),
        access_token: access.to_string(),
        refresh_token: v["refresh_token"]
            .as_str()
            .map(str::to_string)
            .or_else(|| before.and_then(|b| b.refresh_token.clone())),
        expires_at,
        scope: v["scope"]
            .as_str()
            .map(str::to_string)
            .or_else(|| before.and_then(|b| b.scope.clone())),
    })
}

/// The browser walk. Answers what the profile keeps.
pub async fn login(ctx: &mut Ctx, url: &str) -> Result<AgentLogin> {
    let client = Client::new(url, None)?;
    let reply = client
        .send(
            Method::POST,
            REGISTER,
            &[],
            Body::Json(json!({
                "client_name": CLIENT_NAME,
                "redirect_uris": [REDIRECT_URI],
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "none",
            })),
        )
        .await?;
    if reply.status == 404 {
        return Err(Fail::Failed(format!(
            "{url}: the agent sign-in is not offered here (an open control plane, shared mode, a self-service instance, RUSTYBIN_CONTROL_OAUTH=off, or an authorization server elsewhere): give rustybin mcp-stdio a token with --token"
        )));
    }
    let registered = reply.ok()?.value();
    let client_id = registered["client_id"]
        .as_str()
        .ok_or_else(|| Fail::Failed("the registration answered no client id".into()))?
        .to_string();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| Fail::Failed(format!("no loopback port to listen on: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| Fail::Failed(e.to_string()))?
        .port();
    // The listener answers only the callback that carries this value: a
    // page that finds the port cannot end the sign-in with a forged answer.
    let state = random();
    let verifier = random();
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let mut start = format!(
        "{}{AUTHORIZE}?response_type=code&client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&state={}",
        url.trim_end_matches('/'),
        enc(&client_id),
        enc(&redirect),
        crate::sso::challenge_of(&verifier),
        enc(&state),
    );
    if let Some(scope) = ctx.args.value("--scope") {
        start.push_str(&format!("&scope={}", enc(scope)));
    }
    ctx.say(&format!(
        "Approve the agent's sign-in in the browser (the console asks you to sign in first if you are not). If it does not open, visit:\n{start}"
    ));
    if !ctx.args.has("--no-browser") {
        match &ctx.env.browser {
            Some(b) => (b.0)(&start),
            None => super::commands::open_browser(&start),
        }
    }
    let wait = ctx
        .args
        .value("--wait")
        .and_then(|w| w.parse::<u64>().ok())
        .unwrap_or(300)
        .clamp(1, 3600);
    let query = tokio::time::timeout(
        Duration::from_secs(wait),
        super::commands::loopback_answer(listener, "state", &state),
    )
    .await
    .map_err(|_| Fail::Failed(format!("the browser did not come back in {wait} s")))?
    .map_err(|e| Fail::Failed(format!("the loopback listener failed: {e}")))?;
    let pairs: std::collections::BTreeMap<String, String> =
        form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
    if let Some(reason) = pairs.get("error") {
        // Printable only: what comes back through the browser never sends
        // the terminal an escape sequence.
        return Err(Fail::Auth(format!(
            "{url}: the agent's sign-in was refused ({}): {}",
            crate::logging::printable(reason, 80),
            crate::logging::printable(
                pairs
                    .get("error_description")
                    .map(String::as_str)
                    .unwrap_or(""),
                400
            )
        )));
    }
    let code = pairs
        .get("code")
        .ok_or_else(|| Fail::Failed("the browser came back without a code".into()))?;
    let reply = client
        .send(
            Method::POST,
            TOKEN,
            &[],
            form(&[
                ("grant_type", "authorization_code"),
                ("client_id", &client_id),
                ("code", code),
                ("code_verifier", &verifier),
                ("redirect_uri", &redirect),
            ]),
        )
        .await?;
    if reply.status == 400 || reply.status == 401 {
        return Err(Fail::Auth(format!(
            "{url}: the sign-in's code was refused: {}",
            oauth_error(&reply)
        )));
    }
    kept(&client_id, &reply.ok()?.value(), None)
}

/// A new token pair from the refresh token, as the authorization server
/// defines it. Refused once the agent is revoked, its user signed out
/// everywhere, was disabled or removed, or thirty days passed.
pub async fn refresh(url: &str, agent: &AgentLogin) -> Result<AgentLogin> {
    let ended = |why: String| {
        Fail::Auth(format!(
            "{url}: the agent sign-in ended ({why}): sign in again with rustybin login --agent"
        ))
    };
    let Some(refresh_token) = agent.refresh_token.as_deref() else {
        return Err(ended("no refresh token was kept".into()));
    };
    let client = Client::new(url, None)?;
    let reply = client
        .send(
            Method::POST,
            TOKEN,
            &[],
            form(&[
                ("grant_type", "refresh_token"),
                ("client_id", &agent.client_id),
                ("refresh_token", refresh_token),
            ]),
        )
        .await?;
    match reply.status {
        400 | 401 | 404 => Err(ended(oauth_error(&reply))),
        _ => kept(&agent.client_id, &reply.ok()?.value(), Some(agent)),
    }
}

/// Does the access token expire within `within` seconds (or has it)?
pub fn expiring(agent: &AgentLogin, within: i64) -> bool {
    agent
        .expires_at
        .as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .is_some_and(|t| {
            t.with_timezone(&chrono::Utc) - chrono::Utc::now() < chrono::Duration::seconds(within)
        })
}

/// A profile's agent sign-in in use (`rustybin mcp-stdio`): the bearer for
/// the next call, renewed when it is about to expire and written back.
pub struct AgentSession {
    /// The instance's base URL.
    pub url: String,
    /// Where the profile lives and its name (`None`: nothing is written).
    pub profile: Option<(PathBuf, String)>,
    pub login: AgentLogin,
}

impl AgentSession {
    /// The access token for a call, renewed first when it is about to
    /// expire. A failed renewal keeps the old one (the instance answers
    /// what it thinks of it) and is the `Err`.
    pub async fn bearer(&mut self) -> (String, Result<()>) {
        let renewed = if expiring(&self.login, RENEW_BEFORE_SECS) {
            self.renew().await
        } else {
            Ok(())
        };
        (self.login.access_token.clone(), renewed)
    }

    /// Renew now and keep the new pair in the profile.
    pub async fn renew(&mut self) -> Result<()> {
        let fresh = refresh(&self.url, &self.login).await?;
        self.login = fresh;
        self.save();
        Ok(())
    }

    /// Write the pair back (best effort: another process may hold the file,
    /// and the pair in memory works either way). Only over the sign-in this
    /// session started from: a new `rustybin login --agent` is left alone.
    fn save(&self) {
        let Some((path, name)) = &self.profile else {
            return;
        };
        let Ok(mut all) = profiles::load(path) else {
            return;
        };
        let Some(p) = all.profiles.get_mut(name) else {
            return;
        };
        if p.agent
            .as_ref()
            .is_some_and(|a| a.client_id == self.login.client_id)
        {
            p.agent = Some(self.login.clone());
            let _ = profiles::save(path, &all);
        }
    }
}
