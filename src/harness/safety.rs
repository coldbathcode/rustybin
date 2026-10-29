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

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

// The allowlist grammar, the forbidden addresses, URL parsing and pinning
// are the egress policy's (`crate::egress`, step 01 of the 0.2 plan); the
// harness keeps its own list (`RUSTYBIN_HARNESS_TARGETS`) and verification.
pub use crate::egress::{
    forbidden_address, in_net, is_control_prefix, parse_target_url, pin_with, valid_hostname,
    Allowlist, ParsedTarget, MAX_ENTRY_LEN, MAX_PINNED_ADDRS, RESOLVE_TIMEOUT,
};

/// Check a target against the harness allowlist and resolve it to pinned
/// addresses ([`pin_with`] naming `RUSTYBIN_HARNESS_TARGETS`).
pub async fn pin(target: &ParsedTarget, allowlist: &Allowlist) -> Result<Vec<SocketAddr>, String> {
    pin_with(target, Some(allowlist), "RUSTYBIN_HARNESS_TARGETS").await
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

/// The dead man's switch of a cluster run (plan section 9): a client keeps
/// sending only while its leader answers heartbeats. Every answer renews
/// the lease; a run whose lease expired stops (`lease_expired`), so a lost
/// leader, a partition or a dead coordinator never leaves a client sending.
pub struct Lease {
    period: Duration,
    renewed: std::sync::Mutex<std::time::Instant>,
}

impl Lease {
    pub fn new(period: Duration) -> Arc<Self> {
        Arc::new(Self {
            period,
            renewed: std::sync::Mutex::new(std::time::Instant::now()),
        })
    }

    pub fn period(&self) -> Duration {
        self.period
    }

    pub fn renew(&self) {
        *self.renewed.lock().unwrap_or_else(|p| p.into_inner()) = std::time::Instant::now();
    }

    /// Time since the last renewal.
    pub fn age(&self) -> Duration {
        self.renewed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .elapsed()
    }

    pub fn expired(&self) -> bool {
        self.age() > self.period
    }
}

/// Check a coordinator URL before joining: `http` or `https`, no
/// credentials, and never a host that resolves to a forbidden address
/// (link-local, cloud metadata, unspecified, multicast). Joining sends a few
/// small requests to the URL a controller names, never to those places.
pub async fn check_join_url(url: &str) -> Result<(), String> {
    if url.len() > 2048 {
        return Err("the join URL is longer than 2048 characters".into());
    }
    let parsed = reqwest::Url::parse(url.trim()).map_err(|e| format!("invalid join URL: {e}"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("the join URL must use http or https".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("the join URL must not contain credentials".into());
    }
    let host = parsed
        .host_str()
        .ok_or("the join URL has no host")?
        .trim_matches(['[', ']'])
        .to_string();
    let port = parsed
        .port_or_known_default()
        .ok_or("the join URL has no port")?;
    let resolved = tokio::time::timeout(
        RESOLVE_TIMEOUT,
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    .map_err(|_| format!("resolving {host} timed out"))?
    .map_err(|e| format!("cannot resolve {host}: {e}"))?;
    let mut any = false;
    for addr in resolved.take(MAX_PINNED_ADDRS) {
        any = true;
        if let Some(why) = forbidden_address(addr.ip()) {
            return Err(format!(
                "{host} resolves to {} ({why}): never a coordinator",
                addr.ip()
            ));
        }
    }
    if !any {
        return Err(format!("{host} does not resolve"));
    }
    Ok(())
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
