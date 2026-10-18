//! HTTP handlers: the mocks themselves (`/mock/{name}/...`, data plane) and
//! their management API (`/_rustybin/mocks*`, control plane: reads need the
//! `console` scope, mutations the admin token or `admin` scope).

use axum::body::Bytes;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, Extensions, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use super::capture::{MockCapture, OpStats, MAX_RESPONSE_PREVIEW};
use super::checks;
use super::compile::{Compiled, RouteMatch};
use super::config_file;
use super::host;
use super::model::{
    MockSettings, OperationSettings, ResponseMode, ResponseSettings, SecuritySettings,
    ValidationMode,
};
use super::request;
use super::respond::{self, BodyChoice, RequestView};
use super::schema::{self, Direction};
use super::security::{AuthContext, AuthResult};
use super::spec;
use super::store::{Lookup, MockStore, Origin, Snapshot, StoreError};
use crate::config::Config;
use crate::control_auth::ControlAuthorized;
use crate::request_bin::BinHeader;
use crate::server::TlsConnectionInfo;
use crate::session::ClientIp;
use crate::state::AppState;

/// Names that would collide with fixed control-plane routes.
pub const RESERVED_NAMES: &[&str] = &["import", "export", "apply", "reload"];

// ── Helpers ─────────────────────────────────────────────────────────

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn problem(status: u16, detail: &str, extra: Value) -> Response {
    let mut body = respond::problem(status, detail);
    if let (Some(obj), Value::Object(more)) = (body.as_object_mut(), extra) {
        obj.extend(more);
    }
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut resp = (code, Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    resp
}

fn store_error(e: StoreError) -> Response {
    let message = e.message();
    match e {
        StoreError::NotFound(_) => json_error(StatusCode::NOT_FOUND, &message),
        StoreError::Conflict(_) => json_error(StatusCode::CONFLICT, &message),
        StoreError::Invalid(_) => json_error(StatusCode::BAD_REQUEST, &message),
        StoreError::Full(_) => json_error(StatusCode::SERVICE_UNAVAILABLE, &message),
        StoreError::ChecksFailed(report) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": message, "report": report })),
        )
            .into_response(),
    }
}

fn base_url(headers: &HeaderMap, extensions: &Extensions, uri: &Uri, config: &Config) -> String {
    crate::session::request_origin(headers, extensions, uri, config).base_url()
}

fn query_pairs(uri: &Uri) -> Vec<(String, String)> {
    uri.query()
        .map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect())
        .unwrap_or_default()
}

fn header_list(headers: &HeaderMap) -> Vec<BinHeader> {
    headers
        .iter()
        .map(|(k, v)| BinHeader {
            name: k.as_str().to_string(),
            value: String::from_utf8_lossy(v.as_bytes()).into_owned(),
        })
        .collect()
}

/// Read the spec field of a request: text, or a JSON/YAML object.
fn spec_text(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Object(_) => serde_json::to_string(v).map_err(|e| e.to_string()),
        _ => Err("spec must be the document text or a JSON object".into()),
    }
}

fn check_name(name: &str) -> Result<(), StoreError> {
    if RESERVED_NAMES.contains(&name) {
        return Err(StoreError::Invalid(format!("{name:?} is a reserved name")));
    }
    Ok(())
}

// ── Data plane ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct MockPath {
    name: String,
}

#[derive(Deserialize)]
pub struct MockSubPath {
    name: String,
    #[allow(dead_code)]
    path: String,
}

#[allow(clippy::too_many_arguments)]
pub async fn mock_root(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    ClientIp(ip): ClientIp,
    tls: Option<Extension<TlsConnectionInfo>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    serve(
        state,
        name,
        ip,
        tls.map(|t| t.0),
        method,
        uri,
        headers,
        body,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn mock_sub(
    State(state): State<AppState>,
    Path(MockSubPath { name, .. }): Path<MockSubPath>,
    ClientIp(ip): ClientIp,
    tls: Option<Extension<TlsConnectionInfo>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    serve(
        state,
        name,
        ip,
        tls.map(|t| t.0),
        method,
        uri,
        headers,
        body,
    )
    .await
}

/// Everything a capture needs besides the outcome.
struct Incoming<'a> {
    method: &'a Method,
    uri: &'a Uri,
    sub_path: &'a str,
    headers: &'a HeaderMap,
    body: &'a Bytes,
    ip: Option<std::net::IpAddr>,
    started: Instant,
    max_body: usize,
}

struct Outcome<'a> {
    outcome: &'static str,
    operation: Option<&'a str>,
    params: &'a [(String, String)],
    revision: u64,
    source: Option<String>,
    validation: Vec<schema::Issue>,
    auth: Option<String>,
}

