//! Running a storyboard: one step's action, a reset, and each session's
//! progress. Every action goes through the function its own route and tool
//! call, as the runner: the permission each needs is checked here, at run
//! time, and a missing one is the `403 permission_required` of that
//! permission. An action that fails for another reason (a checkpoint that
//! is gone, an assertion) is a result with `ok: false`, so a rehearsal goes
//! on to the next step and reports every verdict.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde::Serialize;
use serde_json::{json, Value};

use super::{none_named, visible, Action, Storyboard};
use crate::mocks::service::{PatchBody, ServiceError, Svc};
use crate::rbac::Permission as Perm;
use crate::scenarios::{self, CheckBody};
use crate::state::AppState;

/// Most sessions with progress at once.
pub const MAX_RUNS: usize = 256;
/// A session's progress is forgotten this long after its last step.
pub const RUN_TTL: Duration = Duration::from_secs(12 * 3600);
/// The most of an action's own answer a result repeats (as JSON).
const MAX_DETAIL_BYTES: usize = 16 * 1024;

/// Who runs, and with what.
pub struct Runner {
    pub svc: Svc,
    /// The session the progress, the scenario runs and in-process requests
    /// belong to.
    pub session: String,
    /// For the test history: never a credential.
    pub principal: String,
    /// `{{name}}` values, never kept.
    pub variables: BTreeMap<String, String>,
}

/// What a step did.
#[derive(Clone, Debug, Serialize)]
pub struct StepResult {
    /// The step, counted from 1.
    pub step: usize,
    pub title: String,
    /// The action's `type`.
    pub action: &'static str,
    pub ok: bool,
    pub summary: String,
    /// The action's own answer (a scenario's expectations, a test's
    /// failures, what a restore changed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
    pub ran_at: String,
}

struct Progress {
    storyboard: String,
    /// The save the progress was made on: a replaced storyboard starts over.
    saved_at: String,
    /// The next step, counted from 1.
    current: usize,
    results: BTreeMap<usize, StepResult>,
    /// The scenario runs its `start_scenario` steps started, by scenario.
    scenario_runs: BTreeMap<String, String>,
    touched: Instant,
}

/// Each session's progress through a storyboard: one per session, in
/// memory, at most [`MAX_RUNS`], forgotten after [`RUN_TTL`].
#[derive(Default)]
pub struct Runs {
    items: Mutex<HashMap<String, Progress>>,
}

impl Runs {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Progress>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The session's progress through this save of the storyboard.
    pub fn view(&self, session: &str, storyboard: &str, saved_at: &str, steps: usize) -> Value {
        let items = self.lock();
        match items.get(session).filter(|p| {
            p.storyboard == storyboard && p.saved_at == saved_at && p.touched.elapsed() < RUN_TTL
        }) {
            Some(p) => json!({
                "current": p.current.min(steps.max(1)),
                "done": p.current > steps,
                "results": p.results.values().collect::<Vec<_>>(),
            }),
            None => json!({ "current": 1, "done": false, "results": [] }),
        }
    }

    /// The scenario runs the session's progress started.
    fn scenario_runs(
        &self,
        session: &str,
        storyboard: &str,
        saved_at: &str,
    ) -> BTreeMap<String, String> {
        self.lock()
            .get(session)
            .filter(|p| p.storyboard == storyboard && p.saved_at == saved_at)
            .map(|p| p.scenario_runs.clone())
            .unwrap_or_default()
    }

