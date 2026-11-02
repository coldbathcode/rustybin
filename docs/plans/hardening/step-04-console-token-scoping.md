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

## Build notes (2026-11-02)

- Every finding is fixed; none was closed as not reproducible.
- `withConsoleHeaders` adds the bearer only with `control: true` and a control URL of
  this server; 53 literal control calls got `control: true`, plus the ones whose URL
  is built in a variable (`traffic.js`, `agents.js`, `lib/traffic.js`,
  `authorize.js`). `getJson` defaults to `control: true` (its paths are always
  literals). The API explorer sends `control: isControlUrl(url)`: the user picked the
  route in the console's own tool. `fetchDoc`, `run_request` and the MCP client's
  browser calls never ask for it (the management server's token is an explicit auth
  header the MCP view fills in).
- The session header goes to this server only; the admin header too; credentials the
  console added make the fetch `redirect: 'manual'` (an opaque redirect answers with
  an error that says why).
- `mcpclient.js`: a legacy `endpoint` on another origin, or on this instance's control
  plane, is refused; a closed legacy stream is opened again, and `legacyOpen` now
  returns the ready promise (it returned the state object, so a call could race the
  endpoint event).
- `mcpauth.js`: `untrustedUrlProblem` allows absolute `https:`, `http:` on loopback or
  this instance's origin, and never `/_rustybin/*` other than `/_rustybin/oauth/*`;
  the popup opens only an http(s) URL. `dom.webUrl` guards the A2A artifact links and
  images.
- `run_request`: `assist::task::data_plane_path` refuses `/_rustybin`, `/ui`,
  `/portal` and anything a browser could normalise into them (dot segments,
  backslashes, `%2e`, `%2f`, `%5c`, `%5f`); the console checks the normalised URL
  again, shows a notice instead of a card when refused, and titles the card with the
  method and path only.
- Scalar: pinned to 1.76.4 with its sha384, the document inlined into the page
  (`<` escaped), served with `Content-Security-Policy: sandbox allow-scripts
  allow-popups allow-popups-to-escape-sandbox`. An opaque origin has no
  `localStorage`, which the viewer reads at start, so the page installs an in-memory
  one first; checked in Chromium. Inlining also means the page needs no CORS.
- Settings forms: a choice gets a first option, `(not set)` or `(default: x)`, read as
  `null`.
- Tests: the jwt run of `conformance/ui` has a hostile MCP server (legacy endpoint and
  OAuth metadata pointing at the control plane) and checks that no request to
  `/_rustybin/mcp` or `/_rustybin/settings` carries the bearer, plus
  `withConsoleHeaders` evaluated in the page; `task.rs` and the `/docs` and portal
  reference tests in Rust.
