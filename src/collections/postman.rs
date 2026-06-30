use serde_json::{json, Value};

use super::{AuthDef, BodyDef, Category, RequestDef};

pub fn build(categories: &[Category]) -> Value {
    let version = env!("CARGO_PKG_VERSION");
    json!({
        "info": {
            "name": "Rustybin",
            "description": format!("Rustybin v{version} — High-performance HTTP stub service for API gateway testing.\n\nSet the {{{{base_url}}}} variable to your Rustybin instance (default: http://localhost)."),
            "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json",
            "version": version
        },
        "variable": [
            { "key": "base_url", "value": "http://localhost", "type": "string" }
        ],
        "item": categories.iter().map(build_folder).collect::<Vec<_>>()
    })
}

fn build_folder(cat: &Category) -> Value {
    json!({
        "name": cat.name,
        "item": cat.requests.iter().map(build_request).collect::<Vec<_>>()
    })
}

fn build_request(req: &RequestDef) -> Value {
    let path = req.path;
    let path_segments: Vec<&str> = path
        .split('?')
        .next()
        .unwrap_or(path)
        .trim_start_matches('/')
        .split('/')
        .collect();

    let mut headers: Vec<Value> = req
        .headers
        .iter()
        .map(|(k, v)| json!({ "key": *k, "value": *v }))
        .collect();

    // Auth adds headers or auth block
    let mut auth_block = Value::Null;
    if let Some(ref auth) = req.auth {
        match auth {
            AuthDef::Basic { user, pass } => {
                auth_block = json!({
                    "type": "basic",
                    "basic": [
                        { "key": "username", "value": *user },
                        { "key": "password", "value": *pass }
                    ]
                });
            }
            AuthDef::Bearer(token) => {
                headers.push(json!({ "key": "Authorization", "value": format!("Bearer {token}") }));
            }
        }
    }

    let mut request = json!({
        "method": req.method,
        "url": {
            "raw": format!("{{{{base_url}}}}{path}"),
            "host": ["{{base_url}}"],
            "path": path_segments
        },
        "header": headers
    });

    if !auth_block.is_null() {
        request["auth"] = auth_block;
    }

    // Body
    if let Some(ref body) = req.body {
        match body {
            BodyDef::Json(raw) => {
                request["body"] = json!({ "mode": "raw", "raw": *raw });
            }
            BodyDef::Form(params) => {
                let urlencoded: Vec<Value> = params
                    .iter()
                    .map(|(k, v)| json!({ "key": *k, "value": *v }))
                    .collect();
                request["body"] = json!({ "mode": "urlencoded", "urlencoded": urlencoded });
                // Ensure Content-Type header for forms
                if !headers.iter().any(|h| h["key"] == "Content-Type") {
                    if let Some(arr) = request["header"].as_array_mut() {
                        arr.push(json!({ "key": "Content-Type", "value": "application/x-www-form-urlencoded" }));
                    }
                }
            }
            BodyDef::Xml(raw) => {
                request["body"] = json!({ "mode": "raw", "raw": *raw });
            }
        }
    }

    json!({
        "name": req.name,
        "request": request
    })
}
