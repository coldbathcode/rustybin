# Plan: an MCP server for every OpenAPI mock

Status: **built** (phases 1 to 4, 2026-10-24). Section 13 lists where the build differs from this plan; user
docs in `docs/src/mocks/mcp.md`.

## 1. Goal

Upload an OpenAPI spec, deploy the mock, and get an MCP server for that API as well as the REST endpoints:
one tool per operation, answered by the same mock engine.

```bash
claude mcp add --transport http petstore https://<host>/mock/petstore/_rustybin/mcp
# or on the mocks domain
claude mcp add --transport http petstore https://petstore.mock.example.com/_rustybin/mcp
```

Today a mock is a REST API only. The mock MCP server at `/mcp` serves a fixed set of fake tools
(`get_weather`, `lookup_customer`, ...), and the management server at `/_rustybin/mcp` configures mocks but
does not present a mock's operations as tools. A gateway that converts REST to MCP can already use a mock
as its upstream (spec at `/mock/{name}/_rustybin/openapi.json`); this plan covers the other half, an MCP
upstream that speaks the customer's own domain.

### What it is for

1. **MCP gateway demos with the customer's API**: per-tool authorization and rate limits, guardrails on
   tool arguments and results, audit, tool filtering, all against tools named after the customer's own
   operations instead of a weather demo.
2. **Comparing the two routes to MCP**: the gateway converting the REST mock, next to the mock's native
   MCP server, with identical data behind both.
3. **Agent development**: give an agent a realistic, deterministic tool set before the real API or its MCP
   server exists; rules, examples and stateful mode shape what the tools return.

## 2. How it fits the existing code

- **Transport and protocol**: reuse `src/mcp/` (Streamable HTTP, every protocol version `/mcp` speaks,
  sessions, `_meta` envelopes, pagination). A third profile family, `Family::Mock` and `Family::Manage`
  being the others: `Family::OpenApi`, whose profile names the mock (`key` = `mock:<name>`). `core.rs`
  already branches on the family for server info, capabilities, instructions and dispatch; the new family
  gets its own branch, like `manage`.
- **Tools are data, not code**: `tools.rs` and `manage/tools.rs` hold static `ToolDef`s. The new family
  builds its tools from the live `Compiled` revision. A revision is immutable, so the tool list is
  computed once per revision (a `OnceLock` on `Compiled`) and dropped with it: no new state to bound.
- **Calls go through the mock engine**: a tool call becomes a request to the operation and runs through
  `mocks::handlers::serve` (lookup, on/off, path matching, request validation, mocked security, rules,
  overrides, examples, stateful data, delays, visitor response restrictions, capture). Nothing about
  responses is reimplemented, so REST and MCP answers can never drift apart.
- **Routing**: `/mock/{name}/_rustybin/mcp` is below the reserved `/_rustybin` sub-path that already
  serves `openapi.json` and `openapi.yaml` (`is_reserved_sub_path`), so it can never collide with an
  operation of the mock. The host layer (`host.rs`) rewrites `<name>.<domain>/_rustybin/mcp` to it before
  routing, so it works on `RUSTYBIN_MOCKS_HOST` and the self-service mocks domain without changes. It is a
  data-plane route like the rest of `/mock/*`: never behind control auth, subject to plan limits and keys.

## 3. Endpoint

| Route | |
|---|---|
| `POST`, `GET`, `DELETE /mock/{name}/_rustybin/mcp` | Streamable HTTP, protocol versions as `/mcp` (2025-03-26 to 2026-07-28) |

- Server name `rustybin-mock-<name>`, title the spec's `info.title`, instructions from `info.description`
  (truncated) plus one line saying the data is mocked.
- Capabilities: `tools`, `logging`, and `resources` with one resource, the served spec
  (`openapi.json`), so a client can read what the tools are built from.
