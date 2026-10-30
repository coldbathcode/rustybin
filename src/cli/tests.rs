//! The command line: argument parsing, profiles, rendering, the parity of
//! the command tree with the route catalogue, the generated reference, and
//! every noun against a real instance in password mode.

use std::io::Write;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::*;
use crate::config::Config;
use crate::control_auth::ControlAuth;
use crate::state::AppState;

/// An `Out` the test reads back.
#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().map(|mut v| v.extend_from_slice(b)).ok();
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Buf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().map(|v| v.clone()).unwrap_or_default()).to_string()
    }
}

const TEST_FLAGS: &[Flag] = &[
    Flag::value("--spec", ""),
    Flag::switch("--force", ""),
    Flag::value("-m", ""),
];

fn flags() -> &'static [Flag] {
    TEST_FLAGS
}

#[test]
fn arguments_parse_flags_values_and_positionals() {
    let raw: Vec<String> = [
        "orders",
        "--spec",
        "a.yaml",
        "--force",
        "--json",
        "-m",
        "msg",
        "--url=http://x",
        "--",
        "--not-a-flag",
        "-1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let a = Args::parse(&raw, flags()).expect("parse");
    assert_eq!(a.positional, vec!["orders", "--not-a-flag", "-1"]);
    assert_eq!(a.value("--spec"), Some("a.yaml"));
    assert!(a.has("--force") && a.has("--json"));
    assert_eq!(a.value("-m"), Some("msg"));
    assert_eq!(a.value("--url"), Some("http://x"));
    assert_eq!(a.arg(0, "x").expect("arg"), "orders");
    assert_eq!(
        a.arg(3, "a thing"),
        Err(Fail::Usage("a thing is required".into()))
    );
    let bad: Vec<String> = vec!["--nope".into()];
    assert!(
        matches!(Args::parse(&bad, flags()), Err(Fail::Usage(m)) if m.contains("unknown flag"))
    );
    let missing: Vec<String> = vec!["--spec".into()];
    assert!(
        matches!(Args::parse(&missing, flags()), Err(Fail::Usage(m)) if m.contains("needs a value"))
    );
    let negative: Vec<String> = vec!["-5".into()];
    assert_eq!(
        Args::parse(&negative, flags())
            .expect("negative")
            .positional,
        vec!["-5"]
    );
}

#[test]
fn profiles_resolve_in_order_and_are_private() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = Env {
        config_home: Some(dir.path().to_path_buf()),
        http_port: Some(8080),
        ..Env::default()
    };
    let path = profiles::path(&env).expect("path");
    assert_eq!(path, dir.path().join("profiles.json"));
    let none = Args::default();
    let r = profiles::resolve(&env, &none).expect("resolve");
    assert_eq!(r.url, "http://localhost:8080");
    assert_eq!(r.token, None);
    let mut all = profiles::Profiles::default();
    all.profiles.insert(
        "demo".into(),
        profiles::Profile {
            url: "https://demo.example".into(),
            token: Some("t-demo".into()),
            ..Default::default()
        },
    );
    all.default = Some("demo".into());
    profiles::save(&path, &all).expect("save");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let r = profiles::resolve(&env, &none).expect("default profile");
    assert_eq!(
        (r.url.as_str(), r.token.as_deref(), r.profile.as_deref()),
        ("https://demo.example", Some("t-demo"), Some("demo"))
    );
    // The environment wins over the profile; the profile's token only goes
    // to the profile's URL.
    let env2 = Env {
        url: Some("https://other.example/".into()),
        ..env.clone()
    };
    let r = profiles::resolve(&env2, &none).expect("env url");
    assert_eq!(
        (r.url.as_str(), r.token, r.profile),
        ("https://other.example", None, None)
    );
    let env3 = Env {
        token: Some("t-env".into()),
        ..env.clone()
    };
    assert_eq!(
        profiles::resolve(&env3, &none)
            .expect("env token")
            .token
            .as_deref(),
        Some("t-env")
    );
    // Flags win over everything; an unknown profile is an error.
    let flagged = Args::parse(
        &[
            "--url".to_string(),
            "demo.example:9".into(),
            "--token".into(),
            "t-flag".into(),
        ],
        &[],
    )
    .expect("args");
    let r = profiles::resolve(&env3, &flagged).expect("flags");
    assert_eq!(
        (r.url.as_str(), r.token.as_deref()),
        ("http://demo.example:9", Some("t-flag"))
    );
    let unknown = Args::parse(&["--profile".to_string(), "nope".into()], &[]).expect("args");
    assert!(
        matches!(profiles::resolve(&env, &unknown), Err(Fail::Usage(m)) if m.contains("no profile"))
    );
    assert_eq!(
        profiles::name_for("https://mocks.example.com:8443/"),
        "mocks.example.com-8443"
    );
    assert_eq!(profiles::name_for(""), "default");
}

