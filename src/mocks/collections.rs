//! A mock's operations as a ready-to-run collection: Postman, Hurl, curl,
//! `.http` (VS Code REST Client / JetBrains) and k6. Every request is the
//! operation's valid sample (parameters, body and demo credentials), with
//! the status the mock answers it with.

use axum::http::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{json, Value};

use super::checks::{self, SampleRequest};
use super::compile::{Compiled, RouteMatch};
use super::model::ResponseMode;
use super::respond::{self, RequestView};
use super::spec::Operation;

/// Formats: (name, content type, file extension).
pub const FORMATS: &[(&str, &str, &str)] = &[
    ("postman", "application/json", "postman.json"),
    ("hurl", "text/plain; charset=utf-8", "hurl"),
    ("curl", "text/x-shellscript", "sh"),
    ("http", "text/plain; charset=utf-8", "http"),
    ("k6", "application/javascript", "k6.js"),
];

/// One request of the collection.
struct Item {
    name: String,
    folder: String,
    sample: SampleRequest,
    /// Status the mock answers with (`None` when it depends on state).
    status: Option<u16>,
}

/// Header and line values come from the spec: keep them on one line.
fn one_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

fn query_string(q: &[(String, String)]) -> String {
    if q.is_empty() {
        return String::new();
    }
    let mut enc = form_urlencoded::Serializer::new(String::new());
    for (k, v) in q {
        enc.append_pair(k, v);
    }
    format!("?{}", enc.finish())
}

fn expected_status(c: &Compiled, op: &Operation, sample: &SampleRequest) -> Option<u16> {
    let mode = c
        .settings
        .operations
        .get(&op.id)
        .and_then(|o| o.mode)
        .unwrap_or(c.settings.responses.mode);
    if mode == ResponseMode::Stateful {
        return None;
    }
    let params = match c.match_route(&sample.method, &sample.path) {
        RouteMatch::Found { params, .. } => params,
        _ => Vec::new(),
    };
    let mut headers = HeaderMap::new();
    for (k, v) in &sample.headers {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            headers.append(n, v);
        }
    }
    let out = respond::respond(
        c,
        op,
        &RequestView {
            path_params: &params,
            query: &sample.query,
            headers: &headers,
            body_json: sample.body.as_ref(),
            body_text: None,
        },
    );
    Some(out.status)
}

/// Without `reveal` the demo credentials are placeholders (the expected
/// status is still the one the real credentials get).
fn items(c: &Compiled, reveal: bool) -> Vec<Item> {
    c.spec
        .operations
        .iter()
        .map(|op| {
            let mut sample = checks::sample_request(c, op);
            let status = expected_status(c, op, &sample);
            if !reveal {
                checks::hide_credentials(c, op, &mut sample);
            }
            Item {
                name: op.summary.clone().unwrap_or_else(|| op.id.clone()),
                folder: op.tags.first().cloned().unwrap_or_else(|| "default".into()),
                sample,
                status,
            }
        })
        .collect()
}

fn body_text(s: &SampleRequest) -> Option<String> {
    s.body
        .as_ref()
        .map(|b| serde_json::to_string_pretty(b).unwrap_or_default())
}

