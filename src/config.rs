//! Runtime configuration, read from `RUSTYBIN_*` environment variables.
//!
//! Invalid values never abort startup: they produce a warning (collected in
//! [`Config::load`] and logged once tracing is initialised) and the default
//! is used instead.

use std::env;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use crate::limits::{LimitsConfig, Period, Plan, Scope};

/// Maximum delay a client may request via `X-Rustybin-Delay` (normal mode).
pub const MAX_DELAY_MS: u64 = 30_000;
/// Maximum delay a client may request via `X-Rustybin-Delay` (public mode).
pub const MAX_DELAY_MS_PUBLIC: u64 = 10_000;

#[derive(Clone, Debug)]
pub struct Config {
    /// HTTP listen port (`RUSTYBIN_HTTP_PORT`, default 80). 0 picks an ephemeral port.
    pub http_port: u16,
    /// HTTPS listen port (`RUSTYBIN_HTTPS_PORT`, default 443).
    pub https_port: u16,
    /// gRPC listen port (`RUSTYBIN_GRPC_PORT`, default 50051).
    pub grpc_port: u16,
    /// Bind address (`RUSTYBIN_HOST`, default 0.0.0.0). IPv6 such as `::` works.
    pub host: IpAddr,
    /// Tracing filter (`RUSTYBIN_LOG_LEVEL`, falling back to `RUST_LOG`, default `info`).
    pub log_level: String,
    /// Trust `X-Forwarded-For` for client IP detection (`RUSTYBIN_TRUST_FORWARD`).
    pub trust_forward: bool,
    /// Maximum request body size in bytes (`RUSTYBIN_BODY_LIMIT`, default 1 MiB).
    /// Enforced for every request (413 when exceeded) and used as the echo display limit.
    pub body_limit: usize,
    /// Instance identifier (`RUSTYBIN_INSTANCE_ID`, default random UUID).
    pub instance_id: String,
    /// TLS certificate path (`RUSTYBIN_TLS_CERT`).
    pub tls_cert: String,
    /// TLS private key path (`RUSTYBIN_TLS_KEY`).
    pub tls_key: String,
    /// Header carrying a URL-encoded client certificate (`RUSTYBIN_MTLS_IN_HEADER`).
    pub mtls_in_header: Option<String>,
    /// Public mode (`RUSTYBIN_PUBLIC_MODE`): tighter caps, per-session scoping,
    /// global mutations require the admin token.
    pub public_mode: bool,
    /// Admin token (`RUSTYBIN_ADMIN_TOKEN`). Never exposed by any endpoint.
    pub admin_token: Option<String>,
    /// Allowed CORS origins (`RUSTYBIN_CORS_ORIGINS`, comma separated, default `*`).
    /// `off` (or an empty value) disables the CORS layer entirely, which is what
    /// you want when demonstrating a gateway's own CORS handling.
    pub cors_allow_origins: Vec<String>,
    /// Time-to-headers timeout in seconds (`RUSTYBIN_REQUEST_TIMEOUT`, default 120, 0 disables).
    /// Streaming bodies (SSE, chunked) are not cut off once headers are sent.
    pub request_timeout_secs: u64,
    /// Number of requests kept by the inspector ring buffer (`RUSTYBIN_INSPECTOR_CAPACITY`, default 500).
    pub inspector_capacity: usize,
    /// Plan limits (`RUSTYBIN_PLAN`, `RUSTYBIN_LIMIT_*`, `RUSTYBIN_USAGE_FILE`).
    /// The default (plan `none`, no overrides) enforces nothing.
    pub limits: LimitsConfig,
}

impl Config {
    /// Read the configuration from the process environment, logging any
    /// warnings through `tracing` (call after logging is initialised, or use
    /// [`Config::load`] to get the warnings back).
    pub fn from_env() -> Self {
        let (config, warnings) = Self::load();
        for w in warnings {
            tracing::warn!("{w}");
        }
        config
    }

