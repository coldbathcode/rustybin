//! Demo storyboards: a demo as a script. A storyboard is an ordered list of
//! steps, each with a talk track for the presenter, an action (restore a
//! checkpoint, start or check a gateway scenario, run a saved test, send a
//! request, set the health toggle, set a mock's response mode, open a
//! console view, or nothing) and an optional reset. A presenter clicks
//! through it in the console; `rustybin storyboard run` walks it as a
//! rehearsal. Kept in `<state>/storyboards.json` and in `rustybin.yaml`
//! (`storyboards:`). A step's action calls the same function as its own
//! route or tool ([`run`]), with the runner's permissions checked at run
//! time, so a storyboard does nothing its runner could not do directly. A
//! credential is never saved: a `send` step's secret-looking headers hold
//! `{{NAME}}` variables, as a saved test's do.

pub mod run;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::mocks::model::ResponseMode;
use crate::mocks::service::{ServiceError, Svc};
use crate::rbac::{Permission as Perm, Resolved};
use crate::replay::{Assertion, SavedTest, TestRequest};
use crate::session::Session;
use crate::state::AppState;

/// Most storyboards.
pub const MAX_STORYBOARDS: usize = 50;
/// Most steps of one.
pub const MAX_STEPS: usize = 40;
/// The largest storyboard (as JSON).
pub const MAX_STORYBOARD_BYTES: usize = 64 * 1024;
/// The largest talk track (Markdown).
pub const MAX_TALK_TRACK: usize = 4 * 1024;
const MAX_NAME: usize = 64;
const MAX_TITLE: usize = 200;
const MAX_DESCRIPTION: usize = 2000;
const MAX_ROUTE: usize = 200;
const FILE: &str = "storyboards.json";

/// One storyboard, as in `rustybin.yaml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Storyboard {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// The project it belongs to; absent: the instance's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// One step: what the presenter says, what Next does, what Reset undoes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub title: String,
    /// What to say (Markdown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub talk_track: Option<String>,
    #[serde(default, skip_serializing_if = "Action::is_none")]
    pub action: Action,
    /// Run by a reset, before a rehearsal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<Action>,
}

/// What a step does. Each one is what a route or a tool already does.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// A talking point.
    #[default]
    None,
    /// Restore a checkpoint (`checkpoints.restore`).
    RestoreCheckpoint { name: String },
    /// Start a gateway scenario run (`scenarios.run`).
    StartScenario { scenario: String },
    /// Check the scenario this storyboard started through the gateway
    /// (`scenarios.run`); its verdict is the step's.
    CheckScenario {
        scenario: String,
        gateway_url: String,
        /// The scenario's test parameters by name.
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        params: serde_json::Map<String, Value>,
    },
    /// Run a saved test (`tests.run`); its assertions are the verdict.
    RunTest { name: String },
    /// Send a request, in process or through a gateway, and judge the
    /// answer with the assertions.
    Send {
        request: TestRequest,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gateway: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        assert: Vec<Assertion>,
    },
    /// Set the `/health` toggle (`settings.write`).
    SetHealth { healthy: bool },
    /// Set a mock's response mode and deploy its draft (`mocks.write`).
    SetMockMode { mock: String, mode: ResponseMode },
    /// Show a console view (`#/mocks/orders`): the console follows it, the
    /// other surfaces answer where to look.
    OpenView { route: String },
}

impl Action {
    pub fn is_none(&self) -> bool {
        matches!(self, Action::None)
    }

