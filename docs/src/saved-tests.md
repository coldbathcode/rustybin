# Saved tests

A saved test is a request with assertions. Make one from any request the
[request inspector](concepts/inspector.md) captured (the console's **Save as test**
on a Live traffic entry), or write one out. Run it against this instance, or through
a gateway, and export every test as one Hurl file for CI.

## A test from a captured request

Send traffic, find the capture, and save it under a name. The test keeps the method,
the path and query, the body and the headers, except the transport's own headers
(`Host`, `Content-Length`, the `X-Forwarded-*` family) and Rustybin's tags
(`X-Rustybin-Session`, `X-Request-Id`). The captured status becomes its first
assertion.

```hurl
{{#include ../examples/tests/saved-tests.hurl:capture}}
```

```hurl
{{#include ../examples/tests/saved-tests.hurl:save_capture}}
```

**Credentials are never saved.** A header or query value whose name looks like a
secret (`Authorization`, `Cookie`, anything with `key`, `token`, `secret`,
`password`, `auth` or `sig` in it) becomes a variable named after it:
`X-Api-Key: secret` is saved as `X-Api-Key: {{X_API_KEY}}`, `?token=abc` as
`?token={{TOKEN}}`; so does a secret-looking field of a captured JSON or form body
(`"password": "{{PASSWORD}}"`). The answer lists them as `introduced_variables`. A
written test is held to the same rule: a secret-looking header or query value must
reference a variable (`Authorization: Bearer {{TOKEN}}`), and a literal one is refused.
A run's report never shows a filled variable's value, even when the answer echoes it,
and the inspector captures the in-process requests of a run redacted. A binary capture
cannot be saved as a test.

Saved tests, their export and `list_tests` are admin only: a test may hold a captured
request.

On a public instance a capture can be saved only with its own session
(`X-Rustybin-Session`), as the inspector shows it; in shared mode only with its
API key.

## Writing a test

```hurl
{{#include ../examples/tests/saved-tests.hurl:save_written}}
```

A test has a `name` (letters, digits, `.`, `_`, `-`; not `run`, `runs` or `export`), a
`request` (`method`, `path` with the query, `headers`, `body`), an optional
`gateway` and its assertions. Each assertion checks one thing:

| Assertion | Passes when |
|---|---|
| `status: 200` | the answer's status is 200 |
| `header: content-type` | the header is there; with `equals: "application/json"`, it has that value |
| `body_contains: "\"id\""` | the body contains the text |
| `json: $.items[0].id` | the JSON body has a value there (`$.a.b`, `$.list[0]`, `$['a b']`); with `equals: 7`, that value |
| `max_ms: 500` | the answer came within 500 ms |

At most 200 tests, 64 KiB each. `PUT /_rustybin/tests/{name}` saves or replaces a
whole test.

## Running

`POST /_rustybin/tests/run` runs every test, or `names`. A test without a `gateway`
goes through this instance's application in process, with the caller's session; a
test with one is sent to the gateway's URL plus its path. The answer has each test's
status, time and failed assertions (what was expected, what was seen). One run goes
at a time, whichever surface starts it: another answers `409` until it ends. A test
run in process carries a peer address of the caller's own (its session), so
per-address limits and `ip:` sessions are never shared by every run.

```hurl
{{#include ../examples/tests/saved-tests.hurl:run}}
```

Variables come from the run's `variables`, else from the instance's environment
variable `RUSTYBIN_TEST_<NAME>` (only that prefix is ever read). A test whose
variable has no value does not run:

```hurl
{{#include ../examples/tests/saved-tests.hurl:missing_variable}}
```

`?format=junit` answers JUnit XML, for a CI system's test report:

```hurl
{{#include ../examples/tests/saved-tests.hurl:junit}}
```

A run sends at most 200 requests in 60 seconds. Gateway runs go through the
[egress policy](configuration.md#outbound-connections) under the purpose `replay`, which has
no default: add each gateway's host to `RUSTYBIN_EGRESS_ALLOW`.

## History

Every run is kept, whichever surface started it: its `run_id` (in the run's answer,
and in the `X-Rustybin-Test-Run` header of a JUnit answer), when it ran, its
`trigger`, the caller's `principal`, the tests asked for (`selection`, empty for
every test), the counts and each test's result with its failures. A run's answer and
its kept copy also say what changed since the previous run of the same tests (else the
previous run): the tests that passed there and fail now (`newly_failing`), and the
other way (`newly_passing`).

