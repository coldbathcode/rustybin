//! Compression and transfer helpers (httpbin compatible shapes).
//!
//! - `/gzip`, `/deflate`, `/brotli`, `/zstd`: a JSON echo (`{"gzipped": true,
//!   "headers", "method", "origin"}`) compressed with that encoding and
//!   labelled with `Content-Encoding`, whatever `Accept-Encoding` says (so a
//!   gateway's decompression or pass-through can be tested). `deflate` is the
//!   zlib wrapped format, as HTTP specifies.
//! - `/encoding/utf8`: a UTF-8 sample page.
//! - `/range/{n}`: `n` bytes (`abc...z` repeated) honouring single byte
//!   ranges: 206 with `Content-Range`, 416 for unsatisfiable or malformed
//!   ranges, `Accept-Ranges`, a strong `ETag` and `If-Range`. Multiple ranges
//!   are not supported (the full body is returned with 200, as RFC 9110
//!   allows).

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::session::ClientIp;
use crate::state::AppState;

/// Largest `/range/{n}` (normal mode).
pub const MAX_RANGE_BYTES: usize = 100 * 1024;
/// Largest `/range/{n}` in public mode.
pub const MAX_RANGE_BYTES_PUBLIC: usize = 10 * 1024;

/// Content encodings served by this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Gzip,
    Deflate,
    Brotli,
    Zstd,
}

impl Encoding {
    pub fn token(self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Deflate => "deflate",
            Self::Brotli => "br",
            Self::Zstd => "zstd",
        }
    }

    /// httpbin's flag name (`gzipped`, `deflated`, `brotli`, `zstd`).
    fn flag(self) -> &'static str {
        match self {
            Self::Gzip => "gzipped",
            Self::Deflate => "deflated",
            Self::Brotli => "brotli",
            Self::Zstd => "zstd",
        }
    }
}

/// Compress `data` with `encoding`.
pub fn compress(encoding: Encoding, data: &[u8]) -> std::io::Result<Vec<u8>> {
    match encoding {
        Encoding::Gzip => {
            let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(data)?;
            enc.finish()
        }
        Encoding::Deflate => {
            let mut enc =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(data)?;
            enc.finish()
        }
        Encoding::Brotli => {
            let mut out = Vec::new();
            {
                let mut w = brotli::CompressorWriter::new(&mut out, 4096, 5, 22);
                w.write_all(data)?;
                w.flush()?;
            }
            Ok(out)
        }
        Encoding::Zstd => Ok(ruzstd::encoding::compress_to_vec(
            data,
            ruzstd::encoding::CompressionLevel::Fastest,
        )),
    }
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

fn compressed_echo(
    encoding: Encoding,
    method: &Method,
    headers: &HeaderMap,
    ip: Option<std::net::IpAddr>,
) -> Response {
    let body = json!({
        encoding.flag(): true,
        "method": method.as_str(),
        "headers": header_map(headers),
        "origin": ip.map(|i| i.to_string()),
    });
    let raw = serde_json::to_vec_pretty(&body).unwrap_or_default();
    match compress(encoding, &raw) {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CONTENT_ENCODING, encoding.token()),
                (header::VARY, "Accept-Encoding"),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("compression failed: {e}") })),
        )
            .into_response(),
    }
}

async fn gzip_handler(method: Method, headers: HeaderMap, ClientIp(ip): ClientIp) -> Response {
    compressed_echo(Encoding::Gzip, &method, &headers, ip)
}

async fn deflate_handler(method: Method, headers: HeaderMap, ClientIp(ip): ClientIp) -> Response {
    compressed_echo(Encoding::Deflate, &method, &headers, ip)
}

async fn brotli_handler(method: Method, headers: HeaderMap, ClientIp(ip): ClientIp) -> Response {
    compressed_echo(Encoding::Brotli, &method, &headers, ip)
}

async fn zstd_handler(method: Method, headers: HeaderMap, ClientIp(ip): ClientIp) -> Response {
    compressed_echo(Encoding::Zstd, &method, &headers, ip)
}

