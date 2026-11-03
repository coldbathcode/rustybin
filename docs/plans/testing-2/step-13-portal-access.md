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
