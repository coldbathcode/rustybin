//! JSON-RPC 2.0 binding (spec section 9) for both protocol generations.
//!
//! The method name selects the generation (`SendMessage` vs `message/send`)
//! and the requested `A2A-Version` must match it, like the reference SDK:
//! v1.0 methods need `A2A-Version: 1.0`, v0.3 methods need no header (empty
//! means 0.3) or `0.3`. Anything else is a `VersionNotSupportedError`.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;
use serde_json::{json, Map, Value};

use super::card::{self, CardKind};
use super::errors::{A2aError, Kind};
use super::model::{StreamEvent, Version, Wire};
use super::tasks::{self, ListQuery, Started};
use super::wire::{self, TaskView};
use super::{agents::AuthOutcome, json_response, sse_response, A2a, Req, MAX_BLOCKING_WAIT};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Send,
    Stream,
    Get,
    List,
    Cancel,
    Subscribe,
    PushSet,
    PushGet,
    PushList,
    PushDelete,
    ExtendedCard,
}

fn lookup(method: &str) -> Option<(Op, Version)> {
    Some(match method {
        "SendMessage" => (Op::Send, Version::V10),
        "SendStreamingMessage" => (Op::Stream, Version::V10),
        "GetTask" => (Op::Get, Version::V10),
        "ListTasks" => (Op::List, Version::V10),
        "CancelTask" => (Op::Cancel, Version::V10),
        "SubscribeToTask" => (Op::Subscribe, Version::V10),
        "CreateTaskPushNotificationConfig" => (Op::PushSet, Version::V10),
        "GetTaskPushNotificationConfig" => (Op::PushGet, Version::V10),
        "ListTaskPushNotificationConfigs" => (Op::PushList, Version::V10),
        "DeleteTaskPushNotificationConfig" => (Op::PushDelete, Version::V10),
        "GetExtendedAgentCard" => (Op::ExtendedCard, Version::V10),
        "message/send" => (Op::Send, Version::V03),
        "message/stream" => (Op::Stream, Version::V03),
        "tasks/get" => (Op::Get, Version::V03),
        "tasks/cancel" => (Op::Cancel, Version::V03),
        "tasks/resubscribe" => (Op::Subscribe, Version::V03),
        "tasks/pushNotificationConfig/set" => (Op::PushSet, Version::V03),
        "tasks/pushNotificationConfig/get" => (Op::PushGet, Version::V03),
        "tasks/pushNotificationConfig/list" => (Op::PushList, Version::V03),
        "tasks/pushNotificationConfig/delete" => (Op::PushDelete, Version::V03),
        "agent/getAuthenticatedExtendedCard" => (Op::ExtendedCard, Version::V03),
        _ => return None,
    })
}

fn wire_of(v: Version) -> Wire {
    match v {
        Version::V10 => Wire::V1,
        Version::V03 => Wire::V03Rpc,
    }
}

fn error_response(id: &Value, err: &A2aError, version: Version) -> Response {
    let status = if err.kind == Kind::Unauthenticated {
        StatusCode::UNAUTHORIZED
    } else {
        StatusCode::OK
    };
    let mut resp = json_response(
        status,
        &err.jsonrpc_response(id, version),
        "application/json",
        Some(version),
    );
    if err.kind == Kind::Unauthenticated {
        resp.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"rustybin-a2a\", error=\"invalid_token\""),
        );
    }
    resp
}

fn ok_response(id: &Value, result: Value, version: Version) -> Response {
    json_response(
        StatusCode::OK,
        &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        "application/json",
        Some(version),
    )
}

fn params_obj(params: &Value) -> Result<&Map<String, Value>, A2aError> {
    params
        .as_object()
        .ok_or_else(|| A2aError::invalid_params("params must be an object"))
}

fn req_str(o: &Map<String, Value>, keys: &[&str]) -> Result<String, A2aError> {
    wire::get_str(o, keys)
        .map_err(A2aError::invalid_params)?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| A2aError::invalid_params(format!("params.{} is required", keys[0])))
}

fn opt_str(o: &Map<String, Value>, keys: &[&str]) -> Result<Option<String>, A2aError> {
    wire::get_str(o, keys)
        .map_err(A2aError::invalid_params)
        .map(|v| v.filter(|s| !s.is_empty()))
}

fn history_view(o: &Map<String, Value>) -> Result<TaskView, A2aError> {
    let hl =
        wire::get_int(o, &["historyLength", "history_length"]).map_err(A2aError::invalid_params)?;
    Ok(TaskView {
        history_length: wire::parse_history_length(hl).map_err(A2aError::invalid_params)?,
        include_artifacts: true,
    })
}

