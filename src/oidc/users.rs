//! Configured end users for the built-in OIDC provider (`RUSTYBIN_OIDC_USERS_FILE`).
//!
//! Without the file the provider signs in the demo users (`demo`, `alice`, `bob`, password = name).
//! With it, only the users in the file sign in: the demo users are off and the login page no longer
//! prints their passwords, so the provider can guard something real (a staging portal, a team's
//! own tools). Passwords are stored as PBKDF2-HMAC-SHA256 hashes, made with
//! `rustybin oidc-hash-password`.
//!
//! ```json
//! {"users": [{"username": "oli", "password_hash": "pbkdf2-sha256$600000$<salt>$<hash>",
//!             "email": "oli@example.com", "name": "Oli", "groups": ["users", "admins"]}]}
//! ```
//!
//! The file is read once at start. A file that cannot be read or does not validate stops the
//! server: falling back to the demo users would open what the file was meant to close.

use std::path::Path;
use std::sync::OnceLock;

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde_json::Value;
use sha2::Sha256;

use super::clients::{DemoUser, USERS};
use crate::types::constant_time_eq;

/// Iterations of new hashes (OWASP's figure for PBKDF2-HMAC-SHA256).
pub const DEFAULT_ITERATIONS: u32 = 600_000;
/// Fewer iterations than this are refused in a users file.
pub const MIN_ITERATIONS: u32 = 100_000;
const MAX_ITERATIONS: u32 = 10_000_000;
const SALT_BYTES: usize = 16;
const HASH_BYTES: usize = 32;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_USERS: usize = 1000;
const MAX_GROUPS: usize = 16;
const PREFIX: &str = "pbkdf2-sha256";

/// A PBKDF2-HMAC-SHA256 password hash: `pbkdf2-sha256$<iterations>$<salt>$<hash>`, salt and hash
/// in base64 without padding.
pub struct PasswordHash {
    iterations: u32,
    salt: Vec<u8>,
    hash: [u8; HASH_BYTES],
}

impl PasswordHash {
    /// A new hash of `password` with a random salt.
    pub fn new(password: &str, iterations: u32) -> Self {
        let mut salt = vec![0u8; SALT_BYTES];
        rand::thread_rng().fill_bytes(&mut salt);
        let hash = pbkdf2_sha256(password.as_bytes(), &salt, iterations);
        Self {
            iterations,
            salt,
            hash,
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.split('$').collect();
        let [prefix, iterations, salt, hash] = parts[..] else {
            return Err(format!("expected {PREFIX}$<iterations>$<salt>$<hash>"));
        };
        if prefix != PREFIX {
            return Err(format!(
                "unknown hash scheme {prefix:?} (expected {PREFIX})"
            ));
        }
        let iterations: u32 = iterations
            .parse()
            .map_err(|_| format!("iterations {iterations:?} is not a number"))?;
        if !(MIN_ITERATIONS..=MAX_ITERATIONS).contains(&iterations) {
            return Err(format!(
                "iterations must be {MIN_ITERATIONS} to {MAX_ITERATIONS}, not {iterations}"
            ));
        }
        let salt = STANDARD_NO_PAD
            .decode(salt)
            .map_err(|_| "the salt is not base64".to_string())?;
        if salt.len() < SALT_BYTES {
            return Err(format!("the salt must be at least {SALT_BYTES} bytes"));
        }
        let hash: [u8; HASH_BYTES] = STANDARD_NO_PAD
            .decode(hash)
            .ok()
            .and_then(|h| h.try_into().ok())
            .ok_or_else(|| format!("the hash must be {HASH_BYTES} bytes of base64"))?;
        Ok(Self {
            iterations,
            salt,
            hash,
        })
    }

    pub fn verify(&self, password: &str) -> bool {
        let got = pbkdf2_sha256(password.as_bytes(), &self.salt, self.iterations);
        constant_time_eq(&got, &self.hash)
    }
}

impl std::fmt::Display for PasswordHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{PREFIX}${}${}${}",
            self.iterations,
            STANDARD_NO_PAD.encode(&self.salt),
            STANDARD_NO_PAD.encode(self.hash)
        )
    }
}

