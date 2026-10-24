//! The MCP server of an OpenAPI mock: `/mock/{name}/_rustybin/mcp`
//! (`docs/plans/mock-mcp.md`).
//!
//! A third profile family on the protocol and transport code of `/mcp`: one
//! tool per operation of the mock's live revision
//! ([`crate::mocks::mcp`]), plus the served spec as a resource. Every tool
//! call becomes a request to the operation and runs in process through the
//! mock engine ([`crate::mocks::handlers::serve`]): rules, examples,
//! stateful data, mocked security, delays, visitor restrictions and capture
//! all apply, so REST and MCP answers never differ.
//!
//! The endpoint is a data-plane route like the rest of `/mock/*` (never
//! behind control auth). Credentials for mocked security come from the MCP
//! request: its `Authorization` and `Cookie` headers and the API key
//! headers and query parameters of the mock's schemes are forwarded into
//! each in-process request.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::Response;
use axum::Extension;
use serde_json::{json, Value};

use super::core::{self, CallCtx};
use super::protocol::{self, RpcError, Version};
use super::tools::{text, tool_error};
use super::{transport_streamable, Profile, Shared};
use crate::mocks::compile::Compiled;
use crate::mocks::handlers;
use crate::mocks::host::ViaMocksDomain;
use crate::mocks::mcp::{Tool, BODY_ARG};
use crate::mocks::spec::{self, Location, SchemeKind};
use crate::mocks::store::Lookup;
use crate::server::TlsConnectionInfo;
use crate::state::AppState;

/// Session namespace prefix (`mock:<name>`).
pub const PROFILE_PREFIX: &str = "mock:";
/// Longest instructions text taken from the spec description.
const MAX_SPEC_DESCRIPTION: usize = 600;
/// Text content of a result when structured content carries the full body.
const MAX_TEXT_WITH_STRUCTURED: usize = 32 * 1024;
/// `_meta` key naming the operation behind a tool.
pub const META_OPERATION: &str = "rustybin.dev/operation";

/// The endpoint of the mock `name`.
pub fn path_for(name: &str) -> String {
    format!("/mock/{name}/_rustybin/mcp")
}

/// The mock a request addresses, resolved once per HTTP request from the
/// live revision (so one request sees one revision throughout).
pub struct MockTarget {
    pub name: String,
    pub compiled: Arc<Compiled>,
    pub visitor: bool,
    pub via_mocks_domain: bool,
    pub tls: Option<TlsConnectionInfo>,
    /// `servers[0].url` and identity provider base of the served spec.
    pub spec_server: String,
    pub idp_base: String,
}

impl std::fmt::Debug for MockTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockTarget")
            .field("name", &self.name)
            .field("revision", &self.compiled.revision)
            .field("visitor", &self.visitor)
            .finish()
    }
}

/// Control characters removed, at most `max` characters.
fn clean(text: &str, max: usize) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

impl MockTarget {
    pub fn title(&self) -> String {
        let t = clean(&self.compiled.spec.title, 120);
        if t.is_empty() {
            format!("Rustybin mock {}", self.name)
        } else {
            t
        }
    }

    pub fn server_description(&self) -> String {
        format!(
            "MCP server of the Rustybin mock {:?}: one tool per operation of {} {}. The answers \
             are mocked.",
            self.name,
            self.title(),
            clean(&self.compiled.spec.version, 40)
        )
    }

    pub fn instructions(&self) -> String {
        let mut text = format!(
            "Tools of the API {} ({}), one per operation, served by the Rustybin mock {:?}. Every \
             answer comes from a mock (spec examples, generated data, configured rules or stored \
             test data), never from a real system. A tool result carries the HTTP status and body \
             the REST call would have returned; a status outside 2xx is a tool error. The \
             OpenAPI description is the resource {}.",
            self.title(),
            clean(&self.compiled.spec.version, 40),
            self.name,
            self.spec_uri()
        );
        if let Some(d) = self
            .compiled
            .spec
            .description
            .as_deref()
            .map(|d| clean(d, MAX_SPEC_DESCRIPTION))
            .filter(|d| !d.is_empty())
        {
            text.push_str("\n\nAbout the API: ");
            text.push_str(&d);
        }
        text
    }

    pub fn spec_uri(&self) -> String {
        format!("rustybin://mock/{}/openapi.json", self.name)
    }
}

