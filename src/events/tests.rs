use std::time::Duration;

use axum::http::StatusCode;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message as TMessage;
use tower::ServiceExt;

use crate::config::Config;
use crate::mocks::asyncapi;
use crate::mocks::capture::{Sender, TrafficScope};
use crate::mocks::spec::ParseLimits;
use crate::state::AppState;
use crate::test_support::{body_json, get_request, json_request, test_app_with, test_state_with};

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// An order events API in AsyncAPI 3.0: Rustybin sends `orderCreated`,
/// receives `orderCancel`, and answers a `quoteRequest` with a `quote`.
const V3: &str = r##"{
  "asyncapi": "3.0.0",
  "info": {"title": "Order events", "version": "1.0.0"},
  "servers": {"main": {"host": "example.com", "protocol": "wss"}},
  "channels": {
    "orderCreated": {
      "address": "orders/created",
      "messages": {"orderCreated": {"$ref": "#/components/messages/OrderCreated"}}
    },
    "orderCancel": {
      "address": "orders/cancel",
      "messages": {"orderCancel": {"$ref": "#/components/messages/OrderCancel"}}
    },
    "quotes": {
      "address": "quotes",
      "messages": {
        "quoteRequest": {"name": "quoteRequest", "payload": {"type": "object", "required": ["sku"], "properties": {"sku": {"type": "string"}}}},
        "quote": {"name": "quote", "payload": {"type": "object", "required": ["price"], "properties": {"price": {"type": "number"}}}}
      }
    }
  },
  "operations": {
    "publishOrderCreated": {"action": "send", "channel": {"$ref": "#/channels/orderCreated"}},
    "receiveOrderCancel": {"action": "receive", "channel": {"$ref": "#/channels/orderCancel"}},
    "askQuote": {
      "action": "receive", "channel": {"$ref": "#/channels/quotes"},
      "messages": [{"$ref": "#/channels/quotes/messages/quoteRequest"}],
      "reply": {"channel": {"$ref": "#/channels/quotes"}, "messages": [{"$ref": "#/channels/quotes/messages/quote"}]}
    }
  },
  "components": {
    "messages": {
      "OrderCreated": {"name": "orderCreated", "payload": {"$ref": "#/components/schemas/Order"}},
      "OrderCancel": {"name": "orderCancel", "payload": {"type": "object", "required": ["id", "reason"], "additionalProperties": false, "properties": {"id": {"type": "integer"}, "reason": {"type": "string", "minLength": 3}}}}
    },
    "schemas": {
      "Order": {"type": "object", "required": ["id", "total"], "properties": {"id": {"type": "integer", "minimum": 1}, "total": {"type": "number"}, "status": {"type": "string", "enum": ["new", "paid"]}}}
    }
  }
}"##;

/// The same API in AsyncAPI 2.6 (no reply there: 2.x has none).
const V2: &str = r##"{
  "asyncapi": "2.6.0",
  "info": {"title": "Order events", "version": "1.0.0"},
  "channels": {
    "orders/created": {"subscribe": {"operationId": "publishOrderCreated", "message": {"$ref": "#/components/messages/OrderCreated"}}},
    "orders/cancel": {"publish": {"operationId": "receiveOrderCancel", "message": {"$ref": "#/components/messages/OrderCancel"}}},
    "quotes": {
      "publish": {"message": {"name": "quoteRequest", "payload": {"type": "object", "required": ["sku"], "properties": {"sku": {"type": "string"}}}}},
      "subscribe": {"message": {"name": "quote", "payload": {"type": "object", "required": ["price"], "properties": {"price": {"type": "number"}}}}}
    }
  },
  "components": {
    "messages": {
      "OrderCreated": {"name": "orderCreated", "payload": {"$ref": "#/components/schemas/Order"}},
      "OrderCancel": {"name": "orderCancel", "payload": {"type": "object", "required": ["id", "reason"], "additionalProperties": false, "properties": {"id": {"type": "integer"}, "reason": {"type": "string", "minLength": 3}}}}
    },
    "schemas": {
      "Order": {"type": "object", "required": ["id", "total"], "properties": {"id": {"type": "integer", "minimum": 1}, "total": {"type": "number"}, "status": {"type": "string", "enum": ["new", "paid"]}}}
    }
  }
}"##;

const LIMITS: ParseLimits = ParseLimits {
    max_bytes: 1024 * 1024,
    max_operations: 500,
};

fn open_config() -> Config {
    let mut c = Config::for_tests();
    c.control_auth = crate::control_auth::ControlAuth::Open;
    c
}

