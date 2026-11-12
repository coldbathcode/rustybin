# Step 07: Arazzo workflows

## Goal

Arazzo (the OpenAPI Initiative's specification for multi-step API workflows) describes journeys such
as "sign in, create an order, pay, check its status", with outputs of one step feeding the next and
success criteria per step. Rustybin imports an Arazzo document and runs each workflow as a test
through a gateway, and as harness traffic.

## Where things stand

- A saved test (`replay::SavedTest`) is one request with assertions; there is no chaining.
- The harness imports Postman collections, Artillery scripts, HAR files and captures into
  `source.requests` (`harness/import.rs`); an import never runs or guesses and names what it left
  out (`left_out`).
- Nothing reads Arazzo.

## Model

- Saved tests gain a multi-step form: `SavedTest.steps: [TestStep]` (exclusive with `request`), each
  `TestStep { id, request, assert, outputs: { name: expression } }`, at most 20 steps. Expressions are
  a small, documented subset: `$response.body#/json/pointer`, `$response.header.<name>`,
  `$statusCode`, and `{{steps.<id>.outputs.<name>}}` in later requests. A step that fails stops the
  test; the result shows each step.
- Arazzo import: `arazzo` 1.0.x documents; each workflow becomes one multi-step saved test. Source
  descriptions are resolved against the instance's mocks by name or by the operation ids they share
  (never fetched); `operationId` and `operationPath` become concrete requests from the mock's spec;
  `successCriteria` of the simple and JSONPath kinds become assertions; `outputs` become outputs;
  `parameters` and `requestBody` map directly; workflow `inputs` become `{{NAME}}` variables.
- What has no counterpart (regex criteria beyond the subset, `onFailure` retries and gotos, nested
  workflows, XPath) is listed in `left_out` with its place in the document, and the step it belongs to
  is still imported when it can run without it. As with the harness imports, the import never guesses.
- Harness: the same import produces `source.requests` sequences per virtual user (`harness/import.rs`
  gains `arazzo`), so a workflow can be load tested.
- Export: a multi-step saved test exports to Hurl (captures and asserts per entry) and back to Arazzo.

## Surfaces

| Surface | What changes |
|---|---|
| API | `POST /_rustybin/tests/import` (Arazzo, with `dry_run` answering the tests and `left_out`); the existing test routes carry `steps`; `/_rustybin/tests/{name}/export?format=arazzo`; the harness import takes `format: arazzo` |
| Console | Saved tests: Import Arazzo, a step list in the test view with each step's result and outputs |
| Management MCP | `import_tests` (Arazzo), `save_test` and `run_tests` with steps; `import_scenario` takes `arazzo` |
| CLI | `rustybin tests import <file>`, `rustybin harness import --format arazzo` |
| `rustybin.yaml` | `tests:` entries with `steps:` |

Docs: `docs/src/saved-tests.md` gains "Multi-step tests and Arazzo"; `docs/src/harness.md` the import;
an example Arazzo document against the petstore in `docs/examples/`.

## Owner decision (master plan section 6)

Workflows run as multi-step saved tests and as a harness import, not as a third engine.

## Rules

- Variables keep their rule: values from the run or `RUSTYBIN_TEST_*` only, never kept in a run's
  history; outputs extracted from a response are kept in the run only when they do not look like a
  credential (the redaction of `request_from_capture`).
- The import validates like the apply (`import::fits`).

## Tests that fail first

- A two-step workflow (create a pet, read it back by the id from step one) passes against the
  petstore mock and fails when the second step's path is wrong, naming the step.
- An Arazzo document with an `onFailure` goto imports its steps and lists the goto in `left_out`.
- A source description that names no mock on the instance is refused with its name.
- Hurl export of a multi-step test runs green under `hurl --test` in `docs/examples/run.sh`.

## Done when

Arazzo workflows import, run as tests and as harness traffic, and export on every surface, documented
with tested examples.

## Build notes (2026-11-12)

Built whole, with the recommendation of section 6: workflows run as multi-step saved tests and as
a harness import, not as a third engine. Where the build differs from the plan above:

- **`SavedTest.request` stays a field, absent with `steps`.** A multi-step test has an empty
  `request` that is not written to a document (`TestRequest::is_absent`); `validate` wants exactly
  one of the two, and with `steps` no top-level `assert`. Everything that walked one request
  walks `SavedTest::requests()` (the variables, `changes_state` for the tools' confirmation).
- **Outputs** are `$statusCode`, `$response.header.<name>` and `$response.body#/json/pointer`
  (a JSON pointer, as Arazzo writes it); a step uses `{{steps.<id>.outputs.<name>}}` only of a
  step before it, checked at save. Variables are filled first and outputs second, so a value an
  answer gave is never read as a variable; a filled path with a space or a control character is
  refused. A secret-looking header may take an output (a token from a sign-in step).
- **A result** has `steps` (each with `passed`, `skipped`, `status`, `ms`, `failures`, `outputs`),
  and the test's own `failures` or `error` carry `step <id>: ...`, so the history, JUnit and the
  CLI name the step without knowing about steps. An output is cut to 200 characters, `(hidden)`
  when its name looks like a credential, and scrubbed of variable values. A run's 200 requests
  count each step.
- **Source descriptions** are the mock of the same name, else the one mock that has every
  `operationId` the workflows use from that source; none or several refuses the document naming
  the source. Only mocks the caller may read are candidates (`replay::mocks_for`).
- **The subset of criteria**: simple `$statusCode == n`, `$response.header.X == 'v'`,
  `$response.body#/pointer == v`; JSONPath with the context `$response.body` and a condition
  that is a path or a path `==` a literal. Anything else is in `left_out`. A step without
  criteria has no assertion (nothing is guessed).
- **A workflow input** becomes a variable named as a captured credential is
  (`petName` is `{{PETNAME}}`). A whole-value expression in a JSON payload stays a string.
- **What leaves a workflow out whole**: a nested workflow step, an operation no source has, a
  path parameter without a value, a payload expression with no counterpart, more than 20 steps,
  a `workflowId` that is not a test name. Each is one line of `left_out` with why.
- **The export to Arazzo** needs every request to be an operation of a mock (matched through the
  mock's route table); a test that calls anything else is `422` naming the step. Assertions with
  no Arazzo counterpart are named in `X-Rustybin-Left-Out` (and `left_out` of the tool).
- **The harness import** makes request templates of the steps that stand alone. Templates are
  sent by weight, not as a journey, so a step that needs an input or an earlier step's output is
  left out and named in `variables`; the plan's "sequences per virtual user" would need the
  engine to pass values on, which is the third engine the decision ruled out.
- **Routes**: `POST /_rustybin/tests/import` also takes the document as the body
  (`application/yaml`, the rest in the query), and `GET /_rustybin/tests/{name}/export` answers
  one test as Hurl by default. `import` is no longer a test name.
- **The CLI**: `tests export [<name>] [--format hurl|arazzo]` covers both export routes.
- The tests were written with the code: `src/replay/tests/arazzo.rs` (the four the plan names,
  the rules of a multi-step test, the harness import), the tools' and the CLI's tests, a console
  step, and `docs/examples/tests/arazzo-hurl.sh`, which runs the exported Hurl under
  `hurl --test`.

### For the merge

`SavedTest.steps` and `TestStep` (every `SavedTest` literal gains `steps`), `replay/run.rs` (the
step loop, `StepResult`, `step_refs`), `replay/hurl.rs` (entries and captures), the new
`replay/arazzo.rs`, two routes in the tests router (`PROJECT_ROUTES`, `NEW_SINCE_0_4`),
`harness::import::Format::Arazzo` with `import_text_with` (the route and `import_scenario` pass
the caller's mocks), one tool at the end of `TOOLS` (163 for an admin), `tests import` and the
wider `tests export` in the CLI, the Import Arazzo card and step results in `ui/views/tests.js`.
