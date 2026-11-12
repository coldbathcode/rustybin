//! `projects:` of `rustybin.yaml` (step 11): each project with its
//! description and members by user name. Exported when the instance has
//! projects. Applied in two steps: the projects first (so `project:` on
//! mocks and tests finds them), the members after `users:` (so a member the
//! document invites exists); with `prune`, a project the document does not
//! name is deleted only when it holds nothing.
//!
//! A caller gives only roles it holds in the project, never changes its own
//! (the rules of `PUT /_rustybin/projects/{name}`); the section needs
//! `users.manage`. A document from the repository (`untrusted`) may create
//! a project and change descriptions, never members (a binding gives
//! roles), nor prune one; and never moves an existing mock or test to
//! another project (`project_gate`).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::ApplyOptions;
use crate::rbac::projects::{self, Member};
use crate::state::AppState;

/// One entry of `projects:`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDoc {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<MemberDoc>,
}

/// A member by user name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberDoc {
    pub user: String,
    pub roles: Vec<String>,
}

pub fn schema() -> Value {
    json!({
        "type": "array",
        "maxItems": projects::MAX_PROJECTS,
        "description": "Projects: roles on part of the instance. A mock or a saved test names its project with project:. Members by user name; a user's roles in a project apply to its resources on top of their instance roles. A document from the repository creates projects and changes descriptions only.",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["name"],
            "properties": {
                "name": { "type": "string", "pattern": "^[a-z0-9]([a-z0-9-]{0,38}[a-z0-9])?$", "description": "The mock name rules; never instance" },
                "description": { "type": "string", "maxLength": projects::MAX_DESCRIPTION_CHARS },
                "members": { "type": "array", "maxItems": projects::MAX_MEMBERS, "items": {
                    "type": "object", "additionalProperties": false, "required": ["user", "roles"],
                    "properties": {
                        "user": { "type": "string", "description": "A user's sign-in name" },
                        "roles": { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                    }
                } }
            }
        }
    })
}

/// The projects as the document carries them (`None` when there are none,
/// so an instance without projects exports as before).
pub fn export(state: &AppState) -> Option<Vec<ProjectDoc>> {
    let list = state.users.projects();
    if list.is_empty() {
        return None;
    }
    Some(
        list.into_iter()
            .map(|p| ProjectDoc {
                members: p
                    .members
                    .iter()
                    .filter_map(|m| {
                        state.users.get(&m.user).map(|u| MemberDoc {
                            user: u.name,
                            roles: m.roles.clone(),
                        })
                    })
                    .collect(),
                name: p.name,
                description: p.description,
            })
            .collect(),
    )
}

fn key(name: &str) -> String {
    format!("projects.{}", crate::logging::printable(name, 64))
}

/// Step one: the projects the document names exist (created, or their
/// description changed). Answers the names a resource may name afterwards
/// (in a dry run: those that exist plus those it would create).
pub fn ensure(
    state: &AppState,
    docs: Option<&Vec<ProjectDoc>>,
    opts: &ApplyOptions,
    problems: &mut Vec<String>,
    result: &mut Value,
) -> BTreeSet<String> {
    let mut known: BTreeSet<String> = state.users.projects().into_iter().map(|p| p.name).collect();
    let Some(docs) = docs else {
        return known;
    };
    let mut created = Vec::new();
    let mut changed = Vec::new();
    for d in docs {
        if let Err(e) = projects::check_name(&d.name) {
            problems.push(format!("{}: {e} (skipped)", key(&d.name)));
            continue;
        }
        let existing = state.users.project(&d.name);
        let same = existing
            .as_ref()
            .is_some_and(|p| p.description == d.description);
        if same {
            continue;
        }
        if opts.dry_run {
            if existing.is_none() {
                if known.len() >= projects::MAX_PROJECTS {
                    problems.push(format!(
                        "projects: an instance keeps at most {} projects",
                        projects::MAX_PROJECTS
                    ));
                }
                created.push(d.name.clone());
            } else {
                changed.push(d.name.clone());
            }
            known.insert(d.name.clone());
            continue;
        }
        match state
            .users
            .save_project(&d.name, Some(d.description.clone()), None)
        {
            Ok((_, true)) => {
                created.push(d.name.clone());
                known.insert(d.name.clone());
            }
            Ok((_, false)) => changed.push(d.name.clone()),
            Err(e) => problems.push(format!("{}: {e}", key(&d.name))),
        }
    }
    result["created"] = json!(created);
    result["changed"] = json!(changed);
    known
}

