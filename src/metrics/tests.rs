use super::*;
use crate::test_support::{body_string, get_request, json_request, test_app_with, test_state};
use axum::http::Request as HttpRequest;
use std::collections::BTreeSet;
use tower::ServiceExt;

async fn scrape(app: &Router) -> String {
    let resp = app
        .clone()
        .oneshot(get_request(METRICS_PATH))
        .await
        .expect("response");
    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/plain; version=0.0.4")));
    body_string(resp).await
}

/// Every sample line's labels as (metric name, label map).
fn samples(text: &str) -> Vec<(String, HashMap<String, String>, f64)> {
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| {
            let (series, value) = l.rsplit_once(' ').expect("value");
            let value: f64 = value.parse().expect("number");
            let (name, labels) = match series.split_once('{') {
                Some((n, rest)) => (n, rest.trim_end_matches('}')),
                None => (series, ""),
            };
            let labels = labels
                .split("\",")
                .filter(|kv| !kv.is_empty())
                .map(|kv| {
                    let (k, v) = kv.split_once('=').expect("label");
                    (k.to_string(), v.trim_matches('"').to_string())
                })
                .collect();
            (name.to_string(), labels, value)
        })
        .collect()
}

fn value(text: &str, name: &str, labels: &[(&str, &str)]) -> f64 {
    samples(text)
        .into_iter()
        .filter(|(n, l, _)| {
            n == name
                && labels
                    .iter()
                    .all(|(k, v)| l.get(*k).map(String::as_str) == Some(*v))
        })
        .map(|(_, _, v)| v)
        .sum()
}

#[test]
fn model_families_are_bounded() {
    assert_eq!(model_family("gpt-4o-mini"), "gpt-4o");
    assert_eq!(model_family("openai/gpt-4.1-nano"), "gpt-4.1");
    assert_eq!(model_family("o3-mini"), "o-series");
    assert_eq!(
        model_family("us.anthropic.claude-3-5-sonnet-20241022-v2:0"),
        "claude"
    );
    assert_eq!(model_family("gemini-2.0-flash"), "gemini");
    assert_eq!(model_family("text-embedding-3-small"), "embedding");
    assert_eq!(model_family("llama3.2"), "llama");
    assert_eq!(model_family("my-custom-model-123"), "other");
    assert_eq!(model_family("pro1"), "other", "o1 only as a prefix");
}

#[test]
fn label_caps() {
    let m = Arc::new(Metrics::new());
    for i in 0..(MAX_ROUTES + 50) {
        m.observe_request(
            &format!("/r{i}"),
            &Method::GET,
            200,
            Duration::from_millis(1),
        );
    }
    let labels = m.route_labels();
    assert_eq!(labels.len(), MAX_ROUTES + 1);
    assert!(labels.contains(&"other".to_string()));
    for i in 0..(MAX_LLM_SERIES + 10) {
        let provider: &'static str = Box::leak(format!("p{i}").into_boxed_str());
        m.record_llm_tokens(provider, "gpt-4o", 1, 1);
    }
    assert!(m.lock().llm_tokens.len() <= MAX_LLM_SERIES + 2);
    m.count_fault("weird");
    assert_eq!(m.lock().faults.get("other"), Some(&1));
    let mut odd = Method::from_bytes(b"BREW").expect("method");
    m.observe_request("/x", &odd, 799, Duration::ZERO);
    odd = Method::GET;
    let _ = odd;
    assert!(m.render().contains("method=\"OTHER\",status_class=\"5xx\""));
}

#[tokio::test]
async fn route_label_is_the_template_and_cardinality_is_bounded() {
    let state = test_state();
    let app = test_app_with(state.clone());
    // Many distinct raw paths, a handful of route templates.
    for i in 0..150 {
        for uri in [
            format!("/status/{}", 200 + (i % 3)),
            format!("/echo/item-{i}"),
            format!("/no/such/route/{i}"),
            format!("/anything/{i}?q={i}"),
            format!("/bytes/{}", i + 1),
        ] {
            let _ = app
                .clone()
                .oneshot(get_request(&uri))
                .await
                .expect("response");
        }
    }
    let text = scrape(&app).await;
    let routes: BTreeSet<String> = samples(&text)
        .into_iter()
        .filter_map(|(n, l, _)| (n == "rustybin_requests_total").then(|| l.get("route").cloned()))
        .flatten()
        .collect();
    assert!(routes.contains("/status/{code}"), "{routes:?}");
    assert!(routes.contains("/echo/{*path}"), "{routes:?}");
    assert!(routes.contains(UNMATCHED), "{routes:?}");
    assert!(
        !text.contains("item-7"),
        "raw paths must never become labels"
    );
    assert!(!text.contains("/no/such/route"));
    // Bounded by the number of catalogued routes (plus `unmatched`).
    assert!(routes.len() <= 10, "{routes:?}");
    assert!(state.metrics.route_labels().len() <= crate::catalog::all().len() + 1);
    assert_eq!(
        value(
            &text,
            "rustybin_requests_total",
            &[("route", "/status/{code}")]
        ),
        150.0
    );
    assert_eq!(
        value(
            &text,
            "rustybin_request_duration_seconds_count",
            &[("route", "/status/{code}")]
        ),
        150.0
    );
    assert_eq!(
        value(
            &text,
            "rustybin_requests_total",
            &[("route", UNMATCHED), ("status_class", "4xx")]
        ),
        150.0
    );
}

