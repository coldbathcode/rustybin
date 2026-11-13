use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Extensions, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::test_support::{body_json, test_app_with, test_state_with};

/// A stand-in gateway: forwards to one upstream (request and response
/// headers included) and, when told, retries a 503 once, limits requests,
/// adds a header, times out, caches GETs, opens a circuit breaker, refuses
/// large bodies, answers CORS or transforms headers.
#[derive(Default)]
struct Gw {
    upstream: String,
    retry: bool,
    limit: Option<u32>,
    add: Option<(&'static str, String)>,
    timeout: Option<Duration>,
    cache: bool,
    /// Open after this many failures in a row, for this long.
    breaker: Option<(u32, Duration)>,
    /// Refuse bodies larger than this with 413.
    max_body: Option<usize>,
    /// CORS for one origin.
    cors: Option<Cors>,
    /// A request header removed before the upstream.
    remove: Option<&'static str>,
    /// A response header added on the way back.
    add_response: Option<(&'static str, &'static str)>,
    /// An AI gateway in front of the AI mock (the AI scenarios).
    ai: Option<AiGw>,
    /// An MCP gateway in front of an attack server (the MCP scenarios).
    mcp: Option<McpGw>,
    /// This request (the first is 1) never reaches the upstream: the call
    /// fails at once (`502`), or times out after a moment (`504`), as one
    /// lost connection between a gateway and its upstream does.
    fail_call: Option<(u32, FailedCall)>,
    calls: AtomicU32,
    count: AtomicU32,
    cached: Mutex<HashMap<String, (u16, bytes::Bytes)>>,
    circuit: Mutex<Circuit>,
    client: reqwest::Client,
}

/// The gateway's CORS policy: the one origin it allows; preflights it
/// answers itself, or lets through to the upstream.
struct Cors {
    origin: String,
    answer_preflights: bool,
}

/// How a call the stand-in gateway loses ends.
#[derive(Clone, Copy, Debug)]
enum FailedCall {
    Dropped,
    TimedOut,
}

/// The answer to the request the gateway loses on the way to the upstream.
async fn lost_call(g: &Gw) -> Option<Response> {
    let (nth, how) = g.fail_call?;
    if g.calls.fetch_add(1, Ordering::SeqCst) + 1 != nth {
        return None;
    }
    Some(match how {
        FailedCall::Dropped => StatusCode::BAD_GATEWAY.into_response(),
        FailedCall::TimedOut => {
            tokio::time::sleep(Duration::from_millis(100)).await;
            StatusCode::GATEWAY_TIMEOUT.into_response()
        }
    })
}

/// Headers a proxy never forwards as they are.
const HOP: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
];

/// The breaker's state: failures in a row, when it opened, and whether a
/// trial request is out.
#[derive(Default)]
struct Circuit {
    failures: u32,
    opened: Option<std::time::Instant>,
    trial: bool,
}

/// What a guard does with a prompt carrying planted data.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Guard {
    Block,
    Redact,
}

/// A stand-in AI gateway: a token budget, a fallback target, a semantic
/// (or exact) cache, a prompt guard, a response guard, round robin.
#[derive(Default)]
struct AiGw {
    second: Option<String>,
    fallback: bool,
    balance: bool,
    token_limit: Option<u64>,
    /// A request limit instead (to prove a request limit is told apart).
    request_limit: Option<u32>,
    semantic: bool,
    exact_cache: bool,
    guard: Option<Guard>,
    redact_response: bool,
    rr: AtomicU32,
    used: AtomicU64,
    /// (embedding, prompt, answer) of the answers kept.
    cache: Mutex<Vec<(Vec<f32>, String, bytes::Bytes)>>,
}

const REDACTED: &str = "[REDACTED]";

