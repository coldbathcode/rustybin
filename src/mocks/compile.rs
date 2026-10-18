//! A compiled, immutable mock revision: parsed spec + validated settings +
//! route table + deploy-check report. Requests only ever read it.

use chrono::{DateTime, Utc};
use std::sync::Arc;

use super::checks::{self, Report};
use super::model::{MockSettings, SettingsLimits};
use super::spec::{self, Operation, ParseLimits, ParsedSpec};

/// Instance limits applied when compiling.
#[derive(Clone, Copy, Debug)]
pub struct CompileLimits {
    pub parse: ParseLimits,
    pub settings: SettingsLimits,
}

pub struct Compiled {
    /// Store-wide revision number.
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    /// The spec text as uploaded (re-parsed on edits, exported verbatim).
    pub source: Arc<str>,
    /// Shared between revisions that only change settings.
    pub spec: Arc<ParsedSpec>,
    pub settings: MockSettings,
    routes: Vec<Route>,
    pub report: Report,
    /// Compiled for a self-service mock (restricted settings and responses).
    pub visitor: bool,
}

/// One segment of a path template.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Seg {
    Lit(String),
    /// `{name}` with optional literal text around it (`{file}.json`).
    Param {
        name: String,
        prefix: String,
        suffix: String,
    },
}

impl Seg {
    /// Sort rank: literals first, then mixed segments, then bare params.
    fn rank(&self) -> u8 {
        match self {
            Seg::Lit(_) => 0,
            Seg::Param { prefix, suffix, .. } if !prefix.is_empty() || !suffix.is_empty() => 1,
            Seg::Param { .. } => 2,
        }
    }
}

struct Route {
    op: usize,
    method: String,
    segments: Vec<Seg>,
}

/// Result of matching a request against the route table.
pub enum RouteMatch<'a> {
    Found {
        op: &'a Operation,
        params: Vec<(String, String)>,
    },
    /// The path exists, the method does not (`Allow` values).
    MethodNotAllowed(Vec<String>),
    NotFound,
}

fn split_path(path: &str) -> Vec<&str> {
    let trimmed = path.trim_start_matches('/');
    let trimmed = trimmed.strip_suffix('/').unwrap_or(trimmed);
    if trimmed.is_empty() {
        Vec::new()
    } else {
        trimmed.split('/').collect()
    }
}

fn parse_template(path: &str) -> Vec<Seg> {
    split_path(path)
        .into_iter()
        .map(|s| match (s.find('{'), s.find('}')) {
            (Some(open), Some(close)) if open < close => Seg::Param {
                name: s[open + 1..close].to_string(),
                prefix: s[..open].to_string(),
                suffix: s[close + 1..].to_string(),
            },
            _ => Seg::Lit(s.to_string()),
        })
        .collect()
}

/// Decode `%XX` escapes (invalid escapes are kept as is).
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn match_segments(template: &[Seg], actual: &[&str]) -> Option<Vec<(String, String)>> {
    if template.len() != actual.len() {
        return None;
    }
    let mut params = Vec::new();
    for (seg, raw) in template.iter().zip(actual) {
        match seg {
            Seg::Lit(lit) => {
                if lit != raw && *lit != percent_decode(raw) {
                    return None;
                }
            }
            Seg::Param {
                name,
                prefix,
                suffix,
            } => {
                let inner = raw
                    .strip_prefix(prefix.as_str())?
                    .strip_suffix(suffix.as_str())?;
                if inner.is_empty() {
                    return None;
                }
                params.push((name.clone(), percent_decode(inner)));
            }
        }
    }
    Some(params)
}

impl Compiled {
    /// Parse, validate and check a definition.
    pub fn build(
        revision: u64,
        source: Arc<str>,
        settings: MockSettings,
        limits: &CompileLimits,
    ) -> Result<Self, String> {
        let spec = spec::parse(&source, limits.parse)?;
        Self::from_parsed(revision, source, Arc::new(spec), settings, limits)
    }

    /// Compile with an already parsed spec (settings-only edits reuse the
    /// draft's spec and source instead of parsing and storing them again).
    pub fn from_parsed(
        revision: u64,
        source: Arc<str>,
        spec: Arc<ParsedSpec>,
        settings: MockSettings,
        limits: &CompileLimits,
    ) -> Result<Self, String> {
        settings.validate(&limits.settings)?;
        let mut routes: Vec<Route> = spec
            .operations
            .iter()
            .enumerate()
            .map(|(i, op)| Route {
                op: i,
                method: op.method.clone(),
                segments: parse_template(&op.path),
            })
            .collect();
        // Most specific first (literal segments beat parameters, left to
        // right); declaration order breaks ties. sort_by_key is stable.
        routes.sort_by_key(|r| r.segments.iter().map(Seg::rank).collect::<Vec<_>>());
        let mut compiled = Self {
            revision,
            created_at: Utc::now(),
            source,
            spec,
            settings,
            routes,
            report: Report::default(),
            visitor: limits.settings.visitor,
        };
        compiled.report = checks::run(&compiled);
        Ok(compiled)
    }

    /// The prefix requests must carry below the mock root (`/v1`), if any.
    pub fn path_prefix(&self) -> &str {
        if self.settings.strip_server_prefix {
            ""
        } else {
            &self.spec.server_prefix
        }
    }

