# Rustybin

**A single-binary upstream for API gateway, AI gateway and agent gateway demos.**

Put Rustybin behind any gateway and you get something realistic to route to: a mock
LLM that speaks seven providers' native APIs, an MCP server, A2A agents, an OAuth /
OIDC provider, real auth checks, every common protocol, and failure modes you can
trigger on demand. Everything is deterministic, every bit of state is bounded, and a
web console shows exactly what the gateway forwarded.

Documentation: **<https://oliwynn.github.io/rustybin/>** (built from [`docs/`](docs/src/introduction.md);
every request example in it is tested in CI).

## Why

Gateway demos need an upstream that is predictable, observable and able to fail on
cue. Public echo services cannot be pointed at from a customer's network, real LLM
providers cost money and answer differently every time, and building a fake OAuth
server, MCP server or A2A agent for each demo takes days. Rustybin is all of them in
one container, and shows you what arrived.

## 60 second quick start

```bash
git clone https://github.com/oliwynn/rustybin && cd rustybin
docker build -t rustybin .
docker run -d --name rustybin -p 8080:80 -p 8443:443 -p 50051:50051 rustybin
```

Or without Docker: `RUSTYBIN_HTTP_PORT=8080 RUSTYBIN_HTTPS_PORT=8443 cargo run --release`
(Rust 1.86+). Then:

```bash
# What did the upstream receive?
curl -s 'http://localhost:8080/anything/hello?demo=1' -H 'X-Demo: rustybin'

# A chat completion in the OpenAI wire format
curl -s http://localhost:8080/ai/openai/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}'

# Make any route fail
curl -si http://localhost:8080/echo -H 'X-Rustybin-Fail: 503'
```

Open **<http://localhost:8080/ui>** for the web console, `/` for every endpoint with
a runnable example, and `/docs` for the OpenAPI reference. See the
[quick start](docs/src/quick-start.md) for Docker Compose and Fly.io.

## Highlights

**AI and agents**

- **Mock LLM** for OpenAI (Chat Completions, Responses, embeddings, moderations,
  images, audio), Azure OpenAI, Anthropic, Gemini, AWS Bedrock, Ollama and Cohere.
  The official SDKs accept it. Streaming in each native format, tool calls with
  arguments generated from your JSON schema, structured output, deterministic token
  counts and rate-limit headers.
- **Gateway features made visible**: echo mode returns the exact prompt the upstream
  received (prompt decorators), scripted mode returns fake PII, secrets or unsafe
  text on demand (sanitisers, guards), embeddings preserve similarity (semantic
  cache), native 429 / 529 / timeout / content filter errors on demand (fallback,
  retries), and `X-Rustybin-Credential` proves which key the gateway injected.
- **Guardrail mocks**: Azure AI Content Safety, Bedrock ApplyGuardrail and generic
  PII / jailbreak detection.
- **MCP server**: Streamable HTTP for protocol 2026-07-28 and the 2025 handshake
  versions, legacy HTTP+SSE, an OAuth 2.1 protected variant (RFC 9728 metadata,
  step-up scopes), an API key variant and named servers with tool subsets.
- **A2A agents**: seven agents over A2A v1.0 and v0.3, JSON-RPC and HTTP+JSON,
  streaming, multi-turn, auth-required and SSRF-safe push notifications.

**API gateway essentials**

- Echo, any status code, delays, caching with ETags, compression, ranges, redirects,
  cookies, images.
- Real credential checks: Basic, API keys, JWT (HS256 and RS256), HMAC signatures,
  mTLS (TLS or forwarded in a header).
- A built-in OAuth 2.0 / OIDC provider: authorization code + PKCE, client
  credentials, password, refresh rotation, token exchange, introspection,
  revocation, dynamic client registration.
- GraphQL (subscriptions, persisted queries), SOAP 1.1 / 1.2, gRPC (reflection,
  health, gRPC-Web), WebSocket, Server-Sent Events, JSON-RPC.
- Chaos: `X-Rustybin-Delay` and `X-Rustybin-Fail` on every route, flaky endpoints per
  client, a health toggle shared with gRPC health.
- Observability: a request inspector with a live feed, request bins, webhook
  signature verification.
- Collections for Postman, Insomnia, Bruno, curl, `.http`, Hurl, k6 and HAR,
  generated from the same route catalogue as everything else.

