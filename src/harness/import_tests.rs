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
    // What is kept is what a layer takes: more would not validate.
    assert_eq!(
        MAX_TEMPLATES,
        crate::harness::scenario::MAX_REQUESTS_PER_LAYER
    );
    assert_eq!(out.skipped["rarer than the 64 kept"], 50);

    let flood: Vec<Value> = (0..MAX_ENTRIES + 10)
        .map(|_| entry("GET", "https://a.test/same"))
        .collect();
    let flooded = import(&har(flood)).expect("imported");
    assert_eq!(flooded.entries, MAX_ENTRIES);
    // A weight never passes what a template takes.
    assert_eq!(flooded.requests[0].weight, 1000);
    assert_eq!(flooded.notes.len(), 1, "{:?}", flooded.notes);

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

// ── Postman collections and Artillery scripts (core next step 13) ───

/// A v2.1 collection: a folder, scripts, an auth block, variables.
fn collection() -> Value {
    json!({
        "info": {
            "name": "Orders",
            "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
        },
        "auth": { "type": "bearer", "bearer": [{ "key": "token", "value": SECRET }] },
        "variable": [
            { "key": "version", "value": "v2" },
            { "key": "apiKey", "value": SECRET }
        ],
        "event": [{ "listen": "prerequest", "script": { "exec": ["pm.environment.set('x', 1)"] } }],
        "item": [
            {
                "name": "Orders",
                "item": [
                    {
                        "name": "List",
                        "event": [{ "listen": "test", "script": { "exec": ["pm.test('ok', () => {})"] } }],
                        "request": {
                            "method": "GET",
                            "header": [
                                { "key": "Accept", "value": "application/json" },
                                { "key": "X-Api-Key", "value": "{{apiKey}}" },
                                { "key": "X-Off", "value": "1", "disabled": true }
                            ],
                            "url": {
                                "raw": "{{baseUrl}}/{{version}}/orders?page=1&token={{token}}",
                                "host": ["{{baseUrl}}"],
                                "path": ["{{version}}", "orders"],
                                "query": [
                                    { "key": "page", "value": "1" },
                                    { "key": "token", "value": "{{token}}" },
                                    { "key": "off", "value": "1", "disabled": true }
                                ]
                            }
                        }
                    },
                    {
                        "name": "Create",
                        "request": {
                            "method": "POST",
                            "header": [{ "key": "Content-Type", "value": "application/json" }],
                            "body": { "mode": "raw", "raw": "{\"item\": \"tea\", \"api\": \"{{version}}\"}" },
                            "url": { "raw": "https://api.example.com/v2/orders", "protocol": "https",
                                "host": ["api", "example", "com"], "path": ["v2", "orders"] }
                        }
                    }
                ]
            },
            {
                "name": "One order",
                "request": {
                    "method": "GET",
                    "header": [],
                    "url": { "raw": "{{baseUrl}}/v2/orders/:id", "host": ["{{baseUrl}}"],
                        "path": ["v2", "orders", ":id"], "variable": [{ "key": "id", "value": "42" }] }
                }
            },
            {
                "name": "Customer",
                "request": {
                    "method": "GET",
                    "header": [],
                    "url": { "raw": "{{baseUrl}}/customers/{{customerId}}", "host": ["{{baseUrl}}"],
                        "path": ["customers", "{{customerId}}"] }
                }
            },
            {
                "name": "Upload",
                "request": {
                    "method": "POST",
                    "header": [],
                    "body": { "mode": "formdata", "formdata": [{ "key": "file", "type": "file", "src": "a.png" }] },
                    "url": "https://api.example.com/upload"
                }
            }
        ]
    })
}

#[test]
fn a_postman_collection_becomes_weighted_requests() {
    let out = import(&collection()).expect("imported");
    assert_eq!(out.format, Format::Postman);
    assert_eq!(out.entries, 5);
    assert_eq!(out.requests.len(), 3, "{out:?}");
    let list = out
        .requests
        .iter()
        .find(|t| t.path == "/v2/orders?page=1")
        .expect("the list, its variable put in and its token removed");
    assert_eq!(list.method, "GET");
    assert_eq!(list.headers.len(), 1, "{:?}", list.headers);
    assert_eq!(list.headers["accept"], "application/json");
    let create = out
        .requests
        .iter()
        .find(|t| t.method == "POST")
        .expect("the create");
    assert_eq!(create.path, "/v2/orders");
    assert_eq!(create.body, Some(json!({ "item": "tea", "api": "v2" })));
    assert!(out.requests.iter().any(|t| t.path == "/v2/orders/42"));
    // A scenario names its own target.
    assert_eq!(out.hosts, vec!["api.example.com"]);
}

