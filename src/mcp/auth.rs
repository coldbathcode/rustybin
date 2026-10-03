//! Authorization for the `/mcp/protected` (OAuth 2.1 bearer, RFC 9728
//! protected resource metadata) and `/mcp/apikey` variants, plus Origin
//! validation shared by every MCP endpoint.

use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::{McpConfig, PROTECTED_PATH};
use crate::jwt_state::JwtState;

/// Scopes advertised in the protected resource metadata.
pub const SCOPES_SUPPORTED: &[&str] = &["mcp:tools", super::tools::WRITE_SCOPE];
/// Scope named in the 401 challenge (basic functionality).
pub const BASE_SCOPE: &str = "mcp:tools";

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Externally visible base URL (`scheme://host[:port]`) of the request, from
/// [`crate::session::request_origin_with`]: `https` on the TLS listener, the
/// `Host` header, and `Forwarded` / `X-Forwarded-*` only when
/// `RUSTYBIN_TRUST_FORWARD` is on (matching the built-in IdP's issuer).
pub fn base_url(
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
    cfg: &McpConfig,
) -> String {
    crate::session::request_origin_with(headers, extensions, uri, cfg.origin_policy()).base_url()
}

/// Canonical resource identifier of the protected MCP endpoint (RFC 8707):
/// `RUSTYBIN_MCP_RESOURCE_URL` when set, else derived from `base`.
pub fn resource_url(base: &str, cfg: &McpConfig) -> String {
    match &cfg.resource_url {
        Some(u) => u.trim_end_matches('/').to_string(),
        None => format!("{base}{PROTECTED_PATH}"),
    }
}

/// URL of the RFC 9728 metadata document for the protected endpoint.
pub fn metadata_url(base: &str) -> String {
    format!("{base}/.well-known/oauth-protected-resource{PROTECTED_PATH}")
}

/// RFC 9728 Protected Resource Metadata for `/mcp/protected`.
/// `base` is the request's [`base_url`].
pub fn protected_resource_metadata(base: &str, cfg: &McpConfig) -> Value {
    json!({
        "resource": resource_url(base, cfg),
        "authorization_servers": [base],
        "scopes_supported": SCOPES_SUPPORTED,
        "bearer_methods_supported": ["header"],
        "resource_name": "Rustybin MCP (protected)",
        // Informational (RFC 9728 allows extra members): where the built-in
        // IdP publishes RFC 8414 metadata and dynamic client registration.
        "rustybin_authorization_server_metadata": format!("{base}/.well-known/oauth-authorization-server"),
        "rustybin_registration_endpoint": format!("{base}/oauth/register"),
        "rustybin_accepted_audiences": accepted_audiences(base, cfg),
        "resource_documentation": format!("{base}/"),
    })
}

fn quote(s: &str) -> String {
    s.chars()
        .filter(|c| *c != '"' && *c != '\\' && !c.is_control())
        .collect()
}

fn challenge(params: &[(&str, &str)]) -> HeaderValue {
    let parts: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", quote(v)))
        .collect();
    HeaderValue::from_str(&format!("Bearer {}", parts.join(", ")))
        .unwrap_or_else(|_| HeaderValue::from_static("Bearer"))
}

/// 401 with the RFC 6750 / RFC 9728 challenge.
pub fn unauthorized(base: &str, error: Option<(&str, &str)>) -> Response {
    let metadata = metadata_url(base);
    let mut params = vec![
        ("resource_metadata", metadata.as_str()),
        ("scope", BASE_SCOPE),
    ];
    if let Some((code, desc)) = error {
        params.insert(0, ("error", code));
        params.push(("error_description", desc));
    }
    let description = error
        .map(|(_, d)| d.to_string())
        .unwrap_or_else(|| "Bearer token required".into());
    let mut resp = (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": error.map(|(c, _)| c).unwrap_or("unauthorized"),
            "error_description": description,
            "resource_metadata": metadata,
        })),
    )
        .into_response();
    resp.headers_mut()
        .insert(header::WWW_AUTHENTICATE, challenge(&params));
    resp
}

/// 403 insufficient_scope (step-up authorization).
pub fn insufficient_scope(base: &str, scope: &str, body: Value) -> Response {
    let metadata = metadata_url(base);
    let desc = format!("This operation requires the {scope} scope");
    let mut resp = (StatusCode::FORBIDDEN, Json(body)).into_response();
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        challenge(&[
            ("error", "insufficient_scope"),
            ("scope", scope),
            ("resource_metadata", &metadata),
            ("error_description", &desc),
        ]),
    );
    resp
}

fn audience_matches(claims: &Value, expected: &[String]) -> bool {
    let norm = |s: &str| s.trim_end_matches('/').to_string();
    let auds: Vec<String> = match claims.get("aud") {
        Some(Value::String(s)) => vec![norm(s)],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(norm).collect(),
        _ => Vec::new(),
    };
    auds.iter().any(|a| expected.iter().any(|e| norm(e) == *a))
}

/// Audiences accepted on `/mcp/protected`: the resource URL first, then the
/// configured extras (by default the IdP's default audience `rustybin`).
pub fn accepted_audiences(base: &str, cfg: &McpConfig) -> Vec<String> {
    let mut expected = vec![resource_url(base, cfg)];
    expected.extend(cfg.extra_audiences.iter().cloned());
    expected
}

