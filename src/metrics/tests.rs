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
