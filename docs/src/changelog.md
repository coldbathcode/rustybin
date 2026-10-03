# Changelog

## Unreleased: the AI and agent gateway upgrade

A broad rework of Rustybin, from an httpbin-style echo service into an upstream for
API, AI and agent gateway demos.

### Foundation

- axum 0.8, a library plus a thin binary, a shared `AppState`, and a middleware stack
  with request ids, CORS, a time-to-headers timeout, a request body limit and the
  request inspector.
- The route catalogue as the single source of truth for the landing page, the eight
  collection exports, the README tables and the console's API explorer, with tests
  that keep routes, catalogue and OpenAPI in sync.
- Fault injection on every route (`X-Rustybin-Delay`, `X-Rustybin-Fail`), sessions
  (`X-Rustybin-Session`), public mode, an admin token for global mutations, and a
  control plane under `/_rustybin/`.
- Graceful shutdown; the HTTPS and gRPC listeners are optional; invalid settings
  warn instead of failing; bounded state everywhere.

### New

- **Mock LLM** (`/ai/*`): OpenAI (Chat Completions, Responses, completions,
  embeddings, moderations, images, audio), Azure OpenAI, Anthropic, Gemini, AWS
  Bedrock (with binary event streams), Ollama and Cohere, with modes, scripted
  rules, tool calls, structured output, deterministic tokens, native errors and
  rate-limit headers, latency and pace controls, credential checks and request
  inspection.
- **Guardrail mocks**: Azure AI Content Safety, Bedrock ApplyGuardrail, generic
  check and PII redaction.
- **MCP server**: Streamable HTTP for 2026-07-28 and the 2025 handshake versions,
  legacy HTTP+SSE, OAuth 2.1 protected and API key variants, named servers.
- **A2A agents**: seven agents over A2A v1.0 and v0.3, JSON-RPC and HTTP+JSON, with
  streaming, multi-turn, auth-required and SSRF-safe push notifications.
- **Identity provider**: authorization code + PKCE with a login page, refresh token
  rotation, password, client credentials, token exchange, introspection,
  revocation, dynamic client registration.
- **Real auth checks**: JWT validation (HS256 and RS256), HMAC signatures (hmac-auth
  and draft-cavage, with digests), mTLS on the HTTPS listener or forwarded in a
  header, with a persisted demo CA.
- Request bins, generic SSE streams, webhook signing and verification (Standard
  Webhooks, GitHub and Stripe styles), compression (gzip, deflate, Brotli, zstd),
  ranges, data transfer helpers, generic JSON-RPC, GraphQL subscriptions and
  automatic persisted queries, gRPC reflection, health and gRPC-Web.
- The **web console** at `/ui`.
- This documentation site, with every example tested in CI, and SDK conformance
  suites for the mock LLM, MCP, A2A and OAuth.

### Changed

- Open redirects and other abuse-prone endpoints were removed or made safe
  (`/redirect-to` only redirects to the same host).
- Content negotiation serves JSON to browsers by default.
- The Docker image no longer generates certificates with OpenSSL at startup:
  Rustybin generates and persists its own demo PKI (the old script produced a CA
  that the server replaced, which broke mTLS and HTTPS verification in containers).
- `tests/smoke_test.sh` was replaced by the documentation examples
  (`docs/examples/run.sh`).
