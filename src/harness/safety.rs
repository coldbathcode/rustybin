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
///
/// Every press counts a generation. The switch clears once a whole hold
/// passes with no new press (one clearing thread at most, however often it
/// is pressed), and a start that is still being prepared compares the
/// generation it began under ([`KillSwitch::generation`]), so a press
/// during its preparation stops it even after the switch has cleared.
#[derive(Default)]
pub struct KillSwitch {
    set: AtomicBool,
    notify: Notify,
    /// The generation (presses so far) and whether a clearer is waiting.
    presses: std::sync::Mutex<(u64, bool)>,
}

impl KillSwitch {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn is_set(&self) -> bool {
        self.set.load(Ordering::SeqCst)
    }

    /// Presses so far: a start records it before its first await and
    /// refuses to start when it changed.
    pub fn generation(&self) -> u64 {
        self.presses.lock().unwrap_or_else(|p| p.into_inner()).0
    }

    pub fn set(&self) {
        {
            let mut presses = self.presses.lock().unwrap_or_else(|p| p.into_inner());
            presses.0 = presses.0.wrapping_add(1);
            self.set.store(true, Ordering::SeqCst);
        }
        self.notify.notify_waiters();
    }

    /// Press, and clear once `hold` passes without another press, so the
    /// next run may start. A press while a clearer waits extends the hold
    /// (no second thread).
    pub fn press_for(self: &Arc<Self>, hold: Duration) {
        self.set();
        {
            let mut presses = self.presses.lock().unwrap_or_else(|p| p.into_inner());
            if presses.1 {
                return;
            }
            presses.1 = true;
        }
        let me = self.clone();
        let spawned = std::thread::Builder::new()
            .name("harness-kill".into())
            .spawn(move || me.clear_when_quiet(hold));
        if spawned.is_err() {
            // No thread: the switch stays set until the next press finds
            // one (runs refuse to start meanwhile, the safe side).
            self.presses.lock().unwrap_or_else(|p| p.into_inner()).1 = false;
        }
    }

    fn clear_when_quiet(&self, hold: Duration) {
        let mut seen = self.generation();
        loop {
            std::thread::sleep(hold);
            let mut presses = self.presses.lock().unwrap_or_else(|p| p.into_inner());
            if presses.0 == seen {
                self.set.store(false, Ordering::SeqCst);
                presses.1 = false;
                return;
            }
            seen = presses.0;
        }
    }

    /// Clear now (tests).
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

// ── Server-to-server chains ─────────────────────────────────────────
//
// A chain is the one case where the server role sends: a server answering
// a request of a run calls a further hop the scenario names. The rules
// (step 13 of `docs/plans/core-next`), each enforced here or named where:
//
//  1. off unless the run's `servers.chain` names hops;
//  2. only for a request whose marker verified with the run key (the
//     correlation layer calls [`ChainGate::admit`] after `Marker::verify`,
//     never before);
//  3. only while the run is live ([`ChainGate::live`]: the run did not end
//     and its liveness was renewed, as a client's lease is);
//  4. only to `hops[n]`, `n` being the hop count of the verified marker:
//     nothing of the incoming request chooses or reaches the destination;
//  5. through the pinned, allowlisted path: [`pin_chain`] parses every hop
//     like a target, checks it against this node's allowlist and pins its
//     addresses, whole or not at all; the caller builds the engine's
//     pinned client on those addresses and verifies each hop like a target;
//  6. the hop count is inside the marker's signature;
//  7. a fixed depth, [`MAX_CHAIN_HOPS`];
//  8. at most one onward call per client request and hop: a repeat (a
//     gateway's retry, a mirrored or replayed request) calls nothing, and
//     so does a request the bitmap cannot track;
//  9. bounded: [`ChainGate`]'s permits (no queue) and a timeout per call;
// 10. the kill switch refuses at the gate and ends a call under way.

/// The depth of a chain: hops after the server a client's request reaches.
pub const MAX_CHAIN_HOPS: usize = 3;
/// Onward calls in flight per run on a node: the default and the most.
pub const DEFAULT_CHAIN_CONCURRENCY: u32 = 64;
pub const MAX_CHAIN_CONCURRENCY: u32 = 1024;
/// One onward call: the default and the longest.
pub const DEFAULT_CHAIN_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_CHAIN_TIMEOUT: Duration = Duration::from_secs(30);
/// Sequence numbers followed per hop and run (a bitmap of 2 MiB); a
/// request past it makes no onward call.
pub const CHAIN_SEEN_BITS: u64 = 1 << 24;
/// Client nodes followed per hop and run.
pub const CHAIN_SEEN_NODES: usize = 64;

/// Why a server makes no onward call for a request of its run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChainRefusal {
    /// The chain could not be prepared on this node (a hop not allowed,
    /// not verified, not resolvable).
    NotPrepared,
    /// The run is not live: ended, or its liveness was not renewed.
    NotLive,
    KillSwitch,
    /// The request already made its call at this hop.
    SeenBefore,
    /// Past what the bitmap follows: not known to be the first.
    Untracked,
    /// The run's onward calls in flight are at their cap.
    Busy,
}