/// `404` with a JSON-RPC style body.
fn not_found(message: &str, name: Option<&str>) -> Response {
    let mut err = RpcError::invalid_request(message);
    if let Some(n) = name {
        err = err.with_data(json!({ "mock": n }));
    }
    transport_streamable::rpc_error(StatusCode::NOT_FOUND, None, &err)
}

/// One answer for every reason a visitor's mock has no MCP server, so other
/// people's mock names cannot be probed.
const NO_SERVER: &str = "no MCP server at this URL: the mock does not exist, is not deployed or \
                         switched off, or its MCP server is disabled";

/// `POST, GET, DELETE /mock/{name}/_rustybin/mcp`.
pub async fn handler(
    State(app): State<AppState>,
    Extension(sh): Extension<Arc<Shared>>,
    Path(name): Path<String>,
    tls: Option<Extension<TlsConnectionInfo>>,
    via: Option<Extension<ViaMocksDomain>>,
    req: Request,
) -> Response {
    let live = match app.mocks.lookup(&name) {
        Lookup::Missing => return not_found(NO_SERVER, None),
        Lookup::NotDeployed(..) => {
            return not_found(
                "this mock has not been deployed yet: deploy it to get its MCP server",
                Some(&name),
            )
        }
        Lookup::Ready(live) => live,
    };
    let via_mocks_domain = via.is_some();
    // A visitor mock answers only on the mocks domain when there is one.
    if live.visitor && !via_mocks_domain && app.config.self_service.host.is_some() {
        return not_found(NO_SERVER, None);
    }
    let c = live.compiled.clone();
    let reason = if !live.enabled {
        Some("this mock is switched off")
    } else if !c.settings.mcp.enabled {
        Some("the MCP server of this mock is disabled (settings mcp.enabled)")
    } else {
        None
    };
    if let Some(reason) = reason {
        return if live.visitor {
            not_found(NO_SERVER, None)
        } else {
            not_found(reason, Some(&name))
        };
    }
    let (spec_server, idp_base) = handlers::served_bases(
        &c,
        &name,
        req.headers(),
        req.extensions(),
        req.uri(),
        &app.config,
    );
    let target = MockTarget {
        name,
        compiled: c,
        visitor: live.visitor,
        via_mocks_domain,
        tls: tls.map(|t| t.0),
        spec_server,
        idp_base,
    };
    let profile = Arc::new(Profile::openapi(Arc::new(target)));
    transport_streamable::handle(app, sh, Some(profile), req).await
}

/// Method dispatch for an OpenAPI mock's profile (called by
/// [`core::dispatch`]).
pub async fn dispatch(ctx: &CallCtx, method: &str, params: &Value) -> Result<Value, RpcError> {
    let Some(target) = ctx.profile.mock.clone() else {
        return Err(RpcError::internal("no mock for this MCP server"));
    };
    let modern = ctx.version.is_modern();
    let result = match method {
        "tools/list" => tools_list(ctx, &target, params)?,
        "tools/call" => call(ctx, &target, params).await?,
        "resources/list" => core::cacheable(
            ctx,
            json!({ "resources": [spec_resource(&target)] }),
            60_000,
            "public",
        ),
        "resources/templates/list" => {
            core::cacheable(ctx, json!({ "resourceTemplates": [] }), 300_000, "public")
        }
        "resources/read" => read_resource(ctx, &target, params)?,
        "server/discover" if modern => json!({
            "supportedVersions": Version::all_strings(),
            "capabilities": core::capabilities(&ctx.profile, ctx.version),
            "instructions": core::instructions(&ctx.profile),
            "ttlMs": 60_000,
            "cacheScope": "public",
        }),
        "ping" if !modern => json!({}),
        "logging/setLevel" if !modern => {
            let level = params
                .get("level")
                .and_then(Value::as_str)
                .filter(|l| protocol::log_level_rank(l).is_some())
                .ok_or_else(|| RpcError::invalid_params("Invalid log level"))?;
            if let Some(session) = &ctx.session {
                session.state().log_level = Some(level.to_string());
            }
            json!({})
        }
        _ => return Err(RpcError::method_not_found(method)),
    };
    Ok(result)
}

