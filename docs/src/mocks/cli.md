# CLI

The `rustybin` binary doubles as a command-line tool for mocks: `rustybin mock ...`.
These are the offline commands (and the first remote ones); since 0.2 the same
binary manages a whole instance with `rustybin <noun> <verb>` after `rustybin login`:
see [The command line](../cli-guide.md) and the [CLI reference](../cli.md), where
`rustybin mocks ...` (plural) holds every mock verb of the control API.
`validate`, `init`, `sample` and `check-config` work offline on local files; the
other commands call the control API of a running instance. `rustybin mock --help`
prints the summary.

The examples run in a directory holding `inventory.yaml` (the spec shown in
[Configuration as code](config.md#loading-mocks-at-startup)), against a server on
`http://localhost:8080` with the admin token `change-me`. The outputs are real.

## Offline commands

### validate

Parses a spec and runs the [deploy checks](responses.md#deploy-checks) with default
settings: the operations with their ids and check status, every finding, and a
summary. Exit code `0` (warnings allowed), `1` when the checks report errors, `2`
when the file cannot be read or is not an OpenAPI 3 document.

```bash
{{#include ../../examples/mocks/cli.sh:validate}}
```

```text
Inventory 2.0.0 (OpenAPI 3.1.0)
3 operation(s), server prefix "/v2", suggested name "inventory"
  GET     /health                                  health                   ok
  GET     /items                                   listItems                ok
  GET     /items/{sku}                             getItem                  ok
Warning: 2 of 3 operation(s) have no example for a success response: the mock answers with data generated from the schema (generate examples with AI or the generator, or add them to the spec)
examples: 1 of 3 operation(s) have one for their success responses (without: listItems, getItem)
0 error(s), 1 warning(s)
```

The warning is the example coverage: operations without an example for a success
response answer with generated data ([Build mocks with AI](ai.md) fills them in).

A spec with problems (an example that breaks its schema, an undeclared path
parameter, an operation without responses):

```text
Bad 1 (OpenAPI 3.0.3)
2 operation(s), server prefix "", suggested name "bad"
  GET     /a                                       a                        warning
          Warning: the spec example for 200 does not match the schema (/ expected integer, got string "nope")
  GET     /b/{x}                                   get_b_x                  ok
warning: GET /b/{x}: path parameter {x} is not declared (treated as a string)
warning: GET /b/{x}: no responses documented, answers 200 with an empty body
0 error(s), 3 warning(s)
```

A Swagger 2.0 file is converted to OpenAPI 3.0 first, and the report says so:

```text
$ rustybin mock validate swagger.json
Legacy 1 (OpenAPI 3.0.3)
1 operation(s), server prefix "/v1", suggested name "legacy"
  GET     /pets                                    listPets                 ok
warning: converted from Swagger 2.0 to OpenAPI 3.0
0 error(s), 1 warning(s)
```

### init

Prints a `rustybin.yaml` for the spec: the name (`--name`, default from
`info.title`), the spec path as you typed it (keep the file next to the spec, or
fix the path: it is relative to the config file), `--generated` for generated
responses and `--secure` to switch [mocked security](security.md) on.

```bash
{{#include ../../examples/mocks/cli.sh:init}}
```

```yaml
# rustybin.yaml for Inventory (3 operations)
version: 1
mocks:
- name: docs-cli
  spec: inventory.yaml
  enabled: true
  responses:
    mode: generated
    validate_requests: warn
  security:
    enabled: false
    mode: spec
```

### sample

Prints a valid response for one operation (`--status` picks a documented status,
`--generated` and `--seed N` switch to generated data), or with `--request` a valid
request. The first line, on stderr, says where the body came from.

```bash
{{#include ../../examples/mocks/cli.sh:sample}}
```

```text
# GET /items/{sku} -> 404 (example)
{
  "title": "unknown SKU"
}
{
  "method": "GET",
  "path": "/v2/items/SKU-0",
  "query": [],
  "headers": []
}
```

```text
$ rustybin mock sample inventory.yaml getItem --generated --seed 7
# GET /items/{sku} -> 200 (generated)
{
  "sku": "SKU-0",
  "name": "Margaret Hamilton",
  "quantity": 8,
  "updated": "2026-09-14T09:30:00Z"
}
```

### check-config

Loads a config file the way the server does (environment variables expanded, spec
paths relative to the file) and compiles every mock. Exit code `1` when a mock does
not load or its checks report errors. The file defaults to `./rustybin.yaml`.

```bash
{{#include ../../examples/mocks/cli.sh:check_config}}
```

```text
inventory: Inventory (3 operations): 0 error(s), 1 warning(s)
```

## Remote commands

| Option | Default | Meaning |
|---|---|---|
| `--server URL` | `$RUSTYBIN_SERVER`, else `http://localhost:$RUSTYBIN_HTTP_PORT` (port 80 when unset, the server's own default) | The instance |
| `--token TOKEN` | `$RUSTYBIN_TOKEN`, else `$RUSTYBIN_ADMIN_TOKEN` | Sent as `Authorization: Bearer`: the admin token, or a control-plane JWT (`admin` scope for changes, `console` is enough for `list` and `export`) |
| `--json` | off | Print the raw JSON responses |

Errors print the server's message and exit with code `2`
(`rustybin mock: HTTP 401: admin token required: ...`).

### push

Applies a config file (default `./rustybin.yaml`) with
[`POST /_rustybin/mocks/apply`](config.md#export-and-apply): spec files are read
locally and inlined, so the server never needs access to them, and environment
variables are expanded. `--prune` deletes the mocks created through the API or
console that the file does not list. Pushing an unchanged file changes nothing.

```bash
{{#include ../../examples/mocks/cli.sh:push}}
```

```text
created: docs-cli
unchanged: docs-cli
```

### list

```bash
{{#include ../../examples/mocks/cli.sh:list}}
```

```text
docs-cli                 live      3 ops      0 req  http://localhost:8080/mock/docs-cli/v2
inventory                live      3 ops      0 req  http://localhost:8080/mock/inventory secured
petstore                 live      6 ops      0 req  http://localhost:8080/mock/petstore/v1
weather                  live      1 ops      0 req  http://localhost:8080/mock/weather
```

Columns: name, state (`live`, `off`, `draft`), operations, captured requests, URL,
then `secured` and `(draft changes)` when they apply. `--json` prints the
[`GET /_rustybin/mocks`](index.md#listing-and-inspecting-mocks) response:

```bash
{{#include ../../examples/mocks/cli.sh:json}}
```

### enable, disable, deploy, delete, export

```bash
{{#include ../../examples/mocks/cli.sh:switch}}
```

```text
docs-cli: disabled
503
docs-cli: enabled
200
```

`deploy` publishes the draft (add `--force` to deploy despite check errors);
`export` prints one mock, or every mock without a name, as `rustybin.yaml`:

```bash
{{#include ../../examples/mocks/cli.sh:deploy_export}}
```

```bash
{{#include ../../examples/mocks/cli.sh:delete}}
```

```text
docs-cli: deleted
```

## In a container

The image has the same binary on its `PATH`, and `docker exec` inherits the
container's environment: a `RUSTYBIN_ADMIN_TOKEN` set on the container is used as
the token, and the default server (`http://localhost`, or the port in
`RUSTYBIN_HTTP_PORT`) is the server in the container:

```bash
docker exec rustybin rustybin mock list
docker exec rustybin rustybin mock disable orders
docker exec rustybin rustybin mock check-config /app/rustybin.yaml
```

From the host, the same commands work against the published port:

```bash
rustybin mock list --server http://localhost:8080
```
