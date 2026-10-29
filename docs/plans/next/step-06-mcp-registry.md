# Step 06: MCP registry: a mock registry and inspecting real ones

Master plan: [master-plan.md](master-plan.md). Needs: 05. Status: **not started**. About 1.5 days.

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

(filled in when built)
