//! A2A module tests: both protocol versions over the JSON-RPC and HTTP+JSON
//! bindings, driven through the module router.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::test_support::{body_json, body_string, module_app, module_app_with_config, test_state};

fn app() -> Router {
    module_app(super::router)
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    text: String,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or(Value::Null)
    }

    /// `data:` payloads of an SSE body.
    fn events(&self) -> Vec<Value> {
        self.text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str(d.trim()).ok())
            .collect()
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> Resp {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "agents.test:8080");
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = app
        .clone()
        .oneshot(b.body(body).expect("request"))
        .await
        .expect("infallible");
    let status = resp.status();
    let headers = resp.headers().clone();
    let text = body_string(resp).await;
    Resp {
        status,
        headers,
        text,
    }
}

const V1: &[(&str, &str)] = &[("a2a-version", "1.0")];
const V03: &[(&str, &str)] = &[];

async fn rpc(
    app: &Router,
    agent: &str,
    headers: &[(&str, &str)],
    method: &str,
    params: Value,
) -> Resp {
    let path = if agent.is_empty() {
        "/a2a".to_string()
    } else {
        format!("/a2a/{agent}")
    };
    call(
        app,
        "POST",
        &path,
        headers,
        Some(json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params})),
    )
    .await
}

fn v1_msg(text: &str) -> Value {
    json!({"messageId": uuid::Uuid::new_v4().to_string(), "role": "ROLE_USER", "parts": [{"text": text}], "metadata": {"stepDelayMs": 1}})
}

fn v03_msg(text: &str) -> Value {
    json!({"kind": "message", "messageId": uuid::Uuid::new_v4().to_string(), "role": "user",
        "parts": [{"kind": "text", "text": text}], "metadata": {"stepDelayMs": 1}})
}

fn with_session<'a>(base: &'a [(&'a str, &'a str)], session: &'a str) -> Vec<(&'a str, &'a str)> {
    let mut v = base.to_vec();
    v.push(("x-rustybin-session", session));
    v
}

fn token(claims: Value) -> String {
    let state = test_state();
    let mut hdr = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    hdr.kid = Some("rustybin-rs256-key".into());
    jsonwebtoken::encode(&hdr, &claims, &state.jwt.rs256_encoding_key).expect("sign")
}

fn valid_token() -> String {
    let now = chrono::Utc::now().timestamp();
    token(
        json!({"sub": "demo", "iss": "http://agents.test:8080", "aud": "rustybin", "exp": now + 600, "iat": now, "scope": "openid"}),
    )
}

// ── Cards ───────────────────────────────────────────────────────────

#[tokio::test]
async fn cards_are_served_and_derived_from_host() {
    let app = app();
    let r = call(&app, "GET", "/.well-known/agent-card.json", &[], None).await;
    assert_eq!(r.status, StatusCode::OK);
    let c = r.json();
    assert_eq!(c["name"], "Echo Agent");
    assert_eq!(
        c["supportedInterfaces"][0]["url"],
        "http://agents.test:8080/a2a/echo"
    );
    assert_eq!(c["supportedInterfaces"][0]["protocolVersion"], "1.0");
    assert_eq!(
        c["url"], "http://agents.test:8080/a2a/echo",
        "v0.3 compat field"
    );
    assert_eq!(c["capabilities"]["streaming"], true);
    assert_eq!(c["capabilities"]["pushNotifications"], true);
    assert_eq!(c["provider"]["organization"], "Rustybin");
    let listed = c["capabilities"]["extensions"][0]["params"]["agents"]
        .as_array()
        .map(|a| a.len());
    assert_eq!(listed, Some(super::agents::AGENTS.len()));
    let etag = r
        .headers
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .expect("etag")
        .to_string();
    assert!(r.headers.get("cache-control").is_some());
    let again = call(
        &app,
        "GET",
        "/.well-known/agent-card.json",
        &[("if-none-match", &etag)],
        None,
    )
    .await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);

    let legacy = call(&app, "GET", "/.well-known/agent.json", &[], None)
        .await
        .json();
    assert_eq!(legacy["protocolVersion"], "0.3.0");
    assert_eq!(legacy["preferredTransport"], "JSONRPC");
    assert!(legacy.get("supportedInterfaces").is_none());

    let secure = call(
        &app,
        "GET",
        "/a2a/secure/.well-known/agent-card.json",
        &[],
        None,
    )
    .await
    .json();
    assert_eq!(secure["capabilities"]["extendedAgentCard"], true);
    assert_eq!(
        secure["securitySchemes"]["oauth2"]["oauth2SecurityScheme"]["flows"]["clientCredentials"]
            ["tokenUrl"],
        "http://agents.test:8080/oauth/token"
    );
    assert_eq!(
        secure["securitySchemes"]["oidc"]["openIdConnectSecurityScheme"]["openIdConnectUrl"],
        "http://agents.test:8080/.well-known/openid-configuration"
    );
    let by_get = call(&app, "GET", "/a2a/weather", &[], None).await.json();
    assert_eq!(by_get["name"], "Weather Agent");
    assert_eq!(by_get["skills"][0]["id"], "forecast");
    let legacy_agent = call(
        &app,
        "GET",
        "/a2a/weather/.well-known/agent.json",
        &[],
        None,
    )
    .await
    .json();
    assert_eq!(legacy_agent["url"], "http://agents.test:8080/a2a/weather");
    let unknown = call(
        &app,
        "GET",
        "/a2a/nope/.well-known/agent-card.json",
        &[],
        None,
    )
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    let dir = call(&app, "GET", "/a2a", &[], None).await.json();
    assert_eq!(
        dir["agents"].as_array().map(|a| a.len()),
        Some(super::agents::AGENTS.len())
    );
}

#[tokio::test]
async fn forwarded_headers_only_with_trust_forward() {
    let fwd = [
        ("x-forwarded-host", "gw.example.com"),
        ("x-forwarded-proto", "https"),
    ];
    let plain = call(&app(), "GET", "/.well-known/agent-card.json", &fwd, None)
        .await
        .json();
    assert_eq!(
        plain["supportedInterfaces"][0]["url"],
        "http://agents.test:8080/a2a/echo"
    );
    let mut config = crate::config::Config::for_tests();
    config.trust_forward = true;
    let trusted = module_app_with_config(config, super::router);
    let c = call(&trusted, "GET", "/.well-known/agent-card.json", &fwd, None)
        .await
        .json();
    assert_eq!(
        c["supportedInterfaces"][0]["url"],
        "https://gw.example.com/a2a/echo"
    );
}

