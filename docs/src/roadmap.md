# Roadmap

What may come next. Nothing under "Next" or "Under consideration" is available yet;
the pages for the features that exist today describe current behaviour.

## Next

- **Harness mode**: Rustybin as the load client as well as the upstream. One
  instance loops traffic through a gateway back to itself, or a cluster of client and
  server instances drives load and chaos through a gateway at scale. Both sides
  correlate every request, so a run separates the gateway's own responses, lost
  requests and the overhead the gateway adds. Ramp, step, target, breakpoint, spike,
  pulse and burst patterns, plus burst attacks that check rate limiting (window edges,
  distributed bursts, key and IP spraying), all tunable live. Driven from the API, MCP
  or the console, or headless; the cluster elects a new leader if the current one
  dies. Off by default and limited to allowlisted targets.

## Recently built

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
