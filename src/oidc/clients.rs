//! Demo clients, demo users, dynamic client registration (RFC 7591) and
//! redirect URI handling for the built-in OAuth / OIDC provider.

use axum::http::{HeaderValue, Uri};
use serde_json::{json, Value};

use super::store::BoundedStore;
use crate::types::constant_time_eq;

/// Confidential demo client (client_secret_basic or client_secret_post).
pub const DEMO_CLIENT_ID: &str = "rustybin";
pub const DEMO_CLIENT_SECRET: &str = "secret";
/// Public demo client (no secret, PKCE required).
pub const PUBLIC_CLIENT_ID: &str = "rustybin-public";

pub const GRANT_AUTHORIZATION_CODE: &str = "authorization_code";
pub const GRANT_CLIENT_CREDENTIALS: &str = "client_credentials";
pub const GRANT_PASSWORD: &str = "password";
pub const GRANT_REFRESH_TOKEN: &str = "refresh_token";
pub const GRANT_TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";

pub const ALL_GRANTS: &[&str] = &[
    GRANT_AUTHORIZATION_CODE,
    GRANT_CLIENT_CREDENTIALS,
    GRANT_PASSWORD,
    GRANT_REFRESH_TOKEN,
    GRANT_TOKEN_EXCHANGE,
];

const MAX_REDIRECT_URIS: usize = 10;
const MAX_URI_LEN: usize = 2048;

/// A registered OAuth client.
#[derive(Clone, Debug)]
pub struct Client {
    pub client_id: String,
    /// `None` for public clients (`token_endpoint_auth_method: none`).
    pub secret: Option<String>,
    /// Exact redirect URIs. Empty means "any absolute http(s) URI" (demo clients).
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<String>,
    pub name: String,
    pub scope: Option<String>,
}

impl Client {
    pub fn is_public(&self) -> bool {
        self.secret.is_none()
    }

    pub fn allows_grant(&self, grant: &str) -> bool {
        self.grant_types.iter().any(|g| g == grant)
    }

    /// Whether `uri` may be used as this client's redirect URI.
    pub fn redirect_allowed(&self, uri: &str) -> bool {
        if validate_redirect_uri(uri).is_err() {
            return false;
        }
        self.redirect_uris.is_empty() || self.redirect_uris.iter().any(|r| r == uri)
    }

    pub fn secret_matches(&self, secret: &str) -> bool {
        self.secret
            .as_deref()
            .is_some_and(|s| constant_time_eq(s.as_bytes(), secret.as_bytes()))
    }
}

fn demo_clients() -> [Client; 2] {
    [
        Client {
            client_id: DEMO_CLIENT_ID.into(),
            secret: Some(DEMO_CLIENT_SECRET.into()),
            redirect_uris: Vec::new(),
            grant_types: ALL_GRANTS.iter().map(|g| g.to_string()).collect(),
            name: "Rustybin demo client (confidential)".into(),
            scope: None,
        },
        Client {
            client_id: PUBLIC_CLIENT_ID.into(),
            secret: None,
            redirect_uris: Vec::new(),
            grant_types: vec![GRANT_AUTHORIZATION_CODE.into(), GRANT_REFRESH_TOKEN.into()],
            name: "Rustybin demo client (public, PKCE)".into(),
            scope: None,
        },
    ]
}

/// Demo clients plus a bounded store of dynamically registered ones.
pub struct ClientRegistry {
    dynamic: BoundedStore<Client>,
}

impl ClientRegistry {
    pub fn new(capacity: usize, ttl_secs: i64) -> Self {
        Self {
            dynamic: BoundedStore::new(capacity, ttl_secs),
        }
    }

    pub fn get(&self, client_id: &str) -> Option<Client> {
        demo_clients()
            .into_iter()
            .find(|c| c.client_id == client_id)
            .or_else(|| self.dynamic.get(client_id))
    }

    /// Validate RFC 7591 client metadata and register the client.
    /// Returns the registration response, or `(error, description)`.
    pub fn register(&self, metadata: &Value) -> Result<Value, (&'static str, String)> {
        let reg = parse_registration(metadata)?;
        let now = chrono::Utc::now().timestamp();
        let client_id = format!("dyn-{}", uuid::Uuid::new_v4().simple());
        let secret = (reg.auth_method != "none").then(|| {
            format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            )
        });
        let client = Client {
            client_id: client_id.clone(),
            secret: secret.clone(),
            redirect_uris: reg.redirect_uris.clone(),
            grant_types: reg.grant_types.clone(),
            name: reg.client_name.clone(),
            scope: reg.scope.clone(),
        };
        let expires_at = self.dynamic.insert(client_id.clone(), client);
        let mut response = json!({
            "client_id": client_id,
            "client_id_issued_at": now,
            "client_name": reg.client_name,
            "redirect_uris": reg.redirect_uris,
            "grant_types": reg.grant_types,
            "response_types": reg.response_types,
            "token_endpoint_auth_method": reg.auth_method,
        });
        if let Some(obj) = response.as_object_mut() {
            if let Some(secret) = secret {
                obj.insert("client_secret".into(), json!(secret));
                obj.insert("client_secret_expires_at".into(), json!(expires_at));
            }
            if let Some(scope) = reg.scope {
                obj.insert("scope".into(), json!(scope));
            }
            obj.insert(
                "rustybin_note".into(),
                json!(format!(
                    "demo registration, forgotten after {} s or when the registry is full",
                    self.dynamic.ttl_secs()
                )),
            );
        }
        Ok(response)
    }
}

