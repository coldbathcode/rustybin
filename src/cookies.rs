//! Cookie endpoints: read, set (with attributes), delete.
//!
//! - Names must be RFC 6265 tokens (else 400). Values are percent-encoded
//!   where they contain characters outside `cookie-octet` (space, `"`, `,`,
//!   `;`, `\`, controls, non-ASCII), so a value can never inject attributes.
//! - `Path=/` is the default; `_path`, `_domain` are validated.
//! - `SameSite=None` forces `Secure` (browsers reject it otherwise).
//! - `/cookies/delete` expires cookies with the same `Path` (and `Domain`)
//!   they were set with (`_path` / `_domain`, default `/`).

use axum::{
    extract::Path,
    http::{header, HeaderMap, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde::Serialize;
use std::collections::BTreeMap;

use crate::catalog::{category, Endpoint, Example};
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::state::AppState;
use crate::types::ErrorResponse;

/// Maximum number of cookies one request may set or delete.
const MAX_COOKIES: usize = 20;
/// Maximum encoded cookie value length.
const MAX_VALUE_LEN: usize = 4096;

// ── /cookies ─────────────────────────────────────────────────────────

#[derive(Serialize)]
struct CookiesResponse {
    cookies: BTreeMap<String, String>,
}

async fn cookies_handler(headers: HeaderMap) -> Response {
    negotiate(
        &headers,
        &CookiesResponse {
            cookies: parse_cookies(&headers),
        },
    )
}

fn bad_request(headers: &HeaderMap, details: String) -> Response {
    negotiate_with_status(
        headers,
        &ErrorResponse {
            error: "invalid_cookie".to_string(),
            details: Some(details),
        },
        StatusCode::BAD_REQUEST,
    )
}

fn redirect_with(cookies: Vec<String>) -> Response {
    let mut resp = StatusCode::FOUND.into_response();
    resp.headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static("/cookies"));
    for c in cookies {
        if let Ok(v) = HeaderValue::from_str(&c) {
            resp.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    resp
}

fn query_pairs(uri: &Uri) -> Vec<(String, String)> {
    form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
        .into_owned()
        .collect()
}

// ── /cookies/set ─────────────────────────────────────────────────────

async fn cookies_set_handler(uri: Uri, headers: HeaderMap) -> Response {
    let mut attrs = CookieAttrs::default();
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (key, value) in query_pairs(&uri) {
        match key.as_str() {
            "_path" => attrs.path = Some(value),
            "_domain" => attrs.domain = Some(value),
            "_secure" => attrs.secure = is_true(&value),
            "_httponly" => attrs.httponly = is_true(&value),
            "_samesite" => attrs.samesite = Some(value),
            "_maxage" => match value.parse::<i64>() {
                Ok(v) => attrs.max_age = Some(v),
                Err(_) => return bad_request(&headers, "_maxage must be an integer".into()),
            },
            _ => pairs.push((key, value)),
        }
    }
    if pairs.len() > MAX_COOKIES {
        return bad_request(
            &headers,
            format!("at most {MAX_COOKIES} cookies per request"),
        );
    }
    let mut out = Vec::with_capacity(pairs.len());
    for (name, value) in &pairs {
        match build_set_cookie(name, value, &attrs) {
            Ok(c) => out.push(c),
            Err(e) => return bad_request(&headers, e),
        }
    }
    redirect_with(out)
}

fn is_true(v: &str) -> bool {
    matches!(
        v.to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on" | ""
    )
}

// ── /cookies/set/{name}/{value} ──────────────────────────────────────

async fn cookies_set_single_handler(
    Path((name, value)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    match build_set_cookie(&name, &value, &CookieAttrs::default()) {
        Ok(c) => redirect_with(vec![c]),
        Err(e) => bad_request(&headers, e),
    }
}

// ── /cookies/delete ──────────────────────────────────────────────────

async fn cookies_delete_handler(uri: Uri, headers: HeaderMap) -> Response {
    let mut attrs = CookieAttrs {
        max_age: Some(0),
        ..CookieAttrs::default()
    };
    let mut names = Vec::new();
    for (key, value) in query_pairs(&uri) {
        match key.as_str() {
            "_path" => attrs.path = Some(value),
            "_domain" => attrs.domain = Some(value),
            k if k.starts_with('_') => {}
            _ => names.push(key),
        }
    }
    if names.len() > MAX_COOKIES {
        return bad_request(
            &headers,
            format!("at most {MAX_COOKIES} cookies per request"),
        );
    }
    let mut out = Vec::with_capacity(names.len());
    for name in &names {
        match build_set_cookie(name, "", &attrs) {
            Ok(c) => out.push(format!("{c}; Expires=Thu, 01 Jan 1970 00:00:00 GMT")),
            Err(e) => return bad_request(&headers, e),
        }
    }
    redirect_with(out)
}

// ── Helpers ──────────────────────────────────────────────────────────

fn parse_cookies(headers: &HeaderMap) -> BTreeMap<String, String> {
    let mut cookies = BTreeMap::new();
    for value in headers.get_all(header::COOKIE) {
        if let Ok(s) = value.to_str() {
            for pair in s.split(';') {
                if let Some((name, val)) = pair.trim().split_once('=') {
                    let val = val.trim();
                    let val = val
                        .strip_prefix('"')
                        .and_then(|v| v.strip_suffix('"'))
                        .unwrap_or(val);
                    cookies.insert(name.trim().to_string(), val.to_string());
                }
            }
        }
    }
    cookies
}

#[derive(Default)]
struct CookieAttrs {
    path: Option<String>,
    domain: Option<String>,
    secure: bool,
    httponly: bool,
    samesite: Option<String>,
    max_age: Option<i64>,
}

/// RFC 6265 `token` (RFC 7230 tchar).
fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// RFC 6265 `cookie-octet`.
fn is_cookie_octet(b: u8) -> bool {
    matches!(b, 0x21 | 0x23..=0x2B | 0x2D..=0x3A | 0x3C..=0x5B | 0x5D..=0x7E)
}

/// Percent-encode every byte that is not a `cookie-octet` (and `%` itself,
/// so the encoding is unambiguous).
pub fn encode_cookie_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if is_cookie_octet(b) && b != b'%' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Attribute values (Path, Domain) may not contain `;` or controls.
fn valid_attr(s: &str) -> bool {
    !s.is_empty() && s.len() <= 1024 && s.bytes().all(|b| (0x20..0x7f).contains(&b) && b != b';')
}

fn build_set_cookie(name: &str, value: &str, attrs: &CookieAttrs) -> Result<String, String> {
    if !is_token(name) || name.starts_with('_') {
        return Err(format!(
            "cookie name {name:?} must be an RFC 6265 token (letters, digits and !#$%&'*+-.^`|~, \
             not starting with _)"
        ));
    }
    let value = encode_cookie_value(value);
    if value.len() > MAX_VALUE_LEN {
        return Err(format!("cookie value longer than {MAX_VALUE_LEN} bytes"));
    }
    let mut parts = vec![format!("{name}={value}")];

    let path = attrs.path.as_deref().unwrap_or("/");
    if !valid_attr(path) || !path.starts_with('/') {
        return Err("_path must start with / and contain no ; or control characters".into());
    }
    parts.push(format!("Path={path}"));
    if let Some(domain) = &attrs.domain {
        let ok = valid_attr(domain)
            && domain
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
        if !ok {
            return Err("_domain must be a host name".into());
        }
        parts.push(format!("Domain={domain}"));
    }
    if let Some(max_age) = attrs.max_age {
        parts.push(format!("Max-Age={max_age}"));
    }
    let samesite = match attrs.samesite.as_deref().map(str::to_ascii_lowercase) {
        None => None,
        Some(s) if s == "strict" => Some("Strict"),
        Some(s) if s == "lax" => Some("Lax"),
        Some(s) if s == "none" => Some("None"),
        Some(_) => return Err("_samesite must be Strict, Lax or None".into()),
    };
    // Browsers reject SameSite=None without Secure.
    if attrs.secure || samesite == Some("None") {
        parts.push("Secure".to_string());
    }
    if attrs.httponly {
        parts.push("HttpOnly".to_string());
    }
    if let Some(s) = samesite {
        parts.push(format!("SameSite={s}"));
    }
    Ok(parts.join("; "))
}

// ── Router ───────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/cookies", get(cookies_handler))
        .route("/cookies/set", get(cookies_set_handler))
        .route(
            "/cookies/set/{name}/{value}",
            get(cookies_set_single_handler),
        )
        .route("/cookies/delete", get(cookies_delete_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/cookies",
            &["GET"],
            category::REDIRECTS,
            "Request cookies as JSON",
        )
        .example(Example::get("Get cookies", "/cookies")),
        Endpoint::new(
            "/cookies/set",
            &["GET"],
            category::REDIRECTS,
            "Set cookies from query parameters, then 302 to /cookies",
        )
        .description(
            "Attributes: `_path` (default /), `_domain`, `_secure`, `_httponly`, \
             `_samesite` (Strict, Lax, None; None forces Secure), `_maxage`. Names must be \
             tokens; values are percent-encoded where needed.",
        )
        .example(Example::get(
            "Set cookies",
            "/cookies/set?session=abc123&theme=dark",
        ))
        .example(Example::get(
            "Cross-site cookie",
            "/cookies/set?session=abc123&_samesite=None&_httponly=true",
        )),
        Endpoint::new(
            "/cookies/set/{name}/{value}",
            &["GET"],
            category::REDIRECTS,
            "Set a single cookie (Path=/)",
        )
        .example(Example::get("Set one cookie", "/cookies/set/theme/dark")),
        Endpoint::new(
            "/cookies/delete",
            &["GET"],
            category::REDIRECTS,
            "Expire the cookies named in the query string (_path / _domain must match how they were set)",
        )
        .example(Example::get("Delete cookies", "/cookies/delete?session")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_json, get_request};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app() -> Router {
        crate::test_support::module_app(router)
    }

    async fn set_cookies(uri: &str) -> (StatusCode, Vec<String>) {
        let resp = test_app()
            .oneshot(get_request(uri))
            .await
            .expect("response");
        let cookies = resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok().map(str::to_string))
            .collect();
        (resp.status(), cookies)
    }

    #[tokio::test]
    async fn cookies_returns_request_cookies() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/cookies")
                    .header("cookie", "session=abc123; theme=\"dark\"")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["cookies"]["session"], "abc123");
        assert_eq!(json["cookies"]["theme"], "dark");
    }

    #[tokio::test]
    async fn set_defaults_to_root_path_and_redirects() {
        let (status, cookies) = set_cookies("/cookies/set?session=abc123&theme=dark").await;
        assert_eq!(status, StatusCode::FOUND);
        assert_eq!(cookies.len(), 2);
        assert!(cookies.iter().all(|c| c.contains("; Path=/")));
        let (_, cookies) = set_cookies("/cookies/set/token/xyz789").await;
        assert_eq!(cookies, vec!["token=xyz789; Path=/".to_string()]);
    }

    #[tokio::test]
    async fn attributes_and_samesite_none_forces_secure() {
        let (_, c) =
            set_cookies("/cookies/set?session=abc&_path=/api&_secure=true&_httponly=true").await;
        assert_eq!(c[0], "session=abc; Path=/api; Secure; HttpOnly");
        let (_, c) = set_cookies("/cookies/set?s=1&_samesite=none").await;
        assert_eq!(c[0], "s=1; Path=/; Secure; SameSite=None");
        let (status, _) = set_cookies("/cookies/set?s=1&_samesite=sometimes").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn values_are_encoded_and_names_validated() {
        // ";" and CR/LF in a value cannot inject attributes or headers.
        let (status, c) =
            set_cookies("/cookies/set?s=a%3B%20Domain%3Devil.com%0D%0AX-Injected%3A%201").await;
        assert_eq!(status, StatusCode::FOUND);
        assert_eq!(
            c[0],
            "s=a%3B%20Domain=evil.com%0D%0AX-Injected:%201; Path=/"
        );
        for uri in [
            "/cookies/set?bad%20name=1",
            "/cookies/set?a%3Bb=1",
            "/cookies/set?s=1&_path=/x%3BDomain%3Devil",
            "/cookies/set?s=1&_domain=evil.com%3B",
            "/cookies/set?s=1&_maxage=soon",
        ] {
            let (status, _) = set_cookies(uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        }
        let (status, _) = set_cookies("/cookies/set/bad%20name/x").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_uses_matching_path() {
        let (status, cookies) = set_cookies("/cookies/delete?session&theme").await;
        assert_eq!(status, StatusCode::FOUND);
        assert_eq!(cookies.len(), 2);
        for c in &cookies {
            assert!(c.contains("Max-Age=0"));
            assert!(c.contains("; Path=/;"));
            assert!(c.contains("Expires=Thu, 01 Jan 1970"));
        }
        let (_, cookies) = set_cookies("/cookies/delete?session&_path=/api").await;
        assert_eq!(cookies.len(), 1);
        assert!(cookies[0].starts_with("session=; Path=/api; Max-Age=0"));
    }

    #[tokio::test]
    async fn cookies_xml_negotiation() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/cookies")
                    .header("accept", "application/xml")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.headers()["content-type"], "application/xml");
    }
}
