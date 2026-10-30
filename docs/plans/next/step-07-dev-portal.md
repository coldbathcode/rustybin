# Step 07: Dev portal: publish and consume

Master plan: [master-plan.md](master-plan.md). Needs: 01, 02. Status: **done 2026-10-30**. About 3 days.

## Goal

Someone runs a Rustybin as the team's mocking service. Testers come to `/portal`, see what has
been published (mock APIs, their MCP servers, the mock LLM, the MCP and A2A servers, the registry),
read the docs, try requests, download collections and connection snippets, and never need the
console or its password. The operator publishes and unpublishes from the console, the CLI, the MCP
server or `rustybin.yaml`, and sees what testers look at.

## Read first

- `src/ui.rs` and `build.rs` (how `ui/` is embedded and served under `/ui`), `ui/index.html`,
  the shell in `ui/app.js` (what to reuse: `dom.js`, `format.js`, `http.js`; what not to: auth).
- `src/openapi.rs` and the `/docs` Scalar page (the spec viewer to embed per mock), the served
  spec of a mock (`get_mock_spec` with `served: true`), `src/mocks/collections.rs` (collection
  exports), `src/catalog.rs` (the catalogue and its categories).
- `src/a2a/` agent cards, `src/mcp/mod.rs` catalogue entries, `docs/src/ai/providers.md`.
- `src/mocks/selfservice.rs` and `visits.rs` (owners: who may publish what), `src/landing.rs`
  (HTML escaping, the existing public page).
