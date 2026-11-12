//! Core next 12 on the command line: the agent sign-in (`rustybin login
//! --agent`) through the control plane's own authorization server, and a
//! secret an agent asked for, fulfilled or declined from the terminal.

use super::*;

/// What a browser does with `rustybin login --agent`'s address: the
/// authorization endpoint sends it to the console's consent view, where
/// the signed-in user answers (the consent API, with their own token), and
/// the answer's address is the CLI's loopback listener.
async fn browse_consent(start: String, bearer: String, decision: &'static str) {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");
    let r = client.get(&start).send().await.expect("authorize");
    let consent = r
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(consent.starts_with("/ui/#/authorize/"), "{consent}");
    let id = consent.rsplit('/').next().expect("id").to_string();
    let base = start.split("/_rustybin/").next().expect("base").to_string();
    let answer: Value = client
        .post(format!("{base}/_rustybin/oauth/requests/{id}"))
        .bearer_auth(&bearer)
        .json(&json!({ "decision": decision }))
        .send()
        .await
        .expect("consent")
        .json()
        .await
        .expect("json");
    let back = answer["redirect_to"].as_str().expect("redirect_to");
    assert!(back.starts_with("http://127.0.0.1:"), "{back}");
    let _ = client.get(back).send().await;
}

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": { "name": "claude-code", "version": "1" },
    })
}

/// One `tools/list` on the management server as the client `claude-code`.
async fn mcp_status(url: &str, token: &str) -> u16 {
    reqwest::Client::new()
        .post(format!("{url}/_rustybin/mcp"))
        .bearer_auth(token)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", "tools/list")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": { "_meta": meta() } }))
        .send()
        .await
        .expect("mcp")
        .status()
        .as_u16()
}

