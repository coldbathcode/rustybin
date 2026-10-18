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

use crate::control_auth::ControlAuth;
use crate::limits::{LimitsConfig, Period, Plan, Scope};
use crate::logging::LogFormat;

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
    /// Hosted preset (`RUSTYBIN_HOSTED_MODE`): control auth `jwt` and JSON
    /// logs unless `RUSTYBIN_CONTROL_AUTH` / `RUSTYBIN_LOG_FORMAT` say otherwise.
    pub hosted_mode: bool,
    /// Control-plane authentication (`RUSTYBIN_CONTROL_AUTH`, default `open`).
    pub control_auth: ControlAuth,
    /// Ed25519 public key for control-plane JWTs (`RUSTYBIN_CONTROL_JWT_PUBLIC_KEY`).
    pub control_jwt_key: Option<[u8; 32]>,
    /// Required `aud` of control-plane JWTs (`RUSTYBIN_CONTROL_JWT_AUDIENCE`,
    /// default the instance id).
    pub control_jwt_audience: String,
    /// Log line format (`RUSTYBIN_LOG_FORMAT`, `text` or `json`).
    pub log_format: LogFormat,
    /// Serve gRPC on the HTTP and HTTPS listeners too (`RUSTYBIN_GRPC_ON_HTTP`).
    pub grpc_on_http: bool,
    /// Link back to a portal, shown in the console header (`RUSTYBIN_CONSOLE_BACKLINK`).
    pub console_backlink: Option<String>,
    /// Title shown in the console header (`RUSTYBIN_CONSOLE_TITLE`).
    pub console_title: Option<String>,
    /// Shared mode (`RUSTYBIN_SHARED_MODE`): every data-plane request needs an
    /// API key from the key registry; implies public mode, session-scoped
    /// limits and daily quotas (see `crate::keys`).
    pub shared_mode: bool,
    /// Require an API key on the data plane of a normal instance
    /// (`RUSTYBIN_REQUIRE_KEY`); control-plane auth is unchanged.
    pub require_key: bool,
    /// Key registry file (`RUSTYBIN_KEYS_FILE`), reloaded when it changes.
    pub keys_file: Option<PathBuf>,
    /// Key registry URL (`RUSTYBIN_KEYS_URL`), polled.
    pub keys_url: Option<String>,
    /// Bearer token sent to the registry URL (`RUSTYBIN_KEYS_TOKEN`). Never exposed.
    pub keys_token: Option<String>,
    /// Registry URL poll interval in seconds (`RUSTYBIN_KEYS_POLL_SECS`, default 30).
    pub keys_poll_secs: u64,
    /// Most keys accepted in a registry (`RUSTYBIN_KEYS_MAX`, default 200,000).
    pub keys_max: usize,
    /// Shared quota store URL (`RUSTYBIN_QUOTA_STORE`, `postgres://...`;
    /// needs the `quota-postgres` feature). Never exposed.
    pub quota_store: Option<String>,
    /// Extra CA certificates (PEM) for the quota store's TLS (`RUSTYBIN_QUOTA_STORE_CA`).
    pub quota_store_ca: Option<PathBuf>,
    /// Quota store flush interval in milliseconds (`RUSTYBIN_QUOTA_FLUSH_MS`, default 2000).
    pub quota_flush_ms: u64,
    /// Link returned in daily quota 429 bodies (`RUSTYBIN_QUOTA_UPGRADE_URL`).
    pub quota_upgrade_url: Option<String>,
    /// `rustybin.yaml` declaring mocks (`RUSTYBIN_CONFIG_FILE` or `--config`).
    pub config_file: Option<String>,
    /// Directory whose OpenAPI files become mocks (`RUSTYBIN_MOCKS_DIR`).
    pub mocks_dir: Option<String>,
    /// Create the built-in `petstore` mock (`RUSTYBIN_MOCKS_SAMPLE`, default
    /// on outside public mode).
    pub mocks_sample: bool,
    /// Directory where mocks created or edited through the API are saved and
    /// reloaded from at startup (`RUSTYBIN_MOCKS_STATE_DIR`).
    pub mocks_state_dir: Option<String>,
    /// Domain under which `<name>.<domain>` serves the mock `<name>` at its
    /// root (`RUSTYBIN_MOCKS_HOST`, e.g. `mock.example.com`).
    pub mocks_host: Option<String>,
    /// Self-service mocks for visitors of a public instance
    /// (`RUSTYBIN_MOCKS_SELF_SERVICE` and friends).
    pub self_service: SelfServiceConfig,
}

