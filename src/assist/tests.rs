use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::provider::{Event, Kind, Problem, SseParser};
use super::*;
use crate::limits::ManualClock;
use crate::test_support::{
    bearer_request, body_bytes, body_json, control_jwt, control_keys, json_request, test_app_with,
    test_state_with,
};

fn mock_config() -> Config {
    let mut c = Config::for_tests();
    c.assist_provider = Some("mock".into());
    c
}

fn state_with_mock() -> crate::state::AppState {
    test_state_with(mock_config())
}

async fn collect(stream: OutStream) -> Vec<Out> {
    stream.collect::<Vec<_>>().await
}

fn text_of(outs: &[Out]) -> String {
    outs.iter()
        .filter_map(|o| match o {
            Out::Delta(d) => Some(d.as_str()),
            _ => None,
        })
        .collect()
}

fn json_of(outs: &[Out]) -> Option<Value> {
    outs.iter().find_map(|o| match o {
        Out::Json(v) => Some(v.clone()),
        _ => None,
    })
}

fn error_of(outs: &[Out]) -> Option<String> {
    outs.iter().find_map(|o| match o {
        Out::Error { message, .. } => Some(message.clone()),
        _ => None,
    })
}

fn run_task(
    state: &crate::state::AppState,
    task: &str,
    inputs: Value,
) -> Result<OutStream, Refusal> {
    state.assist.run(RunParams {
        task: task::find(task).expect("task"),
        inputs,
        model: None,
        principal: "test".into(),
    })
}

// ── Pieces ──────────────────────────────────────────────────────────

#[test]
fn sse_parser_handles_split_chunks_and_line_endings() {
    let mut p = SseParser::default();
    let mut events = p.feed(b"event: ping\ndata: {\"a\":");
    assert!(events.is_empty());
    events.extend(p.feed(b" 1}\n\ndata: x\ndata: y\r\n\r\n: comment\n\n"));
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event, "ping");
    assert_eq!(events[0].data, "{\"a\": 1}");
    assert_eq!(events[1].data, "x\ny");
    // A multi-byte character split across chunks survives.
    let bytes = "data: caf\u{e9}\n\n".as_bytes();
    let mut p = SseParser::default();
    assert!(p.feed(&bytes[..8]).is_empty());
    let ev = p.feed(&bytes[8..]);
    assert_eq!(ev[0].data, "caf\u{e9}");
    // The last block without a blank line comes out of finish().
    let mut p = SseParser::default();
    assert!(p.feed(b"data: tail").is_empty());
    assert_eq!(p.finish().expect("tail").data, "tail");
    assert!(p.finish().is_none());
}

