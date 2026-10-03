//! Generic JSON-RPC 2.0 endpoint (`POST /jsonrpc`) for gateways that route
//! JSON-RPC traffic (MCP and A2A style).
//!
//! Implements the specification (<https://www.jsonrpc.org/specification>):
//! single requests, batches (processed concurrently, capped), notifications
//! (no `id`, no response; a batch of only notifications or a single
//! notification answers 204), and the standard error codes -32700, -32600,
//! -32601, -32602 and -32603.
//!
//! Methods: `echo` (returns params), `add` (positional numbers or named
//! `{a, b}`), `subtract` (`[minuend, subtrahend]` or named), `sleep`
//! (`[ms]` or `{ms}`, capped by the instance delay limit) and `error`
//! (`[code, message?]` or `{code, message, data}`: answers with that error).

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Map, Value};
use std::sync::Arc;
use std::time::Duration;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::state::AppState;

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// Largest batch (normal / public mode).
pub const MAX_BATCH: usize = 100;
pub const MAX_BATCH_PUBLIC: usize = 20;

/// Supported methods.
pub const METHODS: &[&str] = &["echo", "add", "subtract", "sleep", "error"];

fn error_obj(code: i64, message: &str, data: Option<Value>) -> Value {
    let mut e = json!({ "code": code, "message": message });
    if let (Some(d), Some(obj)) = (data, e.as_object_mut()) {
        obj.insert("data".into(), d);
    }
    e
}

fn error_response(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    json!({ "jsonrpc": "2.0", "error": error_obj(code, message, data), "id": id })
}

/// A method failure: `(code, message, data)`.
type RpcError = (i64, String, Option<Value>);

fn invalid_params(message: &str) -> RpcError {
    (INVALID_PARAMS, format!("Invalid params: {message}"), None)
}

/// Numbers from positional params, or the named fields `names` in order.
fn numbers(params: &Option<Value>, names: &[&str]) -> Result<Vec<Value>, RpcError> {
    match params {
        Some(Value::Array(items)) => Ok(items.clone()),
        Some(Value::Object(map)) => names
            .iter()
            .map(|n| {
                map.get(*n)
                    .cloned()
                    .ok_or_else(|| invalid_params(&format!("missing named parameter {n:?}")))
            })
            .collect(),
        _ => Err(invalid_params(&format!(
            "expected an array or an object with {}",
            names.join(", ")
        ))),
    }
}

/// Exact integer arithmetic when possible, floating point otherwise.
fn arith(values: &[Value], subtract: bool) -> Result<Value, RpcError> {
    if values.iter().any(|v| !v.is_number()) {
        return Err(invalid_params("all parameters must be numbers"));
    }
    let ints: Option<Vec<i64>> = values.iter().map(Value::as_i64).collect();
    if let Some(ints) = ints {
        let mut iter = ints.into_iter();
        let mut acc = iter.next().unwrap_or(0);
        let mut overflow = false;
        for v in iter {
            match if subtract {
                acc.checked_sub(v)
            } else {
                acc.checked_add(v)
            } {
                Some(r) => acc = r,
                None => overflow = true,
            }
        }
        if !overflow {
            return Ok(json!(acc));
        }
    }
    let floats: Vec<f64> = values.iter().filter_map(Value::as_f64).collect();
    let mut iter = floats.into_iter();
    let first = iter.next().unwrap_or(0.0);
    let r = iter.fold(first, |acc, v| if subtract { acc - v } else { acc + v });
    if r.is_finite() {
        Ok(json!(r))
    } else {
        Err(invalid_params("result is not a finite number"))
    }
}

