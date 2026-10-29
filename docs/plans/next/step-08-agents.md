# Step 08: AI agents: who talks to the instance over MCP

Master plan: [master-plan.md](master-plan.md). Needs: 01, 02. Status: **not started**. About 2 days.

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

(filled in when built)