/// Validate the bearer token of a request to `/mcp/protected` (`base` is
/// the request's [`base_url`]). Returns the token claims, or the 401
/// response to send.
#[allow(clippy::result_large_err)]
pub fn require_bearer(
    headers: &HeaderMap,
    base: &str,
    cfg: &McpConfig,
    jwt: &JwtState,
) -> Result<Value, Response> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            let (scheme, rest) = v.split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then(|| rest.trim())
        })
        .filter(|t| !t.is_empty());
    let Some(token) = token else {
        return Err(unauthorized(base, None));
    };
    let claims = jwt.verify_rs256(token).map_err(|e| {
        unauthorized(
            base,
            Some(("invalid_token", &format!("token validation failed: {e}"))),
        )
    })?;
    let expected = accepted_audiences(base, cfg);
    if !audience_matches(&claims, &expected) {
        let desc = format!(
            "token audience does not include this resource ({}); request the token with resource={}",
            expected[0], expected[0]
        );
        return Err(unauthorized(base, Some(("invalid_token", &desc))));
    }
    Ok(claims)
}

/// Does the token carry `scope` (space separated `scope` or `scp` array)?
pub fn has_scope(claims: &Value, scope: &str) -> bool {
    let in_str = claims
        .get("scope")
        .and_then(Value::as_str)
        .is_some_and(|s| s.split_whitespace().any(|x| x == scope));
    let in_arr = claims
        .get("scp")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(scope)));
    in_str || in_arr
}

/// `X-API-Key` check for `/mcp/apikey`: any non-empty value, or exactly
/// `RUSTYBIN_MCP_API_KEY` when set.
#[allow(clippy::result_large_err)]
pub fn require_api_key(headers: &HeaderMap, cfg: &McpConfig) -> Result<Value, Response> {
    let presented = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let ok = match (presented, &cfg.api_key) {
        (Some(p), Some(expected)) => constant_time_eq(p.as_bytes(), expected.as_bytes()),
        (Some(_), None) => true,
        (None, _) => false,
    };
    if ok {
        return Ok(json!({ "sub": "api-key-client", "auth": "x-api-key" }));
    }
    let mut resp = (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": "unauthorized",
            "error_description": if presented.is_some() { "invalid X-API-Key" } else { "X-API-Key header required" },
        })),
    )
        .into_response();
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("ApiKey realm=\"rustybin-mcp\", header=\"X-API-Key\""),
    );
    Err(resp)
}

/// Origin validation (DNS rebinding protection). With the default `*` every
/// origin is accepted; otherwise a present Origin must be listed (403).
#[allow(clippy::result_large_err)]
pub fn check_origin(headers: &HeaderMap, cfg: &McpConfig) -> Result<(), Response> {
    if cfg.allowed_origins.iter().any(|o| o == "*") {
        return Ok(());
    }
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    let origin = origin.to_str().unwrap_or("");
    if cfg
        .allowed_origins
        .iter()
        .any(|o| o.trim_end_matches('/').eq_ignore_ascii_case(origin))
    {
        return Ok(());
    }
    Err((
        StatusCode::FORBIDDEN,
        Json(json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": super::protocol::SERVER_ERROR, "message": "Forbidden: Origin not allowed" },
        })),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audience_and_scope_checks() {
        let claims =
            json!({ "aud": ["x", "http://h/mcp/protected/"], "scope": "openid mcp:tools:write" });
        assert!(audience_matches(
            &claims,
            &["http://h/mcp/protected".into()]
        ));
        assert!(!audience_matches(&claims, &["http://other".into()]));
        assert!(has_scope(&claims, "mcp:tools:write"));
        assert!(!has_scope(&claims, "mcp:tools"));
        assert!(has_scope(&json!({ "scp": ["a"] }), "a"));
    }

    #[test]
    fn base_url_respects_trust_forward() {
        let mut cfg = McpConfig::for_tests();
        cfg.http_port = 8080;
        let (ext, uri) = (Extensions::new(), Uri::from_static("/mcp/protected"));
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("example.test:8080"));
        h.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        h.insert("x-forwarded-host", HeaderValue::from_static("gw.example"));
        assert_eq!(base_url(&h, &ext, &uri, &cfg), "http://example.test:8080");
        cfg.trust_forward = true;
        assert_eq!(base_url(&h, &ext, &uri, &cfg), "https://gw.example");
        h.insert(header::HOST, HeaderValue::from_static("bad host\"x"));
        cfg.trust_forward = false;
        assert_eq!(base_url(&h, &ext, &uri, &cfg), "http://127.0.0.1:8080");
    }

    #[test]
    fn base_url_uses_the_tls_listener() {
        let cfg = McpConfig::for_tests();
        let mut ext = Extensions::new();
        ext.insert(crate::session::ListenerInfo::https(8443));
        let uri = Uri::from_static("/mcp/protected");
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("localhost:8443"));
        let base = base_url(&h, &ext, &uri, &cfg);
        assert_eq!(base, "https://localhost:8443");
        assert_eq!(
            resource_url(&base, &cfg),
            "https://localhost:8443/mcp/protected"
        );
        assert_eq!(
            metadata_url(&base),
            "https://localhost:8443/.well-known/oauth-protected-resource/mcp/protected"
        );
        // RUSTYBIN_MCP_RESOURCE_URL still wins.
        let mut fixed = cfg.clone();
        fixed.resource_url = Some("https://mcp.example.com/mcp/protected/".into());
        assert_eq!(
            resource_url(&base, &fixed),
            "https://mcp.example.com/mcp/protected"
        );
    }

    #[test]
    fn challenge_is_well_formed() {
        let v = challenge(&[("error", "insufficient_scope"), ("scope", "a \"b\"")]);
        assert_eq!(
            v.to_str().unwrap_or(""),
            "Bearer error=\"insufficient_scope\", scope=\"a b\""
        );
    }
}
