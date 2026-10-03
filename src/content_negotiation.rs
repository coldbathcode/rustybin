//! JSON / XML content negotiation for plain data responses.
//!
//! - `Accept` is parsed with q-values (RFC 9110 section 12.5.1). Supported
//!   representations, in server preference order: `application/json`,
//!   `application/xml`, `text/xml`. Anything else (or no `Accept`) gets JSON:
//!   this is a lenient demo service, so it never answers 406.
//! - Every negotiated response carries `Vary: Accept`.
//! - XML is produced from the JSON value: an XML declaration, a `<response>`
//!   root, one element per object key, `<item>` elements for array entries.
//!   Keys that are not safe XML element names become
//!   `<entry key="original key">`, so serialisation never fails on user data.

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::Value;

/// The representation chosen for a response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    /// `application/xml`
    Xml,
    /// `text/xml`
    TextXml,
}

impl Format {
    pub fn content_type(self) -> &'static str {
        match self {
            Format::Json => "application/json",
            Format::Xml => "application/xml",
            Format::TextXml => "text/xml",
        }
    }

    pub fn is_xml(self) -> bool {
        matches!(self, Format::Xml | Format::TextXml)
    }
}

/// One media range from an `Accept` header.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaRange {
    pub type_: String,
    pub subtype: String,
    pub q: f32,
}

impl MediaRange {
    /// Specificity of the match against `type/subtype` (higher wins), or `None`.
    fn matches(&self, type_: &str, subtype: &str) -> Option<u8> {
        match (self.type_.as_str(), self.subtype.as_str()) {
            ("*", "*") => Some(0),
            (t, "*") if t == type_ => Some(1),
            (t, s) if t == type_ && s == subtype => Some(2),
            _ => None,
        }
    }
}

/// Parse an `Accept` header value into media ranges (invalid entries skipped).
pub fn parse_accept(value: &str) -> Vec<MediaRange> {
    value
        .split(',')
        .take(64)
        .filter_map(|part| {
            let mut params = part.split(';');
            let media = params.next()?.trim().to_ascii_lowercase();
            let (type_, subtype) = media.split_once('/')?;
            if type_.is_empty() || subtype.is_empty() {
                return None;
            }
            let mut q = 1.0f32;
            for p in params {
                if let Some((k, v)) = p.split_once('=') {
                    if k.trim().eq_ignore_ascii_case("q") {
                        q = v.trim().parse::<f32>().ok().filter(|q| q.is_finite())?;
                        q = q.clamp(0.0, 1.0);
                    }
                }
            }
            Some(MediaRange {
                type_: type_.trim().to_string(),
                subtype: subtype.trim().to_string(),
                q,
            })
        })
        .collect()
}

/// The q-value the client assigns to `type/subtype` (most specific range
/// wins), or `None` when no range matches.
pub fn quality(ranges: &[MediaRange], type_: &str, subtype: &str) -> Option<f32> {
    ranges
        .iter()
        .filter_map(|r| r.matches(type_, subtype).map(|spec| (spec, r.q)))
        .max_by_key(|(spec, _)| *spec)
        .map(|(_, q)| q)
}

/// Choose among `candidates` (server preference order). Returns the index of
/// the best acceptable candidate, or `None` when the client accepts none of
/// them (or sent no `Accept`, in which case the caller picks its default).
pub fn choose(headers: &HeaderMap, candidates: &[(&str, &str)]) -> Option<usize> {
    let accept: Vec<&str> = headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    if accept.is_empty() {
        return None;
    }
    let ranges = parse_accept(&accept.join(","));
    let mut best: Option<(usize, f32)> = None;
    for (i, (t, s)) in candidates.iter().enumerate() {
        if let Some(q) = quality(&ranges, t, s) {
            if q > 0.0 && best.is_none_or(|(_, bq)| q > bq) {
                best = Some((i, q));
            }
        }
    }
    best.map(|(i, _)| i)
}

