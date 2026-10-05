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
- **Plan limits** for hosted offerings (`RUSTYBIN_PLAN`, `RUSTYBIN_LIMIT_*`,
  `RUSTYBIN_USAGE_FILE`): rate, concurrency, streams and daily or monthly request and
  egress quotas, per instance or per session, with IETF RateLimit headers on 429s and
  `GET /_rustybin/usage`. Off by default.
- This documentation site, with every example tested in CI, and SDK conformance
  suites for the mock LLM, MCP, A2A and OAuth.

### Operations and hosting

- Control-plane authentication (`RUSTYBIN_CONTROL_AUTH`): `token` (admin token) or
  `jwt` (admin token or Ed25519 JWTs with `aud`, `exp` and the scopes `inspector`,
  `console`, `admin`) for every `/_rustybin/*` route except the new readiness probe.
  The web console signs in with a token from the URL fragment or a sign-in screen
  and reads its live feed with `fetch` so the bearer header is sent.
  `RUSTYBIN_HOSTED_MODE=true` presets `jwt` and JSON logs.
- `GET /_rustybin/ready`: readiness, always 200 while serving, independent of the
  `/health` demo toggle, exempt from auth, plan limits and capture.
- `GET /_rustybin/metrics`: Prometheus metrics with bounded labels (route templates,
  status classes, protocols, mock LLM tokens by model family, injected faults,
  open streams, egress, build info).
- More metrics: `rustybin_limit_rejections_total{dimension}` (plan limiter 429s and
  gRPC `RESOURCE_EXHAUSTED`), `rustybin_llm_requests_total{provider,model_family,streaming}`
  and `rustybin_llm_faults_total{provider,kind}` (mock LLM faults by kind), and
  latency buckets from 1 ms to 60 s on `rustybin_request_duration_seconds` so p50
  and p99 can be computed with `histogram_quantile` (PromQL examples in the docs).
- `RUSTYBIN_LOG_FORMAT=json`: one JSON object per line with the request id.
- `RUSTYBIN_GRPC_ON_HTTP=true`: the gRPC services on the HTTP and HTTPS listeners
  (h2c and ALPN `h2`) for a TLS-terminating proxy.
- `RUSTYBIN_CONSOLE_TITLE` and `RUSTYBIN_CONSOLE_BACKLINK` in the console header.
- `/_rustybin/version`, `/_rustybin/status` and `/_rustybin/usage` report the build
  commit (`git_sha`) and the control auth mode.

### Changed

- Open redirects and other abuse-prone endpoints were removed or made safe
  (`/redirect-to` only redirects to the same host).
- Content negotiation serves JSON to browsers by default.
- The Docker image no longer generates certificates with OpenSSL at startup:
  Rustybin generates and persists its own demo PKI (the old script produced a CA
  that the server replaced, which broke mTLS and HTTPS verification in containers).
- `tests/smoke_test.sh` was replaced by the documentation examples
  (`docs/examples/run.sh`).
- The Docker image runs as an unprivileged user (uid 10001) and supports a
  read-only root filesystem; `docker-compose.yml` keeps the demo PKI in a named
  volume, and `fly.toml` uses ports 8080 / 8443 and the readiness probe.
- MCP `tools/list` (and the other list methods) render their result once per server
  and protocol version instead of on every request: about 7x the throughput, same
  bytes on the wire.