fn capture_entry(
    inc: &Incoming,
    out: Outcome,
    resp: &Response,
    preview: Option<&[u8]>,
) -> MockCapture {
    let kept = &inc.body[..inc.body.len().min(inc.max_body)];
    let (body, body_base64) = if kept.is_empty() {
        (None, None)
    } else {
        match std::str::from_utf8(kept) {
            Ok(s) => (Some(s.to_string()), None),
            Err(_) => (
                None,
                Some(base64::engine::general_purpose::STANDARD.encode(kept)),
            ),
        }
    };
    let (response_body, response_truncated) = match preview {
        Some(b) if !b.is_empty() => {
            let cut = &b[..b.len().min(MAX_RESPONSE_PREVIEW)];
            (
                Some(String::from_utf8_lossy(cut).into_owned()),
                b.len() > MAX_RESPONSE_PREVIEW,
            )
        }
        _ => (None, false),
    };
    MockCapture {
        seq: 0,
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: Utc::now().to_rfc3339(),
        method: inc.method.to_string(),
        path: inc.sub_path.to_string(),
        full_path: inc.uri.path().to_string(),
        query: inc.uri.query().map(str::to_string),
        headers: header_list(inc.headers),
        content_type: inc
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
        body,
        body_base64,
        body_size: inc.body.len(),
        body_truncated: inc.body.len() > inc.max_body,
        client_ip: inc.ip.map(|i| i.to_string()),
        outcome: out.outcome,
        operation: out.operation.map(str::to_string),
        params: out
            .params
            .iter()
            .map(|(name, value)| BinHeader {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
        revision: out.revision,
        status: resp.status().as_u16(),
        source: out.source,
        validation: out.validation,
        auth: out.auth,
        response_headers: header_list(resp.headers()),
        response_body,
        response_truncated,
        duration_ms: inc.started.elapsed().as_millis() as u64,
    }
}

/// Count a mock request in `/_rustybin/metrics` (normal mode only: in public
/// mode mock names are visitor input).
fn count(state: &AppState, name: &str, outcome: &'static str) {
    if !state.config.public_mode {
        state.metrics.count_mock(name, outcome);
    }
}

/// `/mock/{name}/_rustybin/...` belongs to Rustybin (the served spec), never
/// to the mock's operations.
fn is_reserved_sub_path(sub_path: &str) -> bool {
    sub_path == "/_rustybin" || sub_path.starts_with("/_rustybin/")
}

fn add_mock_headers(resp: &mut Response, name: &str, op: Option<&str>) {
    if let Ok(v) = HeaderValue::from_str(name) {
        resp.headers_mut()
            .insert(HeaderName::from_static("x-rustybin-mock"), v);
    }
    if let Some(v) = op.and_then(|o| HeaderValue::from_str(o).ok()) {
        resp.headers_mut()
            .insert(HeaderName::from_static("x-rustybin-mock-operation"), v);
    }
}

#[allow(clippy::too_many_arguments)]
async fn serve(
    state: AppState,
    name: String,
    ip: Option<std::net::IpAddr>,
    tls: Option<TlsConnectionInfo>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let started = Instant::now();
    let store = &state.mocks;
    let prefix_len = "/mock/".len() + name.len();
    let sub_path = match uri.path().get(prefix_len..) {
        Some("") | None => "/".to_string(),
        Some(rest) => rest.to_string(),
    };
    if is_reserved_sub_path(&sub_path) {
        return problem(
            404,
            "reserved path: below /_rustybin a mock serves only openapi.json and openapi.yaml",
            json!({ "mock": name }),
        );
    }
    let inc = Incoming {
        method: &method,
        uri: &uri,
        sub_path: &sub_path,
        headers: &headers,
        body: &body,
        ip,
        started,
        max_body: store.limits().max_captured_body,
    };
    let live = match store.lookup(&name) {
        Lookup::Missing => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": "mock not found",
                    "mock": name,
                    "hint": "create a mock in the console (/ui/#/mocks) or with POST /_rustybin/mocks",
                })),
            )
                .into_response();
        }
        Lookup::NotDeployed(capture, load_error) => {
            let mut resp = match load_error {
                Some(error) => problem(
                    503,
                    "this mock could not be loaded from its file: fix the definition and reload",
                    json!({ "mock": name, "load_error": error }),
                ),
                None => problem(
                    503,
                    "this mock has not been deployed yet: deploy it in the console or with POST /_rustybin/mocks/{name}/deploy",
                    json!({ "mock": name }),
                ),
            };
            add_mock_headers(&mut resp, &name, None);
            count(&state, &name, "not_deployed");
            let out = Outcome {
                outcome: "not_deployed",
                operation: None,
                params: &[],
                revision: 0,
                source: None,
                validation: Vec::new(),
                auth: None,
            };
            capture.record(capture_entry(&inc, out, &resp, None));
            return resp;
        }
        Lookup::Ready(live) => live,
    };
    let c: &Compiled = &live.compiled;
    let record = |resp: Response, out: Outcome, preview: Option<Vec<u8>>| {
        count(&state, &name, out.outcome);
        live.capture
            .record(capture_entry(&inc, out, &resp, preview.as_deref()));
        resp
    };
    let outcome = |o: &'static str| Outcome {
        outcome: o,
        operation: None,
        params: &[],
        revision: c.revision,
        source: None,
        validation: Vec::new(),
        auth: None,
    };

    if !live.enabled {
        let mut resp = problem(
            c.settings.responses.disabled_status,
            "this mock is switched off",
            json!({ "mock": name }),
        );
        add_mock_headers(&mut resp, &name, None);
        return record(resp, outcome("disabled"), None);
    }

    let (op, params) = match c.match_route(method.as_str(), &sub_path) {
        RouteMatch::NotFound => {
            let mut resp = problem(
                404,
                &format!("no operation of this mock matches {} {sub_path}", method),
                json!({ "mock": name, "base_path": format!("/mock/{name}{}", c.path_prefix()) }),
            );
            add_mock_headers(&mut resp, &name, None);
            return record(resp, outcome("not_found"), None);
        }
        RouteMatch::MethodNotAllowed(allowed) => {
            let allow = allowed.join(", ");
            let mut resp = if method == Method::OPTIONS {
                StatusCode::NO_CONTENT.into_response()
            } else {
                problem(
                    405,
                    &format!("{method} is not documented for {sub_path} (allowed: {allow})"),
                    json!({ "mock": name }),
                )
            };
            if let Ok(v) = HeaderValue::from_str(&allow) {
                resp.headers_mut().insert(header::ALLOW, v);
            }
            add_mock_headers(&mut resp, &name, None);
            return record(resp, outcome("method_not_allowed"), None);
        }
        RouteMatch::Found { op, params } => (op, params),
    };

    let query = query_pairs(&uri);
    let client_cert = crate::auth_mtls::has_verified_client_cert(
        tls.as_ref(),
        &headers,
        &state.config,
        &state.certs,
    );
    let auth = c.authorize(
        op,
        &AuthContext {
            headers: &headers,
            query: &query,
            jwt: &state.jwt,
            client_cert,
        },
    );
    let auth_label = auth.label();
    if let AuthResult::Failed(f) = &auth {
        let mut resp = problem(
            f.status,
            &f.reason,
            json!({ "mock": name, "operation": op.id }),
        );
        for ch in &f.challenges {
            if let Ok(v) = HeaderValue::from_str(ch) {
                resp.headers_mut().append(header::WWW_AUTHENTICATE, v);
            }
        }
        add_mock_headers(&mut resp, &name, Some(&op.id));
        let out = Outcome {
            outcome: if f.status == 403 {
                "forbidden"
            } else {
                "unauthorized"
            },
            operation: Some(&op.id),
            params: &params,
            revision: c.revision,
            source: None,
            validation: Vec::new(),
            auth: auth_label,
        };
        return record(resp, out, None);
    }

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    let mode = c.settings.responses.validate_requests;
    let issues = if mode == ValidationMode::Off {
        Vec::new()
    } else {
        request::validate(
            &c.spec.root,
            op,
            &params,
            &query,
            &headers,
            content_type,
            &body,
        )
    };
    if mode == ValidationMode::Enforce && !issues.is_empty() {
        let mut resp = problem(
            400,
            "the request does not match the OpenAPI description",
            json!({ "mock": name, "operation": op.id, "errors": issues }),
        );
        add_mock_headers(&mut resp, &name, Some(&op.id));
        let out = Outcome {
            outcome: "invalid_request",
            operation: Some(&op.id),
            params: &params,
            revision: c.revision,
            source: None,
            validation: issues,
            auth: auth_label,
        };
        return record(resp, out, None);
    }

    let body_json: Option<Value> = if content_type
        .map(request::base_media)
        .is_some_and(|ct| spec::is_json_media(&ct))
    {
        serde_json::from_slice(&body).ok()
    } else {
        None
    };
    let body_text = std::str::from_utf8(&body).ok().filter(|s| !s.is_empty());
    let out = respond::respond_with(
        c,
        op,
        &RequestView {
            path_params: &params,
            query: &query,
            headers: &headers,
            body_json: body_json.as_ref(),
            body_text,
        },
        Some((&live.state, &sub_path)),
    );
    if mode == ValidationMode::Enforce && out.source != "undocumented" {
        let accept = headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .filter(|a| !a.trim().is_empty());
        if let (Some(accept), Some(ct)) = (accept, out.content_type.as_deref()) {
            if !respond::accept_allows(accept, ct) {
                let available: Vec<&str> = op
                    .response_for(out.status)
                    .map(|d| d.content.iter().map(|m| m.media_type.as_str()).collect())
                    .unwrap_or_else(|| vec![ct]);
                let mut resp = problem(
                    406,
                    &format!(
                        "the operation answers {} but Accept is {accept:?}",
                        available.join(", ")
                    ),
                    json!({ "mock": name, "operation": op.id, "available": available }),
                );
                add_mock_headers(&mut resp, &name, Some(&op.id));
                let entry = Outcome {
                    outcome: "not_acceptable",
                    operation: Some(&op.id),
                    params: &params,
                    revision: c.revision,
                    source: None,
                    validation: issues,
                    auth: auth_label,
                };
                return record(resp, entry, None);
            }
        }
    }
    if out.delay_ms > 0 {
        let capped = out.delay_ms.min(state.config.max_delay_ms());
        tokio::time::sleep(Duration::from_millis(capped)).await;
    }
    let bytes = out
        .body
        .as_ref()
        .map(respond::Body::to_bytes)
        .unwrap_or_default();
    let status = StatusCode::from_u16(out.status).unwrap_or(StatusCode::OK);
    let mut resp = (status, bytes.clone()).into_response();
    if let Some(ct) = out
        .content_type
        .as_deref()
        .and_then(|c| HeaderValue::from_str(c).ok())
    {
        resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    for (k, v) in &out.headers {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            resp.headers_mut().insert(n, v);
        }
    }
    add_mock_headers(&mut resp, &name, Some(&op.id));
    if let Ok(v) = HeaderValue::from_str(&out.source) {
        resp.headers_mut()
            .insert(HeaderName::from_static("x-rustybin-mock-source"), v);
    }
    if !issues.is_empty() {
        resp.headers_mut().insert(
            HeaderName::from_static("x-rustybin-mock-validation"),
            HeaderValue::from_static("failed"),
        );
    }
    let entry = Outcome {
        outcome: "mocked",
        operation: Some(&op.id),
        params: &params,
        revision: c.revision,
        source: Some(out.source.clone()),
        validation: issues,
        auth: auth_label,
    };
    record(resp, entry, Some(bytes))
}

