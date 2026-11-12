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

mod agent;

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
    // An environment token never goes to another instance a --url names
    // (it does go to a local one).
    let away = Args::parse(
        &["--url".to_string(), "https://elsewhere.example".into()],
        &[],
    )
    .expect("args");
    assert_eq!(profiles::resolve(&env3, &away).expect("away").token, None);
    let local =
        Args::parse(&["--url".to_string(), "http://127.0.0.1:9".into()], &[]).expect("args");
    assert_eq!(
        profiles::resolve(&env3, &local)
            .expect("local")
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

/// The longest one command line may take against the test instance.
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

struct Instance {
    server: crate::server::RunningServer,
    url: String,
    home: tempfile::TempDir,
    state: AppState,
}

async fn instance() -> Instance {
    instance_with(|_| {}).await
}

async fn instance_with(f: impl FnOnce(&mut Config)) -> Instance {
    let home = tempfile::tempdir().expect("tempdir");
    let mut config = Config::for_tests();
    f(&mut config);
    config.tls_cert = home.path().join("server.crt").display().to_string();
    config.tls_key = home.path().join("server.key").display().to_string();
    config.control_auth = ControlAuth::Password;
    config.state_dir = Some(home.path().join("state"));
    config.egress_allow_loopback = true;
    let state = AppState::for_tests(config);
    let server = crate::server::start_with_state(state.clone())
        .await
        .expect("server starts");
    let url = format!("http://{}", server.http_addr);
    Instance {
        server,
        url,
        home,
        state,
    }
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
    ///
    /// A command that does not finish in [`COMMAND_TIMEOUT`] fails the test
    /// with its command line instead of hanging the suite. A watchdog
    /// thread covers a command that blocks the test's runtime thread (where
    /// the timer cannot fire): it names the command and ends the process.
    async fn run_full(&self, stdin: Option<&str>, args: &[&str]) -> (u8, String, String) {
        let line = args.join(" ");
        let (done, watch) = std::sync::mpsc::channel::<()>();
        let watched = line.clone();
        std::thread::spawn(move || {
            if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                watch.recv_timeout(COMMAND_TIMEOUT * 3)
            {
                // Straight to stderr: `eprintln!` here goes to the test's
                // capture, which `exit` throws away with the reason.
                let _ = writeln!(
                    std::io::stderr(),
                    "rustybin {watched}: blocked for {COMMAND_TIMEOUT:?} x 3, ending the test process"
                );
                std::process::exit(101);
            }
        });
        let buf = Buf::default();
        let mut out: Out = Box::new(buf.clone());
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let env = self.env(stdin);
        let result =
            tokio::time::timeout(COMMAND_TIMEOUT, execute_inner(&args, &env, &mut out)).await;
        let _ = done.send(());
        let (code, message) = match result {
            Err(_) => panic!("rustybin {line}: did not finish in {COMMAND_TIMEOUT:?}"),
            Ok(Ok(())) => (0, String::new()),
            Ok(Err(f)) => (f.code(), f.message().to_string()),
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
        "recording.har",
        r#"{"log":{"entries":[{"request":{"method":"GET","url":"https://a.test/x","headers":[]},"response":{"status":200}}]}}"#,
    );
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
                | ("password", "hash")
                | ("passwd", "")
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
                    "recording.har",
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
    // A kept revision: JSON, then the summary for people; one never kept is exit 1.
    let rev = inst.json(&["mocks", "get", "orders"]).await["draft_revision"]
        .as_u64()
        .expect("rev")
        .to_string();
    let got = inst.json(&["mocks", "revision", "orders", &rev]).await;
    assert_eq!(got["revision"].to_string(), rev);
    assert!(got["spec"]["text"].is_string(), "{got}");
    let (code, text) = inst.run(None, &["mocks", "revision", "orders", &rev]).await;
    assert_eq!(code, 0);
    assert!(text.contains(&format!("revision {rev}")), "{text}");
    let (code, _) = inst.run(None, &["mocks", "revision", "orders", "1"]).await;
    assert_eq!(code, 1);
    let (code, _) = inst.run(None, &["mocks", "revision", "orders", "x"]).await;
    assert_eq!(code, 2);
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
        .run(Some(""), &["password", "change", "--stdin", "--quiet"])
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
    // Your own password, and back; the profile keeps the fresh token.
    let (code, out) = inst
        .run(Some("rustybin-2\nrustybin-2b\n"), &["passwd", "--stdin"])
        .await;
    assert_eq!(code, 0, "{out}");
    let (code, out) = inst
        .run(Some("rustybin-2b\nrustybin-2\n"), &["passwd", "--stdin"])
        .await;
    assert_eq!(code, 0, "{out}");
    assert_eq!(inst.json(&["me"]).await["name"], json!("rustybin"));
    // A user invited, joined through the link, then seen as active.
    let invited = inst
        .json(&["users", "invite", "bob", "--role", "viewer,tester"])
        .await;
    let url = invited["invitation"]["url"]
        .as_str()
        .expect("url")
        .to_string();
    assert!(
        url.starts_with(&format!("{}/ui/#/join/rbi_", inst.url)),
        "{url}"
    );
    let (code, out) = inst
        .run(None, &["users", "invite", "bob", "--role", "viewer"])
        .await;
    assert_eq!(code, 1, "a second invitation of a name is refused: {out}");
    let token = url.rsplit('/').next().expect("token").to_string();
    let joined: Value = reqwest::Client::new()
        .post(format!("{}/_rustybin/join", inst.url))
        .json(&json!({ "token": token, "password": "bob-secret-1" }))
        .send()
        .await
        .expect("join")
        .json()
        .await
        .expect("json");
    assert_eq!(joined["user"], json!("bob"), "{joined}");
    let bob = inst.json(&["users", "show", "bob"]).await;
    assert_eq!(bob["status"], json!("active"));
    assert_eq!(bob["roles"], json!(["viewer", "tester"]));
    let (code, text) = inst.run(None, &["users", "list"]).await;
    assert_eq!(code, 0);
    assert!(text.contains("bob") && text.contains("active"), "{text}");
    let (code, _) = inst.run(None, &["users", "delete", "bob", "--yes"]).await;
    assert_eq!(code, 0);
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

#[tokio::test]
async fn ai_use_sets_the_model_the_fast_model_or_a_job() {
    let inst = instance_with(|c| c.assist_provider = Some("mock".into())).await;
    inst.login().await;
    let job = |v: &Value, task: &str| {
        v["jobs"]
            .as_array()
            .and_then(|a| a.iter().find(|j| j["task"] == task))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let v = inst
        .json(&["ai", "use", "mock-fast-9", "--job", "explain"])
        .await;
    assert_eq!(job(&v, "explain")["model"], "mock-fast-9");
    assert_eq!(job(&v, "explain")["source"], "setting");
    let v = inst.json(&["ai", "use", "mock-small", "--fast"]).await;
    assert_eq!(
        job(&v, "suggest")["model"],
        "mock-small",
        "suggest is a fast job"
    );
    assert_eq!(job(&v, "explain")["model"], "mock-fast-9");
    let v = inst.json(&["ai", "use", "mock-big"]).await;
    assert_eq!(job(&v, "spec_from_description")["model"], "mock-big");
    // Both: a usage error, and nothing changes.
    let (code, _, message) = inst
        .run_full(None, &["ai", "use", "x", "--job", "explain", "--fast"])
        .await;
    assert_eq!(code, 2, "{message}");
    assert!(message.contains("--fast"), "{message}");
    let v = inst.json(&["ai", "jobs"]).await;
    assert_eq!(job(&v, "explain")["model"], "mock-fast-9");
    // An empty model with --job clears the choice.
    let v = inst.json(&["ai", "use", "", "--job", "explain"]).await;
    assert_eq!(job(&v, "explain")["source"], "default");
    inst.server.shutdown();
}

#[tokio::test]
async fn git_commit_takes_checkpoints_and_restore_reads_the_repository() {
    if !std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("git is not installed: skipped");
        return;
    }
    let inst = instance().await;
    inst.login().await;
    let bare = inst.home.path().join("remote.git");
    let out = std::process::Command::new("git")
        .args(["init", "--quiet", "--bare", "-b", "main"])
        .arg(&bare)
        .output()
        .expect("git init --bare");
    assert!(out.status.success());
    let remote = format!("file://{}", bare.display());
    inst.json(&["settings", "set", "git.remote", &remote]).await;
    inst.json(&["git", "init"]).await;
    inst.json(&["checkpoint", "save", "one"]).await;
    inst.json(&["checkpoint", "save", "two"]).await;
    let v = inst
        .json(&[
            "git",
            "commit",
            "-m",
            "Two checkpoints",
            "--checkpoint",
            "one",
            "--checkpoint",
            "two,one",
        ])
        .await;
    assert_eq!(v["checkpoints"], json!(["one", "two"]), "{v}");
    let v = inst.json(&["checkpoint", "list", "--from-git"]).await;
    assert_eq!(v["from"], json!("git"), "{v}");
    assert_eq!(v["checkpoints"].as_array().map(Vec::len), Some(2), "{v}");
    let v = inst
        .json(&[
            "checkpoint",
            "restore",
            "two",
            "--from-git",
            "--dry-run",
            "--yes",
        ])
        .await;
    assert_eq!(
        (v["from"].clone(), v["dry_run"].clone()),
        (json!("git"), json!(true)),
        "{v}"
    );
    inst.server.shutdown();
}

/// An Arazzo file is previewed, imported, run and exported back.
#[tokio::test]
async fn an_arazzo_file_is_imported_run_and_exported() {
    let inst = instance().await;
    inst.login().await;
    let file = inst.home.path().join("pets.arazzo.yaml");
    std::fs::write(
        &file,
        "arazzo: 1.0.1\ninfo: {title: Pets, version: '1'}\nsourceDescriptions: [{name: petstore, url: ./petstore.yaml, type: openapi}]\nworkflows:\n  - workflowId: pet-journey\n    steps:\n      - stepId: list\n        operationId: listPets\n        successCriteria: [{condition: $statusCode == 200}]\n        outputs: {total: $response.header.X-Total-Count}\n        onFailure: [{name: again, type: retry}]\n      - stepId: read\n        operationId: getPetById\n        parameters: [{name: petId, in: path, value: $steps.list.outputs.total}]\n        successCriteria: [{condition: $statusCode == 200}]\n",
    )
    .expect("file");
    let path = file.display().to_string();
    let preview = inst.json(&["tests", "import", &path, "--dry-run"]).await;
    assert_eq!(
        preview["tests"][0]["name"],
        json!("pet-journey"),
        "{preview}"
    );
    assert!(
        preview["left_out"]
            .to_string()
            .contains("onFailure[0] (retry)"),
        "{preview}"
    );
    assert_eq!(inst.json(&["tests", "list"]).await["tests"], json!([]));
    let (code, text) = inst.run(None, &["tests", "import", &path]).await;
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("pet-journey") && text.contains("onFailure"),
        "{text}"
    );
    let run = inst.json(&["tests", "run"]).await;
    assert_eq!(run["passed"], json!(true), "{run}");
    assert_eq!(
        run["results"][0]["steps"][0]["outputs"]["total"],
        json!("3"),
        "{run}"
    );
    let (code, hurl) = inst.run(None, &["tests", "export", "pet-journey"]).await;
    assert_eq!(code, 0);
    assert!(
        hurl.contains("list_total: header \"X-Total-Count\""),
        "{hurl}"
    );
    let (code, yaml) = inst
        .run(
            None,
            &["tests", "export", "pet-journey", "--format", "arazzo"],
        )
        .await;
    assert_eq!(code, 0, "{yaml}");
    assert!(
        yaml.contains("arazzo: 1.0.1") && yaml.contains("operationId: getPetById"),
        "{yaml}"
    );
    let (code, _) = inst
        .run(None, &["tests", "export", "--format", "arazzo"])
        .await;
    assert_eq!(code, 2);
    let j = inst.json(&["harness", "import", &path]).await;
    assert_eq!(j["format"], json!("arazzo"), "{j}");
    inst.server.shutdown();
}

/// A pack directory (its spec a file beside it) is applied, listed,
/// exported and torn down; a second apply is refused naming what exists.
#[tokio::test]
async fn a_pack_directory_is_applied_exported_and_torn_down() {
    let inst = instance().await;
    inst.login().await;
    let dir = inst.home.path().join("acme-pack");
    std::fs::create_dir_all(dir.join("specs")).expect("dir");
    std::fs::write(dir.join("specs/orders.yaml"), SPEC).expect("spec");
    std::fs::write(
        dir.join("rustybin.yaml"),
        "version: 2\npack: {name: acme, title: Acme checkout, customer: Acme Corp}\nmocks:\n  - {name: acme-orders, spec: specs/orders.yaml}\nstoryboards:\n  - name: acme-demo\n    steps: [{title: Welcome}]\n",
    )
    .expect("pack");
    let path = dir.display().to_string();
    let before = inst.run(None, &["config", "export"]).await.1;
    let preview = inst.json(&["pack", "apply", &path, "--dry-run"]).await;
    assert_eq!(
        preview["would_create"]["mocks"],
        json!(["acme-orders"]),
        "{preview}"
    );
    assert_eq!(inst.json(&["pack", "list"]).await["packs"], json!([]));
    let applied = inst.json(&["pack", "apply", &path]).await;
    assert_eq!(
        applied["created"]["storyboards"],
        json!(["acme-demo"]),
        "{applied}"
    );
    let (code, text) = inst.run(None, &["pack", "list"]).await;
    assert_eq!(code, 0);
    assert!(
        text.contains("acme") && text.contains("Acme Corp"),
        "{text}"
    );
    // Again: refused, with the names that are taken.
    let (code, out, message) = inst.run_full(None, &["pack", "apply", &path]).await;
    assert_eq!(code, 1, "{out}");
    assert!(message.contains("already exists"), "{message}");
    assert!(out.contains("acme-orders"), "{out}");
    // A spec outside the directory is not read.
    let escape = inst.home.path().join("escape");
    std::fs::create_dir_all(&escape).expect("dir");
    std::fs::write(
        escape.join("rustybin.yaml"),
        "pack: {name: escape}\nmocks:\n  - {name: x, spec: ../acme-pack/specs/orders.yaml}\n",
    )
    .expect("pack");
    let (code, _, message) = inst
        .run_full(None, &["pack", "apply", &escape.display().to_string()])
        .await;
    assert_eq!(code, 2, "{message}");
    // The export is a pack again, in one file.
    let out = inst.home.path().join("acme.yaml");
    let (code, _) = inst
        .run(
            None,
            &[
                "pack",
                "export",
                "acme",
                "--output",
                &out.display().to_string(),
            ],
        )
        .await;
    assert_eq!(code, 0);
    let exported = std::fs::read_to_string(&out).expect("export");
    assert!(
        exported.contains("customer: Acme Corp") && exported.contains("spec_inline"),
        "{exported}"
    );
    let would = inst.json(&["pack", "teardown", "acme", "--dry-run"]).await;
    assert_eq!(
        would["would_remove"]["mocks"],
        json!(["acme-orders"]),
        "{would}"
    );
    let gone = inst.json(&["pack", "teardown", "acme", "--yes"]).await;
    assert_eq!(gone["ok"], json!(true), "{gone}");
    assert_eq!(inst.run(None, &["config", "export"]).await.1, before);
    // The exported file applies.
    let again = inst
        .json(&["pack", "apply", &out.display().to_string()])
        .await;
    assert_eq!(again["created"]["mocks"], json!(["acme-orders"]), "{again}");
    inst.server.shutdown();
}

/// `storyboard run` is a rehearsal: every step's verdict, exit 1 when one
/// fails; `--step` runs one; a viewer is refused naming the permission.
#[tokio::test]
async fn a_storyboard_is_rehearsed_step_by_step() {
    let inst = instance().await;
    inst.login().await;
    let doc = r#"{"title":"A demo","steps":[
        {"title":"Say hello","talk_track":"Welcome."},
        {"title":"It answers","action":{"type":"send","request":{"path":"/status/200"},"assert":[{"status":200}]}},
        {"title":"Break it","action":{"type":"set_health","healthy":false},"reset":{"type":"set_health","healthy":true}},
        {"title":"A wrong guess","action":{"type":"send","request":{"path":"/health"},"assert":[{"status":200}]}}
    ]}"#;
    let saved = inst
        .json(&["storyboard", "apply", "demo", "--file", doc])
        .await;
    assert_eq!(saved["name"], json!("demo"));
    let list = inst.json(&["storyboard", "list"]).await;
    assert_eq!(list["storyboards"][0]["name"], json!("demo"));
    // One step.
    let one = inst
        .json(&["storyboard", "run", "demo", "--step", "2"])
        .await;
    assert_eq!(one["ok"], json!(true), "{one}");
    assert_eq!(one["results"][0]["action"], json!("send"));
    // The whole rehearsal: the last step fails, the others are reported.
    let (code, out, message) = inst
        .run_full(None, &["storyboard", "run", "demo", "--json"])
        .await;
    assert_eq!(code, 1, "{out} {message}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let oks: Vec<bool> = v["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|r| r["ok"] == json!(true))
        .collect();
    assert_eq!(oks, vec![true, true, true, false], "{out}");
    assert!(!inst.state.health.is_healthy());
    let (code, text, message) = inst.run_full(None, &["storyboard", "run", "demo"]).await;
    assert_eq!(code, 1);
    assert!(text.contains("A wrong guess"), "{text}");
    assert!(
        message.contains("step 4 (A wrong guess)") && message.contains("expected 200, saw 503"),
        "{message}"
    );
    // Where the presenter is, then the reset.
    let shown = inst.json(&["storyboard", "show", "demo"]).await;
    assert_eq!(shown["progress"]["done"], json!(true), "{shown}");
    let reset = inst.json(&["storyboard", "reset", "demo"]).await;
    assert_eq!(reset["ok"], json!(true), "{reset}");
    assert!(inst.state.health.is_healthy());
    let (code, _) = inst
        .run(None, &["storyboard", "run", "demo", "--step", "0"])
        .await;
    assert_eq!(code, 2);
    let gone = inst.json(&["storyboard", "delete", "demo", "--yes"]).await;
    assert_eq!(gone["deleted"], json!("demo"));
    inst.server.shutdown();
}