#[tokio::test]
async fn cards_use_https_on_the_tls_listener() {
    let app = app();
    let get = |uri: &'static str| {
        let mut req = Request::builder()
            .uri(uri)
            .header("host", "localhost:8443")
            .body(Body::empty())
            .expect("request");
        req.extensions_mut()
            .insert(crate::session::ListenerInfo::https(8443));
        app.clone().oneshot(req)
    };
    let card = body_json(get("/.well-known/agent-card.json").await.expect("resp")).await;
    assert_eq!(
        card["supportedInterfaces"][0]["url"],
        "https://localhost:8443/a2a/echo"
    );
    assert_eq!(card["url"], "https://localhost:8443/a2a/echo");
    let legacy = body_json(
        get("/a2a/weather/.well-known/agent.json")
            .await
            .expect("resp"),
    )
    .await;
    assert_eq!(legacy["url"], "https://localhost:8443/a2a/weather");
    let agent = body_json(
        get("/a2a/travel-planner/.well-known/agent-card.json")
            .await
            .expect("resp"),
    )
    .await;
    assert!(agent
        .to_string()
        .contains("https://localhost:8443/a2a/travel-planner"));
    assert!(!agent.to_string().contains("http://localhost"));
    let dir = body_json(get("/a2a").await.expect("resp")).await;
    assert!(dir.to_string().contains("https://localhost:8443/a2a/echo"));
}

// ── JSON-RPC: send, versions, errors ───────────────────────────────

#[tokio::test]
async fn send_message_both_versions() {
    let app = app();
    let r = rpc(
        &app,
        "echo",
        V1,
        "SendMessage",
        json!({"message": v1_msg("hello")}),
    )
    .await;
    assert_eq!(
        r.headers.get("a2a-version").and_then(|v| v.to_str().ok()),
        Some("1.0")
    );
    let v = r.json();
    assert_eq!(v["id"], 7);
    let task = &v["result"]["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_COMPLETED");
    assert_eq!(task["artifacts"][0]["parts"][0]["text"], "hello");
    assert_eq!(task["history"][0]["role"], "ROLE_USER");
    assert!(task.get("kind").is_none());

    let r = rpc(
        &app,
        "echo",
        V03,
        "message/send",
        json!({"message": v03_msg("hello")}),
    )
    .await;
    let task = &r.json()["result"];
    assert_eq!(task["kind"], "task");
    assert_eq!(task["status"]["state"], "completed");
    assert_eq!(task["artifacts"][0]["parts"][0]["kind"], "text");
    assert_eq!(task["history"][0]["role"], "user");

    // Default agent alias.
    let r = rpc(&app, "", V1, "SendMessage", json!({"message": v1_msg("x")})).await;
    assert_eq!(
        r.json()["result"]["task"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );

    // Direct message replies.
    let r = rpc(
        &app,
        "echo",
        V1,
        "SendMessage",
        json!({"message": v1_msg("msg: hi")}),
    )
    .await;
    assert_eq!(r.json()["result"]["message"]["role"], "ROLE_AGENT");
    let r = rpc(
        &app,
        "echo",
        V03,
        "message/send",
        json!({"message": v03_msg("msg: hi")}),
    )
    .await;
    assert_eq!(r.json()["result"]["kind"], "message");
}

#[tokio::test]
async fn each_agent_reaches_its_state() {
    let app = app();
    let cases = [
        ("weather", "weather in Oslo", "TASK_STATE_COMPLETED"),
        ("travel-planner", "trip to Rome", "TASK_STATE_COMPLETED"),
        ("approval", "approve my lunch", "TASK_STATE_INPUT_REQUIRED"),
        ("flaky", "anything", "TASK_STATE_FAILED"),
        ("flaky", "please succeed", "TASK_STATE_COMPLETED"),
        ("secure", "who am I", "TASK_STATE_AUTH_REQUIRED"),
        ("reject", "anything", "TASK_STATE_REJECTED"),
    ];
    for (agent, text, state) in cases {
        let r = rpc(
            &app,
            agent,
            V1,
            "SendMessage",
            json!({"message": v1_msg(text)}),
        )
        .await
        .json();
        assert_eq!(
            r["result"]["task"]["status"]["state"], state,
            "{agent}: {r}"
        );
    }
    let r = rpc(
        &app,
        "weather",
        V1,
        "SendMessage",
        json!({"message": v1_msg("weather in Oslo")}),
    )
    .await
    .json();
    let parts = &r["result"]["task"]["artifacts"][0]["parts"];
    assert_eq!(parts[0]["data"]["city"], "Oslo");
    assert_eq!(parts[0]["mediaType"], "application/json");
    let r = rpc(
        &app,
        "travel-planner",
        V1,
        "SendMessage",
        json!({"message": v1_msg("trip to Rome")}),
    )
    .await
    .json();
    let arts = r["result"]["task"]["artifacts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let all: Vec<&Value> = arts
        .iter()
        .flat_map(|a| a["parts"].as_array().into_iter().flatten())
        .collect();
    assert!(all.iter().any(|p| p
        .get("url")
        .is_some_and(|u| u == "http://agents.test:8080/image/png")));
    assert!(all.iter().any(|p| p.get("raw").is_some()));
    assert!(all.iter().any(|p| p.get("data").is_some()));
    let itinerary = arts
        .iter()
        .find(|a| a["artifactId"] == "itinerary")
        .expect("itinerary");
    assert!(
        itinerary["parts"].as_array().map(|p| p.len()).unwrap_or(0) > 2,
        "chunks appended"
    );
}

#[tokio::test]
async fn version_negotiation_errors() {
    let app = app();
    let r = rpc(
        &app,
        "echo",
        V03,
        "SendMessage",
        json!({"message": v1_msg("x")}),
    )
    .await
    .json();
    assert_eq!(r["error"]["code"], -32009);
    assert_eq!(r["error"]["data"][0]["reason"], "VERSION_NOT_SUPPORTED");
    let r = rpc(
        &app,
        "echo",
        V1,
        "message/send",
        json!({"message": v03_msg("x")}),
    )
    .await
    .json();
    assert_eq!(r["error"]["code"], -32009);
    let r = rpc(
        &app,
        "echo",
        &[("a2a-version", "2.0")],
        "SendMessage",
        json!({"message": v1_msg("x")}),
    )
    .await
    .json();
    assert_eq!(r["error"]["code"], -32009);
    let r = rpc(
        &app,
        "echo",
        &[("a2a-version", "0.3")],
        "message/send",
        json!({"message": v03_msg("x")}),
    )
    .await
    .json();
    assert_eq!(r["result"]["status"]["state"], "completed");
    // Query parameter instead of the header.
    let r = call(&app, "POST", "/a2a/echo?A2A-Version=1.0", &[],
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "SendMessage", "params": {"message": v1_msg("q")}}))).await.json();
    assert_eq!(
        r["result"]["task"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );
    // REST: unsupported version is a 400 google.rpc.Status.
    let r = call(
        &app,
        "GET",
        "/a2a/echo/v1/tasks",
        &[("a2a-version", "9.9")],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        r.json()["error"]["details"][0]["reason"],
        "VERSION_NOT_SUPPORTED"
    );
}

