# Step 08: AI agents: who talks to the instance over MCP

Master plan: [master-plan.md](master-plan.md). Needs: 01, 02. Status: **done 2026-10-30**. About 2 days.

## Goal

See every coding agent that talks to the instance through the management MCP server, what it did,
and control it: revoke its token, end its sessions, limit its rights below what its credential
allows, allow or deny named tools, and keep those rules in `rustybin.yaml`. Agreed with the user
before this plan; folded in here because it builds on the sign-in and settings work.

## Read first

- `src/mcp/manage/access.rs` (`Access`, `Right`, `principal`), `src/mcp/manage/tools.rs` (`call`,
  `audit`, `list`), `src/mcp/manage/mod.rs` (the handler, `Access::from_request`),
  `src/mcp/sessions.rs` (`client_info` captured at `initialize`), `src/mcp/transport_streamable.rs`
  (where `_meta.clientInfo` arrives for stateless calls, `META_CLIENT_INFO`), `src/mcp/manage/stdio.rs`.
- `src/control_oauth.rs` (the token claims: `sub`, `client_id`, `jti`; the stateless design that
  needs the denylist), `src/control_auth.rs` `enforce`, step 02's `jti` denylist (reuse it).
- `src/metrics.rs` `count_manage_call`, `ui/lib/agent.js` (the connect panel to move), `ui/app.js`.
- `src/mocks/config_file.rs` for the `agents:` section (step 09 formalises the file).

## Design

### The registry (`src/mcp/manage/agents.rs`)

```rust
pub struct AgentRecord {
    id: String,                 // sha256 of (credential kind, credential fingerprint, client name)
    name: String, version: String,   // clientInfo
    credential: CredKind,       // AdminToken | ControlJwt{sub, scopes, jti?} | OAuth{client_id, sub} | Key{id} | Visit | Stdio
    transport: String,          // streamable-2026 | streamable-2025 | legacy-sse | stdio
    first_seen, last_seen, sessions_open: u32,
    calls: BTreeMap<tool, Counts{ok, error, denied, asked}>,
    last_errors: VecDeque<(at, tool, message)>,   // 20
    rights: AgentRights,        // from settings and config: ceiling, allow, deny, note
    revoked: Option<(at, by)>,
}
```

Bounded: 512 agents, LRU by `last_seen`; sessions tracked by the existing session pool. Every
`tools/call` and `tools/list` updates the record (`clientInfo` from the session or `_meta`;
without one the name is `unknown` and the record is keyed by the credential alone). Persisted
summary (counts, rights, revocations) in the settings store under `agents.<id>`, so rights and
revocations survive a restart; call history is memory only.

### Enforcement

- `Access` gains `agent: Option<Arc<AgentRecord>>`; `allows(right)` applies the agent's ceiling
  (`read-only`, `traffic`, `manage`, `full`), then the tool allow and deny lists in `call` and in
  `tools/list` (a denied tool is not listed). Revoked: every call answers a tool error "this agent
  was revoked by the operator" and the transport answers 403 for its sessions.
- Token revocation: a revoked OAuth or console token's `jti` goes into step 02's denylist for its
  remaining life; the admin token cannot be revoked per agent (the operator rotates it), which the
  UI says.
- Audit lines gain the agent id.

### Routes

| Route | | |
|---|---|---|
| `GET /_rustybin/agents` | admin | The list with status, credential kind, last seen, calls, rights |
| `GET /_rustybin/agents/{id}` | admin | Detail with per-tool counts, last errors, open sessions |
| `PATCH /_rustybin/agents/{id}` | admin | `{rights: {ceiling, allow, deny}, note}` |
| `POST /_rustybin/agents/{id}/revoke`, `/restore` | admin | Revoke the credential's tokens and future calls; restore |
| `DELETE /_rustybin/agents/{id}/sessions` | admin | End its sessions |
| `GET /_rustybin/agents/events` | admin | SSE: calls as they happen (bounded fan-out) |

### The console

