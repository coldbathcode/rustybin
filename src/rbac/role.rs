//! Roles: a name, a description and a permission set. Five built-in roles
//! cover the usual team and never change; custom roles (at most
//! [`MAX_CUSTOM_ROLES`], kept in the user store) pick any permissions. A
//! role that grants `users.manage` or `settings.security` is privileged and
//! marked so in every view.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::permission::{PermSet, Permission};

/// Custom roles an instance keeps.
pub const MAX_CUSTOM_ROLES: usize = 50;
/// The longest role name and description.
pub const MAX_ROLE_NAME_CHARS: usize = 40;
pub const MAX_ROLE_DESCRIPTION_CHARS: usize = 200;

/// The built-in roles, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltIn {
    Viewer,
    Tester,
    Editor,
    Admin,
    PortalReader,
}

impl BuiltIn {
    pub const ALL: &'static [BuiltIn] = &[
        BuiltIn::Viewer,
        BuiltIn::Tester,
        BuiltIn::Editor,
        BuiltIn::Admin,
        BuiltIn::PortalReader,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            BuiltIn::Viewer => "viewer",
            BuiltIn::Tester => "tester",
            BuiltIn::Editor => "editor",
            BuiltIn::Admin => "admin",
            BuiltIn::PortalReader => "portal_reader",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            BuiltIn::Viewer => "Look around: the console read-only",
            BuiltIn::Tester => "Run what others built: traffic, tests, scenarios, load runs, AI questions, the MCP inspector",
            BuiltIn::Editor => "Build mocks and tests: everything a tester does, plus mocks, saved tests, checkpoints, AI-built mocks, publishing and exports",
            BuiltIn::Admin => "Everything",
            BuiltIn::PortalReader => "Reads the API portal's closed items and opens nothing in the console",
        }
    }

    pub fn parse(name: &str) -> Option<BuiltIn> {
        Self::ALL.iter().copied().find(|r| r.name() == name)
    }

    /// The role's permissions.
    pub fn permissions(self) -> PermSet {
        use Permission as P;
        match self {
            // Every `*.read` except traffic, the audit log and the users.
            BuiltIn::Viewer => Permission::ALL
                .iter()
                .copied()
                .filter(|p| p.name().ends_with(".read"))
                .filter(|p| !matches!(p, P::TrafficRead | P::AuditRead | P::UsersRead))
                .collect(),
            BuiltIn::Tester => BuiltIn::Viewer.permissions().union(PermSet::of(&[
                P::TrafficRead,
                P::TestsRun,
                P::ScenariosRun,
                P::HarnessRead,
                P::HarnessRun,
                P::AssistUse,
                P::McpInspect,
            ])),
            BuiltIn::Editor => BuiltIn::Tester.permissions().union(PermSet::of(&[
                P::MocksRead,
                P::MocksWrite,
                P::MocksDelete,
                P::MocksData,
                P::TestsWrite,
                P::CheckpointsWrite,
                P::CheckpointsRestore,
                P::AssistJobs,
                P::PortalPublish,
                P::RegistryWrite,
                P::ConfigExport,
                P::GitRead,
                P::TrafficClear,
            ])),
            BuiltIn::Admin => PermSet::all(),
            BuiltIn::PortalReader => PermSet::of(&[P::PortalRead]),
        }
    }
}

/// Is a role with these permissions privileged (it can change who may do
/// what)?
pub fn is_privileged(perms: PermSet) -> bool {
    perms.has(Permission::UsersManage) || perms.has(Permission::SettingsSecurity)
}

/// A role an operator defined.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomRole {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Permission names (kept as written; validated on save).
    pub permissions: Vec<String>,
}

impl CustomRole {
    /// The permission set (names that are not permissions, which
    /// validation refuses, count for nothing).
    pub fn permission_set(&self) -> PermSet {
        self.permissions
            .iter()
            .filter_map(|n| Permission::parse(n))
            .collect()
    }
}

/// A role name: 1 to 40 characters, lower case letters, digits, `_` and
/// `-`, starting with a letter.
pub fn valid_role_name(name: &str) -> Result<(), String> {
    let n = name.chars().count();
    if n == 0 || n > MAX_ROLE_NAME_CHARS {
        return Err(format!(
            "a role name has 1 to {MAX_ROLE_NAME_CHARS} characters"
        ));
    }
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err("a role name starts with a lower case letter".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err("a role name has lower case letters, digits, _ and - only".into());
    }
    Ok(())
}

/// Check a custom role before it is saved: its name (not a built-in one),
/// description and permissions (each one known, at least one, no
/// repeats). Answers every problem.
pub fn validate_custom(role: &CustomRole) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    if let Err(e) = valid_role_name(&role.name) {
        problems.push(format!("name: {e}"));
    } else if BuiltIn::parse(&role.name).is_some() {
        problems.push(format!(
            "name: {} is a built-in role, which cannot be changed",
            role.name
        ));
    }
    if role.description.chars().count() > MAX_ROLE_DESCRIPTION_CHARS {
        problems.push(format!(
            "description: at most {MAX_ROLE_DESCRIPTION_CHARS} characters"
        ));
    }
    if role.description.chars().any(char::is_control) {
        problems.push("description: no control characters".into());
    }
    if role.permissions.is_empty() {
        problems.push("permissions: a role grants at least one permission".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    for name in &role.permissions {
        if Permission::parse(name).is_none() {
            problems.push(format!(
                "permissions: {} is not a permission",
                crate::logging::printable(name, 64)
            ));
        } else if !seen.insert(name.as_str()) {
            problems.push(format!("permissions: {name} is listed twice"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// A role as answers show it: built-in or custom, privileged or not.
pub fn view(name: &str, description: &str, perms: PermSet, built_in: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "built_in": built_in,
        "privileged": is_privileged(perms),
        "permissions": perms.names(),
    })
}
