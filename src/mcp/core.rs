//! Transport-independent request handling: the per-request context, method
//! dispatch, capabilities, pagination and result post-processing.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::http::HeaderMap;
use base64::Engine;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use super::protocol::{self, RpcError, Version};
use super::sessions::{CancelToken, Session};
use super::{prompts, resources, tools, Profile, Shared};

/// What the transport knows about the HTTP request carrying the message.
#[derive(Clone, Debug, Default)]
pub struct HttpInfo {
    pub method: String,
    pub uri: String,
    pub headers: HeaderMap,
    pub client_ip: Option<String>,
    pub transport: &'static str,
}

/// Everything a method handler needs about the request being processed.
pub struct CallCtx {
    pub shared: Arc<Shared>,
    pub profile: Arc<Profile>,
    pub version: Version,
    pub client_caps: Value,
    /// Minimum log level rank (see `protocol::LOG_LEVELS`) or `None` (no logs).
    pub log_level: Option<usize>,
    pub progress_token: Option<Value>,
    /// Request-scoped outbound channel (SSE response stream). `None` when the
    /// response is plain JSON: notifications are then dropped.
    pub sink: Option<mpsc::Sender<Value>>,
    pub cancel: CancelToken,
    pub request_id: Value,
    pub session: Option<Arc<Session>>,
    pub http: Arc<HttpInfo>,
    /// Verified access token claims (protected variant).
    pub auth: Option<Value>,
    pub page_size: usize,
}

impl CallCtx {
    /// Send a notification on the request's stream. Returns false when there
    /// is no stream or the client went away.
    pub async fn notify(&self, method: &str, params: Value) -> bool {
        let Some(sink) = &self.sink else {
            return false;
        };
        if self.cancel.is_cancelled() {
            return false;
        }
        sink.send(protocol::notification(method, params))
            .await
            .is_ok()
    }

    /// `notifications/progress`, when the request carried a progress token.
    pub async fn progress(&self, progress: f64, total: Option<f64>, message: Option<String>) {
        let Some(token) = &self.progress_token else {
            return;
        };
        let mut params = json!({ "progressToken": token, "progress": progress });
        if let Some(total) = total {
            params["total"] = json!(total);
        }
        // `message` exists from 2025-03-26.
        if let Some(message) = message.filter(|_| self.version.has_annotations()) {
            params["message"] = json!(message);
        }
        self.notify("notifications/progress", params).await;
    }

    /// `notifications/message` at `level`, when the client opted in at or below it.
    pub async fn log(&self, level: &str, data: Value) {
        let (Some(min), Some(rank)) = (self.log_level, protocol::log_level_rank(level)) else {
            return;
        };
        if rank < min {
            return;
        }
        self.notify(
            "notifications/message",
            json!({ "level": level, "logger": "rustybin-mcp", "data": data }),
        )
        .await;
    }

    /// Does the client declare this capability (`elicitation`, `sampling`, ...)?
    pub fn client_has(&self, capability: &str) -> bool {
        self.client_caps
            .get(capability)
            .is_some_and(|v| v.is_object() || v.as_bool() == Some(true))
    }

    /// Can the server send a request to the client (legacy era only)?
    pub fn can_request_client(&self) -> bool {
        !self.version.is_modern() && self.sink.is_some() && self.session.is_some()
    }