/// What the email form asks for (`RUSTYBIN_LEADS_CONSENT`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LeadsConsent {
    /// A separate, unticked marketing checkbox (the default).
    #[default]
    Checkbox,
    /// No checkbox: the notice says what the address is used for.
    Notice,
}

impl LeadsConsent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Checkbox => "checkbox",
            Self::Notice => "notice",
        }
    }
}

/// Self-service mocks (`docs/plans/hosted-mocks.md`): visitors without the
/// admin token create and manage their own mocks, on a time allowance.
#[derive(Clone, Debug)]
pub struct SelfServiceConfig {
    /// Effective switch: requested, in public mode, and with a control plane
    /// visitors can reach (`open`, or shared mode).
    pub enabled: bool,
    /// Domain serving visitor mocks as `<name>.<domain>`
    /// (`RUSTYBIN_MOCKS_SELF_SERVICE_HOST`).
    pub host: Option<String>,
    /// Where the bare mocks domain redirects (`RUSTYBIN_MOCKS_SELF_SERVICE_HOME`).
    pub home_url: Option<String>,
    /// Mocks per visit or API key (`RUSTYBIN_MOCKS_PER_OWNER`).
    pub per_owner: usize,
    /// Visitor mocks on the instance (`RUSTYBIN_MOCKS_SELF_SERVICE_MAX`).
    pub max_mocks: usize,
    /// Creations per session key per hour (`RUSTYBIN_MOCKS_CREATE_PER_HOUR`).
    pub create_per_hour: u32,
    /// First period of a visit and each extension (`RUSTYBIN_MOCKS_VISIT_MINUTES`).
    pub visit_minutes: u64,
    /// Visit time without an email address (`RUSTYBIN_MOCKS_ANON_HOURS`).
    pub anon_hours: u64,
    /// Time per email address and UTC day (`RUSTYBIN_MOCKS_DAILY_HOURS`).
    pub daily_hours: u64,
    /// Anonymous visits per session key and UTC day
    /// (`RUSTYBIN_MOCKS_ANON_VISITS_PER_DAY`).
    pub anon_visits_per_day: u32,
    /// Slug globs refused on create (`RUSTYBIN_MOCKS_BLOCKED_NAMES`).
    pub blocked_names: Vec<String>,
    /// Where captured email addresses are posted (`RUSTYBIN_LEADS_WEBHOOK_URL`).
    pub leads_webhook_url: Option<String>,
    /// Bearer token for the webhook (`RUSTYBIN_LEADS_WEBHOOK_TOKEN`). Never exposed.
    pub leads_webhook_token: Option<String>,
    /// JSON lines file the leads are appended to (`RUSTYBIN_LEADS_FILE`).
    pub leads_file: Option<PathBuf>,
    /// Privacy notice linked from the email form (`RUSTYBIN_PRIVACY_URL`).
    pub privacy_url: Option<String>,
    pub leads_consent: LeadsConsent,
    /// Salt of the email hashes in the daily ledger (`RUSTYBIN_LEADS_SALT`;
    /// generated at startup when unset). Never exposed.
    pub leads_salt: Option<String>,
}

impl Default for SelfServiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: None,
            home_url: None,
            per_owner: 3,
            max_mocks: 200,
            create_per_hour: 10,
            visit_minutes: 60,
            anon_hours: 3,
            daily_hours: 6,
            anon_visits_per_day: 3,
            blocked_names: Vec::new(),
            leads_webhook_url: None,
            leads_webhook_token: None,
            leads_file: None,
            privacy_url: None,
            leads_consent: LeadsConsent::Checkbox,
            leads_salt: None,
        }
    }
}

impl SelfServiceConfig {
    /// Email capture is on: somewhere to deliver the address and a privacy
    /// notice to link.
    pub fn leads_enabled(&self) -> bool {
        (self.leads_webhook_url.is_some() || self.leads_file.is_some())
            && self.privacy_url.is_some()
    }
}

