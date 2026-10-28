//! Identities (`docs/plans/harness.md`, phase 6): what a layer's requests
//! carry to tell clients apart. A layer cycles through its identities;
//! identity `n` takes the `n`th of each pool (wrapping): an API key (listed,
//! or minted as `<prefix><n>`), a JWT minted by the node's built-in IdP
//! (subject `<prefix><n>`, signed on first use and again before it expires),
//! an OAuth access token fetched with client credentials (cached, refreshed
//! before it expires; one fetch at a time per client), a forwarded address
//! from a range, and a local source address to connect from.
//!
//! Secrets stay here and in the requests: reports name an identity by a
//! label that carries none (`key 3`, the JWT subject, the OAuth client id,
//! the forwarded address).

use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Uri};
use serde_json::{json, Map, Value};

use super::engine::PinnedTarget;
use super::scenario::{parse_cidr, Identities, JwtAlg};
use crate::jwt_state::JwtState;

/// The range forwarded addresses come from by default (RFC 2544).
pub const DEFAULT_CIDR: &str = "198.18.0.0/15";
/// The longest a token fetch may take.
pub const TOKEN_TIMEOUT: Duration = Duration::from_secs(10);
/// The largest token answer read.
pub const MAX_TOKEN_ANSWER: usize = 64 * 1024;
/// A cached token is renewed this long before it expires (or at a tenth
/// of its lifetime, when that is shorter).
pub const RENEW_BEFORE: Duration = Duration::from_secs(30);

/// What one identity adds to a request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resolved {
    pub headers: Vec<(String, String)>,
    pub query: Option<(String, String)>,
    /// The forwarded address (`{{ip}}`).
    pub ip: Option<String>,
    /// The local address to connect from.
    pub source: Option<IpAddr>,
    /// Why a credential is missing (the request goes without it).
    pub error: Option<String>,
}

struct ApiKeyPool {
    header: Option<String>,
    query: Option<String>,
    values: Vec<String>,
    prefix: String,
    minted: u32,
}

impl ApiKeyPool {
    fn len(&self) -> u32 {
        if self.values.is_empty() {
            self.minted.max(1)
        } else {
            self.values.len() as u32
        }
    }

    fn key(&self, i: u32) -> String {
        if self.values.is_empty() {
            format!("{}{}", self.prefix, i % self.len())
        } else {
            self.values[(i % self.len()) as usize].clone()
        }
    }
}

struct JwtPool {
    state: Arc<JwtState>,
    alg: JwtAlg,
    subjects: u32,
    prefix: String,
    issuer: String,
    audience: Option<String>,
    scope: Option<String>,
    ttl: Duration,
    /// Per subject: the token and its expiry (Unix seconds).
    tokens: Vec<Mutex<Option<(String, i64)>>>,
}

impl JwtPool {
    fn subject(&self, i: u32) -> String {
        format!("{}{}", self.prefix, i % self.subjects.max(1))
    }

    fn token(&self, i: u32) -> Result<String, String> {
        let slot = &self.tokens[(i % self.subjects.max(1)) as usize];
        let now = chrono::Utc::now().timestamp();
        let mut cached = slot.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((t, exp)) = cached.as_ref() {
            if *exp - now > RENEW_BEFORE.as_secs() as i64 {
                return Ok(t.clone());
            }
        }
        let exp = now + self.ttl.as_secs() as i64;
        let mut claims = Map::new();
        claims.insert("iss".into(), json!(self.issuer));
        claims.insert("sub".into(), json!(self.subject(i)));
        claims.insert("iat".into(), json!(now));
        claims.insert("nbf".into(), json!(now));
        claims.insert("exp".into(), json!(exp));
        claims.insert("jti".into(), json!(uuid::Uuid::new_v4().to_string()));
        if let Some(a) = &self.audience {
            claims.insert("aud".into(), json!(a));
        }
        if let Some(s) = &self.scope {
            claims.insert("scope".into(), json!(s));
        }
        let token = self.sign(&claims)?;
        *cached = Some((token.clone(), exp));
        Ok(token)
    }

    fn sign(&self, claims: &Map<String, Value>) -> Result<String, String> {
        match self.alg {
            JwtAlg::Rs256 => self.state.sign_rs256(claims, "JWT"),
            JwtAlg::Hs256 => self.state.sign_hs256(claims),
        }
    }

