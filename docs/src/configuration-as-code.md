# The whole instance as code

Everything an instance is can be exported into one `rustybin.yaml` (version 2) that
is committed, diffed and reviewed; applied to an instance with a dry run first; loaded
at startup; and kept in a git repository from the console (see
[Source control](source-control.md)). The mocks-only file of earlier releases
([Configuration as code for mocks](mocks/config.md)) is a version 1 document, still
loaded and still exported by the mocks routes; a version 2 document holds the mocks
sections plus the rest of the instance.

## The document

```yaml
version: 2
instance:
  title: Payments team mocks        # the setting console.title
  console: { user: payments }       # console.user; the password is never here
settings:                           # stored, non-secret settings by dotted key
  agents.default_ceiling: manage
  portal.enabled: "on"
secrets:                            # references only, never values
  assist.openai_key: ${RUSTYBIN_ASSIST_OPENAI_KEY}
  leads.webhook_token: secret:stored
mocks:                              # as in a version 1 file
  - name: orders
    spec: specs/orders.yaml         # next to the document (a bundle), or spec_inline
    responses: { mode: examples }
registry:
  servers: [...]                    # server.json documents of the mock MCP registry
portal:
  items: [{ id: "mock:orders", title: Orders API, tags: [payments] }]
harness:
  scenarios: [{ name: smoke, scenario: { ... } }]   # saved scenarios (harness mode)
agents:
  default_ceiling: traffic
  rules: [{ match: { name: "ci-*" }, ceiling: read-only }]
health:
  initial: healthy                  # the /health toggle
```

| Section | What it holds | Applied through |
|---|---|---|
| `instance` | `title` (the `console.title` setting) and `console.user` | the settings store |
| `settings` | Non-secret settings by dotted key, as `GET /_rustybin/settings` lists them; `null` unsets the stored value | the settings store |
| `secrets` | Secret settings as references: `${VAR}` reads the variable from the environment when the document is loaded or applied, `secret:stored` keeps what the instance holds, `null` unsets it; a value that resolves to nothing is kept as it is and reported | the settings store |
| `mocks` | The mocks, each with its spec inline (`spec_inline`) or as a file next to the document (`spec`), exactly the [version 1 entries](mocks/config.md#reference) | the mock store, as `POST /_rustybin/mocks/apply` |
| `registry` | `servers`: `server.json` documents of the [mock MCP registry](mcp/registry.md) | the registry's file copy (entries published through the API that the document names move into it) |
| `portal` | `items`: what the [dev portal](portal.md) publishes | the portal (a mock the item names must exist; a `mock:` item of a visitor is skipped) |
| `harness` | `scenarios`: the [saved scenarios](harness.md), inline, validated against the node's caps | the harness (a problem when harness mode is off) |
| `agents` | The default ceiling and the rules of [AI agents](agent-mcp.md#managing-agents) | the agents policy |
| `health` | `initial`: `healthy` or `unhealthy` | the `/health` toggle |

Two rules make a document safe to apply:

- **A section that is absent is left alone.** A document with only `health` changes
  nothing else. An export always writes every section (empty lists where there is
  nothing), so an export is the whole instance.
- **With `prune`, a present section is the whole truth for its part of the
  instance.** API-created mocks, published portal items, published registry entries,
  saved scenarios and stored settings or secrets that the section does not name are
  removed. Without `prune`, applying only adds and updates. The `git.*` settings are
  never exported nor pruned: where an instance keeps its configuration is the
  deployment's business.

Mocks applied through the API or a repository are `origin: api` (saved in the mocks
state directory, as console changes are); the ones a `RUSTYBIN_CONFIG_FILE` declares
stay `origin: file`. The built-in `petstore` sample and visitors' self-service mocks
are never exported. The mocks' demo credentials (API keys, basic users, bearer tokens)
are part of the mocks and exported with them unless `secrets=omit` is asked; a secret
of the instance (an AI key, a webhook token, a password hash) never is.

The JSON Schema of the document is `GET /_rustybin/config/schema` (and the management
MCP resource `rustybin://config.schema.json`):

```hurl
{{#include ../examples/config/config.hurl:schema}}
```

## Export

`GET /_rustybin/config/export` (admin) answers the instance as YAML. `?format=json`
gives the same document as JSON; `?secrets=omit` leaves the `secrets` section out and
redacts the mocks' demo credentials.

```hurl
{{#include ../examples/config/config.hurl:export}}
```

```hurl
{{#include ../examples/config/config.hurl:export_json}}
```

`?format=bundle` answers a gzipped tar: `rustybin-config/rustybin.yaml` with every
spec as a file under `rustybin-config/specs/` (`spec: specs/orders.json`), the shape a
repository holds and a reviewer diffs. Unpacked, the directory loads with
`RUSTYBIN_CONFIG_FILE=rustybin-config/rustybin.yaml`.

In the console, **Management, Export** shows the document with **Copy**, **Download**
and **Bundle**, the differences since the last export seen in that browser, and
**Apply a document** below.

## Apply, with a dry run first

`POST /_rustybin/config/apply` (admin) takes `{document, prune, dry_run}`: the
document as YAML or JSON text, or as a JSON object. The answer carries the diff
computed before anything changed (`changes`, and `summary` as lines such as
`mocks.orders: spec changed (1 operation(s) added), disabled`), each section's result
under `results`, and `problems` (an entry that could not be applied never stops the
others; `ok` is false when there were any).

A dry run answers the same and changes nothing:

```hurl
{{#include ../examples/config/config.hurl:dry_run}}
```

Then the same document, applied:

```hurl
{{#include ../examples/config/config.hurl:apply}}
```

Spec file paths (`spec:`) are only read next to a local file (`RUSTYBIN_CONFIG_FILE`,
a repository's working copy): a document sent to the API must inline its specs
(`spec_inline`), as `rustybin mock push` does.

`POST /_rustybin/mocks/apply` still accepts a version 2 document and applies its
`mocks` section only.

## At startup and on reload

`RUSTYBIN_CONFIG_FILE` loads a version 1 or 2 document before the instance serves its
first request and again on `SIGHUP` or `POST /_rustybin/mocks/reload`. The mocks,
registry entries, portal items and agent rules are the file's (`origin: file`, the
file is the truth for those sections and a reload prunes what it no longer names);
the instance, settings, secrets, scenarios and health are applied without pruning.
`${VAR}` references in the file are expanded from the environment when it is read, so
a secret reference becomes the value and is stored sealed. Problems never stop the
server: they are logged and listed in `GET /_rustybin/mocks` (`load_errors`).

The registry and agents sections applied through the API or a repository are kept in
`<state>/config.json` (with `RUSTYBIN_STATE_DIR`) and loaded at startup, before the
config file, which wins for the sections it carries.

[Source control](source-control.md) connects an instance to a git repository: commit
and push the export, pull and apply the repository's version, and start from the
repository with `RUSTYBIN_CONFIG_GIT`.
