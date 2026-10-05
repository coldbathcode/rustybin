//! Web console under `/ui` plus the control-plane endpoints it relies on.
//!
//! - The console is a set of static files (vanilla ES modules and CSS) in
//!   `ui/`, embedded into the binary at build time by `build.rs`
//!   (`$OUT_DIR/ui_assets.rs`). No Node build step, no CDN: it works offline.
//! - `GET /ui` redirects to `/ui/` (relative URLs, so the console also works
//!   behind a gateway path prefix); `GET /ui/` serves `index.html`;
//!   `GET /ui/{*path}` serves an asset, or `index.html` for extension-less
//!   paths (SPA fallback, e.g. the OAuth callback `/ui/callback`).
//! - Every asset carries a strong `ETag` and `Cache-Control: no-cache`, so
//!   browsers revalidate cheaply (`304 Not Modified`) and never run stale code.
//! - `GET /_rustybin/catalog`: the route catalogue as JSON (API explorer).
//! - `GET /_rustybin/status`: uptime, health and inspector counters (overview).
//!
//! - `index.html` carries `<meta name="rustybin-*">` elements ([`console_meta`]):
//!   the control auth mode, `RUSTYBIN_CONSOLE_TITLE` and
//!   `RUSTYBIN_CONSOLE_BACKLINK` (HTML-escaped), so the console can show its
//!   sign-in screen and header before it may call the control plane.
//!
//! `/ui/*` and `/_rustybin/*` are excluded from inspector capture and fault
//! injection (see [`crate::control::is_control_path`]). The console files are
//! served without credentials in every `RUSTYBIN_CONTROL_AUTH` mode (they hold
//! no instance data); `/_rustybin/*` is protected by [`crate::control_auth`].

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;

use axum::extract::{Path, RawQuery, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::catalog::{self, category, AuthDef, BodyDef, Endpoint, Example, Protocol, RouteCheck};
use crate::config::Config;
use crate::state::AppState;
use std::sync::Arc;

include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));

/// Content-Security-Policy of the console. Requests may go to any origin
/// (the "send through a gateway" feature), scripts and styles only come from
/// the console itself.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
                   img-src 'self' data: blob: http: https:; connect-src 'self' http: https: ws: wss:; \
                   frame-ancestors 'self'; base-uri 'self'; form-action 'self'";

/// One embedded file.
struct Asset {
    bytes: &'static [u8],
    content_type: &'static str,
    etag: String,
}

/// MIME type from the file extension.
pub fn content_type_for(path: &str) -> &'static str {
    let ext = path
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn etag_of(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

fn assets() -> &'static HashMap<&'static str, Asset> {
    static ASSETS: OnceLock<HashMap<&'static str, Asset>> = OnceLock::new();
    ASSETS.get_or_init(|| {
        UI_ASSETS
            .iter()
            .map(|(path, bytes)| {
                (
                    *path,
                    Asset {
                        bytes,
                        content_type: content_type_for(path),
                        etag: etag_of(bytes),
                    },
                )
            })
            .collect()
    })
}

/// Paths of every embedded console file (tests, diagnostics).
pub fn asset_paths() -> Vec<&'static str> {
    let mut v: Vec<_> = UI_ASSETS.iter().map(|(p, _)| *p).collect();
    v.sort_unstable();
    v
}

fn not_modified(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .map(|t| t.trim().trim_start_matches("W/"))
                .any(|t| t == etag || t == "*")
        })
}

fn serve_bytes(
    headers: &HeaderMap,
    bytes: Vec<u8>,
    content_type: &'static str,
    etag: &str,
) -> Response {
    let mut resp = if not_modified(headers, etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let mut r = bytes.into_response();
        r.headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
        r
    };
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(etag) {
        h.insert(header::ETAG, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    if content_type.starts_with("text/html") {
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        );
    }
    resp
}

fn serve_asset(headers: &HeaderMap, asset: &Asset) -> Response {
    serve_bytes(
        headers,
        asset.bytes.to_vec(),
        asset.content_type,
        &asset.etag,
    )
}

fn missing_console() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": "console assets are not embedded in this build",
            "hint": "build from a checkout that contains the ui/ directory",
        })),
    )
        .into_response()
}

