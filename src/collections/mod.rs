mod bruno;
mod curl;
mod har;
mod http_file;
mod hurl;
mod insomnia;
mod k6;
mod postman;

use std::sync::Arc;

use axum::{
    extract::{FromRef, FromRequestParts},
    http::{header, request::Parts, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;

use crate::catalog::{self, category, Endpoint, Example, Protocol, RouteCheck};
use crate::config::Config;
use crate::session::{request_origin, RequestOrigin};
use crate::state::AppState;

// Shared data model: the exporters consume catalogue examples grouped by
// category. Everything is generated from `crate::catalog::all()`.

pub use crate::catalog::{AuthDef, BodyDef};

/// A request in a collection (a catalogue example).
pub type RequestDef = Example;

/// A folder of requests (one catalogue category).
pub struct Category {
    pub name: &'static str,
    pub requests: Vec<RequestDef>,
}

/// Catalogue examples grouped by category, in display order. WebSocket
/// endpoints and examples marked `RouteCheck::Skip` (never-ending streams,
/// upgrades) are left out because plain HTTP clients cannot run them.
pub fn all_categories() -> Vec<Category> {
    catalog::grouped()
        .into_iter()
        .filter_map(|(name, endpoints)| {
            let requests: Vec<RequestDef> = endpoints
                .iter()
                .filter(|ep| ep.protocol != Protocol::WebSocket)
                .flat_map(|ep| ep.examples.iter())
                .filter(|ex| !matches!(ex.check, RouteCheck::Skip(_)))
                .cloned()
                .collect();
            (!requests.is_empty()).then_some(Category { name, requests })
        })
        .collect()
}

/// Split a path like "/cache/60?ttl=1" into ("/cache/60", "ttl=1").
/// Returns (path, "") if there is no query string.
pub fn split_path_query(path: &str) -> (&str, &str) {
    match path.find('?') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => (path, ""),
    }
}

// ── Base URL ────────────────────────────────────────────────────

/// Longest accepted `?base_url=`.
const MAX_BASE_URL_LEN: usize = 512;

/// The base URL written into an export: `?base_url=` when given (validated),
/// else the origin the request used ([`request_origin`]: listener scheme,
/// `Host`, proxy headers only with `RUSTYBIN_TRUST_FORWARD`).
pub struct ExportBase(pub String);

impl<S> FromRequestParts<S> for ExportBase
where
    Arc<Config>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let requested = parts.uri.query().and_then(|q| {
            form_urlencoded::parse(q.as_bytes())
                .find(|(k, _)| k == "base_url")
                .map(|(_, v)| v.into_owned())
        });
        if let Some(raw) = requested {
            return validate_base_url(&raw).map(ExportBase).map_err(|reason| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": "invalid base_url",
                        "reason": reason,
                        "hint": "use an absolute http(s) URL such as https://gateway.example.com/rustybin",
                    })),
                )
                    .into_response()
            });
        }
        let config = Arc::<Config>::from_ref(state);
        let origin = request_origin(&parts.headers, &parts.extensions, &parts.uri, &config);
        Ok(ExportBase(origin_base_url(&origin)))
    }
}

/// `scheme://authority` of the request, with `ws`/`wss` (a proxy may say
/// so) mapped to `http`/`https`.
fn origin_base_url(origin: &RequestOrigin) -> String {
    let scheme = match origin.scheme.as_str() {
        "https" | "wss" => "https",
        _ => "http",
    };
    let normalized = RequestOrigin {
        scheme: scheme.to_string(),
        host: origin.host.clone(),
        port: origin.port,
    };
    normalized.base_url()
}

/// Validate a `?base_url=` override: an absolute http(s) URL with a host,
/// optionally a path prefix, and no credentials, query or fragment. The
/// value is embedded in shell, JavaScript and Hurl files, so only URL-safe
/// characters are accepted. Returned without a trailing slash.
pub fn validate_base_url(raw: &str) -> Result<String, &'static str> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_BASE_URL_LEN {
        return Err("base_url must be 1 to 512 characters");
    }
    let url = reqwest::Url::parse(raw).map_err(|_| "base_url is not an absolute URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("base_url must use http or https");
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("base_url has no host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("base_url must not contain credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("base_url must not have a query or fragment");
    }
    let out = url.as_str().trim_end_matches('/').to_string();
    let safe = out
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-._~:/[]%".contains(&b));
    if !safe {
        return Err("base_url contains characters that are not allowed");
    }
    Ok(out)
}