#[tokio::test]
async fn every_family_is_exported() {
    let state = test_state();
    let app = test_app_with(state.clone());
    // Plain HTTP with an injected fault and a delay.
    let req = HttpRequest::builder()
        .uri("/echo")
        .header("x-rustybin-fail", "503")
        .body(axum::body::Body::empty())
        .expect("request");
    let _ = app.clone().oneshot(req).await.expect("response");
    let req = HttpRequest::builder()
        .uri("/uuid")
        .header("x-rustybin-delay", "1")
        .body(axum::body::Body::empty())
        .expect("request");
    let _ = app.clone().oneshot(req).await.expect("response");
    // Mock LLM.
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/ai/openai/v1/chat/completions",
            &json!({"model": "gpt-4o-mini", "messages": [{"role": "user", "content": "hi"}]}),
        ))
        .await
        .expect("response");
    assert_eq!(resp.status(), 200);
    let _ = body_string(resp).await;
    // GraphQL.
    let _ = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/graphql",
            &json!({"query": "{ __typename }"}),
        ))
        .await
        .expect("response");
    // SSE: the stream is open while the body is alive, closed after.
    let resp = app
        .clone()
        .oneshot(get_request("/sse?count=2&interval_ms=10"))
        .await
        .expect("response");
    assert_eq!(state.metrics.streams_open(), 1);
    let _ = body_string(resp).await;
    assert_eq!(state.metrics.streams_open(), 0);

    let text = scrape(&app).await;
    assert_eq!(
        value(&text, "rustybin_faults_injected_total", &[("kind", "fail")]),
        1.0
    );
    assert_eq!(
        value(
            &text,
            "rustybin_faults_injected_total",
            &[("kind", "delay")]
        ),
        1.0
    );
    assert!(
        value(
            &text,
            "rustybin_protocol_requests_total",
            &[("protocol", "llm")]
        ) >= 1.0
    );
    assert!(
        value(
            &text,
            "rustybin_protocol_requests_total",
            &[("protocol", "graphql")]
        ) >= 1.0
    );
    assert!(
        value(
            &text,
            "rustybin_protocol_requests_total",
            &[("protocol", "sse")]
        ) >= 1.0
    );
    assert!(
        value(
            &text,
            "rustybin_protocol_requests_total",
            &[("protocol", "http")]
        ) >= 2.0
    );
    for p in Protocol::ALL {
        assert!(
            text.contains(&format!(
                "rustybin_protocol_requests_total{{protocol=\"{}\"}}",
                p.as_str()
            )),
            "{p:?}"
        );
    }
    assert!(
        value(
            &text,
            "rustybin_llm_tokens_total",
            &[
                ("provider", "openai"),
                ("model_family", "gpt-4o"),
                ("direction", "input")
            ]
        ) > 0.0,
        "{text}"
    );
    assert!(
        value(
            &text,
            "rustybin_llm_tokens_total",
            &[("model_family", "gpt-4o"), ("direction", "output")]
        ) > 0.0
    );
    assert!(value(&text, "rustybin_egress_bytes_total", &[]) > 0.0);
    assert_eq!(value(&text, "rustybin_streams_open", &[]), 0.0);
    assert_eq!(
        value(
            &text,
            "rustybin_build_info",
            &[("version", env!("CARGO_PKG_VERSION"))]
        ),
        1.0
    );
    // Every line is a comment or a well-formed sample.
    for line in text.lines() {
        assert!(
            line.starts_with("# HELP ")
                || line.starts_with("# TYPE ")
                || line.starts_with("rustybin_"),
            "{line}"
        );
    }
}

