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
tests:                              # saved tests: a request and its assertions
  - name: order-1
    request: { path: /mock/orders/orders/1, headers: { x-api-key: "{{X_API_KEY}}" } }
    assert: [{ status: 200 }, { json: $.id, equals: 1 }]
test_schedules:                     # saved tests on a schedule, a webhook for failures
  - name: nightly
    at: "02:30"                     # or every: 15m (5m to 24h)
    prefix: order-                  # or names: [order-1], or neither: every test
    webhook: https://hooks.example.com/rustybin
agents:
  default_ceiling: traffic
  rules: [{ match: { name: "ci-*" }, ceiling: read-only }]
roles:                              # custom roles (the five built-in ones never change)
  - name: catalog-reader
    description: Reads mocks and traffic
    permissions: [mocks.read, traffic.read]
users:                              # never a password: a new user is invited
  - name: alice
    display_name: Alice Doe
    email: alice@example.com
    roles: [editor, catalog-reader]
health:
  initial: healthy                  # the /health toggle
```

| Section | What it holds | Applied through |
|---|---|---|
| `instance` | `title` (the `console.title` setting) and `console.user` (in password mode a document never changes the user name: a different one is a problem) | the settings store |
| `settings` | Non-secret settings by dotted key, as `GET /_rustybin/settings` lists them; `null` unsets the stored value | the settings store |
| `secrets` | Secret settings as references: `${VAR}` reads the variable from the environment when the document is loaded or applied, `secret:stored` keeps what the instance holds, `null` unsets it; a value that resolves to nothing is kept as it is and reported | the settings store |
| `mocks` | The mocks, each with its spec inline (`spec_inline`) or as a file next to the document (`spec`), exactly the [version 1 entries](mocks/config.md#reference) | the mock store, as `POST /_rustybin/mocks/apply` |
| `registry` | `servers`: `server.json` documents of the [mock MCP registry](mcp/registry.md) | the registry's file copy (entries published through the API that the document names move into it) |
| `portal` | `items`: what the [dev portal](portal.md) publishes | the portal (a mock the item names must exist; a `mock:` item of a visitor is skipped) |
| `harness` | `scenarios`: the [saved scenarios](harness.md), inline, validated against the node's caps | the harness (a problem when harness mode is off) |
| `tests` | The [saved tests](saved-tests.md), with `{{variables}}` for credentials (never values); a test that fails validation is a problem and is kept, never pruned | the saved tests (`<state>/tests.json`) |
| `test_schedules` | The [test schedules](saved-tests.md#schedules), without their status (a replaced schedule keeps its own); one that fails validation is a problem and is kept, never pruned | the schedules (`<state>/test-schedules.json`) |
| `agents` | The default ceiling and the rules of [AI agents](agent-mcp.md#managing-agents) | the agents policy |
| `roles` | The [custom roles](users.md#roles): name, description, permissions | the user store; a prune never deletes a role somebody holds |
| `users` | The [users](users.md) (password sign-in only): name, display name, email, roles, `disabled`; never a password or a hash (a document with one does not load the section). A user the document creates is invited, and the apply answers the link (`results.users.invitations`) | the user store, with the API's rules: nobody changes their own roles, a caller grants only what it holds; a prune never deletes the first admin, the applying user or the last enabled admin |
| `health` | `initial`: `healthy` or `unhealthy` | the `/health` toggle |

Two rules make a document safe to apply:

- **A section that is absent is left alone.** A document with only `health` changes
  nothing else. An export always writes every section (empty lists where there is
  nothing), so an export is the whole instance.
- **With `prune`, a present section is the whole truth for its part of the
  instance.** API-created mocks, published portal items, published registry entries,
  saved scenarios, saved tests, test schedules and stored settings or secrets that the section does not name are
  removed. Without `prune`, applying only adds and updates. The `git.*` settings are
  never exported nor pruned: where an instance keeps its configuration is the
  deployment's business. A mock the document names but cannot load (an entry that
  does not parse, a spec that cannot be read, does not parse or does not compile) is a
  problem in the answer and is kept as it is: it is never pruned, and the dry run's
  `would_delete` says the same. A prune of `mocks` deletes only the instance's own API
  mocks: never a visitor's or an API key's mock (self-service), nor a running gateway
  scenario's mock, which is not exported either. The `harness` and `registry` sections
  prune only what the document does not name at all (an entry that fails validation
  is a problem, and its saved copy stays).

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

A dry run answers the same and changes nothing. It validates with the apply's rules:
setting kinds and checks, locked settings, secret references, every mock compiled, the
portal's metadata, and the instance's capacities, so a document the apply would refuse
has `ok: false` in its preview too. An apply is not one transaction: sections are applied
in order (settings, secrets, mocks, registry, portal, harness, tests, test schedules,
agents, health),
and an entry that fails is reported while the others go ahead, so preview first. An
apply (and its dry run) runs on the compile pool, as mock edits do: when the pool stays
busy the answer is `503` with `Retry-After`.

A document pulled from a repository is applied with stricter rules (it cannot repoint
outbound requests, the sign-in, a test's gateway or a schedule's webhook); [Source
control](source-control.md#what-a-repository-writer-can-do) lists them.

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

## On every surface

The same job from the API, the console, the command line, an agent and the configuration document.

| Surface | Where |
|---|---|
| API | `GET /_rustybin/config/export`, `POST /_rustybin/config/apply` (`dry_run`, `prune`), `GET /_rustybin/config/schema` |
| Console | **Management, Export**: the document, copy, download, the bundle, the diff, apply with a dry run |
| CLI | `rustybin config export`, `apply`, `diff`, `schema`, `check` ([reference](cli.md#config)) |
| Management MCP | `export_config`, `diff_config`, `apply_config`; the resource `rustybin://config.schema.json`; the prompt `build_instance` ends with an export ([agent page](agent-mcp.md)) |
| `rustybin.yaml` | This page: the document is the surface |
| Environment | `RUSTYBIN_CONFIG_FILE` (loaded at startup and on reload), `RUSTYBIN_STATE_DIR` (where applied state persists), `RUSTYBIN_SETTINGS_LOCK` ([configuration](configuration.md#state-directory-settings-and-secrets)) |
