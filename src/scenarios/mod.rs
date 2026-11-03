//! Guided gateway scenarios: pick an outcome (retries, rate limiting,
//! credential injection, timeouts, caching, circuit breakers, request size
//! limits, CORS, header transformation), and a run prepares the upstream
//! the test needs (a mock, or a `/flaky` route), says how to point the
//! gateway at it, sends test traffic through the gateway and compares what
//! the client got with what reached the upstream (the inspector's captures,
//! tied to each request by its `X-Rustybin-Session`). Ending a run removes
//! what it created and nothing else.
//!
//! Runs are bounded ([`MAX_RUNS`], [`RUN_TTL`]) and belong to the session
//! that started them (`X-Rustybin-Session`, else the client address). A run
//! id has 96 random bits; the sessions of a run's requests carry a per-run
//! secret that is never shown, so nobody can send requests that count in a
//! check. The credential-injection key is in the setup for admin callers
//! only (who could read it on the run's mock anyway): callers without admin
//! rights that share a session key with the starter (one address behind a
//! NAT, a console-scope token) see the run without it. Test traffic goes
//! through the egress policy under `Purpose::Scenario`.
//!
//! The mock of a `cors` run skips the instance's own CORS layer
//! ([`own_cors`]): its preflights reach the mock (and the inspector), and
//! its only CORS answer is the one the check asked for (`upstream_cors`),
//! so what the client sees comes from the gateway or from that answer.

pub mod catalog;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{Extensions, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::egress::Purpose;
use crate::mocks::service::Svc;
use crate::session::Session;
use crate::state::AppState;
pub use catalog::{Scenario, Sent, Upstream, SCENARIOS};

/// Most runs at once, every caller together.
pub const MAX_RUNS: usize = 32;
/// A run (and the mock it created) ends after this.
pub const RUN_TTL: Duration = Duration::from_secs(3600);
/// Most checks of one run.
pub const MAX_CHECKS: u32 = 100;
/// The longest a check request waits for the gateway.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// The most of an answer's body read (and dropped).
const MAX_BODY: usize = 64 * 1024;
/// How often, and how far apart, a check looks for a request's upstream
/// capture before it counts the request as answered by the gateway.
const REACHED_LOOKS: u32 = 5;
const REACHED_SPACING: Duration = Duration::from_millis(50);
/// The time between the trials of a `circuit_breaker` check.
const TRIAL_SPACING: Duration = Duration::from_millis(250);
/// The most headers of an answer a check keeps, and of each value.
const MAX_ANSWER_HEADERS: usize = 64;
const MAX_ANSWER_HEADER_LEN: usize = 512;
/// The header that ties an upstream request to the check request.
pub const SESSION_HEADER: &str = "x-rustybin-session";
/// Random bytes of a run id (96 bits).
const RUN_ID_BYTES: usize = 12;
/// Random bytes of a run's session secret (64 bits).
const RUN_SECRET_BYTES: usize = 8;
/// What the setup shows in place of the key to callers without admin rights.
const KEY_HIDDEN: &str = "<the run's key: shown to admins only>";

pub const LIST_PATH: &str = "/_rustybin/scenarios";
const RUNS_PATH: &str = "/_rustybin/scenarios/{id}/runs";
const RUN_PATH: &str = "/_rustybin/scenarios/{id}/runs/{run}";
const CHECK_PATH: &str = "/_rustybin/scenarios/{id}/runs/{run}/check";

struct Run {
    id: String,
    scenario: &'static Scenario,
    owner: String,
    created_at: String,
    expires_at: String,
    expires: Instant,
    mock: Option<String>,
    upstream: String,
    key: Option<String>,
    /// In every session of the run's requests (`scn-<id>-<secret>-...`),
    /// never shown: a request without it does not count in a check.
    secret: String,
    checks: u32,
    checking: bool,
    last: Option<Value>,
    /// `cors`: the origin the run's mock answers CORS for (the last
    /// check's `allowed_origin` with `upstream_cors: true`), else none.
    cors_answer: Option<String>,
}

impl Run {
    /// The run as its owner sees it; the key only with `reveal` (admin).
    fn view(&self, reveal: bool) -> Value {
        let mut resources = Vec::new();
        if let Some(m) = &self.mock {
            resources.push(json!({ "kind": "mock", "name": m }));
        }
        if let Upstream::Flaky(p) = self.scenario.upstream {
            resources.push(json!({ "kind": "flaky_counters", "route": p, "sessions": format!("scn-{}-*", self.id) }));
        }
        let key = match (&self.key, reveal) {
            (Some(k), true) => Some(k.as_str()),
            (Some(_), false) => Some(KEY_HIDDEN),
            (None, _) => None,
        };
        json!({
            "id": self.id,
            "scenario": self.scenario.id,
            "title": self.scenario.title,
            "proves": self.scenario.proves,
            "created_at": self.created_at,
            "expires_at": self.expires_at,
            "upstream": self.upstream,
            "request_path": self.scenario.request_path,
            "parameters": self.scenario.parameters(),
            "setup":self.scenario.setup_for(&self.upstream, key),
            "key_hidden": self.key.is_some() && !reveal,
            "resources": resources,
            "checks": self.checks,
            "checking": self.checking,
            "last_check": self.last,
            "upstream_answers_cors": self.cors_answer.is_some(),
        })
    }

