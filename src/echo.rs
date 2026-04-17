use axum::{
    body::Bytes,
    extract::{ConnectInfo, OriginalUri, State},
    http::{HeaderMap, Method},
    response::Response,
    routing::any,
    Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::content_negotiation::negotiate;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EchoResponse {
    pub method: String,
    pub path: String,
    pub path_info: Vec<String>,
    pub query_string: String,
    pub query_params: serde_json::Value,
    pub headers: HashMap<String, Vec<String>>,
    pub host: String,
    pub port: u16,
    pub scheme: String,
    pub remote_ip: String,
    pub body: EchoBody,
    pub timestamp_unix_ms: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EchoBody {
    pub present: bool,
    pub included: bool,
    pub body: Option<String>,
    pub bytes: usize,
    pub truncated: bool,
    pub utf8: Option<bool>,
    pub reason: Option<String>,
}

async fn echo_handler(
    State(config): State<Arc<Config>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let path_info: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    let query_string = uri.query().unwrap_or("").to_string();
    let query_params = parse_query_params(&query_string);

    let header_map = collect_headers(&headers);

    let host = extract_host(&headers);
    let port = extract_port(&headers, &config);
    let scheme = detect_scheme(&headers, &config);
    let remote_ip = detect_remote_ip(&headers, &config, &addr);
    let echo_body = process_body(&body, config.body_limit);

    let timestamp_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let resp = EchoResponse {
        method: method.to_string(),
        path,
        path_info,
        query_string,
        query_params,
        headers: header_map,
        host,
        port,
        scheme,
        remote_ip,
        body: echo_body,
        timestamp_unix_ms,
    };

    negotiate(&headers, &resp)
}

fn parse_query_params(query: &str) -> serde_json::Value {
    if query.is_empty() {
        return serde_json::Value::Object(serde_json::Map::new());
    }

    let mut params: HashMap<String, Vec<String>> = HashMap::new();
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        params
            .entry(key.into_owned())
            .or_default()
            .push(value.into_owned());
    }

    let mut map = serde_json::Map::new();
    for (key, values) in params {
        if values.len() == 1 {
            map.insert(
                key,
                serde_json::Value::String(values.into_iter().next().unwrap_or_default()),
            );
        } else {
            map.insert(
                key,
                serde_json::Value::Array(
                    values.into_iter().map(serde_json::Value::String).collect(),
                ),
            );
        }
    }

    serde_json::Value::Object(map)
}

fn collect_headers(headers: &HeaderMap) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (name, value) in headers.iter() {
        let key = name.to_string();
        let val = value.to_str().unwrap_or("<binary>").to_string();
        map.entry(key).or_default().push(val);
    }
    map
}

fn extract_host(headers: &HeaderMap) -> String {
    headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn extract_port(headers: &HeaderMap, config: &Config) -> u16 {
    headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| {
            let parts: Vec<&str> = h.split(':').collect();
            if parts.len() == 2 {
                parts[1].parse().ok()
            } else {
                None
            }
        })
        .unwrap_or(config.http_port)
}

fn detect_scheme(headers: &HeaderMap, config: &Config) -> String {
    if config.trust_forward {
        if let Some(proto) = headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
        {
            return proto.to_string();
        }
    }
    "http".to_string()
}

fn detect_remote_ip(headers: &HeaderMap, config: &Config, addr: &SocketAddr) -> String {
    if config.trust_forward {
        if let Some(forwarded_for) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
        {
            if let Some(first_ip) = forwarded_for.split(',').next() {
                return first_ip.trim().to_string();
            }
        }
    }
    addr.ip().to_string()
}

fn process_body(raw: &Bytes, limit: usize) -> EchoBody {
    if raw.is_empty() {
        return EchoBody {
            present: false,
            included: false,
            body: None,
            bytes: 0,
            truncated: false,
            utf8: None,
            reason: None,
        };
    }

    let byte_len = raw.len();

    match std::str::from_utf8(raw) {
        Ok(s) => {
            if byte_len > limit {
                let mut end = limit;
                while end > 0 && !s.is_char_boundary(end) {
                    end -= 1;
                }
                EchoBody {
                    present: true,
                    included: true,
                    body: Some(s[..end].to_string()),
                    bytes: byte_len,
                    truncated: true,
                    utf8: Some(true),
                    reason: None,
                }
            } else {
                EchoBody {
                    present: true,
                    included: true,
                    body: Some(s.to_string()),
                    bytes: byte_len,
                    truncated: false,
                    utf8: Some(true),
                    reason: None,
                }
            }
        }
        Err(_) => EchoBody {
            present: true,
            included: false,
            body: None,
            bytes: byte_len,
            truncated: false,
            utf8: Some(false),
            reason: Some("binary".to_string()),
        },
    }
}