async fn ai_gw(g: &Gw, a: &AiGw, req: Request) -> Response {
    let mut headers = req.headers().clone();
    for h in HOP {
        headers.remove(*h);
    }
    let raw = axum::body::to_bytes(req.into_body(), 1 << 20)
        .await
        .unwrap_or_default();
    let mut body = String::from_utf8_lossy(&raw).into_owned();
    if let Some(l) = a.token_limit {
        if a.used.load(Ordering::SeqCst) >= l {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
    }
    if let Some(l) = a.request_limit {
        if a.rr.fetch_add(1, Ordering::SeqCst) + 1 > l {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
    }
    if let Some(mode) = a.guard {
        let planted = !catalog::planted_in(&body).is_empty();
        if catalog::injection_in(&body) || (planted && mode == Guard::Block) {
            return StatusCode::BAD_REQUEST.into_response();
        }
        for (_, v) in catalog::PLANTED {
            body = body.replace(v, REDACTED);
        }
    }
    let prompt = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| {
            v["messages"].as_array()?.last()?["content"]
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_default();
    let vector = crate::ai::embed::embed(&prompt, 256);
    for (v, p, b) in a.cache.lock().expect("lock").iter() {
        let hit = (a.semantic && crate::ai::embed::cosine(v, &vector) >= 0.9)
            || (a.exact_cache && *p == prompt);
        if hit {
            return (StatusCode::OK, b.clone()).into_response();
        }
    }
    let second = a.second.clone().unwrap_or_default();
    let first = if a.balance && a.rr.fetch_add(1, Ordering::SeqCst) % 2 == 1 {
        second.clone()
    } else {
        g.upstream.clone()
    };
    let send = |url: String| {
        g.client
            .post(url)
            .headers(headers.clone())
            .body(body.clone())
            .send()
    };
    let mut resp = send(first).await;
    if a.fallback && !matches!(&resp, Ok(r) if r.status().is_success()) {
        resp = send(second).await;
    }
    let Ok(r) = resp else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let status = r.status().as_u16();
    let mut text = String::from_utf8_lossy(&r.bytes().await.unwrap_or_default()).into_owned();
    if a.token_limit.is_some() {
        let used = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["usage"]["total_tokens"].as_u64())
            .unwrap_or(0);
        a.used.fetch_add(used, Ordering::SeqCst);
    }
    if a.redact_response {
        for (_, v) in catalog::PLANTED {
            text = text.replace(v, REDACTED);
        }
    }
    let answer = bytes::Bytes::from(text);
    if status == 200 && (a.semantic || a.exact_cache) {
        a.cache
            .lock()
            .expect("lock")
            .push((vector, prompt, answer.clone()));
    }
    let mut out = (StatusCode::from_u16(status).expect("status"), answer).into_response();
    out.headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    out
}

/// A stand-in MCP gateway: removes tools with hidden instructions, pins
/// definitions, namespaces a second server's tools next to the first's,
/// guards tool results, caps the size of answers.
#[derive(Default)]
struct McpGw {
    second: Option<String>,
    strip_poison: bool,
    pin: bool,
    namespace: bool,
    guard_results: bool,
    max_bytes: Option<usize>,
    pinned: Mutex<HashMap<String, Value>>,
}

/// The tools of the second server (the stateless 2026-07-28 protocol).
async fn second_tools(g: &Gw, url: &str) -> Vec<Value> {
    let meta = json!({ "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {} });
    let r = g
        .client
        .post(url)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", "tools/list")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": { "_meta": meta } }))
        .send()
        .await;
    let Ok(r) = r else { return Vec::new() };
    let text = r.text().await.unwrap_or_default();
    catalog::rpc_answer(&text)
        .and_then(|v| v["result"]["tools"].as_array().cloned())
        .unwrap_or_default()
}

async fn mcp_gw(g: &Gw, m: &McpGw, req: Request) -> Response {
    let mut headers = req.headers().clone();
    for h in HOP {
        headers.remove(*h);
    }
    let body = axum::body::to_bytes(req.into_body(), 1 << 20)
        .await
        .unwrap_or_default();
    let Ok(r) = g
        .client
        .post(&g.upstream)
        .headers(headers)
        .body(body)
        .send()
        .await
    else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let status = r.status().as_u16();
    let session = r.headers().get("mcp-session-id").cloned();
    let raw = r.bytes().await.unwrap_or_default();
    if m.max_bytes.is_some_and(|max| raw.len() > max) {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let mut out = match catalog::rpc_answer(&text) {
        None => (StatusCode::from_u16(status).expect("status"), text).into_response(),
        Some(mut v) => {
            if let Some(tools) = v["result"]["tools"].as_array().cloned() {
                let mut tools: Vec<Value> = tools
                    .into_iter()
                    .filter(|t| {
                        !m.strip_poison
                            || crate::mcp::attacks::hidden_instructions(&t.to_string()).is_empty()
                    })
                    .collect();
                if m.pin {
                    let mut pinned = m.pinned.lock().expect("lock");
                    for t in tools.iter_mut() {
                        let name = t["name"].as_str().unwrap_or("").to_string();
                        match pinned.get(&name) {
                            Some(p) => *t = p.clone(),
                            None => {
                                pinned.insert(name, t.clone());
                            }
                        }
                    }
                }
                if let Some(second) = &m.second {
                    let mut more = second_tools(g, second).await;
                    if m.namespace {
                        for t in tools.iter_mut() {
                            t["name"] =
                                json!(format!("attack_{}", t["name"].as_str().unwrap_or("")));
                        }
                        for t in more.iter_mut() {
                            t["name"] = json!(format!("crm_{}", t["name"].as_str().unwrap_or("")));
                        }
                    }
                    tools.extend(more);
                }
                v["result"]["tools"] = json!(tools);
            } else if m.guard_results && v.get("result").is_some() {
                let r = v["result"].to_string();
                if !crate::mcp::attacks::hidden_instructions(&r).is_empty()
                    || r.contains(crate::mcp::attacks::EXFIL_HOST)
                {
                    v["result"] = json!({ "content": [{ "type": "text", "text": "removed by the gateway" }], "isError": true });
                }
            }
            (StatusCode::from_u16(status).expect("status"), v.to_string()).into_response()
        }
    };
    out.headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    if let Some(sid) = session {
        out.headers_mut().insert("mcp-session-id", sid);
    }
    out
}

async fn gw(State(g): State<Arc<Gw>>, req: Request) -> Response {
    if let Some(lost) = lost_call(&g).await {
        return lost;
    }
    if let Some(a) = &g.ai {
        return ai_gw(&g, a, req).await;
    }
    if let Some(m) = &g.mcp {
        return mcp_gw(&g, m, req).await;
    }
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    let url = if path == "/" {
        g.upstream.clone()
    } else {
        format!("{}{path}", g.upstream)
    };
    if let Some(l) = g.limit {
        if g.count.fetch_add(1, Ordering::SeqCst) + 1 > l {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
    }
    if g.cache {
        if let Some((s, b)) = g.cached.lock().expect("lock").get(&url).cloned() {
            return (StatusCode::from_u16(s).expect("status"), b).into_response();
        }
    }
    if let Some((_, half_open)) = g.breaker {
        let mut c = g.circuit.lock().expect("lock");
        if let Some(t) = c.opened {
            // Open (or a trial out): answer on its own.
            if c.trial || t.elapsed() < half_open {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            // Half open: this request is the trial.
            c.trial = true;
        }
    }
    let method = req.method().clone();
    let origin = req
        .headers()
        .get("origin")
        .and_then(|o| o.to_str().ok())
        .map(str::to_string);
    let allowed = |c: &Cors| origin.as_deref() == Some(c.origin.as_str());
    let preflight = method == axum::http::Method::OPTIONS
        && req.headers().contains_key("access-control-request-method");
    if let Some(c) = g.cors.as_ref().filter(|c| preflight && c.answer_preflights) {
        // Answered here: CORS headers for the allowed origin only.
        let mut resp = StatusCode::NO_CONTENT.into_response();
        if allowed(c) {
            let h = resp.headers_mut();
            h.insert(
                "access-control-allow-origin",
                HeaderValue::from_str(&c.origin).expect("origin"),
            );
            h.insert(
                "access-control-allow-methods",
                HeaderValue::from_static("GET, POST"),
            );
        }
        return resp;
    }
    let mut headers = req.headers().clone();
    for h in HOP {
        headers.remove(*h);
    }
    if let Some(r) = g.remove {
        headers.remove(r);
    }
    let body = axum::body::to_bytes(req.into_body(), 4 << 20)
        .await
        .unwrap_or_default();
    if g.max_body.is_some_and(|m| body.len() > m) {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let send = || {
        let mut r = g
            .client
            .request(method.clone(), &url)
            .headers(headers.clone())
            .body(body.clone());
        if let Some((k, v)) = &g.add {
            r = r.header(*k, v);
        }
        if let Some(t) = g.timeout {
            r = r.timeout(t);
        }
        r.send()
    };
    let mut resp = send().await;
    if g.retry && matches!(&resp, Ok(r) if r.status() == 503) {
        resp = send().await;
    }
    if let Some((threshold, _)) = g.breaker {
        let mut c = g.circuit.lock().expect("lock");
        if matches!(&resp, Ok(r) if r.status().is_success()) {
            *c = Circuit::default();
        } else if c.trial {
            // The trial failed: open again.
            c.trial = false;
            c.opened = Some(std::time::Instant::now());
        } else {
            c.failures += 1;
            if c.failures >= threshold {
                c.opened = Some(std::time::Instant::now());
            }
        }
    }
    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            let mut back = r.headers().clone();
            for h in HOP {
                back.remove(*h);
            }
            let body = r.bytes().await.unwrap_or_default();
            if g.cache && status == 200 {
                g.cached
                    .lock()
                    .expect("lock")
                    .insert(url, (status, body.clone()));
            }
            if let Some(c) = g.cors.as_ref().filter(|c| c.answer_preflights) {
                // The gateway's CORS answer, never the upstream's.
                back.remove("access-control-allow-origin");
                if allowed(c) {
                    back.insert(
                        "access-control-allow-origin",
                        HeaderValue::from_str(&c.origin).expect("origin"),
                    );
                }
            }
            if let Some((k, v)) = g.add_response {
                back.insert(k, HeaderValue::from_static(v));
            }
            let mut resp = (StatusCode::from_u16(status).expect("status"), body).into_response();
            resp.headers_mut().extend(back);
            resp
        }
        Err(e) if e.is_timeout() => StatusCode::GATEWAY_TIMEOUT.into_response(),
        Err(_) => StatusCode::BAD_GATEWAY.into_response(),
    }
}

async fn start_gw(g: Gw) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = axum::Router::new().fallback(gw).with_state(Arc::new(g));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn config() -> Config {
    let mut c = Config::for_tests();
    c.egress_allow = vec!["127.0.0.1".into()];
    c.egress_allow_loopback = true;
    c
}

/// A real instance (the gateway's upstream) and its state.
async fn instance(c: Config) -> (crate::server::RunningServer, AppState, String) {
    let state = crate::state::AppState::for_tests(c);
    let server = crate::server::start_with_state(state.clone())
        .await
        .expect("start");
    let base = format!("http://{}", server.http_addr);
    (server, state, base)
}

fn svc(state: &AppState, base: &str) -> Svc {
    let mut headers = HeaderMap::new();
    if state.config.admin_token.is_some() {
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer admin-secret"),
        );
    }
    let host = base.trim_start_matches("http://");
    headers.insert("host", HeaderValue::from_str(host).expect("host"));
    let uri: Uri = "/_rustybin/scenarios".parse().expect("uri");
    Svc::from_request(state, &headers, &Extensions::new(), &uri)
}

async fn run_of(state: &AppState, base: &str, id: &str) -> Value {
    start(state, svc(state, base), "me", id)
        .await
        .expect("start")
}

/// The stand-in gateway's half-open time and body limit.
const HALF_OPEN_MS: u64 = 1000;
const SIZE_LIMIT: u64 = 1024;

async fn check_via(state: &AppState, run: &Value, gateway: &str) -> Value {
    let id = run["scenario"].as_str().expect("scenario");
    let body = match id {
        "circuit_breaker" => CheckBody::new(gateway).with("half_open_ms", HALF_OPEN_MS),
        "request_size_limit" => CheckBody::new(gateway).with("size_limit", SIZE_LIMIT),
        _ => CheckBody::new(gateway),
    };
    check_with(state, run, body).await
}

async fn check_with(state: &AppState, run: &Value, body: CheckBody) -> Value {
    let id = run["scenario"].as_str().expect("scenario");
    check(state, "me", id, run["id"].as_str().expect("id"), body)
        .await
        .expect("check")
}

fn failed(verdict: &Value) -> Vec<String> {
    verdict["expectations"]
        .as_array()
        .expect("expectations")
        .iter()
        .filter(|e| e["ok"] == false)
        .map(|e| e["name"].as_str().unwrap_or("").to_string())
        .collect()
}

fn behaving(id: &str, upstream: &str, key: Option<&str>) -> Gw {
    behaving_with(id, upstream, None, key)
}

fn behaving_with(id: &str, upstream: &str, second: Option<&str>, key: Option<&str>) -> Gw {
    let mut g = Gw {
        upstream: upstream.to_string(),
        ..Gw::default()
    };
    let ai = |a: AiGw| {
        Some(AiGw {
            second: second.map(str::to_string),
            ..a
        })
    };
    let mcp = |m: McpGw| {
        Some(McpGw {
            second: second.map(str::to_string),
            ..m
        })
    };
    match id {
        "mcp_tool_poisoning" => {
            g.mcp = mcp(McpGw {
                strip_poison: true,
                ..McpGw::default()
            })
        }
        "mcp_rug_pull" => {
            g.mcp = mcp(McpGw {
                pin: true,
                ..McpGw::default()
            })
        }
        "mcp_tool_shadowing" => {
            g.mcp = mcp(McpGw {
                namespace: true,
                ..McpGw::default()
            })
        }
        "mcp_result_injection" | "mcp_exfiltration_url" => {
            g.mcp = mcp(McpGw {
                guard_results: true,
                ..McpGw::default()
            })
        }
        "mcp_oversized_result" => {
            g.mcp = mcp(McpGw {
                max_bytes: Some(64 * 1024),
                ..McpGw::default()
            })
        }
        "ai_token_limit" => {
            g.ai = ai(AiGw {
                token_limit: Some(catalog::DEFAULT_TOKEN_LIMIT),
                ..AiGw::default()
            })
        }
        "ai_model_fallback" => {
            g.ai = ai(AiGw {
                fallback: true,
                ..AiGw::default()
            })
        }
        "ai_semantic_cache" => {
            g.ai = ai(AiGw {
                semantic: true,
                ..AiGw::default()
            })
        }
        "ai_prompt_guard" => {
            g.ai = ai(AiGw {
                guard: Some(Guard::Block),
                ..AiGw::default()
            })
        }
        "ai_response_guard" => {
            g.ai = ai(AiGw {
                redact_response: true,
                ..AiGw::default()
            })
        }
        "ai_provider_balance" => {
            g.ai = ai(AiGw {
                balance: true,
                ..AiGw::default()
            })
        }
        "retries" => g.retry = true,
        "rate_limiting" => g.limit = Some(catalog::DEFAULT_LIMIT),
        "credential_injection" => {
            g.add = Some((catalog::KEY_HEADER, key.unwrap_or("").to_string()))
        }
        "timeouts" => g.timeout = Some(Duration::from_millis(150)),
        "caching" => g.cache = true,
        "circuit_breaker" => g.breaker = Some((3, Duration::from_millis(HALF_OPEN_MS))),
        "request_size_limit" => g.max_body = Some(SIZE_LIMIT as usize),
        "cors" => {
            g.cors = Some(Cors {
                origin: catalog::DEFAULT_ALLOWED_ORIGIN.into(),
                answer_preflights: true,
            })
        }
        "header_transformation" => {
            g.add = Some((catalog::DEFAULT_ADD_REQUEST_HEADER, "added".into()));
            g.remove = Some(catalog::DEFAULT_REMOVE_REQUEST_HEADER);
            g.add_response = Some((catalog::DEFAULT_ADD_RESPONSE_HEADER, "yes"));
        }
        _ => {}
    }
    g
}

fn key_of(run: &Value) -> Option<String> {
    run["setup"]
        .as_array()
        .and_then(|s| {
            s.iter()
                .find_map(|l| l.as_str()?.split("X-Upstream-Key: ").nth(1))
        })
        .and_then(|rest| rest.split(' ').next())
        .map(str::to_string)
}

#[tokio::test]
async fn every_scenario_passes_through_a_gateway_that_behaves() {
    let (_server, state, base) = instance(config()).await;
    for sc in SCENARIOS {
        let run = run_of(&state, &base, sc.id).await;
        let upstream = run["upstream"].as_str().expect("upstream").to_string();
        let gateway = start_gw(behaving_with(
            sc.id,
            &upstream,
            run["second_upstream"].as_str(),
            key_of(&run).as_deref(),
        ))
        .await;
        let v = check_via(&state, &run, &gateway).await;
        assert_eq!(v["passed"], true, "{}: {v:#}", sc.id);
        end(&state, "me", sc.id, run["id"].as_str().expect("id")).expect("end");
    }
}

#[tokio::test]
async fn a_gateway_without_the_feature_fails_with_the_expectation_named() {
    let (_server, state, base) = instance(config()).await;
    let expected: &[(&str, &str)] = &[
        ("retries", "The gateway retried the failed attempt"),
        (
            "rate_limiting",
            "The gateway refused requests over the limit with 429",
        ),
        (
            "credential_injection",
            "The upstream accepted the credential the gateway added",
        ),
        (
            "timeouts",
            "The gateway gave up before the upstream answered",
        ),
        ("caching", "Only the first request reached the upstream"),
        (
            "circuit_breaker",
            "The gateway opened the circuit after upstream failures",
        ),
        (
            "request_size_limit",
            "The body over the limit never reached the upstream",
        ),
        (
            "cors",
            "The preflight from the allowed origin got CORS headers",
        ),
        (
            "header_transformation",
            "The added request header reached the upstream",
        ),
        (
            "ai_token_limit",
            "The gateway refused requests once the token budget was spent",
        ),
        (
            "ai_model_fallback",
            "The gateway fell back to another target",
        ),
        (
            "ai_semantic_cache",
            "Prompts that mean the same were answered from the cache",
        ),
        ("ai_prompt_guard", "Personal data never reached the model"),
        ("ai_response_guard", "No personal data reached the client"),
        ("ai_provider_balance", "Both targets received requests"),
        (
            "mcp_tool_poisoning",
            "The poisoned description never reached the client",
        ),
        (
            "mcp_rug_pull",
            "The changed definition never reached the client",
        ),
        (
            "mcp_tool_shadowing",
            "The attacker's copy did not take the trusted tool's name",
        ),
        (
            "mcp_result_injection",
            "The prompt injection never reached the client",
        ),
        (
            "mcp_oversized_result",
            "The client never received more than the limit",
        ),
        (
            "mcp_exfiltration_url",
            "No link to the unknown host reached the client",
        ),
    ];
    assert_eq!(expected.len(), SCENARIOS.len(), "a scenario without a case");
    for (id, name) in expected {
        let run = run_of(&state, &base, id).await;
        let upstream = run["upstream"].as_str().expect("upstream").to_string();
        // A pass-through proxy, then no gateway at all (the instance itself).
        let plain = start_gw(Gw {
            upstream: upstream.clone(),
            ai: id.starts_with("ai_").then(AiGw::default),
            mcp: id.starts_with("mcp_").then(McpGw::default),
            ..Gw::default()
        })
        .await;
        for gateway in [plain.as_str(), upstream.as_str()] {
            let v = check_via(&state, &run, gateway).await;
            assert_eq!(v["passed"], false, "{id}: {v:#}");
            assert!(failed(&v).iter().any(|f| f == name), "{id}: {v:#}");
        }
        end(&state, "me", id, run["id"].as_str().expect("id")).expect("end");
    }
}

#[tokio::test]
async fn ending_a_run_removes_what_it_created_and_nothing_else() {
    let dir = tempfile::tempdir().expect("dir");
    let mut c = config();
    c.mocks_state_dir = Some(dir.path().to_string_lossy().into_owned());
    let (_server, state, base) = instance(c).await;
    let run = run_of(&state, &base, "caching").await;
    let mock = run["resources"][0]["name"]
        .as_str()
        .expect("mock")
        .to_string();
    assert!(state.mocks.get(&mock).is_some());
    // Never saved: a restart does not bring it back without its run.
    assert!(
        !std::fs::read_dir(dir.path()).expect("dir").any(|e| e
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .contains(&mock)),
        "the scenario mock was saved"
    );
    // The petstore sample and someone else's flaky counters stay.
    state.flaky.next("someone", "recover:1");
    let retries = run_of(&state, &base, "retries").await;
    let gateway = start_gw(behaving(
        "retries",
        retries["upstream"].as_str().expect("u"),
        None,
    ))
    .await;
    check_via(&state, &retries, &gateway).await;
    let out = end(&state, "me", "retries", retries["id"].as_str().expect("id")).expect("end");
    assert_eq!(out["removed"][0]["count"], 3, "{out}");
    assert_eq!(state.flaky.session_counters("someone").len(), 1);
    let out = end(&state, "me", "caching", run["id"].as_str().expect("id")).expect("end");
    assert_eq!(out["removed"][0]["name"], mock.as_str());
    assert!(state.mocks.get(&mock).is_none());
    assert!(state.mocks.get("petstore").is_some());
    // Ended means gone.
    assert!(end(&state, "me", "caching", run["id"].as_str().expect("id")).is_err());
}

fn req(method: &str, uri: &str, session: &str) -> axum::http::Request<Body> {
    axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("x-rustybin-session", session)
        .header("content-type", "application/json")
        .body(Body::empty())
        .expect("req")
}

#[tokio::test]
async fn another_session_cannot_see_or_end_a_run() {
    let app = test_app_with(test_state_with(config()));
    let resp = app
        .clone()
        .oneshot(req("POST", "/_rustybin/scenarios/retries/runs", "alice"))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let run = body_json(resp).await;
    let path = format!(
        "/_rustybin/scenarios/retries/runs/{}",
        run["id"].as_str().expect("id")
    );
    for method in ["GET", "DELETE"] {
        let resp = app
            .clone()
            .oneshot(req(method, &path, "bob"))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{method}");
    }
    let list = body_json(
        app.clone()
            .oneshot(req("GET", LIST_PATH, "bob"))
            .await
            .expect("r"),
    )
    .await;
    assert_eq!(list["runs"], json!([]));
    assert_eq!(
        list["scenarios"].as_array().map(Vec::len),
        Some(SCENARIOS.len())
    );
    let list = body_json(
        app.clone()
            .oneshot(req("GET", LIST_PATH, "alice"))
            .await
            .expect("r"),
    )
    .await;
    assert_eq!(list["runs"][0]["id"], run["id"]);
    let resp = app
        .clone()
        .oneshot(req("DELETE", &path, "alice"))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn mutations_are_admin_only_and_the_egress_policy_applies() {
    let mut c = Config::for_tests();
    c.admin_token = Some("admin-secret".into());
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    let resp = app
        .clone()
        .oneshot(req("POST", "/_rustybin/scenarios/retries/runs", "a"))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // Without the gateway's host in the egress allowlist the check is refused.
    let run = run_of(&state, "http://127.0.0.1:9", "retries").await;
    let r = check(
        &state,
        "me",
        "retries",
        run["id"].as_str().expect("id"),
        CheckBody::new("http://127.0.0.1:9/gw"),
    )
    .await;
    match r {
        Err(Refused(status, msg)) => {
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert!(msg.contains("scenario"), "{msg}");
        }
        Ok(v) => panic!("checked: {v}"),
    }
    // A limit on a scenario that has none, and a query string, are refused.
    let bad = check(
        &state,
        "me",
        "retries",
        run["id"].as_str().expect("id"),
        CheckBody::new("http://127.0.0.1:9/gw").with("limit", 3),
    )
    .await;
    assert!(matches!(bad, Err(Refused(StatusCode::BAD_REQUEST, _))));
}

#[tokio::test]
async fn runs_are_bounded() {
    let state = test_state_with(config());
    for _ in 0..MAX_RUNS {
        run_of(&state, "http://127.0.0.1:9", "retries").await;
    }
    let r = start(&state, svc(&state, "http://127.0.0.1:9"), "me", "retries").await;
    assert!(matches!(r, Err(Refused(StatusCode::TOO_MANY_REQUESTS, _))));
    assert_eq!(state.scenarios.len(), MAX_RUNS);
    let r = start(&state, svc(&state, "http://127.0.0.1:9"), "me", "nope").await;
    assert!(matches!(r, Err(Refused(StatusCode::NOT_FOUND, _))));
}

#[tokio::test]
async fn a_run_cannot_be_guessed_nor_its_check_skewed_by_forged_sessions() {
    let (_server, state, base) = instance(config()).await;
    let run = run_of(&state, &base, "caching").await;
    let id = run["id"].as_str().expect("id").to_string();
    assert_eq!(id.len(), 24, "96-bit run ids: {id}");
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    // Requests sent to the upstream under the sessions a check would use
    // when the run id is known: they must not count.
    let client = reqwest::Client::new();
    for n in 0..3 {
        for k in 1..3 {
            let forged = format!("scn-{id}-{k}-{n}");
            let _ = client
                .get(format!(
                    "{upstream}{}",
                    catalog::find("caching").expect("caching").request_path
                ))
                .header(SESSION_HEADER, forged)
                .send()
                .await;
        }
    }
    let gateway = start_gw(behaving("caching", &upstream, None)).await;
    let v = check_via(&state, &run, &gateway).await;
    assert_eq!(v["passed"], true, "{v:#}");
    // The secret in the sessions is never shown.
    let shown = get_run(&state, "me", "caching", &id, true).expect("run");
    let secret = state
        .scenarios
        .runs()
        .iter()
        .find(|r| r.id == id)
        .map(|r| r.secret.clone())
        .expect("run");
    assert!(!shown.to_string().contains(&secret), "{shown}");
    assert!(!v.to_string().contains(&secret), "{v}");
    end(&state, "me", "caching", &id).expect("end");
}

#[tokio::test]
async fn the_injected_credential_is_shown_to_admins_only() {
    let (_server, state, base) = instance(config()).await;
    let run = run_of(&state, &base, "credential_injection").await;
    let key = key_of(&run).expect("the start answer holds the key");
    let id = run["id"].as_str().expect("id");
    let admin = get_run(&state, "me", "credential_injection", id, true).expect("run");
    assert!(admin.to_string().contains(&key));
    assert_eq!(admin["key_hidden"], false);
    // Someone else behind the same address (the same session key) without
    // admin rights: the run, never its key.
    let other = get_run(&state, "me", "credential_injection", id, false).expect("run");
    assert!(!other.to_string().contains(&key), "{other}");
    assert_eq!(other["key_hidden"], true);
    let listed = list(&state, "me", false);
    assert!(!listed.to_string().contains(&key), "{listed}");
    assert!(list(&state, "me", true).to_string().contains(&key));
    end(&state, "me", "credential_injection", id).expect("end");
}

#[tokio::test]
async fn test_parameters_are_bounded_and_belong_to_their_scenario() {
    let state = test_state_with(config());
    let breaker = run_of(&state, "http://127.0.0.1:9", "circuit_breaker").await;
    let size = run_of(&state, "http://127.0.0.1:9", "request_size_limit").await;
    let b = breaker["id"].as_str().expect("id").to_string();
    let s = size["id"].as_str().expect("id").to_string();
    let refused = |r: Result<Value, Refused>| match r {
        Err(Refused(StatusCode::BAD_REQUEST, m)) => m,
        Err(Refused(status, m)) => panic!("refused with {status}: {m}"),
        Ok(v) => panic!("checked: {v}"),
    };
    let body = |half_open_ms: Option<u64>, size_limit: Option<u64>| {
        let mut b = CheckBody::new("http://127.0.0.1:9/gw");
        if let Some(v) = half_open_ms {
            b = b.with("half_open_ms", v);
        }
        if let Some(v) = size_limit {
            b = b.with("size_limit", v);
        }
        b
    };
    for v in [0, 99, 60_001] {
        let m = refused(check(&state, "me", "circuit_breaker", &b, body(Some(v), None)).await);
        assert!(m.contains("half_open_ms: 100 to 60000"), "{m}");
    }
    for v in [0, 15, catalog::MAX_SIZE_LIMIT + 1] {
        let m = refused(check(&state, "me", "request_size_limit", &s, body(None, Some(v))).await);
        assert!(m.contains("size_limit: 16 to"), "{m}");
    }
    let m = refused(check(&state, "me", "circuit_breaker", &b, body(None, Some(1024))).await);
    assert!(
        m.contains("size_limit applies to request_size_limit only"),
        "{m}"
    );
    let m = refused(
        check(
            &state,
            "me",
            "request_size_limit",
            &s,
            body(Some(500), None),
        )
        .await,
    );
    assert!(
        m.contains("half_open_ms applies to circuit_breaker only"),
        "{m}"
    );
    // A refused check is not counted.
    let shown = get_run(&state, "me", "circuit_breaker", &b, true).expect("run");
    assert_eq!(shown["checks"], 0, "{shown}");
    // The listing and the run describe each parameter with its bounds.
    let listed = list(&state, "me", true);
    let p = listed["scenarios"]
        .as_array()
        .expect("scenarios")
        .iter()
        .find(|s| s["id"] == "circuit_breaker")
        .map(|s| s["parameters"][0].clone())
        .expect("circuit_breaker");
    assert_eq!(p["name"], "half_open_ms");
    assert_eq!(p["minimum"], 100);
    assert_eq!(p["maximum"], 60_000);
    assert_eq!(shown["parameters"][0]["name"], "half_open_ms", "{shown}");
}

#[tokio::test]
async fn a_breaker_check_waits_the_half_open_time_and_names_the_trial() {
    let (_server, state, base) = instance(config()).await;
    let run = run_of(&state, &base, "circuit_breaker").await;
    assert!(
        run["upstream"]
            .as_str()
            .expect("upstream")
            .ends_with("/flaky/after/0"),
        "{run}"
    );
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    let gateway = start_gw(behaving("circuit_breaker", &upstream, None)).await;
    let t = std::time::Instant::now();
    let v = check_via(&state, &run, &gateway).await;
    assert!(t.elapsed() >= Duration::from_millis(HALF_OPEN_MS), "{v:#}");
    assert_eq!(v["passed"], true, "{v:#}");
    let requests = v["requests"].as_array().expect("requests");
    // Three failures reach the upstream, the fourth is answered by the gateway
    // and so are the two that confirm the circuit stays open, the trial reaches
    // the upstream again.
    let reached: Vec<bool> = requests
        .iter()
        .map(|r| !r["upstream"].as_array().expect("u").is_empty())
        .collect();
    assert_eq!(
        reached,
        [true, true, true, false, false, false, true],
        "{v:#}"
    );
    assert_eq!(requests[5]["trial"], Value::Null, "{v:#}");
    assert_eq!(requests[6]["trial"], true, "{v:#}");
    let out = end(
        &state,
        "me",
        "circuit_breaker",
        run["id"].as_str().expect("id"),
    )
    .expect("end");
    assert_eq!(out["removed"][0]["kind"], "flaky_counters", "{out}");
}

/// What an expectation of a verdict saw.
fn seen_of(v: &Value, name: &str) -> String {
    v["expectations"]
        .as_array()
        .expect("expectations")
        .iter()
        .find(|e| e["name"] == name)
        .and_then(|e| e["seen"].as_str())
        .unwrap_or("")
        .to_string()
}

const OPENED: &str = "The gateway opened the circuit after upstream failures";

/// The false pass this guards against: a plain proxy lost one call to the
/// upstream and answered `502` (or `504`) itself, and the check read that
/// one answer as an opened circuit.
#[tokio::test]
async fn one_lost_upstream_call_is_not_an_open_circuit() {
    let (_server, state, base) = instance(config()).await;
    for how in [FailedCall::Dropped, FailedCall::TimedOut] {
        let run = run_of(&state, &base, "circuit_breaker").await;
        let upstream = run["upstream"].as_str().expect("upstream").to_string();
        let gateway = start_gw(Gw {
            upstream,
            fail_call: Some((4, how)),
            ..Gw::default()
        })
        .await;
        let v = check_via(&state, &run, &gateway).await;
        assert_eq!(v["passed"], false, "{how:?}: {v:#}");
        assert!(failed(&v).iter().any(|f| f == OPENED), "{how:?}: {v:#}");
        let seen = seen_of(&v, OPENED);
        assert!(
            seen.contains("request 4 was answered") && seen.contains("not an open circuit"),
            "{how:?}: {seen}"
        );
        // No trial was sent: the circuit never opened.
        assert!(
            v["requests"]
                .as_array()
                .expect("requests")
                .iter()
                .all(|r| r["trial"].is_null()),
            "{how:?}: {v:#}"
        );
        end(
            &state,
            "me",
            "circuit_breaker",
            run["id"].as_str().expect("id"),
        )
        .expect("end");
    }
}

/// A lost call in front of a real breaker does not hide it: the check goes
/// on until the circuit opens and stays open.
#[tokio::test]
async fn a_breaker_passes_though_a_call_before_it_opened_was_lost() {
    let (_server, state, base) = instance(config()).await;
    let run = run_of(&state, &base, "circuit_breaker").await;
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    let mut g = behaving("circuit_breaker", &upstream, None);
    g.fail_call = Some((2, FailedCall::Dropped));
    let gateway = start_gw(g).await;
    let v = check_via(&state, &run, &gateway).await;
    assert_eq!(v["passed"], true, "{v:#}");
    let seen = seen_of(&v, OPENED);
    assert!(
        seen.contains("before that, request 2 was answered 502"),
        "{seen}"
    );
}

/// A rate limit answers on its own too, and lets requests through again
/// later: its `429` is not an open circuit.
#[tokio::test]
async fn a_rate_limit_is_not_an_open_circuit() {
    let (_server, state, base) = instance(config()).await;
    let run = run_of(&state, &base, "circuit_breaker").await;
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    let gateway = start_gw(Gw {
        upstream,
        limit: Some(3),
        ..Gw::default()
    })
    .await;
    let v = check_via(&state, &run, &gateway).await;
    assert_eq!(v["passed"], false, "{v:#}");
    assert!(
        seen_of(&v, OPENED).contains("not the 5xx of an open circuit"),
        "{v:#}"
    );
}

/// A route that does not reach the upstream answers a gateway error at
/// once: that is not a timeout.
#[tokio::test]
async fn a_gateway_error_without_the_upstream_is_not_a_timeout() {
    let (_server, state, base) = instance(config()).await;
    let name = "The request reached the upstream, which was still working on it";
    let run = run_of(&state, &base, "timeouts").await;
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    let lost = start_gw(Gw {
        upstream: upstream.clone(),
        fail_call: Some((1, FailedCall::Dropped)),
        ..Gw::default()
    })
    .await;
    // A route to a port nothing listens on.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let dead = format!("http://{}", closed.local_addr().expect("addr"));
    drop(closed);
    let nowhere = start_gw(Gw {
        upstream: dead,
        ..Gw::default()
    })
    .await;
    for gateway in [lost, nowhere] {
        let v = check_via(&state, &run, &gateway).await;
        assert_eq!(v["passed"], false, "{v:#}");
        assert_eq!(failed(&v), [name], "{v:#}");
    }
    // The gateway that gives up on a request the upstream holds still passes.
    let gateway = start_gw(behaving("timeouts", &upstream, None)).await;
    let v = check_via(&state, &run, &gateway).await;
    assert_eq!(v["passed"], true, "{v:#}");
}

/// A guard's refusal is repeated; a lost call is not. A plain proxy that
/// lost the call with the personal data must not get the guard's line.
#[tokio::test]
async fn a_lost_model_call_is_not_a_guards_refusal() {
    let (_server, state, base) = instance(config()).await;
    let name = "Personal data never reached the model";
    let run = run_of(&state, &base, "ai_prompt_guard").await;
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    let gateway = start_gw(Gw {
        upstream: upstream.clone(),
        ai: Some(AiGw::default()),
        fail_call: Some((2, FailedCall::Dropped)),
        ..Gw::default()
    })
    .await;
    let v = check_via(&state, &run, &gateway).await;
    assert_eq!(v["passed"], false, "{v:#}");
    assert!(failed(&v).iter().any(|f| f == name), "{v:#}");
    assert!(
        seen_of(&v, name).contains("a failed call, not a refusal"),
        "{v:#}"
    );
    // A guard that redacts, behind the same lost call, is still seen.
    let gateway = start_gw(Gw {
        upstream,
        ai: Some(AiGw {
            guard: Some(Guard::Redact),
            ..AiGw::default()
        }),
        fail_call: Some((2, FailedCall::Dropped)),
        ..Gw::default()
    })
    .await;
    let v = check_via(&state, &run, &gateway).await;
    assert!(!failed(&v).iter().any(|f| f == name), "{v:#}");
}

fn label<'a>(v: &'a Value, label: &str) -> &'a Value {
    v["requests"]
        .as_array()
        .expect("requests")
        .iter()
        .find(|r| r["label"] == label)
        .expect("request")
}

#[tokio::test]
async fn a_cors_check_sees_the_preflights_and_what_reached_the_upstream() {
    let (_server, state, base) = instance(config()).await;
    let run = run_of(&state, &base, "cors").await;
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    let gateway = start_gw(behaving("cors", &upstream, None)).await;
    let v = check_via(&state, &run, &gateway).await;
    assert_eq!(v["passed"], true, "{v:#}");
    // The gateway answered both preflights; the GET reached the upstream.
    let ups = |l: &str| label(&v, l)["upstream"].as_array().expect("u").len();
    assert_eq!(ups(catalog::PREFLIGHT_ALLOWED), 0, "{v:#}");
    assert_eq!(ups(catalog::PREFLIGHT_REFUSED), 0, "{v:#}");
    assert_eq!(ups(catalog::SIMPLE_ALLOWED), 1, "{v:#}");
    // The instance's own CORS layer does not answer for the run's mock: a
    // preflight sent straight to it reaches the mock and has no CORS.
    let client = reqwest::Client::new();
    let r = client
        .request(reqwest::Method::OPTIONS, format!("{upstream}/cors"))
        .header("origin", "https://somewhere.example.org")
        .header("access-control-request-method", "GET")
        .send()
        .await
        .expect("preflight");
    assert_eq!(r.status(), 204);
    assert!(r.headers().get("access-control-allow-origin").is_none());
    // Any other mock keeps the instance's CORS.
    let r = client
        .request(
            reqwest::Method::OPTIONS,
            format!("{base}/mock/petstore/pets"),
        )
        .header("origin", "https://somewhere.example.org")
        .header("access-control-request-method", "GET")
        .send()
        .await
        .expect("preflight");
    assert!(r.headers().get("access-control-allow-origin").is_some());

    // The upstream answers CORS: a gateway that lets everything through
    // passes, one that answers preflights itself does not.
    let body = || CheckBody::new(gateway.clone()).with("upstream_cors", true);
    let plain = start_gw(Gw {
        upstream: upstream.clone(),
        ..Gw::default()
    })
    .await;
    let v = check_with(
        &state,
        &run,
        CheckBody::new(plain.clone()).with("upstream_cors", true),
    )
    .await;
    assert_eq!(v["passed"], true, "{v:#}");
    assert_eq!(
        label(&v, catalog::PREFLIGHT_ALLOWED)["upstream"],
        json!([204]),
        "{v:#}"
    );
    let shown = get_run(&state, "me", "cors", run["id"].as_str().expect("id"), true).expect("run");
    assert_eq!(shown["upstream_answers_cors"], true, "{shown}");
    let v = check_with(&state, &run, body()).await;
    assert_eq!(v["passed"], false, "{v:#}");
    assert_eq!(
        failed(&v),
        ["The preflight reached the upstream, which answers CORS"],
        "{v:#}"
    );
    // Back to upstream_cors: false, the mock answers no CORS again.
    let v = check_with(&state, &run, CheckBody::new(plain)).await;
    assert!(
        failed(&v)
            .iter()
            .any(|f| f == "The preflight from the allowed origin got CORS headers"),
        "{v:#}"
    );
    // The gateway's CORS values are described, never echoed.
    assert!(!v.to_string().contains("GET, HEAD, OPTIONS"), "{v}");
    end(&state, "me", "cors", run["id"].as_str().expect("id")).expect("end");
}

#[tokio::test]
async fn a_cors_run_works_with_the_instance_cors_off() {
    let mut c = config();
    c.cors_allow_origins = Vec::new();
    let (_server, state, base) = instance(c).await;
    let run = run_of(&state, &base, "cors").await;
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    let gateway = start_gw(behaving("cors", &upstream, None)).await;
    let v = check_via(&state, &run, &gateway).await;
    assert_eq!(v["passed"], true, "{v:#}");
    let plain = start_gw(Gw {
        upstream: upstream.clone(),
        ..Gw::default()
    })
    .await;
    let v = check_with(
        &state,
        &run,
        CheckBody::new(plain.clone()).with("upstream_cors", true),
    )
    .await;
    assert_eq!(v["passed"], true, "{v:#}");
    let v = check_with(&state, &run, CheckBody::new(upstream.clone())).await;
    assert!(
        failed(&v)
            .iter()
            .any(|f| f == "The preflight from the allowed origin got CORS headers"),
        "{v:#}"
    );
    end(&state, "me", "cors", run["id"].as_str().expect("id")).expect("end");
}

#[tokio::test]
async fn cors_and_header_checks_take_their_parameters() {
    let (_server, state, base) = instance(config()).await;
    // Origins are normalised: the check sends the origin as a browser does.
    let run = run_of(&state, &base, "cors").await;
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    let gateway = start_gw(Gw {
        upstream: upstream.clone(),
        cors: Some(Cors {
            origin: "http://localhost:3000".into(),
            answer_preflights: true,
        }),
        ..Gw::default()
    })
    .await;
    let v = check_with(
        &state,
        &run,
        CheckBody::new(gateway.clone())
            .with("allowed_origin", "HTTP://LocalHost:3000")
            .with("refused_origin", "http://localhost:3001"),
    )
    .await;
    assert_eq!(v["passed"], true, "{v:#}");
    // With the defaults the gateway refuses the allowed origin.
    let v = check_via(&state, &run, &gateway).await;
    assert!(
        failed(&v)
            .iter()
            .any(|f| f == "The preflight from the allowed origin got CORS headers"),
        "{v:#}"
    );
    end(&state, "me", "cors", run["id"].as_str().expect("id")).expect("end");

    let run = run_of(&state, &base, "header_transformation").await;
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    let gateway = start_gw(Gw {
        upstream: upstream.clone(),
        add: Some(("X-Team", "blue".into())),
        remove: Some("x-debug"),
        add_response: Some(("X-Served-By", "gw-1")),
        ..Gw::default()
    })
    .await;
    let body = |value: &str| {
        CheckBody::new(gateway.clone())
            .with("add_request_header", "X-Team")
            .with("add_request_value", value)
            .with("remove_request_header", "X-Debug")
            .with("add_response_header", "x-served-by")
    };
    let v = check_with(&state, &run, body("blue")).await;
    assert_eq!(v["passed"], true, "{v:#}");
    let v = check_with(&state, &run, body("red")).await;
    assert_eq!(
        failed(&v),
        ["The added request header reached the upstream"],
        "{v:#}"
    );
    // The header the client sent for removal, through a pass-through proxy.
    let plain = start_gw(Gw {
        upstream,
        ..Gw::default()
    })
    .await;
    let v = check_with(
        &state,
        &run,
        CheckBody::new(plain).with("remove_request_header", "X-Debug"),
    )
    .await;
    let removed = v["expectations"]
        .as_array()
        .expect("e")
        .iter()
        .find(|e| e["name"] == "The removed request header did not reach the upstream")
        .expect("removed")
        .clone();
    assert_eq!(removed["ok"], false, "{v:#}");
    assert_eq!(
        removed["seen"], "present with the value the client sent",
        "{v:#}"
    );
    end(
        &state,
        "me",
        "header_transformation",
        run["id"].as_str().expect("id"),
    )
    .expect("end");
}

#[tokio::test]
async fn origins_and_header_names_are_validated() {
    let state = test_state_with(config());
    let cors = run_of(&state, "http://127.0.0.1:9", "cors").await;
    let hdr = run_of(&state, "http://127.0.0.1:9", "header_transformation").await;
    let refused = |id: &'static str, run: &Value, name: &str, value: Value| {
        let state = state.clone();
        let run = run["id"].as_str().expect("id").to_string();
        let body = CheckBody::new("http://127.0.0.1:9/gw").with(name, value);
        async move {
            match check(&state, "me", id, &run, body).await {
                Err(Refused(StatusCode::BAD_REQUEST, m)) => m,
                Err(Refused(status, m)) => panic!("refused with {status}: {m}"),
                Ok(v) => panic!("checked: {v}"),
            }
        }
    };
    for bad in [
        json!("app.example.com"),
        json!("https://app.example.com/path"),
        json!("https://app.example.com/"),
        json!("ftp://app.example.com"),
        json!("https://user@app.example.com"),
        json!("https://app.example.com?q=1"),
        json!(format!("https://{}.example.com", "a".repeat(200))),
        json!(42),
    ] {
        let m = refused("cors", &cors, "allowed_origin", bad.clone()).await;
        assert!(m.starts_with("allowed_origin: "), "{bad}: {m}");
    }
    let m = refused(
        "cors",
        &cors,
        "refused_origin",
        json!("https://app.example.com"),
    )
    .await;
    assert!(m.contains("another origin than allowed_origin"), "{m}");
    let m = refused("cors", &cors, "upstream_cors", json!("yes")).await;
    assert!(m.contains("upstream_cors: true or false"), "{m}");
    for bad in [
        json!("bad header"),
        json!(""),
        json!("Host"),
        json!("X-Rustybin-Session"),
        json!("Access-Control-Allow-Origin"),
        json!("x".repeat(65)),
        json!(true),
    ] {
        let m = refused(
            "header_transformation",
            &hdr,
            "add_response_header",
            bad.clone(),
        )
        .await;
        assert!(m.starts_with("add_response_header: "), "{bad}: {m}");
    }
    for bad in [
        json!("a\nb"),
        json!(""),
        json!(" padded"),
        json!("v".repeat(201)),
    ] {
        let m = refused(
            "header_transformation",
            &hdr,
            "add_request_value",
            bad.clone(),
        )
        .await;
        assert!(m.starts_with("add_request_value: "), "{bad}: {m}");
    }
    let m = refused(
        "header_transformation",
        &hdr,
        "remove_request_header",
        json!(catalog::DEFAULT_ADD_REQUEST_HEADER),
    )
    .await;
    assert!(m.contains("another header than add_request_header"), "{m}");
    let m = refused(
        "header_transformation",
        &hdr,
        "allowed_origin",
        json!("https://a.example.com"),
    )
    .await;
    assert!(m.contains("allowed_origin applies to cors only"), "{m}");
    let m = refused("cors", &cors, "nope", json!(1)).await;
    assert!(m.contains("unknown test parameter \"nope\""), "{m}");
    // Nothing was counted, and the listing describes the new parameters.
    for (id, run) in [("cors", &cors), ("header_transformation", &hdr)] {
        let shown = get_run(&state, "me", id, run["id"].as_str().expect("id"), true).expect("run");
        assert_eq!(shown["checks"], 0, "{shown}");
    }
    let listed = list(&state, "me", true);
    let params = |id: &str| {
        listed["scenarios"]
            .as_array()
            .expect("scenarios")
            .iter()
            .find(|s| s["id"] == id)
            .map(|s| s["parameters"].clone())
            .expect("scenario")
    };
    let p = params("cors");
    assert_eq!(p[0]["name"], "allowed_origin");
    assert_eq!(p[0]["type"], "string");
    assert_eq!(p[0]["format"], "origin");
    assert_eq!(p[2]["type"], "boolean");
    assert_eq!(p[2]["default"], false);
    let p = params("header_transformation");
    assert_eq!(p.as_array().map(Vec::len), Some(4));
    assert_eq!(p[1]["name"], "add_request_value");
    assert!(p[1].get("default").is_none(), "{p}");
}

