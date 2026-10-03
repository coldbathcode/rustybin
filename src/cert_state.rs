//! Demo PKI: a CA, a server certificate for the HTTPS listener and a client
//! certificate for mTLS demos.
//!
//! The CA is persisted next to the configured TLS certificate when that
//! directory is writable, so client certificates issued by an earlier run
//! keep working after a restart. It is stored as `ca.crt` + `ca.key`, unless
//! those names hold another CA (for example your own): Rustybin never
//! overwrites a CA it did not create (recognised by the subject
//! [`DEMO_CA_CN`]) and then uses `rustybin-demo-ca.crt` +
//! `rustybin-demo-ca.key` instead. Tests use [`CertState::shared_for_tests`]
//! / [`CertState::generate`], which never touch the filesystem.

use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::ClientCertVerifier;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use crate::config::Config;

/// Common name of the demo CA.
pub const DEMO_CA_CN: &str = "Rustybin Demo CA";

/// File stems (`<stem>.crt` + `<stem>.key`) where the demo CA may be
/// persisted, in order of preference. The second one is used when the first
/// holds a CA that is not the demo CA.
pub const DEMO_CA_STEMS: &[&str] = &["ca", "rustybin-demo-ca"];

/// Shared certificate state holding the demo PKI material.
pub struct CertState {
    pub ca_cert_pem: String,
    pub client_cert_pem: String,
    pub client_key_pem: String,
    /// Server certificate (signed by the demo CA), used for HTTPS when no
    /// certificate files are available.
    pub server_cert_pem: String,
    pub server_key_pem: String,
    /// Client certificate verifier trusting only the demo CA (optional
    /// client auth). Shared by the HTTPS listener and header-mode mTLS.
    client_verifier: Arc<dyn ClientCertVerifier>,
    /// Server certificate verifier trusting only the demo CA.
    server_verifier: Arc<dyn ServerCertVerifier>,
}

fn ca_params() -> CertificateParams {
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, DEMO_CA_CN);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    params
}

/// A CA ready to sign: the PEM we publish plus the signing material.
struct Ca {
    cert_pem: String,
    cert: Certificate,
    key: KeyPair,
}

impl Ca {
    /// Only called at startup.
    fn generate() -> Self {
        let key = KeyPair::generate().expect("ca key pair");
        let cert = ca_params().self_signed(&key).expect("ca self-signed cert");
        Self {
            cert_pem: cert.pem(),
            cert,
            key,
        }
    }

    /// Load `<stem>.crt` + `<stem>.key`; `None` unless both parse, the key
    /// matches the certificate and the subject is the demo CA.
    fn load(dir: &Path, stem: &str) -> Option<Self> {
        let cert_pem = std::fs::read_to_string(dir.join(format!("{stem}.crt"))).ok()?;
        let key_pem = std::fs::read_to_string(dir.join(format!("{stem}.key"))).ok()?;
        let key = KeyPair::from_pem(&key_pem).ok()?;
        let der = pem_to_der(&cert_pem)?;
        let (_, parsed) = x509_parser::parse_x509_certificate(&der).ok()?;
        if parsed.public_key().raw != key.public_key_der().as_slice() || !parsed.is_ca() {
            return None;
        }
        // Re-create an issuer with the same subject and key: certificates it
        // signs chain to the persisted ca.crt.
        let cert = ca_params().self_signed(&key).ok()?;
        let (_, recreated) = x509_parser::parse_x509_certificate(cert.der()).ok()?;
        if recreated.subject().as_raw() != parsed.subject().as_raw() {
            return None;
        }
        Some(Self {
            cert_pem,
            cert,
            key,
        })
    }

    /// Best-effort write of `<stem>.crt` and `<stem>.key` (0600).
    fn persist(&self, dir: &Path, stem: &str) {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!("demo CA not persisted ({}): {e}", dir.display());
            return;
        }
        let key_path = dir.join(format!("{stem}.key"));
        let cert_path = dir.join(format!("{stem}.crt"));
        let result = write_private(&key_path, self.key.serialize_pem().as_bytes())
            .and_then(|()| std::fs::write(&cert_path, &self.cert_pem));
        match result {
            Ok(()) => tracing::info!("demo CA written to {}", cert_path.display()),
            Err(e) => {
                let _ = std::fs::remove_file(&key_path);
                tracing::warn!("demo CA not persisted ({}): {e}", dir.display());
            }
        }
    }
}

