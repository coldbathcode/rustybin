//! `rustybin mock ...`: work with specs offline and manage mocks on a
//! running instance through the control API.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::checks;
use super::config_file::{self, MocksFile};
use super::model::{MockSettings, ResponseMode};
use super::respond::{self, BodyChoice};
use super::store::{MockLimits, MockStore};
use crate::config::Config;

/// `println!` that ignores a closed stdout (`rustybin mock list | head -1`).
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

/// `print!` that ignores a closed stdout.
macro_rules! say_raw {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let _ = write!(std::io::stdout(), $($arg)*);
    }};
}

pub const USAGE: &str = "\
rustybin mock: OpenAPI mocks from the command line

OFFLINE (no server needed):
    rustybin mock validate <spec>
        Parse a spec and run the deploy checks; exits 1 on errors.
    rustybin mock init <spec> [--name NAME] [--generated | --stateful] [--secure]
        Print a rustybin.yaml snippet for the spec.
    rustybin mock sample <spec> <operationId> [--status N] [--request] [--generated] [--seed N]
        Print a valid response (or request) for one operation.
    rustybin mock check-config [rustybin.yaml]
        Check a config file: every mock must load and compile.

REMOTE (control API of a running instance):
    rustybin mock push [rustybin.yaml] [--prune]
        Apply the file (spec files are read locally and inlined).
    rustybin mock list
    rustybin mock enable|disable|deploy|delete <name>
    rustybin mock export [name]
    rustybin mock duplicate <name> [--name NEW]
        Copy a mock (default name <name>-copy).
    rustybin mock reset-state <name>
        Forget the items a stateful mock stored.
    rustybin mock collection <name> <postman|hurl|curl|http|k6|bruno|insomnia> [--base-url URL]
        Print the mock's operations as a ready-to-run collection.

    --server URL   instance URL (default $RUSTYBIN_SERVER, else http://localhost:$RUSTYBIN_HTTP_PORT,
                   port 80 when unset: the server's own default, so `docker exec` works)
    --token TOKEN  admin token or control JWT (default $RUSTYBIN_TOKEN, then $RUSTYBIN_ADMIN_TOKEN)
    --json         print raw JSON responses";

/// Parsed flags: positional arguments plus `--flag [value]` pairs.
struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

const VALUE_FLAGS: &[&str] = &[
    "--name",
    "--status",
    "--seed",
    "--server",
    "--token",
    "--base-url",
];

impl Args {
    fn parse(raw: &[String]) -> Result<Self, String> {
        let mut positional = Vec::new();
        let mut flags = Vec::new();
        let mut it = raw.iter();
        while let Some(a) = it.next() {
            if let Some((k, v)) = a.split_once('=').filter(|(k, _)| k.starts_with("--")) {
                flags.push((k.to_string(), Some(v.to_string())));
            } else if VALUE_FLAGS.contains(&a.as_str()) {
                let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
                flags.push((a.clone(), Some(v.clone())));
            } else if a.starts_with("--") {
                flags.push((a.clone(), None));
            } else {
                positional.push(a.clone());
            }
        }
        Ok(Self { positional, flags })
    }

    fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|(k, _)| k == flag)
    }

    fn value(&self, flag: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(k, _)| k == flag)
            .and_then(|(_, v)| v.as_deref())
    }
}

fn limits() -> MockLimits {
    let (config, _) = Config::load();
    MockLimits::for_config(&config)
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
}

/// Run a `mock` subcommand (`args` excludes `rustybin mock`).
pub async fn run(args: &[String]) -> ExitCode {
    let Some(cmd) = args.first() else {
        say!("{USAGE}");
        return ExitCode::from(2);
    };
    let parsed = match Args::parse(&args[1..]) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("rustybin mock: {e}");
            return ExitCode::from(2);
        }
    };
    let result = match cmd.as_str() {
        "validate" => validate(&parsed),
        "init" => init(&parsed),
        "sample" => sample(&parsed),
        "check-config" => check_config(&parsed),
        "push" => push(&parsed).await,
        "list" => remote_list(&parsed).await,
        "enable" | "disable" | "deploy" => remote_action(&parsed, cmd).await,
        "delete" => remote_delete(&parsed).await,
        "export" => remote_export(&parsed).await,
        "duplicate" => remote_duplicate(&parsed).await,
        "reset-state" => remote_reset_state(&parsed).await,
        "collection" => remote_collection(&parsed).await,
        "--help" | "-h" | "help" => {
            say!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("rustybin mock: {e}");
            ExitCode::from(2)
        }
    }
}