async fn call(method: &str, params: Option<Value>, max_sleep_ms: u64) -> Result<Value, RpcError> {
    match method {
        "echo" => Ok(params.unwrap_or(Value::Null)),
        "add" => {
            let v = numbers(&params, &["a", "b"])?;
            if v.is_empty() {
                return Err(invalid_params("add needs at least one number"));
            }
            arith(&v, false)
        }
        "subtract" => {
            let v = numbers(&params, &["minuend", "subtrahend"])?;
            if v.len() != 2 {
                return Err(invalid_params("subtract needs exactly two numbers"));
            }
            arith(&v, true)
        }
        "sleep" => {
            let ms = match &params {
                Some(Value::Array(a)) if a.len() == 1 => a[0].as_u64(),
                Some(Value::Object(m)) => m.get("ms").and_then(Value::as_u64),
                _ => None,
            }
            .ok_or_else(|| invalid_params("sleep takes [ms] or {\"ms\": n}"))?;
            if ms > max_sleep_ms {
                return Err(invalid_params(&format!(
                    "ms must be at most {max_sleep_ms}"
                )));
            }
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(json!({ "slept_ms": ms }))
        }
        "error" => {
            let (code, message, data) = match &params {
                Some(Value::Array(a)) if !a.is_empty() => (
                    a[0].as_i64(),
                    a.get(1).and_then(Value::as_str).map(str::to_string),
                    a.get(2).cloned(),
                ),
                Some(Value::Object(m)) => (
                    m.get("code").and_then(Value::as_i64),
                    m.get("message").and_then(Value::as_str).map(str::to_string),
                    m.get("data").cloned(),
                ),
                _ => (None, None, None),
            };
            let code = code.ok_or_else(|| invalid_params("error takes an integer code"))?;
            Err((
                code,
                message.unwrap_or_else(|| "Requested error".to_string()),
                data,
            ))
        }
        _ => Err((
            METHOD_NOT_FOUND,
            "Method not found".to_string(),
            Some(json!({ "method": method, "available": METHODS })),
        )),
    }
}

/// Process one request object; `None` for notifications.
async fn process(item: Value, max_sleep_ms: u64) -> Option<Value> {
    let Value::Object(obj) = item else {
        return Some(error_response(
            Value::Null,
            INVALID_REQUEST,
            "Invalid Request",
            Some(json!("request must be an object")),
        ));
    };
    let is_notification = !obj.contains_key("id");
    let id = obj.get("id").cloned().unwrap_or(Value::Null);
    if !matches!(id, Value::Null | Value::String(_) | Value::Number(_)) {
        return Some(error_response(
            Value::Null,
            INVALID_REQUEST,
            "Invalid Request",
            Some(json!("id must be a string, a number or null")),
        ));
    }
    let invalid = |why: &str| {
        Some(error_response(
            id.clone(),
            INVALID_REQUEST,
            "Invalid Request",
            Some(json!(why)),
        ))
    };
    if obj.get("jsonrpc") != Some(&json!("2.0")) {
        return invalid("jsonrpc must be exactly \"2.0\"");
    }
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return invalid("method must be a string");
    };
    let params = obj.get("params").cloned();
    if params
        .as_ref()
        .is_some_and(|p| !p.is_array() && !p.is_object())
    {
        if is_notification {
            return None;
        }
        return Some(error_response(
            id,
            INVALID_PARAMS,
            "Invalid params",
            Some(json!("params must be an array or an object")),
        ));
    }
    let result = call(method, params, max_sleep_ms).await;
    if is_notification {
        return None;
    }
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "result": r, "id": id }),
        Err((code, message, data)) => error_response(id, code, &message, data),
    })
}

