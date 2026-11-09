//! Server-to-server chains (step 13 of `docs/plans/core-next`): a server
//! answering a request of a run calls a further hop the scenario names, and
//! the report gains the leg of each hop.
//!
//! This is the only traffic the server role sends, so it is built on the
//! rules of [`super::safety`] and adds none of its own:
//!
//! - [`Chain::prepare`] turns the run's `servers.chain` into pinned clients
//!   through [`safety::pin_chain`] (the allowlist, pinned addresses,
//!   forbidden addresses) and the node's target verification, whole or not
//!   at all. The clients are the engine's [`PinnedTarget`]: there is no
//!   other client here.
//! - [`call_on`] is called by the correlation layer for a marker it has
//!   verified, and only then. It asks the run's [`ChainGate`] (live, the
//!   kill switch, once per request and hop, the cap on calls in flight)
//!   and sends one `GET` of the hop the marker's hop count selects.
//!
//! Nothing of the incoming request reaches the onward one: its destination
//! is the scenario's, its headers are the harness's own, it has no body.
//! The onward marker keeps the run, the client node and the sequence number
//! (so the next server can refuse a repeat) and raises the signed hop count.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderValue, Method};
use bytes::Bytes;
use hdrhistogram::Histogram;
use http_body_util::{BodyExt, Full};
use serde::{Deserialize, Serialize};

use super::correlate::{self, Marker, RunEntry, Tag};
use super::engine::PinnedTarget;
use super::report::{histogram, Percentiles, WireHist};
use super::safety::{self, ChainGate, ChainRefusal, MAX_CHAIN_HOPS};
use super::scenario;
use super::Harness;

/// The most of an onward answer read before it is let go.
const MAX_ANSWER_BYTES: usize = 1024 * 1024;
/// How often a call under way looks at the kill switch and the run.
const WATCH_EVERY: Duration = Duration::from_millis(50);
/// Distinct statuses counted per hop.
const MAX_STATUSES: usize = 64;

/// A run's chain on one server node: a pinned client per hop and the gate.
pub struct Chain {
    /// `hops[n]` is called by the server that got a marker with hop `n`.
    hops: Vec<PinnedTarget>,
    /// The one path each call asks for.
    paths: Vec<String>,
    gate: ChainGate,
    timeout: Duration,
}