## Web console

`/ui` is a browser console for presenting: Overview, Live traffic (what the gateway
forwarded, with gateway headers highlighted), Request bins, API explorer, AI
playground, MCP inspector, A2A client, Chaos and health (with a small load
generator) and Token lab. It is embedded in the binary and works offline. Set a
gateway base URL in its settings to send every request through your gateway.

## Configuration

All settings are environment variables; invalid values log a warning and fall back
to the default. `GET /_rustybin/config` shows the effective configuration. Details:
[configuration](docs/src/configuration.md).

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_HTTP_PORT` | `80` | HTTP port (bind failure is fatal) |
| `RUSTYBIN_HTTPS_PORT` | `443` | HTTPS port (optional listener, demo certificate) |
| `RUSTYBIN_GRPC_PORT` | `50051` | gRPC port (optional listener) |
| `RUSTYBIN_HOST` | `0.0.0.0` | Bind address, IPv4 or IPv6 |
| `RUSTYBIN_LOG_LEVEL` | `info` | Log filter (`EnvFilter` syntax); falls back to `RUST_LOG` |
| `RUSTYBIN_INSTANCE_ID` | random UUID | Instance name for load balancing demos |
| `RUSTYBIN_TRUST_FORWARD` | `false` | Trust `Forwarded`, `X-Forwarded-*`, `X-Real-IP`, `Fly-Client-IP` for client IP, scheme and host |
| `RUSTYBIN_BODY_LIMIT` | `1048576` | Maximum request body in bytes (`413` above) |
| `RUSTYBIN_REQUEST_TIMEOUT` | `120` | Seconds until response headers (`503` after); streams are not cut; `0` disables |
| `RUSTYBIN_TLS_CERT` / `RUSTYBIN_TLS_KEY` | `certs/server.crt` / `certs/server.key` | HTTPS certificate and key; generated (with a persisted demo CA) when missing |
| `RUSTYBIN_MTLS_IN_HEADER` | unset | Header in which a gateway forwards the client certificate for `/auth/mtls` |
| `RUSTYBIN_PUBLIC_MODE` | `false` | Hardening for shared instances: lower caps, session-scoped inspector, global mutations need the admin token |
| `RUSTYBIN_ADMIN_TOKEN` | unset | Required (Bearer or `X-Rustybin-Admin-Token`) for health toggles, global flaky reset and clearing all captured requests |
| `RUSTYBIN_CORS_ORIGINS` | `*` | Allowed CORS origins, comma separated; `off` disables Rustybin's CORS |
| `RUSTYBIN_INSPECTOR_CAPACITY` | `500` | Requests kept by the inspector (max 10000) |
| `RUSTYBIN_MCP_API_KEY` | unset | Exact `X-API-Key` for `/mcp/apikey` (unset: any non-empty key) |
| `RUSTYBIN_MCP_ALLOWED_ORIGINS` | `*` | `Origin` values accepted by the MCP endpoints |
| `RUSTYBIN_MCP_ACCEPTED_AUDIENCES` | `rustybin` | Extra token audiences for `/mcp/protected`; `none` = strict |
| `RUSTYBIN_MCP_RESOURCE_URL` | derived | Resource identifier of `/mcp/protected` |
| `RUSTYBIN_MCP_CLOCK_TICK_SECS` | `5` | Update interval of the `rustybin://clock` resource |
| `RUSTYBIN_AI_REQUIRE_AUTH` | `false` | Enforce each AI provider's native credential |
| `RUSTYBIN_AI_API_KEY` | unset | The only accepted AI key (implies `RUSTYBIN_AI_REQUIRE_AUTH`) |
| `RUSTYBIN_A2A_PUSH_ALLOWLIST` | unset | Hosts A2A push notifications may be delivered to |
| `RUSTYBIN_A2A_PUSH_ALLOW_ALL` | `false` | Allow push notifications to any URL (ignored in public mode) |

## Development

```bash
cargo build && cargo test
cargo clippy --all-targets -- -D warnings
docs/examples/run.sh          # every documentation example (Hurl) against a fresh server
mdbook build docs             # the documentation site
```

Architecture, adding a module and the conformance suites: see
[contributing](docs/src/contributing.md).

## Endpoint reference