struct Registration {
    redirect_uris: Vec<String>,
    grant_types: Vec<String>,
    response_types: Vec<String>,
    auth_method: String,
    client_name: String,
    scope: Option<String>,
}

fn string_list(value: Option<&Value>, field: &str) -> Result<Option<Vec<String>>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(String::from)
                    .ok_or_else(|| format!("{field} must be an array of strings"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(format!("{field} must be an array of strings")),
    }
}

fn parse_registration(metadata: &Value) -> Result<Registration, (&'static str, String)> {
    let invalid = |d: String| ("invalid_client_metadata", d);
    let Some(obj) = metadata.as_object() else {
        return Err(invalid("body must be a JSON object".into()));
    };
    let auth_method = match obj.get("token_endpoint_auth_method") {
        None | Some(Value::Null) => "client_secret_basic".to_string(),
        Some(Value::String(m))
            if ["client_secret_basic", "client_secret_post", "none"].contains(&m.as_str()) =>
        {
            m.clone()
        }
        Some(_) => return Err(invalid(
            "token_endpoint_auth_method must be client_secret_basic, client_secret_post or none"
                .into(),
        )),
    };
    let grant_types = string_list(obj.get("grant_types"), "grant_types")
        .map_err(invalid)?
        .unwrap_or_else(|| vec![GRANT_AUTHORIZATION_CODE.into()]);
    if grant_types.is_empty() || grant_types.len() > ALL_GRANTS.len() {
        return Err(invalid(
            "grant_types must list supported grant types".into(),
        ));
    }
    if let Some(g) = grant_types
        .iter()
        .find(|g| !ALL_GRANTS.contains(&g.as_str()))
    {
        return Err(invalid(format!("unsupported grant type {g:?}")));
    }
    if auth_method == "none"
        && grant_types
            .iter()
            .any(|g| g == GRANT_CLIENT_CREDENTIALS || g == GRANT_TOKEN_EXCHANGE)
    {
        return Err(invalid(
            "public clients (auth method none) cannot use client_credentials or token exchange"
                .into(),
        ));
    }
    let response_types = string_list(obj.get("response_types"), "response_types")
        .map_err(invalid)?
        .unwrap_or_else(|| vec!["code".into()]);
    if response_types.iter().any(|r| r != "code") {
        return Err(invalid("only the response type code is supported".into()));
    }
    let redirect_uris = string_list(obj.get("redirect_uris"), "redirect_uris")
        .map_err(|d| ("invalid_redirect_uri", d))?
        .unwrap_or_default();
    if grant_types.iter().any(|g| g == GRANT_AUTHORIZATION_CODE) && redirect_uris.is_empty() {
        return Err((
            "invalid_redirect_uri",
            "redirect_uris is required for the authorization_code grant".into(),
        ));
    }
    if redirect_uris.len() > MAX_REDIRECT_URIS {
        return Err((
            "invalid_redirect_uri",
            format!("at most {MAX_REDIRECT_URIS} redirect_uris"),
        ));
    }
    for uri in &redirect_uris {
        validate_redirect_uri(uri).map_err(|e| ("invalid_redirect_uri", e.to_string()))?;
    }
    let client_name = match obj.get("client_name") {
        None | Some(Value::Null) => "dynamically registered client".to_string(),
        Some(Value::String(n)) if n.chars().count() <= 200 => n.clone(),
        Some(_) => {
            return Err(invalid(
                "client_name must be a string of at most 200 characters".into(),
            ))
        }
    };
    let scope = match obj.get("scope") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.len() <= 1000 => Some(s.clone()),
        Some(_) => {
            return Err(invalid(
                "scope must be a string of at most 1000 characters".into(),
            ))
        }
    };
    Ok(Registration {
        redirect_uris,
        grant_types,
        response_types,
        auth_method,
        client_name,
        scope,
    })
}

// ── Redirect URIs ───────────────────────────────────────────────────

/// An absolute `http`/`https` URI with a host, no fragment, no whitespace
/// or control characters, at most 2048 bytes.
pub fn validate_redirect_uri(uri: &str) -> Result<(), &'static str> {
    if uri.is_empty() || uri.len() > MAX_URI_LEN {
        return Err("redirect_uri must be 1 to 2048 characters");
    }
    if uri.contains('#') {
        return Err("redirect_uri must not contain a fragment");
    }
    if uri.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return Err("redirect_uri must not contain whitespace or control characters");
    }
    let parsed: Uri = uri
        .parse()
        .map_err(|_| "redirect_uri is not a valid absolute URI")?;
    match parsed.scheme_str() {
        Some("http") | Some("https") => {}
        _ => return Err("redirect_uri must use http or https"),
    }
    match parsed.host() {
        Some(h) if !h.is_empty() => Ok(()),
        _ => Err("redirect_uri must have a host"),
    }
}

