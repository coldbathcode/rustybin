# Plan: a management MCP server (configure Rustybin from an AI agent)

Status: **built** (phases 0 to 5, 2026-10-20). Section 13 lists where the build differs from the plan;
the user documentation is `docs/src/agent-mcp.md`. Planned 2026-10-18. Last updated: 2026-10-20.

## 1. Goal

Let someone connect an AI agent (Claude Code, or any MCP client) to their Rustybin and say what they want
in plain words: "mock this OpenAPI spec, secure it with an API key, make `GET /orders/0` answer 404, then
show me what my gateway sent". The agent does it through a **management MCP server** built into every
Rustybin: self-hosted instances, Rustybin Cloud pods and the free demo alike.

```bash
claude mcp add --transport http rustybin https://<host>/_rustybin/mcp \
  --header "Authorization: Bearer <admin token or control JWT>"
```

Today an agent can already drive the control-plane REST API with `curl`, the `rustybin mock` CLI and
`rustybin.yaml` (with its JSON Schema), but it has to discover all of that from the docs. The management
MCP gives it typed tools, descriptions written for models, safe defaults and the same permissions as the
console.

### What a user can do through an agent

1. Ask what the instance can do ("which mock LLM providers are there?", "how do I test MCP auth?").
2. Create, edit, deploy, roll back, switch off and delete OpenAPI mocks; set responses, rules, delays,
   validation and mocked security per operation; check them before deploying.
3. Test what they built without leaving the conversation: send a request to the instance, get a token from
   the built-in identity provider, validate a body against the spec, and wait for the request their
   gateway sends next ("I'll send it now, tell me what arrived").
4. Read what arrived: a mock's captured calls, the request inspector, a request bin, the plan usage that
   explains a 429.
5. Export and apply `rustybin.yaml`, and write collections (Hurl, Postman, ...) into their repository, so
   agent work ends up as configuration and tests as code.
6. On a shared demo (self-service), manage their own mocks and visit, never anyone else's.

### Non-goals (v1)

