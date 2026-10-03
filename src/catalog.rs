//! Route catalogue: the single source of truth for every HTTP route.
//!
//! Each module exposes `pub fn catalog() -> Vec<Endpoint>` describing EVERY
//! route it registers, and is listed once in [`all`]. The landing page, the
//! collection exports under `/export/*` and the README endpoint table are all
//! generated from it. Consistency tests at the bottom of this file fail when
//! a `.route("...")` is missing here, when a catalogue path is missing from
//! the OpenAPI spec, or when an example does not route (404/405).
//!
//! ```ignore
//! pub fn catalog() -> Vec<Endpoint> {
//!     vec![Endpoint::new("/echo", &["ANY"], category::ECHO, "Echo the request")
//!         .description("Longer text (optional)")
//!         .example(Example::get("GET /echo", "/echo"))
//!         .example(Example::post("POST /echo", "/echo").json(r#"{"a":1}"#))]
//! }
//! ```

use std::sync::OnceLock;

/// Category names. Ordering for every generated view is [`CATEGORY_ORDER`];
/// add new categories to both places (the tests enforce it).
pub mod category {
    pub const ECHO: &str = "Echo & Reflection";
    pub const STATUS: &str = "Status Codes";
    pub const SHAPING: &str = "Response Shaping";
    pub const STREAMING: &str = "Streaming (SSE)";
    pub const REDIRECTS: &str = "Redirects & Cookies";
    pub const INFO: &str = "Info & Random";
    pub const AUTH_BASIC: &str = "Auth: Basic & API Key";
    pub const AUTH_HMAC: &str = "Auth: HMAC";
    pub const AUTH_JWT: &str = "Auth: JWT";
    pub const AUTH_OIDC: &str = "Auth: OIDC Provider";
    pub const AUTH_MTLS: &str = "Auth: mTLS";
    pub const AI_OPENAI: &str = "AI: OpenAI-compatible";
    pub const AI_ANTHROPIC: &str = "AI: Anthropic-compatible";
    pub const AI_MOCK: &str = "AI: Mock LLM";
    pub const AI_GUARDRAILS: &str = "AI: Guardrails";
    pub const MCP: &str = "MCP Server";
    pub const A2A: &str = "A2A Agents";
    pub const GRAPHQL: &str = "GraphQL";
    pub const ORCHESTRATION: &str = "Orchestration";
    pub const SOAP: &str = "SOAP / XML";
    pub const WEBSOCKET: &str = "WebSocket";
    pub const RELIABILITY: &str = "Reliability Testing";
    pub const REQUEST_BIN: &str = "Request Bin & Webhooks";
    pub const HEALTH: &str = "Health & Identity";
    pub const CONTROL: &str = "Control Plane";
    pub const DOCS: &str = "Docs & Exports";
}

/// Display order of categories (landing page, exports, README).
pub const CATEGORY_ORDER: &[&str] = &[
    category::ECHO,
    category::STATUS,
    category::SHAPING,
    category::STREAMING,
    category::REDIRECTS,
    category::INFO,
    category::AUTH_BASIC,
    category::AUTH_HMAC,
    category::AUTH_JWT,
    category::AUTH_OIDC,
    category::AUTH_MTLS,
    category::AI_OPENAI,
    category::AI_ANTHROPIC,
    category::AI_MOCK,
    category::AI_GUARDRAILS,
    category::MCP,
    category::A2A,
    category::GRAPHQL,
    category::ORCHESTRATION,
    category::SOAP,
    category::WEBSOCKET,
    category::RELIABILITY,
    category::REQUEST_BIN,
    category::HEALTH,
    category::CONTROL,
    category::DOCS,
];

/// Position of a category in [`CATEGORY_ORDER`] (unknown categories sort last).
pub fn category_rank(name: &str) -> usize {
    CATEGORY_ORDER
        .iter()
        .position(|c| *c == name)
        .unwrap_or(CATEGORY_ORDER.len())
}

