//! Webhook signature verification and signing (no outbound delivery).
//!
//! Schemes:
//! - Standard Webhooks (<https://www.standardwebhooks.com>): headers
//!   `webhook-id`, `webhook-timestamp` (unix seconds) and `webhook-signature`
//!   (space separated `v1,<base64 HMAC-SHA256>` entries) over
//!   `"{id}.{timestamp}.{body}"`. The secret is `whsec_<base64 key>`; the HMAC
//!   key is the base64-decoded part. Timestamps must be within 5 minutes.
//! - GitHub style: `X-Hub-Signature-256: sha256=<hex HMAC-SHA256(body)>`, the
//!   key is the secret string as is.
//! - Stripe style: `Stripe-Signature: t=<unix>,v1=<hex>` where the HMAC
//!   (key = the full secret string, `whsec_` prefix included) covers
//!   `"{t}.{body}"`; 5 minute tolerance.
//!
//! Endpoints verify incoming requests and report precisely why they fail,
//! sign example payloads so a gateway's webhook validation can be tested, and
//! receive with built-in demo secrets. Rustybin never sends webhooks itself
//! (no outbound requests, so no SSRF).
// Handlers return early with ready-made responses (as admin::require_admin does).
#![allow(clippy::result_large_err)]

use axum::body::Bytes;
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;

use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

type HmacSha256 = Hmac<Sha256>;

/// Default timestamp tolerance (Standard Webhooks and Stripe), in seconds.
pub const DEFAULT_TOLERANCE_SECS: i64 = 300;
/// Largest accepted `?tolerance=`.
pub const MAX_TOLERANCE_SECS: i64 = 30 * 24 * 3600;
/// Prefix of Standard Webhooks secrets.
pub const SECRET_PREFIX: &str = "whsec_";

/// Built-in demo secrets for `/webhooks/receive/{secret_id}`. `demo` is the
/// secret of the Standard Webhooks reference test vector; `github-docs` is
/// the example secret from GitHub's webhook documentation.
pub const DEMO_SECRETS: &[(&str, &str)] = &[
    ("demo", "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw"),
    ("github-docs", "It's a Secret to Everybody"),
];

fn demo_secret(id: &str) -> Option<&'static str> {
    DEMO_SECRETS
        .iter()
        .find(|(name, _)| *name == id)
        .map(|(_, s)| *s)
}

/// Supported signature schemes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Standard,
    GitHub,
    Stripe,
}

impl Scheme {
    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "standard" | "standard-webhooks" | "standardwebhooks" => Some(Self::Standard),
            "github" => Some(Self::GitHub),
            "stripe" => Some(Self::Stripe),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Standard => "standard-webhooks",
            Self::GitHub => "github",
            Self::Stripe => "stripe",
        }
    }
}

/// A verification failure with a machine-readable code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyError {
    pub code: &'static str,
    pub message: String,
}

impl VerifyError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 400 for malformed input, 401 for a well-formed but invalid signature.
    pub fn status(&self) -> StatusCode {
        match self.code {
            "timestamp_too_old"
            | "timestamp_too_new"
            | "signature_mismatch"
            | "no_supported_signature" => StatusCode::UNAUTHORIZED,
            _ => StatusCode::BAD_REQUEST,
        }
    }
}

/// Details of a verification attempt (filled as far as the input allows).
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub webhook_id: Option<String>,
    pub timestamp: Option<i64>,
    pub age_seconds: Option<i64>,
    pub received: Vec<String>,
    pub expected: Option<String>,
    pub signature_valid: Option<bool>,
    pub timestamp_valid: Option<bool>,
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    // HMAC accepts keys of any length, so this never fails.
    let mut mac = match HmacSha256::new_from_slice(key) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    for p in parts {
        mac.update(p);
    }
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// HMAC key of a Standard Webhooks secret (`whsec_` prefix optional).
/// Spaces are read as `+` (a common URL-encoding accident with `?secret=`).
pub fn standard_key(secret: &str) -> Result<Vec<u8>, VerifyError> {
    let raw = secret.trim();
    let raw = raw
        .strip_prefix(SECRET_PREFIX)
        .unwrap_or(raw)
        .replace(' ', "+");
    let key = base64::engine::general_purpose::STANDARD
        .decode(raw.as_bytes())
        .map_err(|_| {
            VerifyError::new(
                "invalid_secret",
                "secret must be whsec_ followed by base64 (Standard Webhooks)",
            )
        })?;
    if key.is_empty() {
        return Err(VerifyError::new("invalid_secret", "secret is empty"));
    }
    Ok(key)
}

/// `v1,<base64>` signature of a Standard Webhooks message.
pub fn standard_sign(
    secret: &str,
    msg_id: &str,
    timestamp: i64,
    body: &[u8],
) -> Result<String, VerifyError> {
    let key = standard_key(secret)?;
    let ts = timestamp.to_string();
    let mac = hmac_sha256(&key, &[msg_id.as_bytes(), b".", ts.as_bytes(), b".", body]);
    Ok(format!(
        "v1,{}",
        base64::engine::general_purpose::STANDARD.encode(mac)
    ))
}