/// A command the credential may not run prints the permission it needs and
/// exits 3.
#[tokio::test]
async fn a_missing_permission_exits_3_and_names_it() {
    let inst = instance().await;
    let hash = crate::login::hash_password("vera-secret-1").expect("hash");
    inst.state
        .users
        .add(crate::rbac::NewUser::new("vera", &["viewer"]), Some(hash))
        .expect("user");
    let (code, out) = inst
        .run(
            Some("vera-secret-1\n"),
            &[
                "login",
                "--url",
                &inst.url,
                "--user",
                "vera",
                "--password-stdin",
            ],
        )
        .await;
    assert_eq!(code, 0, "{out}");
    let (code, _, message) = inst.run_full(None, &["mocks", "list"]).await;
    assert_eq!(code, 0, "{message}");
    let (code, _, message) = inst
        .run_full(None, &["mocks", "delete", "petstore", "--yes"])
        .await;
    assert_eq!(code, 3, "{message}");
    assert!(
        message.starts_with("permission_required: mocks.delete"),
        "{message}"
    );
    let (code, _, message) = inst.run_full(None, &["requests", "list"]).await;
    assert_eq!(code, 3, "{message}");
    assert!(
        message.starts_with("permission_required: traffic.read"),
        "{message}"
    );
    inst.server.shutdown();
}

