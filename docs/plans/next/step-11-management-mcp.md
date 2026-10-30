# Step 11: Management MCP parity and the build-an-instance prompts

Master plan: [master-plan.md](master-plan.md). Needs: 02 to 10. Status: **done 2026-10-30**. About 2 days.

## Goal

An agent can build a whole Rustybin instance for a team: sign in, set the AI provider (not the key),
describe and deploy mocks, fill examples, publish a portal, register servers, set agent rules,
export the configuration and push it to git, run an MCP check against a server, and read every
state. Every control capability is a tool, proven by a parity test, and prompts walk an agent
through the common jobs.

## Read first

- `src/mcp/manage/tools.rs` (`ToolDef`, the `tool!` macro, `call`, confirmations, `openWorldHint`),
  `access.rs` (`Right`), `prompts.rs`, `resources.rs`, `mod.rs` (`INSTRUCTIONS`), `tests.rs`.
- Every route and CLI command of steps 01 to 10 (their "CLI and MCP notes" sections list the tool
  names to use).
- `docs/src/agent-mcp.md`, `conformance/mcp/test_manage.py`.

## Design

### Tools

Added per step's notes, with the rights that the routes have: settings (`get_settings`,
`update_settings`, never secrets), egress (`check_egress`), assist (`assist_status`,
`run_assist_task` for read tasks, `assist_usage`), AI mocks (`generate_spec`, `generate_examples`,
`edit_spec_with_ai`, coverage in `get_mock` and `check_mock`), inspector (`list_mcp_servers`,
`mcp_call` through the relay, `run_mcp_checks`), registry (`list_registry_servers`,
`set_registry_scenario`, `browse_registry`, `validate_server_json`), portal (`list_portal_items`,
`publish_portal_item`, `unpublish_portal_item`, `set_portal_branding`), agents (`list_agents`,
`get_agent`, `set_agent_rights`, `revoke_agent`), config and git (`export_config` and
`apply_config` at version 2, `diff_config`, `git_status`, `git_commit`, `git_push`, `git_pull`),
sign-in management (`list_sessions`, `end_session`; the password is set only through the console,
the CLI or the environment).

Rules carried over: a `Right` per tool, role-filtered `tools/list`, annotations plus confirmation
on destructive and outward-facing tools (publishing to an open portal, pushing to git, relaying to
an external server, revoking an agent), captured or mocked content under `untrusted_*` only, audit
lines without arguments. Secrets are refused as tool inputs and never returned.

### Prompts

| Prompt | Arguments | Walk |
|---|---|---|
| `build_instance` | `brief` (what the team needs), `portal` (yes or no) | status, assist status, describe and create each API from the brief, check coverage and fill examples, publish to the portal, export the config, offer the git push |
| `publish_portal` | `items` | metadata per item, publish, verify the public catalog |
| `backup_to_git` | `remote`, `branch` | connect, status, commit with a message from the diff, push, confirm |
| `test_mcp_server` | `url`, `auth` | relay reachability, connect, checks, report |
| `onboard_agent` | `name` | explain the credential model, read the agent's record, propose rights |

### Parity test

`src/mcp/manage/tests.rs` gains a table from the catalogue: every route under `/_rustybin/` (the
same exclusions as the CLI parity test, plus the transport endpoints) maps to at least one tool
name present in `TOOLS`; the test fails on a route without a mapping. A second test checks every
tool has a `Right`, a description, an input schema, and that destructive or open-world tools carry
`confirm`.

### Instructions and resources

`INSTRUCTIONS` rewritten around the jobs (build, publish, back up, test, manage agents), the
resources gain `rustybin://config.schema.json` at version 2, `rustybin://portal/catalog` and
`rustybin://cli.md`. `rustybin mcp-stdio` gains `--url` and `--profile` so a local agent can manage
a remote instance over stdio.

## Tests

- The parity tests above; one full-app test per new tool through the JSON-RPC surface (the pattern
  of `tests.rs`), with the mock assist provider; the prompts render with their arguments.
- `conformance/mcp/test_manage.py`: the prompt set and a `build_instance` walk through the Python SDK.

## Docs

`docs/src/agent-mcp.md` (every tool, the prompts, a "Build an instance with an agent" walkthrough
with Hurl), `conformance/mcp/README.md`, the changelog.

## Done when

