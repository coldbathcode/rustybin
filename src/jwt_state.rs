use axum::http::{header, HeaderMap};
use base64::Engine;
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Validation};
use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use std::sync::{Arc, OnceLock};

/// The demo HS256 secret. Public on purpose: gateway demos configure it to
/// validate or mint HS256 tokens that Rustybin accepts.
pub const HS256_DEMO_SECRET: &str = "rustybin-demo-secret-do-not-use-in-production";

/// `kid` of the RS256 key published at `/oauth/jwks`.
pub const RS256_KID: &str = "rustybin-rs256-key";

/// The bearer token of the `Authorization` header (scheme matched
/// case-insensitively, RFC 7235). `None` when absent, not Bearer or empty.
pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then_some(token)
}

/// Decode a JWT's header and claims WITHOUT verifying anything.
///
/// Only for display ("what did the gateway forward") and for reading `alg`
/// before verification; never trust the result.
pub fn decode_unverified(
    token: &str,
) -> Result<(serde_json::Value, serde_json::Value), &'static str> {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(_sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("token must have exactly 3 parts");
    };
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header_bytes = b64
        .decode(h.trim_end_matches('='))
        .map_err(|_| "invalid base64url in header")?;
    let payload_bytes = b64
        .decode(p.trim_end_matches('='))
        .map_err(|_| "invalid base64url in payload")?;
    let header: serde_json::Value =
        serde_json::from_slice(&header_bytes).map_err(|_| "invalid JSON in header")?;
    let claims: serde_json::Value =
        serde_json::from_slice(&payload_bytes).map_err(|_| "invalid JSON in payload")?;
    if !header.is_object() || !claims.is_object() {
        return Err("header and payload must be JSON objects");
    }
    Ok((header, claims))
}

/// Extra constraints for [`JwtState::verify`].
#[derive(Default, Clone, Debug)]
pub struct VerifyOptions<'a> {
    /// Required `iss` value.
    pub issuer: Option<&'a str>,
    /// Required `aud` value (one of the token's audiences must match).
    pub audience: Option<&'a str>,
}

/// A token that passed [`JwtState::verify`].
#[derive(Clone, Debug)]
pub struct VerifiedJwt {
    pub header: serde_json::Value,
    pub claims: serde_json::Value,
    /// `HS256` or `RS256`.
    pub alg: &'static str,
}