    /// Legacy server-to-client request (elicitation, sampling): sent on the
    /// request's stream, answered by the client with a separate POST.
    pub async fn request_client(&self, method: &str, params: Value) -> Result<Value, String> {
        let (Some(sink), Some(session)) = (&self.sink, &self.session) else {
            return Err("no stream to send the request on".into());
        };
        let Some((id, rx)) = session.register_pending() else {
            return Err("too many pending client requests".into());
        };
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if sink.send(msg).await.is_err() {
            session.forget_pending(&id);
            return Err("client stream closed".into());
        }
        let timeout = self.shared.cfg.client_request_timeout;
        let outcome = tokio::select! {
            r = rx => r.map_err(|_| "session closed".to_string()),
            _ = tokio::time::sleep(timeout) => Err(format!("client did not answer within {}s", timeout.as_secs())),
            _ = self.cancel.cancelled() => Err("request cancelled".to_string()),
        };
        session.forget_pending(&id);
        let body = outcome?;
        if let Some(err) = body.get("error") {
            return Err(format!(
                "client returned error {}: {}",
                err.get("code").cloned().unwrap_or(Value::Null),
                err.get("message").and_then(Value::as_str).unwrap_or("")
            ));
        }
        Ok(body.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Sleep that ends early on cancellation; returns false when cancelled.
    pub async fn sleep(&self, d: Duration) -> bool {
        tokio::select! {
            _ = tokio::time::sleep(d) => !self.cancel.is_cancelled(),
            _ = self.cancel.cancelled() => false,
        }
    }

    /// Principal used to bind MRTR request state.
    pub fn principal(&self) -> String {
        self.auth
            .as_ref()
            .and_then(|c| c.get("sub"))
            .and_then(Value::as_str)
            .unwrap_or("anonymous")
            .to_string()
    }
}

/// `serverInfo` / `io.modelcontextprotocol/serverInfo` for a profile.
pub fn server_info(profile: &Profile, version: Version) -> Value {
    let mut info = json!({
        "name": profile.server_name(),
        "version": env!("CARGO_PKG_VERSION"),
    });
    if version.has_structured_output() {
        info["title"] = json!(profile.title());
    }
    if version >= Version::V2025_11_25 {
        info["description"] = json!(
            "Mock MCP server for API and AI gateway demos. All data is fake and deterministic."
        );
    }
    info
}

/// Server capabilities for a profile at a protocol version.
pub fn capabilities(profile: &Profile, version: Version) -> Value {
    let mut caps = Map::new();
    caps.insert("tools".into(), json!({ "listChanged": false }));
    caps.insert(
        "resources".into(),
        json!({ "subscribe": true, "listChanged": false }),
    );
    if !prompts::list(profile).is_empty() {
        caps.insert("prompts".into(), json!({ "listChanged": false }));
    }
    caps.insert("logging".into(), json!({}));
    if version.has_annotations() {
        caps.insert("completions".into(), json!({}));
    }
    Value::Object(caps)
}

pub fn instructions(profile: &Profile) -> String {
    format!(
        "Rustybin mock MCP server ({}). Everything is fake test data for gateway demos: \
         tools such as get_weather, lookup_customer and calculate are deterministic, \
         slow_task streams progress, fail/throw demonstrate error handling, and \
         prompt_injection_demo returns a clearly labelled injection sample for guardrail testing.",
        profile.title()
    )
}

/// Modern results carry `resultType` and `_meta.serverInfo`; legacy results
/// are returned as built.
pub fn finalize(ctx: &CallCtx, result: Value) -> Value {
    finalize_for(&ctx.profile, ctx.version, result)
}

fn finalize_for(profile: &Profile, version: Version, mut result: Value) -> Value {
    if !version.is_modern() {
        return result;
    }
    if let Some(obj) = result.as_object_mut() {
        obj.entry("resultType").or_insert_with(|| json!("complete"));
        let meta = obj.entry("_meta").or_insert_with(|| json!({}));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert(
                protocol::META_SERVER_INFO.into(),
                server_info(profile, version),
            );
        }
    }
    result
}

/// Add the CacheableResult fields (modern era only).
pub fn cacheable(ctx: &CallCtx, result: Value, ttl_ms: u64, scope: &str) -> Value {
    cacheable_for(ctx.version, result, ttl_ms, scope)
}

fn cacheable_for(version: Version, mut result: Value, ttl_ms: u64, scope: &str) -> Value {
    if version.is_modern() {
        result["ttlMs"] = json!(ttl_ms);
        result["cacheScope"] = json!(scope);
    }
    result
}

/// The list methods whose result depends only on the profile, the protocol
/// version and the requested page (never on the caller).
const STATIC_LISTS: [&str; 4] = [
    "tools/list",
    "resources/list",
    "resources/templates/list",
    "prompts/list",
];

/// Rendered complete results of the static list methods.
///
/// `tools/list` used to rebuild every tool schema with `json!` (hundreds of
/// small allocations), serialize the ~11 KB tree and free it again on every
/// request: about 10x the cost of other MCP methods. The unpaginated result
/// is now rendered once per (profile, method, protocol version) and spliced
/// into the JSON-RPC envelope ([`protocol::result_response_json`]), so the
/// bytes on the wire are unchanged. Bounded: 4 methods x 5 versions per profile.
#[derive(Debug, Default)]
pub struct ListCache {
    slots: [[OnceLock<Option<RenderedList>>; Version::ALL.len()]; STATIC_LISTS.len()],
}

#[derive(Debug)]
struct RenderedList {
    /// Number of items in the complete list (smaller pages take the normal path).
    total: usize,
    /// The serialized, finalized result object.
    json: Arc<str>,
}

/// The rendered result of a static list method asked for its first page
/// when that page holds the whole list; `None` otherwise (the caller then
/// dispatches normally).
pub fn cached_list(ctx: &CallCtx, method: &str, params: &Value) -> Option<Arc<str>> {
    let m = STATIC_LISTS.iter().position(|x| *x == method)?;
    if !matches!(params.get("cursor"), None | Some(Value::Null)) {
        return None;
    }
    let v = Version::ALL.iter().position(|x| *x == ctx.version)?;
    let cache = ctx.shared.list_cache.get(&ctx.profile.key)?;
    let rendered = cache.slots[m][v].get_or_init(|| {
        let no_params = Value::Object(Map::new());
        let (result, total) =
            list_method(&ctx.profile, ctx.version, method, &no_params, usize::MAX)?.ok()?;
        let result = finalize_for(&ctx.profile, ctx.version, result);
        let json = serde_json::to_string(&result).ok()?;
        Some(RenderedList {
            total,
            json: json.into(),
        })
    });
    rendered
        .as_ref()
        .filter(|r| r.total <= ctx.page_size)
        .map(|r| r.json.clone())
}

/// One page of a static list method (before [`finalize`]) and the total item
/// count; `None` when `method` is not a static list method.
fn list_method(
    profile: &Profile,
    version: Version,
    method: &str,
    params: &Value,
    page_size: usize,
) -> Option<Result<(Value, usize), RpcError>> {
    fn build<T: Clone>(
        all: &[T],
        params: &Value,
        page_size: usize,
        kind: &str,
        key: &str,
        render: impl Fn(&T) -> Value,
    ) -> Result<(Value, usize), RpcError> {
        let (page, next) = paginate(all, params, page_size, kind)?;
        let items: Vec<Value> = page.iter().map(render).collect();
        let mut r = Map::new();
        r.insert(key.into(), Value::Array(items));
        if let Some(next) = next {
            r.insert("nextCursor".into(), json!(next));
        }
        Ok((Value::Object(r), all.len()))
    }
    let (built, ttl_ms) = match method {
        "tools/list" => {
            let all = tools::list(profile);
            let render = |t: &&tools::ToolDef| tools::tool_json(t, version);
            (
                build(&all, params, page_size, "tools", "tools", render),
                300_000,
            )
        }
        "resources/list" => {
            let all = resources::list(profile);
            (
                build(
                    &all,
                    params,
                    page_size,
                    "resources",
                    "resources",
                    Value::clone,
                ),
                60_000,
            )
        }
        "resources/templates/list" => {
            let all = resources::templates(profile);
            let built = build(
                &all,
                params,
                page_size,
                "templates",
                "resourceTemplates",
                Value::clone,
            );
            (built, 300_000)
        }
        "prompts/list" => {
            let all = prompts::list(profile);
            if all.is_empty() {
                return Some(Err(RpcError::method_not_found(method)));
            }
            (
                build(&all, params, page_size, "prompts", "prompts", Value::clone),
                300_000,
            )
        }
        _ => return None,
    };
    Some(built.map(|(r, total)| (cacheable_for(version, r, ttl_ms, "public"), total)))
}

/// Cursor pagination over a list (opaque base64url cursor).
pub fn paginate<T: Clone>(
    items: &[T],
    params: &Value,
    page_size: usize,
    kind: &str,
) -> Result<(Vec<T>, Option<String>), RpcError> {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let start = match params.get("cursor") {
        None | Some(Value::Null) => 0,
        Some(Value::String(c)) => {
            let decoded = b64
                .decode(c)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .and_then(|s| {
                    s.strip_prefix(&format!("{kind}:"))
                        .and_then(|n| n.parse::<usize>().ok())
                });
            match decoded {
                Some(n) if n <= items.len() => n,
                _ => return Err(RpcError::invalid_params("Invalid cursor")),
            }
        }
        Some(_) => return Err(RpcError::invalid_params("Invalid cursor")),
    };
    let size = page_size.max(1);
    let end = (start + size).min(items.len());
    let next = (end < items.len()).then(|| b64.encode(format!("{kind}:{end}")));
    Ok((items[start..end].to_vec(), next))
}

/// Dispatch one request method. `initialize` is handled by the transports
/// (it creates the session). Errors are JSON-RPC errors.
pub async fn dispatch(ctx: &CallCtx, method: &str, params: &Value) -> Result<Value, RpcError> {
    if let Some(built) = list_method(&ctx.profile, ctx.version, method, params, ctx.page_size) {
        return built.map(|(result, _)| finalize(ctx, result));
    }
    let modern = ctx.version.is_modern();
    let result = match method {
        "server/discover" if modern => json!({
            "supportedVersions": Version::all_strings(),
            "capabilities": capabilities(&ctx.profile, ctx.version),
            "instructions": instructions(&ctx.profile),
            "ttlMs": 3_600_000,
            "cacheScope": "public",
        }),
        "ping" if !modern => json!({}),
        "tools/call" => tools::call(ctx, params).await?,
        "resources/read" => resources::read(ctx, params)?,
        "resources/subscribe" | "resources/unsubscribe" if !modern => {
            let uri = params
                .get("uri")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::invalid_params("Missing required parameter: uri"))?;
            if !resources::exists(&ctx.profile, uri) {
                return Err(RpcError::resource_not_found(uri, ctx.version));
            }
            let session = ctx
                .session
                .as_ref()
                .ok_or_else(|| RpcError::internal("subscriptions need a session"))?;
            let mut st = session.state();
            if method == "resources/subscribe" {
                if st.subscriptions.len() >= super::sessions::MAX_SUBSCRIPTIONS {
                    return Err(RpcError::invalid_params("Too many subscriptions"));
                }
                st.subscriptions.insert(uri.to_string());
            } else {
                st.subscriptions.remove(uri);
            }
            json!({})
        }
        "prompts/get" => prompts::get(ctx, params)?,
        "completion/complete" if ctx.version.has_annotations() => complete(ctx, params)?,
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
    Ok(finalize(ctx, result))
}

fn complete(ctx: &CallCtx, params: &Value) -> Result<Value, RpcError> {
    let reference = params
        .get("ref")
        .ok_or_else(|| RpcError::invalid_params("Missing required parameter: ref"))?;
    let arg_name = params
        .pointer("/argument/name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("Missing required parameter: argument.name"))?;
    let value = params
        .pointer("/argument/value")
        .and_then(Value::as_str)
        .unwrap_or("");
    let candidates: Vec<&str> = match reference.get("type").and_then(Value::as_str) {
        Some("ref/prompt") => {
            let name = reference
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            prompts::completions(&ctx.profile, name, arg_name)
                .ok_or_else(|| RpcError::invalid_params(format!("Unknown prompt: {name}")))?
        }
        Some("ref/resource") => {
            let uri = reference
                .get("uri")
                .and_then(Value::as_str)
                .unwrap_or_default();
            resources::completions(&ctx.profile, uri, arg_name).ok_or_else(|| {
                RpcError::invalid_params(format!("Unknown resource template: {uri}"))
            })?
        }
        _ => {
            return Err(RpcError::invalid_params(
                "ref.type must be ref/prompt or ref/resource",
            ))
        }
    };
    let needle = value.to_ascii_lowercase();
    let matches: Vec<&str> = candidates
        .into_iter()
        .filter(|c| c.to_ascii_lowercase().starts_with(&needle))
        .collect();
    let total = matches.len();
    let values: Vec<&str> = matches.into_iter().take(100).collect();
    Ok(json!({
        "completion": { "values": values, "total": total, "hasMore": total > 100 }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagination_round_trip() {
        let items: Vec<u32> = (0..7).collect();
        let (p1, c1) = paginate(&items, &json!({}), 3, "t").expect("page 1");
        assert_eq!(p1, vec![0, 1, 2]);
        let c1 = c1.expect("cursor");
        let (p2, c2) = paginate(&items, &json!({ "cursor": c1 }), 3, "t").expect("page 2");
        assert_eq!(p2, vec![3, 4, 5]);
        let (p3, c3) = paginate(&items, &json!({ "cursor": c2 }), 3, "t").expect("page 3");
        assert_eq!(p3, vec![6]);
        assert!(c3.is_none());
        let err = paginate(&items, &json!({ "cursor": "garbage" }), 3, "t").expect_err("bad");
        assert_eq!(err.code, protocol::INVALID_PARAMS);
    }
}