#[test]
fn provider_bodies_and_events() {
    let req = provider::Request {
        model: "m".into(),
        system: "sys".into(),
        user: "hello".into(),
        schema: Some(json!({"type": "object", "properties": {"a": {"type": "string"}}})),
        max_output_tokens: 100,
        mock_answer: None,
    };
    // Anthropic: tool use forces the structured answer.
    let b = anthropic::body(&req);
    assert_eq!(b["stream"], true);
    assert_eq!(b["system"], "sys");
    assert_eq!(b["tools"][0]["name"], "answer");
    assert_eq!(b["tool_choice"]["name"], "answer");
    assert_eq!(b["max_tokens"], 100);
    assert_eq!(
        anthropic::events(
            &json!({"type": "message_start", "message": {"usage": {"input_tokens": 12}}})
        ),
        vec![Event::Usage {
            input: 12,
            output: 0
        }]
    );
    assert_eq!(
        anthropic::events(
            &json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "hi"}})
        ),
        vec![Event::Delta("hi".into())]
    );
    assert_eq!(
        anthropic::events(
            &json!({"type": "content_block_delta", "delta": {"type": "input_json_delta", "partial_json": "{\"a\""}})
        ),
        vec![Event::Delta("{\"a\"".into())]
    );
    assert_eq!(
        anthropic::events(&json!({"type": "message_delta", "usage": {"output_tokens": 7}})),
        vec![Event::Usage {
            input: 0,
            output: 7
        }]
    );
    assert_eq!(
        anthropic::events(&json!({"type": "error", "error": {"message": "overloaded"}})),
        vec![Event::Error("overloaded".into())]
    );
    // OpenAI: json_schema on OpenAI, json_object plus the schema in the prompt on a compatible endpoint.
    let b = openai::body(&req, false);
    assert_eq!(b["response_format"]["type"], "json_schema");
    assert_eq!(b["max_completion_tokens"], 100);
    assert_eq!(b["messages"][0]["role"], "system");
    assert_eq!(b["messages"][1]["content"], "hello");
    let b = openai::body(&req, true);
    assert_eq!(b["response_format"]["type"], "json_object");
    assert_eq!(b["max_tokens"], 100);
    assert!(b["messages"][1]["content"]
        .as_str()
        .is_some_and(|c| c.contains("JSON Schema")));
    assert_eq!(
        openai::events(&json!({"choices": [{"delta": {"content": "ok"}}]})),
        vec![Event::Delta("ok".into())]
    );
    assert_eq!(
        openai::events(
            &json!({"choices": [], "usage": {"prompt_tokens": 3, "completion_tokens": 4}})
        ),
        vec![Event::Usage {
            input: 3,
            output: 4
        }]
    );
    assert_eq!(
        openai::events(&json!({"error": {"message": "bad key"}})),
        vec![Event::Error("bad key".into())]
    );
    // A stream cut by the output budget is an error, not a shorter answer.
    assert_eq!(
        openai::events(&json!({"choices": [{"delta": {}, "finish_reason": "length"}]})),
        vec![Event::Error(openai::RAN_OUT.into())]
    );
    assert_eq!(
        openai::events(&json!({"choices": [{"delta": {"content": "}"}, "finish_reason": "stop"}]})),
        vec![Event::Delta("}".into())]
    );
    // Reasoning models get a short reasoning_effort on OpenAI, never on a compatible endpoint.
    assert!(openai::is_reasoning_model("gpt-5-mini"));
    assert!(openai::is_reasoning_model("o3"));
    assert!(!openai::is_reasoning_model("gpt-4o"));
    let reasoning = Request {
        model: "gpt-5".into(),
        system: "s".into(),
        user: "hello".into(),
        schema: None,
        max_output_tokens: 100,
        mock_answer: None,
    };
    assert_eq!(openai::body(&reasoning, false)["reasoning_effort"], "low");
    assert!(openai::body(&reasoning, true)
        .get("reasoning_effort")
        .is_none());
    assert!(openai::body(&req, false).get("reasoning_effort").is_none());
    // Gemini: responseSchema without the keywords it rejects, key never in the URL.
    let b = gemini::body(&req);
    assert_eq!(
        b["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert_eq!(b["generationConfig"]["maxOutputTokens"], 100);
    assert_eq!(b["systemInstruction"]["parts"][0]["text"], "sys");
    let cleaned = gemini::response_schema(&json!({
        "type": "object", "additionalProperties": false, "title": "x",
        "properties": {"a": {"type": ["string", "null"], "maxLength": 3}, "b": {"type": "array", "items": {"type": "string", "pattern": "x"}}}
    }));
    assert!(cleaned.get("additionalProperties").is_none());
    assert!(cleaned.get("title").is_none());
    assert_eq!(cleaned["properties"]["a"]["type"], "string");
    assert_eq!(cleaned["properties"]["a"]["nullable"], true);
    assert!(cleaned["properties"]["a"].get("maxLength").is_none());
    assert!(cleaned["properties"]["b"]["items"].get("pattern").is_none());
    assert_eq!(
        gemini::events(
            &json!({"candidates": [{"content": {"parts": [{"text": "a"}, {"text": "b"}]}}], "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 2}})
        ),
        vec![
            Event::Delta("a".into()),
            Event::Delta("b".into()),
            Event::Usage {
                input: 5,
                output: 2
            }
        ]
    );
    assert_eq!(
        gemini::events(&json!({"promptFeedback": {"blockReason": "SAFETY"}})),
        vec![Event::Error("the prompt was blocked (SAFETY)".into())]
    );
    // Provider error bodies.
    assert_eq!(
        provider::provider_error(
            r#"{"error":{"message":"invalid x-api-key","type":"authentication_error"}}"#,
            401
        ),
        "the provider answered 401: invalid x-api-key"
    );
    assert_eq!(
        provider::provider_error("", 500),
        "the provider answered 500: HTTP 500"
    );
}

#[test]
fn model_table_shape() {
    for kind in [Kind::Anthropic, Kind::OpenAi, Kind::Gemini, Kind::Mock] {
        assert!(models::default_model(kind).is_some(), "{kind:?}");
        assert!(models::default_fast_model(kind).is_some(), "{kind:?}");
    }
    assert!(models::default_model(Kind::Compatible).is_none());
    assert_eq!(
        models::cost(Kind::OpenAi, "gpt-5", 1_000_000, 1_000_000),
        Some(11.25)
    );
    assert_eq!(models::cost(Kind::Mock, "anything", 10, 10), Some(0.0));
    assert_eq!(models::cost(Kind::Compatible, "llama", 10, 10), None);
    assert!(models::table()
        .as_array()
        .is_some_and(|t| t.len() == models::MODELS.len()));
    for m in models::MODELS {
        assert!(!m.id.is_empty() && !m.note.is_empty());
        let d = m.describe();
        assert!(d["tier"] == "fast" || d["tier"] == "capable");
    }
    assert_eq!(Kind::parse("Claude"), Some(Kind::Anthropic));
    assert_eq!(Kind::parse("openai-compatible"), Some(Kind::Compatible));
    assert_eq!(Kind::parse("nope"), None);
}

#[test]
fn redact_matches_stored_secrets_and_the_admin_token() {
    let guarded = vec![
        redact::Guarded {
            name: "assist.openai_key",
            value: zeroize::Zeroizing::new("sk-live-1234567890".into()),
        },
        redact::Guarded {
            name: "short",
            value: zeroize::Zeroizing::new("abc".into()),
        },
    ];
    assert_eq!(
        redact::check("my key is sk-live-1234567890 ok", &guarded),
        Err("assist.openai_key")
    );
    assert_eq!(redact::check("abc is too short to match", &guarded), Ok(()));
    assert_eq!(redact::check("nothing here", &guarded), Ok(()));
    let mut c = Config::for_tests();
    c.admin_token = Some("admin-secret-token".into());
    let state = test_state_with(c);
    state
        .settings
        .update(
            BTreeMap::new(),
            BTreeMap::from([(
                "leads.webhook_token".to_string(),
                Some("hook-secret-k3Qx".to_string()),
            )]),
            "test",
        )
        .expect("stored");
    let g = redact::guarded(&state.settings, state.config.admin_token.as_deref());
    assert_eq!(
        redact::check("token hook-secret-k3Qx", &g),
        Err("leads.webhook_token")
    );
    assert_eq!(
        redact::check("admin-secret-token", &g),
        Err("the admin token")
    );
}

#[test]
fn actions_and_json_answers() {
    let text = "Here you go.\n\n```action\n{\"action\":\"open_view\",\"label\":\"Open traffic\",\"payload\":{\"view\":\"traffic\"}}\n```\nmore\n```action\n{\"action\":\"open_view\",\"payload\":{\"view\":\"nope\"}}\n```\n```action\n{\"action\":\"run_request\",\"payload\":{\"method\":\"post\",\"path\":\"/echo\",\"headers\":{\"X-A\":\"1\"},\"body\":{\"a\":1}}}\n```\n```action\n{\"action\":\"run_request\",\"payload\":{\"path\":\"http://evil/x\"}}\n```\n```action\nnot json\n```\n```action\n{\"action\":\"copy\",\"payload\":{\"text\":\"curl x\"}}\n```";
    let (prose, actions) = task::extract_actions(text);
    assert_eq!(prose, "Here you go.\n\n\nmore");
    assert_eq!(actions.len(), 3, "{actions:?}");
    assert_eq!(actions[2]["payload"]["text"], "curl x");
    assert_eq!(actions[0]["payload"]["view"], "traffic");
    assert_eq!(actions[0]["label"], "Open traffic");
    assert_eq!(actions[1]["payload"]["method"], "POST");
    assert_eq!(actions[1]["payload"]["headers"]["X-A"], "1");
    assert_eq!(actions[1]["payload"]["body"]["a"], 1);
    // An unterminated block stays prose.
    let (prose, actions) =
        task::extract_actions("x ```action\n{\"action\":\"copy\",\"payload\":{\"text\":\"t\"}}");
    assert!(prose.contains("```action"));
    assert!(actions.is_empty());
    // JSON answers: bare, fenced, or inside prose.
    assert_eq!(task::parse_json_answer("{\"a\":1}").expect("bare")["a"], 1);
    assert_eq!(
        task::parse_json_answer("```json\n{\"a\":2}\n```").expect("fenced")["a"],
        2
    );
    assert_eq!(
        task::parse_json_answer("Sure: {\"a\":3} done").expect("prose")["a"],
        3
    );
    assert!(task::parse_json_answer("no object").is_err());
    let schema = tasks::explain::schema();
    assert!(task::validate(&json!({"summary": "s", "details": ["d"]}), &schema).is_ok());
    let err = task::validate(&json!({"summary": 1}), &schema).expect_err("invalid");
    assert!(err.contains("details") || err.contains("summary"), "{err}");
    // Inputs validation.
    assert!((task::find("free_answer").expect("t").prompt)(&json!({})).is_err());
    assert!((task::find("suggest").expect("t").prompt)(&json!({"view": "nope"})).is_err());
    assert!(
        (task::find("explain").expect("t").prompt)(&json!({"kind": "weird", "subject": "x"}))
            .is_err()
    );
    let long = "x".repeat(task::MAX_TEXT_CHARS + 1);
    assert!((task::find("free_answer").expect("t").prompt)(&json!({"question": long})).is_err());
    assert_eq!(tasks::suggest::static_for("mocks").len(), 4);
    assert_eq!(tasks::suggest::static_for("unknown").len(), 3);
}

#[test]
fn caps_and_usage_are_bounded() {
    let clock = Arc::new(ManualClock::new(chrono::Utc::now()));
    let mut c = mock_config();
    c.assist_max_calls_per_minute = 3;
    let state = crate::state::AppState::for_tests_with_clock(c, clock.clone());
    let a = &state.assist;
    for _ in 0..3 {
        assert!(a.admit("alice").is_ok());
    }
    let wait = a.admit("alice").expect_err("capped");
    assert!((1..=60).contains(&wait));
    assert!(a.admit("bob").is_ok(), "another caller has its own window");
    clock.advance(Duration::from_secs(61));
    assert!(a.admit("alice").is_ok());
    for i in 0..(MAX_PRINCIPALS + 20) {
        assert!(a.admit(&format!("p{i}")).is_ok());
    }
    assert!(lock(&a.calls).len() <= MAX_PRINCIPALS);
    // Usage series are bounded; totals keep counting.
    for i in 0..(MAX_USAGE_SERIES + 10) {
        a.record(Kind::OpenAi, &format!("model-{i}"), "explain", "ok", 10, 5);
    }
    let u = a.usage();
    assert_eq!(u["total"]["calls"], (MAX_USAGE_SERIES + 10) as u64);
    assert!(u["by_model"]
        .as_array()
        .is_some_and(|b| b.len() <= MAX_USAGE_SERIES));
    assert_eq!(u["by_task"][0]["task"], "explain");
    assert_eq!(
        u["total"]["estimated_cost_usd"],
        Value::Null,
        "unknown prices give no number"
    );
    a.record(Kind::OpenAi, "gpt-5-nano", "suggest", "ok", 1_000_000, 0);
    let u = a.usage();
    assert!(u["by_model"].as_array().is_some_and(|b| b
        .iter()
        .any(|m| m["model"] == "gpt-5-nano" && m["estimated_cost_usd"] == 0.05)));
}

#[test]
fn status_follows_the_settings() {
    let state = test_state_with(Config::for_tests());
    let st = state.assist.status();
    assert!(!st.enabled);
    assert!(st
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("No AI provider")));
    state
        .settings
        .update(
            BTreeMap::from([("assist.provider".to_string(), json!("openai"))]),
            BTreeMap::new(),
            "t",
        )
        .expect("ok");
    let st = state.assist.status();
    assert!(!st.enabled);
    assert_eq!(st.model, "gpt-5");
    assert_eq!(st.fast_model, "gpt-5-mini");
    assert_eq!(st.reason.as_deref(), Some("Add the openai key"));
    state
        .settings
        .update(
            BTreeMap::new(),
            BTreeMap::from([(
                "assist.openai_key".to_string(),
                Some("sk-test-123456".to_string()),
            )]),
            "t",
        )
        .expect("ok");
    let st = state.assist.status();
    assert!(st.enabled && st.key_set);
    // The provider's host is allowed under Purpose::Assist once the key is set.
    state.assist.sync_egress();
    let rule = state.egress.rule(crate::egress::Purpose::Assist);
    assert!(rule.allow.allows_host("api.openai.com", 443));
    assert!(!rule.allow.allows_host("api.anthropic.com", 443));
    // The compatible endpoint needs a base URL and a model.
    state
        .settings
        .update(
            BTreeMap::from([("assist.provider".to_string(), json!("compatible"))]),
            BTreeMap::new(),
            "t",
        )
        .expect("ok");
    assert!(state
        .assist
        .status()
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("base URL")));
    state
        .settings
        .update(
            BTreeMap::from([(
                "assist.base_url".to_string(),
                json!("http://ollama.internal:11434/v1"),
            )]),
            BTreeMap::new(),
            "t",
        )
        .expect("ok");
    assert!(state
        .assist
        .status()
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("model")));
    state
        .settings
        .update(
            BTreeMap::from([("assist.model".to_string(), json!("llama3.2"))]),
            BTreeMap::new(),
            "t",
        )
        .expect("ok");
    assert!(state.assist.status().enabled);
    state.assist.sync_egress();
    assert!(state
        .egress
        .rule(crate::egress::Purpose::Assist)
        .allow
        .allows_host("ollama.internal", 11434));
    // An invalid provider name is refused by the settings.
    assert!(state
        .settings
        .update(
            BTreeMap::from([("assist.provider".to_string(), json!("skynet"))]),
            BTreeMap::new(),
            "t"
        )
        .is_err());
}

