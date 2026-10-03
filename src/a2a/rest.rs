//! HTTP+JSON (REST) binding (spec section 11) under `/a2a/{agent}/v1/...`.
//!
//! axum cannot match `:` inside a segment, so the router hands the whole
//! tail (`tasks/abc:cancel`) to [`handle`], which parses it. The same paths
//! serve v1.0 (`A2A-Version: 1.0`, v1.0 ProtoJSON, `google.rpc.Status`
//! errors) and v0.3 (no header: v0.3 ProtoJSON, where the v0.3 client
//! appends `/v1/...` to the interface URL `/a2a/{agent}`).

use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::Response;
use serde_json::{json, Map, Value};

use super::errors::{A2aError, Kind};
use super::jsonrpc::{extended_card, list_json};
use super::model::{StreamEvent, TaskState, Version, Wire};
use super::tasks::{self, ListQuery, Started};
use super::wire::{self, TaskView};
use super::{card, json_response, sse_response, A2a, Req, MAX_BLOCKING_WAIT};

fn content_type(v: Version) -> &'static str {
    match v {
        Version::V10 => "application/a2a+json",
        Version::V03 => "application/json",
    }
}

fn error(err: &A2aError, version: Version) -> Response {
    let mut resp = json_response(
        err.http_status(),
        &err.rest_body(version),
        content_type(version),
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

fn ok(body: Value, version: Version) -> Response {
    json_response(StatusCode::OK, &body, content_type(version), Some(version))
}

fn method_not_allowed(allowed: &'static str, version: Version) -> Response {
    let err = A2aError::new(
        Kind::MethodNotFound,
        format!("method not allowed here; use {allowed}"),
    );
    let mut resp = json_response(
        StatusCode::METHOD_NOT_ALLOWED,
        &err.rest_body(version),
        content_type(version),
        Some(version),
    );
    resp.headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static(allowed));
    resp
}

fn query_map(query: Option<&str>) -> Map<String, Value> {
    let mut m = Map::new();
    if let Some(q) = query {
        for (k, v) in form_urlencoded::parse(q.as_bytes()) {
            m.insert(k.into_owned(), Value::String(v.into_owned()));
        }
    }
    m
}

fn parse_body(body: &[u8]) -> Result<Value, A2aError> {
    if body.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(body)
        .map_err(|e| A2aError::new(Kind::InvalidRequest, format!("invalid JSON body: {e}")))
}

/// Parsed REST route.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    Send,
    Stream,
    List,
    Get(String),
    Cancel(String),
    Subscribe(String),
    PushConfigs(String),
    PushConfig(String, String),
    ExtendedCard,
    Card,
}

fn parse_route(rest: &str) -> Option<Route> {
    let segs: Vec<&str> = rest.trim_matches('/').split('/').collect();
    Some(match segs.as_slice() {
        ["message:send"] => Route::Send,
        ["message:stream"] => Route::Stream,
        ["tasks"] => Route::List,
        ["extendedAgentCard"] => Route::ExtendedCard,
        ["card"] => Route::Card,
        ["tasks", id] => {
            if let Some(t) = id.strip_suffix(":cancel") {
                Route::Cancel(t.to_string())
            } else if let Some(t) = id.strip_suffix(":subscribe") {
                Route::Subscribe(t.to_string())
            } else {
                Route::Get(id.to_string())
            }
        }
        ["tasks", id, "pushNotificationConfigs"] => Route::PushConfigs(id.to_string()),
        ["tasks", id, "pushNotificationConfigs", cid] => {
            Route::PushConfig(id.to_string(), cid.to_string())
        }
        _ => return None,
    })
    .filter(|r| match r {
        Route::Get(id) | Route::Cancel(id) | Route::Subscribe(id) | Route::PushConfigs(id) => {
            !id.is_empty()
        }
        Route::PushConfig(id, cid) => !id.is_empty() && !cid.is_empty(),
        _ => true,
    })
}

pub async fn handle(
    a2a: &A2a,
    req: Req,
    method: &Method,
    rest: &str,
    query: Option<&str>,
    body: &[u8],
) -> Response {
    let Some(version) = Version::parse(&req.version_raw) else {
        return error(
            &A2aError::version_not_supported(&req.version_raw),
            Version::V10,
        );
    };
    let Some(route) = parse_route(rest) else {
        return error(
            &A2aError::new(
                Kind::MethodNotFound,
                format!("no A2A REST operation at /{}", rest.trim_matches('/')),
            ),
            version,
        );
    };
    match run(a2a, &req, version, method, route, query, body).await {
        Ok(resp) => resp,
        Err(e) => error(&e, version),
    }
}