fn state() -> AppState {
    test_state_with(open_config())
}

/// Create and deploy an AsyncAPI mock; `channels` are its settings.
async fn create(state: &AppState, name: &str, doc: &str, channels: Value) {
    let app = test_app_with(state.clone());
    let spec: Value = serde_json::from_str(doc).expect("document");
    let body = json!({ "name": name, "spec": spec, "channels": channels });
    let resp = app
        .oneshot(json_request("POST", "/_rustybin/mocks", &body))
        .await
        .expect("create");
    let status = resp.status();
    let v = body_json(resp).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert_eq!(v["kind"], "asyncapi");
}

async fn call(state: &AppState, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let app = test_app_with(state.clone());
    let req = if method == "GET" {
        get_request(uri)
    } else {
        json_request(method, uri, &body)
    };
    let resp = app.oneshot(req).await.expect("call");
    let status = resp.status();
    (status, body_json(resp).await)
}

/// The whole application on a real port (a WebSocket needs one).
async fn serve(state: AppState) -> String {
    let app: Router = crate::build_app(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
    });
    format!("{addr}")
}

async fn connect(addr: &str, path: &str) -> Client {
    tokio_tungstenite::connect_async(format!("ws://{addr}{path}"))
        .await
        .expect("upgrade")
        .0
}

/// The next text frame as JSON (`None`: the connection ended).
async fn next(socket: &mut Client) -> Option<Value> {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("a frame in time")?;
        match m.ok()? {
            TMessage::Text(t) => return serde_json::from_str(&t).ok(),
            TMessage::Close(_) => return None,
            _ => continue,
        }
    }
}

/// Wait until a channel has `n` listeners of a kind.
async fn listening(state: &AppState, mock: &str, channel: &str, kind: &str, n: u64) {
    for _ in 0..200 {
        if state.events.status(mock, channel)[kind] == json!(n) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{kind} listeners of {channel} never reached {n}");
}

#[test]
fn both_versions_parse_into_the_same_channels() {
    let three = crate::mocks::spec::parse(V3, LIMITS).expect("3.0");
    let two = crate::mocks::spec::parse(V2, LIMITS).expect("2.6");
    assert!(three.operations.is_empty() && two.operations.is_empty());
    let (e3, e2) = (three.events.expect("events"), two.events.expect("events"));
    assert_eq!(e3.asyncapi, "3.0.0");
    assert_eq!(e2.asyncapi, "2.6.0");
    let shape = |e: &asyncapi::EventSpec| -> Vec<(String, String, Vec<String>, Vec<String>)> {
        let mut out: Vec<_> = e
            .channels
            .iter()
            .map(|c| {
                (
                    c.id.clone(),
                    c.address.clone(),
                    c.send.iter().map(|m| m.name.clone()).collect(),
                    c.receive.iter().map(|m| m.name.clone()).collect(),
                )
            })
            .collect();
        out.sort();
        out
    };
    assert_eq!(shape(&e3), shape(&e2));
    assert_eq!(
        shape(&e3),
        vec![
            (
                "orders-cancel".to_string(),
                "orders/cancel".to_string(),
                vec![],
                vec!["orderCancel".to_string()]
            ),
            (
                "orders-created".to_string(),
                "orders/created".to_string(),
                vec!["orderCreated".to_string()],
                vec![]
            ),
            (
                "quotes".to_string(),
                "quotes".to_string(),
                vec!["quote".to_string()],
                vec!["quoteRequest".to_string()]
            ),
        ]
    );
    // Only 3.0 can declare a reply.
    assert!(e3.channel("quotes").expect("quotes").reply);
    assert!(!e2.channel("quotes").expect("quotes").reply);
    assert_eq!(e3.protocols, vec!["wss"]);
}

#[test]
fn odd_documents_are_refused_or_warned_about_never_a_panic() {
    let parse = |v: Value| crate::mocks::spec::parse(&v.to_string(), LIMITS);
    assert!(parse(json!({"asyncapi": "1.2.0"}))
        .expect_err("old")
        .contains("unsupported AsyncAPI version"));
    assert!(parse(json!({"asyncapi": "3.0.0", "channels": []}))
        .expect_err("channels")
        .contains("must be an object"));
    assert!(
        parse(json!({"asyncapi": "3.0.0", "channels": {"a": {"$ref": "other.yaml#/a"}}}))
            .expect_err("external")
            .contains("external $ref")
    );
    // Nothing at all is a mock with no channels.
    let empty = parse(json!({"asyncapi": "3.0.0"})).expect("empty");
    assert!(empty.events.expect("events").channels.is_empty());
    // An operation on a channel that is not there, an unknown action, a
    // channel nobody names and two addresses with one slug.
    let odd = parse(json!({
        "asyncapi": "3.1.0",
        "channels": {
            "a": {"address": "a/b", "messages": {"m": {"payload": {"type": "string"}}}},
            "b": {"address": "a b", "messages": {"m": {"payload": {"type": "string"}}}}
        },
        "operations": {
            "x": {"action": "send", "channel": {"$ref": "#/channels/nope"}},
            "y": {"action": "dance", "channel": {"$ref": "#/channels/a"}}
        }
    }))
    .expect("odd");
    let e = odd.events.expect("events");
    let ids: Vec<&str> = e.channels.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, vec!["a-b", "a-b-2"]);
    assert!(odd.warnings.iter().any(|w| w.contains("newer than 3.0")));
    assert!(odd.warnings.iter().any(|w| w.contains("operation x")));
    assert!(odd
        .warnings
        .iter()
        .any(|w| w.contains("not send or receive")));
    // The channel no operation names publishes its messages.
    assert_eq!(e.channels[1].send.len(), 1);
}