/// `<meta>` elements describing the instance to the console before it can
/// call the (possibly authenticated) control plane: control auth mode,
/// console title and back link (`RUSTYBIN_CONSOLE_TITLE`,
/// `RUSTYBIN_CONSOLE_BACKLINK`). Values are HTML-escaped.
pub fn console_meta(config: &Config) -> String {
    let mut out = String::new();
    let mut meta = |name: &str, value: &str| {
        out.push_str(&format!(
            "<meta name=\"rustybin-{name}\" content=\"{}\">",
            crate::landing::html_escape(value)
        ));
    };
    meta("control-auth", config.control_auth.as_str());
    if let Some(title) = &config.console_title {
        meta("console-title", title);
    }
    if let Some(link) = &config.console_backlink {
        meta("console-backlink", link);
    }
    out
}

/// `index.html` for a SPA route `depth` segments below `/ui/`: relative asset
/// URLs are re-anchored with a `<base>` element; [`console_meta`] is added.
fn serve_index(headers: &HeaderMap, depth: usize, config: &Config) -> Response {
    let Some(index) = assets().get("index.html") else {
        return missing_console();
    };
    let base = if depth <= 1 {
        String::new()
    } else {
        format!("<base href=\"{}\">", "../".repeat(depth - 1))
    };
    let head = format!("<head>{base}{}", console_meta(config));
    let html = String::from_utf8_lossy(index.bytes).replacen("<head>", &head, 1);
    let etag = etag_of(html.as_bytes());
    serve_bytes(headers, html.into_bytes(), index.content_type, &etag)
}

async fn ui_root(RawQuery(query): RawQuery) -> Response {
    // Relative Location: keeps a gateway path prefix (`/prefix/ui` -> `/prefix/ui/`).
    let location = match query {
        Some(q) if !q.is_empty() => format!("ui/?{q}"),
        _ => "ui/".to_string(),
    };
    let mut resp = StatusCode::TEMPORARY_REDIRECT.into_response();
    if let Ok(v) = HeaderValue::from_str(&location) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    resp
}

async fn ui_index(State(config): State<Arc<Config>>, headers: HeaderMap) -> Response {
    serve_index(&headers, 0, &config)
}

async fn ui_path(
    State(config): State<Arc<Config>>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(asset) = assets().get(path.as_str()) {
        return serve_asset(&headers, asset);
    }
    let last = path.rsplit('/').next().unwrap_or("");
    if last.contains('.') || path.split('/').any(|s| s == "..") {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no such console asset", "path": format!("/ui/{path}") })),
        )
            .into_response();
    }
    let depth = path.trim_end_matches('/').split('/').count();
    serve_index(&headers, depth, &config)
}

// ── /_rustybin/catalog ──────────────────────────────────────────────

fn example_json(ex: &Example) -> Value {
    let mut headers: Vec<Value> = ex
        .headers
        .iter()
        .map(|(k, v)| json!({ "name": k, "value": v }))
        .collect();
    let (body, body_type) = match &ex.body {
        None => (Value::Null, Value::Null),
        Some(BodyDef::Json(s)) => (json!(s), json!("json")),
        Some(BodyDef::Xml(s)) => (json!(s), json!("xml")),
        Some(BodyDef::Form(fields)) => {
            headers.push(json!({
                "name": "Content-Type",
                "value": "application/x-www-form-urlencoded",
            }));
            let encoded = form_urlencoded::Serializer::new(String::new())
                .extend_pairs(fields.iter())
                .finish();
            (json!(encoded), json!("form"))
        }
    };
    let auth = match &ex.auth {
        None => Value::Null,
        Some(AuthDef::Basic { user, pass }) => {
            json!({ "type": "basic", "username": user, "password": pass })
        }
        Some(AuthDef::Bearer(token)) => json!({ "type": "bearer", "token": token }),
    };
    let expect = match ex.check {
        RouteCheck::ExpectStatus(code) => json!(code),
        _ => Value::Null,
    };
    json!({
        "name": ex.name,
        "method": ex.method,
        "path": ex.path,
        "headers": headers,
        "body": body,
        "body_type": body_type,
        "auth": auth,
        "expect_status": expect,
    })
}

fn endpoint_json(ep: &Endpoint) -> Value {
    json!({
        "path": ep.path,
        "methods": ep.methods,
        "expanded_methods": ep.expanded_methods(),
        "category": ep.category,
        "summary": ep.summary,
        "description": ep.description,
        "protocol": match ep.protocol {
            Protocol::Http => "http",
            Protocol::Sse => "sse",
            Protocol::WebSocket => "websocket",
        },
        "examples": ep.examples.iter().map(example_json).collect::<Vec<_>>(),
    })
}

/// The catalogue as JSON, in display order.
pub fn catalog_json() -> Value {
    let groups = catalog::grouped();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "count": catalog::all().len(),
        "categories": groups
            .iter()
            .map(|(name, eps)| json!({ "name": name, "count": eps.len() }))
            .collect::<Vec<_>>(),
        "endpoints": catalog::all().iter().map(endpoint_json).collect::<Vec<_>>(),
    })
}