fn spec_arg(args: &Args) -> Result<&str, String> {
    args.positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| "a spec file is required".to_string())
}

fn validate(args: &Args) -> Result<bool, String> {
    let path = spec_arg(args)?;
    let text = read(path)?;
    let store = MockStore::with_limits(limits());
    let c = store.compile(&text, MockSettings::default())?;
    let s = &c.spec;
    say!("{} {} (OpenAPI {})", s.title, s.version, s.openapi);
    say!(
        "{} operation(s), server prefix {:?}, suggested name {:?}",
        s.operations.len(),
        s.server_prefix,
        s.suggested_name()
    );
    for op in &c.report.operations {
        say!(
            "  {:<7} {:<40} {:<24} {}",
            op.method,
            op.path,
            op.id,
            op.status
        );
        for f in &op.findings {
            say!("          {:?}: {}", f.level, f.message);
        }
    }
    for w in &s.warnings {
        say!("warning: {w}");
    }
    for f in &c.report.findings {
        say!("{:?}: {}", f.level, f.message);
    }
    let cov = &c.report.coverage;
    say!(
        "examples: {} of {} operation(s) have one for their success responses{}",
        cov.with_examples,
        cov.operations,
        if cov.without.is_empty() {
            String::new()
        } else {
            format!(
                " (without: {})",
                cov.without
                    .iter()
                    .map(|o| o.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    );
    say!(
        "{} error(s), {} warning(s)",
        c.report.errors,
        c.report.warnings
    );
    Ok(c.report.ok)
}

fn init(args: &Args) -> Result<bool, String> {
    let path = spec_arg(args)?;
    let text = read(path)?;
    let store = MockStore::with_limits(limits());
    let c = store.compile(&text, MockSettings::default())?;
    let name = args
        .value("--name")
        .map(str::to_string)
        .unwrap_or_else(|| c.spec.suggested_name());
    let mode = if args.has("--stateful") {
        "stateful"
    } else if args.has("--generated") {
        "generated"
    } else {
        "examples"
    };
    let secure = args.has("--secure");
    let doc = json!({
        "version": 1,
        "mocks": [{
            "name": name,
            "spec": path,
            "enabled": true,
            "responses": { "mode": mode, "validate_requests": "warn" },
            "security": { "enabled": secure, "mode": "spec" },
        }]
    });
    let yaml = serde_yaml::to_string(&doc).map_err(|e| e.to_string())?;
    say!(
        "# rustybin.yaml for {} ({} operations)",
        c.spec.title,
        c.spec.operations.len()
    );
    say_raw!("{yaml}");
    Ok(true)
}

fn sample(args: &Args) -> Result<bool, String> {
    let path = spec_arg(args)?;
    let op_id = args
        .positional
        .get(1)
        .ok_or_else(|| "an operationId is required".to_string())?;
    let text = read(path)?;
    let mut settings = MockSettings::default();
    if let Some(seed) = args.value("--seed") {
        settings.responses.seed = seed.parse().map_err(|_| "--seed must be a number")?;
    }
    let store = MockStore::with_limits(limits());
    let c = store.compile(&text, settings)?;
    let Some(op) = c.spec.operation(op_id) else {
        let ids: Vec<&str> = c.spec.operations.iter().map(|o| o.id.as_str()).collect();
        return Err(format!(
            "no operation {op_id:?} (available: {})",
            ids.join(", ")
        ));
    };
    if args.has("--request") {
        let req = checks::sample_request(&c, op);
        say!(
            "{}",
            serde_json::to_string_pretty(&req).map_err(|e| e.to_string())?
        );
        return Ok(true);
    }
    let status = match args.value("--status") {
        Some(s) => s.parse().map_err(|_| "--status must be a number")?,
        None => op.default_status(),
    };
    let mode = if args.has("--generated") {
        ResponseMode::Generated
    } else {
        ResponseMode::Examples
    };
    let (_, body, source) = respond::render_body(
        &c,
        op,
        status,
        BodyChoice {
            body: None,
            example: None,
            mode,
        },
        None,
        &[],
    );
    eprintln!("# {} {} -> {status} ({source})", op.method, op.path);
    match body {
        Some(respond::Body::Json(v)) => {
            say!(
                "{}",
                serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?
            )
        }
        Some(respond::Body::Text(t)) => say!("{t}"),
        None => {}
    }
    Ok(true)
}

fn config_path(args: &Args) -> PathBuf {
    PathBuf::from(
        args.positional
            .first()
            .map(String::as_str)
            .unwrap_or("rustybin.yaml"),
    )
}

fn check_config(args: &Args) -> Result<bool, String> {
    let path = config_path(args);
    let limits = limits();
    let config_file::Loaded { defs, problems, .. } = config_file::load_file(
        &path,
        |k| std::env::var(k).ok(),
        limits.max_spec_bytes as u64,
    )?;
    let store = MockStore::with_limits(limits);
    let mut ok = problems.is_empty();
    for p in &problems {
        say!("error: {p}");
    }
    for d in defs {
        match store.compile(&d.source, d.settings) {
            Ok(c) => {
                say!(
                    "{}: {} ({} operations): {} error(s), {} warning(s)",
                    d.name,
                    c.spec.title,
                    c.spec.operations.len(),
                    c.report.errors,
                    c.report.warnings
                );
                ok &= c.report.ok;
            }
            Err(e) => {
                say!("{}: error: {e}", d.name);
                ok = false;
            }
        }
    }
    Ok(ok)
}

// ── Remote ──────────────────────────────────────────────────────────

struct Remote {
    client: reqwest::Client,
    server: String,
    token: Option<String>,
    raw: bool,
}

impl Remote {
    fn new(args: &Args) -> Result<Self, String> {
        let server = args
            .value("--server")
            .map(str::to_string)
            .or_else(|| std::env::var("RUSTYBIN_SERVER").ok())
            .unwrap_or_else(|| {
                let port = std::env::var("RUSTYBIN_HTTP_PORT")
                    .ok()
                    .and_then(|p| p.trim().parse::<u16>().ok())
                    .unwrap_or(80);
                format!("http://localhost:{port}")
            });
        let token = args
            .value("--token")
            .map(str::to_string)
            .or_else(|| std::env::var("RUSTYBIN_TOKEN").ok())
            .or_else(|| std::env::var("RUSTYBIN_ADMIN_TOKEN").ok())
            .filter(|t| !t.is_empty());
        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("rustybin-cli/", env!("CARGO_PKG_VERSION")));
        if let Ok(tls) = crate::a2a::push::tls_config() {
            builder = builder.use_preconfigured_tls(tls);
        }
        Ok(Self {
            client: builder.build().map_err(|e| e.to_string())?,
            server: server.trim_end_matches('/').to_string(),
            token,
            raw: args.has("--json"),
        })
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<(String, &str)>,
    ) -> Result<(u16, String), String> {
        let url = format!("{}{path}", self.server);
        let mut req = self.client.request(method, &url);
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        if let Some((b, ct)) = body {
            req = req.header("content-type", ct).body(b);
        }
        let resp = req.send().await.map_err(|e| format!("{url}: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        Ok((status, text))
    }

    fn fail(status: u16, text: &str) -> String {
        let msg = serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| text.chars().take(300).collect());
        format!("HTTP {status}: {msg}")
    }
}

/// Read a config file and inline its spec files.
fn inline_file(path: &Path) -> Result<Value, String> {
    let raw = read(&path.display().to_string())?;
    let (text, missing) = config_file::expand_env(&raw, |k| std::env::var(k).ok());
    for m in missing {
        eprintln!("warning: environment variable {m} is not set");
    }
    let mut file: MocksFile = config_file::parse(&text)?;
    let base = path.parent().unwrap_or(Path::new("."));
    for doc in &mut file.mocks {
        if let Some(spec) = doc.spec.take() {
            let p = Path::new(&spec);
            let full = if p.is_absolute() {
                p.to_path_buf()
            } else {
                base.join(p)
            };
            doc.spec_inline = Some(Value::String(read(&full.display().to_string())?));
        }
    }
    serde_json::to_value(&file).map_err(|e| e.to_string())
}

async fn push(args: &Args) -> Result<bool, String> {
    let doc = inline_file(&config_path(args))?;
    let remote = Remote::new(args)?;
    let path = if args.has("--prune") {
        "/_rustybin/mocks/apply?prune=true"
    } else {
        "/_rustybin/mocks/apply"
    };
    let (status, text) = remote
        .call(
            reqwest::Method::POST,
            path,
            Some((doc.to_string(), "application/json")),
        )
        .await?;
    if status != 200 {
        return Err(Remote::fail(status, &text));
    }
    if remote.raw {
        say!("{text}");
        return Ok(true);
    }
    let report: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    for key in ["created", "updated", "unchanged", "deleted"] {
        let names: Vec<&str> = report[key]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if !names.is_empty() {
            say!("{key}: {}", names.join(", "));
        }
    }
    let errors = report["errors"].as_array().cloned().unwrap_or_default();
    for e in &errors {
        say!(
            "error: {}: {}",
            e["name"].as_str().unwrap_or("?"),
            e["error"].as_str().unwrap_or("?")
        );
    }
    Ok(errors.is_empty())
}

async fn remote_list(args: &Args) -> Result<bool, String> {
    let remote = Remote::new(args)?;
    let (status, text) = remote
        .call(reqwest::Method::GET, "/_rustybin/mocks", None)
        .await?;
    if status != 200 {
        return Err(Remote::fail(status, &text));
    }
    if remote.raw {
        say!("{text}");
        return Ok(true);
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mocks = v["mocks"].as_array().cloned().unwrap_or_default();
    if mocks.is_empty() {
        say!("no mocks");
    }
    for m in mocks {
        let draft = if m["has_draft_changes"].as_bool() == Some(true) && m["state"] != "draft" {
            " (draft changes)"
        } else {
            ""
        };
        let secured = if m["security"]["enabled"].as_bool() == Some(true) {
            " secured"
        } else {
            ""
        };
        say!(
            "{:<24} {:<6} {:>4} ops {:>6} req  {}{secured}{draft}",
            m["name"].as_str().unwrap_or("?"),
            m["state"].as_str().unwrap_or("?"),
            m["operations"].as_u64().unwrap_or(0),
            m["requests_total"].as_u64().unwrap_or(0),
            m["url"].as_str().unwrap_or(""),
        );
    }
    for e in v["load_errors"].as_array().cloned().unwrap_or_default() {
        say!("load error: {}", e.as_str().unwrap_or("?"));
    }
    Ok(true)
}

fn name_arg(args: &Args) -> Result<&str, String> {
    args.positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| "a mock name is required".to_string())
}

async fn remote_action(args: &Args, action: &str) -> Result<bool, String> {
    let name = name_arg(args)?;
    let remote = Remote::new(args)?;
    let force = if action == "deploy" && args.has("--force") {
        "?force=true"
    } else {
        ""
    };
    let (status, text) = remote
        .call(
            reqwest::Method::POST,
            &format!("/_rustybin/mocks/{name}/{action}{force}"),
            None,
        )
        .await?;
    if remote.raw {
        say!("{text}");
    }
    if status != 200 {
        return Err(Remote::fail(status, &text));
    }
    if !remote.raw {
        let done = match action {
            "enable" => "enabled",
            "disable" => "disabled",
            _ => "deployed",
        };
        say!("{name}: {done}");
    }
    Ok(true)
}

async fn remote_delete(args: &Args) -> Result<bool, String> {
    let name = name_arg(args)?;
    let remote = Remote::new(args)?;
    let (status, text) = remote
        .call(
            reqwest::Method::DELETE,
            &format!("/_rustybin/mocks/{name}"),
            None,
        )
        .await?;
    if status != 200 {
        return Err(Remote::fail(status, &text));
    }
    say!("{name}: deleted");
    Ok(true)
}

async fn remote_export(args: &Args) -> Result<bool, String> {
    let remote = Remote::new(args)?;
    let path = match args.positional.first() {
        Some(name) => format!("/_rustybin/mocks/{name}/export"),
        None => "/_rustybin/mocks/export".to_string(),
    };
    let (status, text) = remote.call(reqwest::Method::GET, &path, None).await?;
    if status != 200 {
        return Err(Remote::fail(status, &text));
    }
    say_raw!("{text}");
    Ok(true)
}

async fn remote_duplicate(args: &Args) -> Result<bool, String> {
    let name = name_arg(args)?;
    let remote = Remote::new(args)?;
    let body = match args.value("--name") {
        Some(n) => json!({ "name": n }),
        None => json!({}),
    };
    let (status, text) = remote
        .call(
            reqwest::Method::POST,
            &format!("/_rustybin/mocks/{name}/duplicate"),
            Some((body.to_string(), "application/json")),
        )
        .await?;
    if status != 201 {
        return Err(Remote::fail(status, &text));
    }
    if remote.raw {
        say!("{text}");
        return Ok(true);
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    say!(
        "{name}: copied to {} ({})",
        v["name"].as_str().unwrap_or("?"),
        v["url"].as_str().unwrap_or("")
    );
    Ok(true)
}

async fn remote_reset_state(args: &Args) -> Result<bool, String> {
    let name = name_arg(args)?;
    let remote = Remote::new(args)?;
    let (status, text) = remote
        .call(
            reqwest::Method::DELETE,
            &format!("/_rustybin/mocks/{name}/state"),
            None,
        )
        .await?;
    if status != 200 {
        return Err(Remote::fail(status, &text));
    }
    say!("{name}: state reset");
    Ok(true)
}

async fn remote_collection(args: &Args) -> Result<bool, String> {
    let name = name_arg(args)?;
    let format = args
        .positional
        .get(1)
        .ok_or("a format is required: postman, hurl, curl, http, k6, bruno or insomnia")?;
    let remote = Remote::new(args)?;
    let mut path = format!("/_rustybin/mocks/{name}/collections/{format}");
    if let Some(base) = args.value("--base-url") {
        let q: String = form_urlencoded::Serializer::new(String::new())
            .append_pair("base_url", base)
            .finish();
        path = format!("{path}?{q}");
    }
    let (status, text) = remote.call(reqwest::Method::GET, &path, None).await?;
    if status != 200 {
        return Err(Remote::fail(status, &text));
    }
    say_raw!("{text}");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_parse() {
        let raw: Vec<String> = ["spec.yaml", "--name", "x", "--secure", "--status=404"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let a = Args::parse(&raw).expect("parsed");
        assert_eq!(a.positional, vec!["spec.yaml"]);
        assert_eq!(a.value("--name"), Some("x"));
        assert_eq!(a.value("--status"), Some("404"));
        assert!(a.has("--secure"));
        assert!(Args::parse(&["--name".to_string()]).is_err());
    }

    #[test]
    fn push_inlines_spec_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("s.yaml"), "openapi: 3.0.0\n").expect("write");
        let cfg = dir.path().join("rustybin.yaml");
        std::fs::write(&cfg, "mocks:\n  - name: a\n    spec: s.yaml\n").expect("write");
        let doc = inline_file(&cfg).expect("inlined");
        assert_eq!(doc["mocks"][0]["spec_inline"], "openapi: 3.0.0\n");
        assert!(doc["mocks"][0].get("spec").is_none());
    }
}
