//! Native credential checks for credential-injection demos.
//!
//! Each provider looks for its own credential:
//!
//! | Provider | Credential |
//! |---|---|
//! | OpenAI, Ollama, Cohere | `Authorization: Bearer <key>` |
//! | Azure OpenAI | `api-key: <key>` (or `Authorization: Bearer`) |
//! | Anthropic | `x-api-key: <key>` plus `anthropic-version` (or `Authorization: Bearer`) |
//! | Gemini | `x-goog-api-key: <key>` or `?key=<key>` (or `Authorization: Bearer`) |
//! | Bedrock | SigV4 `Authorization: AWS4-HMAC-SHA256 Credential=AKID/date/region/bedrock/aws4_request, SignedHeaders=..., Signature=<hex>` plus `X-Amz-Date` (structural check only), or a Bedrock API key as `Authorization: Bearer` |
//!
//! Checks are enforced only when required: request header
//! `X-Rustybin-Require-Auth: true`, env `RUSTYBIN_AI_REQUIRE_AUTH=true`, or
//! env `RUSTYBIN_AI_API_KEY=<key>` (which also makes that exact key the only
//! valid one; for SigV4 it is compared with the access key id). The
//! credential that was seen is always reported, redacted to its last four
//! characters, in the `X-Rustybin-Credential` response header.

use axum::http::HeaderMap;
use axum::response::Response;
use std::collections::HashMap;

use super::faults::{error_response, ErrorKind, Provider};

/// Instance-wide auth settings (from the environment).
#[derive(Clone, Debug, Default)]
pub struct AuthSettings {
    pub require: bool,
    pub api_key: Option<String>,
}