    /// A token of subject `i` that expired a minute ago (credential churn).
    fn expired(&self, i: u32) -> Result<String, String> {
        let now = chrono::Utc::now().timestamp();
        let mut claims = Map::new();
        claims.insert("iss".into(), json!(self.issuer));
        claims.insert("sub".into(), json!(self.subject(i)));
        claims.insert("iat".into(), json!(now - 3600));
        claims.insert("exp".into(), json!(now - 60));
        if let Some(a) = &self.audience {
            claims.insert("aud".into(), json!(a));
        }
        self.sign(&claims)
    }
}

/// OAuth client credentials: one cached token per client.
pub struct OAuthPool {
    endpoint: PinnedTarget,
    clients: Vec<(String, String)>,
    scope: Option<String>,
    audience: Option<String>,
    tokens: Vec<tokio::sync::Mutex<Option<(String, Instant)>>>,
}

impl OAuthPool {
    pub fn new(
        endpoint: PinnedTarget,
        clients: Vec<(String, String)>,
        scope: Option<String>,
        audience: Option<String>,
    ) -> Self {
        let tokens = clients
            .iter()
            .map(|_| tokio::sync::Mutex::new(None))
            .collect();
        Self {
            endpoint,
            clients,
            scope,
            audience,
            tokens,
        }
    }

    pub fn client_id(&self, i: u32) -> &str {
        &self.clients[(i as usize) % self.clients.len().max(1)].0
    }

    /// The token of client `i`: cached, or fetched (one fetch at a time
    /// per client; the others wait for it).
    pub async fn token(&self, i: u32) -> Result<String, String> {
        let k = (i as usize) % self.clients.len().max(1);
        let mut slot = self.tokens[k].lock().await;
        if let Some((t, expires)) = slot.as_ref() {
            if expires.saturating_duration_since(Instant::now()) > Duration::ZERO {
                return Ok(t.clone());
            }
        }
        let (token, lifetime) = self.fetch(k).await?;
        let renew = RENEW_BEFORE.min(lifetime / 10);
        *slot = Some((
            token.clone(),
            Instant::now() + lifetime.saturating_sub(renew),
        ));
        Ok(token)
    }

    async fn fetch(&self, k: usize) -> Result<(String, Duration), String> {
        let (id, secret) = &self.clients[k];
        let mut form = vec![
            ("grant_type", "client_credentials".to_string()),
            ("client_id", id.clone()),
            ("client_secret", secret.clone()),
        ];
        if let Some(s) = &self.scope {
            form.push(("scope", s.clone()));
        }
        if let Some(a) = &self.audience {
            form.push(("audience", a.clone()));
        }
        let body: String = form
            .iter()
            .map(|(k, v)| format!("{k}={}", form_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let uri: Uri = format!("{}{}", self.endpoint.authority, self.endpoint.parsed.path)
            .parse()
            .map_err(|e| format!("token URL: {e}"))?;
        let req = Request::post(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("accept", "application/json")
            .header(
                "user-agent",
                concat!("rustybin-harness/", env!("CARGO_PKG_VERSION")),
            )
            .body(Full::new(Bytes::from(body)).boxed())
            .map_err(|e| format!("token request: {e}"))?;
        let answer = tokio::time::timeout(TOKEN_TIMEOUT, async {
            let resp = self
                .endpoint
                .client
                .request(req)
                .await
                .map_err(|e| format!("token endpoint: {e}"))?;
            let status = resp.status().as_u16();
            let mut body = resp.into_body();
            let mut bytes = Vec::new();
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|e| format!("token endpoint: {e}"))?;
                if let Some(d) = frame.data_ref() {
                    if bytes.len() + d.len() > MAX_TOKEN_ANSWER {
                        return Err("the token answer is too large".to_string());
                    }
                    bytes.extend_from_slice(d);
                }
            }
            Ok::<_, String>((status, bytes))
        })
        .await
        .map_err(|_| "the token endpoint did not answer in time".to_string())??;
        let (status, bytes) = answer;
        if !(200..300).contains(&status) {
            return Err(format!("the token endpoint answered {status}"));
        }
        let v: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "the token answer is not JSON".to_string())?;
        let token = v
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty() && t.len() <= 16 * 1024)
            .ok_or("the token answer has no access_token")?;
        let lifetime = v
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or(3600)
            .clamp(1, 24 * 3600);
        Ok((token.to_string(), Duration::from_secs(lifetime)))
    }
}

fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A layer's identities, ready to use.
pub struct LayerIdentities {
    pub size: u32,
    api: Option<ApiKeyPool>,
    jwt: Option<JwtPool>,
    oauth: Option<Arc<OAuthPool>>,
    forwarded: Option<(String, u32, u64)>,
    sources: Vec<IpAddr>,
}

impl LayerIdentities {
    /// Build a layer's identities. `oauth` is the pool for its OAuth
    /// clients (made by the caller, which pins the token URL).
    pub fn build(
        ids: &Identities,
        jwt: Option<Arc<JwtState>>,
        oauth: Option<Arc<OAuthPool>>,
    ) -> Self {
        let size = ids.size();
        let api = ids.api_keys.as_ref().map(|k| ApiKeyPool {
            header: match (&k.header, &k.query) {
                (Some(h), _) => Some(h.to_ascii_lowercase()),
                (None, Some(_)) => None,
                (None, None) => Some("apikey".into()),
            },
            query: k.query.clone(),
            values: k.values.clone(),
            prefix: k.prefix.clone().unwrap_or_else(|| "harness-key-".into()),
            minted: k.mint.unwrap_or(0),
        });
        let jwt = match (&ids.jwt, jwt) {
            (Some(j), Some(state)) => {
                let subjects = j
                    .mint
                    .unwrap_or(size)
                    .clamp(1, super::scenario::MAX_JWT_SUBJECTS);
                Some(JwtPool {
                    state,
                    alg: j.alg.unwrap_or_default(),
                    subjects,
                    prefix: j
                        .subject_prefix
                        .clone()
                        .unwrap_or_else(|| "harness-user-".into()),
                    issuer: j
                        .issuer
                        .clone()
                        .unwrap_or_else(|| "rustybin-harness".into()),
                    audience: j.audience.clone(),
                    scope: j.scope.clone(),
                    ttl: j.ttl.map_or(Duration::from_secs(3600), |d| d.0),
                    tokens: (0..subjects).map(|_| Mutex::new(None)).collect(),
                })
            }
            _ => None,
        };
        let forwarded = ids.forwarded.as_ref().map(|f| {
            let (base, range) = parse_cidr(f.cidr.as_deref().unwrap_or(DEFAULT_CIDR))
                .unwrap_or((0xC612_0000, 1 << 17));
            (
                f.header
                    .clone()
                    .unwrap_or_else(|| "x-forwarded-for".into())
                    .to_ascii_lowercase(),
                base,
                range.min(u64::from(f.count.max(1))),
            )
        });
        let sources = ids
            .source_addresses
            .iter()
            .filter_map(|a| a.parse().ok())
            .collect();
        Self {
            size: size.max(1),
            api,
            jwt,
            oauth,
            forwarded,
            sources,
        }
    }

    /// The forwarded address of identity `i`.
    pub fn forwarded_ip(&self, i: u32) -> Option<String> {
        self.forwarded.as_ref().map(|(_, base, count)| {
            let n = u64::from(i) % (*count).max(1);
            std::net::Ipv4Addr::from(base.wrapping_add(n as u32)).to_string()
        })
    }

    /// The source addresses the layer connects from.
    pub fn sources(&self) -> &[IpAddr] {
        &self.sources
    }

    /// Whether the layer mints JWTs (the `expired_tokens` fault needs them).
    pub fn has_jwt(&self) -> bool {
        self.jwt.is_some()
    }

    /// [`LayerIdentities::resolve`] with an expired JWT instead of a valid
    /// one (the `expired_tokens` fault).
    pub async fn resolve_expired(&self, i: u32) -> Resolved {
        let mut out = self.resolve(i).await;
        if let Some(j) = &self.jwt {
            match j.expired(i) {
                Ok(t) => {
                    out.headers.retain(|(n, _)| n != "authorization");
                    out.headers
                        .push(("authorization".into(), format!("Bearer {t}")));
                }
                Err(e) => out.error = Some(format!("expired JWT for {}: {e}", j.subject(i))),
            }
        }
        out
    }