#[test]
fn every_test_parameter_has_a_cli_flag_and_a_tool_property() {
    let check = crate::cli::commands::COMMANDS
        .iter()
        .find(|c| c.noun == "scenario" && c.verb == "check")
        .expect("scenario check");
    let tool = crate::mcp::manage::tools::TOOLS
        .iter()
        .find(|t| t.name == "check_gateway_scenario")
        .map(|t| (t.input_schema)())
        .expect("check_gateway_scenario");
    for p in catalog::PARAMS {
        let flag = format!("--{}", p.name.replace('_', "-"));
        let f = check
            .flags
            .iter()
            .find(|f| f.name == flag)
            .unwrap_or_else(|| panic!("no {flag} on scenario check"));
        assert_eq!(
            f.value,
            !matches!(p.kind, catalog::Kind::Bool { .. }),
            "{flag}: a switch for a boolean, a value otherwise"
        );
        assert!(check.usage.contains(&flag), "{flag} not in the usage");
        assert!(
            tool["properties"].get(p.name).is_some(),
            "{} not in the tool schema",
            p.name
        );
        // Every scenario parameter belongs to a scenario.
        assert!(
            SCENARIOS
                .iter()
                .any(|s| s.params.iter().any(|q| q.name == p.name)),
            "{} belongs to no scenario",
            p.name
        );
    }
}

