//! Checkpoints: the state a demo starts from, saved by name and restored
//! before a run. The scope is stated and narrow: the instance's own API mocks
//! (the deployed revision, a draft that differs, on or off, the stateful
//! data) and the health toggle; a restore also resets the flaky counters.
//! Never visitors' or API keys' mocks, file mocks, the sample, a gateway
//! scenario's mock, settings or secrets. At most [`MAX_CHECKPOINTS`], kept in
//! `<state>/checkpoints/`. An explicit git commit can also write one next to
//! `rustybin.yaml` (`crate::sourcecontrol::checkpoints`); `?from=git` lists
//! and restores the repository's, read as untrusted ([`parse_untrusted`]).

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
    /// The mock's project when it was kept (step 11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub name: String,
    pub saved_at: String,
    pub healthy: bool,
    pub mocks: Vec<KeptMock>,
    /// A project's checkpoint (step 11) keeps that project's mocks only and
    /// a restore changes only those (never the health toggle); `None`: the
    /// instance's, every API mock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

impl Checkpoint {
    fn summary(&self, bytes: usize) -> Value {
        json!({
            "name": self.name,
            "saved_at": self.saved_at,
            "healthy": self.healthy,
            "mocks": self.mocks.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            "bytes": bytes,
            "project": self.project,
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
        project: s.project.clone(),
    }
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// A checkpoint from outside the instance (a file of the configuration
/// repository, `<name>.json`): parsed and held to what a local one can be.
/// The size, the name (the file's), at most the instance's number of
/// mocks, valid and distinct mock names, a revision each within the spec
/// limit, the stateful data within a mock's budget. The specs and settings
/// are then compiled and checked by the restore, like a local checkpoint's.
pub fn parse_untrusted(
    bytes: &[u8],
    name: &str,
    limits: &crate::mocks::store::MockLimits,
) -> Result<Checkpoint, String> {
    let file = format!("{name}.json");
    if bytes.len() > MAX_CHECKPOINT_BYTES {
        return Err(format!(
            "{file} is {} bytes: at most {MAX_CHECKPOINT_BYTES}",
            bytes.len()
        ));
    }
    let cp: Checkpoint =
        serde_json::from_slice(bytes).map_err(|e| format!("{file} is not a checkpoint: {e}"))?;
    if cp.name != name || !valid_name(&cp.name) {
        return Err(format!(
            "{file} holds the checkpoint {:?}: a checkpoint file is named after its checkpoint",
            truncate(&cp.name, 80)
        ));
    }
    if cp.saved_at.len() > 64 {
        return Err(format!("{file}: saved_at is not a time"));
    }
    if cp.mocks.len() > limits.max_mocks {
        return Err(format!(
            "{file} holds {} mocks: this instance keeps at most {}",
            cp.mocks.len(),
            limits.max_mocks
        ));
    }
    let project_ok = |p: &Option<String>| {
        p.as_deref()
            .is_none_or(|p| crate::rbac::projects::check_name(p).is_ok())
    };
    if !project_ok(&cp.project) || !cp.mocks.iter().all(|m| project_ok(&m.project)) {
        return Err(format!("{file}: a project is not a project name"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for m in &cp.mocks {
        if !crate::mocks::model::valid_name(&m.name) {
            return Err(format!(
                "{file}: mock {:?} is not a mock name (1 to 40 characters a-z, 0-9 and '-')",
                truncate(&m.name, 80)
            ));
        }
        if !seen.insert(m.name.as_str()) {
            return Err(format!("{file}: mock {} appears twice", m.name));
        }
        if m.live.is_none() && m.draft.is_none() {
            return Err(format!("{file}: mock {} has no revision", m.name));
        }
        for r in [&m.live, &m.draft].into_iter().flatten() {
            if r.source.len() > limits.max_spec_bytes {
                return Err(format!(
                    "{file}: the spec of mock {} is {} bytes: at most {}",
                    m.name,
                    r.source.len(),
                    limits.max_spec_bytes
                ));
            }
        }
        let collections = m.state.collections.len();
        let items: usize = m.state.collections.iter().map(|(_, i)| i.len()).sum();
        if collections > limits.state_items || items > limits.state_items {
            return Err(format!(
                "{file}: mock {} holds {items} stateful item(s) in {collections} collection(s): at most {}",
                m.name, limits.state_items
            ));
        }
    }
    Ok(cp)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max).collect::<String>())
    }
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

/// A refusal: status, message, and the extra fields of the answer (a
/// repository's unverified commit: `reason`, `commit`, `why`).
#[derive(Debug)]
pub struct Refused(pub StatusCode, pub String, pub Value);

impl Refused {
    fn new(status: StatusCode, m: impl Into<String>) -> Self {
        Self(status, m.into(), Value::Null)
    }

    /// The answer's body: the message and the extra fields.
    pub fn body(&self) -> Value {
        let mut body = json!({ "error": self.1 });
        if let (Value::Object(b), Value::Object(extra)) = (&mut body, &self.2) {
            for (k, v) in extra {
                b.insert(k.clone(), v.clone());
            }
        }
        body
    }
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        (self.0, Json(self.body())).into_response()
    }
}

impl From<ServiceError> for Refused {
    fn from(e: ServiceError) -> Self {
        Self(e.status, e.message(), Value::Null)
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

    /// The names of the checkpoints of `project`.
    pub fn names_in(&self, project: &str) -> Vec<String> {
        self.lock()
            .values()
            .filter(|i| i.cp.project.as_deref() == Some(project))
            .map(|i| i.cp.name.clone())
            .collect()
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
    save_in(state, name, replace, None)
}

/// Does checkpoint `cp` cover the mock: an API mock of the instance, of
/// the checkpoint's project when it is a project's?
fn in_scope(project: Option<&str>, s: &Snapshot) -> bool {
    covered(s) && project.is_none_or(|p| s.project.as_deref() == Some(p))
}

/// Save under `name` the mocks of `project` (`None`: every API mock of the
/// instance) and the health toggle.
pub fn save_in(
    state: &AppState,
    name: &str,
    replace: bool,
    project: Option<String>,
) -> Result<Value, Refused> {
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
        .filter(|s| in_scope(project.as_deref(), s))
        .map(kept)
        .collect();
    let cp = Checkpoint {
        name: name.to_string(),
        saved_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        healthy: state.health.is_healthy(),
        mocks,
        project,
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
    restore_checkpoint(state, &cp, dry_run)
}

/// Restore `cp`, a local checkpoint or one read from the repository (and
/// checked by [`parse_untrusted`]). Runs on the blocking pool.
pub fn restore_checkpoint(
    state: &AppState,
    cp: &Checkpoint,
    dry_run: bool,
) -> Result<Value, Refused> {
    let name = cp.name.as_str();
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
    let scope = cp.project.as_deref();
    // Where a mock the restore makes goes: the project of a project's
    // checkpoint, else the project it had when it was kept.
    let home = |m: &KeptMock| cp.project.clone().or_else(|| m.project.clone());
    for m in &cp.mocks {
        let existing = current.get(&m.name);
        if let Some(s) = existing.filter(|s| covered(s) && !in_scope(scope, s)) {
            problems.push(format!(
                "{}: now in {}, left as it is",
                m.name,
                // Never the other project's name: the caller may not see it.
                match &s.project {
                    Some(_) => "another project".to_string(),
                    None => "the instance's mocks".into(),
                }
            ));
            continue;
        }
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
        let want_project = home(m);
        match &now {
            Some(k)
                if k.live == m.live
                    && k.draft == m.draft
                    && k.enabled == m.enabled
                    && k.state == m.state
                    && k.project == want_project =>
            {
                unchanged.push(m.name.clone());
                continue;
            }
            Some(k) if k.live == m.live && k.draft == m.draft && k.enabled == m.enabled => {
                if k.state != m.state {
                    data.push(m.name.clone());
                }
                if k.project != want_project {
                    replaced.push(m.name.clone());
                }
                if !dry_run {
                    if let Some(s) = existing {
                        s.state.restore(&m.state);
                    }
                    if k.project != want_project {
                        let _ = store.set_project(&m.name, want_project.clone());
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
                let _ = store.set_project_quietly(&m.name, want_project.clone());
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
        if in_scope(scope, s) && !cp.mocks.iter().any(|m| &m.name == n) {
            if !dry_run {
                store.delete(n);
            }
            deleted.push(n.clone());
        }
    }
    let health_before = state.health.is_healthy();
    // A project's checkpoint changes that project's mocks only: never the
    // instance's health toggle nor its flaky counters.
    let healthy_after = if scope.is_some() {
        health_before
    } else {
        cp.healthy
    };
    let flaky = if scope.is_some() {
        0
    } else if dry_run {
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
            || health_before != healthy_after;
    Ok(json!({
        "checkpoint": name,
        "project": cp.project,
        "dry_run": dry_run,
        "changed": changed,
        "mocks": {
            "created": created,
            "replaced": replaced,
            "data_restored": data,
            "unchanged": unchanged,
            "deleted": deleted,
        },
        "health": { "before": if health_before { "healthy" } else { "unhealthy" }, "after": if healthy_after { "healthy" } else { "unhealthy" } },

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
    /// The project whose mocks it keeps (absent: the instance's, or the
    /// one project the caller works in).
    #[serde(default)]
    pub project: Option<String>,
}

/// Where a checkpoint is read from: this instance's store, or the commit
/// `HEAD` of the configuration repository's working copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Instance,
    Git,
}

impl Source {
    pub fn parse(raw: Option<&str>) -> Result<Self, Refused> {
        match raw {
            None | Some("") | Some("instance") => Ok(Self::Instance),
            Some("git") => Ok(Self::Git),
            Some(other) => Err(Refused::new(
                StatusCode::BAD_REQUEST,
                format!("from {other:?}: instance (the default) or git"),
            )),
        }
    }
}

#[derive(Deserialize)]
struct FromQuery {
    #[serde(default)]
    from: Option<String>,
    /// `?project=`: one project's checkpoints (`instance`: the instance's).
    #[serde(default)]
    project: Option<String>,
}

impl From<crate::sourcecontrol::GitError> for Refused {
    fn from(e: crate::sourcecontrol::GitError) -> Self {
        Self(e.status, e.message, e.extra)
    }
}

/// The repository's checkpoints at `HEAD` (admin): each summarised when it
/// parses and passes the checks, else listed with the reason.
pub async fn list_from_git(state: &AppState) -> Result<Value, Refused> {
    let listing = state.git.repository_checkpoints().await?;
    let limits = state.mocks.limits();
    let max = state.git.max_checkpoint_bytes();
    tokio::task::spawn_blocking(move || {
        let items: Vec<Value> = listing
            .entries
            .into_iter()
            .map(|(e, read)| {
                match read.and_then(|b| parse_untrusted(&b, &e.name, &limits).map(|c| (c, b.len())))
                {
                    Ok((cp, n)) => {
                        let mut s = cp.summary(n);
                        s["ok"] = json!(true);
                        s
                    }
                    Err(error) => {
                        json!({ "name": e.name, "bytes": e.bytes, "ok": false, "error": error })
                    }
                }
            })
            .collect();
        json!({
            "from": "git",
            "path": listing.path,
            "head": listing.head,
            "checkpoints": items,
            "truncated": listing.truncated,
            "max_checkpoints": MAX_CHECKPOINTS,
            "max_checkpoint_bytes": max,
        })
    })
    .await
    .map_err(|_| {
        Refused::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the listing did not finish",
        )
    })
}

/// Restore a checkpoint of the repository (`HEAD`): read whole within the
/// cap, then parsed, checked ([`parse_untrusted`]) and restored on the
/// service's compile pool, like a local one.
pub async fn restore_from_git(svc: Svc, name: String, dry_run: bool) -> Result<Value, Refused> {
    svc.need(crate::rbac::Permission::CheckpointsRestore)?;
    svc.need(crate::rbac::Permission::GitRead)?;
    let (bytes, head) = svc.state.git.repository_checkpoint(&name).await?;
    let limits = svc.state.mocks.limits();
    svc.compiling(move |s| {
        let mut cp = parse_untrusted(&bytes, &name, &limits)
            .map_err(|e| ServiceError::new(StatusCode::UNPROCESSABLE_ENTITY, e))?;
        drop(bytes);
        // A checkpoint from the repository never moves a mock between
        // projects (whoever pushes would choose who edits it): each mock
        // keeps the project it has, a new one goes to the checkpoint's.
        for m in &mut cp.mocks {
            m.project = s.state.mocks.get(&m.name).and_then(|x| x.project);
        }
        let mut v =
            restore_checkpoint(&s.state, &cp, dry_run).map_err(|r| ServiceError::new(r.0, r.1))?;
        v["from"] = json!("git");
        v["head"] = head.unwrap_or(Value::Null);
        Ok(v)
    })
    .await
    .map_err(Refused::from)
}

async fn list_handler(
    State(state): State<AppState>,
    Query(q): Query<FromQuery>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
) -> Response {
    match Source::parse(q.from.as_deref()) {
        Err(e) => return e.into_response(),
        Ok(Source::Git) => {
            // The repository is source control's: `git.read`, as its routes.
            if let Err(r) = crate::rbac::require(
                &crate::control_auth::caller_of(&state, &headers, &extensions),
                crate::rbac::Permission::GitRead,
            ) {
                return r;
            }
            return match list_from_git(&state).await {
                Ok(v) => Json(v).into_response(),
                Err(e) => e.into_response(),
            };
        }
        Ok(Source::Instance) => {}
    }
    let filter = match crate::rbac::projects::Filter::parse(q.project.as_deref()) {
        Ok(f) => f,
        Err(e) => return Refused::new(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    Json(list_for(&state, &caller, &filter)).into_response()
}

// ── Who may see and change which checkpoint (step 11) ───────────────

/// A checkpoint of `project` is seen by who reads mocks there (`None`: the
/// instance's, instance-wide `mocks.read`).
fn visible(caller: &crate::rbac::Resolved, project: Option<&str>) -> bool {
    caller.has_in(crate::rbac::Permission::MocksRead, project)
}

fn not_found(name: &str) -> ServiceError {
    ServiceError::not_found(format!("no checkpoint named {name:?}"))
}

/// The checkpoints the caller may see, optionally of one project
/// (`GET /_rustybin/checkpoints`, `list_checkpoints`).
pub fn list_for(
    state: &AppState,
    caller: &crate::rbac::Resolved,
    filter: &Option<crate::rbac::projects::Filter>,
) -> Value {
    let list: Vec<Value> = state
        .checkpoints
        .list()
        .into_iter()
        .filter(|c| {
            let p = c["project"].as_str();
            visible(caller, p) && crate::rbac::projects::passes(filter, p)
        })
        .collect();
    json!({
        "checkpoints": list,
        "max_checkpoints": MAX_CHECKPOINTS,
        "scope": "the instance's API mocks (deployed revision, draft, on or off, stateful data) and the health toggle; a restore also resets the flaky counters. A project's checkpoint holds and restores that project's mocks only. Never visitors' or keys' mocks, file mocks, settings or secrets.",
    })
}

/// One checkpoint's summary, `404` when the caller may not see it.
pub fn summary_for(
    state: &AppState,
    caller: &crate::rbac::Resolved,
    name: &str,
) -> Result<Value, ServiceError> {
    match state.checkpoints.get(name) {
        Some(cp) if visible(caller, cp.project.as_deref()) => state
            .checkpoints
            .summary(name)
            .ok_or_else(|| not_found(name)),
        _ => Err(not_found(name)),
    }
}

/// Save a checkpoint for the caller: of the project named (or the one the
/// caller works in, see `rbac::projects::target`), `checkpoints.write`
/// there; replacing one needs the same on the one replaced. Blocking.
pub fn save_for(
    state: &AppState,
    caller: &crate::rbac::Resolved,
    name: &str,
    replace: bool,
    project: Option<&str>,
) -> Result<Value, ServiceError> {
    crate::audit::target("checkpoint", name);
    let write = crate::rbac::Permission::CheckpointsWrite;
    let project = crate::rbac::projects::target(&state.users, caller, write, project)?;
    if let Some(old) = state.checkpoints.get(name) {
        if !visible(caller, old.project.as_deref()) {
            return Err(ServiceError::new(
                StatusCode::CONFLICT,
                format!("the name {name:?} is taken: pick another"),
            ));
        }
        if replace && !caller.has_in(write, old.project.as_deref()) {
            return Err(ServiceError::permission(caller, write));
        }
    }
    save_in(state, name, replace, project).map_err(|r| ServiceError::new(r.0, r.1))
}

/// Delete a checkpoint (`checkpoints.write` in its project).
pub fn delete_for(
    state: &AppState,
    caller: &crate::rbac::Resolved,
    name: &str,
) -> Result<Value, ServiceError> {
    let Some(cp) = state
        .checkpoints
        .get(name)
        .filter(|cp| visible(caller, cp.project.as_deref()))
    else {
        return Err(not_found(name));
    };
    let write = crate::rbac::Permission::CheckpointsWrite;
    if !caller.has_in(write, cp.project.as_deref()) {
        return Err(ServiceError::permission(caller, write));
    }
    if state.checkpoints.delete(name) {
        Ok(json!({ "deleted": name }))
    } else {
        Err(not_found(name))
    }
}

/// Move the checkpoints of `from` to `to` (a project deleted with
/// `move_to`). Answers the names moved.
pub fn move_project(state: &AppState, from: &str, to: Option<&str>) -> Vec<String> {
    let mut moved = Vec::new();
    for name in state.checkpoints.names_in(from) {
        if let Some(cp) = state.checkpoints.get(&name) {
            let mut cp = (*cp).clone();
            cp.project = to.map(str::to_string);
            for m in &mut cp.mocks {
                if m.project.as_deref() == Some(from) {
                    m.project = to.map(str::to_string);
                }
            }
            if state.checkpoints.put(cp, true).is_ok() {
                moved.push(name);
            }
        }
    }
    moved
}

async fn save_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    body: Bytes,
) -> Response {
    let body: SaveBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            return Refused::new(StatusCode::BAD_REQUEST, format!("invalid body: {e}"))
                .into_response()
        }
    };
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    let s = state.clone();
    let saved = tokio::task::spawn_blocking(move || {
        save_for(
            &s,
            &caller,
            &body.name,
            body.replace,
            body.project.as_deref(),
        )
    })
    .await;
    match saved {
        Ok(Ok(v)) => (StatusCode::CREATED, Json(v)).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => Refused::new(StatusCode::INTERNAL_SERVER_ERROR, "the save did not finish")
            .into_response(),
    }
}

async fn get_handler(
    State(state): State<AppState>,
    UrlPath(name): UrlPath<String>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    match summary_for(&state, &caller, &name) {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn delete_handler(
    State(state): State<AppState>,
    UrlPath(name): UrlPath<String>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    match delete_for(&state, &caller, &name) {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
struct RestoreQuery {
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    from: Option<String>,
}

/// Restore through the service's compile pool (specs are compiled):
/// `checkpoints.restore` in the checkpoint's project (`404` when the caller
/// may not see it).
pub async fn restore_with(svc: Svc, name: String, dry_run: bool) -> Result<Value, Refused> {
    let caller = svc.caller.resolved.clone();
    let project = match svc.state.checkpoints.get(&name) {
        Some(cp) if visible(&caller, cp.project.as_deref()) => cp.project.clone(),
        _ => return Err(Refused::from(not_found(&name))),
    };
    let restore_p = crate::rbac::Permission::CheckpointsRestore;
    if !caller.has_in(restore_p, project.as_deref()) {
        return Err(Refused::from(ServiceError::permission(&caller, restore_p)));
    }
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
    let source = match Source::parse(q.from.as_deref()) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let svc = Svc::from_request(&state, &headers, &extensions, &uri);
    let result = match source {
        Source::Instance => restore_with(svc, name, q.dry_run).await,
        Source::Git => restore_from_git(svc, name, q.dry_run).await,
    };
    match result {
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
            "Checkpoints of the instance's API mocks and health: list them (?from=git: the configuration repository's, admin), or save one (admin)",
        )
.permission_for(&["GET"], crate::rbac::Permission::MocksRead).permission_for(&["POST"], crate::rbac::Permission::CheckpointsWrite).project_scoped()
        .example(Example::get("Checkpoints", LIST_PATH))
        .example(
            Example::get("The repository's checkpoints", "/_rustybin/checkpoints?from=git")
                .expect_status(409),
        ),
        Endpoint::new(
            ONE_PATH,
            &["GET", "DELETE"],
            category::CONTROL,
            "A checkpoint: what it holds; DELETE removes it (admin)",
        )
.permission_for(&["GET"], crate::rbac::Permission::MocksRead).permission_for(&["DELETE"], crate::rbac::Permission::CheckpointsWrite).project_scoped()
        .example(Example::get("A checkpoint", "/_rustybin/checkpoints/demo-start").expect_status(404)),
        Endpoint::new(
            RESTORE_PATH,
            &["POST"],
            category::CONTROL,
            "Restore a checkpoint: mocks, their data and health back, flaky counters reset; ?dry_run=true says what would change, ?from=git restores the configuration repository's (admin)",
        )
.permission(crate::rbac::Permission::CheckpointsRestore).project_scoped()
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
                "description": "This instance's checkpoints; from=git lists the configuration repository's instead (the files under <git.path>/checkpoints/ at the working copy's HEAD, after a pull; admin), each read within the 16 MiB cap and checked, or listed with the reason it cannot be restored.",
                "parameters": [{ "name": "from", "in": "query", "required": false, "schema": { "type": "string", "enum": ["instance", "git"], "default": "instance" } },
                    { "name": "project", "in": "query", "required": false, "description": "One project's checkpoints (instance: the instance's)", "schema": { "type": "string" } }],
                "responses": { "200": { "description": "The checkpoints and their scope", "content": obj }, "409": { "description": "from=git: source control is not configured or connected, or (with git.verify on) the working copy's head is not verified: {error, reason: unverified, commit, why}" } }
            },
            "post": {
                "tags": ["Control Plane"], "summary": "Save a checkpoint (admin)", "operationId": "saveCheckpoint",
                "description": "Saves the instance's API mocks (deployed revision, a draft that differs, on or off, stateful data) and the health toggle. Never visitors' or keys' mocks, file mocks, settings or secrets. At most 20.",
                "requestBody": { "required": true, "content": { "application/json": { "schema": {
                    "type": "object", "required": ["name"], "additionalProperties": false,
                    "properties": { "name": { "type": "string" }, "replace": { "type": "boolean" }, "project": { "type": "string", "description": "Keep this project's mocks only (a restore changes only those, never the health toggle); absent: the instance's API mocks, or the one project the caller works in" } }

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
            "description": "Brings the instance's API mocks back as saved (created, replaced, data restored, unchanged; mocks the checkpoint does not hold deleted), sets the health toggle and resets the flaky counters. dry_run answers the same without changing anything. from=git restores the configuration repository's checkpoint of that name (at the working copy's HEAD): untrusted input, read within the size cap, checked and compiled like a local one.",
            "parameters": [name, { "name": "dry_run", "in": "query", "required": false, "schema": { "type": "boolean" } },
                { "name": "from", "in": "query", "required": false, "schema": { "type": "string", "enum": ["instance", "git"], "default": "instance" } }],
            "responses": { "200": { "description": "What changed", "content": obj }, "404": { "description": "No such checkpoint" },
                "413": { "description": "from=git: the file is larger than a checkpoint may be" }, "422": { "description": "from=git: the file is not a valid checkpoint" } }
        } }
    })
}

#[cfg(test)]
mod tests;