Generated from the route catalogue (`cargo run -- --print-endpoints-markdown`). The
gRPC `EchoService` listens on its own port (default `50051`), see
[gRPC](docs/src/reference/grpc.md). JSON endpoints also answer XML with
`Accept: application/xml`.

<!-- BEGIN ENDPOINTS -->
<!-- Generated from src/catalog.rs: run `cargo run -- --print-endpoints-markdown` and paste, or `RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints`. -->

### Echo & Reflection

| Methods | Path | Description |
|---|---|---|
| ANY | `/echo` | Echo back the full request (method, headers, query, body) |
| ANY | `/echo/{*path}` | Echo with an arbitrary sub-path |
| ANY | `/anything` | Alias for /echo |
| ANY | `/anything/{*path}` | Alias for /echo/{*path} |

### Status Codes

| Methods | Path | Description |
|---|---|---|
| ANY | `/status/{code}` | Respond with any HTTP status code (200-599), or a weighted random choice |

### Response Shaping

| Methods | Path | Description |
|---|---|---|
| ANY | `/delay/{ms}` | Wait, then respond: milliseconds (1500), `250ms` or seconds (`1.5s`); ?jitter=true adds +-20% |
| GET | `/cache/{ttl}` | Cache-Control, ETag and Last-Modified; 304 on If-None-Match / If-Modified-Since |
| GET | `/response-headers` | Query parameters become response headers (hop-by-hop / framing headers refused) |
| GET | `/gzip` | gzip-compressed JSON echo (gzipped: true) |
| GET | `/deflate` | deflate (zlib) compressed JSON echo (deflated: true) |
| GET | `/brotli` | Brotli-compressed JSON echo (brotli: true) |
| GET | `/zstd` | Zstandard-compressed JSON echo (zstd: true) |
| GET | `/encoding/utf8` | UTF-8 sample page (many scripts, symbols, emoji) |
| GET | `/range/{n}` | n bytes supporting Range requests (206, 416, ETag, If-Range) |
| GET | `/bytes/{n}` | n random bytes (seed for deterministic output) |
| GET | `/stream/{n}` | n JSON lines streamed (chunked) |
| GET | `/stream-bytes/{n}` | n random bytes streamed in chunks |
| GET | `/drip` | Drip bytes over a duration (slow body) |
| GET | `/links/{n}` | Redirect to /links/{n}/0 |
| GET | `/links/{n}/{offset}` | HTML page with n links (max 200) |
| GET | `/base64/{value}` | Decode a base64 (standard or URL-safe) value |

### Streaming (SSE)

| Methods | Path | Description |
|---|---|---|
| GET | `/sse` | Numbered event stream with resume (Last-Event-ID) and heartbeats (SSE) |
| GET POST | `/sse/chat` | Provider-neutral streamed chat text, one word per event, ending with [DONE] (SSE) |

### Redirects & Cookies

| Methods | Path | Description |
|---|---|---|
| GET | `/redirect/{n}` | Chain of n relative 302 redirects |
| GET | `/cookies` | Request cookies as JSON |
| GET | `/cookies/set` | Set cookies from query parameters, then 302 to /cookies |
| GET | `/cookies/set/{name}/{value}` | Set a single cookie (Path=/) |
| GET | `/cookies/delete` | Expire the cookies named in the query string (_path / _domain must match how they were set) |
| ANY | `/redirect-to` | Redirect to a relative path or this host only (no open redirect) |
| GET | `/absolute-redirect/{n}` | Chain of n absolute 302 redirects on the same host (max 10) |

### Info & Random

| Methods | Path | Description |
|---|---|---|
| GET | `/ip` | Client IP address (IPv4 and IPv6) |
| GET | `/ip/v4` | Client IPv4 address |
| GET | `/ip/v6` | Client IPv6 address |
| GET | `/date` | Current date (UTC) |
| GET | `/date/{*timezone}` | Current date in an IANA timezone |
| GET | `/time` | Current time, ISO 8601 (UTC) |
| GET | `/time/{*timezone}` | Current time in an IANA timezone |
| GET | `/uuid` | Random UUID v4 |
| GET | `/guuid` | Random braced GUID |
| GET | `/random` | Bundle of random values |
| GET | `/random/int` | Random signed integer |
| GET | `/random/int/{lower}/{upper}` | Random integer in [lower, upper] |
| GET | `/random/uint` | Random unsigned integer |
| GET | `/random/lorem-ipsum` | One paragraph of lorem ipsum |
| GET | `/random/lorem-ipsum/{count}` | count paragraphs of lorem ipsum |
| GET | `/image` | Image in the format chosen by Accept (webp, svg, jpeg, png, gif; 406 otherwise) |
| GET | `/image/png` | 16x16 PNG image |
| GET | `/image/jpeg` | 8x8 JPEG image |
| GET | `/image/gif` | 8x8 GIF image |
| GET | `/image/webp` | 8x8 lossless WebP image |
| GET | `/image/svg` | SVG image |