    /// Keep a step's result (the session's progress through another
    /// storyboard, or an older save of this one, is replaced).
    fn record(
        &self,
        session: &str,
        storyboard: &str,
        saved_at: &str,
        result: StepResult,
        scenario_runs: BTreeMap<String, String>,
    ) {
        let mut items = self.lock();
        items.retain(|_, p| p.touched.elapsed() < RUN_TTL);
        if !items.contains_key(session) && items.len() >= MAX_RUNS {
            let oldest = items
                .iter()
                .min_by_key(|(_, p)| p.touched)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                items.remove(&k);
            }
        }
        let fresh = || Progress {
            storyboard: storyboard.to_string(),
            saved_at: saved_at.to_string(),
            current: 1,
            results: BTreeMap::new(),
            scenario_runs: BTreeMap::new(),
            touched: Instant::now(),
        };
        let p = items.entry(session.to_string()).or_insert_with(fresh);
        if p.storyboard != storyboard || p.saved_at != saved_at {
            *p = fresh();
        }
        p.current = result.step + 1;
        p.results.insert(result.step, result);
        p.scenario_runs = scenario_runs;
        p.touched = Instant::now();
    }

    #[cfg(test)]
    pub(crate) fn record_for_tests(
        &self,
        session: &str,
        storyboard: &str,
        saved_at: &str,
        result: StepResult,
    ) {
        self.record(session, storyboard, saved_at, result, BTreeMap::new());
    }

    /// Forget the session's progress through the storyboard; answers the
    /// scenario runs it had started.
    fn clear(&self, session: &str, storyboard: &str) -> BTreeMap<String, String> {
        let mut items = self.lock();
        match items.get(session) {
            Some(p) if p.storyboard == storyboard => items
                .remove(session)
                .map(|p| p.scenario_runs)
                .unwrap_or_default(),
            _ => BTreeMap::new(),
        }
    }
}

/// What an action did, before it is a step's result.
struct Done {
    ok: bool,
    summary: String,
    detail: Option<Value>,
}

impl Done {
    fn ok(summary: impl Into<String>) -> Self {
        Self {
            ok: true,
            summary: summary.into(),
            detail: None,
        }
    }