/// Request body of an example.
#[derive(Clone, Debug)]
pub enum BodyDef {
    Json(&'static str),
    Form(&'static [(&'static str, &'static str)]),
    Xml(&'static str),
}

/// Credentials of an example (exporters map these to native auth blocks).
#[derive(Clone, Debug)]
pub enum AuthDef {
    Basic {
        user: &'static str,
        pass: &'static str,
    },
    Bearer(&'static str),
}

/// What the consistency test expects when it sends an example.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteCheck {
    /// Must not return 404 or 405 (the default).
    Routable,
    /// Must return exactly this status (e.g. `/status/404`).
    ExpectStatus(u16),
    /// Not sent by the test (e.g. a WebSocket upgrade); the reason is documentation.
    Skip(&'static str),
}

/// Transport of an endpoint. Exporters that cannot express WebSockets skip them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Http,
    Sse,
    WebSocket,
}

/// A concrete, runnable request against an endpoint.
#[derive(Clone, Debug)]
pub struct Example {
    pub name: &'static str,
    pub method: &'static str,
    /// Concrete path, optionally with a query string (`/cookies/set?a=b`).
    pub path: &'static str,
    pub headers: Vec<(&'static str, &'static str)>,
    pub body: Option<BodyDef>,
    pub auth: Option<AuthDef>,
    pub check: RouteCheck,
}

impl Example {
    pub fn new(name: &'static str, method: &'static str, path: &'static str) -> Self {
        Self {
            name,
            method,
            path,
            headers: Vec::new(),
            body: None,
            auth: None,
            check: RouteCheck::Routable,
        }
    }

    pub fn get(name: &'static str, path: &'static str) -> Self {
        Self::new(name, "GET", path)
    }

    pub fn post(name: &'static str, path: &'static str) -> Self {
        Self::new(name, "POST", path)
    }

    pub fn delete(name: &'static str, path: &'static str) -> Self {
        Self::new(name, "DELETE", path)
    }

    pub fn header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }

    fn ensure_content_type(&mut self, value: &'static str) {
        if !self
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        {
            self.headers.push(("Content-Type", value));
        }
    }

    /// JSON body (adds `Content-Type: application/json` unless already set).
    pub fn json(mut self, body: &'static str) -> Self {
        self.ensure_content_type("application/json");
        self.body = Some(BodyDef::Json(body));
        self
    }

    /// Form body (exporters add the urlencoded content type themselves).
    pub fn form(mut self, fields: &'static [(&'static str, &'static str)]) -> Self {
        self.body = Some(BodyDef::Form(fields));
        self
    }

    /// XML body (adds `Content-Type: text/xml` unless already set).
    pub fn xml(mut self, body: &'static str) -> Self {
        self.ensure_content_type("text/xml");
        self.body = Some(BodyDef::Xml(body));
        self
    }

    pub fn basic(mut self, user: &'static str, pass: &'static str) -> Self {
        self.auth = Some(AuthDef::Basic { user, pass });
        self
    }

    pub fn bearer(mut self, token: &'static str) -> Self {
        self.auth = Some(AuthDef::Bearer(token));
        self
    }

    /// The example legitimately returns this status (checked by the tests).
    pub fn expect_status(mut self, status: u16) -> Self {
        self.check = RouteCheck::ExpectStatus(status);
        self
    }

    /// Exclude the example from the routing test, with a reason.
    pub fn skip_check(mut self, reason: &'static str) -> Self {
        self.check = RouteCheck::Skip(reason);
        self
    }

    /// Path without the query string.
    pub fn path_only(&self) -> &'static str {
        self.path.split('?').next().unwrap_or(self.path)
    }
}

/// One registered route (path + method set) with its documentation.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// Path exactly as registered with axum (`/status/{code}`, `/echo/{*path}`).
    pub path: &'static str,
    /// HTTP methods; `"ANY"` for routes registered with `any()`.
    pub methods: &'static [&'static str],
    /// One of the [`category`] constants.
    pub category: &'static str,
    /// One-line summary.
    pub summary: &'static str,
    /// Optional longer description (may be "").
    pub description: &'static str,
    /// Runnable examples (used by the exporters and the routing test).
    pub examples: Vec<Example>,
    /// Transport (HTTP by default).
    pub protocol: Protocol,
}

/// Methods documented for `ANY` routes.
pub const ANY_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE"];