    /// Read the configuration from the process environment and return any
    /// warnings about invalid values (the default was used for those).
    pub fn load() -> (Self, Vec<String>) {
        Self::from_lookup(|key| env::var(key).ok())
    }

    /// Build a configuration from an arbitrary key lookup (used by tests).
    pub fn from_lookup<F>(lookup: F) -> (Self, Vec<String>)
    where
        F: Fn(&str) -> Option<String>,
    {
        let mut warnings = Vec::new();
        let get = |key: &str| {
            lookup(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };

        let host = match get("RUSTYBIN_HOST") {
            None => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            Some(raw) => {
                let stripped = raw.trim_start_matches('[').trim_end_matches(']');
                match stripped.parse::<IpAddr>() {
                    Ok(ip) => ip,
                    Err(_) if stripped.eq_ignore_ascii_case("localhost") => {
                        IpAddr::V4(Ipv4Addr::LOCALHOST)
                    }
                    Err(_) => {
                        warnings.push(format!(
                            "invalid RUSTYBIN_HOST {raw:?} (expected an IP address), using 0.0.0.0"
                        ));
                        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
                    }
                }
            }
        };

        let log_level = get("RUSTYBIN_LOG_LEVEL")
            .or_else(|| get("RUST_LOG"))
            .unwrap_or_else(|| "info".to_string());

        let cors_allow_origins = match lookup("RUSTYBIN_CORS_ORIGINS") {
            None => vec!["*".to_string()],
            Some(raw) => {
                let raw = raw.trim();
                if raw.is_empty() || raw.eq_ignore_ascii_case("off") {
                    Vec::new()
                } else {
                    raw.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                }
            }
        };

        let config = Self {
            http_port: parse_or(&get, "RUSTYBIN_HTTP_PORT", 80, &mut warnings),
            https_port: parse_or(&get, "RUSTYBIN_HTTPS_PORT", 443, &mut warnings),
            grpc_port: parse_or(&get, "RUSTYBIN_GRPC_PORT", 50051, &mut warnings),
            host,
            log_level,
            trust_forward: parse_bool_or(&get, "RUSTYBIN_TRUST_FORWARD", false, &mut warnings),
            body_limit: parse_or(&get, "RUSTYBIN_BODY_LIMIT", 1_048_576, &mut warnings),
            instance_id: get("RUSTYBIN_INSTANCE_ID").unwrap_or_else(|| Uuid::new_v4().to_string()),
            tls_cert: get("RUSTYBIN_TLS_CERT").unwrap_or_else(|| "certs/server.crt".to_string()),
            tls_key: get("RUSTYBIN_TLS_KEY").unwrap_or_else(|| "certs/server.key".to_string()),
            mtls_in_header: get("RUSTYBIN_MTLS_IN_HEADER"),
            public_mode: parse_bool_or(&get, "RUSTYBIN_PUBLIC_MODE", false, &mut warnings),
            admin_token: get("RUSTYBIN_ADMIN_TOKEN"),
            cors_allow_origins,
            request_timeout_secs: parse_or(&get, "RUSTYBIN_REQUEST_TIMEOUT", 120, &mut warnings),
            inspector_capacity: parse_or(&get, "RUSTYBIN_INSPECTOR_CAPACITY", 500, &mut warnings),
            limits: parse_limits(&get, &mut warnings),
        };
        (config, warnings)
    }

    /// Deterministic configuration for tests: loopback host, ephemeral ports,
    /// no TLS file paths (so nothing is ever written into the repo).
    pub fn for_tests() -> Self {
        Self {
            http_port: 0,
            https_port: 0,
            grpc_port: 0,
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            log_level: "warn".to_string(),
            trust_forward: false,
            body_limit: 1_048_576,
            instance_id: "test-instance".to_string(),
            // Empty paths: the server never writes certificate files and
            // HTTPS uses the in-memory demo certificate.
            tls_cert: String::new(),
            tls_key: String::new(),
            mtls_in_header: None,
            public_mode: false,
            admin_token: None,
            cors_allow_origins: vec!["*".to_string()],
            request_timeout_secs: 120,
            inspector_capacity: 500,
            limits: LimitsConfig::default(),
        }
    }

    /// Upper bound for client-requested delays (fault injection, `/delay`).
    pub fn max_delay_ms(&self) -> u64 {
        if self.public_mode {
            MAX_DELAY_MS_PUBLIC
        } else {
            MAX_DELAY_MS
        }
    }

    /// The effective configuration without secrets, for `/_rustybin/config`.
    pub fn public_view(&self) -> serde_json::Value {
        serde_json::json!({
            "http_port": self.http_port,
            "https_port": self.https_port,
            "grpc_port": self.grpc_port,
            "host": self.host.to_string(),
            "log_level": self.log_level,
            "trust_forward": self.trust_forward,
            "body_limit": self.body_limit,
            "instance_id": self.instance_id,
            "tls_cert": self.tls_cert,
            "tls_key": self.tls_key,
            "mtls_in_header": self.mtls_in_header,
            "public_mode": self.public_mode,
            "admin_token_configured": self.admin_token.is_some(),
            "cors_allow_origins": self.cors_allow_origins,
            "request_timeout_secs": self.request_timeout_secs,
            "inspector_capacity": self.inspector_capacity,
            "max_delay_ms": self.max_delay_ms(),
            "plan": self.limits.plan_name(),
            "limits": self.limits.public_view(),
        })
    }
}

/// `RUSTYBIN_PLAN` preset plus the `RUSTYBIN_LIMIT_*` overrides.
fn parse_limits<G>(get: &G, warnings: &mut Vec<String>) -> LimitsConfig
where
    G: Fn(&str) -> Option<String>,
{
    let plan = match get("RUSTYBIN_PLAN") {
        None => Plan::None,
        Some(raw) => Plan::parse(&raw).unwrap_or_else(|| {
            warnings.push(format!(
                "invalid RUSTYBIN_PLAN {raw:?} (expected none, free, pro, team or enterprise), using none"
            ));
            Plan::None
        }),
    };
    let mut cfg = LimitsConfig::preset(plan);
    let mut overridden = false;
    let mut limit = |key: &str, slot: &mut u64, warnings: &mut Vec<String>| {
        if let Some(raw) = get(key) {
            match parse_limit_value(&raw) {
                Some(v) => {
                    *slot = v;
                    overridden = true;
                }
                None => warnings.push(format!(
                    "invalid {key} {raw:?} (expected a whole number, 0 or unlimited), keeping the plan value"
                )),
            }
        }
    };
    limit("RUSTYBIN_LIMIT_RPS", &mut cfg.rps, warnings);
    limit("RUSTYBIN_LIMIT_BURST", &mut cfg.burst, warnings);
    limit("RUSTYBIN_LIMIT_CONCURRENCY", &mut cfg.concurrency, warnings);
    limit("RUSTYBIN_LIMIT_STREAMS", &mut cfg.streams, warnings);
    limit("RUSTYBIN_LIMIT_REQUESTS", &mut cfg.requests, warnings);
    let mut stream_secs = cfg.stream_lifetime.as_secs();
    limit("RUSTYBIN_LIMIT_STREAM_SECS", &mut stream_secs, warnings);
    cfg.stream_lifetime = Duration::from_secs(stream_secs);
    if let Some(raw) = get("RUSTYBIN_LIMIT_EGRESS_MB") {
        match parse_egress_mb(&raw) {
            Some(bytes) => {
                cfg.egress_bytes = bytes;
                overridden = true;
            }
            None => warnings.push(format!(
                "invalid RUSTYBIN_LIMIT_EGRESS_MB {raw:?} (expected megabytes such as 500 or 0.5, 0 or unlimited), keeping the plan value"
            )),
        }
    }
    if let Some(raw) = get("RUSTYBIN_LIMIT_PERIOD") {
        match Period::parse(&raw) {
            Some(p) => {
                cfg.period = p;
                overridden = true;
            }
            None => warnings.push(format!(
                "invalid RUSTYBIN_LIMIT_PERIOD {raw:?} (expected day or month), keeping the plan value"
            )),
        }
    }
    if let Some(raw) = get("RUSTYBIN_LIMIT_SCOPE") {
        match Scope::parse(&raw) {
            Some(s) => {
                cfg.scope = s;
                overridden = true;
            }
            None => warnings.push(format!(
                "invalid RUSTYBIN_LIMIT_SCOPE {raw:?} (expected instance or session), keeping the plan value"
            )),
        }
    }
    cfg.overridden = overridden;
    cfg.usage_file = get("RUSTYBIN_USAGE_FILE").map(PathBuf::from);
    cfg
}

/// A limit value: a whole number, `0`, `off` or `unlimited` (0 = no limit).
fn parse_limit_value(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.eq_ignore_ascii_case("unlimited") || raw.eq_ignore_ascii_case("off") {
        return Some(0);
    }
    raw.replace('_', "").parse::<u64>().ok()
}

/// Megabytes (decimal, 1 MB = 1,000,000 bytes; fractions allowed) to bytes.
fn parse_egress_mb(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.eq_ignore_ascii_case("unlimited") || raw.eq_ignore_ascii_case("off") {
        return Some(0);
    }
    let mb = raw.replace('_', "").parse::<f64>().ok()?;
    if !(0.0..=1.0e12).contains(&mb) {
        return None;
    }
    Some((mb * 1_000_000.0).round() as u64)
}

fn parse_or<T, G>(get: &G, key: &str, default: T, warnings: &mut Vec<String>) -> T
where
    T: std::str::FromStr + std::fmt::Display,
    G: Fn(&str) -> Option<String>,
{
    match get(key) {
        None => default,
        Some(raw) => match raw.parse::<T>() {
            Ok(v) => v,
            Err(_) => {
                warnings.push(format!("invalid {key} {raw:?}, using default {default}"));
                default
            }
        },
    }
}

fn parse_bool_or<G>(get: &G, key: &str, default: bool, warnings: &mut Vec<String>) -> bool
where
    G: Fn(&str) -> Option<String>,
{
    match get(key) {
        None => default,
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => {
                warnings.push(format!("invalid {key} {raw:?}, using default {default}"));
                default
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn load(pairs: &[(&str, &str)]) -> (Config, Vec<String>) {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|k| map.get(k).cloned())
    }

    #[test]
    fn defaults() {
        let (c, w) = load(&[]);
        assert!(w.is_empty());
        assert_eq!(c.http_port, 80);
        assert_eq!(c.https_port, 443);
        assert_eq!(c.grpc_port, 50051);
        assert_eq!(c.host.to_string(), "0.0.0.0");
        assert_eq!(c.log_level, "info");
        assert_eq!(c.body_limit, 1_048_576);
        assert_eq!(c.cors_allow_origins, vec!["*"]);
        assert_eq!(c.request_timeout_secs, 120);
        assert_eq!(c.inspector_capacity, 500);
        assert!(!c.public_mode);
        assert!(c.admin_token.is_none());
    }

    #[test]
    fn invalid_values_warn_and_default() {
        let (c, w) = load(&[
            ("RUSTYBIN_HTTP_PORT", "eighty"),
            ("RUSTYBIN_PUBLIC_MODE", "maybe"),
            ("RUSTYBIN_HOST", "not-an-ip"),
        ]);
        assert_eq!(c.http_port, 80);
        assert!(!c.public_mode);
        assert_eq!(c.host.to_string(), "0.0.0.0");
        assert_eq!(w.len(), 3);
    }

    #[test]
    fn ipv6_host_and_rust_log_fallback() {
        let (c, _) = load(&[("RUSTYBIN_HOST", "[::]"), ("RUST_LOG", "debug")]);
        assert_eq!(c.host.to_string(), "::");
        assert_eq!(c.log_level, "debug");
        let (c, _) = load(&[("RUSTYBIN_LOG_LEVEL", "warn"), ("RUST_LOG", "debug")]);
        assert_eq!(c.log_level, "warn");
    }

    #[test]
    fn cors_origins_parsing() {
        let (c, _) = load(&[(
            "RUSTYBIN_CORS_ORIGINS",
            "https://a.example, https://b.example",
        )]);
        assert_eq!(
            c.cors_allow_origins,
            vec!["https://a.example", "https://b.example"]
        );
        let (c, _) = load(&[("RUSTYBIN_CORS_ORIGINS", "off")]);
        assert!(c.cors_allow_origins.is_empty());
    }

    #[test]
    fn plan_presets_and_overrides() {
        let (c, w) = load(&[]);
        assert!(w.is_empty());
        assert_eq!(c.limits.plan, Plan::None);
        assert!(!c.limits.active());

        let (c, w) = load(&[("RUSTYBIN_PLAN", "Free")]);
        assert!(w.is_empty());
        assert_eq!(c.limits.plan, Plan::Free);
        assert_eq!(c.limits.scope, Scope::Session);
        assert_eq!(c.limits.period, Period::Day);
        assert_eq!((c.limits.rps, c.limits.burst), (5, 20));
        assert_eq!(c.limits.requests, 10_000);
        assert_eq!(c.limits.egress_bytes, 1_000_000_000);
        assert_eq!(c.limits.stream_lifetime, Duration::from_secs(300));

        let (c, w) = load(&[
            ("RUSTYBIN_PLAN", "pro"),
            ("RUSTYBIN_LIMIT_RPS", "1000000"),
            ("RUSTYBIN_LIMIT_BURST", "unlimited"),
            ("RUSTYBIN_LIMIT_EGRESS_MB", "0.5"),
            ("RUSTYBIN_LIMIT_PERIOD", "day"),
            ("RUSTYBIN_LIMIT_SCOPE", "session"),
            ("RUSTYBIN_LIMIT_STREAM_SECS", "0"),
            ("RUSTYBIN_USAGE_FILE", "/data/usage.json"),
        ]);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.limits.rps, 1_000_000);
        assert_eq!(c.limits.burst, 0);
        assert_eq!(c.limits.egress_bytes, 500_000);
        assert_eq!(c.limits.period, Period::Day);
        assert_eq!(c.limits.scope, Scope::Session);
        assert_eq!(c.limits.stream_lifetime, Duration::ZERO);
        assert_eq!(c.limits.requests, 1_000_000);
        assert!(c.limits.usage_file.is_some());

        // Overrides without a plan: active, reported as "custom".
        let (c, _) = load(&[("RUSTYBIN_LIMIT_RPS", "10")]);
        assert!(c.limits.active());
        assert_eq!(c.limits.plan_name(), "custom");

        let (c, w) = load(&[
            ("RUSTYBIN_PLAN", "platinum"),
            ("RUSTYBIN_LIMIT_RPS", "fast"),
            ("RUSTYBIN_LIMIT_PERIOD", "week"),
        ]);
        assert_eq!(c.limits.plan, Plan::None);
        assert_eq!(w.len(), 3);
        assert!(!c.limits.active());
    }

    #[test]
    fn public_view_hides_admin_token() {
        let mut c = Config::for_tests();
        c.admin_token = Some("s3cret".to_string());
        let v = c.public_view();
        assert!(!v.to_string().contains("s3cret"));
        assert_eq!(v["admin_token_configured"], true);
    }
}