    fn with(mut self, detail: Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

fn refused(e: impl Into<(StatusCode, String)>) -> ServiceError {
    let (status, message) = e.into();
    ServiceError::new(status, message)
}

/// Is this the refusal of a missing permission (answered as it is, never
/// as a step's verdict)?
fn is_permission(e: &ServiceError) -> bool {
    e.body.get("reason").and_then(Value::as_str) == Some("permission_required")
}

/// An action's answer, cut to what a result may hold.
fn bounded(v: Value) -> Value {
    match serde_json::to_vec(&v) {
        Ok(b) if b.len() <= MAX_DETAIL_BYTES => v,
        _ => json!({ "truncated": true }),
    }
}

/// The run of `scenario` a `check_scenario` step judges: the one this
/// progress started, else the session's newest run of it.
fn scenario_run(
    state: &AppState,
    session: &str,
    scenario: &str,
    started: &BTreeMap<String, String>,
) -> Option<String> {
    if let Some(id) = started.get(scenario) {
        return Some(id.clone());
    }
    scenarios::list(state, session, false)["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["scenario"] == json!(scenario))
        .filter_map(|r| r["id"].as_str())
        .next_back()
        .map(str::to_string)
}

/// Do what the action says, as the runner. `Err` is a refusal: a missing
/// permission goes to the caller as it is, anything else becomes the
/// step's failed verdict ([`settle`]).
async fn act(
    r: &Runner,
    storyboard: &Storyboard,
    step: usize,
    action: &Action,
    scenario_runs: &mut BTreeMap<String, String>,
) -> Result<Done, ServiceError> {
    let state = &r.svc.state;
    let caller = r.svc.caller.resolved.clone();
    match action {
        Action::None => Ok(Done::ok("a talking point")),
        Action::OpenView { route } => {
            Ok(Done::ok(format!("show {route} in the console")).with(json!({ "open": route })))
        }
        Action::RestoreCheckpoint { name } => {
            let missing = || ServiceError::not_found(format!("no checkpoint named {name:?}"));
            let cp = state.checkpoints.get(name).ok_or_else(missing)?;
            if !caller.has_in(Perm::MocksRead, cp.project.as_deref()) {
                return Err(missing());
            }
            if !caller.has_in(Perm::CheckpointsRestore, cp.project.as_deref()) {
                return Err(ServiceError::permission(&caller, Perm::CheckpointsRestore));
            }
            let v = crate::checkpoints::restore_with(r.svc.clone(), name.clone(), false)
                .await
                .map_err(|e| refused((e.0, e.1)))?;
            let n = |k: &str| v["mocks"][k].as_array().map(Vec::len).unwrap_or(0);
            Ok(Done::ok(format!(
                "restored checkpoint {name}: {} created, {} replaced, {} with data restored, {} deleted, {} unchanged",
                n("created"),
                n("replaced"),
                n("data_restored"),
                n("deleted"),
                n("unchanged")
            ))
            .with(v))
        }
        Action::StartScenario { scenario } => {
            r.svc.need(Perm::ScenariosRun)?;
            // A step run again starts over: its earlier run is ended, so a
            // rehearsal leaves one run a scenario, not one a click.
            if let Some(old) = scenario_runs.remove(scenario) {
                let _ = scenarios::end(state, &r.session, scenario, &old);
            }
            let v = scenarios::start(state, r.svc.clone(), &r.session, scenario)
                .await
                .map_err(|e| refused((e.0, e.1)))?;
            let id = v["id"].as_str().unwrap_or_default().to_string();
            scenario_runs.insert(scenario.clone(), id.clone());
            // What is kept never holds the credential-injection key.
            let kept = scenarios::get_run(state, &r.session, scenario, &id, false).unwrap_or(v);
            Ok(Done::ok(format!(
                "run {id} of {scenario} started; its upstream is {}",
                kept["upstream"].as_str().unwrap_or("?")
            ))
            .with(kept))
        }
        Action::CheckScenario {
            scenario,
            gateway_url,
            params,
        } => {
            r.svc.need(Perm::ScenariosRun)?;
            let Some(run) = scenario_run(state, &r.session, scenario, scenario_runs) else {
                return Err(ServiceError::new(
                    StatusCode::CONFLICT,
                    format!("no run of {scenario} to check: a start_scenario step comes first"),
                ));
            };
            let body = CheckBody {
                gateway_url: gateway_url.clone(),
                params: params.clone(),
            };
            let v = scenarios::check(state, &r.session, scenario, &run, body)
                .await
                .map_err(|e| refused((e.0, e.1)))?;
            let ok = v["passed"] == json!(true);
            let failing: Vec<&str> = v["expectations"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|e| e["ok"] != json!(true))
                .filter_map(|e| e["name"].as_str())
                .collect();
            let summary = if ok {
                format!("the {scenario} scenario passed")
            } else {
                format!("the {scenario} scenario failed: {}", failing.join(", "))
            };
            Ok(Done {
                ok,
                summary,
                detail: Some(v),
            })
        }
        Action::RunTest { name } => {
            let missing = || ServiceError::not_found(format!("no test named {name:?}"));
            let t = state.saved_tests.get(name).ok_or_else(missing)?;
            if !crate::replay::visible(&caller, &t) {
                return Err(missing());
            }
            if !caller.has_in(Perm::TestsRun, t.project.as_deref()) {
                return Err(ServiceError::permission(&caller, Perm::TestsRun));
            }
            let Some(_running) = state.saved_tests.begin_run() else {
                return Err(ServiceError::new(
                    StatusCode::CONFLICT,
                    crate::replay::RUN_BUSY,
                ));
            };
            let body = crate::replay::run::RunBody {
                names: vec![name.clone()],
                variables: r.variables.clone(),
                project: None,
            };
            let kept = crate::replay::run_and_keep(
                state,
                &r.session,
                body,
                &format!("storyboard:{}", storyboard.name),
                &r.principal,
            )
            .await
            .map_err(refused)?;
            Ok(test_verdict(
                &format!("the test {name}"),
                &kept.report,
                Some(&kept.id),
            ))
        }
        Action::Send { .. } => {
            let Some(t) = action.as_test(&format!("step-{step}")) else {
                return Ok(Done::ok("nothing to send"));
            };
            let report =
                crate::replay::run::run_these(state, &r.session, &[t], r.variables.clone()).await;
            Ok(test_verdict("the request", &report, None))
        }
        Action::SetHealth { healthy } => {
            r.svc.need(Perm::SettingsWrite)?;
            state.health.set(*healthy);
            Ok(Done::ok(format!(
                "/health now answers {}",
                if *healthy {
                    "200 (healthy)"
                } else {
                    "503 (unhealthy)"
                }
            ))
            .with(json!({ "healthy": healthy })))
        }
        Action::SetMockMode { mock, mode } => {
            let snap = r.svc.manageable(mock)?;
            let mut responses = snap.draft.settings.responses.clone();
            responses.mode = *mode;
            let patch: PatchBody =
                serde_json::from_value(json!({ "responses": responses, "deploy": true }))
                    .map_err(|e| ServiceError::bad_request(e.to_string()))?;
            let name = mock.clone();
            r.svc
                .clone()
                .compiling(move |s| s.update(&name, patch))
                .await?;
            let mode = serde_json::to_value(mode).unwrap_or(Value::Null);
            Ok(Done::ok(format!(
                "the mock {mock} now answers in {} mode",
                mode.as_str().unwrap_or("?")
            ))
            .with(json!({ "mock": mock, "mode": mode })))
        }
    }
}

/// The verdict of a one-test report.
fn test_verdict(what: &str, report: &crate::replay::run::Report, run_id: Option<&str>) -> Done {
    let result = report.results.first();
    let summary = match result {
        Some(t) if t.passed => format!(
            "{what} passed ({}{})",
            t.status.map(|s| format!("status {s}")).unwrap_or_default(),
            t.ms.map(|ms| format!(", {ms} ms")).unwrap_or_default()
        ),
        Some(t) => match &t.error {
            Some(e) => format!("{what} could not run: {e}"),
            None => format!(
                "{what} failed: {}",
                t.failures
                    .iter()
                    .map(|f| format!("{}: expected {}, saw {}", f.assertion, f.expected, f.seen))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        },
        None => format!("{what} did not run"),
    };
    let mut detail = json!({ "results": report.results });
    if let Some(id) = run_id {
        detail["run_id"] = json!(id);
    }
    Done {
        ok: report.passed && result.is_some(),
        summary,
        detail: Some(detail),
    }
}

/// A refusal that is not about a permission is the step's failed verdict.
fn settle(done: Result<Done, ServiceError>) -> Result<Done, ServiceError> {
    match done {
        Err(e) if !is_permission(&e) => Ok(Done {
            ok: false,
            summary: e.message(),
            detail: None,
        }),
        other => other,
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The storyboard, if the runner may see and run it; the service with no
/// route permission of its own (each action checks what it needs).
fn admit(r: &mut Runner, name: &str) -> Result<(Storyboard, String), ServiceError> {
    crate::audit::target("storyboard", name);
    r.svc.caller.route_need = None;
    let caller = r.svc.caller.resolved.clone();
    let Some((s, saved_at)) = r
        .svc
        .state
        .storyboards
        .get_saved(name)
        .filter(|(s, _)| visible(&caller, s))
    else {
        return Err(none_named(name));
    };
    if !caller.has_in(Perm::TestsRun, s.project.as_deref()) {
        return Err(ServiceError::permission(&caller, Perm::TestsRun));
    }
    Ok((s, saved_at))
}

/// Run step `index` (counted from 1) of the storyboard: the result, kept
/// as the session's progress.
pub async fn run_step(mut r: Runner, name: &str, index: usize) -> Result<Value, ServiceError> {
    let (s, saved_at) = admit(&mut r, name)?;
    let Some(step) = index.checked_sub(1).and_then(|i| s.steps.get(i)) else {
        return Err(ServiceError::not_found(format!(
            "the storyboard {name:?} has {} step(s), counted from 1: no step {index}",
            s.steps.len()
        )));
    };
    let runs = &r.svc.state.storyboards.runs;
    let mut scenario_runs = runs.scenario_runs(&r.session, &s.name, &saved_at);
    let done = settle(act(&r, &s, index, &step.action, &mut scenario_runs).await)?;
    let result = StepResult {
        step: index,
        title: step.title.clone(),
        action: step.action.kind(),
        ok: done.ok,
        summary: done.summary,
        detail: done.detail.map(bounded),
        ran_at: now(),
    };
    runs.record(
        &r.session,
        &s.name,
        &saved_at,
        result.clone(),
        scenario_runs,
    );
    let mut v = serde_json::to_value(&result).unwrap_or(Value::Null);
    v["storyboard"] = json!(s.name);
    v["steps"] = json!(s.steps.len());
    v["next"] = if index < s.steps.len() {
        json!(index + 1)
    } else {
        Value::Null
    };
    Ok(v)
}

/// Reset the storyboard: every step's `reset` action in order, the
/// scenario runs the session's progress started ended, the progress
/// cleared.
pub async fn reset(mut r: Runner, name: &str) -> Result<Value, ServiceError> {
    let (s, saved_at) = admit(&mut r, name)?;
    let state = r.svc.state.clone();
    let mut scenario_runs = state
        .storyboards
        .runs
        .scenario_runs(&r.session, &s.name, &saved_at);
    let mut results = Vec::new();
    for (i, step) in s.steps.iter().enumerate() {
        let Some(action) = &step.reset else { continue };
        let done = settle(act(&r, &s, i + 1, action, &mut scenario_runs).await)?;
        results.push(StepResult {
            step: i + 1,
            title: step.title.clone(),
            action: action.kind(),
            ok: done.ok,
            summary: done.summary,
            detail: done.detail.map(bounded),
            ran_at: now(),
        });
    }
    // What the run started is ended, whichever save it was made on.
    scenario_runs.extend(state.storyboards.runs.clear(&r.session, &s.name));
    let mut ended = Vec::new();
    for (scenario, run) in &scenario_runs {
        if scenarios::end(&state, &r.session, scenario, run).is_ok() {
            ended.push(json!({ "scenario": scenario, "run": run }));
        }
    }
    Ok(json!({
        "storyboard": s.name,
        "ok": results.iter().all(|r| r.ok),
        "reset": results,
        "ended_scenarios": ended,
    }))
}

/// Why a tool asks before it runs this action, if it should: it destroys
/// something, sends traffic out, or replays a request that changes state.
pub fn confirm_reason(state: &AppState, action: &Action) -> Option<String> {
    const SAFE: &[&str] = &["GET", "HEAD", "OPTIONS"];
    let unsafe_method = |m: &str| !SAFE.iter().any(|s| s.eq_ignore_ascii_case(m.trim()));
    match action {
        Action::RestoreCheckpoint { name } => Some(format!(
            "restore checkpoint {name}: the API mocks become what it saved (others are deleted)"
        )),
        Action::CheckScenario {
            scenario,
            gateway_url,
            ..
        } => Some(format!(
            "send the {scenario} scenario's test requests to {}",
            crate::egress::url_origin(gateway_url)
        )),
        Action::Send {
            request, gateway, ..
        } => match gateway {
            Some(g) => Some(format!(
                "send a request through {}",
                crate::egress::url_origin(g)
            )),
            None if unsafe_method(&request.method) => Some(format!(
                "send a {} request that may change state",
                request.method
            )),
            None => None,
        },
        Action::RunTest { name } => {
            let t = state.saved_tests.get(name)?;
            match &t.gateway {
                Some(g) => Some(format!(
                    "send the saved test {name} through {}",
                    crate::egress::url_origin(g)
                )),
                None if t.changes_state() => Some(format!(
                    "replay the saved test {name}, which may change state"
                )),
                None => None,
            }
        }
        _ => None,
    }
}