impl Chain {
    /// Prepare the chain on this node, whole or not at all: every hop is
    /// checked against this node's allowlist and pinned
    /// ([`safety::pin_chain`]), gets the engine's pinned client, and, with
    /// `RUSTYBIN_HARNESS_VERIFY_TARGETS`, must serve `token` like a target.
    /// The problems name the hop.
    pub async fn prepare(
        h: &Harness,
        chain: &scenario::Chain,
        token: &str,
    ) -> Result<Self, Vec<String>> {
        let named: Vec<(String, String)> = chain
            .hops
            .iter()
            .map(|hop| (hop.url.clone(), hop.path.clone()))
            .collect();
        let pinned = safety::pin_chain(&named, &h.allowlist).await?;
        let handle = tokio::runtime::Handle::current();
        let mut hops = Vec::with_capacity(pinned.len());
        let mut problems = Vec::new();
        for (i, (hop, pin)) in chain.hops.iter().zip(pinned).enumerate() {
            if hop.http == scenario::HttpVersion::H3 {
                problems.push(format!(
                    "servers.chain.hops[{i}].http: a hop is called over HTTP/1.1 or HTTP/2"
                ));
                continue;
            }
            match PinnedTarget::new(
                &handle,
                hop.target(),
                pin.parsed,
                pin.addrs,
                chain.max_concurrent(),
            ) {
                Ok(target) => hops.push(target),
                Err(e) => problems.push(format!("servers.chain.hops[{i}]: {e}")),
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        if h.config.verify_targets {
            for (i, target) in hops.iter_mut().enumerate() {
                match h.verify_target(target, token).await {
                    Ok(()) => target.verified = true,
                    Err(e) => problems.push(format!("servers.chain.hops[{i}]: {e}")),
                }
            }
            if !problems.is_empty() {
                return Err(problems);
            }
        }
        Ok(Self {
            gate: ChainGate::new(h.kill.clone(), hops.len(), chain.max_concurrent()),
            paths: chain.hops.iter().map(|hop| hop.path.clone()).collect(),
            hops,
            timeout: chain.timeout(),
        })
    }

    /// The gate: the caller renews the run's liveness through it.
    pub fn gate(&self) -> &ChainGate {
        &self.gate
    }

    /// The pinned addresses and the verification of each hop (the report).
    fn describe(&self, hop: usize) -> (Vec<String>, bool) {
        self.hops.get(hop).map_or((Vec::new(), false), |t| {
            (t.addrs.iter().map(|a| a.to_string()).collect(), t.verified)
        })
    }

    /// One onward call: a `GET` of the hop's own URL and path, marked with
    /// the run's key and the next hop count. Ends at once on the kill
    /// switch or when the run is no longer live.
    async fn send(&self, hop: usize, entry: &RunEntry, incoming: &Marker) -> Outcome {
        let (Some(target), Some(path)) = (self.hops.get(hop), self.paths.get(hop)) else {
            return Outcome::Refused(ChainRefusal::NotPrepared);
        };
        // The destination is the scenario's: nothing of the incoming
        // request is read to build it.
        let uri = format!("{}{}{}", target.authority, target.parsed.path, path);
        let Ok(uri) = uri.parse::<hyper::Uri>() else {
            return Outcome::Transport("protocol", Duration::ZERO);
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::USER_AGENT,
            HeaderValue::from_static(concat!(
                "rustybin-harness/",
                env!("CARGO_PKG_VERSION"),
                " (chain)"
            )),
        );
        let names = vec!["user-agent".to_string()];
        let (headers_digest, _) = correlate::digest_headers(&names, &headers);
        let path_and_query = uri
            .path_and_query()
            .map_or("/".to_string(), |p| p.as_str().to_string());
        let mut marker = Marker {
            run: incoming.run.clone(),
            node: incoming.node.clone(),
            seq: incoming.seq,
            vu: incoming.vu,
            layer: incoming.layer,
            sent_ns: correlate::now_ns(),
            names,
            headers_digest,
            body_digest: String::new(),
            path_digest: correlate::digest_path(&Method::GET, &path_and_query),
            hop: incoming.hop.saturating_add(1),
            sig: String::new(),
        };
        marker.sign(&entry.key);
        let own = [
            (
                "x-request-id",
                format!(
                    "{}-{}-{}-hop{}",
                    incoming.run, incoming.node, incoming.seq, marker.hop
                ),
            ),
            (
                "traceparent",
                correlate::traceparent(&incoming.run, &incoming.node, incoming.seq),
            ),
            (correlate::HEADER, marker.to_header()),
        ];
        for (name, value) in own {
            let Ok(value) = HeaderValue::from_str(&value) else {
                return Outcome::Transport("protocol", Duration::ZERO);
            };
            headers.insert(name, value);
        }
        let Ok(mut req) = hyper::Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Full::new(Bytes::new()).boxed())
        else {
            return Outcome::Transport("protocol", Duration::ZERO);
        };
        *req.headers_mut() = headers;
        let client = target.client.clone();
        let started = Instant::now();
        let call = async {
            let resp = match client.request(req).await {
                Ok(r) => r,
                Err(e) => {
                    return Outcome::Transport(super::engine::classify_error(&e), started.elapsed())
                }
            };
            let status = resp.status().as_u16();
            // The next server's tag, verified with the run key: a tagged
            // answer reached a server, an untagged one came from between.
            let tagged = resp
                .headers()
                .get(correlate::SERVED_BY)
                .and_then(|v| v.to_str().ok())
                .and_then(Tag::parse)
                .is_some_and(|t| {
                    t.run == incoming.run && t.seq == incoming.seq && t.verify(&entry.key)
                });
            let upstream_ms = resp
                .headers()
                .get("server-timing")
                .and_then(|v| v.to_str().ok())
                .and_then(correlate::parse_server_timing)
                .filter(|ms| tagged && ms.is_finite() && *ms >= 0.0);
            // The body is read frame by frame (so the connection goes back
            // to the pool) up to a bound, and never kept: nothing of an
            // answer is held, however many calls are in flight.
            let mut body = resp.into_body();
            let mut read = 0usize;
            while let Some(Ok(frame)) = body.frame().await {
                read = read.saturating_add(frame.data_ref().map_or(0, |d| d.len()));
                if read > MAX_ANSWER_BYTES {
                    break;
                }
            }
            Outcome::Answer {
                status,
                tagged,
                upstream_ms,
                elapsed: started.elapsed(),
            }
        };
        let watch = async {
            loop {
                tokio::time::sleep(WATCH_EVERY).await;
                if self.gate.kill_switch().is_set() {
                    return ChainRefusal::KillSwitch;
                }
                if !self.gate.live() || entry.is_done() {
                    return ChainRefusal::NotLive;
                }
            }
        };
        tokio::select! {
            biased;
            () = self.gate.kill_switch().notified() => Outcome::Refused(ChainRefusal::KillSwitch),
            why = watch => Outcome::Refused(why),
            outcome = tokio::time::timeout(self.timeout, call) => {
                outcome.unwrap_or(Outcome::Transport("timeout", self.timeout))
            }
        }
    }
}

/// What an onward call came to.
enum Outcome {
    Answer {
        status: u16,
        tagged: bool,
        upstream_ms: Option<f64>,
        elapsed: Duration,
    },
    Transport(&'static str, Duration),
    /// Ended before it had an answer (the kill switch, the run's end).
    Refused(ChainRefusal),
}

/// What one hop came to on a server node.
struct HopStats {
    /// Onward calls of the previous hop that arrived here.
    arrivals: u64,
    calls: u64,
    upstream: u64,
    gateway: u64,
    transport: u64,
    statuses: BTreeMap<u16, u64>,
    transport_kinds: BTreeMap<&'static str, u64>,
    refused: BTreeMap<&'static str, u64>,
    /// Microseconds: the whole leg, the next server's processing, the rest.
    latency: Histogram<u64>,
    upstream_time: Histogram<u64>,
    overhead: Histogram<u64>,
}

impl HopStats {
    fn new() -> Self {
        Self {
            arrivals: 0,
            calls: 0,
            upstream: 0,
            gateway: 0,
            transport: 0,
            statuses: BTreeMap::new(),
            transport_kinds: BTreeMap::new(),
            refused: BTreeMap::new(),
            latency: histogram(2),
            upstream_time: histogram(2),
            overhead: histogram(2),
        }
    }
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX).max(1)
}

/// The chain's numbers of one run on a server node: kept on the run's
/// entry from the moment it is known, prepared or not, so a refusal for a
/// chain that could not be prepared is counted too. Bounded by the depth.
pub struct ChainStats {
    labels: Vec<String>,
    hops: Vec<HopStats>,
    /// Why the chain is not prepared on this node, when it is not.
    problem: Option<String>,
}

impl ChainStats {
    pub fn new(chain: &scenario::Chain) -> Self {
        let labels: Vec<String> = chain
            .hops
            .iter()
            .take(MAX_CHAIN_HOPS)
            .map(scenario::Hop::label)
            .collect();
        Self {
            hops: labels.iter().map(|_| HopStats::new()).collect(),
            labels,
            problem: None,
        }
    }

