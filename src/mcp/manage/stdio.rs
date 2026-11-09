//! `rustybin mcp-stdio`: a stdio-to-Streamable-HTTP bridge for MCP clients
//! that only speak stdio. Each JSON-RPC message read from stdin is posted
//! to the server; JSON answers and the messages of SSE answers are written
//! to stdout, one per line. The session id of the handshake era is kept
//! and sent back; a 2026-07-28 client needs no session.
//!
//! ```text
//! rustybin mcp-stdio --server https://host/_rustybin/mcp --token <token>
//! rustybin mcp-stdio --profile mocks.example.com      # a profile of `rustybin login`
//! rustybin mcp-stdio --url https://host               # the profile's token for that URL
//! ```
//!
//! A profile with an agent sign-in (`rustybin login --agent`) is used
//! before the profile's own token: the bridge renews its access token with
//! the refresh token shortly before it expires, and once after a `401`,
//! and writes the new pair back to the profile.

use std::process::ExitCode;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const USAGE: &str = "\
rustybin mcp-stdio: bridge a stdio MCP client to a Rustybin management server

USAGE:
    rustybin mcp-stdio --server <url> [--token <token>] [--header 'Name: value']...
    rustybin mcp-stdio [--url <base-url>] [--profile <name>] [--header 'Name: value']...

    --server   The MCP endpoint, e.g. https://host/_rustybin/mcp (or $RUSTYBIN_MCP_URL;
               a bare base URL gets /_rustybin/mcp appended)
    --token    Sent as Authorization: Bearer <token> (or $RUSTYBIN_TOKEN / $RUSTYBIN_ADMIN_TOKEN)
    --url      The instance's base URL, as `rustybin login --url` keeps it: the token of
               the profile for that URL is used when no --token is given
    --profile  A profile of `rustybin login` (or $RUSTYBIN_PROFILE, else the default
               profile): its URL and its agent sign-in (`rustybin login --agent`,
               renewed as it expires), else its token
    --header   An extra request header, repeatable (e.g. 'X-Rustybin-Mock-Token: rbm_...')

Messages are read from stdin (one JSON-RPC message per line) and answers written
to stdout, one per line.";

/// The bridge's `User-Agent` prefix: the agents registry names its
/// transport `stdio` by it.
pub const USER_AGENT_PREFIX: &str = "rustybin-mcp-stdio/";

struct Args {
    server: String,
    token: Option<String>,
    headers: Vec<(String, String)>,
    /// The profile's agent sign-in, when one is used: `token` is its access
    /// token as kept, and the bridge renews it.
    agent: Option<crate::cli::agent_login::AgentSession>,
}

/// What a profile gives the bridge.
struct FromProfile {
    url: String,
    token: Option<String>,
    agent: Option<crate::cli::agent_login::AgentSession>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    parse_with(args, &crate::cli::Env::from_process())
}

/// The profile's URL and credential for `--url` and `--profile` (the CLI's
/// `profiles.json`): a named profile, else the profile kept for the URL,
/// else the default profile. Its agent sign-in when it has one, else its
/// token.
fn from_profiles(
    env: &crate::cli::Env,
    url: Option<&str>,
    profile: Option<&str>,
) -> Result<FromProfile, String> {
    use crate::cli::profiles;
    let fail = |e: crate::cli::Fail| e.message().to_string();
    let file = profiles::path(env);
    let all = match &file {
        Some(p) => profiles::load(p).map_err(fail)?,
        None if profile.is_some() => {
            return Err(
                "no home directory: profiles are unavailable, use --server and --token".into(),
            )
        }
        None => profiles::Profiles::default(),
    };
    let name = profile.map(str::to_string).or_else(|| {
        std::env::var("RUSTYBIN_PROFILE")
            .ok()
            .filter(|p| !p.is_empty())
    });
    let wanted_url = url.map(profiles::normalise);
    let found =
        match (&name, &wanted_url) {
            (Some(n), _) => Some(all.profiles.get_key_value(n).ok_or_else(|| {
                format!("no profile {n:?} (rustybin login --url ... --profile {n})")
            })?),
            (None, Some(u)) => all.profiles.iter().find(|(_, p)| &p.url == u).or_else(|| {
                all.default
                    .as_ref()
                    .and_then(|d| all.profiles.get_key_value(d))
                    .filter(|(_, p)| &p.url == u)
            }),
            (None, None) => all
                .default
                .as_ref()
                .and_then(|d| all.profiles.get_key_value(d)),
        };
    let url = wanted_url
        .or_else(|| found.map(|(_, p)| p.url.clone()))
        .ok_or_else(|| {
            "no URL: give --server or --url, or sign in with rustybin login".to_string()
        })?;
    let found = found.filter(|(_, p)| p.url == url);
    let agent = found.and_then(|(n, p)| {
        p.agent
            .clone()
            .map(|login| crate::cli::agent_login::AgentSession {
                url: url.clone(),
                profile: file.clone().map(|f| (f, n.clone())),
                login,
            })
    });
    let token = match &agent {
        Some(a) => Some(a.login.access_token.clone()),
        None => found.and_then(|(_, p)| p.token.clone()),
    };
    Ok(FromProfile { url, token, agent })
}

