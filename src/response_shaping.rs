use axum::{
    body::{Body, Bytes},
    extract::{Extension, Path, Query},
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{any, get},
    Router,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use rand::distributions::Alphanumeric;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_stream::wrappers::ReceiverStream;

use crate::config::Config;
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::types::ErrorResponse;

#[derive(Clone)]
struct ServerStartup(DateTime<Utc>);

// ── Delay ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct DelayQuery {
    #[serde(default)]
    jitter: Option<bool>,
}

#[derive(Serialize)]
struct DelayResponse {
    delay_ms: u64,
    method: String,
    path: String,
    query_string: String,
    headers: HashMap<String, Vec<String>>,
    timestamp_unix_ms: u64,
}

async fn delay_handler(
    Path(ms): Path<String>,
    Query(query): Query<DelayQuery>,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let ms_val: u64 = match ms.parse() {
        Ok(v) if v <= 60000 => v,
        _ => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "invalid_delay".to_string(),
                    details: Some("Delay must be between 0 and 60000 ms".to_string()),
                },
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let actual_delay = if query.jitter == Some(true) {
        let mut rng = rand::thread_rng();
        let factor: f64 = rng.gen_range(0.8..=1.2);
        (ms_val as f64 * factor) as u64
    } else {
        ms_val
    };

    tokio::time::sleep(Duration::from_millis(actual_delay)).await;

    let header_map = collect_headers(&headers);
    let query_string = uri.query().unwrap_or("").to_string();

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    negotiate(
        &headers,
        &DelayResponse {
            delay_ms: actual_delay,
            method: method.to_string(),
            path: uri.path().to_string(),
            query_string,
            headers: header_map,
            timestamp_unix_ms: timestamp,
        },
    )
}

// ── Bytes ────────────────────────────────────────────────────────────

async fn bytes_handler(Path(n): Path<String>, headers: HeaderMap) -> Response {
    let n_val: usize = match n.parse() {
        Ok(v) if (1..=10_485_760).contains(&v) => v,
        _ => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "invalid_byte_count".to_string(),
                    details: Some("Byte count must be between 1 and 10485760".to_string()),
                },
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let mut rng = rand::thread_rng();
    let data: Vec<u8> = (0..n_val).map(|_| rng.gen()).collect();

    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        Bytes::from(data),
    )
        .into_response()
}

// ── Stream ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct StreamQuery {
    delay: Option<u64>,
}

#[derive(Serialize)]
struct StreamChunk {
    id: usize,
    timestamp_unix_ms: u64,
    data: String,
}