#[test]
fn rendering_makes_tables_and_key_values() {
    let v = json!({
        "name": "orders", "enabled": true, "tags": ["a", "b"], "nothing": null,
        "nested": { "deep": 1 },
        "rows": [{ "id": 1, "state": "live", "extra": { "x": 1 } }, { "id": 2, "state": "draft" }],
    });
    let text = render(&v);
    assert_eq!(
        text,
        "name: orders\nenabled: true\ntags: a, b\nnothing: none\nnested:\n  deep: 1\nrows:\n  id  state\n  1   live\n  2   draft\n"
    );
    assert_eq!(render(&json!([])), "");
    assert_eq!(render(&json!({})), "(empty)\n");
}

#[test]
fn the_command_tree_is_consistent_with_the_catalogue() {
    // Every control route has a command, or a reason.
    let covered: std::collections::BTreeSet<&str> = COMMANDS
        .iter()
        .flat_map(|c| c.routes.iter().map(|(_, p)| *p))
        .collect();
    let uncovered: std::collections::BTreeMap<&str, &str> =
        commands::UNCOVERED.iter().copied().collect();
    let mut missing = Vec::new();
    let mut catalogue_paths = std::collections::BTreeSet::new();
    for e in crate::catalog::all() {
        if !(e.path.starts_with("/_rustybin/") || e.path.starts_with("/bin")) {
            continue;
        }
        catalogue_paths.insert(e.path);
        if !covered.contains(e.path) && !uncovered.contains_key(e.path) {
            missing.push(e.path);
        }
    }
    assert!(missing.is_empty(), "control routes without a command (add one, or list it in UNCOVERED with a reason): {missing:?}");
    for (p, reason) in commands::UNCOVERED {
        assert!(
            catalogue_paths.contains(p),
            "UNCOVERED names a route that is gone: {p}"
        );
        assert!(!covered.contains(p), "{p} is both covered and in UNCOVERED");
        assert!(!reason.is_empty());
    }
    // Every command's routes exist with that method.
    for c in COMMANDS {
        for (m, p) in c.routes {
            let e = crate::catalog::all()
                .iter()
                .find(|e| e.path == *p)
                .unwrap_or_else(|| panic!("{} calls {p}, which is not in the catalogue", c.line()));
            assert!(
                e.expanded_methods().contains(m),
                "{} calls {m} {p}, but the route takes {:?}",
                c.line(),
                e.expanded_methods()
            );
        }
        assert!(
            NOUNS.iter().any(|(n, _)| *n == c.noun),
            "{} has no noun entry",
            c.line()
        );
        assert!(
            !c.summary.is_empty() && !c.example.is_empty(),
            "{} needs a summary and an example",
            c.line()
        );
        assert!(
            c.example.starts_with(c.noun),
            "{}: the example starts with the noun",
            c.line()
        );
        for f in c.flags {
            assert!(
                !GLOBAL_FLAGS.iter().any(|g| g.name == f.name),
                "{}: {} clashes with a global flag",
                c.line(),
                f.name
            );
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for c in COMMANDS {
        assert!(
            seen.insert((c.noun, c.verb)),
            "duplicate command {}",
            c.line()
        );
    }
    for (n, _) in NOUNS {
        assert!(
            COMMANDS.iter().any(|c| c.noun == *n),
            "noun {n} has no command"
        );
        assert!(help(n).is_some());
    }
    assert!(usage().contains("rustybin help <noun>"));
    assert!(command_help(find("mocks", "create").expect("cmd")).contains("--spec <value>"));
    for shell in ["bash", "zsh", "fish"] {
        let script = completion(shell).expect("script");
        assert!(
            script.contains("traffic-tail"),
            "{shell} completion lists the verbs"
        );
    }
    assert!(completion("powershell").is_none());
}

#[test]
fn cli_docs() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/src/cli.md");
    let generated = reference_markdown();
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    if current != generated {
        if std::env::var("RUSTYBIN_UPDATE_CLI_DOCS").is_ok() {
            std::fs::write(&path, generated).expect("write docs/src/cli.md");
            return;
        }
        panic!("docs/src/cli.md is stale: regenerate with RUSTYBIN_UPDATE_CLI_DOCS=1 cargo test cli_docs");
    }
}

// ── Against an instance ─────────────────────────────────────────────

struct Instance {
    server: crate::server::RunningServer,
    url: String,
    home: tempfile::TempDir,
}

async fn instance() -> Instance {
    let home = tempfile::tempdir().expect("tempdir");
    let mut config = Config::for_tests();
    config.tls_cert = home.path().join("server.crt").display().to_string();
    config.tls_key = home.path().join("server.key").display().to_string();
    config.control_auth = ControlAuth::Password;
    config.state_dir = Some(home.path().join("state"));
    config.egress_allow_loopback = true;
    let server = crate::server::start_with_state(AppState::for_tests(config))
        .await
        .expect("server starts");
    let url = format!("http://{}", server.http_addr);
    Instance { server, url, home }
}

impl Instance {
    fn env(&self, stdin: Option<&str>) -> Env {
        Env {
            config_home: Some(self.home.path().join("cli")),
            stdin: stdin.map(str::to_string),
            ..Env::default()
        }
    }

    /// Run a command line: (exit code, output).
    async fn run(&self, stdin: Option<&str>, args: &[&str]) -> (u8, String) {
        let (code, out, _) = self.run_full(stdin, args).await;
        (code, out)
    }

    /// Run a command line: (exit code, output, the failure's message).
    async fn run_full(&self, stdin: Option<&str>, args: &[&str]) -> (u8, String, String) {
        let buf = Buf::default();
        let mut out: Out = Box::new(buf.clone());
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (code, message) = match execute_inner(&args, &self.env(stdin), &mut out).await {
            Ok(()) => (0, String::new()),
            Err(f) => (f.code(), f.message().to_string()),
        };
        (code, buf.text(), message)
    }

    async fn json(&self, args: &[&str]) -> Value {
        let mut all: Vec<&str> = args.to_vec();
        all.push("--json");
        let (code, out) = self.run(None, &all).await;
        assert_eq!(code, 0, "{}: {out}", args.join(" "));
        serde_json::from_str(&out)
            .unwrap_or_else(|e| panic!("{}: not JSON ({e}): {out}", args.join(" ")))
    }

    async fn login(&self) {
        let (code, out) = self
            .run(
                Some("rustybin\n"),
                &["login", "--url", &self.url, "--password-stdin"],
            )
            .await;
        assert_eq!(code, 0, "{out}");
    }
}

const SPEC: &str = r#"{"openapi":"3.0.3","info":{"title":"Orders","version":"1"},"paths":{"/orders":{"get":{"operationId":"listOrders","responses":{"200":{"description":"ok","content":{"application/json":{"schema":{"type":"array","items":{"type":"object","properties":{"id":{"type":"integer"}}}}}}}}}}}}"#;

/// Split an example the way a shell would (single and double quotes), and
/// cut a redirection off.
fn shell_words(line: &str) -> Vec<String> {
    let line = line.split(" < ").next().unwrap_or(line);
    let line = line.split(" > ").next().unwrap_or(line);
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut had = false;
    for ch in line.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => cur.push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                had = true;
            }
            None if ch.is_whitespace() => {
                if had || !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                    had = false;
                }
            }
            None => cur.push(ch),
        }
    }
    if had || !cur.is_empty() {
        words.push(cur);
    }
    words
}

