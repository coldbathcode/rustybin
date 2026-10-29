# Step 04: AI mocks: specs from a prompt, examples, AI edit

Master plan: [master-plan.md](master-plan.md). Status: **done 2026-10-29**. Needs: 03. About 3 days.

## Goal

Describe an API and get a deployed mock that conforms to the OpenAPI version you chose. Open any
mock whose spec has no examples and be told so, with one click to generate conforming example
payloads for every operation (and so for every MCP tool built from it). Change a spec with an
instruction and see the diff before it applies.

## Read first

- `src/mocks/spec.rs` (parsing, versions 3.0 and 3.1, Swagger 2.0 conversion, warnings),
  `src/mocks/schema.rs` (generation and validation), `src/mocks/checks.rs` (deploy checks),
  `src/mocks/model.rs` (settings: per-operation overrides, examples), `src/mocks/respond.rs` (how
  examples are chosen), `src/mocks/service.rs` (`create`, `update`, `set_operation`, `deploy`),
  `src/mocks/mcp.rs` (tools from operations: where tool result examples come from).
- `ui/views/mocks/` (the wizard "create a mock from a pasted spec", the operation editor, the
  detail page), `ui/lib/format.js` `diffView`.
- `docs/plans/openapi-mocks.md` sections on examples and generation.
- Step 03's task framework and apply cards.

## Design

### Tasks (`src/assist/tasks/`)

| Task | Inputs | Output (validated) | Repair |
|---|---|---|---|
| `spec_from_description` | description, `openapi_version` (3.0.3 or 3.1.0; Swagger 2.0 is produced by converting 3.0 output back, offered as "legacy"), style options (resource naming, auth scheme: none, api key, bearer, OAuth2 client credentials; pagination: none, page, cursor; error shape: problem+json or simple), existing mock names to avoid | An OpenAPI document (JSON) | parse with `spec.rs`, run `checks.rs`; errors and warnings go back, at most two rounds |
| `examples_for_operations` | the spec, the list of operations without examples, how many per response (1 to 3), a persona ("realistic retail data", "edge cases") | `{operationId: {status: [example]}}` | each example validated against its response schema with `schema::validate`; failures go back per operation |
| `edit_spec` | the spec, an instruction, the version to keep | The whole document | parse, checks, and a structural diff must be non-empty and bounded (at most 40 percent of operations removed without an explicit "remove" in the instruction) |
| `request_examples` | an operation | request bodies and parameter sets that the operation accepts | `request.rs` validation |

The tasks run on the capable model by default; `examples_for_operations` batches operations (10
per call) and streams progress.

### Where examples live

`mocks::model` gains `examples: BTreeMap<operation, BTreeMap<status, Vec<Example>>>` with
`source: Spec | Generated | Ai | User` and `created_at`. `respond.rs` prefers spec examples, then
user, then AI, then generated (today's deterministic generation stays the fallback, so nothing that
works today changes). The mock's MCP tools return the same examples as tool results in "example"
mode, and `sample_request` and the inspector's "Generate arguments" use `request_examples`.

### The alert

`Compiled` gains `coverage: { operations, with_examples, without: Vec<operation> }`. It shows:

- on the mock detail page as a banner "12 of 18 operations have no examples in the spec" with
  "Generate with AI" (sparkle; needs a key) and "Use generated values" (the deterministic
  generator, no key), and the count per operation in the list;
- in the wizard's check step and in `check_mock` and the deploy report as a warning class
  `no_examples`;
- in `get_mock` (MCP) and `rustybin mocks check` (CLI) so an agent or a script sees it too.

### The console

- The wizard gets a first tab, **Describe it**: a prompt box with a placeholder that teaches by
  example ("An orders API for a coffee shop: customers, products, orders with line items, payment
  status; API key auth; cursor pagination"), the version dropdown (3.1.0 default, 3.0.3, legacy
  2.0), the style options as chips, and Generate. Progress streams ("drafting 7 operations",
  "checking", "repairing 2 errors"); the result opens in the existing spec review step with the
  checks, the coverage line and Create. The Describe tab is first and expanded when a key is set,
  present but folded without one.
