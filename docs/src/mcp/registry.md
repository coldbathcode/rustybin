# MCP registry

Gateways and MCP clients increasingly find servers through an **MCP registry**: a list
of `server.json` entries, each naming a server, its version and the remotes (URLs) or
packages that run it. Rustybin serves a **mock registry** at `/mcp-registry` that lists
the instance's own MCP servers in the registry API's shape, takes entries you add or
publish, and fails on purpose in the ways a registry sync has to survive. The console's
**MCP registry** view also browses a real registry and opens any entry in the
[inspector](inspector.md).

## The mock registry

The API is the registry's `/v0.1` (the version frozen in 2025); `/mcp-registry/v0`
answers the same. Point a gateway's registry sync at `https://<host>/mcp-registry`.

| Route | |
|---|---|
| `GET /mcp-registry/v0.1/servers` | One record per server version, ordered by name then version; `cursor`, `limit` (1 to 100, default 30), `search` (a substring of the name, at least 3 characters), `updated_since` (RFC 3339), `version` (`latest` or an exact one), `include_deleted` |
| `GET /mcp-registry/v0.1/servers/{serverName}/versions` | Every version of a server; the name is URL-encoded (`io.rustybin%2Fmcp`) |
| `GET /mcp-registry/v0.1/servers/{serverName}/versions/{version}` | One version; `latest` for the latest |
| `POST /mcp-registry/v0.1/publish` | Publish a `server.json` with a registry token |
| `PATCH /mcp-registry/v0.1/servers/{serverName}/versions/{version}/status`, `PATCH /mcp-registry/v0.1/servers/{serverName}/status` | `active`, `deprecated` or `deleted`, with an optional `statusMessage` (published entries) |
| `POST /mcp-registry/v0.1/validate` | `{valid, issues}` |
| `POST /mcp-registry/v0.1/auth/{method}` | Token stubs: `github-at`, `github-oidc`, `oidc`, `dns`, `http`, `none` |
| `GET /mcp-registry/v0.1/health`, `ping`, `version` | As the registry has them |

Errors are problem documents (`application/problem+json`: `title`, `status`,
`detail`, `errors`), as the registry answers them.

```hurl
{{#include ../../examples/mcp/registry.hurl:list}}
```

Follow `metadata.nextCursor` with the same filters:

```hurl
{{#include ../../examples/mcp/registry.hurl:next_page}}
```

```hurl
{{#include ../../examples/mcp/registry.hurl:search}}
```

### Entries

- **Generated** for every MCP server of the instance, in the `io.rustybin` namespace:
  `io.rustybin/mcp` and its variants (`mcp-protected`, `mcp-apikey`, the named
  servers, `mcp-sse` with an `sse` remote), `io.rustybin/manage` for the
  [management server](../agent-mcp.md), and `io.rustybin/mock-<name>` for each mock
  whose [MCP server](../mocks/mcp.md) is serving (deployed, on, MCP enabled; visitor
  mocks are never listed). The remote URLs use the request's origin, so a gateway in
  front of the instance sees URLs it can reach (with `RUSTYBIN_TRUST_FORWARD`, the
  forwarded host). The headers a remote needs (the API key header, `Authorization`)
  are declared on it, and `_meta` carries the instance path under
  `io.modelcontextprotocol.registry/publisher-provided`. A mock's version is its
  spec's `info.version`; the others carry the Rustybin version.
- **From the config file**: `registry.servers` in [`rustybin.yaml`](../mocks/config.md),
  validated when the file loads (an invalid entry is skipped with a warning) and
  reloaded with it.
- **Published** through the API.

File and published entries together are bounded (256). The `io.rustybin` namespace is
reserved for the generated entries; a version can be published once.

```hurl
{{#include ../../examples/mcp/registry.hurl:get}}
```

```yaml
version: 1
mocks: []
registry:
  servers:
    - $schema: https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json
      name: io.example/weather
      description: Forecasts and alerts for any city
      version: 1.0.0
      remotes:
        - type: streamable-http
          url: https://weather.example.com/mcp
```

### Publishing

A publish needs `Authorization: Bearer <registry token>`. The `auth/*` stubs take
the bodies the registry defines (`github-at`: `{github_token}`; `github-oidc` and
`oidc`: `{oidc_token}`; `dns` and `http`: `{domain, timestamp, signed_timestamp}`;
`none`: `{}`) and always succeed: nothing is verified. Tokens last five minutes.

```hurl
{{#include ../../examples/mcp/registry.hurl:token}}
```

```hurl
{{#include ../../examples/mcp/registry.hurl:publish}}
```

