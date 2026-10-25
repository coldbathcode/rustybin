//! `rustybin harness ...`: run a scenario from one process, validate a
//! scenario offline, or render a saved report.
//!
//! `run` builds a client harness from the `RUSTYBIN_HARNESS*` environment
//! (targets, caps) plus flags. With `--local-server` it also starts an
//! in-process Rustybin and loops the load through it (the smallest demo and
//! the way to try a scenario without a gateway). Exit codes follow the
//! report: 0 when the thresholds pass, 1 when they fail, 2 on an error.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::report::Report;
use super::scenario::{parse_duration, Caps, Scenario, Target};
use super::{Harness, Role};
use crate::config::Config;

/// `println!` that ignores a closed stdout.
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

pub const USAGE: &str = "\
rustybin harness: load through a gateway from the command line

    rustybin harness validate <scenario.yaml> [--json]
        Parse a scenario and check it against the caps (RUSTYBIN_HARNESS_MAX_*); exits 1 on problems.
    rustybin harness run <scenario.yaml> [options]
        Run the scenario from this process and print the report. Exit code: 0 when the
        thresholds pass, 1 when they fail, 2 on an error.
    rustybin harness report <report.json> [--format text|junit|json]
        Render a saved report; exits with the report's exit code.

OPTIONS of run:
    --target URL        send to this URL instead of the scenario's targets
    --targets LIST      allow these hosts, domains, IPs or CIDRs (comma separated),
                        on top of RUSTYBIN_HARNESS_TARGETS
    --local-server      start an in-process Rustybin and loop the load through it
                        (the target unless --target is given; allowed automatically)
    --local-port N      port of the local server (default: an ephemeral port)
    --report FILE       write the JSON report to FILE (- for stdout)
    --format FMT        what to print: text (default), json or junit
    --max-rate N, --max-inflight N, --max-duration D, --threads N
                        override the node caps and the engine threads
    --verify            require the targets to serve the verification token
    --quiet             no progress lines while the run is on

The scenario format: GET /_rustybin/harness/schema on any instance, or
docs/plans/harness.md.";

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

const VALUE_FLAGS: &[&str] = &[
    "--target",
    "--targets",
    "--local-port",
    "--report",
    "--format",
    "--max-rate",
    "--max-inflight",
    "--max-duration",
    "--threads",
];

impl Args {
    fn parse(raw: &[String]) -> Result<Self, String> {
        let mut positional = Vec::new();
        let mut flags = Vec::new();
        let mut it = raw.iter();
        while let Some(a) = it.next() {
            if let Some(flag) = a.strip_prefix("--") {
                let flag = format!("--{flag}");
                if VALUE_FLAGS.contains(&flag.as_str()) {
                    let value = it
                        .next()
                        .cloned()
                        .ok_or_else(|| format!("{flag} needs a value"))?;
                    flags.push((flag, Some(value)));
                } else {
                    flags.push((flag, None));
                }
            } else {
                positional.push(a.clone());
            }
        }
        Ok(Self { positional, flags })
    }

    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|(f, _)| f == name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(f, _)| f == name)
            .and_then(|(_, v)| v.as_deref())
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("rustybin harness: {message}");
    ExitCode::from(2)
}

fn read_text(path: &str) -> Result<String, String> {
    if path == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)
            .map_err(|e| format!("cannot read stdin: {e}"))?;
        return Ok(s);
    }
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))
}

/// Apply `--max-*` and `--threads` to the harness settings.
fn apply_caps(config: &mut Config, args: &Args) -> Result<(), String> {
    let h = &mut config.harness;
    if let Some(v) = args.value("--max-rate") {
        h.max_rate = v
            .parse::<u64>()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or("--max-rate needs a positive number")?;
    }
    if let Some(v) = args.value("--max-inflight") {
        h.max_inflight = v
            .parse::<usize>()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or("--max-inflight needs a positive number")?;
    }
    if let Some(v) = args.value("--max-duration") {
        h.max_duration = parse_duration(v)
            .filter(|d| {
                d.as_secs() >= 1 && d.as_secs() <= crate::config::MAX_HARNESS_MAX_DURATION_SECS
            })
            .ok_or("--max-duration needs a duration from 1s to 24h (e.g. 30m)")?;
    }
    if let Some(v) = args.value("--threads") {
        h.threads = Some(
            v.parse::<usize>()
                .ok()
                .filter(|n| (1..=1024).contains(n))
                .ok_or("--threads needs a number from 1 to 1024")?,
        );
    }
    Ok(())
}

