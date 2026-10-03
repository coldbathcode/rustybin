//! Response shaping: `/delay/{duration}`, `/response-headers`, `/cache/{ttl}`.

use axum::{
    extract::{Extension, Path, Query, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{any, get},
    Router,
};
use chrono::{DateTime, NaiveDateTime, SubsecRound, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::content_negotiation::{negotiate, negotiate_with_status, preferred_format, Format};
use crate::state::AppState;
use crate::types::ErrorResponse;

/// Maximum `/delay` (including jitter), normal mode.
pub const MAX_DELAY_MS: u64 = 60_000;
/// Maximum `/delay` (including jitter), public mode.
pub const MAX_DELAY_MS_PUBLIC: u64 = 10_000;
/// Maximum number of headers `/response-headers` sets.
const MAX_RESPONSE_HEADERS: usize = 50;

#[derive(Clone)]
struct ServerStartup(DateTime<Utc>);

fn error(headers: &HeaderMap, status: StatusCode, error: &str, details: String) -> Response {
    negotiate_with_status(
        headers,
        &ErrorResponse {
            error: error.to_string(),
            details: Some(details),
        },
        status,
    )
}

// ── Delay ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct DelayQuery {
    #[serde(default)]
    jitter: Option<bool>,
}

#[derive(Serialize)]
struct DelayResponse {
    delay_ms: u64,
    requested_ms: u64,
    max_delay_ms: u64,
    method: String,
    path: String,
    query_string: String,
    headers: BTreeMap<String, Vec<String>>,
    timestamp_unix_ms: u64,
}

fn max_delay_ms(config: &Config) -> u64 {
    if config.public_mode {
        MAX_DELAY_MS_PUBLIC
    } else {
        MAX_DELAY_MS
    }
}

/// Parse a delay: plain milliseconds (`1500`), or with a unit: `250ms`,
/// `1.5s`. Returns milliseconds.
pub fn parse_delay(raw: &str) -> Option<u64> {
    let raw = raw.trim().to_ascii_lowercase();
    let (num, factor) = if let Some(n) = raw.strip_suffix("ms") {
        (n, 1.0)
    } else if let Some(n) = raw.strip_suffix('s') {
        (n, 1000.0)
    } else {
        (raw.as_str(), 1.0)
    };
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let value: f64 = num.parse().ok()?;
    let ms = (value * factor).round();
    (ms.is_finite() && (0.0..=u64::MAX as f64).contains(&ms)).then_some(ms as u64)
}

/// Scale `requested` by the jitter factor, never exceeding `max`.
fn apply_jitter(requested: u64, max: u64, factor: f64) -> u64 {
    ((requested as f64 * factor) as u64).min(max)
}

async fn delay_handler(
    State(config): State<Arc<Config>>,
    Path(raw): Path<String>,
    Query(query): Query<DelayQuery>,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let max = max_delay_ms(&config);
    let requested = match parse_delay(&raw) {
        Some(v) if v <= max => v,
        _ => {
            return error(
                &headers,
                StatusCode::BAD_REQUEST,
                "invalid_delay",
                format!(
                    "Delay must be 0..={max} ms: plain milliseconds (1500), `250ms` or seconds (`1.5s`)"
                ),
            );
        }
    };

    let actual = if query.jitter == Some(true) {
        apply_jitter(requested, max, rand::thread_rng().gen_range(0.8..=1.2))
    } else {
        requested
    };

    tokio::time::sleep(Duration::from_millis(actual)).await;

    let mut header_map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in headers.iter() {
        header_map
            .entry(name.to_string())
            .or_default()
            .push(value.to_str().unwrap_or("<binary>").to_string());
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    negotiate(
        &headers,
        &DelayResponse {
            delay_ms: actual,
            requested_ms: requested,
            max_delay_ms: max,
            method: method.to_string(),
            path: uri.path().to_string(),
            query_string: uri.query().unwrap_or("").to_string(),
            headers: header_map,
            timestamp_unix_ms: timestamp,
        },
    )
}

// ── Response Headers ─────────────────────────────────────────────────

/// Headers that describe the connection or the message framing. Setting them
/// from user input would corrupt the response (or smuggle a second one).
const FORBIDDEN_RESPONSE_HEADERS: &[&str] = &[
    "connection",
    "content-length",
    "transfer-encoding",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "upgrade",
    "host",
    "http2-settings",
];

async fn response_headers_handler(uri: Uri, headers: HeaderMap) -> Response {
    let params: Vec<(String, String)> =
        form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    if params.len() > MAX_RESPONSE_HEADERS {
        return error(
            &headers,
            StatusCode::BAD_REQUEST,
            "too_many_headers",
            format!("At most {MAX_RESPONSE_HEADERS} headers can be set"),
        );
    }

    let mut set = serde_json::Map::new();
    let mut custom = HeaderMap::new();
    for (key, value) in &params {
        let lower = key.to_ascii_lowercase();
        if FORBIDDEN_RESPONSE_HEADERS.contains(&lower.as_str()) {
            return error(
                &headers,
                StatusCode::BAD_REQUEST,
                "forbidden_header",
                format!(
                    "{key:?} is a hop-by-hop or framing header and cannot be set \
                     (forbidden: {})",
                    FORBIDDEN_RESPONSE_HEADERS.join(", ")
                ),
            );
        }
        let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(key.as_bytes()),
            HeaderValue::from_str(value),
        ) else {
            return error(
                &headers,
                StatusCode::BAD_REQUEST,
                "invalid_header",
                format!("{key:?} is not a valid header name/value pair"),
            );
        };
        custom.append(name, val);
        match set.get_mut(&lower) {
            Some(serde_json::Value::Array(list)) => list.push(value.clone().into()),
            Some(existing) => {
                let first = existing.take();
                *existing = serde_json::Value::Array(vec![first, value.clone().into()]);
            }
            None => {
                set.insert(lower, value.clone().into());
            }
        }
    }

    let mut resp = (
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::Value::Object(set).to_string(),
    )
        .into_response();
    // Replaces our Content-Type when the caller sets one.
    resp.headers_mut().extend(custom);
    resp
}

// ── Cache ────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct CacheResponse {
    cached: bool,
    ttl: u32,
    generated_at: String,
    etag: String,
}

