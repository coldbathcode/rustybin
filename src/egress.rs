//! The egress policy: one place every outbound connection of the instance
//! goes through (`docs/plans/next/step-01-foundations.md`).
//!
//! - The **allowlist grammar** (host names, `*.domain` wildcards, IPs and
//!   CIDRs, each with an optional port): [`Allowlist`], shared with the
//!   harness (`RUSTYBIN_HARNESS_TARGETS`, which stays its own, stricter list).
//! - **Always refused**: link-local and cloud metadata addresses, the
//!   unspecified, multicast and broadcast addresses, this instance's own
//!   control plane (`/_rustybin`, `/ui`) whatever the host, and loopback
//!   unless `RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true` (local demos) or the
//!   feature's own rule allows it.
//! - **Purposes**: every outbound feature resolves under a [`Purpose`].
//!   `RUSTYBIN_EGRESS_ALLOW` (the `egress.allow` setting) applies to all of
//!   them; a feature adds its own rule when it is configured (the A2A push
//!   allowlist, the leads webhook host, the key registry host, the MCP
//!   registry host). The MCP relay has no default: the operator names the
//!   servers the inspector may relay to.
//! - **Pinning**: a name is resolved once per call, every address is checked,
//!   and the client built for the call connects to those addresses only.
//! - **Audit**: every decision is logged on the `rustybin::egress` target
//!   and kept in a bounded ring, shown by `GET /_rustybin/egress` and the
//!   console's Management, Network tab.

use std::collections::{BTreeMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::settings::{Kind, Settings, Spec};
use crate::state::AppState;

/// Longest allowlist entry accepted.
pub const MAX_ENTRY_LEN: usize = 253;
/// Most addresses kept per target (a huge DNS answer is truncated).
pub const MAX_PINNED_ADDRS: usize = 16;
/// Time allowed for resolving one target.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// One allowlist entry.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    /// `gw.example.com` or `*.example.com` (one or more labels).
    Host {
        name: String,
        wildcard: bool,
        port: Option<u16>,
    },
    /// `10.0.0.0/8`, `192.168.1.5` (a /32), `fd00::/8`.
    Net {
        addr: IpAddr,
        prefix: u8,
        port: Option<u16>,
    },
}

/// The hosts and networks a client node may send load to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Allowlist {
    entries: Vec<Entry>,
}

impl Allowlist {
    /// Parse the entries of an allowlist variable (`RUSTYBIN_EGRESS_ALLOW`,
    /// `RUSTYBIN_HARNESS_TARGETS`). Invalid entries are skipped and reported
    /// (the caller warns), never silently allowed.
    pub fn parse(items: &[String]) -> (Self, Vec<String>) {
        let mut entries = Vec::new();
        let mut warnings = Vec::new();
        for raw in items {
            match parse_entry(raw) {
                Ok(entry) => {
                    if !entries.contains(&entry) {
                        entries.push(entry);
                    }
                }
                Err(why) => warnings.push(format!("ignoring allowlist entry {raw:?}: {why}")),
            }
        }
        (Self { entries }, warnings)
    }

    /// Both lists' entries.
    pub fn merged(&self, other: &Allowlist) -> Allowlist {
        let mut entries = self.entries.clone();
        for e in &other.entries {
            if !entries.contains(e) {
                entries.push(e.clone());
            }
        }
        Allowlist { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Any network entries (a name not listed may still resolve into one).
    pub fn has_networks(&self) -> bool {
        self.entries.iter().any(|e| matches!(e, Entry::Net { .. }))
    }

    /// The entries as written (normalised), for status and reports.
    pub fn describe(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|e| match e {
                Entry::Host {
                    name,
                    wildcard,
                    port,
                } => {
                    let base = if *wildcard {
                        format!("*.{name}")
                    } else {
                        name.clone()
                    };
                    with_port(base, *port)
                }
                Entry::Net { addr, prefix, port } => {
                    let base = match addr {
                        IpAddr::V4(_) if *prefix == 32 => addr.to_string(),
                        IpAddr::V6(_) if *prefix == 128 => format!("[{addr}]"),
                        IpAddr::V4(_) => format!("{addr}/{prefix}"),
                        IpAddr::V6(_) => format!("[{addr}]/{prefix}"),
                    };
                    with_port(base, *port)
                }
            })
            .collect()
    }

    /// Does an entry name this host (a DNS name, lower case) on this port?
    pub fn allows_host(&self, host: &str, port: u16) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.entries.iter().any(|e| match e {
            Entry::Host {
                name,
                wildcard,
                port: p,
            } => {
                port_ok(*p, port)
                    && if *wildcard {
                        host.strip_suffix(name.as_str())
                            .is_some_and(|rest| rest.ends_with('.') && rest.len() > 1)
                    } else {
                        host == *name
                    }
            }
            Entry::Net { .. } => false,
        })
    }

    /// Does a network entry contain this address (on this port)?
    pub fn allows_ip(&self, ip: IpAddr, port: u16) -> bool {
        self.entries.iter().any(|e| match e {
            Entry::Net {
                addr,
                prefix,
                port: p,
            } => port_ok(*p, port) && in_net(ip, *addr, *prefix),
            Entry::Host { .. } => false,
        })
    }
}