fn describe_jwt_error(e: &jsonwebtoken::errors::Error) -> String {
    match e.kind() {
        ErrorKind::ExpiredSignature => "token expired".into(),
        ErrorKind::ImmatureSignature => "token not yet valid (nbf)".into(),
        ErrorKind::InvalidSignature => "invalid signature".into(),
        ErrorKind::InvalidIssuer => "issuer mismatch".into(),
        ErrorKind::InvalidAudience => "audience mismatch".into(),
        ErrorKind::MissingRequiredClaim(c) => format!("missing required claim {c}"),
        ErrorKind::InvalidAlgorithm => "algorithm mismatch".into(),
        _ => format!("invalid token ({e})"),
    }
}

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

    /// Verify a token signed by Rustybin: `HS256` with [`HS256_DEMO_SECRET`]
    /// or `RS256` with the IdP key, chosen by the header `alg`. `alg: none`
    /// and every other algorithm are rejected. Checks the signature, `exp`
    /// (required), `nbf` (when present; 30 s leeway on both) and the optional
    /// issuer / audience constraints.
    pub fn verify(&self, token: &str, opts: &VerifyOptions) -> Result<VerifiedJwt, String> {
        let (header, _) = decode_unverified(token).map_err(String::from)?;
        let alg = header.get("alg").and_then(|a| a.as_str()).unwrap_or("");
        let (algorithm, alg_name) = match alg {
            "HS256" => (Algorithm::HS256, "HS256"),
            "RS256" => (Algorithm::RS256, "RS256"),
            "" => return Err("missing alg in header".into()),
            a if a.eq_ignore_ascii_case("none") => return Err("alg none is not accepted".into()),
            _ => return Err("unsupported alg (only HS256 and RS256 are accepted)".into()),
        };
        let mut validation = Validation::new(algorithm);
        validation.leeway = 30;
        validation.validate_nbf = true;
        let mut required = vec!["exp"];
        if let Some(iss) = opts.issuer {
            validation.set_issuer(&[iss]);
            required.push("iss");
        }
        match opts.audience {
            Some(aud) => {
                validation.set_audience(&[aud]);
                required.push("aud");
            }
            None => validation.validate_aud = false,
        }
        validation.set_required_spec_claims(&required);
        let result = match algorithm {
            Algorithm::HS256 => jsonwebtoken::decode::<serde_json::Value>(
                token,
                &DecodingKey::from_secret(self.hs256_secret.as_bytes()),
                &validation,
            ),
            _ => jsonwebtoken::decode::<serde_json::Value>(
                token,
                &self.rs256_decoding_key,
                &validation,
            ),
        };
        result
            .map(|data| VerifiedJwt {
                header,
                claims: data.claims,
                alg: alg_name,
            })
            .map_err(|e| describe_jwt_error(&e))
    }

    /// Sign claims with the RS256 IdP key (`kid` set, `typ` as given, e.g.
    /// `at+jwt` for access tokens or `JWT` for ID tokens).
    pub fn sign_rs256(
        &self,
        claims: &serde_json::Map<String, serde_json::Value>,
        typ: &str,
    ) -> Result<String, String> {
        let mut hdr = jsonwebtoken::Header::new(Algorithm::RS256);
        hdr.kid = Some(RS256_KID.to_string());
        hdr.typ = Some(typ.to_string());
        jsonwebtoken::encode(&hdr, claims, &self.rs256_encoding_key).map_err(|e| e.to_string())
    }

    /// Sign claims with the demo HS256 secret.
    pub fn sign_hs256(
        &self,
        claims: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<String, String> {
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(Algorithm::HS256),
            claims,
            &EncodingKey::from_secret(self.hs256_secret.as_bytes()),
        )
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
        let hs256_secret = HS256_DEMO_SECRET.to_string();

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
            "kid": RS256_KID,
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn claims(extra: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        let now = chrono::Utc::now().timestamp();
        let mut base = serde_json::json!({"sub": "u1", "iat": now, "exp": now + 600});
        if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                b.insert(k.clone(), v.clone());
            }
        }
        base.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn bearer_token_is_case_insensitive() {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("bEaReR   abc.def.ghi "),
        );
        assert_eq!(bearer_token(&h), Some("abc.def.ghi"));
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer"));
        assert_eq!(bearer_token(&h), None);
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert_eq!(bearer_token(&h), None);
    }

    #[test]
    fn verify_hs256_and_rs256() {
        let jwt = JwtState::shared_for_tests();
        let hs = jwt
            .sign_hs256(&claims(serde_json::json!({})))
            .expect("sign");
        let opts = VerifyOptions::default();
        assert_eq!(jwt.verify(&hs, &opts).expect("hs").alg, "HS256");
        let rs = jwt
            .sign_rs256(&claims(serde_json::json!({})), "JWT")
            .expect("sign");
        assert_eq!(jwt.verify(&rs, &opts).expect("rs").alg, "RS256");
        // verify_rs256 stays the shared entry point for RS256 tokens.
        assert!(jwt.verify_rs256(&rs).is_ok());
        assert!(jwt.verify_rs256(&hs).is_err());
    }

    #[test]
    fn verify_rejects_alg_none_forged_and_expired() {
        let jwt = JwtState::shared_for_tests();
        let opts = VerifyOptions::default();
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let now = chrono::Utc::now().timestamp();
        let payload = b64.encode(format!(r#"{{"sub":"x","exp":{}}}"#, now + 600));
        let none = format!("{}.{payload}.", b64.encode(r#"{"alg":"none"}"#));
        assert!(jwt.verify(&none, &opts).unwrap_err().contains("none"));
        let forged = format!("{}.{payload}.c2ln", b64.encode(r#"{"alg":"HS256"}"#));
        assert_eq!(jwt.verify(&forged, &opts).unwrap_err(), "invalid signature");
        let other = format!("{}.{payload}.c2ln", b64.encode(r#"{"alg":"HS512"}"#));
        assert!(jwt.verify(&other, &opts).is_err());
        let expired = jwt
            .sign_hs256(&claims(serde_json::json!({"exp": now - 3600})))
            .expect("sign");
        assert_eq!(jwt.verify(&expired, &opts).unwrap_err(), "token expired");
        let future = jwt
            .sign_hs256(&claims(serde_json::json!({"nbf": now + 3600})))
            .expect("sign");
        assert!(jwt.verify(&future, &opts).is_err());
        let no_exp = serde_json::json!({"sub": "x"})
            .as_object()
            .cloned()
            .unwrap_or_default();
        let no_exp = jwt.sign_hs256(&no_exp).expect("sign");
        assert!(jwt.verify(&no_exp, &opts).is_err());
    }

    #[test]
    fn verify_issuer_and_audience() {
        let jwt = JwtState::shared_for_tests();
        let t = jwt
            .sign_hs256(&claims(serde_json::json!({"iss": "me", "aud": ["a", "b"]})))
            .expect("sign");
        let ok = VerifyOptions {
            issuer: Some("me"),
            audience: Some("b"),
        };
        assert!(jwt.verify(&t, &ok).is_ok());
        let bad_iss = VerifyOptions {
            issuer: Some("you"),
            audience: None,
        };
        assert_eq!(jwt.verify(&t, &bad_iss).unwrap_err(), "issuer mismatch");
        let bad_aud = VerifyOptions {
            issuer: None,
            audience: Some("c"),
        };
        assert_eq!(jwt.verify(&t, &bad_aud).unwrap_err(), "audience mismatch");
    }

    #[test]
    fn decode_unverified_shapes() {
        assert!(decode_unverified("a.b").is_err());
        assert!(decode_unverified("a.b.c.d").is_err());
        assert!(decode_unverified("!!.b.c").is_err());
    }
}
