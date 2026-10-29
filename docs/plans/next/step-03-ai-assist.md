# Step 03: AI assist: providers, keys, the assist bar

Master plan: [master-plan.md](master-plan.md). Status: **not started**. Needs: 01, 02. About 2.5 days.

## Goal

A user brings their own key (Anthropic, OpenAI, Gemini, or any OpenAI-compatible endpoint), from
the environment at startup or in Management, AI, and the console turns into an AI-first interface:
an assist bar on every view, sparkle entry points, streamed answers with apply buttons. This step
builds the assist engine and the shell; step 4 adds the mock-building tasks, step 5 the MCP ones.

## Read first

- `src/ai/` (the mock LLM: provider shapes, SSE streaming helpers, `X-Rustybin-*` pacing headers)
  and `docs/src/ai/providers.md`: the request and response formats of the Anthropic Messages API,
  OpenAI Chat Completions and Responses, and Gemini `generateContent` are already modelled there
  for the mock side; the assist client is their mirror image.
- `src/egress.rs` and `src/settings.rs` from step 1.
- `ui/app.js` (topbar, keyboard navigation), `ui/lib/dom.js`, `ui/lib/http.js` (`send({stream:
  true})` and `sseParser`), `ui/views/ai.js` (streaming chat rendering to reuse).
- `src/metrics.rs` (bounded labels; add assist counters), `src/limits.rs`.

## Design

### Providers and models (`src/assist/`)

```
src/assist/mod.rs        Assist state in AppState: settings, the provider client, usage counters
src/assist/provider.rs   trait Provider { complete(req) -> stream of Event; models() }
src/assist/anthropic.rs  Messages API (streaming, tool use for structured output)
src/assist/openai.rs     Chat Completions with response_format json_schema; also the compatible base URL
src/assist/gemini.rs     generateContent with responseSchema
src/assist/models.rs     the curated table: provider, id, tier (fast | capable), context, price per 1M tokens in and out
src/assist/task.rs       Task { name, system, user template, output schema, validate, repair }
src/assist/tasks/        one file per task (this step: explain, suggest, free_answer)
src/assist/api.rs        the routes
```

- Settings (step 1 keys): `assist.provider` (`anthropic` | `openai` | `gemini` | `compatible` | `mock`),
  `assist.model`, `assist.fast_model`, `assist.base_url` (compatible), secrets `assist.anthropic_key`,
  `assist.openai_key`, `assist.gemini_key`, `assist.compatible_key`; env seeds
  `RUSTYBIN_ASSIST_PROVIDER`, `RUSTYBIN_ASSIST_MODEL`, `RUSTYBIN_ASSIST_FAST_MODEL`,
  `RUSTYBIN_ASSIST_BASE_URL`, `RUSTYBIN_ASSIST_ANTHROPIC_KEY`, `RUSTYBIN_ASSIST_OPENAI_KEY`,
  `RUSTYBIN_ASSIST_GEMINI_KEY`, `RUSTYBIN_ASSIST_COMPATIBLE_KEY`. The names say "assist" so they are
  never confused with `RUSTYBIN_AI_API_KEY`, the key the mock LLM *accepts*.
- The curated model table lists, per provider, two or three current models with a tier and a price,
  checked against each provider's model listing endpoint when the step is built (the ids are not
  written into this plan on purpose; they change). Free-text model ids are allowed. The default
  model is the provider's capable tier; `fast_model` is used by cheap tasks (suggestions, explain).
- `mock` provider: the instance's own mock LLM through an in-process call with a scripted rule set
  in `src/assist/mock_rules.yaml`, so tests and demos run every AI path without a key and without
  the network. It answers the tasks' JSON schemas with deterministic content.