/// One entity tag from `If-None-Match`.
fn parse_etag_list(value: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut rest = value.trim();
    while !rest.is_empty() {
        rest = rest.trim_start_matches([',', ' ', '\t']);
        if rest.is_empty() {
            break;
        }
        if let Some(r) = rest.strip_prefix('*') {
            tags.push("*".to_string());
            rest = r;
            continue;
        }
        let weak = rest.starts_with("W/");
        let body = if weak { &rest[2..] } else { rest };
        if let Some(after_quote) = body.strip_prefix('"') {
            match after_quote.find('"') {
                Some(end) => {
                    tags.push(after_quote[..end].to_string());
                    rest = &after_quote[end + 1..];
                }
                None => break,
            }
        } else {
            // Unquoted (non-conforming) tag: take up to the next comma.
            let end = body.find(',').unwrap_or(body.len());
            tags.push(body[..end].trim().to_string());
            rest = &body[end..];
        }
        if tags.len() >= 64 {
            break;
        }
    }
    tags
}

/// Weak comparison (RFC 9110 section 8.8.3.2), as required for If-None-Match.
fn none_match_hits(headers: &HeaderMap, opaque: &str) -> Option<bool> {
    let values: Vec<&str> = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    if values.is_empty() {
        return None;
    }
    let tags = parse_etag_list(&values.join(","));
    Some(tags.iter().any(|t| t == "*" || t == opaque))
}

async fn cache_handler(
    Extension(ServerStartup(startup)): Extension<ServerStartup>,
    Path(ttl): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Ok(ttl_val) = ttl.parse::<u32>() else {
        return error(
            &headers,
            StatusCode::BAD_REQUEST,
            "invalid_ttl",
            "TTL must be a non-negative integer (seconds)".to_string(),
        );
    };

    let format = preferred_format(&headers);
    let etag_value = compute_etag(ttl_val, &startup, format);
    let etag_header = format!("\"{etag_value}\"");
    let last_modified = startup.format("%a, %d %b %Y %H:%M:%S GMT").to_string();

    let validators = |resp: &mut Response| {
        let h = resp.headers_mut();
        if let Ok(v) = HeaderValue::from_str(&format!("public, max-age={ttl_val}")) {
            h.insert(header::CACHE_CONTROL, v);
        }
        if let Ok(v) = HeaderValue::from_str(&etag_header) {
            h.insert(header::ETAG, v);
        }
        if let Ok(v) = HeaderValue::from_str(&last_modified) {
            h.insert(header::LAST_MODIFIED, v);
        }
        h.insert(header::VARY, HeaderValue::from_static("Accept"));
    };

    // RFC 9110 section 13.2.2: If-None-Match takes precedence; If-Modified-Since
    // is only evaluated when If-None-Match is absent.
    let not_modified = match none_match_hits(&headers, &etag_value) {
        Some(hit) => hit,
        None => headers
            .get(header::IF_MODIFIED_SINCE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_http_date)
            .is_some_and(|ims| startup <= ims),
    };
    if not_modified {
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        validators(&mut resp);
        return resp;
    }

    let mut resp = negotiate(
        &headers,
        &CacheResponse {
            cached: true,
            ttl: ttl_val,
            generated_at: startup.to_rfc3339(),
            etag: etag_value.clone(),
        },
    );
    validators(&mut resp);
    resp
}

