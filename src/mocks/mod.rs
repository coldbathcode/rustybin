//! OpenAPI mocks: upload a spec, get a named mock API at `/mock/{name}`.
//!
//! - [`spec`] parses OpenAPI 3.0 / 3.1 into a small model (warnings, not
//!   failures, for odd specs).
//! - [`schema`] generates and validates JSON in the OpenAPI dialect.
//! - [`model`] holds the settings (control API, `rustybin.yaml`, exports).
//! - [`compile`] turns spec + settings into an immutable revision with a
//!   route table; [`checks`] produces its deploy report.
//! - [`respond`], [`request`] and [`security`] decide each response.
//! - [`store`] keeps the mocks (draft, live, history, on/off, captures).
//! - [`config_file`] loads and exports `rustybin.yaml`; [`cli`] is the
//!   `rustybin mock ...` command.
//! - [`handlers`] serves `/mock/{name}/...` (data plane) and
//!   `/_rustybin/mocks*` (control plane).

pub mod capture;
pub mod checks;
pub mod cli;
pub mod collections;
pub mod compile;
pub mod config_file;
pub mod handlers;
pub mod host;
pub mod leads;
pub mod model;
pub mod persist;
pub mod request;
pub mod respond;
pub mod restrict;
pub mod schema;
pub mod security;
pub mod selfservice;
pub mod service;
pub mod spec;
pub mod state;
pub mod store;
pub mod swagger2;
pub mod visits;

use axum::routing::{any, get, post};
use axum::Router;
use serde_json::{json, Value};
use std::path::Path;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::state::AppState;
use model::MockSettings;
use store::{ApplyReport, MockStore, Origin};

/// The built-in sample spec.
pub const SAMPLE_SPEC: &str = include_str!("petstore.yaml");
/// Name of the built-in sample.
pub const SAMPLE_NAME: &str = "petstore";

/// Settings of the built-in sample: `GET /pets/0` answers 404 by a rule.
pub fn sample_settings() -> MockSettings {
    serde_json::from_value(json!({
        "operations": {
            "getPetById": {
                "rules": [{
                    "name": "unknown pet",
                    "when": { "path.petId": { "equals": "0" } },
                    "respond": { "status": 404, "example": "notFound" }
                }]
            }
        }
    }))
    .unwrap_or_default()
}

/// A store for `config`, with the sample when `RUSTYBIN_MOCKS_SAMPLE` is on.
pub fn new_store(config: &Config) -> MockStore {
    new_store_with(
        config,
        std::sync::Arc::new(crate::limits::SystemClock::new()),
        std::sync::Arc::new(crate::metrics::Metrics::new()),
    )
}

/// [`new_store`] on a clock, counting into `metrics` (self-service visits
/// and leads).
pub fn new_store_with(
    config: &Config,
    clock: std::sync::Arc<dyn crate::limits::Clock>,
    metrics: std::sync::Arc<crate::metrics::Metrics>,
) -> MockStore {
    let mut store = MockStore::new(config);
    let ss = &config.self_service;
    if ss.enabled {
        let leads = std::sync::Arc::new(leads::Leads::new(ss, metrics.clone()));
        let mut visits = visits::Visits::new(ss.clone(), clock, leads);
        if let Some(dir) = &config.mocks_state_dir {
            visits = visits.with_persistence(std::path::PathBuf::from(dir));
        }
        metrics.set_mock_visits(visits.running() as i64);
        store = store.with_visits(std::sync::Arc::new(visits));
    }
    if config.mocks_sample {
        match store.compile(SAMPLE_SPEC, sample_settings()) {
            Ok(c) => {
                if let Err(e) = store.create(SAMPLE_NAME, c, Origin::Sample, true, true, true) {
                    tracing::warn!("mock sample not created: {}", e.message());
                }
            }
            Err(e) => tracing::warn!("mock sample does not compile: {e}"),
        }
    }
    // Saved mocks load after the sample (a saved copy of it wins) and before
    // persistence is switched on, so loading writes nothing.
    let Some(dir) = &config.mocks_state_dir else {
        return store;
    };
    let dir = std::path::PathBuf::from(dir);
    let (loaded, problems) = persist::load(&store, &dir);
    if !loaded.is_empty() {
        tracing::info!(
            "saved mocks loaded from {}: {}",
            dir.display(),
            loaded.join(", ")
        );
    }
    for p in problems {
        tracing::warn!("saved mock not loaded: {p}");
        store.record_load_error(format!("saved mock {p}"));
    }
    store.with_persistence(dir)
}

