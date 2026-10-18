//! Choosing and rendering a mock response.
//!
//! Order, first match wins: `Prefer` / `X-Rustybin-Mock-*` headers, the
//! operation's rules, the operation's `respond` override, then the default
//! (lowest documented 2xx) with the configured mode.

use axum::http::{HeaderMap, HeaderValue};
use serde_json::{json, Value};

use super::compile::Compiled;
use super::model::{Condition, ResponseMode, ResponseOverride, Rule};
use super::schema::{self, Direction, GenOptions};
use super::spec::{Media, Operation, ResponseDef};
use super::state::MockState;

/// Header alternative to `Prefer: code=...`.
pub const STATUS_HEADER: &str = "x-rustybin-mock-status";
/// Header alternative to `Prefer: example=...`.
pub const EXAMPLE_HEADER: &str = "x-rustybin-mock-example";

/// What the handler knows about the incoming request.
pub struct RequestView<'a> {
    pub path_params: &'a [(String, String)],
    pub query: &'a [(String, String)],
    pub headers: &'a HeaderMap,
    /// Parsed JSON body, when the body is JSON.
    pub body_json: Option<&'a Value>,
    /// Raw body as text (rules on `body`).
    pub body_text: Option<&'a str>,
}

/// A rendered body.
#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    Json(Value),
    Text(String),
}

impl Body {
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Body::Json(v) => serde_json::to_vec_pretty(v).unwrap_or_default(),
            Body::Text(s) => s.clone().into_bytes(),
        }
    }

    /// The JSON value (text bodies that parse as JSON count).
    pub fn as_json(&self) -> Option<Value> {
        match self {
            Body::Json(v) => Some(v.clone()),
            Body::Text(s) => serde_json::from_str(s).ok(),
        }
    }
}

/// A fully decided response.
#[derive(Clone, Debug)]
pub struct Outgoing {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub content_type: Option<String>,
    pub body: Option<Body>,
    /// How the response was chosen (`example:rex`, `generated`, `rule:1`, ...).
    pub source: String,
    pub delay_ms: u64,
}

/// Where a body comes from.
#[derive(Clone, Copy, Debug)]
pub struct BodyChoice<'a> {
    pub body: Option<&'a Value>,
    pub example: Option<&'a str>,
    pub mode: ResponseMode,
}

/// `Prefer` directives (RFC 7240 style, Prism compatible).
#[derive(Default, Debug)]
struct Preference {
    code: Option<u16>,
    example: Option<String>,
    dynamic: Option<bool>,
}

fn preference(headers: &HeaderMap) -> Preference {
    let mut p = Preference::default();
    for value in headers.get_all("prefer") {
        let Ok(text) = value.to_str() else {
            continue;
        };
        for part in text.split([',', ';']) {
            let mut kv = part.trim().splitn(2, '=');
            let key = kv.next().unwrap_or("").trim().to_ascii_lowercase();
            let val = kv.next().unwrap_or("").trim().trim_matches('"');
            match key.as_str() {
                "code" => p.code = val.parse().ok().filter(|c| (100..=599).contains(c)),
                "example" if !val.is_empty() => p.example = Some(val.to_string()),
                "dynamic" => p.dynamic = Some(val.eq_ignore_ascii_case("true")),
                _ => {}
            }
        }
    }
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
    };
    if let Some(code) = header(STATUS_HEADER).and_then(|v| v.parse().ok()) {
        if (100..=599).contains(&code) {
            p.code = Some(code);
        }
    }
    if let Some(ex) = header(EXAMPLE_HEADER).filter(|v| !v.is_empty()) {
        p.example = Some(ex.to_string());
    }
    p
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The request value a rule key refers to.
fn lookup(key: &str, req: &RequestView) -> Option<String> {
    if key == "body" {
        return req
            .body_json
            .map(text_of)
            .or_else(|| req.body_text.map(str::to_string));
    }
    let (kind, name) = key.split_once('.')?;
    match kind {
        "path" => req
            .path_params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone()),
        "query" => req
            .query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone()),
        "header" => req
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
        "body" => {
            let pointer: String = name
                .split('.')
                .map(|p| format!("/{}", schema::escape_pointer(p)))
                .collect();
            req.body_json?.pointer(&pointer).map(text_of)
        }
        _ => None,
    }
}