// ── Handlers ────────────────────────────────────────────────────

async fn postman_handler(ExportBase(base): ExportBase) -> Response {
    let cats = all_categories();
    let collection = postman::build(&cats, &base);
    json_attachment_response(&collection, "rustybin-postman.json")
}

async fn insomnia_handler(ExportBase(base): ExportBase) -> Response {
    let cats = all_categories();
    let export = insomnia::build(&cats, &base);
    json_attachment_response(&export, "rustybin-insomnia.json")
}

async fn curl_handler(ExportBase(base): ExportBase) -> Response {
    let cats = all_categories();
    let script = curl::build(&cats, &base);
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/x-shellscript"),
            ),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=\"rustybin-curl.sh\""),
            ),
        ],
        script,
    )
        .into_response()
}

async fn bruno_handler(ExportBase(base): ExportBase) -> Response {
    let cats = all_categories();
    let collection = bruno::build(&cats, &base);
    json_attachment_response(&collection, "rustybin-bruno.json")
}

async fn http_file_handler(ExportBase(base): ExportBase) -> Response {
    let cats = all_categories();
    let output = http_file::build(&cats, &base);
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            ),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=\"rustybin.http\""),
            ),
        ],
        output,
    )
        .into_response()
}

async fn hurl_handler(ExportBase(base): ExportBase) -> Response {
    let cats = all_categories();
    let output = hurl::build(&cats, &base);
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            ),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=\"rustybin.hurl\""),
            ),
        ],
        output,
    )
        .into_response()
}

async fn k6_handler(ExportBase(base): ExportBase) -> Response {
    let cats = all_categories();
    let output = k6::build(&cats, &base);
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/javascript"),
            ),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=\"rustybin-k6.js\""),
            ),
        ],
        output,
    )
        .into_response()
}

async fn har_handler(ExportBase(base): ExportBase) -> Response {
    let cats = all_categories();
    let export = har::build(&cats, &base);
    json_attachment_response(&export, "rustybin.har.json")
}

