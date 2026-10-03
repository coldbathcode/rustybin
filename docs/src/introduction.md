# Rustybin

Rustybin is a single-binary stub service for API gateway and AI gateway demos.
Put it behind any gateway as the upstream and it gives you something realistic to
route to: echo endpoints that show exactly what the gateway forwarded, auth
endpoints that really validate credentials, a mock LLM that speaks the native wire
format of seven AI providers, an MCP server, A2A agents, and protocol endpoints for
GraphQL, SOAP, gRPC, WebSocket, Server-Sent Events and JSON-RPC.

Think httpbin, but broader and built for gateway demos: every response is
deterministic where it matters, every failure mode can be triggered on demand, and
every piece of state is bounded so a shared instance stays healthy.

## Who it is for

- **Presales and solution engineers** who need a dependable upstream for live demos
  of rate limiting, authentication, transformation, AI proxying, prompt guards,
  caching, retries and circuit breakers.
- **Gateway and platform teams** who want a test target for plugin development and
  conformance checks (the mock LLM, MCP server and A2A agents are exercised by the
  official OpenAI, Anthropic, Google Gen AI, MCP and A2A SDKs in CI).
- **Developers** who want a local fake for an LLM provider, an OAuth provider or a
  webhook sender.

## What is inside

| Area | Highlights |
|---|---|
| [Mock LLM](ai/index.md) | OpenAI (Chat Completions, Responses, embeddings, moderations, images, audio), Azure OpenAI, Anthropic, Gemini, AWS Bedrock, Ollama and Cohere wire formats; streaming; tool calls and structured output from your JSON schema; deterministic tokens; native errors and rate-limit headers on demand; credential checks that prove what the gateway injected |
| [Guardrails](ai/guardrails.md) | Fake Azure AI Content Safety, Bedrock ApplyGuardrail and generic PII / jailbreak detectors for guardrail plugins |
| [MCP server](mcp/index.md) | Streamable HTTP for protocol 2026-07-28 (stateless) and the 2025 handshake versions, legacy HTTP+SSE, OAuth 2.1 protected and API key variants, named servers with tool subsets |
| [A2A agents](a2a/index.md) | Seven demo agents speaking A2A v1.0 and v0.3 over JSON-RPC and HTTP+JSON, streaming, multi-turn, auth-required, push notifications |
| [Identity provider](reference/oidc.md) | OAuth 2.0 / OpenID Connect with authorization code + PKCE, client credentials, password, refresh, token exchange, introspection, revocation and dynamic client registration |
| [Auth checks](reference/auth.md) | Basic, API key, JWT (HS256 and RS256), HMAC signatures, mTLS |
| [Protocols](reference/graphql.md) | GraphQL (with subscriptions and persisted queries), SOAP 1.1 / 1.2, gRPC (with reflection, health and gRPC-Web), WebSocket, SSE, JSON-RPC |
| [HTTP toolbox](reference/http-basics.md) | Echo, any status code, delays, caching, compression, ranges, redirects, cookies, request bins, webhook signatures |
| [Chaos](concepts/fault-injection.md) | `X-Rustybin-Delay` and `X-Rustybin-Fail` on every route, flaky endpoints, a health toggle |
| [Observability](concepts/inspector.md) | A request inspector with a live feed, per-session scoping, and an [AI request inspector](ai/gateway-features.md#request-inspection) |
| [Web console](console.md) | A browser UI at `/ui` for live traffic, request bins, the AI playground, MCP and A2A clients and more |

## Ports

| Listener | Default port | Environment variable |
|---|---|---|
| HTTP | 80 | `RUSTYBIN_HTTP_PORT` |
| HTTPS (self-signed demo certificate, optional client certificates) | 443 | `RUSTYBIN_HTTPS_PORT` |
| gRPC (h2c, plus gRPC-Web over HTTP/1.1) | 50051 | `RUSTYBIN_GRPC_PORT` |

The HTTP and HTTPS listeners serve the same routes. Only the HTTP listener is
required: if HTTPS or gRPC cannot start, Rustybin logs a warning and keeps serving.

## Where to go next

- [Quick start](quick-start.md): run it with Docker, Docker Compose, Cargo or on Fly.io.
- [Configuration](configuration.md): every environment variable.
- [Gateway recipes](gateway-recipes.md): what to configure in a gateway and what to
  look at in Rustybin for each capability.
- `GET /` on a running instance lists every endpoint with a runnable example, and
  `/openapi.json` and `/docs` describe the HTTP API.

Rustybin is open source under the GNU AGPL-3.0, with a commercial license for
organisations that cannot use AGPL software (see `LICENSING.md` in the
repository).
