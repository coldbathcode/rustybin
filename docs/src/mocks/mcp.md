# An MCP server for every mock

Every deployed mock is also an MCP server for the same API, at
`/mock/<name>/_rustybin/mcp`: one tool per operation, answered by the mock. Upload
the customer's OpenAPI description and you have an MCP upstream that speaks their
domain, for MCP gateway demos (per-tool authorization and rate limits, guardrails
on tool arguments and results, audit, tool filtering), for comparing a gateway's
REST-to-MCP conversion with a native MCP server over identical data, and for
giving an agent a realistic tool set before the real API exists.

```bash
claude mcp add --transport http petstore http://localhost:8080/mock/petstore/_rustybin/mcp
```

With `RUSTYBIN_MOCKS_HOST`, the same server answers at
`<name>.<domain>/_rustybin/mcp`. Self-service mocks have one too, on the mocks
domain only. The console's **MCP server** tab on a mock shows the URL, the
`claude mcp add` command and `mcpServers` JSON, and which operations are tools.

## The server

The endpoint speaks Streamable HTTP with the same protocol versions as the
[mock MCP server](../mcp/index.md): 2026-07-28 (stateless, `server/discover`) and the
2025 handshake versions (`initialize`, `Mcp-Session-Id`). It offers `tools`, one
resource (the OpenAPI document the mock serves, `rustybin://mock/<name>/openapi.json`)
and `logging`. Legacy HTTP+SSE is not offered.

```hurl
{{#include ../../examples/mocks/mcp.hurl:discover}}
```

