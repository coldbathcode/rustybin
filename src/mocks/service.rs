//! The mock control-plane service: the logic behind `/_rustybin/mocks*`,
//! shared by the REST handlers ([`super::handlers`]) and the management MCP
//! server (`crate::mcp::manage`), so permissions, redaction, caps and
//! errors are identical whichever way a mock is managed.
//!
//! A [`Svc`] is built per request from the caller ([`Caller`]: admin rights,
//! owned visits and keys, session) and the request's URLs ([`Links`]); its
//! methods take plain inputs and return JSON values or a [`ServiceError`]
//! (an HTTP status plus a JSON body, which the REST layer sends as is and the
//! MCP layer turns into a tool error).

use std::collections::{BTreeMap, HashMap};

use axum::http::{header, Extensions, HeaderMap, HeaderName, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::capture::OpStats;
use super::checks;
use super::compile::{Compiled, RouteMatch};
use super::config_file;
use super::host::{self, HostBase};
use super::model::{
    McpSettings, MockSettings, OperationSettings, ResponseMode, ResponseSettings, Rule,
    SecuritySettings,
};
use super::request;
use super::respond::{self, BodyChoice, RequestView};
use super::schema::{self, Direction};
use super::selfservice::{self as ss, Caller};
use super::spec;
use super::store::{MockStore, Origin, Snapshot, StoreError};
use super::visits::{EmailInput, Owner, VisitError};
use crate::config::Config;
use crate::state::AppState;

/// Names that would collide with fixed control-plane routes.
pub const RESERVED_NAMES: &[&str] = &[
    "import", "export", "apply", "reload", "visit", "visits", "ai",
];

// ── Errors ──────────────────────────────────────────────────────────

/// A refusal or failure: the HTTP status and JSON body the REST API sends.
#[derive(Clone, Debug)]
pub struct ServiceError {
    pub status: StatusCode,
    pub body: Value,
    /// `Retry-After` seconds, when the answer carries one.
    pub retry_after: Option<u64>,
}

impl ServiceError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: json!({ "error": message.into() }),
            retry_after: None,
        }
    }

    pub fn with_body(status: StatusCode, body: Value) -> Self {
        Self {
            status,
            body,
            retry_after: None,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    /// The refusal of a caller without `p` (`403`, or `401` for nobody on
    /// an instance with an admin token), naming the permission.
    pub fn permission(caller: &crate::rbac::Resolved, p: crate::rbac::Permission) -> Self {
        let (status, message) = crate::rbac::need::refusal_of(caller, p);
        Self::with_body(
            status,
            crate::rbac::need::refusal_body(p, message.as_deref()),
        )
    }

    /// The human-readable message (`message`, else `error`).
    pub fn message(&self) -> String {
        self.body
            .get("message")
            .or_else(|| self.body.get("error"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| self.status.to_string())
    }

    /// A refusal with `Retry-After`.
    pub fn retry(status: StatusCode, message: impl Into<String>, secs: u64) -> Self {
        let mut e = Self::new(status, message);
        e.retry_after = Some(secs);
        e
    }

    /// The machine-readable code when the body carries one (`rustybin_*`).
    pub fn code(&self) -> Option<&str> {
        self.body
            .get("error")
            .and_then(Value::as_str)
            .filter(|e| e.starts_with("rustybin_"))
    }
}

impl IntoResponse for ServiceError {
    fn into_response(self) -> Response {
        let mut resp = (self.status, Json(self.body)).into_response();
        if let Some(secs) = self.retry_after {
            resp.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(secs));
        }
        resp
    }
}

impl From<StoreError> for ServiceError {
    fn from(e: StoreError) -> Self {
        let message = e.message();
        match e {
            StoreError::NotFound(_) => Self::new(StatusCode::NOT_FOUND, message),
            StoreError::Conflict(_) => Self::new(StatusCode::CONFLICT, message),
            StoreError::Invalid(_) => Self::new(StatusCode::BAD_REQUEST, message),
            StoreError::Full(_) => Self::new(StatusCode::SERVICE_UNAVAILABLE, message),
            StoreError::OwnerCap(_) => Self::with_body(
                StatusCode::CONFLICT,
                json!({ "error": "rustybin_mock_cap", "message": message }),
            ),
            StoreError::ChecksFailed(report) => Self::with_body(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({ "error": message, "report": report }),
            ),
            StoreError::Stale {
                expected, current, ..
            } => Self::with_body(
                StatusCode::CONFLICT,
                json!({
                    "error": "rustybin_stale_revision",
                    "message": message,
                    "expected_revision": expected,
                    "current_revision": current,
                }),
            ),
        }
    }
}

/// `Result<T, ServiceError>`.
pub type SvcResult<T> = Result<T, ServiceError>;

// ── Request-derived context ─────────────────────────────────────────

/// Where mocks answer, as seen by this request.
#[derive(Clone)]
pub struct Links {
    base: String,
    mocks_host: Option<HostBase>,
    self_service_host: Option<HostBase>,
}

impl Links {
    pub fn from_request(
        headers: &HeaderMap,
        extensions: &Extensions,
        uri: &Uri,
        config: &Config,
    ) -> Self {
        Self {
            base: crate::session::request_origin(headers, extensions, uri, config).base_url(),
            mocks_host: host::base(headers, extensions, uri, config),
            self_service_host: config
                .self_service
                .host
                .as_deref()
                .and_then(|d| host::base_for(d, headers, extensions, uri, config)),
        }
    }

    /// `scheme://host[:port]` of this instance as the caller reached it.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// The host-based URL base of instance mocks (`RUSTYBIN_MOCKS_HOST`).
    pub fn host(&self) -> Option<&HostBase> {
        self.mocks_host.as_ref()
    }

    /// `{base}/mock/{name}`, or `https://{name}.{domain}` for a visitor mock
    /// with a mocks domain.
    pub fn mock_base(&self, owner: &Owner, name: &str) -> String {
        if owner.is_visitor() {
            if let Some(h) = &self.self_service_host {
                return h.url(name, "");
            }
        }
        format!("{}/mock/{name}", self.base)
    }
}

/// The service for one caller (cloned to run more than one call on the
/// compile pool).
#[derive(Clone)]
pub struct Svc {
    pub state: AppState,
    pub caller: Caller,
    pub links: Links,
}

// ── Request bodies (the REST bodies, also the MCP tool arguments) ───

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBody {
    pub name: Option<String>,
    pub spec: Value,
    pub enabled: Option<bool>,
    pub deploy: Option<bool>,
    pub force: Option<bool>,
    pub strip_server_prefix: Option<bool>,
    pub responses: Option<ResponseSettings>,
    pub security: Option<SecuritySettings>,
    pub operations: Option<BTreeMap<String, OperationSettings>>,
    pub mcp: Option<McpSettings>,
    /// The project the mock goes to (step 11): `mocks.write` there; absent:
    /// the instance's, or the one project the caller works in.
    pub project: Option<String>,
}

/// A field that may be absent (`None`), `null` (`Some(None)`) or a value.
fn double_option<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchBody {
    /// Move the mock to this project (`null` or `instance`: the
    /// instance's): `mocks.write` in both.
    #[serde(default, deserialize_with = "double_option")]
    pub project: Option<Option<String>>,
    /// The draft revision the edit was made from (else `If-Match`).
    pub expected_revision: Option<u64>,
    pub name: Option<String>,
    pub spec: Option<Value>,
    pub strip_server_prefix: Option<bool>,
    pub responses: Option<ResponseSettings>,
    pub security: Option<SecuritySettings>,
    pub operations: Option<BTreeMap<String, OperationSettings>>,
    pub mcp: Option<McpSettings>,
    pub deploy: Option<bool>,
    pub force: Option<bool>,
}

impl PatchBody {
    /// Nothing but a move (the project was taken out): no draft change.
    fn is_only_a_move(&self) -> bool {
        self.name.is_none()
            && self.spec.is_none()
            && self.strip_server_prefix.is_none()
            && self.responses.is_none()
            && self.security.is_none()
            && self.operations.is_none()
            && self.mcp.is_none()
            && self.deploy.is_none()
    }
}

#[derive(Deserialize, Default)]
pub struct SampleQuery {
    pub status: Option<u16>,
    pub seed: Option<u64>,
    pub mode: Option<ResponseMode>,
    pub example: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidateBody {
    /// `response` (default) or `request`.
    pub kind: Option<String>,
    pub status: Option<u16>,
    pub body: Value,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RuleTestBody {
    /// Rules to try (default: the operation's rules in the draft).
    pub rules: Option<Vec<Rule>>,
    /// Captured request to try them on (default: the latest call of the
    /// operation).
    pub request: Option<u64>,
}

/// What `create` adds to the detail: whether the mock is the caller's, and
/// the visit it started (manage token shown once).
pub struct Created {
    pub detail: Value,
}

// ── Pure helpers ────────────────────────────────────────────────────

/// Read the spec field of a request: text, or a JSON/YAML object.
pub fn spec_text(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Object(_) => serde_json::to_string(v).map_err(|e| e.to_string()),
        _ => Err("spec must be the document text or a JSON object".into()),
    }
}

pub fn check_name(name: &str) -> Result<(), StoreError> {
    if RESERVED_NAMES.contains(&name) {
        return Err(StoreError::Invalid(format!("{name:?} is a reserved name")));
    }
    Ok(())
}

pub fn unique_name(store: &MockStore, base: &str) -> String {
    let base = if RESERVED_NAMES.contains(&base) {
        format!("{base}-api")
    } else {
        base.to_string()
    };
    if store.get(&base).is_none() {
        return base;
    }
    for n in 2..1000 {
        let stem: String = base.chars().take(36).collect();
        let candidate = format!("{}-{n}", stem.trim_end_matches('-'));
        if store.get(&candidate).is_none() {
            return candidate;
        }
    }
    base
}

/// Lowercase `a-z0-9-` slug of at most `max` characters (`mock` if empty).
pub fn slug(raw: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in raw.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let cut: String = out.chars().take(max).collect();
    let trimmed = cut.trim_matches('-');
    if trimmed.is_empty() {
        "mock".into()
    } else {
        trimmed.to_string()
    }
}

/// Room for the slug of a visitor mock name: 40 characters minus `-` and
/// the six character suffix.
pub const VISITOR_SLUG_MAX: usize = 33;

/// A visitor mock name: the slug plus a random suffix nobody else holds.
fn visitor_name(store: &MockStore, slug_base: &str) -> String {
    for _ in 0..8 {
        let candidate = format!("{slug_base}-{}", super::visits::name_suffix());
        if store.get(&candidate).is_none() {
            return candidate;
        }
    }
    format!("{slug_base}-{}", super::visits::name_suffix())
}

pub fn limits_json(store: &MockStore) -> Value {
    serde_json::to_value(store.limits()).unwrap_or(Value::Null)
}

pub fn operation_rows(c: &Compiled, stats: &HashMap<String, OpStats>) -> Vec<Value> {
    c.spec
        .operations
        .iter()
        .map(|op| {
            let settings = c.settings.operations.get(&op.id);
            let check = c.report.operations.iter().find(|r| r.id == op.id);
            let source = match settings {
                Some(s) if !s.rules.is_empty() => "rules",
                Some(s) if s.respond.as_ref().is_some_and(|r| r.body.is_some()) => "custom",
                Some(s) if s.respond.is_some() => "override",
                _ => match settings
                    .and_then(|s| s.mode)
                    .unwrap_or(c.settings.responses.mode)
                {
                    ResponseMode::Examples => "examples",
                    ResponseMode::Generated => "generated",
                    ResponseMode::Static => "static",
                    ResponseMode::Stateful => "stateful",
                },
            };
            let secured = c.settings.security.enabled
                && !settings.is_some_and(|s| s.public)
                && (c.settings.security.mode != super::model::SecurityMode::Spec
                    || !op.security.is_empty());
            json!({
                "id": op.id,
                "method": op.method,
                "path": op.path,
                "summary": op.summary,
                "tags": op.tags,
                "deprecated": op.deprecated,
                "statuses": op.responses.iter().map(|r| r.status.clone()).collect::<Vec<_>>(),
                "spec_security": !op.security.is_empty(),
                "secured": secured,
                "public": settings.is_some_and(|s| s.public),
                "source": source,
                "rules": settings.map(|s| s.rules.len()).unwrap_or(0),
                "check": check.map(|r| r.status),
                "missing_examples": checks::missing_examples(c, op),
                "stored_examples": settings.map(|s| s.examples.values().map(Vec::len).sum::<usize>()).unwrap_or(0),
                "hits": stats.get(&op.id).map_or(0, |s| s.hits),
                "last_hit": stats.get(&op.id).and_then(|s| s.last_hit.clone()),
            })
        })
        .collect()
}

/// The example coverage of a revision, as the detail and the preview show it.
pub fn coverage_summary(c: &Compiled) -> Value {
    let cov = &c.report.coverage;
    json!({
        "operations": cov.operations,
        "with_examples": cov.with_examples,
        "without": cov.without,
        "complete": cov.is_complete(),
    })
}

fn revisions(s: &Snapshot) -> Vec<Value> {
    let live = s.live.as_ref().map(|l| l.revision);
    let mut list: Vec<Value> = s
        .history
        .iter()
        .rev()
        .map(|c| json!({ "revision": c.revision, "created_at": c.created_at.to_rfc3339(), "live": false }))
        .collect();
    if let Some(l) = &s.live {
        list.insert(0, json!({ "revision": l.revision, "created_at": l.created_at.to_rfc3339(), "live": true }));
    }
    if live != Some(s.draft.revision) {
        list.insert(0, json!({ "revision": s.draft.revision, "created_at": s.draft.created_at.to_rfc3339(), "live": false, "draft": true }));
    }
    list
}

/// Most entries of `draft_changes`.
const MAX_DRAFT_CHANGES: usize = 50;

/// What the draft changes compared with the live revision (empty when the
/// draft is live or was never deployed): `spec`, `responses`, `security`,
/// `operation <id>`, ...
fn draft_changes(s: &Snapshot) -> Vec<String> {
    let (Some(live), draft) = (&s.live, &s.draft) else {
        return Vec::new();
    };
    if live.revision == draft.revision {
        return Vec::new();
    }
    changes_between(live, draft)
}

/// What differs between two revisions: the spec, the parts of the settings,
/// the operations.
pub fn changes_between(live: &Compiled, draft: &Compiled) -> Vec<String> {
    let mut out = Vec::new();
    if live.source != draft.source {
        out.push("spec".to_string());
    }
    let (a, b) = (&live.settings, &draft.settings);
    if a.strip_server_prefix != b.strip_server_prefix {
        out.push("server prefix".to_string());
    }
    if a.responses != b.responses {
        out.push("responses".to_string());
    }
    if a.security != b.security {
        out.push("security".to_string());
    }
    if a.mcp != b.mcp {
        out.push("mcp".to_string());
    }
    let ids: std::collections::BTreeSet<&String> =
        a.operations.keys().chain(b.operations.keys()).collect();
    for id in ids {
        if a.operations.get(id) != b.operations.get(id) {
            out.push(format!("operation {id}"));
        }
    }
    out.truncate(MAX_DRAFT_CHANGES);
    out
}

/// A mock's full definition. Without `reveal` the security secrets are
/// redacted.
pub fn detail(
    s: &Snapshot,
    root: &str,
    host: Option<&HostBase>,
    limits: Value,
    reveal: bool,
) -> Value {
    let c = &s.draft;
    let mut out = s.summary();
    if let Value::Object(m) = &mut out {
        if let Some(h) = host.filter(|_| !s.owner.is_visitor()) {
            m.insert("host_url".into(), json!(h.url(&s.name, c.path_prefix())));
        }
        m.insert("url".into(), json!(format!("{root}{}", c.path_prefix())));
        m.insert("base_url".into(), json!(root));
        m.insert(
            "spec".into(),
            json!({
                "openapi": c.spec.openapi,
                "title": c.spec.title,
                "version": c.spec.version,
                "description": c.spec.description,
                "server_prefix": c.spec.server_prefix,
                "tags": c.spec.tags,
                "security_schemes": c.spec.security_schemes,
                "warnings": c.spec.warnings,
                "spec_url": format!("{root}/_rustybin/openapi.json"),
            }),
        );
        let settings = if reveal {
            serde_json::to_value(&c.settings)
        } else {
            serde_json::to_value(c.settings.redacted())
        };
        m.insert("settings".into(), settings.unwrap_or(Value::Null));
        m.insert("secrets_hidden".into(), json!(!reveal));
        m.insert(
            "operations".into(),
            json!(operation_rows(c, &s.capture.operation_stats())),
        );
        m.insert("credentials".into(), c.credentials_summary(reveal));
        m.insert(
            "report".into(),
            json!({
                "ok": c.report.ok, "errors": c.report.errors, "warnings": c.report.warnings,
                "spec_warnings": c.report.spec_warnings, "findings": c.report.findings,
            }),
        );
        m.insert("coverage".into(), coverage_summary(c));
        m.insert("revisions".into(), json!(revisions(s)));
        m.insert("draft_changes".into(), json!(draft_changes(s)));
        m.insert("mcp".into(), mcp_detail(s, root, host));
        m.insert("limits".into(), limits);
    }
    out
}

/// The mock's MCP server: URLs, on/off and the tools of the live revision
/// (the draft's before the first deploy).
fn mcp_detail(s: &Snapshot, root: &str, host: Option<&HostBase>) -> Value {
    let c = s.live.as_ref().unwrap_or(&s.draft);
    let set = c.mcp_tools();
    let tools: Vec<Value> = set
        .tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name, "operation": t.operation, "method": t.method, "path": t.path,
                "read_only": t.read_only, "destructive": t.destructive,
            })
        })
        .collect();
    let mut out = json!({
        "url": format!("{root}/_rustybin/mcp"),
        "enabled": c.settings.mcp.enabled,
        "serving": s.live.is_some() && s.enabled && c.settings.mcp.enabled,
        "tools": tools,
        "skipped": set.skipped,
    });
    if let Some(h) = host.filter(|_| !s.owner.is_visitor()) {
        out["host_url"] = json!(h.url(&s.name, "/_rustybin/mcp"));
    }
    out
}