#[tokio::test]
async fn jsonrpc_protocol_errors() {
    let app = app();
    let raw = |body: &'static str| {
        let app = app.clone();
        async move {
            let req = Request::builder()
                .method("POST")
                .uri("/a2a/echo")
                .header("a2a-version", "1.0")
                .body(Body::from(body))
                .expect("req");
            body_json(app.oneshot(req).await.expect("resp")).await
        }
    };
    assert_eq!(raw("{not json").await["error"]["code"], -32700);
    assert_eq!(raw("[]").await["error"]["code"], -32600);
    assert_eq!(
        raw(r#"{"id":1,"method":"GetTask"}"#).await["error"]["code"],
        -32600
    );
    assert_eq!(
        raw(r#"{"jsonrpc":"2.0","id":1}"#).await["error"]["code"],
        -32600
    );
    assert_eq!(
        raw(r#"{"jsonrpc":"2.0","id":{"x":1},"method":"GetTask"}"#).await["error"]["code"],
        -32600
    );
    let r = raw(r#"{"jsonrpc":"2.0","id":"a","method":"tasks/list"}"#).await;
    assert_eq!(r["error"]["code"], -32601);
    assert_eq!(r["id"], "a");
    assert_eq!(
        raw(r#"{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{}}"#).await["error"]
            ["code"],
        -32602
    );
    assert_eq!(
        raw(r#"{"jsonrpc":"2.0","id":1,"method":"GetTask","params":[]}"#).await["error"]["code"],
        -32602
    );
    let r = raw(r#"{"jsonrpc":"2.0","id":1,"method":"GetTask","params":{"id":"missing"}}"#).await;
    assert_eq!(r["error"]["code"], -32001);
    assert_eq!(
        r["error"]["data"][0]["@type"],
        "type.googleapis.com/google.rpc.ErrorInfo"
    );
    assert_eq!(r["error"]["data"][0]["metadata"]["taskId"], "missing");
    let r = rpc(&app, "echo", V03, "tasks/get", json!({"id": "missing"}))
        .await
        .json();
    assert_eq!(r["error"]["code"], -32001);
    // Unknown agent.
    let r = rpc(&app, "nobody", V1, "GetTask", json!({"id": "x"})).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn content_type_negotiation() {
    let app = app();
    let msg = json!({"messageId": "m", "role": "ROLE_USER", "parts": [{"raw": "aGk=", "mediaType": "image/png"}]});
    let r = rpc(&app, "weather", V1, "SendMessage", json!({"message": msg}))
        .await
        .json();
    assert_eq!(r["error"]["code"], -32005);
    let r = rpc(
        &app,
        "weather",
        V1,
        "SendMessage",
        json!({"message": v1_msg("x"), "configuration": {"acceptedOutputModes": ["image/gif"]}}),
    )
    .await
    .json();
    assert_eq!(r["error"]["code"], -32005);
    let r = rpc(&app, "weather", V1, "SendMessage",
        json!({"message": v1_msg("x"), "configuration": {"acceptedOutputModes": ["application/*"]}})).await.json();
    assert_eq!(
        r["result"]["task"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );
    // Echo accepts anything and returns files back.
    let r = rpc(&app, "echo", V03, "message/send", json!({"message": {"kind": "message", "messageId": "f", "role": "user",
        "parts": [{"kind": "file", "file": {"bytes": "aGk=", "mimeType": "image/png", "name": "a.png"}}, {"kind": "data", "data": {"a": 1}}]}})).await.json();
    let parts = &r["result"]["artifacts"][0]["parts"];
    assert_eq!(parts[0]["file"]["bytes"], "aGk=");
    assert_eq!(parts[1]["data"]["a"], 1);
}

// ── Streaming and subscriptions ─────────────────────────────────────

#[tokio::test]
async fn streaming_both_versions() {
    let app = app();
    let r = rpc(
        &app,
        "travel-planner",
        V1,
        "SendStreamingMessage",
        json!({"message": v1_msg("2 day trip to Kyoto")}),
    )
    .await;
    assert!(r
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .starts_with("text/event-stream"));
    let ev = r.events();
    assert!(ev.len() > 5);
    assert_eq!(
        ev[0]["result"]["task"]["status"]["state"],
        "TASK_STATE_SUBMITTED"
    );
    assert!(ev
        .iter()
        .any(|e| e["result"]["statusUpdate"]["status"]["state"] == "TASK_STATE_WORKING"));
    assert!(ev
        .iter()
        .any(|e| e["result"]["artifactUpdate"]["append"] == true));
    assert!(ev
        .iter()
        .any(|e| e["result"]["artifactUpdate"]["lastChunk"] == true));
    assert_eq!(
        ev.last()
            .map(|e| e["result"]["statusUpdate"]["status"]["state"].clone()),
        Some(json!("TASK_STATE_COMPLETED"))
    );

    let r = rpc(
        &app,
        "travel-planner",
        V03,
        "message/stream",
        json!({"message": v03_msg("trip to Kyoto")}),
    )
    .await;
    let ev = r.events();
    assert_eq!(ev[0]["result"]["kind"], "task");
    assert!(ev.iter().any(|e| e["result"]["kind"] == "artifact-update"));
    let last = &ev[ev.len() - 1]["result"];
    assert_eq!(last["kind"], "status-update");
    assert_eq!(last["final"], true);
    assert_eq!(last["status"]["state"], "completed");

    // Interrupted state closes the stream too.
    let r = rpc(
        &app,
        "approval",
        V1,
        "SendStreamingMessage",
        json!({"message": v1_msg("approve this")}),
    )
    .await;
    let ev = r.events();
    assert_eq!(
        ev.last()
            .map(|e| e["result"]["statusUpdate"]["status"]["state"].clone()),
        Some(json!("TASK_STATE_INPUT_REQUIRED"))
    );

    // Direct message: exactly one event.
    let r = rpc(
        &app,
        "echo",
        V1,
        "SendStreamingMessage",
        json!({"message": v1_msg("msg: one")}),
    )
    .await;
    let ev = r.events();
    assert_eq!(ev.len(), 1);
    assert!(ev[0]["result"]["message"].is_object());
}

fn slow_v1(text: &str, delay: u64) -> Value {
    json!({"messageId": uuid::Uuid::new_v4().to_string(), "role": "ROLE_USER", "parts": [{"text": text}], "metadata": {"stepDelayMs": delay}})
}

#[tokio::test]
async fn cancel_and_subscribe() {
    let app = app();
    // Running task: returnImmediately, then cancel.
    let r = rpc(&app, "travel-planner", V1, "SendMessage",
        json!({"message": slow_v1("trip to Oslo", 200), "configuration": {"returnImmediately": true}})).await.json();
    let task = &r["result"]["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_SUBMITTED");
    let id = task["id"].as_str().expect("id").to_string();
    let r = rpc(&app, "travel-planner", V1, "CancelTask", json!({"id": id}))
        .await
        .json();
    assert_eq!(r["result"]["status"]["state"], "TASK_STATE_CANCELED");
    let r = rpc(&app, "travel-planner", V1, "CancelTask", json!({"id": id}))
        .await
        .json();
    assert_eq!(r["error"]["code"], -32002, "not cancelable once canceled");
    let r = rpc(
        &app,
        "travel-planner",
        V1,
        "SubscribeToTask",
        json!({"id": id}),
    )
    .await
    .json();
    assert_eq!(
        r["error"]["code"], -32004,
        "terminal tasks cannot be subscribed"
    );

    // v0.3 cancel of a completed task.
    let r = rpc(
        &app,
        "echo",
        V03,
        "message/send",
        json!({"message": v03_msg("x")}),
    )
    .await
    .json();
    let id = r["result"]["id"].as_str().expect("id").to_string();
    let r = rpc(&app, "echo", V03, "tasks/cancel", json!({"id": id}))
        .await
        .json();
    assert_eq!(r["error"]["code"], -32002);

    // Subscribe to a running task (v1 and v0.3).
    for (headers, send, sub) in [
        (V1, "SendMessage", "SubscribeToTask"),
        (V03, "message/send", "tasks/resubscribe"),
    ] {
        let params = if headers.is_empty() {
            json!({"message": {"kind": "message", "messageId": uuid::Uuid::new_v4().to_string(), "role": "user",
                "parts": [{"kind": "text", "text": "trip to Bern"}], "metadata": {"stepDelayMs": 20}},
                "configuration": {"blocking": false}})
        } else {
            json!({"message": slow_v1("trip to Bern", 20), "configuration": {"returnImmediately": true}})
        };
        let r = rpc(&app, "travel-planner", headers, send, params)
            .await
            .json();
        let id = r["result"]["task"]["id"]
            .as_str()
            .or(r["result"]["id"].as_str())
            .expect("id")
            .to_string();
        let ev = rpc(&app, "travel-planner", headers, sub, json!({"id": id}))
            .await
            .events();
        assert!(ev.len() >= 2, "{sub}: {ev:?}");
        let first = &ev[0]["result"];
        assert!(first["task"]["id"] == id.as_str() || first["id"] == id.as_str());
        let last = &ev[ev.len() - 1]["result"];
        let state = last["statusUpdate"]["status"]["state"]
            .as_str()
            .or(last["status"]["state"].as_str());
        assert!(
            matches!(state, Some("TASK_STATE_COMPLETED") | Some("completed")),
            "{sub}: {last}"
        );
    }
}

// ── Tasks: get, list, history, scoping, contexts ────────────────────

#[tokio::test]
async fn get_and_list_tasks() {
    let app = app();
    let me = with_session(V1, "list-me");
    let mut ctx = String::new();
    for i in 0..3 {
        let mut m = v1_msg(&format!("n{i}"));
        if !ctx.is_empty() {
            m["contextId"] = json!(ctx);
        }
        let r = rpc(&app, "echo", &me, "SendMessage", json!({"message": m}))
            .await
            .json();
        ctx = r["result"]["task"]["contextId"]
            .as_str()
            .expect("ctx")
            .to_string();
        let status_text = r["result"]["task"]["status"]["message"]["parts"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        assert!(
            status_text.contains(&format!("turn {}", i + 1)),
            "{status_text}"
        );
    }
    let r = rpc(
        &app,
        "reject",
        &me,
        "SendMessage",
        json!({"message": v1_msg("x")}),
    )
    .await
    .json();
    let reject_id = r["result"]["task"]["id"].as_str().expect("id").to_string();

    let r = rpc(&app, "echo", &me, "ListTasks", json!({})).await.json();
    assert_eq!(r["result"]["totalSize"], 3, "only this agent's tasks: {r}");
    assert_eq!(r["result"]["nextPageToken"], "");
    assert!(
        r["result"]["tasks"][0].get("artifacts").is_none(),
        "artifacts omitted by default"
    );
    let r = rpc(
        &app,
        "echo",
        &me,
        "ListTasks",
        json!({"pageSize": 2, "includeArtifacts": true, "historyLength": 0}),
    )
    .await
    .json();
    assert_eq!(r["result"]["tasks"].as_array().map(|t| t.len()), Some(2));
    assert!(r["result"]["tasks"][0]["artifacts"].is_array());
    assert!(r["result"]["tasks"][0].get("history").is_none());
    let next = r["result"]["nextPageToken"]
        .as_str()
        .expect("token")
        .to_string();
    assert!(!next.is_empty());
    let r = rpc(
        &app,
        "echo",
        &me,
        "ListTasks",
        json!({"pageSize": 2, "pageToken": next}),
    )
    .await
    .json();
    assert_eq!(r["result"]["tasks"].as_array().map(|t| t.len()), Some(1));
    let r = rpc(
        &app,
        "echo",
        &me,
        "ListTasks",
        json!({"contextId": ctx, "status": "TASK_STATE_COMPLETED"}),
    )
    .await
    .json();
    assert_eq!(r["result"]["totalSize"], 3);
    let r = rpc(
        &app,
        "echo",
        &me,
        "ListTasks",
        json!({"status": "TASK_STATE_WORKING"}),
    )
    .await
    .json();
    assert_eq!(r["result"]["totalSize"], 0);
    let r = rpc(
        &app,
        "echo",
        &me,
        "ListTasks",
        json!({"statusTimestampAfter": "2000-01-01T00:00:00Z"}),
    )
    .await
    .json();
    assert_eq!(r["result"]["totalSize"], 3);
    for bad in [
        json!({"pageSize": 150}),
        json!({"historyLength": -5}),
        json!({"status": "TASK_STATE_RUNNING"}),
        json!({"pageToken": "%%%"}),
    ] {
        let r = rpc(&app, "echo", &me, "ListTasks", bad.clone())
            .await
            .json();
        assert_eq!(r["error"]["code"], -32602, "{bad}");
    }
    // Another session sees nothing.
    let other = with_session(V1, "list-other");
    let r = rpc(&app, "echo", &other, "ListTasks", json!({}))
        .await
        .json();
    assert_eq!(r["result"]["totalSize"], 0);
    let r = rpc(&app, "reject", &other, "GetTask", json!({"id": reject_id}))
        .await
        .json();
    assert_eq!(r["error"]["code"], -32001);
    // History length on GetTask.
    let r = rpc(
        &app,
        "reject",
        &me,
        "GetTask",
        json!({"id": reject_id, "historyLength": 0}),
    )
    .await
    .json();
    assert!(r["result"].get("history").is_none());
    let r = rpc(&app, "reject", &me, "GetTask", json!({"id": reject_id}))
        .await
        .json();
    assert_eq!(r["result"]["history"].as_array().map(|h| h.len()), Some(1));
    let r = rpc(
        &app,
        "reject",
        V03,
        "tasks/get",
        json!({"id": reject_id, "historyLength": 1}),
    )
    .await
    .json();
    assert_eq!(
        r["error"]["code"], -32001,
        "v0.3 call without the session header is another client"
    );
    let me03 = with_session(V03, "list-me");
    let r = rpc(
        &app,
        "reject",
        &me03,
        "tasks/get",
        json!({"id": reject_id, "historyLength": 1}),
    )
    .await
    .json();
    assert_eq!(r["result"]["status"]["state"], "rejected");
}

#[tokio::test]
async fn input_required_continuation() {
    let app = app();
    // v1.0
    let r = rpc(
        &app,
        "approval",
        V1,
        "SendMessage",
        json!({"message": v1_msg("Approve $40 lunch")}),
    )
    .await
    .json();
    let task = &r["result"]["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_INPUT_REQUIRED");
    let (id, ctx) = (
        task["id"].as_str().expect("id").to_string(),
        task["contextId"].as_str().expect("ctx").to_string(),
    );
    let mut m = v1_msg("hmm?");
    m["taskId"] = json!(id);
    let r = rpc(&app, "approval", V1, "SendMessage", json!({"message": m}))
        .await
        .json();
    assert_eq!(
        r["result"]["task"]["status"]["state"], "TASK_STATE_INPUT_REQUIRED",
        "re-prompt"
    );
    let mut m = v1_msg("approve");
    m["taskId"] = json!(id);
    m["contextId"] = json!("other-context");
    let r = rpc(&app, "approval", V1, "SendMessage", json!({"message": m}))
        .await
        .json();
    assert_eq!(r["error"]["code"], -32602, "mismatched contextId");
    let mut m = v1_msg("approve");
    m["taskId"] = json!(id);
    m["contextId"] = json!(ctx);
    let r = rpc(&app, "approval", V1, "SendMessage", json!({"message": m}))
        .await
        .json();
    let task = &r["result"]["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_COMPLETED");
    assert_eq!(task["artifacts"][0]["parts"][0]["data"]["approved"], true);
    assert!(task["history"].as_array().map(|h| h.len()).unwrap_or(0) >= 5);
    let mut m = v1_msg("again");
    m["taskId"] = json!(id);
    let r = rpc(&app, "approval", V1, "SendMessage", json!({"message": m}))
        .await
        .json();
    assert_eq!(
        r["error"]["code"], -32004,
        "terminal tasks accept no messages"
    );
    let mut m = v1_msg("x");
    m["taskId"] = json!("does-not-exist");
    let r = rpc(&app, "approval", V1, "SendMessage", json!({"message": m}))
        .await
        .json();
    assert_eq!(r["error"]["code"], -32001);

    // v0.3
    let r = rpc(
        &app,
        "approval",
        V03,
        "message/send",
        json!({"message": v03_msg("Approve $40 lunch")}),
    )
    .await
    .json();
    assert_eq!(r["result"]["status"]["state"], "input-required");
    let id = r["result"]["id"].as_str().expect("id").to_string();
    let mut m = v03_msg("deny");
    m["taskId"] = json!(id);
    let r = rpc(&app, "approval", V03, "message/send", json!({"message": m}))
        .await
        .json();
    assert_eq!(r["result"]["status"]["state"], "completed");
    assert_eq!(
        r["result"]["artifacts"][0]["parts"][0]["data"]["approved"],
        false
    );
}

// ── Auth ────────────────────────────────────────────────────────────

#[tokio::test]
async fn auth_required_and_extended_card() {
    let app = app();
    let tok = valid_token();
    let bearer = format!("Bearer {tok}");
    let authed: Vec<(&str, &str)> =
        vec![("a2a-version", "1.0"), ("authorization", bearer.as_str())];

    let r = rpc(
        &app,
        "secure",
        V1,
        "SendMessage",
        json!({"message": v1_msg("who am I")}),
    )
    .await
    .json();
    let task = &r["result"]["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_AUTH_REQUIRED");
    let id = task["id"].as_str().expect("id").to_string();
    let r = rpc(
        &app,
        "secure",
        &authed,
        "SendMessage",
        json!({"message": v1_msg("who am I")}),
    )
    .await
    .json();
    assert_eq!(
        r["result"]["task"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );
    assert_eq!(
        r["result"]["task"]["artifacts"][0]["parts"][0]["data"]["sub"],
        "demo"
    );
    // Continue the auth-required task with a token.
    let mut m = v1_msg("here is my token");
    m["taskId"] = json!(id);
    let r = rpc(
        &app,
        "secure",
        &authed,
        "SendMessage",
        json!({"message": m}),
    )
    .await
    .json();
    assert_eq!(
        r["result"]["task"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );
    // Invalid and expired tokens.
    let expired = token(json!({"sub": "x", "exp": 1000}));
    let bad = format!("Bearer {expired}");
    let r = rpc(
        &app,
        "secure",
        &[("a2a-version", "1.0"), ("authorization", bad.as_str())],
        "SendMessage",
        json!({"message": v1_msg("x")}),
    )
    .await
    .json();
    let text = r["result"]["task"]["status"]["message"]["parts"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(text.contains("rejected"), "{text}");
    let r = rpc(
        &app,
        "secure",
        &[("a2a-version", "1.0"), ("authorization", "Bearer nope")],
        "SendMessage",
        json!({"message": v1_msg("x")}),
    )
    .await
    .json();
    assert_eq!(
        r["result"]["task"]["status"]["state"],
        "TASK_STATE_AUTH_REQUIRED"
    );

    // Extended card.
    let r = rpc(&app, "secure", V1, "GetExtendedAgentCard", json!({})).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.headers.get("www-authenticate").is_some());
    assert_eq!(r.json()["error"]["code"], -32000);
    let r = rpc(&app, "secure", &authed, "GetExtendedAgentCard", json!({}))
        .await
        .json();
    let skills: Vec<&str> = r["result"]["skills"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["id"].as_str())
        .collect();
    assert!(skills.contains(&"audit-log"), "{skills:?}");
    assert!(r["result"].get("url").is_none(), "pure v1.0 card");
    let r = rpc(&app, "echo", V1, "GetExtendedAgentCard", json!({}))
        .await
        .json();
    assert_eq!(r["error"]["code"], -32004);
    let v03_auth: Vec<(&str, &str)> = vec![("authorization", bearer.as_str())];
    let r = rpc(
        &app,
        "secure",
        &v03_auth,
        "agent/getAuthenticatedExtendedCard",
        json!({}),
    )
    .await
    .json();
    assert_eq!(r["result"]["protocolVersion"], "0.3.0");
    assert_eq!(r["result"]["securitySchemes"]["bearer"]["type"], "http");
    let r = rpc(
        &app,
        "secure",
        V03,
        "message/send",
        json!({"message": v03_msg("x")}),
    )
    .await
    .json();
    assert_eq!(r["result"]["status"]["state"], "auth-required");
}

// ── Push notifications ──────────────────────────────────────────────

#[tokio::test]
async fn push_config_crud_ssrf_and_delivery() {
    let app = app();
    let r = rpc(&app, "travel-planner", V1, "SendMessage",
        json!({"message": slow_v1("trip to Nice", 30), "configuration": {"returnImmediately": true,
            "taskPushNotificationConfig": {"url": "http://agents.test:8080/a2a/webhook-sink/unit-sink",
                "token": "tok", "authentication": {"scheme": "Bearer", "credentials": "s3cret"}}}})).await.json();
    let id = r["result"]["task"]["id"].as_str().expect("id").to_string();

    // CRUD (v1.0).
    let r = rpc(
        &app,
        "travel-planner",
        V1,
        "CreateTaskPushNotificationConfig",
        json!({"taskId": id, "id": "second", "url": "/a2a/webhook-sink/unit-sink-2"}),
    )
    .await
    .json();
    assert_eq!(r["result"]["id"], "second");
    assert_eq!(r["result"]["taskId"], id.as_str());
    let r = rpc(
        &app,
        "travel-planner",
        V1,
        "GetTaskPushNotificationConfig",
        json!({"taskId": id, "id": "second"}),
    )
    .await
    .json();
    assert_eq!(r["result"]["url"], "/a2a/webhook-sink/unit-sink-2");
    let r = rpc(
        &app,
        "travel-planner",
        V1,
        "ListTaskPushNotificationConfigs",
        json!({"taskId": id}),
    )
    .await
    .json();
    assert_eq!(r["result"]["configs"].as_array().map(|c| c.len()), Some(2));
    let r = rpc(
        &app,
        "travel-planner",
        V1,
        "DeleteTaskPushNotificationConfig",
        json!({"taskId": id, "id": "second"}),
    )
    .await
    .json();
    assert_eq!(r["result"], Value::Null);
    let r = rpc(
        &app,
        "travel-planner",
        V1,
        "DeleteTaskPushNotificationConfig",
        json!({"taskId": id, "id": "second"}),
    )
    .await
    .json();
    assert!(r.get("error").is_none(), "delete is idempotent");
    let r = rpc(
        &app,
        "travel-planner",
        V1,
        "GetTaskPushNotificationConfig",
        json!({"taskId": id, "id": "second"}),
    )
    .await
    .json();
    assert_eq!(r["error"]["code"], -32001);

    // SSRF: rejected targets.
    for url in [
        "http://169.254.169.254/latest/meta-data",
        "http://127.0.0.1:22/",
        "http://localhost/a2a/webhook-sink/x",
        "file:///etc/passwd",
        "https://example.com/hook",
        "/etc/passwd",
    ] {
        let r = rpc(
            &app,
            "travel-planner",
            V1,
            "CreateTaskPushNotificationConfig",
            json!({"taskId": id, "url": url}),
        )
        .await
        .json();
        assert_eq!(r["error"]["code"], -32602, "{url}");
    }
    let r = rpc(&app, "echo", V1, "SendMessage",
        json!({"message": v1_msg("x"), "configuration": {"taskPushNotificationConfig": {"url": "http://10.0.0.5/hook"}}})).await.json();
    assert_eq!(r["error"]["code"], -32602);

    // Wait for completion, then inspect the in-process deliveries.
    let mut done = false;
    for _ in 0..200 {
        let r = rpc(&app, "travel-planner", V1, "GetTask", json!({"id": id}))
            .await
            .json();
        if r["result"]["status"]["state"] == "TASK_STATE_COMPLETED" {
            done = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(done);
    let sink = call(&app, "GET", "/a2a/webhook-sink/unit-sink", &[], None)
        .await
        .json();
    let notes = sink["notifications"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(notes.len() > 3);
    assert_eq!(notes[0]["headers"]["authorization"], "Bearer s3cret");
    assert_eq!(notes[0]["headers"]["x-a2a-notification-token"], "tok");
    assert_eq!(notes[0]["headers"]["content-type"], "application/a2a+json");
    assert!(notes
        .iter()
        .any(|n| n["body"]["statusUpdate"]["status"]["state"] == "TASK_STATE_COMPLETED"));
    assert!(notes
        .iter()
        .any(|n| n["body"]["artifactUpdate"].is_object()));

    // v0.3 CRUD and Task payloads.
    let r = rpc(
        &app,
        "approval",
        V03,
        "message/send",
        json!({"message": v03_msg("approve?")}),
    )
    .await
    .json();
    let id = r["result"]["id"].as_str().expect("id").to_string();
    let r = rpc(&app, "approval", V03, "tasks/pushNotificationConfig/set",
        json!({"taskId": id, "pushNotificationConfig": {"url": "/a2a/webhook-sink/unit-v03", "authentication": {"schemes": ["Basic"], "credentials": "dXNlcjpwdw=="}}})).await.json();
    let cid = r["result"]["pushNotificationConfig"]["id"]
        .as_str()
        .expect("cid")
        .to_string();
    assert_eq!(r["result"]["taskId"], id.as_str());
    let r = rpc(
        &app,
        "approval",
        V03,
        "tasks/pushNotificationConfig/get",
        json!({"id": id}),
    )
    .await
    .json();
    assert_eq!(r["result"]["pushNotificationConfig"]["id"], cid.as_str());
    let r = rpc(
        &app,
        "approval",
        V03,
        "tasks/pushNotificationConfig/get",
        json!({"id": id, "pushNotificationConfigId": cid}),
    )
    .await
    .json();
    assert_eq!(
        r["result"]["pushNotificationConfig"]["authentication"]["schemes"][0],
        "Basic"
    );
    let r = rpc(
        &app,
        "approval",
        V03,
        "tasks/pushNotificationConfig/list",
        json!({"id": id}),
    )
    .await
    .json();
    assert_eq!(r["result"].as_array().map(|c| c.len()), Some(1));
    let mut m = v03_msg("approve");
    m["taskId"] = json!(id);
    let r = rpc(&app, "approval", V03, "message/send", json!({"message": m}))
        .await
        .json();
    assert_eq!(r["result"]["status"]["state"], "completed");
    let sink = call(&app, "GET", "/a2a/webhook-sink/unit-v03", &[], None)
        .await
        .json();
    let notes = sink["notifications"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(!notes.is_empty());
    assert_eq!(notes[0]["headers"]["authorization"], "Basic dXNlcjpwdw==");
    assert_eq!(
        notes.last().map(|n| n["body"]["kind"].clone()),
        Some(json!("task"))
    );
    assert_eq!(
        notes.last().map(|n| n["body"]["status"]["state"].clone()),
        Some(json!("completed"))
    );
    let r = rpc(
        &app,
        "approval",
        V03,
        "tasks/pushNotificationConfig/delete",
        json!({"id": id, "pushNotificationConfigId": cid}),
    )
    .await
    .json();
    assert_eq!(r["result"], Value::Null);
    let r = rpc(
        &app,
        "approval",
        V03,
        "tasks/pushNotificationConfig/set",
        json!({"taskId": id, "pushNotificationConfig": {"url": "http://192.168.1.1/x"}}),
    )
    .await
    .json();
    assert_eq!(r["error"]["code"], -32602);
}

#[tokio::test]
async fn webhook_sink_endpoints() {
    let app = app();
    let r = call(
        &app,
        "POST",
        "/a2a/webhook-sink/s1",
        &[("authorization", "Bearer x")],
        Some(json!({"a": 1})),
    )
    .await
    .json();
    assert_eq!(r["count"], 1);
    let r = call(&app, "GET", "/a2a/webhook-sink/s1", &[], None)
        .await
        .json();
    assert_eq!(r["notifications"][0]["body"]["a"], 1);
    assert_eq!(
        r["notifications"][0]["headers"]["authorization"],
        "Bearer x"
    );
    let r = call(&app, "DELETE", "/a2a/webhook-sink/s1", &[], None)
        .await
        .json();
    assert_eq!(r["deleted"], true);
    assert_eq!(
        call(&app, "GET", "/a2a/webhook-sink/s1", &[], None)
            .await
            .json()["count"],
        0
    );
    let r = call(&app, "GET", "/a2a/webhook-sink/bad%20id", &[], None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

// ── HTTP+JSON binding ───────────────────────────────────────────────

#[tokio::test]
async fn rest_binding_v1() {
    let app = app();
    let send = json!({"message": v1_msg("hello rest")});
    let r = call(&app, "POST", "/a2a/echo/v1/message:send", V1, Some(send)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("application/a2a+json")
    );
    let task = r.json()["task"].clone();
    assert_eq!(task["status"]["state"], "TASK_STATE_COMPLETED");
    let id = task["id"].as_str().expect("id").to_string();
    let r = call(
        &app,
        "GET",
        &format!("/a2a/echo/v1/tasks/{id}?historyLength=0"),
        V1,
        None,
    )
    .await
    .json();
    assert_eq!(r["id"], id.as_str());
    assert!(r.get("history").is_none());
    let r = call(
        &app,
        "GET",
        "/a2a/echo/v1/tasks?pageSize=5&status=TASK_STATE_COMPLETED",
        V1,
        None,
    )
    .await
    .json();
    assert!(r["totalSize"].as_u64().unwrap_or(0) >= 1);
    let r = call(&app, "GET", "/a2a/echo/v1/tasks?pageSize=0", V1, None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["error"]["status"], "INVALID_ARGUMENT");

    let r = call(
        &app,
        "POST",
        &format!("/a2a/echo/v1/tasks/{id}:cancel"),
        V1,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        r.json()["error"]["details"][0]["reason"],
        "TASK_NOT_CANCELABLE"
    );
    let r = call(&app, "GET", "/a2a/echo/v1/tasks/unknown-id", V1, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let body = r.json();
    assert_eq!(body["error"]["code"], 404);
    assert_eq!(body["error"]["status"], "NOT_FOUND");
    assert_eq!(body["error"]["details"][0]["domain"], "a2a-protocol.org");

    // Stream + subscribe (GET and POST) + cancel of a running task.
    let r = call(
        &app,
        "POST",
        "/a2a/travel-planner/v1/message:stream",
        V1,
        Some(json!({"message": v1_msg("trip to Kyoto")})),
    )
    .await;
    let ev = r.events();
    assert!(ev[0]["task"].is_object());
    assert_eq!(
        ev.last()
            .map(|e| e["statusUpdate"]["status"]["state"].clone()),
        Some(json!("TASK_STATE_COMPLETED"))
    );
    for method in ["GET", "POST"] {
        let r = call(&app, "POST", "/a2a/travel-planner/v1/message:send", V1,
            Some(json!({"message": slow_v1("trip to Bern", 20), "configuration": {"returnImmediately": true}}))).await.json();
        let id = r["task"]["id"].as_str().expect("id").to_string();
        let ev = call(
            &app,
            method,
            &format!("/a2a/travel-planner/v1/tasks/{id}:subscribe"),
            V1,
            None,
        )
        .await
        .events();
        assert_eq!(ev[0]["task"]["id"], id.as_str());
        assert_eq!(
            ev.last()
                .map(|e| e["statusUpdate"]["status"]["state"].clone()),
            Some(json!("TASK_STATE_COMPLETED")),
            "{method}"
        );
    }
    let r = call(&app, "POST", "/a2a/travel-planner/v1/message:send", V1,
        Some(json!({"message": slow_v1("trip to Bern", 200), "configuration": {"returnImmediately": true}}))).await.json();
    let id = r["task"]["id"].as_str().expect("id").to_string();
    let r = call(
        &app,
        "POST",
        &format!("/a2a/travel-planner/v1/tasks/{id}:cancel"),
        V1,
        Some(json!({"id": id})),
    )
    .await
    .json();
    assert_eq!(r["status"]["state"], "TASK_STATE_CANCELED");

    // Push config CRUD.
    let base = format!("/a2a/travel-planner/v1/tasks/{id}/pushNotificationConfigs");
    let r = call(
        &app,
        "POST",
        &base,
        V1,
        Some(json!({"url": "/a2a/webhook-sink/rest-1", "id": "c1"})),
    )
    .await
    .json();
    assert_eq!(r["id"], "c1");
    let r = call(&app, "GET", &base, V1, None).await.json();
    assert_eq!(r["configs"][0]["id"], "c1");
    let r = call(&app, "GET", &format!("{base}/c1"), V1, None)
        .await
        .json();
    assert_eq!(r["url"], "/a2a/webhook-sink/rest-1");
    let r = call(&app, "DELETE", &format!("{base}/c1"), V1, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), json!({}));
    let r = call(
        &app,
        "POST",
        &base,
        V1,
        Some(json!({"url": "http://169.254.169.254/"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // Extended card, errors.
    let r = call(&app, "GET", "/a2a/secure/v1/extendedAgentCard", V1, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let bearer = format!("Bearer {}", valid_token());
    let r = call(
        &app,
        "GET",
        "/a2a/secure/v1/extendedAgentCard",
        &[("a2a-version", "1.0"), ("authorization", bearer.as_str())],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json()["supportedInterfaces"].is_array());
    let r = call(&app, "GET", "/a2a/echo/v1/extendedAgentCard", V1, None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(&app, "GET", "/a2a/echo/v1/message:send", V1, None).await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(r.headers.get("allow").is_some());
    let r = call(&app, "GET", "/a2a/echo/v1/nothing/here", V1, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(
        &app,
        "POST",
        "/a2a/echo/v1/message:send",
        V1,
        Some(json!({"message": {"role": "ROLE_USER"}})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn rest_binding_v03() {
    let app = app();
    // v0.3 ProtoJSON: snake_case accepted, `content` instead of `parts`,
    // file bytes double base64 encoded, blocking must be explicit.
    let body = json!({
        "request": {"message_id": "m1", "role": "ROLE_USER", "content": [
            {"text": "hello"},
            {"file": {"file_with_bytes": "YUdWc2JHOD0=", "mime_type": "text/plain", "name": "h.txt"}}
        ]},
        "configuration": {"blocking": true}
    });
    let r = call(&app, "POST", "/a2a/echo/v1/message:send", V03, Some(body)).await;
    assert_eq!(r.status, StatusCode::OK);
    let v = r.json();
    let task = &v["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_COMPLETED");
    assert_eq!(task["history"][0]["content"][0]["text"], "hello");
    assert_eq!(
        task["artifacts"][0]["parts"][1]["file"]["fileWithBytes"],
        "YUdWc2JHOD0="
    );
    let id = task["id"].as_str().expect("id").to_string();
    let r = call(
        &app,
        "POST",
        &format!("/a2a/echo/v1/tasks/{id}:cancel"),
        V03,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["type"], "TaskNotCancelableError");
    let r = call(&app, "GET", "/a2a/echo/v1/tasks", V03, None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "ListTasks is v1.0 only");
    // Without blocking: returns immediately (v0.3 proto default).
    let r = call(&app, "POST", "/a2a/travel-planner/v1/message:send", V03,
        Some(json!({"message": {"messageId": "m2", "role": "ROLE_USER", "content": [{"text": "trip"}], "metadata": {"stepDelayMs": 50}}}))).await.json();
    assert_eq!(r["task"]["status"]["state"], "TASK_STATE_SUBMITTED");
    let id = r["task"]["id"].as_str().expect("id").to_string();
    let r = call(
        &app,
        "POST",
        &format!("/a2a/travel-planner/v1/tasks/{id}:cancel"),
        V03,
        None,
    )
    .await
    .json();
    assert_eq!(r["status"]["state"], "TASK_STATE_CANCELLED");
    let r = call(&app, "POST", &format!("/a2a/travel-planner/v1/tasks/{id}/pushNotificationConfigs"), V03,
        Some(json!({"parent": format!("tasks/{id}"), "config_id": "p1", "config": {"push_notification_config": {"url": "/a2a/webhook-sink/v03rest"}}}))).await.json();
    assert_eq!(r["name"], format!("tasks/{id}/pushNotificationConfigs/p1"));
    let card = call(&app, "GET", "/a2a/secure/v1/card", V03, None)
        .await
        .json();
    assert_eq!(card["protocolVersion"], "0.3.0");
    assert!(card["securitySchemes"]["bearer"]["httpAuthSecurityScheme"].is_object());
    let ev = call(&app, "POST", "/a2a/weather/v1/message:stream", V03,
        Some(json!({"message": {"messageId": "m3", "role": "ROLE_USER", "content": [{"text": "weather in Rome"}], "metadata": {"stepDelayMs": 1}}}))).await.events();
    assert_eq!(ev[0]["task"]["status"]["state"], "TASK_STATE_SUBMITTED");
    let last = ev.last().cloned().unwrap_or_default();
    assert_eq!(last["statusUpdate"]["final"], true);
    assert_eq!(
        last["statusUpdate"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );
}

#[tokio::test]
async fn full_app_routes_a2a() {
    // Through the complete middleware stack.
    let app = crate::test_support::test_app();
    let r = call(
        &app,
        "POST",
        "/a2a/echo",
        V1,
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "SendMessage",
        "params": {"message": v1_msg("via full app")}})),
    )
    .await
    .json();
    assert_eq!(
        r["result"]["task"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );
    let r = call(&app, "GET", "/.well-known/agent-card.json", &[], None).await;
    assert_eq!(r.status, StatusCode::OK);
}
