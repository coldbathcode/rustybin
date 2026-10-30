# Configure Rustybin from an AI agent

Every Rustybin instance runs a management [MCP](https://modelcontextprotocol.io) server
at `/_rustybin/mcp`, so an agent such as Claude Code can configure it for you: create
and deploy OpenAPI mocks, change their responses, rules and security, send test
requests, mint tokens, and read what your gateway sent. Ask in plain words ("mock
`openapi/orders.yaml`, secure it with JWT and tell me which token the gateway needs")
and the agent picks the tools.

This is not the [mock MCP server](mcp/index.md) at `/mcp`, which serves fake tools for
MCP gateway demos. Both speak the same protocol versions and transports; the
management server sits on the control plane, so [control-plane
authentication](concepts/control-plane-security.md) protects it, and its tools call the
same code as the REST routes under `/_rustybin/*`: permissions, redaction, size caps
and error messages are identical.

## Connect an agent

Claude Code:

```bash
claude mcp add --transport http rustybin https://<host>/_rustybin/mcp \
  --header "Authorization: Bearer <token>"
```

Other clients take the usual `mcpServers` JSON:

```json
{
  "mcpServers": {
    "rustybin": {
      "type": "http",
      "url": "https://<host>/_rustybin/mcp",
      "headers": { "Authorization": "Bearer <token>" }
    }
  }
}
```

The [web console](console.md) fills both in for you: "Connect an agent" in the AI
agents view and "Connect an AI agent" in the Mock APIs view show the command with this instance's
URL and the credential the console already holds (the token of your password
sign-in, a control token, the admin token or a visit's manage token). The token stays
masked until you ask to see it; the copy buttons copy the real value. A script gets
the same token from `POST /_rustybin/login` (see [User name and
password](concepts/control-plane-security.md#user-name-and-password)).

Clients that only speak stdio use the bridge built into the binary. It reads one
JSON-RPC message per line on stdin, posts it to the server and writes the answers to
stdout:

```bash
rustybin mcp-stdio --server https://<host>/_rustybin/mcp --token <token>
# a visitor on a self-service demo
rustybin mcp-stdio --server https://<host> --header 'X-Rustybin-Mock-Token: rbm_...'
```

`--server` falls back to `RUSTYBIN_MCP_URL` (a bare base URL gets `/_rustybin/mcp`
appended) and `--token` to `RUSTYBIN_TOKEN` or `RUSTYBIN_ADMIN_TOKEN`.

The bridge handles one request at a time and does not relay requests the server
sends while a call runs. Clients on protocol 2026-07-28 get confirmation questions
inside the call's own response, so they work. An older client that declares
elicitation never sees the question through the bridge, and destructive tools fail
after the server's wait: there, leave elicitation out and let the model pass
`"confirm": true` after the user agreed (see [Confirmations](#confirmations)).

On an instance whose control plane asks for credentials you can also leave the
header out and [sign in through the browser](#sign-in-with-oauth).

## Sign in with OAuth

Where the control plane asks for credentials (`RUSTYBIN_CONTROL_AUTH=password`,
the default, `token` or `jwt`), an agent can sign in instead of you pasting a token:

```bash
claude mcp add --transport http rustybin https://<host>/_rustybin/mcp
```

The first call answers `401` with an [RFC 9728](https://www.rfc-editor.org/rfc/rfc9728)
`resource_metadata` pointer. The client reads the metadata, finds Rustybin's own
authorization server at `/_rustybin/oauth`, registers itself and opens your browser
on the console's consent page (`/ui/#/authorize/<id>`). Sign in to the console there
if it asks (with the admin token or a control JWT, as usual), check the agent's name
and where the browser will be sent back to (a warning shows when that is not your own
machine), choose what to grant and approve. The agent gets a token and carries on.

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_challenge}}
```

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_metadata}}
```

**What you can grant.** The consent page offers full access (`admin`), read only
(`console`) and traffic only (`inspector`), the same scopes as [control
JWTs](concepts/control-plane-security.md), but never more than the credential the
console holds: a console signed in with a `console` scope token can let an agent read,
not change anything. An `admin` grant is admin on the management server, like the
admin token there.

**What the token opens.** Only `/_rustybin/mcp` on this instance and host: the token
is bound to that URL (RFC 8707), and every other control-plane route, the data plane's
admin checks and other instances refuse it. Access tokens last an hour; the agent
renews them with a refresh token (30 days) without asking you again, and a refresh can
narrow the scopes, never widen them.

**The key.** Client registrations and tokens are signed with a key, never stored, so
nothing grows on the server. The key comes from `RUSTYBIN_CONTROL_OAUTH_KEY` (32
characters or more), else from `RUSTYBIN_ADMIN_TOKEN`, else it is random per process.
Rotating it (or the admin token it derives from) signs every agent out. With a random
key a restart signs agents out too, and several replicas behind one URL need
`RUSTYBIN_CONTROL_OAUTH_KEY` set to the same value. Single tokens cannot be revoked.

**Clients.** Registration is open ([RFC 7591](https://www.rfc-editor.org/rfc/rfc7591)),
for public clients with PKCE (S256) only. Redirect URIs are `https`, `http` on a
loopback address (any port when the agent signs in, as native apps pick a free one)
or an app's own scheme (`cursor://...`). A failed request goes back to the agent
only when it waits on a loopback address; for any other redirect URI the browser
shows the error, so the server never redirects anywhere on someone else's behalf.

These are the requests an agent makes, step by step. The console's part is the
consent API, called with its credential:

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_register}}
```

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_authorize}}
```

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_consent}}
```

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_token}}
```

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_use}}
```

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_refresh}}
```

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_errors}}
```

**An external issuer.** With `RUSTYBIN_CONTROL_OAUTH_ISSUER=https://...` the metadata
names that authorization server instead and the built-in one answers `404`. The tokens
it issues must be the [control JWTs](concepts/control-plane-security.md) the instance
already accepts (`RUSTYBIN_CONTROL_JWT_PUBLIC_KEY`, `aud` equal to
`RUSTYBIN_CONTROL_JWT_AUDIENCE`): the way for a hosting platform to sign agents in
with its own accounts.

**When it is off.** On an open control plane (nothing asks for credentials, so no
`401` starts the flow), in shared mode (API keys authenticate there), and with
`RUSTYBIN_CONTROL_OAUTH=off`. The metadata documents need no API key even with
`RUSTYBIN_REQUIRE_KEY`, since an agent reads them before it holds anything.
Self-service visitors keep sending their visit's manage token as a header.

## Credentials and what they allow

The agent gets the rights its credential has, no more. `tools/list` shows only the
tools the caller may use; a tool called anyway answers a tool error saying what is
missing, never a protocol error.

| Instance | Credential | Tools |
|---|---|---|
| Open control plane, no admin token | none | All |
| Open control plane with `RUSTYBIN_ADMIN_TOKEN` | none | Reads, tests and traffic; no changes |
| | the admin token (`Authorization: Bearer`, or `X-Rustybin-Admin-Token`) | All |
| `password` control auth (the default) | the token of a sign-in with the console's user name and password (`POST /_rustybin/login`) | All |
| `password` / `token` / `jwt` control auth | no token | `401`, like the rest of the control plane |
| | control JWT, `console` scope | Reads, tests and traffic |
| | control JWT, `inspector` scope | Traffic only |
| | the admin token, or a control JWT with the `admin` scope | All |
| Public mode | none | Reads, tests and traffic (the inspector needs a session); changes are forbidden |
| Self-service demo | none | Reads and tests, plus `create_mock` (the first one starts a visit) |
| | the visit's manage token (`X-Rustybin-Mock-Token`) | The above, changes to the visit's own mocks, `get_visit`, `extend_visit` |
| Shared mode | an API key (`X-Rustybin-Key`) | The key's own mocks, inspector entries and usage |

`get_instance` reports what the caller is (`caller.admin`, `caller.scopes`,
`caller.rights`), so an agent can explain why a tool is missing. On a secured
instance:

```hurl
{{#include ../examples/agent/manage_secured.hurl:secured_no_token}}
```

```hurl
{{#include ../examples/agent/manage_secured.hurl:secured_console_scope}}
```

```hurl
{{#include ../examples/agent/manage_secured.hurl:secured_admin}}
```

## Managing agents

Every client that calls the server is an **agent**: its credential plus the name it
gives in `clientInfo` (sent with `initialize`, or in `_meta` of a 2026-07-28 call).
Two clients on the same token are two agents; a client that sends no name is
`unknown`. The console's **AI agents** view (under AI and agents) lists them with
their credential, first and last seen, calls today and in total, open sessions and
status (`active`, `idle`, `revoked`), and opens one to show its calls per tool with
their outcomes (ok, error, denied, asked to confirm), its last errors and its
sessions. The live feed shows calls as they happen. The same data is at
`GET /_rustybin/agents` (admin):

```hurl
{{#include ../examples/agent/manage_agents.hurl:agents_call}}
```

```hurl
{{#include ../examples/agent/manage_agents.hurl:agents_list}}
```

**Rights.** An agent never gets more than its credential allows; the operator can
give it less. A **ceiling** caps its tools: `read-only` (read tools, no captured
traffic), `traffic` (every read tool), `manage` (reads and mock changes; nothing
instance-wide, no visit operator tools, no harness) or `full` (whatever the
credential allows). An **allow** list narrows it to the tools named; a **deny** list
removes tools. A tool outside its rights is not in its `tools/list`, and calling it
anyway answers a tool error that says the operator limited the agent. Rights come
from, field by field: the agent's own (set in the console or with `PATCH
/_rustybin/agents/{id}`), else the first matching rule of
[`rustybin.yaml`](mocks/config.md)'s `agents:` section, else the default ceiling
(the `agents.default_ceiling` setting, `RUSTYBIN_AGENTS_DEFAULT_CEILING`, `full` by
default).

```hurl
{{#include ../examples/agent/manage_agents.hurl:agents_rights}}
```

```yaml
agents:
  default_ceiling: traffic
  rules:
    - match: { name: "claude-*", credential: oauth }
      ceiling: manage
      deny: [delete_mock, apply_config]
      note: Team assistants sign in with OAuth
    - match: { credential: admin }
      ceiling: full
```

`match.name` is the client's name, case insensitive, with `*` as a wildcard;
`match.credential` is `admin`, `console`, `jwt`, `oauth`, `key`, `visit` or `open`.
A saved or environment default ceiling wins over the file's `default_ceiling`.

**Revoke** refuses every call of the agent (a tool error saying it was revoked), ends
its sessions (their next request answers `403`), and revokes its tokens for their
remaining life: the console sign-in tokens and OAuth access tokens it used, and an
OAuth client's refresh tokens. **Restore** undoes all of it. The admin token cannot be
revoked per agent (rotate `RUSTYBIN_ADMIN_TOKEN`; the agent's calls are refused
meanwhile); API keys are revoked in the key registry. If you gave an agent your own
console token, revoking it signs your console out too. **End sessions** closes the
agent's handshake-era sessions without revoking it.

```hurl
{{#include ../examples/agent/manage_agents.hurl:agents_revoke}}
```

```hurl
{{#include ../examples/agent/manage_agents.hurl:agents_restore}}
```

| Route | |
|---|---|
| `GET /_rustybin/agents` | The agents and the policy (default ceiling, rules) |
| `GET /_rustybin/agents/events` | Server-Sent Events: a `call` per tool call, an `agent` event on revoke and restore (at most 16 listeners) |
| `GET /_rustybin/agents/{id}` | One agent: calls per tool, last errors, sessions, the tools and whether its rights allow each |
| `PATCH /_rustybin/agents/{id}` | `{"rights": {"ceiling", "allow", "deny"} \| null, "note": "..." \| null}` |
| `POST /_rustybin/agents/{id}/revoke`, `/restore` | Revoke or restore |
| `DELETE /_rustybin/agents/{id}/sessions` | End its sessions |

All need admin rights. The registry keeps at most 512 agents (the least recently seen
goes first, agents with rights, a note or a revocation last), 20 errors, 32 sessions
and 16 token ids per agent. With `RUSTYBIN_STATE_DIR`, rights, notes, revocations and
the call totals are kept in `<state>/agents.json`; sessions and errors are not. On an
open control plane anyone may connect under any name, so rights there are advisory:
turn on the console sign-in to control agents for real. **Explain what this agent
did** (with an AI key) summarises an agent's calls, errors and rights.

## Talking to the server

`server/discover` (or `initialize` on the handshake versions) returns the server
`instructions`, which tell the model how to treat captured traffic and confirmations,
and the capabilities: tools, prompts and resources.

```hurl
{{#include ../examples/agent/manage.hurl:discover}}
```

```hurl
{{#include ../examples/agent/manage.hurl:tools_list}}
```

Every result has a one-line text summary plus `structuredContent`, with links the user
can open: the mock's URL, its console page and a `curl` for a sample request.

```hurl
{{#include ../examples/agent/manage.hurl:get_instance}}
```

## Tools

Arguments mirror the REST bodies and [`rustybin.yaml`](mocks/config.md), so what you
learn in one carries over to the other. Rights: **read** (the `console` scope, or
anyone on an open control plane), **traffic** (read, or the `inspector` scope),
**manage** (admin for instance mocks; the owner for visitor and key-owned mocks),
**admin**, **visitor** (a running visit) and **operator** (admin on a self-service
demo).

### Instance

| Tool | Right | |
|---|---|---|
| `get_instance` | read | Version, modes, control auth, limits, the public configuration, the caller's rights, console and docs URLs. Where an agent starts |
| `find_endpoints` | read | Search the [route catalogue](concepts/catalogue.md) by text or category ("anthropic streaming"): path, methods, summary, an example request |
| `get_health` / `set_health` | read / admin | The `/health` demo toggle |
| `get_usage` | read | The plan, its limits and what is left, for the caller (their key in shared mode), so an agent can explain a `429` |
| `get_key_usage` | admin | Per-key daily totals in shared mode, paginated |
| `reset_flaky` | read | Reset the `/flaky` counters of the caller's session between test runs; `scope: "all"` needs admin |

### OpenAPI mocks

| Tool | Right | |
|---|---|---|
| `list_mocks` | read | Name, URL, state, security, calls, owner; filter by text or state |
| `get_mock` | read | Operations with hit counts, settings (secrets redacted unless the caller may change the mock), deploy report, revisions, undeployed draft changes |
| `preview_spec` | read | Parse a spec without creating anything: title, operations, security schemes, warnings, deploy checks |
| `get_mock_spec` | read | The spec as uploaded, to edit and send back with `update_mock`; or with `served: true` the OpenAPI document the mock serves |
| `create_mock` | manage | From spec text or an object (JSON or YAML; Swagger 2.0 is converted) with optional name, responses, security and operations; deployed unless `deploy: false` |
| `update_mock` | manage | Replace the draft's spec or settings, or rename it; `deploy: true` publishes |
| `set_operation` | manage | One operation's response override, [rules](mocks/responses.md), mode and public flag |
| `test_rules` | read | Which rule would answer a captured call, every condition explained; try new rules without saving |
| `check_mock` | read | The deploy report: a valid request and response per operation |
| `deploy_mock` | manage | Publish the draft; `force` only after the user accepted the check errors |
| `rollback_mock` | manage | Make an earlier revision live again (confirmed) |
| `set_mock_enabled` | manage | On or off (off answers `503`) |
| `duplicate_mock` | manage | Copy under a new name |
| `delete_mock` | manage | The mock, its revisions, stored data and traffic (confirmed) |
| `sample_request` | read | A valid request for an operation, as `curl` and as parts, plus the response the mock would send |
| `validate_payload` | read | Check a request or response body against an operation's schema, for example after a gateway transformation |
| `export_collection` | read | The mock as a Postman collection, Hurl file, curl script, `.http` file, k6 script, Bruno collection or Insomnia export, optionally against a gateway's base URL (see [collection exports](exports.md)) |
| `get_mock_data` / `clear_mock_data` | read / manage | What a `stateful` mock stored; clearing is confirmed |
| `get_mock_traffic` / `clear_traffic` | traffic / manage | The calls a mock received: operation, status, validation and auth result, how the response was chosen; clearing is confirmed |
| `export_config` | read | `rustybin.yaml` for one mock or all (secrets redacted unless manageable), to commit as configuration as code |
| `apply_config` | admin | Apply a `rustybin.yaml` document; `dry_run: true` reports what would change; `prune: true` also deletes API-created mocks it does not name (confirmed) |

### Traffic

| Tool | Right | |
|---|---|---|
| `list_requests` / `get_request` | traffic | The [request inspector](concepts/inspector.md), newest first, with filters (path, method, status, session, since); scoped by key in shared mode |
| `wait_for_requests` | traffic | Wait up to `timeout_secs` (default 30, at most 60) for the next captured requests matching a filter (path prefix, method, header, session, or a mock's calls) and return them |
| `clear_requests` | admin | Empty the inspector (or one session's entries) before a test run (confirmed) |
| `create_bin` / `get_bin` | read / traffic | A [request bin](reference/request-bin.md) owned by the caller's session, and what arrived in it |
| `list_push_notifications` | traffic | What the [A2A](a2a/index.md) push notification sink received for a sink id |

`wait_for_requests` is for "I'll send it through the gateway now, tell me what arrived":
the agent starts the wait, you send the call, the answer lists what Rustybin received.
At most 100 waits run at once (20 in public mode); more answer a tool error asking to
retry.

### Testing

| Tool | Right | |
|---|---|---|
| `try_request` | read | Send a request to this instance's own data plane and return status, headers, body and timing |
| `issue_token` | read | A token from the built-in [identity provider](reference/oidc.md) (client credentials of the demo client, with the asked scopes and audience), returned with its decoded claims and expiry |

`try_request` runs **in process** through the full application, with the same
middleware a real client meets (plan limits, fault injection, inspector capture), and
never opens a socket: it cannot reach another host. Control-plane paths (`/_rustybin/*`,
`/ui/*`) are refused, since managing the instance is what the other tools are for. The
request is captured under the session `mcp-<id>` (or the caller's
`X-Rustybin-Session`), so `list_requests` finds it. It carries the caller's API key in
shared mode, never the credential the agent connected with (admin token, control
token or manage token): add headers to the request when the target needs them. It suits MCP clients without a
shell; Claude Code may just use `curl`.

```hurl
{{#include ../examples/agent/manage.hurl:create_mock}}
```

```hurl
{{#include ../examples/agent/manage.hurl:try_request}}
```

```hurl
{{#include ../examples/agent/manage.hurl:control_plane_refused}}
```

A token from `issue_token` opens mocks with JWT security, the OAuth-protected mock MCP
server and a gateway's JWT validation:

```hurl
{{#include ../examples/agent/manage.hurl:issue_token}}
```

### Harness

Only when [load test mode](harness.md) is on (`RUSTYBIN_HARNESS=on`). Rights: **harness**
is the `harness` or `admin` scope, or the admin token on an open control plane.

| Tool | Right | |
|---|---|---|
| `harness_status` | read | Whether harness mode is on, the node's roles, caps, engine, allowed targets, the active run, the console URL. Where an agent starts |
| `list_scenarios` / `get_scenario` / `save_scenario` | read / read / harness | The built-in scenarios to copy, one as an object and YAML, and saving your own by name |
| `list_patterns` | read | The pattern types and their parameters, from the scenario schema |
| `validate_scenario` / `preview_scenario` | read | Problems against the node's caps, the normalised scenario and its summary; the expected requests per second of every layer. Neither sends anything |
| `create_run` | harness | Validate, pin and (when configured) verify the targets and **start sending load** from this node, or submit to a cluster with `cluster_id`. Confirmed (elicitation, else `confirm: true`) |
| `start_run` | harness | Start a submitted cluster run on every client node at once. Confirmed |
| `update_run` | harness | Tune a running run: multiplier, pause, hold, skip, `set`, layers, faults. Confirmed |
| `list_runs` / `get_run` | read | The node's runs, and the live view of one (progress, counts, latency, the last second, saturation, `report_ready`) |
| `stop_run` | harness | Stop a run early (it drains and reports); with `cluster_id`, on every client node |
| `get_run_report` | read | The final report as JSON, text or JUnit |
| `harness_stop` | harness | The kill switch: all load on this node stops now (no confirmation) |
| `create_cluster` / `list_cluster_nodes` / `export_upstreams` / `get_leader` | harness / read / read / read | A cluster led by this node (the join token once), its members, the server nodes' URLs for the gateway's upstream pool, and who leads with the succession and the history of leader changes |

Starting load reaches outside the instance, so `create_run`, `start_run` and `update_run`
carry `openWorldHint` and ask the user to confirm; targets must be in
`RUSTYBIN_HARNESS_TARGETS` whatever the tool. The prompt `load_test_gateway` walks an
agent from the allowlist to a validated scenario, a dry run at a low rate and the real
run, and `rustybin://harness.schema.json` is the scenario schema.

### Self-service

Only on instances with [self-service mocks](mocks/self-service.md).

| Tool | Right | |
|---|---|---|
| `get_visit` | visitor | Time left, hours used today, what the next extension needs |
| `extend_visit` | visitor | One more period without the email step. When an address is needed the answer says so and links the console, where the form shows the consent choice and privacy notice |
| `list_visits`, `end_visit`, `pause_self_service`, `resume_self_service` | operator | The [operator routes](mocks/self-service.md#operators); ending a visit and pausing are confirmed |

A visitor's agent needs no credential for its first mock: `create_mock` starts a visit
and returns its manage token, which the agent then sends as `X-Rustybin-Mock-Token`.
Other people's visitor mocks do not exist for the caller (`404`), exactly as on the
REST routes.

```hurl
{{#include ../examples/agent/manage_self_service.hurl:ss_agent_create}}
```

```hurl
{{#include ../examples/agent/manage_self_service.hurl:ss_agent_token}}
```

```hurl
{{#include ../examples/agent/manage_self_service.hurl:ss_agent_cleanup}}
```

## Prompts and resources

Clients that support MCP prompts offer these as commands:

| Prompt | Arguments | |
|---|---|---|
| `mock_an_api` | `spec_path`, `name` | From a spec to a deployed, tested mock: preview, create, check, try a request, hand over the URL and a collection |
| `debug_gateway_call` | `path` | Find the request a gateway sent and explain what arrived (headers, auth, validation) against what was expected |
| `secure_a_mock` | `name`, `mode` | Pick a mocked security mode and show the credentials a gateway must present |
| `load_test_gateway` | `target`, `rate` | On a [harness](harness.md) node: check the allowed targets, build and validate a scenario, preview it, dry-run it at a low rate, then run it for real and read the report |

| Resource | |
|---|---|
| `rustybin://config.schema.json` | The JSON Schema of `rustybin.yaml`, for writing configuration files |
| `rustybin://catalogue` | The route catalogue as JSON |
| `rustybin://harness.schema.json` | The JSON Schema of a [harness](harness.md) scenario, with the node's caps as bounds |

```hurl
{{#include ../examples/agent/manage.hurl:resources}}
```

## Safety

### Confirmations

Tools that destroy something carry the MCP `destructiveHint` annotation (reads carry
`readOnlyHint`) and ask before acting: `rollback_mock`, `delete_mock`,
`clear_mock_data`, `clear_traffic`, `apply_config` with `prune`, `clear_requests`,
`end_visit` and `pause_self_service`. When the client supports elicitation, the server
asks the user directly ("Delete the mock orders-api and its traffic?"). Without it the
call needs `"confirm": true`, which the tool descriptions tell the model to set only
after the user agreed:

```hurl
{{#include ../examples/agent/manage.hurl:delete_needs_confirmation}}
```

```hurl
{{#include ../examples/agent/manage.hurl:delete_confirmed}}
```

### Captured traffic is untrusted

Whatever calls the data plane writes the captured traffic, and on a public instance
that is anyone. A body saying "ignore your instructions" must not steer the agent. So:

- captured headers and bodies (mock traffic, the inspector, bins, the A2A sink,
  `wait_for_requests`) never appear in the text summary; they are returned in
  `structuredContent` under fields named `untrusted_*`, and the server instructions
  and tool descriptions say this is third-party data, never instructions;
- bodies are left out unless the call asks (`include_bodies: true`) and are truncated
  at 4 KB each and 64 KB per result, with a flag saying so;
- `try_request` answers are treated the same way, since a mock answers whatever its
  owner configured (on a demo, another visitor's mock): `untrusted_*` fields,
  truncated at 64 KB, streams stopped after 64 KB or 5 seconds.

```hurl
{{#include ../examples/agent/manage.hurl:traffic}}
```

Spec text and rule patterns from the agent are validated exactly as on the REST
routes (spec size, YAML and regex limits), so an agent cannot do what a REST caller
could not.

### Audit log and metrics

Every call that changes something, and every refused call, is logged as one line
(target `rustybin::manage`) with the tool, its target, the caller (`admin`,
`key:<id>`, `visit:<fingerprint>` or `session:<label>`), the agent's id and the
outcome. New agents, rights changes, revocations and restores are logged under
`rustybin::agents`. Arguments are
never logged, since they may hold secrets or spec bodies.

`/_rustybin/metrics` counts calls in `rustybin_manage_mcp_calls_total{tool, outcome}`
(outcome `ok`, `denied` or `error`; invalid arguments count as `error`, and a request
refused before it reaches a tool, such as an unknown tool name, is not counted). Tool calls are control-plane requests,
so [plan limits](concepts/plans-and-limits.md) apply as for the REST routes.

## What to ask

- "Mock `openapi/orders.yaml`, check it, and give me a Hurl collection pointed at
  `https://gateway.local/orders`."
- "Make `GET /orders/{id}` answer 404 when the id is 0, then show me that it does."
- "Secure the orders mock with JWT, issue me a token with `orders:read`, and try a
  call with it."
- "I'm sending a request through the gateway now: tell me what headers arrived."
- "Why am I getting 429s?"
- "Export everything as `rustybin.yaml` so I can commit it."

## Not in this version

- Platform operations (creating or deleting instances) and runtime changes to
  `RUSTYBIN_*` settings: those stay in the deployment.
- Fetching a spec from a URL on the server: the agent reads the file and sends its
  content.
- The self-service email step: consent stays in the console.
- OAuth sign-in for self-service visitors and shared-mode key holders (they send their
  manage token or API key as a header), and client ID metadata documents (the server
  would have to fetch a URL).
