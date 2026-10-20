//! Traffic tools: the request inspector, waiting for the next requests,
//! request bins and the A2A push notification sink. Everything a third
//! party sent is returned under `untrusted_*` fields.

use std::sync::Arc;
use std::time::Duration;

use axum::http::Method;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use super::access::Access;
use super::mocks::capture_row;
use super::testing::in_process;
use super::tools::{
    arg_bool, arg_str, arg_u64, arg_usize, confirm, err, need_str, ok, untrusted_headers, Budget,
    Confirm,
};
use crate::inspector::{CapturedRequest, InspectorQuery};
use crate::mcp::core::CallCtx;

/// Filters of `list_requests` and `wait_for_requests`.
struct Filter {
    path_prefix: Option<String>,
    method: Option<String>,
    status: Option<u16>,
    session: Option<String>,
    since: Option<String>,
    header: Option<(String, Option<String>)>,
    key_id: Option<String>,
}

impl Filter {
    fn from_args(args: &Value, access: &Access) -> Result<Self, Value> {
        let header = match arg_str(args, "header") {
            None => None,
            Some(h) => {
                let (name, value) = match h.split_once(':') {
                    Some((n, v)) => (n.trim().to_string(), Some(v.trim().to_string())),
                    None => (h.trim().to_string(), None),
                };
                if name.is_empty() {
                    return Err(err("header must be \"Name\" or \"Name: value\""));
                }
                Some((name.to_ascii_lowercase(), value))
            }
        };
        let session = arg_str(args, "session").map(str::to_string);
        if session
            .as_deref()
            .is_some_and(|s| !crate::session::is_valid_session(s))
        {
            return Err(err("invalid session value"));
        }
        Ok(Self {
            path_prefix: arg_str(args, "path_prefix").map(str::to_string),
            method: arg_str(args, "method").map(|m| m.to_ascii_uppercase()),
            status: arg_u64(args, "status").map(|s| s as u16),
            session,
            since: arg_str(args, "since").map(str::to_string),
            header,
            key_id: access.key_id.clone(),
        })
    }

    /// Public mode without keys: a session is mandatory (the caller's own
    /// when the request carried one).
    fn scope(&mut self, access: &Access) -> Result<(), Value> {
        if access.public_mode && !access.shared_mode && self.session.is_none() {
            match &access.session_header {
                Some(s) => self.session = Some(s.clone()),
                None => return Err(err(
                    "this instance runs in public mode: give session (the X-Rustybin-Session of \
                         the traffic to read, for example the session try_request used)",
                )),
            }
        }
        Ok(())
    }

    fn matches(&self, e: &CapturedRequest) -> bool {
        if let Some(k) = &self.key_id {
            if e.key_id.as_deref() != Some(k.as_str()) {
                return false;
            }
        }
        if let Some(s) = &self.session {
            if e.session.as_deref() != Some(s.as_str()) {
                return false;
            }
        }
        if let Some(p) = &self.path_prefix {
            if !e.path.starts_with(p.as_str()) {
                return false;
            }
        }
        if let Some(m) = &self.method {
            if !e.method.eq_ignore_ascii_case(m) {
                return false;
            }
        }
        if let Some(st) = self.status {
            if e.status != st {
                return false;
            }
        }
        if let Some(since) = &self.since {
            if e.timestamp.as_str() < since.as_str() {
                return false;
            }
        }
        if let Some((name, value)) = &self.header {
            let found = e.headers.iter().any(|h| {
                h.name.eq_ignore_ascii_case(name) && value.as_deref().is_none_or(|v| h.value == v)
            });
            if !found {
                return false;
            }
        }
        true
    }
}

/// An inspector entry as a result row.
fn request_row(e: &CapturedRequest, include_bodies: bool, budget: &mut Budget) -> Value {
    let mut row = json!({
        "id": e.id,
        "request_id": e.request_id,
        "timestamp": e.timestamp,
        "method": e.method,
        "path": e.path,
        "query": e.query,
        "status": e.status,
        "latency_ms": e.latency_ms,
        "session": e.session,
        "key_id": e.key_id,
        "client_ip": e.client_ip,
        "body_bytes": e.body_size,
        "untrusted_headers": untrusted_headers(&e.headers.iter().map(|h| (h.name.clone(), h.value.clone())).collect::<Vec<_>>()),
        "untrusted_query": e.query,
    });
    if include_bodies {
        if let Some(body) = e.body.as_deref().or(e.body_base64.as_deref()) {
            if let Some((cut, truncated)) = budget.body(body) {
                row["untrusted_body"] = json!(cut);
                row["body_truncated"] = json!(truncated || e.body_truncated);
                row["body_base64"] = json!(e.body.is_none());
            }
        }
    }
    row
}

