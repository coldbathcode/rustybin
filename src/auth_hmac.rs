//! `/auth/hmac`: HTTP signature validation in the draft-cavage / common
//! gateway hmac-auth format.
//!
//! ```text
//! Authorization: hmac username="alice", algorithm="hmac-sha256",
//!                headers="date request-line digest", signature="<base64>"
//! ```
//!
//! `Signature keyId="alice", ...` (draft-cavage) and `Proxy-Authorization`
//! are accepted too. The signing string is one `name: value` line per listed
//! header (repeated headers joined with `", "`), with the pseudo headers
//! `request-line` (`GET /path?q HTTP/1.1`) and `(request-target)`
//! (`(request-target): get /path?q`). `date` or `x-date` is required and
//! must be within the clock skew; a signed `digest` header is checked
//! against the body.

use axum::{
    body::Bytes,
    extract::{OriginalUri, Path},
    http::{header, HeaderMap, HeaderName, Method, StatusCode, Version},
    response::Response,
    routing::any,
    Router,
};
use base64::Engine;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};
use std::collections::HashMap;

use crate::catalog::{category, Endpoint, Example};
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::state::AppState;
use crate::types::{constant_time_eq, AuthFailure, AuthResponse};

const DEFAULT_USERNAME: &str = "alice";
const DEFAULT_SECRET: &str = "secret";
/// Default allowed difference between the `date`/`x-date` header and now.
const DEFAULT_CLOCK_SKEW_SECS: i64 = 300;
/// Upper bound for `?clock_skew=`.
const MAX_CLOCK_SKEW_SECS: i64 = 7 * 24 * 3600;

// ── Algorithms ──────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HmacAlg {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl HmacAlg {
    fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "hmac-sha1" => Some(Self::Sha1),
            "hmac-sha256" => Some(Self::Sha256),
            "hmac-sha384" => Some(Self::Sha384),
            "hmac-sha512" => Some(Self::Sha512),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "hmac-sha1",
            Self::Sha256 => "hmac-sha256",
            Self::Sha384 => "hmac-sha384",
            Self::Sha512 => "hmac-sha512",
        }
    }

    fn sign(self, key: &[u8], msg: &[u8]) -> Vec<u8> {
        fn mac<M: Mac + hmac::digest::KeyInit>(key: &[u8], msg: &[u8]) -> Vec<u8> {
            // HMAC accepts keys of any length, so this never fails.
            match <M as hmac::digest::KeyInit>::new_from_slice(key) {
                Ok(mut m) => {
                    m.update(msg);
                    m.finalize().into_bytes().to_vec()
                }
                Err(_) => Vec::new(),
            }
        }
        match self {
            Self::Sha1 => mac::<Hmac<Sha1>>(key, msg),
            Self::Sha256 => mac::<Hmac<Sha256>>(key, msg),
            Self::Sha384 => mac::<Hmac<Sha384>>(key, msg),
            Self::Sha512 => mac::<Hmac<Sha512>>(key, msg),
        }
    }
}

// ── Handlers ────────────────────────────────────────────────────────

async fn hmac_default(
    method: Method,
    uri: OriginalUri,
    version: Version,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let req = SignedRequest {
        method: &method,
        uri: &uri.0,
        version,
        headers: &headers,
        body: &body,
    };
    check_hmac(&req, DEFAULT_USERNAME, DEFAULT_SECRET)
}

async fn hmac_custom(
    Path((username, secret)): Path<(String, String)>,
    method: Method,
    uri: OriginalUri,
    version: Version,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let req = SignedRequest {
        method: &method,
        uri: &uri.0,
        version,
        headers: &headers,
        body: &body,
    };
    check_hmac(&req, &username, &secret)
}

struct SignedRequest<'a> {
    method: &'a Method,
    uri: &'a axum::http::Uri,
    version: Version,
    headers: &'a HeaderMap,
    body: &'a [u8],
}

fn unauthorized(headers: &HeaderMap, detail: &str) -> Response {
    let mut resp = negotiate_with_status(
        headers,
        &AuthFailure::new(format!("unauthorized: {detail}")),
        StatusCode::UNAUTHORIZED,
    );
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        header::HeaderValue::from_static("hmac realm=\"rustybin\", headers=\"date request-line\""),
    );
    resp
}