#[test]
fn postman_scripts_and_auth_are_named_never_run() {
    let out = import(&collection()).expect("imported");
    for named in [
        "collection.event (scripts)",
        "collection.auth (bearer)",
        "item[0].item[0].event (scripts)",
    ] {
        assert!(
            out.left_out.contains(&named.to_string()),
            "{named}: {:?}",
            out.left_out
        );
    }
    assert_eq!(out.skipped["a multipart or file body"], 1);
    // Nothing of a script is in the answer.
    let text = serde_json::to_string(&out).expect("json");
    assert!(!text.contains("pm."), "{text}");
}

#[test]
fn postman_variables_are_resolved_or_the_request_is_left_out() {
    let out = import(&collection()).expect("imported");
    // Defined by the collection: put in. Not defined: the request is out
    // and the variable named, never left for the harness to read.
    assert_eq!(out.skipped["an unresolved variable"], 1);
    assert_eq!(out.variables, vec!["customerId"]);
    let text = serde_json::to_string(&out.requests).expect("json");
    assert!(!text.contains("{{"), "{text}");
    // A harness placeholder of the same name is not an exception.
    let mut doc = collection();
    doc["item"] = json!([{ "name": "x", "request": { "method": "GET",
        "url": { "raw": "{{baseUrl}}/items/{{id}}", "host": ["{{baseUrl}}"], "path": ["items", "{{id}}"] } } }]);
    let err = import(&doc).unwrap_err();
    assert!(
        err.contains("nothing to import") && err.contains("id"),
        "{err}"
    );
    // Older collections are refused with the way out.
    let mut old = collection();
    old["info"]["schema"] =
        json!("https://schema.getpostman.com/json/collection/v2.0.0/collection.json");
    assert!(import(&old).unwrap_err().contains("v2.1"));
    let v1 = json!({ "id": "1", "name": "old", "order": ["a"], "requests": [{ "id": "a", "url": "https://a.test/x", "method": "GET" }] });
    assert!(import(&v1).unwrap_err().contains("v2.1"));
}

const SCRIPT: &str = r#"
config:
  target: "https://api.example.com/shop"
  phases:
    - duration: 60
      arrivalRate: 5
      name: warm up
    - duration: 120
      arrivalRate: 5
      rampTo: 20
    - pause: 10
    - duration: 30
      arrivalCount: 60
  defaults:
    headers:
      Accept: application/json
      X-Api-Key: "s3cr3t-value-that-must-not-survive"
  variables:
    region: ["eu"]
  http:
    timeout: 5
scenarios:
  - name: browse
    weight: 3
    flow:
      - get:
          url: "/items?region={{ region }}"
      - get:
          url: "/items/1"
          expect:
            - statusCode: [200, 404]
  - name: buy
    flow:
      - post:
          url: "/orders"
          json:
            item: tea
            region: "{{ region }}"
"#;

#[test]
fn an_artillery_script_maps_requests_and_phases() {
    let out = import_text(SCRIPT, None).expect("imported");
    assert_eq!(out.format, Format::Artillery);
    assert_eq!(out.requests.len(), 3, "{out:?}");
    assert_eq!(out.hosts, vec!["api.example.com"]);
    // The target's path leads, the variable is put in, the key is gone.
    let browse = out
        .requests
        .iter()
        .find(|t| t.path == "/shop/items?region=eu")
        .expect("browse");
    assert_eq!(browse.weight, 3);
    assert_eq!(browse.headers["accept"], "application/json");
    assert!(!browse.headers.contains_key("x-api-key"));
    assert!(out.removed.contains(&"x-api-key".to_string()));
    let item = out
        .requests
        .iter()
        .find(|t| t.path == "/shop/items/1")
        .expect("item");
    assert_eq!(item.expect_status, vec![200, 404]);
    let buy = out
        .requests
        .iter()
        .find(|t| t.method == "POST")
        .expect("buy");
    assert_eq!(buy.weight, 1);
    assert_eq!(buy.body, Some(json!({ "item": "tea", "region": "eu" })));
    assert_eq!(out.timeout, Some(Dur(Duration::from_secs(5))));
    // Arrivals run one flow each: (3 x 2 + 1 x 1) / 4 = 1.75 requests.
    let pattern = serde_json::to_value(out.pattern.as_ref().expect("pattern")).expect("json");
    assert_eq!(pattern["type"], "constant");
    assert_eq!(pattern["rate"], 8.75);
    assert_eq!(pattern["for"], "1m");
    assert_eq!(pattern["then"]["type"], "ramp");
    assert_eq!(pattern["then"]["from"], 8.75);
    assert_eq!(pattern["then"]["to"], 35.0);
    assert_eq!(pattern["then"]["over"], "2m");
    assert_eq!(pattern["then"]["then"]["rate"], 0.0);
    assert_eq!(pattern["then"]["then"]["for"], "10s");
    assert_eq!(pattern["then"]["then"]["then"]["rate"], 3.5);
    assert!(
        out.notes.iter().any(|n| n.contains("1.75")),
        "{:?}",
        out.notes
    );
    let text = serde_json::to_string(&out).expect("json");
    assert!(!text.contains(SECRET), "{text}");
}

