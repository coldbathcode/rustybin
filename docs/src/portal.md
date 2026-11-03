# Dev portal

When one Rustybin is the team's mocking service, testers need to find what is on it
without the console or its password. The **dev portal** at `/portal` is a read-only
site that shows only what the operator **published**: mock APIs (with their MCP
servers), the mock MCP servers, the mock LLM, the A2A agents, the HTTP tools and the
mock MCP registry. Testers read the docs, send requests from the page, download
collections and copy connection snippets.

## For operators

### Publish

Every item has a stable id:

| Id | What the portal shows |
|---|---|
| `mock:<name>` | A [mock API](mocks/index.md): base URL, authentication with the demo credentials it accepts, every operation with **Try it** and curl, the full API reference, downloads (Postman, Hurl, curl, `.http`, k6, Bruno, Insomnia), the mock's [MCP server](mocks/mcp.md) with `claude mcp add` and the `mcpServers` JSON, and its registry entry when the registry is published |
| `mcp:main`, `mcp:protected`, `mcp:apikey`, `mcp:sse`, `mcp:weather`, `mcp:crm`, `mcp:devtools` | One of the [mock MCP servers](mcp/index.md): URL, transport, the auth it expects, its tools and their input schemas, connection snippets, a link to the console's inspector |
| `llm` | The [mock LLM](ai/index.md): every provider's base URL and routes, a sample request per provider with Try it, the headers that shape answers |
| `a2a:<agent>` | An [A2A agent](a2a/index.md): its card, skills, a sample message with Try it |
| `tools` | The echo, status, shaping, streaming, redirect, auth, GraphQL, SOAP, WebSocket and reliability routes of the [catalogue](concepts/catalogue.md), with their examples |
| `registry` | The [mock MCP registry](mcp/registry.md): its URL, scenario and a sample list call |

Publish from the console (a mock's page: **Publish to portal**; or **Management,
Portal**, one item or several at once), from `rustybin.yaml`, or with the API:

```hurl
{{#include ../examples/portal/portal.hurl:publish_mock}}
```