/// Default `RUSTYBIN_KEYS_POLL_SECS`.
pub const DEFAULT_KEYS_POLL_SECS: u64 = 30;
/// Default `RUSTYBIN_KEYS_MAX`.
pub const DEFAULT_KEYS_MAX: usize = 200_000;
/// Default `RUSTYBIN_QUOTA_FLUSH_MS`.
pub const DEFAULT_QUOTA_FLUSH_MS: u64 = 2_000;

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

        let instance_id = get("RUSTYBIN_INSTANCE_ID").unwrap_or_else(|| Uuid::new_v4().to_string());
        let hosted_mode = parse_bool_or(&get, "RUSTYBIN_HOSTED_MODE", false, &mut warnings);
        let admin_token = get("RUSTYBIN_ADMIN_TOKEN");

        let control_auth_default = if hosted_mode {
            ControlAuth::Jwt
        } else {
            ControlAuth::Open
        };
        let control_auth = match get("RUSTYBIN_CONTROL_AUTH") {
            None => control_auth_default,
            Some(raw) => ControlAuth::parse(&raw).unwrap_or_else(|| {
                warnings.push(format!(
                    "invalid RUSTYBIN_CONTROL_AUTH {raw:?} (expected open, token or jwt), using {}",
                    control_auth_default.as_str()
                ));
                control_auth_default
            }),
        };
        let control_jwt_key = get("RUSTYBIN_CONTROL_JWT_PUBLIC_KEY").and_then(|raw| {
            let key = crate::control_auth::parse_public_key(&raw);
            if key.is_none() {
                warnings.push(
                    "invalid RUSTYBIN_CONTROL_JWT_PUBLIC_KEY (expected an Ed25519 public key: PEM, \
                     or base64 of the raw 32 bytes), control-plane JWTs are refused"
                        .to_string(),
                );
            }
            key
        });
        let control_jwt_audience =
            get("RUSTYBIN_CONTROL_JWT_AUDIENCE").unwrap_or_else(|| instance_id.clone());
        match control_auth {
            ControlAuth::Token if admin_token.is_none() => warnings.push(
                "RUSTYBIN_CONTROL_AUTH=token without RUSTYBIN_ADMIN_TOKEN: every control-plane \
                 request is refused"
                    .to_string(),
            ),
            ControlAuth::Jwt if control_jwt_key.is_none() => warnings.push(format!(
                "RUSTYBIN_CONTROL_AUTH=jwt without a valid RUSTYBIN_CONTROL_JWT_PUBLIC_KEY: {}",
                if admin_token.is_some() {
                    "only the admin token is accepted on the control plane"
                } else {
                    "every control-plane request is refused"
                }
            )),
            _ => {}
        }

        let log_format_default = if hosted_mode {
            LogFormat::Json
        } else {
            LogFormat::Text
        };
        let log_format = match get("RUSTYBIN_LOG_FORMAT") {
            None => log_format_default,
            Some(raw) => LogFormat::parse(&raw).unwrap_or_else(|| {
                warnings.push(format!(
                    "invalid RUSTYBIN_LOG_FORMAT {raw:?} (expected text or json), using {}",
                    log_format_default.as_str()
                ));
                log_format_default
            }),
        };

        let console_backlink = get("RUSTYBIN_CONSOLE_BACKLINK").and_then(|raw| {
            let ok = valid_backlink(&raw);
            if !ok {
                warnings.push(format!(
                    "invalid RUSTYBIN_CONSOLE_BACKLINK {raw:?} (expected an http:// or https:// URL), ignored"
                ));
            }
            ok.then_some(raw)
        });
        let console_title = get("RUSTYBIN_CONSOLE_TITLE")
            .map(|raw| {
                raw.chars()
                    .filter(|c| !c.is_control())
                    .take(MAX_CONSOLE_TITLE_CHARS)
                    .collect::<String>()
                    .trim()
                    .to_string()
            })
            .filter(|t| !t.is_empty());

        let shared_mode = parse_bool_or(&get, "RUSTYBIN_SHARED_MODE", false, &mut warnings);
        let require_key = parse_bool_or(&get, "RUSTYBIN_REQUIRE_KEY", false, &mut warnings);
        let keys_file = get("RUSTYBIN_KEYS_FILE").map(PathBuf::from);
        let keys_url = get("RUSTYBIN_KEYS_URL").and_then(|raw| {
            let ok = valid_backlink(&raw);
            if !ok {
                warnings.push(format!(
                    "invalid RUSTYBIN_KEYS_URL {raw:?} (expected an http:// or https:// URL), ignored"
                ));
            }
            ok.then_some(raw)
        });
        if keys_file.is_some() && keys_url.is_some() {
            warnings.push(
                "both RUSTYBIN_KEYS_FILE and RUSTYBIN_KEYS_URL are set: using the file".to_string(),
            );
        }
        if (shared_mode || require_key) && keys_file.is_none() && keys_url.is_none() {
            warnings.push(format!(
                "{} without RUSTYBIN_KEYS_FILE or RUSTYBIN_KEYS_URL: every data-plane request \
                 gets 503 rustybin_keys_unavailable",
                if shared_mode {
                    "RUSTYBIN_SHARED_MODE"
                } else {
                    "RUSTYBIN_REQUIRE_KEY"
                }
            ));
        }
        let quota_upgrade_url = get("RUSTYBIN_QUOTA_UPGRADE_URL").and_then(|raw| {
            let ok = valid_backlink(&raw);
            if !ok {
                warnings.push(format!(
                    "invalid RUSTYBIN_QUOTA_UPGRADE_URL {raw:?} (expected an http:// or https:// URL), ignored"
                ));
            }
            ok.then_some(raw)
        });
        let mut limits = parse_limits(&get, &mut warnings);
        if shared_mode {
            // Shared mode: one set of counters per API key, daily quotas.
            limits.scope = Scope::Session;
            limits.period = Period::Day;
        }
        let public_mode =
            shared_mode || parse_bool_or(&get, "RUSTYBIN_PUBLIC_MODE", false, &mut warnings);
        let mocks_sample =
            parse_bool_or(&get, "RUSTYBIN_MOCKS_SAMPLE", !public_mode, &mut warnings);
        let self_service = parse_self_service(
            &get,
            public_mode,
            shared_mode || control_auth == ControlAuth::Open,
            &mut warnings,
        );

        let config = Self {
            http_port: parse_or(&get, "RUSTYBIN_HTTP_PORT", 80, &mut warnings),
            https_port: parse_or(&get, "RUSTYBIN_HTTPS_PORT", 443, &mut warnings),
            grpc_port: parse_or(&get, "RUSTYBIN_GRPC_PORT", 50051, &mut warnings),
            host,
            log_level,
            trust_forward: parse_bool_or(&get, "RUSTYBIN_TRUST_FORWARD", false, &mut warnings),
            body_limit: parse_or(&get, "RUSTYBIN_BODY_LIMIT", 1_048_576, &mut warnings),
            instance_id,
            tls_cert: get("RUSTYBIN_TLS_CERT").unwrap_or_else(|| "certs/server.crt".to_string()),
            tls_key: get("RUSTYBIN_TLS_KEY").unwrap_or_else(|| "certs/server.key".to_string()),
            mtls_in_header: get("RUSTYBIN_MTLS_IN_HEADER"),
            public_mode,
            admin_token,
            cors_allow_origins,
            request_timeout_secs: parse_or(&get, "RUSTYBIN_REQUEST_TIMEOUT", 120, &mut warnings),
            inspector_capacity: parse_or(&get, "RUSTYBIN_INSPECTOR_CAPACITY", 500, &mut warnings),
            limits,
            hosted_mode,
            control_auth,
            control_jwt_key,
            control_jwt_audience,
            log_format,
            grpc_on_http: parse_bool_or(&get, "RUSTYBIN_GRPC_ON_HTTP", false, &mut warnings),
            console_backlink,
            console_title,
            shared_mode,
            require_key,
            keys_file,
            keys_url,
            keys_token: get("RUSTYBIN_KEYS_TOKEN"),
            keys_poll_secs: parse_or(
                &get,
                "RUSTYBIN_KEYS_POLL_SECS",
                DEFAULT_KEYS_POLL_SECS,
                &mut warnings,
            )
            .max(1),
            keys_max: parse_or(&get, "RUSTYBIN_KEYS_MAX", DEFAULT_KEYS_MAX, &mut warnings).max(1),
            quota_store: get("RUSTYBIN_QUOTA_STORE"),
            quota_store_ca: get("RUSTYBIN_QUOTA_STORE_CA").map(PathBuf::from),
            quota_flush_ms: parse_or(
                &get,
                "RUSTYBIN_QUOTA_FLUSH_MS",
                DEFAULT_QUOTA_FLUSH_MS,
                &mut warnings,
            )
            .max(10),
            quota_upgrade_url,
            config_file: get("RUSTYBIN_CONFIG_FILE"),
            mocks_dir: get("RUSTYBIN_MOCKS_DIR"),
            mocks_sample,
            mocks_state_dir: get("RUSTYBIN_MOCKS_STATE_DIR"),
            mocks_host: get("RUSTYBIN_MOCKS_HOST").and_then(|h| normalise_domain(&h)),
            self_service,
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
            hosted_mode: false,
            control_auth: ControlAuth::Open,
            control_jwt_key: None,
            control_jwt_audience: "test-instance".to_string(),
            log_format: LogFormat::Text,
            grpc_on_http: false,
            console_backlink: None,
            console_title: None,
            shared_mode: false,
            require_key: false,
            keys_file: None,
            keys_url: None,
            keys_token: None,
            keys_poll_secs: DEFAULT_KEYS_POLL_SECS,
            keys_max: DEFAULT_KEYS_MAX,
            quota_store: None,
            quota_store_ca: None,
            quota_flush_ms: DEFAULT_QUOTA_FLUSH_MS,
            quota_upgrade_url: None,
            config_file: None,
            mocks_dir: None,
            mocks_sample: true,
            mocks_state_dir: None,
            mocks_host: None,
            self_service: SelfServiceConfig::default(),
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
        let mut view = serde_json::json!({
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
            "hosted_mode": self.hosted_mode,
            "control_auth": self.control_auth.as_str(),
            "control_jwt_key_configured": self.control_jwt_key.is_some(),
            "control_jwt_audience": self.control_jwt_audience,
            "log_format": self.log_format.as_str(),
            "grpc_on_http": self.grpc_on_http,
            "console_backlink": self.console_backlink,
            "console_title": self.console_title,
            "shared_mode": self.shared_mode,
            "require_key": self.require_key,
            "keys_source": if self.keys_file.is_some() {
                Some("file")
            } else if self.keys_url.is_some() {
                Some("url")
            } else {
                None
            },
            "keys_poll_secs": self.keys_poll_secs,
            "keys_max": self.keys_max,
            "quota_store_configured": self.quota_store.is_some(),
            "quota_flush_ms": self.quota_flush_ms,
            "quota_upgrade_url": self.quota_upgrade_url,
            "config_file_configured": self.config_file.is_some(),
            "mocks_dir_configured": self.mocks_dir.is_some(),
            "mocks_sample": self.mocks_sample,
            "mocks_state_dir_configured": self.mocks_state_dir.is_some(),
            "mocks_host": self.mocks_host,
        });
        if let serde_json::Value::Object(m) = &mut view {
            let ss = &self.self_service;
            m.insert("mocks_self_service".into(), ss.enabled.into());
            m.insert("mocks_self_service_host".into(), serde_json::json!(ss.host));
            m.insert(
                "leads_enabled".into(),
                (ss.enabled && ss.leads_enabled()).into(),
            );
        }
        view
    }
}

