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

A test has a `name` (letters, digits, `.`, `_`, `-`; not `run`, `runs`, `schedules` or `export`), a
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

The `trigger` is `console`, `cli`, `mcp`, `api` or `schedule:<name>` (a run of a
[schedule](#schedules), whose principal is `scheduler`). The console and the CLI say which they are with the header
`X-Rustybin-Client: console` or `cli`; a run through the management MCP is `mcp`;
anything else is `api`. It is a label for people and is never trusted for anything:
who ran it is the `principal` (`jwt:<subject>`, `admin-token`, `key:<id>`, `open`, or
on the management MCP the caller's name there), never a credential. The history is
admin only, as the tests are.

## Schedules

A schedule runs saved tests without anyone at the console: every N minutes, or once a
day at a time (UTC). Each run is kept in the [history](#history) with the trigger
`schedule:<name>` and the principal `scheduler`, and a failing run can post a notice
to a webhook.

```hurl
{{#include ../examples/tests/schedules.hurl:setup}}
```

```hurl
{{#include ../examples/tests/schedules.hurl:create}}
```

A schedule has a `name` (the saved tests' rules), exactly one of `every` (`5m` to
`24h`, with `s`, `m` or `h`) and `at` (`HH:MM`, UTC), the tests it runs (`names`, or a
name `prefix`, or neither for every saved test), `enabled` (`true` unless you turn it
off), an optional `webhook` and `notify_recovery`. `every` runs at each multiple of
the interval since the Unix epoch (`15m` runs at :00, :15, :30 and :45; `1h` on the
hour), so the times do not move when the instance restarts. A time missed while the
instance was down is not caught up: the schedule runs at its next time. At most 20
schedules, in `<state>/test-schedules.json` (in memory without a state directory).

```hurl
{{#include ../examples/tests/schedules.hurl:invalid}}
```

`POST /_rustybin/tests/schedules` creates one (`409` when the name is taken);
`PUT /_rustybin/tests/schedules/{name}` saves or replaces one (a replaced schedule
keeps its status); `GET` reads one, `DELETE` removes it (the tests and their history
stay). `POST /_rustybin/tests/schedules/{name}/run` runs it now, as the scheduler
would, and leaves its next time as it is:

```hurl
{{#include ../examples/tests/schedules.hurl:run_now}}
```

`GET /_rustybin/tests/schedules` lists them with their status: `running`, `next_due`,
`last_result` (`passed`, `failed` or `skipped`) with `last_at` and `last_detail`,
`last_run_id` (read it with `GET /_rustybin/tests/runs/{id}`), `runs`, `skipped`,
`last_notice` and the last ten outcomes (`recent`):

```hurl
{{#include ../examples/tests/schedules.hurl:list}}
```

**One run at a time.** A run of a schedule never overlaps its previous run, nor any
other run of the saved tests: when the console, the CLI or an agent is running tests,
a due run waits (it is tried again every second until the other run ends). A run that
falls due while the [harness](harness.md) sends load from this instance (a run, or a
cluster run it leads) is **skipped**: the status counts it under `skipped`, with
`last_result: skipped` and the reason, and the schedule runs again at its next time.
A schedule whose tests are all gone is skipped too. A run by hand is refused with
`409` in both cases.

**Variables come from the instance only.** A scheduled run fills `{{NAME}}` from the
instance's `RUSTYBIN_TEST_<NAME>` environment variables, and a test whose variable has
none does not run (an error in the run, as in a manual run). A schedule never holds a
credential nor a variable's value.

### The failure notice

With a `webhook`, a failing run (a test failed or could not run) posts one JSON notice;
a run that passes after a failing one posts one more when `notify_recovery` is on.
Nothing is posted for a run that passes after a pass.

```hurl
{{#include ../examples/tests/schedules.hurl:webhook}}
```

```json
{
  "event": "test_schedule.failed",
  "schedule": "daily",
  "selection": "tests sched-fails",
  "run_id": "20261103T023000123Z-ab12cd",
  "ran_at": "2026-11-03T02:30:00Z",
  "passed": false,
  "total": 1,
  "failed": 1,
  "errors": 0,
  "failing": ["sched-fails"],
  "failing_more": 0,
  "newly_failing": ["sched-fails"],
  "detail": null,
  "run_path": "/_rustybin/tests/runs/20261103T023000123Z-ab12cd",
  "run_url": "https://mocks.example.com/_rustybin/tests/runs/20261103T023000123Z-ab12cd"
}
```

The event is `test_schedule.failed` or `test_schedule.recovered`. A notice names the
failing tests (at most 50, `failing_more` counts the rest), never what a test saw nor
a variable. `run_url` is set when the `notify.base_url` setting (or
`RUSTYBIN_NOTIFY_BASE_URL`) gives the address people open the instance at; else
there is `run_path` only. When the receiver needs a credential, set the secret
`notify.webhook_token` (or `RUSTYBIN_NOTIFY_WEBHOOK_TOKEN`): it is sent as
`Authorization: Bearer` to every schedule's webhook. The URL itself takes no
credential (no user and password, no query string) and is part of `rustybin.yaml`.

Notices go out under the [egress policy](configuration.md#outbound-connections)'s
purpose `notify`, which has **no default**: add each webhook's host to
`RUSTYBIN_EGRESS_ALLOW` (or the `egress.allow` setting). A refused notice is not
retried: the status says `refused` with the reason. A delivery that fails (no answer,
a status other than 2xx) is tried 4 times, 15 seconds, then 1 and 4 minutes apart,
then reported `failed`. At most 50 notices wait at once (a notice beyond that is
`dropped`), they are kept in memory only, and deleting a schedule drops its waiting
notices. `last_notice.result` is `pending`, `retrying`, `delivered`, `failed`,
`refused` or `dropped`.

```hurl
{{#include ../examples/tests/schedules.hurl:failing_run}}
```

```hurl
{{#include ../examples/tests/schedules.hurl:refused}}
```

```hurl
{{#include ../examples/tests/schedules.hurl:delete}}
```

**Several replicas.** A schedule runs on the instance that holds it. Replicas that
load the same `rustybin.yaml` (or pull the same repository) each run every schedule,
and each posts its own notices; there is no election of one runner. Keep the
schedules on one instance, or give each replica its own document.

## Multi-step tests

A test can be a journey: several requests in order, where a value from one answer goes
into the next request. Give `steps` instead of `request`:

```json
{{#include ../examples/tests/two-steps.json}}
```

```hurl
{{#include ../examples/tests/arazzo.hurl:steps}}
```

Each step has an `id` (letters, digits, `_`, `-`), a `request`, its own `assert`, and
`outputs`: values taken from its answer, by name.

| Output | Is |
|---|---|
| `$statusCode` | The answer's status |
| `$response.header.<name>` | A response header |
| `$response.body#/json/pointer` | A value of the JSON body, by JSON pointer (`#/id`, `#/items/0/id`) |

A later step uses one as `{{steps.<id>.outputs.<name>}}`, in its path, a header or
its body; a step can only name an output of a step before it. A secret-looking header
may take one too (`Authorization: Bearer {{steps.sign-in.outputs.token}}`), so a
journey can sign in first. At most 20 steps, 16 outputs a step.

The steps run in order and **a step that fails stops the test**: the steps after it
are reported as `skipped`. A run's result has each step:

```hurl
{{#include ../examples/tests/arazzo.hurl:run_steps}}
```

A failing test names its step in the failure (`step read: status 200`), so the
history, JUnit and the CLI's table say where a journey broke. An output is shown cut
to 200 characters, and one whose name looks like a credential (`token`, `key`,
`secret`) as `(hidden)`: what a run keeps never holds one. Each step counts toward a
run's 200 requests.

## Arazzo workflows

[Arazzo](https://spec.openapis.org/arazzo/latest.html) is the OpenAPI Initiative's
format for multi-step API workflows. An instance imports an Arazzo 1.0 document:
each workflow becomes one multi-step test.

```yaml
{{#include ../examples/tests/pets.arazzo.yaml}}
```

A preview answers the tests it would make and what it left out, and saves nothing.
Send the document as the body, or as `{"document": ..., "dry_run": true}`:

```hurl
{{#include ../examples/tests/arazzo.hurl:preview}}
```

```hurl
{{#include ../examples/tests/arazzo.hurl:import}}
```

```hurl
{{#include ../examples/tests/arazzo.hurl:run}}
```

| In the document | Becomes |
|---|---|
| A source description | The mock of that name on this instance, else the one mock that has the operations the workflows use from it. Its `url` is never fetched |
| A workflow | A test named after its `workflowId`, with its `summary` |
| `operationId`, `operationPath` | The step's request: the method and path of that operation in the mock's spec, under `/mock/<name>` |
| `parameters` (the workflow's and the step's) | Path, query and header values |
| `requestBody.payload` | The body (`contentType`, default `application/json`) |
| `$inputs.<name>` | The variable `{{NAME}}`, filled when the test runs |
| `$steps.<id>.outputs.<name>` | `{{steps.<id>.outputs.<name>}}` |
| `successCriteria`: `$statusCode == 200`, `$response.header.X == 'v'`, `$response.body#/p == v`, or a JSONPath criterion on `$response.body` that is a path (`$.a.b`) or a path and a value | Assertions |
| `outputs`: `$statusCode`, `$response.header.<name>`, `$response.body#/pointer` | The step's outputs |

**Nothing runs at import and nothing is guessed.** What has no counterpart is named in
`left_out` by its place in the document: `onSuccess` and `onFailure` actions (a goto,
a retry, an end), regex and XPath criteria and criteria with other operators,
`replacements`, reusable `components`, a workflow's own `outputs` and `dependsOn`. The
step it belongs to is still imported when it can run without it. A workflow that
cannot (a nested workflow, a path parameter without a value, more than 20 steps) is
left out whole, with why.

A source description that is no mock here refuses the document, by name:

```hurl
{{#include ../examples/tests/arazzo.hurl:no_mock}}
```

`gateway` sends every imported test through a gateway, which is how a workflow tests
what a gateway does to a journey; `replace: true` overwrites tests of the same names
(without it they are a `409`); `project` puts the tests in a project.

A test exports back to Arazzo, when every request of it is an operation of a mock.
Assertions Arazzo's subset cannot say (`max_ms`, `body_contains`, a header that only
has to exist) are named in the `X-Rustybin-Left-Out` header:

```hurl
{{#include ../examples/tests/arazzo.hurl:export_arazzo}}
```

The Hurl export has a multi-step test's steps as entries, a step's outputs as
`[Captures]`, so the journey runs in CI without the instance's console:

```bash
{{#include ../examples/tests/arazzo-hurl.sh:import}}
{{#include ../examples/tests/arazzo-hurl.sh:export}}
```

```hurl
{{#include ../examples/tests/arazzo.hurl:cleanup}}
```

The same document is a [load test's source](harness.md#importing-an-arazzo-document).

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
| API | `GET`, `POST /_rustybin/tests`, `GET`, `PUT`, `DELETE /_rustybin/tests/{name}` (with `request`, or `steps`), `POST /_rustybin/tests/run` (`?format=junit`), `GET /_rustybin/tests/export`, `GET /_rustybin/tests/{name}/export` (`?format=arazzo`), `POST /_rustybin/tests/import` (Arazzo; `dry_run`, `gateway`, `replace`); the history: `GET`, `DELETE /_rustybin/tests/runs`, `GET /_rustybin/tests/runs/{id}` (`?format=junit`); schedules: `GET`, `POST /_rustybin/tests/schedules`, `GET`, `PUT`, `DELETE /_rustybin/tests/schedules/{name}`, `POST /_rustybin/tests/schedules/{name}/run` |
| Console | **Live traffic**: **Save as test** on an entry; **Saved tests**: the list, **Run all** with variables, a test's run, the failures and the change since the previous run, **Export as Hurl**, delete; a multi-step test's steps with each one's result and outputs, its Arazzo download; **Import Arazzo** (a file or pasted text, **Preview**, **Import**, what was left out); **History**: every run, a run's detail, its JUnit download, **Clear history**; **Schedules**: the list with each one's next time, last run and last notice, on or off, **Run now**, edit, delete (confirmed), and the form; the notice settings under **Management**, **Network** |
| CLI | `rustybin tests list`, `show`, `save`, `put`, `delete`, `run`, `export` (`export <name> --format arazzo`), `import <file>` (Arazzo; `--dry-run`, `--gateway`, `--replace`) (`run --junit`, `--var NAME=value`, `--var NAME=-` or `NAME=@file` for a credential); the history: `tests runs` (`--limit`, `--cursor`), `tests run-show <id>` (`--junit`), `tests runs-clear`; schedules: `tests schedules`, `schedule-show <name>`, `schedule-save <name> --every 15m` (or `--at 02:30`, `--names`, `--prefix`, `--webhook`, `--notify-recovery`, `--disabled`), `schedule-run <name>` (exits `1` when it fails), `schedule-delete <name>` ([reference](cli.md#tests)) |
| Management MCP | `list_tests` (one, all, `format: hurl`, or one as `format: arazzo`), `save_test` (with `request`, `capture` or `steps`), `import_tests` (Arazzo; `dry_run`), `delete_test` (confirmed), `run_tests` (confirmed when a test goes through a gateway or sends a method other than `GET`, `HEAD` or `OPTIONS`; one run at a time; variables only from `RUSTYBIN_TEST_*`; answers the `run_id` and the change), `list_test_runs`, `get_test_run` (results under `untrusted_results`, JUnit with `format: junit`), `clear_test_runs` (confirmed); `list_test_schedules`, `save_test_schedule` (confirmed when it names a new webhook), `delete_test_schedule` (confirmed), `run_test_schedule` (confirmed when its tests go through a gateway or change state, or it has a webhook) ([agent page](agent-mcp.md)) |
| `rustybin.yaml` | `tests:`, a list of tests as above (with `request`, or `steps`), and `test_schedules:`, a list of schedules (without their status) ([instance as code](configuration-as-code.md)); the history is not configuration and is never exported |