async fn stream_handler(
    Path(n): Path<String>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Response {
    let n_val: usize = match n.parse() {
        Ok(v) if (1..=1000).contains(&v) => v,
        _ => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "invalid_chunk_count".to_string(),
                    details: Some("Chunk count must be between 1 and 1000".to_string()),
                },
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let delay_ms = query.delay;

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(16);

    tokio::spawn(async move {
        let mut rng = StdRng::from_entropy();
        for i in 0..n_val {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);

            let data: String = (&mut rng)
                .sample_iter(&Alphanumeric)
                .take(32)
                .map(char::from)
                .collect();

            let chunk = StreamChunk {
                id: i,
                timestamp_unix_ms: timestamp,
                data,
            };

            let line = match serde_json::to_string(&chunk) {
                Ok(s) => format!("{s}\n"),
                Err(_) => break,
            };

            if tx.send(Ok(Bytes::from(line))).await.is_err() {
                break;
            }

            if let Some(d) = delay_ms {
                tokio::time::sleep(Duration::from_millis(d)).await;
            }
        }
    });

    let body = Body::from_stream(ReceiverStream::new(rx));

    ([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
}

// ── Drip ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(default)]
struct DripQuery {
    bytes: usize,
    delay: u64,
    chunk_size: usize,
}

impl Default for DripQuery {
    fn default() -> Self {
        Self {
            bytes: 1024,
            delay: 100,
            chunk_size: 10,
        }
    }
}

async fn drip_handler(Query(query): Query<DripQuery>, headers: HeaderMap) -> Response {
    let total_bytes = query.bytes.min(10_485_760);
    let delay_ms = query.delay.min(10_000);
    let chunk_size = query.chunk_size.max(1);

    if total_bytes == 0 {
        return negotiate_with_status(
            &headers,
            &ErrorResponse {
                error: "invalid_drip_params".to_string(),
                details: Some("Total bytes must be at least 1".to_string()),
            },
            StatusCode::BAD_REQUEST,
        );
    }

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(16);

    tokio::spawn(async move {
        let mut rng = StdRng::from_entropy();
        let mut remaining = total_bytes;

        while remaining > 0 {
            let this_chunk = remaining.min(chunk_size);
            let data: Vec<u8> = (0..this_chunk).map(|_| rng.gen()).collect();
            remaining -= this_chunk;

            if tx.send(Ok(Bytes::from(data))).await.is_err() {
                break;
            }

            if remaining > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        }
    });

    let body = Body::from_stream(ReceiverStream::new(rx));

    ([(header::CONTENT_TYPE, "application/octet-stream")], body).into_response()
}

// ── Response Headers ─────────────────────────────────────────────────

async fn response_headers_handler(uri: Uri) -> Response {
    let query = uri.query().unwrap_or("");
    let params: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();

    let mut headers_set = serde_json::Map::new();
    let mut custom_headers = HeaderMap::new();

    for (key, value) in &params {
        if let Ok(name) = HeaderName::from_bytes(key.as_bytes()) {
            if let Ok(val) = HeaderValue::from_str(value) {
                custom_headers.append(name, val);
                headers_set.insert(key.clone(), serde_json::Value::String(value.clone()));
            }
        }
    }

    let body_json = serde_json::to_string(&serde_json::Value::Object(headers_set))
        .unwrap_or_else(|_| "{}".to_string());

    let mut resp = (
        [(header::CONTENT_TYPE, "application/json")],
        body_json,
    )
        .into_response();

    resp.headers_mut().extend(custom_headers);
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

async fn cache_handler(
    Extension(ServerStartup(startup)): Extension<ServerStartup>,
    Path(ttl): Path<String>,
    headers: HeaderMap,
) -> Response {
    let ttl_val: u32 = match ttl.parse() {
        Ok(v) => v,
        Err(_) => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "invalid_ttl".to_string(),
                    details: Some("TTL must be a non-negative integer".to_string()),
                },
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let generated_at = startup.to_rfc3339();
    let etag_value = compute_etag(ttl_val, &startup);
    let etag_header = format!("\"{etag_value}\"");
    let last_modified = startup.format("%a, %d %b %Y %H:%M:%S GMT").to_string();

    // Conditional: If-None-Match
    if let Some(inm) = headers.get("if-none-match").and_then(|v| v.to_str().ok()) {
        if inm == etag_header || inm == "*" {
            return StatusCode::NOT_MODIFIED.into_response();
        }
    }

    // Conditional: If-Modified-Since
    if let Some(ims) = headers
        .get("if-modified-since")
        .and_then(|v| v.to_str().ok())
    {
        if let Some(ims_dt) = parse_http_date(ims) {
            if ims_dt >= startup {
                return StatusCode::NOT_MODIFIED.into_response();
            }
        }
    }

    let data = CacheResponse {
        cached: true,
        ttl: ttl_val,
        generated_at,
        etag: etag_value,
    };

    let mut resp = negotiate(&headers, &data);

    if let Ok(cc) = HeaderValue::from_str(&format!("public, max-age={ttl_val}")) {
        resp.headers_mut().insert(header::CACHE_CONTROL, cc);
    }
    if let Ok(et) = HeaderValue::from_str(&etag_header) {
        resp.headers_mut().insert(header::ETAG, et);
    }
    if let Ok(lm) = HeaderValue::from_str(&last_modified) {
        resp.headers_mut().insert(header::LAST_MODIFIED, lm);
    }
    resp.headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Accept"));

    resp
}

// ── Helpers ──────────────────────────────────────────────────────────

fn collect_headers(headers: &HeaderMap) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (name, value) in headers.iter() {
        let key = name.to_string();
        let val = value.to_str().unwrap_or("<binary>").to_string();
        map.entry(key).or_default().push(val);
    }
    map
}

fn compute_etag(ttl: u32, startup: &DateTime<Utc>) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ttl.hash(&mut hasher);
    startup.to_rfc3339().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn parse_http_date(s: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(s, "%a, %d %b %Y %H:%M:%S GMT")
        .ok()
        .map(|dt| dt.and_utc())
}

// ── Router ───────────────────────────────────────────────────────────

pub fn router() -> Router<Arc<Config>> {
    let startup = ServerStartup(Utc::now());
    Router::new()
        .route("/delay/:ms", any(delay_handler))
        .route("/bytes/:n", get(bytes_handler))
        .route("/stream/:n", get(stream_handler))
        .route("/drip", get(drip_handler))
        .route("/response-headers", get(response_headers_handler))
        .route("/cache/:ttl", get(cache_handler))
        .layer(axum::Extension(startup))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_config() -> Arc<Config> {
        Arc::new(Config {
            http_port: 80,
            https_port: 443,
            host: "0.0.0.0".to_string(),
            log_level: "info".to_string(),
            trust_forward: false,
            body_limit: 1_048_576,
            instance_id: "test-instance".to_string(),
            tls_cert: "certs/server.crt".to_string(),
            tls_key: "certs/server.key".to_string(),
            mtls_in_header: None,
        })
    }

    fn test_app() -> Router {
        router().with_state(test_config())
    }

    #[tokio::test]
    async fn delay_returns_response() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/delay/10")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert!(json["delay_ms"].is_number());
        assert_eq!(json["method"], "GET");
    }

    #[tokio::test]
    async fn delay_invalid_returns_400() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/delay/99999")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn bytes_returns_correct_count() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/bytes/256")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .expect("ct")
            .to_str()
            .expect("str");
        assert_eq!(ct, "application/octet-stream");

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(body.len(), 256);
    }

    #[tokio::test]
    async fn bytes_invalid_returns_400() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/bytes/0")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn stream_returns_ndjson() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/stream/3")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .expect("ct")
            .to_str()
            .expect("str");
        assert_eq!(ct, "application/x-ndjson");

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let text = String::from_utf8(body.to_vec()).expect("utf8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);

        for line in &lines {
            let chunk: serde_json::Value = serde_json::from_str(line).expect("json");
            assert!(chunk["id"].is_number());
            assert!(chunk["data"].is_string());
        }
    }

    #[tokio::test]
    async fn stream_invalid_returns_400() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/stream/0")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn drip_returns_bytes() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/drip?bytes=50&delay=1&chunk_size=10")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .expect("ct")
            .to_str()
            .expect("str");
        assert_eq!(ct, "application/octet-stream");

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(body.len(), 50);
    }

    #[tokio::test]
    async fn response_headers_sets_custom_headers() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/response-headers?x-custom=hello&x-another=world")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("x-custom")
                .expect("x-custom")
                .to_str()
                .expect("str"),
            "hello"
        );
        assert_eq!(
            resp.headers()
                .get("x-another")
                .expect("x-another")
                .to_str()
                .expect("str"),
            "world"
        );

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["x-custom"], "hello");
    }

    #[tokio::test]
    async fn cache_returns_cache_headers() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/cache/300")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get("cache-control").is_some());
        assert!(resp.headers().get("etag").is_some());
        assert!(resp.headers().get("last-modified").is_some());
        assert_eq!(
            resp.headers()
                .get("vary")
                .expect("vary")
                .to_str()
                .expect("str"),
            "Accept"
        );

        let cc = resp
            .headers()
            .get("cache-control")
            .expect("cc")
            .to_str()
            .expect("str");
        assert!(cc.contains("max-age=300"));

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["cached"], true);
        assert_eq!(json["ttl"], 300);
    }

    #[tokio::test]
    async fn cache_if_none_match_returns_304() {
        let app = test_app();

        // First request to get the ETag
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/cache/300")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        let etag = resp
            .headers()
            .get("etag")
            .expect("etag")
            .to_str()
            .expect("str")
            .to_string();

        // Second request with If-None-Match
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/cache/300")
                    .header("if-none-match", &etag)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn cache_invalid_ttl_returns_400() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/cache/abc")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
