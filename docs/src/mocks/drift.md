# Contract drift

A mock says what an API does; the real API may have moved on. A **drift check** sends one
valid request per operation to the real API the mock imitates and reports where its
answers disagree with the spec, so you know the mock (and the spec behind it) is out of
date before a demo, or before a gateway's validation rejects the real thing. Run it from
the console, the CLI (in CI, with a JUnit report) or an agent.

```hurl
{{#include ../../examples/mocks/drift.hurl:runs}}
```

## What a check sends and reports

For each operation the check sends the mock's own sample request (the one its
[deploy checks](responses.md) make: path parameters, query parameters and a body from the
spec's examples or schemas), to the real API's base URL instead of the mock. The mock's
fake credentials are never sent. Each answer is judged against the spec:

| Finding | When |
|---|---|
| `status_differs` | The API answered a status the operation does not declare |
| `schema_violation` | The body breaks the response schema (the JSON pointer and the rule) |
| `missing_field` | A required field is absent |
| `undocumented_field` | The body has a field the schema does not name (a warning: it fails nothing; a schema with `additionalProperties` is left to its own rule) |
| `unreachable` | The API answered `404` or `405`, timed out, or could not be reached: the operation was removed or renamed |

A run answers each operation's status and findings, never what the API sent back, and
passes when no operation has a finding other than a warning. The last 10 runs of each mock
are kept (for 50 mocks). A check never changes the mock.

The real API is the mock's `drift.upstream` (its base URL, as the spec's `servers` give it,
such as `https://api.example.com/v1`), or `upstream` for one run:

```hurl
{{#include ../../examples/mocks/drift.hurl:no_upstream}}
```

`drift` is a setting of the mock, saved to the draft like its other settings and carried
by [`rustybin.yaml`](config.md):

```yaml
mocks:
  - name: orders
    spec: specs/orders.yaml
    drift:
      upstream: https://api.example.com/v1
```

## Credentials for the real API

Give the headers the real API needs in `headers`. A value may hold `{{VAR}}` variables,
filled from `variables` given with the run or from `RUSTYBIN_TEST_<VAR>` in the instance's
environment (the CLI fills them from its own shell's `RUSTYBIN_TEST_<VAR>`). Values are
never kept, never in a run and never in an audit entry. A variable that is not given
refuses the run before anything is sent:

```hurl
{{#include ../../examples/mocks/drift.hurl:variable}}
```

## Reaching the real API

A check reaches out, under the rules of [recordings](record.md): it needs `mocks.write`
(reading the kept runs needs `mocks.read`), goes through the
[outbound policy](../concepts/control-plane-security.md) under the purpose `record`, which
has no default (add the API's host to `RUSTYBIN_EGRESS_ALLOW`), and never follows a
redirect. At most 200 operations, 15 seconds a request and a minute in all; operations not
reached in time are counted as `skipped` and fail the run.

```hurl
{{#include ../../examples/mocks/drift.hurl:refused}}
```

## In CI

```sh
export RUSTYBIN_TEST_TOKEN=...            # the real API's credential
rustybin mocks drift orders \
  --header 'Authorization: Bearer {{TOKEN}}' --junit > drift.xml
```

The command exits `1` when the mock drifted. With `--junit` it prints a JUnit report: a
test case per operation, a failure per finding that is not a warning
(`?format=junit` on the API).

## On every surface

| Surface | Where |
|---|---|
| API | `POST /_rustybin/mocks/{name}/drift` (JSON or `?format=junit`), `GET /_rustybin/mocks/{name}/drift` (the kept runs); `drift` in a mock's create and `PATCH` |
| Console | A mock's **Drift** tab: the real API's base URL (**Save as the mock's upstream**), a header for its credential, **Check against the real API**, the findings by operation, the kept checks |
| CLI | `rustybin mocks drift <name> [--upstream URL] [--header 'K: V'] [--operation ID] [--junit]` (exits `1` on drift), `rustybin mocks drift-runs <name>` ([reference](../cli.md#mocks)) |
| Management MCP | `check_mock_drift` (confirmed: it sends requests out), `list_drift_runs`; `create_mock` and `update_mock` take `drift` |
| `rustybin.yaml` | A mock's `drift: { upstream }` |
