//! Swagger 2.0 to OpenAPI 3.0 conversion, enough for mocking: servers from
//! `host` / `basePath` / `schemes`, body and form parameters to request
//! bodies, response schemas and examples to `content` (per `produces`),
//! `definitions` / `parameters` / `responses` / `securityDefinitions` to
//! `components`, and the schema dialect differences (`x-nullable`,
//! `type: file`, string discriminators).

use serde_json::{json, Map, Value};

const METHODS: &[&str] = &["get", "put", "post", "delete", "options", "head", "patch"];

/// Parameter keys that move into the OpenAPI 3 `schema`.
const SCHEMA_KEYS: &[&str] = &[
    "type",
    "format",
    "items",
    "enum",
    "default",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minLength",
    "maxLength",
    "pattern",
    "minItems",
    "maxItems",
    "uniqueItems",
    "multipleOf",
];

fn strings(v: Option<&Value>) -> Option<Vec<String>> {
    let list: Vec<String> = v?
        .as_array()?
        .iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect();
    (!list.is_empty()).then_some(list)
}

/// Fix schema dialect differences, recursively.
fn schema(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, val) in m {
                match k.as_str() {
                    "x-nullable" => {
                        out.insert("nullable".into(), val.clone());
                    }
                    "discriminator" if val.is_string() => {
                        out.insert("discriminator".into(), json!({ "propertyName": val }));
                    }
                    "type" if val == "file" => {
                        out.insert("type".into(), json!("string"));
                        out.insert("format".into(), json!("binary"));
                    }
                    "x-example" => {
                        out.entry("example").or_insert_with(|| val.clone());
                    }
                    "example" | "default" | "enum" => {
                        out.insert(k.clone(), val.clone());
                    }
                    _ => {
                        out.insert(k.clone(), schema(val));
                    }
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(schema).collect()),
        other => other.clone(),
    }
}