/// `redirect_uri` with `params` appended to its query (form-encoded), as a
/// header value. `None` when the result is not a valid header value.
pub fn redirect_location(redirect_uri: &str, params: &[(&str, &str)]) -> Option<HeaderValue> {
    validate_redirect_uri(redirect_uri).ok()?;
    let query = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params.iter().filter(|(_, v)| !v.is_empty()))
        .finish();
    let separator = match redirect_uri.find('?') {
        None => "?",
        Some(_) if redirect_uri.ends_with('?') || redirect_uri.ends_with('&') => "",
        Some(_) => "&",
    };
    HeaderValue::from_str(&format!("{redirect_uri}{separator}{query}")).ok()
}

// ── Demo users ──────────────────────────────────────────────────────

/// Demo end users for the password grant and the login form.
pub struct DemoUser {
    pub username: &'static str,
    pub password: &'static str,
    pub name: &'static str,
    pub email: &'static str,
    pub groups: &'static [&'static str],
}

pub const USERS: &[DemoUser] = &[
    DemoUser {
        username: "demo",
        password: "demo",
        name: "Demo User",
        email: "demo@rustybin.local",
        groups: &["users"],
    },
    DemoUser {
        username: "alice",
        password: "alice",
        name: "Alice Admin",
        email: "alice@rustybin.local",
        groups: &["users", "admins"],
    },
    DemoUser {
        username: "bob",
        password: "bob",
        name: "Bob Builder",
        email: "bob@rustybin.local",
        groups: &["users"],
    },
];

pub fn user_by_name(username: &str) -> Option<&'static DemoUser> {
    USERS.iter().find(|u| u.username == username)
}

/// The demo user with these credentials (password compared in constant time).
pub fn authenticate_user(username: &str, password: &str) -> Option<&'static DemoUser> {
    let user = user_by_name(username)?;
    constant_time_eq(user.password.as_bytes(), password.as_bytes()).then_some(user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_uri_validation() {
        assert!(validate_redirect_uri("http://localhost:8080/cb").is_ok());
        assert!(validate_redirect_uri("https://app.example/cb?x=1").is_ok());
        for bad in [
            "",
            "/relative",
            "javascript:alert(1)",
            "ftp://host/x",
            "http://host/cb#frag",
            "http://host/c b",
            "http://host/cb\r\nSet-Cookie: x=1",
            "http:///nohost",
        ] {
            assert!(validate_redirect_uri(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn location_is_encoded() {
        let v = redirect_location(
            "http://a.example/cb?x=1",
            &[("code", "c"), ("state", "a b\n&<")],
        )
        .expect("location");
        assert_eq!(
            v.to_str().expect("ascii"),
            "http://a.example/cb?x=1&code=c&state=a+b%0A%26%3C"
        );
        let v = redirect_location("http://a.example/cb", &[("code", "c"), ("state", "")])
            .expect("location");
        assert_eq!(v.to_str().expect("ascii"), "http://a.example/cb?code=c");
        assert!(redirect_location("javascript:x", &[("code", "c")]).is_none());
    }

    #[test]
    fn registration_validation() {
        let reg = ClientRegistry::new(2, 60);
        let ok = reg
            .register(&json!({"redirect_uris": ["http://localhost/cb"], "client_name": "x"}))
            .expect("registered");
        let id = ok["client_id"].as_str().expect("id");
        assert!(ok["client_secret"].is_string());
        let client = reg.get(id).expect("stored");
        assert!(client.redirect_allowed("http://localhost/cb"));
        assert!(!client.redirect_allowed("http://localhost/other"));

        let public = reg
            .register(&json!({"redirect_uris": ["http://localhost/cb"], "token_endpoint_auth_method": "none"}))
            .expect("registered");
        assert!(public.get("client_secret").is_none());

        assert_eq!(
            reg.register(&json!({})).unwrap_err().0,
            "invalid_redirect_uri"
        );
        assert_eq!(
            reg.register(&json!({"redirect_uris": ["javascript:x"]}))
                .unwrap_err()
                .0,
            "invalid_redirect_uri"
        );
        assert_eq!(
            reg.register(&json!({"grant_types": ["magic"], "redirect_uris": ["http://a/cb"]}))
                .unwrap_err()
                .0,
            "invalid_client_metadata"
        );
        // Client-credentials-only registration needs no redirect URI.
        assert!(reg
            .register(&json!({"grant_types": ["client_credentials"]}))
            .is_ok());
    }

    #[test]
    fn demo_users() {
        assert!(authenticate_user("demo", "demo").is_some());
        assert!(authenticate_user("alice", "wrong").is_none());
        assert!(authenticate_user("nobody", "x").is_none());
    }
}
