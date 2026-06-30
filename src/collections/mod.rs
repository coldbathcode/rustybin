mod postman;
mod insomnia;
mod curl;
mod bruno;
mod http_file;
mod hurl;
mod k6;
mod har;

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::sync::Arc;

use crate::config::Config;

// ── Shared data model ───────────────────────────────────────────

pub enum BodyDef {
    Json(&'static str),
    Form(&'static [(&'static str, &'static str)]),
    Xml(&'static str),
}

pub enum AuthDef {
    Basic {
        user: &'static str,
        pass: &'static str,
    },
    Bearer(&'static str),
}

pub struct RequestDef {
    pub name: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub headers: &'static [(&'static str, &'static str)],
    pub body: Option<BodyDef>,
    pub auth: Option<AuthDef>,
}

pub struct Category {
    pub name: &'static str,
    pub requests: Vec<RequestDef>,
}

const SAMPLE_JWT: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";

pub fn all_categories() -> Vec<Category> {
    vec![
        Category {
            name: "Echo & Reflection",
            requests: vec![
                RequestDef {
                    name: "GET /echo",
                    method: "GET",
                    path: "/echo",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "POST /echo with JSON body",
                    method: "POST",
                    path: "/echo",
                    headers: &[("Content-Type", "application/json")],
                    body: Some(BodyDef::Json(r#"{"message": "hello", "number": 42}"#)),
                    auth: None,
                },
                RequestDef {
                    name: "GET /anything",
                    method: "GET",
                    path: "/anything",
                    headers: &[],
                    body: None,
                    auth: None,
                },
            ],
        },
        Category {
            name: "Status Codes",
            requests: vec![
                RequestDef {
                    name: "200 OK",
                    method: "GET",
                    path: "/status/200",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "418 I'm a Teapot",
                    method: "GET",
                    path: "/status/418",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "503 Service Unavailable",
                    method: "GET",
                    path: "/status/503",
                    headers: &[],
                    body: None,
                    auth: None,
                },
            ],
        },
        Category {
            name: "Response Shaping",
            requests: vec![
                RequestDef {
                    name: "Delay 500ms",
                    method: "GET",
                    path: "/delay/500",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Cache 60s",
                    method: "GET",
                    path: "/cache/60",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Response headers",
                    method: "GET",
                    path: "/response-headers?X-Custom=hello&X-Trace-Id=abc123",
                    headers: &[],
                    body: None,
                    auth: None,
                },
            ],
        },
        Category {
            name: "Redirects & Cookies",
            requests: vec![
                RequestDef {
                    name: "Redirect chain (3)",
                    method: "GET",
                    path: "/redirect/3",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Get cookies",
                    method: "GET",
                    path: "/cookies",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Set cookies",
                    method: "GET",
                    path: "/cookies/set?session=abc123&theme=dark",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Delete cookies",
                    method: "GET",
                    path: "/cookies/delete?session",
                    headers: &[],
                    body: None,
                    auth: None,
                },
            ],
        },
        Category {
            name: "Info & Random",
            requests: vec![
                RequestDef {
                    name: "Client IP",
                    method: "GET",
                    path: "/ip",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Current date (UTC)",
                    method: "GET",
                    path: "/date",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Current time (UTC)",
                    method: "GET",
                    path: "/time",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "UUID v4",
                    method: "GET",
                    path: "/uuid",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "GUID",
                    method: "GET",
                    path: "/guuid",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Random bundle",
                    method: "GET",
                    path: "/random",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Random int",
                    method: "GET",
                    path: "/random/int",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Lorem Ipsum (3 paragraphs)",
                    method: "GET",
                    path: "/random/lorem-ipsum/3",
                    headers: &[],
                    body: None,
                    auth: None,
                },
            ],
        },
        Category {
            name: "Auth: Basic & API Key",
            requests: vec![
                RequestDef {
                    name: "Basic auth (default)",
                    method: "GET",
                    path: "/auth/basic-auth",
                    headers: &[],
                    body: None,
                    auth: Some(AuthDef::Basic {
                        user: "basic",
                        pass: "password",
                    }),
                },
                RequestDef {
                    name: "Basic auth (custom)",
                    method: "GET",
                    path: "/auth/basic-auth/alice/secret",
                    headers: &[],
                    body: None,
                    auth: Some(AuthDef::Basic {
                        user: "alice",
                        pass: "secret",
                    }),
                },
                RequestDef {
                    name: "API key (default)",
                    method: "GET",
                    path: "/auth/api-key",
                    headers: &[("apikey", "my-key")],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "API key (custom)",
                    method: "GET",
                    path: "/auth/api-key/x-token/s3cret",
                    headers: &[("x-token", "s3cret")],
                    body: None,
                    auth: None,
                },
            ],
        },
        Category {
            name: "Auth: JWT",
            requests: vec![
                RequestDef {
                    name: "Validate JWT",
                    method: "GET",
                    path: "/auth/jwt",
                    headers: &[],
                    body: None,
                    auth: Some(AuthDef::Bearer(SAMPLE_JWT)),
                },
                RequestDef {
                    name: "Exchange JWT",
                    method: "POST",
                    path: "/auth/jwt/exchange",
                    headers: &[],
                    body: None,
                    auth: Some(AuthDef::Bearer(SAMPLE_JWT)),
                },
            ],
        },
        Category {
            name: "Auth: OIDC Provider",
            requests: vec![
                RequestDef {
                    name: "OIDC Discovery",
                    method: "GET",
                    path: "/.well-known/openid-configuration",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Token (client_credentials)",
                    method: "POST",
                    path: "/oauth/token",
                    headers: &[],
                    body: Some(BodyDef::Form(&[
                        ("grant_type", "client_credentials"),
                        ("client_id", "rustybin"),
                        ("client_secret", "secret"),
                    ])),
                    auth: None,
                },
                RequestDef {
                    name: "Token (password grant)",
                    method: "POST",
                    path: "/oauth/token",
                    headers: &[],
                    body: Some(BodyDef::Form(&[
                        ("grant_type", "password"),
                        ("client_id", "rustybin"),
                        ("client_secret", "secret"),
                        ("username", "demo"),
                        ("password", "demo"),
                    ])),
                    auth: None,
                },
                RequestDef {
                    name: "Token Exchange (RFC 8693)",
                    method: "POST",
                    path: "/oauth/token",
                    headers: &[],
                    body: Some(BodyDef::Form(&[
                        ("grant_type", "urn:ietf:params:oauth:grant-type:token-exchange"),
                        ("subject_token", SAMPLE_JWT),
                        ("subject_token_type", "urn:ietf:params:oauth:token-type:access_token"),
                        ("audience", "https://api.example.com"),
                    ])),
                    auth: None,
                },
                RequestDef {
                    name: "JWKS",
                    method: "GET",
                    path: "/oauth/jwks",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "UserInfo",
                    method: "GET",
                    path: "/oauth/userinfo",
                    headers: &[("Authorization", "Bearer <access_token>")],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Introspect token",
                    method: "POST",
                    path: "/oauth/introspect",
                    headers: &[],
                    body: Some(BodyDef::Form(&[
                        ("token", "<access_token>"),
                        ("client_id", "rustybin"),
                        ("client_secret", "secret"),
                    ])),
                    auth: None,
                },
            ],
        },
        Category {
            name: "Auth: mTLS",
            requests: vec![
                RequestDef {
                    name: "Validate client cert",
                    method: "GET",
                    path: "/auth/mtls",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Get client cert",
                    method: "GET",
                    path: "/auth/mtls/get-client-cert",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Get CA cert",
                    method: "GET",
                    path: "/auth/mtls/get-ca-cert",
                    headers: &[],
                    body: None,
                    auth: None,
                },
            ],
        },
        Category {
            name: "AI Gateway (OpenAI-compatible)",
            requests: vec![
                RequestDef {
                    name: "Chat completions",
                    method: "POST",
                    path: "/ai/v1/chat/completions",
                    headers: &[("Content-Type", "application/json")],
                    body: Some(BodyDef::Json(
                        r#"{"model":"rustybin-gpt","messages":[{"role":"user","content":"hello"}]}"#,
                    )),
                    auth: None,
                },
                RequestDef {
                    name: "Chat completions (streaming)",
                    method: "POST",
                    path: "/ai/v1/chat/completions",
                    headers: &[("Content-Type", "application/json")],
                    body: Some(BodyDef::Json(
                        r#"{"model":"rustybin-gpt","messages":[{"role":"user","content":"hello"}],"stream":true}"#,
                    )),
                    auth: None,
                },
                RequestDef {
                    name: "Embeddings",
                    method: "POST",
                    path: "/ai/v1/embeddings",
                    headers: &[("Content-Type", "application/json")],
                    body: Some(BodyDef::Json(
                        r#"{"model":"rustybin-embed","input":"Hello world"}"#,
                    )),
                    auth: None,
                },
                RequestDef {
                    name: "List models",
                    method: "GET",
                    path: "/ai/v1/models",
                    headers: &[],
                    body: None,
                    auth: None,
                },
            ],
        },
        Category {
            name: "GraphQL",
            requests: vec![
                RequestDef {
                    name: "Query users",
                    method: "POST",
                    path: "/graphql",
                    headers: &[("Content-Type", "application/json")],
                    body: Some(BodyDef::Json(
                        r#"{"query":"{ users { id name email } }"}"#,
                    )),
                    auth: None,
                },
                RequestDef {
                    name: "SDL schema",
                    method: "GET",
                    path: "/graphql/schema",
                    headers: &[],
                    body: None,
                    auth: None,
                },
            ],
        },
        Category {
            name: "Orchestration Pipeline",
            requests: vec![
                RequestDef {
                    name: "Step 1: Authenticate",
                    method: "POST",
                    path: "/orchestration/step/1",
                    headers: &[
                        ("Content-Type", "application/json"),
                        ("X-Api-Key", "my-api-key"),
                    ],
                    body: Some(BodyDef::Json(r#"{"merchant_id":"merchant_123"}"#)),
                    auth: None,
                },
                RequestDef {
                    name: "Step 2: Enrich",
                    method: "POST",
                    path: "/orchestration/step/2",
                    headers: &[
                        ("Content-Type", "application/json"),
                        ("X-Correlation-Id", "<from-step-1>"),
                    ],
                    body: Some(BodyDef::Json(
                        r#"{"card_number":"4111111111111111","amount":99.99}"#,
                    )),
                    auth: None,
                },
                RequestDef {
                    name: "Step 3: Validate",
                    method: "POST",
                    path: "/orchestration/step/3",
                    headers: &[
                        ("Content-Type", "application/json"),
                        ("X-Correlation-Id", "<from-step-1>"),
                    ],
                    body: Some(BodyDef::Json(r#"{"amount":99.99,"currency":"USD"}"#)),
                    auth: None,
                },
                RequestDef {
                    name: "Step 4: Process",
                    method: "POST",
                    path: "/orchestration/step/4",
                    headers: &[
                        ("Content-Type", "application/json"),
                        ("X-Correlation-Id", "<from-step-1>"),
                        ("X-Validation-Result", "approved"),
                    ],
                    body: Some(BodyDef::Json(r#"{"amount":99.99,"currency":"USD"}"#)),
                    auth: None,
                },
                RequestDef {
                    name: "Pipeline status",
                    method: "GET",
                    path: "/orchestration/status",
                    headers: &[],
                    body: None,
                    auth: None,
                },
            ],
        },
        Category {
            name: "SOAP / XML",
            requests: vec![
                RequestDef {
                    name: "SOAP GetUser",
                    method: "POST",
                    path: "/soap",
                    headers: &[
                        ("Content-Type", "text/xml"),
                        ("SOAPAction", "GetUser"),
                    ],
                    body: Some(BodyDef::Xml(
                        r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body><GetUser xmlns="http://rustybin.local/users"><userId>123</userId></GetUser></soap:Body></soap:Envelope>"#,
                    )),
                    auth: None,
                },
                RequestDef {
                    name: "WSDL",
                    method: "GET",
                    path: "/soap/wsdl",
                    headers: &[],
                    body: None,
                    auth: None,
                },
            ],
        },
        Category {
            name: "Reliability Testing",
            requests: vec![
                RequestDef {
                    name: "Flaky 50%",
                    method: "GET",
                    path: "/flaky/50",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Flaky pattern SSFSS",
                    method: "GET",
                    path: "/flaky/pattern/SSFSS",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Reset counters",
                    method: "POST",
                    path: "/flaky/reset",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Counter status",
                    method: "GET",
                    path: "/flaky/status",
                    headers: &[],
                    body: None,
                    auth: None,
                },
            ],
        },
        Category {
            name: "Utility",
            requests: vec![
                RequestDef {
                    name: "Health check",
                    method: "GET",
                    path: "/health",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "Identity",
                    method: "GET",
                    path: "/identity",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "OpenAPI JSON",
                    method: "GET",
                    path: "/openapi.json",
                    headers: &[],
                    body: None,
                    auth: None,
                },
                RequestDef {
                    name: "OpenAPI YAML",
                    method: "GET",
                    path: "/openapi.yaml",
                    headers: &[],
                    body: None,
                    auth: None,
                },
            ],
        },
    ]
}

/// Split a path like "/cache/60?ttl=1" into ("/cache/60", "ttl=1").
/// Returns (path, "") if there is no query string.
pub fn split_path_query(path: &str) -> (&str, &str) {
    match path.find('?') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => (path, ""),
    }
}

// ── Handlers ────────────────────────────────────────────────────

async fn postman_handler() -> Response {
    let cats = all_categories();
    let collection = postman::build(&cats);
    json_attachment_response(&collection, "rustybin-postman.json")
}

async fn insomnia_handler() -> Response {
    let cats = all_categories();
    let export = insomnia::build(&cats);
    json_attachment_response(&export, "rustybin-insomnia.json")
}

async fn curl_handler() -> Response {
    let cats = all_categories();
    let script = curl::build(&cats);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("text/x-shellscript")),
            (header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment; filename=\"rustybin-curl.sh\"")),
        ],
        script,
    )
        .into_response()
}

async fn bruno_handler() -> Response {
    let cats = all_categories();
    let collection = bruno::build(&cats);
    json_attachment_response(&collection, "rustybin-bruno.json")
}

async fn http_file_handler() -> Response {
    let cats = all_categories();
    let output = http_file::build(&cats);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8")),
            (header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment; filename=\"rustybin.http\"")),
        ],
        output,
    )
        .into_response()
}

async fn hurl_handler() -> Response {
    let cats = all_categories();
    let output = hurl::build(&cats);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8")),
            (header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment; filename=\"rustybin.hurl\"")),
        ],
        output,
    )
        .into_response()
}

async fn k6_handler() -> Response {
    let cats = all_categories();
    let output = k6::build(&cats);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("application/javascript")),
            (header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment; filename=\"rustybin-k6.js\"")),
        ],
        output,
    )
        .into_response()
}