#[test]
fn artillery_phases_are_all_or_nothing() {
    // A think time, a loop, code: each leaves the pattern out and says why.
    for (change, why) in [
        ("      - think: 2\n", "think"),
        (
            "      - loop:\n          - get:\n              url: /x\n        count: 3\n",
            "loop",
        ),
        ("      - function: \"setUp\"\n", "function"),
    ] {
        let script = SCRIPT.replace(
            "  - name: buy\n    flow:\n",
            &format!("  - name: buy\n    flow:\n{change}"),
        );
        let out = import_text(&script, None).expect("imported");
        assert!(out.pattern.is_none(), "{why}");
        assert!(
            out.left_out
                .iter()
                .any(|l| l.starts_with("config.phases") && l.contains(why)),
            "{why}: {:?}",
            out.left_out
        );
        // The requests still come.
        assert_eq!(out.requests.len(), 3);
    }
    // A cap on users, a rate that is not a number, a request left out.
    let capped = SCRIPT.replace("      name: warm up\n", "      maxVusers: 10\n");
    let out = import_text(&capped, None).expect("imported");
    assert!(out.pattern.is_none());
    assert!(out
        .left_out
        .iter()
        .any(|l| l.contains("phases[0].maxVusers")));
    let templated = SCRIPT.replace(
        "arrivalRate: 5\n      name",
        "arrivalRate: \"{{ $environment.RATE }}\"\n      name",
    );
    let out = import_text(&templated, None).expect("imported");
    assert!(out.pattern.is_none());
    let unresolved = SCRIPT.replace("url: \"/items/1\"", "url: \"/items/{{ itemId }}\"");
    let out = import_text(&unresolved, None).expect("imported");
    assert!(out.pattern.is_none());
    assert_eq!(out.variables, vec!["itemId"]);
    assert_eq!(out.skipped["an unresolved variable"], 1);
}

#[test]
fn artillery_code_and_plugins_are_named_never_run() {
    let script = r#"
config:
  target: "http://localhost:3000"
  processor: "./functions.js"
  plugins:
    expect: {}
  payload:
    path: users.csv
    fields: [user]
  environments:
    staging: { target: "https://staging.example.com" }
  ensure:
    p99: 200
  tls:
    rejectUnauthorized: false
  variables:
    colour: [red, green]
before:
  flow:
    - function: seed
scenarios:
  - beforeScenario: setUp
    flow:
      - function: "pickUser"
      - get:
          url: "/items"
          beforeRequest: sign
          afterResponse: check
          capture:
            json: "$.id"
            as: id
      - log: "got {{ id }}"
      - post:
          url: "/login"
          auth: { user: a, pass: b }
          cookie: { sid: abc }
          json: { ok: true }
  - engine: ws
    flow:
      - send: hello
"#;
    let out = import_text(script, None).expect("imported");
    for named in [
        "config.processor (code, never run)",
        "config.plugins",
        "config.payload",
        "config.environments",
        "config.ensure",
        "config.tls",
        "config.variables.colour (several values)",
        "before",
        "scenarios[0].beforeScenario (code, never run)",
        "scenarios[0].flow[0].function (code, never run)",
        "scenarios[0].flow[1].beforeRequest (code, never run)",
        "scenarios[0].flow[1].afterResponse (code, never run)",
        "scenarios[0].flow[1].capture",
        "scenarios[0].flow[2].log",
        "scenarios[1] (engine ws)",
    ] {
        assert!(
            out.left_out.contains(&named.to_string()),
            "{named}: {:#?}",
            out.left_out
        );
    }
    assert_eq!(out.requests.len(), 2, "{out:?}");
    // The step's credentials are removed and named, not carried.
    for name in ["authorization", "cookie"] {
        assert!(out.removed.contains(&name.to_string()), "{:?}", out.removed);
    }
    let text = serde_json::to_string(&out.requests).expect("json");
    assert!(!text.contains("abc") && !text.contains("pass"), "{text}");
    assert!(out.pattern.is_none());
}

