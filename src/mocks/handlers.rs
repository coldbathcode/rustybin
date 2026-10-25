//! HTTP handlers: the mocks themselves (`/mock/{name}/...`, data plane) and
//! their management API (`/_rustybin/mocks*`, control plane: reads need the
//! `console` scope, mutations the admin token or `admin` scope). With
//! self-service on, visitors also create and manage their own mocks
//! ([`super::selfservice`]).

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
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use super::capture::{MockCapture, MAX_RESPONSE_PREVIEW};
use super::compile::{Compiled, RouteMatch};
use super::config_file;
use super::host;
use super::model::{OperationSettings, ValidationMode};
use super::request;
use super::respond::{self, RequestView};
use super::restrict;
use super::schema;
use super::security::{AuthContext, AuthResult};
use super::service::{
    self, CreateBody, PatchBody, RuleTestBody, SampleQuery, ServiceError, Svc, ValidateBody,
};
use super::spec;
use super::store::Lookup;
use crate::config::Config;
use crate::request_bin::BinHeader;
use crate::server::TlsConnectionInfo;
use crate::session::ClientIp;
use crate::state::AppState;

pub use super::service::RESERVED_NAMES;

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

fn query_pairs(uri: &Uri) -> Vec<(String, String)> {
    uri.query()
        .map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect())
        .unwrap_or_default()
}

fn header_list(headers: &HeaderMap, control_authorization: bool) -> Vec<BinHeader> {
    headers
        .iter()
        .map(|(k, v)| BinHeader {
            name: k.as_str().to_string(),
            value: crate::keys::redact_header(k.as_str(), v.as_bytes(), control_authorization),
        })
        .collect()
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
    via: Option<Extension<host::ViaMocksDomain>>,
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
        via.is_some(),
        method,
        uri,
        headers,
        body,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn mock_sub(
    State(state): State<AppState>,
    Path(MockSubPath { name, .. }): Path<MockSubPath>,
    ClientIp(ip): ClientIp,
    tls: Option<Extension<TlsConnectionInfo>>,
    via: Option<Extension<host::ViaMocksDomain>>,
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
        via.is_some(),
        method,
        uri,
        headers,
        body,
        None,
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
    /// The MCP tool that sent this request in process (`None`: HTTP).
    mcp_tool: Option<&'a str>,
    /// `Authorization` carries a control credential (redacted in captures).
    control_authorization: bool,
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
        // API keys (X-Rustybin-Key, ?rb_key=) are redacted like in the inspector.
        query: inc
            .uri
            .query()
            .map(|q| crate::keys::redact_query(q).unwrap_or_else(|| q.to_string())),
        headers: header_list(inc.headers, inc.control_authorization),
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
        response_headers: header_list(resp.headers(), false),
        response_body,
        response_truncated,
        duration_ms: inc.started.elapsed().as_millis() as u64,
        channel: inc.mcp_tool.map(|_| "mcp"),
        tool: inc.mcp_tool.map(str::to_string),
    }
}

/// Count a mock request in `/_rustybin/metrics` (normal mode only: in public
/// mode mock names are visitor input).
fn count(state: &AppState, name: &str, outcome: &'static str) {
    if !state.config.public_mode {
        state.metrics.count_mock(name, outcome);
    }
}

/// `/mock/{name}/_rustybin/...` belongs to Rustybin (the served spec, the
/// MCP server), never to the mock's operations.
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