async fn run(
    a2a: &A2a,
    req: &Req,
    version: Version,
    method: &Method,
    route: Route,
    query: Option<&str>,
    body: &[u8],
) -> Result<Response, A2aError> {
    let w = match version {
        Version::V10 => Wire::V1,
        Version::V03 => Wire::V03Rest,
    };
    let caller = req.caller(version);
    let svc = &a2a.svc;
    let q = query_map(query);
    let view_from_query = || -> Result<TaskView, A2aError> {
        let hl = wire::get_int(&q, &["historyLength", "history_length"])
            .map_err(A2aError::invalid_params)?;
        let hl = if w == Wire::V03Rest && hl == Some(0) {
            None
        } else {
            hl
        };
        Ok(TaskView {
            history_length: wire::parse_history_length(hl).map_err(A2aError::invalid_params)?,
            include_artifacts: true,
        })
    };
    match route {
        Route::Send | Route::Stream => {
            if method != Method::POST {
                return Ok(method_not_allowed("POST", version));
            }
            let body = parse_body(body)?;
            let send = wire::parse_send(&body, w).map_err(A2aError::invalid_params)?;
            let view = TaskView {
                history_length: send.history_length,
                include_artifacts: true,
            };
            let return_immediately = send.return_immediately;
            let started = svc.send(&caller, send)?;
            if route == Route::Stream {
                let render = move |ev: &StreamEvent| wire::event(ev, w, view).to_string();
                return Ok(match started {
                    Started::Reply(m) => {
                        sse_response(StreamEvent::Message(m), None, render, version)
                    }
                    Started::Task { snapshot, rx } => {
                        sse_response(StreamEvent::Task(snapshot), Some(rx), render, version)
                    }
                });
            }
            let ev = match started {
                Started::Reply(m) => StreamEvent::Message(m),
                Started::Task { snapshot, mut rx } => {
                    if !return_immediately {
                        tasks::wait_until_settled(&mut rx, MAX_BLOCKING_WAIT).await;
                    }
                    StreamEvent::Task(svc.get(&caller, &snapshot.id).unwrap_or(snapshot))
                }
            };
            Ok(ok(wire::send_result(&ev, w, view), version))
        }
        Route::List => {
            if method != Method::GET {
                return Ok(method_not_allowed("GET", version));
            }
            if version == Version::V03 {
                return Err(A2aError::unsupported(
                    "ListTasks is not part of A2A v0.3; send A2A-Version: 1.0",
                ));
            }
            let status = match wire::get_str(&q, &["status"]).map_err(A2aError::invalid_params)? {
                None => None,
                Some(s) if s.is_empty() || s == "TASK_STATE_UNSPECIFIED" => None,
                Some(s) => Some(TaskState::parse(&s).ok_or_else(|| {
                    A2aError::invalid_params(format!(
                        "Invalid status value '{s}'. Must be one of: {}",
                        TaskState::ALL.map(|s| s.v1()).join(", ")
                    ))
                })?),
            };
            let lq = ListQuery {
                context_id: wire::get_str(&q, &["contextId", "context_id"])
                    .map_err(A2aError::invalid_params)?
                    .filter(|s| !s.is_empty()),
                status,
                page_size: wire::get_int(&q, &["pageSize", "page_size"])
                    .map_err(A2aError::invalid_params)?,
                page_token: wire::get_str(&q, &["pageToken", "page_token"])
                    .map_err(A2aError::invalid_params)?,
                history_length: wire::get_int(&q, &["historyLength", "history_length"])
                    .map_err(A2aError::invalid_params)?,
                status_timestamp_after: wire::get_str(
                    &q,
                    &["statusTimestampAfter", "status_timestamp_after"],
                )
                .map_err(A2aError::invalid_params)?,
                include_artifacts: wire::get_bool(&q, &["includeArtifacts", "include_artifacts"])
                    .map_err(A2aError::invalid_params)?
                    .unwrap_or(false),
            };
            let r = svc.list(&caller, &lq)?;
            Ok(ok(list_json(&r, w), version))
        }
        Route::Get(id) => {
            if method != Method::GET {
                return Ok(method_not_allowed("GET", version));
            }
            let view = view_from_query()?;
            let task = svc.get(&caller, &id)?;
            Ok(ok(wire::task(&task, w, view), version))
        }
        Route::Cancel(id) => {
            if method != Method::POST {
                return Ok(method_not_allowed("POST", version));
            }
            let task = svc.cancel(&caller, &id)?;
            Ok(ok(wire::task(&task, w, TaskView::FULL), version))
        }
        Route::Subscribe(id) => {
            // The proto annotation says GET, the spec text says POST: accept both.
            if method != Method::GET && method != Method::POST {
                return Ok(method_not_allowed("GET, POST", version));
            }
            let (snapshot, rx) = svc.subscribe(&caller, &id)?;
            let render = move |ev: &StreamEvent| wire::event(ev, w, TaskView::FULL).to_string();
            Ok(sse_response(
                StreamEvent::Task(snapshot),
                Some(rx),
                render,
                version,
            ))
        }
        Route::PushConfigs(task_id) => {
            if method == Method::GET {
                let configs = svc.push_list(&caller, &task_id)?;
                let mut out = Map::new();
                out.insert(
                    "configs".into(),
                    Value::Array(tasks::configs_json(&configs, w)),
                );
                return Ok(ok(Value::Object(out), version));
            }
            if method != Method::POST {
                return Ok(method_not_allowed("GET, POST", version));
            }
            let body = parse_body(body)?;
            let cfg_v = match (version, body.as_object()) {
                // v0.3: {parent, config_id, config: {name, push_notification_config: {..}}}
                (Version::V03, Some(o)) => {
                    let config = wire::get(o, &["config"]).unwrap_or(&body);
                    let inner = config
                        .as_object()
                        .and_then(|c| {
                            wire::get(c, &["pushNotificationConfig", "push_notification_config"])
                        })
                        .unwrap_or(config);
                    let mut inner = inner.clone();
                    if let (Some(obj), Some(cid)) = (
                        inner.as_object_mut(),
                        wire::get_str(o, &["configId", "config_id"]).ok().flatten(),
                    ) {
                        if !cid.is_empty() && !obj.contains_key("id") {
                            obj.insert("id".into(), Value::String(cid));
                        }
                    }
                    inner
                }
                _ => body.clone(),
            };
            let input = wire::parse_push_input(&cfg_v).map_err(A2aError::invalid_params)?;
            let cfg = svc.push_create(&caller, &task_id, input)?;
            Ok(ok(wire::push_config(&cfg, w), version))
        }
        Route::PushConfig(task_id, config_id) => {
            if method == Method::GET {
                let cfg = svc.push_get(&caller, &task_id, Some(&config_id))?;
                return Ok(ok(wire::push_config(&cfg, w), version));
            }
            if method == Method::DELETE {
                svc.push_delete(&caller, &task_id, &config_id)?;
                return Ok(ok(json!({}), version));
            }
            Ok(method_not_allowed("GET, DELETE", version))
        }
        Route::ExtendedCard => {
            if method != Method::GET {
                return Ok(method_not_allowed("GET", version));
            }
            Ok(ok(extended_card(req, version, true)?, version))
        }
        Route::Card => {
            if method != Method::GET {
                return Ok(method_not_allowed("GET", version));
            }
            // v0.3 GetAgentCard: the extended card when authenticated.
            if req.agent.secured && matches!(req.auth, super::agents::AuthOutcome::Valid(_)) {
                return Ok(ok(extended_card(req, version, true)?, version));
            }
            let base = &req.origin.base_url;
            let c = match version {
                Version::V10 => card::v1(base, req.agent, card::CardKind::Public, false),
                Version::V03 => card::v03_proto(base, req.agent, card::CardKind::Public),
            };
            Ok(ok(c, version))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes() {
        assert_eq!(parse_route("message:send"), Some(Route::Send));
        assert_eq!(parse_route("/message:stream"), Some(Route::Stream));
        assert_eq!(parse_route("tasks"), Some(Route::List));
        assert_eq!(parse_route("tasks/abc"), Some(Route::Get("abc".into())));
        assert_eq!(
            parse_route("tasks/abc:cancel"),
            Some(Route::Cancel("abc".into()))
        );
        assert_eq!(
            parse_route("tasks/abc:subscribe"),
            Some(Route::Subscribe("abc".into()))
        );
        assert_eq!(
            parse_route("tasks/abc/pushNotificationConfigs"),
            Some(Route::PushConfigs("abc".into()))
        );
        assert_eq!(
            parse_route("tasks/abc/pushNotificationConfigs/p1"),
            Some(Route::PushConfig("abc".into(), "p1".into()))
        );
        assert_eq!(parse_route("extendedAgentCard"), Some(Route::ExtendedCard));
        assert_eq!(parse_route("tasks/:cancel"), None);
        assert_eq!(parse_route("nope"), None);
    }
}
