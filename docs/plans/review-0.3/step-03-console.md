# Step 03: Console: the token, stale edits, popups, sign-out

| # | Severity | Finding | Anchor | Fix |
|---|---|---|---|---|
| 1 | high | The MCP inspector copies the console token into the shared auth form for the management server; a registry "Open in inspector" link or `#/mcp/url/bearer/...` keeps it, so Connect sends the admin token to a third-party server | `ui/views/mcp.js:90`, `:809`, `ui/views/registry.js:73` | never store the console token in `st.auth`; add it at send time for this instance's control URL only; reset auth on source or URL change |
| 2 | medium | Edit-conflict protection is defeated by the detail view's polling: every read updates the remembered revision, so a stale form sends the current one; deploy and rollback send none | `ui/lib/mockapi.js:31`, `:65`, `ui/views/mocks/detail.js:683` | each editor keeps the revision it loaded and sends it; deploy names the revision shown |
| 3 | medium | The MCP OAuth popup opens a third party's authorization page without `noopener` and without COOP: reverse tabnabbing | `ui/lib/mcpauth.js:164`, `ui/app.js:276` | `noopener` with the code back through `BroadcastChannel` and a state check, or COOP `same-origin-allow-popups` |
| 4 | medium | Signing out leaves the previous user's captured traffic, the admin token, the inspector's auth state, the assistant history and the last export in the tab | `ui/lib/auth.js:201`, `ui/lib/traffic.js:13`, `ui/lib/settings.js:8` | clear them on sign-out and sign-in |
| 5 | low | Ticking prune after a dry run applies deletions the preview never showed | `ui/views/manage/export.js:96`, `:113` | a prune change invalidates the preview |
| 6 | low | A malformed `%` escape in the hash stops routing (console and portal) | `ui/app.js:132`, `ui-portal/portal.js:602` | guarded decode |
| 7 | low | The consent page follows `redirect_to` without a client-side scheme check (the server checks) | `ui/views/authorize.js:117` | allow http(s) and non-script custom schemes only |
| 8 | low (verify first) | The discovery-URL check matches `/_rustybin/oauth/` anywhere in the path and allows loopback `http:` | `ui/lib/mcpauth.js:63` | exact prefixes; confirm loopback targets |