    /// The prefix of every session the run's requests carry.
    fn session_prefix(&self) -> String {
        format!("scn-{}-{}-", self.id, self.secret)
    }
}

/// A refusal: status and message.
#[derive(Debug)]
pub struct Refused(pub StatusCode, pub String);

impl Refused {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self(status, message.into())
    }
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

/// The runs of an instance (`AppState.scenarios`).
#[derive(Default)]
pub struct Scenarios {
    runs: Mutex<Vec<Run>>,
}

impl Scenarios {
    /// The runs, the expired ones dropped (their mocks expire on their own).
    fn runs(&self) -> MutexGuard<'_, Vec<Run>> {
        let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        runs.retain(|r| r.expires > now || r.checking);
        runs
    }

    pub fn len(&self) -> usize {
        self.runs().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The mock `name` belongs to a live `cors` run: `Some` with the origin
    /// it answers CORS for, if any.
    fn cors_mock(&self, name: &str) -> Option<Option<String>> {
        self.runs()
            .iter()
            .find(|r| r.scenario.id == "cors" && r.mock.as_deref() == Some(name))
            .map(|r| r.cors_answer.clone())
    }
}

fn not_found(scenario: &str, run: &str) -> Refused {
    Refused::new(
        StatusCode::NOT_FOUND,
        format!("no run {run:?} of scenario {scenario:?} in this session"),
    )
}

fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The catalogue and the caller's runs (`reveal`: the caller holds admin
/// rights and sees the credential-injection key).
pub fn list(state: &AppState, owner: &str, reveal: bool) -> Value {
    let runs: Vec<Value> = state
        .scenarios
        .runs()
        .iter()
        .filter(|r| r.owner == owner)
        .map(|r| r.view(reveal))
        .collect();
    json!({
        "scenarios": SCENARIOS.iter().map(Scenario::view).collect::<Vec<_>>(),
        "runs": runs,
        "max_runs": MAX_RUNS,
    })
}

/// One of the caller's runs (`reveal` as in [`list`]).
pub fn get_run(
    state: &AppState,
    owner: &str,
    scenario: &str,
    run: &str,
    reveal: bool,
) -> Result<Value, Refused> {
    state
        .scenarios
        .runs()
        .iter()
        .find(|r| r.id == run && r.scenario.id == scenario && r.owner == owner)
        .map(|r| r.view(reveal))
        .ok_or_else(|| not_found(scenario, run))
}

/// Prepare a run: the upstream mock (admin, through `svc`), the setup.
pub async fn start(
    state: &AppState,
    svc: Svc,
    owner: &str,
    scenario: &str,
) -> Result<Value, Refused> {
    let Some(sc) = catalog::find(scenario) else {
        return Err(Refused::new(
            StatusCode::NOT_FOUND,
            format!(
                "no scenario {scenario:?}: one of {}",
                SCENARIOS
                    .iter()
                    .map(|s| s.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    };
    svc.require_admin()
        .map_err(|e| Refused(e.status, e.message()))?;
    let full = || {
        Refused::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!("at most {MAX_RUNS} scenario runs at once: end one first"),
        )
    };
    if state.scenarios.len() >= MAX_RUNS {
        return Err(full());
    }
    let id = random_hex(RUN_ID_BYTES);
    let key = (sc.id == "credential_injection").then(|| format!("scn-{}", random_hex(12)));
    let base = svc.links.base_url().to_string();
    let (mock, upstream) = match sc.upstream {
        Upstream::Flaky(path) => (None, format!("{base}{path}")),
        Upstream::Mock => {
            let name = format!("scn-{id}");
            let Some((text, settings)) = sc.mock(key.as_deref().unwrap_or("")) else {
                return Err(Refused::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "the scenario has no upstream",
                ));
            };
            let n = name.clone();
            let url = svc
                .compiling(move |s| s.create_scratch(&n, &text, settings, RUN_TTL))
                .await
                .map_err(|e| Refused(e.status, e.message()))?;
            (Some(name), url)
        }
    };
    let now = chrono::Utc::now();
    let run = Run {
        id: id.clone(),
        scenario: sc,
        owner: owner.to_string(),
        created_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        expires_at: (now + chrono::TimeDelta::from_std(RUN_TTL).unwrap_or_default())
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        expires: Instant::now() + RUN_TTL,
        mock,
        upstream,
        key,
        secret: random_hex(RUN_SECRET_BYTES),
        checks: 0,
        checking: false,
        last: None,
        cors_answer: None,
    };
    // The starter is admin (checked above): the answer holds the key.
    let view = run.view(true);
    {
        let mut runs = state.scenarios.runs();
        if runs.len() < MAX_RUNS {
            runs.push(run);
            tracing::info!(scenario = sc.id, run = %id, "gateway scenario started");
            return Ok(view);
        }
    }
    // Filled up meanwhile: take the mock back.
    if let Some(m) = &run.mock {
        state.mocks.delete(m);
    }
    Err(full())
}

/// What a check takes.
#[derive(Debug, Default, Deserialize)]
pub struct CheckBody {
    /// The URL the gateway serves the route at.
    pub gateway_url: String,
    /// The test parameters by name ([`catalog::PARAMS`]); any other field
    /// is refused by [`Scenario::params_of`].
    #[serde(flatten)]
    pub params: serde_json::Map<String, Value>,
}

impl CheckBody {
    pub fn new(gateway_url: impl Into<String>) -> Self {
        Self {
            gateway_url: gateway_url.into(),
            params: serde_json::Map::new(),
        }
    }

    /// With one test parameter.
    pub fn with(mut self, name: &str, value: impl Into<Value>) -> Self {
        self.params.insert(name.to_string(), value.into());
        self
    }
}

/// Clears a run's `checking` flag however the check ends (a dropped
/// request included).
struct Checking {
    scenarios: Arc<Scenarios>,
    run: String,
}

impl Drop for Checking {
    fn drop(&mut self) {
        if let Some(r) = self.scenarios.runs().iter_mut().find(|r| r.id == self.run) {
            r.checking = false;
        }
    }
}

/// Send the test traffic through the gateway and judge it.
pub async fn check(
    state: &AppState,
    owner: &str,
    scenario: &str,
    run: &str,
    body: CheckBody,
) -> Result<Value, Refused> {
    let bad = |m: String| Refused::new(StatusCode::BAD_REQUEST, m);
    let gateway = body.gateway_url.trim().trim_end_matches('/').to_string();
    crate::egress::parse_url(&gateway, false).map_err(|e| bad(format!("gateway_url: {e}")))?;
    let (sc, k, upstream, prefix, params) = {
        let mut runs = state.scenarios.runs();
        let r = runs
            .iter_mut()
            .find(|r| r.id == run && r.scenario.id == scenario && r.owner == owner)
            .ok_or_else(|| not_found(scenario, run))?;
        let params = r.scenario.params_of(&body.params).map_err(bad)?;
        // The body under the limit must fit this instance's own limit.
        if r.scenario.id == "request_size_limit"
            && params.size_limit / 2 > state.config.body_limit as u64
        {
            return Err(bad(format!(
                "size_limit: at most {} on this instance (twice RUSTYBIN_BODY_LIMIT: the body under the limit must reach the upstream)",
                state.config.body_limit as u64 * 2
            )));
        }
        if r.checking {
            return Err(Refused::new(
                StatusCode::CONFLICT,
                "a check of this run is running",
            ));
        }
        if r.checks >= MAX_CHECKS {
            return Err(Refused::new(
                StatusCode::TOO_MANY_REQUESTS,
                format!("at most {MAX_CHECKS} checks per run: start a new run"),
            ));
        }
        r.checking = true;
        r.checks += 1;
        if r.scenario.id == "cors" {
            r.cors_answer = params.upstream_cors.then(|| params.allowed_origin.clone());
        }
        (
            r.scenario,
            r.checks,
            r.upstream.clone(),
            r.session_prefix(),
            params,
        )
    };
    let _checking = Checking {
        scenarios: state.scenarios.clone(),
        run: run.to_string(),
    };
    let target = format!("{gateway}{}", sc.request_path);
    let pinned = state
        .egress
        .resolve(&target, Purpose::Scenario)
        .await
        .map_err(|e| Refused::new(StatusCode::FORBIDDEN, e.0))?;
    let client = state
        .egress
        .client(&pinned, REQUEST_TIMEOUT)
        .map_err(|e| Refused::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // The run's secret prefix: only the requests of this check match it.
    let prefix = format!("{prefix}{k}-");
    let session = |n: usize| format!("{prefix}{n}");
    let mut sent = Vec::new();
    match sc.id {
        "request_size_limit" => {
            // A body half the limit, then one twice the limit.
            let sizes = [params.size_limit / 2, params.size_limit * 2];
            for (n, size) in sizes.into_iter().enumerate() {
                let size = size as usize;
                let out = Out {
                    body: Some(Bytes::from(vec![b'x'; size])),
                    ..Out::default()
                };
                let mut s = send_one(&client, &target, n, &session(n), out).await;
                s.bytes = Some(size);
                sent.push(s);
            }
        }
        "circuit_breaker" => {
            // Until the gateway answers one on its own (nothing upstream).
            let mut opened_at = None;
            for n in 0..catalog::MAX_BREAKER_REQUESTS {
                let s = send_one(&client, &target, n, &session(n), Out::default()).await;
                let answered = Instant::now();
                let failed = s.status == 0;
                sent.push(s);
                if failed {
                    break;
                }
                if !reached_upstream(state, &session(n)).await {
                    opened_at = (n > 0).then_some(answered);
                    break;
                }
            }
            if let Some(at) = opened_at {
                // The half-open time, plus a little for the gateway's clock.
                let ms = params.half_open_ms + params.half_open_ms / 10 + 100;
                tokio::time::sleep_until((at + Duration::from_millis(ms)).into()).await;
                for i in 0..catalog::BREAKER_TRIALS {
                    let n = sent.len();
                    if i > 0 {
                        tokio::time::sleep(TRIAL_SPACING).await;
                    }
                    let mut s = send_one(&client, &target, n, &session(n), Out::default()).await;
                    s.trial = true;
                    let failed = s.status == 0;
                    sent.push(s);
                    if failed || reached_upstream(state, &session(n)).await {
                        break;
                    }
                }
            }
        }
        "cors" => {
            // A preflight from each origin, then a GET from the allowed one.
            let requests = [
                (catalog::PREFLIGHT_ALLOWED, &params.allowed_origin, true),
                (catalog::PREFLIGHT_REFUSED, &params.refused_origin, true),
                (catalog::SIMPLE_ALLOWED, &params.allowed_origin, false),
            ];
            for (n, (label, origin, preflight)) in requests.into_iter().enumerate() {
                let mut headers = vec![("origin".to_string(), origin.clone())];
                if preflight {
                    headers.push(("access-control-request-method".into(), "GET".into()));
                }
                let out = Out {
                    method: preflight.then_some(reqwest::Method::OPTIONS),
                    headers,
                    ..Out::default()
                };
                let mut s = send_one(&client, &target, n, &session(n), out).await;
                s.label = Some(label);
                sent.push(s);
            }
        }
        "header_transformation" => {
            // The header the gateway removes, with a value of this check.
            let marker = format!("scn-{}", random_hex(6));
            let out = Out {
                headers: vec![(params.remove_request_header.clone(), marker.clone())],
                ..Out::default()
            };
            let mut s = send_one(&client, &target, 0, &session(0), out).await;
            s.marker = Some(marker);
            sent.push(s);
        }
        _ => {
            for n in 0..sc.requests(params.limit) {
                sent.push(send_one(&client, &target, n, &session(n), Out::default()).await);
            }
        }
    }
    // Upstream captures land as the upstream finishes: wait for them to
    // settle (the slow upstream of `timeouts` is not waited for).
    let observe = |sent: &mut Vec<Sent>| {
        for s in sent.iter_mut() {
            let mut caps = state.inspector.for_session(&format!("{prefix}{}", s.n), 50);
            caps.sort_by_key(|c| c.seq);
            s.upstream = caps.iter().map(|c| c.status).collect();
            s.upstream_headers = caps
                .iter()
                .map(|c| {
                    c.headers
                        .iter()
                        .map(|h| (h.name.clone(), h.value.clone()))
                        .collect()
                })
                .collect();
        }
        sent.iter().map(|s| s.upstream.len()).sum::<usize>()
    };
    let mut seen = observe(&mut sent);
    if sc.id != "timeouts" {
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let now = observe(&mut sent);
            if now == seen && now > 0 {
                break;
            }
            seen = now;
        }
    }
    let expectations = sc.evaluate(&sent, &params);
    let passed = !expectations.is_empty() && expectations.iter().all(|e| e.ok);
    let mut hints = Vec::new();
    if let Some(e) = sent.iter().find_map(|s| s.error.as_ref()) {
        hints.push(format!("The gateway did not answer: {e}"));
    } else if seen == 0 && sc.id != "timeouts" {
        hints.push(format!(
            "Nothing reached the upstream: point the route at {upstream} and let it forward the X-Rustybin-Session header."
        ));
    }
    if state.inspector.capacity() == 0 {
        hints.push("The inspector keeps no captures on this instance (RUSTYBIN_INSPECTOR_CAPACITY=0): what reached the upstream cannot be seen.".into());
    }
    let verdict = json!({
        "run": run,
        "scenario": sc.id,
        "check": k,
        "gateway_url": gateway,
        "checked_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "passed": passed,
        "expectations": expectations,
        "requests": sent,
        "hints": hints,
    });
    if let Some(r) = state.scenarios.runs().iter_mut().find(|r| r.id == run) {
        r.last = Some(verdict.clone());
    }
    tracing::info!(scenario = sc.id, run, passed, "gateway scenario checked");
    Ok(verdict)
}

/// One request of a check: a GET, a POST of `body`, or `method`; with
/// `headers` (checked names and values) besides the session.
#[derive(Default)]
struct Out {
    method: Option<reqwest::Method>,
    headers: Vec<(String, String)>,
    body: Option<Bytes>,
}

/// Send one request of a check and read (and drop) at most [`MAX_BODY`]
/// of the answer; at most [`MAX_ANSWER_HEADERS`] of its headers are kept.
async fn send_one(
    client: &reqwest::Client,
    target: &str,
    n: usize,
    session: &str,
    out: Out,
) -> Sent {
    let t = Instant::now();
    let mut req = match (out.method, out.body) {
        (Some(m), _) => client.request(m, target),
        (None, Some(b)) => client
            .post(target)
            .header("content-type", "application/octet-stream")
            .body(b),
        (None, None) => client.get(target),
    };
    for (k, v) in &out.headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let r = req.header(SESSION_HEADER, session).send().await;
    let mut s = Sent {
        n,
        ..Sent::default()
    };
    match r {
        Ok(mut resp) => {
            s.status = resp.status().as_u16();
            s.answer = resp
                .headers()
                .iter()
                .take(MAX_ANSWER_HEADERS)
                .filter_map(|(k, v)| {
                    let v: String = v
                        .to_str()
                        .ok()?
                        .chars()
                        .take(MAX_ANSWER_HEADER_LEN)
                        .collect();
                    Some((k.as_str().to_string(), v))
                })
                .collect();
            let mut read = 0usize;
            while let Ok(Some(chunk)) = resp.chunk().await {
                read += chunk.len();
                if read > MAX_BODY {
                    break;
                }
            }
        }
        Err(e) => s.error = Some(without_url(&e)),
    }
    s.ms = t.elapsed().as_millis() as u64;
    s
}

/// Did a request of a check reach the upstream? Its capture may land a
/// moment after the gateway answered: look a few times.
async fn reached_upstream(state: &AppState, session: &str) -> bool {
    for i in 0..REACHED_LOOKS {
        if !state.inspector.for_session(session, 1).is_empty() {
            return true;
        }
        if i + 1 < REACHED_LOOKS {
            tokio::time::sleep(REACHED_SPACING).await;
        }
    }
    false
}

/// A transport error without the URL (the caller gave it; the text stays short).
fn without_url(e: &reqwest::Error) -> String {
    let kind = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "could not connect"
    } else {
        "the request failed"
    };
    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(e);
    let mut last = None;
    while let Some(s) = src {
        last = Some(s.to_string());
        src = s.source();
    }
    match last {
        Some(detail) => format!("{kind} ({detail})"),
        None => kind.to_string(),
    }
}

/// End a run: remove its mock and reset its flaky counters.
pub fn end(state: &AppState, owner: &str, scenario: &str, run: &str) -> Result<Value, Refused> {
    let r = {
        let mut runs = state.scenarios.runs();
        let i = runs
            .iter()
            .position(|r| r.id == run && r.scenario.id == scenario && r.owner == owner)
            .ok_or_else(|| not_found(scenario, run))?;
        if runs[i].checking {
            return Err(Refused::new(
                StatusCode::CONFLICT,
                "a check of this run is running",
            ));
        }
        runs.remove(i)
    };
    let mut removed = Vec::new();
    if let Some(m) = &r.mock {
        if state.mocks.delete(m) {
            removed.push(json!({ "kind": "mock", "name": m }));
        }
    }
    if let Upstream::Flaky(p) = r.scenario.upstream {
        let n = state.flaky.reset_prefix(&r.session_prefix());
        removed.push(json!({ "kind": "flaky_counters", "route": p, "count": n }));
    }
    tracing::info!(scenario = r.scenario.id, run, "gateway scenario ended");
    Ok(json!({ "ended": run, "scenario": r.scenario.id, "removed": removed }))
}

/// An `OPTIONS` request to a `cors` run's mock, held past the instance's
/// CORS layer (which answers every `OPTIONS` itself) by [`own_cors`] as a
/// `GET`, and put back by [`restore_preflight`].
#[derive(Clone)]
struct HeldOptions;

/// The state of [`own_cors`]: `hold` when the instance has a CORS layer
/// (`RUSTYBIN_CORS_ORIGINS` is not `off`) to hold requests past.
#[derive(Clone)]
pub struct OwnCors {
    pub state: AppState,
    pub hold: bool,
}

/// Outside the instance's own CORS layer (`build_app`): for a request to
/// the mock of a live `cors` run, that layer neither answers the preflight
/// (an `OPTIONS` passes it as a `GET` marked [`HeldOptions`], so the
/// request reaches the mock and the inspector) nor leaves CORS headers on
/// the answer, so no CORS answer of the instance hides the gateway's. With
/// `upstream_cors` (the run's last check) the mock's answers carry CORS for
/// the allowed origin. Everything else goes on unchanged.
pub async fn own_cors(
    State(OwnCors { state, hold }): State<OwnCors>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    use axum::http::header;
    let name = req
        .uri()
        .path()
        .strip_prefix("/mock/")
        .and_then(|rest| rest.split('/').next())
        .filter(|n| n.starts_with("scn-"));
    let Some(answer) = name.and_then(|n| state.scenarios.cors_mock(n)) else {
        return next.run(req).await;
    };
    let options = req.method() == axum::http::Method::OPTIONS;
    let preflight = options
        && req
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);
    if options && hold {
        *req.method_mut() = axum::http::Method::GET;
        req.extensions_mut().insert(HeldOptions);
    }
    let requested = req
        .headers()
        .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .cloned();
    let mut resp = next.run(req).await;
    // The instance's CORS answer goes (the mock answers none of its own).
    let h = resp.headers_mut();
    let cors: Vec<_> = h
        .keys()
        .filter(|k| k.as_str().starts_with("access-control-"))
        .cloned()
        .collect();
    for k in cors {
        h.remove(k);
    }
    h.remove(header::VARY);
    let Some(origin) = answer else {
        return resp;
    };
    let Ok(value) = axum::http::HeaderValue::from_str(&origin) else {
        return resp;
    };
    let h = resp.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
    h.append(header::VARY, axum::http::HeaderValue::from_static("origin"));
    if preflight {
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            axum::http::HeaderValue::from_static("GET, HEAD, OPTIONS"),
        );
        if let Some(r) = requested {
            h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, r);
        }
        h.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            axum::http::HeaderValue::from_static("600"),
        );
    }
    resp
}