pub async fn run(args: &[String]) -> ExitCode {
    let Some(command) = args.first().map(String::as_str) else {
        say!("{USAGE}");
        return ExitCode::from(2);
    };
    let parsed = match Args::parse(&args[1..]) {
        Ok(a) => a,
        Err(e) => return fail(&e),
    };
    match command {
        "--help" | "-h" | "help" => {
            say!("{USAGE}");
            ExitCode::SUCCESS
        }
        "validate" => validate(&parsed),
        "run" => run_scenario(&parsed).await,
        "report" => report(&parsed),
        other => fail(&format!("unknown command {other:?}\n\n{USAGE}")),
    }
}

fn load_scenario(args: &Args) -> Result<Scenario, String> {
    let path = args
        .positional
        .first()
        .ok_or("a scenario file is required (or - for stdin)")?;
    let text = read_text(path)?;
    let mut scenario = Scenario::parse(&text)?;
    scenario.normalise();
    Ok(scenario)
}

fn validate(args: &Args) -> ExitCode {
    let (mut config, _) = Config::load();
    if let Err(e) = apply_caps(&mut config, args) {
        return fail(&e);
    }
    let scenario = match load_scenario(args) {
        Ok(s) => s,
        Err(e) => return fail(&e),
    };
    let caps = Caps::from_config(&config.harness);
    let problems = scenario.validate(&caps);
    if args.flag("--json") {
        say!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "valid": problems.is_empty(),
                "problems": problems,
                "scenario": scenario,
                "hash": scenario.hash(),
                "summary": scenario.summary(),
                "caps": caps,
            }))
            .unwrap_or_default()
        );
    } else {
        let s = scenario.summary();
        say!(
            "{}: {} layer(s), {} target(s), {:.0} s, peak {:.0} requests/s, about {:.0} requests",
            scenario.name,
            s.layers.len(),
            s.targets,
            s.duration_secs,
            s.peak_rate,
            s.expected_requests
        );
        if problems.is_empty() {
            say!("valid");
        } else {
            for p in &problems {
                say!("problem: {p}");
            }
        }
    }
    if problems.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn report(args: &Args) -> ExitCode {
    let Some(path) = args.positional.first() else {
        return fail("a report file is required");
    };
    let text = match read_text(path) {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };
    let report = match Report::from_json(&text) {
        Ok(r) => r,
        Err(e) => return fail(&e),
    };
    print_report(&report, args.value("--format").unwrap_or("text"));
    ExitCode::from(report.exit_code)
}

fn print_report(report: &Report, format: &str) {
    match format {
        "json" => say!(
            "{}",
            serde_json::to_string_pretty(report).unwrap_or_default()
        ),
        "junit" | "xml" => say!("{}", report.to_junit().trim_end()),
        _ => say!("{}", report.to_text().trim_end()),
    }
}

