//! Provider identities, native error shapes, simulated faults, rate-limit
//! headers and streaming pace.
//!
//! Request headers (all optional, values are capped):
//!
//! | Header | Effect |
//! |---|---|
//! | `X-Rustybin-Latency-Ms: N` | wait N ms before the response headers |
//! | `X-Rustybin-TTFT-Ms: N` | time to first token: wait before the first streamed token (added to latency when not streaming) |
//! | `X-Rustybin-Tokens-Per-Second: N` | streaming pace (default 100, 0 = as fast as possible) |
//! | `X-Rustybin-Fail: kind[:percent]` | fail with the provider's native error (kinds below) |
//! | `?fail=kind&fail_rate=0.3` | the same through the query string (`fail_rate` is a 0..1 probability or a percent) |
//!
//! Fail kinds: `429`/`rate_limit`, `500`/`server_error`, `503`/`unavailable`,
//! `529`/`overloaded` (Anthropic 529, other providers 503), `504`/`timeout`,
//! `context_length` (400 context_length_exceeded), `content_filter` (200 with
//! the provider's filtered finish reason), `prompt_filter` (400 prompt blocked),
//! `401`/`auth`, `403`, `404`, `400`/`bad_request`, `413`, and any other
//! status 400..=599.

use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::time::Duration;

/// Upstream API family, chosen by path prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    OpenAi,
    Azure,
    Anthropic,
    Gemini,
    Bedrock,
    Ollama,
    Cohere,
}

impl Provider {
    pub fn from_path(path: &str) -> Provider {
        let rest = path.strip_prefix("/ai/").unwrap_or(path);
        match rest.split('/').next().unwrap_or("") {
            "anthropic" => Provider::Anthropic,
            "gemini" => Provider::Gemini,
            "bedrock" => Provider::Bedrock,
            "azure" => Provider::Azure,
            "ollama" => Provider::Ollama,
            "cohere" => Provider::Cohere,
            _ => Provider::OpenAi,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Provider::OpenAi => "openai",
            Provider::Azure => "azure",
            Provider::Anthropic => "anthropic",
            Provider::Gemini => "gemini",
            Provider::Bedrock => "bedrock",
            Provider::Ollama => "ollama",
            Provider::Cohere => "cohere",
        }
    }
}

/// What went wrong (mapped to each provider's native status and shape).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    BadRequest,
    MissingCredential,
    InvalidCredential,
    Forbidden,
    NotFound,
    TooLarge,
    RateLimit,
    Server,
    Unavailable,
    Overloaded,
    Timeout,
    ContextLength,
    PromptFilter,
    /// Any other status.
    Status(u16),
}

impl ErrorKind {
    fn default_message(&self, p: Provider) -> String {
        match self {
            ErrorKind::BadRequest => "Invalid request.".into(),
            ErrorKind::MissingCredential => match p {
                Provider::Anthropic => "x-api-key header is required".into(),
                Provider::Gemini => "Method doesn't allow unregistered callers (callers without established identity). Please use API Key or other form of API consumer identity to call this API.".into(),
                Provider::Bedrock => "Missing Authentication Token".into(),
                Provider::Azure => "Access denied due to missing subscription key. Make sure to include subscription key when making requests to an API.".into(),
                _ => "You didn't provide an API key. You need to provide your API key in an Authorization header using Bearer auth (i.e. Authorization: Bearer YOUR_KEY).".into(),
            },
            ErrorKind::InvalidCredential => match p {
                Provider::Anthropic => "invalid x-api-key".into(),
                Provider::Gemini => "API key not valid. Please pass a valid API key.".into(),
                Provider::Bedrock => "The security token included in the request is invalid.".into(),
                Provider::Azure => "Access denied due to invalid subscription key or wrong API endpoint. Make sure to provide a valid key for an active subscription and use a correct regional API endpoint for your resource.".into(),
                _ => "Incorrect API key provided.".into(),
            },
            ErrorKind::Forbidden => "You are not allowed to access this resource (simulated).".into(),
            ErrorKind::NotFound => "The requested model or resource does not exist (simulated).".into(),
            ErrorKind::TooLarge => "Request exceeds the maximum allowed size (simulated).".into(),
            ErrorKind::RateLimit => match p {
                Provider::Anthropic => "This request would exceed the rate limit for your organization (simulated). Please retry after a short wait.".into(),
                Provider::Gemini => "Resource has been exhausted (e.g. check quota) (simulated).".into(),
                Provider::Bedrock => "Too many requests, please wait before trying again (simulated).".into(),
                _ => "Rate limit reached for requests (simulated by Rustybin). Please try again in 2s.".into(),
            },
            ErrorKind::Server => "The server had an error while processing your request (simulated).".into(),
            ErrorKind::Unavailable => "The service is temporarily unavailable (simulated). Please retry.".into(),
            ErrorKind::Overloaded => "Overloaded (simulated).".into(),
            ErrorKind::Timeout => "The upstream timed out (simulated).".into(),
            ErrorKind::ContextLength => "This model's maximum context length is 8192 tokens. However, your messages resulted in 9001 tokens (simulated). Please reduce the length of the messages.".into(),
            ErrorKind::PromptFilter => "The prompt was filtered due to triggering the content management policy (simulated).".into(),
            ErrorKind::Status(s) => format!("Simulated upstream error with status {s}."),
        }
    }

