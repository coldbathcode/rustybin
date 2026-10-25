//! The fences around load generation (`docs/plans/harness.md`, section 11).
//!
//! They come first because harness mode is the first feature that sends load
//! to targets a caller chooses:
//!
//! - the **target allowlist** (`RUSTYBIN_HARNESS_TARGETS`): host names,
//!   wildcard domains, IP addresses and CIDRs, each with an optional port.
//!   Nothing is allowed without it;
//! - **address pinning**: a target's name is resolved once per run, every
//!   address is checked, and the connections of the run go to those
//!   addresses only (no DNS rebinding in the middle of a run);
//! - **always refused**: link-local and cloud metadata addresses, the
//!   unspecified, multicast and broadcast addresses, and this node's own
//!   control plane (`/_rustybin/*`, `/ui`) whatever the host;
//! - the **kill switch**: one flag that stops every run on the node;
//! - **target verification** (`RUSTYBIN_HARNESS_VERIFY_TARGETS`): before a
//!   run every target must serve the node's verification token at
//!   [`WELL_KNOWN_PATH`](super::WELL_KNOWN_PATH). A node serves it itself,
//!   so a gateway that routes to this node passes, which also proves the
//!   route works before a single load request is sent.
//!
//! The caps (rate, in flight, duration) are enforced by scenario validation
//! (`super::scenario::Caps`) and by the engine.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

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
    /// Parse the entries of `RUSTYBIN_HARNESS_TARGETS`. Invalid entries are
    /// skipped and reported (the caller warns), never silently allowed.
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
                Err(why) => warnings.push(format!("ignoring harness target {raw:?}: {why}")),
            }
        }
        (Self { entries }, warnings)
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
    if parsed.query().is_some() || parsed.fragment().is_some() {
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

/// Check a target against the allowlist and resolve it to pinned addresses.
pub async fn pin(target: &ParsedTarget, allowlist: &Allowlist) -> Result<Vec<SocketAddr>, String> {
    if allowlist.is_empty() {
        return Err(
            "no targets are allowed on this node (set RUSTYBIN_HARNESS_TARGETS to the hosts, \
             domains, IPs or CIDRs the harness may send load to)"
                .into(),
        );
    }
    let addrs: Vec<IpAddr> = match target.ip {
        Some(ip) => vec![ip],
        None => {
            let name_allowed = allowlist.allows_host(&target.host, target.port);
            if !name_allowed && !allowlist.has_networks() {
                return Err(format!(
                    "{}:{} is not in RUSTYBIN_HARNESS_TARGETS",
                    target.host, target.port
                ));
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
                        "{}:{} is not in RUSTYBIN_HARNESS_TARGETS by name, and it does not resolve to addresses that could be",
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
                    "{}:{} is not in RUSTYBIN_HARNESS_TARGETS (neither the name nor every address it resolves to)",
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
                "{} resolves to {ip}, {why}, which is never a harness target",
                target.host
            ));
        }
        if target.ip.is_some() && !allowlist.allows_ip(ip, target.port) {
            return Err(format!(
                "{ip}:{} is not in RUSTYBIN_HARNESS_TARGETS",
                target.port
            ));
        }
        let addr = SocketAddr::new(ip, target.port);
        if !pinned.contains(&addr) {
            pinned.push(addr);
        }
    }
    Ok(pinned)
}

/// The kill switch: stops every run on the node. Runs poll [`KillSwitch::is_set`]
/// and the engine's scheduler wakes on [`KillSwitch::notified`].
#[derive(Default)]
pub struct KillSwitch {
    set: AtomicBool,
    notify: Notify,
}

impl KillSwitch {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn is_set(&self) -> bool {
        self.set.load(Ordering::SeqCst)
    }