- Not deployed, switched off, or MCP disabled for the mock (section 7): `404` with a JSON-RPC style error
  body, the same answer whatever the reason is for visitor mocks (no probing of other people's names).
- Legacy HTTP+SSE (2024-11-05) is not offered.

## 4. Operations as tools

### Names

- The `operationId`, else the synthesised id the spec parser already makes (`get_pets_petId`).
- Sanitised to `[A-Za-z0-9_.-]`, at most 64 characters (the common client limit, inside the 128 the
  2025-11-25 spec allows). Collisions after sanitising get `_2`, `_3` in spec order.
- Order: spec order, which `serde_json` `preserve_order` keeps.

### Descriptions and annotations

- `title` = the operation's `summary`; `description` = summary, description and `METHOD /path`, capped at
  1,000 characters with control characters removed.
- Annotations from the method: `GET`/`HEAD` `readOnlyHint`; `DELETE` `destructiveHint`; `GET`, `HEAD`,
  `PUT`, `DELETE` `idempotentHint`; `openWorldHint` false. Deprecated operations stay listed with
  "Deprecated." at the start of the description.

### Input schema

One flat object, because models handle flat arguments best:

- path parameters (always required), query parameters, header parameters, each under its own name with
  its schema and description;
- the request body under `body` (required when `requestBody.required`), JSON bodies only in this version;
- a name that appears in two locations (`id` in path and query) becomes `<location>_<name>` for the later
  one, and the description says where it goes;
- `Authorization`, `Content-Type`, `Accept` and the headers of mocked security schemes are not arguments:
  credentials come from the MCP request (section 6).

Schemas are inlined: the parser resolves `$ref` on demand (`schema::deref`), but an MCP client needs a
self-contained schema. A new `schema::inline` follows references to a depth of 8 and replaces a recursive
reference with `{}` plus a description; the result is capped in size like other mock limits.

### Output

- `structuredContent` `{ "status": 200, "body": <JSON> }`, with an `outputSchema` built from the default
  2xx response schema when the operation documents one.
- Text content: the response body (JSON pretty-printed), truncated at the mock's response preview cap with
  a note saying so.
- A response outside 2xx is a tool result with `isError: true`, the status and the problem body, so models
  see the 404 or 422 they would have got over REST.
- A response with a non-JSON body is returned as text (images and binary as a short description, no blob).

### Skipped operations

Operations whose request body is not JSON (multipart, form, XML, binary) are not tools in this version.
They are listed under `skipped` with the reason in the mock's detail (section 8) and in the deploy report,
never silently dropped.

## 5. Calling a tool

1. Find the operation for the tool name in the live revision; an unknown name is a JSON-RPC error.
2. Validate the arguments against the input schema (the validator the mock already uses for requests),
   answering a tool error that lists the problems.
3. Build the request: method, path with path parameters substituted and percent-encoded, query string,
   header arguments, JSON body, plus the forwarded headers of section 6.
4. Call `serve` in process with an extension marking the channel, so captures show
   `channel: "mcp"` and the tool name next to the operation.
5. Read the response body up to the cap and map it to a result (section 4).

Progress and cancellation: a rule delay longer than one second sends progress notifications when the
client asked for them; a cancelled request drops the in-process call.

## 6. Security

The MCP endpoint itself is open, like the mock's REST endpoints. Mocked security applies per tool:

- The MCP request's `Authorization` header, the header and query names of the mock's `apiKey` schemes,
  and their cookies are forwarded into each in-process request.
- An operation that requires security and gets no valid credential answers a tool error carrying the 401
  or 403 the REST call would have returned, including the hint to get a token from the built-in identity
  provider.

So a gateway in front can inject the credential (API key or token exchange) and the demo shows the tool
failing without it and working through the gateway.

Not in this version: MCP-level OAuth for a mock (a `401` from the MCP endpoint itself with RFC 9728
metadata pointing at the mocked identity provider). See section 12.

## 7. Settings

In `MockSettings` (control API, `rustybin.yaml`, exports, console, management MCP):

```yaml
mcp:
  enabled: true            # default
  exclude: [deletePet]     # operationIds that are not tools
```

On by default: a deployed mock is already public at its URL, and the MCP endpoint exposes nothing the REST
endpoints do not. Visitor mocks get it too, with the same restrictions (section 9).

## 8. Surfaces

- **Control API**: `GET /_rustybin/mocks/{name}` gains
  `mcp: { url, host_url, enabled, tools: [{ name, operation }], skipped: [{ operation, reason }] }`.
- **Console**: an "MCP server" section on the mock's page: the URL (path and host based), copy buttons for
  `claude mcp add` and `mcpServers` JSON (the `ui/lib/agent.js` pattern), the tool list with skipped
  operations, and the on/off switch. Live traffic marks MCP calls with an `MCP` badge and the tool name.
- **Management MCP**: `get_mock`, `create_mock` and `deploy_mock` results include `mcp_url`, so an agent can
  build a mock and then connect to its tools. No new management tools.
- **Catalogue and OpenAPI**: `/mock/{name}/_rustybin/mcp` with an example against `petstore`
  (`initialize` on 2025-06-18) in `src/mocks/mod.rs`'s catalogue and paths; README tables regenerated.
- **Served spec**: an `x-rustybin-mcp` extension at the top of the served `openapi.json` with the URL.

## 9. Limits and safety

- No new unbounded state: tool lists live on the immutable revision; sessions use the mock MCP's bounded
  session store (`key` = `mock:<name>`, existing capacity and idle TTL); a deleted mock's sessions expire
  with the TTL and its calls answer `404`.
- Tools per mock are bounded by `MockLimits::max_operations` (200); `tools/list` is paginated with the
  existing page size.
- The MCP request is itself a data-plane request, so it already passes the key layer, plan limits, quotas
  and rate limits; `serve` is called directly, below that middleware, so a tool call counts once, not
  twice. The in-process request carries the caller's `KeyContext` so captures and usage stay attributed.
- **Visitor mocks**: tool names, descriptions and results are visitor-controlled text sent to whoever's
  agent connects. That is the same trust position as any MCP server on the internet and the reason visitor
  mocks live on their own domain. Descriptions are capped and stripped of control characters; result text
  goes through the visitor response restrictions (`restrict.rs`) because it is produced by `serve`.
  Nothing from a visitor mock reaches the management server or another visitor's mocks.

## 10. Phases

| Phase | Scope | Done when |
|---|---|---|
| 1. Tools from a spec | `schema::inline`, tool generation from `Compiled` (names, collisions, schemas, annotations, skipped operations), cached per revision | Unit tests on petstore and fixtures: names and collisions, `$ref` and recursive schemas, parameter name clashes, skipped bodies, deprecated |
| 2. Endpoint | `Family::OpenApi` profile, route below the reserved sub-path, `tools/list`, `tools/call` through `serve` with the channel marker, captures, settings `mcp.enabled` / `exclude`, the spec resource | Tests through the full app: initialize and modern stateless requests, list, call, 404 and validation as tool errors, mocked security with and without credentials, stateful create then read, rules and delays, disabled, host-based URL, visitor mock on the mocks domain |
| 3. Surfaces | Control API field, console section and traffic badge, management MCP `mcp_url`, catalogue, OpenAPI, served spec extension | Catalogue and OpenAPI consistency tests; browser check of the console section (`conformance/ui/`) |
| 4. Conformance and docs | Python MCP SDK client against `/mock/petstore/_rustybin/mcp`; `docs/src/mocks/mcp.md`, Hurl examples, gateway recipe, README, changelog, roadmap | `conformance/mcp/run.sh`, `docs/examples/run.sh`, `check_includes.py` pass |

## 11. Testing

- Unit tests next to the generator (`src/mocks/mcp/tests.rs` or `src/mcp/openapi/tests.rs`, wherever the
  module lands) for everything in phase 1.
- Full-app tests for phase 2 with `test_support`, reusing the petstore sample and small fixture specs.
- `conformance/mcp/test_mock_mcp.py`: the official SDK lists the petstore tools, calls a read and a write,
  sees a tool error for a missing pet, reads the spec resource.
- The console check gains the MCP section and the size guard on its icons.

## 12. Open questions

1. **MCP-level OAuth for mocks** (later): answering `401` with RFC 9728 metadata so MCP clients run the
   OAuth flow against the mocked identity provider. Valuable for "MCP auth through a gateway" demos; left
   out of v1 because per-tool mocked security already covers the common gateway setups.
2. **Non-JSON request bodies**: form and multipart as tool arguments (base64 for files) if demos need them.
3. **Grouped arguments** (`path`, `query`, `body` objects) as an opt-in setting if flat names confuse
   specific clients.
4. **Prompts**: one prompt per tag ("explore the Pets API") is cheap to add if it helps demos.

## 13. Where the build differs

- **Modules**: the generator is `src/mocks/mcp.rs` (tests in `src/mocks/mcp/tests.rs`), the endpoint and the
  `Family::OpenApi` dispatch `src/mcp/openapi.rs` (tests in `src/mcp/openapi/tests.rs`). The route is registered
  with the mock MCP router so it shares its `Shared` (session store, limits), so its catalogue and OpenAPI entries
  live in `src/mcp/mod.rs`, in the Mock APIs category. `Profile` gained `mock: Option<Arc<MockTarget>>`, built per
  request from the live revision; the static demo tools, resources and prompts are gated on the family.
- **Argument validation**: no separate validation against the input schema. A call is refused before anything is
  sent only for unknown arguments, missing required path parameters or body, and values of the wrong kind (an
  object for a path parameter, a header value that is not valid). Everything else is the mock's own request
  validation (`responses.validate_requests`), so `off`, `warn` and `enforce` behave as over REST.
- **Output**: the text content is the serialised `structuredContent` (`{status, body}`), as the MCP specification
  recommends for structured results, cut at 32 KB when structured content is sent and at the MCP server's
  large-output cap (1 MB, 256 KB public) otherwise. When a tool declares an output schema and a 2xx body does not
  match it (a rule or override), the result is a tool error naming the mismatch rather than structured content the
  client would reject (the Python SDK validates every non-error result). Error results also carry
  `structuredContent`.
- **Tool `_meta`**: each tool carries `_meta["rustybin.dev/operation"]` `{operationId, method, path}`.
- **Progress**: a call that has not answered after a second sends `notifications/progress` every second (when the
  client gave a progress token), whatever the cause of the wait.
- **404 answers**: instance mocks get the reason (not deployed, switched off, `mcp.enabled` false); visitor mocks
  and unknown names get one shared message.
- **Visitor mocks**: the restrictions apply to tool results (they come from `serve`); the MCP transport's own
  responses are not passed through `restrict::harden`, which would rewrite `text/event-stream`.
- **Surfaces**: the detail field also has `serving` (deployed, on, MCP enabled); `deploy` answers `mcp_url`
  (null when disabled); draft changes list `mcp`. The console tab is called "MCP server" and lets the user untick
  operations (saved as `mcp.exclude`).