    fn status(&self, p: Provider) -> u16 {
        match self {
            ErrorKind::BadRequest | ErrorKind::ContextLength | ErrorKind::PromptFilter => 400,
            ErrorKind::MissingCredential => match p {
                Provider::Gemini | Provider::Bedrock => 403,
                _ => 401,
            },
            ErrorKind::InvalidCredential => match p {
                Provider::Gemini => 400,
                Provider::Bedrock => 403,
                _ => 401,
            },
            ErrorKind::Forbidden => 403,
            ErrorKind::NotFound => 404,
            ErrorKind::TooLarge => 413,
            ErrorKind::RateLimit => 429,
            ErrorKind::Server => 500,
            ErrorKind::Unavailable => 503,
            ErrorKind::Overloaded => match p {
                Provider::Anthropic => 529,
                _ => 503,
            },
            ErrorKind::Timeout => 504,
            ErrorKind::Status(s) => *s,
        }
    }

    fn from_status(code: u16) -> ErrorKind {
        match code {
            400 => ErrorKind::BadRequest,
            401 => ErrorKind::InvalidCredential,
            403 => ErrorKind::Forbidden,
            404 => ErrorKind::NotFound,
            413 => ErrorKind::TooLarge,
            429 => ErrorKind::RateLimit,
            500 => ErrorKind::Server,
            503 => ErrorKind::Unavailable,
            529 => ErrorKind::Overloaded,
            504 => ErrorKind::Timeout,
            other => ErrorKind::Status(other),
        }
    }
}

/// Seconds clients are told to wait after a simulated 429 / 529.
pub const RETRY_AFTER_SECS: u64 = 2;

