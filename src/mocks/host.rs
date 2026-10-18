//! Host-based mock URLs (`RUSTYBIN_MOCKS_HOST=mock.example.com`): a request
//! to `petstore.mock.example.com/v1/pets` is served by the mock `petstore`,
//! as if it were `/mock/petstore/v1/pets`. Handy as a gateway upstream (no
//! path prefix to strip). Needs wildcard DNS (and a wildcard certificate for
//! HTTPS) pointing at the instance.
//!
//! The rewrite happens before routing; every other host is untouched.
//!
//! The self-service mocks domain (`RUSTYBIN_MOCKS_SELF_SERVICE_HOST`) works
//! the same way and serves nothing else: every path of `<name>.<domain>` is
//! below that mock, the bare domain redirects home (or answers `404`), and
//! deeper names answer `404`. Requests through it are tagged with
//! [`ViaMocksDomain`].

use axum::extract::{Request, State};
use axum::http::uri::PathAndQuery;
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::sync::Arc;

use super::model::valid_name;
use crate::config::Config;
use crate::session::{request_origin, RequestOrigin};

/// The mock a host name addresses (`petstore.mock.example.com` ->
/// `petstore`), if any.
pub fn mock_for_host<'h>(host: &'h str, domain: &str) -> Option<&'h str> {
    let name = host.strip_suffix(domain)?.strip_suffix('.')?;
    (valid_name(name) && !name.contains('.')).then_some(name)
}

/// Scheme, domain and port of host-based URLs as seen by this request.
pub struct HostBase {
    scheme: String,
    domain: String,
    port: Option<u16>,
}

impl HostBase {
    pub fn url(&self, name: &str, prefix: &str) -> String {
        match self.port {
            Some(p) => format!("{}://{name}.{}:{p}{prefix}", self.scheme, self.domain),
            None => format!("{}://{name}.{}{prefix}", self.scheme, self.domain),
        }
    }
}

/// The host-based URL base for a request (`None` when not configured).
pub fn base(
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
    config: &Config,
) -> Option<HostBase> {
    base_for(
        config.mocks_host.as_ref()?,
        headers,
        extensions,
        uri,
        config,
    )
}

/// [`base`] for a given domain (the self-service mocks domain).
pub fn base_for(
    domain: &str,
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
    config: &Config,
) -> Option<HostBase> {
    let origin: RequestOrigin = request_origin(headers, extensions, uri, config);
    let default = matches!(
        (origin.scheme.as_str(), origin.port),
        ("http", 80) | ("https", 443)
    );
    Some(HostBase {
        scheme: origin.scheme.clone(),
        domain: domain.to_string(),
        port: (!default).then_some(origin.port),
    })
}

/// Request extension: this request came through the self-service mocks
/// domain.
#[derive(Clone, Copy, Debug)]
pub struct ViaMocksDomain;

/// Point `req` at `/mock/{name}` (the original path below it).
fn rewrite(req: &mut Request, name: &str) {
    let path = req.uri().path();
    let new_path = if path == "/" {
        format!("/mock/{name}")
    } else {
        format!("/mock/{name}{path}")
    };
    let pq = match req.uri().query() {
        Some(q) => format!("{new_path}?{q}"),
        None => new_path,
    };
    if let Ok(pq) = pq.parse::<PathAndQuery>() {
        let mut parts = req.uri().clone().into_parts();
        parts.path_and_query = Some(pq);
        if let Ok(uri) = Uri::from_parts(parts) {
            *req.uri_mut() = uri;
        }
    }
}

/// Middleware (outside the router): rewrite mock hosts to `/mock/{name}`.
pub async fn route(State(config): State<Arc<Config>>, mut req: Request, next: Next) -> Response {
    let origin = request_origin(req.headers(), req.extensions(), req.uri(), &config);
    let ss = &config.self_service;
    if let Some(domain) = ss.host.as_deref().filter(|_| ss.enabled) {
        if origin.host == domain {
            return match &ss.home_url {
                Some(home) => {
                    let mut resp = StatusCode::FOUND.into_response();
                    if let Ok(v) = HeaderValue::from_str(home) {
                        resp.headers_mut().insert(header::LOCATION, v);
                    }
                    resp
                }
                None => mocks_domain_404("this domain only serves mocks: <name>.this-domain"),
            };
        }
        if let Some(name) = mock_for_host(&origin.host, domain) {
            rewrite(&mut req, name);
            req.extensions_mut().insert(ViaMocksDomain);
            return next.run(req).await;
        }
        if origin.host.ends_with(&format!(".{domain}")) {
            return mocks_domain_404("no mock answers on this host");
        }
    }
    if let Some(domain) = &config.mocks_host {
        if let Some(name) = mock_for_host(&origin.host, domain) {
            rewrite(&mut req, name);
        }
    }
    next.run(req).await
}

fn mocks_domain_404(message: &str) -> Response {
    let mut resp = (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "not found", "message": message })),
    )
        .into_response();
    super::restrict::harden(&mut resp);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_string, test_state_with};
    use axum::body::Body;
    use axum::http::StatusCode;
    use tower::ServiceExt;

    #[test]
    fn host_names() {
        let d = "mock.example.com";
        assert_eq!(
            mock_for_host("petstore.mock.example.com", d),
            Some("petstore")
        );
        assert_eq!(mock_for_host("mock.example.com", d), None);
        assert_eq!(mock_for_host("a.b.mock.example.com", d), None);
        assert_eq!(mock_for_host("xmock.example.com", d), None);
        assert_eq!(mock_for_host("petstore.example.com", d), None);
        let b = HostBase {
            scheme: "http".into(),
            domain: d.into(),
            port: Some(8080),
        };
        assert_eq!(
            b.url("petstore", "/v1"),
            "http://petstore.mock.example.com:8080/v1"
        );
    }

    #[tokio::test]
    async fn requests_to_mock_hosts_reach_the_mock() {
        let mut config = Config::for_tests();
        config.mocks_host = Some("mock.test".into());
        let app = crate::build_app(test_state_with(config));
        let call = |host: &str, path: &str| {
            axum::http::Request::builder()
                .uri(path)
                .header("host", host)
                .body(Body::empty())
                .expect("request")
        };
        let resp = app
            .clone()
            .oneshot(call("petstore.mock.test", "/v1/pets?limit=1"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["x-rustybin-mock"], "petstore");
        let resp = app
            .clone()
            .oneshot(call("petstore.mock.test", "/"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(resp.headers()["x-rustybin-mock"], "petstore");
        // The served spec names the host as its server (no /mock/{name}).
        let resp = app
            .clone()
            .oneshot(call("petstore.mock.test", "/_rustybin/openapi.json"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let spec: serde_json::Value = serde_json::from_str(&body_string(resp).await).expect("json");
        assert_eq!(spec["servers"][0]["url"], "http://petstore.mock.test/v1");
        // Other hosts are untouched.
        let resp = app
            .clone()
            .oneshot(call("rustybin.test", "/uuid"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = app
            .oneshot(call("rustybin.test", "/_rustybin/mocks/petstore"))
            .await
            .expect("response");
        let detail: serde_json::Value =
            serde_json::from_str(&body_string(resp).await).expect("json");
        assert_eq!(detail["host_url"], "http://petstore.mock.test/v1");
    }
}