It is a data-plane route like the rest of `/mock/*`: never behind control-plane
authentication, subject to plan limits, API keys and quotas like any other request,
captured by the request inspector, and open to fault injection. A mock that does not
exist, is not deployed or is switched off, or whose MCP server is disabled, answers
`404` with a JSON-RPC error (for self-service mocks always the same message, so other
visitors' names cannot be probed).

## Operations as tools

```hurl
{{#include ../../examples/mocks/mcp.hurl:list}}
```

| Part of the tool | Comes from |
|---|---|
| `name` | The `operationId` (or the generated id, such as `get_pets_petId`), limited to `A-Z a-z 0-9 _ . -` and 64 characters; a name taken twice gets `_2`, `_3` in spec order |
| `title`, `description` | The summary; the summary, description and `METHOD /path`, at most 1,000 characters. Deprecated operations start with "Deprecated." |
| `inputSchema` | One flat object: path parameters (always required), query, header and cookie parameters under their names, and the request body under `body` |
| `outputSchema` | `{status, body}` with the schema of the default 2xx JSON response, when the operation documents one |
| `annotations` | From the method: `GET`, `HEAD` and `OPTIONS` are read only, `DELETE` is destructive, `GET`, `HEAD`, `OPTIONS`, `PUT` and `DELETE` are idempotent; `openWorldHint` is false |
| `_meta["rustybin.dev/operation"]` | The `operationId`, method and path behind the tool |

Schemas are self-contained: `$ref`s are inlined (to a depth of 8; a recursive
reference becomes an empty schema with a description), OpenAPI 3.0 `nullable`
becomes a `null` type, boolean `exclusiveMinimum` and `exclusiveMaximum` become
numeric, `readOnly` properties are left out of arguments and `writeOnly` properties
out of results. When two parameters share a name (`id` in the path and the query),
the later one is called `<location>_<name>` (`query_id`) and its description says
where it goes.

Some things are never arguments: `Authorization`, `Content-Type`, `Accept`,
`Cookie` and the API key parameters of the spec's security schemes. Credentials come
from the MCP request instead (see [Security](#security)). Operations whose request
body is not JSON (forms, multipart, XML, binary) are not tools in this version; the
mock's detail lists them under `mcp.skipped` with the reason.

## Calling a tool

A call becomes a request to the operation (path parameters substituted and
percent-encoded, the query string, header and cookie arguments, the JSON body) and
runs through the mock exactly like a request over HTTP. Rules, overrides,
examples, generated data, stateful mode, request validation, mocked security,
delays and the restrictions on self-service responses all apply, so a tool returns
what the REST call returns:

```hurl
{{#include ../../examples/mocks/mcp.hurl:call}}
```

- `structuredContent` is `{status, body}` for JSON and empty bodies; the text content
  carries the same JSON. Other bodies are returned as text.
- A status outside 2xx is a tool error (`isError: true`) with the problem body, so a
  model sees the 404 or 422 it would have got over REST:

```hurl
{{#include ../../examples/mocks/mcp.hurl:call_error}}
```

- When the tool declares an output schema and a 2xx body does not match it (a rule
  or override that returns something else), the call is a tool error that names the
  mismatch instead of structured content a client would reject.
- Arguments that are unknown, missing (required path parameters and bodies) or of
  the wrong kind are a tool error before anything is sent. Everything else is the
  mock's request validation (`responses.validate_requests`): with `enforce` the call
  answers the `400` and its errors.
- A slow answer (a configured delay) reports progress every second to a client that
  asked for it; cancelling the request stops the call.

Every call is captured in the mock's traffic, marked with `channel: "mcp"` and the
tool name (the console shows an **MCP** badge):

```hurl
{{#include ../../examples/mocks/mcp.hurl:captured}}
```

## Security

The MCP endpoint itself is open, like the mock's REST endpoints. Mocked security
applies per tool: the MCP request's `Authorization` and `Cookie` headers, the header
of the `api_key` mode and the headers and query parameters of the spec's `apiKey`
schemes are passed into every call. Without a valid credential the call answers the
`401` or `403` the REST call would have:

```hurl
{{#include ../../examples/mocks/mcp.hurl:secured}}
```

So a gateway in front of the MCP server can inject the credential (an API key, a
token exchange) and the demo shows the tool failing without it and working through
the gateway. The console's snippets include the mock's API key header when security
is on. MCP-level OAuth for a mock (the endpoint answering `401` with protected
resource metadata) is not built yet.

## Settings

The MCP server is on for every mock. The `mcp` block of the
[settings](config.md#reference) switches it off or hides operations, in the control
API, `rustybin.yaml`, exports, the console and the management MCP server's
`create_mock` and `update_mock`. Like every setting it goes into the draft and is
served after a deploy.

```yaml
mcp:
  enabled: true            # the default
  exclude: [deletePet]     # operationIds that are not tools
```

```hurl
{{#include ../../examples/mocks/mcp.hurl:exclude}}
```

```hurl
{{#include ../../examples/mocks/mcp.hurl:disable}}
```

`GET /_rustybin/mocks/<name>` describes the server under `mcp`: `url`, `host_url`
(with a mocks domain), `enabled`, `serving` (deployed, switched on and enabled), the
`tools` and the `skipped` operations with their reasons. The served
`openapi.json` names the server in an `x-rustybin-mcp` extension. Deploying a mock
from the [management MCP server](../agent-mcp.md) returns the URL, so an agent can
build a mock and then connect to its tools.

```hurl
{{#include ../../examples/mocks/mcp.hurl:detail}}
```

## Limits

- Tools per mock are bounded by the operations a mock may have (200); `tools/list`
  is paginated like on `/mcp` (`?page_size=`).
- The tool list is computed once per deployed revision and dropped with it; sessions
  of the handshake versions use the bounded session store of `/mcp` and belong to
  one mock.
- A result carries at most the MCP server's large-output cap (1 MB, 256 KB in public
  mode); the text copy of a structured result is cut at 32 KB.
- Tool names, descriptions and results of a self-service mock are visitor-written
  text, served on the mocks domain only. Descriptions are capped and stripped of
  control characters, and results go through the same response restrictions as the
  mock's REST answers.