#[tokio::test]
async fn histogram_buckets_are_cumulative() {
    let m = Metrics::new();
    m.observe_request("/a", &Method::GET, 200, Duration::from_millis(3));
    m.observe_request("/a", &Method::GET, 200, Duration::from_millis(300));
    m.observe_request("/a", &Method::POST, 404, Duration::from_secs(30));
    let text = m.render();
    assert_eq!(
        value(
            &text,
            "rustybin_request_duration_seconds_bucket",
            &[("le", "0.005")]
        ),
        1.0
    );
    assert_eq!(
        value(
            &text,
            "rustybin_request_duration_seconds_bucket",
            &[("le", "0.5")]
        ),
        2.0
    );
    assert_eq!(
        value(
            &text,
            "rustybin_request_duration_seconds_bucket",
            &[("le", "+Inf")]
        ),
        3.0
    );
    assert_eq!(
        value(&text, "rustybin_request_duration_seconds_count", &[]),
        3.0
    );
    assert_eq!(
        value(
            &text,
            "rustybin_requests_total",
            &[("method", "POST"), ("status_class", "4xx")]
        ),
        1.0
    );
}

#[tokio::test]
async fn metrics_are_protected_when_control_auth_is_on() {
    let app = test_app_with(crate::test_support::test_state_with(
        crate::test_support::jwt_config(),
    ));
    let resp = app
        .clone()
        .oneshot(get_request(METRICS_PATH))
        .await
        .expect("response");
    assert_eq!(resp.status(), 401);
    let resp = app
        .oneshot(crate::test_support::bearer_request(
            "GET",
            METRICS_PATH,
            Some(&crate::test_support::control_jwt(Some("console"))),
        ))
        .await
        .expect("response");
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn limit_rejections_are_counted_by_dimension() {
    use crate::limits::{Limiter, LimitsConfig, Plan};
    let mut config = crate::config::Config::for_tests();
    config.limits = LimitsConfig {
        plan: Plan::Pro,
        requests: 2,
        ..LimitsConfig::default()
    };
    let state = crate::test_support::test_state_with(config);
    let app = test_app_with(state.clone());
    let mut statuses = Vec::new();
    for _ in 0..5 {
        let resp = app
            .clone()
            .oneshot(get_request("/uuid"))
            .await
            .expect("response");
        statuses.push(resp.status().as_u16());
    }
    assert_eq!(statuses, [200, 200, 429, 429, 429]);
    let text = state.metrics.render();
    assert_eq!(
        value(
            &text,
            "rustybin_limit_rejections_total",
            &[("dimension", "requests")]
        ),
        3.0
    );
    // Every dimension is exported (zero until it rejects something).
    for d in Dimension::ALL {
        assert!(
            text.contains(&format!(
                "rustybin_limit_rejections_total{{dimension=\"{}\"}}",
                d.as_str()
            )),
            "{d:?}"
        );
    }
    assert_eq!(
        value(
            &text,
            "rustybin_limit_rejections_total",
            &[("dimension", "rps")]
        ),
        0.0
    );

    // gRPC: RESOURCE_EXHAUSTED counts too.
    let metrics = Arc::new(Metrics::new());
    let limiter = Limiter::new(LimitsConfig {
        plan: Plan::Pro,
        rps: 1,
        ..LimitsConfig::default()
    })
    .with_metrics(metrics.clone());
    assert!(limiter.check_grpc(&tonic::Request::new(())).is_ok());
    assert!(limiter.check_grpc(&tonic::Request::new(())).is_err());
    assert_eq!(
        value(
            &metrics.render(),
            "rustybin_limit_rejections_total",
            &[("dimension", "rps")]
        ),
        1.0
    );
}

#[tokio::test]
async fn llm_requests_and_faults_are_counted() {
    let state = test_state();
    let app = test_app_with(state.clone());
    let chat = |stream: bool| {
        json_request(
            "POST",
            "/ai/openai/v1/chat/completions",
            &json!({
                "model": "gpt-4o-mini",
                "stream": stream,
                "messages": [{"role": "user", "content": "hi"}]
            }),
        )
    };
    for stream in [false, false, true] {
        let resp = app.clone().oneshot(chat(stream)).await.expect("response");
        assert_eq!(resp.status(), 200);
        let _ = body_string(resp).await;
    }
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/ai/anthropic/v1/messages",
            &json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "hi"}]
            }),
        ))
        .await
        .expect("response");
    assert_eq!(resp.status(), 200);
    let _ = body_string(resp).await;
    // Injected faults: a native error, a raw status and a content filter.
    for fail in ["rate_limit", "418", "content_filter"] {
        let mut req = chat(false);
        req.headers_mut().insert(
            "x-rustybin-fail",
            HeaderValue::from_str(fail).expect("header"),
        );
        let resp = app.clone().oneshot(req).await.expect("response");
        let _ = body_string(resp).await;
    }

    let text = scrape(&app).await;
    let count = |labels: &[(&str, &str)]| value(&text, "rustybin_llm_requests_total", labels);
    assert_eq!(
        count(&[
            ("provider", "openai"),
            ("model_family", "gpt-4o"),
            ("streaming", "false")
        ]),
        3.0,
        "two plain chats and the content filtered one: {text}"
    );
    assert_eq!(
        count(&[
            ("provider", "openai"),
            ("model_family", "gpt-4o"),
            ("streaming", "true")
        ]),
        1.0
    );
    assert_eq!(
        count(&[
            ("provider", "anthropic"),
            ("model_family", "claude"),
            ("streaming", "false")
        ]),
        1.0
    );
    let faults = |kind: &str| {
        value(
            &text,
            "rustybin_llm_faults_total",
            &[("provider", "openai"), ("kind", kind)],
        )
    };
    assert_eq!(faults("rate_limit"), 1.0);
    assert_eq!(faults("status_4xx"), 1.0);
    assert_eq!(faults("content_filter"), 1.0);
    // The generic fault counter still sees the error responses.
    assert_eq!(
        value(&text, "rustybin_faults_injected_total", &[("kind", "ai")]),
        2.0
    );
}

