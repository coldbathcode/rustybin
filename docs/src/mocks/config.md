# Configuration as code

Mocks can be declared in a `rustybin.yaml` file next to your specs, committed with
your demo or test setup, loaded at startup, reloaded without a restart, and
exported from mocks you built in the console.

## Loading mocks at startup

| Source | How |
|---|---|
| A config file | `rustybin --config path/to/rustybin.yaml`, else `RUSTYBIN_CONFIG_FILE=path/to/rustybin.yaml`, else a `rustybin.yaml` in the working directory when there is one |
| A directory of specs | `RUSTYBIN_MOCKS_DIR=path/to/specs`: every `*.yaml`, `*.yml` and `*.json` file (at most 200, not recursive) becomes a mock with default settings, named after the file (`Orders API.yaml` becomes `orders-api`) |
| The built-in sample | `RUSTYBIN_MOCKS_SAMPLE` (default `true`, `false` in public mode) |

Both can be used together; a mock in the config file wins over a spec file of
the same name in the directory. Nothing stops the server from starting: a file that
cannot be read or parsed, an unset environment variable or a spec that does not
compile is logged as a warning and listed in `load_errors` of
`GET /_rustybin/mocks` (and in the console), and the other mocks load normally.
File-defined mocks are deployed even when their deploy checks report errors (the
report shows them) and never expire. Their `origin` is `file`.

Here is the file the documentation's example server runs with (via
`RUSTYBIN_CONFIG_FILE`), next to `specs/inventory.yaml`, plus
`RUSTYBIN_MOCKS_DIR` pointing at a directory holding `weather.yaml`:

```yaml
{{#include ../../examples/mocks/rustybin.yaml}}
```

```hurl
{{#include ../../examples/mocks/config.hurl:file_mocks}}
```

```hurl
{{#include ../../examples/mocks/config.hurl:call_inventory}}
```

## Reference

```yaml
version: 1                       # optional, must be 1
mocks:
  - name: orders                 # required: a-z, 0-9 and '-', 1 to 40 characters
    spec: specs/orders.yaml      # a spec file, relative to this file (or absolute)
    # spec_inline: {...}         # or the spec itself (object or text), not both
    enabled: true                # the on/off switch
    strip_server_prefix: false   # serve /orders, not /api/orders
    responses:
      mode: examples             # examples | generated | static
      seed: 1                    # generated data
      delay_ms: 0                # before every response (max 30000, 10000 in public mode)
      validate_requests: warn    # off | warn | enforce
      disabled_status: 503       # status while switched off (400 to 599)
    security:
      enabled: false
      mode: spec                 # spec | api_key | basic | bearer | jwt
      api_key:
        header: x-api-key        # the header of the fixed api_key mode
        keys: [demo-key]
      basic:
        users: { demo: demo }
      bearer:
        tokens: [demo-token]
      jwt:
        issuer: http://localhost:8080   # optional iss check
        audience: rustybin              # optional aud check
    operations:                  # keyed by operationId
      getOrder:
        mode: generated
        delay_ms: 150
        public: false
        respond: { status: 200, example: big, headers: { X-Cache: miss } }
        rules:
          - name: missing order
            when: { path.orderId: { equals: 0 } }
            respond: { status: 404 }
          - when: { header.x-tier: { equals: gold }, query.expand: { exists: true } }
            respond: { status: 200, body: { orderId: 1, total: 0 } }
```

Every field except `name` (and one of `spec` / `spec_inline`) is optional, with the
defaults shown. Unknown fields are errors, so a typo does not go unnoticed. The
response fields are described in [Responses, rules and validation](responses.md),
the security fields in [Mocked security](security.md).

### Editor completion