// ── Running tasks ───────────────────────────────────────────────────

#[tokio::test]
async fn the_mock_provider_answers_every_task() {
    let state = state_with_mock();
    // A text task streams deltas and proposes actions.
    let outs = collect(
        run_task(
            &state,
            "free_answer",
            json!({"question": "How do I mock an API?", "view": "mocks", "context": {"mocks": 1}}),
        )
        .expect("runs"),
    )
    .await;
    assert!(matches!(outs.first(), Some(Out::Sent(_))));
    assert!(matches!(outs.last(), Some(Out::Done)));
    let text = text_of(&outs);
    assert!(text.contains("Mock assistant"), "{text}");
    let actions: Vec<&Out> = outs
        .iter()
        .filter(|o| matches!(o, Out::Action(_)))
        .collect();
    assert_eq!(actions.len(), 2);
    if let Out::Action(a) = actions[0] {
        assert_eq!(a["payload"]["view"], "mocks");
    }
    let usage = outs
        .iter()
        .find(|o| matches!(o, Out::Usage { .. }))
        .expect("usage");
    if let Out::Usage {
        provider,
        input,
        output,
        cost,
        cached,
        ..
    } = usage
    {
        assert_eq!(*provider, "mock");
        assert!(*input > 0 && *output > 0);
        assert_eq!(*cost, Some(0.0));
        assert!(!cached);
    }
    if let Out::Sent(sent) = &outs[0] {
        assert!(sent["user"]
            .as_str()
            .is_some_and(|u| u.contains("How do I mock an API?") && u.contains("\"mocks\": 1")));
        assert!(sent.get("key").is_none());
    }
    // A structured task answers JSON valid against its schema.
    let outs = collect(run_task(&state, "explain", json!({"kind": "request", "subject": {"method": "GET", "path": "/echo", "headers": {"X-Forwarded-For": "1.2.3.4"}}})).expect("runs")).await;
    let v = json_of(&outs).expect("json");
    assert!(task::validate(&v, &tasks::explain::schema()).is_ok());
    assert!(v["summary"]
        .as_str()
        .is_some_and(|s| s.contains("GET request to /echo")));
    assert!(v["gateway_notes"][0]
        .as_str()
        .is_some_and(|s| s.contains("X-Forwarded-For")));
    assert!(
        text_of(&outs).is_empty(),
        "structured tasks stream no deltas"
    );
    // Suggestions are cached per view and model.
    let outs =
        collect(run_task(&state, "suggest", json!({"view": "traffic"})).expect("runs")).await;
    assert_eq!(
        json_of(&outs).expect("json")["suggestions"]
            .as_array()
            .map(Vec::len),
        Some(3)
    );
    let outs =
        collect(run_task(&state, "suggest", json!({"view": "traffic"})).expect("runs")).await;
    assert!(matches!(
        outs.iter().find(|o| matches!(o, Out::Usage { .. })),
        Some(Out::Usage { cached: true, .. })
    ));
    let u = state.assist.usage();
    assert_eq!(u["total"]["calls"], 3, "the cached answer is not a call");
    assert_eq!(
        state
            .metrics
            .render()
            .matches("rustybin_assist_calls_total{")
            .count(),
        3
    );
    assert!(state.metrics.render().contains("rustybin_assist_tokens_total{provider=\"mock\",model=\"rustybin-mock\",direction=\"input\"}"));
    // Refusals before any call.
    assert_eq!(
        run_task(&state, "free_answer", json!({})).err(),
        Some(Refusal::Invalid("inputs.question is required".into()))
    );
    state
        .settings
        .update(
            BTreeMap::new(),
            BTreeMap::from([(
                "leads.webhook_token".to_string(),
                Some("hook-secret-k3Qx".to_string()),
            )]),
            "t",
        )
        .expect("stored");
    assert_eq!(
        run_task(
            &state,
            "free_answer",
            json!({"question": "is hook-secret-k3Qx a good token?"})
        )
        .err(),
        Some(Refusal::Secret("leads.webhook_token"))
    );
    let off = test_state_with(Config::for_tests());
    assert!(matches!(
        run_task(&off, "suggest", json!({"view": "mocks"})).err(),
        Some(Refusal::Disabled(_))
    ));
}