async fn catalog_handler() -> Response {
    static CACHED: OnceLock<Value> = OnceLock::new();
    Json(CACHED.get_or_init(catalog_json).clone()).into_response()
}

// ── /_rustybin/status ───────────────────────────────────────────────

async fn status_handler(State(state): State<AppState>) -> Response {
    let uptime = state.identity.start_time.elapsed();
    let started = chrono::Utc::now()
        - chrono::Duration::from_std(uptime).unwrap_or_else(|_| chrono::Duration::zero());
    Json(json!({
        "name": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "instance_id": state.config.instance_id,
        "hostname": gethostname::gethostname().to_string_lossy(),
        "uptime_seconds": uptime.as_secs(),
        "started_at": started.to_rfc3339(),
        "healthy": state.health.is_healthy(),
        "public_mode": state.config.public_mode,
        "admin_token_configured": state.config.admin_token.is_some(),
        "control_auth": state.config.control_auth.as_str(),
        "hosted_mode": state.config.hosted_mode,
        "git_sha": crate::control::git_sha(),
        "console": {
            "title": state.config.console_title,
            "backlink": state.config.console_backlink,
        },
        "identity_requests": state.identity.request_count.load(Ordering::Relaxed),
        "inspector": {
            "stored": state.inspector.len(),
            "capacity": state.inspector.capacity(),
            "total_captured": state.inspector.total_recorded(),
        },
        "ports": {
            "http": state.config.http_port,
            "https": state.config.https_port,
            "grpc": state.config.grpc_port,
        },
    }))
    .into_response()
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/ui", get(ui_root))
        .route("/ui/", get(ui_index))
        .route("/ui/{*path}", get(ui_path))
        .route("/_rustybin/catalog", get(catalog_handler))
        .route("/_rustybin/status", get(status_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/ui",
            &["GET"],
            category::DOCS,
            "Web console (redirects to /ui/)",
        )
        .description(
            "Console for presenters: live traffic inspector, request bins, API explorer, AI \
             playground, MCP inspector, A2A client, chaos controls and token lab. Embedded in \
             the binary, works offline.",
        )
        .example(Example::get("Open the console", "/ui").expect_status(307)),
        Endpoint::new("/ui/", &["GET"], category::DOCS, "Web console entry page")
            .example(Example::get("Console index", "/ui/")),
        Endpoint::new(
            "/ui/{*path}",
            &["GET"],
            category::DOCS,
            "Console assets; extension-less paths fall back to index.html",
        )
        .description("Strong ETags with Cache-Control: no-cache (If-None-Match answers 304).")
        .example(Example::get("Console script", "/ui/app.js"))
        .example(Example::get("SPA fallback", "/ui/callback")),
        Endpoint::new(
            "/_rustybin/catalog",
            &["GET"],
            category::CONTROL,
            "Route catalogue as JSON (paths, methods, categories, examples)",
        )
        .example(Example::get("Catalogue", "/_rustybin/catalog")),
        Endpoint::new(
            "/_rustybin/status",
            &["GET"],
            category::CONTROL,
            "Uptime, health state and inspector counters",
        )
        .example(Example::get("Status", "/_rustybin/status")),
    ]
}