- **Platform operations** (create pods, change a pod's size or settings): the platform API's job, a separate
  MCP later ("option 2" in the 2026-10-18 discussion).
- **Changing environment configuration at runtime.** Settings come from `RUSTYBIN_*` variables; the MCP shows
  them (the same view as `/_rustybin/config`) but cannot change them.
- **Fetching from URLs or calling other hosts.** No server-side fetch of specs or anything else (SSRF, the
  same rule as the mocks plan): the agent reads local files and passes their content, and `try_request`
  only reaches this instance, in process.
- **Rotating the visitor's manage token** through the MCP: the agent's own connection uses that token.
- **The email step of self-service visits.** Consent belongs in the console form, not in an agent's tool call
  (section 5).
- Replacing the REST API, the console or the CLI. They stay; the MCP is another client of the same logic.

## 2. How it fits the existing code

| Existing piece | Reuse |
|---|---|
| The mock MCP server (`src/mcp/`) | Protocol (`protocol.rs`), Streamable HTTP transport, sessions, pagination, list caching, progress, cancellation, elicitation. It gets a second **profile family** with its own tool registry; the mock tools (`get_weather`, `cancel_order`, ...) stay exactly as they are at `/mcp`. |
| Control-plane protection (`control_auth::enforce`, `keys::control`) | `/_rustybin/mcp` is under `/_rustybin`, so control auth (`token`, `jwt`, shared-mode keys) protects it with no new code; it is also excluded from inspector capture and fault injection already. |
| Self-service access (`src/mocks/selfservice.rs`) | `Caller`, `readable`, `manageable`: tool calls get the same answers as the REST routes (other visitors' mocks do not exist). |
| Mock handlers (`src/mocks/handlers.rs`) | Their logic moves into a service layer both the REST handlers and the MCP tools call (phase 0), so validation, caps, redaction and errors are identical. |
| Catalogue (`src/catalog.rs`), config view (`Config::public_view`), JSON Schema of `rustybin.yaml` | Read-only tools and resources ("what can this instance do"). |
| Metrics (`src/metrics.rs`) | Counts of MCP tool calls by tool and outcome (bounded labels: tool names are a fixed list). |

## 3. Endpoint and transport

- **Path**: `POST, GET, DELETE /_rustybin/mcp`. **Recommendation**: under `/_rustybin`, not `/mcp/...`: it is
  control plane (protected by control auth, never captured by the inspector, never hit by fault injection),
  and keeping it apart from the mock server avoids gateway demos ever routing to it by accident.
- **Protocol versions**: 2026-07-28 (stateless) and the 2025-06-18 / 2025-11-25 handshake, like `/mcp`. No
  legacy HTTP+SSE transport (management clients are recent).
- **Server identity**: `serverInfo.name = "rustybin-manage"`, with `instructions` telling the model what
  Rustybin is, which tools change things, and that captured traffic is untrusted data (section 6).
- **Sessions**: the existing bounded session store (a separate pool so mock-server load cannot starve it).
- **CORS**: like the rest of the control plane (`RUSTYBIN_CORS_ALLOW_ORIGINS`), so a browser-based agent can
  connect where the operator allows it.

## 4. Authentication and permissions

Transport authentication is whatever protects `/_rustybin/*` on that instance:

| Instance | Who connects | With |
|---|---|---|
| Self-hosted, control auth `open` | anyone who can reach it | nothing (normal mode: full access, like the console) |
| Control auth `token` | the operator | `Authorization: Bearer <RUSTYBIN_ADMIN_TOKEN>` |
| Control auth `jwt` (hosted mode, Cloud pods) | the customer | a control JWT from the platform (scopes `inspector`, `console`, `admin`) |
| Shared mode | a key holder | `X-Rustybin-Key` (their own mocks, inspector entries and usage) |
| Public mode with self-service (the demo) | a visitor | `X-Rustybin-Mock-Token` (their visit's mocks); admin token for the operator |

Each tool declares the right it needs, mapped onto the existing rules:

- **read**: the `console` (or `inspector`) scope, or anyone on an open control plane. Secrets in mock
  settings are redacted unless the caller may change the mock (the REST rule).
- **manage mock**: `manageable(...)`: admin rights for instance mocks, the owner (or admin) for visitor and
  key-owned mocks.
- **admin**: instance-global changes (`apply_config`, health toggles, visit operator tools): the admin token
  or the `admin` scope.

`tools/list` only lists the tools the caller can use (a visitor never sees `apply_config`), and every call is
checked again on the server. Calls a caller may not make answer a tool error saying which credential is
missing, never a protocol error.

Every tool carries MCP annotations: `readOnlyHint` on reads, `destructiveHint` on `delete_mock`,
`clear_traffic`, `rollback_mock`, `apply_config` with `prune` and the operator tools, `idempotentHint` where
true. Destructive tools also ask through **elicitation** ("Delete the mock orders-api and its traffic?")
when the client supports it; without elicitation they need `"confirm": true` in the arguments, which the
tool description tells the model to set only after the user agreed.

Every call that changes something is logged as one JSON line: tool, target, caller kind and fingerprint,
outcome, never arguments that may hold secrets or spec bodies.

## 5. Tools (v1)

Names are short verbs on nouns; arguments mirror the REST bodies (and `rustybin.yaml`), so docs and agents
can move between the two. Every result has a one-line text summary plus `structuredContent`, and includes
links the user can open: the mock's URL, its console page (`/ui/#/mocks/<name>/<tab>`), a `curl` for a sample
request.

**Instance**

| Tool | Right | |
|---|---|---|
| `get_instance` | read | Version, modes (public, shared, hosted, self-service), control auth mode, limits, the public config view, URLs of the console and docs |
| `find_endpoints` | read | Search the route catalogue by text or category ("mock LLM Anthropic streaming"): path, methods, summary, an example request |
| `get_health` / `set_health` | read / admin | The `/health` demo toggle |
| `get_usage` | read | Plan, limits and what is left (requests, egress, in flight, streams; the caller's key in shared mode), so an agent can explain a 429 instead of guessing |
| `get_key_usage` | admin | Per-key daily totals in shared mode (the `/_rustybin/keys/usage` route), paginated |
| `reset_flaky` | read / admin | Reset the caller's `/flaky` counters between test runs; `scope: "all"` (everyone's) needs admin, like `/flaky/reset` |

**OpenAPI mocks**

| Tool | Right | |
|---|---|---|
| `list_mocks` | read | Name, URL, state, security, calls, owner (filters: text, state) |
| `get_mock` | read | Detail: operations with hit counts, settings (redacted unless manageable), deploy report summary, revisions |
| `preview_spec` | read | Parse a spec without creating anything (the `import` route): title, operations, schemes, warnings, check results |
| `get_mock_spec` | read | The spec as uploaded (the draft's), so the agent can edit it and send it back with `update_mock`; optionally the served `openapi.json` with the mock's server URL |
| `create_mock` | manage | From spec text (JSON or YAML, Swagger 2.0 accepted) with optional name, responses, security, validation, deploy; visitors get the suffixed name and their manage token is not returned (they hold it already) |
| `update_mock` | manage | Replace the spec and/or settings (draft); `deploy: true` to publish |
| `set_operation` | manage | One operation's response override, rules, mode, delay, public flag |
| `test_rules` | read | Which rule would answer a captured call (the "Would match" preview) |
| `check_mock` | read | The deploy report: errors and warnings per operation |
| `deploy_mock` | manage | Publish the draft (`force` only after the user accepted the check errors) |
| `rollback_mock` | manage | Back to a revision (destructive hint) |
| `set_mock_enabled` | manage | On or off |
| `duplicate_mock` | manage | Copy under a new name |
| `delete_mock` | manage | Destructive, confirmed |
| `sample_request` | read | A valid request for an operation, as `curl` and as parts, plus the response the mock would send |
| `validate_payload` | read | Check a request or response body against an operation's schema (the `.../validate` route): which fields break the spec, for example after a gateway transformation |
| `export_collection` | read | The mock as a Postman collection, Hurl file, curl script, `.http` file or k6 script, with an optional base URL (a gateway route), for the agent to write into the user's repository |
| `get_mock_data` / `clear_mock_data` | read / manage | Items a `stateful` mock stored, per collection; clearing is destructive and confirmed |
| `get_mock_traffic` | read (owner for visitor mocks) | Captured calls with operation, status, validation and auth result; bodies only on request (section 6) |
| `clear_traffic` | manage | Destructive, confirmed |
| `export_config` | read | `rustybin.yaml` for one mock or all (secrets redacted unless manageable) |
| `apply_config` | admin | Apply a `rustybin.yaml` document; `dry_run: true` reports what would change; `prune` is destructive and confirmed |

**Traffic**

| Tool | Right | |
|---|---|---|
| `list_requests` | read | Request inspector entries (filters: path, method, status, session, since); scoped by key in shared mode |
| `get_request` | read | One captured request in full |
| `wait_for_requests` | read | Wait for the next captured requests matching a filter (path prefix, method, header, session; a mock name for its captures), up to `timeout_secs` (default 30, at most 60), and return them; built on the inspector's and the mocks' live feeds. For "I'll send it through the gateway now, tell me what arrived" |
| `clear_requests` | admin | Empty the request inspector before a test run (the `DELETE /_rustybin/requests` rule); destructive and confirmed |
| `create_bin` / `get_bin` | read | A request bin (with its configured response) and what arrived in it (data-plane resources, created for the caller's session) |
| `list_push_notifications` | read | What the A2A push notification sink received for a sink id (`/a2a/webhook-sink/{id}`) |

**Testing**

| Tool | Right | |
|---|---|---|
| `try_request` | read | Send a request to this instance's own data plane and return status, headers, body and timing: a mock operation, the mock LLM, the mock MCP server, any catalogued route. Handled **in process** through the full application (the same middleware a real client meets), never over the network, so it cannot reach another host; `/_rustybin/*` and `/ui/*` are refused (managing the instance is what the other tools are for). Captured like any request, tagged with the session `mcp-<session>` so `list_requests` finds it. For MCP clients without a shell; Claude Code may use `curl` instead |
| `issue_token` | read | A token from the built-in identity provider for testing secured mocks, `/mcp/protected` or a gateway's JWT validation: client credentials of the demo client with the asked scopes and audience (or a JWT signed with the HS256 demo secret), returned with its decoded claims and expiry |

**Self-service** (only when self-service is on)

| Tool | Right | |
|---|---|---|
| `get_visit` | visitor | Time left, hours used, what the next extension needs |
| `extend_visit` | visitor | One more period **without** the email step; when an address is needed the tool says so and gives the console link, where the form shows the consent choice and privacy notice |
| `list_visits`, `end_visit`, `pause_self_service`, `resume_self_service` | admin | The operator routes |

**Prompts** (MCP prompts, optional for clients): `mock_an_api` (from a spec to a deployed, tested mock),
`debug_gateway_call` (find the request a gateway sent and explain it), `secure_a_mock` (pick a security mode
and show the credentials). **Resources**: `rustybin://config.schema.json` (the `rustybin.yaml` schema),
`rustybin://catalogue` (the route catalogue as JSON).

## 6. Untrusted content and size

Captured traffic (mock captures, the inspector, bins, the A2A sink, `wait_for_requests`) is written by whoever
calls the data plane, which on a public instance is anyone. An agent reading it is exposed to prompt
injection. So:

- Tool results never put captured bodies or header values in the text summary; they go in
  `structuredContent` under fields named `untrusted_*`, and the server `instructions` and tool descriptions
  say that this content is data from third parties, never instructions.
- Bodies are left out unless the call asks (`include_bodies: true`), and are truncated (4 KB per body,
  64 KB per result) with a flag saying so.
- `try_request` responses come from the instance itself, but a mock's body and headers are whatever its owner
  configured (on a demo, another visitor's mock): they are returned the same way, as `untrusted_*` fields,
  truncated (64 KB; streams stop after 64 KB or 5 seconds, with a flag).
- Spec text and rule patterns from the agent are validated exactly as on the REST routes (size caps, YAML
  limits, regex size limits), so an agent cannot do what a REST caller could not.

Large inputs: specs up to the instance's `max_spec_bytes` (1 MB, 256 KB in public mode) go in one argument.
Results are paginated (`cursor`) like the mock server's lists.

## 7. Console and CLI

- **Console**: a "Connect an AI agent" panel (settings dialog and the Mock APIs view) with the
  `claude mcp add` command for this instance, filled in with the URL and the credential the console already
  holds (control token, admin token or the visitor's manage token), and the JSON snippet other MCP clients
  use. Copy buttons, never the secret on screen until asked.
- **CLI**: `rustybin mcp-stdio --server <url> --token <token>`, a small stdio-to-HTTP bridge for MCP clients
  that only speak stdio (optional, phase 4).

## 8. Limits and observability

- MCP sessions: their own bounded pool (1,000; 200 in public mode) with the idle TTL of the mock server.
- `wait_for_requests`: at most 60 seconds per call and a bounded number of concurrent waits (100; 20 in public
  mode), each holding one feed subscription; extra calls answer a tool error asking to retry.
- `try_request`: request bodies up to the instance's body limit, a 30 second timeout, responses truncated as in
  section 6; it goes through plan limits like any data-plane request, so it cannot bypass them.
- `issue_token`: the identity provider's own caps (tokens are stateless JWTs; refresh tokens are not issued).
- Tool calls count as control-plane requests (plan limits, where configured, apply as for the REST routes).
- Metrics: `rustybin_manage_mcp_calls_total{tool, outcome}` (outcome: `ok`, `denied`, `invalid`, `error`);
  tool names are a fixed list, so the series are bounded.

## 9. Documentation

- A page "Configure Rustybin from an AI agent" (`docs/src/concepts/agent-mcp.md` or under Mock APIs): connect
  Claude Code and other clients, credentials per instance type, the tool list, safety (confirmations,
  untrusted content), examples of what to ask.
- Hurl examples of the JSON-RPC calls (`tools/list` per role, `create_mock`, `deploy_mock`, a refused call,
  `get_mock_traffic` with untrusted fields) on the open, secured and self-service documentation instances.
- README endpoint tables (the catalogue entry), configuration (no new variables expected), console page,
  changelog, roadmap.

## 10. Phases

| Phase | Scope | Done when |
|---|---|---|
| 0. Service layer | Move the logic of the mock control-plane handlers into functions shared by REST and MCP (inputs and outputs as plain types; access through `selfservice`); no behaviour change | All existing tests pass unchanged; handlers are thin |
| 1. Endpoint, reads and testing | `/_rustybin/mcp` (profile family, registry, role-filtered `tools/list`), `get_instance`, `find_endpoints`, `get_health`, `get_usage`, `list_mocks`, `get_mock`, `get_mock_spec`, `preview_spec`, `check_mock`, `sample_request`, `validate_payload`, `export_config`, `list_requests`, `get_request`, `try_request`, `issue_token`; catalogue and OpenAPI entries | Unit tests through the full app per tool and per role (open, read-only token, admin, visitor, key); `try_request` refuses `/_rustybin/*` and never opens a socket |
| 2. Mock changes and waiting | `create_mock`, `update_mock`, `set_operation`, `test_rules`, `deploy_mock`, `rollback_mock`, `set_mock_enabled`, `duplicate_mock`, `delete_mock`, `clear_traffic`, `apply_config` (dry run), `set_health`, `wait_for_requests`, `clear_requests`; annotations, elicitation or `confirm`, audit lines, metrics | A scripted session builds, deploys, calls (with `try_request`) and deletes a mock; `wait_for_requests` returns a call sent while it waits and times out cleanly; refused calls for every role that may not |
| 3. Untrusted content, the rest, self-service, shared mode | `get_mock_traffic`, bins, `list_push_notifications` and `try_request` bodies as `untrusted_*` fields with truncation; `export_collection`, `get_mock_data`, `clear_mock_data`, `reset_flaky`, `get_key_usage`; visit and operator tools; key-scoped results in shared mode | Tests that captured and mocked bodies never reach the text summary; visitor isolation tests mirroring `selfservice_tests.rs` |
| 4. Prompts, resources, console, CLI | The three prompts, two resources, the "Connect an AI agent" panel, optional `rustybin mcp-stdio` | Browser check of the panel; the official Python MCP SDK lists tools, prompts and resources (`conformance/mcp/`) |
| 5. Docs | Page, Hurl examples on three documentation instances, README, changelog | `docs/examples/run.sh` and `check_includes.py` pass |

## 11. Testing

- Every tool through the full application (`test_support`), for each role: allowed, refused with the right
  message, and absent from `tools/list` when refused.
- The official Python MCP SDK (`conformance/mcp/`) as a client of `/_rustybin/mcp`: handshake and stateless
  versions, `tools/list`, a create, deploy and delete round trip, elicitation for a delete.
- An injection test: a captured call whose body says "ignore previous instructions" appears only under an
  `untrusted_` field, truncated, never in the text content; the same for a mock whose example body says it,
  read through `try_request`.
- `try_request`: answers like a real client gets (status, headers, body of a mock, a mock LLM stream cut at
  the limit), refuses control-plane paths, and is captured with its `mcp-` session.
- `wait_for_requests`: a request sent during the wait is returned, a filter that matches nothing times out
  with an empty list, the concurrency cap answers a tool error, and the subscription is released either way.
- `issue_token`: a token it returns opens a mock secured with JWT security and `/mcp/protected`.
- Hurl examples double as end-to-end tests (section 9).

## 12. Open questions (settled by the build)

1. **Path**: `/_rustybin/mcp`, on the control plane.
2. **OAuth discovery**: not in v1; agents send a bearer header. RFC 9728 metadata is on the roadmap under
   "Under consideration".
3. **Instance-global toggles**: `get_health` / `set_health` (admin) only. Fault injection stays per request
   (`X-Rustybin-Delay`, `X-Rustybin-Fail`), which `try_request` can send.
4. **stdio bridge**: built in v1 (`rustybin mcp-stdio`, `src/mcp/manage/stdio.rs`).

## 13. As built (differences from the plan)

Built 2026-10-20 in `src/mcp/manage/` (registry `tools.rs`, rights `access.rs`, tool modules `instance.rs`,
`mocks.rs`, `testing.rs`, `traffic.rs`, `visits.rs`, plus `prompts.rs`, `resources.rs`, `stdio.rs`) over the
service layer `src/mocks/service.rs` (phase 0: the REST handlers in `handlers.rs` and the self-service guards
call it too). Every tool, prompt and resource in sections 5 and 7 exists. Differences:

- **Rights**: besides read, manage and admin there are **traffic** (read, or the `inspector` scope alone:
  `list_requests`, `get_request`, `wait_for_requests`, `get_bin`, `get_mock_traffic`,
  `list_push_notifications`), **visitor** (a running visit) and **operator** (admin on a self-service
  instance). `control_auth` lets `inspector` and `console` scopes through to `/_rustybin/mcp` for every
  method, since the server checks each tool against the grant itself.
- **Open control plane with an admin token set**: callers without the token get the read, test and traffic
  tools only (the `admin::require_admin` rule), not full access. The admin token is accepted as a bearer or as
  `X-Rustybin-Admin-Token`.
- **Self-service**: a visitor's agent needs no credential for its first mock: `create_mock` starts a visit and
  **returns** its manage token (section 5 said the token is never returned, assuming the visitor already held
  it), which the agent then sends as `X-Rustybin-Mock-Token`.
- **`try_request`** captures under the caller's `X-Rustybin-Session` when it sends one, else `mcp-<id>`.
- **`issue_token`** issues client-credentials tokens of the demo client only; the HS256 demo-secret JWT
  option was left out.
- **`export_collection`** takes the formats of `mocks::collections::FORMATS` (Postman, Hurl, curl, `.http`,
  k6). Bruno and Insomnia stay on the roadmap.
- **Audit log**: refused calls are logged too (target `rustybin::manage`), with the caller as `admin`,
  `key:<id>`, `visit:<fingerprint>` or `session:<label>`.
- **Console**: the "Connect an AI agent" panel is `ui/lib/agent.js` (settings dialog and the Mock APIs
  view); the token is masked until asked for, the copy buttons copy the real value.
- **stdio bridge**: `--server` falls back to `RUSTYBIN_MCP_URL` (a bare base URL gets `/_rustybin/mcp`
  appended), `--token` to `RUSTYBIN_TOKEN` or `RUSTYBIN_ADMIN_TOKEN`; `--header` adds any header (a
  visitor's `X-Rustybin-Mock-Token`).
- **Docs**: the page is `docs/src/agent-mcp.md` (not under `concepts/`); Hurl examples in
  `docs/examples/agent/` on the open, secured and self-service instances.
- **Tests**: `src/mcp/manage/tests.rs` (full app per role, injection, `try_request`, `wait_for_requests`,
  `issue_token`, elicitation, visitor isolation, shared mode); the official Python SDK checks in
  `conformance/mcp/` and the console panel in `conformance/ui/console_e2e.py`.
- Also in the same commit, unrelated to the plan: `.github/workflows/docker-publish.yml` publishes the image
  to GHCR.
