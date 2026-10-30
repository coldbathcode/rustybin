# Mock APIs (OpenAPI)

Upload an OpenAPI document and Rustybin serves it as a named mock API at
`/mock/<name>`: every operation answers with a response that matches the spec,
every call is captured, and security from the spec (API keys, basic, bearer,
OAuth 2.0 scopes, mTLS) can be switched on in front of it. Point a gateway at the
mock URL and you have a realistic upstream for an API that does not exist yet, or
that you cannot reach from a demo laptop.

- OpenAPI 3.0 and 3.1, as JSON or YAML, one bundled file (local `#/...` references
  only; external `$ref`s are refused with a clear message). Swagger 2.0 is converted
  to OpenAPI 3.0 on import, with a warning saying so.
- Responses come from the spec's examples or are generated from the schemas,
  deterministically, with path parameters echoed (`GET /pets/42` returns `"id": 42`).
  Rules, overrides and the `Prefer` header pick other documented responses. See
  [Responses, rules and validation](responses.md).
- Requests are validated against the spec (warn by default, or enforce with `400`).
- Every deployed mock is also an MCP server at `/mock/<name>/_rustybin/mcp`, one tool
  per operation, answered by the same mock. See [An MCP server for every
  mock](mcp.md).
- Publish a mock to the [dev portal](../portal.md) at `/portal`: testers read its
  reference, try it, download collections and copy its MCP snippets without the console.
- Each mock has a draft and a live revision: edits are checked (a valid request and
  response per operation) before they are deployed, and a deploy can be rolled back.