/// The deployed spec, rewritten for this mock's URL (and the built-in IdP).
/// The deployed spec with `servers` set to `server` and, when security is
/// on, the OAuth flows pointed at the identity provider at `idp_base`.
pub fn served_spec(c: &Compiled, server: &str, idp_base: &str, name: &str) -> Value {
    let mut doc = c.spec.root.clone();
    let description = format!("Rustybin mock {name:?}");
    if let Value::Object(m) = &mut doc {
        m.insert(
            "servers".into(),
            json!([{ "url": server, "description": description }]),
        );
        if c.settings.security.enabled {
            if let Some(Value::Object(schemes)) = m
                .get_mut("components")
                .and_then(|c| c.get_mut("securitySchemes"))
            {
                for s in schemes.values_mut() {
                    rewrite_flows(s, idp_base);
                }
            }
        }
    }
    doc
}

fn rewrite_flows(scheme: &mut Value, base: &str) {
    match scheme.get("type").and_then(Value::as_str) {
        Some("oauth2") => {
            if let Some(Value::Object(flows)) = scheme.get_mut("flows") {
                for flow in flows.values_mut() {
                    if let Value::Object(f) = flow {
                        for (key, path) in [
                            ("tokenUrl", "/oauth/token"),
                            ("refreshUrl", "/oauth/token"),
                            ("authorizationUrl", "/oauth/authorize"),
                        ] {
                            if f.contains_key(key) {
                                f.insert(key.into(), json!(format!("{base}{path}")));
                            }
                        }
                    }
                }
            }
        }
        Some("openIdConnect") => {
            if let Value::Object(s) = scheme {
                s.insert(
                    "openIdConnectUrl".into(),
                    json!(format!("{base}/.well-known/openid-configuration")),
                );
            }
        }
        _ => {}
    }
}