/// What a `<stem>.crt` / `<stem>.key` pair in the certs directory holds.
enum CaSlot {
    /// A consistent demo CA, ready to use.
    Demo(Box<Ca>),
    /// Nothing, or a demo CA certificate whose key is missing or does not
    /// match: Rustybin's own files, safe to (re)write.
    Free,
    /// Something Rustybin did not create (another CA, an unreadable file, a
    /// key without a certificate): never touched.
    Foreign,
}

/// Whether a PEM certificate is a CA certificate whose subject is the demo
/// CA, i.e. one Rustybin generated.
fn is_demo_ca_cert(cert_pem: &str) -> bool {
    let Some(der) = pem_to_der(cert_pem) else {
        return false;
    };
    x509_parser::parse_x509_certificate(&der).is_ok_and(|(_, c)| {
        c.is_ca()
            && c.subject()
                .iter_common_name()
                .any(|cn| cn.as_str() == Ok(DEMO_CA_CN))
    })
}

fn inspect_slot(dir: &Path, stem: &str) -> CaSlot {
    let cert_path = dir.join(format!("{stem}.crt"));
    let key_path = dir.join(format!("{stem}.key"));
    match (cert_path.exists(), key_path.exists()) {
        (false, false) => CaSlot::Free,
        (false, true) => CaSlot::Foreign,
        (true, _) => {
            let ours = std::fs::read_to_string(&cert_path).is_ok_and(|pem| is_demo_ca_cert(&pem));
            if !ours {
                CaSlot::Foreign
            } else if let Some(ca) = Ca::load(dir, stem) {
                CaSlot::Demo(Box::new(ca))
            } else {
                tracing::warn!(
                    "{} is a demo CA without a matching key, replacing it",
                    cert_path.display()
                );
                CaSlot::Free
            }
        }
    }
}

/// Write a file readable only by the owner (on Unix).
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(contents)
}

/// DER of the first certificate in a PEM string.
pub fn pem_to_der(pem: &str) -> Option<Vec<u8>> {
    rustls_pemfile::certs(&mut pem.as_bytes())
        .next()?
        .ok()
        .map(|c| c.as_ref().to_vec())
}

/// Whether `key_pem` is the private key of `cert_pem`. `None` when either
/// cannot be parsed here (rustls then has the final word).
fn key_matches_cert(cert_pem: &[u8], key_pem: &[u8]) -> Option<bool> {
    let cert_der = pem_to_der(std::str::from_utf8(cert_pem).ok()?)?;
    let key = KeyPair::from_pem(std::str::from_utf8(key_pem).ok()?).ok()?;
    let (_, cert) = x509_parser::parse_x509_certificate(&cert_der).ok()?;
    Some(cert.public_key().raw == key.public_key_der().as_slice())
}

impl CertState {
    /// Generate a fresh in-memory demo PKI (CA, server cert, client cert).
    /// Only called at startup or in tests, never on a request path.
    pub fn generate() -> Self {
        Self::from_ca(Ca::generate())
    }

    /// The production PKI: the CA is loaded from (or persisted to) the
    /// directory of the configured TLS certificate; with no TLS path the
    /// PKI is in-memory only.
    pub fn for_config(config: &Config) -> Self {
        if config.tls_cert.is_empty() {
            return Self::generate();
        }
        let dir = Path::new(&config.tls_cert)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Self::load_or_generate(dir)
    }

