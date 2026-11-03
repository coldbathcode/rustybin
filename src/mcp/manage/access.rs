//! Who is calling the management server and what they may do.
//!
//! Transport authentication happened before the request reached the MCP
//! handler (control auth in `token` / `jwt` mode, the key layer in shared
//! mode). The [`Access`] turns what those layers left on the request (a
//! [`Grant`], a [`KeyContext`], visit manage tokens, the admin token) into
//! the rights the tools check, through the same [`Caller`] the REST
//! handlers use.

use axum::http::{Extensions, HeaderMap, HeaderName, Uri};
use serde_json::{json, Value};

use crate::control_auth::{ControlAuthorized, Grant};
use crate::keys::KeyContext;
use crate::mocks::selfservice::{self as ss, Caller};
use crate::mocks::service::{Links, Svc};
use crate::rbac::{Permission, Resolved};
use crate::state::AppState;

/// What a tool needs (`ToolDef.permission`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolNeed {
    /// A permission of the caller.
    Perm(Permission),
    /// A permission of the caller, or ownership of the mock: on a
    /// self-service instance (or for an API key in shared mode) a caller
    /// without it changes its own mocks only (checked per mock by the
    /// service; the tool is listed when the caller could change something).
    Owner(Permission),
    /// A permission of the caller, or a user acting on their own AI agents
    /// (the agents tools: everyone's with the permission, a user's own
    /// without it; checked per agent by the tool).
    Own(Permission),
    /// The caller's own self-service visit (its manage token).
    Visitor,
}

impl ToolNeed {
    /// The permission, unless this is the visitor rule.
    pub fn permission(self) -> Option<Permission> {
        match self {
            ToolNeed::Perm(p) | ToolNeed::Owner(p) | ToolNeed::Own(p) => Some(p),
            ToolNeed::Visitor => None,
        }
    }

    /// How the docs' tool tables write it.
    pub fn label(self) -> String {
        match self {
            ToolNeed::Perm(p) => p.name().to_string(),
            ToolNeed::Owner(p) => format!("{} (or the mock's owner)", p.name()),
            ToolNeed::Own(p) => format!("{} (or your own agents)", p.name()),
            ToolNeed::Visitor => "a visit's manage token".to_string(),
        }
    }
}

/// Request headers forwarded into in-process requests (`try_request`,
/// bins, the flaky reset), so a caller tests as who they are. Only the API
/// key (shared mode: data-plane routes need it, captures redact it). The
/// caller's control credentials (bearer, admin token, visit manage token)
/// are never forwarded: the data plane does not use them, and the request
/// would be captured where other readers see it.
const FORWARDED: &[&str] = &["x-rustybin-key"];

/// The caller of the management server.
pub struct Access {
    pub caller: Caller,
    /// The 0.4 scopes of an authenticated caller (`None` on an open
    /// control plane, and for key holders in shared mode): names only.
    pub grant: Option<Grant>,
    /// The caller's roles and permissions, resolved for this request.
    pub resolved: std::sync::Arc<Resolved>,
    /// The caller's API key in shared mode.
    pub key_id: Option<String>,
    pub key: Option<std::sync::Arc<crate::keys::KeyRecord>>,
    /// Where mocks answer as seen by this request.
    pub links: Links,
    /// Credentials to forward into in-process requests.
    pub forward: Vec<(HeaderName, axum::http::HeaderValue)>,
    /// `X-Rustybin-Session` of the request, if any.
    pub session_header: Option<String>,
    /// What the caller authenticates with (who the agent is, with the
    /// client's name) and the presented token's id and expiry.
    pub credential: super::agents::Credential,
    pub token: Option<(String, i64)>,
    /// `User-Agent` says the stdio bridge (`rustybin mcp-stdio`).
    pub stdio: bool,
    /// Public mode without keys: the instance.
    pub public_mode: bool,
    pub self_service: bool,
    pub shared_mode: bool,
}

