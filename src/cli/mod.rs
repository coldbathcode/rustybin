//! The `rustybin` command line for remote instances (step 10 of
//! `docs/plans/next/master-plan.md`): `rustybin <noun> <verb> [args]
//! [--url U] [--profile P] [--token T] [--json] [--yes]`, nouns and verbs
//! that mirror the control API, tables for people and `--json` for scripts,
//! profiles for several instances (`profiles.rs`), a small client over the
//! control API (`client.rs`), and a parity test so a route never lands
//! without a command (`tests.rs`).
//!
//! The command tree is the table [`commands::COMMANDS`]: every command
//! names the routes it calls (the parity test walks the catalogue against
//! it), its flags (the parser, the help and the shell completion read them)
//! and an example (the generated reference `docs/src/cli.md`). The offline
//! families `rustybin mock ...` and `rustybin harness validate | run | report`
//! and the `mcp-stdio` bridge keep their own modules; `rustybin harness` with
//! `--remote` or a remote verb lands here.
//!
//! Exit codes: 0, 1 a refused or failed operation, 2 usage, 3 the network
//! or the credentials.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::Value;

pub mod client;
pub mod commands;
pub mod profiles;
#[cfg(test)]
mod tests;

use client::Client;
pub use commands::{Cmd, Flag, COMMANDS, NOUNS};

/// Where the output goes (stdout, or a buffer in tests).
pub type Out = Box<dyn Write + Send>;

/// What the process gives the command line: the environment variables it
/// reads, and in tests a canned stdin.
#[derive(Clone, Debug, Default)]
pub struct Env {
    /// `RUSTYBIN_CONFIG_HOME`: where `profiles.json` lives.
    pub config_home: Option<PathBuf>,
    /// `RUSTYBIN_URL` (also `RUSTYBIN_SERVER`, the mocks CLI's name).
    pub url: Option<String>,
    /// `RUSTYBIN_TOKEN`, else `RUSTYBIN_ADMIN_TOKEN`.
    pub token: Option<String>,
    /// `RUSTYBIN_HTTP_PORT`: the default URL's port (the server's own
    /// default, 80, so `docker exec` works).
    pub http_port: Option<u16>,
    /// `RUSTYBIN_MOCK_TOKEN`: a self-service manage token.
    pub mock_token: Option<String>,
    /// `HOME` (the default profiles directory).
    pub home: Option<PathBuf>,
    /// Canned stdin (tests); `None` reads the real one.
    pub stdin: Option<String>,
}

impl Env {
    pub fn from_process() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Self {
            config_home: var("RUSTYBIN_CONFIG_HOME").map(PathBuf::from),
            url: var("RUSTYBIN_URL").or_else(|| var("RUSTYBIN_SERVER")),
            token: var("RUSTYBIN_TOKEN").or_else(|| var("RUSTYBIN_ADMIN_TOKEN")),
            http_port: var("RUSTYBIN_HTTP_PORT").and_then(|p| p.trim().parse().ok()),
            mock_token: var("RUSTYBIN_MOCK_TOKEN"),
            home: var("HOME").map(PathBuf::from),
            stdin: None,
        }
    }
}

/// Why a command stopped, and the exit code it means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fail {
    /// Wrong arguments (2).
    Usage(String),
    /// The instance refused or the operation failed (1).
    Failed(String),
    /// Not signed in, or the credential was refused (3).
    Auth(String),
    /// The instance could not be reached (3).
    Network(String),
}

impl Fail {
    pub fn code(&self) -> u8 {
        match self {
            Fail::Usage(_) => 2,
            Fail::Failed(_) => 1,
            Fail::Auth(_) | Fail::Network(_) => 3,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Fail::Usage(m) | Fail::Failed(m) | Fail::Auth(m) | Fail::Network(m) => m,
        }
    }
}

pub type Result<T> = std::result::Result<T, Fail>;