#[tokio::test]
async fn mocks_deploy_can_name_the_revision_it_publishes() {
    let inst = instance().await;
    inst.login().await;
    let spec = inst.home.path().join("orders.json");
    std::fs::write(&spec, SPEC).expect("spec");
    let spec = spec.display().to_string();
    let v = inst
        .json(&["mocks", "create", "orders", "--spec", &spec, "--no-deploy"])
        .await;
    let rev = v["draft_revision"].as_u64().expect("draft_revision");
    // An older revision than the draft: refused, nothing deployed.
    let stale = (rev - 1).to_string();
    let (code, _, message) = inst
        .run_full(
            None,
            &["mocks", "deploy", "orders", "--expect-revision", &stale],
        )
        .await;
    assert_ne!(code, 0, "{message}");
    assert!(message.contains("409"), "{message}");
    let current = rev.to_string();
    let v = inst
        .json(&["mocks", "deploy", "orders", "--expect-revision", &current])
        .await;
    assert_eq!(v["revision"], rev, "{v}");
    inst.server.shutdown();
}

// ── Single sign-on ──────────────────────────────────────────────────

/// What a browser does with `rustybin login --sso`'s address: the
/// instance's start, the demo provider's sign-in page (as `user`), the
/// instance's callback, then the CLI's loopback address with the code.
async fn browse_sso(start: String, user: &'static str) {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");
    let next = |r: &reqwest::Response| {
        r.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let r = client.get(&start).send().await.expect("start");
    let authorize = next(&r);
    let callback = crate::sso::tests::approve_at_demo_provider(&authorize, user).await;
    let r = client.get(&callback).send().await.expect("callback");
    let loopback = next(&r);
    assert!(loopback.starts_with("http://127.0.0.1:"), "{loopback}");
    let _ = client.get(&loopback).send().await;
}

#[tokio::test]
async fn login_sso_takes_the_browser_back_on_a_loopback_address() {
    let idp_dir = tempfile::tempdir().expect("tempdir");
    let mut idp = Config::for_tests();
    idp.tls_cert = idp_dir.path().join("server.crt").display().to_string();
    idp.tls_key = idp_dir.path().join("server.key").display().to_string();
    let provider = crate::server::start_with_state(AppState::for_tests(idp))
        .await
        .expect("the provider starts");
    let inst = instance().await;
    let mut values = std::collections::BTreeMap::new();
    values.insert("sso.enabled".to_string(), json!(true));
    values.insert(
        "sso.issuer".to_string(),
        json!(format!("http://{}", provider.http_addr)),
    );
    values.insert(
        "sso.client_id".to_string(),
        json!(crate::oidc::PUBLIC_CLIENT_ID),
    );
    values.insert(
        "sso.scopes".to_string(),
        json!(["openid", "profile", "email", "groups"]),
    );
    values.insert("sso.role_map".to_string(), json!(["admins=admin"]));
    inst.state
        .settings
        .update(values, std::collections::BTreeMap::new(), "test")
        .expect("settings");

    let run = |user: &'static str| {
        let mut env = inst.env(None);
        env.browser = Some(Browser(Arc::new(move |url: &str| {
            tokio::spawn(browse_sso(url.to_string(), user));
        })));
        let url = inst.url.clone();
        async move {
            let buf = Buf::default();
            let mut out: Out = Box::new(buf.clone());
            let args: Vec<String> = ["login", "--sso", "--url", url.as_str(), "--wait", "20"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let r = execute_inner(&args, &env, &mut out).await;
            (r, buf.text())
        }
    };
    let (r, out) = run("alice").await;
    assert!(
        r.is_ok(),
        "{out} {:?}",
        r.err().map(|f| f.message().to_string())
    );
    assert!(out.contains("Signed in to"), "{out}");
    assert!(out.contains("as alice"), "{out}");
    let who = inst.json(&["whoami"]).await;
    assert_eq!(who["obtained"], json!("sso"), "{who}");
    assert_eq!(who["user"], json!("alice"));
    // bob's groups map to no role: the refusal comes back to the CLI.
    let (r, out) = run("bob").await;
    let f = r.expect_err("bob has no role");
    assert_eq!(f.code(), 3, "{out}");
    assert!(f.message().contains("no_role"), "{}", f.message());
    provider.shutdown();
    inst.server.shutdown();
}

/// Finding A7: the loopback listener answers only the callback carrying
/// its own `cli_state`: a page that finds the port and sends `?error=x`
/// first neither ends the sign-in nor wins over the real callback.
#[tokio::test]
async fn the_loopback_listener_waits_for_its_own_callback() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let waiting =
        tokio::spawn(
            async move { commands::loopback_answer(listener, "cli_state", "the-state").await },
        );
    let get = |target: &'static str| async move {
        let mut s = tokio::net::TcpStream::connect(addr).await.expect("connect");
        s.write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .await
            .expect("write");
        let mut out = String::new();
        let _ = s.read_to_string(&mut out).await;
        out
    };
    let forged = get("/callback?error=x&error_description=%1b%5b2J").await;
    assert!(forged.starts_with("HTTP/1.1 400"), "{forged}");
    let wrong = get("/callback?cli_state=guess&code=rbs_x").await;
    assert!(wrong.starts_with("HTTP/1.1 400"), "{wrong}");
    let real = get("/callback?cli_state=the-state&code=rbs_real").await;
    assert!(real.starts_with("HTTP/1.1 200"), "{real}");
    let query = waiting.await.expect("join").expect("query");
    assert!(query.contains("code=rbs_real"), "{query}");
}