- Egress: each provider registers its host as an `Assist` default when its key is set
  (`api.anthropic.com`, `api.openai.com`, `generativelanguage.googleapis.com`; the compatible base
  URL's host when configured). Timeouts 60 s, response cap 2 MiB, no redirects.
- Caps: `RUSTYBIN_ASSIST_MAX_CALLS_PER_MINUTE` (20), `RUSTYBIN_ASSIST_MAX_OUTPUT_TOKENS` (8192), per
  caller (control principal). Usage counters per provider and model (calls, tokens in and out,
  estimated cost, errors) in memory, bounded, in `GET /_rustybin/assist/usage` and the metrics
  `rustybin_assist_calls_total{provider,model,task,outcome}`, `rustybin_assist_tokens_total{...}`.
- Privacy rules in code, not prose: the request builder refuses any input containing a value that
  matches a stored secret or the admin token (a `redact` pass over the task inputs), and the "what
  was sent" record the console shows is exactly what went out.

### Tasks

`Task` is the only way to call a model. `run(task, inputs, model_choice) -> Stream<Event>` where
events are `delta` (text), `json` (the validated structured result), `usage`, `error`. Structured
tasks use the provider's native JSON schema mode (tool use for Anthropic, `response_format` for
OpenAI, `responseSchema` for Gemini) and validate with the instance's JSON Schema validator
(`mocks::schema`); on failure the validation errors go back once or twice (`repair`).

This step's tasks: `free_answer` (the assist bar's chat, with the view's context document),
`explain` (a captured request, a tool, a spec operation), `suggest` (the per-view suggestions,
cached per view per model for an hour).

### Routes

| Route | | |
|---|---|---|
| `GET /_rustybin/assist` | read | Enabled, provider, models, whether a key is set (hint), the curated table, usage summary |
| `POST /_rustybin/assist/test` | admin | Calls the provider once with a one-token prompt; answers latency, model, or the provider's error |
| `POST /_rustybin/assist/run` | read (admin for tasks that write) | `{task, inputs, model?, stream: true}`; SSE of events, or the final JSON |
| `GET /_rustybin/assist/usage` | admin | Counters |

### The console shell

- **Assist bar** (`ui/lib/assist.js`): the input in the topbar, `Cmd+K`, with the current view's
  suggestions (from `suggest`, else a static list per view when there is no key). Submit streams
  `free_answer` into the **drawer** (right side, resizable, remembers its state): markdown rendered
  through the existing safe renderer, code blocks with copy, and **apply cards** when a task returns
  an action (`{action: "create_mock", payload}`; this step defines the card and `open_view`,
  `copy`, `run_request`; later steps add theirs). The drawer keeps the last 20 exchanges in
  sessionStorage.
- **Sparkle actions**: a shared `assistButton(task, inputs, label)` component that views drop where
  the work is. Without a key it renders with a tooltip "Add an AI key in Management, AI" and opens
  that tab on click.
- **Management, AI**: provider picker with the curated models (tier, price), key fields (masked,
  Replace), base URL for compatible, "Test the key", usage this session and since start, the privacy
  note, "Use the built-in mock LLM" for demos.
- **Navigation**: when assist is enabled, the sidebar shows a sparkle next to views that have
  AI actions, and the Overview gets an "Ask Rustybin" card with three suggestions.

### CLI and MCP notes

Step 10: `rustybin ai status|test|run <task> --input file.json|models`. Step 11: `assist_status`,
`run_assist_task` (read tasks only; write tasks are their own tools), `assist_usage`. Keys are never
settable through MCP.

## Tests

- Unit: each provider's request and stream parsing against recorded fixtures, the model table
  shape, the redact pass, caps, usage counters, the task validate and repair loop with the mock
  provider.
- Full app: the routes with and without a key, `test` against the mock provider and against a tiny
  in-process stand-in server for each real provider's wire format (`test_support`), the streaming
  endpoint, denial without rights.
- Console check: the bar without a key (invitation), with the mock provider (a streamed answer and
  an apply card), the Management tab.

## Docs

New book page `docs/src/ai-assist.md` ("Bring your own AI"): keys, providers, privacy, costs, the
assist bar, with Hurl examples against the mock provider. `configuration.md` variables.
`console.md`. The changelog.

## Done when

With `RUSTYBIN_ASSIST_PROVIDER=mock` the console shows the assist bar, suggestions and a streamed
answer with an apply card; with a real key, `test` round-trips; usage and metrics count; all checks
pass; committed and pushed.

## Build notes

(filled in when built)