`GET /_rustybin/mocks/config.schema.json` is the JSON Schema of the file. With the
YAML language server (VS Code's YAML extension, Neovim, JetBrains IDEs), a comment
on the first line gives completion, hover documentation and validation:

```yaml
# yaml-language-server: $schema=http://localhost:8080/_rustybin/mocks/config.schema.json
version: 1
mocks: []
```

```hurl
{{#include ../../examples/mocks/config.hurl:schema}}
```

### Environment variables

In files read from disk (the config file, and `rustybin mock push` /
`check-config`), `${VAR}` is replaced by the environment variable `VAR` and
`${VAR:-default}` by `default` when `VAR` is unset. An unset variable without a
default becomes an empty string and is reported as a load problem. Use it for
credentials:

```yaml
security:
  enabled: true
  api_key:
    keys: ["${ORDERS_API_KEY}", "${ORDERS_API_KEY_2:-backup-key}"]
```

Documents sent to the API (`POST /_rustybin/mocks/apply`) are never expanded.

## Reloading

`POST /_rustybin/mocks/reload` (admin) and the `SIGHUP` signal re-read the config
file and the mocks directory and apply the difference: new mocks are created,
changed ones are redeployed (their traffic history is kept), unchanged ones are
left alone, and file-defined mocks that are no longer declared are deleted. Mocks
created through the console or the API are never deleted by a reload.

```hurl
{{#include ../../examples/mocks/config.hurl:reload}}
```

```bash
kill -HUP "$(pidof rustybin)"            # or: docker kill --signal HUP rustybin
```

## Export and apply

`GET /_rustybin/mocks/{name}/export` returns one mock, and `GET /_rustybin/mocks/export`
every mock except the built-in sample, as a `rustybin.yaml` document with the spec
inlined (`spec_inline`) and every setting spelled out. It describes the draft, so
it includes changes that are not deployed yet. The console offers the same
export, so "build it in the console, export it, commit the file" is a supported
workflow.

```hurl
{{#include ../../examples/mocks/config.hurl:export}}
```

`POST /_rustybin/mocks/apply` (admin) applies such a document (YAML or JSON) to a
running instance: listed mocks are created, or updated and redeployed when their
spec or settings differ, and reported as `created`, `updated`, `unchanged`;
problems are listed in `errors` (a changed mock is deployed even with check errors,
which are named there). With `?prune=true`, mocks created through the console or
the API that the document does not list are deleted (`deleted`); file-defined mocks
and the sample are never pruned. This is what `rustybin mock push` calls.

```hurl
{{#include ../../examples/mocks/config.hurl:apply}}
```

```hurl
{{#include ../../examples/mocks/config.hurl:prune}}
```

The API only accepts `spec_inline`: a server never reads files named by a request.
`rustybin mock push` reads the `spec` files locally and inlines them before
sending the document.

```hurl
{{#include ../../examples/mocks/config.hurl:apply_file_path}}
```

## Docker

The image's working directory is `/app` and it runs as uid 10001, so the simplest
setup mounts the file as `/app/rustybin.yaml` (found without any setting) and the
specs next to it, read-only:

```text
demo/
  rustybin.yaml        # spec: specs/orders.yaml
  specs/
    orders.yaml
```

```bash
docker run -d --name rustybin -p 8080:80 \
  -v "$PWD/demo/rustybin.yaml:/app/rustybin.yaml:ro" \
  -v "$PWD/demo/specs:/app/specs:ro" \
  rustybin
```

With only a folder of specs and default settings:

```bash
docker run -d --name rustybin -p 8080:80 \
  -e RUSTYBIN_MOCKS_DIR=/app/specs \
  -v "$PWD/specs:/app/specs:ro" \
  rustybin
```

The files must be readable by uid 10001 (world-readable files are). After editing
them, `docker kill --signal HUP rustybin` reloads the mocks without a restart.

With Docker Compose, the repository's `docker-compose.yml` has both variants
commented out; enabled, the service looks like this:

```yaml
services:
  rustybin:
    build: .
    container_name: rustybin
    ports:
      - "80:80"
    environment:
      - ORDERS_API_KEY=change-me          # used as ${ORDERS_API_KEY} in rustybin.yaml
      # - RUSTYBIN_MOCKS_DIR=/app/specs   # or: every spec in /app/specs, default settings
    volumes:
      - rustybin-certs:/app/certs
      - ./rustybin.yaml:/app/rustybin.yaml:ro
      - ./specs:/app/specs:ro
volumes:
  rustybin-certs:
```

`docker compose kill -s HUP rustybin` reloads. To manage mocks from inside the
container, see [the CLI](cli.md#in-a-container).