/// A tool as sent in `tools/list` for a protocol version.
pub fn tool_json(t: &Tool, version: Version) -> Value {
    let mut v = json!({
        "name": t.name,
        "description": t.description,
        "inputSchema": t.input_schema,
        "_meta": { META_OPERATION: {
            "operationId": t.operation,
            "method": t.method,
            "path": t.path,
        } },
    });
    if version.has_structured_output() {
        if let Some(title) = &t.title {
            v["title"] = json!(title);
        }
        if let Some(out) = &t.output_schema {
            v["outputSchema"] = out.clone();
        }
    }
    if version.has_annotations() {
        let mut a = json!({
            "readOnlyHint": t.read_only,
            "destructiveHint": t.destructive,
            "idempotentHint": t.idempotent,
            "openWorldHint": false,
        });
        if let Some(title) = &t.title {
            a["title"] = json!(title);
        }
        v["annotations"] = a;
    }
    v
}

fn tools_list(ctx: &CallCtx, target: &MockTarget, params: &Value) -> Result<Value, RpcError> {
    let set = target.compiled.mcp_tools();
    let (page, next) = core::paginate(&set.tools, params, ctx.page_size, "tools")?;
    let tools: Vec<Value> = page.iter().map(|t| tool_json(t, ctx.version)).collect();
    let mut result = json!({ "tools": tools });
    if let Some(next) = next {
        result["nextCursor"] = json!(next);
    }
    Ok(core::cacheable(ctx, result, 60_000, "public"))
}

fn spec_resource(target: &MockTarget) -> Value {
    json!({
        "uri": target.spec_uri(),
        "name": "openapi.json",
        "title": format!("OpenAPI description of {}", target.title()),
        "description": "The OpenAPI document this MCP server's tools are built from, as the mock serves it.",
        "mimeType": "application/json",
    })
}

fn read_resource(ctx: &CallCtx, target: &MockTarget, params: &Value) -> Result<Value, RpcError> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("Missing required parameter: uri"))?;
    if uri != target.spec_uri() {
        return Err(RpcError::resource_not_found(uri, ctx.version));
    }
    let doc = handlers::served_spec(
        &target.compiled,
        &target.spec_server,
        &target.idp_base,
        &target.name,
    );
    let body = serde_json::to_string_pretty(&doc).unwrap_or_default();
    Ok(json!({
        "contents": [{ "uri": uri, "mimeType": "application/json", "text": body }]
    }))
}

// ── Tool calls ──────────────────────────────────────────────────────

/// The in-process request a tool call becomes.
struct Built {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
}

/// Percent-encode everything but unreserved characters (path segments).
fn encode_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A scalar argument as text (`None` for objects and arrays).
fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Headers of the MCP request that carry credentials for mocked security.
fn credential_headers(c: &Compiled) -> Vec<String> {
    let mut names = vec![
        "authorization".to_string(),
        c.settings.security.api_key.header.to_ascii_lowercase(),
    ];
    for s in &c.spec.security_schemes {
        if let SchemeKind::ApiKey {
            name,
            location: Location::Header,
        } = &s.kind
        {
            names.push(name.to_ascii_lowercase());
        }
    }
    names
}

