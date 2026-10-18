//! Response rules for visitor mocks (self-service, `docs/plans/hosted-mocks.md`
//! section 6): a visitor controls the bodies and headers, so their mocks
//! never set cookies, never redirect to another host, never ship active
//! content (HTML, SVG, scripts are served as `text/plain`) and always carry
//! `nosniff` and a sandboxing Content-Security-Policy.

use axum::http::{header, HeaderMap, HeaderName, HeaderValue};
use axum::response::Response;

use super::compile::Compiled;
use super::respond;

/// The Content-Security-Policy of every visitor mock response.
pub const CSP: &str = "sandbox; default-src 'none'; frame-ancestors 'none'";

/// Headers a visitor mock may not send (some only with certain values, see
/// [`header_denied`]).
const DENIED: &[&str] = &[
    "set-cookie",
    "set-cookie2",
    "content-security-policy",
    "content-security-policy-report-only",
    "access-control-allow-credentials",
    "strict-transport-security",
    "clear-site-data",
    "x-frame-options",
    "x-content-type-options",
];

/// A value pointing at another host (`https://...`, `//host`, `javascript:`).
fn points_elsewhere(value: &str) -> bool {
    let v = value.trim().to_ascii_lowercase();
    // `Refresh: 0; url=...` carries the URL after `url=`.
    let target = v
        .split_once("url=")
        .map_or(v.as_str(), |(_, rest)| rest.trim_matches(['"', '\'', ' ']));
    target.starts_with("//")
        || target.starts_with("\\\\")
        || target
            .split_once(':')
            .is_some_and(|(scheme, _)| !scheme.is_empty() && !scheme.contains('/'))
}

/// May a visitor mock send this header?
pub fn header_denied(name: &str, value: &str) -> bool {
    let n = name.to_ascii_lowercase();
    DENIED.contains(&n.as_str())
        || n.starts_with("content-security-policy")
        || (matches!(n.as_str(), "location" | "refresh" | "content-location")
            && points_elsewhere(value))
}

/// May a visitor mock answer with this media type? Inert data formats only.
pub fn media_allowed(content_type: &str) -> bool {
    let base = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let Some((kind, sub)) = base.split_once('/') else {
        return false;
    };
    match kind {
        "text" => matches!(sub, "plain" | "csv" | "xml"),
        "application" => {
            matches!(
                sub,
                "json" | "xml" | "octet-stream" | "x-www-form-urlencoded" | "x-ndjson" | "yaml"
            ) || (sub.ends_with("+json"))
                || (sub.ends_with("+xml") && sub != "xhtml+xml")
        }
        _ => false,
    }
}

/// The media type a visitor mock actually sends for `content_type`.
pub fn served_media(content_type: &str) -> &str {
    if media_allowed(content_type) {
        content_type
    } else {
        "text/plain; charset=utf-8"
    }
}

/// Apply the rules to a finished response.
pub fn harden(resp: &mut Response) {
    harden_headers(resp.headers_mut());
}

pub fn harden_headers(h: &mut HeaderMap) {
    let names: Vec<HeaderName> = h.keys().cloned().collect();
    for name in names {
        let denied = h
            .get_all(&name)
            .iter()
            .any(|v| header_denied(name.as_str(), v.to_str().unwrap_or("//")));
        if denied {
            h.remove(&name);
        }
    }
    if let Some(ct) = h.get(header::CONTENT_TYPE) {
        let text = ct.to_str().unwrap_or("");
        if !media_allowed(text) {
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
        }
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
}

/// Settings a visitor may not save: configured headers that would be
/// dropped (refused up front instead of silently removed).
pub fn check_header(name: &str, value: &str) -> Result<(), String> {
    if header_denied(name, value) {
        return Err(format!(
            "header {name:?} cannot be set on a self-service mock (cookies, cross-host redirects \
             and security headers are reserved)"
        ));
    }
    if name.eq_ignore_ascii_case("content-type") && !media_allowed(value) {
        return Err(format!(
            "content type {value:?} cannot be served by a self-service mock: use JSON, XML, \
             CSV or plain text"
        ));
    }
    Ok(())
}

/// Deploy warnings for a visitor mock's spec: documented headers that are
/// dropped and media types served as `text/plain`.
pub fn findings(c: &Compiled) -> Vec<String> {
    let mut out = Vec::new();
    for op in &c.spec.operations {
        for def in &op.responses {
            if let Some(code) = def.code() {
                for (name, value) in respond::spec_headers(c, op, code) {
                    if header_denied(&name, &value) {
                        out.push(format!(
                            "{} {}: header {name:?} of the {} response is not sent by \
                             self-service mocks",
                            op.method.to_uppercase(),
                            op.path,
                            def.status
                        ));
                    }
                }
            }
            for media in &def.content {
                if !media_allowed(&media.media_type) {
                    out.push(format!(
                        "{} {}: the {} response's {} body is served as text/plain by \
                         self-service mocks",
                        op.method.to_uppercase(),
                        op.path,
                        def.status,
                        media.media_type
                    ));
                }
            }
        }
        if out.len() >= 50 {
            break;
        }
    }
    out.truncate(50);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    #[test]
    fn headers_and_media_types() {
        for (name, value) in [
            ("Set-Cookie", "a=b"),
            ("Location", "https://evil.example/"),
            ("location", "//evil.example/x"),
            ("Location", "javascript:alert(1)"),
            ("Refresh", "0; url=https://evil.example/"),
            ("Content-Security-Policy", "default-src *"),
            ("Access-Control-Allow-Credentials", "true"),
            ("Strict-Transport-Security", "max-age=1"),
            ("Clear-Site-Data", "\"*\""),
        ] {
            assert!(header_denied(name, value), "{name}: {value}");
        }
        for (name, value) in [
            ("Location", "/v1/pets/1"),
            ("Location", "pets/1"),
            ("Refresh", "5"),
            ("X-Rate-Limit", "10"),
            ("Cache-Control", "no-store"),
        ] {
            assert!(!header_denied(name, value), "{name}: {value}");
        }
        for ok in [
            "application/json",
            "application/problem+json; charset=utf-8",
            "application/xml",
            "application/atom+xml",
            "text/plain",
            "text/csv",
            "application/octet-stream",
        ] {
            assert!(media_allowed(ok), "{ok}");
        }
        for bad in [
            "text/html",
            "image/svg+xml",
            "application/xhtml+xml",
            "application/javascript",
            "text/javascript",
            "image/png",
            "",
            "nonsense",
        ] {
            assert!(!media_allowed(bad), "{bad}");
        }
        assert!(check_header("content-type", "text/html").is_err());
        assert!(check_header("x-trace", "1").is_ok());
    }

    #[test]
    fn harden_rewrites_the_response() {
        let mut resp = (
            [
                ("content-type", "text/html"),
                ("set-cookie", "session=1"),
                ("location", "https://evil.example/"),
                ("content-security-policy", "default-src *"),
                ("x-kept", "yes"),
            ],
            "<script>alert(1)</script>",
        )
            .into_response();
        harden(&mut resp);
        let h = resp.headers();
        assert_eq!(h["content-type"], "text/plain; charset=utf-8");
        assert!(h.get("set-cookie").is_none());
        assert!(h.get("location").is_none());
        assert_eq!(h["content-security-policy"], CSP);
        assert_eq!(h["x-content-type-options"], "nosniff");
        assert_eq!(h["x-frame-options"], "DENY");
        assert_eq!(h["x-kept"], "yes");
    }
}
