use crate::config::Config;
use tracing_subscriber::EnvFilter;

/// Initialize the tracing subscriber with the configured log level
/// (`RUSTYBIN_LOG_LEVEL`, falling back to `RUST_LOG`). Safe to call twice.
pub fn init(config: &Config) {
    let filter = EnvFilter::try_new(&config.log_level).unwrap_or_else(|_| EnvFilter::new("info"));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        // Colour codes only on a terminal; container log collectors get plain text.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .try_init();
}
