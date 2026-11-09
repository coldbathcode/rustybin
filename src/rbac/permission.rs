//! The named permissions of the control plane: one enum, a stable dotted
//! name, a group and a one-line summary each (the console and the docs
//! render the table from [`Permission::ALL`]). A new feature adds its
//! permissions here, never a yes/no admin check.

use serde_json::{json, Value};

macro_rules! permissions {
    ($( $variant:ident => $name:literal, $group:ident, $summary:literal; )+) => {
        /// One thing a caller may do on the control plane.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(u8)]
        pub enum Permission {
            $( $variant, )+
        }

        impl Permission {
            /// Every permission, in table order (the order of the docs).
            pub const ALL: &'static [Permission] = &[ $( Permission::$variant, )+ ];

            /// The stable dotted name (`mocks.write`): what tokens, roles,
            /// documents and answers carry. Never renamed.
            pub const fn name(self) -> &'static str {
                match self { $( Permission::$variant => $name, )+ }
            }

            /// The group the tables list it under.
            pub const fn group(self) -> Group {
                match self { $( Permission::$variant => Group::$group, )+ }
            }

            /// One line on what it allows.
            pub const fn summary(self) -> &'static str {
                match self { $( Permission::$variant => $summary, )+ }
            }
        }
    };
}

permissions! {
    InstanceRead => "instance.read", Instance, "Status, catalogue, version, metrics and the instance's public views";
    SettingsRead => "settings.read", Instance, "Read the instance settings (secrets show only whether they are set), diagnostics, outbound decisions and usage";
    SettingsWrite => "settings.write", Instance, "Change the instance settings outside the security groups";
    SettingsSecurity => "settings.security", Instance, "Change the security groups: egress, the AI provider and base URL, proxy, agents, console and single sign-on";
    SecretsWrite => "secrets.write", Instance, "Set or clear stored secrets; read, fulfil and decline the secrets AI agents asked to have set";
    MocksRead => "mocks.read", Mocks, "List and read mocks, their revisions, reports, stateful data and checkpoints";
    MocksWrite => "mocks.write", Mocks, "Create, edit, deploy, roll back and turn mocks on or off";
    MocksDelete => "mocks.delete", Mocks, "Delete mocks";
    MocksData => "mocks.data", Mocks, "Reset a mock's stateful data and clear its request log";
    TrafficRead => "traffic.read", Traffic, "Captured requests, bins and mock traffic";
    TrafficClear => "traffic.clear", Traffic, "Clear everyone's captured requests";
    TestsRead => "tests.read", Testing, "Read saved tests, schedules and their run history";
    TestsWrite => "tests.write", Testing, "Save, change and delete saved tests and schedules";
    TestsRun => "tests.run", Testing, "Run saved tests";
    ScenariosRun => "scenarios.run", Testing, "Start, check and end gateway scenarios";
    CheckpointsWrite => "checkpoints.write", Testing, "Take and delete checkpoints";
    CheckpointsRestore => "checkpoints.restore", Testing, "Restore a checkpoint";
    HarnessRead => "harness.read", Load, "Read load test runs, reports and saved scenarios";
    HarnessRun => "harness.run", Load, "Start, change and stop load test runs";
    HarnessCluster => "harness.cluster", Load, "Manage the load test cluster";
    AssistUse => "assist.use", Ai, "Ask AI assist (explain, suggest, answer)";
    AssistJobs => "assist.jobs", Ai, "AI-built mocks: specs, examples and edits";
    McpInspect => "mcp.inspect", Mcp, "The MCP inspector, its relay and checks";
    McpConnections => "mcp.connections", Mcp, "Saved MCP connections and their credentials, check runs, calls that act on a server elsewhere";
    RegistryWrite => "registry.write", Mcp, "Publish and change mock MCP registry entries";
    PortalRead => "portal.read", Portal, "Read the closed items of the API portal";
    PortalPublish => "portal.publish", Portal, "Publish and unpublish portal items";
    PortalSettings => "portal.settings", Portal, "Change the portal's settings";
    ConfigExport => "config.export", AsCode, "Export the instance as rustybin.yaml";
    ConfigApply => "config.apply", AsCode, "Apply a rustybin.yaml document";
    GitRead => "git.read", AsCode, "Read the source control status and timeline";
    GitWrite => "git.write", AsCode, "Commit, push, pull and configure source control";
    AgentsRead => "agents.read", People, "List AI agents and what they did";
    AgentsManage => "agents.manage", People, "Change, revoke and restore AI agents";
    UsersRead => "users.read", People, "List users and roles";
    UsersManage => "users.manage", People, "Invite, change, disable and delete users; manage roles";
    AuditRead => "audit.read", People, "Read the audit log";
    VisitsOperate => "visits.operate", SelfService, "Operate self-service visits (pause, end, extend)";
}