/// The flags every command takes.
pub const GLOBAL_FLAGS: &[Flag] = &[
    Flag::value(
        "--url",
        "The instance URL (else RUSTYBIN_URL, the profile, http://localhost:80)",
    ),
    Flag::value(
        "--profile",
        "The profile to use (else RUSTYBIN_PROFILE, the default profile)",
    ),
    Flag::value(
        "--token",
        "A control token or the admin token (else RUSTYBIN_TOKEN, the profile)",
    ),
    Flag::value("--session", "The X-Rustybin-Session the calls carry"),
    Flag::value(
        "--mock-token",
        "A self-service manage token (else RUSTYBIN_MOCK_TOKEN)",
    ),
    Flag::switch("--json", "Print the instance's JSON answer as it is"),
    Flag::switch("--yes", "Skip the confirmation of a destructive verb"),
    Flag::switch("--quiet", "No progress or confirmation lines"),
    Flag::switch("--help", "This help"),
];

/// Parsed arguments: positionals and `--flag [value]` pairs.
#[derive(Clone, Debug, Default)]
pub struct Args {
    pub positional: Vec<String>,
    pub flags: Vec<(String, Option<String>)>,
}

impl Args {
    /// Parse against the command's flags plus the global ones. `--flag=v`
    /// and `--flag v` both work; `--` ends the flags.
    pub fn parse(raw: &[String], known: &[Flag]) -> Result<Self> {
        let find = |name: &str| {
            known
                .iter()
                .chain(GLOBAL_FLAGS)
                .find(|f| f.name == name)
                .copied()
        };
        let mut positional = Vec::new();
        let mut flags = Vec::new();
        let mut it = raw.iter();
        let mut only_positional = false;
        while let Some(a) = it.next() {
            if only_positional || !a.starts_with('-') || a == "-" {
                positional.push(a.clone());
                continue;
            }
            if a == "--" {
                only_positional = true;
                continue;
            }
            // A short flag (-m) only when a command declares it; anything
            // else with one dash is a value (a negative number).
            if !a.starts_with("--")
                && find(a.split_once('=').map(|(n, _)| n).unwrap_or(a)).is_none()
            {
                positional.push(a.clone());
                continue;
            }
            let (name, inline) = match a.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (a.clone(), None),
            };
            let Some(flag) = find(&name) else {
                return Err(Fail::Usage(format!("unknown flag {name}")));
            };
            if flag.value {
                let value = match inline {
                    Some(v) => v,
                    None => it
                        .next()
                        .cloned()
                        .ok_or_else(|| Fail::Usage(format!("{name} needs a value")))?,
                };
                flags.push((name, Some(value)));
            } else {
                if inline.is_some() {
                    return Err(Fail::Usage(format!("{name} takes no value")));
                }
                flags.push((name, None));
            }
        }
        Ok(Self { positional, flags })
    }

    pub fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(k, _)| k == name)
    }

    /// The last value of a flag.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.as_deref())
    }

    /// Every value of a repeatable flag.
    pub fn values(&self, name: &str) -> Vec<&str> {
        self.flags
            .iter()
            .filter(|(k, _)| k == name)
            .filter_map(|(_, v)| v.as_deref())
            .collect()
    }

    /// The positional argument at `i`, or a usage error naming it.
    pub fn arg(&self, i: usize, what: &str) -> Result<&str> {
        self.positional
            .get(i)
            .map(String::as_str)
            .ok_or_else(|| Fail::Usage(format!("{what} is required")))
    }
}

/// Everything a command runs with.
pub struct Ctx {
    pub cmd: &'static Cmd,
    pub args: Args,
    pub env: Env,
    pub out: Out,
    pub json: bool,
    pub yes: bool,
    pub quiet: bool,
    client: Option<Client>,
}