/// Shell single quoting.
fn sh(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn js(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn postman(items: Vec<Item>, title: &str, base: &str) -> String {
    let mut folders: Vec<(String, Vec<Value>)> = Vec::new();
    for it in items {
        let s = &it.sample;
        let segments: Vec<&str> = s.path.split('/').filter(|p| !p.is_empty()).collect();
        let mut request = json!({
            "method": s.method,
            "header": s.headers.iter().map(|(k, v)| json!({ "key": k, "value": v })).collect::<Vec<_>>(),
            "url": {
                "raw": format!("{{{{baseUrl}}}}{}{}", s.path, query_string(&s.query)),
                "host": ["{{baseUrl}}"],
                "path": segments,
                "query": s.query.iter().map(|(k, v)| json!({ "key": k, "value": v })).collect::<Vec<_>>(),
            },
        });
        if let (Some(body), Some(obj)) = (body_text(s), request.as_object_mut()) {
            obj.insert(
                "body".into(),
                json!({ "mode": "raw", "raw": body, "options": { "raw": { "language": "json" } } }),
            );
        }
        let mut item = json!({ "name": it.name, "request": request });
        if let (Some(status), Some(obj)) = (it.status, item.as_object_mut()) {
            obj.insert(
                "event".into(),
                json!([{ "listen": "test", "script": { "type": "text/javascript", "exec": [
                    format!("pm.test(\"status is {status}\", function () {{ pm.response.to.have.status({status}); }});")
                ] } }]),
            );
        }
        match folders.iter_mut().find(|(f, _)| *f == it.folder) {
            Some((_, list)) => list.push(item),
            None => folders.push((it.folder, vec![item])),
        }
    }
    let doc = json!({
        "info": {
            "name": format!("{title} (Rustybin mock)"),
            "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json",
        },
        "variable": [{ "key": "baseUrl", "value": base }],
        "item": folders.into_iter().map(|(name, item)| json!({ "name": name, "item": item })).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

fn hurl(items: Vec<Item>, title: &str, base: &str) -> String {
    let mut out = format!(
        "# {} (Rustybin mock)\n# Run: hurl --test --variable base_url={base} <this file>\n\n",
        one_line(title)
    );
    for it in items {
        let s = &it.sample;
        out.push_str(&format!("# {}\n", one_line(&it.name)));
        out.push_str(&format!(
            "{} {{{{base_url}}}}{}{}\n",
            s.method,
            s.path,
            one_line(&query_string(&s.query))
        ));
        for (k, v) in &s.headers {
            out.push_str(&format!("{}: {}\n", one_line(k), one_line(v)));
        }
        if let Some(body) = body_text(s) {
            out.push_str(&format!("```json\n{body}\n```\n"));
        }
        match it.status {
            Some(st) => out.push_str(&format!("HTTP {st}\n\n")),
            None => out.push_str("HTTP *\n\n"),
        }
    }
    out
}

fn curl(items: Vec<Item>, title: &str, base: &str) -> String {
    let mut out = format!(
        "#!/bin/sh\n# {} (Rustybin mock)\nBASE_URL=\"${{BASE_URL:-{base}}}\"\n\n",
        one_line(title)
    );
    for it in items {
        let s = &it.sample;
        out.push_str(&format!("# {}\n", one_line(&it.name)));
        out.push_str(&format!(
            "curl -sS -X {} \"$BASE_URL\"{}",
            s.method,
            sh(&format!("{}{}", s.path, query_string(&s.query)))
        ));
        for (k, v) in &s.headers {
            out.push_str(&format!(" \\\n  -H {}", sh(&format!("{k}: {v}"))));
        }
        if let Some(body) = s.body.as_ref() {
            let compact = serde_json::to_string(body).unwrap_or_default();
            out.push_str(&format!(
                " \\\n  -H 'Content-Type: application/json' \\\n  --data {}",
                sh(&compact)
            ));
        }
        out.push_str("\necho\n\n");
    }
    out
}

fn http_file(items: Vec<Item>, title: &str, base: &str) -> String {
    let mut out = format!(
        "# {} (Rustybin mock)\n@baseUrl = {base}\n\n",
        one_line(title)
    );
    for it in items {
        let s = &it.sample;
        out.push_str(&format!("### {}\n", one_line(&it.name)));
        out.push_str(&format!(
            "{} {{{{baseUrl}}}}{}{}\n",
            s.method,
            s.path,
            one_line(&query_string(&s.query))
        ));
        for (k, v) in &s.headers {
            out.push_str(&format!("{}: {}\n", one_line(k), one_line(v)));
        }
        if let Some(body) = body_text(s) {
            out.push_str(&format!("Content-Type: application/json\n\n{body}\n"));
        }
        out.push('\n');
    }
    out
}

fn k6(items: Vec<Item>, title: &str, base: &str) -> String {
    let mut out = format!(
        "// {} (Rustybin mock). Run: k6 run <this file>\nimport http from 'k6/http';\nimport {{ check }} from 'k6';\n\nconst BASE_URL = __ENV.BASE_URL || {};\n\nexport default function () {{\n",
        one_line(title),
        js(base)
    );
    for it in items {
        let s = &it.sample;
        let mut headers = serde_json::Map::new();
        for (k, v) in &s.headers {
            headers.insert(k.clone(), json!(v));
        }
        let body = match s.body.as_ref() {
            Some(b) => {
                headers.insert("Content-Type".into(), json!("application/json"));
                js(&serde_json::to_string(b).unwrap_or_default())
            }
            None => "null".into(),
        };
        out.push_str(&format!(
            "  {{\n    // {}\n    const res = http.request({}, BASE_URL + {}, {body}, {{ headers: {} }});\n",
            one_line(&it.name),
            js(&s.method),
            js(&format!("{}{}", s.path, query_string(&s.query))),
            Value::Object(headers)
        ));
        if let Some(st) = it.status {
            out.push_str(&format!(
                "    check(res, {{ {}: (r) => r.status === {st} }});\n",
                js(&format!("{} status {st}", one_line(&it.name)))
            ));
        }
        out.push_str("  }\n");
    }
    out.push_str("}\n");
    out
}

/// Render a collection; `None` for an unknown format. Returns (content type,
/// file name, text). Without `reveal` the credentials are placeholders.
pub fn render(
    c: &Compiled,
    name: &str,
    format: &str,
    base: &str,
    reveal: bool,
) -> Option<(&'static str, String, String)> {
    let (_, ct, ext) = FORMATS.iter().find(|(f, _, _)| *f == format)?;
    let title = &c.spec.title;
    let list = items(c, reveal);
    let text = match format {
        "postman" => postman(list, title, base),
        "hurl" => hurl(list, title, base),
        "curl" => curl(list, title, base),
        "http" => http_file(list, title, base),
        "k6" => k6(list, title, base),
        _ => return None,
    };
    Some((ct, format!("{name}.{ext}"), text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::compile::tests::compile;
    use crate::mocks::{sample_settings, SAMPLE_SPEC};

    #[test]
    fn every_format_renders_the_sample() {
        let c = compile(SAMPLE_SPEC, sample_settings());
        let base = "http://localhost:8080/mock/petstore";
        for (format, _, _) in FORMATS {
            let (_, file, text) = render(&c, "petstore", format, base, true).expect("format");
            assert!(file.starts_with("petstore."), "{file}");
            assert!(text.contains("/v1/pets"), "{format}: {text}");
        }
        let (_, _, pm) = render(&c, "petstore", "postman", base, true).expect("postman");
        let doc: Value = serde_json::from_str(&pm).expect("json");
        assert_eq!(doc["variable"][0]["value"], base);
        let (_, _, h) = render(&c, "petstore", "hurl", base, true).expect("hurl");
        assert!(h.contains("GET {{base_url}}/v1/pets"), "{h}");
        assert!(h.contains("HTTP 200"), "{h}");
        assert!(render(&c, "petstore", "nope", base, true).is_none());
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(sh("a'b"), r"'a'\''b'");
        assert_eq!(one_line("a\r\nb"), "a  b");
    }
}
