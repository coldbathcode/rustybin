use base64::Engine;
use jsonwebtoken::{DecodingKey, EncodingKey};
use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use std::sync::{Arc, OnceLock};

/// Shared JWT state holding signing keys for HS256 and RS256.
/// Created once at startup and shared across auth modules.
pub struct JwtState {
    pub hs256_secret: String,
    pub rs256_encoding_key: EncodingKey,
    pub rs256_decoding_key: DecodingKey,
    pub rs256_jwk: serde_json::Value,
}

impl JwtState {
    /// Verify an RS256 access token issued by the built-in OIDC provider.
    ///
    /// Checks the signature and `exp`/`nbf`; audience is not checked here so
    /// callers (MCP, A2A, protected resources) can apply their own rules.
    /// This is the stable entry point other modules use to validate tokens.
    pub fn verify_rs256(&self, token: &str) -> Result<serde_json::Value, String> {
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        validation.validate_aud = false;
        validation.validate_nbf = true;
        jsonwebtoken::decode::<serde_json::Value>(token, &self.rs256_decoding_key, &validation)
            .map(|data| data.claims)
            .map_err(|e| e.to_string())
    }

    /// A process-wide JWT state generated once and shared by every caller.
    /// Tests use this so RSA key generation happens at most once per test
    /// binary instead of once per test.
    #[doc(hidden)]
    pub fn shared_for_tests() -> Arc<JwtState> {
        static SHARED: OnceLock<Arc<JwtState>> = OnceLock::new();
        SHARED
            .get_or_init(|| Arc::new(JwtState::generate()))
            .clone()
    }

    /// Generate a fresh HS256 secret holder and RS256 key pair.
    ///
    /// Only called at startup (never on a request path), so failing loudly
    /// on a broken RNG or encoder is acceptable.
    pub fn generate() -> Self {
        let hs256_secret = "rustybin-demo-secret-do-not-use-in-production".to_string();

        // Generate RS256 key pair
        let mut rng = rand::thread_rng();
        let private_key =
            RsaPrivateKey::new(&mut rng, 2048).expect("failed to generate RSA private key");
        let public_key = private_key.to_public_key();

        let private_pem = private_key
            .to_pkcs8_pem(LineEnding::LF)
            .expect("failed to encode RSA private key to PEM");
        let public_pem = public_key
            .to_public_key_pem(LineEnding::LF)
            .expect("failed to encode RSA public key to PEM");

        let rs256_encoding_key = EncodingKey::from_rsa_pem(private_pem.as_bytes())
            .expect("failed to create RS256 encoding key");
        let rs256_decoding_key = DecodingKey::from_rsa_pem(public_pem.as_bytes())
            .expect("failed to create RS256 decoding key");

        // Build JWK from public key components
        let b64url = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let n_b64 = b64url.encode(public_key.n().to_bytes_be());
        let e_b64 = b64url.encode(public_key.e().to_bytes_be());

        let rs256_jwk = serde_json::json!({
            "kty": "RSA",
            "alg": "RS256",
            "use": "sig",
            "kid": "rustybin-rs256-key",
            "n": n_b64,
            "e": e_b64,
        });

        tracing::info!("JWT state initialized: HS256 secret + RS256 key pair generated");

        Self {
            hs256_secret,
            rs256_encoding_key,
            rs256_decoding_key,
            rs256_jwk,
        }
    }
}
