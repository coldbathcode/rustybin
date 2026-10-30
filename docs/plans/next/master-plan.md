# Master plan: Rustybin 0.2, the AI-assisted mocking service

Planned 2026-10-29. Status: **in progress: steps 01 to 06 done**. The build marker is the status table in
section 4; every step has its own file in this folder and this file is the one to read first in
every session.

## 1. What 0.2 is

0.1 made Rustybin a stub service with mocks, a mock LLM, MCP and A2A servers, an identity provider, a
management MCP server and a load harness. 0.2 turns an instance into something a team runs as a
service: a mocking and testing portal that is **set up by describing what you need**, **explored by
the people who test against it**, **managed from a CLI, an agent, a config file in git or the
console**, and **safe to put behind a password on day one**.

The eight asks, in the user's words, and where each lands:

| Ask | Steps |
|---|---|
| 1. An MCP inspector that works on any MCP server, picks from the deployed mock MCPs, has every auth mode, and tests registries (mocked or real) | [05 MCP inspector](step-05-mcp-inspector.md), [06 MCP registry](step-06-mcp-registry.md) |
| 2. The console locked with a username and password, default `rustybin` / `rustybin`, overridable by env | [02 Console sign-in](step-02-console-sign-in.md) |
| 3. A user's own Claude, OpenAI or Gemini key for AI features, from env at startup and changeable in settings | [03 AI assist](step-03-ai-assist.md) |
| 4. AI that writes specs from a prompt (to the spec version the user picks), fills in missing example payloads for APIs and MCP tools, alerts the user, and edits specs on request | [04 AI mocks](step-04-ai-mocks.md) |
| 5. A dev portal where testers explore and consume the published APIs, MCPs, LLM and agent surfaces | [07 Dev portal](step-07-dev-portal.md) |
| 6. The whole instance as a YAML file in git, with settings, management, source control, add, push, pull, load from the source of truth | [09 Config and source control](step-09-config-and-source-control.md) |
| 7. A `rustybin` CLI with all the functionality | [10 CLI](step-10-cli.md) |
| 8. A management MCP that covers every capability, so an agent can build an instance | [11 Management MCP parity](step-11-management-mcp.md) |

Plus two things the asks need underneath, and one carried over:

- [01 Foundations](step-01-foundations.md): a persisted instance settings store with secrets at rest,
  one egress policy for every outbound call, and the Management area of the console that steps 2, 3,
  7, 8 and 9 fill.
- [08 AI agents](step-08-agents.md): the agents management view agreed before this plan (who talks
  to the instance over MCP, revoke, rights).
- [12 Release 0.2.0](step-12-release.md): the docs sweep, conformance, changelog, tag, image.

## 2. Principles

These hold for every step. A step file repeats the ones it leans on.

- **AI is optional and user-keyed.** Nothing calls a model until the user puts a key in. Without a key
  every AI entry point is a visible invitation, never a dead button. With a key, AI is the first thing
  the console offers on every view where it helps (section 3). Keys never leave the instance: not in
  exports, not through MCP, not in logs, not in captures.
- **Every capability on five surfaces.** API (catalogue and OpenAPI), console, management MCP, CLI
  and `rustybin.yaml`. The parity tests of steps 10 and 11 fail when a control route has no CLI
  command or MCP tool. A feature lands on all five in the same step, or its step is not done.
- **Vendor neutral.** Providers of AI, git hosting and registries are named by their API shape
  (OpenAI-compatible, Anthropic Messages, Gemini, a git remote, an MCP registry), never by a gateway
  vendor's branding. No vendor-specific UI.
- **Outbound traffic is allowlisted.** Every new way the instance reaches out (AI providers, MCP
  servers, registries, git remotes) goes through the egress policy of step 1, with forbidden ranges,
  pinning and audit, exactly as the harness allowlist works today.
- **Bounded state.** Every new in-memory structure has a capacity and an eviction rule. Persisted
  state lives under one `RUSTYBIN_STATE_DIR`.
- **Secrets at rest are encrypted** (step 1), compared in constant time, shown masked, rotated by
  replacing, and never written to the config export in the clear (environment references instead).
- **No breaking change without a switch and a line in the changelog.** The console lock (step 2) is
  the one deliberate change of default; `RUSTYBIN_CONTROL_AUTH=open` keeps the old behaviour.
