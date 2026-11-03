//! Single sign-on with OpenID Connect (access control step 10): a team
//! signs in to the console and the control plane with the identity
//! provider it has, any OpenID Connect provider, and its groups decide the
//! roles.
//!
//! - **Settings** (group `sso`, all `settings.security`, seeded by
//!   `RUSTYBIN_SSO_*`): [`ENABLED`], [`NAME`], [`ISSUER`], [`CLIENT_ID`],
//!   the secret [`CLIENT_SECRET`] (none: a public client, PKCE only),
//!   [`SCOPES`], [`GROUPS_CLAIM`], [`ROLE_MAP`] (`group=role role`
//!   entries; a user's roles are the union), [`DEFAULT_ROLES`] (a member of
//!   no mapped group; empty refuses them), [`ALLOWED_DOMAINS`],
//!   [`PROVISION`], [`PASSWORD_SIGN_IN`].
//! - **The flow** ([`api`]): `GET /_rustybin/sso/start` (the console, or a
//!   loopback address for the CLI, with the S256 challenge of a verifier
//!   the client keeps) sends the browser to the provider (authorization
//!   code, PKCE, `state`, `nonce`); `GET /_rustybin/sso/callback` takes the
//!   code back, exchanges it at the token endpoint, verifies the ID token
//!   ([`provider::verify_id_token`]), maps the groups to roles, links or
//!   provisions the user ([`crate::rbac::Users::sso_sign_in`]) and sends the
//!   browser back with a one-time code; `POST /_rustybin/sso/token` trades
//!   that code and the verifier for a sign-in token like a password
//!   user's. The provider's tokens are never kept.
//! - **The network**: discovery, the JWKS and the token endpoint go out
//!   under `Purpose::Sso`, whose rule permits the issuer's host once single
//!   sign-on is set up ([`Sso::sync_egress`]). Discovery and the keys are
//!   cached for an hour; a key id the cache does not know fetches the keys
//!   once more.
//! - **State** is bounded: sign-ins under way ([`MAX_PENDING`], ten
//!   minutes, [`MAX_PENDING_PER_ADDRESS`] per client address; a full table
//!   drops the oldest sign-in of the busiest address, never refuses a
//!   start) and one-time codes ([`MAX_HANDOFFS`], two minutes).

pub mod api;
pub mod provider;

#[cfg(test)]
pub(crate) mod tests;

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use base64::Engine;
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::config::Config;
use crate::egress::{Allowlist, Egress, Purpose, Rule};
use crate::limits::Clock;
use crate::settings::{Kind, Settings, Spec};
use provider::{Discovery, Jwk, VerifyError};

// ── Settings ────────────────────────────────────────────────────────

pub const ENABLED: &str = "sso.enabled";
pub const NAME: &str = "sso.name";
pub const ISSUER: &str = "sso.issuer";
pub const CLIENT_ID: &str = "sso.client_id";
pub const CLIENT_SECRET: &str = "sso.client_secret";
pub const SCOPES: &str = "sso.scopes";
pub const GROUPS_CLAIM: &str = "sso.groups_claim";
pub const ROLE_MAP: &str = "sso.role_map";
pub const DEFAULT_ROLES: &str = "sso.default_roles";
pub const ALLOWED_DOMAINS: &str = "sso.allowed_domains";
pub const PROVISION: &str = "sso.provision";
pub const PASSWORD_SIGN_IN: &str = "sso.password_sign_in";

/// The environment variables that seed the settings.
pub const ENV_VARS: [&str; 12] = [
    "RUSTYBIN_SSO_ENABLED",
    "RUSTYBIN_SSO_NAME",
    "RUSTYBIN_SSO_ISSUER",
    "RUSTYBIN_SSO_CLIENT_ID",
    "RUSTYBIN_SSO_CLIENT_SECRET",
    "RUSTYBIN_SSO_SCOPES",
    "RUSTYBIN_SSO_GROUPS_CLAIM",
    "RUSTYBIN_SSO_ROLE_MAP",
    "RUSTYBIN_SSO_DEFAULT_ROLES",
    "RUSTYBIN_SSO_ALLOWED_DOMAINS",
    "RUSTYBIN_SSO_PROVISION",
    "RUSTYBIN_SSO_PASSWORD_SIGN_IN",
];

const MAX_ROLE_MAP: usize = 100;
const MAX_ROLE_MAP_ENTRY: usize = 300;

/// The `RUSTYBIN_SSO_*` variables as read at startup (validated when the
/// settings store reads them: an invalid one warns and seeds nothing).
#[derive(Clone, Default)]
pub struct SsoEnv(BTreeMap<&'static str, String>);

impl std::fmt::Debug for SsoEnv {
    // Never the client secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<&str> = self.0.keys().copied().collect();
        write!(f, "SsoEnv({})", keys.join(", "))
    }
}

impl SsoEnv {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let mut m = BTreeMap::new();
        for var in ENV_VARS {
            if let Some(v) = get(var).filter(|v| !v.trim().is_empty()) {
                m.insert(var, v);
            }
        }
        Self(m)
    }

    /// One variable (tests set them this way).
    pub fn set(&mut self, var: &'static str, value: &str) {
        self.0.insert(var, value.to_string());
    }

    fn raw(&self, var: &str) -> Option<&String> {
        self.0.get(var)
    }
}

/// Validate a seed with the setting's own rules; an invalid one warns once
/// per store and seeds nothing.
fn seed(c: &Config, var: &'static str, key: &str) -> Option<Value> {
    let raw = c.sso_env.raw(var)?;
    let spec = specs().into_iter().find(|s| s.key == key)?;
    let value = Value::String(raw.clone());
    let value = spec.normalize.map_or(value.clone(), |f| f(&value));
    match spec.kind.validate(&value).and_then(|v| {
        if let Some(check) = spec.validate {
            check(&v)?;
        }
        Ok(v)
    }) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(target: "rustybin::sso", "{var}: {e} (ignored)");
            None
        }
    }
}

fn check_issuer(v: &Value) -> Result<(), String> {
    let Some(s) = v.as_str() else { return Ok(()) };
    let url = reqwest::Url::parse(s).map_err(|e| format!("not a URL: {e}"))?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err("the issuer has no query or fragment".into());
    }
    let loopback = url.host_str().is_some_and(|h| {
        let h = h.trim_matches(['[', ']']);
        h.eq_ignore_ascii_case("localhost") || h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    });
    if url.scheme() != "https" && !loopback {
        return Err("the issuer must be https (http only on a loopback address)".into());
    }
    Ok(())
}