### Auth: Basic & API Key

| Methods | Path | Description |
|---|---|---|
| ANY | `/auth/basic-auth` | HTTP Basic auth (default user basic, password password) |
| ANY | `/auth/basic-auth/{username}/{password}` | HTTP Basic auth with credentials from the path |
| ANY | `/auth/api-key` | API key in a header (default apikey: my-key) |
| ANY | `/auth/api-key/{header_name}/{key_value}` | API key with header name and value from the path |

### Auth: HMAC

| Methods | Path | Description |
|---|---|---|
| ANY | `/auth/hmac` | Validate an hmac-auth / draft-cavage signature (default alice / secret) |
| ANY | `/auth/hmac/{username}/{secret}` | HMAC validation with username and secret from the path |

### Auth: JWT

| Methods | Path | Description |
|---|---|---|
| ANY | `/auth/jwt` | Validate a Bearer JWT (HS256 demo secret or RS256 IdP key, exp/nbf, optional ?iss= ?aud=) |
| ANY | `/auth/jwt/decode` | Decode a Bearer JWT WITHOUT validation (shows what the gateway forwarded) |
| ANY | `/auth/jwt/exchange` | Exchange a valid JWT for a new HS256-signed token (same checks as /auth/jwt) |

### Auth: OIDC Provider

| Methods | Path | Description |
|---|---|---|
| GET | `/.well-known/openid-configuration` | OIDC discovery document |
| GET | `/.well-known/oauth-authorization-server` | OAuth 2.0 authorization server metadata (RFC 8414) |
| POST | `/oauth/token` | Token endpoint (authorization_code + PKCE, refresh_token, client_credentials, password, token exchange) |
| GET | `/oauth/jwks` | RS256 public key in JWK Set format |
| GET POST | `/oauth/authorize` | Authorization code flow with a demo login form (PKCE, nonce, resource) |
| GET POST | `/oauth/userinfo` | User claims for a Bearer access token (scope openid) |
| POST | `/oauth/introspect` | Token introspection (RFC 7662, client authentication required) |
| POST | `/oauth/revoke` | Token revocation (RFC 7009) for refresh and access tokens |
| POST | `/oauth/register` | Dynamic client registration (RFC 7591, used by MCP clients) |

### Auth: mTLS

| Methods | Path | Description |
|---|---|---|
| ANY | `/auth/mtls` | Validate the client certificate (TLS peer certificate, or the RUSTYBIN_MTLS_IN_HEADER header) |
| GET | `/auth/mtls/get-client-cert` | Download the demo client certificate and key |
| GET | `/auth/mtls/get-ca-cert` | Download the demo CA certificate (persisted across restarts when the cert dir is writable) |

### AI: OpenAI-compatible

