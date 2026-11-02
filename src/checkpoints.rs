//! Checkpoints: the state a demo starts from, saved by name and restored
//! before a run. The scope is stated and narrow: the instance's own API mocks
//! (the deployed revision, a draft that differs, on or off, the stateful
//! data) and the health toggle; a restore also resets the flaky counters.
//! Never visitors' or API keys' mocks, file mocks, the sample, a gateway
//! scenario's mock, settings or secrets. At most [`MAX_CHECKPOINTS`], kept in
//! `<state>/checkpoints/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{Extensions, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::mocks::model::MockSettings;
use crate::mocks::service::{ServiceError, Svc};
use crate::mocks::state::StateCopy;
use crate::mocks::store::{Origin, Snapshot};
use crate::mocks::visits::Owner;
use crate::state::AppState;

pub const MAX_CHECKPOINTS: usize = 20;
/// The largest checkpoint (as JSON).
pub const MAX_CHECKPOINT_BYTES: usize = 16 * 1024 * 1024;
/// Every checkpoint together.
pub const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const DIR: &str = "checkpoints";

/// One revision of a mock: its spec text and settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rev {
    pub source: String,
    pub settings: MockSettings,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeptMock {
    pub name: String,
    pub enabled: bool,
    /// The deployed revision (`None`: never deployed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<Rev>,
    /// The draft, when it differs from the deployed revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<Rev>,
    #[serde(default)]
    pub state: StateCopy,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub name: String,
    pub saved_at: String,
    pub healthy: bool,
    pub mocks: Vec<KeptMock>,
}

impl Checkpoint {
    fn summary(&self, bytes: usize) -> Value {
        json!({
            "name": self.name,
            "saved_at": self.saved_at,
            "healthy": self.healthy,
            "mocks": self.mocks.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            "bytes": bytes,
        })
    }
}

/// A mock a checkpoint covers: the instance's own, created through the API,
/// not a scenario's scratch mock (it expires).
pub fn covered(s: &Snapshot) -> bool {
    s.owner == Owner::Instance && s.origin == Origin::Api && s.expires_at.is_none() && !s.scratch
}

fn rev_of(c: &crate::mocks::compile::Compiled) -> Rev {
    Rev {
        source: c.source.to_string(),
        settings: c.settings.clone(),
    }
}

