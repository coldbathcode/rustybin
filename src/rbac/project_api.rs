//! Projects (step 11): the logic the REST routes (`/_rustybin/projects*`),
//! the management tools (`list_projects`, `save_project`,
//! `delete_project`), the CLI and documents share, and the routes.
//!
//! - **Seeing a project**: its members, or a caller who reads users
//!   (`users.read`) or every mock (instance-wide `mocks.read`). The
//!   members are shown with `users.read`; everyone else sees their own
//!   roles in it.
//! - **Changing one** (create, describe, set the members, delete) needs
//!   `users.manage` (a binding gives roles), and the rules of users and
//!   roles hold: nobody changes their own roles in a project, and nobody
//!   gives (or takes) roles whose permissions they do not hold in that
//!   project.
//! - **Deleting** one is refused while it holds resources (mocks, saved
//!   tests, checkpoints): `409 not_empty` with the counts, unless
//!   `move_to` names where they go (another project, or `instance`), which
//!   needs the write permission of each kind of resource in both.

use axum::extract::{Path as RoutePath, Query, State};
use axum::http::{Extensions, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::projects::{self, Member, Project};
use super::{PermSet, Permission, Resolved};
use crate::catalog::{category, Endpoint, Example};
use crate::mocks::service::{ServiceError, SvcResult};
use crate::state::AppState;

pub const PROJECTS_PATH: &str = "/_rustybin/projects";
pub const PROJECT_PATH: &str = "/_rustybin/projects/{name}";

fn refused(status: StatusCode, reason: &str, message: impl Into<String>) -> ServiceError {
    ServiceError::with_body(status, json!({ "error": message.into(), "reason": reason }))
}

fn not_found(name: &str) -> ServiceError {
    refused(
        StatusCode::NOT_FOUND,
        "not_found",
        format!("no project {}", crate::logging::printable(name, 64)),
    )
}

/// The resources of a project, by kind.
#[derive(Clone, Debug, Default)]
pub struct Resources {
    pub mocks: Vec<String>,
    pub tests: Vec<String>,
    pub checkpoints: Vec<String>,
}

impl Resources {
    pub fn is_empty(&self) -> bool {
        self.mocks.is_empty() && self.tests.is_empty() && self.checkpoints.is_empty()
    }

    pub fn view(&self) -> Value {
        json!({
            "mocks": self.mocks,
            "tests": self.tests,
            "checkpoints": self.checkpoints,
        })
    }
}

/// What belongs to `project`: mocks, saved tests, checkpoints. (A mock's
/// portal item follows its mock.)
pub fn resources(state: &AppState, project: &str) -> Resources {
    Resources {
        mocks: state.mocks.in_project(project),
        tests: state
            .saved_tests
            .list()
            .into_iter()
            .filter(|t| t.project.as_deref() == Some(project))
            .map(|t| t.name)
            .collect(),
        checkpoints: state.checkpoints.names_in(project),
    }
}

/// May the caller see the project?
fn visible(caller: &Resolved, name: &str) -> bool {
    caller.projects.contains_key(name)
        || caller.has(Permission::UsersRead)
        || caller.has(Permission::MocksRead)
}

fn project_view(state: &AppState, caller: &Resolved, p: &Project) -> Value {
    let mut v = projects::view(&state.users, p, caller.has(Permission::UsersRead));
    let mine = caller.projects.get(&p.name);
    v["your_roles"] = json!(mine.map(|g| g.roles.clone()).unwrap_or_default());
    v["your_permissions"] = json!(mine.map(|g| g.permissions.names()).unwrap_or_default());
    // The names of what the caller may read in the project only: seeing a
    // project (a member, `users.read`) is not reading its mocks or tests.
    let mut held = resources(state, &p.name);
    let may = |perm| caller.has_in(perm, Some(&p.name));
    if !may(Permission::MocksRead) {
        held.mocks.clear();
        held.checkpoints.clear();
    }
    if !may(Permission::TestsRead) {
        held.tests.clear();
    }
    v["resources"] = held.view();
    v
}

/// `GET /_rustybin/projects`: the projects the caller may see.
pub fn list(state: &AppState, caller: &Resolved) -> Value {
    let list: Vec<Value> = state
        .users
        .projects()
        .iter()
        .filter(|p| visible(caller, &p.name))
        .map(|p| project_view(state, caller, p))
        .collect();
    json!({
        "count": list.len(),
        "capacity": projects::MAX_PROJECTS,
        "projects": list,
    })
}

/// `GET /_rustybin/projects/{name}`.
pub fn get(state: &AppState, caller: &Resolved, name: &str) -> SvcResult<Value> {
    match state.users.project(name) {
        Some(p) if visible(caller, &p.name) => Ok(project_view(state, caller, &p)),
        _ => Err(not_found(name)),
    }
}

/// One member as a body or a tool gives it: a user (id or sign-in name)
/// and roles.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberInput {
    pub user: String,
    pub roles: Vec<String>,
}