// ── AI scenarios ────────────────────────────────────────────────────

/// An AI run and a stand-in gateway in front of it.
async fn ai_run(state: &AppState, base: &str, id: &str, a: AiGw) -> (Value, String) {
    let run = run_of(state, base, id).await;
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    let gateway = start_gw(Gw {
        upstream,
        ai: Some(AiGw {
            second: run["second_upstream"].as_str().map(str::to_string),
            ..a
        }),
        ..Gw::default()
    })
    .await;
    (run, gateway)
}

/// A chat completion to this instance's AI mock in one session.
async fn chat(base: &str, path: &str, session: &str, prompt: &str) -> (u16, String) {
    let r = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .header(SESSION_HEADER, session)
        .json(
            &json!({ "model": "gpt-4o-mini", "messages": [{ "role": "user", "content": prompt }] }),
        )
        .send()
        .await
        .expect("send");
    let status = r.status().as_u16();
    (status, r.text().await.unwrap_or_default())
}

#[tokio::test]
async fn an_ai_runs_profile_answers_its_own_requests_only() {
    let (_server, state, base) = instance(config()).await;
    let run = run_of(&state, &base, "ai_model_fallback").await;
    let id = run["id"].as_str().expect("id").to_string();
    let prefix = {
        let runs = state.scenarios.runs();
        runs.iter()
            .find(|r| r.id == id)
            .expect("run")
            .session_prefix()
    };
    let primary = catalog::AI_PRIMARY_PATH;
    // The run's requests: the primary target fails, the second answers.
    let (s, _) = chat(&base, primary, &format!("{prefix}1-0"), "hello").await;
    assert_eq!(s, 429);
    let (s, _) = chat(
        &base,
        catalog::AI_SECOND_PATH,
        &format!("{prefix}1-0"),
        "hello",
    )
    .await;
    assert_eq!(s, 200);
    // Anyone else, a forged prefix without the secret included: untouched.
    for other in ["someone", &format!("scn-{id}-0000000000000000-1-0")] {
        let (s, _) = chat(&base, primary, other, "hello").await;
        assert_eq!(s, 200, "{other}");
    }
    // The answer of a response guard run: a customer record, for its
    // requests only.
    let guard = run_of(&state, &base, "ai_response_guard").await;
    let gid = guard["id"].as_str().expect("id").to_string();
    let gprefix = {
        let runs = state.scenarios.runs();
        runs.iter()
            .find(|r| r.id == gid)
            .expect("run")
            .session_prefix()
    };
    let (_, body) = chat(
        &base,
        primary,
        &format!("{gprefix}1-0"),
        "the customer record",
    )
    .await;
    assert!(body.contains("123-45-6789"), "{body}");
    let (_, body) = chat(&base, primary, "someone", "the customer record").await;
    assert!(!body.contains("123-45-6789"), "{body}");
    // Ended: the profile is gone.
    end(&state, "me", "ai_model_fallback", &id).expect("end");
    let (s, _) = chat(&base, primary, &format!("{prefix}1-0"), "hello").await;
    assert_eq!(s, 200);
}