fn check_client_id(v: &Value) -> Result<(), String> {
    match v.as_str() {
        Some(s) if s.chars().any(|c| c.is_whitespace()) => Err("no spaces in a client id".into()),
        _ => Ok(()),
    }
}

fn valid_role_name(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= 40
        && r.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// One role of a role map entry: `role` (an instance role) or
/// `project:role` (a role in that project, step 11).
fn valid_role_token(t: &str) -> bool {
    match t.split_once(':') {
        Some((project, role)) => crate::mocks::model::valid_name(project) && valid_role_name(role),
        None => valid_role_name(t),
    }
}

/// One entry of the role map: `group=role role` (the last `=` splits, so
/// a group name may hold one; roles are separated by spaces or `+`). A
/// role written `project:role` is a role in that project.
pub fn parse_role_map_entry(entry: &str) -> Result<(String, Vec<String>), String> {
    let (group, roles) = entry
        .rsplit_once('=')
        .ok_or_else(|| format!("{entry:?}: expected group=role (roles separated by spaces)"))?;
    let group = group.trim();
    if group.is_empty() {
        return Err(format!("{entry:?}: the group is empty"));
    }
    let roles: Vec<String> = roles
        .split([' ', '+'])
        .filter(|r| !r.is_empty())
        .map(str::to_string)
        .collect();
    if roles.is_empty() {
        return Err(format!("{entry:?}: no role after ="));
    }
    if let Some(bad) = roles.iter().find(|r| !valid_role_token(r)) {
        return Err(format!("{entry:?}: {bad:?} is not a role name"));
    }
    Ok((group.to_string(), roles))
}

fn check_role_map(v: &Value) -> Result<(), String> {
    for item in v.as_array().into_iter().flatten() {
        if let Some(s) = item.as_str() {
            parse_role_map_entry(s)?;
        }
    }
    Ok(())
}

/// A role map given as `{group, roles}` objects (a document, the API)
/// becomes the `group=role role` strings the setting keeps; `{group,
/// project, roles}` becomes `group=project:role project:role`.
fn normalize_role_map(v: &Value) -> Value {
    match v {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|i| match i {
                    Value::Object(o) => {
                        let group = o.get("group").and_then(Value::as_str).unwrap_or("");
                        let list: Vec<String> = match o.get("roles") {
                            Some(Value::Array(r)) => r
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect(),
                            Some(Value::String(s)) => s
                                .split([',', ' '])
                                .filter(|r| !r.is_empty())
                                .map(str::to_string)
                                .collect(),
                            _ => Vec::new(),
                        };
                        let roles = match o.get("project").and_then(Value::as_str) {
                            Some(p) if !p.is_empty() => list
                                .iter()
                                .map(|r| format!("{p}:{r}"))
                                .collect::<Vec<_>>()
                                .join(" "),
                            _ => list.join(" "),
                        };
                        Value::String(format!("{group}={roles}"))
                    }
                    other => other.clone(),
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// What setting `key` to `value` grants that the caller does not hold: a
/// role map or default roles naming a role (in the instance, or in a
/// project for `project:role`) with a permission the caller lacks there.
/// Unknown roles grant nothing. `None` for any other key.
pub fn grants_need(
    caller: &crate::rbac::Resolved,
    users: &crate::rbac::Users,
    key: &str,
    value: &Value,
) -> Option<crate::rbac::Permission> {
    let list = |v: &Value| -> Vec<String> {
        match v {
            Value::Array(items) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            Value::String(s) => s.split(',').map(|t| t.trim().to_string()).collect(),
            _ => Vec::new(),
        }
    };
    let tokens: Vec<String> = match key {
        ROLE_MAP => list(&normalize_role_map(value))
            .iter()
            .filter_map(|e| parse_role_map_entry(e).ok())
            .flat_map(|(_, roles)| roles)
            .collect(),
        DEFAULT_ROLES => list(value),
        _ => return None,
    };
    for t in &tokens {
        let (project, role) = match t.split_once(':') {
            Some((p, r)) => (Some(p), r),
            None => (None, t.as_str()),
        };
        let Some(perms) = users.role_permissions(role) else {
            continue;
        };
        let missing = perms.iter().find(|p| !caller.has_in(*p, project));
        if missing.is_some() {
            return missing;
        }
    }
    None
}

/// Do the role map or the default roles name `role` (in the instance or
/// in a project), whether single sign-on is on or not?
pub fn names_role(settings: &Settings, role: &str) -> bool {
    let in_map = settings
        .list(ROLE_MAP)
        .iter()
        .filter_map(|e| parse_role_map_entry(e).ok())
        .flat_map(|(_, roles)| roles)
        .any(|t| t.rsplit(':').next() == Some(role));
    in_map || settings.list(DEFAULT_ROLES).iter().any(|r| r == role)
}

fn check_roles_list(v: &Value) -> Result<(), String> {
    for r in v.as_array().into_iter().flatten().filter_map(Value::as_str) {
        if !valid_role_name(r) {
            return Err(format!("{r:?} is not a role name"));
        }
    }
    Ok(())
}

fn check_domains(v: &Value) -> Result<(), String> {
    for d in v.as_array().into_iter().flatten().filter_map(Value::as_str) {
        let ok = d.contains('.')
            && d.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        if !ok {
            return Err(format!("{d:?} is not a domain (example.com)"));
        }
    }
    Ok(())
}

/// Scopes: a list, or one string of space or comma separated scopes.
fn normalize_scopes(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::Array(
            s.split([' ', ','])
                .filter(|x| !x.is_empty())
                .map(|x| Value::String(x.to_string()))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn check_scopes(v: &Value) -> Result<(), String> {
    for s in v.as_array().into_iter().flatten().filter_map(Value::as_str) {
        if !s
            .bytes()
            .all(|b| b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b))
        {
            return Err(format!("{s:?} is not a scope"));
        }
    }
    Ok(())
}

fn check_claim(v: &Value) -> Result<(), String> {
    match v.as_str() {
        Some(s)
            if s.split('.').any(str::is_empty)
                || s.chars().any(|c| c.is_whitespace() || c.is_control()) =>
        {
            Err("a claim name, or a dotted path into nested claims (realm_access.roles)".into())
        }
        _ => Ok(()),
    }
}

/// The settings of single sign-on (the `sso` group).
pub fn specs() -> Vec<Spec> {
    vec![
        Spec::new(
            ENABLED,
            "sso",
            "Single sign-on",
            "Offer \"Sign in with\" the identity provider on the sign-in screen (the password sign-in, RUSTYBIN_CONTROL_AUTH=password). Needs the issuer and the client id.",
            Kind::Bool,
            json!(false),
        )
        .env("RUSTYBIN_SSO_ENABLED", |c| seed(c, "RUSTYBIN_SSO_ENABLED", ENABLED)),
        Spec::new(
            NAME,
            "sso",
            "Button label",
            "The name on the sign-in button: \"Sign in with <name>\". Empty: the issuer's host.",
            Kind::Text { max_len: 60 },
            Value::Null,
        )
        .env("RUSTYBIN_SSO_NAME", |c| seed(c, "RUSTYBIN_SSO_NAME", NAME)),
        Spec::new(
            ISSUER,
            "sso",
            "Issuer",
            "The provider's issuer URL, exactly as its discovery document says (https; http only on a loopback address). Its host is allowed by the outbound policy (purpose sso) once single sign-on is set up. Register <this instance>/_rustybin/sso/callback as the client's redirect URI.",
            Kind::Url,
            Value::Null,
        )
        .env("RUSTYBIN_SSO_ISSUER", |c| seed(c, "RUSTYBIN_SSO_ISSUER", ISSUER))
        .check(check_issuer),
        Spec::new(
            CLIENT_ID,
            "sso",
            "Client id",
            "The client this instance is at the provider (an OpenID Connect client with the authorization code flow).",
            Kind::Text { max_len: 256 },
            Value::Null,
        )
        .env("RUSTYBIN_SSO_CLIENT_ID", |c| seed(c, "RUSTYBIN_SSO_CLIENT_ID", CLIENT_ID))
        .check(check_client_id),
        Spec::new(
            CLIENT_SECRET,
            "sso",
            "Client secret",
            "The client's secret (a confidential client). Not set: a public client, PKCE only.",
            Kind::Text { max_len: 512 },
            Value::Null,
        )
        .env("RUSTYBIN_SSO_CLIENT_SECRET", |c| {
            c.sso_env
                .raw("RUSTYBIN_SSO_CLIENT_SECRET")
                .map(|s| Value::String(s.clone()))
        })
        .secret(),
        Spec::new(
            SCOPES,
            "sso",
            "Scopes",
            "The scopes asked for (openid is always added). Add the scope your provider needs for the groups claim (groups, often).",
            Kind::List { max_items: 20, max_len: 100 },
            json!(["openid", "profile", "email"]),
        )
        .env("RUSTYBIN_SSO_SCOPES", |c| seed(c, "RUSTYBIN_SSO_SCOPES", SCOPES))
        .normalize(normalize_scopes)
        .check(check_scopes),
        Spec::new(
            GROUPS_CLAIM,
            "sso",
            "Groups claim",
            "The ID token claim that lists the person's groups (a dotted path reads a nested claim, realm_access.roles).",
            Kind::Text { max_len: 100 },
            json!("groups"),
        )
        .env("RUSTYBIN_SSO_GROUPS_CLAIM", |c| seed(c, "RUSTYBIN_SSO_GROUPS_CLAIM", GROUPS_CLAIM))
        .check(check_claim),
        Spec::new(
            ROLE_MAP,
            "sso",
            "Role map",
            "Which roles each group gives, one entry per group: group=role (several roles separated by spaces: developers=editor tester). A user's roles are the union of their groups' roles, recomputed at every sign-in.",
            Kind::List { max_items: MAX_ROLE_MAP, max_len: MAX_ROLE_MAP_ENTRY },
            json!([]),
        )
        .env("RUSTYBIN_SSO_ROLE_MAP", |c| seed(c, "RUSTYBIN_SSO_ROLE_MAP", ROLE_MAP))
        .normalize(normalize_role_map)
        .check(check_role_map),
        Spec::new(
            DEFAULT_ROLES,
            "sso",
            "Default roles",
            "The roles of a person in none of the mapped groups. Empty: they are refused.",
            Kind::List { max_items: 16, max_len: 40 },
            json!([]),
        )
        .env("RUSTYBIN_SSO_DEFAULT_ROLES", |c| seed(c, "RUSTYBIN_SSO_DEFAULT_ROLES", DEFAULT_ROLES))
        .check(check_roles_list),
        Spec::new(
            ALLOWED_DOMAINS,
            "sso",
            "Allowed email domains",
            "Only people whose email is at one of these domains sign in (the provider must not say the email is unverified). Empty: any.",
            Kind::List { max_items: 50, max_len: 253 },
            json!([]),
        )
        .env("RUSTYBIN_SSO_ALLOWED_DOMAINS", |c| {
            seed(c, "RUSTYBIN_SSO_ALLOWED_DOMAINS", ALLOWED_DOMAINS)
        })
        .check(check_domains),
        Spec::new(
            PROVISION,
            "sso",
            "Create users on first sign-in",
            "On: a person the provider signs in becomes a user at their first sign-in. Off: only users an admin added (as single sign-on users) sign in.",
            Kind::Bool,
            json!(true),
        )
        .env("RUSTYBIN_SSO_PROVISION", |c| seed(c, "RUSTYBIN_SSO_PROVISION", PROVISION)),
        Spec::new(
            PASSWORD_SIGN_IN,
            "sso",
            "Password sign-in",
            "Off: only single sign-on and the admin token sign in (no password sign-in, no invitations). Applies only while single sign-on is set up, so a half-configured provider never locks everyone out.",
            Kind::Bool,
            json!(true),
        )
        .env("RUSTYBIN_SSO_PASSWORD_SIGN_IN", |c| {
            seed(c, "RUSTYBIN_SSO_PASSWORD_SIGN_IN", PASSWORD_SIGN_IN)
        }),
    ]
}

// ── The effective configuration ─────────────────────────────────────

/// Single sign-on as the settings configure it now.
#[derive(Clone)]
pub struct Setup {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<Zeroizing<String>>,
    pub scopes: Vec<String>,
    pub groups_claim: String,
    pub role_map: Vec<(String, Vec<String>)>,
    pub default_roles: Vec<String>,
    pub allowed_domains: Vec<String>,
    pub provision: bool,
    pub name: String,
}

/// Read the setup: `Err` with the reason when single sign-on is off or
/// incomplete.
pub fn setup(settings: &Settings) -> Result<Setup, String> {
    if !settings.bool(ENABLED) {
        return Err("single sign-on is off (sso.enabled)".into());
    }
    let issuer = settings
        .string(ISSUER)
        .map(|i| provider::normalise_issuer(&i))
        .ok_or("sso.issuer is not set")?;
    let client_id = settings
        .string(CLIENT_ID)
        .ok_or("sso.client_id is not set")?;
    let mut scopes = vec!["openid".to_string()];
    for s in settings.list(SCOPES) {
        if !scopes.contains(&s) {
            scopes.push(s);
        }
    }
    let role_map = settings
        .list(ROLE_MAP)
        .iter()
        .filter_map(|e| parse_role_map_entry(e).ok())
        .collect();
    let host = reqwest::Url::parse(&issuer)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| issuer.clone());
    Ok(Setup {
        client_secret: settings.secret(CLIENT_SECRET).filter(|s| !s.is_empty()),
        issuer,
        client_id,
        scopes,
        groups_claim: settings
            .string(GROUPS_CLAIM)
            .unwrap_or_else(|| "groups".into()),
        role_map,
        default_roles: settings.list(DEFAULT_ROLES),
        allowed_domains: settings
            .list(ALLOWED_DOMAINS)
            .into_iter()
            .map(|d| d.to_ascii_lowercase())
            .collect(),
        provision: settings.get(PROVISION).0.as_bool().unwrap_or(true),
        name: settings.string(NAME).unwrap_or(host),
    })
}

/// Does the password sign-in work? Off only while single sign-on is set up
/// and `sso.password_sign_in` is off.
pub fn password_sign_in(settings: &Settings) -> bool {
    settings.get(PASSWORD_SIGN_IN).0.as_bool().unwrap_or(true) || setup(settings).is_err()
}

// ── State ───────────────────────────────────────────────────────────

/// Sign-ins under way.
pub const MAX_PENDING: usize = 256;
/// Sign-ins under way from one client address (an IPv6 /64).
pub const MAX_PENDING_PER_ADDRESS: usize = 10;
/// How long a sign-in at the provider may take.
pub const PENDING_TTL: Duration = Duration::from_secs(600);
/// One-time codes waiting for their client.
pub const MAX_HANDOFFS: usize = 256;
/// How long a one-time code works.
pub const HANDOFF_TTL: Duration = Duration::from_secs(120);
/// How long discovery and the keys are reused.
pub const CACHE_TTL: Duration = Duration::from_secs(3600);
/// Groups read from a token.
pub const MAX_GROUPS: usize = 256;
/// How long a failure to read discovery is answered without fetching again.
pub const DISCOVERY_RETRY: Duration = Duration::from_secs(10);

/// Where the browser goes back once the provider answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReturnTo {
    /// The console (`/ui/#/sso/<code>`).
    Console,
    /// A loopback address the CLI listens on (`?code=` added).
    Loopback(String),
}

impl ReturnTo {
    /// `console`, or an `http://` URL on a loopback host (localhost,
    /// 127.0.0.1, [::1]) with any port: the rule the management server's
    /// OAuth sign-in applies to native clients.
    pub fn parse(s: &str) -> Option<Self> {
        if s.is_empty() || s == "console" {
            return Some(ReturnTo::Console);
        }
        if s.len() > 512 {
            return None;
        }
        let url = reqwest::Url::parse(s).ok()?;
        let loopback = url.host_str().is_some_and(|h| {
            let h = h.trim_matches(['[', ']']);
            h.eq_ignore_ascii_case("localhost")
                || h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        });
        (url.scheme() == "http"
            && loopback
            && url.fragment().is_none()
            && url.username().is_empty()
            && url.password().is_none())
        .then(|| ReturnTo::Loopback(s.to_string()))
    }
}

struct Pending {
    nonce: String,
    verifier: String,
    challenge: String,
    return_to: ReturnTo,
    redirect_uri: String,
    issuer: String,
    address: Option<IpAddr>,
    at: Duration,
}

struct Handoff {
    user_id: String,
    challenge: String,
    at: Duration,
}

struct Cached<T> {
    key: String,
    value: T,
    at: Duration,
}

/// The single sign-on state of an instance (`AppState.sso`).
pub struct Sso {
    settings: Arc<Settings>,
    egress: Arc<Egress>,
    clock: Arc<dyn Clock>,
    pending: Mutex<HashMap<String, Pending>>,
    handoffs: Mutex<HashMap<String, Handoff>>,
    discovery: Mutex<Option<Cached<Discovery>>>,
    /// The last failure to read discovery (answered again for a while).
    discovery_failure: Mutex<Option<Cached<String>>>,
    jwks: Mutex<Option<Cached<Vec<Jwk>>>>,
    /// JWKS fetches since start (tests see the refetch).
    jwks_fetches: AtomicU64,
}

impl std::fmt::Debug for Sso {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sso({} under way)", lock(&self.pending).len())
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn random_b64(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// The S256 challenge of a PKCE verifier.
pub fn challenge_of(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn hash_hex(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// An S256 challenge's shape: 43 base64url characters.
fn is_challenge(s: &str) -> bool {
    s.len() == 43
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A PKCE verifier's shape (RFC 7636): 43 to 128 unreserved characters.
fn is_verifier(s: &str) -> bool {
    (43..=128).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~'))
}

/// Why a sign-in was refused: a stable reason (the console and the CLI
/// show a text for it) and the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub reason: &'static str,
    pub message: String,
}

impl Refusal {
    pub fn new(reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }
}

/// What the ID token says about the person.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Person {
    pub subject: String,
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub email_verified: Option<bool>,
    pub groups: Vec<String>,
}

/// A claim at a dotted path.
fn claim_at<'a>(claims: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(claims, |v, k| v.get(k))
}

/// Read the person out of verified claims.
pub fn person_of(claims: &Value, groups_claim: &str) -> Person {
    let s = |k: &str| {
        claims
            .get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|v| !v.is_empty())
    };
    let groups = match claim_at(claims, groups_claim) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .filter(|g| !g.is_empty() && g.len() <= 256)
            .take(MAX_GROUPS)
            .map(str::to_string)
            .collect(),
        Some(Value::String(g)) if !g.is_empty() && g.len() <= 256 => vec![g.clone()],
        _ => Vec::new(),
    };
    let email_verified = match claims.get("email_verified") {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::String(s)) => Some(s == "true"),
        _ => None,
    };
    Person {
        subject: s("sub").unwrap_or_default(),
        name: s("preferred_username"),
        display_name: s("name"),
        email: s("email"),
        email_verified,
        groups,
    }
}

/// The sign-in name a new user gets: `preferred_username`, else the email
/// when the provider verified it (`email_verified: true`), else the
/// subject, made to fit the user name rules (letters, digits and `. _ @
/// -`, at most 64).
pub fn sign_in_name(p: &Person) -> Option<String> {
    let email = p.email.as_ref().filter(|_| p.email_verified == Some(true));
    for candidate in [p.name.as_ref(), email].into_iter().flatten() {
        if crate::login::valid_user(candidate).is_ok() {
            return Some(candidate.clone());
        }
    }
    let raw = p.name.as_ref().or(email).unwrap_or(&p.subject);
    let clean: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(crate::login::MAX_USER_CHARS)
        .collect();
    let clean = clean.trim_matches('-').to_string();
    crate::login::valid_user(&clean).is_ok().then_some(clean)
}

/// The roles a person's groups give (the role map's union, roles the
/// instance has only), else the default roles. Also answers the role names
/// the map or the defaults name that do not exist.
pub fn map_roles(
    setup: &Setup,
    groups: &[String],
    known: impl Fn(&str) -> bool,
) -> (Vec<String>, Vec<String>) {
    let mut roles: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    let mut add = |r: &String, roles: &mut Vec<String>| {
        if !known(r) {
            if !unknown.contains(r) {
                unknown.push(r.clone());
            }
        } else if !roles.contains(r) {
            roles.push(r.clone());
        }
    };
    let mut in_projects = false;
    for (group, mapped) in &setup.role_map {
        if groups.iter().any(|g| g == group) {
            for r in mapped {
                if r.contains(':') {
                    in_projects = true;
                } else {
                    add(r, &mut roles);
                }
            }
        }
    }
    if roles.is_empty() && !in_projects {
        for r in &setup.default_roles {
            add(r, &mut roles);
        }
    }
    (roles, unknown)
}

/// The roles a person's groups give in projects (`project:role` entries
/// of the role map): projects and roles the instance has only. Also
/// answers what the map names that does not exist (`project:role`).
pub fn map_project_roles(
    setup: &Setup,
    groups: &[String],
    known_role: impl Fn(&str) -> bool,
    known_project: impl Fn(&str) -> bool,
) -> (BTreeMap<String, Vec<String>>, Vec<String>) {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut unknown: Vec<String> = Vec::new();
    for (group, mapped) in &setup.role_map {
        if !groups.iter().any(|g| g == group) {
            continue;
        }
        for t in mapped {
            let Some((project, role)) = t.split_once(':') else {
                continue;
            };
            if !known_project(project) || !known_role(role) {
                if !unknown.contains(t) {
                    unknown.push(t.clone());
                }
                continue;
            }
            let roles = out.entry(project.to_string()).or_default();
            if !roles.iter().any(|r| r == role) {
                roles.push(role.to_string());
            }
        }
    }
    (out, unknown)
}

/// Refused when the address (an IPv6 /64) has
/// [`MAX_PENDING_PER_ADDRESS`] sign-ins under way.
fn per_address(pending: &HashMap<String, Pending>, bucket: Option<IpAddr>) -> Result<(), Refusal> {
    if bucket.is_some()
        && pending.values().filter(|p| p.address == bucket).count() >= MAX_PENDING_PER_ADDRESS
    {
        return Err(Refusal::new(
            "rate_limited",
            "too many sign-ins under way from this address: finish one, or wait ten minutes",
        ));
    }
    Ok(())
}

/// Make room in a full table: the oldest sign-in of the address with the
/// most under way goes. Whoever fills the table from a few addresses
/// pushes out their own sign-ins first, never a person's single one, so a
/// full table never refuses a start.
fn evict_one(pending: &mut HashMap<String, Pending>) {
    let mut counts: HashMap<Option<IpAddr>, usize> = HashMap::new();
    for p in pending.values() {
        *counts.entry(p.address).or_default() += 1;
    }
    let most = counts.values().copied().max().unwrap_or(0);
    let victim = pending
        .iter()
        .filter(|(_, p)| counts.get(&p.address).copied() == Some(most))
        .min_by_key(|(_, p)| p.at)
        .map(|(k, _)| k.clone());
    if let Some(k) = victim {
        pending.remove(&k);
    }
}

/// The one reason the browser gets when the identity matches no user it
/// may use ([`is_account_refusal`]).
pub const NOT_LINKED: &str = "not_linked";
const NOT_LINKED_MESSAGE: &str = "this sign-in matches no user it may use here: ask an admin to add you (a user created ahead links through the email the provider verified)";

/// The store's refusals that say something about the users of a name
/// (a password account has it, another identity, a user created ahead,
/// nobody): all answered as [`NOT_LINKED`].
fn is_account_refusal(reason: &str) -> bool {
    use crate::rbac::users::{
        SSO_NAME_TAKEN, SSO_NOT_LINKED, SSO_NOT_PROVISIONED, SSO_OTHER_IDENTITY,
    };
    [
        SSO_NAME_TAKEN,
        SSO_OTHER_IDENTITY,
        SSO_NOT_LINKED,
        SSO_NOT_PROVISIONED,
    ]
    .contains(&reason)
}

/// Is the email at an allowed domain? `Err` with the reason otherwise.
pub fn check_domain(setup: &Setup, p: &Person) -> Result<(), Refusal> {
    if setup.allowed_domains.is_empty() {
        return Ok(());
    }
    let Some(email) = &p.email else {
        return Err(Refusal::new(
            "domain_not_allowed",
            "the provider gave no email, and sign-in is limited to some email domains (sso.allowed_domains)",
        ));
    };
    if p.email_verified == Some(false) {
        return Err(Refusal::new(
            "email_unverified",
            "the provider says the email is not verified",
        ));
    }
    let domain = email
        .rsplit_once('@')
        .map(|(_, d)| d.to_ascii_lowercase())
        .unwrap_or_default();
    if setup.allowed_domains.contains(&domain) {
        Ok(())
    } else {
        Err(Refusal::new(
            "domain_not_allowed",
            format!(
                "email addresses at {} may not sign in here (sso.allowed_domains)",
                crate::logging::printable(&domain, 100)
            ),
        ))
    }
}

/// A sign-in that came back verified: who, and where the browser goes.
pub struct Completed {
    pub user: crate::rbac::User,
    pub return_to: ReturnTo,
    /// The one-time code the client trades for a sign-in token.
    pub code: String,
}

impl Sso {
    pub fn new(settings: Arc<Settings>, egress: Arc<Egress>, clock: Arc<dyn Clock>) -> Self {
        let sso = Self {
            settings,
            egress,
            clock,
            pending: Mutex::new(HashMap::new()),
            handoffs: Mutex::new(HashMap::new()),
            discovery: Mutex::new(None),
            discovery_failure: Mutex::new(None),
            jwks: Mutex::new(None),
            jwks_fetches: AtomicU64::new(0),
        };
        sso.sync_egress();
        sso
    }

    /// The setup now ([`setup`]), with the outbound rule kept in step.
    pub fn setup(&self) -> Result<Setup, String> {
        self.sync_egress();
        setup(&self.settings)
    }

    /// Permit the issuer's host under `Purpose::Sso` while single sign-on
    /// is set up (nothing otherwise).
    pub fn sync_egress(&self) {
        let entries: Vec<String> = setup(&self.settings)
            .ok()
            .and_then(|s| crate::egress::url_entry(&s.issuer))
            .into_iter()
            .collect();
        let (allow, _) = Allowlist::parse(&entries);
        self.egress.permit(
            Purpose::Sso,
            Rule {
                allow,
                any: false,
                loopback: false,
            },
        );
    }

    /// JWKS fetches since start.
    pub fn jwks_fetches(&self) -> u64 {
        self.jwks_fetches.load(Ordering::Relaxed)
    }

    fn now(&self) -> Duration {
        self.clock.mono()
    }

    fn fresh<T: Clone>(&self, c: &Option<Cached<T>>, key: &str) -> Option<T> {
        c.as_ref()
            .filter(|c| c.key == key && self.now().saturating_sub(c.at) < CACHE_TTL)
            .map(|c| c.value.clone())
    }

    /// The provider's discovery document (cached for an hour; `force`
    /// fetches it again). A failure is answered again for
    /// [`DISCOVERY_RETRY`] without fetching: an open start route cannot make
    /// the instance hammer a provider that is down.
    pub async fn discovery(&self, setup: &Setup, force: bool) -> Result<Discovery, String> {
        if !force {
            if let Some(d) = self.fresh(&lock(&self.discovery), &setup.issuer) {
                return Ok(d);
            }
            if let Some(f) = lock(&self.discovery_failure).as_ref().filter(|f| {
                f.key == setup.issuer && self.now().saturating_sub(f.at) < DISCOVERY_RETRY
            }) {
                return Err(f.value.clone());
            }
        }
        let fetched =
            match provider::get_json(&self.egress, &provider::discovery_url(&setup.issuer))
                .await
                .and_then(|doc| provider::parse_discovery(&doc, &setup.issuer))
            {
                Ok(d) => d,
                Err(e) => {
                    *lock(&self.discovery_failure) = Some(Cached {
                        key: setup.issuer.clone(),
                        value: e.clone(),
                        at: self.now(),
                    });
                    return Err(e);
                }
            };
        *lock(&self.discovery_failure) = None;
        let d = fetched;
        *lock(&self.discovery) = Some(Cached {
            key: setup.issuer.clone(),
            value: d.clone(),
            at: self.now(),
        });
        Ok(d)
    }

    /// The provider's keys (cached for an hour; `force` fetches them again).
    pub async fn keys(&self, d: &Discovery, force: bool) -> Result<Vec<Jwk>, String> {
        if !force {
            if let Some(k) = self.fresh(&lock(&self.jwks), &d.jwks_uri) {
                return Ok(k);
            }
        }
        self.jwks_fetches.fetch_add(1, Ordering::Relaxed);
        let doc = provider::get_json(&self.egress, &d.jwks_uri).await?;
        let keys = provider::parse_jwks(&doc)?;
        *lock(&self.jwks) = Some(Cached {
            key: d.jwks_uri.clone(),
            value: keys.clone(),
            at: self.now(),
        });
        Ok(keys)
    }

    fn prune(&self) {
        let now = self.now();
        lock(&self.pending).retain(|_, p| now.saturating_sub(p.at) < PENDING_TTL);
        lock(&self.handoffs).retain(|_, h| now.saturating_sub(h.at) < HANDOFF_TTL);
    }

    /// Sign-ins under way (tests, diagnostics).
    pub fn pending_count(&self) -> usize {
        self.prune();
        lock(&self.pending).len()
    }

    /// Start a sign-in: the provider's authorization URL the browser goes
    /// to. `challenge` is the S256 challenge of the verifier the client
    /// keeps for [`Sso::redeem`]; `redirect_uri` is this instance's
    /// callback.
    pub async fn begin(
        &self,
        return_to: ReturnTo,
        challenge: &str,
        redirect_uri: &str,
        address: Option<IpAddr>,
    ) -> Result<String, Refusal> {
        let setup = self
            .setup()
            .map_err(|e| Refusal::new("not_configured", e))?;
        if !is_challenge(challenge) {
            return Err(Refusal::new(
                "invalid_request",
                "challenge: the S256 challenge of a PKCE verifier (43 base64url characters)",
            ));
        }
        let bucket = address.map(crate::session::address_bucket);
        self.prune();
        per_address(&lock(&self.pending), bucket)?;
        let d = self
            .discovery(&setup, false)
            .await
            .map_err(|e| Refusal::new("provider_error", e))?;
        let state = random_b64(32);
        let nonce = random_b64(32);
        let verifier = random_b64(32);
        let mut url = reqwest::Url::parse(&d.authorization_endpoint)
            .map_err(|e| Refusal::new("provider_error", format!("authorization_endpoint: {e}")))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &setup.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", &setup.scopes.join(" "))
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &challenge_of(&verifier))
            .append_pair("code_challenge_method", "S256");
        let mut pending = lock(&self.pending);
        // Checked again: other starts from the address may have landed
        // while discovery was fetched.
        per_address(&pending, bucket)?;
        if pending.len() >= MAX_PENDING {
            evict_one(&mut pending);
        }
        pending.insert(
            state,
            Pending {
                nonce,
                verifier,
                challenge: challenge.to_string(),
                return_to,
                redirect_uri: redirect_uri.to_string(),
                issuer: setup.issuer.clone(),
                address: bucket,
                at: self.now(),
            },
        );
        Ok(url.to_string())
    }

    /// End a sign-in under way (the provider refused): where it goes back.
    pub fn cancel(&self, state: &str) -> Option<ReturnTo> {
        self.prune();
        lock(&self.pending).remove(state).map(|p| p.return_to)
    }

    /// Finish a sign-in: the provider's code for its ID token, verified,
    /// then the user linked or provisioned with the roles of their groups.
    /// The pending sign-in is used once whatever the outcome.
    pub async fn complete(
        &self,
        users: &crate::rbac::Users,
        state: &str,
        code: &str,
    ) -> Result<Completed, (Option<ReturnTo>, Refusal)> {
        self.prune();
        let Some(p) = lock(&self.pending).remove(state) else {
            return Err((
                None,
                Refusal::new(
                    "expired",
                    "this sign-in is not under way any more (it took over ten minutes, or was used): start again",
                ),
            ));
        };
        let back = Some(p.return_to.clone());
        let fail = |r: Refusal| (back.clone(), r);
        let setup = self
            .setup()
            .map_err(|e| fail(Refusal::new("not_configured", e)))?;
        if setup.issuer != p.issuer {
            return Err(fail(Refusal::new(
                "expired",
                "the issuer changed during the sign-in: start again",
            )));
        }
        if code.is_empty() || code.len() > 2048 {
            return Err(fail(Refusal::new(
                "invalid_request",
                "no code from the provider",
            )));
        }
        let d = self
            .discovery(&setup, false)
            .await
            .map_err(|e| fail(Refusal::new("provider_error", e)))?;
        let secret = setup.client_secret.as_deref().map(|s| s.as_str());
        let mut form: Vec<(&str, &str)> = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", p.redirect_uri.as_str()),
            ("code_verifier", p.verifier.as_str()),
        ];
        let basic = match secret {
            Some(s) if !d.secret_in_form => Some((setup.client_id.as_str(), s)),
            Some(s) => {
                form.push(("client_id", setup.client_id.as_str()));
                form.push(("client_secret", s));
                None
            }
            None => {
                form.push(("client_id", setup.client_id.as_str()));
                None
            }
        };
        let (status, body) = provider::post_form(&self.egress, &d.token_endpoint, &form, basic)
            .await
            .map_err(|e| fail(Refusal::new("provider_error", e)))?;
        if !(200..300).contains(&status) {
            let what = body
                .get("error")
                .and_then(Value::as_str)
                .map(|e| crate::logging::printable(e, 80))
                .unwrap_or_else(|| status.to_string());
            return Err(fail(Refusal::new(
                "provider_error",
                format!("the provider's token endpoint refused the code ({what})"),
            )));
        }
        let Some(id_token) = body.get("id_token").and_then(Value::as_str) else {
            return Err(fail(Refusal::new(
                "provider_error",
                "the provider answered no ID token (is openid among the scopes?)",
            )));
        };
        let keys = self
            .keys(&d, false)
            .await
            .map_err(|e| fail(Refusal::new("provider_error", e)))?;
        let verified =
            match provider::verify_id_token(id_token, &keys, &d.issuer, &setup.client_id, &p.nonce)
            {
                Err(VerifyError::UnknownKey) => {
                    let keys = self
                        .keys(&d, true)
                        .await
                        .map_err(|e| fail(Refusal::new("provider_error", e)))?;
                    provider::verify_id_token(
                        id_token,
                        &keys,
                        &d.issuer,
                        &setup.client_id,
                        &p.nonce,
                    )
                }
                other => other,
            };
        let claims = verified.map_err(|e| fail(Refusal::new("invalid_id_token", e.to_string())))?;
        let person = person_of(&claims, &setup.groups_claim);
        check_domain(&setup, &person).map_err(fail)?;
        let Some(name) = sign_in_name(&person) else {
            return Err(fail(Refusal::new(
                "invalid_id_token",
                "the ID token names nobody this instance can call a user",
            )));
        };
        // For the audit log: the groups that mapped (never the others the
        // provider sent).
        for (group, _) in &setup.role_map {
            if person.groups.iter().any(|g| g == group) {
                crate::audit::target("group", group);
            }
        }
        let (mapped, mut unknown) = map_roles(&setup, &person.groups, |r| {
            users.role_permissions(r).is_some()
        });
        let (mapped_projects, unknown_in_projects) = map_project_roles(
            &setup,
            &person.groups,
            |r| users.role_permissions(r).is_some(),
            |p| users.project_exists(p),
        );
        unknown.extend(unknown_in_projects);
        if !unknown.is_empty() {
            tracing::warn!(target: "rustybin::sso", roles = %unknown.join(","), "the role map or the default roles name roles this instance does not have: ignored");
        }
        let ident = crate::rbac::SsoIdentity {
            issuer: d.issuer.clone(),
            subject: person.subject.clone(),
            name,
            display_name: person.display_name.clone(),
            email: person.email.clone(),
            email_verified: person.email_verified == Some(true),
            mapped_roles: mapped,
            mapped_projects,
        };
        let user = users
            .sso_sign_in(&ident, setup.provision)
            .map_err(|e| match e {
                crate::rbac::UsersError::Refused(reason, m) if is_account_refusal(reason) => {
                    // The detail stays in the log: the browser (and a name
                    // the provider lets people choose) learns only that no
                    // user may be used, never whether a password account or
                    // another identity has the name.
                    tracing::info!(target: "rustybin::sso", reason, detail = %m, "single sign-on refused");
                    fail(Refusal::new(NOT_LINKED, NOT_LINKED_MESSAGE))
                }
                crate::rbac::UsersError::Refused(reason, m) => fail(Refusal::new(reason, m)),
                other => fail(Refusal::new("server_error", other.to_string())),
            })?;
        let code = format!("rbs_{}", random_b64(32));
        let mut handoffs = lock(&self.handoffs);
        if handoffs.len() >= MAX_HANDOFFS {
            return Err(fail(Refusal::new(
                "busy",
                "too many sign-ins are finishing at once: try again in a moment",
            )));
        }
        handoffs.insert(
            hash_hex(&code),
            Handoff {
                user_id: user.id.clone(),
                challenge: p.challenge.clone(),
                at: self.now(),
            },
        );
        Ok(Completed {
            user,
            return_to: p.return_to,
            code,
        })
    }

    /// Trade a one-time code and the verifier of the challenge the sign-in
    /// started with for the user id. Used once, whatever the outcome.
    pub fn redeem(&self, code: &str, verifier: &str) -> Option<String> {
        self.prune();
        if code.len() > 100 || !is_verifier(verifier) {
            return None;
        }
        let h = lock(&self.handoffs).remove(&hash_hex(code))?;
        crate::control_auth::constant_time_eq(&challenge_of(verifier), &h.challenge)
            .then_some(h.user_id)
    }

    /// What the sign-in screen needs (never a secret, nothing fetched).
    pub fn status(&self) -> Value {
        let setup = self.setup();
        let enabled = self.settings.bool(ENABLED);
        json!({
            "enabled": enabled,
            "ready": setup.is_ok(),
            "name": setup.as_ref().ok().map(|s| s.name.clone()),
            "password_sign_in": password_sign_in(&self.settings),
            "reason": setup.err(),
            "start": api::START_PATH,
        })
    }

    /// Check the setup against the provider: fetch discovery and the keys
    /// (fresh), and say what the role map names that does not exist.
    pub async fn test(&self, users: &crate::rbac::Users, redirect_uri: &str) -> Value {
        let mut out = self.status();
        out["redirect_uri"] = json!(redirect_uri);
        let setup = match self.setup() {
            Ok(s) => s,
            Err(e) => {
                out["ok"] = json!(false);
                out["error"] = json!(e);
                return out;
            }
        };
        out["issuer"] = json!(setup.issuer);
        out["client_id"] = json!(setup.client_id);
        out["client_secret"] = self.settings.secret_state(CLIENT_SECRET);
        out["scopes"] = json!(setup.scopes);
        out["groups_claim"] = json!(setup.groups_claim);
        let mut unknown: Vec<String> = Vec::new();
        for r in setup
            .role_map
            .iter()
            .flat_map(|(_, r)| r)
            .chain(&setup.default_roles)
        {
            let known = match r.split_once(':') {
                Some((p, role)) => {
                    users.project_exists(p) && users.role_permissions(role).is_some()
                }
                None => users.role_permissions(r).is_some(),
            };
            if !known && !unknown.contains(r) {
                unknown.push(r.clone());
            }
        }
        out["role_map"] = json!(setup
            .role_map
            .iter()
            .map(|(g, r)| json!({ "group": g, "roles": r }))
            .collect::<Vec<_>>());
        out["default_roles"] = json!(setup.default_roles);
        out["unknown_roles"] = json!(unknown);
        match self.discovery(&setup, true).await {
            Ok(d) => {
                out["discovery"] = json!({
                    "issuer": d.issuer,
                    "authorization_endpoint": d.authorization_endpoint,
                    "token_endpoint": d.token_endpoint,
                    "jwks_uri": d.jwks_uri,
                    "id_token_signing_alg_values_supported": d.algorithms,
                    "client_auth": if d.secret_in_form { "client_secret_post" } else { "client_secret_basic" },
                });
                match self.keys(&d, true).await {
                    Ok(k) => {
                        out["keys"] = json!(k
                            .iter()
                            .map(|k| json!({ "kid": k.kid, "kty": k.kty, "alg": k.alg }))
                            .collect::<Vec<_>>());
                        out["ok"] = json!(true);
                    }
                    Err(e) => {
                        out["ok"] = json!(false);
                        out["error"] = json!(format!("the keys (jwks_uri): {e}"));
                    }
                }
            }
            Err(e) => {
                out["ok"] = json!(false);
                out["error"] = json!(format!("discovery: {e}"));
            }
        }
        out
    }
}

