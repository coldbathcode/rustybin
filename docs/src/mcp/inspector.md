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

The inspector connects to HTTP servers. For a local agent that only speaks stdio,
use the [`rustybin mcp-stdio` bridge](../agent-mcp.md) to reach the management
server. The bridge does not launch or inspect arbitrary stdio servers.

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
4. The client, in this order: a client id you gave; this instance's [client ID
   metadata document](#the-inspector-as-a-client-with-a-metadata-document) when the
   server supports one and the instance can offer its own; dynamic registration
   ([RFC 7591](https://www.rfc-editor.org/rfc/rfc7591)) when the server offers it;
   else the walk asks for a client id. The step says which was used and why the
   others were not.
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

### The inspector as a client with a metadata document

Current MCP clients do not register with an authorization server: they name
themselves with the `https` URL of a document that describes them, which the server
fetches (a client ID metadata document). A server that takes one says
`client_id_metadata_document_supported` in its metadata, and the inspector tests it
the same way. The instance serves the inspector's document at an open route; it holds
nothing secret, only the name and the console's callback as the one redirect URI:

```hurl
{{#include ../../examples/mcp/inspector.hurl:client_document}}
```

The document's URL is the client id, so the walk makes no registration call and step 4
shows the document. An authorization server has to fetch it from where it runs, so the
walk uses it only when this instance is reached over `https` on a host that is not this
computer, and the console is open at that same address (the redirect URI the walk sends
is the document's, byte for byte). From `http://localhost` step 4 says why not and
registers instead; `client_document` of `GET /_rustybin/mcp-servers` says the same
ahead of time (`usable`, `reason`).

**Client** in the auth form chooses the way:

| Client | What the walk does |
|---|---|
| Automatic | the order above |
| Client ID metadata document | sends the document's URL even where the server does not say it supports one, or cannot reach it: for testing how a server answers |
| Dynamic registration | registers even where a document would do; stops when the server names no registration endpoint |
| The client id below | uses the id you typed; stops without one |

A [saved connection](#saved-connections) keeps the choice (`auth.oauth.client`: `auto`,
`document`, `registration` or `given`) with the other OAuth facts, and never a token it
was not given. The [`client_document` check](#checks) asks the same question without a
person: does the server say yes, and does it then take the document.

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
At most 16 relay exchanges and management `mcp_call`s go at once, and two check runs
(`mcp-checks`, `run_mcp_checks`): past that the answer is `503` (`reason: busy`).

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
| `descriptions_look_clean` | no tool description or schema shows signs of hidden instructions (a warning: see [MCP security](security.md#security-checks-in-the-inspector)) |
| `results_carry_no_instructions` | no answer to the calls shows signs of instructions for the model (a warning) |
| `definitions_are_stable` | `tools/list` after the calls is the same as before them (a warning: a rug pull, or an update) |
| `unknown_tool` | an unknown tool gives a JSON-RPC error |
| `bad_arguments` | a call without its required arguments is refused |
| `unknown_method` | an unknown method gives `-32601` |
| `resources` | `resources/list`, a read and the templates answer |
| `prompts` | `prompts/list` answers and a prompt renders |
| `ping` | `ping` answers |
| `routing_headers` | 2026-07-28: a routing header that contradicts the body is refused |
| `server_stream` | session transports: the server-to-client stream opens or is declined |
| `session` | session transports: `DELETE` ends the session and the old id is refused |
| `client_document` | OAuth: the authorization server says `client_id_metadata_document_supported` and accepts [this instance's document](#the-inspector-as-a-client-with-a-metadata-document) as the client |

Tools marked destructive are skipped unless you include them, and at most 25 tools are
called by default (`max_tools`, up to 50). A check that does not apply is skipped.

`client_document` is the one check that needs no connection: a server behind OAuth
answers `401` until somebody signs in, which is when the question matters. It reads the
protected resource metadata (from the challenge, else the well-known path) and the
authorization server's, and is skipped with the reason when there is no OAuth, when the
server does not say it supports documents, or when this instance cannot offer its own
(reached over plain `http` or on a loopback address). Otherwise it starts one
authorization request with the document's URL as `client_id` and the console's callback,
follows nothing and signs nobody in: a sign-in or consent page (or a redirect to one)
passes, an error fails with the server's status in the details. The server fetches the
document from the address the check was asked at, so run it against the instance's
public URL.

```hurl
{{#include ../../examples/mcp/inspector.hurl:client_document_check}}
```

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

A visitor of a public instance gets the answer and neither keeps a run nor reads the
saved ones, because they are one list:

```hurl
{{#include ../../examples/mocks/self_service.hurl:ss_checks_not_kept}}
```

`GET /_rustybin/mcp-checks` lists the saved runs and `DELETE
/_rustybin/mcp-checks/{id}` (admin) removes one. A report, an answer and an audit line
show the target without its query string, which may hold a saved connection's API key
(a run saved before 0.3.1 is stripped when read).

## Saved connections

A saved connection holds the target, the transport and version, the mode and the auth
mode (for OAuth also the facts of the walk: issuer, token endpoint, client id, scope and
`client`, [how the walk names its client](#the-inspector-as-a-client-with-a-metadata-document)).
Its credentials (token, API key, password, refresh token, client secret, custom
header values) are sealed with the [settings](../configuration.md) secrets key in
`<state>/mcp-inspector.json` and are returned only as `{set, hint}`. A connection that
holds credentials is used through the relay, which adds them on the instance, so the
browser never sees them again. Saving and deleting need the admin token; at most 50.

**The target rule.** A saved connection's credentials go only to its own target. With
`connection`, a `target` that names another origin (scheme, host and port), or another
path on this instance, is refused with `400` (`connection_target_mismatch`) by the
relay, the check runs and the management tools alike; leave the target out to use the
connection's. Using a connection that holds credentials needs admin rights (`403`,
`connection_needs_admin`): a console-scope token or a read-only agent may use
connections without secrets only. A legacy server's `endpoint` event must stay on the
stream's origin, so the credentials never follow it to another host. The management
server (`/_rustybin/mcp`) is never an inspector target: the console connects to it
from the browser with its own token, and an agent cannot call it through `mcp_call`
to slip past its own limits. Captures of the inspector's in-process requests redact
every header and query value that looks like a credential.

```hurl
{{#include ../../examples/mcp/inspector.hurl:connections}}
```

## Who may use it

The relay, the fetch helper and check runs are read-only control-plane calls: an admin
or a console-scope token may use them, and a console token may end its own relay
sessions. Without admin rights a server on another host only sees read-only methods
(`initialize`, `server/discover`, `ping`, `tools/list`, `resources/list`,
`resources/read`, `resources/templates/list`, `prompts/list`, `prompts/get`) and
notifications: the relay answers `403` with
`reason: admin_required` to anything else, a check run skips `tools_call` and
`bad_arguments`, and the `mcp_call` tool refuses. The servers of this instance take any
method. Saving or deleting connections, deleting runs and using a saved connection
that holds credentials need admin.

## On every surface

The same job from the API, the console, the command line, an agent and the configuration document.

| Surface | Where |
|---|---|
| API | `GET /_rustybin/mcp-servers` (with `client_document`), `GET /_rustybin/oauth/inspector-client.json` (open), `POST /_rustybin/mcp-relay` and `DELETE /_rustybin/mcp-relay/{session}`, `POST /_rustybin/mcp-relay/fetch`, `POST`, `GET /_rustybin/mcp-checks`, `GET`, `DELETE /_rustybin/mcp-checks/{id}`, `GET /_rustybin/mcp-connections`, `GET`, `PUT`, `DELETE /_rustybin/mcp-connections/{id}` |
| Console | The **MCP inspector** view (servers, connections, the protocol tabs, the timeline, checks; **Client** in the OAuth form chooses a metadata document, registration or a given id); a mock's **MCP** tab opens it on the mock's server |
| CLI | `rustybin mcp servers \| tools <target> \| call <target> <method> \| check <target> [--junit] \| runs \| run <id> \| delete-run <id> \| connections \| connection <id> \| save-connection [--oauth-client auto\|document\|registration\|given] \| delete-connection <id> \| fetch \| end-relay \| client-document` ([reference](../cli.md#mcp)) |
| Management MCP | `list_mcp_servers` (with `client_document`: the document, its URL, whether a server can use it and why not), `mcp_call` (answers under `untrusted_result`), `run_mcp_checks` (the `client_document` check included) and `list_mcp_check_runs` (reports under `untrusted_report`), `list_mcp_connections` (each with its `auth.oauth.client`); the prompt `test_mcp_server` ([agent page](../agent-mcp.md)) |
| `rustybin.yaml` | Nothing: saved connections hold credentials and stay in `<state>/mcp-inspector.json`; the servers a document creates (mocks with MCP on) appear in the server list |
| Environment | `RUSTYBIN_EGRESS_ALLOW` and `RUSTYBIN_EGRESS_ALLOW_LOOPBACK` for the relay's destinations, `RUSTYBIN_STATE_DIR` for saved connections and runs ([configuration](../configuration.md#outbound-connections)) |