    /// Load the demo CA persisted in `dir` (see [`DEMO_CA_STEMS`]), or
    /// generate one and persist it (best effort) in the first slot that is
    /// free or already Rustybin's. Files that are not the demo CA are never
    /// overwritten; when no slot is usable the CA stays in memory.
    pub fn load_or_generate(dir: &Path) -> Self {
        let mut free = None;
        for stem in DEMO_CA_STEMS {
            match inspect_slot(dir, stem) {
                CaSlot::Demo(ca) => {
                    tracing::info!("demo CA loaded from {}", dir.join(format!("{stem}.crt")).display());
                    return Self::from_ca(*ca);
                }
                CaSlot::Free => {
                    free.get_or_insert(*stem);
                }
                CaSlot::Foreign => tracing::warn!(
                    "{dir}/{stem}.crt / {stem}.key are not the Rustybin demo CA (subject CN {DEMO_CA_CN}); \
                     leaving them untouched",
                    dir = dir.display()
                ),
            }
        }
        let ca = Ca::generate();
        match free {
            Some(stem) => ca.persist(dir, stem),
            None => tracing::warn!(
                "no free file name for the demo CA in {}; it is kept in memory only, so client \
                 certificates it issues stop working after a restart",
                dir.display()
            ),
        }
        Self::from_ca(ca)
    }

    fn from_ca(ca: Ca) -> Self {
        // Server cert signed by the CA.
        let mut server_params = CertificateParams::new(vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
            "rustybin".to_string(),
        ])
        .expect("server params");
        server_params
            .distinguished_name
            .push(DnType::CommonName, "rustybin");
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server_key = KeyPair::generate().expect("server key pair");
        let server_cert = server_params
            .signed_by(&server_key, &ca.cert, &ca.key)
            .expect("server cert signed by CA");

        // Client cert signed by the CA.
        let mut client_params = CertificateParams::default();
        client_params.distinguished_name = DistinguishedName::new();
        client_params
            .distinguished_name
            .push(DnType::CommonName, "demo-client");
        client_params
            .distinguished_name
            .push(DnType::OrganizationName, "Rustybin Demo");
        client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let client_key = KeyPair::generate().expect("client key pair");
        let client_cert = client_params
            .signed_by(&client_key, &ca.cert, &ca.key)
            .expect("client cert signed by CA");

        let ca_der =
            CertificateDer::from(pem_to_der(&ca.cert_pem).expect("CA certificate PEM decodes"));
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(ca_der)
            .expect("CA certificate is a valid trust anchor");
        let roots = Arc::new(roots);
        let client_verifier = rustls::server::WebPkiClientVerifier::builder(roots.clone())
            .allow_unauthenticated()
            .build()
            .expect("client certificate verifier");
        let server_verifier = rustls::client::WebPkiServerVerifier::builder(roots)
            .build()
            .expect("server certificate verifier");

        tracing::info!("demo PKI ready: CA, server cert, client cert");