#[test]
fn no_credential_survives_a_collection_or_a_script() {
    // The collection's bearer token, its secret variable and a secret field.
    let mut doc = collection();
    doc["item"][0]["item"][1]["request"]["body"]["raw"] =
        json!(format!("{{\"password\": \"{SECRET}\"}}"));
    doc["item"][1]["request"]["header"] =
        json!([{ "key": "Authorization", "value": format!("Bearer {SECRET}") }]);
    doc["item"][1]["request"]["url"]["query"] = json!([
        { "key": "access_token", "value": SECRET },
        { "key": "x", "value": "{{apiKey}}" }
    ]);
    let out = import(&doc).expect("imported");
    let text = serde_json::to_string(&out).expect("json");
    assert!(!text.contains(SECRET), "{text}");
    assert_eq!(out.skipped["a body with a secret-looking field"], 1);
    // A secret-looking variable is never put in: its request is left out.
    assert!(out.variables.contains(&"apiKey".to_string()));
    for name in ["x-api-key", "token"] {
        assert!(
            out.removed.contains(&name.to_string()),
            "{name}: {:?}",
            out.removed
        );
    }
    // A script: a form with a secret field, a secret query parameter.
    let script = format!(
        "config:\n  target: http://a.test\nscenarios:\n  - flow:\n      - post:\n          url: /token\n          form:\n            client_secret: {SECRET}\n      - get:\n          url: /x?api_key={SECRET}&page=2\n          qs:\n            signature: {SECRET}\n"
    );
    let out = import_text(&script, None).expect("imported");
    let text = serde_json::to_string(&out).expect("json");
    assert!(!text.contains(SECRET), "{text}");
    assert_eq!(out.requests.len(), 1);
    assert_eq!(out.requests[0].path, "/x?page=2");
    for name in ["api_key", "signature"] {
        assert!(
            out.removed.contains(&name.to_string()),
            "{name}: {:?}",
            out.removed
        );
    }
}

#[test]
fn a_program_is_refused_with_the_reason() {
    for program in [
        "import http from 'k6/http';\n\nexport default function () {\n  http.get('https://test.example.com');\n}\n",
        "from locust import HttpUser, task\n\nclass User(HttpUser):\n    @task\n    def index(self):\n        self.client.get(\"/\")\n",
        "module.exports = { setUp: function (context, events, done) { return done(); } };\n",
        "just some words",
        "",
    ] {
        let err = import_text(program, None).unwrap_err();
        assert!(
            err.contains("never run or translated"),
            "{program:?}: {err}"
        );
    }
    // A JSON document that is none of the formats names them.
    let err = import_text("{\"hello\": 1}", None).unwrap_err();
    assert!(
        err.contains("postman") && err.contains("artillery"),
        "{err}"
    );
    // The named format is what the document is read as.
    let err = import_text("{\"hello\": 1}", Some(Format::Artillery)).unwrap_err();
    assert!(err.contains("scenarios"), "{err}");
    assert!(Format::parse("k6").is_none());
}

#[test]
fn collections_and_scripts_are_bounded() {
    // Folders past the depth are named, not walked.
    let mut folder =
        json!({ "name": "leaf", "request": { "method": "GET", "url": "https://a.test/deep" } });
    for i in 0..MAX_FOLDER_DEPTH + 2 {
        folder = json!({ "name": format!("f{i}"), "item": [folder] });
    }
    let mut doc = collection();
    doc["item"].as_array_mut().expect("items").push(folder);
    let out = import(&doc).expect("imported");
    assert!(!out.requests.iter().any(|t| t.path == "/deep"));
    assert!(out.left_out.iter().any(|l| l.contains("nested deeper")));
    // The names kept are bounded.
    let many: Vec<Value> = (0..MAX_NAMES + 20)
        .map(|i| {
            json!({ "name": "r", "event": [{ "listen": "test" }],
                "request": { "method": "GET", "url": format!("https://a.test/{{{{v{i}}}}}") } })
        })
        .chain([
            json!({ "name": "ok", "request": { "method": "GET", "url": "https://a.test/ok" } }),
        ])
        .collect();
    doc["item"] = json!(many);
    let out = import(&doc).expect("imported");
    assert_eq!(out.left_out.len(), MAX_NAMES);
    assert_eq!(out.variables.len(), MAX_NAMES);
    // The requests read and kept are bounded as a recording's are.
    let flood: Vec<Value> = (0..MAX_ENTRIES + 10)
        .map(|i| {
            json!({ "name": "r", "request": { "method": "GET", "url": format!("https://a.test/p/{i}") } })
        })
        .collect();
    doc["item"] = json!(flood);
    let out = import(&doc).expect("imported");
    assert_eq!(out.entries, MAX_ENTRIES);
    assert_eq!(out.requests.len(), MAX_TEMPLATES);
    // A script over the size is refused before it is parsed as YAML.
    let big = format!(
        "config:\n  target: http://a.test\n# {}\n",
        "x".repeat(MAX_TEXT_BYTES)
    );
    assert!(import_text(&big, None)
        .unwrap_err()
        .contains("at most 4 MiB"));
}