/// Server URL and identity provider base of the served spec. Through a
/// host-based URL (`petstore.mock.example.com`) the server is that host
/// itself and the identity provider is the mocks domain.
fn served_bases(
    c: &Compiled,
    name: &str,
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
    config: &Config,
) -> (String, String) {
    let origin = crate::session::request_origin(headers, extensions, uri, config);
    let base = origin.base_url();
    if let Some(domain) = &config.mocks_host {
        if host::mock_for_host(&origin.host, domain) == Some(name) {
            let apex = crate::session::RequestOrigin {
                scheme: origin.scheme.clone(),
                host: domain.clone(),
                port: origin.port,
            };
            return (format!("{base}{}", c.path_prefix()), apex.base_url());
        }
    }
    (format!("{base}/mock/{name}{}", c.path_prefix()), base)
}

fn spec_lookup(store: &MockStore, name: &str) -> Option<Arc<Compiled>> {
    match store.lookup(name) {
        Lookup::Ready(l) => Some(l.compiled),
        _ => None,
    }
}

pub async fn spec_json(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let Some(c) = spec_lookup(&state.mocks, &name) else {
        return json_error(StatusCode::NOT_FOUND, "mock not found or not deployed");
    };
    let (server, idp) = served_bases(&c, &name, &headers, &extensions, &uri, &state.config);
    Json(served_spec(&c, &server, &idp, &name)).into_response()
}

pub async fn spec_yaml(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let Some(c) = spec_lookup(&state.mocks, &name) else {
        return json_error(StatusCode::NOT_FOUND, "mock not found or not deployed");
    };
    let (server, idp) = served_bases(&c, &name, &headers, &extensions, &uri, &state.config);
    match serde_yaml::to_string(&served_spec(&c, &server, &idp, &name)) {
        Ok(text) => ([(header::CONTENT_TYPE, "application/yaml")], text).into_response(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ── Control plane: views ────────────────────────────────────────────

/// Operations without schemas (the list view of a mock), with call counts
/// when the mock has received traffic.
fn operation_rows(c: &Compiled, stats: &HashMap<String, OpStats>) -> Vec<Value> {
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
                "hits": stats.get(&op.id).map_or(0, |s| s.hits),
                "last_hit": stats.get(&op.id).and_then(|s| s.last_hit.clone()),
            })
        })
        .collect()
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
/// redacted (see [`sees_secrets`]).
fn detail(
    s: &Snapshot,
    base: &str,
    host: Option<&host::HostBase>,
    limits: Value,
    reveal: bool,
) -> Value {
    let c = &s.draft;
    let mut out = s.summary();
    if let Value::Object(m) = &mut out {
        if let Some(h) = host {
            m.insert("host_url".into(), json!(h.url(&s.name, c.path_prefix())));
        }
        m.insert(
            "url".into(),
            json!(format!("{base}/mock/{}{}", s.name, c.path_prefix())),
        );
        m.insert("base_url".into(), json!(format!("{base}/mock/{}", s.name)));
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
                "spec_url": format!("{base}/mock/{}/_rustybin/openapi.json", s.name),
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
        m.insert("revisions".into(), json!(revisions(s)));
        m.insert("draft_changes".into(), json!(draft_changes(s)));
        m.insert("limits".into(), limits);
    }
    out
}

