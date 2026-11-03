# Step 13: Open and closed APIs on the portal

Access control, chapter 7 of 8.

## Goal

Each API the operator publishes on the dev portal is **open** or **closed**, and nothing more
elaborate than that:

- **Open**: anyone who reaches `/portal` reads it, as every item does today.
- **Closed**: a visitor who is not signed in sees it with a lock and can read it (its docs,
  operations, schemas, spec and reference) but not use it: Try it, the demo credentials, the
  snippets and collections that carry them, and the MCP connection details unlock after signing
  in to the portal with a **user name and password**. The calls themselves need the credentials
  too: a closed mock has its mocked security on, so a call without the credentials the portal
  hands to a signed-in reader is refused by the mock.

A tester gets a login that opens the portal and nothing in the console, so handing out a portal
login never hands out the instance.

## What exists (0.3.1)

- `portal.auth` (`open`, `password`, `key`) is one switch for the whole portal; there is no
  setting per item.
- `password` mode's sign-in screen posts to `/_rustybin/login` and keeps the answer, a console
  token with the `admin` scope, in the portal's tab: the only portal login is the instance's admin
  account.
- `/portal/reference/{id}` skips `portal.auth` (a page load carries no header), and a mock's spec
  is public at `/mock/<name>/_rustybin/openapi.{json,yaml}`, which the portal links to.

## Model

- **One field per item**: `access: open | closed` on the published metadata (`portal::Meta`, the
  `portal.items` entries of `rustybin.yaml`), default `open`, so every 0.3 item stays as it is.
- **Who may read a closed item**: a user (step 07) holding the new permission `portal.read`. Every
  built-in role has it (it is a `*.read`, so `viewer` and up), and a fifth built-in role,
  `portal_reader`, has it alone: the login to give testers. A user whose roles hold nothing but
  `portal.read` who signs in to the console is told the account opens the API portal and sent
  there.
- **The portal session.** `POST /portal/api/login` (`username`, `password`, the same throttle and
  answers as `/_rustybin/login`, a disabled user answered as a wrong password) sets a cookie,
  `rustybin_portal` (`HttpOnly`, `SameSite=Strict`, `Secure` behind HTTPS, `Path=/portal`, 12
  hours), holding an HS256 token signed with its own subkey (`portal`) of the OAuth key, with
  `rb_use: "portal"`, the user id and the generations of step 07. It is accepted under `/portal`
  only, never on the control plane, the data plane or `/_rustybin/mcp` (a test sends it to every
  catalogue route), as the OAuth server's tokens are kept to `/_rustybin/mcp`. Permissions are
  resolved at every request, so a disabled user or a removed role loses the closed items on the
  next one. `POST /portal/api/logout` clears it; `GET /portal/api/session` answers the signed-in
  user's display name or nothing. A cookie (not a header) is what lets a page load, such as the
  reference page, carry the session.
- **Scripts keep a header**: a bearer control credential (the admin token, a control JWT, a
  sign-in token) that resolves `portal.read` reads closed items too, for CI that downloads
  collections.
- **`portal.auth`** keeps its three values with the per-item field on top: `open` (the default:
  each item's `access` decides), `password` (the whole portal behind the sign-in: every item is
  treated as closed and the home page asks to sign in) and `key` (an API key reads open items, as
  today; a closed item still needs the sign-in). The 0.3 sign-in screen that stored a console
  token goes away.
- **Joining as a tester**: an invitation (step 09) for a user whose only role is `portal_reader`
  links to `/portal/#/join/<token>`, so a tester sets their password and lands on the portal
  without seeing the console.

## What closed covers, route by route

Owner decision (2026-11-03): docs and calls are seen, not used, until the reader signs in and gets
the credentials.

| Route | Signed out, closed item |
|---|---|
| `GET /portal/api/catalog` | the whole entry (operations, descriptions, schemas, base URL) with `locked: true` and without the demo credentials, credentialed snippets, MCP connection snippets and collection links |
| `GET /portal/api/items/{id}/collections/{format}` | `401` (collections carry the credentials) |
| Try it | disabled, "Sign in to try this API"; the curl lines show the request with a placeholder for the credential |
| `GET /portal/reference/{id}` and the mock's `/_rustybin/openapi.{json,yaml}` | readable, as today (the spec holds no credential) |
| `POST /portal/api/events` | views counted for everyone, Try it and downloads only for readers |

**The calls need the credentials.** A mock can be closed only with its mocked security on (the
mode the spec declares, or `api_key` when it declares none): `PUT /_rustybin/portal/items/{id}`
with `access: closed` on a mock without it answers `409` naming the fix, and the console's
publish dialog offers **Turn on security and close** in one step. A call without the credentials
is refused by the mock as mocked security always refuses it; the signed-in reader gets the
credentials on the portal. A shared-mode instance may close a mock with API keys instead (the
`key` mode). The other kinds of item (`mcp:*`, `llm`, `a2a:*`, `tools`, `registry`) are built-in
endpoints: closing one hides its credentials and connection snippets, and the protected ones
(`mcp:protected`, `mcp:apikey`) already refuse a call without them; the publish dialog says which
open endpoints stay callable.

