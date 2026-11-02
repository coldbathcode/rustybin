# Step 03: MCP: saved credentials, self-calls, agent identity, untrusted content

## Goal

A saved MCP connection's credentials go only to that connection's target. The management MCP server
is never reachable from its own tools. An agent's revocation and ceiling hold whatever client name
it sends. Third-party text reaches agents only under `untrusted_*`.

## Findings

| Sev | Where | Problem | Fix |
|---|---|---|---|
| high | `src/mcp/manage/inspect.rs:51-77` (`connection`), `src/mcp/inspector/relay.rs:328-343`, `src/mcp/inspector/mod.rs:233-256` (`build_run_spec`) | the caller's `target` overrides the saved connection's target while the sealed credentials stay attached; `mcp_call` and `run_mcp_checks` are `Right::Read` (every API-key holder in shared mode). Pointed at `/mcp`, `inspect_request` echoes them and inspector capture stores `Authorization` unredacted; pointed at an allowed host, the token goes there | with `connection`, refuse a different target (or a different origin) in all three paths; require admin to use a connection that holds secrets |
| high | `src/mcp/inspector/client.rs:58` (`local_allowed`) | accepts `MANAGE_PATH`: a ceilinged or deny-listed agent calls a denied tool through `mcp_call`; the nested call is a new agent with the default ceiling (`full` unless set); on an open plane it is admin | refuse `/_rustybin/mcp` as an inspector, relay and checks target; never attach saved credentials to a local control path |
| high | `src/mcp/manage/agents/mod.rs:189,709-725,872-880` | the agent id hashes credential plus `clientInfo.name`; revocation and rights live on that record, so `{name: "x2"}` starts over for the admin token, API keys, visit tokens and open mode; `credential_note` says calls are refused | apply revocation and rights per `credential.fingerprint()` (most restrictive record wins); fix the note and `docs/src/agent-mcp.md` |
| medium | `src/mcp/manage/registry.rs:18-35`, `src/mcp/registry/mod.rs:1054-1079` | anonymously published registry entries (256 x 64 KiB) come back to agents in plain fields, uncut, about 16 MiB per `list_registry_servers`, validated again per call | `untrusted_entries`, 4 KiB each within `MAX_UNTRUSTED_TOTAL`, paging, cached validation |
| medium | `src/mcp/manage/inspect.rs` (`mcp_call` structured block, `run_mcp_checks`), `src/mcp/inspector/checks.rs:154-158,660` | remote servers' error messages, headers, tool names and check row messages return in normal fields; `untrusted_body` is not cut | `untrusted_error`, `untrusted_headers`, `untrusted_report`; truncate like `untrusted()` |
| low | `src/mcp/inspector/client.rs:436` | the legacy transport's server-chosen `endpoint` may be another host, and saved credentials follow | require the endpoint's origin to equal the SSE URL's |
| low | `src/mcp/manage/access.rs` (`admin_credential`), `instance.rs:323` | `reset_flaky` forwards the caller's own console or OAuth token as `X-Rustybin-Admin-Token` into a captured request | call the flaky reset function directly |
| low | `src/inspector.rs`, `src/keys` `redact_header` | in-process inspector requests are captured with `Authorization`, `X-API-Key` and `?api_key` in clear | redact credential headers and query keys on captures of inspector-originated requests |

## Tests

A read-scope caller with `connection` and another target gets an error (tool, relay, checks); the
inspector refuses `/_rustybin/mcp`; a revoked agent reconnecting under another name is refused; a
denied tool through `mcp_call` is refused; a published entry with instructions in its description
is returned only under `untrusted_entries`, truncated.

## Done when

The tests pass, `docs/src/mcp/inspector.md` and `docs/src/agent-mcp.md` describe the target rule and
the identity rule.

## Build notes (2026-11-02)

- Every finding is fixed; none was closed as not reproducible.
- One rule, `store::use_connection(id, target, admin)`, for the management tools, the
  relay and the checks: another target with `connection` is `400`
  `connection_target_mismatch` (`store::same_target`: the same path on this instance,
  query aside, or the same origin), and a connection holding secrets needs admin
  (`403` `connection_needs_admin`; on REST, `admin::admin_check` of the request).
- `client::local_allowed` no longer accepts `/_rustybin/mcp`; the console still lists
  the management server and connects to it in browser mode. With it gone no local
  target is a control path, so saved credentials never reach one.
- Agents: `Effective.also` carries the overrides of the other records of the same
  `credential.fingerprint()`, and a sibling's revocation refuses the call. Rules of
  `rustybin.yaml` still match by name (the operator writes them per client), so
  only per-agent overrides and revocations follow the credential. The existing
  tests that asserted "another client on the same token is not limited" now assert
  the opposite.
- `list_registry_servers`: `untrusted_entries` (each cut at 4 KiB through
  `tools::Budget`, 64 KiB per result), `cursor` and `limit` (16, at most 50),
  `total`, `next_cursor`; validations cached by digest (`validate_cached`, 1024).
- `mcp_call`: `untrusted_error`, `untrusted_headers`, a cut `untrusted_body`, and the
  JSON-RPC code in the summary only when it is a number. `run_mcp_checks` answers the
  report under `untrusted_report` (cut) beside `id`, `ok`, `counts`. The REST check
  route still answers the full report to the console.
- `Access.admin_credential` and `in_process(as_admin)` are gone: `reset_flaky` resets
  `AppState.flaky` (the `/flaky/*` counters, now shared) directly.
- Captures: `client::InspectorOrigin` marks the inspector's in-process requests; the
  inspector capture redacts every header and query value whose name
  `keys::looks_secret`.