/// The response format for this request (JSON unless XML is preferred).
pub fn preferred_format(headers: &HeaderMap) -> Format {
    const CANDIDATES: [(&str, &str); 3] = [
        ("application", "json"),
        ("application", "xml"),
        ("text", "xml"),
    ];
    match choose(headers, &CANDIDATES) {
        Some(1) => Format::Xml,
        Some(2) => Format::TextXml,
        _ => Format::Json,
    }
}

/// Negotiate the response format based on the Accept header (status 200).
pub fn negotiate<T: Serialize>(headers: &HeaderMap, data: &T) -> Response {
    negotiate_with_status(headers, data, StatusCode::OK)
}

/// Negotiate with a custom status code.
pub fn negotiate_with_status<T: Serialize>(
    headers: &HeaderMap,
    data: &T,
    status: StatusCode,
) -> Response {
    let format = preferred_format(headers);
    // JSON is serialised directly (keeps struct field order); XML goes
    // through a JSON value.
    let rendered = if format.is_xml() {
        serde_json::to_value(data).map(|v| to_xml_document(&v, "response"))
    } else {
        serde_json::to_string(data)
    };
    let mut resp = match rendered {
        Ok(body) => render(format, body, status),
        Err(e) => {
            let err =
                serde_json::json!({ "error": "serialization_error", "details": e.to_string() });
            let body = if format.is_xml() {
                to_xml_document(&err, "response")
            } else {
                err.to_string()
            };
            render(format, body, StatusCode::INTERNAL_SERVER_ERROR)
        }
    };
    add_vary(resp.headers_mut(), "Accept");
    resp
}

fn render(format: Format, body: String, status: StatusCode) -> Response {
    (
        status,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static(format.content_type()),
        )],
        body,
    )
        .into_response()
}

/// Add `token` to `Vary` unless already listed.
pub fn add_vary(headers: &mut HeaderMap, token: &str) {
    let present = headers
        .get_all(header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|t| t.trim().eq_ignore_ascii_case(token) || t.trim() == "*");
    if !present {
        if let Ok(v) = HeaderValue::from_str(token) {
            headers.append(header::VARY, v);
        }
    }
}

// ── XML rendering ───────────────────────────────────────────────────

/// Escape text for XML element content and attribute values.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Characters not allowed in XML 1.0 are replaced.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => out.push('\u{FFFD}'),
            '\u{FFFE}' | '\u{FFFF}' => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out
}

/// A conservative XML element name check: ASCII letter or `_` first, then
/// letters, digits, `-`, `_`, `.`; no `xml` prefix (reserved), max 128 chars.
pub fn is_safe_xml_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 128
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        && !name.to_ascii_lowercase().starts_with("xml")
}

/// Render a JSON value as a complete XML document with the given root element.
pub fn to_xml_document(value: &Value, root: &str) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    write_element(&mut out, root, value, 0);
    out
}

/// Render a JSON value as an XML fragment rooted at `name`.
pub fn to_xml_fragment(value: &Value, name: &str) -> String {
    let mut out = String::new();
    write_element(&mut out, name, value, 0);
    out
}

const MAX_XML_DEPTH: usize = 64;

