mod bruno;
mod curl;
mod har;
mod http_file;
mod hurl;
mod insomnia;
mod k6;
mod postman;

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use crate::catalog::{self, category, Endpoint, Example, Protocol, RouteCheck};
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

async fn hurl_handler() -> Response {
    let cats = all_categories();
    let output = hurl::build(&cats);
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

async fn k6_handler() -> Response {
    let cats = all_categories();
    let output = k6::build(&cats);
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

async fn har_handler() -> Response {
    let cats = all_categories();
    let export = har::build(&cats);
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

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/export/postman.json",
            &["GET"],
            category::DOCS,
            "Postman collection (v2.1)",
        )
        .example(Example::get("Postman collection", "/export/postman.json")),
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
    ]
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
        let collection = postman::build(&cats);
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
        let collection = postman::build(&cats);
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
        let collection = postman::build(&cats);
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
        let export = insomnia::build(&cats);
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

    #[tokio::test]
    async fn har_has_creator() {
        let cats = all_categories();
        let export = har::build(&cats);
        assert_eq!(export["log"]["creator"]["name"], "Rustybin");
    }
}