fn condition_holds(cond: &Condition, value: Option<&str>) -> bool {
    if let Some(exists) = cond.exists {
        if exists != value.is_some() {
            return false;
        }
    }
    if let Some(expected) = &cond.equals {
        if value != Some(text_of(expected).as_str()) {
            return false;
        }
    }
    if let Some(unexpected) = &cond.not_equals {
        if value == Some(text_of(unexpected).as_str()) {
            return false;
        }
    }
    if let Some(pattern) = &cond.matches {
        let Some(v) = value else {
            return false;
        };
        let ok = schema::cached_regex(pattern).is_some_and(|re| re.is_match(v));
        if !ok {
            return false;
        }
    }
    true
}

/// Each condition of a rule with the request value it saw and whether it
/// held (the console's "would match" preview).
pub fn explain_rule(rule: &Rule, req: &RequestView) -> Vec<Value> {
    rule.when
        .iter()
        .map(|(key, cond)| {
            let value = lookup(key, req);
            let held = condition_holds(cond, value.as_deref());
            json!({ "key": key, "value": value, "matched": held })
        })
        .collect()
}

/// Does every condition of the rule hold?
pub fn rule_matches(rule: &Rule, req: &RequestView) -> bool {
    rule.when
        .iter()
        .all(|(key, cond)| condition_holds(cond, lookup(key, req).as_deref()))
}

/// Pick the content entry for an `Accept` header (JSON preferred otherwise).
pub fn negotiate<'m>(content: &'m [Media], accept: Option<&str>) -> Option<&'m Media> {
    if content.is_empty() {
        return None;
    }
    if let Some(accept) = accept {
        let mut ranges: Vec<(String, f32)> = accept
            .split(',')
            .filter_map(|part| {
                let mut bits = part.split(';');
                let media = bits.next()?.trim().to_ascii_lowercase();
                let q = bits
                    .filter_map(|b| b.trim().strip_prefix("q="))
                    .find_map(|q| q.parse::<f32>().ok())
                    .unwrap_or(1.0);
                (!media.is_empty() && q > 0.0).then_some((media, q))
            })
            .collect();
        ranges.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        for (range, _) in &ranges {
            if range == "*/*" {
                break;
            }
            let found = content.iter().find(|m| {
                let base = m
                    .media_type
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                match range.strip_suffix("/*") {
                    Some(ty) => base.starts_with(&format!("{ty}/")),
                    None => &base == range,
                }
            });
            if found.is_some() {
                return found;
            }
        }
    }
    content.iter().find(|m| m.is_json()).or(content.first())
}

/// Does an `Accept` header allow `media_type`?
pub fn accept_allows(accept: &str, media_type: &str) -> bool {
    let base = media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    accept.split(',').any(|part| {
        let mut bits = part.split(';');
        let range = bits.next().unwrap_or("").trim().to_ascii_lowercase();
        let q = bits
            .filter_map(|b| b.trim().strip_prefix("q="))
            .find_map(|q| q.parse::<f32>().ok())
            .unwrap_or(1.0);
        if range.is_empty() || q <= 0.0 {
            return false;
        }
        match range.strip_suffix("/*") {
            Some("*") => true,
            Some(ty) => base.starts_with(&format!("{ty}/")),
            None => range == base,
        }
    })
}

fn reason(status: u16) -> &'static str {
    axum::http::StatusCode::from_u16(status)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or("Response")
}

/// A problem+json body for statuses the spec does not document.
pub fn problem(status: u16, detail: &str) -> Value {
    json!({
        "type": "about:blank",
        "title": reason(status),
        "status": status,
        "detail": detail,
    })
}