/// The body of `PUT /_rustybin/projects/{name}`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveInput {
    #[serde(default)]
    pub description: Option<String>,
    /// The whole member list (absent: kept).
    #[serde(default)]
    pub members: Option<Vec<MemberInput>>,
}

fn roles_permissions(state: &AppState, roles: &[String]) -> PermSet {
    roles
        .iter()
        .filter_map(|r| state.users.role_permissions(r))
        .fold(PermSet::EMPTY, PermSet::union)
}

/// The caller holds every permission of `perms` in `project`, else the
/// refusal naming the first one missing.
fn within_project(caller: &Resolved, project: &str, perms: PermSet) -> SvcResult<()> {
    match perms.iter().find(|p| !caller.has_in(*p, Some(project))) {
        None => Ok(()),
        Some(p) => Err(ServiceError::permission(caller, p)),
    }
}

/// Resolve the members of a body (names or ids) to user ids.
pub fn resolve_members(state: &AppState, input: Vec<MemberInput>) -> SvcResult<Vec<Member>> {
    let mut out = Vec::new();
    for m in input {
        let Some(u) = state.users.find(&m.user) else {
            return Err(refused(
                StatusCode::BAD_REQUEST,
                "invalid",
                format!(
                    "members: {} is not a user",
                    crate::logging::printable(&m.user, 64)
                ),
            ));
        };
        out.push(Member {
            user: u.id,
            roles: m.roles,
        });
    }
    Ok(out)
}

/// Is the change of members allowed for the caller: their own roles never
/// change, and every role given or taken is within what the caller holds
/// in the project?
pub fn check_members_change(
    state: &AppState,
    caller: &Resolved,
    project: &str,
    before: &[Member],
    after: &[Member],
) -> SvcResult<()> {
    let roles_of = |list: &[Member], id: &str| -> Vec<String> {
        list.iter()
            .find(|m| m.user == id)
            .map(|m| m.roles.clone())
            .unwrap_or_default()
    };
    let mut ids: Vec<&str> = before.iter().map(|m| m.user.as_str()).collect();
    for m in after {
        if !ids.contains(&m.user.as_str()) {
            ids.push(&m.user);
        }
    }
    for id in ids {
        let (b, a) = (roles_of(before, id), roles_of(after, id));
        if b == a {
            continue;
        }
        if caller.user_id() == Some(id) {
            return Err(refused(
                StatusCode::FORBIDDEN,
                "own_roles",
                "nobody changes their own roles, in a project too: ask another user who manages users",
            ));
        }
        within_project(caller, project, roles_permissions(state, &b))?;
        within_project(caller, project, roles_permissions(state, &a))?;
    }
    Ok(())
}

fn audit(event: &str, caller: &Resolved, project: &str) {
    crate::audit::event(event);
    crate::audit::target("project", project);
    tracing::info!(
        target: "rustybin::auth",
        event,
        by = %crate::logging::printable(&super::service::principal_name(caller), 80),
        project = %crate::logging::printable(project, 64),
        "projects"
    );
}

/// `PUT /_rustybin/projects/{name}`: create the project or change it.
pub fn save(state: &AppState, caller: &Resolved, name: &str, input: SaveInput) -> SvcResult<Value> {
    let members = match input.members {
        Some(m) => Some(resolve_members(state, m)?),
        None => None,
    };
    if let Some(after) = &members {
        let before = state
            .users
            .project(name)
            .map(|p| p.members)
            .unwrap_or_default();
        check_members_change(state, caller, name, &before, after)?;
    }
    let (p, created) = state
        .users
        .save_project(name, input.description, members)
        .map_err(super::service::store_error)?;
    audit(
        if created {
            "create_project"
        } else {
            "change_project"
        },
        caller,
        name,
    );
    let mut v = project_view(state, caller, &p);
    v["created"] = json!(created);
    Ok(v)
}

/// Where a deleted project's resources go: another project, or the
/// instance (`instance`).
pub fn parse_move_to(raw: Option<&str>) -> SvcResult<Option<Option<String>>> {
    match raw.map(str::trim).filter(|r| !r.is_empty()) {
        None => Ok(None),
        Some(projects::INSTANCE) => Ok(Some(None)),
        Some(p) => projects::check_name(p)
            .map(|_| Some(Some(p.to_string())))
            .map_err(|e| refused(StatusCode::BAD_REQUEST, "invalid", e)),
    }
}