/// Query parameters of the MCP request that carry API keys.
fn credential_query(c: &Compiled) -> Vec<String> {
    c.spec
        .security_schemes
        .iter()
        .filter_map(|s| match &s.kind {
            SchemeKind::ApiKey {
                name,
                location: Location::Query,
            } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// The request for a tool call, or the problems with the arguments.
fn build_request(
    ctx: &CallCtx,
    target: &MockTarget,
    tool: &Tool,
    args: &serde_json::Map<String, Value>,
) -> Result<Built, Vec<String>> {
    let c = &target.compiled;
    let mut problems = Vec::new();
    for key in args.keys() {
        let known =
            tool.args.iter().any(|a| &a.name == key) || (key == BODY_ARG && tool.body.is_some());
        if !known {
            problems.push(format!("unknown argument {key:?}"));
        }
    }
    let present = |name: &str| args.get(name).filter(|v| !v.is_null());
    for a in tool.args.iter().filter(|a| a.required) {
        if present(&a.name).is_none() {
            problems.push(format!("missing required argument {:?}", a.name));
        }
    }
    if let Some(b) = tool.body.as_ref().filter(|b| b.required) {
        if present(BODY_ARG).is_none() {
            problems.push(format!(
                "missing required argument {BODY_ARG:?} (the {} request body)",
                b.media_type
            ));
        }
    }

    let mut path = tool.path.clone();
    let mut query = form_urlencoded::Serializer::new(String::new());
    let mut has_query = false;
    let mut headers = HeaderMap::new();
    let mut cookies: Vec<String> = Vec::new();
    for a in &tool.args {
        let Some(v) = present(&a.name) else {
            continue;
        };
        match a.location {
            Location::Path => match scalar(v) {
                Some(s) => {
                    path = path.replace(&format!("{{{}}}", a.param), &encode_segment(&s));
                }
                None => problems.push(format!(
                    "argument {:?} must be a string, number or boolean",
                    a.name
                )),
            },
            Location::Query => {
                has_query = true;
                match v {
                    Value::Array(items) => {
                        for item in items {
                            let s = scalar(item).unwrap_or_else(|| item.to_string());
                            query.append_pair(&a.param, &s);
                        }
                    }
                    Value::Object(_) => {
                        query.append_pair(&a.param, &v.to_string());
                    }
                    other => {
                        query.append_pair(&a.param, &scalar(other).unwrap_or_default());
                    }
                }
            }
            Location::Header => {
                let s = match v {
                    Value::Array(items) => items
                        .iter()
                        .map(|i| scalar(i).unwrap_or_else(|| i.to_string()))
                        .collect::<Vec<_>>()
                        .join(","),
                    other => scalar(other).unwrap_or_else(|| other.to_string()),
                };
                match (
                    HeaderName::from_bytes(a.param.as_bytes()),
                    HeaderValue::from_str(&s),
                ) {
                    (Ok(n), Ok(v)) => {
                        headers.append(n, v);
                    }
                    _ => {
                        problems.push(format!("argument {:?} is not a valid header value", a.name))
                    }
                }
            }
            Location::Cookie => {
                let s = scalar(v).unwrap_or_else(|| v.to_string());
                cookies.push(format!("{}={}", a.param, s));
            }
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }

    // Credentials from the MCP request.
    let incoming = &ctx.http.headers;
    for name in credential_headers(c) {
        if let Ok(n) = HeaderName::from_bytes(name.as_bytes()) {
            if headers.contains_key(&n) {
                continue;
            }
            for v in incoming.get_all(&n) {
                headers.append(n.clone(), v.clone());
            }
        }
    }
    let forwarded_cookie = incoming
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    cookies.extend(forwarded_cookie);
    if !cookies.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&cookies.join("; ")) {
            headers.insert(header::COOKIE, v);
        }
    }
    let incoming_query = ctx
        .http
        .uri
        .parse::<Uri>()
        .ok()
        .and_then(|u| u.query().map(str::to_string))
        .unwrap_or_default();
    for name in credential_query(c) {
        for (k, v) in form_urlencoded::parse(incoming_query.as_bytes()) {
            if k == name.as_str() {
                has_query = true;
                query.append_pair(&k, &v);
            }
        }
    }

    let mut body = Bytes::new();
    if let (Some(b), Some(v)) = (&tool.body, present(BODY_ARG)) {
        body = Bytes::from(serde_json::to_vec(v).unwrap_or_default());
        let media = if b.media_type.contains('*') {
            "application/json"
        } else {
            b.media_type.as_str()
        };
        if let Ok(ct) = HeaderValue::from_str(media) {
            headers.insert(header::CONTENT_TYPE, ct);
        }
    }

    let mut target_path = format!("/mock/{}{}{}", target.name, c.path_prefix(), path);
    if has_query {
        target_path.push('?');
        target_path.push_str(&query.finish());
    }
    let uri = target_path
        .parse::<Uri>()
        .map_err(|e| vec![format!("the arguments do not make a valid URL: {e}")])?;
    let method = Method::from_bytes(tool.method.as_bytes())
        .map_err(|_| vec![format!("unsupported method {}", tool.method)])?;
    Ok(Built {
        method,
        uri,
        headers,
        body,
    })
}

async fn call(ctx: &CallCtx, target: &MockTarget, params: &Value) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("Missing required parameter: name"))?;
    let set = target.compiled.mcp_tools();
    let Some(tool) = set.find(name) else {
        return Err(RpcError::invalid_params(format!("Unknown tool: {name}")));
    };
    let args = match params.get("arguments") {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return Err(RpcError::invalid_params("arguments must be an object")),
    };
    let built = match build_request(ctx, target, tool, &args) {
        Ok(b) => b,
        Err(problems) => {
            return Ok(tool_error(format!(
                "Invalid arguments for {name}: {}",
                problems.join("; ")
            )))
        }
    };
    ctx.log(
        "debug",
        json!({ "event": "tools/call", "tool": name, "operation": tool.operation }),
    )
    .await;
    let ip = ctx
        .http
        .client_ip
        .as_deref()
        .and_then(|s| s.parse::<IpAddr>().ok());
    let line = format!("{} {}", built.method, built.uri.path());
    let fut = handlers::serve(
        ctx.app.clone(),
        target.name.clone(),
        ip,
        target.tls.clone(),
        target.via_mocks_domain,
        built.method,
        built.uri,
        built.headers,
        built.body,
        Some(&tool.name),
    );
    tokio::pin!(fut);
    // Slow answers (rule delays) report progress every second.
    let started = Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.tick().await;
    let resp = loop {
        tokio::select! {
            resp = &mut fut => break resp,
            _ = ticker.tick() => {
                let waited = started.elapsed().as_secs_f64().floor();
                ctx.progress(waited, None, Some(format!("waiting for the mock to answer {line}"))).await;
            }
            _ = ctx.cancel.cancelled() => {
                return Err(RpcError::new(protocol::SERVER_ERROR, "Request cancelled"));
            }
        }
    };
    Ok(to_result(ctx, tool, &target.compiled, resp).await)
}

