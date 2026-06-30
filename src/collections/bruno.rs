use serde_json::{json, Value};

use super::{AuthDef, BodyDef, Category};

pub fn build(categories: &[Category]) -> Value {
    let version = env!("CARGO_PKG_VERSION");

    json!({
        "version": "1",
        "name": "Rustybin",
        "type": "collection",
        "description": format!("Rustybin v{version} — HTTP stub service for API gateway testing"),
        "environments": [
            {
                "name": "Default",
                "variables": [
                    { "name": "base_url", "value": "http://localhost", "enabled": true }
                ]
            }
        ],
        "items": categories.iter().map(build_folder).collect::<Vec<_>>()
    })
}

fn build_folder(cat: &Category) -> Value {
    json!({
        "name": cat.name,
        "type": "folder",
        "items": cat.requests.iter().map(build_request).collect::<Vec<_>>()
    })
}

fn build_request(req: &super::RequestDef) -> Value {
    let mut headers: Vec<Value> = req
        .headers
        .iter()
        .map(|(k, v)| json!({ "name": *k, "value": *v, "enabled": true }))
        .collect();

    let mut auth_block = Value::Null;
    if let Some(ref auth) = req.auth {
        match auth {
            AuthDef::Basic { user, pass } => {
                auth_block = json!({
                    "type": "basic",
                    "username": *user,
                    "password": *pass
                });
            }
            AuthDef::Bearer(token) => {
                headers.push(json!({
                    "name": "Authorization",
                    "value": format!("Bearer {token}"),
                    "enabled": true
                }));
            }
        }
    }

    let mut body = Value::Null;
    if let Some(ref b) = req.body {
        match b {
            BodyDef::Json(raw) => {
                body = json!({
                    "mode": "json",
                    "json": *raw
                });
            }
            BodyDef::Form(params) => {
                let form: Vec<Value> = params
                    .iter()
                    .map(|(k, v)| json!({ "name": *k, "value": *v, "enabled": true }))
                    .collect();
                body = json!({
                    "mode": "formUrlEncoded",
                    "formUrlEncoded": form
                });
            }
            BodyDef::Xml(raw) => {
                body = json!({
                    "mode": "xml",
                    "xml": *raw
                });
            }
        }
    }

    let mut item = json!({
        "name": req.name,
        "type": "http",
        "request": {
            "method": req.method,
            "url": format!("{{{{base_url}}}}{}", req.path),
            "headers": headers
        }
    });

    if !body.is_null() {
        item["request"]["body"] = body;
    }
    if !auth_block.is_null() {
        item["request"]["auth"] = auth_block;
    }

    item
}
