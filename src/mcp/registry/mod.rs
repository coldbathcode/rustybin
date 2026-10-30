//! The mock MCP registry (`docs/plans/next/step-06-mcp-registry.md`): this
//! instance's MCP servers, the `rustybin.yaml` `registry.servers` and the
//! entries published through the API, in the shape of the MCP registry API
//! (`/v0.1`, the version frozen in 2025; `/v0` answers the same), with
//! scenarios that break a gateway's registry sync on purpose. And the
//! inspector side: browsing a real registry ([`browse`]) and validating
//! `server.json` documents ([`validate`]).
//!
//! | Route | |
//! |---|---|
//! | `GET /mcp-registry` | What this registry is and where its API lives |
//! | `GET /mcp-registry/v0.1/servers` | One record per server version: `cursor`, `limit`, `search`, `updated_since`, `version`, `include_deleted` |
//! | `GET /mcp-registry/v0.1/servers/{serverName}/versions` | Every version of a server (the name URL-encoded) |
//! | `GET /mcp-registry/v0.1/servers/{serverName}/versions/{version}` | One version, `latest` for the latest |
//! | `PATCH .../versions/{version}/status`, `PATCH .../{serverName}/status` | Status of published entries (token) |
//! | `POST /mcp-registry/v0.1/publish` | Publish a `server.json` (a token from `auth/*`) |
//! | `POST /mcp-registry/v0.1/validate` | `{valid, issues}` |
//! | `POST /mcp-registry/v0.1/auth/{method}` | Token stubs: `github-at`, `github-oidc`, `oidc`, `dns`, `http`, `none` |
//! | `GET /mcp-registry/v0.1/health`, `ping`, `version` | As the API has them |
//! | `/mcp-registry/v0/{*path}` | The same API under `/v0` |
//! | `GET /_rustybin/registry` | The console's view: entries with their source, the scenario, the sync snippet |
//! | `DELETE /_rustybin/registry/servers/{serverName}` | Remove published entries (admin) |
//! | `POST /_rustybin/registry/validate` | Validate a `server.json` (read scope) |
//! | `GET /_rustybin/registry/browse` | One page of a real registry through the egress policy |
//!
//! State is bounded: at most [`MAX_USER_ENTRIES`] file and published
//! entries together, [`MAX_TOKENS`] tokens of [`TOKEN_TTL_SECS`] each, the
//! browse cache ([`browse::CACHE_MAX`] pages for a minute).

pub mod browse;
pub mod entries;
pub mod validate;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, Extensions, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::catalog::{category, Endpoint, Example};
use crate::settings::{Kind, Settings, Spec};
use crate::state::AppState;
use entries::{Entry, Filter, Source, NAMESPACE};

pub const ROOT_PATH: &str = "/mcp-registry";
pub const API_PREFIX: &str = "/mcp-registry/v0.1";
pub const ALIAS_PREFIX: &str = "/mcp-registry/v0";
pub const STATUS_PATH: &str = "/_rustybin/registry";
pub const VALIDATE_PATH: &str = "/_rustybin/registry/validate";
pub const BROWSE_PATH: &str = "/_rustybin/registry/browse";

/// File and published entries together.
pub const MAX_USER_ENTRIES: usize = 256;
/// Registry tokens kept, and their lifetime.
pub const MAX_TOKENS: usize = 256;
pub const TOKEN_TTL_SECS: i64 = 300;
/// The wait of the `slow` scenario.
pub const SLOW_DELAY: Duration = Duration::from_secs(2);
/// The token exchange methods (`POST /v0.1/auth/{method}`).
pub const AUTH_METHODS: &[&str] = &["github-at", "github-oidc", "oidc", "dns", "http", "none"];

// ── Scenarios ───────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    Normal,
    Empty,
    Large,
    Flaky,
    Slow,
    Stale,
    Invalid,
    AuthFail,
}

/// Every scenario and what it does (the order of the console's picker).
pub const SCENARIOS: &[(&str, &str)] = &[
    ("normal", "Every entry, answered at once"),
    ("empty", "No entries at all"),
    (
        "large",
        "2000 synthetic entries on top, every tenth with two versions (pagination)",
    ),
    ("flaky", "Every third list request answers 503"),
    ("slow", "Every answer waits 2 seconds"),
    ("stale", "Every entry last updated on 2024-01-15"),
    ("invalid", "One entry that violates the server.json schema"),
    (
        "auth_fail",
        "Every token exchange, publish and status change answers 401",
    ),
];

const SCENARIO_IDS: &[&str] = &[
    "normal",
    "empty",
    "large",
    "flaky",
    "slow",
    "stale",
    "invalid",
    "auth_fail",
];

pub fn scenario_ids() -> Vec<&'static str> {
    SCENARIO_IDS.to_vec()
}

impl Scenario {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "normal" => Self::Normal,
            "empty" => Self::Empty,
            "large" => Self::Large,
            "flaky" => Self::Flaky,
            "slow" => Self::Slow,
            "stale" => Self::Stale,
            "invalid" => Self::Invalid,
            "auth_fail" => Self::AuthFail,
            _ => return None,
        })
    }

    pub fn id(self) -> &'static str {
        SCENARIO_IDS
            .iter()
            .copied()
            .find(|id| Self::parse(id) == Some(self))
            .unwrap_or("normal")
    }

    pub fn current(settings: &Settings) -> Self {
        settings
            .string("registry.scenario")
            .and_then(|s| Self::parse(&s))
            .unwrap_or(Self::Normal)
    }
}

// ── Settings ────────────────────────────────────────────────────────

fn valid_registry_urls(v: &Value) -> Result<(), String> {
    for item in v.as_array().into_iter().flatten() {
        let s = item.as_str().unwrap_or("");
        browse::api_base(s).map_err(|e| format!("{s:?}: {e}"))?;
    }
    Ok(())
}

/// The registry's settings (`registry.scenario`, `registry.saved`).
pub fn specs() -> Vec<Spec> {
    vec![
        Spec::new(
            "registry.scenario",
            "registry",
            "Mock registry scenario",
            "What the mock MCP registry at /mcp-registry does: normal, empty, large (2000 synthetic entries), flaky (every third list answers 503), slow (2 s), stale (old dates), invalid (one entry breaks the schema), auth_fail (every token and publish answers 401).",
            Kind::Choice { options: SCENARIO_IDS },
            json!("normal"),
        )
        .env("RUSTYBIN_REGISTRY_SCENARIO", |c| {
            c.registry_scenario.clone().map(Value::String)
        }),
        Spec::new(
            "registry.saved",
            "registry",
            "Saved registries",
            "Registry base URLs the console offers under Browse a registry (registry.modelcontextprotocol.io is always offered). Their hosts may be reached for browsing.",
            Kind::List {
                max_items: 20,
                max_len: 2048,
            },
            json!([]),
        )
        .env("RUSTYBIN_REGISTRY_SAVED", |c| {
            (!c.registry_saved.is_empty()).then(|| json!(c.registry_saved))
        })
        .check(valid_registry_urls),
    ]
}