/// The first `Authorization` / `Proxy-Authorization` value using the
/// `hmac` or `Signature` scheme (scheme followed by a space).
fn find_credentials(headers: &HeaderMap) -> Option<(&'static str, &'static str, &str)> {
    for (header_name, label) in [
        (header::AUTHORIZATION, "Authorization"),
        (header::PROXY_AUTHORIZATION, "Proxy-Authorization"),
    ] {
        for value in headers.get_all(&header_name) {
            let Ok(value) = value.to_str() else { continue };
            let Some((scheme, params)) = value.trim().split_once(' ') else {
                continue;
            };
            if scheme.eq_ignore_ascii_case("hmac") {
                return Some((label, "username", params));
            }
            if scheme.eq_ignore_ascii_case("signature") {
                return Some((label, "keyid", params));
            }
        }
    }
    None
}

fn clock_skew(uri: &axum::http::Uri) -> Result<i64, &'static str> {
    let query = uri.query().unwrap_or("");
    match form_urlencoded::parse(query.as_bytes()).find(|(k, _)| k == "clock_skew") {
        None => Ok(DEFAULT_CLOCK_SKEW_SECS),
        Some((_, v)) => v
            .parse::<i64>()
            .ok()
            .filter(|n| (0..=MAX_CLOCK_SKEW_SECS).contains(n))
            .ok_or("clock_skew must be an integer between 0 and 604800 (0 disables the check)"),
    }
}

fn check_hmac(req: &SignedRequest, expected_user: &str, secret: &str) -> Response {
    let headers = req.headers;
    let skew = match clock_skew(req.uri) {
        Ok(s) => s,
        Err(e) => {
            return negotiate_with_status(
                headers,
                &AuthFailure::new(format!("bad_request: {e}")),
                StatusCode::BAD_REQUEST,
            )
        }
    };

    let Some((credential_header, user_param, params_str)) = find_credentials(headers) else {
        return unauthorized(
            headers,
            "missing 'hmac' (or 'Signature') Authorization or Proxy-Authorization header",
        );
    };
    let params = match parse_params(params_str) {
        Ok(p) => p,
        Err(e) => return unauthorized(headers, e),
    };

    let Some(username) = params.get(user_param) else {
        return unauthorized(headers, "missing username (or keyId) parameter");
    };
    if !constant_time_eq(username.as_bytes(), expected_user.as_bytes()) {
        return unauthorized(headers, "unknown username");
    }

    let algorithm = match params.get("algorithm") {
        None => HmacAlg::Sha256,
        Some(a) => match HmacAlg::parse(a) {
            Some(alg) => alg,
            None => {
                return unauthorized(
                    headers,
                    "unsupported algorithm (hmac-sha1, hmac-sha256, hmac-sha384, hmac-sha512)",
                )
            }
        },
    };

    let Some(signature) = params.get("signature") else {
        return unauthorized(headers, "missing signature parameter");
    };

    // Date (or X-Date) is always required; it must be within the skew.
    let Some((date_name, date_value)) = ["date", "x-date"].iter().find_map(|name| {
        headers
            .get(*name)
            .and_then(|v| v.to_str().ok())
            .map(|v| (*name, v))
    }) else {
        return unauthorized(headers, "missing date or x-date header");
    };
    if skew > 0 {
        let Ok(date) = chrono::DateTime::parse_from_rfc2822(date_value.trim()) else {
            return unauthorized(headers, "date header is not a valid HTTP date");
        };
        let diff = (chrono::Utc::now().timestamp() - date.timestamp()).abs();
        if diff > skew {
            return unauthorized(
                headers,
                &format!("{date_name} header outside the allowed clock skew of {skew} s"),
            );
        }
    }

    let signed_headers: Vec<String> = params
        .get("headers")
        .map(|s| s.split_whitespace().map(str::to_ascii_lowercase).collect())
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| vec!["date".to_string()]);

    let signing_string = match build_signing_string(&signed_headers, req) {
        Ok(s) => s,
        Err(missing) => {
            return unauthorized(
                headers,
                &format!("request missing signed header '{missing}'"),
            );
        }
    };

    let digest_verified = signed_headers.iter().any(|h| h == "digest");
    if digest_verified {
        if let Err(e) = verify_digest(headers, req.body) {
            return unauthorized(headers, e);
        }
    }

    let computed = algorithm.sign(secret.as_bytes(), signing_string.as_bytes());
    let provided = base64::engine::general_purpose::STANDARD
        .decode(signature.trim())
        .unwrap_or_default();
    if !constant_time_eq(&computed, &provided) {
        return unauthorized(headers, "signature mismatch");
    }

    let claims = serde_json::json!({
        "algorithm": algorithm.name(),
        "signed_headers": signed_headers.join(" "),
        "signing_string": signing_string,
        "date_header": date_name,
        "clock_skew": skew,
        "digest_verified": digest_verified,
        "credential_header": credential_header,
    });

    negotiate(
        headers,
        &AuthResponse {
            username: Some(username.clone()),
            header: Some(credential_header.to_string()),
            claims: Some(claims),
            ..AuthResponse::ok("hmac")
        },
    )
}

