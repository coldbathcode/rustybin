//! What a control-plane route or a management tool needs, and the answer
//! when the caller does not hold it.
//!
//! Every route under `/_rustybin/*` declares a [`Need`] in the catalogue
//! (`Endpoint::permission`, `.open()`, `.authenticated()`), and every
//! management tool declares one in its `ToolDef`. The check is made in one
//! place (`control_auth::check_route` for routes, `Access::allows` for
//! tools); a handler whose need depends on the body adds [`require`].
//!
//! A refusal names the permission that is missing, never what the caller
//! holds: `403 {"error", "reason": "permission_required", "permission"}`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::{PermSet, Permission, Resolved};

/// What a route (for one method) needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// No credential: the readiness probe, the sign-in, the OAuth sign-in
    /// endpoints and the harness node routes (which check the join token or
    /// the node credential themselves).
    Open,
    /// Any caller the control plane accepts: the session, sign-out, the
    /// management MCP server (each tool checks its own permission), the
    /// OAuth consent API (capped at the approver's own permissions) and a
    /// visit's own routes (the visit's manage token).
    Authenticated,
    /// One permission.
    Perm(Permission),
}

impl Need {
    /// The permission, if this is one.
    pub fn permission(self) -> Option<Permission> {
        match self {
            Need::Perm(p) => Some(p),
            _ => None,
        }
    }

    /// How the catalogue, the docs and the CLI reference write it.
    pub fn label(self) -> &'static str {
        match self {
            Need::Open => "open",
            Need::Authenticated => "signed in",
            Need::Perm(p) => p.name(),
        }
    }

    /// Does a caller with `perms` meet it? (`Authenticated` and `Open` are
    /// met by any caller that reached the check.)
    pub fn met_by(self, perms: PermSet) -> bool {
        match self {
            Need::Perm(p) => perms.has(p),
            Need::Open | Need::Authenticated => true,
        }
    }
}

/// The permissions of a caller without a credential on an open control
/// plane that has an admin token (or runs in public mode), and of an API
/// key in shared mode (the key layer keeps a key to its own requests,
/// usage, status, the catalogue, the management MCP server and, with
/// self-service, its own mocks): what such a caller could read in 0.4.
/// Without an admin token and outside public mode an open control plane
/// is every permission, as before.
pub const ANONYMOUS: &[Permission] = &[
    Permission::InstanceRead,
    Permission::MocksRead,
    Permission::TrafficRead,
    Permission::HarnessRead,
    Permission::AssistUse,
    Permission::McpInspect,
];

/// The [`ANONYMOUS`] set.
pub fn anonymous() -> PermSet {
    PermSet::of(ANONYMOUS)
}

/// The checks made inside handlers, on top of the route's declaration,
/// because what a request needs depends on its body or query (and the
/// data-plane routes that change something instance-wide). The docs list
/// them; `every_permission_is_used` counts them.
pub const BODY_CHECKS: &[(Permission, &str)] = &[
    (
        Permission::TrafficClear,
        "DELETE /_rustybin/requests without ?session= (everyone's captures)",
    ),
    (
        Permission::AssistJobs,
        "POST /_rustybin/assist/run with a task that writes (an AI-built mock)",
    ),
    (
        Permission::GitRead,
        "GET /_rustybin/checkpoints?from=git and a restore from the repository",
    ),
    (
        Permission::McpConnections,
        "the MCP relay and checks: a saved connection's credentials, or a method that acts on a server on another host",
    ),
    (
        Permission::SettingsSecurity,
        "a settings change (PATCH /_rustybin/settings, DELETE /_rustybin/settings/{key}, a document's settings:) of a security group: egress, proxy, agents, console, sso, assist.provider, assist.base_url",
    ),
    (
        Permission::PortalSettings,
        "a settings change of a portal.* setting",
    ),
    (
        Permission::SecretsWrite,
        "a settings change that sets or clears a secret, a document with secrets:",
    ),
    (
        Permission::SettingsWrite,
        "a document (POST /_rustybin/config/apply) with settings: or instance:; the /health toggles and the global /flaky reset on the data plane",
    ),
    (
        Permission::AgentsManage,
        "a document with agents:; changing, revoking, restoring or ending the sessions of an AI agent that is not the caller's own (a user's own: limit, revoke, end sessions, restore one they revoked)",
    ),
    (
        Permission::AgentsRead,
        "GET /_rustybin/agents* of agents that are not the caller's own (a user sees their own)",
    ),
    (
        Permission::UsersManage,
        "a document with users: or roles:; PUT /_rustybin/password by a caller who is not a user (the first admin's password)",
    ),
    (
        Permission::PortalRead,
        "the API portal's closed items: their credentials, Try it, collections and connection snippets (/portal/api/*), and the whole portal with portal.auth=password; a portal session or a bearer control credential",
    ),
    (
        Permission::MocksWrite,
        "PUT /_rustybin/portal/items/{id} with secure: true (turns on a mock's mocked security to close it)",
    ),
];