fn parse_with(args: &[String], env: &crate::cli::Env) -> Result<Args, String> {
    let mut server: Option<String> = None;
    let mut token: Option<String> = None;
    let mut url: Option<String> = None;
    let mut profile: Option<String> = None;
    let mut headers = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let value = |i: &mut usize| -> Result<String, String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a {
            "--server" | "-s" => server = Some(value(&mut i)?),
            "--token" | "-t" => token = Some(value(&mut i)?),
            "--url" => url = Some(value(&mut i)?),
            "--profile" => profile = Some(value(&mut i)?),
            "--header" | "-H" => {
                let raw = value(&mut i)?;
                let (n, v) = raw
                    .split_once(':')
                    .ok_or_else(|| format!("--header needs 'Name: value', got {raw:?}"))?;
                headers.push((n.trim().to_string(), v.trim().to_string()));
            }
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        }
        i += 1;
    }
    let server = server
        .or_else(|| std::env::var("RUSTYBIN_MCP_URL").ok())
        .filter(|s| !s.trim().is_empty());
    let mut profile_token = None;
    let mut agent = None;
    let server = match server {
        Some(s) => s,
        None if url.is_some() || profile.is_some() || env.url.is_none() => {
            let found = from_profiles(env, url.as_deref(), profile.as_deref())
                .map_err(|e| format!("{e}\n\n{USAGE}"))?;
            profile_token = found.token;
            agent = found.agent;
            found.url
        }
        None => env.url.clone().unwrap_or_default(),
    };
    let server = server.trim().trim_end_matches('/').to_string();
    let server = if server.ends_with(super::MANAGE_PATH) || server.ends_with("/mcp") {
        server
    } else {
        format!("{server}{}", super::MANAGE_PATH)
    };
    let given = token
        .or_else(|| std::env::var("RUSTYBIN_TOKEN").ok())
        .or_else(|| std::env::var("RUSTYBIN_ADMIN_TOKEN").ok())
        .filter(|t| !t.trim().is_empty());
    // A token given on the command line or in the environment wins over the
    // profile's, agent sign-in included (nothing is renewed then).
    if given.is_some() {
        agent = None;
    }
    Ok(Args {
        server,
        token: given.or(profile_token),
        headers,
        agent,
    })
}

