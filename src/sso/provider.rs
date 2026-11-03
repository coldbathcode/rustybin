//! The identity provider side of single sign-on: the discovery document,
//! the keys (JWKS), the token endpoint and the ID token checks. Every call
//! goes out through the egress policy under `Purpose::Sso`
//! ([`crate::egress::Egress`]), bounded in time and size; nothing here
//! keeps the provider's tokens.

use std::time::Duration;

use base64::Engine;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde_json::Value;

use crate::egress::{Egress, Purpose};

/// The longest a call to the provider may take.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// The largest answer read from the provider.
pub const MAX_BODY_BYTES: usize = 256 * 1024;
/// Keys kept from a JWKS.
pub const MAX_KEYS: usize = 32;
/// Leeway on `exp`, `iat` and `nbf`.
pub const LEEWAY_SECS: u64 = 60;
/// The algorithms an ID token may use.
pub const ALGORITHMS: [&str; 2] = ["RS256", "ES256"];

/// The parts of the discovery document single sign-on uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    /// `id_token_signing_alg_values_supported` (may be empty).
    pub algorithms: Vec<String>,
    /// Send the client secret in the form (`client_secret_post`) rather
    /// than as HTTP Basic (the default).
    pub secret_in_form: bool,
}

/// One signing key of the provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Jwk {
    pub kid: Option<String>,
    pub kty: String,
    pub alg: Option<String>,
    pub n: Option<String>,
    pub e: Option<String>,
    pub crv: Option<String>,
    pub x: Option<String>,
    pub y: Option<String>,
}

/// The issuer without its trailing slashes (the form compared).
pub fn normalise_issuer(issuer: &str) -> String {
    issuer.trim().trim_end_matches('/').to_string()
}

/// The discovery document's URL.
pub fn discovery_url(issuer: &str) -> String {
    format!(
        "{}/.well-known/openid-configuration",
        normalise_issuer(issuer)
    )
}

async fn read_body(resp: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut resp = resp;
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("the answer could not be read: {e}"))?
    {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(format!(
                "the answer is larger than {} KiB",
                MAX_BODY_BYTES / 1024
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// GET a JSON document from the provider through the egress policy.
pub async fn get_json(egress: &Egress, url: &str) -> Result<Value, String> {
    let pinned = egress
        .resolve(url, Purpose::Sso)
        .await
        .map_err(|r| r.to_string())?;
    let client = egress.client(&pinned, HTTP_TIMEOUT)?;
    let resp = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| format!("{}: {e}", crate::egress::url_origin(url)))?;
    let status = resp.status();
    let body = read_body(resp).await?;
    if !status.is_success() {
        return Err(format!(
            "{} answered {}",
            crate::egress::url_origin(url),
            status.as_u16()
        ));
    }
    serde_json::from_slice(&body).map_err(|e| format!("not JSON: {e}"))
}

/// POST a form to the provider's token endpoint through the egress policy.
/// Answers the status and the JSON body.
pub async fn post_form(
    egress: &Egress,
    url: &str,
    form: &[(&str, &str)],
    basic: Option<(&str, &str)>,
) -> Result<(u16, Value), String> {
    let pinned = egress
        .resolve(url, Purpose::Sso)
        .await
        .map_err(|r| r.to_string())?;
    let client = egress.client(&pinned, HTTP_TIMEOUT)?;
    let body = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form.iter())
        .finish();
    let mut req = client
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body);
    if let Some((id, secret)) = basic {
        // RFC 6749 2.3.1: both form-encoded before Basic.
        let enc = |s: &str| form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
        req = req.basic_auth(enc(id), Some(enc(secret)));
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("{}: {e}", crate::egress::url_origin(url)))?;
    let status = resp.status().as_u16();
    let body = read_body(resp).await?;
    let json = serde_json::from_slice(&body).unwrap_or(Value::Null);
    Ok((status, json))
}

/// An endpoint of the discovery document: an http(s) URL, `https` when
/// the issuer is (`http` only under a loopback `http` issuer, which
/// `sso.issuer` allows for local providers): the client secret, the code
/// and the keys never travel in the clear.
fn url_field(doc: &Value, key: &str, https_only: bool) -> Result<String, String> {
    let s = doc
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("the discovery document has no {key}"))?;
    let parsed = reqwest::Url::parse(s).map_err(|e| format!("{key}: not a URL ({e})"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(format!("{key}: not an http(s) URL"));
    }
    if https_only && parsed.scheme() != "https" {
        return Err(format!(
            "{key}: must be https like the issuer (the provider named {})",
            crate::egress::url_origin(s)
        ));
    }
    Ok(s.to_string())
}