fn with_port(base: String, port: Option<u16>) -> String {
    match port {
        Some(p) => format!("{base}:{p}"),
        None => base,
    }
}

fn port_ok(entry: Option<u16>, port: u16) -> bool {
    entry.is_none_or(|p| p == port)
}

/// Is `ip` inside `net/prefix`? Families never mix (an IPv4-mapped IPv6
/// address is compared as IPv4).
pub fn in_net(ip: IpAddr, net: IpAddr, prefix: u8) -> bool {
    let ip = canonical(ip);
    let net = canonical(net);
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(n)) => {
            let prefix = prefix.min(32);
            if prefix == 0 {
                return true;
            }
            let mask = u32::MAX << (32 - prefix);
            (u32::from(a) & mask) == (u32::from(n) & mask)
        }
        (IpAddr::V6(a), IpAddr::V6(n)) => {
            let prefix = prefix.min(128);
            if prefix == 0 {
                return true;
            }
            let mask = u128::MAX << (128 - prefix);
            (u128::from(a) & mask) == (u128::from(n) & mask)
        }
        _ => false,
    }
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => ip,
        },
        v4 => v4,
    }
}

/// `host`, `host:port`, `*.domain`, `ip`, `[ipv6]`, `[ipv6]:port`, `cidr`.
fn parse_entry(raw: &str) -> Result<Entry, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("empty".into());
    }
    if raw.len() > MAX_ENTRY_LEN {
        return Err("too long".into());
    }
    if raw.contains([' ', '\t', '"', '\'', '<', '>', ',']) {
        return Err("unexpected character".into());
    }
    // Bracketed IPv6, optionally with a prefix or port.
    if let Some(rest) = raw.strip_prefix('[') {
        let (inner, after) = rest
            .split_once(']')
            .ok_or_else(|| "unbalanced [".to_string())?;
        let addr: Ipv6Addr = inner
            .parse()
            .map_err(|_| format!("{inner:?} is not an IPv6 address"))?;
        let (prefix, port) = if let Some(p) = after.strip_prefix('/') {
            (parse_prefix(p, 128)?, None)
        } else if let Some(p) = after.strip_prefix(':') {
            (128, Some(parse_port(p)?))
        } else if after.is_empty() {
            (128, None)
        } else {
            return Err(format!("unexpected {after:?} after the address"));
        };
        return Ok(Entry::Net {
            addr: IpAddr::V6(addr),
            prefix,
            port,
        });
    }
    // Bare IPv6 (two or more colons, no brackets, no port).
    if raw.matches(':').count() >= 2 {
        let (addr, prefix) = match raw.split_once('/') {
            Some((a, p)) => (a, parse_prefix(p, 128)?),
            None => (raw, 128),
        };
        let addr: Ipv6Addr = addr
            .parse()
            .map_err(|_| format!("{addr:?} is not an IPv6 address"))?;
        return Ok(Entry::Net {
            addr: IpAddr::V6(addr),
            prefix,
            port: None,
        });
    }
    // CIDR or IPv4 (with an optional port).
    let (main, port) = match raw.rsplit_once(':') {
        Some((m, p)) => (m, Some(parse_port(p)?)),
        None => (raw, None),
    };
    let (host_part, prefix) = match main.split_once('/') {
        Some((h, p)) => (h, Some(p)),
        None => (main, None),
    };
    if let Ok(v4) = host_part.parse::<Ipv4Addr>() {
        let prefix = match prefix {
            Some(p) => parse_prefix(p, 32)?,
            None => 32,
        };
        return Ok(Entry::Net {
            addr: IpAddr::V4(v4),
            prefix,
            port,
        });
    }
    if prefix.is_some() {
        return Err("a prefix length needs an IP address".into());
    }
    let (wildcard, name) = match host_part.strip_prefix("*.") {
        Some(rest) => (true, rest),
        None => (false, host_part),
    };
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    if !valid_hostname(&name) {
        return Err(format!("{host_part:?} is not a host name"));
    }
    Ok(Entry::Host {
        name,
        wildcard,
        port,
    })
}

fn parse_prefix(p: &str, max: u8) -> Result<u8, String> {
    let n: u8 = p
        .parse()
        .map_err(|_| format!("{p:?} is not a prefix length"))?;
    if n > max {
        return Err(format!("prefix length {n} is over {max}"));
    }
    Ok(n)
}

fn parse_port(p: &str) -> Result<u16, String> {
    let n: u16 = p.parse().map_err(|_| format!("{p:?} is not a port"))?;
    if n == 0 {
        return Err("port 0".into());
    }
    Ok(n)
}

/// RFC 1123 host name: labels of letters, digits and hyphens.
pub fn valid_hostname(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ENTRY_LEN
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

/// Why an address may never be a target, whatever the allowlist says.
pub fn forbidden_address(ip: IpAddr) -> Option<&'static str> {
    match canonical(ip) {
        IpAddr::V4(v4) => {
            if v4.is_unspecified() {
                Some("the unspecified address")
            } else if v4.is_broadcast() {
                Some("the broadcast address")
            } else if v4.is_multicast() {
                Some("a multicast address")
            } else if v4 == Ipv4Addr::new(169, 254, 169, 254) {
                Some("the cloud metadata address")
            } else if v4.is_link_local() {
                Some("a link-local address")
            } else if v4.octets()[0] == 0 {
                Some("the 0.0.0.0/8 network")
            } else {
                None
            }
        }
        IpAddr::V6(v6) => {
            if v6.is_unspecified() {
                Some("the unspecified address")
            } else if v6.is_multicast() {
                Some("a multicast address")
            } else if v6 == Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 0x254)
                || v6 == Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254)
            {
                Some("the cloud metadata address")
            } else if (v6.segments()[0] & 0xffc0) == 0xfe80 {
                Some("a link-local address")
            } else {
                None
            }
        }
    }
}