#[test]
fn an_imported_source_validates_as_a_scenario() {
    use crate::harness::scenario::{Caps, Scenario};
    let mut odd = collection();
    odd["item"].as_array_mut().expect("items").extend([
        json!({ "name": "control", "request": { "method": "GET", "url": "https://a.test/_rustybin/config" } }),
        json!({ "name": "method", "request": { "method": "PROPFIND", "url": "https://a.test/dav" } }),
        json!({ "name": "space", "request": { "method": "GET",
            "url": { "raw": "https://a.test/a b", "host": ["a", "test"], "path": ["a b"] } } }),
        json!({ "name": "header", "request": { "method": "GET",
            "header": [{ "key": "bad name", "value": "1" }], "url": "https://a.test/h" } }),
    ]);
    for out in [
        import(&odd).expect("collection"),
        import_text(SCRIPT, None).expect("script"),
    ] {
        let mut value = json!({
            "name": "imported",
            "target": "http://127.0.0.1:8080",
            "source": { "requests": out.requests },
            "pattern": out.pattern.as_ref().map_or(
                json!({ "type": "constant", "rate": 1, "for": "1s" }),
                |p| json!(p)
            ),
        });
        if let Some(t) = &out.timeout {
            value["timeout"] = json!(t);
        }
        let mut scenario = Scenario::from_value(&value).expect("a scenario");
        scenario.normalise();
        let problems = scenario.validate(&Caps::default());
        assert!(problems.is_empty(), "{problems:#?}");
    }
    let out = import(&odd).expect("collection");
    for reason in [
        "a control plane path",
        "a method the harness does not send",
        "a path that is not a URL path",
        "a header that is not a header",
    ] {
        assert_eq!(out.skipped[reason], 1, "{reason}: {:?}", out.skipped);
    }
}

// ── Variables given by the caller (core next step 18) ───────────────

fn given(v: Value) -> Variables {
    Variables::from_value(&v).expect("variables")
}

/// A collection as the app exports it when its base URL and ids live in an
/// environment: nothing of it is defined in the document.
fn environment_collection() -> Value {
    json!({
        "info": { "name": "Shop",
            "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json" },
        "item": [
            { "name": "A user", "request": { "method": "GET", "header": [],
                "url": { "raw": "{{baseUrl}}/users/{{userId}}", "host": ["{{baseUrl}}"],
                    "path": ["users", "{{userId}}"] } } },
            { "name": "Its orders", "request": { "method": "GET",
                "header": [{ "key": "X-Tenant", "value": "{{tenant}}" }],
                "url": { "raw": "{{baseUrl}}/users/{{userId}}/orders/:order?state={{state}}",
                    "host": ["{{baseUrl}}"], "path": ["users", "{{userId}}", "orders", ":order"],
                    "query": [{ "key": "state", "value": "{{state}}" }] } } },
            { "name": "Create", "request": { "method": "POST",
                "header": [{ "key": "Content-Type", "value": "application/json" }],
                "body": { "mode": "raw", "raw": "{\"user\": \"{{userId}}\", \"tenant\": \"{{tenant}}\"}" },
                "url": "{{baseUrl}}/users/{{userId}}/orders" } }
        ]
    })
}

#[test]
fn a_collection_with_an_environment_imports_whole_with_its_variables() {
    // Without the environment nothing imports, and every variable is named.
    let err = import(&environment_collection()).unwrap_err();
    assert!(err.contains("nothing to import"), "{err}");
    // Every one a request needs, not only the first it meets.
    for name in ["userId", "tenant", "state", ":order"] {
        assert!(err.contains(name), "{name}: {err}");
    }
    // With it everything does: the base URL's path leads the requests and
    // its host is listed, as any host of the document is.
    let vars = given(json!({
        "baseUrl": "https://api.example.com/v1/",
        "userId": 7,
        "tenant": "acme",
        "state": "open",
        ":order": "o-1"
    }));
    let out = import_given(&environment_collection(), None, &[], &vars).expect("imported");
    assert_eq!(out.entries, 3);
    assert_eq!(out.requests.len(), 3, "{out:?}");
    assert!(out.skipped.is_empty(), "{:?}", out.skipped);
    assert!(out.variables.is_empty(), "{:?}", out.variables);
    assert_eq!(out.hosts, vec!["api.example.com"]);
    let paths: Vec<&str> = out.requests.iter().map(|t| t.path.as_str()).collect();
    for path in [
        "/v1/users/7",
        "/v1/users/7/orders/o-1?state=open",
        "/v1/users/7/orders",
    ] {
        assert!(paths.contains(&path), "{path}: {paths:?}");
    }
    let create = out
        .requests
        .iter()
        .find(|t| t.method == "POST")
        .expect("the create");
    assert_eq!(create.body, Some(json!({ "user": "7", "tenant": "acme" })));
    let orders = out
        .requests
        .iter()
        .find(|t| t.path.contains("o-1"))
        .expect("the orders");
    assert_eq!(orders.headers["x-tenant"], "acme");
    assert!(out.notes.is_empty(), "{:?}", out.notes);

    // An Artillery script's URL that starts with a variable is a path: the
    // same holds there.
    let script = "config:\n  target: https://gw.test\nscenarios:\n  - flow:\n      - get:\n          url: \"{{ baseUrl }}/orders\"\n      - get:\n          url: \"{{ baseUrl }}/orders/{{ orderId }}\"\n";
    let err = import_text(script, None).unwrap_err();
    assert!(err.contains("baseUrl") && err.contains("orderId"), "{err}");
    let vars = given(json!({ "baseUrl": "/v1", "orderId": "42" }));
    let out = import_text_given(script, None, &[], &vars).expect("imported");
    assert_eq!(out.requests.len(), 2, "{out:?}");
    assert!(out.requests.iter().any(|t| t.path == "/v1/orders/42"));
    assert!(out.variables.is_empty());
}