## Surfaces

- **API**: `access` on `PUT /_rustybin/portal/items/{id}` and in `GET /_rustybin/portal`;
  `/portal/api/login`, `/logout` and `/session` in the catalogue and the
  OpenAPI document. Changing an item's access needs `portal.publish`, as publishing does.
- **Console**: the publish dialog's **Open / Closed** choice (open by default, the one sentence
  on what closed covers, **Turn on security and close** for a mock without mocked security); Management, Portal shows a lock on closed items and a count of readers;
  Management, Users and roles has **Invite a portal reader** (the `portal_reader` role, the
  portal's join link).
- **Portal**: a **Sign in** button in the header (user name and password), the lock on closed
  cards and a disabled Try it with "Sign in to try this API", the signed-in name and **Sign
  out**, the join page.
- **Management MCP**: `publish_portal_item` takes `access`; `list_portal_items` answers it;
  `invite_user` takes the `portal_reader` role like any other.
- **CLI**: `rustybin portal publish <id> --access closed`; `portal status` shows each item's
  access; `rustybin users invite <name> --role portal_reader`.
- **`rustybin.yaml`**: `access: closed` on a `portal.items` entry, exported and applied. A
  document pulled from git (untrusted, 0.3.1) may close an item but not open a closed one; that
  entry is reported and the item stays closed.

## Tests

The catalogue's locked entry holds no credential, credentialed snippet or collection link (a test
plants credentials in every kind of item and greps the answer); every route of the table above,
signed out and signed in; closing a mock without mocked security is refused and the console's
one-step fix; a closed mock refuses a call without its credentials and serves one with them; the
portal cookie refused on every control, data-plane and MCP route; a role removed or a user
disabled hides closed items on the next request; `portal.auth=password` closes every item and
`key` still asks for the sign-in on a closed one; a console sign-in by a `portal_reader`; the sign-in throttle shared with `/_rustybin/login`; the
five-surface parity tests; the document round trip and the untrusted refusal; the portal
conformance in Playwright (a closed API read signed out, Try it locked, sign in, the
credentials and Try it appear, sign out).

## Build notes (2026-11-03)

- **Where it is.** The model in `src/portal/mod.rs` (`Access`, `closed`, `mock_secured`; `access` on `Published`,
  `Meta` and `FileItem`, unset keeps what the item had), the locked catalogue in `src/portal/items.rs` (`Ctx.reader`,
  `locked` passed into each kind's detail so the entry is built without the credentials), the sign-in in
  `src/portal/session.rs`, the routes and the 409 in `src/portal/api.rs` (`gate` answers whether the caller reads
  closed items; `check_closable`, `secure_mock`, `publish_secured`), the shared sign-in check in `src/login.rs`
  (`check_sign_in`, used by `/_rustybin/login` and `/portal/api/login`; `portal_only`), the subkey in
  `src/control_oauth.rs` (`portal_secret`), the portal invitation link in `src/rbac/service.rs`
  (`invitation_link_for`), the document rules in `src/configdoc/mod.rs` (`apply_portal`, the export). Tests:
  `src/portal/access_tests.rs` (the planted credentials test first, then every route signed out and in, the cookie on
  every catalogue route, a role removed or a user disabled, the 409 and `secure`, `portal.auth`, a console sign-in by
  a reader, the shared throttle, documents), `portal_tools_open_and_close_items` in the management tests,
  `docs/examples/portal/portal.hurl` (anchors `close`, `locked`, `reader`, `security_off`) and
  `docs/examples/portal/access.hurl` (the password instance: invite, join, sign in and out), `run_portal_access` in
  `conformance/ui/console_e2e.py`.
- **What a locked entry leaves out**, by kind: a mock's demo credentials (`reveal` false, so the samples and curl
  lines carry the placeholders `<api-key>`, `Basic <base64 user:password>`, `<token>`; the security summary also
  drops the Basic user names), its collection links and its MCP connection (`url`, `claude`, `json`; the tools stay,
  they are docs); an MCP server's `claude`, `json` and inspector link (the URL, auth and tools stay); the tools item's
  examples (they carry demo Basic users and tokens). The LLM, the agents and the registry carry no credential: Try it
  is locked for them in the portal, and their endpoints stay callable, as the plan says. The answer carries `access`
  and `locked` per item and `portal.reader` and `portal.signin`; a reader's answer is `private, no-cache`, and every
  catalogue answer has `Vary: Cookie, Authorization, X-Rustybin-Admin-Token, X-Rustybin-Key` (the cache key holds
  the reader flag).