/// Render the body for a status (shared by requests and deploy checks).
/// Returns (content type, body, source label).
pub fn render_body(
    c: &Compiled,
    op: &Operation,
    status: u16,
    choice: BodyChoice,
    accept: Option<&str>,
    echo: &[(String, String)],
) -> (Option<String>, Option<Body>, String) {
    if let Some(b) = choice.body {
        return match b {
            Value::String(s) => {
                let ct = if serde_json::from_str::<Value>(s).is_ok() {
                    "application/json"
                } else {
                    "text/plain; charset=utf-8"
                };
                (
                    Some(ct.into()),
                    Some(Body::Text(s.clone())),
                    "static".into(),
                )
            }
            other => (
                Some("application/json".into()),
                Some(Body::Json(other.clone())),
                "static".into(),
            ),
        };
    }
    if status < 200 || status == 204 || status == 304 {
        return (None, None, "empty".into());
    }
    let def: Option<&ResponseDef> = op.response_for(status);
    let Some(def) = def else {
        if status >= 400 {
            return (
                Some("application/problem+json".into()),
                Some(Body::Json(problem(
                    status,
                    "mock response: this status is not documented for the operation",
                ))),
                "undocumented".into(),
            );
        }
        return (None, None, "empty".into());
    };
    let Some(media) = negotiate(&def.content, accept) else {
        return (None, None, "empty".into());
    };
    let gen = |use_examples: bool| {
        media.schema.as_ref().map(|s| {
            schema::generate(
                s,
                &GenOptions {
                    root: &c.spec.root,
                    direction: Direction::Response,
                    seed: c.settings.responses.seed,
                    use_examples,
                    echo,
                },
            )
        })
    };
    let (value, source) = match (choice.example, choice.mode) {
        (Some(name), _) if media.named_example(name).is_some() => (
            media.named_example(name).cloned(),
            format!("example:{name}"),
        ),
        (_, ResponseMode::Static) => (None, "empty".to_string()),
        (_, ResponseMode::Generated) => match gen(false) {
            Some(v) => (Some(v), "generated".to_string()),
            None => spec_example(media, &gen),
        },
        (_, ResponseMode::Examples | ResponseMode::Stateful) => spec_example(media, &gen),
    };
    let Some(value) = value else {
        return (None, None, source);
    };
    let declared = media.media_type.clone();
    if media.is_json() {
        let ct = if declared == "*/*" {
            "application/json".to_string()
        } else {
            declared
        };
        (Some(ct), Some(Body::Json(value)), source)
    } else {
        match value {
            Value::String(s) => (Some(declared), Some(Body::Text(s)), source),
            other => (
                Some("application/json".into()),
                Some(Body::Json(other)),
                source,
            ),
        }
    }
}

/// The media-level example, the first named example, else generated data
/// that uses schema examples.
fn spec_example(media: &Media, gen: &dyn Fn(bool) -> Option<Value>) -> (Option<Value>, String) {
    if let Some(e) = &media.example {
        return (Some(e.clone()), "example".into());
    }
    if let Some(e) = media.examples.first() {
        return (Some(e.value.clone()), format!("example:{}", e.name));
    }
    match gen(true) {
        Some(v) => (Some(v), "generated".into()),
        None => (None, "empty".into()),
    }
}

/// Response headers documented for a status (values from examples or schemas).
pub fn spec_headers(c: &Compiled, op: &Operation, status: u16) -> Vec<(String, String)> {
    let Some(def) = op.response_for(status) else {
        return Vec::new();
    };
    def.headers
        .iter()
        .filter_map(|h| {
            let v = h.example.clone().unwrap_or_else(|| {
                schema::generate(
                    &h.schema,
                    &GenOptions {
                        root: &c.spec.root,
                        direction: Direction::Response,
                        seed: c.settings.responses.seed,
                        use_examples: true,
                        echo: &[],
                    },
                )
            });
            let text = text_of(&v);
            HeaderValue::from_str(&text)
                .ok()
                .map(|_| (h.name.clone(), text))
        })
        .collect()
}

/// Decide the full response for a request.
pub fn respond(c: &Compiled, op: &Operation, req: &RequestView) -> Outgoing {
    respond_with(c, op, req, None)
}