const UTF8_SAMPLE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>UTF-8 sample</title></head>
<body>
<h1>UTF-8 encoded sample text</h1>
<ul>
<li>Latin: Ça va? Grüße aus Köln, ¿qué tal? Smørrebrød, Łódź, İstanbul</li>
<li>Greek: Η γρήγορη καφέ αλεπού πηδά πάνω από τον τεμπέλη σκύλο</li>
<li>Cyrillic: Съешь же ещё этих мягких французских булок</li>
<li>Hebrew: דג סקרן שט בים מאוכזב</li>
<li>Arabic: نص حكيم له سر قاطع وذو شأن عظيم</li>
<li>Devanagari: ऋषियों को सताने वाले दुष्ट राक्षसों</li>
<li>Chinese: 我能吞下玻璃而不伤身体</li>
<li>Japanese: いろはにほへと ちりぬるを</li>
<li>Korean: 다람쥐 헌 쳇바퀴에 타고파</li>
<li>Thai: เป็นมนุษย์สุดประเสริฐเลิศคุณค่า</li>
<li>Math: ∀x ∈ ℝ: ⌈x⌉ = −⌊−x⌋, ∑ ∫ √ ∞ ≠ ≤ ≥</li>
<li>Symbols: € £ ¥ ₹ © ® ™ ✓ ✗ ★ ♫ ☂</li>
<li>Emoji (4 byte): 😀 🚀 🦀 👍🏽 🇪🇺</li>
</ul>
</body>
</html>
"#;

async fn utf8_handler() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        UTF8_SAMPLE,
    )
        .into_response()
}

// ── /range/{n} ──────────────────────────────────────────────────────

/// Outcome of evaluating a `Range` header against a representation length.
#[derive(Debug, PartialEq, Eq)]
pub enum RangeOutcome {
    /// Serve the whole body (no, ignored or multi range).
    Full,
    /// Serve `start..=end`.
    Partial(usize, usize),
    /// 416.
    Unsatisfiable,
}

/// Evaluate a single `Range` header value for a body of `len` bytes.
pub fn evaluate_range(value: &str, len: usize) -> RangeOutcome {
    let value = value.trim();
    let Some(spec) = value
        .get(..6)
        .filter(|p| p.eq_ignore_ascii_case("bytes="))
        .and_then(|_| value.get(6..))
    else {
        // Unknown range unit: ignore the header.
        return RangeOutcome::Full;
    };
    if spec.contains(',') {
        return RangeOutcome::Full;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return RangeOutcome::Unsatisfiable;
    };
    let (first, last) = (first.trim(), last.trim());
    let parse = |s: &str| -> Option<usize> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            None
        } else {
            // Huge numbers saturate (they are still valid syntax).
            Some(s.parse::<usize>().unwrap_or(usize::MAX))
        }
    };
    if len == 0 {
        return RangeOutcome::Unsatisfiable;
    }
    match (first.is_empty(), parse(first), parse(last)) {
        // bytes=-N: the last N bytes.
        (true, _, Some(suffix)) => {
            if suffix == 0 {
                RangeOutcome::Unsatisfiable
            } else {
                RangeOutcome::Partial(len.saturating_sub(suffix), len - 1)
            }
        }
        // bytes=N-
        (false, Some(start), None) if last.is_empty() => {
            if start >= len {
                RangeOutcome::Unsatisfiable
            } else {
                RangeOutcome::Partial(start, len - 1)
            }
        }
        // bytes=N-M
        (false, Some(start), Some(end)) => {
            if start > end || start >= len {
                RangeOutcome::Unsatisfiable
            } else {
                RangeOutcome::Partial(start, end.min(len - 1))
            }
        }
        _ => RangeOutcome::Unsatisfiable,
    }
}

fn range_body(n: usize) -> Vec<u8> {
    (0..n).map(|i| b'a' + (i % 26) as u8).collect()
}

