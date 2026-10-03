use axum::{
    extract::{Extension, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{any, get},
    Router,
};
use serde::Serialize;
use std::sync::Arc;

use crate::catalog::{category, Endpoint, Example};
use crate::cert_state::CertState;
use crate::config::Config;
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::state::AppState;
use crate::types::{AuthFailure, AuthResponse};

// ── Response types ───────────────────────────────────────────────────

#[derive(Serialize)]
struct ClientCertResponse {
    cert_pem: String,
    key_pem: String,
    usage: String,
}

#[derive(Serialize)]
struct CaCertResponse {
    ca_cert_pem: String,
    usage: String,
}

// ── Helpers ─────────────────────────────────────────────────────────

fn parse_cert_dns(pem_data: &[u8]) -> Result<(String, String), String> {
    let (_, pem) =
        x509_parser::pem::parse_x509_pem(pem_data).map_err(|e| format!("PEM parse error: {e}"))?;
    let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents)
        .map_err(|e| format!("X509 parse error: {e}"))?;

    let subject = cert.subject().to_string();
    let issuer = cert.issuer().to_string();
    Ok((subject, issuer))
}

// ── Handlers ────────────────────────────────────────────────────────

async fn mtls_handler(
    State(config): State<Arc<Config>>,
    Extension(cert_state): Extension<Arc<CertState>>,
    headers: HeaderMap,
) -> Response {
    // Header mode: read cert from configured header
    if let Some(ref header_name) = config.mtls_in_header {
        if let Some(cert_header) = headers.get(header_name.as_str()) {
            let cert_value = match cert_header.to_str() {
                Ok(v) => v,
                Err(_) => {
                    return negotiate_with_status(
                        &headers,
                        &AuthFailure {
                            authenticated: false,
                            error: "unauthorized".to_string(),
                        },
                        StatusCode::UNAUTHORIZED,
                    );
                }
            };

            // URL-decode the PEM
            let pem_str = match form_urlencoded::parse(cert_value.as_bytes())
                .map(|(k, v)| {
                    if v.is_empty() {
                        k.into_owned()
                    } else {
                        format!("{k}={v}")
                    }
                })
                .next()
            {
                Some(decoded) => decoded,
                None => cert_value.to_string(),
            };

            // If it doesn't look like PEM, try raw URL decode
            let pem_str = if pem_str.contains("BEGIN CERTIFICATE") {
                pem_str
            } else {
                urldecode(cert_value)
            };

            return match parse_cert_dns(pem_str.as_bytes()) {
                Ok((subject, issuer)) => negotiate(
                    &headers,
                    &AuthResponse {
                        authenticated: true,
                        auth_type: "mtls".to_string(),
                        username: None,
                        header: None,
                        claims: None,
                        jwt_header: None,
                        client_dn: Some(subject),
                        client_ca: Some(issuer),
                    },
                ),
                Err(e) => negotiate_with_status(
                    &headers,
                    &AuthFailure {
                        authenticated: false,
                        error: format!("invalid_certificate: {e}"),
                    },
                    StatusCode::UNAUTHORIZED,
                ),
            };
        }

        // Header configured but not present
        return negotiate_with_status(
            &headers,
            &AuthFailure {
                authenticated: false,
                error: "unauthorized".to_string(),
            },
            StatusCode::UNAUTHORIZED,
        );
    }

    // No header mode configured - check if we can detect a known client cert
    // In production, this would extract from the TLS handshake.
    // For demo purposes, return 401 with a helpful message.
    let _ = cert_state; // available for future TLS peer cert extraction
    negotiate_with_status(
        &headers,
        &crate::types::ErrorResponse {
            error: "unauthorized".to_string(),
            details: Some(
                "No client certificate found. Set RUSTYBIN_MTLS_IN_HEADER to a header name \
                 (e.g. X-Client-Cert) for header-based mTLS, or use the HTTPS listener with \
                 a client certificate."
                    .to_string(),
            ),
        },
        StatusCode::UNAUTHORIZED,
    )
}

async fn get_client_cert(
    Extension(cert_state): Extension<Arc<CertState>>,
    headers: HeaderMap,
) -> Response {
    negotiate(
        &headers,
        &ClientCertResponse {
            cert_pem: cert_state.client_cert_pem.clone(),
            key_pem: cert_state.client_key_pem.clone(),
            usage: "curl --cert client.crt --key client.key https://localhost:443/auth/mtls"
                .to_string(),
        },
    )
}

async fn get_ca_cert(
    Extension(cert_state): Extension<Arc<CertState>>,
    headers: HeaderMap,
) -> Response {
    negotiate(
        &headers,
        &CaCertResponse {
            ca_cert_pem: cert_state.ca_cert_pem.clone(),
            usage: "Configure this as the trusted CA in your API gateway".to_string(),
        },
    )
}

