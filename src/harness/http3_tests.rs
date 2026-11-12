//! HTTP/3 targets (the cargo feature `harness-http3`): a run over QUIC is
//! marked, classified and reported like any other, and UDP is no way around
//! the fences (the allowlist, the pinned address, verification, the kill
//! switch, TLS verification).
//!
//! The target is an in-process HTTP/3 server on loopback with an ephemeral
//! UDP port and the test PKI: a stand-in for a gateway that terminates QUIC
//! and routes to the node behind it (here: the node's own router, in
//! process). Rates stay low.

use std::net::{SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, StatusCode};
use axum::Router;
use bytes::Buf;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::config::Config;
use crate::harness::tests::{harness_config, running, send, wait_for_report};
use crate::harness::Role;
use crate::state::AppState;

/// What the stand-in gateway saw.
#[derive(Default)]
struct Seen {
    requests: AtomicU64,
    marked: AtomicU64,
    /// QUIC connections that completed their handshake, and those of them
    /// that were closed since.
    opened: AtomicU64,
    closed: AtomicU64,
}

impl Seen {
    fn n(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::SeqCst)
    }

    /// Wait (two seconds at most) until every connection opened so far is
    /// closed; answers how many were opened.
    async fn all_closed(&self) -> u64 {
        for _ in 0..100 {
            if Self::n(&self.closed) == Self::n(&self.opened) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            Self::n(&self.closed),
            Self::n(&self.opened),
            "QUIC connections left open"
        );
        Self::n(&self.opened)
    }
}

struct Gateway {
    addr: SocketAddr,
    seen: Arc<Seen>,
    endpoint: quinn::Endpoint,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.endpoint.close(0u32.into(), b"test over");
    }
}

/// An HTTP/3 server on `127.0.0.1` (an ephemeral UDP port) with the test
/// PKI's server certificate, handing every request to `app`. With
/// `preferred`, it asks its clients to move to that address.
fn gateway(state: &AppState, app: Router, preferred: Option<SocketAddrV4>) -> Gateway {
    use rustls::pki_types::pem::PemObject;
    let cert = rustls::pki_types::CertificateDer::from(
        crate::cert_state::pem_to_der(&state.certs.server_cert_pem).expect("certificate"),
    );
    let key =
        rustls::pki_types::PrivateKeyDer::from_pem_slice(state.certs.server_key_pem.as_bytes())
            .expect("key");
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("server certificate");
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(tls).expect("quic tls"),
    ));
    config.preferred_address_v4(preferred);
    let endpoint =
        quinn::Endpoint::server(config, "127.0.0.1:0".parse().expect("addr")).expect("endpoint");
    let addr = endpoint.local_addr().expect("local addr");
    let seen = Arc::new(Seen::default());
    let accepting = endpoint.clone();
    let counting = seen.clone();
    tokio::spawn(async move {
        while let Some(incoming) = accepting.accept().await {
            let app = app.clone();
            let seen = counting.clone();
            tokio::spawn(async move {
                let Ok(conn) = incoming.await else { return };
                seen.opened.fetch_add(1, Ordering::SeqCst);
                let (watched, closing) = (conn.clone(), seen.clone());
                tokio::spawn(async move {
                    watched.closed().await;
                    closing.closed.fetch_add(1, Ordering::SeqCst);
                });
                let Ok(mut h3c) =
                    h3::server::Connection::new(h3_quinn::Connection::new(conn)).await
                else {
                    return;
                };
                while let Ok(Some(resolver)) = h3c.accept().await {
                    let app = app.clone();
                    let seen = seen.clone();
                    tokio::spawn(async move {
                        let Ok((req, mut stream)) = resolver.resolve_request().await else {
                            return;
                        };
                        let mut body = Vec::new();
                        while let Ok(Some(mut chunk)) = stream.recv_data().await {
                            let n = chunk.remaining();
                            body.extend_from_slice(&chunk.copy_to_bytes(n));
                        }
                        seen.requests.fetch_add(1, Ordering::SeqCst);
                        if req
                            .headers()
                            .contains_key(crate::harness::correlate::HEADER)
                        {
                            seen.marked.fetch_add(1, Ordering::SeqCst);
                        }
                        let (parts, ()) = req.into_parts();
                        let path = parts
                            .uri
                            .path_and_query()
                            .map_or("/".to_string(), |p| p.as_str().to_string());
                        let mut inner = axum::http::Request::builder()
                            .method(parts.method)
                            .uri(path);
                        for (name, value) in &parts.headers {
                            inner = inner.header(name, value);
                        }
                        let inner = inner.body(Body::from(body)).expect("request");
                        let Ok(resp) = app.oneshot(inner).await;
                        let (rparts, rbody) = resp.into_parts();
                        let bytes = axum::body::to_bytes(rbody, 4 * 1024 * 1024)
                            .await
                            .unwrap_or_default();
                        let mut out = axum::http::Response::builder().status(rparts.status);
                        for (name, value) in &rparts.headers {
                            if !matches!(name.as_str(), "connection" | "transfer-encoding") {
                                out = out.header(name, value);
                            }
                        }
                        if stream
                            .send_response(out.body(()).expect("response"))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        if !bytes.is_empty() {
                            let _ = stream.send_data(bytes).await;
                        }
                        let _ = stream.finish().await;
                    });
                }
            });
        }
    });
    Gateway {
        addr,
        seen,
        endpoint,
    }
}

