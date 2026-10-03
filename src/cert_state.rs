use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// Shared certificate state holding the demo PKI material.
///
/// Generation is purely in-memory; writing files is a separate, explicit step
/// ([`CertState::write_files`]) performed only by the server at startup, so
/// tests never touch the repository's `certs/` directory.
pub struct CertState {
    pub ca_cert_pem: String,
    pub client_cert_pem: String,
    pub client_key_pem: String,
    /// Server certificate (signed by the demo CA), used for HTTPS when no
    /// certificate files are available.
    pub server_cert_pem: String,
    pub server_key_pem: String,
}

impl CertState {
    /// Generate a demo PKI (CA, server cert, client cert) in memory.
    ///
    /// Only called at startup, never on a request path.
    pub fn generate() -> Self {
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
        let mut client_params =
            CertificateParams::new(Vec::<String>::new()).expect("client params");
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

        tracing::info!("demo PKI generated: CA, server cert, client cert");

        Self {
            ca_cert_pem: ca_cert.pem(),
            client_cert_pem: client_cert.pem(),
            client_key_pem: client_key.serialize_pem(),
            server_cert_pem: server_cert.pem(),
            server_key_pem: server_key.serialize_pem(),
        }
    }

    /// A process-wide demo PKI generated once and shared by every caller (tests).
    #[doc(hidden)]
    pub fn shared_for_tests() -> Arc<CertState> {
        static SHARED: OnceLock<Arc<CertState>> = OnceLock::new();
        SHARED
            .get_or_init(|| Arc::new(CertState::generate()))
            .clone()
    }

    /// Best-effort: write the server cert/key if the files are missing, and
    /// always (re)write `ca.crt` next to the server cert. Failures are logged.
    pub fn write_files(&self, tls_cert_path: &str, tls_key_path: &str) {
        let cert_path = Path::new(tls_cert_path);
        let key_path = Path::new(tls_key_path);

        if let Some(parent) = cert_path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }

        if !cert_path.exists() {
            match std::fs::write(cert_path, &self.server_cert_pem) {
                Ok(()) => tracing::info!("wrote server cert to {}", cert_path.display()),
                Err(e) => tracing::warn!("failed to write server cert: {e}"),
            }
        }
        if !key_path.exists() {
            match std::fs::write(key_path, &self.server_key_pem) {
                Ok(()) => tracing::info!("wrote server key to {}", key_path.display()),
                Err(e) => tracing::warn!("failed to write server key: {e}"),
            }
        }

        let ca_cert_path = cert_path.parent().unwrap_or(Path::new(".")).join("ca.crt");
        if let Err(e) = std::fs::write(&ca_cert_path, &self.ca_cert_pem) {
            tracing::warn!("failed to write CA cert: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_state_is_reused_and_in_memory() {
        let a = CertState::shared_for_tests();
        let b = CertState::shared_for_tests();
        assert!(Arc::ptr_eq(&a, &b));
        assert!(a.ca_cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(a.server_key_pem.contains("PRIVATE KEY"));
    }

    #[test]
    fn write_files_into_tempdir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cert = dir.path().join("sub/server.crt");
        let key = dir.path().join("sub/server.key");
        let state = CertState::shared_for_tests();
        state.write_files(
            cert.to_str().expect("utf8 path"),
            key.to_str().expect("utf8 path"),
        );
        assert!(cert.exists());
        assert!(key.exists());
        assert!(dir.path().join("sub/ca.crt").exists());
    }
}
