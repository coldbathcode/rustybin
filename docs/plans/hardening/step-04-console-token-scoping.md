# Step 04: Console: the control token only where the console chose to send it

## Goal

`withConsoleHeaders` (`ui/lib/http.js:92-97`) attaches the admin bearer to any same-origin URL under
`/_rustybin/`, decided by the URL's shape. Every caller that takes a URL or path from third-party
content (an MCP server, OAuth metadata, an AI answer) therefore sends the admin token where that
content points. Make the bearer opt-in per call, then fix each caller. Classic DOM XSS is already
well covered (no `innerHTML` sinks, `script-src 'self'`), so this step is about where requests go.

## Findings

| Sev | Where | Problem | Fix |
|---|---|---|---|
| high | `ui/lib/http.js:92-97` | the bearer is inferred from the URL | `send({control: true})` for the console's own control calls; no bearer otherwise |
| high | `ui/lib/mcpclient.js:210,239` | the legacy transport trusts the server's `endpoint` event with no origin check: a malicious server points the console's posts at `/_rustybin/mcp`, which receive the admin bearer | refuse an endpoint whose origin differs from the SSE URL's |
| high | `ui/lib/mcpauth.js:51-55` (used at 85, 104, 120, 165) | `fetchDoc` resolves relative and same-origin URLs from discovery documents, so `resource_metadata`, `registration_endpoint` or `token_endpoint` set to `/_rustybin/...` make authenticated control calls | accept only absolute `https:` (loopback `http:`); never console credentials in `fetchDoc` |
| high | `ui/lib/assist.js:149-161`, `src/assist/task.rs:223-238` | a `run_request` action may target `/_rustybin/*`, with an AI-chosen label: a prompt injection in captured traffic offers "Copy the corrected header" that sends `PATCH /_rustybin/settings` as admin | refuse `/_rustybin`, `/ui` and `/portal` in `task.rs` and in `applyCard`; title is always method and path; send without control credentials |
| medium | `src/portal/api.rs:450-458`, `src/openapi.rs:1503` | the Scalar reference loads an unpinned CDN script with no SRI and no CSP, on the console's origin, rendering publishers' specs; it could read `localStorage['rustybin.console.rememberedToken']` | pin the version with SRI; serve with `Content-Security-Policy: sandbox allow-scripts allow-popups` (opaque origin) |
| medium | `ui/lib/settingsform.js:44-46` | a choice with a null value and default has no empty option: the browser preselects `anthropic`, saving the AI group stores it silently, and it cannot be cleared | a `(not set)` option mapped to `null` |
| low | `ui/lib/http.js:90` | `X-Rustybin-Session` goes to every URL, including third-party MCP servers in browser mode | only to `isServerUrl(url)` |
| low | `ui/lib/http.js:95-96,162` | in shared mode the token rides as `X-Rustybin-Key-Token` on data-plane calls with `redirect: 'follow'`; a mock answering 302 cross-origin receives it | `redirect: 'manual'` when credentials were added |
| low | `ui/lib/mcpclient.js:197-198,223-224` | a closed legacy stream is never reopened; every later call waits 30 s | reopen when `this.legacy.closed` |
| low | `ui/lib/mcpauth.js:143`, `ui/views/a2a.js:296` | untrusted URLs reach `window.open`, `href` and `img src` with no scheme check (only the CSP stops `javascript:`) | allow `https?:` only |

## Tests

`conformance/ui/run.sh` with a hostile MCP server fixture (legacy endpoint on another origin,
metadata pointing at `/_rustybin/`) asserting no request to a control path carries a bearer; a unit
test of `withConsoleHeaders`; a `task.rs` test refusing control paths.

## Done when

No console code path attaches the control token to a URL it did not build itself, and the UI
conformance run is green in open and jwt mode.
