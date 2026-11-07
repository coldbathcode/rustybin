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
| 02 | A mocks-only export at version 2 | not started |
| 03 | Fetching a spec from a URL on the server, for an agent | not started |
| 04 | Harness: a leader transfer on purpose, before maintenance | not started |
| 05 | Stateful mode for visitor mocks, with tight caps | not started |
| 06 | Source control: webhooks that trigger a pull | not started |
| 07 | AI mocks: Generate examples per operation, with a persona | not started |
| 08 | Harness: replay captured traffic or a HAR file as a traffic source | not started |
| 09 | MCP-level OAuth on a mock's own MCP server | not started |
| 10 | Harness: OpenTelemetry export | not started |
| 11 | Source control: branches per environment, signed commits, merges | not started |
| 12 | Agents: secrets with an out-of-band confirmation; an interactive sign-in from the CLI | not started |
| 13 | Harness: scenario import, HTTP/3 targets, server-to-server chains | not started |