#[test]
fn the_same_seed_and_number_give_the_same_message() {
    let spec = crate::mocks::spec::parse(V3, LIMITS).expect("parse");
    let e = spec.events.as_deref().expect("events");
    let ch = e.channel("orders-created").expect("channel");
    let a = asyncapi::outgoing(&spec.root, ch, &[], 1, 3).expect("message");
    let b = asyncapi::outgoing(&spec.root, ch, &[], 1, 3).expect("message");
    assert_eq!(a.payload, b.payload);
    assert_eq!(a.source, "generated");
    assert!(a.payload["id"].as_i64().is_some_and(|i| i >= 1));
    // A stored example comes before anything generated, in turn.
    let stored = [json!({"id": 7, "total": 1.5}), json!({"id": 8, "total": 2})];
    let s1 = asyncapi::outgoing(&spec.root, ch, &stored, 1, 1).expect("message");
    let s2 = asyncapi::outgoing(&spec.root, ch, &stored, 1, 2).expect("message");
    assert_eq!((s1.source, &s1.payload["id"]), ("stored", &json!(7)));
    assert_eq!(s2.payload["id"], 8);
    // A channel that only receives publishes nothing.
    let cancel = e.channel("orders-cancel").expect("channel");
    assert!(asyncapi::outgoing(&spec.root, cancel, &[], 1, 1).is_none());
    let ok = asyncapi::check_incoming(&spec.root, cancel, &json!({"id": 1, "reason": "late"}));
    assert_eq!(ok.message.as_deref(), Some("orderCancel"));
    let bad = asyncapi::check_incoming(&spec.root, cancel, &json!({"id": "x"}));
    assert!(!bad.valid());
    // And one that only sends accepts nothing.
    assert!(!asyncapi::check_incoming(&spec.root, ch, &json!({})).valid());
}

