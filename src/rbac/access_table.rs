//! The safety net of step 08: who could call each control route in 0.4,
//! and who can now.
//!
//! The 0.4 rules are frozen here as data and code: [`old_grant_allows`]
//! is `Grant::allows` with its path rules as they were, [`OLD_ADMIN_ONLY`]
//! lists every route and method whose handler needed admin rights (the 73
//! `require_admin` call sites, the sign-in management routes, the visit
//! operator routes), [`OLD_OPEN_ADMIN_ONLY`] the harness mutations an open
//! control plane guarded, [`old_key_fence`] what shared mode let an API key
//! reach. The lists were recorded by calling the 0.4 app with every
//! credential on every catalogue route (the probe behind this file) and
//! reading each call site.
//!
//! `the_old_scopes_reach_what_they_reached` compares, for every control
//! route and method in the catalogue, the 0.4 answer with what the
//! catalogue's declarations give the same callers now (the scopes as
//! roles, `crate::rbac::scope_role`; nobody on an open control plane with
//! an admin token; an API key in shared mode). Every difference is listed
//! in [`DELIBERATE`] with its reason, and the test fails on one that is
//! not, or on a listed one that no longer is. `the_route_check_enforces_the_declarations`
//! then calls the full app with each of those credentials on every route
//! and checks that the one check answers what the declarations say.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::{Need, PermSet};
use crate::catalog::{self, Endpoint};
use crate::config::Config;
use crate::control_auth::Grant;
use crate::state::AppState;
use crate::test_support::{control_jwt, jwt_config, test_app_with};

/// Routes and methods whose 0.4 handler refused a caller without admin
/// rights (whatever the mode).
pub(crate) const OLD_ADMIN_ONLY: &[(&str, &str)] = &[
    ("POST", "/_rustybin/mocks/ai/spec"),
    ("POST", "/_rustybin/mocks/{name}/ai/examples"),
    ("POST", "/_rustybin/mocks/{name}/ai/edit"),
    ("POST", "/_rustybin/mocks"),
    ("POST", "/_rustybin/mocks/apply"),
    ("POST", "/_rustybin/mocks/reload"),
    ("GET", "/_rustybin/mocks/visits"),
    ("DELETE", "/_rustybin/mocks/visits"),
    ("POST", "/_rustybin/mocks/visits/pause"),
    ("POST", "/_rustybin/mocks/visits/resume"),
    ("DELETE", "/_rustybin/mocks/visits/{id}"),
    ("PATCH", "/_rustybin/mocks/{name}"),
    ("DELETE", "/_rustybin/mocks/{name}"),
    ("POST", "/_rustybin/mocks/{name}/enable"),
    ("POST", "/_rustybin/mocks/{name}/disable"),
    ("POST", "/_rustybin/mocks/{name}/deploy"),
    ("POST", "/_rustybin/mocks/{name}/rollback"),
    ("POST", "/_rustybin/mocks/{name}/duplicate"),
    ("DELETE", "/_rustybin/mocks/{name}/state"),
    ("PUT", "/_rustybin/mocks/{name}/operations/{op}"),
    ("DELETE", "/_rustybin/mocks/{name}/requests"),
    ("GET", "/_rustybin/agents"),
    ("GET", "/_rustybin/agents/events"),
    ("GET", "/_rustybin/agents/{id}"),
    ("PATCH", "/_rustybin/agents/{id}"),
    ("POST", "/_rustybin/agents/{id}/revoke"),
    ("POST", "/_rustybin/agents/{id}/restore"),
    ("DELETE", "/_rustybin/agents/{id}/sessions"),
    ("GET", "/_rustybin/config/export"),
    ("POST", "/_rustybin/config/apply"),
    ("GET", "/_rustybin/git"),
    ("POST", "/_rustybin/git/init"),
    ("POST", "/_rustybin/git/commit"),
    ("POST", "/_rustybin/git/push"),
    ("POST", "/_rustybin/git/pull"),
    ("POST", "/_rustybin/git/resolve"),
    ("GET", "/_rustybin/git/key"),
    ("POST", "/_rustybin/git/key"),
    ("GET", "/_rustybin/git/history"),
    ("DELETE", "/_rustybin/mcp-checks/{id}"),
    ("PUT", "/_rustybin/mcp-connections/{id}"),
    ("DELETE", "/_rustybin/mcp-connections/{id}"),
    ("DELETE", "/_rustybin/registry/servers/{serverName}"),
    ("PUT", "/_rustybin/portal/items/{id}"),
    ("DELETE", "/_rustybin/portal/items/{id}"),
    ("POST", "/_rustybin/assist/test"),
    ("GET", "/_rustybin/assist/usage"),
    ("POST", "/_rustybin/assist/models"),
    ("GET", "/_rustybin/settings"),
    ("PATCH", "/_rustybin/settings"),
    ("DELETE", "/_rustybin/settings/{key}"),
    ("GET", "/_rustybin/diagnostics"),
    ("POST", "/_rustybin/scenarios/{id}/runs"),
    ("DELETE", "/_rustybin/scenarios/{id}/runs/{run}"),
    ("POST", "/_rustybin/scenarios/{id}/runs/{run}/check"),
    ("GET", "/_rustybin/tests"),
    ("POST", "/_rustybin/tests"),
    ("GET", "/_rustybin/tests/{name}"),
    ("PUT", "/_rustybin/tests/{name}"),
    ("DELETE", "/_rustybin/tests/{name}"),
    ("POST", "/_rustybin/tests/run"),
    ("GET", "/_rustybin/tests/runs"),
    ("DELETE", "/_rustybin/tests/runs"),
    ("GET", "/_rustybin/tests/runs/{id}"),
    ("GET", "/_rustybin/tests/export"),
    ("GET", "/_rustybin/tests/schedules"),
    ("POST", "/_rustybin/tests/schedules"),
    ("GET", "/_rustybin/tests/schedules/{name}"),
    ("PUT", "/_rustybin/tests/schedules/{name}"),
    ("DELETE", "/_rustybin/tests/schedules/{name}"),
    ("POST", "/_rustybin/tests/schedules/{name}/run"),
    ("POST", "/_rustybin/checkpoints"),
    ("DELETE", "/_rustybin/checkpoints/{name}"),
    ("POST", "/_rustybin/checkpoints/{name}/restore"),
    ("GET", "/_rustybin/egress"),
    ("GET", "/_rustybin/keys/usage"),
    ("GET", "/_rustybin/sessions"),
    ("DELETE", "/_rustybin/sessions"),
    ("DELETE", "/_rustybin/sessions/{id}"),
    ("PUT", "/_rustybin/password"),
];