/// A target URL taken apart, before resolution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedTarget {
    /// `http` or `https`.
    pub scheme: String,
    /// The host as written, lower case, without brackets.
    pub host: String,
    /// The host as an IP literal, when it is one.
    pub ip: Option<IpAddr>,
    pub port: u16,
    /// Path prefix without a trailing slash (`""` for the root).
    pub path: String,
}

impl ParsedTarget {
    pub fn tls(&self) -> bool {
        self.scheme == "https"
    }

    /// `scheme://host:port` (the port only when it is not the default).
    pub fn origin(&self) -> String {
        let default = if self.tls() { 443 } else { 80 };
        let host = if self.ip.is_some_and(|ip| ip.is_ipv6()) {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == default {
            format!("{}://{host}", self.scheme)
        } else {
            format!("{}://{host}:{}", self.scheme, self.port)
        }
    }
}

/// Parse and check a target URL: `http` or `https`, a host, no credentials,
/// no query or fragment, and never this node's control plane.
pub fn parse_target_url(url: &str) -> Result<ParsedTarget, String> {
    parse_url(url, false)
}

/// [`parse_target_url`], with a query string allowed when `allow_query` (a
/// webhook or registry URL may carry one; a fragment never makes sense).
pub fn parse_url(url: &str, allow_query: bool) -> Result<ParsedTarget, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("the target URL is empty".into());
    }
    if url.len() > 2048 {
        return Err("the target URL is longer than 2048 characters".into());
    }
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid target URL: {e}"))?;
    let scheme = parsed.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err("the target URL must use http or https".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("the target URL must not contain credentials".into());
    }
    if parsed.fragment().is_some() || (!allow_query && parsed.query().is_some()) {
        return Err("the target URL must not have a query string or fragment".into());
    }
    let host = parsed
        .host_str()
        .ok_or("the target URL has no host")?
        .trim_matches(['[', ']'])
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let ip = host.parse::<IpAddr>().ok();
    if ip.is_none() && !valid_hostname(&host) {
        return Err(format!("{host:?} is not a host name"));
    }
    let port = parsed
        .port_or_known_default()
        .ok_or("the target URL has no port")?;
    let path = parsed.path().trim_end_matches('/').to_string();
    if crate::control::is_control_path(&path) || is_control_prefix(&path) {
        return Err("the target URL points at a Rustybin control plane (/_rustybin, /ui)".into());
    }
    Ok(ParsedTarget {
        scheme,
        host,
        ip,
        port,
        path,
    })
}

/// A path at or under the control plane, however it is written.
pub fn is_control_prefix(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let decoded = lower.replace("%5f", "_");
    crate::control::is_control_path(&decoded)
        || decoded.starts_with("/_rustybin")
        || decoded == "/ui"
        || decoded.starts_with("/ui/")
}

/// Check a target against an allowlist and resolve it to pinned addresses.
/// `allowlist: None` skips the list (a feature allowed to reach anything)
/// but never the forbidden addresses; `var` names the variable in the
/// refusals (`RUSTYBIN_HARNESS_TARGETS`, `RUSTYBIN_EGRESS_ALLOW`).
pub async fn pin_with(
    target: &ParsedTarget,
    allowlist: Option<&Allowlist>,
    var: &str,
) -> Result<Vec<SocketAddr>, String> {
    let any = Allowlist::default();
    let (allowlist, unrestricted) = match allowlist {
        Some(l) => (l, false),
        None => (&any, true),
    };
    if allowlist.is_empty() && !unrestricted {
        return Err(format!(
            "nothing is allowed (set {var} to the hosts, domains, IPs or CIDRs that may be reached)"
        ));
    }
    let addrs: Vec<IpAddr> = match target.ip {
        Some(ip) => vec![ip],
        None => {
            let name_allowed = unrestricted || allowlist.allows_host(&target.host, target.port);
            if !name_allowed && !allowlist.has_networks() {
                return Err(format!("{}:{} is not in {var}", target.host, target.port));
            }
            let lookup = tokio::net::lookup_host((target.host.as_str(), target.port));
            let resolved: Vec<IpAddr> = match tokio::time::timeout(RESOLVE_TIMEOUT, lookup).await
            {
                Err(_) => return Err(format!("resolving {} timed out", target.host)),
                Ok(Err(e)) if name_allowed => {
                    return Err(format!("cannot resolve {}: {e}", target.host))
                }
                Ok(Err(_)) => {
                    return Err(format!(
                        "{}:{} is not in {var} by name, and it does not resolve to addresses that could be",
                        target.host, target.port
                    ))
                }
                Ok(Ok(iter)) => iter.map(|a| a.ip()).take(MAX_PINNED_ADDRS).collect(),
            };
            if resolved.is_empty() {
                return Err(format!("{} resolves to no address", target.host));
            }
            if !name_allowed
                && !resolved
                    .iter()
                    .all(|ip| allowlist.allows_ip(*ip, target.port))
            {
                return Err(format!(
                    "{}:{} is not in {var} (neither the name nor every address it resolves to)",
                    target.host, target.port
                ));
            }
            resolved
        }
    };
    let mut pinned = Vec::with_capacity(addrs.len());
    for ip in addrs {
        if let Some(why) = forbidden_address(ip) {
            return Err(format!(
                "{} resolves to {ip}, {why}, which is never reached",
                target.host
            ));
        }
        if target.ip.is_some() && !unrestricted && !allowlist.allows_ip(ip, target.port) {
            return Err(format!("{ip}:{} is not in {var}", target.port));
        }
        let addr = SocketAddr::new(ip, target.port);
        if !pinned.contains(&addr) {
            pinned.push(addr);
        }
    }
    Ok(pinned)
}

