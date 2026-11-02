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

## Build notes (2026-11-02)

Every finding is fixed; none was closed as not reproducible. Console code only (`ui/`,
`ui-portal/`); no Rust change. Tests: new and extended steps of `conformance/ui/console_e2e.py`
(open mode and the jwt run).

1. **The console token in the MCP inspector.** `applyServerDefaults` no longer copies
   `auth.getToken()` into `st.auth`; the form (`st.auth`, kept in the per-tab scratch memory)
   only ever holds what the user typed or an OAuth walk fetched. `authParts()` adds
   `Authorization: Bearer <console token>` at send time, only when the target is exactly this
   instance's `/_rustybin/mcp` (origin and path, gateway prefix kept: `isOwnManageUrl`), the auth
   kind is bearer and the field was left empty (the field's hint says so); "No authentication"
   now means none. The auth form is reset (`defaultAuth()`, the OAuth state dropped) on a source
   change, a server or saved-connection pick, every `#/mcp/server/...` and `#/mcp/url/...` route
   (the registry's "Open in inspector"), and when the URL field is committed on another origin
   than before with credentials in the form (a toast says they were cleared). The reset also
   starts a new timeline: its entries keep the headers they were sent with, and the Hurl and curl
   exports replay those headers against the current URL. A `#/mcp/server/...` link switches to
   browser mode (the relay never reaches this instance's management server, so a registry link's
   relay mode used to make the next one fail). A saved connection therefore never seals the
   console token either. The export still includes the bearer for the
   management server on Copy (masked on screen), as before. Conformance (jwt): after connecting
   to the management server the token field is empty and the token is not in the inspector's
   state; a `#/mcp/url/bearer/-/modern/https://mcp.example.com/mcp` link opens with an empty
   field and no relay call's body carries the token; back on the management server it connects
   again.
2. **Stale edits.** `mockapi.api` no longer keeps a per-mock revision map updated by every read
   (the 10 s header poll made a stale form look current). An edit passes `opts.revision`, the
   draft revision its form was built from, sent as `If-Match`: the settings panel (behaviour,
   rename, replace the spec), the security panel (save, the on/off switch, per-operation public
   toggles; the switch now uses the rendered detail, not the polled one), the MCP panel and the
   operation editor (`data.draft_revision` from its own GET or the PUT answer). A 409
   `rustybin_stale_revision` keeps the existing reload prompt. Deploy: the server's deploy
   route takes no revision, so the page reads the mock first and, when its `draft_revision`
   differs from the one shown, asks before deploying (naming the revisions and the draft
   changes; Cancel refreshes the page); it also sends `If-Match` with the revision it deploys,
   ignored by today's server. That pre-read leaves a window of one round trip: closing it needs
   the server change below. Rollback names its target revision explicitly and does not touch
   the draft, so it sends none. Conformance (open): a background PATCH while the settings form
   is open (the poll frozen through `document.hidden`), Save gets the prompt and the server
   keeps the background values; Deploy then asks and nothing is deployed.
   **Server change needed (not made, `src/` is out of scope):** `POST
   /_rustybin/mocks/{name}/deploy` (and the `deploy_mock` tool, the CLI) should accept
   `If-Match` / `expected_revision` and check it under the store's write lock, as PATCH and PUT
   operation do, so a deploy can never publish a draft newer than the one the caller saw.
3. **OAuth popups.** COOP cannot be set without Rust (the console's headers are in
   `src/ui.rs`), so the popup has no opener instead: the new `ui/lib/oauthpopup.js` opens a
   blank popup, sets its `opener` to null and only then sends it to the authorization page
   (`openPopup`; the console keeps the handle, so a blocked or closed popup is still noticed,
   which a plain `noopener` open cannot tell). `/ui/callback` hands the query string back over a
   `BroadcastChannel` (same origin only, `deliverCallback`); the waiting flow takes it only when
   the `state` is its own and acknowledges it (`waitForCallback`); without an acknowledgement the
   callback keeps the token lab's same-window fallback. The callback also strips the code from
   the address bar. Used by the MCP inspector's OAuth walk and the token lab. Conformance: the
   token lab's popup asserts `window.opener === null` on the authorization page, and its code
   flow completes through the channel.
4. **Sign-out.** `auth.js` tracks who the tab's data belongs to (a JWT's `sub`, `rb_key`, `iss`;
   an opaque token itself) and emits `reset` when the token is forgotten (sign-out, a cleared
   token) or a token for someone else arrives (the sign-in screen, a `#token=` link). `app.js`
   then clears captured traffic, the admin token, every per-tab scratch value
   (`settings.forgetScratch`: the MCP inspector's auth form among them), the assistant's history
   and drawer (`assist.forget`), the last export and OAuth flows in progress (session keys), and
   unmounts the current view (the sign-in mounts it again). An expired session signed in again
   by the same user keeps them. Conformance (jwt): traffic, an admin token, typed inspector
   credentials, assistant history and the last export are all gone after sign-out, and again
   after another user's `#token=` sign-in; the inspector form starts empty.
5. **Prune after a dry run.** The dry run that enables Apply is remembered with its prune choice;
   ticking or unticking prune disables Apply with "Prune changed: run the dry run again", and
   Apply sends the previewed prune value. Conformance (open) checks it.
6. **Malformed escapes.** The console's `parseRoute`, the portal's router and the inspector's
   route parts decode inside `try` and keep the raw text. Conformance: `#/mcp/server/%E0%A4%A`
   opens the inspector, `/portal/#/k/bad%E0%A4%A` renders the kind page.
7. **Consent redirect.** `authorize.js` checks `redirect_to` before following it
   (`redirectProblem`): absolute http(s), or a custom app scheme; never `javascript:`,
   `data:`, `vbscript:`, `blob:`, `file:`, `about:`, `filesystem:`. A refused one shows an error
   (the answer was recorded). Checked in the page by the console guards step.
8. **Discovery URLs (verified: it reproduced).** `/_rustybin/mocks/x/_rustybin/oauth/token` was
   accepted (the check was `includes`), and a remote server's documents could point the browser
   at `http://127.0.0.1:<port>` (local services). `untrustedUrlProblem(url, resource)` now allows
   only this instance's exact `/_rustybin/oauth/{register,authorize,token}` among control paths,
   and a loopback host (any scheme) only when the inspected server, the URL the user chose, is
   itself on loopback (the walk passes it; refresh uses the stored target, not the document's
   own `resource`). Impact was limited (these fetches never carry the control token); the
   console guards step checks both rules.
