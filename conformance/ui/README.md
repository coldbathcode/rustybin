# Web console end-to-end check

`console_e2e.py` drives the web console (`/ui`) in a real Chromium with
Playwright, once in light and once in dark mode, and fails on any browser
console error or uncaught exception.

## Run

```bash
# Python venv with: pip install playwright pyjwt cryptography
PYTHON=/path/to/venv/bin/python conformance/ui/run.sh
```

`run.sh` builds the binary, starts it on ports 18700 (HTTP), 18701 (HTTPS) and
18702 (gRPC), plus a second instance on 18703 with `RUSTYBIN_CONTROL_AUTH=jwt`
(a signing key generated with openssl for the run, a console title and back link),
runs the script and stops the servers. `RUSTYBIN_UI_PORT` moves the ports. Screenshots land in
`conformance/ui/screenshots/` (override with `SCREENSHOT_DIR`).

Against an already running server:

```bash
RUSTYBIN_URL=http://127.0.0.1:8080 SCREENSHOT_DIR=/tmp/shots python conformance/ui/console_e2e.py
```

Other knobs: `THEMES=dark` (one theme only), `HEADED=1` (visible browser),
`CHROMIUM_PATH=/path/to/chrome` (otherwise the newest Chromium under
`$PLAYWRIGHT_BROWSERS_PATH`, else Playwright's own download).

## What it covers

| View | Flow |
|---|---|
| Overview | status tiles, base URLs, `/identity`, configuration |
| Live traffic | requests sent from outside appear live, gateway header highlighting, body viewer, compare mode (header diff) |
| Request bins | create a bin, send a test request, see it arrive over SSE |
| API explorer | catalogue search, Try it, response viewer |
| AI playground | streaming chat with OpenAI chat, OpenAI Responses, Anthropic, Gemini, Ollama and Bedrock (non-stream), tool call round trip, raw wire panel |
| MCP inspector | 2026-07-28 connect, tools list, `slow_task` with progress, elicitation (multi round-trip), resources, 2025-11-25 session connect, `/mcp/protected` OAuth discovery and token |
| A2A client | travel planner stream with artifacts, approval agent input-required continuation, secure agent auth-required continuation |
| Chaos and health | flaky counters, load generator (histogram, percentiles) |
| Token lab | client_credentials, JWT decoder with countdown, introspection, authorization code + PKCE popup, HMAC signer, Standard Webhooks sign and verify |
| Shell | settings dialog, keyboard navigation (Alt+1..9), send via a CORS-enabled second origin, unreachable gateway message, phone width without horizontal scroll |
| Control-plane auth (jwt instance) | sign-in screen with back link, refused token, `#token=` fragment sign-in (token stripped from the URL, kept for the tab), title and back link in the header, live feed over fetch with the bearer token, reload, expiry screen, sign out |
