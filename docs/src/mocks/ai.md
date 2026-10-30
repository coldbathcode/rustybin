# Build mocks with AI

With [your own AI key](../ai-assist.md) (or the built-in mock provider), a mock starts
from a sentence: describe the API and get a deployed mock on the OpenAPI version you
chose. Open any mock whose spec has no examples and be told so, with one click to
generate conforming example payloads for every operation (and so for every MCP tool
built from it). Change a spec with an instruction and see the diff before it applies.
Every call runs through an [assist task](../ai-assist.md#tasks): validated inputs, a
schema-checked answer, the deploy checks, at most two repairs, and nothing applied
without your click.

The examples on this page run against the built-in provider
(`RUSTYBIN_ASSIST_PROVIDER=mock`), whose answers are deterministic.

## Describe an API

**New mock, Describe it** in the console: a description, the OpenAPI version
(3.1.0 by default, 3.0.3, or Swagger 2.0 for legacy tooling), and the style as chips:
authentication (none, API key, bearer, OAuth2 client credentials), pagination (none,
page, cursor) and error shape (`problem+json` or a simple object). Generate streams
the progress ("asking openai", "checking the answer", "repairing") and lands in the
wizard's review step with the checks and the example coverage; Create and deploy
as usual. The same from the assist bar: "make me a petstore clone with OAuth" ends
in an apply card that opens the tab with the description filled in.

`POST /_rustybin/mocks/ai/spec` is the route: the document, its text and the preview
(what `POST /_rustybin/mocks/import` gives); nothing is created.

```hurl
{{#include ../../examples/mocks/ai.hurl:describe}}
```

```hurl
{{#include ../../examples/mocks/ai.hurl:describe_stream}}
```

The `text` of the answer is what `POST /_rustybin/mocks` takes as `spec` (the console
does this in the wizard's review step).

The task asks for an operationId, a summary, a request body schema and at least one
success response with a schema and an example on every operation, reuses schemas
through components and applies the style. The answer must declare the version asked
for, parse, and pass the deploy checks; the errors go back to the model, at most
twice. Swagger 2.0 output is converted to OpenAPI 3.0 when the mock is created, as any
2.0 upload is.

## Examples, and the alert when a spec has none

A response is served from the spec's `example`, else its first named example, else an
**example stored with the mock**, else data generated from the schema (see [How a
response is chosen](responses.md#how-a-response-is-chosen)). Every mock reports its
**example coverage**: which operations have an example for their success responses,
in the spec or stored. An operation whose success responses have no body needs none.

- The detail page shows a banner, "4 of 6 operations have no example for a success
  response", with **Generate with AI** and **Use generated values**, and the operation
  list says which; the wizard's review step shows the count.
- The deploy report carries a warning of class `no_examples`, so `check_mock` (MCP),
  `rustybin mock validate` (CLI) and the report tab say it too; `get_mock` names the
  count.
- `GET /_rustybin/mocks/{name}/coverage` answers it for the draft and the live revision.

```hurl
{{#include ../../examples/mocks/ai.hurl:coverage}}
```

**Generate with AI** runs the task `examples_for_operations` on the operations that
lack examples (or the ones you name), ten per call, with a persona ("realistic
support tickets", "edge cases") and one to three examples per response. Every example
is validated against its response schema; the failures go back to the model per
operation. The dialog previews them per operation; **Save to draft** stores them in
the draft as `operations.<id>.examples.<status>` with `source: ai`:

```hurl
{{#include ../../examples/mocks/ai.hurl:examples_preview}}
```

```hurl
{{#include ../../examples/mocks/ai.hurl:examples_apply}}
```

**Use generated values** needs no key: the deterministic generator fills the same
slots (`source: generated`), so the values the mock served implicitly become
stored, visible and editable. Named operations are redone even when covered; a new
run replaces the earlier examples of its source and keeps a person's:

```hurl
{{#include ../../examples/mocks/ai.hurl:generated_values}}
```

Stored examples appear in the operation editor with their source and are offered by
name: `Prefer: example=ai-1` picks one, and `X-Rustybin-Mock-Source` says
`example:ai:ai-1`. The mock's MCP tools return them as tool results, and
[`rustybin.yaml`](config.md) exports carry them under the operation. At most eight per
operation and status, each within the mock's body limit.

```hurl
{{#include ../../examples/mocks/ai.hurl:nothing_to_do}}
```

## Edit with AI

**Edit with AI** on the detail page: an instruction ("add a DELETE for orders and a 404
example"), the diff of the spec, the checks of the new version, **Apply as draft**,
then the usual Deploy. The task `edit_spec` returns the whole document; it must keep
the version, parse, pass the checks, differ from the original, and may not remove
more than 40 percent of the operations unless the instruction asks to remove
something.

```hurl
{{#include ../../examples/mocks/ai.hurl:edit}}
```

## Request examples

The task `request_examples` writes parameter sets and request bodies an operation
accepts, validated against its parameter and body schemas, for "Generate arguments"
on an MCP tool and sample requests. This release ships the task (through
`POST /_rustybin/assist/run` with `spec` and `operation`); the MCP inspector's button
comes with the inspector.

## Who may, and what is sent

The instance's key belongs to the operator: on a [self-service](self-service.md)
instance visitors get `403` from the AI routes unless `RUSTYBIN_ASSIST_VISITORS=true`
(their calls count against the same per-caller cap); **Use generated values** works
for them regardless. What goes to the provider is the task's prompt: the description
or instruction, the spec (or the operations and schemas that need examples) and the
style; the "What was sent" record of the console and the `sent` field of the JSON
answers show it exactly, and the [redact pass](../ai-assist.md#privacy-and-costs)
refuses inputs that hold a stored secret or the admin token.

## On every surface

The same job from the API, the console, the command line, an agent and the configuration document.

| Surface | Where |
|---|---|
| API | `POST /_rustybin/mocks/ai/spec`, `POST /_rustybin/mocks/{name}/ai/examples`, `POST /_rustybin/mocks/{name}/ai/edit`, `GET /_rustybin/mocks/{name}/coverage` |
| Console | The wizard's **Describe it** tab, the coverage banner with **Generate with AI** and **Use generated values**, **Edit with AI** on a mock, the assist bar's `create_mock`, `generate_examples` and `edit_spec` cards |
| CLI | `rustybin mocks describe`, `rustybin mocks examples <name> [--apply]`, `rustybin mocks edit <name> "..." [--apply]`, `rustybin mocks coverage <name>` ([reference](../cli.md#mocks)) |
| Management MCP | `generate_spec` (with `create`), `generate_examples` (with `apply`), `edit_spec_with_ai` (with `apply`), `check_mock` and `get_mock` with the coverage; the prompt `mock_an_api` ([agent page](../agent-mcp.md)) |
| `rustybin.yaml` | Stored examples travel with the mock: `mocks.<name>.operations.<id>.examples.<status>` ([configuration as code](config.md)) |
| Environment | The provider and key of [Bring your own AI](../ai-assist.md); `RUSTYBIN_ASSIST_VISITORS` lets self-service visitors use them ([configuration](../configuration.md#ai-assist)) |