impl Endpoint {
    pub fn new(
        path: &'static str,
        methods: &'static [&'static str],
        category: &'static str,
        summary: &'static str,
    ) -> Self {
        Self {
            path,
            methods,
            category,
            summary,
            description: "",
            examples: Vec::new(),
            protocol: Protocol::Http,
        }
    }

    pub fn description(mut self, description: &'static str) -> Self {
        self.description = description;
        self
    }

    pub fn example(mut self, mut example: Example) -> Self {
        if self.protocol == Protocol::WebSocket && example.check == RouteCheck::Routable {
            example.check = RouteCheck::Skip("websocket upgrade");
        }
        self.examples.push(example);
        self
    }

    /// Mark as a WebSocket endpoint. Its examples are skipped by the routing
    /// test and by exporters that cannot express WebSockets.
    pub fn websocket(mut self) -> Self {
        self.protocol = Protocol::WebSocket;
        for ex in &mut self.examples {
            ex.check = RouteCheck::Skip("websocket upgrade");
        }
        self
    }

    /// Mark as a Server-Sent Events endpoint.
    pub fn sse(mut self) -> Self {
        self.protocol = Protocol::Sse;
        self
    }

    pub fn is_any(&self) -> bool {
        self.methods.contains(&"ANY")
    }

    /// Concrete methods (`ANY` expands to [`ANY_METHODS`]).
    pub fn expanded_methods(&self) -> Vec<&'static str> {
        if self.is_any() {
            ANY_METHODS.to_vec()
        } else {
            self.methods.to_vec()
        }
    }

    /// Human label for the method set, e.g. `GET POST` or `ANY`.
    pub fn methods_label(&self) -> String {
        self.methods.join(" ")
    }
}

/// Every module's catalogue function. Register new modules here (once).
const SOURCES: &[fn() -> Vec<Endpoint>] = &[
    crate::landing::catalog,
    crate::echo::catalog,
    crate::status::catalog,
    crate::response_shaping::catalog,
    crate::redirects::catalog,
    crate::cookies::catalog,
    crate::info::catalog,
    crate::random::catalog,
    crate::image::catalog,
    crate::auth_basic::catalog,
    crate::auth_apikey::catalog,
    crate::auth_hmac::catalog,
    crate::auth_jwt::catalog,
    crate::oidc::catalog,
    crate::auth_mtls::catalog,
    crate::ai_gateway::catalog,
    crate::ai_anthropic::catalog,
    crate::graphql::catalog,
    crate::orchestration::catalog,
    crate::soap::catalog,
    crate::websocket::catalog,
    crate::flaky::catalog,
    crate::health::catalog,
    crate::identity::catalog,
    crate::inspector::catalog,
    crate::control::catalog,
    crate::openapi::catalog,
    crate::collections::catalog,
];

/// The full catalogue, ordered by [`CATEGORY_ORDER`] (module order within a
/// category). Built once and cached.
pub fn all() -> &'static [Endpoint] {
    static CATALOG: OnceLock<Vec<Endpoint>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut endpoints: Vec<Endpoint> = SOURCES.iter().flat_map(|f| f()).collect();
        endpoints.sort_by_key(|e| category_rank(e.category));
        endpoints
    })
}

/// The catalogue grouped by category, in display order.
pub fn grouped() -> Vec<(&'static str, Vec<&'static Endpoint>)> {
    let mut groups: Vec<(&'static str, Vec<&'static Endpoint>)> = Vec::new();
    for ep in all() {
        match groups.last_mut() {
            Some((name, list)) if *name == ep.category => list.push(ep),
            _ => groups.push((ep.category, vec![ep])),
        }
    }
    groups
}

/// Marker lines delimiting the generated table in README.md.
pub const README_BEGIN: &str = "<!-- BEGIN ENDPOINTS -->";
pub const README_END: &str = "<!-- END ENDPOINTS -->";