/// `DELETE /_rustybin/projects/{name}` (`move_to`: where its resources go
/// first; without it a project that holds any is refused).
pub fn delete(
    state: &AppState,
    caller: &Resolved,
    name: &str,
    move_to: Option<Option<String>>,
) -> SvcResult<Value> {
    let Some(p) = state.users.project(name) else {
        return Err(not_found(name));
    };
    if let Some(Some(t)) = &move_to {
        if t == name {
            return Err(refused(
                StatusCode::BAD_REQUEST,
                "invalid",
                "move_to: another project (or instance), not the one deleted",
            ));
        }
    }
    // Taking the members' roles away is a change of their roles.
    check_members_change(state, caller, name, &p.members, &[])?;
    let held = resources(state, name);
    let mut moved = json!({});
    if !held.is_empty() {
        let Some(to) = move_to else {
            return Err(ServiceError::with_body(
                StatusCode::CONFLICT,
                json!({
                    "error": format!(
                        "the project {name} holds {} mock(s), {} saved test(s) and {} checkpoint(s): move them first, or delete with move_to (another project, or instance)",
                        held.mocks.len(), held.tests.len(), held.checkpoints.len()
                    ),
                    "reason": projects::NOT_EMPTY,
                    "resources": held.view(),
                }),
            ));
        };
        let to = to.as_deref();
        let need = |kinds: &[(bool, Permission)]| -> SvcResult<()> {
            for (has, p) in kinds {
                if *has {
                    projects::check_move(&state.users, caller, *p, Some(name), to)?;
                }
            }
            Ok(())
        };
        need(&[
            (!held.mocks.is_empty(), Permission::MocksWrite),
            (!held.tests.is_empty(), Permission::TestsWrite),
            (!held.checkpoints.is_empty(), Permission::CheckpointsWrite),
        ])?;
        moved = move_resources(state, &held, to);
    }
    let gone = state
        .users
        .delete_project(name)
        .map_err(super::service::store_error)?;
    audit("delete_project", caller, name);
    Ok(json!({ "deleted": gone.name, "moved": moved }))
}

/// Move a project's resources to `to` (`None`: the instance's).
pub fn move_resources(state: &AppState, held: &Resources, to: Option<&str>) -> Value {
    let mut mocks = Vec::new();
    for m in &held.mocks {
        if state.mocks.set_project(m, to.map(str::to_string)).is_ok() {
            mocks.push(m.clone());
        }
    }
    let mut tests = Vec::new();
    for t in &held.tests {
        if let Some(mut test) = state.saved_tests.get(t) {
            test.project = to.map(str::to_string);
            if state.saved_tests.put(test, true).is_ok() {
                tests.push(t.clone());
            }
        }
    }
    let from = held
        .checkpoints
        .first()
        .and_then(|c| state.checkpoints.get(c))
        .and_then(|c| c.project.clone());
    let checkpoints = match from {
        Some(f) => crate::checkpoints::move_project(state, &f, to),
        None => Vec::new(),
    };
    json!({ "to": to.unwrap_or(projects::INSTANCE), "mocks": mocks, "tests": tests, "checkpoints": checkpoints })
}

// ── Routes ──────────────────────────────────────────────────────────

fn answer(r: SvcResult<Value>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn list_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    Json(list(&state, &caller)).into_response()
}

async fn get_handler(
    State(state): State<AppState>,
    RoutePath(name): RoutePath<String>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    answer(get(&state, &caller, &name))
}

async fn put_handler(
    State(state): State<AppState>,
    RoutePath(name): RoutePath<String>,
    headers: HeaderMap,
    extensions: Extensions,
    body: axum::body::Bytes,
) -> Response {
    let input: SaveInput = if body.iter().all(u8::is_ascii_whitespace) {
        SaveInput::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(i) => i,
            Err(e) => {
                return refused(
                    StatusCode::BAD_REQUEST,
                    "invalid_body",
                    format!(
                        "expected {{\"description\", \"members\": [{{\"user\", \"roles\"}}]}}: {e}"
                    ),
                )
                .into_response()
            }
        }
    };
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    match save(&state, &caller, &name, input) {
        Ok(v) if v["created"] == json!(true) => (StatusCode::CREATED, Json(v)).into_response(),
        r => answer(r),
    }
}

#[derive(Deserialize)]
struct DeleteQuery {
    #[serde(default)]
    move_to: Option<String>,
}