| Methods | Path | Description |
|---|---|---|
| POST | `/ai/openai/v1/chat/completions` | Chat completions (SSE streaming with stream=true, tools, structured output, n) |
| POST | `/ai/v1/chat/completions` | Chat completions (SSE streaming with stream=true, tools, structured output, n) |
| POST | `/ai/openai/v1/completions` | Legacy text completions (stream, echo, suffix, n) |
| POST | `/ai/v1/completions` | Legacy text completions (stream, echo, suffix, n) |
| POST | `/ai/openai/v1/embeddings` | Deterministic bag-of-words embeddings (dimensions, base64) |
| POST | `/ai/v1/embeddings` | Deterministic bag-of-words embeddings (dimensions, base64) |
| GET | `/ai/openai/v1/models` | List available models |
| GET | `/ai/v1/models` | List available models |
| GET | `/ai/openai/v1/models/{model}` | Retrieve a model (404 model_not_found for unknown ids) |
| GET | `/ai/v1/models/{model}` | Retrieve a model (404 model_not_found for unknown ids) |
| POST | `/ai/openai/v1/moderations` | Moderation (keyword-based categories, deterministic scores) |
| POST | `/ai/v1/moderations` | Moderation (keyword-based categories, deterministic scores) |
| POST | `/ai/openai/v1/images/generations` | Image generation (tiny valid PNG as b64_json or a URL to /image/png) |
| POST | `/ai/v1/images/generations` | Image generation (tiny valid PNG as b64_json or a URL to /image/png) |
| POST | `/ai/openai/v1/audio/transcriptions` | Audio transcription (multipart; json, text, srt, vtt, verbose_json) |
| POST | `/ai/v1/audio/transcriptions` | Audio transcription (multipart; json, text, srt, vtt, verbose_json) |
| POST | `/ai/openai/v1/responses` | Responses API (output items, function calls, native SSE events with stream=true) |
| POST | `/ai/v1/responses` | Responses API |
| POST | `/ai/azure/openai/deployments/{deployment}/chat/completions` | Azure OpenAI chat completions (api-version required, model = deployment) |
| POST | `/ai/azure/openai/deployments/{deployment}/completions` | Azure OpenAI legacy completions |
| POST | `/ai/azure/openai/deployments/{deployment}/embeddings` | Azure OpenAI embeddings |

### AI: Anthropic-compatible

| Methods | Path | Description |
|---|---|---|
| POST | `/ai/anthropic/v1/messages` | Messages API (tools, prompt caching usage, native SSE event stream with stream=true) |
| POST | `/ai/anthropic/v1/messages/count_tokens` | Count input tokens |
| GET | `/ai/anthropic/v1/models` | List models |
| GET | `/ai/anthropic/v1/models/{model}` | Retrieve a model |

### AI: Mock LLM

| Methods | Path | Description |
|---|---|---|
| GET | `/ai/gemini/v1beta/models` | Gemini: list models |
| GET POST | `/ai/gemini/v1beta/models/{*rest}` | Gemini: {model}:generateContent, :streamGenerateContent (?alt=sse), :countTokens, :embedContent, :batchEmbedContents |
| POST | `/ai/bedrock/model/{model_id}/converse` | Bedrock Converse |
| POST | `/ai/bedrock/model/{model_id}/converse-stream` | Bedrock ConverseStream (binary AWS event stream with CRC32 framing) |
| POST | `/ai/bedrock/model/{model_id}/invoke` | Bedrock InvokeModel (Anthropic body, Titan text/embeddings, Llama prompt) |
| POST | `/ai/bedrock/model/{model_id}/invoke-with-response-stream` | Bedrock InvokeModelWithResponseStream (event stream of base64 chunks) |
| POST | `/ai/ollama/api/chat` | Ollama chat (NDJSON stream by default; tools, format) |
| POST | `/ai/ollama/api/generate` | Ollama generate (NDJSON stream by default) |
| GET | `/ai/ollama/api/tags` | Ollama local models |
| POST | `/ai/ollama/api/embed` | Ollama embeddings (768 dimensions by default) |
| POST | `/ai/ollama/api/embeddings` | Ollama legacy embeddings |
| POST | `/ai/cohere/v2/rerank` | Rerank (deterministic relevance scores, top_n) |
| POST | `/ai/cohere/v2/embed` | Cohere embed (float, int8, uint8, base64) |
| POST | `/ai/v1/rerank` | Generic rerank alias (same as /ai/cohere/v2/rerank) |
| GET | `/ai/requests` | Recent mock LLM exchanges (newest first) |
| GET | `/ai/requests/{id}` | What the upstream received for one AI request |

### AI: Guardrails

| Methods | Path | Description |
|---|---|---|
| POST | `/guardrails/azure/contentsafety/{operation}` | Azure AI Content Safety style text:analyze and text:shieldPrompt |
| POST | `/guardrails/bedrock/guardrail/{id}/version/{version}/apply` | AWS Bedrock ApplyGuardrail style assessment (block, anonymize PII) |
| POST | `/guardrails/check` | Generic guardrail check: flagged, categories, jailbreak, PII spans, redacted text |
| POST | `/guardrails/pii/redact` | Redact emails, phone numbers, SSNs, credit cards (Luhn) and IPs |

### MCP Server

