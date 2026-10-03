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

## Views

`Alt+1` to `Alt+9` switch between the views, in this order:

| View | What it does |
|---|---|
| **Overview** | Which instance answers, whether it is healthy, uptime and inspector counters, and the base URLs (HTTP, HTTPS, gRPC, the AI providers, MCP, A2A, OIDC) to paste into a gateway configuration |
| **Live traffic** | The [request inspector](concepts/inspector.md) live over SSE: method, path, headers, body for every request Rustybin received, with filters, highlighting of headers gateways typically add or rewrite, copy as curl, and a compare mode that diffs two requests |
| **Request bins** | Create a [request bin](reference/request-bin.md) with a configured response, send a test request, watch captures arrive live |
| **API explorer** | The route catalogue with search and category filters, and a "Try it" request builder prefilled from each endpoint's examples |
| **AI playground** | Chat with the [mock LLM](ai/index.md) in OpenAI chat, OpenAI Responses, Anthropic, Gemini, Ollama (streaming) and Bedrock (non-streaming) wire formats, with modes, fault and latency injection, tool calling with a tool result round trip, the raw wire frames, and the [AI request record](ai/gateway-features.md#request-inspection) of each call |
| **MCP inspector** | An MCP client for 2026-07-28 (stateless) and 2025-11-25 (session): list and call tools with forms generated from their schemas, follow progress, read resources, get prompts, answer elicitation and sampling, a JSON-RPC message log, and the OAuth discovery steps of `/mcp/protected` |
| **A2A client** | Agent directory and cards, blocking or streaming sends over JSON-RPC in v1.0 or v0.3 (the HTTP+JSON binding is not used), a task timeline with artifacts, input-required and auth-required continuation, cancel, and the push notification sink |
| **Chaos and health** | The health toggle, flaky endpoints with live counters, a fault injection header builder, and a load generator (at most 500 requests and 50 in flight) with a status histogram and latency percentiles, for rate limiting, retry and circuit breaker demos |
| **Token lab** | Tokens from the [identity provider](reference/oidc.md) (client credentials, password, authorization code + PKCE in a popup), a JWT decoder with an expiry countdown, introspection, UserInfo and JWKS, an HMAC request signer for `/auth/hmac`, and a Standard Webhooks signer and verifier |

## Settings

The settings dialog (gear icon) stores, in the browser:

| Setting | Effect |
|---|---|
| Session | The `X-Rustybin-Session` the console sends: it tags the console's traffic and scopes per-client state (flaky counters, bins, A2A tasks). In [public mode](concepts/sessions.md#public-mode) a random session is generated on first load, because the live traffic view needs one there. |
| Gateway base URL | Views offer to send their requests through this URL instead of directly to Rustybin (the gateway must allow CORS for the console's origin) |
| Admin token | Sent for admin-guarded actions (health toggles, clearing all captured requests) when the instance has `RUSTYBIN_ADMIN_TOKEN` |
| Theme | Follow the system, light or dark |

## Control-plane endpoints used by the console

| Route | Returns |
|---|---|
| `GET /_rustybin/status` | Name, version, instance id, hostname, uptime and start time, health, public mode, `admin_token_configured`, `/identity` request count, inspector counters (stored, capacity, total captured) and ports |
| `GET /_rustybin/catalog` | The route catalogue as JSON: categories with counts, and every endpoint with methods, category, summary, description and runnable examples |

```hurl
{{#include ../examples/exports/console.hurl:status}}
```

```hurl
{{#include ../examples/exports/console.hurl:catalog}}
```

`conformance/ui/run.sh` drives every view in a real Chromium (Playwright), in light
and dark mode, and fails on any browser console error.
