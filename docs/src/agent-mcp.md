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

The [web console](console.md) fills both in for you: "Connect an AI agent" in the
settings dialog and in the Mock APIs view shows the command with this instance's URL
and the credential the console already holds (the control token, the admin token or a
visit's manage token). The token stays masked until you ask to see it; the copy
buttons copy the real value.

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

## Credentials and what they allow

The agent gets the rights its credential has, no more. `tools/list` shows only the
tools the caller may use; a tool called anyway answers a tool error saying what is
missing, never a protocol error.

| Instance | Credential | Tools |
|---|---|---|
| Open control plane, no admin token | none | All |
| Open control plane with `RUSTYBIN_ADMIN_TOKEN` | none | Reads, tests and traffic; no changes |
| | the admin token (`Authorization: Bearer`, or `X-Rustybin-Admin-Token`) | All |
| `token` / `jwt` control auth | no token | `401`, like the rest of the control plane |
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
| `export_collection` | read | The mock as a Postman collection, Hurl file, curl script, `.http` file, k6 script, Bruno or Insomnia export, optionally against a gateway's base URL (see [collection exports](exports.md)) |
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
`X-Rustybin-Session`), so `list_requests` finds it. It suits MCP clients without a
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

| Resource | |
|---|---|
| `rustybin://config.schema.json` | The JSON Schema of `rustybin.yaml`, for writing configuration files |
| `rustybin://catalogue` | The route catalogue as JSON |

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
`key:<id>`, `visit:<fingerprint>` or `session:<label>`) and the outcome. Arguments are
never logged, since they may hold secrets or spec bodies.

`/_rustybin/metrics` counts calls in `rustybin_manage_mcp_calls_total{tool, outcome}`
(outcome `ok`, `denied`, `invalid` or `error`). Tool calls are control-plane requests,
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
- OAuth sign-in to the management server (RFC 9728 metadata): use a bearer header.