/// Nodes [`inline_refs`] writes for one value: past it a `$ref` is left
/// in place (marked), so a spec whose levels each reference the next many
/// times cannot grow the answer exponentially.
pub const INLINE_REFS_NODES: usize = 20_000;

/// Replace `$ref`s with their targets (bounded: recursive refs, refs past the
/// depth and every ref once [`INLINE_REFS_NODES`] are written are kept and
/// marked) so the console can build forms without a resolver.
pub fn inline_refs(v: &Value, root: &Value) -> Value {
    let mut budget = INLINE_REFS_NODES;
    inline_refs_in(v, root, &mut Vec::new(), 0, &mut budget)
}

fn inline_refs_in(
    v: &Value,
    root: &Value,
    stack: &mut Vec<String>,
    depth: usize,
    budget: &mut usize,
) -> Value {
    if depth > 24 {
        return json!({});
    }
    *budget = budget.saturating_sub(1);
    match v {
        Value::Object(m) => {
            if let Some(Value::String(r)) = m.get("$ref") {
                if stack.contains(r) || stack.len() > 10 {
                    return json!({ "$ref": r, "x-recursive": true });
                }
                if *budget == 0 {
                    return json!({ "$ref": r, "x-not-expanded": true });
                }
                let Ok(target) = spec::resolve_pointer(root, r) else {
                    return json!({});
                };
                stack.push(r.clone());
                let mut out = inline_refs_in(target, root, stack, depth + 1, budget);
                stack.pop();
                if let Value::Object(o) = &mut out {
                    let title = r.rsplit('/').next().unwrap_or_default().to_string();
                    o.entry("title").or_insert(json!(title));
                    for (k, val) in m {
                        if k != "$ref" {
                            o.insert(
                                k.clone(),
                                inline_refs_in(val, root, stack, depth + 1, budget),
                            );
                        }
                    }
                }
                return out;
            }
            Value::Object(
                m.iter()
                    .map(|(k, val)| {
                        (
                            k.clone(),
                            inline_refs_in(val, root, stack, depth + 1, budget),
                        )
                    })
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|i| inline_refs_in(i, root, stack, depth + 1, budget))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn operation_detail(c: &Compiled, op_id: &str, root: &str, reveal: bool) -> Option<Value> {
    let op = c.spec.operation(op_id)?;
    let mut op_json = serde_json::to_value(op).unwrap_or(Value::Null);
    let op_json = inline_refs(&op_json.take(), &c.spec.root);
    let mut report = c.report.operations.iter().find(|r| r.id == op_id).cloned();
    let mut sample = checks::sample_request(c, op);
    if !reveal {
        checks::hide_credentials(c, op, &mut sample);
        if let Some(r) = &mut report {
            checks::hide_credentials(c, op, &mut r.request);
        }
    }
    Some(json!({
        "draft_revision": c.revision,
        "operation": op_json,
        "settings": c.settings.operations.get(op_id).cloned().unwrap_or_default(),
        "default_status": op.default_status(),
        "mode": c.settings.operations.get(op_id).and_then(|o| o.mode).unwrap_or(c.settings.responses.mode),
        "report": report,
        "sample_request": sample,
        "url": format!("{root}{}", sample.path),
    }))
}

fn op_not_found(name: &str, op: &str) -> ServiceError {
    StoreError::NotFound(format!("{name}/{op}")).into()
}

/// The owner a visitor's new mock gets, and a manage token when this
/// creation started the visit.
struct NewOwner {
    owner: Owner,
    started: Option<(String, String)>,
}

// ── The service ─────────────────────────────────────────────────────

impl Svc {
    /// Build the service for a request.
    pub fn from_request(
        state: &AppState,
        headers: &HeaderMap,
        extensions: &Extensions,
        uri: &Uri,
    ) -> Self {
        Self {
            caller: ss::caller(state, headers, extensions),
            links: Links::from_request(headers, extensions, uri, &state.config),
            state: state.clone(),
        }
    }

    /// Run a call that parses or compiles a spec on the blocking pool, at
    /// most `COMPILE_PERMITS` at once (503 when none frees up within
    /// `COMPILE_WAIT`), so a hostile spec never holds an async worker; a
    /// caller without admin rights (a visitor, with a manage token or not)
    /// gets `VISITOR_COMPILES_PER_MINUTE` (429 past it), see
    /// [`Self::compile_key`].
    pub async fn compiling<T, F>(self, f: F) -> SvcResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Svc) -> SvcResult<T> + Send + 'static,
    {
        if !self.caller.writes_somewhere()
            && !self.store().allow_visitor_compile(&self.compile_key())
        {
            return Err(ServiceError::retry(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "at most {} spec compiles per minute for a visitor: try again in a minute",
                    super::store::VISITOR_COMPILES_PER_MINUTE
                ),
                60,
            ));
        }
        let permits = self.store().compile_permits();
        let permit = tokio::time::timeout(super::store::COMPILE_WAIT, permits.acquire_owned())
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or_else(|| {
                ServiceError::retry(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the instance is busy compiling other specs: try again in a moment",
                    2,
                )
            })?;
        let (out, note) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crate::audit::carried(|| f(&self))
        })
        .await
        .map_err(|_| {
            ServiceError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the compile did not finish",
            )
        })?;
        crate::audit::merge(note);
        out
    }

    /// Whose compile allowance a call without admin rights spends: the
    /// visit, else the API key, else the client's address (the address part
    /// of the ledger key, which a new `X-Rustybin-Session` per request does
    /// not change), so a caller without a manage token is counted too.
    fn compile_key(&self) -> String {
        let c = &self.caller;
        if let Some(v) = c.visit() {
            return format!("visit:{v}");
        }
        if let Some(k) = c.key() {
            return format!("key:{k}");
        }
        let address = c.ledger.split('/').next().unwrap_or(&c.ledger);
        format!("anon:{address}")
    }

    fn store(&self) -> &MockStore {
        &self.state.mocks
    }

    fn config(&self) -> &Config {
        &self.state.config
    }

    /// `Ok` when the caller holds `p`, else the refusal naming it.
    pub fn need(&self, p: crate::rbac::Permission) -> SvcResult<()> {
        self.caller.need(p)
    }

    /// The mock, if the caller may read it (`404` otherwise). On a
    /// project-scoped route the route's permission is checked again on the
    /// mock's project (step 11), for a mock in a project or a caller bound
    /// to projects (`403` naming it).
    pub fn readable(&self, name: &str) -> SvcResult<Snapshot> {
        match self.store().get(name) {
            Some(s) if self.caller.can_read(&s) => {
                self.route_need_on(&s)?;
                Ok(s)
            }
            _ => Err(ServiceError::not_found(format!("mock {name:?} not found"))),
        }
    }

    /// The route's (or tool's) permission on an instance mock: in the
    /// mock's project, or the instance's. A visitor's mock answers to the
    /// visitor rules instead; reading an instance mock outside projects
    /// stays open to whoever reached the handler (a visitor on a
    /// self-service demo reads the instance's mocks, as in 0.4), but its
    /// traffic, data, deletion or publication need their permission.
    fn route_need_on(&self, s: &Snapshot) -> SvcResult<()> {
        let Some(p) = self.caller.route_need else {
            return Ok(());
        };
        if s.owner.is_visitor() || self.caller.may(p, s) {
            return Ok(());
        }
        let scoped = s.project.is_some() || self.caller.resolved.has_projects();
        if !scoped && p == crate::rbac::Permission::MocksRead {
            return Ok(());
        }
        Err(ServiceError::permission(&self.caller.resolved, p))
    }

    /// The mock, if the caller may change it: `mocks.write` in the instance
    /// or the mock's project for instance mocks (with the permission
    /// refusal), admin or the owner for visitor mocks (`404` for anyone
    /// else).
    pub fn manageable(&self, name: &str) -> SvcResult<Snapshot> {
        match self.readable(name) {
            Ok(s) => {
                let write = crate::rbac::Permission::MocksWrite;
                if !s.owner.is_visitor() && !self.caller.may(write, &s) {
                    return Err(ServiceError::permission(&self.caller.resolved, write));
                }
                Ok(s)
            }
            // Without self-service an unknown name answers as before: the
            // permission first (a member of a project gets the 404 of a mock
            // it may not read). With it, unknown and other people's mocks
            // are 404.
            Err(not_found) => {
                if self.store().visits().is_none() && !self.caller.writes_somewhere() {
                    self.need(crate::rbac::Permission::MocksWrite)?;
                }
                Err(not_found)
            }
        }
    }

    fn detail_of(&self, s: &Snapshot, reveal: bool) -> Value {
        let root = self.links.mock_base(&s.owner, &s.name);
        detail(
            s,
            &root,
            self.links.host(),
            limits_json(self.store()),
            reveal,
        )
    }

    /// `GET /_rustybin/mocks`. `all` (admin): every visitor's mocks too.
    pub fn list(&self, all: bool) -> Value {
        self.list_in(all, &None)
    }

    /// [`Self::list`], optionally of one project (`?project=`; `instance`:
    /// the instance's mocks without one). Only the mocks the caller may
    /// read (step 11: another project's are not listed).
    pub fn list_in(&self, all: bool, filter: &Option<crate::rbac::projects::Filter>) -> Value {
        let c = &self.caller;
        let all = c.admin && all;
        let host = self.links.host();
        let mocks: Vec<Value> = self
            .store()
            .list()
            .iter()
            // Admins see their own and the instance's mocks unless they ask
            // for every visitor's.
            .filter(|s| !s.owner.is_visitor() || c.owns(&s.owner) || all)
            .filter(|s| s.owner.is_visitor() || c.can_read(s))
            .filter(|s| {
                filter.is_none()
                    || (!s.owner.is_visitor()
                        && crate::rbac::projects::passes(filter, s.project.as_deref()))
            })
            .map(|s| {
                let mut v = s.summary();
                let root = self.links.mock_base(&s.owner, &s.name);
                if let Value::Object(m) = &mut v {
                    m.insert(
                        "url".into(),
                        json!(format!("{root}{}", s.draft.path_prefix())),
                    );
                    m.insert("yours".into(), json!(c.owns(&s.owner)));
                    m.insert("can_manage".into(), json!(c.can_manage(s)));
                    if c.admin && s.owner.is_visitor() {
                        m.insert("owner_fingerprint".into(), json!(s.owner.fingerprint()));
                    }
                    if let Some(h) = host.filter(|_| !s.owner.is_visitor()) {
                        m.insert(
                            "host_url".into(),
                            json!(h.url(&s.name, s.draft.path_prefix())),
                        );
                    }
                }
                v
            })
            .collect();
        let mut out = json!({
            "count": mocks.len(),
            "mocks": mocks,
            "load_errors": self.store().load_errors(),
            "persistent": self.store().persist_dir().is_some(),
            "host_domain": self.config().mocks_host,
            "limits": limits_json(self.store()),
            "can_manage_instance": c.admin,
        });
        if let Value::Object(m) = &mut out {
            m.insert("self_service".into(), ss::info(self.config()));
            if let Some(v) = c.visit().and_then(|h| ss::visit_json(&self.state, h)) {
                m.insert("visit".into(), v);
            }
        }
        out
    }

    /// `POST /_rustybin/mocks/import`: parse a spec without creating anything.
    pub fn preview(&self, text: &str) -> SvcResult<Value> {
        let compiled = self
            .store()
            .compile(text, MockSettings::default())
            .map_err(ServiceError::bad_request)?;
        let s = &compiled.spec;
        let suggested = unique_name(self.store(), &s.suggested_name());
        Ok(json!({
            "openapi": s.openapi,
            "title": s.title,
            "version": s.version,
            "description": s.description,
            "suggested_name": suggested,
            "server_prefix": s.server_prefix,
            "tags": s.tags,
            "operations": operation_rows(&compiled, &HashMap::new()),
            "security_schemes": s.security_schemes,
            "declares_security": s.declares_security(),
            "warnings": s.warnings,
            "report": { "ok": compiled.report.ok, "errors": compiled.report.errors, "warnings": compiled.report.warnings },
            "coverage": coverage_summary(&compiled),
        }))
    }

    fn visitor_owner(&self) -> SvcResult<NewOwner> {
        let c = &self.caller;
        let Some(visits) = self.store().visits() else {
            return Err(self
                .need(crate::rbac::Permission::MocksWrite)
                .err()
                .unwrap_or_else(|| ServiceError::forbidden("admin rights required")));
        };
        if visits.paused() {
            return Err(ss::visit_error(VisitError::Paused, self.config()));
        }
        if let Some(k) = c.key() {
            return Ok(NewOwner {
                owner: Owner::Key(k.to_string()),
                started: None,
            });
        }
        if let Some(h) = c.visit() {
            return Ok(NewOwner {
                owner: Owner::Visit(h.to_string()),
                started: None,
            });
        }
        match visits.start(&c.ledger, &EmailInput::default()) {
            Ok((token, hash)) => {
                self.state.metrics.count_visit_event("started");
                Ok(NewOwner {
                    owner: Owner::Visit(hash.clone()),
                    started: Some((token, hash)),
                })
            }
            Err(e) => {
                self.state.metrics.count_visit_event(ss::event_of(&e));
                Err(ss::visit_error(e, self.config()))
            }
        }
    }

    /// Caps and rates a visitor's creation must pass before a visit starts.
    fn visitor_admission(&self, name_slug: &str) -> SvcResult<()> {
        self.visitor_name(name_slug)?;
        self.visitor_caps()
    }

    /// A visitor's mock name is neither blocked nor reserved.
    fn visitor_name(&self, name_slug: &str) -> SvcResult<()> {
        let Some(visits) = self.store().visits() else {
            return Ok(());
        };
        let cfg = visits.config();
        if super::visits::blocked(name_slug, &cfg.blocked_names)
            || RESERVED_NAMES.contains(&name_slug)
        {
            return Err(ServiceError::bad_request(format!(
                "the name {name_slug:?} is not available: pick another"
            )));
        }
        Ok(())
    }

    /// The caps and the creation rate (counted here) of a visitor's new
    /// mock, checked before its spec is compiled.
    fn visitor_caps(&self) -> SvcResult<()> {
        let c = &self.caller;
        let Some(visits) = self.store().visits() else {
            return Ok(());
        };
        if visits.paused() {
            return Err(ss::visit_error(VisitError::Paused, self.config()));
        }
        let cfg = visits.config();
        let mine = c
            .owners
            .iter()
            .map(|o| self.store().owned_names(o).len())
            .max()
            .unwrap_or(0);
        if mine >= cfg.per_owner {
            return Err(ServiceError::with_body(
                StatusCode::CONFLICT,
                json!({
                    "error": "rustybin_mock_cap",
                    "message": format!("you have {mine} mocks, the most one visitor may keep: delete one first"),
                }),
            ));
        }
        if self.store().visitor_count() >= cfg.max_mocks {
            self.state.metrics.count_visit_event("busy");
            return Err(ss::visit_error(
                VisitError::Busy { retry_after: 300 },
                self.config(),
            ));
        }
        visits.note_creation(&c.ledger).map_err(|e| {
            self.state.metrics.count_visit_event(ss::event_of(&e));
            ss::visit_error(e, self.config())
        })
    }

    /// `POST /_rustybin/mocks`: create a mock (deployed unless `deploy` is
    /// false). Visitors get a suffixed name and, when this call started
    /// their visit, the manage token (once).
    pub fn create(&self, req: CreateBody) -> SvcResult<Value> {
        let c = &self.caller;
        // A mock of a project (named, or the one project a member works in)
        // is the instance's, made by a member of the team: never a
        // visitor's.
        let in_project = req.project.is_some() || (!c.admin && c.writes_somewhere());
        let project = if in_project {
            crate::rbac::projects::target(
                &self.state.users,
                &c.resolved,
                crate::rbac::Permission::MocksWrite,
                req.project.as_deref(),
            )?
        } else {
            None
        };
        let visitor = !c.admin && !in_project;
        if visitor && self.store().visits().is_none() {
            self.need(crate::rbac::Permission::MocksWrite)?;
        }
        let text = spec_text(&req.spec).map_err(ServiceError::bad_request)?;
        if visitor {
            // The name given, the caps and the creation rate before the
            // compile: a refused creation costs no compile.
            if let Some(n) = req.name.as_deref() {
                self.visitor_name(&slug(n, VISITOR_SLUG_MAX))?;
            }
            self.visitor_caps()?;
        }
        let settings = MockSettings {
            strip_server_prefix: req.strip_server_prefix.unwrap_or(false),
            responses: req.responses.unwrap_or_default(),
            security: req.security.unwrap_or_default(),
            operations: req.operations.unwrap_or_default(),
            mcp: req.mcp.unwrap_or_default(),
        };
        let compiled = self
            .store()
            .compile_for(&text, settings, visitor)
            .map_err(ServiceError::bad_request)?;
        let deploy = req.deploy.unwrap_or(true);
        let force = req.force.unwrap_or(false);
        if deploy && !force && !compiled.report.ok {
            return Err(StoreError::ChecksFailed(Box::new(compiled.report.clone())).into());
        }
        let (name, owner) = if visitor {
            let base = slug(
                req.name
                    .as_deref()
                    .unwrap_or(&compiled.spec.suggested_name()),
                VISITOR_SLUG_MAX,
            );
            self.visitor_name(&base)?;
            let owner = self.visitor_owner()?;
            (visitor_name(self.store(), &base), owner)
        } else {
            let name = match req.name {
                Some(n) => n,
                None => unique_name(self.store(), &compiled.spec.suggested_name()),
            };
            check_name(&name)?;
            (
                name,
                NewOwner {
                    owner: Owner::Instance,
                    started: None,
                },
            )
        };
        crate::audit::target("mock", &name);
        if let Some(p) = &project {
            crate::audit::target("project", p);
        }
        let created = self.store().create_in(
            &name,
            compiled,
            Origin::Api,
            owner.owner.clone(),
            req.enabled.unwrap_or(true),
            deploy,
            force,
            project,
        );
        match created {
            Ok(s) => {
                let mut out = self.detail_of(&s, c.reveals(&s) || visitor);
                if let Value::Object(m) = &mut out {
                    m.insert("yours".into(), json!(visitor));
                    if let Some((token, _)) = &owner.started {
                        m.insert("manage_token".into(), json!(token));
                    }
                    let hash = owner
                        .started
                        .as_ref()
                        .map(|(_, h)| h.as_str())
                        .or(c.visit());
                    if let Some(v) = hash.and_then(|h| ss::visit_json(&self.state, h)) {
                        m.insert("visit".into(), v);
                    }
                }
                if visitor {
                    tracing::info!(mock = %s.name, owner = %s.owner.fingerprint(), "visitor mock created");
                }
                Ok(out)
            }
            Err(e) => {
                if let (Some((_, hash)), Some(v)) = (&owner.started, self.store().visits()) {
                    v.abandon(hash);
                }
                Err(ss::store_refusal(&e).unwrap_or_else(|| e.into()))
            }
        }
    }

    /// A gateway scenario's upstream (admin): `text` compiled with
    /// `settings`, never saved, gone after `ttl`. Answers its base URL.
    pub fn create_scratch(
        &self,
        name: &str,
        text: &str,
        settings: MockSettings,
        ttl: std::time::Duration,
    ) -> SvcResult<String> {
        self.need(crate::rbac::Permission::ScenariosRun)?;
        check_name(name)?;
        let compiled = self
            .store()
            .compile_for(text, settings, false)
            .map_err(ServiceError::bad_request)?;
        if !compiled.report.ok {
            return Err(StoreError::ChecksFailed(Box::new(compiled.report.clone())).into());
        }
        let snap = self.store().create_scratch(name, compiled, ttl)?;
        Ok(self.links.mock_base(&snap.owner, name))
    }

    /// `GET /_rustybin/mocks/{name}`.
    pub fn get(&self, name: &str) -> SvcResult<Value> {
        let s = self.readable(name)?;
        let c = &self.caller;
        let mut out = self.detail_of(&s, c.reveals(&s));
        if let Value::Object(m) = &mut out {
            m.insert("yours".into(), json!(c.owns(&s.owner)));
            m.insert("can_manage".into(), json!(c.can_manage(&s)));
        }
        Ok(out)
    }

    /// `PATCH /_rustybin/mocks/{name}`: change the draft (spec, settings,
    /// name), optionally deploying it.
    /// The setting `mocks.require_revision`: edits must name the revision
    /// they were made from.
    fn check_revision_given(&self, expected: Option<u64>) -> SvcResult<()> {
        if expected.is_none() && self.state.settings.bool("mocks.require_revision") {
            return Err(ServiceError::with_body(
                StatusCode::PRECONDITION_REQUIRED,
                json!({
                    "error": "rustybin_revision_required",
                    "message": "this instance requires the revision an edit was made from: send If-Match or expected_revision (the draft_revision of the mock)",
                }),
            ));
        }
        Ok(())
    }

    /// A stale edit's refusal, with what changed since the revision it was
    /// made from (when that revision is still in the history).
    fn stale(&self, e: StoreError) -> ServiceError {
        let StoreError::Stale { name, expected, .. } = &e else {
            return e.into();
        };
        let since =
            self.store()
                .get(name)
                .map(|s| match self.store().kept_revision(name, *expected) {
                    Some(o) => json!(changes_between(&o, &s.draft)),
                    None => json!(format!("revision {expected} is no longer in the history")),
                });
        let mut err: ServiceError = e.into();
        if let (Some(v), Value::Object(m)) = (since, &mut err.body) {
            m.insert("changed".into(), v);
        }
        err
    }

    pub fn update(&self, name: &str, mut req: PatchBody) -> SvcResult<Value> {
        if let Some(to) = req.project.take() {
            self.move_to(name, to)?;
            if req.is_only_a_move() {
                let s = self.readable(name)?;
                return Ok(self.detail_of(&s, self.caller.reveals(&s)));
            }
        }
        self.check_revision_given(req.expected_revision)?;
        let edit = self.store().edit_lock();
        let mut attempt = 0;
        let mut snap = loop {
            attempt += 1;
            match self.update_once(name, &req)? {
                Ok(snap) => break snap,
                Err(e) if req.expected_revision.is_none() && attempt < 3 => {
                    tracing::debug!(mock = name, "{}: made again", e.message());
                }
                Err(e) => return Err(self.stale(e)),
            }
        };
        if req.deploy == Some(true) {
            // The revision just written goes live, still under the edit
            // lock: a newer draft is never deployed by this call.
            snap = self
                .store()
                .deploy_if(
                    &snap.name,
                    req.force.unwrap_or(false),
                    Some(snap.draft.revision),
                )
                .map_err(|e| self.stale(e))?;
        }
        drop(edit);
        Ok(self.detail_of(&snap, self.caller.reveals(&snap)))
    }

    /// One attempt of [`Self::update`]: `Ok(Err(stale))` when the draft is
    /// not the revision the edit was made from (named, or read here).
    fn update_once(&self, name: &str, req: &PatchBody) -> SvcResult<Result<Snapshot, StoreError>> {
        let current = self.manageable(name)?;
        let c = &self.caller;
        let draft = current.draft.clone();
        if let Some(e) = req.expected_revision.filter(|e| *e != draft.revision) {
            return Ok(Err(StoreError::Stale {
                name: name.to_string(),
                expected: e,
                current: draft.revision,
            }));
        }
        if let Some(n) = req.name.as_deref().filter(|n| *n != name) {
            if current.owner.is_visitor() && !c.admin {
                return Err(ServiceError::bad_request(
                    "self-service mocks keep the name they were given",
                ));
            }
            check_name(n)?;
        }
        let changes_definition = req.spec.is_some()
            || req.strip_server_prefix.is_some()
            || req.responses.is_some()
            || req.security.is_some()
            || req.operations.is_some()
            || req.mcp.is_some();
        let compiled = if changes_definition {
            let source = match &req.spec {
                Some(v) => Some(spec_text(v).map_err(ServiceError::bad_request)?),
                None => None,
            };
            let mut settings = draft.settings.clone();
            if let Some(v) = req.strip_server_prefix {
                settings.strip_server_prefix = v;
            }
            if let Some(v) = &req.responses {
                settings.responses = v.clone();
            }
            if let Some(v) = &req.security {
                settings.security = v.clone();
            }
            if let Some(v) = &req.operations {
                settings.operations = v.clone();
            }
            if let Some(v) = &req.mcp {
                settings.mcp = v.clone();
            }
            Some(
                self.store()
                    .recompile(&draft, source.as_deref(), settings)
                    .map_err(ServiceError::bad_request)?,
            )
        } else {
            None
        };
        match self
            .store()
            .set_draft_if(name, compiled, req.name.as_deref(), Some(draft.revision))
        {
            Ok(s) => Ok(Ok(s)),
            Err(e @ StoreError::Stale { .. }) => Ok(Err(e)),
            Err(e) => Err(e.into()),
        }
    }

    /// Move a mock to another project (`None`: the instance's): `mocks.write`
    /// in both (step 11). A visitor's mock never moves.
    pub fn move_to(&self, name: &str, to: Option<String>) -> SvcResult<Snapshot> {
        let to = to.filter(|t| t != crate::rbac::projects::INSTANCE && !t.is_empty());
        crate::audit::target("mock", name);
        crate::audit::target(
            "project",
            to.as_deref().unwrap_or(crate::rbac::projects::INSTANCE),
        );
        if let Some(t) = &to {
            crate::rbac::projects::check_name(t).map_err(ServiceError::bad_request)?;
        }
        let current = self.manageable(name)?;
        if current.project == to {
            return Ok(current);
        }
        if current.owner.is_visitor() {
            return Err(ServiceError::bad_request(
                "a self-service mock belongs to its visit, never to a project",
            ));
        }
        crate::rbac::projects::check_move(
            &self.state.users,
            &self.caller.resolved,
            crate::rbac::Permission::MocksWrite,
            current.project.as_deref(),
            to.as_deref(),
        )?;
        Ok(self.store().set_project(name, to)?)
    }

    /// `DELETE /_rustybin/mocks/{name}`.
    pub fn delete(&self, name: &str) -> SvcResult<Value> {
        self.manageable(name)?;
        if self.store().delete(name) {
            Ok(json!({ "deleted": name }))
        } else {
            Err(StoreError::NotFound(name.to_string()).into())
        }
    }

    /// `POST /_rustybin/mocks/{name}/enable` and `/disable`.
    pub fn set_enabled(&self, name: &str, on: bool) -> SvcResult<Value> {
        self.manageable(name)?;
        Ok(self.store().set_enabled(name, on)?.summary())
    }

    /// `POST /_rustybin/mocks/{name}/deploy`. With `expected` (`If-Match` or
    /// `expected_revision`), refused with `409` unless the draft is that
    /// revision, checked under the store's write lock: what goes live is
    /// what the caller saw.
    pub fn deploy(&self, name: &str, force: bool, expected: Option<u64>) -> SvcResult<Value> {
        self.manageable(name)?;
        let s = self
            .store()
            .deploy_if(name, force, expected)
            .map_err(|e| self.stale(e))?;
        let root = self.links.mock_base(&s.owner, &s.name);
        Ok(json!({
            "deployed": true,
            "revision": s.live.as_ref().map(|l| l.revision),
            "report": { "ok": s.draft.report.ok, "errors": s.draft.report.errors, "warnings": s.draft.report.warnings },
            "mock": s.summary(),
            "mcp_url": s.draft.settings.mcp.enabled.then(|| format!("{root}/_rustybin/mcp")),
        }))
    }

    /// `POST /_rustybin/mocks/{name}/rollback`.
    pub fn rollback(&self, name: &str, revision: Option<u64>) -> SvcResult<Value> {
        self.manageable(name)?;
        let s = self.store().rollback(name, revision)?;
        Ok(json!({
            "rolled_back": true,
            "revision": s.live.as_ref().map(|l| l.revision),
            "mock": s.summary(),
        }))
    }

    /// `GET /_rustybin/mocks/{name}/report`: the draft's deploy report.
    pub fn report(&self, name: &str) -> SvcResult<Value> {
        let s = self.readable(name)?;
        if self.caller.reveals(&s) {
            return Ok(serde_json::to_value(&s.draft.report).unwrap_or(Value::Null));
        }
        let mut report = s.draft.report.clone();
        checks::hide_report_credentials(&s.draft, &mut report);
        Ok(serde_json::to_value(&report).unwrap_or(Value::Null))
    }

    /// `GET /_rustybin/mocks/{name}/export`: one mock as `rustybin.yaml`.
    pub fn export_one(&self, name: &str) -> SvcResult<String> {
        let s = self.readable(name)?;
        let reveal = self.caller.reveals(&s);
        Ok(config_file::export(&[s], reveal))
    }

    /// `GET /_rustybin/mocks/export`: the instance's mocks (and the caller's
    /// own; never other visitors') as `rustybin.yaml`. Secrets only for
    /// callers who could change the instance's mocks.
    pub fn export_all(&self) -> String {
        let c = &self.caller;
        let reveal = c.admin;
        let all: Vec<Snapshot> = self
            .store()
            .list()
            .into_iter()
            .filter(|s| {
                s.origin != Origin::Sample
                    && !s.scratch
                    && (!s.owner.is_visitor() || c.owns(&s.owner))
                    && (s.owner.is_visitor() || c.can_read(s))
            })
            .collect();
        config_file::export(&all, reveal)
    }

    /// `POST /_rustybin/mocks/apply` (admin): apply a `rustybin.yaml`
    /// document; `prune` deletes API-created mocks it does not name.
    pub fn apply(&self, text: &str, prune: bool) -> SvcResult<Value> {
        self.need(crate::rbac::Permission::ConfigApply)?;
        let file = config_file::parse(text).map_err(ServiceError::bad_request)?;
        for doc in file.mocks() {
            check_name(&doc.name)?;
        }
        let named: Vec<String> = file.mocks().iter().map(|d| d.name.clone()).collect();
        let (defs, mut errors) = config_file::definitions(
            file.mocks.unwrap_or_default(),
            None,
            self.store().limits().max_spec_bytes as u64,
        );
        // A mock the document names but cannot load is never pruned.
        let keep: Vec<String> = named
            .into_iter()
            .filter(|n| !defs.iter().any(|d| &d.name == n))
            .collect();
        let prune = prune.then_some(Origin::Api);
        let mut report = self.store().apply_keeping(defs, Origin::Api, prune, &keep);
        errors.append(&mut report.errors);
        report.errors = errors;
        Ok(serde_json::to_value(&report).unwrap_or(Value::Null))
    }

    /// What `apply` would do, without doing it: the mocks the document
    /// names, which exist, and the document's errors.
    pub fn apply_dry_run(&self, text: &str, prune: bool) -> SvcResult<Value> {
        self.need(crate::rbac::Permission::ConfigApply)?;
        let file = config_file::parse(text).map_err(ServiceError::bad_request)?;
        for doc in file.mocks() {
            check_name(&doc.name)?;
        }
        let names: Vec<String> = file.mocks().iter().map(|d| d.name.clone()).collect();
        let (defs, mut errors) = config_file::definitions(
            file.mocks.unwrap_or_default(),
            None,
            self.store().limits().max_spec_bytes as u64,
        );
        let mut create = Vec::new();
        let mut update = Vec::new();
        for d in &defs {
            // Compiled like the apply compiles it: the preview reports the
            // same errors.
            if let Err(e) = self.store().compile(&d.source, d.settings.clone()) {
                errors.push(json!({ "name": d.name, "error": e }));
                continue;
            }
            if self.store().get(&d.name).is_some() {
                update.push(d.name.clone());
            } else {
                create.push(d.name.clone());
            }
        }
        let pruned: Vec<String> = if prune {
            self.store()
                .list()
                .into_iter()
                .filter(|s| s.origin == Origin::Api && s.prunable() && !names.contains(&s.name))
                .map(|s| s.name)
                .collect()
        } else {
            Vec::new()
        };
        Ok(json!({
            "dry_run": true,
            "would_create": create,
            "would_update": update,
            "would_delete": pruned,
            "errors": errors,
        }))
    }

    /// `POST /_rustybin/mocks/reload` (admin): reload file-defined mocks.
    pub fn reload(&self) -> SvcResult<Value> {
        self.need(crate::rbac::Permission::ConfigApply)?;
        let (report, problems) = super::reload_files(&self.state);
        Ok(json!({ "report": report, "problems": problems }))
    }

    /// `GET /_rustybin/mocks/{name}/operations/{op}`.
    pub fn operation(&self, name: &str, op: &str) -> SvcResult<Value> {
        let s = self.readable(name)?;
        let root = self.links.mock_base(&s.owner, &s.name);
        operation_detail(&s.draft, op, &root, self.caller.reveals(&s))
            .ok_or_else(|| op_not_found(name, op))
    }

    /// `PUT /_rustybin/mocks/{name}/operations/{op}`: replace one
    /// operation's settings in the draft (`None` removes them).
    pub fn set_operation(
        &self,
        name: &str,
        op: &str,
        settings: Option<OperationSettings>,
        expected: Option<u64>,
    ) -> SvcResult<Value> {
        self.manageable(name)?;
        self.check_revision_given(expected)?;
        let snap = self
            .store()
            .set_operation(name, op, settings, expected)
            .map_err(|e| self.stale(e))?;
        let root = self.links.mock_base(&snap.owner, &snap.name);
        operation_detail(&snap.draft, op, &root, self.caller.reveals(&snap))
            .ok_or_else(|| op_not_found(name, op))
    }

    /// `GET /_rustybin/mocks/{name}/operations/{op}/sample`: a valid
    /// request and the response the mock would send.
    pub fn sample(&self, name: &str, op: &str, q: &SampleQuery) -> SvcResult<Value> {
        let snap = self.readable(name)?;
        let draft = snap.draft.clone();
        let Some(operation) = draft.spec.operation(op) else {
            return Err(op_not_found(name, op));
        };
        // A different seed means a different compiled view of the same draft.
        let reseeded;
        let c: &Compiled = match q.seed {
            Some(seed) if seed != draft.settings.responses.seed => {
                let mut settings = draft.settings.clone();
                settings.responses.seed = seed;
                reseeded = self
                    .store()
                    .recompile(&draft, None, settings)
                    .map_err(ServiceError::bad_request)?;
                &reseeded
            }
            _ => &draft,
        };
        let Some(operation) = c.spec.operation(&operation.id) else {
            return Err(op_not_found(name, op));
        };
        let status = q.status.unwrap_or_else(|| operation.default_status());
        let mode = q.mode.unwrap_or(c.settings.responses.mode);
        let mut request = checks::sample_request(c, operation);
        if !self.caller.reveals(&snap) {
            checks::hide_credentials(c, operation, &mut request);
        }
        let (content_type, body, source) = respond::render_body(
            c,
            operation,
            status,
            BodyChoice {
                body: None,
                example: q.example.as_deref(),
                mode,
            },
            None,
            &[],
        );
        let issues =
            checks::validate_body(c, operation, status, content_type.as_deref(), body.as_ref());
        let root = self.links.mock_base(&snap.owner, name);
        Ok(json!({
            "request": request,
            "url": format!("{root}{}", request.path),
            "response": {
                "status": status,
                "content_type": content_type,
                "headers": respond::spec_headers(c, operation, status),
                "body": body.as_ref().and_then(respond::Body::as_json).or_else(|| match &body {
                    Some(respond::Body::Text(t)) => Some(json!(t)),
                    _ => None,
                }),
                "source": source,
                "valid": issues.is_empty(),
                "issues": issues,
            },
        }))
    }

    /// `POST /_rustybin/mocks/{name}/operations/{op}/validate`: check a
    /// request or response body against the operation's schema.
    pub fn validate(&self, name: &str, op: &str, req: &ValidateBody) -> SvcResult<Value> {
        let draft = self.readable(name)?.draft;
        let Some(operation) = draft.spec.operation(op) else {
            return Err(op_not_found(name, op));
        };
        let (schema_v, direction) = if req.kind.as_deref() == Some("request") {
            (
                operation
                    .request_body
                    .as_ref()
                    .and_then(|rb| rb.json_media())
                    .and_then(|m| m.schema.clone()),
                Direction::Request,
            )
        } else {
            let status = req.status.unwrap_or_else(|| operation.default_status());
            (
                operation
                    .response_for(status)
                    .and_then(|d| respond::negotiate(&d.content, None))
                    .and_then(|m| m.schema.clone()),
                Direction::Response,
            )
        };
        let issues = match &schema_v {
            Some(s) => schema::validate(&req.body, s, &draft.spec.root, direction),
            None => Vec::new(),
        };
        Ok(
            json!({ "valid": issues.is_empty(), "has_schema": schema_v.is_some(), "issues": issues }),
        )
    }

    /// `GET /_rustybin/mocks/{name}/requests`: the mock's captured calls,
    /// newest first.
    pub fn requests(&self, name: &str, limit: Option<usize>) -> SvcResult<Value> {
        let s = self.readable(name)?;
        let limit = limit.unwrap_or(50).clamp(1, self.store().limits().captures);
        let entries = s.capture.entries(limit);
        Ok(json!({
            "mock": name,
            "total": s.capture.total(),
            "count": entries.len(),
            "requests": entries,
        }))
    }

    /// `DELETE /_rustybin/mocks/{name}/requests`.
    pub fn clear_requests(&self, name: &str) -> SvcResult<Value> {
        let s = self.manageable(name)?;
        s.capture.clear();
        Ok(json!({ "cleared": name }))
    }

    /// `GET /_rustybin/mocks/{name}/requests/{n}`.
    pub fn request_entry(&self, name: &str, seq: u64) -> SvcResult<Value> {
        let s = self.readable(name)?;
        match s.capture.entry(seq) {
            Some(e) => Ok(serde_json::to_value(e.as_ref()).unwrap_or(Value::Null)),
            None => Err(ServiceError::not_found(
                "no such request (never received or already evicted)",
            )),
        }
    }

    /// `GET /_rustybin/mocks/{name}/revisions/{revision}`: a revision the
    /// store still keeps (the draft, the live one, the deployed history, an
    /// earlier draft), its spec as uploaded and its settings (secrets
    /// redacted unless the caller may see them), and what the current draft
    /// changes since. Read like the mock: other people's visitor mocks are
    /// `404`, and so is a revision no longer kept.
    pub fn revision(&self, name: &str, revision: u64) -> SvcResult<Value> {
        let s = self.readable(name)?;
        let c = self.store().kept_revision(name, revision).ok_or_else(|| {
            ServiceError::not_found(format!(
                "revision {revision} of mock {name:?} is no longer kept (the draft, the live revision and the last {} deployed and earlier drafts are)",
                self.store().limits().revisions
            ))
        })?;
        let reveal = self.caller.reveals(&s);
        let text = c.source.to_string();
        let format = if text.trim_start().starts_with('{') {
            "json"
        } else {
            "yaml"
        };
        let mut settings = if reveal {
            serde_json::to_value(&c.settings)
        } else {
            serde_json::to_value(c.settings.redacted())
        }
        .unwrap_or(Value::Null);
        // Every section, the default MCP server too (left out when saved), so
        // two revisions compare section by section.
        if let Value::Object(m) = &mut settings {
            if !m.contains_key("mcp") {
                m.insert(
                    "mcp".into(),
                    serde_json::to_value(&c.settings.mcp).unwrap_or(Value::Null),
                );
            }
        }
        let operations: Vec<Value> = c
            .spec
            .operations
            .iter()
            .map(|op| json!({ "id": op.id, "method": op.method, "path": op.path }))
            .collect();
        Ok(json!({
            "name": s.name,
            "revision": c.revision,
            "created_at": c.created_at.to_rfc3339(),
            "draft": c.revision == s.draft.revision,
            "live": s.live.as_ref().is_some_and(|l| l.revision == c.revision),
            "current_draft_revision": s.draft.revision,
            "changes_to_draft": changes_between(&c, &s.draft),
            "spec": {
                "format": format,
                "openapi": c.spec.openapi,
                "title": c.spec.title,
                "version": c.spec.version,
                "text": text,
            },
            "operations": operations,
            "settings": settings,
            "secrets_hidden": !reveal,
        }))
    }

    /// `GET /_rustybin/mocks/{name}/spec`: the spec as uploaded (the
    /// draft's) and whether it is JSON.
    pub fn spec_source(&self, name: &str) -> SvcResult<(String, bool)> {
        let draft = self.readable(name)?.draft;
        let text = draft.source.to_string();
        let json = text.trim_start().starts_with('{');
        Ok((text, json))
    }

    /// `POST /_rustybin/mocks/{name}/duplicate`: a copy under a new name.
    pub fn duplicate(&self, name: &str, new_name: Option<String>) -> SvcResult<Value> {
        let source = self.manageable(name)?;
        let c = &self.caller;
        let new_name = if source.owner.is_visitor() && !c.admin {
            // `orders-k3f9q2` copies as `orders-<new suffix>`.
            let stem = new_name.unwrap_or_else(|| match name.rsplit_once('-') {
                Some((stem, suffix)) if suffix.len() == 6 => stem.to_string(),
                _ => name.to_string(),
            });
            let base = slug(&stem, VISITOR_SLUG_MAX);
            self.visitor_admission(&base)?;
            visitor_name(self.store(), &base)
        } else {
            let n = new_name.unwrap_or_else(|| unique_name(self.store(), &format!("{name}-copy")));
            check_name(&n)?;
            n
        };
        crate::audit::target("mock", &new_name);
        match self.store().duplicate(name, &new_name) {
            Ok(s) => Ok(self.detail_of(&s, c.reveals(&s))),
            Err(e) => Err(ss::store_refusal(&e).unwrap_or_else(|| e.into())),
        }
    }

    /// `POST /_rustybin/mocks/{name}/operations/{op}/rules/test`: which
    /// rule would answer a captured request.
    pub fn test_rules(&self, name: &str, op: &str, req: RuleTestBody) -> SvcResult<Value> {
        let s = self.readable(name)?;
        let c = &s.draft;
        let Some(operation) = c.spec.operation(op) else {
            return Err(op_not_found(name, op));
        };
        let rules = req.rules.unwrap_or_else(|| {
            c.settings
                .operations
                .get(op)
                .map(|o| o.rules.clone())
                .unwrap_or_default()
        });
        let max_rules = self.store().limits().max_rules;
        if rules.len() > max_rules {
            return Err(ServiceError::bad_request(format!(
                "at most {max_rules} rules per operation"
            )));
        }
        let capture = match req.request {
            Some(seq) => s.capture.entry(seq),
            None => s
                .capture
                .entries(self.store().limits().captures)
                .into_iter()
                .find(|e| e.operation.as_deref() == Some(operation.id.as_str())),
        };
        let Some(capture) = capture else {
            return Ok(json!({
                "request": null,
                "winner": null,
                "rules": [],
                "hint": "no captured call of this operation yet: send one (Try it) and test again",
            }));
        };
        let params = match c.match_route(&capture.method, &capture.path) {
            RouteMatch::Found { op: found, params } if found.id == operation.id => params,
            _ => Vec::new(),
        };
        let query: Vec<(String, String)> = capture
            .query
            .as_deref()
            .map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect())
            .unwrap_or_default();
        let mut hm = HeaderMap::new();
        for h in &capture.headers {
            if let (Ok(n), Ok(v)) = (
                HeaderName::from_bytes(h.name.as_bytes()),
                HeaderValue::from_str(&h.value),
            ) {
                hm.append(n, v);
            }
        }
        let is_json = capture
            .content_type
            .as_deref()
            .map(request::base_media)
            .is_some_and(|ct| spec::is_json_media(&ct));
        let body_json: Option<Value> = capture
            .body
            .as_deref()
            .filter(|_| is_json)
            .and_then(|b| serde_json::from_str(b).ok());
        let view = RequestView {
            path_params: &params,
            query: &query,
            headers: &hm,
            body_json: body_json.as_ref(),
            body_text: capture.body.as_deref(),
        };
        let results: Vec<Value> = rules
            .iter()
            .enumerate()
            .map(|(i, rule)| {
                json!({
                    "index": i,
                    "name": rule.name,
                    "matched": respond::rule_matches(rule, &view),
                    "conditions": respond::explain_rule(rule, &view),
                })
            })
            .collect();
        let winner = rules.iter().position(|r| respond::rule_matches(r, &view));
        Ok(json!({
            "request": {
                "seq": capture.seq,
                "method": capture.method,
                "path": capture.path,
                "query": capture.query,
                "timestamp": capture.timestamp,
                "status": capture.status,
            },
            "winner": winner,
            "respond": winner.and_then(|i| rules.get(i)).map(|r| &r.respond),
            "rules": results,
        }))
    }

    /// `GET /_rustybin/mocks/{name}/state`: the stored items of a stateful
    /// mock.
    pub fn state_dump(&self, name: &str) -> SvcResult<Value> {
        Ok(self.readable(name)?.state.dump())
    }

    /// `DELETE /_rustybin/mocks/{name}/state`.
    pub fn clear_state(&self, name: &str) -> SvcResult<Value> {
        let s = self.manageable(name)?;
        s.state.clear();
        Ok(json!({ "cleared": name }))
    }

    /// `GET /_rustybin/mocks/{name}/collections/{format}`: the mock's
    /// operations as a collection (content type, file name, text).
    pub fn collection(
        &self,
        name: &str,
        format: &str,
        base_url: Option<&str>,
    ) -> SvcResult<(&'static str, String, String)> {
        let s = self.readable(name)?;
        let base = match base_url {
            Some(raw) => {
                crate::collections::validate_base_url(raw).map_err(ServiceError::bad_request)?
            }
            None => self.links.mock_base(&s.owner, name),
        };
        let c = s.live.as_ref().unwrap_or(&s.draft);
        let reveal = self.caller.reveals(&s);
        super::collections::render(c, name, format, &base, reveal).ok_or_else(|| {
            let known: Vec<&str> = super::collections::FORMATS
                .iter()
                .map(|(f, _, _)| *f)
                .collect();
            ServiceError::not_found(format!(
                "unknown format {format:?}: use one of {}",
                known.join(", ")
            ))
        })
    }
}