```hurl
{{#include ../examples/tests/saved-tests.hurl:change}}
```

`GET /_rustybin/tests/runs` lists them newest first, 20 a page (`limit`, at most 100);
`next_cursor` is the `cursor` of the next page:

```hurl
{{#include ../examples/tests/saved-tests.hurl:history}}
```

`GET /_rustybin/tests/runs/{id}` is one run, whole, and `?format=junit` its JUnit
report, so a CI job can fetch the report of a run it did not start:

```hurl
{{#include ../examples/tests/saved-tests.hurl:run_detail}}
```

```hurl
{{#include ../examples/tests/saved-tests.hurl:run_junit}}
```

The history keeps the last 200 runs for 30 days, one file each in
`<state>/test-runs/`, and reads them back after a restart; without a state directory
it keeps the last 50 in memory. Variables given to a run are never kept: a value is
replaced by its `{{NAME}}` wherever a result would repeat it, in the answer and in the
kept run. `DELETE /_rustybin/tests/runs` forgets every run (the tests stay):

```hurl
{{#include ../examples/tests/saved-tests.hurl:clear_history}}
```

The `trigger` is `console`, `cli`, `mcp` or `api` (a later release adds
`schedule:<name>`). The console and the CLI say which they are with the header
`X-Rustybin-Client: console` or `cli`; a run through the management MCP is `mcp`;
anything else is `api`. It is a label for people and is never trusted for anything:
who ran it is the `principal` (`jwt:<subject>`, `admin-token`, `key:<id>`, `open`, or
on the management MCP the caller's name there), never a credential. The history is
admin only, as the tests are.

## In CI without an instance's console

`GET /_rustybin/tests/export` is every test as one Hurl file, assertions included.
Tests without a gateway call `{{base_url}}`; variables stay Hurl variables:

```hurl
{{#include ../examples/tests/saved-tests.hurl:export}}
```

```bash
rustybin tests export > tests.hurl
hurl --test --variable base_url=https://mocks.example.com --variable X_API_KEY=... tests.hurl
```

Or run them on the instance and keep the report: `rustybin tests run --junit > report.xml`
exits `1` when a test fails.

```hurl
{{#include ../examples/tests/saved-tests.hurl:delete}}
```

## On every surface

| Surface | Where |
|---|---|
| API | `GET`, `POST /_rustybin/tests`, `GET`, `PUT`, `DELETE /_rustybin/tests/{name}`, `POST /_rustybin/tests/run` (`?format=junit`), `GET /_rustybin/tests/export`; the history: `GET`, `DELETE /_rustybin/tests/runs`, `GET /_rustybin/tests/runs/{id}` (`?format=junit`) |
| Console | **Live traffic**: **Save as test** on an entry; **Saved tests**: the list, **Run all** with variables, a test's run, the failures and the change since the previous run, **Export as Hurl**, delete; **History**: every run, a run's detail, its JUnit download, **Clear history** |
| CLI | `rustybin tests list`, `show`, `save`, `put`, `delete`, `run`, `export` (`run --junit`, `--var NAME=value`, `--var NAME=-` or `NAME=@file` for a credential); the history: `tests runs` (`--limit`, `--cursor`), `tests run-show <id>` (`--junit`), `tests runs-clear` ([reference](cli.md#tests)) |
| Management MCP | `list_tests` (one, all, or `format: hurl`), `save_test`, `delete_test` (confirmed), `run_tests` (confirmed when a test goes through a gateway or sends a method other than `GET`, `HEAD` or `OPTIONS`; one run at a time; variables only from `RUSTYBIN_TEST_*`; answers the `run_id` and the change), `list_test_runs`, `get_test_run` (results under `untrusted_results`, JUnit with `format: junit`), `clear_test_runs` (confirmed) ([agent page](agent-mcp.md)) |
| `rustybin.yaml` | `tests:`, a list of tests as above ([instance as code](configuration-as-code.md)); the history is not configuration and is never exported |
