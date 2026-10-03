use std::process::ExitCode;

use rustybin::{catalog, Config};

const USAGE: &str = "\
rustybin: HTTP stub service for API and AI gateway demos

USAGE:
    rustybin [--print-endpoints-markdown | --version | --help]

Configuration is read from RUSTYBIN_* environment variables (see README.md).";

#[tokio::main]
async fn main() -> ExitCode {
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "--print-endpoints-markdown" => {
                print!("{}", catalog::endpoints_markdown());
                return ExitCode::SUCCESS;
            }
            "--version" | "-V" => {
                println!("rustybin {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument: {other}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }

    let (config, warnings) = Config::load();
    rustybin::logging::init(&config);
    for warning in warnings {
        tracing::warn!("{warning}");
    }

    match rustybin::run(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("fatal: {e}");
            eprintln!("rustybin: fatal: {e}");
            ExitCode::FAILURE
        }
    }
}
