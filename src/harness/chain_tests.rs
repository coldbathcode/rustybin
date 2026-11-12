//! Server-to-server chains: what a chain does, and above all what can never
//! make a server send. A stranger's request, a forged or replayed marker, a
//! run that ended, a hop nobody named, a depth past the fixed one, a repeat,
//! the kill switch, a hop that is not allowed or not verified: each makes no
//! onward call. Everything is on loopback; rates stay low.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::harness::cluster_tests::{
    client_config, join, node, wait_for_alive, wait_for_cluster_report, TOKEN,
};
use crate::harness::correlate::{self, Marker, RunKey};
use crate::harness::tests::{harness_config, running, send, wait_for_report};
use crate::harness::Role;
use crate::state::AppState;

/// Something a hop points at: counts what reaches it and keeps the last
/// request (a plain server, not a Rustybin: its answers carry no tag).
struct Sink {
    addr: SocketAddr,
    hits: Arc<AtomicU64>,
    last: Arc<Mutex<Option<(String, HeaderMap)>>>,
}

impl Sink {
    fn hits(&self) -> u64 {
        self.hits.load(Ordering::SeqCst)
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

async fn sink(delay: Duration) -> Sink {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("sink");
    let addr = listener.local_addr().expect("sink addr");
    let hits = Arc::new(AtomicU64::new(0));
    let last = Arc::new(Mutex::new(None));
    let (h, l) = (hits.clone(), last.clone());
    let app = Router::new().fallback(move |req: Request<Body>| {
        let (h, l) = (h.clone(), l.clone());
        async move {
            h.fetch_add(1, Ordering::SeqCst);
            *l.lock().expect("last") = Some((req.uri().to_string(), req.headers().clone()));
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            "sink"
        }
    });
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Sink { addr, hits, last }
}

/// A scenario through `target` whose servers call on to `hops`.
fn scenario(target: &str, rate: u64, secs: &str, hops: Value) -> Value {
    json!({
        "name": "chain",
        "target": target,
        "source": { "request": { "path": "/harness/front" } },
        "pattern": { "type": "constant", "rate": rate, "for": secs },
        "timeout": "5s",
        "servers": { "chain": { "hops": hops } }
    })
}

async fn start(app: &Router, scenario: Value) -> (StatusCode, Value) {
    send(app, Method::POST, "/_rustybin/harness/runs", Some(scenario)).await
}

/// The key and the client node of a run on `state`'s node.
fn run_key(state: &AppState, id: &str) -> (RunKey, String) {
    let h = state.harness.as_ref().expect("harness");
    (h.run(id).expect("the run").key().clone(), h.node_id.clone())
}

/// A marker signed with `key`, as a client of the run (or, with `hop`, a
/// server of it) would sign it for `GET path` without headers.
fn signed(key: &RunKey, run: &str, node: &str, seq: u64, hop: u8, path: &str) -> String {
    let names: Vec<String> = Vec::new();
    let (headers_digest, _) = correlate::digest_headers(&names, &HeaderMap::new());
    let mut m = Marker {
        run: run.to_string(),
        node: node.to_string(),
        seq,
        vu: 0,
        layer: 0,
        sent_ns: correlate::now_ns(),
        names,
        headers_digest,
        body_digest: String::new(),
        path_digest: correlate::digest_path(&Method::GET, path),
        hop,
        sig: String::new(),
    };
    m.sign(key);
    m.to_header()
}

/// A request from outside the run, straight to the node.
async fn knock(app: &Router, path: &str, headers: &[(&str, &str)]) -> StatusCode {
    let mut req = Request::builder().method(Method::GET).uri(path);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    app.clone()
        .oneshot(req.body(Body::empty()).expect("request"))
        .await
        .expect("response")
        .status()
}

/// Wait until the run on the node has sent something (it is live).
async fn wait_until_sending(app: &Router, id: &str) {
    for _ in 0..200 {
        let (_, run) = send(
            app,
            Method::GET,
            &format!("/_rustybin/harness/runs/{id}"),
            None,
        )
        .await;
        if run["sent"].as_u64().unwrap_or(0) >= 2 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("run {id} never sent");
}

/// The chain's numbers on the node, read from the run's entry (also after
/// the run, and for what was refused then).
fn chain_of(state: &AppState, id: &str) -> Value {
    let h = state.harness.as_ref().expect("harness");
    let report = h
        .correlator
        .entry(id)
        .expect("the run's entry")
        .chain_report()
        .expect("a chain");
    serde_json::to_value(report).expect("json")
}

/// The series of a run's onward calls: `(hop, result)` to its count.
type Series = BTreeMap<(u64, String), u64>;

/// `rustybin_harness_chain_calls_total` of the run as the node exports it.
fn series_of(state: &AppState, id: &str) -> Series {
    let prefix = format!("rustybin_harness_chain_calls_total{{run=\"{id}\",hop=\"");
    state
        .metrics
        .render()
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix(&prefix)?;
            let (hop, rest) = rest.split_once("\",result=\"")?;
            let (result, n) = rest.split_once("\"} ")?;
            Some((
                (hop.parse().expect("hop"), result.to_string()),
                n.parse().expect("count"),
            ))
        })
        .collect()
}

/// What the series must be for a node's chain numbers (`chain_of`): per hop
/// `ok` the calls that were answered, `failed` those that got no answer,
/// and every refusal under its reason (a call stopped under way is a
/// refusal, as in the report). Nothing that is zero.
fn series_for(chain: &Value) -> Series {
    let mut want = Series::new();
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    for leg in chain["hops"].as_array().expect("hops") {
        let hop = n(&leg["hop"]);
        let ok = n(&leg["upstream"]) + n(&leg["gateway"]);
        let failed = n(&leg["transport"]) - n(&leg["transport_kinds"]["stopped"]);
        let mut put = |result: &str, count: u64| {
            if count > 0 {
                want.insert((hop, result.to_string()), count);
            }
        };
        put("ok", ok);
        put("failed", failed);
        for (why, count) in leg["refused"].as_object().expect("refused") {
            put(why, n(count));
        }
    }
    want
}

/// The node's series say what its chain numbers say (the two are counted
/// one after the other, so a call ending right now is given a moment).
async fn series_agree(state: &AppState, id: &str) -> Series {
    for _ in 0..100 {
        if series_of(state, id) == series_for(&chain_of(state, id)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let series = series_of(state, id);
    assert_eq!(series, series_for(&chain_of(state, id)));
    series
}

fn at(series: &Series, hop: u64, result: &str) -> u64 {
    series.get(&(hop, result.to_string())).copied().unwrap_or(0)
}

async fn stop_all(servers: Vec<crate::RunningServer>) {
    for s in servers {
        s.shutdown();
        let _ = s.wait().await;
    }
}

fn both() -> crate::config::Config {
    harness_config(&[Role::Client, Role::Server], &["127.0.0.1"])
}

#[tokio::test]
async fn a_chain_reaches_every_hop_and_reports_each_leg() {
    let (server, app, state) = running(both()).await;
    let url = format!("http://{}", server.http_addr);
    let hops = json!([
        { "url": url, "path": "/harness/middle" },
        { "url": url, "path": "/harness/back" }
    ]);
    // The preview and the summary show the hops before anything is sent.
    let (status, preview) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/preview",
        Some(scenario(&url, 100, "1s", hops.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["chain"]["calls_per_request"], 2, "{preview}");
    assert_eq!(
        preview["chain"]["hops"][0]["url"],
        format!("{url}/harness/middle")
    );
    assert_eq!(preview["chain"]["hops"][1]["hop"], 2);
    let (_, valid) = send(
        &app,
        Method::POST,
        "/_rustybin/harness/validate",
        Some(scenario(&url, 100, "1s", hops.clone())),
    )
    .await;
    assert_eq!(valid["valid"], true, "{valid}");
    assert_eq!(valid["summary"]["chain"].as_array().unwrap().len(), 2);

    let (status, run) = start(&app, scenario(&url, 100, "1s", hops)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    let report = wait_for_report(&app, &id).await;
    let t = &report["totals"];
    let sent = t["sent"].as_u64().unwrap();
    assert!((96..=104).contains(&sent), "sent {sent}");
    assert_eq!(t["ok"], sent, "{t}");
    assert_eq!(t["upstream"], sent, "{t}");
    // The clients' requests, and only theirs: an onward call is not an
    // arrival of the run, so lost and upstream_reach keep their meaning.
    let srv = &report["server"];
    assert_eq!(srv["arrivals"], sent, "{srv}");
    assert_eq!(report["rejected_markers"], 0, "the onward markers verify");
    let chain = &srv["chain"];
    let legs = chain["hops"].as_array().expect("hops");
    assert_eq!(legs.len(), 2, "{chain}");
    for (i, leg) in legs.iter().enumerate() {
        assert_eq!(leg["hop"], i as u64 + 1);
        assert_eq!(leg["calls"], sent, "{leg}");
        // The next server's tag verified: the leg reached a server.
        assert_eq!(leg["upstream"], sent, "{leg}");
        assert_eq!(leg["gateway"], 0, "{leg}");
        assert_eq!(leg["transport"], 0, "{leg}");
        assert_eq!(leg["arrivals"], sent, "{leg}");
        assert_eq!(leg["statuses"]["200"], sent, "{leg}");
        assert_eq!(leg["refused"], json!({}), "{leg}");
        assert_eq!(leg["pinned"], json!([server.http_addr.to_string()]));
        assert!(leg["latency"]["p50_ms"].as_f64().unwrap() > 0.0, "{leg}");
        assert!(leg["upstream_latency"]["p50_ms"].as_f64().unwrap() > 0.0);
        // The gateway's part of the leg, measured on every tagged answer.
        assert_eq!(leg["overhead"]["count"], sent, "{leg}");
    }
    assert_eq!(legs[0]["url"], format!("{url}/harness/middle"));
    // A server's processing time includes its dependency: the first hop's
    // leg holds the second's.
    assert!(
        legs[0]["latency"]["max_ms"].as_f64().unwrap()
            >= legs[1]["latency"]["p50_ms"].as_f64().unwrap()
    );
    let (_, text) = send(
        &app,
        Method::GET,
        &format!("/_rustybin/harness/runs/{id}/report?format=text"),
        None,
    )
    .await;
    assert!(text.as_str().unwrap().contains("chain hop 2"), "{text}");
    // The series of the onward calls: one per hop and result, the report's
    // numbers, and nothing else of the run.
    let series = series_agree(&state, &id).await;
    assert_eq!(
        series,
        Series::from([((1, "ok".to_string()), sent), ((2, "ok".to_string()), sent)])
    );
    let metrics = state.metrics.render();
    assert_eq!(
        metrics
            .matches("# TYPE rustybin_harness_chain_calls_total counter")
            .count(),
        1,
        "{metrics}"
    );
    // The onward calls are not arrivals of the run in the series either.
    assert!(
        metrics.contains(&format!(
            "rustybin_harness_server_arrivals_total{{run=\"{id}\"}} {sent}\n"
        )),
        "{metrics}"
    );
    stop_all(vec![server]).await;
}

#[tokio::test]
async fn a_request_without_a_valid_marker_makes_no_onward_call() {
    let (server, app, state) = running(both()).await;
    let url = format!("http://{}", server.http_addr);
    let hop = sink(Duration::ZERO).await;
    let hops = json!([{ "url": hop.url(), "path": "/sink" }]);
    let (status, run) = start(&app, scenario(&url, 10, "2s", hops)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    wait_until_sending(&app, &id).await;
    let (key, node) = run_key(&state, &id);
    // A stranger, while the run is live: no marker.
    assert_eq!(knock(&app, "/harness/front", &[]).await, StatusCode::OK);
    // A marker that is not one.
    let garbage = [("x-rustybin-harness", "v1; run=whatever; sig=nope")];
    assert_eq!(
        knock(&app, "/harness/front", &garbage).await,
        StatusCode::OK
    );
    // The run's id and a well-formed marker, signed with another key.
    let forged = signed(&RunKey::random(), &id, &node, 900_001, 0, "/harness/front");
    let forged = [("x-rustybin-harness", forged.as_str())];
    assert_eq!(knock(&app, "/harness/front", &forged).await, StatusCode::OK);
    // A run this node does not know.
    let unknown = signed(&key, "norun", &node, 900_002, 0, "/harness/front");
    let unknown = [("x-rustybin-harness", unknown.as_str())];
    assert_eq!(
        knock(&app, "/harness/front", &unknown).await,
        StatusCode::OK
    );
    // A marker of the run with a hop count put in afterwards.
    let raised =
        signed(&key, &id, &node, 900_003, 0, "/harness/front").replace("; sig=", "; hop=1; sig=");
    let raised = [("x-rustybin-harness", raised.as_str())];
    assert_eq!(knock(&app, "/harness/front", &raised).await, StatusCode::OK);
    // A replay inside the run: a request the run already sent (the very
    // signature a gateway saw) calls nothing a second time.
    let replay = signed(&key, &id, &node, 0, 0, "/harness/front");
    let replay = [("x-rustybin-harness", replay.as_str())];
    assert_eq!(knock(&app, "/harness/front", &replay).await, StatusCode::OK);

    let report = wait_for_report(&app, &id).await;
    let sent = report["totals"]["sent"].as_u64().unwrap();
    assert!(sent >= 15, "{report}");
    let leg = &report["server"]["chain"]["hops"][0];
    // Exactly one onward call per request the run sent, and not one more.
    assert_eq!(hop.hits(), sent, "{leg}");
    assert_eq!(leg["calls"], sent, "{leg}");
    assert_eq!(leg["refused"]["seen_before"], 1, "{leg}");
    // A plain server between: answered, without the run's tag.
    assert_eq!(leg["gateway"], sent, "{leg}");
    assert_eq!(leg["upstream"], 0, "{leg}");
    assert_eq!(report["rejected_markers"], 4, "{report}");
    stop_all(vec![server]).await;
}

#[tokio::test]
async fn a_marker_of_an_ended_run_makes_no_onward_call() {
    let (server, app, state) = running(both()).await;
    let url = format!("http://{}", server.http_addr);
    let hop = sink(Duration::ZERO).await;
    let hops = json!([{ "url": hop.url(), "path": "/sink" }]);
    let (status, run) = start(&app, scenario(&url, 20, "1s", hops)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    let (key, node) = run_key(&state, &id);
    let report = wait_for_report(&app, &id).await;
    let sent = report["totals"]["sent"].as_u64().unwrap();
    assert_eq!(hop.hits(), sent);
    // The run is over. Its key still signs, the node still knows the run:
    // a marker that verifies (a new sequence number, so it is no repeat)
    // and a replayed one both call nothing.
    for seq in [1_000_000, 0] {
        let late = signed(&key, &id, &node, seq, 0, "/harness/front");
        let late = [("x-rustybin-harness", late.as_str())];
        assert_eq!(knock(&app, "/harness/front", &late).await, StatusCode::OK);
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(hop.hits(), sent, "a call after the run ended");
    let chain = chain_of(&state, &id);
    assert_eq!(chain["hops"][0]["refused"]["not_live"], 2, "{chain}");
    assert_eq!(chain["hops"][0]["calls"], sent, "{chain}");
    stop_all(vec![server]).await;
}

#[test]
fn a_forged_hop_count_does_not_verify() {
    let key = RunKey::random();
    // A client's marker is what it was before chains: no hop anywhere.
    let client = signed(&key, "run1", "node-1", 7, 0, "/x");
    assert!(!client.contains("hop="), "{client}");
    assert!(Marker::parse(&client).expect("parses").verify(&key));
    // A server's onward marker says its hop, signed.
    let onward = signed(&key, "run1", "node-1", 7, 2, "/x");
    assert!(onward.contains("; hop=2; "), "{onward}");
    let parsed = Marker::parse(&onward).expect("parses");
    assert_eq!(parsed.hop, 2);
    assert!(parsed.verify(&key));
    // Raised, lowered, taken off or put on: the signature no longer holds.
    for forged in [
        onward.replace("hop=2", "hop=1"),
        onward.replace("hop=2", "hop=3"),
        onward.replace(" hop=2;", ""),
        client.replace("; sig=", "; hop=1; sig="),
    ] {
        let m = Marker::parse(&forged).expect("well formed");
        assert!(!m.verify(&key), "{forged}");
    }
    // Past the fixed depth, or written another way: not a marker at all.
    for bad in ["hop=4", "hop=0", "hop=01", "hop=-1", "hop=255", "hop=x"] {
        assert!(
            Marker::parse(&onward.replace("hop=2", bad)).is_none(),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn a_hop_not_named_by_the_scenario_is_never_called() {
    let (server, app, state) = running(both()).await;
    let url = format!("http://{}", server.http_addr);
    let named = sink(Duration::ZERO).await;
    // Allowed by the node's list, and never named by the scenario.
    let decoy = sink(Duration::ZERO).await;
    let hops = json!([{ "url": named.url(), "path": "/sink" }]);
    let (status, run) = start(&app, scenario(&url, 10, "2s", hops)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    wait_until_sending(&app, &id).await;
    let (key, node) = run_key(&state, &id);
    // A request of the run (its marker verifies) that names another place
    // every way a request can: the path, the query, the headers.
    let elsewhere = decoy.url();
    let path = format!("/harness/front?next={elsewhere}/steal&url={elsewhere}");
    let marker = signed(&key, &id, &node, 900_000, 0, &path);
    let status = knock(
        &app,
        &path,
        &[
            ("x-rustybin-harness", marker.as_str()),
            ("host", &decoy.addr.to_string()),
            ("x-forwarded-host", &decoy.addr.to_string()),
            ("x-forwarded-for", "203.0.113.9"),
            ("forwarded", &format!("host={}", decoy.addr)),
            ("referer", &elsewhere),
            ("location", &elsewhere),
            ("x-rustybin-next", &elsewhere),
            ("x-secret", "carried-nowhere"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let report = wait_for_report(&app, &id).await;
    let sent = report["totals"]["sent"].as_u64().unwrap();
    assert_eq!(decoy.hits(), 0, "a hop nobody named was called");
    // The request was the run's: it made its one call, to the named hop.
    assert_eq!(named.hits(), sent + 1);
    let (uri, headers) = named.last.lock().unwrap().clone().expect("a request");
    assert_eq!(uri, "/sink", "the hop's own path, nothing of the request's");
    assert_eq!(headers["host"], named.addr.to_string());
    for carried in [
        "x-forwarded-host",
        "x-forwarded-for",
        "forwarded",
        "referer",
        "location",
        "x-rustybin-next",
        "x-secret",
    ] {
        assert!(!headers.contains_key(carried), "{carried} was handed on");
    }
    // What it does carry: the run's marker with the next hop count.
    let onward = Marker::parse(headers["x-rustybin-harness"].to_str().unwrap()).expect("marker");
    assert_eq!(onward.hop, 1);
    assert!(onward.verify(&key));
    stop_all(vec![server]).await;
}

#[tokio::test]
async fn the_depth_is_fixed_and_a_repeat_calls_nothing() {
    let (server, app, state) = running(both()).await;
    let url = format!("http://{}", server.http_addr);
    let hop = |path: &str| json!({ "url": url, "path": path });
    // Four hops, or the same hop twice: refused before anything runs.
    let (status, v) = start(
        &app,
        scenario(
            &url,
            10,
            "1s",
            json!([
                hop("/harness/1"),
                hop("/harness/2"),
                hop("/harness/3"),
                hop("/harness/4")
            ]),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(
        v["problems"][0]
            .as_str()
            .unwrap()
            .contains("at most 3 hops"),
        "{v}"
    );
    let (status, v) = start(
        &app,
        scenario(
            &url,
            10,
            "1s",
            json!([hop("/harness/1"), hop("/harness/1")]),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(
        v["problems"][0]
            .as_str()
            .unwrap()
            .contains("the same hop twice"),
        "{v}"
    );

    // The deepest chain there is, every hop routed back to this node.
    let (status, run) = start(
        &app,
        scenario(
            &url,
            25,
            "2s",
            json!([hop("/harness/1"), hop("/harness/2"), hop("/harness/3")]),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    wait_until_sending(&app, &id).await;
    let (key, node) = run_key(&state, &id);
    // An onward call at the end of the chain (a marker a server of the
    // run would sign): it arrives, and calls nothing.
    let last = signed(&key, &id, &node, 900_000, 3, "/harness/3");
    let last = [("x-rustybin-harness", last.as_str())];
    assert_eq!(knock(&app, "/harness/3", &last).await, StatusCode::OK);
    // A repeat of an onward call the run already made (a gateway's retry,
    // a loop back to this server): nothing a second time.
    let again = signed(&key, &id, &node, 0, 2, "/harness/2");
    let again = [("x-rustybin-harness", again.as_str())];
    assert_eq!(knock(&app, "/harness/2", &again).await, StatusCode::OK);

    let report = wait_for_report(&app, &id).await;
    let sent = report["totals"]["sent"].as_u64().unwrap();
    assert!(sent >= 40, "{report}");
    let legs = report["server"]["chain"]["hops"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(legs.len(), 3);
    // One call per request at each hop: three per request, never more.
    for leg in &legs {
        assert_eq!(leg["calls"], sent, "{leg}");
        assert_eq!(leg["upstream"], sent, "{leg}");
    }
    assert_eq!(legs[0]["arrivals"], sent);
    assert_eq!(legs[1]["arrivals"], sent + 1, "the repeat arrived");
    assert_eq!(
        legs[2]["arrivals"],
        sent + 1,
        "the injected last hop arrived"
    );
    assert_eq!(legs[2]["refused"]["seen_before"], 1, "{legs:?}");
    assert_eq!(report["server"]["arrivals"], sent);
    // The series: a call per request at each of the three hops, and the
    // repeat under its reason.
    let series = series_agree(&state, &id).await;
    for hop in 1..=3 {
        assert_eq!(at(&series, hop, "ok"), sent, "{series:?}");
    }
    assert_eq!(at(&series, 3, "seen_before"), 1, "{series:?}");
    assert_eq!(series.len(), 4, "{series:?}");
    stop_all(vec![server]).await;
}

#[tokio::test]
async fn the_kill_switch_stops_onward_calls() {
    let (server, app, state) = running(both()).await;
    let url = format!("http://{}", server.http_addr);
    // A hop that takes its time, so calls are under way when the switch
    // is pressed.
    let hop = sink(Duration::from_secs(3)).await;
    let hops = json!([{ "url": hop.url(), "path": "/sink" }]);
    let (status, run) = start(&app, scenario(&url, 10, "20s", hops)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let id = run["run_id"].as_str().unwrap().to_string();
    wait_until_sending(&app, &id).await;
    let (key, node) = run_key(&state, &id);
    tokio::time::sleep(Duration::from_millis(300)).await;
    // The run is on and its calls are under way: nothing has an answer.
    let before = series_of(&state, &id);
    let stopped_by = |s: &Series| at(s, 1, "kill_switch") + at(s, 1, "not_live");
    assert_eq!(stopped_by(&before), 0, "{before:?}");
    let (status, v) = send(&app, Method::POST, "/_rustybin/harness/stop", None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    // With the switch set, a request of the run calls nothing.
    let hits = hop.hits();
    let late = signed(&key, &id, &node, 900_000, 0, "/harness/front");
    let late = [("x-rustybin-harness", late.as_str())];
    assert_eq!(knock(&app, "/harness/front", &late).await, StatusCode::OK);
    // The series while the run winds down: the refusals rose (the calls
    // under way and the late request), `ok` did not.
    for _ in 0..100 {
        if stopped_by(&series_of(&state, &id)) >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let after = series_of(&state, &id);
    assert!(stopped_by(&after) >= 2, "{after:?}");
    assert!(at(&after, 1, "kill_switch") >= 1, "{after:?}");
    assert_eq!(at(&after, 1, "ok"), at(&before, 1, "ok"), "{after:?}");
    let report = wait_for_report(&app, &id).await;
    assert_eq!(report["state"], "stopped", "{report}");
    // Nothing left after the switch, and the calls under way ended with
    // it (`stopped`), not with the hop's three seconds.
    assert_eq!(hop.hits(), hits, "a call after the kill switch");
    let chain = chain_of(&state, &id);
    let leg = &chain["hops"][0];
    assert!(
        leg["transport_kinds"]["stopped"].as_u64().unwrap_or(0) >= 1,
        "{leg}"
    );
    let refused = &leg["refused"];
    assert!(
        refused["kill_switch"].as_u64().unwrap_or(0) + refused["not_live"].as_u64().unwrap_or(0)
            >= 2,
        "{leg}"
    );
    let series = series_agree(&state, &id).await;
    assert_eq!(at(&series, 1, "ok"), at(&before, 1, "ok"), "{series:?}");
    assert_eq!(at(&series, 1, "failed"), 0, "a stopped call: {series:?}");
    stop_all(vec![server]).await;
}

#[tokio::test]
async fn a_hop_outside_the_allowlist_refuses_the_run() {
    // The target is allowed by name; a hop must be allowed too.
    let (server, app, _state) = running(harness_config(
        &[Role::Client, Role::Server],
        &["localhost"],
    ))
    .await;
    let url = format!("http://localhost:{}", server.http_addr.port());
    let hop = sink(Duration::ZERO).await;
    for (hops, needle) in [
        (
            json!([{ "url": hop.url(), "path": "/sink" }]),
            "not in RUSTYBIN_HARNESS_TARGETS",
        ),
        (json!(["http://169.254.169.254/latest"]), "metadata"),
        // One hop refused refuses the chain whole.
        (
            json!([{ "url": url, "path": "/harness/ok" }, hop.url()]),
            "servers.chain.hops[1].url",
        ),
    ] {
        let (status, v) = start(&app, scenario(&url, 10, "1s", hops)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
        assert!(
            v["problems"][0].as_str().unwrap().contains(needle),
            "{needle}: {v}"
        );
        assert!(v["error"].as_str().unwrap().contains("chain hop"), "{v}");
    }
    // A control plane is never a hop, whatever the list says.
    let (status, v) = start(
        &app,
        scenario(
            &url,
            10,
            "1s",
            json!([{ "url": url, "path": "/_rustybin/harness/stop" }]),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(
        v["problems"][0].as_str().unwrap().contains("control plane"),
        "{v}"
    );
    let (_, list) = send(&app, Method::GET, "/_rustybin/harness/runs", None).await;
    assert_eq!(list["runs"].as_array().unwrap().len(), 0, "nothing started");
    assert_eq!(hop.hits(), 0);
    // A node without the server role has nobody to make the calls.
    let (client, client_app, _) = running(client_config(1000)).await;
    let (status, v) = start(
        &client_app,
        scenario(
            &format!("http://{}", server.http_addr),
            10,
            "1s",
            json!([hop.url()]),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert!(
        v["problems"][0].as_str().unwrap().contains("server role"),
        "{v}"
    );
    stop_all(vec![server, client]).await;
}

#[tokio::test]
async fn an_unverified_hop_refuses_the_run() {
    let mut config = both();
    config.harness.verify_targets = true;
    let (server, app, _state) = running(config).await;
    let url = format!("http://{}", server.http_addr);
    // Another instance serves another token, a plain server none at all.
    let (other, _, _) = running(harness_config(&[Role::Server], &[])).await;
    let plain = sink(Duration::ZERO).await;
    for (hop, needle) in [
        (format!("http://{}", other.http_addr), "verification token"),
        (plain.url(), "verification token"),
    ] {
        let (status, v) = start(&app, scenario(&url, 10, "1s", json!([hop]))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
        let problem = v["problems"][0].as_str().unwrap();
        assert!(
            problem.starts_with("servers.chain.hops[0]") && problem.contains(needle),
            "{v}"
        );
    }
    // The verification asked for the token and nothing else.
    assert_eq!(plain.hits(), 1);
    let (uri, _) = plain.last.lock().unwrap().clone().expect("a request");
    assert_eq!(uri, "/.well-known/rustybin-harness");
    // A hop that routes back to this node serves its token: verified.
    let hops = json!([{ "url": url, "path": "/harness/back" }]);
    let (status, run) = start(&app, scenario(&url, 20, "1s", hops)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    let leg = &report["server"]["chain"]["hops"][0];
    assert_eq!(leg["verified"], true, "{leg}");
    assert_eq!(leg["calls"], report["totals"]["sent"], "{leg}");
    stop_all(vec![server, other]).await;
}

#[tokio::test]
async fn onward_calls_are_bounded_per_run() {
    let (server, app, state) = running(both()).await;
    let url = format!("http://{}", server.http_addr);
    let slow = sink(Duration::from_millis(400)).await;
    // One call in flight at a time: the rest are refused, not queued.
    let mut s = scenario(
        &url,
        20,
        "1s",
        json!([{ "url": slow.url(), "path": "/sink" }]),
    );
    s["servers"]["chain"]["max_concurrent"] = json!(1);
    let (status, run) = start(&app, s).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    let sent = report["totals"]["sent"].as_u64().unwrap();
    let leg = &report["server"]["chain"]["hops"][0];
    let calls = leg["calls"].as_u64().unwrap();
    let busy = leg["refused"]["busy"].as_u64().unwrap_or(0);
    assert!((1..=5).contains(&calls), "{leg}");
    assert_eq!(calls + busy, sent, "{leg}");
    assert_eq!(slow.hits(), calls);
    // The clients' requests were answered all the same.
    assert_eq!(report["totals"]["ok"], sent);
    // The series: the answered calls and the refused ones are the requests.
    let series = series_agree(&state, run["run_id"].as_str().unwrap()).await;
    assert_eq!(at(&series, 1, "ok"), calls, "{series:?}");
    assert_eq!(at(&series, 1, "busy"), busy, "{series:?}");
    // A call is given its timeout and no longer.
    let mut s = scenario(
        &url,
        20,
        "1s",
        json!([{ "url": slow.url(), "path": "/sink" }]),
    );
    s["servers"]["chain"]["timeout"] = json!("100ms");
    let (status, run) = start(&app, s).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let report = wait_for_report(&app, run["run_id"].as_str().unwrap()).await;
    let sent = report["totals"]["sent"].as_u64().unwrap();
    let leg = &report["server"]["chain"]["hops"][0];
    assert_eq!(leg["calls"], sent, "{leg}");
    assert_eq!(leg["transport_kinds"]["timeout"], sent, "{leg}");
    assert!(leg["latency"]["max_ms"].as_f64().unwrap() < 400.0, "{leg}");
    // A call without an answer is `failed`, under its own run.
    let series = series_agree(&state, run["run_id"].as_str().unwrap()).await;
    assert_eq!(
        series,
        Series::from([((1, "failed".to_string()), sent)]),
        "{series:?}"
    );
    stop_all(vec![server]).await;
}

#[tokio::test]
async fn a_cluster_hands_the_chain_to_its_servers() {
    let l = node(harness_config(&[Role::Coordinator], &[])).await;
    let (status, cluster) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters",
        Some(json!({ "cluster_id": "chain-1", "join_token": TOKEN })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{cluster}");
    let join_url = format!("{}/_rustybin/harness/clusters/chain-1", l.base());
    let c = node(client_config(1000)).await;
    // A server's own allowlist decides what it may call on to.
    let front = node(harness_config(&[Role::Server], &["127.0.0.1"])).await;
    let back = node(harness_config(&[Role::Server], &["127.0.0.1"])).await;
    for n in [&c, &front, &back] {
        join(n, &join_url).await;
    }
    wait_for_alive(&l, "chain-1", 1, 2).await;
    let mut s = scenario(
        &front.base(),
        100,
        "1s",
        json!([{ "url": back.base(), "path": "/harness/back" }]),
    );
    s["scale"] = json!({ "client_nodes": 1, "server_nodes": 2 });
    let (status, run) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters/chain-1/runs",
        Some(s),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{run}");
    let run_id = run["run_id"].as_str().unwrap().to_string();
    // Once started, the servers learn the run with its chain from the
    // leader and prepare it, each against its own list.
    let (status, started) = send(
        &l.app,
        Method::POST,
        &format!("/_rustybin/harness/clusters/chain-1/runs/{run_id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let report = wait_for_cluster_report(&l, "chain-1", &run_id).await;
    let sent = report["totals"]["sent"].as_u64().unwrap();
    assert!((90..=104).contains(&sent), "sent {sent}");
    let srv = &report["server"];
    assert_eq!(srv["arrivals"], sent, "{srv}");
    let leg = &srv["chain"]["hops"][0];
    // The front server called, the back server answered: one merged leg.
    let calls = leg["calls"].as_u64().unwrap();
    assert!(calls > 0 && calls <= sent, "{leg}");
    assert_eq!(leg["upstream"], calls, "{leg}");
    assert_eq!(leg["arrivals"], calls, "{leg}");
    assert_eq!(leg["url"], format!("{}/harness/back", back.base()));
    assert!(leg["latency"]["p50_ms"].as_f64().unwrap() > 0.0, "{leg}");
    // Whatever made no call says why (the first requests can beat the
    // leader's word that the run is live), and nothing is unaccounted for.
    let refused: u64 = leg["refused"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(calls + refused, sent, "{leg}");
    assert!(calls * 10 >= sent * 8, "most requests were followed: {leg}");
    assert!(srv["chain"].get("problems").is_none(), "{srv}");
    // Each server node exports its own calls: the front server made them
    // all, the back server (the end of the chain) none, and the nodes that
    // serve nothing have no series. Together they are the merged report,
    // so nothing is counted twice.
    let series = series_agree(&front.state, &run_id).await;
    assert_eq!(at(&series, 1, "ok"), calls, "{series:?}");
    assert_eq!(series.values().sum::<u64>(), sent, "{series:?}");
    assert_eq!(series_agree(&back.state, &run_id).await, Series::new());
    for n in [&l, &c, &back] {
        let metrics = n.state.metrics.render();
        assert!(
            !metrics.contains("rustybin_harness_chain_calls_total"),
            "{metrics}"
        );
    }
    // The run is over: the leader no longer says live, and a request of
    // the run (its key still signs) calls nothing on a member.
    let entry = front
        .state
        .harness
        .as_ref()
        .unwrap()
        .correlator
        .entry(&run_id)
        .expect("the run on the front server");
    for _ in 0..100 {
        if !entry.chain().expect("chain").gate().live() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(!entry.chain().expect("chain").gate().live());
    let before = entry.chain_report().expect("chain").hops[0].calls;
    let client_id = c.state.harness.as_ref().unwrap().node_id.clone();
    let late = signed(
        &entry.key,
        &run_id,
        &client_id,
        900_000,
        0,
        "/harness/front",
    );
    let late = [("x-rustybin-harness", late.as_str())];
    assert_eq!(
        knock(&front.app, "/harness/front", &late).await,
        StatusCode::OK
    );
    assert_eq!(entry.chain_report().expect("chain").hops[0].calls, before);

    for n in [l, c, front, back] {
        n.stop().await;
    }

    // A server whose own list does not allow the hop makes no call: a
    // leader cannot aim a member elsewhere. The report says why. (A leader
    // is a member of its own cluster, so this one has its own.)
    let l = node(harness_config(&[Role::Coordinator], &[])).await;
    let strict = node(harness_config(&[Role::Server], &[])).await;
    let (status, cluster) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters",
        Some(json!({ "cluster_id": "chain-2", "join_token": TOKEN })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{cluster}");
    let join_url = format!("{}/_rustybin/harness/clusters/chain-2", l.base());
    let c2 = node(client_config(1000)).await;
    for n in [&c2, &strict] {
        join(n, &join_url).await;
    }
    wait_for_alive(&l, "chain-2", 1, 1).await;
    let hop = sink(Duration::ZERO).await;
    let mut s = scenario(&strict.base(), 50, "1s", json!([hop.url()]));
    s["scale"] = json!({ "client_nodes": 1, "server_nodes": 1 });
    let (status, run) = send(
        &l.app,
        Method::POST,
        "/_rustybin/harness/clusters/chain-2/runs",
        Some(s),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{run}");
    let run_id = run["run_id"].as_str().unwrap().to_string();
    let (status, started) = send(
        &l.app,
        Method::POST,
        &format!("/_rustybin/harness/clusters/chain-2/runs/{run_id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let report = wait_for_cluster_report(&l, "chain-2", &run_id).await;
    let sent = report["totals"]["sent"].as_u64().unwrap();
    assert!(sent > 0, "{report}");
    let chain = &report["server"]["chain"];
    assert_eq!(chain["hops"][0]["calls"], 0, "{chain}");
    assert_eq!(chain["hops"][0]["refused"]["not_prepared"], sent, "{chain}");
    assert!(
        chain["problems"][0]
            .as_str()
            .unwrap()
            .contains("RUSTYBIN_HARNESS_TARGETS"),
        "{chain}"
    );
    assert_eq!(hop.hits(), 0);
    let series = series_agree(&strict.state, &run_id).await;
    assert_eq!(
        series,
        Series::from([((1, "not_prepared".to_string()), sent)])
    );
    for n in [l, strict, c2] {
        n.stop().await;
    }
}
