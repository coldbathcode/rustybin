use axum::{
    extract::{OriginalUri, Path},
    http::{HeaderMap, Method, StatusCode},
    response::Response,
    routing::any,
    Router,
};
use base64::Engine;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Sha256, Sha384, Sha512};
use std::sync::Arc;

use crate::config::Config;
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::types::{AuthFailure, AuthResponse};

const DEFAULT_USERNAME: &str = "alice";
const DEFAULT_SECRET: &str = "secret";

// ── Handlers ────────────────────────────────────────────────────────

async fn hmac_default(method: Method, uri: OriginalUri, headers: HeaderMap) -> Response {
    check_hmac(&method, &uri, &headers, DEFAULT_USERNAME, DEFAULT_SECRET)
}

async fn hmac_custom(
    Path((username, secret)): Path<(String, String)>,
    method: Method,
    uri: OriginalUri,
    headers: HeaderMap,
) -> Response {
    check_hmac(&method, &uri, &headers, &username, &secret)
}

fn unauthorized(headers: &HeaderMap, detail: &str) -> Response {
    negotiate_with_status(
        headers,
        &AuthFailure {
            authenticated: false,
            error: format!("unauthorized: {detail}"),
        },
        StatusCode::UNAUTHORIZED,
    )
}

fn check_hmac(
    method: &Method,
    uri: &OriginalUri,
    headers: &HeaderMap,
    expected_user: &str,
    secret: &str,
) -> Response {
    // Extract the `Authorization: hmac ...` header.
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let Some(auth) = auth else {
        return unauthorized(headers, "missing Authorization header");
    };

    let trimmed = auth.trim();
    // Scheme is case-insensitive per RFC 7235.
    let Some(params_str) = trimmed
        .get(..4)
        .filter(|p| p.eq_ignore_ascii_case("hmac"))
        .map(|_| trimmed[4..].trim())
    else {
        return unauthorized(headers, "Authorization scheme is not 'hmac'");
    };

    let params = parse_params(params_str);

    let Some(username) = params.get("username") else {
        return unauthorized(headers, "missing username in Authorization");
    };
    if username != expected_user {
        return unauthorized(headers, "unknown username");
    }

    let algorithm = params
        .get("algorithm")
        .map(|s| s.as_str())
        .unwrap_or("hmac-sha256");

    let Some(signature) = params.get("signature") else {
        return unauthorized(headers, "missing signature in Authorization");
    };

    // `headers` lists which request headers (space-separated, lowercase) form
    // the signing string, in order. Defaults to `date` when omitted.
    let signed_headers = params
        .get("headers")
        .map(|s| s.as_str())
        .unwrap_or("date");

    let signing_string = match build_signing_string(signed_headers, method, uri, headers) {
        Ok(s) => s,
        Err(missing) => {
            return unauthorized(headers, &format!("request missing signed header '{missing}'"));
        }
    };

    let computed = match compute_hmac(algorithm, secret.as_bytes(), signing_string.as_bytes()) {
        Some(sig) => sig,
        None => return unauthorized(headers, "unsupported algorithm"),
    };

    if !constant_time_eq(computed.as_bytes(), signature.as_bytes()) {
        return unauthorized(headers, "signature mismatch");
    }

    let mut claims = serde_json::Map::new();
    claims.insert("algorithm".to_string(), algorithm.into());
    claims.insert("signed_headers".to_string(), signed_headers.into());
    claims.insert("signing_string".to_string(), signing_string.into());

    negotiate(
        headers,
        &AuthResponse {
            authenticated: true,
            auth_type: "hmac".to_string(),
            username: Some(username.clone()),
            header: Some("Authorization".to_string()),
            claims: Some(serde_json::Value::Object(claims)),
            jwt_header: None,
            client_dn: None,
            client_ca: None,
        },
    )
}

/// Parse `key="value", key2="value2"` (also tolerates unquoted values).
fn parse_params(s: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for part in s.split(',') {
        let part = part.trim();
        if let Some((k, v)) = part.split_once('=') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().trim_matches('"').to_string();
            map.insert(key, val);
        }
    }
    map
}

/// Build the signing string from the listed header names, in order, joined by
/// `\n`. The pseudo-header `request-line` expands to `"<METHOD> <path> HTTP/1.1"`
/// (matching common gateway hmac-auth behaviour). Returns Err(name) if a listed header
/// is not present on the request.
fn build_signing_string(
    signed_headers: &str,
    method: &Method,
    uri: &OriginalUri,
    headers: &HeaderMap,
) -> Result<String, String> {
    let mut lines = Vec::new();
    for name in signed_headers.split_whitespace() {
        let lname = name.to_ascii_lowercase();
        if lname == "request-line" {
            let path = uri.0.path_and_query().map(|p| p.as_str()).unwrap_or("/");
            lines.push(format!("{} {} HTTP/1.1", method.as_str(), path));
        } else {
            let value = headers
                .get(&lname)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| lname.clone())?;
            lines.push(format!("{lname}: {value}"));
        }
    }
    Ok(lines.join("\n"))
}

