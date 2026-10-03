//! Capped versions of httpbin's data transfer and redirect helpers.
//!
//! - `/bytes/{n}`, `/stream-bytes/{n}`: random bytes (`?seed=` for
//!   deterministic output), at most [`MAX_BYTES`] (public [`MAX_BYTES_PUBLIC`]).
//! - `/stream/{n}`: `n` JSON lines, at most 100 (public 20).
//! - `/drip`: bytes dripped over a duration after an optional delay.
//! - `/links/{n}/{offset}`: an HTML page of links.
//! - `/base64/{value}`: decode base64 (standard or URL-safe).
//! - `/redirect-to?url=`: redirects ONLY to relative paths or to the request's
//!   own host, so it can never be used as an open redirect.
//! - `/absolute-redirect/{n}`: chain of absolute redirects on the same host.
//!
//! Oversized counts are clamped to the cap (as httpbin does), and the
//! response says so with `X-Rustybin-Capped`.
// Handlers return early with ready-made responses (as admin::require_admin does).
#![allow(clippy::result_large_err)]

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use base64::Engine;
use rand::{RngCore, SeedableRng};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::session::ClientIp;
use crate::state::AppState;

/// Largest `/bytes` and `/stream-bytes` body (normal mode).
pub const MAX_BYTES: usize = 100 * 1024;
/// Largest `/bytes` and `/stream-bytes` body in public mode.
pub const MAX_BYTES_PUBLIC: usize = 10 * 1024;
/// Most `/stream` lines (normal / public).
pub const MAX_STREAM_LINES: usize = 100;
pub const MAX_STREAM_LINES_PUBLIC: usize = 20;
/// Most `/drip` bytes (normal / public).
pub const MAX_DRIP_BYTES: usize = 10 * 1024;
pub const MAX_DRIP_BYTES_PUBLIC: usize = 1024;
/// Most links on a `/links` page.
pub const MAX_LINKS: usize = 200;
/// Longest `/absolute-redirect` chain.
pub const MAX_ABSOLUTE_REDIRECTS: u32 = 10;

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn caps(config: &Config) -> (usize, usize, usize) {
    if config.public_mode {
        (
            MAX_BYTES_PUBLIC,
            MAX_STREAM_LINES_PUBLIC,
            MAX_DRIP_BYTES_PUBLIC,
        )
    } else {
        (MAX_BYTES, MAX_STREAM_LINES, MAX_DRIP_BYTES)
    }
}

/// Parse a count path segment and clamp it to `cap`; `(value, capped)`.
fn count(raw: &str, cap: usize) -> Result<(usize, bool), Response> {
    match raw.parse::<u64>() {
        Ok(n) if n as u128 > cap as u128 => Ok((cap, true)),
        Ok(n) => Ok((n as usize, false)),
        Err(_) => Err(json_error(
            StatusCode::BAD_REQUEST,
            "count must be a non-negative integer",
        )),
    }
}

fn mark_capped(resp: &mut Response, capped: bool, cap: usize) {
    if capped {
        if let Ok(v) = HeaderValue::from_str(&cap.to_string()) {
            resp.headers_mut().insert("x-rustybin-capped", v);
        }
    }
}

fn rng(seed: Option<u64>) -> rand::rngs::StdRng {
    match seed {
        Some(s) => rand::rngs::StdRng::seed_from_u64(s),
        None => rand::rngs::StdRng::from_entropy(),
    }
}

#[derive(Debug, Default, Deserialize)]
struct BytesQuery {
    seed: Option<u64>,
    chunk_size: Option<usize>,
}

async fn bytes_handler(
    State(config): State<Arc<Config>>,
    Path(n): Path<String>,
    Query(q): Query<BytesQuery>,
) -> Response {
    let cap = caps(&config).0;
    let (n, capped) = match count(&n, cap) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let mut data = vec![0u8; n];
    rng(q.seed).fill_bytes(&mut data);
    let mut resp = ([(header::CONTENT_TYPE, "application/octet-stream")], data).into_response();
    mark_capped(&mut resp, capped, cap);
    resp
}