- Step 01's settings store, step 02's auth (the portal's own setting), step 06's registry.

## Design

### Publishing

- `Published { title, summary, tags, contact, order, published_at, by }` on: mocks (`mocks::model`
  settings, persisted with the mock), the mock LLM, the MCP servers (`/mcp` and its variants, the
  registry), the A2A agents, the echo and testing routes as a group ("HTTP tools"). Every item has
  a stable portal id (`mock:petstore`, `mcp:main`, `llm`, `a2a:travel-planner`, `tools`).
- `PUT /_rustybin/portal/items/{id}` (manage for a mock, admin for the rest) publishes with the
  metadata, `DELETE` unpublishes, `GET /_rustybin/portal/items` lists everything with its state.
  Bulk publish in the console. Visitors on a self-service instance may publish their own mocks only
  when `RUSTYBIN_PORTAL_VISITORS=true` (their items carry a visitor badge and expire with the visit).
- `rustybin.yaml` `portal:` section (step 09 formalises it): branding and the published items, so a
  portal is reproducible from the repo.

### The portal (`/portal`, `ui-portal/`)

A second small SPA embedded like the console (`ui-portal/`, same `build.rs` mechanism, served by
`src/portal.rs` under `/portal` and `/portal/*`), with no sidebar and a reader's layout:

- **Home**: the instance's portal title and intro (markdown, rendered safely), a search box, cards
  by kind: APIs, MCP servers, LLM, Agents, Tools, Registry.
- **An API**: the summary, the base URL, security instructions with the demo credentials the mock
  accepts (from mocked security; never a real secret), the spec rendered with Scalar (the same
  `/docs` viewer pointed at the mock's served spec), **Try it** (requests from the portal to the
  mock with the response shown; through the gateway URL if the operator set one, so testers go the
  way real clients go), example coverage per operation, downloads (Postman, Hurl, curl, `.http`,
  k6, Bruno, Insomnia from `collections.rs`), the mock's MCP server with `claude mcp add` and the
  `mcpServers` JSON, the registry entry.
- **An MCP server**: tools with schemas and examples (step 04), connection snippets, the auth it
  expects, "test it with the inspector" when the console is reachable.
- **The LLM**: providers and paths, a sample curl per provider, the pacing headers.
- **Agents**: the cards, a sample send.
- **Tools**: the catalogue's categories for the echo, status, auth and reliability routes, with
  the examples from the catalogue.
- **A "what's new" strip**: items published or changed in the last 7 days.

The portal is read-only, has no AI, and reads one endpoint: `GET /portal/api/catalog` (public,
only published items, cached, ETag) plus the public spec and collection routes of each mock.
`RUSTYBIN_PORTAL=on|off` (default on when at least one item is published), `RUSTYBIN_PORTAL_AUTH=
open|password|key` (`password`: the console's sign-in page in portal clothes, `key`: an API key
header for shared-mode instances), `RUSTYBIN_PORTAL_BASE_URL` (what the snippets print when the
portal is behind a gateway).

### Insights

A bounded counter per item and page (views per day for 30 days, downloads by format, try-it calls),
in memory and in `<state>/portal.json`, shown in Management, Portal as a small table ("the orders
API: 41 views this week"). No identifiers of visitors are stored.

### The console

Management, Portal: branding (title, intro, accent colour token, contact), auth mode, the gateway
URL testers should use, the list of items with publish toggles and metadata forms, insights, and a
"Preview the portal" link. On a mock's detail page: a **Publish to portal** button with the
metadata form, and a portal badge in the mocks list. The assist bar suggests "Write the portal
summary for this mock" (a `summarise_for_portal` task).

### CLI and MCP notes

Step 10: `rustybin portal list|publish <id> --title ...|unpublish|branding|insights`. Step 11:
`list_portal_items`, `publish_portal_item` (confirmed when the portal is open to the world),
`unpublish_portal_item`, `set_portal_branding`, prompt `publish_portal`.

## Tests

- Unit: the catalogue of published items, escaping of every operator-written string, counters'
  bounds, the auth modes.
- Full app: publish and unpublish each kind, the public catalog endpoint only shows published
  items, the portal's static files and the API behind `password` and `key`, a visitor publishing
  only with the switch, the catalogue tests cover the new routes.
- Console check: publish a mock from its page, open the portal in a second context without
  credentials, try a request, download a collection, read an MCP server's page.

## Docs

`docs/src/portal.md` (new: for operators and for testers), `console.md`, `configuration.md`,
`mocks/index.md` cross-link, the README's feature list, Hurl examples, the changelog.

## Done when

A published mock, its MCP server, the LLM and an agent are explorable and tryable at `/portal`
without credentials (or behind the chosen auth), from the catalog endpoint only; insights count;
all checks pass; committed and pushed.

## Build notes

Built 2026-10-30 (`src/portal/`, `ui-portal/`, `ui/lib/portal.js`, the Portal tab of
`ui/views/manage.js`, `docs/src/portal.md`, `docs/examples/portal/portal.hurl`). Where the build
differs from the design above, and why:

- **One store, not the mock settings.** `Published` lives in `src/portal/` (`AppState.portal`,
  `<state>/portal.json` with the insights), for every kind alike: publishing a mock must not make
  a draft revision or need a deploy. A mock's entry records the mock's owner: a visitor's entry
  leaves with the visit (swept), another owner's mock under the same name is not published by
  it, and an instance mock deleted and created again keeps its entry (shown "hidden: the mock
  does not exist" until then). A mock that is not deployed is hidden, and `GET
  /_rustybin/portal` says why.
- **Ids.** `mock:<name>`, `mcp:main|protected|apikey|sse|weather|crm|devtools` (the management
  server is never publishable), `llm`, `a2a:<agent>`, `tools` (a fixed list of catalogue
  categories: echo, status, shaping, streaming, redirects, info, auth, GraphQL, SOAP, WebSocket,
  reliability), `registry`. At most 256 items.
- **The spec viewer.** The portal's own CSP allows scripts from itself only (no CDN, like the
  console), so an API's page renders the operations itself (grouped by tag, Try it, curl) and
  links the Scalar viewer as a separate page, `/portal/reference/{id}` (the `/docs` page pointed
  at the mock's served spec). That page is not behind `portal.auth`: a page load carries no
  header and the spec is public on the data plane anyway.
- **Reuse.** The portal imports the console's `dom.js`, `format.js` and stylesheet from `../ui/`
  (always open); the Markdown renderer moved from `assist.js` to `ui/lib/markdown.js` (both use
  it) and gained `[text](https://...)` links. `build.rs` embeds `ui-portal/` as a second table.
- **Auth.** `password` accepts any control credential (`control_auth::credentials_grant`: the
  console sign-in token, the admin token, a control JWT); the portal's sign-in screen posts to
  `/_rustybin/login` when the password sign-in is on, else asks for a token. `key` accepts a key
  of the registry (`keys::lookup`), which Try it then sends (`X-Rustybin-Key`); Try it never
  sends a control token. The page always loads. `/portal/*` is exempt from the key layer and is
  a control path for the inspector, fault injection, egress and `try_request`.
- **`auto`** also answers `404` for the page itself while nothing is shown.
- **Insights** count views per item per day (30 days), downloads by format (counted where the
  download is served) and Try it calls; not per page as the design says, since an item has one
  page. Views and Try it calls come from the SPA through `POST /portal/api/events` (only for
  shown items, so the counters stay bounded by the published set; 512 items tracked at most).
  Counts reach disk at most every 30 seconds.
- **Bulk publish** is the console calling `PUT` per item; there is no bulk route.
- **`rustybin.yaml`**: `portal.items` (an id plus the metadata), validated on load, reloaded with
  the file; the API answers `409` for an item the file publishes. The branding goes into the
  file with the other settings in step 09.
- **Settings** gained `Kind::Markdown` (newlines kept; a textarea in `settingsform.js`) for
  `portal.intro`. Visitors' manage tokens now also go to `/_rustybin/portal*` (`isMocksUrl`).
- **AI**: the task `summarise_for_portal` (title, summary, tags, checked with
  `portal::clean_meta`) behind **Write with AI** in the publish dialog, and a fourth suggestion
  on the mocks view.

CLI and MCP notes for later steps: step 10's `rustybin portal list | publish <id> [--title
--summary --tag --contact --order --no-credentials] | unpublish <id> | branding | insights` call
`GET /_rustybin/portal`, `PUT` and `DELETE /_rustybin/portal/items/{id}`, `PATCH
/_rustybin/settings` (`portal.*`) and `GET /_rustybin/portal/insights`. Step 11's
`list_portal_items`, `publish_portal_item` (confirmed when `portal.auth` is `open` and the
portal is on), `unpublish_portal_item` and `set_portal_branding` wrap `items::candidates`,
`Portal::publish` with `api::authorize_item`'s rules, `Portal::unpublish` and the settings
update; the `publish_portal` prompt walks through them. Step 09 exports `portal.items` and the
`portal.*` settings.