// ── Entries from the config file ────────────────────────────────────

/// The usable `registry.servers` of a config file, and a problem line per
/// entry that is not (invalid, in the reserved namespace, a duplicate, over
/// the cap).
pub fn file_entries(servers: Option<Vec<Value>>) -> (Vec<Value>, Vec<String>) {
    let mut ok: Vec<Value> = Vec::new();
    let mut problems = Vec::new();
    for (i, s) in servers.into_iter().flatten().enumerate() {
        let name = s["name"].as_str().unwrap_or("?").to_string();
        let label = format!("registry.servers[{i}] ({name})");
        let result = validate::validate(&s);
        let errors = validate::errors(&result);
        if !errors.is_empty() {
            problems.push(format!("{label}: {} (skipped)", errors.join("; ")));
        } else if reserved(&name) {
            problems.push(format!(
                "{label}: the {NAMESPACE} namespace holds this instance's own servers (skipped)"
            ));
        } else if ok
            .iter()
            .any(|o| o["name"] == s["name"] && o["version"] == s["version"])
        {
            problems.push(format!(
                "{label}: a duplicate of an earlier entry (skipped)"
            ));
        } else if ok.len() >= MAX_USER_ENTRIES {
            problems.push(format!(
                "{label}: more than {MAX_USER_ENTRIES} entries (skipped)"
            ));
        } else {
            ok.push(s);
        }
    }
    (ok, problems)
}

fn reserved(name: &str) -> bool {
    name.split('/')
        .next()
        .is_some_and(|ns| ns.eq_ignore_ascii_case(NAMESPACE))
}

// ── State ───────────────────────────────────────────────────────────

/// The registry's state (module-local, attached with `Extension`).
pub struct Registry {
    started: DateTime<Utc>,
    published: Mutex<Vec<Entry>>,
    /// Token -> expiry (unix seconds).
    tokens: Mutex<HashMap<String, i64>>,
    lists: AtomicU64,
    pub browse: browse::Cache,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            started: Utc::now(),
            published: Mutex::new(Vec::new()),
            tokens: Mutex::new(HashMap::new()),
            lists: AtomicU64::new(0),
            browse: browse::Cache::default(),
        }
    }
}