impl Ctx {
    /// The client for the instance (built once from the flags, the
    /// environment and the profile).
    pub fn client(&mut self) -> Result<&Client> {
        if self.client.is_none() {
            let resolved = profiles::resolve(&self.env, &self.args)?;
            let mut client = Client::new(&resolved.url, resolved.token.clone())?;
            client.session = self.args.value("--session").map(str::to_string);
            client.mock_token = self
                .args
                .value("--mock-token")
                .map(str::to_string)
                .or_else(|| self.env.mock_token.clone());
            self.client = Some(client);
        }
        match &self.client {
            Some(c) => Ok(c),
            None => Err(Fail::Network("no client".into())),
        }
    }

    /// Print an answer: the JSON as it is with `--json`, else rendered for
    /// people.
    pub fn emit(&mut self, v: &Value) {
        if self.json {
            let _ = writeln!(
                self.out,
                "{}",
                serde_json::to_string_pretty(v).unwrap_or_default()
            );
        } else {
            let _ = write!(self.out, "{}", render(v));
        }
    }

    /// Print text as it is (YAML, a collection, a public key).
    pub fn raw(&mut self, text: &str) {
        let _ = write!(self.out, "{text}");
        if !text.ends_with('\n') {
            let _ = writeln!(self.out);
        }
    }

    /// A line for people (not with `--quiet`, not with `--json`).
    pub fn say(&mut self, line: &str) {
        if !self.quiet && !self.json {
            let _ = writeln!(self.out, "{line}");
        }
    }

    /// Ask before a destructive verb (`--yes` skips it; a non-interactive
    /// stdin refuses).
    pub fn confirm(&mut self, what: &str) -> Result<()> {
        if self.yes {
            return Ok(());
        }
        let answer = match &self.env.stdin {
            Some(canned) => canned.lines().next().unwrap_or("").to_string(),
            None => {
                if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                    return Err(Fail::Usage(format!(
                        "{what}: add --yes to confirm (stdin is not a terminal)"
                    )));
                }
                eprint!("{what} [y/N] ");
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                line
            }
        };
        if matches!(answer.trim(), "y" | "Y" | "yes") {
            Ok(())
        } else {
            Err(Fail::Failed("cancelled".into()))
        }
    }

    /// The text of a file (`-` is stdin), or inline JSON when the argument
    /// starts with `{` or `[`.
    pub fn input(&self, path: &str) -> Result<String> {
        let trimmed = path.trim_start();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            return Ok(path.to_string());
        }
        if path == "-" {
            return self.stdin();
        }
        std::fs::read_to_string(path).map_err(|e| Fail::Usage(format!("{path}: {e}")))
    }

    /// Everything on stdin.
    pub fn stdin(&self) -> Result<String> {
        if let Some(s) = &self.env.stdin {
            return Ok(s.clone());
        }
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)
            .map_err(|e| Fail::Usage(format!("cannot read stdin: {e}")))?;
        Ok(s)
    }

    /// A secret: line `line` of a canned stdin, the next line of stdin
    /// with `from_stdin` (or when stdin is not a terminal), else prompted
    /// without echo.
    pub fn secret(&self, label: &str, from_stdin: bool, line: usize) -> Result<String> {
        if let Some(s) = &self.env.stdin {
            return Ok(s.lines().nth(line).unwrap_or("").to_string());
        }
        if from_stdin || !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map_err(|e| Fail::Usage(format!("cannot read stdin: {e}")))?;
            return Ok(line.trim_end_matches(['\r', '\n']).to_string());
        }
        prompt_secret(label)
    }

    /// A JSON value from a flag: inline JSON or a file path.
    pub fn json_flag(&self, name: &str) -> Result<Option<Value>> {
        match self.args.value(name) {
            None => Ok(None),
            Some(v) => {
                let text = self.input(v)?;
                serde_json::from_str(&text)
                    .map(Some)
                    .map_err(|e| Fail::Usage(format!("{name}: not JSON: {e}")))
            }
        }
    }
}