#[test]
fn llm_labels_are_bounded() {
    let m = Metrics::new();
    for i in 0..(MAX_LLM_SERIES + 50) {
        m.count_llm_request(&format!("provider-{i}"), &format!("model-{i}"), i % 2 == 0);
        m.count_llm_fault(&format!("provider-{i}"), &format!("kind-{i}"));
    }
    for p in LLM_PROVIDERS {
        for model in ["gpt-4o", "claude-3", "gemini-2.0", "llama3", "x"] {
            m.count_llm_request(p, model, true);
            m.count_llm_request(p, model, false);
        }
        for kind in LLM_FAULT_KINDS {
            m.count_llm_fault(p, kind);
        }
    }
    let text = m.render();
    let mut providers = BTreeSet::new();
    let mut families = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    let mut streaming = BTreeSet::new();
    for (name, labels, _) in samples(&text) {
        match name.as_str() {
            "rustybin_llm_requests_total" => {
                providers.insert(labels["provider"].clone());
                families.insert(labels["model_family"].clone());
                streaming.insert(labels["streaming"].clone());
            }
            "rustybin_llm_faults_total" => {
                providers.insert(labels["provider"].clone());
                kinds.insert(labels["kind"].clone());
            }
            _ => {}
        }
    }
    assert!(providers.len() <= LLM_PROVIDERS.len() + 1, "{providers:?}");
    assert!(providers.contains("other"));
    assert!(kinds.len() <= LLM_FAULT_KINDS.len() + 1, "{kinds:?}");
    assert!(kinds.contains("other"));
    assert_eq!(
        streaming,
        BTreeSet::from(["false".to_string(), "true".to_string()])
    );
    assert!(
        families.iter().all(|f| !f.starts_with("model-")),
        "{families:?}"
    );
    let inner = m.lock();
    assert!(inner.llm_requests.len() <= MAX_LLM_SERIES + 2);
    assert!(inner.llm_faults.len() <= (LLM_PROVIDERS.len() + 1) * (LLM_FAULT_KINDS.len() + 1));
}

#[test]
fn buckets_cover_1ms_to_60s_for_quantiles() {
    assert_eq!(BUCKETS.first(), Some(&0.001));
    assert_eq!(BUCKETS.last(), Some(&60.0));
    assert!(BUCKETS.windows(2).all(|w| w[0] < w[1]));
    // No gap wider than 3x between neighbours: quantile interpolation stays
    // within one bucket of the true value.
    assert!(BUCKETS.windows(2).all(|w| w[1] / w[0] <= 3.0));
    let m = Metrics::new();
    m.observe_request("/a", &Method::GET, 200, Duration::from_micros(800));
    m.observe_request("/a", &Method::GET, 200, Duration::from_secs(45));
    m.observe_request("/a", &Method::GET, 200, Duration::from_secs(90));
    let text = m.render();
    let bucket = |le: &str| {
        value(
            &text,
            "rustybin_request_duration_seconds_bucket",
            &[("le", le)],
        )
    };
    assert_eq!(bucket("0.001"), 1.0);
    assert_eq!(bucket("30"), 1.0);
    assert_eq!(bucket("60"), 2.0);
    assert_eq!(bucket("+Inf"), 3.0);
}