impl Registry {
    fn published(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        self.published.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The `server.json` documents published through the API (the
    /// configuration export).
    pub fn published_servers(&self) -> Vec<Value> {
        self.published().iter().map(|e| e.server.clone()).collect()
    }

    /// Drop the published entries `keep` refuses (name, version); the
    /// number removed. The configuration document's apply uses it: entries
    /// it carries move into the file copy, the rest go when pruning.
    pub fn retain_published(&self, keep: impl Fn(&str, &str) -> bool) -> usize {
        let mut published = self.published();
        let before = published.len();
        published.retain(|e| keep(e.name(), e.version()));
        before - published.len()
    }

    /// Drop a published entry (every version, or one); the number removed.
    pub fn delete_published(&self, name: &str, version: Option<&str>) -> usize {
        let mut published = self.published();
        let before = published.len();
        published.retain(|e| !(e.name() == name && version.is_none_or(|v| e.version() == v)));
        let removed = before - published.len();
        if removed > 0 {
            tracing::info!(target: "rustybin::registry", name = %name, removed, "published entries deleted");
        }
        removed
    }

    fn mint(&self) -> (String, i64) {
        let now = Utc::now().timestamp();
        let expires = now + TOKEN_TTL_SECS;
        let token = format!("rbreg_{}", uuid::Uuid::new_v4().simple());
        let mut tokens = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        tokens.retain(|_, exp| *exp > now);
        while tokens.len() >= MAX_TOKENS {
            let Some(oldest) = tokens
                .iter()
                .min_by_key(|(_, exp)| **exp)
                .map(|(t, _)| t.clone())
            else {
                break;
            };
            tokens.remove(&oldest);
        }
        tokens.insert(token.clone(), expires);
        (token, expires)
    }

    fn token_valid(&self, token: &str) -> bool {
        let now = Utc::now().timestamp();
        let mut tokens = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        match tokens.get(token) {
            Some(exp) if *exp > now => true,
            Some(_) => {
                tokens.remove(token);
                false
            }
            None => false,
        }
    }
}

/// The base URL the generated remotes point at (the request's origin).
fn base_url(state: &AppState, headers: &HeaderMap, extensions: &Extensions, uri: &Uri) -> String {
    crate::session::request_origin(headers, extensions, uri, &state.config).base_url()
}

/// Every entry under the current scenario, sorted by name then version.
pub fn all_entries(state: &AppState, reg: &Registry, base: &str) -> Vec<Entry> {
    let mut list = entries::generated(state, base, reg.started);
    let (file, at) = state.mocks.file_registry();
    list.extend(file.into_iter().map(|s| Entry::new(s, Source::File, at)));
    list.extend(reg.published().iter().cloned());
    entries::assemble(list, Scenario::current(&state.settings), Utc::now())
}

// ── Answers ─────────────────────────────────────────────────────────

/// An RFC 9457 problem, as the registry answers errors.
fn problem(status: StatusCode, detail: impl Into<String>, errors: Option<Value>) -> Response {
    let mut body = json!({
        "title": status.canonical_reason().unwrap_or("Error"),
        "status": status.as_u16(),
        "detail": detail.into(),
    });
    if let Some(e) = errors {
        body["errors"] = e;
    }
    (
        status,
        [(header::CONTENT_TYPE, "application/problem+json")],
        body.to_string(),
    )
        .into_response()
}

/// What the scenario does before a route answers: wait (`slow`), fail
/// every third list (`flaky`).
async fn prelude(state: &AppState, reg: &Registry, list: bool) -> Option<Response> {
    match Scenario::current(&state.settings) {
        Scenario::Slow => {
            tokio::time::sleep(SLOW_DELAY).await;
            None
        }
        Scenario::Flaky if list => {
            let n = reg.lists.fetch_add(1, Ordering::Relaxed);
            (n % 3 == 2).then(|| {
                problem(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the registry is unavailable (the flaky scenario fails every third list request)",
                    None,
                )
            })
        }
        // The flaky count starts over once a list runs under another scenario.
        _ if list => {
            reg.lists.store(0, Ordering::Relaxed);
            None
        }
        _ => None,
    }
}

/// Writes in public mode need the admin token in `X-Rustybin-Admin-Token`
/// (`Authorization` carries the registry token).
fn public_guard(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    if !state.config.public_mode {
        return None;
    }
    let presented = headers
        .get(crate::admin::ADMIN_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok());
    let ok = match (state.config.admin_token.as_deref(), presented) {
        (Some(expected), Some(p)) => crate::control_auth::constant_time_eq(p, expected),
        _ => false,
    };
    (!ok).then(|| {
        problem(
            StatusCode::FORBIDDEN,
            "publishing to the mock registry is disabled in public mode without the admin token (X-Rustybin-Admin-Token)",
            None,
        )
    })
}

/// The registry token of a write, or the 401.
fn require_token(state: &AppState, reg: &Registry, headers: &HeaderMap) -> Option<Response> {
    if Scenario::current(&state.settings) == Scenario::AuthFail {
        return Some(problem(
            StatusCode::UNAUTHORIZED,
            "invalid or expired Registry JWT token (the auth_fail scenario refuses every token)",
            None,
        ));
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or(v.strip_prefix("bearer ")))
        .map(str::trim);
    match token {
        None => Some(problem(
            StatusCode::UNAUTHORIZED,
            "required header parameter is missing: Authorization: Bearer <registry token> (POST /v0.1/auth/none gives one)",
            Some(json!([{ "message": "required header parameter is missing", "location": "header.Authorization", "value": "" }])),
        )),
        Some(t) if reg.token_valid(t) => None,
        Some(_) => Some(problem(
            StatusCode::UNAUTHORIZED,
            "invalid or expired Registry JWT token",
            None,
        )),
    }
}

fn list_body(items: &[Entry], next: Option<String>) -> Value {
    let mut metadata = json!({ "count": items.len() });
    if let Some(c) = next {
        metadata["nextCursor"] = json!(c);
    }
    json!({
        "servers": items.iter().map(Entry::response).collect::<Vec<_>>(),
        "metadata": metadata,
    })
}

#[allow(clippy::result_large_err)]
fn parse_body(body: &Bytes) -> Result<Value, Response> {
    if body.len() > validate::MAX_DOC_BYTES {
        return Err(problem(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "the body is larger than {} KiB",
                validate::MAX_DOC_BYTES / 1024
            ),
            None,
        ));
    }
    serde_json::from_slice(body).map_err(|e| {
        problem(
            StatusCode::BAD_REQUEST,
            format!("the body is not JSON: {e}"),
            None,
        )
    })
}

fn issue_errors(result: &Value) -> Value {
    Value::Array(
        result["issues"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| i["severity"] == "error")
            .map(|i| {
                let path = i["path"].as_str().unwrap_or("");
                json!({
                    "message": i["message"],
                    "location": if path.is_empty() { "body".to_string() } else { format!("body.{path}") },
                })
            })
            .collect(),
    )
}

// ── Data plane handlers ─────────────────────────────────────────────

async fn index(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let base = base_url(&state, &headers, &extensions, &uri);
    Json(json!({
        "name": "Rustybin mock MCP registry",
        "description": "This instance's MCP servers, the entries of its config file and the ones published here, in the shape of the MCP registry API. The scenario (Management, Network, or the registry.scenario setting) makes it fail on purpose.",
        "api": format!("{base}{API_PREFIX}"),
        "aliases": [format!("{base}{ALIAS_PREFIX}")],
        "servers": format!("{base}{API_PREFIX}/servers"),
        "schema": validate::SCHEMA_URL,
        "scenario": Scenario::current(&state.settings).id(),
        "console": format!("{base}/ui/#/registry"),
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
struct ListQuery {
    cursor: Option<String>,
    limit: Option<String>,
    updated_since: Option<String>,
    search: Option<String>,
    version: Option<String>,
    include_deleted: Option<String>,
}

fn parse_bool(v: Option<&str>) -> Result<bool, String> {
    match v.map(str::trim) {
        None | Some("") => Ok(false),
        Some("true" | "1") => Ok(true),
        Some("false" | "0") => Ok(false),
        Some(other) => Err(format!("expected a boolean, got {other:?}")),
    }
}

#[allow(clippy::result_large_err)]
fn filter_from(q: &ListQuery) -> Result<Filter, Response> {
    let bad = |detail: String, location: &str, value: &str| {
        problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            detail.clone(),
            Some(json!([{ "message": detail, "location": location, "value": value }])),
        )
    };
    let limit = match q.limit.as_deref().map(str::trim) {
        None | Some("") => 30,
        Some(raw) => match raw.parse::<usize>() {
            Ok(n) if (1..=100).contains(&n) => n,
            _ => {
                return Err(bad(
                    "limit must be an integer between 1 and 100".into(),
                    "query.limit",
                    raw,
                ))
            }
        },
    };
    let search =
        match q.search.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(s) if s.chars().count() < 3 => return Err(bad(
                "search must contain at least 3 characters after trimming surrounding whitespace"
                    .into(),
                "query.search",
                s,
            )),
            Some(s) => Some(s.to_string()),
        };
    let updated_since = match q.updated_since.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => match DateTime::parse_from_rfc3339(raw) {
            Ok(t) => Some(t.with_timezone(&Utc)),
            Err(_) => {
                return Err(bad(
                    "updated_since must be an RFC 3339 date-time".into(),
                    "query.updated_since",
                    raw,
                ))
            }
        },
    };
    let include_deleted = parse_bool(q.include_deleted.as_deref())
        .map_err(|e| bad(e, "query.include_deleted", ""))?;
    Ok(Filter {
        search,
        version: q
            .version
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        updated_since,
        include_deleted,
        cursor: q
            .cursor
            .as_deref()
            .filter(|c| !c.is_empty())
            .map(str::to_string),
        limit,
    })
}

async fn list_servers(
    State(state): State<AppState>,
    Extension(reg): Extension<Arc<Registry>>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    Query(q): Query<ListQuery>,
) -> Response {
    if let Some(r) = prelude(&state, &reg, true).await {
        return r;
    }
    let filter = match filter_from(&q) {
        Ok(f) => f,
        Err(r) => return r,
    };
    let base = base_url(&state, &headers, &extensions, &uri);
    let all = all_entries(&state, &reg, &base);
    let (items, next) = entries::page(&all, &filter);
    Json(list_body(&items, next)).into_response()
}

#[derive(Deserialize, Default)]
struct DeletedQuery {
    include_deleted: Option<String>,
}

async fn server_versions(
    State(state): State<AppState>,
    Extension(reg): Extension<Arc<Registry>>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    Path(name): Path<String>,
    Query(q): Query<DeletedQuery>,
) -> Response {
    if let Some(r) = prelude(&state, &reg, false).await {
        return r;
    }
    let include_deleted = parse_bool(q.include_deleted.as_deref()).unwrap_or(false);
    let base = base_url(&state, &headers, &extensions, &uri);
    let items: Vec<Entry> = all_entries(&state, &reg, &base)
        .into_iter()
        .filter(|e| e.name() == name && (include_deleted || e.status != "deleted"))
        .collect();
    if items.is_empty() {
        return problem(StatusCode::NOT_FOUND, "Server not found", None);
    }
    Json(list_body(&items, None)).into_response()
}

async fn server_version(
    State(state): State<AppState>,
    Extension(reg): Extension<Arc<Registry>>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    Path((name, version)): Path<(String, String)>,
    Query(q): Query<DeletedQuery>,
) -> Response {
    if let Some(r) = prelude(&state, &reg, false).await {
        return r;
    }
    let include_deleted = parse_bool(q.include_deleted.as_deref()).unwrap_or(false);
    let base = base_url(&state, &headers, &extensions, &uri);
    let found = all_entries(&state, &reg, &base).into_iter().find(|e| {
        e.name() == name
            && (include_deleted || e.status != "deleted")
            && if version == "latest" {
                e.is_latest
            } else {
                e.version() == version
            }
    });
    match found {
        Some(e) => Json(e.response()).into_response(),
        None => problem(StatusCode::NOT_FOUND, "Server not found", None),
    }
}

async fn auth_exchange(
    State(state): State<AppState>,
    Extension(reg): Extension<Arc<Registry>>,
    Path(method): Path<String>,
    body: Bytes,
) -> Response {
    if let Some(r) = prelude(&state, &reg, false).await {
        return r;
    }
    if !AUTH_METHODS.contains(&method.as_str()) {
        return problem(
            StatusCode::NOT_FOUND,
            format!(
                "no auth method {method:?}: one of {}",
                AUTH_METHODS.join(", ")
            ),
            None,
        );
    }
    let doc: Value = if body.iter().all(u8::is_ascii_whitespace) {
        json!({})
    } else {
        match parse_body(&body) {
            Ok(d) => d,
            Err(r) => return r,
        }
    };
    if !doc.is_object() {
        return problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            "expected a JSON object",
            None,
        );
    }
    let required: &[&str] = match method.as_str() {
        "github-at" => &["github_token"],
        "github-oidc" | "oidc" => &["oidc_token"],
        "dns" | "http" => &["domain", "timestamp", "signed_timestamp"],
        _ => &[],
    };
    let missing: Vec<Value> = required
        .iter()
        .filter(|k| doc[**k].as_str().is_none_or(|s| s.trim().is_empty()))
        .map(|k| json!({ "message": format!("expected required property {k} to be present"), "location": "body", "value": doc }))
        .collect();
    if !missing.is_empty() {
        return problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation failed",
            Some(Value::Array(missing)),
        );
    }
    if Scenario::current(&state.settings) == Scenario::AuthFail {
        let what = match method.as_str() {
            "dns" => "the DNS TXT record does not verify the signature",
            "http" => "the key at /.well-known/mcp-registry-auth does not verify the signature",
            "github-at" => "the GitHub token is invalid",
            "none" => "anonymous publishing is off",
            _ => "the OIDC token is invalid",
        };
        return problem(
            StatusCode::UNAUTHORIZED,
            format!("authentication failed: {what} (the auth_fail scenario)"),
            None,
        );
    }
    let (token, expires_at) = reg.mint();
    tracing::info!(target: "rustybin::registry", method = %method, "registry token issued");
    Json(json!({ "registry_token": token, "expires_at": expires_at })).into_response()
}