// ── The policy ──────────────────────────────────────────────────────

/// What an outbound call is for. Every feature that reaches out has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// AI assist: the user's model provider (step 03).
    Assist,
    /// The MCP inspector's relay to an external MCP server (step 05).
    McpRelay,
    /// Browsing an MCP registry (step 06).
    Registry,
    /// The git remote of the configuration repository (step 09).
    Git,
    /// A2A push notifications (`RUSTYBIN_A2A_PUSH_ALLOWLIST`).
    A2aPush,
    /// The leads webhook of self-service mocks (`RUSTYBIN_LEADS_WEBHOOK_URL`).
    Leads,
    /// The API key registry URL (`RUSTYBIN_KEYS_URL`).
    KeyRegistry,
}

impl Purpose {
    pub const ALL: [Purpose; 7] = [
        Purpose::Assist,
        Purpose::McpRelay,
        Purpose::Registry,
        Purpose::Git,
        Purpose::A2aPush,
        Purpose::Leads,
        Purpose::KeyRegistry,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Purpose::Assist => "assist",
            Purpose::McpRelay => "mcp_relay",
            Purpose::Registry => "registry",
            Purpose::Git => "git",
            Purpose::A2aPush => "a2a_push",
            Purpose::Leads => "leads",
            Purpose::KeyRegistry => "key_registry",
        }
    }

    /// What the console calls it.
    pub fn title(&self) -> &'static str {
        match self {
            Purpose::Assist => "AI assist provider",
            Purpose::McpRelay => "MCP inspector relay",
            Purpose::Registry => "MCP registry",
            Purpose::Git => "Git remote",
            Purpose::A2aPush => "A2A push notifications",
            Purpose::Leads => "Leads webhook",
            Purpose::KeyRegistry => "API key registry",
        }
    }

    /// What configures the feature's own rule.
    pub fn configured_by(&self) -> &'static str {
        match self {
            Purpose::Assist => "the AI provider's host, once a key is set",
            Purpose::McpRelay => "RUSTYBIN_EGRESS_ALLOW only (no default)",
            Purpose::Registry => "registry.modelcontextprotocol.io by default",
            Purpose::Git => "the git remote's host, once configured",
            Purpose::A2aPush => "RUSTYBIN_A2A_PUSH_ALLOWLIST, RUSTYBIN_A2A_PUSH_ALLOW_ALL",
            Purpose::Leads => "the host of RUSTYBIN_LEADS_WEBHOOK_URL",
            Purpose::KeyRegistry => "the host of RUSTYBIN_KEYS_URL",
        }
    }
}

/// A feature's own rule, on top of `RUSTYBIN_EGRESS_ALLOW`.
#[derive(Clone, Debug, Default)]
pub struct Rule {
    /// Hosts and networks the feature may reach.
    pub allow: Allowlist,
    /// The feature may reach any host (never in public mode).
    pub any: bool,
    /// The feature may reach loopback addresses when its list names them
    /// (features that predate the policy keep their semantics).
    pub loopback: bool,
}

impl Rule {
    /// A rule allowing one URL's host and port.
    pub fn for_url(url: &str) -> Rule {
        let mut rule = Rule::default();
        if let Some(entry) = url_entry(url) {
            let (allow, _) = Allowlist::parse(&[entry]);
            rule.allow = allow;
        }
        rule
    }
}

/// `host:port` (bracketed for IPv6) of a URL, as an allowlist entry.
pub fn url_entry(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    let host = parsed.host_str()?.trim_matches(['[', ']']).to_string();
    let port = parsed.port_or_known_default()?;
    if host.parse::<Ipv6Addr>().is_ok() {
        Some(format!("[{host}]:{port}"))
    } else {
        Some(format!("{host}:{port}"))
    }
}

/// One decision, kept in the ring and shown in the console.
#[derive(Clone, Debug, Serialize)]
pub struct Decision {
    pub at: String,
    pub purpose: Purpose,
    /// `scheme://host:port` (never the path or query: a URL may carry a token).
    pub origin: String,
    pub addrs: Vec<String>,
    pub allowed: bool,
    pub reason: String,
}

/// Most decisions kept.
pub const MAX_DECISIONS: usize = 100;
/// Connect timeout of every outbound client.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest response body an outbound client reads (features may cap lower).
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// A resolved and allowed destination: the client connects to `addrs` only.
#[derive(Clone, Debug)]
pub struct Pinned {
    pub url: String,
    pub target: ParsedTarget,
    pub addrs: Vec<SocketAddr>,
}