#[test]
fn what_is_still_undefined_stays_named_and_what_is_unused_is_said() {
    let vars = given(json!({ "baseUrl": "https://api.example.com", "userId": "7", "extra": "x" }));
    let out = import_given(&environment_collection(), None, &[], &vars).expect("imported");
    // The user imports; the two that need more are still out, by name.
    assert_eq!(out.requests.len(), 1, "{out:?}");
    assert_eq!(out.requests[0].path, "/users/7");
    assert_eq!(out.skipped["an unresolved variable"], 2);
    assert_eq!(out.variables, vec![":order", "state", "tenant"]);
    assert!(
        out.notes
            .iter()
            .any(|n| n == "variables: given and not used by the document: extra"),
        "{:?}",
        out.notes
    );
    // A host's variable without a value is dropped with the host, as it
    // was, and a note says a value may be given.
    let out = import(&collection()).expect("imported");
    assert!(
        out.notes
            .iter()
            .any(|n| n.starts_with("hosts:") && n.contains("baseUrl")),
        "{:?}",
        out.notes
    );
    // A recording has no variables.
    let err = import_given(
        &har(vec![entry("GET", "https://a.test/x")]),
        None,
        &[],
        &given(json!({ "a": "b" })),
    )
    .unwrap_err();
    assert!(err.contains("a recording (har) has no variables"), "{err}");
}

#[test]
fn a_given_value_wins_over_the_documents() {
    // The collection says version is v2.
    let out = import_given(&collection(), None, &[], &given(json!({ "version": "v3" })))
        .expect("imported");
    assert!(
        out.requests.iter().any(|t| t.path == "/v3/orders?page=1"),
        "{out:?}"
    );
    let create = out
        .requests
        .iter()
        .find(|t| t.method == "POST")
        .expect("the create");
    assert_eq!(create.body, Some(json!({ "item": "tea", "api": "v3" })));
    // A path variable's own value too.
    let out =
        import_given(&collection(), None, &[], &given(json!({ ":id": "77" }))).expect("imported");
    assert!(out.requests.iter().any(|t| t.path == "/v2/orders/77"));
    assert!(!out.requests.iter().any(|t| t.path == "/v2/orders/42"));
    // A script's variable with one value, and one with several, which the
    // document alone leaves out.
    let several = SCRIPT.replace("region: [\"eu\"]", "region: [\"eu\", \"us\"]");
    let alone = import_text(&several, None).expect("imported");
    assert!(alone
        .left_out
        .contains(&"config.variables.region (several values)".to_string()));
    assert_eq!(alone.variables, vec!["region"]);
    for script in [SCRIPT.to_string(), several] {
        let out = import_text_given(&script, None, &[], &given(json!({ "region": "ap" })))
            .expect("imported");
        assert!(
            out.requests
                .iter()
                .any(|t| t.path == "/shop/items?region=ap"),
            "{out:?}"
        );
        assert!(out.variables.is_empty());
        assert!(!out.left_out.iter().any(|l| l.contains("region")));
        assert!(out.pattern.is_some(), "every request kept: {out:?}");
    }
}

