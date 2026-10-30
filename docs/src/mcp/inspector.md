# MCP inspector

The console's **MCP inspector** is an MCP client for any server: the servers this
instance serves, a saved connection, or any URL. It speaks every transport, signs in
with every common auth mode, can relay through the instance when the browser cannot
reach a server, and runs a repeatable test suite with a JSON or JUnit report. Point it
at a gateway in front of an MCP server to see exactly what the gateway passes, changes
or refuses.

The view has three columns:

- **Server** (left): what to connect to, the protocol version, browser or relay mode,
  the auth mode, Connect, and the connection state (version, session, mode, latency).
  An admin can save the connection with its credentials sealed on the instance.
- **Protocol** (middle): Tools, Resources, Prompts, Server and Checks tabs.
- **Timeline** (right): every JSON-RPC message sent and received, with the HTTP request
  headers that carried it, and an export of the session as `claude mcp add`, Hurl,
  curl or a Python MCP SDK script. Credentials are masked on screen; Copy includes
  them.

## Targets

**This instance** lists every server from `GET /_rustybin/mcp-servers`: the mock MCP
server at `/mcp` and its variants (`/mcp/protected` behind OAuth, `/mcp/apikey` behind
an API key, the named tool subsets, `/mcp/sse` for the legacy transport), the
[management server](../agent-mcp.md) at `/_rustybin/mcp`, and the
[MCP server of every mock](../mocks/mcp.md) the caller may read, with its state and
the auth it expects. Picking one fills in the auth mode. The MCP tab of a mock links
straight here with its server selected (`#/mcp/server/<path>`).

```hurl
{{#include ../../examples/mcp/inspector.hurl:servers}}
```

**Saved** connections and **Another server** (a URL) complete the picker.

## Transports and versions