async fn publish(
    State(state): State<AppState>,
    Extension(reg): Extension<Arc<Registry>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(r) = prelude(&state, &reg, false).await {
        return r;
    }
    if let Some(r) = public_guard(&state, &headers) {
        return r;
    }
    if let Some(r) = require_token(&state, &reg, &headers) {
        return r;
    }
    let doc = match parse_body(&body) {
        Ok(d) => d,
        Err(r) => return r,
    };
    let result = validate::validate(&doc);
    if result["valid"] != true {
        let schema = result["issues"].as_array().is_some_and(|a| {
            a.iter()
                .any(|i| i["type"] == "schema" && i["severity"] == "error")
        });
        return problem(
            if schema {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::BAD_REQUEST
            },
            format!(
                "validation failed: {}",
                validate::errors(&result).join("; ")
            ),
            Some(issue_errors(&result)),
        );
    }
    let name = doc["name"].as_str().unwrap_or("").to_string();
    let version = doc["version"].as_str().unwrap_or("").to_string();
    if reserved(&name) {
        return problem(
            StatusCode::FORBIDDEN,
            format!("you do not have permission to publish this server: the {NAMESPACE} namespace holds this instance's own servers"),
            None,
        );
    }
    let (file, _) = state.mocks.file_registry();
    let entry = {
        let mut published = reg.published();
        let duplicate = published
            .iter()
            .any(|e| e.name() == name && e.version() == version)
            || file
                .iter()
                .any(|s| s["name"] == name.as_str() && s["version"] == version.as_str());
        if duplicate {
            return problem(
                StatusCode::BAD_REQUEST,
                format!("invalid version: cannot publish duplicate version {version} of {name}"),
                None,
            );
        }
        if published.len() + file.len() >= MAX_USER_ENTRIES {
            return problem(
                StatusCode::CONFLICT,
                format!("the mock registry holds at most {MAX_USER_ENTRIES} file and published entries: delete one first (DELETE /_rustybin/registry/servers/{{name}})"),
                None,
            );
        }
        let e = Entry::new(doc, Source::Published, Utc::now());
        published.push(e.clone());
        e
    };
    tracing::info!(target: "rustybin::registry", name = %name, version = %version, "server published");
    Json(entry.response()).into_response()
}

async fn validate_doc(
    State(state): State<AppState>,
    Extension(reg): Extension<Arc<Registry>>,
    body: Bytes,
) -> Response {
    if let Some(r) = prelude(&state, &reg, false).await {
        return r;
    }
    match parse_body(&body) {
        Ok(doc) => Json(validate::validate(&doc)).into_response(),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusBody {
    status: String,
    #[serde(default, rename = "statusMessage")]
    status_message: Option<String>,
}

#[allow(clippy::result_large_err)]
fn status_body(body: &Bytes) -> Result<(&'static str, Option<String>), Response> {
    let doc = parse_body(body)?;
    let b: StatusBody = serde_json::from_value(doc).map_err(|e| {
        problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("expected {{status, statusMessage?}}: {e}"),
            None,
        )
    })?;
    let Some(status) = entries::STATUSES.iter().copied().find(|s| *s == b.status) else {
        return Err(problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("status must be one of {}", entries::STATUSES.join(", ")),
            None,
        ));
    };
    if b.status_message
        .as_deref()
        .is_some_and(|m| m.chars().count() > 500)
    {
        return Err(problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            "statusMessage is longer than 500 characters",
            None,
        ));
    }
    Ok((status, b.status_message))
}

