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

The console's files load without credentials, but every call to `/_rustybin/*`
carries a token. By default (`RUSTYBIN_CONTROL_AUTH=password`) the sign-in screen
asks for the console's user name and password, `rustybin` / `rustybin` until
changed, and the instance answers a 12 hour control token; **Remember me** keeps it
in the browser until it expires. A banner reminds you to change the default
password, and the top bar shows who is signed in and when the session expires. With
`token` or `jwt` control auth the screen asks for a token instead (in password mode
that form is folded under "Use a token instead"); a token also comes in the URL
fragment, `/ui/#token=<token>` (or `/ui/#/traffic&token=<token>`). The console
keeps the token for the browser tab, strips it from the address bar, sends it as
`Authorization: Bearer` (the live traffic feed included), and shows the sign-in
screen again when the token is refused, revoked or its `exp` passes. A valid token
with a narrow scope (`inspector`) signs in too, and the views it may not read say
so. **Management, General, Sign out** revokes a sign-in token and forgets it.
Details: [Control-plane security](concepts/control-plane-security.md#user-name-and-password).

```hurl
{{#include ../examples/concepts/login.hurl:login}}
```

`RUSTYBIN_CONSOLE_TITLE` (up to 80 characters) is shown as a badge in the header and
in the browser tab title; `RUSTYBIN_CONSOLE_BACKLINK` (an `http://` or `https://`
URL, anything else is ignored with a warning) adds a "Back to ..." link to the
header and to the sign-in screen, for example back to the portal that launched the
console. Both reach the page as HTML-escaped `<meta>` elements, so they are visible
before sign-in. Both are [instance settings](configuration.md#state-directory-settings-and-secrets)
(`console.title`, `console.backlink`): a value saved under Management, General wins
over the variable, and `/_rustybin/status` reports the effective title:

```hurl
{{#include ../examples/concepts/settings.hurl:title}}
```

## The assist bar

With an AI provider and key set ([Bring your own AI](ai-assist.md)), the top bar
carries **Ask Rustybin** (`Ctrl+K` / `Cmd+K`): suggestions for the view you are on,
answers streamed into a drawer on the right with the provider, the tokens, the
estimated cost and "What was sent", and apply cards that open a view, copy text or
send a request when you click them. Views with AI actions carry a sparkle in the
navigation and sparkle buttons where the work is (**Explain** on a captured request).
Without a key the bar and the buttons point at **Management, AI**; the built-in mock
provider (`RUSTYBIN_ASSIST_PROVIDER=mock`) shows the whole shell without one.

## Views

The views, in sidebar order (`Alt+1` to `Alt+9` switch to the first nine):

| View | What it does |
|---|---|
| **Overview** | Which instance answers, whether it is healthy, uptime and inspector counters, an **Ask Rustybin** card with three suggestions when [AI assist](ai-assist.md) is on, and the base URLs (HTTP, HTTPS, gRPC, the AI providers, MCP, A2A, OIDC) to paste into a gateway configuration |
| **Live traffic** | The [request inspector](concepts/inspector.md) live over SSE: method, path, headers, body for every request Rustybin received, with filters, highlighting of headers gateways typically add or rewrite, copy as curl, and a compare mode that diffs two requests |
| **Mock APIs** | [OpenAPI mocks](mocks/index.md): create one from a spec or [describe it to the AI](mocks/ai.md), switch mocks on and off, edit responses, generate missing examples, edit the spec with an instruction, rules and security, try operations and watch their traffic (see [below](#mock-apis)) |
| **Request bins** | Create a [request bin](reference/request-bin.md) with a configured response, send a test request, watch captures arrive live |
| **API explorer** | The route catalogue with search and category filters, and a "Try it" request builder prefilled from each endpoint's examples |
| **Management** | The instance's settings and this browser's, in tabs (see [below](#management)): General (the console title and back link, the session, gateway URL, admin token and theme of this browser), Sign-in (the console user name and password, the sessions), AI, Network (what the instance may connect to, each feature's rule, the last decisions, the leads webhook token), Portal (the [dev portal](portal.md): branding, access, published items, insights), Export ([the whole instance as `rustybin.yaml`](configuration-as-code.md): copy, download, the bundle, apply a document with a dry run) and Source control ([a git repository](source-control.md): connect, commit and push, pull and apply, the deploy key, the history). The gear in the top bar opens it |
| **AI playground** | Chat with the [mock LLM](ai/index.md) in OpenAI chat, OpenAI Responses, Anthropic, Gemini, Ollama (streaming) and Bedrock (non-streaming) wire formats, with modes, fault and latency injection, tool calling with a tool result round trip, the raw wire frames, and the [AI request record](ai/gateway-features.md#request-inspection) of each call |
| **MCP inspector** | An [MCP client for any server](mcp/inspector.md): this instance's servers (`/mcp` and its variants, the management server, every mock's server), saved connections or any URL; every transport (2026-07-28, the 2025 sessions, legacy HTTP+SSE) and auth mode (bearer, API key, basic, headers, OAuth 2.1 discovery with PKCE or client credentials); from the browser or through the instance's relay; tools with generated forms, progress, cancel and elicitation, resources, prompts, sampling; a timeline with exports (`claude mcp add`, Hurl, curl, Python); **Checks**, a test suite with a JSON or JUnit report |
| **AI agents** | The agents that manage this instance through its [management MCP server](agent-mcp.md#managing-agents): name and version, credential, first and last seen, calls today, ceiling and status; per agent its calls per tool with their outcomes, last errors and sessions, the rights form (a ceiling, tool allow or deny chips, a note), **Revoke** and **Restore**, **End sessions**, and **Explain what this agent did** (AI); the live feed of calls; the default ceiling and the rules of `rustybin.yaml`; and **Connect an agent**, the `claude mcp add` command and JSON with the credential the console holds |
| **MCP registry** | The [mock MCP registry](mcp/registry.md) at `/mcp-registry`: the URL and a sync snippet to point a gateway at, the scenario picker, the entries with their source and validity, a publish form that validates as you type, **Write a server.json for my mock** (AI); and **Browse a registry**: the public registry, this instance's or any URL, with **Validate**, **Explain** and **Open in inspector** per remote |
| **A2A client** | Agent directory and cards, blocking or streaming sends over JSON-RPC in v1.0 or v0.3 (the HTTP+JSON binding is not used), a task timeline with artifacts, input-required and auth-required continuation, cancel, and the push notification sink |
| **Chaos and health** | The health toggle, flaky endpoints with live counters, a fault injection header builder, and a load generator (at most 500 requests and 50 in flight) with a status histogram and latency percentiles, for rate limiting, retry and circuit breaker demos |
| **Harness** (harness mode only) | The node's roles, caps and allowed targets, a scenario builder generated from `GET /_rustybin/harness/schema` with a YAML view and the built-in scenarios, validation, a live run view (counts, latency, a per-second chart, stop), the kill switch, and the final report with its classes (upstream, gateway, transport, lost), the gateway overhead and legs, what the gateway changed, thresholds, breakdowns and samples. Shown only when `RUSTYBIN_HARNESS=on` |
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
  [stateful mode](mocks/responses.md#stateful-mode)), whether the mock is secured and how
  requests are validated, then creates and deploys it.
- **Publish to portal** (header): publish the mock to the [dev portal](portal.md)
  with a title, a Markdown summary, tags, a contact and whether its demo credentials
  show; **Write with AI** writes them from the spec. A published mock carries an "on
  the portal" badge here and in the list.
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
  `claude mcp add` command and `mcpServers` JSON, which operations are tools, and
  **Open in the inspector**.
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

## Management

The Management view (the gear in the top bar, or `#/manage`) has a rail of tabs down
the left.

**Instance settings** come from `GET /_rustybin/settings` and save with one `PATCH`
per card (the admin token, or a control token with the `admin` scope). Every form is
generated from the settings' schema: under each field the console says where the
value comes from (saved in this instance, from the environment with the variable's
name, or the default); a setting held by `RUSTYBIN_SETTINGS_LOCK` is read-only with
the reason; a secret shows "Set, ends in k3Qx" with Replace and Clear and never its
value. The badges in the header say whether changes persist (`RUSTYBIN_STATE_DIR`)
or last until restart, whether the lock is on, and whether a stored secret became
unreadable after the secrets key changed. See [State directory, settings and
secrets](configuration.md#state-directory-settings-and-secrets).

| Tab | Cards |
|---|---|
| General | **Console**: the title and back link everyone sees. **This browser**: the settings below, kept in the browser. A link to **AI agents**, where the command and JSON for `claude mcp add` and other clients now live. |
| Sign-in | **Sign-in**: the control-auth mode, the user name and where it comes from, whether the password is the default, a stored hash or the environment's, who is signed in. **Change the password**: the user name and a new password, the current password required (`PUT /_rustybin/password`); every other session ends and this one continues with a fresh token. **Sessions** (password mode): the sign-in tokens issued since the last restart with their address and times, End for each, and **Sign out everywhere**. See [User name and password](concepts/control-plane-security.md#user-name-and-password). |
| AI | **AI assist**: on or why not, **Test the key**, **Use the built-in mock LLM**, the caps, the curated models of every provider with tier and price and **Use** / **Fast** buttons. **Provider and keys**: the `assist.*` settings (provider, model, fast model, compatible base URL, one secret key per provider). **Usage since start** and the privacy note. See [Bring your own AI](ai-assist.md). |
| Network | **Outbound connections**: the allowlist and the loopback switch (`egress.allow`, `egress.allow_loopback`). **Policy as the instance sees it**: the merged list, the always-refused addresses, each feature's own rule, and the last 100 decisions with a refresh. **Leads webhook**: the bearer token (a secret). See [Outbound connections](configuration.md#outbound-connections). |
| Portal | **Dev portal**: on or off and why, its URL with **Preview the portal**, who may read it, the base URL testers use, whether changes persist. **Branding and access**: the `portal.*` settings. **Items**: every publishable item by kind (mock APIs, MCP servers, the mock LLM, agents, the HTTP tools, the registry) with its state, why a published item is hidden, its views this week, **Publish** / **Edit** (the metadata dialog) and checkboxes to publish or unpublish several. **Insights**: views over 7 and 30 days, downloads, Try it calls. See [Dev portal](portal.md). |
| Export | **This instance as rustybin.yaml**: the version 2 document with secrets as references or omitted, **Copy**, **Download**, **Bundle** (a `.tar.gz` with the specs as files), the differences since the last export seen in this browser. **Apply a document**: paste or upload, **Dry run** (the changes and problems, nothing applied), then **Apply** (enabled by the dry run), **Prune** as an explicit checkbox with a confirmation. See [The whole instance as code](configuration-as-code.md). |
| Source control | **Repository**: the `git.*` settings (remote, branch, directory, authentication, the token as a secret, the author, the automatic mode). **Status**: git and the working copy, the last commit, ahead and behind (**Ask the remote**), what a commit would record. **Connect**, **Commit and push** (with **Write the commit message** from the AI), **Pull** (a dry run with the diff, then apply, prune as a checkbox), the two buttons that resolve a diverged branch, **Deploy key** (generate, copy the public half), **History** and **Operations**. See [Source control](source-control.md). |

**This browser** keeps, in the browser (the admin token in this tab only, like the
sign-in token; the others across visits):

| Setting | Effect |
|---|---|
| Session | The `X-Rustybin-Session` the console sends: it tags the console's traffic and scopes per-client state (flaky counters, bins, A2A tasks). In [public mode](concepts/sessions.md#public-mode) a random session is generated on first load, because the live traffic view needs one there. |
| Gateway base URL | Views offer to send their requests through this URL instead of directly to Rustybin (the gateway must allow CORS for the console's origin) |
| Admin token | Sent for admin-guarded actions (instance settings, health toggles, clearing all captured requests) when the instance has `RUSTYBIN_ADMIN_TOKEN`. Without it, the sign-in token is sent instead (useful with the admin token or a JWT with the `admin` scope). |
| Theme | Follow the system, light or dark |

## Control-plane endpoints used by the console

| Route | Returns |
|---|---|
| `GET /_rustybin/status` | Name, version, commit, instance id, hostname, uptime and start time, health, public mode, `admin_token_configured`, `control_auth`, `default_password` and the sign-in user, `hosted_mode`, console title and back link, `/identity` request count, inspector counters (stored, capacity, total captured) and ports |
| `POST /_rustybin/login`, `POST /_rustybin/logout`, `GET /_rustybin/session` | The sign-in (never behind control auth), the sign-out and "who am I" of the password mode |
| `GET`, `DELETE /_rustybin/sessions`, `DELETE /_rustybin/sessions/{id}`, `PUT /_rustybin/password` | The Sign-in tab (admin): the sessions, sign out everywhere, end one, change the user name and password |
| `GET /_rustybin/assist`, `POST /_rustybin/assist/run`, `POST /_rustybin/assist/test`, `GET /_rustybin/assist/usage` | The assist bar and drawer (the status, the streamed tasks), and the AI tab's test and usage (admin) |
| `GET /_rustybin/catalog` | The route catalogue as JSON: categories with counts, and every endpoint with methods, category, summary, description and runnable examples |
| `GET`, `PATCH /_rustybin/settings`, `DELETE /_rustybin/settings/{key}` | The instance settings of the Management view (admin): every registered key with its value, source, lock state and schema; secrets as `{set, hint}` |
| `GET /_rustybin/egress` | The outbound connection policy (admin): the allowlist, each feature's rule and the last decisions |
| `GET /_rustybin/portal`, `PUT`, `DELETE /_rustybin/portal/items/{id}`, `GET /_rustybin/portal/insights` | Management, Portal and Publish to portal: the items, publishing, the counters |
| `GET /_rustybin/config/export`, `POST /_rustybin/config/apply`, `GET /_rustybin/config/schema` | Management, Export (admin): the instance as `rustybin.yaml`, the dry run and the apply |
| `GET /_rustybin/git`, `POST /_rustybin/git/{init,commit,push,pull,resolve}`, `GET`, `POST /_rustybin/git/key`, `GET /_rustybin/git/history` | Management, Source control (admin) |

```hurl
{{#include ../examples/exports/console.hurl:status}}
```

```hurl
{{#include ../examples/exports/console.hurl:catalog}}
```

`conformance/ui/run.sh` drives every view in a real Chromium (Playwright), in light
and dark mode, and fails on any browser console error.
