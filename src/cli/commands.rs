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
    ("password", "Change the console user name and password"),
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
];

const NO_FLAGS: &[Flag] = &[];
const TAIL_FLAGS: &[Flag] = &[
    Flag::value("--max", "Stop after this many events"),
    Flag::value("--timeout", "Stop after this many seconds (default 3600)"),
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
    cmd!("login", "", "[--url U] [--user U] [--password-stdin] [--profile P]", "Sign in with the console user name and password (prompted, or the first line of stdin) and keep the token in a profile; --token keeps a token you already have.",
        &[Flag::value("--user", "The console user name (default rustybin)"), Flag::switch("--password-stdin", "Read the password from the first line of stdin")],
        [("POST", "/_rustybin/login"), ("GET", "/_rustybin/session")], false, "login --url https://mocks.example.com --user rustybin"),
    cmd!("logout", "", "", "Revoke the profile's token on the instance and forget it.", NO_FLAGS, [("POST", "/_rustybin/logout")], false, "logout"),
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
    cmd!("password", "change", "[--user U] [--stdin]", "Change the console password (and user name): the current and the new one prompted, or two lines of stdin.", &[Flag::value("--user", "A new user name"), Flag::switch("--stdin", "Read the current and the new password from two lines of stdin")], [("PUT", "/_rustybin/password")], false, "password change --stdin < passwords.txt"),
    // ── Mocks ──
    cmd!("mocks", "list", "[--all]", "The mocks with their state and traffic.", &[Flag::switch("--all", "Every visitor's mocks too (admin)")], [("GET", "/_rustybin/mocks")], false, "mocks list"),
    cmd!("mocks", "get", "<name>", "One mock: settings, operations, the deploy report.", NO_FLAGS, [("GET", "/_rustybin/mocks/{name}")], false, "mocks get petstore"),
    cmd!("mocks", "create", "<name> --spec FILE", "Create a mock from an OpenAPI file (YAML or JSON).", &[Flag::value("--spec", "The OpenAPI document"), Flag::value("--mode", "examples, generated, static or stateful"), Flag::switch("--disabled", "Create it switched off"), Flag::switch("--no-deploy", "Keep it as a draft"), Flag::switch("--strip-server-prefix", "Serve the paths without the spec's server prefix")], [("POST", "/_rustybin/mocks")], false, "mocks create orders --spec orders.yaml --mode generated"),
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
    cmd!("mocks", "spec", "<name>", "The mock's OpenAPI document as stored.", NO_FLAGS, [("GET", "/_rustybin/mocks/{name}/spec")], false, "mocks spec orders > orders.yaml"),
    cmd!("mocks", "revision", "<name> <revision>", "A kept revision: what changed since in the draft, its settings and spec (the whole answer with --json).", &[Flag::switch("--spec", "Print only the revision's spec as uploaded")], [("GET", "/_rustybin/mocks/{name}/revisions/{revision}")], false, "mocks revision orders 12"),
    cmd!("mocks", "export", "[name]", "One mock, or all, as a version 1 rustybin.yaml.", NO_FLAGS, [("GET", "/_rustybin/mocks/{name}/export"), ("GET", "/_rustybin/mocks/export")], false, "mocks export orders"),
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
    cmd!("checkpoint", "list", "[--from-git]", "The checkpoints and the mocks each holds; --from-git lists the configuration repository's (after git pull).", &[Flag::switch("--from-git", "The repository's checkpoints, at the working copy's HEAD")], [("GET", "/_rustybin/checkpoints")], false, "checkpoint list"),
    cmd!("checkpoint", "show", "<name>", "One checkpoint.", NO_FLAGS, [("GET", "/_rustybin/checkpoints/{name}")], false, "checkpoint show demo-start"),
    cmd!("checkpoint", "save", "<name> [--replace]", "Save the instance's API mocks (with their data) and the health toggle.", &[Flag::switch("--replace", "Overwrite a checkpoint of that name")], [("POST", "/_rustybin/checkpoints")], false, "checkpoint save demo-start"),
    cmd!("checkpoint", "restore", "<name> [--dry-run] [--from-git]", "Bring the API mocks, their data and health back (other API mocks are deleted, flaky counters reset); --dry-run says what would change, --from-git restores the configuration repository's checkpoint.", &[Flag::switch("--dry-run", "Only say what a restore would change"), Flag::switch("--from-git", "The repository's checkpoint of that name (git pull first)")],[("POST", "/_rustybin/checkpoints/{name}/restore")], true, "checkpoint restore demo-start --yes"),
    cmd!("checkpoint", "delete", "<name>", "Delete a checkpoint (the instance is not changed).", NO_FLAGS, [("DELETE", "/_rustybin/checkpoints/{name}")], true, "checkpoint delete demo-start --yes"),
    cmd!("tests", "list", "", "The saved tests and the variables each uses.", NO_FLAGS, [("GET", "/_rustybin/tests")], false, "tests list"),
    cmd!("tests", "show", "<name>", "One saved test.", NO_FLAGS, [("GET", "/_rustybin/tests/{name}")], false, "tests show order-1"),
    cmd!("tests", "save", "<name> --capture ID [--gateway U] [--description D]", "Save a captured request as a test (its status asserted; credentials become {{variables}}).", &[Flag::value("--capture", "The captured request's id (rustybin requests list)"), Flag::value("--gateway", "Send it through this gateway base URL"), Flag::value("--description", "What it checks")], [("POST", "/_rustybin/tests")], false, "tests save order-1 --capture 0123456789abcdef"),
    cmd!("tests", "put", "<name> --file F", "Save or replace a test from a file (JSON or YAML: request, gateway, assert).", &[Flag::value("--file", "The test: a file, - for stdin, or inline JSON")], [("PUT", "/_rustybin/tests/{name}")], false, r#"tests put order-1 --file '{"request":{"path":"/status/200"},"assert":[{"status":200}]}'"#),
    cmd!("tests", "delete", "<name>", "Delete a saved test.", NO_FLAGS, [("DELETE", "/_rustybin/tests/{name}")], true, "tests delete order-1 --yes"),
    cmd!("tests", "run", "[--names a,b] [--var NAME=value]... [--junit]", "Run saved tests (all by default); exits 1 when one fails. --junit prints JUnit XML for CI.", &[Flag::value("--names", "Only these tests, comma separated"), Flag::value("--var", "A {{variable}}: NAME=value, or NAME=- (stdin) or NAME=@file for a credential (repeat it)"), Flag::switch("--junit", "Print the JUnit XML report")], [("POST", "/_rustybin/tests/run")], false, "tests run --junit"),
    cmd!("tests", "export", "", "Every saved test as one Hurl file (hurl --test --variable base_url=...).", NO_FLAGS, [("GET", "/_rustybin/tests/export")], false, "tests export"),
    cmd!("tests", "runs", "[--limit N] [--cursor C]", "The history of test runs, newest first: when, the trigger, who, the counts, what changed since the previous run.", &[Flag::value("--limit", "Runs a page (20, at most 100)"), Flag::value("--cursor", "The next_cursor of the page before")], [("GET", "/_rustybin/tests/runs")], false, "tests runs --limit 5"),
    cmd!("tests", "run-show", "<id> [--junit]", "One kept run: each test's result and failures, the change since the previous run; --junit prints its JUnit XML.", &[Flag::switch("--junit", "Print the JUnit XML report")], [("GET", "/_rustybin/tests/runs/{id}")], false, "tests run-show 20261103T101500000Z-abc123 --junit"),
    cmd!("tests", "runs-clear", "", "Forget every kept test run (the tests stay).", NO_FLAGS, [("DELETE", "/_rustybin/tests/runs")], true, "tests runs-clear --yes"),
    cmd!("tests", "schedules", "", "The saved tests on a schedule: when, what, on or off, the last result, the next time, the last notice.", NO_FLAGS, [("GET", "/_rustybin/tests/schedules")], false, "tests schedules"),
    cmd!("tests", "schedule-show", "<name>", "One test schedule with its status and recent outcomes.", NO_FLAGS, [("GET", "/_rustybin/tests/schedules/{name}")], false, "tests schedule-show nightly"),
    cmd!("tests", "schedule-save", "<name> (--every D | --at HH:MM) [--names a,b | --prefix P] [--webhook U] [--notify-recovery] [--disabled]", "Save or replace a test schedule: every 5m to 24h, or at HH:MM UTC; every saved test unless --names or --prefix.", &[Flag::value("--every", "Run every 5m to 24h (15m, 2h)"), Flag::value("--at", "Run once a day at HH:MM, UTC"), Flag::value("--names", "Only these tests, comma separated"), Flag::value("--prefix", "Only the tests whose name starts with this"), Flag::value("--webhook", "Post a JSON summary of a failing run here (allow its host: egress purpose notify)"), Flag::switch("--notify-recovery", "One more notice when a run passes after a failure"), Flag::switch("--disabled", "Saved, but off"), Flag::value("--description", "What it is for")], [("PUT", "/_rustybin/tests/schedules/{name}")], false, "tests schedule-save nightly --every 1h --prefix order"),
    cmd!("tests", "schedule-run", "<name>", "Run a test schedule now, as the scheduler would (kept in the history); exits 1 when it fails.", NO_FLAGS, [("POST", "/_rustybin/tests/schedules/{name}/run")], false, "tests schedule-run nightly"),
    cmd!("tests", "schedule-delete", "<name>", "Delete a test schedule (the tests and the history stay).", NO_FLAGS, [("DELETE", "/_rustybin/tests/schedules/{name}")], true, "tests schedule-delete nightly --yes"),
    cmd!("scenario", "list", "", "The gateway scenarios (retries, rate limiting, credential injection, timeouts, caching, circuit breakers, request size limits, CORS, header transformation) and your runs.", NO_FLAGS, [("GET", "/_rustybin/scenarios")], false, "scenario list"),
    cmd!("scenario", "start", "<scenario>", "Prepare a run: its upstream and the steps to configure on the gateway.", NO_FLAGS, [("POST", "/_rustybin/scenarios/{id}/runs")], false, "scenario start caching"),
    cmd!("scenario", "show", "<scenario> <run>", "A run: the setup, what it created, the last verdict.", NO_FLAGS, [("GET", "/_rustybin/scenarios/{id}/runs/{run}")], false, "scenario show caching 1a2b3c4d"),
    cmd!("scenario", "check", "<scenario> <run> --gateway U [--limit N] [--half-open-ms MS] [--size-limit BYTES] [--allowed-origin O] [--refused-origin O] [--upstream-cors] [--add-request-header H] [--add-request-value V] [--remove-request-header H] [--add-response-header H]", "Send the run's test requests through the gateway and compare what the client got with what reached the upstream; exits 1 when an expectation fails.", &[Flag::value("--gateway", "The URL the gateway serves the route at"), Flag::value("--limit", "rate_limiting: the limit set on the gateway"), Flag::value("--half-open-ms", "circuit_breaker: the gateway's half-open time in milliseconds"), Flag::value("--size-limit", "request_size_limit: the gateway's body limit in bytes"), Flag::value("--allowed-origin", "cors: an origin the gateway allows (scheme://host[:port])"), Flag::value("--refused-origin", "cors: an origin the gateway refuses"), Flag::switch("--upstream-cors", "cors: the gateway lets preflights through and the upstream answers CORS"), Flag::value("--add-request-header", "header_transformation: the request header the gateway adds"), Flag::value("--add-request-value", "header_transformation: the value it adds (optional)"), Flag::value("--remove-request-header", "header_transformation: the request header the gateway removes"), Flag::value("--add-response-header", "header_transformation: the response header the gateway adds")], [("POST", "/_rustybin/scenarios/{id}/runs/{run}/check")], false, "scenario check caching 1a2b3c4d --gateway http://localhost:8000/catalog-api"),
    cmd!("scenario", "end", "<scenario> <run>", "End a run: remove the mock and counters it created, nothing else.", NO_FLAGS, [("DELETE", "/_rustybin/scenarios/{id}/runs/{run}")], true, "scenario end caching 1a2b3c4d --yes"),
    cmd!("bins", "list", "", "Your request bins.", NO_FLAGS, [("GET", "/bin")], false, "bins list"),
    cmd!("bins", "create", "[--status N] [--body B] [--delay-ms N]", "A new bin, with the answer it gives.", &[Flag::value("--status", "The status the bin answers"), Flag::value("--body", "The body it answers: a file or inline JSON"), Flag::value("--delay-ms", "A delay before the answer")], [("POST", "/bin")], false, "bins create --status 202"),
    cmd!("bins", "delete", "<id>", "Delete a bin.", NO_FLAGS, [("DELETE", "/bin/{id}")], true, "bins delete 0123456789abcdef01234567 --yes"),
    cmd!("bins", "requests", "<id> [--limit N]", "What a bin captured.", &[Flag::value("--limit", "At most this many")], [("GET", "/bin/{id}/requests")], false, "bins requests 0123456789abcdef01234567"),
    cmd!("bins", "get", "<id> <n>", "One captured request of a bin.", NO_FLAGS, [("GET", "/bin/{id}/requests/{n}")], false, "bins get 0123456789abcdef01234567 1"),
    cmd!("bins", "tail", "<id>", "Follow a bin's requests.", TAIL_FLAGS, [("GET", "/bin/{id}/requests/stream")], false, "bins tail 0123456789abcdef01234567 --max 3"),
    // ── Agents ──
    cmd!("agents", "list", "", "Every AI agent that called the management server, and the policy.", NO_FLAGS, [("GET", "/_rustybin/agents")], false, "agents list"),
    cmd!("agents", "get", "<id>", "One agent: tools, last errors, sessions.", NO_FLAGS, [("GET", "/_rustybin/agents/{id}")], false, "agents get 1f3a..."),
    cmd!("agents", "rights", "<id> [--ceiling C] [--allow T] [--deny T] [--note N] [--reset]", "Limit an agent: a ceiling, tools allowed or denied, a note; --reset goes back to the rules.", &[Flag::value("--ceiling", "read-only, traffic, manage or full"), Flag::value("--allow", "Tool names, comma separated"), Flag::value("--deny", "Tool names, comma separated"), Flag::value("--note", "A note"), Flag::switch("--reset", "Drop the agent's own rights and note")], [("PATCH", "/_rustybin/agents/{id}")], false, "agents rights 1f3a... --ceiling read-only --deny try_request"),
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
    cmd!("portal", "status", "", "The portal: on or off, items, who may read it.", NO_FLAGS, [("GET", "/_rustybin/portal")], false, "portal status"),
    cmd!("portal", "publish", "<id>", "Publish an item: mock:NAME, mcp:SERVER, llm, a2a:AGENT, tools or registry.", &[Flag::value("--title", "A title"), Flag::value("--summary", "A summary (Markdown)"), Flag::value("--tags", "Tags, comma separated"), Flag::value("--contact", "A contact"), Flag::value("--order", "Lower comes first"), Flag::switch("--no-credentials", "Hide a mock's demo credentials")], [("PUT", "/_rustybin/portal/items/{id}")], false, "portal publish mock:orders --title \"Orders API\" --tags payments"),
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
    cmd!("git", "resolve", "--with instance|repository", "After a diverged branch: the instance's copy wins, or the repository's.", &[Flag::value("--with", "instance or repository"), Flag::value("--message", "The commit message when the instance wins")], [("POST", "/_rustybin/git/resolve")], true, "git resolve --with repository --yes"),
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
    // ── Keys and egress ──
    cmd!("keys", "usage", "[--day D] [--after A] [--limit N]", "Per-key usage in shared mode.", &[Flag::value("--day", "A UTC day (YYYY-MM-DD)"), Flag::value("--after", "The page cursor"), Flag::value("--limit", "At most this many")], [("GET", "/_rustybin/keys/usage")], false, "keys usage --day 2026-10-30"),
    cmd!("egress", "show", "", "The outbound policy: the allowlist, each feature's rule, the last decisions.", NO_FLAGS, [("GET", "/_rustybin/egress")], false, "egress show"),
];

/// Catalogue paths no command calls, each with its reason (the parity test
/// reads it).
pub const UNCOVERED: &[(&str, &str)] = &[
    (
        "/_rustybin/mcp",
        "the management MCP endpoint: rustybin mcp-stdio bridges a client to it",
    ),
    (
        "/_rustybin/oauth/authorize",
        "the agent sign-in a browser walks",
    ),
    (
        "/_rustybin/oauth/register",
        "the agent sign-in a browser walks",
    ),
    (
        "/_rustybin/oauth/token",
        "the agent sign-in a browser walks",
    ),
    (
        "/_rustybin/oauth/requests/{id}",
        "the console's consent screen",
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
        "password" => password(ctx).await,
        "mocks" => mocks(ctx, cmd.verb).await,
        "visits" => visits(ctx, cmd.verb).await,
        "visit" => visit(ctx, cmd.verb).await,
        "requests" => requests(ctx, cmd.verb).await,
        "bins" => bins(ctx, cmd.verb).await,
        "agents" => agents(ctx, cmd.verb).await,
        "scenario" => scenario(ctx, cmd.verb).await,
        "tests" => tests(ctx, cmd.verb).await,
        "checkpoint" => checkpoint(ctx, cmd.verb).await,
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
        "egress" => get(ctx, "/_rustybin/egress", &[]).await,
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
    let (token, expires_at, user) = match ctx.args.value("--token") {
        Some(t) => {
            let client = super::client::Client::new(&url, Some(t.to_string()))?;
            let session = client.get("/_rustybin/session", &[]).await?.value();
            (
                t.to_string(),
                session["expires_at"].as_str().map(str::to_string),
                session["user"].as_str().map(str::to_string),
            )
        }
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
    all.profiles.insert(
        name.clone(),
        profiles::Profile {
            url: url.clone(),
            token: Some(token),
            expires_at: expires_at.clone(),
            user: user.clone(),
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
        if let Some(p) = all.profiles.get_mut(&name) {
            p.token = None;
            p.expires_at = None;
            p.user = None;
        }
        profiles::save(&path, &all)?;
        ctx.say(&format!("Signed out of {} (profile {name}).", resolved.url));
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
        _ => Err(Fail::Usage(format!("settings {verb}: not implemented"))),
    }
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

async fn password(ctx: &mut Ctx) -> Result<()> {
    let from_stdin = ctx.args.has("--stdin");
    let current = ctx.secret("Current password", from_stdin, 0)?;
    let new = ctx.secret("New password", from_stdin, 1)?;
    let mut body = json!({ "current_password": current, "new_password": new });
    if let Some(u) = ctx.args.value("--user") {
        body["username"] = json!(u);
    }
    let reply = ctx
        .client()?
        .put("/_rustybin/password", Body::Json(body))
        .await?;
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
            let query = if ctx.args.has("--all") {
                vec![("owner", "all".to_string())]
            } else {
                Vec::new()
            };
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
        "get" => {
            need_name(ctx)?;
            get(ctx, &base, &[]).await
        }
        "create" => {
            need_name(ctx)?;
            let spec_path = ctx
                .args
                .value("--spec")
                .ok_or_else(|| Fail::Usage("--spec FILE is required".into()))?
                .to_string();
            let spec = ctx.input(&spec_path)?;
            let mut body = Map::new();
            body.insert("name".into(), json!(name));
            body.insert("spec".into(), json!(spec));
            body.insert("enabled".into(), json!(!ctx.args.has("--disabled")));
            body.insert("deploy".into(), json!(!ctx.args.has("--no-deploy")));
            if ctx.args.has("--strip-server-prefix") {
                body.insert("strip_server_prefix".into(), json!(true));
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

async fn checkpoint(ctx: &mut Ctx, verb: &str) -> Result<()> {
    let from_git = ctx.args.has("--from-git");
    if verb == "list" {
        let query: Vec<(&str, String)> = if from_git {
            vec![("from", "git".to_string())]
        } else {
            Vec::new()
        };
        return get(ctx, "/_rustybin/checkpoints", &query).await;
    }
    let name = ctx.args.arg(0, "a checkpoint name")?.to_string();
    let path = format!("/_rustybin/checkpoints/{}", enc(&name));
    match verb {
        "show" => get(ctx, &path, &[]).await,
        "delete" => delete(ctx, &path, &[]).await,
        "save" => {
            let body = json!({ "name": name, "replace": ctx.args.has("--replace") });
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

// ── Saved tests ─────────────────────────────────────────────────────

async fn tests(ctx: &mut Ctx, verb: &str) -> Result<()> {
    match verb {
        "list" => get(ctx, "/_rustybin/tests", &[]).await,
        "export" => get(ctx, "/_rustybin/tests/export", &[]).await,
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
async fn tests_run(ctx: &mut Ctx) -> Result<()> {
    let mut body = json!({});
    if let Some(names) = list_flag(&ctx.args, "--names") {
        body["names"] = json!(names);
    }
    // A credential never on the command line: `NAME=-` reads stdin (or
    // prompts), `NAME=@file` a file; a literal value is refused for a
    // secret-looking name.
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
            ctx.emit(&json!({ "run": v["id"], "upstream": v["upstream"], "setup": v["setup"] }));
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
            let v = ctx.client()?.get("/_rustybin/agents", &[]).await?.value();
            if ctx.json {
                ctx.emit(&v);
            } else {
                let rows: Vec<Value> = v["agents"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|x| json!({ "id": x["id"], "name": x["name"], "version": x["version"], "credential": x["credential"]["kind"], "status": x["status"], "ceiling": x["rights"]["ceiling"], "calls_today": x["calls"]["today"], "last_seen": x["last_seen"] }))
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
                .ok_or_else(|| Fail::Usage("--with instance or --with repository".into()))?
                .to_string();
            let body =
                json!({ "with": with, "message": ctx.args.value("--message").unwrap_or("") });
            post(ctx, "/_rustybin/git/resolve", Body::Json(body)).await
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