#[cfg(test)]
mod compile_pool_tests {
    use super::*;
    use crate::test_support::{test_app_with, test_state};
    use tower::ServiceExt;

    fn admin_svc(state: &AppState) -> Svc {
        let headers = HeaderMap::new();
        let mut svc = Svc::from_request(
            state,
            &headers,
            &axum::http::Extensions::new(),
            &Uri::from_static("/_rustybin/mocks"),
        );
        svc.caller.admin = true;
        svc
    }

    /// Ten compiles that each hold a thread for a while do not delay `GET /`:
    /// they run on the blocking pool, four at a time.
    #[tokio::test]
    async fn compiles_run_off_the_runtime() {
        let state = test_state();
        let mut jobs = Vec::new();
        for _ in 0..10 {
            let svc = admin_svc(&state);
            jobs.push(tokio::spawn(svc.compiling(|_| {
                std::thread::sleep(std::time::Duration::from_millis(300));
                Ok(())
            })));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let app = test_app_with(state.clone());
        let started = std::time::Instant::now();
        let resp = app
            .oneshot(
                axum::http::Request::get("/")
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(250),
            "GET / waited {:?}",
            started.elapsed()
        );
        for j in jobs {
            assert!(j.await.expect("join").is_ok());
        }
    }

    #[tokio::test]
    async fn a_visitor_past_the_compile_allowance_gets_429() {
        let state = test_state();
        let mut svc = admin_svc(&state);
        svc.caller.admin = false;
        svc.caller.resolved = std::sync::Arc::new(crate::rbac::Resolved {
            permissions: crate::rbac::PermSet::EMPTY,
            ..(*svc.caller.resolved).clone()
        });
        svc.caller.owners = vec![crate::mocks::visits::Owner::Visit("v-1".into())];
        for _ in 0..super::super::store::VISITOR_COMPILES_PER_MINUTE {
            let s = Svc {
                state: svc.state.clone(),
                caller: svc.caller.clone(),
                links: svc.links.clone(),
            };
            assert!(s.compiling(|_| Ok(())).await.is_ok());
        }
        let e = svc.compiling(|_| Ok(())).await.expect_err("refused");
        assert_eq!(e.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(e.retry_after, Some(60));
    }
}