- **Decision: a portal_reader's console sign-in still gets its token.** The plan says the person is told and sent
  to the portal. `/_rustybin/login` (and an invitation accepted on the console's join page) answers the usual token
  plus `portal_only: true` and `portal_url`, and sets the portal's cookie when the portal is on; the console revokes
  the token at once and goes to the portal, signed in. Refusing the token instead was tried and dropped: it would
  have taken `/_rustybin/me` and `rustybin passwd` from portal readers (step 09's "`/me` for every role"). The token
  grants nothing else (the reader holds only `portal.read`).
- **Decision: joining from the portal.** The portal's join page posts `portal: true` to `POST /_rustybin/join`: the
  answer sets the portal's cookie and carries no console token. The invitation link is
  `/portal/#/join/<token>` for a user whose permissions are exactly `portal.read` (any role mix that resolves so),
  else the console's.
- **Decision: the session answer** is `{signed_in, via, user, display_name, reader, signin}` (200 signed out too,
  with `signed_in: false`), not an empty body, so the SPA and the Hurl examples read one shape. `via` says `cookie`
  or `bearer`.
- **Decision: what a session is.** The portal's sessions are not listed with the console's (Management, Sign-in);
  "sign out everywhere" ends them (they carry the sign-in generation), and so do a disabled user and a moved user
  generation; `POST /portal/api/logout` puts the session id on the sign-in denylist. The audience is the control
  audience; the subkey (`portal`) and `rb_use: "portal"` keep it apart from console and OAuth tokens (the console's
  `is_console_token` and the OAuth verifier both refuse it).
- **Decision: `secure: true` on the PUT** (and `secure` on `publish_portal_item`, `--secure` on the CLI) is the
  server side of **Turn on security and close**, so it is one call with one permission check: it needs the mock's
  manage rights (`mocks.write`, listed in `BODY_CHECKS`), is refused while the mock has a draft that is not deployed
  (deploying it would put other changes live), sets the mode the spec declares (`spec`) or `api_key` when it declares
  none (a mock with a mode of its own, such as `basic`, keeps it), and deploys through the compile pool. Closing
  checks the live revision: on, and in `spec` mode with an operation that declares a requirement. A shared-mode
  instance or one with `RUSTYBIN_REQUIRE_KEY` closes a mock on its keys alone. If the security is turned off later the
  item stays closed and `GET /_rustybin/portal` answers a `warning` for it (no 409 then: the plan was silent).
- **Decision: documents.** An entry closing a mock without mocked security is reported and skipped (the mocks section
  is applied first, so a document may turn the security on and close in one go). A pulled document's `access: open`
  on a closed item is reported and the item stays closed; its other metadata applies. Not refused: a pulled document
  pruning (unpublishing) a closed item, or publishing an item it did not have before as open (both 0.3 behaviour; the
  first hides, the second was always allowed). A `RUSTYBIN_CONFIG_FILE` item that closes a mock without security is
  not refused at load (the file loader has no portal rule today); the control view's warning covers it.
- **Decision: `portal.auth=password`** answers `401` signed out (the home page asks to sign in), lets in readers only
  (a signed-in user without `portal.read` gets `403 permission_required: portal.read`), and gates the reference page
  too (a page load carries the cookie now). Without the password sign-in (jwt or token mode) the portal still offers
  the 0.3 token form, kept in the tab and sent as a bearer: the "0.3 screen that stored a console token" is gone in
  password mode only, where the portal sign-in replaces it.
- **Who can call what.** No new control route (the three new routes are under `/portal`, which declares nothing and
  is not in `NEW_SINCE_0_4`); no `DELIBERATE` row. `portal.read` leaves `RESERVED`: it is listed in `BODY_CHECKS`
  (checked in `src/portal/session.rs`). Deliberate changes outside the control plane: with `portal.auth=password` a
  control JWT with only the `inspector` or `harness` scope no longer reads the portal (it holds no `portal.read`; every
  built-in role and the `console` and `admin` scopes do), and the reference page needs a reader there; a Try it
  event or a collection of a closed item answers `401` (or `403` signed in without `portal.read`).
- **Console.** The publish dialog's Access radios (`#portal-access-open`, `#portal-access-closed`) with the sentence
  on what closed covers, which endpoints stay callable, and **Turn on security and close** for a mock whose `secured`
  is false; Management, Portal shows a `closed` badge per item, the closed count and the readers (`readers` in `GET
  /_rustybin/portal`: enabled users holding `portal.read`, and those whose login opens only the portal); Users and
  roles has **Invite a portal reader**. The portal: **Sign in** in the header (`#/signin`), the lock badge on closed
  cards, a notice on the page, the Try it button disabled with "Sign in to try this API", the signed-in name and
  **Sign out**, `#/join/<token>` (the token leaves the address bar at once).
- **Left for later.** A reader count per item and portal sessions in the sessions list (step 14's audit log is the
  place for who read what); a portal page to change one's own password (portal readers keep `rustybin passwd` and
  `/_rustybin/me/password` with the console token their sign-in still answers).
