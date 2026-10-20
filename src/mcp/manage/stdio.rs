//! `rustybin mcp-stdio`: a stdio-to-Streamable-HTTP bridge for MCP clients
//! that only speak stdio. Each JSON-RPC message read from stdin is posted
//! to the server; JSON answers and the messages of SSE answers are written
//! to stdout, one per line. The session id of the handshake era is kept
//! and sent back; a 2026-07-28 client needs no session.
//!
//! ```text
//! rustybin mcp-stdio --server https://host/_rustybin/mcp --token <token>
//! ```

use std::process::ExitCode;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const USAGE: &str = "\
rustybin mcp-stdio: bridge a stdio MCP client to a Rustybin management server

USAGE:
    rustybin mcp-stdio --server <url> [--token <token>] [--header 'Name: value']...

    --server   The MCP endpoint, e.g. https://host/_rustybin/mcp (or $RUSTYBIN_MCP_URL;
               a bare base URL gets /_rustybin/mcp appended)
    --token    Sent as Authorization: Bearer <token> (or $RUSTYBIN_TOKEN / $RUSTYBIN_ADMIN_TOKEN)
    --header   An extra request header, repeatable (e.g. 'X-Rustybin-Mock-Token: rbm_...')

Messages are read from stdin (one JSON-RPC message per line) and answers written
to stdout, one per line.";

struct Args {
    server: String,
    token: Option<String>,
    headers: Vec<(String, String)>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut server: Option<String> = None;
    let mut token: Option<String> = None;
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
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("--server is required\n\n{USAGE}"))?;
    let server = server.trim().trim_end_matches('/').to_string();
    let server = if server.ends_with(super::MANAGE_PATH) || server.ends_with("/mcp") {
        server
    } else {
        format!("{server}{}", super::MANAGE_PATH)
    };
    let token = token
        .or_else(|| std::env::var("RUSTYBIN_TOKEN").ok())
        .or_else(|| std::env::var("RUSTYBIN_ADMIN_TOKEN").ok())
        .filter(|t| !t.trim().is_empty());
    Ok(Args {
        server,
        token,
        headers,
    })
}

/// Entry point of `rustybin mcp-stdio`.
pub async fn run(args: &[String]) -> ExitCode {
    let args = match parse(args) {
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
        let mut req = client
            .post(&args.server)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        if let Some(t) = &args.token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        for (n, v) in &args.headers {
            req = req.header(n.as_str(), v.as_str());
        }
        if let Some(s) = &session {
            req = req.header("mcp-session-id", s.as_str());
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
        let resp = match req.body(line).send().await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("rustybin mcp-stdio: request failed: {e}");
                write_error(&mut stdout, &msg, &format!("request failed: {e}")).await;
                continue;
            }
        };
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
    fn splits_sse_data() {
        let doc = "event: message\ndata: {\"a\":1}\n\n: keepalive\n\ndata: {\"b\":\ndata: 2}\n\n";
        assert_eq!(sse_data(doc), vec!["{\"a\":1}", "{\"b\":\n2}"]);
    }
}
