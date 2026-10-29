# Step 07: Dev portal: publish and consume

Master plan: [master-plan.md](master-plan.md). Needs: 01, 02. Status: **not started**. About 3 days.

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

(filled in when built)