/// Change the status of published versions (`version`: one, else all).
#[allow(clippy::too_many_arguments, clippy::result_large_err)]
async fn change_status(
    state: &AppState,
    reg: &Registry,
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
    name: &str,
    version: Option<&str>,
    body: &Bytes,
) -> Result<Vec<Entry>, Response> {
    if let Some(r) = prelude(state, reg, false).await {
        return Err(r);
    }
    if let Some(r) = public_guard(state, headers) {
        return Err(r);
    }
    if let Some(r) = require_token(state, reg, headers) {
        return Err(r);
    }
    let (status, message) = status_body(body)?;
    let now = Utc::now();
    let changed: Vec<Entry> = {
        let mut published = reg.published();
        let mut changed = Vec::new();
        for e in published
            .iter_mut()
            .filter(|e| e.name() == name && version.is_none_or(|v| e.version() == v))
        {
            e.status = status;
            e.status_message = message.clone();
            e.status_changed_at = now;
            e.updated_at = now;
            changed.push(e.clone());
        }
        changed
    };
    if changed.is_empty() {
        let base = base_url(state, headers, extensions, uri);
        let exists = all_entries(state, reg, &base)
            .iter()
            .any(|e| e.name() == name && version.is_none_or(|v| e.version() == v));
        return Err(if exists {
            problem(
                StatusCode::FORBIDDEN,
                "only published entries change status here (generated entries follow the instance, file entries the config file)",
                None,
            )
        } else {
            problem(StatusCode::NOT_FOUND, "Server not found", None)
        });
    }
    tracing::info!(target: "rustybin::registry", name = %name, status, count = changed.len(), "server status changed");
    Ok(changed)
}

#[allow(clippy::too_many_arguments)]
async fn status_one(
    State(state): State<AppState>,
    Extension(reg): Extension<Arc<Registry>>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    Path((name, version)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    match change_status(
        &state,
        &reg,
        &headers,
        &extensions,
        &uri,
        &name,
        Some(&version),
        &body,
    )
    .await
    {
        Ok(changed) => {
            let mut e = changed[0].clone();
            // isLatest as the listing computes it.
            let base = base_url(&state, &headers, &extensions, &uri);
            if let Some(cur) = all_entries(&state, &reg, &base)
                .into_iter()
                .find(|x| x.name() == e.name() && x.version() == e.version())
            {
                e.is_latest = cur.is_latest;
            }
            Json(e.response()).into_response()
        }
        Err(r) => r,
    }
}

async fn status_all(
    State(state): State<AppState>,
    Extension(reg): Extension<Arc<Registry>>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    Path(name): Path<String>,
    body: Bytes,
) -> Response {
    match change_status(
        &state,
        &reg,
        &headers,
        &extensions,
        &uri,
        &name,
        None,
        &body,
    )
    .await
    {
        Ok(changed) => Json(json!({
            "updatedCount": changed.len(),
            "servers": changed.iter().map(Entry::response).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(r) => r,
    }
}

async fn health(
    State(state): State<AppState>,
    Extension(reg): Extension<Arc<Registry>>,
) -> Response {
    if let Some(r) = prelude(&state, &reg, false).await {
        return r;
    }
    Json(json!({ "status": "ok" })).into_response()
}

async fn ping(State(state): State<AppState>, Extension(reg): Extension<Arc<Registry>>) -> Response {
    if let Some(r) = prelude(&state, &reg, false).await {
        return r;
    }
    Json(json!({ "pong": true })).into_response()
}

async fn version(
    State(state): State<AppState>,
    Extension(reg): Extension<Arc<Registry>>,
) -> Response {
    if let Some(r) = prelude(&state, &reg, false).await {
        return r;
    }
    Json(json!({
        "version": format!("rustybin-{}", env!("CARGO_PKG_VERSION")),
        "git_commit": crate::control::git_sha().unwrap_or("unknown"),
        "build_time": reg.started.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    }))
    .into_response()
}

/// `/mcp-registry/v0/...`: the same API as `/v0.1` (nested, so the
/// prefix arrives stripped and no path parameters are carried over).
async fn alias(api: Router, mut req: Request) -> Result<Response, std::convert::Infallible> {
    let rest = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_default();
    match format!("{API_PREFIX}{rest}").parse::<Uri>() {
        Ok(u) => *req.uri_mut() = u,
        Err(_) => return Ok(problem(StatusCode::BAD_REQUEST, "invalid path", None)),
    }
    api.oneshot(req).await
}

// ── Control plane ───────────────────────────────────────────────────

/// The control view of the mock registry (`GET /_rustybin/registry`; also
/// the management tool `list_registry_servers`): every entry under the
/// current scenario with its validation, the counts, the scenarios and a
/// gateway's sync configuration.
pub fn status_json(state: &AppState, reg: &Registry, base: &str) -> Value {
    let scenario = Scenario::current(&state.settings);
    let all = all_entries(state, reg, base);
    let mut counts = json!({ "generated": 0, "file": 0, "published": 0, "scenario": 0 });
    let mut shown = Vec::new();
    for e in &all {
        let k = e.source.as_str();
        counts[k] = json!(counts[k].as_u64().unwrap_or(0) + 1);
        // The synthetic pages of `large` are counted, not listed.
        if e.source == Source::Scenario && scenario == Scenario::Large {
            continue;
        }
        let check = validate::validate(&e.server);
        shown.push(json!({
            "name": e.name(),
            "version": e.version(),
            "title": e.server["title"],
            "description": e.server["description"],
            "source": k,
            "status": e.status,
            "isLatest": e.is_latest,
            "updatedAt": e.official()["updatedAt"],
            "valid": check["valid"],
            "issues": check["issues"],
            "server": e.server,
        }));
    }
    let registry_url = format!("{base}{ROOT_PATH}");
    let (_, scenario_source) = state.settings.get("registry.scenario");
    json!({
        "path": ROOT_PATH,
        "api": API_PREFIX,
        "aliases": [ALIAS_PREFIX],
        "base_url": registry_url,
        "scenario": scenario.id(),
        "scenario_source": scenario_source.as_str(),
        "scenarios": SCENARIOS.iter().map(|(id, title)| json!({ "id": id, "title": title })).collect::<Vec<_>>(),
        "schema": validate::SCHEMA_URL,
        "entries": shown,
        "counts": counts,
        "capacity": MAX_USER_ENTRIES,
        "auth_methods": AUTH_METHODS,
        "saved": state.settings.list("registry.saved"),
        "default_registry": browse::DEFAULT_REGISTRY,
        "sync_config": {
            "registries": [{
                "name": "rustybin-mock",
                "type": "mcp-registry",
                "url": registry_url,
                "api_version": "v0.1",
                "list_path": "/v0.1/servers",
                "page_size": 30,
                "filters": { "version": "latest" },
                "sync_interval_seconds": 300,
                "auth": { "type": "none" },
            }]
        },
    })
}

async fn status(
    State(state): State<AppState>,
    Extension(reg): Extension<Arc<Registry>>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let base = base_url(&state, &headers, &extensions, &uri);
    Json(status_json(&state, &reg, &base)).into_response()
}

#[derive(Deserialize, Default)]
struct VersionQuery {
    version: Option<String>,
}

async fn delete_server(
    State(state): State<AppState>,
    Extension(reg): Extension<Arc<Registry>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(q): Query<VersionQuery>,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let removed = reg.delete_published(&name, q.version.as_deref());
    if removed == 0 {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("no published entry {name:?}{}", q.version.as_deref().map(|v| format!(" version {v:?}")).unwrap_or_default()), "reason": "unknown_server" })),
        )
            .into_response();
    }
    Json(json!({ "deleted": name, "versions": removed })).into_response()
}