#[test]
fn a_harness_placeholder_is_never_a_variable() {
    for name in crate::harness::scenario::PLACEHOLDERS {
        let err = Variables::from_value(&json!({ *name: "1" })).unwrap_err();
        assert!(
            err.starts_with(&format!("variables.{name}:")) && err.contains("placeholder"),
            "{err}"
        );
    }
    let err = Variables::from_value(&json!({ " seq ": "1" })).unwrap_err();
    assert!(err.starts_with("variables.seq:"), "{err}");
    // A Postman path variable is not a placeholder, whatever its name.
    assert!(Variables::from_value(&json!({ ":id": "1" })).is_ok());
    // A value cannot bring a placeholder in whole,
    let err = Variables::from_value(&json!({ "n": "{{vu}}" })).unwrap_err();
    assert!(
        err.starts_with("variables.n:") && !err.contains("vu"),
        "{err}"
    );
    // nor in pieces: what reads as one after the values are in is left out.
    let mut doc = environment_collection();
    doc["item"] = json!([
        { "name": "pieces", "request": { "method": "GET", "url": "https://a.test/n/{{a}}{{a}}seq}}" } },
        { "name": "plain", "request": { "method": "GET", "url": "https://a.test/n/{{b}}" } }
    ]);
    let out =
        import_given(&doc, None, &[], &given(json!({ "a": "{", "b": "1" }))).expect("imported");
    assert_eq!(out.requests.len(), 1, "{out:?}");
    assert_eq!(out.requests[0].path, "/n/1");
    assert_eq!(out.skipped["text that reads as a placeholder"], 1);
}

#[test]
fn a_given_value_is_removed_where_a_credential_would_be() {
    let mut doc = environment_collection();
    doc["item"] = json!([
        { "name": "header", "request": { "method": "GET",
            "header": [
                { "key": "Authorization", "value": "Bearer {{who}}" },
                { "key": "X-Api-Key", "value": "{{who}}" },
                { "key": "Accept", "value": "application/json" }
            ],
            "url": { "raw": "{{baseUrl}}/a?access_token={{who}}&page=2", "host": ["{{baseUrl}}"], "path": ["a"],
                "query": [{ "key": "access_token", "value": "{{who}}" }, { "key": "page", "value": "2" }] } } },
        { "name": "body", "request": { "method": "POST", "header": [],
            "body": { "mode": "raw", "raw": "{\"password\": \"{{who}}\"}" },
            "url": "{{baseUrl}}/login" } },
        { "name": "form", "request": { "method": "POST", "header": [],
            "body": { "mode": "urlencoded", "urlencoded": [{ "key": "client_secret", "value": "{{who}}" }] },
            "url": "{{baseUrl}}/token" } }
    ]);
    let vars =
        given(json!({ "baseUrl": format!("https://user:{SECRET}@a.test/v1"), "who": SECRET }));
    let out = import_given(&doc, None, &[], &vars).expect("imported");
    let text = serde_json::to_string(&out).expect("json");
    assert!(!text.contains(SECRET), "{text}");
    assert_eq!(out.requests.len(), 1, "{text}");
    assert_eq!(out.requests[0].path, "/v1/a?page=2");
    assert_eq!(out.requests[0].headers.len(), 1, "{text}");
    for name in ["authorization", "x-api-key", "access_token"] {
        assert!(out.removed.contains(&name.to_string()), "{:?}", out.removed);
    }
    assert_eq!(out.skipped["a body with a secret-looking field"], 2);
    // The same in a script.
    let script = "config:\n  target: http://a.test\n  defaults:\n    headers:\n      Authorization: \"Bearer {{ who }}\"\nscenarios:\n  - flow:\n      - get:\n          url: \"/x?api_key={{ who }}&page=2\"\n      - post:\n          url: /login\n          json:\n            password: \"{{ who }}\"\n";
    let out =
        import_text_given(script, None, &[], &given(json!({ "who": SECRET }))).expect("imported");
    let text = serde_json::to_string(&out).expect("json");
    assert!(!text.contains(SECRET), "{text}");
    assert_eq!(out.requests.len(), 1, "{text}");
    assert_eq!(out.requests[0].path, "/x?page=2");
    // A name that looks like a credential is refused, and no refusal, nor
    // the variables' own Debug, shows a value.
    let long = format!("{SECRET}{}", "x".repeat(MAX_VARIABLE_BYTES));
    for (bad, why) in [
        (
            json!({ "apiKey": SECRET }),
            "variables.apiKey: a name that looks like a credential",
        ),
        (
            json!({ "token": SECRET }),
            "variables.token: a name that looks like a credential",
        ),
        (
            json!({ "a": long }),
            "variables.a: a value is at most 1024 bytes",
        ),
        (
            json!({ "a": format!("{SECRET}{{{{x}}}}") }),
            "variables.a: a value does not hold",
        ),
        (json!({ "a": [SECRET] }), "variables.a: a value is text"),
        (json!([SECRET]), "variables: a map of name to value"),
    ] {
        let err = Variables::from_value(&bad).unwrap_err();
        assert!(err.starts_with(why), "{why}: {err}");
        assert!(!err.contains(SECRET), "{err}");
    }
    assert!(!format!("{vars:?}").contains(SECRET), "{vars:?}");
    // The refusal of an import that imports nothing has names only.
    doc["item"]
        .as_array_mut()
        .expect("items")
        .retain(|i| i["name"] != "header");
    let err = import_given(&doc, None, &[], &vars).unwrap_err();
    assert!(err.contains("nothing to import"), "{err}");
    assert!(!err.contains(SECRET), "{err}");
}