    /// Its `type`, as the document spells it.
    pub fn kind(&self) -> &'static str {
        match self {
            Action::None => "none",
            Action::RestoreCheckpoint { .. } => "restore_checkpoint",
            Action::StartScenario { .. } => "start_scenario",
            Action::CheckScenario { .. } => "check_scenario",
            Action::RunTest { .. } => "run_test",
            Action::Send { .. } => "send",
            Action::SetHealth { .. } => "set_health",
            Action::SetMockMode { .. } => "set_mock_mode",
            Action::OpenView { .. } => "open_view",
        }
    }

    /// The test a `send` action is, for the checks and the run.
    pub(crate) fn as_test(&self, name: &str) -> Option<SavedTest> {
        match self {
            Action::Send {
                request,
                gateway,
                assert,
            } => Some(SavedTest {
                name: name.to_string(),
                description: None,
                request: request.clone(),
                gateway: gateway.clone(),
                assert: assert.clone(),
                project: None,
            }),
            _ => None,
        }
    }

    /// The host it sends to on its own word (a gateway), if any.
    pub fn outbound(&self) -> Option<&str> {
        match self {
            Action::CheckScenario { gateway_url, .. } => Some(gateway_url),
            Action::Send { gateway, .. } => gateway.as_deref(),
            _ => None,
        }
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            Action::None | Action::SetHealth { .. } => Ok(()),
            Action::RestoreCheckpoint { name } => {
                if crate::checkpoints::valid_name(name) {
                    Ok(())
                } else {
                    Err(format!("name {name:?}: not a checkpoint name"))
                }
            }
            Action::StartScenario { scenario } => scenario_of(scenario).map(|_| ()),
            Action::CheckScenario {
                scenario,
                gateway_url,
                params,
            } => {
                let sc = scenario_of(scenario)?;
                if gateway_url
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control())
                {
                    return Err("gateway_url: no spaces or control characters".into());
                }
                crate::egress::parse_url(gateway_url, false)
                    .map_err(|e| format!("gateway_url: {e}"))?;
                sc.params_of(params).map(|_| ())
            }
            Action::RunTest { name } => {
                if crate::replay::valid_name(name) {
                    Ok(())
                } else {
                    Err(format!("name {name:?}: not a saved test's name"))
                }
            }
            Action::Send { .. } => match self.as_test("step") {
                Some(t) => t.validate(),
                None => Ok(()),
            },
            Action::SetMockMode { mock, .. } => {
                if valid_name(mock) {
                    Ok(())
                } else {
                    Err(format!("mock {mock:?}: not a mock name"))
                }
            }
            Action::OpenView { route } => {
                if !route.starts_with("#/")
                    || route.len() > MAX_ROUTE
                    || route.chars().any(|c| c.is_whitespace() || c.is_control())
                {
                    return Err(format!(
                        "route {route:?}: a console route such as #/mocks/orders (at most {MAX_ROUTE} characters, no spaces)"
                    ));
                }
                Ok(())
            }
        }
    }
}