    pub fn set_problem(&mut self, problem: String) {
        self.problem = Some(problem.chars().take(1024).collect());
    }

    fn refuse(&mut self, hop: usize, why: ChainRefusal) {
        if let Some(h) = self.hops.get_mut(hop) {
            *h.refused.entry(why.as_str()).or_default() += 1;
        }
    }

    /// An onward call of hop `hop` (1 or more) arrived at this node.
    fn arrived(&mut self, hop: usize) {
        if let Some(h) = hop.checked_sub(1).and_then(|i| self.hops.get_mut(i)) {
            h.arrivals += 1;
        }
    }

    fn record(&mut self, hop: usize, outcome: &Outcome) {
        let Some(h) = self.hops.get_mut(hop) else {
            return;
        };
        match outcome {
            Outcome::Answer {
                status,
                tagged,
                upstream_ms,
                elapsed,
            } => {
                h.calls += 1;
                if *tagged {
                    h.upstream += 1;
                } else {
                    h.gateway += 1;
                }
                if h.statuses.len() < MAX_STATUSES || h.statuses.contains_key(status) {
                    *h.statuses.entry(*status).or_default() += 1;
                }
                h.latency.saturating_record(micros(*elapsed));
                if let Some(ms) = upstream_ms {
                    let up = Duration::from_secs_f64((ms / 1000.0).min(86_400.0));
                    h.upstream_time.saturating_record(micros(up));
                    h.overhead
                        .saturating_record(micros(elapsed.saturating_sub(up)));
                }
            }
            Outcome::Transport(kind, elapsed) => {
                h.calls += 1;
                h.transport += 1;
                *h.transport_kinds.entry(kind).or_default() += 1;
                h.latency.saturating_record(micros(*elapsed));
            }
            // Ended before an answer: the call was made, and stopped.
            Outcome::Refused(why) => {
                h.calls += 1;
                h.transport += 1;
                *h.transport_kinds.entry("stopped").or_default() += 1;
                *h.refused.entry(why.as_str()).or_default() += 1;
            }
        }
    }