/// The provider's native error response.
pub fn error_response(p: Provider, kind: ErrorKind, message: Option<&str>) -> Response {
    let message = message
        .map(str::to_string)
        .unwrap_or_else(|| kind.default_message(p));
    let code = kind.status(p);
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = match p {
        Provider::OpenAi | Provider::Azure => {
            let (ty, c, param): (&str, Value, Value) = match &kind {
                ErrorKind::MissingCredential => ("invalid_request_error", Value::Null, Value::Null),
                ErrorKind::InvalidCredential => (
                    "invalid_request_error",
                    json!("invalid_api_key"),
                    Value::Null,
                ),
                ErrorKind::RateLimit => ("requests", json!("rate_limit_exceeded"), Value::Null),
                ErrorKind::ContextLength => (
                    "invalid_request_error",
                    json!("context_length_exceeded"),
                    json!("messages"),
                ),
                ErrorKind::PromptFilter => (
                    "invalid_request_error",
                    json!("content_filter"),
                    json!("prompt"),
                ),
                ErrorKind::NotFound => (
                    "invalid_request_error",
                    json!("model_not_found"),
                    json!("model"),
                ),
                ErrorKind::Forbidden => (
                    "invalid_request_error",
                    json!("unsupported_country_region_territory"),
                    Value::Null,
                ),
                ErrorKind::Server | ErrorKind::Status(500..=599) => {
                    ("server_error", Value::Null, Value::Null)
                }
                ErrorKind::Unavailable | ErrorKind::Overloaded | ErrorKind::Timeout => {
                    ("server_error", json!("service_unavailable"), Value::Null)
                }
                _ => ("invalid_request_error", Value::Null, Value::Null),
            };
            if p == Provider::Azure && matches!(kind, ErrorKind::PromptFilter) {
                json!({"error": {
                    "message": message, "type": null, "param": "prompt", "code": "content_filter", "status": 400,
                    "innererror": {"code": "ResponsibleAIPolicyViolation", "content_filter_result": {
                        "hate": {"filtered": true, "severity": "high"},
                        "self_harm": {"filtered": false, "severity": "safe"},
                        "sexual": {"filtered": false, "severity": "safe"},
                        "violence": {"filtered": false, "severity": "safe"}
                    }}
                }})
            } else {
                json!({"error": {"message": message, "type": ty, "param": param, "code": c}})
            }
        }
        Provider::Anthropic => {
            let ty = match &kind {
                ErrorKind::MissingCredential | ErrorKind::InvalidCredential => {
                    "authentication_error"
                }
                ErrorKind::Forbidden => "permission_error",
                ErrorKind::NotFound => "not_found_error",
                ErrorKind::TooLarge => "request_too_large",
                ErrorKind::RateLimit => "rate_limit_error",
                ErrorKind::Overloaded => "overloaded_error",
                ErrorKind::Server | ErrorKind::Unavailable | ErrorKind::Timeout => "api_error",
                ErrorKind::Status(s) if *s >= 500 => "api_error",
                _ => "invalid_request_error",
            };
            json!({"type": "error", "error": {"type": ty, "message": message}})
        }
        Provider::Gemini => {
            let st = match code {
                400 => "INVALID_ARGUMENT",
                401 => "UNAUTHENTICATED",
                403 => "PERMISSION_DENIED",
                404 => "NOT_FOUND",
                409 => "ABORTED",
                429 => "RESOURCE_EXHAUSTED",
                499 => "CANCELLED",
                501 => "UNIMPLEMENTED",
                503 => "UNAVAILABLE",
                504 => "DEADLINE_EXCEEDED",
                _ if code >= 500 => "INTERNAL",
                _ => "FAILED_PRECONDITION",
            };
            let mut details = Vec::new();
            if matches!(kind, ErrorKind::InvalidCredential) {
                details.push(json!({
                    "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                    "reason": "API_KEY_INVALID",
                    "domain": "googleapis.com",
                    "metadata": {"service": "generativelanguage.googleapis.com"}
                }));
            }
            if matches!(kind, ErrorKind::RateLimit) {
                details.push(json!({
                    "@type": "type.googleapis.com/google.rpc.RetryInfo",
                    "retryDelay": format!("{RETRY_AFTER_SECS}s")
                }));
            }
            json!({"error": {"code": code, "message": message, "status": st, "details": details}})
        }
        Provider::Bedrock => json!({ "message": message }),
        Provider::Ollama => json!({ "error": message }),
        Provider::Cohere => json!({ "id": uuid::Uuid::new_v4().to_string(), "message": message }),
    };
    let mut resp = (status, Json(body)).into_response();
    let h = resp.headers_mut();
    if p == Provider::Bedrock {
        let ty = match &kind {
            ErrorKind::MissingCredential => "MissingAuthenticationTokenException",
            ErrorKind::InvalidCredential => "UnrecognizedClientException",
            ErrorKind::Forbidden => "AccessDeniedException",
            ErrorKind::NotFound => "ResourceNotFoundException",
            ErrorKind::RateLimit => "ThrottlingException",
            ErrorKind::Unavailable | ErrorKind::Overloaded => "ServiceUnavailableException",
            ErrorKind::Timeout => "ModelTimeoutException",
            ErrorKind::Server => "InternalServerException",
            ErrorKind::Status(s) if *s >= 500 => "InternalServerException",
            _ => "ValidationException",
        };
        h.insert("x-amzn-errortype", HeaderValue::from_static(ty));
    }
    if matches!(kind, ErrorKind::RateLimit | ErrorKind::Overloaded) {
        h.insert("retry-after", HeaderValue::from(RETRY_AFTER_SECS));
        match p {
            Provider::OpenAi | Provider::Azure => {
                for (k, v) in [
                    ("x-ratelimit-limit-requests", "10000"),
                    ("x-ratelimit-limit-tokens", "2000000"),
                    ("x-ratelimit-remaining-requests", "0"),
                    ("x-ratelimit-remaining-tokens", "0"),
                    ("x-ratelimit-reset-requests", "2s"),
                    ("x-ratelimit-reset-tokens", "2s"),
                ] {
                    h.insert(k, HeaderValue::from_static(v));
                }
            }
            Provider::Anthropic => {
                let reset = reset_time(RETRY_AFTER_SECS);
                for (k, v) in [
                    ("anthropic-ratelimit-requests-limit", "4000".to_string()),
                    ("anthropic-ratelimit-requests-remaining", "0".to_string()),
                    ("anthropic-ratelimit-requests-reset", reset.clone()),
                    ("anthropic-ratelimit-tokens-limit", "400000".to_string()),
                    ("anthropic-ratelimit-tokens-remaining", "0".to_string()),
                    ("anthropic-ratelimit-tokens-reset", reset),
                ] {
                    if let Ok(v) = HeaderValue::from_str(&v) {
                        h.insert(k, v);
                    }
                }
            }
            _ => {}
        }
    }
    resp
}

