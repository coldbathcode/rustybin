//! Projects: roles on part of an instance (access control step 11).
//!
//! A project (at most [`MAX_PROJECTS`]) has a name (the mock name rules), a
//! description and its members: bindings `{user, roles}`. A mock, a saved
//! test, a checkpoint and a mock's portal item may belong to one project;
//! without one they belong to the instance, as everything did before.
//!
//! A permission on a resource of a project is the union of the user's
//! instance roles and their roles in that project (their binding, plus the
//! roles a single sign-on group maps to in it). Instance-wide things
//! (settings, egress, users, git) need instance roles. A user with a binding
//! also holds `instance.read` (the console's status and catalogue).
//!
//! Projects live in `users.json` beside the users and roles, so a binding,
//! the role it names and the user it names change under one writer: a
//! deleted user leaves every project, a role a binding uses is never
//! deleted, and the guard rails ("the last enabled admin") count instance
//! roles only, so a project's `admin` never keeps nor replaces an
//! instance admin.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{now_rfc3339, Doc, Users, UsersError, MAX_ROLES_PER_USER, MAX_USERS};
use crate::rbac::permission::{PermSet, Permission};

/// Projects an instance keeps.
pub const MAX_PROJECTS: usize = 100;
/// Members of one project (every user at most).
pub const MAX_MEMBERS: usize = MAX_USERS;
pub const MAX_DESCRIPTION_CHARS: usize = 200;
/// The name that stands for the instance's resources (`move_to=instance`,
/// `?project=instance`): never a project's.
pub const INSTANCE: &str = "instance";
/// The refusal of a project that still holds resources.
pub const NOT_EMPTY: &str = "not_empty";

/// A user's roles in a project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// The user id.
    pub user: String,
    pub roles: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub members: Vec<Member>,
    #[serde(default)]
    pub created_at: String,
}

/// What a caller holds in one project (resolved per request).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectGrant {
    pub roles: Vec<String>,
    pub permissions: PermSet,
}

/// One of a user's bindings as answers show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub project: String,
    pub roles: Vec<String>,
    /// Given by a single sign-on group (`sso.role_map`), not by a member
    /// entry of the project.
    pub sso: bool,
}

impl Binding {
    pub fn view(&self) -> Value {
        json!({ "project": self.project, "roles": self.roles, "sso": self.sso })
    }
}

/// A project name follows the mock name rules: 1 to 40 lower case letters,
/// digits and `-`, not starting or ending with `-`.
pub fn check_name(name: &str) -> Result<(), String> {
    if name == INSTANCE {
        return Err(format!(
            "project {INSTANCE:?}: reserved (it names the instance's resources, as in move_to=instance)"
        ));
    }
    if crate::mocks::model::valid_name(name) {
        Ok(())
    } else {
        Err(format!(
            "project {}: 1 to 40 lower case letters, digits and -, not starting or ending with -",
            crate::logging::printable(name, 64)
        ))
    }
}

fn check_description(d: &str) -> Result<(), String> {
    if d.chars().count() > MAX_DESCRIPTION_CHARS || d.chars().any(char::is_control) {
        Err(format!(
            "description: at most {MAX_DESCRIPTION_CHARS} characters, no control characters"
        ))
    } else {
        Ok(())
    }
}