/// One message to the server: the bearer, the extra headers, the session
/// and the 2026-07-28 routing headers derived from the body.
fn request(
    client: &reqwest::Client,
    args: &Args,
    bearer: Option<&str>,
    session: Option<&str>,
    msg: &Value,
    line: &str,
) -> reqwest::RequestBuilder {
    let mut req = client
        .post(&args.server)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    if let Some(t) = bearer {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    for (n, v) in &args.headers {
        req = req.header(n.as_str(), v.as_str());
    }
    if let Some(s) = session {
        req = req.header("mcp-session-id", s);
    }
    // Routing headers of 2026-07-28 messages, derived from the body.
    if let Some(version) = msg
        .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
        .and_then(Value::as_str)
    {
        req = req.header("mcp-protocol-version", version);
        if let Some(m) = msg.get("method").and_then(Value::as_str) {
            req = req.header("mcp-method", m);
            let name = match m {
                "tools/call" | "prompts/get" => msg.pointer("/params/name"),
                "resources/read" => msg.pointer("/params/uri"),
                _ => None,
            };
            if let Some(n) = name.and_then(Value::as_str) {
                req = req.header("mcp-name", n);
            }
        }
    }
    req.body(line.to_string())
}

/// Entry point of `rustybin mcp-stdio`.
pub async fn run(args: &[String]) -> ExitCode {
    let mut args = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .user_agent(concat!("rustybin-mcp-stdio/", env!("CARGO_PKG_VERSION")));
    if let Ok(tls) = crate::a2a::push::tls_config() {
        builder = builder.use_preconfigured_tls(tls);
    }
    let client = match builder.build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rustybin mcp-stdio: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut stdout = tokio::io::stdout();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut session: Option<String> = None;
    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => {
                eprintln!("rustybin mcp-stdio: stdin: {e}");
                return ExitCode::FAILURE;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("rustybin mcp-stdio: not JSON: {e}");
                continue;
            }
        };
        // An agent sign-in: its access token, renewed first when it is
        // about to expire.
        let mut bearer = args.token.clone();
        if let Some(agent) = args.agent.as_mut() {
            let (token, renewed) = agent.bearer().await;
            if let Err(e) = renewed {
                eprintln!("rustybin mcp-stdio: {}", e.message());
            }
            bearer = Some(token);
        }
        let sent = request(
            &client,
            &args,
            bearer.as_deref(),
            session.as_deref(),
            &msg,
            &line,
        )
        .send()
        .await;
        let mut resp = match sent {
            Ok(r) => r,
            Err(e) => {
                eprintln!("rustybin mcp-stdio: request failed: {e}");
                write_error(&mut stdout, &msg, &format!("request failed: {e}")).await;
                continue;
            }
        };
        // The instance refused the agent's access token (it expired on the
        // way, or the instance's clock is ahead): renew once and send again.
        if resp.status().as_u16() == 401 {
            if let Some(agent) = args.agent.as_mut() {
                match agent.renew().await {
                    Ok(()) => {
                        let token = agent.login.access_token.clone();
                        let again = request(
                            &client,
                            &args,
                            Some(&token),
                            session.as_deref(),
                            &msg,
                            &line,
                        )
                        .send()
                        .await;
                        match again {
                            Ok(r) => resp = r,
                            Err(e) => {
                                eprintln!("rustybin mcp-stdio: request failed: {e}");
                                write_error(&mut stdout, &msg, &format!("request failed: {e}"))
                                    .await;
                                continue;
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("rustybin mcp-stdio: {}", e.message());
                        write_error(&mut stdout, &msg, e.message()).await;
                        continue;
                    }
                }
            }
        }
        if let Some(s) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            session = Some(s.to_string());
        }
        let status = resp.status();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if status.as_u16() == 202 {
            continue;
        }
        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                write_error(
                    &mut stdout,
                    &msg,
                    &format!("reading the answer failed: {e}"),
                )
                .await;
                continue;
            }
        };
        if content_type.starts_with("text/event-stream") {
            for data in sse_data(&text) {
                let _ = stdout.write_all(data.as_bytes()).await;
                let _ = stdout.write_all(b"\n").await;
            }
        } else if text.trim().is_empty() {
            write_error(&mut stdout, &msg, &format!("HTTP {status} without a body")).await;
        } else {
            let _ = stdout.write_all(text.trim().as_bytes()).await;
            let _ = stdout.write_all(b"\n").await;
        }
        let _ = stdout.flush().await;
    }
    ExitCode::SUCCESS
}