/// Rewrite `$ref`s to their OpenAPI 3 locations.
fn refs(v: &mut Value, body_params: &[String]) {
    match v {
        Value::Object(m) => {
            for (k, val) in m.iter_mut() {
                if k == "$ref" {
                    if let Value::String(r) = val {
                        *r = rewrite_ref(r, body_params);
                    }
                } else {
                    refs(val, body_params);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| refs(i, body_params)),
        _ => {}
    }
}

fn rewrite_ref(r: &str, body_params: &[String]) -> String {
    if let Some(rest) = r.strip_prefix("#/definitions/") {
        return format!("#/components/schemas/{rest}");
    }
    if let Some(rest) = r.strip_prefix("#/parameters/") {
        if body_params.iter().any(|b| b == rest) {
            return format!("#/components/requestBodies/{rest}");
        }
        return format!("#/components/parameters/{rest}");
    }
    if let Some(rest) = r.strip_prefix("#/responses/") {
        return format!("#/components/responses/{rest}");
    }
    r.to_string()
}

/// A non-body parameter.
fn parameter(p: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    let mut sch = Map::new();
    for (k, v) in p {
        match k.as_str() {
            "collectionFormat" => match v.as_str() {
                Some("multi") => {
                    out.insert("explode".into(), json!(true));
                }
                Some("csv") => {
                    out.insert("explode".into(), json!(false));
                }
                Some("ssv") => {
                    out.insert("style".into(), json!("spaceDelimited"));
                }
                Some("pipes") => {
                    out.insert("style".into(), json!("pipeDelimited"));
                }
                _ => {}
            },
            "x-example" => {
                out.insert("example".into(), v.clone());
            }
            "allowEmptyValue" | "name" | "in" | "description" | "required" | "deprecated" => {
                out.insert(k.clone(), v.clone());
            }
            key if SCHEMA_KEYS.contains(&key) => {
                sch.insert(k.clone(), v.clone());
            }
            key if key.starts_with("x-") => {
                out.insert(k.clone(), v.clone());
            }
            _ => {}
        }
    }
    if p.get("in").and_then(Value::as_str) == Some("path") {
        out.insert("required".into(), json!(true));
    }
    out.insert("schema".into(), schema(&Value::Object(sch)));
    Value::Object(out)
}

fn content(media: &[String], sch: Option<&Value>, examples: Option<&Value>) -> Value {
    let mut out = Map::new();
    for m in media {
        let mut entry = Map::new();
        if let Some(s) = sch {
            entry.insert("schema".into(), schema(s));
        }
        if let Some(ex) = examples.and_then(|e| e.get(m)) {
            entry.insert("example".into(), ex.clone());
        }
        out.insert(m.clone(), Value::Object(entry));
    }
    Value::Object(out)
}

fn request_body_from(p: &Map<String, Value>, consumes: &[String]) -> Value {
    let mut out = Map::new();
    if let Some(d) = p.get("description") {
        out.insert("description".into(), d.clone());
    }
    if let Some(r) = p.get("required") {
        out.insert("required".into(), r.clone());
    }
    out.insert("content".into(), content(consumes, p.get("schema"), None));
    Value::Object(out)
}

fn form_body(params: &[&Map<String, Value>], consumes: &[String]) -> Value {
    let multipart = consumes.iter().any(|c| c.starts_with("multipart/"))
        || params
            .iter()
            .any(|p| p.get("type").and_then(Value::as_str) == Some("file"));
    let media = if multipart {
        "multipart/form-data"
    } else {
        "application/x-www-form-urlencoded"
    };
    let mut props = Map::new();
    let mut required = Vec::new();
    for p in params {
        let Some(name) = p.get("name").and_then(Value::as_str) else {
            continue;
        };
        let mut prop = parameter(p)
            .get("schema")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if let (Some(d), Value::Object(o)) = (p.get("description"), &mut prop) {
            o.insert("description".into(), d.clone());
        }
        props.insert(name.to_string(), prop);
        if p.get("required").and_then(Value::as_bool) == Some(true) {
            required.push(json!(name));
        }
    }
    let mut sch = json!({ "type": "object", "properties": props });
    if !required.is_empty() {
        sch["required"] = Value::Array(required);
    }
    json!({ "content": { media: { "schema": sch } } })
}

fn response(r: &Value, produces: &[String]) -> Value {
    let Some(m) = r.as_object() else {
        return r.clone();
    };
    if m.contains_key("$ref") {
        return r.clone();
    }
    let mut out = Map::new();
    out.insert(
        "description".into(),
        m.get("description").cloned().unwrap_or_else(|| json!("")),
    );
    if let Some(Value::Object(headers)) = m.get("headers") {
        let converted: Map<String, Value> = headers
            .iter()
            .map(|(k, h)| {
                let mut hdr = Map::new();
                let mut sch = Map::new();
                if let Some(o) = h.as_object() {
                    for (hk, hv) in o {
                        if hk == "description" {
                            hdr.insert(hk.clone(), hv.clone());
                        } else if hk == "x-example" {
                            hdr.insert("example".into(), hv.clone());
                        } else if SCHEMA_KEYS.contains(&hk.as_str()) {
                            sch.insert(hk.clone(), hv.clone());
                        }
                    }
                }
                hdr.insert("schema".into(), schema(&Value::Object(sch)));
                (k.clone(), Value::Object(hdr))
            })
            .collect();
        out.insert("headers".into(), Value::Object(converted));
    }
    let examples = m.get("examples");
    if m.contains_key("schema") || examples.is_some() {
        // Examples for media types the operation does not list still count.
        let mut media = produces.to_vec();
        if let Some(Value::Object(ex)) = examples {
            for k in ex.keys() {
                if !media.contains(k) {
                    media.push(k.clone());
                }
            }
        }
        out.insert("content".into(), content(&media, m.get("schema"), examples));
    }
    Value::Object(out)
}

fn scheme(s: &Value) -> Value {
    let Some(m) = s.as_object() else {
        return s.clone();
    };
    let mut out = Map::new();
    if let Some(d) = m.get("description") {
        out.insert("description".into(), d.clone());
    }
    match m.get("type").and_then(Value::as_str) {
        Some("basic") => {
            out.insert("type".into(), json!("http"));
            out.insert("scheme".into(), json!("basic"));
        }
        Some("apiKey") => {
            out.insert("type".into(), json!("apiKey"));
            for k in ["name", "in"] {
                if let Some(v) = m.get(k) {
                    out.insert(k.into(), v.clone());
                }
            }
        }
        Some("oauth2") => {
            out.insert("type".into(), json!("oauth2"));
            let scopes = m.get("scopes").cloned().unwrap_or_else(|| json!({}));
            let mut flow = Map::new();
            for k in ["authorizationUrl", "tokenUrl"] {
                if let Some(v) = m.get(k) {
                    flow.insert(k.into(), v.clone());
                }
            }
            flow.insert("scopes".into(), scopes);
            let name = match m.get("flow").and_then(Value::as_str) {
                Some("implicit") => "implicit",
                Some("password") => "password",
                Some("application") => "clientCredentials",
                _ => "authorizationCode",
            };
            out.insert("flows".into(), json!({ name: flow }));
        }
        _ => return s.clone(),
    }
    Value::Object(out)
}

/// Convert a Swagger 2.0 document. Returns the OpenAPI 3.0.3 document and
/// notes about what could not be carried over.
pub fn convert(root: &Value) -> Result<(Value, Vec<String>), String> {
    let version = root.get("swagger").and_then(Value::as_str).unwrap_or("");
    if version != "2.0" {
        return Err(format!(
            "unsupported Swagger version {version:?} (expected 2.0)"
        ));
    }
    let mut notes = vec!["converted from Swagger 2.0 to OpenAPI 3.0".to_string()];
    let json_default = vec!["application/json".to_string()];
    let consumes = strings(root.get("consumes")).unwrap_or_else(|| json_default.clone());
    let produces = strings(root.get("produces")).unwrap_or_else(|| json_default.clone());

    let mut out = Map::new();
    out.insert("openapi".into(), json!("3.0.3"));
    for k in ["info", "tags", "externalDocs", "security"] {
        if let Some(v) = root.get(k) {
            out.insert(k.into(), v.clone());
        }
    }
    let base_path = root
        .get("basePath")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim_end_matches('/');
    let url = match root.get("host").and_then(Value::as_str) {
        Some(host) => {
            let scheme = strings(root.get("schemes"))
                .and_then(|s| s.iter().find(|x| *x == "https").or(s.first()).cloned())
                .unwrap_or_else(|| "https".into());
            format!("{scheme}://{host}{base_path}")
        }
        None => base_path.to_string(),
    };
    if !url.is_empty() {
        out.insert("servers".into(), json!([{ "url": url }]));
    }

    // Global parameters: body ones become request bodies.
    let mut components = Map::new();
    let mut body_params = Vec::new();
    if let Some(Value::Object(params)) = root.get("parameters") {
        let mut parameters = Map::new();
        let mut bodies = Map::new();
        for (name, p) in params {
            let Some(pm) = p.as_object() else {
                continue;
            };
            match pm.get("in").and_then(Value::as_str) {
                Some("body") => {
                    body_params.push(name.clone());
                    bodies.insert(name.clone(), request_body_from(pm, &consumes));
                }
                Some("formData") => {
                    notes.push(format!(
                        "global form parameter {name:?} is not converted: declare it on the operation"
                    ));
                }
                _ => {
                    parameters.insert(name.clone(), parameter(pm));
                }
            }
        }
        if !parameters.is_empty() {
            components.insert("parameters".into(), Value::Object(parameters));
        }
        if !bodies.is_empty() {
            components.insert("requestBodies".into(), Value::Object(bodies));
        }
    }
    if let Some(Value::Object(defs)) = root.get("definitions") {
        components.insert(
            "schemas".into(),
            Value::Object(defs.iter().map(|(k, v)| (k.clone(), schema(v))).collect()),
        );
    }
    if let Some(Value::Object(resps)) = root.get("responses") {
        components.insert(
            "responses".into(),
            Value::Object(
                resps
                    .iter()
                    .map(|(k, v)| (k.clone(), response(v, &produces)))
                    .collect(),
            ),
        );
    }
    if let Some(Value::Object(defs)) = root.get("securityDefinitions") {
        components.insert(
            "securitySchemes".into(),
            Value::Object(defs.iter().map(|(k, v)| (k.clone(), scheme(v))).collect()),
        );
    }

    let mut paths = Map::new();
    if let Some(Value::Object(src)) = root.get("paths") {
        for (path, item) in src {
            let Some(item) = item.as_object() else {
                continue;
            };
            let shared: Vec<Value> = item
                .get("parameters")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut new_item = Map::new();
            for (k, v) in item {
                if k.starts_with("x-") || k == "summary" || k == "description" {
                    new_item.insert(k.clone(), v.clone());
                }
            }
            for method in METHODS {
                let Some(Value::Object(op)) = item.get(*method) else {
                    continue;
                };
                let op_consumes = strings(op.get("consumes")).unwrap_or_else(|| consumes.clone());
                let op_produces = strings(op.get("produces")).unwrap_or_else(|| produces.clone());
                let mut new_op = Map::new();
                for (k, v) in op {
                    match k.as_str() {
                        "parameters" | "responses" | "consumes" | "produces" | "schemes" => {}
                        _ => {
                            new_op.insert(k.clone(), v.clone());
                        }
                    }
                }
                // Operation parameters override path-level ones by name + in.
                let own: Vec<Value> = op
                    .get("parameters")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let key = |p: &Value| {
                    (
                        p.get("name").and_then(Value::as_str).map(str::to_string),
                        p.get("in").and_then(Value::as_str).map(str::to_string),
                        p.get("$ref").and_then(Value::as_str).map(str::to_string),
                    )
                };
                let mut all: Vec<Value> = shared
                    .iter()
                    .filter(|s| !own.iter().any(|o| key(o) == key(s)))
                    .cloned()
                    .collect();
                all.extend(own);
                let mut params = Vec::new();
                let mut form = Vec::new();
                let mut body = None;
                for p in &all {
                    let Some(pm) = p.as_object() else {
                        continue;
                    };
                    if let Some(r) = pm.get("$ref").and_then(Value::as_str) {
                        let name = r.strip_prefix("#/parameters/").unwrap_or("");
                        if body_params.iter().any(|b| b == name) {
                            body = Some(json!({ "$ref": r }));
                        } else {
                            params.push(p.clone());
                        }
                        continue;
                    }
                    match pm.get("in").and_then(Value::as_str) {
                        Some("body") => body = Some(request_body_from(pm, &op_consumes)),
                        Some("formData") => form.push(pm),
                        _ => params.push(parameter(pm)),
                    }
                }
                if !form.is_empty() && body.is_none() {
                    body = Some(form_body(&form, &op_consumes));
                }
                if !params.is_empty() {
                    new_op.insert("parameters".into(), Value::Array(params));
                }
                if let Some(b) = body {
                    new_op.insert("requestBody".into(), b);
                }
                let responses: Map<String, Value> = op
                    .get("responses")
                    .and_then(Value::as_object)
                    .map(|r| {
                        r.iter()
                            .filter(|(k, _)| !k.starts_with("x-"))
                            .map(|(k, v)| (k.clone(), response(v, &op_produces)))
                            .collect()
                    })
                    .unwrap_or_default();
                new_op.insert("responses".into(), Value::Object(responses));
                new_item.insert((*method).to_string(), Value::Object(new_op));
            }
            paths.insert(path.clone(), Value::Object(new_item));
        }
    }
    out.insert("paths".into(), Value::Object(paths));
    if !components.is_empty() {
        out.insert("components".into(), Value::Object(components));
    }
    let mut doc = Value::Object(out);
    refs(&mut doc, &body_params);
    Ok((doc, notes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PETSTORE_V2: &str = r##"{
      "swagger": "2.0",
      "info": {"title": "Swagger Petstore", "version": "1.0.0"},
      "host": "petstore.example.com", "basePath": "/v2", "schemes": ["http", "https"],
      "consumes": ["application/json"], "produces": ["application/json", "application/xml"],
      "securityDefinitions": {
        "key": {"type": "apiKey", "name": "api_key", "in": "header"},
        "auth": {"type": "oauth2", "flow": "implicit", "authorizationUrl": "https://example.com/auth", "scopes": {"write:pets": "w"}},
        "basic": {"type": "basic"}
      },
      "parameters": {"NewPet": {"name": "pet", "in": "body", "required": true, "schema": {"$ref": "#/definitions/Pet"}}},
      "paths": {
        "/pets/{petId}": {
          "parameters": [{"name": "petId", "in": "path", "required": true, "type": "integer", "format": "int64"}],
          "get": {"operationId": "getPet", "parameters": [{"name": "tags", "in": "query", "type": "array", "items": {"type": "string"}, "collectionFormat": "multi"}],
            "responses": {"200": {"description": "ok", "schema": {"$ref": "#/definitions/Pet"},
              "headers": {"X-Rate": {"type": "integer"}},
              "examples": {"application/json": {"id": 1, "name": "Rex"}}},
              "404": {"description": "missing"}}},
          "put": {"operationId": "putPet", "parameters": [{"$ref": "#/parameters/NewPet"}], "responses": {"200": {"description": "ok"}}},
          "post": {"operationId": "upload", "consumes": ["multipart/form-data"],
            "parameters": [{"name": "file", "in": "formData", "type": "file", "required": true}, {"name": "note", "in": "formData", "type": "string"}],
            "responses": {"200": {"description": "ok"}}}
        }
      },
      "definitions": {"Pet": {"type": "object", "discriminator": "kind", "required": ["name"],
        "properties": {"id": {"type": "integer"}, "name": {"type": "string", "x-nullable": true}, "kind": {"type": "string"}}}}
    }"##;

    #[test]
    fn converts_the_essentials() {
        let root: Value = serde_json::from_str(PETSTORE_V2).expect("json");
        let (doc, notes) = convert(&root).expect("convert");
        assert_eq!(doc["openapi"], "3.0.3");
        assert_eq!(doc["servers"][0]["url"], "https://petstore.example.com/v2");
        assert!(notes[0].contains("Swagger 2.0"));
        let pet = &doc["components"]["schemas"]["Pet"];
        assert_eq!(pet["discriminator"]["propertyName"], "kind");
        assert_eq!(pet["properties"]["name"]["nullable"], true);
        let get = &doc["paths"]["/pets/{petId}"]["get"];
        assert_eq!(get["parameters"][0]["name"], "petId");
        assert_eq!(get["parameters"][0]["schema"]["format"], "int64");
        assert_eq!(get["parameters"][1]["explode"], true);
        assert_eq!(get["parameters"][1]["schema"]["type"], "array");
        let ok = &get["responses"]["200"];
        assert_eq!(
            ok["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/Pet"
        );
        assert_eq!(ok["content"]["application/json"]["example"]["name"], "Rex");
        assert!(ok["content"]["application/xml"].is_object());
        assert_eq!(ok["headers"]["X-Rate"]["schema"]["type"], "integer");
        assert_eq!(
            doc["paths"]["/pets/{petId}"]["put"]["requestBody"]["$ref"],
            "#/components/requestBodies/NewPet"
        );
        let upload = &doc["paths"]["/pets/{petId}"]["post"]["requestBody"]["content"]
            ["multipart/form-data"]["schema"];
        assert_eq!(upload["properties"]["file"]["format"], "binary");
        assert_eq!(upload["required"], json!(["file"]));
        let schemes = &doc["components"]["securitySchemes"];
        assert_eq!(schemes["basic"]["scheme"], "basic");
        assert_eq!(
            schemes["auth"]["flows"]["implicit"]["authorizationUrl"],
            "https://example.com/auth"
        );
    }

    #[test]
    fn rejects_other_versions() {
        assert!(convert(&json!({"swagger": "1.2"})).is_err());
    }
}
