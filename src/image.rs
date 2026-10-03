//! Small, valid images for proxy / transformation / caching tests.
//!
//! - PNG: a 16x16 RGB gradient generated at first use (zlib with stored
//!   deflate blocks, real CRC-32 and Adler-32), so it is valid by
//!   construction and the tests decode it fully.
//! - JPEG (8x8 baseline), GIF (8x8 GIF89a), WebP (8x8 lossless): embedded,
//!   verified with an independent decoder when generated, and structurally
//!   validated by the tests.
//! - SVG: a tiny static document.
//! - `/image` picks a format from `Accept` (httpbin style, 406 if none fits).

use axum::{
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::sync::OnceLock;

use crate::catalog::{category, Endpoint, Example};
use crate::content_negotiation::{choose, negotiate_with_status};
use crate::state::AppState;
use crate::types::ErrorResponse;

/// PNG dimensions.
pub const PNG_SIZE: u32 = 16;

/// 8x8 baseline JPEG, solid orange (4:2:0, no metadata).
const JPEG_8X8: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06, 0x05, 0x08,
    0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D, 0x0C, 0x0B, 0x0B, 0x0C, 0x19, 0x12,
    0x13, 0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D, 0x1A, 0x1C, 0x1C, 0x20, 0x24, 0x2E, 0x27, 0x20,
    0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28, 0x37, 0x29, 0x2C, 0x30, 0x31, 0x34, 0x34, 0x34, 0x1F, 0x27,
    0x39, 0x3D, 0x38, 0x32, 0x3C, 0x2E, 0x33, 0x34, 0x32, 0xFF, 0xDB, 0x00, 0x43, 0x01, 0x09, 0x09,
    0x09, 0x0C, 0x0B, 0x0C, 0x18, 0x0D, 0x0D, 0x18, 0x32, 0x21, 0x1C, 0x21, 0x32, 0x32, 0x32, 0x32,
    0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32,
    0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32,
    0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0xFF, 0xC0,
    0x00, 0x11, 0x08, 0x00, 0x08, 0x00, 0x08, 0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11,
    0x01, 0xFF, 0xC4, 0x00, 0x15, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xFF, 0xC4, 0x00, 0x14, 0x10, 0x01, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xC4,
    0x00, 0x15, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x06, 0x07, 0xFF, 0xC4, 0x00, 0x14, 0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xDA, 0x00, 0x0C, 0x03,
    0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3F, 0x00, 0x90, 0x00, 0xEA, 0x2B, 0xFF, 0xD9,
];

/// 8x8 GIF89a, solid blue (2-colour global table, LZW image data).
const GIF_8X8: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x08, 0x00, 0x08, 0x00, 0xF0, 0x00, 0x00, 0x19, 0x71, 0xC2,
    0x00, 0x00, 0x00, 0x21, 0xF9, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x2C, 0x00, 0x00, 0x00, 0x00,
    0x08, 0x00, 0x08, 0x00, 0x00, 0x02, 0x07, 0x84, 0x8F, 0xA9, 0xCB, 0xED, 0x5D, 0x00, 0x00, 0x3B,
];

/// 8x8 lossless WebP (VP8L), solid green.
const WEBP_8X8: &[u8] = &[
    0x52, 0x49, 0x46, 0x46, 0x1E, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38, 0x4C,
    0x11, 0x00, 0x00, 0x00, 0x2F, 0x07, 0xC0, 0x01, 0x00, 0x07, 0x50, 0xCF, 0xBE, 0x94, 0xA8, 0xFF,
    0x81, 0x88, 0xE8, 0x7F, 0x00, 0x00,
];

const SVG: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">
  <title>rustybin</title>
  <rect width="64" height="64" rx="12" fill="#b7410e"/>
  <circle cx="32" cy="32" r="16" fill="none" stroke="#ffffff" stroke-width="6"/>
</svg>
"##;

// ── PNG generation ──────────────────────────────────────────────────

fn crc32_table() -> &'static [u32; 256] {
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (n, slot) in table.iter_mut().enumerate() {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        table
    })
}