/// Why a destination was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal(pub String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

/// The policy of one instance.
pub struct Egress {
    settings: Arc<Settings>,
    public_mode: bool,
    rules: RwLock<BTreeMap<Purpose, Rule>>,
    decisions: Mutex<VecDeque<Decision>>,
}

impl Egress {
    /// The policy from the configuration: the features configured today
    /// get their rules (A2A push, the leads webhook, the key registry, the
    /// MCP registry's default host).
    pub fn new(config: &Config, settings: Arc<Settings>) -> Self {
        let egress = Self {
            settings,
            public_mode: config.public_mode,
            rules: RwLock::new(BTreeMap::new()),
            decisions: Mutex::new(VecDeque::with_capacity(MAX_DECISIONS)),
        };
        let (push_allow, warnings) = Allowlist::parse(&config.a2a_push_allowlist);
        for w in warnings {
            tracing::warn!("RUSTYBIN_A2A_PUSH_ALLOWLIST: {w}");
        }
        egress.permit(
            Purpose::A2aPush,
            Rule {
                allow: push_allow,
                any: config.a2a_push_allow_all && !config.public_mode,
                loopback: true,
            },
        );
        let ss = &config.self_service;
        if ss.enabled && ss.leads_enabled() {
            if let Some(url) = &ss.leads_webhook_url {
                let mut rule = Rule::for_url(url);
                rule.loopback = true;
                egress.permit(Purpose::Leads, rule);
            }
        }
        if let Some(url) = &config.keys_url {
            let mut rule = Rule::for_url(url);
            rule.loopback = true;
            egress.permit(Purpose::KeyRegistry, rule);
        }
        let (registry, _) = Allowlist::parse(&["registry.modelcontextprotocol.io".to_string()]);
        egress.permit(
            Purpose::Registry,
            Rule {
                allow: registry,
                ..Rule::default()
            },
        );
        egress
    }

    /// A policy for tests: loopback allowed, no feature rules.
    pub fn for_tests() -> Arc<Self> {
        let mut config = Config::for_tests();
        config.egress_allow_loopback = true;
        Arc::new(Self::new(&config, Arc::new(Settings::new(&config))))
    }

    pub fn settings(&self) -> &Arc<Settings> {
        &self.settings
    }

    /// Set a feature's rule (replacing the previous one).
    pub fn permit(&self, purpose: Purpose, rule: Rule) {
        self.rules
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(purpose, rule);
    }