fn compute_hmac(algorithm: &str, key: &[u8], msg: &[u8]) -> Option<String> {
    let engine = base64::engine::general_purpose::STANDARD;
    let sig = match algorithm.to_ascii_lowercase().as_str() {
        "hmac-sha1" => {
            let mut mac = Hmac::<Sha1>::new_from_slice(key).ok()?;
            mac.update(msg);
            engine.encode(mac.finalize().into_bytes())
        }
        "hmac-sha256" => {
            let mut mac = Hmac::<Sha256>::new_from_slice(key).ok()?;
            mac.update(msg);
            engine.encode(mac.finalize().into_bytes())
        }
        "hmac-sha384" => {
            let mut mac = Hmac::<Sha384>::new_from_slice(key).ok()?;
            mac.update(msg);
            engine.encode(mac.finalize().into_bytes())
        }
        "hmac-sha512" => {
            let mut mac = Hmac::<Sha512>::new_from_slice(key).ok()?;
            mac.update(msg);
            engine.encode(mac.finalize().into_bytes())
        }
        _ => return None,
    };
    Some(sig)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router() -> Router<Arc<Config>> {
    Router::new()
        .route("/auth/hmac", any(hmac_default))
        .route("/auth/hmac/:username/:secret", any(hmac_custom))
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

    fn sign(algorithm: &str, secret: &str, signing_string: &str) -> String {
        compute_hmac(algorithm, secret.as_bytes(), signing_string.as_bytes()).unwrap()
    }

    #[tokio::test]
    async fn valid_date_signature_succeeds() {
        let date = "Mon, 02 Jan 2006 15:04:05 GMT";
        let signing_string = format!("date: {date}");
        let sig = sign("hmac-sha256", DEFAULT_SECRET, &signing_string);
        let auth = format!(
            "hmac username=\"{DEFAULT_USERNAME}\", algorithm=\"hmac-sha256\", headers=\"date\", signature=\"{sig}\""
        );

        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/auth/hmac")
                    .header("date", date)
                    .header("authorization", auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["authenticated"], true);
        assert_eq!(json["auth_type"], "hmac");
        assert_eq!(json["username"], DEFAULT_USERNAME);
    }

    #[tokio::test]
    async fn request_line_signature_succeeds() {
        let date = "Mon, 02 Jan 2006 15:04:05 GMT";
        let signing_string = format!("date: {date}\nrequest-line: not-used");
        // Build the real signing string the server will compute.
        let real = format!("date: {date}\nGET /auth/hmac HTTP/1.1");
        let _ = signing_string;
        let sig = sign("hmac-sha256", DEFAULT_SECRET, &real);
        let auth = format!(
            "hmac username=\"{DEFAULT_USERNAME}\", algorithm=\"hmac-sha256\", headers=\"date request-line\", signature=\"{sig}\""
        );

        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/auth/hmac")
                    .header("date", date)
                    .header("authorization", auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn bad_signature_returns_401() {
        let auth = format!(
            "hmac username=\"{DEFAULT_USERNAME}\", algorithm=\"hmac-sha256\", headers=\"date\", signature=\"bogus\""
        );
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/auth/hmac")
                    .header("date", "Mon, 02 Jan 2006 15:04:05 GMT")
                    .header("authorization", auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn missing_authorization_returns_401() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/auth/hmac")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn custom_credentials_succeed() {
        let date = "Tue, 03 Jan 2006 15:04:05 GMT";
        let signing_string = format!("date: {date}");
        let sig = sign("hmac-sha256", "topsecret", &signing_string);
        let auth = format!(
            "hmac username=\"bob\", algorithm=\"hmac-sha256\", headers=\"date\", signature=\"{sig}\""
        );

        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/auth/hmac/bob/topsecret")
                    .header("date", date)
                    .header("authorization", auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn wrong_username_returns_401() {
        let date = "Tue, 03 Jan 2006 15:04:05 GMT";
        let sig = sign("hmac-sha256", DEFAULT_SECRET, &format!("date: {date}"));
        let auth = format!(
            "hmac username=\"mallory\", algorithm=\"hmac-sha256\", headers=\"date\", signature=\"{sig}\""
        );
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/auth/hmac")
                    .header("date", date)
                    .header("authorization", auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