async fn run_scenario(args: &Args) -> ExitCode {
    let (mut config, warnings) = Config::load();
    for w in warnings.iter().filter(|w| w.contains("HARNESS")) {
        eprintln!("warning: {w}");
    }
    if let Err(e) = apply_caps(&mut config, args) {
        return fail(&e);
    }
    let format = args.value("--format").unwrap_or("text").to_string();
    if !matches!(format.as_str(), "text" | "json" | "junit" | "xml") {
        return fail("--format is text, json or junit");
    }
    let mut scenario = match load_scenario(args) {
        Ok(s) => s,
        Err(e) => return fail(&e),
    };
    let quiet = args.flag("--quiet");
    let local = args.flag("--local-server");
    let local_port: u16 = match args.value("--local-port") {
        None => 0,
        Some(v) => match v.parse() {
            Ok(p) => p,
            Err(_) => return fail("--local-port needs a port number"),
        },
    };

    let h = &mut config.harness;
    h.enabled = true;
    h.roles = if local {
        vec![Role::Client, Role::Server]
    } else {
        vec![Role::Client]
    };
    if let Some(list) = args.value("--targets") {
        h.targets.extend(
            list.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        );
    }
    if local {
        h.targets.push("127.0.0.1".into());
        h.targets.push("localhost".into());
    }
    if args.flag("--verify") {
        h.verify_targets = true;
    }
    // A client-only CLI process never serves the data plane.
    config.public_mode = false;
    config.self_service.enabled = false;

    // The local server (loop-through): the same harness serves the
    // verification token and takes the load.
    let mut server = None;
    let harness: Arc<Harness> = if local {
        config.http_port = local_port;
        config.https_port = 0;
        config.grpc_port = 0;
        config.host = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
        config.mocks_sample = true;
        let state = crate::state::AppState::new(config.clone());
        let running = match crate::server::start_with_state(state.clone()).await {
            Ok(s) => s,
            Err(e) => return fail(&format!("cannot start the local server: {e}")),
        };
        let addr = running.http_addr;
        if args.value("--target").is_none() {
            scenario.target = None;
            scenario.targets = vec![Target::new(format!("http://{addr}"))];
        }
        if !quiet {
            eprintln!("local server on http://{addr} (loop-through)");
        }
        server = Some(running);
        match state.harness.clone() {
            Some(h) => h,
            None => return fail("the local server has no harness"),
        }
    } else {
        if let Err(e) = super::check_startup(&config) {
            return fail(&e);
        }
        Harness::from_harness_config(&config.harness, &config.instance_id)
    };
    if let Some(url) = args.value("--target") {
        scenario.target = None;
        scenario.targets = vec![Target::new(url.to_string())];
    }

    let run = match harness.create_run(scenario).await {
        Ok(r) => r,
        Err(e) => {
            let mut lines = vec![e.error.clone()];
            lines.extend(e.problems.iter().map(|p| format!("  {p}")));
            if let Some(s) = server {
                s.shutdown();
                let _ = s.wait().await;
            }
            return fail(&lines.join("\n"));
        }
    };
    if !quiet {
        eprintln!(
            "run {} started: {} for {:.0} s, peak {:.0} requests/s",
            run.id,
            run.scenario.name,
            run.scenario.duration().as_secs_f64(),
            run.scenario.peak_rate()
        );
    }
    let progress = {
        let run = run.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            tick.tick().await;
            loop {
                tick.tick().await;
                if run.report().is_some() {
                    break;
                }
                if !quiet {
                    let s = run.snapshot();
                    eprintln!(
                        "{:>6.1} s | sent {:>8} | done {:>8} | ok {:>8} | errors {:>6} | not sent {:>6} | p99 {:>8.2} ms{}",
                        s["elapsed_secs"].as_f64().unwrap_or(0.0),
                        s["sent"],
                        s["completed"],
                        s["ok"],
                        s["errors"],
                        s["not_sent"],
                        s["latency"]["p99_ms"].as_f64().unwrap_or(0.0),
                        if s["saturated"].as_bool() == Some(true) {
                            " | SATURATED"
                        } else {
                            ""
                        }
                    );
                }
            }
        })
    };
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::select! {
        _ = run.wait() => {}
        _ = ctrl_c => {
            eprintln!("stopping the run");
            run.stop("user");
            run.wait().await;
        }
    }
    progress.abort();
    let Some(report) = run.report() else {
        return fail("the run ended without a report");
    };
    if let Some(path) = args.value("--report") {
        let text = serde_json::to_string_pretty(report).unwrap_or_default();
        if path == "-" {
            say!("{text}");
        } else if let Err(e) = std::fs::write(path, text) {
            eprintln!("cannot write {path}: {e}");
        } else if !quiet {
            eprintln!("report written to {path}");
        }
    }
    if args.value("--report") != Some("-") {
        print_report(report, &format);
    }
    if let Some(s) = server {
        s.shutdown();
        let _ = s.wait().await;
    }
    ExitCode::from(report.exit_code)
}
