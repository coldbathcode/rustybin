# Step 01: AI gateway scenarios

## Goal

A person points a guided scenario at their AI gateway and gets the same proof the HTTP scenarios
give today: "your gateway did X, here is what reached the upstream". The scenarios cover what AI
gateways sell: token based rate limiting, model fallback, semantic caching, prompt guards and
balancing across providers.

## Where things stand

- `src/scenarios/catalog.rs` has nine scenarios (`retries`, `rate_limiting`, `credential_injection`,
  `timeouts`, `caching`, `circuit_breaker`, `request_size_limit`, `cors`, `header_transformation`),
  each with an `Upstream` of `Mock` (a scratch mock) or `Flaky` (a `/flaky` route).
- The AI mocks (`src/ai/`) already provide every upstream behaviour these need: native errors on
  demand (`X-Rustybin-Fail` and `?fail=`: `rate_limit`, `overloaded`, `timeout`, `context_length`,
  `prompt_filter`, ...), deterministic token counts in usage fields and rate-limit headers,
  similarity preserving embeddings (`src/ai/embed.rs`), scripted fake PII (`docs/src/ai/behaviour.md`,
  scripted rules), `X-Rustybin-Provider` and `X-Rustybin-Model` on every answer, and the normalised
  request log at `/ai/requests`.
- Nothing ties the two together: no scenario has an AI upstream, so these demos are done by hand.

## Model

- A new `Upstream::Ai { provider }` in the catalogue: the run's upstream is this instance's
  `/ai/<provider>/...` path with the run's marker, so the check reads what reached the upstream from
  `/ai/requests` filtered to the run (the same isolation the inspector gives the HTTP scenarios:
  one `X-Rustybin-Session` per request, the run's own captures only).
- The run sets the upstream's behaviour through a per-run AI profile (fail kind and rate, token
  pace, mode), held in the run (bounded with the runs, ended with them), never a global setting.
- New rows, each with `proves`, `setup` steps, `params` (rows of the parameter table, so the console
  field, the MCP schema property and the CLI flag follow) and its `evaluate` arm:

| id | Proves | Upstream behaviour | Evaluate |
|---|---|---|---|
| `ai_token_limit` | The gateway limits by tokens, not requests | Fixed token counts per answer | Requests beyond the token budget are refused by the gateway (429 or its own status) and never reach the upstream |
| `ai_model_fallback` | On a 429 or 529 the gateway retries another model or provider | The primary model fails with the chosen kind | The client sees a success; `/ai/requests` shows the primary call then the fallback, with the model and provider of each |
| `ai_semantic_cache` | Similar prompts are answered from the cache | Embeddings and completions as normal | The second and third prompts (paraphrases from the scenario's fixed set) do not reach the upstream; a dissimilar prompt does |
| `ai_prompt_guard` | A guard blocks or redacts a prompt carrying PII or an injection | Normal | The blocked prompt never reaches the upstream; a redacted one arrives without the planted values |
| `ai_response_guard` | A guard redacts PII in an answer | Scripted mode answers with the fake customer record | The client's answer has none of the planted values |
| `ai_provider_balance` | Traffic is spread across providers or keys by the gateway's rule | Two or more provider paths | The split seen in `/ai/requests` is within the parameter's tolerance |

- The planted PII and injection strings are the existing fixed test values (`INJECTION_SAMPLE`, the
  scripted record), so a pass never depends on a model.

## Surfaces

| Surface | What changes |
|---|---|
| API | No new route: `/_rustybin/scenarios*` lists and runs the new rows; the catalogue's examples gain one AI run |
| Console | The Gateway scenarios view groups scenarios as HTTP and AI; the steps panel shows the provider path to configure |
| Management MCP | The `*_gateway_scenario` tools take the new ids and parameters from the table |
| CLI | `rustybin scenario start ai_model_fallback --provider anthropic ...` (flags from the table) |
| `rustybin.yaml` | Nothing new: runs are not instance state |

Docs: `docs/src/gateway-scenarios.md` gains an "AI gateway scenarios" section with a Hurl example per
new scenario, and `docs/src/ai/index.md` links each capability row to its scenario.

## Rules

- Self-service visitors get the AI scenarios when they get the HTTP ones (the same `Refused` checks).
- A run's AI profile never changes how another session's `/ai` requests are answered.
- A new scenario is still a table row plus its `mock` and `evaluate` arms; no special path in
  `scenarios/mod.rs` beyond the new upstream kind.

## Tests that fail first

- Each scenario against the test proxy (`harness/test_proxy.rs` gains the matching reference
  behaviour: a token bucket, a fallback, a cache keyed by embedding similarity, a guard): pass when it
  behaves, fail with the right expectation named when one behaviour is removed.
- Two runs at once in different sessions: neither sees the other's upstream requests.
- Ending a run resets its AI profile; a later request on the same path is answered normally.

## Done when

The six scenarios run on every surface, pass against the reference proxy, fail with a named reason
against a proxy without the behaviour, and are documented with tested examples.

## Build notes (2026-11-11)

Built whole. Where the build differs from the plan above:

- **One request format, two targets, no `provider` parameter.** The check speaks the OpenAI
  chat completions format (what AI gateways most often accept), and the run's upstream is
  `/ai/openai/v1/chat/completions`; the scenarios that need a second target
  (`ai_model_fallback`, `ai_provider_balance`) get `/ai/v1/chat/completions`, the same API at
  another path, so no gateway has to translate formats and the check tells the targets apart by
  the captured path. Any other provider path of the instance counts as the fallback, for a
  gateway that falls back across providers. `Upstream::Ai { second }`, `second_upstream` on the
  run, `{second_upstream}` and `{base}` in the setup.
- **What reached the model comes from the inspector, not `/ai/requests`.** The captures already
  carry the path, the status and the body per run session (the existing isolation); `Sent` keeps
  the paths and bodies (bounded, never serialised). The gateway's own embedding calls are told
  apart by path and not counted as model calls.
- **The profile lives on the run and is applied in `ai::ai_layer`.** `AiShared` holds
  `state.scenarios`; `Scenarios::ai_profile(session)` matches the run's secret session prefix
  only, so a forged prefix without the secret, or anyone else, gets the mock as before. A
  scenario's fault is applied before the request's own header; its mode over the request's.
  Ending the run reports `ai_profile` among what it removed.
- **Reference behaviours are in the scenarios' own stand-in gateway** (`AiGw` in
  `src/scenarios/tests.rs`, beside the HTTP one), not `harness/test_proxy.rs`: a token budget
  (and a request limit, which must fail the token line), a fallback, a semantic and an exact
  cache (the exact one must fail), a blocking and a redacting guard, a response guard, round
  robin.
- **Parameters**: `token_limit`, `fail_kind` (a new `Kind::Choice`, rendered as a select in the
  console and an `enum` in the schemas), `skip_injection` (a switch defaulting to off, so the CLI
  can turn it on; the plan's wording was a check to turn off) and `min_share`. Planted values
  are the AI mock's scripted record (`engine::PII_TEXT`) and `INJECTION_SAMPLE`; a test keeps them
  in step.
- **The semantic cache check asks another question each time.** A semantic cache rightly takes
  the same question with another reference word for the same prompt, so a second check on a warm
  cache failed in the first build. Twelve questions and twelve different prompts in turn, from a
  point chosen at random per run; a test checks every rewording is at 0.9 or more of its question
  under the mock's embeddings and nothing of another round is near.
- **`ai_token_limit`'s band**: the first `429` after the budget less two requests' worth up to a
  quarter over plus two requests' worth, so a gateway counting completion tokens only, or checking
  `max_tokens` up front, passes, and a request limit does not.

### For the merge

Touches the scenario table and engine, `ai::ai_layer` (one lookup per AI request, a prefix test
first), the CLI's `scenario check` flags, the console's scenarios view and the scenario tool
descriptions. No route, tool, permission or setting is new; the README tables and the CLI
reference were regenerated for the changed summaries.