fn script(entries: Vec<Result<Vec<Event>, Problem>>) -> provider::Script {
    Arc::new(Mutex::new(VecDeque::from(entries)))
}

#[tokio::test]
async fn structured_answers_are_validated_and_repaired() {
    let state = state_with_mock();
    let schema_ok = json!({"summary": "fine", "details": ["one"]}).to_string();
    // Invalid once (wrong type), then valid: one repair, the second prompt carries the issue.
    state.assist.set_script(Some(script(vec![
        Ok(vec![
            Event::Delta("{\"summary\": 1}".into()),
            Event::Usage {
                input: 10,
                output: 5,
            },
        ]),
        Ok(vec![
            Event::Delta(schema_ok.clone()),
            Event::Usage {
                input: 20,
                output: 6,
            },
        ]),
    ])));
    let outs = collect(run_task(&state, "explain", json!({"subject": "x"})).expect("runs")).await;
    assert_eq!(json_of(&outs).expect("json")["summary"], "fine");
    if let Some(Out::Usage { input, output, .. }) =
        outs.iter().find(|o| matches!(o, Out::Usage { .. }))
    {
        assert_eq!((*input, *output), (30, 11), "both attempts are counted");
    }
    // Fenced JSON inside prose is accepted.
    state
        .assist
        .set_script(Some(script(vec![Ok(vec![Event::Delta(format!(
            "Sure!\n```json\n{schema_ok}\n```"
        ))])])));
    let outs = collect(run_task(&state, "explain", json!({"subject": "y"})).expect("runs")).await;
    assert!(json_of(&outs).is_some());
    // Three invalid answers: an error naming the problem, counted as invalid.
    state.assist.set_script(Some(script(vec![
        Ok(vec![Event::Delta("nonsense".into())]),
        Ok(vec![Event::Delta("{}".into())]),
        Ok(vec![Event::Delta("{\"details\": []}".into())]),
    ])));
    let outs = collect(run_task(&state, "explain", json!({"subject": "z"})).expect("runs")).await;
    let err = error_of(&outs).expect("error");
    assert!(err.contains("after 3 attempts"), "{err}");
    assert!(json_of(&outs).is_none());
    // A provider problem before any event, and an error mid-stream.
    state.assist.set_script(Some(script(vec![Err(Problem::new(
        401,
        "the provider answered 401: bad key",
    ))])));
    let outs =
        collect(run_task(&state, "free_answer", json!({"question": "q"})).expect("runs")).await;
    assert!(
        matches!(outs.get(1), Some(Out::Error { status: 401, .. })),
        "{outs:?}"
    );
    state.assist.set_script(Some(script(vec![Ok(vec![
        Event::Delta("part".into()),
        Event::Error("overloaded".into()),
    ])])));
    let outs =
        collect(run_task(&state, "free_answer", json!({"question": "q"})).expect("runs")).await;
    assert_eq!(text_of(&outs), "part");
    assert_eq!(error_of(&outs).as_deref(), Some("overloaded"));
    let u = state.assist.usage();
    assert_eq!(u["total"]["errors"], 3);
    assert!(state
        .metrics
        .render()
        .contains("task=\"explain\",outcome=\"invalid\"} 1"));
}

