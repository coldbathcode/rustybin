use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Standard error response used across all endpoints.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// Shared auth response used by all auth modules.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AuthResponse {
    pub authenticated: bool,
    pub auth_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claims: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwt_header: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_dn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_ca: Option<String>,
}

impl AuthResponse {
    /// A successful authentication of the given type (other fields empty).
    pub fn ok(auth_type: &str) -> Self {
        Self {
            authenticated: true,
            auth_type: auth_type.to_string(),
            ..Self::default()
        }
    }
}

/// Auth failure response.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AuthFailure {
    pub authenticated: bool,
    pub error: String,
}

impl AuthFailure {
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            authenticated: false,
            error: error.into(),
        }
    }
}

/// Compare two secrets without leaking where they differ.
///
/// Both inputs are hashed first so the comparison time does not depend on
/// the length of the expected value either.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let ha = Sha256::digest(a);
    let hb = Sha256::digest(b);
    let diff = ha
        .iter()
        .zip(hb.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));
    diff == 0
}

/// Decode `%XX` escapes into raw bytes (invalid escapes are kept as is).
/// With `plus_as_space`, `+` becomes a space (form encoding); otherwise it
/// is kept, which matters for base64 payloads such as PEM certificates.
pub fn percent_decode(input: &str, plus_as_space: bool) -> Vec<u8> {
    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
                out.push(b'%');
            }
            b'+' if plus_as_space => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_variants() {
        assert_eq!(percent_decode("caf%C3%A9+%2B", false), "café++".as_bytes());
        assert_eq!(percent_decode("a+b%20c", true), b"a b c");
        assert_eq!(percent_decode("100%", true), b"100%");
        assert_eq!(percent_decode("%zz%4", false), b"%zz%4");
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret2"));
        assert!(constant_time_eq(b"", b""));
    }
}