/// Definitions from `RUSTYBIN_CONFIG_FILE` and `RUSTYBIN_MOCKS_DIR`. The
/// flag is false when the file or the directory could not be read at all:
/// a reload then deletes nothing.
fn file_definitions(store: &MockStore, config: &Config) -> (config_file::Loaded, bool) {
    let max_spec = store.limits().max_spec_bytes as u64;
    let mut all = config_file::Loaded::default();
    let mut complete = true;
    if let Some(dir) = &config.mocks_dir {
        match config_file::load_dir(Path::new(dir), max_spec) {
            Ok(l) => all = l,
            Err(e) => {
                all.problems.push(e);
                complete = false;
            }
        }
    }
    if let Some(file) = &config.config_file {
        match config_file::load_file(Path::new(file), |k| std::env::var(k).ok(), max_spec) {
            Ok(l) => {
                // The file wins over a same-named spec from the directory.
                let named = |n: &str| {
                    l.defs.iter().any(|y| y.name == n) || l.failed.iter().any(|(y, _)| y == n)
                };
                all.defs.retain(|x| !named(&x.name));
                all.failed.retain(|(x, _)| !named(x));
                all.defs.extend(l.defs);
                all.failed.extend(l.failed);
                all.problems.extend(l.problems);
            }
            Err(e) => {
                all.problems.push(e);
                complete = false;
            }
        }
    }
    (all, complete)
}

/// (Re)load file-defined mocks: create, update, and delete the file mocks
/// that are gone. A mock whose entry or spec cannot be used keeps its last
/// good revision (or, when new, is created switched off) and shows the
/// error; when the file cannot be read at all nothing is deleted. Problems
/// are logged and kept for the console.
pub fn reload_files(store: &MockStore, config: &Config) -> (ApplyReport, Vec<String>) {
    let (loaded, complete) = file_definitions(store, config);
    let config_file::Loaded {
        defs,
        mut problems,
        mut failed,
    } = loaded;
    if !complete {
        problems.push(
            "the mocks file or directory could not be read: file mocks were kept as they are"
                .into(),
        );
    }
    let keep: Vec<String> = failed.iter().map(|(n, _)| n.clone()).collect();
    let prune = complete.then_some(Origin::File);
    let mut report = store.apply_keeping(defs, Origin::File, prune, &keep);
    for e in &report.errors {
        let name = e["name"].as_str().unwrap_or("?");
        let error = e["error"].as_str().unwrap_or("?");
        problems.push(format!("mock {name}: {error}"));
        if e.get("deployed").is_none() {
            failed.push((name.to_string(), error.to_string()));
        }
    }
    for (name, error) in &failed {
        store.mark_failed(name, Origin::File, error);
    }
    store.clear_load_errors();
    for p in &problems {
        tracing::warn!("mocks: {p}");
        store.record_load_error(p.clone());
    }
    report.errors.clear();
    (report, problems)
}

/// Load file-defined mocks at startup (a no-op without files configured).
pub fn load_startup(state: &AppState) {
    let config = &state.config;
    if config.config_file.is_none() && config.mocks_dir.is_none() {
        return;
    }
    let (report, _) = reload_files(&state.mocks, config);
    if !report.created.is_empty() {
        tracing::info!("mocks loaded: {}", report.created.join(", "));
    }
}

