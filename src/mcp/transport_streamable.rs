//! Streamable HTTP transport (`/mcp`, `/mcp/protected`, `/mcp/apikey`,
//! `/mcp/servers/{name}`).
//!
//! Era selection per message: a request whose `params._meta` carries
//! `io.modelcontextprotocol/protocolVersion`, or whose `MCP-Protocol-Version`
//! header names a non-handshake version, is served statelessly (2026-07-28).
//! Everything else follows the handshake era (initialize + Mcp-Session-Id).

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::Stream;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::core::{self, CallCtx, HttpInfo};
use super::protocol::{self, Message, RpcError, Version};
use super::sessions::{CancelToken, Session, TransportKind};
use super::{auth, resources, tools, Access, Profile, Shared};
use crate::state::AppState;

/// Values echoed in `X-Rustybin-Mcp-*` response headers.
#[derive(Default, Clone)]
pub struct Echo {
    pub profile: String,
    pub session: Option<String>,
    pub rpc_id: Option<String>,
    pub method: Option<String>,
    pub version: Option<&'static str>,
}

fn header_safe(s: &str) -> Option<HeaderValue> {
    if s.len() > 200 || !s.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        return None;
    }
    HeaderValue::from_str(s).ok()
}

/// Add the demo `X-Rustybin-Mcp-*` headers.
pub fn decorate(mut resp: Response, echo: &Echo) -> Response {
    let h = resp.headers_mut();
    let mut set = |name: &'static str, v: Option<&str>| {
        if let Some(v) = v.and_then(header_safe) {
            h.insert(HeaderName::from_static(name), v);
        }
    };
    set("x-rustybin-mcp-server", Some(&echo.profile));
    set(
        "x-rustybin-mcp-session",
        Some(echo.session.as_deref().unwrap_or("none")),
    );
    set("x-rustybin-mcp-request-id", echo.rpc_id.as_deref());
    set("x-rustybin-mcp-method", echo.method.as_deref());
    set("x-rustybin-mcp-protocol-version", echo.version);
    resp
}

fn id_text(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A JSON-RPC error as an HTTP response.
pub fn rpc_error(status: StatusCode, id: Option<&Value>, err: &RpcError) -> Response {
    (status, Json(protocol::error_response(id, err))).into_response()
}

fn plain_error(status: StatusCode, msg: &str) -> Response {
    rpc_error(status, None, &RpcError::invalid_request(msg))
}

/// What the client accepts: (json, sse).
pub fn accepts(headers: &HeaderMap) -> (bool, bool) {
    let values: Vec<String> = headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .collect();
    if values.is_empty() {
        return (true, true);
    }
    let any = values.iter().any(|v| v == "*/*");
    let json = any
        || values
            .iter()
            .any(|v| v == "application/json" || v == "application/*");
    let sse = any
        || values
            .iter()
            .any(|v| v == "text/event-stream" || v == "text/*");
    (json, sse)
}

pub fn http_info(
    app: &AppState,
    parts: &axum::http::request::Parts,
    transport: &'static str,
) -> HttpInfo {
    let ip = crate::session::client_ip(&parts.headers, &parts.extensions, &app.config);
    HttpInfo {
        method: parts.method.to_string(),
        uri: parts.uri.to_string(),
        headers: parts.headers.clone(),
        client_ip: ip.map(|i| i.to_string()),
        transport,
    }
}

fn page_size(sh: &Shared, uri: &axum::http::Uri) -> usize {
    uri.query()
        .and_then(|q| {
            form_urlencoded::parse(q.as_bytes())
                .find(|(k, _)| k == "page_size")
                .and_then(|(_, v)| v.parse::<usize>().ok())
        })
        .map(|n| n.clamp(1, 1000))
        .unwrap_or(sh.cfg.default_page_size)
}

/// Entry point for every Streamable HTTP route.
pub async fn handle(
    app: AppState,
    sh: Arc<Shared>,
    profile: Option<Arc<Profile>>,
    req: Request,
) -> Response {
    let Some(profile) = profile else {
        return plain_error(StatusCode::NOT_FOUND, "unknown MCP server");
    };
    let (parts, body) = req.into_parts();
    let echo = Echo {
        profile: profile.key.clone(),
        ..Default::default()
    };
    if let Err(resp) = auth::check_origin(&parts.headers, &sh.cfg) {
        return decorate(resp, &echo);
    }
    let base_url = auth::base_url(&parts.headers, &parts.extensions, &parts.uri, &sh.cfg);
    let claims = match profile.access {
        Access::Open => None,
        Access::OAuth => match auth::require_bearer(&parts.headers, &base_url, &sh.cfg, &app.jwt) {
            Ok(c) => Some(c),
            Err(resp) => return decorate(resp, &echo),
        },
        Access::ApiKey => match auth::require_api_key(&parts.headers, &sh.cfg) {
            Ok(c) => Some(c),
            Err(resp) => return decorate(resp, &echo),
        },
    };
    let req = Req {
        info: Arc::new(http_info(&app, &parts, "streamable-http")),
        page_size: page_size(&sh, &parts.uri),
        base_url,
        sh,
        profile,
        claims,
        echo,
    };
    match parts.method {
        Method::POST => {
            let limit = req.sh.cfg.max_body;
            match axum::body::to_bytes(body, limit).await {
                Ok(bytes) => post(req, &bytes).await,
                Err(_) => decorate(
                    rpc_error(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        None,
                        &RpcError::invalid_request(format!("Body exceeds {limit} bytes")),
                    ),
                    &req.echo,
                ),
            }
        }
        Method::GET => get_stream(req),
        Method::DELETE => delete(req),
        _ => {
            let mut resp = plain_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed");
            resp.headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("GET, POST, DELETE"));
            resp
        }
    }
}