// ── Routes ──────────────────────────────────────────────────────────

fn parse_sse(text: &str) -> Vec<(String, Value)> {
    text.split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .map(|block| {
            let mut event = String::new();
            let mut data = String::new();
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("event: ") {
                    event = v.to_string();
                } else if let Some(v) = line.strip_prefix("data: ") {
                    data = v.to_string();
                }
            }
            (event, serde_json::from_str(&data).unwrap_or(Value::Null))
        })
        .collect()
}

#[tokio::test]
async fn routes_without_and_with_a_provider() {
    let app = test_app_with(test_state_with(Config::for_tests()));
    let resp = app
        .clone()
        .oneshot(bearer_request("GET", STATUS_PATH, None))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["enabled"], false);
    assert!(v["providers"].as_array().is_some_and(|p| p.len() == 5));
    assert!(v["providers"][0]["models"]
        .as_array()
        .is_some_and(|m| !m.is_empty()));
    assert_eq!(v["providers"][0]["key"]["set"], false);
    assert_eq!(
        v["tasks"].as_array().map(Vec::len),
        Some(crate::assist::task::all().len())
    );
    assert!(v["suggestions"]["overview"]
        .as_array()
        .is_some_and(|s| s.len() == 3));
    assert!(v["privacy"]
        .as_str()
        .is_some_and(|p| p.contains("never leave")));
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            RUN_PATH,
            &json!({"task": "suggest", "inputs": {"view": "mocks"}}),
        ))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body_json(resp).await["reason"], "assist_disabled");
    let resp = app
        .clone()
        .oneshot(bearer_request("POST", TEST_PATH, None))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    // The mock provider from the environment.
    let state = state_with_mock();
    let app = test_app_with(state.clone());
    let v = body_json(
        app.clone()
            .oneshot(bearer_request("GET", STATUS_PATH, None))
            .await
            .expect("r"),
    )
    .await;
    assert_eq!(v["enabled"], true);
    assert_eq!(v["provider"], "mock");
    assert_eq!(v["model"], "rustybin-mock");
    // Streamed: the event sequence.
    let resp = app.clone().oneshot(json_request("POST", RUN_PATH, &json!({"task": "free_answer", "inputs": {"question": "show me traffic"}, "stream": true}))).await.expect("r");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("text/event-stream")));
    let text = String::from_utf8(body_bytes(resp).await.to_vec()).expect("utf8");
    let events = parse_sse(&text);
    let names: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(names[0], "sent");
    assert!(names.contains(&"delta") && names.contains(&"action") && names.contains(&"usage"));
    assert_eq!(names.last(), Some(&"done"));
    let action = events
        .iter()
        .find(|(e, _)| e == "action")
        .map(|(_, d)| d.clone())
        .expect("action");
    assert_eq!(action["payload"]["view"], "traffic");
    // One JSON document.
    let resp = app.clone().oneshot(json_request("POST", RUN_PATH, &json!({"task": "explain", "inputs": {"kind": "text", "subject": "hello"}, "stream": false}))).await.expect("r");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert!(v["json"]["summary"].is_string());
    assert_eq!(v["text"], Value::Null);
    assert_eq!(v["usage"]["provider"], "mock");
    assert_eq!(v["sent"]["task"], "explain");
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            RUN_PATH,
            &json!({"task": "free_answer", "inputs": {"question": "hi"}, "stream": false}),
        ))
        .await
        .expect("r");
    let v = body_json(resp).await;
    assert!(v["text"].as_str().is_some_and(|t| !t.contains("```action")));
    assert_eq!(v["actions"].as_array().map(Vec::len), Some(2));
    // Errors: unknown task, invalid inputs, a secret in the inputs.
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            RUN_PATH,
            &json!({"task": "nope", "inputs": {}}),
        ))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            RUN_PATH,
            &json!({"task": "suggest", "inputs": {"view": "nowhere"}}),
        ))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    state
        .settings
        .update(
            BTreeMap::new(),
            BTreeMap::from([(
                "leads.webhook_token".to_string(),
                Some("hook-secret-k3Qx".to_string()),
            )]),
            "t",
        )
        .expect("stored");
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            RUN_PATH,
            &json!({"task": "free_answer", "inputs": {"question": "hook-secret-k3Qx?"}}),
        ))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body_json(resp).await["reason"], "secret_in_inputs");
    // Test and usage.
    let resp = app
        .clone()
        .oneshot(bearer_request("POST", TEST_PATH, None))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["ok"], true);
    assert_eq!(v["answer"], "OK");
    let resp = app
        .clone()
        .oneshot(bearer_request("GET", USAGE_PATH, None))
        .await
        .expect("r");
    let v = body_json(resp).await;
    assert_eq!(v["total"]["calls"], 4);
    assert!(v["by_task"]
        .as_array()
        .is_some_and(|t| t.iter().any(|x| x["task"] == "test")));
}