fn urldecode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next().unwrap_or(b'0');
            let lo = chars.next().unwrap_or(b'0');
            let hex = [hi, lo];
            if let Ok(s) = std::str::from_utf8(&hex) {
                if let Ok(val) = u8::from_str_radix(s, 16) {
                    result.push(val as char);
                    continue;
                }
            }
            result.push('%');
            result.push(hi as char);
            result.push(lo as char);
        } else if b == b'+' {
            result.push(' ');
        } else {
            result.push(b as char);
        }
    }
    result
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(state: &AppState) -> Router<AppState> {
    routes(state.certs.clone())
}

fn routes(cert_state: Arc<CertState>) -> Router<AppState> {
    Router::new()
        .route("/auth/mtls", any(mtls_handler))
        .route("/auth/mtls/get-client-cert", get(get_client_cert))
        .route("/auth/mtls/get-ca-cert", get(get_ca_cert))
        .layer(Extension(cert_state))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/auth/mtls",
            &["ANY"],
            category::AUTH_MTLS,
            "Validate the client certificate (TLS or forwarded header)",
        )
        .example(Example::get("Validate client cert", "/auth/mtls")),
        Endpoint::new(
            "/auth/mtls/get-client-cert",
            &["GET"],
            category::AUTH_MTLS,
            "Download the demo client certificate and key",
        )
        .example(Example::get(
            "Get client cert",
            "/auth/mtls/get-client-cert",
        )),
        Endpoint::new(
            "/auth/mtls/get-ca-cert",
            &["GET"],
            category::AUTH_MTLS,
            "Download the demo CA certificate",
        )
        .example(Example::get("Get CA cert", "/auth/mtls/get-ca-cert")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_config_with_header(header: Option<&str>) -> AppState {
        let mut config = crate::test_support::test_config();
        config.mtls_in_header = header.map(|s| s.to_string());
        crate::test_support::test_state_with(config)
    }

    fn test_cert_state() -> Arc<CertState> {
        // Generate a real cert state for testing
        Arc::new(CertState {
            ca_cert_pem: "-----BEGIN CERTIFICATE-----\ntest-ca\n-----END CERTIFICATE-----"
                .to_string(),
            client_cert_pem: "-----BEGIN CERTIFICATE-----\ntest-client\n-----END CERTIFICATE-----"
                .to_string(),
            client_key_pem: "-----BEGIN PRIVATE KEY-----\ntest-key\n-----END PRIVATE KEY-----"
                .to_string(),
            server_cert_pem: String::new(),
            server_key_pem: String::new(),
        })
    }

    fn test_app(header: Option<&str>) -> Router {
        routes(test_cert_state()).with_state(test_config_with_header(header))
    }

    async fn json_body(resp: axum::http::Response<Body>) -> serde_json::Value {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&body).expect("json")
    }

    #[tokio::test]
    async fn get_client_cert_returns_pem() {
        let app = test_app(None);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/mtls/get-client-cert")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert!(json["cert_pem"]
            .as_str()
            .expect("cert")
            .contains("CERTIFICATE"));
        assert!(json["key_pem"].as_str().expect("key").contains("KEY"));
        assert!(json["usage"].is_string());
    }

    #[tokio::test]
    async fn get_ca_cert_returns_pem() {
        let app = test_app(None);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/mtls/get-ca-cert")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert!(json["ca_cert_pem"]
            .as_str()
            .expect("ca")
            .contains("CERTIFICATE"));
    }

    #[tokio::test]
    async fn mtls_no_header_configured_returns_401() {
        let app = test_app(None);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/mtls")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn mtls_header_configured_but_missing_returns_401() {
        let app = test_app(Some("X-Client-Cert"));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/mtls")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn mtls_header_with_valid_cert() {
        // Generate a real cert for this test
        let cert_state = crate::cert_state::CertState::shared_for_tests();
        let config = test_config_with_header(Some("X-Client-Cert"));
        let app = routes(cert_state.clone()).with_state(config);

        // URL-encode the client cert PEM
        let encoded: String = form_urlencoded::Serializer::new(String::new())
            .append_key_only(&cert_state.client_cert_pem)
            .finish();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/mtls")
                    .header("X-Client-Cert", &encoded)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["authenticated"], true);
        assert_eq!(json["auth_type"], "mtls");
        assert!(json["client_dn"]
            .as_str()
            .expect("dn")
            .contains("demo-client"));
        assert!(json["client_ca"]
            .as_str()
            .expect("ca")
            .contains("Rustybin Demo CA"));
    }

    #[tokio::test]
    async fn mtls_header_with_invalid_cert_returns_401() {
        let app = test_app(Some("X-Client-Cert"));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/mtls")
                    .header("X-Client-Cert", "not-a-valid-cert")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn mtls_post_method_works() {
        let app = test_app(None);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/mtls")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        // Returns 401 (no cert) but the route accepts POST
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn get_client_cert_xml_negotiation() {
        let app = test_app(None);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/auth/mtls/get-client-cert")
                    .header("accept", "application/xml")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

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