/// `sha256=<hex>` (GitHub `X-Hub-Signature-256`).
pub fn github_sign(secret: &str, body: &[u8]) -> String {
    format!("sha256={}", hex(&hmac_sha256(secret.as_bytes(), &[body])))
}

/// `t=<ts>,v1=<hex>` (Stripe `Stripe-Signature`).
pub fn stripe_sign(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let ts = timestamp.to_string();
    let mac = hmac_sha256(secret.as_bytes(), &[ts.as_bytes(), b".", body]);
    format!("t={ts},v1={}", hex(&mac))
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
}

fn check_timestamp(
    report: &mut Report,
    ts: i64,
    now: i64,
    tolerance: Option<i64>,
) -> Option<VerifyError> {
    report.timestamp = Some(ts);
    report.age_seconds = Some(now - ts);
    let Some(tol) = tolerance else {
        report.timestamp_valid = Some(true);
        return None;
    };
    let err = if now - ts > tol {
        Some(VerifyError::new(
            "timestamp_too_old",
            format!(
                "timestamp is {} s old, tolerance is {tol} s (replay protection)",
                now - ts
            ),
        ))
    } else if ts - now > tol {
        Some(VerifyError::new(
            "timestamp_too_new",
            format!(
                "timestamp is {} s in the future, tolerance is {tol} s",
                ts - now
            ),
        ))
    } else {
        None
    };
    report.timestamp_valid = Some(err.is_none());
    err
}

/// Verify a Standard Webhooks request. `tolerance: None` skips the timestamp
/// check (the report still shows the age).
pub fn verify_standard(
    secret: &str,
    headers: &HeaderMap,
    body: &[u8],
    now: i64,
    tolerance: Option<i64>,
) -> (Report, Result<(), VerifyError>) {
    let mut report = Report::default();
    let id = header(headers, "webhook-id");
    let ts = header(headers, "webhook-timestamp");
    let sig = header(headers, "webhook-signature");
    let missing: Vec<&str> = [
        ("webhook-id", id),
        ("webhook-timestamp", ts),
        ("webhook-signature", sig),
    ]
    .iter()
    .filter(|(_, v)| v.is_none_or(str::is_empty))
    .map(|(n, _)| *n)
    .collect();
    if !missing.is_empty() {
        return (
            report,
            Err(VerifyError::new(
                "missing_headers",
                format!("missing header(s): {}", missing.join(", ")),
            )),
        );
    }
    let (id, ts, sig) = (id.unwrap_or(""), ts.unwrap_or(""), sig.unwrap_or(""));
    report.webhook_id = Some(id.to_string());
    report.received = sig.split_whitespace().map(str::to_string).collect();
    let Ok(ts) = ts.parse::<i64>() else {
        return (
            report,
            Err(VerifyError::new(
                "invalid_timestamp",
                "webhook-timestamp must be an integer number of seconds since the epoch",
            )),
        );
    };
    let ts_err = check_timestamp(&mut report, ts, now, tolerance);
    let expected = match standard_sign(secret, id, ts, body) {
        Ok(s) => s,
        Err(e) => return (report, Err(e)),
    };
    report.expected = Some(expected.clone());
    let expected_mac = expected.trim_start_matches("v1,").as_bytes().to_vec();
    let v1: Vec<&str> = report
        .received
        .iter()
        .filter_map(|s| s.strip_prefix("v1,"))
        .collect();
    let sig_err = if v1.is_empty() {
        Some(VerifyError::new(
            "no_supported_signature",
            "webhook-signature has no v1,<base64> entry (only symmetric v1 signatures are supported)",
        ))
    } else if v1
        .iter()
        .any(|s| constant_time_eq(s.as_bytes(), &expected_mac))
    {
        None
    } else {
        Some(VerifyError::new(
            "signature_mismatch",
            "no v1 signature matches HMAC-SHA256(secret, \"{webhook-id}.{webhook-timestamp}.{body}\")",
        ))
    };
    report.signature_valid = Some(sig_err.is_none());
    let result = match (ts_err, sig_err) {
        (Some(e), _) | (None, Some(e)) => Err(e),
        (None, None) => Ok(()),
    };
    (report, result)
}