#[tokio::test]
async fn a_password_is_hashed_offline_and_the_hash_signs_in() {
    let inst = instance().await;
    let (code, out) = inst
        .run(
            Some("a-long-enough-password\n"),
            &["password", "hash", "--stdin"],
        )
        .await;
    assert_eq!(code, 0, "{out}");
    let hash = out.trim();
    assert!(hash.starts_with("$argon2id$"), "{hash}");
    assert_eq!(hash.lines().count(), 1, "the hash and nothing else");
    assert!(!out.contains("a-long-enough-password"));
    assert!(crate::login::verify_hash(hash, "a-long-enough-password"));
    assert!(!crate::login::verify_hash(hash, "not-the-password"));
    let (code, _) = inst
        .run(Some("short\n"), &["password", "hash", "--stdin"])
        .await;
    assert_eq!(code, 2);
}

#[tokio::test]
async fn the_dry_check_exits_one_when_the_policy_would_refuse() {
    let inst = instance().await;
    inst.login().await;
    let (code, out) = inst
        .run(None, &["egress", "check", "http://169.254.169.254/"])
        .await;
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("always refused"), "{out}");
    let (code, _) = inst.run(None, &["egress", "check"]).await;
    assert_eq!(code, 2);
    let (code, _) = inst
        .run(
            None,
            &["egress", "check", "https://a.test/", "--purpose", "nope"],
        )
        .await;
    assert_eq!(code, 1);
    // A feature whose host is listed: allowed, and exit 0.
    inst.json(&["settings", "set", "egress.allow", r#"["a.test"]"#])
        .await;
    let (code, out) = inst
        .run(
            None,
            &["egress", "check", "https://a.test/", "--purpose", "git"],
        )
        .await;
    assert_eq!(code, 0, "{out}");
}

/// Core next 11: branches, the pull request way out and the signing key
/// from the command line.
#[tokio::test]
async fn git_branches_switch_resolve_and_signing_key() {
    if !std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("git is not installed: skipped");
        return;
    }
    let inst = instance().await;
    inst.login().await;
    let bare = inst.home.path().join("remote.git");
    let out = std::process::Command::new("git")
        .args(["init", "--quiet", "--bare", "-b", "main"])
        .arg(&bare)
        .output()
        .expect("git init --bare");
    assert!(out.status.success());
    let remote = format!("file://{}", bare.display());
    inst.json(&["settings", "set", "git.remote", &remote]).await;
    inst.json(&["git", "init"]).await;
    inst.json(&["git", "commit", "-m", "First"]).await;
    inst.json(&["git", "push"]).await;

    // A new branch from the instance: a preview, then the branch.
    let v = inst.json(&["git", "branch", "staging", "--dry-run"]).await;
    assert_eq!(v["would_create"], json!("staging"), "{v}");
    let v = inst
        .json(&["git", "branch", "staging", "-m", "Staging starts", "--yes"])
        .await;
    assert_eq!(v["created"], json!(true), "{v}");
    let (code, text) = inst.run(None, &["git", "branches"]).await;
    assert_eq!(code, 0);
    assert!(
        text.contains("  main ") && text.contains("* staging "),
        "{text}"
    );
    // Back to main with a preview first.
    let (code, text) = inst
        .run(None, &["git", "switch", "main", "--dry-run", "--prune"])
        .await;
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("would switch from staging to main"), "{text}");
    assert!(text.contains("would apply"), "{text}");
    let v = inst.json(&["git", "switch", "main", "--yes"]).await;
    assert_eq!(v["switched"], json!(true), "{v}");
    let v = inst.json(&["settings", "get", "git.branch"]).await;
    assert_eq!(v["value"], json!("main"));
    // A branch the remote lacks is exit 1, a missing name is usage.
    let (code, _) = inst.run(None, &["git", "switch", "nope", "--yes"]).await;
    assert_eq!(code, 1);
    let (code, _) = inst.run(None, &["git", "switch"]).await;
    assert_eq!(code, 2);

    // The instance's side to a branch for a pull request.
    inst.json(&["settings", "set", "console.title", "For review"])
        .await;
    let v = inst
        .json(&[
            "git",
            "resolve",
            "--with",
            "pull-request",
            "--branch",
            "review/title",
            "--dry-run",
        ])
        .await;
    assert_eq!(v["would_resolve"], json!("pull_request"), "{v}");
    let (code, text) = inst
        .run(
            None,
            &[
                "git",
                "resolve",
                "--with",
                "pull-request",
                "--branch",
                "review/title",
                "--yes",
            ],
        )
        .await;
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("open a pull request from review/title into main"),
        "{text}"
    );
    let (code, text) = inst
        .run(
            None,
            &["git", "resolve", "--with", "repository", "--dry-run"],
        )
        .await;
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("would take the remote's main"), "{text}");
    let (code, _) = inst.run(None, &["git", "resolve", "--dry-run"]).await;
    assert_eq!(code, 2);

    // The signing key: none, then generated; the private half never printed.
    let (code, text) = inst.run(None, &["git", "signing-key"]).await;
    assert_eq!(code, 0);
    assert!(text.contains("no signing key yet"), "{text}");
    if std::process::Command::new("ssh-keygen")
        .arg("-?")
        .output()
        .is_ok()
    {
        let (code, text) = inst.run(None, &["git", "signing-key", "--generate"]).await;
        assert_eq!(code, 0, "{text}");
        assert!(text.starts_with("ssh-ed25519 "), "{text}");
        assert!(text.contains("fingerprint: SHA256:"), "{text}");
        assert!(text.contains("not signed"), "{text}");
        let private = inst
            .state
            .settings
            .secret(crate::sourcecontrol::SIGNING_KEY_KEY)
            .expect("stored");
        assert!(!text.contains(private.as_str()));
        let v = inst.json(&["git", "signing-key"]).await;
        assert_eq!(v["signing"]["key_set"], json!(true));
        assert!(!v.to_string().contains(private.as_str()));

        // Core next step 19: the allowed signers. Off and empty (the
        // instance's own key is allowed without being listed), a signer
        // added and removed, verification on with its verified head.
        let (code, text) = inst.run(None, &["git", "signers"]).await;
        assert_eq!(code, 0, "{text}");
        assert!(text.contains("not verified"), "{text}");
        assert!(text.contains("this instance's signing key"), "{text}");
        let public = v["signing"]["public"].as_str().expect("public");
        let blob = public.split_whitespace().nth(1).expect("blob");
        let ann = format!("ann ssh-ed25519 {blob}");
        let (code, text) = inst.run(None, &["git", "signers", "--add", &ann]).await;
        assert_eq!(code, 0, "{text}");
        assert!(text.contains("ann  ssh-ed25519  SHA256:"), "{text}");
        let (code, text) = inst
            .run(None, &["git", "signers", "--add", "not a signer"])
            .await;
        assert_eq!(code, 1, "{text}");
        inst.json(&["settings", "set", "git.verify", "true"]).await;
        let (code, text) = inst.run(None, &["git", "signers"]).await;
        assert_eq!(code, 0, "{text}");
        assert!(text.contains("verified (git.verify is on)"), "{text}");
        assert!(text.contains("verified head: "), "{text}");
        let v = inst.json(&["git", "signers"]).await;
        assert_eq!(v["enabled"], json!(true), "{v}");
        assert_eq!(v["signers"][0]["name"], json!("ann"), "{v}");
        let fingerprint = v["signers"][0]["fingerprint"]
            .as_str()
            .expect("fingerprint")
            .to_string();
        let (code, text) = inst.run(None, &["git", "signers", "--remove", "bob"]).await;
        assert_eq!(code, 1, "{text}");
        let v = inst
            .json(&["git", "signers", "--remove", &fingerprint])
            .await;
        assert_eq!(v["signers"], json!([]), "{v}");
    }
    inst.server.shutdown();
}