/// Extended card: only for agents declaring it, and only with a valid token.
pub fn extended_card(req: &Req, version: Version, proto_v03: bool) -> Result<Value, A2aError> {
    if !req.agent.secured {
        return Err(A2aError::unsupported(format!(
            "the {} agent does not provide an authenticated extended card (capabilities.extendedAgentCard is false)",
            req.agent.id
        )));
    }
    match &req.auth {
        AuthOutcome::Valid(_) => Ok(match version {
            Version::V10 => card::v1(&req.origin.base_url, req.agent, CardKind::Extended, false),
            Version::V03 if proto_v03 => card::v03_proto(&req.origin.base_url, req.agent, CardKind::Extended),
            Version::V03 => card::v03(&req.origin.base_url, req.agent, CardKind::Extended),
        }),
        AuthOutcome::Invalid(e) => Err(A2aError::new(
            Kind::Unauthenticated,
            format!("invalid bearer token: {e}"),
        )),
        AuthOutcome::Missing => Err(A2aError::new(
            Kind::Unauthenticated,
            format!(
                "authentication required: send Authorization: Bearer <token> (get one from {}/oauth/token)",
                req.origin.base_url
            ),
        )),
    }
}

pub async fn handle(a2a: &A2a, req: Req, body: &[u8]) -> Response {
    let null = Value::Null;
    // Version used to format errors before the method is known.
    let fallback = super::model::Version::parse(&req.version_raw).unwrap_or(Version::V10);
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            return error_response(
                &null,
                &A2aError::new(Kind::JsonParse, format!("Invalid JSON payload: {e}")),
                fallback,
            )
        }
    };
    let Value::Object(obj) = parsed else {
        let msg = if parsed.is_array() {
            "Batch requests are not supported"
        } else {
            "Request payload validation error: expected a JSON-RPC request object"
        };
        return error_response(&null, &A2aError::new(Kind::InvalidRequest, msg), fallback);
    };
    let id = match obj.get("id") {
        None => Value::Null,
        Some(v @ (Value::String(_) | Value::Number(_) | Value::Null)) => v.clone(),
        Some(_) => {
            return error_response(
                &null,
                &A2aError::new(
                    Kind::InvalidRequest,
                    "id must be a string, a number or null",
                ),
                fallback,
            )
        }
    };
    if obj.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
        return error_response(
            &id,
            &A2aError::new(
                Kind::InvalidRequest,
                "Invalid request: 'jsonrpc' must be exactly '2.0'",
            ),
            fallback,
        );
    }
    let Some(method) = obj.get("method").and_then(|m| m.as_str()) else {
        return error_response(
            &id,
            &A2aError::new(Kind::InvalidRequest, "Method is required"),
            fallback,
        );
    };
    let Some((op, family)) = lookup(method) else {
        return error_response(
            &id,
            &A2aError::new(Kind::MethodNotFound, format!("Method not found: {method}")),
            fallback,
        );
    };
    match Version::parse(&req.version_raw) {
        Some(v) if v == family => {}
        _ => {
            let shown = if req.version_raw.is_empty() {
                "0.3 (no A2A-Version header)".to_string()
            } else {
                req.version_raw.clone()
            };
            let err = A2aError::version_not_supported(&req.version_raw).with("method", method);
            let err = A2aError {
                message: format!(
                    "A2A version {shown} is not supported for method {method}: it requires A2A-Version: {}",
                    family.as_str()
                ),
                ..err
            };
            return error_response(&id, &err, family);
        }
    }
    let empty = Value::Object(Map::new());
    let params = match obj.get("params") {
        None | Some(Value::Null) => &empty,
        Some(p) => p,
    };
    match dispatch(a2a, &req, op, family, &id, params).await {
        Ok(resp) => resp,
        Err(e) => error_response(&id, &e, family),
    }
}