fn reset_time(secs: u64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs as i64))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Rate-limit headers on successful responses (consumed `tokens` subtracted).
pub fn success_rate_limit_headers(p: Provider, headers: &mut HeaderMap, tokens: u32) {
    let mut put = |k: &'static str, v: String| {
        if let Ok(v) = HeaderValue::from_str(&v) {
            headers.insert(HeaderName::from_static(k), v);
        }
    };
    match p {
        Provider::OpenAi | Provider::Azure => {
            put("x-ratelimit-limit-requests", "10000".into());
            put("x-ratelimit-limit-tokens", "2000000".into());
            put("x-ratelimit-remaining-requests", "9999".into());
            put(
                "x-ratelimit-remaining-tokens",
                2_000_000u32.saturating_sub(tokens).to_string(),
            );
            put("x-ratelimit-reset-requests", "6ms".into());
            put("x-ratelimit-reset-tokens", "0s".into());
        }
        Provider::Anthropic => {
            let reset = reset_time(60);
            put("anthropic-ratelimit-requests-limit", "4000".into());
            put("anthropic-ratelimit-requests-remaining", "3999".into());
            put("anthropic-ratelimit-requests-reset", reset.clone());
            put("anthropic-ratelimit-tokens-limit", "400000".into());
            put(
                "anthropic-ratelimit-tokens-remaining",
                400_000u32.saturating_sub(tokens).to_string(),
            );
            put("anthropic-ratelimit-tokens-reset", reset.clone());
            put("anthropic-ratelimit-input-tokens-limit", "200000".into());
            put("anthropic-ratelimit-output-tokens-limit", "80000".into());
            put("anthropic-ratelimit-input-tokens-reset", reset.clone());
            put("anthropic-ratelimit-output-tokens-reset", reset);
        }
        _ => {}
    }
}

/// A requested fault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    Error(ErrorKind),
    /// Respond normally but with the provider's content-filter outcome.
    ContentFilter,
}

/// Parse a fail kind (without the `:percent` suffix).
pub fn parse_kind(s: &str) -> Option<Fault> {
    let s = s.trim().to_ascii_lowercase();
    let kind = match s.as_str() {
        "rate_limit" | "ratelimit" | "rate-limit" | "throttle" | "throttling" => {
            ErrorKind::RateLimit
        }
        "server_error" | "server" | "error" | "internal" => ErrorKind::Server,
        "unavailable" | "service_unavailable" => ErrorKind::Unavailable,
        "overloaded" => ErrorKind::Overloaded,
        "timeout" => ErrorKind::Timeout,
        "context_length" | "context_length_exceeded" | "context" => ErrorKind::ContextLength,
        "prompt_filter" => ErrorKind::PromptFilter,
        "auth" | "unauthorized" => ErrorKind::InvalidCredential,
        "forbidden" => ErrorKind::Forbidden,
        "not_found" => ErrorKind::NotFound,
        "bad_request" | "invalid" => ErrorKind::BadRequest,
        "content_filter" | "filter" => return Some(Fault::ContentFilter),
        _ => {
            let code: u16 = s.parse().ok()?;
            if !(400..=599).contains(&code) {
                return None;
            }
            ErrorKind::from_status(code)
        }
    };
    Some(Fault::Error(kind))
}

/// Parse `kind` or `kind:percent` into (fault, percent 0..=100).
pub fn parse_fail(value: &str) -> Option<(Fault, u8)> {
    let (kind, pct) = match value.rsplit_once(':') {
        Some((k, p)) => {
            let p = p.trim().trim_end_matches('%').parse::<u8>().ok()?;
            (k, p.min(100))
        }
        None => (value, 100),
    };
    Some((parse_kind(kind)?, pct))
}

