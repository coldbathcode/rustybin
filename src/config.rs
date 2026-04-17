use std::env;
use uuid::Uuid;

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Config {
    pub http_port: u16,
    pub https_port: u16,
    pub host: String,
    pub log_level: String,
    pub trust_forward: bool,
    pub body_limit: usize,
    pub instance_id: String,
    pub tls_cert: String,
    pub tls_key: String,
    pub mtls_in_header: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            http_port: env_or("RUSTYBIN_HTTP_PORT", "80")
                .parse()
                .unwrap_or(80),
            https_port: env_or("RUSTYBIN_HTTPS_PORT", "443")
                .parse()
                .unwrap_or(443),
            host: env_or("RUSTYBIN_HOST", "0.0.0.0"),
            log_level: env_or("RUSTYBIN_LOG_LEVEL", "info"),
            trust_forward: env_or("RUSTYBIN_TRUST_FORWARD", "false")
                .parse()
                .unwrap_or(false),
            body_limit: env_or("RUSTYBIN_BODY_LIMIT", "1048576")
                .parse()
                .unwrap_or(1_048_576),
            instance_id: env_or("RUSTYBIN_INSTANCE_ID", &Uuid::new_v4().to_string()),
            tls_cert: env_or("RUSTYBIN_TLS_CERT", "certs/server.crt"),
            tls_key: env_or("RUSTYBIN_TLS_KEY", "certs/server.key"),
            mtls_in_header: env::var("RUSTYBIN_MTLS_IN_HEADER").ok(),
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}