/// Verify a GitHub style `X-Hub-Signature-256` request.
pub fn verify_github(
    secret: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> (Report, Result<(), VerifyError>) {
    let mut report = Report::default();
    let Some(sig) = header(headers, "x-hub-signature-256").filter(|s| !s.is_empty()) else {
        return (
            report,
            Err(VerifyError::new(
                "missing_headers",
                "missing header(s): x-hub-signature-256",
            )),
        );
    };
    report.received = vec![sig.to_string()];
    let expected = github_sign(secret, body);
    report.expected = Some(expected.clone());
    let Some(hex_sig) = sig.strip_prefix("sha256=") else {
        return (
            report,
            Err(VerifyError::new(
                "invalid_signature_header",
                "x-hub-signature-256 must look like sha256=<hex>",
            )),
        );
    };
    let ok = unhex(&hex_sig.to_ascii_lowercase()).is_some_and(|got| {
        unhex(expected.trim_start_matches("sha256="))
            .is_some_and(|want| constant_time_eq(&got, &want))
    });
    report.signature_valid = Some(ok);
    let result = if ok {
        Ok(())
    } else {
        Err(VerifyError::new(
            "signature_mismatch",
            "signature does not match HMAC-SHA256(secret, body)",
        ))
    };
    (report, result)
}

/// Verify a Stripe style `Stripe-Signature` request.
pub fn verify_stripe(
    secret: &str,
    headers: &HeaderMap,
    body: &[u8],
    now: i64,
    tolerance: Option<i64>,
) -> (Report, Result<(), VerifyError>) {
    let mut report = Report::default();
    let Some(sig) = header(headers, "stripe-signature").filter(|s| !s.is_empty()) else {
        return (
            report,
            Err(VerifyError::new(
                "missing_headers",
                "missing header(s): stripe-signature",
            )),
        );
    };
    let mut ts = None;
    let mut v1 = Vec::new();
    for part in sig.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => ts = Some(v),
            Some(("v1", v)) => v1.push(v.to_string()),
            _ => {}
        }
    }
    report.received = v1.clone();
    let Some(ts) = ts else {
        return (
            report,
            Err(VerifyError::new(
                "invalid_signature_header",
                "stripe-signature has no t=<timestamp> element",
            )),
        );
    };
    let Ok(ts) = ts.parse::<i64>() else {
        return (
            report,
            Err(VerifyError::new(
                "invalid_timestamp",
                "t= must be an integer number of seconds since the epoch",
            )),
        );
    };
    let ts_err = check_timestamp(&mut report, ts, now, tolerance);
    let expected = stripe_sign(secret, ts, body);
    let want = expected
        .split_once(",v1=")
        .map(|(_, h)| h.to_string())
        .unwrap_or_default();
    report.expected = Some(expected);
    let sig_err = if v1.is_empty() {
        Some(VerifyError::new(
            "no_supported_signature",
            "stripe-signature has no v1=<hex> element",
        ))
    } else if v1
        .iter()
        .any(|s| constant_time_eq(s.to_ascii_lowercase().as_bytes(), want.as_bytes()))
    {
        None
    } else {
        Some(VerifyError::new(
            "signature_mismatch",
            "no v1 signature matches HMAC-SHA256(secret, \"{t}.{body}\")",
        ))
    };
    report.signature_valid = Some(sig_err.is_none());
    let result = match (ts_err, sig_err) {
        (Some(e), _) | (None, Some(e)) => Err(e),
        (None, None) => Ok(()),
    };
    (report, result)
}

// ── Handlers ────────────────────────────────────────────────────────

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

#[derive(Debug, Default, Deserialize)]
struct VerifyQuery {
    secret: Option<String>,
    tolerance: Option<i64>,
    ignore_timestamp: Option<bool>,
}

fn verify_response(
    scheme: Scheme,
    report: Report,
    result: Result<(), VerifyError>,
    now: i64,
    tolerance: Option<i64>,
) -> Response {
    let mut body = json!({
        "valid": result.is_ok(),
        "scheme": scheme.name(),
        "webhook_id": report.webhook_id,
        "timestamp": report.timestamp,
        "age_seconds": report.age_seconds,
        "now": now,
        "tolerance_seconds": tolerance,
        "signature_valid": report.signature_valid,
        "timestamp_valid": report.timestamp_valid,
        "received_signatures": report.received,
        "expected_signature": report.expected,
    });
    match result {
        Ok(()) => (StatusCode::OK, Json(body)).into_response(),
        Err(e) => {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("error".into(), json!(e.code));
                obj.insert("message".into(), json!(e.message));
            }
            (e.status(), Json(body)).into_response()
        }
    }
}

fn tolerance_of(q: &VerifyQuery) -> Result<Option<i64>, Response> {
    if q.ignore_timestamp.unwrap_or(false) {
        return Ok(None);
    }
    let tol = q.tolerance.unwrap_or(DEFAULT_TOLERANCE_SECS);
    if !(0..=MAX_TOLERANCE_SECS).contains(&tol) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_tolerance",
            &format!("tolerance must be between 0 and {MAX_TOLERANCE_SECS} seconds"),
        ));
    }
    Ok(Some(tol))
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "valid": false, "error": code, "message": message })),
    )
        .into_response()
}

fn secret_of(headers: &HeaderMap, q: &VerifyQuery) -> Result<String, Response> {
    header(headers, "x-webhook-secret")
        .map(str::to_string)
        .or_else(|| q.secret.clone())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            error(
                StatusCode::BAD_REQUEST,
                "missing_secret",
                "pass the secret in the X-Webhook-Secret header or ?secret=",
            )
        })
}