- Mocks are created in the [web console](../console.md#mock-apis), through the
  control API, from a `rustybin.yaml` file or a directory of specs at startup
  ([Configuration as code](config.md)), or with the [`rustybin mock` CLI](cli.md).

Mocks live in memory. Mocks declared in files are recreated at every start; mocks
created in the console or through the API are lost on restart unless you export
them to a `rustybin.yaml` ([Export and apply](config.md#export-and-apply)).

## Quick start

### In the console

1. Open `/ui/#/mocks` (for example `http://localhost:8080/ui/#/mocks`).
2. Drop an OpenAPI file on the page, or paste it. Rustybin parses it at once and
   shows the operations, the security schemes and any warnings, with a name derived
   from `info.title`.
3. Choose how responses are produced (spec examples, generated data, or your own),
   whether to secure the mock and whether to validate requests, then **Create and
   deploy**.
4. Copy the mock URL (or the ready curl command) into your gateway. The mock's
   **Traffic** tab shows each call as it arrives.

### With the API

`POST /_rustybin/mocks` takes the spec (a JSON object, or the document text) and
optional settings, runs the deploy checks and makes the mock live (`201`). The
admin token is needed when the instance has one (the example server does):

```hurl
{{#include ../../examples/mocks/mocks.hurl:create}}
```

```hurl
{{#include ../../examples/mocks/mocks.hurl:call_created}}
```

| Field | Default | Meaning |
|---|---|---|
| `spec` | required | The OpenAPI document: an object, or JSON / YAML text |
| `name` | slug of `info.title` | 1 to 40 characters `a-z`, `0-9` and `-`, not at either end; `import`, `export`, `apply` and `reload` are reserved. A taken default gets a `-2`, `-3`, ... suffix; a taken explicit name is a `409`. |
| `deploy` | `true` | Run the checks and make the draft live. `false` creates a draft only (its URL answers `503` until deployed). |
| `force` | `false` | Deploy even when the checks report errors |
| `enabled` | `true` | The on/off switch |
| `strip_server_prefix`, `responses`, `security`, `operations` | defaults | The mock's settings, the same structure as in [`rustybin.yaml`](config.md#reference) |

A deploy that the checks refuse answers `422` with the report; an invalid spec or
invalid settings answer `400` with a message.

`POST /_rustybin/mocks/import` parses a spec without creating anything and returns
what the console's wizard shows: title, version, suggested (free) name, server
prefix, operations, security schemes, warnings and a check summary. The body is
`{"spec": ...}` or the document itself (JSON or YAML text):

```hurl
{{#include ../../examples/mocks/mocks.hurl:import}}
```

### With the CLI

```bash
rustybin mock validate openapi.yaml                    # offline: parse and check
rustybin mock init openapi.yaml --name orders > rustybin.yaml
rustybin mock push --server http://localhost:8080 --token change-me
curl http://localhost:8080/mock/orders/...
```

See [CLI](cli.md) for every subcommand.

## The built-in sample

Every instance starts with a `petstore` mock (`src/mocks/petstore.yaml`, OpenAPI
3.0, server prefix `/v1`), so there is something to call on first run. It is not
created in [public mode](../concepts/sessions.md#public-mode); set
`RUSTYBIN_MOCKS_SAMPLE=false` (or `true`) to change that. It answers from its
examples, generates the pets that have none, and has one rule: pet id `0` answers
the documented `404`.

```hurl
{{#include ../../examples/mocks/mocks.hurl:sample_list}}
```

```hurl
{{#include ../../examples/mocks/mocks.hurl:sample_get}}
```

```hurl
{{#include ../../examples/mocks/mocks.hurl:sample_rule}}
```

```hurl
{{#include ../../examples/mocks/mocks.hurl:sample_prefer}}
```

```hurl
{{#include ../../examples/mocks/mocks.hurl:sample_create}}
```

The sample can be edited, switched off and switched back on like any other mock;
it is left out of `GET /_rustybin/mocks/export` (export it by name if you want a
copy). Restarting the instance restores it.

## URLs

| URL | What it is |
|---|---|
| `/mock/{name}{prefix}{path}` | An operation of the mock: `{path}` is the spec path with its parameters filled in, `{prefix}` the path of the spec's first server URL (`/v1` for `servers: [{url: https://api.example.com/v1}]`, empty without servers). Any method. |
| `/mock/{name}/_rustybin/openapi.json` and `.yaml` | The deployed spec, with `servers` rewritten to the mock URL, ready to import into a gateway or an API client |

`strip_server_prefix: true` serves the paths without the prefix
(`/mock/orders/orders/7` instead of `/mock/orders/api/orders/7`). Server URL
variables are replaced by their defaults. The two spec URLs win over the
operations (a spec path `/_rustybin/openapi.json` cannot be mocked) and are not
captured.

What a mock URL answers when no operation responds, as `application/problem+json`
and captured in the mock's traffic (except for an unknown name, which has no mock
to capture it):

| Situation | Status |
|---|---|
| No mock with that name | `404` `{"error": "mock not found", ...}` |
| Mock created but never deployed | `503` |
| Mock switched off | `503`, or `responses.disabled_status` (any 4xx or 5xx, for example `404`) |
| No operation matches the path | `404`, with the mock's `base_path` |
| The path matches but not the method | `405` with `Allow` |

```hurl
{{#include ../../examples/mocks/mocks.hurl:sample_unmatched}}
```

```hurl
{{#include ../../examples/mocks/mocks.hurl:served_spec}}
```

Every mock response carries `X-Rustybin-Mock: <name>`, and once an operation
matched `X-Rustybin-Mock-Operation: <operationId>` (a stable id such as
`get_orders_id` is made up when the spec has none). Successful mock responses also
carry `X-Rustybin-Mock-Source` (how the response was chosen) and, when the request
did not match the spec, `X-Rustybin-Mock-Validation: failed`. All four are exposed
to browsers through CORS.

[Fault injection](../concepts/fault-injection.md) (`X-Rustybin-Delay`,
`X-Rustybin-Fail`) works on mocks like on every data-plane route.

## Listing and inspecting mocks

```hurl
{{#include ../../examples/mocks/mocks.hurl:list}}
```

The list gives, per mock: `name`, `url`, `title`, `version`, `state` (`live`,
`off` or `draft` when never deployed), `enabled`, `has_draft_changes`, `origin`
(`api`, `file` or `sample`), the operation count, `security` (enabled and mode),
response `mode`, `validate_requests`, `requests_total`, `last_hit`, the live and
draft revision numbers, a `check` summary and `expires_at` (public mode). It also
returns the instance `limits` and `load_errors` (problems from `rustybin.yaml` or
the mocks directory).

`GET /_rustybin/mocks/{name}` adds the draft's details: the spec summary and its
warnings, the settings, every operation (method, path, statuses, whether it is
secured, where its response comes from, its check status), the demo `credentials`,
the deploy report summary and the revisions.

## Draft, deploy, rollback

Every change (`PATCH /_rustybin/mocks/{name}`, `PUT .../operations/{op}`) goes to
the mock's **draft**; the **live** revision keeps answering until you deploy.
`PATCH` takes `name` (rename: the old URL stops working), `spec` (replace the
document), `strip_server_prefix`, `responses`, `security` and `operations`. Each
given section replaces the draft's section as a whole (a `responses` object without
`mode` resets the mode to `examples`); sections you leave out are kept.
`"deploy": true` publishes in the same call.

```hurl
{{#include ../../examples/mocks/mocks.hurl:draft}}
```

`POST /_rustybin/mocks/{name}/deploy` runs the [deploy checks](responses.md#deploy-checks)
on the draft and swaps it in atomically; check errors answer `422` with the report
unless you add `?force=true`. Deploying an unchanged draft is a no-op. The previous
live revisions are kept (5, 2 in public mode): `POST .../rollback` makes the
previous one live again, or the one named in `{"revision": n}` (revision numbers
come from the details' `revisions` list). The draft follows the rollback.

```hurl
{{#include ../../examples/mocks/mocks.hurl:rollback}}
```

## On and off

`POST .../disable` switches a mock off without losing its configuration or
traffic: its URL answers `503` (or `responses.disabled_status`), which gives
gateway health checks and circuit breakers something to react to. `POST .../enable`
switches it back on. `DELETE /_rustybin/mocks/{name}` removes the mock and its
traffic.

```hurl
{{#include ../../examples/mocks/mocks.hurl:switch}}
```

```hurl
{{#include ../../examples/mocks/mocks.hurl:delete}}
```

## Traffic

Every call to a mock is captured, including the ones answered with `401`, `400`,
`404`, `405` or `503`: method, path below the mock (`path`) and full path, query,
headers, client IP, body (up to 64 KiB), the `outcome` (`mocked`, `unauthorized`,
`forbidden`, `invalid_request`, `not_found`, `method_not_allowed`, `disabled`,
`not_deployed`), the matched `operation`, the live `revision`, the response
`source`, the `validation` issues, the `auth` result, the status, the response
headers and body, and the duration.

```hurl
{{#include ../../examples/mocks/mocks.hurl:traffic}}
```

| Route | Returns |
|---|---|
| `GET /_rustybin/mocks/{name}/requests` | Captured requests, newest first (`?limit=`, default 50) |
| `GET /_rustybin/mocks/{name}/requests/{n}` | One request by sequence number |
| `GET /_rustybin/mocks/{name}/requests/stream` | Live feed (Server-Sent Events, one `request` event per call, `Last-Event-ID` replays) |
| `DELETE /_rustybin/mocks/{name}/requests` | Clear the captures (admin) |

The oldest captures are evicted first when the log is full (see the limits below).
Mock calls also appear in the [request inspector](../concepts/inspector.md) like any
other data-plane request.

## Control-plane protection

Mock management lives on the control plane (`/_rustybin/mocks*`); the mocks
themselves (`/mock/*`) are data plane and never need a control-plane token.

- Every change (create, `PATCH`, `PUT`, deploy, rollback, enable, disable, delete,
  apply, reload, clearing traffic) follows the
  [admin token rules](../concepts/sessions.md#admin-token): with
  `RUSTYBIN_ADMIN_TOKEN` set it needs `Authorization: Bearer <token>` or
  `X-Rustybin-Admin-Token`; without one it is open, except in public mode where it
  is refused (`403`), so a public instance is read-only for mocks, unless
  [self-service mocks](self-service.md) let visitors manage their own.
- With [control-plane authentication](../concepts/control-plane-security.md)
  (`RUSTYBIN_CONTROL_AUTH=password`, the default, `token` or `jwt`), reading needs a token with the
  `console` scope and every `POST`, `PUT`, `PATCH` and `DELETE` needs the admin
  token or the `admin` scope, except the read-only `POST` routes `import`,
  `.../validate` and `.../rules/test`, which the `console` scope may call.
- Mock secrets (API keys, bearer tokens, basic passwords) are only shown to
  callers who may change mocks; everyone else gets placeholders
  ([hidden secrets](security.md#demo-credentials)).

```hurl
{{#include ../../examples/mocks/mocks.hurl:admin_required}}
```

```hurl
{{#include ../../examples/mocks/mocks.hurl:secured_console}}
```

## Limits

| | Normal | Public mode |
|---|---|---|
| Mocks per instance (sample and file mocks included) | 100 | 50 |
| Spec size | 1 MiB | 256 KiB |
| Operations per mock | 500 | 200 |
| Rules per operation | 20 | 10 |
| Configured response body | 256 KiB | 64 KiB |
| Previous revisions kept for rollback | 5 | 2 |
| Captured requests per mock | 200 (1 MiB) | 50 (256 KiB) |
| Captured request body | 64 KiB | 64 KiB |
| [Stateful data](responses.md#stateful-mode) per mock | 1,000 items, 2 MiB | 200 items, 256 KiB |
| `delay_ms` | 30 s | 10 s |
| Lifetime of mocks created through the console or API | unlimited | 24 hours |

Mocks from `rustybin.yaml`, the mocks directory and the sample never expire. Other
fixed caps: 20 headers per configured response, `matches` patterns up to 256
characters, 50 API keys, bearer tokens and basic users per mock, `$ref` chains up
to 32 deep, a `rustybin.yaml` up to 8 MiB and 200 files in the mocks directory.
Generated strings are padded to `minLength` up to 64 KiB (a larger minimum gives a
value that fails its own schema, which the deploy checks report), and one request
validation makes at most 500,000 schema checks.
`GET /_rustybin/mocks` returns the instance's limits as `limits`.

## Control API

| Route | Purpose |
|---|---|
| `GET /_rustybin/mocks` | List mocks, limits and load errors |
| `POST /_rustybin/mocks` | Create (admin) |
| `POST /_rustybin/mocks/import` | Preview a spec, create nothing |
| `GET /_rustybin/mocks/{name}` | Details of the draft, revisions, credentials |
| `PATCH /_rustybin/mocks/{name}` | Edit the draft (admin) |
| `DELETE /_rustybin/mocks/{name}` | Delete (admin) |
| `POST /_rustybin/mocks/{name}/enable`, `.../disable` | The on/off switch (admin) |
| `POST /_rustybin/mocks/{name}/deploy` | Check and publish the draft, `?force=true` (admin) |
| `POST /_rustybin/mocks/{name}/rollback` | Previous revision, or `{"revision": n}` (admin) |
| `GET /_rustybin/mocks/{name}/report` | [Deploy checks](responses.md#deploy-checks) of the draft |
| `GET`, `PUT /_rustybin/mocks/{name}/operations/{op}` | One operation, or set its [settings](responses.md#operation-settings) (admin) |
| `GET /_rustybin/mocks/{name}/operations/{op}/sample` | A [valid request and response](responses.md#samples-and-validation) |
| `POST /_rustybin/mocks/{name}/operations/{op}/validate` | Validate a body against the operation's schema |
| `GET`, `DELETE /_rustybin/mocks/{name}/requests`, `.../requests/{n}`, `.../requests/stream` | [Traffic](#traffic) |
| `GET /_rustybin/mocks/{name}/export`, `GET /_rustybin/mocks/export` | [`rustybin.yaml` export](config.md#export-and-apply) |
| `POST /_rustybin/mocks/apply` | Apply a `rustybin.yaml` document, `?prune=true` (admin) |
| `POST /_rustybin/mocks/reload` | Re-read the configured files (admin) |
| `GET /_rustybin/mocks/config.schema.json` | JSON Schema of `rustybin.yaml` |

`{op}` is the `operationId` (or the id Rustybin made up, shown in the details and
in `X-Rustybin-Mock-Operation`). The routes are also in `/openapi.json` under the
`Mocks` tag and in the console's API explorer.
