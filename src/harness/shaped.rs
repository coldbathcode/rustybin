//! The shaped endpoint `/harness/{*path}` (plan section 8, the server side):
//! an upstream whose behaviour the run chooses. A marked request of a known
//! run gets that run's server profile (a log-normal latency from its p50 and
//! p99, a weighted status mix, a padded JSON body, optionally streamed in
//! chunks after a first-byte delay); anything else gets the default profile
//! (200, 256 bytes, no delay), so the route is a plain stub too.

use std::time::Duration;

use axum::body::Body;
use axum::extract::{OriginalUri, Request};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde_json::json;

use super::correlate::HarnessRequest;
use super::scenario::{ServerProfile, MAX_SERVER_DELAY};

/// Chunks a streamed body is cut into.
pub const CHUNKS: usize = 8;
/// Padding character of the body.
const PAD: &str = "x";

/// A log-normal sample with the given median and 99th percentile (the
/// standard normal's 0.99 quantile is 2.326); zero when both are zero.
pub fn sample_latency(p50: Duration, p99: Duration) -> Duration {
    if p99.is_zero() {
        return Duration::ZERO;
    }
    let p50 = p50.as_secs_f64().max(1e-6);
    let p99 = p99.as_secs_f64().max(p50);
    let sigma = (p99 / p50).ln() / 2.326;
    // Box-Muller from two uniform samples.
    let u1: f64 = rand::random::<f64>().max(1e-12);
    let u2: f64 = rand::random();
    let z = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
    let secs = p50 * (sigma * z).exp();
    Duration::from_secs_f64(secs.clamp(0.0, MAX_SERVER_DELAY.as_secs_f64()))
}

/// Pick a status by weight.
pub fn pick_status(profile: &ServerProfile) -> StatusCode {
    let total: u64 = profile.status.values().map(|w| u64::from(*w)).sum();
    if total == 0 {
        return StatusCode::OK;
    }
    let mut point = rand::random::<u64>() % total;
    for (code, weight) in &profile.status {
        let w = u64::from(*weight);
        if point < w {
            return code
                .parse::<u16>()
                .ok()
                .filter(|c| (100..=599).contains(c))
                .and_then(|c| StatusCode::from_u16(c).ok())
                .unwrap_or(StatusCode::OK);
        }
        point -= w;
    }
    StatusCode::OK
}

fn body_json(run: Option<&str>, seq: Option<u64>, path: &str, status: u16, size: usize) -> Vec<u8> {
    let mut v = json!({
        "shaped": true,
        "run": run,
        "seq": seq,
        "path": path,
        "status": status,
        "padding": "",
    });
    let base = v.to_string().len();
    if size > base {
        v["padding"] = json!(PAD.repeat(size - base));
    }
    v.to_string().into_bytes()
}

/// `GET /harness/{*path}` (any method).
pub async fn handler(OriginalUri(uri): OriginalUri, req: Request) -> Response {
    let marked = req.extensions().get::<HarnessRequest>().cloned();
    let default = ServerProfile::default();
    let profile = marked
        .as_ref()
        .map(|m| m.entry.profile.clone())
        .unwrap_or(default);
    let latency = sample_latency(profile.latency.p50.0, profile.latency.p99.0);
    let status = pick_status(&profile);
    let body = body_json(
        marked.as_ref().map(|m| m.run.as_str()),
        marked.as_ref().map(|m| m.seq),
        uri.path(),
        status.as_u16(),
        profile.body_bytes as usize,
    );
    let first_byte = profile.first_byte_delay.0.min(MAX_SERVER_DELAY);
    if !latency.is_zero() {
        tokio::time::sleep(latency).await;
    }
    let mut resp = if profile.chunked {
        let chunk = body.len().div_ceil(CHUNKS).max(1);
        let pieces: Vec<Bytes> = body.chunks(chunk).map(Bytes::copy_from_slice).collect();
        let gap = (latency / 4).min(Duration::from_millis(250));
        let stream = async_stream::stream! {
            if !first_byte.is_zero() {
                tokio::time::sleep(first_byte).await;
            }
            for (i, piece) in pieces.into_iter().enumerate() {
                if i > 0 && !gap.is_zero() {
                    tokio::time::sleep(gap / CHUNKS as u32).await;
                }
                yield Ok::<Bytes, std::convert::Infallible>(piece);
            }
        };
        (status, Body::from_stream(stream)).into_response()
    } else {
        if !first_byte.is_zero() {
            tokio::time::sleep(first_byte).await;
        }
        (status, body).into_response()
    };
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::HeaderName::from_static("x-rustybin-shaped"),
        HeaderValue::from_static("1"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_samples_follow_the_profile() {
        assert_eq!(
            sample_latency(Duration::ZERO, Duration::ZERO),
            Duration::ZERO
        );
        let p50 = Duration::from_millis(20);
        let p99 = Duration::from_millis(200);
        let mut samples: Vec<f64> = (0..4000)
            .map(|_| sample_latency(p50, p99).as_secs_f64() * 1000.0)
            .collect();
        samples.sort_by(f64::total_cmp);
        let median = samples[2000];
        let high = samples[3960];
        assert!((15.0..=26.0).contains(&median), "median {median}");
        assert!((120.0..=320.0).contains(&high), "p99 {high}");
        assert!(samples.iter().all(|s| *s <= 30_000.0));
    }

    #[test]
    fn statuses_follow_the_weights() {
        let mut p = ServerProfile {
            status: [("200".to_string(), 9), ("503".to_string(), 1)].into(),
            ..ServerProfile::default()
        };
        let mut errors = 0;
        for _ in 0..2000 {
            if pick_status(&p) == StatusCode::SERVICE_UNAVAILABLE {
                errors += 1;
            }
        }
        assert!((120..=300).contains(&errors), "{errors}");
        p.status = [("999".to_string(), 1)].into();
        assert_eq!(pick_status(&p), StatusCode::OK);
    }

    #[test]
    fn body_is_padded_to_size() {
        let b = body_json(Some("r"), Some(1), "/harness/x", 200, 512);
        assert_eq!(b.len(), 512);
        let small = body_json(None, None, "/", 200, 0);
        assert!(small.len() < 100);
        assert!(serde_json::from_slice::<serde_json::Value>(&b).is_ok());
    }
}