/// Per-HTTP-request context.
pub struct Req {
    pub sh: Arc<Shared>,
    pub profile: Arc<Profile>,
    pub claims: Option<Value>,
    pub info: Arc<HttpInfo>,
    pub page_size: usize,
    /// Externally visible `scheme://host[:port]` (see [`auth::base_url`]).
    pub base_url: String,
    pub echo: Echo,
}

impl Req {
    fn headers(&self) -> &HeaderMap {
        &self.info.headers
    }

    fn principal(&self) -> Option<String> {
        self.claims
            .as_ref()
            .and_then(|c| c.get("sub"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn header(&self, name: &str) -> Result<Option<&str>, String> {
        protocol::single_header(self.headers(), name)
    }
}

fn meta_version(params: &Value) -> Option<&Value> {
    params
        .get("_meta")
        .and_then(|m| m.get(protocol::META_PROTOCOL_VERSION))
}

async fn post(mut req: Req, bytes: &[u8]) -> Response {
    let (json_ok, sse_ok) = accepts(req.headers());
    if !json_ok && !sse_ok {
        let resp = rpc_error(
            StatusCode::NOT_ACCEPTABLE,
            None,
            &RpcError::invalid_request(
                "Not Acceptable: Accept must allow application/json or text/event-stream",
            ),
        );
        return decorate(resp, &req.echo);
    }
    if let Some(ct) = req.headers().get(header::CONTENT_TYPE) {
        let ct = ct.to_str().unwrap_or("").to_ascii_lowercase();
        if !ct.starts_with("application/json") {
            let resp = rpc_error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                None,
                &RpcError::invalid_request("Content-Type must be application/json"),
            );
            return decorate(resp, &req.echo);
        }
    }
    let value: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => {
            return decorate(
                rpc_error(StatusCode::BAD_REQUEST, None, &RpcError::parse_error()),
                &req.echo,
            )
        }
    };
    if let Value::Array(items) = value {
        return batch(req, items).await;
    }
    let msg = match protocol::classify(&value) {
        Ok(m) => m,
        Err((err, id)) => {
            return decorate(
                rpc_error(StatusCode::BAD_REQUEST, id.as_ref(), &err),
                &req.echo,
            )
        }
    };
    let header_version = req.header(protocol::HEADER_PROTOCOL_VERSION).ok().flatten();
    let modern_header = header_version.is_some_and(|h| !protocol::is_legacy_version_str(h));
    let modern = match &msg {
        Message::Request { params, .. } => meta_version(params).is_some() || modern_header,
        Message::Notification { .. } => modern_header,
        Message::Response { .. } => modern_header,
    };
    if let Message::Request { id, method, .. } = &msg {
        req.echo.rpc_id = Some(id_text(id));
        req.echo.method = Some(method.clone());
    }
    if modern {
        modern_post(req, msg, (json_ok, sse_ok)).await
    } else {
        legacy_post(req, msg, (json_ok, sse_ok)).await
    }
}

// ── 2026-07-28 (stateless) ───────────────────────────────────────────

/// The modern validation ladder (envelope, routing headers, version).
fn modern_ladder(
    req: &Req,
    method: &str,
    params: &Value,
) -> Result<(Version, Value, Option<usize>), RpcError> {
    let meta = params
        .get("_meta")
        .filter(|m| m.is_object())
        .ok_or_else(|| {
            RpcError::invalid_params(format!(
                "params._meta must be an object carrying {} and {}",
                protocol::META_PROTOCOL_VERSION,
                protocol::META_CLIENT_CAPABILITIES
            ))
        })?;
    let missing: Vec<&str> = [
        protocol::META_PROTOCOL_VERSION,
        protocol::META_CLIENT_CAPABILITIES,
    ]
    .into_iter()
    .filter(|k| meta.get(k).is_none())
    .collect();
    if !missing.is_empty() {
        return Err(RpcError::invalid_params(format!(
            "params._meta is missing the required key(s): {}",
            missing.join(", ")
        )));
    }
    let body_version = &meta[protocol::META_PROTOCOL_VERSION];
    // Routing headers: present once, valid, equal to the body.
    let get = |name: &str| req.header(name).map_err(RpcError::header_mismatch);
    match get(protocol::HEADER_PROTOCOL_VERSION)? {
        Some(h) if Some(h) == body_version.as_str() => {}
        Some(h) => {
            return Err(RpcError::header_mismatch(format!(
                "Header mismatch: MCP-Protocol-Version header value '{h}' does not match body value {body_version}"
            )))
        }
        None => return Err(RpcError::header_mismatch("Header mismatch: MCP-Protocol-Version header is required")),
    }
    match get(protocol::HEADER_METHOD)? {
        Some(h) if h == method => {}
        Some(h) => {
            return Err(RpcError::header_mismatch(format!(
            "Header mismatch: Mcp-Method header value '{h}' does not match body value '{method}'"
        )))
        }
        None => {
            return Err(RpcError::header_mismatch(
                "Header mismatch: Mcp-Method header is required",
            ))
        }
    }
    let name_key = match method {
        "tools/call" | "prompts/get" => Some("name"),
        "resources/read" => Some("uri"),
        _ => None,
    };
    if let Some(key) = name_key {
        if let Some(body_value) = params.get(key).and_then(Value::as_str) {
            match get(protocol::HEADER_NAME)? {
                Some(raw) => match protocol::decode_header_value(raw) {
                    Some(h) if h == body_value => {}
                    Some(h) => {
                        return Err(RpcError::header_mismatch(format!(
                            "Header mismatch: Mcp-Name header value '{h}' does not match body value '{body_value}'"
                        )))
                    }
                    None => return Err(RpcError::header_mismatch("Header mismatch: Mcp-Name header has a malformed base64 value")),
                },
                None => return Err(RpcError::header_mismatch("Header mismatch: Mcp-Name header is required")),
            }
        }
    }
    let Some(version_str) = body_version.as_str() else {
        return Err(RpcError::invalid_params("protocolVersion must be a string"));
    };
    let version = match Version::parse(version_str) {
        Some(v) if v.is_modern() => v,
        Some(_) => {
            let mut e = RpcError::unsupported_version(version_str);
            e.message = format!(
                "Unsupported protocol version: {version_str} requires the initialize handshake"
            );
            return Err(e);
        }
        None => return Err(RpcError::unsupported_version(version_str)),
    };
    let caps = meta[protocol::META_CLIENT_CAPABILITIES].clone();
    if !caps.is_object() {
        return Err(RpcError::invalid_params(
            "clientCapabilities must be an object",
        ));
    }
    let log_level = match meta.get(protocol::META_LOG_LEVEL) {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .and_then(protocol::log_level_rank)
                .ok_or_else(|| RpcError::invalid_params(format!("Invalid log level: {v}")))?,
        ),
    };
    Ok((version, caps, log_level))
}