/// Read a line without echo (a terminal); the label goes to stderr.
fn prompt_secret(label: &str) -> Result<String> {
    eprint!("{label}: ");
    #[cfg(unix)]
    let restore = {
        // SAFETY: plain termios calls on the process's own stdin.
        let fd = libc::STDIN_FILENO;
        let mut term = std::mem::MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, term.as_mut_ptr()) } == 0 {
            let original = unsafe { term.assume_init() };
            let mut silent = original;
            silent.c_lflag &= !libc::ECHO;
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &silent) };
            Some(original)
        } else {
            None
        }
    };
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line);
    #[cfg(unix)]
    if let Some(original) = restore {
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &original) };
    }
    eprintln!();
    read.map_err(|e| Fail::Usage(format!("cannot read the terminal: {e}")))?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

// ── Running ─────────────────────────────────────────────────────────

/// `rustybin <noun> <verb> ...` from the process: stdout, the real
/// environment, the exit code.
pub async fn run(args: &[String]) -> ExitCode {
    let out: Out = Box::new(std::io::stdout());
    ExitCode::from(execute(args, &Env::from_process(), out).await)
}

/// Run a command line; the exit code. Usage errors and failures go to
/// stderr, answers to `out`.
pub async fn execute(args: &[String], env: &Env, mut out: Out) -> u8 {
    match execute_inner(args, env, &mut out).await {
        Ok(()) => 0,
        Err(f) => {
            eprintln!("rustybin: {}", f.message());
            f.code()
        }
    }
}

async fn execute_inner(args: &[String], env: &Env, out: &mut Out) -> Result<()> {
    let Some(first) = args.first().map(String::as_str) else {
        let _ = write!(out, "{}", usage());
        return Err(Fail::Usage("a command is required".into()));
    };
    match first {
        "help" | "--help" | "-h" => {
            let text = match args.get(1).map(String::as_str) {
                Some(noun) => {
                    help(noun).ok_or_else(|| Fail::Usage(format!("unknown command {noun}")))?
                }
                None => usage(),
            };
            let _ = write!(out, "{text}");
            return Ok(());
        }
        "completion" => {
            let shell = args.get(1).map(String::as_str).unwrap_or("bash");
            let text = completion(shell)
                .ok_or_else(|| Fail::Usage("completion takes bash, zsh or fish".into()))?;
            let _ = write!(out, "{text}");
            return Ok(());
        }
        _ => {}
    }
    let noun = first;
    if !NOUNS.iter().any(|(n, _)| *n == noun) {
        return Err(Fail::Usage(format!(
            "unknown command {noun} (rustybin help lists them)"
        )));
    }
    // A noun with a verb, or a noun that is its own verb (login, status).
    let (cmd, rest) = match args.get(1).map(String::as_str) {
        Some(verb) if !verb.starts_with("--") => match find(noun, verb) {
            Some(c) => (c, &args[2..]),
            None => match find(noun, "") {
                Some(c) => (c, &args[1..]),
                None => {
                    return Err(Fail::Usage(format!(
                        "unknown verb {verb} for {noun} (rustybin help {noun} lists them)"
                    )))
                }
            },
        },
        _ => match find(noun, "") {
            Some(c) => (c, &args[1..]),
            None => {
                let _ = write!(out, "{}", help(noun).unwrap_or_default());
                return Err(Fail::Usage(format!("{noun} needs a verb")));
            }
        },
    };
    let parsed = Args::parse(rest, cmd.flags)?;
    if parsed.has("--help") {
        let _ = write!(out, "{}", command_help(cmd));
        return Ok(());
    }
    let mut ctx = Ctx {
        cmd,
        json: parsed.has("--json"),
        yes: parsed.has("--yes"),
        quiet: parsed.has("--quiet"),
        args: parsed,
        env: env.clone(),
        out: std::mem::replace(out, Box::new(std::io::sink())),
        client: None,
    };
    let result = commands::dispatch(&mut ctx).await;
    *out = ctx.out;
    result
}

/// The command for a noun and verb.
pub fn find(noun: &str, verb: &str) -> Option<&'static Cmd> {
    COMMANDS.iter().find(|c| c.noun == noun && c.verb == verb)
}

// ── Help ────────────────────────────────────────────────────────────