/// Longest console title kept (`RUSTYBIN_CONSOLE_TITLE`).
pub const MAX_CONSOLE_TITLE_CHARS: usize = 80;
/// Longest console back link accepted (`RUSTYBIN_CONSOLE_BACKLINK`).
pub const MAX_BACKLINK_LEN: usize = 2048;

/// An absolute `http://` or `https://` URL with a host, without whitespace,
/// control characters, quotes or angle brackets.
pub fn valid_backlink(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    let Some(rest) = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
    else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    raw.len() <= MAX_BACKLINK_LEN
        && !host.is_empty()
        && !host.contains('@')
        && raw.chars().all(|c| {
            !c.is_control()
                && !c.is_whitespace()
                && !matches!(c, '"' | '\'' | '<' | '>' | '`' | '\\')
        })
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

fn normalise_domain(raw: &str) -> Option<String> {
    let d = raw.trim().trim_start_matches('.').to_ascii_lowercase();
    (!d.is_empty()).then_some(d)
}

/// `RUSTYBIN_MOCKS_SELF_SERVICE` and its settings. `reachable`: visitors can
/// reach the control plane (control auth `open`, or shared mode).
fn parse_self_service<G>(
    get: &G,
    public_mode: bool,
    reachable: bool,
    warnings: &mut Vec<String>,
) -> SelfServiceConfig
where
    G: Fn(&str) -> Option<String>,
{
    let d = SelfServiceConfig::default();
    let requested = parse_bool_or(get, "RUSTYBIN_MOCKS_SELF_SERVICE", false, warnings);
    let enabled = match (requested, public_mode, reachable) {
        (false, ..) => false,
        (true, false, _) => {
            warnings.push(
                "RUSTYBIN_MOCKS_SELF_SERVICE needs RUSTYBIN_PUBLIC_MODE (or shared mode): outside \
                 public mode everyone may already manage mocks; ignored"
                    .to_string(),
            );
            false
        }
        (true, true, false) => {
            warnings.push(
                "RUSTYBIN_MOCKS_SELF_SERVICE is ignored with RUSTYBIN_CONTROL_AUTH token or jwt: \
                 visitors cannot reach the control plane"
                    .to_string(),
            );
            false
        }
        (true, true, true) => true,
    };
    let url = |key: &str, warnings: &mut Vec<String>| {
        get(key).and_then(|raw| {
            let ok = valid_backlink(&raw);
            if !ok {
                warnings.push(format!(
                    "invalid {key} {raw:?} (expected an http:// or https:// URL), ignored"
                ));
            }
            ok.then_some(raw)
        })
    };
    let visit_minutes = parse_or(
        get,
        "RUSTYBIN_MOCKS_VISIT_MINUTES",
        d.visit_minutes,
        warnings,
    )
    .clamp(1, 1440);
    let anon_hours = parse_or(get, "RUSTYBIN_MOCKS_ANON_HOURS", d.anon_hours, warnings).max(1);
    let daily_hours = parse_or(get, "RUSTYBIN_MOCKS_DAILY_HOURS", d.daily_hours, warnings);
    if daily_hours < anon_hours {
        warnings.push(format!(
            "RUSTYBIN_MOCKS_DAILY_HOURS {daily_hours} is below RUSTYBIN_MOCKS_ANON_HOURS \
             {anon_hours}: using {anon_hours}"
        ));
    }
    let leads_consent = match get("RUSTYBIN_LEADS_CONSENT").as_deref() {
        None | Some("checkbox") => LeadsConsent::Checkbox,
        Some("notice") => LeadsConsent::Notice,
        Some(other) => {
            warnings.push(format!(
                "invalid RUSTYBIN_LEADS_CONSENT {other:?} (checkbox or notice), using checkbox"
            ));
            LeadsConsent::Checkbox
        }
    };
    let cfg = SelfServiceConfig {
        enabled,
        host: get("RUSTYBIN_MOCKS_SELF_SERVICE_HOST").and_then(|h| normalise_domain(&h)),
        home_url: url("RUSTYBIN_MOCKS_SELF_SERVICE_HOME", warnings),
        per_owner: parse_or(get, "RUSTYBIN_MOCKS_PER_OWNER", d.per_owner, warnings).max(1),
        max_mocks: parse_or(
            get,
            "RUSTYBIN_MOCKS_SELF_SERVICE_MAX",
            d.max_mocks,
            warnings,
        )
        .clamp(1, 10_000),
        create_per_hour: parse_or(
            get,
            "RUSTYBIN_MOCKS_CREATE_PER_HOUR",
            d.create_per_hour,
            warnings,
        )
        .max(1),
        visit_minutes,
        anon_hours,
        daily_hours: daily_hours.max(anon_hours),
        anon_visits_per_day: parse_or(
            get,
            "RUSTYBIN_MOCKS_ANON_VISITS_PER_DAY",
            d.anon_visits_per_day,
            warnings,
        ),
        blocked_names: get("RUSTYBIN_MOCKS_BLOCKED_NAMES")
            .map(|raw| {
                raw.split(',')
                    .map(|p| p.trim().to_ascii_lowercase())
                    .filter(|p| !p.is_empty())
                    .take(200)
                    .collect()
            })
            .unwrap_or_default(),
        leads_webhook_url: url("RUSTYBIN_LEADS_WEBHOOK_URL", warnings),
        leads_webhook_token: get("RUSTYBIN_LEADS_WEBHOOK_TOKEN"),
        leads_file: get("RUSTYBIN_LEADS_FILE").map(PathBuf::from),
        privacy_url: url("RUSTYBIN_PRIVACY_URL", warnings),
        leads_consent,
        leads_salt: get("RUSTYBIN_LEADS_SALT").filter(|s| !s.is_empty()),
    };
    if cfg.enabled
        && (cfg.leads_webhook_url.is_some() || cfg.leads_file.is_some())
        && cfg.privacy_url.is_none()
    {
        warnings.push(
            "email capture is off: RUSTYBIN_PRIVACY_URL is required with RUSTYBIN_LEADS_WEBHOOK_URL \
             or RUSTYBIN_LEADS_FILE"
                .to_string(),
        );
    }
    if cfg.enabled && cfg.host.is_none() {
        warnings.push(
            "RUSTYBIN_MOCKS_SELF_SERVICE without RUSTYBIN_MOCKS_SELF_SERVICE_HOST: visitor mocks \
             share the console's origin (fine for testing, not for a public demo)"
                .to_string(),
        );
    }
    cfg
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
    fn self_service_needs_public_mode_and_a_reachable_control_plane() {
        let (c, w) = load(&[("RUSTYBIN_MOCKS_SELF_SERVICE", "true")]);
        assert!(!c.self_service.enabled);
        assert!(w.iter().any(|w| w.contains("needs RUSTYBIN_PUBLIC_MODE")));
        let (c, w) = load(&[
            ("RUSTYBIN_MOCKS_SELF_SERVICE", "true"),
            ("RUSTYBIN_PUBLIC_MODE", "true"),
            ("RUSTYBIN_CONTROL_AUTH", "token"),
            ("RUSTYBIN_ADMIN_TOKEN", "t"),
        ]);
        assert!(!c.self_service.enabled);
        assert!(w.iter().any(|w| w.contains("token or jwt")));
        let (c, w) = load(&[
            ("RUSTYBIN_MOCKS_SELF_SERVICE", "true"),
            ("RUSTYBIN_PUBLIC_MODE", "true"),
            ("RUSTYBIN_MOCKS_SELF_SERVICE_HOST", ".Rustybin-Mock.dev"),
            ("RUSTYBIN_MOCKS_VISIT_MINUTES", "30"),
            ("RUSTYBIN_MOCKS_DAILY_HOURS", "2"),
            ("RUSTYBIN_MOCKS_BLOCKED_NAMES", "*paypal*, Login"),
            (
                "RUSTYBIN_LEADS_WEBHOOK_URL",
                "https://hooks.example.com/leads",
            ),
            ("RUSTYBIN_LEADS_CONSENT", "notice"),
        ]);
        let ss = &c.self_service;
        assert!(ss.enabled);
        assert_eq!(ss.host.as_deref(), Some("rustybin-mock.dev"));
        assert_eq!(ss.visit_minutes, 30);
        assert_eq!(ss.daily_hours, 3, "never below the anonymous hours");
        assert_eq!(ss.blocked_names, vec!["*paypal*", "login"]);
        assert_eq!(ss.leads_consent, LeadsConsent::Notice);
        assert!(!ss.leads_enabled(), "no privacy URL");
        assert!(w.iter().any(|w| w.contains("RUSTYBIN_PRIVACY_URL")));
        assert!(w.iter().any(|w| w.contains("RUSTYBIN_MOCKS_DAILY_HOURS")));
        assert_eq!(c.public_view()["mocks_self_service"], true);
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