/// Parse `key="value", key2=value2` auth parameters (keys lowercased).
/// Quoted values may contain commas; duplicate keys are rejected.
fn parse_params(s: &str) -> Result<HashMap<String, String>, &'static str> {
    let mut map = HashMap::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let eq = rest.find('=').ok_or("malformed Authorization parameters")?;
        let key = rest[..eq].trim().to_ascii_lowercase();
        if key.is_empty() || key.contains([',', ' ', '"']) {
            return Err("malformed Authorization parameters");
        }
        rest = rest[eq + 1..].trim_start();
        let value;
        if let Some(quoted) = rest.strip_prefix('"') {
            let end = quoted
                .find('"')
                .ok_or("unterminated quoted parameter value")?;
            value = quoted[..end].to_string();
            rest = quoted[end + 1..].trim_start();
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            value = rest[..end].trim().to_string();
            rest = &rest[end..];
        }
        rest = match rest.strip_prefix(',') {
            Some(r) => r.trim_start(),
            None if rest.is_empty() => rest,
            None => return Err("malformed Authorization parameters"),
        };
        if map.insert(key, value).is_some() {
            return Err("duplicate Authorization parameter");
        }
    }
    Ok(map)
}

fn version_str(version: Version) -> &'static str {
    match version {
        Version::HTTP_09 => "HTTP/0.9",
        Version::HTTP_10 => "HTTP/1.0",
        Version::HTTP_2 => "HTTP/2.0",
        Version::HTTP_3 => "HTTP/3.0",
        _ => "HTTP/1.1",
    }
}

/// Build the signing string: one line per listed header, joined by `\n`.
/// Returns `Err(name)` when a listed header is missing (or not visible ASCII).
fn build_signing_string(signed_headers: &[String], req: &SignedRequest) -> Result<String, String> {
    let target = req.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let mut lines = Vec::with_capacity(signed_headers.len());
    for name in signed_headers {
        match name.as_str() {
            "request-line" => lines.push(format!(
                "{} {} {}",
                req.method.as_str(),
                target,
                version_str(req.version)
            )),
            "(request-target)" => lines.push(format!(
                "(request-target): {} {}",
                req.method.as_str().to_ascii_lowercase(),
                target
            )),
            _ => {
                let header_name =
                    HeaderName::from_bytes(name.as_bytes()).map_err(|_| name.clone())?;
                let values: Vec<&str> = req
                    .headers
                    .get_all(&header_name)
                    .iter()
                    .map(|v| v.to_str().map(str::trim))
                    .collect::<Result<_, _>>()
                    .map_err(|_| name.clone())?;
                if values.is_empty() {
                    return Err(name.clone());
                }
                lines.push(format!("{name}: {}", values.join(", ")));
            }
        }
    }
    Ok(lines.join("\n"))
}