/// Answer one request to the mock `name` (everything below `/mock/{name}`
/// except the reserved `_rustybin` paths). The mock's MCP server calls it in
/// process for every tool call (`mcp_tool`), so REST and MCP answers are the
/// same by construction.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve(
    state: AppState,
    name: String,
    ip: Option<std::net::IpAddr>,
    tls: Option<TlsConnectionInfo>,
    via_mocks_domain: bool,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    mcp_tool: Option<&str>,
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
            "reserved path: below /_rustybin a mock serves only openapi.json, openapi.yaml and mcp",
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
        mcp_tool,
        control_authorization: crate::keys::authorization_is_control(&headers, &state.config),
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
    // A visitor mock answers only on the mocks domain when there is one.
    if live.visitor && !via_mocks_domain {
        if let Some(domain) = state.config.self_service.host.as_deref() {
            let mut resp = problem(
                404,
                "this self-service mock answers on its own domain",
                json!({ "mock": name, "url": format!("https://{name}.{domain}") }),
            );
            restrict::harden(&mut resp);
            return resp;
        }
    }
    let c: &Compiled = &live.compiled;
    let record = |mut resp: Response, out: Outcome, preview: Option<Vec<u8>>| {
        if live.visitor {
            restrict::harden(&mut resp);
        }
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
        if c.settings.mcp.enabled {
            // The mock's MCP server lives below the mock root, which is the
            // server URL without the spec's path prefix.
            let base = server
                .strip_suffix(c.path_prefix())
                .filter(|_| !c.path_prefix().is_empty())
                .unwrap_or(server);
            m.insert(
                "x-rustybin-mcp".into(),
                json!({ "url": format!("{base}/_rustybin/mcp") }),
            );
        }
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
pub(crate) fn served_bases(
    c: &Compiled,
    name: &str,
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
    config: &Config,
) -> (String, String) {
    let origin = crate::session::request_origin(headers, extensions, uri, config);
    let base = origin.base_url();
    let domains = [
        config.mocks_host.as_ref(),
        config.self_service.host.as_ref(),
    ];
    for domain in domains.into_iter().flatten() {
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

/// The deployed revision whose spec this request may read. A visitor mock
/// answers only on the mocks domain when there is one, like its REST and MCP
/// endpoints (the same 404 as an unknown name).
fn spec_lookup(state: &AppState, name: &str, extensions: &Extensions) -> Option<Arc<Compiled>> {
    match state.mocks.lookup(name) {
        Lookup::Ready(l)
            if l.visitor
                && state.config.self_service.host.is_some()
                && extensions.get::<host::ViaMocksDomain>().is_none() =>
        {
            None
        }
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
    let Some(c) = spec_lookup(&state, &name, &extensions) else {
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
    let Some(c) = spec_lookup(&state, &name, &extensions) else {
        return json_error(StatusCode::NOT_FOUND, "mock not found or not deployed");
    };
    let (server, idp) = served_bases(&c, &name, &headers, &extensions, &uri, &state.config);
    match serde_yaml::to_string(&served_spec(&c, &server, &idp, &name)) {
        Ok(text) => ([(header::CONTENT_TYPE, "application/yaml")], text).into_response(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ── Control plane: handlers (thin wrappers over `super::service::Svc`) ─

fn svc(state: &AppState, headers: &HeaderMap, extensions: &Extensions, uri: &Uri) -> Svc {
    Svc::from_request(state, headers, extensions, uri)
}

fn created(v: Value) -> Response {
    (StatusCode::CREATED, Json(v)).into_response()
}

fn parse_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, ServiceError> {
    serde_json::from_slice(body)
        .map_err(|e| ServiceError::bad_request(format!("invalid request: {e}")))
}

/// An optional JSON body (whitespace means the default).
fn parse_optional<T: serde::de::DeserializeOwned + Default>(
    body: &Bytes,
) -> Result<T, ServiceError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        Ok(T::default())
    } else {
        parse_body(body)
    }
}

#[derive(Deserialize)]
pub struct ListOwnerQuery {
    /// `all` (admin): every visitor's mocks too.
    owner: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(q): Query<ListOwnerQuery>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let s = svc(&state, &headers, &extensions, &uri);
    Json(s.list(q.owner.as_deref() == Some("all"))).into_response()
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
                return service::spec_text(spec);
            }
        }
    }
    String::from_utf8(body.to_vec()).map_err(|_| "the spec is not UTF-8 text".to_string())
}

pub async fn import(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ServiceError> {
    let text = import_text(&headers, &body).map_err(ServiceError::bad_request)?;
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.preview(&text)?).into_response())
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    // The admin guard answers before the body is parsed, as before.
    if !s.caller.admin && state.mocks.visits().is_none() {
        s.require_admin()?;
    }
    let req: CreateBody = parse_body(&body)?;
    Ok(created(s.create(req)?))
}

pub async fn get_one(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.get(&name)?).into_response())
}