fn kept(s: &Snapshot) -> KeptMock {
    let live = s.live.as_deref().map(rev_of);
    let draft = rev_of(&s.draft);
    KeptMock {
        name: s.name.clone(),
        enabled: s.enabled,
        draft: (live.as_ref() != Some(&draft)).then_some(draft),
        live,
        state: s.state.copy(),
    }
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

// ── The store ───────────────────────────────────────────────────────

struct Item {
    cp: Arc<Checkpoint>,
    bytes: usize,
}

/// The checkpoints of an instance (`AppState.checkpoints`).
pub struct Checkpoints {
    dir: Option<PathBuf>,
    items: Mutex<BTreeMap<String, Item>>,
}

/// A refusal: status and message.
#[derive(Debug)]
pub struct Refused(pub StatusCode, pub String);

impl Refused {
    fn new(status: StatusCode, m: impl Into<String>) -> Self {
        Self(status, m.into())
    }
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<ServiceError> for Refused {
    fn from(e: ServiceError) -> Self {
        Self(e.status, e.message())
    }
}

impl Checkpoints {
    /// The store, loaded from `<state>/checkpoints/` when a state directory is set.
    pub fn new(state_dir: Option<&Path>) -> Self {
        let dir = state_dir.map(|d| d.join(DIR));
        let mut items = BTreeMap::new();
        let mut total = 0usize;
        if let Some(d) = &dir {
            let mut files: Vec<PathBuf> = std::fs::read_dir(d)
                .map(|r| r.filter_map(|e| e.ok().map(|e| e.path())).collect())
                .unwrap_or_default();
            files.sort();
            for f in files {
                if f.extension().and_then(|e| e.to_str()) != Some("json")
                    || items.len() >= MAX_CHECKPOINTS
                {
                    continue;
                }
                let bytes = match std::fs::metadata(&f) {
                    Ok(m) if m.len() as usize <= MAX_CHECKPOINT_BYTES => std::fs::read(&f),
                    Ok(_) => {
                        tracing::warn!(
                            "{}: larger than {MAX_CHECKPOINT_BYTES} bytes, skipped",
                            f.display()
                        );
                        continue;
                    }
                    Err(e) => Err(e),
                };
                match bytes.map_err(|e| e.to_string()).and_then(|b| {
                    let n = b.len();
                    serde_json::from_slice::<Checkpoint>(&b)
                        .map(|c| (c, n))
                        .map_err(|e| e.to_string())
                }) {
                    Ok((cp, n)) if valid_name(&cp.name) && total + n <= MAX_TOTAL_BYTES => {
                        total += n;
                        items.insert(
                            cp.name.clone(),
                            Item {
                                cp: Arc::new(cp),
                                bytes: n,
                            },
                        );
                    }
                    Ok(_) => tracing::warn!("{}: skipped (name or the total size)", f.display()),
                    Err(e) => tracing::warn!("{}: not a checkpoint ({e}), skipped", f.display()),
                }
            }
        }
        Self {
            dir,
            items: Mutex::new(items),
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Item>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn list(&self) -> Vec<Value> {
        self.lock()
            .values()
            .map(|i| i.cp.summary(i.bytes))
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<Arc<Checkpoint>> {
        self.lock().get(name).map(|i| i.cp.clone())
    }

    pub fn summary(&self, name: &str) -> Option<Value> {
        self.lock().get(name).map(|i| i.cp.summary(i.bytes))
    }

    fn put(&self, cp: Checkpoint, replace: bool) -> Result<Value, Refused> {
        let bytes = serde_json::to_vec(&cp)
            .map_err(|e| Refused::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if bytes.len() > MAX_CHECKPOINT_BYTES {
            return Err(Refused::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "the checkpoint would be {} bytes: at most {MAX_CHECKPOINT_BYTES}",
                    bytes.len()
                ),
            ));
        }
        let mut items = self.lock();
        let old = items.get(&cp.name).map(|i| i.bytes);
        if old.is_some() && !replace {
            return Err(Refused::new(
                StatusCode::CONFLICT,
                format!("a checkpoint named {:?} exists: save with replace", cp.name),
            ));
        }
        if old.is_none() && items.len() >= MAX_CHECKPOINTS {
            return Err(Refused::new(
                StatusCode::INSUFFICIENT_STORAGE,
                format!("at most {MAX_CHECKPOINTS} checkpoints: delete one first"),
            ));
        }
        let total: usize = items.values().map(|i| i.bytes).sum::<usize>() - old.unwrap_or(0);
        if total + bytes.len() > MAX_TOTAL_BYTES {
            return Err(Refused::new(
                StatusCode::INSUFFICIENT_STORAGE,
                format!("the checkpoints together keep at most {MAX_TOTAL_BYTES} bytes: delete one first"),
            ));
        }
        if let Some(d) = &self.dir {
            crate::settings::write_private(&d.join(format!("{}.json", cp.name)), &bytes)
                .map_err(|e| Refused::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        }
        let summary = cp.summary(bytes.len());
        items.insert(
            cp.name.clone(),
            Item {
                cp: Arc::new(cp),
                bytes: bytes.len(),
            },
        );
        Ok(summary)
    }

    pub fn delete(&self, name: &str) -> bool {
        let removed = self.lock().remove(name).is_some();
        if removed {
            if let Some(d) = &self.dir {
                let _ = std::fs::remove_file(d.join(format!("{name}.json")));
            }
        }
        removed
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ── Save and restore ────────────────────────────────────────────────

/// Save the instance's mocks and health under `name`.
pub fn save(state: &AppState, name: &str, replace: bool) -> Result<Value, Refused> {
    if !valid_name(name) {
        return Err(Refused::new(
            StatusCode::BAD_REQUEST,
            format!("name {name:?}: 1 to 64 letters, digits, . _ -"),
        ));
    }
    // Under the mocks' edit lock: never half of a restore or of an edit.
    let _edit = state.mocks.edit_lock();
    let mocks: Vec<KeptMock> = state
        .mocks
        .list()
        .iter()
        .filter(|s| covered(s))
        .map(kept)
        .collect();
    let cp = Checkpoint {
        name: name.to_string(),
        saved_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        healthy: state.health.is_healthy(),
        mocks,
    };
    let v = state.checkpoints.put(cp, replace)?;
    tracing::info!(checkpoint = name, "checkpoint saved");
    Ok(v)
}

/// Restore a checkpoint (or, with `dry_run`, say what a restore would
/// change). Runs on the blocking pool: it compiles specs.
pub fn restore(state: &AppState, name: &str, dry_run: bool) -> Result<Value, Refused> {
    let cp = state.checkpoints.get(name).ok_or_else(|| {
        Refused::new(
            StatusCode::NOT_FOUND,
            format!("no checkpoint named {name:?}"),
        )
    })?;
    let store = &state.mocks;
    // One restore at a time, and no edit in the middle of one: the edit
    // lock is held from the first read to the last write.
    let _edit = store.edit_lock();
    let current: BTreeMap<String, Snapshot> = store
        .list()
        .into_iter()
        .map(|s| (s.name.clone(), s))
        .collect();
    let (mut created, mut replaced, mut data, mut unchanged, mut deleted, mut problems) = (
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    for m in &cp.mocks {
        let existing = current.get(&m.name);
        if let Some(s) = existing.filter(|s| !covered(s)) {
            problems.push(format!(
                "{}: now a {} mock, left as it is",
                m.name,
                if s.owner != Owner::Instance {
                    "visitor's or key's"
                } else {
                    "file or scenario"
                }
            ));
            continue;
        }
        let now = existing.map(kept);
        match &now {
            Some(k) if k == m => {
                unchanged.push(m.name.clone());
                continue;
            }
            Some(k) if k.live == m.live && k.draft == m.draft && k.enabled == m.enabled => {
                data.push(m.name.clone());
                if !dry_run {
                    if let Some(s) = existing {
                        s.state.restore(&m.state);
                    }
                }
                continue;
            }
            _ => {}
        }
        if dry_run {
            if now.is_some() {
                replaced.push(m.name.clone());
            } else {
                created.push(m.name.clone());
            }
            continue;
        }
        let compile = |r: &Rev| store.compile(&r.source, r.settings.clone());
        let live = match m.live.as_ref().map(compile).transpose() {
            Ok(l) => l,
            Err(e) => {
                problems.push(format!("{}: {e} (left as it is)", m.name));
                continue;
            }
        };
        let draft = match m.draft.as_ref().map(compile).transpose() {
            Ok(d) => d,
            Err(e) => {
                problems.push(format!("{}: {e} (left as it is)", m.name));
                continue;
            }
        };
        if live.is_none() && draft.is_none() {
            problems.push(format!("{}: no revision in the checkpoint", m.name));
            continue;
        }
        // Built above, swapped in one write: a mock that cannot be restored
        // keeps what it has.
        let made = store.restore_replacing(&m.name, live, draft, m.enabled);
        match made {
            Ok(s) => {
                s.state.restore(&m.state);
                store.persist(&m.name);
                if now.is_some() {
                    replaced.push(m.name.clone());
                } else {
                    created.push(m.name.clone());
                }
            }
            Err(e) => problems.push(format!("{}: {} (left as it is)", m.name, e.message())),
        }
    }
    for (n, s) in &current {
        if covered(s) && !cp.mocks.iter().any(|m| &m.name == n) {
            if !dry_run {
                store.delete(n);
            }
            deleted.push(n.clone());
        }
    }
    let health_before = state.health.is_healthy();
    let flaky = if dry_run {
        state.flaky.len()
    } else {
        state.health.set(cp.healthy);
        state.flaky.reset_all()
    };
    if !dry_run {
        tracing::info!(
            checkpoint = name,
            created = created.len(),
            replaced = replaced.len(),
            deleted = deleted.len(),
            "checkpoint restored"
        );
    }
    let changed =
        !(created.is_empty() && replaced.is_empty() && data.is_empty() && deleted.is_empty())
            || health_before != cp.healthy;
    Ok(json!({
        "checkpoint": name,
        "dry_run": dry_run,
        "changed": changed,
        "mocks": {
            "created": created,
            "replaced": replaced,
            "data_restored": data,
            "unchanged": unchanged,
            "deleted": deleted,
        },
        "health": { "before": if health_before { "healthy" } else { "unhealthy" }, "after": if cp.healthy { "healthy" } else { "unhealthy" } },
        "flaky_counters_reset": flaky,
        "problems": problems,
    }))
}

// ── Routes ──────────────────────────────────────────────────────────

pub const LIST_PATH: &str = "/_rustybin/checkpoints";
const ONE_PATH: &str = "/_rustybin/checkpoints/{name}";
const RESTORE_PATH: &str = "/_rustybin/checkpoints/{name}/restore";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveBody {
    pub name: String,
    #[serde(default)]
    pub replace: bool,
}

async fn list_handler(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "checkpoints": state.checkpoints.list(),
        "max_checkpoints": MAX_CHECKPOINTS,
        "scope": "the instance's API mocks (deployed revision, draft, on or off, stateful data) and the health toggle; a restore also resets the flaky counters. Never visitors' or keys' mocks, file mocks, settings or secrets.",
    }))
}

async fn save_handler(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = crate::admin::require_admin(&headers, &state.config) {
        return r;
    }
    let body: SaveBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            return Refused::new(StatusCode::BAD_REQUEST, format!("invalid body: {e}"))
                .into_response()
        }
    };
    let s = state.clone();
    match tokio::task::spawn_blocking(move || save(&s, &body.name, body.replace)).await {
        Ok(Ok(v)) => (StatusCode::CREATED, Json(v)).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => Refused::new(StatusCode::INTERNAL_SERVER_ERROR, "the save did not finish")
            .into_response(),
    }
}

async fn get_handler(State(state): State<AppState>, UrlPath(name): UrlPath<String>) -> Response {
    match state.checkpoints.summary(&name) {
        Some(v) => Json(v).into_response(),
        None => Refused::new(
            StatusCode::NOT_FOUND,
            format!("no checkpoint named {name:?}"),
        )
        .into_response(),
    }
}

async fn delete_handler(
    State(state): State<AppState>,
    UrlPath(name): UrlPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = crate::admin::require_admin(&headers, &state.config) {
        return r;
    }
    if state.checkpoints.delete(&name) {
        Json(json!({ "deleted": name })).into_response()
    } else {
        Refused::new(
            StatusCode::NOT_FOUND,
            format!("no checkpoint named {name:?}"),
        )
        .into_response()
    }
}

#[derive(Deserialize)]
struct RestoreQuery {
    #[serde(default)]
    dry_run: bool,
}

/// Restore through the service's compile pool (specs are compiled).
pub async fn restore_with(svc: Svc, name: String, dry_run: bool) -> Result<Value, Refused> {
    svc.require_admin()?;
    svc.compiling(move |s| {
        restore(&s.state, &name, dry_run).map_err(|r| ServiceError::new(r.0, r.1))
    })
    .await
    .map_err(Refused::from)
}

async fn restore_handler(
    State(state): State<AppState>,
    UrlPath(name): UrlPath<String>,
    Query(q): Query<RestoreQuery>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    if let Err(r) = crate::admin::require_admin(&headers, &state.config) {
        return r;
    }
    let svc = Svc::from_request(&state, &headers, &extensions, &uri);
    match restore_with(svc, name, q.dry_run).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(LIST_PATH, get(list_handler).post(save_handler))
        .route(ONE_PATH, get(get_handler).delete(delete_handler))
        .route(RESTORE_PATH, post(restore_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            LIST_PATH,
            &["GET", "POST"],
            category::CONTROL,
            "Checkpoints of the instance's API mocks and health: list them, or save one (admin)",
        )
        .example(Example::get("Checkpoints", LIST_PATH)),
        Endpoint::new(
            ONE_PATH,
            &["GET", "DELETE"],
            category::CONTROL,
            "A checkpoint: what it holds; DELETE removes it (admin)",
        )
        .example(Example::get("A checkpoint", "/_rustybin/checkpoints/demo-start").expect_status(404)),
        Endpoint::new(
            RESTORE_PATH,
            &["POST"],
            category::CONTROL,
            "Restore a checkpoint: mocks, their data and health back, flaky counters reset; ?dry_run=true says what would change (admin)",
        )
        .example(Example::post("Preview a restore", "/_rustybin/checkpoints/demo-start/restore?dry_run=true").expect_status(404)),
    ]
}

pub fn openapi_paths() -> Value {
    let obj = json!({ "application/json": { "schema": { "type": "object" } } });
    let name =
        json!({ "name": "name", "in": "path", "required": true, "schema": { "type": "string" } });
    json!({
        LIST_PATH: {
            "get": {
                "tags": ["Control Plane"], "summary": "Checkpoints", "operationId": "listCheckpoints",
                "responses": { "200": { "description": "The checkpoints and their scope", "content": obj } }
            },
            "post": {
                "tags": ["Control Plane"], "summary": "Save a checkpoint (admin)", "operationId": "saveCheckpoint",
                "description": "Saves the instance's API mocks (deployed revision, a draft that differs, on or off, stateful data) and the health toggle. Never visitors' or keys' mocks, file mocks, settings or secrets. At most 20.",
                "requestBody": { "required": true, "content": { "application/json": { "schema": {
                    "type": "object", "required": ["name"], "additionalProperties": false,
                    "properties": { "name": { "type": "string" }, "replace": { "type": "boolean" } }
                } } } },
                "responses": { "201": { "description": "Saved", "content": obj }, "409": { "description": "A checkpoint of that name exists" } }
            }
        },
        ONE_PATH: {
            "get": {
                "tags": ["Control Plane"], "summary": "A checkpoint", "operationId": "getCheckpoint",
                "parameters": [name],
                "responses": { "200": { "description": "What it holds", "content": obj }, "404": { "description": "No such checkpoint" } }
            },
            "delete": {
                "tags": ["Control Plane"], "summary": "Delete a checkpoint (admin)", "operationId": "deleteCheckpoint",
                "parameters": [name],
                "responses": { "200": { "description": "Deleted", "content": obj }, "404": { "description": "No such checkpoint" } }
            }
        },
        RESTORE_PATH: { "post": {
            "tags": ["Control Plane"], "summary": "Restore a checkpoint (admin)", "operationId": "restoreCheckpoint",
            "description": "Brings the instance's API mocks back as saved (created, replaced, data restored, unchanged; mocks the checkpoint does not hold deleted), sets the health toggle and resets the flaky counters. dry_run answers the same without changing anything.",
            "parameters": [name, { "name": "dry_run", "in": "query", "required": false, "schema": { "type": "boolean" } }],
            "responses": { "200": { "description": "What changed", "content": obj }, "404": { "description": "No such checkpoint" } }
        } }
    })
}

#[cfg(test)]
mod tests;