/// The credential a request presents, as the agents registry names it
/// (verified upstream: control auth inserted the grant, the key layer the
/// key, the visit lookup the visit), and the token's id and expiry.
fn credential_of(
    state: &AppState,
    headers: &HeaderMap,
    extensions: &Extensions,
    grant: Option<Grant>,
    key_id: &Option<String>,
    caller: &Caller,
) -> (super::agents::Credential, Option<(String, i64)>) {
    // The user a sign-in token or a user's OAuth token acts for, as
    // control auth verified and resolved it.
    let user = caller.resolved.user_id().map(str::to_string);
    use super::agents::{clean, Credential};
    if let Some(id) = key_id {
        return (Credential::Key { id: id.clone() }, None);
    }
    if let Some(h) = caller.visit() {
        return (
            Credential::Visit {
                fingerprint: crate::mocks::visits::Owner::Visit(h.to_string()).fingerprint(),
            },
            None,
        );
    }
    let presented = crate::control_auth::presented_token(headers);
    if let (Some(t), Some(admin)) = (presented, state.config.admin_token.as_deref()) {
        if crate::control_auth::constant_time_eq(t, admin) {
            return (Credential::Admin, None);
        }
    }
    let claims = presented
        .filter(|_| grant.is_some())
        .and_then(crate::control_auth::unverified_claims);
    if let Some(c) = claims {
        let s = |k: &str| c.get(k).and_then(Value::as_str).map(|v| clean(v, 128));
        let token = s("jti")
            .filter(|j| !j.is_empty())
            .zip(c.get("exp").and_then(Value::as_i64));
        let sub = s("sub").unwrap_or_default();
        let credential = if extensions.get::<crate::control_oauth::ViaOAuth>().is_some() {
            match user {
                Some(id) => Credential::User {
                    id,
                    client: Some(sub),
                },
                None => Credential::Oauth { sub },
            }
        } else if c.get("rb_use").and_then(Value::as_str) == Some("console") {
            // By the user's id (a sign-in token always resolves to one;
            // a 0.4 record by name moved to it at startup).
            match user {
                Some(id) => Credential::User { id, client: None },
                None => Credential::Console {
                    user: crate::login::user_of(&c)
                        .map(|u| clean(u, 128))
                        .unwrap_or(sub),
                },
            }
        } else {
            Credential::Jwt {
                sub,
                scopes: grant
                    .map(|g| g.scopes().iter().map(|s| s.to_string()).collect())
                    .unwrap_or_default(),
            }
        };
        return (credential, token);
    }
    (
        Credential::Open {
            session: crate::mocks::visits::session_label(&caller.session),
        },
        None,
    )
}

impl Access {
    pub fn from_request(
        state: &AppState,
        headers: &HeaderMap,
        extensions: &Extensions,
        uri: &Uri,
    ) -> Self {
        let caller = ss::caller(state, headers, extensions);
        let grant = extensions
            .get::<ControlAuthorized>()
            .map(ControlAuthorized::grant);
        let resolved = caller.resolved.clone();
        let key = extensions
            .get::<KeyContext>()
            .filter(|k| k.shared)
            .map(|k| k.key.clone());
        let key_id = key.as_ref().map(|k| k.id.to_string());
        let forward = FORWARDED
            .iter()
            .filter_map(|n| {
                let name = HeaderName::from_static(n);
                headers.get(&name).map(|v| (name, v.clone()))
            })
            .collect();
        let (credential, token) =
            credential_of(state, headers, extensions, grant, &key_id, &caller);
        let stdio = headers
            .get(axum::http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ua| ua.starts_with(super::stdio::USER_AGENT_PREFIX));
        Self {
            credential,
            token,
            stdio,
            caller,
            grant,
            resolved,
            key_id,
            key,
            links: Links::from_request(headers, extensions, uri, &state.config),
            forward,
            session_header: crate::session::session_header(headers),
            public_mode: state.config.public_mode,
            self_service: state.mocks.visits().is_some(),
            shared_mode: state.keys.shared(),
        }
    }

    /// The service for this caller.
    pub fn svc(&self, state: &AppState) -> Svc {
        Svc {
            state: state.clone(),
            caller: self.caller.clone(),
            links: self.links.clone(),
        }
    }

    /// Does the caller hold `p`?
    pub fn has(&self, p: Permission) -> bool {
        self.resolved.has(p)
    }

    /// Does the caller meet `need`?
    pub fn allows(&self, need: ToolNeed) -> bool {
        match need {
            // The visit operator tools exist only with self-service on.
            ToolNeed::Perm(Permission::VisitsOperate) => {
                self.self_service && self.has(Permission::VisitsOperate)
            }
            ToolNeed::Perm(p) => self.has(p),
            // On a self-service instance (or for a key) anyone without the
            // permission may change their own mocks (the first creation
            // starts a visit); an authenticated caller is held to it.
            ToolNeed::Owner(p) => {
                self.has(p)
                    || (self.grant.is_none()
                        && (!self.caller.owners.is_empty() || self.self_service))
            }
            // A user always has their own agents to see and limit.
            ToolNeed::Own(p) => self.has(p) || self.resolved.user_id().is_some(),
            ToolNeed::Visitor => self.self_service && self.caller.visit().is_some(),
        }
    }