| Version | Transport |
|---|---|
| `2026-07-28` | Streamable HTTP, stateless: `server/discover`, the version in `_meta` and `MCP-Protocol-Version`, routing headers (`Mcp-Method`, `Mcp-Name`, `Mcp-Param-*` from a tool's `x-mcp-header`) |
| `2025-11-25`, `2025-06-18`, `2025-03-26` | Streamable HTTP with sessions: `initialize`, `Mcp-Session-Id`, `DELETE` to end |
| `2024-11-05` | Legacy HTTP+SSE: the `endpoint` event, then POSTs to the message URL |

stdio servers are for the CLI (a later release).

## Auth modes

| Mode | Sends |
|---|---|
| None | nothing |
| Bearer token | `Authorization: Bearer <token>` |
| API key | a named header, or a query parameter |
| Basic | `Authorization: Basic` from a user and password |
| Custom headers | any headers you list |
| OAuth 2.1 | a bearer token from the discovery walk below |

**Get a token** walks OAuth discovery as a numbered timeline, each step with the raw
document it fetched:

1. The `401` challenge and its `WWW-Authenticate` header.
2. Protected resource metadata ([RFC 9728](https://www.rfc-editor.org/rfc/rfc9728)).
3. Authorization server metadata ([RFC 8414](https://www.rfc-editor.org/rfc/rfc8414)).
4. The client: dynamic registration ([RFC 7591](https://www.rfc-editor.org/rfc/rfc7591))
   when the server offers it and no client id is given.
5. The grant: authorization code with PKCE in a popup (any issuer; the popup returns to
   `/ui/callback`), or client credentials.
6. The token, with its scope and expiry. A refresh token is kept and **Refresh the
   token** uses it.

Against this instance's `/mcp/protected`, pick client credentials with the built-in
identity provider's client `rustybin` / `secret` (filled in for you). The documents are
fetched by the browser, or through the instance when CORS blocks them:

```hurl
{{#include ../../examples/mcp/inspector.hurl:fetch}}
```

## Browser or relay

**Browser** mode calls the server from the page, which needs CORS from the server.
When the browser cannot reach it (CORS or the network), the inspector offers **Connect
through the relay**: the instance makes the calls and streams the answers back.

`POST /_rustybin/mcp-relay` takes `{target, transport?, headers, message, session?,
connection?}` and answers SSE: a `relay` event (the relay session id, the upstream
status and a safe subset of its headers), a `message` event per JSON-RPC message, a
`body` event for an answer that is not JSON-RPC, then `done`. The relay session (at most
64, idle 10 minutes) keeps the upstream `Mcp-Session-Id` or the legacy stream;
`DELETE /_rustybin/mcp-relay/{session}` ends it, and the upstream session with it.

```hurl
{{#include ../../examples/mcp/inspector.hurl:relay}}
```

Servers on this instance are relayed in process and need nothing. Any other host goes
through the [egress policy](../configuration.md#outbound-connections) under the purpose
`mcp_relay`, which has no default: name the hosts in `RUSTYBIN_EGRESS_ALLOW` (or
Management, Network), and set `RUSTYBIN_EGRESS_ALLOW_LOOPBACK` for a server on the same
machine. A refused host answers `403` with `reason: egress_refused`, and the inspector
links to the Network tab:

```hurl
{{#include ../../examples/mcp/inspector.hurl:relay_refused}}
```

Credentials sent through the relay live only as long as the relay session. They are
never logged (the audit line on the `rustybin::mcp_inspector` target names the origin
only), and the control plane is never captured by the request inspector.

## The protocol tabs

- **Tools**: the list with annotations, a form generated from each `inputSchema`, the
  call with progress notifications, server logs and Cancel, text, image, resource and
  `structuredContent` results, `isError`. Elicitation is answered in a form, also in the
  2026-07-28 multi round trip (`input_required`, then the retry with `inputResponses`).
  With [AI assist](../ai-assist.md) on, **Generate arguments** fills the form with
  arguments valid against the schema, **Explain this tool** describes it, and **Write a
  test for this server** drafts a Hurl test.
- **Resources**: the list, templates, a read, and subscribe on session transports.
- **Prompts**: arguments, the rendered messages, and suggested values when the server
  offers completion.
- **Server**: the server info and capabilities, ping, the log level, End the session.

A server may ask the client to sample a model (`sampling/createMessage`). The inspector
shows the messages and lets you type the reply, answer with your AI provider when assist
is on, or reject the request.

## Checks

The **Checks** tab runs a repeatable suite against the selected server, with the
version and credentials of the Server column (no connection needed first). The instance
runs it: in process for its own servers, through the egress policy for others.

| Check | Passes when |
|---|---|
| `connect` | `initialize` or `server/discover` answers with capabilities |
| `version` | the server agrees on a protocol version |
| `auth_refusal` | with credentials configured, the same request without them is refused |
| `tools_list` | `tools/list` answers and every `inputSchema` is a usable JSON Schema |
| `tools_call` | every tool answers a call with generated arguments (AI when chosen); `isError` is a warning |
| `unknown_tool` | an unknown tool gives a JSON-RPC error |
| `bad_arguments` | a call without its required arguments is refused |
| `unknown_method` | an unknown method gives `-32601` |
| `resources` | `resources/list`, a read and the templates answer |
| `prompts` | `prompts/list` answers and a prompt renders |
| `ping` | `ping` answers |
| `routing_headers` | 2026-07-28: a routing header that contradicts the body is refused |
| `server_stream` | session transports: the server-to-client stream opens or is declined |
| `session` | session transports: `DELETE` ends the session and the old id is refused |

Tools marked destructive are skipped unless you include them, and at most 25 tools are
called by default (`max_tools`, up to 50). A check that does not apply is skipped.

`POST /_rustybin/mcp-checks` runs the suite: `{target, transport?, version?, headers?,
connection?, checks?, include_destructive?, max_tools?, ai_arguments?, save?}`. The report
has a status per check (`pass`, `warn`, `fail`, `skip`) with its message and details,
a summary, and the latency of every call (p50, p95, max, by method). `ok` is true when
nothing failed. Runs are saved (at most 50) unless `save` is false:

```hurl
{{#include ../../examples/mcp/inspector.hurl:checks}}
```

The same report as JUnit, for a CI job (`?format=junit`, or `Accept: application/xml`
when running):

```hurl
{{#include ../../examples/mcp/inspector.hurl:junit}}
```

`GET /_rustybin/mcp-checks` lists the saved runs and `DELETE
/_rustybin/mcp-checks/{id}` (admin) removes one.

## Saved connections

A saved connection holds the target, the transport and version, the mode and the auth
mode. Its credentials (token, API key, password, refresh token, client secret, custom
header values) are sealed with the [settings](../configuration.md) secrets key in
`<state>/mcp-inspector.json` and are returned only as `{set, hint}`. A connection that
holds credentials is used through the relay, which adds them on the instance, so the
browser never sees them again. Saving and deleting need the admin token; at most 50.

```hurl
{{#include ../../examples/mcp/inspector.hurl:connections}}
```

## Who may use it

The relay, the fetch helper and check runs are read-only control-plane calls: an admin
or a console-scope token may use them, and a console token may end its own relay
sessions. Saving or deleting connections and deleting runs need admin.