pub fn router() -> Router<Arc<Config>> {
    Router::new()
        .route("/echo", any(echo_handler))
        .route("/echo/*path", any(echo_handler))
        .route("/anything", any(echo_handler))
        .route("/anything/*path", any(echo_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
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

    fn echo_request(uri: &str) -> Request<Body> {
        let mut req = Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("valid request");
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))));
        req
    }

    #[tokio::test]
    async fn echo_returns_json() {
        let app = test_app();
        let resp = app.oneshot(echo_request("/echo")).await.expect("response");

        assert_eq!(resp.status(), StatusCode::OK);

        let ct = resp
            .headers()
            .get("content-type")
            .expect("content-type header")
            .to_str()
            .expect("valid str");
        assert_eq!(ct, "application/json");

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid json");

        assert_eq!(json["method"], "GET");
        assert_eq!(json["path"], "/echo");
        assert_eq!(json["remote_ip"], "127.0.0.1");
        assert_eq!(json["scheme"], "http");
    }

    #[tokio::test]
    async fn echo_with_subpath() {
        let app = test_app();
        let resp = app
            .oneshot(echo_request("/echo/foo/bar"))
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid json");

        assert_eq!(json["path"], "/echo/foo/bar");
        assert_eq!(json["path_info"], serde_json::json!(["echo", "foo", "bar"]));
    }

    #[tokio::test]
    async fn anything_alias_works() {
        let app = test_app();
        let resp = app
            .oneshot(echo_request("/anything"))
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid json");

        assert_eq!(json["method"], "GET");
        assert_eq!(json["path"], "/anything");
    }

    #[tokio::test]
    async fn query_params_parsed() {
        let app = test_app();
        let resp = app
            .oneshot(echo_request("/echo?a=1&a=2&b=3"))
            .await
            .expect("response");

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid json");

        assert_eq!(json["query_string"], "a=1&a=2&b=3");
        // a appears twice → array
        let a = &json["query_params"]["a"];
        assert!(a.is_array());
        assert_eq!(a.as_array().expect("array").len(), 2);
        // b appears once → string
        assert_eq!(json["query_params"]["b"], "3");
    }

    #[tokio::test]
    async fn post_with_body() {
        let app = test_app();

        let mut req = Request::builder()
            .method("POST")
            .uri("/echo")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"test":true}"#))
            .expect("valid request");
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))));

        let resp = app.oneshot(req).await.expect("response");

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid json");

        assert_eq!(json["method"], "POST");
        assert_eq!(json["body"]["present"], true);
        assert_eq!(json["body"]["included"], true);
        assert_eq!(json["body"]["body"], r#"{"test":true}"#);
        assert_eq!(json["body"]["utf8"], true);
        assert_eq!(json["body"]["truncated"], false);
    }

    #[tokio::test]
    async fn empty_body_handling() {
        let app = test_app();
        let resp = app.oneshot(echo_request("/echo")).await.expect("response");

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid json");

        assert_eq!(json["body"]["present"], false);
        assert_eq!(json["body"]["included"], false);
        assert!(json["body"]["body"].is_null());
        assert_eq!(json["body"]["bytes"], 0);
    }

    #[tokio::test]
    async fn xml_content_negotiation() {
        let app = test_app();

        let mut req = Request::builder()
            .uri("/echo")
            .header("accept", "application/xml")
            .body(Body::empty())
            .expect("valid request");
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))));

        let resp = app.oneshot(req).await.expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .expect("content-type")
            .to_str()
            .expect("valid str");
        assert_eq!(ct, "application/xml");
    }

    #[tokio::test]
    async fn forwarded_headers_trusted() {
        let config = Arc::new(Config {
            trust_forward: true,
            ..(*test_config()).clone()
        });

        let app = router().with_state(config);

        let mut req = Request::builder()
            .uri("/echo")
            .header("x-forwarded-for", "10.0.0.1, 192.168.1.1")
            .header("x-forwarded-proto", "https")
            .body(Body::empty())
            .expect("valid request");
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))));

        let resp = app.oneshot(req).await.expect("response");

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid json");

        assert_eq!(json["remote_ip"], "10.0.0.1");
        assert_eq!(json["scheme"], "https");
    }

    #[tokio::test]
    async fn body_truncated_when_over_limit() {
        let config = Arc::new(Config {
            body_limit: 10,
            ..(*test_config()).clone()
        });

        let app = router().with_state(config);

        let mut req = Request::builder()
            .method("POST")
            .uri("/echo")
            .body(Body::from("this is a long body that exceeds the limit"))
            .expect("valid request");
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))));

        let resp = app.oneshot(req).await.expect("response");

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid json");

        assert_eq!(json["body"]["present"], true);
        assert_eq!(json["body"]["included"], true);
        assert_eq!(json["body"]["truncated"], true);
        assert_eq!(json["body"]["body"], "this is a ");
        assert_eq!(json["body"]["utf8"], true);
    }
}
