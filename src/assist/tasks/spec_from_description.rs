//! `spec_from_description`: an OpenAPI document from a plain description, on
//! the version the user chose, with style options. The answer is parsed and
//! run through the deploy checks; errors go back to the model.

use serde_json::{json, Map, Value};

use super::{check_spec, ABOUT};
use crate::assist::models::Tier;
use crate::assist::task::{text_field, Task, MAX_TEXT_CHARS};

pub const TASK: Task = Task {
    name: "spec_from_description",
    title: "Describe an API",
    description: "An OpenAPI document (3.1.0, 3.0.3 or Swagger 2.0) from a plain description, with auth, pagination and error style options; checked before it is offered",
    writes: false,
    tier: Tier::Capable,
    system: SYSTEM,
    schema: Some(schema),
    prompt,
    mock,
    cache_key: None,
    post: Some(post),
};

const SYSTEM: &str = "You write OpenAPI documents for mock APIs. Answer with one JSON object: the \
complete document, nothing else. Rules: use the exact OpenAPI version asked for (`openapi: \
\"3.1.0\"`, `openapi: \"3.0.3\"`, or `swagger: \"2.0\"` with `definitions`); every operation has an \
operationId, a summary, documented parameters, a request body schema where it takes one and at \
least one success response with a JSON schema; put at least one realistic `example` on every \
success response's JSON media type and on request bodies; reuse schemas through components; apply \
the auth, pagination and error style asked for; keep it to what the description covers, at most \
40 operations.";

pub const VERSIONS: &[&str] = &["3.1.0", "3.0.3", "2.0"];
pub const AUTH: &[&str] = &["none", "api_key", "bearer", "oauth2"];
pub const PAGINATION: &[&str] = &["none", "page", "cursor"];
pub const ERRORS: &[&str] = &["problem", "simple"];

/// The output schema: a document with the version marker, info and paths.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "required": ["info", "paths"],
        "properties": {
            "openapi": { "type": "string" },
            "swagger": { "type": "string" },
            "info": { "type": "object", "required": ["title", "version"],
                "properties": { "title": { "type": "string" }, "version": { "type": "string" }, "description": { "type": "string" } } },
            "paths": { "type": "object", "minProperties": 1 },
            "components": { "type": "object" },
            "definitions": { "type": "object" },
            "servers": { "type": "array" },
            "security": { "type": "array" },
            "tags": { "type": "array" }
        }
    })
}

/// The style options, validated: `(version, auth, pagination, errors)`.
pub fn style(inputs: &Value) -> Result<(String, String, String, String), String> {
    let pick = |name: &str, allowed: &[&str], default: &str| -> Result<String, String> {
        let raw = inputs
            .get("style")
            .and_then(|s| s.get(name))
            .or_else(|| inputs.get(name))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(default);
        if allowed.contains(&raw) {
            Ok(raw.to_string())
        } else {
            Err(format!(
                "inputs.{name} must be one of {}",
                allowed.join(", ")
            ))
        }
    };
    Ok((
        pick("openapi_version", VERSIONS, "3.1.0")?,
        pick("auth", AUTH, "none")?,
        pick("pagination", PAGINATION, "none")?,
        pick("errors", ERRORS, "problem")?,
    ))
}

