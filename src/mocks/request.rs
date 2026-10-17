//! Request validation against an operation: parameters (coerced from text to
//! the schema type), required headers and cookies, and the body (content
//! type, JSON syntax, schema). Issues point into a virtual document:
//! `/path/petId`, `/query/limit`, `/header/x-id`, `/body/name`.

use axum::http::HeaderMap;
use serde_json::{Map, Value};

use super::schema::{self, Direction, Issue};
use super::spec::{is_json_media, Location, Operation};

/// Headers OpenAPI says are described elsewhere (ignored as parameters).
const IGNORED_HEADERS: &[&str] = &["accept", "content-type", "authorization"];

/// Main type of a content type (`application/json`), lower case.
pub fn base_media(ct: &str) -> String {
    ct.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn media_matches(declared: &str, actual: &str) -> bool {
    let declared = base_media(declared);
    if declared == "*/*" || declared == actual {
        return true;
    }
    match declared.strip_suffix("/*") {
        Some(ty) => actual.starts_with(&format!("{ty}/")),
        None => false,
    }
}

fn cookies(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .get_all("cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| {
            let (k, v) = c.trim().split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// Validate a request; `body` is the raw body and `content_type` its header.
pub fn validate(
    root: &Value,
    op: &Operation,
    path_params: &[(String, String)],
    query: &[(String, String)],
    headers: &HeaderMap,
    content_type: Option<&str>,
    body: &[u8],
) -> Vec<Issue> {
    let mut issues = Vec::new();
    let cookie_list = cookies(headers);
    for p in &op.parameters {
        let values: Vec<String> = match p.location {
            Location::Path => path_params
                .iter()
                .filter(|(k, _)| k == &p.name)
                .map(|(_, v)| v.clone())
                .collect(),
            Location::Query => query
                .iter()
                .filter(|(k, _)| k == &p.name)
                .map(|(_, v)| v.clone())
                .collect(),
            Location::Header => {
                if IGNORED_HEADERS.contains(&p.name.to_ascii_lowercase().as_str()) {
                    continue;
                }
                headers
                    .get_all(p.name.as_str())
                    .iter()
                    .filter_map(|v| v.to_str().ok().map(str::to_string))
                    .collect()
            }
            Location::Cookie => cookie_list
                .iter()
                .filter(|(k, _)| k == &p.name)
                .map(|(_, v)| v.clone())
                .collect(),
        };
        let pointer = format!(
            "/{}/{}",
            p.location.as_str(),
            schema::escape_pointer(&p.name)
        );
        if values.is_empty() {
            if p.required {
                issues.push(Issue::new(
                    pointer,
                    format!(
                        "required {} parameter {:?} is missing",
                        p.location.as_str(),
                        p.name
                    ),
                ));
            }
            continue;
        }
        let resolved = schema::deref(&p.schema, root);
        let is_array = resolved.get("type").and_then(Value::as_str) == Some("array");
        let value = if is_array && values.len() > 1 {
            let items = resolved.get("items").cloned().unwrap_or(Value::Null);
            Value::Array(
                values
                    .iter()
                    .map(|v| schema::coerce(v, &items, root))
                    .collect(),
            )
        } else {
            schema::coerce(&values[0], &p.schema, root)
        };
        for mut issue in schema::validate(&value, &p.schema, root, Direction::Request) {
            issue.pointer = if issue.pointer == "/" {
                pointer.clone()
            } else {
                format!("{pointer}{}", issue.pointer)
            };
            issues.push(issue);
        }
    }

    let Some(rb) = &op.request_body else {
        return issues;
    };
    if body.is_empty() {
        if rb.required {
            issues.push(Issue::new("/body", "a request body is required"));
        }
        return issues;
    }
    if rb.content.is_empty() {
        return issues;
    }
    let Some(ct) = content_type.map(base_media) else {
        issues.push(Issue::new("/body", "the body has no Content-Type header"));
        return issues;
    };
    let Some(media) = rb
        .content
        .iter()
        .find(|m| media_matches(&m.media_type, &ct))
    else {
        let accepted: Vec<&str> = rb.content.iter().map(|m| m.media_type.as_str()).collect();
        issues.push(Issue::new(
            "/body",
            format!(
                "content type {ct} is not accepted (expected {})",
                accepted.join(", ")
            ),
        ));
        return issues;
    };
    let Some(schema_v) = &media.schema else {
        return issues;
    };
    let value = if is_json_media(&ct) {
        match serde_json::from_slice::<Value>(body) {
            Ok(v) => v,
            Err(e) => {
                issues.push(Issue::new("/body", format!("invalid JSON: {e}")));
                return issues;
            }
        }
    } else if ct == "application/x-www-form-urlencoded" {
        let target = schema::deref(schema_v, root);
        let mut obj = Map::new();
        for (k, v) in form_urlencoded::parse(body) {
            let prop = target
                .pointer(&format!("/properties/{}", schema::escape_pointer(&k)))
                .cloned()
                .unwrap_or(Value::Null);
            obj.insert(k.into_owned(), schema::coerce(&v, &prop, root));
        }
        Value::Object(obj)
    } else {
        return issues;
    };
    for mut issue in schema::validate(&value, schema_v, root, Direction::Request) {
        issue.pointer = if issue.pointer == "/" {
            "/body".into()
        } else {
            format!("/body{}", issue.pointer)
        };
        issues.push(issue);
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::compile::tests::compile;
    use crate::mocks::model::MockSettings;
    use axum::http::HeaderValue;

    const SPEC: &str = r##"{
      "openapi": "3.0.3", "info": {"title": "t", "version": "1"},
      "paths": {"/pets/{petId}": {"put": {"operationId": "put",
        "parameters": [
          {"name": "petId", "in": "path", "required": true, "schema": {"type": "integer", "minimum": 1}},
          {"name": "tags", "in": "query", "schema": {"type": "array", "items": {"type": "string", "enum": ["a", "b"]}}},
          {"name": "limit", "in": "query", "required": true, "schema": {"type": "integer"}},
          {"name": "X-Trace", "in": "header", "required": true, "schema": {"type": "string"}},
          {"name": "session", "in": "cookie", "required": true, "schema": {"type": "string"}}
        ],
        "requestBody": {"required": true, "content": {
          "application/json": {"schema": {"$ref": "#/components/schemas/Pet"}},
          "application/x-www-form-urlencoded": {"schema": {"$ref": "#/components/schemas/Pet"}}
        }},
        "responses": {"200": {"description": "ok"}}}}},
      "components": {"schemas": {"Pet": {"type": "object", "required": ["name"],
        "properties": {"id": {"type": "integer", "readOnly": true}, "name": {"type": "string"}, "age": {"type": "integer"}}}}}
    }"##;

    fn pointers(issues: &[Issue]) -> Vec<&str> {
        issues.iter().map(|i| i.pointer.as_str()).collect()
    }

    #[test]
    fn valid_and_invalid_requests() {
        let c = compile(SPEC, MockSettings::default());
        let op = c.spec.operation("put").expect("op");
        let mut headers = HeaderMap::new();
        headers.insert("x-trace", HeaderValue::from_static("abc"));
        headers.insert("cookie", HeaderValue::from_static("a=1; session=xyz"));
        let path = vec![("petId".to_string(), "3".to_string())];
        let query = vec![
            ("limit".to_string(), "10".to_string()),
            ("tags".to_string(), "a".to_string()),
            ("tags".to_string(), "b".to_string()),
        ];
        let ok = validate(
            &c.spec.root,
            op,
            &path,
            &query,
            &headers,
            Some("application/json"),
            br#"{"name":"Rex"}"#,
        );
        assert!(ok.is_empty(), "{ok:?}");
        let form = validate(
            &c.spec.root,
            op,
            &path,
            &query,
            &headers,
            Some("application/x-www-form-urlencoded"),
            b"name=Rex&age=3",
        );
        assert!(form.is_empty(), "{form:?}");

        let bad_path = vec![("petId".to_string(), "zero".to_string())];
        let bad_query = vec![("tags".to_string(), "c".to_string())];
        let issues = validate(
            &c.spec.root,
            op,
            &bad_path,
            &bad_query,
            &HeaderMap::new(),
            Some("application/json"),
            br#"{"age":"x"}"#,
        );
        let p = pointers(&issues);
        for expected in [
            "/path/petId",
            "/query/tags/0",
            "/query/limit",
            "/header/X-Trace",
            "/cookie/session",
            "/body",
            "/body/age",
        ] {
            assert!(p.contains(&expected), "{expected} missing from {issues:?}");
        }

        let issues = validate(
            &c.spec.root,
            op,
            &path,
            &query,
            &headers,
            Some("text/plain"),
            b"x",
        );
        assert!(issues[0].message.contains("not accepted"), "{issues:?}");
        let issues = validate(
            &c.spec.root,
            op,
            &path,
            &query,
            &headers,
            Some("application/json"),
            b"{",
        );
        assert!(issues[0].message.contains("invalid JSON"), "{issues:?}");
        let issues = validate(&c.spec.root, op, &path, &query, &headers, None, b"");
        assert_eq!(issues[0].message, "a request body is required");
    }
}
