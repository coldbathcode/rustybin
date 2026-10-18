//! Mocked security: decide whether a request may reach the mock response.
//!
//! `spec` mode evaluates the operation's `security` requirements (OR of
//! alternatives, AND within one) against `components.securitySchemes`; the
//! other modes put one scheme in front of every operation. Credentials come
//! from the mock's settings; JWTs (bearer, OAuth2, OpenID Connect) are
//! verified with the built-in IdP keys, and OAuth2 scopes are checked
//! against the token's `scope` / `scp` claim.

use axum::http::HeaderMap;
use base64::Engine;
use serde::Serialize;
use serde_json::{json, Value};

use super::compile::Compiled;
use super::model::SecurityMode;
use super::spec::{Location, Operation, Requirement, SchemeKind};
use crate::control_auth::constant_time_eq;
use crate::jwt_state::{JwtState, VerifyOptions};

/// Name / value pairs (headers, query parameters).
pub type Pairs = Vec<(String, String)>;

/// Request facts security looks at.
pub struct AuthContext<'a> {
    pub headers: &'a HeaderMap,
    pub query: &'a [(String, String)],
    pub jwt: &'a JwtState,
    /// A client certificate from the demo CA was presented.
    pub client_cert: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthFailure {
    /// 401 (no or bad credentials) or 403 (valid credentials, missing scope).
    pub status: u16,
    pub reason: String,
    /// `WWW-Authenticate` challenges.
    pub challenges: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthResult {
    /// Security off, operation public, or nothing required.
    NotRequired,
    /// Accepted; the text names the scheme(s).
    Passed(String),
    Failed(AuthFailure),
}

impl AuthResult {
    /// Short label for captures (`ok: api_key`, `failed: ...`).
    pub fn label(&self) -> Option<String> {
        match self {
            AuthResult::NotRequired => None,
            AuthResult::Passed(s) => Some(format!("ok: {s}")),
            AuthResult::Failed(f) => Some(format!("failed: {}", f.reason)),
        }
    }
}

enum Check {
    Pass,
    /// Credentials missing or wrong.
    Unauthorized(String),
    /// Credentials fine, scope missing (reason, the missing scopes).
    Forbidden(String, String),
}

/// The RFC 6750 challenge of a token that lacks scopes.
fn insufficient_scope(scopes: &str) -> String {
    format!("Bearer realm=\"mock\", error=\"insufficient_scope\", scope=\"{scopes}\"")
}

/// `jwt.issuer: builtin` (or none) accepts any issuer: the signature already
/// proves the built-in IdP issued the token.
fn required_issuer(issuer: Option<&str>) -> Option<&str> {
    issuer.filter(|i| !i.trim().is_empty() && !i.eq_ignore_ascii_case("builtin"))
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get("authorization")?.to_str().ok()?.trim();
    let (scheme, rest) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(rest.trim())
}

fn basic(headers: &HeaderMap) -> Option<(String, String)> {
    let v = headers.get("authorization")?.to_str().ok()?.trim();
    let (scheme, rest) = v.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(rest.trim())
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (u, p) = text.split_once(':')?;
    Some((u.to_string(), p.to_string()))
}

fn token_scopes(claims: &Value) -> Vec<String> {
    match claims.get("scope").or_else(|| claims.get("scp")) {
        Some(Value::String(s)) => s.split_whitespace().map(str::to_string).collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

impl Compiled {
    fn check_api_key(&self, name: &str, location: Location, ctx: &AuthContext) -> Check {
        let presented = match location {
            Location::Header => ctx
                .headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
            Location::Query => ctx
                .query
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone()),
            Location::Cookie => ctx
                .headers
                .get_all("cookie")
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|v| v.split(';'))
                .filter_map(|c| c.trim().split_once('='))
                .find(|(k, _)| k.trim() == name)
                .map(|(_, v)| v.trim().to_string()),
            Location::Path => None,
        };
        let Some(key) = presented else {
            return Check::Unauthorized(format!("missing API key ({} {name})", location.as_str()));
        };
        if self
            .settings
            .security
            .api_key
            .keys
            .iter()
            .any(|k| constant_time_eq(k, &key))
        {
            Check::Pass
        } else {
            Check::Unauthorized("invalid API key".into())
        }
    }

    fn check_basic(&self, ctx: &AuthContext) -> Check {
        let Some((user, pass)) = basic(ctx.headers) else {
            return Check::Unauthorized("missing basic credentials".into());
        };
        let ok = self
            .settings
            .security
            .basic
            .users
            .get(&user)
            .is_some_and(|p| constant_time_eq(p, &pass));
        if ok {
            Check::Pass
        } else {
            Check::Unauthorized("invalid username or password".into())
        }
    }

    /// Static tokens (all scopes) or a JWT signed by the built-in IdP.
    fn check_token(&self, ctx: &AuthContext, scopes: &[String], allow_static: bool) -> Check {
        let Some(token) = bearer(ctx.headers) else {
            return Check::Unauthorized("missing bearer token".into());
        };
        if allow_static
            && self
                .settings
                .security
                .bearer
                .tokens
                .iter()
                .any(|t| constant_time_eq(t, token))
        {
            return Check::Pass;
        }
        let jwt = &self.settings.security.jwt;
        let opts = VerifyOptions {
            issuer: required_issuer(jwt.issuer.as_deref()),
            audience: jwt.audience.as_deref(),
        };
        let verified = match ctx.jwt.verify(token, &opts) {
            Ok(v) => v,
            Err(e) => return Check::Unauthorized(format!("invalid token: {e}")),
        };
        let granted = token_scopes(&verified.claims);
        let missing: Vec<&String> = scopes.iter().filter(|s| !granted.contains(s)).collect();
        if missing.is_empty() {
            Check::Pass
        } else {
            let names: Vec<&str> = missing.iter().map(|s| s.as_str()).collect();
            let names = names.join(" ");
            Check::Forbidden(format!("insufficient scope (needs {names})"), names)
        }
    }

    fn check_scheme(&self, scheme: &str, scopes: &[String], ctx: &AuthContext) -> Check {
        let Some(s) = self.spec.scheme(scheme) else {
            // Undefined scheme (a spec warning): nothing to check against.
            return Check::Pass;
        };
        match &s.kind {
            SchemeKind::ApiKey { name, location } => self.check_api_key(name, *location, ctx),
            SchemeKind::Basic => self.check_basic(ctx),
            SchemeKind::Bearer { .. } => self.check_token(ctx, scopes, true),
            SchemeKind::OAuth2 { .. } | SchemeKind::OpenIdConnect => {
                self.check_token(ctx, scopes, true)
            }
            SchemeKind::MutualTls => {
                if ctx.client_cert {
                    Check::Pass
                } else {
                    Check::Unauthorized("no client certificate from the demo CA".into())
                }
            }
            SchemeKind::Unsupported { .. } => Check::Pass,
        }
    }

    fn challenge(&self, scheme: &str) -> Option<String> {
        let realm = "realm=\"mock\"";
        match self.spec.scheme(scheme).map(|s| &s.kind) {
            Some(SchemeKind::Basic) => Some(format!("Basic {realm}")),
            Some(SchemeKind::Bearer { .. })
            | Some(SchemeKind::OAuth2 { .. })
            | Some(SchemeKind::OpenIdConnect) => Some(format!("Bearer {realm}")),
            _ => None,
        }
    }

    /// Decide whether the request may proceed.
    pub fn authorize(&self, op: &Operation, ctx: &AuthContext) -> AuthResult {
        let sec = &self.settings.security;
        if !sec.enabled
            || self
                .settings
                .operations
                .get(&op.id)
                .is_some_and(|o| o.public)
        {
            return AuthResult::NotRequired;
        }
        let fixed = |check: Check, label: &str, challenge: Option<String>| match check {
            Check::Pass => AuthResult::Passed(label.to_string()),
            Check::Unauthorized(reason) => AuthResult::Failed(AuthFailure {
                status: 401,
                reason,
                challenges: challenge.into_iter().collect(),
            }),
            Check::Forbidden(reason, scopes) => AuthResult::Failed(AuthFailure {
                status: 403,
                reason,
                challenges: vec![insufficient_scope(&scopes)],
            }),
        };
        let realm = "realm=\"mock\"";
        match sec.mode {
            SecurityMode::ApiKey => fixed(
                self.check_api_key(&sec.api_key.header, Location::Header, ctx),
                "api_key",
                None,
            ),
            SecurityMode::Basic => fixed(
                self.check_basic(ctx),
                "basic",
                Some(format!("Basic {realm}")),
            ),
            SecurityMode::Bearer => fixed(
                self.check_token(ctx, &[], true),
                "bearer",
                Some(format!("Bearer {realm}")),
            ),
            SecurityMode::Jwt => fixed(
                self.check_token(ctx, &[], false),
                "jwt",
                Some(format!("Bearer {realm}")),
            ),
            SecurityMode::Spec => self.authorize_spec(&op.security, ctx),
        }
    }

    fn authorize_spec(&self, alternatives: &[Requirement], ctx: &AuthContext) -> AuthResult {
        if alternatives.is_empty() {
            return AuthResult::NotRequired;
        }
        let mut forbidden: Option<(String, String)> = None;
        let mut reasons: Vec<String> = Vec::new();
        let mut challenges: Vec<String> = Vec::new();
        for alt in alternatives {
            let mut failed = false;
            for (scheme, scopes) in alt {
                match self.check_scheme(scheme, scopes, ctx) {
                    Check::Pass => {}
                    Check::Unauthorized(r) => {
                        failed = true;
                        if !reasons.contains(&r) {
                            reasons.push(r);
                        }
                        if let Some(c) = self.challenge(scheme) {
                            if !challenges.contains(&c) {
                                challenges.push(c);
                            }
                        }
                    }
                    Check::Forbidden(r, scopes) => {
                        failed = true;
                        forbidden.get_or_insert((r, scopes));
                    }
                }
            }
            if !failed {
                let names: Vec<&str> = alt.iter().map(|(s, _)| s.as_str()).collect();
                return AuthResult::Passed(names.join(" + "));
            }
        }
        match forbidden {
            Some((reason, scopes)) => AuthResult::Failed(AuthFailure {
                status: 403,
                reason,
                challenges: vec![insufficient_scope(&scopes)],
            }),
            None => AuthResult::Failed(AuthFailure {
                status: 401,
                reason: reasons.join("; "),
                challenges,
            }),
        }
    }

    /// Headers and query parameters that satisfy security for `op`
    /// (demo credentials), for samples, Try it and curl commands.
    pub fn sample_credentials(&self, op: &Operation) -> (Pairs, Pairs) {
        self.credentials_for(op, true)
    }

    /// [`Self::sample_credentials`] with placeholders (`<api-key>`,
    /// `Basic <base64 user:password>`, `Bearer <token>`) instead of the
    /// configured secrets, in the same order.
    pub fn placeholder_credentials(&self, op: &Operation) -> (Pairs, Pairs) {
        self.credentials_for(op, false)
    }

    fn credentials_for(&self, op: &Operation, reveal: bool) -> (Pairs, Pairs) {
        let sec = &self.settings.security;
        let mut headers = Vec::new();
        let mut query = Vec::new();
        if !sec.enabled
            || self
                .settings
                .operations
                .get(&op.id)
                .is_some_and(|o| o.public)
        {
            return (headers, query);
        }
        let key = match sec.api_key.keys.first() {
            Some(_) if !reveal => "<api-key>".to_string(),
            k => k.cloned().unwrap_or_default(),
        };
        let basic_header = sec.basic.users.iter().next().map(|(u, p)| {
            let enc = if reveal {
                base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"))
            } else {
                "<base64 user:password>".to_string()
            };
            ("Authorization".to_string(), format!("Basic {enc}"))
        });
        let token =
            sec.bearer
                .tokens
                .first()
                .map(|t| if reveal { t.clone() } else { "<token>".into() });
        match sec.mode {
            SecurityMode::ApiKey => headers.push((sec.api_key.header.clone(), key)),
            SecurityMode::Basic => headers.extend(basic_header),
            SecurityMode::Bearer => {
                if let Some(t) = token {
                    headers.push(("Authorization".into(), format!("Bearer {t}")));
                }
            }
            SecurityMode::Jwt => {}
            SecurityMode::Spec => {
                let Some(alt) = op.security.first() else {
                    return (headers, query);
                };
                for (scheme, _) in alt {
                    match self.spec.scheme(scheme).map(|s| &s.kind) {
                        Some(SchemeKind::ApiKey { name, location }) => match location {
                            Location::Query => query.push((name.clone(), key.clone())),
                            Location::Cookie => {
                                headers.push(("Cookie".into(), format!("{name}={key}")))
                            }
                            _ => headers.push((name.clone(), key.clone())),
                        },
                        Some(SchemeKind::Basic) => headers.extend(basic_header.clone()),
                        Some(SchemeKind::Bearer { .. })
                        | Some(SchemeKind::OAuth2 { .. })
                        | Some(SchemeKind::OpenIdConnect) => {
                            if let Some(t) = &token {
                                headers.push(("Authorization".into(), format!("Bearer {t}")));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        (headers, query)
    }

    /// What a client needs to know to call the mock (shown in the console).
    /// Without `reveal` the secrets are [`super::model::REDACTED`].
    pub fn credentials_summary(&self, reveal: bool) -> Value {
        let redacted;
        let sec = if reveal {
            &self.settings.security
        } else {
            redacted = self.settings.security.redacted();
            &redacted
        };
        let schemes: Vec<Value> = match sec.mode {
            SecurityMode::Spec => self
                .spec
                .security_schemes
                .iter()
                .filter(|s| {
                    self.spec
                        .operations
                        .iter()
                        .any(|o| o.security.iter().any(|alt| alt.iter().any(|(n, _)| n == &s.name)))
                })
                .map(|s| {
                    let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
                    if let Value::Object(m) = &mut v {
                        let hint = match &s.kind {
                            SchemeKind::ApiKey { name, location } => {
                                format!("{} {name}: one of the configured API keys", location.as_str())
                            }
                            SchemeKind::Basic => "Authorization: Basic with a configured user".into(),
                            SchemeKind::Bearer { .. } => {
                                "Authorization: Bearer with a configured token or a JWT from the built-in IdP".into()
                            }
                            SchemeKind::OAuth2 { .. } | SchemeKind::OpenIdConnect => {
                                "Authorization: Bearer with a JWT from the built-in IdP (POST /oauth/token); scopes are checked".into()
                            }
                            SchemeKind::MutualTls => "a client certificate from the demo CA".into(),
                            SchemeKind::Unsupported { detail } => detail.clone(),
                        };
                        m.insert("hint".into(), json!(hint));
                    }
                    v
                })
                .collect(),
            mode => vec![json!({ "name": mode.as_str(), "type": mode.as_str() })],
        };
        json!({
            "enabled": sec.enabled,
            "mode": sec.mode.as_str(),
            "schemes": schemes,
            "api_key": { "header": sec.api_key.header, "keys": sec.api_key.keys },
            "basic": { "users": sec.basic.users.keys().collect::<Vec<_>>() },
            "bearer": { "tokens": sec.bearer.tokens },
            "jwt": sec.jwt,
            "token_endpoint": "/oauth/token",
            "hidden": !reveal,
        })
    }
}

/// Serialisable view of a failure (response body).
#[derive(Serialize)]
pub struct FailureBody<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub title: &'static str,
    pub status: u16,
    pub detail: &'a str,
    pub mock: &'a str,
    pub operation: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::compile::tests::compile;
    use crate::mocks::model::MockSettings;
    use axum::http::HeaderValue;
    use std::sync::Arc;

    const SPEC: &str = r##"{
      "openapi": "3.0.3", "info": {"title": "t", "version": "1"},
      "security": [{"key": []}, {"oauth": ["pets:read"]}],
      "paths": {
        "/a": {"get": {"operationId": "a", "responses": {"200": {"description": "ok"}}}},
        "/b": {"get": {"operationId": "b", "security": [{"key": [], "basic": []}], "responses": {"200": {"description": "ok"}}}},
        "/c": {"get": {"operationId": "c", "security": [], "responses": {"200": {"description": "ok"}}}},
        "/d": {"get": {"operationId": "d", "security": [{"cert": []}], "responses": {"200": {"description": "ok"}}}}
      },
      "components": {"securitySchemes": {
        "key": {"type": "apiKey", "name": "X-Key", "in": "header"},
        "basic": {"type": "http", "scheme": "basic"},
        "oauth": {"type": "oauth2", "flows": {"clientCredentials": {"tokenUrl": "/oauth/token", "scopes": {"pets:read": ""}}}},
        "cert": {"type": "mutualTLS"}
      }}
    }"##;

    fn secured() -> MockSettings {
        let mut s = MockSettings::default();
        s.security.enabled = true;
        s
    }

    fn ctx<'a>(headers: &'a HeaderMap, jwt: &'a JwtState, cert: bool) -> AuthContext<'a> {
        AuthContext {
            headers,
            query: &[],
            jwt,
            client_cert: cert,
        }
    }

    fn with(pairs: &[(&'static str, String)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).expect("value"));
        }
        h
    }

    fn jwt_with_scope(jwt: &JwtState, scope: &str) -> String {
        let claims = json!({ "sub": "demo", "scope": scope, "exp": 4_102_444_800u64 });
        jwt.sign_rs256(claims.as_object().expect("obj"), "at+jwt")
            .expect("signed")
    }

    #[test]
    fn spec_mode_alternatives_and_scopes() {
        let jwt: Arc<JwtState> = JwtState::shared_for_tests();
        let c = compile(SPEC, secured());
        let a = c.spec.operation("a").expect("a");
        let none = HeaderMap::new();
        match c.authorize(a, &ctx(&none, &jwt, false)) {
            AuthResult::Failed(f) => {
                assert_eq!(f.status, 401);
                assert_eq!(f.challenges, vec!["Bearer realm=\"mock\""]);
            }
            other => panic!("{other:?}"),
        }
        let key = with(&[("x-key", "demo-key".into())]);
        assert_eq!(
            c.authorize(a, &ctx(&key, &jwt, false)),
            AuthResult::Passed("key".into())
        );
        let wrong = with(&[("x-key", "nope".into())]);
        assert!(
            matches!(c.authorize(a, &ctx(&wrong, &jwt, false)), AuthResult::Failed(f) if f.status == 401)
        );

        let good = with(&[(
            "authorization",
            format!("Bearer {}", jwt_with_scope(&jwt, "openid pets:read")),
        )]);
        assert_eq!(
            c.authorize(a, &ctx(&good, &jwt, false)),
            AuthResult::Passed("oauth".into())
        );
        let scopeless = with(&[(
            "authorization",
            format!("Bearer {}", jwt_with_scope(&jwt, "openid")),
        )]);
        assert!(
            matches!(c.authorize(a, &ctx(&scopeless, &jwt, false)), AuthResult::Failed(f) if f.status == 403)
        );
        let static_token = with(&[("authorization", "Bearer demo-token".into())]);
        assert!(matches!(
            c.authorize(a, &ctx(&static_token, &jwt, false)),
            AuthResult::Passed(_)
        ));

        let b = c.spec.operation("b").expect("b");
        assert!(matches!(
            c.authorize(b, &ctx(&key, &jwt, false)),
            AuthResult::Failed(_)
        ));
        let both = with(&[
            ("x-key", "demo-key".into()),
            ("authorization", "Basic ZGVtbzpkZW1v".into()),
        ]);
        assert_eq!(
            c.authorize(b, &ctx(&both, &jwt, false)),
            AuthResult::Passed("key + basic".into())
        );

        let cop = c.spec.operation("c").expect("c");
        assert_eq!(
            c.authorize(cop, &ctx(&none, &jwt, false)),
            AuthResult::NotRequired
        );
        let d = c.spec.operation("d").expect("d");
        assert!(matches!(
            c.authorize(d, &ctx(&none, &jwt, false)),
            AuthResult::Failed(_)
        ));
        assert!(matches!(
            c.authorize(d, &ctx(&none, &jwt, true)),
            AuthResult::Passed(_)
        ));

        // Credentials helper satisfies security.
        let (h, q) = c.sample_credentials(b);
        assert!(q.is_empty());
        let mut map = HeaderMap::new();
        for (k, v) in h {
            map.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).expect("name"),
                HeaderValue::from_str(&v).expect("value"),
            );
        }
        assert!(matches!(
            c.authorize(b, &ctx(&map, &jwt, false)),
            AuthResult::Passed(_)
        ));
    }

    #[test]
    fn key_locations_openid_connect_and_scope_challenges() {
        const SPEC2: &str = r##"{
          "openapi": "3.0.3", "info": {"title": "t", "version": "1"},
          "paths": {
            "/cookie": {"get": {"operationId": "cookie", "security": [{"session": []}], "responses": {"200": {"description": "ok"}}}},
            "/query": {"get": {"operationId": "query", "security": [{"qkey": []}], "responses": {"200": {"description": "ok"}}}},
            "/oidc": {"get": {"operationId": "oidc", "security": [{"oidc": ["orders:read", "orders:write"]}], "responses": {"200": {"description": "ok"}}}}
          },
          "components": {"securitySchemes": {
            "session": {"type": "apiKey", "name": "sid", "in": "cookie"},
            "qkey": {"type": "apiKey", "name": "api_key", "in": "query"},
            "oidc": {"type": "openIdConnect", "openIdConnectUrl": "https://idp.example/.well-known/openid-configuration"}
          }}
        }"##;
        let jwt: Arc<JwtState> = JwtState::shared_for_tests();
        let mut settings = secured();
        settings.security.jwt.issuer = Some("builtin".into());
        let c = compile(SPEC2, settings);
        let none = HeaderMap::new();

        let cookie = c.spec.operation("cookie").expect("cookie");
        let jar = with(&[("cookie", "theme=dark; sid=demo-key".into())]);
        assert_eq!(
            c.authorize(cookie, &ctx(&jar, &jwt, false)),
            AuthResult::Passed("session".into())
        );
        let other = with(&[("cookie", "sid=nope".into())]);
        assert!(
            matches!(c.authorize(cookie, &ctx(&other, &jwt, false)), AuthResult::Failed(f) if f.status == 401)
        );

        let query = c.spec.operation("query").expect("query");
        let q = vec![("api_key".to_string(), "demo-key".to_string())];
        let qctx = AuthContext {
            headers: &none,
            query: &q,
            jwt: &jwt,
            client_cert: false,
        };
        assert!(matches!(c.authorize(query, &qctx), AuthResult::Passed(_)));
        assert!(
            matches!(c.authorize(query, &ctx(&none, &jwt, false)), AuthResult::Failed(f) if f.reason.contains("query api_key"))
        );

        // OpenID Connect: a token from the built-in IdP (any issuer with
        // `builtin`), scopes from the requirement, RFC 6750 challenge on 403.
        let oidc = c.spec.operation("oidc").expect("oidc");
        let both = with(&[(
            "authorization",
            format!(
                "Bearer {}",
                jwt_with_scope(&jwt, "orders:read orders:write")
            ),
        )]);
        assert_eq!(
            c.authorize(oidc, &ctx(&both, &jwt, false)),
            AuthResult::Passed("oidc".into())
        );
        let read_only = with(&[(
            "authorization",
            format!("Bearer {}", jwt_with_scope(&jwt, "orders:read")),
        )]);
        match c.authorize(oidc, &ctx(&read_only, &jwt, false)) {
            AuthResult::Failed(f) => {
                assert_eq!(f.status, 403);
                assert_eq!(f.reason, "insufficient scope (needs orders:write)");
                assert_eq!(
                    f.challenges,
                    vec![
                        "Bearer realm=\"mock\", error=\"insufficient_scope\", scope=\"orders:write\""
                    ]
                );
            }
            other => panic!("{other:?}"),
        }

        // A literal issuer is still compared.
        let mut settings = secured();
        settings.security.jwt.issuer = Some("https://idp.example".into());
        let c = compile(SPEC2, settings);
        let oidc = c.spec.operation("oidc").expect("oidc");
        assert!(
            matches!(c.authorize(oidc, &ctx(&both, &jwt, false)), AuthResult::Failed(f) if f.status == 401)
        );
    }

    #[test]
    fn fixed_modes_and_switches() {
        let jwt: Arc<JwtState> = JwtState::shared_for_tests();
        let c = compile(SPEC, MockSettings::default());
        let cop = c.spec.operation("c").expect("c");
        assert_eq!(
            c.authorize(cop, &ctx(&HeaderMap::new(), &jwt, false)),
            AuthResult::NotRequired
        );

        let mut s = secured();
        s.security.mode = SecurityMode::ApiKey;
        let c = compile(SPEC, s);
        let cop = c.spec.operation("c").expect("c");
        assert!(matches!(
            c.authorize(cop, &ctx(&HeaderMap::new(), &jwt, false)),
            AuthResult::Failed(_)
        ));
        let key = with(&[("x-api-key", "demo-key".into())]);
        assert!(matches!(
            c.authorize(cop, &ctx(&key, &jwt, false)),
            AuthResult::Passed(_)
        ));

        let mut s = secured();
        s.security.mode = SecurityMode::Jwt;
        s.security.jwt.audience = Some("pets".into());
        s.operations.insert(
            "a".into(),
            serde_json::from_value(json!({"public": true})).expect("op"),
        );
        let c = compile(SPEC, s);
        let cop = c.spec.operation("c").expect("c");
        let static_token = with(&[("authorization", "Bearer demo-token".into())]);
        assert!(matches!(
            c.authorize(cop, &ctx(&static_token, &jwt, false)),
            AuthResult::Failed(_)
        ));
        let wrong_aud = with(&[(
            "authorization",
            format!("Bearer {}", jwt_with_scope(&jwt, "x")),
        )]);
        assert!(
            matches!(c.authorize(cop, &ctx(&wrong_aud, &jwt, false)), AuthResult::Failed(f) if f.reason.contains("invalid token"))
        );
        let a = c.spec.operation("a").expect("a");
        assert_eq!(
            c.authorize(a, &ctx(&HeaderMap::new(), &jwt, false)),
            AuthResult::NotRequired
        );
    }
}