/// Inside the instance's CORS layer: put back the `OPTIONS` [`own_cors`]
/// held, so the mock and the inspector see the request as sent.
pub async fn restore_preflight(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if req.extensions_mut().remove::<HeldOptions>().is_some() {
        *req.method_mut() = axum::http::Method::OPTIONS;
    }
    next.run(req).await
}

// ── Routes ──────────────────────────────────────────────────────────

/// Does the caller hold admin rights (and see a run's key)?
fn reveals(state: &AppState, headers: &HeaderMap, extensions: &Extensions) -> bool {
    crate::mocks::selfservice::caller(state, headers, extensions).admin
}

async fn list_handler(
    State(state): State<AppState>,
    Session(owner): Session,
    headers: HeaderMap,
    extensions: Extensions,
) -> Json<Value> {
    Json(list(&state, &owner, reveals(&state, &headers, &extensions)))
}

async fn start_handler(
    State(state): State<AppState>,
    Session(owner): Session,
    Path(id): Path<String>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    if let Err(r) = crate::admin::require_admin(&headers, &state.config) {
        return r;
    }
    let svc = Svc::from_request(&state, &headers, &extensions, &uri);
    match start(&state, svc, &owner, &id).await {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn get_handler(
    State(state): State<AppState>,
    Session(owner): Session,
    Path((id, run)): Path<(String, String)>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let reveal = reveals(&state, &headers, &extensions);
    match get_run(&state, &owner, &id, &run, reveal) {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn check_handler(
    State(state): State<AppState>,
    Session(owner): Session,
    Path((id, run)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(r) = crate::admin::require_admin(&headers, &state.config) {
        return r;
    }
    let body: CheckBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            return Refused::new(StatusCode::BAD_REQUEST, format!("invalid body: {e}"))
                .into_response()
        }
    };
    match check(&state, &owner, &id, &run, body).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn end_handler(
    State(state): State<AppState>,
    Session(owner): Session,
    Path((id, run)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = crate::admin::require_admin(&headers, &state.config) {
        return r;
    }
    match end(&state, &owner, &id, &run) {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(LIST_PATH, get(list_handler))
        .route(RUNS_PATH, post(start_handler))
        .route(RUN_PATH, get(get_handler).delete(end_handler))
        .route(CHECK_PATH, post(check_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            LIST_PATH,
            &["GET"],
            category::CONTROL,
            "Gateway scenarios (retries, rate limiting, credential injection, timeouts, caching, circuit breakers, request size limits, CORS, header transformation) and the caller's runs",
        )
        .example(Example::get("Scenarios", LIST_PATH)),
        Endpoint::new(
            RUNS_PATH,
            &["POST"],
            category::CONTROL,
            "Start a scenario run: prepare its upstream and get the gateway setup (admin)",
        )
        .example(
            Example::post("Start a caching run", "/_rustybin/scenarios/caching/runs")
                .expect_status(201),
        ),
        Endpoint::new(
            RUN_PATH,
            &["GET", "DELETE"],
            category::CONTROL,
            "A scenario run with its setup and last verdict; DELETE ends it and removes what it created (admin)",
        )
        .example(
            Example::get("A run", "/_rustybin/scenarios/caching/runs/0000")
                .skip_check("needs a run id"),
        ),
        Endpoint::new(
            CHECK_PATH,
            &["POST"],
            category::CONTROL,
            "Send the run's test traffic through the gateway and compare what the client got with what reached the upstream (admin; egress purpose scenario)",
        )
        .example(
            Example::post(
                "Check through the gateway",
                "/_rustybin/scenarios/caching/runs/0000/check",
            )
            .json(r#"{"gateway_url":"https://gateway.example.com/catalog-api"}"#)
            .skip_check("needs a run and a gateway"),
        ),
    ]
}

/// The properties of a check's body: the gateway URL and every test
/// parameter.
fn check_properties() -> Value {
    let mut props = serde_json::Map::new();
    props.insert(
        "gateway_url".into(),
        json!({ "type": "string", "description": "The URL the gateway serves the route at" }),
    );
    props.extend(catalog::param_properties());
    Value::Object(props)
}

pub fn openapi_paths() -> Value {
    let id = json!({ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "enum": SCENARIOS.iter().map(|s| s.id).collect::<Vec<_>>() } });
    let run =
        json!({ "name": "run", "in": "path", "required": true, "schema": { "type": "string" } });
    let obj = json!({ "application/json": { "schema": { "type": "object" } } });
    json!({
        LIST_PATH: { "get": {
            "tags": ["Control Plane"], "summary": "Gateway scenarios and the caller's runs",
            "operationId": "listGatewayScenarios",
            "responses": { "200": { "description": "The catalogue and the runs", "content": obj } }
        } },
        RUNS_PATH: { "post": {
            "tags": ["Control Plane"], "summary": "Start a scenario run (admin)",
            "description": "Creates the run's upstream (a mock that is never saved and ends with the run, or a /flaky route) and answers the gateway setup. At most 32 runs; a run lasts an hour.",
            "operationId": "startGatewayScenario",
            "parameters": [id],
            "responses": { "201": { "description": "The run", "content": obj }, "404": { "description": "No such scenario" }, "429": { "description": "Too many runs" } }
        } },
        RUN_PATH: {
            "get": {
                "tags": ["Control Plane"], "summary": "A scenario run",
                "operationId": "getGatewayScenarioRun",
                "parameters": [id, run],
                "responses": { "200": { "description": "The run with its setup and last verdict", "content": obj }, "404": { "description": "No such run in this session" } }
            },
            "delete": {
                "tags": ["Control Plane"], "summary": "End a scenario run (admin)",
                "description": "Removes the run's mock and resets its flaky counters; nothing else.",
                "operationId": "endGatewayScenarioRun",
                "parameters": [id, run],
                "responses": { "200": { "description": "What was removed", "content": obj }, "404": { "description": "No such run in this session" }, "409": { "description": "A check is running" } }
            }
        },
        CHECK_PATH: { "post": {
            "tags": ["Control Plane"], "summary": "Check a scenario through the gateway (admin)",
            "description": "Sends the run's requests to gateway_url plus the scenario's path (through the egress policy, purpose scenario), then compares the answers with the upstream requests they caused.",
            "operationId": "checkGatewayScenario",
            "parameters": [id, run],
            "requestBody": { "required": true, "content": { "application/json": { "schema": {
                "type": "object", "required": ["gateway_url"], "additionalProperties": false,
                "properties": check_properties()
            } } } },
            "responses": { "200": { "description": "The verdict", "content": obj }, "400": { "description": "Invalid body" }, "403": { "description": "The egress policy refused the gateway URL" }, "404": { "description": "No such run in this session" }, "409": { "description": "A check is running" } }
        } }
    })
}