/// CRC-32 (ISO 3309 / ITU-T V.42), as used by PNG.
pub fn crc32(data: &[u8]) -> u32 {
    let table = crc32_table();
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = table[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

/// Adler-32, as used by zlib.
pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// zlib stream with stored (uncompressed) deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let chunks: Vec<&[u8]> = if data.is_empty() {
        vec![&[]]
    } else {
        data.chunks(65_535).collect()
    };
    let last = chunks.len() - 1;
    for (i, chunk) in chunks.iter().enumerate() {
        out.push(u8::from(i == last));
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc_input);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// A `size` x `size` 8-bit RGB gradient PNG.
pub fn generate_png(size: u32) -> Vec<u8> {
    let mut raw = Vec::with_capacity((size * (1 + size * 3)) as usize);
    let max = size.saturating_sub(1).max(1);
    for y in 0..size {
        raw.push(0); // filter: none
        for x in 0..size {
            raw.push((x * 255 / max) as u8);
            raw.push((y * 255 / max) as u8);
            raw.push(0x80);
        }
    }
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, RGB, deflate, adaptive, no interlace

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    png_chunk(&mut out, b"IEND", &[]);
    out
}

fn png() -> &'static [u8] {
    static PNG: OnceLock<Vec<u8>> = OnceLock::new();
    PNG.get_or_init(|| generate_png(PNG_SIZE))
}

// ── Handlers ────────────────────────────────────────────────────────

/// (content type, `type`, `subtype`) in server preference order for `/image`.
const FORMATS: [(&str, &str, &str); 5] = [
    ("image/webp", "image", "webp"),
    ("image/svg+xml", "image", "svg+xml"),
    ("image/jpeg", "image", "jpeg"),
    ("image/png", "image", "png"),
    ("image/gif", "image", "gif"),
];

fn image_response(content_type: &'static str) -> Response {
    let body: &'static [u8] = match content_type {
        "image/png" => png(),
        "image/jpeg" => JPEG_8X8,
        "image/gif" => GIF_8X8,
        "image/webp" => WEBP_8X8,
        _ => SVG.as_bytes(),
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=3600"),
            ),
        ],
        body,
    )
        .into_response()
}

async fn negotiated_handler(headers: HeaderMap) -> Response {
    let has_accept = headers.contains_key(header::ACCEPT);
    let candidates: Vec<(&str, &str)> = FORMATS.iter().map(|(_, t, s)| (*t, *s)).collect();
    let mut resp = match choose(&headers, &candidates) {
        Some(i) => image_response(FORMATS[i].0),
        None if !has_accept => image_response("image/png"),
        None => negotiate_with_status(
            &headers,
            &ErrorResponse {
                error: "not_acceptable".to_string(),
                details: Some(
                    "Accept one of image/webp, image/svg+xml, image/jpeg, image/png, image/gif"
                        .to_string(),
                ),
            },
            StatusCode::NOT_ACCEPTABLE,
        ),
    };
    crate::content_negotiation::add_vary(resp.headers_mut(), "Accept");
    resp
}

async fn png_handler() -> Response {
    image_response("image/png")
}

async fn jpeg_handler() -> Response {
    image_response("image/jpeg")
}

async fn gif_handler() -> Response {
    image_response("image/gif")
}

async fn webp_handler() -> Response {
    image_response("image/webp")
}