/// The groups of the permission table, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    Instance,
    Mocks,
    Traffic,
    Testing,
    Load,
    Ai,
    Mcp,
    Portal,
    AsCode,
    People,
    SelfService,
}

impl Group {
    pub const ALL: &'static [Group] = &[
        Group::Instance,
        Group::Mocks,
        Group::Traffic,
        Group::Testing,
        Group::Load,
        Group::Ai,
        Group::Mcp,
        Group::Portal,
        Group::AsCode,
        Group::People,
        Group::SelfService,
    ];

    /// The heading of the group.
    pub const fn title(self) -> &'static str {
        match self {
            Group::Instance => "Instance",
            Group::Mocks => "Mocks",
            Group::Traffic => "Traffic",
            Group::Testing => "Testing",
            Group::Load => "Load",
            Group::Ai => "AI",
            Group::Mcp => "MCP",
            Group::Portal => "Portal",
            Group::AsCode => "As code",
            Group::People => "People",
            Group::SelfService => "Self-service",
        }
    }
}

impl Permission {
    /// The permission of a dotted name; `None` for an unknown one.
    pub fn parse(name: &str) -> Option<Permission> {
        Self::ALL.iter().copied().find(|p| p.name() == name)
    }

    /// The table as data: name, group, summary (for answers and docs).
    pub fn table() -> Value {
        Value::Array(
            Self::ALL
                .iter()
                .map(|p| {
                    json!({
                        "name": p.name(),
                        "group": p.group().title(),
                        "summary": p.summary(),
                    })
                })
                .collect(),
        )
    }

    const fn bit(self) -> u64 {
        1u64 << (self as u8)
    }
}

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A set of permissions (a bit set: cheap to copy, compare and join).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PermSet(u64);

// Every permission fits the bit set.
const _: () = assert!(Permission::ALL.len() <= 64);

impl PermSet {
    pub const EMPTY: PermSet = PermSet(0);

    /// Every permission: the admin role, the admin token, an open control
    /// plane.
    pub fn all() -> PermSet {
        Self::of(Permission::ALL)
    }

    pub fn of(perms: &[Permission]) -> PermSet {
        let mut s = PermSet::EMPTY;
        for p in perms {
            s.insert(*p);
        }
        s
    }

    pub fn insert(&mut self, p: Permission) {
        self.0 |= p.bit();
    }

    pub fn has(&self, p: Permission) -> bool {
        self.0 & p.bit() != 0
    }

    pub fn union(self, other: PermSet) -> PermSet {
        PermSet(self.0 | other.0)
    }

    /// Does this set hold every permission of `other`?
    pub fn contains(&self, other: PermSet) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    pub fn is_all(&self) -> bool {
        *self == Self::all()
    }

    pub fn len(&self) -> usize {
        self.0.count_ones() as usize
    }

    /// The permissions in table order.
    pub fn iter(&self) -> impl Iterator<Item = Permission> + '_ {
        Permission::ALL.iter().copied().filter(|p| self.has(*p))
    }

    /// The dotted names, in table order.
    pub fn names(&self) -> Vec<&'static str> {
        self.iter().map(Permission::name).collect()
    }
}

impl FromIterator<Permission> for PermSet {
    fn from_iter<I: IntoIterator<Item = Permission>>(iter: I) -> Self {
        let mut s = PermSet::EMPTY;
        for p in iter {
            s.insert(p);
        }
        s
    }
}
