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