/// The `data:` payloads of an SSE document.
fn sse_data(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if let Some(d) = line.strip_prefix("data:") {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(d.trim_start());
        } else if line.is_empty() && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// A JSON-RPC error for the message's id, so the client is not left waiting.
async fn write_error(stdout: &mut tokio::io::Stdout, msg: &Value, text: &str) {
    let Some(id) = msg.get("id") else {
        return;
    };
    let err = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32000, "message": text },
    });
    let _ = stdout.write_all(err.to_string().as_bytes()).await;
    let _ = stdout.write_all(b"\n").await;
    let _ = stdout.flush().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arguments_and_defaults() {
        let a = parse(&[
            "--server".into(),
            "https://host.example".into(),
            "--token".into(),
            "t".into(),
            "--header".into(),
            "X-Rustybin-Mock-Token: rbm_x".into(),
        ])
        .expect("args");
        assert_eq!(a.server, "https://host.example/_rustybin/mcp");
        assert_eq!(a.token.as_deref(), Some("t"));
        assert_eq!(
            a.headers,
            vec![("X-Rustybin-Mock-Token".into(), "rbm_x".into())]
        );
        assert!(parse(&["--nope".into()]).is_err());
    }

    #[test]
    fn resolves_url_and_profile_through_the_profiles_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(crate::cli::profiles::FILE);
        let mut profiles = crate::cli::profiles::Profiles::default();
        profiles.profiles.insert(
            "mocks.example.com".into(),
            crate::cli::profiles::Profile {
                url: "https://mocks.example.com".into(),
                token: Some("tok-mocks".into()),
                ..Default::default()
            },
        );
        profiles.profiles.insert(
            "other".into(),
            crate::cli::profiles::Profile {
                url: "https://other.example".into(),
                token: Some("tok-other".into()),
                ..Default::default()
            },
        );
        profiles.default = Some("other".into());
        crate::cli::profiles::save(&path, &profiles).expect("save");
        let env = crate::cli::Env {
            config_home: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        // A named profile: its URL and token.
        let a = parse_with(&["--profile".into(), "mocks.example.com".into()], &env).expect("args");
        assert_eq!(a.server, "https://mocks.example.com/_rustybin/mcp");
        assert_eq!(a.token.as_deref(), Some("tok-mocks"));
        // A URL: the token of the profile kept for it.
        let a =
            parse_with(&["--url".into(), "https://mocks.example.com/".into()], &env).expect("args");
        assert_eq!(a.server, "https://mocks.example.com/_rustybin/mcp");
        assert_eq!(a.token.as_deref(), Some("tok-mocks"));
        // A URL without a profile: no token.
        let a = parse_with(&["--url".into(), "https://new.example".into()], &env).expect("args");
        assert_eq!(a.server, "https://new.example/_rustybin/mcp");
        assert!(a.token.is_none());
        // Nothing: the default profile.
        let a = parse_with(&[], &env).expect("args");
        assert_eq!(a.server, "https://other.example/_rustybin/mcp");
        assert_eq!(a.token.as_deref(), Some("tok-other"));
        // --token wins over the profile's.
        let a = parse_with(
            &[
                "--profile".into(),
                "other".into(),
                "--token".into(),
                "mine".into(),
            ],
            &env,
        )
        .expect("args");
        assert_eq!(a.token.as_deref(), Some("mine"));
        assert!(parse_with(&["--profile".into(), "nope".into()], &env).is_err());
    }

    #[test]
    fn a_profiles_agent_sign_in_is_used_before_its_token() {
        use crate::cli::profiles::{AgentLogin, Profile, Profiles};
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(crate::cli::profiles::FILE);
        let mut profiles = Profiles::default();
        profiles.profiles.insert(
            "both".into(),
            Profile {
                url: "https://mocks.example.com".into(),
                token: Some("tok-person".into()),
                agent: Some(AgentLogin {
                    client_id: "rbc.x.y".into(),
                    access_token: "tok-agent".into(),
                    refresh_token: Some("tok-refresh".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        profiles.default = Some("both".into());
        crate::cli::profiles::save(&path, &profiles).expect("save");
        let env = crate::cli::Env {
            config_home: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let a = parse_with(&[], &env).expect("args");
        assert_eq!(a.token.as_deref(), Some("tok-agent"));
        let agent = a.agent.expect("the agent sign-in");
        assert_eq!(agent.url, "https://mocks.example.com");
        assert_eq!(agent.profile, Some((path, "both".to_string())));
        // A token given by hand wins, and nothing is renewed.
        let a = parse_with(&["--token".into(), "mine".into()], &env).expect("args");
        assert_eq!(a.token.as_deref(), Some("mine"));
        assert!(a.agent.is_none());
    }

    #[test]
    fn splits_sse_data() {
        let doc = "event: message\ndata: {\"a\":1}\n\n: keepalive\n\ndata: {\"b\":\ndata: 2}\n\n";
        assert_eq!(sse_data(doc), vec!["{\"a\":1}", "{\"b\":\n2}"]);
    }
}