fn prompt(inputs: &Value) -> Result<String, String> {
    let description = text_field(inputs, "description", MAX_TEXT_CHARS, true)?.unwrap_or_default();
    let (version, auth, pagination, errors) = style(inputs)?;
    let avoid: Vec<String> = inputs
        .get("avoid_names")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .take(50)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut out = format!(
        "{ABOUT}\n\nWrite the OpenAPI document for this API:\n\n{description}\n\n\
         Version: {}.\nAuthentication: {}.\nPagination of list operations: {}.\nError responses: {}.\n",
        match version.as_str() {
            "2.0" => "Swagger 2.0 (`swagger: \"2.0\"`, `definitions`, `produces`)",
            v => v,
        },
        match auth.as_str() {
            "none" => "none",
            "api_key" => "an API key in the X-API-Key header, required on every operation",
            "bearer" => "a bearer token (http bearer, JWT), required on every operation",
            _ => "OAuth 2.0 client credentials with a tokenUrl and two scopes (read, write)",
        },
        match pagination.as_str() {
            "none" => "none",
            "page" => "page and page_size query parameters, with total and page in list responses",
            _ => "a cursor query parameter and next_cursor in list responses",
        },
        match errors.as_str() {
            "problem" => "RFC 9457 problem details (application/problem+json: type, title, status, detail)",
            _ => "a simple {code, message} object",
        },
    );
    if !avoid.is_empty() {
        out.push_str(&format!(
            "Do not title it with any of these names, they are taken: {}.\n",
            avoid.join(", ")
        ));
    }
    Ok(out)
}