/// Paths of the single sign-on flow that need no credentials.
pub fn is_public_path(path: &str) -> bool {
    matches!(
        path,
        api::STATUS_PATH | api::START_PATH | api::CALLBACK_PATH | api::TOKEN_PATH
    )
}

/// The diagnostics check `sso` (password mode, when single sign-on is on):
/// set up or not, and whether anything but the provider still signs in.
/// Nothing is fetched (`rustybin sso test` does that).
pub fn diagnostics(
    state: &crate::state::AppState,
) -> Option<(&'static str, String, Option<&'static str>)> {
    if !crate::login::enabled(&state.config) || !state.settings.bool(ENABLED) {
        return None;
    }
    Some(match setup(&state.settings) {
        Err(e) => (
            "warn",
            format!("single sign-on is on but not set up: {e}"),
            Some("set sso.issuer and sso.client_id under Management, Sign-in (then rustybin sso test)"),
        ),
        Ok(_) if !password_sign_in(&state.settings) && state.config.admin_token.is_none() => (
            "warn",
            "single sign-on is the only sign-in (sso.password_sign_in is off) and there is no RUSTYBIN_ADMIN_TOKEN: if the provider is down, nobody signs in".into(),
            Some("set RUSTYBIN_ADMIN_TOKEN as the break glass, or turn the password sign-in back on"),
        ),
        Ok(s) => (
            "ok",
            format!(
                "single sign-on with {} ({} role map entries){}",
                s.issuer,
                s.role_map.len(),
                if password_sign_in(&state.settings) {
                    ", the password sign-in too"
                } else {
                    ", the password sign-in off"
                }
            ),
            None,
        ),
    })
}