#[test]
fn given_variables_are_capped() {
    let many = |n: usize| {
        Value::Object(
            (0..n)
                .map(|i| (format!("v{i}"), json!("x")))
                .collect::<Map<String, Value>>(),
        )
    };
    assert!(Variables::from_value(&many(MAX_VARIABLES)).is_ok());
    let err = Variables::from_value(&many(MAX_VARIABLES + 1)).unwrap_err();
    assert_eq!(err, "variables: at most 64");
    assert!(Variables::from_value(&json!({ "a": "x".repeat(MAX_VARIABLE_BYTES) })).is_ok());
    let err =
        Variables::from_value(&json!({ "a": "x".repeat(MAX_VARIABLE_BYTES + 1) })).unwrap_err();
    assert_eq!(err, "variables.a: a value is at most 1024 bytes");
    // A name is one the document could write.
    let long = "n".repeat(MAX_VARIABLE_NAME + 1);
    for name in ["", "  ", "a{{b", "a}}b", "a\nb", long.as_str()] {
        let err = Variables::from_value(&json!({ name: "x" })).unwrap_err();
        assert!(
            err.starts_with("variables: a name is 1 to 64 characters"),
            "{err}"
        );
    }
    assert!(Variables::from_value(&json!({ "n".repeat(MAX_VARIABLE_NAME): "x" })).is_ok());
    assert!(Variables::from_value(&Value::Null)
        .expect("none")
        .is_empty());
}

#[test]
fn a_request_body_is_the_document_or_an_envelope() {
    let doc = environment_collection();
    // Bare, as before.
    let err = import_body(&doc.to_string(), None, &[]).unwrap_err().error;
    assert!(err.contains("nothing to import"), "{err}");
    // An envelope: the document itself, or its text.
    let variables = json!({ "baseUrl": "https://api.example.com", "userId": "7", "tenant": "t",
        "state": "open", ":order": "1" });
    for document in [doc.clone(), json!(doc.to_string())] {
        let body = json!({ "document": document, "variables": variables });
        let out = import_body(&body.to_string(), None, &[]).expect("imported");
        assert_eq!(out.format, Format::Postman);
        assert_eq!(out.requests.len(), 3, "{out:?}");
    }
    // A script as text, its format named in the envelope (which wins) or
    // beside it.
    let script = "config:\n  target: http://a.test\nscenarios:\n  - flow:\n      - get:\n          url: \"/items/{{ item }}\"\n";
    let body = json!({ "document": script, "format": "artillery", "variables": { "item": 3 } });
    let out = import_body(&body.to_string(), Some("postman"), &[]).expect("imported");
    assert_eq!(out.format, Format::Artillery);
    assert_eq!(out.requests[0].path, "/items/3");
    let body = json!({ "document": script });
    assert!(import_body(&body.to_string(), Some("postman"), &[])
        .unwrap_err()
        .error
        .contains("v2.1"));
    assert!(import_body(&body.to_string(), Some("artillery"), &[])
        .unwrap_err()
        .error
        .contains("variables not defined: item"));
    // What an envelope refuses: another key, a document that is neither,
    // variables that are no map, a format that is none.
    for (body, why) in [
        (
            json!({ "document": script, "variable": {} }),
            "variable: an import takes document, format and variables",
        ),
        (
            json!({ "document": 4 }),
            "document: the document, or its text",
        ),
        (
            json!({ "document": script, "variables": ["a"] }),
            "variables: a map of name to value",
        ),
        (
            json!({ "document": script, "format": "k6" }),
            "format: one of",
        ),
        (json!({ "document": script, "format": 4 }), "format: one of"),
        (
            json!({ "document": script, "variables": { "seq": "1" } }),
            "variables.seq:",
        ),
    ] {
        let err = import_body(&body.to_string(), None, &[]).unwrap_err().error;
        assert!(err.starts_with(why), "{why}: {err}");
    }
    // A document that says "document" somewhere is still a document.
    let mut recording = entry("GET", "https://a.test/x");
    recording["request"]["headers"] = json!([{ "name": "X-Kind", "value": "document" }]);
    let mut recording = har(vec![recording]);
    recording["document"] = json!("a field of its own");
    let out = import_body(&recording.to_string(), None, &[]).expect("a recording");
    assert_eq!(out.format, Format::Har);
    // The size bound of a collection holds inside an envelope too.
    let mut big = environment_collection();
    big["info"]["description"] = json!("x".repeat(MAX_TEXT_BYTES));
    let body = json!({ "document": big, "variables": variables });
    assert!(import_body(&body.to_string(), None, &[])
        .unwrap_err()
        .error
        .contains("at most 4 MiB"));
}
