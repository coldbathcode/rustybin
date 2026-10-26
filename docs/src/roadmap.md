# Roadmap

What may come next. Nothing under "Next" or "Under consideration" is available yet;
the pages for the features that exist today describe current behaviour.

## Next

- **Harness mode, the next phases** (`docs/plans/harness.md`): phase 3, the cluster.
  A coordinator that client and server nodes join, heartbeats with leases (a client
  stops when it loses its leader), clock sync, the cluster-wide rate split over the
  clients by capacity, synchronised starts, merged histograms and one report, the
  server nodes exported as a gateway's upstream pool, and headless clusters that
  start on quorum. Then failover with an elected leader (phase 3b), every pattern
  (step, target, breakpoint, spike, pulse, burst, wave) with live tuning (phase 4),
  and the rate-limit test that check rate limiting (phase 5).

## Recently built

- [Harness correlation](configuration.md#harness-mode) (phase 2): every harness
  request is signed, the server side tags and times every answer, and the report
  tells `upstream` from `gateway` from `transport` from `lost` requests, with the
  gateway's overhead, what it changed on the way (headers added, removed, rewritten,
  bodies, paths), one-way legs in loop-through, sampled inspector captures,
  `rustybin_harness_*` metrics and a shaped upstream at `/harness/{*path}`.
- [Harness mode, single node](configuration.md#harness-mode) (phase 1): Rustybin as
  the load client. A scenario with targets, layers, open or closed load models,
  constant and ramp patterns and thresholds runs from the API, the management MCP,
  the console builder (generated from one JSON Schema), headless at startup or
  `rustybin harness run --local-server`. Targets are allowlisted and pinned, the
  client watches itself for saturation, reports come as JSON, text and JUnit.

- [An MCP server for every OpenAPI mock](mocks/mcp.md): deploy a mock and get an MCP
  server for the same API at `/mock/<name>/_rustybin/mcp`, one tool per operation,
  answered by the mock (rules, examples, stateful data, mocked security). For MCP
  gateway demos with the customer's own API instead of fake weather tools.
- [OAuth sign-in for agents](agent-mcp.md#sign-in-with-oauth): connect an agent to
  `/_rustybin/mcp` with no token; it signs in through a consent page in the console.
- Mock collections as a Bruno collection and an Insomnia export, next to Postman,
  Hurl, curl, `.http` and k6.
- [Configure Rustybin from an AI agent](agent-mcp.md): a management MCP server at
  `/_rustybin/mcp`, protected like the rest of the control plane, so an agent such
  as Claude Code can create and deploy mocks, change their responses and security,
  send test requests, mint tokens and read captured traffic for you. It is separate
  from the mock MCP server at `/mcp`, which stays a test upstream for MCP gateways.
- [Self-service mocks](mocks/self-service.md): visitors of a public instance create
  and manage their own OpenAPI mocks on a time allowance (an hour, extendable to
  three, then an email address for up to six hours a day), on a separate mocks
  domain. Off by default; built for the hosted demo at rustybin.dev.

## Under consideration

- **Self-service across replicas**: visits and visitor mocks live in memory on one
  instance today. A shared store would let a hosted demo run several replicas
  without routing each mock to one of them.
- **Stateful mode for visitor mocks**, with tight caps on what a mock may store
  (items, bytes, text only).
- **A platform authorization server for hosted instances**: Rustybin already
  points its OAuth metadata at an external issuer (`RUSTYBIN_CONTROL_OAUTH_ISSUER`);
  the platform side that issues control tokens to agents is not built.