async fn delete_handler(
    State(state): State<AppState>,
    RoutePath(name): RoutePath<String>,
    Query(q): Query<DeleteQuery>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    let move_to = match parse_move_to(q.move_to.as_deref()) {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    answer(delete(&state, &caller, &name, move_to))
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(PROJECTS_PATH, routing::get(list_handler))
        .route(
            PROJECT_PATH,
            routing::get(get_handler)
                .put(put_handler)
                .delete(delete_handler),
        )
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            PROJECTS_PATH,
            &["GET"],
            category::CONTROL,
            "Projects: roles on part of the instance (the ones the caller is in, or every one for a caller who reads users or every mock)",
        )
        .authenticated(&[])
        .description(
            "A project groups mocks, saved tests and checkpoints; a user's roles in it apply to \
             its resources on top of their instance roles. At most 100 projects. Members are \
             listed for a caller with users.read; everyone sees their own roles.",
        )
        .example(Example::get("Projects", PROJECTS_PATH)),
        Endpoint::new(
            PROJECT_PATH,
            &["GET", "PUT", "DELETE"],
            category::CONTROL,
            "One project: read it, create or change it (description, members), delete it (?move_to= where its resources go)",
        )
        .authenticated(&["GET"])
        .permission_for(&["PUT", "DELETE"], Permission::UsersManage)
        .description(
            "PUT creates or changes the project: description and members ({user, roles}, the \
             whole list) given replace what it has. Nobody changes their own roles in a project, \
             and a caller gives or takes only roles whose permissions it holds there. DELETE \
             refuses a project that holds mocks, saved tests or checkpoints (409 not_empty) \
             unless move_to names another project or instance; the move needs the write \
             permission of each kind in both.",
        )
        .example(Example::get("A project", "/_rustybin/projects/payments").expect_status(404)),
    ]
}

pub fn openapi_paths() -> Value {
    let project = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "description": { "type": "string" },
            "created_at": { "type": "string", "format": "date-time" },
            "member_count": { "type": "integer" },
            "members": { "type": "array", "description": "With users.read", "items": { "type": "object", "properties": {
                "user": { "type": ["string", "null"] }, "user_id": { "type": "string" },
                "roles": { "type": "array", "items": { "type": "string" } }
            } } },
            "your_roles": { "type": "array", "items": { "type": "string" } },
            "your_permissions": { "type": "array", "items": { "type": "string" } },
            "resources": { "type": "object", "properties": {
                "mocks": { "type": "array", "items": { "type": "string" } },
                "tests": { "type": "array", "items": { "type": "string" } },
                "checkpoints": { "type": "array", "items": { "type": "string" } }
            } }
        }
    });
    let name =
        json!({ "name": "name", "in": "path", "required": true, "schema": { "type": "string" } });
    let content = |schema: Value| json!({ "application/json": { "schema": schema } });
    json!({
        PROJECTS_PATH: { "get": {
            "tags": ["Control Plane"], "summary": "Projects", "operationId": "listProjects",
            "responses": { "200": { "description": "The projects the caller may see", "content": content(json!({
                "type": "object", "properties": {
                    "count": { "type": "integer" }, "capacity": { "type": "integer" },
                    "projects": { "type": "array", "items": project.clone() }
                }
            })) } }
        } },
        PROJECT_PATH: {
            "get": {
                "tags": ["Control Plane"], "summary": "A project", "operationId": "getProject",
                "parameters": [name.clone()],
                "responses": { "200": { "description": "The project", "content": content(project.clone()) },
                    "404": { "description": "No such project, or one the caller may not see" } }
            },
            "put": {
                "tags": ["Control Plane"], "summary": "Create or change a project", "operationId": "saveProject",
                "parameters": [name.clone()],
                "requestBody": { "required": false, "content": content(json!({
                    "type": "object", "additionalProperties": false, "properties": {
                        "description": { "type": "string", "maxLength": projects::MAX_DESCRIPTION_CHARS },
                        "members": { "type": "array", "description": "The whole member list", "items": {
                            "type": "object", "required": ["user", "roles"], "additionalProperties": false,
                            "properties": { "user": { "type": "string", "description": "An id or a sign-in name" },
                                "roles": { "type": "array", "items": { "type": "string" } } }
                        } }
                    }
                })) },
                "responses": { "200": { "description": "Changed", "content": content(project.clone()) },
                    "201": { "description": "Created", "content": content(project.clone()) },
                    "400": { "description": "An invalid name, description or member" },
                    "403": { "description": "users.manage, own_roles, or a role beyond the caller's permissions in the project" } }
            },
            "delete": {
                "tags": ["Control Plane"], "summary": "Delete a project", "operationId": "deleteProject",
                "parameters": [name, { "name": "move_to", "in": "query", "required": false,
                    "description": "Where its mocks, saved tests and checkpoints go: another project, or instance",
                    "schema": { "type": "string" } }],
                "responses": { "200": { "description": "Deleted (and what moved)", "content": content(json!({ "type": "object" })) },
                    "404": { "description": "No such project" },
                    "409": { "description": "not_empty: the project holds resources and no move_to was given" } }
            }
        }
    })
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod tests;