/// Core next step 18: `harness import --var NAME=value` gives a value for a
/// variable the file does not define; a harness placeholder's name is
/// refused by name (exit 1), and a value is in no failure message.
#[tokio::test]
async fn harness_import_takes_variables() {
    let inst = instance().await;
    inst.login().await;
    let dir = inst.home.path().join("import-vars");
    std::fs::create_dir_all(&dir).expect("dir");
    let collection = dir.join("shop.postman_collection.json");
    std::fs::write(
        &collection,
        json!({
            "info": { "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json" },
            "item": [{ "name": "one", "request": { "method": "GET",
                "url": { "raw": "{{baseUrl}}/orders/:id?tenant={{tenant}}", "host": ["{{baseUrl}}"],
                    "path": ["orders", ":id"], "query": [{ "key": "tenant", "value": "{{tenant}}" }] } } }]
        })
        .to_string(),
    )
    .expect("collection");
    let collection = collection.display().to_string();
    // Without the values nothing imports.
    let (code, _, why) = inst
        .run_full(None, &["harness", "import", &collection])
        .await;
    assert_eq!(code, 1);
    assert!(why.contains("tenant") && why.contains(":id"), "{why}");
    let j = inst
        .json(&[
            "harness",
            "import",
            &collection,
            "--var",
            "baseUrl=https://api.example.com/v1",
            "--var",
            "tenant=a=b",
            "--var",
            ":id=42",
        ])
        .await;
    assert_eq!(j["format"], json!("postman"), "{j}");
    assert_eq!(
        j["source"]["requests"][0]["path"],
        json!("/v1/orders/42?tenant=a%3Db")
    );
    assert_eq!(j["variables"], json!([]));
    // A script is YAML: it travels as text, its format still named.
    let script = dir.join("load.yml");
    std::fs::write(
        &script,
        "config:\n  target: https://api.example.com\nscenarios:\n  - flow:\n      - get:\n          url: \"/items/{{ item }}\"\n",
    )
    .expect("script");
    let script = script.display().to_string();
    let j = inst
        .json(&[
            "harness",
            "import",
            &script,
            "--format",
            "artillery",
            "--var",
            "item=3",
        ])
        .await;
    assert_eq!(j["source"]["requests"][0]["path"], json!("/items/3"));
    // A placeholder of the harness is refused by name.
    let (code, _, why) = inst
        .run_full(
            None,
            &["harness", "import", &collection, "--var", "seq=v4lue-1"],
        )
        .await;
    assert_eq!(code, 1);
    assert!(why.contains("variables.seq"), "{why}");
    assert!(!why.contains("v4lue-1"), "{why}");
    // Not NAME=value: a usage error that does not repeat what was typed.
    let (code, _, why) = inst
        .run_full(
            None,
            &["harness", "import", &collection, "--var", "v4lue-2"],
        )
        .await;
    assert_eq!(code, 2, "{why}");
    assert!(
        why.contains("NAME=value") && !why.contains("v4lue-2"),
        "{why}"
    );
}

