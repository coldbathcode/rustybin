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
    - id: mcp:main
    - id: llm
```

The branding and access settings below go into the file with the rest of the
instance's settings in a later release.

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

**Who may read it.** `open` serves everyone who reaches `/portal`. `password` asks for
the console sign-in (the portal's sign-in screen posts to `/_rustybin/login`; any
control-plane credential works, such as the admin token or a control JWT). `key` asks
for an API key of the [key registry](concepts/shared-mode.md), which Try it then sends
with every call (`X-Rustybin-Key`), as shared-mode mocks require. The page itself
always loads (it holds no data); the catalogue and the downloads need the credential:

```hurl
{{#include ../examples/portal/portal.hurl:password}}
```

The full API reference page of a mock (`/portal/reference/mock:<name>`) is not behind
`portal.auth`: a page load carries no header, and it shows the mock's spec, which is
public at `/mock/<name>/_rustybin/openapi.json` anyway.

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
what their pages show. It carries an `ETag`:

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
{{#include ../examples/portal/portal.hurl:event}}
```

## Routes

| Route | |
|---|---|
| `GET /portal`, `GET /portal/`, `GET /portal/{*path}` | The portal (embedded in the binary, no CDN; strict CSP) |
| `GET /portal/api/catalog` | The published items |
| `POST /portal/api/events` | `{item, event: "view" \| "try"}` |
| `GET /portal/api/items/{id}/collections/{format}` | A published mock as a collection |
| `GET /portal/reference/{id}` | A published mock's spec in the API reference viewer (loads it from a CDN, like `/docs`) |
| `GET /_rustybin/portal` | The console's view: settings, every publishable item, why a published one is hidden, its counters |
| `PUT`, `DELETE /_rustybin/portal/items/{id}` | Publish or update, unpublish |
| `GET /_rustybin/portal/insights` | The counters |

`/portal/*` is never captured by the inspector, ignores fault-injection headers, and is
exempt from the key layer (the portal checks `portal.auth` itself).

```hurl
{{#include ../examples/portal/portal.hurl:items}}
```
