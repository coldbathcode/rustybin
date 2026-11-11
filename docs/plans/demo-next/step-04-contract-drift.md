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

## Build notes (2026-11-11)

Built whole. Where the build differs from the plan above:

- **The request source is the mock's own sample per operation** (`checks::sample_request`, the
  deploy checks' request, with the mock's fake credentials taken out and the server prefix
  stripped from the path). Saved tests and recordings as sources were left out: the sample covers
  every operation, which is what a drift report is about.
- **Runs are kept in memory** (`AppState.drift`, 10 per mock for 50 mocks), not in the saved
  tests' history, and **schedules are not part of this step**: a drift check runs from the
  console, an agent or CI (`rustybin mocks drift ... --junit`, exit 1 on drift), which is where
  the plan's scheduled use mostly sits. A schedule target can come later through
  `replay::schedule` if asked.
- **The setting is the draft's, the definition the deployed one.** `drift.upstream` saved through
  `PATCH` deploys nothing, so the check reads the setting from the draft and judges against the
  live revision (or the draft when never deployed).
- **Variables**: `{{VAR}}` in header values from the run's `variables` or the instance's
  `RUSTYBIN_TEST_<VAR>`; the CLI fills `variables` from its own shell's `RUSTYBIN_TEST_<VAR>`, so
  a credential is never on a command line.
- **Findings**: `missing_field` is a required-property issue of the validator, the other issues
  `schema_violation` (at most 5 per operation); `undocumented_field` walks the schema with `$ref`s
  followed and is a warning that fails nothing.
- **Fix the spec with AI** is the mock page's existing AI edit; the Drift tab does not add a
  second entry.
- The docs example shows the refusals (no outbound host is allowed on the docs instance); the
  whole check against a real API that matches and one that drifted in every way is the test
  `an_api_that_matches_has_no_findings_and_each_drift_is_named`.

### For the merge

`src/mocks/drift.rs` (and its tests), `MockSettings.drift` (create, patch, `rustybin.yaml`, the
export and the config schema), the route in the mocks router and `PROJECT_ROUTES`, two tools at
the end of `TOOLS` (`PROJECT_TOOLS`, the frozen rights, 153 tools for an admin),
`rustybin mocks drift` and `drift-runs`, the console's Drift tab.