async fn stream_bytes_handler(
    State(config): State<Arc<Config>>,
    Path(n): Path<String>,
    Query(q): Query<BytesQuery>,
) -> Response {
    let cap = caps(&config).0;
    let (n, capped) = match count(&n, cap) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let chunk = q.chunk_size.unwrap_or(10 * 1024).clamp(1, cap.max(1));
    let mut r = rng(q.seed);
    let stream = async_stream::stream! {
        let mut left = n;
        while left > 0 {
            let size = left.min(chunk);
            let mut buf = vec![0u8; size];
            r.fill_bytes(&mut buf);
            left -= size;
            yield Ok::<Bytes, Infallible>(Bytes::from(buf));
        }
    };
    let mut resp = Response::new(Body::from_stream(stream));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    mark_capped(&mut resp, capped, cap);
    resp
}

fn header_map(headers: &HeaderMap) -> BTreeMap<String, String> {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in headers {
        let v = String::from_utf8_lossy(v.as_bytes()).into_owned();
        map.entry(k.as_str().to_string())
            .and_modify(|e| {
                e.push_str(", ");
                e.push_str(&v);
            })
            .or_insert(v);
    }
    map
}

async fn stream_handler(
    State(config): State<Arc<Config>>,
    Path(n): Path<String>,
    Query(args): Query<BTreeMap<String, String>>,
    ClientIp(ip): ClientIp,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let cap = caps(&config).1;
    let (n, capped) = match count(&n, cap) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let base = json!({
        "url": uri.to_string(),
        "args": args,
        "headers": header_map(&headers),
        "origin": ip.map(|i| i.to_string()),
    });
    let stream = async_stream::stream! {
        for id in 0..n {
            let mut line = base.clone();
            if let Some(obj) = line.as_object_mut() {
                obj.insert("id".into(), json!(id));
            }
            let mut text = line.to_string();
            text.push('\n');
            yield Ok::<Bytes, Infallible>(Bytes::from(text));
        }
    };
    let mut resp = Response::new(Body::from_stream(stream));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    mark_capped(&mut resp, capped, cap);
    resp
}

#[derive(Debug, Default, Deserialize)]
struct DripQuery {
    duration: Option<f64>,
    numbytes: Option<usize>,
    code: Option<u16>,
    delay: Option<f64>,
}

/// Seconds to a duration, rejecting negatives, NaN and values above `max_ms`.
fn seconds(
    value: Option<f64>,
    default: f64,
    max_ms: u64,
    name: &str,
) -> Result<Duration, Response> {
    let v = value.unwrap_or(default);
    if !v.is_finite() || v < 0.0 || v * 1000.0 > max_ms as f64 {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            &format!(
                "{name} must be between 0 and {} seconds",
                max_ms as f64 / 1000.0
            ),
        ));
    }
    Ok(Duration::from_secs_f64(v))
}