/// The top-level help: every noun with its summary.
pub fn usage() -> String {
    let mut s = String::from(
        "rustybin: the command line of a Rustybin instance\n\n\
         USAGE:\n    rustybin <noun> <verb> [args] [--url U] [--profile P] [--token T] [--json] [--yes]\n\n\
         Sign in once (rustybin login --url https://host), then every noun works against\n\
         that profile. rustybin help <noun> lists its verbs; --help on any command shows\n\
         its flags.\n\nNOUNS:\n",
    );
    let width = NOUNS.iter().map(|(n, _)| n.len()).max().unwrap_or(8);
    for (noun, summary) in NOUNS {
        s.push_str(&format!("    {noun:<width$}  {summary}\n"));
    }
    s.push_str(
        "\nOFFLINE AND OTHER FAMILIES:\n    \
         rustybin mock ...        OpenAPI specs offline (rustybin mock --help)\n    \
         rustybin harness ...     validate, run or render a load test from this process\n    \
         rustybin mcp-stdio ...   bridge a stdio MCP client to a management server\n    \
         rustybin completion <bash|zsh|fish>\n\nGLOBAL FLAGS:\n",
    );
    for f in GLOBAL_FLAGS {
        s.push_str(&format!("    {:<14} {}\n", f.name, f.help));
    }
    s.push_str(
        "\nEXIT CODES: 0 done, 1 refused or failed, 2 usage, 3 the network or the credentials.\n",
    );
    s
}

/// The help of one noun: its verbs.
pub fn help(noun: &str) -> Option<String> {
    let (_, summary) = NOUNS.iter().find(|(n, _)| *n == noun)?;
    let cmds: Vec<&Cmd> = COMMANDS.iter().filter(|c| c.noun == noun).collect();
    let mut s = format!("rustybin {noun}: {summary}\n\n");
    for c in &cmds {
        s.push_str(&format!("    {}\n        {}\n", c.line(), c.summary));
    }
    s.push_str("\nrustybin ");
    s.push_str(noun);
    s.push_str(" <verb> --help shows a verb's flags; rustybin help for the global flags.\n");
    Some(s)
}