/// PBKDF2 (RFC 8018) with HMAC-SHA256, one output block (32 bytes).
fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; HASH_BYTES] {
    let mac = Hmac::<Sha256>::new_from_slice(password).expect("HMAC takes a key of any length");
    let mut first = mac.clone();
    first.update(salt);
    first.update(&1u32.to_be_bytes());
    let mut u: [u8; HASH_BYTES] = first.finalize().into_bytes().into();
    let mut out = u;
    for _ in 1..iterations {
        let mut next = mac.clone();
        next.update(&u);
        u = next.finalize().into_bytes().into();
        for (o, b) in out.iter_mut().zip(u) {
            *o ^= b;
        }
    }
    out
}

/// The users of a users file. Kept for the life of the process (read once at start).
static CONFIGURED: OnceLock<&'static [DemoUser]> = OnceLock::new();

/// Read and validate `path`, then make its users the only ones that sign in.
pub fn install_file(path: &Path) -> Result<usize, String> {
    let users = load_file(path)?;
    let count = users.len();
    CONFIGURED
        .set(users)
        .map_err(|_| "the OIDC users are already installed".to_string())?;
    Ok(count)
}

/// The users who sign in: the file's when one is installed, else the demo users.
pub fn active() -> &'static [DemoUser] {
    CONFIGURED.get().copied().unwrap_or(USERS)
}

/// Whether the demo users (and their printed passwords) are in use.
pub fn demo_active() -> bool {
    CONFIGURED.get().is_none()
}

pub fn load_file(path: &Path) -> Result<&'static [DemoUser], String> {
    let shown = path.display();
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot read {shown}: {e}"))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("{shown} is over {MAX_FILE_BYTES} bytes"));
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("cannot read {shown}: {e}"))?;
    parse(&raw).map_err(|e| format!("{shown}: {e}"))
}

/// Parse a users file. The users are leaked: they live as long as the process, like the demo users.
pub fn parse(raw: &str) -> Result<&'static [DemoUser], String> {
    let doc: Value = serde_json::from_str(raw).map_err(|e| format!("not JSON: {e}"))?;
    let list = doc
        .get("users")
        .and_then(Value::as_array)
        .ok_or("expected {\"users\": [...]}")?;
    if list.is_empty() {
        return Err("no users (an empty file would lock everyone out; remove the setting to use the demo users)".into());
    }
    if list.len() > MAX_USERS {
        return Err(format!("at most {MAX_USERS} users"));
    }
    let mut users = Vec::with_capacity(list.len());
    for (i, entry) in list.iter().enumerate() {
        let user = parse_user(entry).map_err(|e| format!("user {}: {e}", i + 1))?;
        if users.iter().any(|u: &DemoUser| u.username == user.username) {
            return Err(format!(
                "user {}: duplicate username {:?}",
                i + 1,
                user.username
            ));
        }
        users.push(user);
    }
    Ok(Box::leak(users.into_boxed_slice()))
}

fn parse_user(v: &Value) -> Result<DemoUser, String> {
    let text = |key: &str| v.get(key).and_then(Value::as_str);
    let username = text("username").ok_or("username is required")?;
    if username.is_empty()
        || username.len() > 64
        || !username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@'))
    {
        return Err(format!(
            "username {username:?} must be 1 to 64 of A-Z a-z 0-9 . _ - @"
        ));
    }
    let hash =
        text("password_hash").ok_or("password_hash is required (rustybin oidc-hash-password)")?;
    let hash = PasswordHash::parse(hash)?;
    let email = text("email").ok_or("email is required")?;
    if email.len() > 254 || !email.contains('@') || email.chars().any(char::is_whitespace) {
        return Err(format!("email {email:?} is not an address"));
    }
    let name = text("name").unwrap_or(username);
    if name.len() > 128 {
        return Err("name is over 128 characters".into());
    }
    let groups: Vec<&'static str> = match v.get("groups") {
        None => vec!["users"],
        Some(Value::Array(a)) if a.len() <= MAX_GROUPS => a
            .iter()
            .map(|g| {
                g.as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 64)
                    .map(leak)
                    .ok_or_else(|| "groups are strings of 1 to 64 characters".to_string())
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(format!("groups is a list of at most {MAX_GROUPS} strings")),
    };
    Ok(DemoUser {
        username: leak(username),
        password: "",
        password_hash: Some(Box::leak(Box::new(hash))),
        name: leak(name),
        email: leak(email),
        groups: Box::leak(groups.into_boxed_slice()),
    })
}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