#[tokio::test]
async fn a_fallback_check_takes_the_kind_of_error() {
    let (_server, state, base) = instance(config()).await;
    let (run, gateway) = ai_run(
        &state,
        &base,
        "ai_model_fallback",
        AiGw {
            fallback: true,
            ..AiGw::default()
        },
    )
    .await;
    let v = check_with(
        &state,
        &run,
        CheckBody::new(&gateway).with("fail_kind", "overloaded"),
    )
    .await;
    assert_eq!(v["passed"], true, "{v:#}");
    let seen = v["expectations"][1]["seen"].as_str().expect("seen");
    assert!(seen.contains("primary 503 then fallback 200"), "{seen}");
    let id = run["id"].as_str().expect("id");
    let e = check(
        &state,
        "me",
        "ai_model_fallback",
        id,
        CheckBody::new(&gateway).with("fail_kind", "teapot"),
    )
    .await
    .expect_err("an unknown kind");
    assert!(e.1.contains("one of rate_limit"), "{}", e.1);
    let e = check(
        &state,
        "me",
        "ai_model_fallback",
        id,
        CheckBody::new(&gateway).with("token_limit", 500),
    )
    .await
    .expect_err("another scenario's parameter");
    assert!(e.1.contains("applies to ai_token_limit only"), "{}", e.1);
}

