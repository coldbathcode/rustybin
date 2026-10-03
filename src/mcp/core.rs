//! Transport-independent request handling: the per-request context, method
//! dispatch, capabilities, pagination and result post-processing.

use std::sync::Arc;
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
pub fn finalize(ctx: &CallCtx, mut result: Value) -> Value {
    if !ctx.version.is_modern() {
        return result;
    }
    if let Some(obj) = result.as_object_mut() {
        obj.entry("resultType").or_insert_with(|| json!("complete"));
        let meta = obj.entry("_meta").or_insert_with(|| json!({}));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert(
                protocol::META_SERVER_INFO.into(),
                server_info(&ctx.profile, ctx.version),
            );
        }
    }
    result
}

/// Add the CacheableResult fields (modern era only).
pub fn cacheable(ctx: &CallCtx, mut result: Value, ttl_ms: u64, scope: &str) -> Value {
    if ctx.version.is_modern() {
        result["ttlMs"] = json!(ttl_ms);
        result["cacheScope"] = json!(scope);
    }
    result
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
        "tools/list" => {
            let all = tools::list(&ctx.profile);
            let (page, next) = paginate(&all, params, ctx.page_size, "tools")?;
            let tools: Vec<Value> = page
                .iter()
                .map(|t| tools::tool_json(t, ctx.version))
                .collect();
            let mut r = json!({ "tools": tools });
            if let Some(next) = next {
                r["nextCursor"] = json!(next);
            }
            cacheable(ctx, r, 300_000, "public")
        }
        "tools/call" => tools::call(ctx, params).await?,
        "resources/list" => {
            let all = resources::list(&ctx.profile);
            let (page, next) = paginate(&all, params, ctx.page_size, "resources")?;
            let mut r = json!({ "resources": page });
            if let Some(next) = next {
                r["nextCursor"] = json!(next);
            }
            cacheable(ctx, r, 60_000, "public")
        }
        "resources/templates/list" => {
            let all = resources::templates(&ctx.profile);
            let (page, next) = paginate(&all, params, ctx.page_size, "templates")?;
            let mut r = json!({ "resourceTemplates": page });
            if let Some(next) = next {
                r["nextCursor"] = json!(next);
            }
            cacheable(ctx, r, 300_000, "public")
        }
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
        "prompts/list" => {
            let all = prompts::list(&ctx.profile);
            if all.is_empty() {
                return Err(RpcError::method_not_found(method));
            }
            let (page, next) = paginate(&all, params, ctx.page_size, "prompts")?;
            let mut r = json!({ "prompts": page });
            if let Some(next) = next {
                r["nextCursor"] = json!(next);
            }
            cacheable(ctx, r, 300_000, "public")
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