/// `list_requests`.
pub fn list_requests(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let mut filter = match Filter::from_args(args, access) {
        Ok(f) => f,
        Err(e) => return e,
    };
    if let Err(e) = filter.scope(access) {
        return e;
    }
    let limit = arg_usize(args, "limit").unwrap_or(20).clamp(1, 500);
    let include_bodies = arg_bool(args, "include_bodies").unwrap_or(false);
    let inspector = &ctx.app.inspector;
    // The inspector filters by session, prefix and key; the rest here.
    let q = InspectorQuery {
        session: filter.session.clone(),
        path_prefix: filter.path_prefix.clone(),
        limit: Some(inspector.capacity().max(limit)),
        key_id: filter.key_id.clone(),
    };
    let entries: Vec<Arc<CapturedRequest>> = inspector
        .list(&q)
        .into_iter()
        .filter(|e| filter.matches(e))
        .take(limit)
        .collect();
    let mut budget = Budget::new();
    let rows: Vec<Value> = entries
        .iter()
        .map(|e| request_row(e, include_bodies, &mut budget))
        .collect();
    let summary = format!(
        "{} captured request(s) match (inspector holds {} of {}); newest first{}. Content is under untrusted_* fields.",
        rows.len(),
        inspector.len(),
        inspector.capacity(),
        if include_bodies { ", with bodies" } else { "" }
    );
    ok(
        ctx,
        summary,
        json!({
            "count": rows.len(),
            "stored": inspector.len(),
            "capacity": inspector.capacity(),
            "result_truncated": budget.truncated,
            "requests": rows,
        }),
    )
}

/// `get_request`.
pub fn get_request(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let id = match need_str(args, "id") {
        Ok(i) => i,
        Err(e) => return e,
    };
    let inspector = &ctx.app.inspector;
    let session = arg_str(args, "session")
        .map(str::to_string)
        .or_else(|| access.session_header.clone());
    let entry = inspector.get(id).filter(|e| match &access.key_id {
        Some(k) => e.key_id.as_deref() == Some(k.as_str()),
        None if access.shared_mode => true,
        None => {
            !access.public_mode || access.is_admin() || (session.is_some() && e.session == session)
        }
    });
    match entry {
        Some(e) => {
            let mut budget = Budget::new();
            let row = request_row(&e, true, &mut budget);
            ok(
                ctx,
                format!(
                    "{} {} answered {} at {} ({} ms); content under untrusted_* fields",
                    e.method, e.path, e.status, e.timestamp, e.latency_ms as u64
                ),
                row,
            )
        }
        None => err("no such captured request (unknown id, evicted, or not in your scope)"),
    }
}