        Self {
            ca_cert_pem: ca.cert_pem,
            client_cert_pem: client_cert.pem(),
            client_key_pem: client_key.serialize_pem(),
            server_cert_pem: server_cert.pem(),
            server_key_pem: server_key.serialize_pem(),
            client_verifier,
            server_verifier,
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

    /// Verifier for the HTTPS listener: client certificates are optional,
    /// but when presented they must chain to the demo CA.
    pub fn client_verifier(&self) -> Arc<dyn ClientCertVerifier> {
        self.client_verifier.clone()
    }

    /// Verify a client certificate (DER) against the demo CA: signature
    /// chain, validity period and (when present) the clientAuth usage.
    pub fn verify_client_cert(&self, der: &[u8]) -> Result<(), String> {
        let cert = CertificateDer::from(der.to_vec());
        self.client_verifier
            .verify_client_cert(&cert, &[], UnixTime::now())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Whether a server certificate (PEM) chains to the current demo CA.
    fn server_cert_chains_to_ca(&self, cert_pem: &[u8]) -> bool {
        let Some(der) = std::str::from_utf8(cert_pem).ok().and_then(pem_to_der) else {
            return false;
        };
        let Ok(name) = ServerName::try_from("localhost") else {
            return false;
        };
        self.server_verifier
            .verify_server_cert(&CertificateDer::from(der), &[], &name, &[], UnixTime::now())
            .is_ok()
    }

    /// Whether a PEM certificate claims to be issued by the demo CA.
    fn issued_by_demo_ca_name(cert_pem: &[u8]) -> bool {
        let Some(der) = std::str::from_utf8(cert_pem).ok().and_then(pem_to_der) else {
            return false;
        };
        x509_parser::parse_x509_certificate(&der)
            .map(|(_, c)| {
                c.issuer()
                    .iter_common_name()
                    .any(|cn| cn.as_str() == Ok(DEMO_CA_CN))
            })
            .unwrap_or(false)
    }

    fn in_memory_server_pair(&self) -> (Vec<u8>, Vec<u8>) {
        (
            self.server_cert_pem.clone().into_bytes(),
            self.server_key_pem.clone().into_bytes(),
        )
    }

    /// Write the generated server cert and key together (key first, 0600);
    /// on any failure neither file is left behind.
    fn write_server_pair(&self, cert_path: &Path, key_path: &Path) -> bool {
        if let Some(parent) = cert_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            let _ = std::fs::create_dir_all(parent);
        }
        let result = write_private(key_path, self.server_key_pem.as_bytes())
            .and_then(|()| std::fs::write(cert_path, &self.server_cert_pem));
        match result {
            Ok(()) => {
                tracing::info!(
                    "wrote demo server cert and key to {} / {}",
                    cert_path.display(),
                    key_path.display()
                );
                true
            }
            Err(e) => {
                let _ = std::fs::remove_file(key_path);
                let _ = std::fs::remove_file(cert_path);
                tracing::warn!("demo server cert not written: {e}");
                false
            }
        }
    }

    /// The HTTPS certificate and key (PEM).
    ///
    /// - both files exist: they are used, unless they are a stale demo pair
    ///   from an older CA (then replaced) or the key does not match (error);
    /// - neither exists: the generated pair is written (best effort) and used;
    /// - only one exists: nothing is written, the in-memory pair is used.
    ///
    /// Empty paths always use the in-memory pair.
    pub fn server_tls_material(
        &self,
        cert_path: &str,
        key_path: &str,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        if cert_path.is_empty() || key_path.is_empty() {
            return Ok(self.in_memory_server_pair());
        }
        let (cert_file, key_file) = (Path::new(cert_path), Path::new(key_path));
        match (cert_file.exists(), key_file.exists()) {
            (true, true) => {
                let cert = std::fs::read(cert_file).map_err(|e| e.to_string())?;
                let key = std::fs::read(key_file).map_err(|e| e.to_string())?;
                if Self::issued_by_demo_ca_name(&cert) && !self.server_cert_chains_to_ca(&cert) {
                    tracing::warn!(
                        "{} was issued by an older demo CA, replacing it",
                        cert_file.display()
                    );
                    self.write_server_pair(cert_file, key_file);
                    return Ok(self.in_memory_server_pair());
                }
                if key_matches_cert(&cert, &key) == Some(false) {
                    return Err(format!(
                        "{} and {} are not a matching certificate / key pair",
                        cert_file.display(),
                        key_file.display()
                    ));
                }
                Ok((cert, key))
            }
            (false, false) => {
                self.write_server_pair(cert_file, key_file);
                Ok(self.in_memory_server_pair())
            }
            _ => {
                tracing::warn!(
                    "only one of {} / {} exists; using the generated demo certificate",
                    cert_file.display(),
                    key_file.display()
                );
                Ok(self.in_memory_server_pair())
            }
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
    fn client_cert_verifies_against_demo_ca_only() {
        let state = CertState::shared_for_tests();
        let der = pem_to_der(&state.client_cert_pem).expect("der");
        assert!(state.verify_client_cert(&der).is_ok());
        // A certificate from another CA is rejected.
        let other = CertState::generate();
        let other_der = pem_to_der(&other.client_cert_pem).expect("der");
        assert!(state.verify_client_cert(&other_der).is_err());
    }

    #[test]
    fn ca_persists_across_restarts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = CertState::load_or_generate(dir.path());
        assert!(dir.path().join("ca.crt").exists());
        assert!(dir.path().join("ca.key").exists());
        let second = CertState::load_or_generate(dir.path());
        assert_eq!(first.ca_cert_pem, second.ca_cert_pem);
        // A client cert issued by the first run is still trusted.
        let der = pem_to_der(&first.client_cert_pem).expect("der");
        assert!(second.verify_client_cert(&der).is_ok());

        // An inconsistent key makes the CA regenerate.
        let other = KeyPair::generate().expect("key");
        std::fs::write(dir.path().join("ca.key"), other.serialize_pem()).expect("write");
        let third = CertState::load_or_generate(dir.path());
        assert_ne!(third.ca_cert_pem, first.ca_cert_pem);
    }

    /// A CA that is not the demo CA (a user's own), as PEM (cert, key).
    fn foreign_ca() -> (String, String) {
        let key = KeyPair::generate().expect("key");
        let mut params = ca_params();
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, "My Company Root CA");
        let cert = params.self_signed(&key).expect("cert");
        (cert.pem(), key.serialize_pem())
    }

    #[test]
    fn foreign_ca_files_are_never_overwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (cert, key) = foreign_ca();
        std::fs::write(dir.path().join("ca.crt"), &cert).expect("write");
        std::fs::write(dir.path().join("ca.key"), &key).expect("write");

        let first = CertState::load_or_generate(dir.path());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("ca.crt")).expect("read"),
            cert
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("ca.key")).expect("read"),
            key
        );
        assert_ne!(first.ca_cert_pem, cert);
        assert!(is_demo_ca_cert(&first.ca_cert_pem));
        let demo_crt = dir.path().join("rustybin-demo-ca.crt");
        assert!(demo_crt.exists() && dir.path().join("rustybin-demo-ca.key").exists());

        // A restart reuses the demo CA from the alternate pair.
        let second = CertState::load_or_generate(dir.path());
        assert_eq!(first.ca_cert_pem, second.ca_cert_pem);
        let der = pem_to_der(&first.client_cert_pem).expect("der");
        assert!(second.verify_client_cert(&der).is_ok());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("ca.crt")).expect("read"),
            cert
        );
    }

    #[test]
    fn unknown_files_are_left_alone() {
        // A lone key and an unparseable certificate are not ours either.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("ca.key"), "my key").expect("write");
        std::fs::write(dir.path().join("rustybin-demo-ca.crt"), "garbage").expect("write");
        let state = CertState::load_or_generate(dir.path());
        assert!(is_demo_ca_cert(&state.ca_cert_pem));
        assert!(!dir.path().join("ca.crt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("ca.key")).expect("read"),
            "my key"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("rustybin-demo-ca.crt")).expect("read"),
            "garbage"
        );
        assert!(!dir.path().join("rustybin-demo-ca.key").exists());
    }

    #[test]
    fn server_material_written_as_a_pair_and_reused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cert = dir.path().join("sub/server.crt");
        let key = dir.path().join("sub/server.key");
        let (c, k) = (cert.to_str().expect("utf8"), key.to_str().expect("utf8"));
        let state = CertState::load_or_generate(&dir.path().join("sub"));
        let (pem, _) = state.server_tls_material(c, k).expect("material");
        assert!(cert.exists() && key.exists());
        assert_eq!(pem, state.server_cert_pem.as_bytes());
        // Restart with the persisted CA: the files are kept.
        let restarted = CertState::load_or_generate(&dir.path().join("sub"));
        let (pem2, key2) = restarted.server_tls_material(c, k).expect("material");
        assert_eq!(pem2, pem);
        assert_eq!(key_matches_cert(&pem2, &key2), Some(true));
        // A new CA replaces the stale demo pair instead of mixing them.
        let fresh = CertState::generate();
        let (pem3, key3) = fresh.server_tls_material(c, k).expect("material");
        assert_eq!(pem3, fresh.server_cert_pem.as_bytes());
        assert_eq!(key_matches_cert(&pem3, &key3), Some(true));
        assert_eq!(std::fs::read(&cert).expect("read"), pem3);
    }

    #[test]
    fn mismatched_or_partial_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cert = dir.path().join("server.crt");
        let key = dir.path().join("server.key");
        let (c, k) = (cert.to_str().expect("utf8"), key.to_str().expect("utf8"));
        let state = CertState::shared_for_tests();
        // Only the key exists: nothing written, in-memory pair used.
        std::fs::write(&key, "x").expect("write");
        let (pem, _) = state.server_tls_material(c, k).expect("material");
        assert_eq!(pem, state.server_cert_pem.as_bytes());
        assert!(!cert.exists());
        // Both exist but do not match.
        std::fs::write(&cert, &state.server_cert_pem).expect("write");
        std::fs::write(&key, &state.client_key_pem).expect("write");
        assert!(state.server_tls_material(c, k).is_err());
    }
}