/// Routes that did not exist in 0.4 (access control step 09 on): there is
/// no 0.4 answer to keep, so the comparison skips them and their
/// declaration is the truth (`every_control_route_declares_a_permission`
/// and `the_route_check_enforces_the_declarations` still cover them).
pub(crate) const NEW_SINCE_0_4: &[&str] = &[
    "/_rustybin/egress/check",
    "/_rustybin/harness/import",
    "/_rustybin/git/webhook",
    "/_rustybin/harness/clusters/{id}/leader/step-aside",
    "/_rustybin/mocks/import/fetch",
    "/_rustybin/git/branches",
    "/_rustybin/git/branch",
    "/_rustybin/git/signing-key",
    "/_rustybin/users",
    "/_rustybin/users/{id}",
    "/_rustybin/users/{id}/invite",
    "/_rustybin/users/{id}/sessions",
    "/_rustybin/roles",
    "/_rustybin/roles/{name}",
    "/_rustybin/me",
    "/_rustybin/me/password",
    "/_rustybin/me/sessions",
    "/_rustybin/me/sessions/{id}",
    "/_rustybin/join",
    "/_rustybin/sso/status",
    "/_rustybin/sso/start",
    "/_rustybin/sso/callback",
    "/_rustybin/sso/token",
    "/_rustybin/sso/test",
    "/_rustybin/projects",
    "/_rustybin/projects/{name}",
    "/_rustybin/audit",
    "/_rustybin/audit/stream",
    "/_rustybin/secret-requests",
    "/_rustybin/secret-requests/{id}",
    "/_rustybin/secret-requests/{id}/fulfil",
    "/_rustybin/secret-requests/{id}/decline",
];

/// The harness mutations an open control plane guarded with the admin
/// token (with control auth on, the `harness` scope was enough).
pub(crate) const OLD_OPEN_ADMIN_ONLY: &[(&str, &str)] = &[
    ("PUT", "/_rustybin/harness/scenarios/{name}"),
    ("DELETE", "/_rustybin/harness/scenarios/{name}"),
    ("POST", "/_rustybin/harness/runs"),
    ("PATCH", "/_rustybin/harness/runs/{run}"),
    ("POST", "/_rustybin/harness/runs/{run}/stop"),
    ("POST", "/_rustybin/harness/stop"),
    ("POST", "/_rustybin/harness/join"),
    ("POST", "/_rustybin/harness/leave"),
    ("POST", "/_rustybin/harness/clusters"),
    ("DELETE", "/_rustybin/harness/clusters/{id}"),
    ("POST", "/_rustybin/harness/clusters/{id}/runs"),
    ("PATCH", "/_rustybin/harness/clusters/{id}/runs/{run}"),
    ("POST", "/_rustybin/harness/clusters/{id}/runs/{run}/start"),
    ("POST", "/_rustybin/harness/clusters/{id}/runs/{run}/stop"),
];

