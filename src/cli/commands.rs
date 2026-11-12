//! The command tree: every noun and verb of `rustybin`, the routes each one
//! calls (the parity test walks the catalogue against them), its flags and
//! an example, and `dispatch`, which runs one.

use reqwest::Method;
use serde_json::{json, Map, Value};
use std::time::Duration;

use super::client::{Body, Reply};
use super::{profiles, Ctx, Fail, Result};

/// A flag of a command.
#[derive(Clone, Copy, Debug)]
pub struct Flag {
    pub name: &'static str,
    /// Takes a value.
    pub value: bool,
    pub help: &'static str,
}

impl Flag {
    pub const fn value(name: &'static str, help: &'static str) -> Self {
        Self {
            name,
            value: true,
            help,
        }
    }

    pub const fn switch(name: &'static str, help: &'static str) -> Self {
        Self {
            name,
            value: false,
            help,
        }
    }
}

/// One command.
#[derive(Debug)]
pub struct Cmd {
    pub noun: &'static str,
    /// Empty when the noun is the command (`login`, `status`).
    pub verb: &'static str,
    /// The arguments after the verb, as the help shows them.
    pub usage: &'static str,
    pub summary: &'static str,
    pub flags: &'static [Flag],
    /// The routes it calls (method, catalogue path).
    pub routes: &'static [(&'static str, &'static str)],
    /// Asks before running (`--yes` skips it).
    pub destructive: bool,
    /// An example invocation (after `rustybin`).
    pub example: &'static str,
}

impl Cmd {
    /// `noun verb <args>` as the help shows it.
    pub fn line(&self) -> String {
        let mut s = self.noun.to_string();
        if !self.verb.is_empty() {
            s.push(' ');
            s.push_str(self.verb);
        }
        if !self.usage.is_empty() {
            s.push(' ');
            s.push_str(self.usage);
        }
        s
    }
}

/// The nouns in help order with their summaries.
pub const NOUNS: &[(&str, &str)] = &[
    (
        "login",
        "Sign in to an instance and keep its token in a profile",
    ),
    ("logout", "Revoke the profile's token and forget it"),
    ("whoami", "Who the current credential is"),
    ("status", "The instance's status"),
    ("doctor", "What is wrong with the instance, and the fix"),
    (
        "instance",
        "Version, readiness, the running configuration, the catalogue, metrics, usage",
    ),
    ("profiles", "The profiles this machine keeps"),
    (
        "settings",
        "Instance settings: get, set, secrets, unset, the schema",
    ),
    ("sessions", "The console's sign-in sessions"),
    (
        "password",
        "Change the first admin's user name and password (0.3; rustybin passwd changes yours)",
    ),
    (
        "users",
        "Users: invite, change roles, disable, delete (the password sign-in)",
    ),
    (
        "roles",
        "Roles: the built-in ones and custom roles with their permissions",
    ),
    (
        "projects",
        "Projects: roles on part of the instance (mocks, saved tests, checkpoints)",
    ),
    (
        "me",
        "Your own account: who you are, display name, email, sessions",
    ),
    ("passwd", "Change your own password"),
    (
        "sso",
        "Single sign-on with OpenID Connect: its status, and a test against the provider",
    ),
    (
        "mocks",
        "OpenAPI mocks on the instance (rustybin mock ... works offline)",
    ),
    ("visits", "Self-service visits, as the operator"),
    ("visit", "Your own self-service visit, as a visitor"),
    ("requests", "The request inspector"),
    ("bins", "Request bins"),
    (
        "checkpoint",
        "Checkpoints: the instance's API mocks and health, saved and restored",
    ),
    (
        "tests",
        "Saved tests: captured requests with assertions, run here, through a gateway or in CI",
    ),
    (
        "scenario",
        "Guided gateway scenarios: prepare the upstream, check through the gateway, clean up",
    ),
    (
        "storyboard",
        "Demo storyboards: a scripted demo with a talk track, rehearsed step by step",
    ),
    (
        "pack",
        "Customer demo packs: a demo's mocks, tests and storyboards applied and torn down as one",
    ),
    (
        "record",
        "Recordings of a real API: a proxy URL to send traffic through, then a mock built from it",
    ),
    ("agents", "AI agents on the management MCP server"),
    ("ai", "AI assist: status, models, usage, test, run a task"),
    (
        "mcp",
        "The MCP inspector: servers, saved connections, checks, calls through the relay",
    ),
    ("registry", "The mock MCP registry and registry browsing"),
    ("portal", "The dev portal"),
    ("config", "The whole instance as rustybin.yaml"),
    ("git", "Source control"),
    (
        "harness",
        "Load tests on the instance (--remote); validate, run and report work offline too",
    ),
    ("keys", "API keys in shared mode"),
    ("egress", "The outbound connection policy"),
    (
        "audit",
        "The audit log: who changed what, every sign-in and every refusal",
    ),
];

const NO_FLAGS: &[Flag] = &[];
const USER_FLAGS_INVITE: &[Flag] = &[
    Flag::value(
        "--role",
        "A role (repeatable, or comma separated; rustybin roles list shows them)",
    ),
    Flag::value("--display-name", "The name the console shows"),
    Flag::value("--email", "An email address (never logged)"),
    Flag::switch(
        "--reset",
        "A new link for a user who exists (a password reset)",
    ),
    Flag::switch(
        "--sso",
        "A single sign-on user, added ahead of their first sign-in: no link, no password (--role pins roles)",
    ),
    Flag::value(
        "--project",
        "Roles in a project: PROJECT=role+role (repeatable; with one, --role may be left out)",
    ),
];
const PROJECT_FLAG: Flag = Flag::value(
    "--project",
    "A project's only (instance: the instance's, without a project)",
);
const PROJECT_FLAGS: &[Flag] = &[PROJECT_FLAG];
const USER_FLAGS_UPDATE: &[Flag] = &[
    Flag::value(
        "--role",
        "The user's roles (repeatable, or comma separated; replaces them)",
    ),
    Flag::value("--display-name", "The name the console shows"),
    Flag::value("--email", "An email address (\"\" clears it)"),
    Flag::value(
        "--pin-role",
        "A single sign-on user's pinned roles, on top of their groups' (repeatable, or comma separated; replaces them)",
    ),
];
const TAIL_FLAGS: &[Flag] = &[
    Flag::value("--max", "Stop after this many events"),
    Flag::value("--timeout", "Stop after this many seconds (default 3600)"),
];
const AUDIT_FILTERS: [Flag; 6] = [
    Flag::value(
        "--actor",
        "Contained in the actor's kind, user id, name or agent",
    ),
    Flag::value(
        "--action",
        "Contained in the action (route or tool) or the event (login, invite, sso)",
    ),
    Flag::value(
        "--target",
        "Contained in a target's name; kind:name for one kind (mock:orders)",
    ),
    Flag::value(
        "--since",
        "RFC 3339, a UTC day (2026-11-03) or an age (15m, 2h, 7d)",
    ),
    Flag::value("--until", "RFC 3339, a UTC day or an age"),
    Flag::value(
        "--result",
        "ok, error, refused, asked, or an HTTP status code",
    ),
];
const AUDIT_LIST_FLAGS: &[Flag] = &[
    AUDIT_FILTERS[0],
    AUDIT_FILTERS[1],
    AUDIT_FILTERS[2],
    AUDIT_FILTERS[3],
    AUDIT_FILTERS[4],
    AUDIT_FILTERS[5],
    Flag::value("--limit", "At most this many (1 to 500, default 100)"),
    Flag::value(
        "--cursor",
        "The next_cursor of the previous page (older entries)",
    ),
];
const AUDIT_TAIL_FLAGS: &[Flag] = &[
    AUDIT_FILTERS[0],
    AUDIT_FILTERS[1],
    AUDIT_FILTERS[2],
    AUDIT_FILTERS[5],
    Flag::value("--max", "Stop after this many events"),
    Flag::value("--timeout", "Stop after this many seconds (default 3600)"),
];
const RESOLVE_FLAGS: &[Flag] = &[
    Flag::value("--with", "instance, repository or pull-request"),
    Flag::value(
        "--message",
        "The commit message when the instance's side is committed",
    ),
    Flag::value(
        "--branch",
        "pull-request: the new branch on the remote (a name is made without it)",
    ),
    Flag::switch(
        "--dry-run",
        "What the way out would change; nothing changes",
    ),
];
const SWITCH_FLAGS: &[Flag] = &[
    Flag::switch(
        "--dry-run",
        "What applying the branch's document would change; nothing changes",
    ),
    Flag::switch("--prune", "The branch's document is the whole truth"),
    Flag::switch("--no-apply", "Move the branch only, apply nothing"),
];
const BRANCH_FLAGS: &[Flag] = &[
    Flag::value("--message", "The first commit's message (also -m)"),
    Flag::value("-m", "The first commit's message"),
    Flag::switch(
        "--dry-run",
        "What the first commit would record; nothing changes",
    ),
];
const COMMIT_FLAGS: &[Flag] = &[
    Flag::value("--message", "The commit message (also -m)"),
    Flag::value("-m", "The commit message"),
    Flag::value(
        "--checkpoint",
        "A local checkpoint to commit next to rustybin.yaml (repeatable, or comma separated)",
    ),
];

macro_rules! cmd {
    ($noun:expr, $verb:expr, $usage:expr, $summary:expr, $flags:expr, [$(($m:expr, $p:expr)),* $(,)?], $destructive:expr, $example:expr) => {
        Cmd {
            noun: $noun,
            verb: $verb,
            usage: $usage,
            summary: $summary,
            flags: $flags,
            routes: &[$(($m, $p)),*],
            destructive: $destructive,
            example: $example,
        }
    };
}