#[tokio::test]
async fn every_command_runs_against_an_instance() {
    let inst = instance().await;
    // Not signed in: the credential failure is exit 3, usage is 2.
    let (code, _) = inst.run(None, &["status", "--url", &inst.url]).await;
    assert_eq!(code, 3);
    let (code, _) = inst.run(None, &["mocks", "nope"]).await;
    assert_eq!(code, 2);
    let (code, _) = inst.run(None, &["nope"]).await;
    assert_eq!(code, 2);
    let (code, out) = inst.run(None, &["help", "mocks"]).await;
    assert_eq!(code, 0);
    assert!(out.contains("traffic-tail"));
    // A wrong password.
    let (code, _) = inst
        .run(
            Some("wrong\n"),
            &["login", "--url", &inst.url, "--password-stdin"],
        )
        .await;
    assert_eq!(code, 3);
    inst.login().await;
    let profiles_file = inst.home.path().join("cli/profiles.json");
    assert!(profiles_file.exists());
    let who = inst.json(&["whoami"]).await;
    assert_eq!(who["user"], json!("rustybin"));

    // Files the examples name.
    let files = inst.home.path().join("files");
    std::fs::create_dir_all(&files).expect("files");
    let write =
        |name: &str, content: &str| std::fs::write(files.join(name), content).expect("write");
    write("orders.yaml", SPEC);
    write("order.json", r#"{"id": 1}"#);
    write("rustybin.yaml", "version: 2\nhealth: {initial: healthy}\n");
    write("key.txt", "sk-test-key-1234\n");
    write("passwords.txt", "rustybin\nrustybin-2\n");
    write(
        "server.json",
        r#"{"$schema":"https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json","name":"io.example/orders","description":"Orders","version":"1.0.0","remotes":[{"type":"streamable-http","url":"https://mcp.example.com/orders"}]}"#,
    );
    write("smoke.yaml", "name: smoke\ntarget: http://127.0.0.1:1\nsource: { request: { path: / } }\npattern: { type: constant, rate: 1, for: 1s }\n");
    // A mock to point the examples at.
    let created = inst
        .json(&[
            "mocks",
            "create",
            "orders",
            "--spec",
            &files.join("orders.yaml").display().to_string(),
        ])
        .await;
    assert_eq!(created["name"], json!("orders"));

    let mut failures = Vec::new();
    for c in COMMANDS {
        if matches!(
            (c.noun, c.verb),
            ("logout", "")
                | ("sessions", "end-all")
                | ("password", "change")
                | ("profiles", "remove")
        ) {
            continue; // exercised below, in order
        }
        let mut args: Vec<String> = shell_words(c.example)
            .into_iter()
            .map(|w| {
                let w = w
                    .replace("http://localhost:8080", &inst.url)
                    .replace("https://mocks.example.com", &inst.url)
                    .replace("mocks.example.com", &profiles::name_for(&inst.url));
                for f in [
                    "orders.yaml",
                    "order.json",
                    "rustybin.yaml",
                    "key.txt",
                    "server.json",
                    "smoke.yaml",
                    "mcp.xml",
                ] {
                    if w == f {
                        return files.join(f).display().to_string();
                    }
                }
                w
            })
            .collect();
        args.push("--yes".into());
        if c.flags.iter().any(|f| f.name == "--timeout") {
            args.extend(["--timeout".to_string(), "1".to_string()]);
        }
        // Every command gets a canned stdin: nothing may wait on the real one.
        let stdin = Some("rustybin\nsk-test-key-1234\n");
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, _, message) = inst.run_full(stdin, &refs).await;
        if code == 2 || code == 3 {
            failures.push(format!("{} -> exit {code}: {message}", refs.join(" ")));
        }
    }
    assert!(
        failures.is_empty(),
        "commands with a usage or credential failure:\n{}",
        failures.join("\n")
    );

    // Shapes and effects of a few (the examples deleted the mock: back).
    let _ = inst
        .run(
            None,
            &[
                "mocks",
                "create",
                "orders",
                "--spec",
                &files.join("orders.yaml").display().to_string(),
                "--quiet",
            ],
        )
        .await;
    let list = inst.json(&["mocks", "list"]).await;
    assert!(list["mocks"]
        .as_array()
        .expect("mocks")
        .iter()
        .any(|m| m["name"] == json!("orders")));
    let (code, text) = inst.run(None, &["mocks", "list"]).await;
    assert_eq!(code, 0);
    assert!(text.contains("name") && text.contains("orders"), "{text}");
    let set = inst
        .json(&["settings", "set", "console.title", "From the CLI"])
        .await;
    assert_eq!(set["changed"], json!(["console.title"]));
    let got = inst.json(&["settings", "get", "console.title"]).await;
    assert_eq!(got["value"], json!("From the CLI"));
    let (code, yaml) = inst.run(None, &["config", "export"]).await;
    assert_eq!(code, 0);
    assert!(
        yaml.contains("version: 2") && yaml.contains("title: From the CLI"),
        "{yaml}"
    );
    write("rustybin.yaml", "version: 2\nhealth: {initial: healthy}\n");
    let (code, diff) = inst
        .run(
            None,
            &[
                "config",
                "diff",
                &files.join("rustybin.yaml").display().to_string(),
            ],
        )
        .await;
    assert_eq!(code, 0);
    assert!(diff.starts_with("would apply 0 change(s)"), "{diff}");
    let (code, out) = inst.run(None, &["git", "status"]).await;
    assert_eq!(code, 0);
    assert!(out.contains("initialised: false"), "{out}");
    let (code, out) = inst
        .run(
            None,
            &[
                "mocks",
                "traffic-tail",
                "orders",
                "--max",
                "1",
                "--timeout",
                "1",
            ],
        )
        .await;
    assert_eq!((code, out.as_str()), (0, ""));
    // A tail sees a request made meanwhile.
    let url = inst.url.clone();
    let poke = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let _ = reqwest::get(format!("{url}/get?from=cli")).await;
    });
    let (code, out) = inst
        .run(None, &["requests", "tail", "--max", "1", "--timeout", "5"])
        .await;
    poke.await.expect("poke");
    assert_eq!(code, 0);
    assert!(out.starts_with("request:"), "{out}");
    // Destructive without --yes and no terminal: refused as usage.
    let (code, _) = inst.run(None, &["mocks", "delete", "orders"]).await;
    assert_eq!(code, 2);
    let (code, _) = inst.run(Some("n\n"), &["mocks", "delete", "orders"]).await;
    assert_eq!(code, 1);
    assert!(inst.json(&["mocks", "get", "orders"]).await["name"] == json!("orders"));
    // The password changes and the profile keeps the fresh token.
    let (code, _) = inst
        .run(None, &["password", "change", "--stdin", "--quiet"])
        .await;
    assert_ne!(code, 3, "the canned stdin was not used");
    let (code, out) = inst
        .run(
            Some("rustybin\nrustybin-2\n"),
            &["password", "change", "--stdin"],
        )
        .await;
    assert_eq!(code, 0, "{out}");
    assert_eq!(inst.json(&["whoami"]).await["user"], json!("rustybin"));
    // Sign out everywhere, then the profile's token is refused; a fresh login works.
    let (code, _) = inst.run(None, &["sessions", "end-all", "--yes"]).await;
    assert_eq!(code, 0);
    let (code, _) = inst.run(None, &["status"]).await;
    assert_eq!(code, 3);
    let (code, _) = inst
        .run(
            Some("rustybin-2\n"),
            &["login", "--url", &inst.url, "--password-stdin"],
        )
        .await;
    assert_eq!(code, 0);
    let (code, out) = inst.run(None, &["logout"]).await;
    assert_eq!(code, 0, "{out}");
    let profiles = profiles::load(&profiles_file).expect("profiles");
    assert!(profiles.profiles.values().all(|p| p.token.is_none()));
    let (code, _) = inst.run(None, &["status"]).await;
    assert_eq!(code, 3);
    // A token kept without a sign-in, then profiles managed.
    let token = {
        let client = reqwest::Client::new();
        let v: Value = client
            .post(format!("{}/_rustybin/login", inst.url))
            .json(&json!({ "username": "rustybin", "password": "rustybin-2" }))
            .send()
            .await
            .expect("login")
            .json()
            .await
            .expect("json");
        v["token"].as_str().expect("token").to_string()
    };
    let (code, _) = inst
        .run(
            None,
            &[
                "login",
                "--url",
                &inst.url,
                "--token",
                &token,
                "--profile",
                "kept",
            ],
        )
        .await;
    assert_eq!(code, 0);
    let listed = inst.json(&["profiles", "list"]).await;
    assert_eq!(listed["default"], json!("kept"));
    let (code, _) = inst
        .run(None, &["profiles", "remove", "kept", "--yes"])
        .await;
    assert_eq!(code, 0);
    inst.server.shutdown();
}