#[tokio::test]
async fn a_websocket_client_gets_valid_messages_and_an_error_for_an_invalid_one() {
    let state = state();
    create(
        &state,
        "orders",
        V3,
        json!({ "orders-created": { "every_ms": 100 } }),
    )
    .await;
    let addr = serve(state.clone()).await;
    // The channel publishes on its own: two generated messages, valid
    // against the document's schema.
    let mut socket = connect(&addr, "/events/orders/orders-created").await;
    let spec = crate::mocks::spec::parse(V3, LIMITS).expect("parse");
    let schema = json!({"$ref": "#/components/schemas/Order"});
    for _ in 0..2 {
        let m = next(&mut socket).await.expect("a message");
        let issues = crate::mocks::schema::validate(
            &m,
            &schema,
            &spec.root,
            crate::mocks::schema::Direction::Response,
        );
        assert!(issues.is_empty(), "{m}: {issues:?}");
    }
    drop(socket);

    // A channel that receives: an invalid message is answered with an
    // error event and kept with its validation report.
    let mut cancel = connect(&addr, "/events/orders/orders-cancel").await;
    cancel
        .send(TMessage::Text(r#"{"id": "one", "reason": "no"}"#.into()))
        .await
        .expect("send");
    let e = next(&mut cancel).await.expect("an error event");
    assert_eq!(e["error"]["type"], "invalid_message");
    assert!(e["error"]["issues"]
        .as_array()
        .is_some_and(|i| !i.is_empty()));
    cancel
        .send(TMessage::Text("not json".into()))
        .await
        .expect("send");
    let e = next(&mut cancel).await.expect("an error event");
    assert!(e["error"]["issues"][0]["message"]
        .as_str()
        .is_some_and(|m| m.contains("not JSON")));
    cancel
        .send(TMessage::Text(
            r#"{"id": 4, "reason": "changed my mind"}"#.into(),
        ))
        .await
        .expect("send");
    // A valid one gets no answer; the log has all three.
    let mut seen = Value::Null;
    for _ in 0..100 {
        let (_, v) = call(
            &state,
            "GET",
            "/_rustybin/mocks/orders/channels/orders-cancel/messages",
            Value::Null,
        )
        .await;
        if v["count"] == 3 {
            seen = v;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let list = seen["messages"].as_array().expect("three messages");
    assert_eq!(list[0]["valid"], true);
    assert_eq!(list[0]["message"], "orderCancel");
    assert_eq!(list[0]["transport"], "ws");
    assert_eq!(list[1]["valid"], false);
    assert_eq!(list[2]["direction"], "received");
    assert_eq!(list[2]["valid"], false);
    assert!(list[2]["issues"].as_array().is_some_and(|i| !i.is_empty()));
    assert!(list[2].get("sender").is_none());
    // The other channel's messages are its own.
    let (_, created) = call(
        &state,
        "GET",
        "/_rustybin/mocks/orders/channels/orders-created/messages?limit=1",
        Value::Null,
    )
    .await;
    assert_eq!(created["messages"][0]["direction"], "sent");
    assert_eq!(created["messages"][0]["source"], "generated");
    assert_eq!(created["messages"][0]["delivered"]["ws"], 1);
}

/// One event of a server-sent event stream: (event, id, data).
async fn next_sse(resp: &mut reqwest::Response, buffer: &mut String) -> (String, String, Value) {
    loop {
        if let Some(end) = buffer.find("\n\n") {
            let block: String = buffer.drain(..end + 2).collect();
            let field = |name: &str| {
                block
                    .lines()
                    .find_map(|l| l.strip_prefix(name))
                    .map(|v| v.trim().to_string())
            };
            if let Some(data) = field("data:") {
                let v = serde_json::from_str(&data).unwrap_or(Value::String(data));
                return (
                    field("event:").unwrap_or_default(),
                    field("id:").unwrap_or_default(),
                    v,
                );
            }
            continue;
        }
        let chunk = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
            .await
            .expect("a chunk in time")
            .expect("chunk")
            .expect("the stream goes on");
        buffer.push_str(&String::from_utf8_lossy(&chunk));
    }
}

#[tokio::test]
async fn sse_and_websocket_subscribers_get_the_same_messages() {
    let state = state();
    // `off`: only what is published by hand, so the test decides.
    create(
        &state,
        "orders",
        V3,
        json!({ "orders-created": { "pattern": "off" } }),
    )
    .await;
    let addr = serve(state.clone()).await;
    let mut socket = connect(&addr, "/events/orders/orders-created").await;
    let mut sse = reqwest::Client::new()
        .get(format!("http://{addr}/events/orders/orders-created"))
        .header("accept", "text/event-stream")
        .send()
        .await
        .expect("sse");
    assert_eq!(sse.status(), 200);
    assert!(sse
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream")));
    listening(&state, "orders", "orders-created", "ws", 1).await;
    listening(&state, "orders", "orders-created", "sse", 1).await;

    let (status, v) = call(
        &state,
        "POST",
        "/_rustybin/mocks/orders/channels/orders-created/publish",
        json!({ "count": 2 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["published"], 2);
    assert_eq!(
        v["messages"][0]["delivered"],
        json!({"ws": 1, "sse": 1, "webhook": 0})
    );
    let mut buffer = String::new();
    for n in 1..=2 {
        let from_ws = next(&mut socket).await.expect("ws message");
        let (event, id, from_sse) = next_sse(&mut sse, &mut buffer).await;
        assert_eq!(from_ws, from_sse);
        assert_eq!(event, "orderCreated");
        assert_eq!(id, n.to_string());
        assert_eq!(from_ws, v["messages"][n - 1]["payload"]);
    }
    // A payload given is published as it is.
    let (_, v) = call(
        &state,
        "POST",
        "/_rustybin/mocks/orders/channels/orders-created/publish",
        json!({ "payload": { "id": 99, "total": 12.5 } }),
    )
    .await;
    assert_eq!(v["messages"][0]["source"], "given");
    assert_eq!(next(&mut socket).await.expect("ws")["id"], 99);

    // A plain GET describes the channel.
    let (status, info) = call(&state, "GET", "/events/orders/orders-created", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["pattern"], "off");
    assert_eq!(info["send"][0]["name"], "orderCreated");
    // The same below the mock's own root.
    let (status, _) = call(
        &state,
        "GET",
        "/mock/orders/_rustybin/events/orders-created",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Turned off, the mock's subscribers are let go.
    let (status, _) = call(&state, "POST", "/_rustybin/mocks/orders/disable", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(next(&mut socket).await.is_none());
    let (status, _) = call(&state, "GET", "/events/orders/orders-created", Value::Null).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn a_post_publishes_as_a_client_and_a_reply_follows_on_receive() {
    let state = state();
    create(&state, "orders", V3, json!({})).await;
    // Valid and invalid over HTTP.
    let (status, v) = call(
        &state,
        "POST",
        "/events/orders/orders-cancel",
        json!({ "id": 1, "reason": "late" }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["message"], "orderCancel");
    let (status, v) = call(
        &state,
        "POST",
        "/events/orders/orders-cancel",
        json!({ "id": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["error"]["type"], "invalid_message");
    // A channel that only sends accepts nothing.
    let (status, _) = call(&state, "POST", "/events/orders/orders-created", json!({})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // The document declares a reply on `quotes`: its pattern is
    // on_receive, and a valid request is answered with the reply message.
    let (_, channels) = call(
        &state,
        "GET",
        "/_rustybin/mocks/orders/channels",
        Value::Null,
    )
    .await;
    let quotes = channels["channels"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["id"] == "quotes"))
        .expect("quotes");
    assert_eq!(quotes["pattern"], "on_receive");
    assert_eq!(quotes["sample"]["message"], "quoteRequest");
    let (status, v) = call(
        &state,
        "POST",
        "/events/orders/quotes",
        json!({ "sku": "a-1" }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(v["reply"]["message"], "quote");
    assert!(v["reply"]["payload"]["price"].is_number());
    // Other methods, other channels, other mocks.
    let (status, _) = call(&state, "PUT", "/events/orders/quotes", json!({})).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let (status, v) = call(&state, "GET", "/events/orders/nope", Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(v["channels"].as_array().map(Vec::len), Some(3));
    let (status, _) = call(&state, "GET", "/events/petstore/pets", Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, v) = call(
        &state,
        "GET",
        "/_rustybin/mocks/petstore/channels",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(v["reason"], "not_asyncapi");
}

#[tokio::test]
async fn faults_drop_and_cut_messages() {
    let state = state();
    create(
        &state,
        "lossy",
        V3,
        json!({
            "orders-created": { "every_ms": 100, "drop_percent": 100 },
            "quotes": { "pattern": "every", "every_ms": 100, "malformed_percent": 100 }
        }),
    )
    .await;
    let addr = serve(state.clone()).await;
    // Every message of the first channel is lost: the log has them, the
    // subscriber nothing.
    let mut dropped = connect(&addr, "/events/lossy/orders-created").await;
    let mut cut = connect(&addr, "/events/lossy/quotes").await;
    let frame = tokio::time::timeout(Duration::from_secs(5), cut.next())
        .await
        .expect("a frame")
        .expect("open")
        .expect("frame");
    let TMessage::Text(text) = frame else {
        panic!("a text frame");
    };
    assert!(serde_json::from_str::<Value>(&text).is_err(), "{text}");
    let (_, v) = call(
        &state,
        "GET",
        "/_rustybin/mocks/lossy/channels/quotes/messages?limit=1",
        Value::Null,
    )
    .await;
    assert_eq!(v["messages"][0]["fault"], "malformed");
    assert_eq!(v["messages"][0]["valid"], false);
    let mut seen = false;
    for _ in 0..100 {
        let (_, v) = call(
            &state,
            "GET",
            "/_rustybin/mocks/lossy/channels/orders-created/messages?limit=1",
            Value::Null,
        )
        .await;
        if v["messages"][0]["fault"] == "dropped" {
            assert_eq!(v["messages"][0]["delivered"]["ws"], 0);
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(seen, "a dropped message is in the log");
    assert!(
        tokio::time::timeout(Duration::from_millis(300), dropped.next())
            .await
            .is_err(),
        "nothing reaches the subscriber"
    );
}

#[tokio::test]
async fn the_publishing_task_ends_with_its_last_subscriber() {
    let state = state();
    create(
        &state,
        "orders",
        V3,
        json!({ "orders-created": { "every_ms": 100 } }),
    )
    .await;
    let addr = serve(state.clone()).await;
    let mut socket = connect(&addr, "/events/orders/orders-created").await;
    next(&mut socket).await.expect("a message");
    assert_eq!(state.events.open_connections(), 1);
    drop(socket);
    // No listener: the counter stops, and so does the connection count.
    listening(&state, "orders", "orders-created", "ws", 0).await;
    for _ in 0..100 {
        if state.events.open_connections() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(state.events.open_connections(), 0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = state.events.status("orders", "orders-created")["published"].clone();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        state.events.status("orders", "orders-created")["published"],
        before
    );
}

#[tokio::test]
async fn one_more_connection_than_the_cap_is_refused() {
    let state = state();
    create(&state, "orders", V3, json!({})).await;
    let (status, v) = call(
        &state,
        "PATCH",
        "/_rustybin/settings",
        json!({ "values": { "asyncapi.max_connections": 1 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let addr = serve(state.clone()).await;
    let _first = connect(&addr, "/events/orders/orders-created").await;
    let second = reqwest::Client::new()
        .get(format!("http://{addr}/events/orders/orders-created"))
        .header("accept", "text/event-stream")
        .send()
        .await
        .expect("answer");
    assert_eq!(second.status(), 429);
}

/// A receiver of webhook deliveries on loopback: the bodies and headers it
/// got.
async fn receiver() -> (String, tokio::sync::mpsc::Receiver<(String, Value)>) {
    use axum::routing::post;
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    const HOOK: &str = "/hook";
    let app = Router::new().route(
        HOOK,
        post(move |headers: axum::http::HeaderMap, body: String| {
            let tx = tx.clone();
            async move {
                let event = headers
                    .get("x-rustybin-event")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let _ = tx
                    .send((event, serde_json::from_str(&body).unwrap_or(Value::Null)))
                    .await;
                StatusCode::NO_CONTENT
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/hook"), rx)
}

#[tokio::test]
async fn a_webhook_subscriber_outside_the_allowlist_is_refused_before_any_call() {
    // Nothing is allowed for the purpose: the registration is refused.
    let state = state();
    create(&state, "orders", V3, json!({})).await;
    let (url, mut got) = receiver().await;
    let (status, v) = call(
        &state,
        "POST",
        "/_rustybin/mocks/orders/subscribers",
        json!({ "channel": "orders-created", "url": url }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["reason"], "egress_refused");
    assert_eq!(v["purpose"], "events");
    let (_, list) = call(
        &state,
        "GET",
        "/_rustybin/mocks/orders/subscribers",
        Value::Null,
    )
    .await;
    assert_eq!(list["count"], 0);
    let _ = call(
        &state,
        "POST",
        "/_rustybin/mocks/orders/channels/orders-created/publish",
        json!({}),
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), got.recv())
            .await
            .is_err(),
        "nothing was sent"
    );
}

#[tokio::test]
async fn an_allowed_webhook_subscriber_gets_the_channels_messages() {
    let mut config = open_config();
    config.egress_allow = vec!["127.0.0.1".into()];
    config.egress_allow_loopback = true;
    let state = test_state_with(config);
    create(
        &state,
        "orders",
        V3,
        json!({ "orders-created": { "pattern": "off" } }),
    )
    .await;
    let (url, mut got) = receiver().await;
    let (status, sub) = call(
        &state,
        "POST",
        "/_rustybin/mocks/orders/subscribers",
        json!({ "channel": "orders-created", "url": format!("{url}?token=hidden"), "ttl_seconds": 60 }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{sub}");
    // A query may hold a credential: the answer leaves it out.
    assert_eq!(sub["url"], url);
    assert_eq!(sub["query_hidden"], true);
    assert!(!sub.to_string().contains("token="));
    let (_, v) = call(
        &state,
        "POST",
        "/_rustybin/mocks/orders/channels/orders-created/publish",
        json!({}),
    )
    .await;
    assert_eq!(v["messages"][0]["delivered"]["webhook"], 1);
    let (event, body) = tokio::time::timeout(Duration::from_secs(5), got.recv())
        .await
        .expect("a delivery")
        .expect("open");
    assert_eq!(event, "orderCreated");
    assert_eq!(body, v["messages"][0]["payload"]);
    let mut delivered = json!(0);
    for _ in 0..100 {
        let (_, list) = call(
            &state,
            "GET",
            "/_rustybin/mocks/orders/subscribers",
            Value::Null,
        )
        .await;
        delivered = list["subscribers"][0]["delivered"].clone();
        if delivered == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(delivered, 1);
    // Unknown channel, bad URL, bad lifetime; then removal.
    for (body, want) in [
        (
            json!({ "channel": "nope", "url": url }),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({ "channel": "orders-created", "url": "ftp://x" }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "channel": "orders-created", "url": url, "ttl_seconds": 0 }),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, v) = call(&state, "POST", "/_rustybin/mocks/orders/subscribers", body).await;
        assert_eq!(status, want, "{v}");
    }
    let id = sub["id"].as_str().expect("id");
    let (status, _) = call(
        &state,
        "DELETE",
        &format!("/_rustybin/mocks/orders/subscribers/{id}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &state,
        "DELETE",
        &format!("/_rustybin/mocks/orders/subscribers/{id}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_deploy_report_names_what_is_not_served_or_generated() {
    let state = state();
    let doc = json!({
        "asyncapi": "2.6.0",
        "info": {"title": "Mixed", "version": "1"},
        "servers": {"broker": {"url": "broker:9092", "protocol": "kafka"}},
        "channels": {
            "readings": {
                "bindings": {"kafka": {"topic": "readings"}},
                "subscribe": {"message": {"name": "reading", "schemaFormat": "application/vnd.apache.avro;version=1.9.0", "payload": {"type": "record", "name": "Reading", "fields": []}}}
            },
            "alerts": {
                "subscribe": {"message": {"name": "alert", "payload": {"type": "object", "required": ["level"], "properties": {"level": {"type": "string"}}},
                    "examples": [{"payload": {"level": 5}}]}}
            },
            "empty": {}
        }
    });
    create(
        &state,
        "mixed",
        &doc.to_string(),
        json!({ "nope": { "every_ms": 500 } }),
    )
    .await;
    let (status, report) = call(&state, "GET", "/_rustybin/mocks/mixed/report", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let report = report.get("report").unwrap_or(&report).clone();
    // Findings, never errors: the mock deploys.
    assert_eq!(report["ok"], true, "{report}");
    let all = report.to_string();
    assert!(all.contains("kafka"), "{all}");
    assert!(all.contains("settings for channel \\\"nope\\\" are ignored"));
    let channels = report["channels"].as_array().expect("channels");
    let by = |id: &str| {
        channels
            .iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("channel {id}"))
    };
    assert!(by("readings")["findings"][0]["message"]
        .as_str()
        .is_some_and(|m| m.contains("avro") && m.contains("not generated")));
    assert_eq!(by("alerts")["status"], "warning");
    assert!(by("alerts")["findings"][0]["message"]
        .as_str()
        .is_some_and(|m| m.contains("example payload does not match")));
    assert_eq!(by("alerts")["send"][0]["source"], "example");
    assert!(by("empty")["findings"][0]["message"]
        .as_str()
        .is_some_and(|m| m.contains("no messages")));
    // The detail says what it is.
    let (_, detail) = call(&state, "GET", "/_rustybin/mocks/mixed", Value::Null).await;
    assert_eq!(detail["kind"], "asyncapi");
    assert_eq!(detail["spec"]["asyncapi"], "2.6.0");
    assert_eq!(detail["channels"].as_array().map(Vec::len), Some(3));
    assert_eq!(detail["operations"], json!([]));
}

#[tokio::test]
async fn channel_settings_are_validated_and_travel_in_the_document() {
    let state = state();
    let app = test_app_with(state.clone());
    let spec: Value = serde_json::from_str(V3).expect("document");
    for (channels, want) in [
        (json!({ "quotes": { "every_ms": 5 } }), "every_ms"),
        (json!({ "quotes": { "burst": 0 } }), "burst"),
        (json!({ "quotes": { "drop_percent": 101 } }), "drop_percent"),
    ] {
        let body = json!({ "name": "bad", "spec": spec, "channels": channels });
        let resp = app
            .clone()
            .oneshot(json_request("POST", "/_rustybin/mocks", &body))
            .await
            .expect("create");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(resp).await.to_string().contains(want));
    }
    create(
        &state,
        "orders",
        V3,
        json!({ "orders-created": { "pattern": "burst", "burst": 3, "examples": [{ "id": 5, "total": 1 }] } }),
    )
    .await;
    let resp = app
        .clone()
        .oneshot(get_request("/_rustybin/config/export"))
        .await
        .expect("export");
    let text = crate::test_support::body_string(resp).await;
    assert!(text.contains("pattern: burst"), "{text}");
    assert!(text.contains("asyncapi: 3.0.0"), "{text}");
    // Applied to a fresh instance, the mock is the same.
    let other = self::state();
    let resp = test_app_with(other.clone())
        .oneshot(json_request(
            "POST",
            "/_rustybin/config/apply",
            &json!({ "document": text }),
        ))
        .await
        .expect("apply");
    let status = resp.status();
    let v = body_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (_, channels) = call(
        &other,
        "GET",
        "/_rustybin/mocks/orders/channels",
        Value::Null,
    )
    .await;
    let created = channels["channels"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["id"] == "orders-created"))
        .expect("channel");
    assert_eq!(created["pattern"], "burst");
    assert_eq!(created["settings"]["burst"], 3);
    let (_, v) = call(
        &other,
        "POST",
        "/_rustybin/mocks/orders/channels/orders-created/publish",
        json!({}),
    )
    .await;
    assert_eq!(v["messages"][0]["source"], "stored");
    assert_eq!(v["messages"][0]["payload"]["id"], 5);
}

#[test]
fn a_caller_who_reads_their_own_traffic_sees_what_was_sent_and_what_they_published() {
    use crate::mocks::capture::{CaptureLog, EventRecord};
    let log = CaptureLog::new(10, 64 * 1024);
    let record = |direction: &'static str, sender: Option<Sender>| EventRecord {
        seq: 0,
        timestamp: String::new(),
        channel: "c".into(),
        direction,
        transport: None,
        message: None,
        payload: Some(json!({})),
        text: None,
        size: 2,
        truncated: false,
        valid: true,
        issues: Vec::new(),
        source: None,
        fault: None,
        delivered: None,
        revision: 1,
        sender,
    };
    log.record_event(record("sent", None));
    log.record_event(record("received", Some(Sender::Session("a".into()))));
    log.record_event(record("received", Some(Sender::Session("b".into()))));
    log.record_event(record("received", Some(Sender::Key("a".into()))));
    let seqs = |scope: &TrafficScope| -> Vec<u64> {
        log.events(scope, Some("c"), 0, 10)
            .0
            .iter()
            .map(|r| r.seq)
            .collect()
    };
    assert_eq!(seqs(&TrafficScope::All), vec![4, 3, 2, 1]);
    assert_eq!(
        seqs(&TrafficScope::Own(Sender::Session("a".into()))),
        vec![2, 1]
    );
    assert_eq!(
        seqs(&TrafficScope::Own(Sender::Key("a".into()))),
        vec![4, 1]
    );
    // After a sequence number, and bounded: the oldest go first.
    assert_eq!(log.events(&TrafficScope::All, None, 3, 10).0.len(), 1);
    for _ in 0..20 {
        log.record_event(record("sent", None));
    }
    assert_eq!(log.events(&TrafficScope::All, None, 0, 100).0.len(), 10);
    log.clear();
    assert!(log.events(&TrafficScope::All, None, 0, 100).0.is_empty());
}

#[tokio::test]
async fn a_mock_made_again_under_the_same_name_starts_its_channels_afresh() {
    let state = state();
    let off = json!({ "orders-created": { "pattern": "off" } });
    create(&state, "orders", V3, off.clone()).await;
    let addr = serve(state.clone()).await;
    let mut socket = connect(&addr, "/events/orders/orders-created").await;
    listening(&state, "orders", "orders-created", "ws", 1).await;
    let publish = "/_rustybin/mocks/orders/channels/orders-created/publish";
    let (_, first) = call(&state, "POST", publish, json!({ "count": 2 })).await;
    assert_eq!(
        state.events.status("orders", "orders-created")["published"],
        2
    );
    let (status, _) = call(&state, "DELETE", "/_rustybin/mocks/orders", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    create(&state, "orders", V3, off).await;
    // The new mock's first message is message one again, its log is its
    // own, and the old mock's listener is let go.
    let (_, again) = call(&state, "POST", publish, json!({})).await;
    assert_eq!(again["messages"][0]["seq"], 1);
    assert_eq!(
        again["messages"][0]["payload"],
        first["messages"][0]["payload"]
    );
    assert_eq!(again["messages"][0]["delivered"]["ws"], 0);
    assert_eq!(
        state.events.status("orders", "orders-created")["published"],
        1
    );
    let mut closed = false;
    for _ in 0..4 {
        if next(&mut socket).await.is_none() {
            closed = true;
            break;
        }
    }
    assert!(closed, "the deleted mock's listener was not let go");
}