    /// The report of this node's part, with what `chain` says of each hop.
    pub fn report(&self, chain: Option<&Chain>) -> ChainReport {
        ChainReport {
            hops: self
                .hops
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let (pinned, verified) = chain.map_or((Vec::new(), false), |c| c.describe(i));
                    HopReport {
                        hop: i as u8 + 1,
                        url: self.labels.get(i).cloned().unwrap_or_default(),
                        pinned,
                        verified,
                        arrivals: h.arrivals,
                        calls: h.calls,
                        upstream: h.upstream,
                        gateway: h.gateway,
                        transport: h.transport,
                        statuses: h
                            .statuses
                            .iter()
                            .map(|(k, v)| (k.to_string(), *v))
                            .collect(),
                        transport_kinds: h
                            .transport_kinds
                            .iter()
                            .map(|(k, v)| (k.to_string(), *v))
                            .collect(),
                        refused: h.refused.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
                        latency: Percentiles::from_histogram(&h.latency),
                        upstream_latency: Percentiles::from_histogram(&h.upstream_time),
                        overhead: Percentiles::from_histogram(&h.overhead),
                    }
                })
                .collect(),
            problems: self.problem.iter().cloned().collect(),
            followed_up_to: safety::CHAIN_SEEN_BITS,
        }
    }

    /// The histograms, for a cluster's leader to merge (no averaged
    /// percentiles).
    pub fn hists(&self) -> Vec<HopHists> {
        self.hops
            .iter()
            .map(|h| HopHists {
                latency: WireHist::encode(&h.latency),
                upstream: WireHist::encode(&h.upstream_time),
                overhead: WireHist::encode(&h.overhead),
            })
            .collect()
    }
}

/// The chain in a run's report (`server.chain`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChainReport {
    pub hops: Vec<HopReport>,
    /// Why a server node made no onward calls (a hop not allowed or not
    /// verified there), by node in a cluster.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
    /// Requests followed per hop: one past it makes no onward call
    /// (`refused.untracked`).
    pub followed_up_to: u64,
}

/// One hop: the leg from the server that calls it to the next server.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HopReport {
    /// 1: called by the server a client's request reached.
    pub hop: u8,
    pub url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned: Vec<String>,
    /// Served the verification token before the run.
    #[serde(default)]
    pub verified: bool,
    /// Onward calls of this hop that reached a server of the run.
    pub arrivals: u64,
    /// Onward calls made.
    pub calls: u64,
    /// Answered by a server of the run (a valid tag).
    pub upstream: u64,
    /// Answered without the tag: by the gateway, or something else between.
    pub gateway: u64,
    /// No answer, by kind in `transport_kinds`.
    pub transport: u64,
    pub statuses: BTreeMap<String, u64>,
    pub transport_kinds: BTreeMap<String, u64>,
    /// Requests of the run that made no call here, by reason.
    pub refused: BTreeMap<String, u64>,
    /// The whole leg, as the calling server saw it.
    pub latency: Percentiles,
    /// The next server's processing time (`Server-Timing: rb`).
    pub upstream_latency: Percentiles,
    /// The leg minus the next server's processing: the gateway's part.
    pub overhead: Percentiles,
}

/// A hop's histograms on the wire (a cluster's server stats).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HopHists {
    pub latency: WireHist,
    pub upstream: WireHist,
    pub overhead: WireHist,
}