| Methods | Path | Description |
|---|---|---|
| POST GET DELETE | `/mcp` | MCP Streamable HTTP server (2026-07-28 stateless + 2025-xx sessions) |
| POST GET DELETE | `/mcp/protected` | MCP server behind OAuth 2.1 bearer auth (MCP authorization spec) |
| POST GET DELETE | `/mcp/apikey` | MCP server requiring an X-API-Key header |
| POST GET DELETE | `/mcp/servers/{name}` | Named MCP servers with tool subsets: weather, crm, devtools |
| GET | `/mcp/sse` | Legacy MCP HTTP+SSE transport (2024-11-05): event stream (SSE) |
| POST | `/mcp/messages` | Legacy MCP HTTP+SSE transport (2024-11-05): message endpoint |
| GET | `/.well-known/oauth-protected-resource` | OAuth Protected Resource Metadata (RFC 9728) for /mcp/protected |
| GET | `/.well-known/oauth-protected-resource/mcp/protected` | OAuth Protected Resource Metadata (RFC 9728), path-suffixed form |

### A2A Agents

| Methods | Path | Description |
|---|---|---|
| GET | `/.well-known/agent-card.json` | A2A Agent Card (v1.0, readable by v0.3 clients) of the default echo agent, listing every demo agent |
| GET | `/.well-known/agent.json` | Legacy A2A v0.3 Agent Card (url + preferredTransport) of the default agent |
| GET POST | `/a2a` | GET: directory of demo agents; POST: JSON-RPC endpoint of the default echo agent |
| GET POST | `/a2a/{agent}` | A2A JSON-RPC endpoint per agent (v1.0 methods with A2A-Version: 1.0, v0.3 methods without); GET returns the agent card |
| GET | `/a2a/{agent}/.well-known/agent-card.json` | Agent Card of one agent (v1.0 + v0.3 fields) |
| GET | `/a2a/{agent}/.well-known/agent.json` | Legacy v0.3 Agent Card of one agent |
| ANY | `/a2a/{agent}/v1/{*rest}` | A2A HTTP+JSON (REST) binding: message:send, message:stream, tasks, tasks/{id}, tasks/{id}:cancel, tasks/{id}:subscribe, push configs, extendedAgentCard |
| GET POST DELETE | `/a2a/webhook-sink/{id}` | Built-in push notification sink: POST records a notification, GET lists them, DELETE clears |

### GraphQL

| Methods | Path | Description |
|---|---|---|
| GET POST | `/graphql` | GraphQL over HTTP (POST or GET ?query=; GraphiQL for browsers) |
| GET | `/graphql/schema` | Schema in SDL |
| GET | `/graphql/ws` | GraphQL subscriptions over WebSocket (graphql-transport-ws and graphql-ws) (WebSocket) |

### Orchestration

| Methods | Path | Description |
|---|---|---|
| POST | `/orchestration/step/1` | Step 1: authenticate (non-empty X-Api-Key required) |
| POST | `/orchestration/step/2` | Step 2: enrich with a deterministic risk score (X-Correlation-Id required) |
| POST | `/orchestration/step/3` | Step 3: validate (declined when risk score >= 70 or amount > 50000) |
| POST | `/orchestration/step/4` | Step 4: process (requires X-Validation-Result: approved) |
| GET | `/orchestration/status` | Pipeline documentation |
| POST | `/jsonrpc` | Generic JSON-RPC 2.0 endpoint (batches, notifications, standard errors) |

### SOAP / XML

| Methods | Path | Description |
|---|---|---|
| GET POST | `/soap` | SOAP 1.1 / 1.2 service (GetUser, CreateOrder, GetStatus); GET /soap?wsdl returns the WSDL |
| GET | `/soap/wsdl` | WSDL 1.1 document (SOAP 1.1 and 1.2 bindings) |

### WebSocket

| Methods | Path | Description |
|---|---|---|
| GET | `/ws` | WebSocket echo of every text and binary frame (subprotocol echoed) (WebSocket) |
| GET | `/ws/time` | WebSocket timestamp ticker (?interval_ms=&count=), then a normal close (WebSocket) |

### Reliability Testing