async fn verify_handler(
    Path(scheme): Path<String>,
    Query(q): Query<VerifyQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match Scheme::parse(&scheme) {
        Some(s) => verify_scheme(s, q, headers, body),
        None => error(
            StatusCode::NOT_FOUND,
            "unknown_scheme",
            "supported schemes: standard, github, stripe",
        ),
    }
}

async fn verify_standard_handler(
    Query(q): Query<VerifyQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    verify_scheme(Scheme::Standard, q, headers, body)
}

fn verify_scheme(scheme: Scheme, q: VerifyQuery, headers: HeaderMap, body: Bytes) -> Response {
    let secret = match secret_of(&headers, &q) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let tolerance = match tolerance_of(&q) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    let now = now_secs();
    let (report, result) = match scheme {
        Scheme::Standard => verify_standard(&secret, &headers, &body, now, tolerance),
        Scheme::GitHub => verify_github(&secret, &headers, &body),
        Scheme::Stripe => verify_stripe(&secret, &headers, &body, now, tolerance),
    };
    let tolerance = if scheme == Scheme::GitHub {
        None
    } else {
        tolerance
    };
    verify_response(scheme, report, result, now, tolerance)
}

async fn receive_handler(
    Path(secret_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(secret) = demo_secret(&secret_id) else {
        let ids: Vec<&str> = DEMO_SECRETS.iter().map(|(n, _)| *n).collect();
        return error(
            StatusCode::NOT_FOUND,
            "unknown_secret_id",
            &format!("demo secret ids: {}", ids.join(", ")),
        );
    };
    let now = now_secs();
    let tol = Some(DEFAULT_TOLERANCE_SECS);
    let (scheme, (_, result)) = if headers.contains_key("webhook-signature") {
        (
            Scheme::Standard,
            verify_standard(secret, &headers, &body, now, tol),
        )
    } else if headers.contains_key("x-hub-signature-256") {
        (Scheme::GitHub, verify_github(secret, &headers, &body))
    } else if headers.contains_key("stripe-signature") {
        (
            Scheme::Stripe,
            verify_stripe(secret, &headers, &body, now, tol),
        )
    } else {
        return error(
            StatusCode::UNAUTHORIZED,
            "missing_headers",
            "no webhook-signature, x-hub-signature-256 or stripe-signature header",
        );
    };
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "valid": false,
                "scheme": scheme.name(),
                "error": e.code,
                "message": e.message,
            })),
        )
            .into_response(),
    }
}

#[derive(Debug, Default, Deserialize)]
struct SignQuery {
    secret: Option<String>,
    scheme: Option<String>,
    payload: Option<String>,
    id: Option<String>,
    timestamp: Option<i64>,
}

fn default_payload(now: i64) -> String {
    json!({
        "type": "order.created",
        "timestamp": chrono::DateTime::from_timestamp(now, 0).map(|d| d.to_rfc3339()),
        "data": { "id": "ord_123", "amount": 4200, "currency": "EUR" },
    })
    .to_string()
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn sign(q: SignQuery, body: Option<String>) -> Response {
    let scheme = match q.scheme.as_deref().map(Scheme::parse) {
        None => Scheme::Standard,
        Some(Some(s)) => s,
        Some(None) => {
            return error(
                StatusCode::BAD_REQUEST,
                "unknown_scheme",
                "scheme must be standard, github or stripe",
            )
        }
    };
    let now = now_secs();
    let secret = q
        .secret
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEMO_SECRETS[0].1.to_string());
    let payload = body
        .filter(|b| !b.is_empty())
        .or(q.payload)
        .unwrap_or_else(|| default_payload(now));
    let timestamp = q.timestamp.unwrap_or(now);
    let id =
        q.id.filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
            .unwrap_or_else(|| format!("msg_{}", uuid::Uuid::new_v4().simple()));
    let (headers, signed_content, verify_path) = match scheme {
        Scheme::Standard => {
            let sig = match standard_sign(&secret, &id, timestamp, payload.as_bytes()) {
                Ok(s) => s,
                Err(e) => return error(StatusCode::BAD_REQUEST, e.code, &e.message),
            };
            (
                json!({
                    "webhook-id": id,
                    "webhook-timestamp": timestamp.to_string(),
                    "webhook-signature": sig,
                    "content-type": "application/json",
                }),
                format!("{id}.{timestamp}.{payload}"),
                "/webhooks/verify",
            )
        }
        Scheme::GitHub => (
            json!({
                "x-hub-signature-256": github_sign(&secret, payload.as_bytes()),
                "x-github-event": "ping",
                "x-github-delivery": id,
                "content-type": "application/json",
            }),
            payload.clone(),
            "/webhooks/verify/github",
        ),
        Scheme::Stripe => (
            json!({
                "stripe-signature": stripe_sign(&secret, timestamp, payload.as_bytes()),
                "content-type": "application/json",
            }),
            format!("{timestamp}.{payload}"),
            "/webhooks/verify/stripe",
        ),
    };
    let mut curl = String::from("curl -X POST");
    if let Some(map) = headers.as_object() {
        for (k, v) in map {
            curl.push_str(&format!(
                " -H {}",
                shell_quote(&format!("{k}: {}", v.as_str().unwrap_or("")))
            ));
        }
    }
    curl.push_str(&format!(
        " -H {}",
        shell_quote(&format!("x-webhook-secret: {secret}"))
    ));
    curl.push_str(&format!(
        " --data-raw {} http://localhost{verify_path}",
        shell_quote(&payload)
    ));
    let receive_path = DEMO_SECRETS
        .iter()
        .find(|(_, s)| *s == secret)
        .map(|(name, _)| format!("/webhooks/receive/{name}"));
    Json(json!({
        "scheme": scheme.name(),
        "secret": secret,
        "headers": headers,
        "body": payload,
        "signed_content": signed_content,
        "verify_path": verify_path,
        "receive_path": receive_path,
        "curl": curl,
        "note": "send exactly `body` (byte for byte) with `headers`; signatures cover the raw body",
    }))
    .into_response()
}