The document is checked against the vendored `server.json` schema
(`https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json`) and
the registry's semantic checks: both name parts start and end with a letter or digit,
the version is exact (not `^1.0`, `>=1`, `1.x` or `latest`), `$schema` is present. A
schema violation answers `422`, a semantic one `400`, each with the failing locations.
A remote on `localhost` is a warning here (the public registry refuses it).

```hurl
{{#include ../../examples/mcp/registry.hurl:publish_invalid}}
```

Published versions change status with the same token; a deleted version leaves the
list unless `include_deleted=true`, and always shows to `updated_since` (that is how
a sync learns about deletions):

```hurl
{{#include ../../examples/mcp/registry.hurl:deprecate}}
```

In public mode (`RUSTYBIN_PUBLIC_MODE`) publishing and status changes also need the
admin token as `X-Rustybin-Admin-Token`.

### Scenarios

The `registry.scenario` setting (Management, Network, or the MCP registry view;
seeded by `RUSTYBIN_REGISTRY_SCENARIO`) is what a gateway's registry sync must survive:

| Scenario | The registry |
|---|---|
| `normal` | Every entry, answered at once |
| `empty` | No entries at all |
| `large` | 2000 synthetic entries on top (`io.example.synthetic/server-0001` ...), every tenth with two versions and every fifth with a package: pagination and `version=latest` |
| `flaky` | Every third list request answers `503` |
| `slow` | Every answer waits 2 seconds |
| `stale` | Every entry last updated on 2024-01-15 (`updated_since` finds nothing new) |
| `invalid` | One extra entry that violates the schema (no namespace, no description, an unknown transport) |
| `auth_fail` | Every token exchange, publish and status change answers `401` |

```hurl
{{#include ../../examples/mcp/registry.hurl:scenario}}
```

### Validation and the instance's view

`POST /_rustybin/registry/validate` checks any `server.json` (the console's publish
form calls it as you type). `GET /_rustybin/registry` is the console's view of the
mock registry: every entry with its source (`generated`, `file`, `published`,
`scenario`) and validity, the counts, the scenario and a sync snippet in generic JSON.
`DELETE /_rustybin/registry/servers/{serverName}` (admin, `?version=` for one) removes
published entries.

```hurl
{{#include ../../examples/mcp/registry.hurl:validate}}
```

```hurl
{{#include ../../examples/mcp/registry.hurl:state}}
```

```hurl
{{#include ../../examples/mcp/registry.hurl:delete}}
```

## Browsing a real registry

`GET /_rustybin/registry/browse?url=<registry>&search=&cursor=&limit=&version=` fetches
one page of a registry through the instance and answers its entries with their
remotes (type, URL, the headers they declare and the auth that implies), packages and
validity. `url` is a registry base (`https://registry.modelcontextprotocol.io`, the
default; `/v0.1` is appended unless the URL ends in `/v0.1` or `/v0`) or
`/mcp-registry` for this instance (in process). Pages are cached for a minute.

Outbound calls follow the [egress policy](../configuration.md#outbound-connections)
under the purpose `registry`: `registry.modelcontextprotocol.io` and the hosts of the
**saved registries** (`registry.saved`, seeded by `RUSTYBIN_REGISTRY_SAVED`) are
allowed; anything else needs `RUSTYBIN_EGRESS_ALLOW`, and a registry on this machine
the loopback switch.

```hurl
{{#include ../../examples/mcp/registry.hurl:browse}}
```

## In the console

**MCP registry** (in "AI and agents") has two tabs:

- **Mock registry**: the registry URL to point a gateway at, **Copy as registry sync
  config** (a generic JSON snippet), the scenario picker, the entries with their source,
  status and validity (each with its `server.json`, **Inspect**, **Explain** and, for
  published ones, remove), and **Publish a server.json**: an editor that validates as
  you type, an auth stub to get the token with, and **Write a server.json for my mock**
  (AI assist: the generated entry of a mock and its tools, rewritten by your provider and
  validated against the schema before it reaches the editor).
- **Browse a registry**: the public registry, this instance's mock one, a saved
  registry or any URL (**Save this registry** adds it to `registry.saved`), search,
  latest or every version, pages; each entry with its remotes and packages,
  **Validate**, its `server.json`, **Explain this server entry**, and **Open in
  inspector** per remote, which opens the [inspector](inspector.md) on it with the auth
  the remote declares (through the relay; a remote on this instance opens as one of
  its servers).

Later steps add the CLI (`rustybin registry list | get | publish | validate | scenario |
browse`) and the management MCP tools (`list_registry_servers`, `set_registry_scenario`,
`browse_registry`, `validate_server_json`).