/// Read a discovery document: its issuer must be the configured one (the
/// trailing slash aside), and it names the three endpoints.
pub fn parse_discovery(doc: &Value, expected_issuer: &str) -> Result<Discovery, String> {
    let issuer = doc
        .get("issuer")
        .and_then(Value::as_str)
        .ok_or("the discovery document has no issuer")?;
    if normalise_issuer(issuer) != normalise_issuer(expected_issuer) {
        return Err(format!(
            "the discovery document names the issuer {} instead of {} (sso.issuer must be exactly the provider's issuer)",
            crate::logging::printable(issuer, 200),
            crate::logging::printable(expected_issuer, 200)
        ));
    }
    let strings = |key: &str| -> Vec<String> {
        doc.get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .take(32)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let methods = strings("token_endpoint_auth_methods_supported");
    let https_only = reqwest::Url::parse(expected_issuer).map_or(true, |u| u.scheme() != "http");
    Ok(Discovery {
        issuer: issuer.to_string(),
        authorization_endpoint: url_field(doc, "authorization_endpoint", https_only)?,
        token_endpoint: url_field(doc, "token_endpoint", https_only)?,
        jwks_uri: url_field(doc, "jwks_uri", https_only)?,
        algorithms: strings("id_token_signing_alg_values_supported"),
        secret_in_form: !methods.is_empty()
            && !methods.iter().any(|m| m == "client_secret_basic")
            && methods.iter().any(|m| m == "client_secret_post"),
    })
}

/// Read a JWKS: the signature keys of the two families an ID token may use
/// (at most [`MAX_KEYS`]).
pub fn parse_jwks(doc: &Value) -> Result<Vec<Jwk>, String> {
    let keys = doc
        .get("keys")
        .and_then(Value::as_array)
        .ok_or("the JWKS has no keys")?;
    let s = |k: &Value, f: &str| k.get(f).and_then(Value::as_str).map(str::to_string);
    let out: Vec<Jwk> = keys
        .iter()
        .filter(|k| s(k, "use").is_none_or(|u| u == "sig"))
        .filter_map(|k| {
            let kty = s(k, "kty")?;
            (kty == "RSA" || kty == "EC").then(|| Jwk {
                kid: s(k, "kid"),
                kty,
                alg: s(k, "alg"),
                n: s(k, "n"),
                e: s(k, "e"),
                crv: s(k, "crv"),
                x: s(k, "x"),
                y: s(k, "y"),
            })
        })
        .take(MAX_KEYS)
        .collect();
    if out.is_empty() {
        return Err("the JWKS holds no RSA or EC signature key".into());
    }
    Ok(out)
}

/// Why an ID token was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// No key of the JWKS has the token's key id: fetch the keys again.
    UnknownKey,
    /// Anything else, with the reason.
    Invalid(String),
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::UnknownKey => {
                f.write_str("signed with a key the provider does not publish")
            }
            VerifyError::Invalid(m) => f.write_str(m),
        }
    }
}

fn invalid(m: impl Into<String>) -> VerifyError {
    VerifyError::Invalid(m.into())
}

fn decoding_key(jwk: &Jwk, alg: Algorithm) -> Result<DecodingKey, VerifyError> {
    let need = |v: &Option<String>, f: &str| {
        v.clone()
            .ok_or_else(|| invalid(format!("the provider's key lacks {f}")))
    };
    match alg {
        Algorithm::RS256 if jwk.kty == "RSA" => {
            DecodingKey::from_rsa_components(&need(&jwk.n, "n")?, &need(&jwk.e, "e")?)
                .map_err(|e| invalid(format!("the provider's RSA key is not usable: {e}")))
        }
        Algorithm::ES256 if jwk.kty == "EC" => {
            if jwk.crv.as_deref().is_some_and(|c| c != "P-256") {
                return Err(invalid("ES256 needs a P-256 key"));
            }
            DecodingKey::from_ec_components(&need(&jwk.x, "x")?, &need(&jwk.y, "y")?)
                .map_err(|e| invalid(format!("the provider's EC key is not usable: {e}")))
        }
        _ => Err(invalid(
            "the key's type does not match the token's algorithm",
        )),
    }
}