fn scenario_of(id: &str) -> Result<&'static crate::scenarios::catalog::Scenario, String> {
    crate::scenarios::catalog::find(id).ok_or_else(|| {
        format!(
            "scenario {id:?}: one of {}",
            crate::scenarios::SCENARIOS
                .iter()
                .map(|s| s.id)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// A name a storyboard may have.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl Storyboard {
    /// Every rule a storyboard follows (a document's too).
    pub fn validate(&self) -> Result<(), String> {
        if !valid_name(&self.name) {
            return Err(format!(
                "name {:?}: 1 to {MAX_NAME} letters, digits, . _ -",
                self.name
            ));
        }
        if let Some(p) = &self.project {
            crate::rbac::projects::check_name(p)?;
        }
        if self.title.as_deref().is_some_and(|t| t.len() > MAX_TITLE) {
            return Err(format!("title: at most {MAX_TITLE} bytes"));
        }
        if self
            .description
            .as_deref()
            .is_some_and(|d| d.len() > MAX_DESCRIPTION)
        {
            return Err(format!("description: at most {MAX_DESCRIPTION} bytes"));
        }
        if self.steps.len() > MAX_STEPS {
            return Err(format!("steps: at most {MAX_STEPS}"));
        }
        for (i, s) in self.steps.iter().enumerate() {
            let n = i + 1;
            if s.title.trim().is_empty() || s.title.len() > MAX_TITLE {
                return Err(format!("steps[{n}].title: 1 to {MAX_TITLE} bytes"));
            }
            if s.talk_track
                .as_deref()
                .is_some_and(|t| t.len() > MAX_TALK_TRACK)
            {
                return Err(format!(
                    "steps[{n}].talk_track: at most {MAX_TALK_TRACK} bytes"
                ));
            }
            s.action
                .validate()
                .map_err(|e| format!("steps[{n}].action: {e}"))?;
            if let Some(r) = &s.reset {
                r.validate().map_err(|e| format!("steps[{n}].reset: {e}"))?;
            }
        }
        let size = serde_json::to_vec(self)
            .map(|v| v.len())
            .unwrap_or(usize::MAX);
        if size > MAX_STORYBOARD_BYTES {
            return Err(format!(
                "the storyboard is {size} bytes: at most {MAX_STORYBOARD_BYTES}"
            ));
        }
        Ok(())
    }

    fn actions(&self) -> impl Iterator<Item = &Action> {
        self.steps
            .iter()
            .flat_map(|s| std::iter::once(&s.action).chain(s.reset.as_ref()))
    }

    /// The variables its `send` steps use (`{{name}}`), in order of
    /// appearance.
    pub fn variables(&self) -> Vec<String> {
        let mut out = Vec::new();
        for t in self.actions().filter_map(|a| a.as_test("step")) {
            for v in t.variables() {
                if !out.contains(&v) {
                    out.push(v);
                }
            }
        }
        out
    }

    /// The gateways its steps send to.
    pub fn outbound(&self) -> BTreeSet<String> {
        self.actions()
            .filter_map(Action::outbound)
            .map(crate::egress::url_origin)
            .collect()
    }
}

// ── The store ───────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    saved_at: String,
    storyboard: Storyboard,
}

#[derive(Serialize, Deserialize)]
struct Doc {
    version: u32,
    storyboards: Vec<Entry>,
}

/// The storyboards of an instance (`AppState.storyboards`), and the
/// presenters' progress ([`run::Runs`]).
pub struct Storyboards {
    path: Option<PathBuf>,
    items: Mutex<BTreeMap<String, Entry>>,
    save_lock: Mutex<()>,
    /// Each session's progress through a storyboard: in memory, bounded.
    pub runs: run::Runs,
}

/// Why a store change was refused.
#[derive(Debug)]
pub enum StoreError {
    Invalid(String),
    Exists(String),
    Full,
    /// The file could not be written: nothing changed.
    Io(String),
}

impl StoreError {
    pub fn status(&self) -> StatusCode {
        match self {
            StoreError::Invalid(_) => StatusCode::BAD_REQUEST,
            StoreError::Exists(_) => StatusCode::CONFLICT,
            StoreError::Full => StatusCode::INSUFFICIENT_STORAGE,
            StoreError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn message(&self) -> String {
        match self {
            StoreError::Invalid(m) => m.clone(),
            StoreError::Exists(n) => format!(
                "a storyboard named {n:?} exists: PUT /_rustybin/storyboards/{n} replaces it"
            ),
            StoreError::Full => {
                format!("at most {MAX_STORYBOARDS} storyboards: delete one first")
            }
            StoreError::Io(e) => {
                format!("the storyboards file could not be written ({e}): nothing changed")
            }
        }
    }
}

impl From<StoreError> for ServiceError {
    fn from(e: StoreError) -> Self {
        ServiceError::new(e.status(), e.message())
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Storyboards {
    /// The store, loaded from `<state>/storyboards.json` when a state
    /// directory is set.
    pub fn new(state_dir: Option<&std::path::Path>) -> Self {
        let path = state_dir.map(|d| d.join(FILE));
        let mut items = BTreeMap::new();
        if let Some(p) = &path {
            match std::fs::read(p) {
                Ok(bytes) => match serde_json::from_slice::<Doc>(&bytes) {
                    Ok(doc) => {
                        for e in doc.storyboards.into_iter().take(MAX_STORYBOARDS) {
                            match e.storyboard.validate() {
                                Ok(()) => {
                                    items.insert(e.storyboard.name.clone(), e);
                                }
                                Err(err) => {
                                    tracing::warn!("{}: storyboard skipped: {err}", p.display())
                                }
                            }
                        }
                    }
                    Err(e) => tracing::warn!(
                        "{}: not a storyboards document ({e}): starting empty",
                        p.display()
                    ),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!("cannot read {}: {e}", p.display()),
            }
        }
        Self {
            path,
            items: Mutex::new(items),
            save_lock: Mutex::new(()),
            runs: run::Runs::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Entry>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// One change at a time; the file is written before the store holds
    /// it, so a failed write changes nothing (as the saved tests).
    fn change<T>(
        &self,
        change: impl FnOnce(&mut BTreeMap<String, Entry>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let _order = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = self.lock().clone();
        let out = change(&mut next)?;
        if let Some(path) = &self.path {
            let doc = Doc {
                version: 1,
                storyboards: next.values().cloned().collect(),
            };
            let bytes =
                serde_json::to_vec_pretty(&doc).map_err(|e| StoreError::Io(e.to_string()))?;
            crate::mocks::store::off_runtime(|| crate::settings::write_private(path, &bytes))
                .map_err(StoreError::Io)?;
        }
        *self.lock() = next;
        Ok(out)
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn list(&self) -> Vec<Storyboard> {
        self.lock().values().map(|e| e.storyboard.clone()).collect()
    }

    pub fn get(&self, name: &str) -> Option<Storyboard> {
        self.lock().get(name).map(|e| e.storyboard.clone())
    }

    /// A storyboard and when it was saved (a run's progress belongs to
    /// that save).
    pub(crate) fn get_saved(&self, name: &str) -> Option<(Storyboard, String)> {
        self.lock()
            .get(name)
            .map(|e| (e.storyboard.clone(), e.saved_at.clone()))
    }

    fn view(e: &Entry) -> Value {
        let mut v = serde_json::to_value(&e.storyboard).unwrap_or(Value::Null);
        if let Value::Object(m) = &mut v {
            m.insert("saved_at".into(), json!(e.saved_at));
            m.insert("variables".into(), json!(e.storyboard.variables()));
        }
        v
    }

    pub fn view_of(&self, name: &str) -> Option<Value> {
        self.lock().get(name).map(Self::view)
    }

    /// Save a storyboard: `replace` lets it overwrite one of the same name.
    pub fn put(&self, storyboard: Storyboard, replace: bool) -> Result<Value, StoreError> {
        storyboard.validate().map_err(StoreError::Invalid)?;
        self.change(|items| {
            let exists = items.contains_key(&storyboard.name);
            if exists && !replace {
                return Err(StoreError::Exists(storyboard.name));
            }
            if !exists && items.len() >= MAX_STORYBOARDS {
                return Err(StoreError::Full);
            }
            let entry = Entry {
                saved_at: now(),
                storyboard,
            };
            let view = Self::view(&entry);
            items.insert(entry.storyboard.name.clone(), entry);
            Ok(view)
        })
    }

    /// Delete one: `Ok(false)` when there is none of that name.
    pub fn delete(&self, name: &str) -> Result<bool, StoreError> {
        if !self.lock().contains_key(name) {
            return Ok(false);
        }
        self.change(|items| Ok(items.remove(name).is_some()))
    }
}

// ── The service (the routes, the tools and the documents share it) ──

/// May the caller see a storyboard (`tests.read` in its project, or
/// instance-wide)?
pub fn visible(caller: &Resolved, s: &Storyboard) -> bool {
    caller.has_in(Perm::TestsRead, s.project.as_deref())
}

fn none_named(name: &str) -> ServiceError {
    ServiceError::not_found(format!("no storyboard named {name:?}"))
}

/// The storyboards the caller may see, optionally of one project.
pub fn list_for(
    state: &AppState,
    caller: &Resolved,
    filter: &Option<crate::rbac::projects::Filter>,
) -> Vec<Value> {
    state
        .storyboards
        .list()
        .iter()
        .filter(|s| {
            visible(caller, s) && crate::rbac::projects::passes(filter, s.project.as_deref())
        })
        .filter_map(|s| state.storyboards.view_of(&s.name))
        .collect()
}

/// One storyboard with the session's progress through it, `404` when the
/// caller may not see it.
pub fn view_for(
    state: &AppState,
    caller: &Resolved,
    session: &str,
    name: &str,
) -> Result<Value, ServiceError> {
    let Some((s, saved_at)) = state
        .storyboards
        .get_saved(name)
        .filter(|(s, _)| visible(caller, s))
    else {
        return Err(none_named(name));
    };
    let mut v = state
        .storyboards
        .view_of(&s.name)
        .ok_or_else(|| none_named(name))?;
    v["progress"] = state
        .storyboards
        .runs
        .view(session, &s.name, &saved_at, s.steps.len());
    Ok(v)
}

/// Where a storyboard the caller saves goes, and may it replace the one of
/// that name: `tests.write` in its project (the one named, else the
/// caller's one project), and on the one it replaces (a move needs both).
pub fn admit_for(
    state: &AppState,
    caller: &Resolved,
    mut s: Storyboard,
) -> Result<Storyboard, ServiceError> {
    crate::audit::target("storyboard", &s.name);
    let write = Perm::TestsWrite;
    let old = state.storyboards.get(&s.name);
    s.project = match (&old, s.project.as_deref()) {
        // A replace that names no project keeps the old one's.
        (Some(o), None) => o.project.clone(),
        _ => crate::rbac::projects::target(&state.users, caller, write, s.project.as_deref())?,
    };
    if let Some(o) = &old {
        if !visible(caller, o) {
            return Err(ServiceError::new(
                StatusCode::CONFLICT,
                format!("the name {:?} is taken: pick another", s.name),
            ));
        }
        crate::rbac::projects::check_move(
            &state.users,
            caller,
            write,
            o.project.as_deref(),
            s.project.as_deref(),
        )?;
    }
    Ok(s)
}

/// Save a storyboard for the caller (`replace`: over one of its name).
pub fn save_for(
    state: &AppState,
    caller: &Resolved,
    s: Storyboard,
    replace: bool,
) -> Result<Value, ServiceError> {
    if !replace && state.storyboards.get(&s.name).is_some() {
        return Err(StoreError::Exists(s.name).into());
    }
    let s = admit_for(state, caller, s)?;
    Ok(state.storyboards.put(s, replace)?)
}

/// Delete a storyboard (`tests.write` in its project).
pub fn delete_for(state: &AppState, caller: &Resolved, name: &str) -> Result<Value, ServiceError> {
    crate::audit::target("storyboard", name);
    let Some(s) = state.storyboards.get(name).filter(|s| visible(caller, s)) else {
        return Err(none_named(name));
    };
    if !caller.has_in(Perm::TestsWrite, s.project.as_deref()) {
        return Err(ServiceError::permission(caller, Perm::TestsWrite));
    }
    match state.storyboards.delete(name) {
        Ok(true) => Ok(json!({ "deleted": name })),
        Ok(false) => Err(none_named(name)),
        Err(e) => Err(e.into()),
    }
}

/// A full storyboard for `PUT` and the tools (the name comes from the path
/// or the argument; what a read adds is dropped).
pub fn from_value(name: &str, mut v: Value) -> Result<Storyboard, String> {
    let Value::Object(m) = &mut v else {
        return Err("a storyboard object".into());
    };
    match m.get("name") {
        Some(Value::String(n)) if n != name => {
            return Err(format!("the body names {n:?}, the path {name:?}"))
        }
        _ => {}
    }
    m.insert("name".into(), json!(name));
    for k in ["saved_at", "variables", "progress"] {
        m.remove(k);
    }
    serde_json::from_value(v).map_err(|e| e.to_string())
}

// ── Routes ──────────────────────────────────────────────────────────

pub const LIST_PATH: &str = "/_rustybin/storyboards";
pub const ONE_PATH: &str = "/_rustybin/storyboards/{name}";
pub const STEP_PATH: &str = "/_rustybin/storyboards/{name}/steps/{index}/run";
pub const RESET_PATH: &str = "/_rustybin/storyboards/{name}/reset";

fn refuse(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

#[derive(Deserialize)]
struct ProjectQuery {
    #[serde(default)]
    project: Option<String>,
}

fn caller(
    state: &AppState,
    headers: &HeaderMap,
    extensions: &axum::http::Extensions,
) -> Arc<Resolved> {
    crate::control_auth::caller_of(state, headers, extensions)
}

async fn list_handler(
    State(state): State<AppState>,
    Query(q): Query<ProjectQuery>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
) -> Response {
    let filter = match crate::rbac::projects::Filter::parse(q.project.as_deref()) {
        Ok(f) => f,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, e),
    };
    let c = caller(&state, &headers, &extensions);
    Json(json!({
        "storyboards": list_for(&state, &c, &filter),
        "max_storyboards": MAX_STORYBOARDS,
        "max_steps": MAX_STEPS,
    }))
    .into_response()
}

async fn create_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
    body: Bytes,
) -> Response {
    let s: Storyboard = match serde_json::from_slice(&body) {
        Ok(s) => s,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, format!("invalid body: {e}")),
    };
    let c = caller(&state, &headers, &extensions);
    match save_for(&state, &c, s, false) {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn get_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Session(session): Session,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
) -> Response {
    let c = caller(&state, &headers, &extensions);
    match view_for(&state, &c, &session, &name) {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn put_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
    body: Bytes,
) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, format!("invalid body: {e}")),
    };
    let s = match from_value(&name, v) {
        Ok(s) => s,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, e),
    };
    let c = caller(&state, &headers, &extensions);
    match save_for(&state, &c, s, true) {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn delete_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
) -> Response {
    let c = caller(&state, &headers, &extensions);
    match delete_for(&state, &c, &name) {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

/// The body of a step run and of a reset.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunBody {
    /// `{{name}}` values (else `RUSTYBIN_TEST_*` environment variables).
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
}

#[allow(clippy::result_large_err)]
fn run_body(body: &Bytes) -> Result<RunBody, Response> {
    if body.is_empty() {
        return Ok(RunBody::default());
    }
    serde_json::from_slice(body)
        .map_err(|e| refuse(StatusCode::BAD_REQUEST, format!("invalid body: {e}")))
}

#[allow(clippy::too_many_arguments)]
fn runner(
    state: &AppState,
    session: String,
    key: Option<axum::Extension<crate::keys::KeyContext>>,
    headers: &HeaderMap,
    extensions: &axum::http::Extensions,
    uri: &Uri,
    body: RunBody,
) -> run::Runner {
    run::Runner {
        svc: Svc::from_request(state, headers, extensions, uri),
        session,
        principal: crate::replay::rest_principal(state, headers, key.as_ref().map(|k| &k.0)),
        variables: body.variables,
    }
}

#[allow(clippy::too_many_arguments)]
async fn step_handler(
    State(state): State<AppState>,
    Path((name, index)): Path<(String, String)>,
    Session(session): Session,
    key: Option<axum::Extension<crate::keys::KeyContext>>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
    uri: Uri,
    body: Bytes,
) -> Response {
    let body = match run_body(&body) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let Ok(index) = index.parse::<usize>() else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "the step is a number, counted from 1",
        );
    };
    let r = runner(&state, session, key, &headers, &extensions, &uri, body);
    match run::run_step(r, &name, index).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

#[allow(clippy::too_many_arguments)]
async fn reset_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Session(session): Session,
    key: Option<axum::Extension<crate::keys::KeyContext>>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
    uri: Uri,
    body: Bytes,
) -> Response {
    let body = match run_body(&body) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let r = runner(&state, session, key, &headers, &extensions, &uri, body);
    match run::reset(r, &name).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(LIST_PATH, get(list_handler).post(create_handler))
        .route(
            ONE_PATH,
            get(get_handler).put(put_handler).delete(delete_handler),
        )
        .route(STEP_PATH, post(step_handler))
        .route(RESET_PATH, post(reset_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            LIST_PATH,
            &["GET", "POST"],
            category::CONTROL,
            "Demo storyboards: list them, or save a new one (steps with a talk track, an action and a reset)",
        )
        .permission_for(&["GET"], Perm::TestsRead)
        .permission_for(&["POST"], Perm::TestsWrite)
        .project_scoped()
        .example(Example::get("Storyboards", LIST_PATH)),
        Endpoint::new(
            ONE_PATH,
            &["GET", "PUT", "DELETE"],
            category::CONTROL,
            "A storyboard with your progress through it: read, replace or delete it",
        )
        .permission_for(&["GET"], Perm::TestsRead)
        .permission_for(&["PUT", "DELETE"], Perm::TestsWrite)
        .project_scoped()
        .example(Example::get("A storyboard", "/_rustybin/storyboards/retry-demo").expect_status(404)),
        Endpoint::new(
            STEP_PATH,
            &["POST"],
            category::CONTROL,
            "Run one step of a storyboard (counted from 1): its action, with your own permissions, and the verdict",
        )
        .permission(Perm::TestsRun)
        .project_scoped()
        .example(
            Example::post("Run step 1", "/_rustybin/storyboards/retry-demo/steps/1/run")
                .json("{}")
                .expect_status(404),
        ),
        Endpoint::new(
            RESET_PATH,
            &["POST"],
            category::CONTROL,
            "Reset a storyboard before a rehearsal: every step's reset action, the scenario runs it started ended, your progress cleared",
        )
        .permission(Perm::TestsRun)
        .project_scoped()
        .example(
            Example::post("Reset", "/_rustybin/storyboards/retry-demo/reset")
                .json("{}")
                .expect_status(404),
        ),
    ]
}

pub fn openapi_paths() -> Value {
    let obj = json!({ "application/json": { "schema": { "type": "object" } } });
    let name =
        json!({ "name": "name", "in": "path", "required": true, "schema": { "type": "string" } });
    let run_body = json!({ "required": false, "content": { "application/json": { "schema": {
        "type": "object", "additionalProperties": false,
        "properties": { "variables": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Values of the {{name}} variables of send steps and saved tests (else RUSTYBIN_TEST_* on the instance); never kept" } }
    } } } });
    json!({
        LIST_PATH: {
            "get": {
                "tags": ["Control Plane"], "summary": "Demo storyboards",
                "operationId": "listStoryboards",
                "parameters": [{ "name": "project", "in": "query", "required": false, "description": "One project's storyboards (instance: the instance's)", "schema": { "type": "string" } }],
                "responses": { "200": { "description": "The storyboards with the variables each uses", "content": obj } }
            },
            "post": {
                "tags": ["Control Plane"], "summary": "Save a storyboard",
                "description": "A demo as a script: steps with a title, a talk track (Markdown), an action (none, restore_checkpoint, start_scenario, check_scenario, run_test, send, set_health, set_mock_mode, open_view) and an optional reset action. At most 50 storyboards of 40 steps, 64 KiB each. A credential is never saved: a send step's secret-looking headers hold {{variables}}. project: the project it belongs to (tests.write there).",
                "operationId": "createStoryboard",
                "requestBody": { "required": true, "content": { "application/json": { "schema": item_schema() } } },
                "responses": { "201": { "description": "The saved storyboard", "content": obj }, "400": { "description": "Invalid storyboard" }, "409": { "description": "A storyboard of that name exists" } }
            }
        },
        ONE_PATH: {
            "get": {
                "tags": ["Control Plane"], "summary": "A storyboard", "operationId": "getStoryboard",
                "description": "The storyboard and progress: the step this session (X-Rustybin-Session, else the client address) is at and each step's last result. Progress is kept in memory, never part of the instance's state.",
                "parameters": [name],
                "responses": { "200": { "description": "The storyboard with progress", "content": obj }, "404": { "description": "No such storyboard" } }
            },
            "put": {
                "tags": ["Control Plane"], "summary": "Save or replace a storyboard", "operationId": "putStoryboard",
                "parameters": [name],
                "requestBody": { "required": true, "content": { "application/json": { "schema": item_schema() } } },
                "responses": { "200": { "description": "The saved storyboard", "content": obj }, "400": { "description": "Invalid storyboard" } }
            },
            "delete": {
                "tags": ["Control Plane"], "summary": "Delete a storyboard", "operationId": "deleteStoryboard",
                "parameters": [name],
                "responses": { "200": { "description": "Deleted", "content": obj }, "404": { "description": "No such storyboard" } }
            }
        },
        STEP_PATH: { "post": {
            "tags": ["Control Plane"], "summary": "Run a step of a storyboard",
            "description": "Runs the step's action as the caller: tests.run on the storyboard, plus what the action itself needs (checkpoints.restore, scenarios.run, tests.run on the saved test, settings.write for the health toggle, mocks.write on the mock); a missing permission is 403 permission_required and nothing runs. The answer is the verdict: ok, a summary, the action's own answer in detail, and the next step. An action that fails (a missing checkpoint, a failed assertion) is ok: false, not an error status.",
            "operationId": "runStoryboardStep",
            "parameters": [name, { "name": "index", "in": "path", "required": true, "description": "The step, counted from 1", "schema": { "type": "integer", "minimum": 1 } }],
            "requestBody": run_body,
            "responses": { "200": { "description": "The step's result", "content": obj }, "403": { "description": "The caller lacks a permission the action needs" }, "404": { "description": "No such storyboard or step" } }
        } },
        RESET_PATH: { "post": {
            "tags": ["Control Plane"], "summary": "Reset a storyboard",
            "description": "Runs every step's reset action in order (with the caller's permissions, as a step), ends the scenario runs this session's progress started and clears the progress.",
            "operationId": "resetStoryboard",
            "parameters": [name],
            "requestBody": run_body,
            "responses": { "200": { "description": "Each reset's result", "content": obj }, "403": { "description": "The caller lacks a permission a reset needs" }, "404": { "description": "No such storyboard" } }
        } }
    })
}

/// The JSON Schema of an action (`action`, `reset`).
pub fn action_schema() -> Value {
    let s = json!({ "type": "string" });
    let variant = |kind: &str, description: &str, props: Value, required: &[&str]| {
        let mut properties = serde_json::Map::new();
        properties.insert("type".into(), json!({ "const": kind }));
        if let Value::Object(p) = props {
            properties.extend(p);
        }
        let mut req = vec!["type"];
        req.extend_from_slice(required);
        json!({
            "type": "object", "additionalProperties": false, "title": kind,
            "description": description, "required": req, "properties": properties
        })
    };
    let request = json!({
        "type": "object", "additionalProperties": false, "required": ["path"],
        "properties": {
            "method": { "type": "string", "enum": crate::replay::METHODS },
            "path": s,
            "headers": { "type": "object", "additionalProperties": { "type": "string" } },
            "body": s
        }
    });
    let assertion = json!({
        "type": "object", "additionalProperties": false,
        "properties": {
            "status": { "type": "integer", "minimum": 100, "maximum": 599 },
            "header": s,
            "body_contains": s,
            "json": { "type": "string", "description": "A path: $.a.b[0]" },
            "equals": {},
            "max_ms": { "type": "integer", "minimum": 0 }
        }
    });
    let scenario = json!({ "type": "string", "enum": crate::scenarios::SCENARIOS.iter().map(|s| s.id).collect::<Vec<_>>() });
    json!({ "oneOf": [
        variant("none", "A talking point", json!({}), &[]),
        variant("restore_checkpoint", "Restore a checkpoint (checkpoints.restore)", json!({ "name": s }), &["name"]),
        variant("start_scenario", "Start a gateway scenario run (scenarios.run)", json!({ "scenario": scenario }), &["scenario"]),
        variant("check_scenario", "Check the scenario this storyboard started, through the gateway (scenarios.run)", json!({
            "scenario": scenario,
            "gateway_url": { "type": "string", "description": "The URL the gateway serves the route at" },
            "params": { "type": "object", "description": "The scenario's test parameters by name", "properties": crate::scenarios::catalog::param_properties() }
        }), &["scenario", "gateway_url"]),
        variant("run_test", "Run a saved test (tests.run)", json!({ "name": s }), &["name"]),
        variant("send", "Send a request, in process or through a gateway, and judge the answer", json!({
            "request": request,
            "gateway": { "type": "string", "description": "A gateway base URL; absent: this instance" },
            "assert": { "type": "array", "items": assertion }
        }), &["request"]),
        variant("set_health", "Set the /health toggle (settings.write)", json!({ "healthy": { "type": "boolean" } }), &["healthy"]),
        variant("set_mock_mode", "Set a mock's response mode and deploy its draft (mocks.write)", json!({
            "mock": s,
            "mode": { "type": "string", "enum": ["examples", "generated", "static", "stateful"] }
        }), &["mock", "mode"]),
        variant("open_view", "Show a console view", json!({ "route": { "type": "string", "pattern": "^#/", "maxLength": MAX_ROUTE } }), &["route"])
    ] })
}

/// The JSON Schema of one storyboard.
pub fn item_schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["name"],
        "properties": {
            "name": { "type": "string", "maxLength": MAX_NAME, "pattern": "^[A-Za-z0-9._-]+$" },
            "title": { "type": "string", "maxLength": MAX_TITLE },
            "description": { "type": "string", "maxLength": MAX_DESCRIPTION },
            "project": { "type": "string", "description": "The project it belongs to; absent: the instance's" },
            "steps": { "type": "array", "maxItems": MAX_STEPS, "items": {
                "type": "object", "additionalProperties": false, "required": ["title"],
                "properties": {
                    "title": { "type": "string", "maxLength": MAX_TITLE },
                    "talk_track": { "type": "string", "maxLength": MAX_TALK_TRACK, "description": "What the presenter says (Markdown)" },
                    "action": action_schema(),
                    "reset": action_schema()
                }
            } }
        }
    })
}

/// `storyboards:` of `rustybin.yaml`.
pub fn schema() -> Value {
    json!({
        "type": "array", "maxItems": MAX_STORYBOARDS,
        "description": "Demo storyboards: steps with a talk track, an action and a reset",
        "items": item_schema()
    })
}