async fn range_handler(
    State(config): State<Arc<Config>>,
    Path(n): Path<String>,
    headers: HeaderMap,
) -> Response {
    let max = if config.public_mode {
        MAX_RANGE_BYTES_PUBLIC
    } else {
        MAX_RANGE_BYTES
    };
    let n = match n.parse::<usize>() {
        Ok(n) if n >= 1 && n <= max => n,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("number of bytes must be between 1 and {max}") })),
            )
                .into_response()
        }
    };
    let etag = format!("\"range{n}\"");
    let body = range_body(n);
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    // If-Range with a different validator: ignore Range and send everything.
    let if_range_ok = headers
        .get(header::IF_RANGE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| v.trim() == etag);
    let outcome = match range {
        Some(r) if if_range_ok => evaluate_range(r, n),
        _ => RangeOutcome::Full,
    };
    let mut resp = match outcome {
        RangeOutcome::Full => (StatusCode::OK, body).into_response(),
        RangeOutcome::Partial(start, end) => {
            let mut r = (
                StatusCode::PARTIAL_CONTENT,
                body.get(start..=end).unwrap_or_default().to_vec(),
            )
                .into_response();
            if let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{end}/{n}")) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            r
        }
        RangeOutcome::Unsatisfiable => {
            let mut r = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{n}")) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            r
        }
    };
    let h = resp.headers_mut();
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    resp
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/gzip", get(gzip_handler))
        .route("/deflate", get(deflate_handler))
        .route("/brotli", get(brotli_handler))
        .route("/zstd", get(zstd_handler))
        .route("/encoding/utf8", get(utf8_handler))
        .route("/range/{n}", get(range_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    let enc = |path: &'static str, summary: &'static str, name: &'static str| {
        Endpoint::new(path, &["GET"], category::SHAPING, summary).example(Example::get(name, path))
    };
    vec![
        enc("/gzip", "gzip-compressed JSON echo (gzipped: true)", "gzip response"),
        enc(
            "/deflate",
            "deflate (zlib) compressed JSON echo (deflated: true)",
            "deflate response",
        ),
        enc("/brotli", "Brotli-compressed JSON echo (brotli: true)", "Brotli response"),
        enc("/zstd", "Zstandard-compressed JSON echo (zstd: true)", "zstd response"),
        Endpoint::new(
            "/encoding/utf8",
            &["GET"],
            category::SHAPING,
            "UTF-8 sample page (many scripts, symbols, emoji)",
        )
        .example(Example::get("UTF-8 sample", "/encoding/utf8")),
        Endpoint::new(
            "/range/{n}",
            &["GET"],
            category::SHAPING,
            "n bytes supporting Range requests (206, 416, ETag, If-Range)",
        )
        .description(
            "Body is abc...z repeated, n up to 102400 (public 10240). Single ranges only \
             (bytes=a-b, bytes=a-, bytes=-n): 206 with Content-Range; unsatisfiable or malformed \
             ranges are 416 with Content-Range: bytes */n; multiple ranges and unknown units get the \
             full 200 body.",
        )
        .example(Example::get("Full body", "/range/1024"))
        .example(
            Example::get("First 100 bytes", "/range/1024")
                .header("Range", "bytes=0-99")
                .expect_status(206),
        )
        .example(
            Example::get("Unsatisfiable range", "/range/1024")
                .header("Range", "bytes=5000-")
                .expect_status(416),
        ),
    ]
}