async fn dispatch(
    a2a: &A2a,
    req: &Req,
    op: Op,
    version: Version,
    id: &Value,
    params: &Value,
) -> Result<Response, A2aError> {
    let w = wire_of(version);
    let caller = req.caller(version);
    let svc = &a2a.svc;
    match op {
        Op::Send | Op::Stream => {
            let send = wire::parse_send(params, w).map_err(A2aError::invalid_params)?;
            let view = TaskView {
                history_length: send.history_length,
                include_artifacts: true,
            };
            let return_immediately = send.return_immediately;
            let started = svc.send(&caller, send)?;
            if op == Op::Stream {
                let id = id.clone();
                return Ok(match started {
                    Started::Reply(m) => sse_response(
                        StreamEvent::Message(m),
                        None,
                        move |ev| rpc_event(&id, ev, w, view),
                        version,
                    ),
                    Started::Task { snapshot, rx } => sse_response(
                        StreamEvent::Task(snapshot),
                        Some(rx),
                        move |ev| rpc_event(&id, ev, w, view),
                        version,
                    ),
                });
            }
            let ev = match started {
                Started::Reply(m) => StreamEvent::Message(m),
                Started::Task { snapshot, mut rx } => {
                    if !return_immediately {
                        tasks::wait_until_settled(&mut rx, MAX_BLOCKING_WAIT).await;
                    }
                    let current = svc.get(&caller, &snapshot.id).unwrap_or(snapshot);
                    StreamEvent::Task(current)
                }
            };
            Ok(ok_response(id, wire::send_result(&ev, w, view), version))
        }
        Op::Get => {
            let o = params_obj(params)?;
            let task_id = req_str(o, &["id"])?;
            let view = history_view(o)?;
            let task = svc.get(&caller, &task_id)?;
            Ok(ok_response(id, wire::task(&task, w, view), version))
        }
        Op::List => {
            let o = params_obj(params)?;
            let status = match opt_str(o, &["status"])? {
                None => None,
                Some(s) if s == "TASK_STATE_UNSPECIFIED" => None,
                Some(s) => Some(super::model::TaskState::parse(&s).ok_or_else(|| {
                    A2aError::invalid_params(format!(
                        "Invalid status value '{s}'. Must be one of: {}",
                        super::model::TaskState::ALL.map(|s| s.v1()).join(", ")
                    ))
                })?),
            };
            let q = ListQuery {
                context_id: opt_str(o, &["contextId", "context_id"])?,
                status,
                page_size: wire::get_int(o, &["pageSize", "page_size"])
                    .map_err(A2aError::invalid_params)?,
                page_token: opt_str(o, &["pageToken", "page_token"])?,
                history_length: wire::get_int(o, &["historyLength", "history_length"])
                    .map_err(A2aError::invalid_params)?,
                status_timestamp_after: opt_str(
                    o,
                    &["statusTimestampAfter", "status_timestamp_after"],
                )?,
                include_artifacts: wire::get_bool(o, &["includeArtifacts", "include_artifacts"])
                    .map_err(A2aError::invalid_params)?
                    .unwrap_or(false),
            };
            let r = svc.list(&caller, &q)?;
            Ok(ok_response(id, list_json(&r, w), version))
        }
        Op::Cancel => {
            let o = params_obj(params)?;
            let task_id = req_str(o, &["id"])?;
            let task = svc.cancel(&caller, &task_id)?;
            Ok(ok_response(
                id,
                wire::task(&task, w, TaskView::FULL),
                version,
            ))
        }
        Op::Subscribe => {
            let o = params_obj(params)?;
            let task_id = req_str(o, &["id"])?;
            let (snapshot, rx) = svc.subscribe(&caller, &task_id)?;
            let id = id.clone();
            Ok(sse_response(
                StreamEvent::Task(snapshot),
                Some(rx),
                move |ev| rpc_event(&id, ev, w, TaskView::FULL),
                version,
            ))
        }
        Op::PushSet => {
            let o = params_obj(params)?;
            let task_id = req_str(o, &["taskId", "task_id"])?;
            let cfg_v = match version {
                Version::V10 => params,
                Version::V03 => {
                    wire::get(o, &["pushNotificationConfig", "push_notification_config"])
                        .ok_or_else(|| {
                            A2aError::invalid_params("params.pushNotificationConfig is required")
                        })?
                }
            };
            let input = wire::parse_push_input(cfg_v).map_err(A2aError::invalid_params)?;
            let cfg = svc.push_create(&caller, &task_id, input)?;
            Ok(ok_response(id, wire::push_config(&cfg, w), version))
        }
        Op::PushGet => {
            let o = params_obj(params)?;
            let (task_id, config_id) = match version {
                Version::V10 => (
                    req_str(o, &["taskId", "task_id"])?,
                    Some(req_str(o, &["id"])?),
                ),
                Version::V03 => (
                    req_str(o, &["id"])?,
                    opt_str(
                        o,
                        &["pushNotificationConfigId", "push_notification_config_id"],
                    )?,
                ),
            };
            let cfg = svc.push_get(&caller, &task_id, config_id.as_deref())?;
            Ok(ok_response(id, wire::push_config(&cfg, w), version))
        }
        Op::PushList => {
            let o = params_obj(params)?;
            let task_id = match version {
                Version::V10 => req_str(o, &["taskId", "task_id"])?,
                Version::V03 => req_str(o, &["id"])?,
            };
            let configs = svc.push_list(&caller, &task_id)?;
            let list = tasks::configs_json(&configs, w);
            let result = match version {
                Version::V10 => json!({ "configs": list }),
                Version::V03 => Value::Array(list),
            };
            Ok(ok_response(id, result, version))
        }
        Op::PushDelete => {
            let o = params_obj(params)?;
            let (task_id, config_id) = match version {
                Version::V10 => (req_str(o, &["taskId", "task_id"])?, req_str(o, &["id"])?),
                Version::V03 => (
                    req_str(o, &["id"])?,
                    req_str(
                        o,
                        &["pushNotificationConfigId", "push_notification_config_id"],
                    )?,
                ),
            };
            svc.push_delete(&caller, &task_id, &config_id)?;
            Ok(ok_response(id, Value::Null, version))
        }
        Op::ExtendedCard => {
            let card = extended_card(req, version, false)?;
            Ok(ok_response(id, card, version))
        }
    }
}

fn rpc_event(id: &Value, ev: &StreamEvent, w: Wire, view: TaskView) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": wire::event(ev, w, view) }).to_string()
}

pub fn list_json(r: &tasks::ListResult, w: Wire) -> Value {
    json!({
        "tasks": r.tasks.iter().map(|t| wire::task(t, w, r.view)).collect::<Vec<_>>(),
        "nextPageToken": r.next_page_token,
        "pageSize": r.page_size,
        "totalSize": r.total_size,
    })
}