fn json_reply(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

async fn jsonrpc_handler(State(config): State<Arc<Config>>, body: Bytes) -> Response {
    let max_batch = if config.public_mode {
        MAX_BATCH_PUBLIC
    } else {
        MAX_BATCH
    };
    let max_sleep = config.max_delay_ms();
    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return json_reply(error_response(
                Value::Null,
                PARSE_ERROR,
                "Parse error",
                Some(json!(e.to_string())),
            ))
        }
    };
    match value {
        Value::Array(items) => {
            if items.is_empty() {
                return json_reply(error_response(
                    Value::Null,
                    INVALID_REQUEST,
                    "Invalid Request",
                    Some(json!("empty batch")),
                ));
            }
            if items.len() > max_batch {
                return json_reply(error_response(
                    Value::Null,
                    INVALID_REQUEST,
                    "Invalid Request",
                    Some(json!(format!("batch exceeds {max_batch} requests"))),
                ));
            }
            let replies: Vec<Value> = futures_util::future::join_all(
                items.into_iter().map(|item| process(item, max_sleep)),
            )
            .await
            .into_iter()
            .flatten()
            .collect();
            if replies.is_empty() {
                StatusCode::NO_CONTENT.into_response()
            } else {
                json_reply(Value::Array(replies))
            }
        }
        other => match process(other, max_sleep).await {
            Some(reply) => json_reply(reply),
            None => StatusCode::NO_CONTENT.into_response(),
        },
    }
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new().route("/jsonrpc", post(jsonrpc_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![Endpoint::new(
        "/jsonrpc",
        &["POST"],
        category::ORCHESTRATION,
        "Generic JSON-RPC 2.0 endpoint (batches, notifications, standard errors)",
    )
    .description(
        "Methods: echo (returns params), add ([numbers] or {a, b}), subtract ([minuend, \
         subtrahend] or named), sleep ([ms] or {ms}, capped by the instance delay limit), error \
         ([code, message?, data?] or {code, message, data}: replies with that error). Batches \
         (max 100; public 20) run concurrently; notifications get no reply (204 when nothing is \
         left to answer). Errors: -32700 parse, -32600 invalid request, -32601 method not found, \
         -32602 invalid params. HTTP status is 200 for every reply.",
    )
    .example(
        Example::post("Subtract (named params)", "/jsonrpc")
            .json(r#"{"jsonrpc":"2.0","method":"subtract","params":{"minuend":42,"subtrahend":23},"id":1}"#),
    )
    .example(Example::post("Batch with a notification", "/jsonrpc").json(
        r#"[{"jsonrpc":"2.0","method":"add","params":[1,2,4],"id":"1"},{"jsonrpc":"2.0","method":"echo","params":{"hello":"world"}},{"jsonrpc":"2.0","method":"echo","params":["x"],"id":"2"},{"jsonrpc":"2.0","method":"nope","id":"3"}]"#,
    ))
    .example(
        Example::post("Requested error", "/jsonrpc")
            .json(r#"{"jsonrpc":"2.0","method":"error","params":{"code":-32001,"message":"Rate limited"},"id":7}"#),
    )
    .example(
        Example::post("Notification (no reply)", "/jsonrpc")
            .json(r#"{"jsonrpc":"2.0","method":"echo","params":["fire and forget"]}"#)
            .expect_status(204),
    )]
}

pub fn openapi_paths() -> Value {
    let request = json!({
        "type": "object",
        "required": ["jsonrpc", "method"],
        "properties": {
            "jsonrpc": { "type": "string", "enum": ["2.0"] },
            "method": { "type": "string", "enum": METHODS },
            "params": { "oneOf": [{ "type": "array", "items": {} }, { "type": "object" }] },
            "id": { "oneOf": [{ "type": "string" }, { "type": "number" }], "nullable": true }
        }
    });
    let mut ops = Map::new();
    ops.insert(
        "post".into(),
        json!({
            "tags": ["Orchestration"],
            "summary": "JSON-RPC 2.0 endpoint",
            "operationId": "jsonRpc",
            "requestBody": { "required": true, "content": { "application/json": { "schema": {
                "oneOf": [request, { "type": "array", "items": request }]
            } } } },
            "responses": {
                "200": { "description": "Response object or array of response objects" },
                "204": { "description": "Only notifications were sent" }
            }
        }),
    );
    json!({ "/jsonrpc": Value::Object(ops) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_json, module_app};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn rpc(body: &str) -> (StatusCode, Option<Value>) {
        let app = module_app(router);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/jsonrpc")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = resp.status();
        if status == StatusCode::NO_CONTENT {
            return (status, None);
        }
        (status, Some(body_json(resp).await))
    }

    #[tokio::test]
    async fn spec_examples() {
        let (_, r) = rpc(r#"{"jsonrpc":"2.0","method":"subtract","params":[42,23],"id":1}"#).await;
        assert_eq!(r, Some(json!({"jsonrpc":"2.0","result":19,"id":1})));
        let (_, r) = rpc(
            r#"{"jsonrpc":"2.0","method":"subtract","params":{"subtrahend":23,"minuend":42},"id":4}"#,
        )
        .await;
        assert_eq!(r.expect("reply")["result"], 19);
        let (status, r) = rpc(r#"{"jsonrpc":"2.0","method":"update","params":[1,2,3]}"#).await;
        assert_eq!((status, r), (StatusCode::NO_CONTENT, None));
        let (_, r) = rpc(r#"{"jsonrpc":"2.0","method":"foobar","id":"1"}"#).await;
        assert_eq!(r.expect("reply")["error"]["code"], METHOD_NOT_FOUND);
        let (_, r) = rpc(r#"{"jsonrpc":"2.0","method":"foobar,"params":"bar","baz]"#).await;
        let r = r.expect("reply");
        assert_eq!(r["error"]["code"], PARSE_ERROR);
        assert_eq!(r["id"], Value::Null);
        let (_, r) = rpc(r#"{"jsonrpc":"2.0","method":1,"params":"bar"}"#).await;
        assert_eq!(r.expect("reply")["error"]["code"], INVALID_REQUEST);
        let (_, r) = rpc("[]").await;
        assert_eq!(r.expect("reply")["error"]["code"], INVALID_REQUEST);
        let (_, r) = rpc("[1,2]").await;
        let r = r.expect("reply");
        assert_eq!(r.as_array().expect("array").len(), 2);
        assert_eq!(r[1]["error"]["code"], INVALID_REQUEST);
    }

    #[tokio::test]
    async fn batches_and_notifications() {
        let (_, r) = rpc(r#"[
            {"jsonrpc":"2.0","method":"add","params":[1,2,4],"id":"1"},
            {"jsonrpc":"2.0","method":"echo","params":[7]},
            {"jsonrpc":"2.0","method":"subtract","params":[42,23],"id":"2"},
            {"foo":"boo"},
            {"jsonrpc":"2.0","method":"foo.get","params":{"name":"myself"},"id":"5"},
            {"jsonrpc":"2.0","method":"add","params":{"a":1.5,"b":2},"id":"9"}
        ]"#)
        .await;
        let r = r.expect("reply");
        let arr = r.as_array().expect("array");
        assert_eq!(arr.len(), 5);
        assert_eq!(arr[0]["result"], 7);
        assert_eq!(arr[1]["result"], 19);
        assert_eq!(arr[2]["error"]["code"], INVALID_REQUEST);
        assert_eq!(arr[3]["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(arr[4]["result"], 3.5);
        let (status, _) = rpc(
            r#"[{"jsonrpc":"2.0","method":"echo","params":[1]},{"jsonrpc":"2.0","method":"echo"}]"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn params_errors_and_sleep() {
        let (_, r) = rpc(r#"{"jsonrpc":"2.0","method":"add","params":["x"],"id":1}"#).await;
        assert_eq!(r.expect("reply")["error"]["code"], INVALID_PARAMS);
        let (_, r) = rpc(r#"{"jsonrpc":"2.0","method":"echo","params":5,"id":1}"#).await;
        assert_eq!(r.expect("reply")["error"]["code"], INVALID_PARAMS);
        let (_, r) = rpc(
            r#"{"jsonrpc":"2.0","method":"error","params":{"code":-32001,"message":"Rate limited","data":{"retry":1}},"id":3}"#,
        )
        .await;
        let r = r.expect("reply");
        assert_eq!(
            r["error"],
            json!({"code": -32001, "message": "Rate limited", "data": {"retry": 1}})
        );
        assert_eq!(r["id"], 3);
        let (_, r) = rpc(r#"{"jsonrpc":"2.0","method":"sleep","params":[10],"id":1}"#).await;
        assert_eq!(r.expect("reply")["result"]["slept_ms"], 10);
        let (_, r) =
            rpc(r#"{"jsonrpc":"2.0","method":"sleep","params":{"ms":99999999},"id":1}"#).await;
        assert_eq!(r.expect("reply")["error"]["code"], INVALID_PARAMS);
        let (_, r) = rpc(r#"{"jsonrpc":"1.0","method":"echo","id":1}"#).await;
        let r = r.expect("reply");
        assert_eq!(r["error"]["code"], INVALID_REQUEST);
        assert_eq!(r["id"], 1);
        let (_, r) =
            rpc(r#"{"jsonrpc":"2.0","method":"add","params":[9223372036854775807,1],"id":1}"#)
                .await;
        assert!(r.expect("reply")["result"].is_f64());
        let big: Vec<String> = (0..101)
            .map(|i| format!(r#"{{"jsonrpc":"2.0","method":"echo","id":{i}}}"#))
            .collect();
        let (_, r) = rpc(&format!("[{}]", big.join(","))).await;
        assert_eq!(r.expect("reply")["error"]["code"], INVALID_REQUEST);
    }
}
