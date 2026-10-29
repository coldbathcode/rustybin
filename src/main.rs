use std::process::ExitCode;

use rustybin::{catalog, Config};

const USAGE: &str = "\
rustybin: HTTP stub service for API and AI gateway demos

USAGE:
    rustybin [--config rustybin.yaml]          run the server
    rustybin mock <command> ...                 OpenAPI mocks (rustybin mock --help)
    rustybin mcp-stdio --server <url> ...       stdio bridge to a management MCP server (--help)
    rustybin harness <command> ...              load through a gateway (rustybin harness --help)
    rustybin oidc-hash-password < password      a password hash for RUSTYBIN_OIDC_USERS_FILE
    rustybin [--print-endpoints-markdown | --version | --help]

Configuration is read from RUSTYBIN_* environment variables (see README.md).
Mocks are read from --config (or RUSTYBIN_CONFIG_FILE, or ./rustybin.yaml when
it exists) and RUSTYBIN_MOCKS_DIR.";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut config_file: Option<String> = None;
    match args.first().map(String::as_str) {
        None => {}
        Some("--print-endpoints-markdown") => {
            print!("{}", catalog::endpoints_markdown());
            return ExitCode::SUCCESS;
        }
        Some("--version") | Some("-V") => {
            println!("rustybin {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some("--help") | Some("-h") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("mock") => return rustybin::mocks::cli::run(&args[1..]).await,
        Some("mcp-stdio") => return rustybin::mcp::manage::stdio::run(&args[1..]).await,
        Some("harness") => return rustybin::harness::cli::run(&args[1..]).await,
        Some("oidc-hash-password") => return rustybin::oidc::users::hash_password_cli(),
        Some("--config") => match args.get(1) {
            Some(path) if args.len() == 2 => config_file = Some(path.clone()),
            _ => {
                eprintln!("--config needs exactly one file path\n\n{USAGE}");
                return ExitCode::from(2);
            }
        },
        Some(other) => {
            eprintln!("unknown argument: {other}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }

    let (mut config, warnings) = Config::load();
    rustybin::logging::init(&config);
    for warning in warnings {
        tracing::warn!("{warning}");
    }
    if config_file.is_some() {
        config.config_file = config_file;
    } else if config.config_file.is_none() && std::path::Path::new("rustybin.yaml").is_file() {
        config.config_file = Some("rustybin.yaml".to_string());
    }

    match rustybin::server::run_with_exit(config).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            tracing::error!("fatal: {e}");
            eprintln!("rustybin: fatal: {e}");
            ExitCode::FAILURE
        }
    }
}