/// The command tree.
pub static COMMANDS: &[Cmd] = &[
    // ── Sign-in ──
    cmd!("login", "", "[--url U] [--user U] [--password-stdin] [--sso | --agent [--scope S]] [--no-browser] [--wait S] [--profile P]", "Sign in with the console user name and password (prompted, or the first line of stdin) and keep the token in a profile; --sso signs in with the identity provider in the browser instead (a loopback address on this machine takes the answer); --token keeps a token you already have. --agent signs an AI agent in instead: the browser opens the console's consent screen, you approve, and the agent's tokens are kept in the profile for rustybin mcp-stdio. An agent sign-in works with rustybin mcp-stdio only (the instance accepts its tokens on /_rustybin/mcp and nowhere else): the other commands need a sign-in of your own.",
        &[Flag::value("--user", "The console user name (default rustybin)"), Flag::switch("--password-stdin", "Read the password from the first line of stdin"), Flag::switch("--sso", "Single sign-on: the browser signs in at the identity provider"), Flag::switch("--no-browser", "With --sso or --agent: print the address to open instead of opening it"), Flag::value("--wait", "With --sso or --agent: how long to wait for the browser, in seconds (default 300)"), Flag::switch("--agent", "Sign an AI agent in for rustybin mcp-stdio: you approve it on the console's consent screen"), Flag::value("--scope", "With --agent: the most the agent may be given (admin, console, inspector, harness; default: what you choose on the consent screen)")],
        [("POST", "/_rustybin/login"), ("GET", "/_rustybin/session"), ("GET", "/_rustybin/sso/status"), ("GET", "/_rustybin/sso/start"), ("POST", "/_rustybin/sso/token"), ("POST", "/_rustybin/oauth/register"), ("GET", "/_rustybin/oauth/authorize"), ("POST", "/_rustybin/oauth/token")], false, "login --url https://mocks.example.com --user rustybin"),
    cmd!("logout", "", "", "Revoke the profile's token on the instance and forget it, with the profile's agent sign-in (rustybin agents revoke ends an agent on the instance).", NO_FLAGS, [("POST", "/_rustybin/logout")], false, "logout"),
    cmd!("whoami", "", "", "The credential the calls carry: user, scopes, expiry.", NO_FLAGS, [("GET", "/_rustybin/session")], false, "whoami"),
    cmd!("status", "", "", "Name, version, modes, health, the features that are on.", NO_FLAGS, [("GET", "/_rustybin/status")], false, "status"),
    cmd!("doctor", "", "", "What is wrong with the instance and how to fix it; exits 1 when a check fails.", NO_FLAGS, [("GET", "/_rustybin/diagnostics")], false, "doctor"),
    // ── Instance ──
    cmd!("instance", "version", "", "Version and build.", NO_FLAGS, [("GET", "/_rustybin/version")], false, "instance version"),
    cmd!("instance", "ready", "", "Readiness (never authenticated).", NO_FLAGS, [("GET", "/_rustybin/ready")], false, "instance ready"),
    cmd!("instance", "config", "", "The running configuration (no secrets).", NO_FLAGS, [("GET", "/_rustybin/config")], false, "instance config"),
    cmd!("instance", "catalog", "[--category C]", "The route catalogue.", &[Flag::value("--category", "Only this category")], [("GET", "/_rustybin/catalog")], false, "instance catalog --category \"Mock APIs (OpenAPI)\""),
    cmd!("instance", "metrics", "", "Prometheus metrics, as text.", NO_FLAGS, [("GET", "/_rustybin/metrics")], false, "instance metrics"),
    cmd!("instance", "usage", "", "Plan limits and this client's usage.", NO_FLAGS, [("GET", "/_rustybin/usage")], false, "instance usage"),
    // ── Profiles ──
    cmd!("profiles", "list", "", "The profiles and the default.", NO_FLAGS, [], false, "profiles list"),
    cmd!("profiles", "use", "<name>", "Make a profile the default.", NO_FLAGS, [], false, "profiles use mocks.example.com"),
    cmd!("profiles", "remove", "<name>", "Forget a profile.", NO_FLAGS, [], false, "profiles remove old-instance"),
    // ── Settings ──
    cmd!("settings", "get", "[key]", "Every setting with its value and source, or one.", NO_FLAGS, [("GET", "/_rustybin/settings")], false, "settings get console.title"),
    cmd!("settings", "set", "<key> <value>", "Save a setting (JSON values are parsed, anything else is a string).", NO_FLAGS, [("PATCH", "/_rustybin/settings")], false, "settings set console.title \"Payments mocks\""),
    cmd!("settings", "set-secret", "<key> [--stdin]", "Save a secret (prompted without echo, or the first line of stdin).", &[Flag::switch("--stdin", "Read the secret from the first line of stdin")], [("PATCH", "/_rustybin/settings")], false, "settings set-secret assist.openai_key --stdin < key.txt"),
    cmd!("settings", "unset", "<key>", "Forget the stored value (back to the environment or the default).", NO_FLAGS, [("DELETE", "/_rustybin/settings/{key}")], false, "settings unset console.title"),
    cmd!("settings", "schema", "", "Every registered setting: key, kind, default, variable.", NO_FLAGS, [("GET", "/_rustybin/settings")], false, "settings schema"),
    // ── Sessions and password ──
    cmd!("sessions", "list", "", "The sign-in sessions since the last restart.", NO_FLAGS, [("GET", "/_rustybin/sessions")], false, "sessions list"),
    cmd!("sessions", "end", "<id>", "End one session.", NO_FLAGS, [("DELETE", "/_rustybin/sessions/{id}")], false, "sessions end 3f2a"),
    cmd!("sessions", "end-all", "", "Sign out everywhere (every token issued so far).", NO_FLAGS, [("DELETE", "/_rustybin/sessions")], true, "sessions end-all --yes"),
    cmd!("password", "change", "[--user U] [--stdin]", "Change your password (the first admin's also takes a new user name): the current and the new one prompted, or two lines of stdin.", &[Flag::value("--user", "A new user name (the first admin only)"), Flag::switch("--stdin", "Read the current and the new password from two lines of stdin")], [("PUT", "/_rustybin/password")], false, "password change --stdin < passwords.txt"),
    cmd!("password", "hash", "[--stdin]", "Hash a password for RUSTYBIN_CONSOLE_PASSWORD_HASH, offline: prompted, or the first line of stdin. Prints the argon2id hash and nothing else.", &[Flag::switch("--stdin", "Read the password from the first line of stdin")], [], false, "password hash --stdin"),
    // ── Users, roles, your account ──
    cmd!("users", "list", "", "The users: roles, status (active, invited, disabled), last sign-in.", NO_FLAGS, [("GET", "/_rustybin/users")], false, "users list"),
    cmd!("users", "invite", "<name> --role R [--display-name D] [--email E] [--reset] [--sso]", "Invite a user: the answer is a link to send them (shown once), where they set their own password. --reset sends a user who exists a new link (a password reset); --sso adds a single sign-on user instead (no link).", USER_FLAGS_INVITE, [("POST", "/_rustybin/users"), ("POST", "/_rustybin/users/{id}/invite")], false, "users invite alice --role editor --display-name \"Alice Doe\""),
    cmd!("users", "show", "<user>", "One user (an id or a sign-in name): roles, permissions, status, the open invitation.", NO_FLAGS, [("GET", "/_rustybin/users/{id}")], false, "users show alice"),
    cmd!("users", "update", "<user> [--role R] [--pin-role R] [--display-name D] [--email E]", "Change a user's roles (--role replaces them; repeatable), a single sign-on user's pinned roles (--pin-role), display name or email (--email \"\" clears it).", USER_FLAGS_UPDATE, [("PATCH", "/_rustybin/users/{id}")], false, "users update alice --role tester"),
    cmd!("users", "disable", "<user>", "Disable a user: their sessions end and their sign-in is refused.", NO_FLAGS, [("PATCH", "/_rustybin/users/{id}")], true, "users disable alice --yes"),
    cmd!("users", "enable", "<user>", "Enable a disabled user again (an invited one joins through the invitation).", NO_FLAGS, [("PATCH", "/_rustybin/users/{id}")], false, "users enable alice"),
    cmd!("users", "sessions", "<user> [--end]", "A user's sign-in sessions; --end signs them out everywhere.", &[Flag::switch("--end", "Sign the user out everywhere")], [("GET", "/_rustybin/users/{id}/sessions"), ("DELETE", "/_rustybin/users/{id}/sessions")], false, "users sessions alice"),
    cmd!("users", "delete", "<user>", "Delete a user (never the last enabled admin, never the first admin).", NO_FLAGS, [("DELETE", "/_rustybin/users/{id}")], true, "users delete alice --yes"),
    cmd!("roles", "list", "", "The roles with their permissions, whether they are privileged and how many users hold them.", NO_FLAGS, [("GET", "/_rustybin/roles")], false, "roles list"),
    cmd!("roles", "show", "<name>", "One role: its permissions and who holds it.", NO_FLAGS, [("GET", "/_rustybin/roles/{name}")], false, "roles show editor"),
    cmd!("roles", "save", "<name> --permission P [--description D]", "Create or change a custom role (--permission repeatable, or comma separated).", &[Flag::value("--permission", "A permission (repeatable, or comma separated; rustybin roles list shows them)"), Flag::value("--description", "What the role is for")], [("PUT", "/_rustybin/roles/{name}"), ("POST", "/_rustybin/roles")], false, "roles save catalog-reader --permission mocks.read,traffic.read --description \"Reads mocks and traffic\""),
    cmd!("roles", "delete", "<name>", "Delete a custom role nobody holds.", NO_FLAGS, [("DELETE", "/_rustybin/roles/{name}")], true, "roles delete catalog-reader --yes"),
    cmd!("projects", "list", "", "The projects you may see, your roles in each and what each holds.", NO_FLAGS, [("GET", "/_rustybin/projects")], false, "projects list"),
    cmd!("projects", "show", "<name>", "One project: its members (with users.read), your roles, its mocks, saved tests and checkpoints.", NO_FLAGS, [("GET", "/_rustybin/projects/{name}")], false, "projects show payments"),
    cmd!("projects", "save", "<name> [--description D] [--member USER=role+role]...", "Create or change a project; --member (repeatable) replaces the members.", &[Flag::value("--description", "What the project is for"), Flag::value("--member", "A member: USER=role+role (repeatable; the whole list)"), Flag::switch("--no-members", "Remove every member")], [("PUT", "/_rustybin/projects/{name}")], false, "projects save payments --description \"The payments team\""),
    cmd!("projects", "delete", "<name> [--move-to P]", "Delete a project; one that holds resources needs --move-to (another project, or instance).", &[Flag::value("--move-to", "Where its mocks, saved tests and checkpoints go: a project, or instance")], [("DELETE", "/_rustybin/projects/{name}")], true, "projects delete payments --move-to instance --yes"),
    cmd!("me", "", "", "Your own account: user, display name, email, roles, permissions.", NO_FLAGS, [("GET", "/_rustybin/me")], false, "me"),
    cmd!("me", "update", "[--display-name D] [--email E]", "Change your display name or email (--email \"\" clears it).", &[Flag::value("--display-name", "Your display name"), Flag::value("--email", "Your email address")], [("PATCH", "/_rustybin/me")], false, "me update --display-name \"Ops on call\""),
    cmd!("me", "sessions", "[--end-all]", "Your sign-in sessions; --end-all signs you out everywhere (this profile too).", &[Flag::switch("--end-all", "Sign yourself out everywhere")], [("GET", "/_rustybin/me/sessions"), ("DELETE", "/_rustybin/me/sessions")], false, "me sessions"),
    cmd!("me", "end-session", "<id>", "End one of your sign-in sessions.", NO_FLAGS, [("DELETE", "/_rustybin/me/sessions/{id}")], false, "me end-session 3f2a"),
    cmd!("passwd", "", "[--stdin]", "Change your own password: the current and the new one prompted, or two lines of stdin. Your other sessions end; the profile keeps a fresh token.", &[Flag::switch("--stdin", "Read the current and the new password from two lines of stdin")], [("PUT", "/_rustybin/me/password")], false, "passwd --stdin < passwords.txt"),
    // ── Mocks ──
    cmd!("mocks", "list", "[--all] [--project P]", "The mocks with their state and traffic.", &[Flag::switch("--all", "Every visitor's mocks too (admin)"), PROJECT_FLAG], [("GET", "/_rustybin/mocks")], false, "mocks list"),
    cmd!("mocks", "get", "<name>", "One mock: settings, operations, the deploy report.", NO_FLAGS, [("GET", "/_rustybin/mocks/{name}")], false, "mocks get petstore"),
    cmd!("mocks", "create", "<name> (--spec FILE | --spec-url URL) [--project P]", "Create a mock from an OpenAPI file (YAML or JSON) or an AsyncAPI one (an event mock with channels), or from a URL the instance fetches itself.", &[Flag::value("--spec", "The OpenAPI or AsyncAPI document"), Flag::value("--spec-url", "Where the instance fetches it (RUSTYBIN_EGRESS_ALLOW, purpose spec_fetch)"), Flag::value("--mode", "examples, generated, static or stateful"), Flag::switch("--disabled", "Create it switched off"), Flag::switch("--no-deploy", "Keep it as a draft"), Flag::switch("--strip-server-prefix", "Serve the paths without the spec's server prefix"), Flag::value("--project", "The project it belongs to (default: the instance's, or your one project)")], [("POST", "/_rustybin/mocks")], false, "mocks create orders --spec orders.yaml --mode generated"),
    cmd!("mocks", "move", "<name> --project P", "Move a mock to another project (instance: the instance's); needs mocks.write in both.", &[Flag::value("--project", "The project it goes to, or instance")], [("PATCH", "/_rustybin/mocks/{name}")], false, "mocks move orders --project instance"),
    cmd!("mocks", "import", "--spec FILE", "Preview a spec: title, operations, the checks, the suggested name.", &[Flag::value("--spec", "The OpenAPI document")], [("POST", "/_rustybin/mocks/import")], false, "mocks import --spec orders.yaml"),
    cmd!("mocks", "update", "<name> [--spec FILE] [--mode M] [--settings JSON]", "Change a mock's spec or settings (the draft, deployed unless --no-deploy).", &[Flag::value("--spec", "A new OpenAPI document"), Flag::value("--mode", "examples, generated, static or stateful"), Flag::value("--settings", "Settings as JSON or a file: responses, security, operations, mcp, strip_server_prefix"), Flag::switch("--no-deploy", "Change the draft only"), Flag::switch("--force", "Deploy even when the checks report errors"), Flag::value("--expect-revision", "Refuse (409) unless the draft is still this revision (default: the one read just before)")], [("PATCH", "/_rustybin/mocks/{name}")], false, "mocks update orders --mode stateful"),
    cmd!("mocks", "deploy", "<name> [--force] [--expect-revision N]", "Deploy the draft.", &[Flag::switch("--force", "Deploy despite check errors"), Flag::value("--expect-revision", "Refuse (409) unless the draft is still this revision (default: deploy the draft as it is)")], [("POST", "/_rustybin/mocks/{name}/deploy")], false, "mocks deploy orders"),
    cmd!("mocks", "rollback", "<name> [--revision N]", "Go back to the previous (or a given) revision.", &[Flag::value("--revision", "The revision to restore")], [("POST", "/_rustybin/mocks/{name}/rollback")], false, "mocks rollback orders"),
    cmd!("mocks", "enable", "<name>", "Switch a mock on.", NO_FLAGS, [("POST", "/_rustybin/mocks/{name}/enable")], false, "mocks enable orders"),
    cmd!("mocks", "disable", "<name>", "Switch a mock off.", NO_FLAGS, [("POST", "/_rustybin/mocks/{name}/disable")], false, "mocks disable orders"),
    cmd!("mocks", "delete", "<name>", "Delete a mock.", NO_FLAGS, [("DELETE", "/_rustybin/mocks/{name}")], true, "mocks delete orders --yes"),
    cmd!("mocks", "duplicate", "<name> [--name NEW]", "Copy a mock.", &[Flag::value("--name", "The copy's name (default <name>-copy)")], [("POST", "/_rustybin/mocks/{name}/duplicate")], false, "mocks duplicate orders --name orders-v2"),
    cmd!("mocks", "check", "<name>", "The deploy report: a valid request and response per operation.", NO_FLAGS, [("GET", "/_rustybin/mocks/{name}/report")], false, "mocks check orders"),
    cmd!("mocks", "coverage", "<name>", "Which operations lack an example.", NO_FLAGS, [("GET", "/_rustybin/mocks/{name}/coverage")], false, "mocks coverage orders"),
    cmd!("mocks", "drift", "<name> [--upstream URL] [--header 'K: V']... [--operation ID]... [--junit]", "Check the mock against the real API it imitates (its drift.upstream, or --upstream): one request per operation, where the answers disagree with the spec; exits 1 on drift. A header value may hold {{VAR}}, filled from this shell's RUSTYBIN_TEST_<VAR>.", &[Flag::value("--upstream", "The real API's base URL"), Flag::value("--header", "A header for the real API, 'Name: value' (repeatable)"), Flag::value("--operation", "Only this operationId (repeatable)"), Flag::switch("--junit", "Print a JUnit report")], [("POST", "/_rustybin/mocks/{name}/drift")], false, "mocks drift orders --upstream https://api.example.com/v1"),
    cmd!("mocks", "drift-runs", "<name>", "The mock's kept drift checks, newest first.", NO_FLAGS, [("GET", "/_rustybin/mocks/{name}/drift")], false, "mocks drift-runs orders"),
    cmd!("mocks", "channels", "<name>", "An AsyncAPI mock's channels: what each sends and receives, its pattern, its URL and who is connected.", NO_FLAGS, [("GET", "/_rustybin/mocks/{name}/channels")], false, "mocks channels orders"),
    cmd!("mocks", "events", "<name> <channel> [--limit N] [--follow]", "The messages published and received on a channel of an AsyncAPI mock, newest first; --follow keeps printing new ones (--max N, --timeout SECONDS).", &[Flag::value("--limit", "How many to list (default 20)"), Flag::switch("--follow", "Keep printing new messages"), Flag::value("--max", "Stop following after this many messages"), Flag::value("--timeout", "Stop following after this many seconds")], [("GET", "/_rustybin/mocks/{name}/channels/{channel}/messages")], false, "mocks events orders orders-created --limit 5"),
    cmd!("mocks", "publish", "<name> <channel> [--data JSON] [--count N]", "Make an AsyncAPI mock publish now, to every subscriber of the channel: its next message, --count of them, or --data as it is.", &[Flag::value("--data", "The payload: a file or inline JSON (default: the channel's next message)"), Flag::value("--count", "How many of the next messages (1 to 20)")], [("POST", "/_rustybin/mocks/{name}/channels/{channel}/publish")], false, "mocks publish orders orders-created"),
    cmd!("mocks", "subscribers", "<name>", "The webhook subscribers of an AsyncAPI mock.", NO_FLAGS, [("GET", "/_rustybin/mocks/{name}/subscribers")], false, "mocks subscribers orders"),
    cmd!("mocks", "subscribe", "<name> <channel> --webhook URL [--ttl SECONDS]", "Post every message of a channel to a URL; its host must be in RUSTYBIN_EGRESS_ALLOW (purpose events).", &[Flag::value("--webhook", "Where the messages are posted"), Flag::value("--ttl", "Seconds the subscriber lives (default 3600)")], [("POST", "/_rustybin/mocks/{name}/subscribers")], false, "mocks subscribe orders orders-created --webhook https://hooks.example.com/orders"),
    cmd!("mocks", "unsubscribe", "<name> <id>", "Remove a webhook subscriber.", NO_FLAGS, [("DELETE", "/_rustybin/mocks/{name}/subscribers/{id}")], false, "mocks unsubscribe orders 0a1b2c3d4e5f6a7b"),
    cmd!("mocks", "spec", "<name>", "The mock's OpenAPI document as stored.", NO_FLAGS, [("GET", "/_rustybin/mocks/{name}/spec")], false, "mocks spec orders > orders.yaml"),
    cmd!("mocks", "revision", "<name> <revision>", "A kept revision: what changed since in the draft, its settings and spec (the whole answer with --json).", &[Flag::switch("--spec", "Print only the revision's spec as uploaded")], [("GET", "/_rustybin/mocks/{name}/revisions/{revision}")], false, "mocks revision orders 12"),
    cmd!("mocks", "export", "[name]", "One mock, or all, as a rustybin.yaml with only the mocks section.", NO_FLAGS, [("GET", "/_rustybin/mocks/{name}/export"), ("GET", "/_rustybin/mocks/export")], false, "mocks export orders"),
    cmd!("mocks", "apply", "<file> [--prune]", "Apply a rustybin.yaml (spec files are read locally and inlined); --prune deletes API mocks it does not name.", &[Flag::switch("--prune", "Delete API-created mocks the file does not name")], [("POST", "/_rustybin/mocks/apply")], false, "mocks apply rustybin.yaml --prune --yes"),
    cmd!("mocks", "reload", "", "Reload the file-defined mocks (RUSTYBIN_CONFIG_FILE, RUSTYBIN_MOCKS_DIR).", NO_FLAGS, [("POST", "/_rustybin/mocks/reload")], false, "mocks reload"),
    cmd!("mocks", "schema", "", "The JSON Schema of rustybin.yaml.", NO_FLAGS, [("GET", "/_rustybin/mocks/config.schema.json")], false, "mocks schema"),
    cmd!("mocks", "sample", "<name> <operationId>", "A valid response for an operation.", &[Flag::value("--status", "The response status"), Flag::value("--seed", "The generator seed"), Flag::value("--mode", "examples or generated"), Flag::value("--example", "A named example")], [("GET", "/_rustybin/mocks/{name}/operations/{op}/sample")], false, "mocks sample orders listOrders --status 200"),
    cmd!("mocks", "validate", "<name> <operationId> --body FILE", "Validate a request or response body against the spec.", &[Flag::value("--body", "The body: a file or inline JSON"), Flag::value("--kind", "request or response (default)"), Flag::value("--status", "The response status the body is for")], [("POST", "/_rustybin/mocks/{name}/operations/{op}/validate")], false, "mocks validate orders createOrder --body order.json --kind request"),
    cmd!("mocks", "operation", "<name> <operationId>", "One operation's settings and statistics.", NO_FLAGS, [("GET", "/_rustybin/mocks/{name}/operations/{op}")], false, "mocks operation orders listOrders"),
    cmd!("mocks", "set-operation", "<name> <operationId> --settings JSON", "Set an operation's settings (respond, rules, mode, delay_ms, public).", &[Flag::value("--settings", "The settings as JSON or a file"), Flag::value("--expect-revision", "Refuse (409) unless the draft is still this revision (default: the one read just before)")], [("PUT", "/_rustybin/mocks/{name}/operations/{op}")], false, "mocks set-operation orders listOrders --settings '{\"delay_ms\": 200}'"),
    cmd!("mocks", "test-rules", "<name> <operationId> [--rules FILE] [--request N]", "Try an operation's rules (or given ones) on a captured request.", &[Flag::value("--rules", "Rules as JSON or a file (default: the draft's)"), Flag::value("--request", "The captured request number (default: the latest)")], [("POST", "/_rustybin/mocks/{name}/operations/{op}/rules/test")], false, "mocks test-rules orders getOrder"),
    cmd!("mocks", "traffic", "<name> [--limit N]", "The mock's captured requests.", &[Flag::value("--limit", "At most this many")], [("GET", "/_rustybin/mocks/{name}/requests")], false, "mocks traffic orders --limit 20"),
    cmd!("mocks", "traffic-get", "<name> <n>", "One captured request of the mock.", NO_FLAGS, [("GET", "/_rustybin/mocks/{name}/requests/{n}")], false, "mocks traffic-get orders 7"),
    cmd!("mocks", "traffic-tail", "<name>", "Follow the mock's requests as they arrive.", TAIL_FLAGS, [("GET", "/_rustybin/mocks/{name}/requests/stream")], false, "mocks traffic-tail orders --max 10"),
    cmd!("mocks", "traffic-clear", "<name>", "Forget the mock's captured requests.", NO_FLAGS, [("DELETE", "/_rustybin/mocks/{name}/requests")], true, "mocks traffic-clear orders --yes"),
    cmd!("mocks", "data", "<name>", "The items a stateful mock stored.", NO_FLAGS, [("GET", "/_rustybin/mocks/{name}/state")], false, "mocks data orders"),
    cmd!("mocks", "data-clear", "<name>", "Forget the items a stateful mock stored.", NO_FLAGS, [("DELETE", "/_rustybin/mocks/{name}/state")], true, "mocks data-clear orders --yes"),
    cmd!("mocks", "collection", "<name> <format>", "The mock as a postman, hurl, curl, http, k6, bruno or insomnia collection.", &[Flag::value("--base-url", "The base URL the collection calls")], [("GET", "/_rustybin/mocks/{name}/collections/{format}")], false, "mocks collection orders hurl > orders.hurl"),
    cmd!("mocks", "describe", "\"<description>\"", "An OpenAPI spec from a description (AI), printed as JSON.", &[Flag::value("--openapi-version", "3.0 or 3.1"), Flag::value("--model", "A model id instead of the task's default")], [("POST", "/_rustybin/mocks/ai/spec")], false, "mocks describe \"a parcel tracking API with shipments and events\""),
    cmd!("mocks", "examples", "<name> [--apply]", "Example payloads for the operations that lack one (AI).", &[Flag::value("--operations", "Only these operationIds, comma separated"), Flag::value("--per-response", "Examples per response"), Flag::value("--persona", "A persona for the data"), Flag::value("--source", "ai (default) or generated"), Flag::switch("--apply", "Store the examples on the mock")], [("POST", "/_rustybin/mocks/{name}/ai/examples")], false, "mocks examples orders --apply"),
    cmd!("mocks", "edit", "<name> \"<instruction>\" [--apply]", "Edit a spec with an instruction (AI): the diff, applied with --apply.", &[Flag::switch("--apply", "Apply the edit to the draft"), Flag::value("--model", "A model id instead of the task's default")], [("POST", "/_rustybin/mocks/{name}/ai/edit")], false, "mocks edit orders \"add a DELETE /orders/{id} operation\" --apply"),
    // ── Visits ──
    cmd!("visits", "list", "", "Every self-service visit.", NO_FLAGS, [("GET", "/_rustybin/mocks/visits")], false, "visits list"),
    cmd!("visits", "end", "<id>", "End a visit (its mocks go).", NO_FLAGS, [("DELETE", "/_rustybin/mocks/visits/{id}")], true, "visits end 9c1e --yes"),
    cmd!("visits", "end-all", "", "End every visit.", NO_FLAGS, [("DELETE", "/_rustybin/mocks/visits")], true, "visits end-all --yes"),
    cmd!("visits", "pause", "", "Stop accepting new visits.", NO_FLAGS, [("POST", "/_rustybin/mocks/visits/pause")], false, "visits pause"),
    cmd!("visits", "resume", "", "Accept new visits again.", NO_FLAGS, [("POST", "/_rustybin/mocks/visits/resume")], false, "visits resume"),
    cmd!("visit", "get", "", "Your visit (needs --mock-token or RUSTYBIN_MOCK_TOKEN).", NO_FLAGS, [("GET", "/_rustybin/mocks/visit")], false, "visit get --mock-token rbm_..."),
    cmd!("visit", "start", "[--email E]", "Start a visit; the manage token is shown once.", &[Flag::value("--email", "An email for the longer visit")], [("POST", "/_rustybin/mocks/visit")], false, "visit start"),
    cmd!("visit", "extend", "", "Extend your visit.", NO_FLAGS, [("POST", "/_rustybin/mocks/visit/extend")], false, "visit extend --mock-token rbm_..."),
    cmd!("visit", "rotate-token", "", "A new manage token for your visit.", NO_FLAGS, [("POST", "/_rustybin/mocks/visit/token")], false, "visit rotate-token --mock-token rbm_..."),
    cmd!("visit", "end", "", "End your visit.", NO_FLAGS, [("DELETE", "/_rustybin/mocks/visit")], true, "visit end --mock-token rbm_... --yes"),
    // ── Requests and bins ──
    cmd!("requests", "list", "[--limit N] [--path-prefix P]", "The captured requests.", &[Flag::value("--limit", "At most this many"), Flag::value("--path-prefix", "Only paths under this prefix")], [("GET", "/_rustybin/requests")], false, "requests list --limit 20"),
    cmd!("requests", "get", "<id>", "One captured request.", NO_FLAGS, [("GET", "/_rustybin/requests/{id}")], false, "requests get 42"),
    cmd!("requests", "tail", "", "Follow the requests as they arrive.", TAIL_FLAGS, [("GET", "/_rustybin/requests/stream")], false, "requests tail --max 5"),
    cmd!("requests", "clear", "", "Forget the captured requests.", NO_FLAGS, [("DELETE", "/_rustybin/requests")], true, "requests clear --yes"),
    cmd!("record", "list", "", "Your recordings of real APIs: upstream, proxy URL, state, counts.", NO_FLAGS, [("GET", "/_rustybin/recordings")], false, "record list"),
    cmd!("record", "start", "<name> --upstream URL [--mock NAME]", "Start a recording: send requests to the proxy URL it answers (as you would to the upstream); the upstream's host must be in RUSTYBIN_EGRESS_ALLOW (purpose record).", &[Flag::value("--upstream", "The real API's base URL"), Flag::value("--mock", "The mock it becomes (default: the recording's name)")], [("POST", "/_rustybin/recordings")], false, "record start orders --upstream https://api.example.com/v1"),
    cmd!("record", "show", "<name>", "A recording with what it kept (credentials removed).", NO_FLAGS, [("GET", "/_rustybin/recordings/{name}")], false, "record show orders"),
    cmd!("record", "stop", "<name>", "Stop recording: the proxy refuses from now on.", NO_FLAGS, [("POST", "/_rustybin/recordings/{name}/stop")], false, "record stop orders"),
    cmd!("record", "build", "<name> [--mock NAME] [--dry-run]", "Build a mock from a recording: an inferred spec, the recorded answers as examples; --dry-run prints the spec.", &[Flag::value("--mock", "Another mock name than the recording's"), Flag::switch("--dry-run", "Print the inferred spec, change nothing")], [("POST", "/_rustybin/recordings/{name}/build")], false, "record build orders --dry-run"),
    cmd!("record", "delete", "<name>", "Delete a recording and what it kept (the mock it built stays).", NO_FLAGS, [("DELETE", "/_rustybin/recordings/{name}")], true, "record delete orders --yes"),
    cmd!("checkpoint", "list", "[--from-git] [--project P]", "The checkpoints and the mocks each holds; --from-git lists the configuration repository's (after git pull).", &[Flag::switch("--from-git", "The repository's checkpoints, at the working copy's HEAD"), PROJECT_FLAG], [("GET", "/_rustybin/checkpoints")], false, "checkpoint list"),
    cmd!("checkpoint", "show", "<name>", "One checkpoint.", NO_FLAGS, [("GET", "/_rustybin/checkpoints/{name}")], false, "checkpoint show demo-start"),
    cmd!("checkpoint", "save", "<name> [--replace] [--project P]", "Save the instance's API mocks (with their data) and the health toggle; --project keeps one project's mocks only.", &[Flag::switch("--replace", "Overwrite a checkpoint of that name"), Flag::value("--project", "Keep this project's mocks only (a restore changes only those)")], [("POST", "/_rustybin/checkpoints")], false, "checkpoint save demo-start"),
    cmd!("checkpoint", "restore", "<name> [--dry-run] [--from-git]", "Bring the API mocks, their data and health back (other API mocks are deleted, flaky counters reset); --dry-run says what would change, --from-git restores the configuration repository's checkpoint.", &[Flag::switch("--dry-run", "Only say what a restore would change"), Flag::switch("--from-git", "The repository's checkpoint of that name (git pull first)")],[("POST", "/_rustybin/checkpoints/{name}/restore")], true, "checkpoint restore demo-start --yes"),
    cmd!("checkpoint", "delete", "<name>", "Delete a checkpoint (the instance is not changed).", NO_FLAGS, [("DELETE", "/_rustybin/checkpoints/{name}")], true, "checkpoint delete demo-start --yes"),
    cmd!("tests", "list", "[--project P]", "The saved tests and the variables each uses.", PROJECT_FLAGS, [("GET", "/_rustybin/tests")], false, "tests list"),
    cmd!("tests", "show", "<name>", "One saved test.", NO_FLAGS, [("GET", "/_rustybin/tests/{name}")], false, "tests show order-1"),
    cmd!("tests", "save", "<name> --capture ID [--gateway U] [--description D] [--project P]", "Save a captured request as a test (its status asserted; credentials become {{variables}}).", &[Flag::value("--capture", "The captured request's id (rustybin requests list)"), Flag::value("--gateway", "Send it through this gateway base URL"), Flag::value("--description", "What it checks"), Flag::value("--project", "The project it belongs to (default: the instance's, or your one project)")], [("POST", "/_rustybin/tests")], false, "tests save order-1 --capture 0123456789abcdef"),
    cmd!("tests", "put", "<name> --file F", "Save or replace a test from a file (JSON or YAML: request, gateway, assert).", &[Flag::value("--file", "The test: a file, - for stdin, or inline JSON")], [("PUT", "/_rustybin/tests/{name}")], false, r#"tests put order-1 --file '{"request":{"path":"/status/200"},"assert":[{"status":200}]}'"#),
    cmd!("tests", "delete", "<name>", "Delete a saved test.", NO_FLAGS, [("DELETE", "/_rustybin/tests/{name}")], true, "tests delete order-1 --yes"),
    cmd!("tests", "run", "[--names a,b] [--project P] [--var NAME=value]... [--junit]", "Run saved tests (all by default); exits 1 when one fails. --junit prints JUnit XML for CI.", &[Flag::value("--names", "Only these tests, comma separated"), Flag::value("--var", "A {{variable}}: NAME=value, or NAME=- (stdin) or NAME=@file for a credential (repeat it)"), Flag::switch("--junit", "Print the JUnit XML report"), PROJECT_FLAG], [("POST", "/_rustybin/tests/run")], false, "tests run --junit"),
    cmd!("tests", "export", "[<name>] [--format hurl|arazzo]", "Every saved test as one Hurl file (hurl --test --variable base_url=...), or the one named: as Hurl, or as an Arazzo document (--format arazzo).", &[Flag::value("--format", "hurl (default) or arazzo (one test, by name)")], [("GET", "/_rustybin/tests/export"), ("GET", "/_rustybin/tests/{name}/export")], false, "tests export"),
    cmd!("tests", "import", "<file> [--dry-run] [--gateway U] [--replace] [--project P]", "Import an Arazzo document: each workflow becomes a multi-step saved test against the instance's mocks; what has no counterpart is listed as left out. --dry-run saves nothing.", &[Flag::switch("--dry-run", "Answer the tests and what was left out; save nothing"), Flag::value("--gateway", "Send every imported test through this gateway base URL"), Flag::switch("--replace", "Overwrite tests of the same names"), Flag::value("--project", "The project the tests go to")], [("POST", "/_rustybin/tests/import")], false, r#"tests import '{"arazzo":"1.0.1","info":{"title":"Pets","version":"1"},"sourceDescriptions":[{"name":"petstore","url":"./petstore.yaml","type":"openapi"}],"workflows":[{"workflowId":"list-pets","steps":[{"stepId":"list","operationId":"listPets","successCriteria":[{"condition":"$statusCode == 200"}]}]}]}' --dry-run"#),
    cmd!("tests", "runs", "[--limit N] [--cursor C]", "The history of test runs, newest first: when, the trigger, who, the counts, what changed since the previous run.", &[Flag::value("--limit", "Runs a page (20, at most 100)"), Flag::value("--cursor", "The next_cursor of the page before")], [("GET", "/_rustybin/tests/runs")], false, "tests runs --limit 5"),
    cmd!("tests", "run-show", "<id> [--junit]", "One kept run: each test's result and failures, the change since the previous run; --junit prints its JUnit XML.", &[Flag::switch("--junit", "Print the JUnit XML report")], [("GET", "/_rustybin/tests/runs/{id}")], false, "tests run-show 20261103T101500000Z-abc123 --junit"),
    cmd!("tests", "runs-clear", "", "Forget every kept test run (the tests stay).", NO_FLAGS, [("DELETE", "/_rustybin/tests/runs")], true, "tests runs-clear --yes"),
    cmd!("tests", "schedules", "", "The saved tests on a schedule: when, what, on or off, the last result, the next time, the last notice.", NO_FLAGS, [("GET", "/_rustybin/tests/schedules")], false, "tests schedules"),
    cmd!("tests", "schedule-show", "<name>", "One test schedule with its status and recent outcomes.", NO_FLAGS, [("GET", "/_rustybin/tests/schedules/{name}")], false, "tests schedule-show nightly"),
    cmd!("tests", "schedule-save", "<name> (--every D | --at HH:MM) [--names a,b | --prefix P] [--webhook U] [--notify-recovery] [--disabled]", "Save or replace a test schedule: every 5m to 24h, or at HH:MM UTC; every saved test unless --names or --prefix.", &[Flag::value("--every", "Run every 5m to 24h (15m, 2h)"), Flag::value("--at", "Run once a day at HH:MM, UTC"), Flag::value("--names", "Only these tests, comma separated"), Flag::value("--prefix", "Only the tests whose name starts with this"), Flag::value("--webhook", "Post a JSON summary of a failing run here (allow its host: egress purpose notify)"), Flag::switch("--notify-recovery", "One more notice when a run passes after a failure"), Flag::switch("--disabled", "Saved, but off"), Flag::value("--description", "What it is for")], [("PUT", "/_rustybin/tests/schedules/{name}")], false, "tests schedule-save nightly --every 1h --prefix order"),
    cmd!("tests", "schedule-run", "<name>", "Run a test schedule now, as the scheduler would (kept in the history); exits 1 when it fails.", NO_FLAGS, [("POST", "/_rustybin/tests/schedules/{name}/run")], false, "tests schedule-run nightly"),
    cmd!("tests", "schedule-delete", "<name>", "Delete a test schedule (the tests and the history stay).", NO_FLAGS, [("DELETE", "/_rustybin/tests/schedules/{name}")], true, "tests schedule-delete nightly --yes"),
    cmd!("scenario", "list", "", "The gateway scenarios (HTTP: retries, rate limiting, credential injection, timeouts, caching, circuit breakers, request size limits, CORS, header transformation; AI: token limits, model fallback, semantic caching, prompt and response guards, load balancing; MCP: tool poisoning, rug pull, tool shadowing, result injection, oversized results, exfiltration links) and your runs.", NO_FLAGS, [("GET", "/_rustybin/scenarios")], false, "scenario list"),
    cmd!("scenario", "start", "<scenario>", "Prepare a run: its upstream and the steps to configure on the gateway.", NO_FLAGS, [("POST", "/_rustybin/scenarios/{id}/runs")], false, "scenario start caching"),
    cmd!("scenario", "show", "<scenario> <run>", "A run: the setup, what it created, the last verdict.", NO_FLAGS, [("GET", "/_rustybin/scenarios/{id}/runs/{run}")], false, "scenario show caching 1a2b3c4d"),
    cmd!("scenario", "check", "<scenario> <run> --gateway U [--limit N] [--half-open-ms MS] [--size-limit BYTES] [--allowed-origin O] [--refused-origin O] [--upstream-cors] [--add-request-header H] [--add-request-value V] [--remove-request-header H] [--add-response-header H] [--token-limit N] [--fail-kind K] [--skip-injection] [--min-share PCT] [--max-result-kib KIB]", "Send the run's test requests through the gateway and compare what the client got with what reached the upstream; exits 1 when an expectation fails.", &[Flag::value("--gateway", "The URL the gateway serves the route at"), Flag::value("--limit", "rate_limiting: the limit set on the gateway"), Flag::value("--half-open-ms", "circuit_breaker: the gateway's half-open time in milliseconds"), Flag::value("--size-limit", "request_size_limit: the gateway's body limit in bytes"), Flag::value("--allowed-origin", "cors: an origin the gateway allows (scheme://host[:port])"), Flag::value("--refused-origin", "cors: an origin the gateway refuses"), Flag::switch("--upstream-cors", "cors: the gateway lets preflights through and the upstream answers CORS"), Flag::value("--add-request-header", "header_transformation: the request header the gateway adds"), Flag::value("--add-request-value", "header_transformation: the value it adds (optional)"), Flag::value("--remove-request-header", "header_transformation: the request header the gateway removes"), Flag::value("--add-response-header", "header_transformation: the response header the gateway adds"), Flag::value("--token-limit", "ai_token_limit: the token budget set on the gateway"), Flag::value("--fail-kind", "ai_model_fallback: the error the primary target answers (rate_limit, overloaded, server_error, unavailable, timeout)"), Flag::switch("--skip-injection", "ai_prompt_guard: leave out the prompt injection"), Flag::value("--min-share", "ai_provider_balance: the least share of the requests per target, in percent"), Flag::value("--max-result-kib", "mcp_oversized_result: the largest tool result the gateway passes, in KiB")], [("POST", "/_rustybin/scenarios/{id}/runs/{run}/check")], false, "scenario check caching 1a2b3c4d --gateway http://localhost:8000/catalog-api"),
    cmd!("scenario", "end", "<scenario> <run>", "End a run: remove the mock and counters it created, nothing else.", NO_FLAGS, [("DELETE", "/_rustybin/scenarios/{id}/runs/{run}")], true, "scenario end caching 1a2b3c4d --yes"),
    cmd!("storyboard", "list", "[--project P]", "The demo storyboards and the variables each uses.", PROJECT_FLAGS, [("GET", "/_rustybin/storyboards")], false, "storyboard list"),
    cmd!("storyboard", "show", "<name>", "One storyboard: its steps, and where you are in it.", NO_FLAGS, [("GET", "/_rustybin/storyboards/{name}")], false, "storyboard show retry-demo"),
    cmd!("storyboard", "apply", "[<name>] --file F [--project P]", "Save or replace a storyboard from a file (JSON or YAML: title, steps with a talk_track, an action and a reset); the name is the argument, else the file's.", &[Flag::value("--file", "The storyboard: a file, - for stdin, or inline JSON"), Flag::value("--project", "The project it belongs to (default: the instance's, or your one project)")], [("PUT", "/_rustybin/storyboards/{name}")], false, r##"storyboard apply retry-demo --file '{"title":"Retries","steps":[{"title":"Open the mocks","action":{"type":"open_view","route":"#/mocks"}}]}'"##),
    cmd!("storyboard", "run", "<name> [--step N] [--var NAME=value]...", "Rehearse a storyboard: run every step in order (or one with --step) with your own permissions and print each verdict; exits 1 when a step fails.", &[Flag::value("--step", "Only this step, counted from 1"), Flag::value("--var", "A {{variable}}: NAME=value, or NAME=- (stdin) or NAME=@file for a credential (repeat it)")], [("POST", "/_rustybin/storyboards/{name}/steps/{index}/run")], false, "storyboard run retry-demo"),
    cmd!("storyboard", "reset", "<name> [--var NAME=value]...", "Before a rehearsal: run every step's reset action, end the scenario runs the storyboard started, clear your progress.", &[Flag::value("--var", "A {{variable}}: NAME=value, or NAME=- (stdin) or NAME=@file for a credential (repeat it)")], [("POST", "/_rustybin/storyboards/{name}/reset")], false, "storyboard reset retry-demo"),
    cmd!("storyboard", "delete", "<name>", "Delete a storyboard (what its steps changed stays).", NO_FLAGS, [("DELETE", "/_rustybin/storyboards/{name}")], true, "storyboard delete retry-demo --yes"),
    cmd!("pack", "list", "", "The demo packs applied to the instance and what each holds now.", NO_FLAGS, [("GET", "/_rustybin/packs")], false, "pack list"),
    cmd!("pack", "apply", "<file|dir> [--dry-run] [--untrusted]", "Apply a demo pack: a file, or a directory with a rustybin.yaml (its spec files are read here and sent inline). Everything goes to a new project named after the pack; a name that exists is refused. --dry-run previews.", &[Flag::switch("--dry-run", "Say what it would create, after the same checks, and change nothing"), Flag::switch("--untrusted", "A pack from someone else: no new gateways or webhooks")], [("POST", "/_rustybin/packs/apply")], false, r#"pack apply '{"pack":{"name":"acme"},"storyboards":[{"name":"acme-demo","steps":[{"title":"Welcome"}]}]}' --dry-run"#),
    cmd!("pack", "teardown", "<name> [--dry-run]", "Remove an applied pack: what its project holds, its test schedules, the console title it set and the project. --dry-run lists it.", &[Flag::switch("--dry-run", "List what would be removed and change nothing")], [("DELETE", "/_rustybin/packs/{name}")], false, "pack teardown acme --dry-run"),
    cmd!("pack", "export", "<project> [--output FILE] [--format yaml|json]", "A project's mocks, tests and storyboards as a pack (specs inline): a demo built by hand becomes reusable.", &[Flag::value("--output", "Write the pack to this file instead of printing it"), Flag::value("--format", "yaml (default) or json")], [("GET", "/_rustybin/packs/{name}/export")], false, "pack export acme"),
    cmd!("bins", "list", "", "Your request bins.", NO_FLAGS, [("GET", "/bin")], false, "bins list"),
    cmd!("bins", "create", "[--status N] [--body B] [--delay-ms N]", "A new bin, with the answer it gives.", &[Flag::value("--status", "The status the bin answers"), Flag::value("--body", "The body it answers: a file or inline JSON"), Flag::value("--delay-ms", "A delay before the answer")], [("POST", "/bin")], false, "bins create --status 202"),
    cmd!("bins", "delete", "<id>", "Delete a bin.", NO_FLAGS, [("DELETE", "/bin/{id}")], true, "bins delete 0123456789abcdef01234567 --yes"),
    cmd!("bins", "requests", "<id> [--limit N]", "What a bin captured.", &[Flag::value("--limit", "At most this many")], [("GET", "/bin/{id}/requests")], false, "bins requests 0123456789abcdef01234567"),
    cmd!("bins", "get", "<id> <n>", "One captured request of a bin.", NO_FLAGS, [("GET", "/bin/{id}/requests/{n}")], false, "bins get 0123456789abcdef01234567 1"),
    cmd!("bins", "tail", "<id>", "Follow a bin's requests.", TAIL_FLAGS, [("GET", "/bin/{id}/requests/stream")], false, "bins tail 0123456789abcdef01234567 --max 3"),
    // ── Agents ──
    cmd!("agents", "list", "[--owner U]", "Every AI agent that called the management server, the user each acts for, and the policy (yours only without agents.read).", &[Flag::value("--owner", "Only the agents acting for this user (an id or a sign-in name)")], [("GET", "/_rustybin/agents")], false, "agents list"),
    cmd!("agents", "get", "<id>", "One agent: its user, tools, last errors, sessions.", NO_FLAGS, [("GET", "/_rustybin/agents/{id}")], false, "agents get 1f3a..."),
    cmd!("agents", "rights", "<id> [--ceiling C] [--allow T] [--deny T] [--note N] [--reset]", "Limit an agent: a ceiling (a role or a 0.3 ceiling), tools allowed or denied, a note; --reset goes back to the rules. Your own agents only to limit them, without agents.manage.", &[Flag::value("--ceiling", "A role (viewer, editor, a custom role) or read-only, traffic, manage, full"), Flag::value("--allow", "Tool names, comma separated"), Flag::value("--deny", "Tool names, comma separated"), Flag::value("--note", "A note"), Flag::switch("--reset", "Drop the agent's own rights and note")], [("PATCH", "/_rustybin/agents/{id}")], false, "agents rights 1f3a... --ceiling viewer --deny try_request"),
    cmd!("agents", "revoke", "<id>", "Refuse the agent's calls, end its sessions, revoke its tokens.", NO_FLAGS, [("POST", "/_rustybin/agents/{id}/revoke")], true, "agents revoke 1f3a... --yes"),
    cmd!("agents", "restore", "<id>", "Undo a revocation.", NO_FLAGS, [("POST", "/_rustybin/agents/{id}/restore")], false, "agents restore 1f3a..."),
    cmd!("agents", "end-sessions", "<id>", "End the agent's MCP sessions.", NO_FLAGS, [("DELETE", "/_rustybin/agents/{id}/sessions")], false, "agents end-sessions 1f3a..."),
    cmd!("agents", "tail", "", "Follow the agents' calls as they happen.", TAIL_FLAGS, [("GET", "/_rustybin/agents/events")], false, "agents tail --max 10"),
    // ── AI ──
    cmd!("ai", "status", "", "Whether AI assist is on, the provider, the caps.", NO_FLAGS, [("GET", "/_rustybin/assist")], false, "ai status"),
    cmd!("ai", "models", "[--load]", "The configured provider's models with tier, context, price, whether each reasons and the jobs it runs; --load asks the provider for its list with the stored key first (admin).", &[Flag::switch("--load", "Load the list from the provider with the stored key (admin)")], [("GET", "/_rustybin/assist/models"), ("POST", "/_rustybin/assist/models")], false, "ai models --load"),
    cmd!("ai", "jobs", "", "Which model each AI job (task) runs on, and whether a setting chose it.", NO_FLAGS, [("GET", "/_rustybin/assist/models")], false, "ai jobs"),
    cmd!("ai", "use", "<model> [--job TASK] [--fast]", "Choose a model: the model for the capable jobs, with --fast the fast model, with --job the model of one job (empty with --job clears the choice).", &[Flag::value("--job", "The job (task name) to run on this model"), Flag::switch("--fast", "Make it the fast model")], [("PATCH", "/_rustybin/settings")], false, "ai use gpt-5-mini --job explain"),
    cmd!("ai", "usage", "", "Calls and tokens since start.", NO_FLAGS, [("GET", "/_rustybin/assist/usage")], false, "ai usage"),
    cmd!("ai", "test", "", "Call the provider once with the configured key.", NO_FLAGS, [("POST", "/_rustybin/assist/test")], false, "ai test"),
    cmd!("ai", "run", "<task> [--input JSON] [--model M]", "Run an assist task (free_answer, explain, suggest, ...) and print its answer.", &[Flag::value("--input", "The task's inputs as JSON or a file"), Flag::value("--model", "A model id instead of the task's default")], [("POST", "/_rustybin/assist/run")], false, "ai run free_answer --input '{\"question\": \"what does /delay do?\"}'"),
    // ── MCP ──
    cmd!("mcp", "servers", "", "This instance's MCP servers.", NO_FLAGS, [("GET", "/_rustybin/mcp-servers")], false, "mcp servers"),
    cmd!("mcp", "connections", "", "The saved connections.", NO_FLAGS, [("GET", "/_rustybin/mcp-connections")], false, "mcp connections"),
    cmd!("mcp", "connection", "<id>", "One saved connection.", NO_FLAGS, [("GET", "/_rustybin/mcp-connections/{id}")], false, "mcp connection acme"),
    cmd!("mcp", "save-connection", "<id> --target URL", "Save a connection (its secrets sealed on the instance).", &[Flag::value("--target", "The MCP endpoint"), Flag::value("--name", "A display name"), Flag::value("--transport", "modern, session or legacy"), Flag::value("--mode", "browser or relay"), Flag::value("--auth", "The auth spec as JSON ({\"kind\": \"bearer\"})"), Flag::value("--secret", "name=- (stdin) or name=@file, repeatable (bearer, api_key, password, header:<name>)")], [("PUT", "/_rustybin/mcp-connections/{id}")], false, "mcp save-connection acme --target https://mcp.acme.example/mcp --auth '{\"kind\": \"bearer\"}' --secret bearer=- < token.txt"),
    cmd!("mcp", "delete-connection", "<id>", "Forget a saved connection.", NO_FLAGS, [("DELETE", "/_rustybin/mcp-connections/{id}")], true, "mcp delete-connection acme --yes"),
    cmd!("mcp", "check", "<target>", "Run the checks against a server (--junit writes a report).", &[Flag::value("--transport", "modern, session or legacy"), Flag::value("--version", "The protocol version to speak"), Flag::value("--header", "An extra header 'Name: value', repeatable"), Flag::value("--connection", "A saved connection to take the credentials from"), Flag::value("--checks", "Only these checks, comma separated"), Flag::value("--junit", "Write a JUnit report to this file")], [("POST", "/_rustybin/mcp-checks")], false, "mcp check http://localhost:8080/mcp --junit mcp.xml"),
    cmd!("mcp", "runs", "", "The saved check runs.", NO_FLAGS, [("GET", "/_rustybin/mcp-checks")], false, "mcp runs"),
    cmd!("mcp", "run", "<id>", "One saved check run.", NO_FLAGS, [("GET", "/_rustybin/mcp-checks/{id}")], false, "mcp run 3"),
    cmd!("mcp", "delete-run", "<id>", "Forget a saved check run.", NO_FLAGS, [("DELETE", "/_rustybin/mcp-checks/{id}")], true, "mcp delete-run 3 --yes"),
    cmd!("mcp", "call", "<target> <method> [--params JSON]", "Send one JSON-RPC message to a server through the instance's relay.", &[Flag::value("--params", "The params as JSON or a file"), Flag::value("--transport", "streamable (default) or legacy"), Flag::value("--header", "An extra header 'Name: value', repeatable"), Flag::value("--connection", "A saved connection to take the credentials from")], [("POST", "/_rustybin/mcp-relay")], false, "mcp call http://localhost:8080/mcp tools/call --params '{\"name\": \"echo\", \"arguments\": {\"message\": \"hi\"}}'"),
    cmd!("mcp", "tools", "<target>", "The tools a server offers (tools/list through the relay).", &[Flag::value("--header", "An extra header 'Name: value', repeatable"), Flag::value("--connection", "A saved connection to take the credentials from")], [("POST", "/_rustybin/mcp-relay")], false, "mcp tools http://localhost:8080/mcp"),
    cmd!("mcp", "fetch", "<url>", "Fetch an OAuth discovery document through the instance.", NO_FLAGS, [("POST", "/_rustybin/mcp-relay/fetch")], false, "mcp fetch https://mcp.acme.example/.well-known/oauth-authorization-server"),
    cmd!("mcp", "end-relay", "<session>", "End a relay session.", NO_FLAGS, [("DELETE", "/_rustybin/mcp-relay/{session}")], false, "mcp end-relay 5b2c"),
    // ── Registry ──
    cmd!("registry", "status", "", "The mock registry: scenario, entries, capacity.", NO_FLAGS, [("GET", "/_rustybin/registry")], false, "registry status"),
    cmd!("registry", "publish", "<file>", "Publish a server.json to the mock registry (through its publish API).", NO_FLAGS, [], false, "registry publish server.json"),
    cmd!("registry", "delete", "<name> [--version V]", "Delete a published entry.", &[Flag::value("--version", "Only this version")], [("DELETE", "/_rustybin/registry/servers/{serverName}")], true, "registry delete io.example/orders --yes"),
    cmd!("registry", "validate", "<file>", "Validate a server.json against the schema and the semantic checks.", NO_FLAGS, [("POST", "/_rustybin/registry/validate")], false, "registry validate server.json"),
    cmd!("registry", "browse", "[--registry U] [--search S] [--cursor C]", "Browse a registry (the official one by default) through the instance.", &[Flag::value("--registry", "The registry base URL"), Flag::value("--search", "A search term"), Flag::value("--cursor", "The next page")], [("GET", "/_rustybin/registry/browse")], false, "registry browse --search weather"),
    // ── Portal ──
    cmd!("portal", "status", "", "The portal: on or off, who may read it, how many users read closed items, each item and its access (open or closed).", NO_FLAGS, [("GET", "/_rustybin/portal")], false, "portal status"),
    cmd!("portal", "publish", "<id> [--access open|closed] [--secure]", "Publish an item: mock:NAME, mcp:SERVER, llm, a2a:AGENT, tools or registry, open or closed (a closed mock needs an API key or Basic user of its own on every call; --secure sets random ones).", &[Flag::value("--title", "A title"), Flag::value("--summary", "A summary (Markdown)"), Flag::value("--tags", "Tags, comma separated"), Flag::value("--contact", "A contact"), Flag::value("--order", "Lower comes first"), Flag::switch("--no-credentials", "Hide a mock's demo credentials"), Flag::value("--access", "open (default) or closed: everyone reads a closed item, only portal readers use it"), Flag::switch("--secure", "With --access closed on a mock that cannot be closed as it is: turn its security on with random credentials, deploy, then close")], [("PUT", "/_rustybin/portal/items/{id}")], false, "portal publish mock:orders --title \"Orders API\" --tags payments"),
    cmd!("portal", "unpublish", "<id>", "Remove an item from the portal.", NO_FLAGS, [("DELETE", "/_rustybin/portal/items/{id}")], true, "portal unpublish mock:orders --yes"),
    cmd!("portal", "insights", "", "Views, downloads and Try it calls.", NO_FLAGS, [("GET", "/_rustybin/portal/insights")], false, "portal insights"),
    cmd!("portal", "branding", "", "The portal.* settings.", NO_FLAGS, [("GET", "/_rustybin/settings")], false, "portal branding"),
    cmd!("portal", "open", "", "The portal's URL.", NO_FLAGS, [], false, "portal open"),
    // ── Config ──
    cmd!("config", "export", "[--format F] [--secrets S] [--output FILE]", "The whole instance as rustybin.yaml (yaml, json, or a bundle to a file).", &[Flag::value("--format", "yaml (default), json or bundle"), Flag::value("--secrets", "refs (default) or omit"), Flag::value("--output", "Write to this file (a bundle needs it)")], [("GET", "/_rustybin/config/export")], false, "config export --output rustybin.yaml"),
    cmd!("config", "apply", "<file> [--dry-run] [--prune]", "Apply a document; a dry run shows the changes and touches nothing.", &[Flag::switch("--dry-run", "Report only"), Flag::switch("--prune", "A present section is the whole truth")], [("POST", "/_rustybin/config/apply")], false, "config apply rustybin.yaml --dry-run"),
    cmd!("config", "diff", "<file>", "What applying a document would change (a dry run).", &[Flag::switch("--prune", "Count what pruning would remove")], [("POST", "/_rustybin/config/apply")], false, "config diff rustybin.yaml"),
    cmd!("config", "schema", "", "The JSON Schema of rustybin.yaml, version 2.", NO_FLAGS, [("GET", "/_rustybin/config/schema")], false, "config schema"),
    cmd!("config", "check", "<file>", "Check a document offline: every section parses, every mock compiles.", NO_FLAGS, [], false, "config check rustybin.yaml"),
    // ── Git ──
    cmd!("git", "status", "[--fetch]", "The repository, the working copy, the drift, the timeline.", &[Flag::switch("--fetch", "Ask the remote for ahead and behind")], [("GET", "/_rustybin/git")], false, "git status --fetch"),
    cmd!("git", "init", "", "Connect the repository (clone, or start an empty working copy).", NO_FLAGS, [("POST", "/_rustybin/git/init")], false, "git init"),
    cmd!("git", "commit", "-m \"<message>\" [--checkpoint NAME]...", "Export the instance into the working copy and commit; --checkpoint also commits a local checkpoint (repeat it, or a,b).", COMMIT_FLAGS,[("POST", "/_rustybin/git/commit")], false, "git commit -m \"Add the orders mock\""),
    cmd!("git", "push", "", "Push the branch.", NO_FLAGS, [("POST", "/_rustybin/git/push")], false, "git push"),
    cmd!("git", "pull", "[--apply] [--dry-run] [--prune]", "Fetch and fast-forward; --apply applies the repository's document.", &[Flag::switch("--apply", "Apply the document"), Flag::switch("--dry-run", "Show the diff, apply nothing"), Flag::switch("--prune", "The document is the whole truth")], [("POST", "/_rustybin/git/pull")], false, "git pull --apply --dry-run"),
    cmd!("git", "resolve", "--with instance|repository|pull-request [--branch NAME] [--dry-run]", "The ways out of a diverged branch (git status shows what differs): the instance's copy wins, the repository's wins, or the instance's side goes to a new branch for a pull request at the git host (nothing applied). --dry-run previews any of them.", RESOLVE_FLAGS, [("POST", "/_rustybin/git/resolve")], false, "git resolve --with repository --dry-run"),
    cmd!("git", "key", "[--generate] [--replace]", "The deploy key's public half; --generate makes one.", &[Flag::switch("--generate", "Generate the key"), Flag::switch("--replace", "Replace an existing key")], [("GET", "/_rustybin/git/key"), ("POST", "/_rustybin/git/key")], true, "git key --generate"),
    cmd!("git", "history", "", "The last commits of the working copy.", NO_FLAGS, [("GET", "/_rustybin/git/history")], false, "git history"),
    // ── Harness ──
    cmd!("harness", "status", "", "Harness mode: roles, caps, runs, the kill switch.", NO_FLAGS, [("GET", "/_rustybin/harness")], false, "harness status"),
    cmd!("harness", "schema", "", "The JSON Schema of a scenario.", NO_FLAGS, [("GET", "/_rustybin/harness/schema")], false, "harness schema"),
    cmd!("harness", "scenarios", "", "The built-in and saved scenarios.", NO_FLAGS, [("GET", "/_rustybin/harness/scenarios")], false, "harness scenarios"),
    cmd!("harness", "scenario", "<name> [--yaml]", "One scenario.", &[Flag::switch("--yaml", "As YAML")], [("GET", "/_rustybin/harness/scenarios/{name}")], false, "harness scenario smoke --yaml"),
    cmd!("harness", "save-scenario", "<name> <file>", "Save a scenario under a name.", NO_FLAGS, [("PUT", "/_rustybin/harness/scenarios/{name}")], false, "harness save-scenario my-smoke smoke.yaml"),
    cmd!("harness", "delete-scenario", "<name>", "Delete a saved scenario.", NO_FLAGS, [("DELETE", "/_rustybin/harness/scenarios/{name}")], true, "harness delete-scenario my-smoke --yes"),
    cmd!("harness", "validate", "<file> --remote", "Validate a scenario on the instance (without --remote: offline, in this process).", &[Flag::switch("--remote", "On the instance")], [("POST", "/_rustybin/harness/validate")], false, "harness validate smoke.yaml --remote"),
    cmd!("harness", "preview", "<file>", "The run plan of a scenario: stages, requests, duration.", NO_FLAGS, [("POST", "/_rustybin/harness/preview")], false, "harness preview smoke.yaml"),
    cmd!("harness", "import", "<file> [--format F] [--var NAME=value]...", "A recording (a HAR file, or saved GET /_rustybin/requests), a Postman collection (v2.1), an Artillery script or an Arazzo document (its steps against the instance's mocks) as a scenario's source; what does not map is named, nothing in the file is run. --var gives a value for a variable the file does not define (a collection's environment). Sends nothing.", &[Flag::value("--format", "har, captures, postman, artillery or arazzo (default: read from the file)"), Flag::value("--var", "A variable of the file: NAME=value, the name as the file writes it (baseUrl for {{baseUrl}}, :id for a Postman path variable); repeatable, at most 64. Never a credential: an import removes credentials")], [("POST", "/_rustybin/harness/import")], false, "harness import recording.har"),
    cmd!("harness", "runs", "", "The runs on this node.", NO_FLAGS, [("GET", "/_rustybin/harness/runs")], false, "harness runs"),
    cmd!("harness", "run", "<file> --remote [--wait]", "Start a run on the instance (without --remote: from this process); --wait follows it and prints the report.", &[Flag::switch("--remote", "On the instance"), Flag::switch("--wait", "Follow the run and print the report"), Flag::value("--format", "The report format with --wait: text (default), json or junit")], [("POST", "/_rustybin/harness/runs")], false, "harness run smoke.yaml --remote --wait"),
    cmd!("harness", "show", "<run>", "One run: state, progress, the live controls.", NO_FLAGS, [("GET", "/_rustybin/harness/runs/{run}")], false, "harness show r-1"),
    cmd!("harness", "tune", "<run> <change>", "A live change (JSON or a file): rate, users, pause, resume, ...", NO_FLAGS, [("PATCH", "/_rustybin/harness/runs/{run}")], false, "harness tune r-1 '{\"rate\": 50}'"),
    cmd!("harness", "stop", "<run>", "Stop a run.", NO_FLAGS, [("POST", "/_rustybin/harness/runs/{run}/stop")], false, "harness stop r-1"),
    cmd!("harness", "stop-all", "", "The kill switch: stop every run on the node.", NO_FLAGS, [("POST", "/_rustybin/harness/stop")], true, "harness stop-all --yes"),
    cmd!("harness", "events", "<run>", "Follow a run's events.", TAIL_FLAGS, [("GET", "/_rustybin/harness/runs/{run}/events")], false, "harness events r-1"),
    cmd!("harness", "report", "<run> --remote [--format F]", "A run's report (without --remote: render a saved report file).", &[Flag::switch("--remote", "On the instance"), Flag::value("--format", "text (default), json or junit")], [("GET", "/_rustybin/harness/runs/{run}/report")], false, "harness report r-1 --remote --format junit"),
    cmd!("harness", "clusters", "", "The clusters this node leads.", NO_FLAGS, [("GET", "/_rustybin/harness/clusters")], false, "harness clusters"),
    cmd!("harness", "create-cluster", "[--body JSON]", "Create a cluster (cluster_id, min_nodes, lease, join_token).", &[Flag::value("--body", "The cluster settings as JSON or a file")], [("POST", "/_rustybin/harness/clusters")], false, "harness create-cluster --body '{\"min_nodes\": {\"clients\": 2}}'"),
    cmd!("harness", "cluster", "<id>", "A cluster: nodes, leader, runs.", NO_FLAGS, [("GET", "/_rustybin/harness/clusters/{id}")], false, "harness cluster c-1"),
    cmd!("harness", "delete-cluster", "<id>", "Dissolve a cluster.", NO_FLAGS, [("DELETE", "/_rustybin/harness/clusters/{id}")], true, "harness delete-cluster c-1 --yes"),
    cmd!("harness", "leader", "<id>", "The cluster's leader.", NO_FLAGS, [("GET", "/_rustybin/harness/clusters/{id}/leader")], false, "harness leader c-1"),
    cmd!("harness", "step-aside", "<id>", "Before maintenance: the leader hands the cluster to the first in the succession, with the run still on.", NO_FLAGS, [("POST", "/_rustybin/harness/clusters/{id}/leader/step-aside")], false, "harness step-aside perf-1"),
    cmd!("harness", "upstreams", "<id>", "The cluster's server nodes as the gateway reaches them.", NO_FLAGS, [("GET", "/_rustybin/harness/clusters/{id}/upstreams")], false, "harness upstreams c-1"),
    cmd!("harness", "cluster-runs", "<id>", "The cluster's runs.", NO_FLAGS, [("GET", "/_rustybin/harness/clusters/{id}/runs")], false, "harness cluster-runs c-1"),
    cmd!("harness", "cluster-run", "<id> <file>", "Create a cluster run from a scenario (start it with cluster-start).", NO_FLAGS, [("POST", "/_rustybin/harness/clusters/{id}/runs")], false, "harness cluster-run c-1 smoke.yaml"),
    cmd!("harness", "cluster-show", "<id> <run>", "One cluster run.", NO_FLAGS, [("GET", "/_rustybin/harness/clusters/{id}/runs/{run}")], false, "harness cluster-show c-1 cr-1"),
    cmd!("harness", "cluster-tune", "<id> <run> <change>", "A live change on a cluster run.", NO_FLAGS, [("PATCH", "/_rustybin/harness/clusters/{id}/runs/{run}")], false, "harness cluster-tune c-1 cr-1 '{\"rate\": 50}'"),
    cmd!("harness", "cluster-start", "<id> <run>", "Start a cluster run.", NO_FLAGS, [("POST", "/_rustybin/harness/clusters/{id}/runs/{run}/start")], false, "harness cluster-start c-1 cr-1"),
    cmd!("harness", "cluster-stop", "<id> <run>", "Stop a cluster run.", NO_FLAGS, [("POST", "/_rustybin/harness/clusters/{id}/runs/{run}/stop")], false, "harness cluster-stop c-1 cr-1"),
    cmd!("harness", "cluster-events", "<id> <run>", "Follow a cluster run's events.", TAIL_FLAGS, [("GET", "/_rustybin/harness/clusters/{id}/runs/{run}/events")], false, "harness cluster-events c-1 cr-1"),
    cmd!("harness", "cluster-report", "<id> <run> [--format F]", "A cluster run's report.", &[Flag::value("--format", "text (default), json or junit")], [("GET", "/_rustybin/harness/clusters/{id}/runs/{run}/report")], false, "harness cluster-report c-1 cr-1"),
    cmd!("harness", "join", "<cluster-url> --join-token T", "Make this node join a cluster.", &[Flag::value("--join-token", "The join token"), Flag::value("--advertise", "This server node's data-plane URL as the gateway reaches it")], [("POST", "/_rustybin/harness/join")], false, "harness join https://leader.example/_rustybin/harness/clusters/c-1 --join-token ..."),
    cmd!("harness", "leave", "", "Leave the cluster.", NO_FLAGS, [("POST", "/_rustybin/harness/leave")], false, "harness leave"),
    // ── Single sign-on ──
    cmd!("sso", "status", "", "Single sign-on as the sign-in screen sees it: on, ready, the button's name, whether the password sign-in works.", NO_FLAGS, [("GET", "/_rustybin/sso/status")], false, "sso status"),
    cmd!("sso", "test", "", "Fetch the identity provider's discovery document and keys now, and check the role map; exits 1 when the provider cannot be read.", NO_FLAGS, [("POST", "/_rustybin/sso/test")], false, "sso test"),
    // ── Keys and egress ──
    cmd!("keys", "usage", "[--day D] [--after A] [--limit N]", "Per-key usage in shared mode.", &[Flag::value("--day", "A UTC day (YYYY-MM-DD)"), Flag::value("--after", "The page cursor"), Flag::value("--limit", "At most this many")], [("GET", "/_rustybin/keys/usage")], false, "keys usage --day 2026-10-30"),
    cmd!("egress", "show", "", "The outbound policy: the allowlist, each feature's rule, the last decisions.", NO_FLAGS, [("GET", "/_rustybin/egress")], false, "egress show"),
    cmd!("egress", "check", "<url> [--purpose P]", "What the outbound policy would say to a URL, without calling it. Exits 1 when it would refuse.", &[Flag::value("--purpose", "The feature that would call it (default mcp_relay)")], [("GET", "/_rustybin/egress/check")], false, "egress check https://api.example.com --purpose git"),
    // ── The audit log ──
    cmd!("audit", "list", "[--actor A] [--action A] [--target T] [--since S] [--until U] [--result R] [--limit N] [--cursor C]", "The audit log, newest first: who did what and when, sign-ins and refusals (audit.read). --cursor pages back with the next_cursor printed last.", AUDIT_LIST_FLAGS, [("GET", "/_rustybin/audit")], false, "audit list --since 1h --result refused"),
    cmd!("audit", "tail", "[--actor A] [--action A] [--target T] [--result R]", "Follow the audit log: one line per new entry that matches (audit.read).", AUDIT_TAIL_FLAGS, [("GET", "/_rustybin/audit/stream")], false, "audit tail --action mocks --max 10"),
    // ── Secrets an agent asked for (core next 12) ──
    cmd!("settings", "requests", "[--state S]", "The secrets AI agents asked to have set (request_secret): the setting, the agent and the user it acts for, the agent's reason (its claim), the state, the note left on a decline. Everyone's with secrets.write, else your own agents'. An agent never sets a secret: you fulfil or decline.", &[Flag::value("--state", "Only pending, fulfilled, declined or expired")], [("GET", "/_rustybin/secret-requests")], false, "settings requests --state pending"),
    cmd!("settings", "fulfil", "<id> [--stdin]", "Set the secret an agent asked for: the value is prompted without echo, or read from the first line of stdin, never an argument. It goes to the settings store and never to the agent; needs what settings set-secret needs for that setting.", &[Flag::switch("--stdin", "Read the secret from the first line of stdin")], [("GET", "/_rustybin/secret-requests/{id}"), ("POST", "/_rustybin/secret-requests/{id}/fulfil")], false, "settings fulfil sr_0000000000000000 --stdin < key.txt"),
    cmd!("settings", "decline", "<id> [--note TEXT]", "Decline a secret request: nothing is set, and the agent reads declined, with your note when you leave one. Any request with secrets.write, else one of your own agents'.", &[Flag::value("--note", "A few words for the agent (at most 300 characters). Never the secret: the agent reads it")], [("POST", "/_rustybin/secret-requests/{id}/decline")], false, "settings decline sr_0000000000000000 --note \"use the deploy key that is already set\""),
    // ── Source control: branches per environment, signed commits ──
    cmd!("git", "branches", "", "The remote's branches, and the one this instance is on.", NO_FLAGS, [("GET", "/_rustybin/git/branches")], false, "git branches"),
    cmd!("git", "switch", "<branch> [--dry-run] [--prune] [--no-apply]", "Move this instance to another branch of the remote (a branch per environment). --dry-run shows what applying that branch's document would change; without it the branch is stored (git.branch), taken, and its document applied.", SWITCH_FLAGS, [("POST", "/_rustybin/git/branch")], false, "git switch staging --dry-run"),
    cmd!("git", "branch", "<name> [-m \"<message>\"] [--dry-run]", "Start a branch the remote does not have from this instance's configuration: committed, pushed, and the instance commits to it from now on.", BRANCH_FLAGS, [("POST", "/_rustybin/git/branch")], false, "git branch staging --dry-run"),
    cmd!("git", "signing-key", "[--generate] [--replace]", "The commit signing key's public half and fingerprint, to register at the git host as a signing key; --generate makes one. Commits are signed when the git.sign setting is on.", &[Flag::switch("--generate", "Generate the key"), Flag::switch("--replace", "Replace an existing key")], [("GET", "/_rustybin/git/signing-key"), ("POST", "/_rustybin/git/signing-key")], false, "git signing-key"),
];

/// Catalogue paths no command calls, each with its reason (the parity test
/// reads it).
pub const UNCOVERED: &[(&str, &str)] = &[
    (
        "/_rustybin/git/webhook",
        "a git host calls it, signed; rustybin git pull pulls by hand",
    ),
    (
        "/_rustybin/mocks/import/fetch",
        "the console's wizard reads the text first; mocks create --spec-url does it in one call",
    ),
    (
        "/_rustybin/sso/callback",
        "the identity provider sends the browser there (rustybin login --sso waits for its answer on a loopback address)",
    ),
    (
        "/_rustybin/join",
        "the invitation link a person opens in a browser, where they choose their password",
    ),
    (
        "/_rustybin/mcp",
        "the management MCP endpoint: rustybin mcp-stdio bridges a client to it",
    ),
    (
        "/_rustybin/oauth/requests/{id}",
        "the console's consent screen, where a person approves an agent's sign-in (rustybin login --agent waits for it)",
    ),
    (
        "/_rustybin/harness/clusters/{id}/nodes",
        "node-to-node: a member joining",
    ),
    (
        "/_rustybin/harness/clusters/{id}/nodes/{node}/heartbeat",
        "node-to-node: heartbeats",
    ),
    (
        "/_rustybin/harness/clusters/{id}/election/vote",
        "node-to-node: elections",
    ),
    (
        "/_rustybin/harness/clusters/{id}/election/elected",
        "node-to-node: elections",
    ),
    (
        "/bin/{id}/{*path}",
        "a bin's capture URL (the thing under test calls it)",
    ),
];

// ── Helpers ─────────────────────────────────────────────────────────

fn q(args: &super::Args, names: &[(&str, &'static str)]) -> Vec<(&'static str, String)> {
    names
        .iter()
        .filter_map(|(flag, key)| args.value(flag).map(|v| (*key, v.to_string())))
        .collect()
}

/// A value from text: JSON when it parses, else the string.
fn loose_json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
}

fn list_flag(args: &super::Args, name: &str) -> Option<Vec<String>> {
    args.value(name).map(|v| {
        v.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    })
}

fn headers_flag(args: &super::Args) -> Result<Value> {
    let mut m = Map::new();
    for h in args.values("--header") {
        let (k, v) = h
            .split_once(':')
            .ok_or_else(|| Fail::Usage(format!("--header {h:?}: expected 'Name: value'")))?;
        m.insert(k.trim().to_string(), json!(v.trim()));
    }
    Ok(Value::Object(m))
}

fn content_type_for(path: &str, text: &str) -> &'static str {
    if path.ends_with(".json") || text.trim_start().starts_with('{') {
        "application/json"
    } else {
        "application/yaml"
    }
}

fn tail_params(ctx: &Ctx) -> Result<(Option<usize>, Duration)> {
    let max = match ctx.args.value("--max") {
        Some(v) => Some(
            v.parse::<usize>()
                .map_err(|_| Fail::Usage("--max needs a number".into()))?,
        ),
        None => None,
    };
    let secs = match ctx.args.value("--timeout") {
        Some(v) => v
            .parse::<u64>()
            .map_err(|_| Fail::Usage("--timeout needs a number of seconds".into()))?,
        None => 3600,
    };
    Ok((max, Duration::from_secs(secs)))
}

/// Follow an SSE feed, printing one line per event.
async fn tail(ctx: &mut Ctx, path: &str) -> Result<()> {
    let (max, timeout) = tail_params(ctx)?;
    let json = ctx.json;
    let client = ctx.client()?;
    let mut lines: Vec<String> = Vec::new();
    client
        .sse(
            Method::GET,
            path,
            Body::None,
            max,
            timeout,
            &mut |event, data| {
                let parsed: Value = serde_json::from_str(data).unwrap_or_else(|_| json!(data));
                if json {
                    lines.push(json!({ "event": event, "data": parsed }).to_string());
                } else {
                    let compact = match &parsed {
                        Value::Object(m) => m
                            .iter()
                            .filter(|(_, v)| !v.is_object() && !v.is_array())
                            .map(|(k, v)| format!("{k}={}", super::render(v).trim_end()))
                            .collect::<Vec<_>>()
                            .join(" "),
                        other => super::render(other).trim_end().to_string(),
                    };
                    lines.push(format!("{event}: {compact}"));
                }
                true
            },
        )
        .await?;
    for l in lines {
        ctx.raw(&l);
    }
    Ok(())
}

fn emit_reply(ctx: &mut Ctx, reply: &Reply) {
    match &reply.json {
        Some(v) => ctx.emit(v),
        None => ctx.raw(&reply.text),
    }
}

/// `rustybin doctor`: the diagnostics as a table (or JSON), exit 1 on a
/// failed check.
async fn doctor(ctx: &mut Ctx) -> Result<()> {
    let v = ctx
        .client()?
        .get("/_rustybin/diagnostics", &[])
        .await?
        .value();
    if ctx.json {
        ctx.emit(&v);
    } else {
        let rows: Vec<Value> = v["checks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                json!({
                    "status": c["status"],
                    "check": c["id"],
                    "message": c["message"],
                    "fix": c["fix"],
                })
            })
            .collect();
        ctx.emit(&json!({ "status": v["status"], "checks": rows }));
    }
    if v["status"] == "fail" {
        return Err(Fail::Failed("a check failed (see above)".into()));
    }
    Ok(())
}

async fn get(ctx: &mut Ctx, path: &str, query: &[(&str, String)]) -> Result<()> {
    let reply = ctx.client()?.get(path, query).await?;
    emit_reply(ctx, &reply);
    Ok(())
}

async fn post(ctx: &mut Ctx, path: &str, body: Body) -> Result<()> {
    let reply = ctx.client()?.post(path, body).await?;
    emit_reply(ctx, &reply);
    Ok(())
}

async fn delete(ctx: &mut Ctx, path: &str, query: &[(&str, String)]) -> Result<()> {
    let reply = ctx.client()?.delete(path, query).await?;
    emit_reply(ctx, &reply);
    Ok(())
}

fn enc(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

// ── Dispatch ────────────────────────────────────────────────────────

/// Run the command in `ctx`.
pub async fn dispatch(ctx: &mut Ctx) -> Result<()> {
    let cmd = ctx.cmd;
    if cmd.destructive {
        let what = format!(
            "{} {}{}?",
            cmd.noun,
            cmd.verb,
            ctx.args
                .positional
                .first()
                .map(|a| format!(" {a}"))
                .unwrap_or_default()
        );
        ctx.confirm(&what)?;
    }
    match cmd.noun {
        "login" => login(ctx).await,
        "logout" => logout(ctx).await,
        "whoami" => get(ctx, "/_rustybin/session", &[]).await,
        "status" => get(ctx, "/_rustybin/status", &[]).await,
        "doctor" => doctor(ctx).await,
        "instance" => instance(ctx, cmd.verb).await,
        "profiles" => profiles_cmd(ctx, cmd.verb),
        "settings" => settings(ctx, cmd.verb).await,
        "sessions" => sessions(ctx, cmd.verb).await,
        "password" if cmd.verb == "hash" => password_hash(ctx),
        "password" => password(ctx, "/_rustybin/password").await,
        "passwd" => password(ctx, "/_rustybin/me/password").await,
        "sso" => sso(ctx, cmd.verb).await,
        "users" => users(ctx, cmd.verb).await,
        "roles" => roles(ctx, cmd.verb).await,
        "projects" => projects(ctx, cmd.verb).await,
        "me" => me(ctx, cmd.verb).await,
        "mocks" => mocks(ctx, cmd.verb).await,
        "visits" => visits(ctx, cmd.verb).await,
        "visit" => visit(ctx, cmd.verb).await,
        "requests" => requests(ctx, cmd.verb).await,
        "bins" => bins(ctx, cmd.verb).await,
        "agents" => agents(ctx, cmd.verb).await,
        "scenario" => scenario(ctx, cmd.verb).await,
        "tests" => tests(ctx, cmd.verb).await,
        "storyboard" => storyboard(ctx, cmd.verb).await,
        "pack" => pack(ctx, cmd.verb).await,
        "checkpoint" => checkpoint(ctx, cmd.verb).await,
        "record" => record(ctx, cmd.verb).await,
        "ai" => ai(ctx, cmd.verb).await,
        "mcp" => mcp(ctx, cmd.verb).await,
        "registry" => registry(ctx, cmd.verb).await,
        "portal" => portal(ctx, cmd.verb).await,
        "config" => config(ctx, cmd.verb).await,
        "git" => git(ctx, cmd.verb).await,
        "harness" => harness(ctx, cmd.verb).await,
        "keys" => {
            get(
                ctx,
                "/_rustybin/keys/usage",
                &q(
                    &ctx.args.clone(),
                    &[("--day", "day"), ("--after", "after"), ("--limit", "limit")],
                ),
            )
            .await
        }
        "egress" if cmd.verb == "check" => egress_check(ctx).await,
        "egress" => get(ctx, "/_rustybin/egress", &[]).await,
        "audit" => audit(ctx, cmd.verb).await,
        other => Err(Fail::Usage(format!("{other}: not implemented"))),
    }
}

// ── Sign-in ─────────────────────────────────────────────────────────

async fn login(ctx: &mut Ctx) -> Result<()> {
    let url = ctx
        .args
        .value("--url")
        .map(profiles::normalise)
        .or_else(|| ctx.env.url.as_deref().map(profiles::normalise))
        .or_else(|| {
            let args = ctx.args.clone();
            profiles::resolve(&ctx.env, &args).ok().map(|r| r.url)
        })
        .unwrap_or_else(|| "http://localhost:80".into());
    let name = ctx
        .args
        .value("--profile")
        .map(str::to_string)
        .unwrap_or_else(|| profiles::name_for(&url));
    if ctx.args.has("--agent") {
        return agent_login(ctx, &url, &name).await;
    }
    let (token, expires_at, user) = match ctx.args.value("--token") {
        Some(t) if super::agent_login::is_agent_token(t) => {
            return Err(Fail::Auth(format!(
                "{}; rustybin login --agent keeps one in a profile",
                super::agent_login::ONLY_MCP
            )))
        }
        Some(t) => {
            let client = super::client::Client::new(&url, Some(t.to_string()))?;
            let session = client.get("/_rustybin/session", &[]).await?.value();
            (
                t.to_string(),
                session["expires_at"].as_str().map(str::to_string),
                session["user"].as_str().map(str::to_string),
            )
        }
        None if ctx.args.has("--sso") => sso_login(ctx, &url).await?,
        None => {
            let user = ctx.args.value("--user").unwrap_or("rustybin").to_string();
            let password = ctx.secret("Password", ctx.args.has("--password-stdin"), 0)?;
            let client = super::client::Client::new(&url, None)?;
            let reply = client
                .send(
                    Method::POST,
                    "/_rustybin/login",
                    &[],
                    Body::Json(json!({ "username": user, "password": password })),
                )
                .await?;
            let reply = match reply.status {
                401 => {
                    return Err(Fail::Auth(format!(
                        "{url}: the user name or password was refused"
                    )))
                }
                403 if reply.value()["reason"] == "password_sign_in_off" => {
                    return Err(Fail::Auth(format!(
                        "{url}: the password sign-in is off: rustybin login --sso"
                    )))
                }
                404 | 409 => {
                    return Err(Fail::Failed(format!(
                        "{url}: the password sign-in is not on (RUSTYBIN_CONTROL_AUTH is not password); use --token"
                    )))
                }
                _ => reply.ok()?,
            };
            let v = reply.value();
            let token = v["token"]
                .as_str()
                .ok_or_else(|| Fail::Failed("the answer carries no token".into()))?
                .to_string();
            (
                token,
                v["expires_at"].as_str().map(str::to_string),
                v["user"].as_str().map(str::to_string),
            )
        }
    };
    let Some(path) = profiles::path(&ctx.env) else {
        return Err(Fail::Usage(
            "no home directory to keep the profile in: set RUSTYBIN_CONFIG_HOME, or use --token on every call".into(),
        ));
    };
    let mut all = profiles::load(&path)?;
    // The profile's agent sign-in stays when it is for the same instance.
    let agent = all
        .profiles
        .get(&name)
        .filter(|p| p.url == url)
        .and_then(|p| p.agent.clone());
    all.profiles.insert(
        name.clone(),
        profiles::Profile {
            url: url.clone(),
            token: Some(token),
            expires_at: expires_at.clone(),
            user: user.clone(),
            agent,
        },
    );
    all.default = Some(name.clone());
    profiles::save(&path, &all)?;
    if ctx.json {
        ctx.emit(&json!({ "profile": name, "url": url, "user": user, "expires_at": expires_at, "file": path.display().to_string() }));
    } else {
        ctx.say(&format!(
            "Signed in to {url}{} (profile {name}{}, kept in {}).",
            user.map(|u| format!(" as {u}")).unwrap_or_default(),
            expires_at
                .map(|e| format!(", expires {e}"))
                .unwrap_or_default(),
            path.display()
        ));
    }
    Ok(())
}

/// `login --agent`: the browser walk of the control plane's own
/// authorization server (`super::agent_login`), kept in the profile beside
/// the person's token. For `rustybin mcp-stdio` only.
async fn agent_login(ctx: &mut Ctx, url: &str, name: &str) -> Result<()> {
    if ctx.args.has("--sso") || ctx.args.value("--token").is_some() {
        return Err(Fail::Usage(
            "--agent signs an agent in through the browser: it does not go with --sso or --token"
                .into(),
        ));
    }
    let Some(path) = profiles::path(&ctx.env) else {
        return Err(Fail::Usage(
            "no home directory to keep the profile in: set RUSTYBIN_CONFIG_HOME".into(),
        ));
    };
    let kept = super::agent_login::login(ctx, url).await?;
    let mut all = profiles::load(&path)?;
    let entry = all
        .profiles
        .entry(name.to_string())
        .or_insert_with(profiles::Profile::default);
    if entry.url != url {
        // Another instance's sign-in never goes to this one.
        *entry = profiles::Profile {
            url: url.to_string(),
            ..profiles::Profile::default()
        };
    }
    let (scope, expires_at) = (kept.scope.clone(), kept.expires_at.clone());
    entry.agent = Some(kept);
    // A person's default profile stays theirs.
    if all.default.is_none() {
        all.default = Some(name.to_string());
    }
    profiles::save(&path, &all)?;
    if ctx.json {
        ctx.emit(&json!({ "profile": name, "url": url, "agent": { "scope": scope, "expires_at": expires_at }, "for": "mcp-stdio", "file": path.display().to_string() }));
    } else {
        ctx.say(&format!(
            "An agent is signed in to {url}{} (profile {name}, kept in {}).\nUse it with: rustybin mcp-stdio --profile {name}\nIt works with rustybin mcp-stdio only (the instance accepts its tokens on /_rustybin/mcp and nowhere else). The access token lasts an hour and mcp-stdio renews it for up to thirty days; it acts for you, with at most your permissions. See and revoke it in the console (AI agents) or with rustybin agents list | revoke.",
            scope.map(|s| format!(" with the scope {s}")).unwrap_or_default(),
            path.display()
        ));
    }
    Ok(())
}

/// `login --sso`: a loopback listener on this machine takes the browser
/// back from the instance (`return_to`), whose one-time code and this
/// process's PKCE verifier become a sign-in token. Answers the token, its
/// expiry and the user.
async fn sso_login(ctx: &mut Ctx, url: &str) -> Result<(String, Option<String>, Option<String>)> {
    let client = super::client::Client::new(url, None)?;
    let status = client.get("/_rustybin/sso/status", &[]).await?;
    if status.status == 404 {
        return Err(Fail::Failed(format!(
            "{url}: single sign-on works with the password sign-in only (RUSTYBIN_CONTROL_AUTH=password); use --token"
        )));
    }
    let status = status.ok()?.value();
    if status["ready"] != json!(true) {
        return Err(Fail::Failed(format!(
            "{url}: single sign-on is not set up ({})",
            status["reason"].as_str().unwrap_or("sso.enabled is off")
        )));
    }
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| Fail::Failed(format!("no loopback port to listen on: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| Fail::Failed(e.to_string()))?
        .port();
    let random = || {
        use base64::Engine;
        use rand::RngCore;
        let mut b = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut b);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
    };
    // The listener answers only the callback that carries this value (the
    // instance keeps return_to's query and adds the code): a page that
    // finds the port cannot end the sign-in with a forged answer.
    let cli_state = random();
    let return_to = format!("http://127.0.0.1:{port}/callback?cli_state={cli_state}");
    let verifier = random();
    let start = format!(
        "{}/_rustybin/sso/start?return_to={}&challenge={}",
        url.trim_end_matches('/'),
        enc(&return_to),
        crate::sso::challenge_of(&verifier)
    );
    let name = status["name"]
        .as_str()
        .unwrap_or("the identity provider")
        .to_string();
    ctx.say(&format!(
        "Sign in with {name} in the browser. If it does not open, visit:\n{start}"
    ));
    if !ctx.args.has("--no-browser") {
        match &ctx.env.browser {
            Some(b) => (b.0)(&start),
            None => open_browser(&start),
        }
    }
    let wait = ctx
        .args
        .value("--wait")
        .and_then(|w| w.parse::<u64>().ok())
        .unwrap_or(300)
        .clamp(1, 3600);
    let query = tokio::time::timeout(
        Duration::from_secs(wait),
        loopback_answer(listener, "cli_state", &cli_state),
    )
    .await
    .map_err(|_| Fail::Failed(format!("the browser did not come back in {wait} s")))?
    .map_err(|e| Fail::Failed(format!("the loopback listener failed: {e}")))?;
    let pairs: std::collections::BTreeMap<String, String> =
        form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
    if let Some(reason) = pairs.get("error") {
        // Printable only: what comes back through the browser never sends
        // the terminal an escape sequence.
        return Err(Fail::Auth(format!(
            "{url}: single sign-on refused ({}): {}",
            crate::logging::printable(reason, 80),
            crate::logging::printable(
                pairs
                    .get("error_description")
                    .map(String::as_str)
                    .unwrap_or(""),
                400
            )
        )));
    }
    let code = pairs
        .get("code")
        .ok_or_else(|| Fail::Failed("the browser came back without a code".into()))?;
    let reply = client
        .send(
            Method::POST,
            "/_rustybin/sso/token",
            &[],
            Body::Json(json!({ "code": code, "verifier": verifier })),
        )
        .await?;
    if reply.status == 401 {
        return Err(Fail::Auth(format!(
            "{url}: the sign-in's code was refused (used, expired, or not this sign-in's)"
        )));
    }
    let v = reply.ok()?.value();
    let token = v["token"]
        .as_str()
        .ok_or_else(|| Fail::Failed("the answer carries no token".into()))?
        .to_string();
    Ok((
        token,
        v["expires_at"].as_str().map(str::to_string),
        v["user"].as_str().map(str::to_string),
    ))
}

/// Open a URL with the system's opener (best effort: the address is
/// printed too).
pub(crate) fn open_browser(url: &str) {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    let _ = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Wait for the browser on the loopback listener: the query of the first
/// `GET /callback` whose `key` parameter (`cli_state` for single sign-on,
/// OAuth's `state` for an agent sign-in) is `expected`, answered with a page
/// that says what happened. Other requests (a favicon, a callback without
/// the value: another page or local user guessing the port) get an error
/// and the listener keeps waiting.
pub(crate) async fn loopback_answer(
    listener: tokio::net::TcpListener,
    key: &str,
    expected: &str,
) -> std::io::Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut buf = Vec::with_capacity(1024);
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 8192 {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let head = String::from_utf8_lossy(&buf);
        let target = head
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("GET "))
            .and_then(|l| l.split(' ').next())
            .unwrap_or("")
            .to_string();
        let Some(rest) = target.strip_prefix("/callback") else {
            let _ = stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        };
        let query = rest.strip_prefix('?').unwrap_or("").to_string();
        let carries = form_urlencoded::parse(query.as_bytes())
            .any(|(k, v)| k == key && crate::control_auth::constant_time_eq(&v, expected));
        if !carries {
            let _ = stream
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let failed = query.split('&').any(|p| p.starts_with("error="));
        let body = if failed {
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Rustybin</title></head><body><h1>The sign-in was refused</h1><p>The command line says why. You can close this window.</p></body></html>"
        } else {
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Rustybin</title></head><body><h1>Signed in</h1><p>The command line has the token. You can close this window.</p></body></html>"
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes()).await;
        let _ = stream.shutdown().await;
        return Ok(query);
    }
}

async fn sso(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "status" => get(ctx, "/_rustybin/sso/status", &[]).await,
        "test" => {
            let v = ctx
                .client()?
                .post("/_rustybin/sso/test", Body::None)
                .await?
                .ok()?
                .value();
            if ctx.json {
                ctx.emit(&v);
            } else if v["ok"] == json!(true) {
                ctx.say(&format!(
                    "Single sign-on is ready: {} answers its discovery document and {} key(s).\nRegister {} as the client's redirect URI.",
                    v["issuer"].as_str().unwrap_or("the provider"),
                    v["keys"].as_array().map(Vec::len).unwrap_or(0),
                    v["redirect_uri"].as_str().unwrap_or("")
                ));
                if let Some(unknown) = v["unknown_roles"].as_array().filter(|a| !a.is_empty()) {
                    let names: Vec<&str> = unknown.iter().filter_map(Value::as_str).collect();
                    ctx.say(&format!(
                        "The role map names roles this instance does not have: {}",
                        names.join(", ")
                    ));
                }
            }
            if v["ok"] == json!(true) {
                Ok(())
            } else {
                Err(Fail::Failed(format!(
                    "single sign-on is not working: {}",
                    v["error"].as_str().unwrap_or("not set up")
                )))
            }
        }
        other => Err(Fail::Usage(format!("sso {other}: not a command"))),
    }
}

async fn logout(ctx: &mut Ctx) -> Result<()> {
    let args = ctx.args.clone();
    let resolved = profiles::resolve(&ctx.env, &args)?;
    if resolved.token.is_some() {
        let client = ctx.client()?;
        let reply = client
            .send(Method::POST, "/_rustybin/logout", &[], Body::None)
            .await?;
        if !(200..=299).contains(&reply.status) && reply.status != 401 && reply.status != 404 {
            reply.ok()?;
        }
    }
    if let (Some(path), Some(name)) = (profiles::path(&ctx.env), resolved.profile.clone()) {
        let mut all = profiles::load(&path)?;
        let mut agent = false;
        if let Some(p) = all.profiles.get_mut(&name) {
            p.token = None;
            p.expires_at = None;
            p.user = None;
            agent = p.agent.take().is_some();
        }
        profiles::save(&path, &all)?;
        ctx.say(&format!("Signed out of {} (profile {name}).", resolved.url));
        if agent {
            ctx.say("The profile's agent sign-in is forgotten too. Its tokens work until they expire: revoke the agent in the console (AI agents) or with rustybin agents revoke.");
        }
    } else {
        ctx.say(&format!("Signed out of {}.", resolved.url));
    }
    Ok(())
}

// ── Instance and profiles ───────────────────────────────────────────

async fn instance(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "version" => get(ctx, "/_rustybin/version", &[]).await,
        "ready" => get(ctx, "/_rustybin/ready", &[]).await,
        "config" => get(ctx, "/_rustybin/config", &[]).await,
        "catalog" => {
            let reply = ctx.client()?.get("/_rustybin/catalog", &[]).await?;
            let mut v = reply.value();
            if let Some(cat) = ctx.args.value("--category") {
                if let Some(list) = v["endpoints"].as_array() {
                    let filtered: Vec<Value> = list
                        .iter()
                        .filter(|e| e["category"].as_str() == Some(cat))
                        .cloned()
                        .collect();
                    v = json!({ "endpoints": filtered });
                }
            }
            ctx.emit(&v);
            Ok(())
        }
        "metrics" => {
            let reply = ctx.client()?.get("/_rustybin/metrics", &[]).await?;
            ctx.raw(&reply.text);
            Ok(())
        }
        "usage" => get(ctx, "/_rustybin/usage", &[]).await,
        _ => Err(Fail::Usage(format!("instance {verb}: not implemented"))),
    }
}

fn profiles_cmd(ctx: &mut Ctx, verb: &str) -> Result<()> {
    let Some(path) = profiles::path(&ctx.env) else {
        return Err(Fail::Usage(
            "no home directory: set RUSTYBIN_CONFIG_HOME".into(),
        ));
    };
    let mut all = profiles::load(&path)?;
    match verb {
        "list" => {
            let rows: Vec<Value> = all
                .profiles
                .iter()
                .map(|(n, p)| {
                    json!({
                        "profile": n, "url": p.url, "user": p.user,
                        "token": if p.token.is_some() { "set" } else { "none" },
                        "expires_at": p.expires_at, "default": all.default.as_deref() == Some(n),
                    })
                })
                .collect();
            ctx.emit(&json!({ "file": path.display().to_string(), "default": all.default, "profiles": rows }));
            Ok(())
        }
        "use" => {
            let name = ctx.args.arg(0, "a profile name")?.to_string();
            if !all.profiles.contains_key(&name) {
                return Err(Fail::Usage(format!("no profile {name:?}")));
            }
            all.default = Some(name.clone());
            profiles::save(&path, &all)?;
            ctx.say(&format!("Default profile: {name}."));
            Ok(())
        }
        "remove" => {
            let name = ctx.args.arg(0, "a profile name")?.to_string();
            if all.profiles.remove(&name).is_none() {
                return Err(Fail::Usage(format!("no profile {name:?}")));
            }
            if all.default.as_deref() == Some(&name) {
                all.default = all.profiles.keys().next().cloned();
            }
            profiles::save(&path, &all)?;
            ctx.say(&format!("Profile {name} removed."));
            Ok(())
        }
        _ => Err(Fail::Usage(format!("profiles {verb}: not implemented"))),
    }
}

// ── Settings, sessions, password ────────────────────────────────────

async fn settings(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "get" => {
            let key = ctx.args.positional.first().cloned();
            let doc = ctx.client()?.get("/_rustybin/settings", &[]).await?.value();
            match key {
                Some(k) => {
                    let found = doc["settings"]
                        .as_array()
                        .and_then(|a| a.iter().find(|s| s["key"] == json!(k)))
                        .cloned()
                        .ok_or_else(|| Fail::Failed(format!("no setting {k}")))?;
                    ctx.emit(&found);
                }
                None => {
                    if ctx.json {
                        ctx.emit(&doc);
                    } else {
                        let rows: Vec<Value> = doc["settings"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .map(|s| {
                                        json!({
                                            "key": s["key"],
                                            "value": if s["secret"] == json!(true) { json!(if s["value"]["set"] == json!(true) { "(set)" } else { "(not set)" }) } else { s["value"].clone() },
                                            "source": s["source"],
                                            "locked": s["locked"],
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        ctx.emit(&json!({ "persisted": doc["persisted"], "locked": doc["locked"], "settings": rows }));
                    }
                }
            }
            Ok(())
        }
        "set" => {
            let key = ctx.args.arg(0, "a setting key")?.to_string();
            let value = loose_json(ctx.args.arg(1, "a value")?);
            let body = json!({ "values": { key: value } });
            let reply = ctx
                .client()?
                .patch("/_rustybin/settings", Body::Json(body))
                .await?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "set-secret" => {
            let key = ctx.args.arg(0, "a setting key")?.to_string();
            let secret = ctx.secret(&key, ctx.args.has("--stdin"), 0)?;
            let body = json!({ "secrets": { key: secret } });
            let reply = ctx
                .client()?
                .patch("/_rustybin/settings", Body::Json(body))
                .await?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "unset" => {
            let key = ctx.args.arg(0, "a setting key")?.to_string();
            delete(ctx, &format!("/_rustybin/settings/{}", enc(&key)), &[]).await
        }
        "schema" => {
            let doc = ctx.client()?.get("/_rustybin/settings", &[]).await?.value();
            let rows: Vec<Value> = doc["settings"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|s| {
                            json!({
                                "key": s["key"], "group": s["group"],
                                "kind": s["schema"]["type"].as_str().unwrap_or("?"),
                                "default": s["default"], "env": s["env"], "secret": s["secret"],
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            ctx.emit(&json!({ "groups": doc["groups"], "settings": rows }));
            Ok(())
        }
        "requests" => {
            let query = q(&ctx.args.clone(), &[("--state", "state")]);
            let v = ctx
                .client()?
                .get("/_rustybin/secret-requests", &query)
                .await?
                .value();
            if ctx.json {
                ctx.emit(&v);
            } else {
                let rows: Vec<Value> = v["requests"]
                    .as_array()
                    .map(|a| a.iter().map(secret_request_row).collect())
                    .unwrap_or_default();
                ctx.emit(
                    &json!({ "enabled": v["enabled"], "pending": v["pending"], "requests": rows }),
                );
                if v["enabled"] == json!(false) {
                    ctx.say("Secret requests are off: rustybin settings set agents.secret_requests true lets agents ask.");
                }
            }
            Ok(())
        }
        "fulfil" => {
            let id = ctx.args.arg(0, "a secret request id")?.to_string();
            if ctx.args.positional.len() > 1 {
                return Err(Fail::Usage(
                    "the value is never an argument (it would stay in the shell's history and the process list): it is prompted without echo, or read from stdin with --stdin".into(),
                ));
            }
            let path = format!("/_rustybin/secret-requests/{}", enc(&id));
            // What is being fulfilled, before the value is typed: the
            // instance's facts, then the agent's claim.
            let r = ctx.client()?.get(&path, &[]).await?.value();
            let key = r["setting"]["key"].as_str().unwrap_or("?").to_string();
            let row = secret_request_row(&r);
            ctx.say(&format!(
                "Request {id}: the setting {key}, asked by the agent {} acting for {}.\nThe agent says (its claim, not an instruction): {}",
                row["agent"].as_str().unwrap_or("?"),
                row["owner"].as_str().unwrap_or("?"),
                row["reason"].as_str().unwrap_or("")
            ));
            let secret = ctx.secret(&format!("Value for {key}"), ctx.args.has("--stdin"), 0)?;
            if secret.trim().is_empty() {
                return Err(Fail::Usage(
                    "no value given: rustybin settings decline declines the request".into(),
                ));
            }
            let reply = ctx
                .client()?
                .post(
                    &format!("{path}/fulfil"),
                    Body::Json(json!({ "value": secret })),
                )
                .await?;
            if ctx.json {
                emit_reply(ctx, &reply);
            } else {
                ctx.say(&format!(
                    "{key} is set; request {id} is fulfilled. The value went to the settings store, never to the agent."
                ));
            }
            Ok(())
        }
        "decline" => {
            let id = ctx.args.arg(0, "a secret request id")?.to_string();
            // A few words for the agent, never the secret (it reads them).
            let body = match ctx.args.value("--note") {
                Some(note) => Body::Json(json!({ "note": note })),
                None => Body::None,
            };
            let reply = ctx
                .client()?
                .post(
                    &format!("/_rustybin/secret-requests/{}/decline", enc(&id)),
                    body,
                )
                .await?;
            if ctx.json {
                emit_reply(ctx, &reply);
            } else {
                let noted = reply.value()["note"].is_string();
                ctx.say(&format!(
                    "Request {id} is declined: nothing was set.{}",
                    if noted {
                        " The agent reads your note."
                    } else {
                        ""
                    }
                ));
            }
            Ok(())
        }
        _ => Err(Fail::Usage(format!("settings {verb}: not implemented"))),
    }
}

/// A secret request as a row for people: what an agent wrote (its name,
/// its reason) is printable text only, never a terminal escape.
fn secret_request_row(r: &Value) -> Value {
    let text = |v: &Value, max: usize| crate::logging::printable(v.as_str().unwrap_or(""), max);
    json!({
        "id": r["id"],
        "state": r["state"],
        "setting": r["setting"]["key"],
        "agent": format!("{} ({})", text(&r["agent"]["untrusted_name"], 80), r["agent"]["id"].as_str().unwrap_or("?")),
        "owner": text(&r["agent"]["owner_label"], 120),
        "reason": text(&r["untrusted_reason"], 300),
        "expires_in": r["expires_in"],
        "note": text(&r["note"], 300),
    })
}

async fn sessions(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "list" => get(ctx, "/_rustybin/sessions", &[]).await,
        "end" => {
            let id = ctx.args.arg(0, "a session id")?.to_string();
            delete(ctx, &format!("/_rustybin/sessions/{}", enc(&id)), &[]).await
        }
        "end-all" => delete(ctx, "/_rustybin/sessions", &[]).await,
        _ => Err(Fail::Usage(format!("sessions {verb}: not implemented"))),
    }
}

/// `egress check`: the answer, and exit 1 when the policy would refuse.
async fn egress_check(ctx: &mut Ctx) -> Result<()> {
    let url = ctx
        .args
        .positional
        .first()
        .cloned()
        .ok_or_else(|| Fail::Usage("name a url to check".into()))?;
    let mut query = vec![("url", url)];
    if let Some(p) = ctx.args.value("--purpose") {
        query.push(("purpose", p.to_string()));
    }
    let reply = ctx.client()?.get("/_rustybin/egress/check", &query).await?;
    let reply = reply.ok()?;
    let v = reply.value();
    ctx.emit(&v);
    if v["allowed"] == json!(true) {
        return Ok(());
    }
    Err(Fail::Failed("the outbound policy would refuse it".into()))
}

/// The hash of a password, on this machine: nothing is sent anywhere.
fn password_hash(ctx: &mut Ctx) -> Result<()> {
    let plain = ctx.secret("Password", ctx.args.has("--stdin"), 0)?;
    let chars = plain.chars().count();
    if !(crate::login::MIN_PASSWORD_CHARS..=crate::login::MAX_PASSWORD_CHARS).contains(&chars) {
        return Err(Fail::Usage(format!(
            "a password has {} to {} characters",
            crate::login::MIN_PASSWORD_CHARS,
            crate::login::MAX_PASSWORD_CHARS
        )));
    }
    let hash = crate::login::hash_password(&plain).map_err(Fail::Usage)?;
    ctx.raw(&hash);
    Ok(())
}

async fn password(ctx: &mut Ctx, path: &str) -> Result<()> {
    let from_stdin = ctx.args.has("--stdin");
    let current = ctx.secret("Current password", from_stdin, 0)?;
    let new = ctx.secret("New password", from_stdin, 1)?;
    let mut body = json!({ "current_password": current, "new_password": new });
    if let Some(u) = ctx.args.value("--user") {
        body["username"] = json!(u);
    }
    let reply = ctx.client()?.put(path, Body::Json(body)).await?.ok()?;
    let v = reply.value();
    // The answer carries a fresh token: keep it in the profile.
    if let Some(token) = v["token"].as_str() {
        let args = ctx.args.clone();
        if let (Some(path), Ok(resolved)) =
            (profiles::path(&ctx.env), profiles::resolve(&ctx.env, &args))
        {
            if let Some(name) = resolved.profile {
                let mut all = profiles::load(&path)?;
                if let Some(p) = all.profiles.get_mut(&name) {
                    p.token = Some(token.to_string());
                    p.expires_at = v["expires_at"].as_str().map(str::to_string);
                    p.user = v["user"].as_str().map(str::to_string);
                }
                profiles::save(&path, &all)?;
            }
        }
    }
    if ctx.json {
        ctx.emit(&v);
    } else {
        ctx.say("Password changed; the other sessions ended.");
    }
    Ok(())
}

// ── Users, roles, your account ──────────────────────────────────────

/// The names of a repeatable flag, each also comma separated.
fn names_of(ctx: &Ctx, flag: &str) -> Vec<String> {
    ctx.args
        .values(flag)
        .iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// `--email ""` clears the email (null), any other value sets it.
fn email_value(v: &str) -> Value {
    if v.trim().is_empty() {
        Value::Null
    } else {
        json!(v)
    }
}

/// Print an invitation: the link first (it is shown once).
fn emit_invitation(ctx: &mut Ctx, v: &Value) {
    if ctx.json {
        ctx.emit(v);
        return;
    }
    let name = v["user"]["name"].as_str().unwrap_or("");
    if v["invitation"].is_null() {
        ctx.say(&format!(
            "{name} signs in with single sign-on: no link and no password; their groups add roles at every sign-in."
        ));
        return;
    }
    ctx.say(&format!(
        "Invitation for {name} (shown once, valid until {}):\n{}\nSend it to them: they open it and set their own password.",
        v["invitation"]["expires_at"].as_str().unwrap_or(""),
        v["invitation"]["url"].as_str().unwrap_or("")
    ));
}

async fn users(ctx: &mut Ctx, verb: &str) -> Result<()> {
    if verb == "list" {
        let v = ctx
            .client()?
            .get("/_rustybin/users", &[])
            .await?
            .ok()?
            .value();
        if ctx.json {
            ctx.emit(&v);
        } else {
            let rows: Vec<Value> = v["users"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .map(|u| {
                            json!({
                                "name": u["name"], "display_name": u["display_name"],
                                "roles": u["roles"].as_array().map(|r| r.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default(),
                                "status": u["status"], "last_sign_in": u["last_sign_in"], "id": u["id"],
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            ctx.emit(&json!({ "count": v["count"], "users": rows }));
        }
        return Ok(());
    }
    let user = ctx
        .args
        .arg(0, "a user (an id or a sign-in name)")?
        .to_string();
    let path = format!("/_rustybin/users/{}", enc(&user));
    match verb {
        "invite" if ctx.args.has("--reset") => {
            let v = ctx
                .client()?
                .post(&format!("{path}/invite"), Body::None)
                .await?
                .ok()?
                .value();
            emit_invitation(ctx, &v);
            Ok(())
        }
        "invite" => {
            let roles = names_of(ctx, "--role");
            let projects = project_roles(ctx, "--project")?;
            if roles.is_empty() && projects.is_empty() {
                return Err(Fail::Usage(
                    "--role (or --project P=role) is required (rustybin roles list shows them)"
                        .into(),
                ));
            }
            let mut body = json!({ "name": user, "roles": roles });
            if !projects.is_empty() {
                body["projects"] = json!(projects
                    .iter()
                    .map(|(p, r)| json!({ "project": p, "roles": r }))
                    .collect::<Vec<_>>());
            }
            if ctx.args.has("--sso") {
                body["sso"] = json!(true);
            }
            if let Some(d) = ctx.args.value("--display-name") {
                body["display_name"] = json!(d);
            }
            if let Some(e) = ctx.args.value("--email") {
                body["email"] = json!(e);
            }
            let v = ctx
                .client()?
                .post("/_rustybin/users", Body::Json(body))
                .await?
                .ok()?
                .value();
            emit_invitation(ctx, &v);
            Ok(())
        }
        "show" => get(ctx, &path, &[]).await,
        "update" => {
            let mut body = json!({});
            let roles = names_of(ctx, "--role");
            if !roles.is_empty() {
                body["roles"] = json!(roles);
            }
            let pinned = names_of(ctx, "--pin-role");
            if !pinned.is_empty() {
                body["pinned_roles"] = json!(pinned);
            }
            if let Some(d) = ctx.args.value("--display-name") {
                body["display_name"] = json!(d);
            }
            if let Some(e) = ctx.args.value("--email") {
                body["email"] = email_value(e);
            }
            if body.as_object().is_some_and(|m| m.is_empty()) {
                return Err(Fail::Usage(
                    "nothing to change: --role, --pin-role, --display-name or --email".into(),
                ));
            }
            let reply = ctx.client()?.patch(&path, Body::Json(body)).await?.ok()?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "disable" | "enable" => {
            let body = json!({ "disabled": verb == "disable" });
            let reply = ctx.client()?.patch(&path, Body::Json(body)).await?.ok()?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "sessions" if ctx.args.has("--end") => delete(ctx, &format!("{path}/sessions"), &[]).await,
        "sessions" => get(ctx, &format!("{path}/sessions"), &[]).await,
        "delete" => delete(ctx, &path, &[]).await,
        _ => Err(Fail::Usage(format!("users {verb}: not implemented"))),
    }
}

async fn roles(ctx: &mut Ctx, verb: &str) -> Result<()> {
    if verb == "list" {
        let v = ctx
            .client()?
            .get("/_rustybin/roles", &[])
            .await?
            .ok()?
            .value();
        if ctx.json {
            ctx.emit(&v);
        } else {
            let rows: Vec<Value> = v["roles"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .map(|r| {
                            json!({
                                "name": r["name"],
                                "kind": if r["built_in"] == json!(true) { "built-in" } else { "custom" },
                                "privileged": r["privileged"],
                                "permissions": r["permissions"].as_array().map(Vec::len).unwrap_or(0),
                                "users": r["users"],
                                "description": r["description"],
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            ctx.emit(&json!({ "roles": rows }));
        }
        return Ok(());
    }
    let name = ctx.args.arg(0, "a role name")?.to_string();
    let path = format!("/_rustybin/roles/{}", enc(&name));
    match verb {
        "show" => get(ctx, &path, &[]).await,
        "save" => {
            let permissions = names_of(ctx, "--permission");
            if permissions.is_empty() {
                return Err(Fail::Usage(
                    "--permission is required (rustybin roles list shows the table)".into(),
                ));
            }
            let mut body = json!({ "permissions": permissions });
            if let Some(d) = ctx.args.value("--description") {
                body["description"] = json!(d);
            }
            let reply = ctx.client()?.put(&path, Body::Json(body)).await?.ok()?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "delete" => delete(ctx, &path, &[]).await,
        _ => Err(Fail::Usage(format!("roles {verb}: not implemented"))),
    }
}

/// `NAME=role+role` values of a repeatable flag (`--project`, `--member`).
fn project_roles(ctx: &Ctx, flag: &str) -> Result<Vec<(String, Vec<String>)>> {
    let mut out = Vec::new();
    for v in ctx.args.values(flag) {
        let (name, roles) = v
            .split_once('=')
            .ok_or_else(|| Fail::Usage(format!("{flag} {v}: NAME=role+role")))?;
        let roles: Vec<String> = roles
            .split(['+', ',', ' '])
            .filter(|r| !r.is_empty())
            .map(str::to_string)
            .collect();
        if name.trim().is_empty() || roles.is_empty() {
            return Err(Fail::Usage(format!("{flag} {v}: NAME=role+role")));
        }
        out.push((name.trim().to_string(), roles));
    }
    Ok(out)
}

/// The `--project` filter as a query.
fn project_query(ctx: &Ctx) -> Vec<(&'static str, String)> {
    ctx.args
        .value("--project")
        .map(|p| vec![("project", p.to_string())])
        .unwrap_or_default()
}

async fn projects(ctx: &mut Ctx, verb: &str) -> Result<()> {
    if verb == "list" {
        let v = ctx
            .client()?
            .get("/_rustybin/projects", &[])
            .await?
            .ok()?
            .value();
        if ctx.json {
            ctx.emit(&v);
        } else {
            let join = |v: &Value| {
                v.as_array()
                    .map(|r| {
                        r.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default()
            };
            let rows: Vec<Value> = v["projects"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .map(|p| {
                            json!({
                                "name": p["name"],
                                "your_roles": join(&p["your_roles"]),
                                "members": p["member_count"],
                                "mocks": p["resources"]["mocks"].as_array().map(Vec::len).unwrap_or(0),
                                "tests": p["resources"]["tests"].as_array().map(Vec::len).unwrap_or(0),
                                "checkpoints": p["resources"]["checkpoints"].as_array().map(Vec::len).unwrap_or(0),
                                "description": p["description"],
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            ctx.emit(&json!({ "count": v["count"], "projects": rows }));
        }
        return Ok(());
    }
    let name = ctx.args.arg(0, "a project name")?.to_string();
    let path = format!("/_rustybin/projects/{}", enc(&name));
    match verb {
        "show" => get(ctx, &path, &[]).await,
        "save" => {
            let mut body = json!({});
            if let Some(d) = ctx.args.value("--description") {
                body["description"] = json!(d);
            }
            let members = project_roles(ctx, "--member")?;
            if !members.is_empty() || ctx.args.has("--no-members") {
                body["members"] = json!(members
                    .iter()
                    .map(|(u, r)| json!({ "user": u, "roles": r }))
                    .collect::<Vec<_>>());
            }
            let reply = ctx.client()?.put(&path, Body::Json(body)).await?.ok()?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "delete" => {
            let query: Vec<(&str, String)> = ctx
                .args
                .value("--move-to")
                .map(|m| vec![("move_to", m.to_string())])
                .unwrap_or_default();
            delete(ctx, &path, &query).await
        }
        _ => Err(Fail::Usage(format!("projects {verb}: not implemented"))),
    }
}

async fn me(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "" => get(ctx, "/_rustybin/me", &[]).await,
        "update" => {
            let mut body = json!({});
            if let Some(d) = ctx.args.value("--display-name") {
                body["display_name"] = json!(d);
            }
            if let Some(e) = ctx.args.value("--email") {
                body["email"] = email_value(e);
            }
            if body.as_object().is_some_and(|m| m.is_empty()) {
                return Err(Fail::Usage(
                    "nothing to change: --display-name or --email".into(),
                ));
            }
            let reply = ctx
                .client()?
                .patch("/_rustybin/me", Body::Json(body))
                .await?
                .ok()?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "sessions" if ctx.args.has("--end-all") => delete(ctx, "/_rustybin/me/sessions", &[]).await,
        "sessions" => get(ctx, "/_rustybin/me/sessions", &[]).await,
        "end-session" => {
            let id = ctx.args.arg(0, "a session id")?.to_string();
            delete(ctx, &format!("/_rustybin/me/sessions/{}", enc(&id)), &[]).await
        }
        _ => Err(Fail::Usage(format!("me {verb}: not implemented"))),
    }
}

// ── Mocks ───────────────────────────────────────────────────────────

/// The draft revision an edit is made from: `--expect-revision`, else the
/// mock's draft revision read now (an instance with `mocks.require_revision`
/// accepts no edit without one).
async fn expected_revision(ctx: &mut Ctx, base: &str) -> Result<u64> {
    if let Some(r) = ctx.args.value("--expect-revision") {
        return r.parse().map_err(|_| {
            Fail::Usage("--expect-revision needs a number (the mock's draft_revision)".into())
        });
    }
    let v = ctx.client()?.get(base, &[]).await?.value();
    v["draft_revision"]
        .as_u64()
        .ok_or_else(|| Fail::Failed("the mock's draft_revision could not be read".into()))
}

fn mock_settings_body(ctx: &Ctx) -> Result<Map<String, Value>> {
    let mut m = Map::new();
    if let Some(mode) = ctx.args.value("--mode") {
        m.insert("responses".into(), json!({ "mode": mode }));
    }
    if let Some(extra) = ctx.json_flag("--settings")? {
        match extra {
            Value::Object(o) => {
                for (k, v) in o {
                    m.insert(k, v);
                }
            }
            _ => return Err(Fail::Usage("--settings must be a JSON object".into())),
        }
    }
    Ok(m)
}

async fn mocks(ctx: &mut Ctx, verb: &str) -> Result<()> {
    let name = ctx.args.positional.first().cloned().unwrap_or_default();
    let base = format!("/_rustybin/mocks/{}", enc(&name));
    let need_name = |ctx: &Ctx| ctx.args.arg(0, "a mock name").map(|_| ());
    match verb {
        "list" => {
            let mut query = if ctx.args.has("--all") {
                vec![("owner", "all".to_string())]
            } else {
                Vec::new()
            };
            query.extend(project_query(ctx));
            let v = ctx.client()?.get("/_rustybin/mocks", &query).await?.value();
            if ctx.json {
                ctx.emit(&v);
            } else {
                let rows: Vec<Value> = v["mocks"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|m| json!({ "name": m["name"], "state": m["state"], "operations": m["operations"], "requests": m["requests_total"], "origin": m["origin"], "url": m["url"] }))
                            .collect()
                    })
                    .unwrap_or_default();
                ctx.emit(&json!({ "mocks": rows, "load_errors": v["load_errors"] }));
            }
            Ok(())
        }
        "move" => {
            need_name(ctx)?;
            let to = ctx
                .args
                .value("--project")
                .ok_or_else(|| Fail::Usage("--project: a project, or instance".into()))?
                .to_string();
            let reply = ctx
                .client()?
                .patch(&base, Body::Json(json!({ "project": to })))
                .await?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "get" => {
            need_name(ctx)?;
            get(ctx, &base, &[]).await
        }
        "create" => {
            need_name(ctx)?;
            let mut body = Map::new();
            body.insert("name".into(), json!(name));
            match (ctx.args.value("--spec"), ctx.args.value("--spec-url")) {
                (Some(path), None) => {
                    let path = path.to_string();
                    body.insert("spec".into(), json!(ctx.input(&path)?));
                }
                (None, Some(url)) => {
                    body.insert("spec_url".into(), json!(url));
                }
                _ => {
                    return Err(Fail::Usage(
                        "--spec FILE or --spec-url URL is required, one of them".into(),
                    ))
                }
            }
            body.insert("enabled".into(), json!(!ctx.args.has("--disabled")));
            body.insert("deploy".into(), json!(!ctx.args.has("--no-deploy")));
            if ctx.args.has("--strip-server-prefix") {
                body.insert("strip_server_prefix".into(), json!(true));
            }
            if let Some(p) = ctx.args.value("--project") {
                body.insert("project".into(), json!(p));
            }
            for (k, v) in mock_settings_body(ctx)? {
                body.insert(k, v);
            }
            post(ctx, "/_rustybin/mocks", Body::Json(Value::Object(body))).await
        }
        "import" => {
            let spec_path = ctx
                .args
                .value("--spec")
                .ok_or_else(|| Fail::Usage("--spec FILE is required".into()))?
                .to_string();
            let spec = ctx.input(&spec_path)?;
            let ct = content_type_for(&spec_path, &spec);
            post(ctx, "/_rustybin/mocks/import", Body::Text(spec, ct)).await
        }
        "update" => {
            need_name(ctx)?;
            let mut body = mock_settings_body(ctx)?;
            if let Some(p) = ctx.args.value("--spec").map(str::to_string) {
                body.insert("spec".into(), json!(ctx.input(&p)?));
            }
            if ctx.args.has("--no-deploy") {
                body.insert("deploy".into(), json!(false));
            }
            if ctx.args.has("--force") {
                body.insert("force".into(), json!(true));
            }
            if body.is_empty() {
                return Err(Fail::Usage(
                    "nothing to change: --spec, --mode or --settings".into(),
                ));
            }
            body.insert(
                "expected_revision".into(),
                json!(expected_revision(ctx, &base).await?),
            );
            let reply = ctx
                .client()?
                .patch(&base, Body::Json(Value::Object(body)))
                .await?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "deploy" => {
            need_name(ctx)?;
            let path = if ctx.args.has("--force") {
                format!("{base}/deploy?force=true")
            } else {
                format!("{base}/deploy")
            };
            // Only a revision named here: a deploy without one publishes the
            // draft as it is.
            let body = match ctx.args.value("--expect-revision") {
                Some(_) => {
                    Body::Json(json!({ "expected_revision": expected_revision(ctx, &base).await? }))
                }
                None => Body::None,
            };
            post(ctx, &path, body).await
        }
        "rollback" => {
            need_name(ctx)?;
            let body = match ctx.args.value("--revision") {
                Some(r) => {
                    json!({ "revision": r.parse::<u64>().map_err(|_| Fail::Usage("--revision needs a number".into()))? })
                }
                None => json!({}),
            };
            post(ctx, &format!("{base}/rollback"), Body::Json(body)).await
        }
        "enable" | "disable" | "duplicate" => {
            need_name(ctx)?;
            let body = if verb == "duplicate" {
                match ctx.args.value("--name") {
                    Some(n) => Body::Json(json!({ "name": n })),
                    None => Body::Json(json!({})),
                }
            } else {
                Body::None
            };
            post(ctx, &format!("{base}/{verb}"), body).await
        }
        "delete" => {
            need_name(ctx)?;
            delete(ctx, &base, &[]).await
        }
        "check" => {
            need_name(ctx)?;
            get(ctx, &format!("{base}/report"), &[]).await
        }
        "coverage" => {
            need_name(ctx)?;
            get(ctx, &format!("{base}/coverage"), &[]).await
        }
        "drift-runs" => {
            need_name(ctx)?;
            get(ctx, &format!("{base}/drift"), &[]).await
        }
        "drift" => {
            need_name(ctx)?;
            let mut body = json!({});
            if let Some(u) = ctx.args.value("--upstream") {
                body["upstream"] = json!(u);
            }
            let mut headers = serde_json::Map::new();
            let mut variables = serde_json::Map::new();
            for h in ctx.args.values("--header") {
                let (k, v) = h
                    .split_once(':')
                    .ok_or_else(|| Fail::Usage(format!("--header {h:?}: 'Name: value'")))?;
                let v = v.trim().to_string();
                // {{VAR}} from this shell's RUSTYBIN_TEST_<VAR>: the value
                // goes to the instance with the run, never on a command line.
                let mut rest = v.as_str();
                while let Some(i) = rest.find("{{") {
                    let after = &rest[i + 2..];
                    let Some(j) = after.find("}}") else { break };
                    let name = after[..j].trim().to_string();
                    if let Ok(value) = std::env::var(format!("RUSTYBIN_TEST_{name}")) {
                        variables.insert(name, json!(value));
                    }
                    rest = &after[j + 2..];
                }
                headers.insert(k.trim().to_string(), json!(v));
            }
            if !headers.is_empty() {
                body["headers"] = Value::Object(headers);
            }
            if !variables.is_empty() {
                body["variables"] = Value::Object(variables);
            }
            let ops = ctx.args.values("--operation");
            if !ops.is_empty() {
                body["operations"] = json!(ops);
            }
            let junit = ctx.args.has("--junit");
            let path = if junit {
                format!("{base}/drift?format=junit")
            } else {
                format!("{base}/drift")
            };
            let reply = ctx.client()?.post(&path, Body::Json(body)).await?.ok()?;
            if junit {
                ctx.raw(&reply.text);
                let drifted = reply.text.contains("<failure");
                return if drifted {
                    Err(Fail::Failed("the mock drifted from the real API".into()))
                } else {
                    Ok(())
                };
            }
            let v = reply.value();
            ctx.emit(&v);
            if v["passed"] != true {
                return Err(Fail::Failed(
                    "the mock drifted from the real API (see above)".into(),
                ));
            }
            Ok(())
        }
        "revision" => {
            need_name(ctx)?;
            let rev = ctx.args.arg(1, "a revision number")?.to_string();
            let rev: u64 = rev
                .parse()
                .map_err(|_| Fail::Usage(format!("{rev:?}: a revision is a number")))?;
            let v = ctx
                .client()?
                .get(&format!("{base}/revisions/{rev}"), &[])
                .await?
                .value();
            if ctx.args.has("--spec") {
                ctx.raw(v["spec"]["text"].as_str().unwrap_or(""));
            } else if ctx.json {
                ctx.emit(&v);
            } else {
                let kind = match (v["draft"] == json!(true), v["live"] == json!(true)) {
                    (true, true) => "the draft, live",
                    (true, false) => "the draft",
                    (false, true) => "live",
                    (false, false) => "kept",
                };
                let changes: Vec<&str> = v["changes_to_draft"]
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                ctx.say(&format!(
                    "revision {rev} of {name} ({kind}), created {}",
                    v["created_at"].as_str().unwrap_or("?")
                ));
                ctx.emit(&json!({
                    "draft revision": v["current_draft_revision"],
                    "changed in the draft since": if changes.is_empty() { json!("nothing") } else { json!(changes.join(", ")) },
                    "spec": format!(
                        "{} {} (OpenAPI {}, {}, {} operation(s); --spec prints it)",
                        v["spec"]["title"].as_str().unwrap_or(""),
                        v["spec"]["version"].as_str().unwrap_or(""),
                        v["spec"]["openapi"].as_str().unwrap_or("?"),
                        v["spec"]["format"].as_str().unwrap_or("?"),
                        v["operations"].as_array().map(|a| a.len()).unwrap_or(0)
                    ),
                    "settings": v["settings"],
                }));
            }
            Ok(())
        }
        "spec" => {
            need_name(ctx)?;
            let reply = ctx.client()?.get(&format!("{base}/spec"), &[]).await?;
            ctx.raw(&reply.text);
            Ok(())
        }
        "export" => {
            let path = if name.is_empty() {
                "/_rustybin/mocks/export".to_string()
            } else {
                format!("{base}/export")
            };
            let reply = ctx.client()?.get(&path, &[]).await?;
            ctx.raw(&reply.text);
            Ok(())
        }
        "apply" => {
            let file = ctx.args.arg(0, "a rustybin.yaml file")?.to_string();
            let doc =
                crate::mocks::cli::inline_file(std::path::Path::new(&file)).map_err(Fail::Usage)?;
            let path = if ctx.args.has("--prune") {
                "/_rustybin/mocks/apply?prune=true"
            } else {
                "/_rustybin/mocks/apply"
            };
            if ctx.args.has("--prune") {
                ctx.confirm("apply with prune (API mocks the file does not name are deleted)?")?;
            }
            post(ctx, path, Body::Json(doc)).await
        }
        "reload" => post(ctx, "/_rustybin/mocks/reload", Body::None).await,
        "schema" => get(ctx, "/_rustybin/mocks/config.schema.json", &[]).await,
        "sample" => {
            need_name(ctx)?;
            let op = ctx.args.arg(1, "an operationId")?.to_string();
            let query = q(
                &ctx.args.clone(),
                &[
                    ("--status", "status"),
                    ("--seed", "seed"),
                    ("--mode", "mode"),
                    ("--example", "example"),
                ],
            );
            get(
                ctx,
                &format!("{base}/operations/{}/sample", enc(&op)),
                &query,
            )
            .await
        }
        "validate" => {
            need_name(ctx)?;
            let op = ctx.args.arg(1, "an operationId")?.to_string();
            let body_text = ctx
                .args
                .value("--body")
                .ok_or_else(|| Fail::Usage("--body FILE is required".into()))?
                .to_string();
            let body = loose_json(&ctx.input(&body_text)?);
            let mut req = json!({ "body": body });
            if let Some(k) = ctx.args.value("--kind") {
                req["kind"] = json!(k);
            }
            if let Some(s) = ctx.args.value("--status") {
                req["status"] = json!(s
                    .parse::<u16>()
                    .map_err(|_| Fail::Usage("--status needs a number".into()))?);
            }
            post(
                ctx,
                &format!("{base}/operations/{}/validate", enc(&op)),
                Body::Json(req),
            )
            .await
        }
        "operation" => {
            need_name(ctx)?;
            let op = ctx.args.arg(1, "an operationId")?.to_string();
            get(ctx, &format!("{base}/operations/{}", enc(&op)), &[]).await
        }
        "set-operation" => {
            need_name(ctx)?;
            let op = ctx.args.arg(1, "an operationId")?.to_string();
            let mut settings = ctx
                .json_flag("--settings")?
                .ok_or_else(|| Fail::Usage("--settings JSON is required".into()))?;
            let rev = expected_revision(ctx, &base).await?;
            match &mut settings {
                Value::Object(m) => {
                    m.insert("expected_revision".into(), json!(rev));
                }
                // null clears the operation's settings.
                Value::Null => settings = json!({ "expected_revision": rev }),
                _ => return Err(Fail::Usage("--settings must be a JSON object".into())),
            }
            let reply = ctx
                .client()?
                .put(
                    &format!("{base}/operations/{}", enc(&op)),
                    Body::Json(settings),
                )
                .await?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "test-rules" => {
            need_name(ctx)?;
            let op = ctx.args.arg(1, "an operationId")?.to_string();
            let mut body = json!({});
            if let Some(rules) = ctx.json_flag("--rules")? {
                body["rules"] = rules;
            }
            if let Some(n) = ctx.args.value("--request") {
                body["request"] = json!(n
                    .parse::<u64>()
                    .map_err(|_| Fail::Usage("--request needs a number".into()))?);
            }
            post(
                ctx,
                &format!("{base}/operations/{}/rules/test", enc(&op)),
                Body::Json(body),
            )
            .await
        }
        "traffic" => {
            need_name(ctx)?;
            let query = q(&ctx.args.clone(), &[("--limit", "limit")]);
            get(ctx, &format!("{base}/requests"), &query).await
        }
        "traffic-get" => {
            need_name(ctx)?;
            let n = ctx.args.arg(1, "a request number")?.to_string();
            get(ctx, &format!("{base}/requests/{}", enc(&n)), &[]).await
        }
        "traffic-tail" => {
            need_name(ctx)?;
            tail(ctx, &format!("{base}/requests/stream")).await
        }
        "traffic-clear" => {
            need_name(ctx)?;
            delete(ctx, &format!("{base}/requests"), &[]).await
        }
        "channels" => {
            need_name(ctx)?;
            get(ctx, &format!("{base}/channels"), &[]).await
        }
        "events" => {
            need_name(ctx)?;
            let channel = ctx.args.arg(1, "a channel id")?.to_string();
            let path = format!("{base}/channels/{}/messages", enc(&channel));
            let limit = ctx.args.value("--limit").unwrap_or("20").to_string();
            if !ctx.args.has("--follow") {
                return get(ctx, &path, &[("limit", limit)]).await;
            }
            // Following: the list is asked again for what came after the
            // last message seen, oldest first on the screen.
            let (max, timeout) = tail_params(ctx)?;
            let started = std::time::Instant::now();
            let json = ctx.json;
            let mut after: Option<u64> = None;
            let mut printed = 0usize;
            loop {
                let mut query = vec![("limit", "100".to_string())];
                if let Some(a) = after {
                    query.push(("after", a.to_string()));
                }
                let v = ctx.client()?.get(&path, &query).await?.value();
                let mut list: Vec<Value> = v["messages"].as_array().cloned().unwrap_or_default();
                list.reverse();
                // The first look only finds where the channel is now.
                let fresh = after.is_some();
                after = Some(v["last"].as_u64().unwrap_or(0).max(after.unwrap_or(0)));
                for m in list.iter().filter(|_| fresh) {
                    if json {
                        ctx.raw(&m.to_string());
                    } else {
                        let body = if m["payload"].is_null() {
                            m["text"].as_str().unwrap_or_default().to_string()
                        } else {
                            m["payload"].to_string()
                        };
                        ctx.raw(&format!(
                            "{} {:8} {:16} {}{}",
                            m["timestamp"].as_str().unwrap_or_default(),
                            m["direction"].as_str().unwrap_or_default(),
                            m["message"].as_str().unwrap_or("-"),
                            if m["valid"] == false {
                                "(not valid) "
                            } else {
                                ""
                            },
                            body
                        ));
                    }
                    printed += 1;
                    if max.is_some_and(|m| printed >= m) {
                        return Ok(());
                    }
                }
                if started.elapsed() >= timeout {
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
        "publish" => {
            need_name(ctx)?;
            let channel = ctx.args.arg(1, "a channel id")?.to_string();
            let mut body = json!({});
            if let Some(d) = ctx.args.value("--data") {
                let text = match std::fs::read_to_string(d) {
                    Ok(t) => t,
                    Err(_) => d.to_string(),
                };
                body["payload"] = serde_json::from_str(&text)
                    .map_err(|e| Fail::Usage(format!("--data is not JSON: {e}")))?;
            }
            if let Some(c) = ctx.args.value("--count") {
                body["count"] = json!(c
                    .parse::<u32>()
                    .map_err(|_| Fail::Usage("--count is a number from 1 to 20".into()))?);
            }
            let path = format!("{base}/channels/{}/publish", enc(&channel));
            post(ctx, &path, Body::Json(body)).await
        }
        "subscribers" => {
            need_name(ctx)?;
            get(ctx, &format!("{base}/subscribers"), &[]).await
        }
        "subscribe" => {
            need_name(ctx)?;
            let channel = ctx.args.arg(1, "a channel id")?.to_string();
            let url = ctx
                .args
                .value("--webhook")
                .ok_or_else(|| Fail::Usage("--webhook URL is required".into()))?;
            let mut body = json!({ "channel": channel, "url": url });
            if let Some(t) = ctx.args.value("--ttl") {
                body["ttl_seconds"] = json!(t
                    .parse::<u64>()
                    .map_err(|_| Fail::Usage("--ttl is a number of seconds".into()))?);
            }
            post(ctx, &format!("{base}/subscribers"), Body::Json(body)).await
        }
        "unsubscribe" => {
            need_name(ctx)?;
            let id = ctx.args.arg(1, "a subscriber id")?.to_string();
            delete(ctx, &format!("{base}/subscribers/{}", enc(&id)), &[]).await
        }
        "data" => {
            need_name(ctx)?;
            get(ctx, &format!("{base}/state"), &[]).await
        }
        "data-clear" => {
            need_name(ctx)?;
            delete(ctx, &format!("{base}/state"), &[]).await
        }
        "collection" => {
            need_name(ctx)?;
            let format = ctx.args.arg(1, "a format")?.to_string();
            let query = q(&ctx.args.clone(), &[("--base-url", "base_url")]);
            let reply = ctx
                .client()?
                .get(&format!("{base}/collections/{}", enc(&format)), &query)
                .await?;
            ctx.raw(&reply.text);
            Ok(())
        }
        "describe" => {
            let description = ctx.args.arg(0, "a description")?.to_string();
            let mut body = json!({ "description": description });
            if let Some(v) = ctx.args.value("--openapi-version") {
                body["openapi_version"] = json!(v);
            }
            if let Some(m) = ctx.args.value("--model") {
                body["model"] = json!(m);
            }
            post(ctx, "/_rustybin/mocks/ai/spec", Body::Json(body)).await
        }
        "examples" => {
            need_name(ctx)?;
            let mut body = json!({});
            if let Some(ops) = list_flag(&ctx.args, "--operations") {
                body["operations"] = json!(ops);
            }
            if let Some(n) = ctx.args.value("--per-response") {
                body["per_response"] = json!(n
                    .parse::<u64>()
                    .map_err(|_| Fail::Usage("--per-response needs a number".into()))?);
            }
            if let Some(p) = ctx.args.value("--persona") {
                body["persona"] = json!(p);
            }
            if let Some(s) = ctx.args.value("--source") {
                body["source"] = json!(s);
            }
            if ctx.args.has("--apply") {
                body["apply"] = json!(true);
            }
            post(ctx, &format!("{base}/ai/examples"), Body::Json(body)).await
        }
        "edit" => {
            need_name(ctx)?;
            let instruction = ctx.args.arg(1, "an instruction")?.to_string();
            let mut body = json!({ "instruction": instruction, "apply": ctx.args.has("--apply") });
            if let Some(m) = ctx.args.value("--model") {
                body["model"] = json!(m);
            }
            post(ctx, &format!("{base}/ai/edit"), Body::Json(body)).await
        }
        _ => Err(Fail::Usage(format!("mocks {verb}: not implemented"))),
    }
}

// ── Visits ──────────────────────────────────────────────────────────

async fn visits(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "list" => get(ctx, "/_rustybin/mocks/visits", &[]).await,
        "end" => {
            let id = ctx.args.arg(0, "a visit id")?.to_string();
            delete(ctx, &format!("/_rustybin/mocks/visits/{}", enc(&id)), &[]).await
        }
        "end-all" => delete(ctx, "/_rustybin/mocks/visits", &[]).await,
        "pause" | "resume" => {
            post(ctx, &format!("/_rustybin/mocks/visits/{verb}"), Body::None).await
        }
        _ => Err(Fail::Usage(format!("visits {verb}: not implemented"))),
    }
}

async fn visit(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "get" => get(ctx, "/_rustybin/mocks/visit", &[]).await,
        "start" => {
            let body = match ctx.args.value("--email") {
                Some(e) => json!({ "email": e }),
                None => json!({}),
            };
            post(ctx, "/_rustybin/mocks/visit", Body::Json(body)).await
        }
        "extend" => post(ctx, "/_rustybin/mocks/visit/extend", Body::None).await,
        "rotate-token" => post(ctx, "/_rustybin/mocks/visit/token", Body::None).await,
        "end" => delete(ctx, "/_rustybin/mocks/visit", &[]).await,
        _ => Err(Fail::Usage(format!("visit {verb}: not implemented"))),
    }
}

// ── Requests and bins ───────────────────────────────────────────────

async fn requests(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "list" => {
            let query = q(
                &ctx.args.clone(),
                &[("--limit", "limit"), ("--path-prefix", "path_prefix")],
            );
            get(ctx, "/_rustybin/requests", &query).await
        }
        "get" => {
            let id = ctx.args.arg(0, "a request id")?.to_string();
            get(ctx, &format!("/_rustybin/requests/{}", enc(&id)), &[]).await
        }
        "tail" => tail(ctx, "/_rustybin/requests/stream").await,
        "clear" => delete(ctx, "/_rustybin/requests", &[]).await,
        _ => Err(Fail::Usage(format!("requests {verb}: not implemented"))),
    }
}

async fn bins(ctx: &mut Ctx, verb: &str) -> Result<()> {
    let id = ctx.args.positional.first().cloned().unwrap_or_default();
    match verb {
        "list" => get(ctx, "/bin", &[]).await,
        "create" => {
            let mut body = json!({});
            if let Some(s) = ctx.args.value("--status") {
                body["status"] = json!(s
                    .parse::<u16>()
                    .map_err(|_| Fail::Usage("--status needs a number".into()))?);
            }
            if let Some(b) = ctx.args.value("--body").map(str::to_string) {
                body["body"] = loose_json(&ctx.input(&b)?);
            }
            if let Some(d) = ctx.args.value("--delay-ms") {
                body["delay_ms"] = json!(d
                    .parse::<u64>()
                    .map_err(|_| Fail::Usage("--delay-ms needs a number".into()))?);
            }
            post(ctx, "/bin", Body::Json(body)).await
        }
        "delete" => {
            ctx.args.arg(0, "a bin id")?;
            delete(ctx, &format!("/bin/{}", enc(&id)), &[]).await
        }
        "requests" => {
            ctx.args.arg(0, "a bin id")?;
            let query = q(&ctx.args.clone(), &[("--limit", "limit")]);
            get(ctx, &format!("/bin/{}/requests", enc(&id)), &query).await
        }
        "get" => {
            ctx.args.arg(0, "a bin id")?;
            let n = ctx.args.arg(1, "a request number")?.to_string();
            get(ctx, &format!("/bin/{}/requests/{}", enc(&id), enc(&n)), &[]).await
        }
        "tail" => {
            ctx.args.arg(0, "a bin id")?;
            tail(ctx, &format!("/bin/{}/requests/stream", enc(&id))).await
        }
        _ => Err(Fail::Usage(format!("bins {verb}: not implemented"))),
    }
}

// ── Checkpoints ─────────────────────────────────────────────────────

async fn record(ctx: &mut Ctx, verb: &str) -> Result<()> {
    if verb == "list" {
        return get(ctx, "/_rustybin/recordings", &[]).await;
    }
    let name = ctx.args.arg(0, "a recording name")?.to_string();
    let path = format!("/_rustybin/recordings/{}", enc(&name));
    match verb {
        "show" => get(ctx, &path, &[]).await,
        "delete" => delete(ctx, &path, &[]).await,
        "stop" => post(ctx, &format!("{path}/stop"), Body::Json(json!({}))).await,
        "start" => {
            let upstream = ctx
                .args
                .value("--upstream")
                .ok_or_else(|| Fail::Usage("--upstream: the real API's base URL".into()))?
                .to_string();
            let mut body = json!({ "name": name, "upstream": upstream });
            if let Some(m) = ctx.args.value("--mock") {
                body["mock"] = json!(m);
            }
            post(ctx, "/_rustybin/recordings", Body::Json(body)).await
        }
        "build" => {
            let mut body = json!({ "dry_run": ctx.args.has("--dry-run") });
            if let Some(m) = ctx.args.value("--mock") {
                body["mock"] = json!(m);
            }
            post(ctx, &format!("{path}/build"), Body::Json(body)).await
        }
        _ => Err(Fail::Usage(format!("record {verb}: not implemented"))),
    }
}

async fn checkpoint(ctx: &mut Ctx, verb: &str) -> Result<()> {
    let from_git = ctx.args.has("--from-git");
    if verb == "list" {
        let mut query: Vec<(&str, String)> = if from_git {
            vec![("from", "git".to_string())]
        } else {
            Vec::new()
        };
        query.extend(project_query(ctx));
        return get(ctx, "/_rustybin/checkpoints", &query).await;
    }
    let name = ctx.args.arg(0, "a checkpoint name")?.to_string();
    let path = format!("/_rustybin/checkpoints/{}", enc(&name));
    match verb {
        "show" => get(ctx, &path, &[]).await,
        "delete" => delete(ctx, &path, &[]).await,
        "save" => {
            let mut body = json!({ "name": name, "replace": ctx.args.has("--replace") });
            if let Some(p) = ctx.args.value("--project") {
                body["project"] = json!(p);
            }
            post(ctx, "/_rustybin/checkpoints", Body::Json(body)).await
        }
        "restore" => {
            let mut query: Vec<(&str, String)> = Vec::new();
            if ctx.args.has("--dry-run") {
                query.push(("dry_run", "true".to_string()));
            }
            if from_git {
                query.push(("from", "git".to_string()));
            }
            let reply = ctx
                .client()?
                .send(Method::POST, &format!("{path}/restore"), &query, Body::None)
                .await?
                .ok()?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        _ => Err(Fail::Usage(format!("checkpoint {verb}: not implemented"))),
    }
}

// ── Demo packs ──────────────────────────────────────────────────────

/// The pack at `source` as a document: a file (or inline JSON, or `-`), or
/// a directory with a `rustybin.yaml` whose `spec:` files are read here
/// and put inline, since the instance reads no files.
fn pack_document(ctx: &Ctx, source: &str) -> Result<Value> {
    let dir = std::path::Path::new(source);
    if !dir.is_dir() {
        let text = ctx.input(source)?;
        return serde_yaml::from_str(&text)
            .map_err(|e| Fail::Usage(format!("{source}: not YAML or JSON ({e})")));
    }
    let file = dir.join("rustybin.yaml");
    let text = std::fs::read_to_string(&file)
        .map_err(|e| Fail::Usage(format!("{}: {e}", file.display())))?;
    let mut doc: Value = serde_yaml::from_str(&text)
        .map_err(|e| Fail::Usage(format!("{}: not YAML or JSON ({e})", file.display())))?;
    for m in doc["mocks"].as_array_mut().into_iter().flatten() {
        let Some(path) = m["spec"].as_str().map(str::to_string) else {
            continue;
        };
        let rel = std::path::Path::new(&path);
        let inside = rel.is_relative()
            && rel
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)));
        if !inside {
            return Err(Fail::Usage(format!(
                "mocks: spec {path}: a path inside the pack's directory"
            )));
        }
        let spec = std::fs::read_to_string(dir.join(rel))
            .map_err(|e| Fail::Usage(format!("{}: {e}", dir.join(rel).display())))?;
        if let Value::Object(o) = m {
            o.remove("spec");
            o.insert("spec_inline".into(), json!(spec));
        }
    }
    Ok(doc)
}

/// What a pack creates or removes, as rows for people.
fn pack_rows(by_kind: &Value) -> Vec<Value> {
    by_kind
        .as_object()
        .into_iter()
        .flatten()
        .flat_map(|(kind, names)| {
            names
                .as_array()
                .into_iter()
                .flatten()
                .map(move |n| json!({ "kind": kind, "name": n }))
        })
        .collect()
}

async fn pack(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "list" => {
            let v = ctx.client()?.get("/_rustybin/packs", &[]).await?.value();
            if ctx.json {
                ctx.emit(&v);
            } else {
                let rows: Vec<Value> = v["packs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|p| {
                        let n = |k: &str| p["holds"][k].as_array().map(Vec::len).unwrap_or(0);
                        json!({
                            "name": p["name"],
                            "customer": p["customer"],
                            "title": p["title"],
                            "applied_at": p["applied_at"],
                            "mocks": n("mocks"),
                            "tests": n("tests"),
                            "storyboards": n("storyboards"),
                        })
                    })
                    .collect();
                ctx.emit(&json!({ "packs": rows }));
            }
            Ok(())
        }
        "apply" => {
            let source = ctx
                .args
                .arg(0, "a pack: a file, or a directory with a rustybin.yaml")?
                .to_string();
            let document = pack_document(ctx, &source)?;
            let dry_run = ctx.args.has("--dry-run");
            let body = json!({ "document": document, "dry_run": dry_run, "untrusted": ctx.args.has("--untrusted") });
            let reply = ctx
                .client()?
                .send(
                    Method::POST,
                    "/_rustybin/packs/apply",
                    &[],
                    Body::Json(body),
                )
                .await?;
            let v = reply.value();
            // A refusal names what is wrong: the taken names, the problems.
            if reply.status >= 400 && (v["collisions"].is_array() || v["problems"].is_array()) {
                if ctx.json {
                    ctx.emit(&v);
                } else {
                    ctx.emit(&json!({ "collisions": v["collisions"], "problems": v["problems"] }));
                }
                return Err(Fail::Failed(
                    v["error"]
                        .as_str()
                        .unwrap_or("the pack was refused")
                        .to_string(),
                ));
            }
            let v = reply.ok()?.value();
            if ctx.json {
                ctx.emit(&v);
            } else {
                let key = if dry_run { "would_create" } else { "created" };
                ctx.emit(&json!({
                    "pack": v["project"],
                    "dry_run": dry_run,
                    key: pack_rows(&v[key]),
                }));
            }
            Ok(())
        }
        "teardown" => {
            let name = ctx
                .args
                .arg(0, "a pack name (rustybin pack list)")?
                .to_string();
            let dry_run = ctx.args.has("--dry-run");
            if !dry_run {
                ctx.confirm(&format!(
                    "tear down the pack {name} (what its project holds is deleted)?"
                ))?;
            }
            let query: Vec<(&str, String)> = if dry_run {
                vec![("dry_run", "true".to_string())]
            } else {
                Vec::new()
            };
            let v = ctx
                .client()?
                .delete(&format!("/_rustybin/packs/{}", enc(&name)), &query)
                .await?
                .value();
            if ctx.json {
                ctx.emit(&v);
            } else {
                let key = if dry_run { "would_remove" } else { "removed" };
                ctx.emit(&json!({
                    "pack": name,
                    "dry_run": dry_run,
                    "console_title": v["console_title"],
                    key: pack_rows(&v[key]),
                }));
            }
            if !dry_run && v["ok"] != json!(true) {
                return Err(Fail::Failed(format!(
                    "torn down with problems: {}",
                    v["problems"]
                )));
            }
            Ok(())
        }
        "export" => {
            let name = ctx.args.arg(0, "a project name")?.to_string();
            let format = ctx.args.value("--format").unwrap_or("yaml").to_string();
            let reply = ctx
                .client()?
                .get(
                    &format!("/_rustybin/packs/{}/export", enc(&name)),
                    &[("format", format)],
                )
                .await?;
            match ctx.args.value("--output").map(str::to_string) {
                Some(file) => {
                    std::fs::write(&file, reply.text.as_bytes())
                        .map_err(|e| Fail::Usage(format!("{file}: {e}")))?;
                    ctx.say(&format!("Written to {file}."));
                }
                None => ctx.raw(&reply.text),
            }
            Ok(())
        }
        _ => Err(Fail::Usage(format!("pack {verb}: not implemented"))),
    }
}

// ── Storyboards ─────────────────────────────────────────────────────

async fn storyboard(ctx: &mut Ctx, verb: &str) -> Result<()> {
    if verb == "list" {
        let query = project_query(ctx);
        return get(ctx, "/_rustybin/storyboards", &query).await;
    }
    if verb == "apply" {
        let file = ctx
            .args
            .value("--file")
            .ok_or_else(|| Fail::Usage("--file: the storyboard as JSON or YAML".into()))?
            .to_string();
        let text = ctx.input(&file)?;
        let mut v: Value = serde_yaml::from_str(&text)
            .map_err(|e| Fail::Usage(format!("--file: not JSON or YAML ({e})")))?;
        let name = match ctx.args.arg(0, "a storyboard name") {
            Ok(n) => n.to_string(),
            Err(_) => v["name"]
                .as_str()
                .ok_or_else(|| {
                    Fail::Usage("a storyboard name: the argument, or name: in the file".into())
                })?
                .to_string(),
        };
        if let Some(p) = ctx.args.value("--project") {
            v["project"] = json!(p);
        }
        let path = format!("/_rustybin/storyboards/{}", enc(&name));
        let reply = ctx.client()?.put(&path, Body::Json(v)).await?;
        emit_reply(ctx, &reply);
        return Ok(());
    }
    let name = ctx
        .args
        .arg(0, "a storyboard name (rustybin storyboard list)")?
        .to_string();
    let path = format!("/_rustybin/storyboards/{}", enc(&name));
    match verb {
        "show" => get(ctx, &path, &[]).await,
        "delete" => delete(ctx, &path, &[]).await,
        "reset" => {
            let vars = var_flags(ctx)?;
            let reply = ctx
                .client()?
                .post(
                    &format!("{path}/reset"),
                    Body::Json(json!({ "variables": vars })),
                )
                .await?;
            let v = reply.value();
            if ctx.json {
                ctx.emit(&v);
            } else {
                ctx.emit(&json!({
                    "ok": v["ok"],
                    "reset": step_rows(&v["reset"]),
                    "ended_scenarios": v["ended_scenarios"],
                }));
            }
            if v["ok"] != json!(true) {
                return Err(Fail::Failed(format!(
                    "a reset failed: {}",
                    failed_steps(&v["reset"]).join("; ")
                )));
            }
            Ok(())
        }
        "run" => storyboard_run(ctx, &path).await,
        _ => Err(Fail::Usage(format!("storyboard {verb}: not implemented"))),
    }
}

/// Step results as rows for people.
fn step_rows(results: &Value) -> Vec<Value> {
    results
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            json!({
                "step": r["step"],
                "title": r["title"],
                "action": r["action"],
                "ok": r["ok"],
                "summary": r["summary"],
            })
        })
        .collect()
}

/// The failed results, each as `step N (title): why`.
fn failed_steps(results: &Value) -> Vec<String> {
    results
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["ok"] != json!(true))
        .map(|r| {
            format!(
                "step {} ({}): {}",
                r["step"],
                r["title"].as_str().unwrap_or(""),
                r["summary"].as_str().unwrap_or("")
            )
        })
        .collect()
}

/// `rustybin storyboard run`: a rehearsal. Every step in order (or the
/// one of `--step`), each verdict printed; exit 1 when one fails.
async fn storyboard_run(ctx: &mut Ctx, path: &str) -> Result<()> {
    let vars = var_flags(ctx)?;
    let body = json!({ "variables": vars });
    let steps: Vec<usize> = match ctx.args.value("--step") {
        Some(n) => vec![n
            .parse::<usize>()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or_else(|| Fail::Usage("--step: a number, counted from 1".into()))?],
        None => {
            let v = ctx.client()?.get(path, &[]).await?.value();
            (1..=v["steps"].as_array().map(Vec::len).unwrap_or(0)).collect()
        }
    };
    let mut results = Vec::new();
    for n in steps {
        let reply = ctx
            .client()?
            .post(&format!("{path}/steps/{n}/run"), Body::Json(body.clone()))
            .await?;
        results.push(reply.value());
    }
    let results = Value::Array(results);
    let ok = results
        .as_array()
        .is_some_and(|a| a.iter().all(|r| r["ok"] == json!(true)));
    if ctx.json {
        ctx.emit(&json!({ "ok": ok, "results": results }));
    } else {
        ctx.emit(&json!({ "ok": ok, "results": step_rows(&results) }));
    }
    if !ok {
        // The table cuts a long summary: the message says each failure whole.
        return Err(Fail::Failed(format!(
            "a step failed: {}",
            failed_steps(&results).join("; ")
        )));
    }
    Ok(())
}

// ── Saved tests ─────────────────────────────────────────────────────

async fn tests(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "list" => {
            let query = project_query(ctx);
            get(ctx, "/_rustybin/tests", &query).await
        }
        "export" => {
            let format = ctx.args.value("--format").unwrap_or("hurl").to_string();
            let path = match ctx.args.arg(0, "a test name") {
                Ok(name) => format!("/_rustybin/tests/{}/export", enc(name)),
                Err(_) if format == "arazzo" => {
                    return Err(Fail::Usage(
                        "--format arazzo exports one test: give its name".into(),
                    ))
                }
                Err(_) => "/_rustybin/tests/export".to_string(),
            };
            let reply = ctx.client()?.get(&path, &[("format", format)]).await?;
            ctx.raw(&reply.text);
            Ok(())
        }
        "import" => {
            let file = ctx
                .args
                .arg(0, "an Arazzo document: a file, or - for stdin")?
                .to_string();
            let text = ctx.input(&file)?;
            let dry_run = ctx.args.has("--dry-run");
            let mut body = json!({ "document": text, "dry_run": dry_run, "replace": ctx.args.has("--replace") });
            if let Some(g) = ctx.args.value("--gateway") {
                body["gateway"] = json!(g);
            }
            if let Some(p) = ctx.args.value("--project") {
                body["project"] = json!(p);
            }
            let v = ctx
                .client()?
                .post("/_rustybin/tests/import", Body::Json(body))
                .await?
                .value();
            if ctx.json {
                ctx.emit(&v);
            } else {
                let rows: Vec<Value> = v["tests"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|t| {
                        json!({
                            "test": t["name"],
                            "steps": t["steps"].as_array().map(Vec::len).unwrap_or(0),
                            "variables": t["variables"],
                        })
                    })
                    .collect();
                ctx.emit(&json!({
                    "dry_run": dry_run,
                    "sources": v["sources"],
                    "tests": rows,
                    "left_out": v["left_out"],
                }));
            }
            Ok(())
        }
        "run" => tests_run(ctx).await,
        "runs" => tests_runs(ctx).await,
        "runs-clear" => delete(ctx, "/_rustybin/tests/runs", &[]).await,
        "schedules" => tests_schedules(ctx).await,
        "schedule-show" | "schedule-save" | "schedule-run" | "schedule-delete" => {
            let name = ctx
                .args
                .arg(0, "a schedule name (rustybin tests schedules)")?
                .to_string();
            let path = format!("/_rustybin/tests/schedules/{}", enc(&name));
            match verb {
                "schedule-show" => get(ctx, &path, &[]).await,
                "schedule-delete" => delete(ctx, &path, &[]).await,
                "schedule-save" => tests_schedule_save(ctx, &name, &path).await,
                _ => tests_schedule_run(ctx, &path).await,
            }
        }
        "run-show" => {
            let id = ctx
                .args
                .arg(0, "a run id (rustybin tests runs)")?
                .to_string();
            let path = format!("/_rustybin/tests/runs/{}", enc(&id));
            if ctx.args.has("--junit") {
                let reply = ctx
                    .client()?
                    .get(&path, &[("format", "junit".to_string())])
                    .await?;
                ctx.raw(&reply.text);
                return Ok(());
            }
            get(ctx, &path, &[]).await
        }
        _ => {
            let name = ctx.args.arg(0, "a test name")?.to_string();
            let path = format!("/_rustybin/tests/{}", enc(&name));
            match verb {
                "show" => get(ctx, &path, &[]).await,
                "delete" => delete(ctx, &path, &[]).await,
                "save" => {
                    let capture = ctx
                        .args
                        .value("--capture")
                        .ok_or_else(|| Fail::Usage("--capture: a captured request's id".into()))?
                        .to_string();
                    let mut body = json!({ "name": name, "capture": capture });
                    if let Some(g) = ctx.args.value("--gateway") {
                        body["gateway"] = json!(g);
                    }
                    if let Some(d) = ctx.args.value("--description") {
                        body["description"] = json!(d);
                    }
                    if let Some(p) = ctx.args.value("--project") {
                        body["project"] = json!(p);
                    }
                    post(ctx, "/_rustybin/tests", Body::Json(body)).await
                }
                "put" => {
                    let file = ctx
                        .args
                        .value("--file")
                        .ok_or_else(|| Fail::Usage("--file: the test as JSON or YAML".into()))?
                        .to_string();
                    let text = ctx.input(&file)?;
                    let v: Value = serde_yaml::from_str(&text)
                        .map_err(|e| Fail::Usage(format!("--file: not JSON or YAML ({e})")))?;
                    let reply = ctx.client()?.put(&path, Body::Json(v)).await?;
                    emit_reply(ctx, &reply);
                    Ok(())
                }
                _ => Err(Fail::Usage(format!("tests {verb}: not implemented"))),
            }
        }
    }
}

/// `rustybin tests runs`: the history as a table (or JSON), newest first.
async fn tests_runs(ctx: &mut Ctx) -> Result<()> {
    let query = q(&ctx.args, &[("--limit", "limit"), ("--cursor", "cursor")]);
    let v = ctx
        .client()?
        .get("/_rustybin/tests/runs", &query)
        .await?
        .value();
    if ctx.json {
        ctx.emit(&v);
        return Ok(());
    }
    let names = |a: &Value| -> String {
        a.as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let rows: Vec<Value> = v["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            json!({
                "id": r["id"],
                "ran_at": r["ran_at"],
                "trigger": r["trigger"],
                "principal": r["principal"],
                "passed": r["passed"],
                "total": r["total"],
                "failing": names(&r["failing"]),
                "newly_failing": names(&r["change"]["newly_failing"]),
                "newly_passing": names(&r["change"]["newly_passing"]),
            })
        })
        .collect();
    let mut out = json!({ "total": v["total"], "runs": rows });
    if v["next_cursor"].is_string() {
        out["next_cursor"] = v["next_cursor"].clone();
    }
    ctx.emit(&out);
    Ok(())
}

/// `rustybin tests schedules`: one row a schedule (or JSON).
async fn tests_schedules(ctx: &mut Ctx) -> Result<()> {
    let v = ctx
        .client()?
        .get("/_rustybin/tests/schedules", &[])
        .await?
        .value();
    if ctx.json {
        ctx.emit(&v);
        return Ok(());
    }
    let rows: Vec<Value> = v["schedules"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            let st = &s["status"];
            let when = match (s["every"].as_str(), s["at"].as_str()) {
                (Some(e), _) => format!("every {e}"),
                (_, Some(a)) => format!("at {a} UTC"),
                _ => String::new(),
            };
            json!({
                "name": s["name"],
                "when": when,
                "tests": s["selection"],
                "enabled": s["enabled"],
                "next_due": st["next_due"],
                "last_result": st["last_result"],
                "last_run_id": st["last_run_id"],
                "last_notice": st["last_notice"]["result"],
            })
        })
        .collect();
    ctx.emit(&json!({ "schedules": rows }));
    Ok(())
}

/// `rustybin tests schedule-save <name>`: the whole schedule from flags.
async fn tests_schedule_save(ctx: &mut Ctx, name: &str, path: &str) -> Result<()> {
    let mut body = json!({ "name": name, "enabled": !ctx.args.has("--disabled") });
    match (ctx.args.value("--every"), ctx.args.value("--at")) {
        (Some(e), None) => body["every"] = json!(e),
        (None, Some(a)) => body["at"] = json!(a),
        _ => {
            return Err(Fail::Usage(
                "one of --every (5m to 24h) or --at (HH:MM, UTC)".into(),
            ))
        }
    }
    if let Some(names) = list_flag(&ctx.args, "--names") {
        body["names"] = json!(names);
    }
    for (flag, key) in [
        ("--prefix", "prefix"),
        ("--webhook", "webhook"),
        ("--description", "description"),
    ] {
        if let Some(v) = ctx.args.value(flag) {
            body[key] = json!(v);
        }
    }
    if ctx.args.has("--notify-recovery") {
        body["notify_recovery"] = json!(true);
    }
    let reply = ctx.client()?.put(path, Body::Json(body)).await?;
    emit_reply(ctx, &reply);
    Ok(())
}

/// `rustybin tests schedule-run <name>`: the result in a few lines (or
/// JSON); exit 1 when the run failed.
async fn tests_schedule_run(ctx: &mut Ctx, path: &str) -> Result<()> {
    let v = ctx
        .client()?
        .send(Method::POST, &format!("{path}/run"), &[], Body::None)
        .await?
        .ok()?
        .value();
    if ctx.json {
        ctx.emit(&v);
    } else {
        let run = &v["run"];
        let failing: Vec<&str> = run["results"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| r["passed"] != json!(true))
            .filter_map(|r| r["name"].as_str())
            .collect();
        let mut out = json!({ "result": v["result"], "run_id": run["run_id"] });
        if !failing.is_empty() {
            out["failing"] = json!(failing.join(", "));
        }
        if v["detail"].is_string() {
            out["detail"] = v["detail"].clone();
        }
        ctx.emit(&out);
    }
    if v["result"] == json!("failed") {
        return Err(Fail::Failed("the scheduled run failed (see above)".into()));
    }
    Ok(())
}

/// `rustybin tests run`: a table (or JSON, or JUnit XML); exit 1 when a
/// test fails or cannot run.
/// The `--var` flags of a run as `{{variables}}`. A credential is never on
/// the command line: `NAME=-` reads stdin (or prompts), `NAME=@file` a
/// file; a literal value is refused for a secret-looking name.
fn var_flags(ctx: &mut Ctx) -> Result<Map<String, Value>> {
    let mut vars = Map::new();
    let given: Vec<String> = ctx
        .args
        .values("--var")
        .into_iter()
        .map(str::to_string)
        .collect();
    for (line, v) in given.iter().enumerate() {
        let (k, val) = v
            .split_once('=')
            .ok_or_else(|| Fail::Usage(format!("--var {v}: NAME=value, NAME=- or NAME=@file")))?;
        let value = match val {
            "-" => ctx.secret(k, true, line)?,
            file if file.starts_with('@') => std::fs::read_to_string(&file[1..])
                .map(|t| t.trim_end_matches(['\r', '\n']).to_string())
                .map_err(|e| Fail::Usage(format!("--var {k}: {}: {e}", &file[1..])))?,
            _ if crate::keys::looks_secret(k) => {
                return Err(Fail::Usage(format!(
                    "--var {k}=...: {k} looks like a credential: give {k}=- (stdin or a prompt) or {k}=@file, never the value on the command line"
                )));
            }
            literal => literal.to_string(),
        };
        vars.insert(k.to_string(), json!(value));
    }
    Ok(vars)
}

async fn tests_run(ctx: &mut Ctx) -> Result<()> {
    let mut body = json!({});
    if let Some(p) = ctx.args.value("--project") {
        body["project"] = json!(p);
    }
    if let Some(names) = list_flag(&ctx.args, "--names") {
        body["names"] = json!(names);
    }
    let vars = var_flags(ctx)?;
    if !vars.is_empty() {
        body["variables"] = Value::Object(vars);
    }
    let junit = ctx.args.has("--junit");
    let query: Vec<(&str, String)> = if junit {
        vec![("format", "junit".to_string())]
    } else {
        Vec::new()
    };
    let reply = ctx
        .client()?
        .send(
            Method::POST,
            "/_rustybin/tests/run",
            &query,
            Body::Json(body),
        )
        .await?
        .ok()?;
    let passed = if junit {
        ctx.raw(&reply.text);
        !reply.text.contains("<failure") && !reply.text.contains("<error")
    } else {
        let v = reply.value();
        if ctx.json {
            ctx.emit(&v);
        } else {
            let rows: Vec<Value> = v["results"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| {
                    let failures: Vec<String> = r["failures"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|f| {
                            format!(
                                "{}: expected {}, saw {}",
                                f["assertion"].as_str().unwrap_or(""),
                                f["expected"].as_str().unwrap_or(""),
                                f["seen"].as_str().unwrap_or("")
                            )
                        })
                        .collect();
                    let problem = r["error"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| failures.join("; "));
                    json!({
                        "test": r["name"],
                        "passed": r["passed"],
                        "status": r["status"],
                        "ms": r["ms"],
                        "problem": problem,
                    })
                })
                .collect();
            let mut out = json!({ "passed": v["passed"], "run_id": v["run_id"], "results": rows });
            let change = &v["change"];
            for k in ["newly_failing", "newly_passing"] {
                if change[k].as_array().is_some_and(|a| !a.is_empty()) {
                    out[k] = change[k].clone();
                }
            }
            ctx.emit(&out);
        }
        v["passed"] == true
    };
    if !passed {
        return Err(Fail::Failed("a test failed (see above)".into()));
    }
    Ok(())
}

// ── Gateway scenarios ───────────────────────────────────────────────

async fn scenario(ctx: &mut Ctx, verb: &str) -> Result<()> {
    if verb == "list" {
        let v = ctx
            .client()?
            .get("/_rustybin/scenarios", &[])
            .await?
            .value();
        if ctx.json {
            ctx.emit(&v);
        } else {
            let pick = |items: &Value, keys: &[&str]| -> Value {
                Value::Array(
                    items
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|i| {
                            Value::Object(
                                keys.iter()
                                    .map(|k| (k.to_string(), i[*k].clone()))
                                    .collect(),
                            )
                        })
                        .collect(),
                )
            };
            ctx.emit(&json!({
                "scenarios": pick(&v["scenarios"], &["id", "title", "proves"]),
                "runs": pick(&v["runs"], &["scenario", "id", "upstream", "expires_at", "checks"]),
            }));
        }
        return Ok(());
    }
    let id = ctx.args.arg(0, "a scenario id")?.to_string();
    let runs = format!("/_rustybin/scenarios/{}/runs", enc(&id));
    if verb == "start" {
        let v = ctx
            .client()?
            .post(&runs, Body::Json(json!({})))
            .await?
            .value();
        if ctx.json {
            ctx.emit(&v);
        } else {
            let mut out = json!({ "run": v["id"], "upstream": v["upstream"], "setup": v["setup"] });
            if !v["second_upstream"].is_null() {
                out["second_upstream"] = v["second_upstream"].clone();
            }
            ctx.emit(&out);
        }
        return Ok(());
    }
    let run = ctx.args.arg(1, "a run id")?.to_string();
    let path = format!("{runs}/{}", enc(&run));
    match verb {
        "show" => get(ctx, &path, &[]).await,
        "end" => delete(ctx, &path, &[]).await,
        "check" => {
            let gateway = ctx
                .args
                .value("--gateway")
                .ok_or_else(|| {
                    Fail::Usage("--gateway: the URL the gateway serves the route at".into())
                })?
                .to_string();
            let mut body = json!({ "gateway_url": gateway });
            // Every test parameter as --its-name (the instance checks the
            // values and which scenario takes them).
            use crate::scenarios::catalog::{Kind, PARAMS};
            for p in PARAMS {
                let flag = format!("--{}", p.name.replace('_', "-"));
                match p.kind {
                    Kind::Bool { .. } => {
                        if ctx.args.has(&flag) {
                            body[p.name] = json!(true);
                        }
                    }
                    Kind::Integer { .. } => {
                        if let Some(l) = ctx.args.value(&flag) {
                            body[p.name] = json!(l
                                .parse::<u64>()
                                .map_err(|_| Fail::Usage(format!("{flag} needs a number")))?);
                        }
                    }
                    _ => {
                        if let Some(v) = ctx.args.value(&flag) {
                            body[p.name] = json!(v);
                        }
                    }
                }
            }
            let v = ctx
                .client()?
                .post(&format!("{path}/check"), Body::Json(body))
                .await?
                .value();
            if ctx.json {
                ctx.emit(&v);
            } else {
                let rows: Vec<Value> = v["expectations"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| {
                        json!({
                            "ok": e["ok"],
                            "expectation": e["name"],
                            "expected": e["expected"],
                            "seen": e["seen"],
                        })
                    })
                    .collect();
                ctx.emit(
                    &json!({ "passed": v["passed"], "expectations": rows, "hints": v["hints"] }),
                );
            }
            if v["passed"] != true {
                return Err(Fail::Failed("an expectation failed (see above)".into()));
            }
            Ok(())
        }
        _ => Err(Fail::Usage(format!("scenario {verb}: not implemented"))),
    }
}

// ── Agents ──────────────────────────────────────────────────────────

async fn agents(ctx: &mut Ctx, verb: &str) -> Result<()> {
    let id = ctx.args.positional.first().cloned().unwrap_or_default();
    let base = format!("/_rustybin/agents/{}", enc(&id));
    match verb {
        "list" => {
            let query: Vec<(&str, String)> = ctx
                .args
                .value("--owner")
                .map(|o| vec![("owner", o.to_string())])
                .unwrap_or_default();
            let v = ctx
                .client()?
                .get("/_rustybin/agents", &query)
                .await?
                .value();
            if ctx.json {
                ctx.emit(&v);
            } else {
                let rows: Vec<Value> = v["agents"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|x| json!({ "id": x["id"], "name": x["name"], "version": x["version"], "owner": x["owner_label"], "credential": x["credential"]["kind"], "status": x["status"], "ceiling": x["rights"]["ceiling"], "calls_today": x["calls"]["today"], "last_seen": x["last_seen"] }))
                            .collect()
                    })
                    .unwrap_or_default();
                ctx.emit(&json!({ "agents": rows, "policy": v["policy"] }));
            }
            Ok(())
        }
        "get" => {
            ctx.args.arg(0, "an agent id")?;
            get(ctx, &base, &[]).await
        }
        "rights" => {
            ctx.args.arg(0, "an agent id")?;
            let body = if ctx.args.has("--reset") {
                json!({ "rights": null, "note": null })
            } else {
                let mut rights = Map::new();
                if let Some(c) = ctx.args.value("--ceiling") {
                    rights.insert("ceiling".into(), json!(c));
                }
                if let Some(a) = list_flag(&ctx.args, "--allow") {
                    rights.insert("allow".into(), json!(a));
                }
                if let Some(d) = list_flag(&ctx.args, "--deny") {
                    rights.insert("deny".into(), json!(d));
                }
                let mut body = Map::new();
                if !rights.is_empty() {
                    body.insert("rights".into(), Value::Object(rights));
                }
                if let Some(n) = ctx.args.value("--note") {
                    body.insert("note".into(), json!(n));
                }
                if body.is_empty() {
                    return Err(Fail::Usage(
                        "nothing to set: --ceiling, --allow, --deny, --note or --reset".into(),
                    ));
                }
                Value::Object(body)
            };
            let reply = ctx.client()?.patch(&base, Body::Json(body)).await?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "revoke" | "restore" => {
            ctx.args.arg(0, "an agent id")?;
            post(ctx, &format!("{base}/{verb}"), Body::None).await
        }
        "end-sessions" => {
            ctx.args.arg(0, "an agent id")?;
            delete(ctx, &format!("{base}/sessions"), &[]).await
        }
        "tail" => tail(ctx, "/_rustybin/agents/events").await,
        _ => Err(Fail::Usage(format!("agents {verb}: not implemented"))),
    }
}

// ── AI ──────────────────────────────────────────────────────────────

async fn ai(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "status" => get(ctx, "/_rustybin/assist", &[]).await,
        "models" => {
            let v = if ctx.args.has("--load") {
                ctx.client()?
                    .post("/_rustybin/assist/models", Body::Json(json!({})))
                    .await?
                    .value()
            } else {
                ctx.client()?
                    .get("/_rustybin/assist/models", &[])
                    .await?
                    .value()
            };
            if ctx.json {
                ctx.emit(&v);
            } else {
                ctx.emit(&json!({
                    "provider": v["provider"],
                    "loaded": v["loaded"],
                    "models": v["models"].as_array().map(|a| a.iter().map(|m| json!({
                        "id": m["id"],
                        "tier": m["tier"],
                        "context": m["context"],
                        "price_per_million": m["price_per_million"],
                        "reasoning": m["reasoning"],
                        "listed": m["listed"],
                        "jobs": m["jobs"],
                        "default_for": m["default_for"],
                    })).collect::<Vec<_>>()),
                }));
            }
            Ok(())
        }
        "jobs" => {
            let v = ctx
                .client()?
                .get("/_rustybin/assist/models", &[])
                .await?
                .value();
            ctx.emit(&json!({ "provider": v["provider"], "jobs": v["jobs"] }));
            Ok(())
        }
        "use" => {
            let model = ctx.args.arg(0, "a model id")?.to_string();
            if ctx.args.has("--fast") && ctx.args.value("--job").is_some() {
                return Err(Fail::Usage(
                    "--fast and --job do not go together: a job names its own model".to_string(),
                ));
            }
            let key = match ctx.args.value("--job") {
                Some(job) => format!("assist.job.{job}"),
                None if ctx.args.has("--fast") => "assist.fast_model".to_string(),
                None => "assist.model".to_string(),
            };
            let value = if model.trim().is_empty() {
                Value::Null
            } else {
                json!(model)
            };
            let body = json!({ "values": { key: value } });
            ctx.client()?
                .patch("/_rustybin/settings", Body::Json(body))
                .await?
                .value();
            let v = ctx
                .client()?
                .get("/_rustybin/assist/models", &[])
                .await?
                .value();
            ctx.emit(&json!({ "provider": v["provider"], "jobs": v["jobs"] }));
            Ok(())
        }
        "usage" => get(ctx, "/_rustybin/assist/usage", &[]).await,
        "test" => post(ctx, "/_rustybin/assist/test", Body::Json(json!({}))).await,
        "run" => {
            let task = ctx.args.arg(0, "a task name")?.to_string();
            let inputs = ctx.json_flag("--input")?.unwrap_or_else(|| json!({}));
            let mut body = json!({ "task": task, "inputs": inputs, "stream": false });
            if let Some(m) = ctx.args.value("--model") {
                body["model"] = json!(m);
            }
            let v = ctx
                .client()?
                .post("/_rustybin/assist/run", Body::Json(body))
                .await?
                .value();
            if ctx.json {
                ctx.emit(&v);
            } else if let Some(text) = v.get("text").and_then(Value::as_str) {
                ctx.raw(text);
            } else {
                ctx.emit(&v);
            }
            Ok(())
        }
        _ => Err(Fail::Usage(format!("ai {verb}: not implemented"))),
    }
}

// ── MCP ─────────────────────────────────────────────────────────────

async fn relay_call(ctx: &mut Ctx, target: &str, method: &str, params: Value) -> Result<()> {
    let mut message = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    if let Some(p) = message["params"].as_object_mut() {
        p.entry("_meta").or_insert(json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": { "name": "rustybin-cli", "version": env!("CARGO_PKG_VERSION") },
        }));
    }
    let mut body =
        json!({ "target": target, "message": message, "headers": headers_flag(&ctx.args)? });
    if let Some(t) = ctx.args.value("--transport") {
        body["transport"] = json!(t);
    }
    if let Some(c) = ctx.args.value("--connection") {
        body["connection"] = json!(c);
    }
    let json_out = ctx.json;
    let client = ctx.client()?;
    let mut answers: Vec<Value> = Vec::new();
    let mut failure: Option<String> = None;
    client
        .sse(
            Method::POST,
            "/_rustybin/mcp-relay",
            Body::Json(body),
            None,
            Duration::from_secs(120),
            &mut |event, data| {
                let parsed: Value = serde_json::from_str(data).unwrap_or_else(|_| json!(data));
                match event {
                    "message" => answers.push(parsed),
                    "body" | "error" => failure = Some(parsed.to_string()),
                    "relay" => {
                        if let Some(s) = parsed["status"].as_u64().filter(|s| *s >= 400) {
                            failure = Some(format!("the server answered HTTP {s}"));
                        }
                    }
                    _ => {}
                }
                event != "done"
            },
        )
        .await?;
    if let Some(f) = failure.filter(|_| answers.is_empty()) {
        return Err(Fail::Failed(f));
    }
    let v = if answers.len() == 1 {
        answers.remove(0)
    } else {
        Value::Array(answers)
    };
    if json_out {
        ctx.emit(&v);
    } else if let Some(err) = v.get("error") {
        return Err(Fail::Failed(format!("{}", err)));
    } else if let Some(result) = v.get("result") {
        ctx.emit(result);
    } else {
        ctx.emit(&v);
    }
    Ok(())
}

async fn mcp(ctx: &mut Ctx, verb: &str) -> Result<()> {
    let first = ctx.args.positional.first().cloned().unwrap_or_default();
    match verb {
        "servers" => get(ctx, "/_rustybin/mcp-servers", &[]).await,
        "connections" => get(ctx, "/_rustybin/mcp-connections", &[]).await,
        "connection" => {
            ctx.args.arg(0, "a connection id")?;
            get(
                ctx,
                &format!("/_rustybin/mcp-connections/{}", enc(&first)),
                &[],
            )
            .await
        }
        "save-connection" => {
            ctx.args.arg(0, "a connection id")?;
            let target = ctx
                .args
                .value("--target")
                .ok_or_else(|| Fail::Usage("--target URL is required".into()))?;
            // A secret never on the command line (shell history, the process
            // list): `name=-` reads the next line of stdin (or prompts),
            // `name=@path` reads a file.
            let mut secrets = Map::new();
            for (line, s) in ctx.args.values("--secret").into_iter().enumerate() {
                let (k, v) = s.split_once('=').ok_or_else(|| {
                    Fail::Usage(format!("--secret {s:?}: expected name=- or name=@file"))
                })?;
                let value = match v {
                    "-" => ctx.secret(k, true, line)?,
                    file if file.starts_with('@') => std::fs::read_to_string(&file[1..])
                        .map(|t| t.trim_end_matches(['\r', '\n']).to_string())
                        .map_err(|e| Fail::Usage(format!("--secret {k}: {}: {e}", &file[1..])))?,
                    _ => {
                        return Err(Fail::Usage(format!(
                            "--secret {k}=...: give name=- (stdin or a prompt) or name=@file, never the value on the command line"
                        )))
                    }
                };
                secrets.insert(k.to_string(), json!(value));
            }
            let body = json!({
                "name": ctx.args.value("--name").unwrap_or(&first),
                "target": target,
                "transport": ctx.args.value("--transport").unwrap_or("modern"),
                "mode": ctx.args.value("--mode").unwrap_or("relay"),
                "auth": ctx.json_flag("--auth")?.unwrap_or_else(|| json!({ "kind": "none" })),
                "secrets": secrets,
            });
            let reply = ctx
                .client()?
                .put(
                    &format!("/_rustybin/mcp-connections/{}", enc(&first)),
                    Body::Json(body),
                )
                .await?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "delete-connection" => {
            ctx.args.arg(0, "a connection id")?;
            delete(
                ctx,
                &format!("/_rustybin/mcp-connections/{}", enc(&first)),
                &[],
            )
            .await
        }
        "check" => {
            ctx.args.arg(0, "a target URL")?;
            let mut body = json!({ "target": first, "headers": headers_flag(&ctx.args)? });
            for (flag, key) in [
                ("--transport", "transport"),
                ("--version", "version"),
                ("--connection", "connection"),
            ] {
                if let Some(v) = ctx.args.value(flag) {
                    body[key] = json!(v);
                }
            }
            if let Some(checks) = list_flag(&ctx.args, "--checks") {
                body["checks"] = json!(checks);
            }
            let junit = ctx.args.value("--junit").map(str::to_string);
            let path = if junit.is_some() {
                "/_rustybin/mcp-checks?format=junit&save=true"
            } else {
                "/_rustybin/mcp-checks?save=true"
            };
            let reply = ctx.client()?.post(path, Body::Json(body)).await?;
            if let Some(file) = junit {
                std::fs::write(&file, reply.text.as_bytes())
                    .map_err(|e| Fail::Usage(format!("{file}: {e}")))?;
                ctx.say(&format!("JUnit report written to {file}."));
            } else {
                emit_reply(ctx, &reply);
            }
            Ok(())
        }
        "runs" => get(ctx, "/_rustybin/mcp-checks", &[]).await,
        "run" => {
            ctx.args.arg(0, "a run id")?;
            get(ctx, &format!("/_rustybin/mcp-checks/{}", enc(&first)), &[]).await
        }
        "delete-run" => {
            ctx.args.arg(0, "a run id")?;
            delete(ctx, &format!("/_rustybin/mcp-checks/{}", enc(&first)), &[]).await
        }
        "call" => {
            ctx.args.arg(0, "a target URL")?;
            let method = ctx.args.arg(1, "a JSON-RPC method")?.to_string();
            let params = ctx.json_flag("--params")?.unwrap_or_else(|| json!({}));
            relay_call(ctx, &first, &method, params).await
        }
        "tools" => {
            ctx.args.arg(0, "a target URL")?;
            relay_call(ctx, &first, "tools/list", json!({})).await
        }
        "fetch" => {
            ctx.args.arg(0, "a URL")?;
            post(
                ctx,
                "/_rustybin/mcp-relay/fetch",
                Body::Json(json!({ "url": first })),
            )
            .await
        }
        "end-relay" => {
            ctx.args.arg(0, "a relay session")?;
            delete(ctx, &format!("/_rustybin/mcp-relay/{}", enc(&first)), &[]).await
        }
        _ => Err(Fail::Usage(format!("mcp {verb}: not implemented"))),
    }
}

// ── Registry ────────────────────────────────────────────────────────

async fn registry(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "status" => get(ctx, "/_rustybin/registry", &[]).await,
        "publish" => {
            let file = ctx.args.arg(0, "a server.json file")?.to_string();
            let doc: Value = serde_json::from_str(&ctx.input(&file)?)
                .map_err(|e| Fail::Usage(format!("{file}: not JSON: {e}")))?;
            let client = ctx.client()?;
            let token = client
                .post("/mcp-registry/v0.1/auth/none", Body::Json(json!({})))
                .await?
                .value()["registry_token"]
                .as_str()
                .ok_or_else(|| Fail::Failed("the registry gave no token".into()))?
                .to_string();
            let mut publisher = super::client::Client::new(&client.base, Some(token))?;
            publisher.session = client.session.clone();
            let reply = publisher
                .post("/mcp-registry/v0.1/publish", Body::Json(doc))
                .await?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "delete" => {
            let name = ctx.args.arg(0, "a server name")?.to_string();
            let query = q(&ctx.args.clone(), &[("--version", "version")]);
            delete(
                ctx,
                &format!("/_rustybin/registry/servers/{}", enc(&name)),
                &query,
            )
            .await
        }
        "validate" => {
            let file = ctx.args.arg(0, "a server.json file")?.to_string();
            let text = ctx.input(&file)?;
            post(
                ctx,
                "/_rustybin/registry/validate",
                Body::Text(text, "application/json"),
            )
            .await
        }
        "browse" => {
            let query = q(
                &ctx.args.clone(),
                &[
                    ("--registry", "url"),
                    ("--search", "search"),
                    ("--cursor", "cursor"),
                ],
            );
            get(ctx, "/_rustybin/registry/browse", &query).await
        }
        _ => Err(Fail::Usage(format!("registry {verb}: not implemented"))),
    }
}

// ── Portal ──────────────────────────────────────────────────────────

async fn portal(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "status" => get(ctx, "/_rustybin/portal", &[]).await,
        "publish" => {
            let id = ctx.args.arg(0, "an item id")?.to_string();
            let mut meta = Map::new();
            for (flag, key) in [
                ("--title", "title"),
                ("--summary", "summary"),
                ("--contact", "contact"),
            ] {
                if let Some(v) = ctx.args.value(flag) {
                    meta.insert(key.into(), json!(v));
                }
            }
            if let Some(tags) = list_flag(&ctx.args, "--tags") {
                meta.insert("tags".into(), json!(tags));
            }
            if let Some(o) = ctx.args.value("--order") {
                meta.insert(
                    "order".into(),
                    json!(o
                        .parse::<i64>()
                        .map_err(|_| Fail::Usage("--order needs a number".into()))?),
                );
            }
            if ctx.args.has("--no-credentials") {
                meta.insert("credentials".into(), json!(false));
            }
            if let Some(a) = ctx.args.value("--access") {
                if a != "open" && a != "closed" {
                    return Err(Fail::Usage("--access is open or closed".into()));
                }
                meta.insert("access".into(), json!(a));
            }
            if ctx.args.has("--secure") {
                meta.insert("secure".into(), json!(true));
            }
            let reply = ctx
                .client()?
                .put(
                    &format!("/_rustybin/portal/items/{}", enc(&id)),
                    Body::Json(Value::Object(meta)),
                )
                .await?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "unpublish" => {
            let id = ctx.args.arg(0, "an item id")?.to_string();
            delete(ctx, &format!("/_rustybin/portal/items/{}", enc(&id)), &[]).await
        }
        "insights" => get(ctx, "/_rustybin/portal/insights", &[]).await,
        "branding" => {
            let doc = ctx.client()?.get("/_rustybin/settings", &[]).await?.value();
            let rows: Vec<Value> = doc["settings"]
                .as_array()
                .map(|a| a.iter().filter(|s| s["group"] == json!("portal")).map(|s| json!({ "key": s["key"], "value": s["value"], "source": s["source"] })).collect())
                .unwrap_or_default();
            ctx.emit(&json!({ "settings": rows }));
            Ok(())
        }
        "open" => {
            let url = format!("{}/portal", ctx.client()?.base);
            ctx.raw(&url);
            Ok(())
        }
        _ => Err(Fail::Usage(format!("portal {verb}: not implemented"))),
    }
}

// ── Config ──────────────────────────────────────────────────────────

fn print_apply_report(ctx: &mut Ctx, report: &Value) {
    if ctx.json {
        ctx.emit(report);
        return;
    }
    let dry = report["dry_run"] == json!(true);
    let summary: Vec<&str> = report["summary"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let problems: Vec<&str> = report["problems"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let sections: Vec<&str> = report["sections"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut text = format!(
        "{} {} change(s){} (sections: {})\n",
        if dry { "would apply" } else { "applied" },
        summary.len(),
        if report["prune"] == json!(true) {
            " with prune"
        } else {
            ""
        },
        sections.join(", ")
    );
    for l in &summary {
        text.push_str(&format!("  {l}\n"));
    }
    for p in &problems {
        text.push_str(&format!("problem: {p}\n"));
    }
    ctx.raw(&text);
}

async fn config(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "export" => {
            let format = ctx.args.value("--format").unwrap_or("yaml").to_string();
            let secrets = ctx.args.value("--secrets").unwrap_or("refs").to_string();
            let output = ctx.args.value("--output").map(str::to_string);
            let query = vec![("format", format.clone()), ("secrets", secrets)];
            if format == "bundle" {
                let Some(file) = output else {
                    return Err(Fail::Usage(
                        "a bundle needs --output FILE (a .tar.gz)".into(),
                    ));
                };
                let bytes = ctx
                    .client()?
                    .bytes("/_rustybin/config/export", &query)
                    .await?;
                std::fs::write(&file, &bytes).map_err(|e| Fail::Usage(format!("{file}: {e}")))?;
                ctx.say(&format!(
                    "Bundle written to {file} ({} bytes).",
                    bytes.len()
                ));
                return Ok(());
            }
            let reply = ctx
                .client()?
                .get("/_rustybin/config/export", &query)
                .await?;
            match output {
                Some(file) => {
                    std::fs::write(&file, reply.text.as_bytes())
                        .map_err(|e| Fail::Usage(format!("{file}: {e}")))?;
                    ctx.say(&format!("Written to {file}."));
                }
                None => {
                    if format == "json" {
                        emit_reply(ctx, &reply);
                    } else {
                        ctx.raw(&reply.text);
                    }
                }
            }
            Ok(())
        }
        "apply" | "diff" => {
            let file = ctx.args.arg(0, "a document file")?.to_string();
            let text = ctx.input(&file)?;
            let dry_run = verb == "diff" || ctx.args.has("--dry-run");
            let prune = ctx.args.has("--prune");
            if prune && !dry_run {
                ctx.confirm("apply with prune (what a present section does not name is removed)?")?;
            }
            let body = json!({ "document": text, "dry_run": dry_run, "prune": prune });
            let report = ctx
                .client()?
                .post("/_rustybin/config/apply", Body::Json(body))
                .await?
                .value();
            print_apply_report(ctx, &report);
            if report["ok"] == json!(false) && !dry_run {
                return Err(Fail::Failed("applied with problems".into()));
            }
            Ok(())
        }
        "schema" => get(ctx, "/_rustybin/config/schema", &[]).await,
        "check" => {
            let file = ctx.args.arg(0, "a document file")?.to_string();
            let path = std::path::Path::new(&file);
            let (cfg, _) = crate::config::Config::load();
            let limits = crate::mocks::store::MockLimits::for_config(&cfg);
            let loaded = crate::mocks::config_file::load_file(
                path,
                |k| std::env::var(k).ok(),
                limits.max_spec_bytes as u64,
            )
            .map_err(Fail::Usage)?;
            let store = crate::mocks::store::MockStore::with_limits(limits);
            let mut ok = loaded.problems.is_empty();
            let mut lines = Vec::new();
            for p in &loaded.problems {
                lines.push(json!({ "mock": "", "status": "problem", "detail": p }));
            }
            for d in loaded.defs {
                match store.compile(&d.source, d.settings) {
                    Ok(c) => {
                        ok &= c.report.ok;
                        lines.push(json!({ "mock": d.name, "status": if c.report.ok { "ok" } else { "errors" }, "detail": format!("{} ({} operations): {} error(s), {} warning(s)", c.spec.title, c.spec.operations.len(), c.report.errors, c.report.warnings) }));
                    }
                    Err(e) => {
                        ok = false;
                        lines.push(json!({ "mock": d.name, "status": "error", "detail": e }));
                    }
                }
            }
            ctx.emit(
                &json!({ "file": file, "sections": loaded.sections, "ok": ok, "mocks": lines }),
            );
            if ok {
                Ok(())
            } else {
                Err(Fail::Failed("the document has problems".into()))
            }
        }
        _ => Err(Fail::Usage(format!("config {verb}: not implemented"))),
    }
}

// ── Git ─────────────────────────────────────────────────────────────

async fn git(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "status" => {
            let query = if ctx.args.has("--fetch") {
                vec![("fetch", "true".to_string())]
            } else {
                Vec::new()
            };
            let v = ctx.client()?.get("/_rustybin/git", &query).await?.value();
            if ctx.json {
                ctx.emit(&v);
            } else {
                let mut shown = v.clone();
                if let Some(m) = shown.as_object_mut() {
                    m.remove("timeline");
                }
                ctx.emit(&shown);
            }
            Ok(())
        }
        "init" => post(ctx, "/_rustybin/git/init", Body::None).await,
        "commit" => {
            let message = ctx
                .args
                .value("--message")
                .or_else(|| ctx.args.value("-m"))
                .ok_or_else(|| Fail::Usage("a message is required: -m \"...\"".into()))?
                .to_string();
            let checkpoints: Vec<String> = ctx
                .args
                .values("--checkpoint")
                .iter()
                .flat_map(|v| v.split(','))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            let mut body = json!({ "message": message });
            if !checkpoints.is_empty() {
                body["checkpoints"] = json!(checkpoints);
            }
            post(ctx, "/_rustybin/git/commit", Body::Json(body)).await
        }
        "push" => post(ctx, "/_rustybin/git/push", Body::None).await,
        "pull" => {
            let body = json!({ "apply": ctx.args.has("--apply"), "dry_run": ctx.args.has("--dry-run"), "prune": ctx.args.has("--prune") });
            if ctx.args.has("--apply") && ctx.args.has("--prune") && !ctx.args.has("--dry-run") {
                ctx.confirm("pull and apply with prune (the repository is the whole truth)?")?;
            }
            let v = ctx
                .client()?
                .post("/_rustybin/git/pull", Body::Json(body))
                .await?
                .value();
            if ctx.json {
                ctx.emit(&v);
            } else {
                ctx.raw(v["detail"].as_str().unwrap_or("pulled"));
                if v["applied"].is_object() {
                    print_apply_report(ctx, &v["applied"]);
                }
            }
            Ok(())
        }
        "resolve" => {
            let with = ctx
                .args
                .value("--with")
                .ok_or_else(|| {
                    Fail::Usage("--with instance, --with repository or --with pull-request".into())
                })?
                .replace('-', "_");
            let dry_run = ctx.args.has("--dry-run");
            let mut body = json!({
                "with": with,
                "message": ctx.args.value("--message").unwrap_or(""),
                "dry_run": dry_run,
            });
            if let Some(branch) = ctx.args.value("--branch") {
                body["branch"] = json!(branch);
            }
            if !dry_run {
                ctx.confirm(&match with.as_str() {
                    "instance" => "git resolve: replace the repository's configuration with this instance's".to_string(),
                    "repository" => "git resolve: replace this instance's configuration with the repository's (with prune)".to_string(),
                    _ => "git resolve: push this instance's side to a new branch for a pull request".to_string(),
                })?;
            }
            let v = ctx
                .client()?
                .post("/_rustybin/git/resolve", Body::Json(body))
                .await?
                .value();
            if ctx.json {
                ctx.emit(&v);
            } else {
                ctx.raw(v["detail"].as_str().unwrap_or("done"));
                for key in ["summary", "commits"] {
                    for line in v[key].as_array().into_iter().flatten() {
                        ctx.raw(&format!("  {}", line.as_str().unwrap_or("")));
                    }
                }
                if v["applied"].is_object() {
                    print_apply_report(ctx, &v["applied"]);
                }
            }
            Ok(())
        }
        "branches" => {
            let v = ctx
                .client()?
                .get("/_rustybin/git/branches", &[])
                .await?
                .value();
            if ctx.json {
                ctx.emit(&v);
            } else {
                for b in v["branches"].as_array().into_iter().flatten() {
                    ctx.raw(&format!(
                        "{} {} {}",
                        if b["current"] == json!(true) {
                            "*"
                        } else {
                            " "
                        },
                        b["name"].as_str().unwrap_or(""),
                        b["short"].as_str().unwrap_or("")
                    ));
                }
                if v["on_remote"] == json!(false) {
                    ctx.raw(&format!(
                        "* {} (not on the remote yet)",
                        v["branch"].as_str().unwrap_or("")
                    ));
                }
                if v["truncated"] == json!(true) {
                    ctx.raw("  (more branches are not listed)");
                }
            }
            Ok(())
        }
        "switch" | "branch" => {
            let create = verb == "branch";
            let name = ctx.args.arg(0, "a branch name")?.to_string();
            let dry_run = ctx.args.has("--dry-run");
            let prune = ctx.args.has("--prune");
            let body = if create {
                json!({
                    "branch": name,
                    "create": true,
                    "dry_run": dry_run,
                    "message": ctx.args.value("--message").or_else(|| ctx.args.value("-m")).unwrap_or(""),
                })
            } else {
                json!({
                    "branch": name,
                    "dry_run": dry_run,
                    "prune": prune,
                    "apply": !ctx.args.has("--no-apply"),
                })
            };
            if !dry_run {
                ctx.confirm(&if create {
                    format!("git branch {name}: commit this instance's configuration to the new branch and push it")
                } else if ctx.args.has("--no-apply") {
                    format!("git switch {name}: move this instance to that branch")
                } else {
                    format!(
                        "git switch {name}: move this instance to that branch and apply its document{}",
                        if prune { " with prune" } else { "" }
                    )
                })?;
            }
            let v = ctx
                .client()?
                .post("/_rustybin/git/branch", Body::Json(body))
                .await?
                .value();
            if ctx.json {
                ctx.emit(&v);
            } else {
                ctx.raw(v["detail"].as_str().unwrap_or("done"));
                for line in v["summary"].as_array().into_iter().flatten() {
                    ctx.raw(&format!("  {}", line.as_str().unwrap_or("")));
                }
                if v["applied"].is_object() {
                    print_apply_report(ctx, &v["applied"]);
                }
            }
            Ok(())
        }
        "signing-key" => {
            let v = if ctx.args.has("--generate") {
                let body = json!({ "replace": ctx.args.has("--replace") });
                ctx.client()?
                    .post("/_rustybin/git/signing-key", Body::Json(body))
                    .await?
                    .value()
            } else {
                ctx.client()?
                    .get("/_rustybin/git/signing-key", &[])
                    .await?
                    .value()
            };
            if ctx.json {
                ctx.emit(&v);
                return Ok(());
            }
            let s = &v["signing"];
            match s["public"].as_str() {
                Some(public) => {
                    ctx.raw(public);
                    ctx.raw(&format!(
                        "fingerprint: {}",
                        s["fingerprint"].as_str().unwrap_or("")
                    ));
                    ctx.raw(&format!(
                        "commits are {}",
                        if s["enabled"] == json!(true) {
                            "signed (git.sign is on)"
                        } else {
                            "not signed (rustybin settings set git.sign true)"
                        }
                    ));
                }
                None => ctx.raw("no signing key yet: rustybin git signing-key --generate"),
            }
            if let Some(problem) = s["problem"].as_str() {
                ctx.raw(&format!("problem: {problem}"));
            }
            Ok(())
        }
        "key" => {
            if ctx.args.has("--generate") {
                let body = json!({ "replace": ctx.args.has("--replace") });
                let v = ctx
                    .client()?
                    .post("/_rustybin/git/key", Body::Json(body))
                    .await?
                    .value();
                if ctx.json {
                    ctx.emit(&v);
                } else {
                    ctx.raw(v["public"].as_str().unwrap_or(""));
                }
            } else {
                let v = ctx.client()?.get("/_rustybin/git/key", &[]).await?.value();
                if ctx.json {
                    ctx.emit(&v);
                } else if let Some(public) = v["deploy_key"]["public"].as_str() {
                    ctx.raw(public);
                } else {
                    return Err(Fail::Failed(
                        "no deploy key yet: rustybin git key --generate".into(),
                    ));
                }
            }
            Ok(())
        }
        "history" => get(ctx, "/_rustybin/git/history", &[]).await,
        _ => Err(Fail::Usage(format!("git {verb}: not implemented"))),
    }
}

// ── Harness ─────────────────────────────────────────────────────────

async fn harness(ctx: &mut Ctx, verb: &str) -> Result<()> {
    let first = ctx.args.positional.first().cloned().unwrap_or_default();
    let second = ctx.args.positional.get(1).cloned().unwrap_or_default();
    let runs = "/_rustybin/harness/runs";
    let cluster = format!("/_rustybin/harness/clusters/{}", enc(&first));
    let scenario_body = |ctx: &Ctx, path: &str| -> Result<Body> {
        let text = ctx.input(path)?;
        Ok(Body::Text(text.clone(), content_type_for(path, &text)))
    };
    match verb {
        "status" => get(ctx, "/_rustybin/harness", &[]).await,
        "schema" => get(ctx, "/_rustybin/harness/schema", &[]).await,
        "scenarios" => get(ctx, "/_rustybin/harness/scenarios", &[]).await,
        "scenario" => {
            ctx.args.arg(0, "a scenario name")?;
            let path = format!("/_rustybin/harness/scenarios/{}", enc(&first));
            if ctx.args.has("--yaml") {
                let reply = ctx
                    .client()?
                    .get(&path, &[("format", "yaml".into())])
                    .await?;
                ctx.raw(&reply.text);
                Ok(())
            } else {
                get(ctx, &path, &[]).await
            }
        }
        "save-scenario" => {
            ctx.args.arg(0, "a scenario name")?;
            let file = ctx.args.arg(1, "a scenario file")?.to_string();
            let body = scenario_body(ctx, &file)?;
            let reply = ctx
                .client()?
                .put(
                    &format!("/_rustybin/harness/scenarios/{}", enc(&first)),
                    body,
                )
                .await?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "delete-scenario" => {
            ctx.args.arg(0, "a scenario name")?;
            delete(
                ctx,
                &format!("/_rustybin/harness/scenarios/{}", enc(&first)),
                &[],
            )
            .await
        }
        "import" => {
            ctx.args
                .arg(0, "a recording, a collection or a script file")?;
            let text = ctx.input(&first)?;
            let path = match ctx.args.value("--format") {
                Some(format) => format!("/_rustybin/harness/import?format={}", enc(format)),
                None => "/_rustybin/harness/import".to_string(),
            };
            // Values for the file's variables go in an envelope with its
            // text. A refusal names the variable, never what was typed.
            let mut variables = serde_json::Map::new();
            for pair in ctx.args.values("--var") {
                let (name, value) = pair
                    .split_once('=')
                    .map(|(n, v)| (n.trim(), v))
                    .filter(|(n, _)| !n.is_empty())
                    .ok_or_else(|| Fail::Usage("--var takes NAME=value".into()))?;
                variables.insert(name.to_string(), Value::String(value.to_string()));
            }
            if variables.is_empty() {
                // JSON, or YAML for an Artillery script: the instance reads both.
                let content_type = content_type_for(&first, &text);
                post(ctx, &path, Body::Text(text, content_type)).await
            } else {
                let envelope = json!({ "document": text, "variables": variables });
                post(ctx, &path, Body::Json(envelope)).await
            }
        }
        "validate" | "preview" => {
            ctx.args.arg(0, "a scenario file")?;
            let body = scenario_body(ctx, &first)?;
            post(ctx, &format!("/_rustybin/harness/{verb}"), body).await
        }
        "runs" => get(ctx, runs, &[]).await,
        "run" => {
            ctx.args.arg(0, "a scenario file")?;
            let body = scenario_body(ctx, &first)?;
            let started = ctx.client()?.post(runs, body).await?.value();
            let id = started["run_id"]
                .as_str()
                .or_else(|| started["id"].as_str())
                .unwrap_or("")
                .to_string();
            if !ctx.args.has("--wait") {
                ctx.emit(&started);
                return Ok(());
            }
            ctx.say(&format!("run {id} started; following it"));
            let json_out = ctx.json;
            let quiet = ctx.quiet;
            let client = ctx.client()?;
            let mut progress: Vec<String> = Vec::new();
            client
                .sse(
                    Method::GET,
                    &format!("{runs}/{}/events", enc(&id)),
                    Body::None,
                    None,
                    Duration::from_secs(86_400),
                    &mut |event, data| {
                        if event == "snapshot" && !quiet && !json_out {
                            let v: Value = serde_json::from_str(data).unwrap_or(Value::Null);
                            progress.push(format!(
                                "{}: {} sent, {} errors",
                                v["state"].as_str().unwrap_or("?"),
                                v["requests"]["sent"]
                                    .as_u64()
                                    .or_else(|| v["sent"].as_u64())
                                    .unwrap_or(0),
                                v["requests"]["errors"]
                                    .as_u64()
                                    .or_else(|| v["errors"].as_u64())
                                    .unwrap_or(0)
                            ));
                        }
                        event != "finished"
                    },
                )
                .await?;
            for p in progress.iter().rev().take(1) {
                ctx.say(p);
            }
            let format = ctx.args.value("--format").unwrap_or("text").to_string();
            let reply = ctx
                .client()?
                .get(
                    &format!("{runs}/{}/report", enc(&id)),
                    &[("format", format.clone())],
                )
                .await?;
            if format == "json" || ctx.json {
                emit_reply(ctx, &reply);
            } else {
                ctx.raw(&reply.text);
            }
            let failed = reply
                .json
                .as_ref()
                .is_some_and(|j| j["passed"] == json!(false))
                || reply.text.contains("Result: FAIL");
            if failed {
                return Err(Fail::Failed("the thresholds failed".into()));
            }
            Ok(())
        }
        "show" => {
            ctx.args.arg(0, "a run id")?;
            get(ctx, &format!("{runs}/{}", enc(&first)), &[]).await
        }
        "tune" => {
            ctx.args.arg(0, "a run id")?;
            let change = ctx.args.arg(1, "a change (JSON or a file)")?.to_string();
            let body: Value = serde_json::from_str(&ctx.input(&change)?)
                .map_err(|e| Fail::Usage(format!("the change is not JSON: {e}")))?;
            let reply = ctx
                .client()?
                .patch(&format!("{runs}/{}", enc(&first)), Body::Json(body))
                .await?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "stop" => {
            ctx.args.arg(0, "a run id")?;
            post(ctx, &format!("{runs}/{}/stop", enc(&first)), Body::None).await
        }
        "stop-all" => post(ctx, "/_rustybin/harness/stop", Body::None).await,
        "events" => {
            ctx.args.arg(0, "a run id")?;
            tail(ctx, &format!("{runs}/{}/events", enc(&first))).await
        }
        "report" => {
            ctx.args.arg(0, "a run id")?;
            let format = ctx.args.value("--format").unwrap_or("text").to_string();
            let reply = ctx
                .client()?
                .get(
                    &format!("{runs}/{}/report", enc(&first)),
                    &[("format", format)],
                )
                .await?;
            if ctx.json {
                emit_reply(ctx, &reply);
            } else {
                ctx.raw(&reply.text);
            }
            Ok(())
        }
        "clusters" => get(ctx, "/_rustybin/harness/clusters", &[]).await,
        "create-cluster" => {
            let body = ctx.json_flag("--body")?.unwrap_or_else(|| json!({}));
            post(ctx, "/_rustybin/harness/clusters", Body::Json(body)).await
        }
        "cluster" => {
            ctx.args.arg(0, "a cluster id")?;
            get(ctx, &cluster, &[]).await
        }
        "delete-cluster" => {
            ctx.args.arg(0, "a cluster id")?;
            delete(ctx, &cluster, &[]).await
        }
        "step-aside" => {
            ctx.args.arg(0, "a cluster id")?;
            post(ctx, &format!("{cluster}/leader/step-aside"), Body::None).await
        }
        "leader" | "upstreams" => {
            ctx.args.arg(0, "a cluster id")?;
            get(ctx, &format!("{cluster}/{verb}"), &[]).await
        }
        "cluster-runs" => {
            ctx.args.arg(0, "a cluster id")?;
            get(ctx, &format!("{cluster}/runs"), &[]).await
        }
        "cluster-run" => {
            ctx.args.arg(0, "a cluster id")?;
            let file = ctx.args.arg(1, "a scenario file")?.to_string();
            let body = scenario_body(ctx, &file)?;
            post(ctx, &format!("{cluster}/runs"), body).await
        }
        "cluster-show" => {
            ctx.args.arg(0, "a cluster id")?;
            ctx.args.arg(1, "a run id")?;
            get(ctx, &format!("{cluster}/runs/{}", enc(&second)), &[]).await
        }
        "cluster-tune" => {
            ctx.args.arg(0, "a cluster id")?;
            ctx.args.arg(1, "a run id")?;
            let change = ctx.args.arg(2, "a change (JSON or a file)")?.to_string();
            let body: Value = serde_json::from_str(&ctx.input(&change)?)
                .map_err(|e| Fail::Usage(format!("the change is not JSON: {e}")))?;
            let reply = ctx
                .client()?
                .patch(
                    &format!("{cluster}/runs/{}", enc(&second)),
                    Body::Json(body),
                )
                .await?;
            emit_reply(ctx, &reply);
            Ok(())
        }
        "cluster-start" | "cluster-stop" => {
            ctx.args.arg(0, "a cluster id")?;
            ctx.args.arg(1, "a run id")?;
            let action = verb.trim_start_matches("cluster-");
            post(
                ctx,
                &format!("{cluster}/runs/{}/{action}", enc(&second)),
                Body::None,
            )
            .await
        }
        "cluster-events" => {
            ctx.args.arg(0, "a cluster id")?;
            ctx.args.arg(1, "a run id")?;
            tail(ctx, &format!("{cluster}/runs/{}/events", enc(&second))).await
        }
        "cluster-report" => {
            ctx.args.arg(0, "a cluster id")?;
            ctx.args.arg(1, "a run id")?;
            let format = ctx.args.value("--format").unwrap_or("text").to_string();
            let reply = ctx
                .client()?
                .get(
                    &format!("{cluster}/runs/{}/report", enc(&second)),
                    &[("format", format)],
                )
                .await?;
            if ctx.json {
                emit_reply(ctx, &reply);
            } else {
                ctx.raw(&reply.text);
            }
            Ok(())
        }
        "join" => {
            ctx.args.arg(0, "a cluster URL")?;
            let token = ctx
                .args
                .value("--join-token")
                .ok_or_else(|| Fail::Usage("--join-token is required".into()))?;
            let mut body = json!({ "url": first, "join_token": token });
            if let Some(a) = ctx.args.value("--advertise") {
                body["advertise"] = json!(a);
            }
            post(ctx, "/_rustybin/harness/join", Body::Json(body)).await
        }
        "leave" => post(ctx, "/_rustybin/harness/leave", Body::None).await,
        _ => Err(Fail::Usage(format!("harness {verb}: not implemented"))),
    }
}

/// Who an audit entry names, for people: a user's name, else the
/// credential's kind (and name), plus the agent.
fn audit_actor(a: &Value) -> String {
    let kind = a["kind"].as_str().unwrap_or("");
    let mut s = match (kind, a["name"].as_str()) {
        ("user", Some(n)) => n.to_string(),
        (k, Some(n)) => format!("{k}:{n}"),
        (k, None) => k.to_string(),
    };
    if let Some(agent) = a["agent"].as_str() {
        s.push_str(&format!(" (agent {agent})"));
    }
    s
}

/// An audit entry's targets as `kind:name`, comma separated.
fn audit_targets(e: &Value) -> String {
    e["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            format!(
                "{}:{}",
                t["kind"].as_str().unwrap_or(""),
                t["name"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// An audit entry's result: the status or the outcome, with a refusal's
/// permission.
fn audit_result(e: &Value) -> String {
    let r = &e["result"];
    let mut s = match r["status"].as_u64() {
        Some(code) => code.to_string(),
        None => r["outcome"].as_str().unwrap_or("").to_string(),
    };
    if let Some(p) = r["permission"].as_str() {
        s.push_str(&format!(" {p}"));
    } else if let Some(reason) = r["reason"].as_str() {
        s.push_str(&format!(" {reason}"));
    }
    s
}

/// `rustybin audit list | tail`.
async fn audit(ctx: &mut Ctx, verb: &str) -> Result<()> {
    let filters = [
        ("--actor", "actor"),
        ("--action", "action"),
        ("--target", "target"),
        ("--since", "since"),
        ("--until", "until"),
        ("--result", "result"),
    ];
    match verb {
        "list" => {
            let mut query = q(&ctx.args.clone(), &filters);
            query.extend(q(
                &ctx.args.clone(),
                &[("--limit", "limit"), ("--cursor", "cursor")],
            ));
            let v = ctx.client()?.get("/_rustybin/audit", &query).await?.value();
            if ctx.json {
                ctx.emit(&v);
                return Ok(());
            }
            let rows: Vec<Value> = v["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| {
                    json!({
                        "id": e["id"],
                        "time": e["time"],
                        "actor": audit_actor(&e["actor"]),
                        "action": match e["event"].as_str() {
                            Some(ev) => format!("{} [{ev}]", e["action"].as_str().unwrap_or("")),
                            None => e["action"].as_str().unwrap_or("").to_string(),
                        },
                        "targets": audit_targets(e),
                        "result": audit_result(e),
                    })
                })
                .collect();
            ctx.emit(&json!({ "entries": rows }));
            if let Some(c) = v["next_cursor"].as_str() {
                ctx.say(&format!("older entries: rustybin audit list --cursor {c}"));
            }
            Ok(())
        }
        "tail" => {
            let query = q(&ctx.args.clone(), &filters);
            let path = if query.is_empty() {
                "/_rustybin/audit/stream".to_string()
            } else {
                let mut ser = form_urlencoded::Serializer::new(String::new());
                for (k, v) in &query {
                    ser.append_pair(k, v);
                }
                format!("/_rustybin/audit/stream?{}", ser.finish())
            };
            tail(ctx, &path).await
        }
        _ => Err(Fail::Usage(format!("audit {verb}: not implemented"))),
    }
}
