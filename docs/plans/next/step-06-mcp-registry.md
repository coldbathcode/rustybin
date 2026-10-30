# Step 06: MCP registry: a mock registry and inspecting real ones

Master plan: [master-plan.md](master-plan.md). Needs: 05. Status: **done 2026-10-30**. About 1.5 days.

## Goal

Rustybin serves a mock MCP registry that lists its own MCP servers (and entries the user adds) in
the registry API shape, so gateways and clients that discover servers through a registry can be
tested against it, with scenarios for the awkward cases. And the inspector can browse a real
registry, open any entry in the inspector and validate `server.json` documents.

## Read first

- The MCP registry API and `server.json` schema (vendored under `src/mcp/registry/schema/` with
  the version noted in a comment; the published OpenAPI of the registry's `/v0` API is the source).
- `src/mcp/openapi.rs` and `src/mocks/mcp.rs` (how a mock's server describes itself),
  `src/mcp/manage/mod.rs` (`catalog`), step 05's `GET /_rustybin/mcp-servers`.
- `src/mocks/config_file.rs` (a config section pattern) and step 09's plan for the `registry:` section.
- `src/egress.rs` (`Purpose::Registry`).

## Design

### The mock registry (`src/mcp/registry/`)

Data plane routes under `/mcp-registry`:

| Route | |
|---|---|
| `GET /mcp-registry/v0/servers` | List with `cursor` and `limit` pagination, `search`, `updated_since`, `version` filters, as the registry API defines them |
| `GET /mcp-registry/v0/servers/{id}` | One entry (`server.json` plus registry metadata) |
| `GET /mcp-registry/v0/servers/{id}/versions` | Versions |
| `POST /mcp-registry/v0/publish` | Accepts a `server.json` under the registry's auth shapes (bearer from `/mcp-registry/v0/auth/*` stubs: none, GitHub-style token, DNS or HTTP challenge stubs that always succeed, or always fail by scenario) |
| `GET /mcp-registry/v0/health`, `/v0/ping` | As the API has them |
| `GET /.well-known/mcp-registry` | Discovery, when the API defines it; else omitted |

- **Entries**: generated for every server of the instance (the `/mcp` variants, the management
  server as `io.rustybin/manage`, each deployed mock as `io.rustybin/mock-<name>` with a `remotes`
  entry pointing at `/mock/<name>/_rustybin/mcp` under the instance's base URL), plus user entries
  (`rustybin.yaml` `registry.servers`, and `POST .../publish`), bounded (256), published entries
  flagged by step 07's portal.
- **Scenarios** (`registry.scenario` setting: `normal`, `empty`, `large` (2000 synthetic entries for
  pagination tests), `flaky` (every third list answers 503), `slow` (2 s), `stale` (entries with old
  `updated_at`), `invalid` (one entry violating the schema)): what a gateway's registry sync must
  survive.
- Validation: `server.json` documents are validated against the vendored schema on publish and
  on `POST /_rustybin/registry/validate`.

### Inspecting a real registry

`GET /_rustybin/registry/browse?url=<registry base>&search=&cursor=` (admin or console scope) fetches
through the egress client (`Purpose::Registry`, default allow `registry.modelcontextprotocol.io`,
others through `RUSTYBIN_EGRESS_ALLOW`), caches pages for a minute (bounded), and answers the
entries with a `remotes` list the inspector can connect to. Saved registries (settings store).

### The console

A view **MCP registry** in "AI and agents": tab **Mock registry** (the entries, the scenario
picker, a "server.json" viewer per entry with copy, the publish form that validates as you type,
the base URL to point a gateway at, a "copy as registry sync config" snippet in generic JSON),
tab **Browse a registry** (URL or a saved one, search, entries with their remotes and packages,
"Open in inspector" which hands the remote URL and auth hints to step 05's view, "Validate" on any
entry), and sparkle actions "Explain this server entry" and "Write a server.json for my mock".

### CLI and MCP notes

Step 10: `rustybin registry list|get|publish|validate|scenario`, `rustybin registry browse <url>`.
Step 11: `list_registry_servers`, `set_registry_scenario`, `browse_registry`, `validate_server_json`.

## Tests

- Unit: the generated entries, pagination cursors, every scenario, schema validation.
- Full app: the registry routes against a deployed mock, publish with each auth stub, the browse
  route against a second in-process instance's mock registry (loopback switch on).
- Console check: the two tabs, open an entry in the inspector.
- Hurl examples under `docs/examples/mcp/registry.hurl`.

## Docs

`docs/src/mcp/registry.md` (new), the MCP index, `console.md`, `configuration.md`, the changelog.

## Done when

A gateway can be pointed at `/mcp-registry` and list, page, fetch and publish; scenarios break it on
purpose; the console browses a real registry and opens entries in the inspector; all checks pass;
committed and pushed.

## Build notes

Built 2026-10-30 (`src/mcp/registry/`, `ui/views/registry.js`, `docs/src/mcp/registry.md`). Where
the build differs from the design above, and why:

- **The routes follow the live registry API** (1.8.6, checked on 2026-10-30), not the table
  above: the API was frozen as `/v0.1` and addresses servers by name, so it is `GET
  /v0.1/servers`, `GET /v0.1/servers/{serverName}/versions` and
  `.../versions/{version}` (`latest` for the latest; the name URL-encoded), with `/v0`
  answering the same (a nested alias, so path parameters are not carried over). Added because
  the API has them: `POST /v0.1/validate`, `GET /v0.1/version`, and the two status `PATCH`
  routes (`active`, `deprecated`, `deleted`, published entries only), which is how a sync
  learns about deletions (`include_deleted`, and `updated_since` always includes them).
  `GET /mcp-registry` describes the registry. There is no `/.well-known/mcp-registry`: the
  registry does not define one. Errors are problem documents, as the registry answers them.
- **The schema** is the 2025-12-11 `server.schema.json` the registry serves in `$schema`,
  vendored in `src/mcp/registry/schema/` and checked with the mocks' validator
  (`mocks::schema::validate`, which resolves its `#/definitions` refs), plus the registry's
  semantic checks (name parts start and end with a letter or digit, exact versions, `$schema`
  present). A remote on `localhost` is a warning, not an error as on the public registry: the
  mock registry's own entries point at a local instance in demos. Schema errors answer `422`,
  semantic ones `400`.
- **Auth stubs and the failure scenario.** `auth/{github-at, github-oidc, oidc, dns, http,
  none}` check the body's required fields and always succeed; failing "by scenario" is an
  eighth scenario, `auth_fail`, which makes every exchange, publish and status change answer
  `401`. Tokens last five minutes (at most 256). In public mode writes also need the admin
  token as `X-Rustybin-Admin-Token` (`Authorization` carries the registry token).
- **Entries.** Generated entries use the request's origin for their remotes, declare the
  headers each auth needs, and carry the instance path in `_meta` under
  `io.modelcontextprotocol.registry/publisher-provided` (the console opens them in the
  inspector by path). Visitor and API-key mocks are never listed. A mock's version is its
  spec's `info.version`, the others the crate version. `io.rustybin` is reserved. Published
  entries live in memory (lost on a restart); the config file's `registry.servers` are the
  durable way. "Published entries flagged by the portal" is left to step 07.
- **`rustybin.yaml`** takes a `registry: { servers: [...] }` section now (validated on load,
  bad entries skipped with a load error, reloaded with the file; kept on the `MockStore`).
  `registry.scenario` in the file and the export of `registry.servers` come with step 09,
  which puts settings and the rest of the instance in the file.
- **Settings**: `registry.scenario` (a new `settings::Kind::Choice`, rendered as a select by
  `settingsform.js`) and `registry.saved` (seeded by `RUSTYBIN_REGISTRY_SCENARIO` and
  `RUSTYBIN_REGISTRY_SAVED`), on Management, Network. The saved registries' hosts join
  `registry.modelcontextprotocol.io` in the `Purpose::Registry` rule (recomputed at every
  browse).
- **Control plane**: `GET /_rustybin/registry` (the console's view, with each entry's source
  and validity and the sync snippet), `DELETE /_rustybin/registry/servers/{serverName}`
  (admin), `POST /_rustybin/registry/validate` (a read-only POST for the console scope) and
  `GET /_rustybin/registry/browse` (`/v0.1` appended unless the URL ends in a version; a
  `/mcp-registry` path in process; 64 pages for a minute, 4 MiB a page).
- **Console**: the publish form gets its token from the chosen auth stub in the browser, as a
  client would. Open in inspector hands a remote over as
  `#/mcp/url/<auth>/<header>/<modern|legacy>/<url>` (through the relay, with the auth its
  headers declare), or `#/mcp/server/<path>` for this instance's servers. AI: the task
  `server_json` (the generated draft and the mock's tools in, a document validated against the
  schema out) and the explain kind `server_entry`; suggestions for the `registry` view.

CLI and MCP notes for later steps: step 10's `rustybin registry list | get | publish | validate |
scenario | browse` call `/mcp-registry/v0.1/*` (publish through `auth/none`),
`/_rustybin/registry/validate`, the `registry.scenario` setting and
`/_rustybin/registry/browse` on the remote instance. Step 11's `list_registry_servers`,
`set_registry_scenario`, `browse_registry` and `validate_server_json` wrap `all_entries`, the
settings update, the browse handler and `validate::validate`; publishing from MCP needs
confirmation (it changes what every client of the registry sees).