/// Merges the chain reports of a cluster's server nodes.
#[derive(Default)]
pub struct Merge {
    hops: Vec<(HopReport, [Histogram<u64>; 3])>,
    problems: Vec<String>,
    any: bool,
}

fn add_counts(into: &mut BTreeMap<String, u64>, from: &BTreeMap<String, u64>) {
    for (k, v) in from {
        if into.len() < MAX_STATUSES || into.contains_key(k) {
            *into.entry(k.clone()).or_default() += v;
        }
    }
}

impl Merge {
    /// Add one server node's part.
    pub fn add(&mut self, node: &str, report: &ChainReport, hists: &[HopHists]) {
        self.any = true;
        for p in &report.problems {
            if self.problems.len() < super::scenario::MAX_CLUSTER_NODES as usize {
                self.problems.push(format!("{node}: {p}"));
            }
        }
        for (i, r) in report.hops.iter().take(MAX_CHAIN_HOPS).enumerate() {
            if self.hops.len() <= i {
                self.hops.push((
                    HopReport {
                        hop: r.hop,
                        url: r.url.clone(),
                        // Until a node says otherwise.
                        verified: true,
                        ..HopReport::default()
                    },
                    [histogram(2), histogram(2), histogram(2)],
                ));
            }
            let Some((m, h)) = self.hops.get_mut(i) else {
                continue;
            };
            m.arrivals += r.arrivals;
            m.calls += r.calls;
            m.upstream += r.upstream;
            m.gateway += r.gateway;
            m.transport += r.transport;
            m.verified &= r.verified;
            for a in &r.pinned {
                if !m.pinned.contains(a) && m.pinned.len() < 64 {
                    m.pinned.push(a.clone());
                }
            }
            add_counts(&mut m.statuses, &r.statuses);
            add_counts(&mut m.transport_kinds, &r.transport_kinds);
            add_counts(&mut m.refused, &r.refused);
            if let Some(w) = hists.get(i) {
                let _ = w.latency.add_to(&mut h[0]);
                let _ = w.upstream.add_to(&mut h[1]);
                let _ = w.overhead.add_to(&mut h[2]);
            }
        }
    }

    pub fn finish(self) -> Option<ChainReport> {
        self.any.then(|| ChainReport {
            hops: self
                .hops
                .into_iter()
                .map(|(mut r, h)| {
                    r.latency = Percentiles::from_histogram(&h[0]);
                    r.upstream_latency = Percentiles::from_histogram(&h[1]);
                    r.overhead = Percentiles::from_histogram(&h[2]);
                    r
                })
                .collect(),
            problems: self.problems,
            followed_up_to: safety::CHAIN_SEEN_BITS,
        })
    }
}

/// The chain's part for a marker the correlation layer has verified (and
/// only for one): count the arrival of an onward call, then make this
/// server's own onward call when the chain goes on and the gate lets it.
/// The incoming request is answered either way.
pub async fn call_on(entry: &Arc<RunEntry>, marker: &Marker) {
    let Some(stats) = entry.chain_stats() else {
        return; // the run declares no chain
    };
    let hop = usize::from(marker.hop);
    fn lock(s: &Mutex<ChainStats>) -> std::sync::MutexGuard<'_, ChainStats> {
        s.lock().unwrap_or_else(|p| p.into_inner())
    }
    if hop > 0 {
        lock(stats).arrived(hop);
    }
    let hops = entry
        .profile
        .chain
        .as_ref()
        .map_or(0, |c| c.hops.len().min(MAX_CHAIN_HOPS));
    if hop >= hops {
        return; // the end of the chain
    }
    let Some(chain) = entry.chain() else {
        lock(stats).refuse(hop, ChainRefusal::NotPrepared);
        return;
    };
    // The run ended on this node: its entry says so before the gate does.
    if entry.is_done() {
        lock(stats).refuse(hop, ChainRefusal::NotLive);
        return;
    }
    let permit = match chain.gate.admit(hop, &marker.node, marker.seq) {
        Ok(permit) => permit,
        Err(why) => {
            lock(stats).refuse(hop, why);
            return;
        }
    };
    let outcome = chain.send(hop, entry, marker).await;
    drop(permit);
    lock(stats).record(hop, &outcome);
}

#[cfg(test)]
#[path = "chain_tests.rs"]
mod tests;