#[tokio::test]
async fn login_agent_walks_the_control_planes_own_oauth() {
    let inst = instance().await;
    inst.login().await;
    let file = inst.home.path().join("cli/profiles.json");
    let name = profiles::name_for(&inst.url);
    let person = profiles::load(&file).expect("profiles").profiles[&name]
        .token
        .clone()
        .expect("the person's token");

    let run = |decision: &'static str, profile: Option<&'static str>| {
        let mut env = inst.env(None);
        let bearer = person.clone();
        env.browser = Some(Browser(Arc::new(move |url: &str| {
            tokio::spawn(browse_consent(url.to_string(), bearer.clone(), decision));
        })));
        let url = inst.url.clone();
        async move {
            let buf = Buf::default();
            let mut out: Out = Box::new(buf.clone());
            let mut args: Vec<String> = ["login", "--agent", "--url", url.as_str(), "--wait", "20"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            if let Some(p) = profile {
                args.extend(["--profile".to_string(), p.to_string()]);
            }
            let r = execute_inner(&args, &env, &mut out).await;
            (r, buf.text())
        }
    };

    // Denied on the consent screen: the refusal comes back, nothing is kept.
    let (r, out) = run("deny", None).await;
    let f = r.expect_err("denied");
    assert_eq!(f.code(), 3, "{out}");
    assert!(f.message().contains("access_denied"), "{}", f.message());
    assert!(profiles::load(&file).expect("profiles").profiles[&name]
        .agent
        .is_none());

    // Approved: the pair is kept beside the person's token.
    let (r, out) = run("approve", None).await;
    assert!(
        r.is_ok(),
        "{out} {:?}",
        r.err().map(|f| f.message().to_string())
    );
    assert!(
        out.contains(&format!("rustybin mcp-stdio --profile {name}"))
            && out.contains("mcp-stdio only"),
        "{out}"
    );
    let kept = profiles::load(&file).expect("profiles");
    assert_eq!(kept.default.as_deref(), Some(name.as_str()));
    let profile = &kept.profiles[&name];
    assert_eq!(profile.token.as_deref(), Some(person.as_str()));
    let agent = profile.agent.clone().expect("the agent sign-in");
    assert!(agent.client_id.starts_with("rbc."));
    assert!(agent.refresh_token.is_some() && agent.expires_at.is_some());
    assert!(agent_login::is_agent_token(&agent.access_token));
    assert!(!agent_login::is_agent_token(&person));
    assert!(!agent_login::is_agent_token("not-a-jwt"));
    assert!(!agent_login::expiring(
        &agent,
        agent_login::RENEW_BEFORE_SECS
    ));
    assert!(!format!("{agent:?}").contains(&agent.access_token));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // The person's commands still carry the person's token.
    assert_eq!(inst.json(&["whoami"]).await["user"], json!("rustybin"));
    // The agent's token works on /_rustybin/mcp and nowhere else.
    assert_eq!(mcp_status(&inst.url, &agent.access_token).await, 200);
    let status = reqwest::Client::new()
        .get(format!("{}/_rustybin/status", inst.url))
        .bearer_auth(&agent.access_token)
        .send()
        .await
        .expect("status")
        .status();
    assert_eq!(status.as_u16(), 401);
    // The other commands say so themselves instead of that 401.
    for args in [
        vec!["status", "--token", &agent.access_token],
        vec!["mocks", "list", "--token", &agent.access_token],
        vec!["login", "--url", &inst.url, "--token", &agent.access_token],
    ] {
        let (code, _, message) = inst.run_full(None, &args).await;
        assert_eq!(code, 3, "{}", args[0]);
        assert!(
            message.contains("works with rustybin mcp-stdio only"),
            "{}: {message}",
            args[0]
        );
    }

    // It acts for the user who approved, and is listed as theirs.
    let agents = inst.json(&["agents", "list"]).await;
    let listed = agents["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find(|a| a["credential"]["via"] == json!("oauth"))
        .cloned()
        .expect("the OAuth agent");
    assert_eq!(listed["name"], json!("claude-code"));
    assert_eq!(listed["credential"]["kind"], json!("user"));
    assert_eq!(listed["owner_label"], json!("rustybin"));
    let agent_id = listed["id"].as_str().expect("id").to_string();

    // A profile with nothing but an agent sign-in: said before any call;
    // a command whose routes are open still runs.
    let (r, out) = run("approve", Some("agent-only")).await;
    assert!(r.is_ok(), "{out}");
    let kept = profiles::load(&file).expect("profiles");
    assert_eq!(kept.default.as_deref(), Some(name.as_str()));
    assert!(kept.profiles["agent-only"].token.is_none());
    // The same registration, so the same agent on the instance.
    assert_eq!(
        kept.profiles["agent-only"]
            .agent
            .as_ref()
            .map(|a| a.client_id.as_str()),
        Some(agent.client_id.as_str())
    );
    let (code, _, message) = inst
        .run_full(None, &["status", "--profile", "agent-only"])
        .await;
    assert_eq!(code, 3);
    assert!(
        message.contains("holds an agent sign-in only") && message.contains("mcp-stdio only"),
        "{message}"
    );
    let (code, _) = inst
        .run(None, &["instance", "ready", "--profile", "agent-only"])
        .await;
    assert_eq!(code, 0);

    // mcp-stdio's session: an access token about to expire is renewed with
    // the refresh token and the new pair is written back.
    let mut session = agent_login::AgentSession {
        url: inst.url.clone(),
        profile: Some((file.clone(), name.clone())),
        login: agent.clone(),
    };
    let (same, renewed) = session.bearer().await;
    assert!(renewed.is_ok());
    assert_eq!(same, agent.access_token);
    session.login.expires_at =
        Some((chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc3339());
    let (fresh, renewed) = session.bearer().await;
    assert!(
        renewed.is_ok(),
        "{:?}",
        renewed.err().map(|f| f.message().to_string())
    );
    assert_ne!(fresh, agent.access_token);
    assert_eq!(mcp_status(&inst.url, &fresh).await, 200);
    let written = profiles::load(&file).expect("profiles").profiles[&name]
        .agent
        .clone()
        .expect("agent");
    assert_eq!(written.access_token, fresh);
    assert!(!agent_login::expiring(
        &written,
        agent_login::RENEW_BEFORE_SECS
    ));

    // Revoked like any other agent: its tokens end, the refresh included.
    let (code, out) = inst
        .run(None, &["agents", "revoke", &agent_id, "--yes"])
        .await;
    assert_eq!(code, 0, "{out}");
    assert_eq!(mcp_status(&inst.url, &fresh).await, 401);
    let ended = session.renew().await.expect_err("revoked");
    assert_eq!(ended.code(), 3);
    assert!(
        ended.message().contains("rustybin login --agent"),
        "{}",
        ended.message()
    );
    // The person stays signed in.
    assert_eq!(inst.json(&["whoami"]).await["user"], json!("rustybin"));

    // Signing out forgets the agent sign-in too.
    let (code, out) = inst.run(None, &["logout"]).await;
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("agent sign-in is forgotten"), "{out}");
    let after = profiles::load(&file).expect("profiles");
    assert!(after.profiles[&name].agent.is_none() && after.profiles[&name].token.is_none());
    inst.server.shutdown();
}

#[tokio::test]
async fn login_agent_says_when_the_instance_offers_no_agent_sign_in() {
    // An open control plane has no authorization server.
    let home = tempfile::tempdir().expect("tempdir");
    let mut config = Config::for_tests();
    config.tls_cert = home.path().join("server.crt").display().to_string();
    config.tls_key = home.path().join("server.key").display().to_string();
    let server = crate::server::start_with_state(AppState::for_tests(config))
        .await
        .expect("server starts");
    let url = format!("http://{}", server.http_addr);
    let env = Env {
        config_home: Some(home.path().join("cli")),
        ..Env::default()
    };
    let mut out: Out = Box::<Buf>::default();
    let args: Vec<String> = [
        "login",
        "--agent",
        "--url",
        &url,
        "--no-browser",
        "--wait",
        "1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let f = execute_inner(&args, &env, &mut out)
        .await
        .expect_err("no sign-in");
    assert_eq!(f.code(), 1);
    assert!(f.message().contains("not offered here"), "{}", f.message());
    // --agent goes with neither --sso nor --token.
    for extra in [vec!["--sso"], vec!["--token", "t"]] {
        let mut args: Vec<String> = ["login", "--agent", "--url", &url]
            .iter()
            .map(|s| s.to_string())
            .collect();
        args.extend(extra.iter().map(|s| s.to_string()));
        let f = execute_inner(&args, &env, &mut out)
            .await
            .expect_err("usage");
        assert_eq!(f.code(), 2, "{}", f.message());
    }
    server.shutdown();
}

#[tokio::test]
async fn settings_fulfil_reads_the_value_from_stdin_never_an_argument() {
    let inst = instance().await;
    inst.login().await;
    let file = inst.home.path().join("cli/profiles.json");
    let person = profiles::load(&file).expect("profiles").profiles[&profiles::name_for(&inst.url)]
        .token
        .clone()
        .expect("token");
    // Off: the list says so.
    let (code, out) = inst.run(None, &["settings", "requests"]).await;
    assert_eq!(code, 0);
    assert!(out.contains("Secret requests are off"), "{out}");
    let set = inst
        .json(&["settings", "set", "agents.secret_requests", "true"])
        .await;
    assert_eq!(set["changed"], json!(["agents.secret_requests"]));

    // An agent asks, over the management server.
    let ask = |setting: &'static str| {
        let (url, person) = (inst.url.clone(), person.clone());
        async move {
            let v: Value = reqwest::Client::new()
                .post(format!("{url}/_rustybin/mcp"))
                .bearer_auth(&person)
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", "tools/call")
                .header("mcp-name", "request_secret")
                .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                    "name": "request_secret",
                    "arguments": { "setting": setting, "reason": "the failing schedule\u{1b}[2J posts to the team's webhook" },
                    "_meta": meta() } }))
                .send()
                .await
                .expect("mcp")
                .json()
                .await
                .expect("json");
            v["result"]["structuredContent"]["request"]["id"]
                .as_str()
                .unwrap_or_else(|| panic!("an id: {v}"))
                .to_string()
        }
    };
    let id = ask("notify.webhook_token").await;
    let listed = inst
        .json(&["settings", "requests", "--state", "pending"])
        .await;
    assert_eq!(listed["requests"][0]["id"], json!(id));
    assert_eq!(
        listed["requests"][0]["agent"]["owner_label"],
        json!("rustybin")
    );
    let (code, out) = inst.run(None, &["settings", "requests"]).await;
    assert_eq!(code, 0);
    assert!(
        out.contains(&id) && out.contains("notify.webhook_token") && out.contains("claude-code"),
        "{out}"
    );
    // What the agent wrote is text: no escape reaches the terminal.
    assert!(!out.contains('\u{1b}'), "{out:?}");

    // The value is never an argument or a flag.
    let value = "PLANTED-cli-value-77";
    let (code, _, message) = inst
        .run_full(Some("x\n"), &["settings", "fulfil", &id, value])
        .await;
    assert_eq!(code, 2);
    assert!(message.contains("never an argument"), "{message}");
    let (code, _, _) = inst
        .run_full(Some("x\n"), &["settings", "fulfil", &id, "--value", value])
        .await;
    assert_eq!(code, 2);
    // No value: nothing is sent.
    let (code, _, _) = inst
        .run_full(Some("\n"), &["settings", "fulfil", &id, "--stdin"])
        .await;
    assert_eq!(code, 2);
    assert!(inst.state.settings.secret("notify.webhook_token").is_none());
    // From stdin: set, and what was typed is not printed back.
    let (code, out) = inst
        .run(
            Some(&format!("{value}\n")),
            &["settings", "fulfil", &id, "--stdin"],
        )
        .await;
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("notify.webhook_token is set")
            && out.contains("its claim, not an instruction")
            && out.contains("acting for rustybin"),
        "{out}"
    );
    assert!(
        !out.contains("PLANTED") && !out.contains('\u{1b}'),
        "{out:?}"
    );
    assert_eq!(
        inst.state
            .settings
            .secret("notify.webhook_token")
            .as_deref()
            .map(String::as_str),
        Some(value)
    );
    // Fulfilled once: a second time is refused (exit 1).
    let (code, _) = inst
        .run(Some("again\n"), &["settings", "fulfil", &id, "--stdin"])
        .await;
    assert_eq!(code, 1);
    // Another one, declined.
    let id = ask("git.token").await;
    let declined = inst.json(&["settings", "decline", &id]).await;
    assert_eq!(declined["state"], json!("declined"));
    assert!(inst.state.settings.secret("git.token").is_none());
    let (code, _) = inst.run(None, &["settings", "decline", &id]).await;
    assert_eq!(code, 1);
    let (code, _) = inst.run(None, &["settings", "decline"]).await;
    assert_eq!(code, 2);
    // With a note for the agent (core next 17): cleaned by the instance,
    // listed with the request.
    let id = ask("git.token").await;
    let (code, out) = inst
        .run(
            None,
            &[
                "settings",
                "decline",
                &id,
                "--note",
                "use the deploy key\u{1b}[2J that is set",
            ],
        )
        .await;
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("The agent reads your note"), "{out}");
    let listed = inst
        .json(&["settings", "requests", "--state", "declined"])
        .await;
    assert_eq!(
        listed["requests"][0]["note"],
        json!("use the deploy key [2J that is set")
    );
    let (code, out) = inst
        .run(None, &["settings", "requests", "--state", "declined"])
        .await;
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("use the deploy key [2J that is set"), "{out}");
    inst.server.shutdown();
}