    pub fn rule(&self, purpose: Purpose) -> Rule {
        self.rules
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&purpose)
            .cloned()
            .unwrap_or_default()
    }

    /// The global allowlist (`egress.allow`: stored, else `RUSTYBIN_EGRESS_ALLOW`).
    pub fn allowlist(&self) -> (Allowlist, Vec<String>) {
        Allowlist::parse(&self.settings.list("egress.allow"))
    }

    pub fn loopback_allowed(&self) -> bool {
        self.settings.bool("egress.allow_loopback")
    }

    /// Check and resolve a destination for `purpose`.
    pub async fn resolve(&self, url: &str, purpose: Purpose) -> Result<Pinned, Refusal> {
        let target = match parse_url(url, true) {
            Ok(t) => t,
            Err(e) => {
                self.record(purpose, url_origin(url), Vec::new(), Err(&e));
                return Err(Refusal(e));
            }
        };
        let origin = target.origin();
        let rule = self.rule(purpose);
        let (global, _) = self.allowlist();
        let merged = global.merged(&rule.allow);
        let unrestricted = rule.any && !self.public_mode;
        let pinned = pin_with(
            &target,
            if unrestricted { None } else { Some(&merged) },
            "RUSTYBIN_EGRESS_ALLOW",
        )
        .await;
        let addrs = match pinned {
            Ok(a) => a,
            Err(e) => {
                let e = format!(
                    "{e} (purpose {}: {})",
                    purpose.as_str(),
                    purpose.configured_by()
                );
                self.record(purpose, origin, Vec::new(), Err(&e));
                return Err(Refusal(e));
            }
        };
        if !(self.loopback_allowed() || rule.loopback) {
            if let Some(lo) = addrs.iter().find(|a| canonical(a.ip()).is_loopback()) {
                let e = format!(
                    "{} resolves to the loopback address {}: set RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true (or the egress.allow_loopback setting) for local targets",
                    target.host,
                    lo.ip()
                );
                self.record(purpose, origin, Vec::new(), Err(&e));
                return Err(Refusal(e));
            }
        }
        self.record(purpose, origin, addrs.clone(), Ok(()));
        Ok(Pinned {
            url: url.trim().to_string(),
            target,
            addrs,
        })
    }

    /// A client for one resolved destination: pinned addresses, no redirects
    /// (a redirect could leave the allowed host), the TLS roots, timeouts.
    pub fn client(&self, pinned: &Pinned, timeout: Duration) -> Result<reqwest::Client, String> {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(concat!("rustybin/", env!("CARGO_PKG_VERSION")));
        match tls_config() {
            Ok(tls) => builder = builder.use_preconfigured_tls(tls),
            Err(e) => tracing::warn!("egress: https unavailable ({e})"),
        }
        if pinned.target.ip.is_none() {
            builder = builder.resolve_to_addrs(&pinned.target.host, &pinned.addrs);
        }
        builder.build().map_err(|e| e.to_string())
    }

    fn record(
        &self,
        purpose: Purpose,
        origin: String,
        addrs: Vec<SocketAddr>,
        outcome: Result<(), &str>,
    ) {
        let (allowed, reason) = match outcome {
            Ok(()) => (true, "allowed".to_string()),
            Err(e) => (false, e.to_string()),
        };
        let addrs: Vec<String> = addrs.iter().map(|a| a.to_string()).collect();
        if allowed {
            tracing::info!(
                target: "rustybin::egress",
                purpose = purpose.as_str(),
                %origin,
                addrs = %addrs.join(","),
                "egress allowed"
            );
        } else {
            tracing::warn!(
                target: "rustybin::egress",
                purpose = purpose.as_str(),
                %origin,
                "egress refused: {reason}"
            );
        }
        let mut ring = self.decisions.lock().unwrap_or_else(|e| e.into_inner());
        if ring.len() >= MAX_DECISIONS {
            ring.pop_front();
        }
        ring.push_back(Decision {
            at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            purpose,
            origin,
            addrs,
            allowed,
            reason,
        });
    }

    /// The last decisions, newest first.
    pub fn decisions(&self) -> Vec<Decision> {
        self.decisions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .rev()
            .cloned()
            .collect()
    }

    /// The policy as the instance sees it (`GET /_rustybin/egress`).
    pub fn view(&self) -> Value {
        let (allow, warnings) = self.allowlist();
        let (_, allow_source) = self.settings.get("egress.allow");
        let (_, loopback_source) = self.settings.get("egress.allow_loopback");
        let rules = self.rules.read().unwrap_or_else(|e| e.into_inner());
        let purposes: Vec<Value> = Purpose::ALL
            .iter()
            .map(|p| {
                let rule = rules.get(p).cloned().unwrap_or_default();
                json!({
                    "purpose": p.as_str(),
                    "title": p.title(),
                    "configured_by": p.configured_by(),
                    "allow": rule.allow.describe(),
                    "any": rule.any && !self.public_mode,
                    "loopback": rule.loopback,
                })
            })
            .collect();
        drop(rules);
        json!({
            "allow": allow.describe(),
            "allow_source": allow_source.as_str(),
            "allow_loopback": self.loopback_allowed(),
            "allow_loopback_source": loopback_source.as_str(),
            "public_mode": self.public_mode,
            "warnings": warnings,
            "always_refused": [
                "link-local addresses", "cloud metadata addresses", "the unspecified address",
                "multicast and broadcast addresses", "this instance's control plane (/_rustybin, /ui)",
                "loopback unless allowed",
            ],
            "purposes": purposes,
            "decisions": self.decisions(),
        })
    }
}

fn url_origin(url: &str) -> String {
    reqwest::Url::parse(url.trim())
        .ok()
        .and_then(|u| {
            let host = u.host_str()?.to_string();
            Some(match u.port() {
                Some(p) => format!("{}://{host}:{p}", u.scheme()),
                None => format!("{}://{host}", u.scheme()),
            })
        })
        .unwrap_or_else(|| "(invalid URL)".to_string())
}

/// HTTPS client configuration (public web roots, explicit crypto provider).
/// The process may have several rustls providers compiled in, so nothing
/// relies on a process-wide default.
pub fn tls_config() -> Result<rustls::ClientConfig, rustls::Error> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth())
}

// ── Settings ────────────────────────────────────────────────────────

fn valid_allowlist(value: &Value) -> Result<(), String> {
    let items: Vec<String> = value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let (_, warnings) = Allowlist::parse(&items);
    if warnings.is_empty() {
        Ok(())
    } else {
        Err(warnings.join("; "))
    }
}

/// The policy's settings (`egress.allow`, `egress.allow_loopback`).
pub fn specs() -> Vec<Spec> {
    vec![
        Spec::new(
            "egress.allow",
            "egress",
            "Allowed destinations",
            "Hosts, *.domain wildcards, IPs and CIDRs (each with an optional port) every outbound feature may reach, on top of each feature's own rule.",
            Kind::List {
                max_items: 200,
                max_len: MAX_ENTRY_LEN,
            },
            json!([]),
        )
        .env("RUSTYBIN_EGRESS_ALLOW", |c| {
            (!c.egress_allow.is_empty()).then(|| json!(c.egress_allow))
        })
        .check(valid_allowlist),
        Spec::new(
            "egress.allow_loopback",
            "egress",
            "Allow loopback",
            "Let outbound calls reach 127.0.0.1, ::1 and localhost (a mock MCP server or webhook on this machine). Off for anything shared.",
            Kind::Bool,
            json!(false),
        )
        .env("RUSTYBIN_EGRESS_ALLOW_LOOPBACK", |c| {
            c.egress_allow_loopback.then_some(json!(true))
        }),
    ]
}

// ── Route ───────────────────────────────────────────────────────────