/// `Mcp-Param-*` validation for tools with `x-mcp-header` annotations.
fn validate_param_headers(req: &Req, params: &Value) -> Result<(), RpcError> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(tool) = tools::find(&req.profile, name) else {
        return Ok(());
    };
    let schema = (tool.input_schema)();
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return Ok(());
    };
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);
    for (prop, spec) in props {
        let Some(token) = spec.get("x-mcp-header").and_then(Value::as_str) else {
            continue;
        };
        let header_name = format!(
            "{}{}",
            protocol::HEADER_PARAM_PREFIX,
            token.to_ascii_lowercase()
        );
        let raw = req
            .header(&header_name)
            .map_err(RpcError::header_mismatch)?;
        let value = args.get(prop).filter(|v| !v.is_null());
        let rendered = value.and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Bool(b) => Some(b.to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        });
        match (rendered, raw) {
            (None, None) => {}
            (None, Some(_)) => {
                return Err(RpcError::header_mismatch(format!(
                    "Header mismatch: Mcp-Param-{token} is present but argument '{prop}' is absent"
                )))
            }
            (Some(_), None) => {
                return Err(RpcError::header_mismatch(format!(
                    "Header mismatch: Mcp-Param-{token} header is required because argument '{prop}' is present"
                )))
            }
            (Some(body), Some(raw)) => {
                let decoded = protocol::decode_header_value(raw).ok_or_else(|| {
                    RpcError::header_mismatch(format!(
                        "Header mismatch: Mcp-Param-{token} has a malformed base64 value"
                    ))
                })?;
                let numeric_eq = matches!(
                    (decoded.parse::<f64>(), body.parse::<f64>()),
                    (Ok(a), Ok(b)) if a == b
                ) && spec.get("type").and_then(Value::as_str) == Some("integer");
                if decoded != body && !numeric_eq {
                    return Err(RpcError::header_mismatch(format!(
                        "Header mismatch: Mcp-Param-{token} header value '{decoded}' does not match body value '{body}'"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Scope check for tools that need one (protected variant only).
fn scope_check(req: &Req, id: &Value, params: &Value) -> Option<Response> {
    if req.profile.access != Access::OAuth {
        return None;
    }
    let name = params.get("name").and_then(Value::as_str)?;
    let scope = tools::find(&req.profile, name)?.required_scope?;
    let claims = req.claims.as_ref()?;
    if auth::has_scope(claims, scope) {
        return None;
    }
    let err = RpcError::new(
        protocol::SERVER_ERROR,
        format!("Forbidden: tool {name} requires the {scope} scope"),
    )
    .with_data(json!({ "requiredScope": scope }));
    Some(auth::insufficient_scope(
        &req.base_url,
        scope,
        protocol::error_response(Some(id), &err),
    ))
}

async fn modern_post(mut req: Req, msg: Message, accept: (bool, bool)) -> Response {
    match msg {
        Message::Request { id, method, params } => {
            let (version, caps, log_level) = match modern_ladder(&req, &method, &params) {
                Ok(v) => v,
                Err(e) => {
                    let status = e.modern_http_status();
                    return decorate(rpc_error(status, Some(&id), &e), &req.echo);
                }
            };
            req.echo.version = Some(version.as_str());
            req.echo.session = Some("stateless".into());
            if method == "tools/call" {
                if let Err(e) = validate_param_headers(&req, &params) {
                    return decorate(rpc_error(StatusCode::BAD_REQUEST, Some(&id), &e), &req.echo);
                }
                if let Some(resp) = scope_check(&req, &id, &params) {
                    return decorate(resp, &req.echo);
                }
            }
            if method == "subscriptions/listen" {
                if !accept.1 {
                    let e = RpcError::invalid_request(
                        "subscriptions/listen requires Accept: text/event-stream",
                    );
                    return decorate(
                        rpc_error(StatusCode::NOT_ACCEPTABLE, Some(&id), &e),
                        &req.echo,
                    );
                }
                return listen(req, id, version, params);
            }
            let ctx = make_ctx(&req, version, caps, log_level, &id, &params, None);
            let echo = req.echo.clone();
            let resp = run(ctx, id, method, params, accept, true).await;
            decorate(resp, &echo)
        }
        Message::Notification { .. } => {
            // 2026-07-28 defines no client notifications over HTTP: accept and drop
            // at a served version.
            let h = req
                .header(protocol::HEADER_PROTOCOL_VERSION)
                .ok()
                .flatten()
                .unwrap_or("");
            match Version::parse(h) {
                Some(v) if v.is_modern() => {
                    decorate(StatusCode::ACCEPTED.into_response(), &req.echo)
                }
                _ => decorate(
                    rpc_error(
                        StatusCode::BAD_REQUEST,
                        None,
                        &RpcError::unsupported_version(h),
                    ),
                    &req.echo,
                ),
            }
        }
        Message::Response { .. } => decorate(
            rpc_error(
                StatusCode::BAD_REQUEST,
                None,
                &RpcError::invalid_request(
                    "Clients must not send JSON-RPC responses at protocol 2026-07-28",
                ),
            ),
            &req.echo,
        ),
    }
}

fn make_ctx(
    req: &Req,
    version: Version,
    caps: Value,
    log_level: Option<usize>,
    id: &Value,
    params: &Value,
    session: Option<Arc<Session>>,
) -> CallCtx {
    CallCtx {
        shared: req.sh.clone(),
        profile: req.profile.clone(),
        version,
        client_caps: caps,
        log_level,
        progress_token: protocol::progress_token(params),
        sink: None,
        cancel: CancelToken::new(),
        request_id: id.clone(),
        session,
        http: req.info.clone(),
        auth: req.claims.clone(),
        page_size: req.page_size,
    }
}

fn sse_event(msg: &Value) -> Event {
    Event::default().event("message").data(msg.to_string())
}

fn sse_response<S>(stream: S, keepalive: std::time::Duration) -> Response
where
    S: Stream<Item = Result<Event, Infallible>> + Send + 'static,
{
    let mut resp = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(keepalive))
        .into_response();
    let h = resp.headers_mut();
    h.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    resp
}

/// The final JSON-RPC message for a finished request.
fn final_message(id: &Value, outcome: Result<Value, RpcError>) -> (Value, Option<RpcError>) {
    match outcome {
        Ok(v) => (protocol::result_response(id, v), None),
        Err(e) => (protocol::error_response(Some(id), &e), Some(e)),
    }
}

/// Run one request and answer with JSON or SSE.
///
/// With SSE allowed, the response commits to `text/event-stream` as soon as
/// the handler emits a notification or server request (or after the
/// deferral window), otherwise the result is sent as one JSON object.
/// `modern`: closing the SSE stream cancels the request (2026-07-28); in the
/// handshake era a disconnect is not a cancellation (`notifications/cancelled` is).
pub async fn run(
    mut ctx: CallCtx,
    id: Value,
    method: String,
    params: Value,
    (json_ok, sse_ok): (bool, bool),
    modern: bool,
) -> Response {
    // Static list results are rendered once; such a request never emits a
    // notification, so the answer is the same JSON object the dispatch
    // below would produce (the SSE-only case keeps the normal path).
    if json_ok {
        if let Some(result) = core::cached_list(&ctx, &method, &params) {
            let body = protocol::result_response_json(&id, &result);
            return (
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                )],
                body,
            )
                .into_response();
        }
    }
    let cancel = ctx.cancel.clone();
    let session = ctx.session.clone();
    if let Some(s) = &session {
        s.track_inflight(&id, cancel.clone());
    }
    let (tx, mut rx) = mpsc::channel::<Value>(64);
    if sse_ok {
        ctx.sink = Some(tx);
    } else {
        drop(tx);
    }
    let deferral = ctx.shared.cfg.sse_deferral;
    let keepalive = ctx.shared.cfg.keepalive;
    let task_session = session.clone();
    let task_id = id.clone();
    let mut handle = tokio::spawn(async move {
        let out = core::dispatch(&ctx, &method, &params).await;
        if let Some(s) = &task_session {
            s.finish_inflight(&task_id);
        }
        out
    });
    let join = |r: Result<Result<Value, RpcError>, tokio::task::JoinError>| {
        r.unwrap_or_else(|e| Err(RpcError::internal(format!("handler failed: {e}"))))
    };

    // 2026-07-28: the client going away (before or during the response) cancels.
    let guard = modern.then(|| cancel.drop_guard());
    if !sse_ok {
        let outcome = join(handle.await);
        if cancel.is_cancelled() && !modern {
            let e = RpcError::new(protocol::SERVER_ERROR, "Request cancelled");
            return rpc_error(StatusCode::OK, Some(&id), &e);
        }
        let (body, err) = final_message(&id, outcome);
        let status = match (&err, modern) {
            (Some(e), true) => e.modern_http_status(),
            _ => StatusCode::OK,
        };
        return (status, Json(body)).into_response();
    }

    // Wait for the result, a first message, or the deferral window.
    let mut first: Option<Value> = None;
    let mut done: Option<Result<Value, RpcError>> = None;
    tokio::select! {
        r = &mut handle => done = Some(join(r)),
        Some(m) = rx.recv() => first = Some(m),
        _ = tokio::time::sleep(deferral) => {}
    }
    if let Some(outcome) = done.take() {
        let mut queued = Vec::new();
        while let Ok(m) = rx.try_recv() {
            queued.push(m);
        }
        if queued.is_empty() && json_ok {
            let (body, err) = final_message(&id, outcome);
            let status = match (&err, modern) {
                (Some(e), true) => e.modern_http_status(),
                _ => StatusCode::OK,
            };
            return (status, Json(body)).into_response();
        }
        let (final_msg, _) = final_message(&id, outcome);
        let stream = async_stream::stream! {
            for m in queued {
                yield Ok::<Event, Infallible>(sse_event(&m));
            }
            yield Ok(sse_event(&final_msg));
        };
        return sse_response(stream, keepalive);
    }

    let stream = async_stream::stream! {
        let _guard = guard;
        if let Some(m) = first {
            yield Ok::<Event, Infallible>(sse_event(&m));
        }
        while let Some(m) = rx.recv().await {
            yield Ok(sse_event(&m));
        }
        let outcome = join(handle.await);
        // A request cancelled with notifications/cancelled gets no response.
        if !(cancel.is_cancelled() && !modern) {
            let (final_msg, _) = final_message(&id, outcome);
            yield Ok(sse_event(&final_msg));
        }
    };
    sse_response(stream, keepalive)
}

/// `subscriptions/listen`: acknowledgement, then `notifications/resources/updated`
/// for subscribed URIs, until the client disconnects or the lifetime cap.
fn listen(req: Req, id: Value, version: Version, params: Value) -> Response {
    let Some(filter) = params.get("notifications").filter(|f| f.is_object()) else {
        let e = RpcError::invalid_params("Missing required parameter: notifications");
        return decorate(rpc_error(StatusCode::BAD_REQUEST, Some(&id), &e), &req.echo);
    };
    let requested: Vec<String> = filter
        .get("resourceSubscriptions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let honoured: Vec<String> = requested
        .into_iter()
        .filter(|u| resources::exists(&req.profile, u))
        .take(super::sessions::MAX_SUBSCRIPTIONS)
        .collect();
    let mut acked = serde_json::Map::new();
    if !honoured.is_empty() {
        acked.insert("resourceSubscriptions".into(), json!(honoured));
    }
    let sub_meta = json!({ protocol::META_SUBSCRIPTION_ID: id });
    let ack = protocol::notification(
        "notifications/subscriptions/acknowledged",
        json!({ "_meta": sub_meta, "notifications": acked }),
    );
    let tick = req.sh.cfg.tick;
    let max = req.sh.cfg.max_stream;
    let keepalive = req.sh.cfg.keepalive;
    let ticking = honoured.iter().any(|u| u == resources::CLOCK_URI);
    let closing = protocol::result_response(
        &id,
        json!({
            "resultType": "complete",
            "_meta": {
                protocol::META_SUBSCRIPTION_ID: id,
                protocol::META_SERVER_INFO: core::server_info(&req.profile, version),
            }
        }),
    );
    let stream = async_stream::stream! {
        yield Ok::<Event, Infallible>(sse_event(&ack));
        let deadline = tokio::time::Instant::now() + max;
        let mut interval = tokio::time::interval(tick);
        interval.tick().await;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if ticking {
                        let n = protocol::notification(
                            "notifications/resources/updated",
                            json!({ "uri": resources::CLOCK_URI, "_meta": { protocol::META_SUBSCRIPTION_ID: id } }),
                        );
                        yield Ok(sse_event(&n));
                    }
                }
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        // Graceful closure: answer the listen request.
        yield Ok(sse_event(&closing));
    };
    decorate(sse_response(stream, keepalive), &req.echo)
}

// ── Handshake era (2025-11-25, 2025-06-18, 2025-03-26) ──────────────

/// Result of `initialize` (shared with the HTTP+SSE transport).
pub fn initialize_result(profile: &Profile, session: &Session, params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = protocol::negotiate_legacy(requested);
    {
        let mut st = session.state();
        st.version = version;
        st.client_capabilities = params
            .get("capabilities")
            .filter(|c| c.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        st.client_info = params.get("clientInfo").cloned().unwrap_or(Value::Null);
    }
    json!({
        "protocolVersion": version.as_str(),
        "capabilities": core::capabilities(profile, version),
        "serverInfo": core::server_info(profile, version),
        "instructions": core::instructions(profile),
    })
}

/// Handle a client notification in a legacy session.
pub fn legacy_notification(session: &Session, method: &str, params: &Value) {
    match method {
        "notifications/initialized" => session.state().initialized = true,
        "notifications/cancelled" => {
            if let Some(id) = params.get("requestId") {
                session.cancel_inflight(id);
            }
        }
        _ => {}
    }
}

/// Look up the session named by `Mcp-Session-Id` for this profile/principal.
#[allow(clippy::result_large_err)]
fn require_session(req: &Req, id: Option<&Value>) -> Result<Arc<Session>, Response> {
    let sid = match req.header(protocol::HEADER_SESSION_ID) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Err(rpc_error(
                StatusCode::BAD_REQUEST,
                id,
                &RpcError::invalid_request("Bad Request: Mcp-Session-Id header is required (send initialize first, or use protocol 2026-07-28 with per-request _meta)"),
            ))
        }
        Err(e) => {
            return Err(rpc_error(StatusCode::BAD_REQUEST, id, &RpcError::invalid_request(e)))
        }
    };
    let session = req
        .sh
        .sessions
        .get(sid)
        .filter(|s| s.profile == req.profile.key && s.transport == TransportKind::StreamableHttp)
        .filter(|s| s.principal.is_none() || s.principal == req.principal());
    session.ok_or_else(|| {
        rpc_error(
            StatusCode::NOT_FOUND,
            id,
            &RpcError::new(protocol::SESSION_NOT_FOUND, "Session not found"),
        )
    })
}

/// Validate `MCP-Protocol-Version` on a legacy request (400 when unsupported).
#[allow(clippy::result_large_err)]
fn legacy_header_version(req: &Req, id: Option<&Value>) -> Result<Option<Version>, Response> {
    match req.header(protocol::HEADER_PROTOCOL_VERSION) {
        Ok(None) => Ok(None),
        Ok(Some(h)) => match Version::parse(h).filter(|v| !v.is_modern()) {
            Some(v) => Ok(Some(v)),
            None => Err(rpc_error(
                StatusCode::BAD_REQUEST,
                id,
                &RpcError::invalid_request(format!(
                    "Bad Request: unsupported MCP-Protocol-Version {h}"
                ))
                .with_data(json!({ "supported": Version::all_strings() })),
            )),
        },
        Err(e) => Err(rpc_error(
            StatusCode::BAD_REQUEST,
            id,
            &RpcError::invalid_request(e),
        )),
    }
}

async fn legacy_post(mut req: Req, msg: Message, accept: (bool, bool)) -> Response {
    let id = match &msg {
        Message::Request { id, .. } => Some(id.clone()),
        _ => None,
    };
    if let Err(resp) = legacy_header_version(&req, id.as_ref()) {
        return decorate(resp, &req.echo);
    }
    if let Message::Request { id, method, params } = &msg {
        if method == "initialize" {
            let session = Arc::new(Session::new(
                &req.profile.key,
                TransportKind::StreamableHttp,
                Version::LATEST_LEGACY,
                req.principal(),
            ));
            let result = initialize_result(&req.profile, &session, params);
            req.sh.sessions.insert(session.clone());
            req.echo.session = Some(session.id.clone());
            req.echo.version = Some(session.version().as_str());
            let mut resp = Json(protocol::result_response(id, result)).into_response();
            if let Ok(v) = HeaderValue::from_str(&session.id) {
                resp.headers_mut()
                    .insert(HeaderName::from_static(protocol::HEADER_SESSION_ID), v);
            }
            return decorate(resp, &req.echo);
        }
    }
    let session = match require_session(&req, id.as_ref()) {
        Ok(s) => s,
        Err(resp) => return decorate(resp, &req.echo),
    };
    req.echo.session = Some(session.id.clone());
    req.echo.version = Some(session.version().as_str());
    match msg {
        Message::Request { id, method, params } => {
            if method == "tools/call" {
                if let Some(resp) = scope_check(&req, &id, &params) {
                    return decorate(resp, &req.echo);
                }
            }
            let (version, caps, level) = {
                let st = session.state();
                (
                    st.version,
                    st.client_capabilities.clone(),
                    st.log_level.as_deref().and_then(protocol::log_level_rank),
                )
            };
            let ctx = make_ctx(&req, version, caps, level, &id, &params, Some(session));
            let echo = req.echo.clone();
            decorate(run(ctx, id, method, params, accept, false).await, &echo)
        }
        Message::Notification { method, params } => {
            legacy_notification(&session, &method, &params);
            decorate(StatusCode::ACCEPTED.into_response(), &req.echo)
        }
        Message::Response { id, body } => {
            session.resolve_pending(&id, body);
            decorate(StatusCode::ACCEPTED.into_response(), &req.echo)
        }
    }
}

/// JSON-RPC batches (2025-03-26 sessions only; removed in 2025-06-18).
async fn batch(mut req: Req, items: Vec<Value>) -> Response {
    let reject = |req: &Req, msg: &str| {
        decorate(
            rpc_error(
                StatusCode::BAD_REQUEST,
                None,
                &RpcError::invalid_request(msg),
            ),
            &req.echo,
        )
    };
    if items.is_empty() || items.len() > 50 {
        return reject(&req, "Invalid Request: batch must hold 1 to 50 messages");
    }
    let session = match require_session(&req, None) {
        Ok(s) => s,
        Err(_) => return reject(
            &req,
            "Invalid Request: JSON-RPC batches are only supported in protocol 2025-03-26 sessions",
        ),
    };
    if session.version() != Version::V2025_03_26 {
        return reject(
            &req,
            "Invalid Request: JSON-RPC batches are only supported in protocol 2025-03-26 sessions",
        );
    }
    req.echo.session = Some(session.id.clone());
    req.echo.version = Some(Version::V2025_03_26.as_str());
    let mut out = Vec::new();
    for item in items {
        match protocol::classify(&item) {
            Err((e, id)) => out.push(protocol::error_response(id.as_ref(), &e)),
            Ok(Message::Notification { method, params }) => {
                legacy_notification(&session, &method, &params)
            }
            Ok(Message::Response { id, body }) => {
                session.resolve_pending(&id, body);
            }
            Ok(Message::Request { id, method, params }) => {
                let (caps, level) = {
                    let st = session.state();
                    (
                        st.client_capabilities.clone(),
                        st.log_level.as_deref().and_then(protocol::log_level_rank),
                    )
                };
                let ctx = make_ctx(
                    &req,
                    Version::V2025_03_26,
                    caps,
                    level,
                    &id,
                    &params,
                    Some(session.clone()),
                );
                let outcome = core::dispatch(&ctx, &method, &params).await;
                out.push(final_message(&id, outcome).0);
            }
        }
    }
    if out.is_empty() {
        return decorate(StatusCode::ACCEPTED.into_response(), &req.echo);
    }
    decorate(Json(Value::Array(out)).into_response(), &req.echo)
}

/// Is the GET/DELETE addressed to the stateless era (no streams, no sessions)?
fn modern_only(req: &Req) -> bool {
    req.header(protocol::HEADER_PROTOCOL_VERSION)
        .ok()
        .flatten()
        .is_some_and(|h| !protocol::is_legacy_version_str(h))
}

fn method_not_allowed(req: &Req) -> Response {
    let mut resp = plain_error(
        StatusCode::METHOD_NOT_ALLOWED,
        "Method Not Allowed: protocol 2026-07-28 has no GET stream or sessions (use POST)",
    );
    resp.headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    decorate(resp, &req.echo)
}

/// GET: the standalone server-to-client SSE stream of a legacy session.
fn get_stream(mut req: Req) -> Response {
    if modern_only(&req) {
        return method_not_allowed(&req);
    }
    let (_, sse_ok) = accepts(req.headers());
    if !sse_ok {
        return decorate(
            plain_error(
                StatusCode::NOT_ACCEPTABLE,
                "Not Acceptable: GET requires Accept: text/event-stream",
            ),
            &req.echo,
        );
    }
    let session = match require_session(&req, None) {
        Ok(s) => s,
        Err(resp) => return decorate(resp, &req.echo),
    };
    req.echo.session = Some(session.id.clone());
    req.echo.version = Some(session.version().as_str());
    let (tx, rx) = mpsc::channel::<Value>(64);
    {
        let mut st = session.state();
        if st.outbound.as_ref().is_some_and(|t| !t.is_closed()) {
            drop(st);
            return decorate(
                plain_error(
                    StatusCode::CONFLICT,
                    "Conflict: a GET stream is already open for this session",
                ),
                &req.echo,
            );
        }
        st.outbound = Some(tx);
    }
    let stream = session_stream(req.sh.clone(), session, rx, None);
    decorate(sse_response(stream, req.sh.cfg.keepalive), &req.echo)
}

/// A long-lived session stream: queued messages, clock ticks for subscribed
/// sessions, bounded by the session lifetime and `max_stream`. `first` is
/// sent before anything else (the HTTP+SSE `endpoint` event).
pub fn session_stream(
    sh: Arc<Shared>,
    session: Arc<Session>,
    mut rx: mpsc::Receiver<Value>,
    first: Option<Event>,
) -> impl Stream<Item = Result<Event, Infallible>> + Send + 'static {
    async_stream::stream! {
        struct Cleanup(Arc<Session>, Arc<Shared>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                self.0.state().outbound = None;
                if self.0.transport == TransportKind::HttpSse {
                    self.1.sessions.remove(&self.0.id);
                }
            }
        }
        let _cleanup = Cleanup(session.clone(), sh.clone());
        if let Some(ev) = first {
            yield Ok::<Event, Infallible>(ev);
        }
        let deadline = tokio::time::Instant::now() + sh.cfg.max_stream;
        let mut interval = tokio::time::interval(sh.cfg.tick);
        interval.tick().await;
        loop {
            tokio::select! {
                m = rx.recv() => match m {
                    Some(m) => yield Ok(sse_event(&m)),
                    None => break,
                },
                _ = interval.tick() => {
                    let subscribed = session.state().subscriptions.contains(resources::CLOCK_URI);
                    if subscribed {
                        let n = protocol::notification(
                            "notifications/resources/updated",
                            json!({ "uri": resources::CLOCK_URI }),
                        );
                        yield Ok(sse_event(&n));
                    }
                }
                _ = session.closed.cancelled() => break,
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
    }
}

/// DELETE: terminate a legacy session.
fn delete(mut req: Req) -> Response {
    if modern_only(&req) {
        return method_not_allowed(&req);
    }
    let session = match require_session(&req, None) {
        Ok(s) => s,
        Err(resp) => return decorate(resp, &req.echo),
    };
    req.sh.sessions.remove(&session.id);
    req.echo.session = Some(session.id.clone());
    decorate(
        (StatusCode::OK, Json(json!({ "terminated": session.id }))).into_response(),
        &req.echo,
    )
}