fn scenario(addr: SocketAddr, rate: u64, secs: &str) -> Value {
    json!({
        "name": "h3",
        "targets": [{ "url": format!("https://{addr}"), "http": "h3", "insecure": true }],
        "source": { "requests": [
            { "path": "/status/200" },
            { "method": "POST", "path": "/anything?n={{seq}}", "headers": { "x-vu": "{{vu}}" }, "body": { "n": "{{seq}}" } }
        ] },
        "pattern": { "type": "constant", "rate": rate, "for": secs },
        "timeout": "5s",
        "thresholds": { "error_rate": 0, "not_sent": 0 }
    })
}

async fn start(app: &Router, scenario: Value) -> (StatusCode, Value) {
    send(app, Method::POST, "/_rustybin/harness/runs", Some(scenario)).await
}

async fn stop_all(servers: Vec<crate::RunningServer>) {
    for s in servers {
        s.shutdown();
        let _ = s.wait().await;
    }
}

#[tokio::test]
async fn a_run_over_http3_is_marked_classified_and_reported() {
    let (server, app, state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let (_, st) = send(&app, Method::GET, "/_rustybin/harness", None).await;
    assert_eq!(st["http3"], true, "{st}");
    let gw = gateway(&state, app.clone(), None);
    let (status, run) = start(&app, scenario(gw.addr, 100, "1s")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    assert_eq!(report["state"], "finished", "{report}");
    let t = &report["totals"];
    let sent = t["sent"].as_u64().unwrap();
    assert!((97..=103).contains(&sent), "sent {sent}");
    assert_eq!(t["completed"], sent, "{t}");
    assert_eq!(t["ok"], sent, "{t}");
    assert_eq!(t["transport"], 0, "{t}");
    // Every answer carried a valid tag: the marker went over QUIC on every
    // request and the server side verified it.
    assert_eq!(t["upstream"], sent, "{t}");
    assert_eq!(t["gateway"], 0, "{t}");
    assert_eq!(gw.seen.requests.load(Ordering::SeqCst), sent);
    assert_eq!(gw.seen.marked.load(Ordering::SeqCst), sent);
    // The server side: what arrived is what was sent, bodies included.
    let srv = &report["server"];
    assert_eq!(srv["arrivals"], sent, "{srv}");
    assert_eq!(srv["changes"]["body_changed"], 0, "{srv}");
    assert_eq!(srv["changes"]["path_changed"], 0, "{srv}");
    assert_eq!(srv["changes"]["headers_changed"], 0, "{srv}");
    // Reported as for HTTP/1 and 2.
    assert_eq!(report["targets"][0]["http"], "h3");
    assert_eq!(report["targets"][0]["pinned"], json!([gw.addr.to_string()]));
    assert!(report["latency"]["p99_ms"].as_f64().unwrap() > 0.0);
    // The overhead and the one-way legs, as for any tagged answer.
    assert!(report["overhead"]["p50_ms"].as_f64().unwrap() > 0.0);
    assert!(report["upstream_latency"]["p50_ms"].as_f64().unwrap() > 0.0);
    assert_eq!(report["passed"], true, "{:?}", report["thresholds"]);
    stop_all(vec![server]).await;
}

#[tokio::test]
async fn http3_needs_https_and_plain_http_sources() {
    let app = crate::test_support::test_app_with(AppState::for_tests(Config::for_tests()));
    let validate = |scenario: Value| {
        let app = app.clone();
        async move {
            let (status, v) = send(
                &app,
                Method::POST,
                "/_rustybin/harness/validate",
                Some(scenario),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{v}");
            v["problems"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };
    let ok = validate(scenario("127.0.0.1:4433".parse().unwrap(), 1, "1s")).await;
    assert!(ok.is_empty(), "{ok:?}");
    let mut plain = scenario("127.0.0.1:4433".parse().unwrap(), 1, "1s");
    plain["targets"][0]["url"] = json!("http://127.0.0.1:4433");
    let problems = validate(plain).await;
    assert!(
        problems
            .iter()
            .any(|p| p.contains("h3 needs an https target")),
        "{problems:?}"
    );
    for (source, name) in [
        (json!({ "websocket": { "path": "/ws" } }), "websocket"),
        (json!({ "sse": { "path": "/sse" } }), "sse"),
        (
            json!({ "grpc": { "path": "/x/Y", "message": "hi" } }),
            "grpc",
        ),
        (json!({ "llm": { "prompt": "hi" } }), "llm"),
        (json!({ "mcp": { "tool": "echo" } }), "mcp"),
    ] {
        let mut s = scenario("127.0.0.1:4433".parse().unwrap(), 1, "1s");
        s["source"] = source;
        let problems = validate(s).await;
        assert!(
            problems
                .iter()
                .any(|p| p.contains(&format!("source.{name}: not sent over HTTP/3"))),
            "{name}: {problems:?}"
        );
    }
    let mut faulty = scenario("127.0.0.1:4433".parse().unwrap(), 1, "1s");
    faulty["faults"] = json!({ "client": { "connection_churn": 0.5, "http2": 0.5 } });
    let problems = validate(faulty).await;
    for needle in ["faults.client.connection_churn", "faults.client.http2"] {
        assert!(
            problems.iter().any(|p| p.contains(needle)),
            "{needle}: {problems:?}"
        );
    }
    let mut bound = json!({
        "name": "h3",
        "targets": [{ "url": "https://127.0.0.1:4433", "http": "h3" }],
        "layers": [{
            "name": "l",
            "source": { "request": { "path": "/" } },
            "pattern": { "type": "constant", "rate": 1, "for": "1s" },
            "identities": { "source_addresses": ["127.0.0.1"] }
        }]
    });
    let problems = validate(bound.take()).await;
    assert!(
        problems
            .iter()
            .any(|p| p.contains("identities.source_addresses: not with an HTTP/3 target")),
        "{problems:?}"
    );
}

#[tokio::test]
async fn an_http3_target_outside_the_allowlist_is_refused() {
    // Only one TCP and UDP port of loopback is allowed; metadata never is.
    let (server, app, state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1:1"],
    ))
    .await;
    let gw = gateway(&state, app.clone(), None);
    let (status, v) = start(&app, scenario(gw.addr, 20, "1s")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert!(
        v["problems"][0]
            .as_str()
            .unwrap()
            .contains("not in RUSTYBIN_HARNESS_TARGETS"),
        "{v}"
    );
    let mut meta = scenario(gw.addr, 20, "1s");
    meta["targets"][0]["url"] = json!("https://169.254.169.254");
    let (status, v) = start(&app, meta).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert!(
        v["problems"][0].as_str().unwrap().contains("metadata"),
        "{v}"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        gw.seen.requests.load(Ordering::SeqCst),
        0,
        "no datagram left"
    );
    stop_all(vec![server]).await;
}

/// The server asks its clients to move to another address (a QUIC server's
/// preferred address): the load stays on the pinned one. A second DNS
/// answer has nowhere to come from (nothing here resolves a name), and
/// this is the one way a QUIC peer could name another address.
#[tokio::test]
async fn http3_goes_to_the_pinned_address_only() {
    let (server, app, state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let decoy = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("decoy");
    let SocketAddr::V4(decoy_addr) = decoy.local_addr().expect("decoy addr") else {
        panic!("an IPv4 socket");
    };
    let gw = gateway(&state, app.clone(), Some(decoy_addr));
    let (status, run) = start(&app, scenario(gw.addr, 50, "1s")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    let t = &report["totals"];
    let sent = t["sent"].as_u64().unwrap();
    assert!(sent >= 45, "{t}");
    assert_eq!(t["upstream"], sent, "{t}");
    assert_eq!(gw.seen.requests.load(Ordering::SeqCst), sent);
    assert_eq!(report["targets"][0]["pinned"], json!([gw.addr.to_string()]));
    // Not one datagram went to the address the server preferred.
    let mut buf = [0u8; 64];
    assert!(
        decoy.try_recv_from(&mut buf).is_err(),
        "a datagram reached the preferred address"
    );
    stop_all(vec![server]).await;
}

#[tokio::test]
async fn http3_targets_are_verified_over_http3() {
    let mut config = harness_config(&[Role::Client, Role::Server], &["127.0.0.1"]);
    config.harness.verify_targets = true;
    let (server, app, state) = running(config).await;
    // The gateway routes to this node, which serves its token: verified,
    // over QUIC (nothing listens on TCP at the gateway's port).
    let gw = gateway(&state, app.clone(), None);
    assert!(
        std::net::TcpStream::connect_timeout(&gw.addr, Duration::from_millis(200)).is_err(),
        "the stand-in gateway has no TCP listener"
    );
    let (status, run) = start(&app, scenario(gw.addr, 20, "1s")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    assert_eq!(report["targets"][0]["verified"], true, "{report}");
    // A gateway that routes to another instance serves another token:
    // refused before a single load request.
    let (other, other_app, other_state) = running(harness_config(&[Role::Server], &[])).await;
    let elsewhere = gateway(&other_state, other_app, None);
    let (status, v) = start(&app, scenario(elsewhere.addr, 20, "1s")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert!(
        v["problems"][0]
            .as_str()
            .unwrap()
            .contains("verification token"),
        "{v}"
    );
    // One verification request reached it, no load.
    assert_eq!(elsewhere.seen.requests.load(Ordering::SeqCst), 1);
    assert_eq!(elsewhere.seen.marked.load(Ordering::SeqCst), 0);
    stop_all(vec![server, other]).await;
}

#[tokio::test]
async fn the_kill_switch_stops_an_http3_run() {
    let (server, app, state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let gw = gateway(&state, app.clone(), None);
    let (status, run) = start(&app, scenario(gw.addr, 50, "20s")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    tokio::time::sleep(Duration::from_millis(700)).await;
    let (status, v) = send(&app, Method::POST, "/_rustybin/harness/stop", None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["kill_switch"], true);
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    assert_eq!(report["state"], "stopped", "{report}");
    let sent = report["totals"]["sent"].as_u64().unwrap();
    assert!(sent > 0 && sent < 500, "sent {sent} of 1000");
    // Nothing more leaves once it stopped.
    let after = gw.seen.requests.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(gw.seen.requests.load(Ordering::SeqCst), after);
    stop_all(vec![server]).await;
}

#[tokio::test]
async fn a_closed_udp_port_is_a_transport_error() {
    let (server, app, _state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    // A UDP port nobody listens on.
    let closed = {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("socket");
        socket.local_addr().expect("addr")
    };
    let mut s = scenario(closed, 20, "1s");
    s["timeout"] = json!("300ms");
    s["thresholds"] = json!({});
    let (status, run) = start(&app, s).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    let t = &report["totals"];
    let sent = t["sent"].as_u64().unwrap();
    assert!(sent > 0, "{t}");
    assert_eq!(t["transport"], sent, "{t}");
    assert_eq!(t["upstream"], 0, "{t}");
    assert_eq!(t["gateway"], 0, "{t}");
    assert_eq!(t["ok"], 0, "{t}");
    stop_all(vec![server]).await;
}

/// TLS verification is on by the same rules as for HTTP/1 and 2: without
/// `insecure`, a certificate no web PKI root signed fails the handshake.
#[tokio::test]
async fn http3_verifies_the_certificate_unless_insecure() {
    let (server, app, state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let gw = gateway(&state, app.clone(), None);
    let mut s = scenario(gw.addr, 20, "1s");
    s["targets"][0]["insecure"] = json!(false);
    s["thresholds"] = json!({});
    let (status, run) = start(&app, s).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    let t = &report["totals"];
    let sent = t["sent"].as_u64().unwrap();
    assert!(sent > 0, "{t}");
    assert_eq!(t["transport"], sent, "{t}");
    assert_eq!(t["transport_kinds"]["tls"], sent, "{t}");
    assert_eq!(gw.seen.requests.load(Ordering::SeqCst), 0);
    stop_all(vec![server]).await;
}

// A chain hop over HTTP/3 (step 21 of `docs/plans/core-next`): the server
// role's onward call goes through the same client, to the addresses the
// chain's hops were pinned to, and everything a chain may never do stays so.

/// A hop reached through the stand-in gateway at `addr`.
fn h3_hop(addr: SocketAddr, path: &str) -> Value {
    json!({ "url": format!("https://{addr}"), "http": "h3", "insecure": true, "path": path })
}

/// A scenario through `target` whose servers call on to `hops`.
fn chain_scenario(target: Value, rate: u64, secs: &str, hops: Value) -> Value {
    json!({
        "name": "h3-chain",
        "targets": [target],
        "source": { "request": { "path": "/harness/front" } },
        "pattern": { "type": "constant", "rate": rate, "for": secs },
        "timeout": "5s",
        "servers": { "chain": { "hops": hops } }
    })
}

/// `rustybin_harness_chain_calls_total` of one hop and result of a run.
fn chain_series(state: &AppState, id: &str, hop: u64, result: &str) -> u64 {
    let line = format!(
        "rustybin_harness_chain_calls_total{{run=\"{id}\",hop=\"{hop}\",result=\"{result}\"}} "
    );
    state
        .metrics
        .render()
        .lines()
        .find_map(|l| l.strip_prefix(&line).and_then(|n| n.parse().ok()))
        .unwrap_or(0)
}

/// The run's chain as prepared on the node.
fn chain_of(state: &AppState, id: &str) -> Arc<crate::harness::chain::Chain> {
    state
        .harness
        .as_ref()
        .expect("harness")
        .correlator
        .entry(id)
        .expect("the run's entry")
        .chain()
        .expect("a prepared chain")
}

#[tokio::test]
async fn a_chain_hop_over_http3_is_called_and_reported() {
    let (server, app, state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let tcp = format!("http://{}", server.http_addr);
    let gw = gateway(&state, app.clone(), None);
    // A client target over TCP, the first hop over QUIC, the second over
    // TCP again: a mixed chain.
    let hops = json!([
        h3_hop(gw.addr, "/harness/middle"),
        { "url": tcp, "path": "/harness/back" }
    ]);
    let (status, run) = start(&app, chain_scenario(json!({ "url": tcp }), 50, "1s", hops)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    let report = wait_for_report(&app, &id).await;
    let t = &report["totals"];
    let sent = t["sent"].as_u64().unwrap();
    assert!((46..=54).contains(&sent), "sent {sent}");
    assert_eq!(t["upstream"], sent, "{t}");
    let srv = &report["server"];
    // The clients' requests only: an onward call over QUIC is no arrival.
    assert_eq!(srv["arrivals"], sent, "{srv}");
    assert_eq!(report["rejected_markers"], 0, "the onward markers verify");
    let legs = srv["chain"]["hops"].as_array().expect("hops").clone();
    assert_eq!(legs.len(), 2, "{srv}");
    for leg in &legs {
        assert_eq!(leg["calls"], sent, "{leg}");
        // The next server's tag came back over the leg and verified.
        assert_eq!(leg["upstream"], sent, "{leg}");
        assert_eq!(leg["gateway"], 0, "{leg}");
        assert_eq!(leg["transport"], 0, "{leg}");
        assert_eq!(leg["arrivals"], sent, "{leg}");
        assert_eq!(leg["statuses"]["200"], sent, "{leg}");
        assert_eq!(leg["refused"], json!({}), "{leg}");
        assert!(leg["latency"]["p50_ms"].as_f64().unwrap() > 0.0, "{leg}");
        assert_eq!(leg["overhead"]["count"], sent, "{leg}");
    }
    // The QUIC leg went to the hop's pinned address, marked, once a request.
    assert_eq!(legs[0]["pinned"], json!([gw.addr.to_string()]));
    assert_eq!(
        legs[0]["url"],
        format!("https://{}/harness/middle", gw.addr)
    );
    assert_eq!(legs[1]["pinned"], json!([server.http_addr.to_string()]));
    assert_eq!(Seen::n(&gw.seen.requests), sent);
    assert_eq!(Seen::n(&gw.seen.marked), sent);
    // Counted through the one path every onward call is.
    for hop in [1, 2] {
        assert_eq!(chain_series(&state, &id, hop, "ok"), sent);
        assert_eq!(chain_series(&state, &id, hop, "failed"), 0);
    }
    // Bounded like a target's connections, and closed with the run: for
    // good, so nothing of the chain is left to send over QUIC.
    let opened = gw.seen.all_closed().await;
    assert!(
        (1..=crate::harness::http3::CONNECTIONS as u64).contains(&opened),
        "{opened} connections"
    );
    let quic = chain_of(&state, &id).quic_of(0).expect("an h3 hop");
    assert!(quic.is_shut());
    assert_eq!(quic.open_connections(), 0);
    assert!(chain_of(&state, &id).quic_of(1).is_none(), "a TCP hop");

    // The reverse: the clients' target over QUIC, the hop over TCP.
    let before = Seen::n(&gw.seen.requests);
    let (status, run) = start(
        &app,
        chain_scenario(
            json!({ "url": format!("https://{}", gw.addr), "http": "h3", "insecure": true }),
            50,
            "1s",
            json!([{ "url": tcp, "path": "/harness/back" }]),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    let sent = report["totals"]["sent"].as_u64().unwrap();
    assert!(sent >= 45, "{report}");
    assert_eq!(report["totals"]["upstream"], sent, "{report}");
    let leg = &report["server"]["chain"]["hops"][0];
    assert_eq!(leg["calls"], sent, "{leg}");
    assert_eq!(leg["upstream"], sent, "{leg}");
    assert_eq!(leg["arrivals"], sent, "{leg}");
    // Only the clients' requests crossed the gateway: the hop went by TCP.
    assert_eq!(Seen::n(&gw.seen.requests) - before, sent);
    stop_all(vec![server]).await;
}

#[tokio::test]
async fn an_http3_hop_is_verified_over_http3() {
    let mut config = harness_config(&[Role::Client, Role::Server], &["127.0.0.1"]);
    config.harness.verify_targets = true;
    let (server, app, state) = running(config).await;
    let tcp = json!({ "url": format!("http://{}", server.http_addr) });
    // The gateway routes to this node, which serves its token: verified,
    // over QUIC (nothing listens on TCP at the gateway's port).
    let gw = gateway(&state, app.clone(), None);
    assert!(
        std::net::TcpStream::connect_timeout(&gw.addr, Duration::from_millis(200)).is_err(),
        "the stand-in gateway has no TCP listener"
    );
    let hops = json!([h3_hop(gw.addr, "/harness/back")]);
    let (status, run) = start(&app, chain_scenario(tcp.clone(), 20, "1s", hops)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    let leg = &report["server"]["chain"]["hops"][0];
    assert_eq!(leg["verified"], true, "{leg}");
    assert_eq!(leg["calls"], report["totals"]["sent"], "{leg}");
    // A hop that routes to another instance serves another token: the run
    // is refused before a single onward call.
    let (other, other_app, other_state) = running(harness_config(&[Role::Server], &[])).await;
    let elsewhere = gateway(&other_state, other_app, None);
    let hops = json!([h3_hop(elsewhere.addr, "/harness/back")]);
    let (status, v) = start(&app, chain_scenario(tcp, 20, "1s", hops)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    let problem = v["problems"][0].as_str().unwrap();
    assert!(
        problem.starts_with("servers.chain.hops[0]") && problem.contains("verification token"),
        "{v}"
    );
    // One verification request reached it, nothing of a run, and the
    // connection it took is closed with the chain that was not to be.
    assert_eq!(Seen::n(&elsewhere.seen.requests), 1);
    assert_eq!(Seen::n(&elsewhere.seen.marked), 0);
    assert_eq!(elsewhere.seen.all_closed().await, 1);
    stop_all(vec![server, other]).await;
}

/// The hop's server asks its clients to move to another address: the
/// onward calls stay on the address the hop was pinned to.
#[tokio::test]
async fn an_http3_hop_goes_to_the_pinned_address_only() {
    let (server, app, state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let decoy = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("decoy");
    let SocketAddr::V4(decoy_addr) = decoy.local_addr().expect("decoy addr") else {
        panic!("an IPv4 socket");
    };
    let gw = gateway(&state, app.clone(), Some(decoy_addr));
    let hops = json!([h3_hop(gw.addr, "/harness/back")]);
    let (status, run) = start(
        &app,
        chain_scenario(
            json!({ "url": format!("http://{}", server.http_addr) }),
            50,
            "1s",
            hops,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    let sent = report["totals"]["sent"].as_u64().unwrap();
    assert!(sent >= 45, "{report}");
    let leg = &report["server"]["chain"]["hops"][0];
    assert_eq!(leg["calls"], sent, "{leg}");
    assert_eq!(leg["upstream"], sent, "{leg}");
    assert_eq!(leg["pinned"], json!([gw.addr.to_string()]));
    assert_eq!(Seen::n(&gw.seen.requests), sent);
    // Not one datagram went to the address the server preferred.
    let mut buf = [0u8; 64];
    assert!(
        decoy.try_recv_from(&mut buf).is_err(),
        "a datagram reached the preferred address"
    );
    stop_all(vec![server]).await;
}

#[tokio::test]
async fn the_kill_switch_ends_an_http3_onward_call_and_closes_the_connections() {
    let (server, app, state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    // A hop that takes its time, so calls are under way when the switch is
    // pressed.
    let slow = Router::new().fallback(|| async {
        tokio::time::sleep(Duration::from_secs(3)).await;
        "slow"
    });
    let gw = gateway(&state, slow, None);
    let hops = json!([h3_hop(gw.addr, "/sink")]);
    let (status, run) = start(
        &app,
        chain_scenario(
            json!({ "url": format!("http://{}", server.http_addr) }),
            10,
            "20s",
            hops,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    for _ in 0..200 {
        if Seen::n(&gw.seen.requests) >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Calls are under way on open connections, and none has an answer.
    assert!(Seen::n(&gw.seen.requests) >= 3, "the hop was called");
    assert!(Seen::n(&gw.seen.opened) >= 1);
    assert_eq!(Seen::n(&gw.seen.closed), 0, "open while the run is on");
    assert_eq!(chain_series(&state, &id, 1, "ok"), 0);
    let pressed = std::time::Instant::now();
    let (status, v) = send(&app, Method::POST, "/_rustybin/harness/stop", None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    // Every connection to the hop is closed at once, not when the hop
    // would have answered.
    gw.seen.all_closed().await;
    assert!(pressed.elapsed() < Duration::from_secs(2), "closed late");
    let hits = Seen::n(&gw.seen.requests);
    let report = wait_for_report(&app, &id).await;
    assert_eq!(report["state"], "stopped", "{report}");
    assert_eq!(Seen::n(&gw.seen.requests), hits, "a call after the switch");
    let leg = &report["server"]["chain"]["hops"][0];
    // The calls under way ended with the switch: stopped, not failed.
    let stopped = leg["transport_kinds"]["stopped"].as_u64().unwrap_or(0);
    assert!(stopped >= 1, "{leg}");
    assert_eq!(leg["transport"], stopped, "{leg}");
    let refused = &leg["refused"];
    assert!(
        refused["kill_switch"].as_u64().unwrap_or(0) + refused["not_live"].as_u64().unwrap_or(0)
            >= stopped,
        "{leg}"
    );
    assert_eq!(chain_series(&state, &id, 1, "failed"), 0);
    assert_eq!(chain_series(&state, &id, 1, "ok"), 0);
    // And nothing was opened again afterwards.
    gw.seen.all_closed().await;
    let quic = chain_of(&state, &id).quic_of(0).expect("an h3 hop");
    assert_eq!(quic.open_connections(), 0);
    stop_all(vec![server]).await;
}

/// A cluster's leader says the run is not live while onward calls are under
/// way over QUIC: their connections are closed under them, and they are
/// counted as stopped for that reason, as over HTTP/1 and 2, not as calls
/// the hop failed.
#[tokio::test]
async fn an_http3_call_under_way_when_the_run_is_not_live_is_stopped_not_failed() {
    let (server, app, state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["127.0.0.1"],
    ))
    .await;
    let slow = Router::new().fallback(|| async {
        tokio::time::sleep(Duration::from_secs(3)).await;
        "slow"
    });
    let gw = gateway(&state, slow, None);
    let hops = json!([h3_hop(gw.addr, "/sink")]);
    let (status, run) = start(
        &app,
        chain_scenario(
            json!({ "url": format!("http://{}", server.http_addr) }),
            20,
            "20s",
            hops,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    for _ in 0..200 {
        if Seen::n(&gw.seen.requests) >= 8 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let under_way = Seen::n(&gw.seen.requests);
    assert!(under_way >= 8, "the hop was called");
    let entry = state
        .harness
        .as_ref()
        .expect("harness")
        .correlator
        .entry(&id)
        .expect("the run's entry");
    // What a member does at its leader's word.
    entry.chain_not_live();
    gw.seen.all_closed().await;
    let mut leg = json!(null);
    for _ in 0..100 {
        leg = serde_json::to_value(&entry.chain_report().expect("a chain").hops[0]).expect("json");
        if leg["calls"].as_u64().unwrap_or(0) >= under_way {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let stopped = leg["transport_kinds"]["stopped"].as_u64().unwrap_or(0);
    assert!(stopped >= under_way, "{leg}");
    assert_eq!(leg["transport"], stopped, "nothing failed: {leg}");
    assert_eq!(leg["refused"]["kill_switch"], json!(null), "{leg}");
    assert!(leg["refused"]["not_live"].as_u64().unwrap_or(0) >= stopped);
    assert_eq!(chain_series(&state, &id, 1, "failed"), 0);
    assert!(chain_series(&state, &id, 1, "not_live") >= stopped);
    let (status, v) = send(&app, Method::POST, "/_rustybin/harness/stop", None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    wait_for_report(&app, &id).await;
    stop_all(vec![server]).await;
}

/// The kill switch closes the QUIC connections of a chain whose run goes
/// on (a server node of a cluster, whose run is its leader's to end), and
/// the end of the run closes them for good.
#[tokio::test]
async fn a_chains_quic_connections_close_on_the_kill_switch_and_for_good_at_the_end() {
    let mut config = harness_config(&[Role::Client, Role::Server], &["127.0.0.1"]);
    config.harness.verify_targets = true;
    let (server, app, state) = running(config).await;
    let gw = gateway(&state, app.clone(), None);
    let h = state.harness.clone().expect("harness");
    let declared: crate::harness::scenario::Chain =
        serde_json::from_value(json!({ "hops": [h3_hop(gw.addr, "/harness/back")] }))
            .expect("a chain");
    let chain = crate::harness::chain::Chain::prepare(&h, &declared, &h.verification_token)
        .await
        .expect("prepared");
    let quic = chain.quic_of(0).expect("an h3 hop");
    // The verification went over QUIC and left its connection.
    assert_eq!(Seen::n(&gw.seen.requests), 1);
    assert_eq!(Seen::n(&gw.seen.opened), 1);
    assert_eq!(Seen::n(&gw.seen.closed), 0);
    assert_eq!(quic.open_connections(), 1);
    let get = || {
        hyper::Request::builder()
            .method("GET")
            .uri(format!("https://{}/status/200", gw.addr))
            .body(http_body_util::BodyExt::boxed(http_body_util::Full::new(
                bytes::Bytes::new(),
            )))
            .expect("request")
    };
    // The kill switch: closed, with no call under way to notice it.
    h.kill.set();
    assert_eq!(gw.seen.all_closed().await, 1);
    assert_eq!(quic.open_connections(), 0);
    // Cleared, a run that is still live opens a new connection, to the
    // pinned address.
    h.kill.clear();
    let answer = quic.request(get(), false).await.expect("an answer");
    assert_eq!(answer.status, 200);
    assert_eq!(Seen::n(&gw.seen.opened), 2);
    assert_eq!(quic.open_connections(), 1);
    // The run is not live (a leader's word): closed, and open again later.
    chain.close_connections();
    assert_eq!(gw.seen.all_closed().await, 2);
    let answer = quic.request(get(), false).await.expect("an answer");
    assert_eq!(answer.status, 200);
    assert_eq!(Seen::n(&gw.seen.opened), 3);
    // The run ended: closed for good.
    chain.shut();
    assert_eq!(gw.seen.all_closed().await, 3);
    assert_eq!(quic.request(get(), false).await.err(), Some("connect"));
    assert_eq!(Seen::n(&gw.seen.opened), 3, "nothing opens after the end");
    assert_eq!(Seen::n(&gw.seen.requests), 3);
    drop(chain);
    stop_all(vec![server]).await;
}