/// The routes whose resources may belong to a project (step 11): the
/// catalogue marks them `.project_scoped()`, the route check lets in a
/// caller who holds the permission in some project, and the handler checks
/// it again on each resource, in the resource's project (another project's
/// resource the caller may not read is `404`, a list leaves it out).
/// `every_project_route_checks_each_resource` calls each one as a member of
/// another project.
pub const PROJECT_ROUTES: &[&str] = &[
    "/_rustybin/mocks",
    "/_rustybin/mocks/import",
    "/_rustybin/mocks/export",
    "/_rustybin/mocks/ai/spec",
    "/_rustybin/mocks/{name}",
    "/_rustybin/mocks/{name}/coverage",
    "/_rustybin/mocks/{name}/ai/examples",
    "/_rustybin/mocks/{name}/ai/edit",
    "/_rustybin/mocks/{name}/enable",
    "/_rustybin/mocks/{name}/disable",
    "/_rustybin/mocks/{name}/deploy",
    "/_rustybin/mocks/{name}/rollback",
    "/_rustybin/mocks/{name}/report",
    "/_rustybin/mocks/{name}/export",
    "/_rustybin/mocks/{name}/spec",
    "/_rustybin/mocks/{name}/revisions/{revision}",
    "/_rustybin/mocks/{name}/duplicate",
    "/_rustybin/mocks/{name}/state",
    "/_rustybin/mocks/{name}/collections/{format}",
    "/_rustybin/mocks/{name}/operations/{op}",
    "/_rustybin/mocks/{name}/operations/{op}/sample",
    "/_rustybin/mocks/{name}/operations/{op}/validate",
    "/_rustybin/mocks/{name}/operations/{op}/rules/test",
    "/_rustybin/mocks/{name}/requests",
    "/_rustybin/mocks/{name}/requests/stream",
    "/_rustybin/mocks/{name}/requests/{n}",
    "/_rustybin/tests",
    "/_rustybin/tests/{name}",
    "/_rustybin/tests/run",
    "/_rustybin/tests/runs",
    "/_rustybin/tests/runs/{id}",
    "/_rustybin/tests/export",
    "/_rustybin/checkpoints",
    "/_rustybin/checkpoints/{name}",
    "/_rustybin/checkpoints/{name}/restore",
    "/_rustybin/portal/items/{id}",
];

/// Permissions no route, tool or check uses yet, and the step that will.
pub const RESERVED: &[(Permission, &str)] =
    &[(Permission::AuditRead, "the audit log (testing-2 step 14)")];

/// The body of a refusal: what is missing, never what the caller holds.
pub fn refusal_body(p: Permission, message: Option<&str>) -> Value {
    let error = match message {
        Some(m) => m.to_string(),
        None => format!("this needs the {} permission", p.name()),
    };
    json!({
        "error": error,
        "reason": "permission_required",
        "permission": p.name(),
    })
}

/// `403 permission_required` naming `p`.
pub fn forbidden(p: Permission) -> Response {
    (StatusCode::FORBIDDEN, Json(refusal_body(p, None))).into_response()
}

/// The refusal for a caller without `p`: `403`, or `401` for a caller
/// with no credential on an open control plane that has an admin token
/// (sending the admin token would let it through).
pub fn refuse(caller: &Resolved, p: Permission) -> Response {
    let (status, message) = refusal_of(caller, p);
    (status, Json(refusal_body(p, message.as_deref()))).into_response()
}

/// The status and message of [`refuse`], as data.
pub fn refusal_of(caller: &Resolved, p: Permission) -> (StatusCode, Option<String>) {
    match caller.principal {
        super::Principal::Anonymous { admin_token: true } => (
            StatusCode::UNAUTHORIZED,
            Some(format!(
                "this needs the {} permission: send the admin token as Authorization: Bearer \
                 <token> or X-Rustybin-Admin-Token",
                p.name()
            )),
        ),
        super::Principal::Anonymous { admin_token: false } => (
            StatusCode::FORBIDDEN,
            Some(format!(
                "this needs the {} permission, which is off in public mode without an admin token",
                p.name()
            )),
        ),
        _ => (StatusCode::FORBIDDEN, None),
    }
}

/// Inside a handler whose need depends on the body (a settings change
/// that touches a security group, a document with secrets): `Ok` when the
/// caller holds `p`, else the refusal.
#[allow(clippy::result_large_err)]
pub fn require(caller: &Resolved, p: Permission) -> Result<(), Response> {
    if caller.has(p) {
        Ok(())
    } else {
        Err(refuse(caller, p))
    }
}