#[tokio::test]
async fn a_request_limit_does_not_pass_for_a_token_budget() {
    let (_server, state, base) = instance(config()).await;
    // Two requests then 429: refusals, but far under the token budget.
    let (run, gateway) = ai_run(
        &state,
        &base,
        "ai_token_limit",
        AiGw {
            request_limit: Some(2),
            ..AiGw::default()
        },
    )
    .await;
    let v = check_via(&state, &run, &gateway).await;
    assert_eq!(v["passed"], false, "{v:#}");
    assert_eq!(failed(&v), ["The budget was counted in tokens"], "{v:#}");
    // The budget the check is told: a gateway with 1000 passes when told 1000.
    let (run, gateway) = ai_run(
        &state,
        &base,
        "ai_token_limit",
        AiGw {
            token_limit: Some(1000),
            ..AiGw::default()
        },
    )
    .await;
    let v = check_with(
        &state,
        &run,
        CheckBody::new(&gateway).with("token_limit", 1000),
    )
    .await;
    assert_eq!(v["passed"], true, "{v:#}");
}

#[tokio::test]
async fn a_redacting_guard_passes_and_an_exact_cache_does_not() {
    let (_server, state, base) = instance(config()).await;
    let (run, gateway) = ai_run(
        &state,
        &base,
        "ai_prompt_guard",
        AiGw {
            guard: Some(Guard::Redact),
            ..AiGw::default()
        },
    )
    .await;
    let v = check_via(&state, &run, &gateway).await;
    assert_eq!(v["passed"], true, "{v:#}");
    assert!(v.to_string().contains("redacted before the model"), "{v:#}");
    // Without the injection request: two requests, two lines of verdict
    // about them.
    let v = check_with(
        &state,
        &run,
        CheckBody::new(&gateway).with("skip_injection", true),
    )
    .await;
    assert_eq!(v["requests"].as_array().map(Vec::len), Some(2), "{v:#}");
    assert_eq!(v["expectations"].as_array().map(Vec::len), Some(2), "{v:#}");
    // A guard that lets the data through names what reached the model,
    // never the values.
    let (run, plain) = ai_run(&state, &base, "ai_prompt_guard", AiGw::default()).await;
    let v = check_via(&state, &run, &plain).await;
    let seen = v["expectations"][1]["seen"].as_str().expect("seen");
    assert_eq!(seen, "reached the model with: SSN, card number, email");
    assert!(!v.to_string().contains("123-45-6789"), "{v:#}");
    // An exact cache answers none of the rewordings.
    let (run, gateway) = ai_run(
        &state,
        &base,
        "ai_semantic_cache",
        AiGw {
            exact_cache: true,
            ..AiGw::default()
        },
    )
    .await;
    let v = check_via(&state, &run, &gateway).await;
    assert_eq!(
        failed(&v),
        ["Prompts that mean the same were answered from the cache"],
        "{v:#}"
    );
    // A second check of the semantic cache uses new prompts: it passes
    // again on a warm cache.
    let (run, gateway) = ai_run(
        &state,
        &base,
        "ai_semantic_cache",
        AiGw {
            semantic: true,
            ..AiGw::default()
        },
    )
    .await;
    for _ in 0..2 {
        let v = check_via(&state, &run, &gateway).await;
        assert_eq!(v["passed"], true, "{v:#}");
    }
}

