//! `/auth/mtls*`: client certificate authentication.
//!
//! On the HTTPS listener the TLS peer certificate (already verified against
//! the demo CA during the handshake) is used. Otherwise, when
//! `RUSTYBIN_MTLS_IN_HEADER` names a header, a gateway that terminated mTLS
//! can forward the client certificate there (URL-encoded PEM, raw PEM or
//! base64 DER); it is verified against the demo CA and its validity period.

use axum::{
    extract::{Extension, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{any, get},
    Router,
};
use base64::Engine;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::catalog::{category, Endpoint, Example};
use crate::cert_state::CertState;
use crate::config::Config;
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::server::TlsConnectionInfo;
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

const PEM_BEGIN: &str = "-----BEGIN CERTIFICATE-----";
const PEM_END: &str = "-----END CERTIFICATE-----";

/// Percent-decode (`%XX`) into bytes, then UTF-8. `+` is kept literally
/// because it is a base64 character in PEM bodies.
fn percent_decode(input: &str) -> Result<String, &'static str> {
    String::from_utf8(crate::types::percent_decode(input, false))
        .map_err(|_| "certificate header is not UTF-8 after URL decoding")
}

/// DER of a certificate forwarded in a header: URL-encoded or raw PEM
/// (newlines may be replaced by spaces), or bare base64 DER.
fn decode_header_cert(value: &str) -> Result<Vec<u8>, &'static str> {
    let decoded = percent_decode(value.trim())?;
    // Form encoding turns the spaces of the PEM markers into '+'.
    let text = decoded
        .replace("-----BEGIN+CERTIFICATE-----", PEM_BEGIN)
        .replace("-----END+CERTIFICATE-----", PEM_END);
    let body = match text.find(PEM_BEGIN) {
        Some(start) => {
            let after = &text[start + PEM_BEGIN.len()..];
            let end = after
                .find(PEM_END)
                .ok_or("PEM certificate has no END marker")?;
            &after[..end]
        }
        None => text.as_str(),
    };
    let b64: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|_| "certificate is neither PEM nor base64 DER")
}

/// Subject, issuer and details of a DER certificate.
fn describe_cert(der: &[u8]) -> Result<(String, String, serde_json::Value), &'static str> {
    let (_, cert) =
        x509_parser::parse_x509_certificate(der).map_err(|_| "not a valid X.509 certificate")?;
    let fingerprint: String = Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    let details = serde_json::json!({
        "serial": cert.raw_serial_as_string(),
        "not_before": cert.validity().not_before.to_rfc2822().unwrap_or_default(),
        "not_after": cert.validity().not_after.to_rfc2822().unwrap_or_default(),
        "sha256_fingerprint": fingerprint,
    });
    Ok((
        cert.subject().to_string(),
        cert.issuer().to_string(),
        details,
    ))
}

fn unauthorized(headers: &HeaderMap, error: String) -> Response {
    negotiate_with_status(headers, &AuthFailure::new(error), StatusCode::UNAUTHORIZED)
}

fn authenticated(
    headers: &HeaderMap,
    der: &[u8],
    source: &str,
    header_name: Option<&str>,
) -> Response {
    let (subject, issuer, mut details) = match describe_cert(der) {
        Ok(d) => d,
        Err(e) => return unauthorized(headers, format!("invalid_certificate: {e}")),
    };
    if let Some(obj) = details.as_object_mut() {
        obj.insert("source".into(), source.into());
    }
    negotiate(
        headers,
        &AuthResponse {
            header: header_name.map(String::from),
            claims: Some(details),
            client_dn: Some(subject),
            client_ca: Some(issuer),
            ..AuthResponse::ok("mtls")
        },
    )
}

// ── Handlers ────────────────────────────────────────────────────────