pub fn router(_state: &AppState) -> Router<AppState> {
    use handlers as h;
    use selfservice as ss;
    Router::new()
        .route("/mock/{name}", any(h::mock_root))
        .route("/mock/{name}/_rustybin/openapi.json", get(h::spec_json))
        .route("/mock/{name}/_rustybin/openapi.yaml", get(h::spec_yaml))
        .route("/mock/{name}/{*path}", any(h::mock_sub))
        .route("/_rustybin/mocks", get(h::list).post(h::create))
        .route("/_rustybin/mocks/import", post(h::import))
        .route("/_rustybin/mocks/apply", post(h::apply))
        .route("/_rustybin/mocks/reload", post(h::reload))
        .route("/_rustybin/mocks/export", get(h::export_all))
        .route("/_rustybin/mocks/config.schema.json", get(h::config_schema))
        .route(
            "/_rustybin/mocks/visit",
            get(ss::get_visit)
                .post(ss::start_visit)
                .delete(ss::end_visit),
        )
        .route("/_rustybin/mocks/visit/extend", post(ss::extend_visit))
        .route("/_rustybin/mocks/visit/token", post(ss::rotate_token))
        .route(
            "/_rustybin/mocks/visits",
            get(ss::list_visits).delete(ss::end_all_visits),
        )
        .route("/_rustybin/mocks/visits/pause", post(ss::pause))
        .route("/_rustybin/mocks/visits/resume", post(ss::resume))
        .route(
            "/_rustybin/mocks/visits/{id}",
            axum::routing::delete(ss::end_one_visit),
        )
        .route(
            "/_rustybin/mocks/{name}",
            get(h::get_one).patch(h::patch).delete(h::delete),
        )
        .route("/_rustybin/mocks/{name}/enable", post(h::enable))
        .route("/_rustybin/mocks/{name}/disable", post(h::disable))
        .route("/_rustybin/mocks/{name}/deploy", post(h::deploy))
        .route("/_rustybin/mocks/{name}/rollback", post(h::rollback))
        .route("/_rustybin/mocks/{name}/report", get(h::report))
        .route("/_rustybin/mocks/{name}/export", get(h::export_one))
        .route("/_rustybin/mocks/{name}/spec", get(h::spec_source))
        .route("/_rustybin/mocks/{name}/duplicate", post(h::duplicate))
        .route(
            "/_rustybin/mocks/{name}/state",
            get(h::get_state).delete(h::clear_state),
        )
        .route(
            "/_rustybin/mocks/{name}/collections/{format}",
            get(h::collection),
        )
        .route(
            "/_rustybin/mocks/{name}/operations/{op}",
            get(h::get_operation).put(h::put_operation),
        )
        .route(
            "/_rustybin/mocks/{name}/operations/{op}/sample",
            get(h::sample),
        )
        .route(
            "/_rustybin/mocks/{name}/operations/{op}/validate",
            post(h::validate),
        )
        .route(
            "/_rustybin/mocks/{name}/operations/{op}/rules/test",
            post(h::test_rules),
        )
        .route(
            "/_rustybin/mocks/{name}/requests",
            get(h::requests).delete(h::clear_requests),
        )
        .route("/_rustybin/mocks/{name}/requests/stream", get(h::stream))
        .route(
            "/_rustybin/mocks/{name}/requests/{n}",
            get(h::request_entry),
        )
}

// ── Catalogue ───────────────────────────────────────────────────────