async fn control_validate(body: Bytes) -> Response {
    if body.len() > validate::MAX_DOC_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "error": "the body is larger than 64 KiB", "reason": "too_large" })),
        )
            .into_response();
    }
    match serde_json::from_slice::<Value>(&body) {
        Ok(doc) => Json(validate::validate(&doc)).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(
                json!({ "error": format!("the body is not JSON: {e}"), "reason": "invalid_body" }),
            ),
        )
            .into_response(),
    }
}

/// POST routes the console scope may call (they change nothing).
pub fn is_console_post(path: &str) -> bool {
    path == VALIDATE_PATH
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/mcp-registry/v0.1/servers", get(list_servers))
        .route(
            "/mcp-registry/v0.1/servers/{serverName}/versions",
            get(server_versions),
        )
        .route(
            "/mcp-registry/v0.1/servers/{serverName}/versions/{version}",
            get(server_version),
        )
        .route(
            "/mcp-registry/v0.1/servers/{serverName}/versions/{version}/status",
            patch(status_one),
        )
        .route(
            "/mcp-registry/v0.1/servers/{serverName}/status",
            patch(status_all),
        )
        .route("/mcp-registry/v0.1/publish", post(publish))
        .route("/mcp-registry/v0.1/validate", post(validate_doc))
        .route("/mcp-registry/v0.1/auth/{method}", post(auth_exchange))
        .route("/mcp-registry/v0.1/health", get(health))
        .route("/mcp-registry/v0.1/ping", get(ping))
        .route("/mcp-registry/v0.1/version", get(version))
}

pub fn router(state: &AppState) -> Router<AppState> {
    let reg = state.registry.clone();
    let api: Router = api_routes()
        .layer(Extension(reg.clone()))
        .with_state(state.clone());
    api_routes()
        .route("/mcp-registry", get(index))
        .nest_service(
            ALIAS_PREFIX,
            tower::service_fn(move |req: Request| alias(api.clone(), req)),
        )
        .route(STATUS_PATH, get(status))
        .route(
            "/_rustybin/registry/servers/{serverName}",
            delete(delete_server),
        )
        .route(VALIDATE_PATH, post(control_validate))
        .route(BROWSE_PATH, get(browse::browse))
        .layer(Extension(reg))
}