fn json_attachment_response(value: &serde_json::Value, filename: &str) -> Response {
    match serde_json::to_string_pretty(value) {
        Ok(json) => {
            let disposition = format!("attachment; filename=\"{filename}\"");
            let disposition = HeaderValue::from_str(&disposition)
                .unwrap_or_else(|_| HeaderValue::from_static("attachment"));
            (
                StatusCode::OK,
                [
                    (
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    ),
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

/// Shared description of every `/export/*` endpoint.
const EXPORT_DESCRIPTION: &str = "The base URL defaults to the origin the request used (scheme, Host, and Forwarded / X-Forwarded-* with RUSTYBIN_TRUST_FORWARD); override it with ?base_url=https://gateway.example.com/prefix (absolute http(s) URL).";

pub fn catalog() -> Vec<Endpoint> {
    let endpoints = vec![
        Endpoint::new(
            "/export/postman.json",
            &["GET"],
            category::DOCS,
            "Postman collection (v2.1)",
        )
        .example(Example::get("Postman collection", "/export/postman.json"))
        .example(Example::get(
            "Postman collection for a gateway URL",
            "/export/postman.json?base_url=https://gateway.example.com/rustybin",
        )),
        Endpoint::new(
            "/export/insomnia.json",
            &["GET"],
            category::DOCS,
            "Insomnia export (v4)",
        )
        .example(Example::get("Insomnia export", "/export/insomnia.json")),
        Endpoint::new(
            "/export/curl.sh",
            &["GET"],
            category::DOCS,
            "cURL shell script",
        )
        .example(Example::get("cURL script", "/export/curl.sh")),
        Endpoint::new(
            "/export/bruno.json",
            &["GET"],
            category::DOCS,
            "Bruno collection",
        )
        .example(Example::get("Bruno collection", "/export/bruno.json")),
        Endpoint::new(
            "/export/requests.http",
            &["GET"],
            category::DOCS,
            "VS Code / JetBrains .http file",
        )
        .example(Example::get(".http file", "/export/requests.http")),
        Endpoint::new(
            "/export/requests.hurl",
            &["GET"],
            category::DOCS,
            "Hurl file",
        )
        .example(Example::get("Hurl file", "/export/requests.hurl")),
        Endpoint::new(
            "/export/k6.js",
            &["GET"],
            category::DOCS,
            "k6 load-test script",
        )
        .example(Example::get("k6 script", "/export/k6.js")),
        Endpoint::new("/export/har.json", &["GET"], category::DOCS, "HAR archive")
            .example(Example::get("HAR archive", "/export/har.json")),
    ];
    endpoints
        .into_iter()
        .map(|ep| ep.description(EXPORT_DESCRIPTION))
        .collect()
}

pub fn router(_state: &AppState) -> Router<AppState> {
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

    fn test_app() -> Router {
        crate::test_support::module_app(router)
    }

    async fn body_string(resp: axum::http::Response<Body>) -> String {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8(body.to_vec()).expect("utf8")
    }

    // ── Shared model tests ──────────────────────────────

    #[test]
    fn all_categories_has_many_groups() {
        let cats = all_categories();
        assert!(
            cats.len() >= 15,
            "should have at least 15 categories, got {}",
            cats.len()
        );
    }

    #[test]
    fn exports_skip_websocket_examples() {
        let cats = all_categories();
        assert!(cats
            .iter()
            .flat_map(|c| c.requests.iter())
            .all(|r| !r.path.starts_with("/ws")));
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
            .oneshot(
                Request::builder()
                    .uri("/export/postman.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
        assert!(resp
            .headers()
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("rustybin-postman.json"));
        let body = body_string(resp).await;
        let val: Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(
            val["info"]["schema"],
            "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
        );
        assert_eq!(val["info"]["name"], "Rustybin");
    }

    #[tokio::test]
    async fn postman_has_all_folders() {
        let cats = all_categories();
        let collection = postman::build(&cats, "http://localhost");
        let items = collection["item"].as_array().expect("items array");
        assert!(items.len() >= 15);
        let names: Vec<&str> = items.iter().filter_map(|i| i["name"].as_str()).collect();
        assert!(names.contains(&"Echo & Reflection"));
        assert!(names.contains(&"Status Codes"));
        assert!(names.contains(&"Auth: OIDC Provider"));
        assert!(names.contains(&"AI: OpenAI-compatible"));
        assert!(names.contains(&"SOAP / XML"));
        assert!(names.contains(&"Reliability Testing"));
    }

    #[tokio::test]
    async fn postman_has_base_url_variable() {
        let cats = all_categories();
        let collection = postman::build(&cats, "http://localhost");
        let vars = collection["variable"].as_array().expect("variables");
        let base = vars.iter().find(|v| v["key"] == "base_url");
        assert!(base.is_some());
        assert_eq!(base.unwrap()["value"], "http://localhost");
    }

    #[tokio::test]
    async fn postman_echo_folder_has_requests() {
        let cats = all_categories();
        let collection = postman::build(&cats, "http://localhost");
        let items = collection["item"].as_array().unwrap();
        let echo = items
            .iter()
            .find(|i| i["name"] == "Echo & Reflection")
            .unwrap();
        let reqs = echo["item"].as_array().unwrap();
        assert!(reqs.len() >= 3);
        assert_eq!(reqs[0]["request"]["method"], "GET");
        assert_eq!(reqs[1]["request"]["method"], "POST");
    }

    #[tokio::test]
    async fn postman_basic_auth_has_credentials() {
        let cats = all_categories();
        let collection = postman::build(&cats, "http://localhost");
        let items = collection["item"].as_array().unwrap();
        let auth = items
            .iter()
            .find(|i| i["name"] == "Auth: Basic & API Key")
            .unwrap();
        let reqs = auth["item"].as_array().unwrap();
        assert_eq!(reqs[0]["request"]["auth"]["type"], "basic");
    }

    #[tokio::test]
    async fn postman_urls_use_base_url_variable() {
        let cats = all_categories();
        let collection = postman::build(&cats, "http://localhost");
        let items = collection["item"].as_array().unwrap();
        let echo = items
            .iter()
            .find(|i| i["name"] == "Echo & Reflection")
            .unwrap();
        let reqs = echo["item"].as_array().unwrap();
        let raw_url = reqs[0]["request"]["url"]["raw"].as_str().unwrap();
        assert!(raw_url.contains("{{base_url}}"), "got: {raw_url}");
    }

    // ── Insomnia tests ──────────────────────────────────

    #[tokio::test]
    async fn insomnia_returns_valid_json() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/export/insomnia.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
        assert!(resp
            .headers()
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("rustybin-insomnia.json"));
        let body = body_string(resp).await;
        let val: Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(val["_type"], "export");
        assert_eq!(val["__export_format"], 4);
    }

    #[tokio::test]
    async fn insomnia_has_workspace_and_environment() {
        let cats = all_categories();
        let export = insomnia::build(&cats, "http://localhost");
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
        let export = insomnia::build(&cats, "http://localhost");
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
        let export = insomnia::build(&cats, "http://localhost");
        let resources = export["resources"].as_array().unwrap();
        let reqs: Vec<&Value> = resources
            .iter()
            .filter(|r| r["_type"] == "request")
            .collect();
        assert!(!reqs.is_empty());
        // All requests should have a parentId starting with "fld_"
        for req in &reqs {
            let parent = req["parentId"].as_str().unwrap_or_default();
            assert!(
                parent.starts_with("fld_"),
                "request {} has bad parent: {parent}",
                req["name"]
            );
        }
    }

    #[tokio::test]
    async fn insomnia_basic_auth_has_credentials() {
        let cats = all_categories();
        let export = insomnia::build(&cats, "http://localhost");
        let resources = export["resources"].as_array().unwrap();
        let basic = resources.iter().find(|r| {
            r["_type"] == "request"
                && r["name"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("Basic auth (default)")
        });
        assert!(basic.is_some());
        let basic = basic.unwrap();
        assert_eq!(basic["authentication"]["type"], "basic");
        assert_eq!(basic["authentication"]["username"], "basic");
    }

    #[tokio::test]
    async fn insomnia_export_source_has_version() {
        let cats = all_categories();
        let export = insomnia::build(&cats, "http://localhost");
        let source = export["__export_source"].as_str().unwrap();
        assert!(source.starts_with("rustybin:v"));
        assert!(source.contains(env!("CARGO_PKG_VERSION")));
    }

    #[tokio::test]
    async fn insomnia_urls_use_base_url_variable() {
        let cats = all_categories();
        let export = insomnia::build(&cats, "http://localhost");
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
            .oneshot(
                Request::builder()
                    .uri("/export/curl.sh")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/x-shellscript"
        );
        assert!(resp
            .headers()
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("rustybin-curl.sh"));
        let body = body_string(resp).await;
        assert!(body.starts_with("#!/usr/bin/env bash"));
        assert!(body.contains("BASE_URL"));
        assert!(body.contains("curl"));
    }

    #[tokio::test]
    async fn curl_has_all_categories() {
        let cats = all_categories();
        let script = curl::build(&cats, "http://localhost");
        for cat in &cats {
            assert!(script.contains(cat.name), "missing category: {}", cat.name);
        }
    }

    // ── Bruno tests ─────────────────────────────────────

    #[tokio::test]
    async fn bruno_returns_valid_json() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/export/bruno.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
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
            .oneshot(
                Request::builder()
                    .uri("/export/requests.http")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("text/plain"));
        assert!(resp
            .headers()
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("rustybin.http"));
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
            .oneshot(
                Request::builder()
                    .uri("/export/requests.hurl")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("text/plain"));
        assert!(resp
            .headers()
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("rustybin.hurl"));
        let body = body_string(resp).await;
        assert!(body.contains("GET "));
        assert!(body.contains("HTTP"));
    }

    // ── k6 tests ────────────────────────────────────────

    #[tokio::test]
    async fn k6_returns_javascript() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/export/k6.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/javascript"
        );
        assert!(resp
            .headers()
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("rustybin-k6.js"));
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
            .oneshot(
                Request::builder()
                    .uri("/export/har.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
        assert!(resp
            .headers()
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("rustybin.har.json"));
        let body = body_string(resp).await;
        let val: Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(val["log"]["version"], "1.2");
        assert!(val["log"]["entries"].as_array().unwrap().len() >= 55);
    }

    // ── Base URL ────────────────────────────────────────

    async fn get(app: &Router, uri: &str, host: &str, https: bool) -> (StatusCode, String) {
        let mut req = Request::builder()
            .uri(uri)
            .header("host", host)
            .body(Body::empty())
            .expect("request");
        if https {
            req.extensions_mut()
                .insert(crate::session::ListenerInfo::https(8443));
        }
        let resp = app.clone().oneshot(req).await.expect("response");
        let status = resp.status();
        (status, body_string(resp).await)
    }

    #[tokio::test]
    async fn exports_default_to_the_request_origin() {
        let app = test_app();
        let (status, body) = get(&app, "/export/postman.json", "rb.test:8080", false).await;
        assert_eq!(status, StatusCode::OK);
        let val: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(val["variable"][0]["value"], "http://rb.test:8080");
        let (_, body) = get(&app, "/export/insomnia.json", "localhost:8443", true).await;
        let val: Value = serde_json::from_str(&body).expect("json");
        let env = val["resources"]
            .as_array()
            .and_then(|r| r.iter().find(|r| r["_type"] == "environment"))
            .cloned()
            .unwrap_or_default();
        assert_eq!(env["data"]["base_url"], "https://localhost:8443");
        let (_, body) = get(&app, "/export/har.json", "localhost:8443", true).await;
        let val: Value = serde_json::from_str(&body).expect("json");
        let url = val["log"]["entries"][0]["request"]["url"]
            .as_str()
            .unwrap_or_default();
        assert!(url.starts_with("https://localhost:8443/"), "got: {url}");
        let (_, body) = get(&app, "/export/curl.sh", "rb.test", false).await;
        assert!(body.contains("BASE_URL=\"${BASE_URL:-http://rb.test}\""));
        let (_, body) = get(&app, "/export/k6.js", "rb.test", false).await;
        assert!(body.contains("__ENV.BASE_URL || 'http://rb.test'"));
        let (_, body) = get(&app, "/export/requests.http", "rb.test", false).await;
        assert!(body.contains("@base_url = http://rb.test\n"));
        let (_, body) = get(&app, "/export/requests.hurl", "rb.test", false).await;
        assert!(body.contains("--variable base_url=http://rb.test "));
        let (_, body) = get(&app, "/export/bruno.json", "rb.test", false).await;
        assert!(body.contains("\"http://rb.test\""));
    }

    #[tokio::test]
    async fn base_url_query_overrides_and_is_validated() {
        let app = test_app();
        let (status, body) = get(
            &app,
            "/export/postman.json?base_url=https%3A%2F%2Fgw.example.com%2Frustybin%2F",
            "rb.test",
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let val: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(
            val["variable"][0]["value"],
            "https://gw.example.com/rustybin"
        );
        for bad in [
            "ftp://gw.example.com",
            "/relative",
            "https://user:pw@gw.example.com",
            "https://gw.example.com/?a=1",
            "https://gw.example.com/a'b",
            "https://gw.example.com/$(id)",
            "",
        ] {
            let q: String = form_urlencoded::byte_serialize(bad.as_bytes()).collect();
            let (status, body) = get(
                &app,
                &format!("/export/curl.sh?base_url={q}"),
                "rb.test",
                false,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
            assert!(body.contains("invalid base_url"));
        }
    }

    #[test]
    fn validate_base_url_normalizes() {
        assert_eq!(
            validate_base_url(" HTTP://Example.COM:8080/ ").as_deref(),
            Ok("http://example.com:8080")
        );
        assert_eq!(
            validate_base_url("https://[::1]:8443/gw").as_deref(),
            Ok("https://[::1]:8443/gw")
        );
        assert!(validate_base_url("javascript:alert(1)").is_err());
    }

    #[tokio::test]
    async fn har_has_creator() {
        let cats = all_categories();
        let export = har::build(&cats, "http://localhost");
        assert_eq!(export["log"]["creator"]["name"], "Rustybin");
    }
}