/// The header of a JWT, read without verifying: the algorithm and key id.
fn header(token: &str) -> Result<(String, Option<String>), VerifyError> {
    let first = token.split('.').next().unwrap_or("");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(first.trim_end_matches('='))
        .map_err(|_| invalid("the ID token is malformed"))?;
    let v: Value =
        serde_json::from_slice(&bytes).map_err(|_| invalid("the ID token is malformed"))?;
    let alg = v
        .get("alg")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let kid = v.get("kid").and_then(Value::as_str).map(str::to_string);
    Ok((alg, kid))
}

/// Verify an ID token: the algorithm (RS256 or ES256, never `none` nor a
/// shared secret), the signature with the provider's key, the issuer, the
/// audience (the client id; `azp` when present or when there are several
/// audiences), the expiry, `iat` and the nonce. Answers the claims.
pub fn verify_id_token(
    token: &str,
    keys: &[Jwk],
    issuer: &str,
    client_id: &str,
    nonce: &str,
) -> Result<Value, VerifyError> {
    if token.len() > 16 * 1024 || token.split('.').count() != 3 {
        return Err(invalid("the ID token is malformed"));
    }
    let (alg_name, kid) = header(token)?;
    let alg = match alg_name.as_str() {
        "RS256" => Algorithm::RS256,
        "ES256" => Algorithm::ES256,
        other => {
            return Err(invalid(format!(
                "the ID token's algorithm {} is not accepted (RS256 or ES256)",
                crate::logging::printable(other, 20)
            )))
        }
    };
    let family = if alg == Algorithm::RS256 { "RSA" } else { "EC" };
    let candidates: Vec<&Jwk> = keys
        .iter()
        .filter(|k| k.kty == family && k.alg.as_deref().is_none_or(|a| a == alg_name))
        .collect();
    let jwk = match &kid {
        Some(kid) => candidates
            .iter()
            .find(|k| k.kid.as_deref() == Some(kid.as_str()))
            .copied(),
        None if candidates.len() == 1 => candidates.first().copied(),
        None => None,
    };
    let Some(jwk) = jwk else {
        return Err(VerifyError::UnknownKey);
    };
    let key = decoding_key(jwk, alg)?;
    let mut validation = Validation::new(alg);
    validation.leeway = LEEWAY_SECS;
    validation.validate_nbf = true;
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[client_id]);
    validation.set_required_spec_claims(&["exp", "iat", "iss", "aud", "sub"]);
    let claims = jsonwebtoken::decode::<Value>(token, &key, &validation)
        .map_err(|e| {
            use jsonwebtoken::errors::ErrorKind as K;
            invalid(match e.kind() {
                K::ExpiredSignature => "the ID token expired".to_string(),
                K::ImmatureSignature => "the ID token is not valid yet".to_string(),
                K::InvalidAudience => "the ID token is for another client (aud)".to_string(),
                K::InvalidIssuer => "the ID token is from another issuer (iss)".to_string(),
                K::InvalidSignature => "the ID token's signature is not valid".to_string(),
                K::MissingRequiredClaim(c) => format!("the ID token lacks {c}"),
                _ => format!("the ID token was refused: {e}"),
            })
        })?
        .claims;
    let several = claims
        .get("aud")
        .and_then(Value::as_array)
        .is_some_and(|a| a.len() > 1);
    match claims.get("azp").and_then(Value::as_str) {
        Some(azp) if azp != client_id => {
            return Err(invalid("the ID token was issued to another client (azp)"))
        }
        None if several => {
            return Err(invalid(
                "the ID token has several audiences and no azp naming this client",
            ))
        }
        _ => {}
    }
    match claims.get("nonce").and_then(Value::as_str) {
        Some(n) if crate::control_auth::constant_time_eq(n, nonce) => {}
        Some(_) => return Err(invalid("the ID token's nonce does not match this sign-in")),
        None => return Err(invalid("the ID token carries no nonce")),
    }
    let sub = claims.get("sub").and_then(Value::as_str).unwrap_or("");
    if sub.is_empty() || sub.len() > 255 {
        return Err(invalid("the ID token's subject is empty or too long"));
    }
    Ok(claims)
}
