use serde_json::{json, Value};

use super::{AuthDef, BodyDef, Category};

pub fn build(categories: &[Category], base_url: &str) -> Value {
    let version = env!("CARGO_PKG_VERSION");
    let now = chrono::Utc::now().to_rfc3339();
    let mut resources: Vec<Value> = Vec::new();

    // Workspace
    resources.push(json!({
        "_type": "workspace",
        "_id": "wrk_rustybin",
        "name": "Rustybin",
        "description": format!("Rustybin v{version} - HTTP stub service for API gateway testing"),
        "scope": "collection"
    }));

    // Base environment
    resources.push(json!({
        "_type": "environment",
        "_id": "env_base",
        "parentId": "wrk_rustybin",
        "name": "Base Environment",
        "data": { "base_url": base_url }
    }));

    // Build folders and requests
    for (i, cat) in categories.iter().enumerate() {
        let folder_id = format!("fld_{i}");
        resources.push(json!({
            "_type": "request_group",
            "_id": &folder_id,
            "parentId": "wrk_rustybin",
            "name": cat.name
        }));

        for (j, req) in cat.requests.iter().enumerate() {
            let req_id = format!("req_{i}_{j}");

            let mut headers: Vec<Value> = req
                .headers
                .iter()
                .map(|(k, v)| json!({ "name": *k, "value": *v }))
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
                        headers.push(
                            json!({ "name": "Authorization", "value": format!("Bearer {token}") }),
                        );
                    }
                }
            }

            let mut resource = json!({
                "_type": "request",
                "_id": req_id,
                "parentId": &folder_id,
                "name": req.name,
                "method": req.method,
                "url": format!("{{{{ base_url }}}}{}", req.path),
                "headers": headers,
                "body": {}
            });

            if !auth_block.is_null() {
                resource["authentication"] = auth_block;
            }

            // Body
            if let Some(ref body) = req.body {
                match body {
                    BodyDef::Json(raw) => {
                        resource["body"] = json!({ "mimeType": "application/json", "text": *raw });
                        // Ensure Content-Type header
                        if let Some(arr) = resource["headers"].as_array_mut() {
                            if !arr.iter().any(|h| h["name"] == "Content-Type") {
                                arr.push(
                                    json!({ "name": "Content-Type", "value": "application/json" }),
                                );
                            }
                        }
                    }
                    BodyDef::Form(params) => {
                        let form_params: Vec<Value> = params
                            .iter()
                            .map(|(k, v)| json!({ "name": *k, "value": *v }))
                            .collect();
                        resource["body"] = json!({
                            "mimeType": "application/x-www-form-urlencoded",
                            "params": form_params
                        });
                        if let Some(arr) = resource["headers"].as_array_mut() {
                            if !arr.iter().any(|h| h["name"] == "Content-Type") {
                                arr.push(json!({ "name": "Content-Type", "value": "application/x-www-form-urlencoded" }));
                            }
                        }
                    }
                    BodyDef::Xml(raw) => {
                        resource["body"] = json!({ "mimeType": "text/xml", "text": *raw });
                        if let Some(arr) = resource["headers"].as_array_mut() {
                            if !arr.iter().any(|h| h["name"] == "Content-Type") {
                                arr.push(json!({ "name": "Content-Type", "value": "text/xml" }));
                            }
                        }
                    }
                }
            }

            resources.push(resource);
        }
    }

    json!({
        "_type": "export",
        "__export_format": 4,
        "__export_date": now,
        "__export_source": format!("rustybin:v{version}"),
        "resources": resources
    })
}