async fn svg_handler() -> Response {
    image_response("image/svg+xml")
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/image", get(negotiated_handler))
        .route("/image/png", get(png_handler))
        .route("/image/jpeg", get(jpeg_handler))
        .route("/image/gif", get(gif_handler))
        .route("/image/webp", get(webp_handler))
        .route("/image/svg", get(svg_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/image",
            &["GET"],
            category::INFO,
            "Image in the format chosen by Accept (webp, svg, jpeg, png, gif; 406 otherwise)",
        )
        .example(Example::get("Image (negotiated)", "/image").header("Accept", "image/webp")),
        Endpoint::new("/image/png", &["GET"], category::INFO, "16x16 PNG image")
            .example(Example::get("PNG image", "/image/png")),
        Endpoint::new("/image/jpeg", &["GET"], category::INFO, "8x8 JPEG image")
            .example(Example::get("JPEG image", "/image/jpeg")),
        Endpoint::new("/image/gif", &["GET"], category::INFO, "8x8 GIF image")
            .example(Example::get("GIF image", "/image/gif")),
        Endpoint::new(
            "/image/webp",
            &["GET"],
            category::INFO,
            "8x8 lossless WebP image",
        )
        .example(Example::get("WebP image", "/image/webp")),
        Endpoint::new("/image/svg", &["GET"], category::INFO, "SVG image")
            .example(Example::get("SVG image", "/image/svg")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_bytes, get_request};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn fetch(uri: &str, accept: Option<&str>) -> (StatusCode, String, Vec<u8>) {
        let mut b = Request::builder().uri(uri);
        if let Some(a) = accept {
            b = b.header("accept", a);
        }
        let resp = crate::test_support::module_app(router)
            .oneshot(b.body(Body::empty()).expect("request"))
            .await
            .expect("response");
        let status = resp.status();
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        (status, ct, body_bytes(resp).await.to_vec())
    }

    fn be32(b: &[u8]) -> u32 {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    }

    /// Full PNG validation: signature, chunk CRCs, IHDR, zlib (stored
    /// blocks) with Adler-32, and the exact scanline payload size.
    fn validate_png(png: &[u8]) -> (u32, u32) {
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        let mut pos = 8;
        let mut dims = None;
        let mut idat = Vec::new();
        let mut saw_iend = false;
        while pos < png.len() {
            let len = be32(&png[pos..]) as usize;
            let kind = &png[pos + 4..pos + 8];
            let data = &png[pos + 8..pos + 8 + len];
            let crc = be32(&png[pos + 8 + len..]);
            assert_eq!(crc, crc32(&png[pos + 4..pos + 8 + len]), "CRC of {kind:?}");
            match kind {
                b"IHDR" => {
                    assert_eq!(len, 13);
                    assert_eq!(&data[8..13], &[8, 2, 0, 0, 0]);
                    dims = Some((be32(data), be32(&data[4..])));
                }
                b"IDAT" => idat.extend_from_slice(data),
                b"IEND" => saw_iend = true,
                _ => {}
            }
            pos += 12 + len;
        }
        assert!(saw_iend);
        assert_eq!(pos, png.len());
        // zlib: CMF/FLG check, stored blocks only (what we generate).
        assert_eq!((u16::from(idat[0]) << 8 | u16::from(idat[1])) % 31, 0);
        let mut p = 2;
        let mut raw = Vec::new();
        loop {
            let hdr = idat[p];
            assert_eq!(hdr & 0b110, 0, "stored block");
            let len = u16::from_le_bytes([idat[p + 1], idat[p + 2]]);
            let nlen = u16::from_le_bytes([idat[p + 3], idat[p + 4]]);
            assert_eq!(len, !nlen);
            raw.extend_from_slice(&idat[p + 5..p + 5 + len as usize]);
            p += 5 + len as usize;
            if hdr & 1 == 1 {
                break;
            }
        }
        assert_eq!(be32(&idat[p..]), adler32(&raw));
        let (w, h) = dims.expect("IHDR");
        assert_eq!(raw.len() as u32, h * (1 + w * 3));
        (w, h)
    }

    #[test]
    fn checksums_match_known_vectors() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[tokio::test]
    async fn png_is_valid() {
        let (status, ct, body) = fetch("/image/png", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct, "image/png");
        assert_eq!(validate_png(&body), (PNG_SIZE, PNG_SIZE));
        // Large images span several stored blocks.
        validate_png(&generate_png(200));
    }

    #[tokio::test]
    async fn jpeg_is_valid() {
        let (status, ct, b) = fetch("/image/jpeg", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct, "image/jpeg");
        assert_eq!(&b[..2], &[0xFF, 0xD8]);
        assert_eq!(&b[b.len() - 2..], &[0xFF, 0xD9]);
        // Walk the marker segments up to SOS; every length must fit.
        let mut pos = 2;
        let mut sof = None;
        loop {
            assert_eq!(b[pos], 0xFF, "marker at {pos}");
            let marker = b[pos + 1];
            let len = usize::from(u16::from_be_bytes([b[pos + 2], b[pos + 3]]));
            assert!(pos + 2 + len <= b.len());
            if marker == 0xC0 {
                let h = u16::from_be_bytes([b[pos + 5], b[pos + 6]]);
                let w = u16::from_be_bytes([b[pos + 7], b[pos + 8]]);
                sof = Some((w, h));
            }
            pos += 2 + len;
            if marker == 0xDA {
                break;
            }
        }
        assert_eq!(sof, Some((8, 8)));
    }

    #[tokio::test]
    async fn gif_is_valid() {
        let (status, ct, b) = fetch("/image/gif", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct, "image/gif");
        assert_eq!(&b[..6], b"GIF89a");
        assert_eq!(u16::from_le_bytes([b[6], b[7]]), 8);
        assert_eq!(u16::from_le_bytes([b[8], b[9]]), 8);
        // Global colour table of 2 entries, then extension / image blocks.
        let gct = 3 * (1usize << ((b[10] & 0x07) + 1));
        let mut pos = 13 + gct;
        let mut images = 0;
        loop {
            match b[pos] {
                0x21 => {
                    pos += 2;
                    while b[pos] != 0 {
                        pos += 1 + usize::from(b[pos]);
                    }
                    pos += 1;
                }
                0x2C => {
                    images += 1;
                    assert_eq!(b[pos + 9] & 0x80, 0, "no local colour table");
                    pos += 10 + 1; // descriptor + LZW min code size
                    while b[pos] != 0 {
                        pos += 1 + usize::from(b[pos]);
                    }
                    pos += 1;
                }
                0x3B => break,
                other => panic!("unexpected GIF block {other:#x}"),
            }
        }
        assert_eq!(images, 1);
        assert_eq!(pos, b.len() - 1);
    }

    #[tokio::test]
    async fn webp_is_valid() {
        let (status, ct, b) = fetch("/image/webp", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct, "image/webp");
        assert_eq!(&b[..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize,
            b.len() - 8
        );
        assert_eq!(&b[8..16], b"WEBPVP8L");
        let chunk = u32::from_le_bytes([b[16], b[17], b[18], b[19]]) as usize;
        assert_eq!(20 + chunk + (chunk & 1), b.len());
        assert_eq!(b[20], 0x2F, "VP8L signature");
        let bits = u32::from_le_bytes([b[21], b[22], b[23], b[24]]);
        assert_eq!((bits & 0x3FFF) + 1, 8);
        assert_eq!(((bits >> 14) & 0x3FFF) + 1, 8);
    }

    #[tokio::test]
    async fn svg_is_well_formed() {
        let (status, ct, b) = fetch("/image/svg", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct, "image/svg+xml");
        let text = String::from_utf8(b).expect("utf8");
        let mut reader = quick_xml::Reader::from_str(&text);
        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(_) => {}
                Err(e) => panic!("invalid SVG: {e}"),
            }
        }
    }

    #[tokio::test]
    async fn negotiated_image() {
        assert_eq!(fetch("/image", None).await.1, "image/png");
        assert_eq!(
            fetch("/image", Some("image/webp,*/*")).await.1,
            "image/webp"
        );
        assert_eq!(fetch("/image", Some("image/jpeg")).await.1, "image/jpeg");
        assert_eq!(fetch("/image", Some("image/*")).await.1, "image/webp");
        assert_eq!(
            fetch("/image", Some("image/png;q=0.5, image/gif")).await.1,
            "image/gif"
        );
        let (status, ct, _) = fetch("/image", Some("text/html")).await;
        assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
        assert_eq!(ct, "application/json");
        let resp = crate::test_support::module_app(router)
            .oneshot(get_request("/image"))
            .await
            .expect("response");
        assert_eq!(resp.headers()["vary"], "Accept");
    }
}