/// A mocked response as a tool result: `structuredContent` `{status, body}`
/// for JSON (and empty) bodies, with the same JSON as text; other bodies as
/// text. A status outside 2xx is a tool error.
async fn to_result(ctx: &CallCtx, tool: &Tool, c: &Compiled, resp: Response) -> Value {
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let cap = ctx.shared.cfg.max_large_output_kb.saturating_mul(1024);
    let bytes = match axum::body::to_bytes(resp.into_body(), cap).await {
        Ok(b) => b,
        Err(_) => {
            return tool_error(format!(
                "The mocked response ({status}) is larger than the {} KB a tool result may carry",
                ctx.shared.cfg.max_large_output_kb
            ))
        }
    };
    let is_error = !(200..300).contains(&status);
    let structured_ok = ctx.version.has_structured_output();
    let json_body = if bytes.is_empty() {
        Some(None)
    } else if content_type.as_deref().is_some_and(spec::is_json_media) {
        serde_json::from_slice::<Value>(&bytes).ok().map(Some)
    } else {
        None
    };
    let Some(body) = json_body else {
        // Text or binary: no structured content.
        let text_body = match std::str::from_utf8(&bytes) {
            Ok(s) => truncate(s, cap),
            Err(_) => format!(
                "<{} bytes of {}>",
                bytes.len(),
                content_type.as_deref().unwrap_or("binary data")
            ),
        };
        if !is_error && tool.output_schema.is_some() && structured_ok {
            return tool_error(format!(
                "The mock answered {status} with {} instead of the documented JSON body:\n{text_body}",
                content_type.as_deref().unwrap_or("no content type")
            ));
        }
        return json!({
            "content": [text(format!("{status}\n{text_body}"))],
            "isError": is_error,
        });
    };
    let mut structured = json!({ "status": status });
    if let Some(b) = &body {
        structured["body"] = b.clone();
    }
    let rendered = serde_json::to_string_pretty(&structured).unwrap_or_default();
    // Declared output must match: a mocked body that does not fit the
    // documented schema (a rule, an override) is reported, not passed on.
    if let (false, true, Some(schema), Some(b)) =
        (is_error, structured_ok, &tool.output_body, &body)
    {
        let issues = crate::mocks::schema::validate(
            b,
            schema,
            &c.spec.root,
            crate::mocks::schema::Direction::Response,
        );
        if !issues.is_empty() {
            let list: Vec<String> = issues
                .iter()
                .take(5)
                .map(|i| {
                    format!(
                        "{} {}",
                        if i.pointer.is_empty() {
                            "/"
                        } else {
                            &i.pointer
                        },
                        i.message
                    )
                })
                .collect();
            return tool_error(format!(
                "The mocked response ({status}) does not match the response schema documented for \
                 this operation: {}\n{}",
                list.join("; "),
                truncate(&rendered, MAX_TEXT_WITH_STRUCTURED)
            ));
        }
    }
    let text_cap = if structured_ok {
        MAX_TEXT_WITH_STRUCTURED
    } else {
        cap
    };
    let mut result = json!({
        "content": [text(truncate(&rendered, text_cap))],
        "isError": is_error,
    });
    if structured_ok {
        result["structuredContent"] = structured;
    }
    result
}

/// At most `max` bytes (on a character boundary), with a note when cut.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n... (truncated: {} of {} bytes shown)",
        &s[..end],
        end,
        s.len()
    )
}

#[cfg(test)]
mod tests;
