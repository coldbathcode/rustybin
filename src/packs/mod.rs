//! Customer demo packs: everything one customer's demo needs (mocks, their
//! portal items, saved tests and their schedules, storyboards, a console
//! title) as one document, applied as a unit and torn down as a unit.
//!
//! A pack is a `rustybin.yaml` document with a `pack:` header and only the
//! sections a demo needs. Applying it creates a project named after the
//! pack and applies the document into it (`configdoc::apply`, the same
//! code as any document), so every resource it creates belongs to that
//! project and a teardown removes what the project holds, never something
//! found by its name. What a project does not hold (the pack's test
//! schedules, the console title it set) is in a small record
//! (`<state>/packs.json`, [`Packs`]). A name that is taken is refused,
//! never overwritten, at the preview as at the apply.

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::configdoc::{self, ApplyOptions, ProjectDoc, SecretsMode};
use crate::mocks::config_file::MocksFile;
use crate::mocks::service::ServiceError;
use crate::rbac::{Permission as Perm, Resolved};
use crate::state::AppState;

/// Most packs applied at once.
pub const MAX_PACKS: usize = 20;
/// The sections a pack may carry, beside its `pack:` header.
pub const SECTIONS: &[&str] = &["mocks", "portal", "tests", "test_schedules", "storyboards"];
const TITLE_KEY: &str = "console.title";
const MAX_NAME: usize = 64;
const MAX_TITLE: usize = 200;
const MAX_NOTES: usize = 2000;
const FILE: &str = "packs.json";

/// The `pack:` header of a pack document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    /// The pack's name, and its project's.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// The console's title while the pack is applied (restored by the
    /// teardown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console_title: Option<String>,
}

impl Header {
    fn validate(&self) -> Result<(), String> {
        let ok = !self.name.is_empty()
            && self.name.len() <= MAX_NAME
            && self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !ok {
            return Err(format!(
                "pack.name {:?}: 1 to {MAX_NAME} letters, digits, . _ -",
                self.name
            ));
        }
        crate::rbac::projects::check_name(&self.name).map_err(|e| format!("pack.name: {e}"))?;
        for (what, v, max) in [
            ("title", &self.title, MAX_TITLE),
            ("customer", &self.customer, MAX_TITLE),
            ("notes", &self.notes, MAX_NOTES),
            ("console_title", &self.console_title, MAX_TITLE),
        ] {
            if v.as_deref().is_some_and(|t| t.len() > max) {
                return Err(format!("pack.{what}: at most {max} bytes"));
            }
        }
        if self
            .console_title
            .as_deref()
            .is_some_and(|t| t.trim().is_empty())
        {
            return Err("pack.console_title: not empty (leave it out to keep the title)".into());
        }
        Ok(())
    }
}

/// The console title a pack set, and what was stored before it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct TitleChange {
    set: String,
    /// The stored value before (absent: none was stored, the teardown
    /// unsets it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<Value>,
}

/// An applied pack: its header and what its project does not hold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Record {
    #[serde(flatten)]
    header: Header,
    applied_at: String,
    /// Who applied it: never a credential.
    applied_by: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    schedules: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title_change: Option<TitleChange>,
}

#[derive(Serialize, Deserialize)]
struct Doc {
    version: u32,
    packs: Vec<Record>,
}

/// The applied packs of an instance (`AppState.packs`).
pub struct Packs {
    path: Option<PathBuf>,
    items: Mutex<BTreeMap<String, Record>>,
    /// One apply or teardown at a time.
    busy: tokio::sync::Mutex<()>,
}