async fn sign_get(Query(q): Query<SignQuery>) -> Response {
    sign(q, None)
}

async fn sign_post(Query(q): Query<SignQuery>, body: Bytes) -> Response {
    match String::from_utf8(body.to_vec()) {
        Ok(s) => sign(q, Some(s)),
        Err(_) => error(
            StatusCode::BAD_REQUEST,
            "invalid_payload",
            "payload must be UTF-8 text",
        ),
    }
}

async fn info_handler() -> Response {
    let secrets: Vec<Value> = DEMO_SECRETS
        .iter()
        .map(|(id, secret)| {
            json!({ "id": id, "secret": secret, "receive_path": format!("/webhooks/receive/{id}") })
        })
        .collect();
    Json(json!({
        "schemes": {
            "standard-webhooks": {
                "headers": ["webhook-id", "webhook-timestamp", "webhook-signature"],
                "signature": "v1,<base64 HMAC-SHA256(base64decode(secret without whsec_), \"{id}.{timestamp}.{body}\")>",
                "tolerance_seconds": DEFAULT_TOLERANCE_SECS,
                "verify_path": "/webhooks/verify",
            },
            "github": {
                "headers": ["x-hub-signature-256"],
                "signature": "sha256=<hex HMAC-SHA256(secret, body)>",
                "verify_path": "/webhooks/verify/github",
            },
            "stripe": {
                "headers": ["stripe-signature"],
                "signature": "t=<unix>,v1=<hex HMAC-SHA256(secret, \"{t}.{body}\")>",
                "tolerance_seconds": DEFAULT_TOLERANCE_SECS,
                "verify_path": "/webhooks/verify/stripe",
            }
        },
        "demo_secrets": secrets,
        "sign_path": "/webhooks/sign",
        "outbound_delivery": false,
    }))
    .into_response()
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/webhooks", get(info_handler))
        .route("/webhooks/verify", post(verify_standard_handler))
        .route("/webhooks/verify/{scheme}", post(verify_handler))
        .route("/webhooks/sign", get(sign_get).post(sign_post))
        .route("/webhooks/receive/{secret_id}", post(receive_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/webhooks",
            &["GET"],
            category::REQUEST_BIN,
            "Webhook signature schemes and demo secrets",
        )
        .example(Example::get("Webhook schemes and demo secrets", "/webhooks")),
        Endpoint::new(
            "/webhooks/verify",
            &["POST"],
            category::REQUEST_BIN,
            "Verify a Standard Webhooks signature and explain failures",
        )
        .description(
            "Secret via X-Webhook-Secret header or ?secret= (whsec_<base64>). Checks webhook-id, \
             webhook-timestamp (default tolerance 300 s, ?tolerance=, ?ignore_timestamp=true) and \
             webhook-signature (v1,<base64 HMAC-SHA256 of id.timestamp.body>). 200 when valid; 401 \
             with error timestamp_too_old, timestamp_too_new, signature_mismatch or \
             no_supported_signature; 400 for missing_headers, missing_secret, invalid_secret, \
             invalid_timestamp. The reply includes the expected signature.",
        )
        .example(
            Example::post(
                "Verify the Standard Webhooks test vector",
                "/webhooks/verify?ignore_timestamp=true",
            )
            .header("X-Webhook-Secret", "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw")
            .header("webhook-id", "msg_p5jXN8AQM9LWM0D4loKWxJek")
            .header("webhook-timestamp", "1614265330")
            .header(
                "webhook-signature",
                "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=",
            )
            .json(r#"{"test": 2432232314}"#)
            .expect_status(200),
        )
        .example(
            Example::post("Stale timestamp is rejected", "/webhooks/verify")
                .header("X-Webhook-Secret", "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw")
                .header("webhook-id", "msg_p5jXN8AQM9LWM0D4loKWxJek")
                .header("webhook-timestamp", "1614265330")
                .header(
                    "webhook-signature",
                    "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=",
                )
                .json(r#"{"test": 2432232314}"#)
                .expect_status(401),
        ),
        Endpoint::new(
            "/webhooks/verify/{scheme}",
            &["POST"],
            category::REQUEST_BIN,
            "Verify a webhook signature: standard, github (X-Hub-Signature-256) or stripe (Stripe-Signature)",
        )
        .description(
            "Same secret handling and error codes as /webhooks/verify. github: sha256=<hex \
             HMAC-SHA256(secret, body)>, no timestamp. stripe: t=<unix>,v1=<hex HMAC-SHA256(secret, \
             t.body)> with the full secret string as key and a 300 s tolerance.",
        )
        .example(
            Example::post("Verify a GitHub style signature", "/webhooks/verify/github")
                .header("X-Webhook-Secret", "demo-secret")
                .header(
                    "X-Hub-Signature-256",
                    "sha256=a30169f10c965c2302e48cf57ce4d4b80cc383086b1f62f3ebeb54d21bab149a",
                )
                .json(r#"{"zen":"Keep it logically awesome."}"#)
                .expect_status(200),
        )
        .example(
            Example::post(
                "Verify a Stripe style signature",
                "/webhooks/verify/stripe?ignore_timestamp=true",
            )
            .header("X-Webhook-Secret", "whsec_test_secret")
            .header(
                "Stripe-Signature",
                "t=1700000000,v1=13941114bb88ac44a76abcfddea5b92aa6182a4b63d8be3aae908a616083bd7e",
            )
            .json(r#"{"id":"evt_test"}"#)
            .expect_status(200),
        ),
        Endpoint::new(
            "/webhooks/sign",
            &["GET", "POST"],
            category::REQUEST_BIN,
            "Produce a signed example webhook (headers + body) for a secret",
        )
        .description(
            "Query: scheme=standard|github|stripe (default standard), secret (default the `demo` \
             secret), payload (or the POST body), id, timestamp. Returns the headers, body, signed \
             content and a curl command, ready to replay through a gateway's webhook validation.",
        )
        .example(Example::get("Sign the default payload", "/webhooks/sign"))
        .example(
            Example::post("Sign a payload (Stripe style)", "/webhooks/sign?scheme=stripe")
                .json(r#"{"id":"evt_1","type":"invoice.paid"}"#),
        ),
        Endpoint::new(
            "/webhooks/receive/{secret_id}",
            &["POST"],
            category::REQUEST_BIN,
            "Webhook receiver with a demo secret: 204 when valid, 401 otherwise",
        )
        .description(
            "Demo secrets: `demo` = whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw, `github-docs` = It's a \
             Secret to Everybody. The scheme is detected from the headers (webhook-signature, \
             x-hub-signature-256 or stripe-signature).",
        )
        .example(
            Example::post("Unsigned delivery is rejected", "/webhooks/receive/demo")
                .json(r#"{"type":"order.created"}"#)
                .expect_status(401),
        )
        .example(
            Example::post(
                "GitHub style delivery signed with the demo secret",
                "/webhooks/receive/demo",
            )
            .header(
                "X-Hub-Signature-256",
                "sha256=410626f706ca56b1a5ff8511e5b7408344cf2174e3ad856b2f8b4b5a3bc69c81",
            )
            .json(r#"{"zen":"Keep it logically awesome."}"#)
            .expect_status(204),
        ),
    ]
}

pub fn openapi_paths() -> Value {
    let tags = json!(["Webhooks"]);
    let secret_header = json!({ "name": "X-Webhook-Secret", "in": "header", "required": false, "schema": { "type": "string" }, "description": "Signing secret (or ?secret=)" });
    let secret_q = json!({ "name": "secret", "in": "query", "required": false, "schema": { "type": "string" } });
    let tolerance = json!({ "name": "tolerance", "in": "query", "required": false, "schema": { "type": "integer", "default": DEFAULT_TOLERANCE_SECS } });
    let ignore = json!({ "name": "ignore_timestamp", "in": "query", "required": false, "schema": { "type": "boolean" } });
    let raw_body =
        json!({ "required": true, "content": { "*/*": { "schema": { "type": "string" } } } });
    let verify_responses = json!({
        "200": { "description": "Valid signature (report)" },
        "400": { "description": "Malformed input: missing_headers, missing_secret, invalid_secret, invalid_timestamp" },
        "401": { "description": "Invalid: signature_mismatch, timestamp_too_old, timestamp_too_new, no_supported_signature" }
    });
    json!({
        "/webhooks": {
            "get": {
                "tags": tags, "summary": "Webhook schemes and demo secrets", "operationId": "webhooksInfo",
                "responses": { "200": { "description": "Schemes and demo secrets" } }
            }
        },
        "/webhooks/verify": {
            "post": {
                "tags": tags, "summary": "Verify a Standard Webhooks signature", "operationId": "verifyWebhook",
                "parameters": [secret_header, secret_q, tolerance, ignore,
                    { "name": "webhook-id", "in": "header", "schema": { "type": "string" } },
                    { "name": "webhook-timestamp", "in": "header", "schema": { "type": "string" } },
                    { "name": "webhook-signature", "in": "header", "schema": { "type": "string" } }],
                "requestBody": raw_body,
                "responses": verify_responses
            }
        },
        "/webhooks/verify/{scheme}": {
            "post": {
                "tags": tags, "summary": "Verify a webhook signature (standard, github, stripe)", "operationId": "verifyWebhookScheme",
                "parameters": [
                    { "name": "scheme", "in": "path", "required": true, "schema": { "type": "string", "enum": ["standard", "github", "stripe"] } },
                    secret_header, secret_q, tolerance, ignore
                ],
                "requestBody": raw_body,
                "responses": verify_responses
            }
        },
        "/webhooks/sign": {
            "get": {
                "tags": tags, "summary": "Produce a signed example webhook", "operationId": "signWebhook",
                "parameters": [
                    { "name": "scheme", "in": "query", "schema": { "type": "string", "enum": ["standard", "github", "stripe"] } },
                    secret_q,
                    { "name": "payload", "in": "query", "schema": { "type": "string" } },
                    { "name": "id", "in": "query", "schema": { "type": "string" } },
                    { "name": "timestamp", "in": "query", "schema": { "type": "integer" } }
                ],
                "responses": { "200": { "description": "Headers, body, signed content and curl command" }, "400": { "description": "Invalid secret or scheme" } }
            },
            "post": {
                "tags": tags, "summary": "Sign the request body", "operationId": "signWebhookBody",
                "parameters": [
                    { "name": "scheme", "in": "query", "schema": { "type": "string" } },
                    secret_q,
                    { "name": "id", "in": "query", "schema": { "type": "string" } },
                    { "name": "timestamp", "in": "query", "schema": { "type": "integer" } }
                ],
                "requestBody": raw_body,
                "responses": { "200": { "description": "Headers, body, signed content and curl command" }, "400": { "description": "Invalid secret or scheme" } }
            }
        },
        "/webhooks/receive/{secret_id}": {
            "post": {
                "tags": tags, "summary": "Receive a webhook signed with a demo secret", "operationId": "receiveWebhook",
                "parameters": [{ "name": "secret_id", "in": "path", "required": true, "schema": { "type": "string", "enum": ["demo", "github-docs"] } }],
                "requestBody": raw_body,
                "responses": { "204": { "description": "Valid" }, "401": { "description": "Invalid or unsigned" }, "404": { "description": "Unknown secret id" } }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_json, module_app};
    use axum::body::Body;
    use axum::http::{HeaderValue, Request};
    use tower::ServiceExt;

    const SECRET: &str = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
    const MSG_ID: &str = "msg_p5jXN8AQM9LWM0D4loKWxJek";
    const TS: i64 = 1614265330;
    const PAYLOAD: &str = r#"{"test": 2432232314}"#;
    // Cross-checked with Python: base64(hmac.new(b64decode(key), msg, sha256)).
    const SIG: &str = "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=";

    fn std_headers(id: &str, ts: i64, sig: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("webhook-id", HeaderValue::from_str(id).expect("v"));
        h.insert(
            "webhook-timestamp",
            HeaderValue::from_str(&ts.to_string()).expect("v"),
        );
        h.insert("webhook-signature", HeaderValue::from_str(sig).expect("v"));
        h
    }

    #[test]
    fn standard_webhooks_test_vector() {
        let sig = standard_sign(SECRET, MSG_ID, TS, PAYLOAD.as_bytes()).expect("sign");
        assert_eq!(sig, SIG);
        let h = std_headers(MSG_ID, TS, SIG);
        let (_, r) = verify_standard(SECRET, &h, PAYLOAD.as_bytes(), TS + 10, Some(300));
        assert_eq!(r, Ok(()));
        // Several signatures, one valid (key rotation).
        let h = std_headers(MSG_ID, TS, &format!("v1,bm90LXZhbGlk {SIG} v1a,xyz"));
        assert!(
            verify_standard(SECRET, &h, PAYLOAD.as_bytes(), TS, Some(300))
                .1
                .is_ok()
        );
    }

    #[test]
    fn standard_failures_are_precise() {
        let body = PAYLOAD.as_bytes();
        let h = std_headers(MSG_ID, TS, SIG);
        let code = |r: Result<(), VerifyError>| r.err().map(|e| e.code);
        let (report, r) = verify_standard(SECRET, &h, body, TS + 301, Some(300));
        assert_eq!(code(r), Some("timestamp_too_old"));
        assert_eq!(report.signature_valid, Some(true));
        let (_, r) = verify_standard(SECRET, &h, body, TS - 301, Some(300));
        assert_eq!(code(r), Some("timestamp_too_new"));
        let (_, r) = verify_standard(SECRET, &h, b"{}", TS, Some(300));
        assert_eq!(code(r), Some("signature_mismatch"));
        let h2 = std_headers(MSG_ID, TS, "v1a,abc");
        let (_, r) = verify_standard(SECRET, &h2, body, TS, Some(300));
        assert_eq!(code(r), Some("no_supported_signature"));
        let mut h3 = h.clone();
        h3.remove("webhook-id");
        let (_, r) = verify_standard(SECRET, &h3, body, TS, Some(300));
        assert_eq!(r.unwrap_err().message, "missing header(s): webhook-id");
        let (_, r) = verify_standard("whsec_!!!", &h, body, TS, Some(300));
        assert_eq!(code(r), Some("invalid_secret"));
        // Timestamp check disabled.
        let (_, r) = verify_standard(SECRET, &h, body, TS + 99_999, None);
        assert!(r.is_ok());
    }

    #[test]
    fn github_and_stripe_vectors() {
        // GitHub documentation example.
        assert_eq!(
            github_sign("It's a Secret to Everybody", b"Hello, World!"),
            "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
        );
        // Cross-checked with Python hmac/hashlib.
        assert_eq!(
            stripe_sign("whsec_test_secret", 1_700_000_000, br#"{"id":"evt_test"}"#),
            "t=1700000000,v1=13941114bb88ac44a76abcfddea5b92aa6182a4b63d8be3aae908a616083bd7e"
        );
        let mut h = HeaderMap::new();
        h.insert(
            "stripe-signature",
            HeaderValue::from_static("t=1700000000,v1=00,v1=13941114bb88ac44a76abcfddea5b92aa6182a4b63d8be3aae908a616083bd7e"),
        );
        let body = br#"{"id":"evt_test"}"#;
        assert!(
            verify_stripe("whsec_test_secret", &h, body, 1_700_000_100, Some(300))
                .1
                .is_ok()
        );
        let (_, r) = verify_stripe("whsec_test_secret", &h, body, 1_700_000_400, Some(300));
        assert_eq!(r.unwrap_err().code, "timestamp_too_old");
        let mut h = HeaderMap::new();
        h.insert("x-hub-signature-256", HeaderValue::from_static("sha256=00"));
        let (_, r) = verify_github("s", &h, b"x");
        assert_eq!(r.unwrap_err().code, "signature_mismatch");
    }

    #[tokio::test]
    async fn sign_then_verify_roundtrip() {
        let app = module_app(router);
        for scheme in ["standard", "github", "stripe"] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/webhooks/sign?scheme={scheme}"))
                        .body(Body::from(r#"{"a":1}"#))
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            let signed = body_json(resp).await;
            let mut req = Request::builder()
                .method("POST")
                .uri("/webhooks/receive/demo");
            for (k, v) in signed["headers"].as_object().expect("headers") {
                req = req.header(k.as_str(), v.as_str().expect("str"));
            }
            let body = signed["body"].as_str().expect("body").to_string();
            let resp = app
                .clone()
                .oneshot(req.body(Body::from(body.clone())).expect("request"))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::NO_CONTENT, "{scheme}");
            // Tampered body fails.
            let mut req = Request::builder()
                .method("POST")
                .uri("/webhooks/receive/demo");
            for (k, v) in signed["headers"].as_object().expect("headers") {
                req = req.header(k.as_str(), v.as_str().expect("str"));
            }
            let resp = app
                .clone()
                .oneshot(req.body(Body::from(format!("{body} "))).expect("request"))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{scheme}");
            assert_eq!(body_json(resp).await["error"], "signature_mismatch");
        }
    }

    #[tokio::test]
    async fn verify_endpoint_reports_reasons() {
        let app = module_app(router);
        let req = |uri: &str, h: &[(&str, &str)]| {
            let mut b = Request::builder().method("POST").uri(uri);
            for (k, v) in h {
                b = b.header(*k, *v);
            }
            b.body(Body::from(PAYLOAD)).expect("request")
        };
        let ts = TS.to_string();
        let full = [
            ("x-webhook-secret", SECRET),
            ("webhook-id", MSG_ID),
            ("webhook-timestamp", ts.as_str()),
            ("webhook-signature", SIG),
        ];
        let resp = app
            .clone()
            .oneshot(req("/webhooks/verify", &full))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let j = body_json(resp).await;
        assert_eq!(j["error"], "timestamp_too_old");
        assert_eq!(j["signature_valid"], true);
        assert_eq!(j["expected_signature"], SIG);

        let resp = app
            .clone()
            .oneshot(req("/webhooks/verify?ignore_timestamp=true", &full))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["valid"], true);

        let resp = app
            .clone()
            .oneshot(req("/webhooks/verify", &full[..2]))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(resp).await["error"], "missing_headers");

        let resp = app
            .clone()
            .oneshot(req("/webhooks/verify", &full[1..]))
            .await
            .expect("response");
        assert_eq!(body_json(resp).await["error"], "missing_secret");

        let resp = app
            .oneshot(req("/webhooks/receive/nope", &[]))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn query_secret_with_space_for_plus() {
        let a = standard_key("whsec_ab+c").expect("key");
        let b = standard_key("whsec_ab c").expect("key");
        assert_eq!(a, b);
    }
}
