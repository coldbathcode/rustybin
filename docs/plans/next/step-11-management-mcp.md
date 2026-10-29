# Step 11: Management MCP parity and the build-an-instance prompts

Master plan: [master-plan.md](master-plan.md). Needs: 02 to 10. Status: **not started**. About 2 days.

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

(filled in when built)