impl ChainRefusal {
    pub const ALL: [ChainRefusal; 6] = [
        ChainRefusal::NotPrepared,
        ChainRefusal::NotLive,
        ChainRefusal::KillSwitch,
        ChainRefusal::SeenBefore,
        ChainRefusal::Untracked,
        ChainRefusal::Busy,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ChainRefusal::NotPrepared => "not_prepared",
            ChainRefusal::NotLive => "not_live",
            ChainRefusal::KillSwitch => "kill_switch",
            ChainRefusal::SeenBefore => "seen_before",
            ChainRefusal::Untracked => "untracked",
            ChainRefusal::Busy => "busy",
        }
    }
}

/// Sequence numbers already called on at one hop, per client node.
#[derive(Default)]
struct HopSeen {
    nodes: std::collections::HashMap<String, Vec<u64>>,
    words: u64,
}

impl HopSeen {
    /// Mark `(node, seq)`: `Ok(true)` when it is the first time,
    /// `Ok(false)` when it was marked before, `Err` when it cannot be told.
    fn first(&mut self, node: &str, seq: u64) -> Result<bool, ()> {
        if seq >= CHAIN_SEEN_BITS {
            return Err(());
        }
        let word = seq / 64;
        let have = self.nodes.get(node).map_or(0, |v| v.len() as u64);
        let grow = (word + 1).saturating_sub(have);
        if self.words + grow > CHAIN_SEEN_BITS / 64
            || (!self.nodes.contains_key(node) && self.nodes.len() >= CHAIN_SEEN_NODES)
        {
            return Err(());
        }
        let bits = self.nodes.entry(node.to_string()).or_default();
        if grow > 0 {
            bits.resize(word as usize + 1, 0);
            self.words += grow;
        }
        let mask = 1u64 << (seq % 64);
        match bits.get_mut(word as usize) {
            Some(w) if *w & mask != 0 => Ok(false),
            Some(w) => {
                *w |= mask;
                Ok(true)
            }
            None => Err(()),
        }
    }
}

/// The gate every onward call of a run passes on a server node: liveness,
/// the kill switch, the one call per request and hop, and the cap on calls
/// in flight. It decides whether a call may be made, never where to.
pub struct ChainGate {
    kill: Arc<KillSwitch>,
    permits: Arc<tokio::sync::Semaphore>,
    /// The run is live until then; `None`: not live (yet, or any more).
    live_until: std::sync::Mutex<Option<std::time::Instant>>,
    /// One bitmap per hop.
    seen: std::sync::Mutex<Vec<HopSeen>>,
}

impl ChainGate {
    /// A gate for a chain of `hops` hops with `max_concurrent` calls in
    /// flight. Not live until [`ChainGate::renew`] says so.
    pub fn new(kill: Arc<KillSwitch>, hops: usize, max_concurrent: usize) -> Self {
        let hops = hops.min(MAX_CHAIN_HOPS);
        Self {
            kill,
            permits: Arc::new(tokio::sync::Semaphore::new(
                max_concurrent.clamp(1, MAX_CHAIN_CONCURRENCY as usize),
            )),
            live_until: std::sync::Mutex::new(None),
            seen: std::sync::Mutex::new((0..hops).map(|_| HopSeen::default()).collect()),
        }
    }

    /// The run is live for `period` from now (a single node: until the
    /// run's longest duration; a cluster member: until the leader's next
    /// word is overdue, as a client's lease).
    pub fn renew(&self, period: Duration) {
        *self.live_until.lock().unwrap_or_else(|p| p.into_inner()) =
            std::time::Instant::now().checked_add(period);
    }

