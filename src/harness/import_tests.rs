//! Recordings to templates: the mix, and above all what must not survive.

use serde_json::{json, Value};

use super::*;

const SECRET: &str = "s3cr3t-value-that-must-not-survive";

fn har(entries: Vec<Value>) -> Value {
    json!({ "log": { "version": "1.2", "entries": entries } })
}

fn entry(method: &str, url: &str) -> Value {
    json!({ "request": { "method": method, "url": url, "headers": [] }, "response": { "status": 200 } })
}

#[test]
fn the_mix_is_the_recordings() {
    let doc = har(vec![
        entry("GET", "https://api.example.com/orders?page=1"),
        entry("GET", "https://api.example.com/orders?page=1"),
        entry("GET", "https://api.example.com/orders?page=1"),
        entry("post", "https://api.example.com/orders"),
    ]);
    let out = import(&doc).expect("imported");
    assert_eq!(out.entries, 4);
    assert_eq!(out.requests.len(), 2);
    assert_eq!(out.requests[0].path, "/orders?page=1");
    assert_eq!(out.requests[0].weight, 3);
    assert_eq!(out.requests[1].method, "POST");
    assert_eq!(out.hosts, vec!["api.example.com"]);
}

#[test]
fn no_credential_survives_in_any_form() {
    let mut with_headers = entry("GET", &format!("https://a.test/x?api_key={SECRET}&page=2"));
    with_headers["request"]["headers"] = json!([
        { "name": "Authorization", "value": format!("Bearer {SECRET}") },
        { "name": "Cookie", "value": format!("sid={SECRET}") },
        { "name": "X-Api-Key", "value": SECRET },
        { "name": "Accept", "value": "application/json" },
        { "name": "Host", "value": "a.test" },
        { "name": ":authority", "value": "a.test" }
    ]);
    let mut json_body = entry("POST", "https://a.test/login");
    json_body["request"]["postData"] = json!({ "mimeType": "application/json",
        "text": format!(r#"{{"user":"a","nested":{{"password":"{SECRET}"}}}}"#) });
    let mut form_body = entry("POST", "https://a.test/token");
    form_body["request"]["postData"] = json!({ "mimeType": "application/x-www-form-urlencoded",
        "text": format!("grant_type=password&client_secret={SECRET}") });
    let out = import(&har(vec![with_headers, json_body, form_body])).expect("imported");

    let text = serde_json::to_string(&out).expect("json");
    assert!(!text.contains(SECRET), "{text}");
    assert_eq!(out.requests.len(), 1, "{text}");
    let kept = &out.requests[0];
    assert_eq!(kept.path, "/x?page=2");
    assert_eq!(kept.headers.len(), 1, "{:?}", kept.headers);
    assert_eq!(kept.headers["accept"], "application/json");
    for name in ["api_key", "authorization", "cookie", "x-api-key"] {
        assert!(out.removed.contains(&name.to_string()), "{:?}", out.removed);
    }
    assert_eq!(out.skipped["a body with a secret-looking field"], 2);
}

#[test]
fn captures_import_too_and_a_recorded_failure_is_expected() {
    let doc = json!({ "requests": [
        { "method": "GET", "path": "/status/503", "query": null, "headers": [], "status": 503 },
        { "method": "POST", "path": "/up", "headers": [], "body_base64": "AAEC", "body_size": 3 },
        { "method": "POST", "path": "/cut", "headers": [], "body": "abc", "body_truncated": true }
    ] });
    let out = import(&doc).expect("imported");
    assert_eq!(out.requests.len(), 2, "{out:?}");
    let failed = out
        .requests
        .iter()
        .find(|t| t.path == "/status/503")
        .expect("kept");
    assert_eq!(failed.expect_status, vec![503]);
    assert_eq!(out.skipped["a binary or cut body"], 1);
}

#[test]
fn a_recording_is_bounded_and_refused_when_empty_or_foreign() {
    let many: Vec<Value> = (0..MAX_TEMPLATES + 50)
        .map(|i| entry("GET", &format!("https://a.test/p/{i}")))
        .collect();
    let out = import(&har(many)).expect("imported");
    assert_eq!(out.requests.len(), MAX_TEMPLATES);
    assert_eq!(out.skipped["rarer than the 200 kept"], 50);

    let flood: Vec<Value> = (0..MAX_ENTRIES + 10)
        .map(|_| entry("GET", "https://a.test/same"))
        .collect();
    assert_eq!(import(&har(flood)).expect("imported").entries, MAX_ENTRIES);

    assert!(import(&json!({ "hello": 1 })).is_err());
    let ftp = har(vec![entry("GET", "ftp://a.test/x")]);
    assert!(import(&ftp).unwrap_err().contains("nothing to replay"));
    let huge = har(vec![{
        let mut e = entry("POST", "https://a.test/big");
        e["request"]["postData"] = json!({ "text": "x".repeat(MAX_BODY_BYTES + 1) });
        e
    }]);
    assert!(import(&huge).is_err());
}
