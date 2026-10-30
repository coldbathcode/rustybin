# Roadmap

What may come next. Nothing under "Next" or "Under consideration" is available yet;
the pages for the features that exist today describe current behaviour.

## Next

**Rustybin 0.2.2, the hardening release** (planned 2026-10-30, not started): no new features. It
fixes what a code review of 0.2.1 found for public instances: memory and CPU budgets for hostile
specs and queries, MCP saved credentials bound to their target, the console's control token sent
only where the console chose, sign-outs that survive a restart, outbound pinning and the git
transport, an untrusted configuration repository, inert request bins, and the harness's run and
cluster checks. The plan is `docs/plans/hardening/master-plan.md` in the repository.

## Recently built

- [Rustybin 0.2, the AI-assisted mocking service](changelog.md#020-2026-10-30-the-ai-assisted-mocking-service)
  (2026-10-29 to 2026-10-30): the console locked with a user name and password out of
  the box; a settings store with secrets at rest and one outbound policy; your own AI
  key (Anthropic, OpenAI, Gemini or any OpenAI-compatible endpoint) behind an assist
  bar that writes mock specs from a description, fills in missing example payloads and
  edits specs on request; an [MCP inspector](mcp/inspector.md) for any server with
  every auth mode, a relay and test runs; a [mock MCP registry](mcp/registry.md) and
  browsing of real ones; a [dev portal](portal.md) where testers explore and try what
  was published; [AI agents](agent-mcp.md#managing-agents) with rights and revocation;
  the [whole instance as a `rustybin.yaml`](configuration-as-code.md) in
  [git](source-control.md); the [`rustybin` CLI](cli-guide.md) for remote instances;
  and a [management MCP](agent-mcp.md) with a tool for every control route.
- [Load testing through a gateway](harness.md): Rustybin as the load client, with run
  patterns, rate-limit tests, mock-driven traffic and identities, faults and protocol
  sources, clusters with failover, and a conformance run of the compose cluster.
- [An MCP server for every OpenAPI mock](mocks/mcp.md), [OAuth sign-in for
  agents](agent-mcp.md#sign-in-with-oauth), and [self-service mocks](mocks/self-service.md)
  for the hosted demo.

## Under consideration

From the build notes of the 0.2 steps and the harness plan, in no order:

- **AI mocks**: a per-operation "Generate examples" with a persona picker in the
  operation editor (today the dialog is scoped to the operation without its own
  preview); fetching a spec from a URL on the server for an agent (today the agent
  sends the file's content).
- **MCP inspector**: MCP-level OAuth on a mock's own MCP server (the endpoint
  answering `401` with protected resource metadata); client ID metadata documents.
- **Source control**: merges and conflict resolution inside the instance (resolve
  with either side, or in a pull request), branches per environment, signed commits,
  webhooks that trigger a pull (`git.auto: pull` polls instead), `file:` references for
  the harness section, and a mocks-only export at version 2 (`GET /_rustybin/mocks/export`
  still writes version 1).
- **The command line**: `rustybin egress check <url>` (the management MCP's
  `check_egress` does the dry check today); an interactive agent sign-in from the CLI.
- **Agents**: setting a secret from an agent with an out-of-band confirmation (today
  the console, the CLI or the environment); OAuth sign-in for self-service visitors and
  shared-mode key holders.
- **Harness follow-ups** (`docs/plans/harness.md`, open questions): a leader transfer
  on purpose before maintenance; OpenTelemetry export of the run's traces and metrics;
  HTTP/3 targets; replaying captured traffic or a HAR file as a traffic source; scenario
  import from other tools' formats; server-to-server chains for multi-hop meshes (held
  back until the safety model covers outbound traffic from the server role).
- **Self-service across replicas**: visits and visitor mocks live in memory on one
  instance today. A shared store would let a hosted demo run several replicas
  without routing each mock to one of them.
- **Stateful mode for visitor mocks**, with tight caps on what a mock may store
  (items, bytes, text only).
- **A platform authorization server for hosted instances**: Rustybin already
  points its OAuth metadata at an external issuer (`RUSTYBIN_CONTROL_OAUTH_ISSUER`);
  the platform side that issues control tokens to agents is not built.
