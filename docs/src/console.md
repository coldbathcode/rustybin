# Web console

Rustybin ships a browser console at **`/ui`**, built for presenting: it shows what
the upstream received, lets you send traffic directly or through your gateway, and
drives every protocol without extra tools. The console is plain HTML, CSS and
JavaScript modules embedded into the binary at build time (no Node build, no CDN),
so it works offline and on air-gapped demo laptops.

`GET /ui` redirects (`307`) to `/ui/` with a relative `Location`, so the console
also works behind a gateway path prefix. Assets are served with a strong `ETag` and
`Cache-Control: no-cache`, the page with a strict Content Security Policy (scripts
and styles only from the console itself; requests may go to any origin, which is
what "send through the gateway" needs). Like the control plane, `/ui/*` is never
captured by the inspector and ignores fault injection headers.

```hurl
{{#include ../examples/exports/console.hurl:ui_redirect}}
```

```hurl
{{#include ../examples/exports/console.hurl:ui_index}}
```

## Signing in, title and back link

When the instance protects its control plane (`RUSTYBIN_CONTROL_AUTH=token` or
`jwt`), the console's files still load without credentials, but every call to
`/_rustybin/*` carries a token. Open the console with the token in the URL fragment,
`/ui/#token=<token>` (or `/ui/#/traffic&token=<token>`), or paste it on the sign-in
screen. The console keeps it for the browser tab only, strips it from the address
bar, sends it as `Authorization: Bearer` (the live traffic feed included), and
shows the sign-in screen again when the token is refused or its `exp` passes. A
valid token with a narrow scope (`inspector`) signs in too, and the views it may not
read say so. **Console settings, Sign out** forgets it. Details:
[Control-plane security](concepts/control-plane-security.md#the-console-with-a-token).

`RUSTYBIN_CONSOLE_TITLE` (up to 80 characters) is shown as a badge in the header and
in the browser tab title; `RUSTYBIN_CONSOLE_BACKLINK` (an `http://` or `https://`
URL, anything else is ignored with a warning) adds a "Back to ..." link to the
header and to the sign-in screen, for example back to the portal that launched the
console. Both reach the page as HTML-escaped `<meta>` elements, so they are visible
before sign-in.

## Views

The views, in sidebar order (`Alt+1` to `Alt+9` switch to the first nine):

| View | What it does |
|---|---|
| **Overview** | Which instance answers, whether it is healthy, uptime and inspector counters, and the base URLs (HTTP, HTTPS, gRPC, the AI providers, MCP, A2A, OIDC) to paste into a gateway configuration |
| **Live traffic** | The [request inspector](concepts/inspector.md) live over SSE: method, path, headers, body for every request Rustybin received, with filters, highlighting of headers gateways typically add or rewrite, copy as curl, and a compare mode that diffs two requests |
| **Mock APIs** | [OpenAPI mocks](mocks/index.md): create one from a spec, switch mocks on and off, edit responses, rules and security, try operations and watch their traffic (see [below](#mock-apis)) |
| **Request bins** | Create a [request bin](reference/request-bin.md) with a configured response, send a test request, watch captures arrive live |
| **API explorer** | The route catalogue with search and category filters, and a "Try it" request builder prefilled from each endpoint's examples |
| **AI playground** | Chat with the [mock LLM](ai/index.md) in OpenAI chat, OpenAI Responses, Anthropic, Gemini, Ollama (streaming) and Bedrock (non-streaming) wire formats, with modes, fault and latency injection, tool calling with a tool result round trip, the raw wire frames, and the [AI request record](ai/gateway-features.md#request-inspection) of each call |
| **MCP inspector** | An MCP client for 2026-07-28 (stateless) and 2025-11-25 (session): list and call tools with forms generated from their schemas, follow progress, read resources, get prompts, answer elicitation and sampling, a JSON-RPC message log, and the OAuth discovery steps of `/mcp/protected` |
| **A2A client** | Agent directory and cards, blocking or streaming sends over JSON-RPC in v1.0 or v0.3 (the HTTP+JSON binding is not used), a task timeline with artifacts, input-required and auth-required continuation, cancel, and the push notification sink |
| **Chaos and health** | The health toggle, flaky endpoints with live counters, a fault injection header builder, and a load generator (at most 500 requests and 50 in flight) with a status histogram and latency percentiles, for rate limiting, retry and circuit breaker demos |
| **Token lab** | Tokens from the [identity provider](reference/oidc.md) (client credentials, password, authorization code + PKCE in a popup), a JWT decoder with an expiry countdown, introspection, UserInfo and JWKS, an HMAC request signer for `/auth/hmac`, and a Standard Webhooks signer and verifier |

## Mock APIs

The **Mock APIs** view (`/ui/#/mocks`) manages the instance's
[OpenAPI mocks](mocks/index.md):

- **List**: every mock with its URL (copy button, plus the
  [host-based URL](mocks/config.md#host-based-urls) when one is configured), title
  and version, operation count, an on/off switch, a security badge, calls in the
  last hour with the total and the last hit, stored items for stateful mocks, a
  marker for undeployed draft changes, and a "load error" badge for a file mock
  whose last reload failed (the detail page shows the error). Select mocks with the checkboxes to switch
  them on or off or delete them in bulk. With no mocks yet, the page is a drop zone
  for a spec.
- **Create wizard**: drop, browse or paste an OpenAPI 3.0 / 3.1 or Swagger 2.0 file
  (JSON or YAML; Swagger 2.0 is converted); the review step shows the operations,
  security schemes and warnings (`POST /_rustybin/mocks/import`) and a name with the
  resulting URL; the last step picks how responses are produced (spec examples,
  generated data, your own responses, or "Remember changes", the
  [stateful mode](mocks/responses.md)), whether the mock is secured and how
  requests are validated, then creates and deploys it.
- **Header menu**: Rename, Duplicate (a copy under a new name, deployed when the
  source is, with empty traffic), Export YAML, Download spec (as uploaded),
  Download collection (Postman, Hurl, curl, `.http`, k6, Bruno or Insomnia, with an optional base URL
  such as a gateway route), Rollback, Clear traffic and Delete.
- **Operations and the operation editor**: each operation with its hit count and
  last call; per documented status, the response source (spec example, generated
  data, stateful, or a custom body built in a form generated from the schema, with
  a JSON view validated as you type; "Edit as custom body" turns a generated
  preview into a fixed response), response headers, a delay (slider or number), a
  public toggle, and
  [rules](mocks/responses.md#rules) ("when path parameter `id` equals `0`, respond
  `404`"), evaluated in order. Drag rules by their handle (or use the arrows) to
  reorder them. The **Would match** preview runs the unsaved rules against the
  latest captured call of the operation (or one you pick) and shows which rule
  would answer and the value each condition saw.
- **Try it**: a request prefilled from a valid sample (parameters, body, demo
  credentials), sent to the mock, with the response, the response source and the
  validation result. A picker sets the [`Prefer` header](mocks/responses.md) to ask
  for another status or example, and a button fetches a token from the built-in
  identity provider.
- **Security**: the [mocked security](mocks/security.md) switch and mode, the demo
  credentials to copy, a way to get a token from the built-in identity provider,
  and which operations are public.
- **Data** (stateful mocks only): the items the mock stored, per collection, with
  Refresh and Reset.
- **MCP server**: the mock's [MCP server](mocks/mcp.md): its URL, the
  `claude mcp add` command and `mcpServers` JSON, and which operations are tools.
- **Traffic**: the mock's captured calls, live, with the matched operation and its
  path parameters, status, validation and authentication result; calls from the
  mock's MCP server carry an **MCP** badge and the tool name.
- **Deploy report**: the [deploy checks](mocks/responses.md#deploy-checks) per
  operation, deploy, revisions and rollback, and export as `rustybin.yaml`.

Edits go to the mock's draft and reach the live mock when you press **Deploy**
(the button shows how many parts changed, such as the spec, the security settings
or an operation, and the check results; the banner lists them). The **Security** switch is the exception: it
deploys at once, and asks first when other draft changes would go live with it.
Every view has its own link: `#/mocks/<name>/<tab>/<operationId>`.

With a read-only control-plane token (`console` scope) the view shows everything
but the controls that change mocks (bulk actions, Duplicate, Reset, Rename,
Delete and the editors); downloads and the rule preview still work.

### Self-service

On a public instance with [self-service mocks](mocks/self-service.md), visitors
get **New mock** without an admin token:

- The wizard shows the address the mock will get (the name plus a random
  suffix, on the mocks domain when there is one) and leaves out the stateful
  mode. When the server asks for an email address, a short form appears (the
  address, an unticked "send me product news" checkbox and the privacy notice).
- The manage token is kept in the browser's local storage and sent with mock
  management calls only. **Copy manage link** (in the visit bar, the "mock is
  live" banner and the header menu) opens the same mocks in another browser;
  **Rotate manage token** replaces it.
- A **visit bar** above the list shows the time left, the hours used, **Extend**
  (open from 15 minutes before the end) and **End visit**. When the day is used
  or the demo is busy, a panel says when to come back and links to running your
  own Rustybin.
- The list splits **Your mocks** from the instance's mocks, which stay
  read-only; the detail view of your own mock has every editor except Rename.

## Settings

The settings dialog (gear icon) stores, in the browser (the admin token in this tab
only, like the sign-in token; the others across visits):

| Setting | Effect |
|---|---|
| Session | The `X-Rustybin-Session` the console sends: it tags the console's traffic and scopes per-client state (flaky counters, bins, A2A tasks). In [public mode](concepts/sessions.md#public-mode) a random session is generated on first load, because the live traffic view needs one there. |
| Gateway base URL | Views offer to send their requests through this URL instead of directly to Rustybin (the gateway must allow CORS for the console's origin) |
| Admin token | Sent for admin-guarded actions (health toggles, clearing all captured requests) when the instance has `RUSTYBIN_ADMIN_TOKEN`. Without it, the sign-in token is sent instead (useful with the admin token or a JWT with the `admin` scope). |
| Theme | Follow the system, light or dark |

## Control-plane endpoints used by the console

| Route | Returns |
|---|---|
| `GET /_rustybin/status` | Name, version, commit, instance id, hostname, uptime and start time, health, public mode, `admin_token_configured`, `control_auth`, `hosted_mode`, console title and back link, `/identity` request count, inspector counters (stored, capacity, total captured) and ports |
| `GET /_rustybin/catalog` | The route catalogue as JSON: categories with counts, and every endpoint with methods, category, summary, description and runnable examples |

```hurl
{{#include ../examples/exports/console.hurl:status}}
```

```hurl
{{#include ../examples/exports/console.hurl:catalog}}
```

`conformance/ui/run.sh` drives every view in a real Chromium (Playwright), in light
and dark mode, and fails on any browser console error.
