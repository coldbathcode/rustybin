use serde_json::{json, Value};

use super::{split_path_query, AuthDef, BodyDef, Category};

pub fn build(categories: &[Category], base_url: &str) -> Value {
    let version = env!("CARGO_PKG_VERSION");
    let mut entries: Vec<Value> = Vec::new();

    for cat in categories {
        for req in &cat.requests {
            entries.push(build_entry(req, cat.name, base_url));
        }
    }

    json!({
        "log": {
            "version": "1.2",
            "creator": {
                "name": "Rustybin",
                "version": version
            },
            "entries": entries
        }
    })
}

fn build_entry(req: &super::RequestDef, category: &str, base_url: &str) -> Value {
    let (_path, query_string) = split_path_query(req.path);
    let url = format!("{base_url}{}", req.path);

    let mut headers: Vec<Value> = req
        .headers
        .iter()
        .map(|(k, v)| json!({ "name": *k, "value": *v }))
        .collect();

    // Auth headers
    if let Some(ref auth) = req.auth {
        match auth {
            AuthDef::Basic { user, pass } => {
                let encoded = base64_encode(&format!("{user}:{pass}"));
                headers
                    .push(json!({ "name": "Authorization", "value": format!("Basic {encoded}") }));
            }
            AuthDef::Bearer(token) => {
                headers
                    .push(json!({ "name": "Authorization", "value": format!("Bearer {token}") }));
            }
        }
    }

    // Query string params
    let query_params: Vec<Value> = if query_string.is_empty() {
        vec![]
    } else {
        query_string
            .split('&')
            .map(|pair| {
                let mut parts = pair.splitn(2, '=');
                let name = parts.next().unwrap_or_default();
                let value = parts.next().unwrap_or_default();
                json!({ "name": name, "value": value })
            })
            .collect()
    };

    // PostData
    let post_data = match &req.body {
        Some(BodyDef::Json(raw)) => json!({
            "mimeType": "application/json",
            "text": *raw
        }),
        Some(BodyDef::Form(params)) => {
            let form_params: Vec<Value> = params
                .iter()
                .map(|(k, v)| json!({ "name": *k, "value": *v }))
                .collect();
            json!({
                "mimeType": "application/x-www-form-urlencoded",
                "params": form_params
            })
        }
        Some(BodyDef::Xml(raw)) => json!({
            "mimeType": "text/xml",
            "text": *raw
        }),
        None => Value::Null,
    };

    let mut request = json!({
        "method": req.method,
        "url": url,
        "httpVersion": "HTTP/1.1",
        "headers": headers,
        "queryString": query_params,
        "cookies": [],
        "headersSize": -1,
        "bodySize": -1
    });

    if !post_data.is_null() {
        request["postData"] = post_data;
    }

    json!({
        "comment": format!("[{}] {}", category, req.name),
        "request": request,
        "response": {
            "status": 200,
            "statusText": "OK",
            "httpVersion": "HTTP/1.1",
            "headers": [],
            "cookies": [],
            "content": { "size": 0, "mimeType": "application/json" },
            "redirectURL": "",
            "headersSize": -1,
            "bodySize": -1
        },
        "cache": {},
        "timings": {
            "send": 0,
            "wait": 0,
            "receive": 0
        },
        "startedDateTime": "2024-01-01T00:00:00.000Z",
        "time": 0
    })
}

fn base64_encode(input: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input.as_bytes())
}
