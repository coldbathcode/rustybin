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

Mocks created in the console or through the API are saved when
`RUSTYBIN_STATE_DIR` (or `RUSTYBIN_MOCKS_STATE_DIR`) points to a writable directory.
Without it, they live in memory and are lost on restart. Mocks declared in files
are recreated at startup. Use [export and apply](config.md#export-and-apply) for a
portable `rustybin.yaml`; see [persistence](../troubleshooting.md#my-changes-disappeared-after-a-restart).

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
example runner uses an open control plane and an admin token. On a default
instance, use a token from [sign-in](../quick-start.md#first-sign-in), which also
satisfies the admin guard:

```hurl
{{#include ../../examples/mocks/mocks.hurl:create}}
```

```hurl
{{#include ../../examples/mocks/mocks.hurl:call_created}}
```

| Field | Default | Meaning |
|---|---|---|
| `spec` | required | The OpenAPI document: an object, or JSON / YAML text |
| `name` | slug of `info.title` | 1 to 40 characters `a-z`, `0-9` and `-`, not at either end; `import`, `export`, `apply`, `reload`, `visit`, `visits` and `ai` are reserved. A taken default gets a `-2`, `-3`, ... suffix; a taken explicit name is a `409`. |
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

### Two people editing one mock

Every mock answer carries its `draft_revision`, and `GET /_rustybin/mocks/{name}`
(and the operation's `GET`) sends it as the `ETag`. An edit that names the revision
it was made from, as `If-Match: "12"` or `"expected_revision": 12` in the body, is
refused with `409` when the draft has moved on since. The answer names the current
revision and what changed (the spec, the responses, an operation), and nothing is
saved: read the mock again, then edit. A deploy can name the revision it publishes
the same way (`If-Match` or `{"expected_revision": 12}` on `POST .../deploy`): a
draft changed since is refused with `409` and nothing goes live. `PATCH` with
`"deploy": true` always publishes the revision it just wrote. Revision numbers keep
growing across restarts (a new process starts counting from the clock), so a revision
read before a restart never matches a draft compiled after it.

```hurl
{{#include ../../examples/mocks/mocks.hurl:conflict}}
```

The console, the CLI (`--expect-revision`, else the revision it reads just before
the edit) and the management tools (`expected_revision` on `update_mock` and
`set_operation`) always send one. A plain API call without one is accepted, and
an edit that raced another without naming a revision is made again on top of it
rather than lost. The setting `mocks.require_revision` (Management, General) makes the
revision mandatory for every caller: an edit without one answers `428`.

| Surface | Where |
|---|---|
| API | `If-Match` or `expected_revision` on `PATCH /_rustybin/mocks/{name}`, `PUT /_rustybin/mocks/{name}/operations/{op}` and `POST /_rustybin/mocks/{name}/deploy`; the `ETag` on the mock and its operations; `GET /_rustybin/mocks/{name}/revisions/{revision}` |
| Console | Sent with every edit and deploy; a refused edit opens the [compare view](#comparing-a-refused-edit) (Keep mine, Take theirs); a refused deploy says what changed and offers to reload |
| CLI | `rustybin mocks update` and `mocks set-operation` with `--expect-revision N` (default: read just before); `mocks deploy --expect-revision N` (default: the draft as it is); `mocks revision NAME N` |
| Management MCP | `expected_revision` on `update_mock`, `set_operation` and `deploy_mock`; `get_mock_revision` |
| `rustybin.yaml` | `settings: { mocks.require_revision: true }` (kept revisions are not part of a document: they are the store's memory) |

### Comparing a refused edit

`GET /_rustybin/mocks/{name}/revisions/{revision}` answers a revision the store still
keeps: the draft, the live revision, the last deployed ones (5, 2 in public mode) and
as many drafts that were replaced before anyone deployed them. Drafts and deployed
revisions share one numbering, so the `expected_revision` of a `409` is a revision
you can read here (until it is pushed out). The answer holds the spec as uploaded
(`spec.text`), the settings (secrets redacted unless you may change the mock),
whether it is the draft or live, and `changes_to_draft`: the sections the current
draft changes since, the same list as the `409`'s `changed`. A revision no longer kept
is `404`, and so is another visitor's mock (it is read like the mock itself). Kept
revisions live in memory: after a restart only the saved draft and live revision are
there.

```hurl
{{#include ../../examples/mocks/mocks.hurl:compare}}
```

In the console a refused edit opens a compare dialog instead of a plain refusal. Per
section (the spec, the responses, security, the MCP server, each operation's settings,
rules and examples) it shows what the other change did, from the revision the edit was
made from to the current draft, beside the edit that was refused, from the same
revision, each as a line diff. A badge says what saving yours does to the section:
your edit, their change stays, or changed on both sides (yours replaces theirs).
**Keep mine** sends the refused edit again naming the current revision: the sections
it sets replace theirs and the rest of their change stays (an operation editor's save
touches that one operation only). **Take theirs** reloads the page with their version.
**Close** (or Escape) keeps your form as it is, nothing saved. When the revision the
edit was made from is no longer kept, each section shows the current draft changed
into your edit. Every action is a button reached with Tab; the dialog keeps the focus
until it closes.

| Surface | Where |
|---|---|
| API | `GET /_rustybin/mocks/{name}/revisions/{revision}` |
| Console | The compare dialog of a refused edit (Settings, an operation's editor, the security panel) |
| CLI | `rustybin mocks revision orders 12` (a summary; `--json` the whole answer, `--spec` the spec only) |
| Management MCP | `get_mock_revision` (`name`, `revision`), then `update_mock` or `set_operation` with the current `expected_revision` |
| `rustybin.yaml` | Nothing: kept revisions are not configuration |

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
value that fails its own schema, which the deploy checks report), one generated value
holds at most 1 MiB of string content (past it strings stay short and the rest is
null), and one request validation makes at most 500,000 schema checks (`uniqueItems`
counts one per item). A path template may name a parameter once, hold at most 32
of them and contain no whitespace or control character (other paths are skipped
with a warning), a server URL is at most 2 KiB once
its variables are filled in (else `servers[0]` is ignored with a warning), and a
sample request cuts a path value at 1 KiB. Parsing and compiling a spec (preview,
create, edits, operation settings, samples, duplicates, AI previews and generated
examples) runs off the request threads, four at a time: past five seconds of waiting
the answer is `503` with `Retry-After`, and a caller without admin rights gets 30
compiles a minute (`429` past it): a visitor's per visit, an API key's per key, and
a caller without a manage token per address (a new session header per request does
not reset it). A visitor's creation past the caps is refused before its spec is
compiled.
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
| `GET /_rustybin/mocks/{name}/spec` | The draft's spec as uploaded (JSON or YAML as written) |
| `GET /_rustybin/mocks/{name}/revisions/{revision}` | A [kept revision](#comparing-a-refused-edit): its spec, settings and what the draft changed since |
| `POST /_rustybin/mocks/{name}/duplicate` | Copy the mock, `{"name": ...}` optional (default `<name>-copy`) (admin) |
| `GET`, `DELETE /_rustybin/mocks/{name}/state` | The items of a [stateful](responses.md#stateful-mode) mock, or reset them (admin) |
| `GET /_rustybin/mocks/{name}/coverage` | [Example coverage](ai.md#examples-and-the-alert-when-a-spec-has-none) of the draft and the live revision |
| `GET /_rustybin/mocks/{name}/collections/{format}` | The mock as a [collection](../exports.md) |
| `GET`, `PUT /_rustybin/mocks/{name}/operations/{op}` | One operation, or set its [settings](responses.md#operation-settings) (admin) |
| `GET /_rustybin/mocks/{name}/operations/{op}/sample` | A [valid request and response](responses.md#samples-and-validation) |
| `POST /_rustybin/mocks/{name}/operations/{op}/validate` | Validate a body against the operation's schema |
| `POST /_rustybin/mocks/{name}/operations/{op}/rules/test` | Which [rule](responses.md#rules) would answer a request |
| `GET`, `DELETE /_rustybin/mocks/{name}/requests`, `.../requests/{n}`, `.../requests/stream` | [Traffic](#traffic) |
| `GET /_rustybin/mocks/{name}/export`, `GET /_rustybin/mocks/export` | [`rustybin.yaml` export](config.md#export-and-apply) |
| `POST /_rustybin/mocks/apply` | Apply a `rustybin.yaml` document, `?prune=true` (admin) |
| `POST /_rustybin/mocks/reload` | Re-read the configured files (admin) |
| `GET /_rustybin/mocks/config.schema.json` | JSON Schema of `rustybin.yaml` |

`{op}` is the `operationId` (or the id Rustybin made up, shown in the details and
in `X-Rustybin-Mock-Operation`). The routes are also in `/openapi.json` under the
`Mocks` tag and in the console's API explorer.

## On every surface

The same job from the API, the console, the command line, an agent and the configuration document.

| Surface | Where |
|---|---|
| API | `/_rustybin/mocks*` ([above](#control-api)); the mocks themselves at `/mock/{name}/...` |
| Console | **Mock APIs** (`/ui/#/mocks`): the wizard, a mock's settings, security, operations, traffic, data, MCP server and deploy report ([console](../console.md#mock-apis)) |
| CLI | `rustybin mocks ...` for every route above ([reference](../cli.md#mocks)); `rustybin mock validate`, `init`, `sample` and `check-config` offline ([CLI](cli.md)) |
| Management MCP | `list_mocks`, `get_mock`, `preview_spec`, `get_mock_spec`, `get_mock_revision`, `create_mock`, `update_mock`, `set_operation`, `test_rules`, `check_mock`, `deploy_mock`, `rollback_mock`, `set_mock_enabled`, `duplicate_mock`, `delete_mock`, `sample_request`, `validate_payload`, `export_collection`, `get_mock_data`, `clear_mock_data`, `get_mock_traffic`, `clear_traffic`, `reload_mocks`, `try_request`; the prompt `mock_an_api` ([agent page](../agent-mcp.md)) |
| `rustybin.yaml` | `mocks:` ([mocks as code](config.md)), in a version 2 document with the rest of the instance ([the whole instance as code](../configuration-as-code.md)) |
| Environment | `RUSTYBIN_CONFIG_FILE`, `RUSTYBIN_MOCKS_DIR`, `RUSTYBIN_MOCKS_SAMPLE`, `RUSTYBIN_MOCKS_HOST`, `RUSTYBIN_MOCKS_STATE_DIR` ([configuration](../configuration.md#openapi-mocks)) |