/// [`respond`], with the mock's state and the request path for `stateful`
/// mode.
pub fn respond_with(
    c: &Compiled,
    op: &Operation,
    req: &RequestView,
    stateful: Option<(&MockState, &str)>,
) -> Outgoing {
    let op_settings = c.settings.operations.get(&op.id);
    let base_mode = op_settings
        .and_then(|o| o.mode)
        .unwrap_or(c.settings.responses.mode);
    let delay_ms = op_settings
        .and_then(|o| o.delay_ms)
        .unwrap_or(c.settings.responses.delay_ms);
    let pref = preference(req.headers);

    // The override that applies, and a label for it.
    let mut chosen: Option<(&ResponseOverride, String)> = None;
    if pref.code.is_none() && pref.example.is_none() {
        if let Some(o) = op_settings {
            if let Some((i, rule)) = o
                .rules
                .iter()
                .enumerate()
                .find(|(_, r)| rule_matches(r, req))
            {
                let label = match &rule.name {
                    Some(n) => format!("rule:{n}"),
                    None => format!("rule:{}", i + 1),
                };
                chosen = Some((&rule.respond, label));
            } else if let Some(r) = &o.respond {
                chosen = Some((r, "override".into()));
            }
        }
    }
    let mode = match pref.dynamic {
        Some(true) => ResponseMode::Generated,
        Some(false) => ResponseMode::Examples,
        None => base_mode,
    };
    let explicit = chosen.is_some() || pref.code.is_some() || pref.example.is_some();
    if let (ResponseMode::Stateful, false, Some((state, path))) = (mode, explicit, stateful) {
        if let Some(out) = super::state::respond(c, op, req, path, state, delay_ms) {
            return out;
        }
    }
    let status = pref
        .code
        .or_else(|| chosen.as_ref().and_then(|(o, _)| o.status))
        .unwrap_or_else(|| op.default_status());
    let choice = BodyChoice {
        body: chosen.as_ref().and_then(|(o, _)| o.body.as_ref()),
        example: pref
            .example
            .as_deref()
            .or_else(|| chosen.as_ref().and_then(|(o, _)| o.example.as_deref())),
        mode,
    };
    let accept = req.headers.get("accept").and_then(|v| v.to_str().ok());
    let (content_type, body, body_source) =
        render_body(c, op, status, choice, accept, req.path_params);
    let mut headers = spec_headers(c, op, status);
    if let Some((o, _)) = &chosen {
        for (k, v) in &o.headers {
            headers.retain(|(name, _)| !name.eq_ignore_ascii_case(k));
            headers.push((k.clone(), v.clone()));
        }
    }
    let source = match (&chosen, pref.code.is_some() || pref.example.is_some()) {
        (_, true) => format!("prefer {body_source}"),
        (Some((_, label)), _) => format!("{label} {body_source}"),
        (None, _) => body_source,
    };
    Outgoing {
        status,
        headers,
        content_type,
        body,
        source,
        delay_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::compile::tests::compile;
    use crate::mocks::model::MockSettings;

    const SPEC: &str = r##"{
      "openapi": "3.0.3", "info": {"title": "t", "version": "1"},
      "paths": {
        "/pets/{petId}": {"get": {"operationId": "getPet",
          "parameters": [{"name": "petId", "in": "path", "required": true, "schema": {"type": "integer"}}],
          "responses": {
            "200": {"description": "ok",
              "headers": {"X-Rate-Limit": {"schema": {"type": "integer", "example": 100}}},
              "content": {
                "application/json": {"schema": {"$ref": "#/components/schemas/Pet"},
                  "examples": {"rex": {"value": {"id": 1, "name": "Rex"}}, "tom": {"value": {"id": 2, "name": "Tom"}}}},
                "text/plain": {"schema": {"type": "string"}, "example": "Rex"}
              }},
            "404": {"description": "missing", "content": {"application/json": {"schema": {"type": "object", "properties": {"code": {"type": "integer"}}}}}}
          }}},
        "/pets": {"post": {"operationId": "createPet", "responses": {"201": {"description": "created",
          "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Pet"}}}}}}}
      },
      "components": {"schemas": {"Pet": {"type": "object", "required": ["id", "name"],
        "properties": {"id": {"type": "integer"}, "name": {"type": "string"}}}}}
    }"##;

    fn view<'a>(
        params: &'a [(String, String)],
        headers: &'a HeaderMap,
        body: Option<&'a Value>,
    ) -> RequestView<'a> {
        RequestView {
            path_params: params,
            query: &[],
            headers,
            body_json: body,
            body_text: None,
        }
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).expect("value"));
        }
        h
    }

    #[test]
    fn examples_prefer_and_negotiation() {
        let c = compile(SPEC, MockSettings::default());
        let op = c.spec.operation("getPet").expect("op");
        let params = vec![("petId".to_string(), "7".to_string())];
        let h = HeaderMap::new();
        let out = respond(&c, op, &view(&params, &h, None));
        assert_eq!(out.status, 200);
        assert_eq!(out.body, Some(Body::Json(json!({"id": 1, "name": "Rex"}))));
        assert_eq!(out.source, "example:rex");
        assert_eq!(
            out.headers,
            vec![("X-Rate-Limit".to_string(), "100".to_string())]
        );

        let h = headers(&[("prefer", "example=tom")]);
        let out = respond(&c, op, &view(&params, &h, None));
        assert_eq!(out.body, Some(Body::Json(json!({"id": 2, "name": "Tom"}))));

        let h = headers(&[("prefer", "code=404, dynamic=true")]);
        let out = respond(&c, op, &view(&params, &h, None));
        assert_eq!(out.status, 404);
        assert!(out.body.expect("body").as_json().expect("json")["code"].is_i64());

        let h = headers(&[("x-rustybin-mock-status", "418")]);
        let out = respond(&c, op, &view(&params, &h, None));
        assert_eq!(out.status, 418);
        assert_eq!(
            out.content_type.as_deref(),
            Some("application/problem+json")
        );

        let h = headers(&[("accept", "text/plain")]);
        let out = respond(&c, op, &view(&params, &h, None));
        assert_eq!(out.body, Some(Body::Text("Rex".into())));
        assert_eq!(out.content_type.as_deref(), Some("text/plain"));
    }

    #[test]
    fn generated_mode_echoes_path_params() {
        let mut settings = MockSettings::default();
        settings.responses.mode = ResponseMode::Generated;
        let c = compile(SPEC, settings);
        let op = c.spec.operation("getPet").expect("op");
        let params = vec![("petId".to_string(), "42".to_string())];
        let h = HeaderMap::new();
        let out = respond(&c, op, &view(&params, &h, None));
        assert_eq!(out.source, "generated");
        assert_eq!(out.body.expect("body").as_json().expect("json")["id"], 42);
    }

    #[test]
    fn rules_and_overrides() {
        let settings: MockSettings = serde_json::from_value(json!({
            "operations": {
                "getPet": {
                    "rules": [
                        {"when": {"path.petId": {"equals": 0}}, "respond": {"status": 404}},
                        {"name": "vip", "when": {"header.x-tier": {"matches": "^gold$"}}, "respond": {"body": {"id": 9, "name": "Goldie"}, "headers": {"X-Tier": "gold"}}}
                    ],
                    "respond": {"example": "tom"}
                },
                "createPet": {"respond": {"status": 202, "body": "accepted"}}
            }
        }))
        .expect("settings");
        let c = compile(SPEC, settings);
        let op = c.spec.operation("getPet").expect("op");
        let zero = vec![("petId".to_string(), "0".to_string())];
        let one = vec![("petId".to_string(), "1".to_string())];
        let h = HeaderMap::new();
        assert_eq!(respond(&c, op, &view(&zero, &h, None)).status, 404);
        let out = respond(&c, op, &view(&one, &h, None));
        assert_eq!(out.source, "override example:tom");
        let h = headers(&[("x-tier", "gold")]);
        let out = respond(&c, op, &view(&one, &h, None));
        assert_eq!(out.source, "rule:vip static");
        assert!(out
            .headers
            .contains(&("X-Tier".to_string(), "gold".to_string())));
        // Prefer beats rules.
        let h = headers(&[("x-tier", "gold"), ("prefer", "code=200")]);
        assert_eq!(
            respond(&c, op, &view(&one, &h, None)).source,
            "prefer example:rex"
        );

        let op = c.spec.operation("createPet").expect("op");
        let out = respond(&c, op, &view(&[], &HeaderMap::new(), None));
        assert_eq!(out.status, 202);
        assert_eq!(out.body, Some(Body::Text("accepted".into())));
    }

    #[test]
    fn body_rules_and_static_mode() {
        let mut settings: MockSettings = serde_json::from_value(json!({
            "operations": {"createPet": {"rules": [
                {"when": {"body.name": {"equals": "fail"}}, "respond": {"status": 422}},
                {"when": {"body": {"exists": false}}, "respond": {"status": 400}}
            ]}}
        }))
        .expect("settings");
        settings.responses.mode = ResponseMode::Static;
        let c = compile(SPEC, settings);
        let op = c.spec.operation("createPet").expect("op");
        let h = HeaderMap::new();
        let body = json!({"name": "fail"});
        assert_eq!(respond(&c, op, &view(&[], &h, Some(&body))).status, 422);
        assert_eq!(respond(&c, op, &view(&[], &h, None)).status, 400);
        let ok = json!({"name": "Rex"});
        let out = respond(&c, op, &view(&[], &h, Some(&ok)));
        assert_eq!(out.status, 201);
        assert!(out.body.is_none());
    }

    #[test]
    fn accept_negotiation() {
        let media = |t: &str| Media {
            media_type: t.into(),
            schema: None,
            example: None,
            examples: Vec::new(),
        };
        let content = vec![media("application/xml"), media("application/json")];
        assert_eq!(
            negotiate(&content, None).expect("m").media_type,
            "application/json"
        );
        assert_eq!(
            negotiate(&content, Some("application/xml;q=0.9, text/html"))
                .expect("m")
                .media_type,
            "application/xml"
        );
        assert_eq!(
            negotiate(&content, Some("application/*"))
                .expect("m")
                .media_type,
            "application/xml"
        );
        assert_eq!(
            negotiate(&content, Some("*/*")).expect("m").media_type,
            "application/json"
        );
        assert_eq!(
            negotiate(&content, Some("image/png"))
                .expect("m")
                .media_type,
            "application/json"
        );
    }
}