The parity test passes with every route mapped; `build_instance` carries an agent from a brief to
a published, exported instance in the conformance walk; all checks pass; committed and pushed.

## Build notes

Built 2026-10-30 (`src/mcp/manage/{settings,assist,inspect,registry,portal,config}.rs`,
`src/mcp/manage/agents/tools.rs`, the sessions and `reload_mocks` tools in `instance.rs`, the
rewritten `prompts.rs`, `resources.rs`, `INSTRUCTIONS`, `stdio.rs`; user docs in
`docs/src/agent-mcp.md` with the walkthrough `docs/examples/agent/manage_build.hurl`). Where
the build differs from the design:

- **One code path per feature.** The tools never call control routes in process (the
  in-process helper refuses control paths on purpose) and never duplicate handler logic.
  Where a handler held the logic inline, it was split: `mocks::ai::{spec_work, examples_work,
  edit_work}` behind the SSE routes, `assist::api::{status_json, collect, refusal_parts}`,
  `portal::api::{control_json, publish_item, unpublish_item, insights_json, ctx_for}`,
  `registry::{status_json, Registry::delete_published}`, `registry::browse::browse_json`,
  `inspector::{servers_json, build_run_spec, run_and_save}`. The MCP inspector's state
  (relay sessions, saved connections and runs) moved from a router-local `Extension` to
  `AppState.mcp_inspector`, since the tools share it with the routes.
- **The parity table lives in `tests.rs`** (`COVERAGE`): every `/_rustybin/*` and `/bin*`
  route maps to a tool, a resource or an excluded reason (the sign-in transport, the OAuth
  walk, node-to-node harness routes, the metrics text, the relay's discovery fetch, the
  visit token rotation, the password route, the MCP endpoint itself). A second test checks
  every definition (title, description, an object schema, `confirm` on destructive and
  open-world tools, no input named like a secret) and a third calls every tool with empty
  arguments to prove each has a dispatch arm.
- **Tools beyond the design's list**, one per capability the routes had: `reload_mocks`,
  `test_assist`, `list_mcp_check_runs`, `list_mcp_connections` (read only: credentials are
  saved in the console or the CLI), `unpublish_registry_server`, `get_portal_insights`,
  `git_init`, `git_resolve`, `git_deploy_key`, `restore_agent`, `end_agent_sessions`.
  `diff_config` is `apply` with `dry_run` forced on (the diff is what every apply answers).
- **Confirmation rule.** Destructive tools and tools that reach another host or publish
  something carry `confirm` in their definition; the implementation asks only when the call
  is of that kind (a local `mcp_call` or `browse_registry` of `/mcp-registry` is not
  confirmed, a remote one is; `publish_portal_item` asks only when the portal is on and open;
  the AI tools ask only when they write; `git_pull` only when it applies). The AI write
  tools confirm before the model call, so a confirmation round trip never runs the model
  twice.
- **`run_assist_task`** refuses the three mock tasks by name (they are not flagged
  `writes`, their routes carry the manage guard) and points at their tools.
- **`export_config`** answers version 2 to an admin and the version 1 mocks-only document
  to a reader (the whole instance needs admin rights, as the REST route does); the
  per-mock export stays version 1. `apply_config` keeps the `yaml` argument as an alias of
  `document` for older callers.
- **`revoke_agent`** gets the caller's own agent id from the dispatch (the observed
  snapshot), so an agent cannot revoke itself.
- **Secrets.** `update_settings` and `set_portal_branding` refuse secret keys with a
  pointer to the console, `rustybin settings set-secret` and the variable; nothing returns a
  value.
- **Left out:** the `check_egress` design note's CLI verb (`rustybin egress check`) was never
  built in step 10 and is not added here; the MCP tool does the dry check. The console
  needs nothing for this step.

Checks run: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (the
parity, definition and dispatch tests plus one test per tool group: settings and egress,
the AI tools with the mock provider, the inspector, the registry, the portal, the
configuration document, git against a bare repository, agents, sessions on a password
instance, the prompts and resources, the scopes; the stdio bridge's profile resolution),
`docs/examples/run.sh` (the new `agent/manage_build.hurl` on the main instance),
`mdbook build docs`, `check_includes.py`, `check-no-dashes.sh`. The Python conformance
walk (`conformance/mcp/test_manage.py`) was extended but not run in this session (no
environment with the MCP SDK on this machine): it runs in CI with the rest of the suite.
