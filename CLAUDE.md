# Rustybin: notes for coding agents

HTTP stub service for API and AI gateway demos (Rust, axum 0.8, tokio). Crate is lib + bin.

## Rustybin 0.2, the AI-assisted mocking service (built, released 2026-10-30)

[docs/plans/next/master-plan.md](docs/plans/next/master-plan.md) (planned 2026-10-29, every step done, `v0.2.0`
tagged 2026-10-30) is the plan of the 0.2 build: twelve steps, one file each (`docs/plans/next/step-NN-*.md`): foundations (step 01, done 2026-10-29: the
settings store `src/settings.rs`, the egress policy `src/egress.rs`, the console's Management view
`ui/views/manage.js`; see the step file's build notes), the console locked with a username and password (step 02,
done 2026-10-29: `src/login.rs`, the one change of default; test and example instances that read the control plane
without a token set `RUSTYBIN_CONTROL_AUTH=open`), AI assist with the user's own key (step 03, done 2026-10-29:
`src/assist/`, see below; Anthropic, OpenAI, Gemini, OpenAI-compatible, or the built-in mock provider for tests;
the console's assist bar, drawer and `ui/lib/assist.js`), AI-built mocks (step 04, done 2026-10-29: the tasks in
`src/assist/tasks/`, the routes in `src/mocks/ai.rs`, stored examples and coverage in the mocks model and checks,
`ui/views/mocks/ai.js`; specs from a
prompt to the chosen OpenAPI version, example payloads with an alert when a spec has none, AI edits through a
diff), an MCP inspector for any server with every auth mode and a relay (step 05, done 2026-10-30:
`src/mcp/inspector/`, `ui/lib/mcpclient.js`, `ui/lib/mcpauth.js`, `ui/views/mcp.js`, see below), a mock MCP registry and registry
browsing (step 06, done 2026-10-30: `src/mcp/registry/`, `ui/views/registry.js`, see below), a dev portal (step 07, done
2026-10-30: `src/portal/`, `ui-portal/`, see below), AI agents management (step 08, done 2026-10-30:
`src/mcp/manage/agents/`, `ui/views/agents.js`, see below), the whole instance as `rustybin.yaml` with git source
control (step 09, done 2026-10-30: `src/configdoc/`, `src/sourcecontrol/`, see below), a `rustybin` CLI for remote
instances (step 10, done 2026-10-30: `src/cli/`, see below), management MCP parity (step 11, done 2026-10-30:
every control route has a tool, see below), and the 0.2.0 release (step 12, done 2026-10-30: the book
reordered around the jobs with an "On every surface" table on every Using Rustybin page, every 0.2 route with a
tested example, the SDK and console conformance green, the changelog, the tag and the signed image). The next plan
goes in a new folder under `docs/plans/` with the same shape (a master plan whose section 4 is the build marker,
one file per step, a step built whole and pushed as it lands, only when the user says go). **The five-surface
rule**: a capability lands on the API (catalogue and OpenAPI), the console, the management MCP, the CLI and
`rustybin.yaml` in the same step, and three parity tests keep it so (`the_command_tree_is_consistent_with_the_catalogue`,
`every_control_route_has_a_management_tool`, the harness schema parity); a user-facing page of the book gets its
"On every surface" table and a Hurl example per route. Rules the foundations set: a feature that reaches out resolves through `egress::Egress` under its
`Purpose` (never its own `reqwest::Client`); a feature's configurable value is a `settings::Spec` (seeded by its
variable, stored override, `secret()` for credentials, never returned as a value); the console's instance
settings are forms from the settings schema (`ui/lib/settingsform.js`), one card per group on a Management tab.

## Rustybin 0.2.2, the hardening release (built and released 2026-11-02)

[docs/plans/hardening/master-plan.md](docs/plans/hardening/master-plan.md) is the plan from a full code review of
`1328b07`: ten steps, one file each, all built on 2026-11-02 (each step file has build notes; section 4 is the
build marker; `v0.2.2` tagged, the signed image's digest is in the changelog). Rules it left: a request path that
parses or compiles a spec goes through `mocks::service::Svc::compiling` (blocking pool, 4 permits); the console's
`send` adds the control token only with `control: true`; a saved MCP connection is used through
`inspector::store::use_connection`; agent revocations and overrides follow `credential.fingerprint()`; a document
from git is applied with `ApplyOptions.untrusted`; every egress client resolves through the pinned resolver; the
YAML output is pinned by `tests/snapshots/export.yaml`. Section 8 of the master plan queues the next milestone
(instance reliability and testing workflows).

## The hosted demo (planned 2026-11-06, live 2026-11-07)

[docs/plans/hosted-demo/master-plan.md](docs/plans/hosted-demo/master-plan.md): making a public self-service instance
safe on the current release and putting it online; section 4 is the build marker; `v0.5.1` released (the signed image's
digest is in the changelog); the demo is live on the owner's VPS from `deploy/demo` (its own compose project and Cloudflare Tunnel, not through `rustybin-platform`), locked to the owner's sign-in on their word, so no visitors for now. Rules it left: every reader of a
mock's captures goes through `Svc::traffic_scope` and `TrafficScope::sees` or `CaptureLog::visible` (a capture holds
what its sender presented; `MockCapture.sender` is never in an answer); `capture::Sender` keeps sessions and keys apart
(a session name may hold a `:`); what a compiled revision keeps is bounded for the whole revision, never per schema (a
`$ref` named by every operation multiplies: `mcp::VISITOR_TOOL_NODES` through `schema::inline_shared`,
`checks::KEPT_SAMPLE_BYTES`), and a new thing kept on a `Compiled` belongs in `Compiled::memory_estimate`, which the
visitors' budget (`RUSTYBIN_MOCKS_SELF_SERVICE_MAX_MB`, `MockStore::check_memory`) counts; a public instance's proxy sets
one header and names it (`RUSTYBIN_TRUST_FORWARD_HEADER`), because `any` lets a client pick its own address; on a
self-service instance with the password sign-in (`login::optional`) a caller without a credential is a visitor, and a new
route that declares `.authenticated()` stays closed to it unless it is also `.self_service()`; a caller without a credential
reaches what it reaches on `open`, which `a_caller_without_a_credential_reaches_what_an_open_plane_gives` walks.

## Rustybin 0.6, more from an instance (built and released 2026-11-07)