async fn mtls_handler(
    State(config): State<Arc<Config>>,
    Extension(cert_state): Extension<Arc<CertState>>,
    tls: Option<Extension<TlsConnectionInfo>>,
    headers: HeaderMap,
) -> Response {
    // 1. TLS peer certificate (verified against the demo CA in the handshake).
    if let Some(der) = tls.as_ref().and_then(|t| t.peer_certificate.clone()) {
        return authenticated(&headers, &der, "tls", None);
    }

    // 2. Header mode: certificate forwarded by a gateway.
    if let Some(header_name) = config.mtls_in_header.as_deref() {
        let Some(value) = headers.get(header_name) else {
            return unauthorized(
                &headers,
                format!("unauthorized: no client certificate in the {header_name} header"),
            );
        };
        let Ok(value) = value.to_str() else {
            return unauthorized(&headers, "invalid_certificate: header is not ASCII".into());
        };
        let der = match decode_header_cert(value) {
            Ok(der) => der,
            Err(e) => return unauthorized(&headers, format!("invalid_certificate: {e}")),
        };
        if let Err(e) = cert_state.verify_client_cert(&der) {
            return unauthorized(
                &headers,
                format!("untrusted_certificate: not a valid certificate from the demo CA ({e})"),
            );
        }
        return authenticated(&headers, &der, "header", Some(header_name));
    }

    negotiate_with_status(
        &headers,
        &crate::types::ErrorResponse {
            error: "unauthorized".to_string(),
            details: Some(
                "No client certificate found. Call the HTTPS listener with a client certificate \
                 issued by the demo CA (/auth/mtls/get-client-cert), or set \
                 RUSTYBIN_MTLS_IN_HEADER to the header your gateway forwards the certificate in \
                 (e.g. X-Client-Cert)."
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
            usage: "curl --cacert ca.crt --cert client.crt --key client.key https://localhost/auth/mtls"
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

// ── Router ──────────────────────────────────────────────────────────

pub fn router(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/auth/mtls", any(mtls_handler))
        .route("/auth/mtls/get-client-cert", get(get_client_cert))
        .route("/auth/mtls/get-ca-cert", get(get_ca_cert))
        .layer(Extension(state.certs.clone()))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/auth/mtls",
            &["ANY"],
            category::AUTH_MTLS,
            "Validate the client certificate (TLS peer certificate, or the RUSTYBIN_MTLS_IN_HEADER header)",
        )
        .description(
            "On the HTTPS listener the verified TLS client certificate is used. Otherwise, when \
             RUSTYBIN_MTLS_IN_HEADER is set, the certificate forwarded by the gateway in that header \
             (URL-encoded PEM, PEM or base64 DER) must be issued by the demo CA and currently valid.",
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
            "Download the demo CA certificate (persisted across restarts when the cert dir is writable)",
        )
        .example(Example::get("Get CA cert", "/auth/mtls/get-ca-cert")),
    ]
}

pub fn openapi_paths() -> serde_json::Value {
    use serde_json::json;
    json!({
        "/auth/mtls": { "get": {
            "tags": ["Auth"],
            "summary": "Mutual TLS authentication",
            "description": "Uses the TLS client certificate on the HTTPS listener (verified against the demo CA during the handshake). Otherwise, when RUSTYBIN_MTLS_IN_HEADER is set, reads the certificate forwarded in that header (URL-encoded PEM, PEM or base64 DER) and verifies it was issued by the demo CA and is within its validity period.",
            "operationId": "getMtls",
            "responses": {
                "200": { "description": "Authenticated", "content": crate::openapi::json_xml_content(json!({ "$ref": "#/components/schemas/AuthResponse" })) },
                "401": { "description": "Missing, invalid or untrusted client certificate" }
            }
        }},
        "/auth/mtls/get-client-cert": { "get": {
            "tags": ["Auth"],
            "summary": "Get demo client certificate",
            "description": "Returns a demo client certificate (CN demo-client, issued by the demo CA) and its private key.",
            "operationId": "getMtlsClientCert",
            "responses": {
                "200": { "description": "Client cert and key PEM",
                    "content": crate::openapi::json_xml_content(json!({
                        "type": "object",
                        "properties": {
                            "cert_pem": { "type": "string" },
                            "key_pem": { "type": "string" },
                            "usage": { "type": "string" }
                        }
                    }))
                }
            }
        }},
        "/auth/mtls/get-ca-cert": { "get": {
            "tags": ["Auth"],
            "summary": "Get demo CA certificate",
            "description": "Returns the demo CA certificate for configuring trust in your API gateway. The CA is persisted next to the TLS certificate when that directory is writable.",
            "operationId": "getMtlsCaCert",
            "responses": {
                "200": { "description": "CA certificate PEM",
                    "content": crate::openapi::json_xml_content(json!({
                        "type": "object",
                        "properties": {
                            "ca_cert_pem": { "type": "string" },
                            "usage": { "type": "string" }
                        }
                    }))
                }
            }
        }}
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_json, module_app_with_config, test_config};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn app(header: Option<&str>) -> Router {
        let mut config = test_config();
        config.mtls_in_header = header.map(String::from);
        module_app_with_config(config, router)
    }

    fn request(header: Option<(&str, &str)>) -> Request<Body> {
        let mut b = Request::builder().uri("/auth/mtls");
        if let Some((k, v)) = header {
            b = b.header(k, v);
        }
        b.body(Body::empty()).expect("request")
    }

    fn url_encode(s: &str) -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
    }

    #[tokio::test]
    async fn get_client_and_ca_cert() {
        let resp = app(None)
            .oneshot(
                Request::builder()
                    .uri("/auth/mtls/get-client-cert")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert!(json["cert_pem"]
            .as_str()
            .unwrap_or("")
            .contains("CERTIFICATE"));
        assert!(json["key_pem"].as_str().unwrap_or("").contains("KEY"));

        let resp = app(None)
            .oneshot(
                Request::builder()
                    .uri("/auth/mtls/get-ca-cert")
                    .header("accept", "application/xml")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/xml")
        );
    }

    #[tokio::test]
    async fn no_certificate_is_401() {
        let resp = app(None).oneshot(request(None)).await.expect("response");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = app(Some("X-Client-Cert"))
            .oneshot(request(None))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn header_mode_accepts_demo_ca_cert_in_several_encodings() {
        let certs = CertState::shared_for_tests();
        let pem = certs.client_cert_pem.clone();
        let form: String = form_urlencoded::Serializer::new(String::new())
            .append_key_only(&pem)
            .finish();
        let der = crate::cert_state::pem_to_der(&pem).expect("der");
        let encodings = [
            url_encode(&pem),
            form,
            pem.replace('\n', " "),
            base64::engine::general_purpose::STANDARD.encode(der),
        ];
        for value in encodings {
            let resp = app(Some("X-Client-Cert"))
                .oneshot(request(Some(("X-Client-Cert", &value))))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK, "{value}");
            let json = body_json(resp).await;
            assert_eq!(json["auth_type"], "mtls");
            assert_eq!(json["claims"]["source"], "header");
            assert!(json["client_dn"]
                .as_str()
                .unwrap_or("")
                .contains("demo-client"));
            assert!(json["client_ca"]
                .as_str()
                .unwrap_or("")
                .contains("Rustybin Demo CA"));
        }
    }

    #[tokio::test]
    async fn header_mode_rejects_foreign_ca_and_garbage() {
        // Same subject names, but issued by a different CA.
        let foreign = CertState::generate();
        for value in [
            url_encode(&foreign.client_cert_pem),
            "not-a-valid-cert".into(),
        ] {
            let resp = app(Some("X-Client-Cert"))
                .oneshot(request(Some(("X-Client-Cert", &value))))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn tls_peer_certificate_is_used_first() {
        let certs = CertState::shared_for_tests();
        let der = crate::cert_state::pem_to_der(&certs.client_cert_pem).expect("der");
        let mut req = request(None);
        req.extensions_mut().insert(TlsConnectionInfo {
            peer_certificate: Some(Arc::new(der)),
        });
        let resp = app(Some("X-Client-Cert"))
            .oneshot(req)
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["claims"]["source"], "tls");
    }

    #[test]
    fn percent_decode_handles_multibyte_utf8() {
        assert_eq!(
            percent_decode("caf%C3%A9%20%2B+x").expect("utf8"),
            "café ++x"
        );
        assert_eq!(percent_decode("100%").expect("utf8"), "100%");
        assert_eq!(percent_decode("%zz%4").expect("utf8"), "%zz%4");
        assert!(percent_decode("%FF").is_err());
    }
}