/// Who is calling, in the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Col {
    /// A control JWT with the `console` scope (password, jwt mode).
    Console,
    /// A control JWT with the `inspector` scope.
    Inspector,
    /// A control JWT with the `harness` scope.
    Harness,
    /// Nobody, on an open control plane with an admin token (or public
    /// mode).
    Anonymous,
    /// An API key in shared mode (self-service off).
    Key,
}

impl Col {
    pub(crate) const ALL: &'static [Col] = &[
        Col::Console,
        Col::Inspector,
        Col::Harness,
        Col::Anonymous,
        Col::Key,
    ];

    fn scope(self) -> Option<&'static str> {
        match self {
            Col::Console => Some("console"),
            Col::Inspector => Some("inspector"),
            Col::Harness => Some("harness"),
            Col::Anonymous | Col::Key => None,
        }
    }

    /// The permissions the column's caller holds now.
    pub(crate) fn permissions(self) -> PermSet {
        match self.scope() {
            Some(s) => super::from_scope_claim(Some(&Value::String(s.into()))).1,
            None => super::need::anonymous(),
        }
    }
}

fn safe(method: &Method) -> bool {
    method == Method::GET || method == Method::HEAD || method == Method::OPTIONS
}

fn under(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `Grant::allows` as it was in 0.4.
pub(crate) fn old_grant_allows(g: Grant, method: &Method, path: &str) -> bool {
    if g.admin {
        return true;
    }
    if path == "/_rustybin/mcp" {
        return true;
    }
    if crate::control_oauth::is_consent_path(path) {
        return true;
    }
    if under(path, "/_rustybin/harness") {
        return g.harness || (safe(method) && g.console);
    }
    if !safe(method) {
        let relay_session = path
            .strip_prefix("/_rustybin/mcp-relay/")
            .is_some_and(|id| !id.is_empty() && id != "fetch" && !id.contains('/'));
        if method == Method::DELETE && relay_session {
            return g.console;
        }
        return g.console && method == Method::POST && old_read_only_post(path);
    }
    g.console || (g.inspector && under(path, "/_rustybin/requests"))
}

fn old_read_only_post(path: &str) -> bool {
    if matches!(
        path,
        "/_rustybin/mocks/import"
            | "/_rustybin/assist/run"
            | "/_rustybin/mcp-relay"
            | "/_rustybin/mcp-relay/fetch"
            | "/_rustybin/mcp-checks"
            | "/_rustybin/registry/validate"
    ) {
        return true;
    }
    let Some(rest) = path.strip_prefix("/_rustybin/mocks/") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        [name, "operations", op, "validate"] | [name, "operations", op, "rules", "test"] => {
            !name.is_empty() && !op.is_empty()
        }
        _ => false,
    }
}

/// Paths control auth left alone in 0.4 (and still does).
fn unprotected(method: &Method, path: &str) -> bool {
    !crate::control_auth::is_protected_path(path) || crate::harness::is_node_path(method, path)
}

/// What shared mode's key layer let an API key reach (self-service off):
/// its requests, usage, status, the catalogue, the management MCP server,
/// and the routes the key layer never checks.
pub(crate) fn old_key_fence(method: &Method, path: &str) -> bool {
    if crate::keys::is_exempt(method, path) {
        return true;
    }
    if path == "/_rustybin/mcp" {
        return true;
    }
    (method == Method::GET || method == Method::HEAD)
        && (under(path, "/_rustybin/requests")
            || matches!(
                path,
                "/_rustybin/usage" | "/_rustybin/status" | "/_rustybin/catalog"
            ))
}

fn grant_of_col(col: Col) -> Grant {
    super::grant_of(
        super::from_scope_claim(col.scope().map(|s| Value::String(s.into())).as_ref()).1,
    )
}