    /// What identity `i` adds to a request.
    pub async fn resolve(&self, i: u32) -> Resolved {
        let mut out = Resolved::default();
        if let Some(k) = &self.api {
            let key = k.key(i);
            match (&k.header, &k.query) {
                (Some(h), _) => out.headers.push((h.clone(), key)),
                (None, Some(q)) => out.query = Some((q.clone(), key)),
                (None, None) => {}
            }
        }
        if let Some(j) = &self.jwt {
            match j.token(i) {
                Ok(t) => out
                    .headers
                    .push(("authorization".into(), format!("Bearer {t}"))),
                Err(e) => out.error = Some(format!("JWT for {}: {e}", j.subject(i))),
            }
        }
        if let Some(o) = &self.oauth {
            match o.token(i).await {
                Ok(t) => out
                    .headers
                    .push(("authorization".into(), format!("Bearer {t}"))),
                Err(e) => out.error = Some(format!("OAuth client {}: {e}", o.client_id(i))),
            }
        }
        if let (Some((header, _, _)), Some(ip)) = (&self.forwarded, self.forwarded_ip(i)) {
            out.headers.push((header.clone(), ip.clone()));
            out.ip = Some(ip);
        }
        if !self.sources.is_empty() {
            out.source = Some(self.sources[(i as usize) % self.sources.len()]);
        }
        out
    }

    /// A name for identity `i` that carries no secret, for the report.
    pub fn label(&self, i: u32) -> String {
        let i = i % self.size;
        if let Some(j) = &self.jwt {
            return j.subject(i);
        }
        if let Some(o) = &self.oauth {
            return format!("client {}", o.client_id(i));
        }
        if let Some(k) = &self.api {
            return if k.values.is_empty() {
                format!("{}{}", k.prefix, i % k.len())
            } else {
                format!("key {}", i % k.len())
            };
        }
        if let Some(ip) = self.forwarded_ip(i) {
            return ip;
        }
        if !self.sources.is_empty() {
            return format!("from {}", self.sources[(i as usize) % self.sources.len()]);
        }
        format!("identity {i}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(yaml: &str) -> Identities {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[tokio::test]
    async fn pools_wrap_and_labels_carry_no_secret() {
        let l = LayerIdentities::build(
            &ids("api_keys: { values: [secret-a, secret-b] }\nforwarded: { count: 3 }"),
            None,
            None,
        );
        assert_eq!(l.size, 3);
        let r = l.resolve(4).await;
        assert_eq!(r.headers[0], ("apikey".to_string(), "secret-a".to_string()));
        assert_eq!(r.ip.as_deref(), Some("198.18.0.1"));
        assert_eq!(l.label(1), "key 1");
        let minted = LayerIdentities::build(
            &ids("api_keys: { query: key, mint: 5, prefix: k- }\nsource_addresses: [127.0.0.1]"),
            None,
            None,
        );
        let r = minted.resolve(7).await;
        assert_eq!(r.query, Some(("key".to_string(), "k-2".to_string())));
        assert_eq!(r.source, Some("127.0.0.1".parse().unwrap()));
        assert_eq!(minted.label(7), "k-2");
        let ranged = LayerIdentities::build(
            &ids("forwarded: { count: 2, cidr: 203.0.113.8/29, header: X-Real-IP }"),
            None,
            None,
        );
        let r = ranged.resolve(3).await;
        assert_eq!(
            r.headers[0],
            ("x-real-ip".to_string(), "203.0.113.9".to_string())
        );
    }

    #[tokio::test]
    async fn minted_jwts_verify_with_the_idp_key_and_are_reused() {
        let state = JwtState::shared_for_tests();
        let l = LayerIdentities::build(
            &ids("jwt: { mint: 2, audience: api, scope: read }"),
            Some(state.clone()),
            None,
        );
        assert_eq!(l.size, 2);
        let r = l.resolve(3).await;
        let bearer = &r.headers[0].1;
        let token = bearer.strip_prefix("Bearer ").unwrap();
        let claims = state.verify_rs256(token).unwrap();
        assert_eq!(claims["sub"], "harness-user-1");
        assert_eq!(claims["aud"], "api");
        assert_eq!(claims["iss"], "rustybin-harness");
        // Signed once, reused until it nears its expiry.
        assert_eq!(l.resolve(1).await.headers[0].1, *bearer);
        assert_eq!(l.label(1), "harness-user-1");
        let hs = LayerIdentities::build(&ids("jwt: { alg: hs256 }"), Some(state), None);
        assert!(hs.resolve(0).await.headers[0].1.starts_with("Bearer ey"));
    }
}