    /// Match a path below the mock root (`/v1/pets/1`).
    pub fn match_route(&self, method: &str, path: &str) -> RouteMatch<'_> {
        let prefix = self.path_prefix();
        let rest = if prefix.is_empty() {
            path
        } else {
            match path.strip_prefix(prefix) {
                Some(r) if r.is_empty() || r.starts_with('/') => r,
                _ => return RouteMatch::NotFound,
            }
        };
        let actual = split_path(rest);
        let mut allowed: Vec<String> = Vec::new();
        let mut found: Option<(usize, Vec<(String, String)>)> = None;
        let mut get_fallback: Option<(usize, Vec<(String, String)>)> = None;
        for route in &self.routes {
            let Some(params) = match_segments(&route.segments, &actual) else {
                continue;
            };
            if !allowed.contains(&route.method) {
                allowed.push(route.method.clone());
            }
            if found.is_none() && route.method == method {
                found = Some((route.op, params));
            } else if get_fallback.is_none() && method == "HEAD" && route.method == "GET" {
                get_fallback = Some((route.op, params));
            }
        }
        match found.or(get_fallback) {
            Some((i, params)) => match self.spec.operations.get(i) {
                Some(op) => RouteMatch::Found { op, params },
                None => RouteMatch::NotFound,
            },
            None if allowed.is_empty() => RouteMatch::NotFound,
            None => RouteMatch::MethodNotAllowed(allowed),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mocks::model::SettingsLimits;

    pub(crate) const LIMITS: CompileLimits = CompileLimits {
        parse: ParseLimits {
            max_bytes: 1024 * 1024,
            max_operations: 500,
        },
        settings: SettingsLimits {
            max_delay_ms: 10_000,
            max_body_bytes: 64 * 1024,
            max_rules: 20,
            visitor: false,
        },
    };

    pub(crate) fn compile(text: &str, settings: MockSettings) -> Compiled {
        Compiled::build(1, Arc::from(text), settings, &LIMITS).expect("compiles")
    }

    const SPEC: &str = r#"{
      "openapi": "3.0.3", "info": {"title": "t", "version": "1"},
      "servers": [{"url": "/v1"}],
      "paths": {
        "/pets": {"get": {"operationId": "list", "responses": {"200": {"description": "ok"}}},
                  "post": {"operationId": "create", "responses": {"201": {"description": "ok"}}}},
        "/pets/{id}": {"get": {"operationId": "get", "responses": {"200": {"description": "ok"}}}},
        "/pets/mine": {"get": {"operationId": "mine", "responses": {"200": {"description": "ok"}}}},
        "/files/{name}.json": {"get": {"operationId": "file", "responses": {"200": {"description": "ok"}}}},
        "/": {"get": {"operationId": "root", "responses": {"200": {"description": "ok"}}}}
      }
    }"#;

    fn found(c: &Compiled, method: &str, path: &str) -> Option<(String, Vec<(String, String)>)> {
        match c.match_route(method, path) {
            RouteMatch::Found { op, params } => Some((op.id.clone(), params)),
            _ => None,
        }
    }

    #[test]
    fn routes_match_most_specific_first() {
        let c = compile(SPEC, MockSettings::default());
        assert_eq!(
            found(&c, "GET", "/v1/pets").map(|f| f.0).as_deref(),
            Some("list")
        );
        assert_eq!(
            found(&c, "GET", "/v1/pets/").map(|f| f.0).as_deref(),
            Some("list")
        );
        assert_eq!(
            found(&c, "GET", "/v1/pets/mine").map(|f| f.0).as_deref(),
            Some("mine")
        );
        let (id, params) = found(&c, "GET", "/v1/pets/a%20b").expect("get");
        assert_eq!(id, "get");
        assert_eq!(params, vec![("id".to_string(), "a b".to_string())]);
        let (id, params) = found(&c, "GET", "/v1/files/report.json").expect("file");
        assert_eq!(id, "file");
        assert_eq!(params[0].1, "report");
        assert_eq!(
            found(&c, "GET", "/v1").map(|f| f.0).as_deref(),
            Some("root")
        );
        assert_eq!(
            found(&c, "HEAD", "/v1/pets").map(|f| f.0).as_deref(),
            Some("list")
        );
        assert!(matches!(
            c.match_route("GET", "/pets"),
            RouteMatch::NotFound
        ));
        assert!(matches!(
            c.match_route("GET", "/v1x/pets"),
            RouteMatch::NotFound
        ));
        assert!(matches!(
            c.match_route("GET", "/v1/nope"),
            RouteMatch::NotFound
        ));
        match c.match_route("DELETE", "/v1/pets") {
            RouteMatch::MethodNotAllowed(m) => assert_eq!(m, vec!["GET", "POST"]),
            _ => panic!("expected 405"),
        }
    }

    #[test]
    fn server_prefix_can_be_stripped() {
        let settings = MockSettings {
            strip_server_prefix: true,
            ..MockSettings::default()
        };
        let c = compile(SPEC, settings);
        assert_eq!(
            found(&c, "GET", "/pets").map(|f| f.0).as_deref(),
            Some("list")
        );
        assert!(found(&c, "GET", "/v1/pets").is_none());
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
    }
}
