# MCP server

Rustybin is a mock [Model Context Protocol](https://modelcontextprotocol.io) server
for MCP gateway demos: routing on MCP headers, authorization (OAuth 2.1 and API keys),
tool filtering, aggregation of several servers, guardrails on tool output, and
observability of agent traffic. Everything it returns is fake, deterministic test
data. It is exercised by the official Python MCP SDK (and optionally the MCP
Inspector CLI) in `conformance/mcp/`.

## Endpoints and variants

| Endpoint | Variant |
|---|---|
| `POST, GET, DELETE /mcp` | Open server, Streamable HTTP transport |
| `POST, GET, DELETE /mcp/protected` | Same server behind OAuth 2.1 bearer tokens (MCP authorization spec) |
| `POST, GET, DELETE /mcp/apikey` | Same server behind an `X-API-Key` header |
| `POST, GET, DELETE /mcp/servers/{name}` | Named servers with tool subsets: `weather`, `crm`, `devtools` |
| `GET /mcp/sse` + `POST /mcp/messages?sessionId=` | Legacy HTTP+SSE transport (2024-11-05) |
| `GET /.well-known/oauth-protected-resource` and `.../oauth-protected-resource/mcp/protected` | RFC 9728 metadata of `/mcp/protected` |

Every Streamable HTTP response carries `X-Rustybin-Mcp-Server`,
`X-Rustybin-Mcp-Session` (`stateless` for 2026-07-28 requests),
`X-Rustybin-Mcp-Method`, `X-Rustybin-Mcp-Request-Id` and
`X-Rustybin-Mcp-Protocol-Version`, handy when checking what a gateway routed where.

## Protocol versions

| Version | Style |
|---|---|
| `2026-07-28` | Stateless: no `initialize`; every request carries `params._meta["io.modelcontextprotocol/protocolVersion"]` and `clientCapabilities`, plus the `MCP-Protocol-Version`, `Mcp-Method` and (for `tools/call`, `prompts/get`, `resources/read`) `Mcp-Name` headers; `server/discover`; list results carry `ttlMs` and `cacheScope`; elicitation and sampling are multi round-trip requests; `subscriptions/listen` |
| `2025-11-25`, `2025-06-18`, `2025-03-26` | Handshake: `initialize` returns `Mcp-Session-Id` (required afterwards), `GET` opens the server-to-client stream, `DELETE` ends the session; `MCP-Protocol-Version` header from 2025-06-18 |
| `2024-11-05` | HTTP+SSE transport at `/mcp/sse`, or `initialize` with this version |

The era is chosen per message: a request with the `_meta` protocol version, or an
`MCP-Protocol-Version` header naming 2026-07-28, is served statelessly; everything
else follows the handshake.

### 2026-07-28 (stateless)

The routing headers must match the body (gateways route on them without parsing
JSON): `MCP-Protocol-Version` equals the `_meta` version, `Mcp-Method` the method,
`Mcp-Name` the tool, prompt or resource, and for tool arguments that a tool marks as
header-routed (such as `get_weather`'s `city`), an `Mcp-Param-<Name>` header.
Mismatches are JSON-RPC error `-32020` with HTTP 400.

```hurl
{{#include ../../examples/mcp/stateless.hurl:discover}}
```

```hurl
{{#include ../../examples/mcp/stateless.hurl:tools_list}}
```

```hurl
{{#include ../../examples/mcp/stateless.hurl:tools_call}}
```

```hurl
{{#include ../../examples/mcp/stateless.hurl:header_mismatch}}
```

```hurl
{{#include ../../examples/mcp/stateless.hurl:resources_prompts}}
```

Responses are plain JSON unless the server has something to stream first (progress
notifications, logs at the requested `_meta` log level): then the answer is an SSE
stream that ends with the result. `?page_size=N` paginates list results (default
100) with `nextCursor`.

```hurl
{{#include ../../examples/mcp/stateless.hurl:progress}}
```

```hurl
{{#include ../../examples/mcp/stateless.hurl:pagination}}
```

### Handshake versions (2025-xx)

```hurl
{{#include ../../examples/mcp/session_and_oauth.hurl:initialize}}
```

```hurl
{{#include ../../examples/mcp/session_and_oauth.hurl:session_call}}
```

```hurl
{{#include ../../examples/mcp/session_and_oauth.hurl:session_end}}
```

A `GET` with the session id opens the server-to-client SSE stream (resource update
notifications after `resources/subscribe`, server requests such as
`elicitation/create` and `sampling/createMessage`). Without a session, `GET` and
`DELETE` answer `400`; an unknown or deleted session answers `404`. Sessions are
bounded (1000, idle TTL 30 minutes; 200 and 10 minutes in public mode).

### Legacy HTTP+SSE (2024-11-05)

`GET /mcp/sse` opens a stream whose first event is `endpoint`, carrying
`/mcp/messages?sessionId=...`. The client POSTs JSON-RPC messages there and gets
`202 Accepted`; responses and notifications arrive as `message` events on the
stream. The session ends with the stream. Because the two halves must run at the
same time, this transport has no Hurl example; `conformance/mcp/test_sse_legacy.py`
drives it with the Python SDK.

## Tools, resources and prompts

| Tool | Purpose |
|---|---|
| `echo`, `add`, `calculate` | Deterministic utilities (`calculate` evaluates `+ - * / % ^` and parentheses) |
| `get_weather`, `get_time` | Fake weather (same city, same answer) and time in a timezone |
| `lookup_customer`, `search_orders` | CRM data, the same customers (`u1` to `u5`) and orders as [GraphQL](../reference/graphql.md) |
| `cancel_order` | A "destructive" tool (nothing changes) that needs the `mcp:tools:write` scope on `/mcp/protected` |
| `slow_task` | Runs `duration_ms` in steps with progress notifications and logs; honours cancellation |
| `fail`, `throw` | A tool error (`isError: true`) and a JSON-RPC error |
| `large_output` | About `kb` kilobytes of text (capped), for response size limits |
| `generate_image` | A small PNG as image content |
| `fetch_resource_link` | A `resource_link` content block (embedded resource before 2025-06-18) |
| `prompt_injection_demo` | Clearly labelled injection text in tool output, for guardrail demos |
| `elicit_confirmation`, `sample_llm` | Elicitation and sampling (multi round-trip on 2026-07-28, server requests before) |
| `inspect_request` | The HTTP request the server received: headers (credentials masked), client IP, token claims, session, transport |

Resources: `rustybin://docs/readme` (Markdown), `rustybin://data/customers.json`,
`rustybin://images/logo.png` (blob) and `rustybin://clock` (subscribable, updates
every `RUSTYBIN_MCP_CLOCK_TICK_SECS`), plus the templates
`rustybin://customers/{id}` and `rustybin://weather/{city}`. Prompts: `summarize`
(`text`, `style`), `code_review` (`code`, `language`, `focus`) and
`incident_report` (`service`, `severity`, `summary`; embeds a runbook resource).
`completion/complete` completes prompt arguments and template variables;
`logging/setLevel` (or the `_meta` log level on 2026-07-28) controls log
notifications.

`inspect_request` is the tool to show identity propagation: call it through the
gateway and the reply lists the headers the gateway injected.

## Named servers

`/mcp/servers/{name}` are separate MCP servers (their own `serverInfo`) for
aggregation and per-server routing demos:

| Server | Tools |
|---|---|
| `weather` | `get_weather`, `get_time`, `inspect_request` |
| `crm` | `lookup_customer`, `search_orders`, `cancel_order`, `inspect_request` |
| `devtools` | `echo`, `add`, `calculate`, `get_time`, `slow_task`, `fail`, `throw`, `large_output`, `generate_image`, `fetch_resource_link`, `prompt_injection_demo`, `elicit_confirmation`, `sample_llm`, `inspect_request` |

Unknown names answer `404` with the list of available servers.

```hurl
{{#include ../../examples/mcp/stateless.hurl:named_server}}
```

## API key variant

`/mcp/apikey` requires `X-API-Key`: any non-empty value, or exactly
`RUSTYBIN_MCP_API_KEY` when that is set. Use it to show a gateway injecting the key
so that agents never see it.

```hurl
{{#include ../../examples/mcp/stateless.hurl:api_key}}
```

## OAuth-protected variant

`/mcp/protected` follows the MCP authorization spec with the built-in
[identity provider](../reference/oidc.md) as authorization server:

1. Without a valid token it answers `401` with
   `WWW-Authenticate: Bearer resource_metadata="<base>/.well-known/oauth-protected-resource/mcp/protected", scope="mcp:tools"`.
2. The RFC 9728 metadata names the resource (`<base>/mcp/protected`, or
   `RUSTYBIN_MCP_RESOURCE_URL`), the authorization server (this Rustybin) and the
   scopes `mcp:tools` and `mcp:tools:write`.
3. The client registers itself (RFC 7591) and gets a token with
   `resource=<base>/mcp/protected` (RFC 8707), typically through authorization code
   + PKCE.
4. Tokens must be RS256 tokens from the built-in provider whose audience is the
   resource URL, or one of `RUSTYBIN_MCP_ACCEPTED_AUDIENCES` (default `rustybin`, so a
   plain client credentials token works; set it to `none` for strict audience
   checks).
5. `cancel_order` needs `mcp:tools:write`; without it the call gets `403` with an
   `insufficient_scope` challenge (step-up authorization).

The base URL comes from the request, like the built-in IdP's issuer: `https` on the
HTTPS listener, the `Host` header, and `Forwarded` / `X-Forwarded-Proto` /
`X-Forwarded-Host` / `X-Forwarded-Port` with `RUSTYBIN_TRUST_FORWARD=true`
(`RUSTYBIN_MCP_RESOURCE_URL` still overrides the resource identifier).

```hurl
{{#include ../../examples/mcp/session_and_oauth.hurl:protected_challenge}}
```

```hurl
{{#include ../../examples/mcp/session_and_oauth.hurl:protected_metadata}}
```

```hurl
{{#include ../../examples/mcp/session_and_oauth.hurl:protected_token}}
```

```hurl
{{#include ../../examples/mcp/session_and_oauth.hurl:protected_step_up}}
```

## Origin validation and limits

Every MCP endpoint checks the `Origin` header against `RUSTYBIN_MCP_ALLOWED_ORIGINS`
(default `*`): a browser request from another origin gets `403` (DNS rebinding
protection, as the transport spec requires). Long-lived streams (GET streams,
HTTP+SSE, `subscriptions/listen`) are closed after an hour (5 minutes in public
mode); request bodies are limited to 4 MiB (256 KiB).

## Conformance

```bash
PYTHON=/path/to/venv/bin/python conformance/mcp/run.sh               # Python SDK scripts
PYTHON=/path/to/venv/bin/python conformance/mcp/run.sh --inspector   # plus the MCP Inspector CLI (Node, npm registry)
```