    pub fn set(&self) {
        self.set.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Clear after every run stopped, so the next run may start.
    pub fn clear(&self) {
        self.set.store(false, Ordering::SeqCst);
    }

    pub async fn notified(&self) {
        self.notify.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Allowlist {
        let (l, warnings) =
            Allowlist::parse(&items.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(warnings.is_empty(), "{warnings:?}");
        l
    }

    #[test]
    fn hosts_wildcards_ports() {
        let l = list(&["gw.example.com", "*.internal", "api.acme.io:8443"]);
        assert!(l.allows_host("gw.example.com", 443));
        assert!(l.allows_host("GW.EXAMPLE.COM.", 80));
        assert!(!l.allows_host("evil-gw.example.com", 443));
        assert!(l.allows_host("a.internal", 80));
        assert!(l.allows_host("a.b.internal", 80));
        assert!(!l.allows_host("internal", 80));
        assert!(!l.allows_host("xinternal", 80));
        assert!(l.allows_host("api.acme.io", 8443));
        assert!(!l.allows_host("api.acme.io", 443));
        assert!(!l.allows_ip("10.0.0.1".parse().unwrap(), 80));
    }

    #[test]
    fn networks_and_literals() {
        let l = list(&["10.0.0.0/8", "192.168.1.5", "[fd12::]/16", "127.0.0.1:9000"]);
        assert!(l.allows_ip("10.200.3.4".parse().unwrap(), 80));
        assert!(!l.allows_ip("11.0.0.1".parse().unwrap(), 80));
        assert!(l.allows_ip("192.168.1.5".parse().unwrap(), 443));
        assert!(!l.allows_ip("192.168.1.6".parse().unwrap(), 443));
        assert!(l.allows_ip("fd12:3::1".parse().unwrap(), 80));
        assert!(!l.allows_ip("fd13::1".parse().unwrap(), 80));
        assert!(l.allows_ip("127.0.0.1".parse().unwrap(), 9000));
        assert!(!l.allows_ip("127.0.0.1".parse().unwrap(), 9001));
        // IPv4-mapped IPv6 compares as IPv4.
        assert!(l.allows_ip("::ffff:10.1.2.3".parse().unwrap(), 80));
        assert_eq!(
            l.describe(),
            vec!["10.0.0.0/8", "192.168.1.5", "[fd12::]/16", "127.0.0.1:9000"]
        );
    }

    #[test]
    fn invalid_entries_are_reported_not_allowed() {
        let (l, warnings) = Allowlist::parse(&[
            "".into(),
            "bad host".into(),
            "10.0.0.0/40".into(),
            "host:0".into(),
            "ok.example".into(),
        ]);
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        assert_eq!(l.describe(), vec!["ok.example"]);
    }

    #[test]
    fn forbidden_addresses() {
        for (ip, why) in [
            ("169.254.169.254", "metadata"),
            ("169.254.1.1", "link-local"),
            ("0.0.0.0", "unspecified"),
            ("224.0.0.1", "multicast"),
            ("255.255.255.255", "broadcast"),
            ("fe80::1", "link-local"),
            ("fd00:ec2::254", "metadata"),
            ("::", "unspecified"),
            ("ff02::1", "multicast"),
            ("::ffff:169.254.169.254", "metadata"),
        ] {
            let got = forbidden_address(ip.parse().unwrap()).unwrap_or("allowed");
            assert!(got.contains(why), "{ip}: {got}");
        }
        for ip in ["127.0.0.1", "10.0.0.1", "8.8.8.8", "::1", "2001:db8::1"] {
            assert_eq!(forbidden_address(ip.parse().unwrap()), None, "{ip}");
        }
    }

    #[test]
    fn target_urls() {
        let t = parse_target_url("https://GW.example.com/api/").unwrap();
        assert_eq!(
            (t.scheme.as_str(), t.host.as_str(), t.port, t.path.as_str()),
            ("https", "gw.example.com", 443, "/api")
        );
        assert_eq!(t.origin(), "https://gw.example.com");
        let t = parse_target_url("http://127.0.0.1:8080").unwrap();
        assert_eq!(t.ip, Some("127.0.0.1".parse().unwrap()));
        assert_eq!(t.origin(), "http://127.0.0.1:8080");
        let t = parse_target_url("http://[::1]:8080/x").unwrap();
        assert_eq!(t.origin(), "http://[::1]:8080");
        for bad in [
            "ftp://x",
            "http://user:pw@gw",
            "http://gw/?a=1",
            "http://gw/#f",
            "http://gw/_rustybin",
            "http://gw/ui/",
            "http://gw/%5Frustybin/x",
            "not a url",
            "",
        ] {
            assert!(parse_target_url(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn pinning_checks_names_literals_and_forbidden_addresses() {
        let l = list(&["localhost", "10.0.0.0/8", "127.0.0.1:9000"]);
        let t = parse_target_url("http://localhost:8080").unwrap();
        let pinned = pin(&t, &l).await.unwrap();
        assert!(!pinned.is_empty());
        assert!(pinned
            .iter()
            .all(|a| a.ip().is_loopback() && a.port() == 8080));
        // A literal must be in a network entry (with its port).
        let t = parse_target_url("http://127.0.0.1:9000").unwrap();
        assert_eq!(
            pin(&t, &l).await.unwrap(),
            vec!["127.0.0.1:9000".parse().unwrap()]
        );
        let t = parse_target_url("http://127.0.0.1:9001").unwrap();
        assert!(pin(&t, &l)
            .await
            .unwrap_err()
            .contains("not in RUSTYBIN_HARNESS_TARGETS"));
        let t = parse_target_url("http://10.1.2.3").unwrap();
        assert_eq!(
            pin(&t, &l).await.unwrap(),
            vec!["10.1.2.3:80".parse().unwrap()]
        );
        // Forbidden, even when a network entry would contain it.
        let l2 = list(&["169.254.0.0/16", "0.0.0.0/0"]);
        let t = parse_target_url("http://169.254.169.254/latest").unwrap();
        assert!(pin(&t, &l2).await.unwrap_err().contains("metadata"));
        let t = parse_target_url("http://224.0.0.1").unwrap();
        assert!(pin(&t, &l2).await.unwrap_err().contains("multicast"));
        // Nothing without an allowlist.
        let t = parse_target_url("http://localhost").unwrap();
        assert!(pin(&t, &Allowlist::default())
            .await
            .unwrap_err()
            .contains("RUSTYBIN_HARNESS_TARGETS"));
        // Unknown names are refused by name before any address check.
        let t = parse_target_url("http://gw.example.com").unwrap();
        let l3 = list(&["other.example.com"]);
        let err = pin(&t, &l3).await.unwrap_err();
        assert!(err.contains("not in RUSTYBIN_HARNESS_TARGETS"), "{err}");
    }

    #[test]
    fn kill_switch() {
        let k = KillSwitch::new();
        assert!(!k.is_set());
        k.set();
        assert!(k.is_set());
        k.clear();
        assert!(!k.is_set());
    }
}