    /// Why a need is not met, for the tool error: the permission that is
    /// missing (never what the caller holds).
    pub fn denial(&self, need: ToolNeed) -> String {
        match need {
            ToolNeed::Visitor => {
                if !self.self_service {
                    "self-service mocks are off on this instance".into()
                } else {
                    "no running visit: send the visit's manage token as X-Rustybin-Mock-Token"
                        .into()
                }
            }
            ToolNeed::Perm(Permission::VisitsOperate) if !self.self_service => {
                "self-service mocks are off on this instance".into()
            }
            ToolNeed::Perm(p) | ToolNeed::Owner(p) | ToolNeed::Own(p) => {
                let (_, message) = crate::rbac::need::refusal_of(&self.resolved, p);
                let mut out = format!(
                    "permission_required: {}. {}",
                    p.name(),
                    message.unwrap_or_else(|| format!(
                        "This tool needs the {} permission (a role that grants it, or the admin token).",
                        p.name()
                    ))
                );
                if matches!(need, ToolNeed::Owner(_)) && self.self_service {
                    out.push_str(" On a self-service demo send the visit's X-Rustybin-Mock-Token header to manage your own mocks.");
                }
                out
            }
        }
    }

    /// How the caller is named in the audit log and the per-caller caps
    /// (the assist calls per minute): never a secret. A token caller is its
    /// credential's fingerprint, never the session header it chooses.
    pub fn principal(&self) -> String {
        if let Some(k) = &self.key_id {
            return format!("key:{k}");
        }
        if let Some(h) = self.caller.visit() {
            return format!(
                "visit:{}",
                crate::mocks::visits::Owner::Visit(h.to_string()).fingerprint()
            );
        }
        if self.caller.admin {
            return "admin".into();
        }
        if self.grant.is_some()
            && !matches!(self.credential, super::agents::Credential::Open { .. })
        {
            return self.credential.fingerprint();
        }
        format!(
            "session:{}",
            crate::mocks::visits::session_label(&self.caller.session)
        )
    }

    /// The session key for data-plane calls made on the caller's behalf
    /// (`X-Rustybin-Session`): the request's own session, else one derived
    /// from the caller.
    pub fn test_session(&self) -> String {
        if let Some(s) = &self.session_header {
            return s.clone();
        }
        format!(
            "mcp-{}",
            &crate::mocks::visits::sha256_hex(&self.caller.session)[..12]
        )
    }

    /// `caller` block of `get_instance`.
    pub fn describe(&self) -> Value {
        let user = match &self.resolved.principal {
            crate::rbac::Principal::User { id, name, .. } => json!({ "id": id, "name": name }),
            _ => Value::Null,
        };
        json!({
            "admin": self.resolved.permissions.is_all(),
            "user": user,
            "via": self.credential.via(),
            "scopes": self.grant.map(|g| g.scopes()),
            "roles": self.resolved.roles,
            "permissions": self.resolved.permissions.names(),
            "key_id": self.key_id,
            "visit": self.caller.visit().is_some(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{control_jwt, jwt_config, test_state_with};

    fn access_with(state: &AppState, token: &str, session: &str) -> Access {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_str(&format!("Bearer {token}")).expect("header"),
        );
        headers.insert(
            "x-rustybin-session",
            axum::http::HeaderValue::from_str(session).expect("header"),
        );
        let mut extensions = Extensions::new();
        let resolved = crate::rbac::from_jwt_claims(&json!({"scope": "console"}), &state.users);
        extensions.insert(ControlAuthorized(std::sync::Arc::new(resolved)));
        Access::from_request(
            state,
            &headers,
            &extensions,
            &Uri::from_static("/_rustybin/mcp"),
        )
    }

    #[test]
    fn a_token_caller_is_its_credential_whatever_the_session_header() {
        // The assist caps key on the principal: a console-scope token that
        // changes X-Rustybin-Session per call must stay one caller.
        let state = test_state_with(jwt_config());
        let token = control_jwt(Some("console"));
        let a = access_with(&state, &token, "one");
        let b = access_with(&state, &token, "two");
        assert_eq!(a.principal(), b.principal());
        assert_eq!(a.principal(), "jwt:user-1");
    }
}