pub fn openapi_paths() -> Value {
    let html = json!({ "200": { "description": "HTML", "content": { "text/html": { "schema": { "type": "string" } } } } });
    json!({
        "/ui": {
            "get": {
                "tags": ["Docs"],
                "summary": "Web console",
                "operationId": "getConsoleRedirect",
                "responses": { "307": { "description": "Redirect to /ui/" } }
            }
        },
        "/ui/": {
            "get": {
                "tags": ["Docs"],
                "summary": "Web console entry page",
                "operationId": "getConsoleIndex",
                "responses": html
            }
        },
        "/ui/{path}": {
            "get": {
                "tags": ["Docs"],
                "summary": "Console asset (SPA fallback to index.html)",
                "operationId": "getConsoleAsset",
                "parameters": [{ "name": "path", "in": "path", "required": true, "schema": { "type": "string" } }],
                "responses": {
                    "200": { "description": "Asset" },
                    "304": { "description": "Not modified (If-None-Match)" },
                    "404": { "description": "Unknown asset" }
                }
            }
        },
        "/_rustybin/catalog": {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Route catalogue",
                "operationId": "getRustybinCatalog",
                "responses": { "200": { "description": "Catalogue", "content": { "application/json": { "schema": {
                    "type": "object",
                    "properties": {
                        "count": { "type": "integer" },
                        "categories": { "type": "array", "items": { "type": "object" } },
                        "endpoints": { "type": "array", "items": { "type": "object", "properties": {
                            "path": { "type": "string" },
                            "methods": { "type": "array", "items": { "type": "string" } },
                            "category": { "type": "string" },
                            "summary": { "type": "string" },
                            "description": { "type": "string" },
                            "protocol": { "type": "string", "enum": ["http", "sse", "websocket"] },
                            "examples": { "type": "array", "items": { "type": "object" } }
                        } } }
                    }
                } } } } }
            }
        },
        "/_rustybin/status": {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Instance status",
                "operationId": "getRustybinStatus",
                "responses": { "200": { "description": "Status", "content": { "application/json": { "schema": { "type": "object" } } } } }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_json, body_string, get_request, test_app};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn header<'a>(resp: &'a Response, name: &str) -> &'a str {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }

    #[tokio::test]
    async fn root_redirects_relative() {
        let resp = test_app()
            .oneshot(get_request("/ui"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(header(&resp, "location"), "ui/");
        let resp = test_app()
            .oneshot(get_request("/ui?x=1"))
            .await
            .expect("response");
        assert_eq!(header(&resp, "location"), "ui/?x=1");
    }

    #[tokio::test]
    async fn index_is_html() {
        let resp = test_app()
            .oneshot(get_request("/ui/"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(header(&resp, "content-type").starts_with("text/html"));
        assert!(!header(&resp, "etag").is_empty());
        assert_eq!(header(&resp, "cache-control"), "no-cache");
        assert!(header(&resp, "content-security-policy").contains("script-src 'self'"));
        let html = body_string(resp).await;
        assert!(html.contains("<title>Rustybin console</title>"));
        assert!(html.contains("app.js"));
    }

    #[tokio::test]
    async fn assets_have_content_types() {
        for (path, ct) in [
            ("/ui/app.js", "text/javascript"),
            ("/ui/styles.css", "text/css"),
            ("/ui/favicon.svg", "image/svg+xml"),
            ("/ui/views/traffic.js", "text/javascript"),
        ] {
            let resp = test_app()
                .oneshot(get_request(path))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK, "{path}");
            assert!(header(&resp, "content-type").starts_with(ct), "{path}");
            assert_eq!(header(&resp, "x-content-type-options"), "nosniff");
        }
    }

    #[tokio::test]
    async fn spa_fallback_and_unknown_assets() {
        let resp = test_app()
            .oneshot(get_request("/ui/callback?code=abc&state=x"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(header(&resp, "content-type").starts_with("text/html"));
        let html = body_string(resp).await;
        assert!(!html.contains("<base"), "depth 1 needs no base element");

        let resp = test_app()
            .oneshot(get_request("/ui/a/b/c"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_string(resp).await.contains("<base href=\"../../\">"));

        let resp = test_app()
            .oneshot(get_request("/ui/missing.js"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn etag_revalidation_returns_304() {
        let resp = test_app()
            .oneshot(get_request("/ui/app.js"))
            .await
            .expect("response");
        let etag = header(&resp, "etag").to_string();
        assert!(etag.starts_with('"'));
        let req = Request::builder()
            .uri("/ui/app.js")
            .header("if-none-match", &etag)
            .body(Body::empty())
            .expect("request");
        let resp = test_app().oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(header(&resp, "etag"), etag);
        assert!(body_string(resp).await.is_empty());

        let req = Request::builder()
            .uri("/ui/app.js")
            .header("if-none-match", "\"other\"")
            .body(Body::empty())
            .expect("request");
        let resp = test_app().oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn console_is_exempt_from_faults_and_capture() {
        let state = crate::test_support::test_state();
        let app = crate::test_support::test_app_with(state.clone());
        let req = Request::builder()
            .uri("/ui/")
            .header("x-rustybin-fail", "503")
            .body(Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(state.inspector.len(), 0);
    }

    #[tokio::test]
    async fn catalog_shape() {
        let resp = test_app()
            .oneshot(get_request("/_rustybin/catalog"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let endpoints = v["endpoints"].as_array().expect("endpoints");
        assert_eq!(v["count"].as_u64(), Some(endpoints.len() as u64));
        assert_eq!(endpoints.len(), catalog::all().len());
        assert!(v["categories"].as_array().is_some_and(|c| !c.is_empty()));
        let echo = endpoints
            .iter()
            .find(|e| e["path"] == "/echo")
            .expect("echo endpoint");
        assert!(echo["methods"].is_array());
        assert!(echo["category"].is_string());
        assert!(echo["summary"].is_string());
        assert!(echo["description"].is_string());
        assert_eq!(echo["protocol"], "http");
        assert!(echo["examples"][0]["method"].is_string());
        let token = endpoints
            .iter()
            .find(|e| e["path"] == "/oauth/token")
            .expect("token endpoint");
        let ex = &token["examples"][0];
        assert_eq!(ex["body_type"], "form");
        assert!(ex["body"]
            .as_str()
            .is_some_and(|b| b.contains("grant_type=client_credentials")));
        assert!(endpoints
            .iter()
            .any(|e| e["protocol"] == "websocket" || e["protocol"] == "sse"));
    }

    #[tokio::test]
    async fn status_reports_counters() {
        let resp = test_app()
            .oneshot(get_request("/_rustybin/status"))
            .await
            .expect("response");
        let v = body_json(resp).await;
        assert_eq!(v["instance_id"], "test-instance");
        assert_eq!(v["healthy"], true);
        assert!(v["inspector"]["capacity"].as_u64().is_some_and(|c| c > 0));
        assert!(v["uptime_seconds"].is_number());
    }

    #[tokio::test]
    async fn console_meta_is_escaped_and_reported() {
        let mut config = crate::test_support::test_config();
        config.console_title = Some("Acme <script>\"x\"</script>".to_string());
        config.console_backlink = Some("https://portal.example.com/p?a=1&b=2".to_string());
        config.control_auth = crate::control_auth::ControlAuth::Token;
        config.admin_token = Some("t".to_string());
        let app = crate::test_support::test_app_with(crate::test_support::test_state_with(config));
        for path in ["/ui/", "/ui/a/b"] {
            let resp = app
                .clone()
                .oneshot(get_request(path))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK, "{path}");
            let html = body_string(resp).await;
            assert!(html.contains("<meta name=\"rustybin-control-auth\" content=\"token\">"));
            assert!(html.contains("content=\"Acme &lt;script&gt;&quot;x&quot;&lt;/script&gt;\""));
            assert!(html.contains("content=\"https://portal.example.com/p?a=1&amp;b=2\""));
            assert!(!html.contains("<script>\"x\""));
        }
        let req = Request::builder()
            .uri("/_rustybin/status")
            .header("authorization", "Bearer t")
            .body(Body::empty())
            .expect("request");
        let v = body_json(app.oneshot(req).await.expect("response")).await;
        assert_eq!(v["control_auth"], "token");
        assert_eq!(
            v["console"]["backlink"],
            "https://portal.example.com/p?a=1&b=2"
        );
    }

    #[test]
    fn backlink_validation() {
        use crate::config::valid_backlink;
        assert!(valid_backlink("https://portal.example.com/pods/acme"));
        assert!(valid_backlink("http://localhost:3000"));
        for bad in [
            "javascript:alert(1)",
            "ftp://example.com",
            "https://",
            "https:///path",
            "https://user@evil.example.com",
            "https://a.example.com/\"onmouseover=x",
            "https://a.example.com/ x",
            "//example.com",
        ] {
            assert!(!valid_backlink(bad), "{bad}");
        }
        let (c, w) = Config::from_lookup(|k| match k {
            "RUSTYBIN_CONSOLE_BACKLINK" => Some("javascript:alert(1)".to_string()),
            "RUSTYBIN_CONSOLE_TITLE" => Some(format!("  Demo\u{7}{}  ", "x".repeat(200))),
            _ => None,
        });
        assert!(c.console_backlink.is_none());
        assert_eq!(w.len(), 1);
        let title = c.console_title.expect("title");
        assert!(title.starts_with("Demo") && !title.contains('\u{7}'));
        assert_eq!(
            title.chars().count(),
            crate::config::MAX_CONSOLE_TITLE_CHARS
        );
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type_for("a/b.JS"), "text/javascript; charset=utf-8");
        assert_eq!(content_type_for("x.svg"), "image/svg+xml");
        assert_eq!(content_type_for("noext"), "application/octet-stream");
    }

    #[test]
    fn embedded_files_have_no_dashes() {
        // Project rule: no em or en dash characters anywhere, UI text included.
        for (path, bytes) in UI_ASSETS {
            let text = String::from_utf8_lossy(bytes);
            assert!(
                !text.contains('\u{2014}') && !text.contains('\u{2013}'),
                "{path} contains an em or en dash"
            );
        }
        assert!(asset_paths().contains(&"index.html"));
    }
}
