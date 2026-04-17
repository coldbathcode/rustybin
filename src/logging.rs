use crate::config::Config;
use tracing_subscriber::EnvFilter;

/// Initialize the tracing subscriber with the configured log level.
pub fn init(config: &Config) {
    let filter = EnvFilter::try_new(&config.log_level)
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}