/// `rustybin oidc-hash-password`: read a password from stdin (the first line), print its hash.
pub fn hash_password_cli() -> std::process::ExitCode {
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        eprintln!("could not read a password from stdin");
        return std::process::ExitCode::from(2);
    }
    let password = line.trim_end_matches(['\r', '\n']);
    if password.len() < 12 {
        eprintln!("use a password of at least 12 characters");
        return std::process::ExitCode::from(2);
    }
    println!("{}", PasswordHash::new(password, DEFAULT_ITERATIONS));
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn pbkdf2_matches_the_published_vectors() {
        // PBKDF2-HMAC-SHA256 test vectors (RFC 7914 section 11 uses the same construction).
        assert_eq!(
            hex(&pbkdf2_sha256(b"password", b"salt", 1)),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        assert_eq!(
            hex(&pbkdf2_sha256(b"password", b"salt", 2)),
            "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
        );
        assert_eq!(
            hex(&pbkdf2_sha256(b"password", b"salt", 4096)),
            "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"
        );
    }

    #[test]
    fn a_hash_round_trips_and_verifies() {
        let h = PasswordHash::new("correct horse battery", MIN_ITERATIONS);
        let parsed = PasswordHash::parse(&h.to_string()).expect("parses");
        assert!(parsed.verify("correct horse battery"));
        assert!(!parsed.verify("correct horse batterY"));
        assert!(!parsed.verify(""));
    }

    #[test]
    fn weak_or_malformed_hashes_are_refused() {
        let good = PasswordHash::new("x", MIN_ITERATIONS).to_string();
        let parts: Vec<&str> = good.split('$').collect();
        for bad in [
            "plain".to_string(),
            format!("bcrypt${}${}${}", parts[1], parts[2], parts[3]),
            format!("{PREFIX}$1000${}${}", parts[2], parts[3]),
            format!("{PREFIX}$abc${}${}", parts[2], parts[3]),
            format!("{PREFIX}${}$c2FsdA${}", parts[1], parts[3]),
            format!("{PREFIX}${}${}$AAAA", parts[1], parts[2]),
        ] {
            assert!(PasswordHash::parse(&bad).is_err(), "{bad}");
        }
    }

    fn file(users: &str) -> String {
        format!("{{\"users\": [{users}]}}")
    }

    #[test]
    fn a_users_file_parses_and_validates() {
        let h = PasswordHash::new("staging password 1", MIN_ITERATIONS).to_string();
        let users = parse(&file(&format!(
            r#"{{"username": "oli", "password_hash": "{h}", "email": "oli@example.com", "name": "Oli", "groups": ["users", "admins"]}},
               {{"username": "ci-bot", "password_hash": "{h}", "email": "ci@example.com"}}"#
        )))
        .expect("valid");
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].name, "Oli");
        assert_eq!(users[0].groups, ["users", "admins"]);
        assert_eq!(users[1].name, "ci-bot", "name defaults to the username");
        assert_eq!(users[1].groups, ["users"]);
        assert!(users[0].check_password("staging password 1"));
        assert!(
            !users[0].check_password(""),
            "a configured user never matches an empty password"
        );

        for (bad, why) in [
            ("{}".to_string(), "no users key"),
            (file(""), "empty list"),
            (
                file(&format!(
                    r#"{{"username": "a b", "password_hash": "{h}", "email": "a@b"}}"#
                )),
                "username",
            ),
            (
                file(r#"{"username": "a", "password_hash": "x", "email": "a@b"}"#),
                "hash",
            ),
            (
                file(&format!(r#"{{"username": "a", "password_hash": "{h}"}}"#)),
                "email",
            ),
            (
                file(&format!(
                    r#"{{"username": "a", "password_hash": "{h}", "email": "a@b", "groups": "admins"}}"#
                )),
                "groups",
            ),
            (
                file(&format!(
                    r#"{{"username": "a", "password_hash": "{h}", "email": "a@b"}}, {{"username": "a", "password_hash": "{h}", "email": "c@d"}}"#
                )),
                "duplicate",
            ),
        ] {
            assert!(parse(&bad).is_err(), "{why}: {bad}");
        }
    }

    #[test]
    fn the_demo_users_still_sign_in_by_default() {
        let demo = USERS.iter().find(|u| u.username == "alice").expect("alice");
        assert!(demo.check_password("alice"));
        assert!(!demo.check_password("bob"));
    }
}
