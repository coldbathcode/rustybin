# Bring your own AI

The console's AI features run on **your** model provider and key: Anthropic (the
Messages API), OpenAI (Chat Completions), Gemini (`generateContent`), any
OpenAI-compatible endpoint (Ollama, vLLM, a gateway's AI route), or the built-in
mock provider for demos and tests. Nothing calls a model until a key is set; with one,
an **assist bar** sits in the console's top bar (`Ctrl+K` / `Cmd+K`) with suggestions
for the view you are on, answers stream into a drawer, and **apply cards** do what
the answer proposes (open a view, copy a command, send a request) only when you
click them. This page is the engine and the shell; the mock-building tasks (describe an API,
generate examples, edit a spec) are on [Build mocks with AI](mocks/ai.md); the MCP
tasks come with the inspector.

## Set it up

From the environment at startup, or under **Management, AI** at any time (the
[instance settings](configuration.md#state-directory-settings-and-secrets) `assist.*`;
a value saved in the console wins over the variable):

```bash
RUSTYBIN_ASSIST_PROVIDER=anthropic RUSTYBIN_ASSIST_ANTHROPIC_KEY=sk-ant-... rustybin
RUSTYBIN_ASSIST_PROVIDER=openai    RUSTYBIN_ASSIST_OPENAI_KEY=sk-...        rustybin
RUSTYBIN_ASSIST_PROVIDER=gemini    RUSTYBIN_ASSIST_GEMINI_KEY=AIza...       rustybin
# any OpenAI-compatible endpoint; a local one also needs the outbound loopback switch
RUSTYBIN_ASSIST_PROVIDER=compatible RUSTYBIN_ASSIST_BASE_URL=http://localhost:11434/v1 \
  RUSTYBIN_ASSIST_MODEL=llama3.2 RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true rustybin
# the built-in provider: deterministic answers, no key, no network
RUSTYBIN_ASSIST_PROVIDER=mock rustybin
```

| Setting | Variable | Meaning |
|---|---|---|
| `assist.provider` | `RUSTYBIN_ASSIST_PROVIDER` | `anthropic`, `openai`, `gemini`, `compatible` or `mock`; empty: off |
| `assist.model` | `RUSTYBIN_ASSIST_MODEL` | The model for specs, edits and answers; empty: the provider's default from the curated table |
| `assist.fast_model` | `RUSTYBIN_ASSIST_FAST_MODEL` | The model for cheap tasks (suggestions, explanations); empty: the provider's fast default |
| `assist.base_url` | `RUSTYBIN_ASSIST_BASE_URL` | The compatible endpoint's base, without `/chat/completions` |
| `assist.anthropic_key`, `assist.openai_key`, `assist.gemini_key`, `assist.compatible_key` | `RUSTYBIN_ASSIST_*_KEY` | One secret per provider, sealed at rest, returned as `{set, hint}` only |

**Management, AI** shows the state (on, or why not), the curated models of every
provider with their tier (`fast` or `capable`) and list price where known, with
**Use** and **Fast** buttons, the key fields (masked, Replace), the compatible base
URL, **Test the key** (one short call: latency, model, the answer), the usage since
start and the privacy note. **Use the built-in mock LLM** switches to the mock provider
for a demo without a key.

```hurl
{{#include ../examples/assist/assist.hurl:status}}
```

```hurl
{{#include ../examples/assist/assist.hurl:settings}}
```

The provider's host is allowed under the [outbound policy](configuration.md#outbound-connections)'s
`assist` purpose the moment its key is set (`api.anthropic.com`, `api.openai.com`,
`generativelanguage.googleapis.com`, the compatible base URL's host); calls carry a
60 second timeout, a 2 MiB response cap, pinned addresses and no redirects. In public
mode no feature may reach any host, so only the mock provider works there.

## The console

- **The assist bar** (top bar, `Ctrl+K` / `Cmd+K`): "Ask Rustybin". Focusing it shows
  three or four suggestions for the current view (a static list; with a real provider
  the `suggest` task refines them, cached for an hour per view and model). Submitting
  streams the answer into the **drawer** on the right: markdown with copyable code
  blocks, the provider and model, the tokens used and the estimated cost (where the
  list price is known), and **What was sent**, the exact prompts that went out. The
  drawer resizes, keeps the last 20 exchanges for the browser tab, and reopens where
  the tab left it.
- **Apply cards.** An answer may end with actions: `open_view` (a console view),
  `copy` (text to the clipboard), `run_request` (a request to this instance's own data
  plane, whose status and body appear under the card), `create_mock` (the wizard's
  Describe tab with the description filled in), `edit_spec` and `generate_examples`
  (the dialogs of a mock). Nothing is applied without a click.
- **Sparkle buttons** where the work is: **Explain** on a captured request in Live
  traffic, **Describe it**, **Generate with AI** and **Edit with AI** on mocks; generate
  arguments on MCP tools and write a scenario on the harness builder come with their
  steps. Without a key they say so and open Management, AI.
- **The overview** gets an **Ask Rustybin** card with three suggestions, and the
  navigation marks the views with AI actions with a sparkle once assist is on.
- **Without a key** the bar reads "Add an AI key to describe what you want" and opens
  Management, AI. No feature is hidden, none is a dead end.

## Tasks

A task is the only way to call a model: a name, a system prompt, a user prompt rendered
from validated inputs, an optional output JSON schema and a deterministic mock answer.
Structured tasks use the provider's native JSON mode (tool use on Anthropic,
`response_format: json_schema` on OpenAI, `responseSchema` on Gemini, `json_object` plus
the schema in the prompt on a compatible endpoint) and are validated with the same JSON
Schema validator the mocks use; an invalid answer goes back to the model with the
errors, at most twice.

| Task | Inputs | Answer | Tier |
|---|---|---|---|
| `free_answer` | `question`, `view`, `context` (what the view shows, at most 32 KiB) | Markdown, with action blocks | capable |
| `explain` | `kind` (`request`, `tool`, `operation`, `text`), `subject` | `{summary, details, gateway_notes, next_steps}` | fast |
| `suggest` | `view`, `context` | `{suggestions: [{label, prompt}]}` (three or four), cached per view and model for an hour | fast |
| `spec_from_description` | `description`, `openapi_version`, `style` | An OpenAPI document, parsed and checked ([mocks](mocks/ai.md)) | capable |
| `examples_for_operations` | `spec`, `operations`, `per_response`, `persona` | `{examples: {op: {status: [...]}}}`, each validated against its schema | capable |
| `edit_spec` | `spec`, `instruction` | The whole document, checked, with a bounded structural diff | capable |
| `request_examples` | `spec`, `operation`, `count` | `{requests: [{path, query, headers, body}]}`, validated | fast |

`POST /_rustybin/assist/run` runs one. Streamed (`stream: true`, or `Accept:
text/event-stream`), the events are `sent` (what goes to the provider, never the
key), `delta` (`{text}`), `json` (the validated structured answer), `action`, `usage`
(`input_tokens`, `output_tokens`, `estimated_cost_usd`, `cached`), `error` and `done`.
Otherwise one JSON document comes back:

```hurl
{{#include ../examples/assist/assist.hurl:ask}}
```

```hurl
{{#include ../examples/assist/assist.hurl:stream}}
```

```hurl
{{#include ../examples/assist/assist.hurl:explain}}
```

```hurl
{{#include ../examples/assist/assist.hurl:suggest}}
```

```hurl
{{#include ../examples/assist/assist.hurl:refusals}}
```

With control-plane auth, read tasks are open to the `console` scope; tasks that change
the instance need admin, like `test` and `usage`.

## Privacy and costs

- **Keys never leave the instance**: not in the API (`{set, hint}`), not in exports,
  not in captures, not in prompts. The request builder scans every prompt for the
  value of any stored secret and the admin token and refuses the call, naming the
  setting, never the value:

```hurl
{{#include ../examples/assist/assist.hurl:secret}}
```

- **What was sent** in the drawer is the record the server made: the provider, the
  model, the system prompt, the user prompt and the schema.
- **Caps**: `RUSTYBIN_ASSIST_MAX_CALLS_PER_MINUTE` (20) per caller (the control
  principal, plus the client address on an open control plane) answers `429` with
  `Retry-After`; `RUSTYBIN_ASSIST_MAX_OUTPUT_TOKENS` (8192) bounds every answer. A
  reasoning model (OpenAI's GPT-5 and o-series) spends part of that budget thinking:
  Rustybin asks it for low reasoning effort, and an answer the budget cuts off is
  reported as "the model hit its output token limit" rather than used.
- **Usage**: `GET /_rustybin/assist/usage` (admin) counts calls, errors, tokens and the
  estimated cost since start, in total, per provider and model (at most 64 series)
  and per task; the metrics `rustybin_assist_calls_total{provider,model,task,outcome}`
  and `rustybin_assist_tokens_total{provider,model,direction}` count the same (the
  model label is bounded to 32 values, then `other`). Costs use the curated table's
  list prices; a model without one shows no number.

```hurl
{{#include ../examples/assist/assist.hurl:test}}
```

```hurl
{{#include ../examples/assist/assist.hurl:usage}}
```

## The mock provider

`RUSTYBIN_ASSIST_PROVIDER=mock` (or **Use the built-in mock LLM**) answers every task
deterministically from the instance itself: the free answer names the view and the
question and proposes two actions, `explain` summarises the subject and flags headers
that look gateway-added, `suggest` returns the static list. It is how the console
check, the documentation examples and the tests exercise every AI path without a key
or the network, and how a demo shows the shell before a key is at hand.

## On every surface

The same job from the API, the console, the command line, an agent and the configuration document.

| Surface | Where |
|---|---|
| API | `GET /_rustybin/assist`, `POST /_rustybin/assist/run`, `POST /_rustybin/assist/test`, `GET /_rustybin/assist/usage` |
| Console | The assist bar (`Ctrl+K`), the sparkles in every view, **Management, AI** |
| CLI | `rustybin ai status | models | usage | test | run`, `rustybin settings set assist.provider ...`, `rustybin settings set-secret assist.anthropic_key` ([reference](cli.md#ai)) |
| Management MCP | `assist_status`, `assist_usage`, `test_assist`, `run_assist_task` (the read tasks); keys are never tool inputs ([agent page](agent-mcp.md)) |
| `rustybin.yaml` | `settings: assist.provider, assist.model, assist.fast_model, assist.base_url`; the keys under `secrets:` as `${VAR}` or `secret:stored` references, never values |
| Environment | `RUSTYBIN_ASSIST_PROVIDER`, `_MODEL`, `_FAST_MODEL`, `_BASE_URL`, `_ANTHROPIC_KEY`, `_OPENAI_KEY`, `_GEMINI_KEY`, `_COMPATIBLE_KEY`, `_MAX_CALLS_PER_MINUTE`, `_MAX_OUTPUT_TOKENS`, `_VISITORS` ([configuration](configuration.md#ai-assist)) |
