# Roadmap

What may come next. Nothing under "Next" or "Under consideration" is available yet;
the pages for the features that exist today describe current behaviour.

## Next

**Rustybin 0.2, the AI-assisted mocking service** (`docs/plans/next/master-plan.md`, planned
2026-10-29, nothing built yet): the console locked with a username and password out of the
box; bring your own AI key (Anthropic, OpenAI, Gemini or any OpenAI-compatible endpoint) and an
AI-first console that writes mock specs from a description, fills in missing example payloads
and edits specs on request; an MCP inspector for any server with every auth mode, a relay and
test runs; a mock MCP registry and browsing of real ones; a dev portal where testers explore
and try what was published; management of the coding agents that talk to the instance; the
whole instance as a `rustybin.yaml` in git with push, pull and load at startup; a `rustybin`
CLI for remote instances; and a management MCP that covers every capability so an agent can
build an instance. Twelve steps, each committed and pushed as it lands.

## Recently built

- [Load testing through a gateway](harness.md) (harness phase 8, the last): the guide
  with tested examples, a compose cluster with a leader, a standby, two clients, two
  servers and a stand-in gateway, the harness conformance run (the leader killed
  mid-run, the standby's report asserted) and the harness tools on the
  [agent page](agent-mcp.md#harness).
- [Harness faults and protocols](configuration.md#harness-mode) (phase 7): fault
  presets with client faults (connection churn, abandoned requests, slow and malformed
  bodies, wrong content types, oversized headers, retries on 5xx, expired tokens, an
  HTTP/2 mix) and server faults (drops, stalls, flapping health, node churn), and the
  `llm`, `sse`, `websocket`, `grpc` and `mcp` sources with stream metrics (time to first
  token, inter-token latency, tokens per second).
- [Harness mock-driven traffic and identities](configuration.md#harness-mode) (phase
  6): layers that send an OpenAPI mock's operations (weights, invalid requests, create
  then read), identities (API keys, minted JWTs, OAuth client credentials, forwarded
  addresses, source addresses), and the report's breakdowns by operation and credential.

- [Harness rate-limit tests](configuration.md#harness-mode) (phase 5): a layer's
  `expect` and the `limit_test` pattern (`over_limit`, `window_edge`, `just_under`,
  `distributed`, `many_keys`, `many_ips`, `retry_wave`, `concurrency`, `slow_rise`,
  `limit_probe`), and the report's rate-limit analysis: admission per window and in
  the worst window-long span, the first refusal, recovery, the rate-limit headers,
  identities and gateway nodes, upstream protection and collateral damage, with
  per-layer thresholds.

- [Harness patterns and live tuning](configuration.md#harness-mode) (phase 4): every
  run pattern (step, target, breakpoint, spike, pulse, burst, wave) chained with
  `then:`, with jitter and cluster or node sync; the closed loops `target` on gateway
  overhead and `breakpoint`; live changes to a running run (multiplier, pause, hold,
  skip, `set`, layers) that survive a failover; the run's feed; preview curves; saved
  scenarios; `client_nodes: auto` with a signed scale hook.
- [Harness failover](configuration.md#harness-mode) (phase 3b): eligible nodes, a
  succession ordered by work with standbys first, a replicated snapshot with
  checkpoints, elections by a majority of the eligible nodes, terms that make an old
  leader step down, and a new leader that takes over within the lease, so the
  clients never stop. A minority partition stops sending; a two-node cluster has no
  failover.
- [Harness clusters](configuration.md#harness-mode) (phase 3): a coordinator that
  client and server nodes join, heartbeats with leases (a client stops when it loses
  its leader), clock sync, the rate split over the clients by capacity, synchronised
  starts, one report merged from every node's histograms with `lost` checked against
  every server node, the server nodes exported as the gateway's upstream pool, and
  headless clusters that start on quorum.
- [Harness correlation](configuration.md#harness-mode) (phase 2): every harness
  request is signed, the server side tags and times every answer, and the report
  tells `upstream` from `gateway` from `transport` from `lost` requests, with the
  gateway's overhead, what it changed on the way (headers added, removed, rewritten,
  bodies, paths), one-way legs in loop-through, sampled inspector captures,
  `rustybin_harness_*` metrics and a shaped upstream at `/harness/{*path}`.
- [Load test mode (harness), single node](configuration.md#harness-mode) (phase 1): Rustybin as
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