/// A concrete path for a template (the path rules look at segments).
pub(crate) fn fill(template: &str) -> String {
    template
        .split('/')
        .map(|seg| {
            if seg.starts_with('{') {
                match seg {
                    "{name}" => "petstore",
                    "{op}" => "listPets",
                    "{format}" => "postman",
                    _ => "x",
                }
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Could `col` call `method template` in 0.4?
pub(crate) fn old_allows(col: Col, method: &Method, template: &str) -> bool {
    let path = fill(template);
    let m = method.as_str();
    let admin_only = OLD_ADMIN_ONLY.contains(&(m, template));
    match col {
        Col::Console | Col::Inspector | Col::Harness => {
            if unprotected(method, &path) {
                return true;
            }
            old_grant_allows(grant_of_col(col), method, &path) && !admin_only
        }
        Col::Anonymous => !admin_only && !OLD_OPEN_ADMIN_ONLY.contains(&(m, template)),
        Col::Key => old_key_fence(method, &path) && !admin_only,
    }
}

/// Can `col` call `method` on `ep` now (by the declaration; the key layer's
/// fence is unchanged code)?
pub(crate) fn new_allows(col: Col, ep: &Endpoint, method: &Method) -> bool {
    let path = fill(ep.path);
    if col == Col::Key && !old_key_fence(method, &path) {
        return false;
    }
    match ep.need(method.as_str()) {
        Some(Need::Open) | Some(Need::Authenticated) => true,
        Some(Need::Perm(p)) => col.permissions().has(p),
        None => false,
    }
}

/// Every difference from 0.4, and why. `true`: the caller may call it
/// now and could not before; `false`: the reverse.
pub(crate) const DELIBERATE: &[(Col, &str, &str, bool, &str)] = &[
    // Step 09: the 0.3 password route is the caller's own password. Any
    // signed-in caller reaches it; one who is not a user (these columns'
    // control JWTs, nobody on an open control plane) changes the first
    // admin's only with users.manage, a body check.
    (
        Col::Console,
        "PUT",
        "/_rustybin/password",
        true,
        "your own password: a caller who is not a user still needs users.manage (a body check)",
    ),
    (
        Col::Inspector,
        "PUT",
        "/_rustybin/password",
        true,
        "your own password: a caller who is not a user still needs users.manage (a body check)",
    ),
    (
        Col::Harness,
        "PUT",
        "/_rustybin/password",
        true,
        "your own password: a caller who is not a user still needs users.manage (a body check)",
    ),
    (
        Col::Anonymous,
        "PUT",
        "/_rustybin/password",
        true,
        "your own password: a caller who is not a user still needs users.manage (a body check)",
    ),
    // The viewer role's reads (`*.read`): the console scope is the viewer
    // role plus its 0.4 extras, and the viewer reads what 0.4 kept for admin.
    (
        Col::Console,
        "GET",
        "/_rustybin/agents",
        true,
        "agents.read is a viewer read",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/agents/events",
        true,
        "agents.read is a viewer read",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/agents/{id}",
        true,
        "agents.read is a viewer read",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/git",
        true,
        "git.read is a viewer read",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/git/key",
        true,
        "git.read is a viewer read (the deploy key's public half)",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/git/history",
        true,
        "git.read is a viewer read",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/settings",
        true,
        "settings.read is a viewer read (secrets show only whether they are set)",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/diagnostics",
        true,
        "settings.read is a viewer read (diagnostics never show a credential)",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/egress",
        true,
        "settings.read is a viewer read",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/keys/usage",
        true,
        "settings.read is a viewer read (shared mode only: a key never gets past the key layer)",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/assist/usage",
        true,
        "settings.read is a viewer read",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/tests",
        true,
        "tests.read is a viewer read",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/tests/{name}",
        true,
        "tests.read is a viewer read",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/tests/runs",
        true,
        "tests.read is a viewer read",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/tests/runs/{id}",
        true,
        "tests.read is a viewer read",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/tests/export",
        true,
        "tests.read is a viewer read",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/tests/schedules",
        true,
        "tests.read is a viewer read",
    ),
    (
        Col::Console,
        "GET",
        "/_rustybin/tests/schedules/{name}",
        true,
        "tests.read is a viewer read",
    ),
    // Compute-only harness posts read like the rest of the harness.
    (
        Col::Console,
        "POST",
        "/_rustybin/harness/preview",
        true,
        "harness.read: a preview computes and sends nothing",
    ),
    (
        Col::Console,
        "POST",
        "/_rustybin/harness/validate",
        true,
        "harness.read: a validation computes and sends nothing",
    ),
    // Any caller may sign out and end its own visit (the handlers check
    // the sign-in token and the visit's manage token).
    (
        Col::Console,
        "POST",
        "/_rustybin/logout",
        true,
        "any caller may sign out",
    ),
    (
        Col::Inspector,
        "POST",
        "/_rustybin/logout",
        true,
        "any caller may sign out",
    ),
    (
        Col::Harness,
        "POST",
        "/_rustybin/logout",
        true,
        "any caller may sign out",
    ),
    (
        Col::Inspector,
        "GET",
        "/_rustybin/session",
        true,
        "any caller may read its own session",
    ),
    (
        Col::Harness,
        "GET",
        "/_rustybin/session",
        true,
        "any caller may read its own session",
    ),
    (
        Col::Console,
        "POST",
        "/_rustybin/mocks/visit",
        true,
        "a visit's own route: the manage token decides (self-service is off with control auth on)",
    ),
    (
        Col::Console,
        "DELETE",
        "/_rustybin/mocks/visit",
        true,
        "a visit's own route: the manage token decides",
    ),
    (
        Col::Console,
        "POST",
        "/_rustybin/mocks/visit/extend",
        true,
        "a visit's own route: the manage token decides",
    ),
    (
        Col::Console,
        "POST",
        "/_rustybin/mocks/visit/token",
        true,
        "a visit's own route: the manage token decides",
    ),
    (
        Col::Inspector,
        "POST",
        "/_rustybin/mocks/visit",
        true,
        "a visit's own route: the manage token decides",
    ),
    (
        Col::Inspector,
        "DELETE",
        "/_rustybin/mocks/visit",
        true,
        "a visit's own route: the manage token decides",
    ),
    (
        Col::Inspector,
        "POST",
        "/_rustybin/mocks/visit/extend",
        true,
        "a visit's own route: the manage token decides",
    ),
    (
        Col::Inspector,
        "POST",
        "/_rustybin/mocks/visit/token",
        true,
        "a visit's own route: the manage token decides",
    ),
    (
        Col::Harness,
        "POST",
        "/_rustybin/mocks/visit",
        true,
        "a visit's own route: the manage token decides",
    ),
    (
        Col::Harness,
        "DELETE",
        "/_rustybin/mocks/visit",
        true,
        "a visit's own route: the manage token decides",
    ),
    (
        Col::Harness,
        "POST",
        "/_rustybin/mocks/visit/extend",
        true,
        "a visit's own route: the manage token decides",
    ),
    (
        Col::Harness,
        "POST",
        "/_rustybin/mocks/visit/token",
        true,
        "a visit's own route: the manage token decides",
    ),
    // Clearing one's own session's captures needs only traffic.read (as on
    // an open control plane); clearing everyone's still needs traffic.clear.
    (
        Col::Console,
        "DELETE",
        "/_rustybin/requests",
        true,
        "traffic.read clears one session's captures (?session=); everyone's needs traffic.clear",
    ),
    (
        Col::Inspector,
        "DELETE",
        "/_rustybin/requests",
        true,
        "traffic.read clears one session's captures (?session=); everyone's needs traffic.clear",
    ),
    // A mock's traffic is traffic.read (the inspector scope already read the
    // same captures under /_rustybin/requests).
    (
        Col::Inspector,
        "GET",
        "/_rustybin/mocks/{name}/requests",
        true,
        "traffic.read covers a mock's traffic",
    ),
    (
        Col::Inspector,
        "GET",
        "/_rustybin/mocks/{name}/requests/stream",
        true,
        "traffic.read covers a mock's traffic",
    ),
    (
        Col::Inspector,
        "GET",
        "/_rustybin/mocks/{name}/requests/{n}",
        true,
        "traffic.read covers a mock's traffic",
    ),
    // Step 12: a user sees, limits, revokes and ends the sessions of their
    // own agents without agents.read or agents.manage, so any signed-in
    // caller reaches the agents routes. These columns' callers are no user:
    // the handlers still need agents.read (to see) and agents.manage (to
    // change), body checks, so what they can do is unchanged.
    (Col::Inspector, "GET", "/_rustybin/agents", true, OWN_READ),
    (Col::Harness, "GET", "/_rustybin/agents", true, OWN_READ),
    (Col::Anonymous, "GET", "/_rustybin/agents", true, OWN_READ),
    (
        Col::Inspector,
        "GET",
        "/_rustybin/agents/events",
        true,
        OWN_READ,
    ),
    (
        Col::Harness,
        "GET",
        "/_rustybin/agents/events",
        true,
        OWN_READ,
    ),
    (
        Col::Anonymous,
        "GET",
        "/_rustybin/agents/events",
        true,
        OWN_READ,
    ),
    (
        Col::Inspector,
        "GET",
        "/_rustybin/agents/{id}",
        true,
        OWN_READ,
    ),
    (
        Col::Harness,
        "GET",
        "/_rustybin/agents/{id}",
        true,
        OWN_READ,
    ),
    (
        Col::Anonymous,
        "GET",
        "/_rustybin/agents/{id}",
        true,
        OWN_READ,
    ),
    (
        Col::Console,
        "PATCH",
        "/_rustybin/agents/{id}",
        true,
        OWN_MANAGE,
    ),
    (
        Col::Inspector,
        "PATCH",
        "/_rustybin/agents/{id}",
        true,
        OWN_MANAGE,
    ),
    (
        Col::Harness,
        "PATCH",
        "/_rustybin/agents/{id}",
        true,
        OWN_MANAGE,
    ),
    (
        Col::Anonymous,
        "PATCH",
        "/_rustybin/agents/{id}",
        true,
        OWN_MANAGE,
    ),
    (
        Col::Console,
        "POST",
        "/_rustybin/agents/{id}/revoke",
        true,
        OWN_MANAGE,
    ),
    (
        Col::Inspector,
        "POST",
        "/_rustybin/agents/{id}/revoke",
        true,
        OWN_MANAGE,
    ),
    (
        Col::Harness,
        "POST",
        "/_rustybin/agents/{id}/revoke",
        true,
        OWN_MANAGE,
    ),
    (
        Col::Anonymous,
        "POST",
        "/_rustybin/agents/{id}/revoke",
        true,
        OWN_MANAGE,
    ),
    (
        Col::Console,
        "POST",
        "/_rustybin/agents/{id}/restore",
        true,
        OWN_MANAGE,
    ),
    (
        Col::Inspector,
        "POST",
        "/_rustybin/agents/{id}/restore",
        true,
        OWN_MANAGE,
    ),
    (
        Col::Harness,
        "POST",
        "/_rustybin/agents/{id}/restore",
        true,
        OWN_MANAGE,
    ),
    (
        Col::Anonymous,
        "POST",
        "/_rustybin/agents/{id}/restore",
        true,
        OWN_MANAGE,
    ),
    (
        Col::Console,
        "DELETE",
        "/_rustybin/agents/{id}/sessions",
        true,
        OWN_MANAGE,
    ),
    (
        Col::Inspector,
        "DELETE",
        "/_rustybin/agents/{id}/sessions",
        true,
        OWN_MANAGE,
    ),
    (
        Col::Harness,
        "DELETE",
        "/_rustybin/agents/{id}/sessions",
        true,
        OWN_MANAGE,
    ),
    (
        Col::Anonymous,
        "DELETE",
        "/_rustybin/agents/{id}/sessions",
        true,
        OWN_MANAGE,
    ),
];

/// Why the agents' reads are reachable (step 12).
const OWN_READ: &str =
    "a user's own agents: a caller who is not a user still needs agents.read (a body check)";
/// Why the agents' changes are reachable (step 12).
const OWN_MANAGE: &str =
    "a user's own agents: a caller who is not a user still needs agents.manage (a body check)";

#[test]
fn the_old_scopes_reach_what_they_reached() {
    let mut found = Vec::new();
    let mut compared = 0;
    for ep in catalog::all()
        .iter()
        .filter(|e| e.is_control() && !NEW_SINCE_0_4.contains(&e.path))
    {
        for m in ep.expanded_methods() {
            let method: Method = m.parse().expect("method");
            for col in Col::ALL {
                compared += 1;
                let (old, new) = (
                    old_allows(*col, &method, ep.path),
                    new_allows(*col, ep, &method),
                );
                if old != new {
                    found.push((*col, m, ep.path, new));
                }
            }
        }
    }
    assert!(compared > 800, "too few comparisons: {compared}");
    let listed: Vec<(Col, &str, &str, bool)> = DELIBERATE
        .iter()
        .map(|(c, m, p, n, _)| (*c, *m, *p, *n))
        .collect();
    let unlisted: Vec<_> = found.iter().filter(|d| !listed.contains(d)).collect();
    let stale: Vec<_> = listed.iter().filter(|d| !found.contains(d)).collect();
    assert!(
        unlisted.is_empty(),
        "who may call these changed from 0.4 without a reason in DELIBERATE (col, method, route, allowed now): {unlisted:#?}"
    );
    assert!(
        stale.is_empty(),
        "these DELIBERATE differences are no longer differences: {stale:#?}"
    );
}

/// The 0.4 lists name real routes (a renamed route updates them).
#[test]
fn the_old_lists_name_catalogue_routes() {
    for (m, p) in OLD_ADMIN_ONLY.iter().chain(OLD_OPEN_ADMIN_ONLY) {
        let ep = catalog::control_route(p).unwrap_or_else(|| panic!("{p} is not a control route"));
        assert!(
            ep.expanded_methods().contains(m),
            "{m} {p} is not a method of the route"
        );
    }
    for (_, m, p, _, why) in DELIBERATE {
        assert!(catalog::control_route(p).is_some(), "{m} {p}");
        assert!(!why.is_empty());
    }
    for p in NEW_SINCE_0_4 {
        assert!(
            catalog::control_route(p).is_some(),
            "{p} is not a control route"
        );
        assert!(
            !OLD_ADMIN_ONLY.iter().any(|(_, o)| o == p),
            "{p} existed in 0.4"
        );
    }
}

fn harness_on(mut c: Config) -> Config {
    c.harness.enabled = true;
    c.harness.roles = vec![crate::harness::Role::Client, crate::harness::Role::Server];
    c.harness.targets = vec!["127.0.0.1".into()];
    c.harness.threads = Some(1);
    c
}

const KEY_A: &str = "rbk_ka1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1";

fn shared_state() -> AppState {
    let mut c = harness_on(Config::for_tests());
    c.shared_mode = true;
    c.public_mode = true;
    c.admin_token = Some("admin-secret".into());
    let state = AppState::for_tests(c);
    let doc = serde_json::json!({"version": "v1", "keys": [{
        "id": "ka1",
        "hash": crate::keys::registry::key_hash_hex(KEY_A),
        "label": "starter",
        "status": "active",
        "limits": {},
    }]});
    state
        .keys
        .load_bytes(doc.to_string().as_bytes())
        .expect("registry");
    state
}

fn open_state() -> AppState {
    let mut c = harness_on(Config::for_tests());
    c.admin_token = Some("admin-secret".into());
    AppState::for_tests(c)
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        b = b.header(*k, v);
    }
    let resp = app
        .clone()
        .oneshot(b.body(Body::empty()).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        (status, crate::test_support::body_json(resp).await)
    } else {
        (status, Value::Null)
    }
}

#[tokio::test]
async fn the_route_check_enforces_the_declarations() {
    let jwt_app = test_app_with(AppState::for_tests(harness_on(jwt_config())));
    let open_app = test_app_with(open_state());
    let shared_app = test_app_with(shared_state());
    let bearer = |t: String| vec![("authorization", format!("Bearer {t}"))];
    type Cred<'a> = (Col, &'a axum::Router, Vec<(&'a str, String)>);
    let creds: Vec<Cred> = vec![
        (Col::Console, &jwt_app, bearer(control_jwt(Some("console")))),
        (
            Col::Inspector,
            &jwt_app,
            bearer(control_jwt(Some("inspector"))),
        ),
        (Col::Harness, &jwt_app, bearer(control_jwt(Some("harness")))),
        (Col::Anonymous, &open_app, vec![]),
        (
            Col::Key,
            &shared_app,
            vec![("x-rustybin-key", KEY_A.to_string())],
        ),
    ];
    let mut wrong = Vec::new();
    for ep in catalog::all().iter().filter(|e| e.is_control()) {
        if ep.protocol == catalog::Protocol::WebSocket {
            continue;
        }
        for m in ep.expanded_methods() {
            let method: Method = m.parse().expect("method");
            let path = ep
                .examples
                .iter()
                .find(|e| e.method == m)
                .map(|e| e.path.to_string())
                .unwrap_or_else(|| fill(ep.path));
            for (col, app, headers) in &creds {
                let allowed = new_allows(*col, ep, &method);
                let (status, body) = call(app, m, &path, headers).await;
                let refused = status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN;
                let route_refusal = refused
                    && body["reason"] == "permission_required"
                    && ep
                        .need(m)
                        .and_then(Need::permission)
                        .is_some_and(|p| body["permission"] == p.name());
                if allowed && route_refusal {
                    wrong.push(format!(
                        "{col:?} {m} {}: refused by the route check",
                        ep.path
                    ));
                }
                if !allowed && !refused {
                    wrong.push(format!("{col:?} {m} {}: let through ({status})", ep.path));
                }
                if !allowed && refused && *col != Col::Key && !route_refusal {
                    wrong.push(format!(
                        "{col:?} {m} {}: refused without naming the permission: {body}",
                        ep.path
                    ));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Deny by default: every control route declares, for every method, a
/// permission or that it is open or needs only a signed-in caller.
#[test]
fn every_control_route_declares_a_permission() {
    let mut missing = Vec::new();
    let mut declared = 0;
    for ep in catalog::all().iter().filter(|e| e.is_control()) {
        for m in ep.expanded_methods() {
            match ep.need(m) {
                Some(_) => declared += 1,
                None => missing.push(format!("{m} {}", ep.path)),
            }
        }
        for (m, _) in &ep.needs {
            assert!(
                m.is_empty() || ep.expanded_methods().contains(m),
                "{} declares {m}, which it does not have",
                ep.path
            );
        }
    }
    assert!(declared > 150, "{declared}");
    assert!(
        missing.is_empty(),
        "control routes without a permission (Endpoint::permission, ::permission_for, ::open or ::authenticated): {missing:#?}"
    );
    // The OpenAPI document says the same.
    let spec = crate::openapi::build_spec();
    for ep in catalog::all().iter().filter(|e| e.is_control()) {
        let item = spec["paths"]
            .as_object()
            .and_then(|p| {
                p.iter().find(|(k, _)| {
                    k.split('/').map(|s| s.starts_with('{')).collect::<Vec<_>>()
                        == ep
                            .path
                            .split('/')
                            .map(|s| s.starts_with('{'))
                            .collect::<Vec<_>>()
                        && k.split('/')
                            .zip(ep.path.split('/'))
                            .all(|(a, b)| a == b || (a.starts_with('{') && b.starts_with('{')))
                })
            })
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        for m in ep.expanded_methods() {
            assert_eq!(
                item[m.to_ascii_lowercase()]["x-rustybin-permission"],
                json!(ep.need(m).map(Need::label)),
                "OpenAPI {m} {}",
                ep.path
            );
        }
    }
    // Data-plane routes are not checked here and declare nothing.
    for ep in catalog::all().iter().filter(|e| !e.is_control()) {
        assert!(ep.needs.is_empty(), "{} is not a control route", ep.path);
    }
    // The open routes are the ones control auth leaves alone.
    for ep in catalog::all().iter().filter(|e| e.is_control()) {
        for m in ep.expanded_methods() {
            let method: Method = m.parse().expect("method");
            let open = ep.need(m) == Some(Need::Open);
            let unprotected = !crate::control_auth::is_protected_path(&fill(ep.path))
                || crate::harness::is_node_path(&method, &fill(ep.path));
            assert_eq!(
                open, unprotected,
                "{m} {}: open and unauthenticated agree",
                ep.path
            );
        }
    }
}

/// The checks the access model counts as uses of a permission.
fn used_permissions() -> PermSet {
    let mut used = PermSet::EMPTY;
    for ep in catalog::all() {
        for (_, n) in &ep.needs {
            if let Some(p) = n.permission() {
                used.insert(p);
            }
        }
    }
    for t in crate::mcp::manage::tools::TOOLS {
        if let Some(p) = t.permission.permission() {
            used.insert(p);
        }
    }
    for (p, _) in super::need::BODY_CHECKS {
        used.insert(*p);
    }
    used
}

/// Every permission is checked somewhere (a route, a tool or a listed
/// handler check), except the ones a later step needs, which say so.
#[test]
fn every_permission_is_used() {
    let used = used_permissions();
    for p in super::Permission::ALL {
        let reserved = super::need::RESERVED.iter().any(|(r, _)| r == p);
        assert!(
            used.has(*p) != reserved,
            "{}: {}",
            p.name(),
            if reserved {
                "reserved for a later step but used: drop it from RESERVED"
            } else {
                "no route, tool or check uses it"
            }
        );
    }
    // Each listed handler check is made in the code.
    let src = |f: &str| {
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(f))
            .unwrap_or_default()
    };
    for (file, needle) in [
        ("src/inspector.rs", "Permission::TrafficClear"),
        ("src/assist/api.rs", "Permission::AssistJobs"),
        ("src/checkpoints.rs", "Permission::GitRead"),
        ("src/mcp/inspector/mod.rs", "Permission::McpConnections"),
        ("src/mcp/inspector/relay.rs", "Permission::McpConnections"),
        ("src/settings.rs", "P::SettingsSecurity"),
        ("src/settings.rs", "P::PortalSettings"),
        ("src/settings.rs", "P::SecretsWrite"),
        ("src/configdoc/mod.rs", "P::SettingsWrite"),
        ("src/configdoc/mod.rs", "P::SecretsWrite"),
        ("src/configdoc/mod.rs", "P::AgentsManage"),
        ("src/health.rs", "Permission::SettingsWrite"),
        ("src/flaky.rs", "Permission::SettingsWrite"),
        ("src/portal/session.rs", "Permission::PortalRead"),
    ] {
        assert!(src(file).contains(needle), "{file} checks {needle}");
    }
}