| Field | |
|---|---|
| `title` | At most 120 characters; empty uses the item's own (the spec's title, the agent's name) |
| `summary` | Markdown, at most 2000 characters; empty uses the item's own description |
| `tags` | At most 10, shown on the cards and searched |
| `contact` | Who testers ask about it |
| `order` | Lower comes first within its kind (-1000 to 1000) |
| `credentials` | A mock only: show the demo credentials it accepts (default `true`). Mocked security keys gate a mock, not anything real; turn this off when testers should ask for one |
| `access` | `open` (default) or `closed`: see [open and closed items](#open-and-closed-items) |
| `secure` | Not stored: with `access: closed` on a mock without mocked security, turn its security on and deploy it first (needs `mocks.write`) |

A second `PUT` updates the entry (it keeps its publish time); `DELETE` removes it.
Publishing a mock needs the rights to change it, everything else needs admin. A mock
that is not deployed stays hidden until it is (`GET /_rustybin/portal` says why an
item is hidden). On a [self-service](mocks/self-service.md) instance, visitors may
publish their own mocks when `portal.visitors` is on: their entries carry a visitor
badge and leave with the visit.

```hurl
{{#include ../examples/portal/portal.hurl:publish_more}}
```

```hurl
{{#include ../examples/portal/portal.hurl:unpublish}}
```

In the console, **Write with AI** in the publish dialog of a mock writes the title,
summary and tags from the spec (the `summarise_for_portal` task of
[AI assist](ai-assist.md)); you check them before you publish.

### Publish from rustybin.yaml

A `portal:` section in [the config file](mocks/config.md) publishes items as code;
they reload with the file, and the API refuses to change them (`409`):

```yaml
version: 1
mocks:
  - name: orders
    spec: specs/orders.yaml
portal:
  items:
    - id: mock:orders
      title: Orders
      summary: Create and track orders. Use the **demo-key** API key.
      tags: [commerce]
      access: closed
    - id: mcp:main
    - id: llm
```

A document [pulled from git](source-control.md) may close an item but never open a
closed one: that entry is reported in `problems` and the item stays closed. An entry
that closes a mock without mocked security is reported and skipped (turn the mock's
`security` on in the same document: the mocks section is applied first).

The branding and access settings below are included under `settings` in the
[version 2 instance document](configuration-as-code.md), alongside published items.

### Turn it on, brand it, lock it

Settings under **Management, Portal** (or `PATCH /_rustybin/settings`), seeded by
their variables (see [configuration](configuration.md#dev-portal)):

| Setting | Variable | |
|---|---|---|
| `portal.enabled` | `RUSTYBIN_PORTAL` | `auto` (default): on once something is published; `on`; `off` (`/portal` answers `404`) |
| `portal.auth` | `RUSTYBIN_PORTAL_AUTH` | `open` (default), `password`, `key` |
| `portal.base_url` | `RUSTYBIN_PORTAL_BASE_URL` | The URL testers should call when a gateway sits in front of the instance: snippets, collections and Try it use it |
| `portal.title`, `portal.intro`, `portal.accent`, `portal.contact` | | The header, the home page's introduction (Markdown), the accent colour (`rust`, `blue`, `green`, `purple`, `teal`, `slate`), the footer's contact |
| `portal.visitors` | `RUSTYBIN_PORTAL_VISITORS` | Visitors may publish their own mocks |

With nothing published the portal is off:

```hurl
{{#include ../examples/portal/portal.hurl:off}}
```

**Who may read it.** `portal.auth` is the switch for the whole portal; each item's
`access` sits on top of it:

| `portal.auth` | Signed out | A portal reader |
|---|---|---|
| `open` (default) | Reads every item; uses the open ones, a closed one is locked | Uses everything |
| `password` | The home page asks to sign in (`401`); every item counts as closed | Uses everything |
| `key` | An API key of the [key registry](concepts/shared-mode.md) reads the open items, which Try it then sends with every call (`X-Rustybin-Key`); a closed one is locked until the sign-in | Uses everything |

A **portal reader** is a user holding `portal.read` signed in to the portal (below),
or a request with a bearer control credential that resolves `portal.read` (the admin
token, a control JWT, a console sign-in token), for scripts that download
collections. The page itself always loads (it holds no data):

```hurl
{{#include ../examples/portal/portal.hurl:password}}
```

The full API reference page of a mock (`/portal/reference/mock:<name>`) is read by
everyone in `open` and `key` mode, closed item or not: it shows the mock's spec, which
is public at `/mock/<name>/_rustybin/openapi.json` anyway and holds no credential.
With `portal.auth=password` it needs a reader (the page load carries the portal's
cookie).

### Open and closed items

Each published item is **open** or **closed**. A closed item is read by everyone who
reaches the portal (its docs, operations, schemas, spec and reference) but used only by
a reader: Try it, the demo credentials, the snippets and collections that carry them
and the MCP connection details unlock after the sign-in. The calls themselves need
the credentials too: a mock closes only with its **mocked security on** (the mode the
spec declares, or `api_key` when it declares none), so a call without the credentials
the portal hands to a signed-in reader is refused by the mock. On a
[shared-mode](concepts/shared-mode.md) instance (or with `RUSTYBIN_REQUIRE_KEY`) every
call already needs an API key, so a mock closes on the keys alone.

```hurl
{{#include ../examples/portal/portal.hurl:close}}
```

Signed out, the catalogue answers a closed item with `locked: true` and without the
demo credentials (the curl lines show a placeholder such as `<api-key>`), the
collection links and the MCP connection snippets; the collections answer `401`; a
view is counted for everyone, a Try it only for readers:

```hurl
{{#include ../examples/portal/portal.hurl:locked}}
```

```hurl
{{#include ../examples/portal/portal.hurl:reader}}
```

Closing a mock without mocked security is refused with `409` naming the fix;
`secure: true` (the console's **Turn on security and close**) turns it on, deploys it
and closes the entry in one step. It is refused while the mock has a draft that is not
deployed (deploying it would put other changes live). If the security is turned off
later, the item stays closed and `GET /_rustybin/portal` warns that its calls answer
without credentials.

```hurl
{{#include ../examples/portal/portal.hurl:security_off}}
```

The other kinds of item (`mcp:*`, `llm`, `a2a:*`, `tools`, `registry`) are built-in
endpoints: closing one hides its credentials and connection snippets (and the tools'
examples), but the endpoints stay callable, except `mcp:protected` and `mcp:apikey`,
which already refuse a call without their credentials. The publish dialog says which.

### Portal logins

A tester gets a login that opens the portal and nothing in the console: invite them
with the `portal_reader` role (Management, **Users and roles**, **Invite a portal
reader**; `rustybin users invite <name> --role portal_reader`; the `invite_user` tool;
`users:` of `rustybin.yaml`). The invitation links to the portal's join page,
`/portal/#/join/<token>`, where they choose a password and land on the portal signed
in. Every built-in role holds `portal.read`, so any user reads closed items; a
`portal_reader` holds nothing else. See [users and roles](users.md).

```hurl
{{#include ../examples/portal/access.hurl:close}}
```

```hurl
{{#include ../examples/portal/access.hurl:invite_reader}}
```

```hurl
{{#include ../examples/portal/access.hurl:join}}
```

The **Sign in** button in the portal's header posts the user name and password to
`/portal/api/login`, which checks them exactly as the console sign-in does (the same
throttle, shared with `/_rustybin/login`: 5 failures per address and user name a
minute, 20 at one user name from every address together; a disabled user answered as a wrong
password) and sets the `rustybin_portal` cookie: `HttpOnly`, `SameSite=Strict`,
`Secure` behind HTTPS, `Path=/portal`, 12 hours. It is accepted under `/portal` only,
never by the control plane, the data plane or `/_rustybin/mcp`. Permissions are
resolved at every request, so a role removed or a user disabled loses the closed items
on the next one; **Sign out** (`POST /portal/api/logout`) ends the session, and so do
"sign out everywhere" and disabling the user. `GET /portal/api/session` says who is
signed in.

```hurl
{{#include ../examples/portal/access.hurl:signed_out}}
```

```hurl
{{#include ../examples/portal/access.hurl:signed_in}}
```

```hurl
{{#include ../examples/portal/access.hurl:logout}}
```

```hurl
{{#include ../examples/portal/access.hurl:portal_login}}
```

A `portal_reader` who signs in to the console is told the account opens the API portal
and sent there, signed in (`/_rustybin/login` answers `portal_only` and sets the
portal's cookie; the console does not keep its token):

```hurl
{{#include ../examples/portal/access.hurl:console_login}}
```

### Insights

The portal counts, per item, views per day (30 days), downloads by format and Try it
calls. No identifier of a visitor is stored: counts only, bounded, saved in
`<state>/portal.json` with the published items (at most every 30 seconds, so a
restart may lose the last few counts). **Management, Portal** shows them next to the
items; `GET /_rustybin/portal/insights` returns them:

```hurl
{{#include ../examples/portal/portal.hurl:insights}}
```

Without `RUSTYBIN_STATE_DIR` published items and counts live in memory and are lost on
a restart; `portal.items` in `rustybin.yaml` is the durable way then.

## For testers

Open `/portal`. The home page lists what is published by kind (APIs, MCP servers, LLM,
agents, tools, registry), with a search box and a "new this week" strip (published or
changed in the last seven days). An item's page has everything to start: the base
URL, how to authenticate, **Try it** on every operation or sample (sent from your
browser, through the gateway when the operator set one), copyable curl commands and
connection snippets, and downloads.

The portal reads one endpoint, `GET /portal/api/catalog`: only published items, with
what their pages show. A rendered catalogue is cached; the instance renders a new one
at most 30 times a minute in all (a host name it has not seen, a publish), and past
that a cache miss answers `503` with `Retry-After` while cached catalogues keep being
served. It carries an `ETag`:

```hurl
{{#include ../examples/portal/portal.hurl:catalog}}
```

```hurl
{{#include ../examples/portal/portal.hurl:not_modified}}
```

```hurl
{{#include ../examples/portal/portal.hurl:page}}
```

Downloads come from `/portal/api/items/<id>/collections/<format>`, views and Try it
calls are reported to `/portal/api/events`:

```hurl
{{#include ../examples/portal/portal.hurl:download}}
```

```hurl
{{#include ../examples/portal/portal.hurl:reference}}
```

```hurl
{{#include ../examples/portal/portal.hurl:event}}
```

## Routes

| Route | |
|---|---|
| `GET /portal`, `GET /portal/`, `GET /portal/{*path}` | The portal (embedded in the binary, no CDN; strict CSP) |
| `GET /portal/api/catalog` | The published items |
| `POST /portal/api/events` | `{item, event: "view" \| "try"}` |
| `GET /portal/api/items/{id}/collections/{format}` | A published mock as a collection |
| `GET /portal/reference/{id}` | A published mock's spec in the API reference viewer (pinned and sandboxed like `/docs`, the spec inlined) |
| `POST /portal/api/login`, `POST /portal/api/logout`, `GET /portal/api/session` | The portal sign-in: the `rustybin_portal` cookie |
| `GET /_rustybin/portal` | The console's view: settings, every publishable item, why a published one is hidden, its counters |
| `PUT`, `DELETE /_rustybin/portal/items/{id}` | Publish or update (with `access`, and `secure` to turn a mock's security on to close it), unpublish |
| `GET /_rustybin/portal/insights` | The counters |

`/portal/*` is never captured by the inspector, ignores fault-injection headers, and is
exempt from the key layer (the portal checks `portal.auth` and each item's access
itself). `GET /_rustybin/portal` answers each item's `access`, whether a mock is
`secured`, the number of `closed` items and the `readers` (enabled users holding
`portal.read`, and those whose login opens only the portal).

```hurl
{{#include ../examples/portal/portal.hurl:items}}
```

## On every surface

The same job from the API, the console, the command line, an agent and the configuration document.

| Surface | Where |
|---|---|
| API | `GET /_rustybin/portal`, `PUT` (with `access` and `secure`), `DELETE /_rustybin/portal/items/{id}`, `GET /_rustybin/portal/insights`; what testers read: `/portal`, `/portal/api/catalog`, `/portal/api/items/{id}/collections/{format}`, `/portal/reference/{id}`, the sign-in `/portal/api/login`, `/logout`, `/session` |
| Console | **Management, Portal** (status, branding, items with a lock on closed ones, the reader count, insights, Preview), **Publish to portal** on a mock's page (Open or Closed, **Turn on security and close**), **Management, Users and roles**, **Invite a portal reader**; the portal itself: **Sign in**, the lock, **Sign out**, the join page |
| CLI | `rustybin portal status \| publish <id> [--access open\|closed] [--secure] \| unpublish <id> \| insights \| branding \| open`, `rustybin users invite <name> --role portal_reader` ([reference](cli.md#portal)) |
| Management MCP | `list_portal_items` (each item's `access`), `publish_portal_item` (`access`, `secure`), `unpublish_portal_item`, `set_portal_branding`, `get_portal_insights`, `invite_user` with the `portal_reader` role; the prompt `publish_portal`; the resource `rustybin://portal/catalog` ([agent page](agent-mcp.md)) |
| `rustybin.yaml` | `portal.items` (the published items with their metadata and `access`; a pulled document may close, never open), `settings: portal.*` ([the whole instance as code](configuration-as-code.md)) |
| Environment | `RUSTYBIN_PORTAL`, `RUSTYBIN_PORTAL_AUTH`, `RUSTYBIN_PORTAL_BASE_URL`, `RUSTYBIN_PORTAL_VISITORS` ([configuration](configuration.md#dev-portal)) |