    /// The run is over (or not started): no call from here on.
    pub fn end(&self) {
        *self.live_until.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    pub fn live(&self) -> bool {
        self.live_until
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some_and(|until| std::time::Instant::now() < until)
    }

    pub fn kill_switch(&self) -> &Arc<KillSwitch> {
        &self.kill
    }

    /// May the request `(node, seq)` of the run, verified by the caller,
    /// make its onward call at `hop` (0: the server a client reached)? The
    /// permit is held for the call; dropping it gives the slot back.
    pub fn admit(
        &self,
        hop: usize,
        node: &str,
        seq: u64,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, ChainRefusal> {
        if !self.live() {
            return Err(ChainRefusal::NotLive);
        }
        if self.kill.is_set() {
            return Err(ChainRefusal::KillSwitch);
        }
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| ChainRefusal::Busy)?;
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        match seen.get_mut(hop).map(|h| h.first(node, seq)) {
            Some(Ok(true)) => Ok(permit),
            Some(Ok(false)) => Err(ChainRefusal::SeenBefore),
            // Past the bitmap, or a hop the gate has no bitmap for.
            Some(Err(())) | None => Err(ChainRefusal::Untracked),
        }
    }
}

/// A hop checked and pinned: what the engine's pinned client is built on.
pub struct PinnedHop {
    pub parsed: ParsedTarget,
    pub addrs: Vec<SocketAddr>,
}

/// Check every hop of a chain like a target and pin its addresses: parsed
/// by [`parse_target_url`] (never a control plane path), in this node's
/// allowlist, resolved once, never a forbidden address. Whole or not at
/// all: one hop refused and no hop is pinned, the problems named by hop.
pub async fn pin_chain(
    hops: &[(String, String)],
    allowlist: &Allowlist,
) -> Result<Vec<PinnedHop>, Vec<String>> {
    let mut pinned = Vec::with_capacity(hops.len());
    let mut problems = Vec::new();
    if hops.len() > MAX_CHAIN_HOPS {
        return Err(vec![format!(
            "servers.chain.hops: at most {MAX_CHAIN_HOPS} hops"
        )]);
    }
    for (i, (url, path)) in hops.iter().enumerate() {
        let here = format!("servers.chain.hops[{i}]");
        let parsed = match parse_target_url(url) {
            Ok(p) => p,
            Err(e) => {
                problems.push(format!("{here}.url: {e}"));
                continue;
            }
        };
        if !path.starts_with('/') || is_control_prefix(&format!("{}{path}", parsed.path)) {
            problems.push(format!(
                "{here}.path: a chain never calls a Rustybin control plane"
            ));
            continue;
        }
        match pin(&parsed, allowlist).await {
            Ok(addrs) => pinned.push(PinnedHop { parsed, addrs }),
            Err(e) => problems.push(format!("{here}.url: {e}")),
        }
    }
    if problems.is_empty() {
        Ok(pinned)
    } else {
        Err(problems)
    }
}

/// Check a node URL without resolving it: `http` or `https`, no
/// credentials, a host, and a literal address that is never a forbidden
/// one. Returns the host (brackets trimmed) and port. Every node-to-node
/// call runs this (literal addresses never reach a resolver); a name is
/// checked by [`NodeResolver`] at every connection.
pub fn check_node_url(url: &str) -> Result<(String, u16), String> {
    if url.len() > 2048 {
        return Err("the node URL is longer than 2048 characters".into());
    }
    let parsed = reqwest::Url::parse(url.trim()).map_err(|e| format!("invalid node URL: {e}"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("the node URL must use http or https".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("the node URL must not contain credentials".into());
    }
    let host = parsed
        .host_str()
        .ok_or("the node URL has no host")?
        .trim_matches(['[', ']'])
        .to_string();
    let port = parsed
        .port_or_known_default()
        .ok_or("the node URL has no port")?;
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if let Some(why) = forbidden_address(ip) {
            return Err(format!("{host} is {why}: never a cluster node"));
        }
    }
    Ok((host, port))
}

/// Resolve a node's host name, refusing it when any address is a forbidden
/// one (link-local, cloud metadata, unspecified, multicast, broadcast).
async fn resolve_node(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let resolved = tokio::time::timeout(RESOLVE_TIMEOUT, tokio::net::lookup_host((host, port)))
        .await
        .map_err(|_| format!("resolving {host} timed out"))?
        .map_err(|e| format!("cannot resolve {host}: {e}"))?;
    let mut addrs = Vec::new();
    for addr in resolved.take(MAX_PINNED_ADDRS) {
        if let Some(why) = forbidden_address(addr.ip()) {
            return Err(format!(
                "{host} resolves to {} ({why}): never a cluster node",
                addr.ip()
            ));
        }
        addrs.push(addr);
    }
    if addrs.is_empty() {
        return Err(format!("{host} does not resolve"));
    }
    Ok(addrs)
}

/// Check a coordinator URL before joining: [`check_node_url`], and never a
/// host that resolves to a forbidden address. Joining sends a few small
/// requests to the URL a controller names, never to those places. The
/// node client checks again at every connection ([`NodeResolver`]), so a
/// name that changes its answer after this check (DNS rebinding) is
/// refused too.
pub async fn check_join_url(url: &str) -> Result<(), String> {
    let (host, port) = check_node_url(url)?;
    resolve_node(&host, port).await.map(|_| ())
}

/// The resolver of the node-to-node client: the system's answer, refused
/// whole when any address is a forbidden one. Every connection resolves
/// through it, so no node call reaches such an address whatever the name
/// resolved to when its URL was checked.
pub struct NodeResolver;

impl reqwest::dns::Resolve for NodeResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().trim_end_matches('.').to_string();
        Box::pin(async move {
            // The port is the URL's; reqwest sets it on every address.
            let addrs = resolve_node(&host, 0).await?;
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
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

    #[tokio::test]
    async fn node_resolver_refuses_forbidden_addresses() {
        use reqwest::dns::Resolve;
        use std::str::FromStr;
        let r = NodeResolver;
        let ok: Vec<SocketAddr> = r
            .resolve(reqwest::dns::Name::from_str("localhost").unwrap())
            .await
            .map_err(|e| e.to_string())
            .unwrap()
            .collect();
        assert!(!ok.is_empty() && ok.iter().all(|a| a.ip().is_loopback()));
        for name in ["169.254.169.254", "0.0.0.0", "fe80::1"] {
            let err = r
                .resolve(reqwest::dns::Name::from_str(name).unwrap())
                .await
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default();
            assert!(err.contains("never a cluster node"), "{name}: {err}");
        }
        assert!(check_node_url("http://[fd00:ec2::254]/x")
            .unwrap_err()
            .contains("metadata"));
        assert!(check_node_url("ftp://n/x").is_err());
        assert!(check_node_url("http://u:p@n/x").is_err());
        assert_eq!(
            check_node_url("http://n.example:8080/x").unwrap(),
            ("n.example".to_string(), 8080)
        );
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

    // ── Chains (core next step 13) ──────────────────────────────────

    /// The gate, rule by rule: not live until told, the kill switch, once
    /// per request and hop, no call past what it can follow, a cap on
    /// calls in flight with no queue.
    #[tokio::test]
    async fn the_chain_gate_admits_one_call_per_request_and_hop() {
        let kill = KillSwitch::new();
        let gate = ChainGate::new(kill.clone(), 2, 2);
        // A gate is closed until the run is said to be live.
        assert!(!gate.live());
        assert_eq!(
            gate.admit(0, "node-1", 7).unwrap_err(),
            ChainRefusal::NotLive
        );
        gate.renew(Duration::from_secs(60));
        assert!(gate.live());
        // The refusal above marked nothing: the request still has its call.
        let first = gate.admit(0, "node-1", 7).expect("the first call");
        drop(first);
        // Once per request and hop, whoever sends it again.
        assert_eq!(
            gate.admit(0, "node-1", 7).unwrap_err(),
            ChainRefusal::SeenBefore
        );
        // The next hop of the same request, and the same number from
        // another client node, are other calls.
        drop(gate.admit(1, "node-1", 7).expect("the next hop"));
        drop(gate.admit(0, "node-2", 7).expect("another client node"));
        assert_eq!(
            gate.admit(1, "node-1", 7).unwrap_err(),
            ChainRefusal::SeenBefore
        );
        // A hop the chain does not have, a number past the bitmap: not
        // known to be the first, so no call.
        assert_eq!(
            gate.admit(2, "node-1", 8).unwrap_err(),
            ChainRefusal::Untracked
        );
        assert_eq!(
            gate.admit(0, "node-1", CHAIN_SEEN_BITS).unwrap_err(),
            ChainRefusal::Untracked
        );
        // The kill switch closes it at once, and it opens with the switch.
        kill.set();
        assert_eq!(
            gate.admit(0, "node-1", 9).unwrap_err(),
            ChainRefusal::KillSwitch
        );
        kill.clear();
        // Two calls in flight and no more: the third is refused, not
        // queued, and has its call once a slot is back.
        let a = gate.admit(0, "node-1", 9).expect("first slot");
        let b = gate.admit(0, "node-1", 10).expect("second slot");
        assert_eq!(gate.admit(0, "node-1", 11).unwrap_err(), ChainRefusal::Busy);
        drop(a);
        let c = gate.admit(0, "node-1", 11).expect("a slot came back");
        drop((b, c));
        // The run's end closes it for good; an expired word does too.
        gate.end();
        assert_eq!(
            gate.admit(0, "node-1", 12).unwrap_err(),
            ChainRefusal::NotLive
        );
        gate.renew(Duration::ZERO);
        assert_eq!(
            gate.admit(0, "node-1", 12).unwrap_err(),
            ChainRefusal::NotLive
        );
        // Every reason has its own name in the report.
        let mut names: Vec<&str> = ChainRefusal::ALL.iter().map(|r| r.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ChainRefusal::ALL.len());
    }

    /// The client nodes followed per hop are bounded: one past it makes no
    /// call rather than an untracked one.
    #[tokio::test]
    async fn the_chain_gate_follows_a_bounded_number_of_client_nodes() {
        let gate = ChainGate::new(KillSwitch::new(), 1, 4);
        gate.renew(Duration::from_secs(60));
        for n in 0..CHAIN_SEEN_NODES {
            drop(gate.admit(0, &format!("node-{n}"), 1).expect("followed"));
        }
        assert_eq!(
            gate.admit(0, "one-more", 1).unwrap_err(),
            ChainRefusal::Untracked
        );
        // A node already followed keeps its calls.
        drop(gate.admit(0, "node-0", 2).expect("a followed node"));
    }

    #[tokio::test]
    async fn a_chain_is_prepared_whole_or_not_at_all() {
        let l = list(&["127.0.0.1:9000", "10.0.0.0/8"]);
        let hop = |url: &str, path: &str| (url.to_string(), path.to_string());
        // Every hop allowed: each is pinned to its own addresses.
        let pinned = pin_chain(
            &[
                hop("http://127.0.0.1:9000", "/a"),
                hop("http://10.1.2.3/api", "/b"),
            ],
            &l,
        )
        .await
        .map_err(|p| p.join("; "))
        .expect("pinned");
        assert_eq!(pinned.len(), 2);
        assert_eq!(pinned[0].addrs, vec!["127.0.0.1:9000".parse().unwrap()]);
        assert_eq!(pinned[1].addrs, vec!["10.1.2.3:80".parse().unwrap()]);
        assert_eq!(pinned[1].parsed.path, "/api");
        // One hop refused and nothing is pinned; the problem names the hop.
        for (hops, needle) in [
            (
                vec![
                    hop("http://127.0.0.1:9000", "/a"),
                    hop("http://127.0.0.1:9001", "/b"),
                ],
                "servers.chain.hops[1].url: 127.0.0.1:9001 is not in RUSTYBIN_HARNESS_TARGETS",
            ),
            // Forbidden whatever the list says.
            (vec![hop("http://169.254.169.254/latest", "/")], "metadata"),
            // Never this or another node's control plane.
            (
                vec![hop("http://127.0.0.1:9000/_rustybin", "/")],
                "control plane",
            ),
            (
                vec![hop("http://127.0.0.1:9000", "/_rustybin/harness/stop")],
                "control plane",
            ),
            (vec![hop("http://127.0.0.1:9000/ui", "/")], "control plane"),
            (vec![hop("ftp://127.0.0.1:9000", "/")], "hops[0].url"),
            (vec![hop("http://u:p@127.0.0.1:9000", "/")], "hops[0].url"),
            // Past the fixed depth.
            (
                vec![
                    hop("http://127.0.0.1:9000", "/1"),
                    hop("http://127.0.0.1:9000", "/2"),
                    hop("http://127.0.0.1:9000", "/3"),
                    hop("http://127.0.0.1:9000", "/4"),
                ],
                "at most 3 hops",
            ),
        ] {
            let problems = pin_chain(&hops, &l).await.err().expect("refused");
            assert!(
                problems.iter().any(|p| p.contains(needle)),
                "{needle}: {problems:?}"
            );
        }
        // Nothing without a list: a server with no targets calls nowhere.
        let problems = pin_chain(&[hop("http://127.0.0.1:9000", "/")], &Allowlist::default())
            .await
            .err()
            .expect("refused");
        assert!(
            problems[0].contains("RUSTYBIN_HARNESS_TARGETS"),
            "{problems:?}"
        );
    }
}