async fn drip_handler(State(config): State<Arc<Config>>, Query(q): Query<DripQuery>) -> Response {
    let max_ms = config.max_delay_ms();
    let duration = match seconds(q.duration, 2.0, max_ms, "duration") {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let delay = match seconds(q.delay, 0.0, max_ms, "delay") {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let cap = caps(&config).2;
    let numbytes = q.numbytes.unwrap_or(10);
    if numbytes > cap {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("numbytes must be at most {cap}"),
        );
    }
    let status = match q.code.unwrap_or(200) {
        c @ 200..=599 => StatusCode::from_u16(c).unwrap_or(StatusCode::OK),
        _ => return json_error(StatusCode::BAD_REQUEST, "code must be between 200 and 599"),
    };
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    // At most 200 writes, spread evenly over the duration.
    let chunks = numbytes.clamp(1, 200);
    let pause = duration / chunks as u32;
    let stream = async_stream::stream! {
        let mut left = numbytes;
        for i in 0..chunks {
            if i > 0 {
                tokio::time::sleep(pause).await;
            }
            let size = left / (chunks - i);
            left -= size;
            if size > 0 {
                yield Ok::<Bytes, Infallible>(Bytes::from(vec![b'*'; size]));
            }
        }
    };
    let mut resp = Response::new(Body::from_stream(stream));
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    if let Ok(v) = HeaderValue::from_str(&numbytes.to_string()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    resp
}

async fn links_handler(Path((n, offset)): Path<(String, String)>) -> Response {
    let (Ok(n), Ok(offset)) = (n.parse::<usize>(), offset.parse::<usize>()) else {
        return json_error(StatusCode::BAD_REQUEST, "n and offset must be integers");
    };
    let n = n.min(MAX_LINKS);
    let mut html = String::from("<html><head><title>Links</title></head><body>");
    for i in 0..n {
        if i == offset {
            html.push_str(&format!("{i} "));
        } else {
            html.push_str(&format!("<a href='/links/{n}/{i}'>{i}</a> "));
        }
    }
    html.push_str("</body></html>");
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

async fn links_root_handler(Path(n): Path<String>) -> Response {
    match n.parse::<usize>() {
        Ok(n) => redirect(StatusCode::FOUND, &format!("/links/{}/0", n.min(MAX_LINKS))),
        Err(_) => json_error(StatusCode::BAD_REQUEST, "n must be an integer"),
    }
}

async fn base64_handler(Path(value): Path<String>) -> Response {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    let v = value.trim();
    let decoded = STANDARD
        .decode(v)
        .or_else(|_| URL_SAFE.decode(v))
        .or_else(|_| STANDARD_NO_PAD.decode(v))
        .or_else(|_| URL_SAFE_NO_PAD.decode(v));
    match decoded {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => {
                ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response()
            }
            Err(e) => (
                [(header::CONTENT_TYPE, "application/octet-stream")],
                e.into_bytes(),
            )
                .into_response(),
        },
        Err(_) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "incorrect base64 data",
                "example": "/base64/SFRUUEJJTiBpcyBhd2Vzb21l",
            })),
        )
            .into_response(),
    }
}

fn redirect(status: StatusCode, location: &str) -> Response {
    let mut resp = status.into_response();
    if let Ok(v) = HeaderValue::from_str(location) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    resp
}

/// The request's own `Host` (validated: host and port characters only).
fn request_host(headers: &HeaderMap) -> Option<String> {
    let host = headers.get(header::HOST)?.to_str().ok()?.trim();
    let ok = !host.is_empty()
        && host.len() <= 255
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'));
    ok.then(|| host.to_ascii_lowercase())
}

/// Why a `/redirect-to` target is refused, or `Ok` when it is safe: a path
/// starting with a single `/`, or an http(s) URL on the request's own host.
pub fn check_redirect_target(target: &str, own_host: Option<&str>) -> Result<(), &'static str> {
    if target.is_empty() {
        return Err("url is required");
    }
    if target.chars().any(|c| c.is_control() || c.is_whitespace()) || target.contains('\\') {
        return Err("url must not contain whitespace, control characters or backslashes");
    }
    if target.starts_with('/') {
        if target.starts_with("//") {
            return Err("protocol-relative URLs (//host) are not allowed");
        }
        return Ok(());
    }
    let lower = target.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
        .ok_or("only relative paths (/...) or http(s) URLs on this host are allowed")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err("URLs with credentials are not allowed");
    }
    match own_host {
        Some(host) if authority == host => Ok(()),
        _ => Err("absolute URLs must point to this host (no open redirects)"),
    }
}

#[derive(Debug, Default, Deserialize)]
struct RedirectToQuery {
    url: Option<String>,
    status_code: Option<u16>,
}