async fn egress_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    Json(state.egress.view()).into_response()
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new().route("/_rustybin/egress", get(egress_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![Endpoint::new(
        "/_rustybin/egress",
        &["GET"],
        category::CONTROL,
        "The outbound connection policy: the allowlist, each feature's rule and the last 100 decisions",
    )
    .description(
        "Admin. `RUSTYBIN_EGRESS_ALLOW` (the `egress.allow` setting) applies to every outbound \
         feature; each feature adds its own rule when configured. Link-local, metadata, multicast \
         and unspecified addresses, this instance's control plane and (unless allowed) loopback \
         are always refused. Decisions carry the origin, the pinned addresses and the reason.",
    )
    .example(Example::get("Egress policy", "/_rustybin/egress"))]
}

pub fn openapi_paths() -> Value {
    json!({
        "/_rustybin/egress": {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Outbound connection policy",
                "description": "The allowlist as the instance sees it, where it comes from, each feature's own rule, and the last 100 decisions (newest first). Admin.",
                "operationId": "getRustybinEgress",
                "responses": {
                    "200": { "description": "Policy", "content": { "application/json": { "schema": {
                        "type": "object",
                        "properties": {
                            "allow": { "type": "array", "items": { "type": "string" } },
                            "allow_source": { "type": "string", "enum": ["stored", "env", "default"] },
                            "allow_loopback": { "type": "boolean" },
                            "allow_loopback_source": { "type": "string", "enum": ["stored", "env", "default"] },
                            "public_mode": { "type": "boolean" },
                            "warnings": { "type": "array", "items": { "type": "string" } },
                            "always_refused": { "type": "array", "items": { "type": "string" } },
                            "purposes": { "type": "array", "items": { "type": "object", "properties": {
                                "purpose": { "type": "string" }, "title": { "type": "string" }, "configured_by": { "type": "string" },
                                "allow": { "type": "array", "items": { "type": "string" } }, "any": { "type": "boolean" }, "loopback": { "type": "boolean" } } } },
                            "decisions": { "type": "array", "items": { "type": "object", "properties": {
                                "at": { "type": "string", "format": "date-time" }, "purpose": { "type": "string" }, "origin": { "type": "string" },
                                "addrs": { "type": "array", "items": { "type": "string" } }, "allowed": { "type": "boolean" }, "reason": { "type": "string" } } } }
                        }
                    } } } },
                    "401": { "description": "Admin token required" }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_json, get_request, test_app_with, test_state_with};
    use tower::ServiceExt;

    fn policy(allow: &[&str], loopback: bool, public: bool) -> Egress {
        let mut config = Config::for_tests();
        config.egress_allow = allow.iter().map(|s| s.to_string()).collect();
        config.egress_allow_loopback = loopback;
        config.public_mode = public;
        Egress::new(&config, Arc::new(Settings::new(&config)))
    }

    #[tokio::test]
    async fn the_global_list_and_loopback_switch() {
        let e = policy(&["127.0.0.1"], false, false);
        let err = e
            .resolve("http://127.0.0.1:9/x", Purpose::Assist)
            .await
            .expect_err("loopback refused");
        assert!(err.0.contains("RUSTYBIN_EGRESS_ALLOW_LOOPBACK"), "{err}");
        let e = policy(&["127.0.0.1"], true, false);
        let p = e
            .resolve("http://127.0.0.1:9/x?token=t", Purpose::Assist)
            .await
            .expect("allowed");
        assert_eq!(p.addrs, vec!["127.0.0.1:9".parse().expect("addr")]);
        let err = e
            .resolve("http://10.9.9.9/x", Purpose::Assist)
            .await
            .expect_err("not listed");
        assert!(err.0.contains("not in RUSTYBIN_EGRESS_ALLOW"), "{err}");
        assert!(err.0.contains("purpose assist"), "{err}");
        let d = e.decisions();
        assert_eq!(d.len(), 2, "newest first");
        assert_eq!(d[0].origin, "http://10.9.9.9");
        assert!(!d[0].allowed);
        assert_eq!(d[1].origin, "http://127.0.0.1:9", "no path or query kept");
        assert!(d[1].allowed);
        assert_eq!(d[1].addrs, vec!["127.0.0.1:9"]);
    }

    #[tokio::test]
    async fn nothing_allowed_refuses_and_names_the_variable() {
        let e = policy(&[], true, false);
        let err = e
            .resolve("https://api.example.com/v1", Purpose::McpRelay)
            .await
            .expect_err("empty");
        assert!(err.0.contains("set RUSTYBIN_EGRESS_ALLOW"), "{err}");
        let err = e
            .resolve("http://169.254.169.254/latest", Purpose::McpRelay)
            .await
            .expect_err("metadata");
        assert!(err.0.contains("RUSTYBIN_EGRESS_ALLOW"), "{err}");
        let err = e
            .resolve("http://127.0.0.1:8080/_rustybin/config", Purpose::McpRelay)
            .await
            .expect_err("control plane");
        assert!(err.0.contains("control plane"), "{err}");
        let err = e
            .resolve("http://user:pw@127.0.0.1/x", Purpose::McpRelay)
            .await
            .expect_err("credentials");
        assert!(err.0.contains("credentials"), "{err}");
    }

    #[tokio::test]
    async fn feature_rules_add_to_the_global_list_and_any_never_in_public_mode() {
        let e = policy(&[], false, false);
        // A legacy feature: its own list allows loopback.
        e.permit(
            Purpose::Leads,
            Rule {
                allow: Allowlist::parse(&["127.0.0.1:7777".into()]).0,
                any: false,
                loopback: true,
            },
        );
        assert!(e
            .resolve("http://127.0.0.1:7777/hook", Purpose::Leads)
            .await
            .is_ok());
        assert!(e
            .resolve("http://127.0.0.1:7778/hook", Purpose::Leads)
            .await
            .is_err());
        assert!(
            e.resolve("http://127.0.0.1:7777/hook", Purpose::Assist)
                .await
                .is_err(),
            "a rule is per purpose"
        );
        // `any` skips the lists but not the forbidden addresses.
        e.permit(
            Purpose::A2aPush,
            Rule {
                any: true,
                loopback: true,
                ..Rule::default()
            },
        );
        assert!(e
            .resolve("http://127.0.0.1:1/x", Purpose::A2aPush)
            .await
            .is_ok());
        let err = e
            .resolve("http://169.254.169.254/x", Purpose::A2aPush)
            .await
            .expect_err("metadata");
        assert!(err.0.contains("metadata"), "{err}");
        let public = policy(&[], false, true);
        public.permit(
            Purpose::A2aPush,
            Rule {
                any: true,
                loopback: true,
                ..Rule::default()
            },
        );
        assert!(public
            .resolve("http://127.0.0.1:1/x", Purpose::A2aPush)
            .await
            .is_err());
        assert_eq!(public.view()["purposes"][4]["any"], false);
    }

    #[test]
    fn the_configured_features_get_their_rules() {
        let mut config = Config::for_tests();
        config.a2a_push_allowlist = vec!["hooks.example.com".into(), "bad entry".into()];
        config.keys_url = Some("https://keys.example.net:8443/registry?x=1".into());
        config.self_service.enabled = true;
        config.self_service.leads_webhook_url = Some("http://[::1]:9000/leads".into());
        config.self_service.privacy_url = Some("https://example.com/p".into());
        let e = Egress::new(&config, Arc::new(Settings::new(&config)));
        assert_eq!(
            e.rule(Purpose::A2aPush).allow.describe(),
            vec!["hooks.example.com"]
        );
        assert_eq!(
            e.rule(Purpose::KeyRegistry).allow.describe(),
            vec!["keys.example.net:8443"]
        );
        assert_eq!(e.rule(Purpose::Leads).allow.describe(), vec!["[::1]:9000"]);
        assert!(e.rule(Purpose::Leads).loopback);
        assert_eq!(
            e.rule(Purpose::Registry).allow.describe(),
            vec!["registry.modelcontextprotocol.io"]
        );
        assert!(e.rule(Purpose::McpRelay).allow.is_empty());
        assert_eq!(
            url_entry("http://example.org/x"),
            Some("example.org:80".into())
        );
        assert_eq!(url_entry("nope"), None);
    }

    #[tokio::test]
    async fn the_setting_overrides_the_environment_and_is_validated() {
        let e = policy(&["env.example.com"], false, false);
        assert_eq!(e.view()["allow"], json!(["env.example.com"]));
        assert_eq!(e.view()["allow_source"], "env");
        e.settings()
            .update(
                [(
                    "egress.allow".to_string(),
                    json!(["stored.example.com:443"]),
                )]
                .into_iter()
                .collect(),
                Default::default(),
                "test",
            )
            .expect("set");
        assert_eq!(e.view()["allow"], json!(["stored.example.com:443"]));
        assert_eq!(e.view()["allow_source"], "stored");
        let err = e
            .settings()
            .update(
                [("egress.allow".to_string(), json!(["not a host"]))]
                    .into_iter()
                    .collect(),
                Default::default(),
                "test",
            )
            .expect_err("invalid");
        assert!(format!("{err:?}").contains("not a host"));
    }

    #[tokio::test]
    async fn the_ring_is_bounded() {
        let e = policy(&[], false, false);
        for i in 0..(MAX_DECISIONS + 20) {
            let _ = e
                .resolve(&format!("http://10.0.0.{}/x", i % 250), Purpose::Git)
                .await;
        }
        assert_eq!(e.decisions().len(), MAX_DECISIONS);
    }

    #[test]
    fn a_pinned_client_builds() {
        let e = policy(&["example.com"], false, false);
        let pinned = Pinned {
            url: "https://example.com/".into(),
            target: parse_target_url("https://example.com/").expect("target"),
            addrs: vec!["192.0.2.1:443".parse().expect("addr")],
        };
        assert!(e.client(&pinned, Duration::from_secs(5)).is_ok());
    }

    #[tokio::test]
    async fn the_route_needs_the_admin_token() {
        let mut config = Config::for_tests();
        config.admin_token = Some("adm".into());
        config.egress_allow = vec!["*.example.com".into()];
        let app = test_app_with(test_state_with(config));
        let resp = app
            .clone()
            .oneshot(get_request("/_rustybin/egress"))
            .await
            .expect("resp");
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
        let mut req = get_request("/_rustybin/egress");
        req.headers_mut().insert(
            "x-rustybin-admin-token",
            axum::http::HeaderValue::from_static("adm"),
        );
        let v = body_json(app.oneshot(req).await.expect("resp")).await;
        assert_eq!(v["allow"], json!(["*.example.com"]));
        assert_eq!(
            v["purposes"].as_array().expect("purposes").len(),
            Purpose::ALL.len()
        );
    }
}