pub fn openapi_paths() -> Value {
    let tags = json!(["Response Shaping"]);
    let compressed = |flag: &str, enc: &str, op: &str| {
        json!({
            "get": {
                "tags": tags,
                "summary": format!("JSON echo compressed with Content-Encoding: {enc}"),
                "operationId": op,
                "responses": { "200": {
                    "description": format!("Compressed JSON with {flag}: true"),
                    "headers": { "Content-Encoding": { "schema": { "type": "string", "enum": [enc] } } },
                    "content": { "application/json": {} }
                } }
            }
        })
    };
    json!({
        "/gzip": compressed("gzipped", "gzip", "gzip"),
        "/deflate": compressed("deflated", "deflate", "deflate"),
        "/brotli": compressed("brotli", "br", "brotli"),
        "/zstd": compressed("zstd", "zstd", "zstd"),
        "/encoding/utf8": {
            "get": {
                "tags": tags, "summary": "UTF-8 sample page", "operationId": "encodingUtf8",
                "responses": { "200": { "description": "HTML", "content": { "text/html": {} } } }
            }
        },
        "/range/{n}": {
            "get": {
                "tags": tags, "summary": "n bytes with Range support", "operationId": "range",
                "parameters": [
                    { "name": "n", "in": "path", "required": true, "schema": { "type": "integer", "minimum": 1, "maximum": MAX_RANGE_BYTES } },
                    { "name": "Range", "in": "header", "required": false, "schema": { "type": "string", "example": "bytes=0-99" } },
                    { "name": "If-Range", "in": "header", "required": false, "schema": { "type": "string" } }
                ],
                "responses": {
                    "200": { "description": "Full body", "content": { "application/octet-stream": {} } },
                    "206": { "description": "Partial content (Content-Range)" },
                    "400": { "description": "n out of range" },
                    "416": { "description": "Range not satisfiable (Content-Range: bytes */n)" }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_bytes, body_string, get_request, module_app};
    use axum::body::Body;
    use axum::http::Request;
    use std::io::Read;
    use tower::ServiceExt;

    fn decode(encoding: Encoding, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        match encoding {
            Encoding::Gzip => {
                flate2::read::GzDecoder::new(data)
                    .read_to_end(&mut out)
                    .expect("gunzip");
            }
            Encoding::Deflate => {
                flate2::read::ZlibDecoder::new(data)
                    .read_to_end(&mut out)
                    .expect("inflate");
            }
            Encoding::Brotli => {
                brotli::Decompressor::new(data, 4096)
                    .read_to_end(&mut out)
                    .expect("brotli");
            }
            Encoding::Zstd => {
                ruzstd::decoding::StreamingDecoder::new(data)
                    .expect("zstd frame")
                    .read_to_end(&mut out)
                    .expect("zstd");
            }
        }
        out
    }

    #[tokio::test]
    async fn compressed_echoes_decode() {
        let app = module_app(router);
        for (path, enc) in [
            ("/gzip", Encoding::Gzip),
            ("/deflate", Encoding::Deflate),
            ("/brotli", Encoding::Brotli),
            ("/zstd", Encoding::Zstd),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("x-demo", "1")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(resp.headers()["content-encoding"], enc.token());
            let raw = body_bytes(resp).await;
            let json: Value = serde_json::from_slice(&decode(enc, &raw)).expect("json");
            assert_eq!(json[enc.flag()], true, "{path}");
            assert_eq!(json["headers"]["x-demo"], "1");
            assert_eq!(json["method"], "GET");
        }
    }

    #[test]
    fn range_evaluation() {
        use RangeOutcome::*;
        assert_eq!(evaluate_range("bytes=0-9", 100), Partial(0, 9));
        assert_eq!(evaluate_range("bytes=90-", 100), Partial(90, 99));
        assert_eq!(evaluate_range("bytes=-10", 100), Partial(90, 99));
        assert_eq!(evaluate_range("bytes=-1000", 100), Partial(0, 99));
        assert_eq!(evaluate_range("bytes=50-5000", 100), Partial(50, 99));
        assert_eq!(evaluate_range("BYTES=99-99", 100), Partial(99, 99));
        assert_eq!(evaluate_range("bytes=100-", 100), Unsatisfiable);
        assert_eq!(evaluate_range("bytes=10-5", 100), Unsatisfiable);
        assert_eq!(evaluate_range("bytes=-0", 100), Unsatisfiable);
        assert_eq!(evaluate_range("bytes=abc", 100), Unsatisfiable);
        assert_eq!(evaluate_range("bytes=1-x", 100), Unsatisfiable);
        assert_eq!(evaluate_range("bytes=-", 100), Unsatisfiable);
        assert_eq!(evaluate_range("bytes=0-1,5-6", 100), Full);
        assert_eq!(evaluate_range("items=0-1", 100), Full);
        assert_eq!(
            evaluate_range("bytes=99999999999999999999999-", 100),
            Unsatisfiable
        );
    }

    async fn range(app: &Router, uri: &str, h: &[(&str, &str)]) -> Response {
        let mut b = Request::builder().uri(uri);
        for (k, v) in h {
            b = b.header(*k, *v);
        }
        app.clone()
            .oneshot(b.body(Body::empty()).expect("request"))
            .await
            .expect("response")
    }

    #[tokio::test]
    async fn range_responses() {
        let app = module_app(router);
        let resp = range(&app, "/range/30", &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["accept-ranges"], "bytes");
        assert_eq!(resp.headers()["etag"], "\"range30\"");
        assert_eq!(body_string(resp).await, "abcdefghijklmnopqrstuvwxyzabcd");

        let resp = range(&app, "/range/30", &[("range", "bytes=24-27")]).await;
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(resp.headers()["content-range"], "bytes 24-27/30");
        assert_eq!(resp.headers()["content-length"], "4");
        assert_eq!(body_string(resp).await, "yzab");

        let resp = range(&app, "/range/30", &[("range", "bytes=30-")]).await;
        assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(resp.headers()["content-range"], "bytes */30");

        // If-Range mismatch: full body.
        let resp = range(
            &app,
            "/range/30",
            &[("range", "bytes=0-1"), ("if-range", "\"other\"")],
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = range(
            &app,
            "/range/30",
            &[("range", "bytes=0-1"), ("if-range", "\"range30\"")],
        )
        .await;
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);

        for uri in ["/range/0", "/range/102401", "/range/x"] {
            let resp = app
                .clone()
                .oneshot(get_request(uri))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
        }
        let mut config = Config::for_tests();
        config.public_mode = true;
        let app = crate::test_support::module_app_with_config(config, router);
        let resp = app
            .oneshot(get_request("/range/10241"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn utf8_sample() {
        let app = module_app(router);
        let resp = app
            .oneshot(get_request("/encoding/utf8"))
            .await
            .expect("response");
        assert_eq!(resp.headers()["content-type"], "text/html; charset=utf-8");
        assert!(body_string(resp).await.contains("🦀"));
    }
}