pub async fn patch(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    // Access first, then the body, as before.
    s.manageable(&name)?;
    let req: PatchBody = parse_body(&body)?;
    Ok(Json(s.update(&name, req)?).into_response())
}

pub async fn delete(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.delete(&name)?).into_response())
}

pub async fn enable(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.set_enabled(&name, true)?).into_response())
}

pub async fn disable(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.set_enabled(&name, false)?).into_response())
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
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.deploy(&name, q.force.unwrap_or(false))?).into_response())
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
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    s.manageable(&name)?;
    let req: RollbackBody = parse_optional(&body)?;
    Ok(Json(s.rollback(&name, req.revision)?).into_response())
}

pub async fn report(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.report(&name)?).into_response())
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
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    let text = s.export_one(&name)?;
    Ok(yaml_response(text, &format!("{name}.rustybin.yaml")))
}

pub async fn export_all(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let s = svc(&state, &headers, &extensions, &uri);
    yaml_response(s.export_all(), "rustybin.yaml")
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
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    s.require_admin()?;
    let text = std::str::from_utf8(&body)
        .map_err(|_| ServiceError::bad_request("the document is not UTF-8 text"))?;
    Ok(Json(s.apply(text, q.prune.unwrap_or(false))?).into_response())
}

pub async fn reload(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.reload()?).into_response())
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
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.operation(&name, &op)?).into_response())
}

pub async fn put_operation(
    State(state): State<AppState>,
    Path(OpPath { name, op }): Path<OpPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    s.manageable(&name)?;
    let settings: Option<OperationSettings> = serde_json::from_slice(&body)
        .map_err(|e| ServiceError::bad_request(format!("invalid operation settings: {e}")))?;
    Ok(Json(s.set_operation(&name, &op, settings)?).into_response())
}

pub async fn sample(
    State(state): State<AppState>,
    Path(OpPath { name, op }): Path<OpPath>,
    Query(q): Query<SampleQuery>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.sample(&name, &op, &q)?).into_response())
}

pub async fn validate(
    State(state): State<AppState>,
    Path(OpPath { name, op }): Path<OpPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    Json(req): Json<ValidateBody>,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.validate(&name, &op, &req)?).into_response())
}

#[derive(Deserialize)]
pub struct ListQuery {
    limit: Option<usize>,
}

pub async fn requests(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.requests(&name, q.limit)?).into_response())
}

pub async fn clear_requests(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.clear_requests(&name)?).into_response())
}

#[derive(Deserialize)]
pub struct EntryPath {
    name: String,
    n: String,
}

pub async fn request_entry(
    State(state): State<AppState>,
    Path(EntryPath { name, n }): Path<EntryPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    // Access first (404 for mocks the caller may not see), then the number.
    s.readable(&name)?;
    let seq: u64 = n
        .parse()
        .map_err(|_| ServiceError::bad_request("request number must be a positive integer"))?;
    Ok(Json(s.request_entry(&name, seq)?).into_response())
}

pub async fn stream(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let s = match svc(&state, &headers, &extensions, &uri).readable(&name) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
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
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    let (text, json) = s.spec_source(&name)?;
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
    Ok(resp)
}

#[derive(Deserialize, Default)]
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
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    s.manageable(&name)?;
    let req: DuplicateBody = parse_optional(&body)?;
    Ok(created(s.duplicate(&name, req.name)?))
}

/// Which rule would answer a captured request.
pub async fn test_rules(
    State(state): State<AppState>,
    Path(OpPath { name, op }): Path<OpPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ServiceError> {
    let req: RuleTestBody = parse_optional(&body)?;
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.test_rules(&name, &op, req)?).into_response())
}

/// The stored items of a stateful mock.
pub async fn get_state(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.state_dump(&name)?).into_response())
}

pub async fn clear_state(
    State(state): State<AppState>,
    Path(MockPath { name }): Path<MockPath>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    Ok(Json(s.clear_state(&name)?).into_response())
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
) -> Result<Response, ServiceError> {
    let s = svc(&state, &headers, &extensions, &uri);
    let (ct, file, text) = s.collection(&name, &format, q.base_url.as_deref())?;
    let mut resp = ([(header::CONTENT_TYPE, ct)], text).into_response();
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{file}\"")) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}