/// Replace `$ref`s with their targets (bounded; recursive refs are kept and
/// marked) so the console can build forms without a resolver.
pub fn inline_refs(v: &Value, root: &Value, stack: &mut Vec<String>, depth: usize) -> Value {
    if depth > 24 {
        return json!({});
    }
    match v {
        Value::Object(m) => {
            if let Some(Value::String(r)) = m.get("$ref") {
                if stack.contains(r) || stack.len() > 10 {
                    return json!({ "$ref": r, "x-recursive": true });
                }
                let Ok(target) = spec::resolve_pointer(root, r) else {
                    return json!({});
                };
                stack.push(r.clone());
                let mut out = inline_refs(target, root, stack, depth + 1);
                stack.pop();
                if let Value::Object(o) = &mut out {
                    let title = r.rsplit('/').next().unwrap_or_default().to_string();
                    o.entry("title").or_insert(json!(title));
                    for (k, val) in m {
                        if k != "$ref" {
                            o.insert(k.clone(), inline_refs(val, root, stack, depth + 1));
                        }
                    }
                }
                return out;
            }
            Value::Object(
                m.iter()
                    .map(|(k, val)| (k.clone(), inline_refs(val, root, stack, depth + 1)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|i| inline_refs(i, root, stack, depth + 1))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn operation_detail(
    c: &Compiled,
    op_id: &str,
    base: &str,
    name: &str,
    reveal: bool,
) -> Option<Value> {
    let op = c.spec.operation(op_id)?;
    let mut op_json = serde_json::to_value(op).unwrap_or(Value::Null);
    let op_json = inline_refs(&op_json.take(), &c.spec.root, &mut Vec::new(), 0);
    let mut report = c.report.operations.iter().find(|r| r.id == op_id).cloned();
    let mut sample = checks::sample_request(c, op);
    if !reveal {
        checks::hide_credentials(c, op, &mut sample);
        if let Some(r) = &mut report {
            checks::hide_credentials(c, op, &mut r.request);
        }
    }
    Some(json!({
        "operation": op_json,
        "settings": c.settings.operations.get(op_id).cloned().unwrap_or_default(),
        "default_status": op.default_status(),
        "mode": c.settings.operations.get(op_id).and_then(|o| o.mode).unwrap_or(c.settings.responses.mode),
        "report": report,
        "sample_request": sample,
        "url": format!("{base}/mock/{name}{}", sample.path),
    }))
}

fn limits_json(store: &MockStore) -> Value {
    serde_json::to_value(store.limits()).unwrap_or(Value::Null)
}

// ── Control plane: handlers ─────────────────────────────────────────

#[allow(clippy::result_large_err)]
fn admin(headers: &HeaderMap, config: &Config) -> Result<(), Response> {
    crate::admin::require_admin(headers, config)
}

/// May the caller see the mocks' secrets (API keys, bearer tokens, basic
/// passwords)? Only when it could change the mock: the control-auth grant
/// (when control auth or shared mode is on) has the admin scope and the
/// admin guard passes. Everyone else gets placeholders.
fn sees_secrets(headers: &HeaderMap, extensions: &Extensions, config: &Config) -> bool {
    extensions
        .get::<ControlAuthorized>()
        .is_none_or(|g| g.0.admin)
        && crate::admin::is_admin(headers, config)
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let base = base_url(&headers, &extensions, &uri, &state.config);
    let host = host::base(&headers, &extensions, &uri, &state.config);
    let mocks: Vec<Value> = state
        .mocks
        .list()
        .iter()
        .map(|s| {
            let mut v = s.summary();
            if let Value::Object(m) = &mut v {
                m.insert(
                    "url".into(),
                    json!(format!("{base}/mock/{}{}", s.name, s.draft.path_prefix())),
                );
                if let Some(h) = &host {
                    m.insert(
                        "host_url".into(),
                        json!(h.url(&s.name, s.draft.path_prefix())),
                    );
                }
            }
            v
        })
        .collect();
    Json(json!({
        "count": mocks.len(),
        "mocks": mocks,
        "load_errors": state.mocks.load_errors(),
        "persistent": state.mocks.persist_dir().is_some(),
        "host_domain": state.config.mocks_host,
        "limits": limits_json(&state.mocks),
    }))
    .into_response()
}

/// Body of `POST /_rustybin/mocks/import`: `{"spec": ...}`, the spec object
/// itself, or the raw text.
fn import_text(headers: &HeaderMap, body: &Bytes) -> Result<String, String> {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(request::base_media)
        .unwrap_or_default();
    if ct == "application/json" {
        if let Ok(Value::Object(m)) = serde_json::from_slice::<Value>(body) {
            if let Some(spec) = m.get("spec") {
                return spec_text(spec);
            }
        }
    }
    String::from_utf8(body.to_vec()).map_err(|_| "the spec is not UTF-8 text".to_string())
}

pub async fn import(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let text = match import_text(&headers, &body) {
        Ok(t) => t,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
    };
    let compiled = match state.mocks.compile(&text, MockSettings::default()) {
        Ok(c) => c,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
    };
    let s = &compiled.spec;
    let suggested = unique_name(&state.mocks, &s.suggested_name());
    Json(json!({
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
    }))
    .into_response()
}

fn unique_name(store: &MockStore, base: &str) -> String {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBody {
    name: Option<String>,
    spec: Value,
    enabled: Option<bool>,
    deploy: Option<bool>,
    force: Option<bool>,
    strip_server_prefix: Option<bool>,
    responses: Option<ResponseSettings>,
    security: Option<SecuritySettings>,
    operations: Option<BTreeMap<String, OperationSettings>>,
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Response {
    if let Err(r) = admin(&headers, &state.config) {
        return r;
    }
    let req: CreateBody = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("invalid request: {e}")),
    };
    let text = match spec_text(&req.spec) {
        Ok(t) => t,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
    };
    let settings = MockSettings {
        strip_server_prefix: req.strip_server_prefix.unwrap_or(false),
        responses: req.responses.unwrap_or_default(),
        security: req.security.unwrap_or_default(),
        operations: req.operations.unwrap_or_default(),
    };
    let compiled = match state.mocks.compile(&text, settings) {
        Ok(c) => c,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
    };
    let name = match req.name {
        Some(n) => n,
        None => unique_name(&state.mocks, &compiled.spec.suggested_name()),
    };
    if let Err(e) = check_name(&name) {
        return store_error(e);
    }
    let created = state.mocks.create(
        &name,
        compiled,
        Origin::Api,
        req.enabled.unwrap_or(true),
        req.deploy.unwrap_or(true),
        req.force.unwrap_or(false),
    );
    match created {
        Ok(s) => {
            let base = base_url(&headers, &extensions, &uri, &state.config);
            (
                StatusCode::CREATED,
                Json(detail(
                    &s,
                    &base,
                    host::base(&headers, &extensions, &uri, &state.config).as_ref(),
                    limits_json(&state.mocks),
                    sees_secrets(&headers, &extensions, &state.config),
                )),
            )
                .into_response()
        }
        Err(e) => store_error(e),
    }
}

pub async fn get_one(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let Some(s) = state.mocks.get(&name) else {
        return store_error(StoreError::NotFound(name));
    };
    let base = base_url(&headers, &extensions, &uri, &state.config);
    Json(detail(
        &s,
        &base,
        host::base(&headers, &extensions, &uri, &state.config).as_ref(),
        limits_json(&state.mocks),
        sees_secrets(&headers, &extensions, &state.config),
    ))
    .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchBody {
    name: Option<String>,
    spec: Option<Value>,
    strip_server_prefix: Option<bool>,
    responses: Option<ResponseSettings>,
    security: Option<SecuritySettings>,
    operations: Option<BTreeMap<String, OperationSettings>>,
    deploy: Option<bool>,
    force: Option<bool>,
}

pub async fn patch(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Response {
    if let Err(r) = admin(&headers, &state.config) {
        return r;
    }
    let req: PatchBody = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("invalid request: {e}")),
    };
    let draft = match state.mocks.draft(&name) {
        Ok(d) => d,
        Err(e) => return store_error(e),
    };
    if let Some(n) = &req.name {
        if let Err(e) = check_name(n) {
            return store_error(e);
        }
    }
    let changes_definition = req.spec.is_some()
        || req.strip_server_prefix.is_some()
        || req.responses.is_some()
        || req.security.is_some()
        || req.operations.is_some();
    let compiled = if changes_definition {
        let source = match &req.spec {
            Some(v) => match spec_text(v) {
                Ok(t) => Some(t),
                Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
            },
            None => None,
        };
        let mut settings = draft.settings.clone();
        if let Some(v) = req.strip_server_prefix {
            settings.strip_server_prefix = v;
        }
        if let Some(v) = req.responses {
            settings.responses = v;
        }
        if let Some(v) = req.security {
            settings.security = v;
        }
        if let Some(v) = req.operations {
            settings.operations = v;
        }
        match state.mocks.recompile(&draft, source.as_deref(), settings) {
            Ok(c) => Some(c),
            Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
        }
    } else {
        None
    };
    let mut snap = match state.mocks.set_draft(&name, compiled, req.name.as_deref()) {
        Ok(s) => s,
        Err(e) => return store_error(e),
    };
    if req.deploy == Some(true) {
        snap = match state.mocks.deploy(&snap.name, req.force.unwrap_or(false)) {
            Ok(s) => s,
            Err(e) => return store_error(e),
        };
    }
    let base = base_url(&headers, &extensions, &uri, &state.config);
    Json(detail(
        &snap,
        &base,
        host::base(&headers, &extensions, &uri, &state.config).as_ref(),
        limits_json(&state.mocks),
        sees_secrets(&headers, &extensions, &state.config),
    ))
    .into_response()
}

pub async fn delete(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = admin(&headers, &state.config) {
        return r;
    }
    if state.mocks.delete(&name) {
        Json(json!({ "deleted": name })).into_response()
    } else {
        store_error(StoreError::NotFound(name))
    }
}

async fn switch(state: AppState, name: String, headers: HeaderMap, on: bool) -> Response {
    if let Err(r) = admin(&headers, &state.config) {
        return r;
    }
    match state.mocks.set_enabled(&name, on) {
        Ok(s) => Json(s.summary()).into_response(),
        Err(e) => store_error(e),
    }
}

pub async fn enable(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
) -> Response {
    switch(state, name, headers, true).await
}

pub async fn disable(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
) -> Response {
    switch(state, name, headers, false).await
}

#[derive(Deserialize)]
pub struct ForceQuery {
    force: Option<bool>,
}

pub async fn deploy(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    Query(q): Query<ForceQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = admin(&headers, &state.config) {
        return r;
    }
    match state.mocks.deploy(&name, q.force.unwrap_or(false)) {
        Ok(s) => Json(json!({
            "deployed": true,
            "revision": s.live.as_ref().map(|l| l.revision),
            "report": { "ok": s.draft.report.ok, "errors": s.draft.report.errors, "warnings": s.draft.report.warnings },
            "mock": s.summary(),
        }))
        .into_response(),
        Err(e) => store_error(e),
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RollbackBody {
    revision: Option<u64>,
}

pub async fn rollback(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(r) = admin(&headers, &state.config) {
        return r;
    }
    let req: RollbackBody = if body.iter().all(u8::is_ascii_whitespace) {
        RollbackBody::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("invalid request: {e}")),
        }
    };
    match state.mocks.rollback(&name, req.revision) {
        Ok(s) => Json(json!({
            "rolled_back": true,
            "revision": s.live.as_ref().map(|l| l.revision),
            "mock": s.summary(),
        }))
        .into_response(),
        Err(e) => store_error(e),
    }
}

pub async fn report(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    match state.mocks.draft(&name) {
        Ok(d) if sees_secrets(&headers, &extensions, &state.config) => {
            Json(&d.report).into_response()
        }
        Ok(d) => {
            let mut report = d.report.clone();
            checks::hide_report_credentials(&d, &mut report);
            Json(report).into_response()
        }
        Err(e) => store_error(e),
    }
}

fn yaml_response(text: String, filename: &str) -> Response {
    let disposition = format!("inline; filename=\"{filename}\"");
    let mut resp = (
        [(header::CONTENT_TYPE, "application/yaml; charset=utf-8")],
        text,
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&disposition) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}

pub async fn export_one(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let reveal = sees_secrets(&headers, &extensions, &state.config);
    match state.mocks.get(&name) {
        Some(s) => yaml_response(
            config_file::export(&[s], reveal),
            &format!("{name}.rustybin.yaml"),
        ),
        None => store_error(StoreError::NotFound(name)),
    }
}

pub async fn export_all(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let reveal = sees_secrets(&headers, &extensions, &state.config);
    let all: Vec<Snapshot> = state
        .mocks
        .list()
        .into_iter()
        .filter(|s| s.origin != Origin::Sample)
        .collect();
    yaml_response(config_file::export(&all, reveal), "rustybin.yaml")
}

pub async fn config_schema() -> Response {
    Json(config_file::json_schema()).into_response()
}

#[derive(Deserialize)]
pub struct ApplyQuery {
    prune: Option<bool>,
}

pub async fn apply(
    State(state): State<AppState>,
    Query(q): Query<ApplyQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(r) = admin(&headers, &state.config) {
        return r;
    }
    let Ok(text) = std::str::from_utf8(&body) else {
        return json_error(StatusCode::BAD_REQUEST, "the document is not UTF-8 text");
    };
    let file = match config_file::parse(text) {
        Ok(f) => f,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
    };
    for doc in &file.mocks {
        if let Err(e) = check_name(&doc.name) {
            return store_error(e);
        }
    }
    let (defs, mut errors) =
        config_file::definitions(file, None, state.mocks.limits().max_spec_bytes as u64);
    let prune = q.prune.unwrap_or(false).then_some(Origin::Api);
    let mut report = state.mocks.apply(defs, Origin::Api, prune);
    errors.append(&mut report.errors);
    report.errors = errors;
    Json(report).into_response()
}

pub async fn reload(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = admin(&headers, &state.config) {
        return r;
    }
    let (report, problems) = super::reload_files(&state.mocks, &state.config);
    Json(json!({ "report": report, "problems": problems })).into_response()
}

#[derive(Deserialize)]
pub struct OpPath {
    name: String,
    op: String,
}

pub async fn get_operation(
    State(state): State<AppState>,
    Path(OpPath { name, op }): Path<OpPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let draft = match state.mocks.draft(&name) {
        Ok(d) => d,
        Err(e) => return store_error(e),
    };
    let base = base_url(&headers, &extensions, &uri, &state.config);
    match operation_detail(
        &draft,
        &op,
        &base,
        &name,
        sees_secrets(&headers, &extensions, &state.config),
    ) {
        Some(v) => Json(v).into_response(),
        None => store_error(StoreError::NotFound(format!("{name}/{op}"))),
    }
}

pub async fn put_operation(
    State(state): State<AppState>,
    Path(OpPath { name, op }): Path<OpPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Response {
    if let Err(r) = admin(&headers, &state.config) {
        return r;
    }
    let settings: Option<OperationSettings> = match serde_json::from_slice(&body) {
        Ok(s) => s,
        Err(e) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                &format!("invalid operation settings: {e}"),
            )
        }
    };
    let snap = match state.mocks.set_operation(&name, &op, settings) {
        Ok(s) => s,
        Err(e) => return store_error(e),
    };
    let base = base_url(&headers, &extensions, &uri, &state.config);
    match operation_detail(
        &snap.draft,
        &op,
        &base,
        &name,
        sees_secrets(&headers, &extensions, &state.config),
    ) {
        Some(v) => Json(v).into_response(),
        None => store_error(StoreError::NotFound(format!("{name}/{op}"))),
    }
}

#[derive(Deserialize)]
pub struct SampleQuery {
    status: Option<u16>,
    seed: Option<u64>,
    mode: Option<ResponseMode>,
    example: Option<String>,
}

pub async fn sample(
    State(state): State<AppState>,
    Path(OpPath { name, op }): Path<OpPath>,
    Query(q): Query<SampleQuery>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let draft = match state.mocks.draft(&name) {
        Ok(d) => d,
        Err(e) => return store_error(e),
    };
    let Some(operation) = draft.spec.operation(&op) else {
        return store_error(StoreError::NotFound(format!("{name}/{op}")));
    };
    // A different seed means a different compiled view of the same draft.
    let reseeded;
    let c: &Compiled = match q.seed {
        Some(seed) if seed != draft.settings.responses.seed => {
            let mut settings = draft.settings.clone();
            settings.responses.seed = seed;
            reseeded = match state.mocks.recompile(&draft, None, settings) {
                Ok(c) => c,
                Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
            };
            &reseeded
        }
        _ => &draft,
    };
    let Some(operation) = c.spec.operation(&operation.id) else {
        return store_error(StoreError::NotFound(format!("{name}/{op}")));
    };
    let status = q.status.unwrap_or_else(|| operation.default_status());
    let mode = q.mode.unwrap_or(c.settings.responses.mode);
    let mut request = checks::sample_request(c, operation);
    if !sees_secrets(&headers, &extensions, &state.config) {
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
    let base = base_url(&headers, &extensions, &uri, &state.config);
    Json(json!({
        "request": request,
        "url": format!("{base}/mock/{name}{}", request.path),
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
    .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidateBody {
    /// `response` (default) or `request`.
    kind: Option<String>,
    status: Option<u16>,
    body: Value,
}

pub async fn validate(
    State(state): State<AppState>,
    Path(OpPath { name, op }): Path<OpPath>,
    Json(req): Json<ValidateBody>,
) -> Response {
    let draft = match state.mocks.draft(&name) {
        Ok(d) => d,
        Err(e) => return store_error(e),
    };
    let Some(operation) = draft.spec.operation(&op) else {
        return store_error(StoreError::NotFound(format!("{name}/{op}")));
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
    Json(json!({ "valid": issues.is_empty(), "has_schema": schema_v.is_some(), "issues": issues }))
        .into_response()
}

#[derive(Deserialize)]
pub struct ListQuery {
    limit: Option<usize>,
}

pub async fn requests(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    Query(q): Query<ListQuery>,
) -> Response {
    let Some(s) = state.mocks.get(&name) else {
        return store_error(StoreError::NotFound(name));
    };
    let limit = q
        .limit
        .unwrap_or(50)
        .clamp(1, state.mocks.limits().captures);
    let entries = s.capture.entries(limit);
    Json(json!({
        "mock": name,
        "total": s.capture.total(),
        "count": entries.len(),
        "requests": entries,
    }))
    .into_response()
}

pub async fn clear_requests(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = admin(&headers, &state.config) {
        return r;
    }
    match state.mocks.get(&name) {
        Some(s) => {
            s.capture.clear();
            Json(json!({ "cleared": name })).into_response()
        }
        None => store_error(StoreError::NotFound(name)),
    }
}

#[derive(Deserialize)]
pub struct EntryPath {
    name: String,
    n: String,
}

pub async fn request_entry(
    State(state): State<AppState>,
    Path(EntryPath { name, n }): Path<EntryPath>,
) -> Response {
    let Some(s) = state.mocks.get(&name) else {
        return store_error(StoreError::NotFound(name));
    };
    let Ok(seq) = n.parse::<u64>() else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "request number must be a positive integer",
        );
    };
    match s.capture.entry(seq) {
        Some(e) => Json(e.as_ref().clone()).into_response(),
        None => json_error(
            StatusCode::NOT_FOUND,
            "no such request (never received or already evicted)",
        ),
    }
}

pub async fn stream(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
) -> Response {
    let Some(s) = state.mocks.get(&name) else {
        return store_error(StoreError::NotFound(name));
    };
    let mut rx = s.capture.subscribe();
    let last_seen: Option<u64> = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok());
    let mut backlog: Vec<Arc<MockCapture>> = match last_seen {
        Some(last) => s
            .capture
            .entries(usize::MAX)
            .into_iter()
            .filter(|r| r.seq > last)
            .collect(),
        None => Vec::new(),
    };
    backlog.reverse();
    let store = state.mocks.clone();
    let stream = async_stream::stream! {
        let mut sent = last_seen.unwrap_or(0);
        for entry in backlog {
            sent = entry.seq;
            yield Ok::<Event, Infallible>(capture_event(&entry));
        }
        let mut check = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = check.tick() => {
                    // End the stream when the mock is deleted.
                    if store.get(&name).is_none() {
                        break;
                    }
                }
                item = rx.recv() => match item {
                    Ok(entry) => {
                        if entry.seq > sent {
                            sent = entry.seq;
                            yield Ok(capture_event(&entry));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            }
        }
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn capture_event(entry: &MockCapture) -> Event {
    Event::default()
        .event("request")
        .id(entry.seq.to_string())
        .data(serde_json::to_string(entry).unwrap_or_default())
}

// ── Control plane: spec source, duplicate, rule preview, state, collections

/// The spec as uploaded (the draft's), JSON or YAML as written.
pub async fn spec_source(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
) -> Response {
    let draft = match state.mocks.draft(&name) {
        Ok(d) => d,
        Err(e) => return store_error(e),
    };
    let text = draft.source.to_string();
    let json = text.trim_start().starts_with('{');
    let (ct, ext) = if json {
        ("application/json", "json")
    } else {
        ("application/yaml", "yaml")
    };
    let disposition = format!("inline; filename=\"{name}.openapi.{ext}\"");
    let mut resp = ([(header::CONTENT_TYPE, ct)], text).into_response();
    if let Ok(v) = HeaderValue::from_str(&disposition) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DuplicateBody {
    name: Option<String>,
}

pub async fn duplicate(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Response {
    if let Err(r) = admin(&headers, &state.config) {
        return r;
    }
    let req: DuplicateBody = if body.iter().all(u8::is_ascii_whitespace) {
        DuplicateBody { name: None }
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("invalid request: {e}")),
        }
    };
    let new_name = req
        .name
        .unwrap_or_else(|| unique_name(&state.mocks, &format!("{name}-copy")));
    if let Err(e) = check_name(&new_name) {
        return store_error(e);
    }
    match state.mocks.duplicate(&name, &new_name) {
        Ok(s) => {
            let base = base_url(&headers, &extensions, &uri, &state.config);
            (
                StatusCode::CREATED,
                Json(detail(
                    &s,
                    &base,
                    host::base(&headers, &extensions, &uri, &state.config).as_ref(),
                    limits_json(&state.mocks),
                    sees_secrets(&headers, &extensions, &state.config),
                )),
            )
                .into_response()
        }
        Err(e) => store_error(e),
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RuleTestBody {
    /// Rules to try (default: the operation's rules in the draft).
    rules: Option<Vec<super::model::Rule>>,
    /// Captured request to try them on (default: the latest call of the
    /// operation).
    request: Option<u64>,
}

/// Which rule would answer a captured request.
pub async fn test_rules(
    State(state): State<AppState>,
    Path(OpPath { name, op }): Path<OpPath>,
    body: Bytes,
) -> Response {
    let req: RuleTestBody = if body.iter().all(u8::is_ascii_whitespace) {
        RuleTestBody::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("invalid request: {e}")),
        }
    };
    let Some(s) = state.mocks.get(&name) else {
        return store_error(StoreError::NotFound(name));
    };
    let c = &s.draft;
    let Some(operation) = c.spec.operation(&op) else {
        return store_error(StoreError::NotFound(format!("{name}/{op}")));
    };
    let rules = req.rules.unwrap_or_else(|| {
        c.settings
            .operations
            .get(&op)
            .map(|o| o.rules.clone())
            .unwrap_or_default()
    });
    let max_rules = state.mocks.limits().max_rules;
    if rules.len() > max_rules {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("at most {max_rules} rules per operation"),
        );
    }
    let capture = match req.request {
        Some(seq) => s.capture.entry(seq),
        None => s
            .capture
            .entries(state.mocks.limits().captures)
            .into_iter()
            .find(|e| e.operation.as_deref() == Some(operation.id.as_str())),
    };
    let Some(capture) = capture else {
        return Json(json!({
            "request": null,
            "winner": null,
            "rules": [],
            "hint": "no captured call of this operation yet: send one (Try it) and test again",
        }))
        .into_response();
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
    Json(json!({
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
    .into_response()
}

/// The stored items of a stateful mock.
pub async fn get_state(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
) -> Response {
    match state.mocks.get(&name) {
        Some(s) => Json(s.state.dump()).into_response(),
        None => store_error(StoreError::NotFound(name)),
    }
}

pub async fn clear_state(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = admin(&headers, &state.config) {
        return r;
    }
    match state.mocks.get(&name) {
        Some(s) => {
            s.state.clear();
            Json(json!({ "cleared": name })).into_response()
        }
        None => store_error(StoreError::NotFound(name)),
    }
}

#[derive(Deserialize)]
pub struct CollectionPath {
    name: String,
    format: String,
}

#[derive(Deserialize)]
pub struct CollectionQuery {
    base_url: Option<String>,
}

/// The mock's operations as a collection (the live revision, else the draft).
pub async fn collection(
    State(state): State<AppState>,
    Path(CollectionPath { name, format }): Path<CollectionPath>,
    Query(q): Query<CollectionQuery>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let Some(s) = state.mocks.get(&name) else {
        return store_error(StoreError::NotFound(name));
    };
    let base = match q.base_url {
        Some(raw) => match crate::collections::validate_base_url(&raw) {
            Ok(b) => b,
            Err(reason) => return json_error(StatusCode::BAD_REQUEST, reason),
        },
        None => format!(
            "{}/mock/{name}",
            base_url(&headers, &extensions, &uri, &state.config)
        ),
    };
    let c = s.live.as_ref().unwrap_or(&s.draft);
    let reveal = sees_secrets(&headers, &extensions, &state.config);
    let Some((ct, file, text)) = super::collections::render(c, &name, &format, &base, reveal)
    else {
        let known: Vec<&str> = super::collections::FORMATS
            .iter()
            .map(|(f, _, _)| *f)
            .collect();
        return json_error(
            StatusCode::NOT_FOUND,
            &format!("unknown format {format:?}: use one of {}", known.join(", ")),
        );
    };
    let mut resp = ([(header::CONTENT_TYPE, ct)], text).into_response();
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{file}\"")) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}
