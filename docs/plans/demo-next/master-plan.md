# Master plan: demo next, new features for gateway demos

Planned 2026-11-11, when the owner asked what new features could be built and then for them to be
added to the plan with their requirements set out. Nine features and a release, one file each. None of
them exists today (each step file says where things stand); each was chosen for what it shows in an
API or AI gateway demo. Nothing here is built until the owner says go, a step at a time or several at
once.

## 1. What this milestone is

Rustybin so far proves what a gateway does to classic HTTP traffic, imitates AI providers and MCP
servers, and load tests. This milestone turns that into more of the demos people ask for:

- **Proof for AI and MCP gateways** (steps 01, 02): guided scenarios with a pass or fail verdict for
  token limits, model fallback, semantic caching, prompt guards, and attacks on MCP clients.
- **A customer's own API in minutes** (steps 03, 04): a mock recorded from the real upstream, then
  checked against it for drift.
- **The presenter's workflow** (steps 05, 06): a demo as a scripted storyboard, and a customer's whole
  demo as one pack applied and torn down in one step.
- **More of what gateways front** (steps 07, 08, 09): multi-step workflows (Arazzo), realtime and batch
  AI APIs, event-driven APIs (AsyncAPI).
- **A release** (step 10).

## 2. Principles

- **The five-surface rule**: each capability lands on the API (catalogue and OpenAPI), the console,
  the management MCP, the CLI and `rustybin.yaml` in the same step; the parity tests keep it so. A user
  page of the book gets its "On every surface" table and a Hurl example per route.
- **Build on what is there**: a scenario is a row of `src/scenarios/catalog.rs`, a test is a
  `replay::SavedTest`, a reset is a checkpoint, an import lands in an existing model. A new engine only
  where nothing fits (steps 08 and 09).
- **Reaching out is the caller's word and the operator's allowlist**: a feature that calls a host the
  caller names (steps 03, 04, 09 part B) needs `mocks.write` or better and an `egress::Purpose` with no
  default allowlist; `rustybin.yaml` never reaches out when applied.
- **Bounded and secret-free**: every new piece of in-memory state has a cap and a TTL or eviction;
  what is recorded from a real system is redacted as `replay::request_from_capture` does, never stores
  a credential, and never reaches an audit entry.
- **A public instance regrets nothing**: each step says whether self-service visitors get it; the
  default is no for anything that reaches out or keeps a stranger's data.
- **Vendor neutral**: protocols and provider APIs are named, gateway products never are.

## 3. Left out

- **A hosted demo catalogue** (packs shared between instances or published on a site): the platform
  side, not this repository.
- **Audio that sounds like speech** in step 08: the realtime mock streams valid, silent or tone audio
  frames, not synthesised voices.
- **A full message broker** in step 09: Rustybin publishes to and consumes from a broker the operator
  names; it does not become one.
- **Model quality evaluation** (an LLM judging answers): the AI mocks are deterministic on purpose.

## 4. Steps, order and status

The marker a session looks for is the first row that is not done.

| Step | Title | Needs | Size | Status |
|---|---|---|---|---|
| [01](step-01-ai-gateway-scenarios.md) | AI gateway scenarios: token limits, model fallback, semantic cache, prompt guard, provider balancing | | medium | done 2026-11-11 |
| [02](step-02-mcp-security-scenarios.md) | MCP security scenarios: tool poisoning, rug pull, shadowing, result injection, scope overreach | | medium | done 2026-11-11 |
| [03](step-03-record-from-upstream.md) | Record a mock from a real upstream | | large | done 2026-11-11 |
| [04](step-04-contract-drift.md) | Contract drift: a mock checked against the real API it imitates | 03 (shares the purpose) | medium | done 2026-11-11 |
| [05](step-05-demo-storyboards.md) | Demo storyboards: a scripted demo with a talk track and resets | | medium | done 2026-11-12 |
| [06](step-06-demo-packs.md) | Customer demo packs: one apply, one teardown | 05 | medium | done 2026-11-12 (a pack is a project, the recommendation) |
| [07](step-07-arazzo-workflows.md) | Arazzo workflows as multi-step saved tests and harness scenarios | | medium | done 2026-11-12 (multi-step tests and a harness import, the recommendation) |
| [08](step-08-realtime-and-batch-ai.md) | Realtime (WebSocket) and batch AI APIs | | large | done 2026-11-12 (OpenAI-shaped realtime, batch in the OpenAI and Anthropic shapes, the recommendation) |
| [09](step-09-asyncapi-event-mocks.md) | Event-driven mocks from AsyncAPI | | large | planned 2026-11-11: owner decision, section 6 |
| [10](step-10-release.md) | Docs and release | all built | small | the owner's to start |

