//! Deprecated HTTP+SSE transport (protocol 2024-11-05).
//!
//! `GET /mcp/sse` opens the stream; the first event is `endpoint` carrying
//! `/mcp/messages?sessionId=...`. Clients POST JSON-RPC messages there and
//! get `202 Accepted`; responses, notifications and server requests arrive
//! as `message` events on the stream. The session ends with the stream.

use std::sync::Arc;

use axum::extract::{Query, Request, State};
use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::{IntoResponse, Response};
use axum::Extension;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::core::{self, CallCtx};
use super::protocol::{self, Message, RpcError, Version};
use super::sessions::{CancelToken, Session, TransportKind};
use super::transport_streamable::{
    accepts, decorate, http_info, initialize_result, legacy_notification, rpc_error,
    session_stream, Echo,
};
use super::{auth, Shared};
use crate::state::AppState;

/// Prefix honoured in the endpoint URL (gateways that mount the server below a path).
fn forwarded_prefix(app: &AppState, headers: &axum::http::HeaderMap) -> String {
    if !app.config.trust_forward {
        return String::new();
    }
    headers
        .get("x-forwarded-prefix")
        .and_then(|v| v.to_str().ok())
        .map(|p| p.trim().trim_end_matches('/'))
        .filter(|p| {
            p.starts_with('/')
                && p.len() <= 200
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "/-._~".contains(c))
        })
        .map(str::to_string)
        .unwrap_or_default()
}

pub async fn sse_handler(
    State(app): State<AppState>,
    Extension(sh): Extension<Arc<Shared>>,
    req: Request,
) -> Response {
    let (parts, _) = req.into_parts();
    let mut echo = Echo {
        profile: "default".into(),
        ..Default::default()
    };
    if let Err(resp) = auth::check_origin(&parts.headers, &sh.cfg) {
        return decorate(resp, &echo);
    }
    if !accepts(&parts.headers).1 {
        return decorate(
            rpc_error(
                StatusCode::NOT_ACCEPTABLE,
                None,
                &RpcError::invalid_request("Not Acceptable: requires Accept: text/event-stream"),
            ),
            &echo,
        );
    }
    let session = Arc::new(Session::new(
        "default",
        TransportKind::HttpSse,
        Version::V2024_11_05,
        None,
    ));
    let (tx, rx) = mpsc::channel::<Value>(256);
    session.state().outbound = Some(tx);
    sh.sessions.insert(session.clone());
    let endpoint = format!(
        "{}/mcp/messages?sessionId={}",
        forwarded_prefix(&app, &parts.headers),
        session.id
    );
    echo.session = Some(session.id.clone());
    echo.version = Some(Version::V2024_11_05.as_str());
    let first = Event::default().event("endpoint").data(endpoint);
    let stream = session_stream(sh.clone(), session, rx, Some(first));
    let mut resp = axum::response::sse::Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::new().interval(sh.cfg.keepalive))
        .into_response();
    resp.headers_mut().insert(
        axum::http::HeaderName::from_static("x-accel-buffering"),
        axum::http::HeaderValue::from_static("no"),
    );
    decorate(resp, &echo)
}

#[derive(Deserialize)]
pub struct SessionQuery {
    #[serde(rename = "sessionId", alias = "session_id")]
    session_id: Option<String>,
}

pub async fn messages_handler(
    State(app): State<AppState>,
    Extension(sh): Extension<Arc<Shared>>,
    Query(q): Query<SessionQuery>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let mut echo = Echo {
        profile: "default".into(),
        ..Default::default()
    };
    if let Err(resp) = auth::check_origin(&parts.headers, &sh.cfg) {
        return decorate(resp, &echo);
    }
    let session = q
        .session_id
        .as_deref()
        .and_then(|id| sh.sessions.get(id))
        .filter(|s| s.transport == TransportKind::HttpSse);
    let Some(session) = session else {
        return decorate(
            rpc_error(
                StatusCode::NOT_FOUND,
                None,
                &RpcError::new(protocol::SESSION_NOT_FOUND, "Could not find session"),
            ),
            &echo,
        );
    };
    echo.session = Some(session.id.clone());
    let bytes = match axum::body::to_bytes(body, sh.cfg.max_body).await {
        Ok(b) => b,
        Err(_) => {
            return decorate(
                rpc_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    None,
                    &RpcError::invalid_request("Body too large"),
                ),
                &echo,
            )
        }
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            return decorate(
                rpc_error(StatusCode::BAD_REQUEST, None, &RpcError::parse_error()),
                &echo,
            )
        }
    };
    let msg = match protocol::classify(&value) {
        Ok(m) => m,
        Err((e, id)) => {
            return decorate(rpc_error(StatusCode::BAD_REQUEST, id.as_ref(), &e), &echo)
        }
    };
    let Some(profile) = sh.profile("default") else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    match msg {
        Message::Notification { method, params } => legacy_notification(&session, &method, &params),
        Message::Response { id, body } => {
            session.resolve_pending(&id, body);
        }
        Message::Request { id, method, params } => {
            echo.rpc_id = Some(match &id {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            });
            echo.method = Some(method.clone());
            if method == "initialize" {
                let result = initialize_result(&profile, &session, &params);
                session.send_outbound(protocol::result_response(&id, result));
            } else {
                let sink = session.state().outbound.clone();
                let (version, caps, level) = {
                    let st = session.state();
                    (
                        st.version,
                        st.client_capabilities.clone(),
                        st.log_level.as_deref().and_then(protocol::log_level_rank),
                    )
                };
                let cancel = CancelToken::new();
                session.track_inflight(&id, cancel.clone());
                let ctx = CallCtx {
                    shared: sh.clone(),
                    profile,
                    version,
                    client_caps: caps,
                    log_level: level,
                    progress_token: protocol::progress_token(&params),
                    sink: sink.clone(),
                    cancel: cancel.clone(),
                    request_id: id.clone(),
                    session: Some(session.clone()),
                    http: Arc::new(http_info(&app, &parts, "http+sse")),
                    auth: None,
                    page_size: sh.cfg.default_page_size,
                };
                let task_session = session.clone();
                tokio::spawn(async move {
                    let outcome = core::dispatch(&ctx, &method, &params).await;
                    task_session.finish_inflight(&id);
                    if cancel.is_cancelled() {
                        return; // cancelled requests get no response
                    }
                    let msg = match outcome {
                        Ok(v) => protocol::result_response(&id, v),
                        Err(e) => protocol::error_response(Some(&id), &e),
                    };
                    if let Some(sink) = sink {
                        let _ = sink.send(msg).await;
                    }
                });
            }
        }
    }
    echo.version = Some(session.version().as_str());
    decorate(
        (
            StatusCode::ACCEPTED,
            axum::Json(json!({ "accepted": true })),
        )
            .into_response(),
        &echo,
    )
}