/// `wait_for_requests`.
pub async fn wait_for_requests(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let timeout = Duration::from_secs(arg_u64(args, "timeout_secs").unwrap_or(30).clamp(1, 60));
    let limit = arg_usize(args, "limit").unwrap_or(10).clamp(1, 50);
    let include_bodies = arg_bool(args, "include_bodies").unwrap_or(false);
    let Ok(_permit) = ctx.shared.waits.try_acquire() else {
        return err("too many waits in progress on this instance: try again in a moment");
    };
    let mut budget = Budget::new();
    if let Some(mock) = arg_str(args, "mock") {
        let s = access.svc(&ctx.app);
        let snap = match s.readable(mock) {
            Ok(s) => s,
            Err(e) => return super::tools::service_error(&e),
        };
        let method = arg_str(args, "method").map(|m| m.to_ascii_uppercase());
        let prefix = arg_str(args, "path_prefix").map(str::to_string);
        let mut rx = snap.capture.subscribe();
        let matches = |c: &crate::mocks::capture::MockCapture| {
            method
                .as_deref()
                .is_none_or(|m| c.method.eq_ignore_ascii_case(m))
                && prefix.as_deref().is_none_or(|p| c.path.starts_with(p))
        };
        let mut rows = Vec::new();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let next = tokio::select! {
                r = rx.recv() => r,
                _ = tokio::time::sleep_until(deadline) => break,
                _ = ctx.cancel.cancelled() => break,
            };
            match next {
                Ok(c) if matches(&c) => {
                    rows.push(capture_row(&c, include_bodies, &mut budget));
                    // Collect what arrived in the same burst.
                    while rows.len() < limit {
                        match rx.try_recv() {
                            Ok(c) if matches(&c) => {
                                rows.push(capture_row(&c, include_bodies, &mut budget))
                            }
                            Ok(_) => continue,
                            Err(_) => break,
                        }
                    }
                    break;
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        let summary = if rows.is_empty() {
            format!(
                "No call of mock {mock:?} arrived within {} s (the mock's URL is what the gateway must call)",
                timeout.as_secs()
            )
        } else {
            format!(
                "{} call(s) of mock {mock:?} arrived: {}. Content is under untrusted_* fields.",
                rows.len(),
                rows.iter()
                    .map(|r| format!(
                        "{} {} -> {} ({})",
                        r["method"].as_str().unwrap_or(""),
                        r["path"].as_str().unwrap_or(""),
                        r["status"],
                        r["outcome"].as_str().unwrap_or("")
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        };
        return ok(
            ctx,
            summary,
            json!({ "mock": mock, "count": rows.len(), "timed_out": rows.is_empty(), "result_truncated": budget.truncated, "requests": rows }),
        );
    }

    let mut filter = match Filter::from_args(args, access) {
        Ok(f) => f,
        Err(e) => return e,
    };
    if let Err(e) = filter.scope(access) {
        return e;
    }
    let mut rx = ctx.app.inspector.subscribe();
    let mut rows = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let next = tokio::select! {
            r = rx.recv() => r,
            _ = tokio::time::sleep_until(deadline) => break,
            _ = ctx.cancel.cancelled() => break,
        };
        match next {
            Ok(e) if filter.matches(&e) => {
                rows.push(request_row(&e, include_bodies, &mut budget));
                while rows.len() < limit {
                    match rx.try_recv() {
                        Ok(e) if filter.matches(&e) => {
                            rows.push(request_row(&e, include_bodies, &mut budget))
                        }
                        Ok(_) => continue,
                        Err(_) => break,
                    }
                }
                break;
            }
            Ok(_) => continue,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    let summary = if rows.is_empty() {
        format!(
            "No matching request arrived within {} s{}",
            timeout.as_secs(),
            filter
                .session
                .as_deref()
                .map(|s| format!(" (watching session {s})"))
                .unwrap_or_default()
        )
    } else {
        format!(
            "{} request(s) arrived: {}. Content is under untrusted_* fields.",
            rows.len(),
            rows.iter()
                .map(|r| format!(
                    "{} {} -> {}",
                    r["method"].as_str().unwrap_or(""),
                    r["path"].as_str().unwrap_or(""),
                    r["status"]
                ))
                .collect::<Vec<_>>()
                .join("; ")
        )
    };
    ok(
        ctx,
        summary,
        json!({ "count": rows.len(), "timed_out": rows.is_empty(), "result_truncated": budget.truncated, "requests": rows }),
    )
}

/// `clear_requests` (confirmed; admin unless scoped to a session).
pub async fn clear_requests(
    ctx: &CallCtx,
    _access: &Access,
    args: &Value,
    params: &Value,
) -> Value {
    let session = arg_str(args, "session").map(str::to_string);
    if session
        .as_deref()
        .is_some_and(|s| !crate::session::is_valid_session(s))
    {
        return err("invalid session value");
    }
    let action = match &session {
        Some(s) => format!("delete the captured requests of session {s}"),
        None => "delete every captured request of the inspector".to_string(),
    };
    if let Confirm::Answer(v) = confirm(ctx, "clear_requests", args, params, &action).await {
        return v;
    }
    let removed = match &session {
        Some(s) => ctx.app.inspector.clear_session(s),
        None => ctx.app.inspector.clear(),
    };
    ok(
        ctx,
        format!("Cleared {removed} captured request(s)"),
        json!({ "cleared": removed, "session": session }),
    )
}

/// `create_bin`: through `POST /bin` as the caller's session.
pub async fn create_bin(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let mut body = serde_json::Map::new();
    for key in ["status", "headers", "body", "delay_ms"] {
        if let Some(v) = args.get(key).filter(|v| !v.is_null()) {
            body.insert(key.into(), v.clone());
        }
    }
    let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
    let payload = Value::Object(body).to_string().into_bytes();
    let session = arg_str(args, "session");
    match in_process(
        ctx,
        access,
        Method::POST,
        "/bin",
        std::mem::take(&mut headers),
        Some(payload),
        session,
        false,
    )
    .await
    {
        Ok(a) if a.status == 201 => {
            let b = a.json().unwrap_or(Value::Null);
            ok(
                ctx,
                format!(
                    "Created bin {} (owned by session {}): send requests to {} and read them with get_bin",
                    b["id"].as_str().unwrap_or("?"),
                    a.session,
                    b["url"].as_str().unwrap_or("")
                ),
                json!({ "id": b["id"], "url": b["url"], "requests_url": b["requests_url"], "expires_at": b["expires_at"], "response": b["response"], "session": a.session }),
            )
        }
        Ok(a) => err(format!(
            "could not create the bin ({}): {}",
            a.status,
            a.json()
                .and_then(|j| j.get("error").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| super::testing::short(&a.body))
        )),
        Err(e) => e,
    }
}

/// `get_bin`: through `GET /bin/{id}/requests`.
pub async fn get_bin(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let id = match need_str(args, "id") {
        Ok(i) => i,
        Err(e) => return e,
    };
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        || id.len() > 64
    {
        return err("invalid bin id");
    }
    let limit = arg_usize(args, "limit").unwrap_or(20).clamp(1, 200);
    let include_bodies = arg_bool(args, "include_bodies").unwrap_or(false);
    let path = format!("/bin/{id}/requests?limit={limit}");
    let answer = match in_process(
        ctx,
        access,
        Method::GET,
        &path,
        Vec::new(),
        None,
        None,
        false,
    )
    .await
    {
        Ok(a) => a,
        Err(e) => return e,
    };
    if answer.status != 200 {
        return err(format!("bin {id:?}: {}", answer.status));
    }
    let body = answer.json().unwrap_or(Value::Null);
    let mut budget = Budget::new();
    let rows: Vec<Value> = body["requests"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| {
            let headers: Vec<(String, String)> = r["headers"]
                .as_array()
                .map(|hs| {
                    hs.iter()
                        .filter_map(|h| {
                            Some((h["name"].as_str()?.to_string(), h["value"].as_str()?.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut row = json!({
                "seq": r["seq"], "timestamp": r["timestamp"], "method": r["method"], "path": r["path"],
                "query": r["query"], "content_type": r["content_type"], "body_bytes": r["body_size"],
                "client_ip": r["client_ip"],
                "untrusted_headers": untrusted_headers(&headers),
                "untrusted_query": r["query"],
            });
            if include_bodies {
                if let Some(b) = r["body"].as_str().or(r["body_base64"].as_str()) {
                    if let Some((cut, truncated)) = budget.body(b) {
                        row["untrusted_body"] = json!(cut);
                        row["body_truncated"] = json!(truncated || r["body_truncated"].as_bool().unwrap_or(false));
                        row["body_base64"] = json!(r["body"].is_null());
                    }
                }
            }
            row
        })
        .collect();
    ok(
        ctx,
        format!(
            "Bin {id} holds {} request(s) ({} in total); content under untrusted_* fields",
            rows.len(),
            body["total"]
        ),
        json!({ "id": id, "total": body["total"], "count": rows.len(), "response": body["response"], "result_truncated": budget.truncated, "requests": rows }),
    )
}

/// `list_push_notifications`: through `GET /a2a/webhook-sink/{id}`.
pub async fn list_push_notifications(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let id = match need_str(args, "id") {
        Ok(i) => i,
        Err(e) => return e,
    };
    if !crate::a2a::push::valid_sink_id(id) {
        return err("sink ids are 1-64 characters of A-Z a-z 0-9 . _ -");
    }
    let path = format!("/a2a/webhook-sink/{id}");
    let answer = match in_process(
        ctx,
        access,
        Method::GET,
        &path,
        Vec::new(),
        None,
        None,
        false,
    )
    .await
    {
        Ok(a) => a,
        Err(e) => return e,
    };
    if answer.status != 200 {
        return err(format!("sink {id:?}: {}", answer.status));
    }
    let body = answer.json().unwrap_or(Value::Null);
    let items = body["notifications"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let count = items.len();
    ok(
        ctx,
        format!(
            "Sink {id} received {count} notification(s); content under untrusted_notifications"
        ),
        json!({ "id": id, "count": count, "untrusted_notifications": items }),
    )
}