### Order

| Group | Steps | Why here |
|---|---|---|
| AI and MCP proof | 01, 02 | Reuse the most code (scenario table, AI faults, the mock MCP server) and land in the most asked-for demos |
| Customer's own API | 03, 04 | One shared egress purpose and redaction; 04 is a short step once 03 is in |
| Presenter workflow | 05, 06 | 06 bundles 05's storyboards, so 05 first |
| New protocols and shapes | 07, 08, 09 | Each is independent; 09 is the largest and goes last |
| Release | 10 | One release for whatever is built |

Steps 01, 02, 05 and 07 touch different modules and can be built side by side (one agent and one
worktree each, its own `RUSTYBIN_DOCS_PORT` and `RUSTYBIN_UI_PORT`; expect conflicts only at the ends
of the shared tables: `TOOLS`, `COMMANDS`, the catalogue, the changelog). Steps 03 and 04 go one after
the other. Step 08 and step 09 both add WebSocket servers and are better apart.

### Relation to the core-next plan

This plan follows step 14 of [core-next](../core-next/master-plan.md) (the 0.7.0 release), so nothing
here is built on unreleased code. It does not wait for core-next steps 15 to 26; a step of each may be
in flight at once if they touch different modules. Step 10 here is the release after core-next's step
25 (0.8.0), or folds into it if the owner prefers one release.

## 5. How a session works

As in core-next: read this file, the step file and CLAUDE.md; a failing test first where the
behaviour exists to fail; then the code on every surface, then the docs with a tested example per
route; fmt, clippy on both toolchains and with each feature, the full suite,
`scripts/check-no-dashes.sh`, the docs examples, the include and link checks and, when the console
changed, `conformance/ui`. Add the step's build notes and its "For the merge" text, set the row of
section 4 to done with the date, add the line to the changelog's "Unreleased" section, keep
`docs/src/roadmap.md` current, push.

## 6. Decisions for the owner

Each step can be built with the recommendation; a different answer changes only that step.

| Step | The question | Recommended |
|---|---|---|
| go | Build this plan, and in what order? | After core-next step 14; the order of section 4, 01 and 02 first |
| 02 | Are the attack servers on every instance? | On by default, off in public mode (`mcp.attack_servers`); their content is inert test text, but a public instance has no reason to serve it |
| 03 | How is the spec of a recording made? | Inferred by code from the traffic (deterministic, no key needed); AI assist only as an optional polish through the existing `edit_spec` task |
| 03 | Do visitors of a self-service instance get recording? | No: it reaches out on a caller's word and keeps another system's data |
| 06 | What is a pack, under the hood? | A project (`src/rbac/projects.rs`) plus a document: teardown deletes the project's resources, so nothing is left behind by name |
| 07 | Where do Arazzo workflows run? | As multi-step saved tests (a new `steps` form of `SavedTest`) and as a harness import; not a third engine |
| 08 | Which realtime and batch APIs? | The OpenAI-shaped realtime WebSocket API first (the one gateways most often front); batch for OpenAI and Anthropic shapes |
| 09 | Which transports first? | Part A: AsyncAPI channels Rustybin serves itself (WebSocket, SSE, webhooks), no new dependency; part B: Kafka as a client of an operator-named broker behind the cargo feature `asyncapi-kafka`; MQTT only when asked |
| 10 | Its own release (0.9.0) or folded into 0.8.0? | Its own, unless steps 01 and 02 land before core-next step 25 and the owner wants them sooner |

A row answered no closes its step: the row of section 4 says so with the date, and the roadmap keeps
the line under consideration with the reason.