- **Edit with AI** on the mock detail: an instruction box ("add a DELETE for orders and a 404
  example"), a streamed diff (`diffView`) of the spec with the checks of the new version, Apply as
  draft, then the usual Deploy. The history keeps the instruction as the revision note.
- **Generate examples** on the detail banner and per operation in the editor, with a persona
  picker and a preview per operation before Save. Generated examples carry a sparkle badge and can
  be edited or dropped like user examples.
- The assist bar learns the apply cards `create_mock`, `edit_spec`, `generate_examples`, so "make
  me a petstore clone with OAuth" from the bar ends in the wizard's review step.

### Routes

| Route | | |
|---|---|---|
| `POST /_rustybin/mocks/ai/spec` | manage | `{description, openapi_version, style}`; SSE progress then the spec and its check report; nothing is created |
| `POST /_rustybin/mocks/{name}/ai/examples` | manage | `{operations?, per_response, persona, apply: false}`; preview, or apply |
| `POST /_rustybin/mocks/{name}/ai/edit` | manage | `{instruction}`; the new spec, the diff summary and checks; `apply: true` saves it as the draft |
| `GET /_rustybin/mocks/{name}/coverage` | read | The example coverage |

Self-service visitors may use the AI routes only with their own key? No: the instance's key belongs
to the operator; visitors get `403` unless `RUSTYBIN_ASSIST_VISITORS=true` (with a per-visit cap).

### CLI and MCP notes

Step 10: `rustybin mocks describe "..." --version 3.1 --create`, `rustybin mocks examples <name>
--apply`, `rustybin mocks edit <name> "instruction"`, `rustybin mocks coverage <name>`. Step 11:
`generate_spec`, `generate_examples`, `edit_spec_with_ai` (confirmed, since they change the
instance), coverage in `get_mock` and `check_mock`, and the prompt `mock_an_api` gains the
describe path.

## Tests

- Unit: each task's validator and repair with the mock provider's scripted outputs (a valid spec,
  a spec with an unknown ref that gets repaired, an example that fails its schema once), the
  coverage computation, example precedence in `respond.rs`, MCP tool results using AI examples.
- Full app: the routes through the mock provider end to end (describe to create to call the mock
  and get an AI example back; MCP `tools/call` returning it), visitor refusal, the `no_examples`
  warning in `check_mock`.
- Console check: the Describe tab creates a mock with the mock provider, the banner appears on a
  spec without examples and disappears after Generate, Edit with AI shows a diff and applies.

## Docs

`docs/src/mocks/ai.md` ("Build mocks with AI": describe, examples, edit, what is sent, the
versions), the alert in `docs/src/mocks/responses.md`, `ai-assist.md` cross-links, Hurl examples
against the mock provider, the changelog.

## Done when

A description becomes a deployed, checked mock on the chosen version; a spec without examples is
flagged everywhere and filled by AI or the generator; an instruction edits a spec through a diff;
MCP tools of the mock answer with the examples; all checks pass; committed and pushed.

## Build notes

Built 2026-10-29 in one session. Where the build differs from the design above, and why:

- **Examples live per operation, in the settings.** `OperationSettings.examples: {status: [StoredExample]}`
  (`name`, `value`, `source: user | ai | generated`, `created_at`) rather than a mock-level map: it rides
  along `set_operation`, the operation editor, `rustybin.yaml` and the draft diff for free, and the limits
  are per operation and status (8). `respond.rs` serves them after the spec's own examples and before
  generation, and by name with `Prefer: example=<name>` (`X-Rustybin-Mock-Source: example:ai:<name>`), so
  nothing that works today changes and the mock's MCP tools return them with no change of their own.
- **Coverage is part of the deploy report.** `checks::coverage` runs with the checks and lands in
  `Report.coverage` plus a warning finding of class `no_examples` (`Finding` gained an optional `class`),
  so the detail, the preview, `report`, `check_mock`, `get_mock` and `rustybin mock validate` all carry
  it from one place; `GET /_rustybin/mocks/{name}/coverage` adds the live revision's. The rule: a 2xx
  response with a JSON schema needs an example (spec or stored); responses without a body need none.
- **Tasks got a `post` hook.** Schema validation alone cannot say whether a document parses or an
  example matches its response schema, so `Task.post: fn(inputs, answer) -> Result<answer, repair
  message>` runs after the schema and feeds the repair loop like a schema failure. The four tasks use it
  to parse and compile through `tasks::compile_doc` (an instance mock's limits), run the checks, validate
  examples and requests, bound the edit's removals (40 percent unless the instruction says remove, delete,
  drop) and require the version asked for. The runner also streams `progress` events for structured
  runs ("asking openai", "checking the answer", "the answer needs repair: ...").
- **Swagger 2.0 is asked for, not converted.** No 3.0 to 2.0 converter exists; the model (and the mock
  provider) write a `swagger: "2.0"` document directly, which the parser converts to 3.0 on import as any
  2.0 upload. The preview therefore reports `openapi: 3.0.3` for a 2.0 request while the text keeps 2.0;
  the docs say so.
- **The mock provider's answers are real.** `spec_from_description` drafts a CRUD API from the plural
  nouns of the description with the style asked for (and examples on every success response);
  `examples_for_operations` and `request_examples` run the deterministic generator against the schemas;
  `edit_spec` adds the instruction to the description, a 404 where asked and a DELETE for a named
  resource. "Use generated values" is the same generator without any AI call (`source: generated`).
- **Batches and merging.** The examples route batches ten operations per call, streams a progress line
  per batch, previews by default and stores on `apply`: a new run replaces the earlier examples of its own
  source (ai or generated) for the same status and keeps a person's. The edit route previews by default
  and saves the new document as the draft on `apply`; the history keeps no per-revision note (the
  `Compiled` revision has no note field; left for the config and git step, which records change notes).
- **Visitors**: `RUSTYBIN_ASSIST_VISITORS` (off) gates the three AI routes for self-service visitors; the
  generator stays open to them on their own mocks. The management MCP tools (`generate_spec`,
  `generate_examples`, `edit_spec_with_ai`) and the CLI commands are step 11's and 10's as the plan says;
  `get_mock` and `check_mock` summaries name the operations without examples now.
- **Console**: one module `ui/views/mocks/ai.js` holds the Describe tab (first and open when assist is on,
  present behind "Paste or upload" otherwise), the coverage banner, the Generate examples dialog (persona,
  count, a preview per operation, Save to draft) and Edit with AI (instruction, streamed progress, the
  existing `diffView`, Apply as draft). The operation editor lists stored examples with their source,
  offers them in the example picker and keeps them when it saves. Apply cards `create_mock` (opens the
  wizard's Describe tab and runs it), `edit_spec` and `generate_examples` (open the dialogs) complete the
  path from the assist bar; the mock provider proposes `create_mock` when a question describes an API.
- **Left out**: the per-operation "Generate examples" with a persona picker inside the editor is the same
  dialog scoped to that operation (no separate preview UI); the request_examples sparkle on MCP tools is
  the inspector's (step 05).

Checks run: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (the mock AI tests:
coverage and stored examples through REST and MCP, generated examples, the describe route on the three
versions, examples and edit through the mock provider, validation and repair through the scripted
provider, request examples, visitor refusal, no provider), `docs/examples/run.sh` (the new
`mocks/ai.hurl`), the console check (a new "ai mocks" step: Describe it to a deployed mock, the banner,
generated values, the stored example in the editor, Edit with AI with a diff and apply, the apply card
from the bar), `mdbook build docs`, `check_includes.py`, `check-no-dashes.sh`.