/// Check `Digest: SHA-256=<base64>` (or SHA-512) against the body.
fn verify_digest(headers: &HeaderMap, body: &[u8]) -> Result<(), &'static str> {
    let values: Vec<&str> = headers
        .get_all("digest")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut supported = false;
    for entry in values.iter().flat_map(|v| v.split(',')) {
        let Some((alg, value)) = entry.trim().split_once('=') else {
            continue;
        };
        let expected = match alg.to_ascii_lowercase().as_str() {
            "sha-256" => Sha256::digest(body).to_vec(),
            "sha-512" => Sha512::digest(body).to_vec(),
            _ => continue,
        };
        supported = true;
        let provided = b64.decode(value.trim()).unwrap_or_default();
        if !constant_time_eq(&expected, &provided) {
            return Err("body digest mismatch");
        }
    }
    if supported {
        Ok(())
    } else {
        Err("digest header must contain SHA-256=<base64> or SHA-512=<base64>")
    }
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/auth/hmac", any(hmac_default))
        .route("/auth/hmac/{username}/{secret}", any(hmac_custom))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/auth/hmac",
            &["ANY"],
            category::AUTH_HMAC,
            "Validate an hmac-auth / draft-cavage signature (default alice / secret)",
        )
        .description(
            "Authorization (or Proxy-Authorization): hmac username=\"alice\", algorithm=\"hmac-sha256\", \
             headers=\"date request-line\", signature=\"base64(HMAC(secret, signing string))\". \
             `Signature keyId=...` also works. Algorithms hmac-sha1/sha256/sha384/sha512. \
             Date or X-Date required within ?clock_skew= seconds (default 300, 0 disables); \
             a signed digest header (SHA-256=base64) is checked against the body.",
        )
        .example(
            Example::get(
                "HMAC (fixed date, skew check disabled)",
                "/auth/hmac?clock_skew=0",
            )
            .header("Date", "Mon, 02 Jan 2006 15:04:05 GMT")
            .header(
                "Authorization",
                "hmac username=\"alice\", algorithm=\"hmac-sha256\", headers=\"date\", signature=\"nVmVh4q+9UfTQa/fjLD5Wvlyu3yYGgbfnSAhTEW81V4=\"",
            ),
        ),
        Endpoint::new(
            "/auth/hmac/{username}/{secret}",
            &["ANY"],
            category::AUTH_HMAC,
            "HMAC validation with username and secret from the path",
        ),
    ]
}