/// Markdown endpoint tables for the README (`rustybin --print-endpoints-markdown`).
pub fn endpoints_markdown() -> String {
    fn cell(s: &str) -> String {
        s.replace('|', "\\|").replace('\n', " ")
    }
    let mut out = String::new();
    out.push_str("<!-- Generated from src/catalog.rs: run `cargo run -- --print-endpoints-markdown` and paste, or `RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints`. -->\n");
    for (name, endpoints) in grouped() {
        out.push_str(&format!("\n### {}\n\n", cell(name)));
        out.push_str("| Methods | Path | Description |\n|---|---|---|\n");
        for ep in endpoints {
            let kind = match ep.protocol {
                Protocol::WebSocket => " (WebSocket)",
                Protocol::Sse => " (SSE)",
                Protocol::Http => "",
            };
            out.push_str(&format!(
                "| {} | `{}` | {}{} |\n",
                cell(&ep.methods_label()),
                cell(ep.path),
                cell(ep.summary),
                kind
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::collections::{BTreeSet, HashSet};
    use std::path::Path;
    use tower::ServiceExt;

    /// Replace every `{param}` / `{*param}` with `{}` so axum paths and
    /// OpenAPI paths compare independently of parameter names.
    fn normalize(path: &str) -> String {
        let mut out = String::with_capacity(path.len());
        let mut in_param = false;
        for c in path.chars() {
            match c {
                '{' => {
                    in_param = true;
                    out.push_str("{}");
                }
                '}' => in_param = false,
                _ if in_param => {}
                _ => out.push(c),
            }
        }
        out
    }

    fn rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().is_some_and(|e| e == "rs") {
                out.push(p);
            }
        }
    }

    /// String literals passed to `.route(` / `.route_service(` (non-test code).
    fn registered_routes() -> Vec<(String, String)> {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rs_files(&src, &mut files);
        let mut found = Vec::new();
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            // Ignore unit tests (they may build ad-hoc routers).
            let code = match text.find("#[cfg(test)]\nmod tests") {
                Some(i) => &text[..i],
                None => &text[..],
            };
            let code: String = code
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for needle in [".route(", ".route_service("] {
                let mut rest = code.as_str();
                while let Some(i) = rest.find(needle) {
                    rest = &rest[i + needle.len()..];
                    let trimmed = rest.trim_start();
                    if let Some(lit) = trimmed.strip_prefix('"') {
                        if let Some(end) = lit.find('"') {
                            found.push((file.display().to_string(), lit[..end].to_string()));
                        }
                    }
                }
            }
        }
        found
    }

    #[test]
    fn every_registered_route_is_in_the_catalogue() {
        let catalogue: HashSet<&str> = all().iter().map(|e| e.path).collect();
        let routes = registered_routes();
        assert!(routes.len() > 50, "route scan found too few routes");
        let missing: Vec<_> = routes
            .iter()
            .filter(|(_, p)| !catalogue.contains(p.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "routes missing from the catalogue (add them to the module's catalog()): {missing:?}"
        );
    }

    #[test]
    fn catalogue_paths_are_unique_and_categorised() {
        let mut seen = HashSet::new();
        for ep in all() {
            assert!(seen.insert(ep.path), "duplicate catalogue path {}", ep.path);
            assert!(
                CATEGORY_ORDER.contains(&ep.category),
                "category {:?} of {} is not in CATEGORY_ORDER",
                ep.category,
                ep.path
            );
            assert!(!ep.summary.is_empty(), "{} has no summary", ep.path);
            assert!(!ep.methods.is_empty(), "{} has no methods", ep.path);
        }
    }

    #[test]
    fn every_catalogue_path_is_in_openapi() {
        let spec = crate::openapi::build_spec();
        let paths = spec["paths"].as_object().expect("paths object");
        let by_norm: std::collections::HashMap<String, &serde_json::Value> =
            paths.iter().map(|(k, v)| (normalize(k), v)).collect();
        let mut problems = Vec::new();
        for ep in all() {
            match by_norm.get(&normalize(ep.path)) {
                None => problems.push(format!("{} missing from OpenAPI", ep.path)),
                Some(item) => {
                    for m in ep.expanded_methods() {
                        if item.get(m.to_ascii_lowercase()).is_none() {
                            problems.push(format!("{} {} missing from OpenAPI", m, ep.path));
                        }
                    }
                }
            }
        }
        assert!(problems.is_empty(), "{problems:#?}");
    }

    fn build_request(ex: &Example) -> Request<Body> {
        let mut builder = Request::builder().method(ex.method).uri(ex.path);
        for (k, v) in &ex.headers {
            builder = builder.header(*k, *v);
        }
        match &ex.auth {
            Some(AuthDef::Basic { user, pass }) => {
                use base64::Engine;
                let enc =
                    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
                builder = builder.header("Authorization", format!("Basic {enc}"));
            }
            Some(AuthDef::Bearer(t)) => {
                builder = builder.header("Authorization", format!("Bearer {t}"));
            }
            None => {}
        }
        let body = match &ex.body {
            None => Body::empty(),
            Some(BodyDef::Json(s)) | Some(BodyDef::Xml(s)) => Body::from(*s),
            Some(BodyDef::Form(fields)) => {
                builder = builder.header("Content-Type", "application/x-www-form-urlencoded");
                let encoded = form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(fields.iter())
                    .finish();
                Body::from(encoded)
            }
        };
        builder.body(body).expect("request")
    }

    #[tokio::test]
    async fn every_example_routes() {
        let app = test_support::test_app();
        let mut failures = Vec::new();
        let mut sent = 0;
        for ep in all() {
            for ex in &ep.examples {
                if matches!(ex.check, RouteCheck::Skip(_)) {
                    continue;
                }
                sent += 1;
                let resp = app
                    .clone()
                    .oneshot(build_request(ex))
                    .await
                    .expect("infallible");
                let status = resp.status();
                let ok = match ex.check {
                    RouteCheck::ExpectStatus(code) => status.as_u16() == code,
                    _ => {
                        status != StatusCode::NOT_FOUND && status != StatusCode::METHOD_NOT_ALLOWED
                    }
                };
                if !ok {
                    failures.push(format!(
                        "{} {} ({}) -> {}",
                        ex.method, ex.path, ex.name, status
                    ));
                }
            }
        }
        assert!(sent > 50, "too few examples were sent: {sent}");
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn every_example_path_matches_its_endpoint() {
        // An example's path must match its endpoint's pattern (segment-wise).
        fn matches(pattern: &str, path: &str) -> bool {
            let p: Vec<&str> = pattern.trim_start_matches('/').split('/').collect();
            let a: Vec<&str> = path.trim_start_matches('/').split('/').collect();
            for (i, seg) in p.iter().enumerate() {
                if seg.starts_with("{*") {
                    return a.len() > i;
                }
                match a.get(i) {
                    None => return false,
                    Some(actual) if seg.starts_with('{') => {
                        if actual.is_empty() {
                            return false;
                        }
                    }
                    Some(actual) => {
                        if actual != seg {
                            return false;
                        }
                    }
                }
            }
            p.len() == a.len()
        }
        let mut bad = Vec::new();
        for ep in all() {
            for ex in &ep.examples {
                if !matches(ep.path, ex.path_only()) {
                    bad.push(format!("{} example {}", ep.path, ex.path));
                }
                let allowed = ep.expanded_methods();
                if !ep.is_any() && !allowed.contains(&ex.method) {
                    bad.push(format!("{} example uses method {}", ep.path, ex.method));
                }
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
    }

    #[test]
    fn categories_are_grouped_in_order() {
        let groups = grouped();
        let ranks: Vec<usize> = groups.iter().map(|(n, _)| category_rank(n)).collect();
        let mut sorted = ranks.clone();
        sorted.sort();
        assert_eq!(ranks, sorted);
        let unique: BTreeSet<_> = groups.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            unique.len(),
            groups.len(),
            "category split into several groups"
        );
    }

    #[test]
    fn readme_endpoints_block_is_up_to_date() {
        let readme_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
        let readme = std::fs::read_to_string(&readme_path).expect("README.md");
        let begin = readme.find(README_BEGIN).expect("README begin marker");
        let end = readme.find(README_END).expect("README end marker");
        let current = &readme[begin + README_BEGIN.len()..end];
        let generated = format!("\n{}\n", endpoints_markdown().trim_end());
        if current != generated {
            if std::env::var("RUSTYBIN_UPDATE_README").is_ok() {
                let updated = format!(
                    "{}{}{}{}",
                    &readme[..begin],
                    README_BEGIN,
                    generated,
                    &readme[end..]
                );
                std::fs::write(&readme_path, updated).expect("write README");
                return;
            }
            panic!(
                "README.md endpoint table is stale. Regenerate with \
                 `RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints` \
                 (or `cargo run -- --print-endpoints-markdown`)."
            );
        }
    }
}