- The repository's hard rules stand: no em or en dash anywhere, no `unwrap` or `expect` on request
  paths, HTML-escape reflected input, fast tests with `test_support`, MSRV 1.88, `cargo clippy
  --all-targets -- -D warnings` clean, `cargo fmt`.

## 3. The console with AI front and center

The user asked for AI to lead the way through the interface when it is enabled. The shape, built
in step 3 and filled by later steps:

- **The assist bar.** A single input at the top of every view (`Cmd+K` / `Ctrl+K`, and a sparkle
  button in the top bar): "Describe what you want to mock or test". It knows the current view and
  offers three or four suggestions per view ("Describe an API to mock", "Generate examples for the
  petstore mock", "Explain the tools of this MCP server", "Write a load test for the gateway").
  Answers stream into a drawer on the right with **apply** buttons that do the thing (create the
  mock, fill the examples, open the inspector with the arguments ready). Nothing is applied without
  a click.
- **Entry points in place.** Every view that AI helps gets a sparkle action where the work happens:
  the mock wizard's "Describe it" tab, "Edit with AI" and "Generate examples" on a mock, "Generate
  arguments" on an MCP tool, "Write this scenario" on the harness builder, "Explain" on a captured
  request.
- **Without a key** the bar reads "Add an AI key to describe what you want" and opens Management,
  AI; the sparkle actions show the same hint on hover. No feature is hidden, none is a dead end.
- **Transparency.** The drawer shows the model, the tokens used and an estimated cost per answer, a
  "what was sent" disclosure (never the key), and a usage counter in Management, AI. A user can pick a
  cheaper model per task.
- **The portal is not AI-driven**: testers read it. AI is for the people who build the instance.

## 4. Steps, order and status

Dependencies run top to bottom; a step needs the ones it lists. Dates are filled in as steps land.
The marker a new session looks for is the first row that is not done.

| Step | Title | Needs | Days | Status |
|---|---|---|---|---|
| [01](step-01-foundations.md) | Foundations: settings store, secrets, egress policy, Management area | | 2 | done 2026-10-29 |
| [02](step-02-console-sign-in.md) | Console sign-in with username and password | 01 | 1.5 | done 2026-10-29 |
| [03](step-03-ai-assist.md) | AI assist: providers, keys, the assist bar | 01, 02 | 2.5 | done 2026-10-29 |
| [04](step-04-ai-mocks.md) | AI mocks: specs from a prompt, examples, AI edit | 03 | 3 | done 2026-10-29 |
| [05](step-05-mcp-inspector.md) | MCP inspector for any server, every auth mode, test runs | 01, 03 | 3 | done 2026-10-30 |
| [06](step-06-mcp-registry.md) | MCP registry: a mock registry and inspecting real ones | 05 | 1.5 | done 2026-10-30 |
| [07](step-07-dev-portal.md) | Dev portal: publish and consume | 01, 02 | 3 | done 2026-10-30 |
| [08](step-08-agents.md) | AI agents: who talks to the instance over MCP | 01, 02 | 2 | not started |
| [09](step-09-config-and-source-control.md) | The whole instance as YAML, and git | 01, 07, 08 | 3 | not started |
| [10](step-10-cli.md) | The `rustybin` CLI for remote instances | 02 to 09 | 2.5 | not started |
| [11](step-11-management-mcp.md) | Management MCP parity and the build-an-instance prompts | 02 to 10 | 2 | not started |
| [12](step-12-release.md) | Docs sweep, conformance, release 0.2.0 | all | 1.5 | not started |

About 27 days of build sessions. Steps 05 and 06 can run before 04 if the MCP work is wanted
sooner; 07 and 08 can run in either order. Everything else is in order.

## 5. How a session works

1. Read this file's section 4, open the first step that is not done, read its "Read first" list.
2. Build the step completely: model, API, console, MCP tools, CLI (once step 10 exists; before
   that, note the commands the step will need in its file), `rustybin.yaml` section, tests, docs
   (the book, the README tables when routes change, the changelog entry under "Unreleased").
3. Run the checks: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
   `python3 docs/tools/check_includes.py`, `mdbook build docs`, `scripts/check-no-dashes.sh`,
   `docs/examples/run.sh` when examples changed, and the console check
   `PYTHON=<venv>/bin/python conformance/ui/run.sh` when the console changed.
4. In the step file, set the status to done with the date, and write the **Build notes**: what
   differs from the plan and why, what was left out. Update the row in section 4 of this file.
5. Commit as oliwynn with a message that starts with the step number and title (`Next 03: AI assist:
   providers, keys, the assist bar`), the attribution line the session carries, and push to `main`.
   A step that cannot be finished in a session is still committed at a green state (tests pass,
   docs match what exists) with its status set to "in progress" and a note of what is left.
6. Never leave the plan and the code out of step: a departure goes into the build notes the same
   session.

## 6. Shared design decisions

Settled here so the steps do not re-decide them.

- **One state directory.** `RUSTYBIN_STATE_DIR` (default unset: everything in memory, as today).
  When set, mocks go to `<dir>/mocks` (today's `RUSTYBIN_MOCKS_STATE_DIR` keeps working as an
  override), harness scenarios to `<dir>/harness`, instance settings to `<dir>/settings.json`, the
  secrets key to `<dir>/secrets.key`, the git working copy to `<dir>/repo`, the portal's counters
  to `<dir>/portal.json`. Files are created with mode 0600, directories 0700.
- **Env seeds, settings override, lock wins.** A setting that exists both as an environment
  variable and in the settings store takes the stored value when one has been saved from the
  console, CLI or MCP, else the environment's. `RUSTYBIN_SETTINGS_LOCK=true` makes the environment
  authoritative and the console read-only for those settings (for deployments managed by a
  platform or by git). `GET /_rustybin/settings` says where each value comes from.
- **Secrets.** Encrypted at rest with ChaCha20-Poly1305 under a 32-byte key from
  `RUSTYBIN_SECRETS_KEY` (base64) or `<state>/secrets.key` generated on first start. Returned by
  the API as `{ "set": true, "hint": "...k3Qx" }`, never the value. Config exports write
  `${RUSTYBIN_ASSIST_OPENAI_KEY}`-style references, never values.
- **Egress policy.** `src/egress.rs` with `RUSTYBIN_EGRESS_ALLOW` (hosts, `*.domain`, IPs, CIDRs,
  optional ports) plus per-feature defaults that are on only when the feature is configured: the AI
  provider's host when its key is set, the git remote's host when configured, the registry host
  `registry.modelcontextprotocol.io`. Always refused: link-local, metadata, multicast, loopback
  unless `RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true` (local demos), and the instance's own control plane.
  Names are resolved once per call and pinned. Every destination is audited (`rustybin::egress`).
  The harness keeps `RUSTYBIN_HARNESS_TARGETS` as its own, stricter list.
- **Control tokens for everything.** The password sign-in mints the same HS256 control JWTs the
  OAuth sign-in mints, so the whole control plane, the console, the CLI and the MCP server share
  one credential model: admin token, control JWT (scopes `admin`, `console`, `inspector`,
  `harness`), API key in shared mode.
- **Published, not public.** The portal shows only what was published; publishing is explicit and
  reversible; the portal needs no console credential; it can itself be put behind a password or an
  API key.
- **AI tasks are named and structured.** Every AI call is a task in `src/assist/tasks/` with a
  prompt, an output JSON schema, a validator and a repair loop (at most two retries with the
  validation errors fed back). Tasks are what the API, the console, the CLI and the MCP tools call;
  nothing builds prompts elsewhere.

## 7. Risks

- **Model output quality varies.** The repair loop plus the existing spec parser and deploy checks
  keep bad output out of the instance; the user always sees a diff before anything is applied.
- **Git in the image.** Step 9 adds the `git` binary to the runtime image (about 10 MB). The
  alternative, a Rust git library, is heavier to build and audit. Decision: the binary.
- **The console lock changes the default.** Existing deployments that relied on an open console
  get the default password until they set `RUSTYBIN_CONTROL_AUTH=open` or a password. The startup
  log and the changelog say so.
- **Browser limits on the inspector.** Many MCP servers send no CORS headers; the relay in step 5
  is what makes "any MCP server" true, and it is where the egress policy applies.
- **Scope.** Twelve steps is a lot. Each step is shippable alone; the order keeps the user-facing
  value early (sign-in, AI, inspector) and the integration work (CLI, MCP parity) after the
  features exist to integrate.