pub fn openapi_paths() -> serde_json::Value {
    use serde_json::json;
    let ok =
        crate::openapi::json_xml_content(json!({ "$ref": "#/components/schemas/AuthResponse" }));
    let fail =
        crate::openapi::json_xml_content(json!({ "$ref": "#/components/schemas/AuthFailure" }));
    let skew = json!({
        "name": "clock_skew", "in": "query", "required": false,
        "schema": { "type": "integer", "minimum": 0, "maximum": 604800, "default": 300 },
        "description": "Allowed difference in seconds between Date / X-Date and now (0 disables the check)"
    });
    let description = "Validates `Authorization` or `Proxy-Authorization: hmac username=\"...\", algorithm=\"hmac-sha256\", headers=\"date request-line\", signature=\"...\"` (or draft-cavage `Signature keyId=...`). The signature is base64(HMAC(secret, signing string)); the signing string has one `name: value` line per listed header (default `date`; repeated headers joined with \", \"), plus the pseudo headers `request-line` and `(request-target)`. Date or X-Date is required and checked against the clock skew. When `digest` is signed, `Digest: SHA-256=<base64>` must match the body. Supports hmac-sha1/sha256/sha384/sha512. Default credentials: alice / secret.";
    json!({
        "/auth/hmac": { "get": {
            "tags": ["Auth"],
            "summary": "HMAC authentication (default credentials alice / secret)",
            "description": description,
            "operationId": "authHmac",
            "parameters": [skew],
            "responses": {
                "200": { "description": "Signature valid", "content": ok },
                "400": { "description": "Invalid clock_skew", "content": fail },
                "401": { "description": "Missing or invalid signature, date outside the clock skew, or digest mismatch", "content": fail }
            }
        }},
        "/auth/hmac/{username}/{secret}": { "get": {
            "tags": ["Auth"],
            "summary": "HMAC authentication (custom credentials)",
            "description": "Same as /auth/hmac but validates against the username and secret supplied in the path.",
            "operationId": "authHmacCustom",
            "parameters": [
                { "name": "username", "in": "path", "required": true, "schema": { "type": "string" } },
                { "name": "secret", "in": "path", "required": true, "schema": { "type": "string" } },
                skew
            ],
            "responses": {
                "200": { "description": "Signature valid", "content": ok },
                "401": { "description": "Missing or invalid signature", "content": fail }
            }
        }}
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::body_json;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app() -> Router {
        crate::test_support::module_app(router)
    }

    fn now_http_date() -> String {
        chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string()
    }

    fn sign(alg: HmacAlg, secret: &str, signing_string: &str) -> String {
        base64::engine::general_purpose::STANDARD
            .encode(alg.sign(secret.as_bytes(), signing_string.as_bytes()))
    }

    fn auth(user: &str, alg: &str, headers: &str, sig: &str) -> String {
        format!(
            "hmac username=\"{user}\", algorithm=\"{alg}\", headers=\"{headers}\", signature=\"{sig}\""
        )
    }

    async fn send(req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let resp = test_app().oneshot(req).await.expect("response");
        let status = resp.status();
        (status, body_json(resp).await)
    }

    fn date_request(uri: &str, date: &str, authorization: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header("date", date)
            .header("authorization", authorization)
            .body(Body::empty())
            .expect("request")
    }

    #[tokio::test]
    async fn all_algorithms_succeed() {
        for alg in [
            HmacAlg::Sha1,
            HmacAlg::Sha256,
            HmacAlg::Sha384,
            HmacAlg::Sha512,
        ] {
            let date = now_http_date();
            let sig = sign(alg, DEFAULT_SECRET, &format!("date: {date}"));
            let (status, json) = send(date_request(
                "/auth/hmac",
                &date,
                &auth(DEFAULT_USERNAME, alg.name(), "date", &sig),
            ))
            .await;
            assert_eq!(status, StatusCode::OK, "{alg:?} {json}");
            assert_eq!(json["authenticated"], true);
            assert_eq!(json["auth_type"], "hmac");
            assert_eq!(json["claims"]["algorithm"], alg.name());
        }
    }

    #[tokio::test]
    async fn request_line_and_request_target() {
        let date = now_http_date();
        let real = format!(
            "date: {date}\nGET /auth/hmac?x=1 HTTP/1.1\n(request-target): get /auth/hmac?x=1"
        );
        let sig = sign(HmacAlg::Sha256, DEFAULT_SECRET, &real);
        let (status, json) = send(date_request(
            "/auth/hmac?x=1",
            &date,
            &auth(
                DEFAULT_USERNAME,
                "hmac-sha256",
                "date request-line (request-target)",
                &sig,
            ),
        ))
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
    }

    #[tokio::test]
    async fn signature_scheme_and_proxy_authorization() {
        let date = now_http_date();
        let sig = sign(HmacAlg::Sha256, DEFAULT_SECRET, &format!("date: {date}"));
        let req = Request::builder()
            .uri("/auth/hmac")
            .header("date", &date)
            .header(
                "proxy-authorization",
                format!("Signature keyId=\"alice\",algorithm=\"hmac-sha256\",headers=\"date\",signature=\"{sig}\""),
            )
            .body(Body::empty())
            .expect("request");
        let (status, json) = send(req).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["header"], "Proxy-Authorization");
    }

    #[tokio::test]
    async fn repeated_headers_are_joined() {
        let date = now_http_date();
        let sig = sign(
            HmacAlg::Sha256,
            DEFAULT_SECRET,
            &format!("date: {date}\nx-multi: a, b"),
        );
        let req = Request::builder()
            .uri("/auth/hmac")
            .header("date", &date)
            .header("x-multi", "a")
            .header("x-multi", "b")
            .header(
                "authorization",
                auth(DEFAULT_USERNAME, "hmac-sha256", "date x-multi", &sig),
            )
            .body(Body::empty())
            .expect("request");
        let (status, json) = send(req).await;
        assert_eq!(status, StatusCode::OK, "{json}");
    }

    #[tokio::test]
    async fn skew_is_enforced_and_configurable() {
        let old = "Mon, 02 Jan 2006 15:04:05 GMT";
        let sig = sign(HmacAlg::Sha256, DEFAULT_SECRET, &format!("date: {old}"));
        let authz = auth(DEFAULT_USERNAME, "hmac-sha256", "date", &sig);
        let (status, json) = send(date_request("/auth/hmac", old, &authz)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(json["error"].as_str().unwrap_or("").contains("clock skew"));
        let (status, _) = send(date_request("/auth/hmac?clock_skew=0", old, &authz)).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = send(date_request("/auth/hmac?clock_skew=-5", old, &authz)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn x_date_is_accepted() {
        let date = now_http_date();
        let sig = sign(HmacAlg::Sha256, DEFAULT_SECRET, &format!("x-date: {date}"));
        let req = Request::builder()
            .uri("/auth/hmac")
            .header("x-date", &date)
            .header(
                "authorization",
                auth(DEFAULT_USERNAME, "hmac-sha256", "x-date", &sig),
            )
            .body(Body::empty())
            .expect("request");
        let (status, json) = send(req).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["claims"]["date_header"], "x-date");
    }

    fn digest_request(body: &'static str, digest_of: &[u8]) -> Request<Body> {
        let date = now_http_date();
        let digest = format!(
            "SHA-256={}",
            base64::engine::general_purpose::STANDARD.encode(Sha256::digest(digest_of))
        );
        let sig = sign(
            HmacAlg::Sha256,
            DEFAULT_SECRET,
            &format!("date: {date}\ndigest: {digest}"),
        );
        Request::builder()
            .method("POST")
            .uri("/auth/hmac")
            .header("date", &date)
            .header("digest", digest)
            .header(
                "authorization",
                auth(DEFAULT_USERNAME, "hmac-sha256", "date digest", &sig),
            )
            .body(Body::from(body))
            .expect("request")
    }

    #[tokio::test]
    async fn digest_is_verified() {
        let (status, json) = send(digest_request("{\"a\":1}", b"{\"a\":1}")).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["claims"]["digest_verified"], true);
        let (status, json) = send(digest_request("{\"a\":2}", b"{\"a\":1}")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(json["error"]
            .as_str()
            .unwrap_or("")
            .contains("digest mismatch"));
    }

    #[tokio::test]
    async fn missing_signed_header_is_rejected() {
        let date = now_http_date();
        let sig = sign(HmacAlg::Sha256, DEFAULT_SECRET, &format!("date: {date}"));
        let (status, json) = send(date_request(
            "/auth/hmac",
            &date,
            &auth(DEFAULT_USERNAME, "hmac-sha256", "date x-absent", &sig),
        ))
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(json["error"].as_str().unwrap_or("").contains("x-absent"));
    }

    #[tokio::test]
    async fn rejections() {
        let date = now_http_date();
        let good = sign(HmacAlg::Sha256, DEFAULT_SECRET, &format!("date: {date}"));
        let cases = [
            auth(DEFAULT_USERNAME, "hmac-sha256", "date", "bogus"),
            auth("mallory", "hmac-sha256", "date", &good),
            auth(DEFAULT_USERNAME, "hmac-md5", "date", &good),
            // Scheme must be followed by a space.
            format!("hmacusername=\"alice\", signature=\"{good}\""),
            "hmac username=\"alice".to_string(),
        ];
        for authz in cases {
            let (status, _) = send(date_request("/auth/hmac", &date, &authz)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{authz}");
        }
        let (status, _) = send(
            Request::builder()
                .uri("/auth/hmac")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // No date at all.
        let req = Request::builder()
            .uri("/auth/hmac")
            .header(
                "authorization",
                auth(DEFAULT_USERNAME, "hmac-sha256", "date", &good),
            )
            .body(Body::empty())
            .expect("request");
        let (status, _) = send(req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn custom_credentials_succeed() {
        let date = now_http_date();
        let sig = sign(HmacAlg::Sha512, "topsecret", &format!("date: {date}"));
        let (status, _) = send(date_request(
            "/auth/hmac/bob/topsecret",
            &date,
            &auth("bob", "hmac-sha512", "date", &sig),
        ))
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[test]
    fn params_parser() {
        let p =
            parse_params(r#"username="a,b", algorithm=hmac-sha1 ,signature="x=""#).expect("parse");
        assert_eq!(p["username"], "a,b");
        assert_eq!(p["algorithm"], "hmac-sha1");
        assert_eq!(p["signature"], "x=");
        assert!(parse_params(r#"a="1", a="2""#).is_err());
        assert!(parse_params(r#"a="1" b="2""#).is_err());
    }
}