#[test]
fn the_cache_prompts_are_close_and_the_others_are_not() {
    use crate::ai::embed::{cosine, embed};
    assert_eq!(catalog::CACHE_TOPICS.len(), catalog::CACHE_OTHERS.len());
    let rounds = catalog::CACHE_TOPICS.len();
    let e = |t: &str| embed(t, 1536);
    let all: Vec<[String; 4]> = (0..rounds)
        .map(|r| catalog::cache_prompts(r, &format!("w{r}")))
        .collect();
    for (r, [first, case, please, other]) in all.iter().enumerate() {
        let f = e(first);
        for t in [case, please] {
            let c = cosine(&f, &e(t));
            assert!(c >= 0.9, "round {r}, {t}: {c}");
            // They differ as text, so only a semantic cache answers them.
            assert_ne!(first, t);
        }
        assert!(cosine(&f, &e(other)) < 0.5, "round {r}: {other}");
        // Nothing of another round is near this round's question, so a
        // cache an earlier check filled does not answer it.
        for (q, prompts) in all.iter().enumerate().filter(|(q, _)| *q != r) {
            for t in prompts {
                let c = cosine(&f, &e(t));
                assert!(c < 0.6, "round {r} near round {q}: {t}: {c}");
            }
        }
    }
}

#[test]
fn every_planted_value_is_what_the_mock_answers_and_the_prompt_carries() {
    for (kind, v) in catalog::PLANTED {
        if !v.contains(' ') && v.starts_with("4111") {
            continue; // the card without spaces: a guard's rewrite
        }
        assert!(
            crate::ai::engine::PII_TEXT.contains(v),
            "{kind} {v} is not in the scripted record"
        );
    }
    assert_eq!(
        catalog::planted_in(catalog::PROMPT_PII),
        ["SSN", "card number", "email"]
    );
    assert!(catalog::injection_in(crate::mcp::tools::INJECTION_SAMPLE));
}

