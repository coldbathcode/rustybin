use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};
use std::path::Path;

/// Shared certificate state holding the demo PKI material.
pub struct CertState {
    pub ca_cert_pem: String,
    pub client_cert_pem: String,
    pub client_key_pem: String,
}

impl CertState {
    /// Generate a demo PKI (CA, server cert, client cert).
    /// Writes server cert/key and CA cert to disk if they don't already exist.
    pub fn generate(tls_cert_path: &str, tls_key_path: &str) -> Self {
        // 1. Generate CA
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
        ca_params.distinguished_name = DistinguishedName::new();
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "Rustybin Demo CA");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca_key = KeyPair::generate().expect("ca key pair");
        let ca_cert = ca_params.self_signed(&ca_key).expect("ca self-signed cert");

        // 2. Generate server cert signed by CA
        let server_params = CertificateParams::new(vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "rustybin".to_string(),
        ])
        .expect("server params");
        let server_key = KeyPair::generate().expect("server key pair");
        let server_cert = server_params
            .signed_by(&server_key, &ca_cert, &ca_key)
            .expect("server cert signed by CA");

        // 3. Generate client cert signed by CA
        let mut client_params = CertificateParams::new(Vec::<String>::new()).expect("client params");
        client_params.distinguished_name = DistinguishedName::new();
        client_params
            .distinguished_name
            .push(DnType::CommonName, "demo-client");
        client_params
            .distinguished_name
            .push(DnType::OrganizationName, "Rustybin Demo");
        let client_key = KeyPair::generate().expect("client key pair");
        let client_cert = client_params
            .signed_by(&client_key, &ca_cert, &ca_key)
            .expect("client cert signed by CA");

        // Write cert files to disk if missing
        let cert_path = Path::new(tls_cert_path);
        let key_path = Path::new(tls_key_path);

        if let Some(parent) = cert_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if !cert_path.exists() {
            if let Err(e) = std::fs::write(cert_path, server_cert.pem()) {
                tracing::warn!("failed to write server cert: {e}");
            } else {
                tracing::info!("wrote server cert to {}", cert_path.display());
            }
        }
        if !key_path.exists() {
            if let Err(e) = std::fs::write(key_path, server_key.serialize_pem()) {
                tracing::warn!("failed to write server key: {e}");
            } else {
                tracing::info!("wrote server key to {}", key_path.display());
            }
        }

        // Always write CA cert (it's regenerated each time anyway)
        let ca_cert_path = cert_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("ca.crt");
        if let Err(e) = std::fs::write(&ca_cert_path, ca_cert.pem()) {
            tracing::warn!("failed to write CA cert: {e}");
        }

        tracing::info!("demo PKI generated: CA, server cert, client cert");

        Self {
            ca_cert_pem: ca_cert.pem(),
            client_cert_pem: client_cert.pem(),
            client_key_pem: client_key.serialize_pem(),
        }
    }
}
