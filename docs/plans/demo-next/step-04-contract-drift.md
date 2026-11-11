# Step 04: Contract drift

## Goal

A mock says what an API does; the real API may have moved on. A drift check sends the same requests
to the mock and to the real upstream it imitates and reports where they disagree, on demand or on a
schedule. It is the API governance story: "your spec is out of date here, here and here".

## Where things stand

- A mock has a spec, request validation (`mocks/request.rs`) and response validation in its deploy
  checks (`mocks/checks.rs`).
- Saved tests (`src/replay/`) send requests to a gateway under `Purpose::Replay`, with schedules and
  failure notices (`replay/schedule.rs`, `Purpose::Notify`) and kept runs (`replay/history.rs`).
- Step 03 adds `Purpose::Record` and the redaction of recorded traffic.
- Nothing compares a mock with the live API.

## Model

- A drift check on a mock: `upstream` (base URL), the requests to send (default: one valid request
  per operation from the mock's own deploy checks; or the mock's saved tests; or a recording's
  exchanges), variables for credentials (`{{NAME}}`, filled from the run or `RUSTYBIN_TEST_*` only, as
  saved tests are), a request cap (200) and a time cap (60 s).
- Sent to the upstream under `Purpose::Record` (the same allowlist as recording) and to the mock in
  process.
- Findings, each with the operation and the request, never a credential:
  - `status_differs`: the upstream answered a status the spec does not declare for the operation.
  - `schema_violation`: the upstream's body fails the spec's response schema (the path in the body,
    the rule).
  - `undocumented_field`: the upstream answers a field the schema does not name (a warning;
    `additionalProperties` respected).
  - `missing_field`: a required field absent from the upstream's answer.
  - `header_differs`: a header the spec declares is absent.
  - `unreachable`: the operation answers 404 or 405 upstream (removed or renamed).
- A run is kept with the saved test runs (`replay/history.rs`, same caps and retention) and can be
  scheduled with the same schedules (a `drift:<mock>` target); a schedule's failure notice is posted
  through `Purpose::Notify` as today.
- "Fix the spec": from a finding, propose a spec edit through the AI-built mocks' diff (the
  `edit_spec` task, with the finding as the instruction); never applied without a person.

## Surfaces

| Surface | What changes |
|---|---|
| API | `POST /_rustybin/mocks/{name}/drift` (run, JSON or JUnit), `GET /_rustybin/mocks/{name}/drift` (the kept runs), the drift settings on the mock (`upstream`, request source); `mocks.read` to read, `mocks.write` to run |
| Console | A Drift tab on a mock: the upstream, Run, findings grouped by operation, Fix with AI, the schedule |
| Management MCP | `check_mock_drift`, `list_drift_runs`; upstream bodies under `untrusted_*` |
| CLI | `rustybin mock drift <name> [--junit]`, exit code non-zero on findings (for CI) |
| `rustybin.yaml` | A mock's `drift:` block (upstream, request source, schedule) |

Docs: `docs/src/mocks/drift.md` with Hurl examples against an in-process upstream that differs from
the spec on purpose.

## Rules

- The upstream's answers are third-party content: `untrusted_*` in tools, text in the console.
- A drift check never changes the mock.
- Self-service visitors do not get drift checks.

## Tests that fail first

- An upstream equal to the spec: no findings.
- One planted difference of each kind: exactly that finding, with the operation and the body path.
- Credentials given as variables are sent and never kept, exported or answered.
- A scheduled drift run with findings posts one failure notice.
- JUnit output: one test case per operation, a failure per finding.

## Done when

A drift run reports each kind of finding on every surface, can be scheduled and run from CI, and is
documented with tested examples.