impl AuthSettings {
    pub fn from_env() -> Self {
        let api_key = std::env::var("RUSTYBIN_AI_API_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let require = std::env::var("RUSTYBIN_AI_REQUIRE_AUTH")
            .map(|v| is_true(&v))
            .unwrap_or(false);
        Self {
            require: require || api_key.is_some(),
            api_key,
        }
    }
}

pub fn is_true(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// A credential found on the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credential {
    /// Where it came from: `bearer`, `x-api-key`, `api-key`, `x-goog-api-key`,
    /// `query-key`, `sigv4`.
    pub source: &'static str,
    /// The secret (or, for SigV4, the access key id).
    pub value: String,
}

impl Credential {
    /// `source ****last4` (header-safe).
    pub fn redacted(&self) -> String {
        format!("{} {}", self.source, redact(&self.value))
    }
}

/// Keep the last four characters: `****abcd`.
pub fn redact(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() <= 4 {
        return "****".into();
    }
    let tail: String = chars[chars.len() - 4..]
        .iter()
        .map(|c| if c.is_ascii_graphic() { *c } else { '?' })
        .collect();
    format!("****{tail}")
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = header(headers, "authorization")?;
    let (scheme, token) = v.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Parsed SigV4 Authorization header.
#[derive(Debug, PartialEq, Eq)]
pub struct SigV4 {
    pub access_key: String,
    pub date: String,
    pub region: String,
    pub service: String,
    pub signed_headers: Vec<String>,
}

/// Structural SigV4 check (no signature verification).
pub fn parse_sigv4(value: &str) -> Result<SigV4, String> {
    let rest = value
        .strip_prefix("AWS4-HMAC-SHA256 ")
        .ok_or("Authorization header must use the AWS4-HMAC-SHA256 algorithm")?;
    let mut fields: HashMap<&str, &str> = HashMap::new();
    for part in rest.split(',') {
        if let Some((k, v)) = part.trim().split_once('=') {
            fields.insert(k.trim(), v.trim());
        }
    }
    let cred = fields
        .get("Credential")
        .ok_or("Authorization header requires 'Credential' parameter")?;
    let scope: Vec<&str> = cred.split('/').collect();
    if scope.len() != 5 || scope[4] != "aws4_request" || scope[0].is_empty() {
        return Err("Credential should be scoped to a valid region, like 'us-east-1': AKID/yyyymmdd/region/service/aws4_request".into());
    }
    if scope[1].len() != 8 || !scope[1].bytes().all(|b| b.is_ascii_digit()) {
        return Err("Credential date must be yyyymmdd".into());
    }
    let signed = fields
        .get("SignedHeaders")
        .ok_or("Authorization header requires 'SignedHeaders' parameter")?;
    let signed_headers: Vec<String> = signed
        .split(';')
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if !signed_headers.iter().any(|h| h == "host") {
        return Err("'Host' must be a signed header".into());
    }
    let sig = fields
        .get("Signature")
        .ok_or("Authorization header requires 'Signature' parameter")?;
    if sig.len() != 64 || !sig.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Signature must be 64 hex characters".into());
    }
    Ok(SigV4 {
        access_key: scope[0].to_string(),
        date: scope[1].to_string(),
        region: scope[2].to_string(),
        service: scope[3].to_string(),
        signed_headers,
    })
}

/// The credential the provider would look at (no validation).
pub fn find(
    p: Provider,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> Option<Credential> {
    let cred = |source: &'static str, value: String| Some(Credential { source, value });
    match p {
        Provider::Anthropic => {
            if let Some(k) = header(headers, "x-api-key") {
                return cred("x-api-key", k.into());
            }
        }
        Provider::Azure => {
            if let Some(k) = header(headers, "api-key") {
                return cred("api-key", k.into());
            }
        }
        Provider::Gemini => {
            if let Some(k) = header(headers, "x-goog-api-key") {
                return cred("x-goog-api-key", k.into());
            }
            if let Some(k) = query.get("key").filter(|k| !k.is_empty()) {
                return cred("query-key", k.clone());
            }
        }
        Provider::Bedrock => {
            if let Some(v) = header(headers, "authorization") {
                if v.starts_with("AWS4-HMAC-SHA256") {
                    let akid = parse_sigv4(v)
                        .map(|s| s.access_key)
                        .unwrap_or_else(|_| "malformed".into());
                    return cred("sigv4", akid);
                }
            }
        }
        _ => {}
    }
    bearer(headers).and_then(|t| cred("bearer", t))
}

/// Check the credential. `required` is the effective requirement (settings
/// or the `X-Rustybin-Require-Auth` header). Returns the seen credential.
#[allow(clippy::result_large_err)]
pub fn check(
    p: Provider,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    required: bool,
    expected: Option<&str>,
) -> Result<Option<Credential>, Response> {
    let seen = find(p, headers, query);
    if !required {
        return Ok(seen);
    }
    if p == Provider::Bedrock {
        if let Some(v) = header(headers, "authorization") {
            if v.starts_with("AWS4-HMAC-SHA256") {
                if let Err(msg) = parse_sigv4(v) {
                    return Err(bedrock_sig_error(&msg));
                }
                if header(headers, "x-amz-date").is_none() && header(headers, "date").is_none() {
                    return Err(bedrock_sig_error(
                        "Authorization header requires existence of either a 'X-Amz-Date' or a 'Date' header.",
                    ));
                }
            } else if bearer(headers).is_none() {
                return Err(bedrock_sig_error(
                    "Authorization header requires 'Credential' parameter. Authorization header requires 'Signature' parameter.",
                ));
            }
        }
    }
    let Some(c) = seen else {
        return Err(error_response(p, ErrorKind::MissingCredential, None));
    };
    if let Some(exp) = expected {
        if c.value != exp {
            let msg = match p {
                Provider::OpenAi => Some(format!(
                    "Incorrect API key provided: {}. You can find your API key in your account settings.",
                    redact(&c.value)
                )),
                _ => None,
            };
            return Err(error_response(
                p,
                ErrorKind::InvalidCredential,
                msg.as_deref(),
            ));
        }
    }
    if p == Provider::Anthropic && header(headers, "anthropic-version").is_none() {
        return Err(error_response(
            p,
            ErrorKind::BadRequest,
            Some("anthropic-version: header is required"),
        ));
    }
    Ok(Some(c))
}

fn bedrock_sig_error(msg: &str) -> Response {
    let mut r = error_response(Provider::Bedrock, ErrorKind::Forbidden, Some(msg));
    r.headers_mut().insert(
        "x-amzn-errortype",
        axum::http::HeaderValue::from_static("IncompleteSignatureException"),
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn h(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(*k, HeaderValue::from_static(v));
        }
        m
    }

    const SIG: &str = "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/us-east-1/bedrock/aws4_request, SignedHeaders=host;x-amz-date, Signature=fe5f80f77d5fa3beca038a248ff027d0445342fe2855ddc963176630326f1024";

    #[test]
    fn redaction() {
        assert_eq!(redact("sk-test-abcd1234"), "****1234");
        assert_eq!(redact("abc"), "****");
    }

    #[test]
    fn sigv4_structure() {
        let s = parse_sigv4(SIG).expect("valid");
        assert_eq!(s.access_key, "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(s.region, "us-east-1");
        assert!(parse_sigv4("AWS4-HMAC-SHA256 Credential=x").is_err());
        assert!(parse_sigv4("Bearer abc").is_err());
    }

    #[test]
    fn checks_per_provider() {
        let q = HashMap::new();
        // Not required: always ok, credential reported.
        let seen = check(
            Provider::OpenAi,
            &h(&[("authorization", "Bearer sk-1234567")]),
            &q,
            false,
            None,
        )
        .expect("ok");
        assert_eq!(
            seen.map(|c| c.redacted()),
            Some("bearer ****4567".to_string())
        );
        // Required and missing.
        let r = check(Provider::OpenAi, &HeaderMap::new(), &q, true, None).expect_err("401");
        assert_eq!(r.status().as_u16(), 401);
        let r = check(Provider::Anthropic, &HeaderMap::new(), &q, true, None).expect_err("401");
        assert_eq!(r.status().as_u16(), 401);
        let r = check(Provider::Gemini, &HeaderMap::new(), &q, true, None).expect_err("403");
        assert_eq!(r.status().as_u16(), 403);
        // Wrong key.
        let r = check(
            Provider::Azure,
            &h(&[("api-key", "nope")]),
            &q,
            true,
            Some("right"),
        )
        .expect_err("401");
        assert_eq!(r.status().as_u16(), 401);
        // Anthropic needs anthropic-version.
        let r = check(
            Provider::Anthropic,
            &h(&[("x-api-key", "k")]),
            &q,
            true,
            None,
        )
        .expect_err("400");
        assert_eq!(r.status().as_u16(), 400);
        assert!(check(
            Provider::Anthropic,
            &h(&[("x-api-key", "k"), ("anthropic-version", "2023-06-01")]),
            &q,
            true,
            None
        )
        .is_ok());
        // Gemini query key.
        let mut q2 = HashMap::new();
        q2.insert("key".to_string(), "gk-12345".to_string());
        assert!(check(
            Provider::Gemini,
            &HeaderMap::new(),
            &q2,
            true,
            Some("gk-12345")
        )
        .is_ok());
        // Bedrock SigV4.
        assert!(check(
            Provider::Bedrock,
            &h(&[("authorization", SIG), ("x-amz-date", "20260101T000000Z")]),
            &q,
            true,
            None
        )
        .is_ok());
        let r = check(
            Provider::Bedrock,
            &h(&[("authorization", SIG)]),
            &q,
            true,
            None,
        )
        .expect_err("403");
        assert_eq!(
            r.headers()["x-amzn-errortype"],
            "IncompleteSignatureException"
        );
        let r = check(Provider::Bedrock, &HeaderMap::new(), &q, true, None).expect_err("403");
        assert_eq!(
            r.headers()["x-amzn-errortype"],
            "MissingAuthenticationTokenException"
        );
        assert!(check(
            Provider::Bedrock,
            &h(&[("authorization", SIG), ("x-amz-date", "20260101T000000Z")]),
            &q,
            true,
            Some("AKIAIOSFODNN7EXAMPLE")
        )
        .is_ok());
    }
}