impl Doc {
    pub(super) fn project(&self, name: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.name == name)
    }

    /// The bindings of a user: their member entries, then the projects
    /// their single sign-on groups map to (projects that exist only).
    pub(super) fn bindings_of(&self, user_id: &str) -> Vec<Binding> {
        let mut out: Vec<Binding> = self
            .projects
            .iter()
            .filter_map(|p| {
                p.members
                    .iter()
                    .find(|m| m.user == user_id)
                    .map(|m| Binding {
                        project: p.name.clone(),
                        roles: m.roles.clone(),
                        sso: false,
                    })
            })
            .collect();
        let mapped = self
            .user(user_id)
            .and_then(|u| u.sso.as_ref())
            .map(|l| l.mapped_projects.clone())
            .unwrap_or_default();
        for (project, roles) in mapped {
            if self.project(&project).is_none() {
                continue;
            }
            match out.iter_mut().find(|b| b.project == project) {
                Some(b) => {
                    b.roles = super::union_roles(&b.roles, &roles);
                }
                None => out.push(Binding {
                    project,
                    roles,
                    sso: true,
                }),
            }
        }
        out
    }

    /// The per-project grants of a user (known roles only).
    pub(super) fn grants_of(&self, user_id: &str) -> BTreeMap<String, ProjectGrant> {
        let mut out = BTreeMap::new();
        for b in self.bindings_of(user_id) {
            let mut g = ProjectGrant::default();
            for r in &b.roles {
                if let Some(p) = self.role_permissions(r) {
                    g.permissions = g.permissions.union(p);
                    if !g.roles.contains(r) {
                        g.roles.push(r.clone());
                    }
                }
            }
            if !g.roles.is_empty() {
                out.insert(b.project, g);
            }
        }
        out
    }

    /// Does any binding of a project name the role?
    pub(super) fn binding_holders(&self, role: &str) -> usize {
        self.projects
            .iter()
            .flat_map(|p| &p.members)
            .filter(|m| m.roles.iter().any(|r| r == role))
            .count()
    }

    fn check_members(&self, members: &[Member]) -> Result<(), Vec<String>> {
        let mut problems = Vec::new();
        if members.len() > MAX_MEMBERS {
            problems.push(format!("members: at most {MAX_MEMBERS}"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for m in members {
            let shown = crate::logging::printable(&m.user, 64);
            if self.user(&m.user).is_none() {
                problems.push(format!("members: {shown} is not a user"));
            }
            if !seen.insert(m.user.as_str()) {
                problems.push(format!("members: {shown} is listed twice"));
            }
            if m.roles.is_empty() {
                problems.push(format!(
                    "members: {shown} needs at least one role (remove the member instead)"
                ));
            }
            if m.roles.len() > MAX_ROLES_PER_USER {
                problems.push(format!(
                    "members: {shown}: at most {MAX_ROLES_PER_USER} roles"
                ));
            }
            if let Err(p) = self.check_roles(&m.roles) {
                problems.extend(p.into_iter().map(|e| format!("members: {shown}: {e}")));
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}

/// A project as answers show it: the name, the description, the members
/// (with their names, when `members` is shown).
pub fn view(users: &Users, p: &Project, members: bool) -> Value {
    let mut v = json!({
        "name": p.name,
        "description": p.description,
        "created_at": p.created_at,
        "member_count": p.members.len(),
    });
    if members {
        let doc = users.read();
        let list: Vec<Value> = p
            .members
            .iter()
            .map(|m| {
                json!({
                    "user": doc.user(&m.user).map(|u| u.name.clone()),
                    "user_id": m.user,
                    "roles": m.roles,
                })
            })
            .collect();
        v["members"] = json!(list);
    }
    v
}

impl Users {
    /// Every project, in creation order.
    pub fn projects(&self) -> Vec<Project> {
        self.read().projects.clone()
    }

    pub fn project(&self, name: &str) -> Option<Project> {
        self.read().project(name).cloned()
    }

    pub fn project_exists(&self, name: &str) -> bool {
        self.read().project(name).is_some()
    }

    /// A user's bindings (member entries and single sign-on groups).
    pub fn bindings(&self, user_id: &str) -> Vec<Binding> {
        self.read().bindings_of(user_id)
    }

    /// Create or change a project: `description` and `members` given
    /// replace what it has (absent: kept; a new project starts with
    /// neither). Answers the project and whether it was created.
    pub fn save_project(
        &self,
        name: &str,
        description: Option<String>,
        members: Option<Vec<Member>>,
    ) -> Result<(Project, bool), UsersError> {
        let mut problems = Vec::new();
        if let Err(e) = check_name(name) {
            problems.push(e);
        }
        if let Some(Err(e)) = description.as_deref().map(check_description) {
            problems.push(e);
        }
        if !problems.is_empty() {
            return Err(UsersError::Invalid(problems));
        }
        let now = now_rfc3339(self.clock.as_ref());
        self.commit(|doc| {
            if let Some(m) = &members {
                doc.check_members(m).map_err(UsersError::Invalid)?;
            }
            let created = doc.project(name).is_none();
            if created && doc.projects.len() >= MAX_PROJECTS {
                return Err(UsersError::Full(format!(
                    "an instance keeps at most {MAX_PROJECTS} projects"
                )));
            }
            if created {
                doc.projects.push(Project {
                    name: name.to_string(),
                    description: String::new(),
                    members: Vec::new(),
                    created_at: now,
                });
            }
            let p = doc
                .projects
                .iter_mut()
                .find(|p| p.name == name)
                .ok_or(UsersError::NotFound)?;
            if let Some(d) = description {
                p.description = d;
            }
            if let Some(m) = members {
                p.members = m;
            }
            Ok((p.clone(), created))
        })
    }

    /// Set (or, with no roles, remove) one user's binding in a project.
    pub fn set_binding(
        &self,
        project: &str,
        user_id: &str,
        roles: Vec<String>,
    ) -> Result<Project, UsersError> {
        self.commit(|doc| Self::bind(doc, project, user_id, roles))
    }

    pub(super) fn bind(
        doc: &mut Doc,
        project: &str,
        user_id: &str,
        roles: Vec<String>,
    ) -> Result<Project, UsersError> {
        if doc.user(user_id).is_none() {
            return Err(UsersError::NotFound);
        }
        if !roles.is_empty() {
            doc.check_members(&[Member {
                user: user_id.to_string(),
                roles: roles.clone(),
            }])
            .map_err(UsersError::Invalid)?;
        }
        let p = doc
            .projects
            .iter_mut()
            .find(|p| p.name == project)
            .ok_or_else(|| {
                UsersError::Invalid(vec![format!(
                    "projects: {} is not a project",
                    crate::logging::printable(project, 64)
                )])
            })?;
        match (
            p.members.iter_mut().find(|m| m.user == user_id),
            roles.is_empty(),
        ) {
            (Some(_), true) => p.members.retain(|m| m.user != user_id),
            (Some(m), false) => m.roles = roles,
            (None, true) => {}
            (None, false) => {
                if p.members.len() >= MAX_MEMBERS {
                    return Err(UsersError::Full(format!(
                        "a project has at most {MAX_MEMBERS} members"
                    )));
                }
                p.members.push(Member {
                    user: user_id.to_string(),
                    roles,
                });
            }
        }
        Ok(p.clone())
    }

    /// Delete a project (its bindings go with it). The caller checks that
    /// it holds no resources first.
    pub fn delete_project(&self, name: &str) -> Result<Project, UsersError> {
        self.commit(|doc| {
            let pos = doc
                .projects
                .iter()
                .position(|p| p.name == name)
                .ok_or(UsersError::NotFound)?;
            Ok(doc.projects.remove(pos))
        })
    }

    /// The permissions a user holds in `project` (instance roles not
    /// included).
    pub fn project_permissions(&self, user_id: &str, project: &str) -> PermSet {
        self.read()
            .grants_of(user_id)
            .get(project)
            .map(|g| g.permissions)
            .unwrap_or(PermSet::EMPTY)
    }
}

/// What a binding adds to a user's instance permissions: `instance.read`
/// (the console's status and catalogue).
pub const BINDING_INSTANCE: &[Permission] = &[Permission::InstanceRead];

/// A `?project=` filter of a list: the instance's resources only
/// (`instance`), or one project's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filter {
    Instance,
    Project(String),
}

impl Filter {
    /// `None` (no filter) for an absent or empty value.
    pub fn parse(raw: Option<&str>) -> Result<Option<Self>, String> {
        match raw.map(str::trim) {
            None | Some("") => Ok(None),
            Some(INSTANCE) => Ok(Some(Self::Instance)),
            Some(p) => check_name(p).map(|_| Some(Self::Project(p.to_string()))),
        }
    }

    /// Does a resource of `project` pass?
    pub fn matches(&self, project: Option<&str>) -> bool {
        match self {
            Self::Instance => project.is_none(),
            Self::Project(p) => project == Some(p.as_str()),
        }
    }
}

/// Does a resource of `project` pass an optional filter?
pub fn passes(filter: &Option<Filter>, project: Option<&str>) -> bool {
    filter.as_ref().is_none_or(|f| f.matches(project))
}

fn unknown_project(name: &str) -> crate::mocks::service::ServiceError {
    crate::mocks::service::ServiceError::with_body(
        axum::http::StatusCode::BAD_REQUEST,
        json!({
            "error": format!("no project {}", crate::logging::printable(name, 64)),
            "reason": "unknown_project",
        }),
    )
}

/// The project a new resource goes to, for a caller who needs `p` there:
/// the one named (it must exist, and the caller hold `p` in it), `instance`
/// or nothing for the instance's (instance-wide `p`). A caller without the
/// instance-wide permission who holds it in exactly one project and names
/// none creates in that project.
pub fn target(
    users: &Users,
    caller: &crate::rbac::Resolved,
    p: Permission,
    requested: Option<&str>,
) -> Result<Option<String>, crate::mocks::service::ServiceError> {
    use crate::mocks::service::ServiceError;
    match requested.map(str::trim).filter(|r| !r.is_empty()) {
        Some(INSTANCE) => {
            if caller.has(p) {
                Ok(None)
            } else {
                Err(ServiceError::permission(caller, p))
            }
        }
        None => {
            if caller.has(p) {
                return Ok(None);
            }
            match caller.projects_with(p).unwrap_or_default().as_slice() {
                [only] => Ok(Some(only.clone())),
                _ => Err(ServiceError::permission(caller, p)),
            }
        }
        Some(name) => {
            if !users.project_exists(name) {
                // Only a caller who holds `p` on the whole instance learns
                // that the name is unknown: anyone else gets the refusal a
                // project they may not use gets, so names cannot be probed.
                return Err(if caller.has(p) {
                    unknown_project(name)
                } else {
                    ServiceError::permission(caller, p)
                });
            }
            if caller.has_in(p, Some(name)) {
                Ok(Some(name.to_string()))
            } else {
                Err(ServiceError::permission(caller, p))
            }
        }
    }
}

/// May the caller move a resource from `from` to `to` (a move needs `p`
/// in both)? `to` must exist.
pub fn check_move(
    users: &Users,
    caller: &crate::rbac::Resolved,
    p: Permission,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(), crate::mocks::service::ServiceError> {
    if let Some(t) = to {
        if !users.project_exists(t) {
            // As in `target`: the same refusal as a project the caller may
            // not use, unless they hold `p` on the whole instance.
            return Err(if caller.has(p) {
                unknown_project(t)
            } else {
                crate::mocks::service::ServiceError::permission(caller, p)
            });
        }
    }
    for side in [from, to] {
        if !caller.has_in(p, side) {
            return Err(crate::mocks::service::ServiceError::permission(caller, p));
        }
    }
    Ok(())
}

/// A `project` field of a body: absent (no change), `null` or `instance`
/// (the instance's), or a name.
pub fn parse_move(v: Option<&Value>) -> Result<Option<Option<String>>, String> {
    match v {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) if s == INSTANCE || s.is_empty() => Ok(Some(None)),
        Some(Value::String(s)) => check_name(s).map(|_| Some(Some(s.clone()))),
        Some(_) => Err("project: a project name, instance or null".into()),
    }
}
