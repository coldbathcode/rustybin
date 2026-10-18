# Roadmap

What may come next. Nothing under "Under consideration" is available yet; the pages
for the features that exist today describe current behaviour.

## Recently built

- [Self-service mocks](mocks/self-service.md): visitors of a public instance create
  and manage their own OpenAPI mocks on a time allowance (an hour, extendable to
  three, then an email address for up to six hours a day), on a separate mocks
  domain. Off by default; built for the hosted demo at rustybin.dev.

## Next

- **Configure Rustybin from an AI agent**: a management MCP server at `/_rustybin/mcp`, protected like the
  rest of the control plane, so an agent such as Claude Code can create and deploy mocks, change their
  responses and security, and read captured traffic for you. It is separate from the mock MCP server at
  `/mcp`, which stays a test upstream for MCP gateways.

## Under consideration

- **Self-service across replicas**: visits and visitor mocks live in memory on one
  instance today. A shared store would let a hosted demo run several replicas
  without routing each mock to one of them.
- **Stateful mode for visitor mocks**, with tight caps on what a mock may store
  (items, bytes, text only).
- **More collection formats** for mocks: Bruno and Insomnia, next to Postman, Hurl,
  curl, `.http` and k6.