// ── MCP scenarios ───────────────────────────────────────────────────

#[tokio::test]
async fn each_mcp_check_starts_with_an_unpulled_rug() {
    let (_server, state, base) = instance(config()).await;
    let run = run_of(&state, &base, "mcp_rug_pull").await;
    let upstream = run["upstream"].as_str().expect("upstream").to_string();
    assert!(upstream.ends_with("/mcp/attack/rug_pull"), "{upstream}");
    // Straight to the attack server: each check sees the harmless
    // definition first, then the changed one.
    for _ in 0..2 {
        let v = check_via(&state, &run, &upstream).await;
        assert_eq!(
            failed(&v),
            ["The changed definition never reached the client"],
            "{v:#}"
        );
    }
    // A pinning gateway passes, check after check.
    let gateway = start_gw(behaving("mcp_rug_pull", &upstream, None)).await;
    for _ in 0..2 {
        let v = check_via(&state, &run, &gateway).await;
        assert_eq!(v["passed"], true, "{v:#}");
    }
    // The verdict names the requests of the session, never the bodies.
    let v = check_via(&state, &run, &gateway).await;
    let labels: Vec<&str> = v["requests"]
        .as_array()
        .expect("requests")
        .iter()
        .filter_map(|r| r["label"].as_str())
        .collect();
    assert_eq!(
        labels,
        [
            catalog::LABEL_INITIALIZE,
            catalog::LABEL_INITIALIZED,
            catalog::LABEL_FIRST_LIST,
            catalog::LABEL_CALL,
            catalog::LABEL_SECOND_LIST
        ]
    );
    assert!(!v.to_string().contains("RUSTYBIN-RUG-PULL"), "{v:#}");
}

#[tokio::test]
async fn the_attack_servers_are_off_in_public_mode_and_by_the_setting() {
    // Public mode: the routes are 404, the scenarios unlisted, a run refused.
    let mut c = config();
    c.public_mode = true;
    let (_server, state, base) = instance(c).await;
    let r = reqwest::Client::new()
        .post(format!("{base}/mcp/attack/poisoned_description"))
        .header("accept", "application/json, text/event-stream")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } } }))
        .send()
        .await
        .expect("send");
    assert_eq!(r.status().as_u16(), 404);
    let listed = list(&state, "me", true);
    assert!(
        !listed.to_string().contains("mcp_tool_poisoning"),
        "{listed}"
    );
    assert!(
        start(&state, svc(&state, &base), "me", "mcp_tool_poisoning")
            .await
            .is_err()
    );
    // The setting off on an ordinary instance: the same, and an admin is
    // told why.
    let (_server, state, base) = instance(config()).await;
    state
        .settings
        .update(
            std::collections::BTreeMap::from([(
                crate::mcp::attacks::SWITCH_KEY.to_string(),
                json!(false),
            )]),
            std::collections::BTreeMap::new(),
            "test",
        )
        .expect("set");
    let r = reqwest::get(format!(
        "{base}/.well-known/oauth-protected-resource/mcp/attack/scope_overreach"
    ))
    .await
    .expect("get");
    assert_eq!(r.status().as_u16(), 404);
    let e = start(&state, svc(&state, &base), "me", "mcp_rug_pull")
        .await
        .expect_err("refused");
    assert_eq!(e.0, StatusCode::CONFLICT);
    assert!(e.1.contains("mcp.attack_servers"), "{}", e.1);
}

#[tokio::test]
async fn the_scope_overreach_server_asks_for_more_than_it_needs() {
    let (_server, _state, base) = instance(config()).await;
    let r = reqwest::Client::new()
        .post(format!("{base}/mcp/attack/scope_overreach"))
        .header("accept", "application/json, text/event-stream")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }))
        .send()
        .await
        .expect("send");
    assert_eq!(r.status().as_u16(), 401);
    let challenge = r.headers()["www-authenticate"]
        .to_str()
        .expect("header")
        .to_string();
    assert!(
        challenge.contains("scope=\"admin offline_access tools:* files:write\""),
        "{challenge}"
    );
    assert!(challenge.contains("/.well-known/oauth-protected-resource/mcp/attack/scope_overreach"));
    let meta: Value = reqwest::get(format!(
        "{base}/.well-known/oauth-protected-resource/mcp/attack/scope_overreach"
    ))
    .await
    .expect("get")
    .json()
    .await
    .expect("json");
    assert_eq!(meta["scopes_supported"][0], "admin");
}
