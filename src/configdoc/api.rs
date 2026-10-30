//! The configuration document's routes (admin):
//!
//! - `GET /_rustybin/config/export?format=yaml|json|bundle&secrets=refs|omit`:
//!   the instance as a document (a bundle is a gzipped tar of the document
//!   and its spec files).
//! - `POST /_rustybin/config/apply` `{document, prune, dry_run}`: the diff
//!   and each section's result; a dry run changes nothing.
//! - `GET /_rustybin/config/schema`: the JSON Schema of version 2 (also the
//!   MCP resource `rustybin://config.schema.json`).

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{ApplyOptions, SecretsMode};
use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

pub const EXPORT_PATH: &str = "/_rustybin/config/export";
pub const APPLY_PATH: &str = "/_rustybin/config/apply";
pub const SCHEMA_PATH: &str = "/_rustybin/config/schema";

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

#[derive(Deserialize, Default)]
pub struct ExportQuery {
    format: Option<String>,
    secrets: Option<String>,
}

async fn export(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ExportQuery>,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let secrets = match q.secrets.as_deref() {
        None => SecretsMode::Refs,
        Some(s) => match SecretsMode::parse(s) {
            Some(m) => m,
            None => return error(StatusCode::BAD_REQUEST, "secrets must be refs or omit"),
        },
    };
    match q.format.as_deref().unwrap_or("yaml") {
        "yaml" => {
            let export = super::export(&state, secrets, false);
            (
                [(header::CONTENT_TYPE, "application/yaml; charset=utf-8")],
                export.yaml(),
            )
                .into_response()
        }
        "json" => {
            let export = super::export(&state, secrets, false);
            Json(serde_json::to_value(&export.file).unwrap_or(Value::Null)).into_response()
        }
        "bundle" => {
            let export = super::export(&state, secrets, true);
            (
                [
                    (header::CONTENT_TYPE, "application/gzip"),
                    (
                        header::CONTENT_DISPOSITION,
                        "attachment; filename=\"rustybin-config.tar.gz\"",
                    ),
                ],
                super::tar_gz(&export),
            )
                .into_response()
        }
        _ => error(
            StatusCode::BAD_REQUEST,
            "format must be yaml, json or bundle",
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyBody {
    document: Value,
    #[serde(default)]
    prune: bool,
    #[serde(default)]
    dry_run: bool,
}

async fn apply(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let body: ApplyBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!("expected {{document, prune, dry_run}}: {e}"),
            )
        }
    };
    let file = match super::parse_document(&body.document) {
        Ok(f) => f,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let opts = ApplyOptions {
        prune: body.prune,
        dry_run: body.dry_run,
        principal: crate::settings::principal(&headers, &state.config),
        base: None,
    };
    Json(super::apply(&state, file, &opts)).into_response()
}

async fn schema() -> Response {
    (
        [(header::CONTENT_TYPE, "application/schema+json")],
        serde_json::to_string_pretty(&crate::mocks::config_file::json_schema()).unwrap_or_default(),
    )
        .into_response()
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(EXPORT_PATH, get(export))
        .route(APPLY_PATH, post(apply))
        .route(SCHEMA_PATH, get(schema))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            EXPORT_PATH,
            &["GET"],
            category::CONTROL,
            "The whole instance as rustybin.yaml (admin)",
        )
        .description(
            "Version 2 of the configuration file: the instance (title, console user), the stored \
             settings, secret references (never values), every mock with its spec, registry \
             entries, portal items, saved harness scenarios, agent rules and the health toggle. \
             ?format=json for JSON, ?format=bundle for a gzipped tar with the specs as files; \
             ?secrets=omit leaves the references out and redacts the mocks' demo credentials.",
        )
        .example(Example::get("Export the instance", EXPORT_PATH))
        .example(Example::get(
            "Export as JSON without secrets",
            "/_rustybin/config/export?format=json&secrets=omit",
        )),
        Endpoint::new(
            APPLY_PATH,
            &["POST"],
            category::CONTROL,
            "Apply a rustybin.yaml document (admin)",
        )
        .description(
            "{document, prune, dry_run}: the document as YAML or JSON text or a JSON object. A \
             section that is absent is left alone; with prune, a present section is the whole \
             truth for its part of the instance (API mocks, published items and entries, saved \
             scenarios, stored settings it does not name are removed). The answer carries the \
             diff computed first, each section's result and the problems; a dry run changes \
             nothing.",
        )
        .example(
            Example::post("Dry run", APPLY_PATH).json(
                r#"{"document": "version: 2\nhealth: {initial: healthy}\n", "dry_run": true}"#,
            ),
        ),
        Endpoint::new(
            SCHEMA_PATH,
            &["GET"],
            category::CONTROL,
            "JSON Schema of rustybin.yaml (version 2)",
        )
        .example(Example::get("Config schema", SCHEMA_PATH)),
    ]
}

pub fn openapi_paths() -> Value {
    let tags = json!(["Control Plane"]);
    json!({
        EXPORT_PATH: { "get": {
            "tags": tags, "summary": "The whole instance as rustybin.yaml", "operationId": "exportConfig",
            "parameters": [
                { "name": "format", "in": "query", "schema": { "type": "string", "enum": ["yaml", "json", "bundle"], "default": "yaml" } },
                { "name": "secrets", "in": "query", "schema": { "type": "string", "enum": ["refs", "omit"], "default": "refs" }, "description": "refs: ${VAR} and secret:stored references; omit: no secrets section, demo credentials redacted" }
            ],
            "responses": {
                "200": { "description": "The document (application/yaml or application/json) or the bundle (application/gzip)" },
                "401": { "description": "Admin rights required" }
            }
        } },
        APPLY_PATH: { "post": {
            "tags": tags, "summary": "Apply a rustybin.yaml document", "operationId": "applyConfig",
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["document"], "additionalProperties": false, "properties": {
                "document": { "description": "The document: YAML or JSON text, or a JSON object", "oneOf": [{ "type": "string" }, { "type": "object" }] },
                "prune": { "type": "boolean", "default": false, "description": "A present section is the whole truth for its part of the instance" },
                "dry_run": { "type": "boolean", "default": false }
            } } } } },
            "responses": {
                "200": { "description": "{ok, dry_run, prune, sections, changes, summary, results, problems}" },
                "400": { "description": "Not a document" },
                "401": { "description": "Admin rights required" }
            }
        } },
        SCHEMA_PATH: { "get": {
            "tags": tags, "summary": "JSON Schema of rustybin.yaml", "operationId": "getConfigSchema",
            "responses": { "200": { "description": "The schema (application/schema+json)" } }
        } }
    })
}