impl Packs {
    /// The records, loaded from `<state>/packs.json` when a state directory
    /// is set.
    pub fn new(state_dir: Option<&std::path::Path>) -> Self {
        let path = state_dir.map(|d| d.join(FILE));
        let mut items = BTreeMap::new();
        if let Some(p) = &path {
            match std::fs::read(p) {
                Ok(bytes) => match serde_json::from_slice::<Doc>(&bytes) {
                    Ok(doc) => {
                        for r in doc.packs.into_iter().take(MAX_PACKS) {
                            if r.header.validate().is_ok() {
                                items.insert(r.header.name.clone(), r);
                            }
                        }
                    }
                    Err(e) => tracing::warn!(
                        "{}: not a packs document ({e}): starting empty",
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
            busy: tokio::sync::Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Record>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn get(&self, name: &str) -> Option<Record> {
        self.lock().get(name).cloned()
    }

    fn list(&self) -> Vec<Record> {
        self.lock().values().cloned().collect()
    }

    /// Make one change; the file is written before the store holds it.
    fn change(&self, change: impl FnOnce(&mut BTreeMap<String, Record>)) -> Result<(), String> {
        let mut next = self.lock().clone();
        change(&mut next);
        if let Some(path) = &self.path {
            let doc = Doc {
                version: 1,
                packs: next.values().cloned().collect(),
            };
            let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
            crate::mocks::store::off_runtime(|| crate::settings::write_private(path, &bytes))?;
        }
        *self.lock() = next;
        Ok(())
    }
}

// ── A pack document ─────────────────────────────────────────────────

/// A parsed pack: the header and the document without it.
pub struct Pack {
    pub header: Header,
    pub file: MocksFile,
}

fn bad(message: impl Into<String>) -> ServiceError {
    ServiceError::bad_request(message)
}

/// Parse a pack: YAML or JSON text, or a JSON object. The header is
/// required; any section a demo does not need is refused by name, as are
/// spec files (a pack sent to an instance carries its specs inline) and a
/// resource that names another project.
pub fn parse(document: &Value) -> Result<Pack, ServiceError> {
    let value: Value = match document {
        Value::String(text) => {
            serde_yaml::from_str(text).map_err(|e| bad(format!("not YAML or JSON: {e}")))?
        }
        Value::Object(_) => document.clone(),
        _ => return Err(bad("a pack is YAML or JSON text, or a JSON object")),
    };
    let Value::Object(mut top) = value else {
        return Err(bad("a pack is a document with a pack: header"));
    };
    let header: Header = match top.remove("pack") {
        Some(h) => serde_json::from_value(h).map_err(|e| bad(format!("pack: {e}")))?,
        None => {
            return Err(bad(
                "no pack: header (name, title, customer, notes): a document without one is applied with POST /_rustybin/config/apply",
            ))
        }
    };
    header.validate().map_err(bad)?;
    let refused: Vec<&str> = top
        .keys()
        .map(String::as_str)
        .filter(|k| *k != "version" && !SECTIONS.contains(k))
        .collect();
    if !refused.is_empty() {
        return Err(bad(format!(
            "a pack carries {} only: not {}",
            SECTIONS.join(", "),
            refused.join(", ")
        )));
    }
    top.entry("version")
        .or_insert(json!(crate::mocks::config_file::CURRENT_VERSION));
    let file = configdoc::parse_document(&Value::Object(top)).map_err(bad)?;
    let name = header.name.as_str();
    let other = |kind: &str, n: &str, p: &Option<String>| {
        match p.as_deref() {
        Some(p) if p != name => Err(bad(format!(
            "{kind}.{n}: project {p:?}: a pack's resources belong to its own project ({name}); leave project out"
        ))),
        _ => Ok(()),
    }
    };
    for m in file.mocks() {
        other("mocks", &m.name, &m.project)?;
        if m.spec.is_some() {
            return Err(bad(format!(
                "mocks.{}: spec is a file path: a pack sent to an instance carries its specs inline (spec_inline); rustybin pack apply does that for a directory",
                m.name
            )));
        }
    }
    for t in file.tests.iter().flatten() {
        other("tests", &t.name, &t.project)?;
    }
    for s in file.storyboards.iter().flatten() {
        other("storyboards", &s.name, &s.project)?;
    }
    let mocks: BTreeSet<&str> = file.mocks().iter().map(|m| m.name.as_str()).collect();
    for i in file.portal.iter().flat_map(|p| &p.items) {
        match crate::portal::ItemId::parse(&i.id) {
            Ok(crate::portal::ItemId::Mock(m)) if mocks.contains(m.as_str()) => {}
            _ => {
                return Err(bad(format!(
                    "portal.items.{}: a pack publishes its own mocks only (mock:<name of a mock of the pack>)",
                    i.id
                )))
            }
        }
    }
    let tests: BTreeSet<&str> = file
        .tests
        .iter()
        .flatten()
        .map(|t| t.name.as_str())
        .collect();
    for s in file.test_schedules.iter().flatten() {
        if s.names.is_empty()
            || s.prefix.is_some()
            || !s.names.iter().all(|n| tests.contains(n.as_str()))
        {
            return Err(bad(format!(
                "test_schedules.{}: a pack's schedule runs the pack's own tests: names them (names:), no prefix",
                s.name
            )));
        }
    }
    Ok(Pack { header, file })
}

/// What a pack would create: by kind, the names.
fn contents(file: &MocksFile) -> Value {
    json!({
        "mocks": file.mocks().iter().map(|m| &m.name).collect::<Vec<_>>(),
        "portal_items": file.portal.iter().flat_map(|p| &p.items).map(|i| &i.id).collect::<Vec<_>>(),
        "tests": file.tests.iter().flatten().map(|t| &t.name).collect::<Vec<_>>(),
        "test_schedules": file.test_schedules.iter().flatten().map(|s| &s.name).collect::<Vec<_>>(),
        "storyboards": file.storyboards.iter().flatten().map(|s| &s.name).collect::<Vec<_>>(),
    })
}

/// The names the pack would take that something already has.
fn collisions(state: &AppState, pack: &Pack) -> Vec<Value> {
    let mut out = Vec::new();
    let mut hit = |kind: &str, name: &str| out.push(json!({ "kind": kind, "name": name }));
    if state.users.project_exists(&pack.header.name) {
        hit("project", &pack.header.name);
    }
    for m in pack.file.mocks() {
        if state.mocks.get(&m.name).is_some() {
            hit("mock", &m.name);
        }
    }
    let published = state.portal.published();
    for i in pack.file.portal.iter().flat_map(|p| &p.items) {
        if published.contains_key(&i.id) {
            hit("portal_item", &i.id);
        }
    }
    for t in pack.file.tests.iter().flatten() {
        if state.saved_tests.get(&t.name).is_some() {
            hit("test", &t.name);
        }
    }
    for s in pack.file.test_schedules.iter().flatten() {
        if state.saved_tests.schedules.get(&s.name).is_some() {
            hit("test_schedule", &s.name);
        }
    }
    for s in pack.file.storyboards.iter().flatten() {
        if state.storyboards.get(&s.name).is_some() {
            hit("storyboard", &s.name);
        }
    }
    out
}

/// The first permission the caller lacks to apply or tear down a pack
/// (beside the route's `config.apply`): a project is created and deleted
/// (`users.manage`), and a console title is a settings change.
fn need(state: &AppState, caller: &Resolved, title: bool) -> Result<(), ServiceError> {
    let mut needs = vec![Perm::ConfigApply, Perm::UsersManage];
    if title {
        needs.push(Perm::SettingsWrite);
        needs.extend(crate::settings::extra_permissions(TITLE_KEY, false));
    }
    let _ = state;
    match needs.into_iter().find(|p| !caller.has(*p)) {
        Some(p) => Err(ServiceError::permission(caller, p)),
        None => Ok(()),
    }
}

/// How an apply is made.
pub struct Applier {
    pub caller: Arc<Resolved>,
    /// For the audit lines and the portal's `by`: never a credential.
    pub principal: String,
    pub base_url: Option<String>,
}

/// The document handed to `configdoc::apply`: the pack's sections with
/// every resource in the pack's project, which the document creates.
fn into_project(pack: &Pack) -> Result<MocksFile, ServiceError> {
    // A copy through JSON: `MocksFile` is a document, not `Clone`.
    let mut file: MocksFile = serde_json::to_value(&pack.file)
        .and_then(serde_json::from_value)
        .map_err(|e| ServiceError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let name = &pack.header.name;
    let h = &pack.header;
    file.projects = Some(vec![ProjectDoc {
        name: name.clone(),
        description: match (&h.title, &h.customer) {
            (Some(t), Some(c)) => format!("Demo pack: {t} ({c})"),
            (Some(t), None) => format!("Demo pack: {t}"),
            (None, Some(c)) => format!("Demo pack for {c}"),
            (None, None) => "Demo pack".to_string(),
        },
        members: Vec::new(),
    }]);
    for m in file.mocks.iter_mut().flatten() {
        m.project = Some(name.clone());
    }
    for t in file.tests.iter_mut().flatten() {
        t.project = Some(name.clone());
    }
    for s in file.storyboards.iter_mut().flatten() {
        s.project = Some(name.clone());
    }
    Ok(file)
}

fn options(by: &Applier, dry_run: bool, untrusted: bool) -> ApplyOptions {
    ApplyOptions {
        prune: false,
        dry_run,
        principal: by.principal.clone(),
        base: None,
        untrusted,
        caller: Some(by.caller.clone()),
        base_url: by.base_url.clone(),
    }
}

fn problems_of(report: &Value) -> Vec<String> {
    report["problems"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn unavailable(e: String) -> ServiceError {
    ServiceError::retry(StatusCode::SERVICE_UNAVAILABLE, e, 2)
}

/// Check a pack against the instance as the apply would: the names it
/// would take, the caps, and the document itself compiled and validated
/// by a dry run of the apply. `Ok` is the preview.
async fn preview(
    state: &AppState,
    by: &Applier,
    pack: &Pack,
    untrusted: bool,
) -> Result<Value, ServiceError> {
    need(state, &by.caller, pack.header.console_title.is_some())?;
    let taken = collisions(state, pack);
    if !taken.is_empty() {
        let names: Vec<String> = taken
            .iter()
            .map(|c| {
                format!(
                    "{} {}",
                    c["kind"].as_str().unwrap_or(""),
                    c["name"].as_str().unwrap_or("")
                )
            })
            .collect();
        return Err(ServiceError::with_body(
            StatusCode::CONFLICT,
            json!({
                "error": format!(
                    "the pack {} names what already exists ({}): nothing is overwritten. Tear the earlier pack down, or rename",
                    pack.header.name,
                    names.join(", ")
                ),
                "reason": "name_taken",
                "collisions": taken,
            }),
        ));
    }
    if state.packs.len() >= MAX_PACKS {
        return Err(ServiceError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            format!("at most {MAX_PACKS} packs applied at once: tear one down first"),
        ));
    }
    if pack.header.console_title.is_some() && !state.settings.writable(TITLE_KEY) {
        return Err(ServiceError::new(
            StatusCode::CONFLICT,
            "pack.console_title: the console title is locked on this instance (RUSTYBIN_SETTINGS_LOCK): leave it out",
        ));
    }
    // The portal items name mocks the dry run has not created: their
    // metadata is checked here, the rest by the apply's own dry run.
    let mut file = into_project(pack)?;
    let (_, mut problems) = crate::portal::file_items(file.portal.take());
    let report = configdoc::apply_on_pool(state, file, options(by, true, untrusted))
        .await
        .map_err(unavailable)?;
    problems.extend(problems_of(&report));
    if !problems.is_empty() {
        return Err(ServiceError::with_body(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({
                "error": format!("the pack {} cannot be applied: {}", pack.header.name, problems.join("; ")),
                "reason": "invalid_pack",
                "problems": problems,
            }),
        ));
    }
    Ok(json!({
        "ok": true,
        "dry_run": true,
        "pack": pack.header,
        "project": pack.header.name,
        "would_create": contents(&pack.file),
        "console_title": pack.header.console_title,
    }))
}

/// Apply a pack (`dry_run`: the preview only, which validates like the
/// apply). The apply is a unit: a document that turns out to have a
/// problem is torn down again and refused.
pub async fn apply(
    state: &AppState,
    by: Applier,
    document: &Value,
    dry_run: bool,
    untrusted: bool,
) -> Result<Value, ServiceError> {
    let pack = parse(document)?;
    crate::audit::target("pack", &pack.header.name);
    let _one = state.packs.busy.lock().await;
    let mut view = preview(state, &by, &pack, untrusted).await?;
    if dry_run {
        return Ok(view);
    }
    let name = pack.header.name.clone();
    let report =
        configdoc::apply_on_pool(state, into_project(&pack)?, options(&by, false, untrusted))
            .await
            .map_err(unavailable)?;
    let mut problems = problems_of(&report);
    // The title last: the pack's own change of a setting, remembered.
    let mut title_change = None;
    if let (true, Some(title)) = (problems.is_empty(), &pack.header.console_title) {
        let (before, source) = state.settings.get(TITLE_KEY);
        let values = BTreeMap::from([(TITLE_KEY.to_string(), json!(title))]);
        match state
            .settings
            .update(values, BTreeMap::new(), &by.principal)
        {
            Ok(_) => {
                title_change = Some(TitleChange {
                    set: title.clone(),
                    previous: (source == crate::settings::Source::Stored).then_some(before),
                })
            }
            Err(_) => problems.push("pack.console_title: the title could not be set".to_string()),
        }
    }
    let record = Record {
        header: pack.header.clone(),
        applied_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        applied_by: by.principal.clone(),
        schedules: pack
            .file
            .test_schedules
            .iter()
            .flatten()
            .map(|s| s.name.clone())
            .collect(),
        title_change,
    };
    if problems.is_empty() {
        if let Err(e) = state.packs.change(|m| {
            m.insert(name.clone(), record.clone());
        }) {
            problems.push(format!("the pack's record could not be written ({e})"));
        }
    }
    if !problems.is_empty() {
        // Not a unit any more: what was created is removed again.
        let _ = remove(state, &record, &by.principal);
        return Err(ServiceError::with_body(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({
                "error": format!("the pack {name} was not applied (what it had created is removed again): {}", problems.join("; ")),
                "reason": "invalid_pack",
                "problems": problems,
            }),
        ));
    }
    tracing::info!(pack = %name, principal = %by.principal, "demo pack applied");
    if let Value::Object(m) = &mut view {
        m.insert("dry_run".into(), json!(false));
        if let Some(c) = m.remove("would_create") {
            m.insert("created".into(), c);
        }
        m.insert("applied_at".into(), json!(record.applied_at));
    }
    Ok(view)
}

/// What a teardown removes: what the pack's project holds now (a resource
/// a person moved out of it stays), the pack's schedules, its title.
fn held(state: &AppState, record: &Record) -> Value {
    let name = &record.header.name;
    let r = crate::rbac::project_api::resources(state, name);
    let published = state.portal.published();
    let portal: Vec<String> = r
        .mocks
        .iter()
        .map(|m| format!("mock:{m}"))
        .filter(|id| published.contains_key(id))
        .collect();
    let schedules: Vec<&String> = record
        .schedules
        .iter()
        .filter(|s| state.saved_tests.schedules.get(s).is_some())
        .collect();
    json!({
        "mocks": r.mocks,
        "portal_items": portal,
        "tests": r.tests,
        "test_schedules": schedules,
        "storyboards": r.storyboards,
        "checkpoints": r.checkpoints,
    })
}

/// Will the teardown put the console title back (it is still the pack's)?
fn title_restores(state: &AppState, record: &Record) -> bool {
    record
        .title_change
        .as_ref()
        .is_some_and(|t| state.settings.get(TITLE_KEY).0 == json!(t.set))
}

/// Remove what the pack holds, its project and its record. Answers what
/// was removed and what could not be.
fn remove(state: &AppState, record: &Record, principal: &str) -> (Value, Vec<String>) {
    let name = &record.header.name;
    let what = held(state, record);
    let names = |k: &str| -> Vec<String> {
        what[k]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    };
    let mut problems = Vec::new();
    for id in names("portal_items") {
        if let Err(e) = state.portal.unpublish(&id) {
            problems.push(format!("portal item {id}: {e}"));
        }
    }
    for s in names("test_schedules") {
        if let Err(e) = state.saved_tests.schedules.delete(&s) {
            problems.push(format!("test schedule {s}: {}", e.message()));
        }
    }
    for s in names("storyboards") {
        if let Err(e) = state.storyboards.delete(&s) {
            problems.push(format!("storyboard {s}: {}", e.message()));
        }
    }
    for t in names("tests") {
        if let Err(e) = state.saved_tests.delete(&t) {
            problems.push(format!("test {t}: {}", e.message()));
        }
    }
    for c in names("checkpoints") {
        state.checkpoints.delete(&c);
    }
    for m in names("mocks") {
        state.mocks.delete(&m);
    }
    let mut title = "unchanged";
    if let Some(t) = &record.title_change {
        if title_restores(state, record) {
            let done = match &t.previous {
                Some(v) => state
                    .settings
                    .update(
                        BTreeMap::from([(TITLE_KEY.to_string(), v.clone())]),
                        BTreeMap::new(),
                        principal,
                    )
                    .is_ok(),
                None => state.settings.unset(TITLE_KEY, principal).is_ok(),
            };
            if done {
                title = "restored";
            } else {
                problems.push("the console title could not be restored".to_string());
            }
        } else {
            // Somebody changed it since: theirs stays.
            title = "kept (changed since the pack set it)";
        }
    }
    let still = crate::rbac::project_api::resources(state, name);
    if still.is_empty() {
        if state.users.project_exists(name) {
            if let Err(e) = state.users.delete_project(name) {
                problems.push(format!("project {name}: {e}"));
            }
        }
    } else {
        problems.push(format!("project {name}: still holds resources, kept"));
    }
    if problems.is_empty() {
        if let Err(e) = state.packs.change(|m| {
            m.remove(name);
        }) {
            problems.push(format!("the pack's record could not be written ({e})"));
        }
    }
    (json!({ "removed": what, "console_title": title }), problems)
}

fn no_pack(name: &str) -> ServiceError {
    ServiceError::not_found(format!(
        "no applied pack named {name:?} (a project that is not a pack is deleted with DELETE /_rustybin/projects/{{name}})"
    ))
}

/// Tear a pack down (`dry_run`: what it would remove).
pub async fn teardown(
    state: &AppState,
    caller: &Resolved,
    principal: &str,
    name: &str,
    dry_run: bool,
) -> Result<Value, ServiceError> {
    crate::audit::target("pack", name);
    let _one = state.packs.busy.lock().await;
    let record = state.packs.get(name).ok_or_else(|| no_pack(name))?;
    need(state, caller, record.title_change.is_some())?;
    if dry_run {
        return Ok(json!({
            "ok": true,
            "dry_run": true,
            "pack": record.header,
            "would_remove": held(state, &record),
            "console_title": if title_restores(state, &record) { "would be restored" } else { "unchanged" },
        }));
    }
    let st = state.clone();
    let who = principal.to_string();
    let (mut v, problems) = tokio::task::spawn_blocking(move || remove(&st, &record, &who))
        .await
        .map_err(|_| {
            ServiceError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the teardown did not finish",
            )
        })?;
    tracing::info!(pack = %crate::logging::printable(name, 64), principal, "demo pack torn down");
    v["ok"] = json!(problems.is_empty());
    v["dry_run"] = json!(false);
    v["pack"] = json!(name);
    v["problems"] = json!(problems);
    Ok(v)
}

fn record_view(state: &AppState, r: &Record) -> Value {
    let mut v = serde_json::to_value(&r.header).unwrap_or(Value::Null);
    v["applied_at"] = json!(r.applied_at);
    v["applied_by"] = json!(r.applied_by);
    v["project"] = json!(r.header.name);
    v["holds"] = held(state, r);
    v
}

/// The applied packs with what each holds now.
pub fn list(state: &AppState) -> Value {
    json!({
        "packs": state.packs.list().iter().map(|r| record_view(state, r)).collect::<Vec<_>>(),
        "max_packs": MAX_PACKS,
        "sections": SECTIONS,
    })
}

/// A project's resources as a pack document (an applied pack's with its
/// own header and schedules): the inverse of an apply. Specs are inline.
pub fn export(state: &AppState, name: &str) -> Result<Value, ServiceError> {
    if !state.users.project_exists(name) {
        return Err(ServiceError::not_found(format!(
            "no project named {name:?} to export as a pack"
        )));
    }
    let record = state.packs.get(name);
    let all = configdoc::export(state, SecretsMode::Refs, false).file;
    let mine = |p: &Option<String>| p.as_deref() == Some(name);
    let mut mocks: Vec<_> = all
        .mocks
        .into_iter()
        .flatten()
        .filter(|m| mine(&m.project))
        .collect();
    let mut tests: Vec<_> = all
        .tests
        .into_iter()
        .flatten()
        .filter(|t| mine(&t.project))
        .collect();
    let mut storyboards: Vec<_> = all
        .storyboards
        .into_iter()
        .flatten()
        .filter(|s| mine(&s.project))
        .collect();
    for m in &mut mocks {
        m.project = None;
    }
    for t in &mut tests {
        t.project = None;
    }
    for s in &mut storyboards {
        s.project = None;
    }
    let mock_ids: BTreeSet<String> = mocks.iter().map(|m| format!("mock:{}", m.name)).collect();
    let items: Vec<_> = all
        .portal
        .into_iter()
        .flat_map(|p| p.items)
        .filter(|i| mock_ids.contains(&i.id))
        .collect();
    let test_names: BTreeSet<&str> = tests.iter().map(|t| t.name.as_str()).collect();
    // A pack's own schedules; of a project built by hand, the ones that
    // run its tests only.
    let schedules: Vec<_> = all
        .test_schedules
        .into_iter()
        .flatten()
        .filter(|s| match &record {
            Some(r) => r.schedules.contains(&s.name),
            None => {
                !s.names.is_empty()
                    && s.prefix.is_none()
                    && s.names.iter().all(|n| test_names.contains(n.as_str()))
            }
        })
        .collect();
    let header = match &record {
        Some(r) => r.header.clone(),
        None => Header {
            name: name.to_string(),
            title: state
                .users
                .project(name)
                .map(|p| p.description)
                .filter(|d| !d.is_empty()),
            ..Header::default()
        },
    };
    let mut doc = serde_json::Map::new();
    doc.insert(
        "version".into(),
        json!(crate::mocks::config_file::CURRENT_VERSION),
    );
    doc.insert("pack".into(), json!(header));
    let mut put = |key: &str, v: Value, empty: bool| {
        if !empty {
            doc.insert(key.to_string(), v);
        }
    };
    put("mocks", json!(mocks), mocks.is_empty());
    put("portal", json!({ "items": items }), items.is_empty());
    put("tests", json!(tests), tests.is_empty());
    put("test_schedules", json!(schedules), schedules.is_empty());
    put("storyboards", json!(storyboards), storyboards.is_empty());
    Ok(Value::Object(doc))
}

/// A pack document as YAML, with a header that says how to use it.
pub fn to_yaml(doc: &Value) -> String {
    format!(
        "# A Rustybin demo pack: apply with rustybin pack apply <file> (--dry-run first),\n\
         # or POST /_rustybin/packs/apply; remove with rustybin pack teardown <name>.\n{}",
        serde_yaml::to_string(doc).unwrap_or_default()
    )
}

// ── Routes ──────────────────────────────────────────────────────────

pub const LIST_PATH: &str = "/_rustybin/packs";
pub const APPLY_PATH: &str = "/_rustybin/packs/apply";
pub const ONE_PATH: &str = "/_rustybin/packs/{name}";
pub const EXPORT_PATH: &str = "/_rustybin/packs/{name}/export";

fn refuse(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

async fn list_handler(State(state): State<AppState>) -> Response {
    Json(list(&state)).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyBody {
    document: Value,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    untrusted: bool,
}

#[derive(Deserialize, Default)]
struct ApplyQuery {
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    untrusted: bool,
}

/// `POST /_rustybin/packs/apply`: `{document, dry_run, untrusted}` as
/// JSON, or the pack itself as the body (`Content-Type: application/yaml`)
/// with `?dry_run=true` and `?untrusted=true`.
async fn apply_handler(
    State(state): State<AppState>,
    Query(q): Query<ApplyQuery>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
    uri: Uri,
    body: Bytes,
) -> Response {
    let yaml = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.contains("yaml"));
    let body = if yaml {
        ApplyBody {
            document: Value::String(String::from_utf8_lossy(&body).into_owned()),
            dry_run: q.dry_run,
            untrusted: q.untrusted,
        }
    } else {
        match serde_json::from_slice::<ApplyBody>(&body) {
            Ok(mut b) => {
                b.dry_run |= q.dry_run;
                b.untrusted |= q.untrusted;
                b
            }
            Err(e) => {
                return refuse(
                    StatusCode::BAD_REQUEST,
                    format!("expected {{document, dry_run, untrusted}}, or the pack as application/yaml: {e}"),
                )
            }
        }
    };
    let by = Applier {
        caller: crate::control_auth::caller_of(&state, &headers, &extensions),
        principal: crate::settings::principal(&headers, &state.config),
        base_url: Some(
            crate::session::request_origin(&headers, &extensions, &uri, &state.config).base_url(),
        ),
    };
    match apply(&state, by, &body.document, body.dry_run, body.untrusted).await {
        Ok(v) if body.dry_run => Json(v).into_response(),
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize, Default)]
struct TeardownQuery {
    #[serde(default)]
    dry_run: bool,
}

async fn teardown_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<TeardownQuery>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    let principal = crate::settings::principal(&headers, &state.config);
    match teardown(&state, &caller, &principal, &name, q.dry_run).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize, Default)]
struct ExportQuery {
    format: Option<String>,
}

async fn export_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<ExportQuery>,
) -> Response {
    let doc = match export(&state, &name) {
        Ok(d) => d,
        Err(e) => return e.into_response(),
    };
    match q.format.as_deref().unwrap_or("yaml") {
        "yaml" => (
            [(header::CONTENT_TYPE, "application/yaml; charset=utf-8")],
            to_yaml(&doc),
        )
            .into_response(),
        "json" => Json(doc).into_response(),
        _ => refuse(StatusCode::BAD_REQUEST, "format must be yaml or json"),
    }
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(LIST_PATH, get(list_handler))
        .route(APPLY_PATH, post(apply_handler))
        .route(ONE_PATH, delete(teardown_handler))
        .route(EXPORT_PATH, get(export_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            LIST_PATH,
            &["GET"],
            category::CONTROL,
            "The demo packs applied to this instance, with what each holds now",
        )
        .permission(Perm::ConfigExport)
        .example(Example::get("Applied packs", LIST_PATH)),
        Endpoint::new(
            APPLY_PATH,
            &["POST"],
            category::CONTROL,
            "Apply a demo pack: a project named after it with its mocks, portal items, saved tests, schedules and storyboards (dry_run previews)",
        )
        .permission(Perm::ConfigApply)
        .description(
            "{document, dry_run, untrusted}, or the pack itself as the body with Content-Type \
             application/yaml (?dry_run=true, ?untrusted=true). A pack is a rustybin.yaml document \
             with a pack: header (name, title, customer, notes, console_title) and the sections \
             mocks, portal, tests, test_schedules and storyboards only. Every resource goes to a new \
             project named after the pack; a name that is taken is refused (409 name_taken), never \
             overwritten; a pack with a problem is refused whole (422). Also needs users.manage (the \
             project), and settings.write with settings.security for a console_title.",
        )
        .example(
            Example::post("Preview a pack", APPLY_PATH)
                .json(r#"{"document": "pack: {name: acme}\nstoryboards:\n  - name: acme-demo\n    steps: [{title: Welcome}]\n", "dry_run": true}"#),
        ),
        Endpoint::new(
            ONE_PATH,
            &["DELETE"],
            category::CONTROL,
            "Tear a demo pack down: what its project holds, its schedules, the console title it set, the project (?dry_run=true lists it)",
        )
        .permission(Perm::ConfigApply)
        .example(Example::delete("Tear a pack down", "/_rustybin/packs/acme").expect_status(404)),
        Endpoint::new(
            EXPORT_PATH,
            &["GET"],
            category::CONTROL,
            "A project's resources as a demo pack (YAML, or ?format=json): the inverse of an apply",
        )
        .permission(Perm::ConfigExport)
        .example(Example::get("Export a project as a pack", "/_rustybin/packs/acme/export").expect_status(404)),
    ]
}

/// The JSON Schema of the `pack:` header.
pub fn header_schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["name"],
        "description": "Only in a demo pack (POST /_rustybin/packs/apply): the pack's name, which is its project's, and who it is for. A pack carries mocks, portal, tests, test_schedules and storyboards only",
        "properties": {
            "name": { "type": "string", "maxLength": MAX_NAME, "pattern": "^[A-Za-z0-9._-]+$" },
            "title": { "type": "string", "maxLength": MAX_TITLE },
            "customer": { "type": "string", "maxLength": MAX_TITLE },
            "notes": { "type": "string", "maxLength": MAX_NOTES },
            "console_title": { "type": "string", "maxLength": MAX_TITLE, "description": "The console's title while the pack is applied; the teardown restores it" }
        }
    })
}

pub fn openapi_paths() -> Value {
    let obj = json!({ "application/json": { "schema": { "type": "object" } } });
    let name =
        json!({ "name": "name", "in": "path", "required": true, "schema": { "type": "string" } });
    let dry = json!({ "name": "dry_run", "in": "query", "required": false, "schema": { "type": "boolean" } });
    json!({
        LIST_PATH: { "get": {
            "tags": ["Control Plane"], "summary": "Applied demo packs", "operationId": "listPacks",
            "responses": { "200": { "description": "The packs: header, applied_at, applied_by, project and what it holds now", "content": obj } }
        } },
        APPLY_PATH: { "post": {
            "tags": ["Control Plane"], "summary": "Apply a demo pack",
            "description": "A pack is a rustybin.yaml document with a pack: header and the sections mocks, portal (the pack's own mocks), tests, test_schedules (of the pack's own tests) and storyboards; anything else is refused by name. The apply creates a project named after the pack and applies the document into it; a name that is taken (the project, a mock, a test, a schedule, a storyboard, a portal item) is refused with the names, never overwritten; a problem refuses the whole pack. dry_run answers what it would create after the same checks. untrusted applies it as a document from a repository is applied. At most 20 packs.",
            "operationId": "applyPack",
            "parameters": [dry, { "name": "untrusted", "in": "query", "required": false, "schema": { "type": "boolean" } }],
            "requestBody": { "required": true, "content": {
                "application/json": { "schema": { "type": "object", "additionalProperties": false, "required": ["document"], "properties": {
                    "document": { "description": "The pack: YAML or JSON text, or a JSON object" },
                    "dry_run": { "type": "boolean" },
                    "untrusted": { "type": "boolean" }
                } } },
                "application/yaml": { "schema": { "type": "string" } }
            } },
            "responses": {
                "200": { "description": "The preview (dry_run)", "content": obj },
                "201": { "description": "Applied: what was created", "content": obj },
                "400": { "description": "Not a pack: no header, a section a pack does not carry, a spec file path" },
                "403": { "description": "A missing permission, named" },
                "409": { "description": "A name is taken (reason name_taken, collisions)" },
                "422": { "description": "The document has problems (reason invalid_pack, problems): nothing is applied" }
            }
        } },
        ONE_PATH: { "delete": {
            "tags": ["Control Plane"], "summary": "Tear a demo pack down",
            "description": "Removes what the pack's project holds now (mocks with their portal items, saved tests, storyboards, checkpoints), the pack's test schedules, the project and the record, and puts the console title back when it is still the pack's. A resource a person moved out of the project stays. dry_run lists it without removing anything.",
            "operationId": "teardownPack",
            "parameters": [name, dry],
            "responses": { "200": { "description": "What was removed", "content": obj }, "404": { "description": "No applied pack of that name" } }
        } },
        EXPORT_PATH: { "get": {
            "tags": ["Control Plane"], "summary": "A project as a demo pack", "operationId": "exportPack",
            "parameters": [name, { "name": "format", "in": "query", "required": false, "schema": { "type": "string", "enum": ["yaml", "json"] } }],
            "responses": { "200": { "description": "The pack document, specs inline", "content": { "application/yaml": { "schema": { "type": "string" } }, "application/json": { "schema": { "type": "object" } } } }, "404": { "description": "No such project" } }
        } }
    })
}