/// Step two (after `users:`): the members, then the prune.
pub fn members(
    state: &AppState,
    docs: Option<Vec<ProjectDoc>>,
    opts: &ApplyOptions,
    problems: &mut Vec<String>,
    result: &mut Value,
) {
    let Some(docs) = docs else { return };
    let caller = opts.caller.clone();
    let mut updated = Vec::new();
    for d in &docs {
        let current = state.users.project(&d.name).unwrap_or_default();
        let current_docs: Vec<MemberDoc> = current
            .members
            .iter()
            .filter_map(|m| {
                state.users.get(&m.user).map(|u| MemberDoc {
                    user: u.name,
                    roles: m.roles.clone(),
                })
            })
            .collect();
        if current_docs == d.members {
            continue;
        }
        if opts.untrusted {
            problems.push(format!(
                "{}: a document from the repository cannot change a project's members (change them through the API, the console or the CLI): kept as they are",
                key(&d.name)
            ));
            continue;
        }
        let mut wanted: Vec<Member> = Vec::new();
        let mut unknown = false;
        for m in &d.members {
            match state.users.find_by_name(&m.user) {
                Some(u) => wanted.push(Member {
                    user: u.id,
                    roles: m.roles.clone(),
                }),
                // A user the document's users: section creates exists
                // only once it is applied.
                None if opts.dry_run => unknown = true,
                None => {
                    problems.push(format!(
                        "{}: {} is not a user (members kept as they are)",
                        key(&d.name),
                        crate::logging::printable(&m.user, 64)
                    ));
                    unknown = true;
                }
            }
        }
        if unknown && !opts.dry_run {
            continue;
        }
        if let Some(c) = &caller {
            if let Err(e) = crate::rbac::project_api::check_members_change(
                state,
                c,
                &d.name,
                &current.members,
                &wanted,
            ) {
                problems.push(format!("{}: {} (members kept)", key(&d.name), e.message()));
                continue;
            }
        }
        if opts.dry_run {
            updated.push(d.name.clone());
            continue;
        }
        match state.users.save_project(&d.name, None, Some(wanted)) {
            Ok(_) => updated.push(d.name.clone()),
            Err(e) => problems.push(format!("{}: {e}", key(&d.name))),
        }
    }
    result["members_changed"] = json!(updated);
    if !opts.prune {
        return;
    }
    let named: BTreeSet<&str> = docs.iter().map(|d| d.name.as_str()).collect();
    let mut deleted = Vec::new();
    for p in state.users.projects() {
        if named.contains(p.name.as_str()) {
            continue;
        }
        let k = key(&p.name);
        if opts.untrusted {
            problems.push(format!(
                "{k}: a document from the repository cannot delete a project: kept"
            ));
            continue;
        }
        let held = crate::rbac::project_api::resources(state, &p.name);
        if !held.is_empty() {
            problems.push(format!(
                "{k}: holds {} mock(s), {} saved test(s), {} checkpoint(s) and {} storyboard(s), so not pruned",
                held.mocks.len(),
                held.tests.len(),
                held.checkpoints.len(),
                held.storyboards.len()
            ));
            continue;
        }
        if let Some(c) = &caller {
            if let Err(e) =
                crate::rbac::project_api::check_members_change(state, c, &p.name, &p.members, &[])
            {
                problems.push(format!("{k}: {} (kept)", e.message()));
                continue;
            }
        }
        if opts.dry_run {
            deleted.push(p.name);
            continue;
        }
        match state.users.delete_project(&p.name) {
            Ok(_) => deleted.push(p.name),
            Err(e) => problems.push(format!("{k}: {e}")),
        }
    }
    result[if opts.dry_run { "delete" } else { "deleted" }] = json!(deleted);
}

/// The project a mock or a test of the document goes to. `wanted` must be
/// a project the instance has (or the document creates); a document from
/// the repository never moves an existing resource (`existing` is
/// `Some(current project)` for one that exists). Answers the project to
/// apply.
pub fn project_gate(
    what: &str,
    wanted: Option<String>,
    existing: Option<Option<String>>,
    known: &BTreeSet<String>,
    opts: &ApplyOptions,
    problems: &mut Vec<String>,
) -> Option<String> {
    if opts.untrusted {
        if let Some(current) = &existing {
            if *current != wanted {
                problems.push(format!(
                    "{what}: a document from the repository cannot move it to another project: kept in {}",
                    current.as_deref().unwrap_or("the instance's")
                ));
                return current.clone();
            }
        }
    }
    match wanted {
        Some(p) if !known.contains(&p) => {
            problems.push(format!(
                "{what}: {} is not a project (projects: creates one): {}",
                crate::logging::printable(&p, 64),
                if existing.is_some() {
                    "kept where it is"
                } else {
                    "the instance's"
                }
            ));
            existing.flatten()
        }
        other => other,
    }
}
