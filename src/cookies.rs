use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::content_negotiation::negotiate;
use crate::state::AppState;

// ── /cookies ─────────────────────────────────────────────────────────

#[derive(Serialize)]
struct CookiesResponse {
    cookies: HashMap<String, String>,
}

async fn cookies_handler(State(_config): State<Arc<Config>>, headers: HeaderMap) -> Response {
    let cookies = parse_cookies(&headers);
    negotiate(&headers, &CookiesResponse { cookies })
}

// ── /cookies/set ─────────────────────────────────────────────────────

async fn cookies_set_handler(uri: Uri) -> Response {
    let query = uri.query().unwrap_or("");
    let params: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();

    // Separate cookie attributes (prefixed with _) from cookie values
    let mut attrs = CookieAttrs::default();
    let mut cookie_pairs: Vec<(String, String)> = Vec::new();

    for (key, value) in &params {
        match key.as_str() {
            "_path" => attrs.path = Some(value.clone()),
            "_domain" => attrs.domain = Some(value.clone()),
            "_secure" => attrs.secure = value == "true",
            "_httponly" => attrs.httponly = value == "true",
            "_samesite" => attrs.samesite = Some(value.clone()),
            "_maxage" => attrs.max_age = value.parse().ok(),
            _ => cookie_pairs.push((key.clone(), value.clone())),
        }
    }

    let mut resp = StatusCode::FOUND.into_response();
    resp.headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static("/cookies"));

    for (name, value) in &cookie_pairs {
        let cookie_str = build_set_cookie(name, value, &attrs);
        if let Ok(val) = HeaderValue::from_str(&cookie_str) {
            resp.headers_mut().append(header::SET_COOKIE, val);
        }
    }

    resp
}

// ── /cookies/set/:name/:value ────────────────────────────────────────

async fn cookies_set_single_handler(Path((name, value)): Path<(String, String)>) -> Response {
    let cookie_str = build_set_cookie(&name, &value, &CookieAttrs::default());

    let mut resp = StatusCode::FOUND.into_response();
    resp.headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static("/cookies"));
    if let Ok(val) = HeaderValue::from_str(&cookie_str) {
        resp.headers_mut().append(header::SET_COOKIE, val);
    }

    resp
}

// ── /cookies/delete ──────────────────────────────────────────────────

async fn cookies_delete_handler(uri: Uri) -> Response {
    let query = uri.query().unwrap_or("");
    let params: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();

    let mut resp = StatusCode::FOUND.into_response();
    resp.headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static("/cookies"));

    for (name, _) in &params {
        let cookie_str = format!("{name}=; Max-Age=0; Path=/");
        if let Ok(val) = HeaderValue::from_str(&cookie_str) {
            resp.headers_mut().append(header::SET_COOKIE, val);
        }
    }

    resp
}

// ── Helpers ──────────────────────────────────────────────────────────

fn parse_cookies(headers: &HeaderMap) -> HashMap<String, String> {
    let mut cookies = HashMap::new();
    for value in headers.get_all("cookie") {
        if let Ok(s) = value.to_str() {
            for pair in s.split(';') {
                let pair = pair.trim();
                if let Some((name, val)) = pair.split_once('=') {
                    cookies.insert(name.trim().to_string(), val.trim().to_string());
                }
            }
        }
    }
    cookies
}

#[derive(Default)]
struct CookieAttrs {
    path: Option<String>,
    domain: Option<String>,
    secure: bool,
    httponly: bool,
    samesite: Option<String>,
    max_age: Option<i64>,
}

fn build_set_cookie(name: &str, value: &str, attrs: &CookieAttrs) -> String {
    let mut parts = vec![format!("{name}={value}")];

    if let Some(ref path) = attrs.path {
        parts.push(format!("Path={path}"));
    }
    if let Some(ref domain) = attrs.domain {
        parts.push(format!("Domain={domain}"));
    }
    if attrs.secure {
        parts.push("Secure".to_string());
    }
    if attrs.httponly {
        parts.push("HttpOnly".to_string());
    }
    if let Some(ref samesite) = attrs.samesite {
        parts.push(format!("SameSite={samesite}"));
    }
    if let Some(max_age) = attrs.max_age {
        parts.push(format!("Max-Age={max_age}"));
    }

    parts.join("; ")
}

// ── Router ───────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/cookies", get(cookies_handler))
        .route("/cookies/set", get(cookies_set_handler))
        .route(
            "/cookies/set/{name}/{value}",
            get(cookies_set_single_handler),
        )
        .route("/cookies/delete", get(cookies_delete_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/cookies",
            &["GET"],
            category::REDIRECTS,
            "Request cookies as JSON",
        )
        .example(Example::get("Get cookies", "/cookies")),
        Endpoint::new(
            "/cookies/set",
            &["GET"],
            category::REDIRECTS,
            "Set cookies from query parameters",
        )
        .example(Example::get(
            "Set cookies",
            "/cookies/set?session=abc123&theme=dark",
        )),
        Endpoint::new(
            "/cookies/set/{name}/{value}",
            &["GET"],
            category::REDIRECTS,
            "Set a single cookie",
        )
        .example(Example::get("Set one cookie", "/cookies/set/theme/dark")),
        Endpoint::new(
            "/cookies/delete",
            &["GET"],
            category::REDIRECTS,
            "Delete the cookies named in the query string",
        )
        .example(Example::get("Delete cookies", "/cookies/delete?session")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app() -> Router {
        crate::test_support::module_app(router)
    }

    #[tokio::test]
    async fn cookies_returns_request_cookies() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/cookies")
                    .header("cookie", "session=abc123; theme=dark")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["cookies"]["session"], "abc123");
        assert_eq!(json["cookies"]["theme"], "dark");
    }

    #[tokio::test]
    async fn cookies_set_redirects_and_sets_cookies() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/cookies/set?session=abc123&theme=dark")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::FOUND);
        assert_eq!(
            resp.headers()
                .get("location")
                .expect("location")
                .to_str()
                .expect("str"),
            "/cookies"
        );

        let set_cookies: Vec<&str> = resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        assert_eq!(set_cookies.len(), 2);
    }

    #[tokio::test]
    async fn cookies_set_with_attributes() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/cookies/set?session=abc&_path=/api&_secure=true&_httponly=true")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        let set_cookie = resp
            .headers()
            .get("set-cookie")
            .expect("set-cookie")
            .to_str()
            .expect("str");

        assert!(set_cookie.contains("session=abc"));
        assert!(set_cookie.contains("Path=/api"));
        assert!(set_cookie.contains("Secure"));
        assert!(set_cookie.contains("HttpOnly"));
    }

    #[tokio::test]
    async fn cookies_set_single() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/cookies/set/token/xyz789")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::FOUND);

        let set_cookie = resp
            .headers()
            .get("set-cookie")
            .expect("set-cookie")
            .to_str()
            .expect("str");
        assert!(set_cookie.contains("token=xyz789"));
    }

    #[tokio::test]
    async fn cookies_delete_sets_max_age_zero() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/cookies/delete?session&theme")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::FOUND);

        let set_cookies: Vec<&str> = resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        assert_eq!(set_cookies.len(), 2);

        for cookie in &set_cookies {
            assert!(cookie.contains("Max-Age=0"));
        }
    }

    #[tokio::test]
    async fn cookies_xml_negotiation() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/cookies")
                    .header("accept", "application/xml")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .expect("ct")
                .to_str()
                .expect("str"),
            "application/xml"
        );
    }
}