async fn har_handler() -> Response {
    let cats = all_categories();
    let export = har::build(&cats);
    json_attachment_response(&export, "rustybin.har.json")
}

fn json_attachment_response(value: &serde_json::Value, filename: &str) -> Response {
    match serde_json::to_string_pretty(value) {
        Ok(json) => {
            let disposition = format!("attachment; filename=\"{filename}\"");
            let disposition = HeaderValue::from_str(&disposition).unwrap_or_else(|_| {
                HeaderValue::from_static("attachment")
            });
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, HeaderValue::from_static("application/json")),
                    (header::CONTENT_DISPOSITION, disposition),
                ],
                json,
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to serialize: {e}"),
        )
            .into_response(),
    }
}

// ── Router ──────────────────────────────────────────────────────

pub fn router() -> Router<Arc<Config>> {
    Router::new()
        .route("/export/postman.json", get(postman_handler))
        .route("/export/insomnia.json", get(insomnia_handler))
        .route("/export/curl.sh", get(curl_handler))
        .route("/export/bruno.json", get(bruno_handler))
        .route("/export/requests.http", get(http_file_handler))
        .route("/export/requests.hurl", get(hurl_handler))
        .route("/export/k6.js", get(k6_handler))
        .route("/export/har.json", get(har_handler))
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use serde_json::Value;
    use tower::ServiceExt;

    fn test_config() -> Arc<Config> {
        Arc::new(Config {
            http_port: 80,
            https_port: 443,
            host: "0.0.0.0".to_string(),
            log_level: "info".to_string(),
            trust_forward: false,
            body_limit: 1_048_576,
            instance_id: "test-collections".to_string(),
            tls_cert: "certs/server.crt".to_string(),
            tls_key: "certs/server.key".to_string(),
            mtls_in_header: None,
        })
    }

    fn test_app() -> Router {
        router().with_state(test_config())
    }

    async fn body_string(resp: axum::http::Response<Body>) -> String {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8(body.to_vec()).expect("utf8")
    }

    // ── Shared model tests ──────────────────────────────

    #[test]
    fn all_categories_has_15_groups() {
        let cats = all_categories();
        assert_eq!(cats.len(), 15, "should have 15 categories");
    }

    #[test]
    fn all_categories_has_at_least_55_requests() {
        let cats = all_categories();
        let total: usize = cats.iter().map(|c| c.requests.len()).sum();
        assert!(total >= 55, "should have at least 55 requests, got {total}");
    }

    #[test]
    fn split_path_query_works() {
        assert_eq!(split_path_query("/echo"), ("/echo", ""));
        assert_eq!(
            split_path_query("/response-headers?X-Custom=hello&X-Trace-Id=abc"),
            ("/response-headers", "X-Custom=hello&X-Trace-Id=abc")
        );
    }

    // ── Postman tests ───────────────────────────────────

    #[tokio::test]
    async fn postman_returns_valid_json() {
        let app = test_app();
        let resp = app
            .oneshot(Request::builder().uri("/export/postman.json").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "application/json");
        assert!(resp.headers().get("content-disposition").unwrap().to_str().unwrap().contains("rustybin-postman.json"));
        let body = body_string(resp).await;
        let val: Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(val["info"]["schema"], "https://schema.getpostman.com/json/collection/v2.1.0/collection.json");
        assert_eq!(val["info"]["name"], "Rustybin");
    }

    #[tokio::test]
    async fn postman_has_all_folders() {
        let cats = all_categories();
        let collection = postman::build(&cats);
        let items = collection["item"].as_array().expect("items array");
        assert!(items.len() >= 15);
        let names: Vec<&str> = items.iter().filter_map(|i| i["name"].as_str()).collect();
        assert!(names.contains(&"Echo & Reflection"));
        assert!(names.contains(&"Status Codes"));
        assert!(names.contains(&"Auth: OIDC Provider"));
        assert!(names.contains(&"AI Gateway (OpenAI-compatible)"));
        assert!(names.contains(&"SOAP / XML"));
        assert!(names.contains(&"Reliability Testing"));
    }

    #[tokio::test]
    async fn postman_has_base_url_variable() {
        let cats = all_categories();
        let collection = postman::build(&cats);
        let vars = collection["variable"].as_array().expect("variables");
        let base = vars.iter().find(|v| v["key"] == "base_url");
        assert!(base.is_some());
        assert_eq!(base.unwrap()["value"], "http://localhost");
    }

    #[tokio::test]
    async fn postman_echo_folder_has_requests() {
        let cats = all_categories();
        let collection = postman::build(&cats);
        let items = collection["item"].as_array().unwrap();
        let echo = items.iter().find(|i| i["name"] == "Echo & Reflection").unwrap();
        let reqs = echo["item"].as_array().unwrap();
        assert!(reqs.len() >= 3);
        assert_eq!(reqs[0]["request"]["method"], "GET");
        assert_eq!(reqs[1]["request"]["method"], "POST");
    }

    #[tokio::test]
    async fn postman_basic_auth_has_credentials() {
        let cats = all_categories();
        let collection = postman::build(&cats);
        let items = collection["item"].as_array().unwrap();
        let auth = items.iter().find(|i| i["name"] == "Auth: Basic & API Key").unwrap();
        let reqs = auth["item"].as_array().unwrap();
        assert_eq!(reqs[0]["request"]["auth"]["type"], "basic");
    }

    #[tokio::test]
    async fn postman_urls_use_base_url_variable() {
        let cats = all_categories();
        let collection = postman::build(&cats);
        let items = collection["item"].as_array().unwrap();
        let echo = items.iter().find(|i| i["name"] == "Echo & Reflection").unwrap();
        let reqs = echo["item"].as_array().unwrap();
        let raw_url = reqs[0]["request"]["url"]["raw"].as_str().unwrap();
        assert!(raw_url.contains("{{base_url}}"), "got: {raw_url}");
    }

    // ── Insomnia tests ──────────────────────────────────

    #[tokio::test]
    async fn insomnia_returns_valid_json() {
        let app = test_app();
        let resp = app
            .oneshot(Request::builder().uri("/export/insomnia.json").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "application/json");
        assert!(resp.headers().get("content-disposition").unwrap().to_str().unwrap().contains("rustybin-insomnia.json"));
        let body = body_string(resp).await;
        let val: Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(val["_type"], "export");
        assert_eq!(val["__export_format"], 4);
    }

    #[tokio::test]
    async fn insomnia_has_workspace_and_environment() {
        let cats = all_categories();
        let export = insomnia::build(&cats);
        let resources = export["resources"].as_array().expect("resources");
        let workspace = resources.iter().find(|r| r["_type"] == "workspace");
        assert!(workspace.is_some());
        assert_eq!(workspace.unwrap()["name"], "Rustybin");
        let env = resources.iter().find(|r| r["_type"] == "environment");
        assert!(env.is_some());
        assert_eq!(env.unwrap()["data"]["base_url"], "http://localhost");
    }

    #[tokio::test]
    async fn insomnia_has_all_folders() {
        let cats = all_categories();
        let export = insomnia::build(&cats);
        let resources = export["resources"].as_array().unwrap();
        let folders: Vec<&str> = resources
            .iter()
            .filter(|r| r["_type"] == "request_group")
            .filter_map(|r| r["name"].as_str())
            .collect();
        assert!(folders.len() >= 15);
        assert!(folders.contains(&"Echo & Reflection"));
        assert!(folders.contains(&"Auth: OIDC Provider"));
    }

    #[tokio::test]
    async fn insomnia_has_requests_with_correct_parents() {
        let cats = all_categories();
        let export = insomnia::build(&cats);
        let resources = export["resources"].as_array().unwrap();
        let reqs: Vec<&Value> = resources.iter().filter(|r| r["_type"] == "request").collect();
        assert!(!reqs.is_empty());
        // All requests should have a parentId starting with "fld_"
        for req in &reqs {
            let parent = req["parentId"].as_str().unwrap_or_default();
            assert!(parent.starts_with("fld_"), "request {} has bad parent: {parent}", req["name"]);
        }
    }

    #[tokio::test]
    async fn insomnia_basic_auth_has_credentials() {
        let cats = all_categories();
        let export = insomnia::build(&cats);
        let resources = export["resources"].as_array().unwrap();
        let basic = resources.iter().find(|r| {
            r["_type"] == "request" && r["name"].as_str().unwrap_or_default().contains("Basic auth (default)")
        });
        assert!(basic.is_some());
        let basic = basic.unwrap();
        assert_eq!(basic["authentication"]["type"], "basic");
        assert_eq!(basic["authentication"]["username"], "basic");
    }

    #[tokio::test]
    async fn insomnia_export_source_has_version() {
        let cats = all_categories();
        let export = insomnia::build(&cats);
        let source = export["__export_source"].as_str().unwrap();
        assert!(source.starts_with("rustybin:v"));
        assert!(source.contains(env!("CARGO_PKG_VERSION")));
    }

    #[tokio::test]
    async fn insomnia_urls_use_base_url_variable() {
        let cats = all_categories();
        let export = insomnia::build(&cats);
        let resources = export["resources"].as_array().unwrap();
        let req = resources.iter().find(|r| r["_type"] == "request").unwrap();
        let url = req["url"].as_str().unwrap();
        assert!(url.contains("{{ base_url }}"), "got: {url}");
    }

    // ── cURL tests ──────────────────────────────────────

    #[tokio::test]
    async fn curl_returns_shell_script() {
        let app = test_app();
        let resp = app
            .oneshot(Request::builder().uri("/export/curl.sh").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/x-shellscript");
        assert!(resp.headers().get("content-disposition").unwrap().to_str().unwrap().contains("rustybin-curl.sh"));
        let body = body_string(resp).await;
        assert!(body.starts_with("#!/usr/bin/env bash"));
        assert!(body.contains("BASE_URL"));
        assert!(body.contains("curl"));
    }

    #[tokio::test]
    async fn curl_has_all_categories() {
        let cats = all_categories();
        let script = curl::build(&cats);
        for cat in &cats {
            assert!(script.contains(cat.name), "missing category: {}", cat.name);
        }
    }

    // ── Bruno tests ─────────────────────────────────────

    #[tokio::test]
    async fn bruno_returns_valid_json() {
        let app = test_app();
        let resp = app
            .oneshot(Request::builder().uri("/export/bruno.json").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "application/json");
        let body = body_string(resp).await;
        let val: Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(val["version"], "1");
        assert_eq!(val["type"], "collection");
        assert!(val["items"].as_array().unwrap().len() >= 15);
    }

    // ── .http file tests ────────────────────────────────

    #[tokio::test]
    async fn http_file_returns_text() {
        let app = test_app();
        let resp = app
            .oneshot(Request::builder().uri("/export/requests.http").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get("content-type").unwrap().to_str().unwrap().contains("text/plain"));
        assert!(resp.headers().get("content-disposition").unwrap().to_str().unwrap().contains("rustybin.http"));
        let body = body_string(resp).await;
        assert!(body.contains("@base_url"));
        assert!(body.contains("###"));
        assert!(body.contains("{{base_url}}"));
    }

    // ── Hurl tests ──────────────────────────────────────

    #[tokio::test]
    async fn hurl_returns_text() {
        let app = test_app();
        let resp = app
            .oneshot(Request::builder().uri("/export/requests.hurl").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get("content-type").unwrap().to_str().unwrap().contains("text/plain"));
        assert!(resp.headers().get("content-disposition").unwrap().to_str().unwrap().contains("rustybin.hurl"));
        let body = body_string(resp).await;
        assert!(body.contains("GET "));
        assert!(body.contains("HTTP"));
    }

    // ── k6 tests ────────────────────────────────────────

    #[tokio::test]
    async fn k6_returns_javascript() {
        let app = test_app();
        let resp = app
            .oneshot(Request::builder().uri("/export/k6.js").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "application/javascript");
        assert!(resp.headers().get("content-disposition").unwrap().to_str().unwrap().contains("rustybin-k6.js"));
        let body = body_string(resp).await;
        assert!(body.contains("import http from"));
        assert!(body.contains("__ENV.BASE_URL"));
        assert!(body.contains("group("));
    }


    // ── HAR tests ───────────────────────────────────────

    #[tokio::test]
    async fn har_returns_valid_json() {
        let app = test_app();
        let resp = app
            .oneshot(Request::builder().uri("/export/har.json").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "application/json");
        assert!(resp.headers().get("content-disposition").unwrap().to_str().unwrap().contains("rustybin.har.json"));
        let body = body_string(resp).await;
        let val: Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(val["log"]["version"], "1.2");
        assert!(val["log"]["entries"].as_array().unwrap().len() >= 55);
    }

    #[tokio::test]
    async fn har_has_creator() {
        let cats = all_categories();
        let export = har::build(&cats);
        assert_eq!(export["log"]["creator"]["name"], "Rustybin");
    }
}
