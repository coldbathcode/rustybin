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
use crate::state::AppState;

/// The right a tool needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Right {
    /// Read the instance and its mocks: the `console` scope, or anyone on
    /// an open control plane (secrets redacted unless the caller may change
    /// the mock).
    Read,
    /// Read captured traffic: `Read`, or the `inspector` scope.
    Traffic,
    /// Change mocks: admin for instance mocks, the owner for visitor and
    /// key-owned mocks (checked per mock by the service; listed when the
    /// caller could manage something).
    Manage,
    /// Instance-global changes: the admin token or the `admin` scope.
    Admin,
    /// The caller's own self-service visit.
    Visitor,
    /// Self-service operator routes (admin, self-service on).
    Operator,
}

impl Right {
    pub fn as_str(self) -> &'static str {
        match self {
            Right::Read => "read",
            Right::Traffic => "traffic",
            Right::Manage => "manage",
            Right::Admin => "admin",
            Right::Visitor => "visitor",
            Right::Operator => "operator",
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
    /// The control-plane grant (`None` on an open control plane, and for
    /// key holders in shared mode).
    pub grant: Option<Grant>,
    /// The caller's API key in shared mode.
    pub key_id: Option<String>,
    pub key: Option<std::sync::Arc<crate::keys::KeyRecord>>,
    /// Where mocks answer as seen by this request.
    pub links: Links,
    /// Credentials to forward into in-process requests.
    pub forward: Vec<(HeaderName, axum::http::HeaderValue)>,
    /// For admin callers: a credential the data plane's admin guard accepts
    /// (the instance's admin token, else the caller's own token), sent as
    /// `X-Rustybin-Admin-Token` (redacted in captures) only by tools that
    /// call an admin-guarded data-plane route.
    pub admin_credential: Option<axum::http::HeaderValue>,
    /// `X-Rustybin-Session` of the request, if any.
    pub session_header: Option<String>,
    /// Public mode without keys: the instance.
    pub public_mode: bool,
    pub self_service: bool,
    pub shared_mode: bool,
}

impl Access {
    pub fn from_request(
        state: &AppState,
        headers: &HeaderMap,
        extensions: &Extensions,
        uri: &Uri,
    ) -> Self {
        let caller = ss::caller(state, headers, extensions);
        let grant = extensions.get::<ControlAuthorized>().map(|g| g.0);
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
        let admin_credential = caller
            .admin
            .then(|| {
                state
                    .config
                    .admin_token
                    .as_deref()
                    .or_else(|| crate::control_auth::presented_token(headers))
            })
            .flatten()
            .and_then(|t| axum::http::HeaderValue::from_str(t).ok());
        Self {
            caller,
            admin_credential,
            grant,
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

    pub fn is_admin(&self) -> bool {
        self.caller.admin
    }

    fn console(&self) -> bool {
        self.grant.is_none_or(|g| g.console)
    }

    /// Does the caller hold `right`?
    pub fn allows(&self, right: Right) -> bool {
        match right {
            Right::Read => self.console(),
            Right::Traffic => self.grant.is_none_or(|g| g.console || g.inspector),
            // On a self-service instance anyone may create their own mocks
            // (the first creation starts a visit).
            Right::Manage => {
                self.console()
                    && (self.caller.admin || !self.caller.owners.is_empty() || self.self_service)
            }
            Right::Admin => self.caller.admin,
            Right::Visitor => self.self_service && self.caller.visit().is_some(),
            Right::Operator => self.self_service && self.caller.admin,
        }
    }

    /// Why a right is missing, for the tool error.
    pub fn denial(&self, right: Right) -> String {
        match right {
            Right::Read | Right::Traffic => {
                "this token's scope does not include reading the control plane: use a control \
                 token with the console scope (or the admin token)"
                    .into()
            }
            Right::Manage => match self.caller.admin_guard {
                Err((_, m)) if self.caller.owners.is_empty() => format!(
                    "changing this instance's mocks needs admin rights ({m}); on a self-service \
                     demo send the visit's X-Rustybin-Mock-Token header to manage your own mocks"
                ),
                _ => "changing mocks needs the admin token or a token with the admin scope, or \
                      ownership of the mock (a visit manage token or an API key)"
                    .into(),
            },
            Right::Admin => match self.caller.admin_guard {
                Err((_, m)) => format!("this tool changes the whole instance: {m}"),
                Ok(()) => "this tool needs the admin token or a token with the admin scope".into(),
            },
            Right::Visitor => {
                if !self.self_service {
                    "self-service mocks are off on this instance".into()
                } else {
                    "no running visit: send the visit's manage token as X-Rustybin-Mock-Token"
                        .into()
                }
            }
            Right::Operator => {
                if !self.self_service {
                    "self-service mocks are off on this instance".into()
                } else {
                    "the visit operator tools need admin rights".into()
                }
            }
        }
    }

    /// How the caller is named in the audit log (never a secret).
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
        let rights: Vec<&str> = [
            Right::Read,
            Right::Traffic,
            Right::Manage,
            Right::Admin,
            Right::Visitor,
            Right::Operator,
        ]
        .iter()
        .filter(|r| self.allows(**r))
        .map(|r| r.as_str())
        .collect();
        json!({
            "admin": self.caller.admin,
            "scopes": self.grant.map(|g| g.scopes()),
            "key_id": self.key_id,
            "visit": self.caller.visit().is_some(),
            "rights": rights,
        })
    }
}
