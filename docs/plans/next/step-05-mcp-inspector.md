# Step 05: MCP inspector for any server, every auth mode, test runs

Master plan: [master-plan.md](master-plan.md). Needs: 01, 03. Status: **done 2026-10-30**. About 3 days.

## Goal

The MCP inspector connects to any MCP server: this instance's servers picked from a list (the
`/mcp` variants, the management server, and every deployed mock's MCP server), or any URL. It
speaks every transport the instance serves, authenticates in every mode a real server asks for
(none, bearer, API key header, basic, custom headers, OAuth 2.1 with discovery and PKCE), relays
through the instance when the browser cannot reach the server, exercises the whole protocol, and
runs a repeatable test suite against a server with a report. It is the tool for testing an MCP
gateway in front of anything.

## Read first

- `ui/views/mcp.js` (the `McpClient`, versions MODERN and the 2025 sessions, the `/mcp/protected`
  OAuth walk, the raw wire panel, elicitation and progress handling), `ui/views/authorize.js` and
  `ui/views/tokens.js` (the PKCE popup flow).
- `src/mcp/` (protocol, transports, sessions, the profiles, `src/mcp/openapi.rs` for the per-mock
  servers, `src/mcp/manage/`), `docs/src/mcp/index.md`, `docs/src/mocks/mcp.md`.
- `src/harness/report.rs` (the shape of a report with JSON, text and JUnit) and the inspector's
  need for a similar artefact.