fn write_element(out: &mut String, name: &str, value: &Value, depth: usize) {
    let (open, close) = if is_safe_xml_name(name) {
        (format!("<{name}"), format!("</{name}>"))
    } else {
        (
            format!("<entry key=\"{}\"", xml_escape(name)),
            "</entry>".to_string(),
        )
    };
    out.push_str(&open);
    match value {
        Value::Null => out.push_str("/>"),
        Value::Bool(b) => {
            out.push('>');
            out.push_str(if *b { "true" } else { "false" });
            out.push_str(&close);
        }
        Value::Number(n) => {
            out.push('>');
            out.push_str(&n.to_string());
            out.push_str(&close);
        }
        Value::String(s) => {
            out.push('>');
            out.push_str(&xml_escape(s));
            out.push_str(&close);
        }
        _ if depth >= MAX_XML_DEPTH => {
            out.push('>');
            out.push_str(&xml_escape(&value.to_string()));
            out.push_str(&close);
        }
        Value::Array(items) => {
            out.push('>');
            for item in items {
                write_element(out, "item", item, depth + 1);
            }
            out.push_str(&close);
        }
        Value::Object(map) => {
            out.push('>');
            for (k, v) in map {
                write_element(out, k, v, depth + 1);
            }
            out.push_str(&close);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::body_string;
    use serde_json::json;

    fn accept(v: &'static str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT, HeaderValue::from_static(v));
        h
    }

    #[test]
    fn q_values_pick_the_preferred_format() {
        assert_eq!(preferred_format(&HeaderMap::new()), Format::Json);
        assert_eq!(preferred_format(&accept("application/xml")), Format::Xml);
        assert_eq!(preferred_format(&accept("text/xml")), Format::TextXml);
        assert_eq!(
            preferred_format(&accept("application/json;q=0.5, application/xml")),
            Format::Xml
        );
        assert_eq!(
            preferred_format(&accept("application/xml;q=0.9, application/json")),
            Format::Json
        );
        // q=0 means "not acceptable".
        assert_eq!(
            preferred_format(&accept("application/json;q=0, */*;q=0.1")),
            Format::Xml
        );
        assert_eq!(preferred_format(&accept("*/*")), Format::Json);
        assert_eq!(preferred_format(&accept("text/*")), Format::TextXml);
        // Unsupported types fall back to JSON.
        assert_eq!(preferred_format(&accept("image/png")), Format::Json);
        assert_eq!(preferred_format(&accept("garbage;;q=x")), Format::Json);
        // Browsers: text/html first, then application/xml;q=0.9, */*;q=0.8.
        assert_eq!(
            preferred_format(&accept(
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
            )),
            Format::Xml
        );
    }

    #[test]
    fn xml_names_are_sanitised() {
        let v = json!({
            "ok": 1,
            "bad key": "x",
            "1abc": true,
            "a<b": null,
            "xmlns": "reserved",
            "list": [1, "two", {"k": "<v>&"}],
        });
        let xml = to_xml_document(&v, "response");
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(xml.contains("<response>"));
        assert!(xml.contains("<ok>1</ok>"));
        assert!(xml.contains("<entry key=\"bad key\">x</entry>"));
        assert!(xml.contains("<entry key=\"1abc\">true</entry>"));
        assert!(xml.contains("<entry key=\"a&lt;b\"/>"));
        assert!(xml.contains("<entry key=\"xmlns\">reserved</entry>"));
        assert!(xml.contains(
            "<list><item>1</item><item>two</item><item><k>&lt;v&gt;&amp;</k></item></list>"
        ));
        // Well-formed: parses with quick-xml end to end.
        let mut reader = quick_xml::Reader::from_str(&xml);
        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(_) => {}
                Err(e) => panic!("invalid XML: {e}"),
            }
        }
    }

    #[tokio::test]
    async fn responses_carry_vary_and_content_type() {
        let resp = negotiate(&accept("text/xml"), &json!({"a": "b"}));
        assert_eq!(resp.headers()["content-type"], "text/xml");
        assert_eq!(resp.headers()["vary"], "Accept");
        let body = body_string(resp).await;
        assert!(body.contains("<response><a>b</a></response>"));

        let resp = negotiate_with_status(&HeaderMap::new(), &json!({"a": 1}), StatusCode::CREATED);
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(resp.headers()["content-type"], "application/json");
        assert_eq!(resp.headers()["vary"], "Accept");
    }

    #[test]
    fn control_characters_are_replaced() {
        assert_eq!(xml_escape("a\u{0}b"), "a\u{FFFD}b");
        assert_eq!(xml_escape("tab\there"), "tab\there");
    }
}