/// Core next step 13: `harness import` sends a script as YAML and names
/// the format with `--format`; a program is refused (exit 1).
#[tokio::test]
async fn harness_import_reads_a_script_and_refuses_a_program() {
    let inst = instance().await;
    inst.login().await;
    let dir = inst.home.path().join("import");
    std::fs::create_dir_all(&dir).expect("dir");
    let script = dir.join("load.yml");
    std::fs::write(
        &script,
        "config:\n  target: https://api.example.com\n  phases:\n    - duration: 10\n      arrivalRate: 2\nscenarios:\n  - flow:\n      - get:\n          url: /orders\n      - think: 1\n",
    )
    .expect("script");
    let script = script.display().to_string();
    let j = inst.json(&["harness", "import", &script]).await;
    assert_eq!(j["format"], json!("artillery"), "{j}");
    assert_eq!(j["source"]["requests"][0]["path"], json!("/orders"));
    assert!(j["left_out"].to_string().contains("think"), "{j}");
    let j = inst
        .json(&["harness", "import", &script, "--format", "artillery"])
        .await;
    assert_eq!(j["format"], json!("artillery"), "{j}");
    let (code, _) = inst
        .run(None, &["harness", "import", &script, "--format", "postman"])
        .await;
    assert_eq!(code, 1);
    let program = dir.join("load.js");
    std::fs::write(
        &program,
        "import http from 'k6/http';\nexport default function () { http.get('https://a.test'); }\n",
    )
    .expect("program");
    let (code, _, why) = inst
        .run_full(None, &["harness", "import", &program.display().to_string()])
        .await;
    assert_eq!(code, 1);
    assert!(why.contains("never run or translated"), "{why}");
}