View **AI agents** in "AI and agents": the list (name and version, credential, first and last
seen, calls today, status), a detail drawer (tool table with outcomes, errors, sessions, the
rights form with ceiling and tool allow or deny chips, note, Revoke and Restore, End sessions), a
live feed toggle, and the **Connect an agent** panel moved here from the settings dialog (the
`claude mcp add` command and JSON, the signed-in user's token offered). Sparkle action: "Explain
what this agent did today" (a summary task over its call log).

### Configuration

`rustybin.yaml` `agents:` section: `default_ceiling`, `rules: [{match: {name, credential}, ceiling,
allow, deny, note}]`; rules apply to records on sight, settings-store overrides per agent win.
`RUSTYBIN_AGENTS_DEFAULT_CEILING` seeds the default.

### CLI and MCP notes

Step 10: `rustybin agents list|get|rights|revoke|restore|sessions end`. Step 11: `list_agents`,
`get_agent`, `set_agent_rights`, `revoke_agent` (admin, confirmed; an agent cannot revoke itself).

## Tests

- Unit: record keying, bounds and LRU, rights ceiling and tool lists, revocation and the denylist,
  config rules matching.
- Full app: two agents with different `clientInfo` on the same token are two records; a ceiling
  hides tools from `tools/list` and refuses calls; revoke ends sessions and refuses the OAuth
  token; restore; the stdio transport registers as an agent; the events feed.
- Console check: the view lists the Playwright client's agent, revokes and restores it.

## Docs

`docs/src/agent-mcp.md` (a "Managing agents" section), `console.md`, `configuration.md`, the
changelog, Hurl examples.

## Done when

Every management MCP caller is visible with its history and controllable with rights, revocation
and session ending, on all surfaces; all checks pass; committed and pushed.

## Build notes

Built 2026-10-30 (`src/mcp/manage/agents/` with `api.rs` and `tests.rs`, `ui/views/agents.js`,
`docs/examples/agent/manage_agents.hurl`, the "Managing agents" section of
`docs/src/agent-mcp.md`). Where the build differs from the design above, and why:

- **The record is attributed where the client is known, not in `Access`.** `Access` is built
  from the HTTP request before the body or the session is read, so it carries the credential
  (`Credential`: `admin`, `console {user}`, `jwt {sub, scopes}`, `oauth {sub}`, `key {id}`,
  `visit {fingerprint}`, `open {session}`; named from what control auth, the key layer or the
  visit lookup already verified) and the presented token's `jti` and expiry. `tools/list` and
  `tools/call` then call `agents::observe_call` with the session's `clientInfo` or `_meta`'s,
  and apply the agent's rights after the credential's (`Effective::allows`). A revoked agent
  lists no tools. The id is the first 8 bytes of SHA-256 over the credential's fingerprint and
  the client name, in hex.
- **Ceilings** map onto the tool table: `read-only` is a read-only tool whose right is not
  `traffic`; `traffic` every read-only tool; `manage` every tool whose right is not `admin`,
  `operator` or `harness`; `full` all. Rights merge field by field: the agent's override, else
  the first matching rule, else the default ceiling (stored or env setting, else the file's
  `default_ceiling`, else `full`).
- **Persistence is `<state>/agents.json`, not the settings store.** The settings store is a
  schema of named keys with env seeds; per-agent records are dynamic and bounded on their own
  (like the portal and the inspector store). Rights, notes, revocations, call totals and the
  daily count are kept (counts flushed at most every 30 seconds); sessions, errors and token
  ids are memory only. A revoked OAuth client is refused again at load.
- **Revocation** uses step 02's denylist for the token ids the agent presented (console
  tokens, OAuth access tokens, platform control JWTs, which now check the denylist too) and a
  new bounded subject denylist in `login::Tokens` (1024, kept across "sign out everywhere") for
  an OAuth client's `sub`, which also stops its refresh tokens. Restore takes both back
  (`Tokens::unrevoke`, `restore_subject`). Sessions end by cancelling them; a session of a
  revoked agent answers `403` (a bounded session index, 4096), and a revoked token answers
  `401` before that. The API answers a `note` for the credentials it cannot revoke (the admin
  token, keys, visits, an open control plane).
- **Bounds**: 512 agents (least recently seen first, agents with rights, a note or a
  revocation last), 20 errors (300 characters), 32 sessions and 16 token ids per agent, 64
  rules, 128 tools per list, 16 feed listeners.
- **The stdio bridge** is recognised by its `User-Agent` (`rustybin-mcp-stdio/`), so its
  transport shows `stdio`; it remains a client of the HTTP server.
- **AI**: "Explain what this agent did" is a new `agent` kind of the `explain` task (the
  drawer already renders its answer), not a separate task.
- **Console**: the detail is a panel under the list (`#/agents/<id>`), not a drawer (the
  drawer belongs to AI assist). Tool chips cycle default, allow, deny. Connect an agent moved
  from Management, General (now a link) to this view; the Mock APIs view keeps its compact
  panel. The `agents` settings group renders in the view's Defaults and rules card.
- **`tools/list`** keeps its 60 s cache hint; a client that caches it still gets the tool
  error from `tools/call` after a change. No `list_changed` notification is sent.
- **Left out**: nothing from the design.

CLI and MCP notes for later steps: step 10's `rustybin agents list | get <id> | rights <id>
[--ceiling --allow --deny --note --reset] | revoke <id> | restore <id> | sessions end <id>`
call the six routes. Step 11's `list_agents`, `get_agent`, `set_agent_rights` and
`revoke_agent` (admin, confirmed; refuse when the id is the caller's own agent, which
`observe_call` gives) wrap `Agents::list`, `get`, `set_rights`, `revoke` and `restore`. Step 09
exports the `agents:` section (`FileSection`) and the `agents.default_ceiling` setting.

Checks run: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (21 new
tests in `agents/tests.rs`, one in `control_oauth/tests.rs`), `docs/examples/run.sh`
(`agent/manage_agents.hurl` on the secured instance), the console check
`conformance/ui/run.sh` (the new "ai agents" step: list, rights, live feed, revoke and
restore; the jwt agent panel now on this view), `mdbook build docs`, `check_includes.py`,
`check-no-dashes.sh`.
