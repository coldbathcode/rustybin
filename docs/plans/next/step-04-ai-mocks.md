# Step 04: AI mocks: specs from a prompt, examples, AI edit

Master plan: [master-plan.md](master-plan.md). Status: **not started**. Needs: 03. About 3 days.

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

(filled in when built)