[docs/plans/core-next/master-plan.md](docs/plans/core-next/master-plan.md): steps 01 to 10 of the roadmap's candidates,
released as `v0.6.0` (the signed image's digest is in the changelog); steps 11 to 13 waited for the owner (section 4) and were built on 2026-11-09 with the choices of section 5, see the next section. Rules it left: a dry check of the outbound policy is
`Egress::dry_check` (never a copy); a feature that reaches out on a caller's word needs `mocks.write` or better and a
`Purpose` with no default (`mocks/fetch.rs`), and `rustybin.yaml` never reaches out; a route a git host calls is open and
its signature is the credential (`sourcecontrol::is_public_path`), off until its secret is set; a harness feature that
can be an import stays one (`harness/import.rs` makes `source.requests`, so the engine and the safety checks are
untouched), and what reads finished runs does so after the fact (`harness/otlp.rs`); a timing change is measured under
the parallel suite, not alone; a Hurl body in an included example is bare JSON, never a fenced block, which closes the
page's own fence.

## Core next steps 11 to 13 (built and merged 2026-11-09, not released)

The owner said "build it all out" on 2026-11-09; section 5 of
[docs/plans/core-next/master-plan.md](docs/plans/core-next/master-plan.md) records the choice each step took
(the one that keeps the rule it touches), each step file has build notes, and the changelog's "Unreleased"
section lists what is new. A release waits for the owner's word.

Steps 14 to 26 of the same plan (planned 2026-11-09, section 6: the order, a table of decisions for the owner
with a recommendation each, one step file each) are everything left: the 0.7.0 release of steps 11 to 13, the
follow-ups the build notes named, the roadmap's remaining candidates, a 0.8.0 release and the demo. None is
built; the marker is the first row of section 3 that is not done, and steps 14, 25 and 26 are the owner's to
start.

Step 11 (source control; `docs/plans/core-next/step-11-source-control.md`):
`src/sourcecontrol/branches.rs` (a diverged branch: `divergence`, the previews, `pull_request`; the
remote's branches, switching and starting one) and `signing.rs` (signed commits). Rules it left:
the instance never merges and never force-pushes, and no code path of the automatic mode or the
webhook reaches `resolve` (a divergence is a person's call: `instance`, `repository` or
`pull_request`, each with a `dry_run` that validates like the apply); the branch is the
`git.branch` setting, changed only by `switch_branch` or the settings API, never by a document;
a document from the repository never changes a `git.*` setting or secret
(`REPOSITORY_REFUSED_PREFIXES`, the group check in `apply_secrets`); a comparison of the instance
with a repository's document goes through `sourcecontrol::instance_document` (a bundle's `spec:`
path compares a file with itself); the signing key is the secret `git.signing_key`, never the
deploy key, and a commit with `git.sign` on is signed or refused, never unsigned; what a tool
answers about the repository's side (`untrusted_divergence`, `untrusted_branches`) is third-party
content; a file a document names (`spec:`, a scenario's `file:`) goes through
`configdoc::contained_path` and is copied out of the commit by `extract`.

Step 12 (agents; `docs/plans/core-next/step-12-agent-secrets-and-sign-in.md`): a secret set at
an agent's request (`src/secret_requests.rs`, `AppState.secret_requests`, the tools in
`src/mcp/manage/secrets.rs`, `ui/lib/secretrequests.js`, `rustybin settings requests | fulfil |
decline`; user docs `docs/src/agent-mcp.md#a-secret-at-an-agents-request`) and the agent
sign-in from the CLI (`rustybin login --agent`, `src/cli/agent_login.rs`, `Profile.agent`, used
and renewed by `rustybin mcp-stdio`). Rules it left: a secret is still never a tool input or
output (`request_secret` and `get_secret_request` carry a setting's key, a reason and a state;
`every_tool_is_fully_defined` is unchanged), and fulfilling has no tool; a value an agent asked
for is written by a person's own call through `Settings::update`, with what setting that secret
needs (`settings::check_keys`), never by a path of its own; what an agent wrote for a person to
read (its reason, its name) is answered as `untrusted_*`, cleaned and capped
(`secret_requests::clean_reason`), rendered as text, and never in an audit entry; requests are
in memory, bounded and expiring, and no document carries one; an agent sign-in's tokens are
for `/_rustybin/mcp` only, so no command but `mcp-stdio` sends `Profile.agent`, and
`Ctx::client` refuses an agent token before any call (`agent_login::ONLY_MCP`); the CLI
registers one client (`agent_login::CLIENT_NAME`, `REDIRECT_URI`), so a new sign-in is the same
agent.

Step 13 (harness; `docs/plans/core-next/step-13-harness-follow-ups.md`): scenario
import for Postman collections and Artillery scripts (`import.rs`, the `import_scenario` tool),
HTTP/3 targets behind the cargo feature `harness-http3` (`http3.rs`) and server-to-server chains
(`chain.rs`, the rules in `safety.rs`). Rules it left: an import never runs or guesses (what has
no counterpart is named in `left_out`, a variable the document does not define leaves its request
out) and always validates (`import::fits`); a protocol's client is handed the pinned addresses
and resolves nothing itself (the QUIC client: no `Alt-Svc`, no preferred address); what the
default build must not gain goes behind a cargo feature whose parameter stays in the schema and is
refused at validation by name (`scenario::http3_built`); the server role sends only through
`chain::call_on`, which is given a marker the correlation layer verified and never the request,
and only through `safety::ChainGate` (live, the kill switch, once per request and hop, a cap) to
hops `safety::pin_chain` pinned; a field added to the marker goes inside its signature and leaves
a client's marker byte for byte what it was; an onward call is never counted with the clients'
arrivals.

## Demo next, new features for gateway demos (planned 2026-11-11, steps 01 to 05 built, not released)

[docs/plans/demo-next/master-plan.md](docs/plans/demo-next/master-plan.md): nine new features and a
release, one file each (section 4 is the build marker, section 6 the owner's decisions with a
recommendation each): AI gateway scenarios, MCP security scenarios, a mock recorded from a real
upstream, contract drift (steps 01 to 04, built 2026-11-11), demo storyboards (step 05, built
2026-11-12), customer demo packs, Arazzo workflows, realtime and batch AI APIs, AsyncAPI event
mocks. Each built step file has build notes; the changelog's "Unreleased" section lists what is new.
Steps 06 to 09 wait for the owner's decisions (section 6), step 10 (the release) is the owner's to
start, and core-next step 14 (the 0.7.0 release) has not been done either.

Step 05 (storyboards; `docs/plans/demo-next/step-05-demo-storyboards.md`, user docs
`docs/src/storyboards.md`): `src/storyboards/` (`mod.rs`: `Storyboard`, `Step`, `Action`, the store
in `AppState.storyboards` and `<state>/storyboards.json`, the service the routes, tools and documents
share, `/_rustybin/storyboards*`; `run.rs`: `run_step`, `reset`, each session's progress in
`Storyboards.runs`), `src/mcp/manage/storyboards.rs`, `rustybin storyboard`, `ui/views/storyboards.js`,
`storyboards:` of `rustybin.yaml`. Rules it left: an action calls the function its own route and
tool call (`run::act`), never a control route in process, and checks the runner's permission for it
at run time (a missing one is the `403 permission_required` of that permission, any other failure
is a result with `ok: false`); the service handed to an action has no `route_need`; a new kind of
action is a variant of `Action` with its arms in `validate`, `act`, `confirm_reason` and
`action_schema` (the console's forms follow the schema); a storyboard never holds a credential (a
`send` action is validated as a saved test) and progress is never instance state (in memory, bounded,
in no export or checkpoint); a storyboard from git is never moved and never gains a gateway
(`Storyboard::outbound`).

## Rustybin 0.3, reliability and testing workflows (built and released 2026-11-02)

[docs/plans/reliability/master-plan.md](docs/plans/reliability/master-plan.md) (planned 2026-11-02 from section 8 of
the hardening plan; ten steps, one file each with build notes; section 4 is the build marker; one release, 0.3.0, at
the end; `v0.3.0` tagged, the signed image's digest is in the changelog). Built: safe pruning, validated previews, git reads whole,
diagnostics (`src/diagnostics.rs`), gateway scenarios (`src/scenarios/`), saved tests (`src/replay/`), checkpoints
(`src/checkpoints.rs`), edit conflicts, AI job pickers, the docs. Rules it left: a document's section never prunes what it names but could not
load; a preview validates exactly like the apply; a mock edit names the draft revision it was made from (`If-Match`
or `expected_revision`, `StoreError::Stale` checked under the store's write lock, `MockStore::edit_lock` around the
read, recompile and write); a new management tool goes at the end of `TOOLS` (`tools/list` pages at 100 and every
client, test and example follows `nextCursor`); test traffic and replays go out under `Purpose::Scenario` and
`Purpose::Replay`.

## Rustybin 0.3.1, the 0.3 review (released 2026-11-03)

[docs/plans/review-0.3/master-plan.md](docs/plans/review-0.3/master-plan.md): a full review of 0.3.0, 89 findings, all
fixed in eight steps with build notes; `v0.3.1` tagged, the signed image's digest is in the changelog, and the release
carries Linux and macOS binaries (`.github/workflows/release-binaries.yml`). 
## Rustybin 0.5, access control (released 2026-11-03)

Steps 07 to 15 of [docs/plans/testing-2/master-plan.md](docs/plans/testing-2/master-plan.md) (users, roles and
permissions, every route and tool checking a permission, invitations, single sign-on, projects, agents acting for a
user, open and closed portal APIs, the audit log and a security pass of 33 findings, each with build notes), released
as `v0.5.0` (the signed image's digest is in the changelog); the notes per step follow below.

## Rustybin 0.4, testing workflows (built and released 2026-11-03)

[docs/plans/testing-2/master-plan.md](docs/plans/testing-2/master-plan.md), steps 01 to 06 (each with build notes;
section 4 is the build marker; `v0.4.0` released after step 06 with the signed image's digest in the changelog, access control steps 07 to 15 follow as 0.5.0). Built: the
run history (`src/replay/history.rs`), schedules with a failure webhook (`src/replay/schedule.rs`, `Purpose::Notify`),
four scenarios (`circuit_breaker`, `request_size_limit`, `cors`, `header_transformation`; test parameters are a typed
table in `src/scenarios/catalog.rs`, and `scenarios::own_cors` keeps the instance's CORS layer off a `cors` run's
mock), checkpoints in the repository (`src/sourcecontrol/checkpoints.rs`, written only by a commit that names them),
the compare view for a stale edit (`GET /_rustybin/mocks/{name}/revisions/{revision}`, `ui/lib/conflict.js`; the store
keeps replaced drafts). Rules it left: a scenario parameter is a row of the parameter table (the console field, the
MCP schema property and the CLI flag follow from it); a saved-test name never shadows a route (`runs`, `schedules`).
Access control (steps 07 to 15, 0.5.0): step 07 (the model, `src/rbac/`) and step 08 (every route and tool checks a
permission, see `src/control_auth.rs` and `src/rbac/` below) are built. Rules step 08 left: a control route declares
its permission in the catalogue or `every_control_route_declares_a_permission` fails; a tool declares one or
`every_tool_declares_a_permission` fails; a permission no route, tool or listed body check uses fails
`every_permission_is_used`; who may call what is compared with 0.4 in `src/rbac/access_table.rs` (a change of reach
is a deliberate row of `DELIBERATE`, a route new since 0.4 goes in `NEW_SINCE_0_4`); a `403` names the missing
permission (`reason: permission_required`), never what the caller holds. Step 09 (users and roles on every surface,
invitations; user docs `docs/src/users.md`) is built: `src/rbac/api.rs` and `service.rs`, `/_rustybin/join` and
`/me/password` in `src/login.rs`, `src/mcp/manage/users.rs`, `ui/views/manage/users.js`, `ui/views/account.js`, the
join page in `ui/app.js`, `users:` and `roles:` in `rustybin.yaml`. Rules it left: a password or a hash is never a
tool input or output (`every_tool_is_fully_defined` refuses keys containing `password` or `hash`), never in a
document, a log line or a list; an invitation token is answered once, by the call that made it; nobody grants or
manages more than they hold (`rbac::service`); a document from git changes display names and emails only
(`configdoc::REPOSITORY_REFUSED_SECTIONS`); ending a user goes through `rbac::service::ended_user` (their sessions
and their agents' MCP sessions), removing one (disable, delete) through `removed_user` (also revokes their agents).
Step 12 (agents act for a user; `docs/src/agent-mcp.md#agents-act-for-a-user`) is built: see the AI agents
paragraph below. Rules it left: an agent acting for a user never holds more than the user (its caller is the user,
resolved at every call, intersected with an OAuth token's scope; every limit only takes away); OAuth tokens carry
`rb_uid` and `rb_ugen` (a token without `rb_uid` is the first admin's in password mode); the first admin's password
change moves the first admin's generation only; a user's own agents need no `agents.read` / `agents.manage`
(`agents::visible_to`, `may_manage`; limiting only narrows). Step 10 (single sign-on with OpenID Connect; user docs `docs/src/sso.md`) is built:
`src/sso/` (see below), `Users::sso_sign_in` / `add_sso` and `User.sso` / `pinned_roles` in `src/rbac/users.rs`,
`Purpose::Sso`, `login::sso_session`. Rules it left: a single sign-on user (`Source::Sso`) never gets a password, an
invitation or a reset (the store refuses: `SSO_USER`) and its `roles` come from the provider's groups at every
sign-in (admins pin extra ones with `pinned_roles`); an identity never takes over a password account; a document from
git never changes an `sso.*` setting (`configdoc::REPOSITORY_REFUSED_PREFIXES`); the password sign-in can be off only
while single sign-on is set up (`sso::password_sign_in`). Step 11 (projects, roles on part of an instance; user docs
`docs/src/projects.md`) is built: `src/rbac/projects.rs` (the model, a child of `users.rs`, projects kept in
`users.json`), `src/rbac/project_api.rs` (service and `/_rustybin/projects*`), `Resolved.projects` with `has_in`,
`has_somewhere`, `capped`, the catalogue's `.project_scoped()` and `control_auth::RouteNeed`, `ToolNeed::Project`,
`src/mcp/manage/projects.rs`, `src/configdoc/projects.rs`, `ui/lib/projects.js` (the top bar's picker). Rules it left:
a resource without a project is the instance's (a caller with instance roles only meets exactly the 0.4 to 0.5 checks);
a route whose resources may be a project's is `.project_scoped()` and listed in `rbac::need::PROJECT_ROUTES` (the route
check lets in `has_somewhere`, the handler checks `has_in` on each resource: `Svc::readable` / `manageable` with the
caller's `route_need`, `replay::visible` / `admit_for` / `select_for`, `checkpoints::visible`), and
`every_project_route_checks_each_resource` calls each one as another project's member; a tool of those is
`ToolNeed::Project` and in `access::PROJECT_TOOLS`; another project's unreadable resource is `404`, lists leave it out;
a move needs the write permission in both (`projects::check_move`); a project's roles never count as an instance admin;
a document from git never changes members, prunes a project or moves a resource; a new kind of project resource is
counted in `project_api::resources` (deletion and `move_to`). Step 14's audit log (user docs `docs/src/audit-log.md`;
the security pass of step 14, 33 findings fixed, has its build notes in the step file) is built: `src/audit.rs` (see below), `list_audit` in
`src/mcp/manage/audit.rs`, `rustybin audit list | tail`, `ui/views/manage/audit.js`, the `audit.*` settings. Rules it
left: a new mutating control route or management tool is an entry without code (`audit::layer`, the route layer
outside `check_route`, and `tools::call`; `every_mutating_route_and_tool_writes_one_entry` walks the catalogue and
`TOOLS`), and a mutating route that must not be one is a row of `audit::NOT_RECORDED` with its reason; a handler or a
service names what only it knows with `audit::target`, `audit::event`, `audit::actor_user` (task-local, a no-op
outside a request; blocking closures go through `audit::carried` and `merge`, as `Svc::compiling` and
`configdoc::apply_on_pool` do); an entry never holds a value, a body, a header, a password or a token (a sign-in
names the user only when it exists; `nothing_secret_reaches_an_entry_or_a_file` plants secrets and greps the ring
and the files); `rbac::refuse` marks its answer (`need::Refusal`, the caller as `ControlCaller`) so a refusal is
recorded on the data plane too; a document from git never changes `audit.*`.


## The whole instance as code, and source control (built)

Step 09 of the 0.2 plan (user docs `docs/src/configuration-as-code.md`, `docs/src/source-control.md`). `rustybin.yaml`
version 2 is `MocksFile` (`src/mocks/config_file.rs`) grown by `instance`, `settings`, `secrets` (references only),
`harness` and `health`; `mocks` is an `Option` (an absent section is left alone, a present one is the truth when
pruning). `src/configdoc/` owns `export` (secrets as `${VAR}` or `secret:stored`, never values; the `git` settings
group is never exported nor pruned), `diff` (a section-aware list of changes, answered by every apply), `apply`
(each section through its own service: the settings store, `MockStore::apply` with `Origin::Api`, the registry's
file copy on the `MockStore` plus `AppState.registry` for published entries, `Portal::publish`, `Harness.saved`,
the agents policy, the health toggle; the registry and agents sections persist in `<state>/config.json`),
`apply_extra` (what a `RUSTYBIN_CONFIG_FILE` load applies beyond the mocks sections, from `mocks::reload_files`),
the bundle (`write_dir`, `tar_gz`), and the routes `/_rustybin/config/{export,apply,schema}`. A new feature with
instance state gets a section (or a setting) here, so a document stays the whole instance. `src/sourcecontrol/`
(`AppState.git`): the `git` binary in `<state>/repo`, the `git.*` settings seeded by `RUSTYBIN_CONFIG_GIT*`, a
token through `GIT_ASKPASS` or a deploy key through `GIT_SSH_COMMAND`, the remote's host under `Purpose::Git`,
`/_rustybin/git*`, startup from the repository (`SourceControl::startup` in `server.rs`) and the automatic mode
task; never a merge. Console: Management, Export (`ui/views/manage/export.js`) and Source control
(`ui/views/manage/git.js`).

## The command line for remote instances (built)

Step 10 of the 0.2 plan (user docs `docs/src/cli-guide.md`, the generated `docs/src/cli.md`). `src/cli/`:
`commands.rs` is the command tree, one static table `COMMANDS` (noun, verb, usage, summary, flags, the routes it
calls, destructive, example) and one `dispatch` match; `mod.rs` the parser, help, completion, the rendering for
people and `execute` (tests pass a canned stdin and a buffer); `client.rs` the control-API client (bearer,
session and manage-token headers, SSE for the tails); `profiles.rs` `profiles.json` (0600); `agent_login.rs` the agent sign-in
(`rustybin login --agent`, for `mcp-stdio` only). `main.rs` routes every
noun here; `mock`, `mcp-stdio` and the offline harness verbs keep their modules. A new control route needs a
command (or an `UNCOVERED` entry with a reason) or `the_command_tree_is_consistent_with_the_catalogue` fails;
every command's example runs against a live instance in `every_command_runs_against_an_instance`; the reference
page is regenerated with `RUSTYBIN_UPDATE_CLI_DOCS=1 cargo test cli_docs`. A command flag never reuses a global
flag's name (`--url`, `--token`, `--profile`, `--session`, `--mock-token`, `--json`, `--yes`, `--quiet`).

## Load test mode (harness): built

[docs/plans/harness.md](docs/plans/harness.md) is built (planned 2026-10-22; phase 0, the engine spike in
`examples/harness_spike.rs`, phase 1, the single-node engine in `src/harness/`, and phase 2, correlation, done
2026-10-25; phase 3, the cluster, phase 3b, failover, and phase 4, patterns and live tuning, done 2026-10-26; phases
5, the rate-limit tests, 6, mock-driven traffic and identities, 7, faults and protocols, and 8, the docs
(`docs/src/harness.md`, the compose cluster `docs/examples/harness/cluster/`) and the conformance run
(`conformance/harness/run.sh`), done 2026-10-28; the plan's "Phase 1/2/3/3b/4/5/6/7/8 build notes" list where the
build differs; follow-ups are under consideration in `docs/src/roadmap.md`):
Rustybin as a load client and correlating upstream, single node (loop-through) or a cluster (an elected leader,
clients, servers), traffic layers with run patterns and rate-limit tests, every parameter from one schema in the
API, MCP and console (tunable live), or headless. It is the first feature that sends load to targets the caller
chooses (A2A push notifications,
the leads webhook and the key registry URL already send outbound requests, with their own allowlists), so
`src/harness/safety.rs` (allowlist, pinning, forbidden addresses, verification, kill switch) comes first and every
new way to point load goes through it. One model, one schema: a scenario parameter lives in `scenario.rs` (model,
validation, `schema()`), and the parity tests fail when the schema, the MCP tool schemas or the console builder
(`ui/lib/scenarioform.js`, renders every non-shorthand schema property) miss it. A new parameter also belongs in
the tables of `docs/src/configuration.md` and, when it changes how the harness is used, in `docs/src/harness.md`
with a tested example in `docs/examples/harness/`.

## MCP server for every OpenAPI mock (built)

[docs/plans/mock-mcp.md](docs/plans/mock-mcp.md) is built (phases 1 to 4, 2026-10-24; its section 13 lists where the
build differs; user docs in `docs/src/mocks/mcp.md`). `/mock/<name>/_rustybin/mcp` serves one tool per operation:
`src/mocks/mcp.rs` builds the tools from a `Compiled` revision (cached on it), `src/mcp/openapi.rs` is the
`Family::OpenApi` profile whose `tools/call` runs in process through `mocks::handlers::serve` (so REST and MCP never
drift apart; captures carry `channel: "mcp"` and the tool). A new mock feature that changes responses needs nothing
here; one that changes how requests are built (new parameter styles, body media types) belongs in both.

## Management MCP server (built)

[docs/plans/management-mcp.md](docs/plans/management-mcp.md) is built (phases 0 to 5, 2026-10-20; its section 13
lists where the build differs; user docs in `docs/src/agent-mcp.md`). Step 11 of the 0.2 plan (2026-10-30) made
it complete: every route under `/_rustybin/` and `/bin` maps to a tool, a resource or a written reason in the
`COVERAGE` table of `src/mcp/manage/tests.rs`, and `every_control_route_has_a_management_tool` fails on a new
route without one. A new control route therefore needs a tool (one module per feature: `settings.rs`,
`assist.rs`, `inspect.rs`, `registry.rs`, `portal.rs`, `config.rs`, `agents/tools.rs`, plus `instance.rs`,
`mocks.rs`, `traffic.rs`, `testing.rs`, `visits.rs`, `harness.rs`) that calls the same function as the REST
handler (never a control route in process: `testing::in_process` refuses control paths; split the handler's
logic out instead, as `mocks::ai::*_work`, `portal::api::publish_item` and `inspector::build_run_spec` were),
a `ToolDef` with its `permission` (`ToolNeed::Perm`, `Owner` for the per-mock rule, `Visitor`), and a dispatch arm. Destructive and outward-facing tools carry `confirm`
(`every_tool_is_fully_defined` checks it) and ask only when the call is of that kind; secrets are never tool
inputs (keys ending in `_key` or `_token` are refused by the same test). Third-party content (captured
traffic, answers of other MCP servers, browsed registries) goes under `untrusted_*`. Prompts are the jobs
(`prompts.rs`: `build_instance`, `mock_an_api`, `publish_portal`, `backup_to_git`, `test_mcp_server`,
`onboard_agent`, `debug_gateway_call`, `load_test_gateway`, `secure_a_mock`); resources are the config schema,
the catalogue, the harness schema, the portal catalogue and the CLI reference. `rustybin mcp-stdio` takes
`--url` and `--profile` (the CLI's profiles). `/_rustybin/mcp` lets an agent configure the
instance (`claude mcp add --transport http rustybin https://<host>/_rustybin/mcp --header "Authorization: Bearer
<token>"`). It is a second profile family on the mock MCP server's protocol and transport code (`src/mcp/manage/`),
separate from the mock server at `/mcp`. Its tools call `src/mocks/service.rs`, the layer the REST handlers use, so
a new mock feature belongs in the service layer and then in both. Keep the rules when adding tools: a permission per
tool (`every_tool_declares_a_permission`, the Permission column of `docs/src/agent-mcp.md`) and the filtered `tools/list`, annotations plus confirmation (elicitation, else `confirm: true`) on
destructive tools, captured or mocked content only under `untrusted_*` fields (truncated, never in the text
summary), audit lines without arguments. In-process requests from tools (`try_request` and friends) carry only
the caller's API key, never their control credentials (captures would expose them).

AI agents (step 08 of the 0.2 plan, `src/mcp/manage/agents/`): every `tools/list` and `tools/call` is attributed to
an agent (the caller's `Credential` from `Access` plus the client's `clientInfo` name) in `AppState.agents`, whose
rights (a ceiling, tool allow and deny lists: the agent's own, a rule of `rustybin.yaml`'s `agents:` section, the
`agents.default_ceiling_for` per role of the user, the `agents.default_ceiling` setting) apply after the credential's;
revocation refuses calls, ends sessions and puts the
agent's token ids and OAuth subject in `login::Tokens`. `/_rustybin/agents*` (any signed-in caller: everyone's with
`agents.read` / `agents.manage`, a user's own without), console view `ui/views/agents.js` (Owner column and filter).
Since testing-2 step 12 a sign-in token's agent and a user-approved OAuth agent are `Credential::User { id, client }`
(acting for the user; `Policy::owner_of` resolves them), the admin token and control JWTs act for nobody, a
ceiling may name a role (`Ceiling::Role`; the four 0.3 ceilings keep their 0.4 tools), rules match `user` and
`role`, and `Agents::adopt_users` moves 0.4 records to their user once. A new tool needs nothing here: ceilings
follow its permission (`MANAGE_CEILING`) and `read_only` (`ceilings_keep_their_tool_counts` freezes the 0.4 sets).

OAuth sign-in to `/_rustybin/mcp` ([docs/plans/control-oauth.md](docs/plans/control-oauth.md), 2026-10-21):
`src/control_oauth.rs` is a small authorization server for the control plane (metadata, stateless client
registration, PKCE, a consent API the console's `#/authorize/<id>` view calls with its own credential, HS256
tokens bound to the `/_rustybin/mcp` URL, carrying the approving user, `rb_uid` and `rb_ugen`, since testing-2 step 12). Its tokens must stay accepted on `/_rustybin/mcp` only
(`control_auth::enforce` checks them there; `admin::admin_check` must never accept them).

## Hosted self-service mocks (built)

[docs/plans/hosted-mocks.md](docs/plans/hosted-mocks.md) is built (phases 0 to 7, 2026-10-18; its section 16 lists
where the build differs). Visitors of a public instance create their own mocks with `RUSTYBIN_MOCKS_SELF_SERVICE`
(off by default): a manage token per visit, 1 hour extendable to 3, then an email lead for up to 6 hours per UTC
day, visitor mocks only on `RUSTYBIN_MOCKS_SELF_SERVICE_HOST` with restricted responses. What is left for the hosted
demo was thought to be deployment, not code; see the hosted demo plan above. Candidates for later are in `docs/src/roadmap.md`. The OpenAPI mocks and
hosted mocks plans in `docs/plans/` are complete; keep `docs/src/roadmap.md` current when new work is planned.

## Commands

```bash
cargo build
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test                                   # unit + tests/integration.rs, a few seconds
cargo test --features quota-postgres         # + the PostgreSQL quota store (set RUSTYBIN_TEST_QUOTA_STORE to run against a database)
cargo test --features harness-http3          # + HTTP/3 targets (an in-process QUIC server on loopback)
RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints   # regenerate README endpoint tables
cargo run -- --print-endpoints-markdown      # print them instead
cargo run -- mock validate src/mocks/petstore.yaml   # OpenAPI mocks CLI (offline: validate, init, sample, check-config)
cargo run --release --example harness_spike -- --sweep   # harness phase 0 spike: load sweep against an in-process Rustybin
cargo run -- harness run src/harness/scenarios/smoke.yaml --local-server   # a harness run looped through an in-process Rustybin
RUSTYBIN_HTTP_PORT=8080 cargo run
docs/examples/run.sh                         # documentation examples (Hurl) against fresh servers (open, plan, secured, shared, keyed, self-service, harness)
conformance/harness/run.sh                   # the compose harness cluster (docs/examples/harness/cluster): the leader killed mid-run, the report asserted
mdbook build docs                            # documentation site
python3 docs/tools/check_includes.py         # every docs include and example anchor resolves
```

MSRV is Rust 1.88 (`rust-version` in Cargo.toml, Dockerfile builder image, CI `msrv` job).

## Hard rules

- Never write the em dash (U+2014) or en dash (U+2013) anywhere: code, comments, docs, commit messages.
- Vendor neutral: no gateway vendor branding in code or UI.
- No `unwrap()` / `expect()` / panics on request paths (startup and tests are fine).
- Every piece of in-memory state reachable from requests is bounded (capacity cap plus TTL or eviction).
- HTML-escape user input reflected into HTML (`landing::html_escape`), XML-escape into XML, validate header values.
- Keep tests fast: use `crate::test_support`, never generate RSA keys per test, never write into `certs/` or fixed `/tmp` paths (use `tempfile`).

## Layout

- `src/main.rs`: thin CLI (`--print-endpoints-markdown`, `--version`), exits non-zero on fatal errors.
- `src/lib.rs`: module list, `ROUTERS` (every module's router), `build_app(state)` with the middleware stack
  (outermost first): mock host rewrite (wraps the finished router, `mocks::host`), request id, trace, metrics,
  request id propagation, CORS, control-plane auth, API keys (`keys::enforce`), plan limits, time-to-headers timeout,
  harness correlation (load test mode only), inspector capture, body limit, fault injection; on the routes
  themselves the audit log (`audit::layer`) outside the one permission check (`control_auth::check_route`). JSON
  404 fallback.
- `src/server.rs`: `run(config)`, `start(config)`, `start_with_state(state)` -> `RunningServer`
  (bound addresses, `shutdown()`, `wait()`). HTTP bind failure is fatal; HTTPS and gRPC failures only warn.
  Ports may be 0. Graceful shutdown on SIGTERM/SIGINT with a 10 s grace period. With `RUSTYBIN_GRPC_ON_HTTP`
  the HTTP/HTTPS app is wrapped in `grpc::GrpcOnHttp` (content-type `application/grpc*` goes to tonic).
- `src/config.rs`: `Config` from `RUSTYBIN_*` env vars (invalid values warn and default), `Config::for_tests()`.
- `src/state.rs`: `AppState { config, jwt, certs, identity, inspector, health, limits, metrics, keys, mocks }`, `FromRef` for
  each part (handlers can keep extracting `State<Arc<Config>>`).
- `src/catalog.rs`: route catalogue, the single source of truth (landing page, `/export/*`, README tables)
  plus the consistency tests.
- `src/openapi.rs`: hand-written paths plus `MODULE_PATHS` / `MODULE_COMPONENTS` merge lists; methods are
  aligned with the catalogue (routes registered with `any()` are documented as get/post/put/patch/delete).
- `src/inspector.rs`: bounded ring buffer + broadcast feed of captured requests, `/_rustybin/requests*`.
- `src/settings.rs`: the instance settings store (`RUSTYBIN_STATE_DIR/settings.json`, `RUSTYBIN_SETTINGS_LOCK`,
  secrets sealed with ChaCha20-Poly1305 under `RUSTYBIN_SECRETS_KEY` or `<state>/secrets.key`): `Spec` tables per
  feature joined by `settings::specs()`, env seed / stored / default resolution with `Source`, `secret()` for the
  feature that uses one, `{set, hint}` in the API, audit lines without values; `/_rustybin/settings` (GET, PATCH,
  DELETE `/{key}`, admin). `AppState.settings`.
- `src/secret_requests.rs`: secrets an agent asked for (`AppState.secret_requests`, `/_rustybin/secret-requests*`,
  the setting `agents.secret_requests`, off by default): bounded, expiring, in memory; a person fulfils through
  `Settings::update` or declines; see core next step 12 above.
- `src/egress.rs`: the outbound policy (`RUSTYBIN_EGRESS_ALLOW`, `RUSTYBIN_EGRESS_ALLOW_LOOPBACK`, the settings
  `egress.*`): the `Allowlist` grammar (shared with the harness, which re-exports it from `safety.rs`), forbidden
  addresses, `parse_url`, `pin_with`, a `Rule` per `Purpose` (A2A push, leads, key registry, assist, and the
  later registry, MCP relay, git), `resolve` then `client` (pinned addresses, no redirects), a bounded decision ring,
  `/_rustybin/egress` (admin). `AppState.egress`; A2A push, leads and the key registry resolve through it.
- `src/control.rs`: `/_rustybin/config`, `/_rustybin/version`, `/_rustybin/ready` (readiness, never authenticated),
  `git_sha()` / `build_info()` (build.rs sets `RUSTYBIN_GIT_SHA`), `is_control_path()` (`/_rustybin/*`, `/ui/*`).
- `src/audit.rs`: the audit log (`AppState.audit`, `GET /_rustybin/audit` with `actor`, `action`, `target`,
  `since`, `until`, `result`, a cursor; `/_rustybin/audit/stream` SSE; `audit.read`). An `Entry` is time, `Actor`
  (kind, user id and name, agent), via, action (route template and method, or tool), event, `Target`s, `Outcome`,
  request id, address. A ring of 10,000 plus `<state>/audit/audit-YYYY-MM-DD.jsonl` written by one thread through a
  bounded channel (full: counted in `not_written`), `audit.retention_days` (30, at most 365) and `audit.max_bytes`
  (256 MiB) on the injectable `limits::Clock`; unauthenticated entries capped at 600 a minute. `layer` is a route
  layer outside `control_auth::check_route` (which leaves the caller on the response as `ControlCaller`); it reads a
  refusal's `reason` and `permission` from small 401/403 JSON bodies of control and portal routes.
- `src/diagnostics.rs`: `GET /_rustybin/diagnostics` (admin; `rustybin doctor`, `diagnose_instance`, Management,
  Diagnostics): checks with a status, what was seen and the fix, never a credential; startup problems go in
  `AppState.diagnostics.note`. A new failure an operator can fix belongs here as a check.
- `src/checkpoints.rs`: checkpoints (`/_rustybin/checkpoints*`, `rustybin checkpoint`, the `*_checkpoint(s)`
  tools, Management, Checkpoints). `covered` is the scope (instance-owned API mocks without an expiry); a
  checkpoint keeps each one's deployed revision, a differing draft, on or off and `MockState::copy`, plus the
  health toggle; `restore` (on the compile pool, with `dry_run`) answers created / replaced / data_restored /
  unchanged / deleted and resets the flaky counters. 20 checkpoints in `<state>/checkpoints/`. Never settings,
  secrets, visitors' or keys' mocks. In git (`src/sourcecontrol/checkpoints.rs`), an explicit `git/commit` with
  `checkpoints` writes `<git.path>/checkpoints/<name>.json` (never the automatic mode, whose commits exclude the
  directory) and `?from=git` lists and restores the repository's through `parse_untrusted` and the compile pool.
- `src/replay/`: saved tests (`/_rustybin/tests*`, `rustybin tests`, `list_tests` / `save_test` / `delete_test` /
  `run_tests`, the console's Saved tests view and Live traffic's Save as test, `tests:` of `rustybin.yaml`).
  `mod.rs` the model (`SavedTest`, `Assertion`: one kind each), validation, the store (`AppState.saved_tests`,
  `<state>/tests.json`, 200 tests, 64 KiB each), `request_from_capture` (secret-looking headers and query values
  become `{{NAME}}` variables, never values); `run.rs` the runner (in process through `state.router` with the
  caller's session, or the test's gateway under `Purpose::Replay`; 200 requests, 60 s; variables from the run or
  `RUSTYBIN_TEST_*` only; JUnit); `hurl.rs` the export; `history.rs` the kept runs (`<state>/test-runs/`, 200 runs and 30 days, never a variable's value; `run_and_keep` with a trigger label from `X-Rustybin-Client`, `mcp` or `schedule:<name>`); `schedule.rs` the schedules (`Tests.schedules`, 20, `every` or `at` UTC, the scheduler and notice tasks in `server.rs`, skipped while the harness sends load, failure notices under `Purpose::Notify` with the `notify.webhook_token` secret, `test_schedules:` of `rustybin.yaml`). Variables are `{{name}}`, not `${VAR}` (a file load
  expands `${VAR}` across the whole document).
- `src/storyboards/`: demo storyboards (`/_rustybin/storyboards*`, `rustybin storyboard`, the `*_storyboard*`
  tools and the `rehearse_demo` prompt, the console's Storyboards view, `storyboards:` of `rustybin.yaml`): at most
  50 of 40 steps, 64 KiB each; a step's action runs as the caller through `run::act`; see demo next step 05 above.
- `src/scenarios/`: guided gateway scenarios (`/_rustybin/scenarios*`, `rustybin scenario`, the
  `*_gateway_scenario` tools, the console's Gateway scenarios view). `catalog.rs` is the one table (what each
  proves, the upstream, the steps, the requests, `evaluate`); `mod.rs` the runs (`AppState.scenarios`, 32, an hour,
  owned by the session key), `start` (a `MockStore::create_scratch` mock, never saved, or a `/flaky` route),
  `check` (egress `Purpose::Scenario`, one `X-Rustybin-Session` per request, the inspector's captures for what
  reached the upstream) and `end` (only what the run created). A new scenario is a table row plus its `mock` and
  `evaluate` arms.
- `src/control_oauth.rs`: OAuth sign-in to the management MCP server (`RUSTYBIN_CONTROL_OAUTH*`), see above.
- `src/rbac/`: users, roles and permissions (access control step 07, the model). `permission.rs` (`Permission`: one
  enum, a stable dotted name, a group and a summary each; `PermSet`, a bit set), `role.rs` (the built-in roles
  `viewer`, `tester`, `editor`, `admin`, `portal_reader`, immutable; custom roles, at most 50, privileged when they
  grant `users.manage` or `settings.security`), `users.rs` (`Users` in `AppState.users` and `Config.users`,
  `<state>/users.json` 0600, at most 500; a change is made on a copy, written off the runtime one writer at a time,
  and published only when the write succeeded; password hashes kept apart from `User`, never returned;
  `resolve` cached a second per user and dropped at every change), `mod.rs` (`Resolved`: who, roles, permissions;
  the old scopes as roles, `rb_roles` / `rb_perms` on control JWTs, unknown names logged once; `from_sign_in`;
  `grant_of`, the 0.4 scope names of a set, for answers only; `migrate`, the console user becomes user one
  at startup), `need.rs` (`Need`: `Open`, `Authenticated`, `Perm`; `ANONYMOUS`, what nobody on an open control
  plane with an admin token and a shared-mode key hold; the `403 permission_required` body, `refuse`, `require`;
  `BODY_CHECKS` and `RESERVED`), `access_table.rs` (step 08's safety net: the 0.4 rules frozen, compared with the
  declarations for every route and scope, `DELIBERATE` lists each difference; the parity tests
  `every_control_route_declares_a_permission` and `every_permission_is_used`). User one's name and password stay the sign-in settings' (`console.user`, `console.password_hash`,
  the environment); other users' hashes live in the store. Sign-in tokens: `sub` the user id, `rb_user`, `rb_gen`,
  `rb_ugen`; an older token (0.3 and 0.4, `sub: "user:<name>"`) is user one. A new permission goes in the table of
  `permission.rs` (and its test), never a yes/no admin check. The scopes keep their 0.4 reach: `console` is the
  viewer role plus `traffic.read`, `assist.use`, `mcp.inspect`; `harness` every `harness.*`. Step 11: `projects.rs`
  (`#[path]` child of `users.rs`, so it reaches the private `Doc`: `Project` with `Member {user, roles}` in
  `users.json`, at most 100, the bindings and SSO-mapped project roles, `ProjectGrant` per project in the resolution,
  `Filter` for `?project=`, `target` and `check_move`), `project_api.rs` (the service and routes, `resources`, the
  delete with `move_to`), `need::PROJECT_ROUTES`, tests in `project_tests.rs`. Step 09: `users.rs` also
  keeps the invitations (`rbi_` plus 32 random bytes, stored as a SHA-256 in `users.json`, 72 h, at most 100 open,
  one per user; `pending` users are invited and disabled until they accept) and the guard rails inside every
  change (`Users::commit` refuses a change that leaves no enabled admin; user one is never deleted; a role in use is
  never deleted; `UsersError::Refused(reason, message)`); `service.rs` the logic the routes, tools and documents
  share (`ServiceError` answers; no own role changes; the caller grants and manages only within its permissions;
  `ended_user`, the hook of step 12); `api.rs` `/_rustybin/users*`, `/roles*` (every mode), `/me`, `/me/sessions*`
  (users exist only with the password sign-in: `404` otherwise). Changing user one's sign-in settings still rotates
  the instance's generation (every session and the OAuth tokens, as in 0.4); another user's password change moves
  that user's generation only.
- `src/login.rs`: the console sign-in (`RUSTYBIN_CONTROL_AUTH=password`, the default): `RUSTYBIN_CONSOLE_USER`,
  `RUSTYBIN_CONSOLE_PASSWORD` or `_PASSWORD_HASH` (argon2id), the settings `console.user` and the secret
  `console.password_hash`, `POST /_rustybin/login` (open) minting HS256 control tokens (the OAuth key's `console`
  subkey, scope `admin`, 12 h, `rb_use: "console"`, `jti`, `rb_gen`), `logout`, `session`, `sessions*`, `password`
  (the caller's own; a caller who is not a user changes user one's with `users.manage`), `me/password` and `join`
  (open, the sign-in's throttle under the key `(invitation)`, the token checked before the password is hashed).
  `Tokens` (generation, revoked ids, listed sessions, failed attempts per address; all bounded) lives in
  `Config.console_tokens` because every credential check takes `&Config`; `control_auth::verify_jwt_claims` calls
  `login::verify` for sign-in tokens, so every caller accepts them. Public mode with the default password refuses
  to start (`login::check_startup`); the default password warns at startup and sets `default_password` in the status.
- `src/sso/`: single sign-on with any OpenID Connect provider (password mode only; access control step 10). `mod.rs`
  (the `sso.*` settings, group `sso`, seeded by `RUSTYBIN_SSO_*` through `Config.sso_env`; `setup()` and
  `password_sign_in()`; `Sso` in `AppState.sso`: sign-ins under way (256, 10 per address, ten minutes), one-time
  codes (256, two minutes), discovery and the JWKS cached an hour (a failure answered again for 10 s), `sync_egress`
  permitting the issuer's host under `Purpose::Sso`; `begin`, `complete` (token endpoint, the ID token, the role map,
  `Users::sso_sign_in`), `redeem` (the code plus the client's PKCE verifier), `test`; the diagnostics check `sso`),
  `provider.rs` (discovery, JWKS, the token endpoint through `egress::Egress`, bounded bodies; `verify_id_token`:
  RS256 or ES256 only, issuer, audience and `azp`, expiry, nonce; an unknown key id refetches the keys once), `api.rs`
  (`/_rustybin/sso/status`, `/start`, `/callback`, `/token` are open, the callback and the token throttled by
  `Tokens::check` under `(sso)`; `/test` is `settings.write`; the console gets `#/sso/<code>` or
  `#/sso-error/<reason>`, the CLI's loopback address `?code=`). The console side is in `ui/lib/auth.js` (`startSso`,
  `finishSso`) and `ui/app.js`; `rustybin login --sso` in `src/cli/commands.rs` (`Env.browser` lets tests drive it).
- `src/assist/`: AI assist (`RUSTYBIN_ASSIST_*`, the `assist.*` settings). `mod.rs` (`Assist` in `AppState.assist`:
  `specs()`, `status()` resolved from the settings at every call, `sync_egress` permitting the provider's host under
  `Purpose::Assist`, the per-caller calls-per-minute cap, bounded usage, the `suggest` cache, `run(RunParams) ->
  OutStream` with the validate-and-repair loop, `test()`), `provider.rs` (`Kind`, `Request`, `Event`, the `Client`
  enum, the shared `post_sse` through the egress policy, `SseParser`), `anthropic.rs` / `openai.rs` / `gemini.rs`
  (request bodies and event mapping; structured answers through tool use, `response_format`, `responseSchema`),
  `mock.rs` (the task's deterministic answer, streamed), `models.rs` (the curated table with tiers and list prices),
  `redact.rs` (prompts never carry a stored secret or the admin token), `task.rs` (`Task` with an optional `post`
  check beyond the schema, input helpers, action blocks, `parse_json_answer`, `validate` on `mocks::schema`),
  `tasks/` (`free_answer`, `explain`, `suggest`, and the mock tasks `spec_from_description`,
  `examples_for_operations`, `edit_spec`, `request_examples`, each with a deterministic `mock` answer and a `post`
  that parses, compiles and checks through `tasks::compile_doc` / `check_spec`; one file per task, `ALL`), `api.rs`
  (the routes; `RUN_PATH` is a read-only POST for control auth). A new AI feature is a new task file (and, when it
  writes, `writes: true`), never a prompt built elsewhere.
- `src/control_auth.rs`: `RUSTYBIN_CONTROL_AUTH` (`password` / `open` / `token` / `jwt`), Ed25519 JWT verification
  (scope, `rb_roles`, `rb_perms` resolved through `crate::rbac`), the `enforce` middleware (authentication only:
  protects `/_rustybin/*` except `/_rustybin/ready` and the OAuth sign-in endpoints
  `/_rustybin/oauth/{register,authorize,token}`; `/ui/*` static files stay open; inserts `ControlAuthorized`, the
  caller's `Resolved`) and `check_route`, the one permission check: a route layer in `build_app` that reads the
  matched template, looks up what the catalogue declares for the method, resolves the caller (`caller_of`: the
  authenticated one, an API key in shared mode, else `open_caller`) and answers `403 permission_required` (or `401`
  for nobody on an open control plane with an admin token) before the handler runs; it inserts `ControlCaller`
  for the handlers' body checks (`rbac::require`). On a self-service instance routes marked `.self_service()`
  let a caller without the permission through to the per-mock rules. Tests sign tokens with the keys in
  `tests/common/control_jwt_keys.rs` (`test_support::control_jwt`). A valid credential lacking a permission gets
  403 (not 401), so the console is not signed out.
- `src/metrics.rs`: `Metrics` (in `AppState`), the `track` middleware (route label = `MatchedPath` template),
  `/_rustybin/metrics`. Every label is bounded; WebSocket handlers hold a guard from `metrics::open_ws`.
- `src/logging.rs`: `RUSTYBIN_LOG_FORMAT` (`text` / `json`), `logging::subscriber(config, writer, ansi)`.
- `src/ui.rs` + `ui/`: the web console under `/ui` (vanilla ES modules and CSS, no Node build step;
  `build.rs` embeds every file under `ui/`, and `ui-portal/` for the dev portal), plus `/_rustybin/catalog` and `/_rustybin/status`. `index.html` gets
  `<meta name="rustybin-*">` tags (control auth mode, console title and back link from the `console.*` settings);
  `ui/lib/auth.js` handles the control token (`#token=` fragment, sessionStorage, sign-in screen) and `http.js`
  sends it as a bearer on `/_rustybin/*` calls. `ui/views/manage.js` is the Management view (the gear opens it;
  `TABS` is the rail later steps extend; `ui/lib/settingsform.js` renders a settings group from the schema).
  Browser check: `conformance/ui/run.sh` (Playwright; open mode plus a jwt-mode instance).
- `src/keys.rs` + `src/keys/`: API keys. `RUSTYBIN_SHARED_MODE` (keys on every data-plane route, per-key limits
  and daily quotas, inspector and usage scoped by key id, the key layer authenticates the control plane) and
  `RUSTYBIN_REQUIRE_KEY`. `registry.rs` (JSON registry, SHA-256 lookup), `ledger.rs` (bounded per-key daily
  counters), `store.rs` (`QuotaStore`; PostgreSQL behind the cargo feature `quota-postgres`, tests in
  `pg_tests.rs` need `RUSTYBIN_TEST_QUOTA_STORE=postgres://...`), `/_rustybin/keys/usage`. The key layer sits
  outside the limiter and attaches a `KeyContext` extension. Keys are redacted in captures (`redact_*`).
- `src/mocks/`: OpenAPI mocks. `spec.rs` (OAS 3.0/3.1 parsing into a normalised model, `serde_json::Value`
  based, lenient: odd specs give warnings), `schema.rs` (deterministic generation and validation in the OpenAPI
  dialect, readOnly/writeOnly by direction, path parameter echo), `model.rs` (settings: the same struct in the
  control API, `rustybin.yaml` and exports), `compile.rs` (immutable revision: spec + settings + route table +
  deploy report), `respond.rs` (Prefer header, rules, overrides, modes), `request.rs` (request validation),
  `security.rs` (mocked security, JWTs from `jwt_state`), `checks.rs` (deploy checks: a valid request and
  response per operation), `store.rs` (`MockStore` in `AppState`: draft, live, history, on/off, capture log with
  per-operation and hourly counts, stateful data; bounded by `MockLimits`), `state.rs` (`stateful` mode: bounded
  in-memory CRUD keyed by request path), `swagger2.rs` (Swagger 2.0 to OpenAPI 3.0 on parse), `persist.rs`
  (`RUSTYBIN_MOCKS_STATE_DIR`: API-created mocks saved as JSON, loaded by `new_store`), `host.rs`
  (`RUSTYBIN_MOCKS_HOST`: `<name>.<domain>` rewritten to `/mock/<name>` by a layer wrapping the finished router in
  `build_app`), `collections.rs` (a mock as Postman / Hurl / curl / .http / k6 / Bruno / Insomnia), `config_file.rs` (`rustybin.yaml`,
  `RUSTYBIN_MOCKS_DIR`, export), `cli.rs` (`rustybin mock ...`), `mcp.rs` (a revision's MCP tools), `service.rs` (the control-plane logic shared by
  the REST handlers and the management MCP), `handlers.rs` (`/mock/{name}/...` data plane, thin
  `/_rustybin/mocks*` handlers over `service.rs`; access through `selfservice.rs`). Self-service (`RUSTYBIN_MOCKS_SELF_SERVICE`): `ai.rs` holds the AI routes
  (`/_rustybin/mocks/ai/spec`, `/{name}/ai/examples`, `/{name}/ai/edit`, `/{name}/coverage`: assist tasks plus the
  mock context, SSE progress, `RUSTYBIN_ASSIST_VISITORS`). Examples stored with a mock live in
  `OperationSettings.examples` (`StoredExample`, source user / ai / generated); `respond.rs` serves them after the
  spec's and before generation; `checks::coverage` (the `no_examples` finding) says which operations lack one.
  `visits.rs` (`Owner` of every mock: instance, visit by manage-token hash, or API key; the visit clock and daily
  ledger on the injectable `limits::Clock`), `selfservice.rs` (`Caller`, `readable` / `manageable` guards: other
  people's visitor mocks are `404`; the `/_rustybin/mocks/visit*` and operator `visits*` routes), `restrict.rs`
  (visitor responses: safety headers, header denylist, inert media types), `leads.rs` (email leads to a webhook or
  file through a bounded retry queue; addresses are never logged). Mocks are matched inside the handler (operations change at runtime), never as
  axum routes. The built-in `petstore` sample (`petstore.yaml`, `RUSTYBIN_MOCKS_SAMPLE`) is what catalogue
  examples and tests call. File-defined mocks load in `server::start_with_state` and reload on SIGHUP.
- `src/mcp/`: the mock MCP server at `/mcp` (protocol, transports, sessions, fake tools), `src/mcp/openapi.rs` (the
  MCP server of each OpenAPI mock) and `src/mcp/manage/`, the
  management MCP server at `/_rustybin/mcp` (`tools.rs` registry, `access.rs` rights, `stdio.rs` for
  `rustybin mcp-stdio`). `src/mcp/inspector/`: the MCP inspector's server side (`mod.rs`: `GET
  /_rustybin/mcp-servers`, the routes, `is_console_post`; `client.rs`: `Target` (this instance in process
  through `state.router`, else the egress client under `Purpose::McpRelay`), `Transport` (modern, session,
  legacy), `Conn`; `relay.rs`: `POST /_rustybin/mcp-relay` (SSE back, bounded sessions) and `/fetch` for
  OAuth discovery documents; `checks.rs`: `CHECKS` and `run` behind `POST /_rustybin/mcp-checks` (JSON or
  JUnit); `store.rs`: saved connections and runs in `<state>/mcp-inspector.json`, credentials sealed with
  `Settings::seal`). `src/mcp/registry/`: the mock MCP registry at `/mcp-registry` in the registry API's
  `/v0.1` shape (`/v0` nested as an alias): `mod.rs` (routes, the `Registry` state with published entries
  and tokens, `Scenario` from the `registry.scenario` setting, the control routes `/_rustybin/registry*`),
  `entries.rs` (generated entries from `inspector::server_list`, scenarios, `page` with the cursor),
  `validate.rs` (the vendored `schema/server.schema.json` 2025-12-11 plus semantic checks),
  `browse.rs` (`GET /_rustybin/registry/browse` under `Purpose::Registry`, cached). File entries are the
  `registry.servers` of `rustybin.yaml`, kept on the `MockStore`. The console side is `ui/lib/mcpclient.js` (`McpClient`, browser or relay, exports),
  `ui/lib/mcpauth.js` (auth modes, the OAuth walk) and `ui/views/mcp.js`. A new check is a `CHECKS` entry
  plus its judgement in `run`, documented in `docs/src/mcp/inspector.md`.
- `src/harness/`: load test mode (`RUSTYBIN_HARNESS`), see above. `mod.rs` (roles, `check_startup`, the `Harness`
  state in `AppState.harness` with bounded runs, target verification, router, catalogue, OpenAPI, headless mode),
  `safety.rs`, `scenario.rs` (model, `Caps`, validation, JSON Schema, preview), `pattern.rs` (every run pattern
  compiled into stages with an exact value and integral, `then`, jitter, sync, wall-clock alignment),
  `limit_test.rs` (the `limit_test` pattern: its tests, their stages, identity pools, retries),
  `mocksource.rs` (a mock source resolved on the node into request variants, invalid requests and
  create-then-read pairs), `identity.rs` (a layer's API keys, minted JWTs, OAuth tokens, forwarded and
  source addresses), `breakdown.rs` (per-operation and per-credential results), `faults.rs` (the `faults`
  block: presets, client and server faults, the per-request draws), `protocols.rs` (the `llm`, `sse`,
  `websocket`, `grpc` and `mcp` sources, SSE parsing, WebSocket frames, gRPC framing, stream statistics),
  `ratelimit.rs`
  (rate-limit headers, the per-layer `LimitAcc` merged across nodes, the analysis in the report's `rate_limits`),
  `live.rs` (live changes: `Change`, the versioned `Controls` every node applies, the `LiveView` the scheduler
  evaluates, the stage log, the closed loops `target`, `breakpoint` and `limit_probe`), `saved.rs` (saved scenarios, `RUSTYBIN_HARNESS_STATE_DIR`),
  `import.rs` (recordings, Postman collections and Artillery scripts into `source.requests`), `http3.rs` (the
  HTTP/3 client of a pinned target, the cargo feature `harness-http3`; `http3_tests.rs`), `chain.rs`
  (server-to-server chains: the prepared hops, the onward call, the per-hop report and its merge;
  `chain_tests.rs`),
  `engine.rs` (own tokio runtime, a scheduler thread per run, `PinnedConnector` with TLS and ALPN, per-worker
  histogram shards rolled into a per-second timeline by a roller thread, closed-model users, saturation, the marker on every
  request and the classification of every answer), `correlate.rs` (the run key, the signed marker and tag,
  digests, the `Correlator` with per-run server stats and the seen-sequence bitmap, the `layer` in `build_app`
  outside inspector capture, `CaptureDecision` the inspector honours), `shaped.rs` (`/harness/{*path}`, the
  run's `servers` profile), `report.rs` (JSON, text, JUnit; classes, overhead, legs, the server side),
  `api.rs` (handlers, the `PATCH` changes and the SSE run feeds; 404 when off except status, schema, scenarios,
  validate and preview), `cli.rs` (`rustybin
  harness run | validate | report`), `scenarios/*.yaml` (built-ins), `test_proxy.rs` (tests: a misbehaving
  gateway with reference rate limiters), `live_tests.rs` (patterns, loops and live changes through the test
  proxy), `limit_tests.rs` (the rate-limit tests against the proxy's limiters), `traffic_tests.rs` (mock
  traffic and identities), `fault_tests.rs` (faults, streams and the protocol sources). MCP tools in `src/mcp/manage/harness.rs` (`harness.run`, `harness.cluster`), console view `ui/views/harness.js`,
  `rustybin_harness_*` in `metrics.rs`. The `harness` control scope (every `harness.*` permission). Clusters:
  `cluster.rs` (the leader: `Coordinator`, `Cluster`, `ClusterRun`, split, merge, report), `member.rs` (the
  agent each member runs: join, heartbeat, lease, clock, and the candidate and voter rules), `wire.rs` (the
  protocol and the replicated snapshot), `election.rs` (the leader's ticker, promotion, stepping down),
  `cluster_tests.rs`, `failover_tests.rs` (with `test_net.rs`: partitions that refuse or drop calls); the
  node routes (`harness::is_node_path`: join, heartbeat, vote, announcement) authenticate with the join
  token or node credential and skip the control-plane auth, key layer and plan limits.
- `src/portal/`: the dev portal (`/portal`, `RUSTYBIN_PORTAL*`, the `portal.*` settings). `mod.rs` (`ItemId`:
  `mock:<name>`, `mcp:<server>`, `llm`, `a2a:<agent>`, `tools`, `registry`; `Published` with the mock's owner,
  `Meta` and `clean_meta`, `FileItem` for `portal.items` of `rustybin.yaml` kept on the `MockStore`, `Portal` in
  `AppState.portal`: items, insights, the catalogue cache, `<state>/portal.json`), `items.rs` (candidates, published
  entries, `hidden_reason`, the catalogue each kind's page reads), `insights.rs` (bounded counters), `api.rs` (the
  SPA from `ui-portal/`, `/portal/api/catalog`, `/events`, collections, `/portal/reference/{id}`, the control routes
  `/_rustybin/portal*`; `portal.auth` checked in `gate`), `session.rs` (the portal sign-in, access control step 13:
  `/portal/api/login`, `/logout`, `/session`, the `rustybin_portal` cookie, an HS256 token on the OAuth key's
  `portal` subkey with `rb_use: "portal"`, accepted under `/portal` only; `can_read_closed` is a session or a bearer
  control credential holding `portal.read`; the throttle is `login::check_sign_in`'s). Each item is `open` or
  `closed` (`portal::Access` on `Published`, `Meta`, `FileItem`): a closed item's catalogue entry is built locked
  (no credential, collection link or connection snippet) for anyone but a reader, never stripped afterwards; a mock
  closes only when `portal::mock_secured` (else `409 security_off`; `secure: true` turns it on through
  `api::secure_mock`); a pulled document may close, never open. A new kind of item or a new detail field that
  carries a credential must be left out of the locked entry (`access_tests::the_locked_catalogue_holds_no_credential`
  plants one of each). The portal SPA imports the console's `dom.js`, `format.js`,
  `markdown.js` and stylesheet from `../ui/`; never its auth or http code. `/portal/*` is a control path (no
  capture, no faults) and exempt from the key layer. The console side is `ui/lib/portal.js` (publish dialog) and
  the Portal tab of `ui/views/manage.js`.
- `src/fault.rs`: `X-Rustybin-Delay` / `X-Rustybin-Fail` middleware.
- `src/admin.rs`: `require(&headers, &config, Permission)` for the data-plane routes that change something
  instance-wide (the `/health` toggles, the global `/flaky` reset); `admin_check` (every permission) for shared
  mode's gate between the operator and an API key.
- `src/session.rs`: `session_key(&headers, client_ip)`, `client_ip(...)`, extractors `Session`, `ClientIp`, `PeerAddr`.
- `src/test_support.rs` (`cfg(test)`): `test_state()`, `test_app()`, `module_app(router)`, `body_json(...)`, ...
- `tests/integration.rs`: starts the real server on ephemeral ports.

## Adding a module

1. `src/foo.rs` with:
   - `pub fn router(state: &AppState) -> Router<AppState>`; create module-local state here and attach it
     with `Extension` (do not add AppState fields unless several modules must share the state).
   - `pub fn catalog() -> Vec<Endpoint>` describing EVERY route it registers
     (`Endpoint::new(path, &["GET"], category::X, "summary").example(Example::get(...))`; use `"ANY"` for `any()`,
     `.websocket()` / `.sse()` for non-plain HTTP, `.expect_status(..)` / `.skip_check(..)` on examples that do not
     return a normal response); every control route (`/_rustybin/*`) also declares its permission
     (`.permission(Permission::MocksRead)`, `.permission_for(&["POST"], Permission::MocksWrite)`, `.open()`,
     `.authenticated(&[])`, plus `.self_service()` where the per-mock rules apply).
   - `pub fn openapi_paths() -> serde_json::Value` (and optionally `openapi_components()`).
2. Register it once in each list: `pub mod foo;` and `ROUTERS` in `src/lib.rs`, `SOURCES` in `src/catalog.rs`,
   `MODULE_PATHS` (and `MODULE_COMPONENTS`) in `src/openapi.rs`. New categories go in `catalog::category`
   and `CATEGORY_ORDER`.
3. Regenerate the README tables (`RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints`).
4. Document it in `docs/src/` (mdBook) with every request example as a Hurl file under
   `docs/examples/<area>/`, included with anchors and run by `docs/examples/run.sh`.

The catalogue tests fail when a `.route("...")` literal is missing from the catalogue, a catalogue path is
missing from the OpenAPI spec (or lacks a method), or an example returns 404/405 against the full app.

## Conventions

- Route syntax is axum 0.8: `/status/{code}`, wildcard `/echo/{*path}`.
- `serde_json` has `preserve_order`: object keys serialise in insertion order (OpenAPI documents keep their
  path and property order). Do not write tests that rely on sorted keys.
- Per-client state is scoped with `session::session_key` (`X-Rustybin-Session`, else client IP).
- Every control route declares its permission in its `catalog()` (`Endpoint::permission`, `::permission_for`
  per method, `::open`, `::authenticated`); `control_auth::check_route` checks it, a handler adds
  `rbac::require(&caller, Permission)` only when the need depends on the body (listed in `rbac::need::BODY_CHECKS`).
  Never a yes/no admin check. On an open control plane nobody holds every permission without an admin token
  outside public mode, else the anonymous reads (`rbac::need::ANONYMOUS`); the admin token is every permission.
- Control plane lives under `/_rustybin/*`, the web console under `/ui`; both are excluded from inspector
  capture and fault injection. Control-plane routes are protected by `control_auth::enforce` when
  `RUSTYBIN_CONTROL_AUTH` is `password` (the default), `token` or `jwt`; data-plane routes never are. The console must not use
  `EventSource` for control-plane feeds (no headers): use `send({stream: true})` + `sseParser`.
- `GET /` must always return 200 (platform liveness check), `GET /_rustybin/ready` too (readiness).
  `/health` is a demo toggle that may return 503.