fn compute_etag(ttl: u32, startup: &DateTime<Utc>, format: Format) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ttl.hash(&mut hasher);
    startup.timestamp().hash(&mut hasher);
    format.content_type().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// IMF-fixdate (the only format servers generate; obsolete formats ignored).
fn parse_http_date(s: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(s.trim(), "%a, %d %b %Y %H:%M:%S GMT")
        .ok()
        .map(|dt| dt.and_utc())
}

// ── Router ───────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    // Last-Modified has second precision: truncate so comparisons with
    // If-Modified-Since behave.
    let startup = ServerStartup(Utc::now().trunc_subsecs(0));
    Router::new()
        .route("/delay/{ms}", any(delay_handler))
        .route("/response-headers", get(response_headers_handler))
        .route("/cache/{ttl}", get(cache_handler))
        .layer(Extension(startup))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/delay/{ms}",
            &["ANY"],
            category::SHAPING,
            "Wait, then respond: milliseconds (1500), `250ms` or seconds (`1.5s`); ?jitter=true adds +-20%",
        )
        .description(
            "Maximum 60 s (10 s in public mode), jitter included: larger values return 400.",
        )
        .example(Example::get("Delay 500ms", "/delay/500"))
        .example(Example::get("Delay 1.5s", "/delay/1.5s")),
        Endpoint::new(
            "/cache/{ttl}",
            &["GET"],
            category::SHAPING,
            "Cache-Control, ETag and Last-Modified; 304 on If-None-Match / If-Modified-Since",
        )
        .description(
            "If-None-Match (lists, weak tags, `*`) takes precedence over If-Modified-Since. \
             JSON and XML representations have different ETags (Vary: Accept).",
        )
        .example(Example::get("Cache 60s", "/cache/60")),
        Endpoint::new(
            "/response-headers",
            &["GET"],
            category::SHAPING,
            "Query parameters become response headers (hop-by-hop / framing headers refused)",
        )
        .example(Example::get(
            "Response headers",
            "/response-headers?X-Custom=hello&X-Trace-Id=abc123",
        )),
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

    fn req(uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
        let mut b = Request::builder().uri(uri);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        b.body(Body::empty()).expect("request")
    }

    #[test]
    fn delay_parsing() {
        assert_eq!(parse_delay("1500"), Some(1500));
        assert_eq!(parse_delay("250ms"), Some(250));
        assert_eq!(parse_delay("1.5s"), Some(1500));
        assert_eq!(parse_delay("0"), Some(0));
        assert_eq!(parse_delay("-1"), None);
        assert_eq!(parse_delay("abc"), None);
        assert_eq!(parse_delay("1e9"), None);
        assert_eq!(parse_delay("s"), None);
    }

    #[tokio::test]
    async fn delay_returns_response() {
        let resp = test_app()
            .oneshot(get_request("/delay/10"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["delay_ms"], 10);
        assert_eq!(json["method"], "GET");
        assert_eq!(json["max_delay_ms"], MAX_DELAY_MS);
    }

    #[tokio::test]
    async fn delay_seconds_form_and_limits() {
        let resp = test_app()
            .oneshot(get_request("/delay/0.01s"))
            .await
            .expect("response");
        assert_eq!(body_json(resp).await["delay_ms"], 10);
        let resp = test_app()
            .oneshot(get_request("/delay/99999"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let mut config = crate::test_support::test_config();
        config.public_mode = true;
        let app = crate::test_support::module_app_with_config(config, router);
        let resp = app
            .oneshot(get_request("/delay/11s"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn jitter_never_exceeds_the_cap() {
        assert_eq!(
            apply_jitter(10_000, MAX_DELAY_MS_PUBLIC, 1.2),
            MAX_DELAY_MS_PUBLIC
        );
        assert_eq!(apply_jitter(60_000, MAX_DELAY_MS, 1.2), MAX_DELAY_MS);
        assert_eq!(apply_jitter(1_000, MAX_DELAY_MS, 0.8), 800);
    }

    #[tokio::test]
    async fn jitter_query_is_applied_within_bounds() {
        let resp = test_app()
            .oneshot(get_request("/delay/20?jitter=true"))
            .await
            .expect("response");
        let ms = body_json(resp).await["delay_ms"].as_u64().unwrap_or(0);
        assert!((16..=24).contains(&ms), "{ms}");
    }

    #[tokio::test]
    async fn response_headers_sets_custom_headers() {
        let resp = test_app()
            .oneshot(get_request(
                "/response-headers?x-custom=hello&x-another=world&x-custom=again",
            ))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let values: Vec<_> = resp.headers().get_all("x-custom").iter().collect();
        assert_eq!(values, vec!["hello", "again"]);
        assert_eq!(resp.headers()["x-another"], "world");
        let json = body_json(resp).await;
        assert_eq!(json["x-custom"], serde_json::json!(["hello", "again"]));
    }

    #[tokio::test]
    async fn response_headers_refuses_framing_headers() {
        for q in [
            "Content-Length=5",
            "transfer-encoding=chunked",
            "Connection=close",
            "upgrade=h2c",
        ] {
            let resp = test_app()
                .oneshot(get_request(&format!("/response-headers?{q}")))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{q}");
            assert_eq!(body_json(resp).await["error"], "forbidden_header");
        }
        let resp = test_app()
            .oneshot(get_request("/response-headers?bad%20name=x"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        // Content-Type may be overridden.
        let resp = test_app()
            .oneshot(get_request("/response-headers?Content-Type=text/plain"))
            .await
            .expect("response");
        assert_eq!(resp.headers()["content-type"], "text/plain");
    }

    async fn first_etag(app: &Router, accept: &str) -> (String, String) {
        let resp = app
            .clone()
            .oneshot(req("/cache/300", &[("accept", accept)]))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        (
            resp.headers()["etag"].to_str().expect("etag").to_string(),
            resp.headers()["last-modified"]
                .to_str()
                .expect("lm")
                .to_string(),
        )
    }

    #[tokio::test]
    async fn cache_headers_and_representation_etags() {
        let app = test_app();
        let resp = app
            .clone()
            .oneshot(get_request("/cache/300"))
            .await
            .expect("response");
        assert!(resp.headers()["cache-control"]
            .to_str()
            .expect("cc")
            .contains("max-age=300"));
        assert_eq!(resp.headers()["vary"], "Accept");
        let json = body_json(resp).await;
        assert_eq!(json["cached"], true);
        let (json_tag, _) = first_etag(&app, "application/json").await;
        let (xml_tag, _) = first_etag(&app, "application/xml").await;
        assert_ne!(json_tag, xml_tag);
    }

    #[tokio::test]
    async fn conditional_requests() {
        let app = test_app();
        let (etag, last_modified) = first_etag(&app, "application/json").await;
        let opaque = etag.trim_matches('"');
        let check = |headers: Vec<(&'static str, String)>| {
            let app = app.clone();
            async move {
                let mut b = Request::builder().uri("/cache/300");
                for (k, v) in headers {
                    b = b.header(k, v);
                }
                app.oneshot(b.body(Body::empty()).expect("request"))
                    .await
                    .expect("response")
            }
        };
        // Exact, weak, list, star.
        for inm in [
            etag.clone(),
            format!("W/{etag}"),
            format!("\"other\", {etag}"),
            "*".to_string(),
        ] {
            let resp = check(vec![("if-none-match", inm.clone())]).await;
            assert_eq!(resp.status(), StatusCode::NOT_MODIFIED, "{inm}");
            assert_eq!(resp.headers()["etag"], etag.as_str());
            assert!(resp.headers().contains_key("cache-control"));
            assert_eq!(resp.headers()["vary"], "Accept");
        }
        let resp = check(vec![("if-none-match", format!("\"{opaque}x\""))]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        // If-Modified-Since at second precision.
        let resp = check(vec![("if-modified-since", last_modified.clone())]).await;
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        let resp = check(vec![(
            "if-modified-since",
            "Mon, 01 Jan 2001 00:00:00 GMT".to_string(),
        )])
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        // A non-matching If-None-Match wins over a matching If-Modified-Since.
        let resp = check(vec![
            ("if-none-match", "\"nope\"".to_string()),
            ("if-modified-since", last_modified),
        ])
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn cache_invalid_ttl_returns_400() {
        let resp = test_app()
            .oneshot(get_request("/cache/abc"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