/// The help of one command: usage, summary, flags, example.
pub fn command_help(cmd: &Cmd) -> String {
    let mut s = format!("{}\n    {}\n", cmd.line(), cmd.summary);
    if !cmd.flags.is_empty() {
        s.push_str("\nFLAGS:\n");
        for f in cmd.flags {
            s.push_str(&format!(
                "    {:<22} {}\n",
                if f.value {
                    format!("{} <value>", f.name)
                } else {
                    f.name.to_string()
                },
                f.help
            ));
        }
    }
    if !cmd.example.is_empty() {
        s.push_str(&format!("\nEXAMPLE:\n    rustybin {}\n", cmd.example));
    }
    s.push_str(&format!(
        "\nCalls: {}\n",
        cmd.routes
            .iter()
            .map(|(m, p)| format!("{m} {p}"))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    s
}

/// The reference the book carries (`docs/src/cli.md`), generated from the
/// command tree.
pub fn reference_markdown() -> String {
    let mut s = String::from(
        "# CLI reference\n\n\
         Generated from the command tree (`RUSTYBIN_UPDATE_CLI_DOCS=1 cargo test cli_docs`); \
         the [CLI guide](cli-guide.md) says how to sign in, use profiles and script it, and \
         the [mocks CLI](mocks/cli.md) covers the offline `rustybin mock` commands.\n\n\
         `rustybin <noun> <verb> [args] [flags]`. Every command takes the global flags; \
         `--json` prints the instance's answer as it is, `--yes` skips a confirmation. \
         Exit codes: `0` done, `1` refused or failed, `2` usage, `3` the network or the \
         credentials.\n\n| Flag | Meaning |\n|---|---|\n",
    );
    for f in GLOBAL_FLAGS {
        s.push_str(&format!("| `{}` | {} |\n", f.name, f.help));
    }
    for (noun, summary) in NOUNS {
        s.push_str(&format!(
            "\n## {noun}\n\n{summary}\n\n| Command | What it does | Calls |\n|---|---|---|\n"
        ));
        for c in COMMANDS.iter().filter(|c| c.noun == *noun) {
            let routes = c
                .routes
                .iter()
                .map(|(m, p)| format!("`{m} {p}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let flags = if c.flags.is_empty() {
                String::new()
            } else {
                format!(
                    " Flags: {}.",
                    c.flags
                        .iter()
                        .map(|f| format!("`{}`{}", f.name, if f.value { " value" } else { "" }))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            s.push_str(&format!(
                "| `rustybin {}` | {}{} | {} |\n",
                c.line().replace('|', "\\|"),
                c.summary.replace('|', "\\|"),
                flags.replace('|', "\\|"),
                routes
            ));
        }
        let examples: Vec<&Cmd> = COMMANDS
            .iter()
            .filter(|c| c.noun == *noun && !c.example.is_empty())
            .collect();
        if !examples.is_empty() {
            s.push_str("\n```bash\n");
            for c in examples {
                s.push_str(&format!("rustybin {}\n", c.example));
            }
            s.push_str("```\n");
        }
    }
    s.push_str("\n## Not covered by a command\n\nThe management MCP endpoint (`rustybin mcp-stdio` bridges a client to it), the agent sign-in (OAuth) routes a browser walks, the harness cluster's node-to-node routes, a request bin's capture URLs and a mock's data plane.\n");
    s
}

/// A shell completion script.
pub fn completion(shell: &str) -> Option<String> {
    let nouns: Vec<&str> = NOUNS.iter().map(|(n, _)| *n).collect();
    let verbs_of = |noun: &str| -> Vec<&str> {
        COMMANDS
            .iter()
            .filter(|c| c.noun == noun && !c.verb.is_empty())
            .map(|c| c.verb)
            .collect()
    };
    let mut s = String::new();
    match shell {
        "bash" => {
            s.push_str("# rustybin bash completion: source <(rustybin completion bash)\n_rustybin() {\n  local cur=\"${COMP_WORDS[COMP_CWORD]}\"\n  if [[ $COMP_CWORD -eq 1 ]]; then\n");
            s.push_str(&format!(
                "    COMPREPLY=($(compgen -W \"{} mock harness mcp-stdio help completion\" -- \"$cur\"))\n    return\n  fi\n  case \"${{COMP_WORDS[1]}}\" in\n",
                nouns.join(" ")
            ));
            for n in &nouns {
                let verbs = verbs_of(n);
                if !verbs.is_empty() {
                    s.push_str(&format!(
                        "    {n}) [[ $COMP_CWORD -eq 2 ]] && COMPREPLY=($(compgen -W \"{}\" -- \"$cur\")) ;;\n",
                        verbs.join(" ")
                    ));
                }
            }
            s.push_str("  esac\n}\ncomplete -F _rustybin rustybin\n");
        }
        "zsh" => {
            s.push_str("#compdef rustybin\n# rustybin zsh completion: source <(rustybin completion zsh)\n_rustybin() {\n  local -a nouns verbs\n");
            s.push_str(&format!(
                "  nouns=({} mock harness mcp-stdio help completion)\n  if (( CURRENT == 2 )); then\n    compadd -- $nouns\n    return\n  fi\n  case \"$words[2]\" in\n",
                nouns.join(" ")
            ));
            for n in &nouns {
                let verbs = verbs_of(n);
                if !verbs.is_empty() {
                    s.push_str(&format!("    {n}) verbs=({}) ;;\n", verbs.join(" ")));
                }
            }
            s.push_str("  esac\n  (( CURRENT == 3 )) && compadd -- $verbs\n}\ncompdef _rustybin rustybin\n");
        }
        "fish" => {
            s.push_str("# rustybin fish completion: rustybin completion fish > ~/.config/fish/completions/rustybin.fish\n");
            for (n, summary) in NOUNS {
                s.push_str(&format!(
                    "complete -c rustybin -n '__fish_use_subcommand' -a {n} -d '{}'\n",
                    summary.replace('\'', "")
                ));
                for c in COMMANDS
                    .iter()
                    .filter(|c| c.noun == *n && !c.verb.is_empty())
                {
                    s.push_str(&format!(
                        "complete -c rustybin -n '__fish_seen_subcommand_from {n}' -a {} -d '{}'\n",
                        c.verb,
                        c.summary.replace('\'', "")
                    ));
                }
            }
        }
        _ => return None,
    }
    Some(s)
}

// ── Rendering for people ────────────────────────────────────────────

/// A JSON answer as text: objects as `key: value` lines (nested objects
/// indented), lists of objects as tables, lists of scalars joined.
pub fn render(v: &Value) -> String {
    let mut s = String::new();
    render_into(&mut s, v, 0);
    s
}

fn render_into(s: &mut String, v: &Value, indent: usize) {
    let pad = " ".repeat(indent);
    match v {
        Value::Object(m) => {
            if m.is_empty() {
                s.push_str(&format!("{pad}(empty)\n"));
            }
            for (k, v) in m {
                match v {
                    Value::Object(inner) if !inner.is_empty() => {
                        s.push_str(&format!("{pad}{k}:\n"));
                        render_into(s, v, indent + 2);
                    }
                    Value::Array(items) if items.iter().any(Value::is_object) => {
                        s.push_str(&format!("{pad}{k}:\n"));
                        s.push_str(&table(items, indent + 2));
                    }
                    Value::Array(items) if !items.is_empty() => {
                        s.push_str(&format!(
                            "{pad}{k}: {}\n",
                            items.iter().map(scalar).collect::<Vec<_>>().join(", ")
                        ));
                    }
                    other => s.push_str(&format!("{pad}{k}: {}\n", scalar(other))),
                }
            }
        }
        Value::Array(items) if items.iter().any(Value::is_object) => {
            s.push_str(&table(items, indent))
        }
        Value::Array(items) => {
            for i in items {
                s.push_str(&format!("{pad}{}\n", scalar(i)));
            }
        }
        other => s.push_str(&format!("{pad}{}\n", scalar(other))),
    }
}

fn scalar(v: &Value) -> String {
    match v {
        Value::Null => "none".into(),
        Value::String(s) => s.clone(),
        Value::Array(a) if a.is_empty() => "(none)".into(),
        Value::Object(m) if m.is_empty() => "(empty)".into(),
        other => other.to_string(),
    }
}

/// A list of objects as an aligned table over the scalar keys (at most 8).
fn table(items: &[Value], indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut columns: Vec<String> = Vec::new();
    for item in items {
        if let Value::Object(m) = item {
            for (k, v) in m {
                if !v.is_object() && !v.is_array() && !columns.contains(k) && columns.len() < 8 {
                    columns.push(k.clone());
                }
            }
        }
    }
    if columns.is_empty() {
        let mut s = String::new();
        for item in items {
            render_into(&mut s, item, indent);
            s.push('\n');
        }
        return s;
    }
    let cell = |item: &Value, k: &str| -> String {
        let text = item.get(k).map(scalar).unwrap_or_default();
        if text.chars().count() > 40 {
            format!("{}...", text.chars().take(37).collect::<String>())
        } else {
            text
        }
    };
    let widths: Vec<usize> = columns
        .iter()
        .map(|k| {
            items
                .iter()
                .map(|i| cell(i, k).chars().count())
                .chain(std::iter::once(k.len()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut s = String::new();
    let line = |cells: Vec<String>| {
        let mut l = pad.clone();
        for (i, c) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                l.push_str(c);
            } else {
                l.push_str(&format!("{:<w$}  ", c, w = widths[i]));
            }
        }
        l.trim_end().to_string() + "\n"
    };
    s.push_str(&line(columns.clone()));
    for item in items {
        s.push_str(&line(columns.iter().map(|k| cell(item, k)).collect()));
    }
    s
}