- Step 01's egress policy (`McpRelay` purpose) and settings store (saved connections).
- `conformance/mcp/` (the Python SDK checks: the test runner's checks mirror them).

## Design

### Targets and connections

- **Picker** at the top of the view: "This instance" (every MCP server the instance serves, from a
  new `GET /_rustybin/mcp-servers`: path, kind (`mock-mcp`, `mock-api <name>`, `manage`), protocol
  versions, auth, state), "Saved connections" (settings store, secrets encrypted), "Another server"
  (URL). The current `/mcp` quick checks stay as presets.
- **Connection modes**: `browser` (direct, as today; needs CORS from the server) and `relay`
  (through `POST /_rustybin/mcp-relay`, below). The view tries browser first and offers the relay
  on a CORS or network error, with the egress hint when the relay refuses the host.
- **Transports**: Streamable HTTP 2026-07-28 stateless, Streamable HTTP with sessions
  (2025-11-25 and 2025-06-18), legacy HTTP+SSE (the `/sse` and `/messages` pair). Version
  negotiation shows what the server accepted. stdio servers are for the CLI (step 10).
- **Auth modes** (`ui/lib/mcpauth.js`): none; bearer token; API key in a named header or query;
  basic; custom headers; **OAuth 2.1**: `WWW-Authenticate` and protected resource metadata
  discovery, authorization server metadata, dynamic client registration when offered, PKCE in the
  popup (the authorize view's pattern, generalised to any issuer), token exchange, refresh, the
  token kept with the saved connection. Every step of discovery is shown as a timeline with the
  raw documents, which is what someone testing a gateway's OAuth integration needs.

### The relay (`src/mcp/relay.rs`)

`POST /_rustybin/mcp-relay` (control plane; admin, or the `console` scope): `{target, auth,
message, session?}`. The instance opens the connection with the egress client (`Purpose::McpRelay`,
`RUSTYBIN_EGRESS_ALLOW` must name the host; loopback with the switch), forwards the JSON-RPC
message, streams the answer back as SSE to the browser, and keeps the server's session id in a
bounded map (at most 64 relayed sessions, idle 10 minutes) keyed by a relay session id the browser
holds. Auth credentials for relayed servers travel in the request and are held only for the session
(not logged, not captured; the control plane is outside inspector capture). `DELETE
/_rustybin/mcp-relay/{session}` ends one. The relay refuses targets that are this instance's own
control plane.

### Protocol coverage in the view

Tools (list with schemas, a form generated from `inputSchema` like the scenario builder, "Generate
arguments" with AI, call, structured and text results, `isError`), resources and templates (read,
subscribe where offered), prompts (get with arguments), completion, logging level, progress
notifications, cancellation, elicitation (multi round trip), sampling requests from the server
(answered by the assist provider when a key is set, else declined with the reason shown), pings,
session termination, server-initiated notifications in the timeline. The raw wire panel stays, with
export of the whole session as Hurl, curl, Python MCP SDK and a `claude mcp add` snippet.

### Test runs

"Run checks" on a connected server runs a suite (`ui/lib/mcpchecks.js`, with the server-side relay
when needed) and produces a report like a harness report: `initialize` and capabilities, protocol
version negotiation, `tools/list` schema validity (every `inputSchema` is a valid JSON Schema),
each tool called with generated arguments (AI when enabled, else the schema generator) and judged
(an answer, an `isError` with text, a transport error), unknown tool and bad arguments give the
right JSON-RPC errors, resources read, prompts rendered, session lifecycle, SSE keep-alives, auth
refusal without credentials, latency per call. Checks are selectable; a run is saved (settings
store, at most 50) and exported as JSON or JUnit (`POST /_rustybin/mcp-checks` runs the same suite
server-side through the relay so the CLI and the MCP tools can run it headless).

### The console

The view is rebuilt around three columns: the picker and connection state on the left (status,
version, auth mode, relay or browser, latency), the protocol tabs in the middle, the timeline and
raw wire on the right (collapsible). Sparkle actions: "Generate arguments", "Explain this tool",
"Write a test for this server". The mock APIs view's MCP card links straight into the inspector
with the mock's server selected.

### CLI and MCP notes

Step 10: `rustybin mcp connect <url> --auth ... | tools | call <tool> --args | checks --junit`,
with stdio targets too. Step 11: `list_mcp_servers`, `mcp_call` (through the relay, confirmed when
the target is outside the instance), `run_mcp_checks`.

## Tests

- Unit: the auth mode header builders, the OAuth discovery state machine against recorded
  documents, the relay's session map bounds and refusals, the checks' judgements.
- Full app: the relay against a second in-process Rustybin (its `/mcp`, `/mcp/protected` with
  OAuth, a mock's server) with the loopback switch on; refusal without the switch; a check run
  through the relay produces a passing report for `/mcp` and finds the planted faults of
  `/mcp/variants`.
- Console check: pick a deployed mock's server, call a tool, run the checks, connect to the
  jwt-mode instance's management server with a bearer, the OAuth walk against `/mcp/protected`
  through the generalised flow.

## Docs

`docs/src/mcp/inspector.md` (new: targets, auth modes, the relay and its allowlist, test runs, the
report), `docs/src/console.md`, `configuration.md` (the relay allowlist), Hurl examples for
`/_rustybin/mcp-servers`, the relay and a check run, the changelog.

## Done when

The inspector lists and connects to every server of the instance and to an external server through
the relay under the allowlist, in every auth mode, and a check run produces a JUnit report; all
checks pass; committed and pushed.

## Build notes

Built 2026-10-30. Where the build differs from the design above, and why:

- **One check suite, on the server.** There is no `ui/lib/mcpchecks.js`: the console's Checks
  tab calls `POST /_rustybin/mcp-checks`, the same suite the CLI (step 10) and the management
  MCP (step 11) will call, so the three can never disagree. Checks run without a browser
  connection first. Fourteen checks (`checks::CHECKS`); `isError` from a tool is a warning, not
  a failure (the open `/mcp` has tools that fail on purpose), and `ok` means nothing failed.
- **The relay lives in `src/mcp/inspector/relay.rs`**, beside the client, checks and store,
  not `src/mcp/relay.rs`. Targets on this instance (`/mcp*`, `/_rustybin/mcp`,
  `/mock/<name>/_rustybin/mcp`) are served in process through `state.router`, so they need
  neither the loopback switch nor an allowlist entry; every other host goes through
  `Purpose::McpRelay`. The relay does not proxy a session's server-to-client GET stream
  (server requests that arrive inside a POST's SSE answer are relayed and answered); the
  `server_stream` check covers the GET stream server side. A fetch helper, `POST
  /_rustybin/mcp-relay/fetch`, gets OAuth discovery documents and tokens when CORS blocks the
  browser (same egress rules; this instance's `/.well-known/`, `/oauth/` and
  `/_rustybin/oauth/` in process).
- **Saved connections are not in the settings table.** The settings store has a fixed schema
  of keys, so connections and runs live in `<state>/mcp-inspector.json` (at most 50 each, a
  run at most 256 KiB), their credentials sealed with the settings secrets key
  (`Settings::seal` / `open`) and shown as `{set, hint}`. A connection with credentials is
  always used through the relay, which adds them on the instance. Without a state directory
  they are kept in memory.
- **Planted faults.** `/mcp/variants` does not exist; the faulty-server test stands up a
  small MCP server on its own listener (a tool whose `inputSchema` is not an object, an unknown
  tool that succeeds, arguments never validated, `ping` missing, routing headers ignored),
  runs the checks against it through the relay under the egress policy, and asserts each fault
  is found; without the policy the run stops at `connect`.
- **Two assist tasks**: `tool_arguments` (Generate arguments and the checks' AI arguments,
  validated against the tool's `inputSchema` by its `post` check) and `sampling` (the reply to
  a server's `sampling/createMessage`, offered in the console, never sent without the user).
- **Exports mask credentials on screen**; Copy gives the real command, matching the agent
  panel's behaviour. The timeline's header view masks `Authorization`, cookies and API key
  headers.
- **Access.** The relay, fetch and check runs are read-only POSTs for control auth
  (`control_auth::is_read_only_post`), so a console-scope token may use them; a console token
  may also DELETE its relay sessions. Saving and deleting connections and deleting runs need
  admin.

CLI and MCP notes for later steps: step 10's `rustybin mcp connect | tools | call | checks
--junit` calls `/_rustybin/mcp-servers`, `/_rustybin/mcp-relay` and `/_rustybin/mcp-checks`
on the remote instance (stdio targets run locally through `client::Conn` once it grows a stdio
transport). Step 11's `list_mcp_servers`, `mcp_call` and `run_mcp_checks` wrap the same
handlers (`checks::run`, the relay's `Conn`), with confirmation when the target is outside the
instance.