const SMALL_SPEC: &str = r#"{"spec":{"openapi":"3.0.3","info":{"title":"Orders","version":"1"},"paths":{"/orders/{id}":{"get":{"operationId":"getOrder","parameters":[{"name":"id","in":"path","required":true,"schema":{"type":"integer"}}],"responses":{"200":{"description":"ok","content":{"application/json":{"schema":{"type":"object","properties":{"id":{"type":"integer"},"total":{"type":"number"}}}}}}}}}}}}"#;

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/mock/{name}",
            &["ANY"],
            category::MOCKS,
            "Call an OpenAPI mock (its root path)",
        )
        .description(
            "Mocks are created from an OpenAPI 3.0 / 3.1 spec in the console (/ui/#/mocks), with \
             POST /_rustybin/mocks or from rustybin.yaml. Unknown mocks answer 404; switched off \
             mocks answer 503 (configurable). Every call is captured.",
        )
        .example(Example::get("Root of the sample mock (no operation, 404)", "/mock/petstore").expect_status(404)),
        Endpoint::new(
            "/mock/{name}/{*path}",
            &["ANY"],
            category::MOCKS,
            "Call an operation of an OpenAPI mock",
        )
        .description(
            "The path below the mock root is matched against the spec's paths (with the servers[0] \
             path prefix, e.g. /v1, unless strip_server_prefix is set). The response comes from rules, \
             overrides, spec examples or data generated from the schema. `Prefer: code=404`, \
             `Prefer: example=name` and `Prefer: dynamic=true` (or X-Rustybin-Mock-Status / \
             X-Rustybin-Mock-Example) pick a documented response. Requests are validated (warn by \
             default: X-Rustybin-Mock-Validation: failed) and mocked security applies when enabled. \
             Responses carry X-Rustybin-Mock, X-Rustybin-Mock-Operation and X-Rustybin-Mock-Source.",
        )
        .example(Example::get("List pets (sample mock)", "/mock/petstore/v1/pets"))
        .example(Example::get("Get a pet (id echoed)", "/mock/petstore/v1/pets/42"))
        .example(Example::get("Unknown pet (rule: 404)", "/mock/petstore/v1/pets/0").expect_status(404))
        .example(
            Example::get("Pick a documented response", "/mock/petstore/v1/pets/1")
                .header("Prefer", "code=404")
                .expect_status(404),
        )
        .example(
            Example::post("Create a pet", "/mock/petstore/v1/pets")
                .json(r#"{"name":"Rex","tag":"dog"}"#)
                .expect_status(201),
        ),
        Endpoint::new(
            "/mock/{name}/_rustybin/openapi.json",
            &["GET"],
            category::MOCKS,
            "The deployed spec of a mock, servers rewritten to the mock URL",
        )
        .description(
            "Ready to import into a gateway or client: servers point at the mock and, with mocked \
             security on, OAuth2 / OpenID Connect URLs point at the built-in IdP.",
        )
        .example(Example::get("Sample mock spec", "/mock/petstore/_rustybin/openapi.json")),
        Endpoint::new(
            "/mock/{name}/_rustybin/openapi.yaml",
            &["GET"],
            category::MOCKS,
            "The deployed spec of a mock as YAML",
        )
        .example(Example::get("Sample mock spec (YAML)", "/mock/petstore/_rustybin/openapi.yaml")),
        Endpoint::new(
            "/_rustybin/mocks",
            &["GET", "POST"],
            category::MOCKS,
            "List mocks (GET) or create one from a spec (POST, admin)",
        )
        .description(
            "POST {spec (text or object), name?, enabled?, deploy? (default true), force?, \
             strip_server_prefix?, responses?, security?, operations?}. The name defaults to a slug of \
             info.title. Deploying runs the checks; errors answer 422 with the report unless force is set.",
        )
        .example(Example::get("List mocks", "/_rustybin/mocks"))
        .example(Example::post("Create a mock", "/_rustybin/mocks").json(SMALL_SPEC).expect_status(201)),
        Endpoint::new(
            "/_rustybin/mocks/import",
            &["POST"],
            category::MOCKS,
            "Preview a spec: operations, security schemes, warnings, suggested name",
        )
        .description("Body: {\"spec\": ...} as JSON, or the spec itself (JSON or YAML). Creates nothing.")
        .example(Example::post("Preview a spec", "/_rustybin/mocks/import").json(SMALL_SPEC)),
        Endpoint::new(
            "/_rustybin/mocks/apply",
            &["POST"],
            category::MOCKS,
            "Apply a rustybin.yaml document (create, update, ?prune=true deletes the rest)",
        )
        .description(
            "Mocks must use spec_inline (rustybin mock push inlines spec files). Changed mocks are \
             deployed even with check errors, which are listed in the result.",
        )
        .example(
            Example::post("Apply an empty document", "/_rustybin/mocks/apply")
                .json(r#"{"version":1,"mocks":[]}"#),
        ),
        Endpoint::new(
            "/_rustybin/mocks/reload",
            &["POST"],
            category::MOCKS,
            "Re-read RUSTYBIN_CONFIG_FILE and RUSTYBIN_MOCKS_DIR (also on SIGHUP)",
        )
        .example(Example::post("Reload mocks from disk", "/_rustybin/mocks/reload")),
        Endpoint::new(
            "/_rustybin/mocks/export",
            &["GET"],
            category::MOCKS,
            "Every mock (except the sample) as a rustybin.yaml document",
        )
        .example(Example::get("Export all mocks", "/_rustybin/mocks/export")),
        Endpoint::new(
            "/_rustybin/mocks/config.schema.json",
            &["GET"],
            category::MOCKS,
            "JSON Schema of rustybin.yaml (editor completion)",
        )
        .example(Example::get("Config file schema", "/_rustybin/mocks/config.schema.json")),
        Endpoint::new(
            "/_rustybin/mocks/visit",
            &["GET", "POST", "DELETE"],
            category::MOCKS,
            "Self-service: the caller's visit (GET), start one (POST) or end it (DELETE)",
        )
        .description(
            "With RUSTYBIN_MOCKS_SELF_SERVICE on a public instance, visitors create mocks without the \
             admin token. A visit owns them through its manage token (X-Rustybin-Mock-Token, returned \
             once by POST here or by the first POST /_rustybin/mocks) and lasts a time allowance: one \
             hour, extendable to the anonymous hours, then with an email address up to the daily \
             hours. Off: 404 rustybin_self_service_off.",
        )
        .example(
            Example::get("The caller's visit (self-service off: 404)", "/_rustybin/mocks/visit")
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/mocks/visit/extend",
            &["POST"],
            category::MOCKS,
            "Self-service: one more period for the visit (optionally with the email step)",
        )
        .example(
            Example::post("Extend the visit (self-service off: 404)", "/_rustybin/mocks/visit/extend")
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/mocks/visit/token",
            &["POST"],
            category::MOCKS,
            "Self-service: replace the visit's manage token",
        )
        .example(
            Example::post("Rotate the token (self-service off: 404)", "/_rustybin/mocks/visit/token")
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/mocks/visits",
            &["GET", "DELETE"],
            category::MOCKS,
            "Self-service operators: every running visit (GET) or end them all (DELETE)",
        )
        .example(
            Example::get("Running visits (self-service off: 404)", "/_rustybin/mocks/visits")
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/mocks/visits/pause",
            &["POST"],
            category::MOCKS,
            "Self-service operators: stop new visits and visitor mocks",
        )
        .example(
            Example::post("Pause self-service (off: 404)", "/_rustybin/mocks/visits/pause")
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/mocks/visits/resume",
            &["POST"],
            category::MOCKS,
            "Self-service operators: allow new visits again",
        )
        .example(
            Example::post("Resume self-service (off: 404)", "/_rustybin/mocks/visits/resume")
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/mocks/visits/{id}",
            &["DELETE"],
            category::MOCKS,
            "Self-service operators: end one visit and delete its mocks",
        )
        .example(
            Example::delete("End a visit (self-service off: 404)", "/_rustybin/mocks/visits/0123456789abcdef")
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/mocks/{name}",
            &["GET", "PATCH", "DELETE"],
            category::MOCKS,
            "A mock's details (GET), edit its draft (PATCH) or delete it",
        )
        .description(
            "PATCH {name?, spec?, strip_server_prefix?, responses?, security?, operations?, deploy?, \
             force?} replaces the given sections of the draft; deploy publishes it.",
        )
        .example(Example::get("Sample mock details", "/_rustybin/mocks/petstore"))
        .example(Example::delete("Delete a mock", "/_rustybin/mocks/missing").expect_status(404)),
        Endpoint::new(
            "/_rustybin/mocks/{name}/enable",
            &["POST"],
            category::MOCKS,
            "Switch a mock on",
        )
        .example(Example::post("Switch on", "/_rustybin/mocks/missing/enable").expect_status(404)),
        Endpoint::new(
            "/_rustybin/mocks/{name}/disable",
            &["POST"],
            category::MOCKS,
            "Switch a mock off (its URL answers 503, config and history are kept)",
        )
        .example(Example::post("Switch off", "/_rustybin/mocks/missing/disable").expect_status(404)),
        Endpoint::new(
            "/_rustybin/mocks/{name}/deploy",
            &["POST"],
            category::MOCKS,
            "Publish the draft after the deploy checks (?force=true ignores errors)",
        )
        .example(Example::post("Deploy", "/_rustybin/mocks/missing/deploy").expect_status(404)),
        Endpoint::new(
            "/_rustybin/mocks/{name}/rollback",
            &["POST"],
            category::MOCKS,
            "Make a previous revision live again (body {revision} optional)",
        )
        .example(Example::post("Roll back", "/_rustybin/mocks/missing/rollback").expect_status(404)),
        Endpoint::new(
            "/_rustybin/mocks/{name}/report",
            &["GET"],
            category::MOCKS,
            "Deploy-check report of the draft: a valid request and response per operation",
        )
        .example(Example::get("Sample mock report", "/_rustybin/mocks/petstore/report")),
        Endpoint::new(
            "/_rustybin/mocks/{name}/export",
            &["GET"],
            category::MOCKS,
            "One mock as a rustybin.yaml document",
        )
        .example(Example::get("Export the sample", "/_rustybin/mocks/petstore/export")),
        Endpoint::new(
            "/_rustybin/mocks/{name}/spec",
            &["GET"],
            category::MOCKS,
            "The spec as uploaded (the draft's, JSON or YAML as written)",
        )
        .example(Example::get("Sample spec source", "/_rustybin/mocks/petstore/spec")),
        Endpoint::new(
            "/_rustybin/mocks/{name}/duplicate",
            &["POST"],
            category::MOCKS,
            "Copy a mock under a new name (body {name} optional; default <name>-copy)",
        )
        .example(Example::post("Duplicate", "/_rustybin/mocks/missing/duplicate").expect_status(404)),
        Endpoint::new(
            "/_rustybin/mocks/{name}/state",
            &["GET", "DELETE"],
            category::MOCKS,
            "Items stored by a mock in stateful mode (DELETE resets them)",
        )
        .example(Example::get("Sample mock state", "/_rustybin/mocks/petstore/state")),
        Endpoint::new(
            "/_rustybin/mocks/{name}/collections/{format}",
            &["GET"],
            category::MOCKS,
            "The mock's operations as a collection: postman, hurl, curl, http or k6 (?base_url=)",
        )
        .example(Example::get("Sample as Hurl", "/_rustybin/mocks/petstore/collections/hurl")),
        Endpoint::new(
            "/_rustybin/mocks/{name}/operations/{op}",
            &["GET", "PUT"],
            category::MOCKS,
            "One operation (schemas inlined, settings, sample request) or set its settings (PUT)",
        )
        .description(
            "PUT {respond?, rules?, mode?, delay_ms?, public?} replaces the operation's settings in \
             the draft (null clears them).",
        )
        .example(Example::get("Sample operation", "/_rustybin/mocks/petstore/operations/getPetById")),
        Endpoint::new(
            "/_rustybin/mocks/{name}/operations/{op}/sample",
            &["GET"],
            category::MOCKS,
            "Generate a valid request and response (?status=, ?mode=, ?example=, ?seed=)",
        )
        .example(Example::get(
            "Generated 200 for getPetById",
            "/_rustybin/mocks/petstore/operations/getPetById/sample?mode=generated",
        )),
        Endpoint::new(
            "/_rustybin/mocks/{name}/operations/{op}/validate",
            &["POST"],
            category::MOCKS,
            "Validate a response (or request) body against the operation's schema",
        )
        .description("Body: {kind?: response|request, status?, body}. Returns the issues with JSON pointers.")
        .example(
            Example::post("Validate a pet", "/_rustybin/mocks/petstore/operations/getPetById/validate")
                .json(r#"{"status":200,"body":{"id":"x","name":"Rex"}}"#),
        ),
        Endpoint::new(
            "/_rustybin/mocks/{name}/operations/{op}/rules/test",
            &["POST"],
            category::MOCKS,
            "Which rule would answer a captured call (body {rules?, request?})",
        )
        .description(
            "Rules default to the operation's rules in the draft, the request to its latest captured \
             call. Returns each rule's conditions with the value seen and the winner.",
        )
        .example(Example::post(
            "Test the sample rules",
            "/_rustybin/mocks/petstore/operations/getPetById/rules/test",
        )),
        Endpoint::new(
            "/_rustybin/mocks/{name}/requests",
            &["GET", "DELETE"],
            category::MOCKS,
            "Requests a mock received, newest first (DELETE clears them)",
        )
        .example(Example::get("Sample mock traffic", "/_rustybin/mocks/petstore/requests")),
        Endpoint::new(
            "/_rustybin/mocks/{name}/requests/stream",
            &["GET"],
            category::MOCKS,
            "Live feed of a mock's requests (SSE)",
        )
        .sse()
        .example(
            Example::get("Stream sample mock traffic", "/_rustybin/mocks/petstore/requests/stream")
                .skip_check("never-ending SSE stream"),
        ),
        Endpoint::new(
            "/_rustybin/mocks/{name}/requests/{n}",
            &["GET"],
            category::MOCKS,
            "One captured request by sequence number",
        )
        .example(Example::get("First captured request", "/_rustybin/mocks/missing/requests/1").expect_status(404)),
    ]
}

// ── OpenAPI ─────────────────────────────────────────────────────────

pub fn openapi_paths() -> Value {
    let tags = json!(["Mocks"]);
    let name = json!({ "name": "name", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Mock name" });
    let op = json!({ "name": "op", "in": "path", "required": true, "schema": { "type": "string" }, "description": "operationId" });
    let path = json!({ "name": "path", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Path below the mock root" });
    let n = json!({ "name": "n", "in": "path", "required": true, "schema": { "type": "integer" } });
    let not_found = json!({ "description": "Unknown mock" });
    let simple = |id: &str, summary: &str, params: Value| {
        json!({
            "tags": tags, "summary": summary, "operationId": id, "parameters": params,
            "responses": { "200": { "description": "OK" }, "404": not_found }
        })
    };
    let admin = |id: &str, summary: &str, params: Value| {
        json!({
            "tags": tags, "summary": summary, "operationId": id, "parameters": params,
            "responses": {
                "200": { "description": "OK" },
                "401": { "description": "Control-plane credentials required" },
                "403": { "description": "Admin token or admin scope required" },
                "404": not_found
            }
        })
    };
    let json_body = |schema: Value| json!({ "required": true, "content": { "application/json": { "schema": schema } } });
    let token = json!({ "name": "X-Rustybin-Mock-Token", "in": "header", "required": false, "schema": { "type": "string" }, "description": "Manage token of the visit (self-service)" });
    let visit_op = |id: &str, summary: &str| {
        json!({
            "tags": tags, "summary": summary, "operationId": id, "parameters": [token],
            "responses": {
                "200": { "description": "The visit" },
                "201": { "description": "Visit started: manage_token (shown once) and the visit" },
                "403": { "description": "rustybin_email_required: give an email address" },
                "404": { "description": "Self-service off, or no running visit for the token" },
                "409": { "description": "rustybin_extend_too_early" },
                "429": { "description": "rustybin_daily_limit or rustybin_visit_limit (Retry-After)" },
                "503": { "description": "rustybin_mocks_busy or rustybin_self_service_paused (Retry-After)" }
            }
        })
    };
    let email_schema = json!({ "type": "object", "properties": { "email": { "type": "string", "format": "email" }, "marketing_consent": { "type": "boolean" } } });
    let mock_call = |id: &str, params: Value| {
        json!({
            "tags": tags,
            "summary": "Call a mock",
            "operationId": id,
            "parameters": params,
            "requestBody": { "required": false, "content": { "*/*": { "schema": {} } } },
            "responses": {
                "200": { "description": "The mocked response (status, headers and body follow the mock's spec)" },
                "400": { "description": "Request does not match the spec (validation enforce)" },
                "401": { "description": "Mocked security rejected the request" },
                "404": { "description": "Unknown mock or no matching operation" },
                "405": { "description": "Method not documented for this path" },
                "503": { "description": "Mock switched off or not deployed" }
            }
        })
    };
    let mut create = admin(
        "createMock",
        "Create a mock from an OpenAPI spec",
        json!([]),
    );
    if let Some(o) = create.as_object_mut() {
        o.insert(
            "requestBody".into(),
            json_body(json!({
                "type": "object",
                "required": ["spec"],
                "properties": {
                    "spec": { "description": "OpenAPI 3.0/3.1 document (text or object)" },
                    "name": { "type": "string", "pattern": "^[a-z0-9]([a-z0-9-]{0,38}[a-z0-9])?$" },
                    "enabled": { "type": "boolean" },
                    "deploy": { "type": "boolean", "default": true },
                    "force": { "type": "boolean" },
                    "strip_server_prefix": { "type": "boolean" },
                    "responses": { "type": "object" },
                    "security": { "type": "object" },
                    "operations": { "type": "object" }
                }
            })),
        );
        o.insert(
            "responses".into(),
            json!({
                "201": { "description": "Created (details)" },
                "400": { "description": "Invalid spec or settings" },
                "409": { "description": "Name taken" },
                "422": { "description": "Deploy checks failed (report)" }
            }),
        );
    }
    let with_body = |mut op: Value, schema: Value| {
        if let Some(o) = op.as_object_mut() {
            o.insert("requestBody".into(), json_body(schema));
        }
        op
    };
    let mut paths = json!({
        "/mock/{name}": { "post": mock_call("callMock", json!([name])) },
        "/mock/{name}/{path}": { "post": mock_call("callMockPath", json!([name, path])) },
        "/mock/{name}/_rustybin/openapi.json": { "get": simple("getMockSpec", "Deployed spec (JSON)", json!([name])) },
        "/mock/{name}/_rustybin/openapi.yaml": { "get": simple("getMockSpecYaml", "Deployed spec (YAML)", json!([name])) },
        "/_rustybin/mocks": {
            "get": simple("listMocks", "List mocks", json!([])),
            "post": create
        },
        "/_rustybin/mocks/import": { "post": with_body(simple("previewMockSpec", "Preview a spec", json!([])), json!({ "type": "object", "properties": { "spec": {} } })) },
        "/_rustybin/mocks/apply": { "post": admin("applyMocks", "Apply a rustybin.yaml document", json!([{ "name": "prune", "in": "query", "schema": { "type": "boolean" } }])) },
        "/_rustybin/mocks/reload": { "post": admin("reloadMocks", "Reload file-defined mocks", json!([])) },
        "/_rustybin/mocks/export": { "get": simple("exportMocks", "Export mocks as YAML", json!([])) },
        "/_rustybin/mocks/config.schema.json": { "get": simple("getMocksConfigSchema", "JSON Schema of rustybin.yaml", json!([])) },
        "/_rustybin/mocks/{name}": {
            "get": simple("getMock", "Mock details", json!([name])),
            "patch": with_body(admin("updateMock", "Edit the draft", json!([name])), json!({ "type": "object" })),
            "delete": admin("deleteMock", "Delete a mock", json!([name]))
        },
        "/_rustybin/mocks/{name}/enable": { "post": admin("enableMock", "Switch on", json!([name])) },
        "/_rustybin/mocks/{name}/disable": { "post": admin("disableMock", "Switch off", json!([name])) },
        "/_rustybin/mocks/{name}/deploy": { "post": admin("deployMock", "Deploy the draft", json!([name, { "name": "force", "in": "query", "schema": { "type": "boolean" } }])) },
        "/_rustybin/mocks/{name}/rollback": { "post": admin("rollbackMock", "Roll back", json!([name])) },
        "/_rustybin/mocks/{name}/report": { "get": simple("getMockReport", "Deploy-check report", json!([name])) },
        "/_rustybin/mocks/{name}/export": { "get": simple("exportMock", "Export one mock", json!([name])) },
        "/_rustybin/mocks/{name}/spec": { "get": simple("getMockSpecSource", "The spec as uploaded", json!([name])) },
        "/_rustybin/mocks/{name}/duplicate": { "post": with_body(admin("duplicateMock", "Copy a mock", json!([name])), json!({ "type": "object", "properties": { "name": { "type": "string" } } })) },
        "/_rustybin/mocks/{name}/state": {
            "get": simple("getMockState", "Stored items (stateful mode)", json!([name])),
            "delete": admin("clearMockState", "Reset stored items", json!([name]))
        },
        "/_rustybin/mocks/{name}/collections/{format}": { "get": simple("getMockCollection", "The mock as a collection", json!([name,
            { "name": "format", "in": "path", "required": true, "schema": { "type": "string", "enum": ["postman", "hurl", "curl", "http", "k6"] } },
            { "name": "base_url", "in": "query", "schema": { "type": "string" } }])) },
        "/_rustybin/mocks/{name}/operations/{op}/rules/test": { "post": with_body(simple("testMockRules", "Which rule would answer a captured call", json!([name, op])), json!({
            "type": "object", "properties": { "rules": { "type": "array" }, "request": { "type": "integer" } }
        })) },
        "/_rustybin/mocks/{name}/operations/{op}": {
            "get": simple("getMockOperation", "Operation details", json!([name, op])),
            "put": with_body(admin("setMockOperation", "Set operation settings", json!([name, op])), json!({ "type": "object" }))
        },
        "/_rustybin/mocks/{name}/operations/{op}/sample": { "get": simple("sampleMockOperation", "Generate a valid request and response", json!([name, op,
            { "name": "status", "in": "query", "schema": { "type": "integer" } },
            { "name": "mode", "in": "query", "schema": { "type": "string", "enum": ["examples", "generated", "static", "stateful"] } },
            { "name": "example", "in": "query", "schema": { "type": "string" } },
            { "name": "seed", "in": "query", "schema": { "type": "integer" } }])) },
        "/_rustybin/mocks/{name}/operations/{op}/validate": { "post": with_body(simple("validateMockBody", "Validate a body", json!([name, op])), json!({
            "type": "object", "required": ["body"],
            "properties": { "kind": { "type": "string", "enum": ["response", "request"] }, "status": { "type": "integer" }, "body": {} }
        })) },
        "/_rustybin/mocks/{name}/requests": {
            "get": simple("listMockRequests", "Captured requests", json!([name, { "name": "limit", "in": "query", "schema": { "type": "integer" } }])),
            "delete": admin("clearMockRequests", "Clear captured requests", json!([name]))
        },
        "/_rustybin/mocks/{name}/requests/stream": { "get": {
            "tags": tags, "summary": "Live feed of captured requests (SSE)", "operationId": "streamMockRequests",
            "parameters": [name],
            "responses": { "200": { "description": "text/event-stream of `request` events", "content": { "text/event-stream": {} } }, "404": not_found }
        } },
        "/_rustybin/mocks/{name}/requests/{n}": { "get": simple("getMockRequest", "One captured request", json!([name, n])) }
    });
    let visits = json!({
        "/_rustybin/mocks/visit": {
            "get": visit_op("getMockVisit", "The caller's self-service visit"),
            "post": with_body(visit_op("startMockVisit", "Start a self-service visit"), email_schema.clone()),
            "delete": visit_op("endMockVisit", "End the visit and delete its mocks")
        },
        "/_rustybin/mocks/visit/extend": { "post": with_body(visit_op("extendMockVisit", "One more period (optionally with the email step)"), email_schema.clone()) },
        "/_rustybin/mocks/visit/token": { "post": visit_op("rotateMockVisitToken", "Replace the manage token") },
        "/_rustybin/mocks/visits": {
            "get": admin("listMockVisits", "Running visits (operators)", json!([])),
            "delete": admin("endAllMockVisits", "End every visit (operators)", json!([]))
        },
        "/_rustybin/mocks/visits/pause": { "post": admin("pauseMockVisits", "Stop new visits (operators)", json!([])) },
        "/_rustybin/mocks/visits/resume": { "post": admin("resumeMockVisits", "Allow new visits (operators)", json!([])) },
        "/_rustybin/mocks/visits/{id}": { "delete": admin("endMockVisit", "End one visit (operators)", json!([{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Visit id from GET /_rustybin/mocks/visits" }])) }
    });
    if let (Value::Object(all), Value::Object(more)) = (&mut paths, visits) {
        all.extend(more);
    }
    paths
}

#[cfg(test)]
mod tests;
