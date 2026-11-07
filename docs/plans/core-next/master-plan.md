# Master plan: the rest of the roadmap

Planned 2026-11-07, when the owner said to build everything left and put the best possible Rustybin
instances first, leaving `rustybin-platform` for now. The steps are the "Under consideration" list of
`docs/src/roadmap.md`, ordered by what an instance gains for what it risks. Each step is built whole,
committed and pushed as it lands, under the five-surface rule. One release at the end.

## 1. Principles

- **An instance first**: what a person running Rustybin gains, before what a hosted service would.
- **Nothing that a public instance would regret**: a feature that reaches out goes through
  `egress::Egress`, a visitor gets it only when the threat model says so.
- **As in the last plans**: a failing test first, a timing change measured under the parallel suite, a
  Hurl example per route, the changelog, `docs/src/roadmap.md` kept current.

## 2. Left out

- **A platform authorization server**: the platform side, not this repository.
- **Self-service across replicas**: needs a shared store and a second instance to prove it; the demo is
  one instance by the owner's decision of 2026-10-18. Last, if at all.

## 3. Steps, order and status

The marker a session looks for is the first row that is not done.

| Step | Title | Status |
|---|---|---|
| 01 | `rustybin egress check <url>`: the dry check on every surface (`GET /_rustybin/egress/check`, `Egress::dry_check`) | done 2026-11-07 |
| 02 | A mocks-only export at version 2 | done 2026-11-07 |
| 03 | Fetching a spec from a URL on the server, for an agent (`mocks/fetch.rs`, `Purpose::SpecFetch`; `rustybin.yaml` has no `spec_url` on purpose: applying a file never reaches out) | done 2026-11-07 |
| 04 | Harness: a leader transfer on purpose, before maintenance (`Cluster::step_aside`: the leader answers as one without a quorum, so the ordinary election runs and no protocol is new) | done 2026-11-07 |
| 05 | Stateful mode for visitor mocks, with tight caps (`RUSTYBIN_MOCKS_SELF_SERVICE_STATEFUL`, off by default: the owner's decision of 2026-10-18 stands unless they turn it on) | done 2026-11-07 |
| 06 | Source control: webhooks that trigger a pull (`POST /_rustybin/git/webhook`, HMAC signed, off until `git.webhook_secret` is set) | done 2026-11-07 |
| 07 | AI mocks: Generate examples per operation, with a persona (mostly built already: persona and scope were on every surface and the dialog previews; added suggestions and Generate more) | done 2026-11-07 |
| 08 | Harness: replay captured traffic or a HAR file (`harness/import.rs`: an import into `source.requests`, not a new source, so the engine and the safety checks are unchanged; credentials removed) | done 2026-11-07 |
| 09 | MCP-level OAuth on a mock's own MCP server (`mcp.oauth`, through the same `auth::require_bearer_for` as `/mcp/protected`; not on a visitor's mock) | done 2026-11-07 |
| 10 | Harness: OpenTelemetry export (`harness/otlp.rs`: metrics of finished runs as OTLP over HTTP with JSON, no new dependency; traces are not sent, a run has no spans) | done 2026-11-07 |
| 11 | Source control: branches per environment, signed commits, merges | not built: owner decision, section 4 |
| 12 | Agents: secrets with an out-of-band confirmation; an interactive sign-in from the CLI | not built: owner decision, section 4 |
| 13 | Harness: scenario import, HTTP/3 targets, server-to-server chains | not built: owner decision, section 4 |

Released as 0.6.0 after step 10.

## 4. Why steps 11 to 13 stopped

Each of these changes a rule the project set on purpose, or needs a call only the owner can make, so
they wait for one rather than a guess:

- **Merges inside the instance**: source control is "never a merge" by design; a diverged branch
  answers `409` with the two ways out. Changing that decides who resolves a conflict and how a
  repository writer stays limited. Branches per environment and signed commits are smaller, and
  turn on what key signs and whether a document may name a branch.
- **Secrets through an agent**: "a secret is never a tool input" is a hard rule, tested by
  `every_tool_is_fully_defined`. An out-of-band confirmation is an exception to it, and what confirms
  (a console prompt, a one-time link) is the design.
- **An interactive agent sign-in from the CLI** and **OAuth for visitors and key holders**: the second
  reopens what step 04b of the hosted demo closed on purpose.
- **Server-to-server chains**: the harness plan holds them back until the safety model covers outbound
  traffic from the server role.
- **HTTP/3 targets**: a QUIC stack in the load engine, a new dependency with a large surface, for a
  protocol few gateways under test speak.
- **Scenario import from other tools**: which tools, and how much of each one's model, is a choice
  of what to support for years.