#[tokio::test]
async fn calls_per_minute_cap_answers_429() {
    let mut c = mock_config();
    c.assist_max_calls_per_minute = 2;
    let app = test_app_with(test_state_with(c));
    for _ in 0..2 {
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                RUN_PATH,
                &json!({"task": "explain", "inputs": {"subject": "x"}, "stream": false}),
            ))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::OK);
    }
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            RUN_PATH,
            &json!({"task": "explain", "inputs": {"subject": "x"}, "stream": false}),
        ))
        .await
        .expect("r");
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(resp.headers().get("retry-after").is_some());
    assert_eq!(body_json(resp).await["reason"], "rate_limited");
}

#[tokio::test]
async fn control_auth_applies_to_the_assist_routes() {
    let mut c = mock_config();
    c.control_auth = crate::control_auth::ControlAuth::Jwt;
    c.control_jwt_key = crate::control_auth::parse_public_key(control_keys::CONTROL_JWT_PUBLIC_PEM);
    c.admin_token = Some("admin-secret".into());
    let app = test_app_with(test_state_with(c));
    let status = |method: &'static str, path: &'static str, token: Option<String>, body: Value| {
        let app = app.clone();
        async move {
            let mut b = HttpRequest::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json");
            if let Some(t) = token {
                b = b.header("authorization", format!("Bearer {t}"));
            }
            app.oneshot(b.body(Body::from(body.to_string())).expect("req"))
                .await
                .expect("r")
                .status()
        }
    };
    let run = json!({"task": "suggest", "inputs": {"view": "mocks"}, "stream": false});
    assert_eq!(
        status("POST", RUN_PATH, None, run.clone()).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(
            "GET",
            STATUS_PATH,
            Some(control_jwt(Some("console"))),
            json!({})
        )
        .await,
        StatusCode::OK
    );
    // A read task is open to the console scope; test and usage need admin.
    assert_eq!(
        status(
            "POST",
            RUN_PATH,
            Some(control_jwt(Some("console"))),
            run.clone()
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        status(
            "POST",
            TEST_PATH,
            Some(control_jwt(Some("console"))),
            json!({})
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        status(
            "GET",
            USAGE_PATH,
            Some(control_jwt(Some("console"))),
            json!({})
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        status("POST", TEST_PATH, Some("admin-secret".into()), json!({})).await,
        StatusCode::OK
    );
    assert_eq!(
        status(
            "GET",
            USAGE_PATH,
            Some(control_jwt(Some("admin"))),
            json!({})
        )
        .await,
        StatusCode::OK
    );
}

// ── Stand-in servers for the real wire formats ───────────────────────

/// A tiny server speaking one provider's streaming format: checks the
/// credential header and the request shape, answers a structured explain.
async fn stand_in(kind: Kind) -> (std::net::SocketAddr, Arc<Mutex<Vec<Value>>>) {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let answer = json!({"summary": "A GET to /echo.", "details": ["It echoes."], "gateway_notes": [], "next_steps": ["Compare headers."]}).to_string();
    let handler = move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
        let seen = seen2.clone();
        let answer = answer.clone();
        async move {
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let auth_ok = match kind {
                Kind::Anthropic => {
                    headers.get("x-api-key").and_then(|v| v.to_str().ok())
                        == Some("ant-key-1234567")
                        && headers.get("anthropic-version").is_some()
                }
                Kind::OpenAi | Kind::Compatible => {
                    headers.get("authorization").and_then(|v| v.to_str().ok())
                        == Some("Bearer oai-key-1234567")
                }
                Kind::Gemini => {
                    headers.get("x-goog-api-key").and_then(|v| v.to_str().ok())
                        == Some("gem-key-1234567")
                }
                Kind::Mock => true,
            };
            seen.lock().unwrap_or_else(|e| e.into_inner()).push(json!({"headers_ok": auth_ok, "body": v, "accept": headers.get("accept").and_then(|v| v.to_str().ok())}));
            if !auth_ok {
                return (
                    StatusCode::UNAUTHORIZED,
                    [("content-type", "application/json")],
                    json!({"error": {"message": "invalid key"}}).to_string(),
                )
                    .into_response();
            }
            let frames = match kind {
                Kind::Anthropic => vec![
                    json!({"type": "message_start", "message": {"usage": {"input_tokens": 40}}}),
                    json!({"type": "content_block_delta", "delta": {"type": "input_json_delta", "partial_json": answer[..10]}}),
                    json!({"type": "content_block_delta", "delta": {"type": "input_json_delta", "partial_json": answer[10..]}}),
                    json!({"type": "message_delta", "usage": {"output_tokens": 30}}),
                ],
                Kind::OpenAi | Kind::Compatible => vec![
                    json!({"choices": [{"delta": {"content": answer[..10]}}]}),
                    json!({"choices": [{"delta": {"content": answer[10..]}}]}),
                    json!({"choices": [], "usage": {"prompt_tokens": 40, "completion_tokens": 30}}),
                ],
                Kind::Gemini | Kind::Mock => vec![
                    json!({"candidates": [{"content": {"parts": [{"text": answer[..10]}]}}], "usageMetadata": {"promptTokenCount": 40, "candidatesTokenCount": 10}}),
                    json!({"candidates": [{"content": {"parts": [{"text": answer[10..]}]}}], "usageMetadata": {"promptTokenCount": 40, "candidatesTokenCount": 30}}),
                ],
            };
            let mut body = String::new();
            for f in frames {
                body.push_str(&format!("data: {f}\n\n"));
            }
            if kind == Kind::OpenAi {
                body.push_str("data: [DONE]\n\n");
            }
            ([("content-type", "text/event-stream")], body).into_response()
        }
    };
    // The paths as constants: the catalogue test scans route literals.
    const MESSAGES: &str = "/v1/messages";
    const CHAT: &str = "/v1/chat/completions";
    const GENERATE: &str = "/v1beta/models/{model}";
    let app = Router::new()
        .route(MESSAGES, post(handler.clone()))
        .route(CHAT, post(handler.clone()))
        .route(GENERATE, post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, seen)
}

async fn run_against_stand_in(kind: Kind, provider: &str, key_field: fn(&mut Config, String)) {
    let (addr, seen) = stand_in(kind).await;
    let mut c = Config::for_tests();
    c.assist_provider = Some(provider.into());
    c.egress_allow_loopback = true;
    let key = match kind {
        Kind::Anthropic => "ant-key-1234567",
        Kind::Gemini => "gem-key-1234567",
        _ => "oai-key-1234567",
    };
    key_field(&mut c, key.to_string());
    let base = if kind == Kind::OpenAi {
        format!("http://{addr}/v1")
    } else {
        format!("http://{addr}")
    };
    if kind == Kind::Compatible {
        c.assist_base_url = Some(format!("http://{addr}/v1"));
        c.assist_model = Some("local-model".into());
    }
    let state = test_state_with(c);
    if kind != Kind::Compatible {
        state.assist.override_endpoint(kind, &base);
    }
    let outs = collect(
        run_task(
            &state,
            "explain",
            json!({"kind": "request", "subject": {"method": "GET", "path": "/echo"}}),
        )
        .expect("runs"),
    )
    .await;
    let v = json_of(&outs).unwrap_or_else(|| panic!("{kind:?}: no json in {outs:?}"));
    assert_eq!(v["summary"], "A GET to /echo.");
    if let Some(Out::Usage {
        input,
        output,
        provider: p,
        ..
    }) = outs.iter().find(|o| matches!(o, Out::Usage { .. }))
    {
        assert_eq!((*input, *output), (40, 30), "{kind:?}");
        assert_eq!(*p, kind.as_str());
    }
    let seen = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["headers_ok"], true, "{kind:?}");
    let body = &seen[0]["body"];
    match kind {
        Kind::Anthropic => {
            assert_eq!(body["tools"][0]["name"], "answer");
            assert_eq!(body["stream"], true);
        }
        Kind::OpenAi => assert_eq!(body["response_format"]["type"], "json_schema"),
        Kind::Compatible => {
            assert_eq!(body["response_format"]["type"], "json_object");
            assert_eq!(body["model"], "local-model");
        }
        Kind::Gemini => assert_eq!(
            body["generationConfig"]["responseMimeType"],
            "application/json"
        ),
        Kind::Mock => {}
    }
    // The connectivity test round-trips too.
    let t = state.assist.test().await.expect("test");
    assert_eq!(t["ok"], true);
    assert_eq!(t["provider"], kind.as_str());
    assert!(t["latency_ms"].is_number());
    // A wrong key: the provider's message, no stream.
    let mut c2 = Config::for_tests();
    c2.assist_provider = Some(provider.into());
    c2.egress_allow_loopback = true;
    key_field(&mut c2, "wrong-key-1234567".into());
    if kind == Kind::Compatible {
        c2.assist_base_url = Some(format!("http://{addr}/v1"));
        c2.assist_model = Some("local-model".into());
    }
    let state2 = test_state_with(c2);
    if kind != Kind::Compatible {
        state2.assist.override_endpoint(kind, &base);
    }
    let outs = collect(run_task(&state2, "explain", json!({"subject": "x"})).expect("runs")).await;
    match outs
        .iter()
        .find(|o| !matches!(o, Out::Sent(_) | Out::Progress(_)))
    {
        Some(Out::Error {
            message,
            status: 401,
        }) => assert!(message.contains("invalid key"), "{message}"),
        other => panic!("{kind:?}: expected a 401 problem, got {other:?}"),
    }
}

#[tokio::test]
async fn anthropic_wire_format() {
    run_against_stand_in(Kind::Anthropic, "anthropic", |c, k| {
        c.assist_anthropic_key = Some(k)
    })
    .await;
}

#[tokio::test]
async fn openai_wire_format() {
    run_against_stand_in(Kind::OpenAi, "openai", |c, k| c.assist_openai_key = Some(k)).await;
}

#[tokio::test]
async fn compatible_wire_format() {
    run_against_stand_in(Kind::Compatible, "compatible", |c, k| {
        c.assist_compatible_key = Some(k)
    })
    .await;
}

#[tokio::test]
async fn gemini_wire_format() {
    run_against_stand_in(Kind::Gemini, "gemini", |c, k| c.assist_gemini_key = Some(k)).await;
}

#[tokio::test]
async fn egress_refuses_an_unlisted_host() {
    // A key for OpenAI allows api.openai.com only: a compatible base URL on
    // another host is refused before any connection.
    let mut c = Config::for_tests();
    c.assist_provider = Some("compatible".into());
    c.assist_base_url = Some("http://169.254.169.254/v1".into());
    c.assist_model = Some("m".into());
    let state = test_state_with(c);
    // Allowed by the rule (its own base URL), refused as a forbidden address.
    let outs = collect(run_task(&state, "explain", json!({"subject": "x"})).expect("runs")).await;
    let err = error_of(&outs).expect("error");
    assert!(err.contains("refused"), "{err}");
}