pub fn catalog() -> Vec<Endpoint> {
    const GOOD: &str = r#"{"$schema":"https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json","name":"io.example/weather","description":"Weather forecasts","version":"1.0.0","remotes":[{"type":"streamable-http","url":"https://mcp.example.com/mcp"}]}"#;
    vec![
        Endpoint::new(ROOT_PATH, &["GET"], category::MCP, "Mock MCP registry: what it is and where its API lives")
            .example(Example::get("The mock registry", ROOT_PATH)),
        Endpoint::new("/mcp-registry/v0.1/servers", &["GET"], category::MCP, "Mock MCP registry: list servers (one record per version; cursor, limit, search, updated_since, version, include_deleted)")
            .description("This instance's MCP servers (io.rustybin/...), the config file's registry.servers and the published entries, ordered by name then version. The registry.scenario setting breaks it on purpose (empty, large, flaky, slow, stale, invalid, auth_fail).")
            .example(Example::get("Latest versions, a page of 5", "/mcp-registry/v0.1/servers?version=latest&limit=5")),
        Endpoint::new("/mcp-registry/v0.1/servers/{serverName}/versions", &["GET"], category::MCP, "Mock MCP registry: every version of a server (the name URL-encoded)")
            .example(Example::get("Versions of io.rustybin/mcp", "/mcp-registry/v0.1/servers/io.rustybin%2Fmcp/versions")),
        Endpoint::new("/mcp-registry/v0.1/servers/{serverName}/versions/{version}", &["GET"], category::MCP, "Mock MCP registry: one version of a server (latest for the latest)")
            .example(Example::get("The latest io.rustybin/mcp", "/mcp-registry/v0.1/servers/io.rustybin%2Fmcp/versions/latest")),
        Endpoint::new("/mcp-registry/v0.1/servers/{serverName}/versions/{version}/status", &["PATCH"], category::MCP, "Mock MCP registry: change the status of a published version (active, deprecated, deleted; registry token)")
            .example(Example::new("Deprecate without a token", "PATCH", "/mcp-registry/v0.1/servers/io.example%2Fweather/versions/1.0.0/status").json(r#"{"status":"deprecated"}"#).expect_status(401)),
        Endpoint::new("/mcp-registry/v0.1/servers/{serverName}/status", &["PATCH"], category::MCP, "Mock MCP registry: change the status of every published version of a server (registry token)")
            .example(Example::new("Delete without a token", "PATCH", "/mcp-registry/v0.1/servers/io.example%2Fweather/status").json(r#"{"status":"deleted"}"#).expect_status(401)),
        Endpoint::new("/mcp-registry/v0.1/publish", &["POST"], category::MCP, "Mock MCP registry: publish a server.json (Authorization: Bearer <token from auth/*>)")
            .example(Example::post("Publish without a token", "/mcp-registry/v0.1/publish").json(GOOD).expect_status(401)),
        Endpoint::new("/mcp-registry/v0.1/validate", &["POST"], category::MCP, "Mock MCP registry: validate a server.json against the vendored schema ({valid, issues})")
            .example(Example::post("Validate a server.json", "/mcp-registry/v0.1/validate").json(GOOD)),
        Endpoint::new("/mcp-registry/v0.1/auth/{method}", &["POST"], category::MCP, "Mock MCP registry: token exchange stubs (github-at, github-oidc, oidc, dns, http, none) that always succeed, or fail in the auth_fail scenario")
            .example(Example::post("An anonymous registry token", "/mcp-registry/v0.1/auth/none").json("{}")),
        Endpoint::new("/mcp-registry/v0.1/health", &["GET"], category::MCP, "Mock MCP registry: health")
            .example(Example::get("Registry health", "/mcp-registry/v0.1/health")),
        Endpoint::new("/mcp-registry/v0.1/ping", &["GET"], category::MCP, "Mock MCP registry: ping")
            .example(Example::get("Registry ping", "/mcp-registry/v0.1/ping")),
        Endpoint::new("/mcp-registry/v0.1/version", &["GET"], category::MCP, "Mock MCP registry: version")
            .example(Example::get("Registry version", "/mcp-registry/v0.1/version")),
        Endpoint::new("/mcp-registry/v0/{*path}", &["ANY"], category::MCP, "Mock MCP registry: the same API under /v0")
            .example(Example::get("Servers under /v0", "/mcp-registry/v0/servers?limit=3")),
        Endpoint::new(STATUS_PATH, &["GET"], category::CONTROL, "Mock MCP registry: entries with their source and validity, the scenario, the sync snippet")
            .example(Example::get("The mock registry's state", STATUS_PATH)),
        Endpoint::new("/_rustybin/registry/servers/{serverName}", &["DELETE"], category::CONTROL, "Mock MCP registry: remove a published entry (all versions, or ?version=); admin")
            .example(Example::delete("Remove an unknown entry", "/_rustybin/registry/servers/io.example%2Fnone").expect_status(404)),
        Endpoint::new(VALIDATE_PATH, &["POST"], category::CONTROL, "Validate a server.json document against the vendored schema and the registry's semantic checks")
            .example(Example::post("Validate a server.json", VALIDATE_PATH).json(GOOD)),
        Endpoint::new(BROWSE_PATH, &["GET"], category::CONTROL, "Browse an MCP registry (url, search, cursor, limit, version) through the egress policy; entries with their remotes")
            .description("url is a registry base (https://registry.modelcontextprotocol.io by default, its host is allowed for browsing) or /mcp-registry for this instance (in process). Pages are cached for a minute.")
            .example(Example::get("Browse this instance's mock registry", "/_rustybin/registry/browse?url=/mcp-registry&limit=5")),
    ]
}

pub fn openapi_paths() -> Value {
    let tags = json!(["MCP"]);
    let ctl = json!(["Control Plane"]);
    let name = json!({ "name": "serverName", "in": "path", "required": true, "description": "The server name, URL-encoded (io.rustybin%2Fmcp)", "schema": { "type": "string" } });
    let version = json!({ "name": "version", "in": "path", "required": true, "description": "An exact version, or latest", "schema": { "type": "string" } });
    let deleted =
        json!({ "name": "include_deleted", "in": "query", "schema": { "type": "boolean" } });
    let auth_header = json!({ "name": "Authorization", "in": "header", "required": true, "description": "Bearer <registry token from auth/*>", "schema": { "type": "string" } });
    let problem = json!({ "description": "A problem (application/problem+json)", "content": { "application/problem+json": { "schema": { "$ref": "#/components/schemas/RegistryProblem" } } } });
    let server_json = json!({ "$ref": "#/components/schemas/RegistryServerJson" });
    let server_resp = json!({ "description": "A ServerResponse", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/RegistryServerResponse" } } } });
    let server_list = json!({ "description": "A ServerListResponse", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/RegistryServerList" } } } });
    let status_body = json!({ "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["status"], "properties": {
        "status": { "type": "string", "enum": entries::STATUSES }, "statusMessage": { "type": "string", "maxLength": 500 } } } } } });
    let validation = json!({ "description": "{valid, issues: [{type, path, message, severity, reference}]}", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/RegistryValidation" } } } });
    let ok = |d: &str| json!({ "200": { "description": d } });
    json!({
        ROOT_PATH: { "get": { "tags": tags, "summary": "The mock MCP registry", "operationId": "registryIndex", "responses": ok("{name, api, aliases, servers, schema, scenario}") } },
        "/mcp-registry/v0.1/servers": { "get": {
            "tags": tags, "summary": "List MCP servers", "operationId": "registryListServers",
            "description": "One record per server version, ordered by name then version (as text). version=latest for the latest of each. Follow metadata.nextCursor with the same filters.",
            "parameters": [
                { "name": "cursor", "in": "query", "schema": { "type": "string" } },
                { "name": "limit", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 100, "default": 30 } },
                { "name": "updated_since", "in": "query", "schema": { "type": "string", "format": "date-time" } },
                { "name": "search", "in": "query", "description": "Substring of the name, at least 3 characters", "schema": { "type": "string" } },
                { "name": "version", "in": "query", "schema": { "type": "string" } },
                deleted
            ],
            "responses": { "200": server_list, "422": problem, "503": problem }
        } },
        "/mcp-registry/v0.1/servers/{serverName}/versions": { "get": { "tags": tags, "summary": "Every version of a server", "operationId": "registryServerVersions", "parameters": [name, deleted], "responses": { "200": server_list, "404": problem } } },
        "/mcp-registry/v0.1/servers/{serverName}/versions/{version}": { "get": { "tags": tags, "summary": "One version of a server", "operationId": "registryServerVersion", "parameters": [name, version, deleted], "responses": { "200": server_resp, "404": problem } } },
        "/mcp-registry/v0.1/servers/{serverName}/versions/{version}/status": { "patch": { "tags": tags, "summary": "Change the status of a published version", "operationId": "registryVersionStatus", "parameters": [auth_header, name, version], "requestBody": status_body, "responses": { "200": server_resp, "401": problem, "403": problem, "404": problem } } },
        "/mcp-registry/v0.1/servers/{serverName}/status": { "patch": { "tags": tags, "summary": "Change the status of every published version of a server", "operationId": "registryServerStatus", "parameters": [auth_header, name], "requestBody": status_body, "responses": { "200": { "description": "{updatedCount, servers}" }, "401": problem, "403": problem, "404": problem } } },
        "/mcp-registry/v0.1/publish": { "post": {
            "tags": tags, "summary": "Publish a server.json", "operationId": "registryPublish",
            "description": "Validated against the vendored server.json schema and the semantic checks. The io.rustybin namespace is reserved; a duplicate version is refused; at most 256 file and published entries. In public mode the admin token is needed as X-Rustybin-Admin-Token.",
            "parameters": [auth_header],
            "requestBody": { "required": true, "content": { "application/json": { "schema": server_json } } },
            "responses": { "200": server_resp, "400": problem, "401": problem, "403": problem, "409": problem, "422": problem }
        } },
        "/mcp-registry/v0.1/validate": { "post": { "tags": tags, "summary": "Validate a server.json", "operationId": "registryValidate", "requestBody": { "required": true, "content": { "application/json": { "schema": server_json } } }, "responses": { "200": validation, "400": problem } } },
        "/mcp-registry/v0.1/auth/{method}": { "post": {
            "tags": tags, "summary": "Exchange a credential for a registry token (stub)", "operationId": "registryAuth",
            "description": "github-at {github_token}, github-oidc and oidc {oidc_token}, dns and http {domain, timestamp, signed_timestamp}, none {}. Every credential is accepted (nothing is verified), except in the auth_fail scenario, where every exchange answers 401. Tokens last 5 minutes.",
            "parameters": [{ "name": "method", "in": "path", "required": true, "schema": { "type": "string", "enum": AUTH_METHODS } }],
            "requestBody": { "content": { "application/json": { "schema": { "type": "object" } } } },
            "responses": { "200": { "description": "{registry_token, expires_at}" }, "401": problem, "404": problem, "422": problem }
        } },
        "/mcp-registry/v0.1/health": { "get": { "tags": tags, "summary": "Registry health", "operationId": "registryHealth", "responses": ok("{status}") } },
        "/mcp-registry/v0.1/ping": { "get": { "tags": tags, "summary": "Registry ping", "operationId": "registryPing", "responses": ok("{pong}") } },
        "/mcp-registry/v0.1/version": { "get": { "tags": tags, "summary": "Registry version", "operationId": "registryVersion", "responses": ok("{version, git_commit, build_time}") } },
        "/mcp-registry/v0/{path}": {
            "get": { "tags": tags, "summary": "The /v0.1 API under /v0", "operationId": "registryV0Get", "parameters": [{ "name": "path", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": ok("As /v0.1") },
            "post": { "tags": tags, "summary": "The /v0.1 API under /v0", "operationId": "registryV0Post", "parameters": [{ "name": "path", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": ok("As /v0.1") },
            "put": { "tags": tags, "summary": "The /v0.1 API under /v0", "operationId": "registryV0Put", "parameters": [{ "name": "path", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": ok("As /v0.1") },
            "patch": { "tags": tags, "summary": "The /v0.1 API under /v0", "operationId": "registryV0Patch", "parameters": [{ "name": "path", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": ok("As /v0.1") },
            "delete": { "tags": tags, "summary": "The /v0.1 API under /v0", "operationId": "registryV0Delete", "parameters": [{ "name": "path", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": ok("As /v0.1") }
        },
        STATUS_PATH: { "get": { "tags": ctl, "summary": "The mock registry's state", "operationId": "registryState", "responses": ok("{path, api, base_url, scenario, scenarios, entries, counts, capacity, saved, sync_config}") } },
        "/_rustybin/registry/servers/{serverName}": { "delete": { "tags": ctl, "summary": "Remove a published entry (admin)", "operationId": "registryDeleteServer", "parameters": [name, { "name": "version", "in": "query", "schema": { "type": "string" } }], "responses": { "200": { "description": "{deleted, versions}" }, "404": { "description": "No published entry" } } } },
        VALIDATE_PATH: { "post": { "tags": ctl, "summary": "Validate a server.json", "operationId": "registryValidateControl", "requestBody": { "required": true, "content": { "application/json": { "schema": server_json } } }, "responses": { "200": validation, "400": { "description": "Not JSON" } } } },
        BROWSE_PATH: { "get": {
            "tags": ctl, "summary": "Browse an MCP registry", "operationId": "registryBrowse",
            "parameters": [
                { "name": "url", "in": "query", "description": "A registry base URL, or /mcp-registry for this instance", "schema": { "type": "string" } },
                { "name": "search", "in": "query", "schema": { "type": "string" } },
                { "name": "cursor", "in": "query", "schema": { "type": "string" } },
                { "name": "limit", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 100 } },
                { "name": "version", "in": "query", "schema": { "type": "string" } }
            ],
            "responses": { "200": { "description": "{registry, request, status, cached, fetched_at, count, next_cursor, servers: [{name, title, description, version, status, remotes: [{type, url, headers, auth}], packages, valid, issues, server}]}" }, "400": { "description": "Invalid url" }, "403": { "description": "The egress policy refused the host" }, "502": { "description": "The registry failed or did not answer" } }
        } }
    })
}

pub fn openapi_components() -> Value {
    json!({ "schemas": {
        "RegistryProblem": { "type": "object", "properties": {
            "title": { "type": "string" }, "status": { "type": "integer" }, "detail": { "type": "string" },
            "errors": { "type": "array", "items": { "type": "object", "properties": { "message": { "type": "string" }, "location": { "type": "string" }, "value": {} } } } } },
        "RegistryServerJson": { "type": "object", "description": "A server.json document (https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json)", "required": ["name", "description", "version"], "properties": {
            "$schema": { "type": "string", "format": "uri" }, "name": { "type": "string", "pattern": "^[a-zA-Z0-9.-]+/[a-zA-Z0-9._-]+$" },
            "title": { "type": "string" }, "description": { "type": "string", "maxLength": 100 }, "version": { "type": "string" },
            "websiteUrl": { "type": "string", "format": "uri" }, "repository": { "type": "object" },
            "remotes": { "type": "array", "items": { "type": "object" } }, "packages": { "type": "array", "items": { "type": "object" } }, "_meta": { "type": "object" } } },
        "RegistryServerResponse": { "type": "object", "required": ["server", "_meta"], "properties": {
            "server": { "$ref": "#/components/schemas/RegistryServerJson" },
            "_meta": { "type": "object", "properties": { "io.modelcontextprotocol.registry/official": { "type": "object", "properties": {
                "status": { "type": "string", "enum": entries::STATUSES }, "statusMessage": { "type": "string" }, "statusChangedAt": { "type": "string", "format": "date-time" },
                "publishedAt": { "type": "string", "format": "date-time" }, "updatedAt": { "type": "string", "format": "date-time" }, "isLatest": { "type": "boolean" } } } } } } },
        "RegistryServerList": { "type": "object", "required": ["servers", "metadata"], "properties": {
            "servers": { "type": "array", "items": { "$ref": "#/components/schemas/RegistryServerResponse" } },
            "metadata": { "type": "object", "required": ["count"], "properties": { "count": { "type": "integer" }, "nextCursor": { "type": "string" } } } } },
        "RegistryValidation": { "type": "object", "required": ["valid", "issues"], "properties": {
            "valid": { "type": "boolean" },
            "issues": { "type": "array", "items": { "type": "object", "properties": { "type": { "type": "string" }, "path": { "type": "string" }, "message": { "type": "string" }, "severity": { "type": "string", "enum": ["error", "warning"] }, "reference": { "type": "string" } } } } } }
    } })
}

#[cfg(test)]
mod tests;