async fn redirect_to_handler(Query(q): Query<RedirectToQuery>, headers: HeaderMap) -> Response {
    let status = match q.status_code.unwrap_or(302) {
        c @ (300..=303 | 307 | 308) => StatusCode::from_u16(c).unwrap_or(StatusCode::FOUND),
        _ => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "status_code must be 300, 301, 302, 303, 307 or 308",
            )
        }
    };
    let target = q.url.unwrap_or_default();
    let host = request_host(&headers);
    match check_redirect_target(&target, host.as_deref()) {
        Ok(()) => redirect(status, &target),
        Err(reason) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "redirect target refused", "reason": reason, "url": target })),
        )
            .into_response(),
    }
}

async fn absolute_redirect_handler(
    State(config): State<Arc<Config>>,
    Path(n): Path<String>,
    uri: Uri,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let n = match n.parse::<u32>() {
        Ok(n) if (1..=MAX_ABSOLUTE_REDIRECTS).contains(&n) => n,
        _ => {
            return json_error(
                StatusCode::BAD_REQUEST,
                &format!("n must be between 1 and {MAX_ABSOLUTE_REDIRECTS}"),
            )
        }
    };
    if request_host(&headers).is_none() {
        return json_error(StatusCode::BAD_REQUEST, "a valid Host header is required");
    }
    // Listener scheme (https on the TLS listener), Host, and proxy headers
    // only with RUSTYBIN_TRUST_FORWARD.
    let base = crate::session::request_origin(&headers, &extensions, &uri, &config).base_url();
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let location = if n == 1 {
        format!("{base}/echo{query}")
    } else {
        format!("{base}/absolute-redirect/{}{query}", n - 1)
    };
    redirect(StatusCode::FOUND, &location)
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/bytes/{n}", get(bytes_handler))
        .route("/stream/{n}", get(stream_handler))
        .route("/stream-bytes/{n}", get(stream_bytes_handler))
        .route("/drip", get(drip_handler))
        .route("/links/{n}", get(links_root_handler))
        .route("/links/{n}/{offset}", get(links_handler))
        .route("/base64/{value}", get(base64_handler))
        .route("/redirect-to", any(redirect_to_handler))
        .route("/absolute-redirect/{n}", get(absolute_redirect_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new("/bytes/{n}", &["GET"], category::SHAPING, "n random bytes (seed for deterministic output)")
            .description("n is capped at 102400 (public 10240); X-Rustybin-Capped reports clamping. ?seed= makes the output reproducible.")
            .example(Example::get("1 KiB of random bytes", "/bytes/1024"))
            .example(Example::get("Deterministic bytes", "/bytes/64?seed=42")),
        Endpoint::new("/stream/{n}", &["GET"], category::SHAPING, "n JSON lines streamed (chunked)")
            .description("Each line echoes url, args, headers and origin with an id. n is capped at 100 (public 20).")
            .example(Example::get("Stream 5 JSON lines", "/stream/5")),
        Endpoint::new("/stream-bytes/{n}", &["GET"], category::SHAPING, "n random bytes streamed in chunks")
            .description("?chunk_size= (default 10240), ?seed=. Same cap as /bytes.")
            .example(Example::get("Stream 4 KiB in 1 KiB chunks", "/stream-bytes/4096?chunk_size=1024&seed=1")),
        Endpoint::new("/drip", &["GET"], category::SHAPING, "Drip bytes over a duration (slow body)")
            .description("?duration= seconds (default 2), ?numbytes= (default 10, max 10240; public 1024), ?code= status, ?delay= seconds before headers. Duration and delay are capped by the instance delay limit.")
            .example(Example::get("Drip 10 bytes over 1 s", "/drip?duration=1&numbytes=10&code=200&delay=0")),
        Endpoint::new("/links/{n}", &["GET"], category::SHAPING, "Redirect to /links/{n}/0")
            .example(Example::get("Links page redirect", "/links/5").expect_status(302)),
        Endpoint::new("/links/{n}/{offset}", &["GET"], category::SHAPING, "HTML page with n links (max 200)")
            .example(Example::get("Page of 10 links", "/links/10/0")),
        Endpoint::new("/base64/{value}", &["GET"], category::SHAPING, "Decode a base64 (standard or URL-safe) value")
            .example(Example::get("Decode base64", "/base64/SFRUUEJJTiBpcyBhd2Vzb21l")),
        Endpoint::new("/redirect-to", &["ANY"], category::REDIRECTS, "Redirect to a relative path or this host only (no open redirect)")
            .description("?url= must be a path starting with / (not //) or an http(s) URL whose host:port equals the request Host; anything else is 400. ?status_code= 300, 301, 302 (default), 303, 307 or 308.")
            .example(Example::get("Redirect to /echo", "/redirect-to?url=/echo&status_code=307").expect_status(307))
            .example(Example::get("External URL is refused", "/redirect-to?url=https://example.com/").expect_status(400)),
        Endpoint::new("/absolute-redirect/{n}", &["GET"], category::REDIRECTS, "Chain of n absolute 302 redirects on the same host (max 10)")
            .example(Example::get("Absolute redirect chain (3)", "/absolute-redirect/3").header("Host", "localhost").expect_status(302)),
    ]
}

pub fn openapi_paths() -> Value {
    let tags = json!(["Response Shaping"]);
    let n = |desc: &str| json!({ "name": "n", "in": "path", "required": true, "schema": { "type": "integer", "minimum": 0 }, "description": desc });
    let seed = json!({ "name": "seed", "in": "query", "required": false, "schema": { "type": "integer" } });
    let octets = json!({ "description": "Bytes", "content": { "application/octet-stream": {} } });
    json!({
        "/bytes/{n}": { "get": {
            "tags": tags, "summary": "Random bytes", "operationId": "bytes",
            "parameters": [n("Number of bytes (capped)"), seed],
            "responses": { "200": octets, "400": { "description": "Invalid n" } }
        } },
        "/stream/{n}": { "get": {
            "tags": tags, "summary": "Stream n JSON lines", "operationId": "streamLines",
            "parameters": [n("Number of lines (capped)")],
            "responses": { "200": { "description": "Newline delimited JSON", "content": { "application/json": {} } } }
        } },
        "/stream-bytes/{n}": { "get": {
            "tags": tags, "summary": "Stream random bytes", "operationId": "streamBytes",
            "parameters": [n("Number of bytes (capped)"), seed,
                { "name": "chunk_size", "in": "query", "required": false, "schema": { "type": "integer", "default": 10240 } }],
            "responses": { "200": octets }
        } },
        "/drip": { "get": {
            "tags": tags, "summary": "Drip bytes over time", "operationId": "drip",
            "parameters": [
                { "name": "duration", "in": "query", "schema": { "type": "number", "default": 2 } },
                { "name": "numbytes", "in": "query", "schema": { "type": "integer", "default": 10 } },
                { "name": "code", "in": "query", "schema": { "type": "integer", "default": 200 } },
                { "name": "delay", "in": "query", "schema": { "type": "number", "default": 0 } }
            ],
            "responses": { "200": octets, "400": { "description": "Parameter out of range" } }
        } },
        "/links/{n}": { "get": {
            "tags": tags, "summary": "Redirect to the first links page", "operationId": "linksRoot",
            "parameters": [n("Number of links")],
            "responses": { "302": { "description": "Redirect to /links/{n}/0" } }
        } },
        "/links/{n}/{offset}": { "get": {
            "tags": tags, "summary": "HTML page of links", "operationId": "links",
            "parameters": [n("Number of links (max 200)"),
                { "name": "offset", "in": "path", "required": true, "schema": { "type": "integer" } }],
            "responses": { "200": { "description": "HTML", "content": { "text/html": {} } } }
        } },
        "/base64/{value}": { "get": {
            "tags": tags, "summary": "Decode base64", "operationId": "base64Decode",
            "parameters": [{ "name": "value", "in": "path", "required": true, "schema": { "type": "string" } }],
            "responses": { "200": { "description": "Decoded text or bytes" }, "400": { "description": "Invalid base64" } }
        } },
        "/redirect-to": { "get": {
            "tags": ["Redirects & Cookies"], "summary": "Redirect to a relative or same-host URL", "operationId": "redirectTo",
            "parameters": [
                { "name": "url", "in": "query", "required": true, "schema": { "type": "string" } },
                { "name": "status_code", "in": "query", "required": false, "schema": { "type": "integer", "default": 302 } }
            ],
            "responses": { "302": { "description": "Redirect" }, "400": { "description": "Target refused (external or malformed)" } }
        } },
        "/absolute-redirect/{n}": { "get": {
            "tags": ["Redirects & Cookies"], "summary": "Absolute redirect chain", "operationId": "absoluteRedirect",
            "parameters": [n("Remaining redirects (1-10)")],
            "responses": { "302": { "description": "Redirect" }, "400": { "description": "Invalid n or Host" } }
        } }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_bytes, body_string, get_request, module_app};
    use axum::http::Request;
    use tower::ServiceExt;

    fn with_host(uri: &str, host: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header("host", host)
            .body(Body::empty())
            .expect("request")
    }

    #[tokio::test]
    async fn bytes_are_capped_and_seeded() {
        let app = module_app(router);
        let a = body_bytes(
            app.clone()
                .oneshot(get_request("/bytes/32?seed=7"))
                .await
                .expect("r"),
        )
        .await;
        let b = body_bytes(
            app.clone()
                .oneshot(get_request("/bytes/32?seed=7"))
                .await
                .expect("r"),
        )
        .await;
        assert_eq!(a.len(), 32);
        assert_eq!(a, b);
        let resp = app
            .clone()
            .oneshot(get_request("/bytes/999999"))
            .await
            .expect("r");
        assert_eq!(resp.headers()["x-rustybin-capped"], "102400");
        assert_eq!(body_bytes(resp).await.len(), MAX_BYTES);
        let resp = app
            .clone()
            .oneshot(get_request("/stream-bytes/2500?chunk_size=1000&seed=7"))
            .await
            .expect("r");
        assert_eq!(body_bytes(resp).await.len(), 2500);

        let mut config = Config::for_tests();
        config.public_mode = true;
        let app = crate::test_support::module_app_with_config(config, router);
        let resp = app.oneshot(get_request("/bytes/20000")).await.expect("r");
        assert_eq!(body_bytes(resp).await.len(), MAX_BYTES_PUBLIC);
    }

    #[tokio::test]
    async fn stream_lines() {
        let app = module_app(router);
        let text = body_string(
            app.clone()
                .oneshot(get_request("/stream/3?a=b"))
                .await
                .expect("r"),
        )
        .await;
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).expect("json"))
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2]["id"], 2);
        assert_eq!(lines[0]["args"]["a"], "b");
        let text = body_string(app.oneshot(get_request("/stream/500")).await.expect("r")).await;
        assert_eq!(text.lines().count(), MAX_STREAM_LINES);
    }

    #[tokio::test]
    async fn drip_and_validation() {
        let app = module_app(router);
        let resp = app
            .clone()
            .oneshot(get_request("/drip?duration=0.1&numbytes=5&code=201"))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(resp.headers()["content-length"], "5");
        assert_eq!(body_string(resp).await, "*****");
        for uri in [
            "/drip?numbytes=20000",
            "/drip?duration=-1",
            "/drip?duration=99999",
            "/drip?code=99",
        ] {
            let resp = app.clone().oneshot(get_request(uri)).await.expect("r");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
        }
    }

    #[tokio::test]
    async fn links_and_base64() {
        let app = module_app(router);
        let html = body_string(
            app.clone()
                .oneshot(get_request("/links/3/1"))
                .await
                .expect("r"),
        )
        .await;
        assert!(html.contains("<a href='/links/3/0'>0</a> 1 <a href='/links/3/2'>2</a>"));
        let resp = app
            .clone()
            .oneshot(get_request("/links/4"))
            .await
            .expect("r");
        assert_eq!(resp.headers()["location"], "/links/4/0");
        let resp = app
            .clone()
            .oneshot(get_request("/base64/SFRUUEJJTiBpcyBhd2Vzb21l"))
            .await
            .expect("r");
        assert_eq!(body_string(resp).await, "HTTPBIN is awesome");
        let resp = app
            .clone()
            .oneshot(get_request("/base64/aGk_"))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = app.oneshot(get_request("/base64/!!!")).await.expect("r");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn redirect_targets() {
        let host = Some("rustybin.test:8080");
        assert!(check_redirect_target("/echo?a=1", host).is_ok());
        assert!(check_redirect_target("http://rustybin.test:8080/echo", host).is_ok());
        assert!(check_redirect_target("HTTPS://RUSTYBIN.TEST:8080", host).is_ok());
        for bad in [
            "https://example.com/",
            "//example.com/x",
            "/\\example.com",
            "http://rustybin.test:8080@example.com/",
            "http://rustybin.test/",
            "javascript:alert(1)",
            "echo",
            "/echo\r\nSet-Cookie: x=1",
            "",
        ] {
            assert!(check_redirect_target(bad, host).is_err(), "{bad:?}");
        }
        assert!(check_redirect_target("http://rustybin.test:8080/", None).is_err());
    }

    #[tokio::test]
    async fn redirect_to_endpoint() {
        let app = module_app(router);
        let resp = app
            .clone()
            .oneshot(with_host(
                "/redirect-to?url=/echo&status_code=307",
                "h.test",
            ))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(resp.headers()["location"], "/echo");
        let resp = app
            .clone()
            .oneshot(with_host(
                "/redirect-to?url=http%3A%2F%2Fh.test%2Fecho",
                "h.test",
            ))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::FOUND);
        assert_eq!(resp.headers()["location"], "http://h.test/echo");
        for uri in [
            "/redirect-to?url=https%3A%2F%2Fevil.example%2F",
            "/redirect-to?url=%2F%2Fevil.example",
            "/redirect-to?url=/echo&status_code=200",
            "/redirect-to",
        ] {
            let resp = app
                .clone()
                .oneshot(with_host(uri, "h.test"))
                .await
                .expect("r");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert!(resp.headers().get("location").is_none());
        }
    }

    #[tokio::test]
    async fn absolute_redirects() {
        let app = module_app(router);
        let resp = app
            .clone()
            .oneshot(with_host("/absolute-redirect/2?x=1", "h.test:81"))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::FOUND);
        assert_eq!(
            resp.headers()["location"],
            "http://h.test:81/absolute-redirect/1?x=1"
        );
        let resp = app
            .clone()
            .oneshot(with_host("/absolute-redirect/1", "h.test"))
            .await
            .expect("r");
        assert_eq!(resp.headers()["location"], "http://h.test/echo");
        for uri in ["/absolute-redirect/0", "/absolute-redirect/11"] {
            let resp = app
                .clone()
                .oneshot(with_host(uri, "h.test"))
                .await
                .expect("r");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        }
        let resp = app
            .clone()
            .oneshot(with_host("/absolute-redirect/1", "evil host"))
            .await
            .expect("r");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        // On the HTTPS listener the chain stays on https.
        let mut req = with_host("/absolute-redirect/1", "localhost:8443");
        req.extensions_mut()
            .insert(crate::session::ListenerInfo::https(8443));
        let resp = app.oneshot(req).await.expect("r");
        assert_eq!(resp.headers()["location"], "https://localhost:8443/echo");
    }
}