fn post(inputs: &Value, answer: Value) -> Result<Value, String> {
    let (version, ..) = style(inputs).unwrap_or_default();
    let declared = answer
        .get("openapi")
        .or_else(|| answer.get("swagger"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let expected_prefix = match version.as_str() {
        "2.0" => "2.0",
        "3.0.3" => "3.0",
        _ => "3.1",
    };
    if !declared.starts_with(expected_prefix) {
        return Err(format!(
            "the document declares version {declared:?} but {version} was asked for"
        ));
    }
    check_spec(&answer)?;
    Ok(answer)
}

// ── The deterministic answer ────────────────────────────────────────

const STOP: &[&str] = &[
    "this",
    "that",
    "with",
    "from",
    "into",
    "status",
    "items",
    "lines",
    "users",
    "api",
    "apis",
    "endpoints",
    "operations",
    "resources",
    "fields",
    "needs",
    "wants",
    "has",
    "its",
    "their",
    "services",
    "requests",
    "responses",
    "errors",
    "details",
    "keys",
    "tokens",
    "pages",
    "cursors",
];

/// Plural nouns in the description, in order of appearance (at most four).
pub fn resources(description: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in description.split(|c: char| !c.is_ascii_alphanumeric()) {
        let w = raw.to_ascii_lowercase();
        if w.len() < 4 || !w.ends_with('s') || w.ends_with("ss") || STOP.contains(&w.as_str()) {
            continue;
        }
        if !out.contains(&w) {
            out.push(w);
        }
        if out.len() == 4 {
            break;
        }
    }
    if out.is_empty() {
        out.push("items".to_string());
    }
    out
}

fn singular(res: &str) -> String {
    if let Some(s) = res.strip_suffix("ies") {
        return format!("{s}y");
    }
    res.strip_suffix('s').unwrap_or(res).to_string()
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn mock(inputs: &Value) -> Value {
    let description = inputs
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("items");
    let (version, auth, pagination, errors) = style(inputs).unwrap_or_else(|_| {
        (
            "3.1.0".into(),
            "none".into(),
            "none".into(),
            "problem".into(),
        )
    });
    let res = resources(description);
    let title = format!("{} API", title_case(&singular(&res[0])));
    let swagger = version == "2.0";
    let schema_ref = |name: &str| {
        if swagger {
            json!({ "$ref": format!("#/definitions/{name}") })
        } else {
            json!({ "$ref": format!("#/components/schemas/{name}") })
        }
    };
    let error_schema = if errors == "problem" {
        json!({ "type": "object", "required": ["title", "status"], "properties": {
            "type": { "type": "string", "format": "uri" }, "title": { "type": "string" },
            "status": { "type": "integer" }, "detail": { "type": "string" } } })
    } else {
        json!({ "type": "object", "required": ["code", "message"], "properties": {
            "code": { "type": "string" }, "message": { "type": "string" } } })
    };
    let error_example = if errors == "problem" {
        json!({ "type": "about:blank", "title": "Not Found", "status": 404, "detail": "No such item" })
    } else {
        json!({ "code": "not_found", "message": "No such item" })
    };
    let mut schemas = Map::new();
    schemas.insert("Error".into(), error_schema);
    let mut paths = Map::new();
    for (i, r) in res.iter().enumerate() {
        let one = title_case(&singular(r));
        let item_schema = json!({
            "type": "object",
            "required": ["id", "name"],
            "properties": {
                "id": { "type": "integer", "minimum": 1, "example": i + 1 },
                "name": { "type": "string", "example": format!("Sample {}", singular(r)) },
                "created_at": { "type": "string", "format": "date-time", "example": "2026-01-15T09:30:00Z" }
            }
        });
        schemas.insert(one.clone(), item_schema);
        let item_example = json!({ "id": i + 1, "name": format!("Sample {}", singular(r)), "created_at": "2026-01-15T09:30:00Z" });
        let list_example = match pagination.as_str() {
            "page" => json!({ "items": [item_example], "page": 1, "total": 1 }),
            "cursor" => json!({ "items": [item_example], "next_cursor": null }),
            _ => json!([item_example]),
        };
        let list_schema = match pagination.as_str() {
            "page" => {
                json!({ "type": "object", "required": ["items"], "properties": { "items": { "type": "array", "items": schema_ref(&one) }, "page": { "type": "integer" }, "total": { "type": "integer" } } })
            }
            "cursor" => {
                json!({ "type": "object", "required": ["items"], "properties": { "items": { "type": "array", "items": schema_ref(&one) }, "next_cursor": { "type": ["string", "null"] } } })
            }
            _ => json!({ "type": "array", "items": schema_ref(&one) }),
        };
        let list_schema = if swagger && pagination == "cursor" {
            json!({ "type": "object", "required": ["items"], "properties": { "items": { "type": "array", "items": schema_ref(&one) }, "next_cursor": { "type": "string" } } })
        } else {
            list_schema
        };
        let mut list_params = Vec::new();
        match pagination.as_str() {
            "page" => {
                list_params.push(json!({ "name": "page", "in": "query", "schema": { "type": "integer", "minimum": 1 } }));
                list_params.push(json!({ "name": "page_size", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 100 } }));
            }
            "cursor" => list_params
                .push(json!({ "name": "cursor", "in": "query", "schema": { "type": "string" } })),
            _ => {}
        }
        let id_param = json!({ "name": "id", "in": "path", "required": true, "schema": { "type": "integer", "minimum": 1 } });
        let (list_params, id_param) = if swagger {
            (
                list_params
                    .into_iter()
                    .map(swagger_param)
                    .collect::<Vec<_>>(),
                swagger_param(id_param),
            )
        } else {
            (list_params, id_param)
        };
        let response = |desc: &str, schema: Value, example: Value| {
            if swagger {
                json!({ "description": desc, "schema": schema, "examples": { "application/json": example } })
            } else {
                json!({ "description": desc, "content": { "application/json": { "schema": schema, "example": example } } })
            }
        };
        let not_found = response("Not found", schema_ref("Error"), error_example.clone());
        let body = |schema: Value, example: Value| {
            if swagger {
                json!({ "name": "body", "in": "body", "required": true, "schema": schema })
            } else {
                json!({ "required": true, "content": { "application/json": { "schema": schema, "example": example } } })
            }
        };
        let mut post_op = json!({
            "operationId": format!("create{one}"),
            "summary": format!("Create a {}", singular(r)),
            "tags": [r],
            "responses": { "201": response("Created", schema_ref(&one), item_example.clone()), "400": response("Invalid", schema_ref("Error"), error_example.clone()) }
        });
        let new_example = json!({ "name": format!("New {}", singular(r)) });
        let new_schema = json!({ "type": "object", "required": ["name"], "properties": { "name": { "type": "string" } } });
        if swagger {
            post_op["parameters"] = json!([body(new_schema, new_example)]);
        } else {
            post_op["requestBody"] = body(new_schema, new_example);
        }
        let mut list_op = json!({
            "operationId": format!("list{}", title_case(r)),
            "summary": format!("List {r}"),
            "tags": [r],
            "responses": { "200": response(&format!("The {r}"), list_schema, list_example) }
        });
        if !list_params.is_empty() {
            list_op["parameters"] = json!(list_params);
        }
        paths.insert(format!("/{r}"), json!({ "get": list_op, "post": post_op }));
        paths.insert(format!("/{r}/{{id}}"), json!({
            "get": { "operationId": format!("get{one}"), "summary": format!("Get a {}", singular(r)), "tags": [r], "parameters": [id_param.clone()],
                "responses": { "200": response(&format!("The {}", singular(r)), schema_ref(&one), item_example.clone()), "404": not_found.clone() } },
            "delete": { "operationId": format!("delete{one}"), "summary": format!("Delete a {}", singular(r)), "tags": [r], "parameters": [id_param],
                "responses": { "204": { "description": "Deleted" }, "404": not_found } }
        }));
    }
    let mut doc = Map::new();
    if swagger {
        doc.insert("swagger".into(), json!("2.0"));
    } else {
        doc.insert("openapi".into(), json!(version));
    }
    doc.insert("info".into(), json!({ "title": title, "version": "1.0.0", "description": format!("Drafted by the built-in mock assistant from: {}", description.chars().take(300).collect::<String>()) }));
    if swagger {
        doc.insert("basePath".into(), json!("/v1"));
        doc.insert("consumes".into(), json!(["application/json"]));
        doc.insert("produces".into(), json!(["application/json"]));
    } else {
        doc.insert("servers".into(), json!([{ "url": "/v1" }]));
    }
    doc.insert("paths".into(), Value::Object(paths));
    let security_scheme = match auth.as_str() {
        "api_key" => Some(json!({ "type": "apiKey", "in": "header", "name": "X-API-Key" })),
        "bearer" => {
            if swagger {
                Some(json!({ "type": "apiKey", "in": "header", "name": "Authorization" }))
            } else {
                Some(json!({ "type": "http", "scheme": "bearer", "bearerFormat": "JWT" }))
            }
        }
        "oauth2" => {
            if swagger {
                Some(
                    json!({ "type": "oauth2", "flow": "application", "tokenUrl": "https://auth.example.com/oauth/token", "scopes": { "read": "Read", "write": "Write" } }),
                )
            } else {
                Some(
                    json!({ "type": "oauth2", "flows": { "clientCredentials": { "tokenUrl": "https://auth.example.com/oauth/token", "scopes": { "read": "Read", "write": "Write" } } } }),
                )
            }
        }
        _ => None,
    };
    if swagger {
        doc.insert("definitions".into(), Value::Object(schemas));
        if let Some(s) = security_scheme {
            doc.insert("securityDefinitions".into(), json!({ "ApiAuth": s }));
            doc.insert("security".into(), json!([{ "ApiAuth": if auth == "oauth2" { json!(["read", "write"]) } else { json!([]) } }]));
        }
    } else {
        let mut components = Map::new();
        components.insert("schemas".into(), Value::Object(schemas));
        if let Some(s) = security_scheme {
            components.insert("securitySchemes".into(), json!({ "ApiAuth": s }));
            doc.insert("security".into(), json!([{ "ApiAuth": if auth == "oauth2" { json!(["read", "write"]) } else { json!([]) } }]));
        }
        doc.insert("components".into(), Value::Object(components));
    }
    Value::Object(doc)
}

/// A 3.x parameter as Swagger 2.0 writes it (the schema's type inline).
fn swagger_param(p: Value) -> Value {
    let mut out = Map::new();
    for k in ["name", "in", "required"] {
        if let Some(v) = p.get(k) {
            out.insert(k.into(), v.clone());
        }
    }
    if let Some(Value::Object(schema)) = p.get("schema") {
        for (k, v) in schema {
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out)
}