| Methods | Path | Description |
|---|---|---|
| ANY | `/flaky/{fail_rate}` | Fail with 503 for fail_rate percent of requests |
| ANY | `/flaky/pattern/{pattern}` | Deterministic success/failure pattern (S = success, F = failure), per session |
| ANY | `/flaky/after/{n}` | Succeed n times, then fail (circuit breaker trip), per session and n |
| ANY | `/flaky/recover/{n}` | Fail n times, then recover (circuit breaker half-open), per session and n |
| POST | `/flaky/reset` | Reset the caller's counters (?scope=all resets everyone's, admin-guarded) |
| GET | `/flaky/status` | The caller's flaky counters |

### Request Bin & Webhooks

| Methods | Path | Description |
|---|---|---|
| GET POST | `/bin` | Create a request bin (POST) or list your bins (GET) |
| ANY | `/bin/{id}` | Capture a request into a bin (DELETE deletes the bin) |
| ANY | `/bin/{id}/{*path}` | Capture a request sent to any sub path of a bin |
| GET | `/bin/{id}/requests` | List the requests captured by a bin (newest first) |
| GET | `/bin/{id}/requests/stream` | Live feed of a bin's captured requests (SSE) |
| GET | `/bin/{id}/requests/{n}` | One captured request by sequence number |
| GET | `/webhooks` | Webhook signature schemes and demo secrets |
| POST | `/webhooks/verify` | Verify a Standard Webhooks signature and explain failures |
| POST | `/webhooks/verify/{scheme}` | Verify a webhook signature: standard, github (X-Hub-Signature-256) or stripe (Stripe-Signature) |
| GET POST | `/webhooks/sign` | Produce a signed example webhook (headers + body) for a secret |
| POST | `/webhooks/receive/{secret_id}` | Webhook receiver with a demo secret: 204 when valid, 401 otherwise |

### Health & Identity

| Methods | Path | Description |
|---|---|---|
| GET | `/health` | Health check (200, or 503 when toggled unhealthy) |
| POST | `/health/healthy` | Mark the instance healthy (admin-guarded) |
| POST | `/health/unhealthy` | Mark the instance unhealthy: 200 here, then /health returns 503 (admin-guarded) |
| POST | `/health/toggle` | Flip the health state, 200 with the new state (admin-guarded) |
| ANY | `/identity` | Instance identity: id, hostname, uptime, /identity request count, ports and config (load-balancing demos) |

### Control Plane

| Methods | Path | Description |
|---|---|---|
| GET DELETE | `/_rustybin/requests` | List captured requests (newest first); DELETE clears them |
| GET | `/_rustybin/requests/stream` | Live feed of captured requests (SSE) |
| GET | `/_rustybin/requests/{id}` | One captured request by id |
| GET | `/_rustybin/config` | Effective configuration (no secrets) |
| GET | `/_rustybin/version` | Service name and version |
| GET | `/_rustybin/catalog` | Route catalogue as JSON (paths, methods, categories, examples) |
| GET | `/_rustybin/status` | Uptime, health state and inspector counters |

### Docs & Exports

| Methods | Path | Description |
|---|---|---|
| GET | `/` | This landing page (always 200, safe for liveness checks) |
| GET | `/ui` | Web console (redirects to /ui/) |
| GET | `/ui/` | Web console entry page |
| GET | `/ui/{*path}` | Console assets; extension-less paths fall back to index.html |
| GET | `/openapi.json` | OpenAPI 3.0.3 specification (JSON) |
| GET | `/openapi.yaml` | OpenAPI 3.0.3 specification (YAML) |
| GET | `/docs` | Interactive API reference (Scalar) |
| GET | `/export/postman.json` | Postman collection (v2.1) |
| GET | `/export/insomnia.json` | Insomnia export (v4) |
| GET | `/export/curl.sh` | cURL shell script |
| GET | `/export/bruno.json` | Bruno collection |
| GET | `/export/requests.http` | VS Code / JetBrains .http file |
| GET | `/export/requests.hurl` | Hurl file |
| GET | `/export/k6.js` | k6 load-test script |
| GET | `/export/har.json` | HAR archive |
<!-- END ENDPOINTS -->

## License

Rustybin is open source under the [GNU AGPL-3.0](LICENSE), with a commercial
license available for organisations that cannot use AGPL software. See
[LICENSING.md](LICENSING.md), the [trademark policy](TRADEMARKS.md) and
[CONTRIBUTING.md](CONTRIBUTING.md) (contributors sign the [CLA](CLA.md)).