/// Parse `fail_rate` as a probability (0..1) or a percent (1..100).
pub fn parse_rate(value: &str) -> Option<u8> {
    let v: f64 = value.trim().trim_end_matches('%').parse().ok()?;
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    let pct = if v <= 1.0 && !value.contains('%') {
        v * 100.0
    } else {
        v
    };
    Some(pct.round().clamp(0.0, 100.0) as u8)
}

/// Decide whether a fault with `percent` probability fires.
pub fn roll(percent: u8) -> bool {
    use rand::Rng;
    percent >= 100 || (percent > 0 && rand::thread_rng().gen_range(0..100u8) < percent)
}

/// Streaming pace.
#[derive(Clone, Copy, Debug)]
pub struct Pace {
    pub ttft: Duration,
    /// Tokens per second (0 = unpaced).
    pub tps: u32,
    /// Cap on the total pacing time of one stream.
    pub max_total: Duration,
}

impl Pace {
    pub const DEFAULT_TPS: u32 = 100;

    /// Delay between two pieces for a stream of `n` pieces.
    pub fn per_piece(&self, n: usize) -> Duration {
        if self.tps == 0 || n == 0 {
            return Duration::ZERO;
        }
        let per = Duration::from_micros(1_000_000 / u64::from(self.tps));
        let cap = self.max_total / (n as u32).max(1);
        per.min(cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fail_parsing() {
        assert_eq!(
            parse_fail("429"),
            Some((Fault::Error(ErrorKind::RateLimit), 100))
        );
        assert_eq!(
            parse_fail("rate_limit:50"),
            Some((Fault::Error(ErrorKind::RateLimit), 50))
        );
        assert_eq!(
            parse_fail("content_filter"),
            Some((Fault::ContentFilter, 100))
        );
        assert_eq!(
            parse_fail("529"),
            Some((Fault::Error(ErrorKind::Overloaded), 100))
        );
        assert_eq!(parse_fail("200"), None);
        assert_eq!(parse_fail("nonsense"), None);
        assert_eq!(parse_rate("0.3"), Some(30));
        assert_eq!(parse_rate("30"), Some(30));
        assert_eq!(parse_rate("1"), Some(100));
        assert_eq!(parse_rate("-1"), None);
    }

    #[test]
    fn provider_from_path() {
        assert_eq!(
            Provider::from_path("/ai/anthropic/v1/messages"),
            Provider::Anthropic
        );
        assert_eq!(
            Provider::from_path("/ai/v1/chat/completions"),
            Provider::OpenAi
        );
        assert_eq!(
            Provider::from_path("/ai/openai/v1/models"),
            Provider::OpenAi
        );
        assert_eq!(
            Provider::from_path("/ai/bedrock/model/x/converse"),
            Provider::Bedrock
        );
    }

    #[tokio::test]
    async fn native_error_shapes() {
        let r = error_response(Provider::Anthropic, ErrorKind::Overloaded, None);
        assert_eq!(r.status().as_u16(), 529);
        let v = crate::test_support::body_json(r).await;
        assert_eq!(v["type"], "error");
        assert_eq!(v["error"]["type"], "overloaded_error");

        let r = error_response(Provider::OpenAi, ErrorKind::RateLimit, None);
        assert_eq!(r.status().as_u16(), 429);
        assert_eq!(r.headers()["retry-after"], "2");
        assert_eq!(r.headers()["x-ratelimit-remaining-requests"], "0");
        let v = crate::test_support::body_json(r).await;
        assert_eq!(v["error"]["code"], "rate_limit_exceeded");

        let r = error_response(Provider::Gemini, ErrorKind::RateLimit, None);
        let v = crate::test_support::body_json(r).await;
        assert_eq!(v["error"]["status"], "RESOURCE_EXHAUSTED");

        let r = error_response(Provider::Bedrock, ErrorKind::RateLimit, None);
        assert_eq!(r.headers()["x-amzn-errortype"], "ThrottlingException");

        let r = error_response(Provider::OpenAi, ErrorKind::ContextLength, None);
        assert_eq!(r.status().as_u16(), 400);
        let v = crate::test_support::body_json(r).await;
        assert_eq!(v["error"]["code"], "context_length_exceeded");
    }

    #[test]
    fn pace_is_capped() {
        let p = Pace {
            ttft: Duration::ZERO,
            tps: 1,
            max_total: Duration::from_secs(10),
        };
        assert_eq!(p.per_piece(100), Duration::from_millis(100));
        let p = Pace { tps: 0, ..p };
        assert_eq!(p.per_piece(100), Duration::ZERO);
    }
}
